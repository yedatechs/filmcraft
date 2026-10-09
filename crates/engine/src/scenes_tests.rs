//! Scenes (`scenes.*`): the demo project with a face clip (other media) on V2 over the first V1
//! clip, and a transcript on the first V1 clip's media (1.0–6.0 s, timeline 0–5 s) with two
//! paragraphs.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::Session;
use filmcraft_project::{ClipId, Interpolation, ItemId, ParamValue, TrackItem, TrackKind, Transcript, Word};
use filmcraft_speech::FixedTranscriber;
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};

fn sec(x: f64) -> Tick {
    Tick((x * TICKS_PER_SECOND as f64).round() as i64)
}

fn line(words: &[&str], t0: f64) -> Vec<Word> {
    words.iter().enumerate().map(|(i, w)| Word::new(*w, sec(t0 + i as f64 * 0.25), sec(t0 + i as f64 * 0.25 + 0.2))).collect()
}

struct Fx {
    s: Session,
    screen: ItemId,
    face: ItemId,
    face_clip: ClipId,
}

fn session() -> Fx {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    let q = s.active_sequence().unwrap();
    let screen = q.video_tracks[0].items[0].item;
    let face = q.video_tracks[0].items[1].item;
    let rate = q.settings.frame_rate;
    let p = Arc::make_mut(&mut s.project);
    let face_item: TrackItem = p.make_track_item(face, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.snap(sec(5.0))), rate).unwrap();
    let face_clip = face_item.id;
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks[1].items = vec![face_item];
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    t.words.extend(line(&["We", "start", "the", "show."], 1.2)); // 1.2–2.15
    t.words.extend(line(&["Here", "is", "the", "screen."], 3.5)); // 3.5–4.45
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    s.execute("transcript.generate", json!({"items": [screen.0]})).unwrap();
    Fx { s, screen, face, face_clip }
}

fn clip(s: &Session, id: ClipId) -> &TrackItem {
    s.active_sequence().unwrap().find_item(id).unwrap().1
}

fn first_screen_clip(s: &Session, item: ItemId) -> ClipId {
    s.active_sequence().unwrap().video_tracks[0].items.iter().find(|i| i.item == item).unwrap().id
}

fn scene_id(s: &mut Session, name: &str) -> u64 {
    let l = s.execute("scenes.list", json!({})).unwrap();
    l["scenes"].as_array().unwrap().iter().find(|x| x["name"] == json!(name)).unwrap()["id"].as_u64().unwrap()
}

fn key_times(it: &TrackItem, effect: &str, param: &str) -> Vec<Tick> {
    it.effect(effect).unwrap().param(param).unwrap().keyframes.iter().map(|k| k.time).collect()
}

#[test]
fn defaults_create_four_scenes() {
    let mut f = session();
    assert!(!f.s.is_enabled("scenes.assign"), "no scenes yet");
    let r = f.s.execute("scenes.defaults", json!({})).unwrap();
    assert_eq!(r["screen"], json!(f.screen.0));
    assert_eq!(r["face"], json!(f.face.0));
    let l = f.s.execute("scenes.list", json!({})).unwrap();
    let names: Vec<&str> = l["scenes"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Screen with face", "Face", "Screen", "Half and half"]);
    assert_eq!(l["scenes"][0]["slots"][1]["shape"], json!("circle"));
    assert_eq!(l["scenes"][0]["slots"][1]["place"], json!("bottomRight"));
    assert_eq!(l["scenes"][1]["slots"][1]["hidden"], json!(true));
    // a second run adds nothing
    assert!(f.s.execute("scenes.defaults", json!({})).is_err());
    // the default scene is applied at once: the face is a bottom-right circle
    let ins = f.s.execute("layout.inspect", json!({"clips": [f.face_clip.0], "time": 0})).unwrap();
    assert_eq!(ins["clips"][0]["at"], json!("bottomRight"), "{ins}");
    assert_eq!(ins["clips"][0]["shape"], json!("circle"), "{ins}");
}

#[test]
fn assign_writes_hold_keyframes_and_apply_is_idempotent() {
    let mut f = session();
    f.s.execute("scenes.defaults", json!({})).unwrap();
    let face_scene = scene_id(&mut f.s, "Face");
    // paragraph 2 = words 4..=7, media 3.5 s = sequence 2.5 s
    f.s.execute("scenes.assign", json!({"scene": face_scene, "from": 4, "to": 7})).unwrap();
    let l = f.s.execute("scenes.list", json!({})).unwrap();
    assert_eq!(l["spans"].as_array().unwrap().len(), 1, "{l}");
    assert_eq!(l["spans"][0]["from"], json!(4));
    assert_eq!(l["spans"][0]["to"], json!(7));
    let span_seq = Tick(l["spans"][0]["seqStart"].as_i64().unwrap());
    let words = f.s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(json!(span_seq.0), words["words"][4]["start"], "the span starts with its first word");
    f.s.execute("scenes.apply", json!({})).unwrap();
    let sc = first_screen_clip(&f.s, f.screen);
    for id in [sc, f.face_clip] {
        let it = clip(&f.s, id);
        let mt = it.source_time_at(span_seq);
        let pos = it.effect("motion").unwrap().param("position").unwrap();
        assert!(pos.keyframes.iter().all(|k| k.interp == Interpolation::Hold));
        // clip start, span start, and the span's end (back to the default scene)
        let times = key_times(it, "motion", "position");
        assert_eq!(times.get(..2), Some(&[it.source_in, mt][..]), "{times:?}");
        assert_eq!(times.len(), 3, "{times:?}");
        assert_eq!(key_times(it, "opacity", "opacity"), times);
    }
    // in the Face scene the screen is hidden and the face is full
    let o = clip(&f.s, sc).effect("opacity").unwrap().param("opacity").unwrap().value_at(clip(&f.s, sc).source_time_at(sec(3.0)));
    assert_eq!(o, ParamValue::Float(0.0));
    let ins = f.s.execute("layout.inspect", json!({"clips": [f.face_clip.0], "time": sec(3.0).0})).unwrap();
    assert_eq!(ins["clips"][0]["at"], json!("full"), "{ins}");
    let ins = f.s.execute("layout.inspect", json!({"clips": [f.face_clip.0], "time": sec(1.0).0})).unwrap();
    assert_eq!(ins["clips"][0]["at"], json!("bottomRight"), "{ins}");
    // the other V1 clips (other media) are untouched
    let other = &f.s.active_sequence().unwrap().video_tracks[0].items[2];
    assert!(!other.effect("motion").unwrap().param("position").unwrap().is_animated());
    // idempotent
    let before = f.s.project.clone();
    let undo = f.s.history.undo.len();
    let r = f.s.execute("scenes.apply", json!({})).unwrap();
    assert_eq!(r["changed"], json!(false));
    assert!(Arc::ptr_eq(&f.s.project, &before));
    assert_eq!(f.s.history.undo.len(), undo);
    // undo takes the assignment back in one step
    f.s.execute("edit.undo", json!({})).unwrap();
    assert!(f.s.execute("scenes.list", json!({})).unwrap()["spans"].as_array().unwrap().is_empty());
}

#[test]
fn transcript_edit_reapplies_in_the_same_undo_step() {
    let mut f = session();
    f.s.execute("scenes.defaults", json!({})).unwrap();
    let face_scene = scene_id(&mut f.s, "Face");
    f.s.execute("scenes.assign", json!({"scene": face_scene, "from": 4, "to": 7})).unwrap();
    let undo = f.s.history.undo.len();
    // cut the first paragraph's first two words: the second paragraph moves earlier
    f.s.execute("transcript.extract", json!({"from": 0, "to": 1})).unwrap();
    assert_eq!(f.s.history.undo.len(), undo + 1, "one undo step");
    let l = f.s.execute("scenes.list", json!({})).unwrap();
    let span_seq = Tick(l["spans"][0]["seqStart"].as_i64().unwrap());
    assert!(span_seq < sec(2.5), "{l}");
    // every owned clip has a keyframe at its own start and at the span's new sequence time
    let q = f.s.active_sequence().unwrap();
    let owned: Vec<&TrackItem> = q.video_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| i.item == f.screen || i.item == f.face).collect();
    assert!(owned.len() >= 3, "the extract split the clips");
    for it in &owned {
        let times = key_times(it, "motion", "position");
        assert!(times.contains(&it.source_time_at(it.start)), "keyframe at the clip start: {times:?}");
        if it.start < span_seq && span_seq < it.end() {
            assert!(times.contains(&it.source_time_at(span_seq)), "keyframe at the span: {times:?}");
        }
    }
    // the hook left nothing for apply to do
    assert_eq!(f.s.execute("scenes.apply", json!({})).unwrap()["changed"], json!(false));
}

#[test]
fn save_load_round_trips_scenes_and_spans() {
    let mut f = session();
    f.s.execute("scenes.defaults", json!({})).unwrap();
    f.s.execute("scenes.assign", json!({"scene": "Half and half", "from": 0, "to": 3})).unwrap();
    let bytes = filmcraft_format::encode(&f.s.project, true);
    let back = filmcraft_format::decode(&bytes).unwrap().project;
    assert_eq!(serde_json::to_string(&back).unwrap(), serde_json::to_string(&*f.s.project).unwrap());
    let seq = f.s.state.active_sequence.unwrap();
    assert_eq!(back.sequence(seq).unwrap().scenes.len(), 4);
    assert_eq!(back.transcripts[&f.screen].scenes.len(), 1);
}

#[test]
fn update_remove_clear_and_hostile_params() {
    let mut f = session();
    f.s.execute("scenes.defaults", json!({})).unwrap();
    let id = scene_id(&mut f.s, "Screen");
    f.s.execute(
        "scenes.update",
        json!({"scene": id, "name": "Just screen", "slots": [{"item": f.screen.0, "place": "center", "size": 60, "shape": "rounded", "radius": 20}]}),
    )
    .unwrap();
    let l = f.s.execute("scenes.list", json!({})).unwrap();
    assert_eq!(l["scenes"][2]["name"], json!("Just screen"));
    assert_eq!(l["scenes"][2]["slots"][0]["shape"], json!("rounded"));
    f.s.execute("scenes.assign", json!({"scene": id, "from": 0, "to": 7})).unwrap();
    f.s.execute("scenes.clear", json!({"from": 0, "to": 3})).unwrap();
    let l = f.s.execute("scenes.list", json!({})).unwrap();
    assert_eq!(l["spans"].as_array().unwrap().len(), 1);
    assert_eq!(l["spans"][0]["from"], json!(4));
    f.s.execute("scenes.remove", json!({"scene": id})).unwrap();
    let l = f.s.execute("scenes.list", json!({})).unwrap();
    assert_eq!(l["scenes"].as_array().unwrap().len(), 3);
    assert!(l["spans"].as_array().unwrap().is_empty(), "the removed scene's spans go too");
    let add = f.s.execute("scenes.add", json!({"name": "Mine", "slots": [{"item": f.face.0, "place": "topLeft", "shape": "square"}]})).unwrap();
    assert!(add["scene"].as_u64().is_some());
    let hostile: Vec<(&str, Value)> = vec![
        ("scenes.assign", json!({"scene": 999999, "from": 0})),
        ("scenes.assign", json!({"scene": "Face", "from": u64::MAX, "to": 0})),
        ("scenes.assign", json!({"scene": "Face", "from": 5, "to": 2})),
        ("scenes.assign", json!({"scene": [], "from": 0})),
        ("scenes.clear", json!({"from": 1e300})),
        ("scenes.add", json!({"slots": "nope"})),
        ("scenes.add", json!({"slots": [{"item": 0}]})),
        ("scenes.add", json!({"slots": [{"item": f.face.0, "place": "nowhere"}]})),
        ("scenes.add", json!({"slots": [{"item": f.face.0, "shape": "star"}]})),
        ("scenes.add", json!({"slots": [{"item": f.face.0}, {"item": f.face.0}]})),
        ("scenes.add", json!({"name": 5})),
        ("scenes.add", json!({"name": "  "})),
        ("scenes.update", json!({"scene": null})),
        ("scenes.remove", json!({"scene": -3})),
    ];
    for (cmd, p) in hostile {
        assert!(f.s.execute(cmd, p.clone()).is_err(), "{cmd} {p}");
    }
    // numbers out of range are clamped, not refused
    let r = f.s.execute(
        "scenes.add",
        json!({"name": "x".repeat(10_000), "slots": [{"item": f.face.0, "size": 1e308, "margin": -1e308, "radius": f64::MAX, "place": "full"}, {"item": f.screen.0, "hidden": true}]}),
    );
    assert!(r.is_ok(), "{r:?}");
    let many: Vec<Value> = (0..100).map(|i| json!({"item": i})).collect();
    assert!(f.s.execute("scenes.add", json!({"slots": many})).is_err());
    assert_eq!(f.s.execute("scenes.apply", json!({})).unwrap()["changed"], json!(false));
}

#[test]
fn scene_owning_names_the_scene() {
    let mut f = session();
    assert_eq!(crate::scenes::scene_owning(&f.s, f.face_clip), None);
    f.s.execute("scenes.defaults", json!({})).unwrap();
    assert_eq!(crate::scenes::scene_owning(&f.s, f.face_clip).as_deref(), Some("Screen with face"));
    let other = f.s.active_sequence().unwrap().video_tracks[0].items[2].id;
    assert_eq!(crate::scenes::scene_owning(&f.s, other), None);
}

/// The spec scenario: a paragraph after a take group keeps its scene when a longer take is
/// switched in and the paragraph moves later.
#[test]
fn switching_takes_moves_the_scene_with_its_words() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    let q = s.active_sequence().unwrap();
    let screen = q.video_tracks[0].items[0].item;
    let face = q.video_tracks[0].items[1].item;
    let rate = q.settings.frame_rate;
    let p = Arc::make_mut(&mut s.project);
    let face_item = p.make_track_item(face, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.snap(sec(5.0))), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[1].items = vec![face_item];
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    t.words.extend(line(&["We", "start", "the", "show", "here,", "friends,", "today."], 1.2)); // 1.2–2.9
    t.words.extend(line(&["We", "start", "the", "show", "here."], 3.0)); // 3.0–4.2
    t.words.extend(line(&["Then", "more", "words."], 4.6)); // 4.6–5.3
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    s.execute("transcript.generate", json!({"items": [screen.0]})).unwrap();
    s.execute("takes.detect", json!({})).unwrap();
    let groups = s.execute("takes.list", json!({})).unwrap()["groups"].clone();
    let gid = groups[0]["id"].as_u64().unwrap();
    assert_eq!(groups[0]["active"], json!(1), "{groups}");
    s.execute("scenes.defaults", json!({})).unwrap();
    // live words: the short take (5) then "Then more words." (5..=7)
    s.execute("scenes.assign", json!({"scene": "Face", "from": 5, "to": 7})).unwrap();
    let before = s.execute("scenes.list", json!({})).unwrap()["spans"][0]["seqStart"].as_i64().unwrap();
    let undo = s.history.undo.len();
    s.execute("takes.select", json!({"group": gid, "take": 0})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 1);
    let l = s.execute("scenes.list", json!({})).unwrap();
    let after = Tick(l["spans"][0]["seqStart"].as_i64().unwrap());
    assert!(after.0 > before, "the paragraph moved later: {before} → {}", after.0);
    assert_eq!(l["spans"][0]["from"], json!(7), "{l}");
    // the face is full and the screen hidden exactly from the paragraph's new start
    let q = s.active_sequence().unwrap();
    let at = |item: ItemId, t: Tick| q.video_tracks.iter().flat_map(|tr| tr.items.iter()).find(|i| i.item == item && i.start <= t && t < i.end()).unwrap();
    let sc = at(screen, after);
    let op = |it: &TrackItem, t: Tick| it.effect("opacity").unwrap().param("opacity").unwrap().value_at(it.source_time_at(t));
    assert_eq!(op(sc, after), ParamValue::Float(0.0));
    let just_before = Tick(after.0 - rate.frame_duration().0);
    assert_eq!(op(at(screen, just_before), just_before), ParamValue::Float(100.0));
    assert_eq!(s.execute("scenes.apply", json!({})).unwrap()["changed"], json!(false));
}
