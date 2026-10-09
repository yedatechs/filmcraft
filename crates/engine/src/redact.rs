//! Tracked redaction: `redact.*` commands. See `openspec/changes/clip-layouts/design.md` §5 and
//! `docs/layouts.md`.
//!
//! A redaction is an ordinary effect instance (`mosaic`, `gaussian_blur`, or `mosaic` with one
//! block for `fill`) on a video clip with one rectangle mask named `Redaction N`, so Effect
//! Controls, undo, save and render need nothing new. `redact.add` creates both in one undo step
//! and, with `track`, tracks the mask forward from the playhead with [`crate::masks`] and then
//! backward: the backward run is queued in [`PENDING`] and started by [`poll`] when the forward
//! job has finished (with `wait` both run synchronously).

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};

use filmcraft_project::{ClipId, EffectInstance, Mask, MaskPath, ParamValue, TrackItem, TrackKind, TrackMethod};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, clip_p, has_seq, str_p, u64_p};
use crate::masks::MaskSel;
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

/// Mask names of redactions: `Redaction N`.
const PREFIX: &str = "Redaction ";
/// Mosaic blocks are about this fraction of the box (a box is ≈ 6 blocks across).
const BLOCKS_PER_BOX: f64 = 6.0;
/// Gaussian Blur blurriness of `style: blur`.
const BLUR: f64 = 60.0;
/// Mosaic's block-count range.
const MAX_BLOCKS: f64 = 4000.0;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_seq, run, journal }
}

/// The `redact.*` commands.
pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "redact.add",
            "Redact Area",
            r#"{"clip":id?,"rect":[x,y,w,h],"style":"mosaic"|"blur"|"fill"?="mosaic","track":bool?=true,"frames":n?,"wait":bool?}"#,
            add,
            true,
        ),
        spec("redact.list", "List Redactions", r#"{"clip":id?}"#, list, false),
        spec("redact.remove", "Remove Redaction", r#"{"clip":id?,"redaction":n}"#, remove, true),
    ]
}

// ------------------------------------------------------------------ lookup

/// The top-most enabled video clip under the playhead.
pub fn top_clip_at(s: &Session, t: Tick) -> Option<ClipId> {
    let q = s.active_sequence()?;
    q.video_tracks.iter().rev().find_map(|tr| tr.items.iter().find(|it| it.enabled && it.start <= t && t < it.end()).map(|it| it.id))
}

/// The addressed video clip: `clip`, else the top-most video clip under the playhead.
fn target(s: &Session, p: &Value, cmd: &str) -> Result<TrackItem> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let id = match clip_p(p, "clip") {
        Some(c) => c,
        None => top_clip_at(s, s.playhead()).ok_or_else(|| bad(cmd, "no video clip under the playhead (pass `clip`)"))?,
    };
    let (tid, it) = q.find_item(id).ok_or_else(|| bad(cmd, format!("no clip {}", id.0)))?;
    if q.track(tid).is_none_or(|t| t.kind != TrackKind::Video) {
        return Err(bad(cmd, "redactions go on video clips"));
    }
    Ok(it.clone())
}

/// Clip pixel size of a clip's source (the sequence frame when it has none).
fn source_size(s: &Session, it: &TrackItem) -> (f64, f64) {
    let seq = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
    let (w, h) = filmcraft_render::source_size(&s.project, it.item).unwrap_or(seq);
    (f64::from(w.max(1)), f64::from(h.max(1)))
}

/// `N` of a mask named `Redaction N`.
fn number(m: &Mask) -> Option<u64> {
    m.name.strip_prefix(PREFIX)?.trim().parse().ok()
}

/// The redactions of a clip: (effect index, mask index, N).
fn redactions(it: &TrackItem) -> Vec<(usize, usize, u64)> {
    let mut out = Vec::new();
    for (ei, e) in it.effects.iter().enumerate() {
        if !matches!(e.effect.as_str(), "mosaic" | "gaussian_blur") {
            continue;
        }
        for (mi, m) in e.masks.iter().enumerate() {
            if let Some(n) = number(m) {
                out.push((ei, mi, n));
            }
        }
    }
    out
}

fn style_of(e: &EffectInstance) -> &'static str {
    match e.effect.as_str() {
        "gaussian_blur" => "blur",
        _ if e.f64_at("horizontal", Tick::ZERO) <= 1.0 && e.f64_at("vertical", Tick::ZERO) <= 1.0 => "fill",
        _ => "mosaic",
    }
}

/// `rect: [x, y, w, h]` (clip pixels) clamped to the clip's picture.
fn rect_p(p: &Value, size: (f64, f64)) -> Result<(f64, f64, f64, f64)> {
    let a = p.get("rect").and_then(Value::as_array).ok_or_else(|| bad("redact.add", "need `rect`: [x, y, w, h] in clip pixels"))?;
    let n: Vec<f64> = a.iter().filter_map(Value::as_f64).filter(|v| v.is_finite()).collect();
    let [x, y, w, h] = n.as_slice() else { return Err(bad("redact.add", "`rect` is [x, y, w, h] (four finite numbers)")) };
    let (x0, x1) = if *w < 0.0 { (x + w, *x) } else { (*x, x + w) };
    let (y0, y1) = if *h < 0.0 { (y + h, *y) } else { (*y, y + h) };
    let (x0, x1) = (x0.clamp(0.0, size.0), x1.clamp(0.0, size.0));
    let (y0, y1) = (y0.clamp(0.0, size.1), y1.clamp(0.0, size.1));
    if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
        return Err(bad("redact.add", "the box is empty or outside the clip's picture"));
    }
    Ok((x0, y0, x1 - x0, y1 - y0))
}

fn effect_for(style: &str, size: (f64, f64), rect: (f64, f64, f64, f64)) -> Result<EffectInstance> {
    let id = if style == "blur" { "gaussian_blur" } else { "mosaic" };
    let def = filmcraft_project::find_effect(id).ok_or_else(|| EngineError::Other(format!("the `{id}` effect is missing")))?;
    let mut e = def.instance();
    let mut set = |k: &str, v: f64| {
        if let Some(prm) = e.params.get_mut(k) {
            prm.value = ParamValue::Float(v);
        }
    };
    match style {
        "blur" => set("blurriness", BLUR),
        "fill" => {
            set("horizontal", 1.0);
            set("vertical", 1.0);
        }
        _ => {
            // blocks ≈ box / 6: the frame holds frame / (box / 6) of them
            let blocks = |frame: f64, side: f64| (frame * BLOCKS_PER_BOX / side.max(1.0)).round().clamp(1.0, MAX_BLOCKS);
            set("horizontal", blocks(size.0, rect.2));
            set("vertical", blocks(size.1, rect.3));
        }
    }
    Ok(e)
}

// ------------------------------------------------------------------ commands

fn add(s: &mut Session, p: &Value) -> Result<Value> {
    let it = target(s, p, "redact.add")?;
    let clip = it.id;
    let size = source_size(s, &it);
    let rect = rect_p(p, size)?;
    let style = str_p(p, "style").unwrap_or("mosaic").to_ascii_lowercase();
    if !matches!(style.as_str(), "mosaic" | "blur" | "fill") {
        return Err(bad("redact.add", format!("style `{style}`: mosaic, blur or fill")));
    }
    // inserting an effect shifts the indices running tracking jobs write to
    if s.mask_jobs.iter().any(|j| j.target.clip == clip) || has_pending(s, clip) {
        return Err(bad("redact.add", "a mask on this clip is being tracked; wait for it or cancel it first"));
    }
    let inst = effect_for(&style, size, rect)?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let (x, y, w, h) = rect;
    let (ei, mi, name) = s.edit("Redact Area", move |pr, st| {
        let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let n = it.effects.iter().flat_map(|e| e.masks.iter()).filter_map(number).max().unwrap_or(0) + 1;
        let name = format!("{PREFIX}{n}");
        let mut mask = Mask::new(name.clone(), MaskPath::rect(x, y, x + w, y + h));
        mask.track_method = TrackMethod::Position;
        let mut inst = inst;
        inst.masks.push(mask);
        // standard effects go before the intrinsic ones (render order), as effects.apply does
        let ei = it.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(it.effects.len());
        it.effects.insert(ei, inst);
        st.selected_mask = Some(MaskSel { clip, effect: ei, mask: 0 });
        Ok((ei, 0usize, name))
    })?;
    let mut out = json!({"clip": clip.0, "effect": ei, "mask": mi, "name": name, "style": style, "rect": [x, y, w, h], "jobs": []});
    if bool_p(p, "track").unwrap_or(true) {
        let wait = bool_p(p, "wait").unwrap_or(false);
        let mut tp = json!({"clip": clip.0, "effect": ei, "mask": mi, "method": "position", "time": s.playhead().0, "wait": wait});
        if let Some(f) = u64_p(p, "frames") {
            tp["frames"] = json!(f);
        }
        let (jobs, err) = start_tracking(s, tp);
        out["jobs"] = json!(jobs);
        if let Some(e) = err {
            out["trackError"] = json!(e);
        }
    }
    Ok(out)
}

/// The clip's redactions `{redaction, name, effect, mask, style, rect, tracked, tracking}`; `rect`
/// is the mask's bounding box at the playhead (clip pixels).
fn list(s: &mut Session, p: &Value) -> Result<Value> {
    let it = target(s, p, "redact.list")?;
    let ph = s.playhead();
    let mt = it.source_time_at(ph.clamp(it.start, (it.end() - Tick(1)).max(it.start)));
    let mut out = Vec::new();
    for (ei, mi, n) in redactions(&it) {
        let (Some(e), Some(m)) = (it.effects.get(ei), it.effects.get(ei).and_then(|e| e.masks.get(mi))) else { continue };
        let (lo, hi) = m.path_at(mt).bounds();
        let running = s.mask_jobs.iter().any(|j| j.target == MaskSel { clip: it.id, effect: ei, mask: mi }) || pending_for(s, it.id, ei);
        out.push(json!({
            "redaction": n,
            "name": m.name,
            "effect": ei,
            "mask": mi,
            "style": style_of(e),
            "rect": [lo.x, lo.y, hi.x - lo.x, hi.y - lo.y],
            "tracked": m.path.keyframes.len() > 1,
            "tracking": running,
        }));
    }
    Ok(json!({"clip": it.id.0, "redactions": out}))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let it = target(s, p, "redact.remove")?;
    let clip = it.id;
    let n = u64_p(p, "redaction").ok_or_else(|| bad("redact.remove", "need `redaction` (its number N, see redact.list)"))?;
    let (ei, _, _) = redactions(&it).into_iter().find(|r| r.2 == n).ok_or_else(|| bad("redact.remove", format!("no Redaction {n} on clip {}", clip.0)))?;
    if s.mask_jobs.iter().any(|j| j.target.clip == clip && j.target.effect > ei) {
        return Err(bad("redact.remove", "another mask on this clip is being tracked; wait for it or cancel it first"));
    }
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    s.edit("Remove Redaction", move |pr, st| {
        let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        if ei >= it.effects.len() {
            return Err(bad("redact.remove", "the redaction is gone"));
        }
        it.effects.remove(ei);
        match st.selected_mask {
            Some(m) if m.clip == clip && m.effect == ei => st.selected_mask = None,
            Some(m) if m.clip == clip && m.effect > ei => st.selected_mask = Some(MaskSel { effect: m.effect - 1, ..m }),
            _ => {}
        }
        Ok(())
    })?;
    // stop tracking the removed mask
    for j in s.mask_jobs.iter().filter(|j| j.target.clip == clip && j.target.effect == ei) {
        if let Some(job) = s.jobs.iter().find(|x| x.id == j.job) {
            job.progress.cancel.store(true, Ordering::Relaxed);
        }
    }
    s.mask_jobs.retain(|j| !(j.target.clip == clip && j.target.effect == ei));
    drop_pending(s, clip, ei);
    Ok(json!({"clip": clip.0, "removed": n}))
}

// ------------------------------------------------------------------ tracking

/// A backward run waiting for the forward job (identified by its progress, which is unique
/// across sessions) to finish.
struct Pending {
    after: Weak<filmcraft_export::Progress>,
    params: Value,
}

/// Queued backward runs (all sessions; [`poll`] only takes those whose forward job is one of the
/// session's jobs).
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

fn pending() -> std::sync::MutexGuard<'static, Vec<Pending>> {
    PENDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn owned_by(s: &Session, pe: &Pending) -> bool {
    pe.after.upgrade().is_some_and(|a| s.jobs.iter().any(|j| Arc::ptr_eq(&j.progress, &a)))
}

fn same_target(v: &Value, clip: ClipId, effect: usize) -> bool {
    v["clip"].as_u64() == Some(clip.0) && v["effect"].as_u64() == Some(effect as u64)
}

fn pending_for(s: &Session, clip: ClipId, effect: usize) -> bool {
    pending().iter().any(|pe| same_target(&pe.params, clip, effect) && owned_by(s, pe))
}

fn has_pending(s: &Session, clip: ClipId) -> bool {
    pending().iter().any(|pe| pe.params["clip"].as_u64() == Some(clip.0) && owned_by(s, pe))
}

fn drop_pending(s: &Session, clip: ClipId, effect: usize) {
    pending().retain(|pe| pe.after.strong_count() > 0 && !(same_target(&pe.params, clip, effect) && owned_by(s, pe)));
}

/// Errors that only mean "nothing to track in this direction".
fn at_edge(e: &EngineError) -> bool {
    let m = e.to_string();
    m.contains("already at the clip's")
}

/// Start forward tracking, then backward (queued, or right away with `wait`). Returns the job ids
/// started and the first real error.
fn start_tracking(s: &mut Session, params: Value) -> (Vec<u64>, Option<String>) {
    let wait = params["wait"].as_bool().unwrap_or(false);
    let mut jobs = Vec::new();
    let mut err = None;
    let mut fwd = params.clone();
    fwd["direction"] = json!("forward");
    let mut back = params;
    back["direction"] = json!("backward");
    let forward_job = match crate::masks::track(s, &fwd) {
        Ok(v) => v["job"].as_u64(),
        Err(e) => {
            if !at_edge(&e) {
                err = Some(e.to_string());
            }
            None
        }
    };
    if let Some(id) = forward_job {
        jobs.push(id);
    }
    let progress = forward_job.and_then(|id| s.jobs.iter().find(|j| j.id == id)).map(|j| Arc::downgrade(&j.progress));
    match progress {
        Some(after) if !wait => pending().push(Pending { after, params: back }),
        _ => match crate::masks::track(s, &back) {
            Ok(v) => jobs.extend(v["job"].as_u64()),
            Err(e) if at_edge(&e) => {}
            Err(e) => {
                err.get_or_insert(e.to_string());
            }
        },
    }
    (jobs, err)
}

/// Start queued backward runs whose forward job has finished and been written (not when it was
/// cancelled). Called from [`Session::poll_persistence`] right after [`crate::masks::poll`].
pub fn poll(s: &mut Session) {
    let ready: Vec<Value> = {
        let mut q = pending();
        q.retain(|pe| pe.after.strong_count() > 0);
        let mut ready = Vec::new();
        let mut i = 0;
        while i < q.len() {
            let Some(pe) = q.get(i) else { break };
            let job = pe.after.upgrade().and_then(|a| s.jobs.iter().find(|j| Arc::ptr_eq(&j.progress, &a)).map(|j| (j.id, a)));
            let done = job.as_ref().is_some_and(|(id, a)| a.finished.load(Ordering::Relaxed) && !s.mask_jobs.iter().any(|m| m.job == *id));
            if done {
                let pe = q.remove(i);
                let cancelled = job.is_some_and(|(_, a)| a.cancel.load(Ordering::Relaxed));
                if !cancelled {
                    ready.push(pe.params);
                }
            } else {
                i += 1;
            }
        }
        ready
    };
    for params in ready {
        if let Err(e) = crate::masks::track(s, &params)
            && !at_edge(&e)
        {
            s.error_toast("redact.add", format!("backward tracking: {e}"));
        }
    }
}
