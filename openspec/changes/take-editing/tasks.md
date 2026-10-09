## 1. Transcription (shipped)

- [x] 1.1 `crates/speech/src/external.rs`: whisper.cpp `Transcriber` (temp WAV, `-ojf -ml 1 -sow`, poll/cancel/timeout, hostile JSON parse, tests)
- [x] 1.2 Preferences `mediaAnalysis.speechEngine/whisperCppCommand/whisperCppModel/whisperCppArgs`; engine builds the recogniser from them; `transcript.models` reports `whisperCpp.ready`
- [x] 1.3 Text panel availability check follows `can_transcribe`; `docs/transcripts.md` section
- [x] 1.4 End-to-end: release build transcribes a clip with `ggml-large-v3-turbo` headlessly

## 2. Data model

- [ ] 2.1 `Transcript::takes`, `TakeGroup`, `Take`, `TakeLabel` with normalisation and checks, unit tests (`crates/project/src/transcript.rs`)
- [ ] 2.2 Schema v13: `v12_to_v13` no-op, `v12-minimal.fcproj` fixture, load test (`crates/format`); `docs/project-files.md` version note

## 3. Edit algebra

- [ ] 3.1 `cut_spans`, `live_ranges` in `crates/edit/src/transcript.rs` with tests on the interview fixture
- [ ] 3.2 `restore_media` / `restore_cut` (grow, lengthen-or-split, shift, insert fallback, captions) with tests incl. an extract/restore round-trip property
- [ ] 3.3 `crates/edit/src/takes.rs`: `utterances`, `similarity`, `detect`, `merge`, `split`, `live_fraction` with tests

## 4. Engine commands

- [ ] 4.1 `transcript.cuts`, `transcript.restore` (+ tests in `transcript_tests.rs`)
- [ ] 4.2 `crates/engine/src/takes.rs`: `takes.detect/list/select/next/previous/cross/restore/label/redo/merge/split/add/remove/preview`, registered in `commands.rs` with shortcuts and menu path
- [ ] 4.3 `crates/engine/src/takes_tests.rs`: happy paths with undo/redo, hostile parameters for every command
- [ ] 4.4 Docs: `docs/transcripts.md` (cuts, takes), `docs/keyboard.md`, `docs/control-protocol.md` command table

## 5. Text panel

- [ ] 5.1 Crossed-out cut spans inline (click restores), wordless pauses, automation ids
- [ ] 5.2 Take chips and underline on live takes; `[` `]` cycling
- [ ] 5.3 Takes list with preview, select, label, cross/restore, redo filter
- [ ] 5.4 Toolbar: Detect Takes, Restore All Cuts

## 6. Acceptance (owner)

- [ ] 6.1 Real 5–10 min recording: transcribe (whisper.cpp), detect takes, ≥ 80 % grouped right, fix the rest in the UI
- [ ] 6.2 Cross out / restore / cycle with undo per step; filler and pause cleanup restorable
- [ ] 6.3 Save, reopen; v12 project still opens
- [ ] 6.4 FCPXML/OTIO export opens in Resolve (markers for labels: later change)
