//! The microphone level before a recording (`record.monitor`): the Record panel's meter moves as
//! soon as a microphone is chosen, not only once recording. User docs: `docs/recording.md`.
//!
//! Each monitored microphone runs on an input of its own, made with [`AudioInput::spawn`] from
//! the session's input (never the session's own, which the voice-over and the first microphone
//! of a recording use; an input that cannot be opened twice simply has no monitor). A small
//! thread per microphone reads what the device captured about every 30 ms, keeps the peak for
//! `record.status` (`monitor: [{device, level, error?}]`) and drops the samples: nothing is
//! written.
//!
//! The list asked for is remembered ([`Recorder::monitor_wanted`]). `record.start` stops the
//! monitors before it opens its microphones, a voice-over too, and they come back when the
//! recording has stopped (or failed to start) and they are still wanted ([`apply`]).

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad};
use crate::record::{MAX_PER_KIND, Recorder};
use crate::voiceover::{AudioInput, SyntheticInput};
use crate::{Result, Session};

/// How `record.monitor` and `record.status` name the default input.
pub const DEFAULT: &str = "default";

/// How often a monitor reads its device.
const PERIOD: std::time::Duration = std::time::Duration::from_millis(30);

/// How often a monitor drops whatever its device still holds and counts from now again.
const RESYNC: std::time::Duration = std::time::Duration::from_secs(60);

/// A microphone whose level is being read.
pub struct MicMonitor {
    /// As asked for: a device name, or [`DEFAULT`].
    pub device: String,
    /// Peak of the last read (f32 bits).
    level: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MicMonitor {
    /// The peak of the last 30 ms or so, 0 to 1.
    pub fn level(&self) -> f32 {
        let l = f32::from_bits(self.level.load(Ordering::Relaxed));
        if l.is_finite() { l.clamp(0.0, 1.0) } else { 0.0 }
    }
    /// Why the microphone could not be read (a permission, a device that went away).
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    /// Stop reading and close the device (waits for its thread: at most a read period and the
    /// device's own stop).
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for MicMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What a monitor thread needs.
struct Job {
    input: Box<dyn AudioInput>,
    /// The device to open (`""` = the system's default input).
    open: String,
    sample_rate: u32,
    /// The channel a mono recording takes, and whether recordings are stereo (the meter shows
    /// what would be recorded).
    channel: usize,
    stereo: bool,
    level: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
}

/// Read the device until told to stop: every [`PERIOD`] what it captured since the last read,
/// the peak kept, the samples dropped.
fn run(job: Job) -> std::result::Result<(), String> {
    let Job { mut input, open, sample_rate, channel, stereo, level, stop } = job;
    let fmt = input.start(&open, sample_rate)?;
    let rate = u64::from(fmt.sample_rate.max(1));
    let mut began = web_time::Instant::now();
    let mut read = 0u64;
    let mut outcome = Ok(());
    while !stop.load(Ordering::Acquire) {
        std::thread::sleep(PERIOD);
        if began.elapsed() > RESYNC {
            // a device whose clock runs a little fast never piles samples up
            input.discard();
            began = web_time::Instant::now();
            read = 0;
            continue;
        }
        // what a live device has captured by now (a synthetic one generates what is asked for)
        let due = (began.elapsed().as_nanos().saturating_mul(u128::from(rate)) / 1_000_000_000).min(u128::from(u64::MAX)) as u64;
        let want = due.saturating_sub(read).min(rate);
        if want == 0 {
            continue;
        }
        let got = input.read(want as usize);
        if let Some(e) = input.error() {
            outcome = Err(e);
            break;
        }
        let picked: Vec<&Vec<f32>> = if stereo { got.iter().take(2).collect() } else { got.get(channel).or(got.first()).into_iter().collect() };
        let n = picked.iter().map(|c| c.len()).min().unwrap_or(0);
        // a device that fell behind is not waited for: the count follows the clock
        read = if n as u64 == want { read.saturating_add(want) } else { due };
        if n > 0 {
            let peak = picked.iter().flat_map(|c| c.iter()).fold(0f32, |m, v| if v.is_finite() { m.max(v.abs()) } else { m });
            level.store(peak.to_bits(), Ordering::Relaxed);
        }
    }
    input.stop();
    level.store(0f32.to_bits(), Ordering::Relaxed);
    outcome
}

/// Start reading `device` ([`DEFAULT`] or a name) on `input`.
fn start(s: &Session, device: &str, input: Box<dyn AudioInput>) -> MicMonitor {
    let open = if device == DEFAULT {
        // the default of a recording's microphone row
        let vo = &s.prefs.voice_over;
        if vo.source.is_empty() { s.prefs.audio_hardware.default_input.clone() } else { vo.source.clone() }
    } else {
        device.to_string()
    };
    let level = Arc::new(AtomicU32::new(0));
    let error: Arc<Mutex<Option<String>>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let job = Job {
        input,
        open,
        sample_rate: s.prefs.recording.sample_rate,
        channel: s.prefs.voice_over.input_channel as usize,
        stereo: s.prefs.recording.stereo(),
        level: level.clone(),
        stop: stop.clone(),
    };
    let e = error.clone();
    let spawned = std::thread::Builder::new().name("filmcraft-record-monitor".into()).spawn(move || {
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| run(job))).unwrap_or_else(|_| Err("the microphone monitor crashed".into()));
        if let Err(why) = r {
            *e.lock().unwrap_or_else(PoisonError::into_inner) = Some(why);
        }
    });
    let thread = match spawned {
        Ok(t) => Some(t),
        Err(why) => {
            *error.lock().unwrap_or_else(PoisonError::into_inner) = Some(format!("cannot start the monitor thread: {why}"));
            None
        }
    };
    MicMonitor { device: device.to_string(), level, error, stop, thread }
}

/// Stop every monitor now (their devices are closed when this returns). The list asked for is
/// kept: [`apply`] starts them again.
pub fn stop_all(r: &mut Recorder) {
    for m in &mut r.monitors {
        m.stop();
    }
    r.monitors.clear();
}

/// Make the running monitors what is wanted now: none while a recording runs or starts or a
/// voice-over records (they use the microphones), else the list asked for.
pub fn apply(s: &mut Session) {
    let busy = s.record.active.is_some() || s.record.starting.is_some() || s.voiceover.recording();
    if busy || s.record.monitor_wanted.is_empty() {
        if !s.record.monitors.is_empty() {
            stop_all(&mut s.record);
        }
        return;
    }
    let wanted = s.record.monitor_wanted.clone();
    // the ones no longer wanted stop
    let mut keep = Vec::new();
    for mut m in std::mem::take(&mut s.record.monitors) {
        if wanted.contains(&m.device) {
            keep.push(m);
        } else {
            m.stop();
        }
    }
    s.record.monitors = keep;
    for device in &wanted {
        if s.record.monitors.iter().any(|m| &m.device == device) {
            continue;
        }
        // an input of its own; one that cannot be opened twice has no monitor
        let Some(input) = s.voiceover.input.get_or_insert_with(|| Box::new(SyntheticInput::clicks())).spawn() else { continue };
        let m = start(s, device, input);
        s.record.monitors.push(m);
    }
    // in the order asked for
    s.record.monitors.sort_by_key(|m| wanted.iter().position(|w| w == &m.device).unwrap_or(usize::MAX));
}

/// The loudest monitored microphone now, 0 to 1 (None: no monitor runs).
pub fn level(s: &Session) -> Option<f32> {
    s.record.monitors.iter().map(MicMonitor::level).reduce(f32::max)
}

/// `monitor` of `record.status`: `[{device, level, error?}]`.
pub fn status(s: &Session) -> Value {
    Value::Array(
        s.record
            .monitors
            .iter()
            .map(|m| {
                let mut v = json!({"device": m.device, "level": m.level()});
                if let (Some(o), Some(e)) = (v.as_object_mut(), m.error()) {
                    o.insert("error".into(), json!(e));
                }
                v
            })
            .collect(),
    )
}

/// `record.monitor {mics: [name, ...]}`: read the level of exactly these microphones (`""` or
/// `"default"` = the default input; `[]` or `null` stops; without `mics` it only reports).
fn monitor(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "record.monitor";
    if cfg!(target_arch = "wasm32") {
        return Err(bad(cmd, "recording is not available in the web app"));
    }
    let wanted = match p.get("mics") {
        None => None,
        Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(list)) => {
            if list.len() > MAX_PER_KIND {
                return Err(bad(cmd, format!("at most {MAX_PER_KIND} `mics`, got {}", list.len())));
            }
            let names = crate::record::microphone_names(s);
            let mut wanted: Vec<String> = Vec::new();
            for v in list {
                let Some(name) = v.as_str() else {
                    return Err(bad(cmd, format!("each of `mics` must be a device name, got {v}")));
                };
                let device = if name.is_empty() || name == DEFAULT { DEFAULT.to_string() } else { name.to_string() };
                if device != DEFAULT && !names.iter().any(|n| n == &device) {
                    return Err(bad(cmd, format!("no microphone `{device}`")));
                }
                if !wanted.contains(&device) {
                    wanted.push(device);
                }
            }
            Some(wanted)
        }
        Some(_) => return Err(bad(cmd, "`mics` must be a list of device names")),
    };
    if let Some(w) = wanted {
        s.record.monitor_wanted = w;
        apply(s);
    }
    Ok(json!({"wanted": s.record.monitor_wanted, "monitor": status(s)}))
}

/// The `record.monitor` command (listed with the other `record.*` commands).
pub fn monitor_spec() -> CommandSpec {
    CommandSpec {
        id: "record.monitor",
        label: "Microphone Level",
        menu: &[],
        shortcut: None,
        params: r#"{"mics":[name|""|"default"]|null}?"#,
        enabled: crate::commands::always,
        run: monitor,
        journal: false,
    }
}
