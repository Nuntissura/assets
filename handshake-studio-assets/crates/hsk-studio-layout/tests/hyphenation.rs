//! Liang hyphenation oracles. Expected breaks are the TeX en-US results (default InDesign-like
//! limits 5/2/2 unless a test says otherwise); equality with InDesign output is UNVERIFIED.
use hsk_studio_layout::{HyphenationParams, Hyphenator, MAX_WORD_BYTES, hyphenated};

fn h(word: &str) -> String {
    hyphenated(Hyphenator::en_us(), word, &HyphenationParams::default())
}

#[test]
fn word_list_matches_tex_en_us() {
    for (word, want) in [
        ("hyphenation", "hy-phen-ation"),
        ("algorithm", "al-go-rithm"),
        ("computer", "com-put-er"),
        ("typesetting", "type-set-ting"),
        ("table", "ta-ble"),
        ("paragraph", "para-graph"),
        ("international", "in-ter-na-tion-al"),
        ("justification", "jus-ti-fi-ca-tion"),
        ("Hyphenation", "Hy-phen-ation"),
        ("PROGRAMMING", "PRO-GRAM-MING"),
    ] {
        assert_eq!(h(word), want, "{word}");
    }
}

#[test]
fn limits_are_honoured() {
    let set = Hyphenator::en_us();
    let p = HyphenationParams::default();
    // Shorter than the minimum word length: no break.
    assert!(set.points("table", &HyphenationParams { min_word_len: 6, ..p }).is_empty());
    // min_before 3 / min_after 4 on hy-phen-ation (letters 2 and 6): 2 < 3 dropped; 6 + 4 <= 11 kept.
    let q = HyphenationParams { min_before: 3, min_after: 4, ..p };
    assert_eq!(set.points("hyphenation", &q), vec![6]);
    // Capitalised words can be excluded.
    let q = HyphenationParams { hyphenate_capitalized: false, ..p };
    assert!(set.points("Hyphenation", &q).is_empty());
    assert!(!set.points("hyphenation", &q).is_empty());
}

#[test]
fn text_scan_reports_absolute_bytes_and_skips_non_ascii_words() {
    let set = Hyphenator::en_us();
    let text = "a hyphenation naïve x2y-table";
    let found = set.hyphenate_text(text, &HyphenationParams::default());
    let starts: Vec<usize> = found.iter().map(|(s, _)| *s).collect();
    // "hyphenation" at 2; "naïve" skipped (non-ASCII); "x2y" skipped (digit); "table" after '-'.
    assert_eq!(starts, vec![2, text.rfind("table").unwrap()]);
    assert_eq!(found[0].1, vec![4, 8]);
    assert_eq!(found[1].1, vec![text.rfind("table").unwrap() + 2]);
}

#[test]
fn hostile_inputs_are_refused_not_guessed() {
    let set = Hyphenator::en_us();
    let p = HyphenationParams::default();
    assert!(set.points("", &p).is_empty());
    assert!(set.points("a", &p).is_empty());
    assert!(set.points(&"hyphenation".repeat(20), &p).is_empty());
    assert!(set.points("hyphén-ation", &p).is_empty());
    assert!(set.points("hyphenation\0", &p).is_empty());
    assert!(set.hyphenate_text("", &p).is_empty());
    // Zero or enormous limits cannot underflow or panic.
    let z = HyphenationParams { min_before: 0, min_after: 0, min_word_len: 0, ..p };
    let _ = set.points("table", &z);
    let big = HyphenationParams { min_before: usize::MAX / 2, min_after: usize::MAX / 2, ..p };
    assert!(set.points("hyphenation", &big).is_empty());
    for bad in ["[patterns]\nA1b", "[patterns]\n.\u{e9}", "x", "[exceptions]\nab1", "[patterns]\n1"] {
        assert!(Hyphenator::from_pattern_text(bad).is_err(), "{bad:?}");
    }
    let long = format!("[patterns]\n{}", "a".repeat(40));
    assert!(Hyphenator::from_pattern_text(&long).is_err());
    assert!(Hyphenator::from_pattern_text("# only comments\n").is_ok());
}

#[test]
fn custom_pattern_set_liang_semantics() {
    // Liang: higher digit wins; odd allows, even forbids. ".ab1c" allows a break after "ab" of "abcd";
    // "b2c" overrides it with an even value.
    let one = Hyphenator::from_pattern_text("[patterns]\n.ab1c\n").unwrap();
    let p = HyphenationParams { min_word_len: 1, min_before: 1, min_after: 1, ..Default::default() };
    assert_eq!(one.points("abcd", &p), vec![2]);
    let two = Hyphenator::from_pattern_text("[patterns]\n.ab1c\nb2c\n").unwrap();
    assert!(two.points("abcd", &p).is_empty());
    let exc = Hyphenator::from_pattern_text("[exceptions]\nab-cd\n").unwrap();
    assert_eq!(exc.points("abcd", &p), vec![2]);
}

#[test]
fn licence_is_recorded() {
    assert!(hsk_studio_layout::EN_US_LICENCE.contains("Kuiken"));
}

/// R28 finding: `min_before + min_after` was an unchecked add (debug overflow panic at
/// hyphenate.rs:159:39 observed on the unfixed code with (1, usize::MAX)). Every integer option at
/// its type's maximum is refused with a typed error, never a panic.
#[test]
fn hostile_integer_limits_at_type_max_are_refused() {
    use hsk_studio_layout::HyphenationError;
    let set = Hyphenator::en_us();
    let d = HyphenationParams::default();
    let hostile = [
        ("min_word_len", HyphenationParams { min_word_len: usize::MAX, ..d }),
        ("min_before", HyphenationParams { min_before: usize::MAX, ..d }),
        ("min_after", HyphenationParams { min_after: usize::MAX, ..d }),
        ("min_before=1+min_after=MAX", HyphenationParams { min_before: 1, min_after: usize::MAX, ..d }),
        ("min_before=MAX+min_after=1", HyphenationParams { min_before: usize::MAX, min_after: 1, ..d }),
        ("both MAX", HyphenationParams { min_before: usize::MAX, min_after: usize::MAX, ..d }),
        // Each side in range, sum one over the bound.
        ("sum over bound", HyphenationParams { min_before: 32, min_after: 33, ..d }),
        ("min_word_len over bound", HyphenationParams { min_word_len: MAX_WORD_BYTES + 1, ..d }),
    ];
    for (name, p) in hostile {
        assert_eq!(p.validate(), Err(HyphenationError::InvalidParams), "{name}");
        assert_eq!(set.try_points("hyphenation", &p), Err(HyphenationError::InvalidParams), "{name}");
        assert!(set.points("hyphenation", &p).is_empty(), "{name}");
        assert!(set.hyphenate_text("a hyphenation b", &p).is_empty(), "{name}");
    }
    assert_eq!(HyphenationError::InvalidParams.to_string(), "invalid_hyphenation_params");
    // Bounds are inclusive: sum exactly MAX_WORD_BYTES is accepted; only k = 32 can qualify.
    let edge = HyphenationParams { min_before: 32, min_after: 32, min_word_len: MAX_WORD_BYTES, ..d };
    assert_eq!(edge.validate(), Ok(()));
    let pts = set.try_points(&"ab".repeat(32), &edge).unwrap();
    assert!(pts.iter().all(|k| *k == 32), "{pts:?}");
    // consecutive_limit accepts its whole u8 range (0 = unlimited; no word has 255 breaks).
    let top = HyphenationParams { consecutive_limit: u8::MAX, ..d };
    assert_eq!(top.validate(), Ok(()));
    assert_eq!(set.try_points("hyphenation", &top), Ok(vec![2, 6]));
}
