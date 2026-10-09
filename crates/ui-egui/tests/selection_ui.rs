//! Headless UI test of selecting words in the Text panel's Transcript tab with the mouse: click,
//! Shift+click to extend, and dragging across words; then Cmd+Backspace crosses out exactly the
//! highlighted words (the Descript habit of dragging over text must select, not just click the
//! first word).
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu.

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
                img.save(dir.join(format!("selection-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

/// The first A1 clip's media (1.0–6.0 s) with one line said three times: 0.2 s, 2.0 s and 3.5 s
/// into the clip (media 1.2, 3.0 and 4.5 s), words 0.2 s long every 0.25 s.
fn load(d: &mut Driver) {
    let mut probe = Session::default();
    probe.execute("file.openDemoProject", json!({})).unwrap();
    let a = probe.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64).round() as i64;
    let line = ["yo", "what", "a", "what", "a", "time", "to", "be", "alive"];
    let words: Vec<Value> = line
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let s = 0.2 + i as f64 * 0.4;
            json!({"text": w, "start": tk(s), "end": tk(s + 0.3), "speaker": 0})
        })
        .collect();
    d.exec("transcript.set", json!({"item": a.item.0, "transcript": {"language": "en", "words": words}}));
    d.frames(3);
}

#[test]
fn shift_click_extends_and_cmd_backspace_crosses_out_the_selection() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.click("text.tab.Transcript");
    load(&mut d);
    d.click("text.transcript.word.1");
    d.ok("ui.click", json!({"id": "text.transcript.word.2", "modifiers": {"shift": true}}));
    d.frames(2);
    let st = d.ok("ui.inspect", json!({}));
    assert_eq!(st["ui"]["transcript_sel"], json!([1, 2]), "{}", st["ui"]["transcript_sel"]);
    d.ok("ui.key", json!({"key": "Cmd+Backspace"}));
    d.frames(3);
    assert_eq!(d.live_words(), ["yo", "what", "a", "time", "to", "be", "alive"]);
    assert_eq!(d.cuts(), 1);
}

#[test]
fn dragging_across_words_selects_them() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.click("text.tab.Transcript");
    load(&mut d);
    d.ok("ui.drag", json!({"from": {"id": "text.transcript.word.1"}, "to": {"id": "text.transcript.word.2"}}));
    d.frames(2);
    let st = d.ok("ui.inspect", json!({}));
    assert_eq!(st["ui"]["transcript_sel"], json!([1, 2]), "{}", st["ui"]["transcript_sel"]);
    d.snapshot("drag-selected");
    d.ok("ui.key", json!({"key": "Cmd+Backspace"}));
    d.frames(3);
    assert_eq!(d.live_words(), ["yo", "what", "a", "time", "to", "be", "alive"]);
    // dragging backwards selects the same range
    d.exec("edit.undo", json!({}));
    d.frames(2);
    d.ok("ui.drag", json!({"from": {"id": "text.transcript.word.4"}, "to": {"id": "text.transcript.word.3"}}));
    d.frames(2);
    let st = d.ok("ui.inspect", json!({}));
    assert_eq!(st["ui"]["transcript_sel"], json!([4, 3]), "{}", st["ui"]["transcript_sel"]);
}

#[test]
fn selection_after_a_removed_pause_crosses_out_the_right_words() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.click("text.tab.Transcript");
    // "yo", a 2.4 s pause, then "what a what a time"
    let mut probe = Session::default();
    probe.execute("file.openDemoProject", json!({})).unwrap();
    let a = probe.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64).round() as i64;
    let mut words: Vec<Value> = vec![json!({"text": "yo", "start": tk(0.1), "end": tk(0.4), "speaker": 0})];
    for (i, w) in ["what", "a", "what", "a", "time"].iter().enumerate() {
        let s = 2.8 + i as f64 * 0.4;
        words.push(json!({"text": w, "start": tk(s), "end": tk(s + 0.3), "speaker": 0}));
    }
    d.exec("transcript.set", json!({"item": a.item.0, "transcript": {"language": "en", "words": words}}));
    d.frames(3);
    d.exec("transcript.removePauses", json!({"minSeconds": 1.0, "keepSeconds": 0.15}));
    d.frames(3);
    assert_eq!(d.cuts(), 1, "the pause is a cut span");
    assert_eq!(d.live_words(), ["yo", "what", "a", "what", "a", "time"]);
    d.ok("ui.drag", json!({"from": {"id": "text.transcript.word.1"}, "to": {"id": "text.transcript.word.2"}}));
    d.frames(2);
    let st = d.ok("ui.inspect", json!({}));
    assert_eq!(st["ui"]["transcript_sel"], json!([1, 2]), "{}", st["ui"]["transcript_sel"]);
    d.exec("playhead.set", json!({"seconds": 0.0}));
    d.ok("ui.key", json!({"key": "Cmd+Backspace"}));
    d.frames(3);
    assert_eq!(d.live_words(), ["yo", "what", "a", "time"], "the selected pair is crossed out");
    let r = d.exec("transcript.cuts", json!({}));
    let texts: Vec<String> = r["cuts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["words"].as_array().unwrap().iter().filter_map(|w| w["text"].as_str()).collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(texts, ["", "what a"], "{r}");
    let st = d.ok("ui.inspect", json!({}));
    assert_eq!(st["playhead"].as_i64(), Some(0), "the playhead stays where it was: {}", st["playhead"]);
    // with nothing selected, ⌘⌫ beside the two spans brings back the words, not the pause
    d.ok("ui.key", json!({"key": "Cmd+Backspace"}));
    d.frames(3);
    assert_eq!(d.live_words(), ["yo", "what", "a", "what", "a", "time"]);
    assert_eq!(d.cuts(), 1, "the pause stays removed");
    let st = d.ok("ui.inspect", json!({}));
    assert_eq!(st["ui"]["transcript_sel"], json!([1, 2]), "the restored words are selected");
    // and again crosses the same words out
    d.ok("ui.key", json!({"key": "Cmd+Backspace"}));
    d.frames(3);
    assert_eq!(d.live_words(), ["yo", "what", "a", "time"]);
}

#[test]
fn dragging_in_a_long_wrapped_paragraph_selects_only_the_words_under_the_pointer() {
    // a paragraph long enough to wrap several lines: a word that lands at a line break used to get
    // a label box spanning both lines, so a drag over its neighbours selected up to that word
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.click("text.tab.Transcript");
    let mut probe = Session::default();
    probe.execute("file.openDemoProject", json!({})).unwrap();
    let a = probe.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64).round() as i64;
    let line = "what a what a time to be alive what a time to be a builder these last three weeks have been crazy anthropic just reset fable usage limits that so now we will get so so now we have three days so now you have three more days to use fable unless they extend fable usage again";
    let words: Vec<Value> = line
        .split(' ')
        .enumerate()
        .map(|(i, w)| {
            let s = 0.1 + i as f64 * 0.08;
            json!({"text": w, "start": tk(s), "end": tk(s + 0.06), "speaker": 0})
        })
        .collect();
    let n = words.len();
    d.exec("transcript.set", json!({"item": a.item.0, "transcript": {"language": "en", "words": words}}));
    d.frames(3);
    assert_eq!(d.ids("text.transcript.word.").len(), n);
    for (from, to) in [(0usize, 1usize), (1, 2), (5, 6), (12, 14)] {
        d.ok("ui.drag", json!({"from": {"id": format!("text.transcript.word.{from}")}, "to": {"id": format!("text.transcript.word.{to}")}}));
        d.frames(2);
        let st = d.ok("ui.inspect", json!({}));
        assert_eq!(st["ui"]["transcript_sel"], json!([from, to]), "drag {from}→{to}: {}", st["ui"]["transcript_sel"]);
    }
    // every word's box is a single line tall
    let els = d.ok("ui.elements", json!({"prefix": "text.transcript.word."}));
    let heights: Vec<f64> = els.as_array().unwrap().iter().filter_map(|e| e["rect"][3].as_f64()).collect();
    let (min, max) = heights.iter().fold((f64::MAX, 0.0f64), |(lo, hi), h| (lo.min(*h), hi.max(*h)));
    assert!(max < min * 1.5, "a word box spans more than one line: {min}..{max}");
    d.snapshot("wrapped-drag");
}
