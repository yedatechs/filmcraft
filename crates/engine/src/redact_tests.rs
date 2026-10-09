//! Tracked redaction (`redact.*`) on the demo project (V1 clips, the V2 overlay at 6–10 s) and on
//! a synthetic moving clip for tracking.

use serde_json::{Value, json};

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_time::{TICKS_PER_SECOND, Tick};

fn sec(x: f64) -> Tick {
    Tick((x * TICKS_PER_SECOND as f64).round() as i64)
}

/// Demo project with the playhead at `t` seconds.
fn demo(t: f64) -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.set_playhead(sec(t));
    s
}

fn list(s: &mut Session, clip: ClipId) -> Vec<Value> {
    s.execute("redact.list", json!({"clip": clip.0})).unwrap()["redactions"].as_array().unwrap().clone()
}

fn effect(s: &Session, clip: ClipId, i: usize) -> &filmcraft_project::EffectInstance {
    let (_, it) = s.active_sequence().unwrap().find_item(clip).unwrap();
    &it.effects[i]
}

fn undo_labels(s: &mut Session) -> Vec<String> {
    let h = s.execute("history.list", json!({})).unwrap();
    h["undo"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
}

#[test]
fn add_without_tracking_puts_a_mosaic_with_a_rectangle_mask_on_the_top_clip() {
    let mut s = demo(2.5);
    let v1 = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    let before = undo_labels(&mut s).len();
    let r = s.execute("redact.add", json!({"rect": [100, 200, 300, 150], "track": false})).unwrap();
    assert_eq!(r["clip"], json!(v1.0), "top clip at 2.5 s is the first V1 clip: {r}");
    assert_eq!(r["name"], "Redaction 1");
    assert_eq!(r["jobs"], json!([]));
    assert_eq!(undo_labels(&mut s).len(), before + 1, "one undo step");
    let ei = r["effect"].as_u64().unwrap() as usize;
    let e = effect(&s, v1, ei);
    assert_eq!(e.effect, "mosaic");
    // blocks ≈ box / 6 over the 1920×1080 source
    assert_eq!(e.f64_at("horizontal", Tick::ZERO), (1920.0f64 * 6.0 / 300.0).round());
    assert_eq!(e.f64_at("vertical", Tick::ZERO), (1080.0f64 * 6.0 / 150.0).round());
    let m = &e.masks[0];
    assert_eq!(m.name, "Redaction 1");
    let (lo, hi) = m.path_at(Tick::ZERO).bounds();
    assert_eq!((lo.x, lo.y, hi.x, hi.y), (100.0, 200.0, 400.0, 350.0));
    assert_eq!(s.state.selected_mask, Some(crate::masks::MaskSel { clip: v1, effect: ei, mask: 0 }));
    let l = list(&mut s, v1);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0]["redaction"], 1);
    assert_eq!(l[0]["style"], "mosaic");
    assert_eq!(l[0]["rect"], json!([100.0, 200.0, 300.0, 150.0]));
    assert_eq!(l[0]["tracked"], false);
    // a second one is Redaction 2
    let r2 = s.execute("redact.add", json!({"clip": v1.0, "rect": [0, 0, 50, 50], "track": false})).unwrap();
    assert_eq!(r2["name"], "Redaction 2");
    assert_eq!(list(&mut s, v1).len(), 2);
}

#[test]
fn top_clip_is_the_v2_overlay_when_it_covers_the_playhead() {
    let mut s = demo(7.0);
    let v2 = s.active_sequence().unwrap().video_tracks[1].items[0].id;
    let r = s.execute("redact.add", json!({"rect": [10, 10, 100, 100], "track": false})).unwrap();
    assert_eq!(r["clip"], json!(v2.0));
}

#[test]
fn blur_and_fill_styles() {
    let mut s = demo(2.5);
    let v1 = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    let r = s.execute("redact.add", json!({"rect": [100, 100, 200, 200], "style": "blur", "track": false})).unwrap();
    let e = effect(&s, v1, r["effect"].as_u64().unwrap() as usize);
    assert_eq!(e.effect, "gaussian_blur");
    assert_eq!(e.f64_at("blurriness", Tick::ZERO), 60.0);
    let r = s.execute("redact.add", json!({"rect": [400, 100, 200, 200], "style": "fill", "track": false})).unwrap();
    let e = effect(&s, v1, r["effect"].as_u64().unwrap() as usize);
    assert_eq!(e.effect, "mosaic");
    assert_eq!((e.f64_at("horizontal", Tick::ZERO), e.f64_at("vertical", Tick::ZERO)), (1.0, 1.0));
    let styles: Vec<String> = list(&mut s, v1).iter().map(|r| r["style"].as_str().unwrap().to_string()).collect();
    assert_eq!(styles.len(), 2);
    assert!(styles.contains(&"blur".to_string()) && styles.contains(&"fill".to_string()), "{styles:?}");
}

#[test]
fn hostile_params_are_refused() {
    let mut s = demo(2.5);
    for p in [
        json!({"track": false}),
        json!({"rect": [1, 2, 3], "track": false}),
        json!({"rect": ["a", 0, 10, 10], "track": false}),
        json!({"rect": [5000, 5000, 10, 10], "track": false}),
        json!({"rect": [10, 10, 0, 0], "track": false}),
        json!({"rect": [10, 10, 50, 50], "style": "smudge", "track": false}),
        json!({"rect": [10, 10, 50, 50], "clip": 999_999, "track": false}),
    ] {
        assert!(s.execute("redact.add", p.clone()).is_err(), "{p}");
    }
    // a box hanging off the picture is clamped to it
    let r = s.execute("redact.add", json!({"rect": [1800, 1000, 500, 500], "track": false})).unwrap();
    assert_eq!(r["rect"], json!([1800.0, 1000.0, 120.0, 80.0]));
    // audio clips are refused
    let a1 = s.active_sequence().unwrap().audio_tracks[0].items[0].id;
    assert!(s.execute("redact.add", json!({"clip": a1.0, "rect": [0, 0, 10, 10], "track": false})).is_err());
    assert!(s.execute("redact.remove", json!({"redaction": 42})).is_err());
    assert!(s.execute("redact.remove", json!({})).is_err());
    // nothing under the playhead
    s.set_playhead(sec(3600.0));
    assert!(s.execute("redact.add", json!({"rect": [0, 0, 10, 10], "track": false})).is_err());
    assert!(s.execute("redact.list", json!({})).is_err());
}

#[test]
fn remove_takes_effect_and_mask_away_in_one_undo_step() {
    let mut s = demo(2.5);
    let v1 = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    let n0 = s.active_sequence().unwrap().find_item(v1).unwrap().1.effects.len();
    s.execute("redact.add", json!({"rect": [100, 100, 200, 200], "track": false})).unwrap();
    s.execute("redact.add", json!({"rect": [400, 100, 200, 200], "style": "blur", "track": false})).unwrap();
    let steps = undo_labels(&mut s).len();
    let r = s.execute("redact.remove", json!({"redaction": 1})).unwrap();
    assert_eq!(r["removed"], 1);
    assert_eq!(undo_labels(&mut s).len(), steps + 1);
    let l = list(&mut s, v1);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0]["name"], "Redaction 2");
    assert_eq!(s.active_sequence().unwrap().find_item(v1).unwrap().1.effects.len(), n0 + 1);
    // the next one does not reuse a number still on the clip
    let r = s.execute("redact.add", json!({"rect": [0, 0, 50, 50], "track": false})).unwrap();
    assert_eq!(r["name"], "Redaction 3");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(list(&mut s, v1).len(), 2, "undo brings the removed redaction back");
}

// ---------------------------------------------------------------- tracking

/// A 320×240 24 fps clip of a textured disc moving right and down by a known amount per frame
/// over a static textured background.
struct Moving {
    info: filmcraft_media::MediaInfo,
}

fn texture(u: f64, v: f64) -> f64 {
    let s = (u * 0.21).sin() * (v * 0.17).cos() + 0.5 * ((u + v) * 0.43).sin() + 0.35 * ((u * 0.9 - v * 0.6).sin() * (v * 0.75).cos());
    let (iu, iv) = ((u / 6.0).floor() as i64, (v / 6.0).floor() as i64);
    let mut x = (iu.wrapping_mul(73_856_093) ^ iv.wrapping_mul(19_349_663)) as u64;
    x ^= x >> 13;
    x = x.wrapping_mul(0x5bd1_e995);
    let h = (x >> 40) as f64 / (1u64 << 24) as f64;
    (0.5 + 0.22 * s + 0.12 * h).clamp(0.0, 1.0)
}

fn centre(k: f64) -> filmcraft_geom::Vec2 {
    filmcraft_geom::Vec2::new(120.0 + 3.0 * k, 110.0 + 1.2 * k)
}

impl filmcraft_media::MediaSource for Moving {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<std::sync::Arc<filmcraft_frame::VideoFrame>> {
        let k = filmcraft_time::FrameRate::FPS_24.frame_at(req.time) as f64;
        let c = centre(k);
        let (w, h) = (320usize, 240usize);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let p = filmcraft_geom::Vec2::new(x as f64 + 0.5, y as f64 + 0.5);
                let o = p - c;
                let v = if o.length() < 70.0 { texture(o.x + 200.0, o.y + 300.0) } else { 0.25 + 0.1 * texture(p.x * 0.5 + 900.0, p.y * 0.5) };
                let v = (v * 255.0).round() as u8;
                px[(y * w + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        Ok(std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba8(w as u32, h as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

fn tracking_session() -> (Session, ClipId) {
    use filmcraft_media::MediaSource;
    let rate = filmcraft_time::FrameRate::FPS_24;
    let g = filmcraft_media::generators::GeneratorSource::new(
        filmcraft_media::Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] },
        320,
        240,
        rate,
        rate.tick_of(48),
    );
    let info = g.info().clone();
    let mut p = filmcraft_project::Project::new("redact");
    let item = p.add_item(
        "moving",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Media(filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::Generator(g.generator.clone()),
            info: info.clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let seq = p.new_sequence("s", filmcraft_project::SequenceSettings { width: 320, height: 240, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let ti =
        p.make_track_item(item, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate).unwrap();
    let clip = ti.id;
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let mut s = Session { project: std::sync::Arc::new(p), ..Default::default() };
    s.state.active_sequence = Some(seq);
    s.media.insert(item, std::sync::Arc::new(Moving { info }));
    s.set_playhead(rate.tick_of(10));
    (s, clip)
}

/// A 60×60 box on the disc at frame 10.
fn box_on_disc() -> Value {
    let c = centre(10.0);
    json!([c.x - 30.0, c.y - 30.0, 60.0, 60.0])
}

fn keyframes(s: &Session, clip: ClipId, ei: usize) -> Vec<Tick> {
    effect(s, clip, ei).masks[0].path.keyframes.iter().map(|k| k.time).collect()
}

#[test]
fn tracking_with_wait_runs_forward_then_backward() {
    let (mut s, clip) = tracking_session();
    let rate = filmcraft_time::FrameRate::FPS_24;
    let r = s.execute("redact.add", json!({"rect": box_on_disc(), "frames": 6, "wait": true})).unwrap();
    assert_eq!(r["clip"], json!(clip.0));
    assert!(r.get("trackError").is_none(), "{r}");
    assert_eq!(r["jobs"].as_array().unwrap().len(), 2, "forward and backward: {r}");
    assert!(s.mask_jobs.is_empty());
    let ei = r["effect"].as_u64().unwrap() as usize;
    let keys = keyframes(&s, clip, ei);
    assert_eq!(keys.len(), 13, "frames 4..=16: {keys:?}");
    assert_eq!(keys.first().copied(), Some(rate.tick_of(4)));
    assert_eq!(keys.last().copied(), Some(rate.tick_of(16)));
    let l = list(&mut s, clip);
    assert_eq!(l[0]["tracked"], true);
    assert_eq!(l[0]["tracking"], false);
    // the box rides on the disc: at frame 16 it moved by (18, 7.2)
    s.set_playhead(rate.tick_of(16));
    let l = list(&mut s, clip);
    let x = l[0]["rect"][0].as_f64().unwrap();
    let y = l[0]["rect"][1].as_f64().unwrap();
    let c = centre(16.0);
    assert!((x - (c.x - 30.0)).abs() < 1.5 && (y - (c.y - 30.0)).abs() < 1.5, "{:?} vs {c:?}", l[0]["rect"]);
    let undo = undo_labels(&mut s);
    assert_eq!(&undo[undo.len() - 3..], ["Redact Area", "Track Mask", "Track Mask"], "{undo:?}");
}

#[test]
fn background_tracking_queues_the_backward_run() {
    let (mut s, clip) = tracking_session();
    let r = s.execute("redact.add", json!({"rect": box_on_disc(), "frames": 4})).unwrap();
    assert_eq!(r["jobs"].as_array().unwrap().len(), 1, "only the forward job runs at first: {r}");
    let ei = r["effect"].as_u64().unwrap() as usize;
    assert_eq!(list(&mut s, clip)[0]["tracking"], true);
    assert!(s.execute("redact.add", json!({"rect": [0, 0, 20, 20], "track": false})).is_err(), "busy clip");
    let t0 = std::time::Instant::now();
    while (s.jobs.len() < 2 || !s.mask_jobs.is_empty()) && t0.elapsed().as_secs() < 60 {
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(s.jobs.len(), 2, "the backward job started after the forward one");
    assert!(s.mask_jobs.is_empty());
    assert_eq!(keyframes(&s, clip, ei).len(), 9, "frames 6..=14");
    let l = list(&mut s, clip);
    assert_eq!(l[0]["tracked"], true);
    assert_eq!(l[0]["tracking"], false);
}

#[test]
fn removing_a_redaction_while_it_tracks_stops_the_tracking() {
    let (mut s, clip) = tracking_session();
    s.execute("redact.add", json!({"rect": box_on_disc()})).unwrap();
    s.execute("redact.remove", json!({"redaction": 1})).unwrap();
    assert!(s.mask_jobs.is_empty());
    assert!(list(&mut s, clip).is_empty());
    let t0 = std::time::Instant::now();
    while t0.elapsed().as_millis() < 300 {
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(s.jobs.len(), 1, "no backward run for a removed redaction");
}
