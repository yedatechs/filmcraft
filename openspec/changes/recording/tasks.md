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

- [x] 8.1 **Opening a capture session turned the Insta360 Link 2C to 9:16.** Cause (measured on the owner's Mac, `AVCaptureDevice.formats`): the camera lists its portrait formats first (736x1280, 1080x1920, 1088x1920 … then 1280x720, 1920x1080 …) and switches itself to portrait mode when one is selected; FilmCraft set the session preset `1920x1080` and macOS picked the 1080x1920 listed before 1920x1080 (the device's active format read 1080x1920 after the owner's recordings). Fix: no size preset any more. `choose_format` picks from the device's own list, landscape unless a portrait size is asked for, nearest the quality, then the frame rate, then the preferred pixel format; it is set as `activeFormat` under the configuration lock held until the session runs, the session on `InputPriority` (`crates/platform/src/capture/camera.rs`, unit tests on the Link 2C's real list; docs/recording.md "Camera format"). Rotate is untouched: Auto still only reads what the camera reports.
- [ ] 8.2 Pop-out preview: Pause / Resume and Stop buttons, a microphone level meter, and a window sized to the camera picture (today it opens far larger than the thumbnail)
- [ ] 8.3 The thumbnail while recording is still large in the pop-out; the panel's own thumbnail shrink is done (7.5)
- [x] 8.4 A stable code-signing identity for the dev bundle: `cargo xtask dev-identity` makes a self-signed "FilmCraft Dev Signing" certificate in a keychain file of its own (never the login keychain, the search list or a trust setting), `cargo xtask bundle` signs with it whenever it exists (`--sign IDENTITY` / `FILMCRAFT_SIGN_IDENTITY` for another identity, `--adhoc` for none), and `--reset-permissions` drops the records of earlier signatures. The designated requirement is then `identifier "ai.storyteller.filmcraft.dev" and certificate leaf = H"…"` for every build (`xtask/src/identity.rs`, a test signs two different programs and compares; docs/contributing.md "macOS app bundle", docs/recording.md "Permissions")
- [ ] 8.5 Retina (2×) display untested for area selection and the overlay placement; Continuity Camera ("Jerephone Camera") listed but not recorded yet
- [ ] 8.6 Double-clicking a `.fcproj` on the bundle opens FilmCraft but not the file (Apple events not handled)
- [ ] 8.7 Still from the original list: camera audio, pause / resume of a recording, several screens, teleprompter, click log (F14), Windows / Linux capture, recovery of a file cut off by a crash
