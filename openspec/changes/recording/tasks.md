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
- [x] 6.2 Owner (2026-10-10, 16:46 and 17:29): screen + Insta360 Link 2C + mic recorded into one synced sequence (Recording 4 / 5 and a 19 s run with the live preview pop-out); transcribe + circle layout not yet exercised on a recording

## 7. Shipped on 2026-10-10 after the first owner run (fork branch `feat/whisper-cpp-engine`)

- [x] 7.1 Aligned start: every source live before the clock starts, `warmup_ns` per sidecar, `START_TIMEOUT` 20 s, "Starting…" state, `wait: false` (532a686)
- [x] 7.2 Live camera preview (`record.preview`, thumbnails, pop-out viewport), border overlay viewport around the recorded display / window (idle grey, red + "● REC" while recording), area selection (`screen.area` → `sourceRect`), overlay and preview windows excluded from the file by title + pid (ee75b4d, b16afa0, fb08c14)
- [x] 7.3 Objective-C exceptions caught at every AVFoundation / ScreenCaptureKit / AppKit call (`capture::catch_objc`, ADR 0002 §4): an exception is the source's error, never an abort (a8f94e1); the Insta360's exact-fraction frame rates set with the format's own duration bounds (3ff3d8c)
- [x] 7.4 Rotation written into the MOV `tkhd` matrix instead of a clip effect; Rotate Auto from the camera's reported orientation (rotation coordinator on macOS 14+, polled); a camera of another aspect gets Scale to Frame next to the screen (a68b87f, e5fb61f, 88e686a)
- [x] 7.5 Record panel: thumbnail no longer overlaps the rows below, 120 px while recording (9be7262); minimize to a strip (3a4e917)
- [x] 7.6 `cargo xtask bundle` → "FilmCraft Dev.app" (own bundle id `ai.storyteller.filmcraft.dev`, camera / microphone usage descriptions, `.fcproj` document type, ad-hoc signed) (cb8fd3e, 636e8b3)
- [x] 7.7 Border frames from CoreGraphics (no ScreenCaptureKit polling), a silent first `startCapture` retried once, the stalled-start error names the stale Screen Recording entry a rebuilt bare binary leaves in macOS (a449601)
- [x] 7.8 `Logs/session-<day>.log`: app start and every quit request (b9aca22)

## 8. Open after the owner's evening run (2026-10-10, 17:29)

- [ ] 8.1 **Opening a capture session turns the Insta360 Link 2C to 9:16.** When FilmCraft starts a preview / recording the camera flips to portrait and the Insta360 Link Controller shows it switched too, so the owner has to turn it back inside FilmCraft; Rotate set in FilmCraft then records fine, but turning it back in the vendor app gives a wrong result. Suspects: the session preset / `activeFormat` chosen for the quality (`preset_for`, 1080p), `AVCaptureDeviceRotationCoordinator` creation, or the connection's `videoRotationAngle`; the vendor app may react to a format with portrait dimensions. Reproduce with the Link Controller open and log the format and angles at session start; prefer the camera's current format and never touch its rotation unless Rotate is set by hand.
- [ ] 8.2 Pop-out preview: Pause / Resume and Stop buttons, a microphone level meter, and a window sized to the camera picture (today it opens far larger than the thumbnail)
- [ ] 8.3 The thumbnail while recording is still large in the pop-out; the panel's own thumbnail shrink is done (7.5)
- [ ] 8.4 A stable code-signing identity for the dev bundle (self-signed certificate) so Screen Recording / Camera grants survive rebuilds; until then launch the bare binary from iTerm and remove a stale "FilmCraft" Screen Recording entry after a rebuild (docs/recording.md § Permissions)
- [ ] 8.5 Retina (2×) display untested for area selection and the overlay placement; Continuity Camera ("Jerephone Camera") listed but not recorded yet
- [ ] 8.6 Double-clicking a `.fcproj` on the bundle opens FilmCraft but not the file (Apple events not handled)
- [ ] 8.7 Still from the original list: camera audio, pause / resume of a recording, several screens, teleprompter, click log (F14), Windows / Linux capture, recovery of a file cut off by a crash
