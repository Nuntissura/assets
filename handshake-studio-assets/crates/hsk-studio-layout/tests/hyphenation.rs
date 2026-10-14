//! Liang hyphenation oracles. Expected breaks are the TeX en-US results (default InDesign-like
//! limits 5/2/2 unless a test says otherwise); equality with InDesign output is UNVERIFIED.
use hsk_studio_layout::{HyphenationParams, Hyphenator, hyphenated};

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
