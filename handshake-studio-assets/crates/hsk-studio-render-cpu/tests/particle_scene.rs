//! Lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 1 on the CPU backend: a particle world layer (layer
//! emitter, sprite particles from a layer's pixels, turbulence) placed in a composition through
//! composite's `ParticleFrame::content` and `render_scene` draws exactly the particle frame (alpha
//! and straight colour within 1e-6 at every pixel), and two renders of the same frame are
//! bit-identical. The particle raster is CPU; the content uses the existing pigment routes (one
//! owned straight tile under a coverage mask). After Effects parity UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use hsk_studio_composite::particles::*;
use hsk_studio_composite::{
    SceneComposition, SceneContent, SceneContext, SceneLayer, SceneLimits, ScenePixels, StudioBlendMode, render_scene,
};
use hsk_studio_pigment::blend_math::BlendSpace;
use hsk_studio_pigment::render_plan::{ColourTag, Exactness, ExecuteRequest, TilePlanes};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_render_cpu::Renderer;

const W: u32 = 48;
const H: u32 = 40;
const TPF: u64 = TICKS_PER_SECOND / 24;

fn space() -> ColourTag {
    ColourTag { profile_sha256: [9; 32], blend_space: BlendSpace::Encoded }
}

struct NoPixels;
impl ScenePixels for NoPixels {
    fn tile(&self, _tile_id: &str) -> Option<TilePlanes<'_>> {
        None
    }
}

#[test]
fn particle_layer_renders_through_the_composition_scene_on_cpu() {
    // Layer emitter: an 8 x 4 alpha ramp (column 0 empty) at (12, 18); sprite: a 3 x 3 dot-like
    // sprite with a soft edge.
    let alpha: Vec<f32> = (0..32).map(|i| (i % 8) as f32 / 7.0).collect();
    let mut sprite = vec![[1.0, 0.9, 0.6, 0.5]; 9];
    sprite[4] = [1.0, 1.0, 1.0, 1.0];
    let mut w = ParticleWorld::point(11, 12.0, 18.0);
    w.emitter.shape = EmitterShape::Layer(LayerEmitter { width: 8, height: 4, alpha });
    w.birth_rate = Track::constant(80.0);
    w.life = 0.8;
    w.velocity = 25.0;
    w.velocity_random = 0.4;
    w.turbulence = Some(Turbulence { amplitude: 40.0, scale: 10.0, evolution: 1.0 });
    w.size = 5.0;
    w.opacity_over_life = LifeCurve { points: vec![(0.0, 1.0), (1.0, 0.2)] };
    w.kind = ParticleKind::Sprite(Sprite { width: 3, height: 3, rgba: sprite });
    let tick = 20 * TPF;
    let frame = render_particles(&w, W, H, tick, TPF, None, &ParticleLimits::default(), &CancellationToken::default()).unwrap();
    assert!(frame.drawn > 10, "alive: {}", frame.drawn);
    let content = frame.content("SLYR-particles", space(), 7).unwrap().expect("ink");
    let scene = SceneComposition {
        composition_id: "SCMP-particles".into(),
        width: W,
        height: H,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers: vec![SceneLayer {
            layer_id: "SLYR-particles".into(),
            content: SceneContent::Static(Box::new(content)),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2::IDENTITY,
            matte: None,
            clipped: false,
        }],
    };
    let renderer = Renderer::new(64).unwrap();
    let ctx = SceneContext {
        backend: &renderer,
        pixels: &NoPixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: space(),
        limits: SceneLimits::default(),
    };
    let r = render_scene(&scene, &ctx, &CancellationToken::default()).expect("scene renders");
    let mut inked = 0;
    for y in 0..H {
        for x in 0..W {
            let got = r.pixel(x, y).unwrap();
            let want = frame.pixel(x, y).unwrap();
            assert!((got[3] - want[3]).abs() < 1e-6, "({x}, {y}): {got:?} vs {want:?}");
            if want[3] > 0.0 {
                inked += 1;
                for (g, w) in got[..3].iter().zip(&want[..3]) {
                    assert!((g * got[3] - w).abs() < 1e-6, "({x}, {y}): {got:?} vs {want:?}");
                }
            }
        }
    }
    assert!(inked > 20, "inked pixels: {inked}");
    let again = render_scene(&scene, &ctx, &CancellationToken::default()).unwrap();
    assert_eq!(again.digest(), r.digest());
}
