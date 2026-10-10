## 1. Design

- [x] 1.1 Proposal, design, spec, tasks; ADR 0002 (capture FFI in `crates/platform`); AGENTS.md §0.3 and ADR 0001 status pointers

## 2. Engine (headless)

- [x] 2.1 `record.rs`: `VideoInput` / `VideoInputFactory` traits, `RecordClock`, synthetic display / window / camera inputs, bounded drop-oldest frame queue, encoder threads under `catch_unwind`
- [x] 2.2 `filmcraft_export::recorder::MovRecorder` (streaming MOV, slot-based frame durations) and a streaming WAV writer
- [x] 2.3 `record.devices / start / status / stop / cancel`; sidecars; import + sync + sequence as one undo step; `cameraOffsetMs`
- [x] 2.4 `record_tests.rs`: devices, three decodable files with sidecars, skew → offsets, camera offset, undo, double start, hostile params, cancel

## 3. macOS capture

- [x] 3.1 ADR 0002 bindings in `crates/platform/Cargo.toml`; `capture/mod.rs` (factory, permissions, clock mapping)
- [x] 3.2 `capture/screen.rs` (ScreenCaptureKit displays / windows, `SCStream` + output delegate, BGRA)
- [x] 3.3 `capture/camera.rs` (AVFoundation discovery, `AVCaptureSession` + video data output delegate, BGRA)
- [x] 3.4 `register()` installs the factory on macOS; permission-free unit tests

## 4. UI

- [x] 4.1 Record panel (`panels/record.rs`), Window ▸ Record, `UiState::record`, status-bar line
- [x] 4.2 `tests/record_ui.rs` with synthetic inputs (record, stop → sequence; cancel → no files)
- [x] 4.3 `docs/recording.md`, linked from the docs

## 5. Several sources and settings

- [x] 5.1 `cameras` / `mics` lists (aliases `camera` / `mic`), up to four each, refused twice; a file and sidecar each; V1 screen, V2… cameras, A1… mics; `cameraOffsetsMs`; Mirror as a Horizontal Flip effect; `AudioInput::spawn` for a second microphone; camera and microphone rows in the panel
- [x] 5.2 `RecordingSettings` in the preferences (`recording.*`), `record.settings {get|set}`, Settings ▸ Recording, the panel's Settings section; `record.start {settings}` and its defaults
- [x] 5.3 `MovRecorder::create_with` (H.264 / HEVC / ProRes 422, quality tiers, keyframe interval, hardware on / off); HEVC falls back to H.264
- [x] 5.4 Screen resolution (`max_height`; ScreenCaptureKit `width` / `height`, else in software), Show cursor, camera frame rate (`activeVideoMinFrameDuration`), WAV 16 / 24 / 32-bit float mono / stereo at the chosen rate, auto gain
- [x] 5.5 System audio (ScreenCaptureKit audio output → `… - System Audio.wav`), synthetic tone headless
- [x] 5.6 Countdown (`record.start {countdown}`, `record::tick`, a replaceable clock), Stop after, Open the sequence after Stop, Output folder
- [x] 5.7 Tests (engine `record_tests`, export `recorder`, `record_ui`), `docs/recording.md` § Settings

## 6. Acceptance

- [x] 6.1 Smoke test on a Mac: `record.devices` lists the display and two cameras; 5 s screen + mic with the defaults (VideoToolbox H.264), 3 s screen at 60 fps / 720p / no cursor / HEVC, 3 s screen + system audio (no camera recorded: Camera permission undetermined)
- [ ] 6.2 Owner: screen + FaceTime camera + mic for a minute, transcribe, circle the face bottom right
