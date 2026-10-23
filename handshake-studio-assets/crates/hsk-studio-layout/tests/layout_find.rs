//! Lane SMB-LAYOUT-UI-2 slice 4: find/change engine (STU-LAY-129 text + GREP modes).
//! Hand-computed byte offsets. InDesign equality UNVERIFIED (D5).
mod support_layout;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

fn hits(st: &StudioTextStory, pattern: &str, mode: FindMode) -> Vec<(u64, u64)> {
    find_in_story(st, None, pattern, mode, None, None).unwrap().into_iter().map(|h| (h.start, h.end)).collect()
}

#[test]
fn find_modes_tokenize_literals_and_expand_captures_within_bounds() {
    let t = "Price: $5.00 (tax) price  PRICE\u{e9}t A.B a.b";
    assert_eq!(t.len(), 42);
    let (st, _) = story(1, t, &[(0, 42)]);
    let text = FindMode::Text { case_sensitive: false, whole_word: false };
    // Escape: metacharacters in text mode are literal ($, ., parentheses).
    assert_eq!(hits(&st, "$5.00", text), vec![(7, 12)]);
    assert_eq!(hits(&st, "(tax)", text), vec![(13, 18)]);
    assert_eq!(hits(&st, "a.b", text), vec![(35, 38), (39, 42)]);
    // Case and whitespace: case-insensitive by default; the double space is two literal spaces.
    assert_eq!(hits(&st, "price", text).len(), 3);
    assert_eq!(hits(&st, "price", FindMode::Text { case_sensitive: true, whole_word: false }), vec![(19, 24)]);
    assert_eq!(hits(&st, "  ", text), vec![(24, 26)]);
    // Whole word uses Unicode word boundaries: "PRICEét" is one word, so it is not a whole-word hit.
    assert_eq!(hits(&st, "price", FindMode::Text { case_sensitive: false, whole_word: true }), vec![(0, 5), (19, 24)]);
    // GREP: captures expand in the replacement ($$ = literal $); text mode never expands.
    let grep = FindMode::Grep { case_sensitive: true };
    let h = find_in_story(&st, None, r"\$(\d+)\.(\d+)", grep, None, Some("$2,$1 $$")).unwrap();
    assert_eq!((h[0].start, h[0].end, h[0].replacement.as_deref()), (7, 12, Some("00,5 $")));
    let h = find_in_story(&st, None, "tax", text, None, Some("$1")).unwrap();
    assert_eq!(h[0].replacement.as_deref(), Some("$1"));
    // Range scope: only hits inside [13, 26); word boundaries still see the text before the range.
    assert_eq!(find_in_story(&st, None, "price", text, Some((13, 26)), None).unwrap().len(), 1);
    assert_eq!(find_in_story(&st, None, "x", text, Some((5, 3)), None).unwrap_err().code(), "invalid_input");
    assert_eq!(find_in_story(&st, None, "x", text, Some((0, 43)), None).unwrap_err().code(), "invalid_input");
    // Hostile patterns: look-around and back-references refused; empty and oversized refused;
    // empty matches never loop or count.
    for p in [r"(?=a)", r"(a)\1", r"(?<!a)b"] {
        assert_eq!(find_in_story(&st, None, p, grep, None, None).unwrap_err().code(), "grep_pattern_unsupported", "{p}");
    }
    assert_eq!(find_in_story(&st, None, "", text, None, None).unwrap_err().code(), "grep_pattern_invalid");
    assert_eq!(find_in_story(&st, None, &"a".repeat(MAX_FIND_PATTERN_BYTES + 1), text, None, None).unwrap_err().code(), "grep_pattern_invalid");
    assert_eq!(find_in_story(&st, None, "(?:a{1000}){1000}", grep, None, None).unwrap_err().code(), "grep_pattern_unsupported");
    assert!(find_in_story(&st, None, "q*", grep, None, None).unwrap().is_empty());
    // Budget: more than MAX_GREP_MATCHES hits is a typed refusal, not a truncated list.
    let many = "a".repeat(MAX_GREP_MATCHES + 1);
    let (big, _) = story(2, &many, &[(0, many.len() as u64)]);
    assert_eq!(find_in_story(&big, None, "a", text, None, None).unwrap_err().code(), "grep_budget_exceeded");
}
