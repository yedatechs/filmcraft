# Design: recording inside the app (F13)

Status: in progress. Everything here follows `AGENTS.md`: no panics (capture threads run under
`catch_unwind` and report failures as the recording's error), integer ticks for edit math, every
action a command with an automation id, the import and placement one undo step, `unsafe` only in
`crates/platform` ([ADR 0002](../../../docs/adr/0002-platform-capture-ffi.md)).

## 1. Vocabulary

- **Source**: one thing being recorded: `screen` (a display or a window), `camera`, `mic`. Each
  source is written to its own file, like OBS's Source Record: nothing is composited at capture.
- **Recording clock**: one monotonic clock per recording (§3). Every sample of every source is
  stamped on it.
- **First sample time** of a source: its first frame / audio sample on the recording clock.
- **Sidecar**: `<file stem>.recording.json` next to each file (§4).

## 2. Capture traits (`crates/engine/src/record.rs`)

```rust
pub enum PixelFormat { Bgra8, Rgba8 }           // what a frame's bytes are
pub struct CapturedFrame { width, height, stride, format, data: Vec<u8>, time_ns: u64 }
pub struct VideoRequest { width: Option<u32>, height: Option<u32>, fps: u32 }
pub struct VideoFormat { width, height, fps, format }   // what the input agreed to deliver
pub type FrameSink = Arc<dyn Fn(CapturedFrame) + Send + Sync>;

pub trait VideoInput: Send {
    /// Start delivering frames to `sink` (on any thread) stamped on `clock`. Negotiates the format.
    fn start(&mut self, req: &VideoRequest, clock: RecordClock, sink: FrameSink) -> Result<VideoFormat, CaptureError>;
    fn stop(&mut self);
    /// A failure after start (device unplugged, stream stopped by the system).
    fn error(&self) -> Option<String>;
}

pub trait VideoInputFactory: Send + Sync {
    fn name(&self) -> &str;                                  // "ScreenCaptureKit + AVFoundation", "Synthetic"
    fn devices(&self) -> Result<VideoDevices, CaptureError>;  // displays, windows, cameras
    fn permissions(&self) -> Permissions;                     // screen, camera: granted / denied / undetermined / unavailable
    fn open_screen(&self, target: &ScreenTarget) -> Result<Box<dyn VideoInput>, CaptureError>;
    fn open_camera(&self, device: &str) -> Result<Box<dyn VideoInput>, CaptureError>;
}
```

- `CaptureError { kind: Unavailable | Permission { pane } | NoDevice | Failed, message }`. A
  permission error's message names the System Settings pane (§8).
- The microphone reuses the voice-over `AudioInput` (cpal on desktop, `SyntheticInput` headless).
  While recording, the session's input is moved onto the recording's microphone thread and put
  back at stop; `audio.voiceover.start` is refused while a recording runs.
- **Installing a factory.** `record::register_video_factory(Arc<dyn VideoInputFactory>)` (global,
  like `filmcraft_export::register_encoder`): `filmcraft_platform::register()` installs the macOS
  one. A session can override it (`session.record.factory`, used by tests and the UI test). With
  neither, the session uses `SyntheticFactory`: one synthetic display ("Synthetic Display",
  1280 × 720), one synthetic window, one synthetic camera ("Synthetic Camera", 640 × 360), so
  headless sessions (CLI, MCP, tests) can record.
- **Synthetic video** (`SyntheticVideoInput`): a thread producing frames at the requested rate,
  each a mid-grey picture with a vertical white bar whose x position is a function of the frame's
  time and the frame index burnt into the top row as 16 black / white blocks (MSB first), so a test
  decodes a frame and reads back which frame it is. A `start_delay_ms` simulates a source that
  starts late (its first frame time is `start + delay`), which is how tests inject clock skew.

## 3. One clock

`RecordClock` is taken once in `record.start`: a monotonic `Instant` (mach absolute time on macOS)
plus the wall-clock time at that instant. `time_ns` of every frame and audio block is nanoseconds
on that clock since the instant. The platform layer maps each `CMSampleBuffer`'s presentation time
(host-clock seconds, `CMClockGetHostTimeClock`) onto it: at stream start it reads the host clock
and `clock.now_ns()` back to back and keeps the difference; each frame is
`anchor_ns + (pts − anchor_host)`, clamped so it never goes backwards. The microphone has no
per-sample timestamp from cpal: its first sample time is `clock.now_ns()` when the input started.

The sidecar's `clock_start_ns` is the wall-clock (UNIX epoch) nanoseconds of the clock's instant:
the same value in every sidecar of one recording, so files can be matched later; all other times
in the sidecar are relative to it.

## 4. Files and sidecars

- Folder: Project Settings ▸ Scratch Disks ▸ Captured Audio and Video, else the project file's
  folder, else `Recordings` in the data directory (the preferences' folder), else
  `FilmCraft/Recordings` in the temporary directory. `record.start {dir}` overrides (tests). Same
  rule as voice-over, with `Recordings` instead of `Voice-over Recordings`.
- Names: `<Name> - Screen.mov`, `<Name> - Camera.mov`, `<Name> - Mic.wav`. `<Name>` is
  `record.start {name}` or `Recording <n>` with `n` the first number for which no file of that
  recording exists in the folder and no project item has that name. A given name that is already
  taken gets ` 2`, ` 3`… A name must be 1–100 characters without `/`, `\`, `:`, control
  characters, a leading `.`, or `..`; otherwise the command is refused.
- Video: MOV (QuickTime brand), H.264, `moov` written at the end (the MOV writer streams `mdat` to
  the file while recording). Mic: 32-bit float WAV, mono (Voice-Over Record Settings ▸ Input
  channel), at the input's rate, streamed with the sizes patched at stop.
- Sidecar `<Name> - Screen.recording.json`:

```json
{
  "version": 1, "recording": "Recording 3", "source": "screen", "file": "Recording 3 - Screen.mov",
  "device": {"id": "display:1", "name": "Built-in Retina Display"},
  "clock_start_ns": 1791631234567890123, "first_sample_ns": 41000000, "last_sample_ns": 3041000000,
  "frames": 90, "dropped": 0, "width": 1920, "height": 1080, "fps": 30,
  "encoder": "VideoToolbox H.264", "camera_offset_ms": 0, "events": []
}
```

  Mic sidecars carry `sample_rate`, `channels`, `samples` instead of the picture fields. `events`
  is empty: F14 (click tracking / auto zoom) appends `{"t_ns", "kind": "click", "x", "y",
  "button"}` objects there without a format change (readers ignore unknown kinds).

## 5. Encoding

- One encoder thread per video source, fed by a **bounded queue of 4 frames that drops the
  oldest** when full: the capture callback only copies the frame and pushes, never blocks; drops
  are counted (`dropped`).
- The encoder is chosen through `filmcraft_export`'s encoder factories with H.264, hardware
  encoding `Auto` (VideoToolbox when the platform crate registered it), keyframe every 2 s, VBR
  with the bitrate scaled to the frame size: `12 Mbit/s × (pixels / 1920·1080)^0.87` (≈ 6 Mbit/s
  at 720p, 12 at 1080p, 40 at 4K), times `fps / 30` above 30 fps. Without a hardware encoder
  FilmCraft's own H.264 encoder is used, and above 1920 × 1080 the nominal rate is capped at
  15 fps; when it still cannot keep up, frames are dropped from the queue (counted), so the
  file's effective frame rate drops instead of the capture blocking.
- **Frame times**: the file has the nominal rate `fps` as its timebase. A frame at `time_ns` takes
  slot `round((time_ns − first_ns) · fps / 1e9)`; a frame whose slot is not after the previous
  one is skipped (two frames inside one frame period); each sample lasts until the next frame's
  slot. A screen that does not change delivers no frames (ScreenCaptureKit only sends changed
  frames), so a still screen becomes one long sample, and the last sample lasts until stop.
  Durations are therefore whole frames at `fps` and the media's time 0 is its first sample.
- The writer is `filmcraft_export::recorder::MovRecorder`: it opens the `Mp4Writer` after the first
  packet (encoders finish their parameter sets on the first frame), writes each packet when the
  next frame's slot is known, and `finish(end_ns)` flushes the encoder and writes `moov`. If the
  process dies mid-recording the file has no `moov`; recovery of such files is a follow-up.

## 6. Commands (`record.*`)

| Command | Params | Result |
|---|---|---|
| `record.devices` | – | `{displays: [{id, name, width, height}], windows: [{id, title, app}], cameras: [{id, name, formats: [{width, height, fps}]}], microphones: [name], factory, permissions: {screen, camera, microphone}, error?}` (`error`: the factory could not enumerate, e.g. no permission; the lists are then empty) |
| `record.start` | `screen?: {display: id} \| {window: id}` (+ `fps?` 1–60, default 30), `camera?: {device: id, width?, height?, fps?}` (fps 1–60), `mic?: {device?: name}` (`""`/absent: the default input), `name?`, `dir?` | `{recording: true, name, clockStartNs, files: [{kind, path, sidecar}]}`; refused with no source, while recording, during a voice-over, for an unknown device, `fps` 0 / > 60, sizes out of 16–8192, a bad name |
| `record.status` | – | `{recording, name?, elapsed (seconds), sources: [{kind, device, frames, dropped, bytes, level?}], error?}` (`level`: mic peak 0–1 over the last 100 ms) |
| `record.stop` | `discard?: bool`, `cameraOffsetMs?: number` (−5000…5000) | discard: the files are deleted, `{recording: false, placed: false}`. Otherwise the files are finished, imported (`file.import` path: `import_streamed`) into a `Recordings` bin, and a new sequence named `<Name>` is created and opened → `{recording: false, placed: true, sequence, items: {screen?, camera?, mic?}, clips, offsets: {screen?, camera?, mic?} (ticks), files}` |
| `record.cancel` | – | same as `record.stop {discard: true}` |

All five are journaled except `record.status` and `record.devices`. `record.start` / `stop` /
`cancel` have no menu path or shortcut; the Record panel calls them.

## 7. Import and sync

- Offsets: `offset(source) = first_sample_ns(source) − min first_sample_ns`, plus for the camera
  `cameraOffsetMs × 1e6` (negative moves the camera earlier: a webcam whose frames arrive 120 ms
  after the light hit the sensor needs `−120`). If the override makes any offset negative,
  everything shifts so the earliest starts at 0. Offsets become ticks exactly
  (`Tick::from_units(ns, 1e9)`).
- Sequence settings: the screen file's size and rate (else the camera's), the mic's sample rate
  when there is a mic. Tracks: V1 screen, V2 camera, A1 mic (A2 camera audio when a camera ever
  delivers audio; the macOS camera input does not, see §9). Each clip is placed like Merge Clips
  places its clips: at the next frame boundary at or after its exact offset, with the source In
  moved by the difference, so sample accuracy is kept and clips stay frame-aligned.
- Provenance: each item gets metadata `Recording` (the name), `Recording Source`
  (`screen`/`camera`/`mic`) and `Recording Offset` (ns); the sequence gets a marker at 0 named
  `Recording` whose comment is `camera offset <n> ms` when an override was used. No schema change.
- The whole import (items, bin, sequence, clips, marker) is one undo step labelled `Record`.

## 8. Permissions

macOS privacy (TCC) is checked before a stream starts, never by waiting on a prompt:

| Source | Check | Undetermined | Denied message names |
|---|---|---|---|
| Screen | `CGPreflightScreenCaptureAccess` | `CGRequestScreenCaptureAccess` (shows the system prompt once), then the error below | System Settings ▸ Privacy & Security ▸ Screen & System Audio Recording |
| Camera | `AVCaptureDevice.authorizationStatus(.video)` | `requestAccessForMediaType` (prompt; the answer is not awaited), error "answer the prompt, then Record again" | … ▸ Camera |
| Microphone | cpal start failure / `authorizationStatus(.audio)` | same as camera | … ▸ Microphone |

The error says that the permission belongs to the app that launched FilmCraft when it runs from a
terminal, and that macOS needs FilmCraft restarted after Screen Recording is granted. Nothing
changes System Settings for the user. Every async OS call (`SCShareableContent` enumeration,
stream start) waits at most 5 s, then reports a timeout error.

## 9. Platforms

- macOS 12.3+: ScreenCaptureKit (displays and windows, BGRA, the cursor shown, `minimumFrameInterval`
  = 1/fps, `queueDepth` 4) and AVFoundation (`AVCaptureDeviceDiscoverySession` for built-in and
  external cameras, `AVCaptureSession` + `AVCaptureVideoDataOutput` BGRA, late frames discarded).
  Camera audio is not captured: the microphone source records sound (a camera's built-in mic can
  be picked as the microphone).
- Windows, Linux, web: no factory is installed; `record.devices` lists no displays or cameras and
  reports `unavailable`, `record.start` with a screen or camera is refused with "screen and camera
  recording are not available on this system yet". Microphone-only recording works wherever the
  voice-over input works. The web build refuses `record.start` (no threads).

## 10. UI (`crates/ui-egui/src/panels/record.rs`)

Window ▸ Record opens a floating Record panel (and the Program monitor's ⏺ button, documented in
`docs/recording.md`). Widgets and ids: `record.panel.screen` (+ `.screen.<i>`), camera rows
`.camera.<n>.{device,quality,mirror,offset,remove}` and `.camera.add`, microphone rows
`.mic.<n>.{device,remove}` and `.mic.add` (§11), `.name`, `.record` (Record / Stop), `.cancel`,
`.counters`, `.level`, `.error`, `.close`, `.settings` (+ `.settings.<key>`). State is `UiState::record` (serde: open,
screen / camera / mic choice, quality, name, offset), so `ui.set` drives it and `ui.inspect`
shows it. No new keyboard shortcuts. While recording the status bar shows
`Recording · 00:12 · screen 360 f / camera 358 f` and the panel cannot be closed.

## 11. Several sources and settings (extension)

- **Sources.** A recording has at most one screen, up to four cameras and up to four microphones
  (`Src { kind, n }`; keys `camera2`, `mic2`…, files `… - Camera 2.mov`). The first microphone uses
  the session's voice-over input (handed back at stop); the others open their own
  (`AudioInput::spawn`; the desktop cpal input and the synthetic one implement it). Placement:
  screen, then cameras in order on the video tracks; microphones in order, then system audio, on
  the audio tracks. Each camera has its own offset; Mirror adds the existing `horizontal_flip`
  effect first on the clip (no pixel flip in the file).
- **Settings** (`record_settings.rs`): `RecordingSettings` in `Preferences::recording`, a Settings
  category `recording` drawn by the generic Settings page, `record.settings {get|set}` (merge,
  refusal naming the field), `record.start {settings}` for one recording. Plan: settings → defaults
  of the screen request (`fps`, `max_height`, `show_cursor`, system audio), camera requests
  (quality → size, fps, mirror), the encoding (`CaptureEncoding`) and the audio options.
- **Encoding** (`filmcraft_export::recorder`): `MovRecorder::create_with` takes the codec (H.264,
  HEVC through the hardware encoder only, ProRes 422 through FilmCraft's encoder), the quality
  (`capture_kbps_for`: 6 / 12 / 20 / 40 Mbit/s at 1080p30 × `(px/1080p)^0.87` × `fps/30`), the
  keyframe interval and hardware on / off. A refused HEVC falls back to H.264 with a sidecar note.
- **Audio files**: one WAV writer for 16 / 24-bit PCM and 32-bit float, mono or stereo, header
  rewritten at the end (so the system audio file can take the rate the system delivers). Auto gain
  is applied between the read and the write; the level meter reads the input before it.
- **System audio**: `VideoInput::capture_audio(rate, channels, sink)` before `start` (default: not
  supported), `VideoInputFactory::system_audio()`. ScreenCaptureKit: `capturesAudio`, 48 kHz,
  `excludesCurrentProcessAudio`, a second stream output of type Audio on the same queue, float
  buffers copied out of the block buffer, times mapped with their own `HostClockMap`. A bounded
  queue feeds a writer thread that fills gaps with silence.
- **Countdown and Stop after**: `record.start {countdown}` checks everything, stores a
  `Countdown` and returns; `record::tick` (the UI each frame, `record.status` headless) starts it
  when due and stops a recording past its Stop after limit, keeping what it did in
  `Recorder::last_event`. Both use `Recorder::now_ms`, which tests replace (`test_clock_ms`).

## 12. Follow-ups

Click log into `events` (F14), auto zoom from clicks, area selection, camera audio, recovery of a file without `moov` after a crash, pause /
resume, Windows (Windows.Graphics.Capture, Media Foundation) and Linux (PipeWire portal) inputs,
re-applying a camera offset after stop.
