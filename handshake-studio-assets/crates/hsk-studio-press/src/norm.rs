//! Text normalisation for matching removed text (lead decision 2026-10-11T13:10:26Z, R9-P2). Needles
//! and every text they are matched against (font readings of drawn text, text strings, decoded
//! streams read as text) go through the same steps, so text a viewer shows as the needle is found
//! whatever invisible or compatibility characters it is spelled with:
//! - language escapes: in a PDF text string an `ESC`..`ESC` sequence (ISO 32000-2 7.9.2.2; `ESC` =
//!   U+001B) is a language mark, not text; pdf.js `stringToPDFString` drops it, and an `ESC` with no
//!   closing `ESC` drops the rest of the string ([`Escapes::Segments`]). In drawn text there are no
//!   language escapes, so only the `ESC` characters are dropped ([`Escapes::Chars`]): a font cannot
//!   hide glyphs by mapping a code to `ESC`. Callers match both forms for text strings, and for any
//!   other text that holds an `ESC` (matching more is the safe side);
//! - characters that draw nothing are dropped ([`is_ignorable`]: the whole Unicode
//!   `Default_Ignorable_Code_Point` set, R10-P1: soft hyphen, combining grapheme joiner, Arabic letter
//!   mark, Hangul fillers, Khmer inherent vowels, Mongolian variation selectors, zero-width space and
//!   joiners, direction marks, embeddings, overrides and isolates (the bidi controls, R10-P2), word
//!   joiner and invisible operators, variation selectors, the byte-order mark, halfwidth Hangul filler,
//!   reserved U+FFF0..U+FFF8, shorthand format controls, musical symbol format controls, tags); the set
//!   is the `Default_Ignorable_Code_Point` property of the Unicode Character Database as baked by
//!   ICU4X `icu_properties_data` 2.3.0 (Unicode-3.0 licence; 17 ranges, 4174 code points, read from the
//!   crate's `property_binary_default_ignorable_code_point_v1` inversion list 2026-10-11; the Unicode
//!   version of that data is not stated in the crate and is UNVERIFIED here);
//! - text under an explicit right-to-left override (U+202E .. U+202C) is shown reversed by a bidi
//!   viewer (UAX #9 rule X6 with L2): text strings and text streams are also matched in that visual
//!   order ([`visual_order`], R10-P2); embeddings and isolates (which reorder whole runs, not the
//!   letters of a Latin word) are only stripped;
//! - compatibility normalisation: NFKC equivalence, computed on the NFKD form (two strings are
//!   NFKC-equivalent exactly when their NFKD forms are equal): each character is replaced by its
//!   compatibility decomposition and combining marks are put in canonical order. The table
//!   (`norm_data.rs`, generated) is minimal: characters whose decomposition lies in Latin text
//!   (Basic Latin, Latin-1, Latin Extended-A/B, combining diacritics U+0300..U+036F): precomposed
//!   accented letters, fullwidth forms, mathematical alphanumerics, circled and enclosed letters,
//!   super- and subscripts, letterlike symbols, Roman numerals, ligatures, fixed-width spaces. Other
//!   characters compare as themselves (stated in the DESCRIPTOR `proof.normalisation`);
//! - every run of white space reads as one space (U+0020).
//!
//! Tests: unit tests below; `language_escapes_and_invisible_characters_never_hide_text`,
//! `zero_width_and_compatibility_characters_never_hide_text` (R9-P2).
#![deny(clippy::arithmetic_side_effects)]
use crate::norm_data::{COMPAT, MARK_CLASS};

/// What an `ESC` (U+001B) means in the text being normalised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Escapes {
    /// A PDF text string: `ESC` .. `ESC` (or `ESC` .. end) is a language escape and is dropped.
    Segments,
    /// Drawn text and decoded streams: only the `ESC` characters are dropped.
    Chars,
}

const ESC: char = '\u{1B}';

/// Characters that draw nothing and must not split a needle: Unicode `Default_Ignorable_Code_Point`
/// (module docs; unit test `default_ignorables_are_the_ucd_set`).
pub(crate) fn is_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{AD}'
            | '\u{34F}'
            | '\u{61C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFF8}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// `text` in the order a bidi viewer shows it when it holds an explicit right-to-left override
/// (U+202E): explicit levels from the overrides and embeddings (an override to the next odd level,
/// a left-to-right override or any embedding to the next even level, since Latin letters inside an
/// embedding resolve to an even level; U+202C ends the innermost), then UAX #9 rule L2: from the
/// highest level down to 1, every run at that level or higher is reversed. `None` when the text has no
/// right-to-left override (module docs).
pub(crate) fn visual_order(text: &[char]) -> Option<Vec<char>> {
    if !text.contains(&'\u{202E}') {
        return None;
    }
    let (mut chars, mut levels): (Vec<char>, Vec<u8>) = (Vec::with_capacity(text.len()), Vec::with_capacity(text.len()));
    let mut stack: Vec<u8> = Vec::new();
    let mut cur = 0u8;
    for c in text {
        match c {
            '\u{202E}' if stack.len() < 125 => {
                stack.push(cur);
                cur = cur.saturating_add(if cur.is_multiple_of(2) { 1 } else { 2 });
            }
            '\u{202A}' | '\u{202B}' | '\u{202D}' if stack.len() < 125 => {
                stack.push(cur);
                cur = cur.saturating_add(if cur.is_multiple_of(2) { 2 } else { 1 });
            }
            '\u{202C}' => {
                if let Some(l) = stack.pop() {
                    cur = l;
                }
            }
            other => {
                chars.push(*other);
                levels.push(cur);
            }
        }
    }
    let max = levels.iter().copied().max().unwrap_or(0);
    for lvl in (1..=max).rev() {
        let mut i = 0usize;
        while i < chars.len() {
            if levels.get(i).is_some_and(|l| *l >= lvl) {
                let start = i;
                while levels.get(i).is_some_and(|l| *l >= lvl) {
                    i = i.saturating_add(1);
                }
                if let Some(run) = chars.get_mut(start..i) {
                    run.reverse();
                }
                if let Some(run) = levels.get_mut(start..i) {
                    run.reverse();
                }
            } else {
                i = i.saturating_add(1);
            }
        }
    }
    Some(chars)
}

/// Compatibility decomposition of `c` from the table, if it has one.
fn decomposition(c: char) -> Option<&'static str> {
    COMPAT.binary_search_by(|(k, _)| k.cmp(&c)).ok().and_then(|i| COMPAT.get(i)).map(|(_, d)| *d)
}

/// Canonical combining class (only the marks U+0300..U+036F the table produces; every other
/// character is a starter here).
fn mark_class(c: char) -> u8 {
    let cp = u32::from(c);
    if (0x300..0x370).contains(&cp) {
        cp.checked_sub(0x300).and_then(|i| usize::try_from(i).ok()).and_then(|i| MARK_CLASS.get(i)).copied().unwrap_or(0)
    } else {
        0
    }
}

/// Streaming normaliser: characters are fed in order with their source position; the sink receives
/// the normalised characters (a combining-mark run is held until it ends, then emitted in canonical
/// order).
struct Norm<F: FnMut(char, usize)> {
    sink: F,
    escapes: Escapes,
    in_escape: bool,
    last_space: bool,
    marks: Vec<(char, usize)>,
}

impl<F: FnMut(char, usize)> Norm<F> {
    fn new(escapes: Escapes, sink: F) -> Self {
        Self { sink, escapes, in_escape: false, last_space: false, marks: Vec::new() }
    }

    fn feed(&mut self, c: char, at: usize) {
        if c == ESC {
            if self.escapes == Escapes::Segments {
                self.in_escape = !self.in_escape;
            }
            return;
        }
        if self.in_escape || is_ignorable(c) {
            return;
        }
        match decomposition(c) {
            Some(d) => d.chars().for_each(|x| self.emit(x, at)),
            None => self.emit(c, at),
        }
    }

    fn emit(&mut self, c: char, at: usize) {
        if mark_class(c) != 0 {
            self.marks.push((c, at));
            return;
        }
        self.flush_marks();
        if c.is_whitespace() {
            if !self.last_space {
                self.last_space = true;
                (self.sink)(' ', at);
            }
        } else {
            self.last_space = false;
            (self.sink)(c, at);
        }
    }

    fn flush_marks(&mut self) {
        if self.marks.is_empty() {
            return;
        }
        let mut marks = std::mem::take(&mut self.marks);
        marks.sort_by_key(|(c, _)| mark_class(*c));
        self.last_space = false;
        for (c, at) in marks {
            (self.sink)(c, at);
        }
    }
}

/// `text` normalised (module docs) and, per output character, the index of the input character it
/// came from.
pub(crate) fn normalize(text: &[char], escapes: Escapes) -> (Vec<char>, Vec<usize>) {
    let (mut chars, mut src) = (Vec::with_capacity(text.len()), Vec::with_capacity(text.len()));
    let mut n = Norm::new(escapes, |c, at| {
        chars.push(c);
        src.push(at);
    });
    for (at, c) in text.iter().enumerate() {
        n.feed(*c, at);
    }
    n.flush_marks();
    drop(n);
    (chars, src)
}

/// [`normalize`] without the source map.
pub(crate) fn normalized(text: &[char], escapes: Escapes) -> Vec<char> {
    normalize(text, escapes).0
}

/// [`normalize`] of a string into a string (no source map; UTF-8 sized, for large decoded streams).
pub(crate) fn normalized_str(text: &str, escapes: Escapes) -> String {
    let mut out = String::with_capacity(text.len());
    let mut n = Norm::new(escapes, |c, _| out.push(c));
    for c in text.chars() {
        n.feed(c, 0);
    }
    n.flush_marks();
    drop(n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str, e: Escapes) -> String {
        normalized(&s.chars().collect::<Vec<_>>(), e).into_iter().collect()
    }

    #[test]
    fn escapes_invisible_and_compatibility_characters_normalise() {
        // Language escapes in a text string (ISO 32000-2 7.9.2.2; pdf.js drops ESC..ESC and ESC..end).
        assert_eq!(n("SEC\u{1B}en\u{1B}RET", Escapes::Segments), "SECRET");
        assert_eq!(n("SECRET\u{1B}enUS", Escapes::Segments), "SECRET");
        // Drawn text: only the ESC characters go.
        assert_eq!(n("SEC\u{1B}RET\u{1B}", Escapes::Chars), "SECRET");
        // Invisible characters.
        assert_eq!(n("S\u{AD}E\u{200B}C\u{200D}R\u{2060}E\u{FEFF}T\u{FE0F}", Escapes::Chars), "SECRET");
        // Compatibility forms: fullwidth, mathematical bold, circled, ligature, Roman numeral, NBSP runs.
        assert_eq!(n("\u{FF33}\u{FF25}\u{FF23}", Escapes::Chars), "SEC");
        assert_eq!(n("\u{1D412}\u{1D404}\u{1D402}", Escapes::Chars), "SEC");
        assert_eq!(n("\u{24C8}\u{24BA}", Escapes::Chars), "SE");
        assert_eq!(n("o\u{FB03}ce \u{2163}", Escapes::Chars), "office IV");
        assert_eq!(n("a\u{A0}\u{2003} \tb", Escapes::Chars), "a b");
        // Precomposed and decomposed accents compare equal; marks are put in canonical order
        // (U+0323 dot below, class 220, before U+0302 circumflex, class 230).
        assert_eq!(n("caf\u{E9}", Escapes::Chars), n("cafe\u{301}", Escapes::Chars));
        assert_eq!(n("a\u{302}\u{323}", Escapes::Chars), n("a\u{323}\u{302}", Escapes::Chars));
        assert_eq!(n("\u{1EAD}", Escapes::Chars), "a\u{323}\u{302}");
        // Source map: each output char points at its input char; the string form agrees.
        let (chars, src) = normalize(&"\u{FB01}x".chars().collect::<Vec<_>>(), Escapes::Chars);
        assert_eq!((chars, src), (vec!['f', 'i', 'x'], vec![0, 0, 1]));
        for s in ["SEC\u{1B}en\u{1B}RET  x", "\u{1EAD}\u{FF33}\u{200B}"] {
            assert_eq!(normalized_str(s, Escapes::Segments), n(s, Escapes::Segments));
        }
    }

    #[test]
    fn default_ignorables_are_the_ucd_set() {
        // The 17 ranges of the inversion list (module docs), counted: 4174 code points.
        let count = (0..=0x10FFFFu32).filter_map(char::from_u32).filter(|c| is_ignorable(*c)).count();
        assert_eq!(count, 4174);
        for c in ['\u{17B4}', '\u{1BCA0}', '\u{3164}', '\u{FFA0}', '\u{2065}', '\u{115F}', '\u{FFF8}'] {
            assert!(is_ignorable(c), "{:04X}", c as u32);
        }
        for c in ['\u{FFF9}', '\u{1161}', ' ', 'A'] {
            assert!(!is_ignorable(c), "{:04X}", c as u32);
        }
        // A right-to-left override shows its run reversed; other text has no second form.
        let v = |s: &str| visual_order(&s.chars().collect::<Vec<_>>()).map(|v| v.into_iter().collect::<String>());
        assert_eq!(v("\u{202E}AHPLA-TERCES\u{202C} plan").as_deref(), Some("SECRET-ALPHA plan"));
        assert_eq!(v("x \u{202E}CBA").as_deref(), Some("x ABC"));
        assert_eq!(v("\u{202E}C\u{202A}ab\u{202C}B\u{202C}").as_deref(), Some("BabC"));
        assert_eq!(v("plain"), None);
    }

    #[test]
    fn table_is_sorted_and_versioned() {
        assert!(COMPAT.windows(2).all(|w| w[0].0 < w[1].0), "binary search needs sorted code points");
        assert_eq!(COMPAT.len(), 2008);
        assert_eq!(crate::norm_data::UNICODE_VERSION, "16.0.0");
    }
}
