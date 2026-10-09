//! Energy-based tightening of word bounds.
//!
//! Attention alignment places every word boundary on the next word's start, so silence between
//! words is absorbed into the word before it (and leading silence into the first word); whisper.cpp
//! often puts the boundary a frame inside the neighbouring word, so the silence sits *inside* the
//! span, not at its edge. Pause detection and text-based editing need the silences, so each word is
//! snapped to the loudest run of voiced 10 ms frames inside its span (runs separated by less than
//! 150 ms of quiet, a plosive closure, count as one), where voiced means above a threshold set
//! between the clip's noise floor (10th percentile of the levels of frames above −100 dBFS) and its
//! speech level (95th percentile): 30 % of the way up, and at least 6 dB above the floor. A word
//! keeps at least 60 ms; a word with no voiced frame is left alone.

use filmcraft_project::Word;

use crate::{SAMPLE_RATE, TICKS_PER_SAMPLE};

const FRAME: usize = (SAMPLE_RATE / 100) as usize;
const MIN_WORD_FRAMES: usize = 6;

/// Frame levels in dBFS (10 ms RMS).
pub fn frame_db(audio: &[f32]) -> Vec<f32> {
    audio
        .chunks(FRAME)
        .map(|c| {
            let e = c.iter().map(|v| v * v).sum::<f32>() / c.len().max(1) as f32;
            10.0 * (e + 1e-12).log10()
        })
        .collect()
}

/// The speech/silence threshold in dBFS.
pub fn threshold(db: &[f32]) -> f32 {
    if db.is_empty() {
        return -60.0;
    }
    // A few frames of digital silence (< −100 dBFS: a fade, a dropout) would drag the floor far
    // below the recording's own noise, so they are left out. When a good part of the clip is
    // digital silence (a gated microphone, denoised or synthetic speech), that *is* the floor, and
    // leaving it out would set the threshold above the speech itself.
    let silent = db.iter().filter(|v| **v <= -100.0).count();
    let gated = silent.saturating_mul(20) >= db.len();
    let mut s: Vec<f32> = db.iter().copied().filter(|v| gated || *v > -100.0).map(|v| v.max(-100.0)).collect();
    if s.is_empty() {
        return -60.0;
    }
    s.sort_by(f32::total_cmp);
    let q = |p: f32| s[((s.len() - 1) as f32 * p) as usize];
    let (floor, speech) = (q(0.10), q(0.95));
    (floor + 0.3 * (speech - floor)).max(floor + 6.0)
}

/// Quiet stretches shorter than this inside a word do not split it (a stop consonant's closure).
const MAX_GAP_FRAMES: usize = 15;

/// Snap each word to the loudest voiced run inside its span (see the module docs).
pub fn tighten_words(audio: &[f32], words: &mut [Word]) {
    let db = frame_db(audio);
    if db.is_empty() {
        return;
    }
    let th = threshold(&db);
    let frame_ticks = TICKS_PER_SAMPLE * FRAME as i64;
    for w in words {
        let a = (w.start.0 / frame_ticks).max(0) as usize;
        let b = (((w.end.0 + frame_ticks - 1) / frame_ticks).max(0) as usize).min(db.len());
        if b <= a + MIN_WORD_FRAMES {
            continue;
        }
        let Some(frames) = db.get(a..b) else { continue };
        // voiced runs, merging gaps shorter than MAX_GAP_FRAMES; keep the one with the most energy
        let mut best: Option<(usize, usize, f64)> = None;
        let mut run: Option<(usize, usize, f64)> = None;
        let mut quiet = 0usize;
        for (i, &lvl) in frames.iter().enumerate() {
            if lvl >= th {
                let e = 10f64.powf((lvl as f64) / 10.0);
                run = Some(match run {
                    Some((s0, _, sum)) => (s0, i + 1, sum + e),
                    None => (i, i + 1, e),
                });
                quiet = 0;
            } else if let Some(r) = run {
                quiet += 1;
                if quiet >= MAX_GAP_FRAMES {
                    if best.is_none_or(|(_, _, bs)| r.2 > bs) {
                        best = Some(r);
                    }
                    run = None;
                    quiet = 0;
                }
            }
        }
        if let Some(r) = run
            && best.is_none_or(|(_, _, bs)| r.2 > bs)
        {
            best = Some(r);
        }
        // nothing voiced at all: leave the word alone
        let Some((s, e, _)) = best else { continue };
        let (mut s, mut e) = (a + s, a + e);
        if e < s + MIN_WORD_FRAMES {
            // too short to be a word on its own: pad to the minimum inside the original span
            e = (s + MIN_WORD_FRAMES).min(b);
            s = e.saturating_sub(MIN_WORD_FRAMES).max(a);
        }
        if s > a {
            w.start = filmcraft_time::Tick(s as i64 * frame_ticks);
        }
        if e < b {
            w.end = filmcraft_time::Tick(e as i64 * frame_ticks).max(w.start);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seconds_tick;

    #[test]
    fn silence_is_trimmed_from_words() {
        // 0.0–0.5 silence, 0.5–1.0 tone, 1.0–2.0 silence, 2.0–2.5 tone
        let mut audio: Vec<f32> = (0..40_000).map(|i| if i % 2 == 0 { 0.0001 } else { 0.0 }).collect();
        for i in (8_000..16_000).chain(32_000..40_000) {
            audio[i] = 0.3 * (i as f32 * 0.2).sin();
        }
        let mut w = vec![Word::new("a", seconds_tick(0.0), seconds_tick(2.0)), Word::new("b", seconds_tick(2.0), seconds_tick(2.5))];
        tighten_words(&audio, &mut w);
        assert_eq!(w[0].start, seconds_tick(0.5));
        assert_eq!(w[0].end, seconds_tick(1.0));
        assert_eq!((w[1].start, w[1].end), (seconds_tick(2.0), seconds_tick(2.5)));
    }

    /// whisper.cpp puts a word's start a frame inside the previous word's voiced tail, with the
    /// pause after it: the word must still snap to its own voiced run, and a short quiet gap
    /// inside a word (a stop consonant) must not split it.
    #[test]
    fn boundary_inside_the_neighbour_and_internal_stops() {
        // tone 0.0–1.0 ("channel"), silence 1.0–1.9, tone 1.9–2.1, 80 ms closure, tone 2.18–2.6 ("welcome")
        let mut audio: Vec<f32> = vec![0.0; 48_000];
        for i in (0..16_000).chain(30_400..33_600).chain(34_880..41_600) {
            audio[i] = 0.3 * (i as f32 * 0.2).sin();
        }
        let mut w = vec![Word::new("channel", seconds_tick(0.0), seconds_tick(0.99)), Word::new("welcome", seconds_tick(0.99), seconds_tick(2.6))];
        tighten_words(&audio, &mut w);
        assert_eq!((w[0].start, w[0].end), (seconds_tick(0.0), seconds_tick(0.99)), "the first word is all voiced");
        assert_eq!(w[1].start, seconds_tick(1.9), "the pause and the neighbour's tail are dropped");
        assert_eq!(w[1].end, seconds_tick(2.6), "the 80 ms closure does not split the word");
    }
}
