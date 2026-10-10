## ADDED Requirements

### Requirement: Redact an area
`redact.add` SHALL apply a mosaic, blur or fill effect to a clip with a rectangle mask named `Redaction N` over the given clip-pixel rectangle, select that mask, and when `track` is true start mask tracking forward from the playhead and then backward, as background jobs with cancel.

#### Scenario: Draw a tracked box
- **WHEN** the user chooses Redact Area ▸ Tracked Mosaic… and drags a box on the Program picture
- **THEN** `redact.add` runs with that rectangle and `track: true` on the top-most video clip under the box, the mosaic appears at once, tracking jobs show in the status bar by their own labels (`Track Redaction N (forward)`) with the time left that `jobs.list` reports, and `redact.list` reports the redaction as tracked when they finish

### Requirement: Static by default in the menus
The Redact Area ▸ submenu (right-click box menu, timeline clip Layout submenu, Clip ▸ Layout) SHALL offer Static Mosaic, Static Blur, Static Fill, Tracked Mosaic, Tracked Blur and Tracked Fill (`layout.menu.redact.{static|tracked}.{mosaic|blur|fill}`); `redact.start` and the old `layout.menu.redact` id SHALL default to a static box (`track: false`), and the draw-mode status line SHALL name the choice.

#### Scenario: Draw a static box
- **WHEN** the user chooses Redact Area ▸ Static Mosaic… (status line "Drag a box over what to hide · static mosaic (Esc cancels)") and drags a box on the Program picture
- **THEN** `redact.add` runs with `track: false`, the mosaic stays where the box was drawn, no job starts, and `redact.list` reports the redaction with `tracked: false`

### Requirement: List and remove
`redact.list` SHALL report a clip's redactions; `redact.remove` SHALL remove one (effect and mask) as one undo step.
