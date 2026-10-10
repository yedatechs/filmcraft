# Recording (screen, camera, microphone)

Status: macOS screen, window, camera and microphone recording, headless synthetic sources, the
Record panel (`openspec/changes/recording/`). Windows and Linux record the microphone only.

Record your screen and your camera at once without leaving FilmCraft. Each source is written to its
own file, all of them stamped on one clock; Stop imports the files and builds a new sequence with
the screen on V1, the camera on V2 and the microphone on A1, already in sync, as one undo step.
Then transcribe, edit by text and put the face in a corner with [layouts](layouts.md).

## The Record panel

Window ▸ Record, or the red dot at the right end of the Program monitor's transport.

| Control | Does |
|---|---|
| Screen | Off, a display (name and pixel size), or a window (app — title) |
| Camera | Off or a camera; Quality 720p / 1080p / 4K / Native (the camera's best) |
| Microphone | Off, Default Input (Settings ▸ Audio Hardware, or the Voice-Over source), or a device |
| Name | the recording's name (empty: `Recording <n>`) |
| Offset camera by … ms | moves the camera clip when Stop builds the sequence (see Sync) |
| ● Record / ■ Stop 00:12 | starts; becomes Stop with the elapsed time |
| Cancel | stops and deletes the files |
| Refresh | lists the devices again (a camera plugged in, a window opened) |

While recording the panel stays open, shows frames and dropped frames per video source and the
microphone level, and the status bar reads `Recording · 00:12 · screen 360 f / camera 358 f`. The
first time it opens it picks the first display, the first camera and the default microphone. A
refused start (a permission, a missing device) is shown in red in the panel. There are no keyboard
shortcuts for recording.

## Files

Recordings go to Project Settings ▸ Scratch Disks ▸ Captured Audio and Video, else next to the
project file, else `Recordings` in the data directory (the same rule as voice-over):

- `Recording 3 - Screen.mov`, `Recording 3 - Camera.mov`: H.264 in QuickTime, through the hardware
  encoder (VideoToolbox) when there is one, else FilmCraft's own encoder (at most 15 fps above
  1080p). Keyframe every 2 s; about 6 Mbit/s at 720p, 12 at 1080p, 40 at 4K. The file grows while
  recording; its index is written at Stop.
- `Recording 3 - Mic.wav`: 32-bit float mono (Voice-Over Record Settings ▸ Input channel).
- A sidecar per file, `Recording 3 - Screen.recording.json`:

```json
{
  "version": 1, "recording": "Recording 3", "source": "screen", "file": "Recording 3 - Screen.mov",
  "device": {"id": "1", "name": "Built-in Display"},
  "clock_start_ns": 1791631234567890123, "first_sample_ns": 41000000, "last_sample_ns": 3041000000,
  "dropped": 0, "bytes": 5123456, "frames": 90, "width": 1920, "height": 1080, "fps": 30,
  "encoder": "VideoToolbox H.264", "events": []
}
```

`clock_start_ns` is the wall-clock time when the recording started (the same in every sidecar of
one recording); `first_sample_ns` / `last_sample_ns` are on the recording clock, relative to it.
The camera's sidecar also has `camera_offset_ms`; the microphone's has `sample_rate`, `channels`
and `samples` instead of the picture fields. `events` is reserved for click tracking (later).

A screen that does not change sends no frames: the file then holds one long frame, and frames
are never late, only longer. When the encoder cannot keep up, the oldest waiting frame is dropped
(counted in `dropped`) so capture never stalls.

## Sync

Every frame and every block of audio is stamped on one clock taken when Record is pressed. Each
source starts in the new sequence at its first sample time minus the earliest first sample time,
at the next frame boundary with the clip's source In moved by the difference (the same placement
Merge Clips uses), so media time 0 of each file lands exactly where it was captured.

The clock cannot see delay inside a camera (a USB webcam may deliver a frame 50–150 ms after the
light hit the sensor). Offset camera by … ms (`cameraOffsetMs`) moves the camera that much: a
negative value moves it earlier, the usual fix when the face is late. If that would start the
camera before 0, everything moves so the earliest clip starts at 0. The value is kept in the
camera's sidecar and in the comment of the sequence's `Recording` marker.

The imported items carry metadata `Recording`, `Recording Source` and `Recording Offset`, and go
to a `Recordings` bin. Undo removes the sequence, the clips, the bin entries and the items in one
step; the files stay on disk.

## Permissions (macOS)

macOS asks once per app for Screen Recording, Camera and Microphone. FilmCraft checks first and
never waits on the system: when a permission is missing it shows the system prompt (the first
time) and the start fails with a message naming the pane, for example "open System Settings ▸
Privacy & Security ▸ Screen & System Audio Recording, allow it, then press Record again". When
FilmCraft is started from a terminal, macOS asks for (and lists) the terminal app instead. Screen
Recording takes effect after FilmCraft is restarted. `record.devices` lists no displays until
Screen Recording is allowed and says why in `error`. FilmCraft never changes System Settings.

## Commands

| Command | Params | Result |
|---|---|---|
| `record.devices` | – | `{displays: [{id, name, width, height}], windows: [{id, title, app}], cameras: [{id, name, formats: [{width, height, fps}]}], microphones: [name], factory, permissions: {screen, camera, microphone}, error?}` |
| `record.start` | `screen?: {display: id} \| {window: id}` (+ `fps?` 1–60, default 30), `camera?: {device: id, width?, height?, fps?}`, `mic?: {device?: name}`, `name?`, `dir?` | `{recording, name, clockStartNs, dir, files: [{kind, path, sidecar}]}` |
| `record.status` | – | `{recording, name?, elapsed, sources: [{kind, device, frames, dropped, bytes, level?}], error?}` |
| `record.stop` | `discard?`, `cameraOffsetMs?` (−5000…5000) | `{placed, name, sequence, items: {screen?, camera?, mic?}, clips: {kind: {clip, start}}, offsets: {kind: ticks}, files, errors}` |
| `record.cancel` | – | `{placed: false, discarded: true}` |

`record.start` is refused while recording, during a voice-over, with no source, for an unknown
display / window / camera / microphone, `fps` outside 1–60, a size outside 16–8192, or a name that
is empty, longer than 100 characters, or has `/`, `\`, `:`, a control character, `..` or a leading
`.`. A voice-over cannot start while recording (they share the microphone). In headless sessions
(CLI, MCP, tests) the sources are synthetic: `synthetic:display` (1280 × 720), `synthetic:window`,
`synthetic:camera` (640 × 360) and the `Synthetic Input` microphone; their frames carry the frame
index in the top row so tests check sync by pixel.

## Automation ids

| Id | Element |
|---|---|
| `record.panel` | the panel window |
| `record.panel.screen`, `.screen.<i>` | Screen picker and its entries (0 = Off) |
| `record.panel.camera`, `.camera.<i>` | Camera picker |
| `record.panel.quality`, `.quality.<i>` | Quality |
| `record.panel.mic`, `.mic.<i>` | Microphone picker |
| `record.panel.name`, `record.panel.offset` | Name, camera offset |
| `record.panel.record`, `record.panel.cancel`, `record.panel.refresh`, `record.panel.close` | buttons |
| `record.panel.counters`, `record.panel.level`, `record.panel.error` | live counters (or the last result), mic level, error |
| `program.transport.record` | the Program monitor's red dot |

UI command `window.record` opens the panel. `ui.set {"record": {...}}` merges into the panel state
(`open`, `screen` = `""` / `display:<id>` / `window:<id>`, `camera`, `quality` = `720p` / `1080p`
/ `4k` / `native`, `mic` = `""` / `default` / name, `name`, `offsetMs`); `ui.inspect` shows it
under `ui.record`, with `live` (the status line) and `last` (the last result).

## Not there yet

- Area selection (a part of a display), several screens at once, pause / resume.
- Click tracking and auto zoom (the sidecar's `events` list is ready for a click log).
- System audio and camera audio (the microphone records sound; a camera's microphone can be
  picked as the microphone).
- Windows (Windows.Graphics.Capture) and Linux (PipeWire) screen and camera capture.
- Recovering a file whose index was never written (FilmCraft quit while recording).
- Changing the camera offset after Stop (move the clip, or record again).
- Continuity Camera and cameras that macOS only lists through discovery sessions may be missing
  from the camera list.
