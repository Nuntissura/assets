//! Colour-book resource, per-book licensing posture, captured coverage and the named-colour
//! binding (lane SMB-COL-SWATCH-GROUPS-1; STU-COL-184..188).
//!
//! * `StudioColourBook` (`hsk.studio.colour_book@1`) is a DATA ADAPTER that populates native spot
//!   (or process) swatches (STU-COL-185): `book_id`, `title`, `name_prefix`, `name_postfix`,
//!   `description` (the rights holder's own copyright string, kept verbatim), `declared_count`,
//!   `color_space` lab|cmyk|rgb, `components_per_color`, `ink_type` spot|process, `page_size` and
//!   `page_selector_offset` (the fan-deck layout: page `n` holds entries `n * page_size ..`, and
//!   the offset names the entry of a page shown by the page selector, so it is below `page_size`;
//!   Studio reading, UNVERIFIED D5), and the entries (`name`, `code`, `components`). Components
//!   use the Studio colour-model ranges (`StudioColorModel::accepts`: Lab L* 0..100, a*/b*
//!   -128..128; CMYK and RGB 0..1). The displayed swatch name is `name_prefix + name +
//!   name_postfix`; empty prefix and postfix are legal. Entry codes are unique and non-empty (they
//!   are the lookup key).
//! * Licensing is PER BOOK (STU-COL-186): `license_state` bundled|operator_supplied|absent and
//!   `license_note` on every book. `parse_colour_book` loads bundled and operator-supplied books
//!   from the SAME resource format and refuses a book whose declared state does not match its
//!   origin (an operator file cannot declare itself bundled; an absent book is never loaded).
//!   `StudioColourBookLibrary::bundled_books` is the only set a build may ship. A document never
//!   embeds a book: `StudioVectorRecords.colour_book_refs` names a book (id, title, state) and
//!   `swatch_book_links` ties a swatch to a book entry; neither type has an entries member (both
//!   deny unknown fields), so a document is a reference, never a copy. A swatch populated from a
//!   book keeps its stored name and measured definition, so an absent book loses only further
//!   LOOK-UP (`StudioBookLookupError::BookAbsent`); validation and the stored colours do not depend
//!   on the library.
//! * `CAPTURED_BOOK_COVERAGE` records STU-COL-187 / STU-COL-103 coverage as planning data only
//!   (family, book, colour count, space, bundled at capture); no row carries a Studio decision and
//!   nothing reads it to load, enable or bundle a book. No book content is in this crate.
//! * STU-COL-188: a book supplied as an ICC named-colour profile is parsed by Prism (the
//!   STU-COL-126 profile owner, `hsk_studio_prism::parse_named_colour_profile`); the book records
//!   the profile as a typed `named_colour_profile` reference (`SCPF` id + sha256 of the bytes kept
//!   outside Folio) and `StudioColourBook::from_named_colours` builds the book from the parsed
//!   names and measured Lab values. Folio and Prism share no edge: the host joins the two.
//!
//! Hostile input (V4-01/02): integer arithmetic is denied; the byte size, the entry count (during
//! parsing, before the list grows past it) and the components per entry are bounded before
//! anything is stored. Vendor book-format parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, StudioColorModel, StudioSpotInk, StudioSwatch, StudioSwatchKind, StudioVectorRecords, SwatchSchema, check, key, sha256};
use crate::{ContentDigest, Result};
use hsk_studio_accord::{CancellationToken, CommandContext};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::SeqAccess, de::Visitor};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(ColourBookSchema, "hsk.studio.colour_book@1");

/// Bytes of one book resource.
pub const BOOK_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Entries of one book (the largest captured book holds 2,104).
pub const BOOK_MAX_ENTRIES: usize = 16_384;
/// Books in one library and book references in one document.
pub const BOOK_LIBRARY_MAX_BOOKS: usize = 256;
/// Bytes of a name, code, prefix, postfix or title.
pub const BOOK_TEXT_MAX: usize = 256;
/// Bytes of the description.
pub const BOOK_DESCRIPTION_MAX: usize = 4096;
/// Swatches one `add_colour_book_swatches` call populates.
pub const BOOK_PICKS_MAX: usize = super::TOOLS_MAX_ENTRIES;
const BUDGET_SENTINEL: &str = "hsk_colour_book_budget";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBookColorSpace {
    Lab,
    Cmyk,
    Rgb,
}
impl StudioBookColorSpace {
    pub const fn model(self) -> StudioColorModel {
        match self {
            Self::Lab => StudioColorModel::Lab,
            Self::Cmyk => StudioColorModel::Cmyk,
            Self::Rgb => StudioColorModel::Rgb,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBookInkType {
    Spot,
    Process,
}

/// STU-COL-186 per-book licensing posture: an operator decision recorded per book.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBookLicenseState {
    Bundled,
    OperatorSupplied,
    Absent,
}

/// Where a loaded book comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioBookOrigin {
    /// Shipped with the build (only books whose state is `bundled`).
    Bundled,
    /// A file the operator supplies at run time.
    OperatorSupplied,
}

/// STU-COL-188: the ICC named-colour profile a book was read from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioNamedColourProfileRef {
    /// `SCPF` id of the profile.
    pub profile_id: String,
    /// sha256 of the profile bytes (kept outside Folio).
    pub content_digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioColourBookEntry {
    pub name: String,
    pub code: String,
    pub components: Vec<f64>,
}

/// `StudioColourBook` (STU-COL-185, STU-COL-186).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioColourBook {
    pub schema_id: ColourBookSchema,
    /// Key id.
    pub book_id: String,
    pub title: String,
    pub name_prefix: String,
    pub name_postfix: String,
    pub description: String,
    pub declared_count: u32,
    pub color_space: StudioBookColorSpace,
    pub components_per_color: u32,
    pub ink_type: StudioBookInkType,
    pub page_size: u32,
    pub page_selector_offset: u32,
    pub license_state: StudioBookLicenseState,
    pub license_note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named_colour_profile: Option<StudioNamedColourProfileRef>,
    #[serde(deserialize_with = "bounded_entries")]
    pub entries: Vec<StudioColourBookEntry>,
}

/// A document's reference to a book (no entries: a document never embeds a book).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioColourBookRef {
    pub book_id: String,
    pub title: String,
    pub license_state: StudioBookLicenseState,
}

/// A document swatch populated from a book entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSwatchBookLink {
    pub swatch_id: String,
    pub book_id: String,
    pub entry_code: String,
}

/// Typed refusals of book parsing, loading and population.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioColourBookError {
    /// Bytes, entries, declared count or picks over their bound.
    BudgetExceeded,
    /// Not a book resource (JSON shape, unknown field, wrong schema id).
    Malformed,
    InvalidField,
    /// `components_per_color` is not the colour space's channel count, or an entry's component count differs.
    ArityMismatch,
    /// `declared_count` differs from the number of entries.
    CountMismatch,
    /// `page_size` is 0 or above the entry bound, or `page_selector_offset` is not below it.
    InvalidPageLayout,
    /// A component outside the colour model's range or not finite.
    InvalidComponents,
    DuplicateEntry,
    /// The declared `license_state` does not match the origin (or the book is `absent`).
    LicenseStateMismatch,
    /// An `absent` book carries entries.
    AbsentWithEntries,
    EntryMissing,
    /// A populated swatch id is not an `SSWT` id or already exists.
    InvalidSwatchId,
    CommandMissing,
    StaleRevision { expected: u64, current: u64 },
    Canceled,
}
impl StudioColourBookError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::BudgetExceeded => "budget_exceeded",
            Self::Malformed => "malformed",
            Self::InvalidField => "invalid_field",
            Self::ArityMismatch => "arity_mismatch",
            Self::CountMismatch => "count_mismatch",
            Self::InvalidPageLayout => "invalid_page_layout",
            Self::InvalidComponents => "invalid_components",
            Self::DuplicateEntry => "duplicate_entry",
            Self::LicenseStateMismatch => "license_state_mismatch",
            Self::AbsentWithEntries => "absent_with_entries",
            Self::EntryMissing => "entry_missing",
            Self::InvalidSwatchId => "invalid_swatch_id",
            Self::CommandMissing => "command_missing",
            Self::StaleRevision { .. } => "stale_revision",
            Self::Canceled => "canceled",
        }
    }
}
impl std::fmt::Display for StudioColourBookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for StudioColourBookError {}

/// Refusals of a library look-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioBookLookupError {
    /// The library has no record of the book.
    BookUnknown,
    /// The book is recorded as absent: look-up is unavailable, stored swatches are unaffected.
    BookAbsent,
    EntryMissing,
}
impl StudioBookLookupError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::BookUnknown => "book_unknown",
            Self::BookAbsent => "book_absent",
            Self::EntryMissing => "entry_missing",
        }
    }
}

fn bounded_entries<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<StudioColourBookEntry>, D::Error> {
    struct Bounded;
    impl<'de> Visitor<'de> for Bounded {
        type Value = Vec<StudioColourBookEntry>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a bounded list of colour-book entries")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error> {
            let mut out: Vec<StudioColourBookEntry> = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1024));
            while let Some(e) = seq.next_element::<StudioColourBookEntry>()? {
                if out.len() >= BOOK_MAX_ENTRIES || e.components.len() > 4 {
                    return Err(serde::de::Error::custom(BUDGET_SENTINEL));
                }
                out.push(e);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(Bounded)
}

fn text(s: &str, max: usize, empty_ok: bool) -> bool {
    (empty_ok || !s.is_empty()) && s.len() <= max && !s.chars().any(char::is_control)
}

impl StudioColourBook {
    /// The displayed swatch name of `entry` (STU-COL-185).
    pub fn display_name(&self, entry: &StudioColourBookEntry) -> String {
        let mut s = String::with_capacity(self.name_prefix.len().saturating_add(entry.name.len()).saturating_add(self.name_postfix.len()));
        s.push_str(&self.name_prefix);
        s.push_str(&entry.name);
        s.push_str(&self.name_postfix);
        s
    }
    /// The entry with code `code`.
    pub fn entry(&self, code: &str) -> Option<&StudioColourBookEntry> {
        self.entries.iter().find(|e| e.code == code)
    }
    /// Number of fan-deck pages.
    pub fn page_count(&self) -> usize {
        let size = usize::try_from(self.page_size).unwrap_or(usize::MAX).max(1);
        self.entries.len().div_ceil(size)
    }
    /// The entries of page `page` (empty past the last page).
    pub fn page(&self, page: usize) -> &[StudioColourBookEntry] {
        let size = usize::try_from(self.page_size).unwrap_or(usize::MAX).max(1);
        let start = page.saturating_mul(size).min(self.entries.len());
        let end = start.saturating_add(size).min(self.entries.len());
        &self.entries[start..end]
    }
    /// The entry the page selector shows for page `page` (`page_selector_offset` within the page).
    pub fn page_selector_entry(&self, page: usize) -> Option<&StudioColourBookEntry> {
        self.page(page).get(usize::try_from(self.page_selector_offset).ok()?)
    }

    /// STU-COL-185 / STU-COL-186 shape rules of a book resource.
    pub fn validate(&self, token: &CancellationToken) -> std::result::Result<(), StudioColourBookError> {
        use StudioColourBookError as E;
        let max = u32::try_from(BOOK_MAX_ENTRIES).unwrap_or(u32::MAX);
        if self.declared_count > max || self.entries.len() > BOOK_MAX_ENTRIES {
            return Err(E::BudgetExceeded);
        }
        if !key(&self.book_id) || !text(&self.title, BOOK_TEXT_MAX, false) || !text(&self.name_prefix, BOOK_TEXT_MAX, true) || !text(&self.name_postfix, BOOK_TEXT_MAX, true) {
            return Err(E::InvalidField);
        }
        if self.description.len() > BOOK_DESCRIPTION_MAX || self.description.chars().any(|c| c.is_control() && c != '\n') || !text(&self.license_note, BOOK_DESCRIPTION_MAX, true) {
            return Err(E::InvalidField);
        }
        if let Some(p) = &self.named_colour_profile {
            super::id(&p.profile_id, "SCPF", "named_colour_profile").map_err(|_| E::InvalidField)?;
            if !sha256(&p.content_digest) {
                return Err(E::InvalidField);
            }
        }
        let model = self.color_space.model();
        if usize::try_from(self.components_per_color).ok() != Some(model.channels()) {
            return Err(E::ArityMismatch);
        }
        if self.page_size == 0 || self.page_size > max || self.page_selector_offset >= self.page_size {
            return Err(E::InvalidPageLayout);
        }
        if self.license_state == StudioBookLicenseState::Absent {
            return if self.entries.is_empty() { Ok(()) } else { Err(E::AbsentWithEntries) };
        }
        if usize::try_from(self.declared_count).ok() != Some(self.entries.len()) {
            return Err(E::CountMismatch);
        }
        let mut codes: BTreeSet<&str> = BTreeSet::new();
        for (i, e) in self.entries.iter().enumerate() {
            if i.is_multiple_of(256) {
                token.check().map_err(|_| E::Canceled)?;
            }
            if !text(&e.name, BOOK_TEXT_MAX, false) || !text(&e.code, BOOK_TEXT_MAX, false) {
                return Err(E::InvalidField);
            }
            if e.components.len() != model.channels() {
                return Err(E::ArityMismatch);
            }
            if !model.accepts(&e.components) {
                return Err(E::InvalidComponents);
            }
            if !codes.insert(e.code.as_str()) {
                return Err(E::DuplicateEntry);
            }
        }
        Ok(())
    }

    /// STU-COL-188: a Lab spot book from the names and measured Lab values of an ICC named-colour
    /// profile (parsed by Prism). The root name is the entry's name and its code; the book is one
    /// flat page (ICC named-colour profiles carry no fan-deck layout). `license_state` is the
    /// operator's decision for this book (`bundled` or `operator_supplied`).
    #[allow(clippy::too_many_arguments)]
    pub fn from_named_colours(
        book_id: &str,
        title: &str,
        license_state: StudioBookLicenseState,
        license_note: &str,
        profile: StudioNamedColourProfileRef,
        prefix: &str,
        suffix: &str,
        colours: &[(String, [f64; 3])],
        token: &CancellationToken,
    ) -> std::result::Result<Self, StudioColourBookError> {
        if colours.len() > BOOK_MAX_ENTRIES {
            return Err(StudioColourBookError::BudgetExceeded);
        }
        let count = u32::try_from(colours.len()).map_err(|_| StudioColourBookError::BudgetExceeded)?;
        let book = Self {
            schema_id: ColourBookSchema::V1,
            book_id: book_id.into(),
            title: title.into(),
            name_prefix: prefix.into(),
            name_postfix: suffix.into(),
            description: String::new(),
            declared_count: count,
            color_space: StudioBookColorSpace::Lab,
            components_per_color: 3,
            ink_type: StudioBookInkType::Spot,
            page_size: count.max(1),
            page_selector_offset: 0,
            license_state,
            license_note: license_note.into(),
            named_colour_profile: Some(profile),
            entries: colours.iter().map(|(n, lab)| StudioColourBookEntry { name: n.clone(), code: n.clone(), components: lab.to_vec() }).collect(),
        };
        book.validate(token)?;
        Ok(book)
    }

    /// The native swatch an entry populates (STU-COL-185): a spot swatch named and inked with the
    /// displayed name, measured from a Lab book or with the book's CMYK/RGB values as its
    /// alternate; a process swatch for a process book.
    pub fn swatch_for(&self, entry: &StudioColourBookEntry, swatch_id: &str) -> StudioSwatch {
        let name = self.display_name(entry);
        let model = self.color_space.model();
        let (kind, color_model, components, spot) = match self.ink_type {
            StudioBookInkType::Process => (StudioSwatchKind::Process, Some(model), entry.components.clone(), None),
            StudioBookInkType::Spot => {
                let lab = match (self.color_space, entry.components.as_slice()) {
                    (StudioBookColorSpace::Lab, [l, a, b]) => Some([*l, *a, *b]),
                    _ => None,
                };
                let (alternate_space, alternate_color_value) = if lab.is_some() { (None, Vec::new()) } else { (Some(model), entry.components.clone()) };
                (StudioSwatchKind::Spot, None, Vec::new(), Some(StudioSpotInk { ink_name: name.clone(), measured_lab: lab, alternate_space, alternate_color_value }))
            }
        };
        StudioSwatch {
            schema_id: SwatchSchema::V1,
            swatch_id: swatch_id.into(),
            name,
            swatch_kind: kind,
            color_model,
            components,
            profile_ref: None,
            spot,
            base_swatch_ref: None,
            tint_value: None,
            gradient: None,
            group_ref: None,
        }
    }
}

/// Parses a book resource (STU-COL-185) from bytes. Bundled and operator-supplied books use this
/// same format; the declared `license_state` must match `origin` (STU-COL-186).
pub fn parse_colour_book(bytes: &[u8], origin: StudioBookOrigin, token: &CancellationToken) -> std::result::Result<StudioColourBook, StudioColourBookError> {
    token.check().map_err(|_| StudioColourBookError::Canceled)?;
    if bytes.len() > BOOK_MAX_BYTES {
        return Err(StudioColourBookError::BudgetExceeded);
    }
    let book: StudioColourBook = serde_json::from_slice(bytes).map_err(|e| if e.to_string().starts_with(BUDGET_SENTINEL) { StudioColourBookError::BudgetExceeded } else { StudioColourBookError::Malformed })?;
    let expected = match origin {
        StudioBookOrigin::Bundled => StudioBookLicenseState::Bundled,
        StudioBookOrigin::OperatorSupplied => StudioBookLicenseState::OperatorSupplied,
    };
    if book.license_state != expected {
        return Err(StudioColourBookError::LicenseStateMismatch);
    }
    book.validate(token)?;
    Ok(book)
}

#[derive(Clone, Debug, PartialEq)]
enum BookSlot {
    Loaded(Box<StudioColourBook>),
    Absent,
}

/// What a host emits through Observe for one library change or population: codes and counters only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StudioBookReport {
    pub operation: &'static str,
    pub correlation: u64,
    pub entries: usize,
    pub books: usize,
}

/// The host's colour books, one slot per book id: loaded or recorded absent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StudioColourBookLibrary {
    books: BTreeMap<String, BookSlot>,
}

impl StudioColourBookLibrary {
    pub fn new() -> Self {
        Self::default()
    }
    /// Loads a book; on any refusal the library (including a last-good copy of the same book) is unchanged.
    pub fn load(&mut self, ctx: &CommandContext, bytes: &[u8], origin: StudioBookOrigin, token: &CancellationToken) -> std::result::Result<StudioBookReport, StudioColourBookError> {
        let book = parse_colour_book(bytes, origin, token)?;
        if !self.books.contains_key(&book.book_id) && self.books.len() >= BOOK_LIBRARY_MAX_BOOKS {
            return Err(StudioColourBookError::BudgetExceeded);
        }
        let entries = book.entries.len();
        self.books.insert(book.book_id.clone(), BookSlot::Loaded(Box::new(book)));
        Ok(StudioBookReport { operation: "book.load", correlation: ctx.correlation().value(), entries, books: self.books.len() })
    }
    /// Records `book_id` as absent (removing any loaded copy): look-up becomes unavailable.
    pub fn mark_absent(&mut self, ctx: &CommandContext, book_id: &str) -> std::result::Result<StudioBookReport, StudioColourBookError> {
        if !key(book_id) {
            return Err(StudioColourBookError::InvalidField);
        }
        if !self.books.contains_key(book_id) && self.books.len() >= BOOK_LIBRARY_MAX_BOOKS {
            return Err(StudioColourBookError::BudgetExceeded);
        }
        self.books.insert(book_id.into(), BookSlot::Absent);
        Ok(StudioBookReport { operation: "book.mark_absent", correlation: ctx.correlation().value(), entries: 0, books: self.books.len() })
    }
    /// The recorded state of `book_id` (`None` = no record: an unresolved operator decision).
    pub fn state(&self, book_id: &str) -> Option<StudioBookLicenseState> {
        self.books.get(book_id).map(|s| match s {
            BookSlot::Loaded(b) => b.license_state,
            BookSlot::Absent => StudioBookLicenseState::Absent,
        })
    }
    pub fn book(&self, book_id: &str) -> Option<&StudioColourBook> {
        match self.books.get(book_id) {
            Some(BookSlot::Loaded(b)) => Some(b.as_ref()),
            _ => None,
        }
    }
    /// Looks up an entry of a loaded book.
    pub fn lookup(&self, book_id: &str, code: &str) -> std::result::Result<&StudioColourBookEntry, StudioBookLookupError> {
        match self.books.get(book_id) {
            None => Err(StudioBookLookupError::BookUnknown),
            Some(BookSlot::Absent) => Err(StudioBookLookupError::BookAbsent),
            Some(BookSlot::Loaded(b)) => b.entry(code).ok_or(StudioBookLookupError::EntryMissing),
        }
    }
    /// The books a build may ship: loaded books whose state is `bundled` (STU-COL-186).
    pub fn bundled_books(&self) -> impl Iterator<Item = &StudioColourBook> {
        self.books.values().filter_map(|s| match s {
            BookSlot::Loaded(b) if b.license_state == StudioBookLicenseState::Bundled => Some(b.as_ref()),
            _ => None,
        })
    }
}

/// Stable command id of `add_colour_book_swatches`.
pub const BOOK_ADD_SWATCHES_COMMAND_ID: &str = "swatch.book.add";

/// Result of `add_colour_book_swatches`.
#[derive(Clone, Debug, PartialEq)]
pub struct StudioBookSwatchOutcome {
    pub records: StudioVectorRecords,
    pub report: StudioBookReport,
}

/// Populates document swatches from `book` (a loaded book): one swatch per `(entry code, new
/// SSWT swatch id)` pick, a `swatch_book_links` row per swatch and the book's reference. Attributed
/// (the context's command id must be `swatch.book.add` and its base revision `revision`) and
/// atomic: a refusal leaves `records` as it was.
pub fn add_colour_book_swatches(
    records: &StudioVectorRecords,
    ctx: &CommandContext,
    revision: u64,
    book: &StudioColourBook,
    picks: &[(&str, &str)],
    token: &CancellationToken,
) -> std::result::Result<StudioBookSwatchOutcome, StudioColourBookError> {
    use StudioColourBookError as E;
    match ctx.command() {
        Some(c) if c.as_str() == BOOK_ADD_SWATCHES_COMMAND_ID => {}
        _ => return Err(E::CommandMissing),
    }
    if ctx.base_revision() != revision {
        return Err(E::StaleRevision { expected: ctx.base_revision(), current: revision });
    }
    if book.license_state == StudioBookLicenseState::Absent {
        return Err(E::LicenseStateMismatch);
    }
    book.validate(token)?;
    let total = records.swatches.len().checked_add(picks.len()).ok_or(E::BudgetExceeded)?;
    if picks.len() > BOOK_PICKS_MAX || total > super::TOOLS_MAX_ENTRIES {
        return Err(E::BudgetExceeded);
    }
    let mut out = records.clone();
    let mut taken: BTreeSet<&str> = records.swatches.iter().map(|s| s.swatch_id.as_str()).collect();
    for (code, swatch_id) in picks {
        token.check().map_err(|_| E::Canceled)?;
        let entry = book.entry(code).ok_or(E::EntryMissing)?;
        super::id(swatch_id, "SSWT", "swatch_id").map_err(|_| E::InvalidSwatchId)?;
        if !taken.insert(swatch_id) {
            return Err(E::InvalidSwatchId);
        }
        let s = book.swatch_for(entry, swatch_id);
        super::tools::validate_swatch(&s, swatch_id).map_err(|_| E::InvalidField)?;
        out.swatches.push(s);
        out.swatch_book_links.push(StudioSwatchBookLink { swatch_id: (*swatch_id).into(), book_id: book.book_id.clone(), entry_code: entry.code.clone() });
    }
    if !out.colour_book_refs.iter().any(|r| r.book_id == book.book_id) {
        if out.colour_book_refs.len() >= BOOK_LIBRARY_MAX_BOOKS {
            return Err(E::BudgetExceeded);
        }
        out.colour_book_refs.push(StudioColourBookRef { book_id: book.book_id.clone(), title: book.title.clone(), license_state: book.license_state });
    }
    let report = StudioBookReport { operation: BOOK_ADD_SWATCHES_COMMAND_ID, correlation: ctx.correlation().value(), entries: picks.len(), books: out.colour_book_refs.len() };
    Ok(StudioBookSwatchOutcome { records: out, report })
}

/// The document-side book rules inside `validate_vector_records`: references and links.
pub(super) fn check_book_links(r: &StudioVectorRecords, swatches: &BTreeMap<&str, &StudioSwatch>) -> Result<()> {
    check(r.colour_book_refs.len() <= BOOK_LIBRARY_MAX_BOOKS, Code::BudgetExceeded, "colour_book_refs", "count")?;
    check(r.swatch_book_links.len() <= super::TOOLS_MAX_ENTRIES, Code::BudgetExceeded, "swatch_book_links", "count")?;
    let mut books: BTreeSet<&str> = BTreeSet::new();
    for b in &r.colour_book_refs {
        check(key(&b.book_id), Code::InvalidId, "colour_book_refs", "book_id")?;
        check(text(&b.title, BOOK_TEXT_MAX, false), Code::InvalidField, &b.book_id, "title")?;
        check(books.insert(b.book_id.as_str()), Code::DuplicateIdentity, &b.book_id, "book_id")?;
    }
    let mut linked: BTreeSet<&str> = BTreeSet::new();
    for l in &r.swatch_book_links {
        let t = l.swatch_id.as_str();
        let s = swatches.get(t).ok_or_else(|| super::err(Code::DanglingReference, format!("swatch_book_links/{t}")))?;
        check(matches!(s.swatch_kind, StudioSwatchKind::Spot | StudioSwatchKind::Process), Code::InvalidField, t, "swatch_book_links/swatch_id")?;
        check(books.contains(l.book_id.as_str()), Code::DanglingReference, t, "swatch_book_links/book_id")?;
        check(text(&l.entry_code, BOOK_TEXT_MAX, false), Code::InvalidField, t, "swatch_book_links/entry_code")?;
        check(linked.insert(t), Code::DuplicateIdentity, t, "swatch_book_links/swatch_id")?;
    }
    Ok(())
}

/// One captured colour book (STU-COL-103, STU-COL-187): planning data, not a Studio decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StudioCapturedBook {
    pub family: &'static str,
    pub book: &'static str,
    /// Colours in the captured container (`None` when the family was not captured).
    pub colours: Option<u32>,
    pub color_space: Option<StudioBookColorSpace>,
    /// Whether the captured raster application bundled it.
    pub bundled_at_capture: bool,
}

/// STU-COL-187 / STU-COL-103 coverage of the captured raster application (twelve books, 5,243
/// colours, PANTONE not bundled). Evidence of a vendor's licence position at capture time, NOT a
/// Studio bundling instruction: nothing reads it to load, enable or bundle a book, and every
/// Studio decision stays the operator's per-book `license_state`.
pub const CAPTURED_BOOK_COVERAGE: [StudioCapturedBook; 13] = [
    StudioCapturedBook { family: "ANPA", book: "ANPA Color", colours: Some(300), color_space: Some(StudioBookColorSpace::Lab), bundled_at_capture: true },
    StudioCapturedBook { family: "DIC", book: "DIC Color Guide", colours: Some(1280), color_space: Some(StudioBookColorSpace::Lab), bundled_at_capture: true },
    StudioCapturedBook { family: "FOCOLTONE", book: "FOCOLTONE", colours: Some(860), color_space: Some(StudioBookColorSpace::Cmyk), bundled_at_capture: true },
    StudioCapturedBook { family: "HKS", book: "HKS E", colours: Some(98), color_space: Some(StudioBookColorSpace::Lab), bundled_at_capture: true },
    StudioCapturedBook { family: "HKS", book: "HKS E Process", colours: Some(86), color_space: Some(StudioBookColorSpace::Cmyk), bundled_at_capture: true },
    StudioCapturedBook { family: "HKS", book: "HKS K", colours: Some(100), color_space: Some(StudioBookColorSpace::Lab), bundled_at_capture: true },
    StudioCapturedBook { family: "HKS", book: "HKS K Process", colours: Some(86), color_space: Some(StudioBookColorSpace::Cmyk), bundled_at_capture: true },
    StudioCapturedBook { family: "HKS", book: "HKS N", colours: Some(98), color_space: Some(StudioBookColorSpace::Lab), bundled_at_capture: true },
    StudioCapturedBook { family: "HKS", book: "HKS N Process", colours: Some(86), color_space: Some(StudioBookColorSpace::Cmyk), bundled_at_capture: true },
    StudioCapturedBook { family: "HKS", book: "HKS Z", colours: Some(59), color_space: Some(StudioBookColorSpace::Lab), bundled_at_capture: true },
    StudioCapturedBook { family: "HKS", book: "HKS Z Process", colours: Some(86), color_space: Some(StudioBookColorSpace::Cmyk), bundled_at_capture: true },
    StudioCapturedBook { family: "TRUMATCH", book: "TRUMATCH", colours: Some(2104), color_space: Some(StudioBookColorSpace::Cmyk), bundled_at_capture: true },
    StudioCapturedBook { family: "PANTONE", book: "PANTONE", colours: None, color_space: None, bundled_at_capture: false },
];
