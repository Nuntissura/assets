//! GPU bodies of legacy brightness / contrast and the second STU-RAS-151 adjustment set (lane
//! SMB-PS-ADJUST-STYLES-2). Every case runs an adjustment layer over a 17^3 colour lattice and is
//! compared with pigment's `composite_adjustment` (the owner of every formula, which render-cpu
//! runs bit for bit) within a tolerance declared per case before the run (binary32 shader against
//! pigment's binary32 / binary64 HSL helpers). GPU-execution proofs follow `tests/gpu-proof.toml`
//! (V4-03): without an adapter they FAIL with `NOT_PROVEN: no adapter` while `require_gpu = true`.
//! Admission refusals and the table budget are typed and happen before any sink call.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_colour::{
    BlackAndWhite, ChannelMixer, ColourBalance, ColourLookup, GradientMap, GradientMapStop,
    MatchColour, PhotoFilter, ReplaceColour, SelectiveColour, SelectiveColourMethod,
    ShadowsHighlights, ToneCompression, ToneCompressionMethod, Vibrance,
};
use hsk_studio_pigment::adjust_math::{AdjustmentOp, BrightnessContrast, composite_adjustment};
use hsk_studio_pigment::blend_math::Rgba;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer,
    LayerExtension, NoVector, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, PlanSources,
    RenderBackend, RenderPlan, SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput,
    TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

const EXTENT: Rect = Rect { x: 0, y: 0, width: 289, height: 17 };

fn tag() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight }
}

/// Lattice: x = i 17 + j -> (r, g) = (i / 16, j / 16), y -> b = y / 16, opaque.
fn lattice(x: u32, y: u32) -> [f32; 4] {
    [(x / 17) as f32 / 16.0, (x % 17) as f32 / 16.0, y as f32 / 16.0, 1.0]
}

struct Tile {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Tile {
    fn new() -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..EXTENT.height {
            for x in 0..EXTENT.width {
                let p = lattice(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self { colour, alpha }
    }
}
impl PlanSources for Tile {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        (key.0 == 1).then(|| TilePlanes {
            width: EXTENT.width,
            height: EXTENT.height,
            colour: &self.colour,
            colour_stride_bytes: u64::from(EXTENT.width) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(EXTENT.width) * 4,
            colour_sha256: hsk_studio_prism::profile_hash(&self.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        })
    }
    fn mask(&self, _key: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

struct Image {
    px: Vec<[f32; 4]>,
    calls: usize,
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y as usize + y) * EXTENT.width as usize + r.x as usize + x;
                self.px[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        self.calls += 1;
        Ok(())
    }
}

fn plan(tile: &Tile, mode: StudioBlendMode, opacity: f32) -> RenderPlan<NoVector> {
    RenderPlan {
        revision: 3,
        target: OutputTarget { extent: EXTENT, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() },
        nodes: vec![
            PlanNode::Tile(TileInput {
                key: SourceKey(1),
                placement: EXTENT,
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                colour_sha256: hsk_studio_prism::profile_hash(&tile.colour),
                alpha_sha256: hsk_studio_prism::profile_hash(&tile.alpha),
            }),
            PlanNode::Solid { rect: EXTENT, rgba: [0.0; 4] },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    Layer { input: PlanNodeId(0), mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 1.0, mask: None },
                    Layer { input: PlanNodeId(1), mode, opacity, fill_opacity: 1.0, mask: None },
                ],
            }),
        ],
        root: PlanNodeId(2),
    }
}

fn ext(op: AdjustmentOp) -> PlanExtensions {
    PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_adjustment(op)])
}

fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| GpuBackend::new(DeviceOptions { chunk_side: 64, ..DeviceOptions::default() }))
}

/// `true` when a device is live; without an adapter FAIL (`NOT_PROVEN: no adapter`) under the
/// committed strict policy.
fn gate(what: &str) -> bool {
    let b = backend();
    if b.device_info().is_some() {
        return true;
    }
    if let Some(failure) = b.failure() {
        panic!("{what}: adapter present but the backend failed: {failure:?} ({:?})", b.pipeline_fault());
    }
    let strict = include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1");
    match b.unavailable_reason() {
        Some(Reason::NoAdapter) if strict => panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)"),
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        other => panic!("{what}: no live device ({other:?})"),
    }
}

fn execute(b: &GpuBackend, p: &RenderPlan<NoVector>, e: &PlanExtensions, tile: &Tile, budget: u64) -> (Result<(), BackendError>, Image) {
    let mut image = Image { px: vec![[f32::NAN; 4]; (EXTENT.width * EXTENT.height) as usize], calls: 0 };
    let request = ExecuteRequest { expected_revision: 3, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: budget };
    let r = b.execute_extended(p, e, &request, tile, &CancellationToken::default(), &mut image).map(|_| ());
    (r, image)
}

/// Max |gpu - pigment composite_adjustment| over the lattice (alpha must stay 1 exactly).
fn parity(op: &AdjustmentOp, mode: StudioBlendMode, opacity: f32, tile: &Tile) -> f32 {
    let (r, image) = execute(backend(), &plan(tile, mode, opacity), &ext(op.clone()), tile, 1 << 30);
    r.unwrap_or_else(|e| panic!("{:?}: {e:?}", op.kind()));
    let prepared = op.prepare().unwrap();
    let mut worst = 0.0f32;
    for y in 0..EXTENT.height {
        for x in 0..EXTENT.width {
            let p = lattice(x, y);
            let want = composite_adjustment(mode, Rgba::new([p[0], p[1], p[2]], 1.0), &prepared, opacity).unwrap();
            let got = image.px[(y * EXTENT.width + x) as usize];
            assert_eq!(got[3], 1.0, "alpha changed at ({x}, {y})");
            for (g, w) in got.iter().zip(want.rgb) {
                worst = worst.max((g - w).abs());
            }
        }
    }
    worst
}

/// A deterministic non-identity cube of `side` (red fastest), values in [0, 1].
fn cube(side: u16) -> ColourLookup {
    let n = usize::from(side);
    let mut table = Vec::with_capacity(n * n * n);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let (r, g, b) = (r as f32 / (n - 1) as f32, g as f32 / (n - 1) as f32, b as f32 / (n - 1) as f32);
                table.push([(0.8 * r + 0.2 * b * g).min(1.0), (g * g * 0.9 + 0.05).min(1.0), (1.0 - b) * 0.5 + 0.25 * r]);
            }
        }
    }
    ColourLookup { side, table }
}

fn cases() -> Vec<(&'static str, AdjustmentOp, StudioBlendMode, f32, f32)> {
    use StudioBlendMode::{Multiply, Normal};
    let mut ranges = [[0.0f32; 4]; 9];
    ranges[0] = [-40.0, 20.0, 10.0, 5.0];
    ranges[3] = [30.0, -10.0, 0.0, -20.0];
    ranges[6] = [0.0, 0.0, 25.0, 10.0];
    ranges[7] = [10.0, 10.0, -10.0, 0.0];
    ranges[8] = [0.0, 0.0, 0.0, 30.0];
    vec![
        ("brightness_contrast legacy +", AdjustmentOp::BrightnessContrast(BrightnessContrast { brightness: 20.0, contrast: 40.0, use_legacy: true }), Normal, 1.0, 5e-6),
        ("brightness_contrast legacy -", AdjustmentOp::BrightnessContrast(BrightnessContrast { brightness: -30.0, contrast: -50.0, use_legacy: true }), Normal, 1.0, 5e-6),
        ("vibrance", AdjustmentOp::Vibrance(Vibrance { vibrance: 60.0, saturation: -20.0 }), Normal, 1.0, 5e-6),
        ("colour_balance preserve", AdjustmentOp::ColourBalance(ColourBalance { shadows: [30.0, -10.0, 5.0], midtones: [-20.0, 40.0, 0.0], highlights: [0.0, 10.0, -60.0], preserve_luminosity: true }), Normal, 1.0, 2e-5),
        ("colour_balance plain x0.7", AdjustmentOp::ColourBalance(ColourBalance { shadows: [30.0, -10.0, 5.0], midtones: [-20.0, 40.0, 0.0], highlights: [0.0, 10.0, -60.0], preserve_luminosity: false }), Normal, 0.7, 5e-6),
        ("photo_filter preserve", AdjustmentOp::PhotoFilter(PhotoFilter { colour: [0.93, 0.55, 0.1], density: 65.0, preserve_luminosity: true }), Normal, 1.0, 5e-6),
        ("black_and_white", AdjustmentOp::BlackAndWhite(BlackAndWhite { weights: [40.0, 60.0, 40.0, 60.0, 20.0, 80.0], tint: None }), Normal, 1.0, 5e-6),
        ("black_and_white tint", AdjustmentOp::BlackAndWhite(BlackAndWhite { weights: [-50.0, 120.0, 40.0, 200.0, 20.0, -80.0], tint: Some([42.0, 25.0]) }), Normal, 1.0, 2e-5),
        ("channel_mixer", AdjustmentOp::ChannelMixer(ChannelMixer { red: [80.0, 30.0, -10.0, 5.0], green: [0.0, 100.0, 0.0, 0.0], blue: [-20.0, 40.0, 90.0, -10.0], monochrome: false }), Multiply, 0.8, 5e-6),
        ("channel_mixer mono", AdjustmentOp::ChannelMixer(ChannelMixer { red: [30.0, 59.0, 11.0, 0.0], green: [0.0; 4], blue: [0.0; 4], monochrome: true }), Normal, 1.0, 5e-6),
        ("selective_colour relative", AdjustmentOp::SelectiveColour(SelectiveColour { method: SelectiveColourMethod::Relative, ranges }), Normal, 1.0, 5e-6),
        ("selective_colour absolute", AdjustmentOp::SelectiveColour(SelectiveColour { method: SelectiveColourMethod::Absolute, ranges }), Normal, 1.0, 5e-6),
        (
            "gradient_map reversed",
            AdjustmentOp::GradientMap(GradientMap {
                stops: vec![
                    GradientMapStop { position: 0.1, colour: [0.1, 0.0, 0.3] },
                    GradientMapStop { position: 0.45, colour: [0.9, 0.2, 0.1] },
                    GradientMapStop { position: 0.8, colour: [1.0, 0.95, 0.6] },
                ],
                reverse: true,
            }),
            Normal,
            1.0,
            5e-6,
        ),
        ("colour_lookup 17", AdjustmentOp::ColourLookup(cube(17)), Normal, 1.0, 5e-6),
        ("shadows_highlights", AdjustmentOp::ShadowsHighlights(ShadowsHighlights { shadow_amount: 50.0, shadow_tone_width: 60.0, shadow_radius: 0.0, highlight_amount: 30.0, highlight_tone_width: 40.0, highlight_radius: 0.0, colour_correction: 20.0, midtone_contrast: 15.0, black_clip: 0.01, white_clip: 0.01 }), Normal, 1.0, 5e-6),
        ("tone_compression exposure gamma", AdjustmentOp::ToneCompression(ToneCompression { method: ToneCompressionMethod::ExposureGamma, exposure: 0.7, gamma: 1.4 }), Normal, 1.0, 1e-5),
        ("tone_compression highlight", AdjustmentOp::ToneCompression(ToneCompression { method: ToneCompressionMethod::HighlightCompression, exposure: 1.5, gamma: 0.8 }), Normal, 1.0, 1e-5),
        ("match_colour neutralize", AdjustmentOp::MatchColour(MatchColour { luminance: 110.0, colour_intensity: 80.0, fade: 25.0, neutralize: true, source_mean: [0.6, 0.4, 0.3], source_std: [0.2, 0.15, 0.1], target_mean: [0.5, 0.5, 0.5], target_std: [0.25, 0.0, 0.2] }), Normal, 1.0, 5e-6),
        ("replace_colour", AdjustmentOp::ReplaceColour(ReplaceColour { samples: vec![[1.0, 0.25, 0.125], [0.0, 0.5, 1.0]], fuzziness: 90.0, hue: 70.0, saturation: 30.0, lightness: -15.0 }), Normal, 1.0, 2e-5),
        ("replace_colour exact", AdjustmentOp::ReplaceColour(ReplaceColour { samples: vec![[0.5, 0.25, 0.75]], fuzziness: 0.0, hue: -90.0, saturation: 0.0, lightness: 0.0 }), Normal, 1.0, 2e-5),
    ]
}

#[test]
fn gpu_second_set_adjustments_match_pigment_within_tolerance() {
    if !gate("gpu_second_set_adjustments_match_pigment_within_tolerance") {
        return;
    }
    let tile = Tile::new();
    let mut worst_all = 0.0f32;
    for (name, op, mode, opacity, tolerance) in cases() {
        let worst = parity(&op, mode, opacity, &tile);
        println!("GPU_ADJUST_2 {name:<34} max |gpu - pigment| {worst:e} (tolerance {tolerance:e})");
        assert!(worst <= tolerance, "{name}: {worst:e} > {tolerance:e}");
        worst_all = worst_all.max(worst);
    }
    // a changed colour proves the bodies run (not an identity fallback)
    let tile_plain = Tile::new();
    let (r, plain) = execute(backend(), &plan(&tile_plain, StudioBlendMode::Normal, 1.0), &PlanExtensions::default(), &tile_plain, 1 << 30);
    r.unwrap();
    let (r, mapped) = execute(backend(), &plan(&tile, StudioBlendMode::Normal, 1.0), &ext(AdjustmentOp::ColourLookup(cube(5))), &tile, 1 << 30);
    r.unwrap();
    assert!(plain.px.iter().zip(&mapped.px).filter(|(a, b)| a != b).count() > 4000);
    println!("GPU_ADJUST_2 worst {worst_all:e}");
}

/// The largest admitted cube (side 65, 3.3 MB table) renders; the same cube under a working-byte
/// budget below the table is a typed budget refusal before any sink call (hostile size).
#[test]
fn gpu_colour_lookup_65_cube_and_table_budget() {
    if !gate("gpu_colour_lookup_65_cube_and_table_budget") {
        return;
    }
    let tile = Tile::new();
    let op = AdjustmentOp::ColourLookup(cube(65));
    let worst = parity(&op, StudioBlendMode::Normal, 1.0, &tile);
    println!("GPU_ADJUST_2 colour_lookup 65 max |gpu - pigment| {worst:e} (tolerance 5e-6)");
    assert!(worst <= 5e-6, "{worst:e}");
    let table_bytes = 65u64 * 65 * 65 * 3 * 4;
    let (r, image) = execute(backend(), &plan(&tile, StudioBlendMode::Normal, 1.0), &ext(op), &tile, table_bytes / 2);
    assert!(matches!(r, Err(BackendError::BudgetExceeded { .. })), "{r:?}");
    assert_eq!(image.calls, 0);
}

/// Admission refusals of the new kinds are typed with the kind's key and need no device:
/// shadows/highlights with a radius, local-adaptation and equalise-histogram tone compression and
/// modern brightness / contrast stay pigment refusals on every backend; a hostile cube side or a
/// cube whose table length disagrees with its side is a parameter error before any upload.
#[test]
fn second_set_refusals_are_typed_without_adapter() {
    let b = GpuBackend::new(DeviceOptions { backend: hsk_studio_render_gpu::BackendChoice::None, ..DeviceOptions::default() });
    let tile = Tile::new();
    let p = plan(&tile, StudioBlendMode::Normal, 1.0);
    let refused = |op: AdjustmentOp, key: &str| {
        let (r, image) = execute(&b, &p, &ext(op), &tile, 1 << 30);
        assert!(matches!(r, Err(BackendError::Unsupported { operator, .. }) if operator == key), "{key}: {r:?}");
        assert_eq!(image.calls, 0);
    };
    refused(AdjustmentOp::ToneCompression(ToneCompression { method: ToneCompressionMethod::LocalAdaptation, exposure: 0.0, gamma: 1.0 }), "tone_compression");
    refused(AdjustmentOp::ToneCompression(ToneCompression { method: ToneCompressionMethod::EqualizeHistogram, exposure: 0.0, gamma: 1.0 }), "tone_compression");
    refused(AdjustmentOp::BrightnessContrast(BrightnessContrast { brightness: 0.0, contrast: 0.0, use_legacy: false }), "brightness_contrast");
    let mut radius = ShadowsHighlights { shadow_amount: 50.0, shadow_tone_width: 50.0, shadow_radius: 30.0, highlight_amount: 0.0, highlight_tone_width: 50.0, highlight_radius: 0.0, colour_correction: 0.0, midtone_contrast: 0.0, black_clip: 0.0, white_clip: 0.0 };
    refused(AdjustmentOp::ShadowsHighlights(radius), "shadows_highlights");
    radius.shadow_radius = 0.0;
    // hostile cube: side beyond the ceiling with an empty table; and a short table
    for bad in [ColourLookup { side: u16::MAX, table: vec![] }, ColourLookup { side: 3, table: vec![[0.5; 3]; 26] }] {
        let (r, image) = execute(&b, &p, &ext(AdjustmentOp::ColourLookup(bad)), &tile, 1 << 30);
        assert!(matches!(r, Err(BackendError::InvalidPlan(_))), "{r:?}");
        assert_eq!(image.calls, 0);
    }
    // the admitted form reaches the device stage (no adapter here)
    let (r, _) = execute(&b, &p, &ext(AdjustmentOp::ShadowsHighlights(radius)), &tile, 1 << 30);
    assert_eq!(r, Err(BackendError::Unavailable("no_adapter")));
}
