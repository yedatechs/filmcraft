# Design: clip layouts, tracked redaction, pause threshold

Status: in progress. Owner: Jered (yedatechs/filmcraft, branch `feat/whisper-cpp-engine`).
Everything here follows `AGENTS.md`: no panics, integer ticks, every action is a command with an
automation id, every change one undo step.

## 1. Vocabulary

- **Frame**: the sequence's output picture, `settings.width × height` pixels.
- **Visible box** of a clip: the rectangle of the frame its picture covers after Motion (position,
  scale, anchor, rotation; `filmcraft_render::motion_matrix`) and after its *layout shape* (a mask
  named `Layout shape` on the Opacity effect, which hides everything outside the shape). For a clip
  without a layout shape the visible box is the whole transformed source.
- **Place**: where the visible box sits in the frame and how wide it is.
- **Shape**: circle, rounded rectangle, square or free (no layout mask).
- **Scene**: a named arrangement (place + shape per clip) attached to a span of the sequence
  transcript.

## 2. Engine: `layout.*` (`crates/engine/src/layout.rs`, menu Clip ▸ Layout)

All commands take `clips: [id]?` (default: the selected video clips; a graphic clip is an error) and
act at the playhead: when a Motion parameter is animated the edit adds or replaces a keyframe there,
exactly like `effects.setParam`. One undo step per command.

| Command | Params | Effect |
|---|---|---|
| `layout.place` | `at: topLeft\|topRight\|bottomLeft\|bottomRight\|top\|bottom\|left\|right\|center\|full`, `size: 25` (% of the frame width the visible box gets; ignored for `full`), `margin: 3` (% of the frame width from the edges) | Sets Motion `scale` (uniform; `scale_width` left alone, `uniform_scale` true) and `position` so the visible box lands there. `full` fits the whole source in the frame, centred (scale = min fit), shape untouched. Sizing uses the visible box, so a circle sized 25 % is a circle whose diameter is 25 % of the frame width. |
| `layout.shape` | `shape: circle\|rounded\|square\|free`, `radius: 12` (% of the visible box's shorter side, `rounded` only) | Replaces the clip's `Layout shape` mask on the Opacity effect (adds the effect's mask list entry; Opacity is intrinsic). `circle`: `MaskPath::ellipse` inscribed in the central square of the source. `square`: a 4-vertex path of the central square. `rounded`: a Bezier rounded rectangle of the whole source. `free`: removes the `Layout shape` mask (other masks stay). The clip's place is kept: after a shape change the visible box keeps its width and the edges its place touches (a bottom-right clip stays in the bottom-right corner; a custom box keeps its centre; Full stays Full). |
| `layout.swap` | `clips: [a, b]?` (default: the two top-most video clips visible at the playhead) | The two clips exchange place and shape (Motion position/scale and the `Layout shape` mask). |
| `layout.inspect` | `clips?` | Per clip: `{clip, at (preset name or "custom"), size, margin, shape, radius, box: [x, y, w, h]}` in frame pixels. Read-only; drives the UI's checkmarks and tests. |
| `layout.pick` | `x, y` (frame pixels), `time?` | The video clips whose visible box contains the point at the playhead, top track first: `{clips: [id…]}`. Read-only; the monitor click uses it and cycles on repeated clicks. |

Geometry lives in `filmcraft_edit::layout` (pure functions, unit-tested; points are `(f64, f64)`
because the edit crate does not depend on filmcraft_geom): `Pose`, `visible_box`, `place`, `place_box`,
`refit`, `shape_path` / `shape_of_path`, `nearest_place` / `infer_place`. `layout.swap` and `layout.pick`
are enabled with any open sequence (their defaults need no selection); the others need a selected or
passed video clip and refuse graphic clips. Known limits (rotation of non-circle shapes, static scale
when only position is animated, 1 % preset tolerance) are listed in `.frugal-fable/layout/REPORT.md`
and `docs/layouts.md`.
Engine tests: place every preset and read the box back through `layout.inspect`; circle then
place keeps the circle round; swap is an involution; undo restores; pick returns top-most first.

## 3. UI: Program monitor and menus (`crates/ui-egui/src/panels/layout.rs`)

- **Select on click.** With the Selection tool, a click on the Program picture that hits no graphic
  layer runs `layout.pick`; the top-most clip becomes the timeline selection
  (`state.selection`), a second click on the same spot cycles to the next clip in the stack. The
  timeline highlights it as usual.
- **Handles.** A selected non-graphic video clip whose span covers the playhead draws its visible
  box (accent stroke) with 8 handles. Dragging inside moves (`effects.setParam position`, `merge`
  so a drag is one undo step, snapping to the frame edges and centre through
  `monitor_view::snap`); corner handles scale uniformly about the opposite corner; Shift keeps the
  current corner anchoring. Automation ids `program.layout.box`, `program.layout.handle.{nw…se}`.
- **Right-click menu** on the box (and the same `Layout` submenu in the timeline clip menu and the
  Clip menu): Place ▸ nine positions + Full; Size ▸ 20 / 25 / 33 / 50 %; Shape ▸ Circle, Rounded,
  Square, Free; Swap With Clip Below; Redact Area…; the current values carry a check mark
  (`layout.inspect`). Ids `layout.menu.place.{at}`, `layout.menu.size.{n}`, `layout.menu.shape.{s}`,
  `layout.menu.swap`, `layout.menu.redact`.
- **Effect Controls.** The Transform section (and the Properties panel's Transform section, ids `properties.layout.*`) gets one row of nine small position buttons and four
  shape buttons (`effectControls.layout.{at|shape}.*`), tooltips with the command names.
- **Explanation.** A first-run hint in the status bar when a video clip is selected in the monitor:
  "Drag to move, corners to scale, right-click for layouts". `docs/layouts.md` and
  `docs/monitors.md` describe it.

Headless tests (`crates/ui-egui/tests/layout_ui.rs`): demo project, click the picture → selection
is the top clip; right-click → place bottom right → `layout.inspect` says `bottomRight`; drag the box
→ position changed and one undo step; Effect Controls button → shape circle.

## 4. Scenes (`crates/engine/src/scenes.rs`, project schema v14) — after §2 and §3 land

Contract in `specs/scenes/spec.md`. Decisions:

- **Model.** `Sequence::scenes: Vec<Scene>` with `Scene { id: u64, name, slots: Vec<SceneSlot> }`,
  `SceneSlot { item: ItemId (media item), hidden: bool, place: Place, size: f64, margin: f64, shape:
  Shape, radius: f64 }` (the `Place` / `Shape` ids from `filmcraft_edit::layout`, serialised as
  their camelCase names). `Transcript::scenes: Vec<SceneSpan>` with `SceneSpan { scene: u64, range:
  TimeRange (media time) }`, normalised like take groups (sorted, non-overlapping; a new span
  trims the ones it overlaps). Schema v14: `v13_to_v14` is a no-op (both fields default), with a
  `v13-minimal.fcproj` fixture and a load test, as `26972f7` did for v13.
- **Apply = recompute, not patch.** Position, scale, the `Layout shape` mask path and opacity of
  every clip whose media item appears in any scene are owned by the scenes: `scenes.apply` rebuilds
  those keyframe lists from the spans (hold interpolation, one keyframe per span start that
  intersects the clip, plus one at the clip's start). Users change the arrangement by changing the
  scene, not the clip; `docs/layouts.md` says so plainly and the Effect Controls Transform row shows
  "Set by scene <name>" for such clips. Clips of media items in no scene are never touched.
- **Media time → sequence time.** A span `[a, b)` on the transcript's item T maps to the union of
  `[clip.start + (a − clip.source_in) … ]` over the enabled clips of T, clipped to each clip; the
  slot keyframes go on every clip of each slot's item overlapping that sequence range.
- **Re-apply hook.** `Session::edit` gets a post-edit maintenance call: when the project's active
  sequence has scenes and the label is one of the transcript / takes edits (the modules pass a flag
  by calling `scenes::reapply(pr)` at the end of their edit closure; no new engine-wide hook), the
  scenes are re-applied inside the same closure, so one undo step covers both. Fable wires the
  calls in `transcript.rs` (`remove_ranges`, `restore`) and `takes.rs` (`select`, `cross`,
  `restore`).
- **Defaults.** `scenes.defaults` looks at the two top-most video tracks' first media items: the
  lower track is the screen (A), the upper the face (B). "Screen with face": A full, B circle
  bottom right 25 %. "Face": B full, A hidden. "Screen": A full, B hidden. "Half and half": A left
  50 % margin 0, B right 50 % margin 0. The dialog lets the user swap A and B.
- **UI.** The scene chip sits at the start of each paragraph in the Transcript tab (left of the
  speaker line), drawn in the scene's colour (a fixed palette by scene index). The dialog is a
  menu dialog (`ui.set {menuDialog: …}` settable) opened from the chip menu and from Sequence ▸
  Scenes…. Program monitor: nothing new; the handles from §3 keep working but a drag on a
  scene-owned clip asks "This clip's layout is set by the scene <name>. Edit the scene?" (Yes opens
  the dialog, No does nothing) rather than writing keyframes the next apply would discard.

## 5. Tracked redaction (`crates/engine/src/redact.rs`, `panels/redact.rs`)

| Command | Params | Effect |
|---|---|---|
| `redact.add` | `clip?` (default: the top-most video clip at the playhead), `rect: [x, y, w, h]` (clip pixels), `style: mosaic\|blur\|fill` (default mosaic), `track: true` | Applies the effect (`mosaic` with horizontal/vertical blocks sized so blocks are ≈ rect/6; `gaussian_blur` blurriness 60; `fill` = mosaic with one block), adds a rectangle mask named `Redaction N` on it, selects the mask, and when `track` starts `masks.track` forward from the playhead and then backward (two jobs, the second queued when the first finishes, method `position`). Returns `{clip, effect, mask, jobs}`. |
| `redact.list` | `clip?` | The clip's redactions `{effect, mask, style, rect, tracked}`. |
| `redact.remove` | `clip?, redaction: n` | Removes that effect (and so its mask). |

UI: `Redact Area…` in the monitor and clip menus enters a draw mode (status hint "Drag a box over
what to hide"); the drag draws a rectangle on the picture, release runs `redact.add`, Esc cancels.
Ids `program.redact.draw`. The running track jobs show in the status bar as every job does.
Effect Controls lists the effect as usual. Follow-up (not in this change): a scroll-following
tracker for text that scrolls vertically.

## 6. Pause threshold (Text panel)

- Engine `transcript.pauses {minSeconds=1.0, keepSeconds=0.15}` → `{count, seconds}` (read-only;
  `find_pauses` on the live words).
- Text panel toolbar button `text.transcript.removePauses` (Icon::Pause) opens a small dialog
  (`text.pauses.min`, `text.pauses.keep` drag values in seconds, live "14 pauses, 31.2 s", Apply
  `text.pauses.apply`, Cancel). Apply runs `transcript.removePauses` with those values; they are
  remembered in `UiState` (`transcript_pause_min`, `transcript_pause_keep`). The Sequence ▸
  Transcript ▸ Remove Pauses… menu entry opens the same dialog.

## 7. Follow-ups recorded

From the slices as built: the Program monitor scale drag changes two Motion parameters whose
`effects.setParam` merge keys differ, so the overlay trims the undo stack itself; a
`layout.set {clips, position, scale, merge}` engine command using `edit_merged` would make that a
proper single step. Shift-anchored scaling and click-on-empty-to-deselect are not done. Redaction
keeps queued backward tracking runs in a process-global list (`redact::PENDING`); it should live on
the `Session`. Scroll-following redaction tracker, recording, auto zoom.

Recording (screen + camera + mic, OBS-style per-source files, auto sync), click log + auto zoom,
animated layout transitions, scroll-following redaction tracker. See the owner's spec §11.
