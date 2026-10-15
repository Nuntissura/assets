//! Lane SMB-PS-ADJUST-STYLES-2: smart filters over a placed-asset instance render through the
//! composition path on the CPU backend (`SceneContent::Filtered`, built by composite's
//! `placed_scene_layer` from the layer's `smart_filters` record: composite
//! `tests/placed_smart_filters.rs`). Oracle: the source scene rendered alone, filtered by pigment's
//! `SmartFilterStack::render` (the owner of the stack maths), then composited by pigment's
//! `composite_over` at the instance's integer offset; tolerance 1e-6 (two straight-alpha
//! re-composites over transparent). Photoshop's document-space order is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    FilteredContent, SceneComposition, SceneContent, SceneContext, SceneLayer, SceneLimits, ScenePixels,
    render_scene,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{BlendSpace, PixelSite, Rgba, composite_over};
use hsk_studio_pigment::filter_math::LayerFilter;
use hsk_studio_pigment::render_plan::{ColourTag, Exactness, ExecuteRequest, StudioBlendMode, TilePlanes};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::smart_filter::{SmartFilterEntryOp, SmartFilterOp, SmartFilterStack, StackEntry, StackFilter, StackLimits};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_render_cpu::Renderer;

struct NoPixels;
impl ScenePixels for NoPixels {
    fn tile(&self, _: &str) -> Option<TilePlanes<'_>> {
        None
    }
}

fn layer(id: &str, content: SceneContent, matrix: Affine2) -> SceneLayer {
    SceneLayer {
        layer_id: id.into(),
        content,
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        matrix,
        matte: None,
        clipped: false,
    }
}
fn scene(id: &str, width: u32, height: u32, background: Option<[f32; 4]>, layers: Vec<SceneLayer>) -> SceneComposition {
    SceneComposition { composition_id: id.into(), width, height, background, filter: ResampleFilter::NearestNeighbour, layers }
}
/// 6 x 4 blue-grey source with a 2 x 2 red square at (2, 1).
fn source() -> SceneComposition {
    let red = scene("red", 2, 2, Some([0.9, 0.1, 0.1, 1.0]), vec![]);
    scene("src1", 6, 4, Some([0.2, 0.4, 0.6, 1.0]), vec![layer("square", SceneContent::Precomp(Box::new(red)), Affine2::translate(2.0, 1.0))])
}
fn op() -> SmartFilterOp {
    let entry = |filter: LayerFilter, opacity: f32| SmartFilterEntryOp { filter: StackFilter::Filter(filter), enabled: true, opacity, mask: None };
    SmartFilterOp { entries: vec![entry(LayerFilter::GaussianBlur { radius_px: 1.5 }, 1.0), entry(LayerFilter::Median { radius_px: 1 }, 0.5)], stack_mask: None }
}
fn render(s: &SceneComposition) -> Vec<[f32; 4]> {
    let renderer = Renderer::new(64).unwrap();
    let ctx = SceneContext {
        backend: &renderer,
        pixels: &NoPixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 26 },
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
        limits: SceneLimits::default(),
    };
    let out = render_scene(s, &ctx, &CancellationToken::default()).expect("renders");
    (0..s.height).flat_map(|y| (0..s.width).map(move |x| (x, y))).map(|(x, y)| out.pixel(x, y).unwrap()).collect()
}
fn over_clear(p: [f32; 4], x: i64, y: i64) -> [f32; 4] {
    if p[3] <= 0.0 {
        return [0.0; 4];
    }
    let o = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([p[0], p[1], p[2]], p[3]), 1.0, PixelSite { x, y, seed: 7 }).unwrap();
    [o.rgb[0], o.rgb[1], o.rgb[2], o.a]
}

#[test]
fn filtered_placed_instance_matches_pigment_stack_then_placement() {
    let src = source();
    let filtered = SceneContent::Filtered(Box::new(FilteredContent {
        content: SceneContent::Precomp(Box::new(src.clone())),
        rect: Rect { x: 0, y: 0, width: 6, height: 4 },
        filters: op(),
    }));
    let doc = scene("doc", 12, 4, None, vec![layer("SLYR-1", filtered, Affine2::translate(3.0, 0.0))]);
    let got = render(&doc);
    // oracle: the source alone, pigment's stack, then the placement at x + 3
    let plain = render(&src);
    let o = op();
    let stack = SmartFilterStack {
        entries: o.entries.iter().map(|e| StackEntry { filter: e.filter.clone(), enabled: e.enabled, opacity: e.opacity, mask: None }).collect(),
        stack_mask: None,
    };
    let out = stack.render(&RgbaPlane::new(Rect { x: 0, y: 0, width: 6, height: 4 }, plain.clone()).unwrap(), &StackLimits::default()).unwrap();
    let mut worst = 0.0f32;
    for y in 0..4i64 {
        for x in 0..12i64 {
            let want = if (3..9).contains(&x) {
                let s = out.data[(y * 6 + x - 3) as usize];
                over_clear(over_clear(s, x - 3, y), x, y)
            } else {
                [0.0; 4]
            };
            let g = got[(y * 12 + x) as usize];
            for k in 0..4 {
                worst = worst.max((g[k] - want[k]).abs());
            }
        }
    }
    println!("placed smart filter: max |cpu - pigment stack| = {worst:e} (tolerance 1e-6)");
    assert!(worst <= 1e-6, "{worst:e}");
    // the stack changed the instance (not a plain placement)
    let unfiltered = render(&scene("doc", 12, 4, None, vec![layer("SLYR-1", SceneContent::Precomp(Box::new(src)), Affine2::translate(3.0, 0.0))]));
    assert!(unfiltered.iter().zip(&got).filter(|(a, b)| a != b).count() >= 8);
    // an empty rect is a typed refusal, not a silent pass-through
    let empty = SceneContent::Filtered(Box::new(FilteredContent { content: SceneContent::Precomp(Box::new(source())), rect: Rect { x: 0, y: 0, width: 0, height: 4 }, filters: op() }));
    let renderer = Renderer::new(64).unwrap();
    let ctx = SceneContext {
        backend: &renderer,
        pixels: &NoPixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 26 },
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
        limits: SceneLimits::default(),
    };
    assert!(render_scene(&scene("doc", 12, 4, None, vec![layer("SLYR-1", empty, Affine2::IDENTITY)]), &ctx, &CancellationToken::default()).is_err());
}
