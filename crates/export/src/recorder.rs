//! A streaming MOV writer for live capture (Record panel, `record.*` in the engine): pictures come
//! in as they are captured, each with the *slot* (frame number at the nominal rate) it is shown
//! from, are encoded through the registered encoder factories (the hardware encoder when asked for
//! and available, else FilmCraft's own H.264 encoder) and are muxed into a QuickTime file whose
//! `mdat` grows on disk while recording; `moov` is written by [`MovRecorder::finish`].
//!
//! Frame times: every sample lasts until the next picture's slot, so a capture that delivers no
//! picture for a while (a still screen) becomes one long sample, and a capture that drops frames
//! has longer samples instead of wrong times. The track timescale is the nominal rate's numerator
//! and one slot lasts its denominator, so durations are whole frames.

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use filmcraft_isobmff::{Brand, Mp4Writer, TrackConfig, WriteSample, WriterOptions};
use filmcraft_time::FrameRate;

use crate::settings::{BitrateMode, H264Profile, HardwareEncoding};
use crate::{EncodedPacket, EncoderFrame, ExportError, ExportSettings, Format, Result, VideoEncoder, hw_encode_stats, video_factories};

/// The H.264 bitrate (kbps) of a capture of `w × h` at `fps`: 12 Mbit/s at 1920 × 1080 scaled by
/// `(pixels / 1080p)^0.87` (≈ 6 at 720p, 40 at 2160p), times `fps / 30` above 30 fps.
pub fn capture_kbps(w: u32, h: u32, fps: u32) -> u32 {
    let px = f64::from(w.max(1)) * f64::from(h.max(1));
    let base = 12_000.0 * (px / (1920.0 * 1080.0)).powf(0.87);
    let rate = (f64::from(fps.max(1)) / 30.0).max(1.0);
    (base * rate).clamp(500.0, 200_000.0).round() as u32
}

/// A live H.264 MOV recording.
pub struct MovRecorder {
    path: PathBuf,
    enc: Box<dyn VideoEncoder>,
    hardware: bool,
    width: u32,
    height: u32,
    rate: FrameRate,
    /// Before the first packet: the open file (the writer needs the encoder's first parameter sets).
    file: Option<BufWriter<File>>,
    mux: Option<Mp4Writer<BufWriter<File>>>,
    track: usize,
    /// Slots of pictures handed to the encoder whose packet has not come out yet (oldest first).
    pending: VecDeque<i64>,
    /// The newest packet, written when the slot of the picture after it is known.
    held: Option<(EncodedPacket, i64)>,
    last_slot: Option<i64>,
    frames: u64,
    bytes: u64,
}

impl MovRecorder {
    /// Start `path` (created / truncated) for `width × height` pictures at the nominal `rate`.
    /// `hardware`: let a registered hardware encoder take it (else FilmCraft's own encoder).
    pub fn create(path: &Path, width: u32, height: u32, rate: FrameRate, hardware: bool) -> Result<Self> {
        if width < 16 || height < 16 || width > 8192 || height > 8192 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(ExportError::Unsupported(format!("a recording must be an even size of 16–8192 pixels, not {width}x{height}")));
        }
        if rate.num <= 0 || rate.den <= 0 || u32::try_from(rate.num).is_err() || u32::try_from(rate.den).is_err() {
            return Err(ExportError::Unsupported("invalid recording frame rate".into()));
        }
        let fps = (rate.num as f64 / rate.den as f64).round().max(1.0) as u32;
        let kbps = capture_kbps(width, height, fps);
        let settings = ExportSettings {
            format: Format::H264,
            path: path.to_string_lossy().into_owned(),
            bitrate_kbps: kbps,
            max_bitrate_kbps: Some(kbps.saturating_mul(3) / 2),
            keyframe_distance: Some(fps.saturating_mul(2).max(1)),
            bitrate_mode: BitrateMode::Vbr1Pass,
            hardware_encoding: if hardware { HardwareEncoding::Auto } else { HardwareEncoding::Off },
            ..Default::default()
        };
        let make = |settings: &ExportSettings| -> Result<Box<dyn VideoEncoder>> {
            video_factories()
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .find_map(|fac| fac(Format::H264, width, height, rate, settings))
                .ok_or_else(|| ExportError::Unsupported("no H.264 encoder".into()))?
        };
        // the hardware encoder (High profile, no frame reordering) when one takes it, else ours in
        // Constrained Baseline: no B-frames (pictures come out in capture order) and the fastest
        let sessions = hw_encode_stats().sessions;
        let mut hw = None;
        if hardware {
            let enc = make(&settings)?;
            if hw_encode_stats().sessions > sessions {
                hw = Some(enc);
            }
        }
        let hardware = hw.is_some();
        let enc = match hw {
            Some(enc) => enc,
            None => make(&ExportSettings { hardware_encoding: HardwareEncoding::Off, h264_profile: H264Profile::Baseline, ..settings.clone() })?,
        };
        let file = File::create(path).map_err(|e| ExportError::Io(format!("{}: {e}", path.display())))?;
        Ok(Self {
            path: path.to_path_buf(),
            enc,
            hardware,
            width,
            height,
            rate,
            file: Some(BufWriter::with_capacity(1 << 20, file)),
            mux: None,
            track: 0,
            pending: VecDeque::new(),
            held: None,
            last_slot: None,
            frames: 0,
            bytes: 0,
        })
    }

    /// "VideoToolbox H.264" or "FilmCraft H.264".
    pub fn encoder_name(&self) -> &'static str {
        if self.hardware { "VideoToolbox H.264" } else { "FilmCraft H.264" }
    }

    pub fn hardware(&self) -> bool {
        self.hardware
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Pictures encoded so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Compressed bytes written so far.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The slot of the last picture taken (None before the first).
    pub fn last_slot(&self) -> Option<i64> {
        self.last_slot
    }

    /// Encode a straight RGBA picture of the recorder's size, shown from `slot` on. Slots must
    /// increase: a picture whose slot is not after the previous one is refused.
    pub fn push(&mut self, rgba: &[u8], slot: i64) -> Result<()> {
        if self.last_slot.is_some_and(|l| slot <= l) || slot < 0 {
            return Err(ExportError::Encode(format!("picture slot {slot} is not after the previous one")));
        }
        let need = (self.width as usize).saturating_mul(self.height as usize).saturating_mul(4);
        if rgba.len() < need {
            return Err(ExportError::Encode("the picture is smaller than the recording".into()));
        }
        let frame = EncoderFrame { width: self.width, height: self.height, rgba, hdr: None, index: self.frames };
        let packets = self.enc.encode(&frame)?;
        self.frames += 1;
        self.last_slot = Some(slot);
        self.pending.push_back(slot);
        self.take(packets)
    }

    fn take(&mut self, packets: Vec<EncodedPacket>) -> Result<()> {
        for p in packets {
            let Some(slot) = self.pending.pop_front() else {
                return Err(ExportError::Encode("the encoder returned more packets than pictures".into()));
            };
            if let Some((prev, prev_slot)) = self.held.take() {
                self.write(prev, slot.saturating_sub(prev_slot))?;
            }
            self.held = Some((p, slot));
        }
        Ok(())
    }

    fn write(&mut self, p: EncodedPacket, slots: i64) -> Result<()> {
        if self.mux.is_none() {
            let file = self.file.take().ok_or_else(|| ExportError::Encode("internal: the recording file is gone".into()))?;
            let mut mux = Mp4Writer::new(file, WriterOptions::new(Brand::Mov)).map_err(|e| ExportError::Io(e.to_string()))?;
            let timescale = u32::try_from(self.rate.num).map_err(|_| ExportError::Encode("invalid frame rate".into()))?;
            let mut cfg = TrackConfig::new(self.enc.sample_entry(), timescale);
            cfg.handler_name = Some("FilmCraft Recording".into());
            self.track = mux.add_track(cfg).map_err(|e| ExportError::Io(e.to_string()))?;
            self.mux = Some(mux);
        }
        let den = u32::try_from(self.rate.den).unwrap_or(1).max(1);
        // pictures reordered by the encoder keep its own durations (none of ours reorder)
        let duration = if p.composition_offset != 0 { p.duration } else { u32::try_from(slots.max(1)).unwrap_or(u32::MAX / den).saturating_mul(den) };
        let mux = self.mux.as_mut().ok_or_else(|| ExportError::Encode("internal: the container writer was not created".into()))?;
        mux.write_sample(self.track, WriteSample { data: &p.data, duration, composition_offset: p.composition_offset, is_sync: p.key })
            .map_err(|e| ExportError::Io(format!("{}: {e}", self.path.display())))?;
        self.bytes = self.bytes.saturating_add(p.data.len() as u64);
        Ok(())
    }

    /// Flush the encoder, give the last picture the time up to `end_slot` (at least one frame)
    /// and write `moov`. Returns the file size. A recording without any picture is an error (the
    /// caller deletes the file).
    pub fn finish(mut self, end_slot: i64) -> Result<u64> {
        let packets = self.enc.flush()?;
        self.take(packets)?;
        let Some((last, slot)) = self.held.take() else {
            return Err(ExportError::Encode("nothing was recorded".into()));
        };
        self.write(last, end_slot.saturating_sub(slot).max(1))?;
        let mux = self.mux.take().ok_or_else(|| ExportError::Encode("internal: the container writer was not created".into()))?;
        let w = mux.finish().map_err(|e| ExportError::Io(format!("{}: {e}", self.path.display())))?;
        let file = w.into_inner().map_err(|e| ExportError::Io(format!("{}: {e}", self.path.display())))?;
        file.sync_all().map_err(|e| ExportError::Io(format!("{}: {e}", self.path.display())))?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(self.bytes);
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_bitrates() {
        let k1080 = capture_kbps(1920, 1080, 30);
        assert_eq!(k1080, 12_000);
        let k4k = capture_kbps(3840, 2160, 30);
        assert!((38_000..42_000).contains(&k4k), "{k4k}");
        let k720 = capture_kbps(1280, 720, 30);
        assert!((5_000..7_000).contains(&k720), "{k720}");
        assert_eq!(capture_kbps(1920, 1080, 60), 24_000);
    }

    #[test]
    fn slots_become_durations() {
        let dir = std::env::temp_dir().join(format!("filmcraft-recorder-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slots.mov");
        let mut r = MovRecorder::create(&path, 64, 32, FrameRate::new(30, 1), false).unwrap();
        let px = vec![128u8; 64 * 32 * 4];
        r.push(&px, 0).unwrap();
        r.push(&px, 1).unwrap();
        r.push(&px, 5).unwrap();
        assert!(r.push(&px, 5).is_err(), "slots must increase");
        let bytes = r.finish(9).unwrap();
        assert!(bytes > 0);
        let data = std::fs::read(&path).unwrap();
        let mp4 = filmcraft_isobmff::open(data.as_slice()).unwrap();
        let t = &mp4.tracks[0];
        let durations: Vec<u32> = t.samples.iter().map(|s| s.duration).collect();
        assert_eq!(durations, vec![1, 4, 4]);
        assert!(MovRecorder::create(&dir.join("odd.mov"), 63, 32, FrameRate::new(30, 1), false).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
