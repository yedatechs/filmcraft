//! `layout.*` on the demo project: V1 holds six shots, V2 a scaled-down overlay from 6 s to 10 s.
//! At 7 s both V1's second shot and the overlay are visible.

use serde_json::{Value, json};

use crate::Session;
use crate::layout::LAYOUT_MASK;
use filmcraft_project::{ClipId, ItemKind, ParamValue};

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

#[test]
fn swap_twice_restores_both_clips() {
    let (mut s, v1, v2) = session();
    s.execute("layout.place", json!({"clips": [v2.0], "at": "bottomRight"})).unwrap();
    s.execute("layout.shape", json!({"clips": [v2.0], "shape": "circle"})).unwrap();
    let (m1, m2) = (motion(&s, v1), motion(&s, v2));
    let (k1, k2) = (layout_masks(&s, v1), layout_masks(&s, v2));
    let r = s.execute("layout.swap", json!({})).unwrap();
    assert_eq!(r["clips"], json!([v2.0, v1.0]), "top-most first");
    assert_eq!(s.history.undo.last().unwrap().0, "Swap Layouts");
    // the V1 clip is now the bottom-right circle, the overlay fills the frame
    let a = inspect(&mut s, v1);
    assert_eq!((a["at"].clone(), a["shape"].clone()), (json!("bottomRight"), json!("circle")), "{a}");
    let b = inspect(&mut s, v2);
    assert_eq!((b["at"].clone(), b["shape"].clone()), (json!("full"), json!("free")), "{b}");
    s.execute("layout.swap", json!({"clips": [v1.0, v2.0]})).unwrap();
    for (c, (pos, scale), masks) in [(v1, m1, k1), (v2, m2, k2)] {
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
