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
/// Words that open a retake ("okay again, …") or are only a lead-in ("yo", "um"); stripped from the
/// start of an utterance before comparing.
const CUES: [&str; 15] = ["okay", "ok", "again", "sorry", "let", "let's", "lets", "take", "redo", "wait", "yo", "alright", "right", "um", "uh"];
/// How much a retake cue lowers the grouping threshold.
const CUE_BONUS: f32 = 0.15;
/// Shortest opening compared by [`similarity`]'s opening-overlap term.
const OPENING_WORDS: usize = 4;
/// How far back (in content words, stutters collapsed) a restart looks for the phrase it repeats: a
/// speaker who restarts says the phrase again within a few words of the abandoned attempt.
const MAX_LOOKBACK: usize = 10;
/// Longest false start (words from the first occurrence to the second) a two-word repeat may close;
/// a repeat of three or more words counts up to [`MAX_LOOKBACK`]. Two words repeat naturally ("the
/// river … the river"), so they only count close together.
const SHORT_REPEAT_SPAN: usize = 6;
/// Most restarts split out of one utterance (bounds the work on a hostile, endless utterance).
const MAX_RESTARTS: usize = 64;
/// Words after which a repeated phrase continues the sentence ("I went to the store and then I went
/// to the bank", "three days to use it and three days to decide") instead of restarting it.
const JOINERS: [&str; 14] = ["and", "then", "but", "or", "nor", "so", "because", "cause", "while", "when", "until", "if", "plus", "yet"];
/// Words that do not count when a retake is checked for saying a false start's words again.
const CONTINUATION_STOPS: [&str; 20] =
    ["the", "a", "an", "and", "to", "of", "in", "is", "it", "that", "this", "so", "but", "or", "on", "at", "for", "with", "be", "was"];
/// How many words of the retake (after the repeated phrase) are checked for a false start's words.
const CONTINUATION_WORDS: usize = 6;
/// Number words: a false start whose words after the repeated phrase are all numbers is a list
/// ("twenty dollar, hundred dollar, two hundred dollar"), not a restart.
const NUMBER_WORDS: [&str; 33] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
    "million",
    "billion",
    "trillion",
];

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
    // A shorter `a` against the head of `b`: a false start that differs in a word or two.
    let near_prefix = match b.get(..a.len()) {
        Some(head) if a.len() >= min_prefix.max(2) && a.len() < b.len() => 1.0 - levenshtein(a, head) as f32 / a.len() as f32,
        _ => 0.0,
    };
    let longest = a.len().max(b.len());
    let n = OPENING_WORDS.min(longest);
    let run = a.iter().zip(b.iter()).take(n).take_while(|(x, y)| x == y).count();
    let opening = run as f32 / n as f32;
    let sa: BTreeSet<&str> = a.iter().map(String::as_str).collect();
    let sb: BTreeSet<&str> = b.iter().map(String::as_str).collect();
    let union = sa.union(&sb).count();
    let jaccard = if union == 0 { 0.0 } else { sa.intersection(&sb).count() as f32 / union as f32 };
    let edit = 1.0 - levenshtein(a, b) as f32 / longest as f32;
    opening.max(jaccard).max(edit).max(near_prefix).clamp(0.0, 1.0)
}

/// How alike two utterances are, 0..=1, on normalised words ([`normalize_word`]): the best of the
/// shared opening (the first up-to-four words matching in order), token Jaccard overlap and
/// `1 - word edit distance / longer length`; a shorter `a` is also compared with the head of `b` as
/// long as `a` (`1 - edit distance / len(a)`). An `a` of two or more words that is the opening of `b`
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

/// One utterance part prepared for comparison.
struct Utt {
    range: TimeRange,
    /// Normalised content words, stutters collapsed, at most [`MAX_COMPARE_WORDS`].
    words: Vec<String>,
    cue: bool,
    /// This part is a false start of the next part (a restart without a pause, [`restarts`]).
    link: bool,
}

/// `words` with each run of one repeated word kept once ("so so now" reads "so now"), and for each
/// kept word its index in `words` (the first of its run).
fn collapse_stutters(words: &[String]) -> (Vec<String>, Vec<usize>) {
    let mut out: Vec<String> = Vec::new();
    let mut index = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if out.last() != Some(w) {
            out.push(w.clone());
            index.push(i);
        }
    }
    (out, index)
}

/// The start `i` of the nearest earlier occurrence (at or after `floor`, at most [`MAX_LOOKBACK`]
/// words back) of a phrase of at least `m` words that `words` says again at `j`, if that repeat is a
/// restart: two shared words only within [`SHORT_REPEAT_SPAN`] words and when [`two_word_restart`]
/// agrees, three or more anywhere in the look-back; the repeat must not follow a joining word ([`JOINERS`], "… and then I went …") and must
/// not extend to the left (then it repeats from an earlier position, which was already examined).
/// A word without its clitic ("we'll" → "we", "i've" → "i"), so a restart that changes the
/// contraction still reads as saying the word again.
fn stem(w: &str) -> &str {
    w.split('\'').next().unwrap_or(w)
}

fn is_number(w: &str) -> bool {
    NUMBER_WORDS.contains(&w) || (w.chars().any(|c| c.is_ascii_digit()) && w.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ','))
}

/// Whether a two-word repeat at `j` of the phrase at `i` is a restart rather than natural
/// repetition: the false start `i..j` is at most three words ("and they also | and they don't"), or
/// the retake goes on to say one of the false start's remaining words again within
/// [`CONTINUATION_WORDS`] ("and they remove the fire | and they also remove the five hour", "so now
/// we'll get | so now we have"); a false start whose remaining words are all numbers is a list
/// ("twenty dollar, hundred dollar, two hundred dollar") and never a restart. "for my dopamine and
/// terrible | for my sleep" and "complaining about usage limits | complaining about prices" share
/// nothing after the phrase and are left alone.
fn two_word_restart(words: &[String], i: usize, j: usize) -> bool {
    let rest = words.get(i.saturating_add(2)..j).unwrap_or(&[]);
    if !rest.is_empty() && rest.iter().all(|w| is_number(w)) {
        return false;
    }
    if j.saturating_sub(i) <= 3 {
        return true;
    }
    let from = j.saturating_add(2).min(words.len());
    let cont = words.get(from..from.saturating_add(CONTINUATION_WORDS).min(words.len())).unwrap_or(&[]);
    rest.iter().any(|w| !CONTINUATION_STOPS.contains(&stem(w)) && cont.iter().any(|c| stem(c) == stem(w)))
}

fn repeat_before(words: &[String], floor: usize, j: usize, m: usize) -> Option<usize> {
    let before = j.checked_sub(1).and_then(|p| words.get(p))?;
    if JOINERS.contains(&before.as_str()) {
        return None;
    }
    let lo = floor.max(j.saturating_sub(MAX_LOOKBACK));
    (lo..j).rev().find(|&i| {
        let span = j.saturating_sub(i);
        let shared = (0..span).take_while(|&t| words.get(i.saturating_add(t)).is_some_and(|w| words.get(j.saturating_add(t)) == Some(w))).count();
        if shared < m || (shared < 3 && (span > SHORT_REPEAT_SPAN || !two_word_restart(words, i, j))) {
            return false;
        }
        i == floor || i.checked_sub(1).and_then(|p| words.get(p)) != Some(before)
    })
}

/// Where `words` (an utterance's content words, stutters collapsed) restarts without a pause:
/// `(i, j)` pairs where the phrase at `i` (at least `min_words` words, raised to 2) is said again at
/// `j` within a few words ([`repeat_before`]), so `i..j` is a false start of what follows `j`.
/// Scanned left to right, each `i` at or after the previous `j`; at most [`MAX_RESTARTS`] pairs, and
/// `O(words × MAX_LOOKBACK²)`. "what a what a time" restarts at (0, 2); a stutter ("the the") was
/// collapsed and never restarts.
fn restarts(words: &[String], min_words: usize) -> Vec<(usize, usize)> {
    let m = min_words.max(2);
    let mut out = Vec::new();
    let mut floor = 0usize;
    for j in 1..words.len() {
        if out.len() >= MAX_RESTARTS {
            break;
        }
        if let Some(i) = repeat_before(words, floor, j, m) {
            out.push((i, j));
            floor = j;
        }
    }
    out
}

/// The comparison units of one utterance (word indices `r` of `words`): normalised words with the
/// leading retake cues stripped and stutters collapsed, split where the utterance restarts a phrase
/// ([`restarts`]): before the first occurrence and before the repeat. Each part spans from its first
/// word's start to its last word's end; the first part keeps the cue words and the cue flag; a part
/// that is a false start of the next one is `link`ed to it.
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
    let (content, kept) = collapse_stutters(content);
    let word_at = |c: usize| kept.get(c).and_then(|&k| index.get(skipped.saturating_add(k))).copied();
    // (first word index, first content index, false start of the next part) of each part, then the end.
    let mut bounds: Vec<(usize, usize, bool)> = vec![(r.start, 0, false)];
    for (i, j) in restarts(&content, min_words) {
        let (Some(wi), Some(wj)) = (word_at(i), word_at(j)) else { continue };
        match bounds.last_mut() {
            Some(last) if last.1 == i => last.2 = true,
            _ => bounds.push((wi, i, true)),
        }
        bounds.push((wj, j, false));
    }
    bounds.push((r.end, content.len(), false));
    bounds
        .windows(2)
        .enumerate()
        .filter_map(|(k, pair)| {
            let (&(wa, ca, link), &(wb, cb, _)) = (pair.first()?, pair.get(1)?);
            let ws = words.get(wa..wb)?;
            let (first, last) = (ws.first()?, ws.last()?);
            let part: Vec<String> = content.get(ca..cb)?.iter().take(MAX_COMPARE_WORDS).cloned().collect();
            Some(Utt { range: TimeRange::from_bounds(first.start, last.end.max(first.start)), words: part, cue: cue && k == 0, link })
        })
        .collect()
}

/// Group the utterances of `t` that repeat a line, in media order. An utterance that restarts a
/// phrase without a pause ("what a what a time to be alive", "and they don't even have a and they
/// don't even have a five hour window") is first split there ([`restarts`]); the false starts and
/// the part they restart form one unit whose parts are takes of one line. Each unit is compared with
/// the next `window` units that start within `max_gap` of its end: its last part against the other
/// unit's first and last parts. The first one similar enough (`similarity >= threshold`, the
/// threshold lowered by 0.15 when the later unit opens with a retake cue such as "okay", "again" or
/// "one more") joins its group and the chain continues from it. Groups have at least two takes; ids
/// are 0 (the engine assigns them) and `manual` is false. Deterministic, and
/// `O(utterances × window)`.
pub fn detect(t: &Transcript, p: &DetectParams) -> Vec<TakeGroup> {
    let min_words = p.min_words.max(1);
    let window = p.window.min(MAX_WINDOW);
    let threshold = p.threshold();
    let utts: Vec<Utt> = utterances(&t.words, p.pause).into_iter().flat_map(|r| utterance_parts(&t.words, r, p.min_words)).collect();
    // Units: runs of parts each linked to the next (a restart chain), or a single part.
    let mut units: Vec<Range<usize>> = Vec::new();
    let mut from = 0usize;
    for (k, u) in utts.iter().enumerate() {
        if !u.link {
            units.push(from..k + 1);
            from = k + 1;
        }
    }
    if from < utts.len() {
        units.push(from..utts.len());
    }
    let head = |u: &Range<usize>| utts.get(u.start);
    let tail = |u: &Range<usize>| u.end.checked_sub(1).and_then(|e| utts.get(e));
    let eligible = |u: &Range<usize>| tail(u).is_some_and(|t| t.words.len() >= min_words);
    let mut used = vec![false; units.len()];
    let mut groups = Vec::new();
    for (start, su) in units.iter().enumerate() {
        if used.get(start).copied().unwrap_or(true) || !eligible(su) {
            continue;
        }
        let mut members = vec![start];
        let mut k = start;
        // Each step moves `k` strictly forward, so the chain ends within `units.len()` steps.
        while let Some(ku) = units.get(k).and_then(tail) {
            let end = k.saturating_add(window).min(units.len().saturating_sub(1));
            let next = (k + 1..=end).find(|&j| {
                let Some(jr) = units.get(j) else { return false };
                let (Some(jh), Some(jt)) = (head(jr), tail(jr)) else { return false };
                if used.get(j).copied().unwrap_or(true) || !eligible(jr) {
                    return false;
                }
                if jh.range.start.0.saturating_sub(ku.range.end().0) > p.max_gap.0 {
                    return false;
                }
                let thr = if jh.cue { threshold - CUE_BONUS } else { threshold };
                similarity_with(&ku.words, &jh.words, min_words) >= thr || similarity_with(&ku.words, &jt.words, min_words) >= thr
            });
            let Some(j) = next else { break };
            members.push(j);
            k = j;
        }
        let parts: Vec<&Utt> = members.iter().filter_map(|&m| units.get(m)).flat_map(|r| utts.get(r.clone()).unwrap_or(&[])).collect();
        if parts.len() >= 2 {
            for &m in &members {
                if let Some(u) = used.get_mut(m) {
                    *u = true;
                }
            }
            let mut takes: Vec<Take> = parts.iter().map(|u| Take::new(u.range)).collect();
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
