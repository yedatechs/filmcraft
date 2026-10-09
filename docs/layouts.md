# Clip layouts, scenes and redaction

Status: in progress (`openspec/changes/clip-layouts/`). This page is filled in as the pieces land.

## Layouts (`layout.*`)

One-click placement and shape for the video clips of a sequence: put the camera in a corner as a
circle over the screen recording, swap the two, or make one of them fill the frame, from the
Program monitor's right-click menu, the timeline clip menu, Clip ▸ Layout, or Effect Controls.
Every layout is an ordinary Motion edit plus an Opacity mask named `Layout shape`, so Effect
Controls shows exactly what changed and undo takes it back in one step.

## Scenes (`scenes.*`)

A layout attached to a span of the transcript, so the arrangement follows the words when takes change.

## Tracked redaction (`redact.*`)

Draw a box over what to hide; a mosaic with a rectangle mask tracks it across the clip.
