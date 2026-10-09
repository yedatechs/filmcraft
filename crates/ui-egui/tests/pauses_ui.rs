//! Headless UI test of the Text panel's Remove Pauses dialog: the toolbar button opens it, the
//! threshold is set through `ui.set {menuDialog}`, the live count follows `transcript.pauses`,
//! Apply crosses the pauses out (one undo step) and Apply is disabled when nothing would change.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the dialog offscreen with wgpu and write
//! `pauses-*.png` there; without it no GPU is needed.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
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
        let mut d = Driver { harness, tx, snapshots };
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

    fn live_words(&mut self) -> Vec<String> {
        let r = self.exec("transcript.inspect", json!({}));
        r["words"].as_array().unwrap().iter().filter_map(|w| w["text"].as_str().map(str::to_string)).collect()
    }

    fn cuts(&mut self) -> usize {
        self.exec("transcript.cuts", json!({}))["cuts"].as_array().unwrap().len()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("pauses-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
    fn label(&mut self, id: &str) -> String {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().find(|e| e["id"] == id).and_then(|e| e["label"].as_str()).unwrap_or_default().to_string()
    }

    fn duration(&mut self) -> i64 {
        self.harness.state().session.active_sequence().unwrap().duration().0
    }
}

/// The first A1 clip's media with four words and gaps of 0.3 s, 0.8 s and 2.0 s between them.
fn gappy(d: &mut Driver) -> u64 {
    let mut probe = Session::default();
    probe.execute("file.openDemoProject", json!({})).unwrap();
    let a = probe.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64).round() as i64;
    let words: Vec<Value> = [("One", 0.2, 0.5), ("two", 0.8, 1.1), ("three", 1.9, 2.2), ("four.", 4.2, 4.5)]
        .iter()
        .map(|(w, s, e)| json!({"text": w, "start": tk(*s), "end": tk(*e), "speaker": 0}))
        .collect();
    d.exec("transcript.set", json!({"item": a.item.0, "transcript": {"language": "en", "words": words}}));
    d.frames(3);
    a.item.0
}

#[test]
fn remove_pauses_dialog_with_threshold_and_live_count() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.click("text.tab.Transcript");
    gappy(&mut d);
    assert_eq!(d.ids("text.transcript.word.").len(), 4);
    assert!(d.ids("text.pauses.").is_empty(), "the dialog starts closed");
    let before = d.duration();

    // the toolbar button opens the dialog with the remembered defaults (1.0 s → only the 2.0 s gap)
    d.click("text.transcript.removePauses");
    let ids = d.ids("text.pauses.");
    for id in ["text.pauses.min", "text.pauses.keep", "text.pauses.count", "text.pauses.apply", "text.pauses.cancel"] {
        assert!(ids.iter().any(|i| i == id), "{id} missing: {ids:?}");
    }
    assert!(d.label("text.pauses.count").starts_with("1 pause,"), "{}", d.label("text.pauses.count"));
    assert_eq!(d.duration(), before, "opening the dialog changes nothing");
    d.snapshot("dialog");

    // half a second catches the 0.8 s and 2.0 s gaps
    d.ok("ui.set", json!({"menuDialog": {"pauseMin": 0.5, "pauseKeep": 0.15}}));
    d.frames(2);
    let line = d.label("text.pauses.count");
    assert!(line.starts_with("2 pauses,"), "{line}");
    let preview = d.exec("transcript.pauses", json!({"minSeconds": 0.5, "keepSeconds": 0.15}));
    assert_eq!(line, format!("2 pauses, {:.1} s", preview["seconds"].as_f64().unwrap()));
    {
        let ui = &d.harness.state().ui;
        assert_eq!((ui.transcript_pause_min, ui.transcript_pause_keep), (0.5, 0.15), "remembered in UiState");
    }

    d.click("text.pauses.apply");
    assert!(d.ids("text.pauses.").is_empty(), "Apply closes the dialog");
    assert_eq!(d.cuts(), 2);
    assert!(!d.ids("text.transcript.cut.").is_empty(), "the pauses show crossed out");
    assert_eq!(d.live_words(), ["One", "two", "three", "four."]);
    assert!(d.duration() < before);

    // one undo step restores both
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.cuts(), 0);
    assert_eq!(d.duration(), before);

    // from the menu path (no thresholds) the same dialog opens, still at 0.5 s
    let r = d.ok("ui.menu.invoke", json!({"id": "transcript.removePauses"}));
    assert_eq!(r, json!({"dialog": "text.pauses"}));
    d.frames(2);
    assert!(d.label("text.pauses.count").starts_with("2 pauses,"));

    // nothing is longer than 3 s: count 0 and Apply does nothing
    d.ok("ui.set", json!({"menuDialog": {"pauseMin": 3.0}}));
    d.frames(2);
    assert!(d.label("text.pauses.count").starts_with("0 pauses,"), "{}", d.label("text.pauses.count"));
    assert_eq!(d.label("text.pauses.apply"), "Apply (no pauses)");
    d.click("text.pauses.apply");
    assert_eq!(d.cuts(), 0);
    assert!(!d.ids("text.pauses.").is_empty(), "a disabled Apply leaves the dialog open");

    // keep is clamped to the minimum and hostile values never panic
    d.ok("ui.set", json!({"menuDialog": {"pauseMin": 0.2, "pauseKeep": 5.0}}));
    d.frames(2);
    {
        let ui = &d.harness.state().ui;
        assert_eq!((ui.transcript_pause_min, ui.transcript_pause_keep), (0.2, 0.2));
    }
    d.ok("ui.set", json!({"menuDialog": {"pauseMin": -4.0, "pauseKeep": -1.0}}));
    d.frames(2);
    {
        let ui = &d.harness.state().ui;
        assert_eq!((ui.transcript_pause_min, ui.transcript_pause_keep), (0.1, 0.0));
    }
    d.click("text.pauses.cancel");
    assert!(d.ids("text.pauses.").is_empty(), "Cancel closes the dialog");
    assert_eq!(d.cuts(), 0);
}
