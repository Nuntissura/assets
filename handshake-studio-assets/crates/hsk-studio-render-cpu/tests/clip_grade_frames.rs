//! Clip colour grade frames on the CPU backend (lane SMB-PREMIERE-COLOR-TITLES slice 1; decision B1:
//! cross-crate render proofs live in renderer test files). A clip's content is wrapped by
//! `composite::clip_finishing::grade_layer`, rendered by `composite::render_scene` through this
//! backend, and every pixel is compared with values computed by hand from the STU-COL-160 stage
//! order (input LUT, primary, look mixed by intensity, vignette) and the STU-COL-222 maths.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::clip_finishing::{
    CLIP_CROP_OPERATOR, CLIP_GRADE_OPERATOR, CLIP_LUT_DIGEST, CLIP_LUT_UNRESOLVED, ClipEffectFrame, ClipGrade,
    ContentDigest, LutSource, NoLuts, StudioColorGrade, StudioGradeBasic, StudioGradeVignette, StudioLutRef,
    StudioSmartFilterKind, apply_effects, filters, grade_layer,
};
use hsk_studio_composite::{
    RenderedScene, SceneComposition, SceneContent, SceneContext, SceneLayer, SceneLimits, ScenePixels, SceneTile,
    StudioBlendMode, render_scene,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::BlendSpace;
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, Exactness, ExecuteRequest, SampleFormat, TilePlanes};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_prism::{CubeLut as Lut, parse_cube};
use hsk_studio_render_cpu::Renderer;
use std::sync::Arc;

const W: u32 = 4;
const H: u32 = 2;
const BASE: [f32; 3] = [0.5, 0.25, 0.75];

fn space() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded }
}

/// One 4 x 2 tile of colour BASE, alpha 1 except texel (3, 1), which is transparent.
struct Pixels {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Pixels {
    fn new() -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for i in 0..W * H {
            for c in BASE {
                colour.extend_from_slice(&c.to_le_bytes());
            }
            let a: f32 = if i == W * H - 1 { 0.0 } else { 1.0 };
            alpha.extend_from_slice(&a.to_le_bytes());
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

fn render(grade: ClipGrade, pixels: &Pixels) -> RenderedScene {
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
        layers: vec![grade_layer(layer, Rect { x: 0, y: 0, width: W, height: H }, Arc::new(grade))],
    };
    let renderer = Renderer::new(64).unwrap();
    let ctx = SceneContext {
        backend: &renderer,
        pixels,
        request: ExecuteRequest {
            expected_revision: 1,
            min_exactness: Exactness::Approximate,
            deadline: None,
            max_working_bytes: 1 << 24,
        },
        space: space(),
        limits: SceneLimits::default(),
    };
    render_scene(&scene, &ctx, &CancellationToken::default()).unwrap()
}

fn render_effects(effects: &[ClipEffectFrame], pixels: &Pixels, width: u32, height: u32) -> RenderedScene {
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
    let layer = apply_effects(layer, Rect { x: 0, y: 0, width: W, height: H }, effects).unwrap();
    let scene = SceneComposition {
        composition_id: "frame".into(),
        width,
        height,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers: vec![layer],
    };
    let renderer = Renderer::new(64).unwrap();
    let ctx = SceneContext {
        backend: &renderer,
        pixels,
        request: ExecuteRequest {
            expected_revision: 1,
            min_exactness: Exactness::Approximate,
            deadline: None,
            max_working_bytes: 1 << 24,
        },
        space: space(),
        limits: SceneLimits::default(),
    };
    render_scene(&scene, &ctx, &CancellationToken::default()).unwrap()
}

#[test]
fn clip_effect_stack_renders_crop_opacity_transform_and_blur_on_cpu() {
    let pixels = Pixels::new();
    // Crop left 25 % (1 px) and right 12.5 % (half a pixel), opacity 50 %, transform moving the
    // anchor (2, 1) to (6, 2): content texel (x, y) lands on (x + 4, y + 1) with alpha
    // coverage * 0.5 and colour BASE (straight).
    let effects = [
        ClipEffectFrame::Crop { left: 25.0, top: 0.0, right: 12.5, bottom: 0.0 },
        ClipEffectFrame::Opacity(50.0),
        ClipEffectFrame::Transform { anchor: [2.0, 1.0], position: [6.0, 2.0], scale: [100.0, 100.0], rotation: 0.0 },
    ];
    let r = render_effects(&effects, &pixels, 8, 4);
    for (x, y, a) in [(4u32, 1u32, 0.0f32), (5, 1, 0.5), (6, 2, 0.5), (7, 1, 0.25), (0, 0, 0.0), (5, 0, 0.0), (7, 2, 0.0)] {
        let (c, alpha) = pixel(&r, x, y);
        assert!((alpha - a).abs() <= 1e-6, "({x},{y}) alpha {alpha} vs {a}");
        if a > 0.0 {
            assert!(c.iter().zip(&BASE).all(|(p, q)| (p - q).abs() <= 1e-6), "({x},{y}) {c:?}");
        }
    }
    assert!(r.exactness.operators.iter().any(|o| o.key == CLIP_CROP_OPERATOR));

    // Gaussian blur (Pigment's filter over the clip frame): straight colour stays BASE wherever
    // coverage remains; the frame edge loses alpha, the transparent texel (3, 1) gains some.
    let blur = filters("SCLP-clip", StudioSmartFilterKind::GaussianBlur { radius_px: 1.0 }).unwrap();
    let r = render_effects(&[ClipEffectFrame::Filters(blur)], &pixels, W, H);
    let (c, a) = pixel(&r, 1, 0);
    assert!(a > 0.0 && a < 1.0, "edge alpha {a}");
    assert!(c.iter().zip(&BASE).all(|(p, q)| (p - q).abs() <= 1e-5), "{c:?}");
    assert!(pixel(&r, 3, 1).1 > 0.0);
}

fn pixel(r: &RenderedScene, x: u32, y: u32) -> ([f32; 3], f32) {
    let i = (y * r.width + x) as usize;
    let f = |b: &[u8], k: usize| f32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap());
    ([f(&r.colour, i * 3), f(&r.colour, i * 3 + 1), f(&r.colour, i * 3 + 2)], f(&r.alpha, i))
}

#[test]
fn clip_grade_renders_hand_values_through_composite_on_cpu() {
    let pixels = Pixels::new();
    let tol = 2e-6;

    // Temperature +100 -> STU-COL-222 Temperature 0.5: TempGain (2^0.5, 1, 2^-0.5), offset' 0.
    let g = StudioColorGrade {
        basic: StudioGradeBasic { temperature: 100.0, ..StudioGradeBasic::default() },
        ..StudioColorGrade::default()
    };
    let r = render(ClipGrade::prepare(&g, &NoLuts, [0.2126, 0.7152, 0.0722], "SCLP-clip").unwrap(), &pixels);
    let want = [0.5 * 2f32.sqrt(), 0.25, 0.75 / 2f32.sqrt()];
    for (x, y) in [(0, 0), (1, 0), (2, 1)] {
        let (c, a) = pixel(&r, x, y);
        assert!(c.iter().zip(&want).all(|(p, q)| (p - q).abs() <= tol), "{c:?} vs {want:?}");
        assert_eq!(a, 1.0);
    }
    assert!(r.exactness.operators.iter().any(|o| o.key == CLIP_GRADE_OPERATOR));
    // The transparent texel stays transparent.
    assert_eq!(pixel(&r, 3, 1).1, 0.0);

    // Input LUT (1D: red halved), exposure +1, look (1D invert) at 50 %, vignette -1 at midpoint 60:
    // [0.5, 0.25, 0.75] -> [0.25, 0.25, 0.75] -> [0.5, 0.5, 1.0] -> [0.5, 0.5, 0.5]; the outer
    // columns (d = 0.90) are halved, the inner ones (d = 0.56) are not.
    let input = Arc::new(parse_cube(b"LUT_1D_SIZE 2\n0 0 0\n0.5 1 1\n").unwrap());
    let look = Arc::new(parse_cube(b"LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap());
    let luts = Luts(vec![("in".into(), input.clone()), ("look".into(), look.clone())]);
    let g = StudioColorGrade {
        input_lut: Some(lut_ref("in", &input)),
        basic: StudioGradeBasic { exposure: 1.0, ..StudioGradeBasic::default() },
        look: Some(lut_ref("look", &look)),
        look_intensity: 50.0,
        vignette: Some(StudioGradeVignette { amount: -1.0, midpoint: 60.0, roundness: 0.0, feather: 0.0 }),
        ..StudioColorGrade::default()
    };
    let r = render(ClipGrade::prepare(&g, &luts, [0.2126, 0.7152, 0.0722], "SCLP-clip").unwrap(), &pixels);
    for (x, y, v) in [(0, 0, 0.25f32), (1, 0, 0.5), (2, 1, 0.5), (3, 0, 0.25), (0, 1, 0.25)] {
        let (c, _) = pixel(&r, x, y);
        assert!(c.iter().all(|p| (p - v).abs() <= tol), "({x},{y}) {c:?} vs {v}");
    }

    // Hostile: a LUT whose bytes do not match the recorded digest, and an unresolved LUT.
    let mut wrong = lut_ref("look", &look);
    wrong.content_digest.digest = hsk_studio_prism::hash_hex(&input.sha256);
    let g = StudioColorGrade { look: Some(wrong), ..StudioColorGrade::default() };
    let e = ClipGrade::prepare(&g, &luts, [0.2126, 0.7152, 0.0722], "SCLP-clip").unwrap_err();
    assert!(format!("{e:?}").contains(CLIP_LUT_DIGEST));
    let e = ClipGrade::prepare(&g, &NoLuts, [0.2126, 0.7152, 0.0722], "SCLP-clip").unwrap_err();
    assert!(format!("{e:?}").contains(CLIP_LUT_UNRESOLVED));
}
