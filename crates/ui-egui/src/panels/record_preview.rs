//! Live camera previews of the Record panel: the thumbnail in each camera row and the pop-out
//! preview window (an always-on-top deferred viewport titled [`PREVIEW_TITLE`]…, so a screen
//! recording leaves it out). The engine runs the cameras (`record.preview`,
//! `filmcraft_engine::record_preview`); this module asks for the cameras the panel shows, uploads
//! their latest pictures into textures (only when a new frame arrived) and draws them letterboxed,
//! mirrored / rotated like the clip will be.
//!
//! Automation ids: `record.panel.camera.<n>.preview` (the thumbnail; its label is
//! `<w>×<h> @ <fps> fps · frame <k>`, or `No picture yet`), `record.panel.camera.<n>.popout`
//! (Pop out / Pop in) and `record.preview.<n>` (the pop-out window's picture).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use egui::{Color32, Rect, Sense, TextureHandle, TextureOptions, pos2, vec2};
use serde_json::{Value, json};

use filmcraft_engine::record_preview::{CameraTap, PREVIEW_TITLE, PreviewFrame, oriented};

use super::record::{CameraRow, Quality};
use crate::FilmcraftApp;

/// The thumbnail box in a camera row (16:9).
pub const THUMB: egui::Vec2 = vec2(240.0, 135.0);
/// The thumbnail while a recording runs: a glance at the camera, not a monitor.
pub const THUMB_RECORDING: egui::Vec2 = vec2(120.0, 68.0);

/// The thumbnail's size: smaller while recording.
pub fn thumb_size(recording: bool) -> egui::Vec2 {
    if recording { THUMB_RECORDING } else { THUMB }
}
/// Default width of the pop-out window (points).
pub const POPOUT_WIDTH: f32 = 320.0;

/// A texture of a camera's latest preview picture, with what it was made from.
struct Tex {
    index: u64,
    orient: (bool, u32),
    size: (u32, u32),
    handle: TextureHandle,
}

/// What the pop-out window of one camera shares with its (deferred) viewport.
pub struct Popout {
    pub tap: Arc<CameraTap>,
    pub title: String,
    /// Mirror and Rotate of the row (updated every frame by the panel).
    pub orient: Mutex<(bool, u32)>,
    /// The person closed the window (its close box).
    pub closed: AtomicBool,
    tex: Mutex<Option<Tex>>,
    /// Embedded (headless) only: where the picture was drawn and its label, for automation.
    pub drawn: Mutex<Option<(Rect, String)>>,
}

#[derive(Default)]
pub struct PreviewState {
    /// The `record.preview` cameras last asked for: (device, quality, fps).
    applied: Vec<(String, Quality, u32)>,
    textures: HashMap<String, Tex>,
    pub popouts: HashMap<String, Arc<Popout>>,
}

/// UI-only preview state kept in [`super::record::RecordUi`] (not saved; shared by clones).
#[derive(Clone, Default)]
pub struct PreviewCache(pub Arc<Mutex<PreviewState>>);

impl std::fmt::Debug for PreviewCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreviewCache")
    }
}

impl PartialEq for PreviewCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl PreviewCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, PreviewState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Upload `f` (mirrored / rotated) into `slot` unless it already holds that frame.
fn upload(ctx: &egui::Context, slot: &mut Option<Tex>, name: &str, f: &PreviewFrame, orient: (bool, u32)) -> (egui::TextureId, egui::Vec2) {
    if let Some(t) = slot.as_ref()
        && t.index == f.index
        && t.orient == orient
    {
        return (t.handle.id(), vec2(t.size.0 as f32, t.size.1 as f32));
    }
    let o = oriented(f, orient.0, orient.1);
    let img = egui::ColorImage::from_rgba_unmultiplied([o.width as usize, o.height as usize], &o.rgba);
    match slot {
        Some(t) => {
            t.handle.set(img, TextureOptions::LINEAR);
            t.index = f.index;
            t.orient = orient;
            t.size = (o.width, o.height);
        }
        None => *slot = Some(Tex { index: f.index, orient, size: (o.width, o.height), handle: ctx.load_texture(name, img, TextureOptions::LINEAR) }),
    }
    let size = vec2(o.width as f32, o.height as f32);
    (slot.as_ref().map_or(egui::TextureId::default(), |t| t.handle.id()), size)
}

/// `size` letterboxed into `rect`.
fn letterbox(rect: Rect, size: egui::Vec2) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return rect;
    }
    let s = (rect.width() / size.x).min(rect.height() / size.y);
    Rect::from_center_size(rect.center(), size * s)
}

/// The label of a camera's picture: `1280×720 @ 30 fps · frame 42` (the size as shown: rotated).
fn label(tap: &CameraTap, f: Option<&PreviewFrame>, rotate: u32) -> String {
    match (tap.format(), f) {
        (Some(fmt), Some(f)) => {
            let (w, h) = if rotate % 180 == 90 { (fmt.height, fmt.width) } else { (fmt.width, fmt.height) };
            format!("{w}×{h} @ {} fps · frame {}", fmt.fps, f.index)
        }
        _ => "No picture yet".into(),
    }
}

/// Paint camera `tap`'s picture letterboxed into `rect` (black bars); returns its label.
fn paint(ui: &egui::Ui, slot: &mut Option<Tex>, name: &str, tap: &CameraTap, orient: (bool, u32), rect: Rect) -> String {
    ui.painter().rect_filled(rect, 3.0, Color32::BLACK);
    let f = tap.latest();
    if let Some(f) = f.as_deref() {
        let (id, size) = upload(ui.ctx(), slot, name, f, orient);
        ui.painter().image(id, letterbox(rect, size), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    } else {
        let msg = match tap.error() {
            Some(e) => e,
            None => "Starting the camera…".into(),
        };
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(11.0), Color32::from_gray(160));
    }
    label(tap, f.as_deref(), orient.1)
}

/// How a row's picture is shown: (Mirror, Rotate degrees), like its file will be: with Auto the
/// orientation the camera reports now (it follows a turn made in the camera's own software).
fn orient_of(c: &CameraRow, tap: &CameraTap) -> (bool, u32) {
    (c.mirror, c.rotate.effective(tap.rotation()))
}

/// The cameras the panel wants live: the rows with a camera while the panel is open or a
/// recording runs (closing the panel with nothing recording stops them).
fn wanted(app: &FilmcraftApp) -> Vec<(String, Quality, u32)> {
    let r = &app.ui.record;
    if !(r.open || app.session.record.recording()) {
        return Vec::new();
    }
    let fps = app.session.prefs.recording.camera_fps;
    let mut v: Vec<(String, Quality, u32)> = Vec::new();
    for c in r.cameras.iter().filter(|c| !c.device.is_empty()) {
        if !v.iter().any(|w| w.0 == c.device) {
            v.push((c.device.clone(), c.quality, fps));
        }
    }
    v
}

/// The `record.preview` parameters for `want` (the size each row records at, so a recording
/// reuses the running capture).
pub fn preview_params(want: &[(String, Quality, u32)]) -> Value {
    let cams: Vec<Value> = want
        .iter()
        .map(|(d, q, fps)| {
            // the quality's size, or `native` (the camera's best)
            match q.size() {
                Some((w, h)) => json!({"device": d, "fps": fps, "width": w, "height": h}),
                None => json!({"device": d, "fps": fps, "quality": "native"}),
            }
        })
        .collect();
    json!({"cameras": cams})
}

/// Every frame: start / stop the previews the panel wants, keep the pop-out windows, repaint at
/// the cameras' rate while the panel shows them.
pub fn sync(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let want = wanted(app);
    let cache = app.ui.record.preview.clone();
    let changed = cache.lock().applied != want;
    if changed {
        cache.lock().applied = want.clone();
        if let Err(e) = app.session.execute("record.preview", preview_params(&want)) {
            app.ui.record.error = e.to_string();
        }
    }
    // pop-out windows: one per popped-out row whose camera runs
    let mut rows_out: Vec<(usize, CameraRow)> = Vec::new();
    for (i, c) in app.ui.record.cameras.iter_mut().enumerate() {
        if !c.popout {
            continue;
        }
        let closed = cache.lock().popouts.get(&c.device).is_some_and(|p| p.closed.load(Ordering::Acquire));
        if closed || c.device.is_empty() || !want.iter().any(|w| w.0 == c.device) {
            c.popout = false;
            continue;
        }
        rows_out.push((i, c.clone()));
    }
    let names = camera_names(app);
    {
        let mut st = cache.lock();
        st.popouts.retain(|d, _| rows_out.iter().any(|(_, c)| &c.device == d));
    }
    for (i, c) in &rows_out {
        let Some(tap) = filmcraft_engine::record_preview::preview_of(&app.session, &c.device) else { continue };
        let created = !cache.lock().popouts.contains_key(&c.device);
        let pop = {
            let mut st = cache.lock();
            st.popouts
                .entry(c.device.clone())
                .or_insert_with(|| {
                    let name = names.get(&c.device).cloned().unwrap_or_else(|| c.device.clone());
                    Arc::new(Popout {
                        tap: tap.clone(),
                        title: format!("{PREVIEW_TITLE} — {name}"),
                        orient: Mutex::new(orient_of(c, &tap)),
                        closed: AtomicBool::new(false),
                        tex: Mutex::new(None),
                        drawn: Mutex::new(None),
                    })
                })
                .clone()
        };
        *pop.orient.lock().unwrap_or_else(PoisonError::into_inner) = orient_of(c, &tap);
        if created {
            // a preview window opened during a display recording: leave it out of the file
            super::record_overlay::schedule_refresh(app);
        }
        show_popout(ctx, &pop, i + 1);
        if let Some((rect, label)) = pop.drawn.lock().unwrap_or_else(PoisonError::into_inner).take() {
            app.auto.add(&format!("record.preview.{}", i + 1), rect, &label);
        }
    }
    if app.ui.record.open && !want.is_empty() {
        let fps = want.iter().map(|w| w.2).max().unwrap_or(30).clamp(1, 60);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(1.0 / f64::from(fps)));
    }
}

fn camera_names(app: &FilmcraftApp) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Some(a) = app.ui.record.devices.as_ref().and_then(|d| d["cameras"].as_array()) {
        for c in a {
            if let (Some(id), Some(name)) = (c["id"].as_str(), c["name"].as_str()) {
                m.insert(id.to_string(), name.to_string());
            }
        }
    }
    m
}

fn viewport_id(device: &str) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("record-camera-preview", device))
}

/// The pop-out preview window of camera row `n` (a deferred viewport: it repaints at the camera's
/// rate on its own, also while the panel is closed during a recording).
fn show_popout(ctx: &egui::Context, pop: &Arc<Popout>, n: usize) {
    let aspect = pop.tap.format().map_or(9.0 / 16.0, |f| {
        let (w, h) = if pop.orient.lock().unwrap_or_else(PoisonError::into_inner).1 % 180 == 90 { (f.height, f.width) } else { (f.width, f.height) };
        h.max(1) as f32 / w.max(1) as f32
    });
    let builder = egui::ViewportBuilder::default()
        .with_title(pop.title.clone())
        .with_inner_size([POPOUT_WIDTH, (POPOUT_WIDTH * aspect).round().clamp(90.0, 600.0)])
        .with_min_inner_size([120.0, 68.0])
        .with_always_on_top()
        .with_resizable(true)
        .with_active(false);
    let p = pop.clone();
    ctx.show_viewport_deferred(viewport_id(&pop.tap.device), builder, move |ui, class| {
        if ui.input(|i| i.viewport().close_requested()) {
            p.closed.store(true, Ordering::Release);
        }
        let orient = *p.orient.lock().unwrap_or_else(PoisonError::into_inner);
        let rect = if class == egui::ViewportClass::EmbeddedWindow {
            ui.allocate_exact_size(vec2(POPOUT_WIDTH, POPOUT_WIDTH * 9.0 / 16.0), Sense::hover()).0
        } else {
            ui.max_rect()
        };
        let mut slot = p.tex.lock().unwrap_or_else(PoisonError::into_inner);
        let label = paint(ui, &mut slot, &format!("record-popout-{n}"), &p.tap, orient, rect);
        if class == egui::ViewportClass::EmbeddedWindow {
            *p.drawn.lock().unwrap_or_else(PoisonError::into_inner) = Some((rect, label));
        }
        let fps = p.tap.format().map_or(30, |f| f.fps).clamp(1, 60);
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(1.0 / f64::from(fps)));
    });
}

/// The thumbnail of camera row `n` (1-based); pushes its automation element.
pub fn thumbnail(
    ui: &mut egui::Ui,
    elems: &mut Vec<(String, Rect, String)>,
    cache: &PreviewCache,
    app_session: &filmcraft_engine::Session,
    n: usize,
    c: &mut CameraRow,
    recording: bool,
) {
    let id = format!("record.panel.camera.{n}");
    let (rect, _) = ui.allocate_exact_size(thumb_size(recording), Sense::hover());
    let tap = (!c.device.is_empty()).then(|| filmcraft_engine::record_preview::preview_of(app_session, &c.device)).flatten();
    let label = match &tap {
        Some(t) => {
            let mut st = cache.lock();
            let slot = st.textures.remove(&c.device);
            let mut slot = slot;
            let l = paint(ui, &mut slot, &format!("record-thumb-{}", c.device), t, orient_of(c, t), rect);
            if let Some(s) = slot {
                st.textures.insert(c.device.clone(), s);
            }
            l
        }
        None => {
            ui.painter().rect_filled(rect, 3.0, Color32::from_gray(24));
            let msg = if c.device.is_empty() { "Camera off" } else { "No picture yet" };
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(11.0), Color32::from_gray(140));
            msg.to_string()
        }
    };
    elems.push((format!("{id}.preview"), rect, label));
}

/// The Pop out / Pop in button of camera row `n`.
pub fn popout_button(ui: &mut egui::Ui, elems: &mut Vec<(String, Rect, String)>, app_session: &filmcraft_engine::Session, n: usize, c: &mut CameraRow) {
    let id = format!("record.panel.camera.{n}");
    let running = !c.device.is_empty() && filmcraft_engine::record_preview::preview_of(app_session, &c.device).is_some();
    let b = ui.add_enabled(running, egui::Button::new(if c.popout { "Pop in" } else { "Pop out" }).small());
    elems.push((format!("{id}.popout"), b.rect, format!("Pop out {}", c.popout)));
    if b.clicked() {
        c.popout = !c.popout;
    }
}
