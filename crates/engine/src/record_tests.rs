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
    s.record.factory = Some(Arc::new(SyntheticFactory { display_size: (320, 180), camera_size: (320, 180), screen_delay_ms: 0, camera_delay_ms }));
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
    let first = |i: usize| sides[i]["first_sample_ns"].as_u64().unwrap() as f64 / 1e9;
    let skew = first(1) - first(0);
    assert!((0.29..0.36).contains(&skew), "camera starts ~300 ms late: {skew}");
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
    assert_eq!(screen.start, Tick::ZERO);
    let frame = 1.0 / 30.0;
    let cam_at = secs(camera.start) - secs(camera.source_in);
    assert!((cam_at - skew).abs() < 0.002, "camera media time 0 lands at its first sample: {cam_at} vs {skew}");
    assert!((secs(camera.start) - skew).abs() <= frame + 0.001);
    assert!(secs(mic.start) - secs(mic.source_in) <= 0.05, "the mic starts with the screen");
    // the files decode with our decoders and last about 2 s
    for (kind, want) in [("screen", 2.0), ("camera", 1.7), ("mic", 2.0)] {
        let item = filmcraft_project::ItemId(v["items"][kind].as_u64().unwrap());
        let info = s.project.item(item).unwrap().as_media().unwrap().info.clone();
        let d = secs(info.duration);
        assert!((d - want).abs() < 0.35, "{kind}: {d} s, want ~{want}");
        assert_eq!(s.project.item(item).unwrap().metadata.get("Recording Source").map(String::as_str), Some(kind));
    }
    // sync by pixel: what screen and camera show at the same sequence time was captured together
    let period = 1.0 / 30.0;
    for t in [1.0, 1.5] {
        let mut when = Vec::new();
        for (kind, c, side) in [("screen", &screen, &sides[0]), ("camera", &camera, &sides[1])] {
            let item = filmcraft_project::ItemId(v["items"][kind].as_u64().unwrap());
            let src = s.media.source_for(&s.project, item, &*s.services).unwrap();
            let m = Tick::from_seconds_f64(t) - c.start + c.source_in;
            let f = src.video_frame(FrameRequest::full(m)).unwrap();
            let k = read_synthetic_index(&f.luma8(), f.width, f.height).unwrap();
            when.push(side["first_sample_ns"].as_u64().unwrap() as f64 / 1e9 + k as f64 * period);
        }
        assert!((when[0] - when[1]).abs() <= 2.0 * period + 0.001, "at {t}s screen shows {} and camera {}", when[0], when[1]);
    }
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
    let skew = (sidecar(&files[1])["first_sample_ns"].as_u64().unwrap() as f64 - sidecar(&files[0])["first_sample_ns"].as_u64().unwrap() as f64) / 1e9;
    let cam_at = secs(camera.start) - secs(camera.source_in) - (secs(screen.start) - secs(screen.source_in));
    assert!((cam_at - (skew + 0.2)).abs() < 0.002, "camera moved by 200 ms: {cam_at} vs {skew}");
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
