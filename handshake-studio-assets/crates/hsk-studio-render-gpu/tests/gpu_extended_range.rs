//! Plan output stage on render-gpu (lane SMB-PIGMENT-EXTENDED-RANGE): CPU/GPU parity of the
//! extended-range composite and the stage-6a clip. The scene is render-cpu
//! `tests/cpu_extended_range.rs`'s (a 7x5 tile with colour -0.5..4.0 under linear dodge, difference
//! and lighten layers with colours up to 6.0); the reference is pigment `composite_over_extended`,
//! which render-cpu reproduces bit for bit (that test). The refusal half runs on the `none` backend;
//! the execution half follows `tests/gpu-proof.toml` (V4-03).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over_extended};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, DISPLAY_CLIP_OPERATOR, DisplayTransform,
    EXTENDED_RANGE_MODE_OPERATOR, EXTENDED_RANGE_OPERATOR, Exactness, ExecuteRequest, Group, Layer,
    NoVector, OutputStage, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, PlanSources,
    RenderPlan, SampleFormat, SampleRange, SinkError, SourceKey, StudioBlendMode, TileInput,
    TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_prism::profile_hash;
use hsk_studio_render_gpu::{BackendChoice, DeviceOptions, GpuBackend, GpuError, Reason};

/// Bound on |GPU - pigment| relative to max(1, |reference|): binary32 kernel arithmetic (FMA,
/// division) against pigment's binary32 sequence. Measured values are printed.
const TOLERANCE: f32 = 1e-6;
const W: u32 = 7;
const H: u32 = 5;
type Top = (StudioBlendMode, f32, (i64, i64), [f32; 4]);
const TOPS: [Top; 3] = [
    (StudioBlendMode::LinearDodgeAdd, 0.7, (1, 0), [0.5, 1.25, 0.2, 0.8]),
    (StudioBlendMode::Difference, 1.0, (3, 2), [2.0, 0.25, 0.75, 0.6]),
    (StudioBlendMode::Lighten, 0.5, (0, 3), [3.5, 0.1, 6.0, 1.0]),
];

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}
fn tag() -> ColourTag {
    ColourTag { profile_sha256: profile_hash(b"working-profile"), blend_space: BlendSpace::LinearLight }
}
fn scene_pixel(x: u32, y: u32) -> [f32; 4] {
    let (x, y) = (x as f32, y as f32);
    [x * 0.5 - 0.5, 1.0 + y * 0.75, 0.3, 1.0]
}
struct Tile {
    width: u32,
    height: u32,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Tile {
    fn new(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> Self {
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
        Self { width, height, colour, alpha }
    }
    fn planes(&self) -> TilePlanes<'_> {
        TilePlanes {
            width: self.width,
            height: self.height,
            colour: &self.colour,
            colour_stride_bytes: u64::from(self.width) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(self.width) * 4,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        }
    }
    fn input(&self, key: u32, x: i64, y: i64) -> TileInput {
        TileInput {
            key: SourceKey(key),
            placement: rect(x, y, self.width, self.height),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
        }
    }
}
struct Sources<'a>(Vec<(u32, TilePlanes<'a>)>);
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        self.0.iter().find(|(k, _)| *k == key.0).map(|(_, t)| *t)
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}
struct Grid {
    px: Vec<[f32; 4]>,
    calls: usize,
}
impl Grid {
    fn new() -> Self {
        Self { px: vec![[f32::NAN; 4]; (W * H) as usize], calls: 0 }
    }
}
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        self.calls += 1;
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for row in 0..r.height as usize {
            for col in 0..r.width as usize {
                let s = row * r.width as usize + col;
                let i = (r.y as usize + row) * W as usize + r.x as usize + col;
                self.px[i] = [f(colour, s * 12), f(colour, s * 12 + 4), f(colour, s * 12 + 8), f(alpha, s * 4)];
            }
        }
        Ok(())
    }
}
fn plan(base: &Tile, tops: &[Tile], top_mode: Option<StudioBlendMode>) -> RenderPlan<NoVector> {
    let layer = |input: u32, mode: StudioBlendMode, opacity: f32| Layer {
        input: PlanNodeId(input),
        mode,
        opacity,
        fill_opacity: 1.0,
        mask: None,
    };
    let mut nodes = vec![PlanNode::Tile(base.input(1, 0, 0))];
    let mut layers = vec![layer(0, StudioBlendMode::Normal, 1.0)];
    for (i, ((mode, opacity, (x, y), _), tile)) in TOPS.iter().zip(tops).enumerate() {
        let index = i as u32 + 1;
        nodes.push(PlanNode::Tile(tile.input(index + 1, *x, *y)));
        layers.push(layer(index, top_mode.unwrap_or(*mode), *opacity));
    }
    nodes.push(PlanNode::Group(Group { isolated: true, layers }));
    RenderPlan {
        revision: 7,
        target: OutputTarget {
            extent: rect(0, 0, W, H),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: tag(),
        },
        nodes,
        root: PlanNodeId(4),
    }
}
fn oracle(x: i64, y: i64) -> [f32; 4] {
    let site = PixelSite { x, y, seed: 0 };
    let p = scene_pixel(x as u32, y as u32);
    let mut out =
        composite_over_extended(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([p[0], p[1], p[2]], p[3]), 1.0, site)
            .unwrap();
    for (mode, opacity, (ox, oy), c) in TOPS {
        if (ox..ox + 4).contains(&x) && (oy..oy + 2).contains(&y) {
            out = composite_over_extended(mode, out, Rgba::new([c[0], c[1], c[2]], c[3]), opacity, site).unwrap();
        }
    }
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a]
}
fn request(min: Exactness) -> ExecuteRequest {
    ExecuteRequest { expected_revision: 7, min_exactness: min, deadline: None, max_working_bytes: 1 << 30 }
}
fn stage(display: Option<DisplayTransform>) -> PlanExtensions {
    PlanExtensions::default().with_output(OutputStage::new(SampleRange::Extended, display))
}
fn strict() -> bool {
    include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}

/// none backend: the extended plan passes admission and preparation (reaches `no_adapter`), a
/// screen layer is refused `sample_range_extended_mode` and the unit default keeps
/// `sample_range_outside_unit`, with no sink call. Live adapter, chunk sides 2 and 64: the
/// extended composite (values above 1 kept) and the clipped one are within TOLERANCE of pigment,
/// the ledger names `sample_range_extended` and `display_clip`, and an in-range plan renders the
/// same bytes through the v1 call and through an envelope with the default output stage.
#[test]
fn gpu_extended_range_matches_pigment_and_the_unit_default_is_unchanged() {
    let base = Tile::new(W, H, scene_pixel);
    let tops: Vec<Tile> = TOPS.iter().map(|t| Tile::new(4, 2, |_, _| t.3)).collect();
    let p = plan(&base, &tops, None);
    let mut entries = vec![(1, base.planes())];
    entries.extend(tops.iter().enumerate().map(|(i, t)| (i as u32 + 2, t.planes())));
    let src = Sources(entries);
    let cancel = CancellationToken::default();

    let none = GpuBackend::new(DeviceOptions { backend: BackendChoice::None, ..DeviceOptions::default() });
    let mut grid = Grid::new();
    let floor = request(Exactness::Approximate);
    assert_eq!(
        none.execute_extended_detailed(&p, &stage(None), &floor, &src, &cancel, &mut grid).unwrap_err(),
        GpuError::Unavailable(Reason::NoAdapter)
    );
    let screen = plan(&base, &tops, Some(StudioBlendMode::Screen));
    assert_eq!(
        none.execute_extended_detailed(&screen, &stage(None), &floor, &src, &cancel, &mut grid).unwrap_err(),
        GpuError::Backend(BackendError::Unsupported { operator: EXTENDED_RANGE_MODE_OPERATOR, exactness: Exactness::Unsupported })
    );
    assert!(matches!(
        none.execute_detailed(&p, &floor, &src, &cancel, &mut grid).unwrap_err(),
        GpuError::Backend(BackendError::Unsupported { .. })
    ));
    assert_eq!(grid.calls, 0);

    let mut live = 0;
    for chunk_side in [2u32, 64] {
        let gpu = GpuBackend::new(DeviceOptions { chunk_side, ..DeviceOptions::default() });
        if gpu.device_info().is_none() {
            if let Some(failure) = gpu.failure() {
                panic!("adapter present but the backend failed: {failure:?}");
            }
            match gpu.unavailable_reason() {
                Some(Reason::NoAdapter) if strict() => {
                    panic!("NOT_PROVEN: no adapter; gpu extended range parity requires a GPU (tests/gpu-proof.toml)")
                }
                Some(Reason::NoAdapter) => {
                    eprintln!("NOT_PROVEN: no adapter; gpu extended range parity not run");
                    return;
                }
                other => panic!("no live device ({other:?})"),
            }
        }
        let mut above_one = 0;
        for display in [None, Some(DisplayTransform::Clip)] {
            let mut grid = Grid::new();
            let (receipt, _) = gpu
                .execute_extended_detailed(&p, &stage(display), &floor, &src, &cancel, &mut grid)
                .unwrap();
            let has = |key: &str| receipt.exactness.operators.iter().any(|o| o.key == key);
            assert!(has(EXTENDED_RANGE_OPERATOR));
            assert_eq!(has(DISPLAY_CLIP_OPERATOR), display.is_some());
            let mut max = 0f32;
            for y in 0..H as i64 {
                for x in 0..W as i64 {
                    let mut want = oracle(x, y);
                    if display.is_some() {
                        for v in &mut want[..3] {
                            *v = v.clamp(0.0, 1.0);
                        }
                    } else {
                        above_one += usize::from(want[..3].iter().any(|v| *v > 1.0));
                    }
                    let got = grid.px[(y * W as i64 + x) as usize];
                    for c in 0..4 {
                        max = max.max((got[c] - want[c]).abs() / want[c].abs().max(1.0));
                    }
                }
            }
            println!("chunk {chunk_side} display {display:?}: max relative |delta| vs pigment = {max:e} (tolerance {TOLERANCE:e})");
            assert!(max <= TOLERANCE, "chunk {chunk_side} {display:?}: deviation {max:e}");
        }
        assert!(above_one > 0);

        // Unit default: the v1 call and the default output stage give the same bytes.
        let unit_base = Tile::new(W, H, |x, y| scene_pixel(x, y).map(|v| v.clamp(0.0, 1.0)));
        let unit_tops: Vec<Tile> = TOPS.iter().map(|t| Tile::new(4, 2, |_, _| t.3.map(|v| v.clamp(0.0, 1.0)))).collect();
        let unit = plan(&unit_base, &unit_tops, None);
        let mut unit_entries = vec![(1, unit_base.planes())];
        unit_entries.extend(unit_tops.iter().enumerate().map(|(i, t)| (i as u32 + 2, t.planes())));
        let unit_src = Sources(unit_entries);
        let (mut a, mut b) = (Grid::new(), Grid::new());
        let ra = gpu.execute_detailed(&unit, &floor, &unit_src, &cancel, &mut a).unwrap().0;
        let default_stage = PlanExtensions::default().with_output(OutputStage::default());
        let rb = gpu.execute_extended_detailed(&unit, &default_stage, &floor, &unit_src, &cancel, &mut b).unwrap().0;
        assert_eq!(ra.plan_digest, rb.plan_digest);
        assert_eq!(ra.exactness, rb.exactness);
        let bits = |g: &Grid| g.px.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>();
        assert_eq!(bits(&a), bits(&b));
        live += 1;
    }
    assert_eq!(live, 2);
}
