## ADDED Requirements

### Requirement: Scenes describe an arrangement per media item
A sequence SHALL carry named scenes (`Sequence::scenes`), each listing for one or more media items a place, size, margin, shape and radius, or `hidden`. Scenes are stored in the project file (schema v14) and survive save and reload.

### Requirement: Scene spans follow the transcript
A scene SHALL be assigned to a span of the sequence transcript as a media-time range on that transcript's media item (`Transcript::scenes`), beside take groups, so that the span keeps pointing at the same words after takes are swapped, text is crossed out or restored, or clips move.

#### Scenario: Takes change, scene stays with its words
- **WHEN** a paragraph is assigned the scene "Screen with face" and the user then switches the take in an earlier group, moving that paragraph later in the sequence
- **THEN** the clips still show "Screen with face" exactly over the paragraph's words at their new sequence time

### Requirement: Applying scenes writes hold keyframes
`scenes.apply` SHALL rewrite, for every clip whose media item appears in any scene of the sequence, the Motion `position` and `scale` keyframes, the `Layout shape` Opacity mask path keyframes and the Opacity `opacity` keyframes, as hold keyframes at the sequence time where each assigned span starts (and the clip's own start when a span already covers it), from the scene's slots. Clips of a media item that a scene does not mention keep whatever that media item had in the previous span; a `hidden` slot sets opacity 0. Spans with no scene fall back to the sequence's default scene (the first one) when there is one.

#### Scenario: Idempotent
- **WHEN** `scenes.apply` runs twice with no change in between
- **THEN** the project is unchanged by the second run and no extra undo step is added

### Requirement: Scenes re-apply after transcript edits
After any `transcript.*` or `takes.*` command that changes clips, the engine SHALL re-apply the scenes of the affected sequence inside the same undo step, so the arrangement never drifts from the words.

### Requirement: Scene commands
`scenes.list`, `scenes.add {name, slots}`, `scenes.update {scene, name?, slots?}`, `scenes.remove {scene}`, `scenes.assign {scene, from, to}` (word indices of the sequence transcript, inclusive, like `transcript.extract`), `scenes.clear {from, to}`, `scenes.apply`, and `scenes.defaults` (seed four scenes for a sequence with two video media items: "Screen with face", "Face", "Screen", "Half and half") SHALL exist, each edit one undo step.

### Requirement: Scene strip in the Text panel
Each paragraph of the Transcript tab SHALL show a scene chip (`text.scene.{paragraph}`) with the scene assigned at its first word (or "No scene"); clicking opens a menu of the sequence's scenes plus "Scenes…" (the manage dialog) and assigns the chosen scene to the paragraph's words. The manage dialog (`text.scenes.*`) lists scenes with their slots (media item, place, size, shape, hidden) and offers Add, Duplicate, Remove and Create defaults.
