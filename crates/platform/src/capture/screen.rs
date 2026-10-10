//! Screen and window capture through ScreenCaptureKit (macOS 12.3+), FFI module.
//!
//! - [`devices`]: `SCShareableContent` (displays and on-screen windows with a title), only when
//!   Screen Recording is already allowed (`CGPreflightScreenCaptureAccess`), so listing never
//!   shows a prompt.
//! - [`ScreenInput`]: an `SCStream` on one display or window, BGRA at the display's pixel size
//!   (at most 3840 wide, scaled down to the requested resolution by ScreenCaptureKit),
//!   `minimumFrameInterval` 1 / fps, the cursor shown or hidden; with system audio
//!   (`capturesAudio`, macOS 13+) the stream's audio buffers (32-bit float) go to the audio sink
//!   with their own clock mapping; FilmCraft's own sound is left out. A stream output
//!   object (an Objective-C class defined here) receives the `CMSampleBuffer`s on a serial
//!   dispatch queue, copies complete frames and hands them to the engine's frame sink with the
//!   buffer's presentation time mapped onto the recording clock. ScreenCaptureKit only sends a
//!   frame when the screen changes; idle frames carry no picture and are skipped.
//!
//! Every Objective-C callback body runs under `catch_unwind`; a panic becomes the input's error.
//! Completion handlers only send on a channel; the waiting side gives up after
//! [`super::OS_TIMEOUT`].

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_core_graphics::{
    CGDisplayCopyDisplayMode, CGDisplayIsBuiltin, CGDisplayMode, CGMainDisplayID, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
};
use objc2_core_media::{CMAudioFormatDescriptionGetStreamBasicDescription, CMClock, CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_core_video::{
    CVImageBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput, SCStreamOutputType};

use filmcraft_engine::record::{
    AudioSink, CaptureError, CaptureErrorKind, CapturedAudio, CapturedFrame, DisplayInfo, FrameSink, Permission, PixelFormat, RecordClock, ScreenTarget,
    VideoFormat, VideoInput, VideoRequest, WindowInfo,
};

use super::{HostClockMap, Need, OS_TIMEOUT, permission_error, time_ns};

/// `kCVPixelFormatType_32BGRA`.
pub const BGRA: u32 = u32::from_be_bytes(*b"BGRA");

/// Widest picture a screen recording is made at (5K / 6K displays are scaled down to this).
const MAX_WIDTH: u32 = 3840;

/// Screen Recording permission (no prompt).
pub fn permission() -> Permission {
    if CGPreflightScreenCaptureAccess() { Permission::Granted } else { Permission::Denied }
}

/// Check (and when missing, request once: the system prompt) Screen Recording.
fn require_permission() -> Result<(), CaptureError> {
    if CGPreflightScreenCaptureAccess() {
        return Ok(());
    }
    // shows the system prompt the first time; never waits for the answer
    let _ = CGRequestScreenCaptureAccess();
    Err(permission_error(Need::Screen, Permission::Denied).unwrap_or_else(CaptureError::unavailable))
}

fn failed(msg: impl Into<String>) -> CaptureError {
    CaptureError::new(CaptureErrorKind::Failed, msg)
}

fn ns_error(e: *mut NSError) -> String {
    // SAFETY: ScreenCaptureKit passes either null or a valid NSError for the duration of the
    // completion handler; `retain` keeps it alive while we read its description.
    match unsafe { Retained::retain(e) } {
        Some(e) => e.localizedDescription().to_string(),
        None => "unknown error".into(),
    }
}

/// Retained Objective-C objects moved between threads.
struct Sendable<T>(T);

// SAFETY: the ScreenCaptureKit objects held here (shareable content, streams, filters,
// configurations, our output object) are reference-counted Objective-C objects that Apple
// documents as usable from any thread; we never use one from two threads at the same time
// (`ScreenInput` is owned by one thread and its methods take `&mut self`).
unsafe impl<T> Send for Sendable<T> {}

/// The shareable content (displays, windows), waiting at most [`OS_TIMEOUT`].
fn shareable_content() -> Result<Retained<SCShareableContent>, CaptureError> {
    require_permission()?;
    let (tx, rx) = mpsc::sync_channel::<Result<Sendable<Retained<SCShareableContent>>, String>>(1);
    let block = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: the handler receives a valid (or null) SCShareableContent; retaining it
            // keeps it alive after the handler returns.
            match unsafe { Retained::retain(content) } {
                Some(c) => Ok(Sendable(c)),
                None => Err(ns_error(error)),
            }
        }));
        let _ = tx.try_send(r.unwrap_or_else(|_| Err("panic while reading the shareable content".into())));
    });
    // SAFETY: the block lives as long as ScreenCaptureKit keeps it (it copies the block); the
    // arguments are plain booleans.
    unsafe { SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true, true, &block) };
    match rx.recv_timeout(OS_TIMEOUT) {
        Ok(Ok(c)) => Ok(c.0),
        Ok(Err(e)) => Err(failed(format!("cannot list the screens: {e}"))),
        Err(_) => Err(failed("macOS did not list the screens within 5 s")),
    }
}

/// Pixel size of a display (its current mode), else `fallback`.
fn display_pixels(id: u32, fallback: (u32, u32)) -> (u32, u32) {
    let mode = CGDisplayCopyDisplayMode(id);
    let (w, h) = (CGDisplayMode::pixel_width(mode.as_deref()), CGDisplayMode::pixel_height(mode.as_deref()));
    if w == 0 || h == 0 { fallback } else { (u32::try_from(w).unwrap_or(fallback.0), u32::try_from(h).unwrap_or(fallback.1)) }
}

fn display_name(id: u32, index: usize) -> String {
    if CGDisplayIsBuiltin(id) {
        "Built-in Display".into()
    } else if id == CGMainDisplayID() {
        "Main Display".into()
    } else {
        format!("Display {}", index + 1)
    }
}

/// Displays and on-screen windows (with a title, normal window layer).
pub fn devices() -> Result<(Vec<DisplayInfo>, Vec<WindowInfo>), CaptureError> {
    let content = shareable_content()?;
    let mut displays = Vec::new();
    // SAFETY: `content` is a valid SCShareableContent; the arrays and their elements are
    // retained for the duration of the loop.
    unsafe {
        for (i, d) in content.displays().iter().enumerate() {
            let id = d.displayID();
            let pts = (u32::try_from(d.width()).unwrap_or(0), u32::try_from(d.height()).unwrap_or(0));
            let (width, height) = display_pixels(id, pts);
            displays.push(DisplayInfo { id: id.to_string(), name: display_name(id, i), width, height });
        }
    }
    let mut windows = Vec::new();
    // SAFETY: as above.
    unsafe {
        for w in content.windows().iter() {
            if w.windowLayer() != 0 || !w.isOnScreen() {
                continue;
            }
            let title = w.title().map(|t| t.to_string()).unwrap_or_default();
            if title.trim().is_empty() {
                continue;
            }
            let app = w.owningApplication().map(|a| a.applicationName().to_string()).unwrap_or_default();
            windows.push(WindowInfo { id: w.windowID().to_string(), title, app });
            if windows.len() >= 200 {
                break;
            }
        }
    }
    Ok((displays, windows))
}

/// State shared with the stream output object (callbacks on ScreenCaptureKit's queue).
pub(super) struct Shared {
    pub sink: FrameSink,
    pub map: HostClockMap,
    /// System audio: its sink and its own clock mapping (audio and video times are each
    /// monotonic on their own).
    pub audio: Option<(AudioSink, HostClockMap)>,
    /// Set at stop: frames that still arrive are dropped.
    pub stopped: AtomicBool,
    pub error: Mutex<Option<String>>,
}

impl Shared {
    pub(super) fn fail(&self, e: String) {
        let mut g = self.error.lock().unwrap_or_else(PoisonError::into_inner);
        if g.is_none() {
            *g = Some(e);
        }
    }
}

/// The current host time (the clock ScreenCaptureKit and AVFoundation stamp samples with), ns.
pub(super) fn host_now_ns() -> i128 {
    // SAFETY: the host time clock is a process-wide singleton; reading it has no preconditions.
    let t = unsafe { CMClock::host_time_clock().time() };
    time_ns(t.value, t.timescale).unwrap_or(0)
}

/// Unlocks a pixel buffer's base address when dropped.
struct Locked<'a>(&'a CVImageBuffer);

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: `copy_frame` locked the buffer with the same flags and it is still borrowed.
        unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly) };
    }
}

/// Copy a BGRA sample buffer into a [`CapturedFrame`] (None: no picture, an idle frame).
pub(super) fn copy_frame(sb: &CMSampleBuffer, map: &HostClockMap) -> Result<Option<CapturedFrame>, String> {
    // SAFETY: `sb` is a valid sample buffer for the duration of the callback.
    let Some(image) = (unsafe { sb.image_buffer() }) else { return Ok(None) };
    // SAFETY: as above.
    let pts: CMTime = unsafe { sb.presentation_time_stamp() };
    if !pts.flags.contains(CMTimeFlags::Valid) {
        return Ok(None);
    }
    let pb: &CVImageBuffer = &image;
    let fmt = CVPixelBufferGetPixelFormatType(pb);
    if fmt != BGRA {
        return Err(format!("unexpected captured pixel format {fmt:#x}"));
    }
    let (w, h) = (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb));
    // SAFETY: `pb` is a valid pixel buffer (retained by `image`).
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return Err("cannot lock the captured picture".into());
    }
    let lock = Locked(pb);
    let base = CVPixelBufferGetBaseAddress(pb) as *const u8;
    let stride = CVPixelBufferGetBytesPerRow(pb);
    if base.is_null() || w == 0 || h == 0 || w > 16_384 || h > 16_384 || stride < w * 4 {
        return Err(format!("captured picture is not mapped ({w}x{h}, stride {stride})"));
    }
    let len = stride.checked_mul(h).ok_or("captured picture size overflows")?;
    // SAFETY: the base address is locked until `lock` drops (after the copy); CoreVideo maps
    // `bytes_per_row * height` bytes of a non-planar buffer from its base address.
    let data = unsafe { std::slice::from_raw_parts(base, len) }.to_vec();
    drop(lock);
    let time_ns = map.map(time_ns(pts.value, pts.timescale).unwrap_or(0));
    Ok(Some(CapturedFrame { width: w as u32, height: h as u32, stride, format: PixelFormat::Bgra8, data, time_ns }))
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `StreamOutput` does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "FilmCraftScreenStreamOutput"]
    #[ivars = Arc<Shared>]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output(&self, _stream: &SCStream, sample_buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
            let shared = self.ivars();
            let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
                if shared.stopped.load(Ordering::Acquire) {
                    return;
                }
                if kind == SCStreamOutputType::Audio {
                    if let Some((sink, map)) = &shared.audio {
                        match copy_audio(sample_buffer, map) {
                            Ok(Some(a)) => sink(a),
                            Ok(None) => {}
                            Err(e) => shared.fail(e),
                        }
                    }
                    return;
                }
                if kind != SCStreamOutputType::Screen {
                    return;
                }
                match copy_frame(sample_buffer, &shared.map) {
                    Ok(Some(f)) => (shared.sink)(f),
                    Ok(None) => {}
                    Err(e) => shared.fail(e),
                }
            }));
            if r.is_err() {
                shared.fail("panic while reading a captured screen frame".into());
            }
        }
    }

    unsafe impl SCStreamDelegate for StreamOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop(&self, _stream: &SCStream, error: &NSError) {
            let shared = self.ivars();
            let msg = std::panic::catch_unwind(AssertUnwindSafe(|| error.localizedDescription().to_string())).unwrap_or_else(|_| "unknown error".into());
            if !shared.stopped.load(Ordering::Acquire) {
                shared.fail(format!("macOS stopped the screen recording: {msg}"));
            }
        }
    }
);

impl StreamOutput {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(shared);
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

/// Wait for a ScreenCaptureKit completion handler `(NSError?)`.
fn run_with_completion(what: &str, call: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>)) -> Result<(), CaptureError> {
    let (tx, rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let block = RcBlock::new(move |error: *mut NSError| {
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| if error.is_null() { Ok(()) } else { Err(ns_error(error)) }));
        let _ = tx.try_send(r.unwrap_or_else(|_| Err("panic in a completion handler".into())));
    });
    call(&block);
    match rx.recv_timeout(OS_TIMEOUT) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(failed(format!("{what}: {e}"))),
        Err(_) => Err(failed(format!("{what}: macOS did not answer within 5 s"))),
    }
}

struct Running {
    stream: Retained<SCStream>,
    output: Retained<StreamOutput>,
    _queue: DispatchRetained<DispatchQueue>,
    shared: Arc<Shared>,
}

/// One display or window being recorded.
pub struct ScreenInput {
    target: ScreenTarget,
    running: Option<Sendable<Running>>,
    error: Option<Arc<Shared>>,
    /// System audio asked for: rate, channels, sink.
    audio: Option<(u32, u16, AudioSink)>,
}

impl ScreenInput {
    pub fn new(target: ScreenTarget) -> Result<Self, CaptureError> {
        require_permission()?;
        Ok(Self { target, running: None, error: None, audio: None })
    }
}

/// `kAudioFormatFlagIsFloat`, `kAudioFormatFlagIsNonInterleaved`.
const AUDIO_FLOAT: u32 = 1;
const AUDIO_NON_INTERLEAVED: u32 = 1 << 5;

/// Copy a system-audio sample buffer (32-bit float, planar or interleaved) into a
/// [`CapturedAudio`] (None: nothing usable in it).
fn copy_audio(sb: &CMSampleBuffer, map: &HostClockMap) -> Result<Option<CapturedAudio>, String> {
    // SAFETY: `sb` is a valid sample buffer for the duration of the callback.
    let n = unsafe { sb.num_samples() };
    if n <= 0 {
        return Ok(None);
    }
    let n = usize::try_from(n).map_err(|_| "audio sample count overflows")?.min(1 << 20);
    // SAFETY: as above.
    let pts: CMTime = unsafe { sb.presentation_time_stamp() };
    if !pts.flags.contains(CMTimeFlags::Valid) {
        return Ok(None);
    }
    // SAFETY: as above; the format description is retained while used.
    let Some(desc) = (unsafe { sb.format_description() }) else { return Ok(None) };
    // SAFETY: `desc` is a valid format description; the call returns null for a non-audio one,
    // else a pointer into `desc`, which outlives its use below.
    let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&desc) };
    if asbd.is_null() {
        return Ok(None);
    }
    // SAFETY: checked non-null above; points into `desc` (alive).
    let asbd = unsafe { &*asbd };
    let (rate, ch, flags, bits) = (asbd.mSampleRate, asbd.mChannelsPerFrame, asbd.mFormatFlags, asbd.mBitsPerChannel);
    if flags & AUDIO_FLOAT == 0 || bits != 32 || ch == 0 || ch > 8 || !rate.is_finite() || !(1000.0..=384_000.0).contains(&rate) {
        return Err(format!("unexpected system audio format ({rate} Hz, {ch} channels, {bits} bits, flags {flags:#x})"));
    }
    let ch = ch as usize;
    // SAFETY: as above; the block buffer is retained while used.
    let Some(block) = (unsafe { sb.data_buffer() }) else { return Ok(None) };
    // SAFETY: `block` is a valid block buffer.
    let len = unsafe { block.data_length() };
    let want = n.saturating_mul(ch).saturating_mul(4);
    if len < want {
        return Err(format!("system audio buffer holds {len} bytes, not {want}"));
    }
    let mut bytes = vec![0u8; want];
    let Some(dst) = std::ptr::NonNull::new(bytes.as_mut_ptr().cast::<std::ffi::c_void>()) else { return Ok(None) };
    // SAFETY: `dst` points to `want` writable bytes and `want <= data_length`.
    let status = unsafe { block.copy_data_bytes(0, want, dst) };
    if status != 0 {
        return Err(format!("cannot read the system audio ({status})"));
    }
    let samples: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let channels: Vec<Vec<f32>> = if flags & AUDIO_NON_INTERLEAVED != 0 {
        samples.chunks_exact(n).take(ch).map(<[f32]>::to_vec).collect()
    } else {
        (0..ch).map(|c| samples.iter().skip(c).step_by(ch).copied().collect()).collect()
    };
    let time_ns = map.map(time_ns(pts.value, pts.timescale).unwrap_or(0));
    Ok(Some(CapturedAudio { sample_rate: rate.round() as u32, channels, time_ns }))
}

/// An even size of at most [`MAX_WIDTH`] wide with the aspect of `w × h`.
fn fit(w: u32, h: u32) -> (u32, u32) {
    let (w, h) = (w.max(16), h.max(16));
    let (w, h) = if w > MAX_WIDTH { (MAX_WIDTH, ((u64::from(h) * u64::from(MAX_WIDTH)) / u64::from(w)) as u32) } else { (w, h) };
    (w.clamp(16, 8192) & !1, h.clamp(16, 8192) & !1)
}

impl VideoInput for ScreenInput {
    fn start(&mut self, req: &VideoRequest, clock: RecordClock, sink: FrameSink) -> Result<VideoFormat, CaptureError> {
        if self.running.is_some() {
            return Err(failed("the screen is already being recorded"));
        }
        let content = shareable_content()?;
        let fps = req.fps.clamp(1, 60);
        // SAFETY: `content` is valid; every object created here is retained by `Running` (or
        // dropped on error) and only used from this thread until the stream starts, after which
        // ScreenCaptureKit calls the output object on its own serial queue.
        unsafe {
            let (filter, (pw, ph)) = match &self.target {
                ScreenTarget::Display(id) => {
                    let d = content
                        .displays()
                        .iter()
                        .find(|d| d.displayID().to_string() == *id)
                        .ok_or_else(|| CaptureError::new(CaptureErrorKind::NoDevice, format!("no display `{id}`")))?;
                    let pts = (u32::try_from(d.width()).unwrap_or(1280), u32::try_from(d.height()).unwrap_or(720));
                    let size = display_pixels(d.displayID(), pts);
                    (SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &d, &NSArray::new()), size)
                }
                ScreenTarget::Window(id) => {
                    let w = content
                        .windows()
                        .iter()
                        .find(|w| w.windowID().to_string() == *id)
                        .ok_or_else(|| CaptureError::new(CaptureErrorKind::NoDevice, format!("no window `{id}` (it may have closed)")))?;
                    let f = w.frame();
                    // windows are measured in points: capture at the main display's pixel scale
                    let main = CGMainDisplayID();
                    let (mpw, _) = display_pixels(main, (0, 0));
                    let mpts = content.displays().iter().find(|d| d.displayID() == main).map(|d| d.width()).unwrap_or(0);
                    let scale = if mpts > 0 && mpw > 0 { f64::from(mpw) / mpts as f64 } else { 2.0 };
                    let size = ((f.size.width * scale).round().max(16.0) as u32, (f.size.height * scale).round().max(16.0) as u32);
                    (SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &w), size)
                }
            };
            let (w, h) = fit(req.width.unwrap_or(pw), req.height.unwrap_or(ph));
            // Settings ▸ Recording ▸ Resolution: ScreenCaptureKit scales to the configured size
            let (w, h) = req.max_height.and_then(|mh| filmcraft_engine::record_settings::downscale((w, h), mh)).unwrap_or((w, h));
            let config = SCStreamConfiguration::new();
            config.setWidth(w as usize);
            config.setHeight(h as usize);
            config.setPixelFormat(BGRA);
            config.setMinimumFrameInterval(CMTime { value: 1, timescale: fps as i32, flags: CMTimeFlags::Valid, epoch: 0 });
            config.setQueueDepth(5);
            config.setShowsCursor(req.show_cursor);
            let audio_sink = match &self.audio {
                Some((rate, ch, sink)) => {
                    config.setCapturesAudio(true);
                    config.setSampleRate(*rate as isize);
                    config.setChannelCount(*ch as isize);
                    config.setExcludesCurrentProcessAudio(true);
                    Some(sink.clone())
                }
                None => None,
            };
            let (anchor_clock, anchor_host) = (clock.now_ns(), host_now_ns());
            let shared = Arc::new(Shared {
                sink,
                map: HostClockMap::new(anchor_clock, anchor_host),
                audio: audio_sink.map(|s| (s, HostClockMap::new(anchor_clock, anchor_host))),
                stopped: AtomicBool::new(false),
                error: Mutex::new(None),
            });
            let output = StreamOutput::new(shared.clone());
            let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, Some(ProtocolObject::from_ref(&*output)));
            let queue = DispatchQueue::new("org.filmcraft.capture.screen", None);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Screen, Some(&queue))
                .map_err(|e| failed(format!("cannot receive screen frames: {}", e.localizedDescription())))?;
            if shared.audio.is_some() {
                stream
                    .addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Audio, Some(&queue))
                    .map_err(|e| failed(format!("cannot receive the system audio: {}", e.localizedDescription())))?;
            }
            run_with_completion("starting the screen recording", |b| stream.startCaptureWithCompletionHandler(Some(b)))?;
            self.error = Some(shared.clone());
            self.running = Some(Sendable(Running { stream, output, _queue: queue, shared }));
            Ok(VideoFormat { width: w, height: h, fps, format: PixelFormat::Bgra8 })
        }
    }

    fn stop(&mut self) {
        let Some(Sendable(r)) = self.running.take() else { return };
        r.shared.stopped.store(true, Ordering::Release);
        // SAFETY: the stream and output object are retained by `r` until the end of this
        // function; stopping a started stream has no other preconditions.
        let stopped = run_with_completion("stopping the screen recording", |b| unsafe { r.stream.stopCaptureWithCompletionHandler(Some(b)) });
        if let Err(e) = stopped {
            log::warn!("{e}");
        }
        // SAFETY: as above; removing the output we added.
        let _ = unsafe { r.stream.removeStreamOutput_type_error(ProtocolObject::from_ref(&*r.output), SCStreamOutputType::Screen) };
        if r.shared.audio.is_some() {
            // SAFETY: as above; removing the audio output we added.
            let _ = unsafe { r.stream.removeStreamOutput_type_error(ProtocolObject::from_ref(&*r.output), SCStreamOutputType::Audio) };
        }
    }

    fn error(&self) -> Option<String> {
        self.error.as_ref().and_then(|s| s.error.lock().unwrap_or_else(PoisonError::into_inner).clone())
    }

    fn capture_audio(&mut self, sample_rate: u32, channels: u16, sink: AudioSink) -> bool {
        // `capturesAudio` exists from macOS 13 on
        // SAFETY: creating an empty stream configuration has no preconditions.
        let config = unsafe { SCStreamConfiguration::new() };
        if !config.respondsToSelector(sel!(setCapturesAudio:)) {
            return false;
        }
        // ScreenCaptureKit's rates: 8, 16, 24 or 48 kHz; the file takes what it delivers
        let rate = if [8_000, 16_000, 24_000, 48_000].contains(&sample_rate) { sample_rate } else { 48_000 };
        self.audio = Some((rate, channels.clamp(1, 2), sink));
        true
    }
}

impl Drop for ScreenInput {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_fit() {
        assert_eq!(fit(5120, 2880), (3840, 2160));
        assert_eq!(fit(1921, 1081), (1920, 1080));
        assert_eq!(fit(3, 3), (16, 16));
    }

    #[test]
    fn permission_check_shows_no_prompt() {
        // preflight only: whatever the answer, nothing is requested
        let p = permission();
        assert!(matches!(p, Permission::Granted | Permission::Denied));
        assert!(host_now_ns() > 0);
    }
}
