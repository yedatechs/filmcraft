//! Recording inside the app (Window ▸ Record): screen (a display or a window), camera and
//! microphone at once, **each source to its own file**, all stamped on one clock, then imported
//! and placed in sync in a new sequence (screen V1, camera V2, microphone A1) as one undo step.
//! Design: `openspec/changes/recording/design.md`; user docs: `docs/recording.md`.
//!
//! | command | does |
//! |---|---|
//! | `record.devices` | displays, windows, cameras, microphones and the permission states |
//! | `record.start` | start recording the chosen sources; returns the files it writes and the clock start |
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
use crate::voiceover::{AudioInput, SyntheticInput};
use crate::{EngineError, Result, Session};

// ------------------------------------------------------------------------------------- clock

/// The one clock of a recording: a monotonic instant taken at `record.start`, plus the wall-clock
/// time at that instant (`clock_start_ns` in the sidecars). Sample times are nanoseconds since it.
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
}

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
}

impl Default for SyntheticFactory {
    fn default() -> Self {
        Self { display_size: (1280, 720), camera_size: (640, 360), screen_delay_ms: 0, camera_delay_ms: 0, camera2_delay_ms: 0 }
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
    fn open_screen(&self, target: &ScreenTarget) -> std::result::Result<Box<dyn VideoInput>, CaptureError> {
        match target {
            ScreenTarget::Display(id) if id == Self::DISPLAY => Ok(Box::new(SyntheticVideoInput::new(self.display_size, self.screen_delay_ms))),
            ScreenTarget::Window(id) if id == Self::WINDOW => Ok(Box::new(SyntheticVideoInput::new((960, 540), self.screen_delay_ms))),
            ScreenTarget::Display(id) => Err(CaptureError::new(CaptureErrorKind::NoDevice, format!("no display `{id}`"))),
            ScreenTarget::Window(id) => Err(CaptureError::new(CaptureErrorKind::NoDevice, format!("no window `{id}`"))),
        }
    }
    fn open_camera(&self, device: &str) -> std::result::Result<Box<dyn VideoInput>, CaptureError> {
        if device == Self::CAMERA {
            Ok(Box::new(SyntheticVideoInput::new(self.camera_size, self.camera_delay_ms)))
        } else if device == Self::CAMERA2 {
            Ok(Box::new(SyntheticVideoInput::new(self.camera_size, self.camera2_delay_ms)))
        } else {
            Err(CaptureError::new(CaptureErrorKind::NoDevice, format!("no camera `{device}`")))
        }
    }
}

/// Blocks of the burnt-in frame index (top row of a synthetic frame, most significant bit first).
pub const INDEX_BITS: u32 = 16;

/// A generated picture source: mid grey, a white bar moving with time, and the frame index burnt
/// into the top eighth as [`INDEX_BITS`] black / white blocks.
pub struct SyntheticVideoInput {
    native: (u32, u32),
    delay_ms: u64,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    error: Arc<Mutex<Option<String>>>,
}

impl SyntheticVideoInput {
    pub fn new(native: (u32, u32), delay_ms: u64) -> Self {
        Self { native, delay_ms, stop: Arc::new(AtomicBool::new(false)), thread: None, error: Arc::new(Mutex::new(None)) }
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
        let w = req.width.unwrap_or(self.native.0).clamp(16, 8192) & !1;
        let h = req.height.unwrap_or(self.native.1).clamp(16, 8192) & !1;
        let fps = req.fps.clamp(1, 60);
        let first = clock.now_ns().saturating_add(self.delay_ms.saturating_mul(1_000_000));
        let period = 1_000_000_000 / u64::from(fps);
        let stop = self.stop.clone();
        let error = self.error.clone();
        let thread = std::thread::Builder::new()
            .name("filmcraft-synthetic-video".into())
            .spawn(move || {
                let run = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let mut k = 0u64;
                    while !stop.load(Ordering::Acquire) {
                        let t = first.saturating_add(k.saturating_mul(period));
                        let now = clock.now_ns();
                        if now < t {
                            std::thread::sleep(std::time::Duration::from_nanos((t - now).min(20_000_000)));
                            continue;
                        }
                        let data = synthetic_frame(w, h, k, t);
                        sink(CapturedFrame { width: w, height: h, stride: w as usize * 4, format: PixelFormat::Rgba8, data, time_ns: t });
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
pub struct FrameQueue {
    q: Mutex<(VecDeque<CapturedFrame>, bool)>,
    cv: Condvar,
    cap: usize,
}

impl FrameQueue {
    pub fn new(cap: usize) -> Self {
        Self { q: Mutex::new((VecDeque::new(), false)), cv: Condvar::new(), cap: cap.max(1) }
    }
    /// Add a frame; returns whether an older one was dropped to make room. Never blocks on the
    /// consumer. Frames pushed after [`Self::close`] are dropped.
    pub fn push(&self, f: CapturedFrame) -> bool {
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
    pub fn pop(&self, timeout: std::time::Duration) -> Option<Option<CapturedFrame>> {
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
}

impl SourceKind {
    pub fn name(self) -> &'static str {
        match self {
            SourceKind::Screen => "screen",
            SourceKind::Camera => "camera",
            SourceKind::Mic => "mic",
        }
    }
    fn file_label(self) -> &'static str {
        match self {
            SourceKind::Screen => "Screen",
            SourceKind::Camera => "Camera",
            SourceKind::Mic => "Mic",
        }
    }
    fn extension(self) -> &'static str {
        if self == SourceKind::Mic { "wav" } else { "mov" }
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
    /// First / last sample time on the recording clock (`u64::MAX` = none yet).
    pub first_ns: AtomicU64,
    pub last_ns: AtomicU64,
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
}

type Worker = std::thread::JoinHandle<std::result::Result<Finished, String>>;

struct VideoSource {
    src: Src,
    device_id: String,
    device_name: String,
    /// Flip the clip horizontally in the sequence (a camera seen as in a mirror).
    mirror: bool,
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

/// A recording in progress.
pub struct Active {
    pub name: String,
    pub dir: PathBuf,
    pub clock: RecordClock,
    video: Vec<VideoSource>,
    mics: Vec<MicSource>,
    /// Set at stop: the time up to which every source records.
    stop_ns: Arc<AtomicU64>,
    microphones: Vec<String>,
}

impl Active {
    fn files(&self) -> Vec<(Src, PathBuf)> {
        let mut v: Vec<(Src, PathBuf)> = self.video.iter().map(|s| (s.src, s.path.clone())).collect();
        v.extend(self.mics.iter().map(|m| (m.src, m.path.clone())));
        v
    }
}

/// Recording state of a session.
#[derive(Default)]
pub struct Recorder {
    /// Overrides the registered capture factory (tests, the desktop app on systems without one).
    pub factory: Option<Arc<dyn VideoInputFactory>>,
    pub active: Option<Active>,
}

impl Recorder {
    pub fn recording(&self) -> bool {
        self.active.is_some()
    }
}

fn factory(s: &Session) -> Arc<dyn VideoInputFactory> {
    s.record.factory.clone().or_else(registered_video_factory).unwrap_or_else(|| Arc::new(SyntheticFactory::default()))
}

// ------------------------------------------------------------------------------------- workers

/// Copy a captured frame into a straight RGBA picture of `w × h` (cropped or padded with black).
fn to_rgba(f: &CapturedFrame, w: u32, h: u32, out: &mut Vec<u8>) -> bool {
    let (fw, fh) = (f.width as usize, f.height as usize);
    let need = f.stride.checked_mul(fh.saturating_sub(1)).and_then(|n| n.checked_add(fw.saturating_mul(4)));
    if fw == 0 || fh == 0 || f.stride < fw.saturating_mul(4) || need.is_none_or(|n| f.data.len() < n) {
        return false;
    }
    let (w, h) = (w as usize, h as usize);
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

fn video_worker(
    mut rec: filmcraft_export::recorder::MovRecorder,
    queue: Arc<FrameQueue>,
    stats: Arc<SourceStats>,
    stop_ns: Arc<AtomicU64>,
    rate: FrameRate,
) -> std::result::Result<Finished, String> {
    let (w, h) = rec.size();
    let mut rgba = Vec::new();
    let mut first = None;
    loop {
        let f = match queue.pop(std::time::Duration::from_millis(100)) {
            None => break,
            Some(None) => continue,
            Some(Some(f)) => f,
        };
        let t0 = *first.get_or_insert(f.time_ns);
        let slot = slot_of(f.time_ns.saturating_sub(t0), rate);
        if rec.last_slot().is_some_and(|l| slot <= l) || f.time_ns < t0 {
            continue; // two frames inside one frame period
        }
        if !to_rgba(&f, w, h, &mut rgba) {
            stats.dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        rec.push(&rgba, slot).map_err(|e| e.to_string())?;
        if stats.first().is_none() {
            stats.first_ns.store(t0, Ordering::Release);
        }
        stats.last_ns.store(f.time_ns, Ordering::Release);
        stats.frames.fetch_add(1, Ordering::Relaxed);
        stats.bytes.store(rec.bytes(), Ordering::Relaxed);
    }
    let encoder = rec.encoder_name().to_string();
    let frames = rec.frames();
    let Some(t0) = first.filter(|_| frames > 0) else {
        return Err("no frames were captured".into());
    };
    let end = stop_ns.load(Ordering::Acquire);
    let last_slot = rec.last_slot().unwrap_or(0);
    let end_slot = if end == NONE { last_slot + 1 } else { slot_of(end.saturating_sub(t0), rate).max(last_slot + 1) };
    let bytes = rec.finish(end_slot).map_err(|e| e.to_string())?;
    let fps = (rate.num.max(1) / rate.den.max(1)) as u32;
    Ok(Finished { frames, bytes, encoder, width: w, height: h, fps, ..Default::default() })
}

/// A 32-bit float mono WAV written as it is recorded (sizes patched by [`WavStream::finish`]).
struct WavStream {
    w: std::io::BufWriter<std::fs::File>,
    samples: u64,
    rate: u32,
}

/// Samples that fit a WAV's 32-bit data size.
const WAV_MAX_SAMPLES: u64 = (u32::MAX as u64 - 64) / 4;

impl WavStream {
    fn create(path: &Path, rate: u32) -> std::io::Result<Self> {
        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
        w.write_all(&crate::voiceover::write_wav_f32(&[], rate))?;
        Ok(Self { w, samples: 0, rate })
    }
    fn write(&mut self, s: &[f32]) -> std::result::Result<(), String> {
        if self.samples.saturating_add(s.len() as u64) > WAV_MAX_SAMPLES {
            return Err("the microphone file reached the 4 GB WAV limit".into());
        }
        let mut b = Vec::with_capacity(s.len() * 4);
        for v in s {
            b.extend_from_slice(&v.to_le_bytes());
        }
        self.w.write_all(&b).map_err(|e| e.to_string())?;
        self.samples += s.len() as u64;
        Ok(())
    }
    fn finish(mut self) -> std::io::Result<u64> {
        let data = (self.samples * 4) as u32;
        self.w.flush()?;
        let mut f = self.w.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(4))?;
        f.write_all(&(36u32.saturating_add(data)).to_le_bytes())?;
        f.seek(SeekFrom::Start(40))?;
        f.write_all(&data.to_le_bytes())?;
        f.sync_all()?;
        let _ = self.rate;
        Ok(44 + u64::from(data))
    }
}

fn mic_worker(
    input: &mut Box<dyn AudioInput>,
    mut wav: WavStream,
    channel: usize,
    clock: RecordClock,
    first_ns: u64,
    stats: &SourceStats,
    stop_ns: &AtomicU64,
) -> std::result::Result<Finished, String> {
    let rate = u64::from(wav.rate.max(1));
    let due = |ns: u64| -> u64 { (u128::from(ns.saturating_sub(first_ns)) * u128::from(rate) / 1_000_000_000).min(u128::from(u64::MAX)) as u64 };
    loop {
        let end = stop_ns.load(Ordering::Acquire);
        let now = if end == NONE { clock.now_ns() } else { end };
        let want = due(now).saturating_sub(wav.samples);
        if want > 0 {
            let got = input.read(want.min(rate * 2) as usize);
            if let Some(e) = input.error() {
                return Err(format!("microphone failed: {e}"));
            }
            if let Some(c) = got.get(channel).or(got.first()) {
                wav.write(c)?;
                let peak = c.iter().fold(0f32, |m, v| m.max(v.abs()));
                stats.level.store(peak.to_bits(), Ordering::Relaxed);
                stats.bytes.store(44 + wav.samples * 4, Ordering::Relaxed);
                stats.frames.store(wav.samples, Ordering::Relaxed);
                if wav.samples > 0 {
                    stats.last_ns.store(first_ns.saturating_add(wav.samples * 1_000_000_000 / rate), Ordering::Release);
                }
            }
        }
        if end != NONE {
            // a live device may still hold the last few milliseconds: take what it has, once
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    input.stop();
    let samples = wav.samples;
    let sample_rate = wav.rate;
    let bytes = wav.finish().map_err(|e| e.to_string())?;
    if samples == 0 {
        return Err("no audio was captured".into());
    }
    Ok(Finished { frames: samples, bytes, sample_rate, samples, ..Default::default() })
}

// ------------------------------------------------------------------------------------- files

/// Folder recordings are written to (the voice-over rule, with `Recordings`).
fn record_dir(s: &Session, p: &Value) -> PathBuf {
    if let Some(d) = str_p(p, "dir").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
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
    if s.voiceover.recording() {
        return Err("a voice-over is recording".into());
    }
    Ok(())
}

fn is_recording(s: &Session) -> std::result::Result<(), String> {
    if s.record.recording() { Ok(()) } else { Err("nothing is recording".into()) }
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
    let microphones = match &s.record.active {
        Some(a) => a.microphones.clone(),
        None => s.voiceover.input.get_or_insert_with(|| Box::new(SyntheticInput::clicks())).devices(),
    };
    Ok(json!({
        "displays": dev.displays,
        "windows": dev.windows,
        "cameras": dev.cameras,
        "microphones": microphones,
        "factory": f.name(),
        "permissions": f.permissions(),
        "error": error,
    }))
}

fn fps_of(v: &Value, cmd: &str, default: u32) -> Result<u32> {
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

fn size_of(v: &Value, key: &str, cmd: &str) -> Result<Option<u32>> {
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

fn id_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str().map(str::to_string).or_else(|| x.as_u64().map(|n| n.to_string())))
}

/// One camera of a recording.
struct CameraPlan {
    device: String,
    req: VideoRequest,
    mirror: bool,
}

struct Plan {
    screen: Option<(ScreenTarget, u32)>,
    cameras: Vec<CameraPlan>,
    /// Device names ("" = the system default).
    mics: Vec<String>,
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
    let screen = match p.get("screen").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            let target = match (id_of(v, "display"), id_of(v, "window")) {
                (Some(d), None) => ScreenTarget::Display(d),
                (None, Some(w)) => ScreenTarget::Window(w),
                _ => return Err(bad(cmd, "`screen` takes {display: id} or {window: id}")),
            };
            Some((target, fps_of(v, cmd, 30)?))
        }
    };
    let mut cameras: Vec<CameraPlan> = Vec::new();
    for v in list_p(p, "cameras", "camera", cmd)? {
        let device = id_of(v, "device").ok_or_else(|| bad(cmd, "a camera takes {device: id}"))?;
        if cameras.iter().any(|c| c.device == device) {
            return Err(bad(cmd, format!("the camera `{device}` is chosen twice")));
        }
        let mirror = match v.get("mirror").filter(|x| !x.is_null()) {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(bad(cmd, "`mirror` must be true or false")),
        };
        let req = VideoRequest { width: size_of(v, "width", cmd)?, height: size_of(v, "height", cmd)?, fps: fps_of(v, cmd, 30)? };
        cameras.push(CameraPlan { device, req, mirror });
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
    Ok(Plan { screen, cameras, mics })
}

/// Stop every started source of a failed start and delete its files; the session's mic input
/// goes back.
fn abort(s: &mut Session, mut a: Active) {
    a.stop_ns.store(0, Ordering::Release);
    for v in &mut a.video {
        v.input.stop();
        v.queue.close();
        if let Some(w) = v.worker.take() {
            let _ = w.join();
        }
    }
    for m in &mut a.mics {
        if let Some(w) = m.worker.take()
            && let Ok((input, _)) = w.join()
            && m.session_input
        {
            s.voiceover.input = Some(input);
        }
    }
    remove_files(&a.files());
}

/// What a video source is and where it goes.
struct VideoSetup {
    src: Src,
    req: VideoRequest,
    path: PathBuf,
    device_id: String,
    device_name: String,
    mirror: bool,
}

fn start_video(mut input: Box<dyn VideoInput>, setup: VideoSetup, clock: RecordClock, stop_ns: &Arc<AtomicU64>) -> Result<VideoSource> {
    let VideoSetup { src, req, path, device_id, device_name, mirror } = setup;
    let queue = Arc::new(FrameQueue::new(4));
    let stats = Arc::new(SourceStats::default());
    let (q, st) = (queue.clone(), stats.clone());
    let sink: FrameSink = Arc::new(move |f| {
        if q.push(f) {
            st.dropped.fetch_add(1, Ordering::Relaxed);
        }
    });
    let label = src.key();
    let fmt = input.start(&req, clock, sink).map_err(|e| EngineError::Other(format!("{label}: {e}")))?;
    let (w, h) = (fmt.width.clamp(16, 8192) & !1, fmt.height.clamp(16, 8192) & !1);
    let mut fps = fmt.fps.clamp(1, 60);
    let mut rec = filmcraft_export::recorder::MovRecorder::create(&path, w, h, FrameRate::new(i64::from(fps), 1), true);
    // our own encoder cannot do more than about 15 fps above 1080p: record at that rate
    if rec.as_ref().is_ok_and(|r| !r.hardware()) && u64::from(w) * u64::from(h) > 1920 * 1080 && fps > 15 {
        fps = 15;
        rec = filmcraft_export::recorder::MovRecorder::create(&path, w, h, FrameRate::new(15, 1), false);
    }
    let rec = match rec {
        Ok(r) => r,
        Err(e) => {
            input.stop();
            let _ = std::fs::remove_file(&path);
            return Err(EngineError::Other(format!("{label} encoder: {e}")));
        }
    };
    let (q, st, stop) = (queue.clone(), stats.clone(), stop_ns.clone());
    let rate = FrameRate::new(i64::from(fps), 1);
    let worker = std::thread::Builder::new()
        .name(format!("filmcraft-record-{label}"))
        .spawn(move || {
            std::panic::catch_unwind(AssertUnwindSafe(|| video_worker(rec, q, st, stop, rate)))
                .unwrap_or_else(|p| Err(format!("the encoder crashed: {}", panic_text(&p))))
        })
        .map_err(|e| EngineError::Other(format!("{label}: cannot start the encoder thread: {e}")));
    let worker = match worker {
        Ok(w) => w,
        Err(e) => {
            input.stop();
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    Ok(VideoSource { src, device_id, device_name, mirror, input, queue, stats, worker: Some(worker), path })
}

/// Start the microphone `device` into `path` (`input` moves onto the recording thread).
fn start_mic(
    s: &Session,
    mut input: Box<dyn AudioInput>,
    src: Src,
    device: &str,
    path: PathBuf,
    clock: RecordClock,
    stop_ns: &Arc<AtomicU64>,
) -> std::result::Result<MicSource, (Box<dyn AudioInput>, EngineError)> {
    let sr = s.active_sequence().map(|q| q.settings.sample_rate).unwrap_or(48_000).max(8_000);
    let label = src.key();
    let fmt = match input.start(device, sr) {
        Ok(f) => f,
        Err(e) => {
            return Err((
                input,
                EngineError::Other(format!(
                    "{label}: {e} (on macOS, allow FilmCraft or the terminal that launched it in System Settings ▸ Privacy & Security ▸ Microphone)"
                )),
            ));
        }
    };
    let first_ns = clock.now_ns();
    let channel = (s.prefs.voice_over.input_channel as usize).min(usize::from(fmt.channels.max(1)) - 1);
    let wav = match WavStream::create(&path, fmt.sample_rate.max(1)) {
        Ok(w) => w,
        Err(e) => {
            input.stop();
            return Err((input, EngineError::Other(format!("{}: {e}", path.display()))));
        }
    };
    let stats = Arc::new(SourceStats::default());
    stats.first_ns.store(first_ns, Ordering::Release);
    let (st, stop) = (stats.clone(), stop_ns.clone());
    // the input moves to the thread and comes back from it; if the thread cannot start, the
    // input is lost with it, so a spare is made first for the session
    let spare = input.spawn();
    let spawned = std::thread::Builder::new().name(format!("filmcraft-record-{label}")).spawn(move || {
        let mut input = input;
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| mic_worker(&mut input, wav, channel, clock, first_ns, &st, &stop)))
            .unwrap_or_else(|p| Err(format!("the microphone recorder crashed: {}", panic_text(&p))));
        (input, r)
    });
    match spawned {
        Ok(w) => Ok(MicSource {
            src,
            device: if device.is_empty() { "Default".into() } else { device.to_string() },
            session_input: false,
            stats,
            worker: Some(w),
            path,
        }),
        Err(e) => {
            Err((spare.unwrap_or_else(|| Box::new(SyntheticInput::clicks())), EngineError::Other(format!("{label}: cannot start the recording thread: {e}"))))
        }
    }
}

fn start(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "record.start";
    can_start(s).map_err(|e| bad(cmd, e))?;
    let plan = plan(s, p)?;
    let name = match str_p(p, "name") {
        Some(n) => Some(check_name(n).map_err(|e| bad(cmd, e))?),
        None => None,
    };
    let f = factory(s);
    // resolve devices before touching anything
    let needs_video = plan.screen.is_some() || !plan.cameras.is_empty();
    let devs = if needs_video { f.devices().map_err(|e| EngineError::Other(e.message))? } else { VideoDevices::default() };
    let screen_name = match &plan.screen {
        Some((ScreenTarget::Display(id), _)) => {
            Some(devs.displays.iter().find(|d| &d.id == id).map(|d| d.name.clone()).ok_or_else(|| bad(cmd, format!("no display `{id}`")))?)
        }
        Some((ScreenTarget::Window(id), _)) => {
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
    // a second microphone needs a second input of the same kind
    let mut extra_inputs: Vec<Box<dyn AudioInput>> = Vec::new();
    for _ in 1..plan.mics.len() {
        match s.voiceover.input.as_ref().and_then(|i| i.spawn()) {
            Some(i) => extra_inputs.push(i),
            None => return Err(bad(cmd, "this system records one microphone at a time")),
        }
    }
    let mut srcs: Vec<Src> = Vec::new();
    if plan.screen.is_some() {
        srcs.push(Src::new(SourceKind::Screen, 0));
    }
    srcs.extend((0..plan.cameras.len()).map(|n| Src::new(SourceKind::Camera, n)));
    srcs.extend((0..plan.mics.len()).map(|n| Src::new(SourceKind::Mic, n)));
    let dir = record_dir(s, p);
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{}: {e}", dir.display())))?;
    let name = pick_name(s, &dir, name.as_deref(), &srcs);
    let clock = RecordClock::new();
    let stop_ns = Arc::new(AtomicU64::new(NONE));
    let mut active = Active { name: name.clone(), dir: dir.clone(), clock, video: Vec::new(), mics: Vec::new(), stop_ns: stop_ns.clone(), microphones };
    // screen
    if let Some((target, fps)) = &plan.screen {
        let src = Src::new(SourceKind::Screen, 0);
        let id = match target {
            ScreenTarget::Display(d) | ScreenTarget::Window(d) => d.clone(),
        };
        let setup = VideoSetup {
            src,
            req: VideoRequest { width: None, height: None, fps: *fps },
            path: dir.join(file_name(&name, src)),
            device_id: id,
            device_name: screen_name.clone().unwrap_or_default(),
            mirror: false,
        };
        let r = f.open_screen(target).map_err(|e| EngineError::Other(format!("screen: {e}"))).and_then(|input| start_video(input, setup, clock, &stop_ns));
        match r {
            Ok(v) => active.video.push(v),
            Err(e) => {
                abort(s, active);
                return Err(e);
            }
        }
    }
    // cameras
    for (n, (c, cname)) in plan.cameras.iter().zip(&camera_names).enumerate() {
        let src = Src::new(SourceKind::Camera, n);
        let setup =
            VideoSetup { src, req: c.req, path: dir.join(file_name(&name, src)), device_id: c.device.clone(), device_name: cname.clone(), mirror: c.mirror };
        let r = f
            .open_camera(&c.device)
            .map_err(|e| EngineError::Other(format!("{}: {e}", src.key())))
            .and_then(|input| start_video(input, setup, clock, &stop_ns));
        match r {
            Ok(v) => active.video.push(v),
            Err(e) => {
                abort(s, active);
                return Err(e);
            }
        }
    }
    // microphones: the first on the session's input, the others on inputs made for them
    let mut extra = extra_inputs.into_iter();
    for (n, device) in plan.mics.iter().enumerate() {
        let src = Src::new(SourceKind::Mic, n);
        let path = dir.join(file_name(&name, src));
        let input = if n == 0 {
            s.voiceover.input.take().unwrap_or_else(|| Box::new(SyntheticInput::clicks()))
        } else {
            extra.next().unwrap_or_else(|| Box::new(SyntheticInput::clicks()))
        };
        match start_mic(s, input, src, device, path, clock, &stop_ns) {
            Ok(mut m) => {
                m.session_input = n == 0;
                active.mics.push(m);
            }
            Err((input, e)) => {
                if n == 0 {
                    s.voiceover.input = Some(input);
                }
                abort(s, active);
                return Err(e);
            }
        }
    }
    let files: Vec<Value> = active
        .files()
        .iter()
        .map(|(k, f)| json!({"kind": k.kind.name(), "key": k.key(), "path": f.to_string_lossy(), "sidecar": sidecar_path(f).to_string_lossy()}))
        .collect();
    let clock_start = clock.unix_start_ns();
    s.record.active = Some(active);
    Ok(json!({"recording": true, "name": name, "clockStartNs": clock_start, "dir": dir.to_string_lossy(), "files": files}))
}

fn status_json(s: &Session) -> Value {
    let Some(a) = &s.record.active else { return json!({"recording": false, "sources": []}) };
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
    json!({
        "recording": true,
        "name": a.name,
        "elapsed": a.clock.now_ns() as f64 / 1e9,
        "sources": sources,
        "error": (!errors.is_empty()).then(|| errors.join("; ")),
    })
}

fn status(s: &mut Session, _p: &Value) -> Result<Value> {
    Ok(status_json(s))
}

/// One source after stop.
struct Done {
    src: Src,
    path: PathBuf,
    device_id: String,
    device_name: String,
    mirror: bool,
    first_ns: u64,
    last_ns: u64,
    dropped: u64,
    fin: Finished,
}

fn write_sidecar(name: &str, clock: &RecordClock, d: &Done, offset_ms: f64) -> std::io::Result<()> {
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
        "dropped": d.dropped,
        "bytes": d.fin.bytes,
        "events": [],
    });
    if let Some(o) = v.as_object_mut() {
        if d.src.kind == SourceKind::Mic {
            o.insert("sample_rate".into(), json!(d.fin.sample_rate));
            o.insert("channels".into(), json!(1));
            o.insert("samples".into(), json!(d.fin.samples));
        } else {
            o.insert("frames".into(), json!(d.fin.frames));
            o.insert("width".into(), json!(d.fin.width));
            o.insert("height".into(), json!(d.fin.height));
            o.insert("fps".into(), json!(d.fin.fps));
            o.insert("encoder".into(), json!(d.fin.encoder));
        }
        if d.src.kind == SourceKind::Camera {
            o.insert("camera_offset_ms".into(), json!(offset_ms));
            o.insert("mirror".into(), json!(d.mirror));
            if d.mirror {
                o.insert("mirror_note".into(), json!("the file is as the camera saw it; the clip has a Horizontal Flip effect"));
            }
        }
    }
    let bytes = serde_json::to_vec_pretty(&v).map_err(std::io::Error::other)?;
    filmcraft_format::atomic_write(&sidecar_path(&d.path), &bytes)
}

/// Stop every source and collect what each produced (`Err`: that source's failure).
fn finish_all(s: &mut Session, mut a: Active) -> (Active, Vec<std::result::Result<Done, String>>) {
    let stop = a.clock.now_ns();
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
            first_ns: v.stats.first().unwrap_or(0),
            last_ns: v.stats.last().unwrap_or(0),
            dropped: v.stats.dropped.load(Ordering::Relaxed),
            fin,
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
            first_ns: m.stats.first().unwrap_or(0),
            last_ns: m.stats.last().unwrap_or(0),
            dropped: 0,
            fin,
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
        if let Err(e) = write_sidecar(&a.name, &a.clock, d, offset_of(d.src)) {
            errors.push(format!("{}: sidecar: {e}", d.src.key()));
        }
    }
    let offsets_ms: Vec<f64> = done.iter().map(|d| offset_of(d.src)).collect();
    // import + sequence, one undo step
    let n0 = s.history.undo.len();
    let r = import_and_place(s, &a.name, &done, &offsets_ms);
    crate::clip_ops::collapse_history(s, n0, "Record");
    let mut v = r?;
    if let Some(o) = v.as_object_mut() {
        o.insert("files".into(), json!(done.iter().map(|d| d.path.to_string_lossy().into_owned()).collect::<Vec<_>>()));
        o.insert("errors".into(), json!(errors));
    }
    Ok(v)
}

/// Import the finished files and build the synced sequence: the screen on V1, the cameras on the
/// next video tracks in order, the microphones on A1… (`offsets_ms[i]`: the shift of `done[i]`).
fn import_and_place(s: &mut Session, name: &str, done: &[Done], offsets_ms: &[f64]) -> Result<Value> {
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
    let firsts: Vec<(u64, i64)> = done.iter().zip(offsets_ms).map(|(d, o)| (d.first_ns, (o * 1e6).round() as i64)).collect();
    let offsets: Vec<(Src, i64)> = done.iter().map(|d| d.src).zip(sync_offsets_by(&firsts)).collect();
    let off = |k: Src| offsets.iter().find(|o| o.0 == k).map(|o| Tick::from_units(o.1, 1_000_000_000));
    let item = |k: Src| items.iter().find(|i| i.0 == k).map(|i| i.1);
    let of_kind = |kind: SourceKind| -> Vec<Src> { items.iter().map(|i| i.0).filter(|k| k.kind == kind).collect() };
    let videos: Vec<Src> = of_kind(SourceKind::Screen).into_iter().chain(of_kind(SourceKind::Camera)).collect();
    let audios: Vec<Src> = of_kind(SourceKind::Mic);
    // sequence settings: the screen's picture, else the first camera's; the first mic's rate
    let lead = videos.first().or(audios.first()).and_then(|k| item(*k));
    let info = |it: Option<ItemId>| it.and_then(|i| s.project.item(i)).and_then(|i| i.as_media()).map(|m| m.info.clone());
    let mut settings = info(lead).map(|i| crate::commands::default_seq_settings_for(&i)).unwrap_or_default();
    if let Some(a) = info(audios.first().and_then(|k| item(*k))).and_then(|i| i.audio) {
        settings.sample_rate = a.sample_rate.max(8000);
    }
    let mirrored: Vec<Src> = done.iter().filter(|d| d.mirror).map(|d| d.src).collect();
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
        st.active_sequence = Some(sid);
        if !st.open_sequences.contains(&sid) {
            st.open_sequences.push(sid);
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
            r#"{"screen":{"display":id}|{"window":id}?,"cameras":[{"device":id,"width":n?,"height":n?,"fps":n?,"mirror":bool?}]?,"camera":{..}?,"mics":[{"device":str?}]?,"mic":{..}?,"name":str?,"dir":str?}"#,
            can_start,
            start,
            true,
        ),
        spec("record.status", "Recording Status", "{}", crate::commands::always, status, false),
        spec("record.stop", "Stop Recording", r#"{"discard":bool?,"cameraOffsetMs":f64?,"cameraOffsetsMs":[f64]?}"#, is_recording, stop, true),
        spec("record.cancel", "Cancel Recording", "{}", is_recording, cancel, true),
    ]
}

/// `record.status` for hosts (the status bar, the Record panel) without going through a command.
pub fn status_of(s: &Session) -> Value {
    status_json(s)
}
