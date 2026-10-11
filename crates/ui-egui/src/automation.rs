//! Automation registry: every interactive element registers a stable id and its screen rect each
//! frame, so agents can click/drag by id (`ui.click {id:"timeline.track.V1.lock"}`) and
//! `ui.inspect` can report the full on-screen widget tree.
//!
//! An element of another native window (a pop-out camera preview) carries `window` (the id of
//! that window's own element) and its rect is in that window's coordinates: `ui.click` and
//! `ui.move` by id send their input to that window.

use egui::Rect;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Element {
    pub id: String,
    pub label: String,
    pub rect: [f32; 4],
    /// The element is in another native window: that window's element id. `rect` is then in
    /// that window's own coordinates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// That window's viewport: where input for the element goes (None: the main window).
    #[serde(skip)]
    pub viewport: Option<egui::ViewportId>,
}

#[derive(Default)]
pub struct Registry {
    pub elements: Vec<Element>,
    /// Last frame's elements (for lookups from control handlers between frames).
    pub previous: Vec<Element>,
}

impl Registry {
    pub fn add(&mut self, id: &str, rect: Rect, label: &str) {
        self.elements.push(Element {
            id: id.to_string(),
            label: label.to_string(),
            rect: [rect.min.x, rect.min.y, rect.width(), rect.height()],
            window: None,
            viewport: None,
        });
    }
    /// An element of another native window (`window`: that window's element id; `rect` in the
    /// window's own coordinates).
    pub fn add_in(&mut self, id: &str, rect: Rect, label: &str, window: &str, viewport: egui::ViewportId) {
        self.elements.push(Element {
            id: id.to_string(),
            label: label.to_string(),
            rect: [rect.min.x, rect.min.y, rect.width(), rect.height()],
            window: Some(window.to_string()),
            viewport: Some(viewport),
        });
    }
    pub fn begin_frame(&mut self) {
        self.previous = std::mem::take(&mut self.elements);
    }
    pub fn find(&self, id: &str) -> Option<&Element> {
        self.previous.iter().chain(self.elements.iter()).find(|e| e.id == id)
    }
    /// Elements whose id starts with a prefix.
    pub fn query(&self, prefix: &str) -> Vec<&Element> {
        self.previous.iter().filter(|e| e.id.starts_with(prefix)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    #[test]
    fn an_element_of_another_window_says_which_and_keeps_its_viewport_to_itself() {
        let mut r = Registry::default();
        let rect = Rect::from_min_size(pos2(6.0, 186.5), vec2(40.0, 15.0));
        let popout = egui::ViewportId::from_hash_of("a pop-out");
        r.add("record.panel.record", rect, "Stop");
        r.add_in("record.preview.1.record", rect, "Stop", "record.preview.1", popout);
        r.begin_frame();
        let v = serde_json::to_value(r.query("record.")).unwrap();
        assert_eq!(
            v[0],
            serde_json::json!({"id": "record.panel.record", "label": "Stop", "rect": [6.0, 186.5, 40.0, 15.0]}),
            "a main-window element is as before"
        );
        assert_eq!(v[1], serde_json::json!({"id": "record.preview.1.record", "label": "Stop", "rect": [6.0, 186.5, 40.0, 15.0], "window": "record.preview.1"}));
        assert_eq!(r.find("record.panel.record").unwrap().viewport, None);
        assert_eq!(r.find("record.preview.1.record").unwrap().viewport, Some(popout));
    }
}
