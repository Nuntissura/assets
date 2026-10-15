//! Declared display transform on the GPU backend (lane SMB-RENDER-SCENE-DISPLAY): the same scene
//! as render-cpu `tests/cpu_scene_display.rs` (a 7x5 tile with colour from -0.5 to 4.0 under an
//! in-range Multiply tile at opacity 0.8, alpha 0.5) against pigment's `composite_over` of the
//! clipped sources. The refusal half runs on the `none` backend (no hardware: source preparation
//! precedes the device check); the execution half follows `tests/gpu-proof.toml` (V4-03): without
//! an adapter it FAILS with `NOT_PROVEN: no adapter` while `require_gpu = true`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer,
    NoVector, OutputTarget, PlanNode, PlanNodeId, PlanSources, RenderPlan, SAMPLE_RANGE_OPERATOR,
    SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_prism::profile_hash;
use hsk_studio_render_gpu::{
    BackendChoice, DISPLAY_CLIP_OPERATOR, DeviceOptions, DisplayTransform, GpuBackend, GpuError,
    Reason,
};

/// CPU/GPU parity bound on straight colour and alpha (binary32 kernel vs pigment's binary32
/// `composite_over`; FMA and division may differ in the last ulps). Measured values are printed.
const TOLERANCE: f32 = 1e-6;
const W: u32 = 7;
const H: u32 = 5;

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
const TOP: [f32; 4] = [0.6, 0.2, 0.9, 0.5];

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
fn plan(base: &Tile, top: &Tile) -> RenderPlan<NoVector> {
    let layer = |input: u32, mode: StudioBlendMode, opacity: f32| Layer {
        input: PlanNodeId(input),
        mode,
        opacity,
        fill_opacity: 1.0,
        mask: None,
    };
    RenderPlan {
        revision: 7,
        target: OutputTarget {
            extent: rect(0, 0, W, H),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: tag(),
        },
        nodes: vec![
            PlanNode::Tile(base.input(1, 0, 0)),
            PlanNode::Tile(top.input(2, 2, 1)),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    layer(0, StudioBlendMode::Normal, 1.0),
                    layer(1, StudioBlendMode::Multiply, 0.8),
                ],
            }),
        ],
        root: PlanNodeId(2),
    }
}
fn oracle(x: i64, y: i64) -> [f32; 4] {
    let site = PixelSite { x, y, seed: 0 };
    let p = scene_pixel(x as u32, y as u32);
    let base = Rgba { rgb: [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0), p[2].clamp(0.0, 1.0)], a: p[3] };
    let mut out = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, base, 1.0, site).unwrap();
    if (2..6).contains(&x) && (1..4).contains(&y) {
        let top = Rgba { rgb: [TOP[0], TOP[1], TOP[2]], a: TOP[3] };
        out = composite_over(StudioBlendMode::Multiply, out, top, 0.8, site).unwrap();
    }
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a]
}
fn request(min: Exactness) -> ExecuteRequest {
    ExecuteRequest { expected_revision: 7, min_exactness: min, deadline: None, max_working_bytes: 1 << 30 }
}
fn strict() -> bool {
    include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}

/// Undeclared: `sample_range_outside_unit` before any device work on every backend; declared Clip:
/// source preparation passes and the none backend reaches its device check (`no_adapter`), a NaN
/// sample stays a source mismatch and a floor above Approximate is refused with the clip key. On a
/// live adapter, Clip renders the scene within TOLERANCE of pigment's composite of the clipped
/// sources (chunk sides 2 and 64), records `display_clip_sources` Approximate, and undeclared
/// stays refused with no sink call.
#[test]
fn gpu_clip_matches_the_clipped_oracle_and_undeclared_stays_refused() {
    let (base, top) = (Tile::new(W, H, scene_pixel), Tile::new(4, 3, |_, _| TOP));
    let p = plan(&base, &top);
    let src = Sources(vec![(1, base.planes()), (2, top.planes())]);
    let cancel = CancellationToken::default();
    let refused = GpuError::Backend(BackendError::Unsupported {
        operator: SAMPLE_RANGE_OPERATOR,
        exactness: Exactness::Unsupported,
    });

    let none = DeviceOptions { backend: BackendChoice::None, ..DeviceOptions::default() };
    let mut grid = Grid::new();
    let plain = GpuBackend::new(none);
    assert_eq!(plain.display_transform(), None);
    assert_eq!(plain.execute_detailed(&p, &request(Exactness::Approximate), &src, &cancel, &mut grid).unwrap_err(), refused);
    let clip = GpuBackend::new(none).with_display_transform(DisplayTransform::Clip);
    assert_eq!(
        clip.execute_detailed(&p, &request(Exactness::Approximate), &src, &cancel, &mut grid).unwrap_err(),
        GpuError::Unavailable(Reason::NoAdapter)
    );
    assert_eq!(
        clip.execute_detailed(&p, &request(Exactness::Fitted), &src, &cancel, &mut grid).unwrap_err(),
        GpuError::Backend(BackendError::Unsupported {
            operator: DISPLAY_CLIP_OPERATOR,
            exactness: Exactness::Approximate,
        })
    );
    let nan = Tile::new(W, H, |x, y| if (x, y) == (3, 2) { [f32::NAN, 0.0, 0.0, 1.0] } else { scene_pixel(x, y) });
    let nan_plan = plan(&nan, &top);
    let nan_src = Sources(vec![(1, nan.planes()), (2, top.planes())]);
    assert_eq!(
        clip.execute_detailed(&nan_plan, &request(Exactness::Approximate), &nan_src, &cancel, &mut grid).unwrap_err(),
        GpuError::Backend(BackendError::SourceMismatch(SourceKey(1)))
    );
    assert_eq!(grid.calls, 0);
    assert_eq!(clip.resident_bytes(), 0);

    let mut live = 0;
    for chunk_side in [2u32, 64] {
        let options = DeviceOptions { chunk_side, ..DeviceOptions::default() };
        let plain = GpuBackend::new(options);
        if plain.device_info().is_none() {
            if let Some(failure) = plain.failure() {
                panic!("adapter present but the backend failed: {failure:?}");
            }
            match plain.unavailable_reason() {
                Some(Reason::NoAdapter) if strict() => {
                    panic!("NOT_PROVEN: no adapter; gpu display clip parity requires a GPU (tests/gpu-proof.toml)")
                }
                Some(Reason::NoAdapter) => {
                    eprintln!("NOT_PROVEN: no adapter; gpu display clip parity not run");
                    return;
                }
                other => panic!("no live device ({other:?})"),
            }
        }
        let mut grid = Grid::new();
        assert_eq!(plain.execute_detailed(&p, &request(Exactness::Approximate), &src, &cancel, &mut grid).unwrap_err(), refused);
        assert_eq!(grid.calls, 0);
        let clip = plain.with_display_transform(DisplayTransform::Clip);
        let (receipt, _) = clip
            .execute_detailed(&p, &request(Exactness::Approximate), &src, &cancel, &mut grid)
            .unwrap();
        assert!(
            receipt
                .exactness
                .operators
                .iter()
                .any(|o| o.key == DISPLAY_CLIP_OPERATOR && o.exactness == Exactness::Approximate),
            "{:?}",
            receipt.exactness
        );
        let mut max = 0f32;
        for y in 0..H as i64 {
            for x in 0..W as i64 {
                let (got, want) = (grid.px[(y * W as i64 + x) as usize], oracle(x, y));
                for c in 0..4 {
                    max = max.max((got[c] - want[c]).abs());
                }
            }
        }
        println!("chunk {chunk_side}: max |delta| vs clipped oracle = {max:e} (tolerance {TOLERANCE:e})");
        assert!(max <= TOLERANCE, "chunk {chunk_side}: deviation {max:e}");
        live += 1;
    }
    assert_eq!(live, 2);
}
