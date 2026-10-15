//! A 3D run on the CPU backend (lane SMB-AE-3D): `three_d::expand_run` turns three planes under a
//! two-node camera into rank-matted projective layers; every pixel must match the independent
//! ray-cast oracle (`tests/support/three_d_scene.rs` in composite), two of the planes must cut each
//! other inside the frame, and the frame must be identical for every chunk size and thread count.
//! render-gpu's `tests/composition_three_d.rs` renders the same scene against the same oracle.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{RenderedScene, SceneComposition, SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/composition_scenes.rs"]
mod scenes;
#[path = "../../hsk-studio-composite/tests/support/three_d_scene.rs"]
mod three_d;

fn render(renderer: &Renderer, scene: &SceneComposition, pixels: &scenes::Pixels) -> RenderedScene {
    let ctx = SceneContext {
        backend: renderer,
        pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    render_scene(scene, &ctx, &CancellationToken::default()).expect("frame renders")
}

#[test]
fn three_d_run_matches_the_ray_cast_oracle_on_every_chunking() {
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::three_d_scene(&pixels);
    let (want, crossing) = three_d::three_d_oracle(&pixels);
    assert!(crossing[0] > 0 && crossing[1] > 0, "the planes intersect inside the frame: {crossing:?}");
    let b = scenes::BACKGROUND;
    let drawn = want.iter().filter(|p| (0..3).any(|c| (p[c] - b[c]).abs() > 1e-3)).count();
    assert!(drawn > 150, "the planes cover the frame: {drawn} pixels");
    let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
    let delta = scenes::max_delta(&base, &want);
    println!("three_d: {} layers, {drawn} drawn pixels, crossing {crossing:?}, cpu digest {} max |delta| vs oracle {delta:e}", scene.layers.len(), scenes::hex(&base.digest()));
    assert!(delta <= 1e-6, "{delta}");
    for (chunk, threads) in [(5, 1), (7, 4), (64, 2)] {
        let other = render(&Renderer::with_threads(chunk, threads).unwrap(), &scene, &pixels);
        assert_eq!(other.digest(), base.digest(), "chunk {chunk} threads {threads}");
    }
}

#[test]
fn lit_three_d_run_matches_the_shading_oracle() {
    // Lane SMB-AE-3D slice 2: ambient, point (shadows), spot and parallel lights; the lit planes are
    // shaded per texel in composite, then resampled and depth-composited as above.
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::lit_scene(&pixels);
    let (want, shadowed) = three_d::lit_oracle(&pixels);
    assert!(shadowed > 0, "the point light casts a shadow onto a plane");
    let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
    let delta = scenes::max_delta(&base, &want);
    let unlit = render(&Renderer::new(64).unwrap(), &three_d::three_d_scene(&pixels), &pixels);
    println!("three_d lit: {shadowed} shadowed texels, cpu digest {} max |delta| vs oracle {delta:e}", scenes::hex(&base.digest()));
    assert!(delta <= 1e-6, "{delta}");
    assert_ne!(unlit.digest(), base.digest(), "lights change the frame");
    let other = render(&Renderer::with_threads(7, 4).unwrap(), &scene, &pixels);
    assert_eq!(other.digest(), base.digest());
}

#[test]
fn depth_of_field_matches_the_defocus_oracle() {
    // Lane SMB-AE-3D slice 3: a thin-lens camera focused near "tf"/"te"; every plane texel is
    // defocused by its own circle of confusion (fast-rectangle iris) before the resample.
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::dof_scene(&pixels);
    let (want, max_radius) = three_d::dof_oracle(&pixels);
    assert!(max_radius > 1.0, "the far plane is out of focus: {max_radius}");
    let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
    let delta = scenes::max_delta(&base, &want);
    let sharp = render(&Renderer::new(64).unwrap(), &three_d::three_d_scene(&pixels), &pixels);
    println!("three_d dof: max layer radius {max_radius:.3}, cpu digest {} max |delta| vs oracle {delta:e}", scenes::hex(&base.digest()));
    assert!(delta <= 1e-6, "{delta}");
    assert_ne!(sharp.digest(), base.digest(), "the lens changes the frame");
    let other = render(&Renderer::with_threads(5, 3).unwrap(), &scene, &pixels);
    assert_eq!(other.digest(), base.digest());
}
