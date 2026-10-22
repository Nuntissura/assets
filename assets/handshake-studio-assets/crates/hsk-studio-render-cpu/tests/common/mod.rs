//! Fixtures shared by the envelope test files (`cpu_extended.rs`, `cpu_effects.rs`): plan builders,
//! owned tile and mask sources, and a sink that assembles the delivered chunks into a pixel grid.
#![allow(dead_code)]
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_prism::profile_hash;
use hsk_studio_render_cpu::Renderer;

pub fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}
pub fn tag() -> ColourTag {
    ColourTag {
        profile_sha256: profile_hash(b"working-profile"),
        blend_space: BlendSpace::LinearLight,
    }
}
pub fn plan(extent: Rect, nodes: Vec<PlanNode>, root: u32) -> RenderPlan {
    RenderPlan {
        revision: 7,
        target: OutputTarget {
            extent,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: tag(),
        },
        nodes,
        root: PlanNodeId(root),
    }
}
pub fn layer(input: u32, mode: StudioBlendMode, opacity: f32) -> Layer {
    Layer {
        input: PlanNodeId(input),
        mode,
        opacity,
        fill_opacity: 1.0,
        mask: None,
    }
}
pub fn group(layers: Vec<Layer>) -> PlanNode {
    PlanNode::Group(Group {
        isolated: true,
        layers,
    })
}
pub fn solid(rect: Rect, rgba: [f32; 4]) -> PlanNode {
    PlanNode::Solid { rect, rgba }
}
/// The canonical adjustment placeholder (`Layer.input` of an adjustment layer).
pub fn placeholder(rect: Rect) -> PlanNode {
    solid(rect, [0.0; 4])
}
pub fn req(min: Exactness, budget: u64) -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 7,
        min_exactness: min,
        deadline: None,
        max_working_bytes: budget,
    }
}
pub fn one_layer(group: u32, layer: u32) -> LayerExtension {
    LayerExtension::new(PlanNodeId(group), layer)
}
pub fn envelope(layers: Vec<LayerExtension>) -> PlanExtensions {
    PlanExtensions::from_layers(layers)
}
pub fn near(got: [f32; 4], want: [f32; 4], tolerance: f32) {
    for i in 0..4 {
        assert!(
            (got[i] - want[i]).abs() <= tolerance,
            "channel {i}: got {got:?} want {want:?}"
        );
    }
}

pub struct TileData {
    pub width: u32,
    pub height: u32,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl TileData {
    pub fn new(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..height {
            for x in 0..width {
                let p = pixel(x, y);
                for v in &p[..3] {
                    colour.extend_from_slice(&v.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self {
            width,
            height,
            colour,
            alpha,
        }
    }
    pub fn planes(&self) -> TilePlanes<'_> {
        TilePlanes {
            width: self.width,
            height: self.height,
            colour: &self.colour,
            colour_stride_bytes: u64::from(self.width).wrapping_mul(12),
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(self.width).wrapping_mul(4),
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        }
    }
    /// The tile input placed at `(x, y)`.
    pub fn input_at(&self, key: u32, x: i64, y: i64) -> TileInput {
        TileInput {
            key: SourceKey(key),
            placement: rect(x, y, self.width, self.height),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
        }
    }
    pub fn input(&self, key: u32) -> TileInput {
        self.input_at(key, 0, 0)
    }
}

/// A coverage plane at the origin.
pub struct MaskData {
    pub bounds: Rect,
    wide: bool,
    bytes: Vec<u8>,
}
impl MaskData {
    pub fn u8(width: u32, height: u32, value: impl Fn(u32, u32) -> u8) -> Self {
        let mut bytes = Vec::new();
        for y in 0..height {
            for x in 0..width {
                bytes.push(value(x, y));
            }
        }
        Self {
            bounds: rect(0, 0, width, height),
            wide: false,
            bytes,
        }
    }
    pub fn u16(width: u32, height: u32, value: u16) -> Self {
        let count = usize::try_from(width)
            .unwrap()
            .wrapping_mul(usize::try_from(height).unwrap());
        Self {
            bounds: rect(0, 0, width, height),
            wide: true,
            bytes: vec![value; count]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect(),
        }
    }
    fn format(&self) -> MaskFormat {
        if self.wide {
            MaskFormat::CoverageU16Le
        } else {
            MaskFormat::CoverageU8
        }
    }
    pub fn input(&self, key: u32, inverted: bool, density: f32) -> MaskInput {
        MaskInput {
            key: SourceKey(key),
            placement: self.bounds,
            format: self.format(),
            sha256: profile_hash(&self.bytes),
            inverted,
            density,
        }
    }
}
#[derive(Default)]
pub struct Sources<'a> {
    pub tiles: Vec<(u32, TilePlanes<'a>)>,
    pub masks: Vec<(u32, &'a MaskData)>,
}
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        self.tiles
            .iter()
            .find(|(k, _)| *k == key.0)
            .map(|(_, t)| *t)
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        let (_, m) = self.masks.iter().find(|(k, _)| *k == key.0)?;
        let bytes_per_pixel: u64 = if m.wide { 2 } else { 1 };
        Some(Mask {
            bounds: m.bounds,
            sample_format: m.format().key(),
            bytes: &m.bytes,
            stride_bytes: u64::from(m.bounds.width).wrapping_mul(bytes_per_pixel),
            sha256: profile_hash(&m.bytes),
        })
    }
}

/// Assembles the delivered chunks into a full-extent pixel grid.
pub struct Grid {
    extent: Rect,
    pub px: Vec<[f32; 4]>,
    pub calls: usize,
}
impl Grid {
    pub fn new(extent: Rect) -> Self {
        let count = usize::try_from(extent.width)
            .unwrap()
            .wrapping_mul(usize::try_from(extent.height).unwrap());
        Self {
            extent,
            px: vec![[f32::NAN; 4]; count],
            calls: 0,
        }
    }
    fn index(&self, x: i64, y: i64) -> usize {
        usize::try_from(y)
            .unwrap()
            .wrapping_mul(usize::try_from(self.extent.width).unwrap())
            .wrapping_add(usize::try_from(x).unwrap())
    }
    pub fn at(&self, x: i64, y: i64) -> [f32; 4] {
        self.px[self.index(x, y)]
    }
}
fn f32_at(bytes: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([
        bytes[at],
        bytes[at.wrapping_add(1)],
        bytes[at.wrapping_add(2)],
        bytes[at.wrapping_add(3)],
    ])
}
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        self.calls = self.calls.saturating_add(1);
        let width = usize::try_from(r.width).unwrap();
        for row in 0..r.height {
            for col in 0..r.width {
                let source = usize::try_from(row)
                    .unwrap()
                    .wrapping_mul(width)
                    .wrapping_add(usize::try_from(col).unwrap());
                let i = self.index(
                    r.x.wrapping_add(i64::from(col)),
                    r.y.wrapping_add(i64::from(row)),
                );
                self.px[i] = [
                    f32_at(colour, source.wrapping_mul(12)),
                    f32_at(colour, source.wrapping_mul(12).wrapping_add(4)),
                    f32_at(colour, source.wrapping_mul(12).wrapping_add(8)),
                    f32_at(alpha, source.wrapping_mul(4)),
                ];
            }
        }
        Ok(())
    }
}

pub fn run_ext(
    renderer: &Renderer,
    plan: &RenderPlan,
    ext: &PlanExtensions,
    min: Exactness,
    budget: u64,
    sources: &dyn PlanSources,
) -> (Result<RenderReceipt, BackendError>, Grid) {
    let mut grid = Grid::new(plan.target.extent);
    let result = renderer.execute_extended(
        plan,
        ext,
        &req(min, budget),
        sources,
        &CancellationToken::default(),
        &mut grid,
    );
    (result, grid)
}
pub fn no_sources() -> Sources<'static> {
    Sources::default()
}
/// One pixel of a 1x1 plan through the envelope path.
pub fn pixel(plan: &RenderPlan, ext: &PlanExtensions, sources: &dyn PlanSources) -> [f32; 4] {
    let (result, grid) = run_ext(
        &Renderer::new(1).unwrap(),
        plan,
        ext,
        Exactness::Approximate,
        1 << 20,
        sources,
    );
    result.unwrap();
    grid.at(0, 0)
}
