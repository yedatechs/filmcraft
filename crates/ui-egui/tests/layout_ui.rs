//! Headless UI test of clip layouts in the Program monitor: click the picture to select the top
//! clip (a second click cycles), the box and its handles, the right-click Layout menu, a drag of
//! the box (one undo step), the Effect Controls shape buttons and Clip ▸ Layout; Alt-drag pans and
//! Alt-scroll / Alt-corner drag zoom the picture inside a shape; Apply to All Clips of This Source
//! copies a layout to the other clips of the same media on the track, and the status bar points
//! to it after a layout change on one of them.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `layout-*.png` there; without it no GPU is needed.

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
        Self::demo_with_step(0.25)
    }

    /// The demo with `dt` seconds of input time per frame (the harness default is ¼ s).
    fn demo_with_step(dt: f32) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).with_step_dt(dt);
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

    fn request(&mut self, method: &str, params: Value) -> Value {
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
        let v = self.request(method, params.clone());
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

    /// Element rect `[x, y, w, h]`.
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}: {v}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn selection(&mut self) -> Vec<u64> {
        let v = self.ok("ui.inspect", json!({}));
        v["selection"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default()
    }

    fn inspect(&mut self) -> Value {
        self.exec("layout.inspect", json!({}))["clips"][0].clone()
    }

    /// Where a clip is and what it shows (`layout.inspect` without the clip id and track).
    fn look(&mut self, clip: u64) -> Value {
        let mut v = self.exec("layout.inspect", json!({"clips": [clip]}))["clips"][0].clone();
        v.as_object_mut().unwrap().retain(|k, _| k != "clip" && k != "track");
        v
    }

    fn status(&mut self) -> String {
        self.ok("ui.inspect", json!({}))["ui"]["status"].as_str().unwrap_or_default().to_string()
    }

    /// One entry of the menu bar (`ui.menu.list`).
    fn menu_item(&mut self, id: &str) -> Value {
        let items = self.ok("ui.menu.list", json!({}));
        items.as_array().unwrap().iter().find(|it| it["id"] == json!(id)).cloned().unwrap_or_else(|| panic!("no {id} in the menus"))
    }

    fn undo_steps(&mut self) -> usize {
        self.exec("history.list", json!({}))["undo"].as_array().unwrap().len()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("layout-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

#[test]
fn program_monitor_layouts() {
    let mut d = Driver::demo();
    d.exec("playhead.set", json!({"seconds": 7.0}));
    d.ok("ui.set", json!({"tool": "selection"}));
    d.frames(4);
    // the demo's V2 overlay (6–10 s, scaled down around 1540, 820) sits over V1
    let seq = d.exec("sequence.inspect", json!({}));
    let frame = (seq["settings"]["width"].as_f64().unwrap_or(1920.0), seq["settings"]["height"].as_f64().unwrap_or(1080.0));
    let stack: Vec<u64> = d.exec("layout.pick", json!({"x": 1540.0, "y": 820.0}))["clips"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
    assert!(stack.len() >= 2, "two clips under the overlay at 7 s: {stack:?}");
    let at = json!({"id": "program.picture", "fx": 1540.0 / frame.0, "fy": 820.0 / frame.1});

    // click the overlay: the top clip; again on the same spot: the one below; again: back to the top
    d.ok("ui.click", at.clone());
    d.frames(3);
    assert_eq!(d.selection(), vec![stack[0]]);
    let status = d.ok("ui.inspect", json!({}));
    assert!(status.to_string().contains("right-click for layouts"), "first-selection hint");
    d.ok("ui.click", at.clone());
    d.frames(3);
    assert_eq!(d.selection(), vec![stack[1]]);
    d.ok("ui.click", at.clone());
    d.frames(3);
    assert_eq!(d.selection(), vec![stack[0]]);
    let top = stack[0];
    // the box and its eight handles
    let ids = d.ids("program.layout.");
    for h in ["nw", "n", "ne", "e", "se", "s", "sw", "w"] {
        assert!(ids.contains(&format!("program.layout.handle.{h}")), "{ids:?}");
    }
    assert!(ids.contains(&"program.layout.box".to_string()));
    d.snapshot("selected");

    // right-click the box → Place ▸ Bottom Right
    d.ok("ui.click", json!({"id": "program.layout.box", "button": "right"}));
    d.frames(3);
    d.click("layout.menu.place");
    assert!(d.ids("layout.menu.place.").contains(&"layout.menu.place.bottomRight".to_string()));
    d.snapshot("menu");
    d.click("layout.menu.place.bottomRight");
    assert_eq!(d.selection(), vec![top]);
    let i = d.inspect();
    assert_eq!(i["at"], json!("bottomRight"), "{i}");

    // drag the box 40 px right: the position changes, one undo restores it
    let before = d.inspect()["box"].clone();
    let b = d.rect("program.layout.box");
    let (cx, cy) = (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx + 40.0, "y": cy}, "modifiers": {"command": true}}));
    d.frames(4);
    let after = d.inspect()["box"].clone();
    assert_ne!(before, after, "the drag moved the clip");
    let moved = d.inspect();
    assert!(moved["box"][0].as_f64().unwrap() > i["box"][0].as_f64().unwrap() + 10.0, "{moved} vs {i}");
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.inspect()["box"], before, "one undo step");
    assert_eq!(d.inspect()["at"], json!("bottomRight"));

    // a corner handle scales: drag nw outwards
    let nw = d.rect("program.layout.handle.nw");
    let (hx, hy) = (nw[0] + nw[2] / 2.0, nw[1] + nw[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": hx, "y": hy}, "to": {"x": hx - 30.0, "y": hy - 30.0}}));
    d.frames(4);
    let scaled = d.inspect();
    assert!(scaled["size"].as_f64().unwrap() > i["size"].as_f64().unwrap() + 1.0, "{scaled}");
    // the opposite (se) corner stayed
    let se = |v: &Value| (v["box"][0].as_f64().unwrap() + v["box"][2].as_f64().unwrap(), v["box"][1].as_f64().unwrap() + v["box"][3].as_f64().unwrap());
    let (a, b2) = (se(&i), se(&scaled));
    assert!((a.0 - b2.0).abs() < 1.0 && (a.1 - b2.1).abs() < 1.0, "{a:?} {b2:?}");
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.inspect()["size"], i["size"], "a scale drag is one undo step");

    // Effect Controls: the circle button
    d.ok("ui.panel.show", json!({"panel": "Effect Controls"}));
    d.frames(4);
    assert!(d.ids("effectControls.layout.").contains(&"effectControls.layout.place.topLeft".to_string()));
    d.click("effectControls.layout.shape.circle");
    assert_eq!(d.inspect()["shape"], json!("circle"));
    d.snapshot("circle");

    // Clip ▸ Layout ▸ Place ▸ Top Left, through the menu command
    let items = d.ok("ui.menu.list", json!({}));
    let tl = items.as_array().unwrap().iter().find(|it| it["id"] == json!("layout.menu.place.topLeft")).cloned().unwrap();
    assert_eq!(tl["path"], json!(["Clip", "Layout", "Place"]));
    let br = items.as_array().unwrap().iter().find(|it| it["id"] == json!("layout.menu.place.bottomRight")).cloned().unwrap();
    assert_eq!(br["checked"], json!(true), "{br}");
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.place.topLeft"}));
    let i = d.inspect();
    assert_eq!((i["at"].clone(), i["shape"].clone()), (json!("topLeft"), json!("circle")), "{i}");
    // Redact Area… turns the picture into a draw surface (panels::redact)
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.redact"}));
    d.frames(2);
    assert_eq!(d.ids("program.redact.draw"), vec!["program.redact.draw".to_string()]);
    assert!(d.ok("ui.inspect", json!({})).to_string().contains("Drag a box"));
}

#[test]
fn program_monitor_pan() {
    let mut d = Driver::demo();
    d.exec("playhead.set", json!({"seconds": 7.0}));
    d.ok("ui.set", json!({"tool": "selection"}));
    d.frames(4);
    let seq = d.exec("sequence.inspect", json!({}));
    let frame = (seq["settings"]["width"].as_f64().unwrap_or(1920.0), seq["settings"]["height"].as_f64().unwrap_or(1080.0));
    // select the V2 overlay and make it a circle
    d.ok("ui.click", json!({"id": "program.picture", "fx": 1540.0 / frame.0, "fy": 820.0 / frame.1}));
    d.frames(3);
    let top = d.selection()[0];
    d.exec("layout.shape", json!({"clips": [top], "shape": "circle"}));
    d.frames(3);
    let i0 = d.inspect();
    assert_eq!(i0["pan"], json!([0.0, 0.0]), "{i0}");
    let box0: Vec<f64> = i0["box"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    let same_box = |v: &Value| v["box"].as_array().unwrap().iter().zip(&box0).all(|(a, b)| (a.as_f64().unwrap() - b).abs() < 1.0);

    // Alt-drag inside the box 30 px right: the picture slides right, so the shape shows more of
    // the left of the source (a negative pan); the box stays put
    let b = d.rect("program.layout.box");
    let (cx, cy) = (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx + 30.0, "y": cy}, "modifiers": {"alt": true}}));
    d.frames(4);
    let i1 = d.inspect();
    assert!(i1["pan"][0].as_f64().unwrap() < -10.0, "{i1}");
    assert!(i1["pan"][1].as_f64().unwrap().abs() < 1e-6, "a 16:9 circle has no room to pan vertically: {i1}");
    assert!(same_box(&i1), "{i1} vs {i0}");
    assert_eq!(i1["shape"], json!("circle"));
    assert!(d.ok("ui.inspect", json!({})).to_string().contains("Pan: "), "status bar shows the pan");
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.inspect()["pan"], json!([0.0, 0.0]), "the drag is one undo step");

    // Clip ▸ Layout ▸ Pan ▸ Centre on Left Third (a 1920-wide source: the centre at x = 640)
    let items = d.ok("ui.menu.list", json!({}));
    let left = items.as_array().unwrap().iter().find(|it| it["id"] == json!("layout.menu.pan.left")).cloned().unwrap();
    assert_eq!(left["path"], json!(["Clip", "Layout", "Pan"]));
    assert_eq!(left["enabled"], json!(true), "{left}");
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.pan.left"}));
    let i2 = d.inspect();
    assert_eq!(i2["pan"], json!([-320.0, 0.0]), "{i2}");
    assert!(same_box(&i2), "{i2}");

    // the right-click menu has the same entries
    d.ok("ui.click", json!({"id": "program.layout.box", "button": "right"}));
    d.frames(3);
    d.click("layout.menu.pan");
    let ids = d.ids("layout.menu.pan.");
    for p in ["left", "center", "right"] {
        assert!(ids.contains(&format!("layout.menu.pan.{p}")), "{ids:?}");
    }
    d.click("layout.menu.pan.right");
    assert_eq!(d.inspect()["pan"], json!([320.0, 0.0]));
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.inspect()["pan"], json!([-320.0, 0.0]), "undo restores the previous pan");
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.pan.center"}));
    assert_eq!(d.inspect()["pan"], json!([0.0, 0.0]));

    // a free clip cannot pan: the entries are disabled
    d.exec("layout.shape", json!({"clips": [top], "shape": "free"}));
    d.frames(2);
    let items = d.ok("ui.menu.list", json!({}));
    let left = items.as_array().unwrap().iter().find(|it| it["id"] == json!("layout.menu.pan.left")).cloned().unwrap();
    assert_eq!(left["enabled"], json!(false), "{left}");
}

#[test]
fn program_monitor_zoom() {
    // 60 frames a second, so scroll notches a few frames apart are within the 400 ms merge window
    let mut d = Driver::demo_with_step(1.0 / 60.0);
    d.exec("playhead.set", json!({"seconds": 7.0}));
    d.ok("ui.set", json!({"tool": "selection"}));
    d.frames(4);
    let seq = d.exec("sequence.inspect", json!({}));
    let frame = (seq["settings"]["width"].as_f64().unwrap_or(1920.0), seq["settings"]["height"].as_f64().unwrap_or(1080.0));
    // select the V2 overlay and make it a circle
    d.ok("ui.click", json!({"id": "program.picture", "fx": 1540.0 / frame.0, "fy": 820.0 / frame.1}));
    d.frames(3);
    let top = d.selection()[0];
    d.exec("layout.shape", json!({"clips": [top], "shape": "circle"}));
    d.frames(3);
    let i0 = d.inspect();
    assert_eq!(i0["zoom"], json!(1.0), "{i0}");
    let box0: Vec<f64> = i0["box"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    let same_box = |v: &Value| v["box"].as_array().unwrap().iter().zip(&box0).all(|(a, b)| (a.as_f64().unwrap() - b).abs() < 1.0);
    let zoom = |v: &Value| v["zoom"].as_f64().unwrap();

    // Option + scroll over the box: three notches up zoom in, the box stays put
    for _ in 0..3 {
        d.ok("ui.scroll", json!({"id": "program.layout.box", "dy": 40.0, "modifiers": {"alt": true}}));
        d.frames(2);
    }
    let i1 = d.inspect();
    assert!((zoom(&i1) - 1.05f64.powi(3)).abs() < 0.01, "{i1}");
    assert!(same_box(&i1), "{i1} vs {i0}");
    assert_eq!(i1["shape"], json!("circle"));
    assert!(d.ok("ui.inspect", json!({})).to_string().contains("Zoom: 116 %"), "status bar shows the zoom");
    // a notch down zooms out
    d.ok("ui.scroll", json!({"id": "program.layout.box", "dy": -40.0, "modifiers": {"alt": true}}));
    d.frames(2);
    assert!((zoom(&d.inspect()) - 1.05f64.powi(2)).abs() < 0.01);
    // without Option the wheel does not zoom
    d.ok("ui.scroll", json!({"id": "program.layout.box", "dy": 40.0}));
    d.frames(2);
    assert!((zoom(&d.inspect()) - 1.05f64.powi(2)).abs() < 0.01);
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.inspect()["zoom"], json!(1.0), "notches in quick succession are one undo step");

    // Clip ▸ Layout ▸ Zoom ▸ 150 %: set and checked; Pan ▸ Centre is checked too
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.zoom.150"}));
    d.frames(2);
    let i2 = d.inspect();
    assert_eq!(i2["zoom"], json!(1.5), "{i2}");
    assert!(same_box(&i2), "{i2}");
    let items = d.ok("ui.menu.list", json!({}));
    let find = |id: &str| items.as_array().unwrap().iter().find(|it| it["id"] == json!(id)).cloned().unwrap_or_else(|| panic!("no {id}"));
    let z150 = find("layout.menu.zoom.150");
    assert_eq!(
        (z150["path"].clone(), z150["checked"].clone(), z150["enabled"].clone()),
        (json!(["Clip", "Layout", "Zoom"]), json!(true), json!(true)),
        "{z150}"
    );
    assert_eq!(find("layout.menu.zoom.100")["checked"], json!(false));
    assert_eq!(find("layout.menu.pan.center")["checked"], json!(true));
    assert_eq!(find("layout.menu.pan.left")["checked"], json!(false));

    // the right-click menu has the same entries; Zoom In multiplies by 1.25
    d.ok("ui.click", json!({"id": "program.layout.box", "button": "right"}));
    d.frames(3);
    d.click("layout.menu.zoom");
    let ids = d.ids("layout.menu.zoom.");
    for z in ["100", "125", "150", "200", "in", "out"] {
        assert!(ids.contains(&format!("layout.menu.zoom.{z}")), "{ids:?}");
    }
    d.click("layout.menu.zoom.in");
    assert_eq!(d.inspect()["zoom"], json!(1.875));
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.inspect()["zoom"], json!(1.5));

    // Option-drag of the SE handle outward zooms in without moving the box; one undo step
    let se = d.rect("program.layout.handle.se");
    let (hx, hy) = (se[0] + se[2] / 2.0, se[1] + se[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": hx, "y": hy}, "to": {"x": hx + 30.0, "y": hy + 30.0}, "modifiers": {"alt": true}}));
    d.frames(4);
    let i3 = d.inspect();
    assert!(zoom(&i3) > 1.6, "{i3}");
    assert!(same_box(&i3), "{i3} vs {i0}");
    assert!(d.ok("ui.inspect", json!({})).to_string().contains("Zoom: "), "status bar shows the zoom");
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.inspect()["zoom"], json!(1.5), "the drag is one undo step");

    // a free clip cannot zoom: the entries are disabled
    d.exec("layout.shape", json!({"clips": [top], "shape": "free"}));
    d.frames(2);
    let items = d.ok("ui.menu.list", json!({}));
    let z = items.as_array().unwrap().iter().find(|it| it["id"] == json!("layout.menu.zoom.in")).cloned().unwrap();
    assert_eq!(z["enabled"], json!(false), "{z}");
}

#[test]
fn apply_to_all_clips_of_this_source() {
    const HINT: &str = "Layout set on this clip only. Layout ▸ Apply to All Clips of This Source copies it to the other 3.";
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"tool": "selection"}));
    // the V2 overlay (6 s to 10 s) cut into four clips, like a camera a text edit cut up
    for t in [7.0, 8.0, 9.0] {
        d.exec("timeline.razor", json!({"seconds": t, "track": "V2"}));
    }
    d.exec("playhead.set", json!({"seconds": 7.5}));
    d.frames(4);
    let (pieces, name): (Vec<u64>, String) = {
        let q = d.harness.state().session.active_sequence().unwrap();
        (q.video_tracks[1].items.iter().map(|i| i.id.0).collect(), q.video_tracks[1].items[1].name.clone())
    };
    assert_eq!(pieces.len(), 4);
    let seq = d.exec("sequence.inspect", json!({}));
    let frame = (seq["settings"]["width"].as_f64().unwrap_or(1920.0), seq["settings"]["height"].as_f64().unwrap_or(1080.0));
    // click the overlay: the piece under the playhead
    d.ok("ui.click", json!({"id": "program.picture", "fx": 1540.0 / frame.0, "fy": 820.0 / frame.1}));
    d.frames(3);
    let me = pieces[1];
    assert_eq!(d.selection(), vec![me]);
    let others = [pieces[0], pieces[2], pieces[3]];

    // Clip ▸ Layout has the entry, enabled: the clip has siblings
    let it = d.menu_item("layout.menu.applyToSource");
    assert_eq!(
        (it["path"].clone(), it["label"].clone(), it["enabled"].clone()),
        (json!(["Clip", "Layout"]), json!("Apply to All Clips of This Source"), json!(true)),
        "{it}"
    );
    assert!(it.get("reason").is_none(), "{it}");

    // a layout change on this clip only: the status bar says so and names the entry
    d.ok("ui.click", json!({"id": "program.layout.box", "button": "right"}));
    d.frames(3);
    d.click("layout.menu.shape");
    d.click("layout.menu.shape.circle");
    assert_eq!(d.look(me)["shape"], json!("circle"));
    assert_eq!(d.status(), HINT);
    for c in others {
        assert_eq!(d.look(c)["shape"], json!("free"), "the other clips are not changed yet");
    }
    // the menu-bar command and a drag of the box end with the same hint
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.place.bottomRight"}));
    assert_eq!(d.status(), HINT);
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.pan.left"}));
    assert_eq!(d.status(), HINT);
    let b = d.rect("program.layout.box");
    let (cx, cy) = (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx - 30.0, "y": cy - 20.0}, "modifiers": {"command": true}}));
    d.frames(4);
    assert_eq!(d.status(), HINT, "after the drag");
    let want = d.look(me);
    assert_eq!((want["shape"].clone(), want["pan"].clone(), want["at"].clone()), (json!("circle"), json!([-320.0, 0.0]), json!("custom")), "{want}");

    // right-click the box ▸ Apply to All Clips of This Source: the three others get the layout
    let steps = d.undo_steps();
    d.ok("ui.click", json!({"id": "program.layout.box", "button": "right"}));
    d.frames(3);
    assert!(d.ids("layout.menu.").contains(&"layout.menu.applyToSource".to_string()));
    d.snapshot("apply-to-source-menu");
    d.click("layout.menu.applyToSource");
    for c in others {
        assert_eq!(d.look(c), want, "clip {c}");
    }
    assert_eq!(d.look(me), want, "the clip itself is as it was");
    assert_eq!(d.status(), format!("Layout applied to the other 3 clips of {name}"));
    assert_eq!(d.undo_steps(), steps + 1, "one undo step");
    assert_eq!(d.selection(), vec![me]);
    d.snapshot("apply-to-source");
    // now they all match: a change that changes nothing shows no hint
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.shape.circle"}));
    assert_ne!(d.status(), HINT);
    // one undo takes the layout off the three again
    d.exec("edit.undo", json!({}));
    d.exec("edit.undo", json!({}));
    d.frames(2);
    for c in others {
        assert_eq!(d.look(c)["shape"], json!("free"));
    }
    assert_eq!(d.look(me), want);

    // the Effect Controls row hints too; the command with `clips` works without a selection
    d.ok("ui.panel.show", json!({"panel": "Effect Controls"}));
    d.frames(4);
    d.click("effectControls.layout.shape.square");
    assert_eq!(d.look(me)["shape"], json!("square"));
    assert_eq!(d.status(), HINT);
    d.exec("timeline.select", json!({"clips": []}));
    d.frames(2);
    let it = d.menu_item("layout.menu.applyToSource");
    assert_eq!((it["enabled"].clone(), it["reason"].clone()), (json!(false), json!("select a video clip")), "{it}");
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.applyToSource", "params": {"clips": [me]}}));
    let want = d.look(me);
    for c in others {
        assert_eq!(d.look(c), want);
    }

    // a clip that is the only one of its source on its track: disabled, with the reason
    let lone = {
        let q = d.harness.state().session.active_sequence().unwrap();
        let v1 = &q.video_tracks[0].items;
        v1.iter().find(|i| v1.iter().filter(|x| x.item == i.item).count() == 1).map(|i| i.id.0).unwrap()
    };
    d.exec("timeline.select", json!({"clips": [lone]}));
    d.frames(2);
    let it = d.menu_item("layout.menu.applyToSource");
    assert_eq!((it["enabled"].clone(), it["reason"].clone()), (json!(false), json!("no other clip of this source on the track")), "{it}");
    let steps = d.undo_steps();
    let v = d.request("ui.menu.invoke", json!({"id": "layout.menu.applyToSource"}));
    assert_ne!(v["ok"], json!(true), "{v}");
    assert!(d.status().contains("no other clip of this source on the track"), "{}", d.status());
    assert_eq!(d.undo_steps(), steps);
    // and a layout change on it shows no hint
    d.ok("ui.menu.invoke", json!({"id": "layout.menu.place.topLeft"}));
    assert!(!d.status().contains("this clip only"), "{}", d.status());
}
