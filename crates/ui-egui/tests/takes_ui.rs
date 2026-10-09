//! Headless UI test of the Text panel's crossed-out text and take groups: Detect Takes, the
//! crossed-out spans (click restores), the take chip (click cycles), the Takes list, Restore All
//! Cuts, and the empty state when everything is crossed out.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `takes-*.png` there; without it no GPU is needed.

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

    fn groups(&mut self) -> Vec<Value> {
        self.exec("takes.list", json!({}))["groups"].as_array().unwrap().clone()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("takes-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

/// The first A1 clip's media (1.0–6.0 s) with one line said three times: 0.2 s, 2.0 s and 3.5 s
/// into the clip (media 1.2, 3.0 and 4.5 s), words 0.2 s long every 0.25 s.
fn three_passes(d: &mut Driver) -> u64 {
    let mut probe = Session::default();
    probe.execute("file.openDemoProject", json!({})).unwrap();
    let a = probe.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64).round() as i64;
    let mut words: Vec<Value> = Vec::new();
    for (t0, line) in [
        (0.2, vec!["We", "start", "the", "show", "here."]),
        (2.0, vec!["We", "start", "the", "show", "here."]),
        (3.5, vec!["We", "start", "the", "show", "here,", "friends."]),
    ] {
        for (i, w) in line.iter().enumerate() {
            let s = t0 + i as f64 * 0.25;
            words.push(json!({"text": w, "start": tk(s), "end": tk(s + 0.2), "speaker": 0}));
        }
    }
    d.exec("transcript.set", json!({"item": a.item.0, "transcript": {"language": "en", "words": words}}));
    d.frames(3);
    a.item.0
}

#[test]
fn detect_cross_out_restore_and_cycle_in_the_text_panel() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.click("text.tab.Transcript");
    three_passes(&mut d);
    assert_eq!(d.ids("text.transcript.word.").len(), 16);
    assert!(d.ids("text.transcript.cut.").is_empty(), "nothing is crossed out yet");
    assert!(d.ids("text.transcript.take.").is_empty(), "no take groups yet");

    // Detect Takes from the toolbar: the last take stays, the other two show crossed out
    d.click("text.transcript.detectTakes");
    let groups = d.groups();
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0]["active"], json!(2));
    let gid = groups[0]["id"].as_u64().unwrap();
    assert_eq!(d.live_words(), ["We", "start", "the", "show", "here,", "friends."]);
    let chips = d.ids("text.transcript.take.");
    assert_eq!(chips, vec![format!("text.transcript.take.{gid}")]);
    let cut_ids = d.ids("text.transcript.cut.");
    assert!(cut_ids.iter().any(|i| i == "text.transcript.cut.0"), "{cut_ids:?}");
    let cut_words: Vec<&String> = cut_ids.iter().filter(|i| i.matches('.').count() == 4).collect();
    assert_eq!(cut_words.len(), 10, "the two crossed-out passes show word by word: {cut_ids:?}");
    d.snapshot("detected");

    // the chip cycles takes
    d.click(&format!("text.transcript.take.{gid}"));
    assert_eq!(d.groups()[0]["active"], json!(0), "next after the last take wraps to the first");
    assert_eq!(d.live_words(), ["We", "start", "the", "show", "here."]);
    assert_eq!(d.ids("text.transcript.word.").len(), 5);

    // clicking a crossed-out span restores it: more live words, fewer cuts
    let before = d.cuts();
    assert!(before >= 1);
    d.click("text.transcript.cut.0");
    assert!(d.live_words().len() > 5, "{:?}", d.live_words());
    assert!(d.cuts() < before || d.groups()[0]["active"].is_null(), "restoring made a second take live");

    // the Takes list
    assert!(d.ids("text.takes.group.").is_empty(), "list hidden by default");
    d.click("text.transcript.takes");
    assert_eq!(d.ids("text.takes.group."), vec![format!("text.takes.group.{gid}")]);
    d.click(&format!("text.takes.group.{gid}"));
    let take_ids = d.ids("text.takes.take.");
    assert!(take_ids.iter().any(|i| i == &format!("text.takes.take.{gid}.1.select")), "{take_ids:?}");
    d.snapshot("list");
    d.click(&format!("text.takes.take.{gid}.1.select"));
    assert_eq!(d.groups()[0]["active"], json!(1));
    assert_eq!(d.live_words(), ["We", "start", "the", "show", "here."]);

    // Restore All Cuts brings everything back; then an extract shows struck through and restores
    d.click("text.transcript.restoreAll");
    assert_eq!(d.cuts(), 0);
    assert_eq!(d.ids("text.transcript.word.").len(), 16);
    assert!(d.ids("text.transcript.restoreAll").len() == 1);
    d.exec("transcript.extract", json!({"from": 5, "to": 9}));
    d.frames(3);
    assert_eq!(d.ids("text.transcript.word.").len(), 11);
    let cut_ids = d.ids("text.transcript.cut.");
    assert_eq!(cut_ids.iter().filter(|i| i.matches('.').count() == 4).count(), 5, "{cut_ids:?}");
    d.click("text.transcript.cut.0");
    assert_eq!(d.ids("text.transcript.word.").len(), 16);
    assert_eq!(d.cuts(), 0);
}

#[test]
fn everything_crossed_out_still_shows_the_text() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.click("text.tab.Transcript");
    three_passes(&mut d);
    d.exec("takes.detect", json!({}));
    d.frames(3);
    let gid = d.groups()[0]["id"].as_u64().unwrap();
    d.exec("takes.cross", json!({"group": gid, "take": 2}));
    d.frames(3);
    assert!(d.live_words().is_empty());
    assert!(d.ids("text.transcript.generate").is_empty(), "the Transcribe placeholder must not hide crossed-out text");
    let cut_ids = d.ids("text.transcript.cut.");
    assert!(!cut_ids.is_empty(), "{cut_ids:?}");
    assert_eq!(d.ids("text.transcript.take."), vec![format!("text.transcript.take.{gid}")], "the chip stays with the crossed-out group");
    d.snapshot("all-crossed-out");
    d.click("text.transcript.cut.0");
    assert!(!d.live_words().is_empty(), "clicking the crossed-out text brings it back");
}

#[test]
fn cmd_backspace_crosses_out_and_restores_like_descript() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.click("text.tab.Transcript");
    three_passes(&mut d);
    d.ok("ui.set", json!({"focused": "Text"}));
    // select "start the" (words 1–2) and cross them out
    d.click("text.transcript.word.1");
    d.ok("ui.key", json!({"key": "Shift+Right"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Cmd+Backspace"}));
    d.frames(3);
    assert_eq!(d.cuts(), 1);
    assert_eq!(d.ids("text.transcript.word.").len(), 14);
    assert_eq!(&d.live_words()[..4], ["We", "show", "here.", "We"]);
    // nothing selected, the playhead sits where the words were: ⌘⌫ brings them back, selected
    d.ok("ui.key", json!({"key": "Cmd+Backspace"}));
    d.frames(3);
    assert_eq!(d.cuts(), 0);
    assert_eq!(d.ids("text.transcript.word.").len(), 16);
    assert_eq!(&d.live_words()[..4], ["We", "start", "the", "show"]);
    // and again crosses the same words out
    d.ok("ui.key", json!({"key": "Cmd+Backspace"}));
    d.frames(3);
    assert_eq!(d.cuts(), 1);
    assert_eq!(&d.live_words()[..3], ["We", "show", "here."]);
    d.snapshot("cmd-backspace");
}
