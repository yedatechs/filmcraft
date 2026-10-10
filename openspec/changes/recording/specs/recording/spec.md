## ADDED Requirements

### Requirement: Each source is recorded to its own file
`record.start` SHALL record each requested source (screen: a display or a window; camera; microphone) to its own file in the capture folder (Project Settings ▸ Scratch Disks ▸ Captured Audio and Video, else next to the project, else the data directory's `Recordings` folder), named `<Name> - Screen.mov`, `<Name> - Camera.mov` and `<Name> - Mic.wav`, and SHALL refuse a request with no source.

#### Scenario: Three sources
- **WHEN** a recording is started with a screen, a camera and a microphone, and stopped after 2 s
- **THEN** three files exist, the two MOV files decode with FilmCraft's own decoders and last about 2 s, and the WAV file holds about 2 s of samples

### Requirement: One clock and a sidecar per file
Every sample of every source SHALL be stamped on one monotonic clock taken at `record.start`, and each file SHALL get a sidecar `<file stem>.recording.json` with `version`, `recording`, `source`, `file`, `device`, `clock_start_ns` (the same in every sidecar of a recording), `first_sample_ns`, `last_sample_ns`, `dropped`, the picture or audio format, `encoder` (video), and an `events` list that is empty until click tracking exists.

### Requirement: Capture never blocks and never crashes
Frames SHALL pass from the capture callback to the encoder through a bounded queue that drops the oldest frame when full; dropped frames SHALL be counted in `record.status` and the sidecar. Capture and encoder threads SHALL run under `catch_unwind`; a failure SHALL end up as the recording's `error` (in `record.status` and as the error of `record.stop`), never as a hang or a crash.

### Requirement: Stop imports and places the sources in sync, in one undo step
`record.stop` SHALL finish the files, import them, and create a sequence named after the recording with the screen on V1, the camera on V2 and the microphone on A1, each placed at its first sample time minus the earliest first sample time (frame-aligned, with the source In absorbing the sub-frame difference), as one undo step. `cameraOffsetMs` SHALL move the camera clip by that many milliseconds (renormalised so nothing starts before 0).

#### Scenario: A camera that starts late
- **WHEN** the synthetic camera starts 300 ms after the screen
- **THEN** the camera clip starts 300 ms (± one frame) after the screen clip in the new sequence

#### Scenario: Undo
- **WHEN** the user undoes once after `record.stop`
- **THEN** the sequence, its clips and the imported items are gone (the files stay on disk)

### Requirement: Cancel leaves nothing behind
`record.cancel` (and `record.stop {discard: true}`) SHALL stop every source and delete the recording's files and sidecars, leaving the project unchanged.

### Requirement: Hostile parameters are refused
`record.start` SHALL refuse, with an error naming the problem: a second start while recording, an unknown display, window, camera or microphone, `fps` 0 or above 60, a frame size outside 16–8192, and a name that is empty, longer than 100 characters, or contains a path separator, `:`, a control character, `..` or a leading `.`.

### Requirement: Permissions are errors, not hangs
On macOS, a Screen Recording, Camera or Microphone permission that is denied or not yet determined SHALL make `record.start` fail with a message naming the System Settings ▸ Privacy & Security pane to open (and request the permission prompt when undetermined, without waiting for the answer). On systems without screen and camera capture, `record.devices` SHALL report them unavailable and `record.start` SHALL refuse them with "not available on this system yet".

### Requirement: Record panel
Window ▸ Record SHALL open a Record panel with Screen, Camera (+ quality: 720p / 1080p / 4K / native) and Microphone pickers, a Name field, a Record button that becomes Stop with an elapsed timer, per-source frame / dropped counters and the microphone level, Cancel, and an "Offset camera by … ms" field applied at stop. Every widget SHALL have an automation id under `record.panel.*`, the panel's state SHALL be in `UiState` (serde) and, while recording, the status bar SHALL show `Recording · mm:ss · screen N f / camera M f`.
