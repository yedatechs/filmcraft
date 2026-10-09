//! Text-based editing: the sequence transcript and the edits made through it.
//!
//! Clip transcripts (`filmcraft_project::Transcript`) hold words in **media time**. The sequence
//! transcript ([`sequence_words`]) maps them through the audio track items that play them:
//!
//! - audio tracks are read top first (A1, A2…); a word is taken from the first track whose
//!   transcribed clip covers the word's midpoint, so a dialogue clip duplicated on two tracks (or a
//!   stereo pair split over two mono tracks) reads once;
//! - a word belongs to a clip when its midpoint, mapped through the clip's speed, falls inside the
//!   clip; its timeline bounds are clamped to the clip, so a word cut by an edit is shown cut;
//! - disabled clips, reversed clips and frame holds contribute no words (they don't play speech).
//!
//! Text edits turn word ranges into timeline ranges ([`word_range`]) snapped outward to frames, then
//! reuse the ordinary [`crate::extract`] / [`crate::lift`] edits. Pause and filler-word removal
//! ([`find_pauses`], [`find_fillers`]) produce many ranges that [`ripple_delete_ranges`] removes in
//! one pass, right to left. Captions come from [`caption_blocks`].
//!
//! What those edits took out stays visible as **cut spans** ([`cut_spans`]): media between two
//! consecutive clips of the same source on an audio track. Nothing is stored; spans are derived
//! from the timeline. [`restore_cut`] / [`restore_media`] put media back at a span's anchor (the
//! inverse of Extract), and [`live_ranges`] tells where a media range is still heard.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use filmcraft_project::{Caption, ClipId, ItemId, Sequence, Track, TrackId, TrackItem, Transcript};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

use crate::{EditCtx, EditError};

/// A word of the sequence transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct SeqWord {
    pub text: String,
    /// Sequence time.
    pub start: Tick,
    pub end: Tick,
    /// The track item it is heard through and that item's media.
    pub clip: ClipId,
    pub item: ItemId,
    /// Index of the word in the media item's transcript.
    pub index: usize,
    /// Audio track index (0 = A1).
    pub track: usize,
    pub speaker: Option<String>,
    pub confidence: f32,
}

impl SeqWord {
    pub fn normalized(&self) -> String {
        filmcraft_project::transcript::normalize_word(&self.text)
    }
    pub fn range(&self) -> TimeRange {
        TimeRange::from_bounds(self.start, self.end.max(self.start))
    }
}

/// Transcripts by media item (as in `Project::transcripts`).
pub type Transcripts = BTreeMap<ItemId, Arc<Transcript>>;

fn ticks(t: f64) -> Tick {
    Tick(t.round() as i64)
}

/// The sequence transcript: words of every transcribed clip on the audio tracks, in sequence
/// time order (see the module docs for the rules).
pub fn sequence_words(seq: &Sequence, transcripts: &Transcripts) -> Vec<SeqWord> {
    let mut out: Vec<SeqWord> = Vec::new();
    // timeline ranges already served by a higher track's transcribed clips
    let mut claimed: Vec<TimeRange> = Vec::new();
    for (ti, track) in seq.audio_tracks.iter().enumerate() {
        let mut mine = Vec::new();
        for it in &track.items {
            if !it.enabled || it.reverse || it.frame_hold.is_some() || it.speed <= 0.0 {
                continue;
            }
            let Some(tr) = transcripts.get(&it.item) else { continue };
            mine.push(it.range());
            let speed = it.speed;
            let media_end = it.source_in + ticks(it.duration.0 as f64 * speed);
            let to_tl = |m: Tick| it.start + ticks((m - it.source_in).0 as f64 / speed);
            for wi in tr.words_in(TimeRange::from_bounds(it.source_in, media_end.max(it.source_in))) {
                let w = &tr.words[wi];
                let (a, b) = (to_tl(w.start), to_tl(w.end.max(w.start)));
                let mid = Tick(a.0 + (b.0 - a.0) / 2);
                if mid < it.start || mid >= it.end() || claimed.iter().any(|r| r.contains(mid)) {
                    continue;
                }
                out.push(SeqWord {
                    text: w.text.clone(),
                    start: a.max(it.start),
                    end: b.min(it.end()).max(a.max(it.start)),
                    clip: it.id,
                    item: it.item,
                    index: wi,
                    track: ti,
                    speaker: tr.speaker_name(w),
                    confidence: w.confidence,
                });
            }
        }
        claimed.extend(mine);
    }
    out.sort_by_key(|w| (w.start, w.track));
    out
}

/// Index of the word being spoken at `t` (the last word starting at or before `t` whose end is
/// after `t`).
pub fn word_at(words: &[SeqWord], t: Tick) -> Option<usize> {
    let i = words.partition_point(|w| w.start <= t).checked_sub(1)?;
    (t < words[i].end).then_some(i)
}

/// Paragraphs (Text panel segments): runs of words split where the speaker changes or at a pause
/// of at least `gap`.
pub fn paragraphs(words: &[SeqWord], gap: Tick) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut a = 0;
    for i in 1..=words.len() {
        if i == words.len() || words[i].speaker != words[i - 1].speaker || words[i].start - words[i - 1].end >= gap {
            if a < i {
                out.push(a..i);
            }
            a = i;
        }
    }
    out
}

/// Matches of `query` (one or more words, case and punctuation ignored; the last query word may be
/// a prefix) as word index ranges.
pub fn search(words: &[SeqWord], query: &str) -> Vec<std::ops::Range<usize>> {
    let q: Vec<String> = query.split_whitespace().map(filmcraft_project::transcript::normalize_word).filter(|s| !s.is_empty()).collect();
    if q.is_empty() {
        return Vec::new();
    }
    let norm: Vec<String> = words.iter().map(SeqWord::normalized).collect();
    let mut out = Vec::new();
    for i in 0..norm.len().saturating_sub(q.len() - 1) {
        let ok = q.iter().enumerate().all(|(k, qw)| if k + 1 == q.len() { norm[i + k].starts_with(qw.as_str()) } else { norm[i + k] == *qw });
        if ok {
            out.push(i..i + q.len());
        }
    }
    out
}

/// Timeline range of words `a..=b`, snapped outward to frames.
pub fn word_range(words: &[SeqWord], a: usize, b: usize, rate: FrameRate) -> Option<TimeRange> {
    let (a, b) = (a.min(b), a.max(b));
    let (s, e) = (words.get(a)?.start, words.get(b)?.end);
    let s = rate.snap(s);
    let mut e2 = rate.snap(e);
    if e2 < e || e2 <= s {
        e2 += rate.frame_duration();
    }
    Some(TimeRange::from_bounds(s, e2))
}

/// Media range of words `a..=b` of a clip transcript, snapped outward to the media's frames (for
/// Source-monitor In/Out from a text selection).
pub fn media_word_range(t: &Transcript, a: usize, b: usize, rate: FrameRate) -> Option<TimeRange> {
    let (a, b) = (a.min(b), a.max(b));
    let (s, e) = (t.words.get(a)?.start, t.words.get(b)?.end);
    let s = rate.snap(s);
    let mut e2 = rate.snap(e);
    if e2 < e || e2 <= s {
        e2 += rate.frame_duration();
    }
    Some(TimeRange::from_bounds(s, e2))
}

/// Pauses between consecutive words of at least `min`, as the ranges to remove: each keeps `keep`
/// of silence next to both words and is snapped inward to frames (pauses shorter than one frame
/// after that are skipped).
pub fn find_pauses(words: &[SeqWord], min: Tick, keep: Tick, rate: FrameRate) -> Vec<TimeRange> {
    let mut out = Vec::new();
    for p in words.windows(2) {
        let (a, b) = (p[0].end, p[1].start);
        if b - a < min || b <= a {
            continue;
        }
        let s = a + keep;
        let e = b - keep;
        let mut s2 = rate.snap(s);
        if s2 < s {
            s2 += rate.frame_duration();
        }
        let e2 = rate.snap(e);
        if e2 > s2 {
            out.push(TimeRange::from_bounds(s2, e2));
        }
    }
    out
}

/// The default filler words and phrases (configurable in preferences and per command).
pub const DEFAULT_FILLERS: &[&str] = &["um", "uh", "umm", "uhm", "erm", "er", "ah", "hmm", "mm", "mhm"];

/// Filler words: word index ranges matching one of `fillers` (each a word or a phrase such as
/// "you know"; case and punctuation ignored).
pub fn find_fillers(words: &[SeqWord], fillers: &[String]) -> Vec<std::ops::Range<usize>> {
    let norm: Vec<String> = words.iter().map(SeqWord::normalized).collect();
    let mut phrases: Vec<Vec<String>> = fillers
        .iter()
        .map(|f| f.split_whitespace().map(filmcraft_project::transcript::normalize_word).filter(|s| !s.is_empty()).collect::<Vec<_>>())
        .filter(|p| !p.is_empty())
        .collect();
    // longest phrases first, so "you know" wins over a lone "you"
    phrases.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let mut out = Vec::new();
    let mut i = 0;
    while i < norm.len() {
        let hit = phrases.iter().find(|p| i + p.len() <= norm.len() && p.iter().enumerate().all(|(k, w)| norm[i + k] == *w));
        match hit {
            Some(p) => {
                out.push(i..i + p.len());
                i += p.len();
            }
            None => i += 1,
        }
    }
    out
}

/// Timeline ranges removing the filler words `hits`: each word range snapped to the nearest frames
/// (never into the neighbouring words' frames).
pub fn filler_ranges(words: &[SeqWord], hits: &[std::ops::Range<usize>], rate: FrameRate) -> Vec<TimeRange> {
    let mut out = Vec::new();
    for h in hits {
        let (a, b) = (h.start, h.end - 1);
        let mut s = rate.snap_nearest(words[a].start);
        let mut e = rate.snap_nearest(words[b].end);
        if a > 0 && s < words[a - 1].end {
            s = rate.snap(words[a - 1].end) + rate.frame_duration();
        }
        if let Some(n) = words.get(b + 1)
            && e > n.start
        {
            e = rate.snap(n.start);
        }
        if e > s {
            out.push(TimeRange::from_bounds(s, e));
        }
    }
    out
}

/// Sort and merge ranges (overlapping or touching).
pub fn merge_ranges(mut r: Vec<TimeRange>) -> Vec<TimeRange> {
    r.sort_by_key(|x| x.start);
    let mut out: Vec<TimeRange> = Vec::new();
    for x in r {
        match out.last_mut() {
            Some(l) if x.start <= l.end() => *l = TimeRange::from_bounds(l.start, l.end().max(x.end())),
            _ => out.push(x),
        }
    }
    out
}

/// Ripple-delete every range on every unlocked track (and sync-locked caption tracks), right to
/// left so earlier ranges keep their positions. Returns the total time removed.
pub fn ripple_delete_ranges(seq: &mut Sequence, ranges: Vec<TimeRange>, ctx: &mut EditCtx) -> Tick {
    let tracks: Vec<TrackId> = seq.all_tracks().filter(|t| !t.locked).map(|t| t.id).collect();
    let mut total = Tick::ZERO;
    for r in merge_ranges(ranges).into_iter().rev() {
        if r.duration <= Tick::ZERO {
            continue;
        }
        crate::extract(seq, &tracks, r, ctx);
        total += r.duration;
    }
    total
}

/// Whether an audio track item plays speech forwards (the eligibility rule of [`sequence_words`]):
/// enabled, not reversed, no frame hold, positive speed.
fn plays_speech(it: &TrackItem) -> bool {
    it.enabled && !it.reverse && it.frame_hold.is_none() && it.speed > 0.0
}

/// Media time just after the last one an item plays forwards: `source_in + duration × speed`.
fn media_end(it: &TrackItem) -> Tick {
    Tick(it.source_in.0.saturating_add(ticks(it.duration.0 as f64 * it.speed).0))
}

/// Media that was cut out between two clips of one source on an audio track (what Extract, Remove
/// Pauses or Remove Filler Words left behind), anchored where it would go back.
#[derive(Clone, Debug, PartialEq)]
pub struct CutSpan {
    /// The media item.
    pub item: ItemId,
    /// Media time not played.
    pub media: TimeRange,
    /// Sequence time where it goes back (the end of `before`).
    pub at: Tick,
    /// Audio track index (0 = A1).
    pub track: usize,
    /// The clip ending at `at`.
    pub before: ClipId,
    /// The next clip of the same media on that track.
    pub after: ClipId,
    /// Indices into the media transcript of the words whose midpoint lies inside `media` (empty for
    /// a removed pause).
    pub words: std::ops::Range<usize>,
    /// Index in `sequence_words` of the last live word starting before `at` (where the Text panel
    /// shows the crossed-out words).
    pub after_word: Option<usize>,
}

/// Cut spans of the sequence, sorted by `(at, track)`. For two consecutive clips `a`, `b` of the
/// same transcribed media on an audio track (clips that play speech, as in [`sequence_words`]),
/// the media from `a`'s media Out to `b`'s media In is a span when `b` starts later in the media.
/// Media before the first clip or after the last is not a span. `live` is the sequence transcript
/// ([`sequence_words`]), used for `after_word`.
pub fn cut_spans(seq: &Sequence, transcripts: &Transcripts, live: &[SeqWord]) -> Vec<CutSpan> {
    let mut out = Vec::new();
    for (ti, track) in seq.audio_tracks.iter().enumerate() {
        let mut items: Vec<&TrackItem> = track.items.iter().filter(|it| plays_speech(it)).collect();
        items.sort_by_key(|it| it.start);
        for p in items.windows(2) {
            let (Some(a), Some(b)) = (p.first(), p.get(1)) else { continue };
            if a.item != b.item {
                continue;
            }
            let Some(tr) = transcripts.get(&a.item) else { continue };
            let from = media_end(a);
            if b.source_in <= from {
                continue;
            }
            let Some(len) = b.source_in.0.checked_sub(from.0) else { continue };
            let media = TimeRange::new(from, Tick(len));
            let at = a.end();
            out.push(CutSpan {
                item: a.item,
                media,
                at,
                track: ti,
                before: a.id,
                after: b.id,
                words: tr.words_within(media),
                after_word: live.partition_point(|w| w.start < at).checked_sub(1),
            });
        }
    }
    out.sort_by_key(|c| (c.at, c.track));
    out
}

/// Timeline ranges (sorted, merged) on which `media` of `item` is played by the audio track clips
/// that play speech (as in [`sequence_words`]): each clip's share of `media` mapped through its
/// media In and speed, clamped to the clip. Empty when that media is not heard anywhere.
pub fn live_ranges(seq: &Sequence, item: ItemId, media: TimeRange) -> Vec<TimeRange> {
    if media.is_empty() || media.start.0.checked_add(media.duration.0).is_none() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for it in seq.audio_tracks.iter().flat_map(|t| t.items.iter()) {
        if it.item != item || !plays_speech(it) {
            continue;
        }
        let clip_media = TimeRange::from_bounds(it.source_in, media_end(it).max(it.source_in));
        let Some(m) = clip_media.intersect(&media) else { continue };
        let to_tl = |t: Tick| {
            let off = ticks(t.0.saturating_sub(it.source_in.0) as f64 / it.speed);
            Tick(it.start.0.saturating_add(off.0)).max(it.start).min(it.end())
        };
        let (a, b) = (to_tl(m.start), to_tl(m.end()));
        if b > a {
            out.push(TimeRange::from_bounds(a, b));
        }
    }
    merge_ranges(out)
}

/// Put `media` of `item` back at sequence time `at`, after the clip of that media ending at `at` on
/// audio track `track` (0 = A1): the inverse of Extract for one media range. Returns the restored
/// timeline range `[at, at + media.duration)`.
///
/// On every unlocked track (and sync-locked caption track): a clip of that media ending at `at`
/// whose media continues into `media` at 100% grows by it (the clip before, its linked picture),
/// and so does a 100% clip of other media that Extract split at `at` (the clip starting at `at` is
/// the same media resuming exactly `media.duration` later: music, B-roll on a ripple track); a
/// grown clip joins the clip after it when that clip continues it (no edit point is left); a clip spanning
/// `at` is lengthened when its media allows, else split; everything from `at` moves right. When the
/// clip on `track` could not grow (part of a span, a speed change), a new 100% clip of `media` is
/// inserted at `at` with its linked partners. Fails without changing anything when the track does
/// not exist or is locked, no clip of `item` ends at `at` on it, `media` is empty, shorter than a
/// frame or outside the media.
pub fn restore_media(seq: &mut Sequence, item: ItemId, media: TimeRange, at: Tick, track: usize, ctx: &mut EditCtx) -> crate::Result<TimeRange> {
    restore(seq, item, media, at, track, None, ctx)
}

/// Restore a cut span ([`restore_media`] of its media at its anchor, after `span.before`). Fails when
/// `span.before` is no longer on the span's track or no longer ends at the anchor (the span is out
/// of date).
pub fn restore_cut(seq: &mut Sequence, span: &CutSpan, ctx: &mut EditCtx) -> crate::Result<TimeRange> {
    restore(seq, span.item, span.media, span.at, span.track, Some(span.before), ctx)
}

fn restore(seq: &mut Sequence, item: ItemId, media: TimeRange, at: Tick, track: usize, before: Option<ClipId>, ctx: &mut EditCtx) -> crate::Result<TimeRange> {
    let audio = seq.audio_tracks.get(track).ok_or_else(|| EditError::Other(format!("no audio track A{}", track.saturating_add(1))))?;
    if media.is_empty() {
        return Err(EditError::Nothing);
    }
    let dur = media.duration;
    let media_out = media.start.0.checked_add(dur.0).map(Tick).ok_or(EditError::NoHandles)?;
    if media.start < (ctx.media_start)(item) || (ctx.media_duration)(item).is_some_and(|d| media_out > d) {
        return Err(EditError::NoHandles);
    }
    if dur < ctx.min_duration {
        return Err(EditError::TooShort);
    }
    if audio.locked {
        return Err(EditError::Locked);
    }
    let template = match before {
        Some(id) => {
            let it = audio.item(id).ok_or(EditError::NoItem(id))?;
            if it.end() != at || it.item != item {
                return Err(EditError::Other("the cut is out of date: its clip no longer ends where the media goes back".into()));
            }
            it.clone()
        }
        None => audio
            .items
            .iter()
            .find(|it| it.item == item && it.end() == at)
            .cloned()
            .ok_or_else(|| EditError::Other(format!("no clip of this media ends there on A{}", track.saturating_add(1))))?,
    };
    // everything after `at` moves right by `dur`: refuse a sequence that would overflow
    let latest = seq
        .all_tracks()
        .flat_map(|t| t.items.iter().map(TrackItem::end).chain(t.transitions.iter().map(|x| x.end())))
        .chain(seq.caption_tracks.iter().flat_map(|c| c.captions.iter().map(Caption::end)))
        .fold(at, Tick::max);
    let Some(end) = at.0.checked_add(dur.0).map(Tick) else { return Err(EditError::Other("the sequence would be too long".into())) };
    if latest.0.checked_add(dur.0).is_none() {
        return Err(EditError::Other("the sequence would be too long".into()));
    }
    let target = audio.id;
    let mut work = seq.clone();
    let mut links = HashMap::new();
    let mut grown: Vec<ClipId> = Vec::new();
    let mut audio_grew = false;
    for tr in work.all_tracks_mut() {
        if tr.locked {
            continue;
        }
        // 1. grow the clip of this media that ends at `at` and continues into `media`, and any
        //    other clip that Extract split at this cut: its right piece starts at `at` and
        //    continues it after a media jump of exactly `dur` (music, B-roll on a ripple track)
        let forward = |it: &TrackItem| it.speed == 1.0 && !it.reverse && it.frame_hold.is_none();
        let resumes: Option<(ItemId, Tick)> = tr.items.iter().find(|b| b.start == at && forward(b)).map(|b| (b.item, b.source_in));
        let mut grew_here: Vec<ClipId> = Vec::new();
        for it in &mut tr.items {
            if it.end() != at || !forward(it) {
                continue;
            }
            let restored = it.item == item && media_end(it) == media.start;
            let was_split = it.item != item && resumes == Some((it.item, Tick(media_end(it).0.saturating_add(dur.0))));
            if restored || was_split {
                it.duration = Tick(it.duration.0.saturating_add(dur.0));
                grew_here.push(it.id);
            }
        }
        if tr.id == target && !grew_here.is_empty() {
            audio_grew = true;
        }
        // 2. lengthen (media allowing) or split a clip spanning `at`
        let mut split = false;
        for it in &mut tr.items {
            if grew_here.contains(&it.id) || !(it.start < at && at < it.end()) {
                continue;
            }
            let room = (ctx.media_duration)(it.item).is_none_or(|d| media_end(it).0.saturating_add(dur.0) <= d.0);
            if it.speed == 1.0 && !it.reverse && room {
                it.duration = Tick(it.duration.0.saturating_add(dur.0));
            } else {
                split = true;
            }
        }
        if split {
            crate::split_track_at(tr, at, ctx, &mut links);
        }
        // 3. shift
        crate::shift_track_from(tr, at, dur);
        for id in &grew_here {
            join_continuation(tr, *id);
        }
        grown.extend(grew_here);
    }
    for ct in work.caption_tracks.iter_mut().filter(|c| !c.locked && c.sync_lock) {
        crate::captions::shift_from(ct, at, dur);
    }
    // 4. insert a new clip (and its linked partners) when the clip before could not grow
    if !audio_grew {
        let gap = TimeRange::from_bounds(at, end);
        for tr in work.all_tracks_mut() {
            if tr.locked {
                continue;
            }
            let src = if tr.id == target {
                Some(template.clone())
            } else if template.link.is_some() {
                tr.items.iter().find(|it| it.end() == at && it.link == template.link).cloned()
            } else {
                None
            };
            let Some(mut n) = src else { continue };
            if !crate::track_range_empty(tr, gap) {
                return Err(EditError::Other(format!("{} has material where the media goes back", tr.name)));
            }
            n.id = ClipId(ctx.alloc());
            n.start = at;
            n.duration = dur;
            n.source_in = media.start;
            n.speed = 1.0;
            n.reverse = false;
            n.frame_hold = None;
            let idx = tr.items.partition_point(|i| i.start <= at);
            tr.items.insert(idx, n);
        }
    }
    // 5. transitions
    for tr in work.all_tracks_mut().filter(|t| !t.locked) {
        crate::remove_orphan_transitions(tr);
    }
    *seq = work;
    Ok(TimeRange::from_bounds(at, end))
}

/// Join a grown clip with the clip right after it when that one continues it in media time (the
/// rest of the clip Extract split off), leaving no edit point. The left clip's attributes win.
fn join_continuation(tr: &mut Track, id: ClipId) {
    let Some(i) = tr.items.iter().position(|it| it.id == id) else { return };
    let Some(j) = i.checked_add(1) else { return };
    let (Some(a), Some(b)) = (tr.items.get(i), tr.items.get(j)) else { return };
    if !crate::through::is_through_edit(a, b) {
        return;
    }
    let (aid, bid, bdur) = (a.id, b.id, b.duration);
    if let Some(l) = tr.items.get_mut(i) {
        l.duration = Tick(l.duration.0.saturating_add(bdur.0));
    }
    tr.items.retain(|it| it.id != bid);
    tr.transitions.retain(|t| !(t.from == Some(aid) && t.to == Some(bid)));
    for t in &mut tr.transitions {
        if t.from == Some(bid) {
            t.from = Some(aid);
        }
        if t.to == Some(bid) {
            t.to = Some(aid);
        }
    }
}

/// Rules for Create Captions from a transcript (Premiere's dialog defaults).
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionRules {
    /// Maximum characters per line.
    pub max_chars: usize,
    /// Lines per caption (1 = single, 2 = double).
    pub lines: usize,
    /// Minimum caption duration; a short caption is extended into the silence after it, never
    /// over the next one.
    pub min_duration: Tick,
    /// Longest caption.
    pub max_duration: Tick,
    /// Frames left empty between consecutive captions.
    pub gap_frames: i64,
    /// A pause at least this long starts a new caption.
    pub break_pause: Tick,
}

impl Default for CaptionRules {
    fn default() -> Self {
        Self {
            max_chars: 42,
            lines: 2,
            min_duration: Tick(TICKS_PER_SECOND),
            max_duration: Tick(7 * TICKS_PER_SECOND),
            gap_frames: 0,
            break_pause: Tick(TICKS_PER_SECOND),
        }
    }
}

/// A caption block made from words.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionBlock {
    pub start: Tick,
    pub end: Tick,
    /// Lines joined by `\n`.
    pub text: String,
    pub speaker: Option<String>,
    /// The word index range it shows.
    pub words: std::ops::Range<usize>,
}

/// Lay words out as caption blocks: words fill lines of at most `max_chars` (a longer single word
/// gets a line of its own) and blocks of `lines` lines; a new block starts at a speaker change,
/// at a pause of `break_pause`, after `max_duration`, or after sentence-ending punctuation once the
/// block is past half full. Times are snapped to frames, blocks never overlap and keep
/// `gap_frames` between them.
pub fn caption_blocks(words: &[SeqWord], rules: &CaptionRules, rate: FrameRate) -> Vec<CaptionBlock> {
    let max_chars = rules.max_chars.max(1);
    let max_lines = rules.lines.max(1);
    let cap_chars = max_chars * max_lines;
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    let mut a = 0;
    let mut lines: Vec<usize> = vec![0];
    for i in 0..words.len() {
        let w = &words[i];
        let len = w.text.chars().count();
        if i > a {
            let prev = &words[i - 1];
            let cur = *lines.last().unwrap_or(&0);
            let fits_line = cur + 1 + len <= max_chars;
            let fits = fits_line || lines.len() < max_lines;
            let used: usize = lines.iter().sum::<usize>() + lines.len() - 1;
            let sentence_end = prev.text.ends_with(['.', '?', '!']) && used * 2 >= cap_chars;
            let brk =
                !fits || w.speaker != prev.speaker || w.start - prev.end >= rules.break_pause || w.end - words[a].start > rules.max_duration || sentence_end;
            if brk {
                groups.push(a..i);
                a = i;
                lines = vec![len];
                continue;
            }
            if fits_line {
                if let Some(l) = lines.last_mut() {
                    *l += 1 + len;
                }
            } else {
                lines.push(len);
            }
        } else {
            lines = vec![len];
        }
    }
    if a < words.len() {
        groups.push(a..words.len());
    }
    // text, frame-snapped times
    let fd = rate.frame_duration();
    let gap = Tick(fd.0 * rules.gap_frames.max(0));
    let mut out: Vec<CaptionBlock> = Vec::new();
    for g in groups {
        let mut text_lines: Vec<String> = vec![String::new()];
        for w in &words[g.clone()] {
            let Some(l) = text_lines.last_mut() else { break };
            if l.is_empty() {
                l.push_str(&w.text);
            } else if l.chars().count() + 1 + w.text.chars().count() <= max_chars {
                l.push(' ');
                l.push_str(&w.text);
            } else {
                text_lines.push(w.text.clone());
            }
        }
        let start = rate.snap(words[g.start].start);
        let mut end = rate.snap(words[g.end - 1].end);
        if end < words[g.end - 1].end {
            end += fd;
        }
        out.push(CaptionBlock { start, end: end.max(start + fd), text: text_lines.join("\n"), speaker: words[g.start].speaker.clone(), words: g });
    }
    // no overlaps, minimum duration (into the silence after a block, never over the next one)
    for i in 0..out.len() {
        if i > 0 {
            let min_start = out[i - 1].end + gap;
            if out[i].start < min_start {
                out[i].start = min_start;
                out[i].end = out[i].end.max(min_start + fd);
            }
        }
        let next = out.get(i + 1).map(|n| n.start);
        let b = &mut out[i];
        if b.end - b.start < rules.min_duration {
            let want = b.start + rules.min_duration;
            let mut e = rate.snap(want);
            if e < want {
                e += fd;
            }
            b.end = e;
        }
        if let Some(n) = next {
            b.end = b.end.min(n - gap).max(b.start + fd);
        }
    }
    out
}

/// Caption blocks as captions (ids from `ctx`).
pub fn blocks_to_captions(blocks: &[CaptionBlock], ctx: &mut EditCtx) -> Vec<Caption> {
    blocks
        .iter()
        .map(|b| Caption {
            id: ClipId(ctx.alloc()),
            start: b.start,
            duration: b.end - b.start,
            text: b.text.clone(),
            speaker: b.speaker.clone(),
            cue_id: None,
            settings: String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
