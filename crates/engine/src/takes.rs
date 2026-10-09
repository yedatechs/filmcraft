//! Take editing (`takes.*`): lines the speaker recorded more than once, as take groups stored on
//! the media transcript (`Transcript::takes`), switched on the timeline.
//!
//! Nothing about *which* take is in the cut is stored. A take is **live** when the sequence's
//! audio clips play at least half of its media ([`filmcraft_edit::transcript::live_ranges`]).
//! Switching takes is an ordinary timeline edit: the live takes of the group are extracted and
//! the chosen take's media is put back where the first of them was
//! ([`filmcraft_edit::transcript::restore_media`]), so undo, redo, export and interchange need
//! nothing new, and the Text panel can never disagree with the timeline. Removed takes show as
//! crossed-out words through `transcript.cuts`.
//!
//! Design: `openspec/changes/take-editing/design.md` §4–5.

use std::sync::Arc;

use serde_json::{Value, json};

use filmcraft_edit::takes as tk;
use filmcraft_edit::transcript::{self as tx, SeqWord};
use filmcraft_edit::{self as edit, EditCtx};
use filmcraft_project::transcript::{Take, TakeGroup, TakeLabel};
use filmcraft_project::{ItemId, Project, Sequence, TrackId, Transcript};
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};

use crate::commands::{CommandSpec, bad, bool_p, f64_p, has_seq, str_p, u64_p};
use crate::transcript::sequence_words;
use crate::{EditorState, EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, shortcut: Option<&'static str>, params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &["Sequence", "Takes"], shortcut, params, enabled, run, journal }
}

/// A take is live when this much of its media plays in the sequence.
const LIVE_FRACTION: f32 = 0.5;
/// A pause this long ends a sentence (for `takes.preview`).
const SENTENCE_PAUSE: Tick = Tick(TICKS_PER_SECOND / 2);

// ------------------------------------------------------------------ lookup

/// Media items with transcripts heard in the active sequence (track order, no duplicates).
fn sequence_items(s: &Session) -> Vec<ItemId> {
    let mut out = Vec::new();
    if let Some(q) = s.active_sequence() {
        for it in q.audio_tracks.iter().flat_map(|t| t.items.iter()) {
            if let Some(m) = s.project.resolve_media(it.item).map(|(root, _, _)| root)
                && s.project.transcripts.contains_key(&m)
                && !out.contains(&m)
            {
                out.push(m);
            }
        }
    }
    out
}

/// The sequence has transcribed clips (live words or not: with every take crossed out there are
/// no live words, and that is exactly when Restore must stay enabled).
fn has_sequence_transcripts(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if sequence_items(s).is_empty() { Err("the sequence has no transcribed clips (Transcribe first)".into()) } else { Ok(()) }
}

fn has_groups(s: &Session) -> std::result::Result<(), String> {
    has_sequence_transcripts(s)?;
    if sequence_items(s).iter().any(|i| s.project.transcripts.get(i).is_some_and(|t| !t.takes.is_empty())) {
        Ok(())
    } else {
        Err("the sequence has no take groups (run Detect Takes first)".into())
    }
}

/// The group with this id among the sequence's transcripts: `(media item, group)`.
fn find_group(s: &Session, id: u64) -> Result<(ItemId, TakeGroup)> {
    for i in sequence_items(s) {
        if let Some(g) = s.project.transcripts.get(&i).and_then(|t| t.take_group(id)) {
            return Ok((i, g.clone()));
        }
    }
    Err(EngineError::Other(format!("no take group {id}")))
}

fn group_p(s: &Session, p: &Value, cmd: &str) -> Result<(ItemId, TakeGroup)> {
    let id = u64_p(p, "group").ok_or_else(|| bad(cmd, "`group` (take group id, see takes.list) is required"))?;
    find_group(s, id)
}

fn take_p(g: &TakeGroup, p: &Value, cmd: &str) -> Result<usize> {
    let i = u64_p(p, "take").ok_or_else(|| bad(cmd, "`take` (index in the group) is required"))? as usize;
    if i >= g.takes.len() {
        return Err(bad(cmd, format!("take {i} is out of range (group {} has {} takes)", g.id, g.takes.len())));
    }
    Ok(i)
}

/// Timeline ranges on which a take's media plays.
fn live_of(q: &Sequence, item: ItemId, take: &Take) -> Vec<TimeRange> {
    tx::live_ranges(q, item, take.range)
}

/// How much of a take's media the sequence plays (0..1): the live timeline ranges' total
/// duration over the take's duration (the restored material plays at speed 1).
fn live_fraction(q: &Sequence, item: ItemId, take: &Take) -> f32 {
    let dur = take.range.duration.0.max(1) as f64;
    let live: i64 = live_of(q, item, take).iter().fold(0i64, |acc, r| acc.saturating_add(r.duration.0.max(0)));
    (live as f64 / dur).clamp(0.0, 1.0) as f32
}

fn is_live(q: &Sequence, item: ItemId, take: &Take) -> bool {
    live_fraction(q, item, take) >= LIVE_FRACTION
}

/// The single live take of a group.
fn active_take(q: &Sequence, item: ItemId, g: &TakeGroup) -> Option<usize> {
    let live: Vec<usize> = g.takes.iter().enumerate().filter(|(_, t)| is_live(q, item, t)).map(|(i, _)| i).collect();
    if live.len() == 1 { live.first().copied() } else { None }
}

/// Audio track index where the group's media is heard: a clip playing part of the group's media,
/// else the track of a cut span holding part of it.
/// A clip that plays speech (the eligibility of `sequence_words`).
fn plays(it: &filmcraft_project::TrackItem, item: ItemId) -> bool {
    it.item == item && it.enabled && !it.reverse && it.frame_hold.is_none() && it.speed > 0.0
}

/// Media time just after a clip's last sample.
fn media_end(it: &filmcraft_project::TrackItem) -> Tick {
    let d = (it.duration.0 as f64 * it.speed).round();
    let d = if d.is_finite() { d.clamp(0.0, i64::MAX as f64) as i64 } else { 0 };
    Tick(it.source_in.0.saturating_add(d))
}

/// Where media of `item` goes by media order when none of it is live and no cut span holds it
/// (a take crossed out together with everything after it): right after the clip of the same
/// media that ends latest at or before `range`. Returns `(sequence time, audio track index)`.
fn media_order_anchor(q: &Sequence, item: ItemId, range: TimeRange) -> Option<(Tick, usize)> {
    let mut best: Option<(Tick, Tick, usize)> = None;
    for (ti, track) in q.audio_tracks.iter().enumerate() {
        for it in track.items.iter().filter(|it| plays(it, item)) {
            let me = media_end(it);
            if me <= range.start && best.is_none_or(|(b, _, _)| me > b) {
                best = Some((me, it.end(), ti));
            }
        }
    }
    best.map(|(_, at, ti)| (at, ti))
}

fn group_track(q: &Sequence, transcripts: &tx::Transcripts, words: &[SeqWord], item: ItemId, g: &TakeGroup) -> Option<usize> {
    let range = g.range()?;
    for (ti, track) in q.audio_tracks.iter().enumerate() {
        for it in track.items.iter().filter(|it| plays(it, item)) {
            if TimeRange::from_bounds(it.source_in, media_end(it).max(it.source_in)).overlaps(&range) {
                return Some(ti);
            }
        }
    }
    tx::cut_spans(q, transcripts, words)
        .iter()
        .find(|c| c.item == item && c.media.overlaps(&range))
        .map(|c| c.track)
        .or_else(|| media_order_anchor(q, item, range).map(|(_, ti)| ti))
}

/// Where a take goes back: the start of its live material, else the anchor of the cut span
/// holding it, else the group's anchor.
fn take_anchor(q: &Sequence, transcripts: &tx::Transcripts, words: &[SeqWord], item: ItemId, g: &TakeGroup, take: &Take) -> Option<Tick> {
    if let Some(r) = live_of(q, item, take).first() {
        return Some(r.start);
    }
    let spans = tx::cut_spans(q, transcripts, words);
    if let Some(c) = spans.iter().find(|c| c.item == item && c.media.overlaps(&take.range)) {
        return Some(c.at);
    }
    group_anchor(q, transcripts, words, item, g).or_else(|| media_order_anchor(q, item, take.range).map(|(at, _)| at))
}

/// Timeline position of a group: its first live material, else the anchor of a cut span holding
/// one of its takes.
fn group_anchor(q: &Sequence, transcripts: &tx::Transcripts, words: &[SeqWord], item: ItemId, g: &TakeGroup) -> Option<Tick> {
    let live = g.takes.iter().flat_map(|t| live_of(q, item, t)).map(|r| r.start).min();
    if live.is_some() {
        return live;
    }
    let range = g.range()?;
    tx::cut_spans(q, transcripts, words)
        .iter()
        .find(|c| c.item == item && c.media.overlaps(&range))
        .map(|c| c.at)
        .or_else(|| media_order_anchor(q, item, range).map(|(at, _)| at))
}

fn words_text(t: &Transcript, r: TimeRange) -> String {
    let w = t.words_within(r);
    t.words.get(w).map(|ws| ws.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default()
}

fn seconds(t: Tick) -> f64 {
    t.0 as f64 / TICKS_PER_SECOND as f64
}

fn group_json(s: &Session, q: &Sequence, words: &[SeqWord], item: ItemId, t: &Transcript, g: &TakeGroup) -> Value {
    let takes: Vec<Value> = g
        .takes
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let w = t.words_within(k.range);
            json!({
                "index": i, "start": k.range.start.0, "end": k.range.end().0, "seconds": seconds(k.range.duration),
                "text": words_text(t, k.range), "words": [w.start, w.end],
                "label": k.label.map(TakeLabel::name), "note": k.note, "live": is_live(q, item, k),
            })
        })
        .collect();
    let range = g.range();
    json!({
        "id": g.id, "item": item.0,
        "start": range.map(|r| r.start.0), "end": range.map(|r| r.end().0),
        "active": active_take(q, item, g), "redo": g.redo, "manual": g.manual, "note": g.note,
        "at": group_anchor(q, &s.project.transcripts, words, item, g).map(|t| t.0),
        "takes": takes,
    })
}

// ------------------------------------------------------------------ edits

/// Every unlocked track of the sequence (extract applies to these plus sync-locked ones).
fn unlocked(q: &Sequence) -> Vec<TrackId> {
    q.all_tracks().filter(|t| !t.locked).map(|t| t.id).collect()
}

/// Extract the live material of these media ranges, right to left. Returns the earliest start
/// removed.
fn extract_media(q: &mut Sequence, ctx: &mut EditCtx, item: ItemId, media: &[TimeRange]) -> Option<Tick> {
    let ranges = tx::merge_ranges(media.iter().flat_map(|m| tx::live_ranges(q, item, *m)).collect());
    let first = ranges.first().map(|r| r.start);
    let tracks = unlocked(q);
    for r in ranges.into_iter().rev() {
        if r.duration > Tick::ZERO {
            edit::extract(q, &tracks, r, ctx);
        }
    }
    first
}

/// Extract the live material of these takes. Returns the earliest start removed.
fn extract_takes(q: &mut Sequence, ctx: &mut EditCtx, item: ItemId, takes: &[&Take]) -> Option<Tick> {
    let media: Vec<TimeRange> = takes.iter().map(|t| t.range).collect();
    extract_media(q, ctx, item, &media)
}

/// Make `chosen` the only live take of the group (None = cross every take out). The whole group
/// (first take's start to last take's end, silences between takes included) is one slot in the
/// cut: all of it is extracted and the chosen take goes back at its start, so the other takes
/// read as one crossed-out stretch and nothing of the group is listed twice.
fn switch(q: &mut Sequence, ctx: &mut EditCtx, transcripts: &tx::Transcripts, item: ItemId, g: &TakeGroup, chosen: Option<usize>) -> Result<Option<TimeRange>> {
    let words = tx::sequence_words(q, transcripts);
    let track = group_track(q, transcripts, &words, item, g);
    let fallback = chosen.and_then(|i| g.takes.get(i)).and_then(|t| take_anchor(q, transcripts, &words, item, g, t));
    let whole: Vec<TimeRange> = g.range().into_iter().collect();
    let at = extract_media(q, ctx, item, &whole).or(fallback);
    let Some(i) = chosen else { return Ok(None) };
    let take = g.takes.get(i).ok_or_else(|| EngineError::Other(format!("take {i} is out of range")))?;
    let (Some(at), Some(track)) = (at, track) else {
        return Err(EngineError::Other("the take's media is not in the sequence, so there is nowhere to put it back".into()));
    };
    Ok(Some(tx::restore_media(q, item, take.range, at, track, ctx)?))
}

/// An undoable edit of the project *and* the active sequence with the edit-algebra context, for
/// commands that store take data and move the timeline in one step (`takes.detect`).
fn edit_project_and_sequence<R>(s: &mut Session, label: &str, f: impl FnOnce(&mut Project, ItemId, &mut EditCtx, &mut EditorState) -> Result<R>) -> Result<R> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let media = s.media.clone();
    s.edit(label, move |p: &mut Project, st: &mut EditorState| {
        let snapshot = Arc::new(p.clone());
        let snap2 = snapshot.clone();
        let durations = move |id: ItemId| -> Option<Tick> { crate::media_duration(&snapshot, &media, id) };
        let starts = move |id: ItemId| crate::media_start(&snap2, id);
        let min = p.sequence(seq_id).map(|q| q.settings.frame_rate.frame_duration()).unwrap_or(Tick(1));
        let mut next = p.next_id;
        let r = {
            let mut ctx = EditCtx { next_id: &mut next, media_duration: &durations, media_start: &starts, min_duration: min };
            f(p, seq_id, &mut ctx, st)?
        };
        p.next_id = p.next_id.max(next);
        if let Some(q) = p.sequence(seq_id) {
            q.check().map_err(EngineError::Other)?;
        }
        Ok(r)
    })
}

/// Replace a transcript's take groups (undoably), keeping everything else.
fn set_groups(pr: &mut Project, item: ItemId, groups: Vec<TakeGroup>) -> Result<()> {
    let t = pr.transcripts.get(&item).ok_or_else(|| EngineError::Other(format!("item {} has no transcript", item.0)))?;
    let mut t = (**t).clone();
    t.takes = groups;
    t.normalize_takes();
    pr.transcripts.insert(item, Arc::new(t));
    Ok(())
}

fn edit_group(s: &mut Session, label: &str, id: u64, f: impl FnOnce(&mut TakeGroup) -> Result<()>) -> Result<Value> {
    let (item, _) = find_group(s, id)?;
    s.edit(label, move |pr, _| {
        let t = pr.transcripts.get(&item).ok_or_else(|| EngineError::Other("the transcript is gone".into()))?;
        let mut t = (**t).clone();
        let g = t.take_group_mut(id).ok_or_else(|| EngineError::Other(format!("no take group {id}")))?;
        f(g)?;
        t.normalize_takes();
        pr.transcripts.insert(item, Arc::new(t));
        Ok(())
    })?;
    Ok(json!({"group": id}))
}

// ------------------------------------------------------------------ commands

fn detect(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> = match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).map(ItemId).filter_map(|i| s.project.resolve_media(i).map(|(r, _, _)| r)).collect(),
        None => sequence_items(s),
    };
    if items.is_empty() {
        return Err(bad("takes.detect", "nothing to analyse (the sequence has no transcribed clips)"));
    }
    let mut params = tk::DetectParams::default();
    if let Some(x) = f64_p(p, "sensitivity") {
        params.sensitivity = if x.is_finite() { x.clamp(0.0, 1.0) as f32 } else { params.sensitivity };
    }
    if let Some(x) = f64_p(p, "maxGapSeconds")
        && x.is_finite()
    {
        params.max_gap = Tick::from_seconds_f64(x.clamp(0.0, 3600.0));
    }
    let select = str_p(p, "select").unwrap_or("last");
    if !["last", "first", "none"].contains(&select) {
        return Err(bad("takes.detect", "`select` must be \"last\", \"first\" or \"none\""));
    }
    let select = select.to_string();
    let transcripts = s.project.transcripts.clone();
    let r = edit_project_and_sequence(s, "Detect Takes", move |pr, seq_id, ctx, _| {
        let mut report = Vec::new();
        let mut selected = 0usize;
        for item in items {
            let Some(t) = transcripts.get(&item) else { continue };
            let mut groups: Vec<TakeGroup> = t.takes.iter().filter(|g| g.manual).cloned().collect();
            // hand-made groups win: a detected group covering the same stretch is dropped
            let mut fresh: Vec<TakeGroup> =
                tk::detect(t, &params).into_iter().filter(|f| !groups.iter().any(|m| m.range().zip(f.range()).is_some_and(|(a, b)| a.overlaps(&b)))).collect();
            for g in &mut fresh {
                g.id = pr.alloc_id();
            }
            let new_ids: Vec<u64> = fresh.iter().map(|g| g.id).collect();
            groups.extend(fresh);
            set_groups(pr, item, groups)?;
            let stored = pr.transcripts.get(&item).cloned().ok_or_else(|| EngineError::Other("the transcript is gone".into()))?;
            if select != "none" {
                let latest = pr.transcripts.clone();
                let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
                for g in stored.takes.iter().filter(|g| new_ids.contains(&g.id)) {
                    let chosen = if select == "first" { 0 } else { g.takes.len().saturating_sub(1) };
                    if active_take(q, item, g) == Some(chosen) {
                        continue;
                    }
                    let words = tx::sequence_words(q, &latest);
                    if group_track(q, &latest, &words, item, g).is_none() {
                        continue;
                    }
                    if switch(q, ctx, &latest, item, g, Some(chosen))?.is_some() {
                        selected += 1;
                    }
                }
            }
            report.push(json!({"item": item.0, "groups": new_ids.len(), "ids": new_ids}));
        }
        Ok(json!({"items": report, "selected": selected}))
    })?;
    Ok(r)
}

/// The sequence's take groups as `takes.list` reports them (sorted by timeline position), for
/// hosts that draw them every frame: `id`, `item`, media `start`/`end`, `active`, `redo`,
/// `manual`, `note`, `at`, and `takes` (`index`, `start`, `end`, `seconds`, `text`, `words`,
/// `label`, `note`, `live`). Filters: a label every kept group has on some take, the redo flag.
pub fn groups_json(s: &Session, label: Option<TakeLabel>, redo: Option<bool>) -> Vec<Value> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let words = sequence_words(s);
    let mut groups = Vec::new();
    for item in sequence_items(s) {
        let Some(t) = s.project.transcripts.get(&item) else { continue };
        for g in &t.takes {
            if redo.is_some_and(|r| g.redo != r) || label.is_some_and(|l| !g.takes.iter().any(|k| k.label == Some(l))) {
                continue;
            }
            groups.push(group_json(s, q, &words, item, t, g));
        }
    }
    groups.sort_by_key(|g| (g["at"].as_i64().unwrap_or(i64::MAX), g["start"].as_i64().unwrap_or(0)));
    groups
}

fn list(s: &mut Session, p: &Value) -> Result<Value> {
    let label = match str_p(p, "label") {
        Some(l) => Some(TakeLabel::from_name(l).ok_or_else(|| bad("takes.list", format!("unknown label `{l}`")))?),
        None => None,
    };
    let groups = groups_json(s, label, bool_p(p, "redo"));
    Ok(json!({"groups": groups, "labels": TakeLabel::ALL.iter().map(|l| l.name()).collect::<Vec<_>>()}))
}

fn select_impl(s: &mut Session, label: &str, id: u64, item: ItemId, g: TakeGroup, chosen: Option<usize>) -> Result<Value> {
    let transcripts = s.project.transcripts.clone();
    let r = s.edit_sequence(label, |q, ctx, _| switch(q, ctx, &transcripts, item, &g, chosen))?;
    if let Some(r) = r {
        s.set_playhead(r.start);
    }
    Ok(json!({"group": id, "take": chosen, "start": r.map(|r| r.start.0), "end": r.map(|r| r.end().0)}))
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let (item, g) = group_p(s, p, "takes.select")?;
    let i = take_p(&g, p, "takes.select")?;
    select_impl(s, "Switch Take", g.id, item, g, Some(i))
}

/// The group at the playhead (a live take containing it), else from `group`.
fn group_here(s: &Session, p: &Value, cmd: &str) -> Result<(ItemId, TakeGroup)> {
    if p.get("group").is_some() {
        return group_p(s, p, cmd);
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    // the playhead sits on frames; a take may start inside one
    let ph = s.playhead();
    let rate = s.sequence_rate();
    let on = |r: &TimeRange| rate.snap(r.start) <= ph && ph < r.end();
    for item in sequence_items(s) {
        let Some(t) = s.project.transcripts.get(&item) else { continue };
        for g in &t.takes {
            if g.takes.iter().any(|k| live_of(q, item, k).iter().any(on)) {
                return Ok((item, g.clone()));
            }
        }
    }
    Err(bad(cmd, "no take at the playhead (move onto a take, or pass `group`)"))
}

fn step(s: &mut Session, p: &Value, delta: i64, cmd: &str, label: &str) -> Result<Value> {
    let (item, g) = group_here(s, p, cmd)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let n = g.takes.len() as i64;
    if n == 0 {
        return Err(EngineError::Other("the group has no takes".into()));
    }
    let cur = active_take(q, item, &g).map(|i| i as i64).unwrap_or(if delta > 0 { -1 } else { n });
    let next = (cur + delta).rem_euclid(n) as usize;
    select_impl(s, label, g.id, item, g, Some(next))
}

fn next(s: &mut Session, p: &Value) -> Result<Value> {
    step(s, p, 1, "takes.next", "Next Take")
}

fn previous(s: &mut Session, p: &Value) -> Result<Value> {
    step(s, p, -1, "takes.previous", "Previous Take")
}

fn cross(s: &mut Session, p: &Value) -> Result<Value> {
    let (item, g) = group_p(s, p, "takes.cross")?;
    let i = take_p(&g, p, "takes.cross")?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let take = g.takes.get(i).cloned().ok_or_else(|| bad("takes.cross", "take out of range"))?;
    if live_of(q, item, &take).is_empty() {
        return Err(EngineError::Other("that take is already crossed out".into()));
    }
    let removed = s.edit_sequence("Cross Out Take", |q, ctx, _| Ok(extract_takes(q, ctx, item, &[&take])))?;
    if let Some(at) = removed {
        s.set_playhead(at);
    }
    Ok(json!({"group": g.id, "take": i, "at": removed.map(|t| t.0)}))
}

fn restore(s: &mut Session, p: &Value) -> Result<Value> {
    let (item, g) = group_p(s, p, "takes.restore")?;
    let i = take_p(&g, p, "takes.restore")?;
    let take = g.takes.get(i).cloned().ok_or_else(|| bad("takes.restore", "take out of range"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if live_fraction(q, item, &take) >= 0.999 {
        return Err(EngineError::Other("that take is already in the cut".into()));
    }
    let transcripts = s.project.transcripts.clone();
    let words = sequence_words(s);
    let at = take_anchor(q, &transcripts, &words, item, &g, &take)
        .ok_or_else(|| EngineError::Other("the take's media is not in the sequence, so there is nowhere to put it back".into()))?;
    let track = group_track(q, &transcripts, &words, item, &g).ok_or_else(|| EngineError::Other("the take's media is not on any audio track".into()))?;
    let r = s.edit_sequence("Restore Take", |q, ctx, _| {
        // what is already live of this take goes first, so the whole take comes back in one piece
        extract_takes(q, ctx, item, &[&take]);
        Ok(tx::restore_media(q, item, take.range, at, track, ctx)?)
    })?;
    s.set_playhead(r.start);
    Ok(json!({"group": g.id, "take": i, "start": r.start.0, "end": r.end().0}))
}

fn set_label(s: &mut Session, p: &Value) -> Result<Value> {
    let (_, g) = group_p(s, p, "takes.label")?;
    let i = take_p(&g, p, "takes.label")?;
    let label: Option<Option<TakeLabel>> = match p.get("label") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(l)) if l.trim().is_empty() => Some(None),
        Some(Value::String(l)) => Some(Some(
            TakeLabel::from_name(l)
                .ok_or_else(|| bad("takes.label", format!("unknown label `{l}` (one of {})", TakeLabel::ALL.map(TakeLabel::name).join(", "))))?,
        )),
        Some(_) => return Err(bad("takes.label", "`label` must be a string or null")),
    };
    let note = p.get("note").map(|v| match v {
        Value::String(n) => Ok(n.clone()),
        Value::Null => Ok(String::new()),
        _ => Err(bad("takes.label", "`note` must be a string")),
    });
    let note = note.transpose()?;
    if label.is_none() && note.is_none() {
        return Err(bad("takes.label", "pass `label` and/or `note`"));
    }
    edit_group(s, "Label Take", g.id, move |g| {
        let t = g.takes.get_mut(i).ok_or_else(|| EngineError::Other("take out of range".into()))?;
        if let Some(l) = label {
            t.label = l;
        }
        if let Some(n) = note {
            t.note = n;
        }
        g.manual = true;
        Ok(())
    })
}

fn set_redo(s: &mut Session, p: &Value) -> Result<Value> {
    let (_, g) = group_p(s, p, "takes.redo")?;
    let redo = bool_p(p, "redo").unwrap_or(true);
    let note = str_p(p, "note").map(str::to_string);
    edit_group(s, if redo { "Mark Take for Re-record" } else { "Clear Re-record" }, g.id, move |g| {
        g.redo = redo;
        if let Some(n) = note {
            g.note = n;
        }
        Ok(())
    })
}

fn merge(s: &mut Session, p: &Value) -> Result<Value> {
    let ids: Vec<u64> = p.get("groups").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    if ids.len() < 2 {
        return Err(bad("takes.merge", "`groups` needs at least two take group ids"));
    }
    let mut item = None;
    for id in &ids {
        let (i, _) = find_group(s, *id)?;
        if item.is_some_and(|x| x != i) {
            return Err(bad("takes.merge", "take groups of different clips cannot be merged"));
        }
        item = Some(i);
    }
    let item = item.ok_or_else(|| bad("takes.merge", "no groups"))?;
    let kept = s.edit("Merge Take Groups", move |pr, _| {
        let t = pr.transcripts.get(&item).ok_or_else(|| EngineError::Other("the transcript is gone".into()))?;
        let mut t = (**t).clone();
        let kept = tk::merge(&mut t.takes, &ids).ok_or_else(|| EngineError::Other("nothing to merge".into()))?;
        t.normalize_takes();
        pr.transcripts.insert(item, Arc::new(t));
        Ok(kept)
    })?;
    Ok(json!({"group": kept}))
}

fn split(s: &mut Session, p: &Value) -> Result<Value> {
    let (item, g) = group_p(s, p, "takes.split")?;
    let at = u64_p(p, "at").ok_or_else(|| bad("takes.split", "`at` (first take index of the new group) is required"))? as usize;
    if at == 0 || at >= g.takes.len() {
        return Err(bad("takes.split", format!("`at` must be between 1 and {} (the group has {} takes)", g.takes.len().saturating_sub(1), g.takes.len())));
    }
    let id = g.id;
    let new_id = s.edit("Split Take Group", move |pr, _| {
        let new_id = pr.alloc_id();
        let t = pr.transcripts.get(&item).ok_or_else(|| EngineError::Other("the transcript is gone".into()))?;
        let mut t = (**t).clone();
        if !tk::split(&mut t.takes, id, at, new_id) {
            return Err(EngineError::Other("could not split the group".into()));
        }
        t.normalize_takes();
        pr.transcripts.insert(item, Arc::new(t));
        Ok(new_id)
    })?;
    Ok(json!({"group": id, "newGroup": new_id}))
}

/// Media range of a take from `from`/`to` (sequence word indices) or `item` + `start`/`end`
/// (media ticks) or `startSeconds`/`endSeconds`.
fn take_range_p(s: &Session, p: &Value, cmd: &str) -> Result<(ItemId, TimeRange)> {
    if let Some(from) = u64_p(p, "from") {
        let words = sequence_words(s);
        let to = u64_p(p, "to").unwrap_or(from) as usize;
        let (a, b) = (from as usize, to);
        let (a, b) = (a.min(b), a.max(b));
        let (wa, wb) = (words.get(a), words.get(b));
        let (wa, wb) = match (wa, wb) {
            (Some(x), Some(y)) => (x, y),
            _ => return Err(bad(cmd, format!("word index out of range (the transcript has {} words)", words.len()))),
        };
        if wa.item != wb.item {
            return Err(bad(cmd, "the words belong to different clips"));
        }
        let t = s.project.transcripts.get(&wa.item).ok_or_else(|| bad(cmd, "no transcript"))?;
        let (ma, mb) = match (t.words.get(wa.index), t.words.get(wb.index)) {
            (Some(x), Some(y)) => (x.start, y.end),
            _ => return Err(bad(cmd, "word index out of range")),
        };
        return Ok((wa.item, TimeRange::from_bounds(ma, mb.max(ma))));
    }
    let item = u64_p(p, "item")
        .map(ItemId)
        .and_then(|i| s.project.resolve_media(i).map(|(r, _, _)| r))
        .ok_or_else(|| bad(cmd, "pass `from`/`to` (word indices) or `item` with `start`/`end`"))?;
    let tick = |k: &str, ks: &str| -> Option<Tick> {
        p.get(k).and_then(Value::as_i64).map(Tick).or_else(|| f64_p(p, ks).filter(|x| x.is_finite()).map(Tick::from_seconds_f64))
    };
    let (a, b) = match (tick("start", "startSeconds"), tick("end", "endSeconds")) {
        (Some(a), Some(b)) => (a, b),
        _ => return Err(bad(cmd, "`start` and `end` (media ticks) or `startSeconds`/`endSeconds` are required")),
    };
    if a < Tick::ZERO || b <= a {
        return Err(bad(cmd, "the range must be non-empty and start at or after 0"));
    }
    Ok((item, TimeRange::from_bounds(a, b)))
}

fn add(s: &mut Session, p: &Value) -> Result<Value> {
    let (item, range) = take_range_p(s, p, "takes.add")?;
    let group = u64_p(p, "group");
    if let Some(gid) = group {
        let (gi, _) = find_group(s, gid)?;
        if gi != item {
            return Err(bad("takes.add", "the take and the group belong to different clips"));
        }
    }
    let id = s.edit("Add Take", move |pr, _| {
        let t = pr.transcripts.get(&item).ok_or_else(|| EngineError::Other("the clip has no transcript".into()))?;
        let mut t = (**t).clone();
        let id = match group {
            Some(gid) => {
                let g = t.take_group_mut(gid).ok_or_else(|| EngineError::Other(format!("no take group {gid}")))?;
                g.takes.push(Take::new(range));
                g.manual = true;
                gid
            }
            None => {
                let id = pr.alloc_id();
                t.takes.push(TakeGroup { id, takes: vec![Take::new(range)], manual: true, ..Default::default() });
                id
            }
        };
        t.normalize_takes();
        pr.transcripts.insert(item, Arc::new(t));
        Ok(id)
    })?;
    Ok(json!({"group": id, "item": item.0, "start": range.start.0, "end": range.end().0}))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let (item, g) = group_p(s, p, "takes.remove")?;
    let take = match p.get("take") {
        Some(_) => Some(take_p(&g, p, "takes.remove")?),
        None => None,
    };
    let id = g.id;
    s.edit(if take.is_some() { "Remove Take" } else { "Remove Take Group" }, move |pr, _| {
        let t = pr.transcripts.get(&item).ok_or_else(|| EngineError::Other("the transcript is gone".into()))?;
        let mut t = (**t).clone();
        match take {
            Some(i) => {
                let g = t.take_group_mut(id).ok_or_else(|| EngineError::Other(format!("no take group {id}")))?;
                if i < g.takes.len() {
                    g.takes.remove(i);
                }
                g.manual = true;
            }
            None => t.takes.retain(|g| g.id != id),
        }
        t.normalize_takes();
        pr.transcripts.insert(item, Arc::new(t));
        Ok(())
    })?;
    Ok(json!({"group": id, "take": take}))
}

/// Sentence boundaries of the live words: a word ending in `.`, `?` or `!`, or a pause of
/// [`SENTENCE_PAUSE`], ends a sentence.
fn sentence_ends(words: &[SeqWord]) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let punct = w.text.trim_end_matches(['"', '\'', ')', ']']).ends_with(['.', '?', '!']);
        let pause = words.get(i + 1).is_some_and(|n| n.start - w.end >= SENTENCE_PAUSE);
        if punct || pause || i + 1 == words.len() {
            out.push(i);
        }
    }
    out
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    let (item, g) = group_p(s, p, "takes.preview")?;
    let i = take_p(&g, p, "takes.preview")?;
    let take = g.takes.get(i).cloned().ok_or_else(|| bad("takes.preview", "take out of range"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let live = live_of(q, item, &take);
    let (Some(first), Some(last)) = (live.first(), live.last()) else {
        return Err(EngineError::Other("that take is crossed out; select or restore it to preview it in context".into()));
    };
    let (ts, te) = (first.start, last.end());
    let pre = u64_p(p, "pre").unwrap_or(1).min(10) as usize;
    let post = u64_p(p, "post").unwrap_or(1).min(10) as usize;
    let words = sequence_words(s);
    let ends = sentence_ends(&words);
    // sentences strictly before the take's first word and after its last word
    let before: Vec<usize> = ends.iter().copied().filter(|&e| words.get(e).is_some_and(|w| w.end <= ts)).collect();
    let after: Vec<usize> = ends.iter().copied().filter(|&e| words.get(e).is_some_and(|w| w.start >= te)).collect();
    let start = if pre == 0 {
        ts
    } else {
        // the sentence `pre` back starts after the end `pre+1` back
        let idx = before.len().checked_sub(pre + 1).and_then(|k| before.get(k)).map(|&e| e + 1).unwrap_or(0);
        words.get(idx).map(|w| w.start).unwrap_or(ts).min(ts)
    };
    let end = if post == 0 { te } else { after.get(post.saturating_sub(1)).and_then(|&e| words.get(e)).map(|w| w.end).unwrap_or(te).max(te) };
    let rate = s.sequence_rate();
    let fd = rate.frame_duration();
    let (a, b) = (rate.snap(start), rate.snap(end) + fd);
    s.edit_sequence("Preview Take", |q, _, _| {
        q.mark_in = Some(a);
        q.mark_out = Some(b - fd);
        Ok(())
    })?;
    s.set_playhead(a);
    Ok(json!({"group": g.id, "take": i, "in": a.0, "out": b.0, "takeStart": ts.0, "takeEnd": te.0}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "takes.detect",
            "Detect Takes",
            Some("Alt+Shift+T"),
            r#"{"items":[id]?,"sensitivity":0..1=0.5,"maxGapSeconds":f64=12,"select":"last"|"first"|"none"="last"}"#,
            has_sequence_transcripts,
            detect,
            true,
        ),
        spec("takes.list", "List Takes", None, r#"{"label":str?,"redo":bool?}"#, has_sequence_transcripts, list, false),
        spec("takes.select", "Select Take", None, r#"{"group":id,"take":n}"#, has_groups, select, true),
        spec("takes.next", "Next Take", Some("Alt+]"), r#"{"group":id?}"#, has_groups, next, true),
        spec("takes.previous", "Previous Take", Some("Alt+["), r#"{"group":id?}"#, has_groups, previous, true),
        spec("takes.cross", "Cross Out Take", Some("Alt+Shift+X"), r#"{"group":id,"take":n}"#, has_groups, cross, true),
        spec("takes.restore", "Restore Take", Some("Alt+Shift+U"), r#"{"group":id,"take":n}"#, has_groups, restore, true),
        spec(
            "takes.label",
            "Label Take…",
            Some("Alt+Shift+L"),
            r#"{"group":id,"take":n,"label":"good"|"best"|"flat"|"stumble"|"wrongEnergy"|null?,"note":str?}"#,
            has_groups,
            set_label,
            true,
        ),
        spec("takes.redo", "Mark for Re-record", Some("Alt+Shift+R"), r#"{"group":id,"redo":bool=true,"note":str?}"#, has_groups, set_redo, true),
        spec("takes.merge", "Merge Take Groups", None, r#"{"groups":[id,id,…]}"#, has_groups, merge, true),
        spec("takes.split", "Split Take Group", None, r#"{"group":id,"at":n}"#, has_groups, split, true),
        spec(
            "takes.add",
            "Add Take",
            None,
            r#"{"from":n,"to":n? | "item":id,"start":ticks,"end":ticks | "startSeconds","endSeconds"; "group":id?}"#,
            has_sequence_transcripts,
            add,
            true,
        ),
        spec("takes.remove", "Remove Take", None, r#"{"group":id,"take":n?}"#, has_groups, remove, true),
        spec("takes.preview", "Preview Take in Context", None, r#"{"group":id,"take":n,"pre":n=1,"post":n=1}"#, has_groups, preview, true),
    ]
}
