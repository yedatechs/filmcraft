## ADDED Requirements

### Requirement: Pause threshold preview
`transcript.pauses {minSeconds, keepSeconds}` SHALL report how many pauses Remove Pauses would shorten and how many seconds it would remove, without changing the project.

### Requirement: Remove Pauses dialog
The Text panel SHALL offer a Remove Pauses button that opens a dialog with the minimum pause length and the length to keep (seconds), a live count from `transcript.pauses`, and Apply, which runs `transcript.removePauses` with those values; the values SHALL be remembered between sessions, and the Sequence ▸ Transcript ▸ Remove Pauses… menu entry SHALL open the same dialog.

#### Scenario: Pauses over half a second
- **WHEN** the user sets 0.5 s minimum and 0.15 s kept and presses Apply
- **THEN** every pause longer than 0.5 s is shortened to 0.15 s, each shows crossed out in the transcript, and one undo step undoes them all
