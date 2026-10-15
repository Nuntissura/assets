//! Selection tools beyond the marquee / lasso / colour-range producers of [`crate::selection`]
//! (lane SMB-PS-SELECT-LIQUIFY): magic-wand flood select ([`wand`]), quick selection and
//! rectangle GrabCut by graph-cut segmentation ([`quick`]), and the Select and Mask edge
//! refinement surface ([`refine`]). Every tool produces a [`CoveragePlane`], the one selection
//! representation of STU-RAS-017, and combines through [`crate::selection::combine`].
//!
//! Pure compute: no ports, no leases, no Folio writes. Every formula is a documented Studio
//! reading, class Approximate; Photoshop parity is UNVERIFIED (oracle list D5).
#![deny(clippy::arithmetic_side_effects)]
pub mod maxflow;
pub mod quick;
pub mod refine;
pub mod wand;

use crate::brush::BrushError;
use crate::selection::{ColourView, CoveragePlane, SelectionError, SelectionLimits};
use crate::{Error, Rect};
use std::fmt;

/// Typed failure of a selection tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolError {
    Selection(SelectionError),
    Brush(BrushError),
    /// The tool needs an on-device ML model (object / subject / sky select, STU-RAS-019) and no
    /// model is installed. Nothing is downloaded.
    ModelUnavailable,
    /// A numeric option is outside the domain its formula is defined on (never clamped).
    OutOfRange,
}
impl ToolError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Selection(e) => e.code(),
            Self::Brush(e) => e.code(),
            Self::ModelUnavailable => "model_unavailable",
            Self::OutOfRange => "tool_out_of_range",
        }
    }
}
impl From<Error> for ToolError {
    fn from(e: Error) -> Self {
        Self::Selection(SelectionError::Pigment(e))
    }
}
impl From<SelectionError> for ToolError {
    fn from(e: SelectionError) -> Self {
        Self::Selection(e)
    }
}
impl From<BrushError> for ToolError {
    fn from(e: BrushError) -> Self {
        Self::Brush(e)
    }
}
impl From<crate::coverage_math::CoverageError> for ToolError {
    fn from(e: crate::coverage_math::CoverageError) -> Self {
        Self::Selection(SelectionError::Coverage(e))
    }
}
impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ToolError {}

/// Working-memory budget of a tool, checked BEFORE any allocation (V4-02). Output planes are
/// additionally bounded by [`SelectionLimits::max_bytes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolLimits {
    /// Largest working set in bytes (scratch images, graphs, stacks).
    pub max_work_bytes: u64,
}
impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            max_work_bytes: 256 * 1024 * 1024,
        }
    }
}
impl ToolLimits {
    /// Refuses `pixels * bytes_per_pixel` above the budget (overflow is `Overflow`).
    pub fn admit(&self, pixels: u64, bytes_per_pixel: u64) -> Result<usize, Error> {
        let bytes = pixels.checked_mul(bytes_per_pixel).ok_or(Error::Overflow)?;
        if bytes > self.max_work_bytes {
            return Err(Error::BudgetExceeded);
        }
        usize::try_from(pixels).map_err(|_| Error::Overflow)
    }
}

/// Output-plane admission against the selection byte budget (2 bytes per sample).
pub(crate) fn admit_plane(bounds: Rect, limits: &SelectionLimits) -> Result<usize, Error> {
    let bytes = bounds.area().checked_mul(2).ok_or(Error::Overflow)?;
    if bytes > limits.max_bytes {
        return Err(Error::BudgetExceeded);
    }
    usize::try_from(bounds.area()).map_err(|_| Error::Overflow)
}

/// Row-major index of `(x, y)` inside `bounds`, `None` outside.
pub(crate) fn index_in(bounds: Rect, x: i64, y: i64) -> Option<usize> {
    let dx = u32::try_from(x.checked_sub(bounds.x)?).ok()?;
    let dy = u32::try_from(y.checked_sub(bounds.y)?).ok()?;
    if dx >= bounds.width || dy >= bounds.height {
        return None;
    }
    (dy as usize)
        .checked_mul(bounds.width as usize)?
        .checked_add(dx as usize)
}

/// `(x, y)` of a row-major index inside `bounds` (index must be in range).
pub(crate) fn coords_of(bounds: Rect, i: usize) -> (i64, i64) {
    let w = (bounds.width as usize).max(1);
    let (dx, dy) = (i.checked_rem(w).unwrap_or(0), i.checked_div(w).unwrap_or(0));
    (
        bounds.x.saturating_add(dx as i64),
        bounds.y.saturating_add(dy as i64),
    )
}

fn f32_at(bytes: &[u8], index: usize) -> Option<f32> {
    let at = index.checked_mul(4)?;
    let end = at.checked_add(4)?;
    Some(f32::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

/// A bottom-to-top stack of resolved layer views (normal blend, straight alpha). With one view it
/// is that layer; with several it is the merged image ("sample all layers", STU-RAS-119
/// `sample_all_layers`), composited per pixel by Porter-Duff source-over in binary64. Pixels
/// outside every view are transparent.
#[derive(Clone, Copy, Debug)]
pub struct Composite<'a> {
    layers: &'a [ColourView<'a>],
}
impl<'a> Composite<'a> {
    /// Validates every view's stride (12 bytes colour, 4 bytes alpha per pixel) up front.
    pub fn new(layers: &'a [ColourView<'a>]) -> Result<Self, Error> {
        for v in layers {
            let pixels = usize::try_from(v.bounds.area()).map_err(|_| Error::Overflow)?;
            if pixels.checked_mul(12) != Some(v.colour.len())
                || pixels.checked_mul(4) != Some(v.alpha.len())
            {
                return Err(Error::MalformedStride);
            }
            v.bounds.end()?;
        }
        Ok(Self { layers })
    }

    /// The layers this composite reads: `sample_merged` -> all, else only `active`.
    pub fn select(layers: &'a [ColourView<'a>], active: usize, sample_merged: bool) -> Result<Self, Error> {
        if sample_merged {
            Self::new(layers)
        } else {
            let one = layers.get(active..=active).ok_or(Error::InvalidInput)?;
            Self::new(one)
        }
    }

    /// Premultiplied `[r a, g a, b a, a]` at `(x, y)`; NaN / infinite samples refuse `Nonfinite`,
    /// alpha outside `[0, 1]` refuses `AlphaRange`.
    pub fn premul(&self, x: i64, y: i64) -> Result<[f64; 4], Error> {
        let mut acc = [0.0f64; 4];
        for v in self.layers {
            let Some(px) = index_in(v.bounds, x, y) else {
                continue;
            };
            let a = f32_at(v.alpha, px).ok_or(Error::MalformedStride)?;
            if !a.is_finite() {
                return Err(Error::Nonfinite);
            }
            if !(0.0..=1.0).contains(&a) {
                return Err(Error::AlphaRange);
            }
            let a = f64::from(a);
            let k = 1.0 - a;
            for (c, slot) in acc.iter_mut().take(3).enumerate() {
                let i = px
                    .checked_mul(3)
                    .and_then(|v| v.checked_add(c))
                    .ok_or(Error::Overflow)?;
                let s = f32_at(v.colour, i).ok_or(Error::MalformedStride)?;
                if !s.is_finite() {
                    return Err(Error::Nonfinite);
                }
                *slot = f64::from(s) * a + *slot * k;
            }
            acc[3] = a + acc[3] * k;
        }
        Ok(acc)
    }
}

/// An owned premultiplied RGBA image over `bounds` (row-major), the working copy of the ROI
/// tools (quick select, refine). 32 bytes per pixel, admitted against [`ToolLimits`].
#[derive(Clone, Debug, PartialEq)]
pub struct RgbaImage {
    pub bounds: Rect,
    pub px: Vec<[f64; 4]>,
}
impl RgbaImage {
    pub const BYTES_PER_PIXEL: u64 = 32;

    pub fn from_composite(src: &Composite<'_>, bounds: Rect, limits: &ToolLimits) -> Result<Self, Error> {
        let n = limits.admit(bounds.area(), Self::BYTES_PER_PIXEL)?;
        let (ex, ey) = bounds.end()?;
        let mut px = Vec::with_capacity(n);
        for y in bounds.y..ey {
            for x in bounds.x..ex {
                px.push(src.premul(x, y)?);
            }
        }
        Ok(Self { bounds, px })
    }

    /// Straight colour (un-premultiplied; transparent pixels read black).
    pub fn straight(&self, i: usize) -> [f64; 3] {
        let p = self.px.get(i).copied().unwrap_or([0.0; 4]);
        if p[3] > 0.0 {
            [p[0] / p[3], p[1] / p[3], p[2] / p[3]]
        } else {
            [0.0; 3]
        }
    }
}

/// A binary selection over `bounds` as a coverage plane. With `anti_alias` (the document toggle,
/// STU-RAS-119) a pixel whose 4-neighbourhood holds the other state gets the 3x3 box mean of the
/// binary selection, `round(hits / 9 * 65535)` (neighbours outside `bounds` repeat the centre);
/// other pixels are 0 or 65535. Studio reading, Photoshop kernel UNVERIFIED (D5). The caller
/// admitted `bounds` as a plane.
pub(crate) fn binary_plane(
    bounds: Rect,
    selected: impl Fn(usize) -> bool,
    anti_alias: bool,
) -> Result<CoveragePlane, Error> {
    let n = usize::try_from(bounds.area()).map_err(|_| Error::Overflow)?;
    let mut data = Vec::with_capacity(n);
    for i in 0..n {
        let centre = selected(i);
        if !anti_alias {
            data.push(if centre { u16::MAX } else { 0 });
            continue;
        }
        let (x, y) = coords_of(bounds, i);
        let mut boundary = false;
        let mut hits = 0u64;
        for oy in -1i64..=1 {
            for ox in -1i64..=1 {
                let v = match index_in(bounds, x.saturating_add(ox), y.saturating_add(oy)) {
                    Some(j) => selected(j),
                    None => centre,
                };
                if v != centre && (ox == 0 || oy == 0) {
                    boundary = true;
                }
                if v {
                    hits = hits.saturating_add(1);
                }
            }
        }
        data.push(if boundary {
            let q = hits
                .saturating_mul(65535)
                .saturating_mul(2)
                .saturating_add(9)
                .checked_div(18)
                .unwrap_or(0);
            u16::try_from(q).unwrap_or(u16::MAX)
        } else if centre {
            u16::MAX
        } else {
            0
        });
    }
    Ok(CoveragePlane { bounds, data })
}

/// Plane bounds tightened to the samples that are not zero (zero-size at `fallback` when empty).
pub(crate) fn tighten(plane: CoveragePlane) -> Result<CoveragePlane, Error> {
    let b = plane.bounds;
    let w = b.width as usize;
    if w == 0 || plane.data.is_empty() {
        return Ok(plane);
    }
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for (r, row) in plane.data.chunks_exact(w).enumerate() {
        for (c, v) in row.iter().enumerate() {
            if *v != 0 {
                x0 = x0.min(c);
                y0 = y0.min(r);
                x1 = x1.max(c);
                y1 = y1.max(r);
            }
        }
    }
    if x0 == usize::MAX {
        return Ok(CoveragePlane {
            bounds: Rect {
                x: b.x,
                y: b.y,
                width: 0,
                height: 0,
            },
            data: Vec::new(),
        });
    }
    let nw = x1.checked_sub(x0).and_then(|v| v.checked_add(1)).ok_or(Error::Overflow)?;
    let nh = y1.checked_sub(y0).and_then(|v| v.checked_add(1)).ok_or(Error::Overflow)?;
    let mut data = Vec::with_capacity(nw.checked_mul(nh).ok_or(Error::Overflow)?);
    for row in plane.data.chunks_exact(w).skip(y0).take(nh) {
        data.extend_from_slice(row.get(x0..x0.saturating_add(nw)).ok_or(Error::Overflow)?);
    }
    Ok(CoveragePlane {
        bounds: Rect {
            x: b.x.checked_add(i64::try_from(x0).map_err(|_| Error::Overflow)?).ok_or(Error::Overflow)?,
            y: b.y.checked_add(i64::try_from(y0).map_err(|_| Error::Overflow)?).ok_or(Error::Overflow)?,
            width: u32::try_from(nw).map_err(|_| Error::Overflow)?,
            height: u32::try_from(nh).map_err(|_| Error::Overflow)?,
        },
        data,
    })
}

/// Tool surface description (Studio readings; Photoshop parity NOT_PROVEN, UNVERIFIED, D5).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"select_tools","version":1,"lane":"SMB-PS-SELECT-LIQUIFY","spec_basis":["STU-RAS-017","STU-RAS-018 flood select, quick/selection brush, object select","STU-RAS-019","STU-RAS-119 contiguous, sample_all_layers, anti_alias","STU-RAS-121","STU-RAS-122"],"wand":"magic_wand(layers, active, seed, WandOptions{tolerance 8-bit units, contiguous, sample_merged, sample_size, connectivity}); match = Chebyshev distance of premultiplied RGBA x 255 <= tolerance; contiguous = flood from the seed (4- or 8-connected), else global; anti_alias (document toggle) = 3x3 box mean on boundary pixels","quick":"quick_select strokes (brush engine dabs) -> graph cut: Boykov-Jolly 2001 hard seeds + GrabCut (Rother et al. 2004) n-links gamma exp(-beta |dI|^2), beta = 1 / (2 mean |dI|^2), 4-connected; regional term -ln of additively smoothed (0.01 per bin) 16^3 premultiplied-RGB histograms (initial other-side model = ROI ring + band of half the margin), re-estimated per iteration; Dinic max-flow in integer capacities; grabcut_rect = rectangle-initialised GrabCut; object_select refuses model_unavailable (no model, nothing downloaded)","refine":"refine_edge: STU-RAS-121 controls; guided filter (He et al. 2010) or closed-form matting (Levin et al. 2008, CG with the He et al. 2010 matrix-free Laplacian); see refine::DESCRIPTOR","parity":"NOT_PROVEN (Photoshop tolerance metric, AA kernel, quick-select energy UNVERIFIED, D5)"}"#;
