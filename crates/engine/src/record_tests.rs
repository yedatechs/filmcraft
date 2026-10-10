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
    let first = |i: usize| sides[i]["first_sample_ns"].as_u64().unwrap() as f64 / 1e9;
    let (skew1, skew2) = (first(1) - first(0), first(2) - first(0));
    assert!((0.09..0.16).contains(&skew1), "{skew1}");
    assert!((0.39..0.46).contains(&skew2), "{skew2}");
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
    assert!((at("camera") - skew1).abs() < 0.002, "camera at its first sample: {} vs {skew1}", at("camera"));
    assert!((at("camera2") - (skew2 + 0.1)).abs() < 0.002, "camera 2 moved by its own 100 ms: {} vs {skew2}", at("camera2"));
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
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!(st["recording"], true, "{st}");
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
