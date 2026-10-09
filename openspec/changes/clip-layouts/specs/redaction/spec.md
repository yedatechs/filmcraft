## ADDED Requirements

### Requirement: Redact an area
`redact.add` SHALL apply a mosaic, blur or fill effect to a clip with a rectangle mask named `Redaction N` over the given clip-pixel rectangle, select that mask, and when `track` is true start mask tracking forward from the playhead and then backward, as background jobs with cancel.

#### Scenario: Draw a box
- **WHEN** the user chooses Redact Area… and drags a box on the Program picture
- **THEN** `redact.add` runs with that rectangle on the top-most video clip under the box, the mosaic appears at once, tracking jobs show in the status bar, and `redact.list` reports the redaction as tracked when they finish

### Requirement: List and remove
`redact.list` SHALL report a clip's redactions; `redact.remove` SHALL remove one (effect and mask) as one undo step.
