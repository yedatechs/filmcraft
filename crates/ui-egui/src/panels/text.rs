//! The Text panel: Transcript / Captions / Graphics tabs. The Captions tab lists the caption
//! segments of a caption track with editable in/out timecodes and text, a toolbar (add, split,
//! merge, delete), track choice and the track style. Every edit dispatches a `captions.*` engine
//! command; every widget registers an automation id (`text.*`). The Transcript tab shows the
//! sequence transcript as speaker paragraphs of clickable words (click, Shift+click to extend);
//! the selection marks In/Out and can be extracted or lifted (`transcript.*` commands). Crossed-out
//! text (cut spans) shows struck through in place and restores on click; take groups get a
//! `Take 2/3` chip and an optional Takes list (`takes.*` commands). Remove Pauses opens a dialog
//! (`text.pauses.*`) with the threshold, the length kept and a live count from `transcript.pauses`.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::{CaptionAlign, CaptionAnchor, CaptionFormat};
use filmcraft_time::{TimeDisplay, format_time};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

const TABS: [&str; 3] = ["Transcript", "Captions", "Graphics"];

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    // tabs
    let mut x = rect.min.x + 12.0;
    for tab in TABS {
        let w = tab.len() as f32 * 7.0 + 16.0;
        let r = Rect::from_min_size(pos2(x, rect.min.y + 4.0), vec2(w, 24.0));
        let resp = ui.interact(r, egui::Id::new(("text-tab", tab)), Sense::click());
        let active = app.ui.text_tab == tab;
        ui.painter().text(
            pos2(r.min.x, r.center().y),
            Align2::LEFT_CENTER,
            tab,
            if active { Tokens::semibold(12.5) } else { Tokens::ui(12.5) },
            if active { t.text } else { t.text_dim },
        );
        if active {
            ui.painter().line_segment([pos2(r.min.x, r.max.y), pos2(r.min.x + w - 16.0, r.max.y)], Stroke::new(2.0, t.text));
        }
        app.auto.add(&format!("text.tab.{tab}"), r, tab);
        if resp.clicked() {
            app.ui.text_tab = tab.to_string();
        }
        x += w + 6.0;
    }
    let body = Rect::from_min_max(pos2(rect.min.x, rect.min.y + 34.0), rect.max);
    match app.ui.text_tab.as_str() {
        "Captions" => captions(app, ui, body),
        "Transcript" => transcript(app, ui, body),
        _ => crate::dock::placeholder(ui, body, &t, "Graphics text search arrives with M10.1–M10.2"),
    }
}

fn tool_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, icon: Icon, id: &str, label: &str, enabled: bool) -> bool {
    let t = app.tokens;
    // the toolbar is icons only, so a hovered button names itself at once (tooltip without the
    // usual delay) and in the status bar
    ui.style_mut().interaction.tooltip_delay = 0.0;
    let resp = ui.interact(r, egui::Id::new(("text-tool", id)), Sense::click());
    if resp.hovered() {
        if enabled {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        app.ui.status = if enabled { label.to_string() } else { format!("{label} (nothing to apply it to yet)") };
    }
    icons::paint(ui.painter(), r.shrink(5.0), icon, if enabled { t.icon } else { t.text_faint });
    app.auto.add(id, r, label);
    resp.on_hover_text(label).clicked() && enabled
}

fn captions(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some(seq) = app.session.active_sequence().cloned() else {
        crate::dock::placeholder(ui, rect, &t, "Open a sequence to work with captions");
        return;
    };
    let mut actions: Vec<(String, Value)> = Vec::new();
    if seq.caption_tracks.is_empty() {
        let c = rect.center();
        icons::paint(ui.painter(), Rect::from_center_size(c - vec2(0.0, 70.0), vec2(40.0, 40.0)), Icon::Captions, t.text_dim);
        ui.painter().text(c - vec2(0.0, 30.0), Align2::CENTER_CENTER, "Add captions", Tokens::semibold(16.0), t.text);
        ui.painter().text(c - vec2(0.0, 8.0), Align2::CENTER_CENTER, "Create a caption track or import a caption file.", Tokens::ui(12.0), t.text_dim);
        for (i, (id, label, cmd)) in
            [("text.captions.newTrack", "Create new caption track", "captions.newTrack"), ("text.captions.import", "Import captions file…", "captions.import")]
                .into_iter()
                .enumerate()
        {
            let r = Rect::from_center_size(c + vec2(0.0, 26.0 + i as f32 * 34.0), vec2(200.0, 26.0));
            let resp = ui.interact(r, egui::Id::new(id), Sense::click());
            ui.painter().rect_filled(r, 13.0, if i == 0 { if resp.hovered() { t.accent_hover } else { t.accent } } else { t.field_bg });
            ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::semibold(12.0), Color32::WHITE);
            app.auto.add(id, r, label);
            if resp.clicked() {
                actions.push((cmd.into(), json!({})));
            }
        }
        run(app, ui, actions);
        return;
    }
    let rate = seq.settings.frame_rate;
    let df = seq.settings.drop_frame;
    let tc = |x: filmcraft_time::Tick| format_time(x, rate, df, TimeDisplay::Timecode, 48_000);
    // the track shown: the one holding the first selected caption, else C1
    let sel = app.session.state.caption_selection.clone();
    let track_idx = sel.first().and_then(|c| seq.caption_tracks.iter().position(|tr| tr.caption(*c).is_some())).unwrap_or(0);
    let track_idx = ui.ctx().data(|d| d.get_temp::<usize>(egui::Id::new("text-cap-track"))).filter(|i| *i < seq.caption_tracks.len()).unwrap_or(track_idx);
    let track = &seq.caption_tracks[track_idx];

    // ---- toolbar: search, track picker, add / split / merge / delete
    let bar = Rect::from_min_size(rect.min + vec2(10.0, 2.0), vec2(rect.width() - 20.0, 26.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(bar.min, vec2(170.0f32.min(bar.width() * 0.4), 24.0))));
    let sresp = crate::widgets::search_field(&mut child, &mut app.ui.caption_search, "Search", 170.0f32.min(bar.width() * 0.4), &t);
    app.auto.add("text.captions.search", sresp.rect, "Search captions");
    let mut x = bar.min.x + 180.0f32.min(bar.width() * 0.4 + 10.0);
    let picker = Rect::from_min_size(pos2(x, bar.min.y + 1.0), vec2(130.0, 22.0));
    let label = format!("C{} · {}", track_idx + 1, track.name);
    let presp = crate::widgets::dropdown_text(ui, picker, &label, &t, egui::Id::new("text-cap-track-picker"));
    app.auto.add("text.captions.track", picker, "Caption track");
    egui::Popup::menu(&presp).show(|ui| {
        for (i, tr) in seq.caption_tracks.iter().enumerate() {
            if ui.selectable_label(i == track_idx, format!("C{} · {} ({})", i + 1, tr.name, tr.format.label())).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("text-cap-track"), i));
            }
        }
        ui.separator();
        for f in CaptionFormat::ALL {
            if ui.button(format!("New {} track", f.label())).clicked() {
                actions.push(("captions.newTrack".into(), json!({"format": f.label()})));
            }
        }
    });
    x = picker.max.x + 8.0;
    let ph = app.session.playhead();
    let any_sel = !sel.is_empty();
    let under = track.caption_at(ph).filter(|c| c.start < ph).map(|c| c.id);
    let tools: [(Icon, &str, &str, bool, &str, Value); 5] = [
        (Icon::Plus, "text.captions.add", "Add caption at playhead", track.caption_at(ph).is_none(), "captions.add", json!({"track": track.id.0})),
        (
            Icon::Razor,
            "text.captions.split",
            "Split caption at playhead",
            under.is_some(),
            "captions.split",
            json!({"caption": under.map(|c| c.0), "time": ph.0}),
        ),
        (Icon::Link, "text.captions.merge", "Merge selected captions", sel.len() > 1, "captions.merge", json!({})),
        (Icon::Trash, "text.captions.delete", "Delete selected captions", any_sel, "captions.delete", json!({})),
        (Icon::Export, "text.captions.export", "Export captions…", !track.captions.is_empty(), "captions.export", json!({"track": track.id.0})),
    ];
    for (icon, id, label, enabled, cmd, params) in tools {
        let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(24.0, 24.0));
        if r.max.x > rect.max.x {
            break;
        }
        if tool_button(app, ui, r, icon, id, label, enabled) {
            actions.push((cmd.into(), params));
        }
        x += 28.0;
    }

    // ---- style strip
    let style_h = 30.0;
    let style_rect = Rect::from_min_max(pos2(rect.min.x + 10.0, rect.max.y - style_h), pos2(rect.max.x - 10.0, rect.max.y - 2.0));
    style_strip(app, ui, style_rect, track_idx, &mut actions);

    // ---- segment list
    let list = Rect::from_min_max(pos2(rect.min.x + 6.0, bar.max.y + 8.0), pos2(rect.max.x - 6.0, style_rect.min.y - 6.0));
    ui.painter().rect_filled(list, 3.0, t.app_bg);
    let q = app.ui.caption_search.to_lowercase();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list.shrink(4.0)).id_salt("caption-list"));
    let current = track.caption_at(ph).map(|c| c.id);
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("caption-scroll").show(&mut child, |ui| {
        ui.set_width(list.width() - 12.0);
        let mut shown = 0;
        for (n, c) in track.captions.iter().enumerate() {
            if !q.is_empty() && !c.text.to_lowercase().contains(&q) && !c.speaker.as_deref().unwrap_or("").to_lowercase().contains(&q) {
                continue;
            }
            shown += 1;
            let selected = sel.contains(&c.id);
            let fill = if selected {
                t.row_selected
            } else if n % 2 == 1 {
                t.row_alt
            } else {
                Color32::TRANSPARENT
            };
            let frame = egui::Frame::NONE.fill(fill).inner_margin(egui::Margin::symmetric(6, 4)).corner_radius(3.0);
            let fr = frame.show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(92.0);
                        let num = ui.add(egui::Button::new(egui::RichText::new(format!("{}", n + 1)).size(11.0).color(t.text_dim)).frame(false));
                        app.auto.add(&format!("text.captions.{}.goto", c.id.0), num.rect, "Go to caption");
                        if num.clicked() {
                            if ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                                actions.push(("captions.select".into(), json!({"captions": [c.id.0], "add": true})));
                            } else {
                                actions.push(("captions.goTo".into(), json!({"caption": c.id.0})));
                            }
                        }
                        for (edge, val) in [("in", c.start), ("out", c.end())] {
                            let key = egui::Id::new(("cap-tc", c.id.0, edge));
                            let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| tc(val));
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut buf)
                                    .id(key.with("te"))
                                    .desired_width(88.0)
                                    .font(Tokens::mono(11.0))
                                    .text_color(if edge == "in" { t.hot_text } else { t.text_dim }),
                            );
                            app.auto.add(&format!("text.captions.{}.{edge}", c.id.0), resp.rect, if edge == "in" { "Caption in" } else { "Caption out" });
                            if resp.has_focus() {
                                ui.data_mut(|d| d.insert_temp(key, buf.clone()));
                            } else {
                                ui.data_mut(|d| d.remove::<String>(key));
                            }
                            if resp.lost_focus() && buf.trim() != tc(val) {
                                let k = if edge == "in" { "startTimecode" } else { "endTimecode" };
                                actions.push(("captions.setTimes".into(), json!({"caption": c.id.0, k: buf.trim()})));
                            }
                        }
                    });
                    ui.vertical(|ui| {
                        if let Some(sp) = &c.speaker {
                            ui.label(egui::RichText::new(sp).size(11.0).color(t.text_dim).strong());
                        }
                        let key = egui::Id::new(("cap-text", c.id.0));
                        let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| c.text.clone());
                        let resp = ui.add(
                            egui::TextEdit::multiline(&mut buf)
                                .id(key.with("te"))
                                .desired_rows(1)
                                .desired_width(ui.available_width())
                                .font(Tokens::ui(12.5))
                                .frame(egui::Frame::NONE),
                        );
                        app.auto.add(&format!("text.captions.{}.text", c.id.0), resp.rect, "Caption text");
                        if resp.has_focus() {
                            ui.data_mut(|d| d.insert_temp(key, buf.clone()));
                            if !selected {
                                actions.push(("captions.select".into(), json!({"captions": [c.id.0]})));
                            }
                        } else {
                            ui.data_mut(|d| d.remove::<String>(key));
                        }
                        if resp.lost_focus() && buf != c.text {
                            actions.push(("captions.setText".into(), json!({"caption": c.id.0, "text": buf})));
                        }
                    });
                });
            });
            let row = fr.response.rect;
            if current == Some(c.id) {
                ui.painter().rect_stroke(row, 3.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
            }
            app.auto.add(&format!("text.captions.{}.row", c.id.0), row, &c.text);
            ui.add_space(2.0);
        }
        if shown == 0 {
            ui.label(
                egui::RichText::new(if q.is_empty() { "No captions on this track. Press + to add one at the playhead." } else { "No matching captions." })
                    .color(t.text_faint),
            );
        }
    });
    run(app, ui, actions);
}

/// Take labels as `takes.label` names them, with the text the panel shows.
const TAKE_LABELS: [(&str, &str); 5] = [("good", "Good"), ("best", "Best"), ("flat", "Flat"), ("stumble", "Stumble"), ("wrongEnergy", "Wrong energy")];

fn label_text(name: &str) -> &str {
    TAKE_LABELS.iter().find(|(n, _)| *n == name).map(|(_, d)| *d).unwrap_or(name)
}

/// One take of a group, from `takes::groups_json`.
struct TakeRow {
    index: usize,
    seconds: f64,
    text: String,
    /// Media word indices `[a, b)`.
    words: (usize, usize),
    label: Option<String>,
    live: bool,
}

/// A take group, from `takes::groups_json`.
struct GroupRow {
    id: u64,
    item: u64,
    active: Option<usize>,
    redo: bool,
    at: Option<i64>,
    takes: Vec<TakeRow>,
}

fn parse_groups(groups: &[Value]) -> Vec<GroupRow> {
    let idx = |v: &Value| v.as_u64().and_then(|n| usize::try_from(n).ok());
    groups
        .iter()
        .filter_map(|g| {
            let takes = g
                .get("takes")?
                .as_array()?
                .iter()
                .filter_map(|k| {
                    let w = k.get("words")?.as_array()?;
                    Some(TakeRow {
                        index: idx(k.get("index")?)?,
                        seconds: k.get("seconds").and_then(Value::as_f64).unwrap_or(0.0),
                        text: k.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                        words: (idx(w.first()?)?, idx(w.get(1)?)?),
                        label: k.get("label").and_then(Value::as_str).map(str::to_string),
                        live: k.get("live").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect();
            Some(GroupRow {
                id: g.get("id")?.as_u64()?,
                item: g.get("item")?.as_u64()?,
                active: g.get("active").and_then(idx),
                redo: g.get("redo").and_then(Value::as_bool).unwrap_or(false),
                at: g.get("at").and_then(Value::as_i64),
                takes,
            })
        })
        .collect()
}

/// A cut span as the Transcript tab draws it.
struct CutView {
    index: usize,
    item: u64,
    /// Media word indices.
    media_words: std::ops::Range<usize>,
    words: Vec<String>,
    seconds: f64,
}

/// Crossed-out words of a cut span: struck through and dimmed; a click restores the span.
fn cut_span(app: &mut FilmcraftApp, ui: &mut egui::Ui, c: &CutView, actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    let hover = "Restore crossed-out text";
    let style = |s: &str| egui::RichText::new(s).size(13.0).color(t.text_dim).strikethrough();
    let mut first: Option<Rect> = None;
    let mut clicked = false;
    if c.words.is_empty() {
        let resp = ui.add(egui::Label::new(style(&format!("⋯ {:.1} s", c.seconds))).sense(Sense::click())).on_hover_text(hover);
        first = Some(resp.rect);
        clicked |= resp.clicked();
    } else {
        for (k, w) in c.words.iter().enumerate() {
            let resp = ui.add(egui::Label::new(style(w)).sense(Sense::click())).on_hover_text(hover);
            app.auto.add(&format!("text.transcript.cut.{}.{k}", c.index), resp.rect, w);
            first.get_or_insert(resp.rect);
            clicked |= resp.clicked();
        }
    }
    if let Some(r) = first {
        app.auto.add(&format!("text.transcript.cut.{}", c.index), r, hover);
    }
    if clicked {
        actions.push(("transcript.restore".into(), json!({"cut": c.index})));
    }
}

/// The `Take 2/3` chip of a group: click next take, Shift+click previous, right-click labels.
fn take_chip(app: &mut FilmcraftApp, ui: &mut egui::Ui, g: &GroupRow, actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    let n = g.takes.len();
    let text = match g.active {
        Some(a) => format!("Take {}/{n}", a.saturating_add(1)),
        None => format!("Takes {n}"),
    };
    let hover: Vec<String> = g
        .takes
        .iter()
        .map(|k| {
            let mut s = format!("Take {}: {} ({:.1} s)", k.index.saturating_add(1), k.text, k.seconds);
            if let Some(l) = &k.label {
                s.push_str(&format!(" · {}", label_text(l)));
            }
            if g.active == Some(k.index) {
                s.push_str(" · in the cut");
            }
            s
        })
        .collect();
    let resp = ui
        .add(egui::Button::new(egui::RichText::new(text).size(10.5).color(t.text)).fill(t.field_bg).corner_radius(8.0))
        .on_hover_text(format!("{}\nClick: next take · Shift+click: previous · Right-click: label", hover.join("\n")));
    app.auto.add(&format!("text.transcript.take.{}", g.id), resp.rect, "Take");
    if resp.clicked() {
        let cmd = if ui.input(|i| i.modifiers.shift) { "takes.previous" } else { "takes.next" };
        actions.push((cmd.into(), json!({"group": g.id})));
    }
    resp.context_menu(|ui| {
        let active = g.active;
        for (name, shown) in TAKE_LABELS {
            if ui.add_enabled(active.is_some(), egui::Button::new(shown)).clicked() {
                actions.push(("takes.label".into(), json!({"group": g.id, "take": active, "label": name})));
                ui.close();
            }
        }
        if ui.add_enabled(active.is_some(), egui::Button::new("Clear label")).clicked() {
            actions.push(("takes.label".into(), json!({"group": g.id, "take": active, "label": null})));
            ui.close();
        }
        ui.separator();
        let mut redo = g.redo;
        if ui.checkbox(&mut redo, "Needs re-record").changed() {
            actions.push(("takes.redo".into(), json!({"group": g.id, "redo": redo})));
            ui.close();
        }
    });
}

/// The Takes list: one row per group, expandable to its takes.
fn takes_list(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, groups: &[GroupRow], tc: &dyn Fn(i64) -> String, actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 3.0, t.app_bg);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(6.0)).id_salt("takes-list"));
    child.horizontal(|ui| {
        ui.label(egui::RichText::new("Takes").size(12.0).color(t.text).strong());
        let mut only = app.ui.transcript_takes_redo_only;
        let resp = ui.checkbox(&mut only, "Needs re-record only");
        app.auto.add("text.takes.redoOnly", resp.rect, "Needs re-record only");
        if resp.changed() {
            app.ui.transcript_takes_redo_only = only;
        }
    });
    let redo_only = app.ui.transcript_takes_redo_only;
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("takes-scroll").show(&mut child, |ui| {
        ui.set_width(rect.width() - 16.0);
        let mut shown = 0;
        for g in groups.iter().filter(|g| !redo_only || g.redo) {
            shown += 1;
            let open = app.ui.transcript_takes_open == Some(g.id);
            let active = g.active.and_then(|a| g.takes.iter().find(|k| k.index == a));
            let (text, dim) = match active {
                Some(k) => (k.text.as_str(), false),
                None => (g.takes.first().map(|k| k.text.as_str()).unwrap_or(""), true),
            };
            let text: String = text.chars().take(80).collect();
            ui.horizontal_wrapped(|ui| {
                let head = format!("{}  {text}", g.at.map(tc).unwrap_or_else(|| "--".into()));
                let resp = ui.selectable_label(open, egui::RichText::new(head).size(12.0).color(if dim { t.text_dim } else { t.text }));
                app.auto.add(&format!("text.takes.group.{}", g.id), resp.rect, &text);
                if resp.clicked() {
                    app.ui.transcript_takes_open = if open { None } else { Some(g.id) };
                }
                ui.label(egui::RichText::new(format!("{} takes", g.takes.len())).size(11.0).color(t.text_dim));
                if let Some(l) = active.and_then(|k| k.label.as_deref()) {
                    ui.label(egui::RichText::new(label_text(l)).size(11.0).color(t.text));
                }
                if g.redo {
                    ui.label(egui::RichText::new("redo").size(11.0).color(t.hot_text));
                }
            });
            if !open {
                continue;
            }
            for k in &g.takes {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new(format!("{}", k.index.saturating_add(1))).size(11.0).color(t.text_dim).strong());
                    ui.label(egui::RichText::new(format!("{:.1} s", k.seconds)).size(11.0).color(t.text_dim));
                    let mut txt = egui::RichText::new(&k.text).size(12.0);
                    txt = if k.live { txt.color(t.text) } else { txt.color(t.text_dim).strikethrough() };
                    ui.label(txt);
                    let id = |a: &str| format!("text.takes.take.{}.{}.{a}", g.id, k.index);
                    let p = json!({"group": g.id, "take": k.index});
                    let resp = ui.small_button("Play").on_hover_text("Play the take in context (In to Out)");
                    app.auto.add(&id("play"), resp.rect, "Play take");
                    if resp.clicked() {
                        actions.push(("takes.preview".into(), p.clone()));
                        actions.push(("playback.inToOut".into(), json!({})));
                    }
                    let resp = ui.small_button("Select").on_hover_text("Make this the take in the cut");
                    app.auto.add(&id("select"), resp.rect, "Select take");
                    if resp.clicked() {
                        actions.push(("takes.select".into(), p.clone()));
                    }
                    let (name, cmd, action) = if k.live { ("Cross out", "takes.cross", "cross") } else { ("Restore", "takes.restore", "restore") };
                    let resp = ui.small_button(name);
                    app.auto.add(&id(action), resp.rect, name);
                    if resp.clicked() {
                        actions.push((cmd.into(), p.clone()));
                    }
                    let current = k.label.as_deref().map(label_text).unwrap_or("No label");
                    let cb = egui::ComboBox::from_id_salt(("take-label", g.id, k.index)).selected_text(current).width(100.0).show_ui(ui, |ui| {
                        if ui.selectable_label(k.label.is_none(), "No label").clicked() && k.label.is_some() {
                            actions.push(("takes.label".into(), json!({"group": g.id, "take": k.index, "label": null})));
                        }
                        for (n, shown) in TAKE_LABELS {
                            let on = k.label.as_deref() == Some(n);
                            if ui.selectable_label(on, shown).clicked() && !on {
                                actions.push(("takes.label".into(), json!({"group": g.id, "take": k.index, "label": n})));
                            }
                        }
                    });
                    app.auto.add(&id("label"), cb.response.rect, "Take label");
                });
            }
            ui.add_space(4.0);
        }
        if shown == 0 {
            let msg = if groups.is_empty() { "No takes yet. Detect Takes finds lines said more than once." } else { "No group needs a re-record." };
            ui.label(egui::RichText::new(msg).color(t.text_faint));
        }
    });
}

fn transcript(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    if app.session.active_sequence().is_none() {
        crate::dock::placeholder(ui, rect, &t, "Open a sequence to see its transcript");
        return;
    }
    let mut actions: Vec<(String, Value)> = Vec::new();
    let words = filmcraft_engine::transcript::sequence_words(&app.session);
    let spans = filmcraft_engine::transcript::cut_spans(&app.session);
    if words.is_empty() && spans.is_empty() {
        let c = rect.center();
        if let Some(job) = filmcraft_engine::transcript::running(&app.session) {
            transcribing(app, ui, c, &job, &t);
            return;
        }
        icons::paint(ui.painter(), Rect::from_center_size(c - vec2(0.0, 70.0), vec2(40.0, 40.0)), Icon::Captions, t.text_dim);
        ui.painter().text(c - vec2(0.0, 30.0), Align2::CENTER_CENTER, "Transcribe sequence", Tokens::semibold(16.0), t.text);
        let note = if filmcraft_speech_available(app) {
            "Speech-to-text turns the dialogue into editable text."
        } else {
            "This build has no speech-to-text; choose the whisper.cpp engine in Settings, or import a transcript with transcript.set."
        };
        ui.painter().text(c - vec2(0.0, 8.0), Align2::CENTER_CENTER, note, Tokens::ui(12.0), t.text_dim);
        let r = Rect::from_center_size(c + vec2(0.0, 26.0), vec2(200.0, 26.0));
        let resp = ui.interact(r, egui::Id::new("text.transcript.generate"), Sense::click());
        ui.painter().rect_filled(r, 13.0, if resp.hovered() { t.accent_hover } else { t.accent });
        ui.painter().text(r.center(), Align2::CENTER_CENTER, "Transcribe", Tokens::semibold(12.0), Color32::WHITE);
        app.auto.add("text.transcript.generate", r, "Transcribe");
        if resp.clicked() {
            actions.push(("transcript.generate".into(), json!({})));
        }
        run(app, ui, actions);
        return;
    }
    // cut spans with their crossed-out words (media transcript indices)
    let cuts: Vec<CutView> = spans
        .iter()
        .enumerate()
        .map(|(index, c)| {
            let words = app
                .session
                .project
                .transcripts
                .get(&c.item)
                .and_then(|tr| tr.words.get(c.words.clone()))
                .map(|ws| ws.iter().map(|w| w.text.clone()).collect())
                .unwrap_or_default();
            CutView { index, item: c.item.0, media_words: c.words.clone(), words, seconds: c.media.duration.seconds() }
        })
        .collect();
    // where each span goes: after a live word, or before the first word
    let mut cuts_after: std::collections::HashMap<usize, Vec<usize>> = Default::default();
    let mut leading: Vec<usize> = Vec::new();
    for (ci, c) in spans.iter().enumerate() {
        match c.after_word.filter(|i| *i < words.len()) {
            Some(i) => cuts_after.entry(i).or_default().push(ci),
            None => leading.push(ci),
        }
    }
    // take groups: which live word belongs to which take, and where each group's chip goes
    let groups = parse_groups(&filmcraft_engine::takes::groups_json(&app.session, None, None));
    let take_of_word: Vec<Option<(usize, usize)>> = words
        .iter()
        .map(|w| {
            groups
                .iter()
                .enumerate()
                .filter(|(_, g)| g.item == w.item.0)
                .find_map(|(gi, g)| g.takes.iter().find(|k| k.words.0 <= w.index && w.index < k.words.1).map(|k| (gi, k.index)))
        })
        .collect();
    let mut chip_word: std::collections::HashMap<usize, Vec<usize>> = Default::default();
    let mut chip_cut: std::collections::HashMap<usize, Vec<usize>> = Default::default();
    for (gi, g) in groups.iter().enumerate() {
        let live = g
            .active
            .and_then(|a| take_of_word.iter().position(|x| *x == Some((gi, a))))
            .or_else(|| take_of_word.iter().position(|x| x.is_some_and(|(xg, _)| xg == gi)));
        if let Some(i) = live {
            chip_word.entry(i).or_default().push(gi);
        } else if let Some(ci) =
            cuts.iter().position(|c| c.item == g.item && g.takes.iter().any(|k| c.media_words.start < k.words.1 && k.words.0 < c.media_words.end))
        {
            chip_cut.entry(ci).or_default().push(gi);
        }
    }

    let sel = app.ui.transcript_sel.filter(|(a, b)| *a < words.len() && *b < words.len());
    let (sa, sb) = sel.map(|(a, b)| (a.min(b), a.max(b))).unzip();

    // ---- toolbar: search, extract / lift selection, remove fillers / pauses, captions, takes
    let bar = Rect::from_min_size(rect.min + vec2(10.0, 2.0), vec2(rect.width() - 20.0, 26.0));
    let sw = 170.0f32.min(bar.width() * 0.4);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(bar.min, vec2(sw, 24.0))));
    let sresp = crate::widgets::search_field(&mut child, &mut app.ui.transcript_search, "Search", sw, &t);
    app.auto.add("text.transcript.search", sresp.rect, "Search transcript");
    let hits: Vec<std::ops::Range<usize>> = filmcraft_edit::transcript::search(&words, &app.ui.transcript_search);
    let mut x = bar.min.x + sw + 10.0;
    let range = sel.map(|(a, b)| json!({"from": a.min(b), "to": a.max(b)}));
    let has_words = !words.is_empty();
    let tools: [(Icon, &str, &str, bool, &str, Value); 8] = [
        (Icon::Razor, "text.transcript.extract", "Extract selected text", sel.is_some(), "transcript.extract", range.clone().unwrap_or_default()),
        (Icon::Trash, "text.transcript.lift", "Lift selected text", sel.is_some(), "transcript.lift", range.unwrap_or_default()),
        (Icon::Link, "text.transcript.removeFillers", "Remove filler words", has_words, "transcript.removeFillers", json!({})),
        // no thresholds: `menus::invoke` opens the Remove Pauses dialog
        (Icon::Pause, "text.transcript.removePauses", "Remove pauses…", has_words, "transcript.removePauses", json!({})),
        (Icon::Captions, "text.transcript.createCaptions", "Create captions", has_words, "transcript.createCaptions", json!({})),
        (Icon::Sparkle, "text.transcript.detectTakes", "Detect takes", has_words, "takes.detect", json!({})),
        (Icon::Undo, "text.transcript.restoreAll", "Restore all crossed-out text", !cuts.is_empty(), "transcript.restoreAll", json!({})),
        (Icon::ListView, "text.transcript.takes", "Show takes", true, "", json!({})),
    ];
    for (icon, id, label, enabled, cmd, params) in tools {
        let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(24.0, 24.0));
        if r.max.x > rect.max.x {
            break;
        }
        if cmd.is_empty() && app.ui.transcript_show_takes {
            ui.painter().rect_filled(r, 3.0, t.row_selected);
        }
        if tool_button(app, ui, r, icon, id, label, enabled) {
            if cmd.is_empty() {
                app.ui.transcript_show_takes = !app.ui.transcript_show_takes;
            } else if cmd == "transcript.restoreAll" {
                // highest index first: restoring a span never renumbers the ones before it
                for ci in (0..cuts.len()).rev() {
                    actions.push(("transcript.restore".into(), json!({"cut": ci})));
                }
            } else {
                if cmd.ends_with("extract") || cmd.ends_with("lift") {
                    app.ui.transcript_sel = None;
                }
                actions.push((cmd.into(), params));
            }
        }
        x += 28.0;
    }

    // ---- paragraphs of words (and the Takes list below them when shown)
    let full = Rect::from_min_max(pos2(rect.min.x + 6.0, bar.max.y + 8.0), pos2(rect.max.x - 6.0, rect.max.y - 4.0));
    let (list, takes_rect) = if app.ui.transcript_show_takes {
        let split = full.max.y - full.height() * 0.4;
        (Rect::from_min_max(full.min, pos2(full.max.x, split - 3.0)), Some(Rect::from_min_max(pos2(full.min.x, split + 3.0), full.max)))
    } else {
        (full, None)
    };
    ui.painter().rect_filled(list, 3.0, t.app_bg);
    let ph = app.session.playhead();
    let current = filmcraft_edit::transcript::word_at(&words, ph);
    let paras = filmcraft_edit::transcript::paragraphs(&words, filmcraft_time::Tick::from_seconds_f64(1.5));
    let rate = app.session.sequence_rate();
    let df = app.session.active_sequence().is_some_and(|q| q.settings.drop_frame);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list.shrink(6.0)).id_salt("transcript-list"));
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("transcript-scroll").show(&mut child, |ui| {
        ui.set_width(list.width() - 16.0);
        // chips of the groups anchored on a span, then the span
        let span = |app: &mut FilmcraftApp, ui: &mut egui::Ui, ci: usize, actions: &mut Vec<(String, Value)>| {
            for gi in chip_cut.get(&ci).into_iter().flatten() {
                if let Some(g) = groups.get(*gi) {
                    take_chip(app, ui, g, actions);
                }
            }
            if let Some(c) = cuts.get(ci) {
                cut_span(app, ui, c, actions);
            }
        };
        if paras.is_empty() && !leading.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 3.0);
                for ci in &leading {
                    span(app, ui, *ci, &mut actions);
                }
            });
        }
        for (pi, pr) in paras.iter().enumerate() {
            let Some(w0) = words.get(pr.start) else { continue };
            let head = format!("{}  {}", w0.speaker.as_deref().unwrap_or("Speaker"), format_time(w0.start, rate, df, TimeDisplay::Timecode, 48_000));
            let hr = ui
                .horizontal(|ui| {
                    crate::panels::scenes::chip(app, ui, pi, pr.clone(), &words, &mut actions);
                    ui.label(egui::RichText::new(head).size(11.0).color(t.text_dim).strong())
                })
                .inner;
            app.auto.add(&format!("text.transcript.paragraph.{pi}"), hr.rect, "Paragraph");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 3.0);
                if pi == 0 {
                    for ci in &leading {
                        span(app, ui, *ci, &mut actions);
                    }
                }
                for i in pr.clone() {
                    let Some(w) = words.get(i) else { continue };
                    for gi in chip_word.get(&i).into_iter().flatten() {
                        if let Some(g) = groups.get(*gi) {
                            take_chip(app, ui, g, &mut actions);
                        }
                    }
                    let in_sel = sa.is_some_and(|a| i >= a) && sb.is_some_and(|b| i <= b);
                    let hit = hits.iter().any(|h| h.contains(&i));
                    let mut text = egui::RichText::new(&w.text).size(13.0).color(if Some(i) == current { t.hot_text } else { t.text });
                    if in_sel {
                        text = text.background_color(t.row_selected);
                    } else if hit {
                        text = text.background_color(t.hover);
                    }
                    let resp = ui.add(egui::Label::new(text).sense(Sense::click_and_drag()));
                    if take_of_word.get(i).copied().flatten().is_some() {
                        let r = resp.rect;
                        ui.painter().line_segment([pos2(r.min.x, r.max.y), pos2(r.max.x, r.max.y)], Stroke::new(1.0, t.accent));
                    }
                    app.auto.add(&format!("text.transcript.word.{i}"), resp.rect, &w.text);
                    // Selecting words: click selects one, Shift+click extends from the anchor, and
                    // dragging across words selects the range under the pointer (like text).
                    if resp.drag_started() {
                        app.ui.transcript_drag = Some(i);
                        app.ui.transcript_sel = Some((i, i));
                    }
                    if let Some(anchor) = app.ui.transcript_drag
                        && let Some(pos) = ui.input(|inp| inp.pointer.interact_pos())
                        && resp.rect.expand2(vec2(2.0, 1.5)).contains(pos)
                    {
                        app.ui.transcript_sel = Some((anchor, i));
                    }
                    if resp.clicked() {
                        // the modifier state, or the Shift carried by the click event itself
                        let shift = ui.input(|inp| {
                            inp.modifiers.shift || inp.events.iter().any(|e| matches!(e, egui::Event::PointerButton { modifiers, .. } if modifiers.shift))
                        });
                        app.ui.transcript_sel = Some(match (shift, sel) {
                            (true, Some((a, _))) => (a, i),
                            _ => (i, i),
                        });
                        let (a, b) = app.ui.transcript_sel.unwrap_or((i, i));
                        actions.push(("transcript.select".into(), json!({"from": a.min(b), "to": a.max(b)})));
                    }
                    for ci in cuts_after.get(&i).into_iter().flatten() {
                        span(app, ui, *ci, &mut actions);
                    }
                }
            });
            ui.add_space(8.0);
        }
    });
    if app.ui.transcript_drag.is_some() && ui.input(|inp| !inp.pointer.any_down()) {
        app.ui.transcript_drag = None;
        if let Some((a, b)) = app.ui.transcript_sel {
            actions.push(("transcript.select".into(), json!({"from": a.min(b), "to": a.max(b)})));
        }
    }
    if let Some(r) = takes_rect {
        let tc = |at: i64| format_time(filmcraft_time::Tick(at), rate, df, TimeDisplay::Timecode, 48_000);
        takes_list(app, ui, r, &groups, &tc, &mut actions);
    }
    run(app, ui, actions);
}

fn filmcraft_speech_available(app: &FilmcraftApp) -> bool {
    filmcraft_engine::transcript::can_transcribe(&app.session).is_ok()
}

fn style_strip(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, track_idx: usize, actions: &mut Vec<(String, Value)>) {
    let Some(tr) = app.session.active_sequence().and_then(|q| q.caption_tracks.get(track_idx)).cloned() else { return };
    let st = tr.style.clone();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).id_salt("caption-style"));
    child.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let mut size = st.size;
        let resp = ui.add(egui::DragValue::new(&mut size).range(8.0..=200.0).speed(0.5).suffix(" px"));
        app.auto.add("text.captions.style.size", resp.rect, "Caption size");
        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
            actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "size": size})));
        }
        let mut col = Color32::from_rgba_unmultiplied(st.color[0], st.color[1], st.color[2], st.color[3]);
        let resp = egui::color_picker::color_edit_button_srgba(ui, &mut col, egui::color_picker::Alpha::Opaque);
        app.auto.add("text.captions.style.color", resp.rect, "Text colour");
        if resp.changed() {
            let c = col.to_srgba_unmultiplied();
            actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "color": c})));
        }
        let mut bg = st.background;
        let resp = ui.checkbox(&mut bg, "Box");
        app.auto.add("text.captions.style.background", resp.rect, "Background box");
        if resp.changed() {
            actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "background": bg})));
        }
        ui.label(egui::RichText::new("Align").size(11.0).color(app.tokens.text_dim));
        for (a, icon_txt, name) in [(CaptionAlign::Left, "L", "left"), (CaptionAlign::Center, "C", "center"), (CaptionAlign::Right, "R", "right")] {
            let resp = ui.selectable_label(st.align == a, icon_txt).on_hover_text(format!("Align {name}"));
            app.auto.add(&format!("text.captions.style.align.{name}"), resp.rect, name);
            if resp.clicked() {
                actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "align": name})));
            }
        }
        ui.label(egui::RichText::new("Position").size(11.0).color(app.tokens.text_dim));
        for (a, name) in [(CaptionAnchor::Top, "top"), (CaptionAnchor::Middle, "middle"), (CaptionAnchor::Bottom, "bottom")] {
            let resp = ui.selectable_label(st.anchor == a, name[..1].to_uppercase()).on_hover_text(format!("Position: {name}"));
            app.auto.add(&format!("text.captions.style.anchor.{name}"), resp.rect, name);
            if resp.clicked() {
                actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "anchor": name})));
            }
        }
    });
}

/// The empty state while a transcription runs: a spinner, the job's status, a progress bar and
/// Cancel (`text.transcript.cancel`).
fn transcribing(app: &mut FilmcraftApp, ui: &mut egui::Ui, c: egui::Pos2, job: &filmcraft_engine::Job, t: &Tokens) {
    use std::sync::atomic::Ordering;
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    let spin = Rect::from_center_size(c - vec2(0.0, 70.0), vec2(36.0, 36.0));
    ui.put(spin, egui::Spinner::new().size(36.0).color(t.accent));
    ui.painter().text(c - vec2(0.0, 30.0), Align2::CENTER_CENTER, &job.label, Tokens::semibold(16.0), t.text);
    let status = job.progress.status.lock().map(|s| s.clone()).unwrap_or_default();
    ui.painter().text(c - vec2(0.0, 8.0), Align2::CENTER_CENTER, status, Tokens::ui(12.0), t.text_dim);
    let f = job.progress.fraction().clamp(0.0, 1.0);
    let bar = Rect::from_center_size(c + vec2(0.0, 12.0), vec2(240.0, 6.0));
    ui.painter().rect_filled(bar, 3.0, t.separator);
    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * f, bar.height())), 3.0, t.accent);
    if let Some(left) = job.progress.eta().map(crate::panels::left_text) {
        ui.painter().text(c + vec2(0.0, 28.0), Align2::CENTER_CENTER, left, Tokens::ui(11.0), t.text_dim);
    }
    let r = Rect::from_center_size(c + vec2(0.0, 52.0), vec2(90.0, 24.0));
    let resp = ui.interact(r, egui::Id::new("text.transcript.cancel"), Sense::click());
    ui.painter().rect_stroke(r, 12.0, Stroke::new(1.0, if resp.hovered() { t.text } else { t.separator }), egui::StrokeKind::Inside);
    ui.painter().text(r.center(), Align2::CENTER_CENTER, "Cancel", Tokens::ui(12.0), t.text);
    app.auto.add("text.transcript.cancel", r, "Cancel transcription");
    if resp.clicked() {
        job.progress.cancel.store(true, Ordering::Relaxed);
    }
}

/// Remove Pauses dialog limits (seconds): the minimum pause and the length each pause keeps.
const PAUSE_MIN: std::ops::RangeInclusive<f64> = 0.1..=10.0;
const PAUSE_KEEP: std::ops::RangeInclusive<f64> = 0.0..=2.0;

/// The Remove Pauses values in range: minimum 0.1–10 s, kept 0–2 s and never above the minimum
/// (NaN falls back to the defaults).
pub fn clamp_pauses(min: f64, keep: f64) -> (f64, f64) {
    let min = if min.is_nan() { 1.0 } else { min.clamp(*PAUSE_MIN.start(), *PAUSE_MIN.end()) };
    let keep = if keep.is_nan() { 0.15 } else { keep.clamp(*PAUSE_KEEP.start(), *PAUSE_KEEP.end()) };
    (min, keep.min(min))
}

/// Open the Remove Pauses dialog (Text panel button, Sequence ▸ Transcript ▸ Remove Pauses).
pub fn open_pauses_dialog(app: &mut FilmcraftApp) -> Result<Value, String> {
    if let Some(c) = filmcraft_engine::find_command("transcript.removePauses")
        && let Err(e) = (c.enabled)(&app.session)
    {
        app.ui.status = e.clone();
        return Err(e);
    }
    app.ui.transcript_pause_dialog = true;
    Ok(json!({"dialog": "text.pauses"}))
}

/// The Remove Pauses dialog: pauses longer than the minimum are shortened to the kept length, with
/// a live count from `transcript.pauses`. Apply runs `transcript.removePauses` (one undo step).
pub fn pauses_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if !app.ui.transcript_pause_dialog {
        return;
    }
    let (mut min, mut keep) = clamp_pauses(app.ui.transcript_pause_min, app.ui.transcript_pause_keep);
    let (count, seconds, error) = match app.session.execute("transcript.pauses", json!({"minSeconds": min, "keepSeconds": keep})) {
        Ok(v) => (v["count"].as_u64().unwrap_or(0), v["seconds"].as_f64().unwrap_or(0.0), None),
        Err(e) => (0, 0.0, Some(e.to_string())),
    };
    let line = format!("{count} {}, {seconds:.1} s", if count == 1 { "pause" } else { "pauses" });
    let mut elems: Vec<(&str, Rect, String)> = Vec::new();
    let mut action: Option<bool> = None;
    egui::Window::new("Remove Pauses")
        .id(egui::Id::new("text-pauses-dialog"))
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::Grid::new("text-pauses-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Pauses longer than:");
                let r = ui.add(egui::DragValue::new(&mut min).range(PAUSE_MIN).speed(0.05).suffix(" s").max_decimals(2));
                elems.push(("text.pauses.min", r.rect, "Minimum pause".into()));
                ui.end_row();
                ui.label("Shorten to:");
                let r = ui.add(egui::DragValue::new(&mut keep).range(PAUSE_KEEP).speed(0.05).suffix(" s").max_decimals(2));
                elems.push(("text.pauses.keep", r.rect, "Pause length to keep".into()));
                ui.end_row();
            });
            let r = ui.label(egui::RichText::new(&line).strong());
            elems.push(("text.pauses.count", r.rect, line.clone()));
            if let Some(e) = &error {
                ui.colored_label(Color32::from_rgb(0xe0, 0x60, 0x60), e);
            }
            ui.label(egui::RichText::new("Pauses longer than the minimum are shortened to the kept length. They show crossed out and can be restored.").weak());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r = ui.button("Cancel");
                elems.push(("text.pauses.cancel", r.rect, "Cancel".into()));
                if r.clicked() {
                    action = Some(false);
                }
                let r = ui.add_enabled(count > 0, egui::Button::new("Apply"));
                elems.push(("text.pauses.apply", r.rect, if count > 0 { "Apply".into() } else { "Apply (no pauses)".into() }));
                if r.clicked() && count > 0 {
                    action = Some(true);
                }
            });
        });
    for (id, r, label) in elems {
        app.auto.add(id, r, &label);
    }
    let (min, keep) = clamp_pauses(min, keep);
    app.ui.transcript_pause_min = min;
    app.ui.transcript_pause_keep = keep;
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        action = Some(false);
    }
    match action {
        Some(false) => app.ui.transcript_pause_dialog = false,
        Some(true) => {
            app.ui.transcript_pause_dialog = false;
            if let Err(e) = crate::menus::invoke(app, ctx, "transcript.removePauses", json!({"minSeconds": min, "keepSeconds": keep})) {
                app.ui.status = e;
            }
        }
        None => {}
    }
}

fn run(app: &mut FilmcraftApp, ui: &egui::Ui, actions: Vec<(String, Value)>) {
    let ctx = ui.ctx().clone();
    for (cmd, p) in actions {
        if let Err(e) = crate::menus::invoke(app, &ctx, &cmd, p) {
            app.ui.status = e;
        }
    }
}
