//! Live camera previews of the Record panel: the thumbnail in each camera row and the pop-out
//! preview window (an always-on-top deferred viewport titled [`PREVIEW_TITLE`]…, so a screen
//! recording leaves it out). The engine runs the cameras (`record.preview`,
//! `filmcraft_engine::record_preview`); this module asks for the cameras the panel shows, uploads
//! their latest pictures into textures (only when a new frame arrived) and draws them letterboxed,
//! mirrored / rotated like the clip will be.
//!
//! The pop-out is the size of the camera picture: its long side is [`POPOUT_LONG_SIDE`] points,
//! the other side follows the picture as shown (after Mirror / Rotate), and a control bar
//! ([`POPOUT_BAR`] points) sits under it: the state and time, Record / Stop (Cancel while a
//! countdown or a start runs) and the microphone level. The window's callback cannot reach the
//! app, so [`sync`] writes what the bar shows into [`Popout::status`] every frame of the main
//! window and runs the clicks the bar queued in [`Popout::actions`] through the panel's own
//! functions.
//!
//! Automation ids: `record.panel.camera.<n>.preview` (the thumbnail; its label is
//! `<w>×<h> @ <fps> fps · frame <k>`, or `No picture yet`), `record.panel.camera.<n>.popout`
//! (Pop out / Pop in) and, in headless sessions (the pop-out is drawn inside the main window),
//! `record.preview.<n>` (the pop-out's picture) with `.time`, `.record`, `.pause` and `.level`
//! (its bar). `ui.inspect` lists the windows in `ui.record.popouts` ([`PopoutInfo`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use egui::{Color32, Rect, RichText, Sense, TextureHandle, TextureOptions, pos2, vec2};
use serde::{Deserialize, Serialize};
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
/// The long side of the pop-out's picture when the window opens (points).
pub const POPOUT_LONG_SIDE: f32 = 320.0;
/// Height of the control bar under the pop-out's picture (points).
pub const POPOUT_BAR: f32 = 28.0;
/// Smallest pop-out window (inner size, points).
pub const POPOUT_MIN: egui::Vec2 = vec2(160.0, 118.0);

const RED: Color32 = Color32::from_rgb(0xd8, 0x50, 0x3f);
const AMBER: Color32 = Color32::from_rgb(0xe0, 0xa0, 0x30);

/// A picture `shown` (pixels, after Rotate) with its long side `long` points: 320 × 180 for 16:9,
/// 180 × 320 for 9:16.
pub fn popout_picture(shown: (u32, u32), long: f32) -> egui::Vec2 {
    let (w, h) = (shown.0.max(1) as f32, shown.1.max(1) as f32);
    let long = if long.is_finite() { long.max(1.0) } else { POPOUT_LONG_SIDE };
    if w >= h { vec2(long, (long * h / w).round().max(1.0)) } else { vec2((long * w / h).round().max(1.0), long) }
}

/// A texture of a camera's latest preview picture, with what it was made from.
struct Tex {
    index: u64,
    orient: (bool, u32),
    size: (u32, u32),
    handle: TextureHandle,
}

/// What a click on the pop-out's bar asks for. [`sync`] runs it through the panel's functions
/// (`record::run_popout_action`), which check the state again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopoutAction {
    /// Start, as the panel's Record button does (countdown included).
    Record,
    /// Stop and build the sequence.
    Stop,
    /// Cancel a countdown or a start.
    Cancel,
    /// Pause the recording.
    Pause,
    /// Resume a paused recording.
    Resume,
}

/// What the pop-out's control bar shows. [`sync`] writes it every frame of the main window.
#[derive(Clone, Debug, PartialEq)]
pub struct PopoutStatus {
    pub recording: bool,
    pub paused: bool,
    /// Recorded time, seconds.
    pub elapsed: f64,
    /// The state and time at the left of the bar: `● 00:12`, the countdown, `Starting…`; empty
    /// when idle.
    pub label: String,
    /// The loudest microphone's peak, 0 to 1 (None: no microphone, the meter stays empty).
    pub level: Option<f32>,
    /// The Record / Stop / Cancel button: its text, what a click asks for, whether it is enabled.
    pub record: (&'static str, PopoutAction, bool),
    /// The Pause / Resume button while recording (None: not shown).
    pub pause: Option<(&'static str, PopoutAction)>,
    /// The meter's colour (the theme's accent).
    pub accent: Color32,
}

impl Default for PopoutStatus {
    fn default() -> Self {
        Self {
            recording: false,
            paused: false,
            elapsed: 0.0,
            label: String::new(),
            level: None,
            record: ("● Record", PopoutAction::Record, false),
            pause: None,
            accent: Color32::from_rgb(0x2f, 0x6b, 0xdf),
        }
    }
}

/// A pop-out window as shown, for `ui.inspect` (`ui.record.popouts`): sizes in points, what the
/// window reports natively and what was drawn in headless sessions (`null` until its first frame).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PopoutInfo {
    /// The camera id.
    pub device: String,
    /// The window's inner size `[w, h]`: the picture and the control bar.
    pub inner: Option<[f32; 2]>,
    /// The picture as drawn `[w, h]` (letterboxed in the window when the person resized it).
    pub picture: Option<[f32; 2]>,
}

/// What a pop-out drew last.
#[derive(Default)]
struct Drawn {
    inner: Option<egui::Vec2>,
    picture: Option<egui::Vec2>,
    /// Embedded (headless) only: the automation elements (id suffix, rect, label).
    elems: Vec<(&'static str, Rect, String)>,
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
    /// The inner size the window is created with. The builder keeps this size for the window's
    /// whole life: a later change goes through `ViewportCommand::InnerSize`, so a size the
    /// person dragged is never undone by the builder.
    created: egui::Vec2,
    /// The picture (at [`POPOUT_LONG_SIDE`]) the window was last sized for: when the shown
    /// aspect changes, the window is resized around the picture's current long side.
    sized_for: Mutex<egui::Vec2>,
    /// What the bar shows.
    pub status: Mutex<PopoutStatus>,
    /// Clicks on the bar's buttons, waiting for [`sync`].
    pub actions: Mutex<Vec<PopoutAction>>,
    drawn: Mutex<Drawn>,
}

impl Popout {
    /// The pop-out of `tap`, sized for a picture `shown` (pixels, after Rotate).
    fn new(tap: Arc<CameraTap>, title: String, orient: (bool, u32), shown: (u32, u32)) -> Self {
        let picture = popout_picture(shown, POPOUT_LONG_SIDE);
        Self {
            tap,
            title,
            orient: Mutex::new(orient),
            closed: AtomicBool::new(false),
            tex: Mutex::new(None),
            created: picture + vec2(0.0, POPOUT_BAR),
            sized_for: Mutex::new(picture),
            status: Mutex::new(PopoutStatus::default()),
            actions: Mutex::new(Vec::new()),
            drawn: Mutex::new(Drawn::default()),
        }
    }
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

/// Paint camera `tap`'s picture letterboxed into `rect` (black bars, the corners rounded by
/// `radius`); returns its label and where the picture is (`rect` before the first frame).
fn paint(ui: &egui::Ui, slot: &mut Option<Tex>, name: &str, tap: &CameraTap, orient: (bool, u32), rect: Rect, radius: f32) -> (String, Rect) {
    ui.painter().rect_filled(rect, radius, Color32::BLACK);
    let f = tap.latest();
    let mut at = rect;
    if let Some(f) = f.as_deref() {
        let (id, size) = upload(ui.ctx(), slot, name, f, orient);
        at = letterbox(rect, size);
        ui.painter().image(id, at, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    } else {
        let msg = match tap.error() {
            Some(e) => e,
            None => "Starting the camera…".into(),
        };
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(11.0), Color32::from_gray(160));
    }
    (label(tap, f.as_deref(), orient.1), at)
}

/// How a row's picture is shown: (Mirror, Rotate degrees), like its file will be: with Auto the
/// orientation the camera reports now (it follows a turn made in the camera's own software).
fn orient_of(c: &CameraRow, tap: &CameraTap) -> (bool, u32) {
    (c.mirror, c.rotate.effective(tap.rotation()))
}

/// The size of `tap`'s picture as shown (pixels, turned by `rotate`): the latest picture's (what
/// is drawn), else the format the camera said it delivers. None before either is known.
fn shown_size(tap: &CameraTap, rotate: u32) -> Option<(u32, u32)> {
    let (w, h) = tap.latest().map(|f| (f.width, f.height)).or_else(|| tap.format().map(|f| (f.width, f.height)))?;
    Some(if rotate % 180 == 90 { (h, w) } else { (w, h) })
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

/// Every frame: run the clicks made on the pop-outs' bars, start / stop the previews the panel
/// wants, keep the pop-out windows and what their bars show, repaint at the cameras' rate while
/// the panel shows them (and about ten times a second while a pop-out exists, for its time and
/// meter).
pub fn sync(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let cache = app.ui.record.preview.clone();
    // clicks on the pop-outs' bars since the last frame
    let clicked: Vec<PopoutAction> = {
        let st = cache.lock();
        st.popouts.values().flat_map(|p| std::mem::take(&mut *p.actions.lock().unwrap_or_else(PoisonError::into_inner))).collect()
    };
    for a in clicked {
        super::record::run_popout_action(app, a);
    }
    let want = wanted(app);
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
    let status = if rows_out.is_empty() { PopoutStatus::default() } else { super::record::popout_status(app) };
    let mut shown: Vec<PopoutInfo> = Vec::new();
    for (i, c) in &rows_out {
        let Some(tap) = filmcraft_engine::record_preview::preview_of(&app.session, &c.device) else { continue };
        let orient = orient_of(c, &tap);
        let existing = cache.lock().popouts.get(&c.device).cloned();
        let pop = match existing {
            Some(p) => p,
            None => {
                // the window gets its size when it is created: not before the camera says what
                // it delivers
                let Some(size) = shown_size(&tap, orient.1) else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                    continue;
                };
                let name = names.get(&c.device).cloned().unwrap_or_else(|| c.device.clone());
                let p = Arc::new(Popout::new(tap.clone(), format!("{PREVIEW_TITLE}: {name}"), orient, size));
                cache.lock().popouts.insert(c.device.clone(), p.clone());
                // a preview window opened during a display recording: leave it out of the file
                super::record_overlay::schedule_refresh(app);
                p
            }
        };
        *pop.orient.lock().unwrap_or_else(PoisonError::into_inner) = orient;
        *pop.status.lock().unwrap_or_else(PoisonError::into_inner) = status.clone();
        show_popout(ctx, &pop, i + 1);
        let mut drawn = pop.drawn.lock().unwrap_or_else(PoisonError::into_inner);
        for (suffix, rect, label) in drawn.elems.drain(..) {
            app.auto.add(&format!("record.preview.{}{suffix}", i + 1), rect, &label);
        }
        shown.push(PopoutInfo { device: c.device.clone(), inner: drawn.inner.map(|s| [s.x, s.y]), picture: drawn.picture.map(|s| [s.x, s.y]) });
    }
    let any_popout = !shown.is_empty();
    app.ui.record.popouts = shown;
    if app.ui.record.open && !want.is_empty() {
        let fps = want.iter().map(|w| w.2).max().unwrap_or(30).clamp(1, 60);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(1.0 / f64::from(fps)));
    }
    if any_popout {
        // the bars read what this pass wrote: keep it fresh
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
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

/// The control bar under the pop-out's picture, left to right: the state and time, Record / Stop
/// / Cancel, Pause / Resume, and the microphone level in what is left. A click is queued for
/// [`sync`]; returns the automation elements (id suffix, rect, label).
fn bar(ui: &mut egui::Ui, rect: Rect, st: &PopoutStatus, actions: &Mutex<Vec<PopoutAction>>) -> Vec<(&'static str, Rect, String)> {
    ui.painter().rect_filled(rect, 0.0, Color32::from_gray(28));
    let mut elems: Vec<(&'static str, Rect, String)> = Vec::new();
    let mut clicked: Option<PopoutAction> = None;
    let inner = rect.shrink2(vec2(6.0, 2.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if !st.label.is_empty() {
            let color = if st.paused {
                AMBER
            } else if st.recording {
                RED
            } else {
                Color32::from_gray(190)
            };
            let l = ui.add(egui::Label::new(RichText::new(&st.label).small().color(color)).selectable(false));
            elems.push((".time", l.rect, st.label.clone()));
        }
        let (text, action, enabled) = st.record;
        let b = ui.add_enabled(enabled, egui::Button::new(RichText::new(text).small()).small());
        elems.push((".record", b.rect, text.to_string()));
        if b.clicked() {
            clicked = Some(action);
        }
        if let Some((text, action)) = st.pause {
            let b = ui.add(egui::Button::new(RichText::new(text).small()).small());
            elems.push((".pause", b.rect, text.to_string()));
            if b.clicked() {
                clicked = Some(action);
            }
        }
        // the meter takes what is left of the bar
        let w = ui.available_width().max(10.0);
        let (r, _) = ui.allocate_exact_size(vec2(w, 6.0), Sense::hover());
        let level = st.level.unwrap_or(0.0);
        super::record::level_meter(ui.painter(), r, level, st.accent);
        elems.push((".level", r, format!("{level:.2}")));
    });
    if let Some(a) = clicked {
        let mut q = actions.lock().unwrap_or_else(PoisonError::into_inner);
        // the main window runs them on its next frame; never more than a few wait
        if q.len() < 8 {
            q.push(a);
        }
        drop(q);
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
    elems
}

/// The pop-out preview window of camera row `n` (a deferred viewport: it repaints at the camera's
/// rate on its own). Its inner size is the picture plus the control bar; the builder's size is
/// the one it was created with, and when the shown aspect changes later (Rotate, a camera that
/// turned, another format) the window is resized around the picture's current long side.
fn show_popout(ctx: &egui::Context, pop: &Arc<Popout>, n: usize) {
    let builder = egui::ViewportBuilder::default()
        .with_title(pop.title.clone())
        .with_inner_size(pop.created)
        .with_min_inner_size(POPOUT_MIN)
        .with_always_on_top()
        .with_resizable(true)
        .with_active(false);
    let p = pop.clone();
    ctx.show_viewport_deferred(viewport_id(&pop.tap.device), builder, move |ui, class| {
        if ui.input(|i| i.viewport().close_requested()) {
            p.closed.store(true, Ordering::Release);
        }
        let embedded = class == egui::ViewportClass::EmbeddedWindow;
        let orient = *p.orient.lock().unwrap_or_else(PoisonError::into_inner);
        // the picture at the default long side, for the aspect shown now
        let want = shown_size(&p.tap, orient.1).map(|s| (s, popout_picture(s, POPOUT_LONG_SIDE)));
        let mut sized_for = p.sized_for.lock().unwrap_or_else(PoisonError::into_inner);
        let full = if embedded {
            // drawn inside the main window: always the picture's own size
            let picture = want.map_or(*sized_for, |w| w.1);
            *sized_for = picture;
            ui.allocate_exact_size(picture + vec2(0.0, POPOUT_BAR), Sense::hover()).0
        } else {
            ui.max_rect()
        };
        let bar_h = POPOUT_BAR.min(full.height());
        let area = Rect::from_min_max(full.min, pos2(full.max.x, full.max.y - bar_h));
        let bar_rect = Rect::from_min_max(pos2(full.min.x, area.max.y), full.max);
        if !embedded
            && let Some((shown, picture)) = want
            && (picture - *sized_for).abs().max_elem() >= 1.0
        {
            // another aspect: the same long side as the picture has in this window now
            let now = letterbox(area, *sized_for).size();
            let long = now.x.max(now.y).clamp(POPOUT_MIN.x.min(POPOUT_MIN.y - POPOUT_BAR), 4096.0);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(popout_picture(shown, long) + vec2(0.0, POPOUT_BAR)));
            *sized_for = picture;
        }
        drop(sized_for);
        ui.painter().rect_filled(full, 0.0, Color32::BLACK);
        let mut slot = p.tex.lock().unwrap_or_else(PoisonError::into_inner);
        let (label, picture) = paint(ui, &mut slot, &format!("record-popout-{n}"), &p.tap, orient, area, 0.0);
        drop(slot);
        let status = p.status.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let mut elems = bar(ui, bar_rect, &status, &p.actions);
        let inner = ui.input(|i| i.viewport().inner_rect).filter(|_| !embedded).map_or(full.size(), |r| r.size());
        let mut drawn = p.drawn.lock().unwrap_or_else(PoisonError::into_inner);
        drawn.inner = Some(inner);
        drawn.picture = Some(picture.size());
        drawn.elems.clear();
        if embedded {
            elems.insert(0, ("", picture, label));
            drawn.elems = elems;
        }
        drop(drawn);
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
            let (l, _) = paint(ui, &mut slot, &format!("record-thumb-{}", c.device), t, orient_of(c, t), rect, 3.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_popout_picture_keeps_the_aspect_with_the_long_side_given() {
        assert_eq!(popout_picture((320, 180), POPOUT_LONG_SIDE), vec2(320.0, 180.0));
        assert_eq!(popout_picture((1920, 1080), POPOUT_LONG_SIDE), vec2(320.0, 180.0));
        // a portrait picture (a camera on its side, Rotate 90°): the long side is its height
        assert_eq!(popout_picture((1080, 1920), POPOUT_LONG_SIDE), vec2(180.0, 320.0));
        // the preview picture of that camera (640 px wide, even rows) is the same window
        assert_eq!(popout_picture((640, 1136), POPOUT_LONG_SIDE), vec2(180.0, 320.0));
        assert_eq!(popout_picture((640, 480), POPOUT_LONG_SIDE), vec2(320.0, 240.0));
        assert_eq!(popout_picture((500, 500), 200.0), vec2(200.0, 200.0));
        // hostile sizes never divide by zero or give an empty window
        assert_eq!(popout_picture((0, 0), POPOUT_LONG_SIDE), vec2(320.0, 320.0));
        assert_eq!(popout_picture((1, 100_000), POPOUT_LONG_SIDE), vec2(1.0, 320.0));
        assert_eq!(popout_picture((16, 9), f32::NAN), vec2(320.0, 180.0));
    }
}
