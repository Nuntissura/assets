//! Lane SMB-AE-PARTICLES-2: STU-CMP-075 simulation types Rain (MT-34184), Snow (MT-34185), Star
//! burst (MT-34189) and Particle systems (classic) (MT-34177). STU-CMP-075a (deterministic start,
//! identical output for identical parameters and seed, scrubbing backwards never differs from
//! playing forwards), STU-FX-011 (explicit seed), STU-CMP-075b / STU-FX-146 (authored
//! `studio_declared` records, vendor names only as import keys). Motion is checked against
//! hand-derived closed forms (constant terminal velocity with wind and depth layers, the snow sway,
//! the star projection, exact linear-drag ballistics). After Effects parity UNVERIFIED (D5).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, TICKS_PER_SECOND};
use hsk_studio_composite::particles::{ParticleError, ParticleFrame, ParticleLimits, ParticleWorld, Track};
use hsk_studio_composite::report;
use hsk_studio_composite::simulations::precipitation::{RAIN_CORE_ROWS, SNOW_CORE_ROWS};
use hsk_studio_composite::simulations::*;
use hsk_studio_observe::{Budget as ObserveBudget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};
use std::collections::HashMap;

const TPF: u64 = TICKS_PER_SECOND / 30;
const DT: f64 = 1.0 / 30.0;

fn limits() -> ParticleLimits {
    ParticleLimits::default()
}

fn rain() -> Rain {
    let mut r = Rain::default();
    let p = &mut r.precipitation;
    p.seed = 5;
    p.region = [0.0, 0.0, 96.0, 64.0];
    p.rate = 30.0;
    p.speed = 120.0;
    p.speed_random = 0.0;
    p.wind_direction = 90.0;
    p.wind_strength = 40.0;
    p.layers = vec![DepthLayer { density: 1.0, scale: 0.5, speed: 0.5, opacity: 0.4 }, DepthLayer::default()];
    p.colour = [0.7, 0.8, 1.0];
    p.opacity = 1.0;
    r.streak_length = 6.0;
    r.streak_width = 3.0;
    r
}

fn snow() -> Snow {
    let mut s = Snow::default();
    let p = &mut s.precipitation;
    p.seed = 6;
    p.region = [0.0, 0.0, 64.0, 48.0];
    p.rate = 20.0;
    p.speed = 30.0;
    p.speed_random = 0.25;
    p.opacity = 1.0;
    s.flake_size = 4.0;
    s.flake_size_random = 0.0;
    s.drift_amplitude = 5.0;
    s.drift_frequency = 0.5;
    s
}

fn burst() -> StarBurst {
    StarBurst {
        seed: 3,
        centre: [32.0, 24.0],
        star_count: 200,
        field_radius: 20.0,
        near: 0.5,
        far: 8.0,
        speed: 2.0,
        speed_random: 0.2,
        star_size: 1.0,
        size_random: 0.0,
        depth_fade: 0.5,
        ..StarBurst::default()
    }
}

fn classic() -> ParticleSystemsClassic {
    ParticleSystemsClassic {
        seed: 9,
        particle_type: ClassicParticleType::Dot,
        producer: [20.0, 30.0],
        birth_rate: 10.0,
        longevity: 0.9,
        direction: 90.0,
        spread: 0.0,
        velocity: 30.0,
        velocity_random: 0.0,
        gravity: 40.0,
        gravity_direction: 180.0,
        resistance: 1.5,
        birth_size: 4.0,
        death_size: 12.0,
        max_opacity: 0.8,
        birth_colour: [1.0, 0.0, 0.0],
        death_colour: [0.0, 0.0, 1.0],
        ..ParticleSystemsClassic::default()
    }
}

fn world() -> ParticleWorld {
    let mut w = ParticleWorld::point(4, 32.0, 24.0);
    w.birth_rate = Track::constant(40.0);
    w.gravity = [0.0, 20.0];
    w
}

fn frame(s: &Simulation, f: u64) -> ParticleFrame {
    s.render(64, 48, f * TPF, TPF, &limits(), &CancellationToken::default()).expect("frame")
}

fn by_index(v: &[SimParticle]) -> HashMap<(u32, u64), SimParticle> {
    v.iter().map(|p| ((p.layer, p.index), *p)).collect()
}

fn segment_distance(c: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((c[0] - a[0]) * dx + (c[1] - a[1]) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (c[0] - (a[0] + t * dx)).hypot(c[1] - (a[1] + t * dy))
}

#[test]
fn simulation_identities_are_native_stable_and_distinct() {
    let vendor = ["CC ", "CS", "ADBE", "APC"];
    let mut ids = Vec::new();
    for k in SimulationKind::ALL {
        assert!(k.id().starts_with("hsk.studio.simulation."), "{}", k.id());
        assert_eq!(SimulationKind::from_id(k.id()), Some(k));
        assert_eq!(Simulation::new(k).kind(), k, "headless default record of {}", k.name());
        assert!(vendor.iter().all(|v| !k.name().starts_with(v) && !k.id().contains(v.trim())), "{}", k.name());
        for key in k.import_keys() {
            assert_eq!(SimulationKind::from_import_key(key), Some(k), "{key}");
            assert_eq!(SimulationKind::from_id(key), None, "an import key is never an id: {key}");
            assert!(SimulationKind::ALL.iter().all(|o| o.name() != *key), "an import key is never a Studio name: {key}");
        }
        if let Some(rows) = k.params() {
            assert!(!rows.is_empty());
            for r in &rows {
                assert_eq!(r.provenance, "studio_declared", "{}", r.key);
                assert!(vendor.iter().all(|v| !r.key.starts_with(v.trim())), "{}", r.key);
            }
        }
        ids.push(k.id());
    }
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 5, "stable ids are distinct");
    // The STU-CMP-075 capability names, verbatim.
    let names: Vec<&str> = SimulationKind::ALL.iter().map(|k| k.name()).collect();
    assert_eq!(names, ["Particle world", "Particle systems (classic)", "Rain", "Snow", "Star burst"]);
    assert_eq!(SimulationKind::from_id("Rain"), None, "a name is not an id");
    assert_eq!(SimulationKind::from_import_key("CC Particle Systems II"), None, "unmapped table row");
}

#[test]
fn classic_particle_systems_are_their_own_identity_and_record() {
    let (c, w) = (SimulationKind::ParticleSystemsClassic, SimulationKind::ParticleWorld);
    assert_ne!(c.id(), w.id());
    assert_ne!(c.name(), w.name());
    for key in c.import_keys() {
        assert_eq!(SimulationKind::from_import_key(key), Some(c));
    }
    for key in w.import_keys() {
        assert_eq!(SimulationKind::from_import_key(key), Some(w));
    }
    assert!(c.import_keys().iter().all(|k| !w.import_keys().contains(k)));
    assert_eq!(SimulationKind::from_id(c.id()), Some(c));
    assert_eq!(SimulationKind::from_id(w.id()), Some(w));
    // Its own record: authored rows (the world has none; its record is the typed ParticleWorld),
    // classic-only fields, and a default record that is a Classic, never a World.
    assert_eq!(w.params(), None);
    let rows = c.params().expect("classic rows");
    for own in ["producer_radius_x", "longevity", "gravity_direction", "birth_size", "death_size", "max_opacity"] {
        assert!(rows.iter().any(|r| r.key == own), "{own}");
    }
    let sim = Simulation::new(c);
    assert!(matches!(sim, Simulation::Classic(_)));
    assert_ne!(sim.kind(), w);
    assert_ne!(Simulation::new(w).kind(), c);
    assert_eq!(Simulation::new(w).param("longevity"), None, "the world does not answer classic rows");
    assert!(Simulation::new(w).clone().set_param("birth_rate", 1.0).is_err());
}

#[test]
fn authored_parameter_rows_are_the_enforced_bounds() {
    for kind in SimulationKind::ALL {
        let Some(rows) = kind.params() else { continue };
        let base = Simulation::new(kind);
        base.validate().expect("default record is valid");
        for r in &rows {
            assert!(r.hard_min <= r.hard_max, "{}", r.key);
            if let Some(s) = r.soft_min {
                assert!(s >= r.hard_min && r.soft_max.is_none_or(|m| s <= m), "{}", r.key);
            }
            if let Some(s) = r.soft_max {
                assert!(s <= r.hard_max, "{}", r.key);
            }
            if r.scope != ParamScope::Record {
                continue;
            }
            assert_eq!(base.param(r.key), Some(r.default), "default of {}", r.key);
            // Inside the hard bounds: accepted; outside, NaN: refused naming the key, record unchanged.
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
        // An unknown bound stays unknown: some rows declare no soft bound at all.
        assert!(rows.iter().any(|r| r.soft_min.is_none() && r.soft_max.is_none()));
        let mut s = base.clone();
        assert_eq!(s.set_param("CC Rain", 1.0), Err(ParticleError::Parameter("param_key")));
    }
    // validate() walks the same rows when a field is written directly.
    let mut r = rain();
    r.streak_width = 0.0;
    assert_eq!(r.validate(), Err(ParticleError::Parameter("streak_width")));
    let mut r = rain();
    r.precipitation.layers[1].speed = 0.0;
    assert_eq!(r.validate(), Err(ParticleError::Parameter("layer.speed")));
    let mut r = rain();
    assert_eq!(r.precipitation.set_layer_param(0, "layer.opacity", 1.5), Err(ParticleError::Parameter("layer.opacity")));
    r.precipitation.set_layer_param(1, "layer.opacity", 0.25).unwrap();
    assert_eq!(r.precipitation.layers[1].opacity, 0.25);
    assert_eq!(r.precipitation.set_layer_param(7, "layer.opacity", 0.5), Err(ParticleError::Parameter("layers")));
    let mut s = burst();
    s.near = 9.0;
    assert_eq!(s.validate(), Err(ParticleError::Parameter("near")), "near must be in front of far");
    let mut s = Simulation::StarBurst(burst());
    assert_eq!(s.set_param("star_count", 2.5), Err(ParticleError::Parameter("star_count")));
    let mut c = classic();
    c.resistance = f64::INFINITY;
    assert_eq!(c.validate(), Err(ParticleError::Parameter("resistance")));
}

#[test]
fn rain_falls_with_wind_speed_and_depth_layering() {
    let r = rain();
    let cancel = CancellationToken::default();
    let a = r.states_at(20 * TPF, &limits(), &cancel).unwrap();
    let b = r.states_at(21 * TPF, &limits(), &cancel).unwrap();
    assert!(a.iter().any(|p| p.layer == 0) && a.iter().any(|p| p.layer == 1), "both depth layers fall");
    assert!(a.windows(2).all(|w| (w[0].layer, w[0].index) < (w[1].layer, w[1].index)), "draw order: far layer first, then slot index");
    let later = by_index(&b);
    for p in &a {
        // Terminal velocity: layer speed x ((0, speed) + wind 40 px/s blowing right).
        let k = if p.layer == 0 { 0.5 } else { 1.0 };
        assert!((p.velocity[0] - 40.0 * k).abs() < 1e-9 && (p.velocity[1] - 120.0 * k).abs() < 1e-9, "{p:?}");
        // Depth layer scale and opacity.
        assert_eq!((p.size, p.length, p.opacity), (3.0 * k, 6.0 * k, if p.layer == 0 { 0.4 } else { 1.0 }));
        // Life: the travel from the spawn line `margin` above the region to `margin` below it.
        let margin = 9.0 * k;
        assert!((p.life - (64.0 + 2.0 * margin) / p.velocity[1]).abs() < 1e-9);
        assert!((-margin - 1e-9..64.0 + margin).contains(&p.position[1]), "{p:?}");
        assert!((p.age - (p.position[1] + margin) / p.velocity[1]).abs() < 1e-9, "fell from the spawn line");
        if let Some(q) = later.get(&(p.layer, p.index)) {
            assert!((q.position[0] - p.position[0] - p.velocity[0] * DT).abs() < 1e-9);
            assert!((q.position[1] - p.position[1] - p.velocity[1] * DT).abs() < 1e-9);
        }
    }
    // Raster: a near-layer drop well inside the frame inks its head pixel fully (3 px wide streak:
    // the pixel holding the head is within 0.71 px of the segment); pixels farther than every
    // streak's half width + 1 px are empty.
    let f = r.render(96, 64, 20 * TPF, &limits(), &cancel).unwrap();
    assert_eq!(f.drawn, a.len() as u64);
    let p = a.iter().find(|p| p.layer == 1 && p.position[0] > 3.0 && p.position[0] < 93.0 && p.position[1] > 3.0 && p.position[1] < 61.0).expect("a visible near drop");
    let px = f.pixel(p.position[0] as u32, p.position[1] as u32).unwrap();
    for (got, want) in px.iter().zip([0.7, 0.8, 1.0, 1.0]) {
        assert!((got - want).abs() < 1e-6, "{px:?}");
    }
    let mut empty = 0;
    for y in 0..64u32 {
        for x in 0..96u32 {
            let c = [f64::from(x) + 0.5, f64::from(y) + 0.5];
            let far = a.iter().all(|p| {
                let speed = p.velocity[0].hypot(p.velocity[1]);
                let tail = [p.position[0] - p.velocity[0] / speed * p.length, p.position[1] - p.velocity[1] / speed * p.length];
                segment_distance(c, p.position, tail) > p.size / 2.0 + 1.0
            });
            if far {
                empty += 1;
                assert_eq!(f.pixel(x, y).unwrap(), [0.0; 4], "({x}, {y})");
            }
        }
    }
    assert!(empty > 1000, "{empty}");
    // A wind that lifts the rain is refused (typed), not drawn upwards.
    let mut up = rain();
    up.precipitation.wind_direction = 0.0;
    up.precipitation.wind_strength = 200.0;
    assert_eq!(up.render(8, 8, 0, &limits(), &cancel).unwrap_err(), ParticleError::Parameter("wind"));
}

#[test]
fn snow_shares_the_rain_axes_and_sways_its_flakes() {
    // The same axes as rain: identical core keys, units, scopes and hard bounds.
    assert_eq!(RAIN_CORE_ROWS.len(), SNOW_CORE_ROWS.len());
    for (r, s) in RAIN_CORE_ROWS.iter().zip(&SNOW_CORE_ROWS) {
        assert_eq!((r.key, r.unit, r.scope, r.hard_min, r.hard_max), (s.key, s.unit, s.scope, s.hard_min, s.hard_max));
    }
    let snow_keys: Vec<&str> = Snow::rows().iter().map(|r| r.key).collect();
    assert!(Rain::rows().iter().filter(|r| !r.key.starts_with("streak")).all(|r| snow_keys.contains(&r.key)));
    assert_ne!(SimulationKind::Snow.id(), SimulationKind::Rain.id());
    let cancel = CancellationToken::default();
    // Sway: with frequency 0 a flake keeps the constant offset A sin(phi); with frequency f the same
    // flake (same slot, same y) is displaced by A (sin(2 pi f_i age + phi) - sin(phi)), at most 2 A.
    let swaying = snow();
    let mut still = snow();
    still.drift_frequency = 0.0;
    let s = swaying.states_at(30 * TPF, &limits(), &cancel).unwrap();
    let z = still.states_at(30 * TPF, &limits(), &cancel).unwrap();
    assert_eq!(s.len(), z.len());
    assert!(s.len() > 5, "{}", s.len());
    let mut moved = 0;
    for (p, q) in s.iter().zip(&z) {
        assert_eq!((p.index, p.position[1], p.velocity[1]), (q.index, q.position[1], q.velocity[1]));
        let dx = (p.position[0] - q.position[0]).abs();
        assert!(dx <= 10.0 + 1e-9, "{dx}");
        moved += usize::from(dx > 0.1);
        assert!(q.velocity[0].abs() < 1e-12, "no wind, no sway: no drift velocity");
        assert_eq!((p.size, p.length), (4.0, 0.0));
    }
    assert!(moved > 0);
    // Falling: y advances by the fall speed per frame, the speed's random share stays in range.
    let next = by_index(&swaying.states_at(31 * TPF, &limits(), &cancel).unwrap());
    for p in &s {
        assert!((22.5 - 1e-9..=37.5 + 1e-9).contains(&p.velocity[1]), "{p:?}");
        if let Some(q) = next.get(&(p.layer, p.index)) {
            assert!((q.position[1] - p.position[1] - p.velocity[1] * DT).abs() < 1e-9);
        }
    }
    // Wind and depth layers act as for rain: a half-speed layer drifts and falls at half speed.
    let mut windy = still.clone();
    windy.precipitation.wind_strength = 10.0;
    windy.precipitation.layers = vec![DepthLayer { speed: 0.5, ..DepthLayer::default() }];
    for p in windy.states_at(30 * TPF, &limits(), &cancel).unwrap() {
        assert!((p.velocity[0] - 5.0).abs() < 1e-9);
        assert!((11.25 - 1e-9..=18.75 + 1e-9).contains(&p.velocity[1]), "{p:?}");
    }
    // Flakes are dots: a flake well inside the frame inks its centre pixel fully (4 px dot).
    let f = swaying.render(64, 48, 30 * TPF, &limits(), &cancel).unwrap();
    let p = s.iter().find(|p| (3.0..61.0).contains(&p.position[0]) && (3.0..45.0).contains(&p.position[1])).expect("a visible flake");
    let px = f.pixel(p.position[0] as u32, p.position[1] as u32).unwrap();
    assert!(px.iter().all(|c| (c - 1.0).abs() < 1e-6), "{px:?}");
}

#[test]
fn star_burst_moves_radially_with_depth_and_perspective() {
    let sb = burst();
    let cancel = CancellationToken::default();
    let a = sb.states_at(15 * TPF, &limits(), &cancel).unwrap();
    let b = by_index(&sb.states_at(16 * TPF, &limits(), &cancel).unwrap());
    assert_eq!(a.len(), 200, "a starfield keeps every star");
    assert!(a.windows(2).all(|w| w[0].depth >= w[1].depth), "farthest first");
    let span = 7.5;
    let mut checked = 0;
    for p in &a {
        let off = [p.position[0] - 32.0, p.position[1] - 24.0];
        assert!((0.5..8.0).contains(&p.depth), "{p:?}");
        assert!((p.size * p.depth - 1.0).abs() < 1e-12, "perspective size");
        assert!((p.opacity - (1.0 - 0.5 * (p.depth - 0.5) / span)).abs() < 1e-12, "depth fade");
        assert!(off[0].hypot(off[1]) * p.depth <= 20.0 + 1e-9, "inside the field disc");
        let q = b[&(0, p.index)];
        if q.depth < p.depth {
            // Same star one frame later (no recycle): the same world offset, so the same ray out of
            // the centre, farther out.
            let o2 = [q.position[0] - 32.0, q.position[1] - 24.0];
            assert!((o2[0] * q.depth - off[0] * p.depth).abs() < 1e-9 && (o2[1] * q.depth - off[1] * p.depth).abs() < 1e-9);
            assert!(o2[0].hypot(o2[1]) >= off[0].hypot(off[1]));
            // Depth falls by the star's speed (2 +- 20 %) per second.
            let dz = (p.depth - q.depth) / DT;
            assert!((1.6 - 1e-9..=2.4 + 1e-9).contains(&dz), "{dz}");
            checked += 1;
        }
    }
    assert!(checked > 150, "{checked}");
    // Impact: every star starts at the far plane (at the centre), bursts out once and is gone after
    // (far - near) / slowest speed = 7.5 / 1.6 s.
    let mut imp = burst();
    imp.mode = BurstMode::Impact;
    let s0 = imp.states_at(0, &limits(), &cancel).unwrap();
    assert_eq!(s0.len(), 200);
    let radius = |v: &[SimParticle]| v.iter().map(|p| (p.position[0] - 32.0).hypot(p.position[1] - 24.0)).fold(0.0, f64::max);
    assert!(radius(&s0) <= 20.0 / 8.0 + 1e-9);
    let s1 = imp.states_at(60 * TPF, &limits(), &cancel).unwrap();
    assert!(radius(&s1) > radius(&s0));
    assert_eq!(imp.states_at(141 * TPF, &limits(), &cancel).unwrap().len(), 0);
    assert_eq!(imp.render(64, 48, 141 * TPF, &limits(), &cancel).unwrap().drawn, 0);
    assert_eq!(sb.render(64, 48, 141 * TPF, &limits(), &cancel).unwrap().drawn, 200);
}

#[test]
fn classic_particles_follow_exact_drag_ballistics_and_their_type() {
    let cancel = CancellationToken::default();
    let g = 40.0;
    for k in [1.5, 0.0] {
        let mut c = classic();
        c.resistance = k;
        let s = c.states_at(54 * TPF, &limits(), &cancel).unwrap();
        assert!(s.len() >= 5, "{}", s.len());
        for p in &s {
            let a = p.age;
            assert!((0.0..0.9).contains(&a));
            let (v, x, y) = if k > 0.0 {
                let e = (-k * a).exp();
                ([30.0 * e, g * (1.0 - e) / k], 20.0 + 30.0 * (1.0 - e) / k, 30.0 + g * (a - (1.0 - e) / k) / k)
            } else {
                ([30.0, g * a], 20.0 + 30.0 * a, 30.0 + 0.5 * g * a * a)
            };
            assert!((p.velocity[0] - v[0]).abs() < 1e-9 && (p.velocity[1] - v[1]).abs() < 1e-9, "k {k}: {p:?}");
            assert!((p.position[0] - x).abs() < 1e-9 && (p.position[1] - y).abs() < 1e-9, "k {k}: {p:?}");
            let t = a / 0.9;
            assert!((p.opacity - 0.8 * (1.0 - t)).abs() < 1e-12);
            assert!((p.size - (4.0 + 8.0 * t)).abs() < 1e-12);
            assert!((f64::from(p.colour[0]) - (1.0 - t)).abs() < 1e-6 && (f64::from(p.colour[2]) - t).abs() < 1e-6);
        }
    }
    // Types on a lone particle (1 / s, 0.9 s life: find a frame with exactly one alive).
    let mut lone = classic();
    lone.birth_rate = 1.0;
    let f = (30..120).find(|f| lone.states_at(f * TPF, &limits(), &cancel).unwrap().len() == 1).expect("a lone frame");
    let p = lone.states_at(f * TPF, &limits(), &cancel).unwrap()[0];
    let (cx, cy) = (p.position[0] as u32, p.position[1] as u32);
    let o = p.opacity as f32;
    let dot = lone.render(64, 64, f * TPF, &limits(), &cancel).unwrap();
    assert!((dot.pixel(cx, cy).unwrap()[3] - o).abs() < 1e-6, "solid dot: full coverage at the centre");
    lone.particle_type = ClassicParticleType::FadedSphere;
    let soft = lone.render(64, 64, f * TPF, &limits(), &cancel).unwrap();
    let sa = soft.pixel(cx, cy).unwrap()[3];
    assert!(sa > 0.0 && sa < o, "faded sphere falls off from the centre: {sa} vs {o}");
    assert!(soft.premultiplied.iter().map(|p| p[3]).sum::<f32>() < dot.premultiplied.iter().map(|p| p[3]).sum::<f32>());
    lone.particle_type = ClassicParticleType::Line;
    let s = lone.states_at(f * TPF, &limits(), &cancel).unwrap()[0];
    assert_eq!((s.size, s.length), (1.0, p.size), "a 1 px line as long as the particle size");
    let line = lone.render(64, 64, f * TPF, &limits(), &cancel).unwrap();
    let mut inked = 0;
    for y in 0..64u32 {
        for x in 0..64u32 {
            if line.pixel(x, y).unwrap()[3] > 0.0 {
                inked += 1;
                let d = (f64::from(x) + 0.5 - p.position[0]).hypot(f64::from(y) + 0.5 - p.position[1]);
                assert!(d <= p.size + 1.5, "({x}, {y}) {d}");
            }
        }
    }
    assert!(inked > 0);
}

#[test]
fn same_seed_same_digest_scrub_back_equals_direct_and_start_tick_is_deterministic() {
    let sims = [
        Simulation::Rain(rain()),
        Simulation::Snow(snow()),
        Simulation::StarBurst(burst()),
        Simulation::Classic(classic()),
        Simulation::World(world()),
    ];
    for s in &sims {
        let direct = frame(s, 37);
        assert!(direct.drawn > 0, "{:?}", s.kind());
        let mut forward = None;
        for f in 0..=37 {
            forward = Some(frame(s, f));
        }
        let _ = frame(s, 80);
        let back = frame(s, 37);
        assert_eq!(direct.digest(), forward.unwrap().digest(), "{:?} forward play", s.kind());
        assert_eq!(direct.premultiplied, back.premultiplied, "{:?} scrub back", s.kind());
        assert_eq!(frame(&s.clone(), 37).digest(), direct.digest(), "{:?} same parameters and seed", s.kind());
        // Another seed is another simulation; a later start shifts the same frames in time.
        let (mut reseeded, mut late) = (s.clone(), s.clone());
        let (seed, start) = match (&mut reseeded, &mut late) {
            (Simulation::Rain(a), Simulation::Rain(b)) => (&mut a.precipitation.seed, &mut b.precipitation.start_tick),
            (Simulation::Snow(a), Simulation::Snow(b)) => (&mut a.precipitation.seed, &mut b.precipitation.start_tick),
            (Simulation::StarBurst(a), Simulation::StarBurst(b)) => (&mut a.seed, &mut b.start_tick),
            (Simulation::Classic(a), Simulation::Classic(b)) => (&mut a.seed, &mut b.start_tick),
            (Simulation::World(a), Simulation::World(b)) => (&mut a.seed, &mut b.start_tick),
            _ => unreachable!(),
        };
        *seed += 1;
        *start = 10 * TPF;
        assert_ne!(frame(&reseeded, 37).digest(), direct.digest(), "{:?} reseeded", s.kind());
        assert_eq!(frame(&late, 47).digest(), direct.digest(), "{:?} start tick is the only origin", s.kind());
        let before = frame(&late, 5);
        assert_eq!(before.drawn, 0, "{:?} nothing before the start tick", s.kind());
        assert!(before.premultiplied.iter().all(|p| *p == [0.0; 4]));
    }
    // Preroll: the sky at the start tick is the sky `preroll` seconds into the simulation.
    let mut pre = rain();
    pre.precipitation.preroll = 1.0;
    pre.precipitation.start_tick = 30 * TPF;
    let cancel = CancellationToken::default();
    assert_eq!(pre.render(64, 48, 30 * TPF, &limits(), &cancel).unwrap().digest(), rain().render(64, 48, 30 * TPF, &limits(), &cancel).unwrap().digest());
}

#[test]
fn dims_u32_max_is_refused_before_allocation_for_every_simulation() {
    let cancel = CancellationToken::default();
    for kind in SimulationKind::ALL {
        let s = Simulation::new(kind);
        assert_eq!(s.render(u32::MAX, u32::MAX, 0, TPF, &limits(), &cancel).unwrap_err(), ParticleError::Raster, "{kind:?}");
        assert_eq!(s.render(0, 16, 0, TPF, &limits(), &cancel).unwrap_err(), ParticleError::Raster, "{kind:?}");
        assert_eq!(s.render(1 << 16, 1 << 11, 0, TPF, &limits(), &cancel).unwrap_err(), ParticleError::Raster, "{kind:?} over max_pixels");
    }
}

#[test]
fn aggregate_over_budget_is_refused_before_allocation_for_every_simulation() {
    let cancel = CancellationToken::default();
    let run = |s: &Simulation| s.render(64, 48, 600 * TPF, TPF, &limits(), &cancel).map(|_| ());
    // Alive-count bombs, refused from the slot-window estimate before any element is made.
    let mut r = rain();
    r.precipitation.rate = 1.0e6;
    r.precipitation.layers = vec![DepthLayer { density: 1000.0, ..DepthLayer::default() }; 16];
    assert_eq!(run(&Simulation::Rain(r)), Err(ParticleError::Count));
    let mut s = snow();
    s.precipitation.rate = 1.0e6;
    s.precipitation.speed = 1.0;
    s.precipitation.speed_random = 0.0;
    assert_eq!(run(&Simulation::Snow(s)), Err(ParticleError::Count));
    let mut b = burst();
    b.star_count = 1 << 20;
    assert_eq!(run(&Simulation::StarBurst(b.clone())), Err(ParticleError::Count));
    let tight = ParticleLimits { max_alive: 100, ..ParticleLimits::default() };
    assert_eq!(Simulation::StarBurst(burst()).render(8, 8, 0, TPF, &tight, &cancel).unwrap_err(), ParticleError::Count);
    let mut c = classic();
    c.birth_rate = 1.0e6;
    c.longevity = 600.0;
    assert_eq!(run(&Simulation::Classic(c)), Err(ParticleError::Count));
    // Window bomb: a slot index beyond 2^52 (a 1e9 / s layer far into the timeline).
    let mut w = rain();
    w.precipitation.rate = 1.0e6;
    w.precipitation.layers = vec![DepthLayer { density: 1000.0, ..DepthLayer::default() }];
    let e = Simulation::Rain(w).render(8, 8, u64::MAX, TPF, &limits(), &cancel).unwrap_err();
    assert_eq!(e, ParticleError::Window);
    // Raster work bomb: huge near stars against a small work budget.
    let mut big = burst();
    big.star_size = 4096.0;
    let work = ParticleLimits { max_raster_work: 10_000, ..ParticleLimits::default() };
    assert_eq!(Simulation::StarBurst(big).render(256, 256, 0, TPF, &work, &cancel).unwrap_err(), ParticleError::Work);
    // Cancellation.
    let c = CancellationToken::default();
    c.cancel();
    assert_eq!(Simulation::Rain(rain()).render(8, 8, 30 * TPF, TPF, &limits(), &c).unwrap_err(), ParticleError::Canceled);
}

struct Collector(Vec<(DeliveryClass, Vec<u8>)>);

impl SinkPort for Collector {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.to_vec()));
        Ok(())
    }
}

#[test]
fn terminal_outcomes_go_through_composite_report_with_codes_only() {
    let cancel = CancellationToken::default();
    let ok = Simulation::Rain(rain()).render(32, 32, 30 * TPF, TPF, &limits(), &cancel);
    let mut bad = rain();
    bad.precipitation.wind_strength = f64::NAN;
    let refused = Simulation::Rain(bad).render(32, 32, 0, TPF, &limits(), &cancel);
    let stop = CancellationToken::default();
    stop.cancel();
    let canceled = Simulation::Snow(snow()).render(32, 32, 30 * TPF, TPF, &limits(), &stop);
    let cases = [
        (outcome(&ok), Outcome::Success),
        (outcome(&refused), Outcome::Failure(FailureCode::Unavailable)),
        (outcome(&canceled), Outcome::Canceled),
    ];
    let mut sink = Collector(Vec::new());
    for (n, (got, want)) in cases.into_iter().enumerate() {
        assert_eq!(got, want);
        let actor = ActorContext::new("a", "p", "o", "op", "s", "sess").unwrap();
        let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let mut observe = Observe::new(42, 7, resource, actor, ObserveBudget::new(0, 0).unwrap());
        let receipt = report(got, 42, 7, &mut observe, &cancel, &mut sink).unwrap();
        assert_eq!(receipt.outcome, want, "case {n}");
    }
    assert_eq!(sink.0.len(), 3);
    for (class, bytes) in &sink.0 {
        assert_eq!(*class, DeliveryClass::Terminal);
        assert!(bytes.starts_with(b"HSKO"));
        for text in [&b"wind"[..], b"particle", b"Rain", b"rain"] {
            assert!(!bytes.windows(text.len()).any(|w| w == text), "no record text on the wire");
        }
    }
}
