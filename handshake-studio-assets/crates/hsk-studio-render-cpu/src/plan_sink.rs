//! CPU `RenderBackend` for the shared pigment render plan: admission, source verification, then
//! bounded `chunk x chunk` evaluation into a caller `TileSink`.
//!
//! The executor receives buffers already in the plan's colour tag and materialised data only
//! (STU-COL-163): it never resolves a profile, converts a space or calls a colour engine. Blend
//! arithmetic is the single pigment owner `blend_math::composite_over`; this module adds chunking,
//! groups, masks, vector coverage, budgets and cancellation around it.
use crate::coverage::PreparedPath;
use crate::vector::VectorSource;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{BlendError, PixelSite, Rgba, composite_over};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, Applicability, BackendError, BackendIdentity, DeviceClass, Exactness,
    ExecuteRequest, MaskFormat, Operator, PlanNode, PlanSources, RenderBackend, RenderPlan,
    RenderReceipt, SampleFormat, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
    admit,
};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub const RENDERER_NAME: &str = "hsk-studio-render-cpu";
pub const RENDERER_VERSION: &str = "0.1";
pub const MAX_ANCHORS: usize = 65_536;
pub const MAX_CHUNK_SIDE: u32 = 4096;
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
                if fill.anchors.len() > MAX_ANCHORS {
                    return Err(BackendError::BudgetExceeded {
                        requested: fill.anchors.len() as u64,
                        limit: MAX_ANCHORS as u64,
                    }
                    .into());
                }
                let path = PreparedPath::prepare(
                    &fill.anchors,
                    fill.closed,
                    fill.winding,
                    fill.aa_samples,
                    fill.pixels_per_inch,
                )?;
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
                for l in &g.layers {
                    let input = l.input.0 as usize;
                    let child = &plan.nodes[input];
                    let pass_through = l.mode == StudioBlendMode::PassThrough;
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
                    group_bounds = union(group_bounds, bounds[input])?;
                    group_need = group_need.max(match child_isolated {
                        Some(true) => 1 + need[input],
                        _ => need[input],
                    });
                    layers.push(PLayer {
                        input,
                        mode: l.mode,
                        opacity: l.opacity * l.fill_opacity,
                        mask,
                    });
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
        for layer in layers {
            self.layer(layer, chunk, dst, spare)?;
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

impl Renderer {
    pub fn new(chunk: u32) -> Result<Self, RenderError> {
        if chunk == 0 || chunk > MAX_CHUNK_SIDE {
            return Err(RenderError::InvalidConfig("chunk"));
        }
        Ok(Self::with_chunk(chunk))
    }
    fn with_chunk(chunk: u32) -> Self {
        Self {
            chunk,
            live: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        }
    }
    pub fn chunk(&self) -> u32 {
        self.chunk
    }
    /// Scratch bytes currently charged; 0 when no render is running.
    pub fn live_scratch_bytes(&self) -> u64 {
        self.live.load(Ordering::Acquire)
    }
    /// Highest simultaneous charge over this renderer's lifetime.
    pub fn peak_scratch_bytes(&self) -> u64 {
        self.peak.load(Ordering::Acquire)
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
        let (extent_x1, extent_y1) = end(extent)?;
        let side = i64::from(self.chunk);
        let max_pixels =
            u64::from(self.chunk.min(extent.width)) * u64::from(self.chunk.min(extent.height));
        let depth = prepared.need[prepared.root] as u64;
        let scratch_bytes = max_pixels
            .checked_mul(SCRATCH_BYTES_PER_PIXEL + GROUP_SCRATCH_BYTES_PER_PIXEL * depth)
            .ok_or(BackendError::Overflow)?;
        let _lease = self.reserve(scratch_bytes, request.max_working_bytes)?;
        let pixels = max_pixels as usize;
        let mut work: Vec<[f32; 4]> = vec![[0.0; 4]; pixels];
        let mut spare: Vec<Vec<[f32; 4]>> = (0..depth).map(|_| vec![[0.0; 4]; pixels]).collect();
        let mut colour: Vec<u8> = Vec::with_capacity(pixels * 12);
        let mut alpha: Vec<u8> = Vec::with_capacity(pixels * 4);
        let eval = Eval {
            prepared: &prepared,
        };
        // A leaf root is one Normal, opacity-1 layer over transparent black.
        let root_layer = PLayer {
            input: prepared.root,
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            mask: None,
        };

        let mut chunks = 0_u64;
        let mut cy = extent.y;
        while cy < extent_y1 {
            let mut cx = extent.x;
            while cx < extent_x1 {
                cancel.check().map_err(|_| BackendError::Canceled)?;
                if request.deadline.is_some_and(|d| Instant::now() >= d) {
                    return Err(BackendError::DeadlineExceeded.into());
                }
                let rect = Rect {
                    x: cx,
                    y: cy,
                    width: side.min(extent_x1 - cx) as u32,
                    height: side.min(extent_y1 - cy) as u32,
                };
                let count = rect.width as usize * rect.height as usize;
                work[..count].fill([0.0; 4]);
                if matches!(prepared.nodes[prepared.root], PNode::Group { .. }) {
                    // The root backdrop is transparent, so isolated and not are the same here.
                    eval.group(prepared.root, rect, &mut work[..count], &mut spare)?;
                } else {
                    eval.layer(&root_layer, rect, &mut work[..count], &mut spare)?;
                }
                colour.clear();
                alpha.clear();
                for [r, g, b, a] in &work[..count] {
                    colour.extend_from_slice(&r.to_le_bytes());
                    colour.extend_from_slice(&g.to_le_bytes());
                    colour.extend_from_slice(&b.to_le_bytes());
                    alpha.extend_from_slice(&a.to_le_bytes());
                }
                sink.accept(rect, &colour, &alpha)
                    .map_err(BackendError::Sink)?;
                chunks += 1;
                cx += side;
            }
            cy += side;
        }
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
