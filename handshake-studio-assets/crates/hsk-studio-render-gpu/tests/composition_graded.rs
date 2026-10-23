//! Lane SMB-PR-SOURCE-MONITOR slice 2: composite `SceneContent::Graded` on the GPU backend. The
//! grade runs in this backend's `grade.wgsl` kernel (pigment `RenderBackend::execute_grade`), not on
//! the host, and every graded texel is compared with composite's host grade `ClipGrade::apply` (the
//! CPU path render-cpu runs) on the same source texel. Tolerance: 1e-4, the established tolerance of a
//! binary32 GPU transcription of host colour maths (`tests/gpu_keying.rs`); the grade's power and
//! exponential functions run at the GPU's binary32 accuracy. GPU-execution proofs follow
//! `tests/gpu-proof.toml` (V4-03). Lumetri parity UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::clip_finishing::{
    CLIP_GRADE_OPERATOR, ClipGrade, ContentDigest, LutSource, StudioColorGrade, StudioGradeBasic, StudioGradeCurves,
    StudioGradeKey, StudioGradeSecondary, StudioGradeVignette, StudioGradeWheels, StudioKeyRange, StudioLutRef,
    StudioWheelBand, grade_layer,
};
use hsk_studio_composite::{
    RenderedScene, SceneComposition, SceneContent, SceneContext, SceneLayer, SceneLimits, ScenePixels, SceneTile,
    StudioBlendMode, render_scene,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::BlendSpace;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, ColourTag, Exactness, ExecuteRequest, GradeSurface, RenderBackend, SampleFormat,
    TilePlanes,
};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_prism::{CubeLut as Lut, parse_cube};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::{Arc, OnceLock};

/// Odd sides: the 8 x 8 workgroups overhang both edges.
const W: u32 = 37;
const H: u32 = 23;
const TOLERANCE: f32 = 1e-4;
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| GpuBackend::new(DeviceOptions { chunk_side: 16, ..DeviceOptions::default() }))
}
fn gate(what: &str) -> bool {
    let b = backend();
    if b.device_info().is_some() {
        return true;
    }
    if let Some(failure) = b.failure() {
        panic!("{what}: adapter present but the backend failed: {failure:?}");
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

fn space() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded }
}

/// A W x H tile sweeping hue, saturation and lightness; alpha 1, with a transparent column 0 and
/// alpha 0.5 on row 0.
struct Pixels {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
fn source(x: u32, y: u32) -> ([f32; 3], f32) {
    let (u, v) = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
    let rgb = [
        (0.05 + 0.9 * u).min(1.0),
        (0.5 + 0.45 * (6.0 * u + 3.0 * v).sin()).clamp(0.0, 1.0),
        (0.95 - 0.85 * v).max(0.0),
    ];
    let a = if x == 0 {
        0.0
    } else if y == 0 {
        0.5
    } else {
        1.0
    };
    (rgb, a)
}
impl Pixels {
    fn new() -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..H {
            for x in 0..W {
                let (rgb, a) = source(x, y);
                for c in rgb {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&a.to_le_bytes());
            }
        }
        Self { colour, alpha }
    }
    fn tile(&self) -> SceneTile {
        SceneTile {
            tile_id: "clip".into(),
            placement: Rect { x: 0, y: 0, width: W, height: H },
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: hsk_studio_prism::profile_hash(&self.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.alpha),
        }
    }
}
impl ScenePixels for Pixels {
    fn tile(&self, tile_id: &str) -> Option<TilePlanes<'_>> {
        (tile_id == "clip").then(|| TilePlanes {
            width: W,
            height: H,
            colour: &self.colour,
            colour_stride_bytes: u64::from(W) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(W) * 4,
            colour_sha256: hsk_studio_prism::profile_hash(&self.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: space(),
        })
    }
}

struct Luts(Vec<(String, Arc<Lut>)>);
impl LutSource for Luts {
    fn lut(&self, r: &StudioLutRef) -> Option<Arc<Lut>> {
        self.0.iter().find(|(id, _)| *id == r.artifact_manifest_id).map(|(_, l)| l.clone())
    }
}
fn lut_ref(id: &str, lut: &Lut) -> StudioLutRef {
    StudioLutRef {
        artifact_manifest_id: id.into(),
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: hsk_studio_prism::hash_hex(&lut.sha256) },
    }
}
/// A 5^3 table of a smooth, non-separable function (so the tetrahedral weights matter), optionally
/// behind a 1D shaper, with a non-unit domain.
fn cube(shaper: bool) -> Lut {
    let mut s = String::from("TITLE \"gpu grade\"\n");
    if shaper {
        s.push_str("LUT_1D_SIZE 4\n");
    }
    s.push_str("LUT_3D_SIZE 5\nDOMAIN_MIN -0.1 0 0\nDOMAIN_MAX 1.1 1 1.2\n");
    if shaper {
        for i in 0..4 {
            let t = f64::from(i) / 3.0;
            s.push_str(&format!("{} {} {}\n", t * t, t.sqrt(), t));
        }
    }
    for b in 0..5 {
        for g in 0..5 {
            for r in 0..5 {
                let (r, g, b) = (f64::from(r) / 4.0, f64::from(g) / 4.0, f64::from(b) / 4.0);
                s.push_str(&format!("{} {} {}\n", 0.8 * r + 0.15 * g * b, (g * 0.9 + 0.1 * r * r).min(1.0), 0.6 * b + 0.3 * r * g + 0.05));
            }
        }
    }
    parse_cube(s.as_bytes()).unwrap()
}

fn grades() -> (Vec<(&'static str, StudioColorGrade)>, Luts) {
    let input = Arc::new(cube(false));
    let look = Arc::new(cube(true));
    let luts = Luts(vec![("in".into(), input.clone()), ("look".into(), look.clone())]);
    let band = |o: [f64; 3], g: [f64; 3], k: [f64; 3]| StudioWheelBand { offset: o, gamma: g, gain: k };
    let key = |lo: f64, hi: f64| StudioGradeKey {
        hue: StudioKeyRange { lo, hi, soft: 40.0 },
        saturation: StudioKeyRange { lo: 0.1, hi: 1.0, soft: 0.1 },
        luminance: StudioKeyRange { lo: 0.05, hi: 0.95, soft: 0.05 },
        invert: false,
    };
    let list = vec![
        (
            "basic",
            StudioColorGrade {
                basic: StudioGradeBasic {
                    temperature: 40.0,
                    tint: -20.0,
                    exposure: 0.4,
                    contrast: 30.0,
                    highlights: -40.0,
                    shadows: 30.0,
                    whites: 10.0,
                    blacks: -10.0,
                    saturation: 130.0,
                    vibrance: 25.0,
                    ..StudioGradeBasic::default()
                },
                ..StudioColorGrade::default()
            },
        ),
        (
            "luts_faded",
            StudioColorGrade {
                input_lut: Some(lut_ref("in", &input)),
                look: Some(lut_ref("look", &look)),
                look_intensity: 70.0,
                faded_film: 80.0,
                ..StudioColorGrade::default()
            },
        ),
        (
            "curves",
            StudioColorGrade {
                curves: StudioGradeCurves {
                    composite: vec![[0.0, 0.05], [0.3, 0.25], [0.7, 0.8], [1.0, 0.97]],
                    red: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]],
                    hue_vs_saturation: vec![[0.0, 0.5], [0.33, 0.7], [0.66, 0.35], [1.0, 0.5]],
                    luminance_vs_saturation: vec![[0.0, 0.4], [0.5, 0.6], [1.0, 0.45]],
                    hue_vs_hue: vec![[0.0, 0.5], [0.5, 0.55], [1.0, 0.5]],
                    ..StudioGradeCurves::default()
                },
                ..StudioColorGrade::default()
            },
        ),
        (
            "wheels_secondaries_vignette",
            StudioColorGrade {
                wheels: StudioGradeWheels {
                    overall: band([0.01, 0.0, -0.01], [0.05, 0.0, 0.0], [0.0, 0.03, 0.0]),
                    shadows: band([0.02, 0.0, 0.03], [0.0, 0.1, 0.0], [0.0; 3]),
                    midtones: band([0.0; 3], [0.1, -0.05, 0.0], [0.0, 0.0, 0.05]),
                    highlights: band([0.0, -0.02, 0.0], [0.0; 3], [-0.05, 0.0, 0.0]),
                    contrast: 0.2,
                    pivot: 0.45,
                    input_saturation: 1.1,
                    final_saturation: 0.9,
                    low_mid_split: 0.4,
                    mid_high_split: 0.6,
                },
                secondaries: vec![
                    StudioGradeSecondary { key: key(90.0, 150.0), temperature: -120.0, tint: 30.0, contrast: 20.0, saturation: 140.0 },
                    StudioGradeSecondary { key: key(340.0, 20.0), temperature: 60.0, tint: 0.0, contrast: -10.0, saturation: 70.0 },
                ],
                vignette: Some(StudioGradeVignette { amount: -1.5, midpoint: 40.0, roundness: -40.0, feather: 30.0 }),
                ..StudioColorGrade::default()
            },
        ),
    ];
    (list, luts)
}

fn render(grade: Arc<ClipGrade>, pixels: &Pixels) -> RenderedScene {
    let layer = SceneLayer {
        layer_id: "SCLP-clip".into(),
        content: SceneContent::Tiles(vec![pixels.tile()]),
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        matrix: Affine2::IDENTITY,
        matte: None,
        clipped: false,
    };
    let scene = SceneComposition {
        composition_id: "frame".into(),
        width: W,
        height: H,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers: vec![grade_layer(layer, Rect { x: 0, y: 0, width: W, height: H }, grade)],
    };
    let ctx = SceneContext {
        backend: backend(),
        pixels,
        request: ExecuteRequest { expected_revision: 1, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 26 },
        space: space(),
        limits: SceneLimits::default(),
    };
    render_scene(&scene, &ctx, &CancellationToken::default()).expect("graded frame renders")
}

#[test]
fn graded_scene_runs_the_grade_kernel_within_tolerance_of_the_host_grade() {
    if !gate("graded_scene_runs_the_grade_kernel_within_tolerance_of_the_host_grade") {
        return;
    }
    let pixels = Pixels::new();
    let (list, luts) = grades();
    for (name, g) in list {
        let grade = Arc::new(ClipGrade::prepare(&g, &luts, LUMA, "SCLP-clip").unwrap());
        // The kernel itself runs (Some), not the host fallback.
        let mut colour = pixels.colour.clone();
        let mut surface = GradeSurface { width: W, height: H, colour: &mut colour, alpha: &pixels.alpha };
        let direct = backend().execute_grade(&grade.program(), &mut surface, &CancellationToken::default());
        assert!(matches!(direct, Some(Ok(()))), "{name}: {direct:?} {:?}", backend().submit_fault());
        let got = render(grade.clone(), &pixels);
        assert!(got.exactness.operators.iter().any(|o| o.key == CLIP_GRADE_OPERATOR), "{name}");
        let mut worst = 0.0f32;
        let mut moved = 0usize;
        for y in 0..H {
            for x in 0..W {
                let (rgb, a) = source(x, y);
                let p = got.pixel(x, y).unwrap();
                assert!((p[3] - a).abs() <= 1e-6, "{name} ({x},{y}): alpha {} vs {a}", p[3]);
                let at = ((y * W + x) * 12) as usize;
                let k: Vec<f32> = (0..3).map(|c| f32::from_le_bytes(colour[at + 4 * c..at + 4 * c + 4].try_into().unwrap())).collect();
                if a <= 0.0 {
                    assert_eq!(&colour[at..at + 12], &pixels.colour[at..at + 12], "{name}: transparent texel untouched");
                    continue;
                }
                let want = grade.apply(rgb, f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(W), f64::from(H));
                for c in 0..3 {
                    worst = worst.max((p[c] - want[c]).abs()).max((k[c] - want[c]).abs());
                }
                if (0..3).any(|c| (want[c] - rgb[c]).abs() > 1e-3) {
                    moved += 1;
                }
            }
        }
        println!("{name}: gpu max |delta| vs host grade {worst:e}, {moved} texels changed by the grade");
        assert!(moved > 100, "{name}: the grade must change the frame ({moved})");
        assert!(worst <= TOLERANCE, "{name}: {worst}");
    }
}

#[test]
fn grade_kernel_refuses_hostile_programs_and_surfaces_before_device_work() {
    if !gate("grade_kernel_refuses_hostile_programs_and_surfaces_before_device_work") {
        return;
    }
    let pixels = Pixels::new();
    let (list, luts) = grades();
    let grade = ClipGrade::prepare(&list[0].1, &luts, LUMA, "SCLP-clip").unwrap();
    let cancel = CancellationToken::default();
    let code = |r: Option<Result<(), BackendError>>| match r {
        Some(Err(BackendError::Unsupported { operator, .. })) => operator,
        other => panic!("{other:?}"),
    };
    // A program outside Prism's bounds (NaN saturation, an oversized spline, a LUT whose rows do not
    // match its size) is refused before any device work; the surface is left unchanged.
    let mut bad = grade.program();
    bad.saturation = f32::NAN;
    let mut colour = pixels.colour.clone();
    let r = backend().execute_grade(&bad, &mut GradeSurface { width: W, height: H, colour: &mut colour, alpha: &pixels.alpha }, &cancel);
    assert_eq!(code(r), "clip_grade_program");
    assert_eq!(colour, pixels.colour);
    let mut bad = grade.program();
    bad.tone = Some(hsk_studio_prism::grade::GradeSpline { xs: vec![0.0; 20], ys: vec![0.0; 20], m: vec![0.0; 20] });
    let r = backend().execute_grade(&bad, &mut GradeSurface { width: W, height: H, colour: &mut colour, alpha: &pixels.alpha }, &cancel);
    assert_eq!(code(r), "clip_grade_program");
    let mut torn = cube(false);
    torn.lut_3d.pop();
    let mut bad = grade.program();
    bad.input_lut = Some(Arc::new(torn));
    let r = backend().execute_grade(&bad, &mut GradeSurface { width: W, height: H, colour: &mut colour, alpha: &pixels.alpha }, &cancel);
    assert_eq!(code(r), "clip_grade_program");
    // Planes that do not match the declared size (short alpha, u32::MAX sides).
    let r = backend().execute_grade(&grade.program(), &mut GradeSurface { width: W, height: H, colour: &mut colour, alpha: &pixels.alpha[4..] }, &cancel);
    assert_eq!(code(r), "clip_grade_surface");
    let r = backend().execute_grade(&grade.program(), &mut GradeSurface { width: u32::MAX, height: u32::MAX, colour: &mut colour, alpha: &pixels.alpha }, &cancel);
    assert_eq!(code(r), "clip_grade_surface");
    assert_eq!(colour, pixels.colour, "refusals leave the surface untouched");
    // A canceled token is typed before any device work.
    let canceled = CancellationToken::default();
    canceled.cancel();
    let r = backend().execute_grade(&grade.program(), &mut GradeSurface { width: W, height: H, colour: &mut colour, alpha: &pixels.alpha }, &canceled);
    assert!(matches!(r, Some(Err(BackendError::Canceled))), "{r:?}");
}
