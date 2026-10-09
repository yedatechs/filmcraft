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
`layout.swap` SHALL exchange place and shape between two clips (default: the two top-most video clips visible at the playhead), and applying it twice SHALL restore both.

### Requirement: Pick
`layout.pick {x, y}` SHALL return the video clips whose visible box contains the frame point at the playhead, top-most first, without changing the project.

### Requirement: Program monitor direct manipulation
With the Selection tool, a click on the Program picture that hits no graphic SHALL select the top-most video clip under the pointer (repeated clicks cycle); the selected clip SHALL show its visible box with eight handles; dragging inside moves it (one undo step per drag, snapping to frame edges and centre) and corner handles scale it uniformly; right-click SHALL offer Place, Size, Shape, Swap With Clip Below and Redact Area…, with the current values checked. The same Layout submenu SHALL appear in the timeline clip context menu and the Clip menu, and the Effect Controls Transform section SHALL show the place and shape buttons.
