//! Clip layouts: `layout.*` commands. See `openspec/changes/clip-layouts/design.md` §2 and
//! `docs/layouts.md`.
//!
//! A layout is an ordinary Motion edit (position and uniform scale) plus an Opacity mask named
//! [`LAYOUT_MASK`] that hides everything outside the shape. The geometry is
//! [`filmcraft_edit::layout`]; this module reads a clip's Motion at the playhead, asks it where the
//! clip goes, and writes the result the way `effects.setParam` does (a keyframe at the playhead
//! when the parameter is animated, else the static value), one undo step per command.

use filmcraft_edit::layout::{self as lay, Place, Pose, Shape};
use filmcraft_geom::Vec2;
use filmcraft_project::{ClipId, EffectInstance, ItemKind, Mask, MaskPath, Param, ParamValue, Project, Sequence, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, clips_p, f64_p, has_seq, str_p, time_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

/// The name of the Opacity mask that carries a clip's layout shape.
pub const LAYOUT_MASK: &str = "Layout shape";

fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    // no menu path: these take parameters; the UI adds its own Layout entries
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

/// The `layout.*` commands.
pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "layout.place",
            "Place Clip",
            r#"{"clips":[id]?,"at":"topLeft"|"topRight"|"bottomLeft"|"bottomRight"|"top"|"bottom"|"left"|"right"|"center"|"full","size":1..100=25,"margin":0..45=3}"#,
            has_layout_clips,
            place,
            true,
        ),
        spec("layout.shape", "Shape Clip", r#"{"clips":[id]?,"shape":"circle"|"rounded"|"square"|"free","radius":0..50=12}"#, has_layout_clips, shape, true),
        spec("layout.swap", "Swap Layouts", r#"{"clips":[id,id]?}"#, has_seq, swap, true),
        spec("layout.inspect", "Inspect Layout", r#"{"clips":[id]?,"time":ticks?}"#, has_layout_clips, inspect, false),
        spec("layout.pick", "Pick Clip at Point", r#"{"x":px,"y":px,"time":ticks?}"#, has_seq, pick, false),
    ]
}

// ------------------------------------------------------------------ lookup

fn is_graphic(project: &Project, it: &TrackItem) -> bool {
    project.item(it.item).is_some_and(|p| matches!(p.kind, ItemKind::Graphic { .. }))
}

/// Whether the clip is on a video track of the sequence.
fn is_video(q: &Sequence, c: ClipId) -> bool {
    q.find_item(c).and_then(|(t, _)| q.track(t)).is_some_and(|t| t.kind == TrackKind::Video)
}

/// Enabled: the selection (or the clips passed) has a video clip and no graphic clip.
fn has_layout_clips(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let q = s.active_sequence().ok_or("no sequence is open")?;
    let video: Vec<ClipId> = s.state.selection.iter().copied().filter(|c| is_video(q, *c)).collect();
    if video.is_empty() {
        return Err("select a video clip".into());
    }
    if video.iter().any(|c| q.find_item(*c).is_some_and(|(_, it)| is_graphic(&s.project, it))) {
        return Err("layouts apply to video clips, not graphics (move a graphic with its own layers)".into());
    }
    Ok(())
}

/// The clips a command acts on: `clips` / `clip`, else the selected video clips. Any clip that is
/// not a video clip of the active sequence, or is a graphic, is an error.
fn target_clips(s: &Session, p: &Value, cmd: &str) -> Result<Vec<ClipId>> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let explicit = p.get("clips").is_some() || p.get("clip").is_some();
    let mut clips = clips_p(s, p);
    if !explicit {
        clips.retain(|c| is_video(q, *c));
    }
    clips.dedup();
    if clips.is_empty() {
        return Err(bad(cmd, "select a video clip (or pass `clips`)"));
    }
    for c in &clips {
        let Some((_, it)) = q.find_item(*c) else { return Err(bad(cmd, format!("no clip {} in the active sequence", c.0))) };
        if !is_video(q, *c) {
            return Err(bad(cmd, format!("clip {} is not a video clip", c.0)));
        }
        if is_graphic(&s.project, it) {
            return Err(bad(cmd, format!("clip {} is a graphic: layouts apply to video clips", c.0)));
        }
    }
    Ok(clips)
}

fn frame_of(q: &Sequence) -> (u32, u32) {
    (q.settings.width, q.settings.height)
}

/// Media time of a clip at timeline time `t` (clamped into the clip, like `effects.setParam`).
fn media_time(it: &TrackItem, t: Tick) -> Tick {
    it.source_time_at(t.clamp(it.start, (it.end() - Tick(1)).max(it.start)))
}

/// The clip's Motion at media time `mt`, read like `filmcraft_render::motion_matrix`. With
/// `as_rendered`, a disabled Motion effect counts as the defaults (what the picture shows);
/// without, its values are read anyway (what a placing edit, which enables Motion, keeps).
fn pose_of(it: &TrackItem, mt: Tick, as_rendered: bool) -> Pose {
    let mut pose = Pose { fit: it.scale_to_frame, ..Pose::default() };
    let Some(m) = it.effect("motion").filter(|m| m.enabled || !as_rendered) else { return pose };
    if m.param("position").is_some() {
        let v = m.vec2_at("position", mt);
        pose.position = (v.x, v.y);
    }
    if m.param("anchor").is_some() {
        let v = m.vec2_at("anchor", mt);
        pose.anchor = (v.x, v.y);
    }
    let sc = m.f64_at("scale", mt);
    let uniform = m.param("uniform_scale").and_then(|p| p.value.as_bool()).unwrap_or(true);
    pose.scale = (if uniform { sc } else { m.f64_at("scale_width", mt) }, sc);
    pose.rotation = m.f64_at("rotation", mt);
    pose
}

/// The clip's layout mask path at `mt`, if it has one.
fn layout_mask_path(it: &TrackItem, mt: Tick) -> Option<MaskPath> {
    let m = it.effect("opacity")?.masks.iter().find(|m| m.name == LAYOUT_MASK)?;
    match m.path.value_at(mt) {
        ParamValue::Path(p) => Some(p),
        _ => None,
    }
}

/// The clip's layout shape: `Some(Free)` without a layout mask, `None` for a mask edited by hand.
fn shape_of(it: &TrackItem, src: (u32, u32), mt: Tick) -> Option<Shape> {
    match layout_mask_path(it, mt) {
        None => Some(Shape::Free),
        Some(p) => lay::shape_of_path(src, &p),
    }
}

/// What a layout command needs to know about one clip.
struct Clip {
    id: ClipId,
    src: (u32, u32),
    mt: Tick,
    pose: Pose,
    /// `None`: a hand-edited layout mask (treated as the whole source for the box).
    shape: Option<Shape>,
    track: usize,
}

impl Clip {
    fn geometry_shape(&self) -> Shape {
        self.shape.unwrap_or(Shape::Free)
    }
    fn visible_box(&self, frame: (u32, u32)) -> [f64; 4] {
        lay::visible_box(frame, self.src, self.geometry_shape(), &self.pose)
    }
}

fn read_clip(project: &Project, q: &Sequence, c: ClipId, t: Tick, as_rendered: bool, cmd: &str) -> Result<Clip> {
    let (tid, it) = q.find_item(c).ok_or_else(|| bad(cmd, format!("no clip {} in the active sequence", c.0)))?;
    let src = project.source_size(it.item).filter(|s| s.0 > 0 && s.1 > 0).ok_or_else(|| bad(cmd, format!("clip {} has no picture size", c.0)))?;
    let mt = media_time(it, t);
    let track = q.video_tracks.iter().position(|tr| tr.id == tid).unwrap_or(0);
    Ok(Clip { id: c, src, mt, pose: pose_of(it, mt, as_rendered), shape: shape_of(it, src, mt), track })
}

// ------------------------------------------------------------------ writing

/// The effect's parameter `id`, created from the definition's default when an older instance
/// lacks it (as `effects.setParam` does).
fn param_mut<'a>(e: &'a mut EffectInstance, id: &str) -> Option<&'a mut Param> {
    if !e.params.contains_key(id)
        && let Some(d) = e.def().and_then(|d| d.param(id))
    {
        e.params.insert(id.to_string(), Param::new(d.default.clone()));
    }
    e.params.get_mut(id)
}

/// Set Motion position and uniform scale at media time `mt` (keyframe when animated).
fn set_motion(it: &mut TrackItem, mt: Tick, position: (f64, f64), scale: f64, cmd: &str) -> Result<()> {
    let m = it.effect_mut("motion").ok_or_else(|| bad(cmd, "the clip has no Motion effect"))?;
    m.enabled = true;
    param_mut(m, "position").ok_or_else(|| bad(cmd, "Motion has no position"))?.set_at(mt, ParamValue::Vec2(Vec2::new(position.0, position.1)));
    param_mut(m, "scale").ok_or_else(|| bad(cmd, "Motion has no scale"))?.set_at(mt, ParamValue::Float(scale.clamp(0.0, lay::MAX_SCALE)));
    if let Some(u) = param_mut(m, "uniform_scale") {
        u.set_at(mt, ParamValue::Bool(true));
    }
    Ok(())
}

/// Replace (or with `None` remove) the clip's layout mask. `keep` is the mask object to reuse
/// (feather, mode…) when one is moved from another clip.
fn set_layout_mask(it: &mut TrackItem, path: Option<MaskPath>, keep: Option<Mask>, cmd: &str) -> Result<()> {
    let o = it.effect_mut("opacity").ok_or_else(|| bad(cmd, "the clip has no Opacity effect"))?;
    let at = o.masks.iter().position(|m| m.name == LAYOUT_MASK);
    let Some(path) = path else {
        o.masks.retain(|m| m.name != LAYOUT_MASK);
        return Ok(());
    };
    o.enabled = true;
    let mut mask = keep.or_else(|| at.and_then(|i| o.masks.get(i).cloned())).unwrap_or_else(|| {
        let mut m = Mask::new(LAYOUT_MASK, MaskPath::default());
        m.feather = Param::new(ParamValue::Float(0.0));
        m
    });
    mask.name = LAYOUT_MASK.to_string();
    mask.path = Param::new(ParamValue::Path(path));
    match at.and_then(|i| o.masks.get_mut(i)) {
        Some(slot) => *slot = mask,
        None => o.masks.push(mask),
    }
    Ok(())
}

/// Apply per-clip changes in one undo step.
fn edit_clips(s: &mut Session, label: &str, cmd: &'static str, f: impl Fn(ClipId, &mut TrackItem) -> Result<()>) -> Result<()> {
    s.edit_sequence(label, |q, _, _| {
        let ids: Vec<ClipId> = q.video_tracks.iter().flat_map(|t| t.items.iter().map(|i| i.id)).collect();
        for id in ids {
            let (_, it) = q.find_item_mut(id).ok_or_else(|| bad(cmd, "the clip is gone"))?;
            f(id, it)?;
        }
        Ok(())
    })
}

// ------------------------------------------------------------------ commands

fn place(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layout.place";
    let at_s =
        str_p(p, "at").ok_or_else(|| bad(CMD, "`at` is required (topLeft, topRight, bottomLeft, bottomRight, top, bottom, left, right, center, full)"))?;
    let at = Place::parse(at_s).ok_or_else(|| bad(CMD, format!("unknown place `{at_s}`")))?;
    let size = lay::clamp_size(f64_p(p, "size").unwrap_or(lay::DEFAULT_SIZE));
    let margin = lay::clamp_margin(f64_p(p, "margin").unwrap_or(lay::DEFAULT_MARGIN));
    let clips = target_clips(s, p, CMD)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let frame = frame_of(q);
    let t = s.playhead();
    let mut plan = Vec::new();
    for c in &clips {
        let cl = read_clip(&s.project, q, *c, t, false, CMD)?;
        let (pos, scale) = lay::place(frame, cl.src, cl.geometry_shape(), at, size, margin, &cl.pose);
        plan.push((cl.id, cl.mt, pos, scale));
    }
    edit_clips(s, "Place Clip", CMD, |id, it| match plan.iter().find(|x| x.0 == id) {
        Some((_, mt, pos, scale)) => set_motion(it, *mt, *pos, *scale, CMD),
        None => Ok(()),
    })?;
    Ok(json!({"clips": clips.iter().map(|c| c.0).collect::<Vec<_>>(), "at": at.name(), "size": size, "margin": margin}))
}

fn shape(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layout.shape";
    let name = str_p(p, "shape").ok_or_else(|| bad(CMD, "`shape` is required (circle, rounded, square, free)"))?;
    let radius = lay::clamp_radius(f64_p(p, "radius").unwrap_or(lay::DEFAULT_RADIUS));
    let new_shape = Shape::parse(name, radius).ok_or_else(|| bad(CMD, format!("unknown shape `{name}`")))?;
    let clips = target_clips(s, p, CMD)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let frame = frame_of(q);
    let t = s.playhead();
    let mut plan = Vec::new();
    for c in &clips {
        let cl = read_clip(&s.project, q, *c, t, false, CMD)?;
        // the visible box keeps its width and the edges its place touches (its centre if custom)
        let (pos, scale) = lay::refit(frame, cl.src, new_shape, cl.visible_box(frame), &cl.pose);
        plan.push((cl.id, cl.mt, pos, scale, lay::shape_path(cl.src, new_shape)));
    }
    edit_clips(s, "Shape Clip", CMD, |id, it| match plan.iter().find(|x| x.0 == id) {
        Some((_, mt, pos, scale, path)) => {
            set_layout_mask(it, path.clone(), None, CMD)?;
            set_motion(it, *mt, *pos, *scale, CMD)
        }
        None => Ok(()),
    })?;
    Ok(json!({"clips": clips.iter().map(|c| c.0).collect::<Vec<_>>(), "shape": new_shape.name(), "radius": new_shape.radius()}))
}

/// The video clips showing at timeline time `t`, top track first: enabled clips on enabled tracks
/// whose span covers `t`, graphics left out.
fn clips_at(project: &Project, q: &Sequence, t: Tick) -> Vec<ClipId> {
    q.video_tracks
        .iter()
        .rev()
        .filter(|tr| tr.enabled)
        .flat_map(|tr| tr.items.iter().filter(|it| it.enabled && it.start <= t && t < it.end() && !is_graphic(project, it)).map(|it| it.id))
        .collect()
}

fn swap(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layout.swap";
    let t = s.playhead();
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let frame = frame_of(q);
    let pair = if p.get("clips").is_some() {
        let c = target_clips(s, p, CMD)?;
        if c.len() != 2 || c.first() == c.get(1) {
            return Err(bad(CMD, "`clips` must name exactly two different video clips"));
        }
        c
    } else {
        let c = clips_at(&s.project, q, t);
        if c.len() < 2 {
            return Err(bad(CMD, "two video clips must be visible at the playhead (or pass `clips: [a, b]`)"));
        }
        c.into_iter().take(2).collect()
    };
    let (Some(a), Some(b)) = (pair.first().copied(), pair.get(1).copied()) else { return Err(bad(CMD, "need two clips")) };
    let ca = read_clip(&s.project, q, a, t, false, CMD)?;
    let cb = read_clip(&s.project, q, b, t, false, CMD)?;
    let mask_of = |c: ClipId| q.find_item(c).and_then(|(_, it)| it.effect("opacity")).and_then(|o| o.masks.iter().find(|m| m.name == LAYOUT_MASK).cloned());
    let (ma, mb) = (mask_of(a), mask_of(b));
    // where `to` goes: `from`'s box and shape, in `to`'s source pixels
    let target = |to: &Clip, from: &Clip, from_mask: Option<Mask>| {
        let (pos, scale) = lay::refit(frame, to.src, from.geometry_shape(), from.visible_box(frame), &to.pose);
        let path = match (from.shape, from_mask.as_ref()) {
            (_, None) => None,
            (Some(sh), Some(_)) => lay::shape_path(to.src, sh),
            // a hand-edited mask: scaled from one source to the other
            (None, Some(m)) => match m.path.value_at(from.mt) {
                ParamValue::Path(path) => Some(path.transformed(&filmcraft_geom::Affine::scale(
                    f64::from(to.src.0) / f64::from(from.src.0.max(1)),
                    f64::from(to.src.1) / f64::from(from.src.1.max(1)),
                ))),
                _ => None,
            },
        };
        (to.id, to.mt, pos, scale, path, from_mask)
    };
    let plan = [target(&ca, &cb, mb), target(&cb, &ca, ma)];
    edit_clips(s, "Swap Layouts", CMD, |id, it| match plan.iter().find(|x| x.0 == id) {
        Some((_, mt, pos, scale, path, mask)) => {
            set_layout_mask(it, path.clone(), mask.clone(), CMD)?;
            set_motion(it, *mt, *pos, *scale, CMD)
        }
        None => Ok(()),
    })?;
    Ok(json!({"clips": [a.0, b.0]}))
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layout.inspect";
    let clips = target_clips(s, p, CMD)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let frame = frame_of(q);
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let mut out = Vec::new();
    for c in clips {
        let cl = read_clip(&s.project, q, c, t, true, CMD)?;
        let b = cl.visible_box(frame);
        let fw = f64::from(frame.0.max(1));
        let (at, margin) = match lay::infer_place(frame, b) {
            Some((at, m)) => (at.name(), m),
            None => ("custom", None),
        };
        let r = |v: f64| (v * 1000.0).round() / 1000.0;
        out.push(json!({
            "clip": c.0,
            "at": at,
            "size": r(b[2] / fw * 100.0),
            "margin": margin.map(r),
            "shape": cl.shape.map(|sh| sh.name()).unwrap_or("custom"),
            "radius": cl.shape.and_then(|sh| sh.radius()),
            "box": b.map(r),
            "track": cl.track,
        }));
    }
    Ok(json!({"clips": out}))
}

fn pick(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layout.pick";
    let x = f64_p(p, "x").filter(|v| v.is_finite()).ok_or_else(|| bad(CMD, "`x` (frame pixels) is required"))?;
    let y = f64_p(p, "y").filter(|v| v.is_finite()).ok_or_else(|| bad(CMD, "`y` (frame pixels) is required"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let frame = frame_of(q);
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let mut hits = Vec::new();
    for c in clips_at(&s.project, q, t) {
        let Ok(cl) = read_clip(&s.project, q, c, t, true, CMD) else { continue };
        let [bx, by, bw, bh] = cl.visible_box(frame);
        if x >= bx && x <= bx + bw && y >= by && y <= by + bh {
            hits.push(c.0);
        }
    }
    Ok(json!({"clips": hits}))
}
