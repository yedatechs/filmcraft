## ADDED Requirements

### Requirement: whisper.cpp engine selectable in Settings
Settings ▸ Media Analysis & Transcription SHALL offer a speech engine choice of `builtin` and `whisperCpp`, with a command (name on PATH or full path), a ggml model path and free-text extra arguments for `whisperCpp`. The engine MUST build the recogniser from these preferences at run time so `transcript.generate` works in builds compiled without the `whisper` feature.

#### Scenario: Engine chosen, model set
- **WHEN** `mediaAnalysis.speechEngine` is `whisperCpp` and `whisperCppModel` names an existing file and the command resolves
- **THEN** `transcript.generate` runs the command on the clip's mono 16 kHz audio (with `-nfa -dtw <preset>` when the model name maps to a whisper.cpp alignment preset, so word times come from token-level DTW) and stores a transcript whose `source` is `whisper.cpp:<model file stem>` with word times and confidences

#### Scenario: Engine chosen, model missing
- **WHEN** `speechEngine` is `whisperCpp` and `whisperCppModel` is empty
- **THEN** `transcript.generate` is disabled with the reason "the whisper.cpp model path is not set (Settings ▸ Media Analysis & Transcription)"

#### Scenario: Command fails
- **WHEN** the command exits with a non-zero status
- **THEN** the error names the exit status and the tail of the command's stderr, and no transcript is stored

### Requirement: Local only, cancellable, bounded
The engine SHALL write the audio to a temporary file that is removed afterwards, SHALL poll the running command so the progress callback can cancel it, SHALL time out a command that does not finish, and SHALL treat every number in the command's JSON as hostile (negative, reversed or absurd offsets are dropped or clipped; non-finite probabilities are ignored).

#### Scenario: Cancel while running
- **WHEN** the progress callback returns false while the command runs
- **THEN** the command is killed, temporary files are removed and the result is `Cancelled`

#### Scenario: Hostile JSON
- **WHEN** the output contains segments with negative offsets, `to < from`, offsets beyond `i64` range when converted, missing fields or probabilities outside 0..1
- **THEN** parsing returns the valid words only, never panics, and the language is `None` when it is not a plain alphabetic code
