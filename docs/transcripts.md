# Transcripts and text-based editing

FilmCraft's Text panel ▸ **Transcript** tab shows the dialogue of the open sequence as text. Select
words to mark In/Out, then extract or lift them; remove filler words and long pauses in one step;
turn the transcript into captions. Every action is an engine command (`transcript.*`), so the CLI,
the control channel and MCP agents can do the same.

## Model

- A **transcript** belongs to a media item (`Project::transcripts`, saved in the `.fcproj` since
  schema v9). It lists **words** with media-time bounds (`Tick`s), an optional speaker index and a
  confidence, plus the speaker names and the language. Because the times are media time, the
  transcript stays valid however the clip is trimmed, moved, sped up or reused.
- The **sequence transcript** is derived, never stored (`filmcraft_edit::transcript::sequence_words`):
  audio tracks are read top first; a word is heard through the first enabled clip whose range
  covers the word's midpoint (duplicates of the same dialogue on lower tracks read once); disabled,
  reversed and frame-hold clips contribute nothing.
- Speaker names come from the clip transcripts, so renaming "Speaker 1" in every transcript renames
  it across the sequence.

## Commands

| Command | What it does |
|---|---|
| `transcript.generate` | Transcribe media items (`items`, else the Project selection, else the media of the sequence's audio clips). Params: `model` (default `whisper-base`), `language` (`auto` = detect), `diarize`, `maxSpeakers`. One undo step. |
| `transcript.set` | Store a transcript you bring (JSON: `language`, `speakers`, `words` with `text`/`start`/`end`/`speaker`); it is sorted and made well formed. |
| `transcript.delete` | Remove transcripts. |
| `transcript.inspect` | The sequence transcript: words (index, text, sequence times, speaker, clip), paragraphs, speakers, the word at the playhead. |
| `transcript.search` | Word-index ranges matching a phrase (case and punctuation ignored; the last word may be a prefix). |
| `transcript.select` | Mark In/Out around words `from..=to` (frame-snapped outward) and move the playhead there. |
| `transcript.extract` / `transcript.lift` | Extract (ripple) or lift the words' frames on the targeted tracks. |
| `transcript.renameSpeaker` | Rename a speaker by name (every transcript) or by index in one `item`. |
| `transcript.removeFillers` | Ripple-delete filler words (`fillers`, default um/uh/erm/…; phrases such as "you know" allowed). |
| `transcript.removePauses` | Ripple-delete pauses longer than `minSeconds`, keeping `keepSeconds` of air on both sides. |
| `transcript.createCaptions` | Lay the words out as captions on a new caption track (`maxChars`, `lines`, `minSeconds`, `maxSeconds`, `gapFrames`). |
| `transcript.models` / `transcript.downloadModel` | List the speech models (size, licence, installed) / download one. |

## Crossed-out text and takes

Nothing a text edit removes is lost. Media that a clip used to play but no longer does (the
stretch between two clips of the same media on an audio track, which is what Extract, Remove
Pauses, Remove Filler Words and a crossed-out take leave behind) is a **cut span**
(`filmcraft_edit::transcript::cut_spans`): derived from the timeline like the sequence transcript,
never stored. The Text panel shows a cut span's words struck through where they were, a wordless
span as a struck-through pause, and one click puts the media back. Head and tail trims are not
spans (they would list a whole source file).

| Command | What it does |
|---|---|
| `transcript.cuts` | The sequence's cut spans: `index`, `item`, media `start`/`end`, `at` (where it goes back), `track`, `afterWord` (the live word it follows), `words`. |
| `transcript.restore` | Put a span back (`cut` index, or `item` + media `start`/`end`): the clip before it grows, everything from `at` moves right on every unlocked track, a clip on another track spanning `at` is lengthened when its media allows (the inverse of Extract) and otherwise split; linked video grows with the audio. One undo step. |

**Takes** are lines the speaker recorded more than once. `takes.detect` splits a media
transcript into utterances (at pauses or sentence punctuation), compares neighbours by
normalised-word similarity (shared opening words, token overlap, edit distance; retake cues such
as "okay again" help) and stores the matches as **take groups** on the transcript
(`Transcript::takes`, media time, schema v13). Which take is *in the cut* is not stored: a take is
**live** when the sequence plays at least half of its media, and switching takes is an ordinary
timeline edit (the live takes are extracted, the chosen take's media is put back at the first of
their positions), so undo, redo and export need nothing new, and the other takes show crossed out.

| Command | What it does |
|---|---|
| `takes.detect` | Find groups (`sensitivity` 0..1, default 0.5; `maxGapSeconds`); hand-made groups are kept; `select` (`last` by default, `first`, `none`) puts that take of each new group in the cut, in the same undo step. |
| `takes.list` | Groups of the sequence (`label`, `redo` filters): takes with `text`, `live`, `label`, `note`; the group's `active` take and `at`. |
| `takes.select` / `takes.next` / `takes.previous` | Make one take the only live one; next/previous take the group at the playhead when `group` is omitted and wrap around. |
| `takes.cross` / `takes.restore` | Cross out a live take; bring a take back without touching the others. |
| `takes.label` / `takes.redo` | Label a take (`good`, `best`, `flat`, `stumble`, `wrongEnergy`) and note it; flag a group "needs re-record" (the redo list is `takes.list {redo: true}`). |
| `takes.merge` / `takes.split` / `takes.add` / `takes.remove` | Fix a grouping by hand (`add` takes live word indices `from`/`to`, or `item` + media `start`/`end`); results are marked manual. |
| `takes.preview` | Mark In/Out from the sentence before a live take to the sentence after (`pre`, `post`), so Play In to Out auditions it in context. |

Shortcuts (Sequence ▸ Takes): Detect Takes `Alt+Shift+T`, Next/Previous Take `Alt+]` / `Alt+[`,
Cross Out `Alt+Shift+X`, Restore `Alt+Shift+U`, Label `Alt+Shift+L`, Re-record `Alt+Shift+R`.

## Speech recognition

Recognition goes through the `Transcriber` trait (`crates/speech`). The built-in recogniser is
OpenAI's Whisper, run in pure Rust on [candle](https://github.com/huggingface/candle) on the CPU,
with timestamp decoding, language detection and word times from cross-attention alignment (see the
`filmcraft_speech::whisper` module docs). Speakers are labelled by clustering per-chunk MFCC
statistics (`filmcraft_speech::diarize`); no model is involved.

Both are **optional features**, off by default and never built for the web:

- `whisper` (on `filmcraft-speech`, `filmcraft-engine`, and the `filmcraft` / `filmcraft-cli`
  apps, where it also enables downloads): candle inference.
- `download` (`speech-download` on the engine): HTTPS downloads with rustls + RustCrypto and the
  operating system's certificate verifier.

Without `whisper`, and with no recogniser installed, `transcript.generate` and Transcribe Sequence
are disabled, with "speech-to-text is not available in this build" as the reason (`describe`,
`command_list {"enabled_only": true}` and the menus show it); with Automatically transcribe clips
on, `file.import` reports the same reason as a `transcription: …` entry in its `errors`.
Without `speech-download`, `transcript.downloadModel` is disabled the same way. Transcripts can
still be imported with `transcript.set` and edited with every other command. Hosts and tests can
install any recogniser in `Session::transcriber`, which enables transcription in any build.

### whisper.cpp (your own models)

Settings ▸ Media Analysis & Transcription ▸ **Speech engine** can switch from the built-in
Whisper to **whisper.cpp**: FilmCraft then runs the command you name (`whisper-cli` from Homebrew,
or a full path) with any ggml model file you point it at (`ggml-large-v3-turbo.bin`, for
example), entirely on this computer, and reads back its word-level JSON
(`filmcraft_speech::external`). This works in every native build, including releases built
without the `whisper` feature, because the engine builds the recogniser from the preferences at
run time (`mediaAnalysis.speechEngine`, `whisperCppCommand`, `whisperCppModel`, `whisperCppArgs`).

- The audio goes to a temporary 16 kHz WAV file that is deleted afterwards; the command is run
  with `-ojf -ml 1 -sow` so every segment is one word with millisecond offsets and a token
  probability, which becomes the word's confidence. Punctuation-only segments attach to the
  word before. Speaker labelling and word tightening are the same as for the built-in model.
- The command is polled while it runs, so Cancel works and a stuck command times out.
- Errors are specific: command not found (GUI apps don't see Homebrew's PATH, so the usual
  locations are searched too), model not found, the command's own stderr when it fails, or no
  JSON output when the build lacks `-ojf`.
- Transcripts record `whisper.cpp:<model file stem>` as their `source`. `transcript.models`
  reports the engine and whether the whisper.cpp settings are complete (`whisperCpp.ready`).
- Not on the web build (no processes there); choosing the engine there disables transcription
  with that reason.

### Models

Weights are **never** bundled or committed. They are downloaded on request into
`<data dir>/models/<id>/` (see `filmcraft_engine::autosave::default_data_dir`), each file pinned to a
revision of OpenAI's Hugging Face repositories and checked against its SHA-256:

| Id | Languages | Licence |
|---|---|---|
| `whisper-tiny` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-base` (default) | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-small` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |

### Testing

Unit and engine tests use a fake `Transcriber` (`FixedTranscriber`), so CI needs no model. The
end-to-end test `crates/speech/tests/whisper_model.rs` (feature `whisper`) runs only when weights are
in `target/models/<id>/` (or `$FILMCRAFT_MODELS_DIR`) and speech samples (mono 16 kHz f32 with a
reference `.txt`) are in `target/fixtures/speech/`; it reports the word error rate and otherwise
prints SKIPPED. Measured on 2026-10-01: `whisper-tiny`, English, 12.2 % WER over 797 words of
the local speech samples (LibriSpeech read speech and dialogue clips) (about 9.5 minutes for the run in a release build on an
Apple-silicon laptop CPU).

## Limits

- Transcription runs synchronously inside the command (no background job or progress bar yet).
- Track items that refer to a subclip are looked up by the subclip's id, so a transcript made for
  the parent media is not shown through subclip clips yet.
