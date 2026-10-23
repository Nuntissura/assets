//! Lane SMB-PS-ADJUST-STYLES-3: enabled smart-filter masks over a placed-asset instance on the CPU
//! backend (STU-RAS-037 per-filter masking, STU-FX-004). The masks are document-space coverage planes
//! of the scene pixels; composite resamples each into the source surface (nearest document pixel to
//! every surface pixel centre mapped by the instance transform, zero outside) and the stack runs in
//! source space before the placement (STU-CMP-010). Oracle: the source alone, the masks resampled
//! by this test's own loop, pigment's `SmartFilterStack::render` with them, then the placement by
//! pigment's `composite_over`; tolerance 1e-6. Unresolved, malformed and colliding masks are typed
//! refusals. Photoshop parity (document-space evaluation) UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    FilteredContent, FilteredMask, SceneComposition, SceneContent, SceneContext, SceneLayer, SceneLimits, ScenePixels,
    render_scene,
};
use hsk_studio_pigment::blend_math::{BlendSpace, PixelSite, Rgba, composite_over};
use hsk_studio_pigment::filter_math::LayerFilter;
use hsk_studio_pigment::render_plan::{
    ColourTag, Exactness, ExecuteRequest, MaskFormat, MaskInput, SourceKey, StudioBlendMode, TilePlanes,
};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::selection::CoveragePlane;
use hsk_studio_pigment::smart_filter::{SmartFilterEntryOp, SmartFilterOp, SmartFilterStack, StackEntry, StackFilter, StackLimits};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_cpu::Renderer;

/// Document masks: `L/a` coverage_u8 over (4, 1) 4 x 2, `L/b` coverage_u16le over (0, 0) 12 x 4.
struct Pixels {
    a: Vec<u8>,
    b: Vec<u8>,
    a_hash: [u8; 32],
}
const A: Rect = Rect { x: 4, y: 1, width: 4, height: 2 };
const B: Rect = Rect { x: 0, y: 0, width: 12, height: 4 };
impl Pixels {
    fn new() -> Self {
        let a = vec![255, 128, 0, 64, 32, 255, 200, 10];
        let mut b = Vec::new();
        for y in 0..4u32 {
            for x in 0..12u32 {
                b.extend_from_slice(&((x * 5000 + y * 700) as u16).to_le_bytes());
            }
        }
        let a_hash = hsk_studio_prism::profile_hash(&a);
        Self { a, b, a_hash }
    }
    fn a_at(&self, x: i64, y: i64) -> u16 {
        if x < A.x || y < A.y || x >= A.x + i64::from(A.width) || y >= A.y + i64::from(A.height) {
            return 0;
        }
        u16::from(self.a[((y - A.y) * 4 + (x - A.x)) as usize]) * 257
    }
    fn b_at(&self, x: i64, y: i64) -> u16 {
        if x < 0 || y < 0 || x >= 12 || y >= 4 {
            return 0;
        }
        let at = ((y * 12 + x) * 2) as usize;
        u16::from_le_bytes([self.b[at], self.b[at + 1]])
    }
}
impl ScenePixels for Pixels {
    fn tile(&self, _: &str) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, id: &str) -> Option<Mask<'_>> {
        match id {
            "L/a" => Some(Mask { bounds: A, sample_format: "coverage_u8", bytes: &self.a, stride_bytes: 4, sha256: self.a_hash }),
            "L/b" => Some(Mask {
                bounds: B,
                sample_format: "coverage_u16le",
                bytes: &self.b,
                stride_bytes: 24,
                sha256: hsk_studio_prism::profile_hash(&self.b),
            }),
            // a plane whose hash does not match its bytes
            "L/bad" => Some(Mask { bounds: A, sample_format: "coverage_u8", bytes: &self.a, stride_bytes: 4, sha256: [1; 32] }),
            _ => None,
        }
    }
}

fn layer(id: &str, content: SceneContent, matrix: Affine2) -> SceneLayer {
    SceneLayer { layer_id: id.into(), content, mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 1.0, matrix, matte: None, clipped: false }
}
fn scene(id: &str, width: u32, height: u32, background: Option<[f32; 4]>, layers: Vec<SceneLayer>) -> SceneComposition {
    SceneComposition { composition_id: id.into(), width, height, background, filter: ResampleFilter::NearestNeighbour, layers }
}
/// 6 x 4 blue-grey source with a 2 x 2 red square at (2, 1).
fn source() -> SceneComposition {
    let red = scene("red", 2, 2, Some([0.9, 0.1, 0.1, 1.0]), vec![]);
    scene("src1", 6, 4, Some([0.2, 0.4, 0.6, 1.0]), vec![layer("square", SceneContent::Precomp(Box::new(red)), Affine2::translate(2.0, 1.0))])
}
const SRC: Rect = Rect { x: 0, y: 0, width: 6, height: 4 };
fn placeholder(key: u32) -> MaskInput {
    MaskInput { key: SourceKey(key), placement: SRC, format: MaskFormat::CoverageU16Le, sha256: [0; 32], inverted: false, density: 1.0 }
}
/// Blur (entry mask `L/a`), median at 0.5; stack mask `L/b`.
fn op() -> SmartFilterOp {
    SmartFilterOp {
        entries: vec![
            SmartFilterEntryOp { filter: StackFilter::Filter(LayerFilter::GaussianBlur { radius_px: 1.5 }), enabled: true, opacity: 1.0, mask: Some(placeholder(1)) },
            SmartFilterEntryOp { filter: StackFilter::Filter(LayerFilter::Median { radius_px: 1 }), enabled: true, opacity: 0.5, mask: None },
        ],
        stack_mask: Some(placeholder(2)),
    }
}
fn masks(at: Affine2) -> Vec<FilteredMask> {
    vec![
        FilteredMask { key: SourceKey(1), mask_id: "L/a".into(), to_document: at },
        FilteredMask { key: SourceKey(2), mask_id: "L/b".into(), to_document: at },
    ]
}
fn filtered(masks: Vec<FilteredMask>, op: SmartFilterOp) -> SceneContent {
    SceneContent::Filtered(Box::new(FilteredContent { content: SceneContent::Precomp(Box::new(source())), rect: SRC, filters: op, masks }))
}
fn ctx<'a>(renderer: &'a Renderer, pixels: &'a Pixels) -> SceneContext<'a> {
    SceneContext {
        backend: renderer,
        pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 26 },
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
        limits: SceneLimits::default(),
    }
}
fn render(s: &SceneComposition, pixels: &Pixels) -> Result<Vec<[f32; 4]>, String> {
    let renderer = Renderer::new(64).unwrap();
    let out = render_scene(s, &ctx(&renderer, pixels), &CancellationToken::default()).map_err(|e| format!("{e:?}"))?;
    Ok((0..s.height).flat_map(|y| (0..s.width).map(move |x| (x, y))).map(|(x, y)| out.pixel(x, y).unwrap()).collect())
}
fn over_clear(p: [f32; 4], x: i64, y: i64) -> [f32; 4] {
    if p[3] <= 0.0 {
        return [0.0; 4];
    }
    let o = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([p[0], p[1], p[2]], p[3]), 1.0, PixelSite { x, y, seed: 7 }).unwrap();
    [o.rgb[0], o.rgb[1], o.rgb[2], o.a]
}

#[test]
fn cpu_masked_placed_instance_matches_pigment_stack_and_refuses_typed() {
    let pixels = Pixels::new();
    let at = Affine2::translate(3.0, 0.0);
    let doc = scene("doc", 12, 4, None, vec![layer("SLYR-1", filtered(masks(at), op()), at)]);
    let got = render(&doc, &pixels).expect("renders");
    // oracle: source-space masks by the nearest document pixel to each centre (u + 3.5, v + 0.5)
    let resample = |f: &dyn Fn(i64, i64) -> u16| CoveragePlane {
        bounds: SRC,
        data: (0..4i64).flat_map(|v| (0..6i64).map(move |u| (u, v))).map(|(u, v)| f(u + 3, v)).collect(),
    };
    let entry_mask = resample(&|x, y| pixels.a_at(x, y));
    let stack_mask = resample(&|x, y| pixels.b_at(x, y));
    let o = op();
    let stack = SmartFilterStack {
        entries: o
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| StackEntry { filter: e.filter.clone(), enabled: e.enabled, opacity: e.opacity, mask: (i == 0).then(|| entry_mask.clone()) })
            .collect(),
        stack_mask: Some(stack_mask),
    };
    let plain = render(&source(), &pixels).unwrap();
    let out = stack.render(&RgbaPlane::new(SRC, plain).unwrap(), &StackLimits::default()).unwrap();
    let mut worst = 0.0f32;
    for y in 0..4i64 {
        for x in 0..12i64 {
            let want = if (3..9).contains(&x) { over_clear(over_clear(out.data[(y * 6 + x - 3) as usize], x - 3, y), x, y) } else { [0.0; 4] };
            let g = got[(y * 12 + x) as usize];
            for k in 0..4 {
                worst = worst.max((g[k] - want[k]).abs());
            }
        }
    }
    println!("masked placed smart filter: max |cpu - pigment stack| = {worst:e} (tolerance 1e-6)");
    assert!(worst <= 1e-6, "{worst:e}");
    // the masks matter: the unmasked stack renders differently
    let mut unmasked = op();
    unmasked.entries[0].mask = None;
    unmasked.stack_mask = None;
    let plain_doc = scene("doc", 12, 4, None, vec![layer("SLYR-1", filtered(Vec::new(), unmasked), at)]);
    assert!(render(&plain_doc, &pixels).unwrap().iter().zip(&got).any(|(a, b)| a != b));

    // typed refusals before any pixel is produced
    let refused = |m: Vec<FilteredMask>, o: SmartFilterOp, code: &str| {
        let err = render(&scene("doc", 12, 4, None, vec![layer("SLYR-1", filtered(m, o), at)]), &pixels).unwrap_err();
        assert!(err.contains(code), "{code}: {err}");
    };
    // a mask id the scene pixels do not hold
    let mut missing = masks(at);
    missing[0].mask_id = "L/none".into();
    refused(missing, op(), "filtered_mask_unresolved");
    // a document plane whose hash does not match its bytes
    let mut bad = masks(at);
    bad[0].mask_id = "L/bad".into();
    refused(bad, op(), "filtered_mask_plane");
    // a mask on key 0 (the surface tile) and a stack naming a key no mask provides
    let mut zero = masks(at);
    zero[1].key = SourceKey(0);
    refused(zero, op(), "filtered_mask_key");
    refused(masks(at)[..1].to_vec(), op(), "filtered_mask_unresolved");
}
