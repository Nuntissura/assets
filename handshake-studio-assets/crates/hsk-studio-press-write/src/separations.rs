//! Separations preview (lane SMB-ID-PREPRESS; STU-LAY-160 separations preview, STU-COL-212,
//! STU-LAY-250 total ink coverage). Content is separated into one coverage plane per ink (the four
//! process inks first, then every spot ink in order of first use), each pixel holding the ink's
//! tone 0..=1. Everything the preview shows is readable as typed values: per-ink readouts at a point
//! ([`SeparationPreview::sample_pt`]), per-ink coverage ([`SeparationPreview::coverage`]), the
//! ink-limit view against the three independent limits of STU-LAY-250
//! ([`SeparationPreview::check_ink_limits`]) and per-plate on/off renders ([`SeparationPreview::render`]).
//! The total ink coverage CONTRACT (declaration sites, check states, the structured finding of
//! STU-LAY-250..254) evaluates these same planes in [`crate::coverage`].
//!
//! Total ink at a location (STU-LAY-252, opaque ink handling): every plate carries the declared
//! `ink_type` of its ink. Normal and transparent inks add their tone; opaque and opaque-ignore inks
//! obscure what lies beneath them, so with `o` the highest obscuring tone at the location the total is
//! `sum(obscuring tones) + (1 - o) x sum(other tones)`. The ink manager declares no print sequence, so
//! an obscuring ink is taken to lie over every non-obscuring ink at the location. With only normal and
//! transparent inks this is the plain sum. Vendor parity of this rule is UNVERIFIED (D5).
//!
//! Provenance: every painted object is kept as a bounded record (device outline, clip, plate tones,
//! overprint flag, the coverage input stages its paint went through, and its object reference), so
//! the coverage contract can replay the composition at one location exactly (same 4 x 4 sampling,
//! same f32 compositing) to name the composing objects and the stage that raised a value. The record
//! bytes are charged to [`SepLimits::max_bytes`] with the planes, before the planes change.
//!
//! Painting model (clean-room from ISO 32000-1 8.6.6 / 11.7.4 overprint semantics): a knockout object
//! sets every plate under its coverage `a` to `old (1 - a) + a v` with `v = 0` for inks it does not
//! use; an overprinting object changes only the plates of the inks it specifies (a DeviceCMYK colour
//! specifies all four process inks; with nonzero overprint mode, only its nonzero ones). Colours
//! separate as: DeviceCMYK -> C, M, Y, K; DeviceGray g -> K = 1 - g; Separation `All` -> every plate
//! at the tint; Separation `None` -> nothing; another Separation -> the ink manager's target for it
//! (its own plate, an aliased plate, or process through its CMYK alternate); mixed inks -> each part
//! at part tint x tint. RGB, Lab and ICC-based colours need a colour engine: a caller supplies a
//! [`ProcessConverter`] (e.g. one over prism), otherwise the object is not separated and counted
//! ([`SepNote::Unseparated`]). Not separated, each noted: shadings, transparency (groups are painted
//! opaque, [`SepNote::TransparencyFlattenedOpaque`]), overprint flags of PDF input (hayro does not
//! report them; PDF objects knock out, [`SepNote::PdfOverprintNotRead`]).
//!
//! Coverage: 4 x 4 samples per pixel (sample centres), non-zero or even-odd, curves flattened at
//! 0.1 px, strokes expanded by kurbo (hairlines widened to 1 px). Bounds (V4-01/V4-02): canvas pixels
//! and the bytes of every plane and clip mask are checked against [`SepLimits`] BEFORE allocation
//! (a new spot plane included), path segments and sample work are capped. InDesign/Acrobat parity of
//! the preview colours and of the tone rounding is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::coverage::{CoverageInputStage, ObjectRef};
use crate::inks::{InkError, InkManager, InkTarget, InkType, MixedInk, ProcessInk};
use crate::{VectorColour, VectorEl, VectorItem, VectorMode, VectorPage, VectorPaint, VectorSpace};
use hsk_studio_accord::CancellationToken;
use kurbo::{BezPath, PathEl, Point};
use std::collections::BTreeSet;

/// Samples per pixel axis.
const SS: usize = 4;
/// Flattening tolerance, device pixels.
const TOL: f64 = 0.1;
/// Deepest clip nesting.
pub const MAX_SEP_CLIP_DEPTH: usize = 16;
/// Most plates (4 process + spots).
pub const MAX_SEP_PLATES: usize = 64;

/// Caller bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SepLimits {
    /// Canvas pixels.
    pub max_pixels: u64,
    /// Bytes of all planes and clip masks together.
    pub max_bytes: u64,
    /// Path segments of one object after flattening.
    pub max_segments: usize,
    /// Sample spans visited over the whole page.
    pub max_work: u64,
}
impl Default for SepLimits {
    fn default() -> Self {
        Self { max_pixels: 16 << 20, max_bytes: 1 << 30, max_segments: 1 << 20, max_work: 1 << 32 }
    }
}

/// Typed refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SepError {
    /// Non-finite or empty page box, non-positive resolution.
    InvalidInput,
    /// Canvas over `max_pixels` (checked before allocation).
    CanvasTooLarge,
    /// Planes/clips over `max_bytes`, segments or work over their caps.
    Budget,
    TooManyPlates,
    ClipTooDeep,
    Ink(InkError),
    Canceled,
}
impl SepError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::CanvasTooLarge => "canvas_too_large",
            Self::Budget => "budget_exceeded",
            Self::TooManyPlates => "too_many_plates",
            Self::ClipTooDeep => "clip_too_deep",
            Self::Ink(e) => e.code(),
            Self::Canceled => "canceled",
        }
    }
}
impl From<InkError> for SepError {
    fn from(e: InkError) -> Self {
        Self::Ink(e)
    }
}

/// What the preview left out or changed (typed, not silent).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SepNote {
    /// Objects in RGB/Lab/ICC (or without a source colour) not separated: no converter.
    Unseparated,
    ShadingNotSeparated,
    TransparencyFlattenedOpaque,
    PdfOverprintNotRead,
    /// A spot converted to process has no CMYK alternate: it stayed on its own plate.
    SpotWithoutAlternate(String),
}

/// What an object prints with.
#[derive(Clone, Debug, PartialEq)]
pub enum SepPaint {
    /// CMYK 0..=1.
    Process([f64; 4]),
    Spot { name: String, tint: f64 },
    Mixed { ink: MixedInk, tint: f64 },
    /// Every plate (PDF Separation `All`).
    Registration(f64),
    /// Nothing (PDF Separation `None`).
    NoInk,
}

/// Converts a non-device colour to CMYK for separation (a colour engine port).
pub trait ProcessConverter {
    fn to_cmyk(&self, space: &VectorSpace, components: &[f64]) -> Option<[f64; 4]>;
}

/// A plate of the preview.
#[derive(Clone, Debug, PartialEq)]
pub struct Plate {
    pub name: String,
    pub process: Option<ProcessInk>,
    /// Display colour (RGB 0..=1) of the ink at full tone.
    pub display_rgb: [f64; 3],
    /// The declared `ink_type` of the ink this plate prints (STU-LAY-160; normal when the ink manager
    /// does not list the ink).
    pub ink_type: InkType,
}

/// One ink's tone at a point.
#[derive(Clone, Debug, PartialEq)]
pub struct InkReading {
    pub ink: String,
    pub percent: f64,
}

/// Per-ink coverage over the canvas.
#[derive(Clone, Debug, PartialEq)]
pub struct PlateCoverage {
    pub ink: String,
    pub max_percent: f64,
    /// Mean tone over all canvas pixels.
    pub mean_percent: f64,
    /// Pixels with any of this ink.
    pub inked_pixels: u64,
}

/// STU-LAY-250: three independent limits (percent) plus the warning margin; `None` = undeclared.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InkLimits {
    pub total_ink_limit: Option<f64>,
    pub single_ink_limit: Option<f64>,
    pub black_ink_limit: Option<f64>,
    pub coverage_warning_margin: Option<f64>,
}

/// Which limit a finding is about.
#[derive(Clone, Debug, PartialEq)]
pub enum LimitKind {
    Total,
    Single(String),
    Black,
}

/// The preflight severity vocabulary of STU-LAY-154, reused by `ink_limit_severity` (STU-LAY-251):
/// exactly four members, no second scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Disabled,
    Error,
    Warning,
    Informational,
}
impl Severity {
    pub const ALL: [Severity; 4] = [Self::Disabled, Self::Error, Self::Warning, Self::Informational];
    pub const fn code(self) -> &'static str {
        match self {
            Self::Disabled => "severity.disabled",
            Self::Error => "severity.error",
            Self::Warning => "severity.warning",
            Self::Informational => "severity.informational",
        }
    }
    /// The member with this exact token; anything else is `None` (never mapped onto a near member).
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.code() == code)
    }
    /// The severity a finding inside the warning margin is raised at (STU-LAY-250: "a lower
    /// severity"): error -> warning -> informational; informational has no lower member and stays;
    /// a disabled rule is not evaluated, so it stays disabled.
    pub const fn lowered(self) -> Self {
        match self {
            Self::Disabled => Self::Disabled,
            Self::Error => Self::Warning,
            Self::Warning | Self::Informational => Self::Informational,
        }
    }
}

/// One structured ink-limit finding.
#[derive(Clone, Debug, PartialEq)]
pub struct InkLimitFinding {
    pub limit: LimitKind,
    pub severity: Severity,
    pub limit_percent: f64,
    /// Pixels in this class.
    pub pixels: u64,
    pub max_percent: f64,
    /// First pixel (row-major) in this class.
    pub first_px: (u32, u32),
}

/// Result of an ink-limit check.
#[derive(Clone, Debug, PartialEq)]
pub struct InkLimitReport {
    /// Structural maximum of the total for this separation: 100 x ink count (STU-LAY-250).
    pub structural_max_percent: f64,
    pub max_total_percent: f64,
    pub findings: Vec<InkLimitFinding>,
}

/// What [`SeparationPreview::render`] shows.
#[derive(Clone, Debug, PartialEq)]
pub enum PlateView {
    /// Composite of the plates whose flag is true (missing flags = shown).
    Composite(Vec<bool>),
    /// One plate in grey (tone 1 = black).
    Single(usize),
    /// Pixels whose total exceeds the threshold percent in red over a grey composite.
    InkLimit(f64),
}

/// The separated page.
#[derive(Clone, Debug, PartialEq)]
pub struct SeparationPreview {
    pub width: u32,
    pub height: u32,
    pub px_per_pt: f64,
    /// Page box the canvas covers (PDF user space, points).
    pub page_box: [f64; 4],
    pub plates: Vec<Plate>,
    planes: Vec<Vec<f32>>,
    pub notes: BTreeSet<SepNote>,
    /// Objects not separated (see [`SepNote::Unseparated`]).
    pub unseparated: u64,
    /// Painted objects in paint order (provenance for the coverage contract).
    objects: Vec<SepObject>,
    /// Clip outlines (an arena; objects point at the innermost clip in force).
    clip_nodes: Vec<ClipNode>,
    /// The caps the separation ran under, reused by the coverage replay.
    max_segments: usize,
    max_work: u64,
}

/// One painted object, kept for the coverage replay.
#[derive(Clone, Debug, PartialEq)]
struct SepObject {
    reference: ObjectRef,
    /// Device-space fill outline (strokes already expanded).
    outline: BezPath,
    even_odd: bool,
    /// Inclusive device pixel bounds of the pixels it changed: (x0, y0, x1, y1).
    bounds: (usize, usize, usize, usize),
    clip: Option<usize>,
    /// (plate, tone) pairs the object writes, exactly as composited.
    tones: Vec<(usize, f32)>,
    overprint: bool,
    /// Bit set of [`CoverageInputStage::bit`] its paint went through.
    stages: u8,
}

#[derive(Clone, Debug, PartialEq)]
struct ClipNode {
    outline: BezPath,
    even_odd: bool,
    parent: Option<usize>,
}

/// Fixed bytes charged per object or clip record besides its outline and tones.
const RECORD_BYTES: u64 = 128;

/// One object to separate.
#[derive(Clone, Debug, PartialEq)]
pub struct SepDraw<'a> {
    pub path: &'a [VectorEl],
    pub mode: &'a VectorMode,
    pub paint: SepPaint,
    pub overprint: bool,
    /// Nonzero overprint mode (OPM 1): zero process components of an overprinting CMYK colour
    /// leave their plates unchanged.
    pub overprint_nonzero: bool,
}

/// Builds a separation object by object (paint order).
pub struct Separator<'i> {
    inks: &'i InkManager,
    limits: SepLimits,
    out: SeparationPreview,
    clips: Vec<Vec<f32>>,
    /// The innermost clip node in force.
    clip_at: Option<usize>,
    /// Objects drawn through [`Separator::draw`] (their default reference).
    drawn: u64,
    bytes: u64,
    work: u64,
    pixels: usize,
}

fn cmyk_rgb(c: [f64; 4]) -> [f64; 3] {
    let k = 1.0 - c[3];
    [(1.0 - c[0]) * k, (1.0 - c[1]) * k, (1.0 - c[2]) * k]
}

/// A flattened edge: (x at the top, top y, x at the bottom, bottom y, winding).
type Seg = (f64, f64, f64, f64, i32);

/// The non-horizontal edges of `path` flattened at [`TOL`]; more than `cap` edges is `Budget`.
fn segments(path: &BezPath, cap: usize) -> Result<Vec<Seg>, SepError> {
    let mut segs: Vec<Seg> = Vec::new();
    let (mut start, mut last, mut over) = (Point::ZERO, Point::ZERO, false);
    let mut push = |segs: &mut Vec<Seg>, a: Point, b: Point| {
        if segs.len() >= cap {
            over = true;
        } else if a.y != b.y && a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite() {
            segs.push(if a.y < b.y { (a.x, a.y, b.x, b.y, 1) } else { (b.x, b.y, a.x, a.y, -1) });
        }
    };
    kurbo::flatten(path.elements().iter().copied(), TOL, |el| match el {
        PathEl::MoveTo(p) => {
            push(&mut segs, last, start);
            start = p;
            last = p;
        }
        PathEl::LineTo(p) | PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p) => {
            push(&mut segs, last, p);
            last = p;
        }
        PathEl::ClosePath => {
            push(&mut segs, last, start);
            last = start;
        }
    });
    push(&mut segs, last, start);
    if over {
        return Err(SepError::Budget);
    }
    Ok(segs)
}

/// Calls `span(lo, hi)` for every inside run of sample columns `[lo, hi)` (lo < hi) on each of the
/// [`SS`] sample rows of pixel row `row` (sample centres, non-zero or even-odd).
fn row_spans(
    segs: &[Seg],
    row: usize,
    samples_w: usize,
    even_odd: bool,
    xs: &mut Vec<(f64, i32)>,
    mut span: impl FnMut(usize, usize) -> Result<(), SepError>,
) -> Result<(), SepError> {
    for sub in 0..SS {
        let y = row as f64 + (sub as f64 + 0.5) / SS as f64;
        xs.clear();
        for s in segs {
            if s.1 <= y && y < s.3 {
                xs.push((s.0 + (y - s.1) * (s.2 - s.0) / (s.3 - s.1), s.4));
            }
        }
        xs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut wind = 0i32;
        for k in 0..xs.len() {
            wind = wind.wrapping_add(xs[k].1);
            let inside = if even_odd { k % 2 == 0 } else { wind != 0 };
            let Some(next) = xs.get(k.wrapping_add(1)) else { break };
            if !inside {
                continue;
            }
            // Samples j with (j + 0.5) / SS in [xa, xb).
            let lo = (xs[k].0 * SS as f64 - 0.5).ceil().max(0.0);
            let hi = (next.0 * SS as f64 - 0.5).ceil().min(samples_w as f64);
            if lo >= hi {
                continue;
            }
            span(lo as usize, hi as usize)?;
        }
    }
    Ok(())
}

/// Coverage (0..=1) of `path` at the one pixel (x, y): the same samples and the same arithmetic as
/// [`Separator::cover`], so a replayed value equals the painted one. `caps` = (canvas width, segment
/// cap, work cap); `work` counts visited spans.
fn pixel_cover(path: &BezPath, even_odd: bool, x: usize, y: usize, caps: (usize, usize, u64), work: &mut u64) -> Result<f32, SepError> {
    let (w, cap, max_work) = caps;
    let segs = segments(path, cap)?;
    let mut xs: Vec<(f64, i32)> = Vec::new();
    let c0 = x.saturating_mul(SS);
    let c1 = c0.saturating_add(SS);
    let mut count = 0u8;
    row_spans(&segs, y, w.saturating_mul(SS), even_odd, &mut xs, |lo, hi| {
        *work = work.saturating_add(1);
        if *work > max_work {
            return Err(SepError::Budget);
        }
        let (a, b) = (lo.max(c0), hi.min(c1));
        if a < b {
            count = count.saturating_add(u8::try_from(b.saturating_sub(a)).unwrap_or(u8::MAX));
        }
        Ok(())
    })?;
    Ok(f32::from(count) / (SS * SS) as f32)
}

/// Total ink of one location from per-plate tones (0..=1), percent: normal and transparent inks add,
/// opaque and opaque-ignore inks obscure the others (module docs, STU-LAY-252).
fn mixed_total(plates: &[Plate], tone: impl Fn(usize) -> f64) -> f64 {
    let (mut obscuring, mut top, mut other) = (0.0f64, 0.0f64, 0.0f64);
    for (p, plate) in plates.iter().enumerate() {
        let t = tone(p);
        if plate.ink_type.obscures() {
            obscuring += t;
            top = top.max(t);
        } else {
            other += t;
        }
    }
    (obscuring + other * (1.0 - top)) * 100.0
}

impl<'i> Separator<'i> {
    /// Canvas over `page_box` (points, PDF user space) at `px_per_pt`; the four process planes are
    /// allocated after the pixel and byte checks.
    pub fn new(page_box: [f64; 4], px_per_pt: f64, inks: &'i InkManager, limits: SepLimits) -> Result<Self, SepError> {
        inks.validate()?;
        let [x0, y0, x1, y1] = page_box;
        let (w, h) = ((x1 - x0) * px_per_pt, (y1 - y0) * px_per_pt);
        if !(page_box.iter().all(|v| v.is_finite()) && px_per_pt.is_finite() && px_per_pt > 0.0 && w >= 1.0 && h >= 1.0) {
            return Err(SepError::InvalidInput);
        }
        if w * h > limits.max_pixels as f64 || w > f64::from(u32::MAX) || h > f64::from(u32::MAX) {
            return Err(SepError::CanvasTooLarge);
        }
        let (width, height) = (w.ceil() as u32, h.ceil() as u32);
        let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or(SepError::CanvasTooLarge)?;
        if pixels > limits.max_pixels {
            return Err(SepError::CanvasTooLarge);
        }
        let pixels = usize::try_from(pixels).map_err(|_| SepError::CanvasTooLarge)?;
        let mut s = Self {
            inks,
            limits,
            out: SeparationPreview {
                width,
                height,
                px_per_pt,
                page_box,
                plates: Vec::new(),
                planes: Vec::new(),
                notes: BTreeSet::new(),
                unseparated: 0,
                objects: Vec::new(),
                clip_nodes: Vec::new(),
                max_segments: limits.max_segments,
                max_work: limits.max_work,
            },
            clips: Vec::new(),
            clip_at: None,
            drawn: 0,
            bytes: 0,
            work: 0,
            pixels,
        };
        for p in ProcessInk::ALL {
            s.add_plate(Plate {
                name: p.name().into(),
                process: Some(p),
                ink_type: inks.ink_type(p.name()).unwrap_or_default(),
                display_rgb: match p {
                    ProcessInk::Cyan => [0.0, 1.0, 1.0],
                    ProcessInk::Magenta => [1.0, 0.0, 1.0],
                    ProcessInk::Yellow => [1.0, 1.0, 0.0],
                    ProcessInk::Black => [0.0, 0.0, 0.0],
                },
            })?;
        }
        Ok(s)
    }

    fn charge(&mut self, b: u64) -> Result<(), SepError> {
        self.bytes = self.bytes.checked_add(b).filter(|v| *v <= self.limits.max_bytes).ok_or(SepError::Budget)?;
        Ok(())
    }

    fn charge_plane(&mut self) -> Result<Vec<f32>, SepError> {
        let b = u64::try_from(self.pixels).ok().and_then(|p| p.checked_mul(4)).ok_or(SepError::Budget)?;
        self.charge(b)?;
        Ok(vec![0.0; self.pixels])
    }

    /// Bytes of a kept record: its outline, `extra` items of `item` bytes each and the fixed share.
    fn record_bytes(outline: &BezPath, extra: usize, item: usize) -> Result<u64, SepError> {
        let path = outline.elements().len().checked_mul(std::mem::size_of::<PathEl>()).ok_or(SepError::Budget)?;
        let rest = extra.checked_mul(item).ok_or(SepError::Budget)?;
        let total = path.checked_add(rest).ok_or(SepError::Budget)?;
        u64::try_from(total).ok().and_then(|t| t.checked_add(RECORD_BYTES)).ok_or(SepError::Budget)
    }

    fn add_plate(&mut self, plate: Plate) -> Result<usize, SepError> {
        if self.out.plates.len() >= MAX_SEP_PLATES {
            return Err(SepError::TooManyPlates);
        }
        let plane = self.charge_plane()?;
        self.out.plates.push(plate);
        self.out.planes.push(plane);
        Ok(self.out.plates.len().saturating_sub(1))
    }

    fn plate_of(&mut self, name: &str) -> Result<usize, SepError> {
        if let Some(i) = self.out.plates.iter().position(|p| p.name == name) {
            return Ok(i);
        }
        let alt = self.inks.ink(name).and_then(|i| i.process_alternate);
        let ink_type = self.inks.ink_type(name).unwrap_or_default();
        self.add_plate(Plate { name: name.into(), process: None, display_rgb: alt.map_or([0.5, 0.5, 0.5], cmyk_rgb), ink_type })
    }

    /// Plate tones of a paint: (plate, tone) pairs; `stages` collects the coverage input stages the
    /// paint went through (STU-LAY-252).
    fn tones(&mut self, paint: &SepPaint, nonzero_only: bool, stages: &mut u8) -> Result<Vec<(usize, f64)>, SepError> {
        let clamp = |v: f64| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        let mut out: Vec<(usize, f64)> = Vec::new();
        let mut add = |out: &mut Vec<(usize, f64)>, plate: usize, v: f64| match out.iter_mut().find(|(p, _)| *p == plate) {
            Some((_, t)) => *t = clamp(*t + v),
            None => out.push((plate, clamp(v))),
        };
        match paint {
            SepPaint::NoInk => {}
            SepPaint::Process(c) => {
                // An authored rich black: black plus at least one of cyan, magenta, yellow.
                if c[3] > 0.0 && c[..3].iter().any(|v| *v > 0.0) {
                    *stages |= CoverageInputStage::AuthoredRichBlack.bit();
                }
                for (i, v) in c.iter().enumerate() {
                    if !(nonzero_only && *v <= 0.0) {
                        add(&mut out, i, *v);
                    }
                }
            }
            SepPaint::Registration(t) => {
                // A registration swatch is hand-typed like a rich black.
                *stages |= CoverageInputStage::AuthoredRichBlack.bit();
                for i in 0..self.out.plates.len() {
                    add(&mut out, i, *t);
                }
            }
            SepPaint::Spot { name, tint } => self.spot_tones(name, clamp(*tint), &mut out, &mut add, stages)?,
            SepPaint::Mixed { ink, tint } => {
                ink.validate(self.inks)?;
                for (name, part) in &ink.parts {
                    self.spot_tones(name, clamp(*tint) * clamp(*part), &mut out, &mut add, stages)?;
                }
            }
        }
        Ok(out)
    }

    fn spot_tones(
        &mut self,
        name: &str,
        tint: f64,
        out: &mut Vec<(usize, f64)>,
        add: &mut impl FnMut(&mut Vec<(usize, f64)>, usize, f64),
        stages: &mut u8,
    ) -> Result<(), SepError> {
        if let Some(p) = ProcessInk::ALL.iter().find(|p| p.name() == name) {
            add(out, p.index(), tint);
            return Ok(());
        }
        if self.inks.is_aliased(name) {
            *stages |= CoverageInputStage::InkAliasResolution.bit();
        }
        match self.inks.target(name)? {
            InkTarget::Plate(n) => {
                let plate = match ProcessInk::ALL.iter().find(|p| p.name() == n) {
                    Some(p) => p.index(),
                    None => self.plate_of(&n)?,
                };
                add(out, plate, tint);
            }
            InkTarget::Process(c) => {
                *stages |= CoverageInputStage::SpotToProcessConversion.bit();
                for (i, v) in c.iter().enumerate() {
                    add(out, i, v * tint);
                }
            }
            InkTarget::PlateNoAlternate(n) => {
                self.out.notes.insert(SepNote::SpotWithoutAlternate(n.clone()));
                let plate = self.plate_of(&n)?;
                add(out, plate, tint);
            }
        }
        Ok(())
    }

    /// Device-space path of `path` (y down, origin at the page box top-left).
    fn device(&self, path: &[VectorEl]) -> BezPath {
        let [x0, _, _, y1] = self.out.page_box;
        let s = self.out.px_per_pt;
        let m = |p: [f64; 2]| Point::new((p[0] - x0) * s, (y1 - p[1]) * s);
        let mut b = BezPath::new();
        for el in path {
            match el {
                VectorEl::MoveTo(p) => b.move_to(m(*p)),
                VectorEl::LineTo(p) => b.line_to(m(*p)),
                VectorEl::CubicTo(a, c, p) => b.curve_to(m(*a), m(*c), m(*p)),
                VectorEl::Close => b.close_path(),
            }
        }
        b
    }

    /// Coverage (0..=1) of `path` per canvas pixel, sparse rows: calls `f(index, coverage)`.
    fn cover(&mut self, path: &BezPath, even_odd: bool, mut f: impl FnMut(usize, f32)) -> Result<(), SepError> {
        let (w, h) = (self.out.width as usize, self.out.height as usize);
        let segs = segments(path, self.limits.max_segments)?;
        if segs.is_empty() {
            return Ok(());
        }
        let ymin = segs.iter().map(|s| s.1).fold(f64::INFINITY, f64::min).max(0.0);
        let ymax = segs.iter().map(|s| s.3).fold(f64::NEG_INFINITY, f64::max).min(h as f64);
        if ymin >= ymax {
            return Ok(());
        }
        let (row0, row1) = (ymin.floor() as usize, (ymax.ceil() as usize).min(h));
        let mut counts = vec![0u8; w];
        let mut xs: Vec<(f64, i32)> = Vec::new();
        let samples_w = w.saturating_mul(SS);
        let max_work = self.limits.max_work;
        for row in row0..row1 {
            counts.iter_mut().for_each(|c| *c = 0);
            let mut touched = false;
            let work = &mut self.work;
            row_spans(&segs, row, samples_w, even_odd, &mut xs, |lo, hi| {
                *work = work.saturating_add(hi.saturating_sub(lo) as u64);
                if *work > max_work {
                    return Err(SepError::Budget);
                }
                for j in lo..hi {
                    let c = &mut counts[j / SS];
                    *c = c.saturating_add(1);
                }
                touched = true;
                Ok(())
            })?;
            if touched {
                let base = row.saturating_mul(w);
                for (x, c) in counts.iter().enumerate() {
                    if *c > 0 {
                        f(base.saturating_add(x), f32::from(*c) / (SS * SS) as f32);
                    }
                }
            }
        }
        Ok(())
    }

    /// The fill outline of a draw in device space.
    fn outline(&self, path: &[VectorEl], mode: &VectorMode) -> (BezPath, bool) {
        let dev = self.device(path);
        match mode {
            VectorMode::Fill { even_odd } => (dev, *even_odd),
            VectorMode::Stroke(st) => {
                let s = self.out.px_per_pt;
                let width = (st.width.abs() * s).max(1.0);
                let mut stroke = kurbo::Stroke::new(if width.is_finite() { width } else { 1.0 })
                    .with_caps(match st.cap {
                        1 => kurbo::Cap::Round,
                        2 => kurbo::Cap::Square,
                        _ => kurbo::Cap::Butt,
                    })
                    .with_join(match st.join {
                        1 => kurbo::Join::Round,
                        2 => kurbo::Join::Bevel,
                        _ => kurbo::Join::Miter,
                    })
                    .with_miter_limit(if st.miter_limit.is_finite() { st.miter_limit.max(1.0) } else { 10.0 });
                let period: f64 = st.dash.iter().map(|d| d.abs() * s).sum();
                let span = dev.control_box();
                let len = span.width() + span.height();
                if period.is_finite() && period >= 0.5 && len.is_finite() && len / period <= 100_000.0 {
                    stroke = stroke.with_dashes(st.dash_offset * s, st.dash.iter().map(|d| d.abs() * s));
                }
                (kurbo::stroke(dev.elements().iter().copied(), &stroke, &kurbo::StrokeOpts::default(), TOL), false)
            }
        }
    }

    /// Paints one object; its reference is its paint-order index among the objects drawn through
    /// this method (`ObjectRef::PageItem`).
    pub fn draw(&mut self, d: &SepDraw<'_>) -> Result<(), SepError> {
        let reference = ObjectRef::PageItem(self.drawn);
        self.drawn = self.drawn.saturating_add(1);
        self.draw_object(d, reference)
    }

    /// Paints one object carrying a caller's object reference (a page item id, or a placed asset by
    /// link). The object's record (outline, clip, tones, stages, reference) is charged to the byte
    /// budget BEFORE any plate changes, so a refusal leaves the separation as it was.
    pub fn draw_object(&mut self, d: &SepDraw<'_>, object: ObjectRef) -> Result<(), SepError> {
        if !object.is_valid() {
            return Err(SepError::InvalidInput);
        }
        let nonzero = d.overprint && d.overprint_nonzero;
        let mut stages = 0u8;
        let tones = self.tones(&d.paint, nonzero, &mut stages)?;
        if tones.is_empty() && d.overprint {
            return Ok(());
        }
        let (path, even_odd) = self.outline(d.path, d.mode);
        let mut hits: Vec<(usize, f32)> = Vec::new();
        self.cover(&path, even_odd, |i, c| hits.push((i, c)))?;
        let w = (self.out.width as usize).max(1);
        let clip = self.clips.last();
        let mut bounds: Option<(usize, usize, usize, usize)> = None;
        for (i, c) in &mut hits {
            *c *= clip.map_or(1.0, |m| m[*i]);
            if *c > 0.0 {
                let (x, y) = (i.checked_rem(w).unwrap_or(0), i.checked_div(w).unwrap_or(0));
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
        let Some(bounds) = bounds else { return Ok(()) };
        let tones: Vec<(usize, f32)> = tones.into_iter().map(|(p, t)| (p, t as f32)).collect();
        if tones.iter().any(|(p, t)| *t > 0.0 && self.out.plates.get(*p).is_some_and(|pl| pl.ink_type.obscures())) {
            stages |= CoverageInputStage::OpaqueInkHandling.bit();
        }
        self.charge(Self::record_bytes(&path, tones.len(), std::mem::size_of::<(usize, f32)>())?)?;
        let plates = self.out.planes.len();
        let mut full = vec![0.0f32; plates];
        let mut used = vec![false; plates];
        for (p, t) in &tones {
            full[*p] = *t;
            used[*p] = true;
        }
        for (i, a) in hits {
            if a <= 0.0 {
                continue;
            }
            for (p, plane) in self.out.planes.iter_mut().enumerate() {
                if d.overprint && !used[p] {
                    continue;
                }
                let old = plane[i];
                plane[i] = old * (1.0 - a) + a * full[p];
            }
        }
        self.out.objects.push(SepObject {
            reference: object,
            outline: path,
            even_odd,
            bounds,
            clip: self.clip_at,
            tones,
            overprint: d.overprint,
            stages,
        });
        Ok(())
    }

    /// Intersects the clip with `path`.
    pub fn push_clip(&mut self, path: &[VectorEl], even_odd: bool) -> Result<(), SepError> {
        if self.clips.len() >= MAX_SEP_CLIP_DEPTH {
            return Err(SepError::ClipTooDeep);
        }
        let dev = self.device(path);
        self.charge(Self::record_bytes(&dev, 0, 0)?)?;
        let mut mask = self.charge_plane()?;
        self.cover(&dev, even_odd, |i, c| mask[i] = c)?;
        if let Some(prev) = self.clips.last() {
            for (m, p) in mask.iter_mut().zip(prev) {
                *m *= *p;
            }
        }
        self.clips.push(mask);
        self.out.clip_nodes.push(ClipNode { outline: dev, even_odd, parent: self.clip_at });
        self.clip_at = Some(self.out.clip_nodes.len().saturating_sub(1));
        Ok(())
    }

    pub fn pop_clip(&mut self) {
        if self.clips.pop().is_some() {
            let b = u64::try_from(self.pixels).unwrap_or(u64::MAX).saturating_mul(4);
            self.bytes = self.bytes.saturating_sub(b);
            self.clip_at = self.clip_at.and_then(|c| self.out.clip_nodes.get(c)).and_then(|n| n.parent);
        }
    }

    pub fn note(&mut self, n: SepNote) {
        self.out.notes.insert(n);
    }

    pub fn finish(self) -> SeparationPreview {
        self.out
    }
}

/// Separates one page read by [`crate::read_vector_document`] (paint order; clips honoured).
pub fn separate_vector_page(
    page: &VectorPage,
    px_per_pt: f64,
    inks: &InkManager,
    converter: Option<&dyn ProcessConverter>,
    limits: SepLimits,
    cancel: &CancellationToken,
) -> Result<SeparationPreview, SepError> {
    let mut s = Separator::new(page.crop_box, px_per_pt, inks, limits)?;
    s.note(SepNote::PdfOverprintNotRead);
    for (n, item) in page.items.iter().enumerate() {
        if n.is_multiple_of(64) {
            cancel.check().map_err(|_| SepError::Canceled)?;
        }
        match item {
            VectorItem::Draw { path, mode, paint, blend, source, .. } => {
                if let VectorPaint::Gradient { .. } = paint {
                    s.note(SepNote::ShadingNotSeparated);
                    continue;
                }
                if *blend != 0 || matches!(paint, VectorPaint::Solid(c) if c[3] < 1.0) {
                    s.note(SepNote::TransparencyFlattenedOpaque);
                }
                let sep = match source {
                    Some(VectorColour::Process { space: VectorSpace::Cmyk, components }) if components.len() == 4 => {
                        Some(SepPaint::Process([components[0], components[1], components[2], components[3]]))
                    }
                    Some(VectorColour::Process { space: VectorSpace::Gray, components }) if components.len() == 1 => {
                        Some(SepPaint::Process([0.0, 0.0, 0.0, 1.0 - components[0]]))
                    }
                    Some(VectorColour::Spot { name, tint, .. }) => Some(match name.as_str() {
                        "All" => SepPaint::Registration(*tint),
                        "None" => SepPaint::NoInk,
                        _ => SepPaint::Spot { name: name.clone(), tint: *tint },
                    }),
                    Some(VectorColour::Process { space, components }) => {
                        converter.and_then(|c| c.to_cmyk(space, components)).map(SepPaint::Process)
                    }
                    None => None,
                };
                let Some(paint) = sep else {
                    s.note(SepNote::Unseparated);
                    s.out.unseparated = s.out.unseparated.saturating_add(1);
                    continue;
                };
                let item = u64::try_from(n).unwrap_or(u64::MAX);
                s.draw_object(&SepDraw { path, mode, paint, overprint: false, overprint_nonzero: false }, ObjectRef::PageItem(item))?;
            }
            VectorItem::PushClip { path, even_odd } => s.push_clip(path, *even_odd)?,
            VectorItem::PopClip => s.pop_clip(),
            VectorItem::PushGroup { opacity, blend } => {
                if *opacity < 1.0 || *blend != 0 {
                    s.note(SepNote::TransparencyFlattenedOpaque);
                }
            }
            VectorItem::PopGroup | VectorItem::BeginLayer { .. } | VectorItem::EndLayer => {}
        }
    }
    Ok(s.finish())
}

/// Tolerance of every limit comparison, percent (the planes hold f32 tones).
pub(crate) const LIMIT_EPS: f64 = 1e-4;

/// Which value of a location a limit is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Measure {
    /// The total ink (`total_ink_limit`).
    Total,
    /// The highest single plate tone (`single_ink_limit`).
    SingleInk,
    /// The process black plate (`black_ink_limit`).
    Black,
}

/// What a replay at one location found.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Attribution {
    pub(crate) stage: Option<CoverageInputStage>,
    pub(crate) objects: Vec<ObjectRef>,
    pub(crate) over_cap: bool,
}

impl SeparationPreview {
    fn index_pt(&self, x: f64, y: f64) -> Option<usize> {
        let [x0, _, _, y1] = self.page_box;
        let (px, py) = ((x - x0) * self.px_per_pt, (y1 - y) * self.px_per_pt);
        if !(px.is_finite() && py.is_finite()) || px < 0.0 || py < 0.0 || px >= f64::from(self.width) || py >= f64::from(self.height) {
            return None;
        }
        (py as usize).checked_mul(self.width as usize)?.checked_add(px as usize)
    }

    /// Tone of plate `plate` at pixel (x, y), 0..=1.
    pub fn tone(&self, plate: usize, x: u32, y: u32) -> Option<f32> {
        let i = (y as usize).checked_mul(self.width as usize)?.checked_add(x as usize)?;
        if x >= self.width {
            return None;
        }
        self.planes.get(plate)?.get(i).copied()
    }

    /// Per-ink readout at a page point (points, PDF user space): every plate, percent.
    pub fn sample_pt(&self, x: f64, y: f64) -> Option<Vec<InkReading>> {
        let i = self.index_pt(x, y)?;
        Some(
            self.plates
                .iter()
                .zip(&self.planes)
                .map(|(p, v)| InkReading { ink: p.name.clone(), percent: f64::from(v[i]) * 100.0 })
                .collect(),
        )
    }

    /// Total ink at a page point, percent (opaque inks per their declared `ink_type`, module docs).
    pub fn total_pt(&self, x: f64, y: f64) -> Option<f64> {
        let i = self.index_pt(x, y)?;
        Some(self.total_at(i))
    }

    /// The resolution this separation was sampled at, in dpi: `px_per_pt` x 72 points per inch
    /// (`coverage_sample_resolution`, STU-LAY-250/252).
    pub fn sample_resolution(&self) -> f64 {
        self.px_per_pt * 72.0
    }

    /// Canvas pixels (the evaluated sample grid).
    pub(crate) fn pixel_count(&self) -> usize {
        self.planes.first().map_or(0, Vec::len)
    }

    /// Total ink at pixel index `i`, percent.
    pub(crate) fn total_at(&self, i: usize) -> f64 {
        mixed_total(&self.plates, |p| f64::from(self.planes[p][i]))
    }

    /// The value a coverage measure reads at pixel index `i`, percent.
    pub(crate) fn measure_at(&self, m: Measure, i: usize) -> f64 {
        match m {
            Measure::Total => self.total_at(i),
            Measure::SingleInk => self.planes.iter().map(|v| f64::from(v[i])).fold(0.0, f64::max) * 100.0,
            Measure::Black => f64::from(self.planes[ProcessInk::Black.index()][i]) * 100.0,
        }
    }

    fn measure_tones(&self, m: Measure, v: &[f32]) -> f64 {
        match m {
            Measure::Total => mixed_total(&self.plates, |p| v.get(p).map_or(0.0, |t| f64::from(*t))),
            Measure::SingleInk => v.iter().map(|t| f64::from(*t)).fold(0.0, f64::max) * 100.0,
            Measure::Black => v.get(ProcessInk::Black.index()).map_or(0.0, |t| f64::from(*t)) * 100.0,
        }
    }

    /// Every plate's tone at pixel index `i`, percent.
    pub(crate) fn readings_at(&self, i: usize) -> Vec<InkReading> {
        self.plates.iter().zip(&self.planes).map(|(p, v)| InkReading { ink: p.name.clone(), percent: f64::from(v[i]) * 100.0 }).collect()
    }

    /// Page-space (points, PDF user space) box of the inclusive pixel rectangle (x0, y0)-(x1, y1).
    pub(crate) fn pixel_box(&self, (x0, y0, x1, y1): (usize, usize, usize, usize)) -> [f64; 4] {
        let [bx, _, _, by] = self.page_box;
        let s = self.px_per_pt;
        let (l, r) = (x0 as f64, x1.saturating_add(1) as f64);
        let (t, b) = (y0 as f64, y1.saturating_add(1) as f64);
        [bx + l / s, by - b / s, bx + r / s, by - t / s]
    }

    /// Coverage of the clip chain ending at `node` at pixel (x, y), as the masks multiplied it.
    fn clip_cover(&self, node: Option<usize>, x: usize, y: usize, memo: &mut [Option<f32>], work: &mut u64) -> Result<f32, SepError> {
        let w = (self.width as usize).max(1);
        let mut chain = Vec::new();
        let mut at = node;
        while let Some(n) = at {
            if chain.len() > self.clip_nodes.len() {
                return Err(SepError::Budget);
            }
            chain.push(n);
            at = self.clip_nodes.get(n).and_then(|c| c.parent);
        }
        let mut m = 1.0f32;
        let mut first = true;
        for n in chain.into_iter().rev() {
            let c = match memo.get(n).copied().flatten() {
                Some(c) => c,
                None => {
                    let node = &self.clip_nodes[n];
                    let own = pixel_cover(&node.outline, node.even_odd, x, y, (w, self.max_segments, self.max_work), work)?;
                    let c = if first { own } else { own * m };
                    if let Some(slot) = memo.get_mut(n) {
                        *slot = Some(c);
                    }
                    c
                }
            };
            m = c;
            first = false;
        }
        Ok(m)
    }

    /// Replays the composition at pixel index `i` (same coverage and f32 compositing as the paint)
    /// to name the objects whose ink is still present there and the coverage input stage of the
    /// object after which `measure` last rose from at most `threshold` to above it. An overprinting
    /// object is attributed to overprint composition when knocking out instead would have stayed at
    /// or below the threshold; otherwise the first of its paint stages in the order spot-to-process,
    /// ink alias, authored rich black, opaque ink handling; otherwise none (authored process values).
    /// More than `max_refs` composing objects sets `over_cap` (no list).
    pub(crate) fn attribute(&self, i: usize, measure: Measure, threshold: f64, max_refs: usize) -> Result<Attribution, SepError> {
        let w = (self.width as usize).max(1);
        let (x, y) = (i.checked_rem(w).unwrap_or(0), i.checked_div(w).unwrap_or(0));
        let n = self.planes.len();
        let mut work = 0u64;
        let mut memo: Vec<Option<f32>> = vec![None; self.clip_nodes.len()];
        let mut v = vec![0.0f32; n];
        let mut hits: Vec<(usize, f32)> = Vec::new();
        let mut crossing: Option<(usize, bool)> = None;
        let mut full = vec![0.0f32; n];
        let mut used = vec![false; n];
        for (k, o) in self.objects.iter().enumerate() {
            let (x0, y0, x1, y1) = o.bounds;
            if x < x0 || x > x1 || y < y0 || y > y1 {
                continue;
            }
            let c = pixel_cover(&o.outline, o.even_odd, x, y, (w, self.max_segments, self.max_work), &mut work)?;
            let a = c * self.clip_cover(o.clip, x, y, &mut memo, &mut work)?;
            if a <= 0.0 {
                continue;
            }
            full.iter_mut().for_each(|t| *t = 0.0);
            used.iter_mut().for_each(|u| *u = false);
            for (p, t) in &o.tones {
                if let (Some(f), Some(u)) = (full.get_mut(*p), used.get_mut(*p)) {
                    *f = *t;
                    *u = true;
                }
            }
            let before = self.measure_tones(measure, &v);
            let knockout = o.overprint.then(|| v.iter().zip(&full).map(|(old, f)| *old * (1.0 - a) + a * *f).collect::<Vec<f32>>());
            for (p, t) in v.iter_mut().enumerate() {
                if o.overprint && !used[p] {
                    continue;
                }
                *t = *t * (1.0 - a) + a * full[p];
            }
            let after = self.measure_tones(measure, &v);
            if before <= threshold + LIMIT_EPS && after > threshold + LIMIT_EPS {
                let by_overprint = knockout.is_some_and(|ko| self.measure_tones(measure, &ko) <= threshold + LIMIT_EPS);
                crossing = Some((k, by_overprint));
            }
            hits.push((k, a));
        }
        // Composing objects: ink still present after every later object wrote its plates.
        let mut keep = vec![1.0f64; n];
        let mut composing: Vec<usize> = Vec::new();
        for (k, a) in hits.iter().rev() {
            let o = &self.objects[*k];
            if o.tones.iter().any(|(p, t)| *t > 0.0 && keep.get(*p).is_some_and(|r| *r > 0.0)) {
                if composing.len() >= max_refs {
                    return Ok(Attribution { stage: None, objects: Vec::new(), over_cap: true });
                }
                composing.push(*k);
            }
            for (p, r) in keep.iter_mut().enumerate() {
                if !o.overprint || o.tones.iter().any(|(q, _)| *q == p) {
                    *r *= 1.0 - f64::from(*a);
                }
            }
        }
        let objects = composing.iter().rev().map(|k| self.objects[*k].reference.clone()).collect();
        let stage = crossing.and_then(|(k, by_overprint)| {
            if by_overprint {
                return Some(CoverageInputStage::OverprintComposition);
            }
            let st = self.objects[k].stages;
            [
                CoverageInputStage::SpotToProcessConversion,
                CoverageInputStage::InkAliasResolution,
                CoverageInputStage::AuthoredRichBlack,
                CoverageInputStage::OpaqueInkHandling,
            ]
            .into_iter()
            .find(|s| st & s.bit() != 0)
        });
        Ok(Attribution { stage, objects, over_cap: false })
    }

    /// Per-ink coverage.
    pub fn coverage(&self) -> Vec<PlateCoverage> {
        self.plates
            .iter()
            .zip(&self.planes)
            .map(|(p, v)| {
                let n = v.len().max(1) as f64;
                PlateCoverage {
                    ink: p.name.clone(),
                    max_percent: v.iter().fold(0.0f32, |m, t| m.max(*t)) as f64 * 100.0,
                    mean_percent: v.iter().map(|t| f64::from(*t)).sum::<f64>() / n * 100.0,
                    inked_pixels: v.iter().filter(|t| **t > 0.0).count() as u64,
                }
            })
            .collect()
    }

    /// The ink-limit VIEW of the separations preview (STU-LAY-160: a configurable threshold): pixel
    /// classes against the three independent limits of STU-LAY-250. `total_ink_limit` is bounded by
    /// the structural maximum 100 x plate count (not clamped at 400); undeclared limits are not checked.
    /// A value inside `(limit - margin, limit]` is a warning when a margin is declared. The view has no
    /// declaration site and no check state: the contract evaluation with sources, `not_checked` and the
    /// STU-LAY-253 finding is [`crate::evaluate_ink_coverage`] over the same planes and total.
    pub fn check_ink_limits(&self, limits: &InkLimits) -> Result<InkLimitReport, SepError> {
        let structural = self.planes.len() as f64 * 100.0;
        let ok = |v: Option<f64>, hi: f64| v.is_none_or(|x| x.is_finite() && (0.0..=hi).contains(&x));
        if !ok(limits.total_ink_limit, structural)
            || !ok(limits.single_ink_limit, 100.0)
            || !ok(limits.black_ink_limit, 100.0)
            || !ok(limits.coverage_warning_margin, f64::MAX)
        {
            return Err(SepError::InvalidInput);
        }
        let pixels = self.planes.first().map_or(0, Vec::len);
        let w = self.width.max(1) as usize;
        let margin = limits.coverage_warning_margin;
        let mut findings = Vec::new();
        let mut classify = |kind: LimitKind, limit: f64, values: &mut dyn Iterator<Item = f64>| {
            let (mut err, mut warn): (Option<InkLimitFinding>, Option<InkLimitFinding>) = (None, None);
            for (i, v) in values.enumerate() {
                let sev = if v > limit + 1e-4 {
                    Some(Severity::Error)
                } else if margin.is_some_and(|m| v > limit - m + 1e-4 && v > 1e-4) {
                    Some(Severity::Warning)
                } else {
                    None
                };
                let Some(sev) = sev else { continue };
                let slot = if sev == Severity::Error { &mut err } else { &mut warn };
                let f = slot.get_or_insert_with(|| InkLimitFinding {
                    limit: kind.clone(),
                    severity: sev,
                    limit_percent: limit,
                    pixels: 0,
                    max_percent: 0.0,
                    first_px: (i.checked_rem(w).unwrap_or(0) as u32, i.checked_div(w).unwrap_or(0) as u32),
                });
                f.pixels = f.pixels.saturating_add(1);
                f.max_percent = f.max_percent.max(v);
            }
            findings.extend(err);
            findings.extend(warn);
        };
        let total = |i: usize| self.total_at(i);
        let max_total = (0..pixels).map(total).fold(0.0, f64::max);
        if let Some(l) = limits.total_ink_limit {
            classify(LimitKind::Total, l, &mut (0..pixels).map(total));
        }
        if let Some(l) = limits.single_ink_limit {
            for (p, v) in self.plates.iter().zip(&self.planes) {
                classify(LimitKind::Single(p.name.clone()), l, &mut v.iter().map(|t| f64::from(*t) * 100.0));
            }
        }
        if let Some(l) = limits.black_ink_limit {
            classify(LimitKind::Black, l, &mut self.planes[ProcessInk::Black.index()].iter().map(|t| f64::from(*t) * 100.0));
        }
        Ok(InkLimitReport { structural_max_percent: structural, max_total_percent: max_total, findings })
    }

    /// RGB8 rows (top-down) of the chosen view.
    pub fn render(&self, view: &PlateView) -> Vec<u8> {
        let pixels = self.planes.first().map_or(0, Vec::len);
        let mut out = Vec::with_capacity(pixels.saturating_mul(3));
        for i in 0..pixels {
            let rgb = match view {
                PlateView::Single(p) => {
                    let g = 1.0 - self.planes.get(*p).map_or(0.0, |v| f64::from(v[i]));
                    [g, g, g]
                }
                PlateView::Composite(shown) => self.composite(i, shown),
                PlateView::InkLimit(t) => {
                    let total = self.total_at(i);
                    if total > *t + 1e-4 {
                        [1.0, 0.0, 0.0]
                    } else {
                        let c = self.composite(i, &[]);
                        let g = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
                        [g, g, g]
                    }
                }
            };
            out.extend(rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
        }
        out
    }

    fn composite(&self, i: usize, shown: &[bool]) -> [f64; 3] {
        let mut c = [1.0f64; 3];
        for (k, (p, v)) in self.plates.iter().zip(&self.planes).enumerate() {
            if !shown.get(k).copied().unwrap_or(true) {
                continue;
            }
            let t = f64::from(v[i]);
            for (ch, d) in c.iter_mut().zip(p.display_rgb) {
                *ch *= 1.0 - t * (1.0 - d);
            }
        }
        c
    }
}
