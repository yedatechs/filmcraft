//! Deterministic tests of the transcript → sequence mapping and the text-based edits, on the
//! hand-made interview transcript (`tests/fixtures/interview.transcript.json`, 10 s, two speakers,
//! a 2 s pause after "show.", fillers "Um," and "uh", phrase "you know.").

use super::*;
use filmcraft_project::{Label, Project, SequenceSettings, TrackItem};
use filmcraft_time::FrameRate;

const R: FrameRate = FrameRate { num: 25, den: 1 };
const MEDIA: ItemId = ItemId(1);

fn s(x: f64) -> Tick {
    Tick((x * TICKS_PER_SECOND as f64).round() as i64)
}

fn fixture() -> Transcript {
    let mut t: Transcript = serde_json::from_str(include_str!("../../tests/fixtures/interview.transcript.json")).unwrap();
    t.normalize();
    t.check().unwrap();
    t
}

fn transcripts() -> Transcripts {
    let mut m = Transcripts::new();
    m.insert(MEDIA, Arc::new(fixture()));
    m
}

fn seq() -> Sequence {
    let mut p = Project::new("t");
    let id = p.new_sequence("s", SequenceSettings { frame_rate: R, ..Default::default() }, 1, 2, None);
    p.sequence(id).unwrap().clone()
}

fn clip(id: u64, start: f64, dur: f64, src_in: f64) -> TrackItem {
    TrackItem {
        id: ClipId(id),
        item: MEDIA,
        name: "interview".into(),
        label: Label::Iris,
        start: s(start),
        duration: s(dur),
        source_in: s(src_in),
        speed: 1.0,
        reverse: false,
        enabled: true,
        link: None,
        group: None,
        effects: vec![],
        markers: vec![],
        gain_db: 0.0,
        frame_hold: None,
        scale_to_frame: false,
        essential: None,
        multicam: None,
        time_interpolation: Default::default(),
        hold_filters: false,
        field_options: None,
        source_channels: Vec::new(),
        graphic: None,
    }
}

fn texts(w: &[SeqWord]) -> Vec<&str> {
    w.iter().map(|w| w.text.as_str()).collect()
}

fn media_dur(_: ItemId) -> Option<Tick> {
    Some(s(10.0))
}

fn whole() -> Sequence {
    let mut q = seq();
    q.audio_tracks[0].items.push(clip(100, 0.0, 10.0, 0.0));
    q
}

#[test]
fn words_map_through_the_clip() {
    let mut q = seq();
    // media 3.5 s – 6.5 s at timeline 2 s
    q.audio_tracks[0].items.push(clip(100, 2.0, 3.0, 3.5));
    let w = sequence_words(&q, &transcripts());
    assert_eq!(texts(&w), ["Um,", "today", "we", "talk", "about", "rivers."]);
    assert_eq!(w[0].start, s(2.2));
    assert_eq!(w[0].end, s(2.5));
    assert_eq!(w[5].end, s(4.4));
    assert!(w.iter().all(|x| x.clip == ClipId(100) && x.item == MEDIA && x.track == 0));
    assert_eq!(w[0].index, 4);
    assert_eq!(w[0].speaker.as_deref(), Some("Speaker 1"));
}

#[test]
fn a_cut_word_is_kept_by_its_midpoint_and_clamped() {
    let mut q = seq();
    // cut inside "show." (1.20–1.70, midpoint 1.45): the clip starts at media 1.4
    q.audio_tracks[0].items.push(clip(100, 0.0, 1.0, 1.4));
    let w = sequence_words(&q, &transcripts());
    assert_eq!(texts(&w), ["show."]);
    assert_eq!(w[0].start, Tick::ZERO);
    assert_eq!(w[0].end, s(0.3));
}

#[test]
fn speed_scales_word_times() {
    let mut q = seq();
    let mut c = clip(100, 0.0, 5.0, 0.0);
    c.speed = 2.0;
    q.audio_tracks[0].items.push(c);
    let w = sequence_words(&q, &transcripts());
    assert_eq!(w.len(), 20);
    assert_eq!((w[3].start, w[3].end), (s(0.6), s(0.85)));
}

#[test]
fn duplicate_tracks_read_once_and_disabled_or_reversed_clips_are_silent() {
    let mut q = whole();
    q.audio_tracks[1].items.push(clip(101, 0.0, 10.0, 0.0));
    let w = sequence_words(&q, &transcripts());
    assert_eq!(w.len(), 20);
    assert!(w.iter().all(|x| x.track == 0));
    q.audio_tracks[0].items[0].enabled = false;
    let w = sequence_words(&q, &transcripts());
    assert_eq!(w.len(), 20);
    assert!(w.iter().all(|x| x.track == 1));
    q.audio_tracks[1].items[0].reverse = true;
    assert!(sequence_words(&q, &transcripts()).is_empty());
}

#[test]
fn two_clips_of_the_same_media_repeat_words_in_edit_order() {
    let mut q = seq();
    // "rivers." twice: media 5.3–5.9 at 0 s and again at 1 s
    q.audio_tracks[0].items.push(clip(100, 0.0, 1.0, 5.2));
    q.audio_tracks[0].items.push(clip(101, 1.0, 1.0, 5.2));
    let w = sequence_words(&q, &transcripts());
    assert_eq!(texts(&w), ["rivers.", "rivers."]);
    assert_eq!(w[1].start, s(1.1));
    assert_eq!(w[1].clip, ClipId(101));
}

#[test]
fn search_word_at_and_paragraphs() {
    let w = sequence_words(&whole(), &transcripts());
    assert_eq!(search(&w, "rivers"), vec![9..10, 17..18]);
    assert_eq!(search(&w, "YOU kn"), vec![18..20]);
    assert_eq!(search(&w, "talk about"), vec![7..9]);
    assert!(search(&w, "  ").is_empty());
    assert_eq!(word_at(&w, s(4.2)), Some(5));
    assert_eq!(word_at(&w, s(2.5)), None); // in the pause
    assert_eq!(word_at(&w, s(0.1)), None);
    // pause ≥ 1.5 s after "show.", speaker change before "Thanks"
    assert_eq!(paragraphs(&w, s(1.5)), vec![0..4, 4..10, 10..20]);
}

#[test]
fn word_ranges_snap_outward() {
    let w = sequence_words(&whole(), &transcripts());
    // "Welcome to the show." 0.50–1.70 → frames 12..43 (0.48–1.72)
    let r = word_range(&w, 3, 0, R).unwrap();
    assert_eq!((r.start, r.end()), (s(0.48), s(1.72)));
    let m = media_word_range(&fixture(), 4, 9, R).unwrap();
    assert_eq!((m.start, m.end()), (s(3.68), s(5.92)));
    assert!(word_range(&w, 0, 99, R).is_none());
}

#[test]
fn extract_text_closes_the_gap() {
    let mut q = whole();
    let w = sequence_words(&q, &transcripts());
    let r = word_range(&w, 0, 3, R).unwrap();
    let mut next = 1000;
    let mut ctx = EditCtx { next_id: &mut next, media_duration: &media_dur, media_start: &|_| Tick::ZERO, min_duration: R.frame_duration() };
    let tracks: Vec<TrackId> = q.all_tracks().map(|t| t.id).collect();
    crate::extract(&mut q, &tracks, r, &mut ctx);
    q.check().unwrap();
    let w2 = sequence_words(&q, &transcripts());
    assert_eq!(w2.len(), 16);
    assert_eq!(w2[0].text, "Um,");
    assert_eq!(w2[0].start, s(3.70) - r.duration);
}

#[test]
fn pauses_are_found_and_removed_in_one_pass() {
    let mut q = whole();
    let w = sequence_words(&q, &transcripts());
    // only the 2 s pause after "show." is ≥ 1 s; 0.1 s is kept on each side
    let p = find_pauses(&w, s(1.0), s(0.1), R);
    assert_eq!(p.len(), 1);
    assert_eq!((p[0].start, p[0].end()), (s(1.80), s(3.60)));
    // 0.4 s threshold also finds 5.90→6.40 and 7.60→8.00
    let p2 = find_pauses(&w, s(0.4), s(0.1), R);
    assert_eq!(p2.len(), 3);
    let mut next = 1000;
    let mut ctx = EditCtx { next_id: &mut next, media_duration: &media_dur, media_start: &|_| Tick::ZERO, min_duration: R.frame_duration() };
    let removed = ripple_delete_ranges(&mut q, p2.clone(), &mut ctx);
    q.check().unwrap();
    assert_eq!(removed, p2.iter().map(|r| r.duration).fold(Tick::ZERO, |a, b| a + b));
    let w2 = sequence_words(&q, &transcripts());
    assert_eq!(texts(&w2), texts(&w));
    // the long pause is now 0.2 s
    assert_eq!(w2[4].start - w2[3].end, s(0.2));
    assert_eq!(w2.last().unwrap().end, s(10.0) - removed);
    assert!(find_pauses(&w2, s(0.4), s(0.1), R).is_empty());
}

#[test]
fn fillers_are_found_with_phrases_and_removed() {
    let mut q = whole();
    let w = sequence_words(&q, &transcripts());
    let defaults: Vec<String> = DEFAULT_FILLERS.iter().map(|x| x.to_string()).collect();
    let hits = find_fillers(&w, &defaults);
    assert_eq!(hits, vec![4..5, 15..16]);
    let mut more = defaults.clone();
    more.push("You know".into());
    more.push("you".into());
    assert_eq!(find_fillers(&w, &more), vec![4..5, 15..16, 18..20]);
    let ranges = filler_ranges(&w, &hits, R);
    // "Um," 3.70–4.00 → nearest frames 3.68–4.00; "uh" 8.20–8.50 → 8.20–8.48
    assert_eq!(ranges.iter().map(|r| (r.start, r.end())).collect::<Vec<_>>(), vec![(s(3.68), s(4.00)), (s(8.20), s(8.48))]);
    let mut next = 1000;
    let mut ctx = EditCtx { next_id: &mut next, media_duration: &media_dur, media_start: &|_| Tick::ZERO, min_duration: R.frame_duration() };
    ripple_delete_ranges(&mut q, ranges, &mut ctx);
    q.check().unwrap();
    let w2 = sequence_words(&q, &transcripts());
    assert_eq!(w2.len(), 18);
    assert!(find_fillers(&w2, &defaults).is_empty());
    assert_eq!(q.audio_tracks[0].items.len(), 3);
}

#[test]
fn merge_ranges_joins_overlaps() {
    let r = |a: f64, b: f64| TimeRange::from_bounds(s(a), s(b));
    let m = merge_ranges(vec![r(5.0, 6.0), r(1.0, 2.0), r(1.5, 3.0), r(3.0, 4.0)]);
    assert_eq!(m, vec![r(1.0, 4.0), r(5.0, 6.0)]);
}

fn check_blocks(b: &[CaptionBlock], rules: &CaptionRules) {
    for (i, x) in b.iter().enumerate() {
        assert!(x.end > x.start, "{x:?}");
        assert_eq!(R.snap(x.start), x.start);
        assert_eq!(R.snap(x.end), x.end);
        let lines: Vec<&str> = x.text.lines().collect();
        assert!(lines.len() <= rules.lines, "{x:?}");
        assert!(lines.iter().all(|l| l.chars().count() <= rules.max_chars), "{x:?}");
        if i > 0 {
            assert!(x.start >= b[i - 1].end + Tick(R.frame_duration().0 * rules.gap_frames), "{:?} overlaps {:?}", b[i - 1], x);
        }
    }
}

#[test]
fn captions_follow_line_length_speaker_and_pause_rules() {
    let w = sequence_words(&whole(), &transcripts());
    let rules = CaptionRules::default();
    let b = caption_blocks(&w, &rules, R);
    check_blocks(&b, &rules);
    assert_eq!(
        b.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
        ["Welcome to the show.", "Um, today we talk about rivers.", "Thanks for having me. I uh love rivers,\nyou know."]
    );
    assert_eq!((b[0].start, b[0].end), (s(0.48), s(1.72)));
    assert_eq!(b[2].speaker.as_deref(), Some("Speaker 2"));
    assert_eq!(b[2].words, 10..20);

    // single 20-character lines, two-frame gaps, 3 s minimum
    let tight = CaptionRules { max_chars: 20, lines: 1, gap_frames: 2, min_duration: s(3.0), ..Default::default() };
    let b = caption_blocks(&w, &tight, R);
    check_blocks(&b, &tight);
    assert!(b.len() >= 5);
    // the first block is extended to 3 s (room before "Um,")
    assert_eq!(b[0].end - b[0].start, s(3.0));
    // every word is shown exactly once, in order
    let all: Vec<usize> = b.iter().flat_map(|x| x.words.clone()).collect();
    assert_eq!(all, (0..20).collect::<Vec<_>>());
}

#[test]
fn captions_never_overlap_with_dense_words() {
    // 30 one-frame words back to back
    let words: Vec<SeqWord> = (0..30)
        .map(|i| SeqWord {
            text: format!("w{i}."),
            start: R.tick_of(i),
            end: R.tick_of(i + 1),
            clip: ClipId(1),
            item: MEDIA,
            index: i as usize,
            track: 0,
            speaker: if i % 3 == 0 { Some("A".into()) } else { Some("B".into()) },
            confidence: 1.0,
        })
        .collect();
    let rules = CaptionRules { gap_frames: 1, ..Default::default() };
    let b = caption_blocks(&words, &rules, R);
    check_blocks(&b, &rules);
}

// ---------------------------------------------------------------------------------------------
// Cut spans and restore
// ---------------------------------------------------------------------------------------------

const BROLL: ItemId = ItemId(2);
const MUSIC: ItemId = ItemId(3);
const OTHER: ItemId = ItemId(4);
const SCORE: ItemId = ItemId(5);

fn zero(_: ItemId) -> Tick {
    Tick::ZERO
}

/// B-roll has 60 s of media, everything else 10 s.
fn durations(i: ItemId) -> Option<Tick> {
    Some(if i == BROLL || i == SCORE { s(60.0) } else { s(10.0) })
}

fn ctx(next: &mut u64) -> EditCtx<'_> {
    EditCtx { next_id: next, media_duration: &durations, media_start: &zero, min_duration: R.frame_duration() }
}

fn linked_clip(id: u64, item: ItemId, start: f64, dur: f64, src_in: f64, link: u64) -> TrackItem {
    TrackItem { item, link: Some(link), ..clip(id, start, dur, src_in) }
}

/// The interview on A1 with its linked picture on V1, then 2 s of other (linked) media.
fn linked() -> Sequence {
    let mut q = seq();
    q.audio_tracks[0].items.push(linked_clip(100, MEDIA, 0.0, 10.0, 0.0, 1));
    q.video_tracks[0].items.push(linked_clip(200, MEDIA, 0.0, 10.0, 0.0, 1));
    q.audio_tracks[0].items.push(linked_clip(101, OTHER, 10.0, 2.0, 0.0, 2));
    q.video_tracks[0].items.push(linked_clip(201, OTHER, 10.0, 2.0, 0.0, 2));
    q
}

/// Extract words `a..=b` (frame-snapped) on every track; returns the removed timeline range.
fn extract_words(q: &mut Sequence, a: usize, b: usize, ctx: &mut EditCtx) -> TimeRange {
    let w = sequence_words(q, &transcripts());
    let r = word_range(&w, a, b, R).unwrap();
    let tracks: Vec<TrackId> = q.all_tracks().map(|t| t.id).collect();
    crate::extract(q, &tracks, r, ctx);
    q.check().unwrap();
    r
}

fn spans(q: &Sequence) -> Vec<CutSpan> {
    let w = sequence_words(q, &transcripts());
    cut_spans(q, &transcripts(), &w)
}

/// (start, duration, media In) of every item, per track.
fn layout(q: &Sequence) -> Vec<Vec<(Tick, Tick, Tick)>> {
    q.all_tracks().map(|t| t.items.iter().map(|i| (i.start, i.duration, i.source_in)).collect()).collect()
}

#[test]
fn extract_then_cut_spans_finds_the_words() {
    let mut q = linked();
    let mut next = 1000;
    // "Um, today we" (3.70–4.65) → 3.68–4.68
    let r = extract_words(&mut q, 4, 6, &mut ctx(&mut next));
    let sp = spans(&q);
    assert_eq!(sp.len(), 1, "{sp:?}");
    let c = &sp[0];
    assert_eq!(c.item, MEDIA);
    assert_eq!(c.media, r);
    assert_eq!(c.at, r.start);
    assert_eq!(c.track, 0);
    assert_eq!(c.before, ClipId(100));
    assert_eq!(c.after, q.audio_tracks[0].items[1].id);
    assert_eq!(c.words, 4..7);
    let live = sequence_words(&q, &transcripts());
    assert_eq!(c.after_word, Some(3));
    assert_eq!(live[3].text, "show.");
    // the cut media is heard nowhere; the media before it plays where it was
    assert!(live_ranges(&q, MEDIA, c.media).is_empty());
    assert_eq!(live_ranges(&q, MEDIA, TimeRange::from_bounds(Tick::ZERO, r.start)), vec![TimeRange::from_bounds(Tick::ZERO, r.start)]);
    assert_eq!(live_ranges(&q, MEDIA, TimeRange::from_bounds(s(1.0), r.end() + s(1.0))), vec![TimeRange::from_bounds(s(1.0), r.start + s(1.0))]);
}

#[test]
fn restore_after_extract_is_one_clip_with_original_positions() {
    let orig = linked();
    let mut q = orig.clone();
    let mut next = 1000;
    let r = extract_words(&mut q, 4, 6, &mut ctx(&mut next));
    let sp = spans(&q);
    let back = restore_cut(&mut q, &sp[0], &mut ctx(&mut next)).unwrap();
    assert_eq!(back, r);
    q.check().unwrap();
    // one clip again on A1 and V1 (no edit point), later clips back where they were
    assert_eq!(q.audio_tracks[0].items.len(), 2);
    assert_eq!(q.video_tracks[0].items.len(), 2);
    assert_eq!(q, orig);
    assert_eq!(sequence_words(&q, &transcripts()), sequence_words(&orig, &transcripts()));
    assert!(spans(&q).is_empty());
    assert_eq!(live_ranges(&q, MEDIA, r), vec![r]);
}

#[test]
fn partial_restore_inserts_a_clip_and_leaves_the_rest_of_the_span() {
    let mut q = linked();
    let mut next = 1000;
    extract_words(&mut q, 4, 6, &mut ctx(&mut next));
    let c = spans(&q).remove(0);
    let fd = R.frame_duration();
    let inner = TimeRange::from_bounds(c.media.start + fd + fd, c.media.end() - fd);
    let back = restore_media(&mut q, MEDIA, inner, c.at, 0, &mut ctx(&mut next)).unwrap();
    assert_eq!(back, TimeRange::new(c.at, inner.duration));
    q.check().unwrap();
    for t in [&q.audio_tracks[0], &q.video_tracks[0]] {
        assert_eq!(t.items.len(), 4, "{}", t.name);
        let n = &t.items[1];
        assert_eq!((n.start, n.duration, n.source_in, n.speed), (c.at, inner.duration, inner.start, 1.0));
        assert_eq!(n.link, Some(1));
        assert_eq!(n.item, MEDIA);
        assert_eq!(t.items[0].duration, c.at);
        assert_eq!(t.items[2].start, c.at + inner.duration);
    }
    assert_ne!(q.audio_tracks[0].items[1].id, q.video_tracks[0].items[1].id);
    // the rest of the span is still cut, on both sides of the restored piece
    let rest = spans(&q);
    assert_eq!(
        rest.iter().map(|x| x.media).collect::<Vec<_>>(),
        vec![TimeRange::from_bounds(c.media.start, inner.start), TimeRange::from_bounds(inner.end(), c.media.end())]
    );
    assert_eq!(live_ranges(&q, MEDIA, c.media), vec![back]);
}

#[test]
fn restoring_the_head_of_a_span_grows_the_clip_and_its_linked_picture() {
    let mut q = linked();
    let mut next = 1000;
    extract_words(&mut q, 4, 6, &mut ctx(&mut next));
    let c = spans(&q).remove(0);
    let head = TimeRange::new(c.media.start, s(0.4));
    restore_media(&mut q, MEDIA, head, c.at, 0, &mut ctx(&mut next)).unwrap();
    q.check().unwrap();
    for t in [&q.audio_tracks[0], &q.video_tracks[0]] {
        assert_eq!(t.items.len(), 3, "{}", t.name);
        assert_eq!(t.items[0].duration, c.at + s(0.4));
        assert_eq!(t.items[1].start, c.at + s(0.4));
    }
    assert_eq!(q.video_tracks[0].items[0].id, ClipId(200));
    let rest = spans(&q);
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].media, TimeRange::from_bounds(head.end(), c.media.end()));
    assert_eq!(rest[0].at, c.at + s(0.4));
}

#[test]
fn remove_pauses_leaves_wordless_spans_that_restore() {
    let orig = whole();
    let mut q = orig.clone();
    let w = sequence_words(&q, &transcripts());
    let p = find_pauses(&w, s(0.4), s(0.1), R);
    assert_eq!(p.len(), 3);
    let mut next = 1000;
    ripple_delete_ranges(&mut q, p.clone(), &mut ctx(&mut next));
    let sp = spans(&q);
    assert_eq!(sp.len(), 3);
    assert!(sp.iter().all(|c| c.words.is_empty()));
    // the clip played media 0–10 s from 0 s, so the removed media is the removed timeline
    assert_eq!(sp.iter().map(|c| c.media).collect::<Vec<_>>(), p);
    // right to left, the precomputed spans stay valid
    for c in sp.iter().rev() {
        restore_cut(&mut q, c, &mut ctx(&mut next)).unwrap();
        q.check().unwrap();
    }
    assert_eq!(q, orig);
}

#[test]
fn different_media_is_not_a_span() {
    let mut q = seq();
    let mut tr = transcripts();
    tr.insert(OTHER, Arc::new(fixture()));
    q.audio_tracks[0].items.push(clip(100, 0.0, 2.0, 0.0));
    q.audio_tracks[0].items.push(TrackItem { item: OTHER, ..clip(101, 2.0, 2.0, 5.0) });
    let w = sequence_words(&q, &tr);
    assert!(cut_spans(&q, &tr, &w).is_empty());
    // same media played again from earlier (a repeat) is not a span either
    q.audio_tracks[0].items[1].item = MEDIA;
    q.audio_tracks[0].items[1].source_in = s(1.0);
    assert!(cut_spans(&q, &tr, &sequence_words(&q, &tr)).is_empty());
    // nor is media without a transcript
    q.audio_tracks[0].items[1].source_in = s(5.0);
    assert_eq!(cut_spans(&q, &tr, &sequence_words(&q, &tr)).len(), 1);
    assert!(cut_spans(&q, &Transcripts::new(), &[]).is_empty());
}

#[test]
fn a_locked_track_is_unmoved() {
    let mut q = linked();
    let mut next = 1000;
    extract_words(&mut q, 4, 6, &mut ctx(&mut next));
    let c = spans(&q).remove(0);
    q.audio_tracks[1].items.push(TrackItem { item: MUSIC, ..clip(300, 5.0, 1.0, 0.0) });
    q.audio_tracks[1].locked = true;
    let a2 = q.audio_tracks[1].clone();
    restore_cut(&mut q, &c, &mut ctx(&mut next)).unwrap();
    q.check().unwrap();
    assert_eq!(q.audio_tracks[1], a2);
    assert_eq!(q.audio_tracks[0].items.len(), 2);
    assert_eq!(q.audio_tracks[0].items[1].start, s(10.0));
    // a locked A1 can't take the media back
    let mut q2 = linked();
    extract_words(&mut q2, 4, 6, &mut ctx(&mut next));
    q2.audio_tracks[0].locked = true;
    let before = q2.clone();
    let c2 = spans(&q2).remove(0);
    assert_eq!(restore_cut(&mut q2, &c2, &mut ctx(&mut next)), Err(EditError::Locked));
    assert_eq!(q2, before);
}

#[test]
fn broll_spanning_the_anchor_is_lengthened_and_exhausted_music_is_split() {
    let mut q = linked();
    let mut next = 1000;
    extract_words(&mut q, 4, 6, &mut ctx(&mut next));
    let c = spans(&q).remove(0);
    let at = c.at;
    let dur = c.media.duration;
    // B-roll laid over the cut afterwards on V2 (60 s of media), music under it on A2 whose media
    // runs out at 10 s
    let mut v2 = Track::new(TrackId(900), filmcraft_project::TrackKind::Video, "Video 2".into());
    v2.items.push(TrackItem { item: BROLL, ..clip(300, 3.0, 2.0, 1.0) });
    q.video_tracks.push(v2);
    q.audio_tracks[1].items.push(TrackItem { item: MUSIC, ..clip(301, 2.0, 4.0, 6.0) });
    restore_cut(&mut q, &c, &mut ctx(&mut next)).unwrap();
    q.check().unwrap();
    let b = &q.video_tracks[1].items;
    assert_eq!(b.len(), 1);
    assert_eq!((b[0].start, b[0].duration, b[0].source_in), (s(3.0), s(2.0) + dur, s(1.0)));
    let m = &q.audio_tracks[1].items;
    assert_eq!(m.len(), 2);
    assert_eq!((m[0].id, m[0].start, m[0].duration), (ClipId(301), s(2.0), at - s(2.0)));
    assert_eq!((m[1].start, m[1].duration, m[1].source_in), (at + dur, s(6.0) - at, s(6.0) + (at - s(2.0))));
    // B-roll whose media would run out is split too
    let mut q3 = linked();
    extract_words(&mut q3, 4, 6, &mut ctx(&mut next));
    let mut v2 = Track::new(TrackId(900), filmcraft_project::TrackKind::Video, "Video 2".into());
    v2.items.push(TrackItem { item: BROLL, ..clip(300, 3.0, 2.0, 59.0) });
    q3.video_tracks.push(v2);
    let c3 = spans(&q3).remove(0);
    restore_cut(&mut q3, &c3, &mut ctx(&mut next)).unwrap();
    q3.check().unwrap();
    let b = &q3.video_tracks[1].items;
    assert_eq!(b.len(), 2);
    assert_eq!((b[0].start, b[0].duration), (s(3.0), at - s(3.0)));
    assert_eq!(b[1].start, at + dur);
}

#[test]
fn a_speed_changed_clip_before_the_span_gets_an_insert() {
    let mut q = seq();
    // media 0–2 s at 200% in 0–1 s, then media 3–5 s from 1 s: media 2–3 s is cut
    let mut a = linked_clip(100, MEDIA, 0.0, 1.0, 0.0, 1);
    a.speed = 2.0;
    q.audio_tracks[0].items.push(a.clone());
    q.video_tracks[0].items.push(TrackItem { id: ClipId(200), ..a });
    q.audio_tracks[0].items.push(linked_clip(101, MEDIA, 1.0, 2.0, 3.0, 2));
    q.video_tracks[0].items.push(linked_clip(201, MEDIA, 1.0, 2.0, 3.0, 2));
    let sp = spans(&q);
    assert_eq!(sp.len(), 1);
    assert_eq!((sp[0].media, sp[0].at), (TimeRange::from_bounds(s(2.0), s(3.0)), s(1.0)));
    let mut next = 1000;
    restore_cut(&mut q, &sp[0], &mut ctx(&mut next)).unwrap();
    q.check().unwrap();
    // the piece that goes back (media 2–3 s at 1×) runs straight into the clip after it (media
    // 3–5 s), so that clip reaches back over it: one piece of media 2–5 s from 1 s, still the
    // clip it was (its id, its link to its own partner)
    for (t, id) in [(&q.audio_tracks[0], 101), (&q.video_tracks[0], 201)] {
        assert_eq!(t.items.len(), 2, "{}", t.name);
        assert_eq!((t.items[0].duration, t.items[0].speed), (s(1.0), 2.0));
        let n = &t.items[1];
        assert_eq!((n.start, n.duration, n.source_in, n.speed, n.link), (s(1.0), s(3.0), s(2.0), 1.0, Some(2)));
        assert_eq!(n.id, ClipId(id));
    }
    assert!(spans(&q).is_empty());
}

/// Like the demo project: [`linked`], plus a sync-locked score on A2 under the whole sequence.
fn scored() -> Sequence {
    let mut q = linked();
    q.audio_tracks[1].items.push(TrackItem { item: SCORE, ..clip(400, 0.0, 12.0, 5.0) });
    q
}

#[test]
fn restore_rejoins_music_and_broll_that_extract_split() {
    let mut orig = scored();
    let mut v2 = Track::new(TrackId(900), filmcraft_project::TrackKind::Video, "Video 2".into());
    v2.items.push(TrackItem { item: BROLL, ..clip(300, 2.0, 6.0, 1.0) });
    orig.video_tracks.push(v2);
    let mut q = orig.clone();
    let mut next = 1000;
    // like ripple_delete_ranges: every unlocked track
    let r = extract_words(&mut q, 4, 6, &mut ctx(&mut next));
    assert_eq!(q.audio_tracks[1].items.len(), 2);
    assert_eq!(q.video_tracks[1].items.len(), 2);
    let c = spans(&q).remove(0);
    assert_eq!(restore_cut(&mut q, &c, &mut ctx(&mut next)), Ok(r));
    q.check().unwrap();
    assert_eq!(layout(&q), layout(&orig));
    assert_eq!(q.all_tracks().map(|t| t.items.len()).collect::<Vec<_>>(), orig.all_tracks().map(|t| t.items.len()).collect::<Vec<_>>());
    assert_eq!(q, orig);
}

/// An effect instance that stands for "this clip was given a look": `name` tells them apart.
fn look(name: &str) -> filmcraft_project::EffectInstance {
    let mut e = filmcraft_project::EffectInstance {
        effect: name.to_string(),
        enabled: true,
        params: Default::default(),
        masks: Vec::new(),
        post_fader: false,
        essential: false,
        layer: None,
    };
    let mut p = filmcraft_project::Param::new(filmcraft_project::ParamValue::Float(1.0));
    p.keyframes.push(filmcraft_project::Keyframe {
        time: s(0.5),
        value: filmcraft_project::ParamValue::Float(2.0),
        interp: Default::default(),
        out_influence: 1.0 / 3.0,
        in_influence: 1.0 / 3.0,
    });
    e.params.insert("amount".into(), p);
    e
}

/// The picture and the dialogue after words 4..=6 were crossed out, with a look given to the
/// picture clip AFTER the cut only (a layout put on one clip, a redaction): (sequence, the cut).
fn cut_with_a_look_after_it(next: &mut u64) -> (Sequence, CutSpan) {
    let mut q = linked();
    extract_words(&mut q, 4, 6, &mut ctx(next));
    let c = spans(&q).remove(0);
    let after = q.video_tracks[0].items.iter_mut().find(|i| i.start == c.at).unwrap();
    after.effects.push(look("mosaic"));
    (q, c)
}

#[test]
fn a_piece_that_runs_into_the_next_clip_becomes_part_of_it_and_the_clip_keeps_its_effects() {
    let mut next = 1000;
    let (mut q, c) = cut_with_a_look_after_it(&mut next);
    let (v_after, a_after) = (q.video_tracks[0].items[1].clone(), q.audio_tracks[0].items[1].clone());
    // the tail of the cut goes back: it ends where the clip after the cut begins
    let fd = R.frame_duration();
    let tail = TimeRange::from_bounds(c.media.start + fd + fd, c.media.end());
    restore_media(&mut q, MEDIA, tail, c.at, 0, &mut ctx(&mut next)).unwrap();
    q.check().unwrap();
    for (t, was) in [(&q.video_tracks[0], &v_after), (&q.audio_tracks[0], &a_after)] {
        assert_eq!(t.items.len(), 3, "{}: no new clip, the clip after the cut reaches back", t.name);
        let n = &t.items[1];
        assert_eq!(n.id, was.id, "{}: the same clip", t.name);
        assert_eq!((n.start, n.source_in, n.duration), (c.at, tail.start, was.duration + tail.duration));
    }
    let v = &q.video_tracks[0].items[1];
    assert_eq!(v.effects.len(), 1, "the look is still on the clip");
    assert_eq!(v.effects[0].effect, "mosaic");
    // the clip starts earlier, so its keyframe moves later by as much and stays on the same picture
    assert_eq!(v.effects[0].params["amount"].keyframes[0].time, s(0.5) + tail.duration);
    assert!(q.video_tracks[0].items[0].effects.is_empty(), "the clip before the cut is as it was");
    // what is left of the cut is its head
    let rest = spans(&q);
    assert_eq!(rest.iter().map(|x| x.media).collect::<Vec<_>>(), vec![TimeRange::from_bounds(c.media.start, tail.start)]);
}

#[test]
fn a_whole_cut_put_back_between_two_different_looks_leaves_both_clips_as_they_are() {
    let mut next = 1000;
    let (mut q, c) = cut_with_a_look_after_it(&mut next);
    let ids: Vec<ClipId> = q.video_tracks[0].items.iter().map(|i| i.id).collect();
    restore_cut(&mut q, &c, &mut ctx(&mut next)).unwrap();
    q.check().unwrap();
    // the picture: the clip before grew over the cut, and is not joined with the clip after it,
    // which looks different (joining would throw one clip's look away)
    let v = &q.video_tracks[0];
    assert_eq!(v.items.iter().map(|i| i.id).collect::<Vec<_>>(), ids);
    assert!(v.items[0].effects.is_empty());
    assert_eq!(v.items[0].end(), c.at + c.media.duration);
    assert_eq!(v.items[1].effects.len(), 1);
    assert_eq!(v.items[1].effects[0].params["amount"].keyframes[0].time, s(0.5), "untouched");
    // the dialogue, which sounds the same on both sides, is one clip again
    assert_eq!(q.audio_tracks[0].items.len(), 2);
    assert!(spans(&q).is_empty());
}

#[test]
fn a_piece_in_the_middle_of_a_cut_looks_like_the_nearer_clip() {
    for near_the_end in [false, true] {
        let mut next = 1000;
        let (mut q, c) = cut_with_a_look_after_it(&mut next);
        let fd = R.frame_duration();
        // one frame from one end of the cut, several from the other
        let len = c.media.duration.0 / fd.0;
        assert!(len >= 8, "the cut is long enough for the test ({len} frames)");
        let start = if near_the_end { c.media.end() - fd - fd - fd } else { c.media.start + fd };
        let piece = TimeRange::new(start, fd + fd);
        restore_media(&mut q, MEDIA, piece, c.at, 0, &mut ctx(&mut next)).unwrap();
        q.check().unwrap();
        let v = &q.video_tracks[0];
        assert_eq!(v.items.len(), 4, "an island: a clip of its own");
        let n = &v.items[1];
        assert_eq!((n.start, n.duration, n.source_in), (c.at, piece.duration, piece.start));
        assert_eq!(n.effects.len(), usize::from(near_the_end), "near the end: {near_the_end}");
        assert!(v.items[0].effects.is_empty());
        assert_eq!(v.items[2].effects.len(), 1);
    }
}

/// A transcript of `words` (text, start s, end s) and the cut span of media `lo..hi` holding
/// the words whose midpoint lies in it.
fn word_cut(words: &[(&str, f64, f64)], lo: f64, hi: f64) -> (Transcript, CutSpan) {
    let t = Transcript {
        words: words
            .iter()
            .map(|(text, a, b)| filmcraft_project::Word { text: (*text).into(), start: s(*a), end: s(*b), speaker: None, confidence: 1.0 })
            .collect(),
        ..Default::default()
    };
    let media = TimeRange::from_bounds(s(lo), s(hi));
    let cut = CutSpan { item: MEDIA, media, at: s(1.0), track: 0, before: ClipId(1), after: ClipId(2), words: t.words_within(media), after_word: None };
    (t, cut)
}

#[test]
fn one_crossed_out_word_goes_back_without_its_neighbours_or_the_pauses() {
    // "to | uh filmcraft I don't … um wait so I don't think you see all | of": the cut runs from
    // 7.8 s (after "to") to 16.08 s (before "of"), as in the owner's recording
    let words = [
        ("to", 7.35, 7.72),
        ("uh", 8.54, 8.62),
        ("filmcraft", 8.96, 9.48),
        ("I", 9.68, 9.82),
        ("don't", 9.82, 9.88),
        ("wait", 14.24, 14.74),
        ("so", 15.27, 15.36),
        ("I", 15.36, 15.46),
        ("see", 15.78, 15.94),
        ("all", 15.94, 16.04),
        ("of", 16.14, 16.24),
    ];
    let (t, cut) = word_cut(&words, 7.8, 16.08);
    assert_eq!(cut.words, 1..10);
    let fd = R.frame_duration();
    let whole = |r: TimeRange| (r.start - cut.media.start).0 % fd.0 == 0 && (r.duration.0 % fd.0 == 0 || r.end() == cut.media.end());
    // "filmcraft" alone: a little before it and after it, not "uh" before nor "I" after
    let r = restore_range_for_words(&t, &cut, 2, 2, R).unwrap();
    assert!(whole(r), "{r:?}");
    assert!(r.start >= s(8.62) && r.start <= s(8.96), "after uh, before the word: {}", r.start.seconds());
    assert!(r.end() >= s(9.48) && r.end() <= s(9.68), "after the word, before I: {}", r.end().seconds());
    assert!(r.duration < s(0.9), "the word, not the pauses around it");
    // the last words of the cut reach its end (they join the clip after it); the start does not
    // go back into "wait"
    let r = restore_range_for_words(&t, &cut, 6, 9, R).unwrap();
    assert_eq!(r.end(), cut.media.end());
    assert!(r.start >= s(14.74) && r.start <= s(15.27), "{}", r.start.seconds());
    assert!(whole(r));
    // the first word of the cut: 0.74 s of pause lies before it, which stays removed
    let r = restore_range_for_words(&t, &cut, 1, 1, R).unwrap();
    assert!(r.start > cut.media.start + s(0.5) && r.start <= s(8.54), "{}", r.start.seconds());
    assert!(r.end() <= s(8.96));
    // a word right at the start of a cut joins the clip before it
    let (t2, cut2) = word_cut(&[("a", 1.0, 1.2), ("b", 1.25, 1.5), ("c", 3.0, 3.4)], 0.9, 3.5);
    let r = restore_range_for_words(&t2, &cut2, 0, 0, R).unwrap();
    assert_eq!(r.start, cut2.media.start);
    assert!(r.end() <= s(1.25) + fd && r.end() >= s(1.2));
    // a range in either order; everything: the whole cut
    assert_eq!(restore_range_for_words(&t2, &cut2, 1, 0, R), restore_range_for_words(&t2, &cut2, 0, 1, R));
    assert_eq!(restore_range_for_words(&t2, &cut2, 0, 2, R), Some(cut2.media));
}

#[test]
fn restoring_words_one_by_one_puts_the_whole_cut_back_without_slivers() {
    let mut q = linked();
    let mut next = 1000;
    let orig = q.clone();
    extract_words(&mut q, 2, 7, &mut ctx(&mut next));
    let tr = fixture();
    // the last word first, then the others from the end: each click restores one word
    for _ in 0..32 {
        let Some(c) = spans(&q).into_iter().find(|c| !c.words.is_empty()) else { break };
        let w = c.words.end - 1;
        let r = restore_range_for_words(&tr, &c, w, w, R).unwrap();
        assert!(r.start >= c.media.start && r.end() <= c.media.end());
        restore_media(&mut q, MEDIA, r, c.at, c.track, &mut ctx(&mut next)).unwrap();
        q.check().unwrap();
        for c in spans(&q) {
            assert!(c.media.duration >= R.frame_duration(), "no sliver under a frame stays cut: {:?}", c.media);
        }
    }
    let left = spans(&q);
    assert!(left.iter().all(|c| c.words.is_empty()), "every word is back: {left:?}");
    // what is still cut is pauses only; putting those back gives the original edit
    for _ in 0..8 {
        let Some(c) = spans(&q).into_iter().next() else { break };
        restore_cut(&mut q, &c, &mut ctx(&mut next)).unwrap();
    }
    assert_eq!(layout(&q), layout(&orig));
}

#[test]
fn hostile_word_ranges_are_none_not_a_panic() {
    let (t, cut) = word_cut(&[("a", 1.0, 1.2), ("b", 1.25, 1.5)], 0.9, 1.6);
    assert_eq!(restore_range_for_words(&t, &cut, 5, 9, R), None);
    assert_eq!(restore_range_for_words(&t, &cut, usize::MAX, 0, R), None);
    let empty = CutSpan { media: TimeRange::new(s(1.0), Tick(0)), ..cut.clone() };
    assert_eq!(restore_range_for_words(&t, &empty, 0, 0, R), None);
    let zero_rate = FrameRate { num: 0, den: 1 };
    let _ = restore_range_for_words(&t, &cut, 0, 1, zero_rate);
    // words whose times are reversed or far outside the cut still give a range inside it
    let (t, cut) = word_cut(&[("x", 5.0, 4.0), ("y", 4.5, 4.6)], 4.2, 4.9);
    for (a, b) in [(0, 0), (1, 1), (0, 1)] {
        if let Some(r) = restore_range_for_words(&t, &cut, a, b, R) {
            assert!(r.start >= cut.media.start && r.end() <= cut.media.end() && r.duration >= R.frame_duration(), "{r:?}");
        }
    }
}

#[test]
fn a_genuine_edit_at_the_anchor_is_not_grown() {
    let mut q = linked();
    let mut next = 1000;
    extract_words(&mut q, 4, 6, &mut ctx(&mut next));
    let c = spans(&q).remove(0);
    let (at, dur) = (c.at, c.media.duration);
    // music cut at the anchor, but resuming one frame later than a split by this cut would
    let jump = s(5.0) + (at - s(2.0)) + dur + R.frame_duration();
    q.audio_tracks[1].items.push(TrackItem { item: SCORE, ..clip(400, 2.0, at.seconds() - 2.0, 5.0) });
    q.audio_tracks[1].items[0].duration = at - s(2.0);
    q.audio_tracks[1].items.push(TrackItem { item: SCORE, source_in: jump, duration: s(2.0), start: at, ..clip(401, 0.0, 0.0, 0.0) });
    restore_cut(&mut q, &c, &mut ctx(&mut next)).unwrap();
    q.check().unwrap();
    let m = &q.audio_tracks[1].items;
    assert_eq!(m.len(), 2);
    assert_eq!((m[0].start, m[0].duration), (s(2.0), at - s(2.0)));
    assert_eq!((m[1].id, m[1].start, m[1].duration, m[1].source_in), (ClipId(401), at + dur, s(2.0), jump));
}

#[test]
fn extract_then_restore_round_trips_every_word_range() {
    let orig = scored();
    let mut next = 1000;
    let mut restored = 0;
    for a in 0..20 {
        for b in a..20 {
            let mut q = orig.clone();
            let r = extract_words(&mut q, a, b, &mut ctx(&mut next));
            let sp = spans(&q);
            if r.end() >= s(10.0) {
                // the tail of the media: not a span
                assert!(sp.is_empty(), "{a}..={b}");
                continue;
            }
            assert_eq!(sp.len(), 1, "{a}..={b}");
            assert_eq!(sp[0].media, r, "{a}..={b}");
            assert_eq!(restore_cut(&mut q, &sp[0], &mut ctx(&mut next)), Ok(r), "{a}..={b}");
            q.check().unwrap();
            assert_eq!(layout(&q), layout(&orig), "{a}..={b}");
            assert_eq!(q, orig, "{a}..={b}");
            restored += 1;
        }
    }
    assert_eq!(restored, 190);
}

#[test]
fn hostile_restores_fail_and_change_nothing() {
    let mut q = linked();
    let mut next = 1000;
    extract_words(&mut q, 4, 6, &mut ctx(&mut next));
    let c = spans(&q).remove(0);
    let keep = q.clone();
    let mut bad = |media: TimeRange, at: Tick, track: usize| {
        let e = restore_media(&mut q, MEDIA, media, at, track, &mut ctx(&mut next));
        assert_eq!(q, keep);
        e
    };
    assert_eq!(bad(TimeRange::new(c.media.start, Tick::ZERO), c.at, 0), Err(EditError::Nothing));
    assert_eq!(bad(TimeRange::new(c.media.start, Tick(-5)), c.at, 0), Err(EditError::Nothing));
    assert_eq!(bad(TimeRange::new(Tick(-100), s(1.0)), c.at, 0), Err(EditError::NoHandles));
    assert_eq!(bad(TimeRange::new(s(9.5), s(1.0)), c.at, 0), Err(EditError::NoHandles));
    assert_eq!(bad(TimeRange::new(Tick(i64::MAX - 5), Tick(100)), c.at, 0), Err(EditError::NoHandles));
    assert_eq!(bad(TimeRange::new(c.media.start, Tick(10)), c.at, 0), Err(EditError::TooShort));
    assert!(matches!(bad(c.media, c.at, 2), Err(EditError::Other(_))));
    assert!(matches!(bad(c.media, c.at, usize::MAX), Err(EditError::Other(_))));
    // no clip of the media ends there
    assert!(matches!(bad(c.media, s(1.0), 0), Err(EditError::Other(_))));
    assert!(matches!(bad(c.media, Tick(i64::MIN), 0), Err(EditError::Other(_))));
    assert!(matches!(bad(c.media, c.at, 1), Err(EditError::Other(_))));
    // `before` not on the track, or not where the span says
    let mut q2 = keep.clone();
    let gone = CutSpan { before: ClipId(9999), ..c.clone() };
    assert_eq!(restore_cut(&mut q2, &gone, &mut ctx(&mut next)), Err(EditError::NoItem(ClipId(9999))));
    let elsewhere = CutSpan { track: 1, ..c.clone() };
    assert_eq!(restore_cut(&mut q2, &elsewhere, &mut ctx(&mut next)), Err(EditError::NoItem(c.before)));
    let stale = CutSpan { at: c.at + s(1.0), ..c.clone() };
    assert!(matches!(restore_cut(&mut q2, &stale, &mut ctx(&mut next)), Err(EditError::Other(_))));
    let wrong_track = CutSpan { track: 7, ..c.clone() };
    assert!(matches!(restore_cut(&mut q2, &wrong_track, &mut ctx(&mut next)), Err(EditError::Other(_))));
    assert_eq!(q2, keep);
}

#[test]
fn live_ranges_survive_hostile_ranges_and_speeds() {
    let mut q = whole();
    assert!(live_ranges(&q, MEDIA, TimeRange::new(s(1.0), Tick::ZERO)).is_empty());
    assert!(live_ranges(&q, MEDIA, TimeRange::new(Tick(i64::MAX), Tick(10))).is_empty());
    assert!(live_ranges(&q, MEDIA, TimeRange::new(s(20.0), s(1.0))).is_empty());
    assert!(live_ranges(&q, OTHER, TimeRange::new(s(1.0), s(1.0))).is_empty());
    assert_eq!(live_ranges(&q, MEDIA, TimeRange::from_bounds(Tick(i64::MIN / 2), s(1.0))), vec![TimeRange::new(Tick::ZERO, s(1.0))]);
    // absurd speeds: the clip plays (almost) no media, or a frame of media maps to nothing
    q.audio_tracks[0].items[0].speed = 1e-300;
    assert!(live_ranges(&q, MEDIA, TimeRange::new(Tick::ZERO, s(1.0))).is_empty());
    q.audio_tracks[0].items[0].speed = 1e300;
    assert!(live_ranges(&q, MEDIA, TimeRange::new(Tick::ZERO, Tick(1))).is_empty());
    q.audio_tracks[0].items[0].speed = f64::NAN;
    assert!(live_ranges(&q, MEDIA, TimeRange::new(Tick::ZERO, s(1.0))).is_empty());
    // a duplicate on A2, offset: merged with A1 where they touch
    q.audio_tracks[0].items[0].speed = 1.0;
    q.audio_tracks[1].items.push(clip(101, 10.0, 2.0, 0.0));
    assert_eq!(live_ranges(&q, MEDIA, TimeRange::new(Tick::ZERO, s(1.0))), vec![TimeRange::new(Tick::ZERO, s(1.0)), TimeRange::new(s(10.0), s(1.0))]);
}
