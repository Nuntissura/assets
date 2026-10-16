//! Lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 1: the particle world layer (STU-CMP-075 particle
//! world; STU-CMP-075a deterministic start, identical output for identical parameters and seed,
//! scrubbing backwards never differs from playing forwards; STU-FX-011 explicit seed). Physics is
//! checked against hand-derived values (exact exponential damping, the semi-implicit Euler bound for
//! gravity), life curves exactly, motion blur against the average of the shutter sample frames.
//! After Effects (CC Particle World / Particular) parity UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use hsk_studio_composite::particles::*;
use hsk_studio_folio::StudioCompositionMotionSettings;

const TPF: u64 = TICKS_PER_SECOND / 30;

fn busy_world(seed: u64) -> ParticleWorld {
    let mut w = ParticleWorld::point(seed, 32.0, 32.0);
    w.substeps = 4;
    w.birth_rate = Track { keys: vec![(0.0, 30.0), (1.0, 90.0), (2.0, 40.0)] };
    w.life = 1.5;
    w.life_random = 0.3;
    w.velocity = 40.0;
    w.velocity_random = 0.5;
    w.gravity = [0.0, 30.0];
    w.air_resistance = 0.5;
    w.turbulence = Some(Turbulence { amplitude: 60.0, scale: 16.0, evolution: 0.5 });
    w.size = 3.0;
    w.size_random = 0.5;
    w.size_over_life = LifeCurve { points: vec![(0.0, 1.0), (1.0, 0.25)] };
    w.opacity_over_life = LifeCurve { points: vec![(0.0, 1.0), (0.5, 0.8), (1.0, 0.0)] };
    w.colour_birth = [1.0, 0.8, 0.2];
    w.colour_death = [0.6, 0.1, 0.0];
    w.emitter.position[0] = Track { keys: vec![(0.0, 20.0), (2.0, 44.0)] };
    w
}

fn frame(w: &ParticleWorld, f: u64) -> ParticleFrame {
    render_particles(w, 64, 64, f * TPF, TPF, None, &ParticleLimits::default(), &CancellationToken::default()).expect("frame")
}

#[test]
fn particle_frames_are_random_access_and_identical_scrubbed_forward_or_backward() {
    let w = busy_world(7);
    let direct = frame(&w, 37);
    assert!(direct.drawn > 20, "alive at frame 37: {}", direct.drawn);
    // Forward play 0..=37, then a backwards scrub 60 -> 37: bit-identical frame and states.
    let mut forward = None;
    for f in 0..=37 {
        forward = Some(frame(&w, f));
    }
    let forward = forward.unwrap();
    let _ = frame(&w, 60);
    let back = frame(&w, 37);
    assert_eq!(direct.digest(), forward.digest());
    assert_eq!(direct.digest(), back.digest());
    assert_eq!(direct.premultiplied, back.premultiplied);
    let s1 = particles_at(&w, i128::from(37 * TPF), TPF, &ParticleLimits::default(), &CancellationToken::default()).unwrap();
    let _ = particles_at(&w, i128::from(80 * TPF), TPF, &ParticleLimits::default(), &CancellationToken::default()).unwrap();
    let s2 = particles_at(&w, i128::from(37 * TPF), TPF, &ParticleLimits::default(), &CancellationToken::default()).unwrap();
    assert_eq!(s1, s2);
    assert!(s1.windows(2).all(|p| p[0].index < p[1].index), "draw order = ascending birth index");
    // The same particle (same index) at a later frame is the same particle moved on.
    let later = particles_at(&w, i128::from(38 * TPF), TPF, &ParticleLimits::default(), &CancellationToken::default()).unwrap();
    let p = s1.iter().find(|p| p.age < 0.5).expect("a young particle");
    let q = later.iter().find(|q| q.index == p.index).expect("still alive one frame later");
    assert!((q.age - p.age - 1.0 / 30.0).abs() < 1e-9);
    // Another seed is another simulation; a frame before the start draws nothing.
    assert_ne!(frame(&busy_world(8), 37).digest(), direct.digest());
    let mut late = busy_world(7);
    late.start_tick = 10 * TPF;
    let before = frame(&late, 5);
    assert_eq!(before.drawn, 0);
    assert!(before.premultiplied.iter().all(|p| *p == [0.0; 4]));
    // The late world at frame 47 is the early world at frame 37 (the start is the only origin).
    assert_eq!(frame(&late, 47).digest(), direct.digest());
}

/// One particle: births at a constant 1 / s cross the first integer during the step holding t = 1 s,
/// so at t = 1.8 s with a 0.9 s life exactly particle 0 is alive.
fn lone(kind: ParticleKind) -> ParticleWorld {
    let mut w = ParticleWorld::point(3, 10.0, 20.0);
    w.substeps = 16;
    w.birth_rate = Track::constant(1.0);
    w.life = 0.9;
    w.direction = 90.0;
    w.spread = 0.0;
    w.velocity = 30.0;
    w.size = 4.0;
    w.opacity = 0.8;
    w.opacity_over_life = LifeCurve { points: vec![(0.0, 1.0), (1.0, 0.0)] };
    w.size_over_life = LifeCurve { points: vec![(0.0, 1.0), (1.0, 3.0)] };
    w.colour_birth = [1.0, 0.0, 0.0];
    w.colour_death = [0.0, 0.0, 1.0];
    w.kind = kind;
    w
}

#[test]
fn single_particle_follows_hand_computed_physics_life_curves_and_raster() {
    let t = 54 * TPF; // 1.8 s
    let dt = 1.0 / 30.0 / 16.0;
    let limits = ParticleLimits::default();
    let cancel = CancellationToken::default();
    // Air resistance only: v(T) = v0 exp(-k T) exactly (product of exp(-k h)), p within v0 dt of
    // the analytic p0 + v0 (1 - exp(-k T)) / k.
    let mut w = lone(ParticleKind::Dot);
    w.air_resistance = 1.5;
    let s = particles_at(&w, i128::from(t), TPF, &limits, &cancel).unwrap();
    assert_eq!(s.len(), 1);
    let p = s[0];
    assert_eq!(p.index, 0);
    assert!(p.age > 0.8 - 2.0 * dt && p.age < 0.8 + dt, "born in the step holding 1 s: age {}", p.age);
    let v = 30.0 * (-1.5 * p.age).exp();
    assert!((p.velocity[0] - v).abs() < 1e-9 * 30.0, "{} vs {v}", p.velocity[0]);
    let x = 10.0 + 30.0 * (1.0 - (-1.5 * p.age).exp()) / 1.5;
    assert!((p.position[0] - x).abs() <= 30.0 * dt, "{} vs {x}", p.position[0]);
    // Gravity only: semi-implicit Euler is within g T h_max of 0.5 g T^2.
    let mut g = lone(ParticleKind::Dot);
    g.gravity = [0.0, 40.0];
    let p = particles_at(&g, i128::from(t), TPF, &limits, &cancel).unwrap()[0];
    let y = 20.0 + 0.5 * 40.0 * p.age * p.age;
    assert!((p.position[1] - y).abs() <= 40.0 * p.age * dt + 1e-9, "{} vs {y}", p.position[1]);
    assert!((p.position[0] - (10.0 + 30.0 * p.age)).abs() < 1e-9);
    // Life curves and colour, exactly: a = age / life.
    let a = p.age / p.life;
    assert!((p.opacity - 0.8 * (1.0 - a)).abs() < 1e-12);
    assert!((p.size - 4.0 * (1.0 + 2.0 * a)).abs() < 1e-12);
    assert!((f64::from(p.colour[0]) - (1.0 - a)).abs() < 1e-6 && (f64::from(p.colour[2]) - a).abs() < 1e-6);
    // Raster: the pixel holding the centre is fully covered (d <= 0.71 < size / 2 + 0.5), so it is
    // the premultiplied colour at the particle opacity; the pixel 12 px left of the centre is empty.
    let f = render_particles(&g, 48, 64, t, TPF, None, &limits, &cancel).unwrap();
    let (cx, cy) = (p.position[0] as u32, p.position[1] as u32);
    let px = f.pixel(cx, cy).unwrap();
    let o = p.opacity as f32;
    assert!((px[3] - o).abs() < 1e-6, "{px:?}");
    for (got, c) in px.iter().zip(p.colour) {
        assert!((got - c * o).abs() < 1e-6, "{px:?}");
    }
    assert_eq!(f.pixel(cx - 12, cy).unwrap(), [0.0; 4]);
    // A sprite particle (a uniform green 2 x 2 sprite, tinted by the particle colour) at the centre.
    let mut sp = lone(ParticleKind::Sprite(Sprite { width: 2, height: 2, rgba: vec![[0.5, 1.0, 1.0, 1.0]; 4] }));
    sp.gravity = [0.0, 40.0];
    let f = render_particles(&sp, 48, 64, t, TPF, None, &limits, &cancel).unwrap();
    let px = f.pixel(cx, cy).unwrap();
    assert!((px[3] - o).abs() < 1e-6, "{px:?}");
    assert!((px[0] - 0.5 * p.colour[0] * o).abs() < 1e-6 && (px[2] - p.colour[2] * o).abs() < 1e-6, "{px:?}");
}

#[test]
fn hostile_parameters_and_particle_bombs_are_refused_before_allocation() {
    let limits = ParticleLimits::default();
    let cancel = CancellationToken::default();
    let run = |w: &ParticleWorld| render_particles(w, 64, 64, 30 * TPF, TPF, None, &limits, &cancel).map(|_| ());
    let code = |w: &ParticleWorld| run(w).unwrap_err().to_string();
    let base = ParticleWorld::point(1, 0.0, 0.0);
    let mut w = base.clone();
    w.birth_rate = Track::constant(f64::NAN);
    assert_eq!(code(&w), "particle_parameter:birth_rate");
    w.birth_rate = Track { keys: vec![(0.0, 1.0), (0.0, 2.0)] };
    assert_eq!(code(&w), "particle_parameter:birth_rate", "duplicate key time");
    let mut w = base.clone();
    w.gravity = [f64::INFINITY, 0.0];
    assert_eq!(code(&w), "particle_parameter:gravity");
    let mut w = base.clone();
    w.turbulence = Some(Turbulence { amplitude: 1.0, scale: f64::NAN, evolution: 0.0 });
    assert_eq!(code(&w), "particle_parameter:turbulence");
    let mut w = base.clone();
    w.opacity_over_life = LifeCurve { points: vec![(0.0, 1.0), (f64::NAN, 0.0)] };
    assert_eq!(code(&w), "particle_parameter:opacity_over_life");
    let mut w = base.clone();
    w.life = 0.0;
    assert_eq!(code(&w), "particle_parameter:life");
    let mut w = base.clone();
    w.kind = ParticleKind::Sprite(Sprite { width: 3, height: 3, rgba: vec![[1.0; 4]; 8] });
    assert_eq!(code(&w), "particle_sprite");
    let mut w = base.clone();
    w.emitter.shape = EmitterShape::Layer(LayerEmitter { width: 2, height: 2, alpha: vec![0.0; 4] });
    assert_eq!(code(&w), "particle_emitter_empty");
    // Count bomb: 1e9 / s for 600 s; the estimate refuses it before any particle is made.
    let mut w = base.clone();
    w.birth_rate = Track::constant(1.0e9);
    w.life = 600.0;
    assert_eq!(code(&w), "particle_count");
    // Window bomb: a 1-tick frame with a 600 s life spans far more grid steps than the bound.
    let mut w = base.clone();
    w.life = 600.0;
    let e = render_particles(&w, 8, 8, 1000 * TICKS_PER_SECOND, 1, None, &limits, &cancel).unwrap_err();
    assert_eq!(e, ParticleError::Window);
    // Raster work bomb: big dots against a small work budget.
    let mut w = base.clone();
    w.size = 4096.0;
    w.velocity = 0.0;
    let tight = ParticleLimits { max_raster_work: 10_000, ..ParticleLimits::default() };
    let e = render_particles(&w, 512, 512, 30 * TPF, TPF, None, &tight, &cancel).unwrap_err();
    assert_eq!(e, ParticleError::Work);
    // Raster, shutter and cancellation.
    assert_eq!(render_particles(&base, 0, 8, 0, TPF, None, &limits, &cancel).unwrap_err(), ParticleError::Raster);
    assert_eq!(render_particles(&base, 1 << 20, 1 << 20, 0, TPF, None, &limits, &cancel).unwrap_err(), ParticleError::Raster);
    let bad = ParticleShutter { angle: 180.0, phase: 0.0, samples: 0 };
    assert_eq!(render_particles(&base, 8, 8, 0, TPF, Some(&bad), &limits, &cancel).unwrap_err(), ParticleError::Parameter("shutter_samples"));
    let c = CancellationToken::default();
    c.cancel();
    assert_eq!(render_particles(&base, 8, 8, 30 * TPF, TPF, None, &limits, &c).unwrap_err(), ParticleError::Canceled);
    let ce: hsk_studio_composite::CompositeError = ParticleError::Count.into();
    assert_eq!(ce.code(), "composite_unavailable");
}

#[test]
fn motion_blur_through_the_composition_shutter_averages_the_sample_frames() {
    let mut w = lone(ParticleKind::Dot);
    w.velocity = 300.0;
    w.opacity_over_life = LifeCurve::default();
    w.size_over_life = LifeCurve::default();
    let settings = StudioCompositionMotionSettings {
        composition_id: "SCMP-main".into(),
        motion_blur: true,
        shutter_angle: 180.0,
        shutter_phase: -90.0,
        samples_per_frame: 4,
        adaptive_sample_limit: 128,
        motion_blur_layers: vec!["SLYR-particles".into()],
    };
    assert_eq!(ParticleShutter::from_settings(&settings, "SLYR-other"), None, "layer switch off");
    let shutter = ParticleShutter::from_settings(&settings, "SLYR-particles").expect("blurred layer");
    let t = 54 * TPF;
    let limits = ParticleLimits::default();
    let cancel = CancellationToken::default();
    let blurred = render_particles(&w, 400, 40, t, TPF, Some(&shutter), &limits, &cancel).unwrap();
    // Sample ticks: t + round(tpf (phase + angle (k + 0.5) / 4) / 360).
    let ticks = shutter.sample_ticks(t, TPF);
    let want: Vec<i128> = (0..4).map(|k| i128::from(t) + (TPF as f64 * (-90.0 + 180.0 * (k as f64 + 0.5) / 4.0) / 360.0).round() as i128).collect();
    assert_eq!(ticks, want);
    let mut avg = vec![[0.0f32; 4]; 400 * 40];
    for s in &ticks {
        let f = render_particles(&w, 400, 40, *s as u64, TPF, None, &limits, &cancel).unwrap();
        for (a, p) in avg.iter_mut().zip(&f.premultiplied) {
            for (ac, pc) in a.iter_mut().zip(p) {
                *ac += pc * 0.25;
            }
        }
    }
    assert_eq!(blurred.premultiplied, avg, "blur = average of the shutter sample frames");
    assert_eq!(blurred.drawn, 4);
    // The smear: 300 px/s over half a frame (5 px) widens the inked span of the row.
    let row = |f: &ParticleFrame, y: u32| (0..400).filter(|x| f.pixel(*x, y).unwrap()[3] > 0.0).count();
    let sharp = render_particles(&w, 400, 40, t, TPF, None, &limits, &cancel).unwrap();
    let y = 20;
    assert!(row(&blurred, y) >= row(&sharp, y) + 3, "blurred {} sharp {}", row(&blurred, y), row(&sharp, y));
}
