## 1. Design

- [x] 1.1 Proposal, design, spec, tasks; ADR 0002 (capture FFI in `crates/platform`); AGENTS.md §0.3 and ADR 0001 status pointers

## 2. Engine (headless)

- [ ] 2.1 `record.rs`: `VideoInput` / `VideoInputFactory` traits, `RecordClock`, synthetic display / window / camera inputs, bounded drop-oldest frame queue, encoder threads under `catch_unwind`
- [ ] 2.2 `filmcraft_export::recorder::MovRecorder` (streaming MOV, slot-based frame durations) and a streaming WAV writer
- [ ] 2.3 `record.devices / start / status / stop / cancel`; sidecars; import + sync + sequence as one undo step; `cameraOffsetMs`
- [ ] 2.4 `record_tests.rs`: devices, three decodable files with sidecars, skew → offsets, camera offset, undo, double start, hostile params, cancel

## 3. macOS capture

- [ ] 3.1 ADR 0002 bindings in `crates/platform/Cargo.toml`; `capture/mod.rs` (factory, permissions, clock mapping)
- [ ] 3.2 `capture/screen.rs` (ScreenCaptureKit displays / windows, `SCStream` + output delegate, BGRA)
- [ ] 3.3 `capture/camera.rs` (AVFoundation discovery, `AVCaptureSession` + video data output delegate, BGRA)
- [ ] 3.4 `register()` installs the factory on macOS; permission-free unit tests

## 4. UI

- [ ] 4.1 Record panel (`panels/record.rs`), Window ▸ Record, `UiState::record`, status-bar line
- [ ] 4.2 `tests/record_ui.rs` with synthetic inputs (record, stop → sequence; cancel → no files)
- [ ] 4.3 `docs/recording.md`, linked from the docs

## 5. Acceptance

- [ ] 5.1 Smoke test on a Mac: `record.devices` lists the displays and the FaceTime camera; a 3 s screen-only recording
- [ ] 5.2 Owner: screen + FaceTime camera + mic for a minute, transcribe, circle the face bottom right
