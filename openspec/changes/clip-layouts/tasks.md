## 1. Pause threshold (F8a)

- [x] 1.1 `transcript.pauses {minSeconds, keepSeconds}` preview command with tests
- [x] 1.2 Text panel Remove Pauses button + dialog (live count, Apply), values remembered in UiState; menu entry opens the dialog; headless test; `docs/transcripts.md`

## 2. Layout engine (F9.1)

- [ ] 2.1 `filmcraft_edit::layout`: visible box, place transform, nearest preset, rounded-rect path; unit tests
- [ ] 2.2 `layout.place / shape / swap / inspect / pick` commands, keyframe-aware, one undo step each; engine tests; `docs/layouts.md`

## 3. Layout UI (F9.2)

- [ ] 3.1 Program monitor: click selects the top-most video clip (cycle on repeat), box + 8 handles, drag to move (merged undo step, snapping), corners scale
- [ ] 3.2 Right-click menu on the box; the same `Layout` submenu in the timeline clip menu and Clip menu; check marks from `layout.inspect`
- [ ] 3.3 Effect Controls Transform row of layout buttons; status-bar hint; `docs/monitors.md`; headless tests `layout_ui.rs`

## 4. Tracked redaction (F15)

- [ ] 4.1 `redact.add / list / remove` with forward-then-backward tracking jobs; engine tests
- [ ] 4.2 Redact Area… draw mode in the Program monitor; menu entries; headless test; docs

## 5. Scenes (F9.3)

- [ ] 5.1 Spec `specs/scenes/spec.md`; `Sequence::scenes` + transcript assignments, schema v14, fixture
- [ ] 5.2 `scenes.*` commands (apply writes hold keyframes); defaults; tests
- [ ] 5.3 Text panel scene strip; headless test; docs

## 6. Acceptance

- [ ] 6.1 Owner: camera + screen recording, face in a circle bottom right over the screen, swap, redact a box, remove pauses over 0.5 s, all from the UI without typing numbers
