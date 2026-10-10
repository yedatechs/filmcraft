//! Scenes (`scenes.*`): a camera-and-screen arrangement attached to a span of the transcript, so
//! it follows the words when takes change. See `openspec/changes/clip-layouts/design.md` §4,
//! `specs/scenes/spec.md` and `docs/layouts.md` "Scenes".
//!
//! A [`Scene`] (on the sequence) says, per media item, where its clips go; a scene span (on the
//! media transcript, media time) says which scene is on for some words. Applying is a
//! **recompute**: for every video clip whose media item appears in any scene of the sequence, the
//! Motion `position` / `scale`, the Opacity `opacity` and the `Layout shape` mask path are rebuilt
//! as hold keyframes, one at the clip's start and one at every sequence time where a scene span
//! starts or ends inside it. The geometry is exactly that of `layout.place` / `layout.shape`
//! (`filmcraft_edit::layout::place` and `shape_path`, the clip's Motion read with
//! [`crate::layout::pose_of`]). Clips of media items no scene mentions are never touched.
//!
//! Time with no span uses the default scene (the first). A scene that does not mention a media
//! item leaves that item as the scene before it had it.
//!
//! The transcript and take edits that move clips call [`reapply_in`] at the end of their edit
//! closure, so the arrangement is recomputed in the same undo step.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use filmcraft_edit::layout::{self as lay, Place, Shape};
use filmcraft_edit::transcript::{self as tx, SeqWord};
use filmcraft_geom::Vec2;
use filmcraft_project::scene::{MAX_SCENE_NAME, MAX_SLOTS};
use filmcraft_project::{ClipId, Interpolation, ItemId, ItemKind, Keyframe, MaskPath, Param, ParamValue, Project, Scene, SceneSlot, Sequence, Transcript};
use filmcraft_time::{Tick, TimeRange};

use crate::commands::{CommandSpec, bad, has_seq, str_p, u64_p};
use crate::layout::{LAYOUT_MASK, frame_of, media_time, param_mut, pose_of, set_layout_mask};
use crate::transcript::sequence_words;
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

/// Most scenes a sequence keeps.
pub const MAX_SCENES: usize = 64;

fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    // no menu path: they take parameters; the UI adds Sequence ▸ Scenes… and the Text panel chips
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

/// The `scenes.*` commands.
pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("scenes.list", "List Scenes", "{}", has_seq, list, false),
        spec(
            "scenes.add",
            "Add Scene",
            r#"{"name":str?,"slots":[{"item":id,"hidden":bool?,"place":str?,"size":n?,"margin":n?,"shape":str?,"radius":n?}]?}"#,
            has_seq,
            add,
            true,
        ),
        spec(
            "scenes.update",
            "Update Scene",
            r#"{"scene":id|name,"name":str?,"slots":[{"item":id,"hidden":bool?,"place":str?,"size":n?,"margin":n?,"shape":str?,"radius":n?}]?}"#,
            has_scenes,
            update,
            true,
        ),
        spec("scenes.remove", "Remove Scene", r#"{"scene":id|name}"#, has_scenes, remove, true),
        spec("scenes.assign", "Assign Scene", r#"{"scene":id|name,"from":word,"to":word?}"#, has_scenes_and_words, assign, true),
        spec("scenes.clear", "Clear Scene", r#"{"from":word,"to":word?}"#, has_scenes_and_words, clear, true),
        spec("scenes.apply", "Apply Scenes", "{}", has_scenes, apply, true),
        spec("scenes.defaults", "Create Default Scenes", "{}", has_seq, defaults, true),
    ]
}

// ------------------------------------------------------------------ enabled

fn has_scenes(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.active_sequence().is_some_and(|q| !q.scenes.is_empty()) {
        Ok(())
    } else {
        Err("the sequence has no scenes (Sequence ▸ Scenes… ▸ Create Defaults)".into())
    }
}

fn has_scenes_and_words(s: &Session) -> std::result::Result<(), String> {
    has_scenes(s)?;
    if sequence_words(s).is_empty() { Err("the sequence has no transcript (Transcribe first)".into()) } else { Ok(()) }
}

// ------------------------------------------------------------------ the recompute

/// What a re-apply needs from the project besides the sequence: transcripts, picture sizes,
/// media roots and graphics, read before the edit so the sequence alone can be recomputed inside
/// an `edit_sequence` closure.
pub(crate) struct Ctx {
    transcripts: tx::Transcripts,
    sizes: BTreeMap<ItemId, (u32, u32)>,
    roots: BTreeMap<ItemId, ItemId>,
    graphics: BTreeSet<ItemId>,
}

/// The context for re-applying the scenes of sequence `seq`, or `None` when it has none (then
/// nothing is recomputed).
pub(crate) fn prepare(pr: &Project, seq: ItemId) -> Option<Ctx> {
    let q = pr.sequence(seq)?;
    if q.scenes.is_empty() {
        return None;
    }
    let mut c = Ctx { transcripts: pr.transcripts.clone(), sizes: BTreeMap::new(), roots: BTreeMap::new(), graphics: BTreeSet::new() };
    for (id, item) in &pr.items {
        if matches!(item.kind, ItemKind::Graphic { .. }) {
            c.graphics.insert(*id);
            continue;
        }
        if let Some((root, _, _)) = pr.resolve_media(*id) {
            c.roots.insert(*id, root);
        }
        if let Some(sz) = pr.source_size(*id).filter(|s| s.0 > 0 && s.1 > 0) {
            c.sizes.insert(*id, sz);
        }
    }
    Some(c)
}

/// The context for the active sequence (for the transcript and take edits).
pub(crate) fn prepare_active(s: &Session) -> Option<Ctx> {
    prepare(&s.project, s.state.active_sequence?)
}

/// Re-apply the scenes of sequence `seq`: pure on the project and idempotent (a second call
/// changes nothing). Does nothing when the sequence has no scenes.
pub fn reapply(pr: &mut Project, seq: ItemId) {
    let Some(c) = prepare(pr, seq) else { return };
    if let Some(q) = pr.sequence_mut(seq) {
        reapply_in(q, Some(&c));
    }
}

impl Ctx {
    /// Whether clip media `item` is (or resolves to) the slot's media item.
    fn same_media(&self, clip_item: ItemId, slot_item: ItemId) -> bool {
        clip_item == slot_item || self.roots.get(&clip_item).is_some_and(|r| *r == slot_item)
    }
}

/// A span of sequence time with a scene on (an index into `Sequence::scenes`).
#[derive(Clone, Copy, Debug)]
struct Segment {
    start: Tick,
    end: Tick,
    scene: usize,
}

/// The scene spans of the transcripts, mapped to sequence time through the clips that play them
/// (`filmcraft_edit::transcript::live_ranges`), sorted by start.
fn segments(q: &Sequence, transcripts: &tx::Transcripts) -> Vec<Segment> {
    let mut out = Vec::new();
    for (item, t) in transcripts {
        for span in &t.scenes {
            let Some(si) = q.scenes.iter().position(|sc| sc.id == span.scene) else { continue };
            for r in tx::live_ranges(q, *item, span.range) {
                out.push(Segment { start: r.start, end: r.end(), scene: si });
            }
        }
    }
    out.sort_by_key(|g| (g.start, g.end));
    out
}

/// The scene on at sequence time `t`: the last span covering it, else the default (index 0).
fn scene_index_at(segs: &[Segment], t: Tick) -> usize {
    segs.iter().rev().find(|g| g.start <= t && t < g.end).map(|g| g.scene).unwrap_or(0)
}

/// The slot that arranges media `item` at sequence time `t`: the slot of the scene on at `t`,
/// else of the scene on before it that mentions the item (walking back over the change points),
/// else of the default scene.
fn slot_at<'a>(q: &'a Sequence, c: &Ctx, segs: &[Segment], points: &[Tick], clip_item: ItemId, t: Tick) -> Option<&'a SceneSlot> {
    let of = |si: usize| q.scenes.get(si).and_then(|sc| sc.slots.iter().find(|sl| c.same_media(clip_item, sl.item)));
    if let Some(sl) = of(scene_index_at(segs, t)) {
        return Some(sl);
    }
    let before = points.partition_point(|p| *p < t);
    for p in points.get(..before).unwrap_or(&[]).iter().rev() {
        if let Some(sl) = of(scene_index_at(segs, *p)) {
            return Some(sl);
        }
    }
    of(0)
}

/// Replace a parameter's keyframes by hold keyframes (sorted, one per time).
fn hold(p: &mut Param, keys: &BTreeMap<Tick, ParamValue>) {
    p.keyframes = keys
        .iter()
        .map(|(t, v)| {
            let mut k = Keyframe::new(*t, v.clone());
            k.interp = Interpolation::Hold;
            k
        })
        .collect();
    if let Some(v) = keys.values().next() {
        p.value = v.clone();
    }
}

/// One clip's keyframe plan: per media time, position, scale, opacity and the layout mask path
/// (`None` = no shape at that time).
struct Plan {
    position: BTreeMap<Tick, ParamValue>,
    scale: BTreeMap<Tick, ParamValue>,
    opacity: BTreeMap<Tick, ParamValue>,
    path: BTreeMap<Tick, Option<MaskPath>>,
}

/// Recompute the scene keyframes of every scene-owned video clip of `q`. `c` is from
/// [`prepare`] / [`prepare_active`] (taken before the edit); `None` does nothing.
pub(crate) fn reapply_in(q: &mut Sequence, c: Option<&Ctx>) {
    let Some(c) = c else { return };
    if q.scenes.is_empty() {
        return;
    }
    let segs = segments(q, &c.transcripts);
    let mut points: Vec<Tick> = segs.iter().flat_map(|g| [g.start, g.end]).collect();
    points.sort();
    points.dedup();
    let frame = frame_of(q);
    let mut plans: Vec<(ClipId, Plan)> = Vec::new();
    for it in q.video_tracks.iter().flat_map(|t| t.items.iter()) {
        if c.graphics.contains(&it.item) {
            continue;
        }
        let owned = q.scenes.iter().any(|sc| sc.slots.iter().any(|sl| c.same_media(it.item, sl.item)));
        if !owned {
            continue;
        }
        let Some(src) = c.sizes.get(&it.item).copied() else { continue };
        let end = it.end();
        let mut times = vec![it.start];
        times.extend(points.iter().copied().filter(|p| *p > it.start && *p < end));
        let mut plan = Plan { position: BTreeMap::new(), scale: BTreeMap::new(), opacity: BTreeMap::new(), path: BTreeMap::new() };
        for t in times {
            let Some(sl) = slot_at(q, c, &segs, &points, it.item, t) else { continue };
            let mt = media_time(it, t);
            let at = Place::parse(&sl.place).unwrap_or(Place::Full);
            let shape = Shape::parse(&sl.shape, sl.radius).unwrap_or(Shape::Free);
            let pose = pose_of(it, mt, false);
            // scene-owned clips are never panned (a scene slot has no pan)
            let (pos, scale) = lay::place(frame, src, shape, lay::NO_PAN, at, lay::clamp_size(sl.size), lay::clamp_margin(sl.margin), &pose);
            plan.position.insert(mt, ParamValue::Vec2(Vec2::new(pos.0, pos.1)));
            plan.scale.insert(mt, ParamValue::Float(scale.clamp(0.0, lay::MAX_SCALE)));
            plan.opacity.insert(mt, ParamValue::Float(if sl.hidden { 0.0 } else { 100.0 }));
            plan.path.insert(mt, lay::shape_path(src, shape, lay::NO_PAN));
        }
        if !plan.position.is_empty() {
            plans.push((it.id, plan));
        }
    }
    for (id, plan) in plans {
        let Some((_, it)) = q.find_item_mut(id) else { continue };
        let src = c.sizes.get(&it.item).copied().unwrap_or((0, 0));
        if let Some(m) = it.effect_mut("motion") {
            m.enabled = true;
            if let Some(p) = param_mut(m, "position") {
                hold(p, &plan.position);
            }
            if let Some(p) = param_mut(m, "scale") {
                hold(p, &plan.scale);
            }
            if let Some(u) = param_mut(m, "uniform_scale") {
                *u = Param::new(ParamValue::Bool(true));
            }
        }
        if let Some(o) = it.effect_mut("opacity") {
            o.enabled = true;
            if let Some(p) = param_mut(o, "opacity") {
                hold(p, &plan.opacity);
            }
        }
        // the layout mask: none when no time has a shape; else a path per time, the whole source
        // (a plain rectangle) where the scene's shape is free
        let first = plan.path.values().find_map(|p| p.clone());
        let Some(first) = first else {
            let _ = set_layout_mask(it, None, None, "scenes.apply");
            continue;
        };
        let full = lay::shape_path(src, Shape::Rounded { radius_pct: 0.0 }, lay::NO_PAN).unwrap_or_else(|| first.clone());
        let keys: BTreeMap<Tick, ParamValue> = plan.path.iter().map(|(t, p)| (*t, ParamValue::Path(p.clone().unwrap_or_else(|| full.clone())))).collect();
        if set_layout_mask(it, Some(first), None, "scenes.apply").is_err() {
            continue;
        }
        if let Some(mask) = it.effect_mut("opacity").and_then(|o| o.masks.iter_mut().find(|m| m.name == LAYOUT_MASK)) {
            hold(&mut mask.path, &keys);
        }
    }
}

// ------------------------------------------------------------------ queries

/// The scene assigned at a sequence word (its media midpoint), if any.
pub fn word_scene(pr: &Project, w: &SeqWord) -> Option<u64> {
    let t = pr.transcripts.get(&w.item)?;
    let mw = t.words.get(w.index)?;
    t.scene_at(Tick(mw.start.0.saturating_add(mw.end.0.saturating_sub(mw.start.0) / 2)))
}

/// The name of the scene that arranges the clip's media item, if any: the scene on at the
/// playhead when it mentions the item, else the first scene that does. The Program monitor uses
/// it to refuse a hand drag on a scene-owned clip.
pub fn scene_owning(session: &Session, clip: ClipId) -> Option<String> {
    let q = session.active_sequence()?;
    let (_, it) = q.find_item(clip)?;
    let c = prepare_active(session)?;
    let mentions = |sc: &Scene| sc.slots.iter().any(|sl| c.same_media(it.item, sl.item));
    if !q.scenes.iter().any(mentions) {
        return None;
    }
    let segs = segments(q, &c.transcripts);
    let here = q.scenes.get(scene_index_at(&segs, session.playhead())).filter(|sc| mentions(sc));
    here.or_else(|| q.scenes.iter().find(|sc| mentions(sc))).map(|sc| sc.name.clone())
}

fn item_name(pr: &Project, id: ItemId) -> String {
    pr.item(id).map(|i| i.name.clone()).unwrap_or_else(|| format!("item {}", id.0))
}

fn slot_json(pr: &Project, sl: &SceneSlot) -> Value {
    json!({
        "item": sl.item.0,
        "name": item_name(pr, sl.item),
        "hidden": sl.hidden,
        "place": sl.place,
        "size": sl.size,
        "margin": sl.margin,
        "shape": sl.shape,
        "radius": sl.radius,
    })
}

/// `scenes.list` as a value (the Text panel reads it too).
pub fn scenes_json(s: &Session) -> Value {
    let Some(q) = s.active_sequence() else { return json!({"scenes": [], "spans": []}) };
    let pr = &s.project;
    let scenes: Vec<Value> = q
        .scenes
        .iter()
        .enumerate()
        .map(|(i, sc)| json!({"id": sc.id, "name": sc.name, "index": i, "default": i == 0, "slots": sc.slots.iter().map(|sl| slot_json(pr, sl)).collect::<Vec<_>>()}))
        .collect();
    let words = sequence_words(s);
    let mut spans = Vec::new();
    for (item, t) in &pr.transcripts {
        for span in &t.scenes {
            let Some(sc) = q.scenes.iter().find(|sc| sc.id == span.scene) else { continue };
            let idx: Vec<usize> = words.iter().enumerate().filter(|(_, w)| w.item == *item && in_span(t, w, span.range)).map(|(i, _)| i).collect();
            let live = tx::live_ranges(q, *item, span.range);
            spans.push(json!({
                "scene": span.scene,
                "name": sc.name,
                "item": item.0,
                "start": span.range.start.0,
                "end": filmcraft_project::scene::range_end(&span.range).0,
                "from": idx.first(),
                "to": idx.last(),
                "seqStart": live.first().map(|r| r.start.0),
            }));
        }
    }
    json!({"scenes": scenes, "spans": spans})
}

/// Whether the sequence word's media midpoint lies in `r`.
fn in_span(t: &Transcript, w: &SeqWord, r: TimeRange) -> bool {
    t.words.get(w.index).is_some_and(|mw| r.contains(Tick(mw.start.0.saturating_add(mw.end.0.saturating_sub(mw.start.0) / 2))))
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(scenes_json(s))
}

// ------------------------------------------------------------------ params

/// `scene`: an id, or a name (exact, then case-insensitive).
fn scene_p(s: &Session, p: &Value, cmd: &str) -> Result<u64> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let found = match p.get("scene") {
        Some(Value::String(n)) => {
            let n = n.trim();
            q.scenes.iter().find(|sc| sc.name == n).or_else(|| q.scenes.iter().find(|sc| sc.name.eq_ignore_ascii_case(n))).map(|sc| sc.id)
        }
        Some(v) => v.as_u64().and_then(|id| q.scenes.iter().find(|sc| sc.id == id)).map(|sc| sc.id),
        None => return Err(bad(cmd, "`scene` (id or name, see scenes.list) is required")),
    };
    found.ok_or_else(|| bad(cmd, "no such scene in the active sequence (see scenes.list)"))
}

fn name_p(p: &Value, cmd: &str) -> Result<Option<String>> {
    match p.get("name") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(n)) => {
            let n: String = n.trim().chars().take(MAX_SCENE_NAME).collect();
            if n.is_empty() { Err(bad(cmd, "`name` must not be empty")) } else { Ok(Some(n)) }
        }
        Some(_) => Err(bad(cmd, "`name` must be a string")),
    }
}

/// `slots`: `[{item, hidden?, place?, size?, margin?, shape?, radius?}]`.
fn slots_p(s: &Session, p: &Value, cmd: &str) -> Result<Option<Vec<SceneSlot>>> {
    let Some(v) = p.get("slots").filter(|v| !v.is_null()) else { return Ok(None) };
    let a = v.as_array().ok_or_else(|| bad(cmd, "`slots` must be an array"))?;
    if a.len() > MAX_SLOTS {
        return Err(bad(cmd, format!("at most {MAX_SLOTS} slots")));
    }
    let mut out: Vec<SceneSlot> = Vec::new();
    for (i, o) in a.iter().enumerate() {
        let id = u64_p(o, "item").map(ItemId).ok_or_else(|| bad(cmd, format!("slot {i}: `item` (media item id) is required")))?;
        let item = s.project.resolve_media(id).map(|(root, _, _)| root).ok_or_else(|| bad(cmd, format!("slot {i}: item {} is not a media item", id.0)))?;
        if out.iter().any(|x| x.item == item) {
            return Err(bad(cmd, format!("slot {i}: item {} has two slots", item.0)));
        }
        let place = match str_p(o, "place") {
            Some(n) => Place::parse(n).ok_or_else(|| bad(cmd, format!("slot {i}: unknown place `{n}`")))?,
            None => Place::Full,
        };
        let num = |k: &str| o.get(k).and_then(Value::as_f64);
        let radius = lay::clamp_radius(num("radius").unwrap_or(lay::DEFAULT_RADIUS));
        let shape = match str_p(o, "shape") {
            Some(n) => Shape::parse(n, radius).ok_or_else(|| bad(cmd, format!("slot {i}: unknown shape `{n}`")))?,
            None => Shape::Free,
        };
        out.push(SceneSlot {
            item,
            hidden: o.get("hidden").and_then(Value::as_bool).unwrap_or(false),
            place: place.name().to_string(),
            size: lay::clamp_size(num("size").unwrap_or(lay::DEFAULT_SIZE)),
            margin: lay::clamp_margin(num("margin").unwrap_or(lay::DEFAULT_MARGIN)),
            shape: shape.name().to_string(),
            radius,
        });
    }
    Ok(Some(out))
}

/// Media ranges of the sequence words `from..=to`, per transcript item: runs of consecutive media
/// words, each from its first word's start to the start of the media word after its last one (so
/// the pause after a paragraph keeps its scene), or the last word's end.
fn word_ranges(s: &Session, p: &Value, cmd: &str) -> Result<Vec<(ItemId, TimeRange)>> {
    let words = sequence_words(s);
    let from = u64_p(p, "from").ok_or_else(|| bad(cmd, "`from` (word index) is required"))?;
    let to = u64_p(p, "to").unwrap_or(from);
    let n = words.len() as u64;
    if from >= n || to >= n || to < from {
        return Err(bad(cmd, format!("word indices out of range (the transcript has {n} words; `to` ≥ `from`)")));
    }
    let sel = words.get(from as usize..=to as usize).unwrap_or(&[]);
    let mut runs: Vec<(ItemId, usize, usize)> = Vec::new();
    for w in sel {
        match runs.last_mut() {
            Some((item, _, last)) if *item == w.item && w.index == last.saturating_add(1) => *last = w.index,
            _ => runs.push((w.item, w.index, w.index)),
        }
    }
    let mut out = Vec::new();
    for (item, a, b) in runs {
        let Some(t) = s.project.transcripts.get(&item) else { continue };
        let (Some(wa), Some(wb)) = (t.words.get(a), t.words.get(b)) else { continue };
        let end = t.words.get(b.saturating_add(1)).map(|nx| nx.start).filter(|e| *e > wb.end).unwrap_or(wb.end);
        if end > wa.start {
            out.push((item, TimeRange::from_bounds(wa.start, end)));
        }
    }
    if out.is_empty() {
        return Err(bad(cmd, "those words have no media time"));
    }
    Ok(out)
}

// ------------------------------------------------------------------ edits

/// An undoable edit of the project for the active sequence, followed by a re-apply in the same
/// undo step.
fn edit_scenes<R>(s: &mut Session, label: &str, f: impl FnOnce(&mut Project, ItemId) -> Result<R>) -> Result<R> {
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    s.edit(label, move |pr, _| {
        let r = f(pr, seq)?;
        reapply(pr, seq);
        if let Some(q) = pr.sequence(seq) {
            q.check().map_err(EngineError::Other)?;
        }
        Ok(r)
    })
}

fn seq_mut(pr: &mut Project, seq: ItemId) -> Result<&mut Sequence> {
    pr.sequence_mut(seq).ok_or(EngineError::NoSequence)
}

fn add(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "scenes.add";
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if q.scenes.len() >= MAX_SCENES {
        return Err(bad(CMD, format!("a sequence keeps at most {MAX_SCENES} scenes")));
    }
    let name = name_p(p, CMD)?.unwrap_or_else(|| format!("Scene {}", q.scenes.len() + 1));
    let slots = slots_p(s, p, CMD)?.unwrap_or_default();
    let id = edit_scenes(s, "Add Scene", move |pr, seq| {
        let id = pr.alloc_id();
        let mut sc = Scene { id, name, slots };
        sc.normalize();
        seq_mut(pr, seq)?.scenes.push(sc);
        Ok(id)
    })?;
    Ok(json!({"scene": id}))
}

fn update(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "scenes.update";
    let id = scene_p(s, p, CMD)?;
    let name = name_p(p, CMD)?;
    let slots = slots_p(s, p, CMD)?;
    edit_scenes(s, "Update Scene", move |pr, seq| {
        let sc = seq_mut(pr, seq)?.scenes.iter_mut().find(|sc| sc.id == id).ok_or_else(|| bad(CMD, "the scene is gone"))?;
        if let Some(n) = name {
            sc.name = n;
        }
        if let Some(sl) = slots {
            sc.slots = sl;
        }
        sc.normalize();
        Ok(())
    })?;
    Ok(json!({"scene": id}))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let id = scene_p(s, p, "scenes.remove")?;
    edit_scenes(s, "Remove Scene", move |pr, seq| {
        seq_mut(pr, seq)?.scenes.retain(|sc| sc.id != id);
        let items: Vec<ItemId> = pr.transcripts.iter().filter(|(_, t)| t.scenes.iter().any(|sp| sp.scene == id)).map(|(i, _)| *i).collect();
        for i in items {
            if let Some(t) = pr.transcripts.get(&i) {
                let mut t = (**t).clone();
                t.scenes.retain(|sp| sp.scene != id);
                pr.transcripts.insert(i, std::sync::Arc::new(t));
            }
        }
        Ok(())
    })?;
    Ok(json!({"scene": id}))
}

/// Change the scene spans of the transcripts over `ranges`: assign `scene`, or clear with `None`.
fn set_spans(pr: &mut Project, ranges: &[(ItemId, TimeRange)], scene: Option<u64>) {
    for (item, r) in ranges {
        let Some(t) = pr.transcripts.get(item) else { continue };
        let mut t = (**t).clone();
        match scene {
            Some(id) => t.assign_scene(id, *r),
            None => t.clear_scenes(*r),
        }
        pr.transcripts.insert(*item, std::sync::Arc::new(t));
    }
}

fn assign(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "scenes.assign";
    let id = scene_p(s, p, CMD)?;
    let ranges = word_ranges(s, p, CMD)?;
    let n = ranges.len();
    edit_scenes(s, "Assign Scene", move |pr, _| {
        set_spans(pr, &ranges, Some(id));
        Ok(())
    })?;
    Ok(json!({"scene": id, "spans": n}))
}

fn clear(s: &mut Session, p: &Value) -> Result<Value> {
    let ranges = word_ranges(s, p, "scenes.clear")?;
    let n = ranges.len();
    edit_scenes(s, "Clear Scene", move |pr, _| {
        set_spans(pr, &ranges, None);
        Ok(())
    })?;
    Ok(json!({"cleared": n}))
}

fn apply(s: &mut Session, _: &Value) -> Result<Value> {
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let mut next = (*s.project).clone();
    reapply(&mut next, seq);
    // compared as JSON: "auto" points are NaN, which never equals itself
    let same = |a: Option<&Sequence>, b: Option<&Sequence>| serde_json::to_value(a).ok() == serde_json::to_value(b).ok();
    if same(next.sequence(seq), s.project.sequence(seq)) {
        return Ok(json!({"changed": false}));
    }
    edit_scenes(s, "Apply Scenes", |_, _| Ok(()))?;
    Ok(json!({"changed": true}))
}

/// The screen (A, lower) and face (B, upper) media of the default scenes: the first media items
/// of the two top-most video tracks that have a non-graphic clip.
fn default_items(s: &Session) -> Option<(ItemId, ItemId)> {
    let q = s.active_sequence()?;
    let mut found: Vec<ItemId> = Vec::new();
    for tr in q.video_tracks.iter().rev() {
        let Some(it) = tr.items.iter().find(|it| !crate::layout::is_graphic(&s.project, it)) else { continue };
        let root = s.project.resolve_media(it.item).map(|(r, _, _)| r).unwrap_or(it.item);
        if !found.contains(&root) {
            found.push(root);
        }
        if found.len() == 2 {
            break;
        }
    }
    match found.as_slice() {
        [b, a] => Some((*a, *b)),
        _ => None,
    }
}

fn slot(item: ItemId, place: Place, size: f64, margin: f64, shape: Shape, hidden: bool) -> SceneSlot {
    SceneSlot { item, hidden, place: place.name().into(), size, margin, shape: shape.name().into(), radius: shape.radius().unwrap_or(lay::DEFAULT_RADIUS) }
}

/// The four default scenes for screen `a` and face `b`.
pub fn default_scenes(a: ItemId, b: ItemId) -> Vec<(&'static str, Vec<SceneSlot>)> {
    let full = |i| slot(i, Place::Full, 100.0, 0.0, Shape::Free, false);
    let hidden = |i| slot(i, Place::Full, 100.0, 0.0, Shape::Free, true);
    vec![
        ("Screen with face", vec![full(a), slot(b, Place::BottomRight, lay::DEFAULT_SIZE, lay::DEFAULT_MARGIN, Shape::Circle, false)]),
        ("Face", vec![full(b), hidden(a)]),
        ("Screen", vec![full(a), hidden(b)]),
        ("Half and half", vec![slot(a, Place::Left, 50.0, 0.0, Shape::Free, false), slot(b, Place::Right, 50.0, 0.0, Shape::Free, false)]),
    ]
}

fn defaults(s: &mut Session, _: &Value) -> Result<Value> {
    const CMD: &str = "scenes.defaults";
    let (a, b) = default_items(s).ok_or_else(|| bad(CMD, "the two top-most video tracks need a clip each (screen below, face above)"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let todo: Vec<(&'static str, Vec<SceneSlot>)> = default_scenes(a, b).into_iter().filter(|(n, _)| !q.scenes.iter().any(|sc| sc.name == *n)).collect();
    if todo.is_empty() {
        return Err(bad(CMD, "the sequence already has the default scenes"));
    }
    if q.scenes.len().saturating_add(todo.len()) > MAX_SCENES {
        return Err(bad(CMD, format!("a sequence keeps at most {MAX_SCENES} scenes")));
    }
    let ids = edit_scenes(s, "Create Default Scenes", move |pr, seq| {
        let mut ids = Vec::new();
        for (name, slots) in todo {
            let id = pr.alloc_id();
            seq_mut(pr, seq)?.scenes.push(Scene { id, name: name.into(), slots });
            ids.push(id);
        }
        Ok(ids)
    })?;
    Ok(json!({"scenes": ids, "screen": a.0, "face": b.0}))
}
