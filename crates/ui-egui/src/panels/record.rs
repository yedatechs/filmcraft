//! The Record panel (Window ▸ Record, or the red dot at the right of the Program monitor's
//! transport): pick a screen (a display or a window), a camera and its quality, a microphone and a
//! name, press Record, and Stop builds a synced sequence of the three files. Recording itself is
//! the engine's `record.*` commands (`filmcraft_engine::record`, docs/recording.md).
//!
//! Automation ids: `record.panel` (the window), `record.panel.screen` (+ `.screen.<i>`, 0 = Off),
//! `record.panel.camera` (+ `.camera.<i>`), `record.panel.quality` (+ `.quality.<i>`),
//! `record.panel.mic` (+ `.mic.<i>`), `record.panel.name`, `record.panel.offset`,
//! `record.panel.record` (Record / Stop), `record.panel.cancel`, `record.panel.refresh`,
//! `record.panel.counters`, `record.panel.level`, `record.panel.error`, `record.panel.close`; the
//! Program monitor button is `program.transport.record`. UI command: `window.record` (open).
//! The panel state is `UiState::record` ([`RecordUi`]), so `ui.set {"record": {...}}` drives it.
//! No keyboard shortcuts.

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
    fn size(self) -> Option<(u32, u32)> {
        match self {
            Quality::P720 => Some((1280, 720)),
            Quality::P1080 => Some((1920, 1080)),
            Quality::P2160 => Some((3840, 2160)),
            Quality::Native => None,
        }
    }
}

/// Record panel state (`UiState::record`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RecordUi {
    pub open: bool,
    /// `""` = Off, `display:<id>` or `window:<id>`.
    pub screen: String,
    /// `""` = Off, else a camera id.
    pub camera: String,
    pub quality: Quality,
    /// `""` = Off, `default` = the default input, else a microphone name.
    pub mic: String,
    /// Recording name (`""` = `Recording <n>`).
    pub name: String,
    /// "Offset camera by … ms", applied at Stop.
    pub offset_ms: f64,
    /// The last error (a permission, a device), shown in the panel.
    pub error: String,
    /// The outcome of the last recording ("Recorded …").
    pub last: String,
    /// While recording: the status-bar line (`Recording · 00:12 · screen 360 f / camera 358 f`).
    pub live: String,
    /// The sources were preselected once (first display, first camera, default mic).
    pub initialized: bool,
    /// `record.devices`, refreshed when the panel opens (not saved).
    #[serde(skip)]
    pub devices: Option<Value>,
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
    app.ui.record.open = true;
    refresh(app);
}

fn refresh(app: &mut FilmcraftApp) {
    match app.session.execute("record.devices", json!({})) {
        Ok(v) => {
            let r = &mut app.ui.record;
            if !r.initialized {
                r.initialized = true;
                if r.screen.is_empty()
                    && let Some(id) = v["displays"][0]["id"].as_str()
                {
                    r.screen = format!("display:{id}");
                }
                if r.camera.is_empty()
                    && let Some(id) = v["cameras"][0]["id"].as_str()
                {
                    r.camera = id.to_string();
                }
                if r.mic.is_empty() {
                    r.mic = "default".into();
                }
            }
            r.error = v["error"].as_str().unwrap_or("").to_string();
            r.devices = Some(v);
        }
        Err(e) => app.ui.record.error = e.to_string(),
    }
}

fn mmss(secs: f64) -> String {
    let s = if secs.is_finite() { secs.max(0.0) as u64 } else { 0 };
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// The status-bar line while recording (None when not recording).
pub fn status_line(app: &FilmcraftApp) -> Option<String> {
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
        p.insert("screen".into(), json!({"display": id}));
    } else if let Some(id) = r.screen.strip_prefix("window:") {
        p.insert("screen".into(), json!({"window": id}));
    }
    if !r.camera.is_empty() {
        let mut c = json!({"device": r.camera});
        if let (Some((w, h)), Some(o)) = (r.quality.size(), c.as_object_mut()) {
            o.insert("width".into(), json!(w));
            o.insert("height".into(), json!(h));
        }
        p.insert("camera".into(), c);
    }
    match r.mic.as_str() {
        "" => {}
        "default" => {
            p.insert("mic".into(), json!({}));
        }
        m => {
            p.insert("mic".into(), json!({"device": m}));
        }
    }
    if !r.name.trim().is_empty() {
        p.insert("name".into(), json!(r.name.trim()));
    }
    Value::Object(p)
}

/// Record / Stop.
pub fn toggle(app: &mut FilmcraftApp) {
    if app.session.record.recording() {
        let mut p = json!({});
        if !app.ui.record.camera.is_empty() && app.ui.record.offset_ms != 0.0 {
            p = json!({"cameraOffsetMs": app.ui.record.offset_ms.clamp(-5000.0, 5000.0)});
        }
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
    let p = start_params(&app.ui.record);
    match app.session.execute("record.start", p) {
        Ok(v) => {
            app.ui.record.error.clear();
            app.ui.record.last.clear();
            app.ui.status = format!("Recording {}", v["name"].as_str().unwrap_or(""));
        }
        Err(e) => {
            app.ui.record.error = e.to_string();
            app.ui.status = e.to_string();
        }
    }
}

pub fn cancel(app: &mut FilmcraftApp) {
    if !app.session.record.recording() {
        return;
    }
    match app.session.execute("record.cancel", json!({})) {
        Ok(_) => {
            app.ui.record.last = "Recording discarded".into();
            app.ui.status = app.ui.record.last.clone();
        }
        Err(e) => app.ui.record.error = e.to_string(),
    }
    app.ui.record.live.clear();
}

fn combo(ui: &mut egui::Ui, elems: &mut Vec<(String, Rect, String)>, id: &str, value: &mut String, options: &[(String, String)], enabled: bool) {
    let shown = options.iter().find(|o| o.0 == *value).map(|o| o.1.clone()).unwrap_or_else(|| if value.is_empty() { "Off".into() } else { value.clone() });
    let cb = ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(id).selected_text(&shown).width(260.0).show_ui(ui, |ui| {
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
    let recording = app.session.record.recording();
    if recording {
        app.ui.record.live = status_line(app).unwrap_or_default();
        app.ui.record.open = true;
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    } else if !app.ui.record.live.is_empty() {
        // stopped from elsewhere (`record.stop` over the control channel)
        app.ui.record.live.clear();
    }
    if !app.ui.record.open {
        return;
    }
    if app.ui.record.devices.is_none() {
        refresh(app);
    }
    let st = recording.then(|| filmcraft_engine::record::status_of(&app.session));
    let dev = app.ui.record.devices.clone().unwrap_or_default();
    let list = |key: &str, f: &dyn Fn(&Value) -> (String, String)| -> Vec<(String, String)> {
        dev[key].as_array().map(|a| a.iter().map(f).collect()).unwrap_or_default()
    };
    let mut screens = vec![(String::new(), "Off".to_string())];
    screens.extend(list("displays", &|d| {
        (format!("display:{}", d["id"].as_str().unwrap_or("")), format!("{} ({}×{})", d["name"].as_str().unwrap_or("Display"), d["width"], d["height"]))
    }));
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
    let (mut do_toggle, mut do_cancel, mut do_refresh, mut close) = (false, false, false, false);
    let accent = app.tokens.accent;
    let dim = app.tokens.text_dim;
    let win = egui::Window::new("Record")
        .id(egui::Id::new("record-panel"))
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center() - vec2(200.0, 200.0))
        .show(ctx, |ui| {
            egui::Grid::new("record-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Screen:");
                combo(ui, &mut elems, "record.panel.screen", &mut r.screen, &screens, !recording);
                ui.end_row();
                ui.label("Camera:");
                combo(ui, &mut elems, "record.panel.camera", &mut r.camera, &cameras, !recording);
                ui.end_row();
                ui.label("Quality:");
                let mut q = r.quality.label().to_string();
                let qs: Vec<(String, String)> = Quality::ALL.iter().map(|q| (q.label().to_string(), q.label().to_string())).collect();
                combo(ui, &mut elems, "record.panel.quality", &mut q, &qs, !recording && !r.camera.is_empty());
                r.quality = Quality::ALL.into_iter().find(|x| x.label() == q).unwrap_or(r.quality);
                ui.end_row();
                ui.label("Microphone:");
                combo(ui, &mut elems, "record.panel.mic", &mut r.mic, &mics, !recording);
                ui.end_row();
                ui.label("Name:");
                let t = ui.add_enabled(!recording, egui::TextEdit::singleline(&mut r.name).hint_text("Recording <n>").desired_width(260.0));
                elems.push(("record.panel.name".into(), t.rect, r.name.clone()));
                ui.end_row();
                ui.label("Offset camera by:");
                let o = ui.add_enabled(!r.camera.is_empty(), egui::DragValue::new(&mut r.offset_ms).speed(1.0).range(-5000.0..=5000.0).suffix(" ms"));
                elems.push(("record.panel.offset".into(), o.rect, format!("{}", r.offset_ms)));
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if recording {
                    format!("■  Stop  {}", mmss(st.as_ref().and_then(|s| s["elapsed"].as_f64()).unwrap_or(0.0)))
                } else {
                    "●  Record".to_string()
                };
                let b =
                    ui.add(egui::Button::new(RichText::new(&label).color(Color32::WHITE).size(16.0)).fill(RED).corner_radius(18.0).min_size(vec2(150.0, 36.0)));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_from_the_choices() {
        let r = RecordUi {
            screen: "display:7".into(),
            camera: "cam".into(),
            quality: Quality::P720,
            mic: "default".into(),
            name: " Take ".into(),
            ..Default::default()
        };
        assert_eq!(start_params(&r), json!({"screen": {"display": "7"}, "camera": {"device": "cam", "width": 1280, "height": 720}, "mic": {}, "name": "Take"}));
        let r = RecordUi { screen: "window:42".into(), quality: Quality::Native, mic: "USB Mic".into(), ..Default::default() };
        assert_eq!(start_params(&r), json!({"screen": {"window": "42"}, "mic": {"device": "USB Mic"}}));
        assert_eq!(start_params(&RecordUi::default()), json!({}));
        assert_eq!(mmss(75.9), "01:15");
        assert_eq!(mmss(f64::NAN), "00:00");
    }
}
