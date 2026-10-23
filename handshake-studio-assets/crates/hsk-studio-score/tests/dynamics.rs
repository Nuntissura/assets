//! SMB-SCORE-DSP slice 1: compressor / expander / gate and the lookahead limiter. Every expected
//! value is derived from the published gain-computer formulas or the limiter's documented delay
//! and averaging structure; the derivations are in the comments.
mod common_dsp;
use common_dsp::*;
use hsk_studio_score::*;

#[test]
fn compressor_matches_the_static_curve_in_steady_state() {
    bounded_case("compressor_static", || {
        // Hard knee, T = -20 dB, R = 4: gain reduction = (x - T)(1/R - 1) = -0.75 (x + 20).
        let hard = DynamicsParams { knee_db: 0.0, ..DynamicsParams::compressor(-20.0, 4.0) };
        assert_eq!(hard.static_reduction_db(-30.0), 0.0);
        assert!((hard.static_reduction_db(-10.0) - (-7.5)).abs() < 1e-12);
        // Soft knee W = 6: at x = T the reduction is (1/R-1)(W/2)^2/(2W) = -0.75 * 9 / 12 = -0.5625;
        // at x = T + W/2 it meets the hard line: -0.75 * 3 = -2.25; below T - W/2 it is 0.
        let soft = DynamicsParams::compressor(-20.0, 4.0);
        assert!((soft.static_reduction_db(-20.0) - (-0.5625)).abs() < 1e-12);
        assert!((soft.static_reduction_db(-17.0) - (-2.25)).abs() < 1e-12);
        assert_eq!(soft.static_reduction_db(-23.0001), 0.0);

        // Peak detector, DC 0.5 = -6.0206 dB: steady output level = T + (x - T)/R = -16.50515 dB.
        let mut dynamics = Dynamics::new(RATE, 1, hard).unwrap();
        let input = vec![vec![0.5_f32; RATE as usize]];
        let out = run_blocks(&mut dynamics, &mono(), RATE, &input, &[512]);
        let settled = db(f64::from(*out[0].last().unwrap()));
        let expected = -20.0 + (db(0.5) + 20.0) / 4.0;
        assert!((settled - expected).abs() < 1e-5, "{settled} vs {expected}");
        assert_eq!(dynamics.latency_samples(), 0);

        // RMS detector, sine amplitude 0.5: RMS 0.35355 = -9.0309 dB, reduction -0.75 * 10.9691.
        let params = DynamicsParams { detector: Detector::Rms, ..hard };
        let mut dynamics = Dynamics::new(RATE, 1, params).unwrap();
        let tone = vec![sine(1000.0, 0.5, 0.0, RATE, 2 * RATE as usize)];
        let out = run_blocks(&mut dynamics, &mono(), RATE, &tone, &[300]);
        let tail = RATE as usize..2 * RATE as usize;
        let measured = db(rms(&out[0][tail.clone()]) / rms(&tone[0][tail]));
        let expected = -0.75 * (db(0.5 / 2.0_f64.sqrt()) + 20.0);
        assert!((measured - expected).abs() < 0.05, "{measured} vs {expected}");
    });
}

#[test]
fn expander_and_gate_follow_the_curve_range_and_hold() {
    bounded_case("expander_gate", || {
        // Hard expander T = -40, R = 2: reduction = (x - T)(R - 1) = x + 40; DC 0.001 = -60 dB
        // -> -20 dB, so the output is -80 dB (1e-4 * 0.001... = 0.001 * 0.1 = 1e-4).
        let params = DynamicsParams { release_ms: 1.0, ..DynamicsParams::expander(-40.0, 2.0) };
        let mut dynamics = Dynamics::new(RATE, 1, params).unwrap();
        let input = vec![vec![0.001_f32; RATE as usize / 2]];
        let out = run_blocks(&mut dynamics, &mono(), RATE, &input, &[777]);
        assert!((f64::from(*out[0].last().unwrap()) - 1e-4).abs() < 1e-8);
        // Range 10 dB floors the reduction at -10 dB: 0.001 * 10^(-0.5).
        let limited = DynamicsParams { range_db: 10.0, ..params };
        let mut dynamics = Dynamics::new(RATE, 1, limited).unwrap();
        let out = run_blocks(&mut dynamics, &mono(), RATE, &input, &[777]);
        assert!((f64::from(*out[0].last().unwrap()) - 0.001 * 10f64.powf(-0.5)).abs() < 1e-8);
        // Soft expander knee W = 8 at x = T: -(R-1)(0 - 4)^2/(2W) = -16/16 = -1 dB... derived:
        // -(1)*(x - T - W/2)^2/(2W) = -(−4)^2/16 = -1.
        let soft = DynamicsParams { knee_db: 8.0, ..DynamicsParams::expander(-40.0, 2.0) };
        assert!((soft.static_reduction_db(-40.0) - (-1.0)).abs() < 1e-12);
        assert_eq!(soft.static_reduction_db(-35.9), 0.0);

        // Gate with 50 ms hold, 1 ms release: after loud -> quiet at sample S the gain stays open
        // for exactly hold_samples (2400) frames, then closes to the 80 dB range floor.
        let gate = DynamicsParams { release_ms: 1.0, ..DynamicsParams::gate(-40.0) };
        let mut dynamics = Dynamics::new(RATE, 1, gate).unwrap();
        let switch = 9_600;
        let mut signal = vec![0.5_f32; switch];
        signal.extend(std::iter::repeat_n(0.001_f32, 24_000));
        let out = run_blocks(&mut dynamics, &mono(), RATE, &[signal], &[1000]);
        // Held: first 2400 quiet frames pass at unity (the loud frames left the gate open).
        assert_eq!(out[0][switch], 0.001);
        assert_eq!(out[0][switch + 2399], 0.001);
        // Closed: 80 dB below -60 dB wanted (-5940 dB clamped to -80): 0.001 * 1e-4 = 1e-7.
        assert!((f64::from(out[0][switch + 12_000]) - 1e-7).abs() < 1e-10);
    });
}

#[test]
fn partition_independence_and_stereo_link() {
    bounded_case("partition_link", || {
        let stereo = ChannelLayout::stereo();
        let signal = vec![noise(11, 20_000), noise(23, 20_000)];
        let params = [
            DynamicsParams { detector: Detector::Rms, ..DynamicsParams::compressor(-24.0, 6.0) },
            DynamicsParams { link: false, ..DynamicsParams::compressor(-30.0, 3.0) },
            DynamicsParams::gate(-20.0),
        ];
        for p in params {
            let reference = run_blocks(&mut Dynamics::new(RATE, 2, p).unwrap(), &stereo, RATE, &signal, &[20_000]);
            for blocks in [&[1usize][..], &[7, 255, 256, 257, 1000], &[4099]] {
                let got = run_blocks(&mut Dynamics::new(RATE, 2, p).unwrap(), &stereo, RATE, &signal, blocks);
                assert_eq!(bits(&got[0]), bits(&reference[0]), "{p:?} {blocks:?}");
                assert_eq!(bits(&got[1]), bits(&reference[1]), "{p:?} {blocks:?}");
            }
        }
        // Linked: one gain for both channels, so out/in agrees between L and R.
        let p = DynamicsParams::compressor(-30.0, 8.0);
        let out = run_blocks(&mut Dynamics::new(RATE, 2, p).unwrap(), &stereo, RATE, &signal, &[512]);
        let mut checked = 0;
        for n in 0..20_000 {
            if signal[0][n].abs() > 0.2 && signal[1][n].abs() > 0.2 {
                let (a, b) = (out[0][n] / signal[0][n], out[1][n] / signal[1][n]);
                assert!((a - b).abs() < 1e-6, "frame {n}: {a} vs {b}");
                checked += 1;
            }
        }
        assert!(checked > 5000);
        // Unlinked differs when the channels differ in level.
        let quiet_right = vec![noise(5, 20_000), noise(6, 20_000).iter().map(|v| v * 0.01).collect()];
        let p = DynamicsParams { link: false, ..DynamicsParams::compressor(-30.0, 8.0) };
        let out = run_blocks(&mut Dynamics::new(RATE, 2, p).unwrap(), &stereo, RATE, &quiet_right, &[512]);
        let right_gain = rms(&out[1]) / rms(&quiet_right[1]);
        assert!(right_gain > 0.99, "quiet channel must stay untouched: {right_gain}");
    });
}

#[test]
fn limiter_holds_the_ceiling_with_documented_latency_and_true_peak() {
    bounded_case("limiter", || {
        let ceiling = 10f64.powf(-6.0 / 20.0);
        // Sample-peak mode, lookahead 2 ms = 96 samples -> latency L - 1 = 95.
        let params = LimiterParams { ceiling_db: -6.0, lookahead_ms: 2.0, release_ms: 50.0, true_peak: false };
        let mut limiter = LookaheadLimiter::new(RATE, 1, params).unwrap();
        assert_eq!(limiter.latency_samples(), 95);
        // Impulse 4.0 at 1000 in silence: requirement r = ceiling / 4 for the 96 windows that
        // contain it, so the boxcar mean at output time 1000 + 95 is exactly r and the output is
        // 4 * r = ceiling there; every other output sample is zero.
        let mut impulse = vec![0.0_f32; 4000];
        impulse[1000] = 4.0;
        let out = run_blocks(&mut limiter, &mono(), RATE, &[impulse], &[333]);
        assert!((f64::from(out[0][1095]) - ceiling).abs() < 1e-6, "{}", out[0][1095]);
        assert!(out[0].iter().enumerate().all(|(i, v)| i == 1095 || *v == 0.0));
        // Loud noise (amplitude 4x full scale equivalents): never above the ceiling.
        let loud: Vec<f32> = noise(3, 50_000).iter().map(|v| v * 3.0).collect();
        let out = run_blocks(&mut limiter, &mono(), RATE, std::slice::from_ref(&loud), &[1024]);
        assert!(out[0].iter().all(|v| f64::from(v.abs()) <= ceiling * (1.0 + 1e-6)));
        assert_eq!(limiter.clamp_events(), 0, "the unconditional clamp must not have been needed");
        assert!(rms(&out[0][5000..]) > 0.1, "limiter must not mute the signal");
        // Below the ceiling the limiter is a pure delay: bit-exact.
        let quiet: Vec<f32> = noise(4, 5000).iter().map(|v| v * 0.1).collect();
        let mut limiter = LookaheadLimiter::new(RATE, 1, params).unwrap();
        let out = run_blocks(&mut limiter, &mono(), RATE, std::slice::from_ref(&quiet), &[500]);
        assert_eq!(bits(&out[0][95..]), bits(&quiet[..5000 - 95]));

        // True peak: fs/4 sine at 45 degrees, amplitude 1: samples are +/-0.70711, the
        // continuous peak is 1.0. A sample-peak limiter at -6 dB settles to samples of
        // exactly 0.5012 (continuous peak 0.7088 = ceiling + 3.01 dB); the true-peak limiter
        // settles to samples of ceiling / sqrt(2) (continuous peak = ceiling).
        let tone = vec![sine(f64::from(RATE) / 4.0, 1.0, std::f64::consts::FRAC_PI_4, RATE, 30_000)];
        let sp = LimiterParams { true_peak: false, ..params };
        let tp = LimiterParams { true_peak: true, ..params };
        let out_sp = run_blocks(&mut LookaheadLimiter::new(RATE, 1, sp).unwrap(), &mono(), RATE, &tone, &[999]);
        let out_tp = run_blocks(&mut LookaheadLimiter::new(RATE, 1, tp).unwrap(), &mono(), RATE, &tone, &[999]);
        let steady = |s: &[f32]| s[20_000..].iter().fold(0.0_f64, |m, v| m.max(f64::from(v.abs())));
        assert!((steady(&out_sp[0]) - ceiling).abs() < 1e-5);
        let tp_samples = steady(&out_tp[0]);
        let tp_peak_db = db(tp_samples * 2.0_f64.sqrt()) - db(ceiling);
        assert!(tp_peak_db.abs() < 0.25, "true-peak mode continuous peak off ceiling by {tp_peak_db} dB");
        assert_eq!(LookaheadLimiter::new(RATE, 1, tp).unwrap().latency_samples(), 95 + 6);
    });
}

#[test]
fn hostile_parameters_and_signals_are_refused_or_finite() {
    bounded_case("hostile", || {
        let base = DynamicsParams::compressor(-20.0, 4.0);
        let nan = f64::NAN;
        let cases: Vec<(DynamicsParams, ScoreError)> = vec![
            (DynamicsParams { threshold_db: nan, ..base }, ScoreError::NotFinite),
            (DynamicsParams { ratio: f64::INFINITY, ..base }, ScoreError::NotFinite),
            (DynamicsParams { knee_db: nan, ..base }, ScoreError::NotFinite),
            (DynamicsParams { attack_ms: f64::NEG_INFINITY, ..base }, ScoreError::NotFinite),
            (DynamicsParams { threshold_db: 6.0, ..base }, ScoreError::OutOfRange),
            (DynamicsParams { ratio: 0.5, ..base }, ScoreError::OutOfRange),
            (DynamicsParams { release_ms: 0.0, ..base }, ScoreError::OutOfRange),
            (DynamicsParams { rms_window_ms: 0.0, ..base }, ScoreError::OutOfRange),
            (DynamicsParams { makeup_db: 49.0, ..base }, ScoreError::OutOfRange),
        ];
        for (p, want) in cases {
            assert_eq!(Dynamics::new(RATE, 2, p).err(), Some(want), "{p:?}");
        }
        assert_eq!(Dynamics::new(44_101, 2, base).err(), Some(ScoreError::InvalidRate));
        assert_eq!(Dynamics::new(RATE, 0, base).err(), Some(ScoreError::InvalidLayout));
        assert_eq!(Dynamics::new(RATE, 17, base).err(), Some(ScoreError::InvalidLayout));
        let lim = LimiterParams::default();
        for p in [
            LimiterParams { ceiling_db: nan, ..lim },
            LimiterParams { lookahead_ms: f64::INFINITY, ..lim },
        ] {
            assert_eq!(LookaheadLimiter::new(RATE, 1, p).err(), Some(ScoreError::NotFinite));
        }
        for p in [
            LimiterParams { ceiling_db: 0.5, ..lim },
            LimiterParams { lookahead_ms: 0.0, ..lim },
            LimiterParams { release_ms: 5000.0, ..lim },
        ] {
            assert_eq!(LookaheadLimiter::new(RATE, 1, p).err(), Some(ScoreError::OutOfRange));
        }
        assert_eq!(LookaheadLimiter::new(44_101, 1, lim).err(), Some(ScoreError::InvalidRate));

        // Digital silence and f32::MAX swings: no NaN is ever published; either finite or refused.
        let eng = engine();
        let token = hsk_studio_accord::CancellationToken::default();
        let mut procs: Vec<Box<dyn Processor>> = vec![
            Box::new(Dynamics::new(RATE, 1, base).unwrap()),
            Box::new(Dynamics::new(RATE, 1, DynamicsParams::gate(-40.0)).unwrap()),
            Box::new(Dynamics::new(RATE, 1, DynamicsParams { makeup_db: 48.0, ..base }).unwrap()),
            Box::new(LookaheadLimiter::new(RATE, 1, lim).unwrap()),
        ];
        let silence = vec![0.0_f32; 4096];
        let swing: Vec<f32> = (0..4096).map(|n| if n % 2 == 0 { f32::MAX } else { -f32::MAX }).collect();
        for proc in &mut procs {
            for signal in [&silence, &swing] {
                let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames: 4096, start_sample: 0 };
                let channels: [&[f32]; 1] = [signal];
                let mut out = OutputBlock::with_capacity(mono(), 4096).unwrap();
                match eng.process(&ctx(), &InputBlock::new(&sp, &channels), proc.as_mut(), &mut out, &token) {
                    Ok(_) => assert!(out.published_channel(0).unwrap().iter().all(|v| v.is_finite())),
                    Err(e) => assert_eq!(e, ScoreError::NotFinite),
                }
            }
        }
        // Silence stays exactly silent through every processor.
        let mut fresh = Dynamics::new(RATE, 1, base).unwrap();
        let out = run_blocks(&mut fresh, &mono(), RATE, std::slice::from_ref(&silence), &[1024]);
        assert!(out[0].iter().all(|v| *v == 0.0));
        // Wrong layout / rate are refused by the engine before any write.
        let mut stereo_proc = Dynamics::new(RATE, 2, base).unwrap();
        let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames: 16, start_sample: 0 };
        let channels: [&[f32]; 1] = [&silence[..16]];
        let mut out = OutputBlock::with_capacity(mono(), 16).unwrap();
        assert_eq!(
            eng.process(&ctx(), &InputBlock::new(&sp, &channels), &mut stereo_proc, &mut out, &token).err(),
            Some(ScoreError::ChannelMismatch)
        );
        let mut p96 = Dynamics::new(96_000, 1, base).unwrap();
        assert_eq!(
            eng.process(&ctx(), &InputBlock::new(&sp, &channels), &mut p96, &mut out, &token).err(),
            Some(ScoreError::InvalidRate)
        );
    });
}
