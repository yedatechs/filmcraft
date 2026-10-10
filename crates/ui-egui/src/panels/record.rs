//! The Record panel (Window ▸ Record, or the red dot at the right of the Program monitor's
//! transport): pick a screen (a display or a window), up to four cameras (each with its quality,
//! Mirror and offset), up to four microphones and a name, press Record, and Stop builds a synced
//! sequence of the files. Recording itself is the engine's `record.*` commands
//! (`filmcraft_engine::record`, docs/recording.md).
//!
//! Automation ids: `record.panel` (the window), `record.panel.screen` (+ `.screen.<i>`, 0 = Off),
//! per camera row `n` (1-based) `record.panel.camera.<n>.device` (+ `.device.<i>`, 0 = Off),
//! `.quality` (+ `.quality.<i>`), `.mirror`, `.offset`, `.remove`, and `record.panel.camera.add`;
//! per microphone row `record.panel.mic.<n>.device` (+ `.device.<i>`), `.remove`, and
//! `record.panel.mic.add`; `record.panel.name`, `record.panel.record` (Record / Stop),
//! `record.panel.cancel`, `record.panel.refresh`, `record.panel.counters`, `record.panel.level`,
//! `record.panel.error`, `record.panel.close`; the Program monitor button is
//! `program.transport.record`. UI command: `window.record` (open). The panel state is
//! `UiState::record` ([`RecordUi`]), so `ui.set {"record": {...}}` drives it. No keyboard
//! shortcuts.

use egui::{Color32, Rect, RichText, Sense, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;

const RED: Color32 = Color32::from_rgb(0xd8, 0x50, 0x3f);

/// Camera quality: the size asked of the camera.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    #[serde(rename = "720p")]
    P720,
    #[default]
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "4k")]
    P2160,
    #[serde(rename = "native")]
    Native,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Quality::P720, Quality::P1080, Quality::P2160, Quality::Native];
    pub fn label(self) -> &'static str {
        match self {
            Quality::P720 => "720p",
            Quality::P1080 => "1080p",
            Quality::P2160 => "4K",
            Quality::Native => "Native",
        }
    }
    pub(crate) fn size(self) -> Option<(u32, u32)> {
        match self {
            Quality::P720 => Some((1280, 720)),
            Quality::P1080 => Some((1920, 1080)),
            Quality::P2160 => Some((3840, 2160)),
            Quality::Native => None,
        }
    }
}

/// Most camera / microphone rows (the engine's limit).
pub const MAX_ROWS: usize = filmcraft_engine::record::MAX_PER_KIND;

/// One camera row of the panel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CameraRow {
    /// `""` = Off, else a camera id.
    pub device: String,
    pub quality: Quality,
    /// Flip the camera horizontally in the sequence.
    pub mirror: bool,
    /// "Offset … ms", applied at Stop.
    pub offset_ms: f64,
    /// The live preview is popped out into its own always-on-top window.
    pub popout: bool,
}

/// One microphone row of the panel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MicRow {
    /// `""` = Off, `default` = the default input, else a microphone name.
    pub device: String,
}

impl Quality {
    /// The Settings ▸ Recording ▸ Camera quality id (`720p`, `1080p`, `4k`, `native`).
    pub fn from_id(id: &str) -> Quality {
        serde_json::from_value(json!(id)).unwrap_or_default()
    }
}

/// A new camera row with the Settings ▸ Recording camera defaults.
pub fn camera_row(app: &FilmcraftApp, device: String) -> CameraRow {
    let rs = &app.session.prefs.recording;
    CameraRow { device, quality: Quality::from_id(&rs.camera_quality), mirror: rs.camera_mirror, offset_ms: 0.0, popout: false }
}

/// Record panel state (`UiState::record`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RecordUi {
    pub open: bool,
    /// `""` = Off, `display:<id>` or `window:<id>`.
    pub screen: String,
    /// Record only this part of the display: `[x, y, w, h]` in display pixels (None = all).
    pub screen_area: Option<[u32; 4]>,
    /// The border is a drawing surface: drag the area to record.
    pub drawing: bool,
    /// The recording border as shown (`record.overlay`; None = not shown). Set every frame.
    pub overlay: Option<super::record_overlay::OverlayInfo>,
    /// The border went away after a recording stopped (until the screen is chosen again or the
    /// panel reopens).
    pub overlay_dismissed: bool,
    /// Camera rows (at most [`MAX_ROWS`]).
    pub cameras: Vec<CameraRow>,
    /// Microphone rows (at most [`MAX_ROWS`]).
    pub mics: Vec<MicRow>,
    /// Recording name (`""` = `Recording <n>`).
    pub name: String,
    /// The last error (a permission, a device), shown in the panel.
    pub error: String,
    /// The outcome of the last recording ("Recorded …").
    pub last: String,
    /// While recording: the status-bar line (`Recording · 00:12 · screen 360 f / camera 358 f`).
    pub live: String,
    /// The sources were preselected once (first display, first camera, default mic).
    pub initialized: bool,
    /// The Settings section at the bottom is expanded.
    pub settings_open: bool,
    /// `record.devices`, refreshed when the panel opens (not saved).
    #[serde(skip)]
    pub devices: Option<Value>,
    /// Camera preview textures and pop-out windows (not saved).
    #[serde(skip)]
    pub preview: super::record_preview::PreviewCache,
    /// The border window's own state (not saved).
    #[serde(skip)]
    pub overlay_cache: super::record_overlay::OverlayCache,
}

/// UI command `window.record`: open the panel (and list the devices).
pub fn route(app: &mut FilmcraftApp, id: &str) -> Option<Result<Value, String>> {
    match id {
        "window.record" => {
            open(app);
            Some(Ok(json!({"open": true})))
        }
        _ => None,
    }
}

pub fn open(app: &mut FilmcraftApp) {
    tick(app);
    app.ui.record.open = true;
    app.ui.record.overlay_dismissed = false;
    refresh(app);
}

fn refresh(app: &mut FilmcraftApp) {
    match app.session.execute("record.devices", json!({})) {
        Ok(v) => {
            let first_cam = v["cameras"][0]["id"].as_str().map(|id| camera_row(app, id.to_string()));
            let r = &mut app.ui.record;
            if !r.initialized {
                r.initialized = true;
                if r.screen.is_empty()
                    && let Some(id) = v["displays"][0]["id"].as_str()
                {
                    r.screen = format!("display:{id}");
                }
                if r.cameras.is_empty()
                    && let Some(row) = first_cam
                {
                    r.cameras.push(row);
                }
                if r.mics.is_empty() {
                    r.mics.push(MicRow { device: "default".into() });
                }
            }
            r.error = v["error"].as_str().unwrap_or("").to_string();
            r.devices = Some(v);
        }
        Err(e) => app.ui.record.error = e.to_string(),
    }
}

/// "Recording in 3…" (whole seconds left, rounded up).
fn countdown_text(left: f64) -> String {
    let n = if left.is_finite() { left.max(0.0).ceil() as u64 } else { 0 };
    format!("Recording in {n}…")
}

fn mmss(secs: f64) -> String {
    let s = if secs.is_finite() { secs.max(0.0) as u64 } else { 0 };
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// The status-bar line while recording (None when not recording).
pub fn status_line(app: &FilmcraftApp) -> Option<String> {
    if let Some(left) = app.session.record.countdown_left() {
        return Some(countdown_text(left));
    }
    if let Some(l) = app.session.record.starting_label() {
        return Some(l.to_string());
    }
    if !app.session.record.recording() {
        return None;
    }
    let st = filmcraft_engine::record::status_of(&app.session);
    let frames = |kind: &str| st["sources"].as_array().and_then(|a| a.iter().find(|s| s["kind"] == kind)).and_then(|s| s["frames"].as_u64());
    let mut parts = Vec::new();
    for k in ["screen", "camera"] {
        if let Some(n) = frames(k) {
            parts.push(format!("{k} {n} f"));
        }
    }
    if parts.is_empty() && frames("mic").is_some() {
        parts.push("microphone".into());
    }
    Some(format!("Recording · {} · {}", mmss(st["elapsed"].as_f64().unwrap_or(0.0)), parts.join(" / ")))
}

/// The `record.start` parameters for the panel's choices.
pub fn start_params(r: &RecordUi) -> Value {
    let mut p = serde_json::Map::new();
    if let Some(id) = r.screen.strip_prefix("display:") {
        match r.screen_area {
            Some(a) => p.insert("screen".into(), json!({"display": id, "area": a})),
            None => p.insert("screen".into(), json!({"display": id})),
        };
    } else if let Some(id) = r.screen.strip_prefix("window:") {
        p.insert("screen".into(), json!({"window": id}));
    }
    let cameras: Vec<Value> = r
        .cameras
        .iter()
        .filter(|c| !c.device.is_empty())
        .map(|c| {
            let mut v = json!({"device": c.device});
            if let Some(o) = v.as_object_mut() {
                if let Some((w, h)) = c.quality.size() {
                    o.insert("width".into(), json!(w));
                    o.insert("height".into(), json!(h));
                }
                if c.mirror {
                    o.insert("mirror".into(), json!(true));
                }
            }
            v
        })
        .collect();
    if !cameras.is_empty() {
        p.insert("cameras".into(), Value::Array(cameras));
    }
    let mics: Vec<Value> =
        r.mics.iter().filter(|m| !m.device.is_empty()).map(|m| if m.device == "default" { json!({}) } else { json!({"device": m.device}) }).collect();
    if !mics.is_empty() {
        p.insert("mics".into(), Value::Array(mics));
    }
    if !r.name.trim().is_empty() {
        p.insert("name".into(), json!(r.name.trim()));
    }
    Value::Object(p)
}

/// The `record.stop` parameters: each recorded camera's offset, in row order.
pub fn stop_params(r: &RecordUi) -> Value {
    let offsets: Vec<f64> =
        r.cameras.iter().filter(|c| !c.device.is_empty()).map(|c| if c.offset_ms.is_finite() { c.offset_ms.clamp(-5000.0, 5000.0) } else { 0.0 }).collect();
    if offsets.iter().all(|o| *o == 0.0) { json!({}) } else { json!({"cameraOffsetsMs": offsets}) }
}

/// Run what the engine has due (a counted-down start, Stop after) and show what happened.
pub fn tick(app: &mut FilmcraftApp) {
    let Some(ev) = filmcraft_engine::record::tick(&mut app.session) else { return };
    match ev["event"].as_str() {
        Some("started") => {
            app.ui.record.error.clear();
            app.ui.record.last.clear();
            app.ui.status = format!("Recording {}", ev["result"]["name"].as_str().unwrap_or(""));
        }
        Some("stoppedAfter") => {
            let v = &ev["result"];
            app.ui.record.last = format!(
                "Stopped after {} min: {} — {} file(s) in a new sequence",
                ev["minutes"],
                v["name"].as_str().unwrap_or("Recording"),
                v["files"].as_array().map_or(0, Vec::len)
            );
            app.ui.status = app.ui.record.last.clone();
            app.ui.record.live.clear();
        }
        _ => {
            let e = ev["error"].as_str().unwrap_or("recording failed").to_string();
            app.ui.record.error = e.clone();
            app.ui.status = e;
            app.ui.record.live.clear();
        }
    }
}

/// Record / Stop (and Cancel of a countdown).
pub fn toggle(app: &mut FilmcraftApp) {
    if app.session.record.countdown.is_some() {
        cancel(app);
        return;
    }
    if app.session.record.starting.is_some() {
        // the button is disabled while the sources start; Cancel stops them
        return;
    }
    if app.session.record.recording() {
        let p = stop_params(&app.ui.record);
        match app.session.execute("record.stop", p) {
            Ok(v) => {
                let errs: Vec<&str> = v["errors"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                let name = v["name"].as_str().unwrap_or("Recording");
                app.ui.record.last = format!("Recorded {name}: {} file(s) in a new sequence", v["files"].as_array().map_or(0, Vec::len));
                app.ui.record.error = errs.join("; ");
                app.ui.status = app.ui.record.last.clone();
            }
            Err(e) => {
                app.ui.record.error = e.to_string();
                app.ui.status = e.to_string();
            }
        }
        app.ui.record.live.clear();
        return;
    }
    // the border shows (and exists natively) before the screen stream starts, so the stream
    // leaves it out from its first frame
    app.ui.record.overlay_dismissed = false;
    app.ui.record.drawing = false;
    if !super::record_overlay::ready_for_start(app) {
        app.ui.record.overlay_cache.lock().pending_start = true;
        app.ui.status = "Showing the recording border…".into();
        return;
    }
    let mut p = start_params(&app.ui.record);
    let countdown = app.session.prefs.recording.countdown_seconds;
    if let Some(o) = p.as_object_mut() {
        // the sources start on their own thread: the frame loop shows "Starting screen capture…"
        o.insert("wait".into(), json!(false));
        if countdown > 0 {
            o.insert("countdown".into(), json!(countdown));
        }
    }
    match app.session.execute("record.start", p) {
        Ok(v) => {
            app.ui.record.error.clear();
            app.ui.record.last.clear();
            app.ui.status = match (v["countdown"].as_u64(), v["label"].as_str()) {
                (Some(n), _) => countdown_text(n as f64),
                (None, Some(l)) => l.to_string(),
                (None, None) => format!("Recording {}", v["name"].as_str().unwrap_or("")),
            };
        }
        Err(e) => {
            app.ui.record.error = e.to_string();
            app.ui.status = e.to_string();
        }
    }
}

pub fn cancel(app: &mut FilmcraftApp) {
    let starting = app.session.record.starting.is_some();
    if !app.session.record.recording() && app.session.record.countdown.is_none() && !starting {
        return;
    }
    match app.session.execute("record.cancel", json!({})) {
        Ok(v) => {
            app.ui.record.last = if starting {
                "Start cancelled".into()
            } else if v["cancelled"] == true {
                "Countdown cancelled".into()
            } else {
                "Recording discarded".into()
            };
            app.ui.status = app.ui.record.last.clone();
        }
        Err(e) => app.ui.record.error = e.to_string(),
    }
    app.ui.record.live.clear();
}

fn combo(ui: &mut egui::Ui, elems: &mut Vec<(String, Rect, String)>, id: &str, value: &mut String, options: &[(String, String)], enabled: bool, width: f32) {
    let shown = options.iter().find(|o| o.0 == *value).map(|o| o.1.clone()).unwrap_or_else(|| if value.is_empty() { "Off".into() } else { value.clone() });
    let cb = ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(id).selected_text(&shown).width(width).show_ui(ui, |ui| {
            for (i, (val, label)) in options.iter().enumerate() {
                let r = ui.selectable_label(value == val, label);
                elems.push((format!("{id}.{i}"), r.rect, label.clone()));
                if r.clicked() {
                    *value = val.clone();
                }
            }
        })
    });
    elems.push((id.to_string(), cb.inner.response.rect, shown));
}

/// Every frame: the panel (while open or recording) and the live status-bar line.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    tick(app);
    // live camera previews (and their pop-out windows, which stay while recording)
    super::record_preview::sync(app, ctx);
    // the border around what is (or will be) recorded
    super::record_overlay::show(app, ctx);
    let pending = std::mem::take(&mut app.ui.record.overlay_cache.lock().pending_start);
    if pending && !app.session.record.recording() && app.session.record.countdown.is_none() {
        if super::record_overlay::ready_for_start(app) {
            toggle(app);
        } else {
            app.ui.record.overlay_cache.lock().pending_start = true;
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
    let counting = app.session.record.countdown.is_some();
    if counting {
        app.ui.record.live = status_line(app).unwrap_or_default();
        app.ui.record.open = true;
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        // Esc cancels the countdown
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel(app);
            return;
        }
    }
    let starting_label = app.session.record.starting_label().map(str::to_string);
    if starting_label.is_some() {
        app.ui.record.live = status_line(app).unwrap_or_default();
        app.ui.record.open = true;
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    let recording = app.session.record.recording();
    if recording {
        app.ui.record.live = status_line(app).unwrap_or_default();
        app.ui.record.open = true;
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    } else if !counting && starting_label.is_none() && !app.ui.record.live.is_empty() {
        // stopped from elsewhere (`record.stop` over the control channel)
        app.ui.record.live.clear();
    }
    let recording = recording || counting || starting_label.is_some();
    if !app.ui.record.open {
        return;
    }
    if app.ui.record.devices.is_none() {
        refresh(app);
    }
    let st = app.session.record.recording().then(|| filmcraft_engine::record::status_of(&app.session));
    let countdown_left = app.session.record.countdown_left();
    let dev = app.ui.record.devices.clone().unwrap_or_default();
    let list = |key: &str, f: &dyn Fn(&Value) -> (String, String)| -> Vec<(String, String)> {
        dev[key].as_array().map(|a| a.iter().map(f).collect()).unwrap_or_default()
    };
    let mut screens = vec![(String::new(), "Off".to_string())];
    screens.extend(list("displays", &|d| {
        (format!("display:{}", d["id"].as_str().unwrap_or("")), format!("{} ({}×{})", d["name"].as_str().unwrap_or("Display"), d["width"], d["height"]))
    }));
    // "Area of <display>…": draw a part of it on the border
    screens
        .extend(list("displays", &|d| (format!("area:{}", d["id"].as_str().unwrap_or("")), format!("Area of {}…", d["name"].as_str().unwrap_or("Display")))));
    screens.extend(list("windows", &|w| {
        (format!("window:{}", w["id"].as_str().unwrap_or("")), format!("{} — {}", w["app"].as_str().unwrap_or(""), w["title"].as_str().unwrap_or("")))
    }));
    let mut cameras = vec![(String::new(), "Off".to_string())];
    cameras.extend(list("cameras", &|c| (c["id"].as_str().unwrap_or("").to_string(), c["name"].as_str().unwrap_or("Camera").to_string())));
    let mut mics = vec![(String::new(), "Off".to_string()), ("default".to_string(), "Default Input".to_string())];
    mics.extend(
        dev["microphones"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).map(|m| (m.to_string(), m.to_string())).collect::<Vec<_>>())
            .unwrap_or_default(),
    );

    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut r = app.ui.record.clone();
    let new_cam = camera_row(app, String::new());
    let rs = app.session.prefs.recording.clone();
    let hevc = filmcraft_engine::record_settings::hevc_available();
    let sys_ok = dev["systemAudio"].as_bool().unwrap_or(false);
    let mut patch = serde_json::Map::new();
    let mut choose_folder = false;
    let (mut do_toggle, mut do_cancel, mut do_refresh, mut close) = (false, false, false, false);
    let accent = app.tokens.accent;
    let dim = app.tokens.text_dim;
    let preview = app.ui.record.preview.clone();
    let session = &app.session;
    let win = egui::Window::new("Record")
        .id(egui::Id::new("record-panel"))
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center_top() + vec2(-260.0, 40.0))
        .show(ctx, |ui| {
            egui::Grid::new("record-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Screen:");
                ui.vertical(|ui| {
                    // an area shows as its display's "Area of …" entry
                    let shown = match (r.screen.strip_prefix("display:"), r.screen_area.is_some() || r.drawing) {
                        (Some(id), true) => format!("area:{id}"),
                        _ => r.screen.clone(),
                    };
                    let mut sel = shown.clone();
                    let first = elems.len();
                    combo(ui, &mut elems, "record.panel.screen", &mut sel, &screens, !recording, 300.0);
                    // the area entries also answer to `record.panel.screen.area.<display>`
                    let aliases: Vec<(String, Rect, String)> = elems[first..]
                        .iter()
                        .filter_map(|(id, rect, label)| {
                            let i: usize = id.strip_prefix("record.panel.screen.")?.parse().ok()?;
                            let d = screens.get(i)?.0.strip_prefix("area:")?;
                            Some((format!("record.panel.screen.area.{d}"), *rect, label.clone()))
                        })
                        .collect();
                    elems.extend(aliases);
                    if sel != shown {
                        r.screen_area = None;
                        r.overlay_dismissed = false;
                        match sel.strip_prefix("area:") {
                            Some(id) => {
                                r.screen = format!("display:{id}");
                                r.drawing = true;
                            }
                            None => {
                                r.screen = sel;
                                r.drawing = false;
                            }
                        }
                    }
                    if r.drawing {
                        let l = ui.label(RichText::new("Drag the area on the screen (Esc cancels)").small().color(dim));
                        elems.push(("record.panel.screen.area".into(), l.rect, "drawing".into()));
                    } else if let Some([_, _, w, h]) = r.screen_area {
                        ui.horizontal(|ui| {
                            let l = ui.label(format!("Area {w}×{h}"));
                            elems.push(("record.panel.screen.area".into(), l.rect, format!("Area {w}×{h}")));
                            let e = ui.add_enabled(!recording, egui::Button::new("Edit…").small());
                            elems.push(("record.panel.screen.area.edit".into(), e.rect, "Edit…".into()));
                            if e.clicked() {
                                r.drawing = true;
                                r.overlay_dismissed = false;
                            }
                        });
                    }
                });
                ui.end_row();
                let qs: Vec<(String, String)> = Quality::ALL.iter().map(|q| (q.label().to_string(), q.label().to_string())).collect();
                let mut remove_cam = None;
                for (i, c) in r.cameras.iter_mut().enumerate() {
                    let n = i + 1;
                    let id = format!("record.panel.camera.{n}");
                    ui.label(if n == 1 { "Camera:".to_string() } else { format!("Camera {n}:") });
                    ui.horizontal(|ui| {
                        super::record_preview::thumbnail(ui, &mut elems, &preview, session, n, c);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                combo(ui, &mut elems, &format!("{id}.device"), &mut c.device, &cameras, !recording, 220.0);
                                let mut q = c.quality.label().to_string();
                                combo(ui, &mut elems, &format!("{id}.quality"), &mut q, &qs, !recording && !c.device.is_empty(), 70.0);
                                c.quality = Quality::ALL.into_iter().find(|x| x.label() == q).unwrap_or(c.quality);
                                let x = ui.add_enabled(!recording, egui::Button::new("−").small());
                                elems.push((format!("{id}.remove"), x.rect, format!("Remove camera {n}")));
                                if x.clicked() {
                                    remove_cam = Some(i);
                                }
                            });
                            ui.horizontal(|ui| {
                                let m = ui.add_enabled(!recording && !c.device.is_empty(), egui::Checkbox::new(&mut c.mirror, "Mirror"));
                                elems.push((format!("{id}.mirror"), m.rect, format!("Mirror {}", c.mirror)));
                                ui.label("Offset:");
                                let o = ui
                                    .add_enabled(!c.device.is_empty(), egui::DragValue::new(&mut c.offset_ms).speed(1.0).range(-5000.0..=5000.0).suffix(" ms"));
                                elems.push((format!("{id}.offset"), o.rect, format!("{}", c.offset_ms)));
                            });
                            super::record_preview::popout_button(ui, &mut elems, session, n, c);
                        });
                    });
                    ui.end_row();
                }
                if let Some(i) = remove_cam {
                    r.cameras.remove(i);
                }
                let mut remove_mic = None;
                for (i, m) in r.mics.iter_mut().enumerate() {
                    let n = i + 1;
                    let id = format!("record.panel.mic.{n}");
                    ui.label(if n == 1 { "Microphone:".to_string() } else { format!("Microphone {n}:") });
                    ui.horizontal(|ui| {
                        combo(ui, &mut elems, &format!("{id}.device"), &mut m.device, &mics, !recording, 300.0);
                        let x = ui.add_enabled(!recording, egui::Button::new("−").small());
                        elems.push((format!("{id}.remove"), x.rect, format!("Remove microphone {n}")));
                        if x.clicked() {
                            remove_mic = Some(i);
                        }
                    });
                    ui.end_row();
                }
                if let Some(i) = remove_mic {
                    r.mics.remove(i);
                }
                ui.label("");
                ui.horizontal(|ui| {
                    let a = ui.add_enabled(!recording && r.cameras.len() < MAX_ROWS, egui::Button::new("+ Camera"));
                    elems.push(("record.panel.camera.add".into(), a.rect, "+ Camera".into()));
                    if a.clicked() {
                        let used: Vec<&str> = r.cameras.iter().map(|c| c.device.as_str()).collect();
                        let next = cameras.iter().map(|c| c.0.clone()).find(|id| !id.is_empty() && !used.contains(&id.as_str())).unwrap_or_default();
                        r.cameras.push(CameraRow { device: next, ..new_cam.clone() });
                    }
                    let a = ui.add_enabled(!recording && r.mics.len() < MAX_ROWS, egui::Button::new("+ Microphone"));
                    elems.push(("record.panel.mic.add".into(), a.rect, "+ Microphone".into()));
                    if a.clicked() {
                        r.mics.push(MicRow::default());
                    }
                });
                ui.end_row();
                ui.label("Name:");
                let t = ui.add_enabled(!recording, egui::TextEdit::singleline(&mut r.name).hint_text("Recording <n>").desired_width(300.0));
                elems.push(("record.panel.name".into(), t.rect, r.name.clone()));
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if let Some(l) = &starting_label {
                    l.clone()
                } else if let Some(left) = countdown_left {
                    format!("{}  (Cancel)", countdown_text(left))
                } else if recording {
                    format!("■  Stop  {}", mmss(st.as_ref().and_then(|s| s["elapsed"].as_f64()).unwrap_or(0.0)))
                } else {
                    "●  Record".to_string()
                };
                // disabled while the sources start (Cancel stops them)
                let b = ui.add_enabled(
                    starting_label.is_none(),
                    egui::Button::new(RichText::new(&label).color(Color32::WHITE).size(16.0)).fill(RED).corner_radius(18.0).min_size(vec2(150.0, 36.0)),
                );
                elems.push(("record.panel.record".into(), b.rect, label));
                if b.clicked() {
                    do_toggle = true;
                }
                let c = ui.add_enabled(recording, egui::Button::new("Cancel"));
                elems.push(("record.panel.cancel".into(), c.rect, "Cancel (discard the recording)".into()));
                if c.clicked() {
                    do_cancel = true;
                }
                let f = ui.add_enabled(!recording, egui::Button::new("Refresh"));
                elems.push(("record.panel.refresh".into(), f.rect, "Refresh devices".into()));
                if f.clicked() {
                    do_refresh = true;
                }
            });
            // live counters and the mic level
            let counters = match &st {
                Some(s) => s["sources"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter(|x| x["kind"] != "mic")
                            .map(|x| format!("{}: {} frames, {} dropped", x["kind"].as_str().unwrap_or(""), x["frames"], x["dropped"]))
                            .collect::<Vec<_>>()
                            .join("   ")
                    })
                    .unwrap_or_default(),
                None => r.last.clone(),
            };
            let l = ui.label(RichText::new(if counters.is_empty() { " ".to_string() } else { counters.clone() }).small().color(dim));
            elems.push(("record.panel.counters".into(), l.rect, counters));
            let level = st
                .as_ref()
                .and_then(|s| s["sources"].as_array().and_then(|a| a.iter().find(|x| x["kind"] == "mic")).and_then(|m| m["level"].as_f64()))
                .unwrap_or(0.0)
                .clamp(0.0, 1.0) as f32;
            let (rect, _) = ui.allocate_exact_size(vec2(260.0, 6.0), Sense::hover());
            ui.painter().rect_filled(rect, 2.0, Color32::from_gray(50));
            let lv = Rect::from_min_size(rect.min, vec2(rect.width() * level, rect.height()));
            ui.painter().rect_filled(lv, 2.0, if level > 0.9 { RED } else { accent });
            elems.push(("record.panel.level".into(), rect, format!("{level:.2}")));
            if !r.error.is_empty() {
                let mut text = r.error.clone();
                if text.contains("System Settings") {
                    text.push_str("\nOpen System Settings ▸ Privacy & Security to allow it.");
                }
                let e = ui.label(RichText::new(text).small().color(RED));
                elems.push(("record.panel.error".into(), e.rect, r.error.clone()));
            }
            ui.add_space(4.0);
            let hdr = egui::CollapsingHeader::new("Settings").id_salt("record-panel-settings").open(Some(r.settings_open)).show(ui, |ui| {
                settings_section(
                    ui,
                    &mut elems,
                    &rs,
                    &mut patch,
                    SectionCtx { enabled: !recording, hevc, sys_ok, has_screen: !r.screen.is_empty() },
                    &mut choose_folder,
                );
            });
            elems.push(("record.panel.settings".into(), hdr.header_response.rect, "Settings".into()));
            if hdr.header_response.clicked() {
                r.settings_open = !r.settings_open;
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Each source is its own file; Stop builds a synced sequence.").small().weak());
                let x = ui.add_enabled(!recording, egui::Button::new("Close"));
                elems.push(("record.panel.close".into(), x.rect, "Close".into()));
                if x.clicked() {
                    close = true;
                }
            });
        });
    if let Some(w) = win {
        elems.push(("record.panel".into(), w.response.rect, "Record".into()));
    }
    for (id, rect, label) in elems {
        app.auto.add(&id, rect, &label);
    }
    app.ui.record = RecordUi { devices: app.ui.record.devices.take(), ..r };
    if choose_folder && let Some(dir) = app.hooks.pick_folder.as_mut().and_then(|p| p()) {
        patch.insert("outputFolder".into(), json!(dir));
    }
    if !patch.is_empty()
        && let Err(e) = app.session.execute("record.settings", json!({"set": Value::Object(patch)}))
    {
        app.ui.record.error = e.to_string();
    }
    if do_refresh {
        refresh(app);
    }
    if do_toggle {
        toggle(app);
    }
    if do_cancel {
        cancel(app);
    }
    if close && !app.session.record.recording() {
        app.ui.record.open = false;
    }
}

/// What the Settings section needs to know.
struct SectionCtx {
    enabled: bool,
    hevc: bool,
    sys_ok: bool,
    has_screen: bool,
}

fn opts(o: &[(&str, &str)]) -> Vec<(String, String)> {
    o.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect()
}

/// The Settings section of the panel: the same values as Settings ▸ Recording (`record.settings`);
/// a change goes into `patch` (`{field: value}`), written after the frame.
fn settings_section(
    ui: &mut egui::Ui,
    elems: &mut Vec<(String, Rect, String)>,
    rs: &filmcraft_engine::record_settings::RecordingSettings,
    patch: &mut serde_json::Map<String, Value>,
    cx: SectionCtx,
    choose_folder: &mut bool,
) {
    use filmcraft_engine::record_settings as k;
    let id = |key: &str| format!("record.panel.settings.{key}");
    let choice = |ui: &mut egui::Ui,
                  elems: &mut Vec<(String, Rect, String)>,
                  patch: &mut serde_json::Map<String, Value>,
                  key: &str,
                  label: &str,
                  cur: String,
                  o: Vec<(String, String)>,
                  numeric: bool,
                  enabled: bool| {
        ui.label(label);
        let mut v = cur.clone();
        combo(ui, elems, &id(key), &mut v, &o, enabled, 200.0);
        if v != cur {
            patch.insert(key.into(), if numeric { v.parse::<u64>().map(Value::from).unwrap_or(json!(v)) } else { json!(v) });
        }
        ui.end_row();
    };
    let check = |ui: &mut egui::Ui,
                 elems: &mut Vec<(String, Rect, String)>,
                 patch: &mut serde_json::Map<String, Value>,
                 key: &str,
                 label: &str,
                 cur: bool,
                 enabled: bool| {
        ui.label("");
        let mut b = cur;
        let r = ui.add_enabled(enabled, egui::Checkbox::new(&mut b, label));
        elems.push((id(key), r.rect, format!("{label} {b}")));
        if b != cur {
            patch.insert(key.into(), json!(b));
        }
        ui.end_row();
    };
    let head = |ui: &mut egui::Ui, t: &str| {
        ui.label(RichText::new(t).strong());
        ui.end_row();
    };
    let en = cx.enabled;
    egui::Grid::new("record-settings-grid").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
        head(ui, "Screen");
        choice(ui, elems, patch, "screenFps", "Frame rate:", rs.screen_fps.to_string(), opts(k::SCREEN_FPS), true, en);
        choice(ui, elems, patch, "screenResolution", "Resolution:", rs.screen_resolution.clone(), opts(k::SCREEN_RESOLUTION), false, en);
        check(ui, elems, patch, "showCursor", "Show cursor", rs.show_cursor, en);
        let sys_label = if cx.sys_ok { "System audio (with a screen)" } else { "System audio (not available yet)" };
        check(ui, elems, patch, "systemAudio", sys_label, rs.system_audio, en && cx.sys_ok && cx.has_screen);
        head(ui, "Camera (new rows)");
        choice(ui, elems, patch, "cameraQuality", "Quality:", rs.camera_quality.clone(), opts(k::CAMERA_QUALITY), false, en);
        choice(ui, elems, patch, "cameraFps", "Frame rate:", rs.camera_fps.to_string(), opts(k::CAMERA_FPS), true, en);
        check(ui, elems, patch, "cameraMirror", "Mirror", rs.camera_mirror, en);
        head(ui, "Encoding");
        let codecs: Vec<(String, String)> = opts(k::CODECS).into_iter().filter(|(v, _)| cx.hevc || v != "hevc" || rs.codec == "hevc").collect();
        choice(ui, elems, patch, "codec", "Codec:", rs.codec.clone(), codecs, false, en);
        choice(ui, elems, patch, "quality", "Quality:", rs.quality.clone(), opts(k::QUALITIES), false, en && rs.codec != "prores");
        choice(ui, elems, patch, "keyframeSeconds", "Keyframe every:", rs.keyframe_seconds.to_string(), opts(k::KEYFRAMES), true, en && rs.codec != "prores");
        check(ui, elems, patch, "hardwareEncoder", "Hardware encoder", rs.hardware_encoder, en);
        if !rs.hardware_encoder {
            ui.label("");
            ui.label(RichText::new("FilmCraft's own encoder: at most 15 fps above 1080p").small().weak());
            ui.end_row();
        }
        head(ui, "Audio");
        choice(ui, elems, patch, "sampleRate", "Sample rate:", rs.sample_rate.to_string(), opts(k::SAMPLE_RATES), true, en);
        choice(ui, elems, patch, "channels", "Channels:", rs.channels.clone(), opts(k::CHANNELS), false, en);
        choice(ui, elems, patch, "audioFormat", "Format:", rs.audio_format.clone(), opts(k::AUDIO_FORMATS), false, en);
        check(ui, elems, patch, "autoGain", "Auto gain (−18 dBFS, file only)", rs.auto_gain, en);
        head(ui, "Behaviour");
        choice(ui, elems, patch, "countdownSeconds", "Countdown:", rs.countdown_seconds.to_string(), opts(k::COUNTDOWNS), true, en);
        ui.label("Stop after:");
        let mut m = rs.stop_after_minutes;
        let d = ui.add_enabled(
            en,
            egui::DragValue::new(&mut m)
                .range(0..=k::MAX_STOP_AFTER_MINUTES)
                .custom_formatter(|v, _| if v < 0.5 { "Off".into() } else { format!("{v:.0} min") }),
        );
        elems.push((id("stopAfterMinutes"), d.rect, format!("{m}")));
        if m != rs.stop_after_minutes {
            patch.insert("stopAfterMinutes".into(), json!(m));
        }
        ui.end_row();
        check(ui, elems, patch, "openSequence", "Open the sequence after Stop", rs.open_sequence, en);
        ui.label("Output folder:");
        ui.horizontal(|ui| {
            let mut f = rs.output_folder.clone();
            let t = ui.add_enabled(en, egui::TextEdit::singleline(&mut f).hint_text("Captured Audio and Video scratch disk").desired_width(200.0));
            elems.push((id("outputFolder"), t.rect, f.clone()));
            if t.lost_focus() && f != rs.output_folder {
                patch.insert("outputFolder".into(), json!(f));
            }
            let b = ui.add_enabled(en, egui::Button::new("Choose…"));
            elems.push((id("outputFolder.choose"), b.rect, "Choose…".into()));
            if b.clicked() {
                *choose_folder = true;
            }
        });
        ui.end_row();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_from_the_choices() {
        let r = RecordUi {
            screen: "display:7".into(),
            cameras: vec![CameraRow { device: "cam".into(), quality: Quality::P720, ..Default::default() }],
            mics: vec![MicRow { device: "default".into() }],
            name: " Take ".into(),
            ..Default::default()
        };
        assert_eq!(
            start_params(&r),
            json!({"screen": {"display": "7"}, "cameras": [{"device": "cam", "width": 1280, "height": 720}], "mics": [{}], "name": "Take"})
        );
        assert_eq!(stop_params(&r), json!({}));
        let r = RecordUi {
            screen: "window:42".into(),
            cameras: vec![
                CameraRow { device: "a".into(), quality: Quality::Native, mirror: true, ..Default::default() },
                CameraRow::default(),
                CameraRow { device: "b".into(), quality: Quality::Native, mirror: false, offset_ms: -80.0, ..Default::default() },
            ],
            mics: vec![MicRow { device: "USB Mic".into() }, MicRow::default()],
            ..Default::default()
        };
        assert_eq!(
            start_params(&r),
            json!({"screen": {"window": "42"}, "cameras": [{"device": "a", "mirror": true}, {"device": "b"}], "mics": [{"device": "USB Mic"}]})
        );
        assert_eq!(stop_params(&r), json!({"cameraOffsetsMs": [0.0, -80.0]}));
        assert_eq!(start_params(&RecordUi::default()), json!({}));
        assert_eq!(mmss(75.9), "01:15");
        assert_eq!(mmss(f64::NAN), "00:00");
    }
}
