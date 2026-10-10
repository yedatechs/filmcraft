# Design: take editing in FilmCraft

Read first: `AGENTS.md` (never crash, no `unwrap`/`expect`/`panic`/`unsafe`, hostile inputs, exact `Tick` time), `docs/architecture.md` §1 (layers: `project` L1, `edit`/`speech` L2, `engine` L4, `ui-egui` L5; `cargo xtask layers` enforces), `docs/transcripts.md`, `docs/contributing.md` §6 (how to add a command). Owner-facing product spec: `~/BETTERCLAW/takes-editor/SPEC.md` (not in the repo).

## 1. Principles

- **The timeline is the truth.** The sequence transcript is derived from clips today (`filmcraft_edit::transcript::sequence_words`) and stays that way. "Crossed out" is not a stored flag: it is media of a clip that the timeline no longer plays. Restoring is a timeline edit. Undo and redo come for free, interchange export needs nothing new, and the Text panel can never disagree with the timeline.
- **Takes are the one stored layer**, on the media transcript (`Transcript::takes`), as media `TimeRange`s so they survive trims, moves and re-transcription. Which take is *active* is derived (which take's media is live).
- **Everything is a command** in `crates/engine`, registered with id, label, menu path, params doc and `enabled()` reason. The UI only dispatches commands.
- **Pure logic lives in `crates/edit`** (functions on `&Sequence` / `&mut Sequence` with unit tests, no engine types). The engine wraps them in `Session::edit_sequence` / `Session::edit`.

## 2. Data model (`crates/project/src/transcript.rs`)

```rust
pub struct Transcript { …, #[serde(default, skip_serializing_if = "Vec::is_empty")] pub takes: Vec<TakeGroup> }
pub struct TakeGroup { pub id: u64, pub takes: Vec<Take>, pub redo: bool, pub manual: bool, pub note: String }
pub struct Take { pub range: TimeRange /* media */, pub label: Option<TakeLabel>, pub note: String }
pub enum TakeLabel { Good, Best, Flat, Stumble, WrongEnergy }   // serde camelCase
```

`Transcript::normalize` sorts takes by start inside a group, drops takes with non-positive duration, drops groups with fewer than one take, sorts groups by first take, clamps notes to 2000 chars. `Transcript::check` rejects takes whose range starts before 0. Groups may overlap each other (detection avoids it; manual edits are the user's call). Ids come from `Project::alloc_id()`.

Schema: `crates/format` gets `v12_to_v13` (no-op) and a `v12-minimal.fcproj` fixture, so a build without take support refuses a v13 file (`docs/project-files.md`, "silently drop new fields on its next save").

## 3. Cut spans (`crates/edit/src/transcript.rs`)

```rust
pub struct CutSpan {
    pub item: ItemId,          // media item
    pub media: TimeRange,      // media time not played
    pub at: Tick,              // sequence time where it goes back (end of `before`)
    pub track: usize,          // audio track index (0 = A1)
    pub before: ClipId,        // clip ending at `at`
    pub after: ClipId,         // next clip of the same media on that track
    pub words: std::ops::Range<usize>,   // indices into the media transcript (midpoint inside `media`)
    pub after_word: Option<usize>,       // index in `sequence_words` of the last live word with start < at
}
pub fn cut_spans(seq: &Sequence, transcripts: &Transcripts, live: &[SeqWord]) -> Vec<CutSpan>
```

Per audio track, items in start order, same eligibility as `sequence_words` (enabled, not reversed, no frame hold, speed > 0). For consecutive items `a`, `b` with `a.item == b.item` and a transcript for that item: `media_end(a) = a.source_in + round(a.duration * a.speed)`; if `b.source_in > media_end(a)` the span is `[media_end(a), b.source_in)`. Head and tail trims (media before the first clip / after the last) are **not** spans in this change (they would list a whole source file); a later change can add a toggle. Sorted by `(at, track)`.

```rust
/// Timeline ranges on which `media` of `item` is played by eligible clips on the audio tracks.
pub fn live_ranges(seq: &Sequence, item: ItemId, media: TimeRange) -> Vec<TimeRange>
/// Put media back at `at`. Returns the restored timeline range.
pub fn restore_media(seq: &mut Sequence, item: ItemId, media: TimeRange, at: Tick, track: usize, ctx: &mut EditCtx) -> Result<TimeRange>
pub fn restore_cut(seq: &mut Sequence, span: &CutSpan, ctx: &mut EditCtx) -> Result<TimeRange>  // = restore_media(span.item, span.media, span.at, span.track)
```

`restore_media` is the inverse of `extract` for one media range. `dur = media.duration` (speed 1.0 is assumed for the restored material; a clip before it with speed ≠ 1 gets a new clip instead of growing). For every unlocked track (video, audio) and sync-locked caption track:

1. **Grow** every item with `it.item == item`, `it.end() == at`, `it.speed == 1.0`, not reversed, no frame hold, and `media_end(it) == media.start`: `it.duration += dur`. On the audio track `track` this is the clip before the span; on the video track it is the linked picture. Remember whether the audio track grew a clip.
2. **Lengthen or split** every other item with `it.start < at < it.end()`: if `it.speed == 1.0`, not reversed, and `media_end(it) + dur <= ctx.media_duration(it.item)` (None = unlimited) then `it.duration += dur` (B-roll and music under the restored speech come back whole, which is what Extract took from them); else `split_track_at(at)` and the right piece shifts.
3. **Shift** every item with `it.start >= at` by `+dur` (`shift_track_from`); caption tracks with `captions::shift_from`.
4. If step 1 grew nothing on the audio track `track` (partial restore, speed mismatch, or the clip before was deleted), **insert** a new audio item cloned from `before` (new `ClipId`, `start = at`, `source_in = media.start`, `duration = dur`, `speed = 1.0`, `reverse = false`, `frame_hold = None`, same `link`) on that track, and for each video track whose item ends at `at` with `link == before.link` (and `link.is_some()`) a cloned video item likewise; the gap for them was already opened by step 3. Use `edit::insert` only if it fits this exactly; otherwise place directly after the shift (the range is free by construction).
5. Transitions: `remove_orphan_transitions` on touched tracks.

Errors (`EditError`): no such track, `before` missing from the track, `media` empty or starting before `ctx.media_start(item)` or ending after `ctx.media_duration(item)`, `dur < ctx.min_duration`.

Tests (`crates/edit/src/transcript/tests.rs`, interview fixture): extract then `cut_spans` finds the words; restore returns one contiguous clip and original positions; partial restore inserts a clip; remove-pauses yields wordless spans; locked track unmoved; B-roll on V2 spanning the cut is lengthened; music whose media is exhausted is split; linked video grows with audio; speed-2 clip before gets an insert; property: extract(r) then restore(span) round-trips clip starts and durations for random word ranges.

## 4. Take detection (`crates/edit/src/takes.rs`)

```rust
pub struct DetectParams { pub sensitivity: f32 /* 0..1, 0.5 */, pub pause: Tick /* 0.5 s */, pub max_gap: Tick /* 12 s */, pub window: usize /* 4 utterances */, pub min_words: usize /* 2 */ }
pub fn utterances(words: &[Word], pause: Tick) -> Vec<std::ops::Range<usize>>   // split at a gap >= pause or after a word ending in . ? !
pub fn similarity(a: &[String], b: &[String]) -> f32   // 0..1 on normalised words
pub fn detect(t: &Transcript, p: &DetectParams) -> Vec<TakeGroup>   // ids 0; the engine assigns
pub fn merge(groups: &mut Vec<TakeGroup>, ids: &[u64]) -> Option<u64>
pub fn split(groups: &mut Vec<TakeGroup>, id: u64, at_take: usize, new_id: u64) -> bool
pub fn live_fraction(seq_live: &[TimeRange], take: &TimeRange) -> f32   // helper for "live" (≥ 0.5)
```

`similarity` = max of: opening overlap (how many of the first `min(4, len)` normalised words match in order, divided by that count), token Jaccard, and `1 - levenshtein(words) / max(len)`. Prefix relation: if `a` (≥ `min_words` words) equals the opening of `b`, similarity is 1 (false start). Retake cues at the start of `b` (`okay`, `ok`, `again`, `sorry`, `let`, `take`, `one more`, `redo`, `wait`; lead-ins `yo`, `alright`, `right`, `um`, `uh` are stripped the same way) are stripped before comparing and lower the threshold by 0.15. Threshold = `0.85 - 0.5 * sensitivity` (clamped 0.2..0.95). Greedy chaining: utterance `i` compares with `i+1..=i+window` whose start is within `max_gap` of `i`'s end; a match joins `j` to `i`'s group and continues from `j`. Restarts without a pause: within an utterance (stutters collapsed), a phrase of ≥ 2 words said again at most 10 words later (a 2-word phrase at most 6 later; not right after a joining word like `and`/`then`) splits the utterance before both occurrences; the parts form one unit whose parts are takes, and units chain like utterances (last part against the other unit's first and last parts). At most 64 restarts per utterance. Groups need ≥ 2 takes. Deterministic; bounded by `window`. All indexing with `get`.

Tests: exact repeat, reworded repeat, false start, cue words, unrelated lines not grouped, `max_gap` respected, higher sensitivity never yields fewer groups than lower on the same input, empty transcript, one word, merge/split round trip, hostile params (NaN sensitivity, zero window).

## 5. Engine (`crates/engine/src/takes.rs`, additions to `transcript.rs`)

Commands (all `journal: true` except queries):

| id | params | does |
|---|---|---|
| `transcript.cuts` | `{}` | query: spans with `index`, `item`, `start`/`end` (media ticks), `at`, `seconds`, `track`, `afterWord`, `words: [{i, text}]` |
| `transcript.restore` | `{cut}` or `{item, start, end, at?}` | `restore_cut` / `restore_media`; "Restore Text" |
| `takes.detect` | `{items?, sensitivity?, select?: "last"\|"first"\|"none"}` | detect on each media item of the sequence (or `items`), keep manual groups, assign ids, store; then `select` the default take of every new group; one undo step "Detect Takes" |
| `takes.list` | `{label?, redo?}` | query: groups of the sequence's media with takes (`index`, `start`, `end`, `seconds`, `text`, `label`, `note`, `live`), `active`, `redo`, `manual`, `at` (timeline position of the group's first live take or of the cut span holding it) |
| `takes.select` | `{group, take}` | extract live takes of the group (`live_ranges`, right to left), restore the chosen take at the first extracted position (or at the cut span anchor if none was live); "Switch Take" |
| `takes.next` / `takes.previous` | `{group?}` | group = the one whose live take contains the playhead word, else the selection; wraps |
| `takes.cross` | `{group, take}` | extract that take's live ranges; "Cross Out Take" |
| `takes.restore` | `{group, take}` | restore the take at its anchor (keeps others); "Restore Take" |
| `takes.label` | `{group, take, label?, note?}` | `label: null` clears |
| `takes.redo` | `{group, redo}` | flag |
| `takes.merge` | `{groups: [id…]}` | `takes::merge`; result manual |
| `takes.split` | `{group, at}` | `takes::split` |
| `takes.add` | `{item, start, end, group?}` | manual take; new group when `group` absent |
| `takes.remove` | `{group, take?}` | take, or whole group |
| `takes.preview` | `{group, take, pre?: 1, post?: 1}` | mark In/Out from the start of the `pre`-th live sentence before the take to the end of the `post`-th after, playhead at In; the UI then runs `playback.inToOut` |

Groups are addressed by **id** in params (`group`), takes by index within the group. `enabled`: `has_transcript` for all; `takes.*` except `detect`/`add` also need at least one group. Hostile-parameter tests for every command (`crates/engine/src/takes_tests.rs`), plus: detect → select → next → undo → redo on the demo project with a `FixedTranscriber` transcript that repeats a line.

Shortcuts (`docs/keyboard.md`; X, U, L, G are taken): `takes.next` `]`, `takes.previous` `[`, `takes.cross` `Alt+X`, `takes.restore` `Alt+U`, `takes.label` `Alt+L` (UI opens the label picker), `takes.redo` `Alt+R`, `takes.detect` `Alt+Shift+T`. Menu path `["Sequence", "Takes"]`.

## 6. UI (`crates/ui-egui/src/panels/text.rs`)

- Transcript tab: after live word `after_word` of each cut span, draw its words with `RichText::strikethrough()` in `t.text_dim`; wordless spans as `⋯ 1.2 s` struck. Click → `transcript.restore {cut}`. Automation ids `text.transcript.cut.{index}`.
- Take groups: words of a live take get a 1 px underline in the accent colour; before the group's first word a chip `Take 2/3` (click: `takes.next`, Shift+click: `takes.previous`, right-click: label menu). Crossed-out takes of the group render as cut spans already.
- A **Takes** list in the Text panel (toggle button in the toolbar): one row per group (time, text of the active take, `n takes`, label, redo flag), expandable to its takes with play (`takes.preview` then `playback.inToOut`), select, label, cross/restore. Filter: redo only. Automation ids `text.takes.*`.
- Toolbar gains Detect Takes and Restore All Cuts (`transcript.cuts` then `transcript.restore` right to left).
- No new assets. Keyboard: the engine shortcuts above.

## 7. Delivery order

1. Data model + schema v13 (project, format).
2. `edit::transcript` cut spans + restore (tests) and `edit::takes` detection (tests): independent, parallel.
3. Engine commands + tests; `docs/transcripts.md`, `docs/keyboard.md`, `docs/control-protocol.md`.
4. Text panel.
5. Owner acceptance on a real recording (`SPEC.md` §8).

Gates before every commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p <touched crates>`, `cargo xtask layers`, `cargo xtask wasm` when L0–L4 crates change.
