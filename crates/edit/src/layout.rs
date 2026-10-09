//! Clip layout geometry: where a clip's visible box sits in the frame (place), its shape mask, and
//! the Motion position / scale that put it there. Pure functions; the engine's `layout.*` commands
//! (`crates/engine/src/layout.rs`) and the Program monitor handles use them.
//! See `openspec/changes/clip-layouts/design.md` §1–2 and `docs/layouts.md`.
