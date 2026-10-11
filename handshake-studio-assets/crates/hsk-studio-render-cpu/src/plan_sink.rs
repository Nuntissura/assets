//! CPU `RenderBackend` for the shared pigment render plan: admission, source verification, then
//! bounded `chunk x chunk` evaluation into a caller `TileSink`.
//!
//! The executor receives buffers already in the plan's colour tag and materialised data only
//! (STU-COL-163): it never resolves a profile, converts a space or calls a colour engine. Blend
//! arithmetic is the single pigment owner `blend_math::composite_over`; this module adds chunking,
//! groups, masks, vector coverage, budgets and cancellation around it.
//!
//! Clip-to-base (`MaskFormat::BaseAlpha`, pigment's documented run semantics, exactness
//! Approximate, Photoshop rule UNVERIFIED): a run `[base, c1..cn]` is evaluated per chunk as an
//! isolated unit `G` held in one scratch buffer. `G` = base content with the base's own mask (no
//! opacity, fill or mode); each `ci` is blended onto `G` with its own mode, opacity and fill and a
//! source alpha multiplied by the base alpha, then `G`'s alpha is held at the base alpha; `G` is
//! composited onto the running backdrop with the BASE layer's mode, opacity and fill. The base
//! alpha is `G`'s own alpha (it never changes during the run), so a clipped layer needs no plane.
use crate::coverage::PreparedPath;
use crate::vector::VectorSource;
use hsk_studio_accord::CancellationToken;
use hsk_studio_nib::PathLimits;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{BlendError, PixelSite, Rgba, composite_over};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, Applicability, BackendError, BackendIdentity, DeviceClass, Exactness,
    ExecuteRequest, Layer, MaskFormat, MaskInput, Operator, PlanNode, PlanSources, RenderBackend,
    RenderPlan, RenderReceipt, SampleFormat, SourceKey, StudioBlendMode, TileInput, TilePlanes,
    TileSink, admit,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

pub const RENDERER_NAME: &str = "hsk-studio-render-cpu";
pub const RENDERER_VERSION: &str = "0.1";
pub use hsk_studio_nib::MAX_PATH_ELEMENTS;
pub const MAX_CHUNK_SIDE: u32 = 4096;
pub const MAX_THREADS: usize = 64;
pub const DEFAULT_CHUNK_SIDE: u32 = 64;
/// Scratch charged per chunk pixel: 16 working (RGBA f32) + 12 colour out + 4 alpha out.
pub const SCRATCH_BYTES_PER_PIXEL: u64 = 32;
/// Extra scratch per chunk pixel for each level of nested isolated group (one RGBA f32 buffer).
pub const GROUP_SCRATCH_BYTES_PER_PIXEL: u64 = 16;

/// Why a source was refused; every case maps to `BackendError::SourceMismatch(key)` at the port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFault {
    /// Resolved planes differ from what the plan declared (size, hashes, format, alpha, tag).
    Declared,
    MalformedStride,
    HashMismatch,
    Nonfinite,
    AlphaRange,
    /// Mask plane does not contain every pixel its layer can touch inside the target.
    MaskGap,
}
impl SourceFault {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Declared => "source_declared_mismatch",
            Self::MalformedStride => "source_malformed_stride",
            Self::HashMismatch => "source_hash_mismatch",
            Self::Nonfinite => "source_nonfinite",
            Self::AlphaRange => "source_alpha_range",
            Self::MaskGap => "mask_coverage_gap",
        }
    }
}

/// Detailed internal error; `From<RenderError> for BackendError` is the port mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderError {
    Backend(BackendError),
    Source { key: SourceKey, fault: SourceFault },
    InvalidConfig(&'static str),
}
impl From<BackendError> for RenderError {
    fn from(error: BackendError) -> Self {
        Self::Backend(error)
    }
}
impl From<RenderError> for BackendError {
    fn from(error: RenderError) -> Self {
        match error {
            RenderError::Backend(e) => e,
            RenderError::Source { key, .. } => BackendError::SourceMismatch(key),
            RenderError::InvalidConfig(_) => BackendError::Unavailable("invalid_config"),
        }
    }
}
impl RenderError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Backend(e) => e.code(),
            Self::Source { fault, .. } => fault.code(),
            Self::InvalidConfig(_) => "invalid_config",
        }
    }
}
impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source { key, fault } => write!(f, "{}(key {})", fault.code(), key.0),
            Self::InvalidConfig(what) => write!(f, "invalid_config({what})"),
            Self::Backend(e) => write!(f, "{}", e.code()),
        }
    }
}
impl std::error::Error for RenderError {}

pub(crate) fn unsupported(operator: &'static str) -> RenderError {
    RenderError::Backend(BackendError::Unsupported {
        operator,
        exactness: Exactness::Unsupported,
    })
}
fn source(key: SourceKey, fault: SourceFault) -> RenderError {
    RenderError::Source { key, fault }
}
fn blend_error(error: BlendError) -> RenderError {
    match error {
        BlendError::OutOfDomain => unsupported("sample_range_outside_unit"),
        _ => unsupported("blend_mode"),
    }
}

/// True when this backend can evaluate `mode` as a layer mode (STU-RAS-155): layer-applicable
/// with a recovered function. `pass_through` (group-only) is a group operator, not a blend.
pub fn blend_supported(mode: StudioBlendMode) -> bool {
    let facts = mode.facts();
    facts.applicability == Applicability::Layer && facts.exactness != Exactness::Unsupported
}

/// Bounded CPU executor. Scratch is charged against the request's `max_working_bytes` before it is
/// allocated and released on every exit path by an RAII lease; `live_scratch_bytes` is 0 whenever
/// no render runs.
pub struct Renderer {
    chunk: u32,
    threads: usize,
    live: AtomicU64,
    peak: AtomicU64,
}
impl Default for Renderer {
    fn default() -> Self {
        Self::with_chunk(DEFAULT_CHUNK_SIDE)
    }
}

struct ScratchLease<'a> {
    owner: &'a Renderer,
    bytes: u64,
}
impl Drop for ScratchLease<'_> {
    fn drop(&mut self) {
        self.owner.live.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

// ---- prepared (verified) plan -------------------------------------------------------------

#[derive(Clone, Copy)]
struct MaskView<'a> {
    bounds: Rect,
    wide: bool,
    bytes: &'a [u8],
    stride: usize,
    inverted: bool,
}
impl MaskView<'_> {
    fn coverage(&self, x: i64, y: i64) -> f32 {
        let row = (y - self.bounds.y) as usize * self.stride;
        let c = if self.wide {
            let at = row + (x - self.bounds.x) as usize * 2;
            (f64::from(u16::from_le_bytes([self.bytes[at], self.bytes[at + 1]])) / 65535.0) as f32
        } else {
            (f64::from(self.bytes[row + (x - self.bounds.x) as usize]) / 255.0) as f32
        };
        if self.inverted { 1.0 - c } else { c }
    }
}

struct PLayer<'a> {
    input: usize,
    mode: StudioBlendMode,
    /// `opacity * fill_opacity`; fill is folded in as a product (class Approximate, see
    /// `Operator::FillOpacity`) until the Photoshop fill oracle exists.
    opacity: f32,
    mask: Option<MaskView<'a>>,
    /// Clip-to-base layer: its coverage is the alpha of the run's base (no plane, `mask` is None).
    clipped: bool,
}
fn is_clipped(layer: &Layer) -> bool {
    layer.mask.as_ref().is_some_and(MaskInput::is_clip_to_base)
}
enum PNode<'a> {
    Unused,
    Tile {
        planes: TilePlanes<'a>,
        rect: Rect,
    },
    Solid {
        rgba: [f32; 4],
    },
    Vector {
        path: PreparedPath,
        rgba: [f32; 4],
    },
    Group {
        layers: Vec<PLayer<'a>>,
        isolated: bool,
    },
}
struct Prepared<'a> {
    nodes: Vec<PNode<'a>>,
    bounds: Vec<Option<Rect>>,
    /// Scratch buffers an evaluation of the node needs below its own destination.
    need: Vec<usize>,
    root: usize,
    reachable: u64,
    seed: u64,
}

fn end(rect: Rect) -> Result<(i64, i64), RenderError> {
    rect.end()
        .map_err(|_| RenderError::Backend(BackendError::Overflow))
}
fn intersect(a: Rect, b: Rect) -> Result<Option<Rect>, RenderError> {
    a.intersect(b)
        .map_err(|_| RenderError::Backend(BackendError::Overflow))
}
fn union(a: Option<Rect>, b: Option<Rect>) -> Result<Option<Rect>, RenderError> {
    let (Some(a), Some(b)) = (a, b) else {
        return Ok(a.or(b));
    };
    let (ax1, ay1) = end(a)?;
    let (bx1, by1) = end(b)?;
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    let overflow = |_| RenderError::Backend(BackendError::Overflow);
    Ok(Some(Rect {
        x,
        y,
        width: u32::try_from(ax1.max(bx1) - x).map_err(overflow)?,
        height: u32::try_from(ay1.max(by1) - y).map_err(overflow)?,
    }))
}
fn unit_rgb(rgba: [f32; 4]) -> Result<(), RenderError> {
    if rgba
        .iter()
        .all(|c| c.is_finite() && (0.0..=1.0).contains(c))
    {
        Ok(())
    } else {
        Err(unsupported("sample_range_outside_unit"))
    }
}

fn plane_ok(
    bytes: &[u8],
    stride: u64,
    width: u32,
    height: u32,
    bpp: u64,
) -> Result<(), SourceFault> {
    let row = u64::from(width) * bpp;
    let size = stride.checked_mul(u64::from(height));
    if stride < row || !stride.is_multiple_of(4) || u64::try_from(bytes.len()).ok() != size {
        return Err(SourceFault::MalformedStride);
    }
    Ok(())
}
fn f32_at(bytes: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
fn tile_pixel(p: &TilePlanes<'_>, x: u32, y: u32) -> [f32; 4] {
    let c = y as usize * p.colour_stride_bytes as usize + x as usize * 12;
    let a = y as usize * p.alpha_stride_bytes as usize + x as usize * 4;
    [
        f32_at(p.colour, c),
        f32_at(p.colour, c + 4),
        f32_at(p.colour, c + 8),
        f32_at(p.alpha, a),
    ]
}

/// Shape, plane hashes and values of a resolved tile against the plan's declaration.
fn verify_tile(
    input: &TileInput,
    planes: &TilePlanes<'_>,
    plan_space: hsk_studio_pigment::render_plan::ColourTag,
) -> Result<(), RenderError> {
    let key = input.key;
    if input.alpha != AlphaConvention::Straight {
        return Err(unsupported("alpha_premultiplied"));
    }
    if !planes.matches(input, plan_space) {
        return Err(source(key, SourceFault::Declared));
    }
    plane_ok(
        planes.colour,
        planes.colour_stride_bytes,
        planes.width,
        planes.height,
        12,
    )
    .and_then(|()| {
        plane_ok(
            planes.alpha,
            planes.alpha_stride_bytes,
            planes.width,
            planes.height,
            4,
        )
    })
    .map_err(|f| source(key, f))?;
    if hsk_studio_prism::profile_hash(planes.colour) != planes.colour_sha256
        || hsk_studio_prism::profile_hash(planes.alpha) != planes.alpha_sha256
    {
        return Err(source(key, SourceFault::HashMismatch));
    }
    let mut out_of_unit = false;
    for y in 0..planes.height {
        for x in 0..planes.width {
            let [r, g, b, a] = tile_pixel(planes, x, y);
            if ![r, g, b, a].iter().all(|v| v.is_finite()) {
                return Err(source(key, SourceFault::Nonfinite));
            }
            if !(0.0..=1.0).contains(&a) {
                return Err(source(key, SourceFault::AlphaRange));
            }
            out_of_unit |= ![r, g, b].iter().all(|v| (0.0..=1.0).contains(v));
        }
    }
    if out_of_unit {
        return Err(unsupported("sample_range_outside_unit"));
    }
    Ok(())
}

fn prepare<'a, V: VectorSource>(
    plan: &RenderPlan<V>,
    sources: &'a dyn PlanSources,
) -> Result<Prepared<'a>, RenderError> {
    let target = plan.target;
    if target.alpha != AlphaConvention::Straight {
        return Err(unsupported("alpha_premultiplied"));
    }
    if target.format != SampleFormat::RgbF32Le {
        return Err(unsupported("sample_format"));
    }
    let n = plan.nodes.len();
    let root = plan.root.0 as usize;
    let mut reach = vec![false; n];
    reach[root] = true;
    for i in (0..n).rev() {
        if reach[i]
            && let PlanNode::Group(g) = &plan.nodes[i]
        {
            for l in &g.layers {
                reach[l.input.0 as usize] = true;
            }
        }
    }
    let mut nodes: Vec<PNode<'a>> = Vec::with_capacity(n);
    let mut bounds: Vec<Option<Rect>> = Vec::with_capacity(n);
    let mut need: Vec<usize> = Vec::with_capacity(n);
    for (i, node) in plan.nodes.iter().enumerate() {
        if !reach[i] {
            nodes.push(PNode::Unused);
            bounds.push(None);
            need.push(0);
            continue;
        }
        let (pnode, node_bounds, node_need) = match node {
            PlanNode::Tile(t) => {
                let planes = sources
                    .tile(t.key)
                    .ok_or(BackendError::UnresolvedSource(t.key))?;
                verify_tile(t, &planes, target.space)?;
                end(t.placement)?;
                (
                    PNode::Tile {
                        planes,
                        rect: t.placement,
                    },
                    Some(t.placement),
                    0,
                )
            }
            PlanNode::Solid { rect, rgba } => {
                unit_rgb(*rgba)?;
                end(*rect)?;
                (PNode::Solid { rgba: *rgba }, Some(*rect), 0)
            }
            PlanNode::Vector(v) => {
                let fill = v.fill();
                unit_rgb(fill.rgba)?;
                let path =
                    PreparedPath::prepare(&fill.path, fill.aa_samples, &PathLimits::default())?;
                end(path.bounds)?;
                let bounds = path.bounds;
                (
                    PNode::Vector {
                        path,
                        rgba: fill.rgba,
                    },
                    Some(bounds),
                    0,
                )
            }
            PlanNode::Group(g) => {
                let mut layers = Vec::with_capacity(g.layers.len());
                let mut group_bounds = None;
                let mut group_need = 0;
                // Scratch beneath the compositing buffer of the clip run being read (None outside
                // a run): the base evaluates straight into the run buffer, clipped members after.
                let mut run_need: Option<usize> = None;
                for (li, l) in g.layers.iter().enumerate() {
                    let input = l.input.0 as usize;
                    let child = &plan.nodes[input];
                    let pass_through = l.mode == StudioBlendMode::PassThrough;
                    let clipped = is_clipped(l);
                    let starts_run = !clipped && g.layers.get(li + 1).is_some_and(is_clipped);
                    // A pass-through group has no isolated content to take a base alpha from.
                    if starts_run && pass_through {
                        return Err(unsupported("clip_base_pass_through"));
                    }
                    let child_isolated = match child {
                        PlanNode::Group(c) => Some(c.isolated),
                        _ => None,
                    };
                    // Structural validation already guaranteed pass_through <-> non-isolated group.
                    if pass_through && l.opacity < 1.0 {
                        return Err(unsupported("group_pass_through_opacity"));
                    }
                    if !pass_through {
                        if child_isolated == Some(false) {
                            return Err(unsupported("group_non_isolated_blended"));
                        }
                        if !blend_supported(l.mode) {
                            return Err(unsupported("blend_mode"));
                        }
                    }
                    let mask = match &l.mask {
                        None => None,
                        // Canonical clip-to-base encoding (pigment validated): no plane to resolve.
                        Some(_) if clipped => None,
                        Some(m) => {
                            if m.density != 1.0 {
                                return Err(unsupported("mask_density"));
                            }
                            if pass_through {
                                return Err(unsupported("mask_on_pass_through"));
                            }
                            let plane = sources
                                .mask(m.key)
                                .ok_or(BackendError::UnresolvedSource(m.key))?;
                            let wide = match m.format {
                                MaskFormat::CoverageU8 => false,
                                MaskFormat::CoverageU16Le => true,
                                _ => return Err(unsupported("mask_format")),
                            };
                            if plane.sample_format != m.format.key()
                                || plane.sha256 != m.sha256
                                || plane.bounds != m.placement
                            {
                                return Err(source(m.key, SourceFault::Declared));
                            }
                            plane.validate(plane.bounds).map_err(|e| {
                                source(
                                    m.key,
                                    if e == hsk_studio_pigment::Error::HashMismatch {
                                        SourceFault::HashMismatch
                                    } else {
                                        SourceFault::MalformedStride
                                    },
                                )
                            })?;
                            if let Some(region) = bounds[input]
                                .and_then(|b| intersect(b, target.extent).ok().flatten())
                                && !plane
                                    .bounds
                                    .contains(region)
                                    .map_err(|_| BackendError::Overflow)?
                            {
                                return Err(source(m.key, SourceFault::MaskGap));
                            }
                            Some(MaskView {
                                bounds: plane.bounds,
                                wide,
                                bytes: plane.bytes,
                                stride: plane.stride_bytes as usize,
                                inverted: m.inverted,
                            })
                        }
                    };
                    let child_need = match child_isolated {
                        Some(true) => 1 + need[input],
                        _ => need[input],
                    };
                    if clipped {
                        // Clipped content never extends beyond its base: no group extent.
                        run_need = Some(run_need.map_or(child_need, |r| r.max(child_need)));
                    } else {
                        if let Some(r) = run_need.take() {
                            group_need = group_need.max(1 + r);
                        }
                        group_bounds = union(group_bounds, bounds[input])?;
                        if starts_run {
                            run_need = Some(need[input]);
                        } else {
                            group_need = group_need.max(child_need);
                        }
                    }
                    layers.push(PLayer {
                        input,
                        mode: l.mode,
                        opacity: l.opacity * l.fill_opacity,
                        mask,
                        clipped,
                    });
                }
                if let Some(r) = run_need {
                    group_need = group_need.max(1 + r);
                }
                (
                    PNode::Group {
                        layers,
                        isolated: g.isolated,
                    },
                    group_bounds,
                    group_need,
                )
            }
        };
        nodes.push(pnode);
        bounds.push(node_bounds);
        need.push(node_need);
    }
    Ok(Prepared {
        nodes,
        bounds,
        need,
        root,
        reachable: reach.iter().filter(|r| **r).count() as u64,
        seed: plan.revision,
    })
}

// ---- evaluation ------------------------------------------------------------------------------

struct Eval<'p, 'a> {
    prepared: &'p Prepared<'a>,
}

impl Eval<'_, '_> {
    /// Layers of group `idx`, bottom-first, composited into `dst` (a chunk-sized buffer).
    /// `spare` holds one scratch buffer per remaining level of isolated-group nesting.
    fn group(
        &self,
        idx: usize,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
    ) -> Result<(), RenderError> {
        let PNode::Group { layers, .. } = &self.prepared.nodes[idx] else {
            return Err(unsupported("plan_shape"));
        };
        let mut at = 0;
        while at < layers.len() {
            let base = &layers[at];
            // Pigment validation guarantees a clipped layer follows an unclipped one.
            if base.clipped {
                return Err(unsupported("plan_shape"));
            }
            let clipped = layers[at + 1..].iter().take_while(|l| l.clipped).count();
            if clipped == 0 {
                self.layer(base, chunk, dst, spare)?;
            } else {
                self.clip_run(base, &layers[at + 1..=at + clipped], chunk, dst, spare)?;
            }
            at += 1 + clipped;
        }
        Ok(())
    }

    /// One clipping run: `base` plus the layers clipped to it, through the run buffer `G` (see the
    /// module docs). Everything is per pixel, so the result is independent of chunking.
    fn clip_run(
        &self,
        base: &PLayer<'_>,
        clipped: &[PLayer<'_>],
        chunk: Rect,
        dst: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
    ) -> Result<(), RenderError> {
        // Outside the base nothing of the run is visible.
        let Some(region) = self.prepared.bounds[base.input]
            .map(|b| intersect(b, chunk))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        let count = chunk.width as usize * chunk.height as usize;
        let (buffer, deeper) = spare
            .split_first_mut()
            .ok_or(BackendError::Unavailable("group_scratch"))?;
        let run = &mut buffer[..count];
        run.fill([0.0; 4]);
        self.clip_base(base, region, chunk, run, deeper)?;
        for layer in clipped {
            self.clip_layer(layer, chunk, run, deeper)?;
        }
        // The base's own mask is already in `G`; opacity, fill and mode are the base layer's.
        let group = PLayer {
            input: base.input,
            mode: base.mode,
            opacity: base.opacity,
            mask: None,
            clipped: false,
        };
        let result: &[[f32; 4]] = run;
        self.composite(&group, region, chunk, dst, |_, _, at| (result[at], 1.0))
    }

    /// `G` := base content with the base's own mask (alpha scaled), without opacity, fill or mode.
    fn clip_base(
        &self,
        base: &PLayer<'_>,
        region: Rect,
        chunk: Rect,
        run: &mut [[f32; 4]],
        deeper: &mut [Vec<[f32; 4]>],
    ) -> Result<(), RenderError> {
        match &self.prepared.nodes[base.input] {
            PNode::Group { isolated: true, .. } => {
                self.group(base.input, chunk, run, deeper)?;
                seal_base(base, region, chunk, run, |_, _, current| (current, 1.0));
            }
            PNode::Tile { planes, rect } => seal_base(base, region, chunk, run, |x, y, _| {
                (
                    tile_pixel(planes, (x - rect.x) as u32, (y - rect.y) as u32),
                    1.0,
                )
            }),
            PNode::Solid { rgba } => seal_base(base, region, chunk, run, |_, _, _| (*rgba, 1.0)),
            PNode::Vector { path, rgba } => {
                seal_base(base, region, chunk, run, |x, y, _| {
                    (*rgba, path.coverage(x, y))
                });
            }
            // A pass-through base is refused by `prepare`.
            PNode::Group {
                isolated: false, ..
            }
            | PNode::Unused => return Err(unsupported("plan_shape")),
        }
        Ok(())
    }

    /// Blend one clipped layer onto `G`.
    fn clip_layer(
        &self,
        layer: &PLayer<'_>,
        chunk: Rect,
        run: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
    ) -> Result<(), RenderError> {
        let Some(region) = self.prepared.bounds[layer.input]
            .map(|b| intersect(b, chunk))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        match &self.prepared.nodes[layer.input] {
            PNode::Group { isolated: true, .. } => {
                let count = chunk.width as usize * chunk.height as usize;
                let (buffer, deeper) = spare
                    .split_first_mut()
                    .ok_or(BackendError::Unavailable("group_scratch"))?;
                let buffer = &mut buffer[..count];
                buffer.fill([0.0; 4]);
                self.group(layer.input, chunk, buffer, deeper)?;
                let result: &[[f32; 4]] = buffer;
                self.composite_clipped(layer, region, chunk, run, |_, _, at| (result[at], 1.0))
            }
            PNode::Tile { planes, rect } => {
                self.composite_clipped(layer, region, chunk, run, |x, y, _| {
                    (
                        tile_pixel(planes, (x - rect.x) as u32, (y - rect.y) as u32),
                        1.0,
                    )
                })
            }
            PNode::Solid { rgba } => {
                self.composite_clipped(layer, region, chunk, run, |_, _, _| (*rgba, 1.0))
            }
            PNode::Vector { path, rgba } => {
                self.composite_clipped(layer, region, chunk, run, |x, y, _| {
                    (*rgba, path.coverage(x, y))
                })
            }
            // Pass-through clipped layers are refused by pigment validation, non-isolated
            // blended groups by `prepare`.
            PNode::Group {
                isolated: false, ..
            }
            | PNode::Unused => Err(unsupported("plan_shape")),
        }
    }

    /// Like [`Eval::composite`] for a clipped layer onto `G`: the source alpha is multiplied by the
    /// base alpha (`G`'s own alpha), and `G`'s alpha is held at the base alpha afterwards.
    fn composite_clipped(
        &self,
        layer: &PLayer<'_>,
        region: Rect,
        chunk: Rect,
        run: &mut [[f32; 4]],
        mut source: impl FnMut(i64, i64, usize) -> ([f32; 4], f32),
    ) -> Result<(), RenderError> {
        let width = chunk.width as usize;
        for y in region.y..region.y + i64::from(region.height) {
            for x in region.x..region.x + i64::from(region.width) {
                let at = (y - chunk.y) as usize * width + (x - chunk.x) as usize;
                let b = run[at];
                let base_alpha = b[3];
                if base_alpha <= 0.0 {
                    continue;
                }
                let (rgba, coverage) = source(x, y, at);
                let opacity = layer.opacity * coverage;
                let source_alpha = rgba[3] * base_alpha;
                if opacity <= 0.0 || source_alpha <= 0.0 {
                    continue;
                }
                let out = composite_over(
                    layer.mode,
                    Rgba::new([b[0], b[1], b[2]], base_alpha),
                    Rgba::new([rgba[0], rgba[1], rgba[2]], source_alpha),
                    opacity,
                    PixelSite {
                        x,
                        y,
                        seed: self.prepared.seed,
                    },
                )
                .map_err(blend_error)?;
                run[at] = [out.rgb[0], out.rgb[1], out.rgb[2], base_alpha];
            }
        }
        Ok(())
    }

    fn layer(
        &self,
        layer: &PLayer<'_>,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
    ) -> Result<(), RenderError> {
        let Some(region) = self.prepared.bounds[layer.input]
            .map(|b| intersect(b, chunk))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        match &self.prepared.nodes[layer.input] {
            PNode::Group {
                isolated: false, ..
            } => self.group(layer.input, chunk, dst, spare),
            PNode::Group { isolated: true, .. } => {
                let count = chunk.width as usize * chunk.height as usize;
                let (buffer, deeper) = spare
                    .split_first_mut()
                    .ok_or(BackendError::Unavailable("group_scratch"))?;
                let buffer = &mut buffer[..count];
                buffer.fill([0.0; 4]);
                self.group(layer.input, chunk, buffer, deeper)?;
                let result: &[[f32; 4]] = buffer;
                self.composite(layer, region, chunk, dst, |_, _, at| (result[at], 1.0))
            }
            PNode::Tile { planes, rect } => self.composite(layer, region, chunk, dst, |x, y, _| {
                (
                    tile_pixel(planes, (x - rect.x) as u32, (y - rect.y) as u32),
                    1.0,
                )
            }),
            PNode::Solid { rgba } => {
                self.composite(layer, region, chunk, dst, |_, _, _| (*rgba, 1.0))
            }
            PNode::Vector { path, rgba } => self.composite(layer, region, chunk, dst, |x, y, _| {
                (*rgba, path.coverage(x, y))
            }),
            PNode::Unused => Err(unsupported("plan_shape")),
        }
    }

    /// Blend `source(x, y, index) = (straight RGBA, coverage)` into `dst` over `region`.
    fn composite(
        &self,
        layer: &PLayer<'_>,
        region: Rect,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        mut source: impl FnMut(i64, i64, usize) -> ([f32; 4], f32),
    ) -> Result<(), RenderError> {
        let width = chunk.width as usize;
        for y in region.y..region.y + i64::from(region.height) {
            for x in region.x..region.x + i64::from(region.width) {
                let at = (y - chunk.y) as usize * width + (x - chunk.x) as usize;
                let mask = layer.mask.map_or(1.0, |m| m.coverage(x, y));
                if mask <= 0.0 {
                    continue;
                }
                let (rgba, coverage) = source(x, y, at);
                let opacity = layer.opacity * coverage * mask;
                if opacity <= 0.0 || rgba[3] <= 0.0 {
                    continue;
                }
                let b = dst[at];
                let out = composite_over(
                    layer.mode,
                    Rgba::new([b[0], b[1], b[2]], b[3]),
                    Rgba::new([rgba[0], rgba[1], rgba[2]], rgba[3]),
                    opacity,
                    PixelSite {
                        x,
                        y,
                        seed: self.prepared.seed,
                    },
                )
                .map_err(blend_error)?;
                // Alpha can round 1 ulp above 1; keep the working buffer inside the blend domain.
                dst[at] = [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)];
            }
        }
        Ok(())
    }
}

/// Write the clip base into `run` over `region`: `source(x, y, current)` gives the straight RGBA
/// and coverage (`current` is what `run` already holds, for an isolated-group base); the base's
/// own mask scales the alpha. Fully transparent pixels are stored as `[0; 4]`.
fn seal_base(
    base: &PLayer<'_>,
    region: Rect,
    chunk: Rect,
    run: &mut [[f32; 4]],
    source: impl Fn(i64, i64, [f32; 4]) -> ([f32; 4], f32),
) {
    let width = chunk.width as usize;
    for y in region.y..region.y + i64::from(region.height) {
        for x in region.x..region.x + i64::from(region.width) {
            let at = (y - chunk.y) as usize * width + (x - chunk.x) as usize;
            let mask = base.mask.map_or(1.0, |m| m.coverage(x, y));
            let (rgba, coverage) = source(x, y, run[at]);
            let alpha = rgba[3] * coverage * mask;
            run[at] = if alpha > 0.0 {
                [rgba[0], rgba[1], rgba[2], alpha]
            } else {
                [0.0; 4]
            };
        }
    }
}

/// Per-worker buffers for one chunk: working RGBA, one buffer per level of isolated-group nesting,
/// and the two output planes.
struct Scratch {
    work: Vec<[f32; 4]>,
    spare: Vec<Vec<[f32; 4]>>,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Scratch {
    fn new(pixels: usize, depth: usize) -> Self {
        Self {
            work: vec![[0.0; 4]; pixels],
            spare: (0..depth).map(|_| vec![[0.0; 4]; pixels]).collect(),
            colour: Vec::with_capacity(pixels * 12),
            alpha: Vec::with_capacity(pixels * 4),
        }
    }
}

/// Where chunk `index` (row-major) sits in the extent.
#[derive(Clone, Copy)]
struct Geometry {
    extent: Rect,
    side: i64,
    columns: usize,
    total: usize,
}
impl Geometry {
    fn new(extent: Rect, side: u32) -> Result<Self, RenderError> {
        let side = i64::from(side);
        let columns = usize::try_from((i64::from(extent.width) + side - 1) / side)
            .map_err(|_| BackendError::Overflow)?;
        let rows = usize::try_from((i64::from(extent.height) + side - 1) / side)
            .map_err(|_| BackendError::Overflow)?;
        Ok(Self {
            extent,
            side,
            columns,
            total: columns.checked_mul(rows).ok_or(BackendError::Overflow)?,
        })
    }
    fn rect(&self, index: usize) -> Rect {
        let (column, row) = ((index % self.columns) as i64, (index / self.columns) as i64);
        let (x, y) = (
            self.extent.x + column * self.side,
            self.extent.y + row * self.side,
        );
        let right = self.extent.x + i64::from(self.extent.width);
        let bottom = self.extent.y + i64::from(self.extent.height);
        Rect {
            x,
            y,
            width: self.side.min(right - x) as u32,
            height: self.side.min(bottom - y) as u32,
        }
    }
}

/// Evaluate the plan for one chunk into `scratch.colour` / `scratch.alpha`.
fn compute_chunk(
    eval: &Eval<'_, '_>,
    rect: Rect,
    scratch: &mut Scratch,
) -> Result<(), RenderError> {
    let prepared = eval.prepared;
    let count = rect.width as usize * rect.height as usize;
    scratch.work[..count].fill([0.0; 4]);
    if matches!(prepared.nodes[prepared.root], PNode::Group { .. }) {
        // The root backdrop is transparent, so isolated and not are the same here.
        eval.group(
            prepared.root,
            rect,
            &mut scratch.work[..count],
            &mut scratch.spare,
        )?;
    } else {
        // A leaf root is one Normal, opacity-1 layer over transparent black.
        let root_layer = PLayer {
            input: prepared.root,
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            mask: None,
            clipped: false,
        };
        eval.layer(
            &root_layer,
            rect,
            &mut scratch.work[..count],
            &mut scratch.spare,
        )?;
    }
    scratch.colour.clear();
    scratch.alpha.clear();
    for [r, g, b, a] in &scratch.work[..count] {
        scratch.colour.extend_from_slice(&r.to_le_bytes());
        scratch.colour.extend_from_slice(&g.to_le_bytes());
        scratch.colour.extend_from_slice(&b.to_le_bytes());
        scratch.alpha.extend_from_slice(&a.to_le_bytes());
    }
    Ok(())
}

/// Cancel, then deadline, as observed before every chunk.
fn between_chunks(
    cancel: &CancellationToken,
    deadline: Option<Instant>,
) -> Result<(), BackendError> {
    cancel.check().map_err(|_| BackendError::Canceled)?;
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Err(BackendError::DeadlineExceeded);
    }
    Ok(())
}

enum Message {
    Chunk(usize, Vec<u8>, Vec<u8>),
    Failed(RenderError),
}

impl Renderer {
    pub fn new(chunk: u32) -> Result<Self, RenderError> {
        Self::with_threads(chunk, 1)
    }
    /// `threads > 1` evaluates chunks on that many scoped worker threads. Output bytes and the
    /// sink call order (row-major chunks, cancel and deadline observed before each delivery) are
    /// identical to the single-threaded run; only the scratch charge grows (see
    /// [`Renderer::scratch_bytes`]).
    pub fn with_threads(chunk: u32, threads: usize) -> Result<Self, RenderError> {
        if chunk == 0 || chunk > MAX_CHUNK_SIDE {
            return Err(RenderError::InvalidConfig("chunk"));
        }
        if threads == 0 || threads > MAX_THREADS {
            return Err(RenderError::InvalidConfig("threads"));
        }
        Ok(Self {
            chunk,
            threads,
            live: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        })
    }
    fn with_chunk(chunk: u32) -> Self {
        Self {
            chunk,
            threads: 1,
            live: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        }
    }
    pub fn chunk(&self) -> u32 {
        self.chunk
    }
    pub fn threads(&self) -> usize {
        self.threads
    }
    /// Scratch bytes currently charged; 0 when no render is running.
    pub fn live_scratch_bytes(&self) -> u64 {
        self.live.load(Ordering::Acquire)
    }
    /// Highest simultaneous charge over this renderer's lifetime.
    pub fn peak_scratch_bytes(&self) -> u64 {
        self.peak.load(Ordering::Acquire)
    }
    /// Scratch charged for chunks of at most `pixels` pixels, isolated-group nesting `depth` and
    /// `workers` workers: every worker owns `32 + 16 * depth` bytes per pixel, and a parallel run
    /// also holds up to `2 * workers` finished chunks (`16` bytes per pixel) awaiting in-order
    /// delivery.
    pub fn scratch_bytes(pixels: u64, depth: u64, workers: u64) -> Option<u64> {
        let per_worker = SCRATCH_BYTES_PER_PIXEL + GROUP_SCRATCH_BYTES_PER_PIXEL * depth;
        let in_flight = if workers > 1 { 2 * workers * 16 } else { 0 };
        pixels.checked_mul(workers.checked_mul(per_worker)?.checked_add(in_flight)?)
    }

    fn reserve(&self, bytes: u64, limit: u64) -> Result<ScratchLease<'_>, RenderError> {
        if bytes > limit {
            return Err(BackendError::BudgetExceeded {
                requested: bytes,
                limit,
            }
            .into());
        }
        let live = self.live.fetch_add(bytes, Ordering::AcqRel) + bytes;
        self.peak.fetch_max(live, Ordering::AcqRel);
        Ok(ScratchLease { owner: self, bytes })
    }

    /// Execute with the detailed internal error. Order: admission (cancel, stale revision, plan
    /// validation, per-operator exactness), source verification (so a bad plan or source never
    /// reaches the sink), then chunks. Cancel and deadline are observed before every chunk. A
    /// receipt exists only after the last chunk was accepted.
    pub fn execute_detailed<V: VectorSource + Serialize + Send + Sync>(
        &self,
        plan: &RenderPlan<V>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, RenderError> {
        let ledger = admit::<V>(plan, self, request, cancel)?;
        let prepared = prepare(plan, sources)?;
        let digest = plan
            .digest()
            .map_err(|_| BackendError::Unavailable("plan_digest"))?;

        let extent = plan.target.extent;
        end(extent)?;
        let geometry = Geometry::new(extent, self.chunk)?;
        let workers = self.threads.min(geometry.total);
        let max_pixels =
            u64::from(self.chunk.min(extent.width)) * u64::from(self.chunk.min(extent.height));
        let depth = prepared.need[prepared.root];
        let scratch_bytes = Self::scratch_bytes(max_pixels, depth as u64, workers as u64)
            .ok_or(BackendError::Overflow)?;
        let _lease = self.reserve(scratch_bytes, request.max_working_bytes)?;
        let eval = Eval {
            prepared: &prepared,
        };
        let pixels = max_pixels as usize;

        let chunks = if workers == 1 {
            let mut scratch = Scratch::new(pixels, depth);
            for index in 0..geometry.total {
                between_chunks(cancel, request.deadline)?;
                let rect = geometry.rect(index);
                compute_chunk(&eval, rect, &mut scratch)?;
                sink.accept(rect, &scratch.colour, &scratch.alpha)
                    .map_err(BackendError::Sink)?;
            }
            geometry.total as u64
        } else {
            run_parallel(
                &eval,
                geometry,
                workers,
                pixels,
                depth,
                request.deadline,
                cancel,
                sink,
            )?
        };
        Ok(RenderReceipt {
            plan_revision: plan.revision,
            plan_digest: digest,
            backend: identity(),
            region: extent,
            target: plan.target,
            exactness: ledger,
            chunks,
            nodes_executed: prepared.reachable,
            peak_working_bytes: scratch_bytes,
        })
    }
}

/// Workers claim chunk indices in increasing order and may run at most `2 * workers` chunks ahead
/// of delivery; the calling thread reorders results and is the only one that touches `sink`, so the
/// sink sees row-major chunks exactly as in the single-threaded run.
#[allow(clippy::too_many_arguments)]
fn run_parallel(
    eval: &Eval<'_, '_>,
    geometry: Geometry,
    workers: usize,
    pixels: usize,
    depth: usize,
    deadline: Option<Instant>,
    cancel: &CancellationToken,
    sink: &mut dyn TileSink,
) -> Result<u64, RenderError> {
    let window = 2 * workers;
    let next = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);
    let gate = (Mutex::new(0_usize), Condvar::new());
    let (tx, rx) = mpsc::channel::<Message>();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (next, abort, gate) = (&next, &abort, &gate);
            scope.spawn(move || {
                let mut scratch = Scratch::new(pixels, depth);
                loop {
                    let index = next.fetch_add(1, Ordering::AcqRel);
                    if index >= geometry.total || abort.load(Ordering::Acquire) {
                        return;
                    }
                    // Stay within the reorder window of the delivered prefix.
                    {
                        let (delivered, wake) = gate;
                        let mut guard = delivered.lock().unwrap_or_else(PoisonError::into_inner);
                        while index >= *guard + window && !abort.load(Ordering::Acquire) {
                            guard = wake
                                .wait_timeout(guard, Duration::from_millis(20))
                                .unwrap_or_else(PoisonError::into_inner)
                                .0;
                        }
                    }
                    if abort.load(Ordering::Acquire) {
                        return;
                    }
                    let outcome = between_chunks(cancel, deadline)
                        .map_err(RenderError::from)
                        .and_then(|()| compute_chunk(eval, geometry.rect(index), &mut scratch));
                    let message = match outcome {
                        Ok(()) => Message::Chunk(
                            index,
                            std::mem::replace(&mut scratch.colour, Vec::with_capacity(pixels * 12)),
                            std::mem::replace(&mut scratch.alpha, Vec::with_capacity(pixels * 4)),
                        ),
                        Err(error) => Message::Failed(error),
                    };
                    let failed = matches!(message, Message::Failed(_));
                    if tx.send(message).is_err() || failed {
                        return;
                    }
                }
            });
        }
        drop(tx);

        let mut pending: BTreeMap<usize, (Vec<u8>, Vec<u8>)> = BTreeMap::new();
        let mut delivered = 0_usize;
        let result = 'deliver: loop {
            if delivered == geometry.total {
                break Ok(delivered as u64);
            }
            match rx.recv() {
                Ok(Message::Chunk(index, colour, alpha)) => {
                    pending.insert(index, (colour, alpha));
                    while let Some((colour, alpha)) = pending.remove(&delivered) {
                        if let Err(error) = between_chunks(cancel, deadline) {
                            break 'deliver Err(error.into());
                        }
                        if let Err(error) = sink.accept(geometry.rect(delivered), &colour, &alpha) {
                            break 'deliver Err(BackendError::Sink(error).into());
                        }
                        delivered += 1;
                        *gate.0.lock().unwrap_or_else(PoisonError::into_inner) = delivered;
                        gate.1.notify_all();
                    }
                }
                Ok(Message::Failed(error)) => break Err(error),
                Err(_) => break Err(BackendError::Unavailable("worker_lost").into()),
            }
        };
        abort.store(true, Ordering::Release);
        gate.1.notify_all();
        result
    })
}

fn identity() -> BackendIdentity {
    BackendIdentity {
        name: RENDERER_NAME,
        version: RENDERER_VERSION,
        device: DeviceClass::Cpu,
        adapter: String::new(),
        generation: 0,
    }
}

impl<V: VectorSource + Serialize + Send + Sync> RenderBackend<V> for Renderer {
    fn identity(&self) -> BackendIdentity {
        identity()
    }
    fn exactness(&self, operator: Operator) -> Exactness {
        match operator {
            // Pigment's run semantics are implemented as documented; the Photoshop rule is
            // UNVERIFIED, so the class stays Approximate.
            Operator::Mask(MaskFormat::BaseAlpha) => Exactness::Approximate,
            Operator::Tile
            | Operator::Solid
            | Operator::IsolatedGroup
            | Operator::PassThroughGroup
            | Operator::Mask(_) => Exactness::Exact,
            // Supersampled coverage approximates area coverage; exact only on sample-aligned edges.
            Operator::Vector | Operator::FillOpacity => Exactness::Approximate,
            Operator::Blend(mode) => {
                if blend_supported(mode) {
                    mode.facts().exactness
                } else {
                    Exactness::Unsupported
                }
            }
            Operator::PassThroughOpacity | Operator::NonIsolatedBlendedGroup => {
                Exactness::Unsupported
            }
        }
    }
    fn supports_clip_to_base(&self) -> bool {
        true
    }
    fn execute(
        &self,
        plan: &RenderPlan<V>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        self.execute_detailed(plan, request, sources, cancel, sink)
            .map_err(BackendError::from)
    }
}
