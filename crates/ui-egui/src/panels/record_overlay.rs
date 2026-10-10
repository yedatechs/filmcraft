//! The recording border: a borderless, transparent, always-on-top, mouse-pass-through window
//! (an egui immediate viewport titled [`OVERLAY_TITLE`]) over the chosen display or window that
//! draws a 3 pt frame just inside its edges: grey while the Record panel is open with a screen
//! chosen ("this is what will be recorded"), red with a `● REC 00:12` pill while recording. On a
//! window source it follows the window (its frame is read every 0.5 s on a background thread);
//! on an area it frames the area. Choosing "Area of <display>…" turns it into a drawing surface
//! (not pass-through, crosshair): drag the rectangle, Esc cancels.
//!
//! A display recording leaves this window (and the camera preview windows) out of the file:
//! ScreenCaptureKit's filter excludes this process's windows with those titles
//! (`filmcraft_engine::record_preview::excluded_windows`), built when the stream starts, so the
//! panel shows the border a few frames before it starts a recording; a border or preview window
//! that appears later is added to the running stream's filter (`refresh_exclusions`), never by
//! restarting it.
//!
//! In embedded (headless) sessions the border is drawn inside the main window, the display
//! scaled to fit, so tests can drag on it. Automation: `record.overlay` (label
//! `<state> <target> <x>,<y> <w>×<h>`, the framed rectangle in pixels); `ui.inspect` shows
//! `ui.record.overlay` ([`OverlayInfo`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};

use filmcraft_engine::record::{ScreenFrame, ScreenTarget, VideoInputFactory};
use filmcraft_engine::record_preview::{MIN_AREA, OVERLAY_TITLE};

use crate::FilmcraftApp;

const RED: Color32 = Color32::from_rgb(0xe0, 0x3a, 0x2f);
const IDLE: Color32 = Color32::from_rgba_premultiplied(150, 150, 150, 210);
/// Border width (points).
const BORDER: f32 = 3.0;
/// Frames the border must have been shown before a recording of it starts (the window exists,
/// so the stream's filter leaves it out).
const READY_FRAMES: u32 = 6;

/// What `ui.inspect` shows of the border (`ui.record.overlay`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OverlayInfo {
    /// `idle` (grey), `recording` (red) or `drawing` (choosing an area).
    pub state: String,
    /// `display:<id>` or `window:<id>`.
    pub target: String,
    /// The framed rectangle in the target's pixels `[x, y, w, h]` (the area, or all of it).
    pub rect: [u32; 4],
    /// Where the border window is: points, desktop coordinates `[x, y, w, h]`.
    pub frame: [f64; 4],
}

/// Reads a target's frame every 0.5 s on its own thread (ScreenCaptureKit can take a while).
struct Poller {
    target: String,
    latest: Arc<Mutex<Option<ScreenFrame>>>,
    alive: Arc<AtomicBool>,
}

impl Poller {
    fn start(f: Arc<dyn VideoInputFactory>, key: &str, target: ScreenTarget) -> Self {
        let latest: Arc<Mutex<Option<ScreenFrame>>> = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (l, a) = (latest.clone(), alive.clone());
        let spawned = std::thread::Builder::new().name("filmcraft-record-overlay".into()).spawn(move || {
            while a.load(Ordering::Acquire) {
                let fr = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f.screen_frame(&target))).ok().flatten();
                *l.lock().unwrap_or_else(PoisonError::into_inner) = fr;
                for _ in 0..10 {
                    if !a.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        });
        if let Err(e) = spawned {
            log::warn!("recording border: cannot follow the screen: {e}");
        }
        Self { target: key.to_string(), latest, alive }
    }
    fn latest(&self) -> Option<ScreenFrame> {
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}

#[derive(Default)]
pub struct OverlayState {
    poller: Option<Poller>,
    /// Consecutive frames the border was shown.
    shown_frames: u32,
    /// A recording with a screen ran last frame.
    was_recording: bool,
    /// Where a drag on the drawing surface began (in the surface's points).
    drag_from: Option<Pos2>,
    /// The border was asked to take the keyboard (Esc) for drawing.
    focused: bool,
    /// The last frame drew embedded in the main window (headless sessions).
    embedded: bool,
    /// Record was pressed before the border existed: start once it does.
    pub pending_start: bool,
    /// Frames to wait before telling a running display recording to leave out new windows.
    refresh_in: Option<u32>,
}

/// UI-only border state kept in [`super::record::RecordUi`] (not saved; shared by clones).
#[derive(Clone, Default)]
pub struct OverlayCache(pub Arc<Mutex<OverlayState>>);

impl std::fmt::Debug for OverlayCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OverlayCache")
    }
}

impl PartialEq for OverlayCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl OverlayCache {
    pub fn lock(&self) -> std::sync::MutexGuard<'_, OverlayState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The screen target of the panel's `screen` value.
pub fn target_of(screen: &str) -> Option<ScreenTarget> {
    if let Some(id) = screen.strip_prefix("display:") {
        Some(ScreenTarget::Display(id.to_string()))
    } else {
        screen.strip_prefix("window:").map(|id| ScreenTarget::Window(id.to_string()))
    }
}

/// Whether a recording of the panel's screen can start now: the border window exists (natively,
/// for a few frames), so the stream leaves it out from the first frame.
pub fn ready_for_start(app: &FilmcraftApp) -> bool {
    if target_of(&app.ui.record.screen).is_none() {
        return true;
    }
    let st = app.ui.record.overlay_cache.lock();
    st.embedded || st.shown_frames >= READY_FRAMES
}

/// A border or preview window appeared during a recording: have the display recording leave it
/// out (after it had a few frames to appear on screen).
pub fn schedule_refresh(app: &FilmcraftApp) {
    let mut st = app.ui.record.overlay_cache.lock();
    if !st.embedded && app.session.record.recording() {
        st.refresh_in = Some(10);
    }
}

/// The display pixels of a drag from `a` to `b` on a surface `rect` showing `px` pixels: snapped
/// to even pixels, at least [`MIN_AREA`] each side, inside the display.
pub fn area_from_drag(rect: Rect, px: (u32, u32), a: Pos2, b: Pos2) -> [u32; 4] {
    let k = if rect.width() > 0.0 { px.0 as f32 / rect.width() } else { 1.0 };
    let to_px = |p: Pos2, max: u32, along: fn(Pos2) -> f32, min: f32| (((along(p) - min) * k).round().max(0.0) as u32).min(max);
    let (x0, x1) = (to_px(a, px.0, |p| p.x, rect.min.x), to_px(b, px.0, |p| p.x, rect.min.x));
    let (y0, y1) = (to_px(a, px.1, |p| p.y, rect.min.y), to_px(b, px.1, |p| p.y, rect.min.y));
    let (x, w) = (x0.min(x1) & !1, (x0.max(x1) - x0.min(x1)).max(MIN_AREA) & !1);
    let (y, h) = (y0.min(y1) & !1, (y0.max(y1) - y0.min(y1)).max(MIN_AREA) & !1);
    let (w, h) = (w.min(px.0 & !1).max(2), h.min(px.1 & !1).max(2));
    [x.min(px.0.saturating_sub(w)) & !1, y.min(px.1.saturating_sub(h)) & !1, w, h]
}

/// What the drawing surface decided this frame.
enum Drawn {
    Area([u32; 4]),
    Cancel,
}

struct Look {
    state: &'static str,
    /// Display pixels of the surface (a display) or the window's pixels.
    px: (u32, u32),
    area: Option<[u32; 4]>,
    elapsed: f64,
    /// Leave room for the menu bar above the pill (displays).
    pill_inset: f32,
}

/// Paint the border (and the drawing surface) into `rect`.
fn paint(ui: &mut egui::Ui, rect: Rect, look: &Look, drag_from: &mut Option<Pos2>) -> Option<Drawn> {
    let painter = ui.painter_at(rect);
    let k = if look.px.0 > 0 { rect.width() / look.px.0 as f32 } else { 1.0 };
    let framed = match look.area {
        Some([x, y, w, h]) => {
            let r = Rect::from_min_size(rect.min + vec2(x as f32 * k, y as f32 * k), vec2(w as f32 * k, h as f32 * k));
            // around the area (the picture stays clear), within the display
            r.expand(BORDER).intersect(rect)
        }
        None => rect,
    };
    if look.state == "drawing" {
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(70));
        let id = ui.id().with("record-overlay-draw");
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        let pos = resp.interact_pointer_pos().or(ui.input(|i| i.pointer.hover_pos()));
        if resp.drag_started() {
            *drag_from = ui.input(|i| i.pointer.press_origin()).or(pos);
        }
        let mut out = None;
        if let (Some(a), Some(b)) = (*drag_from, pos) {
            let r = Rect::from_two_pos(a, b);
            painter.rect_filled(r, 0.0, Color32::from_white_alpha(18));
            painter.rect_stroke(r, 0.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Outside);
            let [_, _, w, h] = area_from_drag(rect, look.px, a, b);
            painter.text(r.right_bottom() + vec2(-4.0, -4.0), egui::Align2::RIGHT_BOTTOM, format!("{w}×{h}"), egui::FontId::proportional(13.0), Color32::WHITE);
            if resp.drag_stopped() {
                out = Some(Drawn::Area(area_from_drag(rect, look.px, a, b)));
                *drag_from = None;
            }
        }
        painter.text(
            rect.center_top() + vec2(0.0, 60.0),
            egui::Align2::CENTER_TOP,
            "Drag the area to record (Esc cancels)",
            egui::FontId::proportional(18.0),
            Color32::WHITE,
        );
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            *drag_from = None;
            return Some(Drawn::Cancel);
        }
        return out;
    }
    let color = if look.state == "recording" { RED } else { IDLE };
    painter.rect_stroke(framed.shrink(BORDER / 2.0), 0.0, Stroke::new(BORDER, color), StrokeKind::Middle);
    if look.state == "recording" {
        let s = if look.elapsed.is_finite() { look.elapsed.max(0.0) as u64 } else { 0 };
        let text = format!("● REC {:02}:{:02}", s / 60, s % 60);
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(13.0), Color32::WHITE);
        let size = galley.size() + vec2(18.0, 8.0);
        let top = framed.top() + BORDER + look.pill_inset + 6.0;
        let pill = Rect::from_min_size(pos2(framed.center().x - size.x / 2.0, top), size);
        painter.rect_filled(pill, size.y / 2.0, RED);
        painter.galley(pill.min + vec2(9.0, 4.0), galley, Color32::WHITE);
    }
    None
}

fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("record-overlay")
}

/// Every frame (before the panel): show, move or close the border; run a pending start and a
/// due exclusion refresh.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if cfg!(target_arch = "wasm32") {
        return;
    }
    let cache = app.ui.record.overlay_cache.clone();
    let st_json = app.session.record.recording().then(|| filmcraft_engine::record::status_of(&app.session));
    let rec_screen = st_json.as_ref().is_some_and(|s| s["sources"].as_array().is_some_and(|a| a.iter().any(|x| x["kind"] == "screen")));
    let elapsed = st_json.as_ref().and_then(|s| s["elapsed"].as_f64()).unwrap_or(0.0);
    let counting = app.session.record.countdown.is_some();
    let embedded = ctx.embed_viewports();
    {
        let mut st = cache.lock();
        st.embedded = embedded;
        if st.was_recording && !rec_screen {
            // the recording stopped: the border goes until the screen is chosen again or the
            // panel reopens
            app.ui.record.overlay_dismissed = true;
        }
        st.was_recording = rec_screen;
        if let Some(n) = st.refresh_in {
            if n == 0 {
                st.refresh_in = None;
                drop(st);
                if let Err(e) = filmcraft_engine::record_preview::refresh_exclusions(&mut app.session) {
                    app.ui.record.error = format!("the recording may show FilmCraft's border: {e}");
                }
            } else {
                st.refresh_in = Some(n - 1);
            }
        }
    }
    let r = &app.ui.record;
    let target = target_of(&r.screen);
    let visible = target.is_some() && (rec_screen || ((r.open || counting) && !r.overlay_dismissed));
    if !visible {
        app.ui.record.overlay = None;
        app.ui.record.drawing = false;
        let mut st = cache.lock();
        st.shown_frames = 0;
        st.drag_from = None;
        st.focused = false;
        st.pending_start = false;
        return;
    }
    let Some(target) = target else { return };
    // follow the target's frame
    let frame = {
        let mut st = cache.lock();
        if st.poller.as_ref().is_none_or(|p| p.target != r.screen) {
            st.poller = Some(Poller::start(filmcraft_engine::record_preview::capture_factory(&app.session), &r.screen, target.clone()));
        }
        st.poller.as_ref().and_then(Poller::latest)
    };
    let Some(f) = frame.filter(|f| f.w >= 1.0 && f.h >= 1.0 && f.pixels.0 > 0 && f.pixels.1 > 0) else {
        app.ui.record.overlay = None;
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        return;
    };
    let is_display = matches!(target, ScreenTarget::Display(_));
    let drawing = r.drawing && is_display && !rec_screen && !counting;
    let state = if drawing {
        "drawing"
    } else if rec_screen {
        "recording"
    } else {
        "idle"
    };
    let area = if is_display { r.screen_area } else { None };
    let rect_px = area.unwrap_or([0, 0, f.pixels.0, f.pixels.1]);
    let screen = r.screen.clone();
    app.ui.record.overlay = Some(OverlayInfo { state: state.into(), target: screen.clone(), rect: rect_px, frame: [f.x, f.y, f.w, f.h] });
    let look = Look { state, px: f.pixels, area, elapsed, pill_inset: if is_display && area.is_none() { 26.0 } else { 0.0 } };
    let label = format!("{state} {} {},{} {}×{}", screen, rect_px[0], rect_px[1], rect_px[2], rect_px[3]);
    let first = cache.lock().shown_frames == 0;
    let mut drag_from = cache.lock().drag_from;
    let (drawn, auto_rect) = if embedded {
        let avail = ctx.content_rect();
        let k = (avail.width() / f.w as f32).min(avail.height() / f.h as f32).min(1.0);
        let rect = Rect::from_min_size(avail.min, vec2(f.w as f32 * k, f.h as f32 * k));
        let out = egui::Area::new(egui::Id::new("record-overlay")).order(egui::Order::Foreground).fixed_pos(rect.min).interactable(drawing).show(ctx, |ui| {
            ui.set_min_size(rect.size());
            paint(ui, rect, &look, &mut drag_from)
        });
        (out.inner, rect)
    } else {
        if drawing && !cache.lock().focused {
            ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::Focus);
            cache.lock().focused = true;
        }
        if !drawing {
            cache.lock().focused = false;
        }
        let builder = egui::ViewportBuilder::default()
            .with_title(OVERLAY_TITLE)
            .with_position([f.x as f32, f.y as f32])
            .with_inner_size([f.w as f32, f.h as f32])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_mouse_passthrough(!drawing)
            .with_taskbar(false)
            .with_resizable(false)
            .with_has_shadow(false)
            .with_active(false);
        let out = ctx.show_viewport_immediate(viewport_id(), builder, |ui, _class| {
            // Esc or the system never closes it: only the panel does
            if ui.input(|i| i.viewport().close_requested()) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            let rect = ui.max_rect();
            paint(ui, rect, &look, &mut drag_from)
        });
        (out, Rect::from_min_size(pos2(f.x as f32, f.y as f32), vec2(f.w as f32, f.h as f32)))
    };
    {
        let mut st = cache.lock();
        st.drag_from = drag_from;
        st.shown_frames = st.shown_frames.saturating_add(1);
    }
    if first && rec_screen {
        // appeared during a recording (started without the panel): leave it out of the file
        schedule_refresh(app);
    }
    app.auto.add("record.overlay", auto_rect, &label);
    match drawn {
        Some(Drawn::Area(a)) => {
            app.ui.record.screen_area = Some(a);
            app.ui.record.drawing = false;
        }
        Some(Drawn::Cancel) => app.ui.record.drawing = false,
        None => {}
    }
    if drawing || rec_screen || cache.lock().pending_start {
        ctx.request_repaint_after(std::time::Duration::from_millis(if drawing { 16 } else { 250 }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drags_become_even_areas_inside_the_display() {
        let r = Rect::from_min_size(pos2(10.0, 20.0), vec2(640.0, 360.0));
        // a surface at half scale: 1 point = 2 pixels
        assert_eq!(area_from_drag(r, (1280, 720), pos2(60.0, 70.0), pos2(380.0, 250.0)), [100, 100, 640, 360]);
        // dragged up-left, the same rectangle
        assert_eq!(area_from_drag(r, (1280, 720), pos2(380.0, 250.0), pos2(60.0, 70.0)), [100, 100, 640, 360]);
        // a click is at least 64 × 64
        assert_eq!(area_from_drag(r, (1280, 720), pos2(60.0, 70.0), pos2(60.0, 70.0)), [100, 100, 64, 64]);
        // past the edge: clamped inside
        assert_eq!(area_from_drag(r, (1280, 720), pos2(640.0, 370.0), pos2(900.0, 900.0)), [1216, 656, 64, 64]);
        let a = area_from_drag(r, (1280, 720), pos2(-50.0, -50.0), pos2(900.0, 900.0));
        assert_eq!(a, [0, 0, 1280, 720]);
        assert_eq!(target_of("display:1"), Some(ScreenTarget::Display("1".into())));
        assert_eq!(target_of("window:7"), Some(ScreenTarget::Window("7".into())));
        assert_eq!(target_of(""), None);
    }
}
