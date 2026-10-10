## ADDED Requirements

### Requirement: Each source is recorded to its own file
`record.start` SHALL record each requested source (screen: a display or a window; camera; microphone) to its own file in the capture folder (Project Settings ▸ Scratch Disks ▸ Captured Audio and Video, else next to the project, else the data directory's `Recordings` folder), named `<Name> - Screen.mov`, `<Name> - Camera.mov` and `<Name> - Mic.wav`, and SHALL refuse a request with no source.

#### Scenario: Three sources
- **WHEN** a recording is started with a screen, a camera and a microphone, and stopped after 2 s
- **THEN** three files exist, the two MOV files decode with FilmCraft's own decoders and last about 2 s, and the WAV file holds about 2 s of samples

### Requirement: One clock and a sidecar per file
Every sample of every source SHALL be stamped on one monotonic clock, and each file SHALL get a sidecar `<file stem>.recording.json` with `version`, `recording`, `source`, `file`, `device`, `clock_start_ns` (the same in every sidecar of a recording), `first_sample_ns`, `last_sample_ns`, `warmup_ns`, `dropped`, the picture or audio format, `encoder` (video), and an `events` list that is empty until click tracking exists.

### Requirement: The recording begins when every source is live
`record.start` SHALL start every screen, camera and microphone, then wait (at most 20 s overall, `START_TIMEOUT`) until each one has delivered its first frame or audio block. The recording clock SHALL start at the latest of those first-sample times, so that media time 0 of every file is the same instant: frames captured before it SHALL be left out before the encoder (not counted as dropped) and audio SHALL be trimmed to the sample (system audio, which may be silent, is not waited for and is filled with silence from the start). `clockStartNs` and `clock_start_ns` SHALL be that start, `first_sample_ns` SHALL then be under one frame for every source, and `warmup_ns` SHALL say how long each source took from its start to its first sample. A source that delivers nothing within the wait SHALL fail the start with its name ("microphone 'Yeti Stereo Microphone' delivered no audio within 20 s"), the other sources stopped and their files deleted. The platform SHALL give stream and session starts (`startCapture`, the camera session start) 20 s and keep 5 s (`OS_TIMEOUT`) for enumeration and permission calls; a start that times out SHALL say "did not answer within 20 s". While the sources start, `record.status` SHALL report `starting: true` and the Record panel and status bar SHALL show "Starting screen capture…" with the Record button disabled; the countdown, if any, SHALL run before the sources start and the elapsed time SHALL count from the recording's start. `record.start {wait: false}` SHALL return at once (`{starting: true}`) and report the outcome as a `started` / `startFailed` event of `record.status`; `record.cancel` / `record.stop` while starting SHALL stop the sources and leave no files.

#### Scenario: Sources with different start latencies
- **WHEN** a synthetic screen (0 ms), camera (300 ms) and microphone (700 ms) are recorded for 1.5 s
- **THEN** all three clips start at 0 in the new sequence, every sidecar has `first_sample_ns` under one frame and `warmup_ns` of about 0, 300 and 700 ms, and the screen file holds about 45 frames, its first one the screen's frame ~21

#### Scenario: A source that never delivers
- **WHEN** a microphone delivers no audio within the wait
- **THEN** `record.start` fails naming the microphone and no files remain

### Requirement: Capture never blocks and never crashes
Frames SHALL pass from the capture callback to the encoder through a bounded queue that drops the oldest frame when full; dropped frames SHALL be counted in `record.status` and the sidecar. Capture and encoder threads SHALL run under `catch_unwind`; a failure SHALL end up as the recording's `error` (in `record.status` and as the error of `record.stop`), never as a hang or a crash.

### Requirement: Stop imports and places the sources in sync, in one undo step
`record.stop` SHALL finish the files, import them, and create a sequence named after the recording with the screen on V1, the camera on V2 and the microphone on A1, each file's media time 0 (the recording's start) at sequence time 0, as one undo step. `cameraOffsetMs` SHALL move the camera clip by that many milliseconds (frame-aligned, with the source In absorbing the sub-frame difference, renormalised so nothing starts before 0): the delay inside a camera is invisible to the clock.

#### Scenario: A camera that starts late
- **WHEN** the synthetic camera starts 300 ms after the screen
- **THEN** the recording begins when the camera is live: both clips start at 0, the screen file leaves out its first ~300 ms, and what both show at the same sequence time was captured together

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

### Requirement: Several cameras and microphones, in any mix
`record.start` SHALL take `cameras: [...]` and `mics: [...]` (with `camera` / `mic` as one-element aliases), up to four of each, together with an optional screen, in any combination with at least one source, and SHALL refuse the same camera or microphone twice. Each camera and microphone SHALL be its own file and sidecar (`<Name> - Camera 2.mov`, `<Name> - Mic 2.wav`) on the one clock. `record.stop` SHALL place the screen on V1, the cameras on the next video tracks in order and the microphones on A1, A2… in order, each camera moved by its own offset (`cameraOffsetsMs`; the scalar `cameraOffsetMs` applies to every camera without one); a camera recorded with `mirror` SHALL get a Horizontal Flip effect on its clip.

#### Scenario: Two cameras and two microphones
- **WHEN** a screen, two synthetic cameras that start 100 ms and 400 ms late and two microphones are recorded and stopped with `cameraOffsetsMs: [0, 100]`
- **THEN** the sequence has the screen on V1, the cameras on V2 and V3 (the second at 100 ms, everything else at 0), the microphones on A1 and A2, and five files with sidecars

#### Scenario: The same device twice
- **WHEN** `record.start` names one camera (or microphone) twice
- **THEN** it is refused with an error saying the device is chosen twice, and nothing is recorded

### Requirement: Recording settings
The preferences SHALL hold the recording settings (screen frame rate 15/24/30/60, resolution Native/1440p/1080p/720p, show cursor, system audio; camera quality and frame rate and mirror for new camera rows; codec H.264/HEVC/ProRes 422, quality Low/Medium/High/Max, keyframe interval 1/2/4 s, hardware encoder; sample rate 44.1/48/96 kHz, mono/stereo, 16/24/32-bit float WAV, auto gain; countdown 0/3/5/10 s, stop after 0–180 minutes, open the sequence after Stop, output folder), with defaults and every value put back in range on load. `record.settings {get}` SHALL report them and `record.settings {set}` SHALL merge a change, refusing an invalid value with the field's name. `record.start` SHALL use them as defaults, its own parameters winning. Settings ▸ Recording (`settings.recording.*`) and the Record panel's Settings section (`record.panel.settings.*`) SHALL show and change the same values. HEVC SHALL be offered only when the hardware encoder reports it and SHALL fall back to H.264 (said in the sidecar) otherwise; the H.264 / HEVC bitrate SHALL follow the quality table in `docs/recording.md`.

#### Scenario: The settings are the defaults of a recording
- **WHEN** the settings say 15 fps, 720p, no cursor, ProRes, 44.1 kHz stereo 16-bit with auto gain and `record.start` gives the screen `fps: 10` and `settings: {audioFormat: "s24"}`
- **THEN** the screen file is ProRes 422 at 960 × 720 (from 1440 × 1080) and 10 fps, and the microphone file is 44.1 kHz stereo 24-bit, its sidecar marking the auto gain

### Requirement: Countdown
`record.start {countdown: n}` (0–10 s) SHALL check the sources and parameters at once and start the recording `n` seconds later (reported by `record.status` as `countdown.remaining`), on a clock tests can replace; `record.cancel` / `record.stop` during the countdown SHALL cancel it without recording. The Record panel SHALL count down by the Countdown setting, showing `Recording in 3…` on its button and in the status bar, and Esc SHALL cancel it.

### Requirement: Stop after
With Stop after set to `m` minutes, a recording SHALL stop by itself after `m` minutes and build its sequence as `record.stop` does, opening it only when Open the sequence after Stop is on.

### Requirement: System audio
With System audio on and a screen source, the sound the system plays SHALL be recorded (ScreenCaptureKit `capturesAudio` on macOS 13+, FilmCraft's own sound excluded) to `<Name> - System Audio.wav` on the recording clock, gaps filled with silence, and placed on the next free audio track. Where it cannot be captured the recording SHALL go on without it and say so in its `notes`, and the panel SHALL show the control disabled with "(not available yet)".

### Requirement: Live camera preview
`record.preview {camera: {device, quality?, width?, height?, fps?} | null}` (or `cameras: [...]`) SHALL run exactly the listed cameras without writing anything, keeping each camera's latest picture as RGBA at most 640 pixels wide for the UI, and `record.status` SHALL list them as `preview`. A recording of a previewed camera SHALL reuse its capture session (the device is not opened twice); only a recording that asks the camera for another size or rate SHALL restart it, once. While the Record panel is open (or a recording runs), each camera row SHALL show its camera live in a 16:9 thumbnail (`record.panel.camera.<n>.preview`, labelled `W×H @ fps fps · frame k`), mirrored and rotated like its clip; Pop out (`record.panel.camera.<n>.popout`) SHALL open an always-on-top, resizable preview window titled `FilmCraft Camera Preview — <camera>` that stays during a recording with the panel closed. The preview SHALL stop when its row is removed or set to Off, or when the panel closes with nothing recording.

#### Scenario: Recording while previewing
- **WHEN** the synthetic camera is previewed and then recorded for 1.5 s
- **THEN** the camera was opened once, the preview keeps updating during the recording and after Stop, and the camera file holds about 45 frames in index order with its first sample near the clock start

### Requirement: A border around what is recorded
While the Record panel is open with a screen chosen, a borderless, transparent, always-on-top window that lets the mouse through (titled `FilmCraft Recording Overlay`) SHALL frame the chosen display, the chosen window (following it every 0.5 s) or the chosen area with a 3 pt border: grey before recording, red with a `● REC mm:ss` pill while recording. It SHALL close when the screen is set to Off, when the panel closes with nothing recording and when the recording stops, and Esc SHALL not close it. Its state SHALL be in `ui.inspect` as `ui.record.overlay` (`state`, `target`, `rect`, `frame`) and its automation id is `record.overlay`.

### Requirement: Area of a display
The Screen picker SHALL offer "Area of <display>…" per display (`record.panel.screen.area.<display>`): the border becomes a drawing surface (crosshair, "Drag the area to record (Esc cancels)"); the dragged rectangle, in even display pixels and at least 64 × 64, SHALL become the area (`ui.record.screenArea`), shown in the panel as "Area W×H" with "Edit…". `record.start {screen: {display, area: [x, y, w, h]}}` SHALL record only that part of the display at its native pixel size (ScreenCaptureKit `sourceRect` in points = the area ÷ the display's backing scale), refuse an area that is not four whole numbers, smaller than 64 or outside the display, or given with a window, and write `area` in the sidecar.

#### Scenario: Drawing an area
- **WHEN** the synthetic 1280 × 720 display's area entry is chosen and a drag from (101, 99) to (741, 459) is made on `record.overlay`, then Record and Stop are pressed
- **THEN** `screenArea` is `[100, 98, 640, 360]`, the screen file and the sequence are 640 × 360 and the sidecar's `area` is `[100, 98, 640, 360]`

### Requirement: The border and previews are never in the file
A display recording SHALL leave out this process's windows whose titles start with `FilmCraft Recording Overlay` or `FilmCraft Camera Preview` (ScreenCaptureKit `initWithDisplay:excludingWindows:`, matched by the owning process id and the title), and only those: FilmCraft's main window stays recordable. The panel SHALL show the border before it starts a screen recording so the exclusion list built at the start holds it; a border or preview window that appears during a display recording SHALL be added to the running stream's filter (`updateContentFilter`), never by restarting the stream. A window source needs no exclusion.

#### Scenario: The exclusion list
- **WHEN** the window list holds FilmCraft's main window, its border, a pop-out preview, another app's window titled like the border and an untitled FilmCraft window
- **THEN** only the border and the pop-out preview are left out

### Requirement: Rotate per camera
`record.start` cameras SHALL take `rotate` (0 / 90 / 180 / 270, anything else refused; default the `cameraRotate` setting), shown as a Rotate picker in the camera row (`record.panel.camera.<n>.rotate`) and recorded in the sidecar; the file SHALL stay as captured and the clip's Motion SHALL turn it by that angle, scaled to fit the sequence frame without cropping and never enlarged; when a turned camera leads the sequence (no screen), the sequence SHALL be made upright. The preview SHALL be turned the same way.

#### Scenario: A camera on its side next to a screen
- **WHEN** a 320 × 180 screen and a 320 × 180 camera with `rotate: 90` are recorded
- **THEN** the sequence is 320 × 180, the camera clip's Motion has Rotation 90° and Scale 56.25 %, and the camera's sidecar says `rotate: 90` with the file still 320 × 180
