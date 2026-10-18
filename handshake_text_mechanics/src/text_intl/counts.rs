//! Unicode-correct word + character counts for the editor footers (WP-KERNEL-012 MT-077).
//!
//! The MT-019 daily-notes footer counted words by splitting on whitespace and characters by Unicode
//! scalar (`char`). Both are wrong for international text:
//!
//! - **Word count.** A Chinese sentence has NO spaces (`今天我写了很多字`), so whitespace tokenization
//!   reports it as ONE "word" — meaningless. [`word_count`] uses UAX#29 word boundaries
//!   (`unicode-segmentation::unicode_words`), which yields word-like tokens: each CJK ideograph/run is
//!   counted as the standard treats it, Latin/Cyrillic/Greek words are whole words, and punctuation /
//!   whitespace are not counted as words.
//!
//!   DOCUMENTED RULE (AC5): a "word" is a UAX#29 word per `unicode_words()`. For a mixed string like
//!   `"Hello 世界 test"` this counts `Hello`, `世`, `界`, `test` = **4** — the two Latin words plus the
//!   two CJK ideographs (UAX#29 treats each Han ideograph as its own word, the standard behavior all
//!   CJK-aware editors use; there is no whitespace inside `世界` to merge them and Han has no
//!   word-internal joiners). This is the sensible, defined count CJK users expect (a 2-character Chinese
//!   word region contributes 2, matching per-character CJK counting in editors like VS Code / Word's
//!   "Chinese characters" count).
//!
//! - **Character count.** A family ZWJ emoji is 7 scalars but ONE user-perceived character. [`char_count`]
//!   counts GRAPHEME CLUSTERS (UAX#29 extended graphemes), so the family emoji = 1, `e`+combining-accent
//!   = 1, a flag = 1, a Hangul syllable = 1. DOCUMENTED CHOICE (AC6 / RISK-3): grapheme clusters, NOT
//!   scalars and NOT UTF-16 units. Rationale: grapheme = what a human counts as "one character", which
//!   is what a writer expects from a character count. (If a backend ever needs a scalar or UTF-16 count
//!   to match a stored value, that is a separate, explicitly-named count — this footer count is the
//!   human-facing grapheme count.)

use unicode_segmentation::UnicodeSegmentation;

/// The number of UAX#29 words in `text` (the documented footer "word count" — see module docs). Uses
/// `unicode_words()`, which excludes whitespace and most punctuation and treats CJK ideographs as the
/// standard does. Empty text counts 0.
///
/// AC5 / AC7: for pure ASCII this equals the old whitespace-token count for ordinary prose
/// (`"hello world"` -> 2), so the LTR footer is unchanged; for CJK it is meaningful where the old
/// whitespace split was not.
pub fn word_count(text: &str) -> usize {
    text.unicode_words().count()
}

/// The number of GRAPHEME CLUSTERS in `text` (the documented footer "character count" — see module
/// docs). The family emoji / combining sequence / flag / Hangul syllable each count as 1. Empty text
/// counts 0. `is_extended = true` selects extended grapheme clusters (the user-perceived-character
/// definition).
pub fn char_count(text: &str) -> usize {
    text.graphemes(true).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_word_count_matches_prose_tokens_no_regression() {
        // AC7: ordinary ASCII prose counts the same as the old whitespace split.
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("hello"), 1);
        assert_eq!(word_count("hello world"), 2);
        assert_eq!(word_count("  hello   world  foo "), 3);
        assert_eq!(word_count("one\ntwo\tthree"), 3);
    }

    #[test]
    fn cjk_word_count_is_meaningful_not_one() {
        // The core fix: a spaceless Chinese run is NOT one "word". UAX#29 counts each Han ideograph.
        // "今天我写了很多字" = 8 ideographs -> 8 words (the documented per-ideograph CJK rule).
        let s = "今天我写了很多字";
        assert_eq!(s.chars().count(), 8, "sanity: 8 ideographs");
        assert_eq!(
            word_count(s),
            8,
            "spaceless CJK counts per UAX#29 word (per-ideograph), not as 1 whitespace token"
        );
        // The old whitespace split would have returned 1 — prove we are NOT doing that.
        assert_ne!(
            word_count(s),
            1,
            "must not collapse a CJK sentence to a single word"
        );
    }

    #[test]
    fn mixed_latin_cjk_word_count_is_documented() {
        // AC5 mixed string: "Hello 世界 test" -> Hello + 世 + 界 + test = 4 (documented in module docs).
        assert_eq!(word_count("Hello 世界 test"), 4);
        // A pure-Latin pair still counts 2 (no double-count at the script boundary — RISK-5).
        assert_eq!(word_count("Hello test"), 2);
    }

    #[test]
    fn char_count_is_grapheme_clusters_not_scalars() {
        // AC6: the family emoji is ONE character, not 7 codepoints.
        let family = "👨‍👩‍👧";
        assert!(
            family.chars().count() >= 5,
            "sanity: the family emoji is many scalars"
        );
        assert_eq!(
            char_count(family),
            1,
            "the family emoji counts as ONE character (grapheme cluster)"
        );
        // Combining accent: "e" + U+0301 = 1 character.
        assert_eq!(char_count("e\u{0301}"), 1);
        // Flag: two regional indicators = 1 character.
        assert_eq!(char_count("🇯🇵"), 1);
        // Decomposed Hangul syllable (3 jamo) = 1 character.
        assert_eq!(char_count("\u{1112}\u{1161}\u{11AB}"), 1);
    }

    #[test]
    fn char_count_ascii_no_regression() {
        // AC7: ASCII char count equals the scalar count (each ASCII char is its own cluster).
        assert_eq!(char_count(""), 0);
        assert_eq!(char_count("hello"), 5);
        assert_eq!(char_count("héllo"), 5); // precomposed é is one scalar AND one cluster
    }

    #[test]
    fn char_count_cjk_counts_each_ideograph() {
        // A CJK string's character count is its ideograph count (each ideograph is one cluster).
        assert_eq!(char_count("日本語"), 3);
        assert_eq!(char_count("今天我写了很多字"), 8);
    }

    #[test]
    fn mixed_grapheme_char_count() {
        // "a👨‍👩‍👧b日" = a(1) + family(1) + b(1) + 日(1) = 4 characters.
        let s = "a👨‍👩‍👧b日";
        assert_eq!(char_count(s), 4);
    }
}
