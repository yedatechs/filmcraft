//! Camera capture through AVFoundation, FFI module.
//!
//! - [`devices`]: the video capture devices (built-in, external, Continuity cameras) with their
//!   formats (size and highest frame rate); listing needs no permission and shows no prompt.
//! - [`CameraInput`]: an `AVCaptureSession` with the device's input and an
//!   `AVCaptureVideoDataOutput` delivering BGRA `CMSampleBuffer`s (late frames discarded) to a
//!   sample-buffer delegate (an Objective-C class defined here) on a serial dispatch queue; each
//!   frame is copied and handed to the engine's sink with its presentation time mapped onto the
//!   recording clock. The size follows the requested quality through the session preset
//!   (720p / 1080p / 2160p, else the device's best); the frame rate is the one asked for when
//!   the active format supports it (`activeVideoMin/MaxFrameDuration`), else the format's best.
//! - Camera audio is not captured: the microphone source records sound.
//!
//! The Camera permission is checked first; an undetermined one is requested (the system prompt)
//! without waiting, and the start fails with a message naming the System Settings pane.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession, AVCaptureSessionPreset,
    AVCaptureSessionPreset1280x720, AVCaptureSessionPreset1920x1080, AVCaptureSessionPreset3840x2160, AVCaptureSessionPresetHigh, AVCaptureVideoDataOutput,
    AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo,
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

fn preset_for(height: Option<u32>) -> &'static AVCaptureSessionPreset {
    // SAFETY: the preset constants are immutable statics.
    unsafe {
        match height {
            Some(h) if h >= 2160 => AVCaptureSessionPreset3840x2160,
            Some(h) if h >= 1080 => AVCaptureSessionPreset1920x1080,
            Some(_) => AVCaptureSessionPreset1280x720,
            None => AVCaptureSessionPresetHigh,
        }
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
            let preset = preset_for(req.height.or(req.width.map(|w| w * 9 / 16)));
            if session.canSetSessionPreset(preset) {
                session.setSessionPreset(preset);
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
            // the frame rate asked for, when the active format can do it (after the preset,
            // which resets it); else the device's own
            let want = f64::from(req.fps.clamp(1, 60));
            let fmt = device.activeFormat();
            let ranges = fmt.videoSupportedFrameRateRanges();
            let can = ranges.iter().any(|r| r.minFrameRate() <= want + 0.01 && r.maxFrameRate() >= want - 0.01);
            let best = ranges.iter().map(|r| r.maxFrameRate()).fold(0.0f64, f64::max);
            let fps = if can && device.lockForConfiguration().is_ok() {
                let d = CMTime { value: 1, timescale: req.fps.clamp(1, 60) as i32, flags: CMTimeFlags::Valid, epoch: 0 };
                device.setActiveVideoMinFrameDuration(d);
                device.setActiveVideoMaxFrameDuration(d);
                device.unlockForConfiguration();
                req.fps.clamp(1, 60)
            } else if best.is_finite() && best >= 1.0 {
                best.round().clamp(1.0, 60.0) as u32
            } else {
                req.fps.clamp(1, 60)
            };
            start_running(&session)?;
            let desc = device.activeFormat().formatDescription();
            let dim = CMVideoFormatDescriptionGetDimensions(&desc);
            let w = u32::try_from(dim.width).unwrap_or(1280).clamp(16, 8192) & !1;
            let h = u32::try_from(dim.height).unwrap_or(720).clamp(16, 8192) & !1;
            self.error = Some(shared.clone());
            self.running = Some(Sendable(Running { session, _output: output, _delegate: delegate, _queue: queue, shared }));
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
    fn presets_follow_the_quality() {
        // SAFETY: the preset constants are immutable statics; this only compares them.
        unsafe {
            assert_eq!(preset_for(Some(720)), AVCaptureSessionPreset1280x720);
            assert_eq!(preset_for(Some(1080)), AVCaptureSessionPreset1920x1080);
            assert_eq!(preset_for(Some(2160)), AVCaptureSessionPreset3840x2160);
            assert_eq!(preset_for(None), AVCaptureSessionPresetHigh);
        }
    }
}
