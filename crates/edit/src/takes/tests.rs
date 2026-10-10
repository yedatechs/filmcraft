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
    // The false start "what a", then "what a time to be alive" (the owner restarted it: "what a
    // time to be" said again two words on), then the full line.
    assert_eq!(starts(&g[0]), vec![s(0.0), s(0.7), s(8.0 * 0.35)]);
    assert_eq!(g[0].takes[0].range, TimeRange::from_bounds(s(0.0), s(0.35 + 0.3)));
    assert_eq!(g[0].takes[1].range, TimeRange::from_bounds(s(0.7), s(7.0 * 0.35 + 0.3)));
    assert_eq!(g[0].takes[2].range, TimeRange::from_bounds(s(8.0 * 0.35), s(14.0 * 0.35 + 0.3)));
}

#[test]
fn triple_false_start_without_pause() {
    let t = transcript(&[(0.0, "what a what a what a time to go")]);
    let g = detect(&t, &DetectParams::default());
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!(starts(&g[0]), vec![s(0.0), s(0.7), s(1.4)]);
    assert_eq!(restarts(&words("what a what a what a time to go"), 2), vec![(0, 2), (2, 4)]);
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
    // ("what a time to be alive what a time to be a builder" now splits: a five-word phrase said again.)
    for line in ["the the river is wide", "we saw the river and then the river froze"] {
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
    // "what a" is shorter than 3 words, so only the five-word restart counts (the lone "what a" is too
    // short to be a take).
    let g = detect(&t, &DetectParams { min_words: 3, ..Default::default() });
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!(starts(&g[0]), vec![s(0.7), s(8.0 * 0.35)]);
    assert!(detect(&Transcript::default(), &DetectParams { min_words: 0, ..Default::default() }).is_empty());
    assert!(restarts(&[], 0).is_empty());
    assert!(restarts(&words("a"), usize::MAX).is_empty());
    // A long utterance of one repeated pair splits into pairs, at most MAX_RESTARTS of them.
    let long: Vec<String> = (0..1000).map(|i| if i % 2 == 0 { "what".to_string() } else { "a".to_string() }).collect();
    assert_eq!(restarts(&long, 2).len(), MAX_RESTARTS);
    // Words that all touch (no gap) or overlap in time still group.
    let mut o = t.clone();
    for w in &mut o.words {
        w.end = Tick(w.end.0.saturating_add(s(0.2).0));
    }
    assert_eq!(detect(&o, &DetectParams::default()).len(), 1);
}

/// The owner's real take, first 130 whisper words: `(text, start ms, end ms)`. Generated from
/// `.frugal-fable/briefs/jered-opening-words.json`; six restarts, none separated by a pause.
const OPENING: [(&str, i64, i64); 130] = [
    ("yo", 5680, 6000),
    ("what", 6000, 6290),
    ("a", 6290, 6560),
    ("what", 6650, 6780),
    ("a", 6780, 7050),
    ("time", 7050, 7370),
    ("to", 7370, 7480),
    ("be", 7480, 7560),
    ("alive", 7560, 7990),
    ("what", 7990, 8180),
    ("a", 8180, 8400),
    ("time", 8400, 8640),
    ("to", 8640, 8740),
    ("be", 8740, 8840),
    ("a", 8840, 8940),
    ("builder", 8940, 9140),
    ("these", 9140, 9640),
    ("last", 9640, 10000),
    ("three", 10000, 10260),
    ("weeks", 10260, 10590),
    ("have", 10610, 10780),
    ("been", 10780, 11010),
    ("crazy", 11010, 11520),
    ("anthropic", 12180, 12850),
    ("just", 13070, 13410),
    ("reset", 13410, 13790),
    ("fable", 13790, 14030),
    ("usage", 14030, 14280),
    ("limits", 14280, 14830),
    ("that", 15300, 15600),
    ("so", 20720, 20860),
    ("now", 20860, 21060),
    ("we'll", 21060, 21280),
    ("get", 21280, 21510),
    ("so", 21520, 21940),
    ("so", 21940, 22070),
    ("now", 22070, 22280),
    ("we", 22280, 22440),
    ("have", 22440, 22600),
    ("three", 22600, 22780),
    ("days", 22780, 23160),
    ("so", 23340, 23460),
    ("now", 23460, 23600),
    ("you", 23600, 23680),
    ("have", 23680, 23830),
    ("three", 23830, 23980),
    ("more", 23980, 24190),
    ("days", 24190, 24420),
    ("to", 24420, 24480),
    ("use", 24480, 24620),
    ("fable", 24620, 25000),
    ("unless", 25000, 25380),
    ("they", 25380, 25580),
    ("extend", 25580, 26120),
    ("fable", 26290, 26480),
    ("usage", 26480, 26840),
    ("again", 26840, 27080),
    ("which", 27080, 27480),
    ("they", 27480, 27590),
    ("probably", 27600, 27870),
    ("will", 27870, 28160),
    ("open", 28720, 28890),
    ("ai", 28890, 29180),
    ("also", 29180, 29780),
    ("just", 29780, 30100),
    ("reset", 30100, 30480),
    ("users", 30480, 30800),
    ("open", 30800, 31060),
    ("ai", 31430, 31840),
    ("also", 31840, 32390),
    ("just", 32390, 32720),
    ("reset", 32720, 32960),
    ("usage", 32960, 33200),
    ("limits", 33200, 33610),
    ("for", 33620, 33760),
    ("like", 34740, 34950),
    ("the", 34950, 35190),
    ("third", 35200, 35420),
    ("time", 35420, 35600),
    ("this", 35600, 35800),
    ("week", 35800, 36100),
    ("and", 36330, 36460),
    ("they", 36460, 36600),
    ("don't", 36600, 36740),
    ("even", 36740, 36840),
    ("have", 36840, 37040),
    ("a", 37040, 37270),
    ("and", 37360, 37680),
    ("they", 37680, 37810),
    ("don't", 37810, 37960),
    ("even", 37960, 38140),
    ("have", 38140, 38380),
    ("a", 38380, 38530),
    ("five", 38540, 38760),
    ("hour", 38760, 39010),
    ("window", 39010, 39320),
    ("and", 40490, 40750),
    ("they", 40750, 40840),
    ("also", 40840, 41140),
    ("and", 41140, 41680),
    ("they", 41680, 41780),
    ("don't", 41780, 42040),
    ("and", 42110, 42300),
    ("they", 42300, 42460),
    ("remove", 42460, 42810),
    ("the", 42810, 42930),
    ("fire", 42930, 43270),
    ("and", 43550, 43820),
    ("they", 43820, 43900),
    ("also", 43900, 44120),
    ("remove", 44120, 44490),
    ("the", 44490, 44640),
    ("five", 44650, 44890),
    ("hour", 44890, 45200),
    ("and", 45350, 45570),
    ("they", 45570, 45640),
    ("also", 45640, 45860),
    ("remove", 45860, 46170),
    ("the", 46170, 46320),
    ("five", 46330, 46500),
    ("hour", 46500, 46750),
    ("limit", 46750, 47080),
    ("so", 47480, 47730),
    ("i've", 47730, 47800),
    ("been", 47800, 48010),
    ("so", 48080, 48200),
    ("i've", 48200, 48370),
    ("been", 48370, 48500),
    ("going", 48500, 48740),
    ("back", 48740, 48980),
];

fn opening() -> Transcript {
    let ms = |x: i64| Tick(x.saturating_mul(TICKS_PER_SECOND / 1000));
    Transcript { words: OPENING.iter().map(|&(w, a, b)| Word::new(w, ms(a), ms(b))).collect(), ..Default::default() }
}

/// The words of `t` that start inside the take, joined.
fn take_text(t: &Transcript, take: &Take) -> String {
    let end = take.range.end();
    t.words.iter().filter(|w| w.start >= take.range.start && w.start < end).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
}

#[test]
fn owner_opening_six_restarts() {
    let t = opening();
    let g = detect(&t, &DetectParams::default());
    let texts: Vec<Vec<String>> = g.iter().map(|g| g.takes.iter().map(|k| take_text(&t, k)).collect()).collect();
    let expected: [&[&str]; 6] = [
        &["yo what a", "what a time to be alive", "what a time to be a builder these last three weeks have been crazy"],
        &[
            "so now we'll get",
            "so so now we have three days",
            "so now you have three more days to use fable unless they extend fable usage again which they probably will",
        ],
        &["open ai also just reset users", "open ai also just reset usage limits for"],
        &["and they don't even have a", "and they don't even have a five hour window"],
        &["and they also", "and they don't", "and they remove the fire", "and they also remove the five hour", "and they also remove the five hour limit"],
        &["so i've been", "so i've been going back"],
    ];
    assert_eq!(texts, expected.iter().map(|g| g.iter().map(|s| s.to_string()).collect::<Vec<_>>()).collect::<Vec<_>>());
    for g in &g {
        // Takes in media order, not overlapping: the default `select: "last"` keeps the full line
        // live and crosses out every false start before it.
        assert!(g.takes.windows(2).all(|w| w[0].range.end() <= w[1].range.start), "{g:?}");
    }
    // Line 6 opens a new sentence after line 5's last take: separate groups, adjacent in time.
    assert!(g[4].takes.last().unwrap().range.end() <= g[5].takes[0].range.start);
    // min_words 0 reads as 2; a huge one finds nothing.
    assert_eq!(detect(&t, &DetectParams { min_words: 0, ..Default::default() }), g);
    assert!(detect(&t, &DetectParams { min_words: 1000, ..Default::default() }).is_empty());
}

#[test]
fn natural_repetition_is_not_a_restart() {
    // A repeat right after a joining word ("and", "then", …) continues the sentence; so does any
    // repeat a word further left already matched.
    for line in [
        "I went to the store and then I went to the bank",
        "the the",
        "we have three days to use it and three days to decide",
        "we saw the river and then the river froze",
        // two shared words seven words apart
        "the river runs past our old house near the river bank",
    ] {
        assert!(restarts(&words(line), 2).is_empty(), "{line}");
        assert!(detect(&transcript(&[(0.0, line)]), &DetectParams::default()).is_empty(), "{line}");
    }
    // Two shared words with nothing of the false start said again: natural repetition, even
    // without a joining word (the retake does not go on to repeat "use", "dopamine", "limits" …).
    for line in [
        "has just been a roller coaster for my dopamine and terrible for my sleep",
        "the prices are going to go up dramatically we're going to eventually lose fable",
        "like most of you guys complaining about usage limits complaining about prices but i think",
        // a list of numbers after the shared phrase
        "take advantage of the current twenty dollar hundred dollar two hundred dollar pricing models",
    ] {
        assert!(restarts(&words(line), 2).is_empty(), "{line}");
        assert!(detect(&transcript(&[(0.0, line)]), &DetectParams::default()).is_empty(), "{line}");
    }
    // … but a retake that says one of the false start's words again is a restart, and so is a
    // false start of at most three words; a changed contraction still counts ("we'll" / "we").
    assert_eq!(restarts(&words("and they remove the fire and they also remove the five hour"), 2), vec![(0, 5)]);
    // Three shared words count anywhere in the look-back (here without the joining word).
    assert_eq!(restarts(&words("we have three days to use it three days to decide"), 2), vec![(2, 7)]);
    assert_eq!(restarts(&words("so now we'll get so now we have three days"), 2), vec![(0, 4)]);
    assert_eq!(restarts(&words("and they also and they don't"), 2), vec![(0, 3)]);
    assert_eq!(restarts(&words("hundred dollar two hundred dollar pricing models"), 2), Vec::<(usize, usize)>::new());
    // A deliberate repeat (counting) splits: accepted, the user can merge or ignore the group.
    assert_eq!(restarts(&words("one two three one two three"), 2), vec![(0, 3)]);
}

#[test]
fn stutters_collapse_without_splitting() {
    assert_eq!(collapse_stutters(&words("so so now the the the end")), (words("so now the end"), vec![0, 2, 3, 6]));
    assert!(restarts(&words("so so so"), 2).is_empty());
    // A stutter inside a restart: the part boundary lands on the first word of the stutter.
    let t = transcript(&[(0.0, "so now we'll get so so now we have three days")]);
    let g = detect(&t, &DetectParams::default());
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!(starts(&g[0]), vec![s(0.0), s(4.0 * 0.35)]);
}

#[test]
fn restart_hostile_input_is_bounded() {
    assert!(detect(&Transcript { words: Vec::new(), ..Default::default() }, &DetectParams::default()).is_empty());
    assert!(restarts(&[], 2).is_empty());
    assert!(restarts(&words("a b"), 0).is_empty());
    // 5000 words without a pause, repeating every seven words: bounded work, at most MAX_RESTARTS cuts.
    let text: Vec<String> = (0..5000).map(|i| format!("w{}", i % 7)).collect();
    let t = transcript(&[(0.0, &text.join(" "))]);
    let began = std::time::Instant::now();
    let g = detect(&t, &DetectParams::default());
    assert!(began.elapsed() < std::time::Duration::from_secs(5), "{:?}", began.elapsed());
    assert_eq!(restarts(&words(&text.join(" ")), 2).len(), MAX_RESTARTS);
    assert_eq!(g.iter().map(|g| g.takes.len()).sum::<usize>(), MAX_RESTARTS + 1);
    for p in [DetectParams { min_words: 0, ..Default::default() }, DetectParams { min_words: 1000, ..Default::default() }] {
        let _ = detect(&t, &p);
    }
}
