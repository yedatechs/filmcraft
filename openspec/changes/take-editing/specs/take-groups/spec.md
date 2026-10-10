## ADDED Requirements

### Requirement: Take groups are stored on the media transcript
A `Transcript` SHALL carry `takes: Vec<TakeGroup>`; a `TakeGroup` has an id, an ordered list of `Take`s (each a media `TimeRange`, an optional label from a fixed set, a free-text note), a `redo` flag and a `manual` flag. Ranges are media time so they survive trims and re-transcription. The project schema SHALL be v13 so older builds refuse files that carry take data instead of dropping it.

#### Scenario: Save and reload
- **WHEN** a project with take groups, labels and redo flags is saved and reopened
- **THEN** all of them are intact and `transcript.set` with a transcript containing `takes` stores them after normalisation

#### Scenario: Older file
- **WHEN** a v12 project is opened
- **THEN** it loads with no take groups and reports an upgrade to v13

### Requirement: Detection groups repeated lines
`takes.detect` SHALL split a media transcript into utterances (at pauses or sentence punctuation), compare each utterance with the next few within a time window using normalised-word similarity (shared opening words, token overlap, edit distance) and spoken retake cues, and bundle similar utterances into one group of two or more takes. A `sensitivity` parameter (0..1, default 0.5) SHALL lower the similarity threshold monotonically (every pair that groups at a lower sensitivity still groups at a higher one; the group *count* can fall when a looser threshold chains two groups through a bridging utterance). Manual groups SHALL be kept and a detected group covering the same stretch as a manual one SHALL be dropped; other detected groups are replaced on re-detection. Detection MUST be deterministic and bounded (fixed comparison window).

#### Scenario: Exact and reworded repeats
- **WHEN** a transcript has "today we talk about rivers" twice and "so today, rivers, we talk about them" right after
- **THEN** the three utterances form one group of three takes in media order

#### Scenario: False start
- **WHEN** an utterance of two or more words is the opening of the next utterance
- **THEN** both are takes of one group

#### Scenario: Restart without a pause
- **WHEN** an utterance says a phrase of two or more words again within a few words (two words within six, three or more within ten, not right after a joining word such as "and" or "then"), e.g. "and they don't even have a and they don't even have a five hour window"
- **THEN** it is split before both occurrences and the parts are takes of one group in order, so the last part is the default live take; an immediately repeated word ("so so", "the the") is read once and never splits

#### Scenario: Unrelated sentences
- **WHEN** consecutive utterances share no content words
- **THEN** no group is made

### Requirement: Which take is live is derived from the timeline
A take is **live** when at least half of its media range is played by enabled clips on the sequence's audio tracks. `takes.list` SHALL report each take's live state and the group's active take (the single live take, or none).

#### Scenario: After detection
- **WHEN** a clip plays the whole media and `takes.detect` finds a group
- **THEN** every take of the group is live and the group has no single active take until one is chosen

### Requirement: Switching takes is one timeline edit
`takes.select` SHALL make the chosen take the only live take of its group: the live takes' timeline ranges are extracted and the chosen take's media is restored at the first of those positions, in one undo step. `takes.next` and `takes.previous` SHALL select the neighbouring take of the group at the playhead or selection. `takes.cross` SHALL extract a live take; `takes.restore` SHALL bring a take back without removing others.

#### Scenario: Cycle
- **WHEN** take 3 is active and the user runs `takes.previous`
- **THEN** take 2 is the only live take, the words before and after the group are unmoved relative to it, and undo returns take 3

#### Scenario: Default active take after detection
- **WHEN** `takes.detect` runs with `select: "last"` (the default)
- **THEN** every group's last take is made active in the same undo step

### Requirement: Labels, notes, redo list and manual correction
`takes.label` SHALL set or clear a take's label (`good`, `best`, `flat`, `stumble`, `wrongEnergy`) and note; `takes.redo` SHALL set a group's re-record flag; `takes.list` SHALL filter by label and redo. `takes.merge`, `takes.split`, `takes.add` and `takes.remove` SHALL correct groupings, marking the results manual.

#### Scenario: Redo list
- **WHEN** two groups are flagged redo
- **THEN** `takes.list {redo: true}` returns exactly those two

### Requirement: Hostile parameters never crash
Every `takes.*` command SHALL validate group and take indices, ranges and labels and return an error naming the problem.

#### Scenario: Out of range
- **WHEN** `takes.select {group: 99, take: 0}` runs
- **THEN** the result is an error "no take group 99" and the project is unchanged
