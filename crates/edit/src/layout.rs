//! Clip layout geometry: where a clip's visible box sits in the frame (place), its shape mask, and
//! the Motion position / scale that put it there. Pure functions; the engine's `layout.*` commands
//! (`crates/engine/src/layout.rs`) and the Program monitor handles use them.
//! See `openspec/changes/clip-layouts/design.md` §1–2 and `docs/layouts.md`.
//!
//! The maths mirrors `filmcraft_render::motion_matrix`: a source pixel `p` lands at
//! `position + R(rotation) · S(scale × fit) · (p − anchor)`, where `position` defaults to the frame
//! centre and `anchor` to the source centre when they are NaN ("auto"), and `fit` is the
//! Scale-to-Frame factor `min(frame / source)` when the clip has it. This crate does not depend on
//! `filmcraft_geom`, so points are `(x, y)` tuples and the 2×2 linear part is written out here.
//!
//! The **visible box** is the axis-aligned bounding box, in frame pixels, of the shape's bounds
//! after Motion: the central square of the source for a circle or square, the whole source
//! otherwise. A circle's box is exact under rotation (an ellipse's bounding box); a square's or
//! rounded rectangle's is the box of the rotated rectangle.

use filmcraft_project::MaskPath;

/// Default `size` of `layout.place` (% of the frame width).
pub const DEFAULT_SIZE: f64 = 25.0;
/// Default `margin` of `layout.place` (% of the frame width).
pub const DEFAULT_MARGIN: f64 = 3.0;
/// Default corner radius of the rounded shape (% of the shorter side).
pub const DEFAULT_RADIUS: f64 = 12.0;
/// Largest Motion scale (%), the parameter's range.
pub const MAX_SCALE: f64 = 10_000.0;
/// Tolerance of [`nearest_place`] / [`infer_place`]: this fraction of the frame width.
pub const PLACE_TOLERANCE: f64 = 0.01;

/// `v` clamped to `lo..=hi`, `default` when it is not a finite number.
fn clamp_or(v: f64, lo: f64, hi: f64, default: f64) -> f64 {
    if v.is_finite() { v.max(lo).min(hi) } else { default }
}

/// `size` clamped to 1–100 % (NaN → the default).
pub fn clamp_size(v: f64) -> f64 {
    clamp_or(v, 1.0, 100.0, DEFAULT_SIZE)
}
/// `margin` clamped to 0–45 % (NaN → the default).
pub fn clamp_margin(v: f64) -> f64 {
    clamp_or(v, 0.0, 45.0, DEFAULT_MARGIN)
}
/// `radius` clamped to 0–50 % (NaN → the default).
pub fn clamp_radius(v: f64) -> f64 {
    clamp_or(v, 0.0, 50.0, DEFAULT_RADIUS)
}

/// Where the visible box sits in the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Top,
    Bottom,
    Left,
    Right,
    Center,
    /// The whole source fitted in the frame, centred (size and margin ignored).
    Full,
}

impl Place {
    pub const ALL: [Place; 10] = [
        Place::TopLeft,
        Place::TopRight,
        Place::BottomLeft,
        Place::BottomRight,
        Place::Top,
        Place::Bottom,
        Place::Left,
        Place::Right,
        Place::Center,
        Place::Full,
    ];

    /// The command id (`topLeft`, …, `full`).
    pub fn name(self) -> &'static str {
        match self {
            Place::TopLeft => "topLeft",
            Place::TopRight => "topRight",
            Place::BottomLeft => "bottomLeft",
            Place::BottomRight => "bottomRight",
            Place::Top => "top",
            Place::Bottom => "bottom",
            Place::Left => "left",
            Place::Right => "right",
            Place::Center => "center",
            Place::Full => "full",
        }
    }

    /// Parse a command id (case-insensitive; `centre` is accepted).
    pub fn parse(s: &str) -> Option<Place> {
        let l = s.trim().to_ascii_lowercase();
        if l == "centre" {
            return Some(Place::Center);
        }
        Place::ALL.into_iter().find(|p| p.name().eq_ignore_ascii_case(&l))
    }

    /// Horizontal alignment: -1 left, 0 centre, 1 right.
    fn h(self) -> i8 {
        match self {
            Place::TopLeft | Place::BottomLeft | Place::Left => -1,
            Place::TopRight | Place::BottomRight | Place::Right => 1,
            _ => 0,
        }
    }
    /// Vertical alignment: -1 top, 0 centre, 1 bottom.
    fn v(self) -> i8 {
        match self {
            Place::TopLeft | Place::TopRight | Place::Top => -1,
            Place::BottomLeft | Place::BottomRight | Place::Bottom => 1,
            _ => 0,
        }
    }
}

/// The layout shape: which part of the source shows (an Opacity mask named `Layout shape`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// A circle inscribed in the central square of the source.
    Circle,
    /// The whole source with rounded corners; `radius_pct` is % of the shorter side (0–50).
    Rounded { radius_pct: f64 },
    /// The central square of the source.
    Square,
    /// No layout mask: the whole source.
    Free,
}

impl Shape {
    /// The command id (`circle`, `rounded`, `square`, `free`).
    pub fn name(self) -> &'static str {
        match self {
            Shape::Circle => "circle",
            Shape::Rounded { .. } => "rounded",
            Shape::Square => "square",
            Shape::Free => "free",
        }
    }
    /// Parse a command id; `radius_pct` (clamped 0–50) is used by `rounded`.
    pub fn parse(s: &str, radius_pct: f64) -> Option<Shape> {
        match s.trim().to_ascii_lowercase().as_str() {
            "circle" | "round" => Some(Shape::Circle),
            "rounded" | "roundedrect" | "roundedrectangle" => Some(Shape::Rounded { radius_pct: clamp_radius(radius_pct) }),
            "square" => Some(Shape::Square),
            "free" | "none" => Some(Shape::Free),
            _ => None,
        }
    }
    /// The corner radius for `rounded`.
    pub fn radius(self) -> Option<f64> {
        match self {
            Shape::Rounded { radius_pct } => Some(radius_pct),
            _ => None,
        }
    }
    /// Whether the shape uses the central square of the source.
    fn central(self) -> bool {
        matches!(self, Shape::Circle | Shape::Square)
    }
}

/// A clip's Motion values (and Scale to Frame), as `filmcraft_render::motion_matrix` reads them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// Frame pixels; NaN = the frame centre.
    pub position: (f64, f64),
    /// Scale width, scale height (%). Uniform scale has both equal.
    pub scale: (f64, f64),
    /// Source pixels; NaN = the source centre.
    pub anchor: (f64, f64),
    /// Degrees, clockwise on screen.
    pub rotation: f64,
    /// Scale to Frame Size (`TrackItem::scale_to_frame`).
    pub fit: bool,
}

impl Default for Pose {
    fn default() -> Self {
        Self { position: (f64::NAN, f64::NAN), scale: (100.0, 100.0), anchor: (f64::NAN, f64::NAN), rotation: 0.0, fit: false }
    }
}

fn fsize(s: (u32, u32)) -> (f64, f64) {
    (f64::from(s.0), f64::from(s.1))
}

fn valid(s: (u32, u32)) -> bool {
    s.0 > 0 && s.1 > 0
}

/// The Scale-to-Frame factor (1 without it).
fn fit_factor(frame: (u32, u32), src: (u32, u32), fit: bool) -> f64 {
    if !fit || !valid(frame) || !valid(src) {
        return 1.0;
    }
    let (fw, fh) = fsize(frame);
    let (sw, sh) = fsize(src);
    (fw / sw).min(fh / sh)
}

/// A finite number or `d`.
fn fin(v: f64, d: f64) -> f64 {
    if v.is_finite() { v } else { d }
}

/// Position and anchor with "auto" (NaN) resolved like the renderer.
fn resolve(frame: (u32, u32), src: (u32, u32), pose: &Pose) -> ((f64, f64), (f64, f64)) {
    let (fw, fh) = fsize(frame);
    let (sw, sh) = fsize(src);
    let pos = if pose.position.0.is_nan() || pose.position.1.is_nan() {
        (fw / 2.0, fh / 2.0)
    } else {
        (fin(pose.position.0, fw / 2.0), fin(pose.position.1, fh / 2.0))
    };
    let anchor =
        if pose.anchor.0.is_nan() || pose.anchor.1.is_nan() { (sw / 2.0, sh / 2.0) } else { (fin(pose.anchor.0, sw / 2.0), fin(pose.anchor.1, sh / 2.0)) };
    (pos, anchor)
}

/// The linear part `[a c; b d]` of the Motion matrix (`x' = a x + c y`, `y' = b x + d y`).
fn linear(frame: (u32, u32), src: (u32, u32), pose: &Pose) -> [f64; 4] {
    let f = fit_factor(frame, src, pose.fit);
    let sx = fin(pose.scale.0, 100.0) / 100.0 * f;
    let sy = fin(pose.scale.1, 100.0) / 100.0 * f;
    let (s, c) = fin(pose.rotation, 0.0).to_radians().sin_cos();
    [c * sx, s * sx, -s * sy, c * sy]
}

/// The source rectangle the shape shows, `[x, y, w, h]` in source pixels: the central square for
/// a circle or square, the whole source otherwise.
pub fn shape_bounds(src: (u32, u32), shape: Shape) -> [f64; 4] {
    let (sw, sh) = fsize(src);
    if shape.central() {
        let side = sw.min(sh);
        [(sw - side) / 2.0, (sh - side) / 2.0, side, side]
    } else {
        [0.0, 0.0, sw, sh]
    }
}

/// One mask vertex: point, incoming tangent, outgoing tangent (as [`MaskPath::components`]).
type Vtx = [f64; 6];

fn path_of(vertices: &[Vtx]) -> MaskPath {
    let c: Vec<f64> = vertices.iter().flatten().copied().collect();
    let mut p = MaskPath::default().with_components(&c);
    p.closed = true;
    p
}

/// The `Layout shape` mask path in source pixels: an ellipse inscribed in the central square
/// (circle), that square (4 corner vertices), or a Bézier rounded rectangle of the whole source
/// with the radius as % of the shorter side. `None` for `free` or a source without a size.
pub fn shape_path(src: (u32, u32), shape: Shape) -> Option<MaskPath> {
    if !valid(src) {
        return None;
    }
    let [x, y, w, h] = shape_bounds(src, shape);
    let (x1, y1) = (x + w, y + h);
    let k = filmcraft_project::mask::KAPPA;
    match shape {
        Shape::Free => None,
        Shape::Circle => {
            let (cx, cy, r) = (x + w / 2.0, y + h / 2.0, w / 2.0);
            // the vertices of `MaskPath::ellipse`: top, right, bottom, left, mirrored tangents
            Some(path_of(&[
                [cx, cy - r, -r * k, 0.0, r * k, 0.0],
                [cx + r, cy, 0.0, -r * k, 0.0, r * k],
                [cx, cy + r, r * k, 0.0, -r * k, 0.0],
                [cx - r, cy, 0.0, r * k, 0.0, -r * k],
            ]))
        }
        Shape::Square => Some(path_of(&[[x, y, 0.0, 0.0, 0.0, 0.0], [x1, y, 0.0, 0.0, 0.0, 0.0], [x1, y1, 0.0, 0.0, 0.0, 0.0], [x, y1, 0.0, 0.0, 0.0, 0.0]])),
        Shape::Rounded { radius_pct } => {
            let r = (clamp_radius(radius_pct) / 100.0 * w.min(h)).min(w.min(h) / 2.0);
            if r <= 0.0 {
                return Some(path_of(&[[x, y, 0.0, 0.0, 0.0, 0.0], [x1, y, 0.0, 0.0, 0.0, 0.0], [x1, y1, 0.0, 0.0, 0.0, 0.0], [x, y1, 0.0, 0.0, 0.0, 0.0]]));
            }
            let t = r * k;
            Some(path_of(&[
                [x + r, y, -t, 0.0, 0.0, 0.0],
                [x1 - r, y, 0.0, 0.0, t, 0.0],
                [x1, y + r, 0.0, -t, 0.0, 0.0],
                [x1, y1 - r, 0.0, 0.0, 0.0, t],
                [x1 - r, y1, t, 0.0, 0.0, 0.0],
                [x + r, y1, 0.0, 0.0, -t, 0.0],
                [x, y1 - r, 0.0, t, 0.0, 0.0],
                [x, y + r, 0.0, 0.0, 0.0, -t],
            ]))
        }
    }
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

/// Recognise a path [`shape_path`] made for this source: `Some(shape)` for a circle, square or
/// rounded rectangle (radius read back), `None` for any other path (edited by hand).
pub fn shape_of_path(src: (u32, u32), path: &MaskPath) -> Option<Shape> {
    if !valid(src) {
        return None;
    }
    let tol = 0.5;
    let same =
        |s: Shape| shape_path(src, s).is_some_and(|q| q.len() == path.len() && q.components().iter().zip(path.components()).all(|(a, b)| close(*a, b, tol)));
    if same(Shape::Circle) {
        return Some(Shape::Circle);
    }
    if same(Shape::Square) {
        return Some(Shape::Square);
    }
    let (sw, sh) = fsize(src);
    let short = sw.min(sh);
    let radius_pct = match path.len() {
        4 => 0.0,
        8 => {
            let v0 = path.vertices.first()?;
            if short <= 0.0 { 0.0 } else { (v0.p.x / short * 100.0 * 1000.0).round() / 1000.0 }
        }
        _ => return None,
    };
    let s = Shape::Rounded { radius_pct: clamp_radius(radius_pct) };
    same(s).then_some(s)
}

/// Box of the shape's bounds under the linear part `l` centred at `centre`: `[x, y, w, h]`.
fn box_at(src: (u32, u32), shape: Shape, l: [f64; 4], centre: (f64, f64)) -> [f64; 4] {
    let [_, _, bw, bh] = shape_bounds(src, shape);
    let [a, b, c, d] = l;
    let (hx, hy) = if matches!(shape, Shape::Circle) {
        let (rx, ry) = (bw / 2.0, bh / 2.0);
        (((a * rx).powi(2) + (c * ry).powi(2)).sqrt(), ((b * rx).powi(2) + (d * ry).powi(2)).sqrt())
    } else {
        ((a.abs() * bw + c.abs() * bh) / 2.0, (b.abs() * bw + d.abs() * bh) / 2.0)
    };
    let r = [centre.0 - hx, centre.1 - hy, 2.0 * hx, 2.0 * hy];
    if r.iter().all(|v| v.is_finite()) { r } else { [0.0; 4] }
}

/// Where the centre of the shape's bounds lands in the frame.
fn shape_centre(frame: (u32, u32), src: (u32, u32), shape: Shape, pose: &Pose) -> (f64, f64) {
    let (pos, anchor) = resolve(frame, src, pose);
    let [a, b, c, d] = linear(frame, src, pose);
    let [x, y, w, h] = shape_bounds(src, shape);
    let (dx, dy) = (x + w / 2.0 - anchor.0, y + h / 2.0 - anchor.1);
    (pos.0 + a * dx + c * dy, pos.1 + b * dx + d * dy)
}

/// The visible box `[x, y, w, h]` (frame pixels) of a clip with this shape and Motion: the
/// axis-aligned bounds of the transformed shape bounds. A source without a size gives an empty
/// box at the position.
pub fn visible_box(frame: (u32, u32), src: (u32, u32), shape: Shape, pose: &Pose) -> [f64; 4] {
    if !valid(src) || !valid(frame) {
        let (pos, _) = resolve(frame, src, pose);
        return [fin(pos.0, 0.0), fin(pos.1, 0.0), 0.0, 0.0];
    }
    box_at(src, shape, linear(frame, src, pose), shape_centre(frame, src, shape, pose))
}

/// Motion `(position, uniform scale %)` that puts the visible box's centre at `centre` with width
/// `width` (frame pixels), keeping the pose's anchor, rotation and Scale to Frame.
pub fn place_box(frame: (u32, u32), src: (u32, u32), shape: Shape, centre: (f64, f64), width: f64, pose: &Pose) -> ((f64, f64), f64) {
    let (fw, fh) = fsize(frame);
    let fallback = ((fw / 2.0, fh / 2.0), 100.0);
    if !valid(frame) || !valid(src) || !centre.0.is_finite() || !centre.1.is_finite() || !width.is_finite() {
        return fallback;
    }
    let unit = Pose { scale: (100.0, 100.0), ..*pose };
    let w1 = box_at(src, shape, linear(frame, src, &unit), (0.0, 0.0))[2];
    if w1 <= 0.0 || !w1.is_finite() {
        return fallback;
    }
    let scale = (100.0 * width.max(0.0) / w1).min(MAX_SCALE);
    position_for(frame, src, shape, centre, scale, pose)
}

/// The position that puts the shape centre at `centre` at this uniform scale.
fn position_for(frame: (u32, u32), src: (u32, u32), shape: Shape, centre: (f64, f64), scale: f64, pose: &Pose) -> ((f64, f64), f64) {
    let p = Pose { scale: (scale, scale), position: (0.0, 0.0), ..*pose };
    // with position 0 the shape centre lands at L·(c − anchor): subtract it
    let off = shape_centre(frame, src, shape, &p);
    ((centre.0 - off.0, centre.1 - off.1), scale)
}

/// Motion `(position, uniform scale %)` for `layout.place`: the visible box lands at `at` with
/// width `size_pct` % of the frame width and `margin_pct` % of the frame width from the edges it
/// touches. `Full` fits the whole source (not the shape) in the frame, centred. The pose's anchor,
/// rotation and Scale to Frame are kept (its position and scale are what is computed).
pub fn place(frame: (u32, u32), src: (u32, u32), shape: Shape, at: Place, size_pct: f64, margin_pct: f64, pose: &Pose) -> ((f64, f64), f64) {
    let (fw, fh) = fsize(frame);
    if !valid(frame) || !valid(src) {
        return ((fw / 2.0, fh / 2.0), 100.0);
    }
    let unit = Pose { scale: (100.0, 100.0), ..*pose };
    let l = linear(frame, src, &unit);
    if at == Place::Full {
        let b = box_at(src, Shape::Free, l, (0.0, 0.0));
        if b[2] <= 0.0 || b[3] <= 0.0 {
            return ((fw / 2.0, fh / 2.0), 100.0);
        }
        let scale = (100.0 * (fw / b[2]).min(fh / b[3])).min(MAX_SCALE);
        return position_for(frame, src, Shape::Free, (fw / 2.0, fh / 2.0), scale, pose);
    }
    let b1 = box_at(src, shape, l, (0.0, 0.0));
    if b1[2] <= 0.0 || !b1[2].is_finite() {
        return ((fw / 2.0, fh / 2.0), 100.0);
    }
    let w = clamp_size(size_pct) / 100.0 * fw;
    let h = b1[3] * w / b1[2];
    let m = clamp_margin(margin_pct) / 100.0 * fw;
    let cx = match at.h() {
        -1 => m + w / 2.0,
        1 => fw - m - w / 2.0,
        _ => fw / 2.0,
    };
    let cy = match at.v() {
        -1 => m + h / 2.0,
        1 => fh - m - h / 2.0,
        _ => fh / 2.0,
    };
    place_box(frame, src, shape, (cx, cy), w, pose)
}

/// Motion `(position, uniform scale %)` that gives the clip a new shape (or source, for a swap)
/// where `old_box` was: same width, and the edges the box's place touches stay where they are
/// (a bottom-right box keeps its right and bottom edges, a centred box its centre; a custom box
/// keeps its centre). A `Full` box stays full. So a circle given to a box placed bottom right is
/// still bottom right with the same margin, and doing it twice gives the first box back.
pub fn refit(frame: (u32, u32), src: (u32, u32), shape: Shape, old_box: [f64; 4], pose: &Pose) -> ((f64, f64), f64) {
    let (fw, fh) = fsize(frame);
    if !valid(frame) || !valid(src) || old_box.iter().any(|v| !v.is_finite()) {
        return ((fw / 2.0, fh / 2.0), 100.0);
    }
    let at = infer_place(frame, old_box).map(|(p, _)| p);
    if at == Some(Place::Full) {
        return place(frame, src, shape, Place::Full, 100.0, 0.0, pose);
    }
    let [x, y, w, h] = old_box;
    let unit = Pose { scale: (100.0, 100.0), ..*pose };
    let b1 = box_at(src, shape, linear(frame, src, &unit), (0.0, 0.0));
    if b1[2] <= 0.0 || !b1[2].is_finite() {
        return ((fw / 2.0, fh / 2.0), 100.0);
    }
    let nh = b1[3] * w / b1[2];
    // the width is kept, so horizontally the centre stays; vertically the placed edge does
    let cx = x + w / 2.0;
    let cy = match at.map(|p| p.v()).unwrap_or(0) {
        -1 => y + nh / 2.0,
        1 => y + h - nh / 2.0,
        _ => y + h / 2.0,
    };
    place_box(frame, src, shape, (cx, cy), w, pose)
}

/// The box a place gives a box of this size (`Full`: `None`, it depends on the source).
fn expected(frame: (u32, u32), w: f64, h: f64, at: Place, m: f64) -> Option<(f64, f64)> {
    let (fw, fh) = fsize(frame);
    let x = match at.h() {
        -1 => m,
        1 => fw - m - w,
        _ => (fw - w) / 2.0,
    };
    let y = match at.v() {
        -1 => m,
        1 => fh - m - h,
        _ => (fh - h) / 2.0,
    };
    (at != Place::Full).then_some((x, y))
}

/// Whether the box is the whole source fitted in the frame and centred.
fn is_full(frame: (u32, u32), b: [f64; 4], tol: f64) -> bool {
    let (fw, fh) = fsize(frame);
    let [x, y, w, h] = b;
    let fits = (close(w, fw, tol) && h <= fh + tol) || (close(h, fh, tol) && w <= fw + tol);
    fits && close(x + w / 2.0, fw / 2.0, tol) && close(y + h / 2.0, fh / 2.0, tol)
}

/// The place whose box (at this box's size, with `margin_pct`) this box is, within 1 % of the
/// frame width; `None` = custom. A centred box that fills the frame on one axis is `Full`.
pub fn nearest_place(frame: (u32, u32), b: [f64; 4], margin_pct: f64) -> Option<Place> {
    if !valid(frame) || b.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let (fw, _) = fsize(frame);
    let tol = PLACE_TOLERANCE * fw;
    if is_full(frame, b, tol) {
        return Some(Place::Full);
    }
    let m = clamp_margin(margin_pct) / 100.0 * fw;
    Place::ALL.into_iter().find(|p| expected(frame, b[2], b[3], *p, m).is_some_and(|(x, y)| close(x, b[0], tol) && close(y, b[1], tol)))
}

/// The place this box is at and the margin it implies (% of the frame width; `None` for
/// `center` / `full`), within 1 % of the frame width; `None` = custom. Corners need the same
/// margin on both edges; an edge place takes the margin from its edge (0–45 %).
pub fn infer_place(frame: (u32, u32), b: [f64; 4]) -> Option<(Place, Option<f64>)> {
    if !valid(frame) || b.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let (fw, fh) = fsize(frame);
    let tol = PLACE_TOLERANCE * fw;
    let [x, y, w, h] = b;
    if is_full(frame, b, tol) {
        return Some((Place::Full, None));
    }
    let centred_x = close(x + w / 2.0, fw / 2.0, tol);
    let centred_y = close(y + h / 2.0, fh / 2.0, tol);
    if centred_x && centred_y {
        return Some((Place::Center, None));
    }
    let max_m = 45.0 / 100.0 * fw + tol;
    let ok = |m: f64| m >= -tol && m <= max_m;
    let pct = |m: f64| m.max(0.0) / fw * 100.0;
    let (left, right, top, bottom) = (x, fw - x - w, y, fh - y - h);
    for at in [Place::TopLeft, Place::TopRight, Place::BottomLeft, Place::BottomRight] {
        let mx = if at.h() < 0 { left } else { right };
        let my = if at.v() < 0 { top } else { bottom };
        if close(mx, my, tol) && ok(mx) && ok(my) {
            return Some((at, Some(pct((mx + my) / 2.0))));
        }
    }
    let edges = [(Place::Top, centred_x, top), (Place::Bottom, centred_x, bottom), (Place::Left, centred_y, left), (Place::Right, centred_y, right)];
    edges.into_iter().filter(|(_, c, m)| *c && ok(*m)).min_by(|a, b| a.2.total_cmp(&b.2)).map(|(at, _, m)| (at, Some(pct(m))))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: (u32, u32) = (1920, 1080);

    fn near(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    fn placed(src: (u32, u32), shape: Shape, at: Place, size: f64, margin: f64, pose: Pose) -> [f64; 4] {
        let (position, s) = place(HD, src, shape, at, size, margin, &pose);
        visible_box(HD, src, shape, &Pose { position, scale: (s, s), ..pose })
    }

    #[test]
    fn names_round_trip() {
        for p in Place::ALL {
            assert_eq!(Place::parse(p.name()), Some(p));
        }
        assert_eq!(Place::parse("BOTTOMRIGHT"), Some(Place::BottomRight));
        assert_eq!(Place::parse("nowhere"), None);
        for s in [Shape::Circle, Shape::Square, Shape::Free, Shape::Rounded { radius_pct: 12.0 }] {
            assert_eq!(Shape::parse(s.name(), 12.0), Some(s));
        }
        assert_eq!(Shape::parse("rounded", 900.0), Some(Shape::Rounded { radius_pct: 50.0 }));
        assert_eq!(Shape::parse("rounded", f64::NAN), Some(Shape::Rounded { radius_pct: DEFAULT_RADIUS }));
        assert_eq!(Shape::parse("blob", 1.0), None);
    }

    #[test]
    fn default_pose_is_the_whole_frame() {
        assert_eq!(visible_box(HD, HD, Shape::Free, &Pose::default()), [0.0, 0.0, 1920.0, 1080.0]);
        assert_eq!(visible_box(HD, HD, Shape::Circle, &Pose::default()), [420.0, 0.0, 1080.0, 1080.0]);
        // Scale to Frame of a 4K source
        let b = visible_box(HD, (3840, 2160), Shape::Free, &Pose { fit: true, ..Pose::default() });
        assert_eq!(b, [0.0, 0.0, 1920.0, 1080.0]);
    }

    #[test]
    fn every_preset_lands_with_its_margin() {
        let (fw, fh) = (1920.0, 1080.0);
        let m = 0.03 * fw;
        for src in [HD, (1280, 720), (1080, 1920), (640, 480)] {
            for shape in [Shape::Free, Shape::Circle, Shape::Square, Shape::Rounded { radius_pct: 12.0 }] {
                for at in Place::ALL.into_iter().filter(|p| *p != Place::Full) {
                    let b = placed(src, shape, at, 25.0, 3.0, Pose::default());
                    assert!(near(b[2], 0.25 * fw, 1e-6), "{at:?} {shape:?} {src:?} width {b:?}");
                    let (l, r, t, bo) = (b[0], fw - b[0] - b[2], b[1], fh - b[1] - b[3]);
                    match at.h() {
                        -1 => assert!(near(l, m, 1e-6), "{at:?} {b:?}"),
                        1 => assert!(near(r, m, 1e-6), "{at:?} {b:?}"),
                        _ => assert!(near(l, r, 1e-6), "{at:?} {b:?}"),
                    }
                    match at.v() {
                        -1 => assert!(near(t, m, 1e-6), "{at:?} {b:?}"),
                        1 => assert!(near(bo, m, 1e-6), "{at:?} {b:?}"),
                        _ => assert!(near(t, bo, 1e-6), "{at:?} {b:?}"),
                    }
                    assert_eq!(nearest_place(HD, b, 3.0), Some(at), "{shape:?} {src:?} {b:?}");
                    let (got, margin) = infer_place(HD, b).unwrap();
                    assert_eq!(got, at, "{shape:?} {src:?} {b:?}");
                    if at != Place::Center {
                        assert!(near(margin.unwrap(), 3.0, 0.01));
                    }
                }
            }
        }
    }

    #[test]
    fn circle_keeps_its_width_and_is_round() {
        let b = placed(HD, Shape::Circle, Place::BottomRight, 25.0, 3.0, Pose::default());
        assert!(near(b[2], 480.0, 1e-6) && near(b[3], 480.0, 1e-6), "{b:?}");
        // rotated: a circle's box is still its diameter
        let b = placed(HD, Shape::Circle, Place::BottomRight, 25.0, 3.0, Pose { rotation: 37.0, ..Pose::default() });
        assert!(near(b[2], 480.0, 1e-6) && near(b[3], 480.0, 1e-6), "{b:?}");
        assert!(near(1920.0 - b[0] - b[2], 57.6, 1e-6), "{b:?}");
    }

    #[test]
    fn full_fits_both_orientations() {
        for (src, w, h) in [((1080, 1920), 1080.0 * 1080.0 / 1920.0, 1080.0), ((3000, 1000), 1920.0, 640.0), (HD, 1920.0, 1080.0)] {
            for shape in [Shape::Free, Shape::Circle] {
                let (position, s) = place(HD, src, shape, Place::Full, 25.0, 3.0, &Pose::default());
                let b = visible_box(HD, src, Shape::Free, &Pose { position, scale: (s, s), ..Pose::default() });
                assert!(near(b[2], w, 1e-6) && near(b[3], h, 1e-6), "{src:?} {b:?}");
                assert!(near(b[0] + b[2] / 2.0, 960.0, 1e-6) && near(b[1] + b[3] / 2.0, 540.0, 1e-6));
                assert_eq!(nearest_place(HD, b, 3.0), Some(Place::Full));
                assert_eq!(infer_place(HD, b), Some((Place::Full, None)));
            }
        }
        // with Scale to Frame the scale is relative to the fitted size
        let (_, s) = place(HD, (3840, 2160), Shape::Free, Place::Full, 25.0, 3.0, &Pose { fit: true, ..Pose::default() });
        assert!(near(s, 100.0, 1e-9));
    }

    #[test]
    fn custom_anchor_and_rotation_still_place() {
        let pose = Pose { anchor: (100.0, 50.0), rotation: 20.0, ..Pose::default() };
        for shape in [Shape::Free, Shape::Square, Shape::Circle] {
            let b = placed((1280, 720), shape, Place::TopLeft, 30.0, 5.0, pose);
            assert!(near(b[0], 96.0, 1e-6) && near(b[1], 96.0, 1e-6) && near(b[2], 576.0, 1e-6), "{shape:?} {b:?}");
        }
    }

    #[test]
    fn place_box_keeps_centre_and_width_across_shapes() {
        let b = placed(HD, Shape::Free, Place::BottomRight, 25.0, 3.0, Pose::default());
        let c = (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
        let (position, s) = place_box(HD, HD, Shape::Circle, c, b[2], &Pose::default());
        let nb = visible_box(HD, HD, Shape::Circle, &Pose { position, scale: (s, s), ..Pose::default() });
        assert!(near(nb[0] + nb[2] / 2.0, c.0, 1e-6) && near(nb[1] + nb[3] / 2.0, c.1, 1e-6) && near(nb[2], b[2], 1e-6), "{nb:?}");
        assert!(near(nb[3], nb[2], 1e-9));
    }

    #[test]
    fn shape_paths() {
        assert!(shape_path(HD, Shape::Free).is_none());
        let c = shape_path(HD, Shape::Circle).unwrap();
        assert_eq!(c.len(), 4);
        assert!(c.closed);
        let (lo, hi) = c.bounds();
        assert!(near(lo.x, 420.0, 1e-9) && near(hi.x, 1500.0, 1e-9) && near(lo.y, 0.0, 1e-9) && near(hi.y, 1080.0, 1e-9));
        let sq = shape_path(HD, Shape::Square).unwrap();
        assert!(sq.vertices.iter().all(|v| v.is_corner()));
        let r = shape_path(HD, Shape::Rounded { radius_pct: 12.0 }).unwrap();
        assert_eq!(r.len(), 8);
        let (lo, hi) = r.bounds();
        assert!(near(lo.x, 0.0, 1e-9) && near(hi.x, 1920.0, 1e-9) && near(hi.y, 1080.0, 1e-9));
        for s in [Shape::Circle, Shape::Square, Shape::Rounded { radius_pct: 12.0 }, Shape::Rounded { radius_pct: 0.0 }, Shape::Rounded { radius_pct: 50.0 }] {
            for src in [HD, (1080, 1920), (500, 500)] {
                let p = shape_path(src, s).unwrap();
                let back = shape_of_path(src, &p);
                if src.0 == src.1 && matches!(s, Shape::Rounded { radius_pct: 0.0 }) {
                    // a square source: the square is the whole source
                    assert_eq!(back, Some(Shape::Square));
                } else {
                    assert_eq!(back, Some(s), "{src:?}");
                }
            }
        }
        let mut edited = c.clone();
        edited.vertices[0].p.x += 40.0;
        assert_eq!(shape_of_path(HD, &edited), None);
    }

    #[test]
    fn custom_boxes() {
        assert_eq!(nearest_place(HD, [300.0, 200.0, 480.0, 270.0], 3.0), None);
        assert_eq!(infer_place(HD, [300.0, 200.0, 480.0, 270.0]), None);
        // centred horizontally 10 % from the top
        let b = [720.0, 192.0, 480.0, 270.0];
        assert_eq!(infer_place(HD, b), Some((Place::Top, Some(10.0))));
        assert_eq!(nearest_place(HD, b, 3.0), None);
        assert_eq!(nearest_place(HD, b, 10.0), Some(Place::Top));
    }

    #[test]
    fn refit_keeps_the_placed_edges() {
        let b = placed(HD, Shape::Free, Place::BottomRight, 25.0, 3.0, Pose::default());
        let (position, s) = refit(HD, HD, Shape::Circle, b, &Pose::default());
        let c = visible_box(HD, HD, Shape::Circle, &Pose { position, scale: (s, s), ..Pose::default() });
        assert!(near(c[2], 480.0, 1e-6) && near(c[3], 480.0, 1e-6));
        assert!(near(c[0] + c[2], b[0] + b[2], 1e-6) && near(c[1] + c[3], b[1] + b[3], 1e-6), "{c:?} {b:?}");
        assert_eq!(nearest_place(HD, c, 3.0), Some(Place::BottomRight));
        // and back
        let (position, s) = refit(HD, HD, Shape::Free, c, &Pose::default());
        let d = visible_box(HD, HD, Shape::Free, &Pose { position, scale: (s, s), ..Pose::default() });
        assert!(d.iter().zip(b).all(|(x, y)| near(*x, y, 1e-9)), "{d:?} {b:?}");
        // a custom box keeps its centre; a full one stays full
        let custom = [300.0, 200.0, 480.0, 270.0];
        let (position, s) = refit(HD, HD, Shape::Square, custom, &Pose::default());
        let e = visible_box(HD, HD, Shape::Square, &Pose { position, scale: (s, s), ..Pose::default() });
        assert!(near(e[0] + e[2] / 2.0, 540.0, 1e-9) && near(e[1] + e[3] / 2.0, 335.0, 1e-9) && near(e[2], 480.0, 1e-9));
        let (_, s) = refit(HD, (1080, 1920), Shape::Circle, [0.0, 0.0, 1920.0, 1080.0], &Pose::default());
        assert!(near(s, 56.25, 1e-9));
    }

    #[test]
    fn hostile_inputs_never_panic() {
        let nan = f64::NAN;
        let poses = [
            Pose::default(),
            Pose { position: (nan, 3.0), scale: (nan, nan), anchor: (nan, nan), rotation: nan, fit: true },
            Pose { position: (f64::INFINITY, -f64::INFINITY), scale: (0.0, 0.0), anchor: (1e300, -1e300), rotation: 1e308, fit: true },
            Pose { scale: (-50.0, 1e12), ..Pose::default() },
        ];
        for frame in [HD, (0, 0), (0, 1080), (1, 1)] {
            for src in [HD, (0, 0), (1, 0), (u32::MAX, 1)] {
                for shape in [Shape::Free, Shape::Circle, Shape::Square, Shape::Rounded { radius_pct: nan }] {
                    let _ = shape_bounds(src, shape);
                    let _ = shape_path(src, shape);
                    for pose in &poses {
                        let b = visible_box(frame, src, shape, pose);
                        let _ = nearest_place(frame, b, nan);
                        let _ = infer_place(frame, b);
                        for at in Place::ALL {
                            let (p, s) = place(frame, src, shape, at, nan, -1e9, pose);
                            assert!(!s.is_nan() && s <= MAX_SCALE, "{s}");
                            let _ = (p, place(frame, src, shape, at, 1e300, 1e300, pose));
                        }
                        let _ = place_box(frame, src, shape, (nan, nan), nan, pose);
                    }
                }
            }
        }
        assert_eq!(visible_box(HD, (0, 0), Shape::Circle, &Pose::default()), [960.0, 540.0, 0.0, 0.0]);
        assert_eq!(clamp_size(0.0), 1.0);
        assert_eq!(clamp_margin(99.0), 45.0);
        assert_eq!(clamp_radius(-3.0), 0.0);
    }
}
