use std::sync::Arc;

use serde_json::json;

use crate::Session;
use filmcraft_project::{ItemId, Transcript, Word};
use filmcraft_speech::FixedTranscriber;
use filmcraft_time::Tick;

/// The demo project with a fake transcriber whose transcript fits the first A1 clip's media:
/// "Hello um world." (Speaker 1), a 1.2 s pause, "Second speaker here." (Speaker 2).
fn session() -> (Session, ItemId, Tick) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let a = &q.audio_tracks[0].items[0];
    let (item, sin) = (a.item, a.source_in);
    let sec = |x: f64| sin + Tick::from_seconds_f64(x);
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for (text, a, b, sp) in
        [("Hello", 0.2, 0.5, 0), ("um", 0.6, 0.9, 0), ("world.", 1.0, 1.4, 0), ("Second", 2.6, 3.0, 1), ("speaker", 3.0, 3.5, 1), ("here.", 3.5, 4.0, 1)]
    {
        let mut w = Word::new(text, sec(a), sec(b));
        w.speaker = Some(sp);
        t.words.push(w);
    }
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    let start = a_start(&s);
    (s, item, start)
}

fn a_start(s: &Session) -> Tick {
    s.active_sequence().unwrap().audio_tracks[0].items[0].start
}

fn words(s: &mut Session) -> Vec<String> {
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    r["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect()
}

#[test]
fn generate_inspect_search_and_rename() {
    let (mut s, item, start) = session();
    assert!(s.execute("transcript.select", json!({"from": 0})).is_err(), "disabled before transcribing");
    let r = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    assert_eq!(r["items"][0]["words"], 6, "{r}");
    assert_eq!(r["items"][0]["source"], "fixed");
    assert_eq!(s.project.transcripts[&item].words.len(), 6);

    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(words(&mut s), ["Hello", "um", "world.", "Second", "speaker", "here."]);
    assert_eq!(r["words"][0]["start"].as_i64().unwrap(), (start + Tick::from_seconds_f64(0.2)).0, "mapped to sequence time");
    assert_eq!(r["paragraphs"].as_array().unwrap().len(), 2, "speaker change splits paragraphs: {}", r["paragraphs"]);
    assert_eq!(r["speakers"], json!(["Speaker 1", "Speaker 2"]));

    let r = s.execute("transcript.search", json!({"query": "second spea"})).unwrap();
    assert_eq!(r["matches"], json!([{"from": 3, "to": 4, "start": r["matches"][0]["start"], "end": r["matches"][0]["end"]}]));

    s.execute("transcript.renameSpeaker", json!({"speaker": "Speaker 2", "name": "Ann"})).unwrap();
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(r["words"][3]["speaker"], "Ann");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers[1].name, "Speaker 2");
    s.execute("transcript.renameSpeaker", json!({"speaker": 0, "item": item.0, "name": "Bo"})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers[0].name, "Bo");
    assert!(s.execute("transcript.renameSpeaker", json!({"speaker": "Nobody", "name": "X"})).is_err());

    // transcribing is one undo step
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.transcripts.is_empty());
}

#[test]
fn select_extract_and_lift_by_words() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let rate = s.sequence_rate();
    let before = s.active_sequence().unwrap().duration();

    let r = s.execute("transcript.select", json!({"from": 3, "to": 5})).unwrap();
    let (a, b) = (Tick(r["start"].as_i64().unwrap()), Tick(r["end"].as_i64().unwrap()));
    let q = s.active_sequence().unwrap();
    assert_eq!(q.mark_in, Some(a));
    assert_eq!(q.mark_out, Some(b - rate.frame_duration()), "Out is the last frame inside");
    assert_eq!(rate.snap(a), a, "frame aligned");
    assert_eq!(s.playhead(), a);

    // Extract "um": the sequence gets shorter by the word's frames and the word is gone
    let r = s.execute("transcript.extract", json!({"from": 1})).unwrap();
    let cut = Tick(r["end"].as_i64().unwrap()) - Tick(r["start"].as_i64().unwrap());
    assert!(cut > Tick::ZERO);
    assert_eq!(s.active_sequence().unwrap().duration(), before - cut);
    assert!(!words(&mut s).contains(&"um".to_string()), "{:?}", words(&mut s));
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);

    // Lift leaves a gap: same duration, word gone
    s.execute("transcript.lift", json!({"from": 1, "to": 1})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);
    assert_eq!(words(&mut s), ["Hello", "world.", "Second", "speaker", "here."]);
    assert!(s.execute("transcript.extract", json!({"from": 99})).is_err());
}

#[test]
fn remove_fillers_pauses_and_create_captions() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let before = s.active_sequence().unwrap().duration();
    let r = s.execute("transcript.removeFillers", json!({})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    assert!(!words(&mut s).contains(&"um".to_string()));
    let r = s.execute("transcript.removePauses", json!({"minSeconds": 1.0, "keepSeconds": 0.1})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    let after = s.active_sequence().unwrap().duration();
    assert!(after < before - Tick::from_seconds_f64(1.0), "the pause and the filler are gone");
    assert_eq!(words(&mut s).len(), 5);
    s.active_sequence().unwrap().check().unwrap();

    let r = s.execute("transcript.createCaptions", json!({"maxChars": 32})).unwrap();
    assert_eq!(r["captions"], 2, "one caption per speaker: {r}");
    let q = s.active_sequence().unwrap();
    let tr = &q.caption_tracks[0];
    assert_eq!(tr.captions[0].text, "Hello world.");
    assert_eq!(tr.captions[1].speaker.as_deref(), Some("Speaker 2"));
    q.check().unwrap();
}

#[test]
fn pauses_preview_matches_remove_pauses() {
    let (mut s, item, _) = session();
    assert!(s.execute("transcript.pauses", json!({})).is_err(), "disabled without a transcript");
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let before = s.active_sequence().unwrap().duration();
    let p = json!({"minSeconds": 1.0, "keepSeconds": 0.1});
    let r = s.execute("transcript.pauses", p.clone()).unwrap();
    assert_eq!(r["count"], 1, "{r}");
    assert_eq!(s.active_sequence().unwrap().duration(), before, "read-only");
    let removed = s.execute("transcript.removePauses", p).unwrap();
    assert_eq!(removed["removed"], r["count"], "{removed}");
    assert!((removed["seconds"].as_f64().unwrap() - r["seconds"].as_f64().unwrap()).abs() < 1e-9, "{removed} vs {r}");
    s.execute("edit.undo", json!({})).unwrap();

    // a lower threshold also finds the shorter gaps
    let r = s.execute("transcript.pauses", json!({"minSeconds": 0.05, "keepSeconds": 0.0})).unwrap();
    assert!(r["count"].as_u64().unwrap() > 1, "{r}");
    let r = s.execute("transcript.pauses", json!({"minSeconds": 30.0})).unwrap();
    assert_eq!(r, json!({"count": 0, "seconds": 0.0}));

    // hostile values are clamped, never panic
    for p in [
        json!({"minSeconds": -5.0, "keepSeconds": -1.0}),
        json!({"minSeconds": f64::MAX, "keepSeconds": f64::MAX}),
        json!({"minSeconds": 1e300, "keepSeconds": -1e300}),
        json!({"minSeconds": "x", "keepSeconds": null}),
        json!({"minSeconds": 0.5, "keepSeconds": 5.0}),
    ] {
        let r = s.execute("transcript.pauses", p.clone()).unwrap();
        assert!(r["count"].is_u64() && r["seconds"].as_f64().is_some_and(|x| x >= 0.0), "{p}: {r}");
    }
    // NaN can't travel through JSON; the clamp maps it to the default
    use crate::transcript::clamp_seconds;
    assert_eq!(clamp_seconds(Some(f64::NAN), 1.0, 0.0, 10.0), 1.0);
    assert_eq!(clamp_seconds(None, 0.15, 0.0, 10.0), 0.15);
    assert_eq!(clamp_seconds(Some(f64::INFINITY), 1.0, 0.0, 10.0), 10.0);
    assert_eq!(clamp_seconds(Some(f64::NEG_INFINITY), 1.0, 0.0, 10.0), 0.0);
    assert_eq!(s.active_sequence().unwrap().duration(), before);
}

#[test]
fn set_delete_and_models() {
    let (mut s, item, _) = session();
    let t = json!({"language": "en", "words": [
        {"text": "b", "start": 2000, "end": 3000, "speaker": 1},
        {"text": "a", "start": 0, "end": 1000},
    ]});
    let r = s.execute("transcript.set", json!({"item": item.0, "transcript": t})).unwrap();
    assert_eq!(r["words"], 2);
    let tr = &s.project.transcripts[&item];
    assert_eq!(tr.words[0].text, "a", "normalized: sorted");
    assert_eq!(tr.speakers.len(), 2, "missing speaker labels added");
    assert_eq!(tr.source, "imported");
    assert!(s.execute("transcript.set", json!({"item": 999_999, "transcript": {}})).is_err());
    assert!(s.execute("transcript.set", json!({"item": item.0, "transcript": {"words": 3}})).is_err());

    // survives save/load
    let bytes = filmcraft_format::encode(&s.project, true);
    let back = filmcraft_format::decode(&bytes).unwrap();
    assert_eq!(back.project.transcripts[&item].words.len(), 2);

    s.execute("transcript.delete", json!({"items": [item.0]})).unwrap();
    assert!(s.project.transcripts.is_empty());
    assert!(s.execute("transcript.delete", json!({})).is_err());

    let m = s.execute("transcript.models", json!({})).unwrap();
    assert_eq!(m["available"], filmcraft_speech::available());
    assert!(m["models"].as_array().unwrap().iter().any(|x| x["id"] == "whisper-base"));
}

#[test]
fn generate_without_a_transcriber() {
    let (mut s, item, _) = session();
    s.transcriber = None;
    let e = s.execute("transcript.generate", json!({"items": [item.0], "model": "nope"})).unwrap_err().to_string();
    // a build without speech-to-text says that first: the command is disabled (#97)
    let why = if filmcraft_speech::available() { "unknown speech model" } else { "not available in this build" };
    assert!(e.contains(why), "{e}");
    if !filmcraft_speech::available() {
        assert!(!s.is_enabled("sequence.transcribe"), "Transcribe Sequence follows transcript.generate");
        let e = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap_err().to_string();
        assert!(e.contains("whisper") && e.contains("not available"), "{e}");
        #[cfg(not(feature = "speech-download"))]
        assert!(s.execute("transcript.downloadModel", json!({})).unwrap_err().to_string().contains("not available"));
    }
    // defaults to the media of the open sequence's audio clips
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: Transcript::default(), id: "empty".into() }));
    assert!(s.is_enabled("sequence.transcribe"), "an installed recogniser enables Transcribe Sequence");
    let r = s.execute("transcript.generate", json!({})).unwrap();
    assert!(r["items"].as_array().unwrap().len() >= 2, "{r}");
}

/// The whisper.cpp engine from Settings: a fake `whisper-cli` (a shell script) stands in for the
/// real one, so this runs in CI without a model. The settings are validated, the command line and
/// output file round-trip, and every failure has a reason the user can act on.
#[cfg(unix)]
#[test]
fn whisper_cpp_engine_from_settings() {
    use std::os::unix::fs::PermissionsExt;
    let (mut s, item, _) = session();
    s.transcriber = None;
    let dir = crate::media_test_util::tmp_dir("whisper-cpp");
    let set = |s: &mut Session, k: &str, v: serde_json::Value| s.execute("prefs.set", json!({"key": k, "value": v})).unwrap();
    // selecting the engine without a model: disabled, with the reason
    set(&mut s, "mediaAnalysis.speechEngine", json!("whisperCpp"));
    let why = crate::transcript::can_transcribe(&s).unwrap_err();
    assert!(why.contains("model path is not set"), "{why}");
    assert_eq!(s.execute("transcript.models", json!({})).unwrap()["whisperCpp"]["ready"], json!(false));
    // a model that does not exist: enabled (cheap check), but the run says what is missing
    set(&mut s, "mediaAnalysis.whisperCppModel", json!(dir.join("missing.bin").to_string_lossy()));
    assert!(s.is_enabled("transcript.generate"));
    let e = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap_err().to_string();
    assert!(e.contains("model") && e.contains("was not found"), "{e}");
    // a command that does not exist
    let model = dir.join("ggml-fake.bin");
    std::fs::write(&model, b"weights").unwrap();
    set(&mut s, "mediaAnalysis.whisperCppModel", json!(model.to_string_lossy()));
    set(&mut s, "mediaAnalysis.whisperCppCommand", json!("filmcraft-no-such-whisper-zzz"));
    let e = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap_err().to_string();
    assert!(e.contains("not found on PATH"), "{e}");
    // the fake command writes two words and echoes the language flag it got
    let script = dir.join("fake-whisper-cli");
    let body = "#!/bin/sh\nout=\"\"\nlang=\"\"\nwhile [ $# -gt 0 ]; do\n  [ \"$1\" = \"-of\" ] && out=\"$2\"\n  [ \"$1\" = \"-l\" ] && lang=\"$2\"\n  shift\ndone\ncat > \"$out.json\" <<J\n{\"result\":{\"language\":\"$lang\"},\"transcription\":[\n{\"offsets\":{\"from\":200,\"to\":500},\"text\":\" Hello\",\"tokens\":[{\"text\":\" Hello\",\"p\":0.9}]},\n{\"offsets\":{\"from\":600,\"to\":900},\"text\":\" world\",\"tokens\":[{\"text\":\" world\",\"p\":0.7}]},\n{\"offsets\":{\"from\":900,\"to\":900},\"text\":\".\",\"tokens\":[{\"text\":\".\",\"p\":0.9}]}]}\nJ\n";
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    set(&mut s, "mediaAnalysis.whisperCppCommand", json!(script.to_string_lossy()));
    set(&mut s, "mediaAnalysis.whisperCppArgs", json!("-bs 5 --prompt 'two words'"));
    set(&mut s, "mediaAnalysis.speakerLabeling", json!("off"));
    assert_eq!(s.execute("transcript.models", json!({})).unwrap()["whisperCpp"]["ready"], json!(true));
    let r = s.execute("transcript.generate", json!({"items": [item.0], "language": "de"})).unwrap();
    assert_eq!(r["items"][0]["source"], json!("whisper.cpp:ggml-fake"));
    assert_eq!(r["items"][0]["language"], json!("de"), "the language flag reached the command: {r}");
    let t = &s.project.transcripts[&item];
    let texts: Vec<&str> = t.words.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(texts, ["Hello", "world."]);
    assert!(t.words[0].start < t.words[1].start && t.words[1].end <= t.words.last().unwrap().end);
    assert!((t.words[1].confidence - 0.7).abs() < 1e-6);
    assert!(s.execute("edit.undo", json!({})).is_ok());
    assert!(!s.project.transcripts.contains_key(&item), "Transcribe is one undo step");
    // back to the built-in engine: the old behaviour
    set(&mut s, "mediaAnalysis.speechEngine", json!("builtin"));
    assert_eq!(s.is_enabled("transcript.generate"), filmcraft_speech::available());
    assert!(s.execute("prefs.set", json!({"key": "mediaAnalysis.speechEngine", "value": "cloud"})).is_err(), "unknown engines are rejected");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generate_in_the_background_stores_the_transcript_when_polled() {
    let (mut s, item, _) = session();
    let r = s.execute("transcript.generate", json!({"items": [item.0], "background": true})).unwrap();
    let job = r["job"].as_u64().expect("a job id");
    assert_eq!(r["items"], json!([item.0]));
    assert!(s.jobs.iter().any(|j| j.id == job && j.label == "Transcribing 1 clip"), "{:?}", s.jobs.iter().map(|j| &j.label).collect::<Vec<_>>());
    assert!(
        s.execute("transcript.generate", json!({"items": [item.0], "background": true})).is_err() || s.project.transcripts.contains_key(&item),
        "a second run while one is going is refused"
    );
    let t0 = std::time::Instant::now();
    while !s.project.transcripts.contains_key(&item) {
        assert!(t0.elapsed().as_secs() < 10, "the transcript never arrived");
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(s.project.transcripts[&item].words.len(), 6);
    assert!(s.transcribe_jobs.is_empty(), "the pending job is dropped once stored");
    let toast = s.drain_events().into_iter().find_map(|e| match e {
        crate::Event::Toast { message, error: false } if message.starts_with("Transcribed") => Some(message),
        _ => None,
    });
    assert_eq!(toast.as_deref(), Some("Transcribed 1 clip (6 words)"));
    assert_eq!(words(&mut s), ["Hello", "um", "world.", "Second", "speaker", "here."]);
    // the synchronous default still reports the words directly
    s.execute("transcript.delete", json!({})).unwrap();
    let r = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    assert_eq!(r["items"][0]["words"], 6, "{r}");
    assert!(s.transcribe_jobs.is_empty());
}
