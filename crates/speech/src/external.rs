//! Speech recognition through an external whisper.cpp command line (`whisper-cli`).
//!
//! FilmCraft's release builds ship without the built-in Whisper (feature `whisper`), and its
//! catalogue stops at the small model. [`ExternalTranscriber`] runs the user's own whisper.cpp
//! binary with any ggml model they already have (for example `ggml-large-v3-turbo.bin`) and
//! reads back its word-level JSON. Everything stays on the machine: the audio is written to a
//! temporary 16 kHz WAV file, the command is run, the JSON is parsed, and the temporary files are
//! removed. The command is chosen in Settings ▸ Media Analysis & Transcription ▸ Speech engine.
//!
//! Native only: process spawning does not exist in the web build.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use filmcraft_project::{Transcript, Word};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::Value;

use crate::{Options, ProgressFn, SAMPLE_RATE, SpeechError, Transcriber, sample_tick};

/// Ticks per millisecond (whisper.cpp reports offsets in milliseconds).
const TICKS_PER_MS: i64 = TICKS_PER_SECOND / 1000;
/// Largest JSON output read back (ten hours of speech is well under 100 MB).
const MAX_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;
/// Words kept from one run.
const MAX_WORDS: usize = 2_000_000;
/// Characters of the command's stderr quoted in an error.
const MAX_STDERR: usize = 2000;
/// How often the running command is polled (and progress reported).
const POLL: Duration = Duration::from_millis(100);
/// Directories a GUI app's PATH usually misses; searched after PATH for a bare command name.
const EXTRA_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

/// A recogniser that shells out to whisper.cpp.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalTranscriber {
    /// The whisper.cpp command: a bare name (looked up on PATH and in the usual Homebrew and
    /// `/usr/local` locations) or a path.
    pub command: String,
    /// The ggml model file.
    pub model: PathBuf,
    /// Extra arguments appended to the command line.
    pub args: Vec<String>,
    /// Threads (`-t`); 0 = the command's default.
    pub threads: usize,
}

impl ExternalTranscriber {
    pub fn new(command: impl Into<String>, model: impl Into<PathBuf>) -> Self {
        Self { command: command.into(), model: model.into(), args: Vec::new(), threads: 0 }
    }

    /// Where the command is, or why it can't run.
    pub fn resolve_command(&self) -> Result<PathBuf, SpeechError> {
        let c = self.command.trim();
        if c.is_empty() {
            return Err(SpeechError::NotInstalled("the whisper.cpp command is not set (Settings ▸ Media Analysis & Transcription)".into()));
        }
        let p = Path::new(c);
        if p.is_absolute() || p.components().count() > 1 {
            return if p.is_file() { Ok(p.to_path_buf()) } else { Err(SpeechError::NotInstalled(format!("the whisper.cpp command `{c}` was not found"))) };
        }
        let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
        dirs.extend(EXTRA_DIRS.iter().map(PathBuf::from));
        dirs.iter().map(|d| d.join(c)).find(|p| p.is_file()).ok_or_else(|| {
            SpeechError::NotInstalled(format!("the whisper.cpp command `{c}` was not found on PATH (install whisper.cpp, or set its full path in Settings)"))
        })
    }

    /// Both the command and the model exist.
    pub fn check(&self) -> Result<(), SpeechError> {
        self.resolve_command()?;
        if self.model.as_os_str().is_empty() {
            return Err(SpeechError::NotInstalled("the whisper.cpp model path is not set (Settings ▸ Media Analysis & Transcription)".into()));
        }
        if !self.model.is_file() {
            return Err(SpeechError::NotInstalled(format!("the whisper.cpp model `{}` was not found", self.model.display())));
        }
        Ok(())
    }

    fn run(&self, exe: &Path, dir: &Path, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        let wav = dir.join("audio.wav");
        std::fs::write(&wav, write_wav16_mono(audio)?)?;
        let out = dir.join("out");
        let json_path = dir.join("out.json");
        let language = opts.language.as_deref().map(str::trim).filter(|l| !l.is_empty()).unwrap_or("auto");
        let mut cmd = Command::new(exe);
        cmd.arg("-m").arg(&self.model).arg("-f").arg(&wav).arg("-of").arg(&out);
        cmd.args(["-ojf", "-np", "-ml", "1", "-sow", "-l", language]);
        if self.threads > 0 {
            cmd.arg("-t").arg(self.threads.to_string());
        }
        cmd.args(&self.args);
        let stderr_path = dir.join("stderr.txt");
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(std::fs::File::create(&stderr_path)?);
        let mut child = cmd.spawn().map_err(|e| SpeechError::Io(format!("could not start `{}`: {e}", exe.display())))?;

        // Poll so the user can cancel, and so a stuck command can't hang the editor forever.
        let secs = audio.len() as f64 / SAMPLE_RATE as f64;
        let expected = secs * 0.5 + 5.0;
        let timeout = Duration::from_secs_f64(600.0 + secs * 2.0);
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SpeechError::Io(e.to_string()));
                }
            }
            let elapsed = started.elapsed();
            let frac = 0.05 + 0.85 * (elapsed.as_secs_f64() / expected).min(1.0) as f32;
            if !progress(frac, "Transcribing with whisper.cpp") {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SpeechError::Cancelled);
            }
            if elapsed > timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SpeechError::Model(format!("whisper.cpp did not finish within {} s", timeout.as_secs())));
            }
            std::thread::sleep(POLL);
        };
        if !status.success() {
            let err = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            let tail: String = err.chars().rev().take(MAX_STDERR).collect::<Vec<_>>().into_iter().rev().collect();
            return Err(SpeechError::Model(format!("whisper.cpp exited with {status}: {}", tail.trim())));
        }
        let len = std::fs::metadata(&json_path)
            .map_err(|_| SpeechError::Model("whisper.cpp produced no JSON output (is `-ojf` supported by this build?)".into()))?
            .len();
        if len > MAX_OUTPUT_BYTES {
            return Err(SpeechError::Model(format!("whisper.cpp output is too large ({len} bytes)")));
        }
        let bytes = std::fs::read(&json_path)?;
        if !progress(0.92, "Reading words") {
            return Err(SpeechError::Cancelled);
        }
        let (words, detected) = parse_output(&bytes, sample_tick(audio.len() as i64))?;
        let language = detected.or_else(|| opts.language.clone()).unwrap_or_else(|| "en".into());
        let mut t = Transcript { language, source: self.id(), speakers: Vec::new(), words, takes: Vec::new() };
        t.normalize();
        crate::vad::tighten_words(audio, &mut t.words);
        if opts.diarize && !t.words.is_empty() {
            if !progress(0.96, "Labelling speakers") {
                return Err(SpeechError::Cancelled);
            }
            let p = crate::diarize::Params { max_speakers: opts.max_speakers, ..Default::default() };
            crate::diarize::diarize(audio, &mut t, &p);
        }
        progress(1.0, "Done");
        Ok(t)
    }
}

impl Transcriber for ExternalTranscriber {
    fn id(&self) -> String {
        let stem = self.model.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "model".into());
        format!("whisper.cpp:{stem}")
    }

    fn transcribe(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        let exe = self.resolve_command()?;
        self.check()?;
        if audio.is_empty() {
            return Ok(Transcript { language: opts.language.clone().unwrap_or_else(|| "en".into()), source: self.id(), ..Default::default() });
        }
        if !progress(0.02, "Writing audio") {
            return Err(SpeechError::Cancelled);
        }
        let dir = scratch_dir()?;
        let result = self.run(&exe, &dir, audio, opts, progress);
        let _ = std::fs::remove_dir_all(&dir);
        result
    }
}

/// A fresh private directory for one run's files.
fn scratch_dir() -> Result<PathBuf, SpeechError> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("filmcraft-speech-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Mono 16 kHz samples as a 16-bit PCM WAV file.
fn write_wav16_mono(samples: &[f32]) -> Result<Vec<u8>, SpeechError> {
    let data_len = samples.len().checked_mul(2).ok_or_else(|| SpeechError::Io("audio too long for a WAV file".into()))?;
    let data_len32 = u32::try_from(data_len).map_err(|_| SpeechError::Io("audio too long for a WAV file".into()))?;
    let riff_len = data_len32.checked_add(36).ok_or_else(|| SpeechError::Io("audio too long for a WAV file".into()))?;
    let mut v = Vec::with_capacity(44 + data_len);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&riff_len.to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    v.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len32.to_le_bytes());
    for s in samples {
        let x = if s.is_finite() { s.clamp(-1.0, 1.0) } else { 0.0 };
        v.extend_from_slice(&((x * 32767.0).round() as i16).to_le_bytes());
    }
    Ok(v)
}

/// Parse whisper.cpp's full JSON (`-ojf`) into words bounded by `max`, plus the detected language.
///
/// With `-ml 1 -sow` every segment is one word; a segment that still holds several words shares
/// its span between them, and punctuation-only segments attach to the word before. Special
/// tokens (`[_BEG_]`, `<|en|>`) and anything outside `0..max` are dropped. Every field is
/// optional and every number is treated as hostile.
pub fn parse_output(bytes: &[u8], max: Tick) -> Result<(Vec<Word>, Option<String>), SpeechError> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| SpeechError::Model(format!("whisper.cpp output is not valid JSON: {e}")))?;
    let language = v
        .pointer("/result/language")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && s.len() <= 8 && s.chars().all(|c| c.is_ascii_alphabetic()));
    let segments =
        v.get("transcription").and_then(Value::as_array).ok_or_else(|| SpeechError::Model("whisper.cpp output has no `transcription` list".into()))?;
    let mut words: Vec<Word> = Vec::new();
    for seg in segments {
        if words.len() >= MAX_WORDS {
            break;
        }
        let text = seg.get("text").and_then(Value::as_str).unwrap_or("").trim();
        if text.is_empty() || text.starts_with("[_") || text.starts_with("<|") {
            continue;
        }
        let Some((a, b)) = offsets(seg, max) else { continue };
        let confidence = confidence(seg);
        if text.chars().all(|c| !c.is_alphanumeric()) {
            if let Some(last) = words.last_mut() {
                last.text.push_str(text);
            }
            continue;
        }
        let parts: Vec<&str> = text.split_whitespace().collect();
        let n = parts.len() as i64;
        let span = b.0.saturating_sub(a.0);
        for (i, part) in parts.iter().enumerate() {
            let i = i as i64;
            let s = a.0.saturating_add(span.checked_mul(i).map(|x| x / n.max(1)).unwrap_or(0));
            let e = a.0.saturating_add(span.checked_mul(i + 1).map(|x| x / n.max(1)).unwrap_or(span));
            let mut w = Word::new(*part, Tick(s), Tick(e.max(s)));
            w.confidence = confidence;
            words.push(w);
        }
    }
    Ok((words, language))
}

/// A segment's `offsets.from` / `offsets.to` (ms) as ticks within `0..max`.
fn offsets(seg: &Value, max: Tick) -> Option<(Tick, Tick)> {
    let from = seg.pointer("/offsets/from").and_then(Value::as_i64)?;
    let to = seg.pointer("/offsets/to").and_then(Value::as_i64)?;
    if from < 0 || to < from {
        return None;
    }
    let a = Tick(from.checked_mul(TICKS_PER_MS)?);
    if a >= max {
        return None;
    }
    let b = Tick(to.checked_mul(TICKS_PER_MS)?.min(max.0));
    Some((a, b.max(a)))
}

/// Mean token probability of a segment (1.0 when it reports none).
fn confidence(seg: &Value) -> f32 {
    let Some(tokens) = seg.get("tokens").and_then(Value::as_array) else { return 1.0 };
    let ps: Vec<f64> = tokens
        .iter()
        .filter(|t| !t.get("text").and_then(Value::as_str).is_some_and(|s| s.starts_with("[_") || s.starts_with("<|")))
        .filter_map(|t| t.get("p").and_then(Value::as_f64))
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
        .collect();
    if ps.is_empty() {
        return 1.0;
    }
    (ps.iter().sum::<f64>() / ps.len() as f64) as f32
}

/// Split a free-text argument string the way a shell would, honouring single and double quotes.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seconds_tick;

    fn seg(text: &str, from: i64, to: i64, p: f64) -> String {
        format!(
            r#"{{"timestamps":{{"from":"x","to":"y"}},"offsets":{{"from":{from},"to":{to}}},"text":"{text}","tokens":[{{"text":"{text}","p":{p},"offsets":{{"from":{from},"to":{to}}}}}]}}"#
        )
    }

    fn doc(segs: &[String]) -> Vec<u8> {
        format!(r#"{{"systeminfo":"","model":{{}},"params":{{}},"result":{{"language":"en"}},"transcription":[{}]}}"#, segs.join(",")).into_bytes()
    }

    #[test]
    fn parses_words_offsets_confidence_and_language() {
        let bytes =
            doc(&[seg("[_BEG_]", 0, 0, 0.9), seg(" And", 320, 400, 0.6), seg(" so", 400, 690, 0.9), seg(" my", 690, 940, 0.5), seg(".", 940, 940, 0.9)]);
        let (w, lang) = parse_output(&bytes, seconds_tick(10.0)).unwrap();
        assert_eq!(lang.as_deref(), Some("en"));
        let texts: Vec<&str> = w.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["And", "so", "my."]);
        assert_eq!(w[0].start, Tick(320 * TICKS_PER_MS));
        assert_eq!(w[0].end, Tick(400 * TICKS_PER_MS));
        assert!((w[0].confidence - 0.6).abs() < 1e-6);
    }

    #[test]
    fn multiword_segments_share_their_span() {
        let bytes = doc(&[seg(" hello big world", 1000, 1600, 0.8)]);
        let (w, _) = parse_output(&bytes, seconds_tick(10.0)).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].start, Tick(1000 * TICKS_PER_MS));
        assert_eq!(w[0].end, w[1].start);
        assert_eq!(w[2].end, Tick(1600 * TICKS_PER_MS));
    }

    #[test]
    fn hostile_output_never_panics() {
        let max = seconds_tick(2.0);
        assert!(parse_output(b"", max).is_err());
        assert!(parse_output(b"garbage", max).is_err());
        assert!(parse_output(b"[]", max).is_err());
        assert!(parse_output(br#"{"transcription": 5}"#, max).is_err());
        let (w, lang) = parse_output(br#"{"transcription": []}"#, max).unwrap();
        assert!(w.is_empty() && lang.is_none());
        // negative, reversed, absurd and missing offsets; NaN-ish and out-of-range probabilities; odd language
        let bytes = format!(
            r#"{{"result":{{"language":"  Zz-9 "}},"transcription":[
                {seg_neg},{seg_rev},{seg_huge},{seg_late},
                {{"text":"nooffsets"}},{{"offsets":{{"from":"a","to":"b"}},"text":"strings"}},
                {{"offsets":{{"from":100,"to":200}},"text":"badp","tokens":[{{"text":"badp","p":7.5}},{{"text":"x","p":"nan"}}]}},
                {{"offsets":{{"from":300,"to":400}},"text":"ok","tokens":"not a list"}},
                {{"offsets":{{"from":500,"to":600}},"text":"   "}},
                {{"offsets":{{"from":700,"to":800}},"text":"<|en|>"}}
            ]}}"#,
            seg_neg = seg("neg", -5, 10, 0.5),
            seg_rev = seg("rev", 900, 100, 0.5),
            seg_huge = seg("huge", i64::MAX / 2, i64::MAX, 0.5),
            seg_late = seg("late", 5000, 6000, 0.5),
        );
        let (w, lang) = parse_output(bytes.as_bytes(), max).unwrap();
        assert!(lang.is_none(), "{lang:?}");
        let texts: Vec<&str> = w.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["badp", "ok"]);
        assert_eq!(w[0].confidence, 1.0, "out-of-range probabilities are ignored");
        // a word that starts inside the audio but ends after it is clipped
        let bytes = doc(&[seg("tail", 1900, 9000, 0.5)]);
        let (w, _) = parse_output(&bytes, max).unwrap();
        assert_eq!(w[0].end, max);
    }

    #[test]
    fn wav_header_is_well_formed() {
        let v = write_wav16_mono(&[0.0, 0.5, -0.5, f32::NAN, 2.0]).unwrap();
        assert_eq!(&v[..4], b"RIFF");
        assert_eq!(&v[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes([v[24], v[25], v[26], v[27]]), SAMPLE_RATE);
        assert_eq!(u32::from_le_bytes([v[40], v[41], v[42], v[43]]), 10);
        assert_eq!(v.len(), 54);
        assert_eq!(i16::from_le_bytes([v[50], v[51]]), 0, "NaN becomes silence");
        assert_eq!(i16::from_le_bytes([v[52], v[53]]), 32767, "clipped");
    }

    #[test]
    fn args_split_like_a_shell() {
        assert_eq!(split_args(""), Vec::<String>::new());
        assert_eq!(split_args("  -bs 5   --prompt 'hello there' -x \"a b\"c "), ["-bs", "5", "--prompt", "hello there", "-x", "a bc"]);
        assert_eq!(split_args("''"), [""]);
        assert_eq!(split_args("unterminated 'quote"), ["unterminated", "quote"]);
    }

    #[test]
    fn missing_command_and_model_are_reported() {
        let t = ExternalTranscriber::new("", "/nonexistent/model.bin");
        assert!(t.resolve_command().unwrap_err().to_string().contains("not set"));
        let t = ExternalTranscriber::new("filmcraft-no-such-binary-zzz", "/nonexistent/model.bin");
        assert!(t.resolve_command().unwrap_err().to_string().contains("not found on PATH"));
        let t = ExternalTranscriber::new("/nonexistent/dir/whisper-cli", "/nonexistent/model.bin");
        assert!(t.check().unwrap_err().to_string().contains("was not found"));
        assert_eq!(ExternalTranscriber::new("x", "/a/b/ggml-large-v3-turbo.bin").id(), "whisper.cpp:ggml-large-v3-turbo");
    }

    /// A fake whisper-cli (a shell script) proves the command line, the output file and the parse
    /// fit together, and that a failing command's stderr reaches the error.
    #[cfg(unix)]
    #[test]
    fn runs_a_fake_whisper_cli() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir().unwrap();
        let model = dir.join("ggml-test.bin");
        std::fs::write(&model, b"not a real model").unwrap();
        let script = dir.join("fake-whisper-cli");
        let json = doc(&[seg(" Hello", 100, 400, 0.9), seg(" world", 500, 900, 0.8), seg(".", 900, 900, 0.9)]);
        let body = format!(
            "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"-of\" ]; then out=\"$2\"; shift; fi\n  if [ \"$1\" = \"-l\" ]; then echo \"lang=$2\" >&2; fi\n  shift\ndone\n[ -f \"$out\" ] || true\ncat > \"$out.json\" <<'J'\n{}\nJ\n",
            String::from_utf8(json).unwrap()
        );
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut t = ExternalTranscriber::new(script.to_string_lossy().to_string(), &model);
        t.args = vec!["--extra".into()];
        let audio = vec![0.0f32; 16_000 * 2];
        let opts = Options { diarize: false, ..Default::default() };
        let tr = t.transcribe(&audio, &opts, &mut |_, _| true).unwrap();
        assert_eq!(tr.source, "whisper.cpp:ggml-test");
        assert_eq!(tr.language, "en");
        let texts: Vec<&str> = tr.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["Hello", "world."]);
        // cancelling stops it
        assert_eq!(t.transcribe(&audio, &opts, &mut |_, _| false).unwrap_err(), SpeechError::Cancelled);
        // a failing command reports its stderr
        std::fs::write(&script, "#!/bin/sh\necho 'model load failed' >&2\nexit 3\n").unwrap();
        let e = t.transcribe(&audio, &opts, &mut |_, _| true).unwrap_err().to_string();
        assert!(e.contains("model load failed") && e.contains("exited"), "{e}");
        // a command that writes no output says so
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        let e = t.transcribe(&audio, &opts, &mut |_, _| true).unwrap_err().to_string();
        assert!(e.contains("no JSON output"), "{e}");
        // empty audio needs no command at all
        assert!(t.transcribe(&[], &opts, &mut |_, _| true).unwrap().words.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
