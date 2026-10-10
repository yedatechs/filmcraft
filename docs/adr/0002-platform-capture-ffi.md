# ADR 0002: OS media capture (screen, camera) in `crates/platform`

- **Status:** proposed (2026-10-10), for F13 "Recording inside the app"
  ([openspec/changes/recording](../../openspec/changes/recording/design.md)). Extends
  [ADR 0001](0001-platform-ffi.md); it does not replace it.

## Context

ADR 0001 allows `unsafe` in exactly one crate, `crates/platform`, for one job: hardware video
decoding and encoding through the operating system. Recording the screen and a camera inside
FilmCraft needs two more OS APIs that have no pure-Rust alternative: on macOS, ScreenCaptureKit
(displays and windows) and AVFoundation (cameras, and the camera / microphone privacy status).
Both are Objective-C frameworks, reached through the generated `objc2-*` bindings the crate already
uses for VideoToolbox. The microphone already works without new FFI (cpal, in the desktop app,
behind `filmcraft_engine::voiceover::AudioInput`).

## Decision

The scope of `crates/platform` grows from "hardware decoding / encoding" to **"OS media: hardware
decoding / encoding and OS media capture (screen, camera)"**, under the same containment rules as
ADR 0001, unchanged:

1. `unsafe` only in the FFI modules, each with `#[allow(unsafe_code)]`: the new ones are
   `capture::screen` and `capture::camera` (macOS only). `capture` itself (the factory, the clock
   mapping, the permission messages) is safe code.
2. A `// SAFETY:` comment on every `unsafe` block.
3. A safe public API: the crate exposes capture only as implementations of the engine's
   `VideoInputFactory` / `VideoInput` traits; no raw pointers, Objective-C or CoreMedia types in
   public signatures; every failure is a `Result` (`CaptureError`).
4. No panic crosses the FFI boundary: the ScreenCaptureKit stream output and the AVFoundation
   sample-buffer delegate run their Rust body under `catch_unwind`, and a panic becomes the
   input's error (`VideoInput::error`), which stops the recording with that error. Completion
   handlers (shareable-content enumeration, access requests) only send on a channel, and the
   waiting side gives up after 5 s, so a callback that never comes cannot hang the app.
   And no Objective-C exception crosses it the other way: Rust cannot unwind through one (the
   process aborts with "Rust cannot catch foreign exceptions", which is how a camera that rejected
   its configuration took the app down on 2026-10-10). Every AVFoundation, ScreenCaptureKit and
   AppKit call runs under `capture::catch_objc` (`objc2::exception::catch`), which turns an
   exception into a `CaptureError` naming the call and the exception's reason, and logs it.
5. The crate compiles on every target. The new bindings are macOS-only dependencies; on other
   systems `register()` installs no capture factory and the engine reports screen and camera
   capture as not available.

Permissions (macOS TCC: Screen Recording, Camera, Microphone) are checked with
`CGPreflightScreenCaptureAccess` and `AVCaptureDevice.authorizationStatus`; an undetermined
permission is requested (`CGRequestScreenCaptureAccess`, `requestAccessForMediaType`) without
waiting for the answer, and every non-granted state is a typed error whose message names the
System Settings ▸ Privacy & Security pane. Nothing in FilmCraft changes System Settings, signs
binaries or asks for entitlements a terminal-launched, unsigned binary cannot have.

The crate now depends on `filmcraft-engine` (L4; platform is L5), where the capture traits live,
the same way it implements `filmcraft_export::VideoEncoder` and `filmcraft_codecs::VideoDecoder`.

### Bindings

New (MIT / Apache-2.0 / Zlib, the same `objc2` 0.6 / `objc2-*` 0.3.2 generation as the lockfile):

| Crate | Version | For |
|---|---|---|
| `objc2-screen-capture-kit` | 0.3.2 | `SCShareableContent`, `SCDisplay`, `SCWindow`, `SCContentFilter`, `SCStream`, `SCStreamConfiguration`, `SCStreamOutput` |
| `objc2-av-foundation` | 0.3.2 | `AVCaptureDevice`, `AVCaptureDeviceDiscoverySession`, `AVCaptureSession`, `AVCaptureDeviceInput`, `AVCaptureVideoDataOutput`, authorization status |

Both require `objc2 >= 0.6.2, < 0.8` and `objc2-foundation ^0.3.2`; the lockfile has `objc2`
0.6.4 and `objc2-foundation` 0.3.2, so nothing is upgraded. The capture modules also name, as
direct macOS dependencies, crates that are already in the lockfile through the existing bindings
and winit / wgpu: `objc2` (0.6, for `define_class!` delegates), `objc2-foundation` (0.3.2),
`block2` (0.6, completion handlers), `dispatch2` (0.3, the delegate queues) and
`objc2-core-graphics` (0.3.2, the screen-capture access check). No crate outside the lockfile is
added besides the two above.

## Consequences

- Screen and camera recording on macOS without OBS or Descript, written by FilmCraft's own MOV
  writer through the hardware encoder (or FilmCraft's H.264 encoder).
- More FFI to review in `crates/platform`, now including callbacks from OS threads. Anything that
  wants `unsafe` for another purpose (system audio capture through a driver, global input hooks
  for F14 click tracking) still needs its own decision; ScreenCaptureKit system audio and click
  events read from the capture stream would fall under this one.
- Windows (Windows.Graphics.Capture, Media Foundation capture) and Linux (PipeWire through the
  desktop portal) are future implementations of the same traits under this record.
