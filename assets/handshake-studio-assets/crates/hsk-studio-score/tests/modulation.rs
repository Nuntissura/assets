//! SMB-SCORE-EFFECTS-3 slice 2: flanger, chorus and phaser. Expected values are the analytic
//! responses of the documented structures (fractional delay of a sine, feedback comb, first-order
//! allpass cascade) and the documented LFO trajectories.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;
use std::f64::consts::PI;

/// Amplitude of the `f` Hz component over a whole number of periods (lock-in).
fn amplitude(x: &[f32], f: f64, rate: u32) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (n, v) in x.iter().enumerate() {
        let (s, c) = (2.0 * PI * f * n as f64 / f64::from(rate)).sin_cos();
        re += f64::from(*v) * c;
        im += f64::from(*v) * s;
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

/// Complex multiply / divide helpers on (re, im).
fn cmul(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}
fn cdiv(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let d = b.0 * b.0 + b.1 * b.1;
    ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
}
fn cabs(a: (f64, f64)) -> f64 {
    (a.0 * a.0 + a.1 * a.1).sqrt()
}
/// `z^-k` at frequency `f`.
fn zk(f: f64, k: f64, rate: u32) -> (f64, f64) {
    let w = 2.0 * PI * f / f64::from(rate);
    ((w * k).cos(), -(w * k).sin())
}
fn sweep(phase: f64) -> f64 {
    0.5 - 0.5 * (2.0 * PI * phase).cos()
}

fn static_flanger(ms: f64, feedback: f64, mix: f64) -> FlangerParams {
    FlangerParams { initial_ms: ms, final_ms: ms, rate_hz: 1.0, stereo_phase_deg: 0.0, feedback, mix }
}

#[test]
fn flanger_reads_fractional_delays_and_follows_the_lfo_trajectory() {
    bounded_case("flanger_fractional", || {
        // Static fractional delays, wet only: out[n] = x(n - D) for a 1 kHz sine (Lagrange-3 error
        // at w = 0.13 rad is below 1e-5).
        for (rate, ms) in [(RATE, 2.3), (44_100, 3.7), (96_000, 0.77)] {
            let d = ms * (f64::from(rate) / 1000.0); // the processor's own formula
            let mut fl = Flanger::new(rate, 1, static_flanger(ms, 0.0, 1.0)).unwrap();
            let x = sine(1000.0, 0.5, 0.0, rate, 4800);
            let out = run_blocks(&mut fl, &mono(), rate, &[x], &[333]);
            assert_eq!(fl.delay_samples(0), Some(d));
            for (n, v) in out[0].iter().enumerate().skip(300) {
                let want = 0.5 * (2.0 * PI * 1000.0 * (n as f64 - d) / f64::from(rate)).sin();
                assert!((f64::from(*v) - want).abs() < 1e-4, "{rate} {ms} frame {n}");
            }
        }
        // LFO: d(m) = 48 + 192 * sweep(frac(phase0 + m * 0.5 / 48000)) samples; right channel
        // starts at 90 degrees.
        let p = FlangerParams { initial_ms: 1.0, final_ms: 5.0, rate_hz: 0.5, stereo_phase_deg: 90.0, feedback: 0.3, mix: 0.5 };
        for frames in [1usize, 1000, 24_000, 48_017, 100_000] {
            let mut fl = Flanger::new(RATE, 2, p).unwrap();
            let zeros = vec![vec![0.0_f32; frames], vec![0.0_f32; frames]];
            let _ = run_blocks(&mut fl, &ChannelLayout::stereo(), RATE, &zeros, &[777]);
            let m = (frames - 1) as f64;
            for (c, phase0) in [(0usize, 0.0), (1, 0.25)] {
                let want = 48.0 + 192.0 * sweep((phase0 + m * 0.5 / 48_000.0).fract());
                let got = fl.delay_samples(c).unwrap();
                assert!((got - want).abs() < 1e-6, "frames {frames} ch {c}: {got} vs {want}");
            }
        }
    });
}

#[test]
fn flanger_feedback_comb_matches_the_analytic_response() {
    bounded_case("flanger_comb", || {
        // Integer delay D = 48 (1 ms) reads one stored sample exactly. H = (1-mix) + mix z^-D / (1 - g z^-D).
        for g in [0.0, 0.5, -0.5] {
            for f in [500.0, 1000.0, 750.0] {
                let mut fl = Flanger::new(RATE, 1, static_flanger(1.0, g, 0.5)).unwrap();
                let x = sine(f, 0.5, 0.0, RATE, 2 * RATE as usize);
                let out = run_blocks(&mut fl, &mono(), RATE, &[x], &[4096]);
                let z = zk(f, 48.0, RATE);
                let wet = cdiv(z, (1.0 - g * z.0, -g * z.1));
                let want = 0.5 * cabs((0.5 + 0.5 * wet.0, 0.5 * wet.1));
                let got = amplitude(&out[0][RATE as usize..], f, RATE);
                assert!((got - want).abs() < 1e-5, "g {g} f {f}: {got} vs {want}");
            }
        }
        // g = 0 at 500 Hz is a full notch (w D = pi).
        let mut fl = Flanger::new(RATE, 1, static_flanger(1.0, 0.0, 0.5)).unwrap();
        let out = run_blocks(&mut fl, &mono(), RATE, &[sine(500.0, 0.5, 0.0, RATE, RATE as usize)], &[1000]);
        assert!(amplitude(&out[0][4800..], 500.0, RATE) < 1e-5);
    });
}

#[test]
fn chorus_and_phaser_match_their_analytic_responses() {
    bounded_case("chorus_phaser", || {
        // Chorus, 3 voices, wet only, 200 Hz sine: out(n) = mean_v sin(w (n - d_v(n))) with
        // d_v(n) = 480 + 96 sin(2 pi (frac(n * 0.8 / 48000) + v / 3)).
        let p = ChorusParams { voices: 3, delay_ms: 10.0, depth_ms: 2.0, rate_hz: 0.8, stereo_phase_deg: 0.0, mix: 1.0 };
        let x = sine(200.0, 0.5, 0.0, RATE, RATE as usize);
        let out = run_blocks(&mut Chorus::new(RATE, 1, p).unwrap(), &mono(), RATE, &[x], &[999]);
        let w = 2.0 * PI * 200.0 / f64::from(RATE);
        for (n, v) in out[0].iter().enumerate().skip(1000) {
            let phase = (n as f64 * 0.8 / 48_000.0).fract();
            let want = (0..3)
                .map(|k| {
                    let d = 480.0 + 96.0 * (2.0 * PI * (phase + f64::from(k) / 3.0)).sin();
                    0.5 * (w * (n as f64 - d)).sin()
                })
                .sum::<f64>()
                / 3.0;
            assert!((f64::from(*v) - want).abs() < 1e-5, "frame {n}");
        }

        // Phaser, static break 1 kHz: A(z) = (k + z^-1)/(1 + k z^-1); Y = A^S X / (1 - g z^-1 A^S);
        // out = (1-mix) X + mix Y.
        let k = {
            let t = (PI * 1000.0 / f64::from(RATE)).tan();
            (t - 1.0) / (t + 1.0)
        };
        for (stages, g, mix) in [(2usize, 0.0, 0.5), (4, 0.0, 0.5), (4, 0.4, 0.5), (6, -0.3, 1.0)] {
            for f in [300.0, 1000.0, 3000.0] {
                let p = PhaserParams { stages, min_hz: 1000.0, max_hz: 1000.0, rate_hz: 1.0, stereo_phase_deg: 0.0, feedback: g, mix };
                let mut ph = Phaser::new(RATE, 1, p).unwrap();
                let out = run_blocks(&mut ph, &mono(), RATE, &[sine(f, 0.5, 0.0, RATE, 2 * RATE as usize)], &[2048]);
                let z1 = zk(f, 1.0, RATE);
                let a = cdiv((k + z1.0, z1.1), (1.0 + k * z1.0, k * z1.1));
                let mut chain = (1.0, 0.0);
                for _ in 0..stages {
                    chain = cmul(chain, a);
                }
                let fbk = cmul((g * z1.0, g * z1.1), chain);
                let y = cdiv(chain, (1.0 - fbk.0, -fbk.1));
                let want = 0.5 * cabs(((1.0 - mix) + mix * y.0, mix * y.1));
                let got = amplitude(&out[0][RATE as usize..], f, RATE);
                assert!((got - want).abs() < 1e-5, "S {stages} g {g} f {f}: {got} vs {want}");
            }
        }
        // 2 stages at 1 kHz are -180 degrees there: mix 0.5 is a full notch.
        let p = PhaserParams { stages: 2, min_hz: 1000.0, max_hz: 1000.0, rate_hz: 1.0, stereo_phase_deg: 0.0, feedback: 0.0, mix: 0.5 };
        let out = run_blocks(&mut Phaser::new(RATE, 1, p).unwrap(), &mono(), RATE, &[sine(1000.0, 0.5, 0.0, RATE, RATE as usize)], &[512]);
        assert!(amplitude(&out[0][4800..], 1000.0, RATE) < 1e-5);
        // Exponential sweep: f_b(m) = 200 * 10^sweep(frac(m / 48000)).
        let p = PhaserParams { min_hz: 200.0, max_hz: 2000.0, rate_hz: 1.0, ..PhaserParams::default() };
        for frames in [1usize, 12_000, 30_001] {
            let mut ph = Phaser::new(RATE, 1, p).unwrap();
            let _ = run_blocks(&mut ph, &mono(), RATE, &[vec![0.0; frames]], &[700]);
            let want = 200.0 * 10f64.powf(sweep(((frames - 1) as f64 / 48_000.0).fract()));
            assert!((ph.break_hz(0).unwrap() - want).abs() < 1e-6, "{frames}");
        }
    });
}

#[test]
fn every_modulation_effect_is_partition_independent() {
    bounded_case("modulation_partition", || {
        let stereo = ChannelLayout::stereo();
        let signal = vec![noise(31, 30_000), noise(32, 30_000)];
        type Maker = Box<dyn Fn() -> Box<dyn Processor>>;
        let makers: Vec<Maker> = vec![
            Box::new(|| Box::new(Flanger::new(RATE, 2, FlangerParams::default()).unwrap())),
            Box::new(|| Box::new(Chorus::new(RATE, 2, ChorusParams::default()).unwrap())),
            Box::new(|| Box::new(Phaser::new(RATE, 2, PhaserParams { rate_hz: 3.0, ..PhaserParams::default() }).unwrap())),
        ];
        for make in &makers {
            let reference = run_blocks(make().as_mut(), &stereo, RATE, &signal, &[30_000]);
            assert!(reference[0].iter().all(|v| v.is_finite()));
            for blocks in [&[1usize][..], &[7, 255, 256, 257, 1000], &[4099]] {
                let got = run_blocks(make().as_mut(), &stereo, RATE, &signal, blocks);
                assert_eq!(bits(&got[0]), bits(&reference[0]), "{blocks:?}");
                assert_eq!(bits(&got[1]), bits(&reference[1]), "{blocks:?}");
            }
            // reset() restores the initial state.
            let mut p = make();
            let _ = run_blocks(p.as_mut(), &stereo, RATE, &signal, &[512]);
            p.reset();
            assert_eq!(bits(&run_blocks(p.as_mut(), &stereo, RATE, &signal, &[512])[1]), bits(&reference[1]));
        }
    });
}

#[test]
fn hostile_parameters_blocks_and_undeclared_spec_rows_are_refused() {
    bounded_case("modulation_hostile", || {
        let fl = FlangerParams::default();
        let ch = ChorusParams::default();
        let ph = PhaserParams::default();
        for v in [f64::NAN, f64::INFINITY] {
            assert_eq!(Flanger::new(RATE, 1, FlangerParams { initial_ms: v, ..fl }).unwrap_err(), ScoreError::NotFinite);
            assert_eq!(Flanger::new(RATE, 1, FlangerParams { rate_hz: v, ..fl }).unwrap_err(), ScoreError::NotFinite);
            assert_eq!(Chorus::new(RATE, 1, ChorusParams { depth_ms: v, ..ch }).unwrap_err(), ScoreError::NotFinite);
            assert_eq!(Phaser::new(RATE, 1, PhaserParams { max_hz: v, ..ph }).unwrap_err(), ScoreError::NotFinite);
            assert_eq!(Phaser::new(RATE, 1, PhaserParams { feedback: v, ..ph }).unwrap_err(), ScoreError::NotFinite);
        }
        let out_of_range = [
            Flanger::new(RATE, 1, FlangerParams { final_ms: 20.001, ..fl }).err(),
            Flanger::new(RATE, 1, FlangerParams { initial_ms: -1.0, ..fl }).err(),
            Flanger::new(RATE, 1, FlangerParams { feedback: 1.0, ..fl }).err(),
            Flanger::new(RATE, 1, FlangerParams { rate_hz: 0.0, ..fl }).err(),
            Flanger::new(RATE, 1, FlangerParams { stereo_phase_deg: 361.0, ..fl }).err(),
            Flanger::new(RATE, 1, FlangerParams { mix: 1e300, ..fl }).err(),
            Chorus::new(RATE, 1, ChorusParams { voices: 0, ..ch }).err(),
            Chorus::new(RATE, 1, ChorusParams { voices: 5, ..ch }).err(),
            Chorus::new(RATE, 1, ChorusParams { depth_ms: 20.5, ..ch }).err(),
            Chorus::new(RATE, 1, ChorusParams { delay_ms: 51.0, ..ch }).err(),
            Phaser::new(RATE, 1, PhaserParams { stages: 3, ..ph }).err(),
            Phaser::new(RATE, 1, PhaserParams { stages: 14, ..ph }).err(),
            Phaser::new(RATE, 1, PhaserParams { stages: 0, ..ph }).err(),
            Phaser::new(RATE, 1, PhaserParams { min_hz: 10.0, ..ph }).err(),
            Phaser::new(RATE, 1, PhaserParams { min_hz: 3000.0, max_hz: 300.0, ..ph }).err(),
            Phaser::new(RATE, 1, PhaserParams { max_hz: 24_000.0, ..ph }).err(),
        ];
        for (i, e) in out_of_range.iter().enumerate() {
            assert_eq!(*e, Some(ScoreError::OutOfRange), "case {i}");
        }
        for c in [0, 17] {
            assert_eq!(Flanger::new(RATE, c, fl).unwrap_err(), ScoreError::InvalidLayout);
            assert_eq!(Chorus::new(RATE, c, ch).unwrap_err(), ScoreError::InvalidLayout);
            assert_eq!(Phaser::new(RATE, c, ph).unwrap_err(), ScoreError::InvalidLayout);
        }
        assert_eq!(Phaser::new(44_101, 1, ph).unwrap_err(), ScoreError::InvalidRate);
        // 0 ms is admitted (spec range) and reads at the one-sample floor.
        let mut zero = Flanger::new(RATE, 1, static_flanger(0.0, 0.0, 1.0)).unwrap();
        let out = run_blocks(&mut zero, &mono(), RATE, &[vec![1.0, 0.0, 0.0]], &[3]);
        assert_eq!((out[0][1], zero.delay_samples(0)), (1.0, Some(1.0)));

        let mut p = Phaser::new(RATE, 1, ph).unwrap();
        let (eng, token) = (engine(), CancellationToken::default());
        let mut out = OutputBlock::with_capacity(mono(), 64).unwrap();
        let mut x = vec![0.1_f32; 65];
        for frames in [MAX_BLOCK_FRAMES + 1, 65] {
            let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames, start_sample: 0 };
            let slices: Vec<&[f32]> = vec![&x];
            assert_eq!(eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut p, &mut out, &token).unwrap_err(), ScoreError::CapacityExceeded);
        }
        x[0] = f32::INFINITY;
        let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames: 64, start_sample: 0 };
        let slices: Vec<&[f32]> = vec![&x[..64]];
        assert_eq!(eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut p, &mut out, &token).unwrap_err(), ScoreError::NotFinite);
        assert_eq!(out.published_frames(), 0);

        // Spec rows: Flanger names with clear meaning bind; cycles/beats and the Chorus/Flanger and
        // Phaser intensity/depth rows are typed refusals.
        assert!(bind_spec_row("Flanger", "initial").unwrap().contains("initial_ms"));
        assert!(bind_spec_row("Phaser", "mix").is_ok());
        for (e, p) in [("Flanger", "cycles"), ("Flanger", "beats"), ("Chorus/Flanger", "width"), ("Chorus/Flanger", "intensity"), ("Chorus/Flanger", "transience"), ("Phaser", "intensity"), ("Phaser", "depth")] {
            assert!(matches!(bind_spec_row(e, p), Err(SpecRowError::Undeclared(u)) if u.effect == e && u.parameter == p), "{e}/{p}");
        }
    });
}
