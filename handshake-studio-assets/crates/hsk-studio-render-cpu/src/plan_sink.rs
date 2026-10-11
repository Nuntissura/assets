//! Plan, tile sources, bounded chunk executor and pixel sink.
//!
//! The executor receives buffers already in the plan's blending space and materialised data only
//! (STU-COL-163): it never resolves a profile, converts a space or calls a colour engine. Output is
//! the canonical pigment pixel contract: an `rgb_f32le` colour plane (12 bytes per pixel) plus a
//! SEPARATE straight `f32le` alpha plane (4 bytes per pixel), each chunk tightly packed.
use crate::coverage::PreparedPath;
use crate::{BlendMode, RenderError, composite_straight};
use hsk_studio_accord::CancellationToken;
use hsk_studio_nib::{Anchor, Winding};
use hsk_studio_pigment::Rect;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub const RENDERER: &str = "hsk-studio-render-cpu/0.1";
pub const SAMPLE_FORMAT: &str = "rgb_f32le";
pub const ALPHA_ASSOCIATION: &str = "straight";
pub const MAX_OPS: usize = 65_536;
pub const MAX_ANCHORS: usize = 65_536;
pub const MAX_CHUNK_SIDE: u32 = 4096;
pub const DEFAULT_CHUNK_SIDE: u32 = 64;
pub const DEFAULT_MAX_SCRATCH_BYTES: u64 = 16 * 1024 * 1024;
/// Scratch charged per chunk pixel: 16 working (RGBA f32) + 12 colour out + 4 alpha out.
pub const SCRATCH_BYTES_PER_PIXEL: u64 = 32;

/// Label of the space the plan's numbers live in. The renderer only checks that inputs carry the
/// same label; it never converts (STU-COL-162/163).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendSpace {
    LinearLight,
    Encoded,
}
impl BlendSpace {
    pub const fn label(self) -> &'static str {
        match self {
            Self::LinearLight => "linear_light",
            Self::Encoded => "encoded",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkError {
    Rejected,
    Unavailable,
}

/// Caller-owned receiver of rendered chunks. Chunks are provisional until `render` returns
/// `Ok(receipt)`; after any `Err` the caller must treat everything delivered as unpublished.
pub trait PixelSink {
    /// `colour` is `rect.width * rect.height * 12` bytes (`rgb_f32le`), `alpha` is
    /// `rect.width * rect.height * 4` bytes (`f32le`, straight), both row-major without padding.
    fn accept(&mut self, rect: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(pub u32);

/// A resolved source tile as the renderer consumes it: own placement-free size, two planes, hashes
/// and the profile/space it was produced in. Build from pigment with [`SourceTile::from_resolved`].
#[derive(Clone, Copy, Debug)]
pub struct SourceTile<'a> {
    pub width: u32,
    pub height: u32,
    pub colour: &'a [u8],
    pub colour_stride_bytes: u64,
    pub alpha: &'a [u8],
    pub alpha_stride_bytes: u64,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
    pub profile_sha256: [u8; 32],
    pub space: BlendSpace,
}

fn plane_ok(
    bytes: &[u8],
    stride: u64,
    width: u32,
    height: u32,
    bpp: u64,
) -> Result<(), RenderError> {
    let row = u64::from(width)
        .checked_mul(bpp)
        .ok_or(RenderError::Overflow)?;
    let size = stride
        .checked_mul(u64::from(height))
        .ok_or(RenderError::Overflow)?;
    if stride < row || !stride.is_multiple_of(4) || u64::try_from(bytes.len()).ok() != Some(size) {
        return Err(RenderError::MalformedStride);
    }
    Ok(())
}

fn f32_at(bytes: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

impl<'a> SourceTile<'a> {
    /// Adapter from the pigment canonical tile. Runs pigment's shape contract (`rgb_f32le`,
    /// `linear_light`, straight alpha, grid placement, stride alignment); hash and value checks run
    /// in [`SourceTile::validate`] at render time.
    pub fn from_resolved(
        tile: &hsk_studio_pigment::ResolvedTile<'a>,
        grid: hsk_studio_pigment::Grid,
    ) -> Result<Self, RenderError> {
        tile.validate_shape(grid).map_err(pigment_error)?;
        Ok(Self {
            width: tile.bounds.width,
            height: tile.bounds.height,
            colour: tile.colour_bytes,
            colour_stride_bytes: tile.colour_stride_bytes,
            alpha: tile.alpha_bytes,
            alpha_stride_bytes: tile.alpha_stride_bytes,
            colour_sha256: tile.colour_sha256,
            alpha_sha256: tile.alpha_sha256,
            profile_sha256: tile.profile.expected_sha256,
            space: BlendSpace::LinearLight,
        })
    }

    /// Shape, plane hashes, finite colour and `0..=1` alpha. Refuses before any pixel is used.
    pub fn validate(&self) -> Result<(), RenderError> {
        if self.width == 0 || self.height == 0 {
            return Err(RenderError::InvalidInput("tile_size"));
        }
        plane_ok(self.colour, self.colour_stride_bytes, self.width, self.height, 12)?;
        plane_ok(self.alpha, self.alpha_stride_bytes, self.width, self.height, 4)?;
        if hsk_studio_prism::profile_hash(self.colour) != self.colour_sha256
            || hsk_studio_prism::profile_hash(self.alpha) != self.alpha_sha256
        {
            return Err(RenderError::HashMismatch);
        }
        for y in 0..self.height {
            for x in 0..self.width {
                let [r, g, b, a] = self.pixel(x, y);
                if !(r.is_finite() && g.is_finite() && b.is_finite() && a.is_finite()) {
                    return Err(RenderError::Nonfinite);
                }
                if !(0.0..=1.0).contains(&a) {
                    return Err(RenderError::AlphaRange);
                }
            }
        }
        Ok(())
    }

    /// Straight RGBA at tile-local `(x, y)`; the tile must have passed [`SourceTile::validate`].
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        let c = y as usize * self.colour_stride_bytes as usize + x as usize * 12;
        let a = y as usize * self.alpha_stride_bytes as usize + x as usize * 4;
        [
            f32_at(self.colour, c),
            f32_at(self.colour, c + 4),
            f32_at(self.colour, c + 8),
            f32_at(self.alpha, a),
        ]
    }
}

fn pigment_error(error: hsk_studio_pigment::Error) -> RenderError {
    use hsk_studio_pigment::Error as E;
    match error {
        E::MalformedStride => RenderError::MalformedStride,
        E::HashMismatch => RenderError::HashMismatch,
        E::Nonfinite => RenderError::Nonfinite,
        E::AlphaRange => RenderError::AlphaRange,
        E::Overflow => RenderError::Overflow,
        _ => RenderError::InvalidInput("pigment_tile"),
    }
}

/// Resolves plan source ids to immutable tiles. `None` means unresolved and fails the render.
pub trait TileSource: Send + Sync {
    fn resolve(&self, id: SourceId) -> Option<SourceTile<'_>>;
}
/// A plan with no tile sources.
pub struct NoSources;
impl TileSource for NoSources {
    fn resolve(&self, _id: SourceId) -> Option<SourceTile<'_>> {
        None
    }
}
impl TileSource for Vec<(SourceId, SourceTile<'_>)> {
    fn resolve(&self, id: SourceId) -> Option<SourceTile<'_>> {
        self.iter().find(|(i, _)| *i == id).map(|(_, t)| *t)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Solid straight-RGBA rectangle in plan pixel space.
    Fill {
        rect: Rect,
        rgba: [f32; 4],
        blend: BlendMode,
        opacity: f32,
    },
    /// Resolved tile whose top-left pixel lands on `(dst_x, dst_y)`.
    Tile {
        source: SourceId,
        dst_x: i64,
        dst_y: i64,
        blend: BlendMode,
        opacity: f32,
    },
    /// Anti-aliased path fill (nib anchors; coordinates in pixels, or points/mm/in with
    /// `RenderPlan::pixels_per_inch`; handles are anchor-relative vectors).
    PathFill {
        anchors: Vec<Anchor>,
        closed: bool,
        winding: Winding,
        rgba: [f32; 4],
        blend: BlendMode,
        opacity: f32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderPlan {
    pub revision: u64,
    pub extent: Rect,
    pub profile_sha256: [u8; 32],
    pub blend_space: BlendSpace,
    /// Samples per axis for `PathFill` coverage, `1..=16` (coverage resolution is `1/n^2`).
    pub aa_samples: u8,
    pub pixels_per_inch: Option<f64>,
    pub ops: Vec<Op>,
}
impl RenderPlan {
    pub fn new(
        revision: u64,
        extent: Rect,
        profile_sha256: [u8; 32],
        blend_space: BlendSpace,
        ops: Vec<Op>,
    ) -> Self {
        Self {
            revision,
            extent,
            profile_sha256,
            blend_space,
            aa_samples: 4,
            pixels_per_inch: None,
            ops,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderReceipt {
    pub revision: u64,
    pub profile_sha256: [u8; 32],
    pub blend_space: BlendSpace,
    pub extent: Rect,
    pub sample_format: &'static str,
    pub alpha: &'static str,
    pub chunks: u64,
    pub ops_executed: u64,
    pub peak_scratch_bytes: u64,
    pub renderer: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Chunk side in pixels, `1..=MAX_CHUNK_SIDE`.
    pub chunk: u32,
    pub max_scratch_bytes: u64,
    /// Observed between chunks only.
    pub deadline: Option<Instant>,
}
impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            chunk: DEFAULT_CHUNK_SIDE,
            max_scratch_bytes: DEFAULT_MAX_SCRATCH_BYTES,
            deadline: None,
        }
    }
}

/// Bounded executor. Scratch is charged against `max_scratch_bytes` before it is allocated and is
/// released on every exit path by an RAII lease; `live_scratch_bytes` is 0 whenever no render runs.
pub struct Renderer {
    options: RenderOptions,
    live: AtomicU64,
    peak: AtomicU64,
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

enum Prepared<'a> {
    Fill {
        rect: Rect,
        rgba: [f32; 4],
        blend: BlendMode,
        opacity: f32,
    },
    Tile {
        tile: SourceTile<'a>,
        rect: Rect,
        blend: BlendMode,
        opacity: f32,
    },
    Path {
        path: PreparedPath,
        rgba: [f32; 4],
        blend: BlendMode,
        opacity: f32,
    },
}
impl Prepared<'_> {
    fn bounds(&self) -> Rect {
        match self {
            Self::Fill { rect, .. } | Self::Tile { rect, .. } => *rect,
            Self::Path { path, .. } => path.bounds,
        }
    }
}

fn end(rect: Rect) -> Result<(i64, i64), RenderError> {
    rect.end().map_err(|_| RenderError::Overflow)
}
fn intersect(a: Rect, b: Rect) -> Result<Option<Rect>, RenderError> {
    a.intersect(b).map_err(|_| RenderError::Overflow)
}
fn unit_range(value: f32, what: &'static str) -> Result<(), RenderError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(RenderError::InvalidInput(what))
    }
}
fn check_rgba(rgba: [f32; 4]) -> Result<(), RenderError> {
    if !rgba.iter().all(|v| v.is_finite()) {
        return Err(RenderError::Nonfinite);
    }
    if !(0.0..=1.0).contains(&rgba[3]) {
        return Err(RenderError::AlphaRange);
    }
    Ok(())
}

fn prepare<'a>(
    plan: &RenderPlan,
    tiles: &'a dyn TileSource,
) -> Result<Vec<Prepared<'a>>, RenderError> {
    if plan.ops.len() > MAX_OPS {
        return Err(RenderError::BudgetExceeded);
    }
    let mut validated: Vec<SourceId> = Vec::new();
    let mut out = Vec::with_capacity(plan.ops.len());
    for op in &plan.ops {
        out.push(match op {
            Op::Fill {
                rect,
                rgba,
                blend,
                opacity,
            } => {
                unit_range(*opacity, "opacity")?;
                check_rgba(*rgba)?;
                end(*rect)?;
                Prepared::Fill {
                    rect: *rect,
                    rgba: *rgba,
                    blend: *blend,
                    opacity: *opacity,
                }
            }
            Op::Tile {
                source,
                dst_x,
                dst_y,
                blend,
                opacity,
            } => {
                unit_range(*opacity, "opacity")?;
                let tile = tiles
                    .resolve(*source)
                    .ok_or(RenderError::UnresolvedSource(source.0))?;
                if !validated.contains(source) {
                    tile.validate()?;
                    validated.push(*source);
                }
                if tile.profile_sha256 != plan.profile_sha256 {
                    return Err(RenderError::ProfileMismatch);
                }
                if tile.space != plan.blend_space {
                    return Err(RenderError::SpaceMismatch);
                }
                let rect = Rect {
                    x: *dst_x,
                    y: *dst_y,
                    width: tile.width,
                    height: tile.height,
                };
                end(rect)?;
                Prepared::Tile {
                    tile,
                    rect,
                    blend: *blend,
                    opacity: *opacity,
                }
            }
            Op::PathFill {
                anchors,
                closed,
                winding,
                rgba,
                blend,
                opacity,
            } => {
                unit_range(*opacity, "opacity")?;
                check_rgba(*rgba)?;
                if anchors.len() > MAX_ANCHORS {
                    return Err(RenderError::BudgetExceeded);
                }
                let path = PreparedPath::prepare(
                    anchors,
                    *closed,
                    *winding,
                    plan.aa_samples,
                    plan.pixels_per_inch,
                )?;
                end(path.bounds)?;
                Prepared::Path {
                    path,
                    rgba: *rgba,
                    blend: *blend,
                    opacity: *opacity,
                }
            }
        });
    }
    Ok(out)
}

fn apply(op: &Prepared<'_>, chunk: Rect, work: &mut [[f32; 4]]) -> Result<(), RenderError> {
    let Some(region) = intersect(op.bounds(), chunk)? else {
        return Ok(());
    };
    let chunk_width = chunk.width as usize;
    for y in region.y..region.y + i64::from(region.height) {
        for x in region.x..region.x + i64::from(region.width) {
            let at = (y - chunk.y) as usize * chunk_width + (x - chunk.x) as usize;
            let (source, coverage, blend, opacity) = match op {
                Prepared::Fill {
                    rgba,
                    blend,
                    opacity,
                    ..
                } => (*rgba, 1.0, *blend, *opacity),
                Prepared::Tile {
                    tile,
                    rect,
                    blend,
                    opacity,
                } => (
                    tile.pixel((x - rect.x) as u32, (y - rect.y) as u32),
                    1.0,
                    *blend,
                    *opacity,
                ),
                Prepared::Path {
                    path,
                    rgba,
                    blend,
                    opacity,
                } => (*rgba, path.coverage(x, y), *blend, *opacity),
            };
            if coverage > 0.0 {
                work[at] = composite_straight(blend, work[at], source, opacity, coverage);
            }
        }
    }
    Ok(())
}

impl Renderer {
    pub fn new(options: RenderOptions) -> Result<Self, RenderError> {
        if options.chunk == 0 || options.chunk > MAX_CHUNK_SIDE {
            return Err(RenderError::InvalidInput("chunk"));
        }
        Ok(Self {
            options,
            live: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        })
    }
    pub fn options(&self) -> RenderOptions {
        self.options
    }
    /// Scratch bytes currently charged; 0 when no render is running.
    pub fn live_scratch_bytes(&self) -> u64 {
        self.live.load(Ordering::Acquire)
    }
    /// Highest simultaneous charge over this renderer's lifetime.
    pub fn peak_scratch_bytes(&self) -> u64 {
        self.peak.load(Ordering::Acquire)
    }

    fn reserve(&self, bytes: u64) -> Result<ScratchLease<'_>, RenderError> {
        if bytes > self.options.max_scratch_bytes {
            return Err(RenderError::BudgetExceeded);
        }
        let live = self.live.fetch_add(bytes, Ordering::AcqRel) + bytes;
        self.peak.fetch_max(live, Ordering::AcqRel);
        Ok(ScratchLease {
            owner: self,
            bytes,
        })
    }

    /// Execute `plan` into `sink`, row-major over `chunk x chunk` pixel chunks.
    ///
    /// Order: cancel, stale revision, then full validation of every op and source (so a bad plan
    /// never reaches the sink), then chunks. Cancel and deadline are observed before every chunk.
    /// A `RenderReceipt` exists only after the last chunk was accepted.
    pub fn render(
        &self,
        plan: &RenderPlan,
        expected_revision: u64,
        tiles: &dyn TileSource,
        cancel: &CancellationToken,
        sink: &mut dyn PixelSink,
    ) -> Result<RenderReceipt, RenderError> {
        cancel.check().map_err(|_| RenderError::Canceled)?;
        if plan.revision != expected_revision {
            return Err(RenderError::StaleRevision {
                expected: expected_revision,
                found: plan.revision,
            });
        }
        let extent = plan.extent;
        if extent.width == 0 || extent.height == 0 {
            return Err(RenderError::InvalidInput("extent"));
        }
        let (extent_x1, extent_y1) = end(extent)?;
        let ops = prepare(plan, tiles)?;

        let side = i64::from(self.options.chunk);
        let max_w = u64::from(self.options.chunk.min(extent.width));
        let max_h = u64::from(self.options.chunk.min(extent.height));
        let max_pixels = max_w * max_h;
        let scratch_bytes = max_pixels
            .checked_mul(SCRATCH_BYTES_PER_PIXEL)
            .ok_or(RenderError::Overflow)?;
        let _lease = self.reserve(scratch_bytes)?;
        let pixels = max_pixels as usize;
        let mut work: Vec<[f32; 4]> = vec![[0.0; 4]; pixels];
        let mut colour: Vec<u8> = Vec::with_capacity(pixels * 12);
        let mut alpha: Vec<u8> = Vec::with_capacity(pixels * 4);

        let mut chunks = 0_u64;
        let mut cy = extent.y;
        while cy < extent_y1 {
            let mut cx = extent.x;
            while cx < extent_x1 {
                cancel.check().map_err(|_| RenderError::Canceled)?;
                if self.options.deadline.is_some_and(|d| Instant::now() >= d) {
                    return Err(RenderError::DeadlineExceeded);
                }
                let rect = Rect {
                    x: cx,
                    y: cy,
                    width: (side.min(extent_x1 - cx)) as u32,
                    height: (side.min(extent_y1 - cy)) as u32,
                };
                let count = rect.width as usize * rect.height as usize;
                work[..count].fill([0.0; 4]);
                for op in &ops {
                    apply(op, rect, &mut work[..count])?;
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
                    .map_err(RenderError::Sink)?;
                chunks += 1;
                cx += side;
            }
            cy += side;
        }
        Ok(RenderReceipt {
            revision: plan.revision,
            profile_sha256: plan.profile_sha256,
            blend_space: plan.blend_space,
            extent,
            sample_format: SAMPLE_FORMAT,
            alpha: ALPHA_ASSOCIATION,
            chunks,
            ops_executed: ops.len() as u64,
            peak_scratch_bytes: scratch_bytes,
            renderer: RENDERER,
        })
    }
}
