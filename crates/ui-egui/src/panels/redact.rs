//! Tracked redaction: draw a box, get a tracked mosaic in the Program monitor and the clip menus. See
//! `openspec/changes/clip-layouts/design.md` §5 and `docs/layouts.md`.

use egui::Rect;

use crate::FilmcraftApp;

/// Drawn over the Program picture after the graphics and mask overlays.
pub fn monitor_overlay(_app: &mut FilmcraftApp, _ui: &mut egui::Ui, _pic: Rect, _frame: (u32, u32)) {}

/// Entries for the timeline clip context menu (after the standard groups).
pub fn clip_menu(_app: &mut FilmcraftApp, _ui: &mut egui::Ui) {}
