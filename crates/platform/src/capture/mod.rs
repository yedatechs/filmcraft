//! OS media capture for recording (Window ▸ Record; [ADR 0002](../../../../docs/adr/0002-platform-capture-ffi.md)):
//! screens and windows through ScreenCaptureKit ([`screen`]), cameras through AVFoundation
//! ([`camera`]), behind the engine's `filmcraft_engine::record::VideoInputFactory`.
//!
//! This module is safe code: the factory, the permission messages and the mapping of the OS's
//! sample times onto the recording clock. The FFI lives in `screen` and `camera` (macOS only).
//! On other systems nothing is registered and the engine reports screen and camera capture as
//! not available.
//!
//! Permissions (macOS privacy, TCC) are checked before any stream starts and are never waited on:
//! an undetermined permission is requested (the system shows its prompt) and the start fails with
//! a message naming the System Settings pane, so the user answers the prompt and presses Record
//! again.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod camera;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod screen;

use std::sync::atomic::{AtomicU64, Ordering};

use filmcraft_engine::record::{CaptureError, CaptureErrorKind, Permission};

/// The System Settings pane that holds a permission (macOS 13+ names).
pub fn settings_pane(what: Need) -> &'static str {
    match what {
        Need::Screen => "System Settings ▸ Privacy & Security ▸ Screen & System Audio Recording",
        Need::Camera => "System Settings ▸ Privacy & Security ▸ Camera",
        Need::Microphone => "System Settings ▸ Privacy & Security ▸ Microphone",
    }
}

/// A privacy permission recording needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Screen,
    Camera,
    Microphone,
}

impl Need {
    fn label(self) -> &'static str {
        match self {
            Need::Screen => "Screen Recording",
            Need::Camera => "Camera",
            Need::Microphone => "Microphone",
        }
    }
}

/// The error for a permission that is not granted (None when it is).
pub fn permission_error(what: Need, state: Permission) -> Option<CaptureError> {
    let pane = settings_pane(what);
    let label = what.label();
    let restart = if what == Need::Screen { " macOS applies Screen Recording after FilmCraft is restarted." } else { "" };
    let msg = match state {
        Permission::Granted => return None,
        Permission::Undetermined => format!(
            "FilmCraft asked macOS for {label} access: answer the prompt (or allow FilmCraft, or the terminal that launched it, in {pane}), then press Record again.{restart}"
        ),
        Permission::Denied => {
            format!("{label} access is not allowed for FilmCraft (or the terminal that launched it): open {pane}, allow it, then press Record again.{restart}")
        }
        Permission::Unavailable => return Some(CaptureError::unavailable()),
    };
    Some(CaptureError::new(CaptureErrorKind::Permission, msg))
}

/// Nanoseconds of a `value / timescale` seconds time (CMTime), or None when the timescale is not
/// positive or the result does not fit.
pub fn time_ns(value: i64, timescale: i32) -> Option<i128> {
    if timescale <= 0 {
        return None;
    }
    Some(i128::from(value) * 1_000_000_000 / i128::from(timescale))
}

/// Maps the OS's sample times (host clock, nanoseconds) onto the recording clock: anchored once
/// at stream start by reading both clocks back to back; never goes backwards.
#[derive(Debug)]
pub struct HostClockMap {
    anchor_clock_ns: u64,
    anchor_host_ns: i128,
    last: AtomicU64,
}

impl HostClockMap {
    pub fn new(anchor_clock_ns: u64, anchor_host_ns: i128) -> Self {
        Self { anchor_clock_ns, anchor_host_ns, last: AtomicU64::new(0) }
    }
    /// Recording-clock time of a sample stamped `host_ns` (clamped at 0 and to stay after the
    /// previous sample).
    pub fn map(&self, host_ns: i128) -> u64 {
        let t = (i128::from(self.anchor_clock_ns) + (host_ns - self.anchor_host_ns)).clamp(0, i128::from(u64::MAX / 2)) as u64;
        // `last` holds the previous time + 1 (0 = no sample yet)
        let prev = self.last.load(Ordering::Acquire);
        let t = if prev == 0 { t } else { t.max(prev) };
        self.last.store(t.saturating_add(1), Ordering::Release);
        t
    }
}

/// The macOS capture factory: ScreenCaptureKit displays and windows, AVFoundation cameras.
#[cfg(target_os = "macos")]
pub struct MacCaptureFactory;

#[cfg(target_os = "macos")]
impl filmcraft_engine::record::VideoInputFactory for MacCaptureFactory {
    fn name(&self) -> String {
        "ScreenCaptureKit + AVFoundation".into()
    }
    fn devices(&self) -> Result<filmcraft_engine::record::VideoDevices, CaptureError> {
        let mut out = filmcraft_engine::record::VideoDevices::default();
        match screen::devices() {
            Ok((displays, windows)) => {
                out.displays = displays;
                out.windows = windows;
            }
            Err(e) => out.problems.push(format!("screen: {e}")),
        }
        match camera::devices() {
            Ok(c) => out.cameras = c,
            Err(e) => out.problems.push(format!("camera: {e}")),
        }
        Ok(out)
    }
    fn permissions(&self) -> filmcraft_engine::record::Permissions {
        filmcraft_engine::record::Permissions { screen: screen::permission(), camera: camera::permission(false), microphone: camera::permission(true) }
    }
    fn open_screen(&self, target: &filmcraft_engine::record::ScreenTarget) -> Result<Box<dyn filmcraft_engine::record::VideoInput>, CaptureError> {
        Ok(Box::new(screen::ScreenInput::new(target.clone())?))
    }
    fn open_camera(&self, device: &str) -> Result<Box<dyn filmcraft_engine::record::VideoInput>, CaptureError> {
        Ok(Box::new(camera::CameraInput::new(device)?))
    }
}

/// Install the system's capture factory (`filmcraft_platform::register` calls it). Returns
/// whether one was installed.
pub fn register() -> bool {
    #[cfg(target_os = "macos")]
    {
        filmcraft_engine::record::register_video_factory(std::sync::Arc::new(MacCaptureFactory));
        true
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// How long an asynchronous OS call (content enumeration, stream start / stop) may take before
/// it is reported as failed: never a hang.
pub const OS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_messages_name_the_pane() {
        assert!(permission_error(Need::Screen, Permission::Granted).is_none());
        let e = permission_error(Need::Screen, Permission::Denied).unwrap();
        assert_eq!(e.kind, CaptureErrorKind::Permission);
        assert!(e.message.contains("Screen & System Audio Recording"), "{}", e.message);
        assert!(e.message.contains("restarted"), "{}", e.message);
        let e = permission_error(Need::Camera, Permission::Undetermined).unwrap();
        assert!(e.message.contains("Privacy & Security ▸ Camera") && e.message.contains("prompt"), "{}", e.message);
        let e = permission_error(Need::Microphone, Permission::Denied).unwrap();
        assert!(e.message.contains("▸ Microphone"), "{}", e.message);
        assert_eq!(permission_error(Need::Camera, Permission::Unavailable).unwrap().kind, CaptureErrorKind::Unavailable);
    }

    #[test]
    fn host_times_map_monotonically() {
        let m = HostClockMap::new(1_000, 5_000_000_000);
        assert_eq!(m.map(5_000_000_000), 1_000);
        assert_eq!(m.map(5_033_333_333), 33_334_333);
        // a sample stamped before the previous one (or before the clock start) never goes back
        assert_eq!(m.map(5_010_000_000), 33_334_334);
        let early = HostClockMap::new(0, 10);
        assert_eq!(early.map(0), 0);
        assert_eq!(early.map(5), 1);
        assert_eq!(time_ns(3, 2), Some(1_500_000_000));
        assert_eq!(time_ns(3, 0), None);
        assert_eq!(time_ns(-1, 1_000_000_000), Some(-1));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn enumeration_needs_no_permission_and_never_fails_hard() {
        use filmcraft_engine::record::VideoInputFactory;
        // without Screen Recording permission the displays are empty and the reason is listed;
        // no prompt is shown (only `open_screen` asks)
        let d = MacCaptureFactory.devices().unwrap();
        assert!(!d.displays.is_empty() || d.problems.iter().any(|p| p.starts_with("screen")), "{d:?}");
        let p = MacCaptureFactory.permissions();
        assert_ne!(p.screen, Permission::Unavailable);
        assert!(camera::devices().is_ok());
    }

    /// Records one second of the first display through ScreenCaptureKit (run by hand:
    /// `cargo test -p filmcraft-platform live_screen -- --ignored --nocapture`).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "needs Screen Recording permission for the terminal"]
    fn live_screen_frames() {
        use filmcraft_engine::record::{RecordClock, ScreenTarget, VideoInputFactory, VideoRequest};
        use std::sync::atomic::AtomicUsize;
        let d = MacCaptureFactory.devices().unwrap();
        println!("devices: {d:?}");
        let Some(display) = d.displays.first() else {
            println!("no display listed (permission?)");
            return;
        };
        let mut input = MacCaptureFactory.open_screen(&ScreenTarget::Display(display.id.clone())).unwrap();
        let n = std::sync::Arc::new(AtomicUsize::new(0));
        let times = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (n2, t2) = (n.clone(), times.clone());
        let clock = RecordClock::new();
        let fmt = input
            .start(
                &VideoRequest { width: None, height: None, fps: 10 },
                clock,
                std::sync::Arc::new(move |f| {
                    n2.fetch_add(1, Ordering::Relaxed);
                    t2.lock().unwrap().push((f.time_ns, f.width, f.height, f.data.len()));
                }),
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        input.stop();
        println!("format {fmt:?}, frames {}, error {:?}, first {:?}", n.load(Ordering::Relaxed), input.error(), times.lock().unwrap().first());
        assert!(n.load(Ordering::Relaxed) > 0);
        let t = times.lock().unwrap();
        assert!(t.windows(2).all(|w| w[1].0 > w[0].0), "monotonic");
        assert!(t[0].0 < 1_000_000_000, "the first frame is stamped near the clock start: {}", t[0].0);
    }
}
