//! Layer transforms on the GPU backend (lane SMB-PIGMENT-TRANSFORM slice 3): the resample pass
//! (`resample.wgsl`, binary32) against pigment's binary64 reference sampler
//! (`transform_math::PreparedTransform`, the bytes render-cpu produces). GPU-execution proofs follow
//! the checked-in `tests/gpu-proof.toml` (V4-03): without an adapter they FAIL with a message
//! starting `NOT_PROVEN: no adapter` while `require_gpu = true`, and print that line otherwise; they
//! are never a silent green. The admission test is hardware-independent.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::effect_math::{ColourOverlayOp, EffectOp};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer,
    LayerExtension, MaskFormat, MaskInput, NoVector, OutputTarget, PlanError, PlanExtensions,
    PlanNode, PlanNodeId, PlanSources, RenderBackend, RenderPlan, RenderReceipt, SampleFormat,
    SinkError, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::transform_math::{
    Affine2, EdgeMode, LayerTransform, PreparedTransform, ResampleFilter, TransformError,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{BackendChoice, DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

/// Established render-gpu tolerance against pigment's reference (START_LOG 2026-10-11T05:31:12Z:
/// 2e-6 for the blend kernels). Applied here to alpha and to premultiplied colour, the quantities
/// the resample filter accumulates; straight colour is `P / A` and its error grows as `A` shrinks
/// (reported, and bounded where `A >= 1/4`).
const TOLERANCE: f32 = 2e-6;
const FILTERS: [ResampleFilter; 3] = [
    ResampleFilter::NearestNeighbour,
    ResampleFilter::Bilinear,
    ResampleFilter::Bicubic,
];
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const BACKDROP: [f32; 4] = [0.9, 0.7, 0.5, 1.0];

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}
fn tag() -> ColourTag {
    ColourTag {
        profile_sha256: [7; 32],
        blend_space: BlendSpace::LinearLight,
    }
}

/// Colour ramp with a non-binary alpha and transparent holes.
fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let hole = (x * 3 + y * 5).is_multiple_of(7);
    [
        (x as f32 / 9.0).min(1.0),
        (y as f32 / 6.0).min(1.0),
        ((x + y) % 4) as f32 / 4.0,
        if hole { 0.0 } else { ((x + y) % 5 + 1) as f32 / 5.0 },
    ]
}

struct Tile {
    width: u32,
    height: u32,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Tile {
    fn new(width: u32, height: u32) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..height {
            for x in 0..width {
                let p = tile_px(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
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
    fn input(&self, x: i64, y: i64) -> TileInput {
        TileInput {
            key: SourceKey(1),
            placement: rect(x, y, self.width, self.height),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: hsk_studio_prism::profile_hash(&self.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.alpha),
        }
    }
}
struct MaskPlane {
    bounds: Rect,
    bytes: Vec<u8>,
}
impl MaskPlane {
    fn value(x: i64, y: i64) -> u8 {
        ((x * 16 + y * 3).rem_euclid(256)) as u8
    }
    fn new(bounds: Rect) -> Self {
        let mut bytes = Vec::new();
        for y in 0..i64::from(bounds.height) {
            for x in 0..i64::from(bounds.width) {
                bytes.push(Self::value(x, y));
            }
        }
        Self { bounds, bytes }
    }
    fn input(&self) -> MaskInput {
        MaskInput {
            key: SourceKey(9),
            placement: self.bounds,
            format: MaskFormat::CoverageU8,
            sha256: hsk_studio_prism::profile_hash(&self.bytes),
            inverted: false,
            density: 1.0,
        }
    }
}
struct Sources<'a> {
    tile: &'a Tile,
    mask: Option<&'a MaskPlane>,
}
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        (key.0 == 1).then(|| TilePlanes {
            width: self.tile.width,
            height: self.tile.height,
            colour: &self.tile.colour,
            colour_stride_bytes: u64::from(self.tile.width) * 12,
            alpha: &self.tile.alpha,
            alpha_stride_bytes: u64::from(self.tile.width) * 4,
            colour_sha256: hsk_studio_prism::profile_hash(&self.tile.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.tile.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        })
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        let m = self.mask.filter(|_| key.0 == 9)?;
        Some(Mask {
            bounds: m.bounds,
            sample_format: MaskFormat::CoverageU8.key(),
            bytes: &m.bytes,
            stride_bytes: u64::from(m.bounds.width),
            sha256: hsk_studio_prism::profile_hash(&m.bytes),
        })
    }
}
struct Image {
    extent: Rect,
    px: Vec<[f32; 4]>,
    calls: usize,
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y - self.extent.y) as usize * self.extent.width as usize
                    + (r.x - self.extent.x) as usize
                    + y * self.extent.width as usize
                    + x;
                self.px[at] = [
                    f(colour, i * 12),
                    f(colour, i * 12 + 4),
                    f(colour, i * 12 + 8),
                    f(alpha, i * 4),
                ];
            }
        }
        self.calls += 1;
        Ok(())
    }
}

const EXTENT: Rect = Rect {
    x: 0,
    y: 0,
    width: 40,
    height: 33,
};

/// 0: opaque backdrop; 1: the tile at (6, 5); 2: root [backdrop Normal, tile `mode` 0.8 + mask].
fn plan(tile: &Tile, mode: StudioBlendMode, mask: Option<MaskInput>) -> RenderPlan<NoVector> {
    RenderPlan {
        revision: 5,
        target: OutputTarget {
            extent: EXTENT,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: tag(),
        },
        nodes: vec![
            PlanNode::Solid {
                rect: EXTENT,
                rgba: BACKDROP,
            },
            PlanNode::Tile(tile.input(6, 5)),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    Layer {
                        input: PlanNodeId(0),
                        mode: NORMAL,
                        opacity: 1.0,
                        fill_opacity: 1.0,
                        mask: None,
                    },
                    Layer {
                        input: PlanNodeId(1),
                        mode,
                        opacity: 0.8,
                        fill_opacity: 1.0,
                        mask,
                    },
                ],
            }),
        ],
        root: PlanNodeId(2),
    }
}
fn envelope(t: Option<LayerTransform>) -> PlanExtensions {
    match t {
        None => PlanExtensions::default(),
        Some(t) => PlanExtensions::from_layers(vec![
            LayerExtension::new(PlanNodeId(2), 1).with_transform(t),
        ]),
    }
}
fn request(min: Exactness) -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 5,
        min_exactness: min,
        deadline: None,
        max_working_bytes: 1 << 30,
    }
}
fn execute(
    backend: &GpuBackend,
    plan: &RenderPlan<NoVector>,
    ext: &PlanExtensions,
    sources: &Sources<'_>,
) -> (Result<RenderReceipt, BackendError>, Image) {
    let mut image = Image {
        extent: plan.target.extent,
        px: vec![[f32::NAN; 4]; (plan.target.extent.width * plan.target.extent.height) as usize],
        calls: 0,
    };
    let result = backend.execute_extended(
        plan,
        ext,
        &request(Exactness::Approximate),
        sources,
        &CancellationToken::default(),
        &mut image,
    );
    (result, image)
}

fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| {
        GpuBackend::new(DeviceOptions {
            chunk_side: 16,
            ..DeviceOptions::default()
        })
    })
}
/// `true` when a device is live. Without an adapter: FAIL (`NOT_PROVEN: no adapter`) when the
/// committed policy requires the GPU, else print that line (counted NOT_PROVEN by the round).
fn gate(what: &str) -> bool {
    let b = backend();
    if b.device_info().is_some() {
        return true;
    }
    if let Some(failure) = b.failure() {
        panic!("{what}: adapter present but the backend failed: {failure:?}");
    }
    let strict = include_str!("gpu-proof.toml")
        .lines()
        .any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1");
    match b.unavailable_reason() {
        Some(Reason::NoAdapter) if strict => {
            panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)")
        }
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        other => panic!("{what}: no live device ({other:?})"),
    }
}

/// The reference pixel: pigment's binary64 sample composited by pigment's `composite_over` (what
/// render-cpu produces bit for bit).
fn reference(prep: &PreparedTransform, mask: Option<&dyn Fn(i64, i64) -> f32>, x: i64, y: i64) -> [f32; 4] {
    let site = |x, y| PixelSite { x, y, seed: 5 };
    let b = composite_over(
        NORMAL,
        Rgba::new([0.0; 3], 0.0),
        Rgba::new([BACKDROP[0], BACKDROP[1], BACKDROP[2]], 1.0),
        1.0,
        site(x, y),
    )
    .unwrap();
    let b = [b.rgb[0], b.rgb[1], b.rgb[2], b.a.min(1.0)];
    let s = prep
        .sample_rgba(x, y, |i, j| Ok::<_, ()>(tile_px(i, j)))
        .unwrap();
    let m = mask.map_or(1.0, |f| f(x, y));
    let opacity = 0.8 * m;
    if m <= 0.0 || opacity <= 0.0 || s[3] <= 0.0 {
        return b;
    }
    let o = composite_over(
        NORMAL,
        Rgba::new([b[0], b[1], b[2]], b[3]),
        Rgba::new([s[0], s[1], s[2]], s[3]),
        opacity,
        site(x, y),
    )
    .unwrap();
    [o.rgb[0], o.rgb[1], o.rgb[2], o.a.min(1.0)]
}

/// Largest premultiplied / alpha deviation and straight-colour deviation where alpha >= 1/4.
fn deviation(got: &[[f32; 4]], want: &[[f32; 4]]) -> (f32, f32) {
    let (mut premult, mut straight) = (0.0f32, 0.0f32);
    for (g, w) in got.iter().zip(want) {
        premult = premult.max((g[3] - w[3]).abs());
        for c in 0..3 {
            premult = premult.max((g[c] * g[3] - w[c] * w[3]).abs());
            if w[3] >= 0.25 {
                straight = straight.max((g[c] - w[c]).abs());
            }
        }
    }
    (premult, straight)
}

/// Resample parity: a scaled and rotated tile composited (Normal, 0.8) over an opaque backdrop,
/// every filter and edge mode, against the binary64 reference composited by pigment. The deviation
/// includes the composite kernel's own (Fitted) error.
#[test]
fn gpu_resample_parity_vs_pigment_reference() {
    if !gate("gpu_resample_parity_vs_pigment_reference") {
        return;
    }
    let tile = Tile::new(9, 7);
    let sources = Sources {
        tile: &tile,
        mask: None,
    };
    let src = rect(6, 5, 9, 7);
    // Scale 2.3 x 1.7, rotate 31 degrees about the tile centre, move by (3.25, 4.6).
    let affine = Affine2::from_parts(10.5, 8.5, 2.3, 1.7, 31.0, 3.25, 4.6);
    let mut worst = (0.0f32, 0.0f32);
    for filter in FILTERS {
        for edge in [EdgeMode::Transparent, EdgeMode::Clamp] {
            let t = LayerTransform::new(affine, filter).with_edge(edge);
            let prep = PreparedTransform::new(&t, src).unwrap();
            let (result, image) = execute(backend(), &plan(&tile, NORMAL, None), &envelope(Some(t)), &sources);
            result.unwrap();
            let want: Vec<[f32; 4]> = (0..EXTENT.height as i64)
                .flat_map(|y| (0..EXTENT.width as i64).map(move |x| (x, y)))
                .map(|(x, y)| reference(&prep, None, x, y))
                .collect();
            let (premult, straight) = deviation(&image.px, &want);
            println!(
                "transform parity {filter:?} {edge:?}: max |alpha|,|premultiplied| delta {premult:e}, straight (alpha>=1/4) {straight:e}"
            );
            worst = (worst.0.max(premult), worst.1.max(straight));
            assert!(premult <= TOLERANCE, "{filter:?} {edge:?}: {premult:e}");
            assert!(straight <= 4.0 * TOLERANCE, "{filter:?} {edge:?}: {straight:e}");
        }
    }
    println!("transform parity worst: {:e} premultiplied/alpha, {:e} straight", worst.0, worst.1);
}

#[test]
fn gpu_identity_integer_and_quarter_turn_equal_the_untransformed_plan() {
    if !gate("gpu_identity_integer_and_quarter_turn_equal_the_untransformed_plan") {
        return;
    }
    let tile = Tile::new(9, 7);
    let sources = Sources {
        tile: &tile,
        mask: None,
    };
    let multiply = StudioBlendMode::Multiply;
    let base = plan(&tile, multiply, None);
    let (r, plain) = execute(backend(), &base, &PlanExtensions::default(), &sources);
    r.unwrap();
    let mut moved_plan = plan(&tile, multiply, None);
    moved_plan.nodes[1] = PlanNode::Tile(tile.input(9, 3));
    let (r, moved) = execute(backend(), &moved_plan, &PlanExtensions::default(), &sources);
    r.unwrap();
    let bits = |i: &Image| i.px.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>();
    for filter in FILTERS {
        for edge in [EdgeMode::Transparent, EdgeMode::Clamp] {
            let id = LayerTransform::new(Affine2::IDENTITY, filter).with_edge(edge);
            let (r, got) = execute(backend(), &base, &envelope(Some(id)), &sources);
            r.unwrap();
            assert_eq!(bits(&got), bits(&plain), "identity {filter:?} {edge:?}");
            let shift = LayerTransform::new(Affine2::translate(3.0, -2.0), filter).with_edge(edge);
            let (r, got) = execute(backend(), &base, &envelope(Some(shift)), &sources);
            r.unwrap();
            assert_eq!(bits(&got), bits(&moved), "translate {filter:?} {edge:?}");
        }
        // Four quarter turns about a texel corner compose to the identity: each one is an exact
        // texel copy, checked against the binary64 reference composite bit for bit.
        let quarter = LayerTransform::new(
            Affine2::from_parts(10.0, 8.0, 1.0, 1.0, 90.0, 0.0, 0.0),
            filter,
        );
        let prep = PreparedTransform::new(&quarter, rect(6, 5, 9, 7)).unwrap();
        let (r, got) = execute(backend(), &plan(&tile, NORMAL, None), &envelope(Some(quarter)), &sources);
        r.unwrap();
        for y in 0..EXTENT.height as i64 {
            for x in 0..EXTENT.width as i64 {
                let want = reference(&prep, None, x, y);
                let g = got.px[(y * i64::from(EXTENT.width) + x) as usize];
                // The composite kernel itself is tolerance-equal (Fitted); the resampled source is
                // exact, so the deviation is the composite's alone.
                for c in 0..4 {
                    assert!((g[c] - want[c]).abs() <= TOLERANCE, "quarter {filter:?} ({x},{y})");
                }
            }
        }
    }
}

#[test]
fn gpu_linked_mask_follows_the_content() {
    if !gate("gpu_linked_mask_follows_the_content") {
        return;
    }
    let tile = Tile::new(9, 7);
    let mask = MaskPlane::new(EXTENT);
    let sources = Sources {
        tile: &tile,
        mask: Some(&mask),
    };
    let t = LayerTransform::new(
        Affine2::from_parts(10.5, 8.5, 1.25, 1.25, -17.0, 1.5, 0.75),
        ResampleFilter::Bilinear,
    );
    let prep = PreparedTransform::new(&t, rect(6, 5, 9, 7)).unwrap();
    let mask_prep = PreparedTransform::new(&t, EXTENT).unwrap();
    for linked in [false, true] {
        let (r, got) = execute(
            backend(),
            &plan(&tile, NORMAL, Some(mask.input())),
            &envelope(Some(t.with_mask(linked))),
            &sources,
        );
        r.unwrap();
        let coverage = |x: i64, y: i64| -> f32 {
            let raw = |mx: i64, my: i64| (f64::from(MaskPlane::value(mx, my)) / 255.0) as f32;
            if linked {
                mask_prep
                    .sample_coverage(x, y, |i, j| Ok::<_, ()>(raw(i64::from(i), i64::from(j))))
                    .unwrap()
            } else {
                raw(x, y)
            }
        };
        let want: Vec<[f32; 4]> = (0..EXTENT.height as i64)
            .flat_map(|y| (0..EXTENT.width as i64).map(move |x| (x, y)))
            .map(|(x, y)| reference(&prep, Some(&coverage), x, y))
            .collect();
        let (premult, straight) = deviation(&got.px, &want);
        println!("linked {linked}: premultiplied/alpha {premult:e}, straight {straight:e}");
        assert!(premult <= TOLERANCE && straight <= 4.0 * TOLERANCE, "linked {linked}");
    }
}

#[test]
fn transform_admission_refusals_run_without_adapter() {
    let none = GpuBackend::new(DeviceOptions {
        backend: BackendChoice::None,
        ..DeviceOptions::default()
    });
    let tile = Tile::new(9, 7);
    let sources = Sources {
        tile: &tile,
        mask: None,
    };
    let base = plan(&tile, NORMAL, None);
    let check = |ext: PlanExtensions, want: BackendError| {
        let (r, image) = execute(&none, &base, &ext, &sources);
        assert_eq!(r, Err(want));
        assert_eq!(image.calls, 0);
    };
    let matrix = |m: [f64; 6], error| {
        check(
            envelope(Some(LayerTransform::new(Affine2 { m }, ResampleFilter::Bicubic))),
            BackendError::InvalidPlan(PlanError::TransformMatrix { node: 2, error }),
        );
    };
    matrix([f64::NAN, 0.0, 0.0, 0.0, 1.0, 0.0], TransformError::NonFinite);
    matrix([1.0, 1.0, 0.0, 1.0, 1.0, 0.0], TransformError::Singular);
    matrix([0.0001, 0.0, 0.0, 0.0, 1.0, 0.0], TransformError::ScaleOutOfRange);
    let ok = LayerTransform::new(Affine2::translate(0.5, 0.5), ResampleFilter::Bicubic);
    check(
        PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 0).with_transform(ok)]),
        BackendError::InvalidPlan(PlanError::TransformTarget { node: 2 }),
    );
    check(
        PlanExtensions::from_layers(vec![
            LayerExtension::new(PlanNodeId(2), 1)
                .with_transform(ok)
                .with_effects(vec![EffectOp::ColourOverlay(ColourOverlayOp::new(
                    [1.0, 0.0, 0.0],
                    NORMAL,
                    1.0,
                ))]),
        ]),
        BackendError::InvalidPlan(PlanError::TransformCombination { node: 2 }),
    );
    // Declared: the class reported is pigment's Approximate, so a Fitted floor (which the blend
    // kernels pass) refuses it under the filter key.
    let mut image = Image {
        extent: EXTENT,
        px: vec![[0.0; 4]; (EXTENT.width * EXTENT.height) as usize],
        calls: 0,
    };
    let r = none.execute_extended(
        &base,
        &envelope(Some(ok)),
        &request(Exactness::Fitted),
        &sources,
        &CancellationToken::default(),
        &mut image,
    );
    assert_eq!(
        r,
        Err(BackendError::Unsupported {
            operator: "transform_bicubic",
            exactness: Exactness::Approximate
        })
    );
    // Admitted at Approximate: without an adapter the typed Unavailable follows admission.
    let (r, image) = execute(&none, &base, &envelope(Some(ok)), &sources);
    assert_eq!(r, Err(BackendError::Unavailable("no_adapter")));
    assert_eq!(image.calls, 0);
}

/// Projective (corner pin) parity, lane SMB-AE-KEY-TRACK-2: the host takes every sample site from
/// pigment's `PreparedTransform::sample_site` (binary64, the projective inverse included) and the
/// resample pass interpolates in binary32, so a perspective quad must match the binary64 reference
/// composited by pigment within the established tolerance, every filter and edge mode.
#[test]
fn gpu_projective_parity_vs_pigment_reference() {
    if !gate("gpu_projective_parity_vs_pigment_reference") {
        return;
    }
    let tile = Tile::new(9, 7);
    let sources = Sources { tile: &tile, mask: None };
    let src = rect(6, 5, 9, 7);
    let from = [[6.0, 5.0], [15.0, 5.0], [15.0, 12.0], [6.0, 12.0]];
    let to = [[4.5, 3.25], [33.0, 7.5], [29.75, 30.0], [2.0, 22.5]];
    let p = hsk_studio_pigment::transform_math::Projective2::from_quads(from, to).unwrap();
    let mut worst = (0.0f32, 0.0f32);
    for filter in FILTERS {
        for edge in [EdgeMode::Transparent, EdgeMode::Clamp] {
            let t = LayerTransform::projective(p, filter).with_edge(edge);
            let prep = PreparedTransform::new(&t, src).unwrap();
            let (result, image) = execute(backend(), &plan(&tile, NORMAL, None), &envelope(Some(t)), &sources);
            result.unwrap();
            let want: Vec<[f32; 4]> = (0..EXTENT.height as i64)
                .flat_map(|y| (0..EXTENT.width as i64).map(move |x| (x, y)))
                .map(|(x, y)| reference(&prep, None, x, y))
                .collect();
            let (premult, straight) = deviation(&image.px, &want);
            println!("projective parity {filter:?} {edge:?}: premultiplied/alpha {premult:e}, straight {straight:e}");
            worst = (worst.0.max(premult), worst.1.max(straight));
            assert!(premult <= TOLERANCE, "{filter:?} {edge:?}: {premult:e}");
            assert!(straight <= 4.0 * TOLERANCE, "{filter:?} {edge:?}: {straight:e}");
        }
    }
    println!("projective parity worst: {:e} premultiplied/alpha, {:e} straight", worst.0, worst.1);
}

/// Key pipelines (lane SMB-AE-KEY-TRACK-2) have no GPU pass: every stage kind is declared
/// Unsupported, so shared admission refuses the plan under the stage's key before any device work
/// or sink call, on any machine (no adapter needed).
#[test]
fn gpu_keying_is_refused_before_any_device_work() {
    use hsk_studio_pigment::keying::{ChromaKey, ChromaSpace, KeyStage, MatteCombine};
    use hsk_studio_pigment::render_plan::ExtOperator;
    let tile = Tile::new(9, 7);
    let sources = Sources { tile: &tile, mask: None };
    for stage in [
        KeyStage::Chroma(ChromaKey { key: [0.0, 1.0, 0.0], space: ChromaSpace::YCbCr, tolerance: 0.1, softness: 0.1, luma_weight: 0.0 }, MatteCombine::Replace),
        KeyStage::ChokeSpread { radius_px: 1 },
        KeyStage::Feather { radius_px: 1.0 },
        KeyStage::Levels { black: 0.1, white: 0.9 },
    ] {
        assert_eq!(backend().extension_exactness(ExtOperator::Key(stage.kind())), Exactness::Unsupported);
        let ext = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_keys(vec![stage.clone()])]);
        let (result, image) = execute(backend(), &plan(&tile, NORMAL, None), &ext, &sources);
        assert_eq!(result, Err(BackendError::Unsupported { operator: stage.key(), exactness: Exactness::Unsupported }), "{stage:?}");
        assert_eq!(image.calls, 0);
    }
}
