//! Scenes: named camera-and-screen arrangements attached to spans of the transcript
//! (`openspec/changes/clip-layouts/design.md` §4, `docs/layouts.md` "Scenes").
//!
//! A [`Scene`] lives on the sequence (`Sequence::scenes`) and says, per media item, where its clips
//! go (a [`SceneSlot`]: place, size, margin, shape, radius, or hidden). A [`SceneSpan`] lives on a
//! media transcript (`Transcript::scenes`) beside the take groups and names the scene for a
//! **media-time** range, so it keeps pointing at the same words however takes are switched or
//! text is cut. The engine (`filmcraft_engine::scenes`) turns spans into hold keyframes.
//!
//! Place and shape are stored by their command names (`"bottomRight"`, `"circle"`) so the file
//! stays readable; this crate does not depend on the layout geometry, the engine parses them with
//! `filmcraft_edit::layout::{Place, Shape}::parse`.

use filmcraft_time::{Tick, TimeRange};
use serde::{Deserialize, Serialize};

use crate::ItemId;

/// Longest scene name kept.
pub const MAX_SCENE_NAME: usize = 200;
/// Most slots a scene keeps.
pub const MAX_SLOTS: usize = 32;

/// Where one media item's clips go in a scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneSlot {
    /// The media item (the root media, as transcripts are keyed).
    pub item: ItemId,
    /// Hidden: opacity 0 while the scene is on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// A `layout.place` name (`"full"`, `"bottomRight"`…).
    pub place: String,
    /// % of the frame width the visible box gets (ignored for `full`).
    pub size: f64,
    /// % of the frame width from the edges.
    pub margin: f64,
    /// A `layout.shape` name (`"free"`, `"circle"`, `"rounded"`, `"square"`).
    pub shape: String,
    /// Corner radius for `rounded` (% of the shorter side).
    pub radius: f64,
}

impl Default for SceneSlot {
    fn default() -> Self {
        Self { item: ItemId(0), hidden: false, place: "full".into(), size: 25.0, margin: 3.0, shape: "free".into(), radius: 12.0 }
    }
}

/// A named arrangement of media items.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scene {
    /// Project-wide id (`Project::alloc_id`), stable across edits.
    pub id: u64,
    pub name: String,
    /// At most one slot per media item.
    pub slots: Vec<SceneSlot>,
}

impl Scene {
    /// The slot of a media item.
    pub fn slot(&self, item: ItemId) -> Option<&SceneSlot> {
        self.slots.iter().find(|s| s.item == item)
    }

    /// Name bounded and trimmed, one slot per item (the first wins), at most [`MAX_SLOTS`], numbers
    /// finite.
    pub fn normalize(&mut self) {
        self.name = self.name.trim().chars().take(MAX_SCENE_NAME).collect();
        let mut seen = Vec::new();
        self.slots.retain(|s| {
            if seen.contains(&s.item) {
                false
            } else {
                seen.push(s.item);
                true
            }
        });
        self.slots.truncate(MAX_SLOTS);
        for s in &mut self.slots {
            let fin = |v: f64, d: f64| if v.is_finite() { v } else { d };
            s.size = fin(s.size, 25.0);
            s.margin = fin(s.margin, 3.0);
            s.radius = fin(s.radius, 12.0);
        }
    }
}

/// A scene assigned to a media-time range of one transcript.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneSpan {
    /// [`Scene::id`].
    pub scene: u64,
    /// Media time of the transcript's item.
    pub range: TimeRange,
}

/// `r.end()` without overflow.
pub fn range_end(r: &TimeRange) -> Tick {
    Tick(r.start.0.saturating_add(r.duration.0.max(0)))
}

/// Remove `cut` from the spans: a span it covers goes, one it overlaps is trimmed, one it falls
/// inside is split in two.
pub fn cut_spans(spans: &mut Vec<SceneSpan>, cut: TimeRange) {
    let (ca, cb) = (cut.start, range_end(&cut));
    if cb <= ca {
        return;
    }
    let mut out = Vec::with_capacity(spans.len() + 1);
    for s in spans.drain(..) {
        let (a, b) = (s.range.start, range_end(&s.range));
        if b <= ca || a >= cb {
            out.push(s);
            continue;
        }
        if a < ca {
            out.push(SceneSpan { scene: s.scene, range: TimeRange::from_bounds(a, ca) });
        }
        if b > cb {
            out.push(SceneSpan { scene: s.scene, range: TimeRange::from_bounds(cb, b) });
        }
    }
    *spans = out;
}

/// Spans well formed: empty or negative ranges dropped, sorted by start, and where two overlap the
/// later-starting one wins (the earlier one ends where it starts).
pub fn normalize_spans(spans: &mut Vec<SceneSpan>) {
    spans.retain(|s| s.range.duration > Tick::ZERO && s.range.start >= Tick::ZERO);
    for s in spans.iter_mut() {
        s.range = TimeRange::from_bounds(s.range.start, range_end(&s.range));
    }
    spans.sort_by_key(|s| (s.range.start, s.range.duration, s.scene));
    for i in 1..spans.len() {
        let Some(next_start) = spans.get(i).map(|s| s.range.start) else { continue };
        if let Some(prev) = spans.get_mut(i - 1)
            && range_end(&prev.range) > next_start
        {
            prev.range = TimeRange::from_bounds(prev.range.start, next_start);
        }
    }
    spans.retain(|s| s.range.duration > Tick::ZERO);
}

/// Validate what [`normalize_spans`] establishes.
pub fn check_spans(spans: &[SceneSpan]) -> Result<(), String> {
    for (i, s) in spans.iter().enumerate() {
        if s.range.start < Tick::ZERO || s.range.duration <= Tick::ZERO {
            return Err(format!("scene span {i} has an empty or negative range"));
        }
        if let Some(prev) = i.checked_sub(1).and_then(|j| spans.get(j))
            && range_end(&prev.range) > s.range.start
        {
            return Err(format!("scene span {i} overlaps the span before it"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(a: i64, b: i64) -> TimeRange {
        TimeRange::from_bounds(Tick(a), Tick(b))
    }
    fn sp(scene: u64, a: i64, b: i64) -> SceneSpan {
        SceneSpan { scene, range: r(a, b) }
    }

    #[test]
    fn cut_trims_splits_and_drops() {
        let mut v = vec![sp(1, 0, 10), sp(2, 10, 20), sp(3, 20, 40)];
        cut_spans(&mut v, r(5, 25));
        assert_eq!(v, [sp(1, 0, 5), sp(3, 25, 40)]);
        let mut v = vec![sp(1, 0, 100)];
        cut_spans(&mut v, r(40, 60));
        assert_eq!(v, [sp(1, 0, 40), sp(1, 60, 100)]);
        cut_spans(&mut v, r(10, 10));
        assert_eq!(v.len(), 2, "an empty cut changes nothing");
    }

    #[test]
    fn normalize_sorts_and_resolves_overlaps() {
        let mut v = vec![sp(2, 30, 50), sp(1, 0, 40), sp(9, 5, 5), sp(8, -10, 5), SceneSpan { scene: 7, range: TimeRange::new(Tick(60), Tick(i64::MAX)) }];
        normalize_spans(&mut v);
        assert_eq!(v[0], sp(1, 0, 30));
        assert_eq!(v[1], sp(2, 30, 50));
        assert_eq!(v[2].range.start, Tick(60));
        check_spans(&v).unwrap();
        assert!(check_spans(&[sp(1, 0, 10), sp(2, 5, 20)]).unwrap_err().contains("overlaps"));
        assert!(check_spans(&[sp(1, 3, 3)]).unwrap_err().contains("empty"));
    }

    #[test]
    fn scene_normalize_bounds_and_dedups() {
        let mut s = Scene { id: 1, name: format!("  {}  ", "x".repeat(500)), slots: vec![] };
        s.slots.push(SceneSlot { item: ItemId(3), size: f64::NAN, ..Default::default() });
        s.slots.push(SceneSlot { item: ItemId(3), hidden: true, ..Default::default() });
        s.normalize();
        assert_eq!(s.name.chars().count(), MAX_SCENE_NAME);
        assert_eq!(s.slots.len(), 1);
        assert_eq!(s.slots[0].size, 25.0);
        assert!(s.slot(ItemId(3)).is_some_and(|x| !x.hidden));
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"place\":\"full\"") && !json.contains("hidden"), "{json}");
        let back: Scene = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
