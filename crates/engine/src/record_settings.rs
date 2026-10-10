//! Recording settings: Settings ▸ Recording, the Settings section of the Record panel and the
//! `record.settings` command all read and write [`RecordingSettings`] (persisted in the
//! preferences as `recording`, keys `recording.<field>`). `record.start` takes them as its
//! defaults; explicit parameters win. Reference: `docs/recording.md` § Settings.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::bad;
use crate::{EngineError, Result, Session};

pub const SCREEN_FPS: &[(&str, &str)] = &[("15", "15 fps"), ("24", "24 fps"), ("30", "30 fps"), ("60", "60 fps")];
pub const SCREEN_RESOLUTION: &[(&str, &str)] = &[("native", "Native"), ("1440p", "1440p"), ("1080p", "1080p"), ("720p", "720p")];
pub const CAMERA_QUALITY: &[(&str, &str)] = &[("720p", "720p"), ("1080p", "1080p"), ("4k", "4K"), ("native", "Native (the camera's best)")];
pub const CAMERA_FPS: &[(&str, &str)] = &[("24", "24 fps"), ("30", "30 fps"), ("60", "60 fps")];
pub const CAMERA_ROTATE: &[(&str, &str)] =
    &[("auto", "Auto (the camera's own orientation)"), ("0", "0°"), ("90", "90° clockwise"), ("180", "180°"), ("270", "90° counter-clockwise (270°)")];
pub const CODECS: &[(&str, &str)] = &[("h264", "H.264"), ("hevc", "HEVC (H.265, hardware only)"), ("prores", "Apple ProRes 422 (large, edit-friendly)")];
pub const QUALITIES: &[(&str, &str)] = &[("low", "Low"), ("medium", "Medium"), ("high", "High"), ("max", "Max")];
pub const KEYFRAMES: &[(&str, &str)] = &[("1", "1 s"), ("2", "2 s (scrubs best in an editor)"), ("4", "4 s")];
pub const SAMPLE_RATES: &[(&str, &str)] = &[("44100", "44.1 kHz"), ("48000", "48 kHz"), ("96000", "96 kHz")];
pub const CHANNELS: &[(&str, &str)] = &[("mono", "Mono"), ("stereo", "Stereo")];
pub const AUDIO_FORMATS: &[(&str, &str)] = &[("s16", "16-bit WAV"), ("s24", "24-bit WAV"), ("f32", "32-bit float WAV")];
pub const COUNTDOWNS: &[(&str, &str)] = &[("0", "Off"), ("3", "3 s"), ("5", "5 s"), ("10", "10 s")];
/// Longest "Stop after" (minutes); 0 = off.
pub const MAX_STOP_AFTER_MINUTES: u32 = 180;

/// Settings ▸ Recording (`recording.*` in the preferences).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordingSettings {
    // Screen
    /// 15 / 24 / 30 / 60.
    pub screen_fps: u32,
    /// `native` / `1440p` / `1080p` / `720p`: a downscale of the capture, keeping its aspect.
    pub screen_resolution: String,
    pub show_cursor: bool,
    /// The sound the system plays, to `… - System Audio.wav` (only with a screen source).
    pub system_audio: bool,
    // Camera (defaults of new camera rows)
    /// `720p` / `1080p` / `4k` / `native`.
    pub camera_quality: String,
    /// 24 / 30 / 60.
    pub camera_fps: u32,
    pub camera_mirror: bool,
    /// New camera rows' Rotate: Auto (the camera's own orientation) or 0 / 90 / 180 / 270
    /// degrees clockwise, written into the file's track header (nothing is added to the clip).
    pub camera_rotate: CameraRotate,
    // Encoding
    /// `h264` / `hevc` / `prores`.
    pub codec: String,
    /// `low` / `medium` / `high` / `max` (H.264 / HEVC bitrate; ProRes ignores it).
    pub quality: String,
    /// 1 / 2 / 4 seconds between keyframes.
    pub keyframe_seconds: u32,
    /// The system's hardware encoder (VideoToolbox) when there is one; off = FilmCraft's own.
    pub hardware_encoder: bool,
    // Audio
    /// 44100 / 48000 / 96000 (the device's own rate when it cannot do this one).
    pub sample_rate: u32,
    /// `mono` / `stereo`.
    pub channels: String,
    /// `s16` / `s24` / `f32` WAV.
    pub audio_format: String,
    /// Normalise the written microphone files toward −18 dBFS (never the level meter).
    pub auto_gain: bool,
    // Behaviour
    /// 0 / 3 / 5 / 10 seconds of countdown before the Record panel starts.
    pub countdown_seconds: u32,
    /// Stop by itself after this many minutes (0 = off; 1–180).
    pub stop_after_minutes: u32,
    /// Open the new sequence after Stop.
    pub open_sequence: bool,
    /// Folder for the files ("" = Project Settings ▸ Scratch Disks ▸ Captured Audio and Video,
    /// else next to the project, else `Recordings` in the data directory).
    pub output_folder: String,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            screen_fps: 30,
            screen_resolution: "native".into(),
            show_cursor: true,
            system_audio: false,
            camera_quality: "1080p".into(),
            camera_fps: 30,
            camera_mirror: false,
            camera_rotate: CameraRotate::Auto,
            codec: "h264".into(),
            quality: "high".into(),
            keyframe_seconds: 2,
            hardware_encoder: true,
            sample_rate: 48_000,
            channels: "mono".into(),
            audio_format: "f32".into(),
            auto_gain: false,
            countdown_seconds: 3,
            stop_after_minutes: 0,
            open_sequence: true,
            output_folder: String::new(),
        }
    }
}

fn is_choice(opts: &[(&str, &str)], v: &str) -> bool {
    opts.iter().any(|o| o.0 == v)
}

impl RecordingSettings {
    /// Put every value back in range (an unknown choice becomes its default).
    pub fn clamp(&mut self) {
        let d = Self::default();
        let num = |opts: &[(&str, &str)], v: u32, def: u32| if is_choice(opts, &v.to_string()) { v } else { def };
        self.screen_fps = num(SCREEN_FPS, self.screen_fps, d.screen_fps);
        self.camera_fps = num(CAMERA_FPS, self.camera_fps, d.camera_fps);
        if !is_choice(CAMERA_ROTATE, &self.camera_rotate.id()) {
            self.camera_rotate = d.camera_rotate;
        }
        self.keyframe_seconds = num(KEYFRAMES, self.keyframe_seconds, d.keyframe_seconds);
        self.sample_rate = num(SAMPLE_RATES, self.sample_rate, d.sample_rate);
        self.countdown_seconds = num(COUNTDOWNS, self.countdown_seconds, d.countdown_seconds);
        self.stop_after_minutes = self.stop_after_minutes.min(MAX_STOP_AFTER_MINUTES);
        let text = |opts: &[(&str, &str)], v: &mut String, def: &str| {
            if !is_choice(opts, v) {
                *v = def.to_string();
            }
        };
        text(SCREEN_RESOLUTION, &mut self.screen_resolution, &d.screen_resolution);
        text(CAMERA_QUALITY, &mut self.camera_quality, &d.camera_quality);
        text(CODECS, &mut self.codec, &d.codec);
        text(QUALITIES, &mut self.quality, &d.quality);
        text(CHANNELS, &mut self.channels, &d.channels);
        text(AUDIO_FORMATS, &mut self.audio_format, &d.audio_format);
        if self.output_folder.chars().any(char::is_control) || self.output_folder.len() > 4096 {
            self.output_folder.clear();
        }
    }

    /// The size a screen of `native` pixels is recorded at (None = its own size): the
    /// resolution's height when the screen is taller, keeping the aspect, even.
    pub fn screen_size(&self, native: (u32, u32)) -> Option<(u32, u32)> {
        downscale(native, resolution_height(&self.screen_resolution)?)
    }

    pub fn camera_size(&self) -> Option<(u32, u32)> {
        camera_size(&self.camera_quality)
    }

    pub fn capture_quality(&self) -> filmcraft_export::recorder::CaptureQuality {
        use filmcraft_export::recorder::CaptureQuality as Q;
        match self.quality.as_str() {
            "low" => Q::Low,
            "medium" => Q::Medium,
            "max" => Q::Max,
            _ => Q::High,
        }
    }

    pub fn capture_codec(&self) -> filmcraft_export::recorder::CaptureCodec {
        use filmcraft_export::recorder::CaptureCodec as C;
        match self.codec.as_str() {
            "hevc" => C::Hevc,
            "prores" => C::ProRes,
            _ => C::H264,
        }
    }

    pub fn stereo(&self) -> bool {
        self.channels == "stereo"
    }
}

/// The height of a screen resolution choice (None = native).
pub fn resolution_height(r: &str) -> Option<u32> {
    match r {
        "1440p" => Some(1440),
        "1080p" => Some(1080),
        "720p" => Some(720),
        _ => None,
    }
}

/// `native` scaled down to `height` rows keeping its aspect (even sizes); None when it is not
/// taller than that.
pub fn downscale(native: (u32, u32), height: u32) -> Option<(u32, u32)> {
    let (w, h) = (native.0.max(1), native.1.max(1));
    if h <= height {
        return None;
    }
    let nw = (u64::from(w) * u64::from(height) / u64::from(h)).clamp(16, 8192) as u32 & !1;
    Some((nw.max(16), height.clamp(16, 8192) & !1))
}

/// A camera's Rotate: Auto follows the orientation the camera reports (0 when it reports none);
/// a fixed value (0 / 90 / 180 / 270 degrees clockwise) wins over the camera's. Serialised as
/// `"auto"` or the number of degrees; reads either (and a number as text).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CameraRotate {
    #[default]
    Auto,
    Fixed(u32),
}

impl CameraRotate {
    /// `auto`, `0`, `90`, `180` or `270` (the choice id).
    pub fn id(self) -> String {
        match self {
            CameraRotate::Auto => "auto".into(),
            CameraRotate::Fixed(d) => d.to_string(),
        }
    }
    /// The choice `id` (None: not one of them).
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim() {
            "auto" => Some(CameraRotate::Auto),
            t => match t.parse::<u32>() {
                Ok(d @ (0 | 90 | 180 | 270)) => Some(CameraRotate::Fixed(d)),
                _ => None,
            },
        }
    }
    /// The degrees the recording is turned: the fixed value, else the camera's (`camera`), else 0.
    pub fn effective(self, camera: Option<u16>) -> u32 {
        let d = match self {
            CameraRotate::Fixed(d) => d,
            CameraRotate::Auto => camera.map_or(0, u32::from),
        };
        if matches!(d, 90 | 180 | 270) { d } else { 0 }
    }
}

impl Serialize for CameraRotate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            CameraRotate::Auto => s.serialize_str("auto"),
            CameraRotate::Fixed(d) => s.serialize_u32(*d),
        }
    }
}

impl<'de> Deserialize<'de> for CameraRotate {
    /// Lenient (preferences and UI state must load): an unknown text is Auto, any whole number is
    /// kept for [`check`] / [`RecordingSettings::clamp`] to refuse or fix.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(match Value::deserialize(d)? {
            Value::Number(n) => n.as_u64().and_then(|x| u32::try_from(x).ok()).map_or(CameraRotate::Auto, CameraRotate::Fixed),
            Value::String(t) => match t.trim().parse::<u32>() {
                Ok(x) => CameraRotate::Fixed(x),
                Err(_) => CameraRotate::Auto,
            },
            _ => CameraRotate::Auto,
        })
    }
}

/// A camera's `rotate` parameter: `"auto"` or 0 / 90 / 180 / 270 (absent: `default`).
pub fn rotate_of(v: &Value, key_owner_cmd: &str, default: CameraRotate) -> Result<CameraRotate> {
    match v.get("rotate").filter(|x| !x.is_null()) {
        None => Ok(default),
        Some(Value::String(t)) if t == "auto" => Ok(CameraRotate::Auto),
        Some(x) => match x.as_u64() {
            Some(r @ (0 | 90 | 180 | 270)) => Ok(CameraRotate::Fixed(r as u32)),
            _ => Err(bad(key_owner_cmd, format!("`rotate` must be \"auto\", 0, 90, 180 or 270, got {x}"))),
        },
    }
}

/// The size asked of a camera for a quality choice (None = the camera's best).
pub fn camera_size(q: &str) -> Option<(u32, u32)> {
    match q {
        "720p" => Some((1280, 720)),
        "1080p" => Some((1920, 1080)),
        "4k" => Some((3840, 2160)),
        _ => None,
    }
}

/// The fields of [`RecordingSettings`] (camelCase), in the order the panel shows them.
pub fn keys() -> Vec<String> {
    match serde_json::to_value(RecordingSettings::default()) {
        Ok(Value::Object(m)) => m.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// Whether HEVC can be recorded here (the hardware encoder reports it).
pub fn hevc_available() -> bool {
    filmcraft_export::available(filmcraft_export::Format::Hevc)
}

/// Merge `patch` (`{field: value}`) into `cur`: every field is checked (unknown field, wrong
/// type, a value that is not one of the choices or out of range: refused naming the field).
pub fn merge(cur: &RecordingSettings, patch: &Value) -> std::result::Result<RecordingSettings, String> {
    let Some(m) = patch.as_object() else { return Err("`set` must be an object of settings".into()) };
    let mut v = serde_json::to_value(cur).map_err(|e| e.to_string())?;
    let obj = v.as_object_mut().ok_or("internal: settings are not an object")?;
    for (k, x) in m {
        let Some(slot) = obj.get_mut(k) else {
            return Err(format!("unknown recording setting `{k}` (one of {})", keys().join(", ")));
        };
        let x = match (&*slot, x) {
            // Rotate is `"auto"` or a number of degrees (checked below)
            (_, x) if k == "cameraRotate" => match x {
                Value::String(t) => CameraRotate::from_id(t).map(|r| json!(r)).ok_or_else(|| format!("`{k}` must be auto, 0, 90, 180 or 270, got {t}"))?,
                x => x.clone(),
            },
            // numeric choices arrive as strings from dialogs and agents
            (Value::Number(_), Value::String(t)) => t.trim().parse::<u64>().map(Value::from).map_err(|_| format!("`{k}` must be a whole number"))?,
            (Value::Number(_), Value::Number(n)) => {
                let f = n.as_f64().filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0).ok_or_else(|| format!("`{k}` must be a whole number"))?;
                Value::from(f as u64)
            }
            (Value::Bool(_), Value::Bool(b)) => Value::Bool(*b),
            (Value::Bool(_), _) => return Err(format!("`{k}` must be true or false")),
            (Value::String(_), Value::String(t)) => Value::String(t.clone()),
            (Value::String(_), _) => return Err(format!("`{k}` must be text")),
            (_, _) => return Err(format!("`{k}` has the wrong type")),
        };
        *slot = x;
    }
    let next: RecordingSettings = serde_json::from_value(v).map_err(|e| e.to_string())?;
    check(&next)?;
    Ok(next)
}

/// Refuse values that [`RecordingSettings::clamp`] would change, naming the field.
pub fn check(r: &RecordingSettings) -> std::result::Result<(), String> {
    let choice = |k: &str, opts: &[(&str, &str)], v: &str| -> std::result::Result<(), String> {
        if is_choice(opts, v) { Ok(()) } else { Err(format!("`{k}` must be one of {}, got {v}", opts.iter().map(|o| o.0).collect::<Vec<_>>().join(", "))) }
    };
    choice("screenFps", SCREEN_FPS, &r.screen_fps.to_string())?;
    choice("screenResolution", SCREEN_RESOLUTION, &r.screen_resolution)?;
    choice("cameraQuality", CAMERA_QUALITY, &r.camera_quality)?;
    choice("cameraFps", CAMERA_FPS, &r.camera_fps.to_string())?;
    choice("cameraRotate", CAMERA_ROTATE, &r.camera_rotate.id())?;
    choice("codec", CODECS, &r.codec)?;
    choice("quality", QUALITIES, &r.quality)?;
    choice("keyframeSeconds", KEYFRAMES, &r.keyframe_seconds.to_string())?;
    choice("sampleRate", SAMPLE_RATES, &r.sample_rate.to_string())?;
    choice("channels", CHANNELS, &r.channels)?;
    choice("audioFormat", AUDIO_FORMATS, &r.audio_format)?;
    choice("countdownSeconds", COUNTDOWNS, &r.countdown_seconds.to_string())?;
    if r.stop_after_minutes > MAX_STOP_AFTER_MINUTES {
        return Err(format!("`stopAfterMinutes` must be 0 (off) or 1–{MAX_STOP_AFTER_MINUTES}, got {}", r.stop_after_minutes));
    }
    if r.output_folder.chars().any(char::is_control) || r.output_folder.len() > 4096 {
        return Err("`outputFolder` is not a usable folder name".into());
    }
    Ok(())
}

fn settings_json(s: &Session) -> Value {
    json!({
        "settings": s.prefs.recording,
        "hevcAvailable": hevc_available(),
        "systemAudioAvailable": crate::record::factory(s).system_audio(),
    })
}

/// `record.settings {get}` / `{set: {...}}`.
pub fn command(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "record.settings";
    if let Some(patch) = p.get("set").filter(|v| !v.is_null()) {
        let next = merge(&s.prefs.recording, patch).map_err(|e| bad(cmd, e))?;
        if next.codec == "hevc" && s.prefs.recording.codec != "hevc" && !hevc_available() {
            return Err(bad(cmd, "`codec` hevc needs a hardware HEVC encoder, and this system has none"));
        }
        let mut prefs = s.prefs.clone();
        prefs.recording = next;
        s.set_prefs(prefs).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    }
    Ok(settings_json(s))
}

// ------------------------------------------------------------------------------------- audio

/// Sample format of a recorded WAV.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavFormat {
    S16,
    S24,
    F32,
}

impl WavFormat {
    pub fn from_id(id: &str) -> Self {
        match id {
            "s16" => WavFormat::S16,
            "s24" => WavFormat::S24,
            _ => WavFormat::F32,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            WavFormat::S16 => "s16",
            WavFormat::S24 => "s24",
            WavFormat::F32 => "f32",
        }
    }
    pub fn bytes(self) -> u16 {
        match self {
            WavFormat::S16 => 2,
            WavFormat::S24 => 3,
            WavFormat::F32 => 4,
        }
    }
    /// `sample` as little-endian bytes (integers clipped to full scale).
    pub fn put(self, sample: f32, out: &mut Vec<u8>) {
        let x = if sample.is_finite() { sample } else { 0.0 };
        match self {
            WavFormat::S16 => out.extend_from_slice(&((x.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()),
            WavFormat::S24 => {
                let v = (x.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                out.extend_from_slice(&v.to_le_bytes()[..3]);
            }
            WavFormat::F32 => out.extend_from_slice(&x.to_le_bytes()),
        }
    }
}

/// A 44-byte WAV header for `data_len` bytes of `channels` × `format` at `rate`.
pub fn wav_header(rate: u32, channels: u16, format: WavFormat, data_len: u32) -> Vec<u8> {
    let ch = channels.max(1);
    let block = ch.saturating_mul(format.bytes());
    let mut v = Vec::with_capacity(44);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&36u32.saturating_add(data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&(if format == WavFormat::F32 { 3u16 } else { 1u16 }).to_le_bytes());
    v.extend_from_slice(&ch.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&rate.saturating_mul(u32::from(block)).to_le_bytes());
    v.extend_from_slice(&block.to_le_bytes());
    v.extend_from_slice(&(format.bytes() * 8).to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v
}

/// Auto gain for the written microphone file: a running RMS (about 0.3 s) steers a gain toward
/// −18 dBFS, changing it by at most 2 dB per second and by at most ±20 dB in all; a block of
/// near-silence (below −60 dBFS) holds the gain. Peaks are limited to full scale.
#[derive(Clone, Debug)]
pub struct AutoGain {
    rate: f64,
    /// Mean square, smoothed.
    ms: f64,
    gain_db: f64,
}

pub const AUTO_GAIN_TARGET_DB: f64 = -18.0;
pub const AUTO_GAIN_RATE_DB_PER_S: f64 = 2.0;
pub const AUTO_GAIN_MAX_DB: f64 = 20.0;

impl AutoGain {
    pub fn new(rate: u32) -> Self {
        Self { rate: f64::from(rate.max(1)), ms: 0.0, gain_db: 0.0 }
    }
    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }
    /// Apply the gain to one block of planar samples (in place, every channel the same gain) and
    /// update it.
    pub fn process(&mut self, block: &mut [Vec<f32>]) {
        let n = block.iter().map(Vec::len).sum::<usize>();
        let len = block.first().map_or(0, Vec::len);
        if n == 0 || len == 0 {
            return;
        }
        let secs = len as f64 / self.rate;
        let mean: f64 = block.iter().flatten().map(|v| if v.is_finite() { f64::from(*v) * f64::from(*v) } else { 0.0 }).sum::<f64>() / n as f64;
        // a block of near-silence (pauses between words) holds the gain and the running level
        if 10.0 * mean.max(1e-12).log10() > -60.0 {
            let a = 1.0 - (-secs / 0.3).exp();
            self.ms += (mean - self.ms) * a;
            let rms_db = 10.0 * self.ms.max(1e-12).log10();
            let want = (AUTO_GAIN_TARGET_DB - rms_db).clamp(-AUTO_GAIN_MAX_DB, AUTO_GAIN_MAX_DB);
            let step = AUTO_GAIN_RATE_DB_PER_S * secs;
            self.gain_db += (want - self.gain_db).clamp(-step, step);
        }
        let g = 10f64.powf(self.gain_db / 20.0) as f32;
        for v in block.iter_mut().flatten() {
            *v = if v.is_finite() { (*v * g).clamp(-1.0, 1.0) } else { 0.0 };
        }
    }
}
