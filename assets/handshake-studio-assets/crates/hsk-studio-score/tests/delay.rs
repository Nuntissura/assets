//! SMB-SCORE-EFFECTS-2 slice 2: feedback delay. Every expected value follows from the documented
//! recurrence (`line = x + feedback * f(tap)`, `out = (1 - mix) x + mix tap`).
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;

fn impulse(frames: usize) -> Vec<f32> {
    let mut v = vec![0.0_f32; frames];
    v[0] = 1.0;
    v
}

fn wet(time_ms: f64, feedback: f64) -> DelayParams {
    DelayParams { time_ms, feedback, damping_hz: None, mix: 1.0, ping_pong: false }
}

#[test]
fn impulse_echoes_land_on_exact_samples_with_exact_amplitudes() {
    bounded_case("delay_echoes", || {
        // 10 ms: D = 480 at 48 kHz, 441 at 44.1 kHz, 960 at 96 kHz (sample-rate aware).
        for (rate, d) in [(RATE, 480usize), (44_100, 441), (96_000, 960)] {
            for fb in [0.5_f64, -0.5] {
                let mut delay = FeedbackDelay::new(rate, 1, wet(10.0, fb)).unwrap();
                assert_eq!(delay.delay_samples(), d);
                assert_eq!(delay.latency_samples(), 0);
                let out = run_blocks(&mut delay, &mono(), rate, &[impulse(12 * d)], &[333]);
                for (n, v) in out[0].iter().enumerate() {
                    // Echo m (m >= 1) at m*D with feedback^(m-1); powers of 0.5 are exact in f32.
                    let want = if n > 0 && n % d == 0 { fb.powi((n / d) as i32 - 1) as f32 } else { 0.0 };
                    assert_eq!(*v, want, "rate {rate} fb {fb} frame {n}");
                }
            }
        }
        // DC steady state: (1 - mix) + mix / (1 - fb) = 0.5 + 0.5 * 4 = 2.5 (with or without damping,
        // the loop low-pass has unity DC gain). D = 48 (1 ms); after 400 passes 0.75^400 ~ 1e-50.
        for damping_hz in [None, Some(500.0)] {
            let p = DelayParams { time_ms: 1.0, feedback: 0.75, damping_hz, mix: 0.5, ping_pong: false };
            let mut delay = FeedbackDelay::new(RATE, 1, p).unwrap();
            let out = run_blocks(&mut delay, &mono(), RATE, &[vec![1.0; 48 * 400]], &[4096]);
            assert!((f64::from(*out[0].last().unwrap()) - 2.5).abs() < 1e-6, "{damping_hz:?}");
        }
    });
}

#[test]
fn damping_and_ping_pong_follow_the_recurrence() {
    bounded_case("delay_damping_pingpong", || {
        // Damping f_d = 2 kHz: k = exp(-2 pi f_d / rate). Echo 1 is undamped (the input is written
        // straight into the line); echo 2 is fb (1 - k) k^j for 0 <= j < D.
        let d = 480;
        let k = (-2.0 * std::f64::consts::PI * 2000.0 / f64::from(RATE)).exp();
        let p = DelayParams { damping_hz: Some(2000.0), ..wet(10.0, 0.6) };
        let out = run_blocks(&mut FeedbackDelay::new(RATE, 1, p).unwrap(), &mono(), RATE, &[impulse(3 * d)], &[100]);
        assert_eq!(out[0][d], 1.0);
        assert!(out[0][d + 1..2 * d].iter().all(|v| *v == 0.0));
        for j in 0..d {
            let want = 0.6 * (1.0 - k) * k.powi(j as i32);
            assert!((f64::from(out[0][2 * d + j]) - want).abs() < 1e-7, "j {j}");
        }

        // Ping-pong, impulse into L only: L at D (1), R at 2D (fb), L at 3D (fb^2), R at 4D (fb^3);
        // the other side stays exactly 0.
        let p = DelayParams { ping_pong: true, ..wet(10.0, 0.5) };
        let stereo = ChannelLayout::stereo();
        let signal = vec![impulse(5 * d), vec![0.0; 5 * d]];
        let out = run_blocks(&mut FeedbackDelay::new(RATE, 2, p).unwrap(), &stereo, RATE, &signal, &[777]);
        for (n, (l, r)) in out[0].iter().zip(&out[1]).enumerate() {
            let (want_l, want_r) = match n {
                _ if n == d => (1.0, 0.0),
                _ if n == 2 * d => (0.0, 0.5),
                _ if n == 3 * d => (0.25, 0.0),
                _ if n == 4 * d => (0.0, 0.125),
                _ => (0.0, 0.0),
            };
            assert_eq!((*l, *r), (want_l, want_r), "frame {n}");
        }
    });
}

#[test]
fn partition_reset_and_tail_bound() {
    bounded_case("delay_partition_tail", || {
        let stereo = ChannelLayout::stereo();
        let signal = vec![noise(3, 30_000), noise(4, 30_000)];
        let p = DelayParams { time_ms: 7.3, feedback: -0.8, damping_hz: Some(3000.0), mix: 0.4, ping_pong: true };
        let reference = run_blocks(&mut FeedbackDelay::new(RATE, 2, p).unwrap(), &stereo, RATE, &signal, &[30_000]);
        for blocks in [&[1usize][..], &[7, 255, 256, 257, 1000], &[4099]] {
            let got = run_blocks(&mut FeedbackDelay::new(RATE, 2, p).unwrap(), &stereo, RATE, &signal, blocks);
            assert_eq!(bits(&got[0]), bits(&reference[0]), "{blocks:?}");
            assert_eq!(bits(&got[1]), bits(&reference[1]), "{blocks:?}");
        }
        // reset() returns to the initial state: a second run on the same instance repeats the bits.
        let mut delay = FeedbackDelay::new(RATE, 2, p).unwrap();
        let _ = run_blocks(&mut delay, &stereo, RATE, &signal, &[512]);
        delay.reset();
        let again = run_blocks(&mut delay, &stereo, RATE, &signal, &[512]);
        assert_eq!(bits(&again[0]), bits(&reference[0]));

        // Tail bound: fb 0.5 -> m = ceil(ln 1e-6 / ln 0.5) = 20, tail = D * 21; past it every
        // sample is below 1e-6 and at echo 20 (index 20 D) the level is 0.5^19 > 1e-6.
        let mut delay = FeedbackDelay::new(RATE, 1, wet(10.0, 0.5)).unwrap();
        assert_eq!(delay.tail_frames(), 480 * 21);
        let out = run_blocks(&mut delay, &mono(), RATE, &[impulse(480 * 25)], &[4096]);
        assert!(out[0][480 * 20] > 1e-6);
        assert!(out[0][480 * 21..].iter().all(|v| v.abs() < 1e-6));
        assert_eq!(FeedbackDelay::new(RATE, 1, wet(10.0, 0.0)).unwrap().tail_frames(), 480);
    });
}

#[test]
fn hostile_parameters_budgets_and_blocks_are_refused() {
    bounded_case("delay_hostile", || {
        let ok = DelayParams::default();
        let err = |rate, ch, p| FeedbackDelay::new(rate, ch, p).unwrap_err();
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(err(RATE, 1, DelayParams { time_ms: v, ..ok }), ScoreError::NotFinite);
            assert_eq!(err(RATE, 1, DelayParams { feedback: v, ..ok }), ScoreError::NotFinite);
            assert_eq!(err(RATE, 1, DelayParams { mix: v, ..ok }), ScoreError::NotFinite);
            assert_eq!(err(RATE, 1, DelayParams { damping_hz: Some(v), ..ok }), ScoreError::NotFinite);
        }
        // 0 ms and 0.001 ms (0.048 samples) round to 0 samples; 5000.1 ms and 1e300 are above range.
        for t in [0.0, 0.001, -1.0, 5000.1, 1e300] {
            assert_eq!(err(RATE, 1, DelayParams { time_ms: t, ..ok }), ScoreError::OutOfRange, "{t}");
        }
        for f in [1.0, -1.0, 0.991, 2.0, -1e9] {
            assert_eq!(err(RATE, 1, DelayParams { feedback: f, ..ok }), ScoreError::OutOfRange, "{f}");
        }
        assert_eq!(err(RATE, 1, DelayParams { mix: 1.5, ..ok }), ScoreError::OutOfRange);
        for f in [10.0, 24_000.5, 1e12] {
            assert_eq!(err(RATE, 1, DelayParams { damping_hz: Some(f), ..ok }), ScoreError::OutOfRange, "{f}");
        }
        assert_eq!(err(RATE, 1, DelayParams { ping_pong: true, ..ok }), ScoreError::InvalidLayout);
        assert_eq!(err(RATE, 3, DelayParams { ping_pong: true, ..ok }), ScoreError::InvalidLayout);
        for ch in [0, 17, usize::MAX] {
            assert_eq!(err(RATE, ch, ok), ScoreError::InvalidLayout);
        }
        for rate in [0, 44_101, 1_000_000] {
            assert_eq!(err(rate, 1, ok), ScoreError::InvalidRate);
        }
        // Budget: 5 s at 768 kHz is 3.84e6 samples per channel; 16 channels = 61.4e6 > 8Mi,
        // refused before allocating; 2 channels (7.68e6) fit.
        let long = DelayParams { time_ms: 5000.0, ..ok };
        assert_eq!(err(768_000, 16, long), ScoreError::CapacityExceeded);
        assert_eq!(FeedbackDelay::new(768_000, 2, long).unwrap().delay_samples(), 3_840_000);

        // Engine: NaN/inf input, a block above MAX_BLOCK_FRAMES and a block above the output
        // capacity are refused; nothing is published.
        let mut delay = FeedbackDelay::new(RATE, 1, ok).unwrap();
        let (eng, token) = (engine(), CancellationToken::default());
        let mut out = OutputBlock::with_capacity(mono(), 64).unwrap();
        for bad in [f32::NAN, f32::NEG_INFINITY] {
            let mut x = vec![0.1_f32; 64];
            x[9] = bad;
            let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames: 64, start_sample: 0 };
            let slices: Vec<&[f32]> = vec![&x];
            let got = eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut delay, &mut out, &token);
            assert_eq!(got.unwrap_err(), ScoreError::NotFinite);
        }
        let x = vec![0.0_f32; 65];
        for frames in [MAX_BLOCK_FRAMES + 1, 65] {
            let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames, start_sample: 0 };
            let slices: Vec<&[f32]> = vec![&x];
            let got = eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut delay, &mut out, &token);
            assert_eq!(got.unwrap_err(), ScoreError::CapacityExceeded, "{frames}");
        }
        assert_eq!(out.published_frames(), 0);
        // Wrong rate block on a 48 kHz delay.
        let sp = BlockSpec { sample_rate_hz: 44_100, layout: mono(), frames: 64, start_sample: 0 };
        let slices: Vec<&[f32]> = vec![&x[..64]];
        let got = eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut delay, &mut out, &token);
        assert_eq!(got.unwrap_err(), ScoreError::InvalidRate);
    });
}
