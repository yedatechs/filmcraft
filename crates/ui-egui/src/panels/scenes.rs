//! Scenes in the Text panel (`openspec/changes/clip-layouts/design.md` §4, `docs/layouts.md`
//! "Scenes"): the scene chip at the start of each Transcript paragraph and the Scenes… dialog.
//!
//! - Chip `text.scene.{paragraph}`: the scene assigned at the paragraph's first word (or "No
//!   scene"), drawn in the scene's colour. A click opens a menu: the sequence's scenes
//!   (`text.scene.{p}.pick.{index}`, runs `scenes.assign` on the paragraph's words), No Scene
//!   (`text.scene.{p}.none`, `scenes.clear`) and Scenes… (`text.scene.{p}.manage`).
//! - Dialog (Sequence ▸ Scenes…, `scenes.dialog`; open state `UiState::transcript_scenes_dialog`,
//!   selected scene `transcript_scenes_sel`, settable with `ui.set {"menuDialog": {"scenesSelected": n}}`):
//!   `text.scenes.list.{i}`, `text.scenes.add`, `text.scenes.duplicate`, `text.scenes.remove`,
//!   `text.scenes.defaults`, `text.scenes.swap` (swap A/B: the first two slots exchange media),
//!   `text.scenes.close`, and per slot `text.scenes.slot.{j}.place|size|shape|hidden` (the pickers'
//!   options `….place.{name}`, `….size.{n}`, `….shape.{name}`). Every change is one engine command
//!   (`scenes.add` / `scenes.update` / `scenes.remove` / `scenes.defaults`), so one undo step.

use egui::{Color32, Rect};
use filmcraft_edit::layout::{Place, Shape};
use filmcraft_edit::transcript::SeqWord;
use filmcraft_project::{Scene, SceneSlot};
use serde_json::{Value, json};

use crate::FilmcraftApp;

/// Scene colours, by scene index (wrapping).
const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x3d, 0x7e, 0xd6),
    Color32::from_rgb(0xd0, 0x7a, 0x2c),
    Color32::from_rgb(0x3f, 0xa3, 0x6b),
    Color32::from_rgb(0x9b, 0x5c, 0xc9),
    Color32::from_rgb(0xc9, 0x4f, 0x6d),
    Color32::from_rgb(0x2f, 0xa3, 0xa8),
    Color32::from_rgb(0xa8, 0x97, 0x2f),
    Color32::from_rgb(0x6d, 0x7a, 0x8c),
];

/// The colour of the scene at `index`.
pub fn color(index: usize) -> Color32 {
    PALETTE.get(index % PALETTE.len()).copied().unwrap_or(Color32::GRAY)
}

/// Sizes the dialog offers (% of the frame width).
const SIZES: [f64; 6] = [20.0, 25.0, 33.0, 50.0, 75.0, 100.0];

fn scenes(app: &FilmcraftApp) -> Vec<Scene> {
    app.session.active_sequence().map(|q| q.scenes.clone()).unwrap_or_default()
}

/// The scene chip of paragraph `pi` (words `para`), drawn inline before the speaker line.
pub fn chip(app: &mut FilmcraftApp, ui: &mut egui::Ui, pi: usize, para: std::ops::Range<usize>, words: &[SeqWord], actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    let list = scenes(app);
    let current = words.get(para.start).and_then(|w| filmcraft_engine::scenes::word_scene(&app.session.project, w));
    let at = current.and_then(|id| list.iter().position(|s| s.id == id));
    let (text, fill, fg) = match at.and_then(|i| list.get(i).map(|s| (i, s))) {
        Some((i, s)) => (s.name.clone(), color(i), Color32::WHITE),
        None => ("No scene".to_string(), t.field_bg, t.text_dim),
    };
    let resp = ui
        .add(egui::Button::new(egui::RichText::new(text).size(10.5).color(fg)).fill(fill).corner_radius(8.0))
        .on_hover_text("Scene of this paragraph: click to choose");
    app.auto.add(&format!("text.scene.{pi}"), resp.rect, "Scene");
    let (from, to) = (para.start, para.end.saturating_sub(1));
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut manage = false;
    egui::Popup::menu(&resp).show(|ui| {
        for (i, s) in list.iter().enumerate() {
            let r = ui.add(egui::Button::selectable(at == Some(i), egui::RichText::new(&s.name).color(color(i))));
            elems.push((format!("text.scene.{pi}.pick.{i}"), r.rect, s.name.clone()));
            if r.clicked() {
                actions.push(("scenes.assign".into(), json!({"scene": s.id, "from": from, "to": to})));
                ui.close();
            }
        }
        if list.is_empty() {
            let r = ui.button("Create Default Scenes");
            elems.push((format!("text.scene.{pi}.defaults"), r.rect, "Create Default Scenes".into()));
            if r.clicked() {
                actions.push(("scenes.defaults".into(), json!({})));
                ui.close();
            }
        } else {
            let r = ui.add_enabled(current.is_some(), egui::Button::new("No Scene"));
            elems.push((format!("text.scene.{pi}.none"), r.rect, "No Scene".into()));
            if r.clicked() {
                actions.push(("scenes.clear".into(), json!({"from": from, "to": to})));
                ui.close();
            }
        }
        ui.separator();
        let r = ui.button("Scenes…");
        elems.push((format!("text.scene.{pi}.manage"), r.rect, "Scenes…".into()));
        if r.clicked() {
            manage = true;
            ui.close();
        }
    });
    for (id, r, label) in elems {
        app.auto.add(&id, r, &label);
    }
    if manage {
        app.ui.transcript_scenes_sel = at.unwrap_or(0);
        app.ui.transcript_scenes_dialog = true;
    }
}

/// Open the Scenes… dialog (Sequence ▸ Scenes…, the chip menu).
pub fn open_dialog(app: &mut FilmcraftApp) -> Result<Value, String> {
    if app.session.active_sequence().is_none() {
        let e = "no sequence is open".to_string();
        app.ui.status = e.clone();
        return Err(e);
    }
    app.ui.transcript_scenes_dialog = true;
    Ok(json!({"dialog": "text.scenes"}))
}

fn slot_json(s: &SceneSlot) -> Value {
    json!({"item": s.item.0, "hidden": s.hidden, "place": s.place, "size": s.size, "margin": s.margin, "shape": s.shape, "radius": s.radius})
}

fn slots_json(slots: &[SceneSlot]) -> Value {
    Value::Array(slots.iter().map(slot_json).collect())
}

fn place_label(p: Place) -> &'static str {
    match p {
        Place::TopLeft => "Top Left",
        Place::TopRight => "Top Right",
        Place::BottomLeft => "Bottom Left",
        Place::BottomRight => "Bottom Right",
        Place::Top => "Top",
        Place::Bottom => "Bottom",
        Place::Left => "Left",
        Place::Right => "Right",
        Place::Center => "Center",
        Place::Full => "Full",
    }
}

fn shape_label(name: &str) -> &'static str {
    match name {
        "circle" => "Circle",
        "rounded" => "Rounded",
        "square" => "Square",
        _ => "Free",
    }
}

/// The Scenes… dialog.
pub fn dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if !app.ui.transcript_scenes_dialog {
        return;
    }
    let list = scenes(app);
    let sel = app.ui.transcript_scenes_sel.min(list.len().saturating_sub(1));
    let names: std::collections::BTreeMap<u64, String> = app.session.project.items.iter().map(|(id, it)| (id.0, it.name.clone())).collect();
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut new_sel = sel;
    let mut close = false;
    egui::Window::new("Scenes")
        .id(egui::Id::new("text-scenes-dialog"))
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new("Clips in a scene are arranged by the scene: change the scene here, and every paragraph that uses it follows.").weak(),
            );
            ui.add_space(4.0);
            ui.horizontal_top(|ui| {
                // the list
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    if list.is_empty() {
                        ui.label(egui::RichText::new("No scenes yet").weak());
                    }
                    for (i, s) in list.iter().enumerate() {
                        let r = ui.add(egui::Button::selectable(i == sel, egui::RichText::new(&s.name).color(color(i))));
                        elems.push((format!("text.scenes.list.{i}"), r.rect, s.name.clone()));
                        if r.clicked() {
                            new_sel = i;
                        }
                    }
                    ui.add_space(6.0);
                    let all_items: Vec<SceneSlot> = {
                        let mut v: Vec<SceneSlot> = Vec::new();
                        for sl in list.iter().flat_map(|s| s.slots.iter()) {
                            if !v.iter().any(|x| x.item == sl.item) {
                                v.push(SceneSlot { item: sl.item, ..Default::default() });
                            }
                        }
                        v
                    };
                    ui.horizontal_wrapped(|ui| {
                        let r = ui.button("Add");
                        elems.push(("text.scenes.add".into(), r.rect, "Add".into()));
                        if r.clicked() {
                            actions.push(("scenes.add".into(), json!({"slots": slots_json(&all_items)})));
                            new_sel = list.len();
                        }
                        let r = ui.add_enabled(!list.is_empty(), egui::Button::new("Duplicate"));
                        elems.push(("text.scenes.duplicate".into(), r.rect, "Duplicate".into()));
                        if r.clicked()
                            && let Some(s) = list.get(sel)
                        {
                            actions.push(("scenes.add".into(), json!({"name": format!("{} copy", s.name), "slots": slots_json(&s.slots)})));
                            new_sel = list.len();
                        }
                        let r = ui.add_enabled(!list.is_empty(), egui::Button::new("Remove"));
                        elems.push(("text.scenes.remove".into(), r.rect, "Remove".into()));
                        if r.clicked()
                            && let Some(s) = list.get(sel)
                        {
                            actions.push(("scenes.remove".into(), json!({"scene": s.id})));
                            new_sel = sel.saturating_sub(1);
                        }
                        let r = ui.button("Create Defaults");
                        elems.push(("text.scenes.defaults".into(), r.rect, "Create Defaults".into()));
                        if r.clicked() {
                            actions.push(("scenes.defaults".into(), json!({})));
                        }
                    });
                });
                // (no vertical separator: it takes the window's height and the window would grow)
                ui.add_space(12.0);
                // the selected scene's slots
                ui.vertical(|ui| {
                    let Some(s) = list.get(sel) else {
                        ui.label(
                            egui::RichText::new("Create Defaults makes Screen with face, Face, Screen and Half and half from the two top video tracks.").weak(),
                        );
                        return;
                    };
                    ui.label(egui::RichText::new(&s.name).strong().color(color(sel)));
                    let mut slots = s.slots.clone();
                    let mut changed = false;
                    egui::Grid::new("text-scenes-slots").num_columns(5).spacing([8.0, 6.0]).show(ui, |ui| {
                        for (j, sl) in slots.iter_mut().enumerate() {
                            let ab = match j {
                                0 => "A ",
                                1 => "B ",
                                _ => "",
                            };
                            ui.label(format!("{ab}{}", names.get(&sl.item.0).cloned().unwrap_or_else(|| format!("item {}", sl.item.0))));
                            let cur = Place::parse(&sl.place).unwrap_or(Place::Full);
                            let r = egui::ComboBox::from_id_salt(("scene-place", j)).selected_text(place_label(cur)).width(110.0).show_ui(ui, |ui| {
                                for p in Place::ALL {
                                    let r = ui.selectable_label(p == cur, place_label(p));
                                    elems.push((format!("text.scenes.slot.{j}.place.{}", p.name()), r.rect, place_label(p).into()));
                                    if r.clicked() && p != cur {
                                        sl.place = p.name().into();
                                        changed = true;
                                    }
                                }
                            });
                            elems.push((format!("text.scenes.slot.{j}.place"), r.response.rect, "Place".into()));
                            let r = egui::ComboBox::from_id_salt(("scene-size", j)).selected_text(format!("{:.0} %", sl.size)).width(64.0).show_ui(ui, |ui| {
                                for n in SIZES {
                                    let r = ui.selectable_label((sl.size - n).abs() < 0.5, format!("{n:.0} %"));
                                    elems.push((format!("text.scenes.slot.{j}.size.{n:.0}"), r.rect, format!("{n:.0} %")));
                                    if r.clicked() && (sl.size - n).abs() >= 0.5 {
                                        sl.size = n;
                                        changed = true;
                                    }
                                }
                            });
                            elems.push((format!("text.scenes.slot.{j}.size"), r.response.rect, "Size".into()));
                            let r = egui::ComboBox::from_id_salt(("scene-shape", j)).selected_text(shape_label(&sl.shape)).width(80.0).show_ui(ui, |ui| {
                                for name in ["free", "circle", "rounded", "square"] {
                                    let r = ui.selectable_label(sl.shape == name, shape_label(name));
                                    elems.push((format!("text.scenes.slot.{j}.shape.{name}"), r.rect, shape_label(name).into()));
                                    if r.clicked() && sl.shape != name && Shape::parse(name, sl.radius).is_some() {
                                        sl.shape = name.into();
                                        changed = true;
                                    }
                                }
                            });
                            elems.push((format!("text.scenes.slot.{j}.shape"), r.response.rect, "Shape".into()));
                            let mut hidden = sl.hidden;
                            let r = ui.checkbox(&mut hidden, "Hidden");
                            elems.push((format!("text.scenes.slot.{j}.hidden"), r.rect, "Hidden".into()));
                            if r.changed() {
                                sl.hidden = hidden;
                                changed = true;
                            }
                            ui.end_row();
                        }
                    });
                    let r = ui
                        .add_enabled(slots.len() >= 2, egui::Button::new("Swap A and B"))
                        .on_hover_text("The first two media items exchange their places in this scene");
                    elems.push(("text.scenes.swap".into(), r.rect, "Swap A and B".into()));
                    if r.clicked()
                        && let (Some(a), Some(b)) = (slots.first().map(|x| x.item), slots.get(1).map(|x| x.item))
                    {
                        if let Some(x) = slots.get_mut(0) {
                            x.item = b;
                        }
                        if let Some(x) = slots.get_mut(1) {
                            x.item = a;
                        }
                        changed = true;
                    }
                    if changed {
                        actions.push(("scenes.update".into(), json!({"scene": s.id, "slots": slots_json(&slots)})));
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r = ui.button("Close");
                elems.push(("text.scenes.close".into(), r.rect, "Close".into()));
                if r.clicked() {
                    close = true;
                }
            });
        });
    for (id, r, label) in elems {
        app.auto.add(&id, r, &label);
    }
    app.ui.transcript_scenes_sel = new_sel;
    if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.transcript_scenes_dialog = false;
    }
    for (cmd, p) in actions {
        if let Err(e) = crate::menus::invoke(app, ctx, &cmd, p) {
            app.ui.status = e;
        }
    }
}
