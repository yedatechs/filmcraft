//! Take editing (`takes.*`) and crossed-out text (`transcript.cuts` / `transcript.restore`) on the
//! demo project: the first A1 clip (media 1.0–6.0 s) gets a transcript in which one line is said
//! three times, so Detect Takes finds one group of three.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::Session;
use filmcraft_project::{ClipId, ItemId, Transcript, Word};
use filmcraft_speech::FixedTranscriber;
use filmcraft_time::{TICKS_PER_SECOND, Tick};

fn sec(x: f64) -> Tick {
    Tick((x * TICKS_PER_SECOND as f64).round() as i64)
}

/// Words of one pass, 0.2 s each with 0.05 s between, starting at `t0` (media seconds).
fn line(words: &[&str], t0: f64) -> Vec<Word> {
    words.iter().enumerate().map(|(i, w)| Word::new(*w, sec(t0 + i as f64 * 0.25), sec(t0 + i as f64 * 0.25 + 0.2))).collect()
}

/// Demo project, transcript on the first A1 clip's media, `transcript.generate` run.
fn session() -> (Session, ItemId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let item = s.active_sequence().unwrap().audio_tracks[0].items[0].item;
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    // media time: the clip shows 1.0–6.0 s
    t.words.extend(line(&["We", "start", "the", "show", "here."], 1.2)); // 1.2–2.4
    t.words.extend(line(&["We", "start", "the", "show", "here."], 3.0)); // 3.0–4.2
    t.words.extend(line(&["We", "start", "the", "show", "here,", "friends."], 4.5)); // 4.5–5.95
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    (s, item)
}

fn live_text(s: &mut Session) -> String {
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    r["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect::<Vec<_>>().join(" ")
}

fn groups(s: &mut Session) -> Vec<Value> {
    s.execute("takes.list", json!({})).unwrap()["groups"].as_array().unwrap().clone()
}

fn clip_starts(s: &Session) -> Vec<(ClipId, Tick, Tick)> {
    let q = s.active_sequence().unwrap();
    q.all_tracks().flat_map(|t| t.items.iter()).map(|i| (i.id, i.start, i.duration)).collect()
}

#[test]
fn detect_groups_three_takes_and_keeps_the_last_one() {
    let (mut s, item) = session();
    assert!(!s.is_enabled("takes.select"), "no groups yet");
    let before = clip_starts(&s);
    let r = s.execute("takes.detect", json!({})).unwrap();
    assert_eq!(r["items"][0]["item"], json!(item.0));
    assert_eq!(r["items"][0]["groups"], json!(1), "{r}");
    assert_eq!(r["selected"], json!(1), "the default take (last) was put in the cut: {r}");
    let g = groups(&mut s);
    assert_eq!(g.len(), 1, "{g:?}");
    let g = &g[0];
    assert_eq!(g["takes"].as_array().unwrap().len(), 3);
    assert_eq!(g["active"], json!(2));
    let live: Vec<bool> = g["takes"].as_array().unwrap().iter().map(|t| t["live"].as_bool().unwrap()).collect();
    assert_eq!(live, [false, false, true]);
    assert_eq!(g["takes"][2]["text"], json!("We start the show here, friends."));
    assert!(g["takes"][0]["words"] == json!([0, 5]), "{}", g["takes"][0]);
    // the two crossed-out passes are gone from the live transcript but listed as cuts
    assert_eq!(live_text(&mut s), "We start the show here, friends.");
    let cuts = s.execute("transcript.cuts", json!({})).unwrap();
    let cuts = cuts["cuts"].as_array().unwrap();
    assert!(!cuts.is_empty(), "{cuts:?}");
    let cut_words: Vec<String> = cuts.iter().flat_map(|c| c["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string())).collect();
    assert_eq!(cut_words.len(), 10, "{cut_words:?}");
    // one undo step for detect + select
    assert!(s.execute("edit.undo", json!({})).is_ok());
    assert_eq!(clip_starts(&s), before);
    assert!(s.project.transcripts[&item].takes.is_empty());
    assert!(s.execute("edit.redo", json!({})).is_ok());
    assert_eq!(groups(&mut s)[0]["active"], json!(2));
    assert!(s.is_enabled("takes.select"));
}

#[test]
fn cycle_cross_restore_and_undo() {
    let (mut s, _) = session();
    s.execute("takes.detect", json!({})).unwrap();
    let id = groups(&mut s)[0]["id"].as_u64().unwrap();
    let after_detect = clip_starts(&s);
    // previous: take 1 becomes the only live one
    let r = s.execute("takes.previous", json!({"group": id})).unwrap();
    assert_eq!(r["take"], json!(1));
    assert_eq!(groups(&mut s)[0]["active"], json!(1));
    assert_eq!(live_text(&mut s), "We start the show here.");
    // next wraps 1 → 2; next again 2 → 0
    s.execute("takes.next", json!({"group": id})).unwrap();
    assert_eq!(groups(&mut s)[0]["active"], json!(2));
    s.execute("takes.next", json!({"group": id})).unwrap();
    assert_eq!(groups(&mut s)[0]["active"], json!(0));
    // the playhead sits on the live take, so `group` can be omitted
    s.execute("takes.next", json!({})).unwrap();
    assert_eq!(groups(&mut s)[0]["active"], json!(1));
    // the material after the group never moves relative to the group's end: total length is
    // the length with exactly one take live
    let len_one = s.active_sequence().unwrap().duration();
    s.execute("takes.select", json!({"group": id, "take": 2})).unwrap();
    let layout = |v: &[(ClipId, Tick, Tick)]| {
        let mut l: Vec<(Tick, Tick)> = v.iter().map(|(_, a, d)| (*a, *d)).collect();
        l.sort();
        l
    };
    assert_eq!(layout(&clip_starts(&s)), layout(&after_detect), "back on take 2, the layout is the one Detect Takes left (ids differ)");
    // cross out the live take: nothing live, text gone
    s.execute("takes.cross", json!({"group": id, "take": 2})).unwrap();
    assert_eq!(groups(&mut s)[0]["active"], Value::Null);
    assert_eq!(live_text(&mut s), "");
    assert!(s.execute("takes.cross", json!({"group": id, "take": 2})).unwrap_err().to_string().contains("already crossed out"));
    // restore take 0 brings it back where the group was
    s.execute("takes.restore", json!({"group": id, "take": 0})).unwrap();
    assert_eq!(groups(&mut s)[0]["active"], json!(0));
    assert_eq!(live_text(&mut s), "We start the show here.");
    assert!(s.active_sequence().unwrap().duration() <= len_one);
    assert!(s.execute("takes.restore", json!({"group": id, "take": 0})).unwrap_err().to_string().contains("already in the cut"));
    // every step is one undo
    for expect in [Value::Null, json!(2), json!(1), json!(0), json!(2), json!(1)] {
        assert!(s.execute("edit.undo", json!({})).is_ok());
        assert_eq!(groups(&mut s)[0]["active"], expect);
    }
}

#[test]
fn labels_redo_merge_split_add_remove() {
    let (mut s, item) = session();
    s.execute("takes.detect", json!({"select": "none"})).unwrap();
    let g = groups(&mut s);
    assert_eq!(g[0]["active"], Value::Null, "nothing chosen: every take is live");
    let id = g[0]["id"].as_u64().unwrap();
    // labels
    s.execute("takes.label", json!({"group": id, "take": 1, "label": "best", "note": "nice energy"})).unwrap();
    let g = groups(&mut s);
    assert_eq!(g[0]["takes"][1]["label"], json!("best"));
    assert_eq!(g[0]["takes"][1]["note"], json!("nice energy"));
    assert!(g[0]["manual"].as_bool().unwrap());
    assert_eq!(s.execute("takes.list", json!({"label": "best"})).unwrap()["groups"].as_array().unwrap().len(), 1);
    assert_eq!(s.execute("takes.list", json!({"label": "flat"})).unwrap()["groups"].as_array().unwrap().len(), 0);
    s.execute("takes.label", json!({"group": id, "take": 1, "label": null})).unwrap();
    assert_eq!(groups(&mut s)[0]["takes"][1]["label"], Value::Null);
    let e = s.execute("takes.label", json!({"group": id, "take": 1, "label": "meh"})).unwrap_err().to_string();
    assert!(e.contains("unknown label") && e.contains("wrongEnergy"), "{e}");
    // redo list
    assert_eq!(s.execute("takes.list", json!({"redo": true})).unwrap()["groups"].as_array().unwrap().len(), 0);
    s.execute("takes.redo", json!({"group": id, "note": "say it slower"})).unwrap();
    let r = s.execute("takes.list", json!({"redo": true})).unwrap();
    assert_eq!(r["groups"].as_array().unwrap().len(), 1);
    assert_eq!(r["groups"][0]["note"], json!("say it slower"));
    s.execute("takes.redo", json!({"group": id, "redo": false})).unwrap();
    assert_eq!(s.execute("takes.list", json!({"redo": true})).unwrap()["groups"].as_array().unwrap().len(), 0);
    // split at take 2 → two groups; merge them back
    let r = s.execute("takes.split", json!({"group": id, "at": 2})).unwrap();
    let new_id = r["newGroup"].as_u64().unwrap();
    assert_ne!(new_id, id);
    let g = groups(&mut s);
    assert_eq!(g.len(), 2);
    assert_eq!(g[0]["takes"].as_array().unwrap().len() + g[1]["takes"].as_array().unwrap().len(), 3);
    assert!(s.execute("takes.split", json!({"group": id, "at": 0})).is_err());
    assert!(s.execute("takes.split", json!({"group": id, "at": 7})).is_err());
    let r = s.execute("takes.merge", json!({"groups": [id, new_id]})).unwrap();
    assert_eq!(r["group"], json!(id));
    let g = groups(&mut s);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0]["takes"].as_array().unwrap().len(), 3);
    assert!(s.execute("takes.merge", json!({"groups": [id]})).is_err());
    // a manual take by live word indices, into a new group, then removed
    let r = s.execute("takes.add", json!({"from": 0, "to": 1})).unwrap();
    let manual = r["group"].as_u64().unwrap();
    let g = groups(&mut s);
    assert_eq!(g.len(), 2);
    let m = g.iter().find(|g| g["id"] == json!(manual)).unwrap();
    assert_eq!(m["takes"][0]["text"], json!("We start"));
    assert!(m["manual"].as_bool().unwrap());
    // a second pass added to that group by media ticks
    s.execute("takes.add", json!({"group": manual, "item": item.0, "startSeconds": 3.0, "endSeconds": 3.5})).unwrap();
    assert_eq!(groups(&mut s).iter().find(|g| g["id"] == json!(manual)).unwrap()["takes"].as_array().unwrap().len(), 2);
    s.execute("takes.remove", json!({"group": manual, "take": 0})).unwrap();
    assert_eq!(groups(&mut s).iter().find(|g| g["id"] == json!(manual)).unwrap()["takes"].as_array().unwrap().len(), 1);
    s.execute("takes.remove", json!({"group": manual})).unwrap();
    assert_eq!(groups(&mut s).len(), 1);
    // manual groups survive re-detection; detected ones are replaced
    s.execute("takes.label", json!({"group": id, "take": 0, "label": "flat"})).unwrap();
    s.execute("takes.detect", json!({"select": "none"})).unwrap();
    let g = groups(&mut s);
    assert_eq!(g.len(), 1, "the (now manual) group is kept and the fresh detection of the same line is a duplicate: {g:?}");
    assert_eq!(g[0]["takes"][0]["label"], json!("flat"));
    // save and reload keeps everything
    let saved = filmcraft_format::encode(&s.project, true);
    let back = filmcraft_format::decode(&saved).unwrap();
    assert_eq!(back.project.transcripts[&item].takes, s.project.transcripts[&item].takes);
}

#[test]
fn restore_crossed_out_text_and_preview() {
    let (mut s, _) = session();
    // extract the second pass by hand: it shows up as a cut and can be restored
    let before = clip_starts(&s);
    s.execute("transcript.extract", json!({"from": 5, "to": 9})).unwrap();
    assert_eq!(live_text(&mut s), "We start the show here. We start the show here, friends.");
    let cuts = s.execute("transcript.cuts", json!({})).unwrap();
    let c = &cuts["cuts"][0];
    assert_eq!(c["words"].as_array().unwrap().len(), 5, "{cuts}");
    assert_eq!(c["afterWord"], json!(4));
    assert!(s.is_enabled("transcript.restore"));
    s.execute("transcript.restore", json!({"cut": 0})).unwrap();
    assert_eq!(clip_starts(&s), before, "restore is the inverse of extract");
    assert!(!s.is_enabled("transcript.restore"));
    assert!(
        s.execute("transcript.restore", json!({"cut": 0})).unwrap_err().to_string().contains("nothing is crossed out")
            || s.execute("transcript.restore", json!({"cut": 0})).is_err()
    );
    // preview marks In/Out around the live take with one sentence of context on each side
    s.execute("takes.detect", json!({"select": "none"})).unwrap();
    let id = groups(&mut s)[0]["id"].as_u64().unwrap();
    let r = s.execute("takes.preview", json!({"group": id, "take": 1})).unwrap();
    let (i, o) = (r["in"].as_i64().unwrap(), r["out"].as_i64().unwrap());
    let (ts, te) = (r["takeStart"].as_i64().unwrap(), r["takeEnd"].as_i64().unwrap());
    assert!(i < ts && o > te, "{r}");
    let q = s.active_sequence().unwrap();
    assert_eq!(q.mark_in, Some(Tick(i)));
    assert!(q.mark_out.is_some());
    let r0 = s.execute("takes.preview", json!({"group": id, "take": 1, "pre": 0, "post": 0})).unwrap();
    assert_eq!(r0["in"].as_i64().unwrap(), s.sequence_rate().snap(Tick(ts)).0);
}

#[test]
fn hostile_parameters_are_errors_not_crashes() {
    let (mut s, item) = session();
    for (cmd, p) in [
        ("takes.select", json!({"group": 99, "take": 0})),
        ("takes.cross", json!({})),
        ("takes.label", json!({"group": 1})),
        ("takes.merge", json!({"groups": "x"})),
        ("takes.split", json!({"group": 1})),
        ("takes.add", json!({})),
        ("takes.add", json!({"from": 0, "to": 9999})),
        ("takes.add", json!({"item": item.0, "start": 5, "end": 5})),
        ("takes.add", json!({"item": item.0, "start": -5, "end": 5})),
        ("takes.remove", json!({"group": 123456})),
        ("takes.preview", json!({"group": 1, "take": 1})),
        ("takes.detect", json!({"select": "cloud"})),
        ("takes.detect", json!({"items": [999999]})),
        ("transcript.restore", json!({"cut": 42})),
        ("transcript.restore", json!({"item": item.0, "start": 10, "end": 5})),
    ] {
        let e = s.execute(cmd, p.clone());
        assert!(e.is_err(), "{cmd} {p} should fail");
    }
    // with a group: bad indices and labels
    s.execute("takes.detect", json!({"select": "none", "sensitivity": 0.5})).unwrap();
    let id = groups(&mut s)[0]["id"].as_u64().unwrap();
    for (cmd, p) in [
        ("takes.select", json!({"group": id, "take": 99})),
        ("takes.select", json!({"group": id})),
        ("takes.label", json!({"group": id, "take": 0})),
        ("takes.label", json!({"group": id, "take": 0, "label": 5})),
        ("takes.redo", json!({})),
        ("takes.next", json!({"group": id + 1000})),
        ("takes.remove", json!({"group": id, "take": 50})),
        ("takes.preview", json!({"group": id, "take": 3})),
    ] {
        assert!(s.execute(cmd, p.clone()).is_err(), "{cmd} {p} should fail");
    }
    let e = s.execute("takes.select", json!({"group": 99, "take": 0})).unwrap_err().to_string();
    assert!(e.contains("no take group 99"), "{e}");
    // odd but legal numbers are clamped, not refused
    s.execute("takes.detect", json!({"select": "none", "sensitivity": 7.0, "maxGapSeconds": -3.0})).unwrap();
    assert!(s.project.transcripts[&item].check().is_ok());
}

/// A screen recording (another media item, not linked) on V2 over the whole dialogue clip: every
/// take switch and restore keeps it gapless and showing the matching moment.
fn with_screen_track(s: &mut Session) -> ItemId {
    let q = s.active_sequence().unwrap().clone();
    let a = q.audio_tracks[0].items[0].clone();
    let other = s
        .project
        .items
        .values()
        .find(|it| matches!(it.kind, filmcraft_project::ItemKind::Media(ref m) if it.id != a.item && m.duration() >= a.duration + Tick::from_seconds_f64(1.0)))
        .map(|it| it.id)
        .expect("a second media item long enough");
    let seq_id = s.state.active_sequence.unwrap();
    s.edit("screen track", |pr, _| {
        let id = pr.alloc_id();
        let q = pr.sequence_mut(seq_id).unwrap();
        let mut n = a.clone();
        n.id = ClipId(id);
        n.item = other;
        n.source_in = Tick::ZERO;
        // the screen recording runs on past the dialogue clip, as a real one does
        n.duration = a.duration + Tick::from_seconds_f64(1.0);
        n.link = None;
        n.effects.clear();
        q.video_tracks[1].items.retain(|it| it.end() <= a.start || it.start >= n.end());
        q.video_tracks[1].items.push(n);
        q.video_tracks[1].items.sort_by_key(|it| it.start);
        Ok(())
    })
    .unwrap();
    other
}

/// (start, end, source_in) of the clips of `item` on V2, in order.
fn v2_pieces(s: &Session, item: ItemId) -> Vec<(Tick, Tick, Tick)> {
    let q = s.active_sequence().unwrap();
    let mut v: Vec<_> = q.video_tracks[1].items.iter().filter(|it| it.item == item).map(|it| (it.start, it.end(), it.source_in)).collect();
    v.sort();
    v
}

fn assert_in_step(s: &Session, dialogue: ItemId, screen: ItemId, what: &str) {
    let q = s.active_sequence().unwrap();
    let a1: Vec<_> = q.audio_tracks[0].items.iter().filter(|it| it.item == dialogue).map(|it| (it.start, it.end(), it.source_in)).collect();
    let v2 = v2_pieces(s, screen);
    let (a_start, a_end) = (a1.iter().map(|x| x.0).min().unwrap(), a1.iter().map(|x| x.1).max().unwrap());
    // no gap on V2 over the dialogue
    let mut cursor = a_start;
    for (st, en, _) in &v2 {
        assert!(*st <= cursor, "{what}: gap on V2 before {} (covered up to {})", st.seconds(), cursor.seconds());
        cursor = cursor.max(*en);
    }
    assert!(cursor >= a_end, "{what}: V2 ends at {} before the dialogue's {}", cursor.seconds(), a_end.seconds());
    // and in step: at every dialogue piece start, the screen shows media offset by the same amount
    // as when both were laid down (camera source_in − screen source_in, measured on the first pair)
    let offset = {
        let (st, _, sin) = a1.iter().min_by_key(|x| x.0).copied().unwrap();
        let piece = v2.iter().min_by_key(|x| x.0).copied().unwrap();
        sin - (piece.2 + (st - piece.0))
    };
    for (st, _, sin) in &a1 {
        let piece = v2.iter().find(|(a, b, _)| a <= st && st < b).unwrap_or_else(|| panic!("{what}: no V2 piece at {}", st.seconds()));
        let screen_media = piece.2 + (*st - piece.0);
        assert_eq!(screen_media + offset, *sin, "{what}: screen out of step at {}", st.seconds());
    }
}

#[test]
fn take_switching_keeps_an_unlinked_screen_track_gapless_and_in_step() {
    let (mut s, item) = session();
    let screen = with_screen_track(&mut s);
    assert_in_step(&s, item, screen, "before");
    s.execute("takes.detect", json!({})).unwrap();
    assert_in_step(&s, item, screen, "after detect (last take live)");
    let g = groups(&mut s)[0]["id"].as_u64().unwrap();
    for take in [0u64, 1, 2, 1, 0] {
        s.execute("takes.select", json!({"group": g, "take": take})).unwrap();
        assert_in_step(&s, item, screen, &format!("take {take} live"));
    }
    s.execute("takes.cross", json!({"group": g, "take": 0})).unwrap();
    assert_in_step(&s, item, screen, "all crossed out");
    s.execute("takes.restore", json!({"group": g, "take": 2})).unwrap();
    assert_in_step(&s, item, screen, "restored take 2");
    // the plain cut spans too
    let cuts = s.execute("transcript.cuts", json!({})).unwrap()["cuts"].as_array().unwrap().len();
    for ci in (0..cuts).rev() {
        s.execute("transcript.restore", json!({"cut": ci})).unwrap();
    }
    assert_in_step(&s, item, screen, "everything restored");
    // the screen clip's pieces play its media through again: each piece picks up within a frame
    // of where the one before stopped (the take times are not frame-snapped, the edits were)
    let pieces = v2_pieces(&s, screen);
    let frame = s.sequence_rate().frame_duration();
    for w in pieces.windows(2) {
        let media_stop = w[0].2 + (w[0].1 - w[0].0);
        assert!((w[1].2 - media_stop).0.abs() <= frame.0, "screen media jumps between pieces: {pieces:?}");
    }
}

#[test]
fn detect_finds_a_false_start_without_a_pause() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let item = s.active_sequence().unwrap().audio_tracks[0].items[0].item;
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    // 0.05 s between the first "a" and the second "What": one utterance.
    t.words.extend(line(&["What", "a", "what", "a", "time", "to", "be", "alive."], 1.2));
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let r = s.execute("takes.detect", json!({})).unwrap();
    assert_eq!(r["items"][0]["groups"], json!(1), "{r}");
    let g = groups(&mut s);
    assert_eq!(g.len(), 1, "{g:?}");
    let takes = g[0]["takes"].as_array().unwrap();
    assert_eq!(takes.len(), 2, "{g:?}");
    assert_eq!(takes[0]["text"], json!("What a"));
    assert_eq!(takes[1]["text"], json!("what a time to be alive."));
    assert_eq!(g[0]["active"], json!(1));
    assert_eq!(live_text(&mut s), "what a time to be alive.");
}
