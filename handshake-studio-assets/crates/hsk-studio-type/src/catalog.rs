//! STU-TYP-207 insertable special-character, marker, break and white-space catalogs.
//!
//! Every member has a stable id (`<catalog>.<member>`), the spec's member name as label and
//! tooltip, a stream form and typed composition flags that layout's breakers can consume. Real
//! characters carry their Unicode scalar (Unicode Standard code points; the source applications'
//! own internal mapping is UNVERIFIED, D5). Markers, column/frame/page/odd/even breaks and the
//! flush space have no Unicode character with their semantics: their stream sentinel is a
//! spec-owner item (`StreamForm::Pending`), so this crate assigns none and inserting them refuses
//! (`stream_form_pending`) instead of inventing a second marker convention beside Folio's.
//!
//! Composition flags are Studio readings: `break_opportunity` follows the UAX #14 line-break class
//! of the scalar (BA/B2/BK opportunities; GL never breaks); `stretches` is true only for the
//! flexible-width non-breaking space; `absorbs_final_line_slack` only for the flush space
//! (STU-TYP-207). Composition behaviour itself lives in layout, not here.
//! The `currency signs` member set (dollar, cent, pound, currency sign, yen, euro) is a Studio
//! reading of the spec's unenumerated "currency signs".

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

/// How a member enters the character stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamForm {
    /// A real Unicode scalar.
    Unicode(char),
    /// No sentinel assigned yet (spec-owner item); insertion refuses.
    Pending,
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
    pub const fn scalar(&self) -> Option<char> {
        match self.stream_form {
            StreamForm::Unicode(c) => Some(c),
            StreamForm::Pending => None,
        }
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
const fn marker(id: &'static str, label: &'static str) -> CatalogMember {
    member(id, CharacterCatalog::Markers, label, StreamForm::Pending, NONE, None)
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
    marker("markers.current_page_number", "current page number"),
    marker("markers.next_page_number", "next page number"),
    marker("markers.previous_page_number", "previous page number"),
    marker("markers.section_marker", "section marker"),
    marker("markers.footnote_reference", "footnote reference"),
    marker("markers.endnote_reference", "endnote reference"),
    marker("markers.anchored_object", "anchored-object marker"),
    marker("markers.cross_reference", "cross-reference marker"),
    marker("markers.index", "index marker"),
    brk("breaks.forced_line", "forced line break", StreamForm::Unicode('\u{2028}'), BreakTarget::Line),
    brk("breaks.column", "column break", StreamForm::Pending, BreakTarget::Column),
    brk("breaks.frame", "frame break", StreamForm::Pending, BreakTarget::Frame),
    brk("breaks.page", "page break", StreamForm::Pending, BreakTarget::Page),
    brk("breaks.odd_page", "odd-page break", StreamForm::Pending, BreakTarget::OddPage),
    brk("breaks.even_page", "even-page break", StreamForm::Pending, BreakTarget::EvenPage),
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
        StreamForm::Pending,
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
    /// Marker or break without an assigned stream sentinel (spec-owner item).
    StreamFormPending,
    /// The offset is past the end or not a UTF-8 character boundary.
    InvalidOffset,
}
impl CatalogError {
    pub fn code(&self) -> &'static str {
        match self {
            CatalogError::UnknownMember => "unknown_catalog_member",
            CatalogError::StreamFormPending => "stream_form_pending",
            CatalogError::InvalidOffset => "invalid_text_offset",
        }
    }
}

pub fn catalog_member(id: &str) -> Option<&'static CatalogMember> {
    CATALOG.iter().find(|member| member.id == id)
}

/// Headless insertion of member `id` into `text` at byte offset `at`; returns the new text.
/// Refusals leave `text` untouched (it is borrowed, never mutated).
pub fn insert_catalog_member(text: &str, at: usize, id: &str) -> Result<String, CatalogError> {
    let member = catalog_member(id).ok_or(CatalogError::UnknownMember)?;
    let scalar = member.scalar().ok_or(CatalogError::StreamFormPending)?;
    if !text.is_char_boundary(at) {
        return Err(CatalogError::InvalidOffset);
    }
    let mut out = String::with_capacity(text.len().saturating_add(scalar.len_utf8()));
    out.push_str(&text[..at]);
    out.push(scalar);
    out.push_str(&text[at..]);
    Ok(out)
}
