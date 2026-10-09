# Clip layouts, tracked redaction, pause threshold

## Why

The owner records a screen and a camera at once and edits by text. Today, putting the face in a
corner as a circle over the screen recording means typing Motion numbers in Effect Controls,
adding a mask by hand, then Paste Attributes onto every other clip: the Premiere routine he left
Descript to avoid. Hiding an API key on screen means drawing a mask and moving it frame by frame.
Removing silences has a threshold in the engine but no way to set it in the app.

## What changes

1. **Layouts** (`layout.*`): one-click placement (corners, sides, centre, full), shape (circle,
   rounded, square, free) and swap for the video clips of a sequence, as engine commands with
   buttons in the Program monitor's right-click menu, the timeline clip menu, the Clip menu and
   Effect Controls. The Program monitor selects a video clip on click and shows move / scale
   handles, like it does for graphics.
2. **Scenes** (`scenes.*`): a layout attached to a span of the transcript, so the camera-and-screen
   arrangement follows the words when takes change; a strip above the transcript switches them.
3. **Tracked redaction** (`redact.*`): draw a box in the Program monitor, get a mosaic (or blur)
   with a rectangle mask that is already tracking forward and backward across the clip.
4. **Pause threshold**: Remove Pauses gets a dialog (minimum and kept length, live count) in the
   Text panel; the removed pauses show crossed out and restore like every other cut.

Out of scope: recording (screen / camera capture), click-driven auto zoom, animated transitions
between layouts, a scroll-following tracker (listed as follow-ups in `design.md`).

## Impact

- New engine modules `layout.rs`, `redact.rs`, `scenes.rs`; `transcript.pauses` (read-only
  preview). Project schema: scenes are stored on the sequence (v14).
- UI: `panels/layout.rs` (monitor handles, menus, Effect Controls row), `panels/redact.rs`,
  Text panel pause dialog and scene strip.
- Docs: `docs/layouts.md` (new), `docs/transcripts.md`, `docs/monitors.md`, `docs/keyboard.md`.
