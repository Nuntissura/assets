//! Lane SMB-PS-ADJUST-STYLES slice 4: placed-asset instances render through the composition path
//! on the CPU backend. Two instances of one shared source are the scene layers composite's
//! `lower_extensions::placed::placed_scene_layer` builds (a precomposition of the source scene with
//! the instance matrix `placed_matrix`; that equality is composite `tests/placed_assets.rs`); the
//! source is a solid 2 x 2 background, so every expected pixel is a hand value. Editing the source
//! changes every instance; a non-integer instance transform resamples through pigment.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::lower_extensions::placed::placed_matrix;
use hsk_studio_composite::{
    SceneComposition, SceneContent, SceneContext, SceneLayer, SceneLimits, ScenePixels, render_scene,
};
use hsk_studio_pigment::blend_math::BlendSpace;
use hsk_studio_pigment::render_plan::{ColourTag, Exactness, ExecuteRequest, StudioBlendMode, TilePlanes};
use hsk_studio_pigment::transform_math::ResampleFilter;
use hsk_studio_render_cpu::Renderer;

struct NoPixels;
impl ScenePixels for NoPixels {
    fn tile(&self, _: &str) -> Option<TilePlanes<'_>> {
        None
    }
}

fn source(rgba: [f32; 4]) -> SceneComposition {
    SceneComposition {
        composition_id: "src1".into(),
        width: 2,
        height: 2,
        background: Some(rgba),
        filter: ResampleFilter::NearestNeighbour,
        layers: Vec::new(),
    }
}
fn instance(n: u32, src: &SceneComposition, transform: [f64; 6]) -> SceneLayer {
    SceneLayer {
        layer_id: format!("SLYR-{n}"),
        content: SceneContent::Precomp(Box::new(src.clone())),
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        matrix: placed_matrix(transform),
        matte: None,
        clipped: false,
    }
}
fn row(layers: Vec<SceneLayer>, filter: ResampleFilter) -> Vec<[f32; 4]> {
    let scene = SceneComposition {
        composition_id: "doc".into(),
        width: 7,
        height: 2,
        background: None,
        filter,
        layers,
    };
    let renderer = Renderer::new(64).unwrap();
    let ctx = SceneContext {
        backend: &renderer,
        pixels: &NoPixels,
        request: ExecuteRequest {
            expected_revision: 7,
            min_exactness: Exactness::Approximate,
            deadline: None,
            max_working_bytes: 1 << 26,
        },
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
        limits: SceneLimits::default(),
    };
    let out = render_scene(&scene, &ctx, &CancellationToken::default()).expect("renders");
    (0..7).map(|x| out.pixel(x, 0).unwrap()).collect()
}

#[test]
fn placed_instances_render_and_follow_their_source() {
    let shift = |x: f64| [1.0, 0.0, 0.0, 1.0, x, 0.0];
    let clear = [0.0; 4];
    // Instances at x = 1 and x = 4 (integer moves: exact) of the 2 x 2 source.
    for colour in [[0.2, 0.4, 0.6, 1.0], [0.9, 0.1, 0.1, 1.0]] {
        let src = source(colour);
        let got = row(vec![instance(1, &src, shift(1.0)), instance(2, &src, shift(4.0))], ResampleFilter::NearestNeighbour);
        assert_eq!(got, vec![clear, colour, colour, clear, colour, colour, clear], "source {colour:?}");
    }
    // An accumulated non-integer transform (scale 1.5 about the origin, then x + 1) is resampled:
    // nearest-neighbour keeps the solid colour inside the 3 x 3 footprint [1, 4) and nothing
    // outside it.
    let src = source([0.2, 0.4, 0.6, 1.0]);
    let got = row(vec![instance(1, &src, [1.5, 0.0, 0.0, 1.5, 1.0, 0.0])], ResampleFilter::NearestNeighbour);
    assert_eq!(got[0], clear);
    for px in &got[1..4] {
        assert_eq!(*px, [0.2, 0.4, 0.6, 1.0]);
    }
    assert_eq!(&got[4..], &[clear, clear, clear]);
}
