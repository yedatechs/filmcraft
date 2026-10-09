//! Tracked redaction: draw a box, get a tracked mosaic in the Program monitor and the clip menus. See
//! `openspec/changes/clip-layouts/design.md` §5 and `docs/layouts.md`.
//!
//! `redact.start {style?}` (Clip ▸ Layout ▸ Redact Area…) enters a draw mode on the Program
//! monitor; the drag (automation id `program.redact.draw`) draws a rubber band, and the release
//! maps it into the top-most video clip under the box (clip pixels) and runs `redact.add` with
//! tracking. Esc cancels.

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind};
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::{ClipId, TrackItem};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;

/// Status hint while the draw mode is on.
pub const HINT: &str = "Drag a box over what to hide (Esc cancels)";

/// `redact.start`: enter the draw mode (`style`: mosaic | blur | fill).
pub fn start(app: &mut FilmcraftApp, params: &Value) -> Result<Value, String> {
    let style = params.get("style").and_then(Value::as_str).map(str::to_ascii_lowercase);
    if let Some(s) = &style
        && !matches!(s.as_str(), "mosaic" | "blur" | "fill")
    {
        return Err(format!("style `{s}`: mosaic, blur or fill"));
    }
    if app.session.active_sequence().is_none() {
        return Err("no sequence is open".into());
    }
    app.ui.mask_pen = None;
    app.ui.redact_draw = true;
    app.ui.redact_style = style;
    app.ui.status = HINT.into();
    Ok(json!({"drawing": true}))
}

fn stop(app: &mut FilmcraftApp) {
    app.ui.redact_draw = false;
    app.ui.redact_style = None;
    if app.ui.status == HINT {
        app.ui.status.clear();
    }
}

/// Clip pixels → screen points for the clip's current Motion and the monitor picture rect.
fn clip_to_screen(app: &FilmcraftApp, it: &TrackItem, mt: Tick, pic: Rect, frame: (u32, u32)) -> Option<Affine> {
    let seq = app.session.active_sequence()?;
    let size = filmcraft_render::source_size(&app.session.project, it.item).unwrap_or(frame);
    let motion = filmcraft_render::motion_matrix(seq, it, size, mt);
    let view = Affine::translate(f64::from(pic.min.x), f64::from(pic.min.y))
        .then_apply(&Affine::scale(f64::from(pic.width()) / f64::from(frame.0.max(1)), f64::from(pic.height()) / f64::from(frame.1.max(1))));
    Some(view.then_apply(&motion))
}

/// The top-most enabled video clip at the playhead whose picture contains `at` (screen), else the
/// top-most one; with the screen → clip-pixel mapping.
fn clip_under(app: &FilmcraftApp, at: Pos2, pic: Rect, frame: (u32, u32)) -> Option<(ClipId, Affine)> {
    let seq = app.session.active_sequence()?;
    let ph = app.session.playhead();
    let mut first = None;
    for tr in seq.video_tracks.iter().rev() {
        for it in tr.items.iter().filter(|it| it.enabled && it.start <= ph && ph < it.end()) {
            let mt = it.source_time_at(ph);
            let Some(to_clip) = clip_to_screen(app, it, mt, pic, frame).and_then(|m| m.inverse()) else { continue };
            let (w, h) = filmcraft_render::source_size(&app.session.project, it.item).unwrap_or(frame);
            let c = to_clip.apply(Vec2::new(f64::from(at.x), f64::from(at.y)));
            if (0.0..=f64::from(w)).contains(&c.x) && (0.0..=f64::from(h)).contains(&c.y) {
                return Some((it.id, to_clip));
            }
            first.get_or_insert((it.id, to_clip));
        }
    }
    first
}

/// Drawn over the Program picture after the graphics and mask overlays.
pub fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) {
    if !app.ui.redact_draw {
        return;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        stop(app);
        return;
    }
    let resp = ui.interact(pic, egui::Id::new("redact-draw"), Sense::drag());
    app.auto.add("program.redact.draw", pic, "Redact Area");
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let anchor_id = egui::Id::new("redact-draw-anchor");
    if resp.drag_started()
        && let Some(p) = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos())
    {
        ui.data_mut(|d| d.insert_temp(anchor_id, p));
    }
    let anchor: Option<Pos2> = ui.data(|d| d.get_temp(anchor_id));
    let (Some(a), Some(b)) = (anchor, resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.latest_pos()))) else { return };
    let band = Rect::from_two_pos(a, b).intersect(pic);
    if resp.dragged() || resp.drag_stopped() {
        let col = Color32::from_rgb(0xff, 0x6a, 0x5a);
        let painter = ui.painter().with_clip_rect(pic);
        painter.rect_filled(band, 0.0, col.gamma_multiply(0.2));
        painter.rect_stroke(band, 0.0, Stroke::new(1.5, col), StrokeKind::Inside);
    }
    if !resp.drag_stopped() {
        return;
    }
    ui.data_mut(|d| d.remove::<Pos2>(anchor_id));
    if band.width() < 3.0 || band.height() < 3.0 {
        // a click, not a box: stay in the mode
        return;
    }
    let Some((clip, to_clip)) = clip_under(app, band.center(), pic, frame) else {
        app.ui.status = "no video clip under the playhead".into();
        stop(app);
        return;
    };
    let pts: Vec<Vec2> = [band.left_top(), band.right_top(), band.right_bottom(), band.left_bottom()]
        .into_iter()
        .map(|p| to_clip.apply(Vec2::new(f64::from(p.x), f64::from(p.y))))
        .collect();
    let (x0, y0) = pts.iter().fold((f64::INFINITY, f64::INFINITY), |(x, y), p| (x.min(p.x), y.min(p.y)));
    let (x1, y1) = pts.iter().fold((f64::NEG_INFINITY, f64::NEG_INFINITY), |(x, y), p| (x.max(p.x), y.max(p.y)));
    let style = app.ui.redact_style.clone().unwrap_or_else(|| "mosaic".into());
    stop(app);
    let params = json!({"clip": clip.0, "rect": [x0, y0, x1 - x0, y1 - y0], "style": style, "track": true});
    let ctx = ui.ctx().clone();
    match crate::menus::invoke(app, &ctx, "redact.add", params) {
        Ok(v) => {
            let name = v["name"].as_str().unwrap_or("Redaction");
            app.ui.status = match v["trackError"].as_str() {
                Some(e) => format!("{name} added (tracking failed: {e})"),
                None if v["jobs"].as_array().is_some_and(|j| !j.is_empty()) => format!("{name} added; tracking…"),
                None => format!("{name} added"),
            };
        }
        Err(e) => app.ui.status = e,
    }
}

/// Entries for the timeline clip context menu (after the standard groups).
pub fn clip_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let r = ui.add_enabled(app.session.active_sequence().is_some(), egui::Button::new("Redact Area…"));
    app.auto.add("timeline.clipMenu.redact.start", r.rect, "Redact Area…");
    if r.clicked() {
        if let Err(e) = start(app, &json!({})) {
            app.ui.status = e;
        }
        ui.close();
    }
}
