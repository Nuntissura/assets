//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1: script itemization (STU-TYP-127 script run resolution). A
//! host sends each run with its own script instead of declaring `Latn` for everything (ledger
//! 2026-11-07T16:35:52Z "everything shapes as Latin, English").
use hsk_studio_type::{Error, ScriptRun, SourceRange, script_runs};

fn runs(text: &str) -> Vec<(u64, u64, String)> {
    script_runs(text, usize::MAX).unwrap().into_iter().map(|ScriptRun { range, script }| (range.start, range.end, String::from_utf8(script.to_vec()).unwrap())).collect()
}

#[test]
fn one_script_is_one_run_and_common_characters_join_it() {
    assert_eq!(runs("Chapter 12, part 3."), [(0, 19, "Latn".into())]);
    // Leading digits and punctuation join the first strong run.
    assert_eq!(runs("12 - Intro"), [(0, 10, "Latn".into())]);
    // A combining mark (Inherited) stays with its base.
    assert_eq!(runs("cafe\u{301}"), [(0, 6, "Latn".into())]);
    assert!(runs("").is_empty());
    assert_eq!(runs("2026 - 42"), [(0, 9, "Zyyy".into())], "no strong character");
}

#[test]
fn scripts_split_at_their_first_strong_character() {
    // "Hello " (6) + three Arabic letters (6 bytes) + " " + three Han (9 bytes).
    let text = "Hello \u{0645}\u{0631}\u{062D} \u{6F22}\u{5B57}\u{6587}";
    assert_eq!(runs(text), [(0, 6, "Latn".into()), (6, 13, "Arab".into()), (13, 22, "Hani".into())]);
    // Greek and Cyrillic get their own tags; the shaping engine decides whether it supports them.
    assert_eq!(runs("abc \u{3B1}\u{3B2} \u{0434}"), [(0, 4, "Latn".into()), (4, 9, "Grek".into()), (9, 11, "Cyrl".into())]);
}

#[test]
fn runs_tile_the_text_and_the_count_is_bounded() {
    let text = "a\u{3B1}".repeat(50);
    let all = script_runs(&text, usize::MAX).unwrap();
    assert_eq!(all.len(), 100);
    assert_eq!(all.first().map(|r| r.range), Some(SourceRange { start: 0, end: 1 }));
    assert!(all.windows(2).all(|w| w[0].range.end == w[1].range.start));
    assert_eq!(all.last().map(|r| r.range.end), Some(text.len() as u64));
    assert_eq!(script_runs(&text, 99), Err(Error::Budget));
    assert_eq!(script_runs("abc", 0), Err(Error::Budget));
    assert_eq!(script_runs("abc", 1).map(|r| r.len()), Ok(1));
}
