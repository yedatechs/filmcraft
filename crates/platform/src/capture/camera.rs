//! Camera capture through AVFoundation, FFI module.
//!
//! - [`devices`]: the video capture devices (built-in, external, Continuity cameras) with their
//!   formats (size and highest frame rate); listing needs no permission and shows no prompt.
//! - [`CameraInput`]: an `AVCaptureSession` with the device's input and an
//!   `AVCaptureVideoDataOutput` delivering BGRA `CMSampleBuffer`s (late frames discarded) to a
//!   sample-buffer delegate (an Objective-C class defined here) on a serial dispatch queue; each
//!   frame is copied and handed to the engine's sink with its presentation time mapped onto the
//!   recording clock.
//! - The format is chosen here, never by a session preset ([`choose_format`]): the device's own
//!   format nearest the size asked for (720p / 1080p / 2160p, else its largest), landscape unless
//!   a portrait size was asked for, set as the device's `activeFormat` under its configuration
//!   lock, which is held until the session runs, with the session on `InputPriority` so it leaves
//!   the device alone. A size preset lets AVFoundation pick, and for "1920x1080" it picks a
//!   webcam's 1080x1920 when that comes first in the device's list (an Insta360 Link 2C lists its
//!   portrait formats first, and switches itself to portrait mode when one is selected). The
//!   frame rate is the one asked for when the chosen format supports it
//!   (`activeVideoMin/MaxFrameDuration`), else the format's best.
//! - Camera audio is not captured: the microphone source records sound.
//! - Orientation ([`VideoInput::rotation`], Rotate Auto): on macOS 14+ an
//!   `AVCaptureDeviceRotationCoordinator` made for the device (the object FaceTime-style apps use to
//!   stand a turned camera upright) gives `videoRotationAngleForHorizonLevelCapture`, less the
//!   angle the data output's connection already applies (`videoRotationAngle`, 0 unless set);
//!   before macOS 14 (no coordinator) it is the connection's own angle (`videoOrientation`
//!   mapped to degrees). Read on demand (at record start, by the engine's frame loop while
//!   recording and by the preview), never pushed: the pictures are never turned here, the turn
//!   goes into the file's track header. Mirroring is not read: Mirror stays the user's choice.
//!
//! The Camera permission is checked first; an undetermined one is requested (the system prompt)
//! without waiting, and the start fails with a message naming the System Settings pane.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceFormat, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession,
    AVCaptureSessionPresetInputPriority, AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaType, AVMediaTypeAudio,
    AVMediaTypeVideo,
};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags, CMVideoFormatDescriptionGetDimensions};
use objc2_core_video::kCVPixelBufferPixelFormatTypeKey;
use objc2_foundation::{NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};

use filmcraft_engine::record::{
    CameraFormat, CameraInfo, CaptureError, CaptureErrorKind, FrameSink, Permission, PixelFormat, RecordClock, VideoFormat, VideoInput, VideoRequest,
};

use super::screen::{BGRA, Shared, copy_frame, host_now_ns};
use super::{HostClockMap, Need, START_TIMEOUT, permission_error, waited};

fn failed(msg: impl Into<String>) -> CaptureError {
    CaptureError::new(CaptureErrorKind::Failed, msg)
}

fn media(audio: bool) -> Option<&'static AVMediaType> {
    // SAFETY: AVFoundation's media type constants are immutable statics (None if the symbol is
    // missing on this macOS).
    unsafe { if audio { AVMediaTypeAudio } else { AVMediaTypeVideo } }
}

/// Camera (`audio = false`) or Microphone (`true`) permission, without a prompt.
pub fn permission(audio: bool) -> Permission {
    let Some(m) = media(audio) else { return Permission::Unavailable };
    // SAFETY: `m` is AVMediaTypeVideo or AVMediaTypeAudio, the two types the call accepts.
    let s = super::catch_objc("checking the camera permission", || unsafe { AVCaptureDevice::authorizationStatusForMediaType(m) });
    match s.unwrap_or(AVAuthorizationStatus::NotDetermined) {
        AVAuthorizationStatus::Authorized => Permission::Granted,
        AVAuthorizationStatus::NotDetermined => Permission::Undetermined,
        _ => Permission::Denied,
    }
}

/// Check the Camera permission; when undetermined, ask (the system prompt) without waiting.
fn require_permission() -> Result<(), CaptureError> {
    let p = permission(false);
    if p == Permission::Undetermined
        && let Some(m) = media(false)
    {
        let handler = RcBlock::new(|_granted: Bool| {});
        // SAFETY: `m` is AVMediaTypeVideo; AVFoundation copies the (empty) handler block.
        let _ = super::catch_objc("asking for the camera permission", || unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(m, &handler) });
    }
    match permission_error(Need::Camera, p) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// The video capture devices.
fn video_devices() -> Vec<Retained<AVCaptureDevice>> {
    let Some(m) = media(false) else { return Vec::new() };
    // SAFETY: `m` is AVMediaTypeVideo. `devicesWithMediaType:` is deprecated in favour of
    // discovery sessions but available on every macOS we run on, and lists built-in, external
    // and Continuity cameras without naming device types that older systems lack.
    #[allow(deprecated)]
    let list = unsafe { AVCaptureDevice::devicesWithMediaType(m) };
    list.iter().collect()
}

fn formats_of(d: &AVCaptureDevice) -> Vec<CameraFormat> {
    let mut out: Vec<CameraFormat> = Vec::new();
    // SAFETY: `d` is a valid device; formats, descriptions and rate ranges are retained while used.
    unsafe {
        for f in d.formats().iter() {
            let desc = f.formatDescription();
            let dim = CMVideoFormatDescriptionGetDimensions(&desc);
            let fps = f.videoSupportedFrameRateRanges().iter().map(|r| r.maxFrameRate()).fold(0.0f64, f64::max);
            let (Ok(width), Ok(height)) = (u32::try_from(dim.width), u32::try_from(dim.height)) else { continue };
            let fps = if fps.is_finite() { fps.round().clamp(0.0, 240.0) as u32 } else { 0 };
            if width == 0 || height == 0 {
                continue;
            }
            match out.iter_mut().find(|o| o.width == width && o.height == height) {
                Some(o) => o.fps = o.fps.max(fps),
                None => out.push(CameraFormat { width, height, fps }),
            }
        }
    }
    out.sort_by_key(|f| std::cmp::Reverse(u64::from(f.width) * u64::from(f.height)));
    out.truncate(16);
    out
}

/// Cameras with their formats (no permission needed, no prompt).
pub fn devices() -> Result<Vec<CameraInfo>, CaptureError> {
    let r = std::panic::catch_unwind(|| {
        super::catch_objc("listing the cameras", video_devices)
            .unwrap_or_default()
            .iter()
            .map(|d| {
                // SAFETY: `d` is a valid, retained device.
                let (id, name) = unsafe { (d.uniqueID().to_string(), d.localizedName().to_string()) };
                CameraInfo { id, name, formats: formats_of(d) }
            })
            .collect()
    });
    r.map_err(|_| failed("panic while listing the cameras"))
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `SampleDelegate` does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "FilmCraftCameraSampleDelegate"]
    #[ivars = Arc<Shared>]
    struct SampleDelegate;

    unsafe impl NSObjectProtocol for SampleDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for SampleDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(&self, _output: &AVCaptureOutput, sample_buffer: &CMSampleBuffer, _connection: &AVCaptureConnection) {
            let shared = self.ivars();
            let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
                if shared.stopped.load(Ordering::Acquire) {
                    return;
                }
                match copy_frame(sample_buffer, &shared.map) {
                    Ok(Some(f)) => (shared.sink)(f),
                    Ok(None) => {}
                    Err(e) => shared.fail(e),
                }
            }));
            if r.is_err() {
                shared.fail("panic while reading a camera frame".into());
            }
        }
    }
);

impl SampleDelegate {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(shared);
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

struct Running {
    session: Retained<AVCaptureSession>,
    _output: Retained<AVCaptureVideoDataOutput>,
    /// The output's video connection (its applied rotation angle).
    connection: Option<Retained<AVCaptureConnection>>,
    /// An `AVCaptureDeviceRotationCoordinator` for the device (macOS 14+).
    coordinator: Option<Retained<AnyObject>>,
    _delegate: Retained<SampleDelegate>,
    _queue: DispatchRetained<DispatchQueue>,
    shared: Arc<Shared>,
}

struct Sendable<T>(T);

// SAFETY: AVCaptureSession and its outputs are documented as usable from any thread (Apple
// recommends starting and stopping them off the main thread); `CameraInput` owns them and uses
// them from one thread at a time (`&mut self` methods).
unsafe impl<T> Send for Sendable<T> {}

/// Start a configured session: `startRunning` blocks until the camera runs (or fails), so it runs
/// on its own thread and is waited for at most [`START_TIMEOUT`]; a session that starts after
/// that is stopped again.
fn start_running(session: &Retained<AVCaptureSession>) -> Result<(), CaptureError> {
    let (tx, rx) = mpsc::sync_channel::<bool>(1);
    let held = Sendable(session.clone());
    std::thread::Builder::new()
        .name("filmcraft-camera-start".into())
        .spawn(move || {
            let held = held;
            let running = std::panic::catch_unwind(AssertUnwindSafe(|| {
                // SAFETY: the session is retained by `held` and fully configured; starting it
                // from a background thread is what Apple recommends.
                super::catch_objc("starting the camera session", || unsafe {
                    held.0.startRunning();
                    held.0.isRunning()
                })
                .unwrap_or(false)
            }))
            .unwrap_or(false);
            if tx.try_send(running).is_err() && running {
                // nobody waits any more (the start timed out): stop it again
                // SAFETY: as above; stopping a running session is always allowed.
                unsafe { held.0.stopRunning() };
            }
        })
        .map_err(|e| failed(format!("cannot start the camera thread: {e}")))?;
    match rx.recv_timeout(START_TIMEOUT) {
        Ok(true) => Ok(()),
        Ok(false) => Err(failed("the camera did not start (in use by another app?)")),
        Err(_) => Err(failed(format!("starting the camera: macOS {}", waited(START_TIMEOUT)))),
    }
}

/// A camera being recorded.
pub struct CameraInput {
    device: String,
    running: Option<Sendable<Running>>,
    error: Option<Arc<Shared>>,
}

impl CameraInput {
    pub fn new(device: &str) -> Result<Self, CaptureError> {
        require_permission()?;
        if !devices()?.iter().any(|c| c.id == device) {
            return Err(CaptureError::new(CaptureErrorKind::NoDevice, format!("no camera `{device}`")));
        }
        Ok(Self { device: device.to_string(), running: None, error: None })
    }
}

/// A camera format as [`choose_format`] sees it.
#[derive(Clone, Debug, PartialEq)]
struct FormatInfo {
    width: u32,
    height: u32,
    /// Its frame-rate ranges (lowest, highest).
    rates: Vec<(f64, f64)>,
    /// The pixel format's four-character code.
    subtype: u32,
}

impl FormatInfo {
    fn does(&self, fps: f64) -> bool {
        self.rates.iter().any(|(lo, hi)| *lo <= fps + 0.01 && *hi >= fps - 0.01)
    }
}

/// The 4:2:0 formats AVFoundation itself prefers for video (a webcam's compressed stream,
/// decoded): the same size in another pixel format is second choice.
const PREFERRED_SUBTYPES: [u32; 2] = [u32::from_be_bytes(*b"420v"), u32::from_be_bytes(*b"420f")];

/// Which of a camera's `formats` records `want` (width, height; None = the camera's best) at
/// `fps`: its index.
///
/// - Orientation first: a landscape format unless a portrait size was asked for (a camera with
///   portrait formats switches itself to portrait mode when one is selected), and any format
///   only when the camera has none that way up.
/// - Then the size nearest the one asked for (one side given: the other at 16:9), the larger of
///   two equally near; without a size, the most pixels.
/// - Then a format that can do `fps`, then a preferred pixel format, then the device's order.
fn choose_format(formats: &[FormatInfo], want: (Option<u32>, Option<u32>), fps: u32) -> Option<usize> {
    let target = match want {
        (Some(w), Some(h)) => Some((w, h)),
        (None, Some(h)) => Some((h.saturating_mul(16) / 9, h)),
        (Some(w), None) => Some((w, w.saturating_mul(9) / 16)),
        (None, None) => None,
    };
    let portrait = target.is_some_and(|(w, h)| h > w);
    let way_up = |f: &FormatInfo| f.width == f.height || (f.height > f.width) == portrait;
    let usable = |f: &FormatInfo| f.width > 0 && f.height > 0;
    let any_way_up = formats.iter().any(|f| usable(f) && way_up(f));
    let fps = f64::from(fps.clamp(1, 240));
    formats
        .iter()
        .enumerate()
        .filter(|(_, f)| usable(f) && (!any_way_up || way_up(f)))
        .min_by_key(|(i, f)| {
            let area = u64::from(f.width) * u64::from(f.height);
            // (a format the other way up, the camera having none this way: the size turned)
            let off = target.map_or(0, |(w, h)| {
                let (w, h) = if way_up(f) { (w, h) } else { (h, w) };
                u64::from(f.width.abs_diff(w)) + u64::from(f.height.abs_diff(h))
            });
            (off, std::cmp::Reverse(area), !f.does(fps), !PREFERRED_SUBTYPES.contains(&f.subtype), *i)
        })
        .map(|(i, _)| i)
}

/// `f` for [`choose_format`] (a size that does not fit is 0 × 0 and never chosen).
///
/// # Safety
/// `f` must be a valid format of a capture device.
unsafe fn format_info(f: &AVCaptureDeviceFormat) -> FormatInfo {
    // SAFETY: the format, its description and its rate ranges are retained while read.
    unsafe {
        let desc = f.formatDescription();
        let dim = CMVideoFormatDescriptionGetDimensions(&desc);
        let rates = f.videoSupportedFrameRateRanges().iter().map(|r| (r.minFrameRate(), r.maxFrameRate())).collect();
        FormatInfo { width: u32::try_from(dim.width).unwrap_or(0), height: u32::try_from(dim.height).unwrap_or(0), rates, subtype: desc.media_sub_type() }
    }
}

/// A device's configuration lock, given back when dropped.
struct ConfigLock<'a>(&'a AVCaptureDevice);

impl Drop for ConfigLock<'_> {
    fn drop(&mut self) {
        // SAFETY: the lock was taken by `lockForConfiguration` on this device and is given back once.
        unsafe { self.0.unlockForConfiguration() }
    }
}

impl VideoInput for CameraInput {
    fn start(&mut self, req: &VideoRequest, clock: RecordClock, sink: FrameSink) -> Result<VideoFormat, CaptureError> {
        if self.running.is_some() {
            return Err(failed("the camera is already being recorded"));
        }
        require_permission()?;
        // every AVFoundation call below may raise an Objective-C exception (a format or preset the
        // device rejects): caught here, it is this camera's error, not the end of the app
        super::catch_objc("starting the camera", || self.start_session(req, clock, sink))?
    }

    fn stop(&mut self) {
        let Some(Sendable(r)) = self.running.take() else { return };
        r.shared.stopped.store(true, Ordering::Release);
        // SAFETY: the session is retained by `r`; stopping a running session is always allowed
        // and blocks until it stopped, so no frame reaches the sink afterwards.
        let _ = super::catch_objc("stopping the camera", || unsafe { r.session.stopRunning() });
    }

    fn error(&self) -> Option<String> {
        self.error.as_ref().and_then(|s| s.error.lock().unwrap_or_else(PoisonError::into_inner).clone())
    }

    fn rotation(&self) -> Option<u16> {
        let r = &self.running.as_ref()?.0;
        let read = || {
            // SAFETY: the connection and coordinator are retained by `Running`; both selectors are
            // checked with `respondsToSelector:` first (videoRotationAngle and the coordinator's
            // angle are CGFloat = f64 on every Mac we run on), and `videoOrientation` exists on
            // every macOS (deprecated since 14).
            unsafe {
                let applied = r.connection.as_ref().map(|c| {
                    if c.respondsToSelector(sel!(videoRotationAngle)) {
                        let a: f64 = msg_send![&**c, videoRotationAngle];
                        quarter_turn(a)
                    } else {
                        #[allow(deprecated)]
                        let o = c.videoOrientation().0;
                        orientation_degrees(o)
                    }
                });
                let level = r
                    .coordinator
                    .as_ref()
                    .filter(|c| {
                        let ok: bool = msg_send![&***c, respondsToSelector: sel!(videoRotationAngleForHorizonLevelCapture)];
                        ok
                    })
                    .map(|c| {
                        let a: f64 = msg_send![&**c, videoRotationAngleForHorizonLevelCapture];
                        quarter_turn(a)
                    });
                camera_turn(applied, level)
            }
        };
        super::catch_objc("reading the camera's orientation", read).ok().flatten()
    }
}

/// `degrees` to the nearest quarter turn, 0 / 90 / 180 / 270 (not finite: 0).
fn quarter_turn(degrees: f64) -> u16 {
    if !degrees.is_finite() {
        return 0;
    }
    ((degrees / 90.0).round().rem_euclid(4.0) as u16).saturating_mul(90)
}

/// A connection's (pre-macOS 14) `videoOrientation` as a rotation angle, the way Apple maps the
/// two: landscape right 0°, portrait 90°, landscape left 180°, portrait upside down 270°.
/// (`AVCaptureVideoOrientation`'s raw values: 1 portrait, 2 upside down, 3 right, 4 left.)
fn orientation_degrees(o: isize) -> u16 {
    match o {
        1 => 90,
        4 => 180,
        2 => 270,
        _ => 0,
    }
}

/// The turn the recorded pictures need: with a rotation coordinator (`level`, the angle that
/// stands the device's capture upright) less what the connection already turns (`applied`);
/// without one, the connection's angle. None when there is neither.
fn camera_turn(applied: Option<u16>, level: Option<u16>) -> Option<u16> {
    match (applied, level) {
        (a, Some(l)) => Some((l + 360 - a.unwrap_or(0) % 360) % 360),
        (Some(a), None) => Some(a % 360),
        (None, None) => None,
    }
}

/// An `AVCaptureDeviceRotationCoordinator` for `device` (no preview layer), or None before
/// macOS 14 (the class is missing).
///
/// # Safety
/// `device` must be a valid capture device.
unsafe fn rotation_coordinator(device: &AVCaptureDevice) -> Option<Retained<AnyObject>> {
    let cls = AnyClass::get(c"AVCaptureDeviceRotationCoordinator")?;
    // SAFETY: `cls` is AVCaptureDeviceRotationCoordinator (macOS 14+), whose designated
    // initializer is `initWithDevice:previewLayer:`; a nil preview layer is allowed.
    unsafe {
        let obj: objc2::rc::Allocated<AnyObject> = msg_send![cls, alloc];
        msg_send![obj, initWithDevice: device, previewLayer: std::ptr::null_mut::<AnyObject>()]
    }
}

impl CameraInput {
    /// The body of [`VideoInput::start`], run under [`super::catch_objc`].
    fn start_session(&mut self, req: &VideoRequest, clock: RecordClock, sink: FrameSink) -> Result<VideoFormat, CaptureError> {
        let device = video_devices()
            .into_iter()
            // SAFETY: valid, retained devices.
            .find(|d| unsafe { d.uniqueID().to_string() } == self.device)
            .ok_or_else(|| CaptureError::new(CaptureErrorKind::NoDevice, format!("the camera `{}` is gone", self.device)))?;
        // SAFETY: every object created here is retained by `Running` (or dropped on error); the
        // session is configured on this thread before it runs, after which AVFoundation calls the
        // delegate on its own serial queue.
        unsafe {
            let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                .map_err(|e| failed(format!("cannot open the camera: {}", e.localizedDescription())))?;
            let session = AVCaptureSession::new();
            session.beginConfiguration();
            // the device's format is ours to choose (below): the session must not pick one
            if session.canSetSessionPreset(AVCaptureSessionPresetInputPriority) {
                session.setSessionPreset(AVCaptureSessionPresetInputPriority);
            }
            if !session.canAddInput(&input) {
                session.commitConfiguration();
                return Err(failed("the camera is busy or cannot be recorded"));
            }
            session.addInput(&input);
            let output = AVCaptureVideoDataOutput::new();
            let key: &NSString = &*(kCVPixelBufferPixelFormatTypeKey as *const objc2_core_foundation::CFString as *const NSString);
            let value = NSNumber::numberWithUnsignedInt(BGRA);
            let settings: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_slices(&[key], &[&*value as &AnyObject]);
            output.setVideoSettings(Some(&settings));
            output.setAlwaysDiscardsLateVideoFrames(true);
            let shared = Arc::new(Shared {
                sink,
                map: HostClockMap::new(clock.now_ns(), host_now_ns()),
                audio: None,
                stopped: AtomicBool::new(false),
                error: Mutex::new(None),
            });
            let delegate = SampleDelegate::new(shared.clone());
            let queue = DispatchQueue::new("org.filmcraft.capture.camera", None);
            output.setSampleBufferDelegate_queue(Some(ProtocolObject::from_ref(&*delegate)), Some(&queue));
            if !session.canAddOutput(&output) {
                session.commitConfiguration();
                return Err(failed("the camera cannot deliver frames to FilmCraft"));
            }
            session.addOutput(&output);
            session.commitConfiguration();
            // The format: chosen from the device's own list and set under its configuration lock,
            // which stays held until the session runs (macOS lets a session that starts with the
            // device unlocked put its own choice back). Without the lock (another app holds it)
            // the camera records as it is: its format and rate are never touched then.
            let was = format_info(&device.activeFormat());
            let formats = device.formats();
            let infos: Vec<FormatInfo> = formats.iter().map(|f| format_info(&f)).collect();
            let pick = choose_format(&infos, (req.width, req.height), req.fps);
            let lock = match device.lockForConfiguration() {
                Ok(()) => Some(ConfigLock(&device)),
                Err(e) => {
                    log::warn!("camera {}: cannot lock its configuration ({}); recording in its current format", self.device, e.localizedDescription());
                    None
                }
            };
            let mut chosen = None;
            if lock.is_some()
                && let Some(f) = pick.and_then(|i| formats.iter().nth(i))
            {
                match super::catch_objc("choosing the camera format", || device.setActiveFormat(&f)) {
                    Ok(()) => chosen = Some(f),
                    Err(e) => log::warn!("{e}; recording in the camera's current format"),
                }
            }
            // the frame rate asked for, when the format can do it; else the format's own
            let want = f64::from(req.fps.clamp(1, 60));
            let fmt = chosen.unwrap_or_else(|| device.activeFormat());
            let ranges = fmt.videoSupportedFrameRateRanges();
            let range = ranges.iter().find(|r| r.minFrameRate() <= want + 0.01 && r.maxFrameRate() >= want - 0.01);
            let best = ranges.iter().map(|r| r.maxFrameRate()).fold(0.0f64, f64::max);
            // The duration must lie inside the range's own CMTime bounds, which a camera reports as
            // exact fractions (an Insta360 Link says 30.00003 fps = 1000000/30000030): a plain 1/30
            // is a hair outside and AVFoundation raises "Not supported". So the range's own bound is
            // used when the rate asked for is one of its ends, and a failure here only means the
            // device's own rate, never a failed start.
            let applied = range.filter(|_| lock.is_some()).and_then(|r| {
                let at_max = (r.maxFrameRate() - want).abs() <= 0.01;
                let at_min = (r.minFrameRate() - want).abs() <= 0.01;
                let d = if at_max {
                    r.minFrameDuration()
                } else if at_min {
                    r.maxFrameDuration()
                } else {
                    CMTime { value: 1, timescale: req.fps.clamp(1, 60) as i32, flags: CMTimeFlags::Valid, epoch: 0 }
                };
                let set = super::catch_objc("setting the camera frame rate", || {
                    device.setActiveVideoMinFrameDuration(d);
                    device.setActiveVideoMaxFrameDuration(d);
                });
                match set {
                    Ok(()) => Some(req.fps.clamp(1, 60)),
                    Err(e) => {
                        log::warn!("{e}; recording at the camera's own rate");
                        None
                    }
                }
            });
            let fps = if let Some(fps) = applied {
                fps
            } else if best.is_finite() && best >= 1.0 {
                best.round().clamp(1.0, 60.0) as u32
            } else {
                req.fps.clamp(1, 60)
            };
            let started = start_running(&session);
            drop(lock);
            started?;
            let now = format_info(&device.activeFormat());
            log::info!(
                "camera {}: asked {:?}x{:?} @ {} fps; it was at {}x{}, chose {}, runs {}x{} @ {fps} fps",
                self.device,
                req.width,
                req.height,
                req.fps,
                was.width,
                was.height,
                pick.and_then(|i| infos.get(i)).map_or("nothing".to_string(), |f| format!("{}x{}", f.width, f.height)),
                now.width,
                now.height,
            );
            let desc = device.activeFormat().formatDescription();
            let dim = CMVideoFormatDescriptionGetDimensions(&desc);
            let w = u32::try_from(dim.width).unwrap_or(1280).clamp(16, 8192) & !1;
            let h = u32::try_from(dim.height).unwrap_or(720).clamp(16, 8192) & !1;
            // what tells the orientation (Rotate Auto); a failure only means "reports none"
            let connection = media(false).and_then(|m| super::catch_objc("reading the camera connection", || output.connectionWithMediaType(m)).ok().flatten());
            let coordinator = super::catch_objc("making the camera's rotation coordinator", || rotation_coordinator(&device)).ok().flatten();
            self.error = Some(shared.clone());
            self.running = Some(Sendable(Running { session, _output: output, connection, coordinator, _delegate: delegate, _queue: queue, shared }));
            Ok(VideoFormat { width: w, height: h, fps, format: PixelFormat::Bgra8 })
        }
    }
}

impl Drop for CameraInput {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_cameras_needs_no_permission() {
        let cams = devices().unwrap();
        for c in &cams {
            assert!(!c.id.is_empty() && !c.name.is_empty());
            assert!(c.formats.iter().all(|f| f.width > 0 && f.height > 0));
        }
        assert!(matches!(permission(false), Permission::Granted | Permission::Denied | Permission::Undetermined));
    }

    #[test]
    #[allow(deprecated)]
    fn orientation_becomes_a_quarter_turn() {
        assert_eq!([0.0, 89.6, 90.0, 180.0, 270.0, 360.0, -90.0, f64::NAN].map(quarter_turn), [0, 90, 90, 180, 270, 0, 270, 0]);
        let o = [
            objc2_av_foundation::AVCaptureVideoOrientation::LandscapeRight,
            objc2_av_foundation::AVCaptureVideoOrientation::Portrait,
            objc2_av_foundation::AVCaptureVideoOrientation::LandscapeLeft,
            objc2_av_foundation::AVCaptureVideoOrientation::PortraitUpsideDown,
        ];
        assert_eq!(o.map(|o| orientation_degrees(o.0)), [0, 90, 180, 270]);
        // the coordinator's angle less what the connection already turns
        assert_eq!(camera_turn(Some(0), Some(90)), Some(90));
        assert_eq!(camera_turn(Some(90), Some(90)), Some(0));
        assert_eq!(camera_turn(Some(180), Some(90)), Some(270));
        assert_eq!(camera_turn(None, Some(270)), Some(270));
        assert_eq!(camera_turn(Some(90), None), Some(90), "before macOS 14: the connection's");
        assert_eq!(camera_turn(None, None), None);
    }

    fn fmt(width: u32, height: u32, fourcc: &[u8; 4], rates: &[f64]) -> FormatInfo {
        FormatInfo { width, height, rates: rates.iter().map(|r| (*r, *r)).collect(), subtype: u32::from_be_bytes(*fourcc) }
    }

    /// The formats of the owner's Insta360 Link 2C, in the order macOS lists them
    /// (`AVCaptureDevice.formats`, 2026-10-10): portrait sizes come before their landscape twins.
    fn link_2c() -> Vec<FormatInfo> {
        let all = [60.0, 50.0, 30.0, 25.0, 24.0];
        let slow = [30.0, 25.0, 24.0];
        vec![
            fmt(736, 1280, b"420v", &all),
            fmt(736, 1280, b"2vuy", &all),
            fmt(1080, 1920, b"420v", &all),
            fmt(1080, 1920, b"2vuy", &slow),
            fmt(1088, 1920, b"420v", &all),
            fmt(1088, 1920, b"2vuy", &slow),
            fmt(1280, 720, b"420v", &all),
            fmt(1280, 720, b"2vuy", &all),
            fmt(1280, 960, b"420v", &all),
            fmt(1280, 960, b"2vuy", &all),
            fmt(1920, 1080, b"420v", &all),
            fmt(1920, 1080, b"2vuy", &all),
            fmt(1920, 1440, b"420v", &all),
            fmt(1920, 1440, b"2vuy", &all),
            fmt(2176, 3840, b"420v", &slow),
            fmt(3840, 2160, b"420v", &slow),
            fmt(3840, 2160, b"2vuy", &slow),
        ]
    }

    fn size(formats: &[FormatInfo], want: (Option<u32>, Option<u32>), fps: u32) -> Option<(u32, u32)> {
        choose_format(formats, want, fps).and_then(|i| formats.get(i)).map(|f| (f.width, f.height))
    }

    #[test]
    fn a_quality_never_selects_a_portrait_format() {
        let f = link_2c();
        // the panel's qualities: the landscape format of that size, in the preferred pixel format
        assert_eq!(choose_format(&f, (Some(1920), Some(1080)), 30), Some(10), "1080p is 1920x1080, not the 1080x1920 listed before it");
        assert_eq!(choose_format(&f, (Some(1280), Some(720)), 30), Some(6));
        assert_eq!(choose_format(&f, (Some(3840), Some(2160)), 30), Some(15));
        // Native: the most pixels that are landscape (2176x3840 has more, and is portrait)
        assert_eq!(choose_format(&f, (None, None), 30), Some(15));
        // one side given: the other at 16:9
        assert_eq!(size(&f, (None, Some(1080)), 30), Some((1920, 1080)));
        assert_eq!(size(&f, (Some(1280), None), 30), Some((1280, 720)));
        // a size the camera lacks: the nearest landscape one, the larger of two equally near
        assert_eq!(size(&f, (Some(2560), Some(1440)), 30), Some((1920, 1440)));
        assert_eq!(size(&f, (Some(640), Some(360)), 30), Some((1280, 720)));
        for q in
            [(Some(1280), Some(720)), (Some(1920), Some(1080)), (Some(3840), Some(2160)), (None, None), (Some(1), Some(1)), (Some(u32::MAX), Some(u32::MAX))]
        {
            for fps in [0, 1, 24, 30, 60, 240, u32::MAX] {
                let (w, h) = size(&f, q, fps).unwrap();
                assert!(w > h, "{q:?} @ {fps}: {w}x{h}");
            }
        }
    }

    #[test]
    fn a_portrait_size_asks_for_the_cameras_portrait_mode() {
        let f = link_2c();
        assert_eq!(choose_format(&f, (Some(1080), Some(1920)), 30), Some(2));
        assert_eq!(size(&f, (Some(720), Some(1280)), 30), Some((736, 1280)));
        assert_eq!(size(&f, (Some(2160), Some(3840)), 30), Some((2176, 3840)));
    }

    #[test]
    fn the_rate_and_the_pixel_format_break_ties_only() {
        let f = link_2c();
        // 4K cannot do 60: the size asked for wins, the rate falls back to the format's own
        assert_eq!(size(&f, (Some(3840), Some(2160)), 60), Some((3840, 2160)));
        // of two formats of one size, the one that can do the rate; then the preferred pixel format
        let two = vec![fmt(1920, 1080, b"420v", &[30.0]), fmt(1920, 1080, b"2vuy", &[60.0, 30.0])];
        assert_eq!(choose_format(&two, (Some(1920), Some(1080)), 60), Some(1));
        assert_eq!(choose_format(&two, (Some(1920), Some(1080)), 30), Some(0));
        let ranged = vec![FormatInfo { width: 1280, height: 720, rates: vec![(1.0, 30.0)], subtype: 0 }, fmt(1280, 720, b"yuvs", &[60.0])];
        assert_eq!(choose_format(&ranged, (Some(1280), Some(720)), 15), Some(0));
        assert_eq!(choose_format(&ranged, (Some(1280), Some(720)), 60), Some(1));
    }

    #[test]
    fn odd_format_lists_never_panic() {
        assert_eq!(choose_format(&[], (Some(1920), Some(1080)), 30), None);
        // sizes that did not fit are 0 x 0 and never chosen
        assert_eq!(choose_format(&[fmt(0, 0, b"420v", &[30.0]), fmt(0, 1080, b"420v", &[30.0])], (None, None), 30), None);
        // a camera with portrait formats only records in one of them
        let tall = vec![fmt(1080, 1920, b"420v", &[30.0]), fmt(720, 1280, b"420v", &[30.0])];
        assert_eq!(choose_format(&tall, (Some(1920), Some(1080)), 30), Some(0));
        assert_eq!(choose_format(&tall, (None, None), 30), Some(0));
        // a square format is both ways up; no rates at all is still a format
        let square = vec![fmt(1080, 1080, b"420v", &[]), fmt(1080, 1920, b"420v", &[30.0])];
        assert_eq!(choose_format(&square, (Some(1920), Some(1080)), 30), Some(0));
        assert_eq!(choose_format(&square, (Some(1080), Some(1920)), 30), Some(1));
        let nan = vec![FormatInfo { width: 640, height: 480, rates: vec![(f64::NAN, f64::INFINITY)], subtype: 0 }];
        assert_eq!(choose_format(&nan, (None, None), 30), Some(0));
    }
}
