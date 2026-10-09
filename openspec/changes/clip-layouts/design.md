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
| `layout.shape` | `shape: circle\|rounded\|square\|free`, `radius: 12` (% of the visible box's shorter side, `rounded` only) | Replaces the clip's `Layout shape` mask on the Opacity effect (adds the effect's mask list entry; Opacity is intrinsic). `circle`: `MaskPath::ellipse` inscribed in the central square of the source. `square`: a 4-vertex path of the central square. `rounded`: a Bezier rounded rectangle of the whole source. `free`: removes the `Layout shape` mask (other masks stay). The clip's place is kept: after a shape change the visible box is re-placed where its centre was, same width. |
| `layout.swap` | `clips: [a, b]?` (default: the two top-most video clips visible at the playhead) | The two clips exchange place and shape (Motion position/scale and the `Layout shape` mask). |
| `layout.inspect` | `clips?` | Per clip: `{clip, at (preset name or "custom"), size, margin, shape, radius, box: [x, y, w, h]}` in frame pixels. Read-only; drives the UI's checkmarks and tests. |
| `layout.pick` | `x, y` (frame pixels), `time?` | The video clips whose visible box contains the point at the playhead, top track first: `{clips: [id…]}`. Read-only; the monitor click uses it and cycles on repeated clicks. |

Geometry lives in `filmcraft_edit::layout` (pure functions, unit-tested): `visible_box`,
`place_transform(frame, source, shape, at, size, margin) -> (position, scale)`, `nearest_preset`.
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
- **Effect Controls.** The Transform section gets one row of nine small position buttons and four
  shape buttons (`effectControls.layout.{at|shape}.*`), tooltips with the command names.
- **Explanation.** A first-run hint in the status bar when a video clip is selected in the monitor:
  "Drag to move, corners to scale, right-click for layouts". `docs/layouts.md` and
  `docs/monitors.md` describe it.

Headless tests (`crates/ui-egui/tests/layout_ui.rs`): demo project, click the picture → selection
is the top clip; right-click → place bottom right → `layout.inspect` says `bottomRight`; drag the box
→ position changed and one undo step; Effect Controls button → shape circle.

## 4. Scenes (`crates/engine/src/scenes.rs`, project schema v14) — after §2 and §3 land

A scene is `{id, name, label colour, clips: [{item (media item), place, shape}]}` on the sequence
(`Sequence::scenes`), plus assignments `{scene, item (transcript's media item), media range}`
stored beside take groups on the transcript, so they move with the words. `scenes.apply` writes
Motion keyframes (hold interpolation) at the start of each assigned span on the clips showing that
media; `scenes.assign {scene, from, to}` (word indices) and `scenes.clear`. The Text panel shows a
scene strip above each paragraph; clicking a scene chip switches that span. Defaults seeded from
the owner's preference: "Screen with face" (screen full, camera circle bottom right 25 %),
"Face" (camera full), "Screen" (screen full), "Half and half". Detailed spec to follow in
`specs/scenes/spec.md` once §2 is in; this section records the decision.

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

Recording (screen + camera + mic, OBS-style per-source files, auto sync), click log + auto zoom,
animated layout transitions, scroll-following redaction tracker. See the owner's spec §11.
