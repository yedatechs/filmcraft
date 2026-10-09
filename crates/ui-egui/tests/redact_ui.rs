//! Headless UI test of tracked redaction: Clip ▸ Layout ▸ Redact Area… (`redact.start`) enters the
//! Program monitor draw mode, a drag on `program.redact.draw` runs `redact.add` on the top-most
//! video clip with the box in clip pixels, and Esc leaves the mode without adding anything.

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
}

#[test]
fn drag_a_box_on_the_program_picture_adds_a_tracked_redaction() {
    let mut d = Driver::demo();
    // 2.5 s: the first V1 clip is the only picture (the V2 overlay starts at 6 s)
    d.exec("playhead.set", json!({"seconds": 2.5}));
    let probe = {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.active_sequence().unwrap().video_tracks[0].items[0].id.0
    };
    // the entry is in Clip ▸ Layout
    let menu = d.ok("ui.menu.list", json!({}));
    let entry = menu.as_array().unwrap().iter().find(|m| m["id"] == "redact.start").expect("redact.start in the menus").clone();
    assert_eq!(entry["path"], json!(["Clip", "Layout"]));
    assert_eq!(entry["label"], "Redact Area…");

    // Esc leaves the draw mode
    assert!(d.ids("program.redact.draw").is_empty(), "no draw mode yet");
    d.ok("ui.menu.invoke", json!({"id": "redact.start"}));
    d.frames(3);
    assert_eq!(d.ids("program.redact.draw"), ["program.redact.draw"]);
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.ids("program.redact.draw").is_empty(), "Esc cancels");
    assert!(d.redactions(probe).is_empty());

    // draw a box over the second quarter of the picture
    d.ok("ui.menu.invoke", json!({"id": "redact.start"}));
    d.frames(3);
    let ui = d.ui_state();
    assert_eq!(ui["redact_draw"], true);
    assert!(ui["status"].as_str().is_some_and(|s| s.contains("Drag a box")), "{}", ui["status"]);
    let pic = d.rect("program.redact.draw");
    assert!(pic[2] > 100.0 && pic[3] > 50.0, "{pic:?}");
    d.ok("ui.drag", json!({"from": {"id": "program.redact.draw", "fx": 0.25, "fy": 0.25}, "to": {"id": "program.redact.draw", "fx": 0.5, "fy": 0.5}}));
    d.frames(4);
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
    // tracking runs as jobs; stop them so the test does not decode the whole clip
    let jobs = d.exec("jobs.list", json!({}));
    let ids: Vec<u64> = jobs.as_array().unwrap().iter().filter_map(|j| j["id"].as_u64()).collect();
    assert!(!ids.is_empty(), "a tracking job started: {jobs}");
    for id in ids {
        let _ = d.call("engine.execute", json!({"command": "jobs.cancel", "params": {"job": id}}));
    }
}
