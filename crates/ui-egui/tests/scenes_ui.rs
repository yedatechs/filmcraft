//! Headless UI test of scenes in the Text panel: a scene chip per Transcript paragraph, the chip
//! menu (Create Default Scenes, pick a scene → `scenes.list` has the span), and the Scenes…
//! dialog (from the chip menu and Sequence ▸ Scenes…) with its `text.scenes.*` ids.

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
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
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

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
}

/// Two paragraphs on the first A1 clip's media (1.0–6.0 s): three words 0.2 s into the clip and
/// three words 2.6 s into it (a pause longer than the paragraph gap between them).
fn two_paragraphs(d: &mut Driver) {
    let mut probe = Session::default();
    probe.execute("file.openDemoProject", json!({})).unwrap();
    let a = probe.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64).round() as i64;
    let mut words: Vec<Value> = Vec::new();
    for (t0, line) in [(0.2, ["We", "start", "here."]), (2.6, ["Now", "the", "screen."])] {
        for (i, w) in line.iter().enumerate() {
            let s = t0 + i as f64 * 0.25;
            words.push(json!({"text": w, "start": tk(s), "end": tk(s + 0.2), "speaker": 0}));
        }
    }
    d.exec("transcript.set", json!({"item": a.item.0, "transcript": {"language": "en", "words": words}}));
    d.frames(3);
}

/// Exact chip ids (`text.scene.{n}`), not their menu items.
fn chips(d: &mut Driver) -> Vec<String> {
    d.ids("text.scene.").into_iter().filter(|id| id.matches('.').count() == 2).collect()
}

#[test]
fn chips_menu_and_dialog() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.click("text.tab.Transcript");
    two_paragraphs(&mut d);
    assert_eq!(chips(&mut d), ["text.scene.0", "text.scene.1"], "one chip per paragraph");

    // no scenes yet: the chip menu offers the defaults
    d.click("text.scene.1");
    assert!(d.ids("text.scene.1.defaults").len() == 1, "{:?}", d.ids("text.scene.1"));
    d.click("text.scene.1.defaults");
    let l = d.exec("scenes.list", json!({}));
    assert_eq!(l["scenes"].as_array().unwrap().len(), 4, "{l}");

    // pick Face (index 1) for the second paragraph
    d.click("text.scene.1");
    assert_eq!(d.ids("text.scene.1.pick.").len(), 4);
    d.click("text.scene.1.pick.1");
    let l = d.exec("scenes.list", json!({}));
    let spans = l["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1, "{l}");
    assert_eq!(spans[0]["name"], json!("Face"));
    assert_eq!(spans[0]["from"], json!(3));
    assert_eq!(spans[0]["to"], json!(5));

    // the chip menu's Scenes… opens the dialog
    d.click("text.scene.0");
    d.click("text.scene.0.manage");
    let ids = d.ids("text.scenes.");
    for want in [
        "text.scenes.list.0",
        "text.scenes.list.3",
        "text.scenes.add",
        "text.scenes.duplicate",
        "text.scenes.remove",
        "text.scenes.defaults",
        "text.scenes.swap",
        "text.scenes.close",
        "text.scenes.slot.0.place",
        "text.scenes.slot.0.size",
        "text.scenes.slot.1.shape",
        "text.scenes.slot.1.hidden",
    ] {
        assert!(ids.iter().any(|i| i == want), "{want} missing from {ids:?}");
    }
    // select Face through ui.set, then Swap A and B: one scenes.update
    d.ok("ui.set", json!({"menuDialog": {"scenesSelected": 1}}));
    d.frames(2);
    let before = d.exec("scenes.list", json!({}))["scenes"][1]["slots"].clone();
    d.click("text.scenes.swap");
    let after = d.exec("scenes.list", json!({}))["scenes"][1]["slots"].clone();
    assert_eq!(after[0]["item"], before[1]["item"], "{before} {after}");
    d.click("text.scenes.close");
    assert!(d.ids("text.scenes.").is_empty());

    // Sequence ▸ Scenes…
    let menu = d.ok("ui.menu.list", json!({}));
    let entry = menu.as_array().unwrap().iter().find(|m| m["id"] == "scenes.dialog").expect("scenes.dialog in the menus").clone();
    assert_eq!(entry["path"], json!(["Sequence"]));
    d.ok("ui.menu.invoke", json!({"id": "scenes.dialog"}));
    d.frames(3);
    assert!(d.ids("text.scenes.close").len() == 1);
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.ids("text.scenes.").is_empty());
}
