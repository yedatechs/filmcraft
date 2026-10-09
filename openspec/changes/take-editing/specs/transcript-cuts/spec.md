## ADDED Requirements

### Requirement: Removed media is a cut span
For the active sequence, the engine SHALL derive **cut spans**: for two consecutive clips of the same media item on one audio track, where the second clip's media In is later than the first clip's media Out, the media between them is a cut span anchored at the first clip's timeline end. A cut span lists the transcript words whose midpoints fall inside it; a span with no words is still a span (a removed pause). Nothing is stored: cut spans are derived from the timeline like the sequence transcript.

#### Scenario: Extract leaves a cut span
- **WHEN** the user extracts words 3..5 of a clip's transcript
- **THEN** `transcript.cuts` lists one span with those words, anchored at the frame where they were, and `transcript.inspect` no longer lists them as live words

#### Scenario: Remove Pauses leaves wordless spans
- **WHEN** `transcript.removePauses` removes two pauses
- **THEN** `transcript.cuts` lists two spans with zero words and the removed durations

#### Scenario: Different media is not a span
- **WHEN** two consecutive clips on a track come from different media items
- **THEN** no cut span is derived between them

### Requirement: A cut span can be restored
`transcript.restore` SHALL put a cut span's media back at its anchor: the clip before it grows by the span (also its linked partners and any other clip of the same media ending at the anchor with contiguous media), everything on every unlocked track from the anchor moves right by the span's duration, sync-locked caption tracks move with it, and a clip on another track that spans the anchor is lengthened when its media allows (the inverse of Extract) and otherwise split with its right part moved. The edit SHALL be one undo step and SHALL leave the sequence transcript showing the restored words as live.

#### Scenario: Restore after extract
- **WHEN** words 3..5 were extracted and the user restores that span
- **THEN** the clip is contiguous again (one clip, no new edit point), later clips are back at their original times, and undo returns to the extracted state

#### Scenario: Restore part of a span
- **WHEN** the user restores a media range that is strictly inside a cut span
- **THEN** a new clip of that media range (with its linked partners) is inserted at the anchor and the rest of the span remains crossed out

#### Scenario: Locked track
- **WHEN** a track is locked
- **THEN** its clips do not move and the restore still succeeds on the other tracks

### Requirement: Crossed-out words in the Text panel
The Transcript tab SHALL show each cut span's words in place, struck through and dimmed, after the last live word before the anchor; a wordless span SHALL show as a struck-through pause with its duration. Clicking a crossed-out span SHALL restore it.

#### Scenario: Visible after filler removal
- **WHEN** Remove Filler Words removed "um" and "uh"
- **THEN** both appear struck through where they were, and clicking one brings it back
