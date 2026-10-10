//! `layout.*` on the demo project: V1 holds six shots, V2 a scaled-down overlay from 6 s to 10 s.
//! At 7 s both V1's second shot and the overlay are visible.

use serde_json::{Value, json};

use crate::Session;
use crate::layout::LAYOUT_MASK;
use filmcraft_project::{ClipId, ItemKind, ParamValue};
use filmcraft_time::Tick;

const FW: f64 = 1920.0;
const FH: f64 = 1080.0;

/// Demo project, playhead at 7 s. Returns (session, V1 clip, V2 overlay).
fn session() -> (Session, ClipId, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("playhead.set", json!({"seconds": 7.0})).unwrap();
    let q = s.active_sequence().unwrap();
    let t = s.playhead();
    let v1 = q.video_tracks[0].items.iter().find(|i| i.start <= t && t < i.end()).unwrap().id;
    let v2 = q.video_tracks[1].items[0].id;
    (s, v1, v2)
}

fn inspect(s: &mut Session, c: ClipId) -> Value {
    s.execute("layout.inspect", json!({"clips": [c.0]})).unwrap()["clips"][0].clone()
}

fn bx(v: &Value) -> [f64; 4] {
    let a = v["box"].as_array().unwrap();
    [a[0].as_f64().unwrap(), a[1].as_f64().unwrap(), a[2].as_f64().unwrap(), a[3].as_f64().unwrap()]
}

/// Motion position and scale params (static value and keyframes) of a clip.
fn motion(s: &Session, c: ClipId) -> (filmcraft_project::Param, filmcraft_project::Param) {
    let (_, it) = s.active_sequence().unwrap().find_item(c).unwrap();
    let m = it.effect("motion").unwrap();
    (m.param("position").unwrap().clone(), m.param("scale").unwrap().clone())
}

fn layout_masks(s: &Session, c: ClipId) -> Vec<filmcraft_project::Mask> {
    let (_, it) = s.active_sequence().unwrap().find_item(c).unwrap();
    it.effect("opacity").unwrap().masks.clone()
}

fn vec2(p: &filmcraft_project::Param) -> (f64, f64) {
    match &p.value {
        ParamValue::Vec2(v) => (v.x, v.y),
        v => panic!("not a point: {v:?}"),
    }
}

fn float(p: &filmcraft_project::Param) -> f64 {
    p.value.as_f64().unwrap()
}

#[test]
fn every_preset_round_trips_through_inspect() {
    let (mut s, _, v2) = session();
    let m = 0.03 * FW;
    for at in ["topLeft", "topRight", "bottomLeft", "bottomRight", "top", "bottom", "left", "right", "center"] {
        s.execute("layout.place", json!({"clips": [v2.0], "at": at})).unwrap();
        let r = inspect(&mut s, v2);
        assert_eq!(r["at"], json!(at), "{r}");
        assert!((r["size"].as_f64().unwrap() - 25.0).abs() < 0.5, "{r}");
        let [x, y, w, h] = bx(&r);
        assert!((w - 0.25 * FW).abs() < 1.0, "{r}");
        if at.contains("Left") || at == "left" {
            assert!((x - m).abs() < 1.0, "{at} {r}");
        }
        if at.contains("Right") || at == "right" {
            assert!((FW - x - w - m).abs() < 1.0, "{at} {r}");
        }
        if at.starts_with("top") {
            assert!((y - m).abs() < 1.0, "{at} {r}");
        }
        if at.starts_with("bottom") {
            assert!((FH - y - h - m).abs() < 1.0, "{at} {r}");
        }
        if at != "center" {
            assert!((r["margin"].as_f64().unwrap() - 3.0).abs() < 0.05, "{r}");
        }
        assert_eq!(s.history.undo.last().unwrap().0, "Place Clip");
    }
    // size and margin are honoured and clamped
    s.execute("layout.place", json!({"clips": [v2.0], "at": "bottomRight", "size": 40, "margin": 5})).unwrap();
    let r = inspect(&mut s, v2);
    assert_eq!(r["at"], json!("bottomRight"));
    assert!((r["size"].as_f64().unwrap() - 40.0).abs() < 0.5 && (r["margin"].as_f64().unwrap() - 5.0).abs() < 0.05, "{r}");
    s.execute("layout.place", json!({"clips": [v2.0], "at": "topLeft", "size": 900, "margin": -4})).unwrap();
    let r = inspect(&mut s, v2);
    assert!((r["size"].as_f64().unwrap() - 100.0).abs() < 0.5, "{r}");
}

#[test]
fn full_fits_and_centres_a_portrait_source() {
    let (mut s, _, v2) = session();
    // make the overlay's media portrait
    let item = s.active_sequence().unwrap().find_item(v2).unwrap().1.item;
    s.edit("test: portrait", |p, _| {
        if let Some(ItemKind::Media(m)) = p.item_mut(item).map(|i| &mut i.kind) {
            let v = m.info.video.as_mut().unwrap();
            (v.width, v.height) = (1080, 1920);
        }
        Ok(())
    })
    .unwrap();
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "circle"})).unwrap();
    s.execute("layout.place", json!({"clips": [v2.0], "at": "full"})).unwrap();
    // the whole source fits: 1920 tall in a 1080 frame → scale 56.25 %
    let (_, scale) = motion(&s, v2);
    assert!((float(&scale) - 56.25).abs() < 1e-6, "{scale:?}");
    // the shape is untouched and the (circle's) box is centred
    let r = inspect(&mut s, v2);
    assert_eq!(r["shape"], json!("circle"), "{r}");
    let [x, y, w, h] = bx(&r);
    assert!((x + w / 2.0 - FW / 2.0).abs() < 1e-3 && (y + h / 2.0 - FH / 2.0).abs() < 1e-3, "{r}");
    assert!((w - 1080.0 * 0.5625).abs() < 1e-3, "{r}");
    // a landscape source the frame's aspect: Full is scale 100 at the centre
    let (mut s, v1, _) = session();
    s.execute("layout.place", json!({"clips": [v1.0], "at": "topLeft"})).unwrap();
    s.execute("layout.place", json!({"clips": [v1.0], "at": "full"})).unwrap();
    let r = inspect(&mut s, v1);
    assert_eq!(r["at"], json!("full"), "{r}");
    assert_eq!(bx(&r), [0.0, 0.0, FW, FH]);
}

#[test]
fn circle_then_place_stays_round_and_placed() {
    let (mut s, _, v2) = session();
    s.execute("layout.place", json!({"clips": [v2.0], "at": "bottomRight", "size": 25})).unwrap();
    let before = bx(&inspect(&mut s, v2));
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "circle"})).unwrap();
    assert_eq!(s.history.undo.last().unwrap().0, "Shape Clip");
    let r = inspect(&mut s, v2);
    assert_eq!(r["at"], json!("bottomRight"), "{r}");
    assert_eq!(r["shape"], json!("circle"));
    assert!((r["size"].as_f64().unwrap() - 25.0).abs() < 0.5, "{r}");
    let [x, y, w, h] = bx(&r);
    assert!((w - h).abs() < 1e-6, "round: {r}");
    // same width and right / bottom edges as before the shape change
    assert!((x + w - (before[0] + before[2])).abs() < 1e-3 && (w - before[2]).abs() < 1e-3);
    assert!((y + h - (before[1] + before[3])).abs() < 1e-3);
    // placing again keeps it round and in the corner
    s.execute("layout.place", json!({"clips": [v2.0], "at": "topLeft", "size": 20})).unwrap();
    let r = inspect(&mut s, v2);
    assert_eq!(r["at"], json!("topLeft"), "{r}");
    let [x, y, w, h] = bx(&r);
    assert!((w - h).abs() < 1e-6 && (w - 0.2 * FW).abs() < 1.0 && (x - 0.03 * FW).abs() < 1.0 && (y - 0.03 * FW).abs() < 1.0, "{r}");
    let masks = layout_masks(&s, v2);
    assert_eq!(masks.iter().filter(|m| m.name == LAYOUT_MASK).count(), 1);
    // rounded and square, then free removes only the layout mask
    s.execute("masks.add", json!({"clip": v2.0, "shape": "ellipse"})).unwrap();
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "rounded", "radius": 20})).unwrap();
    let r = inspect(&mut s, v2);
    assert_eq!((r["shape"].clone(), r["radius"].clone()), (json!("rounded"), json!(20.0)), "{r}");
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "square"})).unwrap();
    assert_eq!(inspect(&mut s, v2)["shape"], json!("square"));
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "free"})).unwrap();
    let masks = layout_masks(&s, v2);
    assert_eq!(masks.len(), 1, "the user's mask stays");
    assert_ne!(masks[0].name, LAYOUT_MASK);
    let r = inspect(&mut s, v2);
    assert_eq!(r["shape"], json!("free"));
    assert!((r["size"].as_f64().unwrap() - 20.0).abs() < 0.5, "free keeps the width: {r}");
}

/// Which video track (0 = V1) holds the clip.
fn track_of(s: &Session, c: ClipId) -> usize {
    let q = s.active_sequence().unwrap();
    q.video_tracks.iter().position(|t| t.items.iter().any(|i| i.id == c)).unwrap()
}

/// (start, end) in seconds of every item on a video track.
fn spans(s: &Session, track: usize) -> Vec<(f64, f64, filmcraft_project::ItemId)> {
    let q = s.active_sequence().unwrap();
    q.video_tracks[track].items.iter().map(|i| (i.start.seconds(), i.end().seconds(), i.item)).collect()
}

#[test]
fn swap_twice_restores_both_clips() {
    let (mut s, v1, v2) = session();
    s.execute("layout.place", json!({"clips": [v2.0], "at": "bottomRight"})).unwrap();
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "circle"})).unwrap();
    let (m1, m2) = (motion(&s, v1), motion(&s, v2));
    let (k1, k2) = (layout_masks(&s, v1), layout_masks(&s, v2));
    let audio_before = serde_json::to_value(&s.active_sequence().unwrap().audio_tracks).unwrap();
    let r = s.execute("layout.swap", json!({})).unwrap();
    // the overlay (top-most first) starts with the overlap and keeps its id; the V1 shot is cut
    // at 6 s and its piece inside the overlap is a new clip
    let (pa, pb) = (ClipId(r["clips"][0].as_u64().unwrap()), ClipId(r["clips"][1].as_u64().unwrap()));
    assert_eq!(pa, v2, "{r}");
    assert_ne!(pb, v1, "{r}");
    assert_eq!(r["moved"], json!([[1, 0], [0, 1]]), "{r}");
    assert_eq!(s.history.undo.last().unwrap().0, "Swap Layouts");
    // the V1 piece is now the bottom-right circle on V2, the overlay fills the frame on V1
    let a = inspect(&mut s, pb);
    assert_eq!((a["at"].clone(), a["shape"].clone(), a["track"].clone()), (json!("bottomRight"), json!("circle"), json!(1)), "{a}");
    let b = inspect(&mut s, v2);
    assert_eq!((b["at"].clone(), b["shape"].clone(), b["track"].clone()), (json!("full"), json!("free"), json!(0)), "{b}");
    // the audio is untouched
    assert_eq!(serde_json::to_value(&s.active_sequence().unwrap().audio_tracks).unwrap(), audio_before);
    // swapping again (at the playhead) restores the layouts and the tracks
    let r2 = s.execute("layout.swap", json!({})).unwrap();
    assert_eq!(r2["clips"], json!([pb.0, v2.0]), "{r2}");
    assert_eq!((track_of(&s, pb), track_of(&s, v2)), (0, 1));
    for (c, (pos, scale), masks) in [(pb, m1, k1), (v2, m2, k2)] {
        let (p, sc) = motion(&s, c);
        let (a, b) = (vec2(&p), vec2(&pos));
        assert!((a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6, "{c:?} position {a:?} vs {b:?}");
        assert!((float(&sc) - float(&scale)).abs() < 1e-6);
        let now = layout_masks(&s, c);
        assert_eq!(now.len(), masks.len());
        for (x, y) in now.iter().zip(&masks) {
            assert_eq!(x.name, y.name);
            let (ParamValue::Path(px), ParamValue::Path(py)) = (&x.path.value, &y.path.value) else { panic!("paths") };
            assert!(px.components().iter().zip(py.components()).all(|(a, b)| (a - b).abs() < 1e-6));
        }
    }
    // explicit clips work the same way
    s.execute("layout.swap", json!({"clips": [pb.0, v2.0]})).unwrap();
    assert_eq!((track_of(&s, pb), track_of(&s, v2)), (1, 0));
}

#[test]
fn swap_exchanges_tracks_over_the_overlap() {
    let (mut s, v1, v2) = session();
    s.execute("layout.place", json!({"clips": [v2.0], "at": "bottomRight"})).unwrap();
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "circle"})).unwrap();
    let before = s.project.clone();
    let (shot, overlay) = {
        let q = s.active_sequence().unwrap();
        (q.find_item(v1).unwrap().1.clone(), q.find_item(v2).unwrap().1.clone())
    };
    let r = s.execute("layout.swap", json!({})).unwrap();
    let (rs, re) = (Tick(r["range"][0].as_i64().unwrap()).seconds(), Tick(r["range"][1].as_i64().unwrap()).seconds());
    assert!((rs - overlay.start.seconds()).abs() < 1e-9 && (re - shot.end().seconds()).abs() < 1e-9, "{r}");
    let pb = ClipId(r["clips"][1].as_u64().unwrap());
    // V1: the shot up to the overlap (keeps its id and its full-frame layout), then the overlay
    // piece, then the next shot; V2: the shot's piece, then the rest of the overlay
    let v1_spans = spans(&s, 0);
    let k = v1_spans.iter().position(|x| x.2 == shot.item).unwrap();
    assert_eq!(v1_spans[k], (shot.start.seconds(), rs, shot.item));
    assert_eq!(v1_spans[k + 1], (rs, re, overlay.item));
    assert_eq!(spans(&s, 1), vec![(rs, re, shot.item), (re, overlay.end().seconds(), overlay.item)]);
    assert_eq!(inspect(&mut s, v1)["at"], json!("full"), "the piece before the overlap is not changed");
    assert_eq!(track_of(&s, v1), 0);
    // the pieces keep their media: same source in at the same time
    let q = s.active_sequence().unwrap();
    let (_, piece) = q.find_item(pb).unwrap();
    assert_eq!(piece.source_time_at(piece.start), shot.source_time_at(piece.start));
    // the rest of the overlay (on V2, after the shot ended) keeps its circle bottom right
    let rest = q.video_tracks[1].items[1].id;
    let i = inspect(&mut s, rest);
    assert_eq!((i["at"].clone(), i["shape"].clone()), (json!("bottomRight"), json!("circle")), "{i}");
    // one undo restores everything
    assert_eq!(s.undo().as_deref(), Some("Swap Layouts"));
    assert_eq!(serde_json::to_value(&*s.project).unwrap(), serde_json::to_value(&*before).unwrap());
}

#[test]
fn swap_refuses_clips_on_one_track_or_apart_in_time() {
    let (mut s, v1, v2) = session();
    let q = s.active_sequence().unwrap();
    let first = q.video_tracks[0].items[0].id;
    let e = s.execute("layout.swap", json!({"clips": [v1.0, first.0]})).unwrap_err();
    assert!(e.to_string().contains("same track"), "{e}");
    let e = s.execute("layout.swap", json!({"clips": [first.0, v2.0]})).unwrap_err();
    assert!(e.to_string().contains("overlap"), "{e}");
    // a locked track
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    assert!(s.execute("layout.swap", json!({})).unwrap_err().to_string().contains("locked"));
}

/// The overlay becomes a "camera": its own media item (Bars and Tone) with linked audio on A3 (A2 holds the music) and a
/// transcript, like a face camera over a screen recording.
fn camera_session() -> (Session, ClipId, ClipId, filmcraft_project::ItemId) {
    let (mut s, v1, v2) = session();
    let cam = *s.project.items.iter().find(|(_, i)| i.name == "Bars and Tone").unwrap().0;
    let seq = s.state.active_sequence.unwrap();
    s.edit("test: camera", |p, _| {
        let q = p.sequence(seq).unwrap();
        let rate = q.settings.frame_rate;
        let ov = q.find_item(v2).unwrap().1.clone();
        let mut a =
            p.make_track_item(cam, filmcraft_project::TrackKind::Audio, ov.start, filmcraft_time::TimeRange::new(Tick::ZERO, ov.duration), rate).unwrap();
        let link = p.alloc_id();
        a.link = Some(link);
        let q = p.sequence_mut(seq).unwrap();
        let (_, v) = q.find_item_mut(v2).unwrap();
        v.item = cam;
        v.source_in = Tick::ZERO;
        v.link = Some(link);
        q.audio_tracks[2].items.push(a);
        Ok(())
    })
    .unwrap();
    let sec = |x: f64| Tick((x * filmcraft_time::TICKS_PER_SECOND as f64).round() as i64);
    // six words 0.2 s long every 0.25 s from media 1.0 s (timeline 7.0–8.45 s)
    let words: Vec<filmcraft_project::Word> = ["one", "two", "three", "four", "five", "six"]
        .iter()
        .enumerate()
        .map(|(i, w)| filmcraft_project::Word::new(*w, sec(1.0 + i as f64 * 0.25), sec(1.0 + i as f64 * 0.25 + 0.2)))
        .collect();
    let t = filmcraft_project::Transcript { language: "en".into(), words, ..Default::default() };
    s.execute("transcript.set", json!({"item": cam.0, "transcript": serde_json::to_value(&t).unwrap()})).unwrap();
    (s, v1, v2, cam)
}

fn live_text(s: &mut Session) -> String {
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    r["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect::<Vec<_>>().join(" ")
}

/// What a video track shows at `t`: (media item, media time).
fn shown(s: &Session, track: usize, t: Tick) -> Option<(filmcraft_project::ItemId, i64)> {
    let q = s.active_sequence().unwrap();
    q.video_tracks[track].item_at(t).map(|i| (i.item, i.source_time_at(t).0))
}

#[test]
fn extract_after_swap_cuts_both_tracks_alike() {
    let (mut s, _v1, v2, cam) = camera_session();
    s.execute("layout.place", json!({"clips": [v2.0], "at": "bottomRight"})).unwrap();
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "circle"})).unwrap();
    assert_eq!(live_text(&mut s), "one two three four five six");
    let r = s.execute("layout.swap", json!({})).unwrap();
    assert_eq!(track_of(&s, v2), 0, "the camera piece is on V1 now: {r}");
    assert_eq!(live_text(&mut s), "one two three four five six", "the audio carries the words, untouched by the swap");
    let fd = s.sequence_rate().frame_duration();
    let samples: Vec<Tick> = (0..200).map(|k| Tick(6 * filmcraft_time::TICKS_PER_SECOND) + Tick(fd.0 * k / 4)).collect();
    let before: Vec<_> = samples.iter().map(|t| (shown(&s, 0, *t), shown(&s, 1, *t))).collect();
    // extract "three four" (inside the swapped span)
    let cut = s.execute("transcript.extract", json!({"from": 2, "to": 3})).unwrap();
    let (cs, ce) = (Tick(cut["start"].as_i64().unwrap()), Tick(cut["end"].as_i64().unwrap()));
    assert!(cs.seconds() > 7.0 && ce.seconds() < 9.5, "{cut}");
    assert_eq!(live_text(&mut s), "one two five six");
    // both video tracks lost the same range: after the cut each shows what it showed `len` later
    let len = ce - cs;
    for (k, t) in samples.iter().enumerate() {
        let src = if *t < cs { *t } else { *t + len };
        let Some(j) = samples.iter().position(|x| *x == src) else { continue };
        assert_eq!((shown(&s, 0, *t), shown(&s, 1, *t)), before[j], "at sample {k} ({:.3} s)", t.seconds());
    }
    // the camera is still on V1 around the cut, the screen on V2
    assert_eq!(shown(&s, 0, cs).map(|x| x.0), Some(cam));
    assert_ne!(shown(&s, 1, cs).map(|x| x.0), Some(cam));
}

#[test]
fn swap_needs_two_visible_clips() {
    let (mut s, v1, _) = session();
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    assert!(s.execute("layout.swap", json!({})).is_err());
    assert!(s.execute("layout.swap", json!({"clips": [v1.0]})).is_err());
    assert!(s.execute("layout.swap", json!({"clips": [v1.0, v1.0]})).is_err());
}

#[test]
fn undo_after_place_restores_the_position() {
    let (mut s, _, v2) = session();
    let (pos, scale) = motion(&s, v2);
    s.execute("layout.place", json!({"clips": [v2.0], "at": "topLeft"})).unwrap();
    assert_ne!(motion(&s, v2).0, pos);
    assert_eq!(s.undo().as_deref(), Some("Place Clip"));
    assert_eq!(motion(&s, v2), (pos, scale));
}

#[test]
fn pick_returns_the_top_clip_first_and_skips_disabled_clips() {
    let (mut s, v1, v2) = session();
    let before = s.project.clone();
    let r = s.execute("layout.pick", json!({"x": 1540, "y": 820})).unwrap();
    assert_eq!(r["clips"], json!([v2.0, v1.0]));
    let r = s.execute("layout.pick", json!({"x": 100, "y": 100})).unwrap();
    assert_eq!(r["clips"], json!([v1.0]), "outside the overlay");
    let r = s.execute("layout.pick", json!({"x": -5, "y": 100})).unwrap();
    assert_eq!(r["clips"], json!([]), "outside the frame");
    assert!(std::sync::Arc::ptr_eq(&before, &s.project), "read-only");
    // the overlay's span ends at 10 s
    let r = s.execute("layout.pick", json!({"x": 1540, "y": 820, "seconds": 11.0})).unwrap();
    assert!(!r["clips"].as_array().unwrap().contains(&json!(v2.0)), "{r}");
    let seq = s.state.active_sequence.unwrap();
    s.edit("test: disable", |p, _| {
        p.sequence_mut(seq).unwrap().find_item_mut(v2).unwrap().1.enabled = false;
        Ok(())
    })
    .unwrap();
    let r = s.execute("layout.pick", json!({"x": 1540, "y": 820})).unwrap();
    assert_eq!(r["clips"], json!([v1.0]));
    assert!(s.execute("layout.pick", json!({"x": "left"})).is_err());
}

#[test]
fn graphic_clips_are_refused() {
    let (mut s, _, _) = session();
    let r = s.execute("graphics.newText", json!({"text": "Hello"})).unwrap();
    let g = r["clip"].as_u64().unwrap();
    assert!(!s.is_enabled("layout.place"), "a selected graphic disables layouts");
    let e = s.execute("layout.place", json!({"clips": [g], "at": "topLeft"})).unwrap_err();
    assert!(e.to_string().contains("graphic"), "{e}");
    assert!(s.execute("layout.shape", json!({"clips": [g], "shape": "circle"})).is_err());
    assert!(s.execute("layout.inspect", json!({"clips": [g]})).is_err());
    // swap at the playhead ignores the graphic on top
    let r = s.execute("layout.swap", json!({})).unwrap();
    assert!(!r["clips"].as_array().unwrap().contains(&json!(g)));
}

#[test]
fn keyframed_position_gets_a_keyframe_per_place() {
    let (mut s, _, v2) = session();
    s.execute("effects.toggleAnimation", json!({"clip": v2.0, "param": "position"})).unwrap();
    s.execute("layout.place", json!({"clips": [v2.0], "at": "topLeft"})).unwrap();
    s.execute("playhead.set", json!({"seconds": 8.5})).unwrap();
    s.execute("layout.place", json!({"clips": [v2.0], "at": "bottomRight"})).unwrap();
    let (pos, _) = motion(&s, v2);
    assert_eq!(pos.keyframes.len(), 2, "{pos:?}");
    assert_eq!(inspect(&mut s, v2)["at"], json!("bottomRight"));
    s.execute("playhead.set", json!({"seconds": 7.0})).unwrap();
    assert_eq!(inspect(&mut s, v2)["at"], json!("topLeft"));
}

#[test]
fn hostile_params() {
    let (mut s, v1, v2) = session();
    for p in [
        json!({"clips": [v2.0]}),
        json!({"clips": [v2.0], "at": "nowhere"}),
        json!({"clips": [v2.0], "at": 7}),
        json!({"clips": [999_999], "at": "top"}),
        json!({"clips": [], "at": "top"}),
        json!({"clips": "x", "at": "top"}),
    ] {
        assert!(s.execute("layout.place", p.clone()).is_err(), "{p}");
    }
    for p in [json!({"clips": [v2.0], "at": "top", "size": "big", "margin": null}), json!({"clips": [v2.0], "at": "top", "size": -1e308, "margin": 1e308})] {
        s.execute("layout.place", p).unwrap();
        let r = inspect(&mut s, v2);
        assert!(bx(&r).iter().all(|v| v.is_finite()), "{r}");
    }
    assert!(s.execute("layout.shape", json!({"clips": [v2.0], "shape": "blob"})).is_err());
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "rounded", "radius": 1e9})).unwrap();
    assert_eq!(inspect(&mut s, v2)["radius"], json!(50.0));
    // nothing selected and nothing passed
    s.state.selection.clear();
    assert!(s.execute("layout.place", json!({"at": "top"})).is_err());
    s.state.selection = vec![v1];
    s.execute("layout.place", json!({"at": "top"})).unwrap();
    assert_eq!(inspect(&mut s, v1)["at"], json!("top"));
    assert!(s.execute("layout.pick", json!({"x": f64::MAX, "y": 1})).unwrap()["clips"].as_array().unwrap().is_empty());
}

#[test]
fn set_moves_and_scales_as_one_merged_step() {
    let (mut s, a, _) = session();
    let before = s.history.undo.len();
    let (p0, _) = motion(&s, a);
    // a drag: several merged calls, one undo step; `begin` starts the next drag's step
    s.execute("layout.set", json!({"clips": [a.0], "position": [100.0, 80.0], "scale": 40.0, "merge": true, "begin": true})).unwrap();
    s.execute("layout.set", json!({"clips": [a.0], "position": [120.0, 90.0], "scale": 42.0, "merge": true})).unwrap();
    s.execute("layout.set", json!({"clips": [a.0], "position": [130.0, 95.0], "merge": true})).unwrap();
    assert_eq!(s.history.undo.len(), before + 1, "one undo step for the whole drag");
    let (p, sc) = motion(&s, a);
    assert_eq!(vec2(&p), (130.0, 95.0));
    assert_eq!(float(&sc), 42.0);
    s.execute("layout.set", json!({"clips": [a.0], "position": [10.0, 10.0], "merge": true, "begin": true})).unwrap();
    assert_eq!(s.history.undo.len(), before + 2, "begin starts a new step");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let (p, _) = motion(&s, a);
    assert_eq!(vec2(&p), vec2(&p0), "both steps undone");
    // hostile input
    assert!(s.execute("layout.set", json!({"clips": [a.0]})).is_err());
    assert!(s.execute("layout.set", json!({"clips": [a.0], "position": [1.0]})).is_err());
    assert!(s.execute("layout.set", json!({"clips": [a.0], "position": ["x", 2.0]})).is_err());
    s.execute("layout.set", json!({"clips": [a.0], "scale": 1e300})).unwrap();
    assert!(float(&motion(&s, a).1) <= filmcraft_edit::layout::MAX_SCALE);
}

#[test]
fn pan_moves_the_picture_not_the_box() {
    let (mut s, v1, v2) = session();
    // only a circle or square can pan
    assert!(s.execute("layout.pan", json!({"clips": [v2.0], "dx": -100})).unwrap_err().to_string().contains("circle, square or rounded"));
    s.execute("layout.place", json!({"clips": [v2.0], "at": "bottomRight", "size": 33})).unwrap();
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "circle"})).unwrap();
    let r0 = inspect(&mut s, v2);
    assert_eq!(r0["pan"], json!([0.0, 0.0]));
    let b0 = bx(&r0);
    let (p0, _) = motion(&s, v2);
    let before = s.history.undo.len();
    // a drag: merged calls, one undo step
    s.execute("layout.pan", json!({"clips": [v2.0], "dx": -100, "dy": 0, "merge": true, "begin": true})).unwrap();
    let r = s.execute("layout.pan", json!({"clips": [v2.0], "dx": -320, "merge": true})).unwrap();
    assert_eq!(r["pan"], json!([[-320.0, 0.0]]), "{r}");
    assert_eq!(s.history.undo.len(), before + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Pan Clip");
    let r1 = inspect(&mut s, v2);
    assert_eq!((r1["pan"].clone(), r1["shape"].clone(), r1["at"].clone()), (json!([-320.0, 0.0]), json!("circle"), json!("bottomRight")), "{r1}");
    let b1 = bx(&r1);
    assert!(b0.iter().zip(b1).all(|(a, b)| (a - b).abs() < 1e-6), "{b0:?} {b1:?}");
    // the picture moved right by the pan at the clip's scale
    let (p1, sc) = motion(&s, v2);
    assert!((vec2(&p1).0 - vec2(&p0).0 - 320.0 * float(&sc) / 100.0).abs() < 1e-6, "{p1:?} {p0:?}");
    // clamped to the source (1920 × 1080: ±420 sideways, none vertically); NaN / junk → 0
    s.execute("layout.pan", json!({"clips": [v2.0], "dx": -1e9, "dy": 1e9})).unwrap();
    assert_eq!(inspect(&mut s, v2)["pan"], json!([-420.0, 0.0]));
    s.execute("layout.pan", json!({"clips": [v2.0], "dx": "left"})).unwrap();
    assert_eq!(inspect(&mut s, v2)["pan"], json!([0.0, 0.0]));
    s.execute("layout.pan", json!({"clips": [v2.0], "dx": 200})).unwrap();
    assert!(s.execute("layout.pan", json!({"clips": [v2.0]})).is_err(), "nothing to set");
    // place, shape keep the pan; the box is still where the place puts it
    s.execute("layout.place", json!({"clips": [v2.0], "at": "topLeft", "size": 25})).unwrap();
    let r = inspect(&mut s, v2);
    assert_eq!((r["pan"].clone(), r["at"].clone()), (json!([200.0, 0.0]), json!("topLeft")), "{r}");
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "square"})).unwrap();
    let r = inspect(&mut s, v2);
    assert_eq!((r["pan"].clone(), r["at"].clone(), r["shape"].clone()), (json!([200.0, 0.0]), json!("topLeft"), json!("square")), "{r}");
    // rounded cannot pan: the pan goes back to 0
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "rounded"})).unwrap();
    assert_eq!(inspect(&mut s, v2)["pan"], json!([0.0, 0.0]));
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "circle"})).unwrap();
    s.execute("layout.pan", json!({"clips": [v2.0], "dx": -300})).unwrap();
    // swap carries the pan to the other clip
    let r = s.execute("layout.swap", json!({})).unwrap();
    let pb = ClipId(r["clips"][1].as_u64().unwrap());
    let i = inspect(&mut s, pb);
    assert_eq!((i["pan"].clone(), i["shape"].clone()), (json!([-300.0, 0.0]), json!("circle")), "{i}");
    assert_eq!(inspect(&mut s, v2)["pan"], json!([0.0, 0.0]));
    // undo goes back step by step
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(inspect(&mut s, v2)["pan"], json!([-300.0, 0.0]));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(inspect(&mut s, v2)["pan"], json!([0.0, 0.0]));
    let _ = v1;
}

fn zoom_of(v: &Value) -> f64 {
    v["zoom"].as_f64().unwrap()
}

fn same_box(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01)
}

#[test]
fn zoom_grows_the_picture_not_the_box() {
    let (mut s, _, v2) = session();
    // only a circle, square or rounded shape can zoom
    let e = s.execute("layout.zoom", json!({"clips": [v2.0], "zoom": 1.5})).unwrap_err();
    assert!(e.to_string().contains("circle, square or rounded"), "{e}");
    s.execute("layout.place", json!({"clips": [v2.0], "at": "bottomRight", "size": 33})).unwrap();
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "circle"})).unwrap();
    assert!(s.execute("layout.zoom", json!({"clips": [v2.0]})).is_err(), "nothing to set");
    assert!(s.execute("layout.zoom", json!({"clips": [v2.0], "zoom": 2, "by": 2})).is_err(), "one of the two");
    let r0 = inspect(&mut s, v2);
    assert_eq!(zoom_of(&r0), 1.0, "{r0}");
    let b0 = bx(&r0);
    let (_, sc0) = motion(&s, v2);
    let before = s.history.undo.len();
    // absolute: the box stays, the scale grows by the zoom
    let r = s.execute("layout.zoom", json!({"clips": [v2.0], "zoom": 1.5})).unwrap();
    assert_eq!(r["zoom"], json!([1.5]), "{r}");
    assert_eq!(s.history.undo.len(), before + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Zoom Clip");
    let r1 = inspect(&mut s, v2);
    assert_eq!((zoom_of(&r1), r1["shape"].clone(), r1["at"].clone()), (1.5, json!("circle"), json!("bottomRight")), "{r1}");
    assert!(same_box(b0, bx(&r1)), "{b0:?} {r1}");
    let (_, sc1) = motion(&s, v2);
    assert!((float(&sc1) - float(&sc0) * 1.5).abs() < 1e-6, "{sc1:?} {sc0:?}");
    // relative
    s.execute("layout.zoom", json!({"clips": [v2.0], "by": 2})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 3.0);
    s.execute("layout.zoom", json!({"clips": [v2.0], "by": 0.5})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 1.5);
    assert!(same_box(b0, bx(&inspect(&mut s, v2))));
    // scroll notches: merged into one undo step; `begin` starts the next
    let n = s.history.undo.len();
    s.execute("layout.zoom", json!({"clips": [v2.0], "by": 1.05, "merge": true, "begin": true})).unwrap();
    for _ in 0..4 {
        s.execute("layout.zoom", json!({"clips": [v2.0], "by": 1.05, "merge": true})).unwrap();
    }
    assert_eq!(s.history.undo.len(), n + 1, "one undo step for the notches");
    let z = zoom_of(&inspect(&mut s, v2));
    assert!((z - 1.5 * 1.05f64.powi(5)).abs() < 1e-3, "{z}");
    assert!(same_box(b0, bx(&inspect(&mut s, v2))));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 1.5, "undo takes the notches back together");
    // clamping: 1–8; junk counts as 1 (`zoom`) or no change (`by`)
    s.execute("layout.zoom", json!({"clips": [v2.0], "zoom": 100})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 8.0);
    s.execute("layout.zoom", json!({"clips": [v2.0], "by": "lots"})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 8.0);
    s.execute("layout.zoom", json!({"clips": [v2.0], "zoom": -3})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 1.0);
    s.execute("layout.zoom", json!({"clips": [v2.0], "by": 1e300})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 8.0);
    s.execute("layout.zoom", json!({"clips": [v2.0], "zoom": "big"})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 1.0);
    assert!(same_box(b0, bx(&inspect(&mut s, v2))));
    // the pan has more room in a zoomed shape (1920 × 1080 at zoom 2: ±690 × ±270) and keeps the zoom
    s.execute("layout.zoom", json!({"clips": [v2.0], "zoom": 2})).unwrap();
    s.execute("layout.pan", json!({"clips": [v2.0], "dx": -1e9, "dy": 1e9})).unwrap();
    let r = inspect(&mut s, v2);
    assert_eq!((r["pan"].clone(), zoom_of(&r)), (json!([-690.0, 270.0]), 2.0), "{r}");
    assert!(same_box(b0, bx(&r)), "{r}");
    // zooming out again clamps the pan to the larger shape
    s.execute("layout.zoom", json!({"clips": [v2.0], "zoom": 1})).unwrap();
    let r = inspect(&mut s, v2);
    assert_eq!((r["pan"].clone(), zoom_of(&r)), (json!([-420.0, 0.0]), 1.0), "{r}");
    assert!(same_box(b0, bx(&r)), "{r}");
    // place and shape keep the zoom
    s.execute("layout.zoom", json!({"clips": [v2.0], "zoom": 2})).unwrap();
    s.execute("layout.place", json!({"clips": [v2.0], "at": "topLeft", "size": 25})).unwrap();
    let r = inspect(&mut s, v2);
    assert_eq!((zoom_of(&r), r["at"].clone()), (2.0, json!("topLeft")), "{r}");
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "square"})).unwrap();
    let r = inspect(&mut s, v2);
    assert_eq!((zoom_of(&r), r["at"].clone(), r["shape"].clone()), (2.0, json!("topLeft"), json!("square")), "{r}");
    // a zoomed rounded rectangle can pan (960 × 540 inside 1920 × 1080: ±480 × ±270)
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "rounded"})).unwrap();
    s.execute("layout.pan", json!({"clips": [v2.0], "dx": -1e9, "dy": 0})).unwrap();
    let r = inspect(&mut s, v2);
    assert_eq!((zoom_of(&r), r["pan"].clone(), r["shape"].clone(), r["at"].clone()), (2.0, json!([-480.0, 0.0]), json!("rounded"), json!("topLeft")), "{r}");
    // free has no zoom
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "free"})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 1.0);
    // swap carries pan and zoom to the other clip
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "circle"})).unwrap();
    s.execute("layout.zoom", json!({"clips": [v2.0], "zoom": 1.25})).unwrap();
    s.execute("layout.pan", json!({"clips": [v2.0], "dx": -200})).unwrap();
    let r = s.execute("layout.swap", json!({})).unwrap();
    let pb = ClipId(r["clips"][1].as_u64().unwrap());
    let i = inspect(&mut s, pb);
    assert_eq!((i["pan"].clone(), zoom_of(&i), i["shape"].clone()), (json!([-200.0, 0.0]), 1.25, json!("circle")), "{i}");
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 1.0);
    // undo goes back step by step
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 1.25);
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(zoom_of(&inspect(&mut s, v2)), 1.0);
}
