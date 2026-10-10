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
        Self::with_display(dir, (320, 180))
    }

    fn with_display(dir: &std::path::Path, display_size: (u32, u32)) -> Self {
        let mut session = Session::default();
        session.record.factory = Some(Arc::new(SyntheticFactory { display_size, camera_size: (320, 180), ..Default::default() }));
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

    /// Step frames until the recording runs: Record starts the sources on their own thread and
    /// the recording begins when every one of them is live.
    fn wait_recording(&mut self) -> bool {
        let t0 = Instant::now();
        while !self.harness.state().session.record.recording() && t0.elapsed() < Duration::from_secs(5) {
            self.frames(1);
            std::thread::sleep(Duration::from_millis(10));
        }
        self.harness.state().session.record.recording()
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
    assert!(d.wait_recording(), "{}", d.harness.state().ui.status);
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
    assert!(d.wait_recording());
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
    assert!(d.wait_recording(), "{}", d.harness.state().ui.record.error);
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
    assert!(d.wait_recording(), "{}", d.harness.state().ui.record.error);
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

#[test]
fn record_says_starting_until_every_source_is_live() {
    let dir = tmp("starting");
    let mut d = Driver::new(&dir);
    // a screen that takes 1.5 s to deliver its first frame (ScreenCaptureKit's first start)
    d.harness.state_mut().session.record.factory =
        Some(Arc::new(SyntheticFactory { display_size: (320, 180), camera_size: (320, 180), screen_delay_ms: 1500, ..Default::default() }));
    d.ok(
        "ui.set",
        json!({"record": {"open": true, "initialized": true, "screen": "display:synthetic:display", "cameras": [], "mics": [{"device": "default"}]}}),
    );
    d.frames(3);
    d.click("record.panel.record");
    assert!(!d.harness.state().session.record.recording());
    assert_eq!(d.harness.state().ui.record.live, "Starting screen capture…");
    assert_eq!(d.element("record.panel.record").unwrap()["label"], "Starting screen capture…");
    let st = d.ok("engine.execute", json!({"command": "record.status", "params": {}}));
    assert_eq!((st["starting"].as_bool(), st["recording"].as_bool()), (Some(true), Some(false)), "{st}");
    // the button does nothing while the sources start
    d.click("record.panel.record");
    assert!(d.harness.state().session.record.starting.is_some());
    assert!(d.wait_recording(), "{}", d.harness.state().ui.record.error);
    d.frames(1);
    let live = d.harness.state().ui.record.live.clone();
    assert!(live.starts_with("Recording · 00:00"), "the clock starts when every source is live: {live}");
    d.run_for(0.5);
    d.click("record.panel.record"); // Stop
    assert!(!d.harness.state().session.record.recording());
    let s = &d.harness.state().session;
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks[0].items[0].start == filmcraft_time::Tick::ZERO && q.audio_tracks[0].items[0].start == filmcraft_time::Tick::ZERO);
    std::fs::remove_dir_all(&dir).ok();
}

/// Wait (stepping frames) until element `id`'s label satisfies `ok`.
fn wait_label(d: &mut Driver, id: &str, ok: impl Fn(&str) -> bool) -> String {
    let t0 = Instant::now();
    loop {
        d.frames(1);
        if let Some(e) = d.element(id) {
            let l = e["label"].as_str().unwrap_or("").to_string();
            if ok(&l) {
                return l;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "{id}: {l}");
        } else {
            assert!(t0.elapsed() < Duration::from_secs(5), "{id} is not on screen");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn frame_no(label: &str) -> u64 {
    label.rsplit("frame ").next().and_then(|n| n.trim().parse().ok()).unwrap_or(0)
}

#[test]
fn a_camera_row_shows_a_live_preview_and_pops_out() {
    let dir = tmp("preview");
    let mut d = Driver::new(&dir);
    d.ok("ui.menu.invoke", json!({"id": "window.record"}));
    d.ok("ui.set", json!({"record": {"cameras": [{"device": "synthetic:camera", "quality": "native"}]}}));
    // the row's camera runs live: "320×180 @ 30 fps · frame n", n going up
    let l = wait_label(&mut d, "record.panel.camera.1.preview", |l| l.contains("320×180 @ 30 fps") && frame_no(l) > 2);
    let n = frame_no(&l);
    wait_label(&mut d, "record.panel.camera.1.preview", |l| frame_no(l) > n + 3);
    let st = d.ok("engine.execute", json!({"command": "record.status", "params": {}}));
    assert_eq!(st["preview"], json!(["synthetic:camera"]), "{st}");
    // the thumbnail is a 16:9 box about 240 px wide
    let e = d.element("record.panel.camera.1.preview").unwrap();
    assert_eq!((e["rect"][2].as_f64().unwrap(), e["rect"][3].as_f64().unwrap()), (240.0, 135.0));
    // Pop out: a preview window that keeps showing the camera
    d.click("record.panel.camera.1.popout");
    assert!(d.harness.state().ui.record.cameras[0].popout);
    let l = wait_label(&mut d, "record.preview.1", |l| frame_no(l) > 0);
    assert!(l.contains("320×180"), "{l}");
    // recording keeps the preview (and the window) going, through the same capture
    d.click("record.panel.record");
    assert!(d.wait_recording(), "{}", d.harness.state().ui.record.error);
    let l = d.element("record.preview.1").unwrap()["label"].as_str().unwrap().to_string();
    let n = frame_no(&l);
    d.run_for(0.8);
    wait_label(&mut d, "record.preview.1", |l| frame_no(l) > n + 5);
    d.click("record.panel.record"); // Stop
    assert!(!d.harness.state().session.record.recording());
    let q = d.harness.state().session.active_sequence().unwrap();
    assert_eq!(q.video_tracks.len(), 2, "screen and camera");
    // the camera set to Off stops its preview and closes the window
    d.ok("ui.set", json!({"record": {"cameras": [{"device": ""}]}}));
    d.frames(3);
    let st = d.ok("engine.execute", json!({"command": "record.status", "params": {}}));
    assert_eq!(st["preview"], json!([]), "{st}");
    assert!(d.element("record.preview.1").is_none());
    assert_eq!(d.element("record.panel.camera.1.preview").unwrap()["label"], "Camera off");
    // back on, then closing the panel (nothing recording) stops it too
    d.ok("ui.set", json!({"record": {"cameras": [{"device": "synthetic:camera", "quality": "native"}]}}));
    wait_label(&mut d, "record.panel.camera.1.preview", |l| frame_no(l) > 0);
    d.click("record.panel.close");
    d.frames(2);
    assert!(!d.harness.state().ui.record.open);
    let st = d.ok("engine.execute", json!({"command": "record.status", "params": {}}));
    assert_eq!(st["preview"], json!([]), "{st}");
    std::fs::remove_dir_all(&dir).ok();
}

fn overlay(d: &mut Driver) -> Value {
    d.ok("ui.inspect", json!({}))["ui"]["record"]["overlay"].clone()
}

#[test]
fn the_border_frames_the_screen_follows_the_recording_and_an_area_can_be_drawn() {
    let dir = tmp("overlay");
    let mut d = Driver::with_display(&dir, (1280, 720));
    // the border needs a screen: nothing before the panel opens
    assert!(overlay(&mut d).is_null());
    d.ok("ui.menu.invoke", json!({"id": "window.record"}));
    d.ok("ui.set", json!({"record": {"cameras": [], "mics": []}}));
    d.frames(4);
    // the display is chosen: a grey frame around all of it
    let o = overlay(&mut d);
    assert_eq!(o["state"], "idle", "{o}");
    assert_eq!(o["target"], "display:synthetic:display");
    assert_eq!(o["rect"], json!([0, 0, 1280, 720]));
    let e = d.element("record.overlay").unwrap();
    assert_eq!(e["label"], "idle display:synthetic:display 0,0 1280×720");
    // recording turns it red
    d.click("record.panel.record");
    assert!(d.wait_recording(), "{}", d.harness.state().ui.record.error);
    d.frames(2);
    assert_eq!(overlay(&mut d)["state"], "recording");
    assert!(d.element("record.overlay").unwrap()["label"].as_str().unwrap().starts_with("recording "));
    d.run_for(0.4);
    // stopping closes it
    d.click("record.panel.record");
    assert!(!d.harness.state().session.record.recording());
    d.frames(2);
    assert!(overlay(&mut d).is_null(), "{}", overlay(&mut d));
    assert!(d.element("record.overlay").is_none());
    // Area of Synthetic Display…: the border becomes a drawing surface
    d.click("record.panel.screen");
    d.click("record.panel.screen.area.synthetic:display");
    let o = overlay(&mut d);
    assert_eq!(o["state"], "drawing", "{o}");
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["record"]["drawing"], true);
    let e = d.element("record.overlay").unwrap();
    let (w, h) = (e["rect"][2].as_f64().unwrap(), e["rect"][3].as_f64().unwrap());
    assert_eq!((w, h), (1280.0, 720.0), "the 1280 × 720 synthetic display at 1:1 in the 1600 × 980 window");
    d.ok(
        "ui.drag",
        json!({"from": {"id": "record.overlay", "fx": 101.0 / 1280.0, "fy": 99.0 / 720.0}, "to": {"id": "record.overlay", "fx": 741.0 / 1280.0, "fy": 459.0 / 720.0}}),
    );
    d.frames(3);
    let ui = d.ok("ui.inspect", json!({}))["ui"]["record"].clone();
    assert_eq!(ui["screenArea"], json!([100, 98, 640, 360]), "{ui}");
    assert_eq!(ui["drawing"], false);
    assert_eq!(ui["overlay"]["state"], "idle");
    assert_eq!(ui["overlay"]["rect"], json!([100, 98, 640, 360]));
    assert_eq!(d.element("record.panel.screen.area").unwrap()["label"], "Area 640×360");
    assert!(d.element("record.panel.screen.area.edit").is_some());
    // record.start receives the area: the file is 640 × 360
    d.click("record.panel.record");
    assert!(d.wait_recording(), "{}", d.harness.state().ui.record.error);
    d.run_for(0.4);
    d.click("record.panel.record");
    let q = d.harness.state().session.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height), (640, 360));
    let side =
        std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).map(|e| e.path()).find(|p| p.to_string_lossy().ends_with("Screen.recording.json")).unwrap();
    let side: Value = serde_json::from_slice(&std::fs::read(side).unwrap()).unwrap();
    assert_eq!(side["area"], json!([100, 98, 640, 360]));
    // Edit… redraws; Esc cancels and keeps the area
    d.ok("ui.set", json!({"record": {"overlayDismissed": false}}));
    d.frames(2);
    d.click("record.panel.screen.area.edit");
    assert_eq!(overlay(&mut d)["state"], "drawing");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    let ui = d.ok("ui.inspect", json!({}))["ui"]["record"].clone();
    assert_eq!((ui["drawing"].clone(), ui["screenArea"].clone()), (json!(false), json!([100, 98, 640, 360])));
    // Off closes the border
    d.ok("ui.set", json!({"record": {"screen": ""}}));
    d.frames(2);
    assert!(overlay(&mut d).is_null());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rotate_turns_the_camera_preview_and_lands_on_the_clip() {
    let dir = tmp("rotate");
    let mut d = Driver::new(&dir);
    d.ok("ui.menu.invoke", json!({"id": "window.record"}));
    d.ok("ui.set", json!({"record": {"screen": "", "cameras": [{"device": "synthetic:camera", "quality": "native"}], "mics": []}}));
    wait_label(&mut d, "record.panel.camera.1.preview", |l| l.starts_with("320×180"));
    d.click("record.panel.camera.1.rotate");
    d.click("record.panel.camera.1.rotate.1"); // 0°, 90°, 180°, 270°
    assert_eq!(d.harness.state().ui.record.cameras[0].rotate, 90);
    // the preview stands upright like the clip will
    wait_label(&mut d, "record.panel.camera.1.preview", |l| l.starts_with("180×320"));
    d.click("record.panel.record");
    assert!(d.wait_recording(), "{}", d.harness.state().ui.record.error);
    d.run_for(0.6);
    d.click("record.panel.record");
    let s = &d.harness.state().session;
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height), (180, 320), "the turned camera alone: an upright sequence");
    let m = q.video_tracks[0].items[0].effect("motion").unwrap();
    assert_eq!(m.f64_at("rotation", filmcraft_engine::time::Tick::ZERO), 90.0);
    // the Settings section has the default for new rows
    d.click("record.panel.settings");
    assert!(d.element("record.panel.settings.cameraRotate").is_some());
    std::fs::remove_dir_all(&dir).ok();
}
