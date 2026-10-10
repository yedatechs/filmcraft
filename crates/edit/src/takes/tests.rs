use super::*;

/// Seconds as ticks.
fn s(x: f64) -> Tick {
    Tick::from_seconds_f64(x)
}

/// A transcript of lines `(start second, text)`; words are 0.3 s long, 0.05 s apart.
fn transcript(lines: &[(f64, &str)]) -> Transcript {
    let mut words = Vec::new();
    for &(start, text) in lines {
        let mut t = start;
        for w in text.split_whitespace() {
            words.push(Word::new(w, s(t), s(t + 0.3)));
            t += 0.35;
        }
    }
    Transcript { words, ..Default::default() }
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(normalize_word).filter(|w| !w.is_empty()).collect()
}

fn starts(g: &TakeGroup) -> Vec<Tick> {
    g.takes.iter().map(|t| t.range.start).collect()
}

#[test]
fn utterances_split_at_pauses_and_punctuation() {
    let t = transcript(&[(0.0, "one two. three four"), (5.0, "five six")]);
    let u = utterances(&t.words, s(0.5));
    assert_eq!(u, vec![0..2, 2..4, 4..6]);
    assert!(utterances(&[], s(0.5)).is_empty());
}

#[test]
fn similarity_terms() {
    assert_eq!(similarity(&words("a b c"), &words("a b c")), 1.0);
    assert_eq!(similarity(&words("today we"), &words("today we talk about rivers")), 1.0);
    assert_eq!(similarity(&words("the weather is nice"), &words("my cat sleeps all day")), 0.0);
    assert_eq!(similarity(&[], &words("a")), 0.0);
    let long: Vec<String> = (0..500).map(|i| format!("w{i}")).collect();
    assert_eq!(similarity(&long, &long), 1.0);
}

#[test]
fn exact_repeat_grouped() {
    let t = transcript(&[(0.0, "today we talk about rivers."), (3.0, "today we talk about rivers.")]);
    let g = detect(&t, &DetectParams::default());
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].id, 0);
    assert!(!g[0].manual);
    assert_eq!(starts(&g[0]), vec![s(0.0), s(3.0)]);
    // From the first word's start to the last word's end.
    assert_eq!(g[0].takes[0].range, TimeRange::from_bounds(s(0.0), s(4.0 * 0.35 + 0.3)));
}

#[test]
fn reworded_repeat_grouped() {
    let t = transcript(&[(0.0, "today we talk about rivers."), (3.0, "today we talk about rivers."), (6.0, "so today, rivers, we talk about them.")]);
    let g = detect(&t, &DetectParams::default());
    assert_eq!(g.len(), 1);
    assert_eq!(starts(&g[0]), vec![s(0.0), s(3.0), s(6.0)]);
}

#[test]
fn false_start_grouped() {
    let t = transcript(&[(0.0, "the main reason"), (2.0, "the main reason we came here is the water.")]);
    let g = detect(&t, &DetectParams::default());
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].takes.len(), 2);
}

#[test]
fn cue_words_help_grouping() {
    let a = "today we look at the rivers of europe.";
    let b = "european rivers are what we look at today.";
    let p = DetectParams::default();
    // Too different on their own...
    assert!(similarity(&words(a), &words(b)) < p.threshold());
    let plain = transcript(&[(0.0, a), (4.0, b)]);
    assert!(detect(&plain, &p).is_empty());
    // ...but a retake cue lowers the bar.
    let cued = transcript(&[(0.0, a), (4.0, &format!("okay again, {b}"))]);
    let g = detect(&cued, &p);
    assert_eq!(g.len(), 1);
    assert_eq!(starts(&g[0]), vec![s(0.0), s(4.0)]);
}

#[test]
fn unrelated_lines_not_grouped() {
    let t = transcript(&[(0.0, "the weather is nice today."), (3.0, "my cat sleeps all day."), (6.0, "rivers carry water downhill.")]);
    for sens in [0.0, 0.5, 1.0] {
        assert!(detect(&t, &DetectParams { sensitivity: sens, ..Default::default() }).is_empty(), "sensitivity {sens}");
    }
}

#[test]
fn max_gap_respected() {
    let t = transcript(&[(0.0, "today we talk about rivers."), (32.0, "today we talk about rivers.")]);
    assert!(detect(&t, &DetectParams::default()).is_empty());
    let wide = DetectParams { max_gap: s(60.0), ..Default::default() };
    assert_eq!(detect(&t, &wide).len(), 1);
}

#[test]
fn higher_sensitivity_never_fewer_groups() {
    let t = transcript(&[
        (0.0, "today we talk about rivers."),
        (3.0, "today we talk about rivers."),
        (6.0, "the weather is nice today."),
        (9.0, "rivers carry water to the sea."),
        (12.0, "rivers carry the water to the ocean."),
        (16.0, "my cat sleeps all day long."),
        (19.0, "my dog sleeps at night."),
        (22.0, "mountains are made of rock."),
        (25.0, "hills are made of soft rock and clay."),
    ]);
    let counts: Vec<usize> = [0.2f32, 0.5, 0.8].iter().map(|&x| detect(&t, &DetectParams { sensitivity: x, ..Default::default() }).len()).collect();
    assert!(counts.windows(2).all(|w| w[0] <= w[1]), "{counts:?}");
    assert!(counts[0] >= 1);
    assert!(counts[2] > counts[0], "{counts:?}");
}

#[test]
fn empty_and_one_word() {
    assert!(detect(&Transcript::default(), &DetectParams::default()).is_empty());
    let one = transcript(&[(0.0, "hello")]);
    assert!(detect(&one, &DetectParams::default()).is_empty());
}

#[test]
fn deterministic() {
    let t = transcript(&[(0.0, "today we talk about rivers."), (3.0, "today we talk about rivers."), (6.0, "so today, rivers, we talk about them.")]);
    let p = DetectParams::default();
    assert_eq!(detect(&t, &p), detect(&t, &p));
}

#[test]
fn merge_then_split_round_trip() {
    let t = transcript(&[
        (0.0, "today we talk about rivers."),
        (3.0, "today we talk about rivers."),
        (6.0, "my cat sleeps all day long."),
        (9.0, "my cat sleeps all day long."),
    ]);
    let mut groups = detect(&t, &DetectParams::default());
    assert_eq!(groups.len(), 2);
    for (i, g) in groups.iter_mut().enumerate() {
        g.id = 10 + i as u64;
    }
    let original = groups.clone();
    assert_eq!(merge(&mut groups, &[10, 11]), Some(10));
    assert_eq!(groups.len(), 1);
    assert!(groups[0].manual);
    assert_eq!(groups[0].takes.len(), 4);
    assert!(groups[0].takes.windows(2).all(|w| w[0].range.start <= w[1].range.start));
    assert!(split(&mut groups, 10, 2, 11));
    assert_eq!(groups.len(), 2);
    assert!(groups.iter().all(|g| g.manual));
    for (a, b) in groups.iter().zip(original.iter()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.takes, b.takes);
    }
}

#[test]
fn merge_and_split_reject_bad_input() {
    let mut groups =
        vec![TakeGroup { id: 1, takes: vec![Take::new(TimeRange::new(s(0.0), s(1.0))), Take::new(TimeRange::new(s(2.0), s(1.0)))], ..Default::default() }];
    let before = groups.clone();
    assert_eq!(merge(&mut groups, &[]), None);
    assert_eq!(merge(&mut groups, &[1, 1]), None);
    assert_eq!(merge(&mut groups, &[1, 99]), None);
    assert!(!split(&mut groups, 99, 1, 2));
    assert!(!split(&mut groups, 1, 0, 2));
    assert!(!split(&mut groups, 1, 2, 2));
    assert!(!split(&mut groups, 1, usize::MAX, 2));
    assert!(!split(&mut groups, 1, 1, 1));
    assert_eq!(groups, before);
}

#[test]
fn live_fraction_union() {
    let take = TimeRange::new(s(10.0), s(10.0));
    assert_eq!(live_fraction(&[], &take), 0.0);
    assert_eq!(live_fraction(&[TimeRange::new(s(0.0), s(100.0))], &take), 1.0);
    // Overlapping parts count once.
    let f = live_fraction(&[TimeRange::new(s(10.0), s(4.0)), TimeRange::new(s(12.0), s(3.0)), TimeRange::new(s(30.0), s(5.0))], &take);
    assert!((f - 0.5).abs() < 1e-6, "{f}");
    assert_eq!(live_fraction(&[TimeRange::new(s(0.0), s(100.0))], &TimeRange::new(s(5.0), Tick::ZERO)), 0.0);
    assert_eq!(live_fraction(&[TimeRange::new(Tick::MIN, Tick::MAX)], &TimeRange::new(Tick::MAX, Tick::MAX)), 0.0);
}

#[test]
fn hostile_params_do_not_panic() {
    let t = transcript(&[(0.0, "today we talk about rivers."), (3.0, "today we talk about rivers."), (6.0, "okay again one more")]);
    let hostile = [
        DetectParams { sensitivity: f32::NAN, ..Default::default() },
        DetectParams { sensitivity: f32::INFINITY, ..Default::default() },
        DetectParams { sensitivity: -5.0, ..Default::default() },
        DetectParams { window: 0, ..Default::default() },
        DetectParams { window: usize::MAX, ..Default::default() },
        DetectParams { pause: Tick::ZERO, ..Default::default() },
        DetectParams { pause: Tick(i64::MIN), max_gap: Tick(i64::MIN), ..Default::default() },
        DetectParams { max_gap: Tick(i64::MAX), pause: Tick(i64::MAX), ..Default::default() },
        DetectParams { min_words: 0, ..Default::default() },
        DetectParams { min_words: usize::MAX, ..Default::default() },
    ];
    for p in hostile {
        let _ = detect(&t, &p);
    }
    assert_eq!(detect(&t, &DetectParams { sensitivity: f32::NAN, ..Default::default() }), detect(&t, &DetectParams::default()));
    assert!(detect(&t, &DetectParams { window: 0, ..Default::default() }).is_empty());
    // Hostile word times.
    let mut w = t.clone();
    w.words.push(Word::new("x", Tick(i64::MIN), Tick(i64::MAX)));
    w.words.push(Word::new("x", Tick(i64::MAX), Tick(i64::MIN)));
    let _ = detect(&w, &DetectParams::default());
}

/// The owner's line, said without a pause: one utterance.
const OWNER: &str = "what a what a time to be alive what a time to be a builder";

#[test]
fn false_start_without_pause_grouped() {
    let t = transcript(&[(0.0, OWNER)]);
    assert_eq!(utterances(&t.words, DetectParams::default().pause).len(), 1, "no pause in the line");
    let g = detect(&t, &DetectParams::default());
    assert_eq!(g.len(), 1, "{g:?}");
    // The false start "what a" first, then the full line (the anaphora "what a time to be alive,
    // what a time to be a builder" is not a restart: its first half is not an opening of the rest).
    assert_eq!(starts(&g[0]), vec![s(0.0), s(0.7)]);
    assert_eq!(g[0].takes[0].range, TimeRange::from_bounds(s(0.0), s(0.35 + 0.3)));
    assert_eq!(g[0].takes[1].range, TimeRange::from_bounds(s(0.7), s(14.0 * 0.35 + 0.3)));
}

#[test]
fn triple_false_start_without_pause() {
    let t = transcript(&[(0.0, "what a what a what a time to go")]);
    let g = detect(&t, &DetectParams::default());
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!(starts(&g[0]), vec![s(0.0), s(0.7), s(1.4)]);
    assert_eq!(restarts(&words("what a what a what a time to go"), 2), vec![2, 4]);
}

#[test]
fn restart_after_cue_words() {
    // Cue words are stripped before looking for the restart; the first part keeps them.
    let t = transcript(&[(0.0, "okay so the plan so the plan is simple")]);
    let g = detect(&t, &DetectParams::default());
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!(starts(&g[0]), vec![s(0.0), s(4.0 * 0.35)]);
}

#[test]
fn stutter_and_middle_repeat_not_split() {
    for line in ["the the river is wide", "we saw the river and then the river froze", "what a time to be alive what a time to be a builder"] {
        let t = transcript(&[(0.0, line)]);
        assert!(detect(&t, &DetectParams::default()).is_empty(), "{line}");
        assert!(restarts(&words(line), 2).is_empty(), "{line}");
    }
    // Even with min_words 1 a one-word stutter is not a restart.
    assert!(restarts(&words("the the river"), 1).is_empty());
    assert!(detect(&transcript(&[(0.0, "the the river")]), &DetectParams { min_words: 1, ..Default::default() }).is_empty());
}

#[test]
fn restart_hostile_params() {
    let t = transcript(&[(0.0, OWNER)]);
    // min_words 0 reads as 2 for restarts.
    assert_eq!(detect(&t, &DetectParams { min_words: 0, ..Default::default() }), detect(&t, &DetectParams::default()));
    assert!(detect(&t, &DetectParams { min_words: usize::MAX, ..Default::default() }).is_empty());
    assert!(detect(&t, &DetectParams { min_words: 3, ..Default::default() }).is_empty(), "\"what a\" is shorter than 3 words");
    assert!(detect(&Transcript::default(), &DetectParams { min_words: 0, ..Default::default() }).is_empty());
    assert!(restarts(&[], 0).is_empty());
    assert!(restarts(&words("a"), usize::MAX).is_empty());
    // A long utterance of one repeated pair splits into pairs, bounded.
    let long: Vec<String> = (0..1000).map(|i| if i % 2 == 0 { "what".to_string() } else { "a".to_string() }).collect();
    assert_eq!(restarts(&long, 2).len(), 499);
    // Words that all touch (no gap) or overlap in time still group.
    let mut o = t.clone();
    for w in &mut o.words {
        w.end = Tick(w.end.0.saturating_add(s(0.2).0));
    }
    assert_eq!(detect(&o, &DetectParams::default()).len(), 1);
}
