//! Headless UI test of the Timeline's marquee: a drag that starts in the blank part under the
//! tracks selects the clips it crosses, and a click there deselects. With every track full of
//! clips (a recording cut by text) there is no empty lane to start a drag in, so the blank part
//! must do.

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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 1700.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
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

    /// `[x, y, w, h]` of the elements whose id starts with `prefix`, by id.
    fn rects(&mut self, prefix: &str) -> Vec<(String, [f64; 4])> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array()
            .unwrap()
            .iter()
            .filter_map(|e| {
                let r = e["rect"].as_array()?;
                Some((e["id"].as_str()?.to_string(), [r[0].as_f64()?, r[1].as_f64()?, r[2].as_f64()?, r[3].as_f64()?]))
            })
            .collect()
    }

    fn selection(&self) -> Vec<u64> {
        self.harness.state().session.state.selection.iter().map(|c| c.0).collect()
    }
}

#[test]
fn a_drag_from_the_blank_part_under_the_tracks_selects_the_clips_it_crosses() {
    let mut d = Driver::demo();
    // the blank part under the tracks, else the one above them (which one there is depends on
    // how many tracks the sequence has)
    let blanks = d.rects("timeline.blank.");
    let below = blanks.iter().find(|b| b.0.ends_with("below")).map(|b| b.1);
    let blank = below.or(blanks.first().map(|b| b.1)).expect("the Timeline has a blank part above or under its tracks at this window size");
    // whole clips only (not `timeline.clip.<id>.empty` and the like)
    let clips: Vec<(u64, [f64; 4])> =
        d.rects("timeline.clip.").into_iter().filter_map(|(id, r)| id.rsplit('.').next().and_then(|n| n.parse().ok()).map(|n| (n, r))).collect();
    assert!(clips.len() >= 2, "{clips:?}");
    assert!(d.selection().is_empty());
    // from the blank part, across every track to the far side of the clips and to their right end
    let top = clips.iter().map(|c| c.1[1]).fold(f64::MAX, f64::min);
    let bottom = clips.iter().map(|c| c.1[1] + c.1[3]).fold(0.0, f64::max);
    let right = clips.iter().map(|c| c.1[0] + c.1[2]).fold(0.0, f64::max);
    // the press is well inside the blank part and the pointer moves in small steps, as a hand
    // does: the drag is recognised while the pointer is still over the blank part
    assert!(blank[3] >= 60.0, "the blank part is tall enough to start a drag in: {blank:?}");
    let (blank_y, far_y) = if below.is_some() { (blank[1] + 40.0, top + 4.0) } else { (blank[1] + blank[3] - 40.0, bottom - 4.0) };
    let from = json!({"x": blank[0] + 4.0, "y": blank_y});
    let to = json!({"x": (right + 6.0).min(blank[0] + blank[2] - 4.0), "y": far_y});
    d.ok("ui.drag", json!({"from": from, "to": to, "steps": 60}));
    d.frames(4);
    let sel = d.selection();
    let seq = d.harness.state().session.active_sequence().unwrap().clone();
    let tracks_hit = seq.all_tracks().filter(|t| t.items.iter().any(|i| sel.contains(&i.id.0))).count();
    assert!(sel.len() >= 2 && tracks_hit >= 2, "the marquee selected clips on several tracks: {sel:?} on {tracks_hit} track(s)");
    for (id, _) in &clips {
        assert!(sel.contains(id), "clip {id} lies inside the marquee: {sel:?}");
    }
    // a click in the blank part deselects
    d.ok("ui.click", json!({"x": blank[0] + 12.0, "y": blank_y}));
    d.frames(3);
    assert!(d.selection().is_empty(), "{:?}", d.selection());
    // a marquee over part of the Timeline takes only what it crosses: the left half
    let mid = (blank[0] + right) / 2.0;
    d.ok("ui.drag", json!({"from": from, "to": {"x": mid, "y": far_y}, "steps": 60}));
    d.frames(4);
    let half = d.selection();
    assert!(!half.is_empty() && half.len() <= sel.len(), "{half:?}");
    for (id, r) in &clips {
        if r[0] > mid + 2.0 {
            assert!(!half.contains(id), "clip {id} starts right of the marquee");
        }
    }
}
