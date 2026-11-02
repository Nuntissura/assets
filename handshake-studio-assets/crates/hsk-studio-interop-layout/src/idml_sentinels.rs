//! STU-TYP-207 stream sentinels (Master Spec v02.229, the v02.228 SD-29 assignment) in IDML
//! stories (lane SMB-SENTINELS-NATIVE-1). The spec's rule: a source that stores a page-number
//! marker as one marker plus a current / next / previous attribute, or a break as a paragraph end
//! plus a break-type attribute, maps to the matching sentinel; a code point in U+FDD0..U+FDEF in
//! foreign text is never read as a sentinel (replaced, loss reported); a target with no
//! equivalent reports the loss. The IDML forms are the ones the SD-29 research record names:
//!
//! - page-number marker: processing instruction `ACE 18` inside `Content`, with `PageNumberType`
//!   on its `CharacterStyleRange`: absent = current page number (U+FDD0), `NextPageNumber` =
//!   U+FDD1, `PreviousPageNumber` = U+FDD2 (idml.paged.media, stories-text/text-variables);
//! - section marker: `ACE 19` = U+FDD3 (same source);
//! - break: `Br` with `ParagraphBreakType` on its range: `NextFrame` = U+FDD9, `NextPage` =
//!   U+FDDA, `NextOddPage` = U+FDDB, `NextEvenPage` = U+FDDC (the record's practitioner source,
//!   UNVERIFIED against InDesign); no value is recorded for the column break.
//!
//! Not stated, so typed unavailable (never an invented mapping): the column break, flush space and
//! the footnote, endnote, cross-reference and index markers on export; any other `PageNumberType`
//! or `ParagraphBreakType` value on import. Export falls back as the spec states for plain text:
//! a break ends the paragraph (a plain `Br`), flush space becomes U+0020, a marker without a
//! resolved value writes nothing. Vendor parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::model::Attrs;

pub(crate) const PAGE_NUMBER_TYPE: &str = "PageNumberType";
pub(crate) const PARAGRAPH_BREAK_TYPE: &str = "ParagraphBreakType";
pub(crate) const PAGE_NUMBER_ACE: &str = "<?ACE 18?>";
pub(crate) const SECTION_MARKER_ACE: &str = "<?ACE 19?>";

pub(crate) const CURRENT_PAGE: char = '\u{FDD0}';
pub(crate) const NEXT_PAGE: char = '\u{FDD1}';
pub(crate) const PREVIOUS_PAGE: char = '\u{FDD2}';
pub(crate) const SECTION_MARKER: char = '\u{FDD3}';
pub(crate) const FLUSH_SPACE: char = '\u{FDDD}';

/// `(PageNumberType value, sentinel)`; an absent attribute is the current page number.
const PAGE_NUMBER_TYPES: [(&str, char); 2] = [("NextPageNumber", NEXT_PAGE), ("PreviousPageNumber", PREVIOUS_PAGE)];
/// `(ParagraphBreakType value, sentinel)`.
const BREAK_TYPES: [(&str, char); 4] =
    [("NextFrame", '\u{FDD9}'), ("NextPage", '\u{FDDA}'), ("NextOddPage", '\u{FDDB}'), ("NextEvenPage", '\u{FDDC}')];

/// Loss codes.
pub(crate) const PAGE_NUMBER_TYPE_UNMAPPED: &str = "page_number_type_unmapped";
pub(crate) const PARAGRAPH_BREAK_TYPE_UNMAPPED: &str = "paragraph_break_type_unmapped";
pub(crate) const FOREIGN_SENTINEL_REPLACED: &str = "foreign_stream_sentinel_replaced";
pub(crate) const MARKER_UNAVAILABLE: &str = "stream_marker_unavailable_in_idml";
pub(crate) const FLUSH_UNAVAILABLE: &str = "flush_space_unavailable_in_idml";
pub(crate) const COLUMN_BREAK_UNAVAILABLE: &str = "column_break_unavailable_in_idml";
pub(crate) const SENTINEL_UNMAPPED: &str = "stream_sentinel_unmapped";

pub(crate) fn attr<'a>(attrs: &'a Attrs, key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// The sentinel an `ACE` processing instruction stands for in a range with `attrs`: `None` when the
/// instruction is not a stream marker (another target or code), `Some(Err(code))` for a page-number
/// marker whose `PageNumberType` value no source states.
pub(crate) fn marker_of_instruction(target: &str, value: Option<&str>, attrs: &Attrs) -> Option<Result<char, &'static str>> {
    if target != "ACE" {
        return None;
    }
    match value.map(str::trim) {
        Some("18") => Some(match attr(attrs, PAGE_NUMBER_TYPE) {
            None => Ok(CURRENT_PAGE),
            Some(v) => PAGE_NUMBER_TYPES.iter().find(|(name, _)| *name == v).map(|(_, c)| *c).ok_or(PAGE_NUMBER_TYPE_UNMAPPED),
        }),
        Some("19") => Some(Ok(SECTION_MARKER)),
        _ => None,
    }
}

/// The paragraph terminator a `Br` in a range with `attrs` stands for: the break sentinel of its
/// `ParagraphBreakType`, U+000A without one; `Err(code)` (with U+000A) for a value no source states.
pub(crate) fn break_of(attrs: &Attrs) -> Result<char, (char, &'static str)> {
    match attr(attrs, PARAGRAPH_BREAK_TYPE) {
        None => Ok('\n'),
        Some(v) => BREAK_TYPES.iter().find(|(name, _)| *name == v).map(|(_, c)| *c).ok_or(('\n', PARAGRAPH_BREAK_TYPE_UNMAPPED)),
    }
}

/// The `ParagraphBreakType` value a break sentinel exports as; `None` for the column break.
pub(crate) fn break_type_of(c: char) -> Option<&'static str> {
    BREAK_TYPES.iter().find(|(_, s)| *s == c).map(|(name, _)| *name)
}

/// The `PageNumberType` value a page-number sentinel needs on its range (`None` = the attribute
/// is absent); `Err(())` for a character that is not a page-number marker.
pub(crate) fn page_number_type_of(c: char) -> Result<Option<&'static str>, ()> {
    if c == CURRENT_PAGE {
        return Ok(None);
    }
    PAGE_NUMBER_TYPES.iter().find(|(_, s)| *s == c).map(|(name, _)| Some(*name)).ok_or(())
}

/// The instruction a marker sentinel is written as inside `Content`.
pub(crate) fn instruction_of(c: char) -> Option<&'static str> {
    match c {
        CURRENT_PAGE | NEXT_PAGE | PREVIOUS_PAGE => Some(PAGE_NUMBER_ACE),
        SECTION_MARKER => Some(SECTION_MARKER_ACE),
        _ => None,
    }
}

/// Foreign IDML text with every U+FDD0..U+FDEF replaced by U+FFFD (the Type rule,
/// `hsk_studio_type::import_foreign_text`) and the exact number replaced.
pub(crate) fn foreign_text(text: &str) -> (String, usize) {
    if !text.chars().any(hsk_studio_type::is_sentinel_block) {
        return (text.to_owned(), 0);
    }
    let imported = hsk_studio_type::import_foreign_text(text);
    let count = imported.replaced_count;
    (imported.text, count)
}
