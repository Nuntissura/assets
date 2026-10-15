//! Tables (STU-LAY-035, STU-LAY-139..141): a Folio `StudioTable` flows through the frame thread of
//! its host story, row by row, from the top of the first frame or from a caller-given position.
//! The result is a revision-bound proposal: frame parts with their row pieces, placed cells (frame
//! coordinates), resolved edge strokes in drawing order, cell fills and the cell text as a
//! `LayoutProposal` of the cell story (lines in frame coordinates, ready for the PDF lowering).
//!
//! Rules (documented here; InDesign parity UNVERIFIED, D5):
//! - Cell text is composed by the same core as `reflow_typeset` (`compose`) into the cell's width
//!   minus its left/right insets; the first baseline is the top inset plus the first paragraph's
//!   ascent (InDesign's "Ascent" first-baseline default). A cell's content height is its last line's
//!   bottom plus the bottom inset (insets only when empty).
//! - Row heights: the larger of `minimum_height` and the tallest single-row cell; a row-spanning
//!   cell that needs more adds the difference to its last row (narrow spans first); then every row
//!   is capped at `maximum_height`. A cell whose content exceeds its final height keeps the lines
//!   that fit and is listed in `overset_cells`.
//! - Header rows repeat at the top of every frame part (`break_headers`: once per text frame and in
//!   all text columns -> every part, one column only; once per page -> the first part on each page),
//!   unless `skip_first_header` hides them in the first part. Footer rows follow the last body row of
//!   a part on the same rule; `skip_last_footer` hides them in the last part. Footer space is always
//!   reserved while planning.
//! - Body rows break across frames between rows. Rows joined by a row-spanning cell or by
//!   `keep_with_next_row` move as one group. `start_row` next frame / page / odd / even page starts
//!   the row in a later frame (page parity = 1-based document page ordinal). A body row with
//!   `allow_split` that does not fit an otherwise empty frame part splits at line boundaries: each
//!   cell keeps the lines that fit, and its next piece starts with the cell's next line at the top
//!   inset. A group that fits no frame is overset (`overset_row`), never truncated.
//! - Strokes: on each boundary between two vertically or horizontally adjacent cells of a part, the
//!   stroke of the cell with the higher edge-stroke priority draws; on a tie, the lower or right cell
//!   (the later one in reading order); a cell without a stroke on that edge yields to the other.
//!   Boundaries inside a span draw nothing. Collinear unit segments with the same stroke are merged.
//!   `stroke_order` row on top draws column (vertical) strokes first, column on top the reverse;
//!   best joins and legacy are drawn as row on top and noted (`stroke_order_approximated`).
//! - Fills: one rectangle per placed cell piece with the cell fill and tint.
//!
//! Hostile input: rows, columns and `rows * columns` are checked (checked arithmetic) against
//! `TableLimits` before anything is composed or allocated; spans are checked against the grid edge
//! (`span_overflow`), overlaps and holes are refused. Integer arithmetic is denied (V4-01).
#![deny(clippy::arithmetic_side_effects)]
use crate::folio_input::{Frame, pt, resolve, story_text};
use crate::styles::ParagraphStyling;
use crate::threaded_reflow::{FramePlacement, LayoutProposal, Limits, Line, compose};
use crate::{
    Composer, LayoutError, Rect, ShapedParagraph, SourceSpan, SpaceParams, TypesetOptions,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    MAX_TABLE_CELLS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, StudioCellStroke, StudioColor,
    StudioDocument, StudioTable, StudioTextStory, TableHeaderBreak, TableRowType, TableStartRow,
    TableStrokeOrder, VerticalJustification, check_table_grid,
};

const EPS: f64 = 1e-9;
/// Height of the scratch column a cell is composed in; a cell taller than this is refused.
const CELL_HEIGHT_CAP_PT: f64 = 1.0e6;

/// A position inside the frame thread of a story: thread position and distance from the frame top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlowPosition {
    pub frame: usize,
    pub y_pt: f64,
}

/// Hard bounds of one table call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableLimits {
    pub max_rows: usize,
    pub max_columns: usize,
    /// Grid slots, `rows * columns`.
    pub max_cells: usize,
    /// Line, glyph and paragraph bounds of the cell text.
    pub lines: Limits,
}
impl Default for TableLimits {
    fn default() -> Self {
        Self {
            max_rows: MAX_TABLE_ROWS,
            max_columns: MAX_TABLE_COLUMNS,
            max_cells: MAX_TABLE_CELLS,
            lines: Limits::default(),
        }
    }
}

/// Typed refusals; nothing partial is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableError {
    Layout(LayoutError),
    /// The grid is malformed: `table_empty`, `row_order`, `span_zero`, `span_overflow`,
    /// `span_crosses_row_type`, `cells_overlap`, `cell_missing`.
    Grid(&'static str),
    /// Rows, columns or grid slots over `TableLimits` (checked before allocation).
    TooLarge,
    /// A cell's content range does not start and end on shaped paragraph boundaries.
    CellContent,
    /// A table anchor is neither a host paragraph start nor the story end (`reflow_with_tables`).
    Anchor,
}
impl TableError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Layout(e) => e.code(),
            Self::Grid(w) => w,
            Self::TooLarge => "table_too_large",
            Self::CellContent => "cell_content_not_on_paragraphs",
            Self::Anchor => "table_anchor_not_on_paragraph",
        }
    }
}
impl std::fmt::Display for TableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for TableError {}
impl From<LayoutError> for TableError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// Everything `layout_table` reads.
#[derive(Clone, Copy, Debug)]
pub struct TableRequest<'a> {
    pub document: &'a StudioDocument,
    pub table: &'a StudioTable,
    /// The host story named by `table.story_id`; its thread carries the table.
    pub host_story: &'a StudioTextStory,
    /// Resolved bytes of a stored host story (see `ReflowRequest::resolved_text`).
    pub host_resolved_text: Option<&'a str>,
    /// The story named by `table.cell_story_id` (required when it is set).
    pub cell_story: Option<&'a StudioTextStory>,
    pub cell_resolved_text: Option<&'a str>,
    /// The cell story's shaped paragraphs in order (byte offsets into the cell story).
    pub cell_paragraphs: &'a [ShapedParagraph],
    /// Where the table starts; `None` = the top inset of the first thread frame.
    pub start: Option<FlowPosition>,
    pub composer: Composer,
    pub space: SpaceParams,
    pub expected_revision: u64,
    pub limits: TableLimits,
}

/// Why a row piece is where it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PieceKind {
    /// A header row (repeated in every part that shows headers).
    Header,
    Body,
    /// A piece of a split body row: `index` 0 is the first piece; `last` closes the row.
    Split { index: usize, last: bool },
    Footer,
}

/// One row (or row piece) inside a frame part, in frame coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedRow {
    pub row: usize,
    pub y_pt: f64,
    pub height_pt: f64,
    pub kind: PieceKind,
}

/// The table rows placed in one thread frame.
#[derive(Clone, Debug, PartialEq)]
pub struct TablePart {
    pub frame_id: String,
    /// Position of the frame in the thread.
    pub thread_index: usize,
    pub frame_index: usize,
    pub spread: usize,
    pub page: usize,
    /// Frame top-left in spread coordinates.
    pub origin_x_pt: f64,
    pub origin_y_pt: f64,
    pub rows: Vec<PlacedRow>,
}

/// One drawn cell (or cell piece) of a part.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedCell {
    /// Index into `StudioTable::cells`.
    pub cell: usize,
    pub part: usize,
    /// Frame coordinates.
    pub rect: Rect,
    pub fill: Option<StudioColor>,
    /// Percent 0..=100.
    pub fill_tint: f64,
}

/// One resolved stroke segment (frame coordinates of its part).
#[derive(Clone, Debug, PartialEq)]
pub struct StrokeSegment {
    pub part: usize,
    pub from: [f64; 2],
    pub to: [f64; 2],
    pub horizontal: bool,
    pub weight_pt: f64,
    pub colour: StudioColor,
    pub tint: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableProposal {
    pub base_revision: u64,
    pub parts: Vec<TablePart>,
    pub cells: Vec<PlacedCell>,
    /// In drawing order (see module docs).
    pub strokes: Vec<StrokeSegment>,
    /// The cell story's lines, one `FramePlacement` per part (same order), paragraph indices into
    /// `TableRequest::cell_paragraphs`.
    pub text: LayoutProposal,
    /// First row that was not placed.
    pub overset_row: Option<usize>,
    /// Cells whose content exceeds their final height (indices into `StudioTable::cells`).
    pub overset_cells: Vec<usize>,
    /// Where the table ends (the frame and y after its last part); `None` when nothing was placed.
    pub end: Option<FlowPosition>,
    /// Approximations made (stable codes, sorted, unique).
    pub notes: Vec<&'static str>,
}

/// `colour` mixed with paper white by `tint_percent` (100 = the colour, 0 = white), per channel.
pub fn tint_rgb(colour: [f64; 3], tint_percent: f64) -> [f64; 3] {
    let t = (tint_percent / 100.0).clamp(0.0, 1.0);
    colour.map(|c| 1.0 - t * (1.0 - c))
}

/// A composed cell: lines relative to the cell's top-left (row-local).
struct CellText {
    lines: Vec<Line>,
    content_h: f64,
}

struct Grid<'t> {
    table: &'t StudioTable,
    cols: usize,
    /// Owner cell of each slot (row-major).
    owner: Vec<usize>,
    col_x: Vec<f64>,
    /// Cells by their top row.
    starts: Vec<Vec<usize>>,
}
impl Grid<'_> {
    fn at(&self, r: usize, c: usize) -> Option<usize> {
        r.checked_mul(self.cols)
            .and_then(|v| v.checked_add(c))
            .and_then(|i| self.owner.get(i).copied())
    }
    fn span(&self, cell: usize) -> (usize, usize, usize, usize) {
        let c = &self.table.cells[cell];
        let (r0, c0) = (c.row as usize, c.column as usize);
        // Spans were checked against the grid edge by `check_table_grid`.
        (
            r0,
            r0.saturating_add(c.row_span as usize),
            c0,
            c0.saturating_add(c.column_span as usize),
        )
    }
}

fn edge_pt(l: &hsk_studio_folio::Length) -> Result<f64, TableError> {
    let v = pt(l)?;
    if v < 0.0 {
        return Err(LayoutError::InvalidGeometry.into());
    }
    Ok(v)
}

/// The shaped paragraphs `[i0, i1)` that tile the byte range exactly.
fn paragraph_range(
    paragraphs: &[ShapedParagraph],
    start: u64,
    end: u64,
) -> Result<(usize, usize), TableError> {
    if start == end {
        return Ok((0, 0));
    }
    let i0 = paragraphs
        .iter()
        .position(|p| p.source.start == start)
        .ok_or(TableError::CellContent)?;
    let mut i1 = i0;
    let mut at = start;
    while at < end {
        let p = paragraphs.get(i1).ok_or(TableError::CellContent)?;
        if p.source.start != at || p.source.end > end || p.source.end < p.source.start {
            return Err(TableError::CellContent);
        }
        if p.source.end == at {
            return Err(TableError::CellContent);
        }
        at = p.source.end;
        i1 = i1.checked_add(1).ok_or(TableError::TooLarge)?;
    }
    Ok((i0, i1))
}

#[allow(clippy::too_many_arguments)]
fn compose_cell(
    frame_id: &str,
    width: f64,
    insets: [f64; 4],
    text: &str,
    paragraphs: &[ShapedParagraph],
    stylings: &[Option<ParagraphStyling>],
    range: (usize, usize),
    req: &TableRequest<'_>,
    opts: &TypesetOptions<'_>,
    cancel: &CancellationToken,
) -> Result<CellText, TableError> {
    let [top, right, bottom, left] = insets;
    if width - left - right <= EPS {
        return Err(LayoutError::InvalidGeometry.into());
    }
    let (i0, i1) = range;
    let (Some(paras), Some(styl)) = (paragraphs.get(i0..i1), stylings.get(i0..i1)) else {
        return Err(TableError::CellContent);
    };
    let Some(first) = paras.first() else {
        return Ok(CellText {
            lines: Vec::new(),
            content_h: top + bottom,
        });
    };
    let frame = Frame {
        id: frame_id,
        layer_index: 0,
        spread: 0,
        page: 0,
        origin_x_pt: 0.0,
        origin_y_pt: 0.0,
        width_pt: width,
        height_pt: CELL_HEIGHT_CAP_PT,
        inset_top_pt: top,
        inset_left_pt: left,
        inset_right_pt: right,
        inset_bottom_pt: bottom,
        first_baseline_pt: top + first.ascent_pt,
        obstacles: Vec::new(),
    };
    let composed = compose(
        std::slice::from_ref(&frame),
        text,
        paras,
        styl,
        req.composer,
        req.space,
        opts,
        req.limits.lines,
        cancel,
    )?;
    if composed.overset.is_some() {
        return Err(LayoutError::BudgetExceeded.into());
    }
    let mut lines = composed.lines.into_iter().next().unwrap_or_default();
    for l in &mut lines {
        l.paragraph = l.paragraph.checked_add(i0).ok_or(TableError::TooLarge)?;
    }
    let content_h = match lines.last() {
        Some(l) => {
            let p = &paragraphs[l.paragraph];
            l.baseline_y_pt + (p.line_height_pt - p.ascent_pt) + bottom
        }
        None => top + bottom,
    };
    Ok(CellText { lines, content_h })
}

/// Line box (top, bottom) relative to the cell top.
fn line_box(l: &Line, paragraphs: &[ShapedParagraph]) -> (f64, f64) {
    let p = &paragraphs[l.paragraph];
    (
        l.baseline_y_pt - p.ascent_pt,
        l.baseline_y_pt + (p.line_height_pt - p.ascent_pt),
    )
}

/// One planned cell piece inside a part: rect in frame coordinates and the lines it draws
/// (row-local lines translated by `dy`).
struct CellPiece {
    cell: usize,
    part: usize,
    rect: Rect,
    lines: Vec<Line>,
}

/// Split state of the row being split: per cell of the row, the next undrawn line.
struct SplitState {
    row: usize,
    next: Vec<(usize, usize)>,
    consumed_h: f64,
    pieces: usize,
}

struct Plan<'t> {
    grid: &'t Grid<'t>,
    heights: Vec<f64>,
    texts: Vec<CellText>,
    insets: Vec<[f64; 4]>,
    vj_offset: Vec<f64>,
    paragraphs: &'t [ShapedParagraph],
    /// Table left edge (frame coordinates) of each part.
    part_x: Vec<f64>,
    parts: Vec<TablePart>,
    cells: Vec<CellPiece>,
    notes: Vec<&'static str>,
}
impl Plan<'_> {
    fn cell_rect(&self, part: usize, cell: usize, y: f64, h: f64) -> Rect {
        let (_, _, c0, c1) = self.grid.span(cell);
        let x = self.part_x.get(part).copied().unwrap_or(0.0);
        let x0 = x + self.grid.col_x[c0];
        let x1 = x + self.grid.col_x[c1];
        Rect {
            x0_pt: x0,
            y0_pt: y,
            x1_pt: x1,
            y1_pt: y + h,
        }
    }
    /// Places whole rows `r0..r1` at `y` in `part`; cells starting in the range are placed once with
    /// their full (spanned) height and their lines.
    fn place_rows(&mut self, part: usize, r0: usize, r1: usize, y: f64, kind: PieceKind) {
        let mut at = y;
        let mut row_y = Vec::new();
        for r in r0..r1 {
            row_y.push(at);
            let h = self.heights[r];
            self.parts[part].rows.push(PlacedRow {
                row: r,
                y_pt: at,
                height_pt: h,
                kind,
            });
            at += h;
        }
        let starting: Vec<usize> = self.grid.starts[r0..r1].iter().flatten().copied().collect();
        for ci in starting {
            let (cr0, cr1, ..) = self.grid.span(ci);
            let top = row_y[cr0.saturating_sub(r0)];
            let h: f64 = self.heights[cr0..cr1.min(self.heights.len())].iter().sum();
            let rect = self.cell_rect(part, ci, top, h);
            let dy = top + self.vj_offset[ci];
            let floor = h - self.insets[ci][2] + EPS;
            let lines = self.texts[ci]
                .lines
                .iter()
                .filter(|l| line_box(l, self.paragraphs).1 <= floor)
                .map(|l| {
                    let mut l = l.clone();
                    l.baseline_y_pt += dy;
                    l.x_start_pt += rect.x0_pt;
                    l
                })
                .collect();
            self.cells.push(CellPiece {
                cell: ci,
                part,
                rect,
                lines,
            });
        }
    }
}

/// Lays out one table (module docs). `opts` applies to the cell text as in `reflow_typeset`.
pub fn layout_table(
    req: &TableRequest<'_>,
    opts: &TypesetOptions<'_>,
    cancel: &CancellationToken,
) -> Result<TableProposal, TableError> {
    cancel.check().map_err(|_| LayoutError::Canceled)?;
    let t = req.table;
    // Size gate first: nothing below allocates per row, column or slot before it passes.
    let (rows, cols) = (t.rows.len(), t.column_widths.len());
    let slots = rows.checked_mul(cols).ok_or(TableError::TooLarge)?;
    if rows > req.limits.max_rows
        || cols > req.limits.max_columns
        || slots > req.limits.max_cells
        || t.cells.len() > slots
    {
        return Err(TableError::TooLarge);
    }
    check_table_grid(t).map_err(|w| {
        if w == "table_too_large" {
            TableError::TooLarge
        } else {
            TableError::Grid(w)
        }
    })?;
    opts.validate()?;
    if req.expected_revision != req.document.revision {
        return Err(LayoutError::StaleRevision {
            expected: req.expected_revision,
            current: req.document.revision,
        }
        .into());
    }
    if t.story_id != req.host_story.story_id {
        return Err(LayoutError::InvalidInput.into());
    }
    let input = resolve(req.document, req.host_story, req.host_resolved_text, &[])?;
    let thread = input.frames;
    // Cell story and paragraphs.
    let cell_text: &str = match (&t.cell_story_id, req.cell_story) {
        (Some(id), Some(s)) if *id == s.story_id => story_text(s, req.cell_resolved_text)?,
        (None, None) => "",
        _ => return Err(LayoutError::InvalidInput.into()),
    };
    let mut prev_end = 0u64;
    for p in req.cell_paragraphs {
        if p.source.start < prev_end
            || p.source.end < p.source.start
            || p.source.end > cell_text.len() as u64
        {
            return Err(LayoutError::InvalidInput.into());
        }
        prev_end = p.source.end;
    }
    let styled;
    let (paragraphs, stylings): (&[ShapedParagraph], Vec<Option<ParagraphStyling>>) =
        match (opts.styles, req.cell_story) {
            (Some(src), Some(story)) => {
                let (a, b) =
                    crate::styles::apply(src, story, req.cell_paragraphs, opts.alignment)?;
                styled = a;
                (&styled, b.into_iter().map(Some).collect())
            }
            _ => (req.cell_paragraphs, vec![None; req.cell_paragraphs.len()]),
        };
    // Column positions.
    let mut col_x = Vec::with_capacity(cols.saturating_add(1));
    col_x.push(0.0);
    let mut acc = 0.0;
    for w in &t.column_widths {
        let v = pt(w)?;
        if v <= 0.0 {
            return Err(LayoutError::InvalidGeometry.into());
        }
        acc += v;
        col_x.push(acc);
    }
    let mut owner = vec![usize::MAX; slots];
    for (ci, c) in t.cells.iter().enumerate() {
        let (r0, c0) = (c.row as usize, c.column as usize);
        for r in r0..r0.saturating_add(c.row_span as usize) {
            for k in c0..c0.saturating_add(c.column_span as usize) {
                if let Some(slot) = r
                    .checked_mul(cols)
                    .and_then(|v| v.checked_add(k))
                    .and_then(|i| owner.get_mut(i))
                {
                    *slot = ci;
                }
            }
        }
    }
    let mut starts = vec![Vec::new(); rows];
    for (ci, c) in t.cells.iter().enumerate() {
        if let Some(v) = starts.get_mut(c.row as usize) {
            v.push(ci);
        }
    }
    let grid = Grid {
        table: t,
        cols,
        owner,
        col_x,
        starts,
    };
    let frame_id = thread.first().map_or("", |f| f.id);
    // Compose every cell.
    let mut texts = Vec::with_capacity(t.cells.len());
    let mut insets = Vec::with_capacity(t.cells.len());
    for (ci, c) in t.cells.iter().enumerate() {
        if ci.is_multiple_of(64) {
            cancel.check().map_err(|_| LayoutError::Canceled)?;
        }
        let e = &c.insets;
        let ins = [
            edge_pt(&e.top)?,
            edge_pt(&e.right)?,
            edge_pt(&e.bottom)?,
            edge_pt(&e.left)?,
        ];
        let range = match c.content {
            Some(r) => paragraph_range(paragraphs, r.start, r.end)?,
            None => (0, 0),
        };
        let (_, _, c0, c1) = grid.span(ci);
        let width = grid.col_x[c1] - grid.col_x[c0];
        texts.push(compose_cell(
            frame_id, width, ins, cell_text, paragraphs, &stylings, range, req, opts, cancel,
        )?);
        insets.push(ins);
    }
    // Row heights.
    let mut heights = Vec::with_capacity(rows);
    let mut max_h = Vec::with_capacity(rows);
    for r in &t.rows {
        let (lo, hi) = (edge_pt(&r.minimum_height)?, edge_pt(&r.maximum_height)?);
        if hi < lo || hi <= 0.0 {
            return Err(LayoutError::InvalidGeometry.into());
        }
        heights.push(lo);
        max_h.push(hi);
    }
    let mut by_span: Vec<usize> = (0..t.cells.len()).collect();
    by_span.sort_by_key(|&ci| (t.cells[ci].row_span, ci));
    for &ci in &by_span {
        let (r0, r1, ..) = grid.span(ci);
        let have: f64 = heights[r0..r1].iter().sum();
        let need = texts[ci].content_h - have;
        if need > EPS
            && let Some(last) = heights.get_mut(r1.saturating_sub(1))
        {
            *last += need;
        }
    }
    for (h, m) in heights.iter_mut().zip(&max_h) {
        *h = h.min(*m);
    }
    let mut overset_cells = Vec::new();
    let mut vj_offset = vec![0.0; t.cells.len()];
    let mut notes: Vec<&'static str> = Vec::new();
    for (ci, c) in t.cells.iter().enumerate() {
        let (r0, r1, ..) = grid.span(ci);
        let h: f64 = heights[r0..r1].iter().sum();
        let text = &texts[ci];
        if text.content_h > h + EPS {
            overset_cells.push(ci);
            continue;
        }
        let [top, _, bottom, _] = insets[ci];
        if let (Some(first), Some(last)) = (text.lines.first(), text.lines.last()) {
            let used = line_box(last, paragraphs).1 - line_box(first, paragraphs).0;
            let inner = h - top - bottom;
            let free = (inner - used).max(0.0);
            vj_offset[ci] = match c.vertical_justification {
                VerticalJustification::Top => 0.0,
                VerticalJustification::Center => free / 2.0,
                VerticalJustification::Bottom => free,
                VerticalJustification::Justify => {
                    notes.push("vertical_justify_drawn_top");
                    0.0
                }
            };
        }
    }
    let row_type = |r: usize| t.rows[r].row_type;
    let headers: Vec<usize> = (0..rows).filter(|&r| row_type(r) == TableRowType::Header).collect();
    let footers: Vec<usize> = (0..rows).filter(|&r| row_type(r) == TableRowType::Footer).collect();
    let body: Vec<usize> = (0..rows).filter(|&r| row_type(r) == TableRowType::Body).collect();
    let header_h: f64 = headers.iter().map(|&r| heights[r]).sum();
    let footer_h: f64 = footers.iter().map(|&r| heights[r]).sum();
    // Body groups: rows joined by a row-spanning cell or keep_with_next_row.
    let mut joined = vec![false; rows];
    for ci in 0..t.cells.len() {
        let (r0, r1, ..) = grid.span(ci);
        for j in joined.iter_mut().take(r1.saturating_sub(1)).skip(r0) {
            *j = true;
        }
    }
    for (r, j) in joined.iter_mut().enumerate() {
        let next_body = r
            .checked_add(1)
            .is_some_and(|n| n < rows && row_type(n) == TableRowType::Body);
        if t.rows[r].keep_with_next_row && next_body {
            *j = true;
        }
    }
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for &r in &body {
        match groups.last_mut() {
            Some((_, end)) if *end == r && r > 0 && joined[r.saturating_sub(1)] => {
                *end = r.saturating_add(1)
            }
            _ => groups.push((r, r.saturating_add(1))),
        }
    }
    // Page ordinals (1-based, document order) of every spread/page.
    let page_ordinal = |spread: usize, page: usize| -> usize {
        req.document
            .page_spreads
            .iter()
            .take(spread)
            .fold(0usize, |n, s| n.saturating_add(s.pages.len()))
            .saturating_add(page)
            .saturating_add(1)
    };
    let mut plan = Plan {
        grid: &grid,
        heights,
        texts,
        insets,
        vj_offset,
        paragraphs,
        part_x: Vec::new(),
        parts: Vec::new(),
        cells: Vec::new(),
        notes,
    };
    let start = req.start.unwrap_or(FlowPosition {
        frame: 0,
        y_pt: thread.first().map_or(0.0, |f| f.inset_top_pt),
    });
    if !start.y_pt.is_finite() || start.frame >= thread.len().max(1) {
        return Err(LayoutError::InvalidInput.into());
    }
    let started_at_top = thread
        .get(start.frame)
        .is_some_and(|f| start.y_pt <= f.inset_top_pt + EPS);
    let mut g = 0usize;
    let mut split: Option<SplitState> = None;
    let mut fi = start.frame;
    let mut overset_row = None;
    let mut part_bottoms: Vec<f64> = Vec::new();
    let no_body = groups.is_empty();
    let mut emitted_headers_only = false;
    loop {
        if (g >= groups.len() && split.is_none()) && !(no_body && !emitted_headers_only) {
            break;
        }
        cancel.check().map_err(|_| LayoutError::Canceled)?;
        let Some(f) = thread.get(fi) else {
            overset_row = match &split {
                Some(s) => Some(s.row),
                None => groups.get(g).map(|x| x.0).or(headers.first().copied()),
            };
            break;
        };
        let top = if fi == start.frame { start.y_pt.max(f.inset_top_pt) } else { f.inset_top_pt };
        let bottom = f.height_pt - f.inset_bottom_pt;
        let part = plan.parts.len();
        plan.part_x.push(f.inset_left_pt);
        let page_key = (f.spread, f.page);
        let prev_page = plan.parts.last().map(|p| (p.spread, p.page));
        let show_header = !headers.is_empty()
            && !(t.skip_first_header && part == 0)
            && match t.break_headers {
                TableHeaderBreak::OncePerPage => prev_page != Some(page_key),
                _ => true,
            };
        plan.parts.push(TablePart {
            frame_id: f.id.to_owned(),
            thread_index: fi,
            frame_index: f.layer_index,
            spread: f.spread,
            page: f.page,
            origin_x_pt: f.origin_x_pt,
            origin_y_pt: f.origin_y_pt,
            rows: Vec::new(),
        });
        let cells_before = plan.cells.len();
        let mut y = top;
        let header_fits = y + if show_header { header_h } else { 0.0 } + footer_h <= bottom + EPS;
        if show_header && header_fits {
            for &r in &headers {
                plan.place_rows(part, r, r.saturating_add(1), y, PieceKind::Header);
                y += plan.heights[r];
            }
        }
        let mut placed_any = false;
        if no_body && header_fits {
            placed_any = true;
            emitted_headers_only = true;
        }
        loop {
            if !header_fits || no_body {
                break;
            }
            // Continue a split row first.
            if let Some(s) = split.take() {
                let avail = bottom - footer_h - y;
                match place_split(&mut plan, part, s, y, avail) {
                    SplitStep::Done(h) => {
                        y += h;
                        placed_any = true;
                        g = g.saturating_add(1);
                        continue;
                    }
                    SplitStep::More(next, h) => {
                        y += h;
                        placed_any = true;
                        split = Some(next);
                        break;
                    }
                    SplitStep::NoProgress(state) => {
                        split = Some(state);
                        break;
                    }
                }
            }
            let Some(&(r0, r1)) = groups.get(g) else { break };
            let need_start = t.rows[r0].start_row;
            let ordinal = page_ordinal(f.spread, f.page);
            let fresh = !placed_any
                && if part == 0 {
                    started_at_top
                } else {
                    true
                };
            let new_page = !placed_any
                && if part == 0 {
                    started_at_top
                } else {
                    prev_page != Some(page_key)
                };
            let start_ok = match need_start {
                TableStartRow::Anywhere => true,
                TableStartRow::NextFrame => fresh,
                TableStartRow::NextPage => new_page,
                TableStartRow::NextOddPage => new_page && !ordinal.is_multiple_of(2),
                TableStartRow::NextEvenPage => new_page && ordinal.is_multiple_of(2),
            };
            if !start_ok {
                break;
            }
            let gh: f64 = plan.heights[r0..r1].iter().sum();
            if y + gh + footer_h <= bottom + EPS {
                plan.place_rows(part, r0, r1, y, PieceKind::Body);
                y += gh;
                placed_any = true;
                g = g.saturating_add(1);
                continue;
            }
            let single = r1 == r0.saturating_add(1);
            if !placed_any && single && t.rows[r0].allow_split {
                let next: Vec<(usize, usize)> = (0..t.cells.len())
                    .filter(|&ci| grid.span(ci).0 == r0)
                    .map(|ci| (ci, 0))
                    .collect();
                split = Some(SplitState {
                    row: r0,
                    next,
                    consumed_h: 0.0,
                    pieces: 0,
                });
                continue;
            }
            break;
        }
        if !placed_any {
            // Nothing of the body fits here: drop the part (and its repeated header) and move on.
            plan.parts.pop();
            plan.part_x.pop();
            plan.cells.truncate(cells_before);
            fi = fi.saturating_add(1);
            continue;
        }
        part_bottoms.push(y);
        fi = fi.saturating_add(1);
    }
    // Footers, now that the parts are known.
    let last = plan.parts.len().checked_sub(1);
    #[allow(clippy::needless_range_loop)]
    for k in 0..plan.parts.len() {
        let key = (plan.parts[k].spread, plan.parts[k].page);
        let next_key = plan.parts.get(k.saturating_add(1)).map(|p| (p.spread, p.page));
        let show = !footers.is_empty()
            && !(t.skip_last_footer && Some(k) == last)
            && match t.break_footers {
                TableHeaderBreak::OncePerPage => Some(k) == last || next_key != Some(key),
                _ => true,
            };
        if show {
            let mut y = part_bottoms[k];
            for &r in &footers {
                plan.place_rows(k, r, r.saturating_add(1), y, PieceKind::Footer);
                y += plan.heights[r];
            }
            part_bottoms[k] = y;
        }
    }
    if matches!(
        t.stroke_order,
        TableStrokeOrder::BestJoins | TableStrokeOrder::LegacyCompatibility
    ) {
        plan.notes.push("stroke_order_approximated");
    }
    let strokes = resolve_strokes(&plan, t.stroke_order);
    let end = plan.parts.last().zip(part_bottoms.last()).map(|(p, y)| FlowPosition {
        frame: p.thread_index,
        y_pt: *y,
    });
    let mut frames: Vec<FramePlacement> = plan
        .parts
        .iter()
        .map(|p| FramePlacement {
            frame_id: p.frame_id.clone(),
            frame_index: p.frame_index,
            spread: p.spread,
            page: p.page,
            origin_x_pt: p.origin_x_pt,
            origin_y_pt: p.origin_y_pt,
            lines: Vec::new(),
        })
        .collect();
    let mut cells = Vec::with_capacity(plan.cells.len());
    for piece in plan.cells {
        let c = &t.cells[piece.cell];
        if let Some(fp) = frames.get_mut(piece.part) {
            fp.lines.extend(piece.lines);
        }
        cells.push(PlacedCell {
            cell: piece.cell,
            part: piece.part,
            rect: piece.rect,
            fill: c.fill.clone(),
            fill_tint: c.fill_tint.unwrap_or(100.0),
        });
    }
    let mut notes = plan.notes;
    notes.sort_unstable();
    notes.dedup();
    let overset_text = overset_cells.first().map(|&ci| {
        let r = t.cells[ci].content.map_or((0, 0), |r| (r.start, r.end));
        SourceSpan {
            start: r.0,
            end: r.1,
        }
    });
    Ok(TableProposal {
        base_revision: req.document.revision,
        parts: plan.parts,
        cells,
        strokes,
        text: LayoutProposal {
            base_revision: req.document.revision,
            frames,
            overset: overset_text,
        },
        overset_row,
        overset_cells,
        end,
        notes,
    })
}

enum SplitStep {
    /// The row is finished; the piece height.
    Done(f64),
    /// A piece was placed and more remains.
    More(SplitState, f64),
    /// Not even one line fits the available height.
    NoProgress(SplitState),
}

/// Places the next piece of a split row at `y` with `avail` height (see module docs).
fn place_split(
    plan: &mut Plan<'_>,
    part: usize,
    mut s: SplitState,
    y: f64,
    avail: f64,
) -> SplitStep {
    let row_h = plan.heights[s.row];
    let first_piece = s.pieces == 0;
    let mut pieces: Vec<(usize, Vec<Line>, usize)> = Vec::new();
    let mut any_line = false;
    let mut remaining_lines = false;
    let mut content_h: f64 = 0.0;
    for (ci, next) in &s.next {
        let [top, _, bottom, _] = plan.insets[*ci];
        let lines = &plan.texts[*ci].lines;
        let offset = match lines.get(*next) {
            Some(l) if !first_piece => line_box(l, plan.paragraphs).0 - top,
            _ => 0.0,
        };
        let mut taken = Vec::new();
        let mut k = *next;
        while let Some(l) = lines.get(k) {
            let (_, lb) = line_box(l, plan.paragraphs);
            if lb - offset > avail - bottom + EPS {
                break;
            }
            let mut l2 = l.clone();
            l2.baseline_y_pt -= offset;
            taken.push(l2);
            k = k.saturating_add(1);
        }
        if let Some(l) = taken.last() {
            content_h = content_h.max(line_box(l, plan.paragraphs).1 + bottom);
            any_line = true;
        }
        if k < lines.len() {
            remaining_lines = true;
        }
        pieces.push((*ci, taken, k));
    }
    let blank_left = row_h - s.consumed_h;
    let more = remaining_lines || blank_left > avail + EPS;
    if more && !any_line && avail <= 0.0 + EPS {
        return SplitStep::NoProgress(s);
    }
    if more && !any_line && remaining_lines {
        return SplitStep::NoProgress(s);
    }
    let h = if more { avail } else { content_h.max(blank_left).max(0.0) };
    plan.parts[part].rows.push(PlacedRow {
        row: s.row,
        y_pt: y,
        height_pt: h,
        kind: PieceKind::Split {
            index: s.pieces,
            last: !more,
        },
    });
    for (ci, lines, k) in pieces {
        let rect = plan.cell_rect(part, ci, y, h);
        let lines = lines
            .into_iter()
            .map(|mut l| {
                l.baseline_y_pt += y;
                l.x_start_pt += rect.x0_pt;
                l
            })
            .collect();
        plan.cells.push(CellPiece {
            cell: ci,
            part,
            rect,
            lines,
        });
        if let Some(e) = s.next.iter_mut().find(|e| e.0 == ci) {
            e.1 = k;
        }
    }
    s.consumed_h += h;
    s.pieces = s.pieces.saturating_add(1);
    if more { SplitStep::More(s, h) } else { SplitStep::Done(h) }
}

/// The stroke a cell draws on one edge (0 top, 1 left, 2 bottom, 3 right) and its priority.
fn edge_of(t: &StudioTable, cell: usize, edge: usize) -> Option<(&StudioCellStroke, i32)> {
    let c = &t.cells[cell];
    let p = &c.stroke_priorities;
    let prio = [p.top, p.left, p.bottom, p.right][edge];
    c.strokes.each()[edge].map(|s| (s, prio))
}

/// Winner of a boundary: `a` is the upper/left cell, `b` the lower/right one (ties go to `b`).
fn winner(
    t: &StudioTable,
    a: Option<(usize, usize)>,
    b: Option<(usize, usize)>,
) -> Option<&StudioCellStroke> {
    let sa = a.and_then(|(c, e)| edge_of(t, c, e));
    let sb = b.and_then(|(c, e)| edge_of(t, c, e));
    match (sa, sb) {
        (Some((x, pa)), Some((y, pb))) => Some(if pa > pb { x } else { y }),
        (Some((x, _)), None) => Some(x),
        (None, Some((y, _))) => Some(y),
        (None, None) => None,
    }
}

fn push_segment(
    out: &mut Vec<StrokeSegment>,
    part: usize,
    from: [f64; 2],
    to: [f64; 2],
    horizontal: bool,
    s: &StudioCellStroke,
) {
    let weight = s.weight.value;
    if weight <= 0.0 {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.part == part
        && last.horizontal == horizontal
        && (last.to[0] - from[0]).abs() < EPS
        && (last.to[1] - from[1]).abs() < EPS
        && last.weight_pt == weight
        && last.colour == s.colour
        && last.tint == s.tint
    {
        last.to = to;
        return;
    }
    out.push(StrokeSegment {
        part,
        from,
        to,
        horizontal,
        weight_pt: weight,
        colour: s.colour.clone(),
        tint: s.tint,
    });
}

/// Boundary strokes of every part (module docs), in drawing order.
fn resolve_strokes(plan: &Plan<'_>, order: TableStrokeOrder) -> Vec<StrokeSegment> {
    let t = plan.grid.table;
    let cols = plan.grid.cols;
    let mut horizontal = Vec::new();
    let mut vertical = Vec::new();
    for (pi, part) in plan.parts.iter().enumerate() {
        // Pieces of this part in y order, each with the owner of every column (placed-cell index).
        let mut rows: Vec<&PlacedRow> = part.rows.iter().collect();
        rows.sort_by(|a, b| a.y_pt.total_cmp(&b.y_pt));
        let pieces: Vec<&CellPiece> = plan.cells.iter().filter(|c| c.part == pi).collect();
        // One piece per cell per part (a split row ends its part; headers repeat once per part).
        let by_cell: std::collections::BTreeMap<usize, usize> =
            pieces.iter().enumerate().map(|(i, p)| (p.cell, i)).collect();
        let owner_of = |row: &PlacedRow, col: usize| -> Option<usize> {
            let cell = plan.grid.at(row.row, col)?;
            by_cell.get(&cell).copied()
        };
        let base_x = plan.part_x.get(pi).copied().unwrap_or(0.0);
        let x_of = |c: usize| base_x + plan.grid.col_x[c];
        // Horizontal boundaries: top of the first piece, between pieces, bottom of the last.
        let n = rows.len();
        for b in 0..=n {
            let upper = b.checked_sub(1).and_then(|i| rows.get(i)).copied();
            let lower = rows.get(b).copied();
            let y = match (upper, lower) {
                (_, Some(l)) => l.y_pt,
                (Some(u), None) => u.y_pt + u.height_pt,
                (None, None) => continue,
            };
            for c in 0..cols {
                let ou = upper.and_then(|u| owner_of(u, c));
                let ol = lower.and_then(|l| owner_of(l, c));
                if ou.is_some() && ou == ol {
                    continue;
                }
                let a = ou.map(|i| (pieces[i].cell, 2));
                let bb = ol.map(|i| (pieces[i].cell, 0));
                if let Some(s) = winner(t, a, bb) {
                    let x1 = x_of(c.saturating_add(1));
                    push_segment(&mut horizontal, pi, [x_of(c), y], [x1, y], true, s);
                }
            }
        }
        // Vertical boundaries: per column line, per piece.
        for c in 0..=cols {
            for row in &rows {
                let ol = c.checked_sub(1).and_then(|k| owner_of(row, k));
                let or = if c < cols { owner_of(row, c) } else { None };
                if ol.is_some() && ol == or {
                    continue;
                }
                let a = ol.map(|i| (pieces[i].cell, 3));
                let bb = or.map(|i| (pieces[i].cell, 1));
                if let Some(s) = winner(t, a, bb) {
                    let x = x_of(c);
                    push_segment(
                        &mut vertical,
                        pi,
                        [x, row.y_pt],
                        [x, row.y_pt + row.height_pt],
                        false,
                        s,
                    );
                }
            }
        }
    }
    match order {
        TableStrokeOrder::ColumnOnTop => {
            horizontal.extend(vertical);
            horizontal
        }
        _ => {
            vertical.extend(horizontal);
            vertical
        }
    }
}
