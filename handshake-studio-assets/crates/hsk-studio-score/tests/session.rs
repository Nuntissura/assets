//! SMB-SCORE-EFFECTS-3 slice 4: multitrack mixdown with sample-accurate automation. Expected
//! samples are recomputed from the documented per-sample formulas and compared bit for bit.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;

/// Renders the whole session (`frames` total) in blocks of the given sizes (last repeats).
fn render_all(session: &mut Session, frames: usize, blocks: &[usize]) -> [Vec<f32>; 2] {
    let token = CancellationToken::default();
    let (mut l, mut r) = (vec![0.0_f32; frames], vec![0.0_f32; frames]);
    let (mut at, mut i) = (0, 0);
    while at < frames {
        let len = blocks[i.min(blocks.len() - 1)].min(frames - at);
        i += 1;
        session.render(at as u64, &mut l[at..at + len], &mut r[at..at + len], &token).unwrap();
        at += len;
    }
    [l, r]
}

fn volume(points: &[(u64, f64)], interpolation: Interpolation) -> AutomationLane {
    AutomationLane::new(points, interpolation, TRACK_VOLUME_DB).unwrap()
}
fn pan(points: &[(u64, f64)], interpolation: Interpolation) -> AutomationLane {
    AutomationLane::new(points, interpolation, TRACK_PAN).unwrap()
}
fn track(clip: Vec<Vec<f32>>, start_sample: u64) -> TrackSpec {
    TrackSpec {
        clip,
        start_sample,
        volume_db: AutomationLane::constant(0.0),
        pan: AutomationLane::constant(0.0),
        effect: None,
        effect_lanes: Vec::new(),
    }
}

#[test]
fn automation_lanes_evaluate_exactly_and_refuse_bad_points() {
    bounded_case("lanes", || {
        let lin = volume(&[(100, 0.0), (200, -20.0), (300, -20.0), (400, 6.0)], Interpolation::Linear);
        for (t, want) in [(0, 0.0), (100, 0.0), (150, -10.0), (199, -19.8), (200, -20.0), (250, -20.0), (350, -7.0), (400, 6.0), (u64::MAX, 6.0)] {
            assert!((lin.value_at(t) - want).abs() < 1e-12, "{t}: {}", lin.value_at(t));
        }
        let step = volume(&[(100, 0.0), (200, -20.0)], Interpolation::Step);
        assert_eq!((step.value_at(199), step.value_at(200), step.value_at(50)), (0.0, -20.0, 0.0));
        let err = |p: &[(u64, f64)]| AutomationLane::new(p, Interpolation::Linear, TRACK_VOLUME_DB).unwrap_err();
        assert_eq!(err(&[]), ScoreError::OutOfRange);
        assert_eq!(err(&[(5, 0.0), (5, 1.0)]), ScoreError::OutOfRange);
        assert_eq!(err(&[(6, 0.0), (5, 1.0)]), ScoreError::OutOfRange);
        assert_eq!(err(&[(0, 12.5)]), ScoreError::OutOfRange);
        assert_eq!(err(&[(0, f64::NAN)]), ScoreError::NotFinite);
        assert_eq!(err(&[(0, f64::NEG_INFINITY)]), ScoreError::NotFinite);
        let many: Vec<(u64, f64)> = (0..=MAX_AUTOMATION_POINTS as u64).map(|i| (i, 0.0)).collect();
        assert_eq!(err(&many), ScoreError::CapacityExceeded);
    });
}

#[test]
fn volume_and_pan_automation_are_sample_accurate_across_tracks() {
    bounded_case("fader_pan", || {
        let frames = 6000;
        // Track 0: mono DC 0.5 at 100, volume ramp 0 -> -20 dB over 1000..2000, pan ramp -1 -> 1.
        let mut t0 = track(vec![vec![0.5; 3000]], 100);
        t0.volume_db = volume(&[(1000, 0.0), (2000, -20.0)], Interpolation::Linear);
        t0.pan = pan(&[(500, -1.0), (2500, 1.0)], Interpolation::Linear);
        // Track 1: stereo noise at 2000, step volume -6 dB from 4000, balance step to 0.5 at 3000.
        let (nl, nr) = (noise(3, 3500), noise(4, 3500));
        let mut t1 = track(vec![nl.clone(), nr.clone()], 2000);
        t1.volume_db = volume(&[(0, 0.0), (4000, -6.0)], Interpolation::Step);
        t1.pan = pan(&[(0, 0.0), (3000, 0.5)], Interpolation::Step);
        let lanes = (t0.volume_db.clone(), t0.pan.clone(), t1.volume_db.clone(), t1.pan.clone());
        let mut session = Session::new(RATE, 1024, vec![t0, t1]).unwrap();
        assert_eq!(session.length_samples(), 5500);
        let reference = render_all(&mut session, frames, &[1024]);
        for t in 0..frames {
            let (mut l, mut r) = (0.0_f64, 0.0_f64);
            if (100..3100).contains(&t) {
                let y = 0.5 * db_to_linear(lanes.0.value_at(t as u64));
                let (gl, gr) = pan_gains(lanes.1.value_at(t as u64));
                l += y * gl;
                r += y * gr;
            }
            if (2000..5500).contains(&t) {
                let g = db_to_linear(lanes.2.value_at(t as u64));
                let p = lanes.3.value_at(t as u64);
                l += f64::from(nl[t - 2000]) * g * (1.0 - p).min(1.0);
                r += f64::from(nr[t - 2000]) * g * (1.0 + p).min(1.0);
            }
            assert_eq!((reference[0][t].to_bits(), reference[1][t].to_bits()), ((l as f32).to_bits(), (r as f32).to_bits()), "t {t}");
        }
        // Pan-law endpoint is exact: hard left at 500 (track 1 starts at 2000).
        assert_eq!(reference[1][500], 0.0);
        // Any block partition renders the same bits.
        for blocks in [&[1usize][..], &[7, 255, 1000], &[999]] {
            session.reset();
            let got = render_all(&mut session, frames, blocks);
            assert_eq!(bits(&got[0]), bits(&reference[0]), "{blocks:?}");
            assert_eq!(bits(&got[1]), bits(&reference[1]), "{blocks:?}");
        }
    });
}

#[test]
fn effect_parameter_automation_is_sample_accurate() {
    bounded_case("effect_automation", || {
        // Stereo track (balance centre = unity) through a 1 ms feedback delay (D = 48, feedback 0):
        // out = (1 - mix(t)) x(t) + mix(t) x(t - 48); mix steps 0 -> 1 at 1000 and ramps 0 -> 1 over
        // 2000..2100 after a step back to 0 at 1500.
        let frames = 4000;
        let x = noise(9, frames);
        let delay = FeedbackDelay::new(RATE, 2, DelayParams { time_ms: 1.0, feedback: 0.0, damping_hz: None, mix: 0.0, ping_pong: false }).unwrap();
        let mix = AutomationLane::new(&[(0, 0.0), (1000, 1.0), (1500, 0.0)], Interpolation::Step, (0.0, 1.0)).unwrap();
        let ramp = AutomationLane::new(&[(1500, 0.0), (2000, 0.0), (2100, 1.0)], Interpolation::Linear, (0.0, 1.0)).unwrap();
        for (lane, name) in [(mix, "step"), (ramp, "linear")] {
            let mut t = track(vec![x.clone(), x.clone()], 0);
            t.effect = Some(Box::new(FeedbackDelay::new(RATE, 2, *delay.params()).unwrap()));
            t.effect_lanes = vec![(1, lane.clone())];
            let mut session = Session::new(RATE, 512, vec![t]).unwrap();
            for blocks in [&[512usize][..], &[1, 13, 300]] {
                session.reset();
                let out = render_all(&mut session, frames, blocks);
                for n in 0..frames {
                    let m = lane.value_at(n as u64);
                    let tap = if n >= 48 { f64::from(x[n - 48]) } else { 0.0 };
                    let want = ((1.0 - m) * f64::from(x[n]) + m * tap) as f32;
                    assert_eq!(out[0][n].to_bits(), want.to_bits(), "{name} {blocks:?} n {n}");
                }
            }
        }
        // Dynamics threshold automation equals running the processor by hand with the parameter
        // set exactly at the breakpoint sample.
        let frames = 9600;
        let x = sine(300.0, 0.8, 0.0, RATE, frames);
        let params = DynamicsParams::compressor(-10.0, 4.0);
        let lane = AutomationLane::new(&[(0, -10.0), (4321, -30.0)], Interpolation::Step, (-100.0, 0.0)).unwrap();
        let mut t = track(vec![x.clone(), x.clone()], 0);
        t.effect = Some(Box::new(Dynamics::new(RATE, 2, params).unwrap()));
        t.effect_lanes = vec![(0, lane)];
        let mut session = Session::new(RATE, 1000, vec![t]).unwrap();
        let out = render_all(&mut session, frames, &[1000]);
        let mut by_hand = Dynamics::new(RATE, 2, params).unwrap();
        let first = run_blocks(&mut by_hand, &ChannelLayout::stereo(), RATE, &[x[..4321].to_vec(), x[..4321].to_vec()], &[4321]);
        by_hand.set_automated(0, -30.0).unwrap();
        let second = run_blocks(&mut by_hand, &ChannelLayout::stereo(), RATE, &[x[4321..].to_vec(), x[4321..].to_vec()], &[5279]);
        let expected: Vec<f32> = first[0].iter().chain(&second[0]).copied().collect();
        assert_eq!(bits(&out[0]), bits(&expected));
        // Ratio and makeup parameters are reachable and range-checked.
        assert_eq!(by_hand.automation_parameters().iter().map(|p| p.name).collect::<Vec<_>>(), ["threshold_db", "ratio", "makeup_db"]);
        assert_eq!(by_hand.set_automated(1, 0.5), Err(ScoreError::OutOfRange));
        assert_eq!(by_hand.set_automated(3, 1.0), Err(ScoreError::OutOfRange));
        assert_eq!(by_hand.set_automated(2, f64::NAN), Err(ScoreError::NotFinite));
    });
}

#[test]
fn contiguity_cancel_and_reset() {
    bounded_case("session_contiguity", || {
        let mut t = track(vec![noise(1, 3000)], 0);
        t.effect = Some(Box::new(FeedbackDelay::new(RATE, 1, DelayParams { time_ms: 3.0, ..DelayParams::default() }).unwrap()));
        let mut session = Session::new(RATE, 1000, vec![t]).unwrap();
        let reference = render_all(&mut session, 5000, &[1000]);
        session.reset();
        let token = CancellationToken::default();
        let (mut l, mut r) = (vec![0.0_f32; 1000], vec![0.0_f32; 1000]);
        session.render(0, &mut l, &mut r, &token).unwrap();
        assert_eq!(session.position(), 1000);
        // Non-contiguous, mismatched, empty and oversized requests are refused without moving.
        assert_eq!(session.render(0, &mut l, &mut r, &token), Err(ScoreError::TimelineMismatch));
        assert_eq!(session.render(2000, &mut l, &mut r, &token), Err(ScoreError::TimelineMismatch));
        assert_eq!(session.render(1000, &mut l[..10], &mut r[..11], &token), Err(ScoreError::LengthMismatch));
        assert_eq!(session.render(1000, &mut [], &mut [], &token), Err(ScoreError::OutOfRange));
        let (mut bl, mut br) = (vec![0.0_f32; 1001], vec![0.0_f32; 1001]);
        assert_eq!(session.render(1000, &mut bl, &mut br, &token), Err(ScoreError::CapacityExceeded));
        assert_eq!(session.position(), 1000);
        // Cancel observed before any work leaves the session where it was; reset() returns to 0
        // and rendering again from 0 reproduces the reference.
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(session.render(1000, &mut l, &mut r, &canceled), Err(ScoreError::Canceled));
        assert_eq!(session.position(), 1000);
        session.reset();
        assert_eq!(session.position(), 0);
        let again = render_all(&mut session, 5000, &[700]);
        assert_eq!(bits(&again[0]), bits(&reference[0]));
    });
}

#[test]
fn hostile_sessions_are_refused_before_rendering() {
    bounded_case("session_hostile", || {
        let ok = || track(vec![vec![0.1; 100]], 0);
        let err = |tracks: Vec<TrackSpec>, block: usize| Session::new(RATE, block, tracks).err().unwrap();
        assert_eq!(err(vec![], 64), ScoreError::OutOfRange);
        assert_eq!(err(vec![ok()], 0), ScoreError::OutOfRange);
        assert_eq!(err(vec![ok()], MAX_BLOCK_FRAMES + 1), ScoreError::CapacityExceeded);
        assert_eq!(err((0..=MAX_TRACKS).map(|_| ok()).collect(), 64), ScoreError::CapacityExceeded);
        assert_eq!(err(vec![track(vec![], 0)], 64), ScoreError::InvalidLayout);
        assert_eq!(err(vec![track(vec![vec![0.0; 4]; 3], 0)], 64), ScoreError::InvalidLayout);
        assert_eq!(err(vec![track(vec![vec![0.0; 4], vec![0.0; 5]], 0)], 64), ScoreError::LengthMismatch);
        assert_eq!(err(vec![track(vec![vec![]], 0)], 64), ScoreError::CapacityExceeded);
        assert_eq!(err(vec![track(vec![vec![0.0, f32::NAN]], 0)], 64), ScoreError::NotFinite);
        assert_eq!(err(vec![track(vec![vec![0.0; 4]], u64::MAX - 2)], 64), ScoreError::PositionOverflow);
        let mut loud = ok();
        loud.volume_db = AutomationLane::constant(40.0);
        assert_eq!(err(vec![loud], 64), ScoreError::OutOfRange);
        let mut nan_pan = ok();
        nan_pan.pan = AutomationLane::constant(f64::NAN);
        assert_eq!(err(vec![nan_pan], 64), ScoreError::NotFinite);
        // Effects: other rate, other layout, latency, unknown parameter, lane outside the
        // parameter range, lanes without an effect.
        let with = |effect: Box<dyn Automatable + Send>, lanes: Vec<(usize, AutomationLane)>| {
            let mut t = ok();
            t.effect = Some(effect);
            t.effect_lanes = lanes;
            t
        };
        let delay = |rate, ch| Box::new(FeedbackDelay::new(rate, ch, DelayParams::default()).unwrap());
        assert_eq!(err(vec![with(delay(44_100, 1), vec![])], 64), ScoreError::InvalidRate);
        assert_eq!(err(vec![with(delay(RATE, 2), vec![])], 64), ScoreError::ChannelMismatch);
        assert_eq!(err(vec![with(delay(RATE, 1), vec![(2, AutomationLane::constant(0.0))])], 64), ScoreError::OutOfRange);
        assert_eq!(err(vec![with(delay(RATE, 1), vec![(0, AutomationLane::constant(1.0))])], 64), ScoreError::OutOfRange);
        let mut bare = ok();
        bare.effect_lanes = vec![(0, AutomationLane::constant(0.0))];
        assert_eq!(err(vec![bare], 64), ScoreError::OutOfRange);
        // A latent effect is refused (no delay compensation): wrap a 5 ms sidechain lookahead.
        struct Latent(SidechainDynamics);
        impl Processor for Latent {
            fn latency_samples(&self) -> u32 {
                self.0.latency_samples()
            }
            fn accepts(&self, i: &ChannelLayout, o: &ChannelLayout) -> bool {
                self.0.accepts(i, o)
            }
            fn process(&mut self, i: &InputBlock<'_>, o: &mut OutputBlock, c: &CancellationToken) -> Result<(), ScoreError> {
                self.0.process(i, o, c)
            }
        }
        impl Automatable for Latent {
            fn automation_parameters(&self) -> &'static [AutomationParam] {
                &[]
            }
            fn set_automated(&mut self, _: usize, _: f64) -> Result<(), ScoreError> {
                Err(ScoreError::OutOfRange)
            }
        }
        let sc = SidechainDynamics::new(RATE, 1, SidechainParams { dynamics: DynamicsParams::compressor(-20.0, 2.0), key: KeySource::Internal, lookahead_ms: 5.0 }).unwrap();
        assert_eq!(err(vec![with(Box::new(Latent(sc)), vec![])], 64), ScoreError::OutOfRange);
        assert_eq!(Session::new(44_101, 64, vec![ok()]).err(), Some(ScoreError::InvalidRate));
    });
}
