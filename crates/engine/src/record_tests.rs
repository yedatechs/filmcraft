//! Tests of [`crate::record`]: synthetic screen + camera + microphone recordings, sync, undo,
//! refused parameters.

use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::record::{CapturedFrame, FrameQueue, PixelFormat, SourceKind, SyntheticFactory, check_name, read_synthetic_index, sync_offsets, synthetic_frame};
use filmcraft_media::FrameRequest;
use serde_json::{Value, json};

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("filmcraft-rec-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A session recording from small synthetic sources; the camera starts `camera_delay_ms` late.
fn session(camera_delay_ms: u64) -> Session {
    let mut s = Session::default();
    s.record.factory = Some(Arc::new(SyntheticFactory { display_size: (320, 180), camera_size: (320, 180), camera_delay_ms, ..Default::default() }));
    s
}

fn record(s: &mut Session, dir: &std::path::Path, secs: f64, stop: Value) -> Value {
    let r = s
        .execute(
            "record.start",
            json!({"screen": {"display": SyntheticFactory::DISPLAY}, "camera": {"device": SyntheticFactory::CAMERA}, "mic": {}, "dir": dir.to_string_lossy()}),
        )
        .unwrap();
    assert_eq!(r["files"].as_array().unwrap().len(), 3, "{r}");
    std::thread::sleep(Duration::from_secs_f64(secs));
    s.execute("record.stop", stop).unwrap()
}

fn sidecar(path: &str) -> Value {
    let p = std::path::Path::new(path);
    let side = p.with_file_name(format!("{}.recording.json", p.file_stem().unwrap().to_string_lossy()));
    serde_json::from_slice(&std::fs::read(side).unwrap()).unwrap()
}

fn clip(s: &Session, v: &Value, kind: &str) -> filmcraft_project::TrackItem {
    let id = filmcraft_project::ClipId(v["clips"][kind]["clip"].as_u64().unwrap());
    s.active_sequence().unwrap().find_item(id).unwrap().1.clone()
}

fn secs(t: Tick) -> f64 {
    t.seconds()
}

/// One frame at 30 fps, in ns.
const FRAME_NS: u64 = 33_333_334;

/// The tests that record in real time and then compare lengths and pictures frame by frame
/// take turns: the test harness runs a test per core, and several recordings at once starve
/// each other's capture and encoder threads on a busy machine (frames are then dropped and the
/// pictures no longer match to the frame).
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Where the moving white bar of a synthetic frame starts (`synthetic_frame` draws it at
/// `(capture time / 10 ms) mod span`): every synthetic source draws it from the same start clock,
/// so two frames captured together show it at the same place.
fn bar_x(luma: &[u8], w: u32, h: u32) -> Option<usize> {
    let (wu, hu) = (w as usize, h as usize);
    let row = luma.get(hu * 3 / 4 * wu..hu * 3 / 4 * wu + wu)?;
    row.iter().position(|&l| l > 180)
}

/// The capture times (as bar positions) of what two clips show at sequence time `t` differ by
/// at most `frames` frames (the bar moves 100 px per second).
fn same_moment(s: &mut Session, v: &Value, a: &str, b: &str, t: f64, frames: f64) {
    let mut at = Vec::new();
    for kind in [a, b] {
        let c = clip(s, v, kind);
        let item = filmcraft_project::ItemId(v["items"][kind].as_u64().unwrap());
        let src = s.media.source_for(&s.project, item, &*s.services).unwrap();
        let m = Tick::from_seconds_f64(t) - c.start + c.source_in;
        let f = src.video_frame(FrameRequest::full(m)).unwrap();
        let span = (f.width as usize).saturating_sub((f.width as usize / 32).max(2)).max(1);
        at.push((bar_x(&f.luma8(), f.width, f.height).unwrap(), span));
    }
    let span = at[0].1;
    let d = (at[0].0 as i64 - at[1].0 as i64).rem_euclid(span as i64) as f64;
    let d = d.min(span as f64 - d);
    assert!(d <= frames * 100.0 / 30.0 + 1.5, "at {t} s {a} shows the bar at {} and {b} at {}", at[0].0, at[1].0);
}

/// The burnt-in index of the frame a clip's file shows at media time `m` seconds.
fn index_at(s: &mut Session, v: &Value, kind: &str, m: f64) -> u64 {
    let item = filmcraft_project::ItemId(v["items"][kind].as_u64().unwrap());
    let src = s.media.source_for(&s.project, item, &*s.services).unwrap();
    let f = src.video_frame(FrameRequest::full(Tick::from_seconds_f64(m))).unwrap();
    read_synthetic_index(&f.luma8(), f.width, f.height).unwrap()
}

#[test]
fn devices_in_a_headless_session_are_synthetic() {
    let mut s = Session::default();
    let d = s.execute("record.devices", json!({})).unwrap();
    assert_eq!(d["displays"][0]["id"], SyntheticFactory::DISPLAY);
    assert_eq!(d["displays"][0]["width"], 1280);
    assert_eq!(d["windows"][0]["app"], "FilmCraft");
    assert_eq!(d["cameras"][0]["name"], "Synthetic Camera");
    assert_eq!(d["microphones"][0], "Synthetic Input");
    assert_eq!(d["permissions"]["screen"], "granted");
    assert!(d["error"].is_null());
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!(st["recording"], false);
}

#[test]
fn three_sources_three_files_one_synced_sequence() {
    let _turn = one_at_a_time();
    let dir = tmp("three");
    let mut s = session(300);
    let seqs_before = s.project.items.values().filter(|i| i.as_sequence().is_some()).count();
    let v = record(&mut s, &dir, 2.0, json!({}));
    assert_eq!(v["placed"], true, "{v}");
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    let files: Vec<String> = v["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
    assert_eq!(files.len(), 3);
    for (f, kind) in files.iter().zip(["Screen", "Camera", "Mic"]) {
        assert!(f.ends_with(&format!("Recording 1 - {kind}.{}", if kind == "Mic" { "wav" } else { "mov" })), "{f}");
        assert!(std::fs::metadata(f).unwrap().len() > 1000, "{f}");
    }
    // sidecars: one clock, first sample times, empty events
    let sides: Vec<Value> = files.iter().map(|f| sidecar(f)).collect();
    assert_eq!(sides[0]["source"], "screen");
    assert_eq!(sides[1]["source"], "camera");
    assert_eq!(sides[2]["source"], "mic");
    for sd in &sides {
        assert_eq!(sd["version"], 1);
        assert_eq!(sd["recording"], "Recording 1");
        assert_eq!(sd["clock_start_ns"], sides[0]["clock_start_ns"]);
        assert_eq!(sd["events"], json!([]));
    }
    assert_eq!(sides[0]["encoder"], "FilmCraft H.264");
    assert_eq!(sides[2]["sample_rate"], 48_000);
    // the recording begins when the late camera is live: every file starts then
    for sd in &sides {
        assert!(sd["first_sample_ns"].as_u64().unwrap() < FRAME_NS, "{sd}");
    }
    let warm = |i: usize| sides[i]["warmup_ns"].as_u64().unwrap() as f64 / 1e9;
    assert!((0.29..0.40).contains(&warm(1)), "the camera took ~300 ms to deliver: {}", warm(1));
    assert!(warm(0) < 0.1 && warm(2) < 0.1, "{} {}", warm(0), warm(2));
    // one new sequence, opened, with the three clips
    let seqs: Vec<_> = s.project.items.values().filter(|i| i.as_sequence().is_some()).collect();
    assert_eq!(seqs.len(), seqs_before + 1);
    let q = s.active_sequence().unwrap();
    assert_eq!(s.project.item(s.state.active_sequence.unwrap()).unwrap().name, "Recording 1");
    assert_eq!((q.settings.width, q.settings.height), (320, 180));
    assert_eq!(q.video_tracks.len(), 2);
    assert_eq!(q.audio_tracks.len(), 1);
    assert_eq!(q.video_tracks[0].items.len(), 1);
    assert_eq!(q.video_tracks[1].items.len(), 1);
    assert_eq!(q.audio_tracks[0].items.len(), 1);
    assert_eq!(q.markers.first().map(|m| m.name.as_str()), Some("Recording"));
    let (screen, camera, mic) = (clip(&s, &v, "screen"), clip(&s, &v, "camera"), clip(&s, &v, "mic"));
    assert_eq!(q.video_tracks[0].items[0].id, screen.id);
    assert_eq!(q.video_tracks[1].items[0].id, camera.id);
    for c in [&screen, &camera, &mic] {
        assert_eq!((c.start, c.source_in), (Tick::ZERO, Tick::ZERO), "every clip starts at 0");
    }
    // the files decode with our decoders and last about 2 s (from the moment all three were live)
    for (kind, want) in [("screen", 2.0), ("camera", 2.0), ("mic", 2.0)] {
        let item = filmcraft_project::ItemId(v["items"][kind].as_u64().unwrap());
        let info = s.project.item(item).unwrap().as_media().unwrap().info.clone();
        let d = secs(info.duration);
        assert!((d - want).abs() < 0.35, "{kind}: {d} s, want ~{want}");
        assert_eq!(s.project.item(item).unwrap().metadata.get("Recording Source").map(String::as_str), Some(kind));
    }
    // sync by pixel: what screen and camera show at the same sequence time was captured together
    for t in [0.0, 1.0, 1.5] {
        same_moment(&mut s, &v, "screen", "camera", t, 2.0);
    }
    // the screen's first ~300 ms (before the camera was live) are not in its file
    let k = index_at(&mut s, &v, "screen", 0.0);
    assert!((8..=12).contains(&k), "the screen file starts at its frame {k}, ~300 ms in");
    // the mic is back with the voice-over
    assert!(s.voiceover.input.is_some());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn camera_offset_moves_the_camera_and_undo_takes_it_all_back() {
    let dir = tmp("offset");
    let mut s = session(0);
    let items_before = s.project.items.len();
    let undo_before = s.history.undo.len();
    let v = record(&mut s, &dir, 0.8, json!({"cameraOffsetMs": 200}));
    let screen = clip(&s, &v, "screen");
    let camera = clip(&s, &v, "camera");
    let files: Vec<String> = v["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
    // every file starts at the recording's start: only the offset moves the camera
    let cam_at = secs(camera.start) - secs(camera.source_in) - (secs(screen.start) - secs(screen.source_in));
    assert!((cam_at - 0.2).abs() < 0.002, "camera moved by 200 ms: {cam_at}");
    assert_eq!(sidecar(&files[1])["camera_offset_ms"], 200.0);
    assert_eq!(s.active_sequence().unwrap().markers[0].comment, "camera offset 200 ms");
    // one undo step: sequence, items and bin are gone; the files stay
    assert_eq!(s.history.undo.len(), undo_before + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Record");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.items.len(), items_before);
    assert!(files.iter().all(|f| std::path::Path::new(f).exists()));
    // a negative offset renormalises so nothing starts before 0
    let o = sync_offsets(&[(SourceKind::Screen, 1_000_000), (SourceKind::Camera, 1_000_000), (SourceKind::Mic, 2_000_000)], -50.0);
    assert_eq!(o, vec![(SourceKind::Screen, 50_000_000), (SourceKind::Camera, 0), (SourceKind::Mic, 51_000_000)]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn cancel_discards_everything() {
    let dir = tmp("cancel");
    let mut s = session(0);
    let items = s.project.items.len();
    s.execute("record.start", json!({"screen": {"window": SyntheticFactory::WINDOW}, "mic": {}, "dir": dir.to_string_lossy(), "name": "Take"})).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!(st["recording"], true);
    assert_eq!(st["name"], "Take");
    assert_eq!(st["sources"].as_array().unwrap().len(), 2);
    assert!(st["sources"][0]["frames"].as_u64().unwrap() > 0, "{st}");
    let r = s.execute("record.cancel", json!({})).unwrap();
    assert_eq!(r["placed"], false);
    assert!(!s.record.recording());
    assert_eq!(s.project.items.len(), items);
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn refused_while_recording_and_for_hostile_params() {
    let dir = tmp("hostile");
    let mut s = session(0);
    let d = dir.to_string_lossy().into_owned();
    let bad = [
        json!({"dir": d}),
        json!({"screen": {"display": "nope"}, "dir": d}),
        json!({"screen": {"window": "nope"}, "dir": d}),
        json!({"screen": {}, "dir": d}),
        json!({"screen": {"display": SyntheticFactory::DISPLAY, "window": SyntheticFactory::WINDOW}, "dir": d}),
        json!({"camera": {"device": "nope"}, "dir": d}),
        json!({"camera": {"device": SyntheticFactory::CAMERA, "fps": 0}, "dir": d}),
        json!({"camera": {"device": SyntheticFactory::CAMERA, "fps": 61}, "dir": d}),
        json!({"camera": {"device": SyntheticFactory::CAMERA, "fps": "fast"}, "dir": d}),
        json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 5}, "dir": d}),
        json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 1e12}, "dir": d}),
        json!({"screen": {"display": SyntheticFactory::DISPLAY, "fps": -3}, "dir": d}),
        json!({"mic": {"device": "No Such Mic"}, "dir": d}),
        json!({"mic": {"device": 7}, "dir": d}),
        json!({"mic": {}, "name": "../escape", "dir": d}),
        json!({"mic": {}, "name": "a/b", "dir": d}),
        json!({"mic": {}, "name": "   ", "dir": d}),
        json!({"mic": {}, "name": ".hidden", "dir": d}),
        json!({"mic": {}, "name": "x".repeat(101), "dir": d}),
    ];
    for p in bad {
        assert!(s.execute("record.start", p.clone()).is_err(), "{p}");
        assert!(!s.record.recording(), "{p}");
    }
    assert!(s.voiceover.input.is_some(), "the mic input is never lost");
    s.execute("record.start", json!({"mic": {}, "dir": d})).unwrap();
    let e = s.execute("record.start", json!({"mic": {}, "dir": d})).unwrap_err().to_string();
    assert!(e.contains("already"), "{e}");
    // the microphone is busy for voice-overs
    s.execute("file.openDemoProject", json!({})).unwrap();
    assert!(s.execute("audio.voiceover.start", json!({})).is_err());
    assert!(s.execute("record.stop", json!({"cameraOffsetMs": 1e9})).is_err());
    assert!(s.record.recording(), "a refused stop keeps recording");
    s.execute("record.cancel", json!({})).unwrap();
    assert!(s.execute("record.stop", json!({})).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn names() {
    assert_eq!(check_name("  Demo take ").unwrap(), "Demo take");
    for n in ["", "a/b", "a\\b", "c:d", "..", "x..y", ".x", "tab\there"] {
        assert!(check_name(n).is_err(), "{n}");
    }
}

#[test]
fn queue_drops_the_oldest_and_never_blocks() {
    let q = FrameQueue::new(2);
    let f = |t| CapturedFrame { width: 2, height: 2, stride: 8, format: PixelFormat::Rgba8, data: vec![0; 16], time_ns: t };
    assert!(!q.push(f(1)));
    assert!(!q.push(f(2)));
    assert!(q.push(f(3)), "full: the oldest goes");
    assert_eq!(q.pop(Duration::ZERO).unwrap().unwrap().time_ns, 2);
    assert_eq!(q.pop(Duration::ZERO).unwrap().unwrap().time_ns, 3);
    assert!(q.pop(Duration::from_millis(1)).unwrap().is_none());
    q.close();
    assert!(q.pop(Duration::ZERO).is_none());
    assert!(q.push(f(4)), "closed: dropped");
}

#[test]
fn synthetic_index_round_trips() {
    for k in [0u64, 1, 2, 77, 1000, 65_535] {
        let px = synthetic_frame(320, 180, k, k * 33_333_333);
        let luma: Vec<u8> = px.chunks(4).map(|p| p[0]).collect();
        assert_eq!(read_synthetic_index(&luma, 320, 180), Some(k));
    }
    assert_eq!(read_synthetic_index(&[], 320, 180), None);
}

#[test]
fn two_cameras_and_two_mics_each_get_a_file_and_a_track() {
    let dir = tmp("multi");
    let mut s = Session::default();
    s.record.factory = Some(Arc::new(SyntheticFactory {
        display_size: (320, 180),
        camera_size: (320, 180),
        camera_delay_ms: 100,
        camera2_delay_ms: 400,
        ..Default::default()
    }));
    let r = s
        .execute(
            "record.start",
            json!({
                "screen": {"display": SyntheticFactory::DISPLAY},
                "cameras": [{"device": SyntheticFactory::CAMERA}, {"device": SyntheticFactory::CAMERA2}],
                "mics": [{}, {"device": "Synthetic Input 2"}],
                "dir": dir.to_string_lossy(),
            }),
        )
        .unwrap();
    let keys: Vec<&str> = r["files"].as_array().unwrap().iter().map(|f| f["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["screen", "camera", "camera2", "mic", "mic2"]);
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!(st["sources"].as_array().unwrap().len(), 5, "{st}");
    std::thread::sleep(Duration::from_millis(1500));
    let v = s.execute("record.stop", json!({"cameraOffsetsMs": [0, 100]})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    let files: Vec<String> = v["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
    for (f, label) in files.iter().zip(["Screen.mov", "Camera.mov", "Camera 2.mov", "Mic.wav", "Mic 2.wav"]) {
        assert!(f.ends_with(&format!("Recording 1 - {label}")), "{f}");
        assert!(std::fs::metadata(f).unwrap().len() > 1000, "{f}");
    }
    let sides: Vec<Value> = files.iter().map(|f| sidecar(f)).collect();
    assert_eq!(sides[2]["source"], "camera");
    assert_eq!(sides[2]["index"], 2);
    assert_eq!(sides[2]["device"]["id"], SyntheticFactory::CAMERA2);
    assert_eq!(sides[2]["camera_offset_ms"], 100.0);
    assert_eq!(sides[1]["camera_offset_ms"], 0.0);
    assert_eq!(sides[4]["index"], 2);
    assert!(sides.iter().all(|sd| sd["clock_start_ns"] == sides[0]["clock_start_ns"]));
    assert!(sides.iter().all(|sd| sd["first_sample_ns"].as_u64().unwrap() < FRAME_NS), "{sides:?}");
    let warm = |i: usize| sides[i]["warmup_ns"].as_u64().unwrap() as f64 / 1e9;
    assert!((0.09..0.2).contains(&warm(1)), "{}", warm(1));
    assert!((0.39..0.5).contains(&warm(2)), "{}", warm(2));
    // V1 screen, V2 camera, V3 camera 2; A1 mic, A2 mic 2
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (3, 2));
    for (t, key) in q.video_tracks.iter().zip(["screen", "camera", "camera2"]) {
        assert_eq!(t.items.len(), 1);
        assert_eq!(t.items[0].id, clip(&s, &v, key).id, "{key}");
    }
    for (t, key) in q.audio_tracks.iter().zip(["mic", "mic2"]) {
        assert_eq!(t.items[0].id, clip(&s, &v, key).id, "{key}");
    }
    let at = |key: &str| {
        let c = clip(&s, &v, key);
        secs(c.start) - secs(c.source_in)
    };
    assert!(at("camera").abs() < 0.002, "camera at the start: {}", at("camera"));
    assert!((at("camera2") - 0.1).abs() < 0.002, "camera 2 moved by its own 100 ms: {}", at("camera2"));
    for key in ["screen", "mic", "mic2"] {
        assert!(at(key).abs() < 0.002, "{key} at the start: {}", at(key));
    }
    assert_eq!(q.markers[0].comment, "camera 2 offset 100 ms");
    assert_eq!(v["cameras"], json!(["camera", "camera2"]));
    assert_eq!(v["mics"], json!(["mic", "mic2"]));
    let item = filmcraft_project::ItemId(v["items"]["camera2"].as_u64().unwrap());
    assert_eq!(s.project.item(item).unwrap().metadata.get("Recording Source").map(String::as_str), Some("camera2"));
    assert!(s.voiceover.input.is_some(), "the session's microphone input is back");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_same_device_twice_or_too_many_is_refused() {
    let dir = tmp("twice");
    let mut s = session(0);
    let d = dir.to_string_lossy().into_owned();
    let cam = json!({"device": SyntheticFactory::CAMERA});
    for p in [
        json!({"cameras": [cam, cam], "dir": d}),
        json!({"mics": [{}, {}], "dir": d}),
        json!({"mics": [{"device": "Synthetic Input 2"}, {"device": "Synthetic Input 2"}], "dir": d}),
        json!({"cameras": [cam, {"device": SyntheticFactory::CAMERA2}, cam, cam, cam], "dir": d}),
        json!({"camera": cam, "cameras": [cam], "dir": d}),
        json!({"cameras": cam, "dir": d}),
        json!({"cameras": [7], "dir": d}),
        json!({"cameras": [{"device": SyntheticFactory::CAMERA, "mirror": "yes"}], "dir": d}),
        json!({"mics": [{}, {"device": "nope"}], "dir": d}),
    ] {
        let e = s.execute("record.start", p.clone());
        assert!(e.is_err(), "{p}");
        assert!(!s.record.recording(), "{p}");
    }
    let e = s.execute("record.start", json!({"cameras": [cam, cam], "dir": d})).unwrap_err().to_string();
    assert!(e.contains("twice"), "{e}");
    assert!(s.voiceover.input.is_some());
    // `cameras: []` with a mic is just the mic; offsets are checked like the scalar
    s.execute("record.start", json!({"cameras": [], "mic": {}, "dir": d})).unwrap();
    assert!(s.execute("record.stop", json!({"cameraOffsetsMs": [0, 9000]})).is_err());
    assert!(s.execute("record.stop", json!({"cameraOffsetsMs": "x"})).is_err());
    assert!(s.record.recording());
    s.execute("record.cancel", json!({})).unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_mirrored_camera_is_flipped_in_the_sequence() {
    let dir = tmp("mirror");
    let mut s = session(0);
    s.execute("record.start", json!({"cameras": [{"device": SyntheticFactory::CAMERA, "mirror": true}], "dir": dir.to_string_lossy()})).unwrap();
    std::thread::sleep(Duration::from_millis(900));
    let v = s.execute("record.stop", json!({})).unwrap();
    let cam = clip(&s, &v, "camera");
    assert_eq!(cam.effects.first().map(|e| e.effect.as_str()), Some("horizontal_flip"), "{:?}", cam.effects.iter().map(|e| &e.effect).collect::<Vec<_>>());
    let file = v["files"][0].as_str().unwrap().to_string();
    assert_eq!(sidecar(&file)["mirror"], true);
    // the rendered picture is the camera's, mirrored: the burnt-in index reads back right to left
    let img = s.render_program_at(1.0, Tick::from_seconds_f64(0.5)).unwrap();
    let luma: Vec<u8> = img.px.chunks(4).map(|p| (p[1].clamp(0.0, 1.0) * 255.0) as u8).collect();
    let mirrored: Vec<u8> = luma.chunks(img.w).flat_map(|row| row.iter().rev().copied().collect::<Vec<_>>()).collect();
    let k = read_synthetic_index(&mirrored, img.w as u32, img.h as u32).unwrap();
    assert!((5..=25).contains(&k), "frame {k} at 0.5 s once unflipped");
    std::fs::remove_dir_all(&dir).ok();
}

// ------------------------------------------------------------------------------------- settings

fn wav_fmt(path: &str) -> (u16, u16, u32, u16) {
    let b = std::fs::read(path).unwrap();
    let le16 = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    (le16(20), le16(22), u32::from_le_bytes([b[24], b[25], b[26], b[27]]), le16(34))
}

#[test]
fn recording_settings_defaults_clamping_and_merge() {
    use crate::record_settings::RecordingSettings;
    let d = RecordingSettings::default();
    assert_eq!((d.screen_fps, d.screen_resolution.as_str(), d.show_cursor, d.system_audio), (30, "native", true, false));
    assert_eq!((d.camera_quality.as_str(), d.camera_fps, d.camera_mirror), ("1080p", 30, false));
    assert_eq!((d.codec.as_str(), d.quality.as_str(), d.keyframe_seconds, d.hardware_encoder), ("h264", "high", 2, true));
    assert_eq!((d.sample_rate, d.channels.as_str(), d.audio_format.as_str(), d.auto_gain), (48_000, "mono", "f32", false));
    assert_eq!((d.countdown_seconds, d.stop_after_minutes, d.open_sequence, d.output_folder.as_str()), (3, 0, true, ""));
    // a hostile preferences file is repaired on load
    let p = crate::autosave::Preferences::from_value(json!({"recording": {
        "screenFps": 999, "codec": "av1", "stopAfterMinutes": 5000, "sampleRate": "abc", "keyframeSeconds": 3, "cameraQuality": 7, "showCursor": false
    }}));
    let r = &p.recording;
    assert_eq!((r.screen_fps, r.codec.as_str(), r.stop_after_minutes, r.sample_rate, r.keyframe_seconds), (30, "h264", 180, 48_000, 2));
    assert_eq!(r.camera_quality, "1080p");
    assert!(!r.show_cursor, "valid values survive");
    // record.settings get / set (merge) and prefs agree
    let mut s = Session::default();
    let g = s.execute("record.settings", json!({"get": true})).unwrap();
    assert_eq!(g["settings"]["screenFps"], 30);
    assert_eq!(g["settings"]["countdownSeconds"], 3);
    let v = s.execute("record.settings", json!({"set": {"screenFps": 60, "quality": "max", "keyframeSeconds": "4", "autoGain": true}})).unwrap();
    assert_eq!(v["settings"]["screenFps"], 60);
    assert_eq!(v["settings"]["keyframeSeconds"], 4);
    assert_eq!(v["settings"]["cameraFps"], 30, "a merge keeps the other fields");
    assert_eq!(s.execute("prefs.get", json!({"key": "recording.quality"})).unwrap(), "max");
    s.execute("prefs.set", json!({"key": "recording.countdownSeconds", "value": "10"})).unwrap();
    assert_eq!(s.execute("record.settings", json!({})).unwrap()["settings"]["countdownSeconds"], 10);
    for (bad, field) in [
        (json!({"screenFps": 50}), "screenFps"),
        (json!({"screenFps": 30.5}), "screenFps"),
        (json!({"codec": "av1"}), "codec"),
        (json!({"stopAfterMinutes": 181}), "stopAfterMinutes"),
        (json!({"stopAfterMinutes": -1}), "stopAfterMinutes"),
        (json!({"showCursor": "yes"}), "showCursor"),
        (json!({"channels": 2}), "channels"),
        (json!({"bogus": 1}), "bogus"),
        (json!({"outputFolder": "a\u{0}b"}), "outputFolder"),
    ] {
        let e = s.execute("record.settings", json!({"set": bad.clone()})).unwrap_err().to_string();
        assert!(e.contains(field), "{bad}: {e}");
    }
    assert!(s.execute("record.settings", json!({"set": [1]})).is_err());
    assert_eq!(s.prefs.recording.screen_fps, 60, "a refused set changes nothing");
    if !crate::record_settings::hevc_available() {
        let e = s.execute("record.settings", json!({"set": {"codec": "hevc"}})).unwrap_err().to_string();
        assert!(e.contains("codec") && e.contains("hardware"), "{e}");
    }
    // the Settings page lists every field under `recording.`
    let keys: Vec<&str> = crate::settings::fields().into_iter().filter(|(c, _)| *c == "recording").map(|(_, f)| f.key).collect();
    for k in crate::record_settings::keys() {
        assert!(keys.contains(&format!("recording.{k}").as_str()), "Settings ▸ Recording has {k}");
    }
}

#[test]
fn record_start_takes_the_recording_settings_and_explicit_params_win() {
    let dir = tmp("settings");
    let mut s = Session::default();
    s.record.factory = Some(Arc::new(SyntheticFactory { display_size: (1440, 1080), camera_size: (320, 180), ..Default::default() }));
    s.execute(
        "record.settings",
        json!({"set": {
            "screenFps": 15, "screenResolution": "720p", "showCursor": false, "systemAudio": true,
            "cameraQuality": "native", "cameraFps": 24, "cameraMirror": true,
            "codec": "prores", "sampleRate": 44100, "channels": "stereo", "audioFormat": "s16", "autoGain": true,
            "outputFolder": dir.to_string_lossy(),
        }}),
    )
    .unwrap();
    let r = s
        .execute(
            "record.start",
            json!({"screen": {"display": SyntheticFactory::DISPLAY, "fps": 10}, "camera": {"device": SyntheticFactory::CAMERA}, "mic": {}, "settings": {"audioFormat": "s24"}}),
        )
        .unwrap();
    let keys: Vec<&str> = r["files"].as_array().unwrap().iter().map(|f| f["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["screen", "camera", "mic", "systemAudio"]);
    assert!(r["dir"].as_str().unwrap().starts_with(&*dir.to_string_lossy()), "the output folder setting: {r}");
    std::thread::sleep(Duration::from_millis(1200));
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    let files: Vec<String> = v["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
    assert!(files[3].ends_with("Recording 1 - System Audio.wav"), "{files:?}");
    let sides: Vec<Value> = files.iter().map(|f| sidecar(f)).collect();
    // screen: 720p of a 1440 × 1080 display, the explicit 10 fps, ProRes, no cursor
    assert_eq!((sides[0]["width"].as_u64(), sides[0]["height"].as_u64(), sides[0]["fps"].as_u64()), (Some(960), Some(720), Some(10)));
    assert_eq!(sides[0]["encoder"], "FilmCraft ProRes 422");
    assert_eq!(sides[0]["show_cursor"], false);
    assert_eq!(sides[0]["bitrate_kbps"], 0);
    // camera: its own size (native), 24 fps, mirrored
    assert_eq!((sides[1]["width"].as_u64(), sides[1]["fps"].as_u64(), sides[1]["mirror"].as_bool()), (Some(320), Some(24), Some(true)));
    // mic: 44.1 kHz stereo, 24-bit (the explicit settings win over the saved 16-bit), auto gain
    assert_eq!((sides[2]["sample_rate"].as_u64(), sides[2]["channels"].as_u64(), sides[2]["format"].as_str()), (Some(44_100), Some(2), Some("s24")));
    assert_eq!(sides[2]["auto_gain"], true);
    assert!(sides[2]["notes"][0].as_str().unwrap().contains("auto gain"), "{}", sides[2]);
    assert_eq!(wav_fmt(&files[2]), (1, 2, 44_100, 24));
    // system audio: the synthetic screen's tone, on the next audio track
    assert_eq!(sides[3]["source"], "systemAudio");
    let (_, ch, rate, bits) = wav_fmt(&files[3]);
    assert_eq!((ch, rate, bits), (2, 44_100, 24));
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (2, 2));
    assert_eq!(q.audio_tracks[1].items[0].id, clip(&s, &v, "systemAudio").id);
    assert_eq!(q.video_tracks[1].items[0].effects[0].effect, "horizontal_flip");
    let item = filmcraft_project::ItemId(v["items"]["systemAudio"].as_u64().unwrap());
    let info = s.project.item(item).unwrap().as_media().unwrap().info.clone();
    assert!((secs(info.duration) - 1.2).abs() < 0.3, "{}", secs(info.duration));
    let b = std::fs::read(&files[3]).unwrap();
    let pcm: Vec<f64> = b[44..].as_chunks::<3>().0.iter().map(|x| f64::from(i32::from_le_bytes([0, x[0], x[1], x[2]]) >> 8) / 8_388_608.0).collect();
    let rms = (pcm.iter().map(|x| x * x).sum::<f64>() / pcm.len() as f64).sqrt();
    assert!((rms - 0.25 / 2f64.sqrt()).abs() < 0.02, "the tone is in the file: rms {rms}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn countdown_uses_the_synthetic_clock_and_can_be_cancelled() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let dir = tmp("countdown");
    let mut s = session(0);
    let clock = Arc::new(AtomicU64::new(1_000));
    s.record.test_clock_ms = Some(clock.clone());
    let d = dir.to_string_lossy().into_owned();
    for bad in [json!(11), json!(2.5), json!("x"), json!(-1)] {
        assert!(s.execute("record.start", json!({"mic": {}, "dir": d, "countdown": bad.clone()})).is_err(), "{bad}");
    }
    assert!(s.execute("record.start", json!({"screen": {"display": "nope"}, "dir": d, "countdown": 3})).is_err(), "devices are checked first");
    assert!(s.execute("record.start", json!({"dir": d, "countdown": 3})).is_err(), "no source");
    let r = s.execute("record.start", json!({"mic": {}, "dir": d, "countdown": 3})).unwrap();
    assert_eq!((r["recording"].as_bool(), r["countdown"].as_u64()), (Some(false), Some(3)));
    assert!(s.execute("record.start", json!({"mic": {}, "dir": d})).is_err(), "one start at a time");
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!(st["countdown"]["remaining"], 3.0);
    clock.store(3_000, Ordering::Release);
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!((st["recording"].as_bool(), st["countdown"]["remaining"].as_f64()), (Some(false), Some(1.0)));
    clock.store(4_000, Ordering::Release);
    // the countdown is over: the sources start (on their own thread), then the recording begins
    let st = s.execute("record.status", json!({})).unwrap();
    assert!(st["starting"] == true || st["recording"] == true, "{st}");
    let t0 = std::time::Instant::now();
    while !s.record.recording() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
        s.execute("record.status", json!({})).unwrap();
    }
    assert!(s.record.recording());
    assert_eq!(s.record.last_event.as_ref().unwrap()["event"], "started");
    std::thread::sleep(Duration::from_millis(300));
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["placed"], true);
    // Cancel (or Stop) during the countdown: nothing starts, nothing is written
    s.execute("record.start", json!({"mic": {}, "dir": d, "countdown": 5})).unwrap();
    let c = s.execute("record.cancel", json!({})).unwrap();
    assert_eq!(c["cancelled"], true);
    clock.store(20_000, Ordering::Release);
    s.execute("record.status", json!({})).unwrap();
    assert!(!s.record.recording() && s.record.countdown.is_none());
    s.execute("record.start", json!({"mic": {}, "dir": d, "countdown": 3})).unwrap();
    assert_eq!(s.execute("record.stop", json!({})).unwrap()["cancelled"], true);
    assert!(s.execute("record.stop", json!({})).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stop_after_and_open_sequence_settings() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let dir = tmp("stopafter");
    let mut s = session(0);
    let clock = Arc::new(AtomicU64::new(0));
    s.record.test_clock_ms = Some(clock.clone());
    s.execute("record.settings", json!({"set": {"stopAfterMinutes": 1, "openSequence": false}})).unwrap();
    let before = s.state.active_sequence;
    s.execute("record.start", json!({"mic": {}, "dir": dir.to_string_lossy()})).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    clock.store(59_000, Ordering::Release);
    assert_eq!(s.execute("record.status", json!({})).unwrap()["recording"], true);
    clock.store(60_000, Ordering::Release);
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!(st["recording"], false);
    let ev = s.record.last_event.clone().unwrap();
    assert_eq!(ev["event"], "stoppedAfter");
    assert_eq!(ev["result"]["placed"], true);
    assert_eq!(ev["result"]["opened"], false);
    assert_eq!(s.state.active_sequence, before, "Open the sequence after Stop is off");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn auto_gain_moves_toward_minus_18_dbfs_at_2_db_per_second() {
    use crate::record_settings::AutoGain;
    let rate = 48_000;
    let mut g = AutoGain::new(rate);
    // a quiet tone (−40 dBFS RMS) for 3 s in 20 ms blocks: +2 dB per second
    let amp = 0.01 * 2f32.sqrt();
    let mut t = 0usize;
    for _ in 0..150 {
        let mut b = vec![(0..960).map(|i| amp * ((t + i) as f32 * 0.05).sin()).collect::<Vec<f32>>()];
        t += 960;
        g.process(&mut b);
    }
    assert!((g.gain_db() - 6.0).abs() < 0.3, "gain {}", g.gain_db());
    // silence holds the gain; it never passes ±20 dB
    let mut b = vec![vec![0.0f32; 48_000]];
    g.process(&mut b);
    assert!((g.gain_db() - 6.0).abs() < 0.3);
    for _ in 0..60 {
        let mut b = vec![(0..48_000).map(|i| amp * (i as f32 * 0.05).sin()).collect::<Vec<f32>>()];
        g.process(&mut b);
    }
    assert!((g.gain_db() - 20.0).abs() < 1e-6, "{}", g.gain_db());
    // loud input is turned down and clipped to full scale
    let mut g = AutoGain::new(rate);
    let mut b = vec![vec![1.5f32, f32::NAN, -2.0]];
    g.process(&mut b);
    assert!(b[0].iter().all(|v| v.is_finite() && v.abs() <= 1.0));
}

// ------------------------------------------------------------------------------------- start sync

#[test]
fn every_source_is_live_before_the_clock_starts() {
    let _turn = one_at_a_time();
    let dir = tmp("live");
    let mut s = Session::default();
    // the screen delivers at once, the camera after 300 ms, the microphone after 700 ms
    s.record.factory = Some(Arc::new(SyntheticFactory { display_size: (320, 180), camera_size: (320, 180), camera_delay_ms: 300, ..Default::default() }));
    s.voiceover.input = Some(Box::new(crate::voiceover::SyntheticInput::delayed(Duration::from_millis(700))));
    let r = s
        .execute(
            "record.start",
            json!({"screen": {"display": SyntheticFactory::DISPLAY}, "camera": {"device": SyntheticFactory::CAMERA}, "mic": {"device": "Synthetic Input"}, "dir": dir.to_string_lossy()}),
        )
        .unwrap();
    let warm = |k: &str| r["warmupNs"][k].as_u64().unwrap() as f64 / 1e9;
    assert!(warm("screen") < 0.1, "{r}");
    assert!((0.29..0.42).contains(&warm("camera")), "{r}");
    assert!((0.69..0.85).contains(&warm("mic")), "{r}");
    // the clock starts when the last source is live: Recording · 00:00
    let st = s.execute("record.status", json!({})).unwrap();
    assert!(st["elapsed"].as_f64().unwrap() < 0.1, "{st}");
    std::thread::sleep(Duration::from_millis(1500));
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    let files: Vec<String> = v["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
    let sides: Vec<Value> = files.iter().map(|f| sidecar(f)).collect();
    for (sd, (lo, hi)) in sides.iter().zip([(0.0, 0.1), (0.29, 0.42), (0.69, 0.85)]) {
        assert!(sd["first_sample_ns"].as_u64().unwrap() < FRAME_NS, "first sample within a frame of the start: {sd}");
        let w = sd["warmup_ns"].as_u64().unwrap() as f64 / 1e9;
        assert!((lo..hi).contains(&w), "warmup_ns {w}: {sd}");
        assert_eq!(sd["clock_start_ns"], sides[0]["clock_start_ns"]);
        assert_eq!(sd["clock_start_ns"], r["clockStartNs"]);
    }
    // all three clips start at 0 in the sequence
    for key in ["screen", "camera", "mic"] {
        let c = clip(&s, &v, key);
        assert_eq!((c.start, c.source_in), (Tick::ZERO, Tick::ZERO), "{key}");
    }
    // the screen file is shorter by the 700 ms before the microphone was live: ~45 frames of
    // 1.5 s, the first of them the screen's frame ~21
    let frames = sides[0]["frames"].as_u64().unwrap();
    assert!((40..=50).contains(&frames), "{frames} screen frames");
    let k = index_at(&mut s, &v, "screen", 0.0);
    assert!((19..=25).contains(&k), "the screen file starts at its frame {k}, ~700 ms in");
    let k = index_at(&mut s, &v, "camera", 0.0);
    assert!((10..=15).contains(&k), "the camera file starts at its frame {k}, ~400 ms in");
    // the microphone covers the same span, to the sample
    let samples = sides[2]["samples"].as_u64().unwrap() as f64 / 48_000.0;
    let screen_s = frames as f64 / 30.0;
    assert!((samples - screen_s).abs() < 0.12, "mic {samples} s, screen {screen_s} s");
    for t in [0.0, 0.5, 1.0] {
        same_moment(&mut s, &v, "screen", "camera", t, 2.0);
    }
    assert!(s.voiceover.input.is_some());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_source_that_never_delivers_fails_the_start_and_leaves_no_files() {
    assert_eq!(crate::record::START_TIMEOUT, Duration::from_secs(20));
    let dir = tmp("never");
    let mut s = Session::default();
    s.record.factory =
        Some(Arc::new(SyntheticFactory { display_size: (320, 180), camera_size: (320, 180), camera_delay_ms: u64::MAX / 2, ..Default::default() }));
    s.record.start_timeout = Some(Duration::from_millis(600));
    let d = dir.to_string_lossy().into_owned();
    // a microphone that never delivers
    s.voiceover.input = Some(Box::new(crate::voiceover::SyntheticInput::delayed(Duration::MAX)));
    let e = s
        .execute("record.start", json!({"screen": {"display": SyntheticFactory::DISPLAY}, "mic": {"device": "Synthetic Input"}, "dir": d}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("microphone 'Synthetic Input' delivered no audio within 0.6 s"), "{e}");
    assert!(!s.record.recording() && s.record.starting.is_none());
    assert!(s.voiceover.input.is_some(), "the microphone input comes back");
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "no files remain");
    // a camera that never delivers
    s.voiceover.input = Some(Box::new(crate::voiceover::SyntheticInput::clicks()));
    let e = s.execute("record.start", json!({"camera": {"device": SyntheticFactory::CAMERA}, "mic": {}, "dir": d})).unwrap_err().to_string();
    assert!(e.contains("camera 'Synthetic Camera' delivered no frames within 0.6 s"), "{e}");
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "no files remain");
    assert!(s.voiceover.input.is_some());
    // started without waiting: `record.status` says starting; the failure comes as an event
    let r = s.execute("record.start", json!({"camera": {"device": SyntheticFactory::CAMERA}, "dir": d, "wait": false})).unwrap();
    assert_eq!((r["starting"].as_bool(), r["label"].as_str()), (Some(true), Some("Starting the camera…")));
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!((st["recording"].as_bool(), st["starting"].as_bool()), (Some(false), Some(true)), "{st}");
    assert!(s.execute("record.start", json!({"mic": {}, "dir": d})).is_err(), "one start at a time");
    assert!(s.execute("audio.voiceover.start", json!({})).is_err());
    let t0 = std::time::Instant::now();
    while s.record.starting.is_some() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
        s.execute("record.status", json!({})).unwrap();
    }
    let ev = s.record.last_event.clone().unwrap();
    assert_eq!(ev["event"], "startFailed");
    assert!(ev["error"].as_str().unwrap().contains("delivered no frames"), "{ev}");
    // cancelled while starting: nothing is written
    s.record.start_timeout = None;
    s.execute(
        "record.start",
        json!({"screen": {"display": SyntheticFactory::DISPLAY}, "camera": {"device": SyntheticFactory::CAMERA}, "dir": d, "wait": false}),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(s.execute("record.status", json!({})).unwrap()["label"], "Starting screen capture…");
    let c = s.execute("record.cancel", json!({})).unwrap();
    assert_eq!(c["cancelled"], true);
    assert!(s.record.starting.is_none() && !s.record.recording());
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "no files remain");
    std::fs::remove_dir_all(&dir).ok();
}

// ------------------------------------------------------------------------------------- pause / resume

/// Whether no video source of a stopped recording dropped a frame. When an encoder could not
/// keep up (a loaded machine) the oldest waiting frame is dropped and the file shows an older
/// picture for a while: lengths and cuts are still exact, a frame-by-frame comparison of the
/// pictures is not.
fn no_frame_dropped(sides: &[Value]) -> bool {
    let clean = sides.iter().filter(|sd| sd["source"] == "screen" || sd["source"] == "camera").all(|sd| sd["dropped"] == 0);
    if !clean {
        eprintln!("an encoder dropped frames: the pictures are not compared frame by frame in this run");
    }
    clean
}

/// Start the synthetic screen, camera and microphone.
fn start_three(s: &mut Session, dir: &std::path::Path) -> Value {
    s.execute(
        "record.start",
        json!({"screen": {"display": SyntheticFactory::DISPLAY}, "camera": {"device": SyntheticFactory::CAMERA}, "mic": {}, "dir": dir.to_string_lossy()}),
    )
    .unwrap()
}

/// The recorded seconds of the running recording (`record.status` `elapsed`).
fn elapsed(s: &mut Session) -> f64 {
    s.execute("record.status", json!({})).unwrap()["elapsed"].as_f64().unwrap()
}

/// The length of an imported recording file, seconds.
fn media_seconds(s: &Session, v: &Value, key: &str) -> f64 {
    let item = filmcraft_project::ItemId(v["items"][key].as_u64().unwrap());
    secs(s.project.item(item).unwrap().as_media().unwrap().info.duration)
}

/// The samples of a mono 32-bit float WAV written by a recording.
fn wav_f32(path: &str) -> Vec<f32> {
    let b = std::fs::read(path).unwrap();
    assert_eq!(wav_fmt(path), (3, 1, 48_000, 32), "{path}");
    b[44..].as_chunks::<4>().0.iter().map(|x| f32::from_le_bytes(*x)).collect()
}

fn files_of(v: &Value) -> Vec<String> {
    v["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect()
}

/// Every file of a stopped recording is as long as the recorded time, and so they agree: within
/// 1.5 frames (60 ms).
fn every_file_lasts(s: &Session, v: &Value, keys: &[&str], recorded: f64) {
    let lens: Vec<f64> = keys.iter().map(|k| media_seconds(s, v, k)).collect();
    for (k, d) in keys.iter().zip(&lens) {
        assert!((d - recorded).abs() < 0.06, "{k} lasts {d} s, recorded {recorded} s ({lens:?})");
    }
    let (min, max) = lens.iter().fold((f64::MAX, 0f64), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
    assert!(max - min < 0.06, "the files agree: {lens:?}");
}

#[test]
fn pause_and_resume_cut_every_source_at_the_same_instants() {
    let _turn = one_at_a_time();
    let dir = tmp("pause");
    let mut s = session(0);
    start_three(&mut s, &dir);
    std::thread::sleep(Duration::from_millis(500));
    let p = s.execute("record.pause", json!({})).unwrap();
    assert_eq!((p["paused"].as_bool(), p["pauses"].as_u64()), (Some(true), Some(1)), "{p}");
    let at_pause = p["elapsed"].as_f64().unwrap();
    assert!((0.45..2.0).contains(&at_pause), "paused after about 0.5 s: {p}");
    // paused: the recorded time stands still, the sources keep running, and nothing captured
    // after the pause began is written (what was captured just before it may still be on its way
    // into the files: never more than the recorded time holds)
    std::thread::sleep(Duration::from_millis(300));
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!((st["recording"].as_bool(), st["paused"].as_bool(), st["pauses"].as_u64()), (Some(true), Some(true), Some(1)), "{st}");
    assert_eq!(st["elapsed"].as_f64().unwrap(), at_pause, "the time stands still while paused");
    let frames = |st: &Value, key: &str| st["sources"].as_array().unwrap().iter().find(|x| x["key"] == key).unwrap()["frames"].as_u64().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let st2 = s.execute("record.status", json!({})).unwrap();
    assert_eq!(st2["elapsed"].as_f64().unwrap(), at_pause);
    for key in ["screen", "camera"] {
        assert!(frames(&st2, key) as f64 <= at_pause * 30.0 + 2.0, "{key} wrote {} frames, {at_pause} s were recorded", frames(&st2, key));
    }
    assert!(frames(&st2, "mic") as f64 <= at_pause * 48_000.0 + 1.0, "the microphone wrote {} samples, {at_pause} s were recorded", frames(&st2, "mic"));
    assert!(st2["sources"][2]["level"].as_f64().is_some(), "the microphone is still read: {st2}");
    assert!(st2["error"].is_null(), "{st2}");
    let r = s.execute("record.resume", json!({})).unwrap();
    assert_eq!((r["paused"].as_bool(), r["pauses"].as_u64()), (Some(false), Some(1)), "{r}");
    assert!((r["elapsed"].as_f64().unwrap() - at_pause).abs() < 0.05, "the time goes on from where it stopped: {r}");
    std::thread::sleep(Duration::from_millis(500));
    let before_stop = elapsed(&mut s);
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(v["pauses"], 1, "{v}");
    // Stop says how much was recorded: what the status said just before it
    let recorded = v["elapsed"].as_f64().unwrap();
    assert!((0.9..3.0).contains(&recorded) && recorded >= before_stop && recorded - before_stop < 0.25, "about 1 s was recorded: {recorded} ({before_stop})");
    // every file is as long as the recorded time: the pause is in none of them
    every_file_lasts(&s, &v, &["screen", "camera", "mic"], recorded);
    let files = files_of(&v);
    let sides: Vec<Value> = files.iter().map(|f| sidecar(f)).collect();
    // the sidecars: the pause on the recording clock (the same in each) and its length
    for sd in &sides {
        let pauses = sd["pauses"].as_array().unwrap();
        assert_eq!(pauses.len(), 1, "{sd}");
        let (from, to) = (pauses[0][0].as_u64().unwrap(), pauses[0][1].as_u64().unwrap());
        assert!((from as f64 / 1e9 - at_pause).abs() < 1e-6, "the pause began at the recorded time it reported: {sd}");
        assert_eq!(sd["paused_ns"].as_u64().unwrap(), to - from, "{sd}");
        assert_eq!(sd["pauses"], sides[0]["pauses"]);
        assert!(sd["last_sample_ns"].as_u64().unwrap() as f64 / 1e9 <= recorded + 0.06, "sample times are file times: {sd}");
    }
    let paused = sides[0]["paused_ns"].as_u64().unwrap() as f64 / 1e9;
    assert!((0.59..2.0).contains(&paused), "paused for about 0.6 s: {paused}");
    // the WAV holds the recorded time, to the sample: what was read less the paused samples
    let n_of = |ns: u64| ns * 48_000 / 1_000_000_000;
    let (from, to) = (sides[2]["pauses"][0][0].as_u64().unwrap(), sides[2]["pauses"][0][1].as_u64().unwrap());
    let cut = n_of(to) - n_of(from);
    let samples = sides[2]["samples"].as_u64().unwrap();
    assert!((samples as f64 / 48_000.0 - recorded).abs() < 0.001, "{samples} samples, recorded {recorded} s");
    // the synthetic microphone clicks every 12 000 samples: across the cut the spacing is short
    // by exactly the paused samples, so not one sample too many or too few was left out
    let pcm = wav_f32(&files[2]);
    assert_eq!(pcm.len() as u64, samples);
    let clicks: Vec<u64> = pcm.iter().enumerate().filter(|(_, v)| **v > 0.4).map(|(i, _)| i as u64).collect();
    assert!(clicks.len() >= 3, "{clicks:?}");
    let cut_at = n_of(from);
    for w in clicks.windows(2) {
        if w[0] < cut_at && w[1] >= cut_at {
            assert_eq!((w[1] - w[0] + cut) % 12_000, 0, "clicks {} and {} around the cut of {cut} samples at {cut_at}", w[0], w[1]);
        } else {
            assert_eq!(w[1] - w[0], 12_000, "clicks {} and {} away from the cut at {cut_at}", w[0], w[1]);
        }
    }
    assert!(clicks.iter().any(|c| *c < cut_at) && clicks.iter().any(|c| *c >= cut_at), "clicks on both sides of the cut: {clicks:?} / {cut_at}");
    if no_frame_dropped(&sides) {
        // the pictures jump by the pause: 0.4 s of file time around it are 0.4 s + the pause of capture
        for key in ["screen", "camera"] {
            let (a, b) = (index_at(&mut s, &v, key, at_pause - 0.2), index_at(&mut s, &v, key, at_pause + 0.2));
            let want = (0.4 + paused) * 30.0;
            assert!(((b - a) as f64 - want).abs() <= 2.5, "{key}: frames {a} and {b} around the pause, want {want:.1} apart");
        }
        // and both show the same moment after it
        same_moment(&mut s, &v, "screen", "camera", at_pause + 0.25, 2.0);
        same_moment(&mut s, &v, "screen", "camera", at_pause - 0.25, 2.0);
    }
    for key in ["screen", "camera", "mic"] {
        let c = clip(&s, &v, key);
        assert_eq!((c.start, c.source_in), (Tick::ZERO, Tick::ZERO), "{key} starts at 0");
    }
    assert!(s.voiceover.input.is_some());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn two_pauses_are_both_left_out() {
    let _turn = one_at_a_time();
    let dir = tmp("pause-two");
    let mut s = session(0);
    start_three(&mut s, &dir);
    let mut at = Vec::new();
    for _ in 0..2 {
        std::thread::sleep(Duration::from_millis(350));
        at.push(s.execute("record.pause", json!({})).unwrap()["elapsed"].as_f64().unwrap());
        std::thread::sleep(Duration::from_millis(250));
        s.execute("record.resume", json!({})).unwrap();
    }
    std::thread::sleep(Duration::from_millis(350));
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!((st["paused"].as_bool(), st["pauses"].as_u64()), (Some(false), Some(2)), "{st}");
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!((v["errors"].as_array().unwrap().len(), v["pauses"].as_u64()), (0, Some(2)), "{v}");
    let recorded = v["elapsed"].as_f64().unwrap();
    assert!(recorded >= st["elapsed"].as_f64().unwrap(), "{v}");
    every_file_lasts(&s, &v, &["screen", "camera", "mic"], recorded);
    let sides: Vec<Value> = files_of(&v).iter().map(|f| sidecar(f)).collect();
    let pauses: Vec<(u64, u64)> = sides[0]["pauses"].as_array().unwrap().iter().map(|p| (p[0].as_u64().unwrap(), p[1].as_u64().unwrap())).collect();
    assert_eq!(pauses.len(), 2, "{}", sides[0]);
    assert!(pauses[0].0 < pauses[0].1 && pauses[0].1 < pauses[1].0 && pauses[1].0 < pauses[1].1, "in order: {pauses:?}");
    let lens: Vec<f64> = pauses.iter().map(|p| (p.1 - p.0) as f64 / 1e9).collect();
    assert_eq!(sides[0]["paused_ns"].as_u64().unwrap(), pauses.iter().map(|p| p.1 - p.0).sum::<u64>());
    // a pause is on the recording clock (wall time since the start): the first began at the
    // recorded time it reported, the second that plus the first pause
    assert!((pauses[0].0 as f64 / 1e9 - at[0]).abs() < 1e-6, "{pauses:?} {at:?}");
    assert!((pauses[1].0 as f64 / 1e9 - (at[1] + lens[0])).abs() < 1e-6, "{pauses:?} {at:?}");
    assert!(sides.iter().all(|sd| sd["pauses"] == sides[0]["pauses"]));
    if no_frame_dropped(&sides) {
        // the pictures jump at each pause by its length
        for (t, len) in at.iter().zip(&lens) {
            let (a, b) = (index_at(&mut s, &v, "screen", t - 0.15), index_at(&mut s, &v, "screen", t + 0.15));
            let want = (0.3 + len) * 30.0;
            assert!(((b - a) as f64 - want).abs() <= 2.5, "frames {a} and {b} around the pause at {t}, want {want:.1} apart");
        }
        same_moment(&mut s, &v, "screen", "camera", at[1] + 0.2, 2.0);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stop_while_paused_ends_the_recording_where_the_pause_began() {
    let _turn = one_at_a_time();
    let dir = tmp("pause-stop");
    let mut s = session(0);
    start_three(&mut s, &dir);
    std::thread::sleep(Duration::from_millis(500));
    let at_pause = s.execute("record.pause", json!({})).unwrap()["elapsed"].as_f64().unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!((v["errors"].as_array().unwrap().len(), v["pauses"].as_u64(), v["placed"].as_bool()), (0, Some(1), Some(true)), "{v}");
    assert_eq!(v["elapsed"].as_f64().unwrap(), at_pause, "the recording ends where the pause began");
    every_file_lasts(&s, &v, &["screen", "camera", "mic"], at_pause);
    let files = files_of(&v);
    let sides: Vec<Value> = files.iter().map(|f| sidecar(f)).collect();
    for sd in &sides {
        let p = &sd["pauses"];
        assert_eq!(p.as_array().unwrap().len(), 1, "{sd}");
        assert!((p[0][0].as_u64().unwrap() as f64 / 1e9 - at_pause).abs() < 1e-6, "{sd}");
        assert!(sd["paused_ns"].as_u64().unwrap() >= 390_000_000, "the pause lasted until Stop: {sd}");
    }
    // the microphone file ends at the sample the pause began
    assert_eq!(sides[2]["samples"].as_u64().unwrap(), sides[2]["pauses"][0][0].as_u64().unwrap() * 48_000 / 1_000_000_000);
    if no_frame_dropped(&sides) {
        // nothing captured during the pause is at the end of the pictures
        let (first, last) = (index_at(&mut s, &v, "screen", 0.0), index_at(&mut s, &v, "screen", at_pause - 0.01));
        assert!(((last - first) as f64 - at_pause * 30.0).abs() <= 2.5, "frames {first}..{last} in {at_pause} s");
    }
    assert!(!s.record.recording() && s.voiceover.input.is_some());
    // a cancel while paused leaves nothing behind
    let dir2 = tmp("pause-cancel");
    start_three(&mut s, &dir2);
    std::thread::sleep(Duration::from_millis(200));
    s.execute("record.pause", json!({})).unwrap();
    s.execute("record.cancel", json!({})).unwrap();
    assert!(std::fs::read_dir(&dir2).unwrap().next().is_none(), "no files remain");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&dir2).ok();
}

#[test]
fn pause_and_resume_are_refused_when_they_make_no_sense() {
    let dir = tmp("pause-refused");
    let mut s = Session::default();
    s.record.factory = Some(Arc::new(SyntheticFactory { display_size: (320, 180), camera_size: (320, 180), camera_delay_ms: 700, ..Default::default() }));
    let d = dir.to_string_lossy().into_owned();
    let refused = |s: &mut Session, id: &str, why: &str| {
        let e = s.execute(id, json!({})).unwrap_err().to_string();
        assert!(e.contains(why), "{id}: {e}");
        let spec = crate::commands::find(id).unwrap();
        assert_eq!((spec.enabled)(s).unwrap_err(), why, "{id} says why it is disabled");
    };
    // nothing records
    refused(&mut s, "record.pause", "not recording");
    refused(&mut s, "record.resume", "not recording");
    // the sources are still starting (the camera takes 700 ms)
    s.execute("record.start", json!({"camera": {"device": SyntheticFactory::CAMERA}, "mic": {}, "dir": d, "wait": false})).unwrap();
    assert_eq!(s.execute("record.status", json!({})).unwrap()["starting"], true);
    refused(&mut s, "record.pause", "not recording");
    refused(&mut s, "record.resume", "not recording");
    let t0 = std::time::Instant::now();
    while !s.record.recording() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
        s.execute("record.status", json!({})).unwrap();
    }
    assert!(s.record.recording());
    // recording: Resume has nothing to resume; Pause works once
    refused(&mut s, "record.resume", "the recording is not paused");
    assert!((crate::commands::find("record.pause").unwrap().enabled)(&s).is_ok());
    s.execute("record.pause", json!({})).unwrap();
    refused(&mut s, "record.pause", "the recording is already paused");
    assert!(s.record.active.as_ref().unwrap().paused());
    assert_eq!(s.execute("record.status", json!({})).unwrap()["pauses"], 1, "a refused pause is not counted");
    s.execute("record.resume", json!({})).unwrap();
    refused(&mut s, "record.resume", "the recording is not paused");
    // a countdown is not a recording either
    s.execute("record.cancel", json!({})).unwrap();
    s.execute("record.start", json!({"mic": {}, "dir": d, "countdown": 5})).unwrap();
    refused(&mut s, "record.pause", "not recording");
    s.execute("record.cancel", json!({})).unwrap();
    refused(&mut s, "record.resume", "not recording");
    // both are listed with their (empty) parameter doc
    for id in ["record.pause", "record.resume"] {
        let spec = crate::commands::find(id).unwrap();
        assert!(!spec.label.is_empty() && spec.params == "{}", "{id}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn hostile_pause_calls_never_panic_and_a_recording_takes_only_so_many_pauses() {
    let _turn = one_at_a_time();
    let dir = tmp("pause-hostile");
    let mut s = session(0);
    start_three(&mut s, &dir);
    std::thread::sleep(Duration::from_millis(200));
    // the parameters are ignored, whatever they are
    let junk = [json!(null), json!([1, 2, 3]), json!("pause"), json!(1e300), json!(-1), json!({"paused": "no", "elapsed": [], "at": 1e99}), json!(true)];
    for (i, p) in junk.iter().enumerate() {
        let id = if i % 2 == 0 { "record.pause" } else { "record.resume" };
        let r = s.execute(id, p.clone()).unwrap_or_else(|e| panic!("{id} {p}: {e}"));
        assert_eq!(r["paused"], i % 2 == 0, "{id} {p}: {r}");
        assert_eq!(r["pauses"].as_u64().unwrap() as usize, i / 2 + 1, "{id} {p}: {r}");
    }
    // junk[6] paused it: many quick pauses and resumes in a row, some of them refused
    for k in 0..40 {
        let _ = s.execute("record.resume", json!({"k": k}));
        let _ = s.execute("record.resume", json!({}));
        let _ = s.execute("record.pause", json!({"k": k}));
        let _ = s.execute("record.pause", json!({}));
    }
    s.execute("record.resume", json!({})).unwrap();
    assert_eq!(s.execute("record.status", json!({})).unwrap()["pauses"], 44);
    // a recording remembers its pauses until Stop: there is a limit, and the error says what to do
    {
        let ledger = s.record.active.as_ref().unwrap().pauses();
        let t0 = ledger.pause_at(u64::MAX / 4).unwrap();
        ledger.resume_at(t0).unwrap();
        while ledger.count() < crate::record::MAX_PAUSES {
            let t = ledger.pause_at(0).unwrap();
            ledger.resume_at(t).unwrap();
        }
    }
    let e = s.execute("record.pause", json!({})).unwrap_err().to_string();
    assert!(e.contains("paused 10000 times") && e.contains("stop it"), "{e}");
    assert!(!s.record.active.as_ref().unwrap().paused());
    // Stop still finishes the files
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!((v["placed"].as_bool(), v["pauses"].as_u64()), (Some(true), Some(10_000)), "{}", v["errors"]);
    assert!(!s.record.recording() && s.voiceover.input.is_some());
    assert!(s.execute("record.pause", json!({})).is_err() && s.execute("record.resume", json!([])).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stop_after_counts_recorded_time_not_the_pauses() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let dir = tmp("pause-stopafter");
    let mut s = session(0);
    let clock = Arc::new(AtomicU64::new(0));
    s.record.test_clock_ms = Some(clock.clone());
    s.execute("record.settings", json!({"set": {"stopAfterMinutes": 2}})).unwrap();
    s.execute("record.start", json!({"mic": {}, "dir": dir.to_string_lossy()})).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    // one minute recorded, one minute paused, one more minute recorded: three minutes of wall time
    clock.store(60_000, Ordering::Release);
    s.execute("record.pause", json!({})).unwrap();
    clock.store(119_000, Ordering::Release);
    assert_eq!(s.execute("record.status", json!({})).unwrap()["recording"], true);
    clock.store(120_000, Ordering::Release);
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!((st["recording"].as_bool(), st["paused"].as_bool()), (Some(true), Some(true)), "two minutes passed, one of them paused: {st}");
    s.execute("record.resume", json!({})).unwrap();
    clock.store(179_000, Ordering::Release);
    assert_eq!(s.execute("record.status", json!({})).unwrap()["recording"], true);
    clock.store(180_000, Ordering::Release);
    assert_eq!(s.execute("record.status", json!({})).unwrap()["recording"], false);
    let ev = s.record.last_event.clone().unwrap();
    assert_eq!((ev["event"].as_str(), ev["result"]["pauses"].as_u64()), (Some("stoppedAfter"), Some(1)), "{ev}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_pause_is_not_filled_with_silence_in_the_system_audio() {
    let _turn = one_at_a_time();
    let dir = tmp("pause-system-audio");
    let mut s = session(0);
    s.execute(
        "record.start",
        json!({"screen": {"display": SyntheticFactory::DISPLAY, "systemAudio": true}, "mic": {}, "dir": dir.to_string_lossy(), "settings": {"audioFormat": "f32", "channels": "mono"}}),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(450));
    s.execute("record.pause", json!({})).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    s.execute("record.resume", json!({})).unwrap();
    std::thread::sleep(Duration::from_millis(450));
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    let recorded = v["elapsed"].as_f64().unwrap();
    every_file_lasts(&s, &v, &["screen", "mic", "systemAudio"], recorded);
    // the tone runs through: half a second of silence would show in the level
    let pcm = wav_f32(&files_of(&v)[2]);
    let quiet = pcm.chunks(2400).filter(|c| c.len() == 2400 && c.iter().all(|v| v.abs() < 0.01)).count();
    assert!(quiet <= 1, "{quiet} silent 50 ms stretches in the system audio");
    let rms = (pcm.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / pcm.len() as f64).sqrt();
    assert!((rms - 0.25 / 2f64.sqrt()).abs() < 0.02, "the tone fills the file: rms {rms}");
    let sd = sidecar(&files_of(&v)[2]);
    assert_eq!(sd["source"], "systemAudio");
    assert_eq!(sd["pauses"].as_array().unwrap().len(), 1, "{sd}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_still_screen_shows_what_it_looked_like_at_the_resume() {
    use crate::record::{PauseLedger, SourceStats, video_worker};
    use filmcraft_export::recorder::MovRecorder;
    use std::sync::atomic::{AtomicU64, Ordering};
    // a screen that only sends a frame when its picture changes: one before the pause, one
    // during it (the window the person switched to), none after the resume
    let dir = tmp("pause-held");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Held - Screen.mov");
    let rate = filmcraft_time::FrameRate::new(30, 1);
    let rec = MovRecorder::create(&path, 320, 180, rate, false).unwrap();
    let queue = Arc::new(FrameQueue::new(8));
    let (stats, stop, origin, pauses) =
        (Arc::new(SourceStats::default()), Arc::new(AtomicU64::new(u64::MAX)), Arc::new(AtomicU64::new(u64::MAX)), Arc::new(PauseLedger::default()));
    let frame =
        |k: u64, t: u64| CapturedFrame { width: 320, height: 180, stride: 1280, format: PixelFormat::Rgba8, data: synthetic_frame(320, 180, k, t), time_ns: t };
    const S: u64 = 1_000_000_000;
    let worker = {
        let (q, st, sp, o, p) = (queue.clone(), stats.clone(), stop.clone(), origin.clone(), pauses.clone());
        std::thread::spawn(move || video_worker(rec, q, st, sp, o, p, rate))
    };
    origin.store(10 * S, Ordering::Release);
    queue.push(frame(7, 10 * S)); // the picture at the start
    pauses.pause_at(11 * S).unwrap(); // paused after 1 s
    queue.push(frame(40, 11 * S + S / 2)); // the screen changes twice while paused
    queue.push(frame(41, 12 * S));
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(stats.frames.load(Ordering::Relaxed), 1, "nothing is written while paused");
    pauses.resume_at(13 * S).unwrap(); // resumed 2 s later; the screen stays still
    let t0 = std::time::Instant::now();
    while stats.frames.load(Ordering::Relaxed) < 2 && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(stats.frames.load(Ordering::Relaxed), 2, "the picture at the resume is written without waiting for a change");
    // a second pause whose picture is replaced by a real frame right at the resume
    pauses.pause_at(14 * S).unwrap();
    queue.push(frame(50, 14 * S + S / 2));
    pauses.resume_at(15 * S).unwrap();
    queue.push(frame(51, 15 * S + 1_000_000));
    // a third pause, resumed just before Stop: its picture is still written, and what the
    // source delivers after Stop is not
    pauses.pause_at(16 * S).unwrap();
    queue.push(frame(60, 16 * S + S / 5));
    pauses.resume_at(16 * S + S / 2).unwrap();
    stop.store(17 * S + S / 2, Ordering::Release); // 7.5 s of wall time, 3.5 s of them paused
    queue.push(frame(70, 17 * S + S / 2 + 1));
    queue.push(frame(71, 18 * S));
    queue.close();
    let fin = worker.join().unwrap().unwrap();
    assert_eq!(fin.frames, 4, "the start, the pictures at the first and third resumes, the frame at the second");
    assert_eq!(stats.dropped.load(Ordering::Relaxed), 0, "paused frames are not dropped frames");
    let mut s = Session::default();
    let item = crate::commands::import_streamed(&mut s, &path.to_string_lossy(), None).unwrap();
    let info = s.project.item(item).unwrap().as_media().unwrap().info.clone();
    assert!((secs(info.duration) - 4.0).abs() < 0.04, "4 s were recorded: {}", secs(info.duration));
    let src = s.media.source_for(&s.project, item, &*s.services).unwrap();
    let at = |m: f64| {
        let f = src.video_frame(FrameRequest::full(Tick::from_seconds_f64(m))).unwrap();
        read_synthetic_index(&f.luma8(), f.width, f.height).unwrap()
    };
    assert_eq!((at(0.5), at(0.95)), (7, 7), "the picture before the pause");
    assert_eq!((at(1.05), at(1.9)), (41, 41), "from the resume on: the latest picture of the pause, not the one before it");
    assert_eq!((at(2.05), at(2.9)), (51, 51), "a frame that arrives with the resume takes the held one's place");
    assert_eq!((at(3.05), at(3.9)), (60, 60), "a picture held when Stop comes is written; frames after Stop are not");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_pause_ledger_maps_capture_time_to_file_time() {
    use crate::record::PauseLedger;
    let l = PauseLedger::default();
    assert!(!l.paused() && l.count() == 0 && !l.contains(5) && l.before(u64::MAX) == 0);
    assert_eq!(l.file_ns(150, 100), 50);
    assert_eq!(l.file_ns(50, 100), 0, "before the start");
    assert_eq!(l.resume_at(10), None, "nothing to resume");
    assert_eq!(l.pause_at(200), Some(200));
    assert_eq!(l.pause_at(250), None, "already paused");
    assert!(l.paused() && l.count() == 1 && l.open_start() == Some(200));
    // inside the open pause the file time stands still
    assert!(l.contains(200) && l.contains(u64::MAX) && !l.contains(199));
    assert_eq!((l.file_ns(199, 100), l.file_ns(200, 100), l.file_ns(999, 100)), (99, 100, 100));
    assert_eq!(l.kept_file_ns(199, 100), Some(99));
    assert_eq!(l.kept_file_ns(200, 100), None);
    assert_eq!(l.kept_file_ns(99, 100), None, "before the start");
    assert_eq!(l.end_of(250), None, "still open");
    assert_eq!(l.resume_at(300), Some((200, 300)));
    assert_eq!((l.contains(299), l.contains(300), l.end_of(250), l.end_of(300)), (true, false, Some(300), None));
    assert_eq!((l.before(250), l.before(300), l.before(1000)), (50, 100, 100));
    assert_eq!((l.file_ns(300, 100), l.file_ns(350, 100)), (100, 150), "the file goes on right where it stopped");
    // a second pause; a clock that seems to go back never puts a pause before the last one
    assert_eq!(l.pause_at(250), Some(300));
    assert_eq!(l.resume_at(100), Some((300, 300)));
    assert_eq!(l.pause_at(400), Some(400));
    assert_eq!(l.resume_at(450), Some((400, 450)));
    assert_eq!(l.count(), 3);
    assert_eq!(l.file_ns(500, 100), 250);
    assert_eq!(l.intervals(100, 9999), vec![[100, 200], [200, 200], [300, 350]]);
    // sample ranges: one index per time unit here
    let cut = |a, b| l.cut(a, b, |t| t);
    assert_eq!(cut(0, 100), vec![(0, 100)]);
    assert_eq!(cut(150, 500), vec![(150, 200), (300, 400), (450, 500)]);
    assert_eq!(cut(200, 300), vec![]);
    assert_eq!(cut(250, 420), vec![(300, 400)]);
    assert_eq!(cut(450, 450), vec![]);
    assert_eq!(cut(500, 400), vec![], "a backwards range is empty");
    // an open pause cuts everything from its start
    l.pause_at(600);
    assert_eq!(cut(500, 10_000), vec![(500, 600)]);
    assert_eq!(cut(700, u64::MAX), vec![]);
    assert_eq!(l.intervals(100, 650), vec![[100, 200], [200, 200], [300, 350], [500, 550]]);
    assert_eq!(l.intervals(100, 0), vec![[100, 200], [200, 200], [300, 350], [500, 500]], "never ends before it began");
    // hostile times never overflow
    let l = PauseLedger::default();
    l.pause_at(u64::MAX);
    l.resume_at(0);
    assert_eq!(l.file_ns(u64::MAX, 0), u64::MAX);
    assert_eq!(l.before(0), 0);
    assert_eq!(l.cut(0, u64::MAX, |_| u64::MAX), vec![(0, u64::MAX)]);
}
