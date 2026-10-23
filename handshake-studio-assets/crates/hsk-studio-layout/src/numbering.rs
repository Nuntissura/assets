//! Sections and page numbering (STU-LAY-014, STU-LAY-015, STU-LAY-108): page labels of every
//! document page from the Folio sections (`StudioLongDocRecords::sections`), and the number formats
//! shared with footnotes. Rules (documented; InDesign parity UNVERIFIED, D5):
//! - Pages are numbered in document order (spreads in order, pages left to right). A section runs
//!   from its start page to the next section's start page. Without sections the whole document is
//!   one implicit arabic section starting at 1.
//! - At a section start the number is `page_number_start` (absent = 1) when `continue_numbering` is
//!   false, else the previous page's number + 1 (1 for the first page).
//! - Label = section prefix (only with `include_section_prefix`) + the number in the section style.
//! - Formats: arabic; roman 1..=3999 (larger is `number_out_of_range`); letters a..z, then the
//!   letter repeated (27 = aa, 53 = aaa; at most 64 repeats); single / double / triple leading
//!   zeros pad to 2 / 3 / 4 digits; full-width arabic (U+FF10..U+FF19); kanji (Japanese numerals
//!   with 十 百 千 万 億, below 10^9, 0 = 〇); symbols * † ‡ § ‖ ¶ then doubled, tripled (at most 64
//!   rounds); asterisks (n asterisks, n <= 64); format_none = empty. Arabic alif-ba-tah, abjad,
//!   Hebrew and katakana styles are typed `number_style_unsupported` (declared gap); `current` is
//!   legal only on a text variable (`number_style_not_legal_here`).
//! - "Page X of Y": [`last_page_label`] with document or section scope (STU-LAY-015
//!   `variable_scope`).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_folio::{StudioDocument, StudioNumberStyle, StudioSection, check_sections};

/// Typed refusals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberingError {
    /// A section rule of STU-LAY-108 (the Folio code, e.g. `section_prefix_too_long`).
    Sections(&'static str),
    StyleUnsupported(StudioNumberStyle),
    NotLegalHere,
    OutOfRange,
}
impl NumberingError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Sections(w) => w,
            Self::StyleUnsupported(_) => "number_style_unsupported",
            Self::NotLegalHere => "number_style_not_legal_here",
            Self::OutOfRange => "number_out_of_range",
        }
    }
}
impl std::fmt::Display for NumberingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for NumberingError {}

const MAX_REPEAT: u32 = 64;

fn roman(mut n: u32, upper: bool) -> Result<String, NumberingError> {
    if !(1..=3999).contains(&n) {
        return Err(NumberingError::OutOfRange);
    }
    const T: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut s = String::new();
    for (v, t) in T {
        while n >= v {
            s.push_str(t);
            n = n.saturating_sub(v);
        }
    }
    Ok(if upper { s.to_uppercase() } else { s })
}

/// `n` in `style` (module docs). Zero is legal only for arabic-like styles and kanji.
pub fn format_number(n: u32, style: StudioNumberStyle) -> Result<String, NumberingError> {
    use StudioNumberStyle as S;
    let repeat = |unit: &str, times: u32| -> Result<String, NumberingError> {
        if times == 0 || times > MAX_REPEAT {
            return Err(NumberingError::OutOfRange);
        }
        Ok(unit.repeat(times as usize))
    };
    match style {
        S::Arabic => Ok(n.to_string()),
        S::SingleLeadingZeros => Ok(format!("{n:02}")),
        S::DoubleLeadingZeros => Ok(format!("{n:03}")),
        S::TripleLeadingZeros => Ok(format!("{n:04}")),
        S::FullWidthArabic => Ok(n
            .to_string()
            .chars()
            .filter_map(|c| c.to_digit(10).and_then(|d| char::from_u32(0xFF10_u32.saturating_add(d))))
            .collect()),
        S::UpperRoman => roman(n, true),
        S::LowerRoman => roman(n, false),
        S::UpperLetters | S::LowerLetters => {
            let k = n.checked_sub(1).ok_or(NumberingError::OutOfRange)?;
            let letter = char::from(b'a'.saturating_add((k % 26) as u8));
            let letter = if style == S::UpperLetters { letter.to_ascii_uppercase() } else { letter };
            repeat(&letter.to_string(), (k / 26).saturating_add(1))
        }
        S::Symbols => {
            const SYM: [&str; 6] = ["*", "\u{2020}", "\u{2021}", "\u{a7}", "\u{2016}", "\u{b6}"];
            let k = n.checked_sub(1).ok_or(NumberingError::OutOfRange)?;
            repeat(SYM[(k % 6) as usize], (k / 6).saturating_add(1))
        }
        S::Asterisks => repeat("*", n),
        S::FormatNone => Ok(String::new()),
        S::Kanji => kanji(n),
        S::Current => Err(NumberingError::NotLegalHere),
        S::ArabicAlifBaTah
        | S::ArabicAbjad
        | S::HebrewBiblical
        | S::HebrewNonStandard
        | S::KatakanaModern
        | S::KatakanaTraditional => Err(NumberingError::StyleUnsupported(style)),
    }
}

fn kanji(n: u32) -> Result<String, NumberingError> {
    const D: [char; 10] = ['〇', '一', '二', '三', '四', '五', '六', '七', '八', '九'];
    if n == 0 {
        return Ok("〇".into());
    }
    if n >= 1_000_000_000 {
        return Err(NumberingError::OutOfRange);
    }
    let group = |g: u32, out: &mut String| {
        for (unit, v) in [('千', 1000u32), ('百', 100), ('十', 10)] {
            let d = g.checked_div(v).unwrap_or(0) % 10;
            if d > 0 {
                if d > 1 {
                    out.push(D[d as usize]);
                }
                out.push(unit);
            }
        }
        let d = g % 10;
        if d > 0 {
            out.push(D[d as usize]);
        }
    };
    let mut s = String::new();
    for (unit, v) in [(Some('億'), 100_000_000u32), (Some('万'), 10_000), (None, 1)] {
        let g = n.checked_div(v).unwrap_or(0) % 10_000;
        if g == 0 {
            continue;
        }
        if g == 1 && unit.is_some() {
            s.push('一');
        } else {
            group(g, &mut s);
        }
        if let Some(u) = unit {
            s.push(u);
        }
    }
    Ok(s)
}

/// The number and label of one document page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageNumber {
    pub page_id: String,
    pub spread: usize,
    /// Position in the spread.
    pub page: usize,
    /// 1-based document page ordinal.
    pub ordinal: usize,
    /// Index into the sections slice; `None` = the implicit section (no sections).
    pub section: Option<usize>,
    pub number: u32,
    pub label: String,
}

/// Page labels of every document page (module docs). The sections are checked first with Folio's
/// `check_sections` (the document is not assumed validated).
pub fn number_pages(
    document: &StudioDocument,
    sections: &[StudioSection],
) -> Result<Vec<PageNumber>, NumberingError> {
    check_sections(sections, document).map_err(NumberingError::Sections)?;
    let mut out = Vec::new();
    let mut current: Option<usize> = None;
    let mut number: u32 = 0;
    let mut ordinal = 0usize;
    for (si, spread) in document.page_spreads.iter().enumerate() {
        for (pi, page) in spread.pages.iter().enumerate() {
            ordinal = ordinal.checked_add(1).ok_or(NumberingError::OutOfRange)?;
            if let Some(k) = sections.iter().position(|s| s.page_start == page.page_id) {
                current = Some(k);
                let s = &sections[k];
                number = if s.continue_numbering {
                    number.checked_add(1).ok_or(NumberingError::OutOfRange)?
                } else {
                    s.page_number_start.unwrap_or(1)
                };
            } else {
                number = number.checked_add(1).ok_or(NumberingError::OutOfRange)?;
            }
            let (style, prefix) = match current.map(|k| &sections[k]) {
                Some(s) => (
                    s.page_number_style,
                    if s.include_section_prefix { s.section_prefix.as_deref().unwrap_or("") } else { "" },
                ),
                None => (StudioNumberStyle::Arabic, ""),
            };
            let label = format!("{prefix}{}", format_number(number, style)?);
            out.push(PageNumber {
                page_id: page.page_id.clone(),
                spread: si,
                page: pi,
                ordinal,
                section: current,
                number,
                label,
            });
        }
    }
    Ok(out)
}

/// STU-LAY-015 `variable_scope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberScope {
    Document,
    Section,
}

/// The label of the last page of the document, or of the section holding `pages[at]`.
pub fn last_page_label(pages: &[PageNumber], scope: NumberScope, at: usize) -> Option<&str> {
    let here = pages.get(at)?;
    match scope {
        NumberScope::Document => pages.last(),
        NumberScope::Section => pages.iter().rev().find(|p| p.section == here.section),
    }
    .map(|p| p.label.as_str())
}
