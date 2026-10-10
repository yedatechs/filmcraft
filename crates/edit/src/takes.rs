//! Take detection: lines the speaker recorded more than once.
//!
//! Pure functions on a media transcript (`filmcraft_project::Transcript`): split the words into
//! utterances, compare neighbouring utterances by normalised-word similarity and spoken retake
//! cues, and bundle similar passes into `TakeGroup`s (media time). The engine (`takes.*`
//! commands) assigns group ids, stores the groups on the transcript and switches takes on the
//! timeline. See `openspec/changes/take-editing/design.md` §4.

use std::collections::BTreeSet;
use std::ops::Range;

use filmcraft_project::Transcript;
use filmcraft_project::Word;
use filmcraft_project::transcript::{Take, TakeGroup, normalize_word};
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};

/// Words of an utterance taken into a comparison (longer utterances are compared on their opening).
const MAX_COMPARE_WORDS: usize = 64;
/// Largest comparison window honoured, whatever the caller asks for.
const MAX_WINDOW: usize = 64;
/// Most retake cue words stripped from the start of an utterance.
const MAX_CUE_WORDS: usize = 8;
/// Words that open a retake ("okay again, …"); stripped before comparing.
const CUES: [&str; 10] = ["okay", "ok", "again", "sorry", "let", "let's", "lets", "take", "redo", "wait"];
/// How much a retake cue lowers the grouping threshold.
const CUE_BONUS: f32 = 0.15;
/// Shortest opening compared by [`similarity`]'s opening-overlap term.
const OPENING_WORDS: usize = 4;

/// Parameters of [`detect`]. Every value is treated as hostile: out-of-range values are clamped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectParams {
    /// 0 (strict) ..= 1 (loose); NaN reads as the default 0.5.
    pub sensitivity: f32,
    /// A silence at least this long ends an utterance.
    pub pause: Tick,
    /// Longest silence between two takes of one line.
    pub max_gap: Tick,
    /// How many following utterances each utterance is compared with.
    pub window: usize,
    /// Fewest content words (after retake cues) an utterance needs to be a take; also the
    /// shortest false start recognised, including one that restarts within an utterance without a
    /// pause (there at least 2, so a stutter like "the the" is not a false start).
    pub min_words: usize,
}

impl Default for DetectParams {
    fn default() -> Self {
        Self { sensitivity: 0.5, pause: Tick(TICKS_PER_SECOND / 2), max_gap: Tick(TICKS_PER_SECOND.saturating_mul(12)), window: 4, min_words: 2 }
    }
}

impl DetectParams {
    fn sensitivity(&self) -> f32 {
        if self.sensitivity.is_nan() { 0.5 } else { self.sensitivity.clamp(0.0, 1.0) }
    }

    /// The similarity two utterances need to be takes of one line (before the retake-cue bonus).
    pub fn threshold(&self) -> f32 {
        (0.85 - 0.5 * self.sensitivity()).clamp(0.2, 0.95)
    }
}

/// Whether the word closes a sentence (`.`, `?` or `!`, ignoring closing quotes and brackets).
fn ends_sentence(w: &Word) -> bool {
    let t = w.text.trim().trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}']);
    t.ends_with(['.', '?', '!'])
}

/// Split the words into utterances: a new one starts after a silence of at least `pause` or after a
/// word that ends a sentence. Ranges are word indices, in order, non-empty and covering every word.
pub fn utterances(words: &[Word], pause: Tick) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, w) in words.iter().enumerate() {
        let last = match words.get(i + 1) {
            None => true,
            Some(next) => ends_sentence(w) || next.start.0.saturating_sub(w.end.0) >= pause.0,
        };
        if last {
            out.push(start..i + 1);
            start = i + 1;
        }
    }
    out
}

/// Levenshtein distance between two word lists (both at most [`MAX_COMPARE_WORDS`] long).
fn levenshtein(a: &[String], b: &[String]) -> usize {
    let a = a.get(..a.len().min(MAX_COMPARE_WORDS)).unwrap_or(&[]);
    let b = b.get(..b.len().min(MAX_COMPARE_WORDS)).unwrap_or(&[]);
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, wa) in a.iter().enumerate() {
        if let Some(c) = cur.get_mut(0) {
            *c = i + 1;
        }
        for (j, wb) in b.iter().enumerate() {
            let sub = prev.get(j).copied().unwrap_or(0).saturating_add(usize::from(wa != wb));
            let del = prev.get(j + 1).copied().unwrap_or(0).saturating_add(1);
            let ins = cur.get(j).copied().unwrap_or(0).saturating_add(1);
            if let Some(c) = cur.get_mut(j + 1) {
                *c = sub.min(del).min(ins);
            }
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev.get(b.len()).copied().unwrap_or(0)
}

/// Similarity with the false-start rule applying to openings of at least `min_prefix` words.
fn similarity_with(a: &[String], b: &[String], min_prefix: usize) -> f32 {
    let a = a.get(..a.len().min(MAX_COMPARE_WORDS)).unwrap_or(&[]);
    let b = b.get(..b.len().min(MAX_COMPARE_WORDS)).unwrap_or(&[]);
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a.len() >= min_prefix.max(1) && b.starts_with(a) {
        return 1.0;
    }
    let longest = a.len().max(b.len());
    let n = OPENING_WORDS.min(longest);
    let run = a.iter().zip(b.iter()).take(n).take_while(|(x, y)| x == y).count();
    let opening = run as f32 / n as f32;
    let sa: BTreeSet<&str> = a.iter().map(String::as_str).collect();
    let sb: BTreeSet<&str> = b.iter().map(String::as_str).collect();
    let union = sa.union(&sb).count();
    let jaccard = if union == 0 { 0.0 } else { sa.intersection(&sb).count() as f32 / union as f32 };
    let edit = 1.0 - levenshtein(a, b) as f32 / longest as f32;
    opening.max(jaccard).max(edit).clamp(0.0, 1.0)
}

/// How alike two utterances are, 0..=1, on normalised words ([`normalize_word`]): the best of the
/// shared opening (the first up-to-four words matching in order), token Jaccard overlap and
/// `1 - word edit distance / longer length`. An `a` of two or more words that is the opening of `b`
/// (a false start) scores 1. Utterances are compared on their first 64 words.
pub fn similarity(a: &[String], b: &[String]) -> f32 {
    similarity_with(a, b, DetectParams::default().min_words)
}

/// Strip leading retake cues; returns the content words and whether any cue was found.
fn strip_cues(words: &[String]) -> (&[String], bool) {
    let mut rest = words;
    let mut cue = false;
    for _ in 0..MAX_CUE_WORDS {
        let first = rest.first().map(String::as_str);
        let second = rest.get(1).map(String::as_str);
        let skip = match (first, second) {
            (Some("one"), Some("more")) => 2,
            (Some(w), _) if CUES.contains(&w) => 1,
            _ => break,
        };
        rest = rest.get(skip..).unwrap_or(&[]);
        cue = true;
    }
    (rest, cue)
}

/// One utterance prepared for comparison.
struct Utt {
    range: TimeRange,
    words: Vec<String>,
    cue: bool,
}

/// Where `words` (an utterance's content words) first restarts its own opening: the smallest
/// `j >= min_words` (at most [`MAX_COMPARE_WORDS`]) such that `words[j..]` begins with all of
/// `words[..j]`, so the part before `j` is a false start of what follows. `None` for openings
/// shorter than `min_words`.
fn restart(words: &[String], min_words: usize) -> Option<usize> {
    let opening = words.get(..min_words)?;
    let last = MAX_COMPARE_WORDS.min(words.len() / 2);
    (min_words..=last).find(|&j| words.get(j..).is_some_and(|tail| tail.starts_with(opening) && words.get(..j).is_some_and(|head| tail.starts_with(head))))
}

/// The content-word positions where `words` restarts its opening without a pause ("what a what a
/// time" splits before the second "what"), repeated on each remainder. Openings are at least two
/// words (`min_words`, raised to 2) so a stutter like "the the" is not a restart. Strictly
/// increasing.
fn restarts(words: &[String], min_words: usize) -> Vec<usize> {
    let m = min_words.max(2);
    let mut out = Vec::new();
    let mut start = 0usize;
    // Each step moves `start` forward by at least `m`, so this ends within `words.len() / 2` steps.
    while let Some(j) = words.get(start..).and_then(|rest| restart(rest, m)) {
        start = start.saturating_add(j);
        out.push(start);
    }
    out
}

/// The comparison units of one utterance (word indices `r` of `words`): normalised words with the
/// leading retake cues stripped, split where the utterance restarts its own opening ([`restarts`]).
/// Each part spans from its first word's start to its last word's end; the first part keeps the
/// cue words and the cue flag.
fn utterance_parts(words: &[Word], r: Range<usize>, min_words: usize) -> Vec<Utt> {
    let mut norm: Vec<String> = Vec::new();
    let mut index: Vec<usize> = Vec::new();
    for (i, w) in words.get(r.clone()).unwrap_or(&[]).iter().enumerate() {
        let n = normalize_word(&w.text);
        if !n.is_empty() {
            norm.push(n);
            index.push(r.start.saturating_add(i));
        }
    }
    let (content, cue) = strip_cues(&norm);
    let skipped = norm.len().saturating_sub(content.len());
    // (first word index, first content index) of each part, then the end.
    let mut bounds: Vec<(usize, usize)> = vec![(r.start, 0)];
    for c in restarts(content, min_words) {
        if let Some(&w) = index.get(skipped.saturating_add(c)) {
            bounds.push((w, c));
        }
    }
    bounds.push((r.end, content.len()));
    bounds
        .windows(2)
        .enumerate()
        .filter_map(|(k, pair)| {
            let (&(wa, ca), &(wb, cb)) = (pair.first()?, pair.get(1)?);
            let ws = words.get(wa..wb)?;
            let (first, last) = (ws.first()?, ws.last()?);
            let part: Vec<String> = content.get(ca..cb)?.iter().take(MAX_COMPARE_WORDS).cloned().collect();
            Some(Utt { range: TimeRange::from_bounds(first.start, last.end.max(first.start)), words: part, cue: cue && k == 0 })
        })
        .collect()
}

/// Group the utterances of `t` that repeat a line, in media order. An utterance that restarts its
/// own opening without a pause ("what a what a time to be alive") is first split there, so the
/// false start and the full line are compared like pause-separated takes. Each utterance is compared with
/// the next `window` utterances that start within `max_gap` of its end; the first one similar
/// enough (`similarity >= threshold`, the threshold lowered by 0.15 when the later utterance opens
/// with a retake cue such as "okay", "again" or "one more") joins its group and the chain continues
/// from it. Groups have at least two takes; ids are 0 (the engine assigns them) and `manual` is
/// false. Deterministic, and `O(utterances × window)`.
pub fn detect(t: &Transcript, p: &DetectParams) -> Vec<TakeGroup> {
    let min_words = p.min_words.max(1);
    let window = p.window.min(MAX_WINDOW);
    let threshold = p.threshold();
    let utts: Vec<Utt> = utterances(&t.words, p.pause).into_iter().flat_map(|r| utterance_parts(&t.words, r, p.min_words)).collect();
    let eligible = |u: &Utt| u.words.len() >= min_words;
    let mut used = vec![false; utts.len()];
    let mut groups = Vec::new();
    for (start, su) in utts.iter().enumerate() {
        if used.get(start).copied().unwrap_or(true) || !eligible(su) {
            continue;
        }
        let mut members = vec![start];
        let mut k = start;
        // Each step moves `k` strictly forward, so the chain ends within `utts.len()` steps.
        while let Some(ku) = utts.get(k) {
            let end = k.saturating_add(window).min(utts.len().saturating_sub(1));
            let next = (k + 1..=end).find(|&j| {
                let Some(ju) = utts.get(j) else { return false };
                if used.get(j).copied().unwrap_or(true) || !eligible(ju) {
                    return false;
                }
                if ju.range.start.0.saturating_sub(ku.range.end().0) > p.max_gap.0 {
                    return false;
                }
                let thr = if ju.cue { threshold - CUE_BONUS } else { threshold };
                similarity_with(&ku.words, &ju.words, min_words) >= thr
            });
            let Some(j) = next else { break };
            members.push(j);
            k = j;
        }
        if members.len() >= 2 {
            for &m in &members {
                if let Some(u) = used.get_mut(m) {
                    *u = true;
                }
            }
            let mut takes: Vec<Take> = members.iter().filter_map(|&m| utts.get(m)).map(|u| Take::new(u.range)).collect();
            takes.sort_by_key(|t| (t.range.start, t.range.duration));
            groups.push(TakeGroup { id: 0, takes, ..Default::default() });
        }
    }
    sort_groups(&mut groups);
    groups
}

fn group_start(g: &TakeGroup) -> Tick {
    g.range().map(|r| r.start).unwrap_or(Tick::ZERO)
}

fn sort_groups(groups: &mut [TakeGroup]) {
    groups.sort_by_key(|g| (group_start(g), g.id));
}

/// Merge the groups with these ids into the first of them (in `ids` order): its takes become all the
/// takes, sorted by media time (identical ranges kept once), and it is marked `manual`. The other
/// groups are removed. Returns the kept id, or `None` (nothing changed) unless at least two distinct
/// ids name existing groups.
pub fn merge(groups: &mut Vec<TakeGroup>, ids: &[u64]) -> Option<u64> {
    let mut found: Vec<u64> = Vec::new();
    for &id in ids {
        if !found.contains(&id) && groups.iter().any(|g| g.id == id) {
            found.push(id);
        }
    }
    let (&target, rest) = found.split_first()?;
    if rest.is_empty() {
        return None;
    }
    let mut moved: Vec<Take> = Vec::new();
    groups.retain_mut(|g| {
        if rest.contains(&g.id) {
            moved.append(&mut g.takes);
            false
        } else {
            true
        }
    });
    let g = groups.iter_mut().find(|g| g.id == target)?;
    g.takes.append(&mut moved);
    g.takes.sort_by_key(|t| (t.range.start, t.range.duration));
    g.takes.dedup_by(|a, b| a.range == b.range);
    g.manual = true;
    sort_groups(groups);
    Some(target)
}

/// Move the takes `at_take..` of group `id` into a new group `new_id` placed after it; both are
/// marked `manual`. Returns false (nothing changed) when `id` is unknown, `new_id` is already used,
/// or `at_take` would leave either group empty.
pub fn split(groups: &mut Vec<TakeGroup>, id: u64, at_take: usize, new_id: u64) -> bool {
    if new_id == id || groups.iter().any(|g| g.id == new_id) {
        return false;
    }
    let Some(pos) = groups.iter().position(|g| g.id == id) else { return false };
    let Some(g) = groups.get_mut(pos) else { return false };
    if at_take == 0 || at_take >= g.takes.len() {
        return false;
    }
    let tail = g.takes.split_off(at_take);
    g.manual = true;
    let new = TakeGroup { id: new_id, takes: tail, manual: true, ..Default::default() };
    groups.insert(pos.saturating_add(1).min(groups.len()), new);
    true
}

/// The fraction (0..=1) of `take`'s duration covered by the union of `live` (the media ranges the
/// sequence plays). 0 for an empty take. A take is "live" at 0.5 or more.
pub fn live_fraction(live: &[TimeRange], take: &TimeRange) -> f32 {
    if take.duration.0 <= 0 {
        return 0.0;
    }
    let take_end = take.start.0.saturating_add(take.duration.0);
    let mut parts: Vec<(i64, i64)> = live
        .iter()
        .filter(|r| r.duration.0 > 0)
        .map(|r| (r.start.0.max(take.start.0), r.start.0.saturating_add(r.duration.0).min(take_end)))
        .filter(|(a, b)| b > a)
        .collect();
    parts.sort_unstable();
    let mut covered: i64 = 0;
    let mut reach = take.start.0;
    for (a, b) in parts {
        let a = a.max(reach);
        if b > a {
            covered = covered.saturating_add(b - a);
            reach = b;
        }
    }
    ((covered as f64 / take.duration.0 as f64) as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests;
