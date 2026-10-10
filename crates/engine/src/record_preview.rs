//! Live camera preview (`record.preview`): a camera runs without writing anything and its latest
//! picture, downscaled to at most [`PREVIEW_MAX_WIDTH`] pixels wide, is kept for the UI (the
//! Record panel's thumbnails and the pop-out preview window). User docs: `docs/recording.md` §
//! Preview.
//!
//! A camera is captured through one [`CameraTap`] at a time, whoever asked first: a preview, or
//! a recording (`record.start` opens its cameras through [`camera_input`]). The tap's capture
//! callback keeps the preview picture (while previewing) and hands every full-size frame to the
//! recording attached to it (if any), re-stamped from the tap's clock onto the recording's. So a
//! recording of a previewed camera reuses the running device session: no second open, no gap.
//! Only when the recording asks the camera for another size or rate than the preview runs at is
//! the device restarted with the new request (the preview keeps going on the new session).
//!
//! Frames arrive on the capture thread; the UI reads [`CameraTap::latest`] at its own rate (an
//! `Arc` clone, never a copy of the picture).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad};
use crate::record::{
    CaptureError, CaptureErrorKind, CapturedFrame, FrameSink, MAX_PER_KIND, RecordClock, Recorder, VideoFormat, VideoInput, VideoInputFactory, VideoRequest,
    choice_of, fps_of, id_of, size_of,
};
use crate::{EngineError, Result, Session};

/// Title of the pop-out camera preview windows (`… — <camera>`): a screen recording leaves out
/// this process's windows whose title starts with it.
pub const PREVIEW_TITLE: &str = "FilmCraft Camera Preview";

/// Widest preview picture kept for the UI.
pub const PREVIEW_MAX_WIDTH: u32 = 640;

/// The latest preview picture of a camera: straight RGBA, `width × height`.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Capture time, nanoseconds on the tap's clock.
    pub time_ns: u64,
    /// Frames the camera delivered so far (1 for the first).
    pub index: u64,
}

/// The size a `w × h` frame is previewed at: at most [`PREVIEW_MAX_WIDTH`] wide, the aspect kept,
/// even, at least 2 × 2.
pub fn preview_size(w: u32, h: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    if w <= PREVIEW_MAX_WIDTH {
        return ((w & !1).max(2), (h & !1).max(2));
    }
    let nh = (u64::from(h) * u64::from(PREVIEW_MAX_WIDTH) / u64::from(w)).clamp(2, 8192) as u32;
    (PREVIEW_MAX_WIDTH, (nh & !1).max(2))
}

/// `f` as the clip will show it: flipped left–right when `mirror`, then turned clockwise by
/// `rotate` degrees (0 / 90 / 180 / 270; anything else counts as 0).
pub fn oriented(f: &PreviewFrame, mirror: bool, rotate: u32) -> PreviewFrame {
    let (w, h) = (f.width as usize, f.height as usize);
    let rotate = if matches!(rotate, 90 | 180 | 270) { rotate } else { 0 };
    if (!mirror && rotate == 0) || f.rgba.len() < w * h * 4 {
        return f.clone();
    }
    let (ow, oh) = if rotate % 180 == 90 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        for x in 0..ow {
            // the source pixel shown at (x, y): undo the rotation, then the mirror
            let (sx, sy) = match rotate {
                90 => (y, h - 1 - x),
                180 => (w - 1 - x, h - 1 - y),
                270 => (w - 1 - y, x),
                _ => (x, y),
            };
            let sx = if mirror { w - 1 - sx } else { sx };
            let (si, di) = ((sy * w + sx) * 4, (y * ow + x) * 4);
            if let (Some(src), Some(dst)) = (f.rgba.get(si..si + 4), out.get_mut(di..di + 4)) {
                dst.copy_from_slice(src);
            }
        }
    }
    PreviewFrame { width: ow as u32, height: oh as u32, rgba: out, time_ns: f.time_ns, index: f.index }
}

/// A recording attached to a tap: its sink and the nanoseconds from the tap's clock start to the
/// recording's.
type Attached = Option<(FrameSink, u64)>;

struct TapState {
    req: Option<VideoRequest>,
    format: Option<VideoFormat>,
    clock: Option<RecordClock>,
}

/// One camera's capture, shared by its preview and a recording of it.
pub struct CameraTap {
    pub device: String,
    input: Mutex<Box<dyn VideoInput>>,
    state: Mutex<TapState>,
    /// The capture callback handed to the input (kept to restart it).
    sink: FrameSink,
    latest: Arc<Mutex<Option<Arc<PreviewFrame>>>>,
    previewing: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    attached: Arc<Mutex<Attached>>,
}

impl CameraTap {
    fn new(device: &str, input: Box<dyn VideoInput>) -> Arc<Self> {
        let latest: Arc<Mutex<Option<Arc<PreviewFrame>>>> = Arc::default();
        let previewing = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(AtomicU64::new(0));
        let attached: Arc<Mutex<Attached>> = Arc::default();
        let (l, p, n, a) = (latest.clone(), previewing.clone(), frames.clone(), attached.clone());
        let sink: FrameSink = Arc::new(move |f: CapturedFrame| {
            let index = n.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            if p.load(Ordering::Acquire) {
                let (w, h) = preview_size(f.width, f.height);
                let mut rgba = Vec::new();
                if crate::record::to_rgba(&f, w, h, &mut rgba) {
                    *l.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(PreviewFrame { width: w, height: h, rgba, time_ns: f.time_ns, index }));
                }
            }
            // held while the recording's sink runs: after `detach` no frame reaches it
            let g = a.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((sink, delta)) = g.as_ref()
                && f.time_ns >= *delta
            {
                let mut f = f;
                f.time_ns -= *delta;
                sink(f);
            }
        });
        Arc::new(Self {
            device: device.to_string(),
            input: Mutex::new(input),
            state: Mutex::new(TapState { req: None, format: None, clock: None }),
            sink,
            latest,
            previewing,
            frames,
            attached,
        })
    }

    /// The latest preview picture (None before the first frame, or when not previewing).
    pub fn latest(&self) -> Option<Arc<PreviewFrame>> {
        self.latest.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    /// What the camera delivers (None when not running).
    pub fn format(&self) -> Option<VideoFormat> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).format
    }
    /// Frames the camera delivered since it was opened.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
    pub fn previewing(&self) -> bool {
        self.previewing.load(Ordering::Acquire)
    }
    /// Whether a recording takes its frames.
    pub fn recording(&self) -> bool {
        self.attached.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }
    /// A failure of the camera after it started.
    pub fn error(&self) -> Option<String> {
        self.input.lock().unwrap_or_else(PoisonError::into_inner).error()
    }

    /// Start the camera with `req` (stamped on `clock`) if it is not running; restart it when it
    /// runs with another request (`restart` false: keep what runs). Returns the format, the tap's
    /// clock and whether the device was restarted.
    fn ensure_started(&self, req: &VideoRequest, clock: RecordClock, restart: bool) -> std::result::Result<(VideoFormat, RecordClock, bool), CaptureError> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut input = self.input.lock().unwrap_or_else(PoisonError::into_inner);
        if let (Some(fmt), Some(c)) = (st.format, st.clock) {
            if st.req.as_ref() == Some(req) || !restart {
                return Ok((fmt, c, false));
            }
            // another size or rate: one restart of the device, the clock kept
            input.stop();
            st.format = None;
            let fmt = input.start(req, c, self.sink.clone())?;
            st.format = Some(fmt);
            st.req = Some(*req);
            return Ok((fmt, c, true));
        }
        let fmt = input.start(req, clock, self.sink.clone())?;
        *st = TapState { req: Some(*req), format: Some(fmt), clock: Some(clock) };
        Ok((fmt, clock, false))
    }

    /// Stop the device (it can be started again).
    fn stop_input(&self) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.input.lock().unwrap_or_else(PoisonError::into_inner).stop();
        st.format = None;
        st.clock = None;
    }

    fn set_previewing(&self, on: bool) {
        self.previewing.store(on, Ordering::Release);
        if !on {
            *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }
}

/// A recording's view of a [`CameraTap`]: what `record.start` gets for a camera.
struct TapInput {
    tap: Arc<CameraTap>,
    attached: bool,
}

impl VideoInput for TapInput {
    fn start(&mut self, req: &VideoRequest, clock: RecordClock, sink: FrameSink) -> std::result::Result<VideoFormat, CaptureError> {
        if self.attached {
            return Err(CaptureError::new(CaptureErrorKind::Failed, "the camera is already being recorded"));
        }
        let (fmt, tap_clock, restarted) = self.tap.ensure_started(req, clock, true)?;
        if restarted {
            log::info!(
                "camera `{}`: restarted at {}×{} {} fps for the recording (the preview ran with another request)",
                self.tap.device,
                fmt.width,
                fmt.height,
                fmt.fps
            );
        }
        *self.tap.attached.lock().unwrap_or_else(PoisonError::into_inner) = Some((sink, clock.ns_after(&tap_clock)));
        self.attached = true;
        Ok(fmt)
    }
    fn stop(&mut self) {
        if !std::mem::take(&mut self.attached) {
            return;
        }
        *self.tap.attached.lock().unwrap_or_else(PoisonError::into_inner) = None;
        if !self.tap.previewing() {
            self.tap.stop_input();
        }
    }
    fn error(&self) -> Option<String> {
        self.tap.error()
    }
}

impl Drop for TapInput {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The live tap of `device` (forgetting dead ones).
fn live_tap(r: &mut Recorder, device: &str) -> Option<Arc<CameraTap>> {
    r.taps.retain(|w| w.strong_count() > 0);
    r.taps.iter().filter_map(Weak::upgrade).find(|t| t.device == device)
}

fn tap_for(r: &mut Recorder, f: &Arc<dyn VideoInputFactory>, device: &str) -> std::result::Result<Arc<CameraTap>, CaptureError> {
    if let Some(t) = live_tap(r, device) {
        return Ok(t);
    }
    let t = CameraTap::new(device, f.open_camera(device)?);
    r.taps.push(Arc::downgrade(&t));
    Ok(t)
}

/// The input `record.start` records camera `device` from: the running preview's capture when
/// there is one, else a newly opened camera (through a tap, so a preview started during the
/// recording shares it).
pub fn camera_input(r: &mut Recorder, f: &Arc<dyn VideoInputFactory>, device: &str) -> std::result::Result<Box<dyn VideoInput>, CaptureError> {
    Ok(Box::new(TapInput { tap: tap_for(r, f, device)?, attached: false }))
}

/// The cameras being previewed (their ids, in `record.preview` order).
pub fn preview_ids(s: &Session) -> Vec<String> {
    s.record.previews.iter().map(|t| t.device.clone()).collect()
}

/// The preview of camera `device`, if it runs.
pub fn preview_of(s: &Session, device: &str) -> Option<Arc<CameraTap>> {
    s.record.previews.iter().find(|t| t.device == device).cloned()
}

/// One camera of `record.preview`.
struct PreviewPlan {
    device: String,
    req: VideoRequest,
}

fn preview_plans(s: &Session, p: &Value, cmd: &str) -> Result<Option<Vec<PreviewPlan>>> {
    let many = p.get("cameras");
    let one = p.get("camera");
    let list: Vec<&Value> = match (many, one) {
        (Some(_), Some(_)) => return Err(bad(cmd, "give `cameras` or `camera`, not both")),
        (Some(Value::Array(a)), None) => a.iter().collect(),
        (Some(Value::Null), None) | (None, Some(Value::Null)) => Vec::new(),
        (Some(_), None) => return Err(bad(cmd, "`cameras` must be a list")),
        (None, Some(o @ Value::Object(_))) => vec![o],
        (None, Some(_)) => return Err(bad(cmd, "`camera` must be {device: id} or null")),
        (None, None) => return Ok(None),
    };
    if list.len() > MAX_PER_KIND {
        return Err(bad(cmd, format!("at most {MAX_PER_KIND} cameras, got {}", list.len())));
    }
    let rs = &s.prefs.recording;
    let mut out: Vec<PreviewPlan> = Vec::new();
    for v in list {
        if !v.is_object() {
            return Err(bad(cmd, format!("each camera must be an object, got {v}")));
        }
        let device = id_of(v, "device").ok_or_else(|| bad(cmd, "a camera takes {device: id}"))?;
        if out.iter().any(|c| c.device == device) {
            return Err(bad(cmd, format!("the camera `{device}` is chosen twice")));
        }
        let quality = choice_of(v, "quality", cmd, crate::record_settings::CAMERA_QUALITY, &rs.camera_quality)?;
        let (width, height) = match (size_of(v, "width", cmd)?, size_of(v, "height", cmd)?) {
            (None, None) => crate::record_settings::camera_size(&quality).map_or((None, None), |(w, h)| (Some(w), Some(h))),
            wh => wh,
        };
        let req = VideoRequest { width, height, fps: fps_of(v, cmd, rs.camera_fps)?, max_height: None, show_cursor: false };
        out.push(PreviewPlan { device, req });
    }
    Ok(Some(out))
}

fn preview_json(s: &Session) -> Value {
    let cams: Vec<Value> = s
        .record
        .previews
        .iter()
        .map(|t| {
            let f = t.format();
            json!({
                "device": t.device,
                "width": f.map(|f| f.width),
                "height": f.map(|f| f.height),
                "fps": f.map(|f| f.fps),
                "frames": t.frames(),
                "recording": t.recording(),
                "error": t.error(),
            })
        })
        .collect();
    json!({"preview": preview_ids(s), "cameras": cams})
}

/// `record.preview {camera: {device, width?, height?, fps?, quality?} | null}` or
/// `{cameras: [...]}`: the cameras to show live (the others stop); `{}` reports them.
fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "record.preview";
    if cfg!(target_arch = "wasm32") {
        return Err(bad(cmd, "recording is not available in the web app"));
    }
    let Some(plans) = preview_plans(s, p, cmd)? else { return Ok(preview_json(s)) };
    // the ones no longer wanted stop (a camera being recorded keeps running for the recording)
    let old = std::mem::take(&mut s.record.previews);
    for t in old {
        if plans.iter().any(|c| c.device == t.device) {
            s.record.previews.push(t);
            continue;
        }
        t.set_previewing(false);
        if !t.recording() {
            t.stop_input();
        }
    }
    let f = crate::record::factory(s);
    let mut errors = Vec::new();
    let mut next = Vec::new();
    for c in &plans {
        let r = tap_for(&mut s.record, &f, &c.device).and_then(|t| {
            t.set_previewing(true);
            // a camera being recorded is not restarted for its preview
            let restart = !t.recording();
            match t.ensure_started(&c.req, RecordClock::new(), restart) {
                Ok(_) => Ok(t),
                Err(e) => {
                    t.set_previewing(false);
                    Err(e)
                }
            }
        });
        match r {
            Ok(t) => next.push(t),
            Err(e) => errors.push(format!("{}: {e}", c.device)),
        }
    }
    s.record.previews = next;
    if !errors.is_empty() {
        return Err(EngineError::Other(format!("camera preview: {}", errors.join("; "))));
    }
    Ok(preview_json(s))
}

/// The `record.preview` command (listed with the other `record.*` commands).
pub fn preview_spec() -> CommandSpec {
    CommandSpec {
        id: "record.preview",
        label: "Camera Preview",
        menu: &[],
        shortcut: None,
        params: r#"{"camera":{"device":id,"quality":"720p|1080p|4k|native"?,"width":n?,"height":n?,"fps":n?}|null}|{"cameras":[{..}]}"#,
        enabled: crate::commands::always,
        run: preview,
        journal: false,
    }
}
