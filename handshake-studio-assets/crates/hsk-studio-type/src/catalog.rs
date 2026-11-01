//! STU-TYP-207 insertable special-character, marker, break and white-space catalogs.
//!
//! Every member has a stable id (`<catalog>.<member>`), the spec's member name as label and
//! tooltip, a stream form and typed composition flags that layout's breakers can consume. Real
//! characters carry their Unicode scalar (Unicode Standard code points; the source applications'
//! own internal mapping is UNVERIFIED, D5).
//!
//! Stream encoding (v02.228, SD-29): a member that is a real Unicode character is stored as that
//! scalar (forced line break U+2028, paragraph return U+2029, the anchored-object marker U+FFFC).
//! A member with no Unicode character carrying its meaning (eight markers, the five column /
//! frame / page / odd-page / even-page breaks, the flush space) is stored as one Studio stream
//! sentinel from the noncharacter block U+FDD0..U+FDEF ([`StreamForm::Sentinel`]): U+FDD0..U+FDDD
//! are assigned in the spec's order, U+FDDE..U+FDEF are reserved. Private-use code points are not
//! used. Flush space is not U+2001 EM QUAD (its canonical decomposition is U+2003). The break
//! sentinels end their paragraph exactly as U+2029 does ([`is_break_sentinel`]; the paragraph
//! partition of request validation and of the shaping engine treats them as terminators, and bidi
//! receives a same-length U+2029 stand-in; the vendored bidi tables are unchanged). Typed
//! marker records (kind, target, options by story byte) and their resolution belong to layout.
//!
//! Foreign text: [`import_foreign_text`] never reads U+FDD0..U+FDEF as a sentinel; it replaces each
//! with U+FFFD and reports the loss. [`insert_literal_text`] refuses the reserved U+FDDE..U+FDEF.
//! A foreign U+FFFC and the other noncharacters (U+FFFE, U+FFFF, plane ends) are not covered by
//! SD-29 and pass through unchanged (proposal in the lane notes).
//!
//! Composition flags are Studio readings: `break_opportunity` follows the UAX #14 line-break class
//! of the scalar (BA/B2/BK opportunities; GL never breaks); `stretches` is true only for the
//! flexible-width non-breaking space; `absorbs_final_line_slack` only for the flush space
//! (STU-TYP-207). Composition behaviour itself lives in layout, not here.
//! The `currency signs` member set (dollar, cent, pound, currency sign, yen, euro) is a Studio
//! reading of the spec's unenumerated "currency signs".

#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe as observe;

/// The four STU-TYP-207 catalogs, in table order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CharacterCatalog {
    Symbols,
    Markers,
    BreakCharacters,
    WhiteSpaceCharacters,
}
impl CharacterCatalog {
    pub const ALL: [CharacterCatalog; 4] = [
        CharacterCatalog::Symbols,
        CharacterCatalog::Markers,
        CharacterCatalog::BreakCharacters,
        CharacterCatalog::WhiteSpaceCharacters,
    ];
    /// The catalog name as the STU-TYP-207 table writes it.
    pub const fn label(self) -> &'static str {
        match self {
            CharacterCatalog::Symbols => "Symbols",
            CharacterCatalog::Markers => "Markers",
            CharacterCatalog::BreakCharacters => "Break characters",
            CharacterCatalog::WhiteSpaceCharacters => "White-space characters",
        }
    }
    pub const fn id(self) -> &'static str {
        match self {
            CharacterCatalog::Symbols => "symbols",
            CharacterCatalog::Markers => "markers",
            CharacterCatalog::BreakCharacters => "breaks",
            CharacterCatalog::WhiteSpaceCharacters => "white_space",
        }
    }
    pub fn members(self) -> impl Iterator<Item = &'static CatalogMember> {
        CATALOG.iter().filter(move |member| member.catalog == self)
    }
}

/// How a member enters the character stream (STU-TYP-207, v02.228 SD-29).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamForm {
    /// A real Unicode scalar (including U+FFFC OBJECT REPLACEMENT CHARACTER for the
    /// anchored-object marker).
    Unicode(char),
    /// A Studio stream sentinel in U+FDD0..U+FDDD; no normalisation form changes it and fonts do
    /// not map it.
    Sentinel(char),
}

/// The flow target of a break member; odd- and even-page breaks are distinct from a page break.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BreakTarget {
    Line,
    Column,
    Frame,
    Page,
    OddPage,
    EvenPage,
    Paragraph,
}

/// Typed composition flags for layout's breakers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CompositionFlags {
    pub stretches: bool,
    pub break_opportunity: bool,
    pub absorbs_final_line_slack: bool,
}

/// One insertable catalog member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogMember {
    pub id: &'static str,
    pub catalog: CharacterCatalog,
    /// The member name as the spec writes it (also the tooltip text).
    pub label: &'static str,
    pub stream_form: StreamForm,
    pub flags: CompositionFlags,
    pub break_target: Option<BreakTarget>,
}
impl CatalogMember {
    pub const fn tooltip(&self) -> &'static str {
        self.label
    }
    /// The one stream character this member is stored as.
    pub const fn scalar(&self) -> char {
        match self.stream_form {
            StreamForm::Unicode(c) | StreamForm::Sentinel(c) => c,
        }
    }
    pub const fn is_sentinel(&self) -> bool {
        matches!(self.stream_form, StreamForm::Sentinel(_))
    }
}

const NONE: CompositionFlags = CompositionFlags {
    stretches: false,
    break_opportunity: false,
    absorbs_final_line_slack: false,
};
const BREAKS: CompositionFlags = CompositionFlags {
    stretches: false,
    break_opportunity: true,
    absorbs_final_line_slack: false,
};

const fn member(
    id: &'static str,
    catalog: CharacterCatalog,
    label: &'static str,
    stream_form: StreamForm,
    flags: CompositionFlags,
    break_target: Option<BreakTarget>,
) -> CatalogMember {
    CatalogMember {
        id,
        catalog,
        label,
        stream_form,
        flags,
        break_target,
    }
}
const fn sym(id: &'static str, label: &'static str, c: char, flags: CompositionFlags) -> CatalogMember {
    member(id, CharacterCatalog::Symbols, label, StreamForm::Unicode(c), flags, None)
}
const fn marker(id: &'static str, label: &'static str, form: StreamForm) -> CatalogMember {
    member(id, CharacterCatalog::Markers, label, form, NONE, None)
}
const fn sentinel(code: u32) -> StreamForm {
    match char::from_u32(code) {
        Some(c) => StreamForm::Sentinel(c),
        None => panic!("sentinel code point"),
    }
}
const fn brk(id: &'static str, label: &'static str, form: StreamForm, target: BreakTarget) -> CatalogMember {
    member(id, CharacterCatalog::BreakCharacters, label, form, BREAKS, Some(target))
}
const fn space(id: &'static str, label: &'static str, form: StreamForm, flags: CompositionFlags) -> CatalogMember {
    member(id, CharacterCatalog::WhiteSpaceCharacters, label, form, flags, None)
}

/// Every member of the four catalogs, in table order.
pub const CATALOG: &[CatalogMember] = &[
    sym("symbols.em_dash", "em dash", '\u{2014}', BREAKS),
    sym("symbols.en_dash", "en dash", '\u{2013}', BREAKS),
    sym("symbols.discretionary_hyphen", "discretionary hyphen", '\u{00AD}', BREAKS),
    sym("symbols.non_breaking_hyphen", "non-breaking hyphen", '\u{2011}', NONE),
    sym("symbols.ellipsis", "ellipsis", '\u{2026}', NONE),
    sym("symbols.bullet", "bullet", '\u{2022}', NONE),
    sym("symbols.copyright", "copyright", '\u{00A9}', NONE),
    sym("symbols.registered", "registered", '\u{00AE}', NONE),
    sym("symbols.trademark", "trademark", '\u{2122}', NONE),
    sym("symbols.section", "section", '\u{00A7}', NONE),
    sym("symbols.paragraph_mark", "paragraph mark", '\u{00B6}', NONE),
    sym("symbols.single_quote_open", "single typographer quote (open)", '\u{2018}', NONE),
    sym("symbols.single_quote_close", "single typographer quote (close)", '\u{2019}', NONE),
    sym("symbols.double_quote_open", "double typographer quote (open)", '\u{201C}', NONE),
    sym("symbols.double_quote_close", "double typographer quote (close)", '\u{201D}', NONE),
    sym("symbols.degree", "degree", '\u{00B0}', NONE),
    sym("symbols.currency_dollar", "currency sign: dollar", '\u{0024}', NONE),
    sym("symbols.currency_cent", "currency sign: cent", '\u{00A2}', NONE),
    sym("symbols.currency_pound", "currency sign: pound", '\u{00A3}', NONE),
    sym("symbols.currency_generic", "currency sign: generic", '\u{00A4}', NONE),
    sym("symbols.currency_yen", "currency sign: yen", '\u{00A5}', NONE),
    sym("symbols.currency_euro", "currency sign: euro", '\u{20AC}', NONE),
    marker("markers.current_page_number", "current page number", sentinel(0xFDD0)),
    marker("markers.next_page_number", "next page number", sentinel(0xFDD1)),
    marker("markers.previous_page_number", "previous page number", sentinel(0xFDD2)),
    marker("markers.section_marker", "section marker", sentinel(0xFDD3)),
    marker("markers.footnote_reference", "footnote reference", sentinel(0xFDD4)),
    marker("markers.endnote_reference", "endnote reference", sentinel(0xFDD5)),
    marker("markers.anchored_object", "anchored-object marker", StreamForm::Unicode('\u{FFFC}')),
    marker("markers.cross_reference", "cross-reference marker", sentinel(0xFDD6)),
    marker("markers.index", "index marker", sentinel(0xFDD7)),
    brk("breaks.forced_line", "forced line break", StreamForm::Unicode('\u{2028}'), BreakTarget::Line),
    brk("breaks.column", "column break", sentinel(0xFDD8), BreakTarget::Column),
    brk("breaks.frame", "frame break", sentinel(0xFDD9), BreakTarget::Frame),
    brk("breaks.page", "page break", sentinel(0xFDDA), BreakTarget::Page),
    brk("breaks.odd_page", "odd-page break", sentinel(0xFDDB), BreakTarget::OddPage),
    brk("breaks.even_page", "even-page break", sentinel(0xFDDC), BreakTarget::EvenPage),
    brk("breaks.paragraph_return", "paragraph return", StreamForm::Unicode('\u{2029}'), BreakTarget::Paragraph),
    space("white_space.em", "em space", StreamForm::Unicode('\u{2003}'), BREAKS),
    space("white_space.en", "en space", StreamForm::Unicode('\u{2002}'), BREAKS),
    space("white_space.third", "third space", StreamForm::Unicode('\u{2004}'), BREAKS),
    space("white_space.quarter", "quarter space", StreamForm::Unicode('\u{2005}'), BREAKS),
    space("white_space.sixth", "sixth space", StreamForm::Unicode('\u{2006}'), BREAKS),
    space("white_space.thin", "thin space", StreamForm::Unicode('\u{2009}'), BREAKS),
    space("white_space.hair", "hair space", StreamForm::Unicode('\u{200A}'), BREAKS),
    space("white_space.figure", "figure space", StreamForm::Unicode('\u{2007}'), NONE),
    space("white_space.punctuation", "punctuation space", StreamForm::Unicode('\u{2008}'), BREAKS),
    space(
        "white_space.flush",
        "flush space",
        sentinel(0xFDDD),
        CompositionFlags {
            stretches: false,
            break_opportunity: true,
            absorbs_final_line_slack: true,
        },
    ),
    space(
        "white_space.non_breaking_fixed",
        "non-breaking space (fixed width)",
        StreamForm::Unicode('\u{202F}'),
        NONE,
    ),
    space(
        "white_space.non_breaking_flexible",
        "non-breaking space (flexible width)",
        StreamForm::Unicode('\u{00A0}'),
        CompositionFlags {
            stretches: true,
            break_opportunity: false,
            absorbs_final_line_slack: false,
        },
    ),
];

/// Typed refusal of a catalog insertion; the text is left unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogError {
    UnknownMember,
    /// The offset is past the end or not a UTF-8 character boundary.
    InvalidOffset,
    /// Literal text carries a code point of U+FDDE..U+FDEF, which SD-29 reserves for later Studio
    /// sentinels (no other meaning may be given to it).
    ReservedSentinel,
}
impl CatalogError {
    /// Stable snake-case code; carries no text, ids or offsets.
    pub fn code(&self) -> &'static str {
        match self {
            CatalogError::UnknownMember => "unknown_catalog_member",
            CatalogError::InvalidOffset => "invalid_text_offset",
            CatalogError::ReservedSentinel => "reserved_stream_sentinel",
        }
    }
}

pub fn catalog_member(id: &str) -> Option<&'static CatalogMember> {
    CATALOG.iter().find(|member| member.id == id)
}

/// The catalog member stored as the stream character `c`, if any (sentinels, U+FFFC, U+2028,
/// U+2029 and every real character of the catalogs). No two members share a stream character.
pub fn catalog_member_for_scalar(c: char) -> Option<&'static CatalogMember> {
    CATALOG.iter().find(|member| member.scalar() == c)
}

/// First and last code point of the noncharacter block SD-29 draws every sentinel from.
pub const SENTINEL_BLOCK_FIRST: char = '\u{FDD0}';
pub const SENTINEL_BLOCK_LAST: char = '\u{FDEF}';
/// First reserved code point (U+FDD0..U+FDDD are assigned, U+FDDE..U+FDEF reserved).
pub const SENTINEL_RESERVED_FIRST: char = '\u{FDDE}';
/// Stand-in for a foreign sentinel-block code point on import (U+FFFD REPLACEMENT CHARACTER; the
/// spec says "replaced" without naming the replacement, so this is a Studio reading).
pub const IMPORT_REPLACEMENT: char = '\u{FFFD}';
/// Most losses an import report lists individually; the count stays exact beyond it.
pub const MAX_REPORTED_IMPORT_LOSSES: usize = 4096;

/// True for U+FDD0..U+FDEF, the whole block SD-29 reserves for Studio stream sentinels.
pub const fn is_sentinel_block(c: char) -> bool {
    matches!(c, SENTINEL_BLOCK_FIRST..=SENTINEL_BLOCK_LAST)
}
/// True for U+FDDE..U+FDEF, which no member uses and which must never be given another meaning.
pub const fn is_reserved_sentinel(c: char) -> bool {
    matches!(c, SENTINEL_RESERVED_FIRST..=SENTINEL_BLOCK_LAST)
}
/// True for the five break sentinels U+FDD8..U+FDDC (column, frame, page, odd-page, even-page
/// break). Each ends its paragraph exactly as U+2029 does; the shaping engine's paragraph
/// partition treats them as terminators (`validate::ends_paragraph`, `engine::bidi_text`).
pub const fn is_break_sentinel(c: char) -> bool {
    matches!(c, '\u{FDD8}'..='\u{FDDC}')
}

/// Headless insertion of member `id` into `text` at byte offset `at`; returns the new text.
/// Refusals leave `text` untouched (it is borrowed, never mutated).
pub fn insert_catalog_member(text: &str, at: usize, id: &str) -> Result<String, CatalogError> {
    let member = catalog_member(id).ok_or(CatalogError::UnknownMember)?;
    insert_str(text, at, member.scalar().encode_utf8(&mut [0; 4]))
}

/// Headless insertion of literal (typed, IME or API) text at byte offset `at`. A code point of
/// U+FDDE..U+FDEF refuses with [`CatalogError::ReservedSentinel`] and the text stays unchanged;
/// everything else, including an assigned sentinel of Studio's own stream, inserts as given.
pub fn insert_literal_text(text: &str, at: usize, literal: &str) -> Result<String, CatalogError> {
    if literal.chars().any(is_reserved_sentinel) {
        return Err(CatalogError::ReservedSentinel);
    }
    insert_str(text, at, literal)
}

fn insert_str(text: &str, at: usize, inserted: &str) -> Result<String, CatalogError> {
    if !text.is_char_boundary(at) {
        return Err(CatalogError::InvalidOffset);
    }
    let mut out = String::with_capacity(text.len().saturating_add(inserted.len()));
    out.push_str(&text[..at]);
    out.push_str(inserted);
    out.push_str(&text[at..]);
    Ok(out)
}

/// One foreign sentinel-block code point replaced on import.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplacedSentinel {
    /// Byte offset of the code point in the foreign text.
    pub offset: usize,
    pub code_point: char,
}

/// Result of [`import_foreign_text`]: the text with every U+FDD0..U+FDEF replaced and the loss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignTextImport {
    pub text: String,
    /// Exact number of replaced code points.
    pub replaced_count: usize,
    /// The first [`MAX_REPORTED_IMPORT_LOSSES`] replacements, in source order.
    pub replaced: Vec<ReplacedSentinel>,
}
impl ForeignTextImport {
    /// True when at least one code point was replaced: the import must report a loss.
    pub fn is_lossy(&self) -> bool {
        self.replaced_count > 0
    }
}

/// Imports foreign text (STU-TYP-207, SD-29): a code point in U+FDD0..U+FDEF is never read as a
/// sentinel; it is replaced with [`IMPORT_REPLACEMENT`] and the loss is reported. Studio's own
/// sentinels enter the stream only through [`insert_catalog_member`]. The input is not changed.
pub fn import_foreign_text(foreign: &str) -> ForeignTextImport {
    let mut text = String::with_capacity(foreign.len());
    let mut replaced = Vec::new();
    let mut replaced_count = 0usize;
    for (offset, c) in foreign.char_indices() {
        if is_sentinel_block(c) {
            replaced_count = replaced_count.saturating_add(1);
            if replaced.len() < MAX_REPORTED_IMPORT_LOSSES {
                replaced.push(ReplacedSentinel { offset, code_point: c });
            }
            text.push(IMPORT_REPLACEMENT);
        } else {
            text.push(c);
        }
    }
    ForeignTextImport { text, replaced_count, replaced }
}

/// Observe failure class of a catalog refusal (closed Observe vocabulary).
pub fn catalog_failure_code(error: &CatalogError) -> observe::FailureCode {
    match error {
        CatalogError::UnknownMember | CatalogError::InvalidOffset | CatalogError::ReservedSentinel => {
            observe::FailureCode::Validation
        }
    }
}

/// Emits one terminal Observe frame for a finished catalog insertion: `Success` or
/// `Failure(catalog_failure_code)`. Only the correlation, revision and the fixed outcome are sent
/// (no text, ids or offsets).
pub fn report_catalog_outcome(
    observer: &mut observe::Observe,
    sink: &mut dyn observe::SinkPort,
    correlation_id: u64,
    revision: u64,
    outcome: Result<(), &CatalogError>,
    cancel: &CancellationToken,
) -> Result<observe::Receipt, observe::Error> {
    let outcome = match outcome {
        Ok(()) => observe::Outcome::Success,
        Err(error) => observe::Outcome::Failure(catalog_failure_code(error)),
    };
    emit(observer, sink, correlation_id, revision, outcome, cancel)
}

/// Emits one terminal Observe frame for a finished foreign-text import: `Success` when nothing
/// was replaced, `Failure(Loss)` when at least one sentinel-block code point was (the loss report
/// of SD-29). The replaced count and offsets stay with the caller; no text reaches Observe.
pub fn report_import_outcome(
    observer: &mut observe::Observe,
    sink: &mut dyn observe::SinkPort,
    correlation_id: u64,
    revision: u64,
    import: &ForeignTextImport,
    cancel: &CancellationToken,
) -> Result<observe::Receipt, observe::Error> {
    let outcome = if import.is_lossy() {
        observe::Outcome::Failure(observe::FailureCode::Loss)
    } else {
        observe::Outcome::Success
    };
    emit(observer, sink, correlation_id, revision, outcome, cancel)
}

fn emit(
    observer: &mut observe::Observe,
    sink: &mut dyn observe::SinkPort,
    correlation_id: u64,
    revision: u64,
    outcome: observe::Outcome,
    cancel: &CancellationToken,
) -> Result<observe::Receipt, observe::Error> {
    let event = observe::Observation {
        correlation_id,
        revision,
        outcome,
        progress: None,
        private_project_text: None,
    };
    observer.emit(event, cancel, sink)
}
