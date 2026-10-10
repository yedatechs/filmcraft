//! Clip layouts (place, shape, swap) with on-monitor handles in the Program monitor and the clip menus. See
//! `openspec/changes/clip-layouts/design.md` §3 and `docs/layouts.md`.
//!
//! - **Select on click.** With the Selection tool, a click on the Program picture that hits no
//!   graphic layer runs `layout.pick` at the playhead; the top-most clip becomes the selection, a
//!   second click on the same spot (within 4 px) cycles to the next clip under the pointer.
//! - **Box and handles.** The selected video clip's visible box (`layout.inspect` → `box`) with 8
//!   handles: drag inside moves it (snapping to the frame edges and centre and the guides; ⌘/Ctrl
//!   moves freely), the handles scale it uniformly about the opposite corner or edge. A drag is one
//!   undo step.
//! - **Menus.** Right-click on the box, the timeline clip menu (Layout ▸) and Clip ▸ Layout share
//!   one table of entries: Place ▸, Size ▸, Shape ▸, Swap With Clip Below, Redact Area ▸ (Static
//!   / Tracked Mosaic, Blur, Fill…), with the current place, size and shape checked.
//! - **Effect Controls.** One row of nine place buttons and four shape buttons under Motion.
//!
//! Automation ids: `program.layout.box`, `program.layout.handle.{nw|n|ne|e|se|s|sw|w}`,
//! `layout.menu.{place|size|shape}` (the submenus), `layout.menu.place.{at}`,
//! `layout.menu.size.{20|25|33|50}`, `layout.menu.shape.{circle|rounded|square|free}`,
//! `layout.menu.swap`, `layout.menu.redact` (the submenu), `layout.menu.redact.{static|tracked}.{mosaic|blur|fill}`,
//! `effectControls.layout.place.{at}`,
//! `effectControls.layout.shape.{s}` (and `properties.layout.*` in the Properties panel).

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::{ClipId, ItemId};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::Tool;

/// The places of `layout.place`, in menu order (3 × 3 grid, then Full).
pub const PLACES: [(&str, &str); 10] = [
    ("topLeft", "Top Left"),
    ("top", "Top"),
    ("topRight", "Top Right"),
    ("left", "Left"),
    ("center", "Center"),
    ("right", "Right"),
    ("bottomLeft", "Bottom Left"),
    ("bottom", "Bottom"),
    ("bottomRight", "Bottom Right"),
    ("full", "Full"),
];
/// Sizes (% of the frame width) of the Size submenu.
pub const SIZES: [(u32, &str); 4] = [(20, "20%"), (25, "25%"), (33, "33%"), (50, "50%")];
/// Shapes of `layout.shape`.
pub const SHAPES: [(&str, &str); 4] = [("circle", "Circle"), ("rounded", "Rounded"), ("square", "Square"), ("free", "Free")];

const HINT: &str = "Drag to move, corners to scale, right-click for layouts";
/// Handles in automation-id order, with their position on the box (0, ½, 1 of width and height).
const HANDLES: [(&str, f32, f32); 8] =
    [("nw", 0.0, 0.0), ("n", 0.5, 0.0), ("ne", 1.0, 0.0), ("e", 1.0, 0.5), ("se", 1.0, 1.0), ("s", 0.5, 1.0), ("sw", 0.0, 1.0), ("w", 0.0, 0.5)];
/// A second click this close (screen points) to the previous one cycles through the stack.
const CYCLE_PX: f32 = 4.0;

/// Per-session UI state of the layout overlay (not saved).
#[derive(Clone, Debug, Default)]
pub struct LayoutUi {
    /// The first-selection hint was shown.
    pub hint_shown: bool,
    cache: Option<Cache>,
    /// The previous monitor click: where, the clips under it (top first), which one is selected.
    last_click: Option<(Pos2, Vec<u64>, usize)>,
}

type CacheKey = (u64, Vec<ClipId>, Tick, Option<ItemId>);

#[derive(Clone, Debug)]
struct Cache {
    key: CacheKey,
    info: Option<Info>,
}

/// `layout.inspect` of the selected video clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub clip: ClipId,
    pub at: String,
    pub size: f64,
    pub margin: Option<f64>,
    pub shape: String,
    /// The visible box in frame pixels `[x, y, w, h]`.
    pub bx: [f64; 4],
}

impl Info {
    fn parse(v: &Value) -> Option<Info> {
        let c = v.get("clips")?.as_array()?.first()?;
        let b = c.get("box")?.as_array()?;
        let n = |i: usize| b.get(i).and_then(Value::as_f64).filter(|v| v.is_finite());
        Some(Info {
            clip: ClipId(c.get("clip")?.as_u64()?),
            at: c.get("at")?.as_str()?.to_string(),
            size: c.get("size").and_then(Value::as_f64).unwrap_or(0.0),
            margin: c.get("margin").and_then(Value::as_f64),
            shape: c.get("shape").and_then(Value::as_str).unwrap_or("custom").to_string(),
            bx: [n(0)?, n(1)?, n(2)?, n(3)?],
        })
    }
}

/// Display name of a place id (`custom` for a box at no place).
pub fn place_label(at: &str) -> &'static str {
    PLACES.iter().find(|p| p.0 == at).map_or("Custom", |p| p.1)
}

// ------------------------------------------------------------------ selection and inspect

/// The selected clip layouts act on: the first selected clip on a video track that is not a graphic.
fn target(app: &FilmcraftApp) -> Option<ClipId> {
    let q = app.session.active_sequence()?;
    app.session
        .state
        .selection
        .iter()
        .copied()
        .find(|c| q.video_tracks.iter().any(|t| t.items.iter().any(|it| it.id == *c)) && !crate::panels::graphics::is_graphic_clip(app, *c))
}

/// Bring the cached `layout.inspect` of the selected video clip up to date (the project, the
/// selection, the playhead or the sequence changed). Kept as is while playing.
pub fn refresh(app: &mut FilmcraftApp) {
    if app.playback.playing && app.ui.layout.cache.is_some() {
        return;
    }
    let key: CacheKey = (app.session.revision, app.session.state.selection.clone(), app.session.playhead(), app.session.state.active_sequence);
    if app.ui.layout.cache.as_ref().is_some_and(|c| c.key == key) {
        return;
    }
    let info = match target(app) {
        Some(c) if app.session.is_enabled("layout.inspect") => {
            app.session.execute("layout.inspect", json!({"clips": [c.0]})).ok().and_then(|v| Info::parse(&v))
        }
        _ => None,
    };
    app.ui.layout.cache = Some(Cache { key, info });
}

/// The selected video clip's layout, as of the last [`refresh`].
pub fn info(app: &FilmcraftApp) -> Option<&Info> {
    app.ui.layout.cache.as_ref()?.info.as_ref()
}

/// Menu check mark of a `layout.menu.*` entry (None: not checkable).
pub fn checked(app: &FilmcraftApp, id: &str) -> Option<bool> {
    let rest = id.strip_prefix("layout.menu.")?;
    let i = info(app);
    if let Some(at) = rest.strip_prefix("place.") {
        return Some(i.is_some_and(|i| i.at == at));
    }
    if let Some(n) = rest.strip_prefix("size.") {
        let n: f64 = n.parse().ok()?;
        return Some(i.is_some_and(|i| i.at != "full" && (i.size - n).abs() < 0.75));
    }
    if let Some(s) = rest.strip_prefix("shape.") {
        return Some(i.is_some_and(|i| i.shape == s));
    }
    None
}

/// Whether a `layout.menu.*` entry can run now.
pub fn enabled(app: &FilmcraftApp, id: &str) -> bool {
    if id == "layout.menu.redact" || id.starts_with("layout.menu.redact.") {
        return true;
    }
    match id {
        "layout.menu.swap" => app.session.is_enabled("layout.swap"),
        _ => app.session.is_enabled("layout.place"),
    }
}

// ------------------------------------------------------------------ commands

/// `layout.place` params for `at`: a clip already in a corner or an edge keeps its size and
/// margin; one that fills the frame (or is at no place) gets the defaults.
fn place_params(info: Option<&Info>, at: &str) -> Value {
    let mut p = json!({"at": at});
    if at != "full"
        && let Some(i) = info.filter(|i| i.at != "full" && i.at != "custom" && i.size > 0.5 && i.size < 95.0)
    {
        p["size"] = json!(i.size);
        if let Some(m) = i.margin {
            p["margin"] = json!(m);
        }
    }
    p
}

/// The clip under `clip` at the playhead: the first enabled, non-graphic video clip on a lower
/// enabled track whose span covers the playhead.
fn clip_below(app: &FilmcraftApp, clip: ClipId) -> Option<ClipId> {
    let q = app.session.active_sequence()?;
    let t = app.session.playhead();
    let k = q.video_tracks.iter().position(|tr| tr.items.iter().any(|it| it.id == clip))?;
    q.video_tracks.get(..k)?.iter().rev().filter(|tr| tr.enabled).find_map(|tr| {
        tr.items.iter().find(|it| it.enabled && it.start <= t && t < it.end() && !crate::panels::graphics::is_graphic_clip(app, it.id)).map(|it| it.id)
    })
}

/// Route a `layout.menu.*` UI command (menus, the control channel). `params.clips` overrides the
/// selection.
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let rest = id.strip_prefix("layout.menu.")?;
    let r = run(app, rest, params);
    if let Err(e) = &r {
        app.ui.status = e.clone();
    }
    Some(r)
}

fn run(app: &mut FilmcraftApp, rest: &str, params: &Value) -> Result<Value, String> {
    refresh(app);
    let current = info(app).cloned();
    let with_clips = |mut p: Value| {
        if let Some(c) = params.get("clips") {
            p["clips"] = c.clone();
        }
        p
    };
    let exec = |app: &mut FilmcraftApp, cmd: &str, p: Value| app.session.execute(cmd, p).map_err(|e| e.to_string());
    if rest == "redact" {
        // the old single entry: an alias of Static Mosaic… (`style` / `track` params still apply)
        return crate::panels::redact::start(app, params);
    }
    if let Some(mode) = rest.strip_prefix("redact.") {
        // Redact Area ▸ …: the Program picture becomes a draw surface (`panels::redact`)
        let (_, _, track, style) = crate::panels::redact::MODES.iter().find(|m| m.0 == mode).ok_or_else(|| format!("unknown redaction `{mode}`"))?;
        return crate::panels::redact::start(app, &json!({"style": style, "track": track}));
    }
    if rest == "swap" {
        let p = match params.get("clips") {
            Some(c) => json!({"clips": c}),
            None => match target(app).and_then(|c| clip_below(app, c).map(|b| (c, b))) {
                Some((a, b)) => json!({"clips": [a.0, b.0]}),
                None => json!({}),
            },
        };
        return exec(app, "layout.swap", p);
    }
    if let Some(at) = rest.strip_prefix("place.") {
        if !PLACES.iter().any(|p| p.0 == at) {
            return Err(format!("unknown place `{at}`"));
        }
        return exec(app, "layout.place", with_clips(place_params(current.as_ref(), at)));
    }
    if let Some(n) = rest.strip_prefix("size.") {
        let n: u32 = n.parse().map_err(|_| format!("unknown size `{n}`"))?;
        if !SIZES.iter().any(|s| s.0 == n) {
            return Err(format!("unknown size `{n}`"));
        }
        // a box at no place grows or shrinks about its centre; otherwise it keeps its place
        if let Some(i) = current.as_ref().filter(|i| i.at == "custom" && params.get("clips").is_none()) {
            let frame_w = app.session.active_sequence().map_or(0, |q| q.settings.width);
            let mt = app.session.active_sequence().and_then(|q| q.find_item(i.clip)).map(|(_, it)| media_time(it, app.session.playhead()));
            let s0 = app
                .session
                .active_sequence()
                .and_then(|q| q.find_item(i.clip))
                .and_then(|(_, it)| it.effect("motion"))
                .zip(mt)
                .map(|(m, mt)| m.f64_at("scale", mt));
            if let Some(s0) = s0
                && i.bx[2] > 1e-6
                && frame_w > 0
            {
                let s = s0 * (f64::from(n) / 100.0 * f64::from(frame_w)) / i.bx[2];
                return exec(app, "effects.setParam", json!({"clip": i.clip.0, "effect": "motion", "param": "scale", "value": s}));
            }
        }
        let at = current.as_ref().map(|i| i.at.as_str()).filter(|a| *a != "full" && *a != "custom").unwrap_or("center").to_string();
        let mut p = json!({"at": at, "size": n});
        if let Some(m) = current.as_ref().and_then(|i| i.margin) {
            p["margin"] = json!(m);
        }
        return exec(app, "layout.place", with_clips(p));
    }
    if let Some(s) = rest.strip_prefix("shape.") {
        if !SHAPES.iter().any(|x| x.0 == s) {
            return Err(format!("unknown shape `{s}`"));
        }
        return exec(app, "layout.shape", with_clips(json!({"shape": s})));
    }
    Err(format!("unknown layout command `layout.menu.{rest}`"))
}

fn media_time(it: &filmcraft_project::TrackItem, t: Tick) -> Tick {
    it.source_time_at(t.clamp(it.start, (it.end() - Tick(1)).max(it.start)))
}

// ------------------------------------------------------------------ menus

fn menu_entry(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, label: &str, run: &mut Option<String>) {
    let text = match checked(app, id) {
        Some(true) => format!("✓ {label}"),
        Some(false) => format!("    {label}"),
        None => label.to_string(),
    };
    let r = ui.add_enabled(enabled(app, id), egui::Button::new(text));
    app.auto.add(id, r.rect, label);
    if r.clicked() {
        *run = Some(id.to_string());
        ui.close();
    }
}

/// The Layout entries (Place ▸, Size ▸, Shape ▸, Swap With Clip Below, Redact Area ▸), shared by
/// the monitor right-click menu and the timeline clip menu.
pub fn menu_body(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    refresh(app);
    let mut run: Option<String> = None;
    let r = ui.menu_button("Place", |ui| {
        for (at, label) in PLACES {
            if at == "full" {
                ui.separator();
            }
            menu_entry(app, ui, &format!("layout.menu.place.{at}"), label, &mut run);
        }
    });
    app.auto.add("layout.menu.place", r.response.rect, "Place");
    let r = ui.menu_button("Size", |ui| {
        for (n, label) in SIZES {
            menu_entry(app, ui, &format!("layout.menu.size.{n}"), label, &mut run);
        }
    });
    app.auto.add("layout.menu.size", r.response.rect, "Size");
    let r = ui.menu_button("Shape", |ui| {
        for (s, label) in SHAPES {
            menu_entry(app, ui, &format!("layout.menu.shape.{s}"), label, &mut run);
        }
    });
    app.auto.add("layout.menu.shape", r.response.rect, "Shape");
    ui.separator();
    menu_entry(app, ui, "layout.menu.swap", "Swap With Clip Below", &mut run);
    let r = ui.menu_button("Redact Area", |ui| {
        for (i, (mode, label, _, _)) in crate::panels::redact::MODES.iter().enumerate() {
            if i == 3 {
                ui.separator();
            }
            menu_entry(app, ui, &format!("layout.menu.redact.{mode}"), label, &mut run);
        }
    });
    app.auto.add("layout.menu.redact", r.response.rect, "Redact Area");
    r.response.on_hover_text(crate::panels::redact::TIP);
    if let Some(id) = run {
        let ctx = ui.ctx().clone();
        if let Err(e) = crate::menus::invoke(app, &ctx, &id, json!({})) {
            app.ui.status = e;
        }
    }
}

/// Entries for the timeline clip context menu (after the standard groups).
pub fn clip_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    ui.separator();
    let r = ui.menu_button("Layout", |ui| {
        ui.set_min_width(200.0);
        menu_body(app, ui);
    });
    app.auto.add("timeline.clipMenu.layout", r.response.rect, "Layout");
}

// ------------------------------------------------------------------ Effect Controls

/// One row of nine place buttons (a 3 × 3 dot grid) and four shape buttons, starting at `x0` in
/// `r`. Ids `{prefix}.layout.place.{at}`, `{prefix}.layout.shape.{s}`.
pub fn controls_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, x0: f32, clip: ClipId, prefix: &str, actions: &mut Vec<(String, Value)>) {
    refresh(app);
    let t = app.tokens;
    if let Some(name) = filmcraft_engine::scenes::scene_owning(&app.session, clip) {
        // arranged by a scene: the buttons would be overwritten on the next re-apply
        let text = format!("Set by scene \"{name}\" (Sequence ▸ Scenes…)");
        let tr = ui.painter().text(pos2(x0, r.center().y), egui::Align2::LEFT_CENTER, &text, crate::theme::Tokens::ui(11.0), t.text_dim);
        app.auto.add(&format!("{prefix}.layout.scene"), tr, &text);
        return;
    }
    let cur = info(app).filter(|i| i.clip == clip).cloned();
    let size = 16.0;
    let step = 18.0;
    let mut x = x0;
    for (k, (at, label)) in PLACES.iter().take(9).enumerate() {
        let br = Rect::from_min_size(pos2(x, r.center().y - size / 2.0), vec2(size, size));
        x += step;
        let tip = format!("Place {label} (layout.place)");
        let resp = ui.interact(br, egui::Id::new(("layout-place", prefix, clip.0, *at)), Sense::click()).on_hover_text(&tip);
        let on = cur.as_ref().is_some_and(|i| i.at == *at);
        if resp.hovered() {
            ui.painter().rect_filled(br, 3.0, t.hover);
        }
        if on {
            ui.painter().rect_stroke(br, 3.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
        }
        let (row, col) = (k / 3, k % 3);
        for j in 0..9 {
            let c = pos2(br.min.x + 4.0 + (j % 3) as f32 * 4.0, br.min.y + 4.0 + (j / 3) as f32 * 4.0);
            if j == row * 3 + col {
                ui.painter().circle_filled(c, 1.8, if on { t.accent } else { t.text });
            } else {
                ui.painter().circle_filled(c, 0.9, t.text_dim);
            }
        }
        app.auto.add(&format!("{prefix}.layout.place.{at}"), br, &tip);
        if resp.clicked() {
            let mut p = place_params(cur.as_ref(), at);
            p["clips"] = json!([clip.0]);
            actions.push(("layout.place".into(), p));
        }
    }
    x += 8.0;
    for (s, label) in SHAPES {
        let br = Rect::from_min_size(pos2(x, r.center().y - size / 2.0), vec2(size, size));
        x += step;
        let tip = format!("{label} shape (layout.shape)");
        let resp = ui.interact(br, egui::Id::new(("layout-shape", prefix, clip.0, s)), Sense::click()).on_hover_text(&tip);
        let on = cur.as_ref().is_some_and(|i| i.shape == s);
        if resp.hovered() {
            ui.painter().rect_filled(br, 3.0, t.hover);
        }
        let col = if on { t.accent } else { t.icon };
        let st = Stroke::new(1.2, col);
        let g = br.shrink(3.5);
        match s {
            "circle" => {
                ui.painter().circle_stroke(g.center(), g.width() / 2.0, st);
            }
            "rounded" => {
                ui.painter().rect_stroke(g, 3.0, st, StrokeKind::Middle);
            }
            "square" => {
                ui.painter().rect_stroke(g, 0.0, st, StrokeKind::Middle);
            }
            _ => {
                let pts = vec![g.left_top(), g.right_top(), g.right_bottom(), g.left_bottom(), g.left_top()];
                ui.painter().extend(egui::Shape::dashed_line(&pts, st, 2.0, 2.0));
            }
        }
        app.auto.add(&format!("{prefix}.layout.shape.{s}"), br, &tip);
        if resp.clicked() {
            actions.push(("layout.shape".into(), json!({"clips": [clip.0], "shape": s})));
        }
    }
}

// ------------------------------------------------------------------ Program monitor

/// A drag of the box or a handle, from press to release.
#[derive(Clone, Copy, Debug)]
struct Drag {
    clip: ClipId,
    /// None: move; else the index in [`HANDLES`].
    handle: Option<usize>,
    start: Pos2,
    /// The visible box at the press, in screen points and in frame pixels.
    screen: Rect,
    bx: [f64; 4],
    position: [f64; 2],
    scale: f64,
    scale_width: f64,
    uniform: bool,
    begun: bool,
}

fn in_quad(q: &[Pos2; 4], p: Pos2) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let c = (b - a).x * (p - a).y - (b - a).y * (p - a).x;
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    true
}

/// Motion position (frame pixels; NaN = frame centre), scale, scale width and uniform scale of a
/// clip at the playhead, as the picture shows them.
fn motion_of(app: &FilmcraftApp, clip: ClipId, frame: (u32, u32)) -> Option<([f64; 2], f64, f64, bool)> {
    let q = app.session.active_sequence()?;
    let (_, it) = q.find_item(clip)?;
    let mt = media_time(it, app.session.playhead());
    let centre = [f64::from(frame.0) / 2.0, f64::from(frame.1) / 2.0];
    let Some(m) = it.effect("motion").filter(|m| m.enabled) else { return Some((centre, 100.0, 100.0, true)) };
    let p = if m.param("position").is_some() { m.vec2_at("position", mt) } else { filmcraft_geom::Vec2::new(f64::NAN, f64::NAN) };
    let pos = [if p.x.is_finite() { p.x } else { centre[0] }, if p.y.is_finite() { p.y } else { centre[1] }];
    let uniform = m.param("uniform_scale").and_then(|p| p.value.as_bool()).unwrap_or(true);
    let sc = m.f64_at("scale", mt);
    let sw = if m.param("scale_width").is_some() { m.f64_at("scale_width", mt) } else { sc };
    Some((pos, sc, sw, uniform))
}

/// Where the drag puts the clip: (position, scale, scale width) for the pointer at `cur`.
fn drag_target(d: &Drag, cur: Pos2, k: (f32, f32), snapped: egui::Vec2) -> ([f64; 2], f64, f64) {
    let (kx, ky) = (f64::from(k.0.max(1e-6)), f64::from(k.1.max(1e-6)));
    let Some(&(_, hx, hy)) = d.handle.and_then(|h| HANDLES.get(h)) else {
        let p = [d.position[0] + f64::from(snapped.x) / kx, d.position[1] + f64::from(snapped.y) / ky];
        return (p, d.scale, d.scale_width);
    };
    let [bx, by, bw, bh] = d.bx;
    let (hx, hy) = (f64::from(hx), f64::from(hy));
    // the handle and the fixed point opposite it, in frame pixels
    let h = (bx + bw * hx, by + bh * hy);
    let fixed = (bx + bw * (1.0 - hx), by + bh * (1.0 - hy));
    let c = (d.bx[0] + (f64::from(cur.x) - f64::from(d.screen.min.x)) / kx, d.bx[1] + (f64::from(cur.y) - f64::from(d.screen.min.y)) / ky);
    let d0 = (h.0 - fixed.0, h.1 - fixed.1);
    let d1 = (c.0 - fixed.0, c.1 - fixed.1);
    let f = if hx == 0.5 {
        if d0.1.abs() > 1e-6 { d1.1 / d0.1 } else { 1.0 }
    } else if hy == 0.5 {
        if d0.0.abs() > 1e-6 { d1.0 / d0.0 } else { 1.0 }
    } else {
        let l2 = d0.0 * d0.0 + d0.1 * d0.1;
        if l2 > 1e-9 { (d1.0 * d0.0 + d1.1 * d0.1) / l2 } else { 1.0 }
    };
    let f = if f.is_finite() { f.clamp(0.01, 100.0) } else { 1.0 };
    // uniform scaling about the anchor (the position): the fixed point stays where it is
    let p = [fixed.0 + f * (d.position[0] - fixed.0), fixed.1 + f * (d.position[1] - fixed.1)];
    (p, (d.scale * f).max(0.1), (d.scale_width * f).max(0.1))
}

/// Drawn over the Program picture after the graphics and mask overlays.
pub fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) {
    if app.ui.tool != Tool::Selection || app.ui.mask_pen.is_some() || app.playback.playing || frame.0 == 0 || frame.1 == 0 {
        return;
    }
    // a mask being edited owns the picture
    if app.session.state.selected_mask.is_some() {
        return;
    }
    refresh(app);
    let t = app.tokens;
    let k = (pic.width() / frame.0 as f32, pic.height() / frame.1 as f32);
    let to_screen = |x: f64, y: f64| pos2(pic.min.x + x as f32 * k.0, pic.min.y + y as f32 * k.1);
    let quads: Vec<[Pos2; 4]> = crate::panels::graphics::visible_layers(app, pic, frame).iter().map(|v| v.quad()).collect();
    let over_graphic = |p: Pos2| quads.iter().any(|q| in_quad(q, p));
    let hover = ui.input(|i| i.pointer.hover_pos());
    let ph = app.session.playhead();
    let shown = info(app)
        .cloned()
        .filter(|i| app.session.active_sequence().and_then(|q| q.find_item(i.clip)).is_some_and(|(_, it)| it.enabled && it.start <= ph && ph < it.end()));
    let drag_id = egui::Id::new("layout-drag");
    let mut drag: Option<Drag> = ui.data(|d| d.get_temp(drag_id));
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut box_resp: Option<egui::Response> = None;
    let painter = ui.painter().with_clip_rect(pic.expand(8.0));
    if let Some(i) = &shown {
        let r = Rect::from_min_max(to_screen(i.bx[0], i.bx[1]), to_screen(i.bx[0] + i.bx[2], i.bx[1] + i.bx[3]));
        painter.rect_stroke(r, 0.0, Stroke::new(1.5, t.accent), StrokeKind::Middle);
        // a graphic under the pointer takes the click
        let sense = if drag.is_none() && hover.is_some_and(over_graphic) { Sense::hover() } else { Sense::click_and_drag() };
        let resp = ui.interact(r.intersect(pic), egui::Id::new("layout-box"), sense);
        app.auto.add("program.layout.box", r.intersect(pic), place_label(&i.at));
        if resp.hovered() && drag.is_none() && !hover.is_some_and(over_graphic) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
        }
        let mut starts: Vec<(Option<usize>, egui::Response)> = Vec::new();
        for (n, (name, fx, fy)) in HANDLES.iter().enumerate() {
            let c = pos2(r.min.x + r.width() * fx, r.min.y + r.height() * fy);
            let hr = Rect::from_center_size(c, vec2(8.0, 8.0));
            painter.rect_filled(hr, 0.0, Color32::WHITE);
            painter.rect_stroke(hr, 0.0, Stroke::new(1.0, t.accent), StrokeKind::Middle);
            let hresp = ui.interact(hr.expand(2.0), egui::Id::new(("layout-handle", *name)), Sense::drag());
            app.auto.add(&format!("program.layout.handle.{name}"), hr, "scale handle");
            if hresp.hovered() {
                let icon = match *name {
                    "n" | "s" => egui::CursorIcon::ResizeVertical,
                    "e" | "w" => egui::CursorIcon::ResizeHorizontal,
                    "nw" | "se" => egui::CursorIcon::ResizeNwSe,
                    _ => egui::CursorIcon::ResizeNeSw,
                };
                ui.ctx().set_cursor_icon(icon);
            }
            starts.push((Some(n), hresp));
        }
        starts.push((None, resp.clone()));
        if let Some((_, sr)) = starts.iter().find(|(_, sr)| sr.drag_started())
            && let Some(name) = filmcraft_engine::scenes::scene_owning(&app.session, i.clip)
        {
            // scene-owned clips are arranged by their scene; a hand edit would be overwritten
            let _ = sr;
            app.ui.status = format!("This clip's layout is set by the scene \"{name}\". Open Sequence ▸ Scenes… to change it.");
        } else if let Some((handle, sr)) = starts.iter().find(|(_, sr)| sr.drag_started())
            && let Some((position, scale, scale_width, uniform)) = motion_of(app, i.clip, frame)
        {
            let start = ui.input(|inp| inp.pointer.press_origin()).or(sr.interact_pointer_pos()).unwrap_or(r.center());
            drag = Some(Drag { clip: i.clip, handle: *handle, start, screen: r, bx: i.bx, position, scale, scale_width, uniform, begun: false });
        }
        let dragging = starts.iter().any(|(_, sr)| sr.dragged());
        let stopped = starts.iter().any(|(_, sr)| sr.drag_stopped());
        if let Some(d) = drag.as_mut()
            && (dragging || stopped)
            && let Some(cur) = ui.input(|inp| inp.pointer.interact_pos())
        {
            let free = ui.input(|inp| inp.modifiers.command);
            let (off, lines) = match d.handle {
                None if !free => crate::panels::monitor_view::snap_move(app, pic, frame, d.screen, cur - d.start),
                _ => (cur - d.start, Vec::new()),
            };
            crate::panels::monitor_view::draw_snap_lines(&painter, pic, &lines);
            if (cur - d.start).length() > 0.5 {
                let (p, s, sw) = drag_target(d, cur, k, off);
                let begin = !d.begun;
                d.begun = true;
                // one command, one merged undo step for the whole drag (`layout.set`)
                let mut params = json!({"clips": [d.clip.0], "position": p, "merge": true, "begin": begin});
                if d.handle.is_some() {
                    params["scale"] = json!(s);
                    if !d.uniform {
                        params["scaleWidth"] = json!(sw);
                    }
                }
                actions.push(("layout.set".into(), params));
            }
        }
        box_resp = Some(resp);
    }
    // ---- run the drag's edits (`layout.set` with `merge`: one undo step per drag)
    for (c, p) in actions {
        if let Err(e) = app.session.execute(&c, p) {
            app.ui.status = e.to_string();
        }
    }
    let released = ui.input(|i| !i.pointer.any_down());
    if drag.is_some() {
        refresh(app);
        if let Some(i) = info(app) {
            app.ui.status = format!("Layout: {}", place_label(&i.at));
        }
    }
    ui.data_mut(|d| match drag {
        Some(dr) if !released => {
            d.insert_temp(drag_id, dr);
        }
        _ => {
            d.remove::<Drag>(drag_id);
        }
    });
    // ---- right-click on the box: the Layout menu
    if let Some(resp) = &box_resp {
        egui::Popup::context_menu(resp).show(|ui| {
            ui.set_min_width(200.0);
            menu_body(app, ui);
        });
    }
    // ---- select on click (anywhere on the picture that no graphic covers)
    let gfx = ui.ctx().read_response(egui::Id::new("gfx-overlay"));
    let click = [box_resp.as_ref(), gfx.as_ref()].into_iter().flatten().find(|r| r.clicked()).and_then(|r| r.interact_pointer_pos());
    if let Some(p) = click
        && pic.contains(p)
        && !over_graphic(p)
    {
        select_at(app, p, pic, k);
    }
}

/// A click at `p` (screen): select the top-most clip there, or the next one on a repeated click.
fn select_at(app: &mut FilmcraftApp, p: Pos2, pic: Rect, k: (f32, f32)) {
    let repeat = app.ui.layout.last_click.as_ref().filter(|(lp, stack, _)| (*lp - p).length() <= CYCLE_PX && !stack.is_empty()).cloned();
    let (stack, idx) = match repeat {
        Some((_, stack, i)) => {
            let n = stack.len().max(1);
            (stack, (i + 1) % n)
        }
        None => {
            let (x, y) = ((p.x - pic.min.x) / k.0.max(1e-6), (p.y - pic.min.y) / k.1.max(1e-6));
            let picked = app.session.execute("layout.pick", json!({"x": x, "y": y}));
            let ids: Vec<u64> =
                picked.ok().and_then(|v| v.get("clips").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect())).unwrap_or_default();
            (ids, 0)
        }
    };
    let Some(&c) = stack.get(idx) else {
        app.ui.layout.last_click = None;
        return;
    };
    app.session.state.selection = vec![ClipId(c)];
    app.ui.layout.last_click = Some((p, stack, idx));
    if !app.ui.layout.hint_shown {
        app.ui.layout.hint_shown = true;
        app.ui.status = HINT.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(handle: Option<usize>) -> Drag {
        Drag {
            clip: ClipId(1),
            handle,
            start: pos2(0.0, 0.0),
            screen: Rect::from_min_size(pos2(100.0, 100.0), vec2(50.0, 50.0)),
            bx: [100.0, 100.0, 100.0, 100.0],
            position: [150.0, 150.0],
            scale: 50.0,
            scale_width: 50.0,
            uniform: true,
            begun: false,
        }
    }

    #[test]
    fn corner_scales_about_the_opposite_corner() {
        // se handle (index 4) from screen (150,150) to (200,200): twice the size, nw corner fixed
        let d = drag(Some(4));
        let (p, s, _) = drag_target(&d, pos2(200.0, 200.0), (0.5, 0.5), egui::Vec2::ZERO);
        assert!((s - 100.0).abs() < 1e-9);
        assert!((p[0] - 200.0).abs() < 1e-9 && (p[1] - 200.0).abs() < 1e-9, "{p:?}");
    }

    #[test]
    fn edge_scales_about_the_opposite_edge() {
        // w handle (index 7) dragged left by 25 screen = 50 frame px: 1.5×, the right edge stays
        let d = drag(Some(7));
        let (p, s, _) = drag_target(&d, pos2(75.0, 125.0), (0.5, 0.5), egui::Vec2::ZERO);
        assert!((s - 75.0).abs() < 1e-9, "{s}");
        assert!((p[0] - 125.0).abs() < 1e-9, "{p:?}");
    }

    #[test]
    fn move_converts_the_screen_offset_to_frame_pixels() {
        let d = drag(None);
        let (p, s, _) = drag_target(&d, pos2(0.0, 0.0), (0.5, 0.5), egui::vec2(10.0, -5.0));
        assert_eq!((p, s), ([170.0, 140.0], 50.0));
    }

    #[test]
    fn place_keeps_size_and_margin_of_a_placed_clip_only() {
        let mut i = Info { clip: ClipId(1), at: "bottomRight".into(), size: 33.0, margin: Some(5.0), shape: "free".into(), bx: [0.0; 4] };
        assert_eq!(place_params(Some(&i), "topLeft"), json!({"at": "topLeft", "size": 33.0, "margin": 5.0}));
        assert_eq!(place_params(Some(&i), "full"), json!({"at": "full"}));
        i.at = "full".into();
        i.size = 100.0;
        assert_eq!(place_params(Some(&i), "topLeft"), json!({"at": "topLeft"}));
        assert_eq!(place_params(None, "center"), json!({"at": "center"}));
    }

    #[test]
    fn quad_hit_test() {
        let q = [pos2(0.0, 0.0), pos2(10.0, 0.0), pos2(10.0, 10.0), pos2(0.0, 10.0)];
        assert!(in_quad(&q, pos2(5.0, 5.0)));
        assert!(!in_quad(&q, pos2(15.0, 5.0)));
    }
}
