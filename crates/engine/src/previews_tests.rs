//! Render previews end to end: render → green, edit → stale, undo → green, save/open keeps them,
//! delete, audio previews, playback frames from previews.

use std::sync::Arc;

use filmcraft_media::Generator;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_project::{ClipId, Label, ParamValue, SequenceSettings, TrackKind, find_effect};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde_json::{Value, json};

use crate::Session;
use crate::previews::BarState;

fn tmp(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("filmcraft-previews-test-{name}-{}-{nanos}", std::process::id()))
}

/// A 64×36 24 fps sequence: V1 = matte with Tint (0..24) + plain matte (24..48), A1 = tone (0..48).
fn session(name: &str) -> (Session, ClipId) {
    let mut s = Session::default();
    s.previews.set_dir(Some(tmp(name)));
    let r = FrameRate::FPS_24;
    let mut p = (*s.project).clone();
    let matte = crate::demo::add_generator(
        &mut p,
        &s.media,
        GeneratorSource::new(Generator::ColorMatte { color: [0.2, 0.5, 0.8, 1.0] }, 64, 36, r, Tick(10 * TICKS_PER_SECOND)),
        "Matte",
        Label::Iris,
        None,
    );
    let tone = crate::demo::add_generator(
        &mut p,
        &s.media,
        GeneratorSource::new(Generator::Tone { hz: 440.0, db: -12.0 }, 0, 0, r, Tick(10 * TICKS_PER_SECOND)),
        "Tone",
        Label::Iris,
        None,
    );
    let seq = p.new_sequence("Seq", SequenceSettings { width: 64, height: 36, frame_rate: r, ..Default::default() }, 2, 1, None);
    let mut a = p.make_track_item(matte, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    let mut b = p.make_track_item(matte, TrackKind::Video, r.tick_of(24), TimeRange::new(r.tick_of(24), r.tick_of(24)), r).unwrap();
    let au = p.make_track_item(tone, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(48)), r).unwrap();
    for e in a.effects.iter_mut().chain(b.effects.iter_mut()) {
        filmcraft_project::resolve_auto_points(e, (64, 36), (64, 36));
    }
    a.effects.push(find_effect("tint").unwrap().instance());
    let a_id = a.id;
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks[0].items = vec![a, b];
    q.audio_tracks[0].items = vec![au];
    s.project = Arc::new(p);
    s.state.active_sequence = Some(seq);
    s.state.open_sequences = vec![seq];
    (s, a_id)
}

fn states(s: &Session) -> Vec<String> {
    let v = crate::previews::bar_json(s).unwrap();
    v["segments"].as_array().unwrap().iter().map(|g| g["state"].as_str().unwrap().to_string()).collect()
}

fn job_ok(s: &Session, v: &Value) {
    let id = v["job"].as_u64().expect("a job was started");
    let j = s.jobs.iter().find(|j| j.id == id).unwrap().to_json();
    assert!(j["finished"].as_bool().unwrap(), "{j}");
    assert!(j["result"].get("error").is_none(), "{j}");
}

#[test]
fn render_turns_green_edit_invalidates_and_undo_restores() {
    let (mut s, a) = session("undo");
    assert_eq!(states(&s), vec!["yellow", "none"]);
    let v = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap();
    assert_eq!(v["segments"], 1, "only the segment with an effect");
    job_ok(&s, &v);
    assert_eq!(states(&s), vec!["green", "none"]);
    assert_eq!(s.previews.count(), 1);
    // playback reads preview frames for the rendered segment only
    let seq = s.state.active_sequence.unwrap();
    let f = s.previews.frame(&s.media, &s.project, seq, 5, 1.0).expect("preview frame");
    assert_eq!((f.width, f.height), (64, 36));
    assert!(s.previews.frame(&s.media, &s.project, seq, 30, 1.0).is_none());
    // the preview frame matches the live render closely (ProRes 10-bit)
    let live = filmcraft_render::render_sequence(
        &s.project,
        seq,
        FrameRate::FPS_24.tick_of(5),
        Default::default(),
        &s.media.provider(s.project.clone(), s.services.clone()),
    );
    let live = live.over_black_rgba8();
    let prev = f.to_rgba8();
    let diff = live.iter().zip(&prev).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
    assert!(diff <= 4, "preview differs from live render by {diff}");
    // edit the effect → stale
    s.edit_sequence("Tint", |q, _, _| {
        let (_, it) = q.find_item_mut(a).unwrap();
        it.effect_mut("tint").unwrap().params.values_mut().find(|p| matches!(p.value, ParamValue::Float(_))).unwrap().value = ParamValue::Float(30.0);
        Ok(())
    })
    .unwrap();
    assert_eq!(states(&s), vec!["yellow", "none"]);
    // undo → the old content hash → green again
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(states(&s), vec!["green", "none"]);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(states(&s), vec!["yellow", "none"]);
    s.execute("edit.undo", json!({})).unwrap();
    // nothing left to render
    let v = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap();
    assert!(v["job"].is_null());
    // Render In to Out also renders the no-bar segment
    let v = s.execute("sequence.renderInToOut", json!({"wait": true})).unwrap();
    assert_eq!(v["segments"], 1);
    assert_eq!(states(&s), vec!["green", "green"]);
    let bar = s.previews.bar(&s.project, s.state.active_sequence.unwrap());
    assert_eq!(bar.len(), 1, "adjacent green spans merge in the drawn bar");
    assert_eq!(bar[0].state, BarState::Green);
    // Delete Render Files In to Out (In/Out around the second clip only)
    s.edit_sequence("marks", |q, _, _| {
        q.mark_in = Some(FrameRate::FPS_24.tick_of(30));
        q.mark_out = Some(FrameRate::FPS_24.tick_of(40));
        Ok(())
    })
    .unwrap();
    s.execute("sequence.deleteRenderFilesInToOut", json!({})).unwrap();
    assert_eq!(states(&s), vec!["green", "none"]);
    s.execute("sequence.deleteRenderFiles", json!({})).unwrap();
    assert_eq!(states(&s), vec!["yellow", "none"]);
    assert_eq!(s.previews.count(), 0);
    assert!(!s.is_enabled("sequence.deleteRenderFiles"), "disabled with no files");
    let _ = std::fs::remove_dir_all(s.previews.dir().unwrap());
}

#[test]
fn previews_survive_save_and_open() {
    let (mut s, _) = session("save");
    let v = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap();
    job_ok(&s, &v);
    let folder = tmp("save-project");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("Film.fcproj").to_string_lossy().to_string();
    s.execute("file.save", json!({"path": path})).unwrap();
    // the unsaved project's previews moved next to the project
    assert_eq!(s.previews.dir().unwrap(), folder.join("FilmCraft Previews").join("Film"));
    assert_eq!(states(&s), vec!["green", "none"]);
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(states(&t), vec!["green", "none"], "hashes are stable across save/load");
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn render_selection_and_audio_previews() {
    let (mut s, a) = session("sel");
    s.state.selection = vec![a];
    let v = s.execute("sequence.renderSelection", json!({"wait": true})).unwrap();
    assert_eq!(v["segments"], 1);
    job_ok(&s, &v);
    // audio: the preview mix equals the live mix
    let seq = s.state.active_sequence.unwrap();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let q = s.project.sequence(seq).unwrap();
    let live = filmcraft_render::audio::mix_sequence(&s.project, q, 1000, 30_000, &provider);
    let v = s.execute("sequence.renderAudio", json!({"wait": true})).unwrap();
    job_ok(&s, &v);
    assert!(s.previews.audio_segments(&s.project, seq).iter().all(|g| s.previews.has_audio(&g.hash)));
    let mixed = s.previews.mix(&s.project, seq, 1000, 30_000, &provider);
    for c in 0..2 {
        for (x, y) in live.channels[c].iter().zip(&mixed.channels[c]) {
            assert!((x - y).abs() < 1e-6);
        }
    }
    assert!(mixed.channels[0].iter().any(|x| x.abs() > 0.05), "tone is audible");
    // muting the track changes the audio hash: the preview no longer applies
    s.edit_sequence("mute", |q, _, _| {
        q.audio_tracks[0].muted = true;
        Ok(())
    })
    .unwrap();
    assert!(!s.previews.audio_segments(&s.project, seq).iter().any(|g| s.previews.has_audio(&g.hash)));
    let _ = std::fs::remove_dir_all(s.previews.dir().unwrap());
}

#[test]
fn render_commands_are_registered_like_premiere() {
    let f = |id| crate::commands::find(id).unwrap();
    assert_eq!(f("sequence.renderEffectsInToOut").shortcut, Some("Enter"));
    for id in [
        "sequence.renderEffectsInToOut",
        "sequence.renderInToOut",
        "sequence.renderSelection",
        "sequence.renderAudio",
        "sequence.deleteRenderFiles",
        "sequence.deleteRenderFilesInToOut",
    ] {
        assert_eq!(f(id).menu, &["Sequence"]);
    }
}

#[test]
fn float_wav_round_trip() {
    let x: Vec<f32> = (0..200).map(|i| (i as f32 * 0.1).sin()).collect();
    assert_eq!(crate::previews::read_wav_f32(&crate::previews::write_wav_f32(&x, 48_000)).unwrap(), x);
}

#[test]
fn bar_state_serializes_lowercase() {
    assert_eq!(serde_json::to_value(BarState::Green).unwrap(), json!("green"));
}

// ---------------------------------------------------------------- disk use: orphans, guard

/// A fresh folder under the build's target directory (never the real Media Cache).
fn target_tmp(name: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = exe.parent().unwrap().join("previews-disk-tests").join(format!("{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

/// An `untitled-<pid>-…` folder with a preview file and (optionally) an `.owner` heartbeat.
fn untitled(root: &std::path::Path, pid: u32, beat: Option<u64>) -> std::path::PathBuf {
    let d = root.join(format!("untitled-{pid}-1f2e3d"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("0123456789abcdef0123456789abcdef.mov"), b"preview").unwrap();
    if let Some(b) = beat {
        std::fs::write(d.join(".owner"), format!("pid={pid}\nheartbeat={b}\n")).unwrap();
    }
    d
}

// pids far above any real pid: no such process is running
const GONE: u32 = 4_000_000_001;

#[test]
fn orphaned_untitled_folders_are_removed_at_startup() {
    let root = target_tmp("orphans");
    let now = unix_now();
    let stale = untitled(&root, GONE, Some(now - 3600));
    let missing = untitled(&root, GONE + 1, None);
    let fresh = untitled(&root, GONE + 2, Some(now - 30));
    let mine = untitled(&root, std::process::id(), Some(now - 3600));
    let other = root.join("not-a-preview-folder");
    std::fs::create_dir_all(&other).unwrap();
    let removed = crate::previews::sweep_orphans(&root, now);
    assert_eq!(removed.len(), 2, "{removed:?}");
    assert!(!stale.exists(), "a stale heartbeat is an orphan");
    assert!(!missing.exists(), "no heartbeat at all is an orphan");
    assert!(fresh.exists(), "a fresh heartbeat is a live session");
    assert!(mine.exists(), "this process's folder is never an orphan");
    assert!(other.exists(), "only untitled-* folders are touched");
    // first use of a preview root sweeps it (Session start: apply_media_cache → set_temp_root)
    let root2 = target_tmp("orphans-startup");
    let stale2 = untitled(&root2, GONE, Some(now - 3600));
    let fresh2 = untitled(&root2, GONE + 2, Some(now));
    let s = Session::default();
    s.previews.set_temp_root(Some(root2.clone()));
    assert!(!stale2.exists());
    assert!(fresh2.exists());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&root2);
}

#[cfg(unix)]
#[test]
fn orphan_sweep_never_follows_symlinks() {
    let root = target_tmp("orphans-link");
    let outside = target_tmp("orphans-outside");
    let victim = untitled(&outside, GONE, None);
    std::os::unix::fs::symlink(&victim, root.join(format!("untitled-{GONE}-link"))).unwrap();
    assert!(crate::previews::sweep_orphans(&root, unix_now()).is_empty());
    assert!(victim.join("0123456789abcdef0123456789abcdef.mov").exists());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn stale_part_files_are_removed_when_the_folder_opens() {
    let dir = target_tmp("parts");
    let old = dir.join("0123456789abcdef0123456789abcdef.mov.part");
    let new = dir.join("fedcba9876543210fedcba9876543210.mov.part");
    let done = dir.join("00112233445566778899aabbccddeeff.mov");
    for p in [&old, &new, &done] {
        std::fs::write(p, b"x").unwrap();
    }
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for p in [&old, &done] {
        std::fs::File::options().write(true).open(p).unwrap().set_modified(hour_ago).unwrap();
    }
    let store = crate::previews::PreviewStore::default();
    store.set_dir(Some(dir.clone()));
    assert!(!old.exists(), "a .part nobody wrote for an hour is left from a crash");
    assert!(new.exists(), "a .part written moments ago may be another session's render");
    assert!(done.exists(), "finished previews stay");
    assert_eq!(store.count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_untitled_folder_goes_with_the_session() {
    let root = target_tmp("lifecycle");
    // shutdown (File ▸ Quit, the app's exit path)
    let (mut s, _) = session("lifecycle");
    s.previews.set_temp_root(Some(root.clone()));
    s.previews.reset_temp();
    let dir = s.previews.dir().unwrap();
    assert!(dir.starts_with(&root));
    let v = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap();
    job_ok(&s, &v);
    let owner = std::fs::read_to_string(dir.join(".owner")).unwrap();
    assert!(owner.starts_with(&format!("pid={}\nheartbeat=", std::process::id())), "{owner}");
    s.shutdown();
    assert!(!dir.exists(), "a clean exit deletes the unsaved project's previews");
    // dropping the session (and its store)
    let (s, _) = session("lifecycle-drop");
    s.previews.set_temp_root(Some(root.clone()));
    s.previews.reset_temp();
    let dir = s.previews.dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    drop(s);
    assert!(!dir.exists());
    // leaving it for another folder (open a saved project, new project)
    let store = crate::previews::PreviewStore::default();
    store.set_temp_root(Some(root.clone()));
    store.reset_temp();
    let a = store.dir().unwrap();
    std::fs::create_dir_all(&a).unwrap();
    store.reset_temp();
    assert!(!a.exists());
    // a saved project's folder is never deleted
    let saved = target_tmp("lifecycle-saved");
    store.set_dir(Some(saved.clone()));
    drop(store);
    assert!(saved.exists());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&saved);
}

fn seg(frames: i64) -> filmcraft_render::preview::Segment {
    filmcraft_render::preview::Segment {
        first_frame: 0,
        frames,
        start: Tick::ZERO,
        end: Tick::ZERO,
        hash: "0".repeat(32),
        need: filmcraft_render::preview::Need::Render,
        cost_ms: 100.0,
        clips: Vec::new(),
    }
}

#[test]
fn preview_size_estimate_uses_the_prores_rate_target() {
    use crate::previews::{estimate_bytes, frame_bytes};
    // ProRes 422 HQ: 900 bits per 16×16 macroblock (220 Mbit/s at 1080p29.97)
    assert_eq!(frame_bytes(1920, 1080), 8160 * 900 / 8);
    assert_eq!(frame_bytes(3840, 2160), 32_400 * 900 / 8);
    assert_eq!(frame_bytes(0, 0), 0);
    assert!(frame_bytes(u32::MAX, u32::MAX) > 0, "hostile sizes saturate");
    // 14 minutes of 3840×2160 at 30 fps in four segments: ~92 GB
    let segs = [seg(6300), seg(6300), seg(6300), seg(6300)];
    let est = estimate_bytes(3840, 2160, &segs);
    assert_eq!(est, 3_645_000 * 25_200 + 4 * 65_536);
    assert_eq!(crate::previews::format_bytes(est), "92 GB");
    assert_eq!(estimate_bytes(3840, 2160, &[seg(-5)]), 65_536, "hostile frame counts");
}

#[test]
fn preview_toast_and_refusal_texts() {
    use crate::previews::{format_bytes, refuse_reason, start_toast};
    assert_eq!(start_toast(4, 11_000_000_000, 57.4 * 60.0), "Rendering 4 preview segments (up to 11 GB, ~57 min). Cancel: × in the status bar.");
    assert_eq!(start_toast(1, 350_000_000, 5.0), "Rendering 1 preview segment (up to 350 MB, ~1 min). Cancel: × in the status bar.");
    assert_eq!(
        refuse_reason(4, 44_000_000_000, Some(7_900_000_000)).unwrap(),
        "Rendering 4 segments needs at least 11 GB (up to 44 GB); 7.9 GB free (FilmCraft keeps 4 GB free). Free space or render a shorter In/Out range."
    );
    // a quarter of the nominal estimate has to fit (ProRes on still content comes out far smaller)
    assert!(refuse_reason(4, 11_000_000_000, Some(7_900_000_000)).is_none(), "11 GB nominal may well be 3 GB real");
    assert!(refuse_reason(4, 44_000_000_000, Some(15_100_000_000)).is_none());
    assert!(refuse_reason(4, 44_000_000_000, Some(14_900_000_000)).is_some(), "the 4 GB reserve counts");
    assert!(refuse_reason(4, u64::MAX, None).is_none(), "unknown free space (web) never refuses");
    assert_eq!(format_bytes(0), "under 1 MB");
}

#[test]
fn render_says_what_it_will_write_and_refuses_to_fill_the_disk() {
    let (mut s, _) = session("guard");
    // plenty of space: a start toast and the estimate in the result
    s.previews.set_space_probe(Some(Arc::new(|_: &std::path::Path| Some(500_000_000_000))));
    s.drain_events();
    let v = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap();
    job_ok(&s, &v);
    assert_eq!(v["estimatedBytes"], 4 * 3 * 900 / 8 * 24 + 65_536);
    let toasts: Vec<String> =
        s.drain_events().into_iter().filter_map(|e| if let crate::Event::Toast { message, .. } = e { Some(message) } else { None }).collect();
    assert!(toasts.iter().any(|t| t.starts_with("Rendering 1 preview segment (up to ")), "{toasts:?}");
    s.execute("sequence.deleteRenderFiles", json!({})).unwrap();
    // 3 GB free: under the 4 GB reserve, nothing starts
    s.previews.set_space_probe(Some(Arc::new(|_: &std::path::Path| Some(3_000_000_000))));
    let jobs = s.jobs.len();
    let e = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap_err().to_string();
    assert!(e.contains("needs at least") && e.contains("3 GB free"), "{e}");
    assert_eq!(s.jobs.len(), jobs, "no job was started");
    assert!(s.drain_events().iter().any(|e| matches!(e, crate::Event::Toast { error: true, message } if message.contains("Free space"))));
    // the disk fills up while rendering: the job stops and leaves no partial file
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    s.previews.set_space_probe(Some(Arc::new(move |_: &std::path::Path| {
        Some(if c.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 { 500_000_000_000 } else { 1_500_000_000 })
    })));
    let v = s.execute("sequence.renderInToOut", json!({"wait": true})).unwrap();
    let id = v["job"].as_u64().unwrap();
    let j = s.jobs.iter().find(|j| j.id == id).unwrap().to_json();
    let err = j["result"]["error"].as_str().unwrap_or_default().to_string();
    assert!(err.starts_with("Rendering stopped: only 1.5 GB free"), "{j}");
    let dir = s.previews.dir().unwrap();
    let parts = std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".part")).count();
    assert_eq!(parts, 0);
    assert_eq!(s.previews.count(), 0);
    let _ = std::fs::remove_dir_all(dir);
}
