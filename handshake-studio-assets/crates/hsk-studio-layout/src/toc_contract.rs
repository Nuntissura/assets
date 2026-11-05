//! Table-of-contents contract (lane SMB-ID-TOC-INDEX-1; STU-LAY-044, STU-LAY-148): the full TOC
//! style record (every field of the style table) and TOC entry record (every field of the entry
//! table), typed paragraph and character style references, and the attributed generate, update and
//! update-all commands. The older `generate_toc` (Folio `StudioTocStyle`, no attribution) is kept
//! unchanged for its callers. Persistence (lane SMB-ID-TOC-PERSIST-1): Folio's `StudioTocStyle`
//! carries every field of this record; [`PersistedTocStyle`] is the lossless lift both ways (the
//! style record plus Folio's `toc_id` and the persisted `include_overset` argument), and
//! [`TocStyle::from_folio`] reads the persisted values without filling any in. The six booleans of
//! the style table have no spec default: `None` (unknown, e.g. a record written before they were
//! persisted) is carried as such, and generation refuses an unknown flag (`toc_flag_unset`) rather
//! than inventing a value.
//!
//! Rules (Studio readings; InDesign parity UNVERIFIED, D5):
//! - Sources are read in the given order, paragraphs in story order; a paragraph whose style is a
//!   mapped entry `name` becomes an entry. Its page is the page of the frame holding the first line
//!   whose source span contains (or starts at) the paragraph start, a paragraph opening with a
//!   glyph-less byte taking its first line (as `generate_toc`, `toc::line_of`).
//! - `include_overset` (a generate-command argument; the spec declares no default, so the caller
//!   states it): an unplaced paragraph is listed without a page label when set, else omitted and
//!   counted (`omitted_overset`), never silently dropped.
//! - `include_hidden`: a paragraph whose frame layer has `visible == false` is listed only when set;
//!   otherwise it is omitted and counted (`omitted_hidden`).
//! - Entry text = the paragraph text without its trailing break, tabs as spaces; forced line breaks
//!   (U+2028) become spaces only with `remove_forced_line_break`, otherwise they are kept.
//! - `page_number_position`: `after` = text + separator + label, `before` = label + separator +
//!   text, `none` = text. The separator run carries `separator_style`, the label run
//!   `page_number_style`, the entry paragraph `format_style` (character ranges and paragraphs of the
//!   generated story).
//! - Levels nest (an entry's children are the following deeper entries); siblings sort by their
//!   lower-cased text, stably, when every sibling's level has `sort_alphabet`.
//! - `run_in`: entries of the deepest mapped level continue the paragraph of the previous entry,
//!   joined by [`RUN_IN_JOINER`] (the spec declares no joiner: a Studio choice, lane notes).
//! - `make_anchor`: a text anchor at each entry's source paragraph start (an existing anchor at that
//!   byte is reused). A source story the caller names as locked gets no anchor and the entry carries
//!   [`TocAnchorOutcome::NotCreatedStoryLocked`]: the degradation is reported, never skipped.
//! - `create_bookmarks`: one [`TocBookmark`] per entry (level, text, page, baseline, anchor).
//! - `include_book_documents`: the entries come from the combined book ([`TocBook`]); without one
//!   the command returns `toc_book_unavailable` (no invented book behaviour); a book chapter missing
//!   from the combined book is `book_document_missing`.
//! - `replace_existing` with an existing story replaces its CONTENT only (story id, text kind and
//!   direction kept); otherwise the TOC is a new story with the request's story id, and the place
//!   point travels with it for the caller to place (no frame is created here).
//! - `numbered_paragraphs` is an enumeration whose members the spec does not list: it is carried as
//!   a captured token and has no effect on generation (spec gap, lane notes). `set_story_direction`
//!   is carried to the result; Folio stories have no vertical direction to apply it to.
//!
//! Typed errors of STU-LAY-148: `toc_no_entries` (a TOC style with no entry; a default-styled TOC
//! is the exception), `toc_style_mapped_twice`, and the locked-story anchor outcome above. Writing
//! into a story the caller names as locked fails closed (`toc_story_locked`, STU-LAY-143/166).
//! Attribution: every command takes the caller's `CommandContext` (actor, client session, command
//! id, base revision, correlation); a context without a command id or against another revision is
//! refused. Hostile input: entries, sources, paragraphs, anchors, text bytes are bounded
//! ([`TocLimits`]), counts checked before each push.
#![deny(clippy::arithmetic_side_effects)]
use crate::LayoutError;
use crate::book::CombinedBook;
use crate::derived_text::{DerivedBudget, DerivedPages, DerivedRefusal, DerivedRules, derive_text};
use crate::numbering::PageNumber;
use crate::threaded_reflow::FramePlacement;
use crate::toc::{TocLimits, TocSource, baseline_of, line_of, page_of};
use hsk_studio_accord::{CancellationToken, CommandContext, DomainId};
use hsk_studio_folio::{
    MAX_TOC_ENTRIES, MAX_TOC_LEVEL, StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind, StudioBook,
    StudioDocument, StudioTextAnchor, StudioTextStory, StudioTocEntry, StudioTocStyle, StudioTypeStyleRecords, TextStorySchema,
    TocPageNumberPosition, TypeStyleScope, is_captured_token,
};
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};
use std::collections::{BTreeMap, BTreeSet};

/// Joins a run-in entry to the previous entry's paragraph (Studio choice; the spec declares none).
pub const RUN_IN_JOINER: &str = "; ";
/// Bytes of a TOC style name (a Studio bound for hostile input; the spec declares none).
pub const MAX_TOC_NAME_BYTES: usize = 256;
/// Bytes of a captured enumeration token (Folio's bound).
pub use hsk_studio_folio::MAX_CAPTURED_TOKEN_BYTES;
/// TOC styles one update-all refreshes (a Studio bound for hostile input).
pub const MAX_TOCS_PER_UPDATE: usize = 1024;

/// A `STYS` style id used as a paragraph style.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParagraphStyleRef(String);
/// A `STYS` style id used as a character style.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CharacterStyleRef(String);

fn stys(value: &str) -> bool {
    DomainId::parse(value).is_ok_and(|d| d.prefix() == "STYS")
}
impl ParagraphStyleRef {
    /// A `STYS` domain id; anything else is `style_ref_invalid`.
    pub fn parse(value: &str) -> Result<Self, StyleRefError> {
        stys(value).then(|| Self(value.to_owned())).ok_or(StyleRefError)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl CharacterStyleRef {
    /// A `STYS` domain id; anything else is `style_ref_invalid`.
    pub fn parse(value: &str) -> Result<Self, StyleRefError> {
        stys(value).then(|| Self(value.to_owned())).ok_or(StyleRefError)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// A style reference that is not a `STYS` domain id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleRefError;
impl StyleRefError {
    pub const fn code(self) -> &'static str {
        "style_ref_invalid"
    }
}

/// Checks a reference against the type-style records: present and of the expected scope.
pub(crate) fn style_scope_ok(
    records: Option<&StudioTypeStyleRecords>,
    id: &str,
    scope: TypeStyleScope,
) -> Result<(), &'static str> {
    let Some(r) = records else { return Ok(()) };
    match r.styles.iter().find(|s| s.style_id == id) {
        None => Err("style_missing"),
        Some(s) if s.scope != scope => Err("style_scope"),
        Some(_) => Ok(()),
    }
}

/// STU-LAY-148 `set_story_direction`: Folio's persisted enumeration (one type for the record and
/// the contract, lane SMB-ID-TOC-PERSIST-1).
pub use hsk_studio_folio::TocStoryDirection;

/// A captured member of an enumeration whose member list the spec does not declare
/// (`numbered_paragraphs`): Folio's rule [`hsk_studio_folio::is_captured_token`] (ASCII lower-case
/// letters, digits, `.` and `_`, 1..=64 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedToken(String);
impl CapturedToken {
    pub fn parse(value: &str) -> Option<Self> {
        is_captured_token(value).then(|| Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One TOC entry (the eight fields of the STU-LAY-148 entry table).
#[derive(Clone, Debug, PartialEq)]
pub struct TocEntryStyle {
    /// The source paragraph style this entry maps.
    pub name: ParagraphStyleRef,
    /// The style applied to the generated entry paragraph.
    pub format_style: Option<ParagraphStyleRef>,
    /// Indent level, 1..=`MAX_TOC_LEVEL`.
    pub level: u32,
    pub page_number_style: Option<CharacterStyleRef>,
    pub page_number_position: TocPageNumberPosition,
    /// Between entry text and page number; at most 100 characters, no line break.
    pub separator: String,
    pub separator_style: Option<CharacterStyleRef>,
    pub sort_alphabet: bool,
}

/// One TOC style (the eleven fields of the STU-LAY-148 style table plus its entries). There is no
/// `Default`: the spec declares no default for any field, so the caller states every one.
#[derive(Clone, Debug, PartialEq)]
pub struct TocStyle {
    pub name: String,
    /// The generated heading text; empty = no title paragraph.
    pub title: String,
    pub title_style: Option<ParagraphStyleRef>,
    /// The six style-table booleans: `None` = unknown (no spec default); generation needs them stated.
    pub include_book_documents: Option<bool>,
    pub include_hidden: Option<bool>,
    pub run_in: Option<bool>,
    pub create_bookmarks: Option<bool>,
    pub make_anchor: Option<bool>,
    pub remove_forced_line_break: Option<bool>,
    /// Members undeclared by the spec: a captured token, `None` = unset.
    pub numbered_paragraphs: Option<CapturedToken>,
    /// `None` = unset (no spec default).
    pub set_story_direction: Option<TocStoryDirection>,
    /// The default-styled TOC, the one style allowed without entries (STU-LAY-148).
    pub default_styled: bool,
    pub entries: Vec<TocEntryStyle>,
}
impl TocStyle {
    /// The persisted Folio style read as this record (every persisted value, unknown kept unknown);
    /// Folio's `toc_id` and `include_overset` are not style-table fields: [`PersistedTocStyle`]
    /// keeps them.
    pub fn from_folio(s: &StudioTocStyle) -> Result<Self, TocCommandError> {
        let para = |v: &str| ParagraphStyleRef::parse(v).map_err(|_| TocCommandError::StyleRef);
        let chr = |v: &Option<String>| v.as_deref().map(CharacterStyleRef::parse).transpose().map_err(|_| TocCommandError::StyleRef);
        let entries = s
            .entries
            .iter()
            .map(|e| {
                Ok(TocEntryStyle {
                    name: para(&e.paragraph_style)?,
                    format_style: e.format_style.as_deref().map(para).transpose()?,
                    level: e.level,
                    page_number_style: chr(&e.page_number_style)?,
                    page_number_position: e.page_number_position,
                    separator: e.separator.clone(),
                    separator_style: chr(&e.separator_style)?,
                    sort_alphabet: e.sort_alphabet,
                })
            })
            .collect::<Result<Vec<_>, TocCommandError>>()?;
        let numbered_paragraphs = s
            .numbered_paragraphs
            .as_deref()
            .map(|t| CapturedToken::parse(t).ok_or(TocCommandError::NumberedParagraphs))
            .transpose()?;
        Ok(Self {
            name: s.name.clone(),
            title: s.title.clone(),
            title_style: s.title_style.as_deref().map(para).transpose()?,
            include_book_documents: s.include_book_documents,
            include_hidden: s.include_hidden,
            run_in: s.run_in,
            create_bookmarks: s.create_bookmarks,
            make_anchor: s.make_anchor,
            remove_forced_line_break: s.remove_forced_line_break,
            numbered_paragraphs,
            set_story_direction: s.set_story_direction,
            default_styled: s.default_styled,
            entries,
        })
    }

    /// This record as a persisted Folio style with `toc_id` and the `include_overset` argument
    /// (the inverse of [`TocStyle::from_folio`]).
    pub fn to_folio(&self, toc_id: &str, include_overset: bool) -> StudioTocStyle {
        let own = |r: &Option<ParagraphStyleRef>| r.as_ref().map(|r| r.as_str().to_owned());
        let own_chr = |r: &Option<CharacterStyleRef>| r.as_ref().map(|r| r.as_str().to_owned());
        StudioTocStyle {
            toc_id: toc_id.to_owned(),
            name: self.name.clone(),
            title: self.title.clone(),
            title_style: own(&self.title_style),
            include_overset,
            entries: self
                .entries
                .iter()
                .map(|e| StudioTocEntry {
                    paragraph_style: e.name.as_str().to_owned(),
                    format_style: own(&e.format_style),
                    level: e.level,
                    page_number_position: e.page_number_position,
                    separator: e.separator.clone(),
                    sort_alphabet: e.sort_alphabet,
                    page_number_style: own_chr(&e.page_number_style),
                    separator_style: own_chr(&e.separator_style),
                })
                .collect(),
            include_book_documents: self.include_book_documents,
            include_hidden: self.include_hidden,
            run_in: self.run_in,
            create_bookmarks: self.create_bookmarks,
            make_anchor: self.make_anchor,
            remove_forced_line_break: self.remove_forced_line_break,
            numbered_paragraphs: self.numbered_paragraphs.as_ref().map(|t| t.as_str().to_owned()),
            set_story_direction: self.set_story_direction,
            default_styled: self.default_styled,
        }
    }

    /// The six style-table booleans, each stated (`toc_flag_unset` names the first unknown one in
    /// table order).
    pub fn stated_flags(&self) -> Result<TocFlags, TocCommandError> {
        let flag = |v: Option<bool>, name: &'static str| v.ok_or(TocCommandError::FlagUnset(name));
        Ok(TocFlags {
            include_book_documents: flag(self.include_book_documents, "include_book_documents")?,
            include_hidden: flag(self.include_hidden, "include_hidden")?,
            run_in: flag(self.run_in, "run_in")?,
            create_bookmarks: flag(self.create_bookmarks, "create_bookmarks")?,
            make_anchor: flag(self.make_anchor, "make_anchor")?,
            remove_forced_line_break: flag(self.remove_forced_line_break, "remove_forced_line_break")?,
        })
    }

    /// The STU-LAY-148 style rules (module docs); with type-style records every reference must
    /// exist with its scope (paragraph or character).
    pub fn check(&self, styles: Option<&StudioTypeStyleRecords>) -> Result<(), TocCommandError> {
        if self.name.trim().is_empty() || self.name.len() > MAX_TOC_NAME_BYTES || self.name.chars().any(char::is_control) {
            return Err(TocCommandError::Name);
        }
        if self.title.len() > 1024 || self.title.chars().any(char::is_control) {
            return Err(TocCommandError::Title);
        }
        if self.entries.is_empty() && !self.default_styled {
            return Err(TocCommandError::NoEntries);
        }
        if self.entries.len() > MAX_TOC_ENTRIES {
            return Err(TocCommandError::BudgetExceeded);
        }
        let para = |r: &ParagraphStyleRef| {
            style_scope_ok(styles, r.as_str(), TypeStyleScope::Paragraph).map_err(TocCommandError::StyleRecord)
        };
        let chr = |r: &CharacterStyleRef| {
            style_scope_ok(styles, r.as_str(), TypeStyleScope::Character).map_err(TocCommandError::StyleRecord)
        };
        if let Some(t) = &self.title_style {
            para(t)?;
        }
        let mut mapped = BTreeSet::new();
        for e in &self.entries {
            if !mapped.insert(e.name.as_str()) {
                return Err(TocCommandError::StyleMappedTwice);
            }
            if !(1..=MAX_TOC_LEVEL).contains(&e.level) {
                return Err(TocCommandError::Level);
            }
            if e.separator.chars().count() > 100 || e.separator.contains(['\n', '\r', '\u{2029}']) {
                return Err(TocCommandError::Separator);
            }
            para(&e.name)?;
            if let Some(f) = &e.format_style {
                para(f)?;
            }
            for c in [&e.page_number_style, &e.separator_style].into_iter().flatten() {
                chr(c)?;
            }
        }
        Ok(())
    }
}

/// The six STU-LAY-148 style booleans as stated for one generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TocFlags {
    pub include_book_documents: bool,
    pub include_hidden: bool,
    pub run_in: bool,
    pub create_bookmarks: bool,
    pub make_anchor: bool,
    pub remove_forced_line_break: bool,
}

/// A persisted Folio TOC style lifted whole: the style record plus the two persisted members that
/// are not style-table fields. `from_folio` and `to_folio` are inverse (lossless both ways).
#[derive(Clone, Debug, PartialEq)]
pub struct PersistedTocStyle {
    /// Folio's document-local key.
    pub toc_id: String,
    /// The persisted include-overset argument of the generate command (pass it as
    /// [`TocGenerateRequest::include_overset`]).
    pub include_overset: bool,
    pub style: TocStyle,
}
impl PersistedTocStyle {
    pub fn from_folio(s: &StudioTocStyle) -> Result<Self, TocCommandError> {
        Ok(Self { toc_id: s.toc_id.clone(), include_overset: s.include_overset, style: TocStyle::from_folio(s)? })
    }
    pub fn to_folio(&self) -> StudioTocStyle {
        self.style.to_folio(&self.toc_id, self.include_overset)
    }
}

/// Typed refusals of the TOC commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TocCommandError {
    /// STU-LAY-148: a TOC style with no entry (not default-styled).
    NoEntries,
    /// STU-LAY-148: one paragraph style mapped twice in one TOC style.
    StyleMappedTwice,
    Name,
    Title,
    Level,
    Separator,
    /// A style reference that is not a `STYS` id.
    StyleRef,
    /// A style reference absent from the type-style records (`style_missing`) or of the wrong
    /// scope (`style_scope`).
    StyleRecord(&'static str),
    /// Two TOC styles of one update share a name (STU-LAY-143 unique style names).
    NameDuplicate,
    /// Two TOCs of one update write the same story.
    StoryShared,
    /// The TOC would read itself (target story is a source, or a format/title style is mapped).
    Loop,
    /// The story the TOC is written into is locked (fails closed, STU-LAY-143/166).
    StoryLocked(String),
    /// `include_book_documents` without a combined book.
    BookUnavailable,
    /// A chapter of the book is missing from the combined book.
    DocumentMissingFromBook(String),
    /// The place point names no page of the document or is not finite.
    PlacePoint,
    /// The new story id is not an `STXT` id.
    StoryId,
    /// The command context carries no command id.
    CommandMissing,
    StaleRevision { expected: u64, current: u64 },
    BudgetExceeded,
    Layout(LayoutError),
    /// A style-table boolean generation reads is unknown (no spec default; never filled in).
    FlagUnset(&'static str),
    /// A persisted `numbered_paragraphs` that is not a captured token.
    NumberedParagraphs,
}
impl TocCommandError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoEntries => "toc_no_entries",
            Self::StyleMappedTwice => "toc_style_mapped_twice",
            Self::Name => "toc_name",
            Self::Title => "toc_title",
            Self::Level => "toc_level",
            Self::Separator => "toc_separator",
            Self::StyleRef => "toc_style_ref",
            Self::StyleRecord(c) => c,
            Self::NameDuplicate => "toc_style_name_duplicate",
            Self::StoryShared => "toc_story_shared",
            Self::Loop => "toc_loop",
            Self::StoryLocked(_) => "toc_story_locked",
            Self::BookUnavailable => "toc_book_unavailable",
            Self::DocumentMissingFromBook(_) => "book_document_missing",
            Self::PlacePoint => "toc_place_point",
            Self::StoryId => "toc_story_id",
            Self::CommandMissing => "command_id_missing",
            Self::StaleRevision { .. } => "stale_revision",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Layout(e) => e.code(),
            Self::FlagUnset(_) => "toc_flag_unset",
            Self::NumberedParagraphs => "toc_numbered_paragraphs",
        }
    }
    /// The Observe failure class; `None` for cancellation.
    pub fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Layout(e) => e.failure_code(),
            Self::BookUnavailable | Self::DocumentMissingFromBook(_) => Some(FailureCode::Unavailable),
            _ => Some(FailureCode::Validation),
        }
    }
}
impl std::fmt::Display for TocCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for TocCommandError {}

/// A combined book the TOC (or index) reads when `include_book_documents` is set.
#[derive(Clone, Copy, Debug)]
pub struct TocBook<'a> {
    pub book: &'a StudioBook,
    pub combined: &'a CombinedBook,
}
impl<'a> TocBook<'a> {
    /// Every book chapter must be in the combined book.
    pub(crate) fn check(&self) -> Result<(), String> {
        for c in &self.book.chapters {
            if !self.combined.chapter_pages.iter().any(|(id, _, _)| *id == c.document_id) {
                return Err(c.document_id.clone());
            }
        }
        Ok(())
    }
}

/// Where a new TOC story is to be placed (the caller places it; no frame is created here).
#[derive(Clone, Debug, PartialEq)]
pub struct PlacePoint {
    pub page_id: String,
    pub x_pt: f64,
    pub y_pt: f64,
}

/// One TOC generate (or update) request.
#[derive(Clone, Copy, Debug)]
pub struct TocGenerateRequest<'a> {
    /// The document the TOC is written into.
    pub document: &'a StudioDocument,
    pub style: &'a TocStyle,
    /// Type-style records to check the style references against (`None` = not checked).
    pub styles: Option<&'a StudioTypeStyleRecords>,
    pub sources: &'a [TocSource<'a>],
    /// `number_pages` of `document` (of the combined book when book documents are included).
    pub pages: &'a [PageNumber],
    pub replace_existing: bool,
    /// The existing TOC story (replaced in place with `replace_existing`).
    pub existing: Option<&'a StudioTextStory>,
    /// Story id of a new TOC story (`STXT`).
    pub new_story_id: &'a str,
    pub book: Option<TocBook<'a>>,
    pub place_point: Option<&'a PlacePoint>,
    /// STU-LAY-148 include-overset flag (no spec default: stated by the caller).
    pub include_overset: bool,
    /// Story ids the caller holds locked (Folio stores no story lock).
    pub locked_stories: &'a [String],
    /// Anchors already in the document (reused; new anchor ids and names avoid them).
    pub existing_anchors: &'a [StudioTextAnchor],
    pub limits: TocLimits,
}

/// What `make_anchor` did for one entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TocAnchorOutcome {
    Created(StudioTextAnchor),
    /// An anchor already at the source byte is reused.
    Existing(String),
    /// STU-LAY-148: text anchors are not created in a locked story; reported, not ignored.
    NotCreatedStoryLocked { story_id: String, source_start: u64 },
}
impl TocAnchorOutcome {
    pub fn anchor_id(&self) -> Option<&str> {
        match self {
            Self::Created(a) => Some(&a.anchor_id),
            Self::Existing(id) => Some(id),
            Self::NotCreatedStoryLocked { .. } => None,
        }
    }
}

/// One generated entry.
#[derive(Clone, Debug, PartialEq)]
pub struct TocContractEntry {
    pub level: u32,
    pub text: String,
    pub page_label: Option<String>,
    pub page_id: Option<String>,
    pub baseline_y: Option<f64>,
    pub story_id: String,
    pub source_start: u64,
    /// The `make_anchor` outcome (`None` without `make_anchor`).
    pub anchor: Option<TocAnchorOutcome>,
    /// The entry was written run-in (continuing the previous entry's paragraph).
    pub run_in: bool,
    /// Markers of the source paragraph whose value `text` does not show (`derived_text`).
    pub marker_gaps: Vec<crate::DerivedMarkerGap>,
}

/// One bookmark (`create_bookmarks`).
#[derive(Clone, Debug, PartialEq)]
pub struct TocBookmark {
    pub level: u32,
    pub title: String,
    pub page_id: Option<String>,
    pub baseline_y: Option<f64>,
    pub anchor_id: Option<String>,
}

/// The generated TOC.
#[derive(Clone, Debug, PartialEq)]
pub struct TocGenerated {
    pub style_name: String,
    pub story: StudioTextStory,
    /// The story content replaced an existing TOC story.
    pub replaced: bool,
    pub place_point: Option<PlacePoint>,
    pub entries: Vec<TocContractEntry>,
    pub bookmarks: Vec<TocBookmark>,
    pub story_direction: Option<TocStoryDirection>,
    pub numbered_paragraphs: Option<CapturedToken>,
    /// Mapped paragraphs left out because they are overset (`include_overset` off).
    pub omitted_overset: usize,
    /// Mapped paragraphs left out because their frame layer is hidden (`include_hidden` off).
    pub omitted_hidden: usize,
    /// Mapped paragraphs left out because they hold only markers, so no text (`derived_text`).
    pub omitted_no_text: usize,
    /// (story id, gap): markers of those paragraphs, which have no entry to carry them.
    pub omitted_marker_gaps: Vec<(String, crate::DerivedMarkerGap)>,
}
impl TocGenerated {
    /// Anchors this generation created (to add to the layout-flow records).
    pub fn created_anchors(&self) -> impl Iterator<Item = &StudioTextAnchor> {
        self.entries.iter().filter_map(|e| match &e.anchor {
            Some(TocAnchorOutcome::Created(a)) => Some(a),
            _ => None,
        })
    }
    /// Entries whose anchor was not created because the source story is locked.
    pub fn locked_anchor_count(&self) -> usize {
        self.entries.iter().filter(|e| matches!(e.anchor, Some(TocAnchorOutcome::NotCreatedStoryLocked { .. }))).count()
    }
}

/// Builds a story of paragraphs made of runs with optional character styles, within a byte budget.
/// Paragraphs and character ranges tile the text (Folio story rules); a newline ends a paragraph.
pub(crate) struct RunWriter {
    pub text: String,
    pub paragraphs: Vec<StoryParagraph>,
    /// (start, end, style) runs; adjacent runs of one style are merged.
    runs: Vec<(u64, u64, Option<String>)>,
    max: usize,
}
impl RunWriter {
    pub(crate) fn new(max: usize) -> Self {
        Self { text: String::new(), paragraphs: Vec::new(), runs: Vec::new(), max }
    }
    fn append(&mut self, s: &str, style: Option<&str>) -> Result<(), ()> {
        if s.is_empty() {
            return Ok(());
        }
        let need = self.text.len().checked_add(s.len()).filter(|n| *n <= self.max).ok_or(())?;
        let start = self.text.len() as u64;
        self.text.push_str(s);
        debug_assert_eq!(self.text.len(), need);
        let end = self.text.len() as u64;
        match self.runs.last_mut() {
            Some(last) if last.2.as_deref() == style && last.1 == start => last.1 = end,
            _ => self.runs.push((start, end, style.map(str::to_owned))),
        }
        Ok(())
    }
    /// Starts a paragraph (a newline ends the previous one).
    pub(crate) fn paragraph(&mut self, style: Option<&str>) -> Result<(), ()> {
        if !self.paragraphs.is_empty() {
            self.append("\n", None)?;
            let end = self.text.len() as u64;
            if let Some(last) = self.paragraphs.last_mut() {
                last.end = end;
            }
        }
        let start = self.text.len() as u64;
        self.paragraphs.push(StoryParagraph { start, end: start, style_ref: style.map(str::to_owned), paragraph_direction: None });
        Ok(())
    }
    /// A run in the current paragraph.
    pub(crate) fn run(&mut self, s: &str, style: Option<&str>) -> Result<(), ()> {
        self.append(s, style)?;
        let end = self.text.len() as u64;
        if let Some(last) = self.paragraphs.last_mut() {
            last.end = end;
        }
        Ok(())
    }
    /// Drops empty paragraphs (a paragraph must be non-empty to tile) by folding them into the
    /// previous one, and returns the story.
    pub(crate) fn into_story(self, id: &str, kind: StoryTextKind, direction: StoryDirection) -> StudioTextStory {
        let mut paragraphs: Vec<StoryParagraph> = Vec::with_capacity(self.paragraphs.len());
        for p in self.paragraphs {
            if p.end > p.start {
                paragraphs.push(p);
            } else if let Some(prev) = paragraphs.last_mut() {
                prev.end = p.end.max(prev.end);
            }
        }
        StudioTextStory {
            schema_id: TextStorySchema::V1,
            story_id: id.to_owned(),
            text_kind: kind,
            story_direction: direction,
            content: StoryContent::Inline { text_utf8: self.text },
            paragraphs,
            character_ranges: self
                .runs
                .into_iter()
                .map(|(start, end, style_ref)| StoryRange { start, end, style_ref, applied_language: None })
                .collect(),
        }
    }
}

/// The frame placement holding the line of story byte `at` (`toc::line_of`).
pub(crate) fn frame_of<'s>(source: &TocSource<'s>, at: u64) -> Option<&'s FramePlacement> {
    line_of(source, at).map(|(f, _)| f)
}

/// True when byte `at` is placed in a frame whose layer is hidden.
pub(crate) fn on_hidden_layer(document: &StudioDocument, source: &TocSource<'_>, at: u64) -> bool {
    frame_of(source, at).and_then(|f| document.layers.get(f.frame_index)).is_some_and(|l| !l.visible)
}

pub(crate) fn check_context(ctx: &CommandContext, document: &StudioDocument) -> Result<(), TocCommandError> {
    if ctx.command().is_none() {
        return Err(TocCommandError::CommandMissing);
    }
    if ctx.base_revision() != document.revision {
        return Err(TocCommandError::StaleRevision { expected: ctx.base_revision(), current: document.revision });
    }
    Ok(())
}

struct Node {
    entry: TocContractEntry,
    sort: bool,
    style_index: usize,
    children: Vec<Node>,
}
fn sort_nodes(nodes: &mut [Node]) {
    if !nodes.is_empty() && nodes.iter().all(|n| n.sort) {
        nodes.sort_by_cached_key(|n| n.entry.text.to_lowercase());
    }
    for n in nodes.iter_mut() {
        sort_nodes(&mut n.children);
    }
}
fn flatten(nodes: Vec<Node>, out: &mut Vec<(TocContractEntry, usize)>) {
    for n in nodes {
        out.push((n.entry, n.style_index));
        flatten(n.children, out);
    }
}

/// Picks a fresh anchor id and name that no existing (or already created) anchor uses.
fn fresh_anchor(taken_ids: &mut BTreeSet<String>, taken_names: &mut BTreeSet<String>, next: &mut u64) -> Option<(String, String)> {
    for _ in 0..1_000_000u32 {
        let id = format!("toc-anchor-{next}");
        *next = next.checked_add(1)?;
        let name = format!("TOC anchor {id}");
        if !taken_ids.contains(&id) && !taken_names.contains(&name) {
            taken_ids.insert(id.clone());
            taken_names.insert(name.clone());
            return Some((id, name));
        }
    }
    None
}

/// Generates one TOC (module docs). An update is the same call with `replace_existing` and the
/// existing TOC story.
pub fn generate_table_of_contents(
    ctx: &CommandContext,
    req: &TocGenerateRequest<'_>,
    cancel: &CancellationToken,
) -> Result<TocGenerated, TocCommandError> {
    let mut taken_ids: BTreeSet<String> = req.existing_anchors.iter().map(|a| a.anchor_id.clone()).collect();
    let mut taken_names: BTreeSet<String> = req.existing_anchors.iter().map(|a| a.name.clone()).collect();
    generate_inner(ctx, req, &mut taken_ids, &mut taken_names, cancel)
}

/// Replaces the content of an existing TOC story (an update): `generate_table_of_contents` with
/// `replace_existing` set and `existing` = `story`.
pub fn update_table_of_contents(
    ctx: &CommandContext,
    req: &TocGenerateRequest<'_>,
    story: &StudioTextStory,
    cancel: &CancellationToken,
) -> Result<TocGenerated, TocCommandError> {
    let r = TocGenerateRequest { replace_existing: true, existing: Some(story), new_story_id: &story.story_id, ..*req };
    generate_table_of_contents(ctx, &r, cancel)
}

/// One TOC of an update-all: its style and the story it was generated into.
#[derive(Clone, Copy, Debug)]
pub struct TocInstance<'a> {
    pub style: &'a TocStyle,
    pub story: &'a StudioTextStory,
}

/// An update-all refusal: which TOC (index into `tocs`) and why; nothing is updated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocBatchError {
    pub toc: usize,
    pub error: TocCommandError,
}

/// Refreshes every TOC of the document, each with its own style (STU-LAY-148 update-all). All or
/// nothing: the first refusal is returned with its TOC index. `base` supplies the shared inputs;
/// its style, story and replace fields are taken from each instance.
pub fn update_all_tables_of_contents(
    ctx: &CommandContext,
    base: &TocGenerateRequest<'_>,
    tocs: &[TocInstance<'_>],
    cancel: &CancellationToken,
) -> Result<Vec<TocGenerated>, TocBatchError> {
    let at = |toc: usize| move |error: TocCommandError| TocBatchError { toc, error };
    if tocs.len() > MAX_TOCS_PER_UPDATE {
        return Err(at(0)(TocCommandError::BudgetExceeded));
    }
    let (mut names, mut stories) = (BTreeSet::new(), BTreeSet::new());
    for (k, t) in tocs.iter().enumerate() {
        if !names.insert(t.style.name.as_str()) {
            return Err(at(k)(TocCommandError::NameDuplicate));
        }
        if !stories.insert(t.story.story_id.as_str()) {
            return Err(at(k)(TocCommandError::StoryShared));
        }
    }
    let mut taken_ids: BTreeSet<String> = base.existing_anchors.iter().map(|a| a.anchor_id.clone()).collect();
    let mut taken_names: BTreeSet<String> = base.existing_anchors.iter().map(|a| a.name.clone()).collect();
    let mut out = Vec::with_capacity(tocs.len());
    for (k, t) in tocs.iter().enumerate() {
        let r = TocGenerateRequest {
            style: t.style,
            replace_existing: true,
            existing: Some(t.story),
            new_story_id: &t.story.story_id,
            place_point: None,
            ..*base
        };
        out.push(generate_inner(ctx, &r, &mut taken_ids, &mut taken_names, cancel).map_err(at(k))?);
    }
    Ok(out)
}

fn generate_inner(
    ctx: &CommandContext,
    req: &TocGenerateRequest<'_>,
    taken_ids: &mut BTreeSet<String>,
    taken_names: &mut BTreeSet<String>,
    cancel: &CancellationToken,
) -> Result<TocGenerated, TocCommandError> {
    let canceled = |_| TocCommandError::Layout(LayoutError::Canceled);
    cancel.check().map_err(canceled)?;
    check_context(ctx, req.document)?;
    let style = req.style;
    style.check(req.styles)?;
    let flags = style.stated_flags()?;
    if req.sources.len() > req.limits.max_entries {
        return Err(TocCommandError::BudgetExceeded);
    }
    // Where the entries come from: the document, or the combined book.
    let (doc, pages) = if flags.include_book_documents {
        let book = req.book.ok_or(TocCommandError::BookUnavailable)?;
        book.check().map_err(TocCommandError::DocumentMissingFromBook)?;
        (&book.combined.document, book.combined.pages.as_slice())
    } else {
        (req.document, req.pages)
    };
    // The target story.
    let replacing = req.replace_existing.then_some(req.existing).flatten();
    let target_id = match replacing {
        Some(s) => s.story_id.as_str(),
        None => {
            if !DomainId::parse(req.new_story_id).is_ok_and(|d| d.prefix() == "STXT") {
                return Err(TocCommandError::StoryId);
            }
            req.new_story_id
        }
    };
    if req.locked_stories.iter().any(|s| s == target_id) {
        return Err(TocCommandError::StoryLocked(target_id.to_owned()));
    }
    if let Some(p) = req.place_point
        && (req.document.locate_page(&p.page_id).is_none() || !p.x_pt.is_finite() || !p.y_pt.is_finite())
    {
        return Err(TocCommandError::PlacePoint);
    }
    let mapped: BTreeMap<&str, usize> = style.entries.iter().enumerate().map(|(i, e)| (e.name.as_str(), i)).collect();
    let feeds_back = style
        .entries
        .iter()
        .filter_map(|e| e.format_style.as_ref())
        .chain(style.title_style.as_ref())
        .any(|s| mapped.contains_key(s.as_str()));
    if feeds_back || req.sources.iter().any(|s| s.story.story_id == target_id) {
        return Err(TocCommandError::Loop);
    }
    for s in req.sources {
        if let Some(p) = s.placement
            && p.base_revision != doc.revision
        {
            return Err(TocCommandError::StaleRevision { expected: p.base_revision, current: doc.revision });
        }
        if s.text.len() as u64 != s.story.byte_len() {
            return Err(TocCommandError::Layout(LayoutError::InvalidInput));
        }
    }
    let by_page: BTreeMap<(usize, usize), &PageNumber> = pages.iter().map(|p| ((p.spread, p.page), p)).collect();
    let locked: BTreeSet<&str> = req.locked_stories.iter().map(String::as_str).collect();
    let existing_at: BTreeMap<(&str, u64), &str> =
        req.existing_anchors.iter().map(|a| ((a.story_id.as_str(), a.byte), a.anchor_id.as_str())).collect();
    let derived_pages = DerivedPages { pages, by_page: &by_page, sections: None };
    let rules = DerivedRules { forced_break_to_space: flags.remove_forced_line_break, tab_to_space: true };
    let mut budget = DerivedBudget::new();
    let mut next_anchor = 1u64;
    let (mut omitted_overset, mut omitted_hidden) = (0usize, 0usize);
    let mut flat: Vec<(TocContractEntry, bool, usize)> = Vec::new();
    let mut scanned = 0usize;
    for s in req.sources {
        for p in &s.story.paragraphs {
            scanned = scanned.saturating_add(1);
            if scanned.is_multiple_of(256) {
                cancel.check().map_err(canceled)?;
            }
            let Some(&ei) = p.style_ref.as_deref().and_then(|r| mapped.get(r)) else {
                continue;
            };
            let e = &style.entries[ei];
            if !flags.include_hidden && on_hidden_layer(doc, s, p.start) {
                omitted_hidden = omitted_hidden.saturating_add(1);
                continue;
            }
            let page = page_of(s, p.start, &by_page);
            if page.is_none() && !req.include_overset {
                omitted_overset = omitted_overset.saturating_add(1);
                continue;
            }
            if flat.len() >= req.limits.max_entries {
                return Err(TocCommandError::BudgetExceeded);
            }
            let raw = usize::try_from(p.start)
                .ok()
                .zip(usize::try_from(p.end).ok())
                .and_then(|(a, b)| s.text.get(a..b))
                .ok_or(TocCommandError::Layout(LayoutError::InvalidInput))?;
            let derived = derive_text(raw, p.start, Some(s), Some(&derived_pages), rules, &mut budget, Some(cancel)).map_err(|e| match e {
                DerivedRefusal::BudgetExceeded => TocCommandError::BudgetExceeded,
                DerivedRefusal::Canceled => TocCommandError::Layout(LayoutError::Canceled),
            })?;
            let anchor = flags.make_anchor.then(|| -> Result<TocAnchorOutcome, TocCommandError> {
                let sid = s.story.story_id.as_str();
                if locked.contains(sid) {
                    Ok(TocAnchorOutcome::NotCreatedStoryLocked { story_id: sid.to_owned(), source_start: p.start })
                } else if let Some(id) = existing_at.get(&(sid, p.start)) {
                    Ok(TocAnchorOutcome::Existing((*id).to_owned()))
                } else {
                    let (anchor_id, name) =
                        fresh_anchor(taken_ids, taken_names, &mut next_anchor).ok_or(TocCommandError::BudgetExceeded)?;
                    Ok(TocAnchorOutcome::Created(StudioTextAnchor { anchor_id, name, story_id: sid.to_owned(), byte: p.start }))
                }
            });
            let anchor = anchor.transpose()?;
            flat.push((
                TocContractEntry {
                    level: e.level,
                    text: derived.text,
                    page_label: page.map(|x| x.label.clone()),
                    page_id: page.map(|x| x.page_id.clone()),
                    baseline_y: page.and_then(|_| baseline_of(s, p.start)),
                    story_id: s.story.story_id.clone(),
                    source_start: p.start,
                    anchor,
                    run_in: false,
                    marker_gaps: derived.gaps,
                },
                e.sort_alphabet,
                ei,
            ));
        }
    }
    // Level tree: an entry's children are the following deeper entries.
    let mut roots: Vec<Node> = Vec::new();
    let mut stack: Vec<Node> = Vec::new();
    for (entry, sort, style_index) in flat {
        let node = Node { entry, sort, style_index, children: Vec::new() };
        while stack.last().is_some_and(|top| top.entry.level >= node.entry.level) {
            let Some(done) = stack.pop() else { break };
            match stack.last_mut() {
                Some(parent) => parent.children.push(done),
                None => roots.push(done),
            }
        }
        stack.push(node);
    }
    while let Some(done) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.children.push(done),
            None => roots.push(done),
        }
    }
    sort_nodes(&mut roots);
    let mut ordered = Vec::new();
    flatten(roots, &mut ordered);
    // Write the story.
    let budget = |_| TocCommandError::BudgetExceeded;
    let mut w = RunWriter::new(req.limits.max_text_bytes);
    if !style.title.is_empty() {
        w.paragraph(style.title_style.as_ref().map(ParagraphStyleRef::as_str)).map_err(budget)?;
        w.run(&style.title, None).map_err(budget)?;
    }
    let deepest = style.entries.iter().map(|e| e.level).max();
    let mut wrote_entry = false;
    let mut entries = Vec::with_capacity(ordered.len());
    for (k, (mut entry, ei)) in ordered.into_iter().enumerate() {
        if k.is_multiple_of(256) {
            cancel.check().map_err(canceled)?;
        }
        let te = &style.entries[ei];
        let continues = flags.run_in && wrote_entry && Some(entry.level) == deepest;
        if continues {
            w.run(RUN_IN_JOINER, None).map_err(budget)?;
        } else {
            w.paragraph(te.format_style.as_ref().map(ParagraphStyleRef::as_str)).map_err(budget)?;
        }
        let sep_style = te.separator_style.as_ref().map(CharacterStyleRef::as_str);
        let num_style = te.page_number_style.as_ref().map(CharacterStyleRef::as_str);
        match (&entry.page_label, te.page_number_position) {
            (Some(label), TocPageNumberPosition::After) => {
                w.run(&entry.text, None).map_err(budget)?;
                w.run(&te.separator, sep_style).map_err(budget)?;
                w.run(label, num_style).map_err(budget)?;
            }
            (Some(label), TocPageNumberPosition::Before) => {
                w.run(label, num_style).map_err(budget)?;
                w.run(&te.separator, sep_style).map_err(budget)?;
                w.run(&entry.text, None).map_err(budget)?;
            }
            _ => w.run(&entry.text, None).map_err(budget)?,
        }
        entry.run_in = continues;
        wrote_entry = true;
        entries.push(entry);
    }
    let bookmarks = if flags.create_bookmarks {
        entries
            .iter()
            .map(|e| TocBookmark {
                level: e.level,
                title: e.text.clone(),
                page_id: e.page_id.clone(),
                baseline_y: e.baseline_y,
                anchor_id: e.anchor.as_ref().and_then(TocAnchorOutcome::anchor_id).map(str::to_owned),
            })
            .collect()
    } else {
        Vec::new()
    };
    let (kind, direction) = match replacing {
        Some(s) => (s.text_kind, s.story_direction),
        None => (StoryTextKind::Area, StoryDirection::LeftToRight),
    };
    Ok(TocGenerated {
        style_name: style.name.clone(),
        story: w.into_story(target_id, kind, direction),
        replaced: replacing.is_some(),
        place_point: if replacing.is_some() { None } else { req.place_point.cloned() },
        entries,
        bookmarks,
        story_direction: style.set_story_direction,
        numbered_paragraphs: style.numbered_paragraphs.clone(),
        omitted_overset,
        omitted_hidden,
        omitted_no_text: 0,
        omitted_marker_gaps: Vec::new(),
    })
}

/// One Observe terminal outcome for a TOC or index command result (codes only, no text), with the
/// command's correlation and base revision.
pub fn report_command<T, E>(
    result: &Result<T, E>,
    failure: impl Fn(&E) -> Option<FailureCode>,
    ctx: &CommandContext,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(e) => failure(e).map_or(Outcome::Canceled, Outcome::Failure),
    };
    observe.emit(
        Observation {
            correlation_id: ctx.correlation().value(),
            revision: ctx.base_revision(),
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}
