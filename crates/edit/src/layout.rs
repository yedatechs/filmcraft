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
//!
//! The **pan** `(dx, dy)` (source pixels) moves the shape inside the source: a circle panned by
//! `(-1000, 0)` shows a part of the picture 1000 source pixels left of the centre. It is clamped so
//! the shape's bounds stay inside the source (a rounded rectangle or free shape, which use the
//! whole source, always have pan 0). The visible box follows the panned bounds, so for a given
//! place [`place`] / [`place_box`] / [`refit`] compute the position that puts the panned shape
//! there, and [`pan_position`] the position that keeps the box where it is when the pan changes.

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

/// A pan of the shape inside the source, `(dx, dy)` in source pixels; `(0, 0)` = centred.
pub type Pan = (f64, f64);
/// No pan: the shape centred in the source.
pub const NO_PAN: Pan = (0.0, 0.0);

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

/// The pan clamped so the shape's bounds stay inside the source: a circle or square moves at most
/// half the difference between the source's sides; other shapes (and a source without a size)
/// get `(0, 0)`. NaN or infinite components count as 0.
pub fn clamp_pan(src: (u32, u32), shape: Shape, pan: Pan) -> Pan {
    if !shape.central() || !valid(src) {
        return NO_PAN;
    }
    let (sw, sh) = fsize(src);
    let side = sw.min(sh);
    let (mx, my) = ((sw - side) / 2.0, (sh - side) / 2.0);
    // both bounds are ≥ 0, so they never cross
    (clamp_or(pan.0, -mx, mx, 0.0), clamp_or(pan.1, -my, my, 0.0))
}

/// The source rectangle the shape shows, `[x, y, w, h]` in source pixels: the central square for
/// a circle or square moved by the (clamped) pan, the whole source otherwise.
pub fn shape_bounds(src: (u32, u32), shape: Shape, pan: Pan) -> [f64; 4] {
    let (sw, sh) = fsize(src);
    if shape.central() {
        let side = sw.min(sh);
        let (dx, dy) = clamp_pan(src, shape, pan);
        [(sw - side) / 2.0 + dx, (sh - side) / 2.0 + dy, side, side]
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

/// The `Layout shape` mask path in source pixels: an ellipse inscribed in the central square moved
/// by the pan (circle), that square (4 corner vertices), or a Bézier rounded rectangle of the
/// whole source with the radius as % of the shorter side. `None` for `free` or a source without a
/// size. The pan is clamped ([`clamp_pan`]).
pub fn shape_path(src: (u32, u32), shape: Shape, pan: Pan) -> Option<MaskPath> {
    if !valid(src) {
        return None;
    }
    let [x, y, w, h] = shape_bounds(src, shape, pan);
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

/// Recognise a path [`shape_path`] made for this source: `Some((shape, pan))` for a circle or
/// square (possibly panned inside the source) or a rounded rectangle (radius read back, pan 0),
/// `None` for any other path (edited by hand, or moved partly outside the source), within 0.5 px.
pub fn shape_of_path(src: (u32, u32), path: &MaskPath) -> Option<(Shape, Pan)> {
    if !valid(src) {
        return None;
    }
    let tol = 0.5;
    let same = |s: Shape, pan: Pan| {
        shape_path(src, s, pan).is_some_and(|q| q.len() == path.len() && q.components().iter().zip(path.components()).all(|(a, b)| close(*a, b, tol)))
    };
    let first = path.vertices.first()?.p;
    for s in [Shape::Circle, Shape::Square] {
        // the translation from the centred shape's first vertex to the path's
        let Some(c0) = shape_path(src, s, NO_PAN).and_then(|q| q.vertices.first().map(|v| v.p)) else { continue };
        let raw = (first.x - c0.x, first.y - c0.y);
        let pan = clamp_pan(src, s, raw);
        if !close(pan.0, raw.0, tol) || !close(pan.1, raw.1, tol) {
            continue;
        }
        let r = |v: f64| {
            let v = (v * 1000.0).round() / 1000.0;
            if v == 0.0 { 0.0 } else { v }
        };
        let pan = (r(pan.0), r(pan.1));
        if same(s, pan) {
            return Some((s, pan));
        }
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
    same(s, NO_PAN).then_some((s, NO_PAN))
}

/// Box of the shape's bounds under the linear part `l` centred at `centre`: `[x, y, w, h]`.
fn box_at(src: (u32, u32), shape: Shape, l: [f64; 4], centre: (f64, f64)) -> [f64; 4] {
    // the box's size does not depend on the pan
    let [_, _, bw, bh] = shape_bounds(src, shape, NO_PAN);
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
fn shape_centre(frame: (u32, u32), src: (u32, u32), shape: Shape, pan: Pan, pose: &Pose) -> (f64, f64) {
    let (pos, anchor) = resolve(frame, src, pose);
    let [a, b, c, d] = linear(frame, src, pose);
    let [x, y, w, h] = shape_bounds(src, shape, pan);
    let (dx, dy) = (x + w / 2.0 - anchor.0, y + h / 2.0 - anchor.1);
    (pos.0 + a * dx + c * dy, pos.1 + b * dx + d * dy)
}

/// The visible box `[x, y, w, h]` (frame pixels) of a clip with this shape, pan and Motion: the
/// axis-aligned bounds of the transformed (panned) shape bounds. A source without a size gives an
/// empty box at the position.
pub fn visible_box(frame: (u32, u32), src: (u32, u32), shape: Shape, pan: Pan, pose: &Pose) -> [f64; 4] {
    if !valid(src) || !valid(frame) {
        let (pos, _) = resolve(frame, src, pose);
        return [fin(pos.0, 0.0), fin(pos.1, 0.0), 0.0, 0.0];
    }
    box_at(src, shape, linear(frame, src, pose), shape_centre(frame, src, shape, pan, pose))
}

/// The Motion position that keeps the visible box where it is when the pan changes from `from` to
/// `to` (both clamped): the position moves by `−L · (to − from)`, `L` the pose's rotation and
/// scale (with Scale to Frame), so the picture slides under a box that stays put. An auto (NaN)
/// position counts as the frame centre.
pub fn pan_position(frame: (u32, u32), src: (u32, u32), shape: Shape, pose: &Pose, from: Pan, to: Pan) -> (f64, f64) {
    let (pos, _) = resolve(frame, src, pose);
    let (a, b) = (clamp_pan(src, shape, from), clamp_pan(src, shape, to));
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let [la, lb, lc, ld] = linear(frame, src, pose);
    let p = (pos.0 - (la * dx + lc * dy), pos.1 - (lb * dx + ld * dy));
    if p.0.is_finite() && p.1.is_finite() { p } else { pos }
}

/// A displacement of the picture in the frame (frame pixels) as a displacement in source pixels:
/// the inverse of the pose's rotation and scale (with Scale to Frame). `(0, 0)` when the pose
/// squashes the picture to nothing. The Program monitor's Alt-drag uses it: the pan moves by the
/// opposite of the picture's displacement in source pixels.
pub fn source_delta(frame: (u32, u32), src: (u32, u32), pose: &Pose, d: (f64, f64)) -> (f64, f64) {
    let [a, b, c, e] = linear(frame, src, pose);
    let det = a * e - b * c;
    if !det.is_finite() || det.abs() < 1e-12 || !d.0.is_finite() || !d.1.is_finite() {
        return (0.0, 0.0);
    }
    // [a c; b e]⁻¹ = [e −c; −b a] / det
    let r = ((e * d.0 - c * d.1) / det, (-b * d.0 + a * d.1) / det);
    if r.0.is_finite() && r.1.is_finite() { r } else { (0.0, 0.0) }
}

/// Motion `(position, uniform scale %)` that puts the visible box's centre at `centre` with width
/// `width` (frame pixels), keeping the pose's anchor, rotation and Scale to Frame.
pub fn place_box(frame: (u32, u32), src: (u32, u32), shape: Shape, pan: Pan, centre: (f64, f64), width: f64, pose: &Pose) -> ((f64, f64), f64) {
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
    position_for(frame, src, shape, pan, centre, scale, pose)
}

/// The position that puts the (panned) shape centre at `centre` at this uniform scale.
fn position_for(frame: (u32, u32), src: (u32, u32), shape: Shape, pan: Pan, centre: (f64, f64), scale: f64, pose: &Pose) -> ((f64, f64), f64) {
    let p = Pose { scale: (scale, scale), position: (0.0, 0.0), ..*pose };
    // with position 0 the shape centre lands at L·(c − anchor): subtract it
    let off = shape_centre(frame, src, shape, pan, &p);
    ((centre.0 - off.0, centre.1 - off.1), scale)
}

/// Motion `(position, uniform scale %)` for `layout.place`: the visible box lands at `at` with
/// width `size_pct` % of the frame width and `margin_pct` % of the frame width from the edges it
/// touches, whatever the pan. `Full` fits the whole source (not the shape) in the frame, centred.
/// The pose's anchor, rotation and Scale to Frame are kept (its position and scale are what is
/// computed).
#[allow(clippy::too_many_arguments)]
pub fn place(frame: (u32, u32), src: (u32, u32), shape: Shape, pan: Pan, at: Place, size_pct: f64, margin_pct: f64, pose: &Pose) -> ((f64, f64), f64) {
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
        return position_for(frame, src, Shape::Free, NO_PAN, (fw / 2.0, fh / 2.0), scale, pose);
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
    place_box(frame, src, shape, pan, (cx, cy), w, pose)
}

/// Motion `(position, uniform scale %)` that gives the clip a new shape (or source, for a swap)
/// where `old_box` was: same width, and the edges the box's place touches stay where they are
/// (a bottom-right box keeps its right and bottom edges, a centred box its centre; a custom box
/// keeps its centre). A `Full` box stays full. So a circle given to a box placed bottom right is
/// still bottom right with the same margin, and doing it twice gives the first box back. `pan` is
/// the new shape's pan (the box does not depend on it).
pub fn refit(frame: (u32, u32), src: (u32, u32), shape: Shape, pan: Pan, old_box: [f64; 4], pose: &Pose) -> ((f64, f64), f64) {
    let (fw, fh) = fsize(frame);
    if !valid(frame) || !valid(src) || old_box.iter().any(|v| !v.is_finite()) {
        return ((fw / 2.0, fh / 2.0), 100.0);
    }
    let at = infer_place(frame, old_box).map(|(p, _)| p);
    if at == Some(Place::Full) {
        return place(frame, src, shape, pan, Place::Full, 100.0, 0.0, pose);
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
    place_box(frame, src, shape, pan, (cx, cy), w, pose)
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
        let (position, s) = place(HD, src, shape, NO_PAN, at, size, margin, &pose);
        visible_box(HD, src, shape, NO_PAN, &Pose { position, scale: (s, s), ..pose })
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
        assert_eq!(visible_box(HD, HD, Shape::Free, NO_PAN, &Pose::default()), [0.0, 0.0, 1920.0, 1080.0]);
        assert_eq!(visible_box(HD, HD, Shape::Circle, NO_PAN, &Pose::default()), [420.0, 0.0, 1080.0, 1080.0]);
        // Scale to Frame of a 4K source
        let b = visible_box(HD, (3840, 2160), Shape::Free, NO_PAN, &Pose { fit: true, ..Pose::default() });
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
                let (position, s) = place(HD, src, shape, NO_PAN, Place::Full, 25.0, 3.0, &Pose::default());
                let b = visible_box(HD, src, Shape::Free, NO_PAN, &Pose { position, scale: (s, s), ..Pose::default() });
                assert!(near(b[2], w, 1e-6) && near(b[3], h, 1e-6), "{src:?} {b:?}");
                assert!(near(b[0] + b[2] / 2.0, 960.0, 1e-6) && near(b[1] + b[3] / 2.0, 540.0, 1e-6));
                assert_eq!(nearest_place(HD, b, 3.0), Some(Place::Full));
                assert_eq!(infer_place(HD, b), Some((Place::Full, None)));
            }
        }
        // with Scale to Frame the scale is relative to the fitted size
        let (_, s) = place(HD, (3840, 2160), Shape::Free, NO_PAN, Place::Full, 25.0, 3.0, &Pose { fit: true, ..Pose::default() });
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
        let (position, s) = place_box(HD, HD, Shape::Circle, NO_PAN, c, b[2], &Pose::default());
        let nb = visible_box(HD, HD, Shape::Circle, NO_PAN, &Pose { position, scale: (s, s), ..Pose::default() });
        assert!(near(nb[0] + nb[2] / 2.0, c.0, 1e-6) && near(nb[1] + nb[3] / 2.0, c.1, 1e-6) && near(nb[2], b[2], 1e-6), "{nb:?}");
        assert!(near(nb[3], nb[2], 1e-9));
    }

    #[test]
    fn shape_paths() {
        assert!(shape_path(HD, Shape::Free, NO_PAN).is_none());
        let c = shape_path(HD, Shape::Circle, NO_PAN).unwrap();
        assert_eq!(c.len(), 4);
        assert!(c.closed);
        let (lo, hi) = c.bounds();
        assert!(near(lo.x, 420.0, 1e-9) && near(hi.x, 1500.0, 1e-9) && near(lo.y, 0.0, 1e-9) && near(hi.y, 1080.0, 1e-9));
        let sq = shape_path(HD, Shape::Square, NO_PAN).unwrap();
        assert!(sq.vertices.iter().all(|v| v.is_corner()));
        let r = shape_path(HD, Shape::Rounded { radius_pct: 12.0 }, NO_PAN).unwrap();
        assert_eq!(r.len(), 8);
        let (lo, hi) = r.bounds();
        assert!(near(lo.x, 0.0, 1e-9) && near(hi.x, 1920.0, 1e-9) && near(hi.y, 1080.0, 1e-9));
        for s in [Shape::Circle, Shape::Square, Shape::Rounded { radius_pct: 12.0 }, Shape::Rounded { radius_pct: 0.0 }, Shape::Rounded { radius_pct: 50.0 }] {
            for src in [HD, (1080, 1920), (500, 500)] {
                let p = shape_path(src, s, NO_PAN).unwrap();
                let back = shape_of_path(src, &p);
                if src.0 == src.1 && matches!(s, Shape::Rounded { radius_pct: 0.0 }) {
                    // a square source: the square is the whole source
                    assert_eq!(back, Some((Shape::Square, NO_PAN)));
                } else {
                    assert_eq!(back, Some((s, NO_PAN)), "{src:?}");
                }
            }
        }
        let mut edited = c.clone();
        edited.vertices[0].p.x += 40.0;
        assert_eq!(shape_of_path(HD, &edited), None);
    }

    /// Box `b` equals `c` within `tol` on every side.
    fn same_box(b: [f64; 4], c: [f64; 4], tol: f64) -> bool {
        b.iter().zip(c).all(|(x, y)| near(*x, y, tol))
    }

    const UHD: (u32, u32) = (3840, 2160);

    #[test]
    fn pan_clamps_inside_the_source() {
        // a 3840×2160 circle (side 2160) moves at most 840 px sideways and not at all vertically
        assert_eq!(clamp_pan(UHD, Shape::Circle, (-5000.0, 300.0)), (-840.0, 0.0));
        assert_eq!(clamp_pan(UHD, Shape::Square, (100.0, -1.0)), (100.0, 0.0));
        assert_eq!(clamp_pan(UHD, Shape::Circle, (f64::NAN, f64::INFINITY)), (0.0, 0.0));
        // portrait: vertical only
        assert_eq!(clamp_pan((1080, 1920), Shape::Circle, (50.0, 1e9)), (0.0, 420.0));
        // shapes that use the whole source never pan
        assert_eq!(clamp_pan(UHD, Shape::Rounded { radius_pct: 12.0 }, (100.0, 0.0)), NO_PAN);
        assert_eq!(clamp_pan(UHD, Shape::Free, (100.0, 0.0)), NO_PAN);
        assert_eq!(clamp_pan((0, 0), Shape::Circle, (100.0, 0.0)), NO_PAN);
        // the bounds stay inside the source
        let [x, y, w, h] = shape_bounds(UHD, Shape::Circle, (-5000.0, 0.0));
        assert_eq!([x, y, w, h], [0.0, 0.0, 2160.0, 2160.0]);
        let [x, _, w, _] = shape_bounds(UHD, Shape::Square, (5000.0, 0.0));
        assert_eq!(x + w, 3840.0);
    }

    #[test]
    fn panned_paths_round_trip() {
        for src in [UHD, HD, (1080, 1920), (500, 500)] {
            for s in [Shape::Circle, Shape::Square] {
                for pan in [NO_PAN, (-640.0, 0.0), (300.25, 0.0), (0.0, -200.0), (-1e6, 1e6)] {
                    let want = clamp_pan(src, s, pan);
                    let p = shape_path(src, s, pan).unwrap();
                    assert_eq!(shape_of_path(src, &p), Some((s, want)), "{src:?} {s:?} {pan:?}");
                }
            }
            // rounded: pan 0 only
            let r = Shape::Rounded { radius_pct: 12.0 };
            assert_eq!(shape_of_path(src, &shape_path(src, r, (90.0, 0.0)).unwrap()), Some((r, NO_PAN)), "{src:?}");
        }
        // a circle moved partly out of the source, or bent, is custom
        let mut out = shape_path(UHD, Shape::Circle, (-840.0, 0.0)).unwrap();
        for v in &mut out.vertices {
            v.p.x -= 10.0;
        }
        assert_eq!(shape_of_path(UHD, &out), None);
        let mut bent = shape_path(UHD, Shape::Circle, (-600.0, 0.0)).unwrap();
        bent.vertices[1].p.x += 3.0;
        assert_eq!(shape_of_path(UHD, &bent), None);
        // the circle centred on the left third of a 4K frame
        let p = shape_path(UHD, Shape::Circle, (1280.0 - 1920.0, 0.0)).unwrap();
        let (lo, hi) = p.bounds();
        assert!(near((lo.x + hi.x) / 2.0, 1280.0, 1e-9) && near(hi.x - lo.x, 2160.0, 1e-9));
    }

    #[test]
    fn pan_keeps_the_visible_box() {
        let poses = [
            Pose::default(),
            Pose { rotation: 90.0, ..Pose::default() },
            Pose { fit: true, rotation: 0.0, ..Pose::default() },
            Pose { fit: true, rotation: 90.0, anchor: (100.0, 700.0), ..Pose::default() },
            Pose { rotation: 33.0, scale: (40.0, 40.0), ..Pose::default() },
        ];
        for pose in poses {
            for s in [Shape::Circle, Shape::Square] {
                for at in [Place::BottomRight, Place::TopLeft, Place::Center, Place::Full] {
                    // placing at any pan lands the box at the same place
                    let (p0, sc) = place(HD, UHD, s, NO_PAN, at, 33.0, 3.0, &pose);
                    let base = Pose { position: p0, scale: (sc, sc), ..pose };
                    let b0 = visible_box(HD, UHD, s, NO_PAN, &base);
                    let pan = (-640.0, 0.0);
                    if at != Place::Full {
                        let (p1, sc1) = place(HD, UHD, s, pan, at, 33.0, 3.0, &pose);
                        let b1 = visible_box(HD, UHD, s, pan, &Pose { position: p1, scale: (sc1, sc1), ..pose });
                        assert!(same_box(b0, b1, 1e-6), "{pose:?} {s:?} {at:?} {b0:?} {b1:?}");
                        assert!(near(sc, sc1, 1e-9));
                    }
                    // changing the pan with pan_position keeps the box where it was
                    let moved = Pose { position: pan_position(HD, UHD, s, &base, NO_PAN, pan), ..base };
                    let b2 = visible_box(HD, UHD, s, pan, &moved);
                    assert!(same_box(b0, b2, 1e-6), "{pose:?} {s:?} {at:?} {b0:?} {b2:?}");
                    // and back
                    let back = Pose { position: pan_position(HD, UHD, s, &moved, pan, NO_PAN), ..moved };
                    assert!(near(back.position.0, base.position.0, 1e-6) && near(back.position.1, base.position.1, 1e-6));
                    // refit to the other shape keeps the box too
                    let other = if s == Shape::Circle { Shape::Square } else { Shape::Circle };
                    let (p3, sc3) = refit(HD, UHD, other, pan, b2, &moved);
                    let b3 = visible_box(HD, UHD, other, pan, &Pose { position: p3, scale: (sc3, sc3), ..moved });
                    if at != Place::Full {
                        assert!(same_box(b2, b3, 1e-6), "{pose:?} {s:?} {at:?} {b2:?} {b3:?}");
                    }
                }
            }
        }
        // unrotated, unscaled: the position moves by −pan exactly; rotated 90°, the pan turns
        let base = Pose { position: (960.0, 540.0), ..Pose::default() };
        let p = pan_position(HD, UHD, Shape::Circle, &base, NO_PAN, (-640.0, 0.0));
        assert!(near(p.0, 1600.0, 1e-9) && near(p.1, 540.0, 1e-9), "{p:?}");
        let rot = Pose { rotation: 90.0, ..base };
        let p = pan_position(HD, UHD, Shape::Circle, &rot, NO_PAN, (-640.0, 0.0));
        assert!(near(p.0, 960.0, 1e-9) && near(p.1, 1180.0, 1e-9), "{p:?}");
        // Scale to Frame (4K into HD: ×0.5) halves the shift
        let fit = Pose { fit: true, ..base };
        let p = pan_position(HD, UHD, Shape::Circle, &fit, NO_PAN, (-640.0, 0.0));
        assert!(near(p.0, 1280.0, 1e-9), "{p:?}");
        // source_delta inverts the pose: the picture moved by −L·pan is a pan of +pan
        for pose in [base, rot, fit, Pose { rotation: 33.0, scale: (40.0, 70.0), ..base }] {
            let p = pan_position(HD, UHD, Shape::Circle, &pose, NO_PAN, (-100.0, 0.0));
            let d = source_delta(HD, UHD, &pose, (p.0 - pose.position.0, p.1 - pose.position.1));
            assert!(near(d.0, 100.0, 1e-9) && near(d.1, 0.0, 1e-9), "{pose:?} {d:?}");
        }
        assert_eq!(source_delta(HD, UHD, &Pose { scale: (0.0, 0.0), ..base }, (5.0, 5.0)), (0.0, 0.0));
        // a pan that cannot move (rounded) changes nothing
        let p = pan_position(HD, UHD, Shape::Rounded { radius_pct: 12.0 }, &base, NO_PAN, (-640.0, 0.0));
        assert_eq!(p, (960.0, 540.0));
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
        let (position, s) = refit(HD, HD, Shape::Circle, NO_PAN, b, &Pose::default());
        let c = visible_box(HD, HD, Shape::Circle, NO_PAN, &Pose { position, scale: (s, s), ..Pose::default() });
        assert!(near(c[2], 480.0, 1e-6) && near(c[3], 480.0, 1e-6));
        assert!(near(c[0] + c[2], b[0] + b[2], 1e-6) && near(c[1] + c[3], b[1] + b[3], 1e-6), "{c:?} {b:?}");
        assert_eq!(nearest_place(HD, c, 3.0), Some(Place::BottomRight));
        // and back
        let (position, s) = refit(HD, HD, Shape::Free, NO_PAN, c, &Pose::default());
        let d = visible_box(HD, HD, Shape::Free, NO_PAN, &Pose { position, scale: (s, s), ..Pose::default() });
        assert!(d.iter().zip(b).all(|(x, y)| near(*x, y, 1e-9)), "{d:?} {b:?}");
        // a custom box keeps its centre; a full one stays full
        let custom = [300.0, 200.0, 480.0, 270.0];
        let (position, s) = refit(HD, HD, Shape::Square, NO_PAN, custom, &Pose::default());
        let e = visible_box(HD, HD, Shape::Square, NO_PAN, &Pose { position, scale: (s, s), ..Pose::default() });
        assert!(near(e[0] + e[2] / 2.0, 540.0, 1e-9) && near(e[1] + e[3] / 2.0, 335.0, 1e-9) && near(e[2], 480.0, 1e-9));
        let (_, s) = refit(HD, (1080, 1920), Shape::Circle, NO_PAN, [0.0, 0.0, 1920.0, 1080.0], &Pose::default());
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
                    let _ = shape_bounds(src, shape, NO_PAN);
                    let _ = shape_path(src, shape, NO_PAN);
                    for pose in &poses {
                        let b = visible_box(frame, src, shape, NO_PAN, pose);
                        let _ = nearest_place(frame, b, nan);
                        let _ = infer_place(frame, b);
                        for at in Place::ALL {
                            let (p, s) = place(frame, src, shape, NO_PAN, at, nan, -1e9, pose);
                            assert!(!s.is_nan() && s <= MAX_SCALE, "{s}");
                            let _ = (p, place(frame, src, shape, NO_PAN, at, 1e300, 1e300, pose));
                        }
                        let _ = place_box(frame, src, shape, NO_PAN, (nan, nan), nan, pose);
                        for pan in [(nan, f64::INFINITY), (-1e308, 1e308), (5.0, -5.0)] {
                            let cp = clamp_pan(src, shape, pan);
                            assert!(cp.0.is_finite() && cp.1.is_finite(), "{cp:?}");
                            let _ = shape_path(src, shape, pan);
                            let _ = visible_box(frame, src, shape, pan, pose);
                            let p = pan_position(frame, src, shape, pose, NO_PAN, pan);
                            let _ = (p, refit(frame, src, shape, pan, b, pose));
                        }
                    }
                }
            }
        }
        assert_eq!(visible_box(HD, (0, 0), Shape::Circle, NO_PAN, &Pose::default()), [960.0, 540.0, 0.0, 0.0]);
        assert_eq!(clamp_size(0.0), 1.0);
        assert_eq!(clamp_margin(99.0), 45.0);
        assert_eq!(clamp_radius(-3.0), 0.0);
    }
}
