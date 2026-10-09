## Why

A creator who restates a line several times while recording has no way, in FilmCraft's Text panel, to see what was dropped, hear the alternatives in context, and switch between them. Extract and lift make words vanish from the transcript; filler and pause removal do the same. Descript shows removed words crossed out and restorable, which is the workflow the owner of this fork edits with today and wants inside one editor. The released macOS build also has no transcription at all (the `whisper` feature is compiled out), so the Text panel is a dead end without a transcript brought from outside.

## What Changes

- Transcription works in release builds through the user's own whisper.cpp command and model, chosen in Settings (shipped as `speech: whisper.cpp engine from Settings`).
- **Crossed-out text.** Media that a clip used to play but no longer does (the gap between two clips of the same media on an audio track) is shown in the Transcript tab as crossed-out words at the place it was removed, and can be restored with one action. Extract, lift, Remove Pauses and Remove Filler Words therefore become non-destructive in the Text panel without changing how they edit the timeline.
- **Take groups.** FilmCraft detects lines that were said more than once and bundles the passes into a take group stored on the media transcript. The group shows which take is in the cut, lets the user cycle to another take (one undo step), label takes, mark a line for re-recording, and fix wrong groupings (merge, split, add, remove).
- **Commands first.** Everything is an engine command (`transcript.cuts`, `transcript.restore`, `takes.*`) so the CLI, control channel and MCP server can drive it; the Text panel is one client.
- Project schema bumps to v13 (no-op migration) so builds that do not know take data refuse the file rather than silently dropping it on save.

Out of scope for this change: recording, noise cleanup, camera-and-screen layouts, caption styling.

## Capabilities

### New Capabilities
- `whisper-cpp-engine`: run the user's whisper.cpp command and ggml model as the speech recogniser, from Settings, in every native build.
- `transcript-cuts`: removed media shown as crossed-out, restorable words in the sequence transcript.
- `take-groups`: detection, storage, switching, labelling and manual correction of repeated lines.

### Modified Capabilities
(none: existing transcript commands keep their behaviour; the new commands sit beside them)

## Impact

- `crates/project` (`Transcript` gains `takes`), `crates/format` (schema v13), `crates/edit` (`transcript::cut_spans`, `transcript::restore_cut`, new `takes` module), `crates/engine` (`transcript.cuts`, `transcript.restore`, `takes.*` commands), `crates/speech` (`external` module), `crates/ui-egui` (Text panel), `docs/transcripts.md`.
- No new dependencies. No network. Nothing leaves the machine.
