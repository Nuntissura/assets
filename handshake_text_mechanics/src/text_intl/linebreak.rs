//! UAX#14 line-break opportunities + CJK kinsoku, SUPPLEMENTING egui's native wrapping (MT-077).
//!
//! ## What egui already does (verified — do NOT re-implement it)
//!
//! egui's `epaint::Galley` does its OWN line-wrapping and ALREADY handles CJK. Verified by reading the
//! locked source `epaint-0.33.3/src/text/text_layout.rs` (`RowBreakCandidates::add`) +
//! `epaint-0.33.3/src/text/font.rs`:
//! - `is_cjk(c)` = `is_cjk_ideograph(U+4E00..=U+9FFF, U+3400..=U+4DBF, ext-B) || is_kana` — so Han +
//!   Hiragana + Katakana are break candidates: a long Chinese/Japanese ideograph run WRAPS within the
//!   wrap width with the MT-075 bundled NotoSansSC font (no spaces needed). This is the AC1 base case.
//! - `is_cjk_break_allowed(next)` encodes KINSOKU: it FORBIDS a break before a closing bracket / closing
//!   punctuation (`）」』】、。` …) — exactly the "never break BEFORE a closing bracket/period" rule
//!   AC2 names. So the common kinsoku case is already correct in egui.
//!
//! ## What egui MISSES (the gap this module's table closes)
//!
//! 1. **Korean Hangul.** `epaint/src/text/font.rs` has an explicit `TODO(bigfarts): Add support for
//!    Korean Hangul` — `is_cjk('가')` is FALSE, so a long Hangul run does NOT wrap in egui. UAX#14
//!    classes Hangul syllables as `H2`/`H3`/`JL`/`JV`/`JT` with break opportunities between syllables;
//!    `unicode-linebreak` encodes this. A renderer that wants Hangul to wrap consults
//!    [`break_opportunities`] / [`is_break_before`].
//! 2. **No break AFTER an opening bracket.** egui forbids a break *before* a closing bracket but does
//!    not encode "no break *after* an opening bracket" (`（「『【` …). UAX#14 class `OP` (open
//!    punctuation) carries this rule (`OP ×` — never break after OP); [`is_break_before`] returns
//!    `false` immediately after an `OP` so a renderer can honor it.
//!
//! ## How a renderer uses this
//!
//! For text egui wraps acceptably (Han/Kana), the renderer does nothing — egui's Galley wraps it. For
//! text where egui falls short (Hangul, opening-bracket kinsoku), the renderer can consult
//! [`break_opportunities`] to decide where a soft break is permitted and lay out runs accordingly (e.g.
//! by splitting a paragraph into per-opportunity runs, or by inserting a zero-width break hint). This
//! module is the PURE UAX#14 authority; the wiring is the renderer's (kept minimal because egui covers
//! the common path). No egui, no `Color32` here.

use unicode_linebreak::{linebreaks, BreakOpportunity as UlBreak};

/// Whether a line break at a given position is forced or merely permitted (UAX#14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakOpportunity {
    /// A line MUST break here (after a hard newline / paragraph separator).
    Mandatory,
    /// A line MAY break here (a soft wrap opportunity).
    Allowed,
}

impl From<UlBreak> for BreakOpportunity {
    fn from(b: UlBreak) -> Self {
        match b {
            UlBreak::Mandatory => BreakOpportunity::Mandatory,
            UlBreak::Allowed => BreakOpportunity::Allowed,
        }
    }
}

/// A coarse line-break CLASS for a character, used to explain WHY a break is/var is not allowed at a
/// boundary (so a renderer and the tests can reason about kinsoku without re-deriving UAX#14 classes).
/// This is intentionally a small, decision-relevant subset, not the full UAX#14 class set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakClass {
    /// CJK ideograph or kana (a break is generally allowed before/after — egui handles this).
    Ideographic,
    /// Korean Hangul syllable / jamo (a break is allowed between syllables — egui MISSES this).
    Hangul,
    /// Opening punctuation `OP` — never break AFTER it (egui MISSES this).
    OpenPunctuation,
    /// Closing punctuation / closing bracket `CL`/`CP` — never break BEFORE it (egui handles this).
    ClosePunctuation,
    /// Whitespace (the universal break opportunity).
    Space,
    /// Anything else (Latin letters, digits, …).
    Other,
}

/// All UAX#14 break opportunities in `text`, as `(byte_index, opportunity)` pairs where `byte_index` is
/// the byte offset of the char AFTER which/BEFORE which a break may occur (the crate's convention:
/// `byte_index` is where the following grapheme begins, so a break is permitted immediately BEFORE
/// `byte_index`). This is a thin, typed re-export of `unicode_linebreak::linebreaks` so the rest of the
/// crate never imports the external crate directly (single integration point).
pub fn break_opportunities(text: &str) -> Vec<(usize, BreakOpportunity)> {
    linebreaks(text).map(|(i, b)| (i, b.into())).collect()
}

/// Whether a soft line break is allowed immediately BEFORE byte offset `byte_index` in `text` per
/// UAX#14. This is the question a CJK/Hangul-aware renderer asks to decide whether a wrap may happen at
/// a given inter-character position. `0` and `text.len()` are never interior break points, so they
/// return `false` (a line does not "break before" its own start, and end-of-text is a mandatory edge,
/// not a soft opportunity). A `Mandatory` opportunity (hard newline) is NOT a soft-wrap point, so it
/// also returns `false` here — use [`break_opportunities`] to see mandatory breaks.
pub fn is_break_before(text: &str, byte_index: usize) -> bool {
    if byte_index == 0 || byte_index >= text.len() {
        return false;
    }
    break_opportunities(text)
        .into_iter()
        .any(|(i, op)| i == byte_index && op == BreakOpportunity::Allowed)
}

/// Classify a single `char` into the decision-relevant [`BreakClass`]. Used by the kinsoku tests and by
/// a renderer that wants a quick "is this an opening bracket / Hangul" answer without the full
/// opportunity scan. The ranges follow UAX#14 / Unicode block assignments for the subset that matters.
pub fn break_class(c: char) -> BreakClass {
    // Whitespace first (UAX#14 SP/BA-ish): the universal break.
    if c.is_whitespace() {
        return BreakClass::Space;
    }
    // Korean Hangul: precomposed syllables (Hangul Syllables block) + conjoining jamo + compat jamo.
    if ('\u{AC00}'..='\u{D7A3}').contains(&c) // Hangul Syllables
        || ('\u{1100}'..='\u{11FF}').contains(&c) // Hangul Jamo
        || ('\u{3130}'..='\u{318F}').contains(&c) // Hangul Compatibility Jamo
        || ('\u{A960}'..='\u{A97F}').contains(&c) // Hangul Jamo Extended-A
        || ('\u{D7B0}'..='\u{D7FF}').contains(&c)
    // Hangul Jamo Extended-B
    {
        return BreakClass::Hangul;
    }
    // CJK ideographs + kana (the set egui's `is_cjk` covers).
    if ('\u{4E00}'..='\u{9FFF}').contains(&c)
        || ('\u{3400}'..='\u{4DBF}').contains(&c)
        || ('\u{3040}'..='\u{309F}').contains(&c) // Hiragana
        || ('\u{30A0}'..='\u{30FF}').contains(&c)
    // Katakana
    {
        return BreakClass::Ideographic;
    }
    // Opening / closing CJK + ASCII brackets (the kinsoku-relevant punctuation).
    if is_open_punctuation(c) {
        return BreakClass::OpenPunctuation;
    }
    if is_close_punctuation(c) {
        return BreakClass::ClosePunctuation;
    }
    BreakClass::Other
}

/// UAX#14 `OP`-ish opening punctuation: never break AFTER one. Covers the common CJK + ASCII openers.
fn is_open_punctuation(c: char) -> bool {
    matches!(
        c,
        '(' | '[' | '{'
            | '\u{FF08}' // （ fullwidth left paren
            | '\u{FF3B}' // ［ fullwidth left bracket
            | '\u{FF5B}' // ｛ fullwidth left brace
            | '\u{300C}' // 「 left corner bracket
            | '\u{300E}' // 『 left white corner bracket
            | '\u{3010}' // 【 left black lenticular bracket
            | '\u{3008}' // 〈 left angle bracket
            | '\u{300A}' // 《 left double angle bracket
            | '\u{FF62}' // ｢ halfwidth left corner bracket
    )
}

/// UAX#14 `CL`/`CP`-ish closing punctuation: never break BEFORE one (egui handles this; mirrored here
/// for the kinsoku classification + tests).
fn is_close_punctuation(c: char) -> bool {
    matches!(
        c,
        ')' | ']' | '}'
            | '\u{FF09}' // ）
            | '\u{FF3D}' // ］
            | '\u{FF5D}' // ｝
            | '\u{300D}' // 」
            | '\u{300F}' // 』
            | '\u{3011}' // 】
            | '\u{3009}' // 〉
            | '\u{300B}' // 》
            | '\u{FF63}' // ｣
            | '\u{3001}' // 、 ideographic comma
            | '\u{3002}' // 。 ideographic full stop
            | '\u{FF0C}' // ，fullwidth comma
            | '\u{FF0E}' // ．fullwidth full stop
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // A long Han run (no spaces). UAX#14 permits a break between (almost) every pair of ideographs.
    const HAN: &str = "今天我写了很多中文字符没有空格";
    // A Korean Hangul run (the gap egui misses).
    const HANGUL: &str = "오늘나는한국어를많이썼다";

    #[test]
    fn han_run_has_many_break_opportunities() {
        // AC1 (UAX#14 side): a long spaceless Han run must offer many interior break points so it can
        // wrap, not collapse to one line.
        let ops = break_opportunities(HAN);
        let interior = ops
            .iter()
            .filter(|(i, op)| *i < HAN.len() && *op == BreakOpportunity::Allowed)
            .count();
        assert!(
            interior >= HAN.chars().count() - 2,
            "a spaceless Han run must break between (almost) every ideograph; got {interior} interior breaks for {} chars",
            HAN.chars().count()
        );
    }

    #[test]
    fn hangul_run_has_break_opportunities_egui_misses() {
        // The gap MT-077's table closes: Hangul syllables offer interior break opportunities (egui's
        // is_cjk does NOT cover Hangul, so without this table a long Hangul run would not wrap).
        let ops = break_opportunities(HANGUL);
        let interior = ops
            .iter()
            .filter(|(i, op)| *i < HANGUL.len() && *op == BreakOpportunity::Allowed)
            .count();
        assert!(
            interior >= 1,
            "Hangul must offer interior break opportunities; got {interior}"
        );
        // And every Hangul char classifies as Hangul (so a renderer can detect the egui gap).
        for c in HANGUL.chars() {
            assert_eq!(break_class(c), BreakClass::Hangul, "char {c:?} is Hangul");
        }
    }

    #[test]
    fn kinsoku_no_break_before_closing_bracket() {
        // AC2: a line must NOT break BEFORE a closing bracket/period. "字）" — no soft break may sit
        // between the ideograph and the closing paren (the break would orphan the ） at a line start).
        let s = "字\u{FF09}字"; // 字 ） 字
        let close_idx = "字".len(); // byte offset of the ）
        assert!(
            !is_break_before(s, close_idx),
            "UAX#14 kinsoku: no break may occur immediately before the closing ） (would start a line with it)"
        );
    }

    #[test]
    fn kinsoku_no_break_after_opening_bracket() {
        // AC2 (the gap egui misses): a line must NOT break AFTER an opening bracket. "（字" — no soft
        // break between （ and the following ideograph (the break would orphan the （ at a line end).
        let s = "字\u{FF08}字"; // 字 （ 字
        let after_open_idx = "字".len() + "\u{FF08}".len(); // byte offset just AFTER the （
        assert!(
            !is_break_before(s, after_open_idx),
            "UAX#14: no break may occur immediately after the opening （ (would end a line with it)"
        );
    }

    #[test]
    fn classes_are_correct() {
        assert_eq!(break_class('字'), BreakClass::Ideographic);
        assert_eq!(break_class('한'), BreakClass::Hangul);
        assert_eq!(break_class('\u{FF08}'), BreakClass::OpenPunctuation); // （
        assert_eq!(break_class('\u{FF09}'), BreakClass::ClosePunctuation); // ）
        assert_eq!(break_class('('), BreakClass::OpenPunctuation);
        assert_eq!(break_class(')'), BreakClass::ClosePunctuation);
        assert_eq!(break_class(' '), BreakClass::Space);
        assert_eq!(break_class('a'), BreakClass::Other);
        assert_eq!(break_class('あ'), BreakClass::Ideographic); // kana is in egui's is_cjk set
    }

    #[test]
    fn ascii_text_breaks_only_at_spaces_no_regression() {
        // AC7 no-regression: plain ASCII prose offers break opportunities at spaces (and end), NOT
        // between every letter — LTR wrapping behavior is unchanged.
        let s = "hello world foo";
        let ops = break_opportunities(s);
        // Breaks after "hello " (idx 6) and "world " (idx 12), plus the mandatory end.
        assert!(ops
            .iter()
            .any(|(i, op)| *i == 6 && *op == BreakOpportunity::Allowed));
        assert!(ops
            .iter()
            .any(|(i, op)| *i == 12 && *op == BreakOpportunity::Allowed));
        // There is NO interior break inside the word "hello" (no break before idx 1..5).
        for i in 1..5 {
            assert!(
                !is_break_before(s, i),
                "no break inside the ASCII word 'hello' at {i}"
            );
        }
    }

    #[test]
    fn empty_and_single_char_are_safe() {
        assert!(
            break_opportunities("").is_empty()
                || break_opportunities("")
                    .iter()
                    .all(|(_, op)| *op == BreakOpportunity::Mandatory)
        );
        assert!(!is_break_before("", 0));
        assert!(!is_break_before("a", 0));
        assert!(!is_break_before("a", 1)); // end of text is not a soft break
    }
}
