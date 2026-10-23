//! Long-document records (`hsk.studio.longdoc_records@1`), added by SMB-LAYOUT-LONGDOC: tables
//! (STU-LAY-035, STU-LAY-139..141). Like the parent-page records this set is stored beside the
//! document (`StudioDocument` gains no fields), and every member list is `serde(default)` and not
//! encoded when empty, so documents and record sets written before a member existed stay
//! byte-identical. Folio stores and validates only; composition belongs to hsk-studio-layout.
//!
//! Tables: a table flows through the frame thread of its host story (`story_id`) from the host
//! story byte `anchor`; its cell text is one `StudioTextStory` (`cell_story_id`) and each cell names
//! a byte range of it. Rows are typed `header`, `body` or `footer` (STU-LAY-139 `row_type`; header
//! rows come first and footer rows last), carry `minimum_height` / `maximum_height` (STU-LAY-141 at
//! least / exactly), `keep_with_next_row` and `start_row`. `allow_split` (a Studio extension, not in
//! the captured cell contract; InDesign parity UNVERIFIED, D5) lets layout break one tall body row
//! across frames at a line boundary. Cells carry `row_span` / `column_span`, the four text insets,
//! a fill colour and tint (0..100 percent, STU-LAY-140), four edge strokes with their tints and the
//! four edge-stroke priorities (STU-LAY-141) and a vertical justification.
//!
//! Declared gaps, not invented here: graphic cells, nested tables, diagonal lines, gradient fills,
//! alternating patterns, table and cell styles, rotation and the second inset group (STU-LAY-141).
//!
//! Graphic frames (STU-LAY-028/029, STU-LAY-131): per raster layer the frame (page, pt bounds,
//! rectangle / inscribed ellipse / `SVPT` path shape), the content size at 100 %, the applied fit
//! (one union enumeration `StudioFit`; absent = cleared fitting), the stored `frame_fitting` rule,
//! the nine-point anchor and the crop amounts. Declared gaps: format-aware place options (layer
//! comps, PDF crop boxes, page selection), the linked-resource record (STU-LAY-030/132) and
//! non-raster content.
//!
//! Sections (STU-LAY-014, STU-LAY-108): a section starts on a page and numbers the pages up to the
//! next section start (document page order). `continue_numbering` and `page_number_start` are
//! order-dependent and mutually exclusive (the start is refused while numbering continues); the
//! prefix is at most FIVE characters without `,` or `+`, unique across sections, and is included
//! in page labels only when `include_section_prefix`. Two sections never start on the same page and
//! the first document page always starts a section once any section exists (the default section
//! cannot be deleted). `StudioNumberStyle` is ONE numbering enumeration (the union the spec
//! requires) with per-consumer legality (`legal_for_pages`, `legal_for_footnotes`).
//!
//! Tables of contents (STU-LAY-044, STU-LAY-148): style-driven TOC styles with a title, a title
//! style, the include-overset flag and ordered entries mapping a source paragraph style to a level,
//! a format style, a page-number position and a separator; at least one entry, a paragraph style
//! mapped at most once. Multiple TOC styles per document are carried. Declared gaps: run-in,
//! bookmarks, anchors, book documents, hidden-layer text, numbered-paragraph modes.
//!
//! Footnotes (STU-LAY-046, STU-LAY-150): a footnote names its host story and the host byte its
//! reference sits at, and the story holding the note text (never the host, never the host of
//! another note: footnotes in footnotes are refused as `RenderCycle`). Footnote options carry the
//! numbering style (footnote-legal members), start 1..=400, restart scope, prefix and suffix (0..=100
//! characters), the space between notes and the spacer above the first note, the first and the
//! continuing rule (weight 0..=1000 pt, tint 0..=100, colour, left indent, width, offset),
//! end-of-story placement and no-splitting. Declared gaps: endnotes, sidenotes, straddling, marker
//! positioning styles, the paragraph-style references.
//!
//! Index (STU-LAY-045, STU-LAY-149): index marks name a story byte and a topic path of one to FOUR
//! levels (the captured maximum; Studio declares four), optional per-level sort-by text and an
//! optional cross reference (eight captured kinds, custom text for the custom kinds). Index options
//! carry the title and styles, nested or run-in format, section headings (with empty sections) and
//! the six independent separators. Declared gaps: page-range scopes other than the current page,
//! capitalisation commands, book documents, hidden entries.
//!
//! Text variables and running text (STU-LAY-015, STU-LAY-016, STU-LAY-048; lane SMB-LAYOUT-LONGDOC-2):
//! the twelve captured variable kinds are carried (`StudioVariableKind`); running texts name the
//! text-frame layers that show them (one per page, e.g. resolved parent items) and a sequence of
//! literal text, variables, page-number and section-marker parts. Dates are resolved by the caller
//! (layout has no clock); cross-reference and live-caption variables are stored but not resolved.
//!
//! Endnotes (STU-LAY-150 endnote fields): references like footnotes, with title, prefix/suffix
//! (0..=100 characters), numbering, restart (continuous or per story) and scope. Sidenotes: the
//! spec names them and declares their option contract a SPEC GAP; the provisional Studio options
//! here (width, gap, inside/outside the margin, anchor to the reference line or the frame top,
//! restart) are labelled provisional and may be replaced by the spec's contract.
//!
//! Hostile wire JSON: integer arithmetic is denied (workflow V4-01); grids are checked by
//! `checked_mul` against `MAX_TABLE_CELLS` before any occupancy grid is allocated, and spans by
//! `checked_add` against the grid edge (span overflow is refused, never clamped).
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Budget, Code, GeometryUnit, LayoutEdges, Length, ResolutionIssue, Resolver, Result, StudioColor,
    StudioDocument, VerticalJustification, canceled, err, finite_length, id,
    values::{Ctx, label, percent},
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(LongDocRecordsSchema, "hsk.studio.longdoc_records@1");

/// Tables per record set.
pub const MAX_TABLES: usize = 4096;
/// Graphic frames per record set.
pub const MAX_GRAPHIC_FRAMES: usize = 65_536;
/// Rows of one table.
pub const MAX_TABLE_ROWS: usize = 10_000;
/// Columns of one table.
pub const MAX_TABLE_COLUMNS: usize = 1_000;
/// Grid slots (rows x columns) of one table; checked before any grid is allocated.
pub const MAX_TABLE_CELLS: usize = 1_000_000;

fn is_false(b: &bool) -> bool {
    !*b
}
fn is_one(v: &u32) -> bool {
    *v == 1
}
fn one() -> u32 {
    1
}
fn is_zero(v: &i32) -> bool {
    *v == 0
}

/// STU-LAY-139 `row_type` (the derived `mixed_state` is not authored).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TableRowType {
    Header,
    Body,
    Footer,
}

/// STU-LAY-139 `break_headers` / `break_footers`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TableHeaderBreak {
    InAllTextColumns,
    #[default]
    OncePerTextFrame,
    OncePerPage,
}

/// STU-LAY-139 `stroke_order`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TableStrokeOrder {
    #[default]
    RowOnTop,
    ColumnOnTop,
    BestJoins,
    LegacyCompatibility,
}

/// STU-LAY-141 `start_row`: where the row may start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TableStartRow {
    #[default]
    Anywhere,
    NextFrame,
    NextPage,
    NextOddPage,
    NextEvenPage,
}
impl TableStartRow {
    fn is_anywhere(&self) -> bool {
        *self == Self::Anywhere
    }
}

/// One row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTableRow {
    pub row_type: TableRowType,
    /// `pt`, non-negative ("at least").
    pub minimum_height: Length,
    /// `pt`, at least `minimum_height` ("exactly" when equal).
    pub maximum_height: Length,
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_with_next_row: bool,
    #[serde(default, skip_serializing_if = "TableStartRow::is_anywhere")]
    pub start_row: TableStartRow,
    /// Studio extension (see module docs): a body row taller than a frame may split at a line.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_split: bool,
}

/// One edge stroke of a cell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCellStroke {
    /// `pt`, non-negative.
    pub weight: Length,
    pub colour: StudioColor,
    /// Percent 0..=100 (STU-LAY-140).
    pub tint: f64,
}

/// The four edge strokes of a cell; an absent edge draws nothing from this cell.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCellEdges {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<StudioCellStroke>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<StudioCellStroke>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<StudioCellStroke>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<StudioCellStroke>,
}
impl StudioCellEdges {
    fn is_empty(&self) -> bool {
        self.top.is_none() && self.left.is_none() && self.bottom.is_none() && self.right.is_none()
    }
    /// Top, left, bottom, right.
    pub fn each(&self) -> [Option<&StudioCellStroke>; 4] {
        [self.top.as_ref(), self.left.as_ref(), self.bottom.as_ref(), self.right.as_ref()]
    }
}

/// STU-LAY-141 edge-stroke priorities (`top_edge_stroke_priority` and siblings).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEdgePriorities {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub top: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub left: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bottom: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub right: i32,
}
impl StudioEdgePriorities {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Half-open byte range of the table's cell story.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioByteRange {
    pub start: u64,
    pub end: u64,
}

/// One cell: the top-left grid slot of its span.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTableCell {
    pub row: u32,
    pub column: u32,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub row_span: u32,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub column_span: u32,
    /// Byte range of the cell story; absent = an empty cell. Must start and end on paragraph
    /// boundaries of the cell story (layout checks against the shaped paragraphs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<StudioByteRange>,
    /// STU-LAY-141 text insets, `pt`, non-negative.
    pub insets: LayoutEdges,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<StudioColor>,
    /// Percent 0..=100; absent = 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_tint: Option<f64>,
    #[serde(default, skip_serializing_if = "StudioCellEdges::is_empty")]
    pub strokes: StudioCellEdges,
    #[serde(default, skip_serializing_if = "StudioEdgePriorities::is_default")]
    pub stroke_priorities: StudioEdgePriorities,
    pub vertical_justification: VerticalJustification,
}

/// One table (STU-LAY-035/139).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTable {
    /// Document-local key, unique in the set.
    pub table_id: String,
    /// `STXT` host story; the table flows through its frame thread.
    pub story_id: String,
    /// Host story byte the table is anchored at.
    pub anchor: u64,
    /// `STXT` story holding the cell text; required when any cell has content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_story_id: Option<String>,
    /// `pt`, each greater than zero.
    pub column_widths: Vec<Length>,
    pub rows: Vec<StudioTableRow>,
    pub cells: Vec<StudioTableCell>,
    #[serde(default)]
    pub break_headers: TableHeaderBreak,
    #[serde(default)]
    pub break_footers: TableHeaderBreak,
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_first_header: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_last_footer: bool,
    #[serde(default)]
    pub stroke_order: TableStrokeOrder,
}

/// STU-LAY-131 fitting: ONE union of the frame fitting enumeration (seven members) and the
/// data-merge fitting enumeration (`preserve_sizes`, `honour_existing_style`; the other data-merge
/// members are the same operations under other names). `honour_existing_style` is legal only for
/// data merge and is refused on a graphic frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioFit {
    ContentToFrame,
    CenterContent,
    Proportionally,
    ContentAware,
    FrameToContent,
    FillProportionally,
    ApplyFrameFittingOptions,
    PreserveSizes,
    HonourExistingStyle,
}

/// STU-LAY-029 anchor point (reference point) of placed content: nine positions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioFitAnchor {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    #[default]
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}
impl StudioFitAnchor {
    fn is_center(&self) -> bool {
        *self == Self::Center
    }
    /// Horizontal and vertical fraction (0 = left/top, 0.5 = centre, 1 = right/bottom).
    pub fn fractions(self) -> (f64, f64) {
        match self {
            Self::TopLeft => (0.0, 0.0),
            Self::TopCenter => (0.5, 0.0),
            Self::TopRight => (1.0, 0.0),
            Self::CenterLeft => (0.0, 0.5),
            Self::Center => (0.5, 0.5),
            Self::CenterRight => (1.0, 0.5),
            Self::BottomLeft => (0.0, 1.0),
            Self::BottomCenter => (0.5, 1.0),
            Self::BottomRight => (1.0, 1.0),
        }
    }
}

/// Picture-frame shape (STU-LAY-028: rectangle, ellipse or any vector path).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioFrameShape {
    #[default]
    Rectangle,
    /// The ellipse inscribed in the frame bounds.
    Ellipse,
    /// An `SVPT` path (spread coordinates, resolved by the caller) clips the content.
    Path { path_id: String },
}
impl StudioFrameShape {
    fn is_rectangle(&self) -> bool {
        *self == Self::Rectangle
    }
}

/// A graphic frame (STU-LAY-028/029): the frame geometry of a placed-asset layer, the placed
/// content's size at 100 % and how it is fitted. The content itself is the layer's payload (raster
/// tiles); nothing is duplicated here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGraphicFrame {
    /// `SLYR` id of a document layer with a raster payload.
    pub layer_id: String,
    /// Page key the frame sits on; page coordinates for `bounds`.
    pub page_id: String,
    /// `pt`, page coordinates, width and height greater than zero.
    pub bounds: super::Bounds,
    #[serde(default, skip_serializing_if = "StudioFrameShape::is_rectangle")]
    pub shape: StudioFrameShape,
    /// Placed content size at 100 %, `pt`, greater than zero.
    pub content_width: Length,
    pub content_height: Length,
    /// The applied fitting; absent = cleared fitting (content at 100 % at the frame's top-left).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<StudioFit>,
    /// The frame's stored content-fit rule (STU-LAY-029), re-applied by `apply_frame_fitting_options`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_fitting: Option<StudioFit>,
    #[serde(default, skip_serializing_if = "StudioFitAnchor::is_center")]
    pub anchor: StudioFitAnchor,
    /// Crop amounts of the fitting options, `pt` (positive trims the content, negative adds space).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<LayoutEdges>,
}

/// ONE numbering-style enumeration, the union of the captured page-number (14 members), list,
/// footnote and text-variable enumerations (STU-LAY-108, STU-LAY-150); legality is per consumer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioNumberStyle {
    UpperRoman,
    LowerRoman,
    UpperLetters,
    LowerLetters,
    Arabic,
    Kanji,
    ArabicAlifBaTah,
    ArabicAbjad,
    HebrewBiblical,
    HebrewNonStandard,
    SingleLeadingZeros,
    DoubleLeadingZeros,
    TripleLeadingZeros,
    FullWidthArabic,
    KatakanaModern,
    KatakanaTraditional,
    FormatNone,
    Symbols,
    Asterisks,
    Current,
}
impl StudioNumberStyle {
    /// The fourteen page-number members (STU-LAY-108).
    pub fn legal_for_pages(self) -> bool {
        !matches!(
            self,
            Self::KatakanaModern
                | Self::KatakanaTraditional
                | Self::FormatNone
                | Self::Symbols
                | Self::Asterisks
                | Self::Current
        )
    }
    /// The fifteen footnote members (STU-LAY-150).
    pub fn legal_for_footnotes(self) -> bool {
        !matches!(
            self,
            Self::TripleLeadingZeros
                | Self::KatakanaModern
                | Self::KatakanaTraditional
                | Self::FormatNone
                | Self::Current
        )
    }
}

fn is_true(b: &bool) -> bool {
    *b
}
fn yes() -> bool {
    true
}

/// One section (STU-LAY-108).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSection {
    /// Document-local key, unique in the set.
    pub section_id: String,
    /// Non-empty.
    pub name: String,
    /// Page key of the section's first page.
    pub page_start: String,
    /// Absent = true (numbering continues from the previous section).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub continue_numbering: bool,
    /// 1..=999999; only with `continue_numbering` false (absent there = 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_number_start: Option<u32>,
    pub page_number_style: StudioNumberStyle,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_section_prefix: bool,
    /// At most five characters, no `,` or `+`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_prefix: Option<String>,
    /// Section marker text (resolved by a section-marker variable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
}

/// STU-LAY-148 page-number position of a TOC entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TocPageNumberPosition {
    #[default]
    After,
    Before,
    None,
}
impl TocPageNumberPosition {
    fn is_after(&self) -> bool {
        *self == Self::After
    }
}

/// One TOC entry (STU-LAY-148): the source paragraph style it maps and how the entry is written.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTocEntry {
    /// `STYS` source paragraph style.
    pub paragraph_style: String,
    /// `STYS` style of the generated entry paragraph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_style: Option<String>,
    /// Indent level, 1..=MAX_TOC_LEVEL.
    pub level: u32,
    #[serde(default, skip_serializing_if = "TocPageNumberPosition::is_after")]
    pub page_number_position: TocPageNumberPosition,
    /// Between entry text and page number, at most 100 characters.
    pub separator: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub sort_alphabet: bool,
}

/// One TOC style (STU-LAY-148).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTocStyle {
    /// Document-local key, unique in the set.
    pub toc_id: String,
    pub name: String,
    /// Generated heading text; empty = no title paragraph.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_style: Option<String>,
    /// STU-LAY-148 include-overset flag of the generate command.
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_overset: bool,
    pub entries: Vec<StudioTocEntry>,
}

/// Deepest TOC level.
pub const MAX_TOC_LEVEL: u32 = 9;
/// Entries of one TOC style.
pub const MAX_TOC_ENTRIES: usize = 256;
/// Sections per record set.
pub const MAX_SECTIONS: usize = 9999;

/// The section rules of STU-LAY-108 against `d` (module docs); the stable code of the first
/// violation. Shared by the Folio validator and layout numbering.
pub fn check_sections(sections: &[StudioSection], d: &StudioDocument) -> std::result::Result<(), &'static str> {
    if sections.len() > MAX_SECTIONS {
        return Err("too_many_sections");
    }
    let mut prefixes = BTreeSet::new();
    let mut starts = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for sec in sections {
        if !label(&sec.section_id) {
            return Err("section_id");
        }
        if !keys.insert(sec.section_id.as_str()) {
            return Err("section_duplicate");
        }
        if sec.name.trim().is_empty() || !label(&sec.name) {
            return Err("section_name_empty");
        }
        if d.locate_page(&sec.page_start).is_none() {
            return Err("section_page_missing");
        }
        if !starts.insert(sec.page_start.as_str()) {
            return Err("sections_share_start_page");
        }
        if !sec.page_number_style.legal_for_pages() {
            return Err("number_style_not_legal_for_pages");
        }
        match (sec.continue_numbering, sec.page_number_start) {
            (true, Some(_)) => return Err("page_number_start_while_continuing"),
            (false, Some(n)) if !(1..=999_999).contains(&n) => return Err("page_number_start_range"),
            _ => {}
        }
        if let Some(p) = &sec.section_prefix {
            if p.chars().count() > 5 {
                return Err("section_prefix_too_long");
            }
            if p.contains(',') || p.contains('+') || p.chars().any(char::is_control) {
                return Err("section_prefix_character");
            }
            if !p.is_empty() && !prefixes.insert(p.as_str()) {
                return Err("section_prefix_duplicate");
            }
        }
        if sec.marker.as_deref().is_some_and(|m| m.len() > 1024 || m.chars().any(char::is_control)) {
            return Err("section_marker");
        }
    }
    if !sections.is_empty() {
        let first = d.page_spreads.iter().flat_map(|s| s.pages.first()).next();
        if first.is_some_and(|p| !starts.contains(p.page_id.as_str())) {
            return Err("first_page_without_section");
        }
    }
    Ok(())
}

/// The TOC-style rules of STU-LAY-148: at least one entry, a paragraph style mapped once, levels
/// 1..=MAX_TOC_LEVEL, separators of at most 100 characters, `STYS` references.
pub fn check_toc_style(t: &StudioTocStyle) -> std::result::Result<(), &'static str> {
    if !label(&t.toc_id) {
        return Err("toc_id");
    }
    if t.entries.is_empty() {
        return Err("toc_no_entries");
    }
    if t.entries.len() > MAX_TOC_ENTRIES {
        return Err("toc_too_many_entries");
    }
    if t.title.len() > 1024 || t.title.chars().any(char::is_control) {
        return Err("toc_title");
    }
    let style = |s: &str| id(s, "STYS", "toc").is_ok();
    if t.title_style.as_deref().is_some_and(|s| !style(s)) {
        return Err("toc_style_ref");
    }
    let mut mapped = BTreeSet::new();
    for e in &t.entries {
        if !style(&e.paragraph_style) || e.format_style.as_deref().is_some_and(|s| !style(s)) {
            return Err("toc_style_ref");
        }
        if !mapped.insert(e.paragraph_style.as_str()) {
            return Err("toc_style_mapped_twice");
        }
        if !(1..=MAX_TOC_LEVEL).contains(&e.level) {
            return Err("toc_level");
        }
        if e.separator.chars().count() > 100 || e.separator.contains('\n') {
            return Err("toc_separator");
        }
    }
    Ok(())
}

/// STU-LAY-150 `footnote_restart_numbering`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FootnoteRestart {
    #[default]
    DontRestart,
    Page,
    Spread,
    Section,
}

/// One footnote: reference position and note text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFootnote {
    /// Document-local key, unique in the set.
    pub note_id: String,
    /// `STXT` host story.
    pub story_id: String,
    /// Host story byte the reference sits at.
    pub anchor: u64,
    /// `STXT` story holding the note text.
    pub note_story_id: String,
}

/// A footnote separator rule (STU-LAY-150 rule groups).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFootnoteRule {
    /// `pt`, 0..=1000.
    pub weight: Length,
    pub colour: StudioColor,
    /// Percent 0..=100.
    pub tint: f64,
    /// `pt` from the column's left inset.
    pub left_indent: Length,
    /// `pt`, the rule length.
    pub width: Length,
    /// `pt` from the top of the note area up to the rule.
    pub offset: Length,
}

/// STU-LAY-150 footnote options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFootnoteOptions {
    pub numbering_style: StudioNumberStyle,
    /// 1..=400.
    pub start_at: u32,
    #[serde(default)]
    pub restart: FootnoteRestart,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suffix: String,
    /// Vertical space between notes, `pt`, non-negative.
    pub space_between: Length,
    /// Minimum space between the text and the first note, `pt`, non-negative.
    pub spacer: Length,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<StudioFootnoteRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuing_rule: Option<StudioFootnoteRule>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub eos_placement: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_splitting: bool,
}

/// STU-LAY-149 cross-reference kinds of an index entry (eight captured members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexXrefKind {
    SeeOrAlsoBracket,
    See,
    SeeAlso,
    SeeHerein,
    SeeAlsoHerein,
    Custom,
    CustomBefore,
    CustomAfter,
}

/// An index cross reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioIndexXref {
    pub kind: IndexXrefKind,
    /// Topic path of the referenced topic (1..=4 levels).
    pub target: Vec<String>,
    /// Text of the custom kinds (required there, refused elsewhere).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_text: Option<String>,
}

/// One index mark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioIndexMark {
    /// Document-local key, unique in the set.
    pub mark_id: String,
    /// `STXT` story holding the mark.
    pub story_id: String,
    /// Story byte of the mark.
    pub at: u64,
    /// Topic path, 1..=4 levels, each non-empty.
    pub topics: Vec<String>,
    /// Per-level sort-by text (same length as `topics` when present; empty = the topic text).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sort_by: Vec<String>,
    /// A cross reference instead of (or beside) the page number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xref: Option<StudioIndexXref>,
}

/// STU-LAY-149 `index_format`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexFormat {
    #[default]
    Nested,
    RunIn,
}

/// STU-LAY-149 index options with the six independent separators.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioIndexOptions {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_style: Option<String>,
    #[serde(default)]
    pub index_format: IndexFormat,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_section_headings: bool,
    /// Valid only with section headings.
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_empty_index_sections: bool,
    /// `STYS` paragraph styles of levels 1..=4 (absent = none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub level_styles: Vec<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_heading_style: Option<String>,
    pub following_topic_separator: String,
    pub between_entries_separator: String,
    pub page_range_separator: String,
    pub between_page_numbers_separator: String,
    pub before_cross_reference_separator: String,
    pub entry_end_separator: String,
}

/// Index topic levels (the captured model declares exactly four).
pub const MAX_INDEX_LEVELS: usize = 4;
/// Footnotes per record set.
pub const MAX_FOOTNOTES: usize = 65_536;
/// Index marks per record set.
pub const MAX_INDEX_MARKS: usize = 262_144;

/// Footnote rules: unique keys, `STXT` references, a note story is neither its host nor the host
/// of another note (`footnote_in_footnote`) and holds one note only.
pub fn check_footnotes(notes: &[StudioFootnote]) -> std::result::Result<(), &'static str> {
    if notes.len() > MAX_FOOTNOTES {
        return Err("too_many_footnotes");
    }
    let hosts: BTreeSet<&str> = notes.iter().map(|n| n.story_id.as_str()).collect();
    let mut keys = BTreeSet::new();
    let mut bodies = BTreeSet::new();
    for n in notes {
        if !label(&n.note_id) {
            return Err("note_id");
        }
        if !keys.insert(n.note_id.as_str()) {
            return Err("note_duplicate");
        }
        if id(&n.story_id, "STXT", "footnote").is_err() || id(&n.note_story_id, "STXT", "footnote").is_err() {
            return Err("note_story_ref");
        }
        if n.note_story_id == n.story_id || hosts.contains(n.note_story_id.as_str()) {
            return Err("footnote_in_footnote");
        }
        if !bodies.insert(n.note_story_id.as_str()) {
            return Err("note_story_shared");
        }
    }
    Ok(())
}

/// Footnote option bounds of STU-LAY-150.
pub fn check_footnote_options(o: &StudioFootnoteOptions) -> std::result::Result<(), &'static str> {
    if !o.numbering_style.legal_for_footnotes() {
        return Err("number_style_not_legal_for_footnotes");
    }
    if !(1..=400).contains(&o.start_at) {
        return Err("footnote_start_at_range");
    }
    if o.prefix.chars().count() > 100 || o.suffix.chars().count() > 100 {
        return Err("footnote_affix_length");
    }
    let nonneg = |l: &Length| l.unit == GeometryUnit::Pt && l.value.is_finite() && l.value >= 0.0;
    if !nonneg(&o.space_between) || !nonneg(&o.spacer) {
        return Err("footnote_spacing");
    }
    for r in [&o.rule, &o.continuing_rule].into_iter().flatten() {
        let ok = nonneg(&r.weight)
            && r.weight.value <= 1000.0
            && percent(r.tint)
            && [r.colour.r, r.colour.g, r.colour.b].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            && [&r.left_indent, &r.width, &r.offset].iter().all(|l| nonneg(l));
        if !ok {
            return Err("footnote_rule");
        }
    }
    Ok(())
}

/// Index-mark rules: unique keys, `STXT` story, one to four non-empty topic levels, sort-by of the
/// same length when present, cross references with a valid target and custom text exactly for the
/// custom kinds.
pub fn check_index_marks(marks: &[StudioIndexMark]) -> std::result::Result<(), &'static str> {
    if marks.len() > MAX_INDEX_MARKS {
        return Err("too_many_index_marks");
    }
    let path_ok = |p: &[String]| {
        (1..=MAX_INDEX_LEVELS).contains(&p.len())
            && p.iter().all(|t| !t.trim().is_empty() && t.len() <= 1024 && !t.chars().any(char::is_control))
    };
    let mut keys = BTreeSet::new();
    for m in marks {
        if !label(&m.mark_id) {
            return Err("mark_id");
        }
        if !keys.insert(m.mark_id.as_str()) {
            return Err("mark_duplicate");
        }
        if id(&m.story_id, "STXT", "index").is_err() {
            return Err("mark_story_ref");
        }
        if !path_ok(&m.topics) {
            return Err("index_topic_levels");
        }
        if !m.sort_by.is_empty() && m.sort_by.len() != m.topics.len() {
            return Err("index_sort_by_levels");
        }
        if let Some(x) = &m.xref {
            if !path_ok(&x.target) {
                return Err("index_xref_target");
            }
            let custom = matches!(x.kind, IndexXrefKind::Custom | IndexXrefKind::CustomBefore | IndexXrefKind::CustomAfter);
            if custom != x.custom_text.as_deref().is_some_and(|t| !t.is_empty()) {
                return Err("index_xref_custom_text");
            }
        }
    }
    Ok(())
}

/// Index-option rules: separators at most 100 characters without line breaks, empty sections only
/// with headings, at most four level styles, `STYS` references.
pub fn check_index_options(o: &StudioIndexOptions) -> std::result::Result<(), &'static str> {
    let seps = [
        &o.following_topic_separator,
        &o.between_entries_separator,
        &o.page_range_separator,
        &o.between_page_numbers_separator,
        &o.before_cross_reference_separator,
        &o.entry_end_separator,
    ];
    if seps.iter().any(|s| s.chars().count() > 100 || s.contains('\n')) {
        return Err("index_separator");
    }
    if o.include_empty_index_sections && !o.include_section_headings {
        return Err("index_empty_sections_without_headings");
    }
    if o.level_styles.len() > MAX_INDEX_LEVELS {
        return Err("index_level_styles");
    }
    let style_ok = |s: &Option<String>| s.as_deref().is_none_or(|v| id(v, "STYS", "index").is_ok());
    if !style_ok(&o.title_style) || !style_ok(&o.section_heading_style) || !o.level_styles.iter().all(style_ok) {
        return Err("index_style_ref");
    }
    if o.title.len() > 1024 || o.title.chars().any(char::is_control) {
        return Err("index_title");
    }
    Ok(())
}

/// STU-LAY-016 change-case enumeration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeCase {
    #[default]
    None,
    Uppercase,
    Lowercase,
    Titlecase,
    Sentencecase,
}

/// Which on-page occurrence a running header uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunningUse {
    #[default]
    FirstOnPage,
    LastOnPage,
}

/// STU-LAY-015 `variable_scope`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VariableScope {
    #[default]
    Document,
    Section,
}

/// The twelve captured text-variable kinds (STU-LAY-048).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "variable_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioVariableKind {
    CustomText {
        text: String,
    },
    FileName {
        #[serde(default, skip_serializing_if = "is_false")]
        include_path: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        include_extension: bool,
    },
    LastPageNumber {
        #[serde(default)]
        scope: VariableScope,
        /// Any numbering member legal on a variable, including `current`.
        style: StudioNumberStyle,
    },
    ChapterNumber {
        number: u32,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        before: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        after: String,
        style: StudioNumberStyle,
    },
    OutputDate {
        format: String,
    },
    CreationDate {
        format: String,
    },
    ModificationDate {
        format: String,
    },
    MatchCharacterStyle {
        /// `STYS` character style.
        style: String,
        #[serde(default)]
        use_on_page: RunningUse,
        #[serde(default, skip_serializing_if = "is_false")]
        delete_end_punctuation: bool,
        #[serde(default)]
        change_case: ChangeCase,
    },
    MatchParagraphStyle {
        /// `STYS` paragraph style.
        style: String,
        #[serde(default)]
        use_on_page: RunningUse,
        #[serde(default, skip_serializing_if = "is_false")]
        delete_end_punctuation: bool,
        #[serde(default)]
        change_case: ChangeCase,
    },
    XrefPageNumber,
    XrefChapterNumber,
    LiveCaption,
}

/// One text variable definition (a single edit updates every instance).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTextVariable {
    /// Document-local key, unique in the set.
    pub variable_id: String,
    pub kind: StudioVariableKind,
}

/// One part of a running text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "part_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioRunningPart {
    Text { text: String },
    Variable { variable_id: String },
    /// STU-LAY-015 automatic page number of the page showing the text.
    PageNumber,
    /// The section marker of the page's section.
    SectionMarker,
}

/// A running header or footer shown in text frames (one per page).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRunningText {
    pub running_id: String,
    /// `SLYR` text-frame layers that show the text (each on its own page).
    pub frame_ids: Vec<String>,
    pub parts: Vec<StudioRunningPart>,
}

/// STU-LAY-150 `restart_endnote_numbering`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndnoteRestart {
    #[default]
    Continuous,
    StoryRestart,
}

/// STU-LAY-150 `scope_value`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndnoteScope {
    #[default]
    Document,
    Story,
}

/// One endnote (same shape as a footnote).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEndnote {
    pub note_id: String,
    pub story_id: String,
    pub anchor: u64,
    pub note_story_id: String,
}

/// STU-LAY-150 endnote options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEndnoteOptions {
    /// 0..=100 characters.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_style: Option<String>,
    pub numbering_style: StudioNumberStyle,
    /// 1..=400 (the footnote bound; the endnote capture states no bound).
    pub start_at: u32,
    #[serde(default)]
    pub restart: EndnoteRestart,
    #[serde(default)]
    pub scope: EndnoteScope,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suffix: String,
    /// Between the number and the note text, 0..=100 characters.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub separator_text: String,
}

/// Provisional sidenote placement (SPEC GAP, see module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SidenoteSide {
    #[default]
    Outside,
    Inside,
}
/// Provisional sidenote anchor (SPEC GAP).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SidenoteAnchor {
    #[default]
    ReferenceLine,
    FrameTop,
}
/// Provisional sidenote options (SPEC GAP).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSidenoteOptions {
    /// Column width, `pt`, greater than zero.
    pub width: Length,
    /// Distance from the frame edge, `pt`, non-negative.
    pub gap: Length,
    /// Vertical space between stacked sidenotes, `pt`, non-negative.
    pub space_between: Length,
    #[serde(default)]
    pub side: SidenoteSide,
    #[serde(default)]
    pub anchor: SidenoteAnchor,
    pub numbering_style: StudioNumberStyle,
    pub start_at: u32,
    #[serde(default)]
    pub restart: FootnoteRestart,
}

/// Text variables per record set.
pub const MAX_TEXT_VARIABLES: usize = 4096;
/// Running texts per record set.
pub const MAX_RUNNING_TEXTS: usize = 4096;

/// Text-variable and running-text rules: unique keys, `STYS` styles on the matching kinds,
/// legal numbering styles, bounded text, running parts naming defined variables and `SLYR`
/// frames, no frame shown by two running texts.
pub fn check_running(vars: &[StudioTextVariable], running: &[StudioRunningText]) -> std::result::Result<(), &'static str> {
    if vars.len() > MAX_TEXT_VARIABLES || running.len() > MAX_RUNNING_TEXTS {
        return Err("too_many_variables");
    }
    let short = |t: &str| t.chars().count() <= 1024 && !t.chars().any(|c| c.is_control() && c != '\t');
    let mut keys = BTreeSet::new();
    for v in vars {
        if !label(&v.variable_id) {
            return Err("variable_id");
        }
        if !keys.insert(v.variable_id.as_str()) {
            return Err("variable_duplicate");
        }
        match &v.kind {
            StudioVariableKind::CustomText { text } if !short(text) => return Err("variable_text"),
            StudioVariableKind::OutputDate { format } | StudioVariableKind::CreationDate { format } | StudioVariableKind::ModificationDate { format }
                if !short(format) => return Err("variable_text"),
            StudioVariableKind::ChapterNumber { before, after, style, .. } => {
                if !short(before) || !short(after) {
                    return Err("variable_text");
                }
                if *style == StudioNumberStyle::Current {
                    return Err("number_style_not_legal_here");
                }
            }
            StudioVariableKind::MatchCharacterStyle { style, .. } | StudioVariableKind::MatchParagraphStyle { style, .. } if id(style, "STYS", "variable").is_err() => {
                return Err("variable_style_ref");
            }
            _ => {}
        }
    }
    let mut shown = BTreeSet::new();
    let mut runs = BTreeSet::new();
    for r in running {
        if !label(&r.running_id) {
            return Err("running_id");
        }
        if !runs.insert(r.running_id.as_str()) {
            return Err("running_duplicate");
        }
        for f in &r.frame_ids {
            if id(f, "SLYR", "running").is_err() {
                return Err("running_frame_ref");
            }
            if !shown.insert(f.as_str()) {
                return Err("running_frame_shared");
            }
        }
        if r.parts.len() > 256 {
            return Err("running_parts");
        }
        for part in &r.parts {
            match part {
                StudioRunningPart::Text { text } if !short(text) => return Err("variable_text"),
                StudioRunningPart::Variable { variable_id } if !keys.contains(variable_id.as_str()) => return Err("running_variable_missing"),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Endnote rules: footnote-shaped references (no note in a note, no shared note story) and the
/// option bounds.
pub fn check_endnotes(notes: &[StudioEndnote], o: Option<&StudioEndnoteOptions>) -> std::result::Result<(), &'static str> {
    let as_foot: Vec<StudioFootnote> = notes
        .iter()
        .map(|n| StudioFootnote { note_id: n.note_id.clone(), story_id: n.story_id.clone(), anchor: n.anchor, note_story_id: n.note_story_id.clone() })
        .collect();
    check_footnotes(&as_foot)?;
    if let Some(o) = o {
        if !o.numbering_style.legal_for_footnotes() {
            return Err("number_style_not_legal_for_endnotes");
        }
        if !(1..=400).contains(&o.start_at) {
            return Err("endnote_start_at_range");
        }
        if [&o.title, &o.prefix, &o.suffix, &o.separator_text].iter().any(|t| t.chars().count() > 100 || t.contains('\n')) {
            return Err("endnote_text_length");
        }
        let style_ok = |s: &Option<String>| s.as_deref().is_none_or(|v| id(v, "STYS", "endnote").is_ok());
        if !style_ok(&o.title_style) || !style_ok(&o.text_style) {
            return Err("endnote_style_ref");
        }
    }
    Ok(())
}

/// Provisional sidenote option bounds.
pub fn check_sidenote_options(o: &StudioSidenoteOptions) -> std::result::Result<(), &'static str> {
    let ok = |l: &Length, positive: bool| l.unit == GeometryUnit::Pt && l.value.is_finite() && if positive { l.value > 0.0 } else { l.value >= 0.0 };
    if !ok(&o.width, true) || !ok(&o.gap, false) || !ok(&o.space_between, false) {
        return Err("sidenote_geometry");
    }
    if !o.numbering_style.legal_for_footnotes() {
        return Err("number_style_not_legal_for_sidenotes");
    }
    if !(1..=400).contains(&o.start_at) {
        return Err("sidenote_start_at_range");
    }
    Ok(())
}

/// The long-document records of one document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StudioLongDocRecords {
    pub schema_id: LongDocRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<StudioTable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphic_frames: Vec<StudioGraphicFrame>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<StudioSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toc_styles: Vec<StudioTocStyle>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footnotes: Vec<StudioFootnote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footnote_options: Option<StudioFootnoteOptions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index_marks: Vec<StudioIndexMark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_options: Option<StudioIndexOptions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_variables: Vec<StudioTextVariable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_texts: Vec<StudioRunningText>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endnotes: Vec<StudioEndnote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endnote_options: Option<StudioEndnoteOptions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sidenotes: Vec<StudioEndnote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidenote_options: Option<StudioSidenoteOptions>,
}
impl StudioLongDocRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: LongDocRecordsSchema::V1,
            document_id: document_id.into(),
            tables: Vec::new(),
            graphic_frames: Vec::new(),
            sections: Vec::new(),
            toc_styles: Vec::new(),
            footnotes: Vec::new(),
            footnote_options: None,
            index_marks: Vec::new(),
            index_options: None,
            text_variables: Vec::new(),
            running_texts: Vec::new(),
            endnotes: Vec::new(),
            endnote_options: None,
            sidenotes: Vec::new(),
            sidenote_options: None,
        }
    }
    /// The graphic frame of this layer.
    pub fn graphic_frame(&self, layer_id: &str) -> Option<&StudioGraphicFrame> {
        self.graphic_frames.iter().find(|g| g.layer_id == layer_id)
    }
    /// The table with this key.
    pub fn table(&self, table_id: &str) -> Option<&StudioTable> {
        self.tables.iter().find(|t| t.table_id == table_id)
    }
}

/// JSON schema (draft 2020-12) of the long-document record set.
pub fn longdoc_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioLongDocRecords>()
}

fn pt(l: &Length, target: &str) -> Result<f64> {
    finite_length(l, GeometryUnit::Pt, true, target)?;
    Ok(l.value)
}

fn stroke(s: &StudioCellStroke, target: &str, c: &mut Ctx) -> Result<()> {
    pt(&s.weight, target)?;
    if !percent(s.tint) {
        return Err(err(Code::InvalidField, format!("{target}/tint")));
    }
    s.colour.validate(target, c)
}

/// Checks one table's grid: sizes, spans inside the grid (checked, never clamped), no overlap and
/// no uncovered slot. The occupancy grid is allocated only after `rows * columns` is known to be at
/// most `MAX_TABLE_CELLS`.
pub fn check_table_grid(t: &StudioTable) -> std::result::Result<(), &'static str> {
    let (rows, cols) = (t.rows.len(), t.column_widths.len());
    if rows == 0 || cols == 0 {
        return Err("table_empty");
    }
    if rows > MAX_TABLE_ROWS || cols > MAX_TABLE_COLUMNS {
        return Err("table_too_large");
    }
    let slots = rows.checked_mul(cols).ok_or("table_too_large")?;
    if slots > MAX_TABLE_CELLS || t.cells.len() > slots {
        return Err("table_too_large");
    }
    let mut seen_body = false;
    let mut seen_footer = false;
    for r in &t.rows {
        match r.row_type {
            TableRowType::Header if seen_body || seen_footer => return Err("row_order"),
            TableRowType::Body if seen_footer => return Err("row_order"),
            TableRowType::Body => seen_body = true,
            TableRowType::Footer => seen_footer = true,
            TableRowType::Header => {}
        }
    }
    let mut taken = vec![false; slots];
    for cell in &t.cells {
        let (r0, c0) = (cell.row as usize, cell.column as usize);
        if cell.row_span == 0 || cell.column_span == 0 {
            return Err("span_zero");
        }
        let r1 = r0.checked_add(cell.row_span as usize).ok_or("span_overflow")?;
        let c1 = c0.checked_add(cell.column_span as usize).ok_or("span_overflow")?;
        if r1 > rows || c1 > cols {
            return Err("span_overflow");
        }
        let kind = t.rows.get(r0).map(|r| r.row_type);
        for r in r0..r1 {
            if t.rows.get(r).map(|x| x.row_type) != kind {
                return Err("span_crosses_row_type");
            }
            for col in c0..c1 {
                let at = r
                    .checked_mul(cols)
                    .and_then(|v| v.checked_add(col))
                    .ok_or("span_overflow")?;
                let slot = taken.get_mut(at).ok_or("span_overflow")?;
                if *slot {
                    return Err("cells_overlap");
                }
                *slot = true;
            }
        }
    }
    if taken.iter().any(|s| !*s) {
        return Err("cell_missing");
    }
    Ok(())
}

fn validate_table(t: &StudioTable, c: &mut Ctx) -> Result<()> {
    c.tick()?;
    let tid = t.table_id.as_str();
    if !label(tid) {
        return Err(err(Code::InvalidField, "table_id"));
    }
    id(&t.story_id, "STXT", &format!("{tid}/story_id"))?;
    if let Some(s) = &t.cell_story_id {
        id(s, "STXT", &format!("{tid}/cell_story_id"))?;
        if *s == t.story_id {
            return Err(err(Code::RenderCycle, format!("{tid}/cell_story_id")));
        }
    }
    if t.rows.len() > MAX_TABLE_ROWS || t.column_widths.len() > MAX_TABLE_COLUMNS {
        return Err(err(Code::BudgetExceeded, tid));
    }
    c.nodes(t.rows.len(), tid)?;
    c.nodes(t.cells.len(), tid)?;
    for (i, w) in t.column_widths.iter().enumerate() {
        let target = format!("{tid}/column_widths/{i}");
        if pt(w, &target)? <= 0.0 {
            return Err(err(Code::InvalidField, target));
        }
    }
    for (i, r) in t.rows.iter().enumerate() {
        c.tick()?;
        let target = format!("{tid}/rows/{i}");
        let (lo, hi) = (pt(&r.minimum_height, &target)?, pt(&r.maximum_height, &target)?);
        if hi < lo || hi <= 0.0 {
            return Err(err(Code::InvalidField, target));
        }
    }
    check_table_grid(t).map_err(|why| {
        let code = if why == "table_too_large" {
            Code::BudgetExceeded
        } else {
            Code::InvalidField
        };
        err(code, format!("{tid}/{why}"))
    })?;
    let mut any_content = false;
    for cell in &t.cells {
        c.tick()?;
        let target = format!("{tid}/cells/{}/{}", cell.row, cell.column);
        for l in [&cell.insets.top, &cell.insets.right, &cell.insets.bottom, &cell.insets.left] {
            pt(l, &target)?;
        }
        if let Some(r) = cell.content {
            any_content = true;
            if r.start > r.end {
                return Err(err(Code::InvalidField, format!("{target}/content")));
            }
        }
        if let Some(f) = &cell.fill {
            f.validate(&target, c)?;
        }
        if cell.fill_tint.is_some_and(|v| !percent(v)) {
            return Err(err(Code::InvalidField, format!("{target}/fill_tint")));
        }
        for s in cell.strokes.each().into_iter().flatten() {
            stroke(s, &target, c)?;
        }
    }
    if any_content && t.cell_story_id.is_none() {
        return Err(err(Code::InvalidField, format!("{tid}/cell_story_id")));
    }
    Ok(())
}

/// Graphic-frame checks: the layer exists and carries raster content, the page exists, positive
/// pt bounds and content size, finite crop leaving a positive content box, a legal fit member.
fn validate_graphic_frame(g: &StudioGraphicFrame, d: &StudioDocument, c: &mut Ctx) -> Result<()> {
    c.tick()?;
    let lid = g.layer_id.as_str();
    id(lid, "SLYR", "graphic_frames/layer_id")?;
    let layer = d
        .layers
        .iter()
        .find(|l| l.layer_id == lid)
        .ok_or_else(|| err(Code::DanglingReference, lid))?;
    if !matches!(layer.payload, super::Payload::Raster { .. }) {
        return Err(err(Code::PayloadMismatch, lid));
    }
    if d.locate_page(&g.page_id).is_none() {
        return Err(err(Code::DanglingReference, format!("{lid}/page_id")));
    }
    let b = &g.bounds;
    let target = format!("{lid}/bounds");
    finite_length(&b.x, GeometryUnit::Pt, false, &target)?;
    finite_length(&b.y, GeometryUnit::Pt, false, &target)?;
    let (w, h) = (pt(&b.width, &target)?, pt(&b.height, &target)?);
    let target = format!("{lid}/content");
    let (cw, ch) = (pt(&g.content_width, &target)?, pt(&g.content_height, &target)?);
    if w <= 0.0 || h <= 0.0 || cw <= 0.0 || ch <= 0.0 {
        return Err(err(Code::InvalidField, target));
    }
    if let Some(e) = &g.crop {
        let target = format!("{lid}/crop");
        for l in [&e.top, &e.right, &e.bottom, &e.left] {
            finite_length(l, GeometryUnit::Pt, false, &target)?;
        }
        if cw - e.left.value - e.right.value <= 0.0 || ch - e.top.value - e.bottom.value <= 0.0 {
            return Err(err(Code::InvalidField, target));
        }
    }
    if [g.fit, g.frame_fitting].contains(&Some(StudioFit::HonourExistingStyle))
        || g.frame_fitting == Some(StudioFit::ApplyFrameFittingOptions)
    {
        return Err(err(Code::InvalidField, format!("{lid}/fit")));
    }
    if let StudioFrameShape::Path { path_id } = &g.shape {
        id(path_id, "SVPT", &format!("{lid}/shape"))?;
    }
    Ok(())
}

/// The single validator: document binding, unique keys, `STXT` story references and every
/// member's bounds (see the member docs). Records count against `Budget::nodes`.
pub fn validate_longdoc_records(
    r: &StudioLongDocRecords,
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Vec<ResolutionIssue>> {
    let mut cx = Ctx::new(b, token, resolver);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.tables.len() > MAX_TABLES {
        return Err(err(Code::BudgetExceeded, "tables"));
    }
    cx.nodes(r.tables.len(), "tables")?;
    let mut keys = BTreeSet::new();
    for t in &r.tables {
        if !keys.insert(t.table_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &t.table_id));
        }
        validate_table(t, &mut cx)?;
    }
    if r.graphic_frames.len() > MAX_GRAPHIC_FRAMES {
        return Err(err(Code::BudgetExceeded, "graphic_frames"));
    }
    cx.nodes(r.graphic_frames.len(), "graphic_frames")?;
    let mut layers = BTreeSet::new();
    for g in &r.graphic_frames {
        if !layers.insert(g.layer_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &g.layer_id));
        }
        validate_graphic_frame(g, d, &mut cx)?;
    }
    cx.nodes(r.sections.len(), "sections")?;
    for sec in &r.sections {
        cx.text(&sec.name, "sections")?;
    }
    check_sections(&r.sections, d).map_err(|why| {
        let code = match why {
            "section_page_missing" => Code::DanglingReference,
            "section_duplicate" | "sections_share_start_page" | "section_prefix_duplicate" => Code::DuplicateIdentity,
            "too_many_sections" => Code::BudgetExceeded,
            _ => Code::InvalidField,
        };
        err(code, format!("sections/{why}"))
    })?;
    cx.nodes(r.toc_styles.len(), "toc_styles")?;
    let mut tocs = BTreeSet::new();
    for t in &r.toc_styles {
        cx.tick()?;
        if !tocs.insert(t.toc_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &t.toc_id));
        }
        cx.nodes(t.entries.len(), &t.toc_id)?;
        check_toc_style(t).map_err(|why| {
            let code = match why {
                "toc_style_mapped_twice" => Code::DuplicateIdentity,
                "toc_style_ref" => Code::InvalidId,
                "toc_too_many_entries" => Code::BudgetExceeded,
                _ => Code::InvalidField,
            };
            err(code, format!("{}/{why}", t.toc_id))
        })?;
    }
    cx.nodes(r.footnotes.len(), "footnotes")?;
    check_footnotes(&r.footnotes).map_err(|why| {
        let code = match why {
            "footnote_in_footnote" => Code::RenderCycle,
            "note_duplicate" | "note_story_shared" => Code::DuplicateIdentity,
            "note_story_ref" => Code::InvalidId,
            "too_many_footnotes" => Code::BudgetExceeded,
            _ => Code::InvalidField,
        };
        err(code, format!("footnotes/{why}"))
    })?;
    if let Some(o) = &r.footnote_options {
        check_footnote_options(o).map_err(|why| err(Code::InvalidField, format!("footnote_options/{why}")))?;
        for rule in [&o.rule, &o.continuing_rule].into_iter().flatten() {
            rule.colour.validate("footnote_options", &mut cx)?;
        }
    }
    cx.nodes(r.index_marks.len(), "index_marks")?;
    for m in &r.index_marks {
        for t in &m.topics {
            cx.text(t, "index_marks")?;
        }
    }
    check_index_marks(&r.index_marks).map_err(|why| {
        let code = match why {
            "mark_duplicate" => Code::DuplicateIdentity,
            "mark_story_ref" => Code::InvalidId,
            "too_many_index_marks" => Code::BudgetExceeded,
            _ => Code::InvalidField,
        };
        err(code, format!("index_marks/{why}"))
    })?;
    if let Some(o) = &r.index_options {
        check_index_options(o).map_err(|why| {
            let code = if why == "index_style_ref" { Code::InvalidId } else { Code::InvalidField };
            err(code, format!("index_options/{why}"))
        })?;
    }
    cx.nodes(r.text_variables.len(), "text_variables")?;
    cx.nodes(r.running_texts.len(), "running_texts")?;
    check_running(&r.text_variables, &r.running_texts).map_err(|why| {
        let code = match why {
            "variable_duplicate" | "running_duplicate" | "running_frame_shared" => Code::DuplicateIdentity,
            "variable_style_ref" | "running_frame_ref" => Code::InvalidId,
            "running_variable_missing" => Code::DanglingReference,
            "too_many_variables" => Code::BudgetExceeded,
            _ => Code::InvalidField,
        };
        err(code, format!("running/{why}"))
    })?;
    for r in &r.running_texts {
        for f in &r.frame_ids {
            let layer = d.layers.iter().find(|l| l.layer_id == *f).ok_or_else(|| err(Code::DanglingReference, format!("running/{f}")))?;
            if layer.text_frame.is_none() {
                return Err(err(Code::PayloadMismatch, format!("running/{f}")));
            }
        }
    }
    cx.nodes(r.endnotes.len(), "endnotes")?;
    check_endnotes(&r.endnotes, r.endnote_options.as_ref()).map_err(|why| {
        let code = match why {
            "footnote_in_footnote" => Code::RenderCycle,
            "note_duplicate" | "note_story_shared" => Code::DuplicateIdentity,
            "note_story_ref" | "endnote_style_ref" => Code::InvalidId,
            "too_many_footnotes" => Code::BudgetExceeded,
            _ => Code::InvalidField,
        };
        err(code, format!("endnotes/{why}"))
    })?;
    cx.nodes(r.sidenotes.len(), "sidenotes")?;
    check_endnotes(&r.sidenotes, None).map_err(|why| {
        let code = if why == "footnote_in_footnote" { Code::RenderCycle } else { Code::InvalidField };
        err(code, format!("sidenotes/{why}"))
    })?;
    if let Some(o) = &r.sidenote_options {
        check_sidenote_options(o).map_err(|why| err(Code::InvalidField, format!("sidenote_options/{why}")))?;
    }
    canceled(token)?;
    Ok(cx.issues)
}
