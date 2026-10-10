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
default). The edits are one undo step each and are journaled; `inspect` and `pick` change
nothing. None of them has a menu path or shortcut of its own: the UI adds its Layout entries.

| Command | Params (defaults) | Effect | Undo label |
|---|---|---|---|
| `layout.place` | `at`: `topLeft` `topRight` `bottomLeft` `bottomRight` `top` `bottom` `left` `right` `center` `full`; `size` = 25 (% of the frame width); `margin` = 3 (% of the frame width) | Sets Motion `position` and uniform `scale` (`uniform_scale` on) so the clip's visible box has width `size` and sits `margin` from the edges of its place. `full` fits the whole source in the frame, centred, whatever the shape. | Place Clip |
| `layout.shape` | `shape`: `circle` `rounded` `square` `free`; `radius` = 12 (% of the shorter side, `rounded` only) | Replaces the `Layout shape` mask (`free` removes it; other masks stay), then re-places the clip: the box keeps its width and the edges its place touches (a bottom-right box stays bottom right with the same margin; a custom box keeps its centre; a full clip stays full). | Shape Clip |
| `layout.swap` | `clips: [a, b]?` (default: the two top-most enabled, non-graphic video clips whose span covers the playhead) | The two clips exchange place, shape and pan (each gets the other's box, re-fitted to its own source the way `layout.shape` re-places, and layout mask; the pan is clamped to the new source) **and their tracks over the time they overlap**, so the clip that was on top is now underneath (otherwise a full-frame clip on V2 would still cover the circle on V1). A clip that extends beyond the overlap is first split at its bounds on its own track only, like a razor on that track (linked audio is not split; a right-hand piece gets a link group of its own). The pieces inside the overlap keep their start, duration and source in, get the other clip's layout and move to the other clip's track; the pieces outside keep their track and layout. A transition of a moving piece alone (a fade) goes with it, one shared with a clip that stays is removed; audio tracks are untouched. Refused when the clips are on the same track, do not overlap in time, or a track is locked. → `{clips: [a', b'], moved: [[fromTrack, toTrack], …], range: [start, end]}` (`a'`, `b'` the pieces inside the overlap, tracks counted from 0 = V1 like `inspect`'s `track`); a selected clip stays selected as its piece. Swapping twice restores both layouts and tracks; the edit points of the splits remain. | Swap Layouts |
| `layout.pan` | `clips?`, `dx?`, `dy?` (source pixels), `merge?`, `begin?` | Sets the pan of the clips' circle or square: the shape moves by `(dx, dy)` inside the source (a missing component keeps its value, one that is not a finite number counts as 0), clamped so it stays inside the source (a 3840 × 2160 circle pans ±840 px sideways and not at all vertically). The visible box stays where it is: Motion `position` moves by the opposite of the pan, rotated and scaled like the picture, so the picture slides under the shape. Refused for a clip without a circle or square shape. `merge` / `begin` as in `layout.set` (the Alt-drag is one undo step). → `{clips, pan: [[dx, dy]…]}` | Pan Clip |
| `layout.set` | `clips?`, `position: [x, y]?`, `scale?`, `scaleWidth?`, `merge?`, `begin?` | Motion position / scale of the clips at the playhead (keyframes when animated). `merge` folds consecutive calls into one undo step and `begin` starts a new one: the Program monitor's move and scale drags use it, so a drag is one undo step even though it changes several parameters. | Transform Clip |
| `layout.inspect` | `clips?`, `time?` | `{clips: [{clip, at, size, margin, shape, radius, pan: [dx, dy], box: [x, y, w, h], track}]}`: `at` is the place the box is at (within 1 % of the frame width) or `"custom"`, `margin` the margin it implies (`null` for `center`, `full`, `custom`), `shape` `"custom"` for a hand-edited layout mask, `radius` `null` unless rounded, `pan` the shape's pan inside the source (`[0, 0]` for free, rounded and custom). | – |
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
- The **pan** moves a circle or square inside the source (the face on the left third of a camera
  frame): the mask path is the canonical shape translated by `(dx, dy)` source pixels, clamped so
  the shape's bounds stay inside the source. It lives in the mask path (nothing new is saved) and
  is read back from it: a path that is a translated circle or square within 0.5 px gives its shape
  and pan, anything else is `custom`. The visible box is the box of the panned shape, so
  `layout.place`, `layout.shape` (which keep the pan) and `layout.swap` (which carries it to the
  other clip) put the box where they always do whatever the pan. Clips arranged by a scene are
  never panned (a scene slot has no pan; `scenes.*` writes pan 0).
- Placing writes `position`, `scale` and `uniform_scale` the way `effects.setParam` does: when a
  parameter is animated a keyframe is added or replaced at the playhead, otherwise its static value
  changes. Anchor, rotation and Scale to Frame are kept and taken into account. Motion is enabled.
- The geometry is pure and unit-tested in `filmcraft_edit::layout` (`place`, `place_box`, `refit`,
  `visible_box`, `shape_bounds`, `shape_path`, `shape_of_path`, `clamp_pan`, `pan_position`,
  `source_delta`, `nearest_place`, `infer_place`).

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
  session shows "Drag to move, corners to scale, Alt-drag to pan inside a shape, right-click for
  layouts" in the status bar.
- **Box and handles.** The selected clip, when it is an enabled, non-graphic video clip whose
  span covers the playhead, shows its visible box (`layout.inspect` → `box`) in the accent colour
  with 8 square handles. Dragging inside the box moves the clip (Motion `position`), snapping its
  edges and centre to the frame edges, centre and guides when View ▸ Snap in Program Monitor is
  on (hold ⌘/Ctrl to move freely). A corner handle scales uniformly about the opposite corner, an
  edge handle about the middle of the opposite edge (Motion `scale`, and `position` so the fixed
  point stays; `scale_width` too when Uniform Scale is off). The status bar names the place the
  box is at (`layout.inspect` → `at`) while dragging. **Alt/Option-drag inside the box pans the
  picture** inside a circle or square: the box stays put and the picture slides under it with the
  pointer (`layout.pan` with `merge`; the status bar shows "Pan: dx, dy px"; a clip without a
  circle or square says so in the status bar). Clips arranged by a scene refuse moves, scales and
  pans alike. A whole drag is one undo step. The overlay
  is hidden while playing, while a mask is selected for editing and while the pen draws a mask.
- **Layout menu.** Right-click the box, open the timeline clip menu's **Layout** submenu, or use
  **Clip ▸ Layout**. All three show the same entries: Place ▸ (Top Left … Bottom Right, Full),
  Size ▸ (20 / 25 / 33 / 50 %), Shape ▸ (Circle, Rounded, Square, Free), Pan ▸ (Centre on Left
  Third, Centre, Centre on Right Third), Swap With Clip Below, Redact Area ▸ (Static Mosaic…,
  Static Blur…, Static Fill…, Tracked Mosaic…, Tracked Blur…, Tracked Fill…). The current place,
  size and shape of the selected clip are checked. Pan ▸ centres the shape on x = ⅓, ½ or ⅔ of
  the source width, keeping the vertical pan (`layout.pan {dx}`; enabled for a circle or square). Place keeps
  the size and margin of a clip that is already in a place (a full-frame clip gets 25 % and 3 %).
  Size keeps the clip's place (a full-frame clip goes to the centre; a box at no place grows or
  shrinks about its centre through `effects.setParam scale`). Swap With Clip Below swaps the
  selected clip with the first visible video clip on a lower track (`layout.swap {clips: [a, b]}`);
  with nothing below, `layout.swap` picks the two top-most clips. Redact Area ▸ turns the Program
  picture into a draw surface for one box ([draw mode](#draw-mode-program-monitor)): a **Static**
  entry keeps the box where it is drawn (no tracking job; right for screen recordings, where
  nothing moves), a **Tracked** entry makes it follow what is under it (forward, then backward
  tracking jobs; slow on long clips). The submenu's tooltip says so, and the status line names the
  choice ("Drag a box over what to hide · static mosaic (Esc cancels)"). The old id
  `layout.menu.redact` still works as Static Mosaic…. The menu-bar entries are UI commands (`ui.menu.invoke {id: "layout.menu.place.topLeft"}`);
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
| `layout.menu.place`, `layout.menu.size`, `layout.menu.shape`, `layout.menu.pan`, `layout.menu.redact` | the submenus (right-click menu and timeline clip menu) |
| `layout.menu.place.{at}`, `layout.menu.size.{20\|25\|33\|50}`, `layout.menu.shape.{circle\|rounded\|square\|free}`, `layout.menu.pan.{left\|center\|right}`, `layout.menu.swap`, `layout.menu.redact.{static\|tracked}.{mosaic\|blur\|fill}` | menu entries; also the UI command ids of Clip ▸ Layout (`layout.menu.redact` as a command: an alias of `layout.menu.redact.static.mosaic`) |
| `timeline.clipMenu.layout` | the Layout submenu of the timeline clip menu |
| `effectControls.layout.place.{at}`, `effectControls.layout.shape.{s}` | the Effect Controls row (nine places, no Full) |
| `properties.layout.place.{at}`, `properties.layout.shape.{s}` | the same row in the Properties panel |

## Scenes (`scenes.*`)

Clips whose media item appears in a scene are arranged by the scene: the Program monitor refuses to drag them (the status bar names the scene), and the Effect Controls / Properties layout row shows "Set by scene …" instead of buttons. Change the scene in Sequence ▸ Scenes….

A layout attached to a span of the transcript, so the arrangement follows the words when takes change.

**Clips in a scene are arranged by the scene.** Once a media item appears in any scene of the
sequence, its video clips' Motion `position` / `scale`, Opacity `opacity` and `Layout shape` mask
are recomputed from the scenes on every scene edit and after every transcript or take edit; a
hand edit of those parameters on such a clip is overwritten the next time. To change the
arrangement, change the scene (Sequence ▸ Scenes…). Clips of media items no scene mentions are
never touched.

### Model (schema v14)

- `Sequence::scenes: Vec<Scene>`: `Scene { id, name, slots }`, one `SceneSlot { item, hidden,
  place, size, margin, shape, radius }` per media item. `place` and `shape` are the `layout.place`
  / `layout.shape` names (`"bottomRight"`, `"circle"`), stored as strings; `size` / `margin` /
  `radius` are clamped like the layout commands. The first scene is the **default scene**.
- `Transcript::scenes: Vec<SceneSpan>`: `SceneSpan { scene, range }`, a media-time range of that
  transcript's item, sorted and non-overlapping (a new span trims or splits the ones it overlaps).
  Assigning words makes one span per run of consecutive media words, from the first word's start
  to the start of the media word after the last one (so the pause after a paragraph keeps its
  scene).

### Applying

A recompute, not a patch (`filmcraft_engine::scenes::reapply`, pure and idempotent):

1. Each span is mapped to sequence time through the clips that play its media
   (`filmcraft_edit::transcript::live_ranges`, the mapping the sequence transcript uses).
2. The scene at a sequence time is the span covering it, else the default scene. For a media item
   the scene does not mention, the item keeps the slot of the scene before it.
3. Every video clip of a media item in any scene gets **hold** keyframes at its own start and at
   every span start or end inside it: Motion `position` and `scale` (`uniform_scale` on) from
   `filmcraft_edit::layout::place` with the slot's place, size, margin and shape (exactly what
   `layout.place` / `layout.shape` compute, with the clip's anchor, rotation and Scale to Frame),
   Opacity `opacity` 100 (0 for a hidden slot), and the `Layout shape` mask path
   (`shape_path`; where the scene's shape is free but another time has a shape, a plain
   rectangle of the whole source). When no time has a shape, the layout mask is removed.

`scenes.apply` runs it on demand and adds no undo step when nothing changes. The scene edits below
and the transcript / take edits that move clips (`transcript.extract` / `lift`, Remove Pauses /
Fillers, `transcript.restore`, `takes.select` / `next` / `previous` / `cross` / `restore` /
`detect`) recompute in the same undo step.

### Commands

| Command | Params | Effect | Undo label |
|---|---|---|---|
| `scenes.list` | – | `{scenes: [{id, name, index, default, slots: [{item, name, hidden, place, size, margin, shape, radius}]}], spans: [{scene, name, item, start, end, from, to, seqStart}]}` (`from` / `to`: sequence word indices, `null` when none of the span's words is live) | – |
| `scenes.add` | `name?` (default "Scene N"), `slots?: [{item, hidden?, place?=full, size?, margin?, shape?=free, radius?}]` | Appends a scene (at most 64; 32 slots; one slot per media item; unknown place / shape is refused, numbers are clamped) | Add Scene |
| `scenes.update` | `scene` (id or name), `name?`, `slots?` | Renames and / or replaces the slots | Update Scene |
| `scenes.remove` | `scene` | Removes the scene and its spans | Remove Scene |
| `scenes.assign` | `scene`, `from`, `to?` (sequence word indices, inclusive, like `transcript.extract`) | Assigns the scene to those words | Assign Scene |
| `scenes.clear` | `from`, `to?` | Removes scene spans from those words | Clear Scene |
| `scenes.apply` | – | Recompute (see above) → `{changed}` | Apply Scenes |
| `scenes.defaults` | – | Seeds "Screen with face" (A full, B circle bottom right 25 %), "Face" (B full, A hidden), "Screen" (A full, B hidden) and "Half and half" (A left 50 %, B right 50 %, margin 0), where A (screen) and B (face) are the first media items of the two top-most video tracks with a clip, B the upper one. Names that exist are skipped. | Create Default Scenes |

`filmcraft_engine::scenes::scene_owning(session, clip)` names the scene that arranges a clip's
media item (the one on at the playhead, else the first that mentions it), for the Program
monitor to refuse hand drags on scene-owned clips (not wired yet).

### Text panel

- Each Transcript paragraph starts with a scene chip `text.scene.{p}`: the scene at its first word
  in the scene's colour (a fixed palette by scene index), or "No scene". A click opens a menu:
  the scenes (`text.scene.{p}.pick.{index}`, assigns the paragraph), No Scene
  (`text.scene.{p}.none`), Create Default Scenes when there are none
  (`text.scene.{p}.defaults`), and Scenes… (`text.scene.{p}.manage`).
- The Scenes… dialog (Sequence ▸ Scenes…, UI command `scenes.dialog`; `UiState::transcript_scenes_dialog`,
  selection `transcript_scenes_sel`, settable with `ui.set {"menuDialog": {"scenesSelected": n}}`):
  the list `text.scenes.list.{i}`, Add / Duplicate / Remove / Create Defaults
  (`text.scenes.add|duplicate|remove|defaults`), per slot of the selected scene the pickers
  `text.scenes.slot.{j}.place|size|shape` (options `….place.{name}`, `….size.{n}`,
  `….shape.{name}`) and `text.scenes.slot.{j}.hidden`, Swap A and B (`text.scenes.swap`: the first
  two slots exchange media in the selected scene) and Close (`text.scenes.close`, or Esc). Each
  change is one command, so one undo step.

Not yet: the Effect Controls "Set by scene" note and the Program monitor drag guard (design §4),
animated transitions between scenes.

## Tracked redaction (`redact.*`)

Draw a box over what to hide; a mosaic with a rectangle mask stays there (static, the default of
the menus) or tracks it across the clip (tracked).

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

`redact.start {style?, track?}` (Clip ▸ Layout ▸ Redact Area ▸ …; UI command; `style` mosaic,
blur or fill, default mosaic; `track` default **false**) turns on the draw mode
(`UiState.redact_draw`, style in `UiState.redact_style`, tracking in `UiState.redact_track`) and
shows "Drag a box over what to hide · static mosaic (Esc cancels)" (or `tracked blur`, …) in the
status bar. The drag on the Program picture (automation id
`program.redact.draw`) draws a rubber band; on release the box is mapped from the screen through
the clip's Motion into source pixels of the top-most enabled video clip whose picture contains the
box centre (else the top-most clip under the playhead) and `redact.add {clip, rect, style, track}`
runs with the chosen `track`. A static box starts no job ("Redaction N added (static)"); a tracked
one starts the forward job, which the status bar shows by its own label ("Track Redaction 1
(forward)… 23% · 1:34:48 left", the time left `jobs.list` reports as `etaSeconds`; only export
jobs say "Exporting"). One box per entry; a click without a drag keeps the mode; Esc cancels.

Tracking already works on frames decoded at most 960 px wide (`masks.track`, `TRACK_MAX_WIDTH`;
positions are scaled back to clip pixels), so on 4K footage its speed is bound by decoding the
full-resolution frames.

Follow-up (not in this change): a scroll-following tracker for text that scrolls vertically (the
`position` tracker follows rigid motion, not a scrolling page whose content changes).
