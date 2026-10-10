# Recording (screen, cameras, microphones, system audio)

Status: macOS screen, window, camera, microphone and system-audio recording, several cameras and
microphones at once, recording settings, live camera previews, the recording border and areas of
a display, per-camera rotate, headless synthetic sources, the Record panel
(`openspec/changes/recording/`). Windows and Linux record microphones only.

Record your screen, up to four cameras and up to four microphones without leaving FilmCraft, in
any mix: screen only, a camera only, a microphone only, or all of them at once. Each source is
written to its own file, all of them stamped on one clock; Stop imports the files and builds a new
sequence with the screen on V1, the cameras on V2, V3… and the microphones on A1, A2… (the system
audio after them), already in sync, as one undo step. Then transcribe, edit by text and put a face
in a corner with [layouts](layouts.md).

## The Record panel

Window ▸ Record, or the red dot at the right end of the Program monitor's transport.

| Control | Does |
|---|---|
| Screen | Off, a display (name and pixel size), Area of <display>… (drag a part of it: see Border and areas), or a window (app — title); with an area: "Area 1280×720" and Edit… |
| Camera rows | per camera: its live picture (see Preview), the camera (or Off), Quality 720p / 1080p / 4K / Native (the camera's best), `−` (remove the row), Mirror, Rotate 0° / 90° / 180° / 270°, Offset … ms (see Sync), Pop out |
| Microphone rows | per microphone: Off, Default Input (Settings ▸ Audio Hardware, or the Voice-Over source), or a device; `−` |
| + Camera / + Microphone | add a row (at most four of each); a new camera row takes the next camera not chosen yet and the camera defaults of the settings |
| Name | the recording's name (empty: `Recording <n>`) |
| ● Record / ■ Stop 00:12 | starts (after the countdown); reads `Starting screen capture…` (disabled) until every source is live, then becomes Stop with the elapsed time |
| Cancel | stops and deletes the files (or cancels the countdown) |
| Refresh | lists the devices again (a camera plugged in, a window opened, a display woken) |
| Settings ▸ | the recording settings below, the same values as Settings ▸ Recording |

While recording the panel stays open, shows frames and dropped frames per video source and the
microphone level, and the status bar reads `Recording · 00:12 · screen 360 f / camera 358 f`.
During the countdown the button and the status bar read `Recording in 3…`; the button or Esc
cancels it. After it (or at once without one) the sources start, and until every one of them
delivers, the button (disabled) and the status bar read `Starting screen capture…` (`Starting the
camera…`, `Starting the microphone…` without a screen); Cancel stops them. This usually takes
well under a second; the first screen recording after FilmCraft starts can take several seconds
(macOS starts its screen-capture helper), and FilmCraft waits up to 20 s. `Recording · 00:00`
appears when the recording begins. The first time the panel opens it picks the first display, the first camera and the
default microphone. A refused start (a permission, a missing device, the same camera twice) is
shown in red in the panel. There are no keyboard shortcuts for recording.

## Settings

Settings ▸ Recording, the Settings section at the bottom of the Record panel and the
`record.settings` command show and change the same values (the preferences' `recording.*` keys,
saved with the other preferences). `record.start` uses them as its defaults; its own parameters
win. Every value is checked when it is set (`record.settings` refuses a bad one and names the
field) and put back in range when the preferences are loaded.

| Group | Field (`recording.<key>`) | Choices | Default | What it does |
|---|---|---|---|---|
| Screen | Frame rate (`screenFps`) | 15 / 24 / 30 / 60 | 30 | ScreenCaptureKit `minimumFrameInterval` = 1/fps; the file's frame rate |
| Screen | Resolution (`screenResolution`) | Native / 1440p / 1080p / 720p | Native | scales a taller screen down to that many rows, keeping its aspect (ScreenCaptureKit `width` / `height`; a source that cannot scale is scaled before encoding) |
| Screen | Show cursor (`showCursor`) | on / off | on | ScreenCaptureKit `showsCursor` |
| Screen | System audio (`systemAudio`) | on / off | off | the sound the system plays (ScreenCaptureKit `capturesAudio`, macOS 13+, FilmCraft's own sound left out) to `… - System Audio.wav` on the next free audio track; only with a screen source (greyed in the panel without one) |
| Camera | Quality (`cameraQuality`) | 720p / 1080p / 4K / Native | 1080p | the default of new camera rows: the AVFoundation session preset |
| Camera | Frame rate (`cameraFps`) | 24 / 30 / 60 | 30 | `activeVideoMin/MaxFrameDuration` when the camera's format can do it, else the camera's best (said in the sidecar) |
| Camera | Mirror (`cameraMirror`) | on / off | off | the default of new camera rows (see Sync) |
| Camera | Rotate (`cameraRotate`) | 0 / 90 / 180 / 270 | 0 | the default of new camera rows: turns the clip clockwise (see Rotate) |
| Encoding | Codec (`codec`) | H.264 / HEVC / ProRes 422 | H.264 | HEVC is offered only when the hardware encoder reports it (VideoToolbox) and falls back to H.264 otherwise; ProRes 422 is FilmCraft's own encoder: every frame a keyframe, large files, the friendliest to edit |
| Encoding | Quality (`quality`) | Low / Medium / High / Max | High | the H.264 / HEVC bitrate (table below); ProRes ignores it |
| Encoding | Keyframe every (`keyframeSeconds`) | 1 / 2 / 4 s | 2 s | 2 s scrubs best in an editor |
| Encoding | Hardware encoder (`hardwareEncoder`) | on / off | on | off = FilmCraft's own H.264 encoder, which records above 1080p at most at 15 fps |
| Audio | Sample rate (`sampleRate`) | 44.1 / 48 / 96 kHz | 48 kHz | asked of each microphone; a device that cannot do it records at its own rate (the sidecar says so); system audio is 48 kHz (ScreenCaptureKit's rates) |
| Audio | Channels (`channels`) | Mono / Stereo | Mono | mono = the Voice-Over input channel; stereo = the device's first two channels (a mono device twice) |
| Audio | Format (`audioFormat`) | 16-bit / 24-bit / 32-bit float WAV | 32-bit float | the WAV sample format of microphones and system audio |
| Audio | Auto gain (`autoGain`) | on / off | off | a running-RMS normaliser (about 0.3 s) toward −18 dBFS that moves at most 2 dB per second and at most ±20 dB, holds during silence (below −60 dBFS) and limits peaks to full scale; applied to the written microphone file only, never to the level meter; marked in the sidecar |
| Behaviour | Countdown (`countdownSeconds`) | Off / 3 / 5 / 10 s | 3 s | the panel's Record button counts down (`Recording in 3…`); Esc or the button cancels |
| Behaviour | Stop after (`stopAfterMinutes`) | Off (0) or 1–180 minutes | Off | stops (and builds the sequence) by itself |
| Behaviour | Open the sequence after Stop (`openSequence`) | on / off | on | off: the new sequence is only selected in the Project panel |
| Behaviour | Output folder (`outputFolder`) | a folder (Browse… / Choose…) | empty | where the files go; empty = the scratch-disk rule under Files |

**Bitrates** (Mbit/s; the 1080p 30 fps rate of the quality, scaled by `(pixels / 1920·1080)^0.87`
and by `fps / 30`, at least half, between 0.5 and 400 Mbit/s; the encoder may peak at 1.5×):

| Quality | 720p 30 | 1080p 30 | 1080p 60 | 1440p 30 | 2160p 30 | 2160p 60 |
|---|---|---|---|---|---|---|
| Low | 3.0 | 6 | 12 | 9.9 | 20 | 40 |
| Medium | 5.9 | 12 | 24 | 19.8 | 40 | 80 |
| High | 9.9 | 20 | 40 | 33 | 67 | 134 |
| Max | 19.8 | 40 | 80 | 66 | 134 | 267 |

## Files

Recordings go to Settings ▸ Recording ▸ Output folder when set, else Project Settings ▸ Scratch
Disks ▸ Captured Audio and Video, else next to the project file, else `Recordings` in the data
directory (the same rule as voice-over):

- `Recording 3 - Screen.mov`, `Recording 3 - Camera.mov`, `Recording 3 - Camera 2.mov`…: H.264,
  HEVC or ProRes 422 in QuickTime (Settings ▸ Encoding), through the hardware encoder
  (VideoToolbox) when there is one and it is on, else FilmCraft's own encoder. The file grows while
  recording; its index is written at Stop.
- `Recording 3 - Mic.wav`, `Recording 3 - Mic 2.wav`…, `Recording 3 - System Audio.wav`: WAV in the
  chosen format, rate and channels.
- A sidecar per file, `Recording 3 - Screen.recording.json`:

```json
{
  "version": 1, "recording": "Recording 3", "source": "screen", "index": 1, "file": "Recording 3 - Screen.mov",
  "device": {"id": "1", "name": "Built-in Display"},
  "clock_start_ns": 1791631234567890123, "first_sample_ns": 12000000, "last_sample_ns": 3012000000,
  "warmup_ns": 410000000,
  "dropped": 0, "bytes": 5123456, "frames": 90, "width": 1920, "height": 1080, "fps": 30,
  "encoder": "VideoToolbox H.264", "bitrate_kbps": 20000, "quality": "high", "keyframe_seconds": 2,
  "hardware_encoder": true, "show_cursor": true, "resolution": "native", "events": []
}
```

`clock_start_ns` is the wall-clock time when the recording began, the instant every source was
live (the same in every sidecar of one recording, and `clockStartNs` of `record.start`);
`first_sample_ns` / `last_sample_ns` are on the recording clock, relative to it, so
`first_sample_ns` is under one frame for every source. `warmup_ns` is how long the source took
from its start to its first frame or audio block (a microphone typically 0.5–0.7 s, the screen
0.2–0.9 s, more the first time).
`source` is `screen`, `camera`, `mic` or `systemAudio`, `index` its number among the sources of that
kind (Camera 2 has `2`). A screen's sidecar has `area` (`[x, y, w, h]` in display pixels) when
only an area was recorded. A camera's sidecar also has `camera_offset_ms`, `rotate` (+
`rotate_note`) and `mirror` (+
`mirror_note`); audio sidecars have `sample_rate`, `requested_sample_rate`, `channels`, `format`
(`s16` / `s24` / `f32`), `samples` (and `auto_gain` for microphones) instead of the picture fields.
`notes` lists what could not be followed (HEVC not available, the 15 fps cap, a device's own sample
rate, the auto gain at the end). `events` is reserved for click tracking (later).

A screen that does not change sends no frames: the file then holds one long frame, and frames
are never late, only longer. When the encoder cannot keep up, the oldest waiting frame is dropped
(counted in `dropped`) so capture never stalls. System audio that pauses (nothing playing) is
filled with silence so the file stays on the clock.

## Sync

All tracks start together, as in other screen recorders: `record.start` starts every screen,
camera and microphone, waits until each one has delivered its first frame or audio block, and
the recording begins at the **latest** of those first samples. Every frame and every block of
audio is stamped on one clock; media time 0 of every file is that same instant. What a source
captured before it (a screen that was live while the microphone was still opening) is left out:
frames before the encoder (not counted as dropped), audio to the sample. So every clip starts at
0 in the new sequence, and `warmup_ns` in each sidecar shows how long each source took.

The wait is at most 20 s overall (`START_TIMEOUT`; the platform gives ScreenCaptureKit's
`startCapture` and the camera session's start as long, and 5 s to listing and permission calls).
A source that delivers nothing in that time fails the start with its name, for example
"microphone 'Yeti Stereo Microphone' delivered no audio within 20 s"; the other sources are
stopped and their files deleted. System audio is not waited for (nothing may be playing): it is
filled with silence from the start.

Tracks: the screen on V1, then the cameras in row order (V2, V3…; without a screen the first
camera is on V1), the microphones in row order on A1, A2…, the system audio on the next audio
track. The sequence takes the screen's size and rate (else the first camera's) and the first
microphone's sample rate.

The clock cannot see delay inside a camera (a USB webcam may deliver a frame 50–150 ms after the
light hit the sensor): that is all the camera offset is for now, since the start latency of every
source is taken care of. Each camera row's Offset … ms (`cameraOffsetsMs`, one per camera; the
scalar `cameraOffsetMs` applies to every camera without its own) moves that camera: a negative
value moves it earlier, the usual fix when the face is late. If that would start a camera before
0, everything moves so the earliest clip starts at 0. The value is kept in the camera's sidecar
and in the comment of the sequence's `Recording` marker (`camera offset -40 ms, camera 2 offset
100 ms`).

Mirror puts a Horizontal Flip effect first on that camera's clip (the file stays as the camera saw
it), so it can be turned off later in Effect Controls.

The imported items carry metadata `Recording`, `Recording Source` (`screen`, `camera`, `camera2`,
`mic`, `mic2`, `systemAudio`…) and `Recording Offset`, and go to a `Recordings` bin. Undo removes the
sequence, the clips, the bin entries and the items in one step; the files stay on disk.

## Rotate

A camera that is mounted on its side or upside down (some USB cameras come out turned) gets its
row's Rotate: 0°, 90° (clockwise), 180° or 270° (`cameras: [{…, rotate}]`, default
`cameraRotate`). Like Mirror, the file stays as the camera saw it: the clip's Motion turns it
(Rotation) and scales it so the whole turned picture fits the sequence frame, never larger than
the file (Scale; a 16:9 camera turned 90° next to a 16:9 screen is 56.25 %: a portrait picture
with bars at the sides, nothing cropped). When the turned camera leads the sequence (no screen),
the sequence itself is made upright (1080 × 1920 for a 1080p camera) and the clip fills it at
100 %. Change or remove it later in Effect Controls ▸ Motion. The preview is turned the same way.

## Preview

While the Record panel is open, each camera row shows its camera live in a 16:9 box about
240 px wide (letterboxed), at the camera's frame rate, mirrored and turned like its clip will be
("is something in my teeth, is the angle bad"). Pop out opens it in a small preview window
(320 pt wide, resizable, always on top, moved by its title bar; its close box puts it back in the
panel) that keeps showing the camera during the recording even when the panel is closed. A
preview stops when its row is removed or set to Off, and when the panel closes with nothing
recording.

Under the hood the panel asks `record.preview` for the cameras of its rows, at the size and rate
each row records at. The camera then runs without writing anything; its latest picture
(downscaled to at most 640 pixels wide, RGBA) is kept for the UI, which uploads it into a texture
only when a new frame arrived. Record reuses the running capture session: the recording's frames
come from the same camera session, re-stamped onto the recording's clock, so the device is not
opened twice and there is no gap. Only a recording that asks the camera for another size or rate
than its preview (an agent's `record.start` with other parameters) restarts the camera once. A
preview asked for during a recording shares the recording's session at the recording's size.

## Border and areas

Descript-style, a border shows what is recorded: while the panel is open with a screen chosen, a
3 pt grey frame hugs the display's edges (or the window's, following it every 0.5 s as it moves,
or the area's); recording turns it red with a small `● REC 00:12` pill at the top. It is a
borderless, transparent, always-on-top window that lets every click through (titled
`FilmCraft Recording Overlay`). It goes away when the screen is set to Off, when the panel
closes with nothing recording, and when the recording stops (it comes back when a screen is
chosen again, the panel is reopened or Record is pressed). Esc never closes it. macOS keeps ordinary
windows below the menu bar, so FilmCraft lifts the border window just above it (the status-bar
window level) to frame the whole display; the `● REC` pill sits below the menu bar. If the system
still keeps it off a part of the display, the border frames (and an area is drawn on) what it
covers, mapped from where the window really is (`ui.record.overlay.window`).

**Area of <display>…** turns the border of that display into a drawing surface (it takes the
mouse, the pointer is a crosshair, "Drag the area to record (Esc cancels)"): drag the rectangle
(snapped to even pixels, at least 64 × 64); on release the area is set, the border lets the mouse
through again and frames the area, and the panel shows "Area 1280×720" with Edit… to draw it
again. Only that part of the display is recorded, at the display's native pixel size:
ScreenCaptureKit's `sourceRect` is in points, so the area (in pixels) is divided by the display's
backing scale (its pixel width, from the display mode, over `SCDisplay`'s width in points: 2 on a
Retina display); the stream's `width` / `height` are the area's pixel size. `record.start
{screen: {display, area: [x, y, w, h]}}` takes it in display pixels; an area outside the display,
smaller than 64 or with a window is refused.

**Never in the file.** The border and the preview windows are for the person, not the file. A
display recording is started with ScreenCaptureKit's `initWithDisplay:excludingWindows:` listing
this process's windows (`owningApplication.processID` = FilmCraft's) whose title starts with
`FilmCraft Recording Overlay` or `FilmCraft Camera Preview`; every other window, FilmCraft's own
main window included, stays recordable (record FilmCraft itself for a tutorial). The list is made
from the windows that exist when the stream starts, so the panel shows the border (and the
pop-outs you opened) before it starts the screen: Record waits until the border has been on screen
for a few frames. A border or preview window that appears later (a pop-out opened during the
recording, or a recording started by an agent with the panel closed) is added to the running
stream's filter a few frames after it appears (`updateContentFilter`, no restart); it can show in
the file for those few frames. A window source records only that window, so nothing needs leaving
out.

Headless sessions (tests, `--control` scripts without a window) draw the border inside the main
window, the display scaled to fit, so `ui.drag` on `record.overlay` draws an area.

## Permissions (macOS)

A camera or screen that rejects its configuration raises an Objective-C exception inside macOS;
FilmCraft catches it (`docs/adr/0002-platform-capture-ffi.md` §4) and shows it in the Record panel
as that source's error ("starting the camera: NSInvalidArgumentException …") instead of quitting.
If you see one, the message is what to report.

macOS asks once per app for Screen Recording (which also covers system audio), Camera and
Microphone. FilmCraft checks first and never waits on the system: when a permission is missing it
shows the system prompt (the first time) and the start fails with a message naming the pane, for
example "open System Settings ▸ Privacy & Security ▸ Screen & System Audio Recording, allow it,
then press Record again". When FilmCraft is started from a terminal, macOS asks for (and lists)
the terminal app instead, and a terminal that declares no camera usage description (the Claude
desktop app's terminal pane, for one) never shows the camera prompt at all. Run the app bundle
instead (`cargo xtask bundle`, then `open -a "target/release/FilmCraft Dev.app"`, see
[contributing.md](contributing.md) "macOS app bundle"), so the permissions are FilmCraft's own.
Screen Recording takes effect after FilmCraft is restarted.
`record.devices` lists no displays until Screen Recording is allowed and says why in `error`; it
also says so when macOS lists no display because the display is asleep or the screen is locked
(wake it and press Refresh). FilmCraft never changes System Settings.

## Commands

| Command | Params | Result |
|---|---|---|
| `record.devices` | – | `{displays: [{id, name, width, height}], windows: [{id, title, app}], cameras: [{id, name, formats: [{width, height, fps}]}], microphones: [name], factory, permissions: {screen, camera, microphone}, systemAudio, error?}` |
| `record.start` | `screen?: {display: id, area?: [x, y, w, h]} \| {window: id}` (+ `fps?` 1–60, `resolution?`, `cursor?`, `systemAudio?`), `cameras?: [{device: id, quality?, width?, height?, fps?, mirror?, rotate?}]` (or `camera: {…}`), `mics?: [{device?: name}]` (or `mic: {…}`), `name?`, `dir?`, `countdown?` (0–10 s), `wait?` (default true), `settings?` (any of the settings fields, for this recording only) | once every source is live: `{recording, name, clockStartNs, dir, files: [{kind, key, path, sidecar}], warmupNs: {key: ns}, notes}`; with a countdown `{recording: false, countdown}`; with `wait: false` at once `{recording: false, starting: true, label}` (the outcome comes as a `started` / `startFailed` event of `record.status`) |
| `record.preview` | `camera: {device, quality?, width?, height?, fps?} \| null`, or `cameras: [{…}]` (at most four); `{}` only reports | the listed cameras run live, the others stop: `{preview: [ids], cameras: [{device, width, height, fps, frames, recording, error}]}` |
| `record.status` | – | `{recording, name?, elapsed, sources: [{kind, key, device, frames, dropped, bytes, level?}], stopAfterMinutes, notes, error?, preview: [camera ids]}`; during a countdown `{recording: false, countdown: {seconds, remaining}}`; while the sources start `{recording: false, starting: true, label: "Starting screen capture…"}`; also runs what is due (a finished start, the countdown's start, Stop after) and reports it as `event` |
| `record.stop` | `discard?`, `cameraOffsetMs?` (−5000…5000), `cameraOffsetsMs?: [..]` | `{placed, name, sequence, opened, items: {screen?, camera?, camera2?, mic?, mic2?, systemAudio?}, clips: {key: {clip, start}}, offsets: {key: ticks}, cameras, mics, cameraOffsetsMs, files, errors, notes}`; during a countdown `{placed: false, cancelled: true}` |
| `record.cancel` | – | `{placed: false, discarded: true}` (`cancelled: true` for a countdown or a start whose sources are starting) |
| `record.settings` | `{get: true}` or `{set: {field: value, …}}` (merged) | `{settings: {…}, hevcAvailable, systemAudioAvailable}` |

`record.start` is refused while recording, starting or counting down, during a voice-over, with no source,
for an unknown display / window / camera / microphone, the same camera or microphone twice, more
than four cameras or microphones, `fps` outside 1–60, a size outside 16–8192, a `countdown` outside
0–10, a bad `settings` value, or a name that is empty, longer than 100 characters, or has `/`, `\`,
`:`, a control character, `..` or a leading `.`. Devices are checked before a countdown starts. A
voice-over cannot start while recording (they share the microphone). The countdown and Stop after
run in the host's frame loop (the desktop app) or when `record.status` is polled (CLI, MCP,
control channel). `record.start` counts down only when given `countdown`: the panel passes the
setting, agents start at once. Without `wait: false`, `record.start` returns when every source
is live (or the start failed), usually within a second and at most 20 s; the panel and a
counted-down start use `wait: false`, so the app never waits for the sources. `record.cancel` /
`record.stop` while the sources start stop them and delete the files. In headless sessions (CLI, MCP, tests) the sources are synthetic:
`synthetic:display` (1280 × 720, with a 440 Hz tone as system audio), `synthetic:window`,
`synthetic:camera` and `synthetic:camera2` (640 × 360) and the `Synthetic Input` and
`Synthetic Input 2` microphones; their frames carry the frame index in the top row so tests check
sync by pixel. A second microphone opens its own input (`AudioInput::spawn`).

## Automation ids

| Id | Element |
|---|---|
| `record.panel` | the panel window |
| `record.panel.screen`, `.screen.<i>` | Screen picker and its entries (0 = Off) |
| `record.panel.camera.<n>.device`, `.device.<i>` | camera row `n` (1-based): its picker (0 = Off) |
| `record.panel.camera.<n>.quality` (+ `.<i>`), `.mirror`, `.rotate` (+ `.<i>`), `.offset`, `.remove` | its quality, Mirror, Rotate, offset, `−` |
| `record.panel.camera.<n>.preview`, `.popout` | its live picture (label `1280×720 @ 30 fps · frame 42`, the turned size), Pop out / Pop in |
| `record.preview.<n>` | the pop-out window's picture (headless sessions; natively it is its own window) |
| `record.panel.screen.area.<display>`, `record.panel.screen.area`, `record.panel.screen.area.edit` | Area of <display>… (in the Screen list), "Area W×H" (or `drawing`), Edit… |
| `record.overlay` | the border (label `idle display:1 0,0 3024×1964`: state, target, the framed rectangle in pixels; natively its rect is in desktop points) |
| `record.panel.mic.<n>.device` (+ `.<i>`), `record.panel.mic.<n>.remove` | microphone row `n` |
| `record.panel.camera.add`, `record.panel.mic.add` | + Camera, + Microphone |
| `record.panel.name` | Name |
| `record.panel.record`, `record.panel.cancel`, `record.panel.refresh`, `record.panel.close` | buttons |
| `record.panel.counters`, `record.panel.level`, `record.panel.error` | live counters (or the last result), mic level, error |
| `record.panel.settings` | the Settings section header |
| `record.panel.settings.<key>` (+ `.<i>` for choices), `record.panel.settings.outputFolder.choose` | its fields (`screenFps`, `screenResolution`, `showCursor`, `systemAudio`, `cameraQuality`, `cameraFps`, `cameraMirror`, `cameraRotate`, `codec`, `quality`, `keyframeSeconds`, `hardwareEncoder`, `sampleRate`, `channels`, `audioFormat`, `autoGain`, `countdownSeconds`, `stopAfterMinutes`, `openSequence`, `outputFolder`) |
| `settings.recording.<key>`, `settings.category.recording` | the same fields in Settings ▸ Recording |
| `program.transport.record` | the Program monitor's red dot |

UI command `window.record` opens the panel. `ui.set {"record": {...}}` merges into the panel state
(`open`, `screen` = `""` / `display:<id>` / `window:<id>`, `screenArea` = `[x, y, w, h]` or
`null`, `drawing`, `cameras: [{device, quality` = `720p` / `1080p` / `4k` / `native`, `mirror,
rotate, offsetMs, popout}]`, `mics: [{device}]` (`""` / `default` / name), `name`,
`settingsOpen`, `overlayDismissed`); `ui.inspect` shows it under `ui.record`, with `live` (the
status line or the countdown), `last` (the last result) and `overlay` (the border as shown:
`{state: idle | recording | drawing, target, rect: [x, y, w, h] pixels, frame: [x, y, w, h]
desktop points}`, `null` when hidden).

## Not there yet

- Several screens at once, pause / resume, a teleprompter.
- Click tracking and auto zoom (the sidecar's `events` list is ready for a click log).
- Camera audio (a camera's microphone can be picked as a microphone).
- Windows (Windows.Graphics.Capture) and Linux (PipeWire) screen and camera capture.
- Recovering a file whose index was never written (FilmCraft quit while recording).
- Changing a camera offset after Stop (move the clip, or record again).
- Continuity Camera and cameras that macOS only lists through discovery sessions may be missing
  from the camera list.
