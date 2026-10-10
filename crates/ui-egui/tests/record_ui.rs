//! Headless UI tests of the Record panel (Window ▸ Record) with the engine's synthetic screen,
//! camera and microphone: Record, live counters and the status-bar line, Stop → a synced
//! sequence; Cancel → nothing left behind; several camera rows; the Settings section; the
//! countdown.

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::record::SyntheticFactory;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("filmcraft-record-ui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

impl Driver {
    fn new(dir: &std::path::Path) -> Self {
        let mut session = Session::default();
        session.record.factory = Some(Arc::new(SyntheticFactory { display_size: (320, 180), camera_size: (320, 180), ..Default::default() }));
        Arc::make_mut(&mut session.project).settings.scratch.captured = Some(dir.to_string_lossy().into_owned());
        // Record starts at once (the countdown test turns it back on)
        session.prefs.recording.countdown_seconds = 0;
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    /// Step frames for about `secs` of wall-clock time (the synthetic sources run in real time).
    fn run_for(&mut self, secs: f64) {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs_f64(secs) {
            self.frames(1);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn element(&mut self, id: &str) -> Option<Value> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().find(|e| e["id"] == id).cloned()
    }

    fn sequences(&self) -> usize {
        self.harness.state().session.project.items.values().filter(|i| i.as_sequence().is_some()).count()
    }
}

#[test]
fn record_and_stop_builds_a_synced_sequence() {
    let dir = tmp("stop");
    let mut d = Driver::new(&dir);
    d.ok("ui.menu.invoke", json!({"id": "window.record"}));
    d.frames(4);
    for id in [
        "record.panel",
        "record.panel.screen",
        "record.panel.camera.1.device",
        "record.panel.camera.1.quality",
        "record.panel.camera.1.mirror",
        "record.panel.camera.1.offset",
        "record.panel.camera.1.remove",
        "record.panel.camera.add",
        "record.panel.mic.1.device",
        "record.panel.mic.add",
        "record.panel.name",
        "record.panel.record",
        "record.panel.cancel",
    ] {
        assert!(d.element(id).is_some(), "{id} is on screen");
    }
    // opening preselects the first display, the first camera and the default microphone
    let ui = d.ok("ui.inspect", json!({}))["ui"]["record"].clone();
    assert_eq!(ui["open"], true);
    assert_eq!(ui["screen"], "display:synthetic:display");
    assert_eq!(ui["cameras"][0]["device"], "synthetic:camera");
    assert_eq!(ui["mics"][0]["device"], "default");
    // the panel is driven by ui.set like any other state
    d.ok("ui.set", json!({"record": {"cameras": [{"device": "synthetic:camera", "quality": "native", "offsetMs": -40}], "name": "Panel Take"}}));
    d.frames(2);
    let before = d.sequences();
    d.click("record.panel.record");
    assert!(d.harness.state().session.record.recording(), "{}", d.harness.state().ui.status);
    d.run_for(1.2);
    let live = d.harness.state().ui.record.live.clone();
    assert!(live.starts_with("Recording · 00:0") && live.contains("screen ") && live.contains("camera "), "{live}");
    let counters = d.element("record.panel.counters").unwrap();
    assert!(counters["label"].as_str().unwrap().contains("frames"), "{counters}");
    assert!(d.element("record.panel.level").is_some());
    d.click("record.panel.record"); // Stop
    let s = &d.harness.state().session;
    assert!(!s.record.recording());
    assert_eq!(d.sequences(), before + 1);
    let q = s.active_sequence().unwrap();
    assert_eq!(s.project.item(s.state.active_sequence.unwrap()).unwrap().name, "Panel Take");
    assert_eq!(q.video_tracks.len() + q.audio_tracks.len(), 3);
    assert!(q.video_tracks.iter().chain(q.audio_tracks.iter()).all(|t| t.items.len() == 1));
    assert_eq!(q.markers[0].comment, "camera offset -40 ms");
    let last = d.harness.state().ui.record.last.clone();
    assert!(last.starts_with("Recorded Panel Take: 3 file(s)"), "{last}");
    d.frames(2);
    let counters = d.element("record.panel.counters").unwrap();
    assert!(counters["label"].as_str().unwrap().contains("Recorded"), "{counters}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 6, "three files and three sidecars");
    assert!(d.harness.state().ui.record.live.is_empty());
    // a recording stopped from outside the panel clears the live line too
    d.ok("engine.execute", json!({"command": "record.start", "params": {"mic": {}}}));
    d.run_for(0.3);
    assert!(!d.harness.state().ui.record.live.is_empty());
    d.ok("engine.execute", json!({"command": "record.cancel", "params": {}}));
    d.frames(2);
    assert!(d.harness.state().ui.record.live.is_empty());
    // the panel can be closed again once stopped
    d.click("record.panel.close");
    assert!(!d.harness.state().ui.record.open);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn cancel_discards_and_leaves_no_files() {
    let dir = tmp("cancel");
    let mut d = Driver::new(&dir);
    // the Program monitor's red dot opens the panel too (the transport shows with a sequence open)
    d.ok("engine.execute", json!({"command": "file.openDemoProject", "params": {}}));
    d.ok("engine.execute", json!({"command": "file.projectSettings.scratchDisks", "params": {"captured": dir.to_string_lossy()}}));
    d.frames(4);
    d.click("program.transport.record");
    assert!(d.harness.state().ui.record.open);
    d.frames(2);
    let before = d.sequences();
    d.click("record.panel.record");
    assert!(d.harness.state().session.record.recording());
    d.run_for(0.6);
    // the panel cannot be closed while recording
    d.click("record.panel.close");
    assert!(d.harness.state().ui.record.open);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3, "the files are written while recording");
    d.click("record.panel.cancel");
    assert!(!d.harness.state().session.record.recording());
    assert_eq!(d.sequences(), before);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "no files left");
    assert_eq!(d.harness.state().ui.record.last, "Recording discarded");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_refused_start_shows_the_error_in_the_panel() {
    let dir = tmp("error");
    let mut d = Driver::new(&dir);
    d.ok("ui.set", json!({"record": {"open": true, "initialized": true, "screen": "display:gone", "cameras": [], "mics": []}}));
    d.frames(3);
    d.click("record.panel.record");
    assert!(!d.harness.state().session.record.recording());
    let e = d.element("record.panel.error").unwrap();
    assert!(e["label"].as_str().unwrap().contains("no display"), "{e}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_second_camera_row_records_to_its_own_track() {
    let dir = tmp("rows");
    let mut d = Driver::new(&dir);
    d.ok("ui.menu.invoke", json!({"id": "window.record"}));
    d.frames(3);
    assert!(d.element("record.panel.camera.2.device").is_none());
    d.click("record.panel.camera.add");
    let ui = d.ok("ui.inspect", json!({}))["ui"]["record"].clone();
    assert_eq!(ui["cameras"].as_array().unwrap().len(), 2);
    assert_eq!(ui["cameras"][1]["device"], "synthetic:camera2", "the new row takes the next free camera");
    assert!(d.element("record.panel.camera.2.device").is_some());
    d.click("record.panel.camera.2.mirror");
    d.click("record.panel.mic.add");
    d.click("record.panel.mic.2.device");
    d.click("record.panel.mic.2.device.3"); // Off, Default Input, Synthetic Input, Synthetic Input 2
    let ui = d.ok("ui.inspect", json!({}))["ui"]["record"].clone();
    assert_eq!(ui["cameras"][1]["mirror"], true);
    assert_eq!(ui["mics"][1]["device"], "Synthetic Input 2");
    d.click("record.panel.record");
    assert!(d.harness.state().session.record.recording(), "{}", d.harness.state().ui.record.error);
    d.run_for(0.8);
    d.click("record.panel.record");
    let s = &d.harness.state().session;
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (3, 2));
    assert_eq!(q.video_tracks[2].items[0].effects.first().map(|e| e.effect.as_str()), Some("horizontal_flip"));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 10, "five files and five sidecars");
    // a row's − removes it
    d.click("record.panel.camera.2.remove");
    assert_eq!(d.harness.state().ui.record.cameras.len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_settings_section_and_preferences_show_the_same_recording_settings() {
    let dir = tmp("settings");
    let mut d = Driver::new(&dir);
    d.ok("ui.menu.invoke", json!({"id": "window.record"}));
    d.frames(3);
    assert!(d.element("record.panel.settings.screenFps").is_none(), "collapsed at first");
    d.click("record.panel.settings");
    assert!(d.harness.state().ui.record.settings_open);
    for id in [
        "screenFps",
        "screenResolution",
        "showCursor",
        "systemAudio",
        "codec",
        "quality",
        "keyframeSeconds",
        "sampleRate",
        "autoGain",
        "countdownSeconds",
        "stopAfterMinutes",
        "outputFolder",
    ] {
        assert!(d.element(&format!("record.panel.settings.{id}")).is_some(), "{id}");
    }
    d.click("record.panel.settings.screenFps");
    d.click("record.panel.settings.screenFps.3"); // 15, 24, 30, 60
    d.click("record.panel.settings.showCursor");
    let g = d.ok("engine.execute", json!({"command": "record.settings", "params": {"get": true}}));
    assert_eq!(g["settings"]["screenFps"], 60, "{g}");
    assert_eq!(g["settings"]["showCursor"], false);
    // a change made elsewhere shows in the panel
    d.ok("engine.execute", json!({"command": "record.settings", "params": {"set": {"codec": "prores"}}}));
    d.frames(2);
    assert!(d.element("record.panel.settings.codec").unwrap()["label"].as_str().unwrap().contains("ProRes"));
    // Preferences ▸ Recording shows the same values
    d.ok("ui.menu.invoke", json!({"id": "app.settings.recording"}));
    d.frames(3);
    let fps = d.element("settings.recording.screenFps").unwrap();
    assert!(fps["label"].as_str().unwrap().contains("60"), "{fps}");
    assert!(d.element("settings.recording.outputFolder.browse").is_some());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_countdown_shows_then_records_and_escape_cancels_it() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let dir = tmp("countdown");
    let mut d = Driver::new(&dir);
    let clock = Arc::new(AtomicU64::new(10_000));
    d.harness.state_mut().session.record.test_clock_ms = Some(clock.clone());
    d.ok("engine.execute", json!({"command": "record.settings", "params": {"set": {"countdownSeconds": 3}}}));
    d.ok(
        "ui.set",
        json!({"record": {"open": true, "initialized": true, "screen": "display:synthetic:display", "cameras": [], "mics": [{"device": "default"}]}}),
    );
    d.frames(3);
    d.click("record.panel.record");
    assert!(!d.harness.state().session.record.recording());
    assert_eq!(d.harness.state().ui.record.live, "Recording in 3…");
    assert!(d.element("record.panel.record").unwrap()["label"].as_str().unwrap().starts_with("Recording in 3…"));
    clock.store(11_500, Ordering::Release);
    d.frames(2);
    assert_eq!(d.harness.state().ui.record.live, "Recording in 2…");
    clock.store(13_000, Ordering::Release);
    d.frames(2);
    assert!(d.harness.state().session.record.recording(), "{}", d.harness.state().ui.record.error);
    d.run_for(0.5);
    assert!(d.harness.state().ui.record.live.starts_with("Recording · "));
    d.click("record.panel.record"); // Stop
    assert!(!d.harness.state().session.record.recording());
    // Esc during the countdown cancels it
    d.click("record.panel.record");
    assert!(d.harness.state().session.record.countdown.is_some());
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    assert!(d.harness.state().session.record.countdown.is_none());
    assert_eq!(d.harness.state().ui.record.last, "Countdown cancelled");
    clock.store(30_000, Ordering::Release);
    d.frames(2);
    assert!(!d.harness.state().session.record.recording());
    assert!(d.harness.state().ui.record.live.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}
