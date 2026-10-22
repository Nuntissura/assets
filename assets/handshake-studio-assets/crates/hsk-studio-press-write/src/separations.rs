//! Separations preview (lane SMB-ID-PREPRESS; STU-LAY-160 separations preview, STU-COL-212,
//! STU-LAY-250 total ink coverage). Content is separated into one coverage plane per ink (the four
//! process inks first, then every spot ink in order of first use), each pixel holding the ink's
//! tone 0..=1. Everything the preview shows is readable as typed values: per-ink readouts at a point
//! ([`SeparationPreview::sample_pt`]), per-ink coverage ([`SeparationPreview::coverage`]), the
//! ink-limit check against the three independent limits of STU-LAY-250
//! ([`SeparationPreview::check_ink_limits`]) and per-plate on/off renders ([`SeparationPreview::render`]).
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
use crate::inks::{InkError, InkManager, InkTarget, MixedInk, ProcessInk};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
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
}

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
    bytes: u64,
    work: u64,
    pixels: usize,
}

fn cmyk_rgb(c: [f64; 4]) -> [f64; 3] {
    let k = 1.0 - c[3];
    [(1.0 - c[0]) * k, (1.0 - c[1]) * k, (1.0 - c[2]) * k]
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
            },
            clips: Vec::new(),
            bytes: 0,
            work: 0,
            pixels,
        };
        for p in ProcessInk::ALL {
            s.add_plate(Plate {
                name: p.name().into(),
                process: Some(p),
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

    fn charge_plane(&mut self) -> Result<Vec<f32>, SepError> {
        let b = u64::try_from(self.pixels).ok().and_then(|p| p.checked_mul(4)).ok_or(SepError::Budget)?;
        self.bytes = self.bytes.checked_add(b).filter(|v| *v <= self.limits.max_bytes).ok_or(SepError::Budget)?;
        Ok(vec![0.0; self.pixels])
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
        self.add_plate(Plate { name: name.into(), process: None, display_rgb: alt.map_or([0.5, 0.5, 0.5], cmyk_rgb) })
    }

    /// Plate tones of a paint: (plate, tone) pairs.
    fn tones(&mut self, paint: &SepPaint, nonzero_only: bool) -> Result<Vec<(usize, f64)>, SepError> {
        let clamp = |v: f64| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        let mut out: Vec<(usize, f64)> = Vec::new();
        let mut add = |out: &mut Vec<(usize, f64)>, plate: usize, v: f64| match out.iter_mut().find(|(p, _)| *p == plate) {
            Some((_, t)) => *t = clamp(*t + v),
            None => out.push((plate, clamp(v))),
        };
        match paint {
            SepPaint::NoInk => {}
            SepPaint::Process(c) => {
                for (i, v) in c.iter().enumerate() {
                    if !(nonzero_only && *v <= 0.0) {
                        add(&mut out, i, *v);
                    }
                }
            }
            SepPaint::Registration(t) => {
                for i in 0..self.out.plates.len() {
                    add(&mut out, i, *t);
                }
            }
            SepPaint::Spot { name, tint } => self.spot_tones(name, clamp(*tint), &mut out, &mut add)?,
            SepPaint::Mixed { ink, tint } => {
                ink.validate(self.inks)?;
                for (name, part) in &ink.parts {
                    self.spot_tones(name, clamp(*tint) * clamp(*part), &mut out, &mut add)?;
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
    ) -> Result<(), SepError> {
        if let Some(p) = ProcessInk::ALL.iter().find(|p| p.name() == name) {
            add(out, p.index(), tint);
            return Ok(());
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
        let mut segs: Vec<(f64, f64, f64, f64, i32)> = Vec::new();
        let (mut start, mut last, mut over) = (Point::ZERO, Point::ZERO, false);
        let cap = self.limits.max_segments;
        let mut push = |segs: &mut Vec<(f64, f64, f64, f64, i32)>, a: Point, b: Point| {
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
        for row in row0..row1 {
            counts.iter_mut().for_each(|c| *c = 0);
            let mut touched = false;
            for sub in 0..SS {
                let y = row as f64 + (sub as f64 + 0.5) / SS as f64;
                xs.clear();
                for s in &segs {
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
                    let (lo, hi) = (lo as usize, hi as usize);
                    self.work = self.work.saturating_add(hi.saturating_sub(lo) as u64);
                    if self.work > self.limits.max_work {
                        return Err(SepError::Budget);
                    }
                    for j in lo..hi {
                        let c = &mut counts[j / SS];
                        *c = c.saturating_add(1);
                    }
                    touched = true;
                }
            }
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

    /// Paints one object.
    pub fn draw(&mut self, d: &SepDraw<'_>) -> Result<(), SepError> {
        let nonzero = d.overprint && d.overprint_nonzero;
        let tones = self.tones(&d.paint, nonzero)?;
        if tones.is_empty() && d.overprint {
            return Ok(());
        }
        let (path, even_odd) = self.outline(d.path, d.mode);
        let mut hits: Vec<(usize, f32)> = Vec::new();
        self.cover(&path, even_odd, |i, c| hits.push((i, c)))?;
        let clip = self.clips.last();
        let plates = self.out.planes.len();
        let mut full = vec![0.0f32; plates];
        let mut used = vec![false; plates];
        for (p, t) in &tones {
            full[*p] = *t as f32;
            used[*p] = true;
        }
        for (i, c) in hits {
            let a = c * clip.map_or(1.0, |m| m[i]);
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
        Ok(())
    }

    /// Intersects the clip with `path`.
    pub fn push_clip(&mut self, path: &[VectorEl], even_odd: bool) -> Result<(), SepError> {
        if self.clips.len() >= MAX_SEP_CLIP_DEPTH {
            return Err(SepError::ClipTooDeep);
        }
        let mut mask = self.charge_plane()?;
        let dev = self.device(path);
        self.cover(&dev, even_odd, |i, c| mask[i] = c)?;
        if let Some(prev) = self.clips.last() {
            for (m, p) in mask.iter_mut().zip(prev) {
                *m *= *p;
            }
        }
        self.clips.push(mask);
        Ok(())
    }

    pub fn pop_clip(&mut self) {
        if self.clips.pop().is_some() {
            let b = u64::try_from(self.pixels).unwrap_or(u64::MAX).saturating_mul(4);
            self.bytes = self.bytes.saturating_sub(b);
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
                s.draw(&SepDraw { path, mode, paint, overprint: false, overprint_nonzero: false })?;
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

    /// Total ink at a page point, percent.
    pub fn total_pt(&self, x: f64, y: f64) -> Option<f64> {
        let i = self.index_pt(x, y)?;
        Some(self.planes.iter().map(|v| f64::from(v[i])).sum::<f64>() * 100.0)
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

    /// Evaluates the three independent limits of STU-LAY-250. `total_ink_limit` is bounded by the
    /// structural maximum 100 x plate count (not clamped at 400); undeclared limits are not checked.
    /// A value inside `(limit - margin, limit]` is a warning when a margin is declared.
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
        let total = |i: usize| self.planes.iter().map(|v| f64::from(v[i])).sum::<f64>() * 100.0;
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
                    let total: f64 = self.planes.iter().map(|v| f64::from(v[i])).sum::<f64>() * 100.0;
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
