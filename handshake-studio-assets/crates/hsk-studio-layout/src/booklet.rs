//! Booklet options, the two print configurations and the imposition plan (lane SMB-ID-BOOKLET-1;
//! STU-LAY-060, STU-LAY-157). Engine only: a typed, pure plan of which page goes into which slot of
//! which printer spread, and where each page sits on its sheet. Nothing is rendered or printed here.
//!
//! Booklet option numerics (spaces, bleed, creep, margins, in pt) admit finite values >= 0 and
//! nothing else: the spec declares no bounds, so that is the only Studio-declared admission.
//! `margins_uniform_size` makes the top margin govern all four.
//!
//! Studio readings, all UNVERIFIED against InDesign (D5, no oracle case):
//! - `page_range`: comma-separated items, each a 1-based page ordinal `n` or an ascending run `a-b`,
//!   white space around items and around `-` allowed, order kept as written; an empty range is
//!   refused (it does not mean "all pages").
//! - two-up saddle stitch: the selected pages are padded with blanks at the end to a multiple of 4;
//!   sheet k (0 = outermost) of a nest of N positions carries the front spread `[N-2k, 2k+1]` and
//!   the back spread `[2k+2, N-2k-1]`, so the outermost pages pair first (8 pages: 8|1, 2|7, 6|3,
//!   4|5).
//! - two-up perfect bound: consecutive signatures of `signature_size` pages (a positive multiple of
//!   4), each saddle-ordered as its own nest; the last signature holds the remaining pages padded
//!   with blanks to a multiple of 4 (it is not padded to the full signature size).
//! - two-, three- and four-up consecutive: the pages in order, 2, 3 or 4 per printer spread; the last
//!   spread is filled with blanks. No sheet sides and no creep (nothing is nested).
//! - `print_blank_printer_spreads` false omits a printer spread whose every slot is blank (the plan
//!   counts it in `omitted_blank_spreads` and keeps the sequence numbers of the full plan).
//! - creep: linear across the sheets of a nest, 0 on the outermost sheet and the full `creep` on the
//!   innermost (sheet k of S gets `creep * k / (S - 1)`); both pages of the sheet move toward the
//!   fold by that amount. InDesign's creep formula is UNVERIFIED.
//! - sheet geometry: pages side by side from the left margin, `space_between_pages` apart; the
//!   bleed rectangle extends each interior page edge by `min(bleed_between_pages,
//!   space_between_pages / 2)` (outer bleed belongs to the print configuration, not the booklet);
//!   `auto_adjust_margins` centres the pages on the caller's paper (paper required), otherwise the
//!   effective margins are used and a given paper must hold them.
//!
//! Hostile input (V4-01/02): the range text, the selected page count (running aggregate, checked
//! before any allocation) and therefore the spread count are bounded; refusals are typed codes.
#![deny(clippy::arithmetic_side_effects)]
use crate::print::{PrintError, PrintPreferences};
use crate::{LayoutError, Rect};

/// Selected pages of one imposition (a Studio bound for hostile input; matches `MAX_BOOK_PAGES`).
pub const MAX_IMPOSITION_PAGES: u32 = 100_000;
/// Bytes of a `page_range` text (a Studio bound for hostile input).
pub const MAX_PAGE_RANGE_BYTES: usize = 4096;

/// `booklet_type`: exactly the five spec members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookletType {
    TwoUpSaddleStitch,
    TwoUpPerfectBound,
    TwoUpConsecutive,
    ThreeUpConsecutive,
    FourUpConsecutive,
}
impl BookletType {
    pub const ALL: [Self; 5] = [
        Self::TwoUpSaddleStitch,
        Self::TwoUpPerfectBound,
        Self::TwoUpConsecutive,
        Self::ThreeUpConsecutive,
        Self::FourUpConsecutive,
    ];
    pub const fn code(self) -> &'static str {
        match self {
            Self::TwoUpSaddleStitch => "booklet.two_up_saddle_stitch",
            Self::TwoUpPerfectBound => "booklet.two_up_perfect_bound",
            Self::TwoUpConsecutive => "booklet.two_up_consecutive",
            Self::ThreeUpConsecutive => "booklet.three_up_consecutive",
            Self::FourUpConsecutive => "booklet.four_up_consecutive",
        }
    }
    /// Exact member code only: no case folding, trimming or aliasing.
    pub fn from_code(code: &str) -> Result<Self, BookletError> {
        Self::ALL.into_iter().find(|t| t.code() == code).ok_or(BookletError::BookletTypeUnknown)
    }
    /// Page slots per printer spread.
    pub const fn slots(self) -> u32 {
        match self {
            Self::TwoUpSaddleStitch | Self::TwoUpPerfectBound | Self::TwoUpConsecutive => 2,
            Self::ThreeUpConsecutive => 3,
            Self::FourUpConsecutive => 4,
        }
    }
    /// Saddle stitch and perfect bound nest sheets (front and back sides, creep).
    pub const fn nested(self) -> bool {
        matches!(self, Self::TwoUpSaddleStitch | Self::TwoUpPerfectBound)
    }
}

/// Margins in pt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BookletMargins {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

/// The booklet-option record (spec :7072-7083).
#[derive(Clone, Debug, PartialEq)]
pub struct BookletOptions {
    pub booklet_type: BookletType,
    pub page_range: String,
    pub space_between_pages: f64,
    pub bleed_between_pages: f64,
    pub creep: f64,
    /// Pages per signature; used by two-up perfect bound only.
    pub signature_size: u32,
    pub top_margin: f64,
    pub bottom_margin: f64,
    pub left_margin: f64,
    pub right_margin: f64,
    pub margins_uniform_size: bool,
    pub auto_adjust_margins: bool,
    pub print_blank_printer_spreads: bool,
}
impl BookletOptions {
    /// The margins in force: with `margins_uniform_size` the top value governs all four.
    pub fn effective_margins(&self) -> BookletMargins {
        if self.margins_uniform_size {
            let m = self.top_margin;
            BookletMargins { top: m, bottom: m, left: m, right: m }
        } else {
            BookletMargins { top: self.top_margin, bottom: self.bottom_margin, left: self.left_margin, right: self.right_margin }
        }
    }
    /// Numerics finite and >= 0; the range text bounded; for perfect bound a signature size that is
    /// a positive multiple of 4 and at most [`MAX_IMPOSITION_PAGES`].
    pub fn validate(&self) -> Result<(), BookletError> {
        for (name, value) in [
            ("space_between_pages", self.space_between_pages),
            ("bleed_between_pages", self.bleed_between_pages),
            ("creep", self.creep),
            ("top_margin", self.top_margin),
            ("bottom_margin", self.bottom_margin),
            ("left_margin", self.left_margin),
            ("right_margin", self.right_margin),
        ] {
            if !(value.is_finite() && value >= 0.0) {
                return Err(BookletError::ValueInvalid(name));
            }
        }
        if self.page_range.len() > MAX_PAGE_RANGE_BYTES {
            return Err(BookletError::PageRangeTooLong);
        }
        if self.booklet_type == BookletType::TwoUpPerfectBound {
            let s = self.signature_size;
            if s == 0 || !s.is_multiple_of(4) || s > MAX_IMPOSITION_PAGES {
                return Err(BookletError::SignatureSizeInvalid);
            }
        }
        Ok(())
    }
}

/// Which of the two print configurations a refusal belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintConfigurationKind {
    Document,
    Booklet,
}

/// Typed refusals of booklet options and imposition. Codes only; no text is carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookletError {
    BookletTypeUnknown,
    /// A booklet numeric that is NaN, infinite or negative (field name).
    ValueInvalid(&'static str),
    PageRangeEmpty,
    PageRangeMalformed,
    PageRangeTooLong,
    /// A page ordinal past the document's last page.
    PageRangeOutOfDocument,
    SignatureSizeInvalid,
    /// The selected pages exceed [`MAX_IMPOSITION_PAGES`] (checked before allocation).
    PageCountOverBound,
    /// A page or paper size that is not finite and positive, or a sheet that overflows.
    PageSizeInvalid,
    /// `auto_adjust_margins` without a paper size.
    PaperSizeRequired,
    /// The pages and margins do not fit the paper.
    SheetDoesNotFit,
    Print { configuration: PrintConfigurationKind, error: PrintError },
}
impl BookletError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::BookletTypeUnknown => "booklet_type_unknown",
            Self::ValueInvalid(_) => "booklet_value_invalid",
            Self::PageRangeEmpty => "page_range_empty",
            Self::PageRangeMalformed => "page_range_malformed",
            Self::PageRangeTooLong => "page_range_too_long",
            Self::PageRangeOutOfDocument => "page_range_out_of_document",
            Self::SignatureSizeInvalid => "signature_size_invalid",
            Self::PageCountOverBound => "page_count_over_bound",
            Self::PageSizeInvalid => "page_size_invalid",
            Self::PaperSizeRequired => "paper_size_required",
            Self::SheetDoesNotFit => "sheet_does_not_fit",
            Self::Print { error, .. } => error.code(),
        }
    }
    /// The layout refusal class for `report` (one Observe terminal outcome, codes only).
    pub const fn layout_error(self) -> LayoutError {
        match self {
            Self::PageCountOverBound | Self::PageRangeTooLong => LayoutError::BudgetExceeded,
            Self::PageSizeInvalid | Self::SheetDoesNotFit => LayoutError::InvalidGeometry,
            Self::Print { error, .. } => error.layout_error(),
            _ => LayoutError::InvalidInput,
        }
    }
}
impl std::fmt::Display for BookletError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for BookletError {}

/// The booklet's own print configuration: its print preferences plus its booklet options.
#[derive(Clone, Debug, PartialEq)]
pub struct BookletPrintConfiguration {
    pub preferences: PrintPreferences,
    pub options: BookletOptions,
}

/// The two print configurations (spec :7085-7087): the document's and the booklet's, held as
/// separate values so a booklet's paper and marks settings never change the document's. The spec
/// counts 99 properties on the booklet object and 109 on the document object; which ten differ is
/// not stated, so both use [`PrintPreferences`] (UNVERIFIED, D5).
#[derive(Clone, Debug, PartialEq)]
pub struct PrintConfigurations {
    pub document: PrintPreferences,
    pub booklet: BookletPrintConfiguration,
}
impl PrintConfigurations {
    pub fn validate(&self) -> Result<(), BookletError> {
        self.document
            .validate()
            .map_err(|error| BookletError::Print { configuration: PrintConfigurationKind::Document, error })?;
        self.booklet
            .preferences
            .validate()
            .map_err(|error| BookletError::Print { configuration: PrintConfigurationKind::Booklet, error })?;
        self.booklet.options.validate()
    }
}

fn ordinal(text: &str) -> Result<u32, BookletError> {
    let t = text.trim();
    if t.is_empty() || t.len() > 10 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return Err(BookletError::PageRangeMalformed);
    }
    match t.parse::<u32>() {
        Ok(0) | Err(_) => Err(BookletError::PageRangeMalformed),
        Ok(n) => Ok(n),
    }
}

/// Parse `range` against a document of `document_pages` pages into the selected page ordinals in
/// written order. The running count is checked against [`MAX_IMPOSITION_PAGES`] before anything is
/// allocated.
pub fn parse_page_range(range: &str, document_pages: u32) -> Result<Vec<u32>, BookletError> {
    if range.len() > MAX_PAGE_RANGE_BYTES {
        return Err(BookletError::PageRangeTooLong);
    }
    if range.trim().is_empty() {
        return Err(BookletError::PageRangeEmpty);
    }
    let mut runs: Vec<(u32, u32)> = Vec::new();
    let mut count: u32 = 0;
    for item in range.split(',') {
        let (a, b) = match item.split_once('-') {
            Some((a, b)) => (ordinal(a)?, ordinal(b)?),
            None => {
                let n = ordinal(item)?;
                (n, n)
            }
        };
        if b < a {
            return Err(BookletError::PageRangeMalformed);
        }
        if b > document_pages {
            return Err(BookletError::PageRangeOutOfDocument);
        }
        let run = b.checked_sub(a).and_then(|d| d.checked_add(1)).ok_or(BookletError::PageCountOverBound)?;
        count = count.checked_add(run).ok_or(BookletError::PageCountOverBound)?;
        if count > MAX_IMPOSITION_PAGES {
            return Err(BookletError::PageCountOverBound);
        }
        runs.push((a, b));
    }
    let mut pages = Vec::with_capacity(usize::try_from(count).map_err(|_| BookletError::PageCountOverBound)?);
    for (a, b) in runs {
        pages.extend(a..=b);
    }
    Ok(pages)
}

/// Side of a nested sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetSide {
    Front,
    Back,
}

/// One printer spread of the plan.
#[derive(Clone, Debug, PartialEq)]
pub struct PrinterSpread {
    /// Position in the full plan (omitted blank spreads keep their numbers).
    pub sequence: u32,
    /// Signature (nest) index; 0 for saddle stitch and consecutive.
    pub signature: u32,
    /// Sheet index inside the signature (0 = outermost); the spread index for consecutive.
    pub sheet: u32,
    /// `None` for consecutive (nothing is nested).
    pub side: Option<SheetSide>,
    /// Left to right: the document page ordinal of each slot, `None` = blank.
    pub slots: Vec<Option<u32>>,
    /// Creep offset of this sheet toward the fold, pt (0 for consecutive).
    pub creep_offset_pt: f64,
}

/// The imposition plan of one booklet configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct ImpositionPlan {
    pub booklet_type: BookletType,
    /// Pages selected by the range.
    pub selected_pages: u32,
    /// Slot positions after padding with blanks.
    pub positions: u32,
    pub spreads: Vec<PrinterSpread>,
    /// All-blank spreads left out because `print_blank_printer_spreads` is false.
    pub omitted_blank_spreads: u32,
}

const OVER: BookletError = BookletError::PageCountOverBound;

fn round_up_4(n: u32) -> Result<u32, BookletError> {
    n.checked_add(3).and_then(|m| (m / 4).checked_mul(4)).ok_or(OVER)
}

struct Builder<'a> {
    pages: &'a [u32],
    keep_blank: bool,
    sequence: u32,
    spreads: Vec<PrinterSpread>,
    omitted: u32,
}
impl Builder<'_> {
    /// Page of 1-based slot position `p`, or blank past the selection.
    fn at(&self, p: u32) -> Option<u32> {
        let i = usize::try_from(p.checked_sub(1)?).ok()?;
        self.pages.get(i).copied()
    }
    fn push(&mut self, mut spread: PrinterSpread) -> Result<(), BookletError> {
        spread.sequence = self.sequence;
        self.sequence = self.sequence.checked_add(1).ok_or(OVER)?;
        if !self.keep_blank && spread.slots.iter().all(Option::is_none) {
            self.omitted = self.omitted.checked_add(1).ok_or(OVER)?;
        } else {
            self.spreads.push(spread);
        }
        Ok(())
    }
    /// A saddle-ordered nest of `size` positions (a multiple of 4) starting after position `base`.
    fn nest(&mut self, base: u32, size: u32, signature: u32, creep: f64) -> Result<(), BookletError> {
        let sheets = size / 4;
        for k in 0..sheets {
            let k2 = k.checked_mul(2).ok_or(OVER)?;
            let outer = base.checked_add(size).and_then(|v| v.checked_sub(k2)).ok_or(OVER)?;
            let inner = base.checked_add(k2).and_then(|v| v.checked_add(1)).ok_or(OVER)?;
            let offset = match sheets.checked_sub(1) {
                Some(last) if last > 0 => creep * f64::from(k) / f64::from(last),
                _ => 0.0,
            };
            let front = vec![self.at(outer), self.at(inner)];
            let back = vec![
                self.at(inner.checked_add(1).ok_or(OVER)?),
                self.at(outer.checked_sub(1).ok_or(OVER)?),
            ];
            for (side, slots) in [(SheetSide::Front, front), (SheetSide::Back, back)] {
                self.push(PrinterSpread { sequence: 0, signature, sheet: k, side: Some(side), slots, creep_offset_pt: offset })?;
            }
        }
        Ok(())
    }
}

/// Arrange the range's pages into printer spreads for folding (STU-LAY-060), per the readings in
/// the module documentation. Options are validated first.
pub fn impose(options: &BookletOptions, document_pages: u32) -> Result<ImpositionPlan, BookletError> {
    options.validate()?;
    let pages = parse_page_range(&options.page_range, document_pages)?;
    let n = u32::try_from(pages.len()).map_err(|_| OVER)?;
    let mut b = Builder { pages: &pages, keep_blank: options.print_blank_printer_spreads, sequence: 0, spreads: Vec::new(), omitted: 0 };
    let positions = match options.booklet_type {
        BookletType::TwoUpSaddleStitch => {
            let size = round_up_4(n)?;
            b.nest(0, size, 0, options.creep)?;
            size
        }
        BookletType::TwoUpPerfectBound => {
            let (mut base, mut signature) = (0u32, 0u32);
            while base < n {
                let remaining = n.checked_sub(base).ok_or(OVER)?;
                let size = options.signature_size.min(round_up_4(remaining)?);
                b.nest(base, size, signature, options.creep)?;
                base = base.checked_add(size).ok_or(OVER)?;
                signature = signature.checked_add(1).ok_or(OVER)?;
            }
            base
        }
        t => {
            let k = t.slots();
            let (mut start, mut index) = (0u32, 0u32);
            while start < n {
                let mut slots = Vec::with_capacity(usize::try_from(k).map_err(|_| OVER)?);
                for j in 1..=k {
                    slots.push(b.at(start.checked_add(j).ok_or(OVER)?));
                }
                b.push(PrinterSpread { sequence: 0, signature: 0, sheet: index, side: None, slots, creep_offset_pt: 0.0 })?;
                start = start.checked_add(k).ok_or(OVER)?;
                index = index.checked_add(1).ok_or(OVER)?;
            }
            start
        }
    };
    Ok(ImpositionPlan { booklet_type: options.booklet_type, selected_pages: n, positions, spreads: b.spreads, omitted_blank_spreads: b.omitted })
}

/// Paper size in pt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaperSize {
    pub width_pt: f64,
    pub height_pt: f64,
}

/// Inputs of the sheet geometry: the document page size and, optionally, the paper.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SheetRequest {
    pub page_width_pt: f64,
    pub page_height_pt: f64,
    pub paper: Option<PaperSize>,
}

/// One page slot placed on its sheet (sheet coordinates: origin top-left, y down, pt).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedPage {
    pub slot: u32,
    pub page: Option<u32>,
    pub trim: Rect,
    pub bleed: Rect,
}

/// The placement of one printer spread on its sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetLayout {
    /// `PrinterSpread::sequence`.
    pub sequence: u32,
    pub width_pt: f64,
    pub height_pt: f64,
    /// Fold line of a nested two-up sheet.
    pub fold_x_pt: Option<f64>,
    pub creep_offset_pt: f64,
    pub pages: Vec<PlacedPage>,
}

fn positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

/// Page placement rectangles for every printer spread of `plan`, per the readings in the module
/// documentation. The output is a typed plan only.
pub fn sheet_geometry(plan: &ImpositionPlan, options: &BookletOptions, request: &SheetRequest) -> Result<Vec<SheetLayout>, BookletError> {
    options.validate()?;
    let (w, h) = (request.page_width_pt, request.page_height_pt);
    if !(positive(w) && positive(h)) {
        return Err(BookletError::PageSizeInvalid);
    }
    if let Some(p) = request.paper
        && !(positive(p.width_pt) && positive(p.height_pt))
    {
        return Err(BookletError::PageSizeInvalid);
    }
    let k = plan.booklet_type.slots();
    let gaps = f64::from(k.checked_sub(1).ok_or(OVER)?);
    let space = options.space_between_pages;
    let content_w = f64::from(k) * w + gaps * space;
    let (left, top, sheet_w, sheet_h) = if options.auto_adjust_margins {
        let p = request.paper.ok_or(BookletError::PaperSizeRequired)?;
        let (mx, my) = ((p.width_pt - content_w) / 2.0, (p.height_pt - h) / 2.0);
        if !(mx >= 0.0 && my >= 0.0) {
            return Err(BookletError::SheetDoesNotFit);
        }
        (mx, my, p.width_pt, p.height_pt)
    } else {
        let m = options.effective_margins();
        let (need_w, need_h) = (m.left + content_w + m.right, m.top + h + m.bottom);
        match request.paper {
            Some(p) if need_w > p.width_pt || need_h > p.height_pt => return Err(BookletError::SheetDoesNotFit),
            Some(p) => (m.left, m.top, p.width_pt, p.height_pt),
            None => (m.left, m.top, need_w, need_h),
        }
    };
    if !(sheet_w.is_finite() && sheet_h.is_finite()) {
        return Err(BookletError::PageSizeInvalid);
    }
    let bleed = options.bleed_between_pages.min(space / 2.0);
    let nested = plan.booklet_type.nested();
    let fold = nested.then_some(left + w + space / 2.0);
    let mut sheets = Vec::with_capacity(plan.spreads.len());
    for spread in &plan.spreads {
        let creep = if nested { spread.creep_offset_pt } else { 0.0 };
        let mut pages = Vec::with_capacity(spread.slots.len());
        for (i, page) in spread.slots.iter().enumerate() {
            let slot = u32::try_from(i).map_err(|_| OVER)?;
            let shift = match (nested, slot) {
                (true, 0) => creep,
                (true, _) => -creep,
                _ => 0.0,
            };
            let x0 = left + f64::from(slot) * (w + space) + shift;
            let trim = Rect { x0_pt: x0, y0_pt: top, x1_pt: x0 + w, y1_pt: top + h };
            let before = if slot > 0 { bleed } else { 0.0 };
            let after = if slot.checked_add(1).ok_or(OVER)? < k { bleed } else { 0.0 };
            let bleed_rect = Rect { x0_pt: trim.x0_pt - before, y0_pt: trim.y0_pt, x1_pt: trim.x1_pt + after, y1_pt: trim.y1_pt };
            pages.push(PlacedPage { slot, page: *page, trim, bleed: bleed_rect });
        }
        sheets.push(SheetLayout { sequence: spread.sequence, width_pt: sheet_w, height_pt: sheet_h, fold_x_pt: fold, creep_offset_pt: creep, pages });
    }
    Ok(sheets)
}
