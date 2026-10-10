//! Clip layouts: `layout.*` commands. See `openspec/changes/clip-layouts/design.md` §2 and
//! `docs/layouts.md`.
//!
//! A layout is an ordinary Motion edit (position and uniform scale) plus an Opacity mask named
//! [`LAYOUT_MASK`] that hides everything outside the shape. The geometry is
//! [`filmcraft_edit::layout`]; this module reads a clip's Motion at the playhead, asks it where the
//! clip goes, and writes the result the way `effects.setParam` does (a keyframe at the playhead
//! when the parameter is animated, else the static value), one undo step per command.

use std::collections::HashMap;

use filmcraft_edit::layout::{self as lay, NO_PAN, Pan, Place, Pose, Shape};
use filmcraft_geom::Vec2;
use filmcraft_project::{ClipId, EffectInstance, ItemKind, Mask, MaskPath, Param, ParamValue, Project, Sequence, TrackItem, TrackKind};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, clips_p, f64_p, has_seq, str_p, time_p};
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
        spec("layout.pan", "Pan Clip", r#"{"clips":[id]?,"dx":px?,"dy":px?,"merge":bool?,"begin":bool?}"#, has_layout_clips, pan, true),
        spec(
            "layout.set",
            "Transform Clip",
            r#"{"clips":[id]?,"position":[x,y]?,"scale":pct?,"scaleWidth":pct?,"merge":bool?,"begin":bool?}"#,
            has_layout_clips,
            set,
            true,
        ),
        spec("layout.inspect", "Inspect Layout", r#"{"clips":[id]?,"time":ticks?}"#, has_layout_clips, inspect, false),
        spec("layout.pick", "Pick Clip at Point", r#"{"x":px,"y":px,"time":ticks?}"#, has_seq, pick, false),
    ]
}

// ------------------------------------------------------------------ lookup

pub(crate) fn is_graphic(project: &Project, it: &TrackItem) -> bool {
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

pub(crate) fn frame_of(q: &Sequence) -> (u32, u32) {
    (q.settings.width, q.settings.height)
}

/// Media time of a clip at timeline time `t` (clamped into the clip, like `effects.setParam`).
pub(crate) fn media_time(it: &TrackItem, t: Tick) -> Tick {
    it.source_time_at(t.clamp(it.start, (it.end() - Tick(1)).max(it.start)))
}

/// The clip's Motion at media time `mt`, read like `filmcraft_render::motion_matrix`. With
/// `as_rendered`, a disabled Motion effect counts as the defaults (what the picture shows);
/// without, its values are read anyway (what a placing edit, which enables Motion, keeps).
pub fn pose_of(it: &TrackItem, mt: Tick, as_rendered: bool) -> Pose {
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

/// The clip's layout shape and pan: `Some((Free, 0))` without a layout mask, `None` for a mask
/// edited by hand.
fn shape_of(it: &TrackItem, src: (u32, u32), mt: Tick) -> Option<(Shape, Pan)> {
    match layout_mask_path(it, mt) {
        None => Some((Shape::Free, NO_PAN)),
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
    /// The shape's pan inside the source (0 for free, rounded or custom).
    pan: Pan,
    track: usize,
}

impl Clip {
    fn geometry_shape(&self) -> Shape {
        self.shape.unwrap_or(Shape::Free)
    }
    fn visible_box(&self, frame: (u32, u32)) -> [f64; 4] {
        lay::visible_box(frame, self.src, self.geometry_shape(), self.pan, &self.pose)
    }
}

fn read_clip(project: &Project, q: &Sequence, c: ClipId, t: Tick, as_rendered: bool, cmd: &str) -> Result<Clip> {
    let (tid, it) = q.find_item(c).ok_or_else(|| bad(cmd, format!("no clip {} in the active sequence", c.0)))?;
    let src = project.source_size(it.item).filter(|s| s.0 > 0 && s.1 > 0).ok_or_else(|| bad(cmd, format!("clip {} has no picture size", c.0)))?;
    let mt = media_time(it, t);
    let track = q.video_tracks.iter().position(|tr| tr.id == tid).unwrap_or(0);
    let sp = shape_of(it, src, mt);
    Ok(Clip { id: c, src, mt, pose: pose_of(it, mt, as_rendered), shape: sp.map(|x| x.0), pan: sp.map_or(NO_PAN, |x| x.1), track })
}

// ------------------------------------------------------------------ writing

/// The effect's parameter `id`, created from the definition's default when an older instance
/// lacks it (as `effects.setParam` does).
pub(crate) fn param_mut<'a>(e: &'a mut EffectInstance, id: &str) -> Option<&'a mut Param> {
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
pub(crate) fn set_layout_mask(it: &mut TrackItem, path: Option<MaskPath>, keep: Option<Mask>, cmd: &str) -> Result<()> {
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
        let (pos, scale) = lay::place(frame, cl.src, cl.geometry_shape(), cl.pan, at, size, margin, &cl.pose);
        plan.push((cl.id, cl.mt, pos, scale));
    }
    edit_clips(s, "Place Clip", CMD, |id, it| match plan.iter().find(|x| x.0 == id) {
        Some((_, mt, pos, scale)) => set_motion(it, *mt, *pos, *scale, CMD),
        None => Ok(()),
    })?;
    Ok(json!({"clips": clips.iter().map(|c| c.0).collect::<Vec<_>>(), "at": at.name(), "size": size, "margin": margin}))
}

/// `layout.set`: Motion position / scale / scale width of the clips at the playhead in one step
/// (keyframes when animated). `merge` folds consecutive calls into one undo step (the Program
/// monitor's move and scale drags, which change several parameters at once); `begin` starts a new
/// step.
fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layout.set";
    let position = match p.get("position") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(CMD, "`position` must be [x, y]"))?;
            let (x, y) = (a[0].as_f64().unwrap_or(f64::NAN), a[1].as_f64().unwrap_or(f64::NAN));
            if !x.is_finite() || !y.is_finite() {
                return Err(bad(CMD, "`position` must be finite numbers"));
            }
            Some((x.clamp(-1e6, 1e6), y.clamp(-1e6, 1e6)))
        }
    };
    let scale = f64_p(p, "scale").filter(|v| v.is_finite()).map(|v| v.clamp(0.0, lay::MAX_SCALE));
    let scale_width = f64_p(p, "scaleWidth").filter(|v| v.is_finite()).map(|v| v.clamp(0.0, lay::MAX_SCALE));
    if position.is_none() && scale.is_none() && scale_width.is_none() {
        return Err(bad(CMD, "give `position`, `scale` or `scaleWidth`"));
    }
    let clips = target_clips(s, p, CMD)?;
    let t = s.playhead();
    let merge = bool_p(p, "merge").unwrap_or(false).then(|| format!("layout.set:{}", clips.iter().map(|c| c.0.to_string()).collect::<Vec<_>>().join(",")));
    if bool_p(p, "begin").unwrap_or(false) {
        s.history.merge_key = None;
    }
    s.edit_sequence_as("Transform Clip", merge.as_deref(), |q, _, _| {
        for c in &clips {
            let (_, it) = q.find_item_mut(*c).ok_or_else(|| bad(CMD, "the clip is gone"))?;
            let mt = media_time(it, t);
            let m = it.effect_mut("motion").ok_or_else(|| bad(CMD, "the clip has no Motion effect"))?;
            m.enabled = true;
            if let Some((x, y)) = position {
                param_mut(m, "position").ok_or_else(|| bad(CMD, "Motion has no position"))?.set_at(mt, ParamValue::Vec2(Vec2::new(x, y)));
            }
            if let Some(v) = scale {
                param_mut(m, "scale").ok_or_else(|| bad(CMD, "Motion has no scale"))?.set_at(mt, ParamValue::Float(v));
            }
            if let Some(v) = scale_width {
                param_mut(m, "scale_width").ok_or_else(|| bad(CMD, "Motion has no scale width"))?.set_at(mt, ParamValue::Float(v));
            }
        }
        Ok(())
    })?;
    Ok(json!({"clips": clips.iter().map(|c| c.0).collect::<Vec<_>>()}))
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
        // the visible box keeps its width and the edges its place touches (its centre if custom);
        // the pan is kept (clamped to the new shape)
        let pan = lay::clamp_pan(cl.src, new_shape, cl.pan);
        let (pos, scale) = lay::refit(frame, cl.src, new_shape, pan, cl.visible_box(frame), &cl.pose);
        plan.push((cl.id, cl.mt, pos, scale, lay::shape_path(cl.src, new_shape, pan)));
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

/// `layout.swap`: the two clips exchange place, shape and pan, and over the time they overlap
/// they also exchange tracks, so the clip that was on top is now under the other (otherwise the
/// full-frame clip would still cover the small one). Each clip is split on its own (video) track
/// at the bounds of the overlap first, like a razor on that track alone: the pieces outside the
/// overlap stay where they are with their layout, and a right-hand piece gets a link group of its
/// own (linked audio is not split). The two pieces inside the overlap keep start, duration and
/// source in, get the layout of the other clip and move to the other clip's track. One undo step.
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
    let (Some((ta, ia)), Some((tb, ib))) = (q.find_item(a), q.find_item(b)) else { return Err(bad(CMD, "a clip to swap is not in the active sequence")) };
    if ta == tb {
        return Err(bad(CMD, "the two clips are on the same track: swap needs clips on two different video tracks"));
    }
    if q.track(ta).is_some_and(|tr| tr.locked) || q.track(tb).is_some_and(|tr| tr.locked) {
        return Err(bad(CMD, "a track of the two clips is locked"));
    }
    let range = TimeRange::from_bounds(ia.start.max(ib.start), ia.end().min(ib.end()).max(ia.start.max(ib.start)));
    if range.is_empty() {
        return Err(bad(CMD, "the two clips do not overlap in time: there is nothing to swap"));
    }
    // read both layouts at the playhead, or inside the overlap when the playhead is outside it
    let te = t.clamp(range.start, (range.end() - Tick(1)).max(range.start));
    let ca = read_clip(&s.project, q, a, te, false, CMD)?;
    let cb = read_clip(&s.project, q, b, te, false, CMD)?;
    let mask_of = |c: ClipId| q.find_item(c).and_then(|(_, it)| it.effect("opacity")).and_then(|o| o.masks.iter().find(|m| m.name == LAYOUT_MASK).cloned());
    let (ma, mb) = (mask_of(a), mask_of(b));
    // where `to` goes: `from`'s box, shape and pan, in `to`'s source pixels
    let target = |to: &Clip, from: &Clip, from_mask: Option<Mask>| {
        let pan = lay::clamp_pan(to.src, from.geometry_shape(), from.pan);
        let (pos, scale) = lay::refit(frame, to.src, from.geometry_shape(), pan, from.visible_box(frame), &to.pose);
        let path = match (from.shape, from_mask.as_ref()) {
            (_, None) => None,
            (Some(sh), Some(_)) => lay::shape_path(to.src, sh, pan),
            // a hand-edited mask: scaled from one source to the other
            (None, Some(m)) => match m.path.value_at(from.mt) {
                ParamValue::Path(path) => Some(path.transformed(&filmcraft_geom::Affine::scale(
                    f64::from(to.src.0) / f64::from(from.src.0.max(1)),
                    f64::from(to.src.1) / f64::from(from.src.1.max(1)),
                ))),
                _ => None,
            },
        };
        (to.mt, pos, scale, path, from_mask)
    };
    let (plan_a, plan_b) = (target(&ca, &cb, mb), target(&cb, &ca, ma));
    let (track_a, track_b) = (ca.track, cb.track);
    let (pa, pb) = s.edit_sequence("Swap Layouts", |q, ctx, st| {
        let mut links = HashMap::new();
        let mut pieces = Vec::new();
        for tid in [ta, tb] {
            let tr = q.track_mut(tid).ok_or_else(|| bad(CMD, "a track of the two clips is gone"))?;
            filmcraft_edit::split_track_at(tr, range.start, ctx, &mut links);
            filmcraft_edit::split_track_at(tr, range.end(), ctx, &mut links);
            let at = tr
                .items
                .iter()
                .position(|i| i.start == range.start && i.end() == range.end())
                .ok_or_else(|| bad(CMD, "could not cut the clip at the overlap"))?;
            let piece = tr.items.remove(at);
            // transitions of the piece alone (a fade in or out) go with it; one shared with a clip
            // that stays is dropped
            let own = |x: Option<ClipId>| x.is_none_or(|c| c == piece.id);
            let (moving, staying): (Vec<_>, Vec<_>) =
                std::mem::take(&mut tr.transitions).into_iter().partition(|x| (x.from == Some(piece.id) || x.to == Some(piece.id)) && own(x.from) && own(x.to));
            tr.transitions = staying;
            filmcraft_edit::remove_orphan_transitions(tr);
            pieces.push((piece, moving));
        }
        let (Some((mut pa, xa)), Some((mut pb, xb))) = (pieces.first().cloned(), pieces.get(1).cloned()) else { return Err(bad(CMD, "need two clips")) };
        for (piece, (mt, pos, scale, path, mask)) in [(&mut pa, &plan_a), (&mut pb, &plan_b)] {
            set_layout_mask(piece, path.clone(), mask.clone(), CMD)?;
            set_motion(piece, *mt, *pos, *scale, CMD)?;
        }
        let (ida, idb) = (pa.id, pb.id);
        for (tid, piece, moving) in [(tb, pa, xa), (ta, pb, xb)] {
            let tr = q.track_mut(tid).ok_or_else(|| bad(CMD, "a track of the two clips is gone"))?;
            tr.items.push(piece);
            tr.transitions.extend(moving);
            tr.sort();
        }
        // a selected clip stays selected as its swapped piece
        for c in st.selection.iter_mut() {
            if *c == a {
                *c = ida;
            } else if *c == b {
                *c = idb;
            }
        }
        Ok((ida, idb))
    })?;
    Ok(json!({
        "clips": [pa.0, pb.0],
        "moved": [[track_a, track_b], [track_b, track_a]],
        "range": [range.start.0, range.end().0],
    }))
}

/// `layout.pan`: set the pan of the clips' circle or square inside their source (source pixels,
/// clamped so the shape stays inside the source; a missing `dx` / `dy` keeps that component, a
/// value that is not a finite number counts as 0). The visible box stays where it is: the Motion
/// position moves by the opposite of the pan (rotated and scaled), so the picture slides under
/// the shape. `merge` / `begin` as in `layout.set` (an Alt-drag in the Program monitor is one
/// undo step).
fn pan(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layout.pan";
    let comp = |k: &str| p.get(k).filter(|v| !v.is_null()).map(|v| v.as_f64().filter(|x| x.is_finite()).unwrap_or(0.0));
    let (dx, dy) = (comp("dx"), comp("dy"));
    if dx.is_none() && dy.is_none() {
        return Err(bad(CMD, "give `dx` and / or `dy` (source pixels)"));
    }
    let clips = target_clips(s, p, CMD)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let frame = frame_of(q);
    let t = s.playhead();
    let mut plan = Vec::new();
    for c in &clips {
        let cl = read_clip(&s.project, q, *c, t, false, CMD)?;
        let Some(shape) = cl.shape.filter(|sh| matches!(sh, Shape::Circle | Shape::Square)) else {
            return Err(bad(CMD, format!("clip {} has no circle or square layout shape to pan inside (use layout.shape first)", c.0)));
        };
        let want = (dx.unwrap_or(cl.pan.0), dy.unwrap_or(cl.pan.1));
        let new = lay::clamp_pan(cl.src, shape, want);
        let pos = lay::pan_position(frame, cl.src, shape, &cl.pose, cl.pan, new);
        plan.push((cl.id, cl.mt, pos, lay::shape_path(cl.src, shape, new), new));
    }
    let merge = bool_p(p, "merge").unwrap_or(false).then(|| format!("layout.pan:{}", clips.iter().map(|c| c.0.to_string()).collect::<Vec<_>>().join(",")));
    if bool_p(p, "begin").unwrap_or(false) {
        s.history.merge_key = None;
    }
    s.edit_sequence_as("Pan Clip", merge.as_deref(), |q, _, _| {
        for (c, mt, pos, path, _) in &plan {
            let (_, it) = q.find_item_mut(*c).ok_or_else(|| bad(CMD, "the clip is gone"))?;
            set_layout_mask(it, path.clone(), None, CMD)?;
            let m = it.effect_mut("motion").ok_or_else(|| bad(CMD, "the clip has no Motion effect"))?;
            m.enabled = true;
            param_mut(m, "position").ok_or_else(|| bad(CMD, "Motion has no position"))?.set_at(*mt, ParamValue::Vec2(Vec2::new(pos.0, pos.1)));
        }
        Ok(())
    })?;
    Ok(json!({"clips": clips.iter().map(|c| c.0).collect::<Vec<_>>(), "pan": plan.iter().map(|x| [x.4.0, x.4.1]).collect::<Vec<_>>()}))
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
            "pan": [r(cl.pan.0), r(cl.pan.1)],
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
