//! Headless UI test of redaction: Clip ▸ Layout ▸ Redact Area ▸ Static / Tracked Mosaic, Blur,
//! Fill… (`layout.menu.redact.*`, which run `redact.start`) enter the Program monitor draw mode, a
//! drag on `program.redact.draw` runs `redact.add` on the top-most video clip with the box in clip
//! pixels (tracking only for the Tracked entries), and Esc leaves the mode without adding anything.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    }

    fn ui_state(&mut self) -> Value {
        self.ok("ui.inspect", json!({}))["ui"].clone()
    }

    fn redactions(&mut self, clip: u64) -> Vec<Value> {
        self.exec("redact.list", json!({"clip": clip}))["redactions"].as_array().unwrap().clone()
    }

    fn jobs(&mut self) -> Vec<Value> {
        self.exec("jobs.list", json!({})).as_array().unwrap().clone()
    }

    fn cancel_jobs(&mut self) {
        for id in self.jobs().iter().filter_map(|j| j["id"].as_u64()) {
            let _ = self.call("engine.execute", json!({"command": "jobs.cancel", "params": {"job": id}}));
        }
    }

    fn status(&mut self) -> String {
        self.ui_state()["status"].as_str().unwrap_or_default().to_string()
    }

    /// Drag a box over the Program picture between two picture fractions.
    fn draw(&mut self, from: (f64, f64), to: (f64, f64)) {
        let pic = self.rect("program.redact.draw");
        assert!(pic[2] > 100.0 && pic[3] > 50.0, "{pic:?}");
        self.ok(
            "ui.drag",
            json!({"from": {"id": "program.redact.draw", "fx": from.0, "fy": from.1}, "to": {"id": "program.redact.draw", "fx": to.0, "fy": to.1}}),
        );
        self.frames(4);
    }
}

/// The id of the first V1 clip of the demo project (the only picture at 2.5 s).
fn first_clip() -> u64 {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.active_sequence().unwrap().video_tracks[0].items[0].id.0
}

const MODES: [(&str, &str); 6] = [
    ("static.mosaic", "Static Mosaic…"),
    ("static.blur", "Static Blur…"),
    ("static.fill", "Static Fill…"),
    ("tracked.mosaic", "Tracked Mosaic…"),
    ("tracked.blur", "Tracked Blur…"),
    ("tracked.fill", "Tracked Fill…"),
];

#[test]
fn clip_layout_has_a_redact_area_submenu_with_static_and_tracked_entries() {
    let mut d = Driver::demo();
    let menu = d.ok("ui.menu.list", json!({}));
    let items = menu.as_array().unwrap();
    for (mode, label) in MODES {
        let id = format!("layout.menu.redact.{mode}");
        let entry = items.iter().find(|m| m["id"] == id.as_str()).unwrap_or_else(|| panic!("{id} in the menus"));
        assert_eq!(entry["path"], json!(["Clip", "Layout", "Redact Area"]), "{entry}");
        assert_eq!(entry["label"], label);
        assert_eq!(entry["enabled"], true);
    }
    assert!(!items.iter().any(|m| m["id"] == "layout.menu.redact" || m["id"] == "redact.start"), "no single Redact Area… entry any more");

    // each entry names its choice in the draw-mode status line; Esc leaves it
    for (mode, _) in MODES {
        d.ok("ui.menu.invoke", json!({"id": format!("layout.menu.redact.{mode}")}));
        d.frames(3);
        assert_eq!(d.ids("program.redact.draw"), ["program.redact.draw"]);
        let (kind, style) = mode.split_once('.').unwrap();
        assert_eq!(d.status(), format!("Drag a box over what to hide · {kind} {style} (Esc cancels)"));
        let ui = d.ui_state();
        assert_eq!(ui["redact_track"], kind == "tracked", "{mode}");
        d.ok("ui.key", json!({"key": "Escape"}));
        d.frames(3);
        assert!(d.ids("program.redact.draw").is_empty(), "Esc cancels {mode}");
        assert_eq!(d.ui_state()["redact_track"], false);
    }
    // the old id is an alias of Static Mosaic…, and `redact.start` is static unless asked
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.redact"}));
    d.frames(3);
    assert_eq!(d.status(), "Drag a box over what to hide · static mosaic (Esc cancels)");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    let r = d.ok("ui.menu.invoke", json!({"id": "redact.start", "params": {"style": "blur", "track": true}}));
    assert_eq!((r["style"].clone(), r["track"].clone()), (json!("blur"), json!(true)), "{r}");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    // hostile params are refused without entering the mode
    for bad in [json!({"track": "yes"}), json!({"style": "pixelate"})] {
        let v = d.call("ui.menu.invoke", json!({"id": "redact.start", "params": bad}));
        assert_ne!(v["ok"], json!(true), "{bad}: {v}");
    }
    let v = d.call("ui.menu.invoke", json!({"id": "layout.menu.redact.wobbly.mosaic"}));
    assert_ne!(v["ok"], json!(true), "{v}");
    d.frames(2);
    assert!(d.ids("program.redact.draw").is_empty());
    assert!(d.jobs().is_empty(), "nothing was tracked");

    // the right-click box menu has the same submenu, with the static / tracked hint
    d.exec("playhead.set", json!({"seconds": 2.5}));
    d.ok("ui.click", json!({"id": "program.picture", "fx": 0.5, "fy": 0.5}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "program.layout.box", "button": "right"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "layout.menu.redact"}));
    d.frames(3);
    let ids = d.ids("layout.menu.redact.");
    for (mode, _) in MODES {
        assert!(ids.contains(&format!("layout.menu.redact.{mode}")), "{ids:?}");
    }
    d.ok("ui.click", json!({"id": "layout.menu.redact.static.blur"}));
    d.frames(3);
    assert_eq!(d.status(), "Drag a box over what to hide · static blur (Esc cancels)");
    assert_eq!(d.ids("program.redact.draw"), ["program.redact.draw"]);
}

#[test]
fn the_static_entry_adds_a_redaction_without_tracking() {
    let mut d = Driver::demo();
    d.exec("playhead.set", json!({"seconds": 2.5}));
    let clip = first_clip();
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.redact.static.fill"}));
    d.frames(3);
    d.draw((0.25, 0.25), (0.5, 0.5));
    let r = d.redactions(clip);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0]["name"], "Redaction 1");
    assert_eq!(r[0]["style"], "fill");
    assert_eq!(r[0]["tracked"], false);
    assert_eq!(r[0]["tracking"], false);
    assert!(d.jobs().is_empty(), "a static box starts no job: {:?}", d.jobs());
    assert_eq!(d.status(), "Redaction 1 added (static)");
    assert!(d.ids("status.job.").is_empty(), "no job in the status bar");
    // one undo step takes it back
    d.exec("edit.undo", json!({}));
    assert!(d.redactions(clip).is_empty());
}

#[test]
fn the_tracked_entry_adds_a_redaction_and_starts_tracking() {
    let mut d = Driver::demo();
    // 2.5 s: the first V1 clip is the only picture (the V2 overlay starts at 6 s)
    d.exec("playhead.set", json!({"seconds": 2.5}));
    let probe = first_clip();

    // Esc leaves the draw mode
    assert!(d.ids("program.redact.draw").is_empty(), "no draw mode yet");
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.redact.tracked.mosaic"}));
    d.frames(3);
    assert_eq!(d.ids("program.redact.draw"), ["program.redact.draw"]);
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.ids("program.redact.draw").is_empty(), "Esc cancels");
    assert!(d.redactions(probe).is_empty());

    // draw a box over the second quarter of the picture
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.redact.tracked.mosaic"}));
    d.frames(3);
    let ui = d.ui_state();
    assert_eq!(ui["redact_draw"], true);
    assert_eq!(ui["status"], "Drag a box over what to hide · tracked mosaic (Esc cancels)");
    d.draw((0.25, 0.25), (0.5, 0.5));
    let r = d.redactions(probe);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0]["name"], "Redaction 1");
    assert_eq!(r[0]["style"], "mosaic");
    // the box in clip pixels of the 1920×1080 source (Motion at its defaults)
    let rect: Vec<f64> = r[0]["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    for (got, want) in rect.iter().zip([480.0, 270.0, 480.0, 270.0]) {
        assert!((got - want).abs() < 12.0, "{rect:?}");
    }
    assert!(d.ids("program.redact.draw").is_empty(), "the mode ends after one box");
    assert_eq!(d.ui_state()["redact_draw"], false);
    // the selected mask is the redaction's (its on-monitor handles show)
    assert!(!d.ids("program.mask.body").is_empty());
    // tracking runs as jobs, and the status bar names the running one
    let jobs = d.jobs();
    assert!(!jobs.is_empty(), "a tracking job started: {jobs:?}");
    let running: Vec<&Value> = jobs.iter().filter(|j| j["finished"] == false).collect();
    if let Some(j) = running.last() {
        assert_eq!(j["label"], "Track Redaction 1 (forward)");
        let text = d.ok("ui.elements", json!({"prefix": "status.job.text"}));
        let label = text[0]["label"].as_str().unwrap_or_default().to_string();
        assert!(label.starts_with("Track Redaction 1 (forward)… "), "{label}");
        assert!(!label.contains("Exporting"), "{label}");
    }
    // stop them so the test does not decode the whole clip
    d.cancel_jobs();
}

#[test]
fn the_status_bar_names_the_running_job_with_the_eta_jobs_list_reports() {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use filmcraft_engine::Job;
    let mut d = Driver::demo();
    // a job at 25 %, doing 50 units a second: 750 left, 15 s (readings from the near future, because
    // the app reads the real clock, which is then behind them)
    let job = |id: u64, label: &str| {
        let job = Job { id, label: label.into(), progress: Default::default(), result: Default::default() };
        job.progress.total.store(1000, Ordering::Relaxed);
        let t0 = web_time::Instant::now() + Duration::from_secs(60);
        for i in 0..=50u64 {
            job.progress.done.store(i * 5, Ordering::Relaxed);
            job.progress.eta_at(t0 + Duration::from_millis(i * 100));
        }
        job
    };
    let text = |d: &mut Driver| d.ok("ui.elements", json!({"prefix": "status.job.text"}))[0]["label"].as_str().unwrap_or_default().to_string();
    // an export is "Exporting"
    let export = job(900, "Export clip.mp4");
    d.harness.state_mut().session.jobs.push(export.clone());
    d.frames(3);
    assert_eq!(text(&mut d), "Exporting… 25% · 15 s left");
    export.progress.finished.store(true, Ordering::Relaxed);
    // any other job by its own label, with the time left `jobs.list` gives
    d.harness.state_mut().session.jobs.push(job(901, "Track Redaction 1 (forward)"));
    d.frames(3);
    assert_eq!(text(&mut d), "Track Redaction 1 (forward)… 25% · 15 s left");
    let listed = d.jobs().into_iter().find(|j| j["id"] == 901).unwrap();
    assert_eq!(listed["label"], "Track Redaction 1 (forward)");
    assert!((listed["etaSeconds"].as_f64().unwrap() - 15.0).abs() < 0.5, "{listed}");
    for (label, verb) in [
        ("Quick Export a.mp4", "Exporting"),
        ("Queue: Sequence 01 → a.mp4", "Exporting"),
        ("Rendering 2 preview segments", "Rendering 2 preview segments"),
        ("Transcribing 1 clip", "Transcribing 1 clip"),
        ("Exported", "Exported"),
    ] {
        assert_eq!(filmcraft_ui_egui::job_verb(label), verb);
    }
}
