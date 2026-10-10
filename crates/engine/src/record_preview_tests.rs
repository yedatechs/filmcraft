//! Tests of [`crate::record_preview`]: a synthetic camera previewed (frames delivered, nothing
//! written), stopped, and recorded while previewing through the same capture.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::*;
use crate::record::{SyntheticFactory, read_synthetic_index};
use crate::record_preview::{PREVIEW_MAX_WIDTH, PreviewFrame, oriented, preview_of, preview_size};
use filmcraft_media::FrameRequest;
use serde_json::json;

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("filmcraft-recprev-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn session(camera_size: (u32, u32)) -> (Session, SyntheticFactory) {
    let mut s = Session::default();
    let f = SyntheticFactory { display_size: (320, 180), camera_size, ..Default::default() };
    s.record.factory = Some(Arc::new(f.clone()));
    (s, f)
}

/// Wait until camera `device`'s preview has a frame with index ≥ `n`.
fn wait_frame(s: &Session, device: &str, n: u64) -> Arc<crate::record_preview::PreviewFrame> {
    let t0 = Instant::now();
    loop {
        if let Some(f) = preview_of(s, device).and_then(|t| t.latest())
            && f.index >= n
        {
            return f;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "no preview frame {n} of {device}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn record_preview_starts_delivers_frames_and_stops() {
    let (mut s, f) = session((1280, 720));
    let v = s.execute("record.preview", json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 1280, "height": 720, "fps": 30}})).unwrap();
    assert_eq!(v["preview"], json!([SyntheticFactory::CAMERA]), "{v}");
    assert_eq!(v["cameras"][0]["width"], 1280);
    assert_eq!(v["cameras"][0]["fps"], 30);
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!(st["preview"], json!([SyntheticFactory::CAMERA]), "{st}");
    assert_eq!(st["recording"], false);
    // the UI gets a small RGBA picture, not the camera's full size
    let fr = wait_frame(&s, SyntheticFactory::CAMERA, 3);
    assert_eq!((fr.width, fr.height), (PREVIEW_MAX_WIDTH, 360));
    assert_eq!(fr.rgba.len(), 640 * 360 * 4);
    // the burnt-in frame index survives the downscale (luma = R of the grey picture)
    let luma: Vec<u8> = fr.rgba.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
    let idx = read_synthetic_index(&luma, fr.width, fr.height).unwrap();
    assert_eq!(idx + 1, fr.index, "frame {} carries index {idx}", fr.index);
    // later frames replace it
    let later = wait_frame(&s, SyntheticFactory::CAMERA, fr.index + 2);
    assert!(later.time_ns > fr.time_ns);
    // a second camera joins; the first keeps running (the same capture: opened once)
    let opens = f.camera_opens.load(Ordering::Relaxed);
    let v = s
        .execute(
            "record.preview",
            json!({"cameras": [{"device": SyntheticFactory::CAMERA, "width": 1280, "height": 720}, {"device": SyntheticFactory::CAMERA2}]}),
        )
        .unwrap();
    assert_eq!(v["preview"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(f.camera_opens.load(Ordering::Relaxed), opens + 1, "only the second camera was opened");
    wait_frame(&s, SyntheticFactory::CAMERA2, 1);
    // null stops every preview
    let v = s.execute("record.preview", json!({"camera": null})).unwrap();
    assert_eq!(v["preview"], json!([]));
    assert!(s.record.previews.is_empty());
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!(st["preview"], json!([]));
    // `{}` only reports
    assert_eq!(s.execute("record.preview", json!({})).unwrap()["preview"], json!([]));
}

#[test]
fn record_preview_refuses_hostile_params() {
    let (mut s, _) = session((320, 180));
    for p in [
        json!({"camera": {"device": "nope"}}),
        json!({"camera": {}}),
        json!({"camera": 5}),
        json!({"cameras": {}}),
        json!({"cameras": [{"device": SyntheticFactory::CAMERA}, {"device": SyntheticFactory::CAMERA}]}),
        json!({"cameras": [{"device": "a"}, {"device": "b"}, {"device": "c"}, {"device": "d"}, {"device": "e"}]}),
        json!({"camera": {"device": SyntheticFactory::CAMERA, "fps": 0}}),
        json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 1e12}}),
        json!({"camera": {"device": SyntheticFactory::CAMERA, "quality": "8k"}}),
        json!({"camera": {"device": SyntheticFactory::CAMERA}, "cameras": []}),
    ] {
        assert!(s.execute("record.preview", p.clone()).is_err(), "{p}");
    }
    assert!(s.record.previews.is_empty());
}

#[test]
fn recording_a_previewed_camera_shares_its_capture_and_makes_a_correct_file() {
    let dir = tmp("shared");
    let (mut s, f) = session((320, 180));
    s.execute("record.preview", json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 320, "height": 180}})).unwrap();
    wait_frame(&s, SyntheticFactory::CAMERA, 5);
    let opens = f.camera_opens.load(Ordering::Relaxed);
    let r =
        s.execute("record.start", json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 320, "height": 180}, "dir": dir.to_string_lossy()})).unwrap();
    assert_eq!(r["recording"], true, "{r}");
    assert_eq!(f.camera_opens.load(Ordering::Relaxed), opens, "the recording reuses the preview's capture");
    let tap = preview_of(&s, SyntheticFactory::CAMERA).unwrap();
    assert!(tap.recording());
    let before = tap.latest().unwrap().index;
    std::thread::sleep(Duration::from_millis(1500));
    // the preview keeps updating while recording
    assert!(tap.latest().unwrap().index > before + 10);
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!(st["preview"], json!([SyntheticFactory::CAMERA]), "{st}");
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    // the preview survives the stop
    assert!(!tap.recording());
    let after = tap.latest().unwrap().index;
    wait_frame(&s, SyntheticFactory::CAMERA, after + 2);
    // the file: frames on the recording's clock, the first near 0, indexes in order
    let file = v["files"][0].as_str().unwrap().to_string();
    let side: serde_json::Value = serde_json::from_slice(&std::fs::read(file.replace(".mov", ".recording.json")).unwrap()).unwrap();
    assert!(side["first_sample_ns"].as_u64().unwrap() < 100_000_000, "{side}");
    let frames = side["frames"].as_u64().unwrap();
    assert!((35..=55).contains(&frames), "{frames} frames in 1.5 s at 30 fps");
    let item = filmcraft_project::ItemId(v["items"]["camera"].as_u64().unwrap());
    let src = s.media.source_for(&s.project, item, &*s.services).unwrap();
    let rate = filmcraft_time::FrameRate::new(30, 1);
    let mut last = None;
    for k in [0i64, 10, 20] {
        let fr = src.video_frame(FrameRequest::full(rate.tick_of(k))).unwrap();
        let idx = read_synthetic_index(&fr.luma8(), fr.width, fr.height).unwrap();
        if let Some(l) = last {
            assert!(idx > l, "frame {k}: index {idx} after {l}");
        }
        last = Some(idx);
    }
    // without the preview the device closes after the recording; turning it off now stops it
    s.execute("record.preview", json!({"camera": null})).unwrap();
    assert!(tap.format().is_none(), "the camera is stopped");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_recording_asking_another_size_restarts_the_preview_once() {
    let dir = tmp("restart");
    let (mut s, f) = session((640, 360));
    s.execute("record.preview", json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 320, "height": 180}})).unwrap();
    wait_frame(&s, SyntheticFactory::CAMERA, 2);
    let opens = f.camera_opens.load(Ordering::Relaxed);
    s.execute("record.start", json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 640, "height": 360}, "dir": dir.to_string_lossy()})).unwrap();
    assert_eq!(f.camera_opens.load(Ordering::Relaxed), opens, "restarted, not opened again");
    let tap = preview_of(&s, SyntheticFactory::CAMERA).unwrap();
    assert_eq!(tap.format().map(|f| (f.width, f.height)), Some((640, 360)));
    std::thread::sleep(Duration::from_millis(500));
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_preview_started_while_recording_shares_the_recording_capture() {
    let dir = tmp("late");
    let (mut s, f) = session((320, 180));
    s.execute("record.start", json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 320, "height": 180}, "dir": dir.to_string_lossy()})).unwrap();
    let opens = f.camera_opens.load(Ordering::Relaxed);
    s.execute("record.preview", json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 1280, "height": 720}})).unwrap();
    assert_eq!(f.camera_opens.load(Ordering::Relaxed), opens);
    let fr = wait_frame(&s, SyntheticFactory::CAMERA, 3);
    assert_eq!((fr.width, fr.height), (320, 180), "the recording's size is kept: no restart for a preview");
    // the preview stopping during the recording leaves the recording running
    s.execute("record.preview", json!({"camera": null})).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn preview_sizes() {
    assert_eq!(preview_size(3840, 2160), (640, 360));
    assert_eq!(preview_size(1920, 1440), (640, 480));
    assert_eq!(preview_size(320, 181), (320, 180));
    assert_eq!(preview_size(0, 0), (2, 2));
    assert_eq!(preview_size(100_000, 1), (640, 2));
}

#[test]
fn record_preview_mirror_flips_the_picture() {
    // 3 × 2: pixel (x, y) has red = 10·x + y
    let rgba: Vec<u8> = (0..2).flat_map(|y| (0..3).flat_map(move |x| [10 * x + y, 0, 0, 255])).collect();
    let f = PreviewFrame { width: 3, height: 2, rgba, time_ns: 7, index: 3 };
    let px = |f: &PreviewFrame, x: usize, y: usize| f.rgba[(y * f.width as usize + x) * 4];
    assert_eq!(oriented(&f, false, 0), f);
    let m = oriented(&f, true, 0);
    assert_eq!((m.width, m.height, px(&m, 0, 0), px(&m, 2, 1)), (3, 2, 20, 1));
    // a damaged frame comes back as it is
    let bad = PreviewFrame { rgba: vec![1, 2, 3], ..f.clone() };
    assert_eq!(oriented(&bad, true, 90), bad);
}

#[test]
fn record_overlay_exclusion_list_takes_only_our_border_and_preview_windows() {
    use crate::record_preview::{OVERLAY_TITLE, PREVIEW_TITLE, excluded_windows};
    let me = 4242;
    let windows = vec![
        (1, me, "FilmCraft".to_string()),                         // the main window: stays recordable
        (2, me, OVERLAY_TITLE.to_string()),                       // the border
        (3, me, format!("{PREVIEW_TITLE} — FaceTime HD Camera")), // a pop-out preview
        (4, 77, OVERLAY_TITLE.to_string()),                       // another process's window with our title
        (5, me, String::new()),                                   // untitled
        (6, me, format!("Untitled — {OVERLAY_TITLE}")),           // the title must start with it
        (7, 77, "Safari".to_string()),
    ];
    assert_eq!(excluded_windows(&windows, me), vec![2, 3]);
    assert_eq!(excluded_windows(&windows, 1), Vec::<u32>::new());
    assert_eq!(excluded_windows(&[], me), Vec::<u32>::new());
}

#[test]
fn record_an_area_of_the_display() {
    let dir = tmp("area");
    let (mut s, _) = session((320, 180));
    s.record.factory = Some(Arc::new(SyntheticFactory { display_size: (1280, 720), ..Default::default() }));
    // refused: not four whole numbers, too small, outside the display, a window with an area
    for area in [json!([0, 0, 640]), json!([0, 0, 32, 360]), json!([-1, 0, 640, 360]), json!([0.5, 0, 640, 360]), json!("all"), json!([0, 0, 9000, 360])] {
        let p = json!({"screen": {"display": SyntheticFactory::DISPLAY, "area": area}, "dir": dir.to_string_lossy()});
        assert!(s.execute("record.start", p.clone()).is_err(), "{p}");
    }
    let p = json!({"screen": {"display": SyntheticFactory::DISPLAY, "area": [700, 400, 640, 360]}, "dir": dir.to_string_lossy()});
    let e = s.execute("record.start", p).unwrap_err().to_string();
    assert!(e.contains("outside"), "{e}");
    assert!(!s.record.recording());
    let p = json!({"screen": {"window": SyntheticFactory::WINDOW, "area": [0, 0, 640, 360]}, "dir": dir.to_string_lossy()});
    assert!(s.execute("record.start", p).is_err());
    // a 640 × 360 area (odd sizes are made even)
    let r = s
        .execute("record.start", json!({"screen": {"display": SyntheticFactory::DISPLAY, "area": [100, 100, 641, 361]}, "dir": dir.to_string_lossy()}))
        .unwrap();
    assert_eq!(r["recording"], true, "{r}");
    std::thread::sleep(Duration::from_millis(400));
    // nothing to refresh on a synthetic display, but the call goes through
    crate::record_preview::refresh_exclusions(&mut s).unwrap();
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    let file = v["files"][0].as_str().unwrap();
    let side: serde_json::Value = serde_json::from_slice(&std::fs::read(file.replace(".mov", ".recording.json")).unwrap()).unwrap();
    assert_eq!((side["width"].as_u64(), side["height"].as_u64()), (Some(640), Some(360)), "{side}");
    assert_eq!(side["area"], json!([100, 100, 640, 360]));
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height), (640, 360));
    // the display's frame on the desktop (for the border)
    let f = crate::record_preview::capture_factory(&s);
    let fr = crate::record_preview::screen_frame(&f, &crate::record::ScreenTarget::Display(SyntheticFactory::DISPLAY.into())).unwrap();
    assert_eq!((fr.w, fr.h, fr.pixels), (1280.0, 720.0, (1280, 720)));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn record_preview_rotate_turns_the_picture_clockwise() {
    // 3 × 2: pixel (x, y) has red = 10·x + y
    let rgba: Vec<u8> = (0..2).flat_map(|y| (0..3).flat_map(move |x| [10 * x + y, 0, 0, 255])).collect();
    let f = PreviewFrame { width: 3, height: 2, rgba, time_ns: 0, index: 1 };
    let px = |f: &PreviewFrame, x: usize, y: usize| f.rgba[(y * f.width as usize + x) * 4];
    // 90° clockwise: 2 × 3, the bottom-left source pixel at the top left, the top left at the top right
    let r = oriented(&f, false, 90);
    assert_eq!((r.width, r.height), (2, 3));
    assert_eq!((px(&r, 0, 0), px(&r, 1, 0), px(&r, 1, 2), px(&r, 0, 2)), (1, 0, 20, 21));
    let r = oriented(&f, false, 180);
    assert_eq!((r.width, r.height, px(&r, 0, 0), px(&r, 2, 1)), (3, 2, 21, 0));
    let r = oriented(&f, false, 270);
    assert_eq!((r.width, r.height, px(&r, 0, 0), px(&r, 1, 2)), (2, 3, 20, 1));
    // mirrored, then turned (the clip: Horizontal Flip first, then Motion)
    let r = oriented(&f, true, 90);
    assert_eq!((px(&r, 0, 0), px(&r, 1, 0)), (21, 20));
    assert_eq!(oriented(&f, false, 45), f, "not a quarter turn: as it is");
    // on the synthetic camera: the burnt-in index row ends up in the right-hand column
    let (mut s, _) = session((320, 180));
    s.execute("record.preview", json!({"camera": {"device": SyntheticFactory::CAMERA, "width": 320, "height": 180}})).unwrap();
    let fr = wait_frame(&s, SyntheticFactory::CAMERA, 3);
    let r = oriented(&fr, false, 90);
    assert_eq!((r.width, r.height), (180, 320));
    // turn it back by reading the columns: the top row of the source is the last column
    let back: Vec<u8> = (0..fr.height as usize)
        .flat_map(|y| (0..fr.width as usize).map(move |x| (x, y)))
        .map(|(x, y)| r.rgba[(x * r.width as usize + (r.width as usize - 1 - y)) * 4])
        .collect();
    assert_eq!(read_synthetic_index(&back, fr.width, fr.height).map(|i| i + 1), Some(fr.index));
}

/// The `tkhd` matrix of a recording's (only) track, read from the bytes (version 0 header: the
/// matrix is 40 bytes after the box type), and the size the importer reports for the file.
pub(crate) fn file_rotation(path: &str) -> ([i32; 9], (u32, u32)) {
    let data: Arc<[u8]> = std::fs::read(path).unwrap().into();
    let at = data.windows(4).position(|w| w == b"tkhd").unwrap() + 4 + 40;
    let m: Vec<i32> = data[at..at + 36].chunks(4).map(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect();
    let src = filmcraft_codecs::mp4::Mp4Source::open("r.mov", data).unwrap();
    let v = filmcraft_media::MediaSource::info(&src).video.clone().unwrap();
    (m.try_into().unwrap(), (v.width, v.height))
}

/// The `tkhd` matrices of a `w × h` picture turned 90° / 270° clockwise.
pub(crate) fn turned(deg: u32, w: i32, h: i32) -> [i32; 9] {
    match deg {
        90 => [0, 0x10000, 0, -0x10000, 0, 0, h << 16, 0, 0x4000_0000],
        180 => [-0x10000, 0, 0, 0, -0x10000, 0, w << 16, h << 16, 0x4000_0000],
        270 => [0, -0x10000, 0, 0x10000, 0, 0, 0, w << 16, 0x4000_0000],
        _ => [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000],
    }
}

#[test]
fn record_a_rotated_camera_writes_the_turn_into_the_file() {
    use crate::record::SyntheticFactory as F;
    let dir = tmp("rotate");
    let (mut s, _) = session((320, 180));
    for bad in [json!(45), json!(-90), json!("90"), json!(360)] {
        let p = json!({"camera": {"device": F::CAMERA, "rotate": bad}, "dir": dir.to_string_lossy()});
        assert!(s.execute("record.start", p.clone()).is_err(), "{p}");
    }
    // with the screen leading: the camera turned 90° is an upright 180 × 320 clip by itself
    s.execute(
        "record.start",
        json!({"screen": {"display": F::DISPLAY}, "camera": {"device": F::CAMERA, "width": 320, "height": 180, "rotate": 90}, "dir": dir.to_string_lossy()}),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height), (320, 180));
    let id = filmcraft_project::ClipId(v["clips"]["camera"]["clip"].as_u64().unwrap());
    let cam = q.find_item(id).unwrap().1.clone();
    let m = cam.effect("motion").unwrap();
    assert_eq!(m.f64_at("rotation", Tick::ZERO), 0.0, "nothing is added to the clip");
    assert_eq!(m.f64_at("scale", Tick::ZERO), 100.0, "Default Media Scaling is None");
    let media = s.project.resolve_media(cam.item).unwrap().1.info.video.clone().unwrap();
    assert_eq!((media.width, media.height), (180, 320), "the item is upright");
    let file = v["files"][1].as_str().unwrap();
    assert_eq!(file_rotation(file), (turned(90, 320, 180), (180, 320)));
    let side: serde_json::Value = serde_json::from_slice(&std::fs::read(file.replace(".mov", ".recording.json")).unwrap()).unwrap();
    assert_eq!(side["rotate"], 90, "{side}");
    assert_eq!((side["width"].as_u64(), side["height"].as_u64()), (Some(320), Some(180)), "the pictures stay as captured");
    // Set to Frame Size fits the portrait clip like any other
    s.prefs.media.default_media_scaling = "setToFrameSize".into();
    s.execute(
        "record.start",
        json!({"screen": {"display": F::DISPLAY}, "camera": {"device": F::CAMERA, "width": 320, "height": 180, "rotate": 90}, "dir": dir.to_string_lossy()}),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let v = s.execute("record.stop", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let id = filmcraft_project::ClipId(v["clips"]["camera"]["clip"].as_u64().unwrap());
    let m = q.find_item(id).unwrap().1.effect("motion").unwrap().clone();
    assert_eq!((m.f64_at("rotation", Tick::ZERO), m.f64_at("scale", Tick::ZERO)), (0.0, 56.3));
    s.prefs.media.default_media_scaling = "none".into();
    // the camera alone, turned: an upright sequence it fills; the setting is the default
    s.execute("record.settings", json!({"set": {"cameraRotate": 270}})).unwrap();
    assert!(s.execute("record.settings", json!({"set": {"cameraRotate": 45}})).is_err());
    s.execute("record.start", json!({"camera": {"device": F::CAMERA, "width": 320, "height": 180}, "dir": dir.to_string_lossy()})).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let v = s.execute("record.stop", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height), (180, 320));
    let id = filmcraft_project::ClipId(v["clips"]["camera"]["clip"].as_u64().unwrap());
    let m = q.find_item(id).unwrap().1.effect("motion").unwrap().clone();
    assert_eq!((m.f64_at("rotation", Tick::ZERO), m.f64_at("scale", Tick::ZERO)), (0.0, 100.0));
    assert_eq!(file_rotation(v["files"][0].as_str().unwrap()), (turned(270, 320, 180), (180, 320)));
    std::fs::remove_dir_all(&dir).ok();
}
