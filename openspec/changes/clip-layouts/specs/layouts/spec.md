## ADDED Requirements

### Requirement: One-click clip placement
`layout.place` SHALL set a video clip's Motion position and uniform scale so that its visible box (after any layout shape) sits at the named place with the given width (% of frame width) and margin, as one undo step, adding or replacing keyframes at the playhead when the parameters are animated.

#### Scenario: Face bottom right
- **WHEN** `layout.place {clips:[c], at:"bottomRight", size:25, margin:3}` runs on a clip
- **THEN** `layout.inspect` reports `at: "bottomRight"`, a box 25 % of the frame wide whose right and bottom edges are 3 % of the frame width from the frame's edges, and one undo step named "Place Clip"

#### Scenario: Full
- **WHEN** `at: "full"` runs on a clip whose source aspect differs from the frame
- **THEN** the source is fitted inside the frame, centred, and its shape is unchanged

### Requirement: Layout shapes
`layout.shape` SHALL replace the clip's `Layout shape` Opacity mask with a circle (inscribed in the central square), a rounded rectangle, a square, or remove it (`free`), keeping the visible box's centre and width, and leaving other masks alone.

#### Scenario: Circle stays placed
- **WHEN** a clip placed bottom right at 25 % gets `shape: "circle"`
- **THEN** `layout.inspect` still reports `bottomRight`, size 25, `shape: "circle"`, and the box is square

### Requirement: Swap
`layout.swap` SHALL exchange place, shape, pan and zoom between two clips on different video tracks (default: the two top-most video clips visible at the playhead) and SHALL exchange their tracks over the span where they overlap, splitting a clip that extends beyond that span at its bounds on its own track only (linked audio untouched), as one undo step. Applying it twice SHALL restore both layouts and tracks (the split edit points may remain). Clips on the same track, or that do not overlap in time, SHALL be refused.

#### Scenario: The circle comes to the top
- **WHEN** V1 holds a full-frame screen clip and V2 a camera circle bottom right, and `layout.swap` runs at a time both show
- **THEN** over the overlap V1 holds the camera piece full frame and V2 the screen piece as the bottom-right circle, `layout.inspect` on the pieces reports the exchanged `at`, `shape` and `track`, and the audio tracks are unchanged

#### Scenario: Text edits after a swap
- **WHEN** words inside the swapped span are extracted from the transcript
- **THEN** both video tracks lose the same timeline range

### Requirement: Pan inside the shape
`layout.pan {dx, dy}` SHALL move a clip's circle, square or rounded shape inside its source by that many source pixels (clamped so the shape stays inside the source; non-finite values count as 0), keeping the visible box where it is by moving Motion position the opposite way, as one undo step (`merge` folds a drag into one). `layout.inspect` SHALL report the pan, and `layout.place`, `layout.shape` and `layout.swap` SHALL keep (swap: carry) it. In the Program monitor, Alt/Option-drag inside the box SHALL pan, and the Layout menus SHALL offer Pan ▸ Centre on Left Third / Centre / Centre on Right Third.

#### Scenario: Face on the left third
- **WHEN** a 3840 × 2160 clip with a circle shape gets `layout.pan {dx: -640}` (the menu's Centre on Left Third)
- **THEN** the mask is the circle centred on x = 1280 of the source, `layout.inspect` reports `pan: [-640, 0]` and the same box as before

### Requirement: Zoom inside the shape
`layout.zoom {zoom}` (absolute) or `layout.zoom {by}` (a factor on the current zoom) SHALL zoom the picture inside a clip's circle, square or rounded shape: the shape's extent in the source is divided by the zoom (clamped to 1–8; a value that is not a positive finite number counts as 1, or as no change for `by`), centred at the source centre plus the pan (clamped to the smaller shape's room), and the visible box SHALL stay where it is (Motion scale grows by the zoom, position compensates), as one undo step "Zoom Clip" (`merge` folds a gesture into one). Clips without such a shape SHALL be refused. The zoom lives in the mask path (no new saved field). `layout.inspect` SHALL report `zoom`; `layout.place`, `layout.shape` and `layout.pan` SHALL keep it and `layout.swap` SHALL carry it; scenes write zoom 1. In the Program monitor, Alt/Option + scroll over the box (×/÷ 1.05 per notch; notches within 400 ms are one undo step) and Alt/Option-drag of a corner handle (outward = zoom in) SHALL zoom, and the Layout menus SHALL offer Zoom ▸ 100 % / 125 % / 150 % / 200 % / Zoom In / Zoom Out with the current preset checked.

#### Scenario: Bigger face in the same circle
- **WHEN** a clip with a circle placed bottom right gets `layout.zoom {zoom: 1.5}` (the menu's 150 %)
- **THEN** the mask is a circle two thirds as wide in the source, `layout.inspect` reports `zoom: 1.5`, the same box and `at`, and Motion scale is 1.5 times what it was; one undo restores zoom 1

### Requirement: Pick
`layout.pick {x, y}` SHALL return the video clips whose visible box contains the frame point at the playhead, top-most first, without changing the project.

### Requirement: Program monitor direct manipulation
With the Selection tool, a click on the Program picture that hits no graphic SHALL select the top-most video clip under the pointer (repeated clicks cycle); the selected clip SHALL show its visible box with eight handles; dragging inside moves it (one undo step per drag, snapping to frame edges and centre) and corner handles scale it uniformly; right-click SHALL offer Place, Size, Shape, Pan, Zoom, Swap With Clip Below and Redact Area…, with the current values checked. The same Layout submenu SHALL appear in the timeline clip context menu and the Clip menu, and the Effect Controls Transform section SHALL show the place and shape buttons.
