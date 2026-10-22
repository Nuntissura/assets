//! Vector layers with solid and image fills and live boolean groups on the GPU (lane
//! SMB-COMPOSITE-NEW-KINDS slices 2-3, decision B1): the GPU executes composite's folio-free
//! `vector_layer_render_plan` and matches the composition of pigment's single owners
//! (`PreparedTransform::sample_rgba`, `blend_math::composite_over`) over composite's coverage plane,
//! which render-cpu reproduces bit for bit (render-cpu tests/vector_paint.rs), within the tolerance
//! of the existing passes (resample 2e-6, blend 1e-6 per layer record). GPU proof policy:
//! tests/gpu-proof.toml (no adapter FAILS with `NOT_PROVEN: no adapter` while `require_gpu = true`).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::{
    BooleanOp, FillPaint, FillSpec, ImageFill, ImageInfo, ImageScale, PreparedPaint,
    VectorGeometry, VectorLayerSpec, VectorResolution, prepare_vector_layers,
    vector_layer_render_plan,
};
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExecuteRequest, OutputTarget, PlanSources,
    RenderBackend, SampleFormat, SinkError, SourceKey, StudioBlendMode, TilePlanes, TileSink,
};
use hsk_studio_pigment::transform_math::{EdgeMode, LayerTransform, PreparedTransform, ResampleFilter};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};

const W: u32 = 24;
const H: u32 = 20;
/// Resample pass tolerance (2e-6) plus four blend records at 1e-6.
const TOLERANCE: f32 = 6e-6;

fn tag() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight }
}
fn target() -> OutputTarget {
    OutputTarget { extent: Rect { x: 0, y: 0, width: W, height: H }, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() }
}
fn poly(points: &[(f64, f64)]) -> OwnedPath {
    let mut elements = vec![PathElement::MoveTo(PathPoint::new(points[0].0, points[0].1))];
    for p in &points[1..] {
        elements.push(PathElement::LineTo(PathPoint::new(p.0, p.1)));
    }
    elements.push(PathElement::Close);
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}
fn image_px(x: u32, y: u32) -> [f32; 4] {
    [x as f32 / 3.0, y as f32 / 3.0, 0.5, if (x + y).is_multiple_of(3) { 0.5 } else { 1.0 }]
}
struct Image {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Image {
    fn new() -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..4 {
            for x in 0..4 {
                let p = image_px(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self { colour, alpha }
    }
    fn info(&self) -> ImageInfo {
        ImageInfo {
            width: 4,
            height: 4,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: hsk_studio_prism::profile_hash(&self.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.alpha),
        }
    }
}
struct Sources<'a> {
    image: &'a Image,
    mask: Mask<'a>,
    mask_key: SourceKey,
}
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        (key != self.mask_key).then(|| TilePlanes {
            width: 4,
            height: 4,
            colour: &self.image.colour,
            colour_stride_bytes: 48,
            alpha: &self.image.alpha,
            alpha_stride_bytes: 16,
            colour_sha256: hsk_studio_prism::profile_hash(&self.image.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.image.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        })
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        (key == self.mask_key).then_some(Mask {
            bounds: self.mask.bounds,
            sample_format: self.mask.sample_format,
            bytes: self.mask.bytes,
            stride_bytes: self.mask.stride_bytes,
            sha256: self.mask.sha256,
        })
    }
}
struct Grid(Vec<[f32; 4]>);
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                self.0[(r.y as usize + y) * W as usize + r.x as usize + x] =
                    [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}
fn over(mode: StudioBlendMode, b: [f32; 4], s: [f32; 4], opacity: f32, x: i64, y: i64) -> [f32; 4] {
    if opacity <= 0.0 || s[3] <= 0.0 {
        return b;
    }
    let o = composite_over(mode, Rgba::new([b[0], b[1], b[2]], b[3]), Rgba::new([s[0], s[1], s[2]], s[3]), opacity, PixelSite { x, y, seed: 7 }).unwrap();
    [o.rgb[0], o.rgb[1], o.rgb[2], o.a.min(1.0)]
}
fn spec(info: ImageInfo, boolean: bool) -> VectorLayerSpec {
    let geometry = if boolean {
        VectorGeometry::Boolean {
            op: BooleanOp::Subtract,
            operands: vec![
                VectorGeometry::Paths(vec![poly(&[(2.0, 3.0), (14.0, 3.0), (14.0, 13.0), (2.0, 13.0)])]),
                VectorGeometry::Paths(vec![poly(&[(6.5, 6.5), (20.0, 6.5), (20.0, 17.0)])]),
            ],
        }
    } else {
        VectorGeometry::Paths(vec![poly(&[(2.0, 3.0), (14.0, 3.0), (2.0, 13.0)])])
    };
    VectorLayerSpec {
        layer_id: "SLYR-test".into(),
        geometry,
        fills: vec![
            FillSpec { paint: FillPaint::Solid([0.25, 0.5, 0.75, 1.0]), mode: StudioBlendMode::Normal, opacity: 1.0 },
            FillSpec {
                paint: FillPaint::Image(ImageFill {
                    source_id: "sha256:img".into(),
                    info,
                    scale: ImageScale::Crop,
                    transform: [0.5, 0.0, 0.0, 0.5, 0.25, 0.25],
                    scaling_factor: 1.0,
                    rotation: 0.0,
                }),
                mode: StudioBlendMode::Multiply,
                opacity: 0.4,
            },
        ],
    }
}
fn gate(backend: &GpuBackend) -> bool {
    if backend.device_info().is_some() {
        return true;
    }
    if let Some(failure) = backend.failure() {
        panic!("adapter present but the backend failed: {failure:?}");
    }
    let strict = include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1");
    match backend.unavailable_reason() {
        Some(Reason::NoAdapter) if strict => panic!("NOT_PROVEN: no adapter; gpu vector parity requires a GPU (tests/gpu-proof.toml)"),
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; gpu vector parity not run");
            false
        }
        other => panic!("no live device ({other:?})"),
    }
}

#[test]
fn gpu_vector_fills_and_booleans_match_pigment_within_tolerance() {
    let backend = GpuBackend::new(DeviceOptions { chunk_side: 8, ..DeviceOptions::default() });
    if !gate(&backend) {
        return;
    }
    let image = Image::new();
    let cancel = CancellationToken::default();
    for boolean in [false, true] {
        let vectors = prepare_vector_layers(&[spec(image.info(), boolean)], &VectorResolution::default(), &cancel).unwrap();
        let plane = vectors.plane("SLYR-test").unwrap();
        let rect = plane.placement.unwrap();
        let low = vector_layer_render_plan(&vectors, "SLYR-test", 7, target()).unwrap();
        let (mask_key, _) = low.vector_masks[0].clone();
        let sources = Sources { image: &image, mask: plane.mask().unwrap(), mask_key };
        let PreparedPaint::Image { placements, .. } = &plane.fills[1].paint else { panic!() };
        let t = LayerTransform::new(placements[0], ResampleFilter::Bilinear).with_edge(EdgeMode::Clamp);
        let prep = PreparedTransform::new(&t, Rect { x: 0, y: 0, width: 4, height: 4 }).unwrap();
        let mut grid = Grid(vec![[f32::NAN; 4]; (W * H) as usize]);
        let request = ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 };
        backend.execute_extended(&low.plan, &low.extensions, &request, &sources, &cancel, &mut grid).unwrap();
        let mut worst = 0.0_f32;
        for y in 0..H as i64 {
            for x in 0..W as i64 {
                let inside = x >= rect.x && y >= rect.y && x < rect.x + rect.width as i64 && y < rect.y + rect.height as i64;
                let want = if inside {
                    let mut rows = over(StudioBlendMode::Normal, [0.0; 4], [0.25, 0.5, 0.75, 1.0], 1.0, x, y);
                    let s = prep.sample_rgba(x, y, |i, j| Ok::<_, ()>(image_px(i, j))).unwrap();
                    rows = over(StudioBlendMode::Multiply, rows, s, 0.4, x, y);
                    let c = plane.coverage[((y - rect.y) * rect.width as i64 + (x - rect.x)) as usize];
                    over(StudioBlendMode::Normal, [0.0; 4], rows, (f64::from(c) / 255.0) as f32, x, y)
                } else {
                    [0.0; 4]
                };
                let got = grid.0[(y * W as i64 + x) as usize];
                let pre = |p: [f32; 4]| [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]];
                for (g, w) in pre(got).iter().zip(pre(want)) {
                    worst = worst.max((g - w).abs());
                }
            }
        }
        println!("boolean {boolean}: max |delta| alpha/premultiplied colour = {worst:e} (tolerance {TOLERANCE:e})");
        assert!(worst <= TOLERANCE, "boolean {boolean}: {worst:e}");
    }
}
