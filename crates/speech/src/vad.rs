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
use filmcraft_time::Tick;

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
/// How far (frames) a word may grow past whisper's span into audio that is still voiced.
const MAX_GROW_FRAMES: usize = 50;

/// Frames searched on each side of a word boundary for the quietest moment between two words.
const BOUNDARY_SEARCH_FRAMES: usize = 12;
/// How much quieter (dB) than the speech on both sides the quietest frame must be to count as
/// the gap between two words.
const DIP_DB: f32 = 6.0;

/// Move the boundary between touching words to the quietest frame near it. Whisper's token times
/// land a little before or after the real end of a word, and a cut made there clips the
/// neighbour ("yo" losing its tail when "what" is removed). Only boundaries with no pause between
/// the words move, each word keeps at least [`MIN_WORD_FRAMES`], and a boundary moves only to a
/// real dip (at least [`DIP_DB`] below the loudest frame on either side of it).
pub fn refine_boundaries(audio: &[f32], words: &mut [Word]) {
    let db = frame_db(audio);
    let frame_ticks = TICKS_PER_SAMPLE * FRAME as i64;
    if db.len() < 3 || frame_ticks <= 0 {
        return;
    }
    let frame = |t: Tick| (t.0 / frame_ticks).max(0) as usize;
    for i in 1..words.len() {
        let (prev, next) = words.split_at_mut(i);
        let (Some(a), Some(b)) = (prev.last_mut(), next.first_mut()) else { continue };
        if (b.start.0 - a.end.0).abs() > frame_ticks {
            continue; // a pause between them: nothing to settle
        }
        let bf = frame(a.end);
        let lo = (frame(a.start) + MIN_WORD_FRAMES).max(bf.saturating_sub(BOUNDARY_SEARCH_FRAMES));
        let hi = frame(b.end).saturating_sub(MIN_WORD_FRAMES).min(bf + BOUNDARY_SEARCH_FRAMES).min(db.len() - 1);
        if lo >= hi {
            continue;
        }
        let Some((best, lvl)) = (lo..=hi).filter_map(|f| db.get(f).map(|l| (f, *l))).min_by(|x, y| x.1.total_cmp(&y.1)) else { continue };
        // the speech on either side: the loudest frame of each word around the dip
        let loudest = |r: std::ops::Range<usize>| r.filter_map(|f| db.get(f)).copied().fold(f32::MIN, f32::max);
        let (left, right) = (loudest(frame(a.start)..best), loudest(best + 1..frame(b.end).min(db.len())));
        if left.min(right) - lvl < DIP_DB {
            continue; // continuous speech: whisper's boundary is as good as any
        }
        let t = Tick(best as i64 * frame_ticks);
        a.end = t.max(a.start);
        b.start = t.min(b.end);
    }
}

/// Snap each word to the loudest voiced run inside its span (see the module docs).
pub fn tighten_words(audio: &[f32], words: &mut [Word]) {
    let db = frame_db(audio);
    if db.is_empty() {
        return;
    }
    let th = threshold(&db);
    let frame_ticks = TICKS_PER_SAMPLE * FRAME as i64;
    let spans: Vec<(usize, usize)> = words
        .iter()
        .map(|w| ((w.start.0 / frame_ticks).max(0) as usize, (((w.end.0 + frame_ticks - 1) / frame_ticks).max(0) as usize).min(db.len())))
        .collect();
    // the previous word's final end bounds how far a word may grow backwards; the next word's
    // span bounds growth forwards (so two words never claim the same frames)
    let mut prev_end = 0usize;
    for (k, w) in words.iter_mut().enumerate() {
        let (a, b) = spans[k];
        let next_start = spans.get(k + 1).map(|s| s.0).unwrap_or(db.len());
        if b <= a + MIN_WORD_FRAMES {
            prev_end = prev_end.max(b);
            continue;
        }
        let Some(frames) = db.get(a..b) else { continue };
        // voiced runs, merging gaps shorter than MAX_GAP_FRAMES; keep the one with the most speech
        // (decibels above the threshold summed over its frames: a long word beats a short loud
        // burst, which linear energy would not)
        let mut best: Option<(usize, usize, f64)> = None;
        let mut run: Option<(usize, usize, f64)> = None;
        let mut quiet = 0usize;
        for (i, &lvl) in frames.iter().enumerate() {
            if lvl >= th {
                let e = (lvl - th) as f64 + 1.0;
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
        let Some((s, e, _)) = best else {
            prev_end = prev_end.max(b);
            continue;
        };
        let (mut s, mut e) = (a + s, a + e);
        if e < s + MIN_WORD_FRAMES {
            // too short to be a word on its own: pad to the minimum inside the original span
            e = (s + MIN_WORD_FRAMES).min(b);
            s = e.saturating_sub(MIN_WORD_FRAMES).max(a);
        }
        // Whisper's times undershoot: when the voiced run reaches the span's edge and the audio
        // stays voiced beyond it, the word keeps going (up to the neighbour's span, at most
        // MAX_GROW_FRAMES), so a pause cut after "yo" starts after "yo" and not inside it.
        if e >= b {
            let limit = next_start.max(b).min(b + MAX_GROW_FRAMES).min(db.len());
            while e < limit && db.get(e).is_some_and(|l| *l >= th) {
                e += 1;
            }
        }
        if s <= a {
            let limit = prev_end.min(a).max(a.saturating_sub(MAX_GROW_FRAMES));
            while s > limit && db.get(s - 1).is_some_and(|l| *l >= th) {
                s -= 1;
            }
        }
        if s != a {
            w.start = filmcraft_time::Tick(s as i64 * frame_ticks);
        }
        if e != b {
            w.end = filmcraft_time::Tick(e as i64 * frame_ticks).max(w.start);
        }
        prev_end = e.max(prev_end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seconds_tick;

    #[test]
    fn boundaries_move_to_the_dip_between_touching_words() {
        // two loud tones with 60 ms of silence between them at 0.50–0.56 s; whisper put the
        // boundary 60 ms late (0.62) so a cut of word B would clip the end of A
        let sr = SAMPLE_RATE as usize;
        let mut audio = vec![0.0f32; sr];
        for (i, v) in audio.iter_mut().enumerate() {
            let t = i as f32 / sr as f32;
            if !(0.50..0.56).contains(&t) {
                *v = 0.5 * (t * 440.0 * std::f32::consts::TAU).sin();
            }
        }
        let w = |text: &str, a: f64, b: f64| Word { text: text.into(), start: seconds_tick(a), end: seconds_tick(b), speaker: None, confidence: 1.0 };
        let mut words = vec![w("a", 0.0, 0.62), w("b", 0.62, 1.0)];
        refine_boundaries(&audio, &mut words);
        let end = words[0].end.0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64;
        assert!((0.49..=0.56).contains(&end), "boundary moved into the gap: {end}");
        assert_eq!(words[1].start, words[0].end, "the words still touch");
        // continuous tone: no dip, the boundary stays
        let flat: Vec<f32> = (0..sr).map(|i| 0.5 * (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin()).collect();
        let mut words = vec![w("a", 0.0, 0.62), w("b", 0.62, 1.0)];
        refine_boundaries(&flat, &mut words);
        assert_eq!(words[0].end, seconds_tick(0.62));
        // a pause between the words: untouched; tiny words keep their minimum length
        let mut words = vec![w("a", 0.0, 0.40), w("b", 0.70, 1.0), w("c", 1.0, 1.02)];
        refine_boundaries(&audio, &mut words);
        assert_eq!(words[0].end, seconds_tick(0.40));
        assert_eq!(words[2].start, seconds_tick(1.0));
        refine_boundaries(&[], &mut words);
    }

    #[test]
    fn a_word_grows_into_voiced_audio_past_its_span_before_a_pause() {
        // "yo" is voiced 0.10–0.45 s, then silence until the next word at 2.8 s; whisper's span
        // ended at 0.25 s, so a pause cut would have started inside the word
        let sr = SAMPLE_RATE as usize;
        let mut audio = vec![0.0f32; 3 * sr];
        for (i, v) in audio.iter_mut().enumerate() {
            let t = i as f32 / sr as f32;
            if (0.10..0.45).contains(&t) || (2.80..3.00).contains(&t) {
                *v = 0.5 * (t * 300.0 * std::f32::consts::TAU).sin();
            }
        }
        let w = |text: &str, a: f64, b: f64| Word { text: text.into(), start: seconds_tick(a), end: seconds_tick(b), speaker: None, confidence: 1.0 };
        let mut words = vec![w("yo", 0.10, 0.25), w("what", 2.80, 3.00)];
        tighten_words(&audio, &mut words);
        let end = words[0].end.0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64;
        assert!((0.43..=0.47).contains(&end), "grew to the end of the voiced audio: {end}");
        let start = words[1].start.0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64;
        assert!((2.78..=2.82).contains(&start), "{start}");
        // growth stops at the next word's span and at MAX_GROW_FRAMES
        let mut words = vec![w("a", 0.10, 0.25), w("b", 0.30, 0.45)];
        tighten_words(&audio, &mut words);
        assert!(words[0].end <= words[1].start, "{:?} {:?}", words[0].end, words[1].start);
        let long: Vec<f32> = (0..3 * sr).map(|i| 0.5 * (i as f32 / sr as f32 * 300.0 * std::f32::consts::TAU).sin()).collect();
        let mut words = vec![w("a", 0.10, 0.25)];
        tighten_words(&long, &mut words);
        let end = words[0].end.0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64;
        assert!(end <= 0.25 + 0.5 + 0.011, "{end}");
    }

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
