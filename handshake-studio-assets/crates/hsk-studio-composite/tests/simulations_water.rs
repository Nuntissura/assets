//! Lane SMB-AE-WATER-SIMS-1: STU-CMP-075 water simulations Wave world (MT-34181), Caustics
//! (MT-34182), Bubbles (MT-34186) and Foam (MT-34183). STU-CMP-075a (deterministic start, identical
//! output for identical parameters and seed, scrubbing backwards never differs from playing forwards,
//! re-simulation from the start tick or a cached state), STU-FX-011 (explicit seed), STU-CMP-075b /
//! STU-FX-146 (authored `studio_declared` records, vendor names only as import keys). Numerics are
//! checked against exact constructions (hard sources, the discrete light cone, flat surfaces) and
//! closed forms (Snell's law, constant-speed rise, linear growth). After Effects parity UNVERIFIED (D5).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, TICKS_PER_SECOND};
use hsk_studio_composite::particles::{ParticleError, ParticleLimits};
use hsk_studio_composite::report;
use hsk_studio_composite::simulations::*;
use hsk_studio_observe::{Budget as ObserveBudget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};

const TPF: u64 = TICKS_PER_SECOND / 30;
const SEC: u64 = TICKS_PER_SECOND;

fn limits() -> ParticleLimits {
    ParticleLimits::default()
}

#[test]
fn water_identities_are_native_stable_and_distinct() {
    let vendor = ["CC ", "CS", "ADBE", "APC"];
    let names = ["Wave world", "Caustics", "Bubbles", "Foam"];
    let keys = ["APC Wave World", "APC Caustics", "CC Bubbles", "APC Foam"];
    assert_eq!(SimulationKind::EVERY.len(), SimulationKind::ALL.len() + SimulationKind::WATER.len());
    assert_eq!(&SimulationKind::EVERY[..SimulationKind::ALL.len()], &SimulationKind::ALL[..]);
    assert_eq!(&SimulationKind::EVERY[SimulationKind::ALL.len()..], &SimulationKind::WATER[..]);
    for (n, k) in SimulationKind::WATER.into_iter().enumerate() {
        assert!(k.id().starts_with("hsk.studio.simulation."), "{}", k.id());
        assert_eq!(SimulationKind::from_id(k.id()), Some(k));
        // The STU-CMP-075 capability name, verbatim; one table row each (no obsolete / new pair).
        assert_eq!(k.name(), names[n]);
        assert_eq!(k.import_keys(), &[keys[n]][..]);
        assert_eq!(SimulationKind::from_import_key(keys[n]), Some(k));
        assert_eq!(SimulationKind::from_id(keys[n]), None, "an import key is never an id");
        assert_eq!(SimulationKind::from_id(k.name()), None, "a name is not an id");
        assert!(SimulationKind::EVERY.iter().all(|o| o.name() != keys[n]), "an import key is never a Studio name");
        assert!(vendor.iter().all(|v| !k.name().starts_with(v) && !k.id().contains(v.trim())), "{}", k.name());
        assert_eq!(Simulation::new(k).kind(), k, "headless default record of {}", k.name());
        let rows = k.params().expect("authored rows");
        assert!(!rows.is_empty());
        for r in &rows {
            assert_eq!(r.provenance, "studio_declared", "{}", r.key);
            assert!(vendor.iter().all(|v| !r.key.starts_with(v.trim())), "{}", r.key);
        }
    }
    let mut ids: Vec<&str> = SimulationKind::EVERY.iter().map(|k| k.id()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), SimulationKind::EVERY.len(), "stable ids are distinct");
}

#[test]
fn water_rows_are_the_enforced_bounds() {
    for kind in SimulationKind::WATER {
        let rows = kind.params().expect("rows");
        let base = Simulation::new(kind);
        base.validate().expect("default record is valid");
        let mut keys: Vec<&str> = rows.iter().map(|r| r.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), rows.len(), "{kind:?}: row keys are distinct");
        for r in &rows {
            assert_eq!(r.scope, ParamScope::Record, "{}", r.key);
            assert!(r.hard_min <= r.hard_max, "{}", r.key);
            if let Some(s) = r.soft_min {
                assert!(s >= r.hard_min && r.soft_max.is_none_or(|m| s <= m), "{}", r.key);
            }
            if let Some(s) = r.soft_max {
                assert!(s <= r.hard_max, "{}", r.key);
            }
            assert_eq!(base.param(r.key), Some(r.default), "default of {}", r.key);
            for v in [r.hard_min, r.hard_max] {
                let mut s = base.clone();
                s.set_param(r.key, v).unwrap_or_else(|e| panic!("{} = {v}: {e}", r.key));
                assert_eq!(s.param(r.key), Some(v));
            }
            for v in [r.hard_max.next_up(), r.hard_min.next_down(), f64::NAN] {
                let mut s = base.clone();
                assert_eq!(s.set_param(r.key, v), Err(ParticleError::Parameter(r.key)), "{} = {v}", r.key);
                assert_eq!(s, base);
            }
        }
        let mut s = base.clone();
        assert_eq!(s.set_param(kind.import_keys()[0], 1.0), Err(ParticleError::Parameter("param_key")));
        assert_eq!(s, base);
    }
}

#[test]
fn water_dims_u32_max_is_refused_before_allocation() {
    let cancel = CancellationToken::default();
    for kind in SimulationKind::WATER {
        let s = Simulation::new(kind);
        assert_eq!(s.render(u32::MAX, u32::MAX, SEC, TPF, &limits(), &cancel).unwrap_err(), ParticleError::Raster, "{kind:?}");
        assert_eq!(s.render(0, 16, SEC, TPF, &limits(), &cancel).unwrap_err(), ParticleError::Raster, "{kind:?}");
        assert_eq!(s.render(1 << 16, 1 << 11, SEC, TPF, &limits(), &cancel).unwrap_err(), ParticleError::Raster, "{kind:?} over max_pixels");
    }
}

struct Collector(Vec<(DeliveryClass, Vec<u8>)>);

impl SinkPort for Collector {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.to_vec()));
        Ok(())
    }
}

#[test]
fn water_outcomes_go_through_composite_report_with_codes_only() {
    let cancel = CancellationToken::default();
    let stop = CancellationToken::default();
    stop.cancel();
    let mut sink = Collector(Vec::new());
    for kind in SimulationKind::WATER {
        let s = Simulation::new(kind);
        let ok = s.render(16, 16, SEC, TPF, &limits(), &cancel);
        let refused = s.render(0, 16, SEC, TPF, &limits(), &cancel);
        let canceled = s.render(16, 16, 2 * SEC, TPF, &limits(), &stop);
        let cases = [
            (outcome(&ok), Outcome::Success),
            (outcome(&refused), Outcome::Failure(FailureCode::Unavailable)),
            (outcome(&canceled), Outcome::Canceled),
        ];
        for (n, (got, want)) in cases.into_iter().enumerate() {
            assert_eq!(got, want, "{kind:?} case {n}");
            let actor = ActorContext::new("a", "p", "o", "op", "s", "sess").unwrap();
            let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
            let mut observe = Observe::new(42, 7, resource, actor, ObserveBudget::new(0, 0).unwrap());
            let receipt = report(got, 42, 7, &mut observe, &cancel, &mut sink).unwrap();
            assert_eq!(receipt.outcome, want, "{kind:?} case {n}");
        }
    }
    assert_eq!(sink.0.len(), 3 * SimulationKind::WATER.len());
    for (class, bytes) in &sink.0 {
        assert_eq!(*class, DeliveryClass::Terminal);
        assert!(bytes.starts_with(b"HSKO"));
        for text in [&b"wave"[..], b"Wave", b"caustic", b"bubble", b"foam", b"raster", b"cfl"] {
            assert!(!bytes.windows(text.len()).any(|w| w == text), "no record text on the wire");
        }
    }
}

// ---- Slice 1: Wave world (MT-34181) ----

fn pool(edge: EdgeMode) -> WaveWorld {
    WaveWorld {
        seed: 7,
        start_tick: 0,
        preroll: 0.0,
        grid: [48, 48],
        step_rate: 60,
        substeps: 2,
        wave_speed: 20.0,
        damping: 0.0,
        edge,
        initial_roughness: 0.0,
        height_range: 1.0,
        producers: Vec::new(),
    }
}

fn point(x: f64, y: f64, amplitude: f64, frequency: f64, phase: f64) -> WaveProducer {
    WaveProducer { shape: ProducerShape::Point, position: [x, y], end: [x, y], amplitude, frequency, phase }
}

fn energy(f: &HeightField) -> f64 {
    f.heights.iter().map(|h| f64::from(*h).powi(2)).sum()
}

#[test]
fn wave_world_still_water_hard_source_and_light_cone_are_exact() {
    use std::f64::consts::TAU;
    let cancel = CancellationToken::default();
    // Still water stays exactly at rest; its frame is the rest grey.
    let still = pool(EdgeMode::Reflect);
    let f = still.height_field_at(5 * SEC, &limits(), &cancel).unwrap();
    assert_eq!((f.width, f.height, f.range), (48, 48, 1.0));
    assert!(f.heights.iter().all(|h| *h == 0.0));
    let frame = still.render(16, 12, 5 * SEC, &limits(), &cancel).unwrap();
    assert!(frame.premultiplied.iter().all(|p| *p == [0.5, 0.5, 0.5, 1.0]));
    // A point producer drives its own cell: A sin(2 pi f t + phase), t = n / (step_rate substeps).
    let mut w = pool(EdgeMode::Reflect);
    w.grid = [33, 33];
    w.producers = vec![point(0.5, 0.5, 0.8, 1.5, 30.0)];
    for k in [1u64, 2, 7, 20] {
        let s = w.state_at(k * TPF, &limits(), &cancel).unwrap();
        assert_eq!(s.step(), 4 * k, "60 steps/s x 2 substeps = 4 steps per 1/30 s frame");
        let t = s.step() as f64 / 120.0;
        let want = (0.8 * ((TAU * 1.5) * t + 30f64.to_radians()).sin()) as f32;
        assert_eq!(s.height_field().at(16, 16), Some(want), "frame {k}");
    }
    // The discrete light cone: after n steps nothing farther than n cells (Manhattan) has moved.
    let s = w.state_at(2 * TPF, &limits(), &cancel).unwrap();
    let n = s.step();
    let f = s.height_field();
    for y in 0..33u32 {
        for x in 0..33u32 {
            if u64::from(x.abs_diff(16) + y.abs_diff(16)) > n {
                assert_eq!(f.at(x, y), Some(0.0), "({x},{y}) is outside the cone after {n} steps");
            }
        }
    }
    assert_ne!(f.at(17, 16), Some(0.0), "the wave has left the source");
    // Before the first step the surface is the initial surface.
    assert!(w.height_field_at(0, &limits(), &cancel).unwrap().heights.iter().all(|h| *h == 0.0));
}

#[test]
fn wave_world_propagates_at_the_wave_speed() {
    let cancel = CancellationToken::default();
    // A plane wave from a line producer on the left edge; the front is at c t cells.
    for (speed, front) in [(20.0, 40usize), (40.0, 80)] {
        let mut w = pool(EdgeMode::Reflect);
        w.grid = [128, 8];
        w.wave_speed = speed;
        w.producers = vec![WaveProducer { shape: ProducerShape::Line, position: [0.0, 0.0], end: [0.0, 1.0], amplitude: 1.0, frequency: 2.0, phase: 0.0 }];
        let f = w.height_field_at(2 * SEC, &limits(), &cancel).unwrap();
        let row: Vec<f32> = (0..128).map(|x| f.at(x, 3).unwrap().abs()).collect();
        // The producer covers the whole left column.
        assert!((0..8).all(|y| f.at(0, y) == f.at(0, 0)));
        let ahead = row[front + 8..].iter().copied().fold(0.0f32, f32::max);
        let behind = row[front - 10..front].iter().copied().fold(0.0f32, f32::max);
        assert!(ahead < 0.01, "c = {speed}: still water ahead of the front ({ahead})");
        assert!(behind > 0.5, "c = {speed}: waves behind the front ({behind})");
    }
}

#[test]
fn wave_world_edges_damping_and_cfl() {
    let cancel = CancellationToken::default();
    let rough = |edge, damping| WaveWorld { initial_roughness: 0.5, damping, ..pool(edge) };
    let e = |w: &WaveWorld, t| energy(&w.height_field_at(t, &limits(), &cancel).unwrap());
    let start = e(&rough(EdgeMode::Reflect, 0.0), 0);
    assert!(start > 0.0, "seeded initial roughness");
    let reflect = e(&rough(EdgeMode::Reflect, 0.0), 5 * SEC);
    let absorb = e(&rough(EdgeMode::Absorb, 0.0), 5 * SEC);
    let damped = e(&rough(EdgeMode::Reflect, 0.5), 5 * SEC);
    assert!(absorb < 0.25 * reflect, "absorbing edges let waves leave ({absorb} vs {reflect})");
    assert!(damped < 0.5 * reflect, "damping removes energy ({damped} vs {reflect})");
    assert!(reflect.is_finite() && reflect < start, "reflecting edges keep the surface bounded");
    // The seed changes the roughness (STU-FX-011).
    let mut other = rough(EdgeMode::Reflect, 0.0);
    other.seed = 8;
    assert_ne!(other.height_field_at(0, &limits(), &cancel).unwrap(), rough(EdgeMode::Reflect, 0.0).height_field_at(0, &limits(), &cancel).unwrap());
    // CFL: c dt above 1 / sqrt 2 is refused; more substeps make it stable.
    let mut fast = pool(EdgeMode::Reflect);
    fast.wave_speed = 85.0;
    assert!(fast.courant() > wave_world::CFL_LIMIT);
    assert_eq!(fast.validate(), Err(ParticleError::Parameter("cfl")));
    assert_eq!(fast.render(8, 8, SEC, &limits(), &cancel).unwrap_err(), ParticleError::Parameter("cfl"));
    fast.substeps = 4;
    assert!(fast.validate().is_ok());
    // Counts are whole; producers are bounded and their rows enforced.
    let mut s = Simulation::WaveWorld(pool(EdgeMode::Reflect));
    let before = s.clone();
    assert_eq!(s.set_param("grid_width", 10.5), Err(ParticleError::Parameter("grid_width")));
    assert_eq!(s.set_param("substeps", 1.5), Err(ParticleError::Parameter("substeps")));
    assert_eq!(s, before);
    let mut w = pool(EdgeMode::Reflect);
    w.producers = vec![point(0.5, 0.5, 1.0, 1.0, 0.0)];
    assert_eq!(w.set_producer_param(0, "frequency", 1000.5), Err(ParticleError::Parameter("frequency")));
    assert_eq!(w.set_producer_param(3, "frequency", 1.0), Err(ParticleError::Parameter("producers")));
    w.set_producer_param(0, "phase", 90.0).unwrap();
    assert_eq!(w.producers[0].phase, 90.0);
    for r in wave_world::WAVE_PRODUCER_ROWS {
        assert_eq!(r.provenance, "studio_declared");
        assert_eq!(WaveProducer::default().param(r.key), Some(r.default), "{}", r.key);
    }
    w.producers = vec![point(0.5, 0.5, 1.0, 1.0, 0.0); wave_world::MAX_WAVE_PRODUCERS + 1];
    assert_eq!(w.validate(), Err(ParticleError::Parameter("producers")));
    let mut w = pool(EdgeMode::Reflect);
    w.damping = f64::NAN;
    assert_eq!(w.validate(), Err(ParticleError::Parameter("damping")));
}

#[test]
fn wave_world_scrub_and_checkpoint_equal_forward_play() {
    let cancel = CancellationToken::default();
    let mut w = pool(EdgeMode::Absorb);
    w.initial_roughness = 0.2;
    w.damping = 0.3;
    w.producers = vec![point(0.3, 0.6, 0.7, 1.25, 10.0)];
    let (n, k) = (45u64, 30u64);
    let frame = |w: &WaveWorld, f: u64| w.render(32, 32, f * TPF, &limits(), &cancel).unwrap().digest();
    let direct = frame(&w, n);
    assert_eq!(frame(&w.clone(), n), direct, "same parameters and seed, same frame");
    let _ = frame(&w, n + k);
    assert_eq!(frame(&w, n), direct, "scrubbing back re-simulates from the start");
    let mut seeded = w.clone();
    seeded.seed = 99;
    assert_ne!(frame(&seeded, n), direct);
    // A checkpoint continues to the same state; a later checkpoint re-simulates; a foreign one is refused.
    let at_n = w.state_at(n * TPF, &limits(), &cancel).unwrap();
    let at_nk = w.state_at((n + k) * TPF, &limits(), &cancel).unwrap();
    assert_eq!(w.resume(&at_n, (n + k) * TPF, &limits(), &cancel).unwrap(), at_nk);
    assert_eq!(w.resume(&at_nk, n * TPF, &limits(), &cancel).unwrap(), at_n);
    assert_eq!(seeded.resume(&at_n, (n + k) * TPF, &limits(), &cancel).unwrap_err(), ParticleError::Parameter("checkpoint"));
    // The deterministic start tick: shifting it shifts the frames; before it, the initial surface.
    let mut late = w.clone();
    late.start_tick = 10 * TPF;
    assert_eq!(frame(&late, 10 + n), direct);
    assert_eq!(late.state_at(5 * TPF, &limits(), &cancel).unwrap().step(), 0);
    assert_eq!(late.height_field_at(5 * TPF, &limits(), &cancel).unwrap(), w.height_field_at(0, &limits(), &cancel).unwrap());
    // Preroll: the surface at the start tick is the surface `preroll` seconds in.
    let mut pre = w.clone();
    pre.preroll = 0.5;
    pre.start_tick = 15 * TPF;
    assert_eq!(frame(&pre, 15), frame(&w, 15));
}

#[test]
fn height_field_is_a_displacement_map_with_declared_range() {
    let cancel = CancellationToken::default();
    let f = HeightField { width: 2, height: 1, heights: vec![-2.0, 2.0], range: 2.0 };
    f.validate().unwrap();
    // Bilinear over cell centres, clamped to the edge; 0.5 grey at rest, 0 / 1 at -range / +range.
    assert_eq!(f.sample(0.5, 0.5), 0.0);
    assert_eq!(f.sample(0.375, 0.5), -1.0);
    assert_eq!(f.sample(-3.0, 9.0), -2.0);
    let frame = f.to_frame(4, 1, &limits(), &cancel).unwrap();
    let grey: Vec<f32> = frame.premultiplied.iter().map(|p| p[0]).collect();
    assert_eq!(grey, [0.0, 0.25, 0.75, 1.0]);
    assert!(frame.premultiplied.iter().all(|p| p[3] == 1.0 && p[0] == p[1] && p[1] == p[2]));
    let loud = HeightField { width: 2, height: 1, heights: vec![-5.0, 5.0], range: 1.0 };
    let grey: Vec<f32> = loud.to_frame(2, 1, &limits(), &cancel).unwrap().premultiplied.iter().map(|p| p[0]).collect();
    assert_eq!(grey, [0.0, 1.0], "outside the declared range clamps");
    // Caller fields are checked before their heights are read (hostile sizes, V4-01/02).
    let bad = |width, height, heights: Vec<f32>, range| HeightField { width, height, heights, range }.validate();
    let refused = Err(ParticleError::Parameter("height_field"));
    assert_eq!(bad(u32::MAX, u32::MAX, Vec::new(), 1.0), refused);
    assert_eq!(bad(1 << 13, 1 << 12, Vec::new(), 1.0), refused);
    assert_eq!(bad(0, 1, Vec::new(), 1.0), refused);
    assert_eq!(bad(2, 2, vec![0.0; 3], 1.0), refused);
    assert_eq!(bad(1, 1, vec![f32::NAN], 1.0), refused);
    assert_eq!(bad(1, 1, vec![0.0], 0.0), refused);
    assert_eq!(f.to_frame(u32::MAX, u32::MAX, &limits(), &cancel).unwrap_err(), ParticleError::Raster);
}

#[test]
fn wave_world_budget_is_refused_before_allocation() {
    let cancel = CancellationToken::default();
    let mut big = pool(EdgeMode::Reflect);
    big.grid = [1024, 1024];
    assert_eq!(big.state_at(100 * SEC, &limits(), &cancel).unwrap_err(), ParticleError::Work, "steps x cells over max_steps");
    assert_eq!(pool(EdgeMode::Reflect).state_at(u64::MAX, &limits(), &cancel).unwrap_err(), ParticleError::Window);
    let tight = ParticleLimits { max_window_steps: 100, ..ParticleLimits::default() };
    assert_eq!(pool(EdgeMode::Reflect).state_at(SEC, &tight, &cancel).unwrap_err(), ParticleError::Window);
    let w = pool(EdgeMode::Reflect);
    let at = w.state_at(SEC, &limits(), &cancel).unwrap();
    let work = ParticleLimits { max_steps: 48 * 48 * 10, ..ParticleLimits::default() };
    assert_eq!(w.resume(&at, 2 * SEC, &work, &cancel).unwrap_err(), ParticleError::Work, "the resumed steps are budgeted too");
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(w.state_at(SEC, &limits(), &stop).unwrap_err(), ParticleError::Canceled);
}

// ---- Slice 2: Caustics (MT-34182) ----

fn flat(width: u32, height: u32) -> HeightField {
    HeightField { width, height, heights: vec![0.0; (width * height) as usize], range: 1.0 }
}

fn lens(n: u32, sign: f32) -> HeightField {
    let mut heights = Vec::new();
    for y in 0..n {
        for x in 0..n {
            let c = f64::from(n) / 2.0;
            let (dx, dy) = ((f64::from(x) + 0.5 - c) / 3.0, (f64::from(y) + 0.5 - c) / 3.0);
            heights.push(sign * (-(dx * dx + dy * dy)).exp() as f32);
        }
    }
    HeightField { width: n, height: n, heights, range: 1.0 }
}

fn caustics(field: HeightField) -> Caustics {
    Caustics {
        surface: CausticSurface::Field(field),
        bottom: None,
        light_direction: 30.0,
        light_elevation: 55.0,
        refractive_index: 1.33,
        depth: 32.0,
        surface_height: 8.0,
        caustic_strength: 1.0,
        surface_opacity: 0.0,
        surface_colour: [0.75, 0.875, 1.0],
        bottom_colour: [0.375, 0.5625, 0.625],
    }
}

#[test]
fn caustics_flat_surface_is_exact_and_tilt_refracts_by_snell() {
    let cancel = CancellationToken::default();
    // A flat surface lights every pixel exactly once and refracts nothing, whatever the light.
    let c = caustics(flat(4, 4));
    assert!(c.light_map(24, 16, 0, &limits(), &cancel).unwrap().iter().all(|l| *l == 1.0));
    let frame = c.render(24, 16, 0, &limits(), &cancel).unwrap();
    assert!(frame.premultiplied.iter().all(|p| *p == [0.375, 0.5625, 0.625, 1.0]));
    // The surface tint goes over the lit bottom.
    let mut tinted = caustics(flat(4, 4));
    tinted.surface_opacity = 0.5;
    let t = [0.75f32 * 0.5, 0.875 * 0.5, 0.5, 0.5];
    let b = [0.375f32, 0.5625, 0.625, 1.0];
    let want: [f32; 4] = std::array::from_fn(|i| t[i] + b[i] * (1.0 - t[3]));
    assert!(tinted.render(8, 8, 0, &limits(), &cancel).unwrap().premultiplied.iter().all(|p| *p == want));
    // A bottom image comes through a flat surface unchanged.
    let mut img = Vec::new();
    for y in 0..16u32 {
        for x in 0..24u32 {
            let a = 0.5 + y as f32 / 64.0;
            img.push([x as f32 / 32.0 * a, 0.25 * a, (23 - x) as f32 / 32.0 * a, a]);
        }
    }
    let mut seen = caustics(flat(4, 4));
    seen.bottom = Some(WaterBottom { width: 24, height: 16, premultiplied: img.clone() });
    assert_eq!(seen.render(24, 16, 0, &limits(), &cancel).unwrap().premultiplied, img);
    // A tilted plane (slope 0.2 px/px across the interior) shifts the view by Snell's law:
    // (depth + h) tan(theta_i - theta_t), sin theta_t = sin theta_i / n, toward the rising side.
    let ramp = HeightField { width: 2, height: 2, heights: vec![0.0, 1.0, 0.0, 1.0], range: 1.0 };
    let mut tilt = caustics(ramp.clone());
    tilt.surface_height = 3.2;
    tilt.light_elevation = 90.0;
    let offs = tilt.view_offsets(32, 8, 0, &limits(), &cancel).unwrap();
    let light = tilt.light_map(32, 8, 0, &limits(), &cancel).unwrap();
    let ti = 0.2f64.atan();
    let tt = (ti.sin() / 1.33).asin();
    for x in 10..=21usize {
        let h = ramp.sample((x as f64 + 0.5) / 32.0, 3.5 / 8.0) * 3.2;
        let want = (32.0 + h) * (ti - tt).tan();
        let o = offs[3 * 32 + x];
        assert!((o[0] - want).abs() < 1e-9 && o[1].abs() < 1e-12, "x {x}: {o:?} vs {want}");
        assert!(o[0] > 0.0);
        if (12..=20).contains(&x) {
            assert!((0.97..=1.0).contains(&light[3 * 32 + x]), "a plane neither focuses nor loses light: {}", light[3 * 32 + x]);
        }
    }
}

#[test]
fn caustics_focus_conserve_and_consume_the_wave_world() {
    let cancel = CancellationToken::default();
    let mut c = caustics(lens(16, 1.0));
    c.light_elevation = 90.0;
    let light = c.light_map(48, 48, 0, &limits(), &cancel).unwrap();
    let (max, min, sum) = (light.iter().copied().fold(0.0, f64::max), light.iter().copied().fold(f64::MAX, f64::min), light.iter().sum::<f64>());
    assert!(max > 2.0, "a bump focuses light ({max})");
    assert!(min < 1.0, "and spreads it elsewhere ({min})");
    assert!(sum <= 48.0 * 48.0 + 1e-6 && sum > 0.99 * 48.0 * 48.0, "light is moved, not created ({sum})");
    let mut trough = c.clone();
    trough.surface = CausticSurface::Field(lens(16, -1.0));
    let tmax = trough.light_map(48, 48, 0, &limits(), &cancel).unwrap().into_iter().fold(0.0, f64::max);
    assert!(tmax < max, "a trough spreads more than it focuses ({tmax} vs {max})");
    // The caustic pattern shows on the bottom; strength 0 leaves the bottom unlit by caustics.
    c.bottom_colour = [0.25; 3];
    let r: Vec<f32> = c.render(48, 48, 0, &limits(), &cancel).unwrap().premultiplied.iter().map(|p| p[0]).collect();
    assert!(r.iter().copied().fold(0.0, f32::max) > 0.5 && r.iter().copied().fold(1.0, f32::min) < 0.25);
    let mut plain = c.clone();
    plain.caustic_strength = 0.0;
    assert!(plain.render(48, 48, 0, &limits(), &cancel).unwrap().premultiplied.iter().all(|p| *p == [0.25, 0.25, 0.25, 1.0]));
    // Wave world's height field is the surface: simulating at the tick equals handing the field in.
    let mut w = pool(EdgeMode::Reflect);
    w.grid = [24, 24];
    w.damping = 0.2;
    w.producers = vec![point(0.4, 0.5, 1.0, 1.5, 0.0)];
    let mut sim = caustics(flat(1, 1));
    sim.surface = CausticSurface::Wave(w.clone());
    let tick = 40 * TPF;
    let given = caustics(w.height_field_at(tick, &limits(), &cancel).unwrap());
    let direct = sim.render(32, 32, tick, &limits(), &cancel).unwrap();
    assert_eq!(direct, given.render(32, 32, 0, &limits(), &cancel).unwrap());
    assert_eq!(given.render(32, 32, 0, &limits(), &cancel).unwrap(), given.render(32, 32, 99 * TPF, &limits(), &cancel).unwrap(), "a caller field is the same at every tick");
    assert_ne!(direct, caustics(flat(1, 1)).render(32, 32, tick, &limits(), &cancel).unwrap(), "the waves refract the bottom");
    // STU-CMP-075a through the wave: scrubbing back, the same record and seed give the same frame.
    let _ = sim.render(32, 32, 70 * TPF, &limits(), &cancel).unwrap();
    assert_eq!(sim.render(32, 32, tick, &limits(), &cancel).unwrap().digest(), direct.digest());
    assert_eq!(sim.clone().render(32, 32, tick, &limits(), &cancel).unwrap().digest(), direct.digest());
}

#[test]
fn caustics_bounds_are_refused_before_allocation() {
    let cancel = CancellationToken::default();
    let with_bottom = |width, height, premultiplied: Vec<[f32; 4]>| {
        let mut c = caustics(flat(2, 2));
        c.bottom = Some(WaterBottom { width, height, premultiplied });
        c.render(8, 8, 0, &limits(), &cancel).map(|_| ())
    };
    let refused = Err(ParticleError::Parameter("bottom"));
    assert_eq!(with_bottom(u32::MAX, u32::MAX, Vec::new()), refused);
    assert_eq!(with_bottom(1 << 13, 1 << 12, Vec::new()), refused, "over MAX_SOURCE_PIXELS, checked before the pixels");
    assert_eq!(with_bottom(2, 2, vec![[0.0; 4]; 3]), refused);
    assert_eq!(with_bottom(1, 1, vec![[0.6, 0.0, 0.0, 0.5]]), Err(ParticleError::Parameter("bottom_pixels")), "colour above alpha");
    assert_eq!(with_bottom(1, 1, vec![[f32::NAN, 0.0, 0.0, 1.0]]), Err(ParticleError::Parameter("bottom_pixels")));
    let mut c = caustics(flat(2, 2));
    c.surface = CausticSurface::Field(HeightField { width: 2, height: 2, heights: vec![0.0], range: 1.0 });
    assert_eq!(c.render(8, 8, 0, &limits(), &cancel).unwrap_err(), ParticleError::Parameter("height_field"));
    let mut big = pool(EdgeMode::Reflect);
    big.grid = [1024, 1024];
    c.surface = CausticSurface::Wave(big);
    assert_eq!(c.render(8, 8, 100 * SEC, &limits(), &cancel).unwrap_err(), ParticleError::Work, "the surface's steps x cells");
    c.surface = CausticSurface::Wave(pool(EdgeMode::Reflect));
    assert_eq!(c.render(8, 8, u64::MAX, &limits(), &cancel).unwrap_err(), ParticleError::Window);
    let work = ParticleLimits { max_raster_work: 100, ..ParticleLimits::default() };
    assert_eq!(caustics(flat(2, 2)).render(16, 16, 0, &work, &cancel).unwrap_err(), ParticleError::Work);
    let mut cfl = pool(EdgeMode::Reflect);
    cfl.wave_speed = 100.0;
    c.surface = CausticSurface::Wave(cfl);
    assert_eq!(c.render(8, 8, 0, &limits(), &cancel).unwrap_err(), ParticleError::Parameter("cfl"));
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(caustics(flat(2, 2)).render(8, 8, 0, &limits(), &stop).unwrap_err(), ParticleError::Canceled);
}
