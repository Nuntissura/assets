//! A 3D run on the CPU backend (lane SMB-AE-3D): `three_d::expand_run` turns three planes under a
//! two-node camera into rank-matted projective layers; every pixel must match the independent
//! ray-cast oracle (`tests/support/three_d_scene.rs` in composite), two of the planes must cut each
//! other inside the frame, and the frame must be identical for every chunk size and thread count.
//! render-gpu's `tests/composition_three_d.rs` renders the same scene against the same oracle.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{RenderedScene, SceneComposition, SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_pigment::transform_math::ResampleFilter;
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

#[test]
fn parented_three_d_run_matches_the_ray_cast_oracle() {
    // Lane SMB-AE-3D-2 (STU-MOT-008): "tf" under a 3D parent (sheared by a non-uniform parent
    // scale) and "te" under a 2D parent; the oracle applies the parents with its own turns and
    // solves every camera ray against the sheared planes by Cramer's rule.
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::parented_scene(&pixels);
    let (want, crossing) = three_d::parented_oracle(&pixels);
    let b = scenes::BACKGROUND;
    let drawn = want.iter().filter(|p| (0..3).any(|c| (p[c] - b[c]).abs() > 1e-3)).count();
    assert!(drawn > 150, "the planes cover the frame: {drawn} pixels");
    let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
    let delta = scenes::max_delta(&base, &want);
    let unparented = render(&Renderer::new(64).unwrap(), &three_d::three_d_scene(&pixels), &pixels);
    println!("three_d parented: {drawn} drawn pixels, crossing {crossing:?}, cpu digest {} max |delta| vs oracle {delta:e}", scenes::hex(&base.digest()));
    assert!(delta <= 1e-6, "{delta}");
    assert_ne!(unparented.digest(), base.digest(), "the parents move the planes");
    let other = render(&Renderer::with_threads(7, 4).unwrap(), &scene, &pixels);
    assert_eq!(other.digest(), base.digest());
}

#[test]
fn masked_and_matted_three_d_run_matches_the_oracle() {
    // Lane SMB-AE-3D-2 (STU-CMP-010 steps 2 and 7): a layer-space mask on "tf" (before the
    // projection) and a screen-space alpha track matte on "te" (after it, 16-bit coverage).
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::masked_scene(&pixels);
    let want = three_d::masked_oracle(&pixels);
    let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
    let delta = scenes::max_delta(&base, &want);
    let plain = render(&Renderer::new(64).unwrap(), &three_d::three_d_scene(&pixels), &pixels);
    println!("three_d masked: cpu digest {} max |delta| vs oracle {delta:e}", scenes::hex(&base.digest()));
    assert!(delta <= 1e-6, "{delta}");
    assert_ne!(plain.digest(), base.digest(), "the mask and the matte change the frame");
    let other = render(&Renderer::with_threads(5, 3).unwrap(), &scene, &pixels);
    assert_eq!(other.digest(), base.digest());
}

#[test]
fn pinned_and_clipped_three_d_run_matches_the_oracle() {
    // Lane SMB-AE-3D-3: "tg" corner-pinned (the pinned content is the plane's texture: one resample
    // through the pin then the plane homography) and "te" clipped to "tf" (tf's alpha drawn alone
    // through the camera as te's screen-space matte, 16-bit coverage).
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::pinned_clipped_scene(&pixels);
    let (want, partial) = three_d::pinned_clipped_oracle(&pixels);
    assert!(partial > 0, "the clip matte has soft edges");
    let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
    let delta = scenes::max_delta(&base, &want);
    let plain = render(&Renderer::new(64).unwrap(), &three_d::three_d_scene(&pixels), &pixels);
    println!("three_d pinned + clipped: {partial} partial clip pixels, cpu digest {} max |delta| vs oracle {delta:e}", scenes::hex(&base.digest()));
    assert!(delta <= 1e-6, "{delta}");
    assert_ne!(plain.digest(), base.digest(), "the pin and the clip change the frame");
    let other = render(&Renderer::with_threads(7, 4).unwrap(), &scene, &pixels);
    assert_eq!(other.digest(), base.digest());
}

#[test]
fn three_d_aux_channels_match_the_ray_cast_oracle() {
    // Lane SMB-AE-3D-2 (STU-CMP-050): Z depth, camera-space normals, object id and coverage of the
    // run, alpha-tested on the rendered content (nearest texel), against the oracle's own ray cast.
    let pixels = scenes::Pixels::fixture();
    let ctx = SceneContext {
        backend: &Renderer::new(64).unwrap(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let (planes, cam) = three_d::aux_inputs(&pixels);
    let aux = hsk_studio_composite::composition::three_d_aux_channels(&planes, &[3, 2, 1], &cam, three_d::W, three_d::H, ResampleFilter::Bilinear, &ctx, &CancellationToken::default()).expect("channels");
    let want = three_d::aux_oracle(&pixels);
    let mut worst = [0.0f64; 2];
    for (i, w) in want.iter().enumerate() {
        assert_eq!((aux.object_id[i], aux.coverage[i]), (w.2, if w.2 > 0.0 { 1.0 } else { 0.0 }), "pixel {i}");
        if w.2 > 0.0 {
            worst[0] = worst[0].max((f64::from(aux.depth[i]) - w.0).abs() / w.0);
            for k in 0..3 {
                worst[1] = worst[1].max((f64::from(aux.normal[k][i]) - w.1[k]).abs());
            }
        } else {
            assert!(aux.depth[i] == f32::INFINITY && aux.normal.iter().all(|n| n[i] == 0.0));
        }
    }
    let hits = want.iter().filter(|w| w.2 > 0.0).count();
    println!("three_d aux: {hits} hit pixels, Z relative error {:e}, normal error {:e}", worst[0], worst[1]);
    assert!(hits > 150 && worst[0] <= 1e-7 && worst[1] <= 1e-6, "{worst:?}");
    // Depth matte (2D insertion between 3D objects) and fog from the Z plane, exact by definition.
    let slice = hsk_studio_composite::three_d::depth_matte(&aux.depth, 55.0, 62.0, 2.0).unwrap();
    for (c, z) in slice.iter().zip(&aux.depth) {
        let z = f64::from(*z);
        let want = if (55.0..=62.0).contains(&z) { 1.0 } else if z < 55.0 { (1.0 - (55.0 - z) / 2.0).max(0.0) } else { (1.0 - (z - 62.0) / 2.0).max(0.0) };
        assert!((f64::from(*c) - want).abs() < 1e-6, "{c} vs {want} at {z}");
    }
    let ids = hsk_studio_composite::three_d::id_matte(&aux.object_id, &[2]);
    assert_eq!(ids.iter().filter(|v| **v == 1.0).count(), aux.object_id.iter().filter(|v| **v == 2.0).count());
    // One EXR file carries every channel, readable back by name (independent minimal reader).
    let exr = aux.to_exr().expect("exr");
    let planes_back = three_d::read_exr_f32(&exr);
    for (name, plane) in aux.channels() {
        let back = planes_back.iter().find(|p| p.0 == name).unwrap_or_else(|| panic!("channel {name}"));
        assert!(back.1.iter().zip(plane).all(|(a, b)| a.to_bits() == b.to_bits()), "{name} round-trips bit for bit");
    }
    // For an external reader check (ffprobe / ffmpeg on the files, recorded in the lane notes): the
    // channel file and the normals as R, G, B, written under cargo's per-target scratch dir.
    let rgb = hsk_studio_composite::three_d::exr::encode_exr_f32(three_d::W, three_d::H, &[("R", &aux.normal[0]), ("G", &aux.normal[1]), ("B", &aux.normal[2])]).unwrap();
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::write(dir.join("three_d_aux.exr"), &exr).unwrap();
    std::fs::write(dir.join("three_d_aux_normals_rgb.exr"), &rgb).unwrap();
}

#[test]
fn equirectangular_plane_to_sphere_and_sphere_rotate_match_the_oracles() {
    // Lane SMB-AE-3D-2 (STU-CMP-055 "plane to sphere", "sphere rotate"; computed in the spherical
    // domain): the run's planes ray-cast into a 360 x 180 equirectangular frame, then the frame
    // turned about X, Y and Z with the longitude wrapping.
    let pixels = scenes::Pixels::fixture();
    let renderer = Renderer::new(64).unwrap();
    let ctx = SceneContext {
        backend: &renderer,
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let frame = hsk_studio_composite::composition::render_equirect(&three_d::immersive_scene(&pixels), ResampleFilter::Bilinear, &ctx, &CancellationToken::default()).expect("frame");
    let got = three_d::surface_pixels(&frame);
    let want = three_d::immersive_oracle(&pixels);
    let b = scenes::BACKGROUND;
    let drawn = want.iter().filter(|p| (0..3).any(|c| (p[c] - b[c]).abs() > 1e-3)).count();
    let delta = three_d::max_delta_px(&got, &want);
    println!("three_d equirect: {drawn} drawn pixels, max |delta| vs oracle {delta:e}");
    assert!(drawn > 200 && delta <= 1e-6, "{drawn} {delta}");
    // Sphere rotate on the rendered frame (straight in, straight out).
    let rgb: Vec<f32> = got.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let alpha: Vec<f32> = got.iter().map(|p| p[3]).collect();
    let rot = [10.0, 35.0, -5.0];
    let (r2, a2) = hsk_studio_composite::three_d::immersive::sphere_rotate(&rgb, &alpha, three_d::EW, three_d::EH, rot[0], rot[1], rot[2]).unwrap();
    let turned: Vec<[f32; 4]> = a2.iter().enumerate().map(|(i, a)| [r2[i * 3], r2[i * 3 + 1], r2[i * 3 + 2], *a]).collect();
    let rdelta = three_d::max_delta_px(&turned, &three_d::sphere_rotate_oracle(&got, three_d::EW, three_d::EH, rot));
    // A quarter turn about Y moves every row by exactly a quarter of the width, across the seam.
    let (q, qa) = hsk_studio_composite::three_d::immersive::sphere_rotate(&rgb, &alpha, three_d::EW, three_d::EH, 0.0, 90.0, 0.0).unwrap();
    let w = three_d::EW as usize;
    let mut seam = 0.0f32;
    for y in 0..three_d::EH as usize {
        for x in 0..w {
            let s = got[y * w + (x + w - w / 4) % w];
            let i = y * w + x;
            seam = seam.max((qa[i] - s[3]).abs()).max((0..3).map(|k| (q[i * 3 + k] * qa[i] - s[k] * s[3]).abs()).fold(0.0, f32::max));
        }
    }
    println!("three_d sphere rotate: max |delta| vs oracle {rdelta:e}, quarter-turn shift {seam:e}");
    assert!(rdelta <= 1e-6 && seam <= 1e-5, "{rdelta} {seam}");
}
