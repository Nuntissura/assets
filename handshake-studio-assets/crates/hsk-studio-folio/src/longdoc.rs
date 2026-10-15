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
}
impl StudioLongDocRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: LongDocRecordsSchema::V1,
            document_id: document_id.into(),
            tables: Vec::new(),
            graphic_frames: Vec::new(),
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
    canceled(token)?;
    Ok(cx.issues)
}
