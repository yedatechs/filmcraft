//! Recording inside the app (Window ▸ Record): screen (a display or a window), camera and
//! microphone at once, **each source to its own file**, all stamped on one clock, then imported
//! and placed in sync in a new sequence (screen V1, camera V2, microphone A1) as one undo step.
//! Design: `openspec/changes/recording/design.md`; user docs: `docs/recording.md`.
//!
//! | command | does |
//! |---|---|
//! | `record.devices` | displays, windows, cameras, microphones and the permission states |
//! | `record.start` | start the chosen sources; the recording begins when every one is live (its files, the clock start) |
//! | `record.status` | elapsed time and per-source frames / dropped / bytes (and the mic level) |
//! | `record.stop` | finish the files, import them and build the synced sequence (or `discard`) |
//! | `record.cancel` | stop and delete the files |
//!
//! Video comes from a [`VideoInput`] made by a [`VideoInputFactory`]: the platform crate registers
//! the macOS one (ScreenCaptureKit, AVFoundation) with [`register_video_factory`]; a session can
//! override it ([`Recorder::factory`]); with neither, the deterministic [`SyntheticFactory`] is used
//! (headless sessions and tests). The microphone is the voice-over [`AudioInput`], moved onto the
//! recording's microphone thread while recording and handed back at stop.
//!
//! Threads: a capture callback only copies the frame into a bounded queue that drops the oldest
//! frame when full (counted as dropped), so capture never blocks. One encoder thread per video
//! source encodes through `filmcraft_export::recorder::MovRecorder` (hardware H.264 when the
//! platform registered it, else FilmCraft's own encoder); the microphone thread streams a WAV.
//! Every thread runs under `catch_unwind` and reports a failure as the source's error.

use std::collections::VecDeque;
use std::io::{Seek, SeekFrom, Write};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock};

use serde::Serialize;
use serde_json::{Value, json};

use filmcraft_project::{ItemId, Label, Marker, MarkerId, MarkerKind, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};

use crate::commands::{CommandSpec, bad, bool_p, str_p};
use crate::record_settings::{AUTO_GAIN_TARGET_DB, AutoGain, CameraRotate, RecordingSettings, WavFormat, wav_header};
use crate::voiceover::{AudioInput, SyntheticInput};
use crate::{EngineError, Result, Session};
use filmcraft_export::recorder::{CaptureCodec, CaptureEncoding, MovRecorder};

// ------------------------------------------------------------------------------------- clock

/// How long `record.start` waits, overall, for every source to start and deliver its first
/// frame or audio block. The platform uses it for stream / session starts too (ScreenCaptureKit's
/// first `startCapture` in a process warms up its helper and can take several seconds).
pub const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The one clock of a recording: a monotonic instant plus the wall-clock time at that instant
/// (`clock_start_ns` in the sidecars). Sample times are nanoseconds since it. Sources are started
/// on a clock taken at `record.start`; the recording's own clock is that one shifted to the
/// instant every source was live ([`Self::shifted`]).
#[derive(Clone, Copy, Debug)]
pub struct RecordClock {
    start: web_time::Instant,
    unix_ns: u64,
}

impl RecordClock {
    pub fn new() -> Self {
        let unix_ns = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)).unwrap_or(0);
        Self { start: web_time::Instant::now(), unix_ns }
    }
    /// Nanoseconds since the clock started.
    pub fn now_ns(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
    /// Nanoseconds from the clock start to `t` (0 for an instant before it).
    pub fn ns_at(&self, t: web_time::Instant) -> u64 {
        u64::try_from(t.saturating_duration_since(self.start).as_nanos()).unwrap_or(u64::MAX)
    }
    /// Wall-clock nanoseconds since the UNIX epoch at the clock start.
    pub fn unix_start_ns(&self) -> u64 {
        self.unix_ns
    }
    /// The same clock started `ns` later.
    pub fn shifted(&self, ns: u64) -> Self {
        let start = self.start.checked_add(std::time::Duration::from_nanos(ns)).unwrap_or(self.start);
        Self { start, unix_ns: self.unix_ns.saturating_add(ns) }
    }

    /// Nanoseconds from `earlier`'s start to this clock's start (0 when this one started first): a
    /// frame stamped `t` on `earlier` is at `t - ns_after(earlier)` on this clock (a camera
    /// preview's frames handed to a recording).
    pub fn ns_after(&self, earlier: &RecordClock) -> u64 {
        u64::try_from(self.start.saturating_duration_since(earlier.start).as_nanos()).unwrap_or(u64::MAX)
    }
}

impl Default for RecordClock {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------------------------- traits

/// Byte order of a captured frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PixelFormat {
    Bgra8,
    Rgba8,
}

/// One picture from a [`VideoInput`].
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row (≥ `width × 4`).
    pub stride: usize,
    pub format: PixelFormat,
    pub data: Vec<u8>,
    /// When it was captured, nanoseconds on the recording clock.
    pub time_ns: u64,
}

/// What the recording asks of a video input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoRequest {
    /// Wanted size (None = the source's own size).
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: u32,
    /// Scale the source down to at most this many rows, keeping its aspect (Settings ▸ Recording ▸
    /// Resolution; None = its own size).
    pub max_height: Option<u32>,
    /// Draw the mouse pointer (screens).
    pub show_cursor: bool,
    /// Record only this part of a display: `[x, y, w, h]` in display pixels (`record.start
    /// {screen: {display, area}}`; None = all of it).
    pub area: Option<[u32; 4]>,
}

impl VideoRequest {
    /// The source's own size at `fps`, the cursor shown.
    pub fn at(fps: u32) -> Self {
        Self { width: None, height: None, fps, max_height: None, show_cursor: true, area: None }
    }
}

/// Where a display or window is on the desktop: points, global coordinates (origin at the top
/// left of the main display), and its size in pixels (the backing scale is `pixels.0 / w`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ScreenFrame {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub pixels: (u32, u32),
}

/// A block of sound from a [`VideoInput`] that also captures audio (system audio of a screen).
pub struct CapturedAudio {
    pub sample_rate: u32,
    /// Planar samples, one `Vec` per channel (all the same length).
    pub channels: Vec<Vec<f32>>,
    /// When its first sample was captured, nanoseconds on the recording clock.
    pub time_ns: u64,
}

/// Receives captured audio blocks (called on the input's own thread).
pub type AudioSink = Arc<dyn Fn(CapturedAudio) + Send + Sync>;

/// What a started video input delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub format: PixelFormat,
}

/// Receives captured frames (called on the input's own thread).
pub type FrameSink = Arc<dyn Fn(CapturedFrame) + Send + Sync>;

/// Why a capture could not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureErrorKind {
    /// No capture on this system.
    Unavailable,
    /// A privacy permission is denied or not determined yet.
    Permission,
    /// The device id names nothing.
    NoDevice,
    /// The OS refused or failed.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureError {
    pub kind: CaptureErrorKind,
    pub message: String,
}

impl CaptureError {
    pub fn new(kind: CaptureErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
    pub fn unavailable() -> Self {
        Self::new(CaptureErrorKind::Unavailable, "screen and camera recording are not available on this system yet")
    }
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// A screen or camera being captured.
pub trait VideoInput: Send {
    /// Start delivering frames to `sink`, stamped on `clock`; returns the negotiated format.
    fn start(&mut self, req: &VideoRequest, clock: RecordClock, sink: FrameSink) -> std::result::Result<VideoFormat, CaptureError>;
    /// Stop delivering frames (no frame reaches the sink after this returns).
    fn stop(&mut self);
    /// A failure after start (device unplugged, the system stopped the stream).
    fn error(&self) -> Option<String> {
        None
    }
    /// The orientation the camera reports now: degrees (0 / 90 / 180 / 270) its pictures must be
    /// turned clockwise to stand upright (None: this input reports none; screens). Read when a
    /// recording starts (Rotate Auto), while it runs (a change is noted, the file keeps the start
    /// value) and by the preview; it must be cheap and never block.
    fn rotation(&self) -> Option<u16> {
        None
    }
    /// Also deliver the sound the system plays (a screen input; called before [`Self::start`]),
    /// preferably at `sample_rate` with `channels`. False: this input cannot.
    fn capture_audio(&mut self, _sample_rate: u32, _channels: u16, _sink: AudioSink) -> bool {
        false
    }
    /// A display being recorded: leave out FilmCraft's recording border and camera preview
    /// windows that appeared after the start (the stream's filter is updated, never restarted).
    fn refresh_exclusions(&mut self) -> std::result::Result<(), CaptureError> {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub app: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CameraFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CameraInfo {
    pub id: String,
    pub name: String,
    pub formats: Vec<CameraFormat>,
}

/// What a factory can open.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct VideoDevices {
    pub displays: Vec<DisplayInfo>,
    pub windows: Vec<WindowInfo>,
    pub cameras: Vec<CameraInfo>,
    /// Why a kind of device could not be listed (a missing permission): `record.devices` `error`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

/// A privacy permission's state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    #[default]
    Granted,
    Denied,
    Undetermined,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Permissions {
    pub screen: Permission,
    pub camera: Permission,
    pub microphone: Permission,
}

/// What the screen source records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenTarget {
    Display(String),
    Window(String),
}

/// Enumerates and opens screens and cameras (the platform crate's, or [`SyntheticFactory`]).
pub trait VideoInputFactory: Send + Sync {
    fn name(&self) -> String;
    fn devices(&self) -> std::result::Result<VideoDevices, CaptureError>;
    fn permissions(&self) -> Permissions;
    fn open_screen(&self, target: &ScreenTarget) -> std::result::Result<Box<dyn VideoInput>, CaptureError>;
    fn open_camera(&self, device: &str) -> std::result::Result<Box<dyn VideoInput>, CaptureError>;
    /// Whether screen inputs can capture system audio ([`VideoInput::capture_audio`]).
    fn system_audio(&self) -> bool {
        false
    }
    /// Where a display or window is now (the recording border follows it); None when unknown.
    fn screen_frame(&self, _target: &ScreenTarget) -> Option<ScreenFrame> {
        None
    }
}

fn registry() -> &'static RwLock<Option<Arc<dyn VideoInputFactory>>> {
    static F: RwLock<Option<Arc<dyn VideoInputFactory>>> = RwLock::new(None);
    &F
}

/// Install the system's capture factory (the platform crate does at startup).
pub fn register_video_factory(f: Arc<dyn VideoInputFactory>) {
    *registry().write().unwrap_or_else(PoisonError::into_inner) = Some(f);
}

/// The registered capture factory, if any.
pub fn registered_video_factory() -> Option<Arc<dyn VideoInputFactory>> {
    registry().read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// A factory with nothing to open: systems without screen / camera capture (the desktop app
/// installs it when the platform crate registered none).
pub struct UnavailableFactory;

impl VideoInputFactory for UnavailableFactory {
    fn name(&self) -> String {
        "none".into()
    }
    fn devices(&self) -> std::result::Result<VideoDevices, CaptureError> {
        Err(CaptureError::unavailable())
    }
    fn permissions(&self) -> Permissions {
        Permissions { screen: Permission::Unavailable, camera: Permission::Unavailable, microphone: Permission::Granted }
    }
    fn open_screen(&self, _: &ScreenTarget) -> std::result::Result<Box<dyn VideoInput>, CaptureError> {
        Err(CaptureError::unavailable())
    }
    fn open_camera(&self, _: &str) -> std::result::Result<Box<dyn VideoInput>, CaptureError> {
        Err(CaptureError::unavailable())
    }
}

// ------------------------------------------------------------------------------------- synthetic

/// Deterministic screen and camera sources for headless sessions and tests: one display, one
/// window, two cameras. `*_delay_ms` makes a source start late (clock skew in tests).
#[derive(Clone, Debug)]
pub struct SyntheticFactory {
    pub display_size: (u32, u32),
    pub camera_size: (u32, u32),
    pub screen_delay_ms: u64,
    pub camera_delay_ms: u64,
    /// The second camera ([`Self::CAMERA2`]) starts this late.
    pub camera2_delay_ms: u64,
    /// How many times a camera was opened (tests: a preview and a recording share one).
    pub camera_opens: Arc<AtomicU64>,
    /// The orientation the synthetic cameras report (degrees clockwise; tests change it while
    /// they run, as a camera's own software would).
    pub camera_rotation: Arc<AtomicU32>,
}

impl Default for SyntheticFactory {
    fn default() -> Self {
        Self {
            display_size: (1280, 720),
            camera_size: (640, 360),
            screen_delay_ms: 0,
            camera_delay_ms: 0,
            camera2_delay_ms: 0,
            camera_opens: Arc::default(),
            camera_rotation: Arc::default(),
        }
    }
}

impl SyntheticFactory {
    pub const DISPLAY: &'static str = "synthetic:display";
    pub const WINDOW: &'static str = "synthetic:window";
    pub const CAMERA: &'static str = "synthetic:camera";
    pub const CAMERA2: &'static str = "synthetic:camera2";
}

impl VideoInputFactory for SyntheticFactory {
    fn name(&self) -> String {
        "Synthetic".into()
    }
    fn devices(&self) -> std::result::Result<VideoDevices, CaptureError> {
        let (cw, ch) = self.camera_size;
        Ok(VideoDevices {
            displays: vec![DisplayInfo { id: Self::DISPLAY.into(), name: "Synthetic Display".into(), width: self.display_size.0, height: self.display_size.1 }],
            windows: vec![WindowInfo { id: Self::WINDOW.into(), title: "Synthetic Window".into(), app: "FilmCraft".into() }],
            cameras: vec![
                CameraInfo { id: Self::CAMERA.into(), name: "Synthetic Camera".into(), formats: vec![CameraFormat { width: cw, height: ch, fps: 30 }] },
                CameraInfo { id: Self::CAMERA2.into(), name: "Synthetic Camera 2".into(), formats: vec![CameraFormat { width: cw, height: ch, fps: 30 }] },
            ],
            problems: Vec::new(),
        })
    }
    fn permissions(&self) -> Permissions {
        Permissions::default()
    }
    fn system_audio(&self) -> bool {
        true
    }
    fn screen_frame(&self, target: &ScreenTarget) -> Option<ScreenFrame> {
        let (w, h) = self.display_size;
        match target {
            ScreenTarget::Display(id) if id == Self::DISPLAY => Some(ScreenFrame { x: 0.0, y: 0.0, w: f64::from(w), h: f64::from(h), pixels: (w, h) }),
            ScreenTarget::Window(id) if id == Self::WINDOW => Some(ScreenFrame { x: 100.0, y: 80.0, w: 960.0, h: 540.0, pixels: (960, 540) }),
            _ => None,
        }
    }
    fn open_screen(&self, target: &ScreenTarget) -> std::result::Result<Box<dyn VideoInput>, CaptureError> {
        match target {
            ScreenTarget::Display(id) if id == Self::DISPLAY => Ok(Box::new(SyntheticVideoInput::new(self.display_size, self.screen_delay_ms))),
            ScreenTarget::Window(id) if id == Self::WINDOW => Ok(Box::new(SyntheticVideoInput::new((960, 540), self.screen_delay_ms))),
            ScreenTarget::Display(id) => Err(CaptureError::new(CaptureErrorKind::NoDevice, format!("no display `{id}`"))),
            ScreenTarget::Window(id) => Err(CaptureError::new(CaptureErrorKind::NoDevice, format!("no window `{id}`"))),
        }
    }
    fn open_camera(&self, device: &str) -> std::result::Result<Box<dyn VideoInput>, CaptureError> {
        if device == Self::CAMERA || device == Self::CAMERA2 {
            self.camera_opens.fetch_add(1, Ordering::Relaxed);
        }
        let camera = |delay| {
            let mut i = SyntheticVideoInput::new(self.camera_size, delay);
            i.rotation = Some(self.camera_rotation.clone());
            Box::new(i)
        };
        if device == Self::CAMERA {
            Ok(camera(self.camera_delay_ms))
        } else if device == Self::CAMERA2 {
            Ok(camera(self.camera2_delay_ms))
        } else {
            Err(CaptureError::new(CaptureErrorKind::NoDevice, format!("no camera `{device}`")))
        }
    }
}

/// Blocks of the burnt-in frame index (top row of a synthetic frame, most significant bit first).
pub const INDEX_BITS: u32 = 16;

/// A generated picture source: mid grey, a white bar moving with time, and the frame index burnt
/// into the top eighth as [`INDEX_BITS`] black / white blocks. With [`VideoInput::capture_audio`]
/// it also plays a 0.25 sine at [`SYNTHETIC_TONE_HZ`] as "system audio".
pub struct SyntheticVideoInput {
    native: (u32, u32),
    delay_ms: u64,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    error: Arc<Mutex<Option<String>>>,
    audio: Option<(u32, u16, AudioSink)>,
    /// A camera's reported orientation (None: a screen, which reports none).
    rotation: Option<Arc<AtomicU32>>,
}

/// The tone of the synthetic system audio.
pub const SYNTHETIC_TONE_HZ: f64 = 440.0;

impl SyntheticVideoInput {
    pub fn new(native: (u32, u32), delay_ms: u64) -> Self {
        Self { native, delay_ms, stop: Arc::new(AtomicBool::new(false)), thread: None, error: Arc::new(Mutex::new(None)), audio: None, rotation: None }
    }
}

/// The synthetic picture of frame `index` at `time_ns` (RGBA, `w × h`).
pub fn synthetic_frame(w: u32, h: u32, index: u64, time_ns: u64) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    let mut px = vec![128u8; wu * hu * 4];
    let bar_w = (wu / 32).max(2);
    let span = wu.saturating_sub(bar_w).max(1);
    let bar_x = ((time_ns / 10_000_000) as usize) % span;
    let block_w = (wu / INDEX_BITS as usize).max(1);
    let block_h = (hu / 8).max(1);
    for y in 0..hu {
        for x in 0..wu {
            let i = (y * wu + x) * 4;
            let v = if y < block_h {
                let bit = (x / block_w).min(INDEX_BITS as usize - 1);
                if (index >> (INDEX_BITS as usize - 1 - bit)) & 1 == 1 { 235 } else { 16 }
            } else if (bar_x..bar_x + bar_w).contains(&x) {
                235
            } else {
                128
            };
            if let Some(p) = px.get_mut(i..i + 4) {
                p.copy_from_slice(&[v, v, v, 255]);
            }
        }
    }
    px
}

/// Read back the frame index burnt in by [`synthetic_frame`] from a luma plane (tests).
pub fn read_synthetic_index(luma: &[u8], w: u32, h: u32) -> Option<u64> {
    let (wu, hu) = (w as usize, h as usize);
    let block_w = (wu / INDEX_BITS as usize).max(1);
    let y = (hu / 8).max(1) / 2;
    let mut v = 0u64;
    for bit in 0..INDEX_BITS as usize {
        let x = bit * block_w + block_w / 2;
        let l = *luma.get(y * wu + x)?;
        v = (v << 1) | u64::from(l > 128);
    }
    Some(v)
}

impl VideoInput for SyntheticVideoInput {
    fn start(&mut self, req: &VideoRequest, clock: RecordClock, sink: FrameSink) -> std::result::Result<VideoFormat, CaptureError> {
        if self.thread.is_some() {
            return Err(CaptureError::new(CaptureErrorKind::Failed, "already started"));
        }
        // a stopped input can start again (a camera preview restarted for a recording)
        self.stop.store(false, Ordering::Release);
        let native = match req.area {
            // an area of the synthetic display is a picture of the area's size
            Some([x, y, aw, ah]) => {
                if u64::from(x) + u64::from(aw) > u64::from(self.native.0) || u64::from(y) + u64::from(ah) > u64::from(self.native.1) {
                    return Err(CaptureError::new(
                        CaptureErrorKind::Failed,
                        format!("the area {x},{y} {aw}×{ah} is outside the {}×{} display", self.native.0, self.native.1),
                    ));
                }
                (aw, ah)
            }
            None => self.native,
        };
        let (w, h) = (req.width.unwrap_or(native.0), req.height.unwrap_or(native.1));
        let (w, h) = req.max_height.and_then(|mh| crate::record_settings::downscale((w, h), mh)).unwrap_or((w, h));
        let (w, h) = (w.clamp(16, 8192) & !1, h.clamp(16, 8192) & !1);
        let fps = req.fps.clamp(1, 60);
        let audio = self.audio.clone();
        let first = clock.now_ns().saturating_add(self.delay_ms.saturating_mul(1_000_000));
        let period = 1_000_000_000 / u64::from(fps);
        let stop = self.stop.clone();
        let error = self.error.clone();
        let thread = std::thread::Builder::new()
            .name("filmcraft-synthetic-video".into())
            .spawn(move || {
                let run = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let mut k = 0u64;
                    let mut sample = 0u64;
                    while !stop.load(Ordering::Acquire) {
                        let t = first.saturating_add(k.saturating_mul(period));
                        let now = clock.now_ns();
                        if now < t {
                            std::thread::sleep(std::time::Duration::from_nanos((t - now).min(20_000_000)));
                            continue;
                        }
                        let data = synthetic_frame(w, h, k, t);
                        sink(CapturedFrame { width: w, height: h, stride: w as usize * 4, format: PixelFormat::Rgba8, data, time_ns: t });
                        if let Some((rate, ch, asink)) = &audio {
                            // the sound of this frame period
                            let rate = u64::from((*rate).max(1));
                            let end = (u128::from(k + 1) * u128::from(period) * u128::from(rate) / 1_000_000_000) as u64;
                            let n = end.saturating_sub(sample).min(rate) as usize;
                            let w = 2.0 * std::f64::consts::PI * SYNTHETIC_TONE_HZ / rate as f64;
                            let tone: Vec<f32> = (0..n).map(|i| (0.25 * (w * (sample + i as u64) as f64).sin()) as f32).collect();
                            let time_ns = first.saturating_add((u128::from(sample) * 1_000_000_000 / u128::from(rate)) as u64);
                            asink(CapturedAudio { sample_rate: rate as u32, channels: vec![tone; usize::from((*ch).max(1))], time_ns });
                            sample += n as u64;
                        }
                        k += 1;
                    }
                }));
                if let Err(p) = run {
                    *error.lock().unwrap_or_else(PoisonError::into_inner) = Some(format!("synthetic input crashed: {}", panic_text(&p)));
                }
            })
            .map_err(|e| CaptureError::new(CaptureErrorKind::Failed, format!("cannot start the capture thread: {e}")))?;
        self.thread = Some(thread);
        Ok(VideoFormat { width: w, height: h, fps, format: PixelFormat::Rgba8 })
    }
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
    fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    fn rotation(&self) -> Option<u16> {
        self.rotation.as_ref().map(|r| u16::try_from(r.load(Ordering::Acquire)).unwrap_or(0))
    }
    fn capture_audio(&mut self, sample_rate: u32, channels: u16, sink: AudioSink) -> bool {
        self.audio = Some((sample_rate.clamp(8_000, 192_000), channels.clamp(1, 2), sink));
        true
    }
}

impl Drop for SyntheticVideoInput {
    fn drop(&mut self) {
        self.stop();
    }
}

fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into())
}

// ------------------------------------------------------------------------------------- queue

/// Frames waiting for the encoder: at most `cap`, the oldest dropped when full.
pub type FrameQueue = BoundedQueue<CapturedFrame>;

/// Items waiting for a worker thread: at most `cap`, the oldest dropped when full.
pub struct BoundedQueue<T> {
    q: Mutex<(VecDeque<T>, bool)>,
    cv: Condvar,
    cap: usize,
}

impl<T> BoundedQueue<T> {
    pub fn new(cap: usize) -> Self {
        Self { q: Mutex::new((VecDeque::new(), false)), cv: Condvar::new(), cap: cap.max(1) }
    }
    /// Add a frame; returns whether an older one was dropped to make room. Never blocks on the
    /// consumer. Frames pushed after [`Self::close`] are dropped.
    pub fn push(&self, f: T) -> bool {
        let mut g = self.q.lock().unwrap_or_else(PoisonError::into_inner);
        if g.1 {
            return true;
        }
        let dropped = g.0.len() >= self.cap;
        if dropped {
            g.0.pop_front();
        }
        g.0.push_back(f);
        drop(g);
        self.cv.notify_one();
        dropped
    }
    /// The next frame; `None` once closed and empty. Waits at most `timeout`
    /// (`Some(None)` = nothing yet).
    pub fn pop(&self, timeout: std::time::Duration) -> Option<Option<T>> {
        let mut g = self.q.lock().unwrap_or_else(PoisonError::into_inner);
        if g.0.is_empty() && !g.1 {
            g = self.cv.wait_timeout(g, timeout).unwrap_or_else(PoisonError::into_inner).0;
        }
        match g.0.pop_front() {
            Some(f) => Some(Some(f)),
            None if g.1 => None,
            None => Some(None),
        }
    }
    pub fn close(&self) {
        self.q.lock().unwrap_or_else(PoisonError::into_inner).1 = true;
        self.cv.notify_all();
    }
    pub fn is_closed(&self) -> bool {
        self.q.lock().unwrap_or_else(PoisonError::into_inner).1
    }
    pub fn len(&self) -> usize {
        self.q.lock().unwrap_or_else(PoisonError::into_inner).0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ------------------------------------------------------------------------------------- sources

/// Which kind of source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Screen,
    Camera,
    Mic,
    /// The sound the system plays, captured with the screen.
    #[serde(rename = "systemAudio")]
    SystemAudio,
}

impl SourceKind {
    pub fn name(self) -> &'static str {
        match self {
            SourceKind::Screen => "screen",
            SourceKind::Camera => "camera",
            SourceKind::Mic => "mic",
            SourceKind::SystemAudio => "systemAudio",
        }
    }
    fn file_label(self) -> &'static str {
        match self {
            SourceKind::Screen => "Screen",
            SourceKind::Camera => "Camera",
            SourceKind::Mic => "Mic",
            SourceKind::SystemAudio => "System Audio",
        }
    }
    fn audio(self) -> bool {
        matches!(self, SourceKind::Mic | SourceKind::SystemAudio)
    }
    fn extension(self) -> &'static str {
        if self.audio() { "wav" } else { "mov" }
    }
}

/// Most cameras / microphones one recording takes.
pub const MAX_PER_KIND: usize = 4;

/// One source of a recording: its kind and its number among the sources of that kind (0-based;
/// the second camera is `Src { kind: Camera, n: 1 }`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Src {
    pub kind: SourceKind,
    pub n: usize,
}

impl Src {
    pub fn new(kind: SourceKind, n: usize) -> Self {
        Self { kind, n }
    }
    /// The key in `record.stop` results: `screen`, `camera`, `camera2`, `mic`, `mic2`…
    pub fn key(self) -> String {
        if self.n == 0 { self.kind.name().to_string() } else { format!("{}{}", self.kind.name(), self.n + 1) }
    }
    /// The file label: `Screen`, `Camera`, `Camera 2`, `Mic 2`…
    fn label(self) -> String {
        if self.n == 0 { self.kind.file_label().to_string() } else { format!("{} {}", self.kind.file_label(), self.n + 1) }
    }
}

const NONE: u64 = u64::MAX;

/// Live counters of one source (shared with its threads).
#[derive(Debug)]
pub struct SourceStats {
    pub frames: AtomicU64,
    pub dropped: AtomicU64,
    pub bytes: AtomicU64,
    /// First / last sample time written, on the recording clock (`u64::MAX` = none yet).
    pub first_ns: AtomicU64,
    pub last_ns: AtomicU64,
    /// When the source was asked to start and when it delivered its first sample, on the start
    /// clock (`u64::MAX` = not yet): the difference is its warm-up (`warmup_ns`).
    pub started_ns: AtomicU64,
    pub raw_first_ns: AtomicU64,
    /// Microphone peak of the last block (f32 bits).
    pub level: AtomicU32,
}

impl Default for SourceStats {
    fn default() -> Self {
        Self {
            frames: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            first_ns: AtomicU64::new(NONE),
            last_ns: AtomicU64::new(NONE),
            started_ns: AtomicU64::new(NONE),
            raw_first_ns: AtomicU64::new(NONE),
            level: AtomicU32::new(0),
        }
    }
}

impl SourceStats {
    fn first(&self) -> Option<u64> {
        Some(self.first_ns.load(Ordering::Acquire)).filter(|v| *v != NONE)
    }
    fn last(&self) -> Option<u64> {
        Some(self.last_ns.load(Ordering::Acquire)).filter(|v| *v != NONE)
    }
    /// The first sample's time on the start clock (None: nothing delivered yet).
    fn raw_first(&self) -> Option<u64> {
        Some(self.raw_first_ns.load(Ordering::Acquire)).filter(|v| *v != NONE)
    }
    /// Note the first delivered sample (later calls keep the first).
    fn delivered(&self, raw_ns: u64) {
        let _ = self.raw_first_ns.compare_exchange(NONE, raw_ns, Ordering::AcqRel, Ordering::Acquire);
    }
    /// How long the source took from its start to its first sample.
    fn warmup(&self) -> Option<u64> {
        let started = self.started_ns.load(Ordering::Acquire);
        self.raw_first().filter(|_| started != NONE).map(|f| f.saturating_sub(started))
    }
}

/// Wait until the recording's start (`origin`, on the start clock) is known; None when `gone()`
/// first (the start failed or was cancelled).
fn wait_origin(origin: &AtomicU64, gone: impl Fn() -> bool) -> Option<u64> {
    loop {
        let o = origin.load(Ordering::Acquire);
        if o != NONE {
            return Some(o);
        }
        if gone() {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// What a finished source produced.
#[derive(Clone, Debug, Default)]
struct Finished {
    frames: u64,
    bytes: u64,
    encoder: String,
    width: u32,
    height: u32,
    fps: u32,
    sample_rate: u32,
    samples: u64,
    channels: u16,
    /// WAV sample format id (`s16` / `s24` / `f32`).
    format: &'static str,
    auto_gain: bool,
    /// Target bitrate (kbps; 0 for ProRes / audio).
    kbps: u32,
    /// What the sidecar should say about choices that could not be followed.
    notes: Vec<String>,
}

type Worker = std::thread::JoinHandle<std::result::Result<Finished, String>>;

struct VideoSource {
    src: Src,
    /// What could not be followed for this source (sidecar `notes`).
    notes: Vec<String>,
    device_id: String,
    device_name: String,
    /// Flip the clip horizontally in the sequence (a camera seen as in a mirror).
    mirror: bool,
    /// The file's display rotation (degrees clockwise, cameras): the Rotate setting, or with Auto
    /// the camera's orientation at the start.
    rotate: u32,
    /// The camera's Rotate (Auto or fixed).
    rotate_setting: CameraRotate,
    /// The orientation the camera reported at the start (None: it reports none).
    camera_rotation: Option<u16>,
    /// With Auto: the camera's orientation changed during the recording (the file keeps the
    /// start's): the last value seen, how many changes, and when the first was seen (ns after the
    /// recording's start).
    rotation_seen: Option<u16>,
    rotation_changes: u32,
    rotation_changed_at_ns: Option<u64>,
    /// The part of the display recorded (screens; sidecar `area`).
    area: Option<[u32; 4]>,
    input: Box<dyn VideoInput>,
    queue: Arc<FrameQueue>,
    stats: Arc<SourceStats>,
    worker: Option<Worker>,
    path: PathBuf,
}

type MicWorker = std::thread::JoinHandle<(Box<dyn AudioInput>, std::result::Result<Finished, String>)>;

struct MicSource {
    src: Src,
    device: String,
    /// The session's own input (the voice-over one): handed back at stop. Other microphones
    /// use inputs made for the recording ([`AudioInput::spawn`]) and are dropped.
    session_input: bool,
    stats: Arc<SourceStats>,
    worker: Option<MicWorker>,
    path: PathBuf,
}

/// The system audio of the screen being recorded.
struct AudioSource {
    src: Src,
    queue: Arc<BoundedQueue<CapturedAudio>>,
    stats: Arc<SourceStats>,
    worker: Option<Worker>,
    path: PathBuf,
}

/// A recording in progress.
pub struct Active {
    pub name: String,
    pub dir: PathBuf,
    /// The recording's clock: starts when every source is live (media time 0 of every file).
    pub clock: RecordClock,
    /// The clock the sources were started on (their sample times), and the recording's start
    /// on it (`origin`, `u64::MAX` until every source is live).
    raw: RecordClock,
    origin: Arc<AtomicU64>,
    video: Vec<VideoSource>,
    mics: Vec<MicSource>,
    system_audio: Option<AudioSource>,
    /// Set at stop: the time up to which every source records.
    stop_ns: Arc<AtomicU64>,
    microphones: Vec<String>,
    /// The settings it was started with (for the sidecars).
    settings: RecordingSettings,
    /// Choices that could not be followed (HEVC without the hardware encoder…).
    notes: Vec<String>,
    /// [`now_ms`] at the start, and the Stop after limit (ms).
    started_ms: u64,
    stop_after_ms: Option<u64>,
}

impl Active {
    /// With Rotate Auto: notice a camera whose orientation changed since the start (the file's
    /// single track header keeps the start's; the sidecar and Stop say so).
    fn watch_rotation(&mut self) {
        let origin = self.origin.load(Ordering::Acquire);
        let now = self.raw.now_ns();
        for v in self.video.iter_mut().filter(|v| v.src.kind == SourceKind::Camera && v.rotate_setting == CameraRotate::Auto) {
            let r = v.input.rotation();
            if r.is_none() || r == v.rotation_seen {
                continue;
            }
            v.rotation_seen = r;
            v.rotation_changes = v.rotation_changes.saturating_add(1);
            if v.rotation_changed_at_ns.is_none() {
                v.rotation_changed_at_ns = Some(if origin == NONE { 0 } else { now.saturating_sub(origin) });
            }
        }
    }
    /// The inputs of the screen sources (to refresh what they leave out).
    pub(crate) fn screen_inputs(&mut self) -> impl Iterator<Item = &mut Box<dyn VideoInput>> {
        self.video.iter_mut().filter(|v| v.src.kind == SourceKind::Screen).map(|v| &mut v.input)
    }
    fn files(&self) -> Vec<(Src, PathBuf)> {
        let mut v: Vec<(Src, PathBuf)> = self.video.iter().map(|s| (s.src, s.path.clone())).collect();
        v.extend(self.mics.iter().map(|m| (m.src, m.path.clone())));
        v.extend(self.system_audio.iter().map(|a| (a.src, a.path.clone())));
        v
    }
}

/// A `record.start` waiting for its countdown.
#[derive(Clone, Debug)]
pub struct Countdown {
    /// The start parameters (without `countdown`).
    pub params: Value,
    /// When it starts ([`now_ms`]).
    pub due_ms: u64,
    pub seconds: u32,
}

/// Recording state of a session.
#[derive(Default)]
pub struct Recorder {
    /// Overrides the registered capture factory (tests, the desktop app on systems without one).
    pub factory: Option<Arc<dyn VideoInputFactory>>,
    /// Cameras shown live (`record.preview`); a recording of one of them shares its capture.
    pub previews: Vec<Arc<crate::record_preview::CameraTap>>,
    /// Every camera capture alive (previewed or recorded), so a preview and a recording of the
    /// same camera share one device session.
    pub(crate) taps: Vec<std::sync::Weak<crate::record_preview::CameraTap>>,
    pub active: Option<Active>,
    /// A start counting down (`record.start {countdown}`); [`tick`] starts it when due.
    pub countdown: Option<Countdown>,
    /// A start whose sources are starting (`record.status` `starting`); [`tick`] collects it.
    pub starting: Option<Starting>,
    /// Replaces [`START_TIMEOUT`] (tests).
    pub start_timeout: Option<std::time::Duration>,
    /// Replaces the wall clock of the countdown and Stop after, in milliseconds (tests).
    pub test_clock_ms: Option<Arc<AtomicU64>>,
    /// What [`tick`] did last (a delayed start, its failure, a Stop after), for the hosts.
    pub last_event: Option<Value>,
}

impl Recorder {
    pub fn recording(&self) -> bool {
        self.active.is_some()
    }
    /// "Starting screen capture…" while the sources of a start are starting (None otherwise).
    pub fn starting_label(&self) -> Option<&str> {
        self.starting.as_ref().map(|s| s.label.as_str())
    }
    /// Seconds left before a counting-down start (None: no countdown).
    pub fn countdown_left(&self) -> Option<f64> {
        let c = self.countdown.as_ref()?;
        Some(c.due_ms.saturating_sub(self.now_ms()) as f64 / 1000.0)
    }
    /// Milliseconds on the countdown clock ([`Self::test_clock_ms`], else since the process started).
    pub fn now_ms(&self) -> u64 {
        if let Some(t) = &self.test_clock_ms {
            return t.load(Ordering::Acquire);
        }
        static EPOCH: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();
        u64::try_from(EPOCH.get_or_init(web_time::Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

pub(crate) fn factory(s: &Session) -> Arc<dyn VideoInputFactory> {
    s.record.factory.clone().or_else(registered_video_factory).unwrap_or_else(|| Arc::new(SyntheticFactory::default()))
}

// ------------------------------------------------------------------------------------- workers

/// Copy a captured frame into a straight RGBA picture of `w × h`: as is when the sizes match,
/// else scaled (nearest pixel; a source that delivers more than was negotiated, e.g. a screen
/// whose capture cannot be scaled by the system).
pub(crate) fn to_rgba(f: &CapturedFrame, w: u32, h: u32, out: &mut Vec<u8>) -> bool {
    let (fw, fh) = (f.width as usize, f.height as usize);
    let need = f.stride.checked_mul(fh.saturating_sub(1)).and_then(|n| n.checked_add(fw.saturating_mul(4)));
    if fw == 0 || fh == 0 || f.stride < fw.saturating_mul(4) || need.is_none_or(|n| f.data.len() < n) {
        return false;
    }
    let (w, h) = (w as usize, h as usize);
    if (fw, fh) != (w, h) {
        out.clear();
        out.resize(w * h * 4, 255);
        let bgra = f.format == PixelFormat::Bgra8;
        for y in 0..h {
            let sy = (y * fh / h.max(1)).min(fh - 1);
            for x in 0..w {
                let sx = (x * fw / w.max(1)).min(fw - 1);
                let i = sy * f.stride + sx * 4;
                let (Some(p), Some(d)) = (f.data.get(i..i + 4), out.get_mut((y * w + x) * 4..(y * w + x) * 4 + 4)) else { return false };
                d.copy_from_slice(&if bgra { [p[2], p[1], p[0], 255] } else { [p[0], p[1], p[2], 255] });
            }
        }
        return true;
    }
    out.clear();
    out.resize(w * h * 4, 0);
    for p in out.as_chunks_mut::<4>().0 {
        p[3] = 255;
    }
    let (cw, ch) = (w.min(fw), h.min(fh));
    for y in 0..ch {
        let (Some(src), Some(dst)) = (f.data.get(y * f.stride..y * f.stride + cw * 4), out.get_mut(y * w * 4..y * w * 4 + cw * 4)) else { return false };
        match f.format {
            PixelFormat::Rgba8 => {
                for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                    *d = [s[0], s[1], s[2], 255];
                }
            }
            PixelFormat::Bgra8 => {
                for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                    *d = [s[2], s[1], s[0], 255];
                }
            }
        }
    }
    true
}

/// The slot (frame number at `rate`) of a frame `ns` after the first.
fn slot_of(ns: u64, rate: FrameRate) -> i64 {
    let (num, den) = (rate.num.max(1) as i128, rate.den.max(1) as i128);
    ((ns as i128 * num + den * 500_000_000) / (den * 1_000_000_000)).clamp(0, i64::MAX as i128) as i64
}

/// Encodes a video source's frames. It starts when the recording's start (`origin`) is known:
/// frames captured before it are left out (not counted as dropped), the first frame kept is
/// frame 0 (it stands for the instant the recording began, less than a frame earlier) and every
/// later frame sits at its time since the start.
fn video_worker(
    mut rec: filmcraft_export::recorder::MovRecorder,
    queue: Arc<FrameQueue>,
    stats: Arc<SourceStats>,
    stop_ns: Arc<AtomicU64>,
    origin: Arc<AtomicU64>,
    rate: FrameRate,
) -> std::result::Result<Finished, String> {
    let Some(origin) = wait_origin(&origin, || queue.is_closed()) else {
        return Err("stopped before the recording began".into());
    };
    let (w, h) = rec.size();
    let mut rgba = Vec::new();
    loop {
        let f = match queue.pop(std::time::Duration::from_millis(100)) {
            None => break,
            Some(None) => continue,
            Some(Some(f)) => f,
        };
        let Some(rel) = f.time_ns.checked_sub(origin) else {
            continue; // captured while the other sources were starting
        };
        let slot = if rec.last_slot().is_none() { 0 } else { slot_of(rel, rate) };
        if rec.last_slot().is_some_and(|l| slot <= l) {
            continue; // two frames inside one frame period
        }
        if !to_rgba(&f, w, h, &mut rgba) {
            stats.dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        rec.push(&rgba, slot).map_err(|e| e.to_string())?;
        if stats.first().is_none() {
            stats.first_ns.store(rel, Ordering::Release);
        }
        stats.last_ns.store(rel, Ordering::Release);
        stats.frames.fetch_add(1, Ordering::Relaxed);
        stats.bytes.store(rec.bytes(), Ordering::Relaxed);
    }
    let encoder = rec.encoder_name().to_string();
    let kbps = rec.kbps();
    let frames = rec.frames();
    if frames == 0 {
        return Err("no frames were captured".into());
    }
    let end = stop_ns.load(Ordering::Acquire);
    let last_slot = rec.last_slot().unwrap_or(0);
    let end_slot = if end == NONE { last_slot + 1 } else { slot_of(end.saturating_sub(origin), rate).max(last_slot + 1) };
    let bytes = rec.finish(end_slot).map_err(|e| e.to_string())?;
    let fps = (rate.num.max(1) / rate.den.max(1)) as u32;
    Ok(Finished { frames, bytes, encoder, width: w, height: h, fps, kbps, ..Default::default() })
}

/// Audio options of a recording (Settings ▸ Recording ▸ Audio).
#[derive(Clone, Copy, Debug)]
struct AudioOpts {
    rate: u32,
    stereo: bool,
    format: WavFormat,
    auto_gain: bool,
}

/// A WAV written as it is recorded (the header is rewritten by [`WavStream::finish`]).
struct WavStream {
    w: std::io::BufWriter<std::fs::File>,
    /// Sample frames written.
    frames: u64,
    rate: u32,
    channels: u16,
    format: WavFormat,
}

/// Bytes of samples that fit a WAV's 32-bit data size.
const WAV_MAX_BYTES: u64 = u32::MAX as u64 - 64;

impl WavStream {
    fn create(path: &Path, rate: u32, channels: u16, format: WavFormat) -> std::io::Result<Self> {
        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
        w.write_all(&wav_header(rate.max(1), channels.max(1), format, 0))?;
        Ok(Self { w, frames: 0, rate: rate.max(1), channels: channels.clamp(1, 2), format })
    }
    fn frame_bytes(&self) -> u64 {
        u64::from(self.channels) * u64::from(self.format.bytes())
    }
    /// Append planar samples (the shortest channel's length; a missing channel repeats the first).
    fn write(&mut self, planar: &[Vec<f32>]) -> std::result::Result<(), String> {
        let Some(first) = planar.first() else { return Ok(()) };
        let n = planar.iter().take(usize::from(self.channels)).map(Vec::len).min().unwrap_or(0);
        if self.frames.saturating_add(n as u64).saturating_mul(self.frame_bytes()) > WAV_MAX_BYTES {
            return Err("the audio file reached the 4 GB WAV limit".into());
        }
        let mut b = Vec::with_capacity(n * self.frame_bytes() as usize);
        for i in 0..n {
            for c in 0..usize::from(self.channels) {
                let v = planar.get(c).unwrap_or(first).get(i).copied().unwrap_or(0.0);
                self.format.put(v, &mut b);
            }
        }
        self.w.write_all(&b).map_err(|e| e.to_string())?;
        self.frames += n as u64;
        Ok(())
    }
    fn bytes(&self) -> u64 {
        44 + self.frames * self.frame_bytes()
    }
    fn finish(self) -> std::io::Result<u64> {
        let data = u32::try_from(self.frames * self.frame_bytes()).unwrap_or(u32::MAX);
        let header = wav_header(self.rate, self.channels, self.format, data);
        let mut f = self.w.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header)?;
        f.sync_all()?;
        Ok(44 + u64::from(data))
    }
}

/// The channels of a microphone block to write: mono = the Voice-Over input channel; stereo =
/// the first two (a mono device twice).
fn pick_channels(got: &[Vec<f32>], channel: usize, stereo: bool) -> Vec<Vec<f32>> {
    let first = got.get(channel).or(got.first()).cloned().unwrap_or_default();
    if stereo {
        let l = got.first().cloned().unwrap_or_default();
        let r = got.get(1).cloned().unwrap_or_else(|| l.clone());
        vec![l, r]
    } else {
        vec![first]
    }
}

/// Most microphone audio kept while waiting for the other sources: the recording begins when the
/// last source delivers, so older samples are never needed.
const MIC_LEAD_MAX_SECONDS: u64 = 2;

/// Records a microphone. Until every source is live (`origin` known) it reads the device as it
/// delivers and keeps the last [`MIC_LEAD_MAX_SECONDS`]; then it trims that to the sample at the
/// recording's start and goes on writing on the recording clock.
#[allow(clippy::too_many_arguments)]
fn mic_worker(
    input: &mut Box<dyn AudioInput>,
    mut wav: WavStream,
    channel: usize,
    opts: AudioOpts,
    clock: RecordClock,
    stats: &SourceStats,
    stop_ns: &AtomicU64,
    origin: &AtomicU64,
) -> std::result::Result<Finished, String> {
    let rate = u64::from(wav.rate.max(1));
    let mut gain = opts.auto_gain.then(|| AutoGain::new(wav.rate));
    let ns_of = |n: u64| -> u64 { (u128::from(n) * 1_000_000_000 / u128::from(rate)).min(u128::from(u64::MAX)) as u64 };
    let n_of = |ns: u64| -> u64 { (u128::from(ns) * u128::from(rate) / 1_000_000_000).min(u128::from(u64::MAX)) as u64 };
    let started = stats.started_ns.load(Ordering::Acquire);
    let started = if started == NONE { 0 } else { started };
    // warm-up: the device's first samples, while the other sources start
    let mut read = 0u64;
    let mut lead: Vec<Vec<f32>> = Vec::new();
    let mut lead_start = NONE;
    let o = loop {
        let o = origin.load(Ordering::Acquire);
        if o != NONE {
            break Some(o);
        }
        if stop_ns.load(Ordering::Acquire) != NONE {
            break None;
        }
        let want = n_of(clock.now_ns().saturating_sub(started)).saturating_sub(read);
        if want > 0 {
            let got = input.read(want.min(rate * 2) as usize);
            if let Some(e) = input.error() {
                input.stop();
                return Err(format!("microphone failed: {e}"));
            }
            let chans = pick_channels(&got, channel, opts.stereo);
            let n = chans.iter().map(Vec::len).min().unwrap_or(0);
            if n > 0 {
                read += n as u64;
                if lead_start == NONE {
                    // the block ends now: its first sample is its length earlier
                    lead_start = clock.now_ns().saturating_sub(ns_of(n as u64)).max(started);
                    stats.delivered(lead_start);
                }
                let peak = chans.iter().flatten().fold(0f32, |m, v| m.max(v.abs()));
                stats.level.store(peak.to_bits(), Ordering::Relaxed);
                if lead.is_empty() {
                    lead = chans
                        .into_iter()
                        .map(|mut c| {
                            c.truncate(n);
                            c
                        })
                        .collect();
                } else {
                    for (l, c) in lead.iter_mut().zip(chans) {
                        l.extend_from_slice(c.get(..n).unwrap_or(&c));
                    }
                }
                let len = lead.first().map_or(0, Vec::len);
                let cap = (rate * MIC_LEAD_MAX_SECONDS) as usize;
                if len > cap {
                    let d = len - cap;
                    for l in &mut lead {
                        l.drain(..d.min(l.len()));
                    }
                    lead_start = lead_start.saturating_add(ns_of(d as u64));
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    let Some(o) = o else {
        input.stop();
        let _ = wav.finish();
        return Err("stopped before the recording began".into());
    };
    let mut put = |wav: &mut WavStream, mut chans: Vec<Vec<f32>>| -> std::result::Result<(), String> {
        // the meter shows the input as it is, never the auto gain
        let peak = chans.iter().flatten().fold(0f32, |m, v| m.max(v.abs()));
        stats.level.store(peak.to_bits(), Ordering::Relaxed);
        if let Some(g) = gain.as_mut() {
            g.process(&mut chans);
        }
        wav.write(&chans)?;
        stats.bytes.store(wav.bytes(), Ordering::Relaxed);
        stats.frames.store(wav.frames, Ordering::Relaxed);
        if wav.frames > 0 {
            stats.last_ns.store(ns_of(wav.frames), Ordering::Release);
        }
        Ok(())
    };
    // the lead, trimmed to the sample at the start (or, when the estimate of the device's first
    // sample is a little after it, preceded by that much silence)
    if lead_start != NONE && !lead.is_empty() {
        if lead_start < o {
            let d = n_of(o - lead_start) as usize;
            for l in &mut lead {
                l.drain(..d.min(l.len()));
            }
            stats.first_ns.store(0, Ordering::Release);
        } else {
            let pad = n_of(lead_start - o).min(rate) as usize;
            if pad > 0 {
                put(&mut wav, vec![vec![0.0; pad]; lead.len()])?;
            }
            stats.first_ns.store(lead_start - o, Ordering::Release);
        }
        put(&mut wav, std::mem::take(&mut lead))?;
    } else {
        stats.first_ns.store(0, Ordering::Release);
    }
    let due = |ns: u64| -> u64 { n_of(ns.saturating_sub(o)) };
    loop {
        let end = stop_ns.load(Ordering::Acquire);
        let now = if end == NONE { clock.now_ns() } else { end };
        let want = due(now).saturating_sub(wav.frames);
        if want > 0 {
            let got = input.read(want.min(rate * 2) as usize);
            if let Some(e) = input.error() {
                return Err(format!("microphone failed: {e}"));
            }
            let chans = pick_channels(&got, channel, opts.stereo);
            if chans.iter().map(Vec::len).min().unwrap_or(0) > 0 {
                put(&mut wav, chans)?;
            }
        }
        if end != NONE {
            // a live device may still hold the last few milliseconds: take what it has, once
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    input.stop();
    let samples = wav.frames;
    let (sample_rate, channels, format) = (wav.rate, wav.channels, wav.format);
    let bytes = wav.finish().map_err(|e| e.to_string())?;
    if samples == 0 {
        return Err("no audio was captured".into());
    }
    let mut notes = Vec::new();
    if let Some(g) = &gain {
        notes.push(format!("auto gain toward {AUTO_GAIN_TARGET_DB} dBFS, {:.1} dB at the end", g.gain_db()));
    }
    Ok(Finished { frames: samples, bytes, sample_rate, samples, channels, format: format.id(), auto_gain: opts.auto_gain, notes, ..Default::default() })
}

/// Writes the system audio blocks of a screen into a WAV from the recording's start (`origin`):
/// what was captured before it is trimmed to the sample, and gaps (nothing played, or nothing
/// yet at the start) are filled with silence so the file stays on the recording clock.
fn system_audio_worker(
    queue: Arc<BoundedQueue<CapturedAudio>>,
    mut wav: WavStream,
    stats: Arc<SourceStats>,
    origin: Arc<AtomicU64>,
) -> std::result::Result<Finished, String> {
    let Some(origin) = wait_origin(&origin, || queue.is_closed()) else {
        return Err("stopped before the recording began".into());
    };
    loop {
        let b = match queue.pop(std::time::Duration::from_millis(100)) {
            None => break,
            Some(None) => continue,
            Some(Some(b)) => b,
        };
        if b.channels.is_empty() || b.sample_rate == 0 {
            continue;
        }
        if wav.frames == 0 {
            // the file takes the rate and channels the system delivers
            wav.rate = b.sample_rate.clamp(8_000, 192_000);
            wav.channels = (b.channels.len() as u16).clamp(1, 2);
        }
        let rate = u64::from(wav.rate);
        let mut chans = b.channels;
        let rel = match b.time_ns.checked_sub(origin) {
            Some(r) => r,
            None => {
                // captured before the start: keep the part after it
                let skip = (u128::from(origin - b.time_ns) * u128::from(rate)).div_ceil(1_000_000_000).min(usize::MAX as u128) as usize;
                if skip >= chans.iter().map(Vec::len).min().unwrap_or(0) {
                    continue;
                }
                for c in &mut chans {
                    c.drain(..skip.min(c.len()));
                }
                0
            }
        };
        if stats.first().is_none() {
            stats.first_ns.store(rel, Ordering::Release);
        }
        let expected = wav.frames.saturating_mul(1_000_000_000) / rate;
        // the first block is put exactly where it was captured; later, only real gaps are filled
        let tolerance = if wav.frames == 0 { 0 } else { 50_000_000 };
        if rel > expected.saturating_add(tolerance) {
            // silence up to this block, at most a minute per gap
            let gap = (u128::from(rel - expected) * u128::from(rate) / 1_000_000_000).min(u128::from(rate) * 60) as usize;
            let mut left = gap;
            while left > 0 {
                let n = left.min(wav.rate as usize);
                wav.write(&vec![vec![0.0; n]; usize::from(wav.channels)])?;
                left -= n;
            }
        }
        wav.write(&chans)?;
        stats.frames.store(wav.frames, Ordering::Relaxed);
        stats.bytes.store(wav.bytes(), Ordering::Relaxed);
        stats.last_ns.store(wav.frames.saturating_mul(1_000_000_000) / rate, Ordering::Release);
        let peak = chans.iter().flatten().fold(0f32, |m, v| m.max(v.abs()));
        stats.level.store(peak.to_bits(), Ordering::Relaxed);
    }
    let samples = wav.frames;
    let (sample_rate, channels, format) = (wav.rate, wav.channels, wav.format);
    let bytes = wav.finish().map_err(|e| e.to_string())?;
    if samples == 0 {
        return Err("no system audio arrived".into());
    }
    Ok(Finished { frames: samples, bytes, sample_rate, samples, channels, format: format.id(), ..Default::default() })
}

// ------------------------------------------------------------------------------------- files

/// Folder recordings are written to (the voice-over rule, with `Recordings`).
fn record_dir(s: &Session, p: &Value, rs: &RecordingSettings) -> PathBuf {
    if let Some(d) = str_p(p, "dir").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    if !rs.output_folder.trim().is_empty() {
        return PathBuf::from(rs.output_folder.trim());
    }
    if let Some(d) = s.project.settings.scratch.captured.clone().filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    if let Some(d) = s.path.as_deref().and_then(|x| Path::new(x).parent()).filter(|d| !d.as_os_str().is_empty()) {
        return d.to_path_buf();
    }
    let base = s.prefs_path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf).unwrap_or_else(|| crate::temp_dir().join("FilmCraft"));
    base.join("Recordings")
}

/// A recording name typed by the user: 1–100 characters, no path separators, `:`, control
/// characters, leading `.` or `..`.
pub fn check_name(name: &str) -> std::result::Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("the name can't be empty".into());
    }
    if n.chars().count() > 100 {
        return Err("the name is longer than 100 characters".into());
    }
    if n.contains(['/', '\\', ':']) || n.chars().any(char::is_control) || n.starts_with('.') || n.contains("..") {
        return Err(format!("`{n}` can't be a file name (no / \\ : control characters, leading . or ..)"));
    }
    Ok(n.to_string())
}

fn file_name(name: &str, src: Src) -> String {
    format!("{name} - {}.{}", src.label(), src.kind.extension())
}

fn sidecar_path(file: &Path) -> PathBuf {
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    file.with_file_name(format!("{stem}.recording.json"))
}

/// `Recording <n>` (or `name`, `name 2`…) such that none of this recording's files exists and no
/// project item has the name.
fn pick_name(s: &Session, dir: &Path, base: Option<&str>, kinds: &[Src]) -> String {
    let items: std::collections::HashSet<&str> = s.project.items.values().map(|i| i.name.as_str()).collect();
    let free = |n: &str| !items.contains(n) && kinds.iter().all(|k| !dir.join(file_name(n, *k)).exists() && !items.contains(file_name(n, *k).as_str()));
    match base {
        Some(b) => {
            if free(b) {
                return b.to_string();
            }
            (2..10_000).map(|k| format!("{b} {k}")).find(|n| free(n)).unwrap_or_else(|| b.to_string())
        }
        None => (1..10_000).map(|k| format!("Recording {k}")).find(|n| free(n)).unwrap_or_else(|| "Recording".into()),
    }
}

fn remove_files(files: &[(Src, PathBuf)]) {
    for (_, f) in files {
        let _ = std::fs::remove_file(f);
        let _ = std::fs::remove_file(sidecar_path(f));
    }
}

// ------------------------------------------------------------------------------------- commands

fn can_start(s: &Session) -> std::result::Result<(), String> {
    if cfg!(target_arch = "wasm32") {
        return Err("recording is not available in the web app".into());
    }
    if s.record.recording() {
        return Err("a recording is already running".into());
    }
    if s.record.countdown.is_some() {
        return Err("a recording is already counting down".into());
    }
    if s.record.starting.is_some() {
        return Err("a recording is already starting".into());
    }
    if s.voiceover.recording() {
        return Err("a voice-over is recording".into());
    }
    Ok(())
}

fn is_recording(s: &Session) -> std::result::Result<(), String> {
    if s.record.recording() || s.record.countdown.is_some() || s.record.starting.is_some() { Ok(()) } else { Err("nothing is recording".into()) }
}

fn devices(s: &mut Session, _p: &Value) -> Result<Value> {
    let f = factory(s);
    let (dev, error) = match f.devices() {
        Ok(d) => {
            let e = (!d.problems.is_empty()).then(|| d.problems.join("; "));
            (d, e)
        }
        Err(e) => (VideoDevices::default(), Some(e.message)),
    };
    let microphones = match (&s.record.active, &s.record.starting) {
        (Some(a), _) => a.microphones.clone(),
        (None, Some(st)) => st.microphones.clone(),
        (None, None) => s.voiceover.input.get_or_insert_with(|| Box::new(SyntheticInput::clicks())).devices(),
    };
    Ok(json!({
        "displays": dev.displays,
        "windows": dev.windows,
        "cameras": dev.cameras,
        "microphones": microphones,
        "factory": f.name(),
        "permissions": f.permissions(),
        "systemAudio": f.system_audio(),
        "error": error,
    }))
}

pub(crate) fn fps_of(v: &Value, cmd: &str, default: u32) -> Result<u32> {
    match v.get("fps").filter(|x| !x.is_null()) {
        None => Ok(default),
        Some(x) => {
            let f = x.as_f64().filter(|f| f.is_finite()).ok_or_else(|| bad(cmd, "`fps` must be a number"))?;
            if !(1.0..=60.0).contains(&f) {
                return Err(bad(cmd, format!("`fps` must be 1–60, got {f}")));
            }
            Ok(f.round() as u32)
        }
    }
}

pub(crate) fn size_of(v: &Value, key: &str, cmd: &str) -> Result<Option<u32>> {
    match v.get(key).filter(|x| !x.is_null()) {
        None => Ok(None),
        Some(x) => {
            let f = x.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0).ok_or_else(|| bad(cmd, format!("`{key}` must be a whole number")))?;
            if !(16.0..=8192.0).contains(&f) {
                return Err(bad(cmd, format!("`{key}` must be 16–8192, got {f}")));
            }
            Ok(Some(f as u32))
        }
    }
}

pub(crate) fn id_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str().map(str::to_string).or_else(|| x.as_u64().map(|n| n.to_string())))
}

pub(crate) fn bool_of(v: &Value, key: &str, cmd: &str, default: bool) -> Result<bool> {
    match v.get(key).filter(|x| !x.is_null()) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad(cmd, format!("`{key}` must be true or false"))),
    }
}

pub(crate) fn choice_of(v: &Value, key: &str, cmd: &str, opts: &[(&str, &str)], default: &str) -> Result<String> {
    match v.get(key).filter(|x| !x.is_null()) {
        None => Ok(default.to_string()),
        Some(Value::String(c)) if opts.iter().any(|o| o.0 == c) => Ok(c.clone()),
        Some(x) => Err(bad(cmd, format!("`{key}` must be one of {}, got {x}", opts.iter().map(|o| o.0).collect::<Vec<_>>().join(", ")))),
    }
}

/// The screen of a recording.
struct ScreenPlan {
    target: ScreenTarget,
    req: VideoRequest,
    system_audio: bool,
}

/// One camera of a recording.
struct CameraPlan {
    device: String,
    req: VideoRequest,
    mirror: bool,
    /// Auto (the camera's orientation) or 0 / 90 / 180 / 270 degrees clockwise.
    rotate: CameraRotate,
}

struct Plan {
    screen: Option<ScreenPlan>,
    cameras: Vec<CameraPlan>,
    /// Device names ("" = the system default).
    mics: Vec<String>,
    /// Settings ▸ Recording with `record.start {settings}` merged in.
    settings: RecordingSettings,
}

/// `key` (a list) or `alias` (one object, a one-element list); `null` / `false` / absent = none.
fn list_p<'a>(p: &'a Value, key: &str, alias: &str, cmd: &str) -> Result<Vec<&'a Value>> {
    let many = p.get(key).filter(|v| !v.is_null());
    let one = p.get(alias).filter(|v| !v.is_null() && v.as_bool() != Some(false));
    if many.is_some() && one.is_some() {
        return Err(bad(cmd, format!("give `{key}` or `{alias}`, not both")));
    }
    let v: Vec<&Value> = match (many, one) {
        (Some(Value::Array(a)), _) => a.iter().collect(),
        (Some(_), _) => return Err(bad(cmd, format!("`{key}` must be a list"))),
        (None, Some(o)) => vec![o],
        (None, None) => Vec::new(),
    };
    if v.len() > MAX_PER_KIND {
        return Err(bad(cmd, format!("at most {MAX_PER_KIND} `{key}`, got {}", v.len())));
    }
    if let Some(x) = v.iter().find(|x| !x.is_object()) {
        return Err(bad(cmd, format!("each of `{key}` must be an object, got {x}")));
    }
    Ok(v)
}

fn plan(s: &Session, p: &Value) -> Result<Plan> {
    let cmd = "record.start";
    let settings = match p.get("settings").filter(|v| !v.is_null()) {
        None => s.prefs.recording.clone(),
        Some(v) => crate::record_settings::merge(&s.prefs.recording, v).map_err(|e| bad(cmd, e))?,
    };
    let rs = &settings;
    let screen = match p.get("screen").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            let target = match (id_of(v, "display"), id_of(v, "window")) {
                (Some(d), None) => ScreenTarget::Display(d),
                (None, Some(w)) => ScreenTarget::Window(w),
                _ => return Err(bad(cmd, "`screen` takes {display: id} or {window: id}")),
            };
            let resolution = choice_of(v, "resolution", cmd, crate::record_settings::SCREEN_RESOLUTION, &rs.screen_resolution)?;
            let area = crate::record_preview::area_of(v, cmd)?;
            if area.is_some() && !matches!(target, ScreenTarget::Display(_)) {
                return Err(bad(cmd, "`area` is a part of a display: give it with {display: id}"));
            }
            let req = VideoRequest {
                width: None,
                height: None,
                fps: fps_of(v, cmd, rs.screen_fps)?,
                max_height: crate::record_settings::resolution_height(&resolution),
                show_cursor: bool_of(v, "cursor", cmd, rs.show_cursor)?,
                area,
            };
            Some(ScreenPlan { target, req, system_audio: bool_of(v, "systemAudio", cmd, rs.system_audio)? })
        }
    };
    let mut cameras: Vec<CameraPlan> = Vec::new();
    for v in list_p(p, "cameras", "camera", cmd)? {
        let device = id_of(v, "device").ok_or_else(|| bad(cmd, "a camera takes {device: id}"))?;
        if cameras.iter().any(|c| c.device == device) {
            return Err(bad(cmd, format!("the camera `{device}` is chosen twice")));
        }
        let mirror = bool_of(v, "mirror", cmd, rs.camera_mirror)?;
        let rotate = crate::record_settings::rotate_of(v, cmd, rs.camera_rotate)?;
        let quality = choice_of(v, "quality", cmd, crate::record_settings::CAMERA_QUALITY, &rs.camera_quality)?;
        let (width, height) = (size_of(v, "width", cmd)?, size_of(v, "height", cmd)?);
        let (width, height) = match (width, height) {
            (None, None) => crate::record_settings::camera_size(&quality).map_or((None, None), |(w, h)| (Some(w), Some(h))),
            wh => wh,
        };
        let req = VideoRequest { width, height, fps: fps_of(v, cmd, rs.camera_fps)?, max_height: None, show_cursor: false, area: None };
        cameras.push(CameraPlan { device, req, mirror, rotate });
    }
    let mut mics: Vec<String> = Vec::new();
    for v in list_p(p, "mics", "mic", cmd)? {
        let dev = match v.get("device") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(d)) => d.clone(),
            Some(_) => return Err(bad(cmd, "`mic.device` must be a device name")),
        };
        let dev = if dev.is_empty() {
            let vo = &s.prefs.voice_over;
            if vo.source.is_empty() { s.prefs.audio_hardware.default_input.clone() } else { vo.source.clone() }
        } else {
            dev
        };
        if mics.contains(&dev) {
            let shown = if dev.is_empty() { "the default input" } else { dev.as_str() };
            return Err(bad(cmd, format!("the microphone `{shown}` is chosen twice")));
        }
        mics.push(dev);
    }
    if screen.is_none() && cameras.is_empty() && mics.is_empty() {
        return Err(bad(cmd, "choose at least one source (screen, camera or mic)"));
    }
    Ok(Plan { screen, cameras, mics, settings })
}

/// Stop every started source of a failed (or cancelled) start and delete its files; returns the
/// session's microphone input when it was handed to the recording.
fn abort(mut a: Active) -> Option<Box<dyn AudioInput>> {
    a.stop_ns.store(0, Ordering::Release);
    for v in &mut a.video {
        v.input.stop();
        v.queue.close();
        if let Some(w) = v.worker.take() {
            let _ = w.join();
        }
    }
    if let Some(sa) = a.system_audio.as_mut() {
        sa.queue.close();
        if let Some(w) = sa.worker.take() {
            let _ = w.join();
        }
    }
    let mut back = None;
    for m in &mut a.mics {
        if let Some(w) = m.worker.take()
            && let Ok((input, _)) = w.join()
            && m.session_input
        {
            back = Some(input);
        }
    }
    remove_files(&a.files());
    back
}

/// What a video source is and where it goes.
struct VideoSetup {
    src: Src,
    req: VideoRequest,
    path: PathBuf,
    device_id: String,
    device_name: String,
    mirror: bool,
    rotate: CameraRotate,
    enc: CaptureEncoding,
}

/// The encoding of a recording from its settings.
fn encoding_of(rs: &RecordingSettings) -> CaptureEncoding {
    CaptureEncoding {
        codec: rs.capture_codec(),
        quality: rs.capture_quality(),
        keyframe_seconds: rs.keyframe_seconds.clamp(1, 10),
        hardware: rs.hardware_encoder,
    }
}

/// Open the MOV writer for a source, falling back to H.264 when HEVC cannot be recorded here
/// and to 15 fps when FilmCraft's own H.264 encoder records above 1080p.
fn open_recorder(path: &Path, w: u32, h: u32, fps: u32, enc: CaptureEncoding, notes: &mut Vec<String>) -> std::result::Result<(MovRecorder, u32), String> {
    let mut enc = enc;
    let mut rec = MovRecorder::create_with(path, w, h, FrameRate::new(i64::from(fps), 1), &enc);
    if enc.codec == CaptureCodec::Hevc
        && let Err(e) = &rec
    {
        notes.push(format!("HEVC was asked for but not available ({e}): recorded as H.264"));
        enc.codec = CaptureCodec::H264;
        rec = MovRecorder::create_with(path, w, h, FrameRate::new(i64::from(fps), 1), &enc);
    }
    let mut fps = fps;
    // our own H.264 encoder cannot do more than about 15 fps above 1080p: record at that rate
    if enc.codec == CaptureCodec::H264 && rec.as_ref().is_ok_and(|r| !r.hardware()) && u64::from(w) * u64::from(h) > 1920 * 1080 && fps > 15 {
        notes.push(format!("FilmCraft's own H.264 encoder records above 1080p at most at 15 fps (asked for {fps})"));
        fps = 15;
        rec = MovRecorder::create_with(path, w, h, FrameRate::new(15, 1), &CaptureEncoding { hardware: false, ..enc });
    }
    rec.map(|r| (r, fps)).map_err(|e| e.to_string())
}

/// Start a video source on the start clock: its frames wait in its queue until the recording's
/// start (`origin`) is known, and frames captured before that start are left out.
fn start_video(
    mut input: Box<dyn VideoInput>,
    setup: VideoSetup,
    clock: RecordClock,
    stop_ns: &Arc<AtomicU64>,
    origin: &Arc<AtomicU64>,
) -> std::result::Result<VideoSource, String> {
    let VideoSetup { src, req, path, device_id, device_name, mirror, rotate, enc } = setup;
    let queue = Arc::new(FrameQueue::new(4));
    let stats = Arc::new(SourceStats::default());
    let (q, st, o) = (queue.clone(), stats.clone(), origin.clone());
    let sink: FrameSink = Arc::new(move |f| {
        st.delivered(f.time_ns);
        let start = o.load(Ordering::Acquire);
        if start == NONE {
            // warming up: older frames make room silently (they precede the start anyway)
            q.push(f);
        } else if f.time_ns >= start && q.push(f) {
            st.dropped.fetch_add(1, Ordering::Relaxed);
        }
    });
    let label = src.key();
    stats.started_ns.store(clock.now_ns(), Ordering::Release);
    let fmt = input.start(&req, clock, sink).map_err(|e| format!("{label}: {e}"))?;
    let (w, h) = (fmt.width, fmt.height);
    // a source that could not scale itself is scaled in software (see `to_rgba`)
    let (w, h) = req.max_height.and_then(|mh| crate::record_settings::downscale((w, h), mh)).unwrap_or((w, h));
    let (w, h) = (w.clamp(16, 8192) & !1, h.clamp(16, 8192) & !1);
    let mut notes = Vec::new();
    if src.kind == SourceKind::Camera && fmt.fps != req.fps {
        notes.push(format!("the camera delivers {} fps, not the {} asked for", fmt.fps, req.fps));
    }
    let (mut rec, fps) = match open_recorder(&path, w, h, fmt.fps.clamp(1, 60), enc, &mut notes) {
        Ok(r) => r,
        Err(e) => {
            input.stop();
            let _ = std::fs::remove_file(&path);
            return Err(format!("{label} encoder: {e}"));
        }
    };
    // a turned camera: the file's track header says so (as iPhones do); the pictures stay as
    // captured. Auto takes the orientation the camera reports now, at the start (a change made
    // while it previewed, in the camera's own software, counts); a fixed Rotate wins.
    let camera_rotation = if src.kind == SourceKind::Camera { input.rotation() } else { None };
    let rotate_setting = rotate;
    let rotate = if src.kind == SourceKind::Camera { rotate.effective(camera_rotation) } else { 0 };
    rec.set_rotation(u16::try_from(rotate).unwrap_or(0));
    let (q, st, stop, o) = (queue.clone(), stats.clone(), stop_ns.clone(), origin.clone());
    let rate = FrameRate::new(i64::from(fps), 1);
    let worker = std::thread::Builder::new()
        .name(format!("filmcraft-record-{label}"))
        .spawn(move || {
            std::panic::catch_unwind(AssertUnwindSafe(|| video_worker(rec, q, st, stop, o, rate)))
                .unwrap_or_else(|p| Err(format!("the encoder crashed: {}", panic_text(&p))))
        })
        .map_err(|e| format!("{label}: cannot start the encoder thread: {e}"));
    let worker = match worker {
        Ok(w) => w,
        Err(e) => {
            input.stop();
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    Ok(VideoSource {
        src,
        notes,
        device_id,
        device_name,
        mirror,
        rotate,
        rotate_setting,
        camera_rotation,
        rotation_seen: camera_rotation,
        rotation_changes: 0,
        rotation_changed_at_ns: None,
        area: req.area,
        input,
        queue,
        stats,
        worker: Some(worker),
        path,
    })
}

/// Ask a screen input for its system audio, into `path`; None when it cannot (the start goes on
/// without it and says so).
fn start_system_audio(
    input: &mut Box<dyn VideoInput>,
    path: &Path,
    rs: &RecordingSettings,
    clock: RecordClock,
    origin: &Arc<AtomicU64>,
) -> std::result::Result<Option<AudioSource>, String> {
    let channels = if rs.stereo() { 2 } else { 1 };
    let format = WavFormat::from_id(&rs.audio_format);
    let queue: Arc<BoundedQueue<CapturedAudio>> = Arc::new(BoundedQueue::new(512));
    let stats = Arc::new(SourceStats::default());
    stats.started_ns.store(clock.now_ns(), Ordering::Release);
    let (q, st, o) = (queue.clone(), stats.clone(), origin.clone());
    let sink: AudioSink = Arc::new(move |b| {
        st.delivered(b.time_ns);
        if q.push(b) && o.load(Ordering::Acquire) != NONE {
            st.dropped.fetch_add(1, Ordering::Relaxed);
        }
    });
    if !input.capture_audio(rs.sample_rate, channels, sink) {
        return Ok(None);
    }
    let wav = WavStream::create(path, rs.sample_rate, channels, format).map_err(|e| format!("{}: {e}", path.display()))?;
    let (q, st, o) = (queue.clone(), stats.clone(), origin.clone());
    let worker = std::thread::Builder::new()
        .name("filmcraft-record-system-audio".into())
        .spawn(move || {
            std::panic::catch_unwind(AssertUnwindSafe(|| system_audio_worker(q, wav, st, o)))
                .unwrap_or_else(|p| Err(format!("the system audio recorder crashed: {}", panic_text(&p))))
        })
        .map_err(|e| format!("system audio: cannot start the recording thread: {e}"))?;
    Ok(Some(AudioSource { src: Src::new(SourceKind::SystemAudio, 0), queue, stats, worker: Some(worker), path: path.to_path_buf() }))
}

/// Start the microphone `device` into `path` on the start clock (`input` moves onto the
/// recording thread; it comes back with the error when the start fails).
#[allow(clippy::too_many_arguments)]
fn start_mic(
    mut input: Box<dyn AudioInput>,
    src: Src,
    device: &str,
    path: PathBuf,
    clock: RecordClock,
    stop_ns: &Arc<AtomicU64>,
    origin: &Arc<AtomicU64>,
    rs: &RecordingSettings,
    input_channel: u32,
) -> std::result::Result<(MicSource, Option<String>), (Box<dyn AudioInput>, String)> {
    let label = src.key();
    let stats = Arc::new(SourceStats::default());
    stats.started_ns.store(clock.now_ns(), Ordering::Release);
    let fmt = match input.start(device, rs.sample_rate) {
        Ok(f) => f,
        Err(e) => {
            return Err((
                input,
                format!("{label}: {e} (on macOS, allow FilmCraft or the terminal that launched it in System Settings ▸ Privacy & Security ▸ Microphone)"),
            ));
        }
    };
    let note = (fmt.sample_rate != rs.sample_rate).then(|| format!("the device records at {} Hz, not the {} Hz asked for", fmt.sample_rate, rs.sample_rate));
    let channel = (input_channel as usize).min(usize::from(fmt.channels.max(1)) - 1);
    let opts = AudioOpts { rate: fmt.sample_rate.max(1), stereo: rs.stereo(), format: WavFormat::from_id(&rs.audio_format), auto_gain: rs.auto_gain };
    let wav = match WavStream::create(&path, opts.rate, if opts.stereo { 2 } else { 1 }, opts.format) {
        Ok(w) => w,
        Err(e) => {
            input.stop();
            return Err((input, format!("{}: {e}", path.display())));
        }
    };
    let (st, stop, o) = (stats.clone(), stop_ns.clone(), origin.clone());
    // the input moves to the thread and comes back from it; if the thread cannot start, the
    // input is lost with it, so a spare is made first for the session
    let spare = input.spawn();
    let spawned = std::thread::Builder::new().name(format!("filmcraft-record-{label}")).spawn(move || {
        let mut input = input;
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| mic_worker(&mut input, wav, channel, opts, clock, &st, &stop, &o)))
            .unwrap_or_else(|p| Err(format!("the microphone recorder crashed: {}", panic_text(&p))));
        (input, r)
    });
    match spawned {
        Ok(w) => Ok((
            MicSource {
                src,
                device: if device.is_empty() { "Default".into() } else { device.to_string() },
                session_input: false,
                stats,
                worker: Some(w),
                path,
            },
            note,
        )),
        Err(e) => Err((spare.unwrap_or_else(|| Box::new(SyntheticInput::clicks())), format!("{label}: cannot start the recording thread: {e}"))),
    }
}

/// `record.start {countdown}`: whole seconds, 0–10.
fn countdown_of(p: &Value, cmd: &str) -> Result<u32> {
    match p.get("countdown").filter(|v| !v.is_null()) {
        None => Ok(0),
        Some(v) => {
            let f = v.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0).ok_or_else(|| bad(cmd, "`countdown` must be whole seconds"))?;
            if !(0.0..=10.0).contains(&f) {
                return Err(bad(cmd, format!("`countdown` must be 0–10 seconds, got {f}")));
            }
            Ok(f as u32)
        }
    }
}

/// The names of the plan's screen and cameras and the microphone list, refusing a device that
/// is not there (before anything is opened).
fn resolve_devices(s: &mut Session, plan: &Plan, f: &Arc<dyn VideoInputFactory>) -> Result<(Option<String>, Vec<String>, Vec<String>)> {
    let cmd = "record.start";
    let needs_video = plan.screen.is_some() || !plan.cameras.is_empty();
    let devs = if needs_video { f.devices().map_err(|e| EngineError::Other(e.message))? } else { VideoDevices::default() };
    let screen_name = match plan.screen.as_ref().map(|x| &x.target) {
        Some(ScreenTarget::Display(id)) => {
            Some(devs.displays.iter().find(|d| &d.id == id).map(|d| d.name.clone()).ok_or_else(|| bad(cmd, format!("no display `{id}`")))?)
        }
        Some(ScreenTarget::Window(id)) => {
            Some(devs.windows.iter().find(|w| &w.id == id).map(|w| format!("{} — {}", w.app, w.title)).ok_or_else(|| bad(cmd, format!("no window `{id}`")))?)
        }
        None => None,
    };
    let mut camera_names = Vec::new();
    for c in &plan.cameras {
        let id = &c.device;
        camera_names.push(devs.cameras.iter().find(|x| &x.id == id).map(|x| x.name.clone()).ok_or_else(|| bad(cmd, format!("no camera `{id}`")))?);
    }
    let microphones = s.voiceover.input.get_or_insert_with(|| Box::new(SyntheticInput::clicks())).devices();
    for m in &plan.mics {
        if !m.is_empty() && !microphones.iter().any(|d| d == m) {
            return Err(bad(cmd, format!("no microphone `{m}`")));
        }
    }
    Ok((screen_name, camera_names, microphones))
}

/// A `record.start` whose sources are starting on their own thread: `record.status` says
/// `starting`, [`tick`] (or a waiting `record.start`) collects it.
pub struct Starting {
    /// What the hosts show ("Starting screen capture…").
    pub label: String,
    /// The microphone names (the session's input is with the starting recording).
    microphones: Vec<String>,
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<StartOutcome>>,
    /// A fresh input of the session's kind, for the session when the start thread lost its own.
    spare: Option<Box<dyn AudioInput>>,
}

impl Starting {
    fn finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }
    fn join(&mut self) -> StartOutcome {
        match self.handle.take().map(|h| h.join()) {
            Some(Ok(o)) => o,
            Some(Err(p)) => StartOutcome { session_input: None, result: Err(format!("the recording start crashed: {}", panic_text(&p))) },
            None => StartOutcome { session_input: None, result: Err("the recording did not start".into()) },
        }
    }
}

/// What the start thread hands back: the recording (every source live), or why it failed (its
/// sources stopped and files deleted), with the session's microphone input when it holds it.
struct StartOutcome {
    session_input: Option<Box<dyn AudioInput>>,
    result: std::result::Result<Active, String>,
}

/// A microphone to start: its input (the session's for the first), its source and device name.
type MicStart = (Box<dyn AudioInput>, Src, String);

/// Everything the start thread needs.
struct StartJob {
    active: Active,
    screen: Option<(Box<dyn VideoInput>, VideoSetup, bool)>,
    cameras: Vec<(Box<dyn VideoInput>, VideoSetup)>,
    mics: Vec<MicStart>,
    input_channel: u32,
    timeout: std::time::Duration,
    cancel: Arc<AtomicBool>,
}

/// A source in words: `screen`, `camera 2`, `microphone`…
fn spoken(src: Src) -> String {
    let k = match src.kind {
        SourceKind::Screen => "screen",
        SourceKind::Camera => "camera",
        SourceKind::Mic => "microphone",
        SourceKind::SystemAudio => "system audio",
    };
    if src.n == 0 { k.to_string() } else { format!("{k} {}", src.n + 1) }
}

/// Wait until every screen, camera and microphone delivered its first sample; returns the
/// latest of those times (on the start clock): the recording's start. System audio is not
/// waited for (nothing may be playing); it is filled with silence from the start.
fn await_live(a: &Active, deadline: web_time::Instant, timeout: std::time::Duration, cancel: &AtomicBool) -> std::result::Result<u64, String> {
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err("the recording was cancelled while it was starting".into());
        }
        let mut latest = 0u64;
        let mut waiting: Option<String> = None;
        for v in &a.video {
            if let Some(e) = v.input.error() {
                return Err(format!("{}: {e}", v.src.key()));
            }
            if v.worker.as_ref().is_some_and(|w| w.is_finished()) {
                return Err(format!("{}: the encoder stopped", v.src.key()));
            }
            match v.stats.raw_first() {
                Some(t) => latest = latest.max(t),
                None => {
                    waiting.get_or_insert_with(|| format!("{} '{}' delivered no frames", spoken(v.src), v.device_name));
                }
            }
        }
        for m in &a.mics {
            if m.worker.as_ref().is_some_and(|w| w.is_finished()) {
                return Err(format!("{} '{}' stopped before it delivered audio", spoken(m.src), m.device));
            }
            match m.stats.raw_first() {
                Some(t) => latest = latest.max(t),
                None => {
                    waiting.get_or_insert_with(|| format!("{} '{}' delivered no audio", spoken(m.src), m.device));
                }
            }
        }
        let Some(w) = waiting else { return Ok(latest) };
        if web_time::Instant::now() >= deadline {
            return Err(format!("{w} within {} s", timeout.as_secs_f64()));
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// The start thread: start every source on the start clock, wait until each one delivers, then
/// begin the recording at the latest first sample.
fn run_start(job: StartJob) -> StartOutcome {
    let StartJob { mut active, screen, cameras, mics, input_channel, timeout, cancel } = job;
    let began = web_time::Instant::now();
    let deadline = began.checked_add(timeout).unwrap_or(began);
    let (raw, stop_ns, origin) = (active.raw, active.stop_ns.clone(), active.origin.clone());
    let rs = active.settings.clone();
    let (dir, name) = (active.dir.clone(), active.name.clone());
    let fail = |a: Active, pending: Vec<MicStart>, msg: String| -> StartOutcome {
        let back = abort(a);
        let unstarted = pending.into_iter().find(|m| m.1.n == 0).map(|m| m.0);
        StartOutcome { session_input: back.or(unstarted), result: Err(msg) }
    };
    if let Some((mut input, setup, system_audio)) = screen {
        if system_audio {
            let path = dir.join(file_name(&name, Src::new(SourceKind::SystemAudio, 0)));
            match start_system_audio(&mut input, &path, &rs, raw, &origin) {
                Ok(Some(a)) => active.system_audio = Some(a),
                Ok(None) => active.notes.push("system audio is not available for this screen".into()),
                Err(e) => active.notes.push(format!("system audio: {e}")),
            }
        }
        match start_video(input, setup, raw, &stop_ns, &origin) {
            Ok(v) => active.video.push(v),
            Err(e) => return fail(active, mics, e),
        }
    }
    for (input, setup) in cameras {
        if cancel.load(Ordering::Acquire) {
            return fail(active, mics, "the recording was cancelled while it was starting".into());
        }
        match start_video(input, setup, raw, &stop_ns, &origin) {
            Ok(v) => active.video.push(v),
            Err(e) => return fail(active, mics, e),
        }
    }
    let mut pending = mics;
    while !pending.is_empty() {
        let (input, src, device) = pending.remove(0);
        let path = dir.join(file_name(&name, src));
        match start_mic(input, src, &device, path, raw, &stop_ns, &origin, &rs, input_channel) {
            Ok((mut m, note)) => {
                m.session_input = src.n == 0;
                if let Some(note) = note {
                    active.notes.push(format!("{}: {note}", src.key()));
                }
                active.mics.push(m);
            }
            Err((input, e)) => {
                if src.n == 0 {
                    pending.insert(0, (input, src, device));
                }
                return fail(active, pending, e);
            }
        }
    }
    match await_live(&active, deadline, timeout, &cancel) {
        Ok(o) => {
            origin.store(o, Ordering::Release);
            active.clock = raw.shifted(o);
            StartOutcome { session_input: None, result: Ok(active) }
        }
        Err(e) => fail(active, Vec::new(), e),
    }
}

/// What the hosts show while the sources start.
fn starting_label(plan: &Plan) -> String {
    if plan.screen.is_some() {
        "Starting screen capture…".into()
    } else if plan.cameras.len() > 1 {
        "Starting the cameras…".into()
    } else if !plan.cameras.is_empty() {
        "Starting the camera…".into()
    } else {
        "Starting the microphone…".into()
    }
}

fn start(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "record.start";
    can_start(s).map_err(|e| bad(cmd, e))?;
    let countdown = countdown_of(p, cmd)?;
    let wait = bool_of(p, "wait", cmd, true)?;
    let plan = plan(s, p)?;
    let name = match str_p(p, "name") {
        Some(n) => Some(check_name(n).map_err(|e| bad(cmd, e))?),
        None => None,
    };
    let f = factory(s);
    let (screen_name, camera_names, microphones) = resolve_devices(s, &plan, &f)?;
    if countdown > 0 {
        // checked now, started by `tick` when the countdown is over (before any source starts)
        let mut params = p.clone();
        if let Some(o) = params.as_object_mut() {
            o.remove("countdown");
        }
        let due_ms = s.record.now_ms().saturating_add(u64::from(countdown) * 1000);
        s.record.countdown = Some(Countdown { params, due_ms, seconds: countdown });
        s.record.last_event = None;
        return Ok(json!({"recording": false, "countdown": countdown}));
    }
    // a second microphone needs a second input of the same kind
    let mut extra_inputs: Vec<Box<dyn AudioInput>> = Vec::new();
    for _ in 1..plan.mics.len() {
        match s.voiceover.input.as_ref().and_then(|i| i.spawn()) {
            Some(i) => extra_inputs.push(i),
            None => return Err(bad(cmd, "this system records one microphone at a time")),
        }
    }
    let rs = plan.settings.clone();
    let mut srcs: Vec<Src> = Vec::new();
    if plan.screen.is_some() {
        srcs.push(Src::new(SourceKind::Screen, 0));
    }
    srcs.extend((0..plan.cameras.len()).map(|n| Src::new(SourceKind::Camera, n)));
    srcs.extend((0..plan.mics.len()).map(|n| Src::new(SourceKind::Mic, n)));
    if plan.screen.as_ref().is_some_and(|x| x.system_audio) {
        srcs.push(Src::new(SourceKind::SystemAudio, 0));
    }
    let dir = record_dir(s, p, &rs);
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{}: {e}", dir.display())))?;
    let name = pick_name(s, &dir, name.as_deref(), &srcs);
    let raw = RecordClock::new();
    let active = Active {
        name: name.clone(),
        dir: dir.clone(),
        clock: raw,
        raw,
        origin: Arc::new(AtomicU64::new(NONE)),
        video: Vec::new(),
        mics: Vec::new(),
        system_audio: None,
        stop_ns: Arc::new(AtomicU64::new(NONE)),
        microphones: microphones.clone(),
        settings: rs.clone(),
        notes: Vec::new(),
        started_ms: 0,
        stop_after_ms: (rs.stop_after_minutes > 0).then(|| u64::from(rs.stop_after_minutes) * 60_000),
    };
    let enc = encoding_of(&rs);
    // the inputs are opened here (permission checks, device lookups: quick, and refused at once);
    // they start on the start thread
    let screen = match &plan.screen {
        Some(sp) => {
            let src = Src::new(SourceKind::Screen, 0);
            let id = match &sp.target {
                ScreenTarget::Display(d) | ScreenTarget::Window(d) => d.clone(),
            };
            let setup = VideoSetup {
                src,
                req: sp.req,
                path: dir.join(file_name(&name, src)),
                device_id: id,
                device_name: screen_name.clone().unwrap_or_default(),
                mirror: false,
                rotate: CameraRotate::Fixed(0),
                enc,
            };
            let input = f.open_screen(&sp.target).map_err(|e| EngineError::Other(format!("screen: {e}")))?;
            Some((input, setup, sp.system_audio))
        }
        None => None,
    };
    let mut cameras = Vec::new();
    for (n, (c, cname)) in plan.cameras.iter().zip(&camera_names).enumerate() {
        let src = Src::new(SourceKind::Camera, n);
        let setup = VideoSetup {
            src,
            req: c.req,
            path: dir.join(file_name(&name, src)),
            device_id: c.device.clone(),
            device_name: cname.clone(),
            mirror: c.mirror,
            rotate: c.rotate,
            enc,
        };
        let input = crate::record_preview::camera_input(&mut s.record, &f, &c.device).map_err(|e| EngineError::Other(format!("{}: {e}", src.key())))?;
        cameras.push((input, setup));
    }
    // microphones: the first on the session's input, the others on inputs made for them
    let mut extra = extra_inputs.into_iter();
    let mut mics: Vec<MicStart> = Vec::new();
    for (n, device) in plan.mics.iter().enumerate() {
        let input = if n == 0 {
            s.voiceover.input.take().unwrap_or_else(|| Box::new(SyntheticInput::clicks()))
        } else {
            extra.next().unwrap_or_else(|| Box::new(SyntheticInput::clicks()))
        };
        mics.push((input, Src::new(SourceKind::Mic, n), device.clone()));
    }
    let spare = mics.first().and_then(|m| m.0.spawn());
    let label = starting_label(&plan);
    let cancel = Arc::new(AtomicBool::new(false));
    let job = StartJob {
        active,
        screen,
        cameras,
        mics,
        input_channel: s.prefs.voice_over.input_channel,
        timeout: s.record.start_timeout.unwrap_or(START_TIMEOUT),
        cancel: cancel.clone(),
    };
    let handle = std::thread::Builder::new().name("filmcraft-record-start".into()).spawn(move || {
        std::panic::catch_unwind(AssertUnwindSafe(|| run_start(job)))
            .unwrap_or_else(|p| StartOutcome { session_input: None, result: Err(format!("the recording start crashed: {}", panic_text(&p))) })
    });
    let handle = match handle {
        Ok(h) => h,
        Err(e) => {
            if s.voiceover.input.is_none() {
                s.voiceover.input = spare;
            }
            return Err(EngineError::Other(format!("cannot start the recording thread: {e}")));
        }
    };
    s.record.starting = Some(Starting { label: label.clone(), microphones, cancel, handle: Some(handle), spare });
    s.record.last_event = None;
    if !wait {
        return Ok(json!({"recording": false, "starting": true, "label": label}));
    }
    match s.record.starting.take() {
        Some(st) => finish_start(s, st),
        None => Err(EngineError::Other("the recording did not start".into())),
    }
}

/// Collect a finished start: the recording runs (its clock starts now for the hosts), or the
/// error. The session gets its microphone input back on failure.
fn finish_start(s: &mut Session, mut st: Starting) -> Result<Value> {
    let o = st.join();
    if let Some(i) = o.session_input {
        s.voiceover.input = Some(i);
    }
    let mut a = match o.result {
        Ok(a) => a,
        Err(e) => {
            if s.voiceover.input.is_none() {
                s.voiceover.input = st.spare.take();
            }
            return Err(EngineError::Other(e));
        }
    };
    a.started_ms = s.record.now_ms();
    let files: Vec<Value> = a
        .files()
        .iter()
        .map(|(k, f)| json!({"kind": k.kind.name(), "key": k.key(), "path": f.to_string_lossy(), "sidecar": sidecar_path(f).to_string_lossy()}))
        .collect();
    let mut warmup = serde_json::Map::new();
    for (k, st) in a.video.iter().map(|v| (v.src, &v.stats)).chain(a.mics.iter().map(|m| (m.src, &m.stats))) {
        warmup.insert(k.key(), json!(st.warmup()));
    }
    let v = json!({
        "recording": true,
        "name": a.name,
        "clockStartNs": a.clock.unix_start_ns(),
        "dir": a.dir.to_string_lossy(),
        "files": files,
        "notes": a.notes,
        "warmupNs": warmup,
    });
    s.record.active = Some(a);
    s.record.last_event = None;
    Ok(v)
}

/// Cancel a start whose sources are starting: wait for its thread (it stops at once unless an OS
/// start is still pending, at most [`START_TIMEOUT`]), stop the sources and delete the files.
fn cancel_start(s: &mut Session) -> Option<Value> {
    let mut st = s.record.starting.take()?;
    st.cancel.store(true, Ordering::Release);
    let o = st.join();
    if let Some(i) = o.session_input {
        s.voiceover.input = Some(i);
    }
    if let Ok(a) = o.result
        && let Some(i) = abort(a)
    {
        s.voiceover.input = Some(i);
    }
    if s.voiceover.input.is_none() {
        s.voiceover.input = st.spare.take();
    }
    Some(json!({"recording": false, "placed": false, "discarded": true, "cancelled": true}))
}

/// Run what is due: a finished start, a counted-down start, a Stop after. The hosts call it
/// every frame; headless sessions through `record.status`. Returns what happened (also kept in
/// [`Recorder::last_event`]).
pub fn tick(s: &mut Session) -> Option<Value> {
    if s.record.starting.as_ref().is_some_and(Starting::finished)
        && let Some(st) = s.record.starting.take()
    {
        let ev = match finish_start(s, st) {
            Ok(v) => json!({"event": "started", "result": v}),
            Err(e) => json!({"event": "startFailed", "error": e.to_string()}),
        };
        s.record.last_event = Some(ev.clone());
        return Some(ev);
    }
    let now = s.record.now_ms();
    if let Some(c) = s.record.countdown.clone()
        && now >= c.due_ms
    {
        s.record.countdown = None;
        // the sources start now, on their own thread (the frame loop never waits for them)
        let mut params = c.params.clone();
        if let Some(o) = params.as_object_mut() {
            o.insert("wait".into(), json!(false));
        }
        match start(s, &params) {
            Ok(_) => {
                s.record.last_event = None;
                return None;
            }
            Err(e) => {
                let ev = json!({"event": "startFailed", "error": e.to_string()});
                s.record.last_event = Some(ev.clone());
                return Some(ev);
            }
        }
    }
    if let Some(a) = s.record.active.as_mut() {
        a.watch_rotation();
    }
    let due = s.record.active.as_ref().and_then(|a| a.stop_after_ms.map(|l| now.saturating_sub(a.started_ms) >= l)).unwrap_or(false);
    if due {
        let minutes = s.record.active.as_ref().map_or(0, |a| a.settings.stop_after_minutes);
        let ev = match stop(s, &json!({})) {
            Ok(v) => json!({"event": "stoppedAfter", "minutes": minutes, "result": v}),
            Err(e) => json!({"event": "stopFailed", "error": e.to_string()}),
        };
        s.record.last_event = Some(ev.clone());
        return Some(ev);
    }
    None
}

fn status_json(s: &Session) -> Value {
    let preview = crate::record_preview::preview_ids(s);
    let Some(a) = &s.record.active else {
        let mut v = json!({"recording": false, "sources": [], "preview": preview});
        if let (Some(o), Some(st)) = (v.as_object_mut(), &s.record.starting) {
            o.insert("starting".into(), json!(true));
            o.insert("label".into(), json!(st.label));
        }
        if let (Some(o), Some(c)) = (v.as_object_mut(), &s.record.countdown) {
            o.insert("countdown".into(), json!({"seconds": c.seconds, "remaining": s.record.countdown_left().unwrap_or(0.0)}));
        }
        if let (Some(o), Some(e)) = (v.as_object_mut(), &s.record.last_event) {
            o.insert("event".into(), e.clone());
        }
        return v;
    };
    let mut sources = Vec::new();
    let mut errors = Vec::new();
    for v in &a.video {
        sources.push(json!({
            "kind": v.src.kind.name(),
            "key": v.src.key(),
            "device": v.device_name,
            "frames": v.stats.frames.load(Ordering::Relaxed),
            "dropped": v.stats.dropped.load(Ordering::Relaxed),
            "bytes": v.stats.bytes.load(Ordering::Relaxed),
        }));
        if let Some(e) = v.input.error() {
            errors.push(format!("{}: {e}", v.src.key()));
        }
        if v.worker.as_ref().is_some_and(|w| w.is_finished()) {
            errors.push(format!("{}: the encoder stopped", v.src.key()));
        }
    }
    for m in &a.mics {
        sources.push(json!({
            "kind": m.src.kind.name(),
            "key": m.src.key(),
            "device": m.device,
            "frames": m.stats.frames.load(Ordering::Relaxed),
            "dropped": 0,
            "bytes": m.stats.bytes.load(Ordering::Relaxed),
            "level": f32::from_bits(m.stats.level.load(Ordering::Relaxed)),
        }));
        if m.worker.as_ref().is_some_and(|w| w.is_finished()) {
            errors.push(format!("{}: the microphone stopped", m.src.key()));
        }
    }
    if let Some(sa) = &a.system_audio {
        sources.push(json!({
            "kind": sa.src.kind.name(),
            "key": sa.src.key(),
            "device": "System Audio",
            "frames": sa.stats.frames.load(Ordering::Relaxed),
            "dropped": sa.stats.dropped.load(Ordering::Relaxed),
            "bytes": sa.stats.bytes.load(Ordering::Relaxed),
            "level": f32::from_bits(sa.stats.level.load(Ordering::Relaxed)),
        }));
    }
    json!({
        "recording": true,
        "name": a.name,
        "elapsed": a.clock.now_ns() as f64 / 1e9,
        "sources": sources,
        "stopAfterMinutes": a.settings.stop_after_minutes,
        "notes": a.notes,
        "error": (!errors.is_empty()).then(|| errors.join("; ")),
        "preview": preview,
    })
}

fn status(s: &mut Session, _p: &Value) -> Result<Value> {
    tick(s);
    Ok(status_json(s))
}

/// One source after stop.
struct Done {
    src: Src,
    path: PathBuf,
    device_id: String,
    device_name: String,
    mirror: bool,
    rotate: u32,
    rotate_setting: CameraRotate,
    camera_rotation: Option<u16>,
    rotation_changes: u32,
    rotation_changed_at_ns: Option<u64>,
    area: Option<[u32; 4]>,
    first_ns: u64,
    last_ns: u64,
    /// From the source's start to its first sample.
    warmup_ns: Option<u64>,
    dropped: u64,
    fin: Finished,
    notes: Vec<String>,
}

fn write_sidecar(name: &str, clock: &RecordClock, rs: &RecordingSettings, d: &Done, offset_ms: f64) -> std::io::Result<()> {
    let file = d.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let mut v = json!({
        "version": 1,
        "recording": name,
        "source": d.src.kind.name(),
        "index": d.src.n + 1,
        "file": file,
        "device": {"id": d.device_id, "name": d.device_name},
        "clock_start_ns": clock.unix_start_ns(),
        "first_sample_ns": d.first_ns,
        "last_sample_ns": d.last_ns,
        "warmup_ns": d.warmup_ns,
        "dropped": d.dropped,
        "bytes": d.fin.bytes,
        "events": [],
    });
    if let Some(o) = v.as_object_mut() {
        if d.src.kind.audio() {
            o.insert("sample_rate".into(), json!(d.fin.sample_rate));
            o.insert("requested_sample_rate".into(), json!(rs.sample_rate));
            o.insert("channels".into(), json!(d.fin.channels));
            o.insert("format".into(), json!(d.fin.format));
            o.insert("samples".into(), json!(d.fin.samples));
            if d.src.kind == SourceKind::Mic {
                o.insert("auto_gain".into(), json!(d.fin.auto_gain));
            }
        } else {
            o.insert("frames".into(), json!(d.fin.frames));
            o.insert("width".into(), json!(d.fin.width));
            o.insert("height".into(), json!(d.fin.height));
            o.insert("fps".into(), json!(d.fin.fps));
            o.insert("encoder".into(), json!(d.fin.encoder));
            o.insert("bitrate_kbps".into(), json!(d.fin.kbps));
            o.insert("quality".into(), json!(rs.quality));
            o.insert("keyframe_seconds".into(), json!(rs.keyframe_seconds));
            o.insert("hardware_encoder".into(), json!(rs.hardware_encoder));
        }
        if d.src.kind == SourceKind::Screen {
            o.insert("show_cursor".into(), json!(rs.show_cursor));
            o.insert("resolution".into(), json!(rs.screen_resolution));
            if let Some(a) = d.area {
                o.insert("area".into(), json!(a));
            }
        }
        let notes: Vec<&String> = d.notes.iter().chain(&d.fin.notes).collect();
        if !notes.is_empty() {
            o.insert("notes".into(), json!(notes));
        }
        if d.src.kind == SourceKind::Camera {
            o.insert("camera_offset_ms".into(), json!(offset_ms));
            o.insert("mirror".into(), json!(d.mirror));
            if d.mirror {
                o.insert("mirror_note".into(), json!("the file is as the camera saw it; the clip has a Horizontal Flip effect"));
            }
            o.insert("rotate".into(), json!(d.rotate));
            o.insert("rotate_setting".into(), json!(d.rotate_setting));
            o.insert("camera_rotation".into(), json!(d.camera_rotation));
            if let Some(at) = d.rotation_changed_at_ns {
                o.insert("rotation_changed_at_ns".into(), json!(at));
                o.insert("rotation_changes".into(), json!(d.rotation_changes));
                o.insert(
                    "rotation_note".into(),
                    json!("the camera turned during the recording: the file keeps the orientation it had at the start (a track header has one rotation)"),
                );
            }
            if d.rotate != 0 {
                o.insert(
                    "rotate_note".into(),
                    json!(format!("the file's track header turns it {}° clockwise (as iPhones do); the pictures are as the camera saw them and nothing is added to the clip", d.rotate)),
                );
            }
        }
    }
    let bytes = serde_json::to_vec_pretty(&v).map_err(std::io::Error::other)?;
    filmcraft_format::atomic_write(&sidecar_path(&d.path), &bytes)
}

/// Stop every source and collect what each produced (`Err`: that source's failure).
fn finish_all(s: &mut Session, mut a: Active) -> (Active, Vec<std::result::Result<Done, String>>) {
    // sample times are on the start clock
    a.watch_rotation();
    let stop = a.raw.now_ns();
    a.stop_ns.store(stop, Ordering::Release);
    let mut out = Vec::new();
    for v in &mut a.video {
        v.input.stop();
        v.queue.close();
        let input_error = v.input.error();
        let r = match v.worker.take().map(|w| w.join()) {
            Some(Ok(Ok(fin))) => Ok(fin),
            Some(Ok(Err(e))) => Err(e),
            Some(Err(p)) => Err(format!("the encoder crashed: {}", panic_text(&p))),
            None => Err("the encoder was not running".into()),
        };
        let r = match (r, input_error) {
            (Err(e), Some(ie)) => Err(format!("{e} ({ie})")),
            (r, _) => r,
        };
        out.push(r.map_err(|e| format!("{}: {e}", v.src.key())).map(|fin| Done {
            src: v.src,
            path: v.path.clone(),
            device_id: v.device_id.clone(),
            device_name: v.device_name.clone(),
            mirror: v.mirror,
            rotate: v.rotate,
            rotate_setting: v.rotate_setting,
            camera_rotation: v.camera_rotation,
            rotation_changes: v.rotation_changes,
            rotation_changed_at_ns: v.rotation_changed_at_ns,
            area: v.area,
            first_ns: v.stats.first().unwrap_or(0),
            last_ns: v.stats.last().unwrap_or(0),
            warmup_ns: v.stats.warmup(),
            dropped: v.stats.dropped.load(Ordering::Relaxed),
            fin,
            notes: v.notes.clone(),
        }));
    }
    for m in &mut a.mics {
        let r = match m.worker.take().map(|w| w.join()) {
            Some(Ok((input, r))) => {
                if m.session_input {
                    s.voiceover.input = Some(input);
                }
                r
            }
            Some(Err(p)) => Err(format!("the microphone recorder crashed: {}", panic_text(&p))),
            None => Err("the microphone was not running".into()),
        };
        out.push(r.map_err(|e| format!("{}: {e}", m.src.key())).map(|fin| Done {
            src: m.src,
            path: m.path.clone(),
            device_id: m.device.clone(),
            device_name: m.device.clone(),
            mirror: false,
            rotate: 0,
            rotate_setting: CameraRotate::Fixed(0),
            camera_rotation: None,
            rotation_changes: 0,
            rotation_changed_at_ns: None,
            area: None,
            first_ns: m.stats.first().unwrap_or(0),
            last_ns: m.stats.last().unwrap_or(0),
            warmup_ns: m.stats.warmup(),
            dropped: 0,
            notes: a.notes.iter().filter_map(|n| n.strip_prefix(&format!("{}: ", m.src.key())).map(str::to_string)).collect(),
            fin,
        }));
    }
    if let Some(sa) = a.system_audio.as_mut() {
        // the screen input is stopped: no block arrives any more
        sa.queue.close();
        let r = match sa.worker.take().map(|w| w.join()) {
            Some(Ok(r)) => r,
            Some(Err(p)) => Err(format!("the system audio recorder crashed: {}", panic_text(&p))),
            None => Err("the system audio was not running".into()),
        };
        out.push(r.map_err(|e| format!("{}: {e}", sa.src.key())).map(|fin| Done {
            src: sa.src,
            path: sa.path.clone(),
            device_id: "system".into(),
            device_name: "System Audio".into(),
            mirror: false,
            rotate: 0,
            rotate_setting: CameraRotate::Fixed(0),
            camera_rotation: None,
            rotation_changes: 0,
            rotation_changed_at_ns: None,
            area: None,
            first_ns: sa.stats.first().unwrap_or(0),
            last_ns: sa.stats.last().unwrap_or(0),
            warmup_ns: sa.stats.warmup(),
            dropped: sa.stats.dropped.load(Ordering::Relaxed),
            fin,
            notes: Vec::new(),
        }));
    }
    (a, out)
}

/// Where each source starts in the new sequence, in ns (earliest = 0): first sample times, every
/// camera moved by `camera_offset_ms`.
pub fn sync_offsets(firsts: &[(SourceKind, u64)], camera_offset_ms: f64) -> Vec<(SourceKind, i64)> {
    let shift = (camera_offset_ms * 1e6).round() as i64;
    let shifted: Vec<(u64, i64)> = firsts.iter().map(|(k, f)| (*f, if *k == SourceKind::Camera { shift } else { 0 })).collect();
    firsts.iter().map(|f| f.0).zip(sync_offsets_by(&shifted)).collect()
}

/// Where each source starts in the new sequence, in ns (earliest = 0), from its first sample
/// time and a shift in ns (a camera offset).
pub fn sync_offsets_by(firsts: &[(u64, i64)]) -> Vec<i64> {
    let raw: Vec<i64> = firsts.iter().map(|(f, shift)| i64::try_from(*f).unwrap_or(i64::MAX / 4).saturating_add(*shift)).collect();
    let min = raw.iter().copied().min().unwrap_or(0);
    raw.into_iter().map(|v| v.saturating_sub(min)).collect()
}

/// A clip of `item` whose media time 0 sits at sequence time `exact`: frame-aligned at or after
/// it, with the source In moved by the difference (Merge Clips' placement).
fn placed(
    pr: &mut filmcraft_project::Project,
    item: ItemId,
    kind: TrackKind,
    exact: Tick,
    settings: &SequenceSettings,
) -> Option<filmcraft_project::TrackItem> {
    let rate = settings.frame_rate;
    let snapped = rate.snap(exact);
    let start = if snapped < exact { snapped + rate.frame_duration() } else { snapped };
    let shift = start - exact;
    let (dur, size) =
        pr.resolve_media(item).map(|(_, m, _)| (m.info.duration, m.info.video.as_ref().map(|v| (v.width, v.height)))).unwrap_or((Tick::ZERO, None));
    if dur <= shift {
        return None;
    }
    let src = TimeRange::from_bounds(shift, dur);
    let mut ti = pr.make_track_item(item, kind, start, src, rate)?;
    let frames = rate.frame_at(src.duration).max(1);
    ti.duration = rate.tick_of(frames);
    if kind == TrackKind::Video {
        let size = size.unwrap_or((settings.width, settings.height));
        for e in &mut ti.effects {
            filmcraft_project::resolve_auto_points(e, (settings.width, settings.height), size);
        }
    }
    Some(ti)
}

fn offset_ms_of(v: &Value, cmd: &str, key: &str) -> Result<f64> {
    let o = v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| bad(cmd, format!("`{key}` must be a number")))?;
    if !(-5000.0..=5000.0).contains(&o) {
        return Err(bad(cmd, format!("`{key}` must be −5000…5000, got {o}")));
    }
    Ok(o)
}

/// The camera offsets of `record.stop`: `cameraOffsetsMs` (one per camera, in order) else the
/// scalar `cameraOffsetMs` for every camera.
fn camera_offsets(p: &Value, cmd: &str) -> Result<(f64, Vec<f64>)> {
    let scalar = match p.get("cameraOffsetMs").filter(|v| !v.is_null()) {
        None => 0.0,
        Some(v) => offset_ms_of(v, cmd, "cameraOffsetMs")?,
    };
    let list = match p.get("cameraOffsetsMs").filter(|v| !v.is_null()) {
        None => Vec::new(),
        Some(Value::Array(a)) if a.len() <= MAX_PER_KIND => a.iter().map(|x| offset_ms_of(x, cmd, "cameraOffsetsMs")).collect::<Result<Vec<f64>>>()?,
        Some(_) => return Err(bad(cmd, format!("`cameraOffsetsMs` must be a list of at most {MAX_PER_KIND} numbers"))),
    };
    Ok((scalar, list))
}

fn stop(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "record.stop";
    let discard = bool_p(p, "discard").unwrap_or(false);
    let (scalar, list) = camera_offsets(p, cmd)?;
    if s.record.active.is_none() && s.record.countdown.take().is_some() {
        return Ok(json!({"recording": false, "placed": false, "cancelled": true}));
    }
    if s.record.active.is_none()
        && let Some(v) = cancel_start(s)
    {
        return Ok(v);
    }
    let a = s.record.active.take().ok_or_else(|| bad(cmd, "nothing is recording"))?;
    let (a, results) = finish_all(s, a);
    let files = a.files();
    if discard {
        remove_files(&files);
        return Ok(json!({"recording": false, "placed": false, "discarded": true}));
    }
    let mut errors = Vec::new();
    let mut done = Vec::new();
    for r in results {
        match r {
            Ok(d) => done.push(d),
            Err(e) => errors.push(e),
        }
    }
    // a failed source leaves no half file behind
    for (k, f) in &files {
        if !done.iter().any(|d| d.src == *k) {
            let _ = std::fs::remove_file(f);
        }
    }
    if done.is_empty() {
        return Err(EngineError::Other(format!("recording failed: {}", errors.join("; "))));
    }
    let offset_of = |src: Src| if src.kind == SourceKind::Camera { list.get(src.n).copied().unwrap_or(scalar) } else { 0.0 };
    for d in &done {
        if let Err(e) = write_sidecar(&a.name, &a.clock, &a.settings, d, offset_of(d.src)) {
            errors.push(format!("{}: sidecar: {e}", d.src.key()));
        }
    }
    let offsets_ms: Vec<f64> = done.iter().map(|d| offset_of(d.src)).collect();
    // import + sequence, one undo step
    let n0 = s.history.undo.len();
    let r = import_and_place(s, &a.name, &done, &offsets_ms, a.settings.open_sequence);
    crate::clip_ops::collapse_history(s, n0, "Record");
    let mut v = r?;
    if let Some(o) = v.as_object_mut() {
        o.insert("files".into(), json!(done.iter().map(|d| d.path.to_string_lossy().into_owned()).collect::<Vec<_>>()));
        o.insert("errors".into(), json!(errors));
        o.insert("notes".into(), json!(a.notes));
        // Auto cameras that turned while recording (the files keep the start's orientation)
        let turned: Vec<Value> =
            done.iter().filter_map(|d| d.rotation_changed_at_ns.map(|at| json!({"source": d.src.key(), "atNs": at, "rotate": d.rotate}))).collect();
        o.insert("rotationChanged".into(), json!(turned));
    }
    Ok(v)
}

/// Import the finished files and build the synced sequence: the screen on V1, the cameras on the
/// next video tracks in order, the microphones on A1…, the system audio after them
/// (`offsets_ms[i]`: the shift of `done[i]`). `open`: make it the active sequence.
fn import_and_place(s: &mut Session, name: &str, done: &[Done], offsets_ms: &[f64], open: bool) -> Result<Value> {
    let bin = s.edit("New Bin", |pr, _| {
        let existing = pr.root.children.iter().find_map(|c| match c {
            filmcraft_project::BinEntry::Bin(b) if b.name == "Recordings" => Some(b.id),
            _ => None,
        });
        Ok(existing.unwrap_or_else(|| pr.add_bin("Recordings", None)))
    })?;
    let mut items: Vec<(Src, ItemId)> = Vec::new();
    for d in done {
        let item = crate::commands::import_streamed(s, &d.path.to_string_lossy(), Some(bin))?;
        items.push((d.src, item));
    }
    // media time 0 of every file is the recording's start: only the camera offsets move a clip
    let firsts: Vec<(u64, i64)> = done.iter().zip(offsets_ms).map(|(_, o)| (0, (o * 1e6).round() as i64)).collect();
    let offsets: Vec<(Src, i64)> = done.iter().map(|d| d.src).zip(sync_offsets_by(&firsts)).collect();
    let off = |k: Src| offsets.iter().find(|o| o.0 == k).map(|o| Tick::from_units(o.1, 1_000_000_000));
    let item = |k: Src| items.iter().find(|i| i.0 == k).map(|i| i.1);
    let of_kind = |kind: SourceKind| -> Vec<Src> { items.iter().map(|i| i.0).filter(|k| k.kind == kind).collect() };
    let videos: Vec<Src> = of_kind(SourceKind::Screen).into_iter().chain(of_kind(SourceKind::Camera)).collect();
    let audios: Vec<Src> = of_kind(SourceKind::Mic).into_iter().chain(of_kind(SourceKind::SystemAudio)).collect();
    // sequence settings: the screen's picture, else the first camera's; the first mic's rate
    let lead = videos.first().or(audios.first()).and_then(|k| item(*k));
    let info = |it: Option<ItemId>| it.and_then(|i| s.project.item(i)).and_then(|i| i.as_media()).map(|m| m.info.clone());
    let mut settings = info(lead).map(|i| crate::commands::default_seq_settings_for(&i)).unwrap_or_default();
    // a turned camera's file is upright by itself (its track header): a camera on its side that
    // leads (no screen) gives an upright sequence like any portrait clip
    if let Some(a) = info(audios.first().and_then(|k| item(*k))).and_then(|i| i.audio) {
        settings.sample_rate = a.sample_rate.max(8000);
    }
    let mirrored: Vec<Src> = done.iter().filter(|d| d.mirror).map(|d| d.src).collect();
    let scaling = s.prefs.media.default_media_scaling.clone();
    let comment = {
        let cams: Vec<String> = done
            .iter()
            .zip(offsets_ms)
            .filter(|(d, o)| d.src.kind == SourceKind::Camera && **o != 0.0)
            .map(|(d, o)| if d.src.n == 0 { format!("camera offset {o} ms") } else { format!("camera {} offset {o} ms", d.src.n + 1) })
            .collect();
        cams.join(", ")
    };
    let name_s = name.to_string();
    let seq = s.edit("Record", |pr, st| {
        for (k, it) in &items {
            if let Some(pi) = pr.item_mut(*it) {
                pi.metadata.insert("Recording".into(), name_s.clone());
                pi.metadata.insert("Recording Source".into(), k.key());
                if let Some(o) = offsets.iter().find(|o| o.0 == *k) {
                    pi.metadata.insert("Recording Offset".into(), format!("{} ns", o.1));
                }
            }
        }
        let sid = pr.new_sequence(&name_s, settings.clone(), videos.len().max(1), audios.len().max(1), Some(bin));
        let mut clips = Vec::new();
        let mut v_items = Vec::new();
        for k in &videos {
            if let (Some(it), Some(at)) = (item(*k), off(*k)) {
                let mut ti = placed(pr, it, TrackKind::Video, at, &settings)
                    .ok_or_else(|| EngineError::Other(format!("the {} recording is too short to place", k.key())))?;
                if mirrored.contains(k)
                    && let Some(flip) = filmcraft_project::find_effect("horizontal_flip").map(|d| d.instance())
                {
                    ti.effects.insert(0, flip);
                }
                // fitted like any clip of another size (Settings ▸ Media ▸ Default Media Scaling);
                // and whatever that setting, a camera of another aspect (a portrait camera next
                // to a 16:9 screen) is scaled to the frame rather than cropped: a recording must
                // come in whole, and the layouts take Scale to Frame into account
                let frame = (settings.width, settings.height);
                if let Some(size) = pr.resolve_media(it).and_then(|(_, m, _)| m.info.video.as_ref().map(|v| (v.width, v.height)))
                    && size != frame
                {
                    crate::settings::apply_media_scaling(&mut ti, &scaling, frame, size);
                    let aspect_differs = size.1 > 0 && frame.1 > 0 && ((size.0 as f64 / size.1 as f64) - (frame.0 as f64 / frame.1 as f64)).abs() > 0.01;
                    if aspect_differs && !matches!(scaling.as_str(), "setToFrameSize") {
                        ti.scale_to_frame = true;
                    }
                }
                clips.push((*k, ti.id, ti.start));
                v_items.push(ti);
            }
        }
        let mut a_items = Vec::new();
        for k in &audios {
            if let (Some(it), Some(at)) = (item(*k), off(*k)) {
                let ti = placed(pr, it, TrackKind::Audio, at, &settings)
                    .ok_or_else(|| EngineError::Other(format!("the {} recording is too short to place", k.key())))?;
                clips.push((*k, ti.id, ti.start));
                a_items.push(ti);
            }
        }
        let marker = MarkerId(pr.alloc_id());
        let q = pr.sequence_mut(sid).ok_or(EngineError::NoSequence)?;
        for (t, it) in q.video_tracks.iter_mut().zip(v_items) {
            t.items.push(it);
        }
        for (t, it) in q.audio_tracks.iter_mut().zip(a_items) {
            t.items.push(it);
        }
        q.markers.push(Marker {
            id: marker,
            start: Tick::ZERO,
            duration: Tick::ZERO,
            name: "Recording".into(),
            comment: comment.clone(),
            kind: MarkerKind::Comment,
            color: Label::Rose,
        });
        q.check().map_err(EngineError::Other)?;
        if let Some(it) = pr.item_mut(sid) {
            it.label = Label::Rose;
        }
        if open {
            st.active_sequence = Some(sid);
            if !st.open_sequences.contains(&sid) {
                st.open_sequences.push(sid);
            }
        }
        st.project_selection = vec![sid];
        Ok((sid, clips))
    })?;
    let (sid, clips) = seq;
    let obj = |f: &dyn Fn(Src) -> Option<Value>| {
        let mut m = serde_json::Map::new();
        for (k, _) in &items {
            if let Some(v) = f(*k) {
                m.insert(k.key(), v);
            }
        }
        Value::Object(m)
    };
    let keys = |kind: SourceKind| -> Vec<String> { items.iter().filter(|i| i.0.kind == kind).map(|i| i.0.key()).collect() };
    let cam_offsets: Vec<f64> = done.iter().zip(offsets_ms).filter(|(d, _)| d.src.kind == SourceKind::Camera).map(|(_, o)| *o).collect();
    Ok(json!({
        "recording": false,
        "placed": true,
        "name": name,
        "sequence": sid.0,
        "opened": open,
        "items": obj(&|k| item(k).map(|i| json!(i.0))),
        "clips": obj(&|k| clips.iter().find(|c| c.0 == k).map(|c| json!({"clip": c.1.0, "start": c.2.0}))),
        "offsets": obj(&|k| off(k).map(|t| json!(t.0))),
        "cameras": keys(SourceKind::Camera),
        "mics": keys(SourceKind::Mic),
        "cameraOffsetMs": cam_offsets.first().copied().unwrap_or(0.0),
        "cameraOffsetsMs": cam_offsets,
    }))
}

fn cancel(s: &mut Session, _p: &Value) -> Result<Value> {
    if s.record.active.is_none() && s.record.countdown.take().is_some() {
        return Ok(json!({"recording": false, "placed": false, "discarded": true, "cancelled": true}));
    }
    if s.record.active.is_none()
        && let Some(v) = cancel_start(s)
    {
        return Ok(v);
    }
    stop(s, &json!({"discard": true}))
}

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: fn(&mut Session, &Value) -> Result<Value>,
    journal: bool,
) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("record.devices", "Recording Devices", "{}", crate::commands::always, devices, false),
        spec(
            "record.start",
            "Start Recording",
            r#"{"screen":{"display":id,"area":[x,y,w,h]?}|{"window":id},"fps":n?,"resolution":"native|1440p|1080p|720p"?,"cursor":bool?,"systemAudio":bool?}?,"cameras":[{"device":id,"quality":"720p|1080p|4k|native"?,"width":n?,"height":n?,"fps":n?,"mirror":bool?,"rotate":"auto"|0|90|180|270?}]?,"camera":{..}?,"mics":[{"device":str?}]?,"mic":{..}?,"name":str?,"dir":str?,"countdown":0..10?,"settings":RecordingSettings?}"#,
            can_start,
            start,
            true,
        ),
        spec("record.status", "Recording Status", "{}", crate::commands::always, status, false),
        spec("record.stop", "Stop Recording", r#"{"discard":bool?,"cameraOffsetMs":f64?,"cameraOffsetsMs":[f64]?}"#, is_recording, stop, true),
        spec("record.cancel", "Cancel Recording", "{}", is_recording, cancel, true),
        crate::record_preview::preview_spec(),
        spec("record.settings", "Recording Settings", r#"{"get":bool?}|{"set":{field:value}}"#, crate::commands::always, crate::record_settings::command, true),
    ]
}

/// `record.status` for hosts (the status bar, the Record panel) without going through a command.
pub fn status_of(s: &Session) -> Value {
    status_json(s)
}
