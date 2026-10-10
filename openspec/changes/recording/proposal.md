# Recording inside the app (F13)

## Why

The owner records his screen and his camera at once, then edits by text. Today that means
recording in Descript or OBS, exporting, importing both files into FilmCraft and lining them up by
hand (or with Synchronize ▸ Audio, which needs both files to carry the same sound). Recording
inside FilmCraft removes the round trip: the files land in the project's capture folder, already in
a sequence, already in sync, ready for transcription and layouts (`openspec/changes/clip-layouts`).

## What changes

1. **Capture traits** (`crates/engine/src/record.rs`): a `VideoInput` (start / stop, a frame
   callback carrying host-clock timestamps, format negotiation) next to the existing voice-over
   `AudioInput`, a `VideoInputFactory` the host installs (enumerates displays, windows and cameras
   and opens them), and deterministic synthetic inputs for headless sessions and tests.
2. **Record commands** (`record.devices`, `record.start`, `record.status`, `record.stop`,
   `record.cancel`): each source is written to its own file (screen and camera to MOV through the
   hardware H.264 encoder when there is one, else FilmCraft's own encoder; microphone to WAV), with
   a JSON sidecar per file timestamped from one clock. Stopping imports the files and builds a new
   sequence with screen on V1, camera on V2 and microphone on A1, placed in sync from the sidecars,
   with an optional camera offset override, as one undo step.
3. **macOS capture** (`crates/platform/src/capture/`): ScreenCaptureKit for displays and windows,
   AVFoundation for cameras, with the macOS privacy permissions (Screen Recording, Camera,
   Microphone) reported as errors that name the System Settings pane to open. Windows and Linux
   report "not available".
4. **Record panel** (Window ▸ Record): source pickers, quality, name, a Record / Stop button with
   an elapsed timer, live per-source counters and the microphone level, Cancel, and the camera
   offset field. A status-bar line while recording.

Out of scope (later): click tracking and auto zoom (F14; the sidecar has an `events` list ready for
a click log), area selection, system-audio capture, camera audio, Windows / Linux capture, pause /
resume.

## Impact

- New engine module `record.rs` (+ `record_tests.rs`); `voiceover.rs` refuses to start while a
  recording runs (they share the microphone input).
- `filmcraft-export` gains a public streaming MOV recorder (`recorder.rs`) used by the engine.
- `filmcraft-platform` gains `capture/` (new FFI modules), depends on `filmcraft-engine` for the
  traits, and two new bindings `objc2-screen-capture-kit` and `objc2-av-foundation`
  ([ADR 0002](../../../docs/adr/0002-platform-capture-ffi.md)).
- UI: `panels/record.rs`, `UiState::record`, a status-bar line, Window ▸ Record.
- Docs: `docs/recording.md` (new), ADR 0002, AGENTS.md §0.3, ADR 0001 status.
- No project schema change: the sync information is kept as item metadata and a sequence marker.
