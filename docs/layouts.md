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

## Scenes (`scenes.*`)

A layout attached to a span of the transcript, so the arrangement follows the words when takes change.

## Tracked redaction (`redact.*`)

Draw a box over what to hide; a mosaic with a rectangle mask tracks it across the clip.
