# Clip layouts, scenes and redaction

Status: in progress (`openspec/changes/clip-layouts/`). This page is filled in as the pieces land.

## Layouts (`layout.*`)

One-click placement and shape for the video clips of a sequence: put the camera in a corner as a
circle over the screen recording, swap the two, or make one of them fill the frame, from the
Program monitor's right-click menu, the timeline clip menu, Clip ▸ Layout, or Effect Controls.
Every layout is an ordinary Motion edit plus an Opacity mask named `Layout shape`, so Effect
Controls shows exactly what changed and undo takes it back in one step.

### Commands

All commands take `clips: [id]?` (default: the selected video clips) and act at the playhead. A
graphic clip, an audio clip or a clip that is not in the active sequence is an error. Numbers are
clamped (`size` 1–100, `margin` 0–45, `radius` 0–50; a value that is not a number takes the
default). The three edits are one undo step each and are journaled; `inspect` and `pick` change
nothing. None of them has a menu path or shortcut of its own: the UI adds its Layout entries.

| Command | Params (defaults) | Effect | Undo label |
|---|---|---|---|
| `layout.place` | `at`: `topLeft` `topRight` `bottomLeft` `bottomRight` `top` `bottom` `left` `right` `center` `full`; `size` = 25 (% of the frame width); `margin` = 3 (% of the frame width) | Sets Motion `position` and uniform `scale` (`uniform_scale` on) so the clip's visible box has width `size` and sits `margin` from the edges of its place. `full` fits the whole source in the frame, centred, whatever the shape. | Place Clip |
| `layout.shape` | `shape`: `circle` `rounded` `square` `free`; `radius` = 12 (% of the shorter side, `rounded` only) | Replaces the `Layout shape` mask (`free` removes it; other masks stay), then re-places the clip: the box keeps its width and the edges its place touches (a bottom-right box stays bottom right with the same margin; a custom box keeps its centre; a full clip stays full). | Shape Clip |
| `layout.swap` | `clips: [a, b]?` (default: the two top-most enabled, non-graphic video clips whose span covers the playhead) | The two clips exchange place and shape: each gets the other's box (re-fitted to its own source the way `layout.shape` re-places) and layout mask. Swapping twice restores both. | Swap Layouts |
| `layout.inspect` | `clips?`, `time?` | `{clips: [{clip, at, size, margin, shape, radius, box: [x, y, w, h], track}]}`: `at` is the place the box is at (within 1 % of the frame width) or `"custom"`, `margin` the margin it implies (`null` for `center`, `full`, `custom`), `shape` `"custom"` for a hand-edited layout mask, `radius` `null` unless rounded. | – |
| `layout.pick` | `x`, `y` (frame pixels), `time?` | `{clips: [id…]}`: the enabled video clips (enabled tracks, graphics left out) whose span covers the time and whose visible box contains the point, top track first. | – |

### How a layout maps to Motion and the mask

- The **visible box** is the axis-aligned bounding box, in frame pixels, of the shape's bounds after
  Motion, computed exactly like `filmcraft_render::motion_matrix`: position (NaN = frame centre),
  anchor (NaN = source centre), scale and scale width, rotation, and Scale to Frame's fit factor. A
  circle's box is exact under rotation; a square or rounded rectangle's is the box of the rotated
  rectangle.
- Shapes are drawn in source pixels on the Opacity effect (which is enabled if it was off): `circle`
  is `MaskPath::ellipse` inscribed in the central square of the source, `square` that square (four
  corner vertices), `rounded` an eight-vertex Bézier rounded rectangle of the whole source. A new
  layout mask has no feather; one that exists keeps its feather, opacity, expansion and mode. The
  shape is read back from the mask path, so a mask edited by hand shows as `custom`.
- Placing writes `position`, `scale` and `uniform_scale` the way `effects.setParam` does: when a
  parameter is animated a keyframe is added or replaced at the playhead, otherwise its static value
  changes. Anchor, rotation and Scale to Frame are kept and taken into account. Motion is enabled.
- The geometry is pure and unit-tested in `filmcraft_edit::layout` (`place`, `place_box`, `refit`,
  `visible_box`, `shape_bounds`, `shape_path`, `shape_of_path`, `nearest_place`, `infer_place`).

### How the UI uses `inspect` and `pick`

- A click on the Program picture that hits no graphic runs `layout.pick` at the playhead; the first
  clip becomes the selection, a second click on the same spot moves to the next one in the list.
- The selected clip's box (`layout.inspect` → `box`) is drawn with handles; the Layout menus put
  check marks on the current place (`at`), size (`size`) and shape (`shape`, `radius`).

### In the app

The UI lives in `crates/ui-egui/src/panels/layout.rs`; every Layout action is the engine command
above, so it is one undo step.

- **Select by clicking the picture.** With the Selection tool, a click on the Program picture
  that hits no graphic layer (graphics take precedence) runs `layout.pick` at the playhead; the
  top-most clip becomes the selection (`state.selection`, the timeline highlights it). A click
  within 4 px of the previous one selects the next clip of the same stack, wrapping around. A
  click where no clip is leaves the selection alone. The first selection made this way in a
  session shows "Drag to move, corners to scale, right-click for layouts" in the status bar.
- **Box and handles.** The selected clip, when it is an enabled, non-graphic video clip whose
  span covers the playhead, shows its visible box (`layout.inspect` → `box`) in the accent colour
  with 8 square handles. Dragging inside the box moves the clip (Motion `position`), snapping its
  edges and centre to the frame edges, centre and guides when View ▸ Snap in Program Monitor is
  on (hold ⌘/Ctrl to move freely). A corner handle scales uniformly about the opposite corner, an
  edge handle about the middle of the opposite edge (Motion `scale`, and `position` so the fixed
  point stays; `scale_width` too when Uniform Scale is off). The status bar names the place the
  box is at (`layout.inspect` → `at`) while dragging. A whole drag is one undo step. The overlay
  is hidden while playing, while a mask is selected for editing and while the pen draws a mask.
- **Layout menu.** Right-click the box, open the timeline clip menu's **Layout** submenu, or use
  **Clip ▸ Layout**. All three show the same entries: Place ▸ (Top Left … Bottom Right, Full),
  Size ▸ (20 / 25 / 33 / 50 %), Shape ▸ (Circle, Rounded, Square, Free), Swap With Clip Below,
  Redact Area…. The current place, size and shape of the selected clip are checked. Place keeps
  the size and margin of a clip that is already in a place (a full-frame clip gets 25 % and 3 %).
  Size keeps the clip's place (a full-frame clip goes to the centre; a box at no place grows or
  shrinks about its centre through `effects.setParam scale`). Swap With Clip Below swaps the
  selected clip with the first visible video clip on a lower track (`layout.swap {clips: [a, b]}`);
  with nothing below, `layout.swap` picks the two top-most clips. Redact Area… only says it is not
  available yet. The menu-bar entries are UI commands (`ui.menu.invoke {id: "layout.menu.place.topLeft"}`);
  they take an optional `clips` param in place of the selection.
- **Effect Controls.** Under Motion (Effect Controls) and in the Transform section of the
  Properties panel, a **Layout** row has nine place buttons (a 3 × 3 dot grid; the clip's place is
  outlined) and four shape buttons; the tooltips name the command (`layout.place`,
  `layout.shape`).

| Automation id | What |
|---|---|
| `program.picture` | the Program picture (click to select) |
| `program.layout.box` | the selected clip's box (drag to move, right-click for the menu) |
| `program.layout.handle.{nw\|n\|ne\|e\|se\|s\|sw\|w}` | scale handles |
| `layout.menu.place`, `layout.menu.size`, `layout.menu.shape` | the submenus (right-click menu and timeline clip menu) |
| `layout.menu.place.{at}`, `layout.menu.size.{20\|25\|33\|50}`, `layout.menu.shape.{circle\|rounded\|square\|free}`, `layout.menu.swap`, `layout.menu.redact` | menu entries; also the UI command ids of Clip ▸ Layout |
| `timeline.clipMenu.layout` | the Layout submenu of the timeline clip menu |
| `effectControls.layout.place.{at}`, `effectControls.layout.shape.{s}` | the Effect Controls row (nine places, no Full) |
| `properties.layout.place.{at}`, `properties.layout.shape.{s}` | the same row in the Properties panel |

## Scenes (`scenes.*`)

A layout attached to a span of the transcript, so the arrangement follows the words when takes change.

## Tracked redaction (`redact.*`)

Draw a box over what to hide; a mosaic with a rectangle mask tracks it across the clip.

A redaction is an ordinary effect with one mask, so Effect Controls lists it, undo takes it back in
one step, and save, render and export need nothing new:

- `mosaic` (default): Mosaic with blocks about a sixth of the box (`horizontal` = source width ×
  6 / box width, `vertical` likewise, clamped to 1–4000);
- `blur`: Gaussian Blur, blurriness 60;
- `fill`: Mosaic with one block (1 × 1), so the box shows a flat colour.

The effect goes before the intrinsic Motion / Opacity (like Apply Effect) and gets a rectangle mask
named `Redaction N` (N = one more than the highest redaction number on the clip) with the tracking
method `Position`. The new mask is selected, so its on-monitor handles show.

| Command | Params | Result |
|---|---|---|
| `redact.add` | `clip?` (default: the top-most enabled video clip under the playhead), `rect: [x, y, w, h]` (clip pixels; clamped to the picture, an empty box is refused), `style: mosaic\|blur\|fill` (default mosaic), `track: bool` (default true), `frames: n?` (cap per direction), `wait: bool?` (track synchronously) | `{clip, effect, mask, name, style, rect, jobs, trackError?}` |
| `redact.list` | `clip?` | `{clip, redactions: [{redaction: N, name, effect, mask, style, rect, tracked, tracking}]}`; `rect` is the mask's bounding box at the playhead, `tracked` = the mask path has keyframes, `tracking` = a job is running or queued |
| `redact.remove` | `clip?`, `redaction: N` | removes that effect and its mask (one undo step) and stops its tracking |

With `track`, `masks.track` runs forward from the playhead (method `position`) and, when that job
finishes, backward from the same frame: two jobs, each one undo step (`Track Mask`), shown in the
status bar with cancel like every job. Cancelling the forward job skips the backward run. A
direction with nothing to track (the playhead on the clip's first or last frame) is skipped. While
a mask on a clip is being tracked, `redact.add` on that clip is refused (effect indices would
shift under the running job).

### Draw mode (Program monitor)

`redact.start {style?}` (Clip ▸ Layout ▸ Redact Area…; UI command) turns on the draw mode
(`UiState.redact_draw`, style in `UiState.redact_style`) and shows "Drag a box over what to hide
(Esc cancels)" in the status bar. The drag on the Program picture (automation id
`program.redact.draw`) draws a rubber band; on release the box is mapped from the screen through
the clip's Motion into source pixels of the top-most enabled video clip whose picture contains the
box centre (else the top-most clip under the playhead) and `redact.add {clip, rect, style, track:
true}` runs. One box per Redact Area…; a click without a drag keeps the mode; Esc cancels.

Follow-up (not in this change): a scroll-following tracker for text that scrolls vertically (the
`position` tracker follows rigid motion, not a scrolling page whose content changes).
