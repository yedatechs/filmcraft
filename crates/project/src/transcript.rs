//! Transcripts of media clips (Text panel ▸ Transcript).
//!
//! A [`Transcript`] belongs to one media item (`Project::transcripts`, keyed by the item id) and
//! lists the spoken [`Word`]s with their **media-time** bounds, so it stays valid however the clip
//! is trimmed, moved or reused: the sequence transcript is derived from the clip transcripts by
//! mapping each word through the track items that show it (`filmcraft_edit::transcript`).
//!
//! Speakers are numbered per transcript (`Word::speaker` indexes [`Transcript::speakers`]); the
//! Text panel shows and renames them by name, so two clips whose speakers share a name read as one
//! speaker in the sequence transcript.

use filmcraft_time::{Tick, TimeRange};
use serde::{Deserialize, Serialize};

use crate::scene::{self, SceneSpan};

/// One transcribed word.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// The word as written, with attached punctuation (`"Hello,"`). No surrounding spaces.
    pub text: String,
    /// Media time of the word's first sample.
    pub start: Tick,
    /// Media time just after the word (exclusive).
    pub end: Tick,
    /// Index into [`Transcript::speakers`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<u32>,
    /// Recogniser confidence 0..1 (1 for hand-made or corrected words).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub confidence: f32,
}

fn one() -> f32 {
    1.0
}
fn is_one(v: &f32) -> bool {
    *v == 1.0
}

impl Word {
    pub fn new(text: impl Into<String>, start: Tick, end: Tick) -> Self {
        Self { text: text.into(), start, end, speaker: None, confidence: 1.0 }
    }
    pub fn range(&self) -> TimeRange {
        TimeRange::from_bounds(self.start, self.end.max(self.start))
    }
    /// Lower-case text without leading/trailing punctuation (`"Um,"` → `"um"`), for search and
    /// filler-word matching.
    pub fn normalized(&self) -> String {
        normalize_word(&self.text)
    }
}

/// Lower-case `s` and trim punctuation and symbols from both ends (apostrophes inside a word stay:
/// `"Don't!"` → `"don't"`).
pub fn normalize_word(s: &str) -> String {
    s.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// A speaker label.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Speaker {
    pub name: String,
}

/// How a take sounded (Text panel ▸ Takes; `takes.label`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TakeLabel {
    Good,
    Best,
    Flat,
    Stumble,
    WrongEnergy,
}

impl TakeLabel {
    pub const ALL: [TakeLabel; 5] = [TakeLabel::Good, TakeLabel::Best, TakeLabel::Flat, TakeLabel::Stumble, TakeLabel::WrongEnergy];

    /// The camelCase name used in commands (`"wrongEnergy"`).
    pub fn name(self) -> &'static str {
        match self {
            TakeLabel::Good => "good",
            TakeLabel::Best => "best",
            TakeLabel::Flat => "flat",
            TakeLabel::Stumble => "stumble",
            TakeLabel::WrongEnergy => "wrongEnergy",
        }
    }

    pub fn from_name(s: &str) -> Option<TakeLabel> {
        TakeLabel::ALL.iter().copied().find(|l| l.name().eq_ignore_ascii_case(s.trim()))
    }
}

/// One pass at a line: a media-time range of the recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Take {
    /// Media time covered by this pass.
    pub range: TimeRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<TakeLabel>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl Default for Take {
    fn default() -> Self {
        Self { range: TimeRange::new(Tick::ZERO, Tick::ZERO), label: None, note: String::new() }
    }
}

impl Take {
    pub fn new(range: TimeRange) -> Self {
        Self { range, ..Default::default() }
    }
}

/// A line the speaker recorded more than once: its takes in media order. Which take is in the
/// cut is not stored; it follows from which take's media the timeline plays (`takes.list`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TakeGroup {
    /// Project-wide id (`Project::alloc_id`), stable across edits.
    pub id: u64,
    /// Takes in media order (at least one after normalisation).
    pub takes: Vec<Take>,
    /// Marked "needs re-record".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub redo: bool,
    /// Made or corrected by hand: kept when takes are detected again.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl TakeGroup {
    /// Media time from the first take's start to the last take's end.
    pub fn range(&self) -> Option<TimeRange> {
        let a = self.takes.iter().map(|t| t.range.start).min()?;
        let b = self.takes.iter().map(|t| t.range.end()).max()?;
        Some(TimeRange::from_bounds(a, b.max(a)))
    }
}

/// Longest note kept on a take or group.
const MAX_NOTE: usize = 2000;

/// The transcript of one media item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transcript {
    /// ISO 639-1 code of the spoken language (`"en"`).
    pub language: String,
    /// What produced it: a model id (`"whisper-base"`), `"imported"` or `"manual"`.
    pub source: String,
    pub speakers: Vec<Speaker>,
    /// Words in time order, non-overlapping.
    pub words: Vec<Word>,
    /// Lines recorded more than once (schema v13). Media time, so they survive trims and
    /// re-transcription.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub takes: Vec<TakeGroup>,
    /// Scenes assigned to media-time spans (schema v14): sorted, non-overlapping
    /// ([`crate::scene`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scenes: Vec<SceneSpan>,
}

impl Transcript {
    /// The speaker name of `word` (`"Speaker 2"` when the label list is short; `None` when the word
    /// has no speaker).
    pub fn speaker_name(&self, word: &Word) -> Option<String> {
        let i = word.speaker? as usize;
        Some(self.speakers.get(i).map(|s| s.name.clone()).unwrap_or_else(|| format!("Speaker {}", i + 1)))
    }

    /// The text of all words joined by spaces.
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// Indices of the words overlapping the media range.
    pub fn words_in(&self, r: TimeRange) -> std::ops::Range<usize> {
        let a = self.words.partition_point(|w| w.end <= r.start);
        let b = self.words.partition_point(|w| w.start < r.end());
        a..b.max(a)
    }

    /// Sort words by time and make the transcript well formed: empty words dropped, `end >= start`,
    /// no overlaps (a word ends at the next one's start at the latest), speaker indices in range
    /// (missing labels are added as "Speaker N").
    pub fn normalize(&mut self) {
        self.words.retain(|w| !w.text.trim().is_empty());
        for w in &mut self.words {
            w.text = w.text.trim().to_string();
            if w.end < w.start {
                w.end = w.start;
            }
            w.confidence = w.confidence.clamp(0.0, 1.0);
        }
        self.words.sort_by_key(|w| (w.start, w.end));
        for i in 1..self.words.len() {
            let s = self.words[i].start;
            if self.words[i - 1].end > s {
                self.words[i - 1].end = s.max(self.words[i - 1].start);
            }
        }
        let max = self.words.iter().filter_map(|w| w.speaker).max();
        if let Some(m) = max {
            while self.speakers.len() <= m as usize {
                let n = self.speakers.len() + 1;
                self.speakers.push(Speaker { name: format!("Speaker {n}") });
            }
        }
        self.normalize_takes();
        scene::normalize_spans(&mut self.scenes);
    }

    /// Takes sorted by start inside each group, empty or negative takes dropped, empty groups
    /// dropped, groups sorted by their first take, notes bounded.
    pub fn normalize_takes(&mut self) {
        for g in &mut self.takes {
            g.takes.retain(|t| t.range.duration > Tick::ZERO && t.range.start >= Tick::ZERO);
            g.takes.sort_by_key(|t| (t.range.start, t.range.duration));
            for t in &mut g.takes {
                if t.note.chars().count() > MAX_NOTE {
                    t.note = t.note.chars().take(MAX_NOTE).collect();
                }
            }
            if g.note.chars().count() > MAX_NOTE {
                g.note = g.note.chars().take(MAX_NOTE).collect();
            }
        }
        self.takes.retain(|g| !g.takes.is_empty());
        self.takes.sort_by_key(|g| (g.range().map(|r| r.start).unwrap_or(Tick::ZERO), g.id));
    }

    /// Assign `scene` to the media range: the spans it overlaps are trimmed (or split), then it is
    /// inserted in order.
    pub fn assign_scene(&mut self, scene: u64, range: TimeRange) {
        if range.duration <= Tick::ZERO || range.start < Tick::ZERO {
            return;
        }
        scene::cut_spans(&mut self.scenes, range);
        self.scenes.push(SceneSpan { scene, range });
        scene::normalize_spans(&mut self.scenes);
    }

    /// Remove scene spans from the media range (trimming and splitting the ones it overlaps).
    pub fn clear_scenes(&mut self, range: TimeRange) {
        scene::cut_spans(&mut self.scenes, range);
    }

    /// The scene assigned at media time `t`.
    pub fn scene_at(&self, t: Tick) -> Option<u64> {
        let i = self.scenes.partition_point(|s| s.range.start <= t);
        i.checked_sub(1).and_then(|j| self.scenes.get(j)).filter(|s| s.range.contains(t)).map(|s| s.scene)
    }

    /// The take group with this id.
    pub fn take_group(&self, id: u64) -> Option<&TakeGroup> {
        self.takes.iter().find(|g| g.id == id)
    }

    pub fn take_group_mut(&mut self, id: u64) -> Option<&mut TakeGroup> {
        self.takes.iter_mut().find(|g| g.id == id)
    }

    /// Indices of the words whose midpoint lies in the media range (what a take or a cut "says").
    pub fn words_within(&self, r: TimeRange) -> std::ops::Range<usize> {
        let cand = self.words_in(r);
        let mid = |w: &Word| Tick(w.start.0 + (w.end.0.saturating_sub(w.start.0)) / 2);
        let a = cand.start + self.words.get(cand.clone()).map(|ws| ws.iter().take_while(|w| mid(w) < r.start).count()).unwrap_or(0);
        let b = cand.end - self.words.get(cand.clone()).map(|ws| ws.iter().rev().take_while(|w| mid(w) >= r.end()).count()).unwrap_or(0);
        a..b.max(a)
    }

    /// Validate the invariants [`Transcript::normalize`] establishes.
    pub fn check(&self) -> Result<(), String> {
        for (i, w) in self.words.iter().enumerate() {
            if w.end < w.start {
                return Err(format!("word {i} ({:?}) ends before it starts", w.text));
            }
            if i > 0 && self.words[i - 1].end > w.start {
                return Err(format!("word {i} ({:?}) overlaps the word before it", w.text));
            }
            if let Some(s) = w.speaker
                && s as usize >= self.speakers.len()
            {
                return Err(format!("word {i} has speaker {s}, but there are {} speakers", self.speakers.len()));
            }
        }
        for (gi, g) in self.takes.iter().enumerate() {
            if g.takes.is_empty() {
                return Err(format!("take group {gi} (id {}) has no takes", g.id));
            }
            for (ti, t) in g.takes.iter().enumerate() {
                if t.range.start < Tick::ZERO || t.range.duration <= Tick::ZERO {
                    return Err(format!("take {ti} of group {} has an empty or negative range", g.id));
                }
            }
        }
        scene::check_spans(&self.scenes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(t: &str, a: i64, b: i64) -> Word {
        Word::new(t, Tick(a), Tick(b))
    }

    #[test]
    fn normalize_sorts_clamps_and_labels() {
        let mut t = Transcript { words: vec![w("b", 10, 30), w(" a ", 0, 15), w("", 40, 50), w("c", 35, 20)], ..Default::default() };
        t.words[0].speaker = Some(1);
        t.normalize();
        assert_eq!(t.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(t.words[0].end, Tick(10));
        assert_eq!(t.words[2].end, Tick(35));
        assert_eq!(t.speakers.len(), 2);
        assert_eq!(t.speaker_name(&t.words[1]).as_deref(), Some("Speaker 2"));
        t.check().unwrap();
    }

    #[test]
    fn words_in_range_and_normalized() {
        let t = Transcript { words: vec![w("Um,", 0, 10), w("Don't!", 10, 20), w("go", 25, 30)], ..Default::default() };
        assert_eq!(t.words_in(TimeRange::from_bounds(Tick(5), Tick(24))), 0..2);
        assert_eq!(t.words_in(TimeRange::from_bounds(Tick(20), Tick(25))), 2..2);
        assert_eq!(t.words[0].normalized(), "um");
        assert_eq!(t.words[1].normalized(), "don't");
        assert_eq!(t.text(), "Um, Don't! go");
    }
}

#[cfg(test)]
mod take_tests {
    use super::*;

    fn r(a: i64, b: i64) -> TimeRange {
        TimeRange::from_bounds(Tick(a), Tick(b))
    }

    #[test]
    fn takes_normalize_sort_drop_and_bound() {
        let mut t = Transcript::default();
        t.takes.push(TakeGroup {
            id: 7,
            takes: vec![Take::new(r(50, 60)), Take::new(r(10, 20)), Take::new(r(30, 30)), Take::new(r(-5, 5))],
            ..Default::default()
        });
        t.takes.push(TakeGroup { id: 3, takes: vec![], ..Default::default() });
        t.takes.push(TakeGroup {
            id: 1,
            takes: vec![Take { range: r(0, 5), label: Some(TakeLabel::Best), note: "x".repeat(5000) }],
            redo: true,
            ..Default::default()
        });
        t.normalize();
        assert_eq!(t.takes.iter().map(|g| g.id).collect::<Vec<_>>(), [1, 7]);
        assert_eq!(t.takes[1].takes.iter().map(|k| k.range.start.0).collect::<Vec<_>>(), [10, 50]);
        assert_eq!(t.takes[0].takes[0].note.chars().count(), MAX_NOTE);
        assert_eq!(t.takes[1].range(), Some(r(10, 60)));
        t.check().unwrap();
        // check rejects what normalize would have fixed
        let mut bad = Transcript::default();
        bad.takes.push(TakeGroup { id: 1, takes: vec![Take::new(r(5, 5))], ..Default::default() });
        assert!(bad.check().unwrap_err().contains("empty or negative"));
        bad.takes[0].takes.clear();
        assert!(bad.check().unwrap_err().contains("no takes"));
    }

    #[test]
    fn take_labels_round_trip_and_serde_is_compact() {
        for l in TakeLabel::ALL {
            assert_eq!(TakeLabel::from_name(l.name()), Some(l));
        }
        assert_eq!(TakeLabel::from_name(" WRONGENERGY "), Some(TakeLabel::WrongEnergy));
        assert_eq!(TakeLabel::from_name("meh"), None);
        let mut t = Transcript::default();
        t.words.push(Word::new("hi", Tick(0), Tick(10)));
        let plain = serde_json::to_string(&t).unwrap();
        assert!(!plain.contains("takes"), "{plain}");
        t.takes.push(TakeGroup {
            id: 2,
            takes: vec![Take { range: r(0, 10), label: Some(TakeLabel::WrongEnergy), note: String::new() }],
            ..Default::default()
        });
        let s = serde_json::to_string(&t).unwrap();
        assert!(s.contains("\"wrongEnergy\"") && !s.contains("\"redo\"") && !s.contains("\"note\""), "{s}");
        let back: Transcript = serde_json::from_str(&s).unwrap();
        assert_eq!(back, t);
        // older documents without takes still load; unknown labels are an error, not a crash
        let old: Transcript = serde_json::from_str(r#"{"language":"en","words":[]}"#).unwrap();
        assert!(old.takes.is_empty());
        assert!(old.scenes.is_empty());
        assert!(serde_json::from_str::<Transcript>(r#"{"takes":[{"id":1,"takes":[{"range":{"start":0,"duration":5},"label":"zzz"}]}]}"#).is_err());
    }

    #[test]
    fn words_within_uses_midpoints() {
        let t = Transcript {
            words: vec![Word::new("a", Tick(0), Tick(10)), Word::new("b", Tick(10), Tick(20)), Word::new("c", Tick(20), Tick(30))],
            ..Default::default()
        };
        assert_eq!(t.words_within(r(5, 25)), 0..2, "a (midpoint 5) is in, c (midpoint 25) is out");
        assert_eq!(t.words_within(r(0, 30)), 0..3);
        assert_eq!(t.words_within(r(12, 13)), 1..1);
        assert_eq!(t.words_within(r(100, 200)), 3..3);
    }
}

#[cfg(test)]
mod scene_tests {
    use super::*;

    fn r(a: i64, b: i64) -> TimeRange {
        TimeRange::from_bounds(Tick(a), Tick(b))
    }

    #[test]
    fn assign_trims_and_scene_at_finds() {
        let mut t = Transcript::default();
        t.assign_scene(1, r(0, 100));
        t.assign_scene(2, r(40, 60));
        assert_eq!(t.scenes.iter().map(|s| (s.scene, s.range.start.0, s.range.end().0)).collect::<Vec<_>>(), [(1, 0, 40), (2, 40, 60), (1, 60, 100)]);
        assert_eq!(t.scene_at(Tick(50)), Some(2));
        assert_eq!(t.scene_at(Tick(99)), Some(1));
        assert_eq!(t.scene_at(Tick(100)), None);
        t.clear_scenes(r(0, 50));
        assert_eq!(t.scene_at(Tick(10)), None);
        assert_eq!(t.scene_at(Tick(55)), Some(2));
        t.assign_scene(3, r(-5, 10));
        t.assign_scene(3, r(5, 5));
        assert_eq!(t.scenes.len(), 2, "hostile ranges are ignored");
        t.check().unwrap();
        let s = serde_json::to_string(&t).unwrap();
        let back: Transcript = serde_json::from_str(&s).unwrap();
        assert_eq!(back, t);
    }
}
