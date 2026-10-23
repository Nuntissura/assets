//! SMB-SCORE-EFFECTS-3 slice 3: click/crackle repair (AR detection + LSAR interpolation) and hum
//! removal. Click results are measured against the known clean signal (latency compensated).
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;
use std::f64::consts::PI;

const L: usize = CLICK_HOP + CLICK_CONTEXT;

/// Three tones plus low-level noise (about -50 dB), faded in over 1000 samples: an abrupt start
/// after the stream's leading silence is itself an impulsive event the detector would flag.
fn music(frames: usize) -> Vec<f32> {
    let a = sine(440.0, 0.3, 0.0, RATE, frames);
    let b = sine(1234.0, 0.2, 0.7, RATE, frames);
    let c = sine(3100.0, 0.1, 1.9, RATE, frames);
    let n = noise(77, frames);
    (0..frames)
        .map(|i| {
            let fade = (0.5 - 0.5 * (PI * (i.min(1000) as f64) / 1000.0).cos()) as f32;
            fade * (a[i] + b[i] + c[i] + 0.003 * n[i])
        })
        .collect()
}

/// Adds clicks `(start, len, amplitude)` of random sign/shape; returns the corrupted copy.
fn with_clicks(clean: &[f32], clicks: &[(usize, usize, f32)]) -> Vec<f32> {
    let mut out = clean.to_vec();
    let shape = noise(5, 4096);
    for (i, (start, len, amp)) in clicks.iter().enumerate() {
        for k in 0..*len {
            let s = shape[(i * 37 + k) % shape.len()];
            out[start + k] += amp * if s >= 0.0 { 1.0 } else { -1.0 } * (0.5 + s.abs());
        }
    }
    out
}

fn energy(x: impl Iterator<Item = f64>) -> f64 {
    x.map(|v| v * v).sum()
}

#[test]
fn clean_material_passes_bit_exact_with_the_documented_latency() {
    bounded_case("click_clean", || {
        let clean = music(2 * RATE as usize);
        let mut cr = ClickRepair::new(RATE, 1, ClickRepairParams::default()).unwrap();
        assert_eq!(cr.latency_samples() as usize, L);
        let out = run_blocks(&mut cr, &mono(), RATE, std::slice::from_ref(&clean), &[1000]);
        assert_eq!(cr.repaired_gaps(), 0);
        assert!(out[0][..L].iter().all(|v| *v == 0.0));
        assert_eq!(bits(&out[0][L..]), bits(&clean[..clean.len() - L]));
    });
}

#[test]
fn clicks_are_detected_and_interpolated() {
    bounded_case("click_repair", || {
        let frames = 2 * RATE as usize;
        let clean = music(frames);
        let clicks: Vec<(usize, usize, f32)> =
            (0..30).map(|i| (3000 + i * 2900, 1 + i % 12, 0.3 + 0.02 * i as f32)).collect();
        let corrupted = with_clicks(&clean, &clicks);
        let mut cr = ClickRepair::new(RATE, 1, ClickRepairParams::default()).unwrap();
        let out = run_blocks(&mut cr, &mono(), RATE, std::slice::from_ref(&corrupted), &[777]);
        assert_eq!((cr.repaired_gaps(), cr.skipped_gaps()), (30, 0));
        let valid = frames - L;
        let before = energy((0..valid).map(|n| f64::from(corrupted[n]) - f64::from(clean[n])));
        let after = energy((0..valid).map(|n| f64::from(out[0][n + L]) - f64::from(clean[n])));
        let improvement = 10.0 * (before / after).log10();
        println!("click SNR improvement {improvement:.2} dB");
        // Measured 28.62 dB at f423b59; bound set with margin from that run.
        assert!(improvement > 25.0, "{improvement}");
        // Every click region is close to the clean signal afterwards (clicks were 0.15..1.4 in
        // size; the signal peak is about 0.6).
        let worst = clicks
            .iter()
            .flat_map(|(start, len, _)| *start..start + len)
            .map(|n| (out[0][n + L] - clean[n]).abs())
            .fold(0.0, f32::max);
        println!("worst repaired-sample error {worst:.5}");
        assert!(worst < 0.05, "{worst}");
    });
}

#[test]
fn long_gaps_are_skipped_or_repaired_by_the_max_gap_setting() {
    bounded_case("click_long_gap", || {
        let frames = RATE as usize;
        let clean = music(frames);
        let corrupted = with_clicks(&clean, &[(20_000, 80, 0.5)]);
        // 1 ms = 48 samples: the 80-sample burst is detected but left untouched.
        let mut cr = ClickRepair::new(RATE, 1, ClickRepairParams::default()).unwrap();
        let out = run_blocks(&mut cr, &mono(), RATE, std::slice::from_ref(&corrupted), &[4096]);
        assert_eq!((cr.repaired_gaps(), cr.skipped_gaps()), (0, 1));
        assert_eq!(bits(&out[0][20_000 + L..20_080 + L]), bits(&corrupted[20_000..20_080]));
        // 2.5 ms = 120 samples at 48 kHz: repaired; the gap error falls below 5 percent of the
        // signal RMS.
        let p = ClickRepairParams { max_gap_ms: 2.5, ..ClickRepairParams::default() };
        let mut cr = ClickRepair::new(RATE, 1, p).unwrap();
        assert_eq!(cr.max_gap_samples(), 120);
        let out = run_blocks(&mut cr, &mono(), RATE, &[corrupted], &[4096]);
        assert_eq!(cr.repaired_gaps(), 1);
        let err = (energy((20_000..20_080).map(|n| f64::from(out[0][n + L]) - f64::from(clean[n]))) / 80.0).sqrt();
        println!("80-sample gap error rms {err:.5} (signal rms {:.4})", rms(&clean));
        assert!(err < 0.05 * rms(&clean), "{err}");
        // Sample-rate aware: the same 2.5 ms is 110 samples at 44.1 kHz (110.25 rounds down).
        assert_eq!(ClickRepair::new(44_100, 1, p).unwrap().max_gap_samples(), 110);
    });
}

#[test]
fn hum_notches_remove_the_harmonics_and_follow_the_analytic_passband() {
    bounded_case("hum_removal", || {
        let frames = 4 * RATE as usize;
        let tone = sine(440.0, 0.3, 0.0, RATE, frames);
        let mut hum = vec![0.0_f32; frames];
        for k in 1..=5 {
            let h = sine(50.0 * f64::from(k), 0.1 / f64::from(k), 0.3 * f64::from(k), RATE, frames);
            hum.iter_mut().zip(h).for_each(|(a, b)| *a += b);
        }
        let input: Vec<f32> = tone.iter().zip(&hum).map(|(a, b)| a + b).collect();
        let p = HumParams { fundamental_hz: 50.0, harmonics: 5, bandwidth_hz: 2.0 };
        let mut hr = HumRemover::new(RATE, 1, p).unwrap();
        assert_eq!(hr.notches(), 5);
        let out = run_blocks(&mut hr, &mono(), RATE, std::slice::from_ref(&input), &[1000]);
        let tail = &out[0][3 * RATE as usize..];
        let amp = |x: &[f32], f: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (n, v) in x.iter().enumerate() {
                let (s, c) = (2.0 * PI * f * n as f64 / f64::from(RATE)).sin_cos();
                re += f64::from(*v) * c;
                im += f64::from(*v) * s;
            }
            2.0 * (re * re + im * im).sqrt() / x.len() as f64
        };
        for k in 1..=5 {
            let a = amp(tail, 50.0 * f64::from(k));
            assert!(a < 1e-5, "harmonic {k}: {a}");
        }
        // The 440 Hz tone passes with the cascade's analytic magnitude.
        let want = 0.3 * hr.magnitude(440.0);
        assert!((amp(tail, 440.0) - want).abs() < 1e-5);
        assert!((hr.magnitude(440.0) - 1.0).abs() < 1e-3);
        // Harmonics at or above 0.95 Nyquist are dropped: 1 kHz x 16 at 32 kHz keeps 15 (15.2 kHz).
        let hr32 = HumRemover::new(32_000, 1, HumParams { fundamental_hz: 1000.0, harmonics: 16, bandwidth_hz: 5.0 }).unwrap();
        assert_eq!(hr32.notches(), 15);

        // Partition independence for both restoration processors (stereo).
        let stereo = ChannelLayout::stereo();
        let signal = vec![with_clicks(&music(30_000), &[(5000, 4, 0.5), (17_000, 9, -0.4)]), input[..30_000].to_vec()];
        type Maker = fn() -> Box<dyn Processor>;
        let makers: [Maker; 2] = [
            || Box::new(ClickRepair::new(RATE, 2, ClickRepairParams::default()).unwrap()),
            || Box::new(HumRemover::new(RATE, 2, HumParams::default()).unwrap()),
        ];
        for make in makers {
            let reference = run_blocks(make().as_mut(), &stereo, RATE, &signal, &[30_000]);
            for blocks in [&[1usize][..], &[7, 255, 256, 257, 1000], &[4099]] {
                let got = run_blocks(make().as_mut(), &stereo, RATE, &signal, blocks);
                assert_eq!(bits(&got[0]), bits(&reference[0]), "{blocks:?}");
                assert_eq!(bits(&got[1]), bits(&reference[1]), "{blocks:?}");
            }
        }
    });
}

#[test]
fn hostile_parameters_blocks_and_undeclared_spec_rows_are_refused() {
    bounded_case("restore_hostile", || {
        let ok = ClickRepairParams::default();
        for v in [f64::NAN, f64::INFINITY] {
            assert_eq!(ClickRepair::new(RATE, 1, ClickRepairParams { threshold: v, ..ok }).unwrap_err(), ScoreError::NotFinite);
            assert_eq!(ClickRepair::new(RATE, 1, ClickRepairParams { max_gap_ms: v, ..ok }).unwrap_err(), ScoreError::NotFinite);
            assert_eq!(HumRemover::new(RATE, 1, HumParams { fundamental_hz: v, ..HumParams::default() }).unwrap_err(), ScoreError::NotFinite);
            assert_eq!(HumRemover::new(RATE, 1, HumParams { bandwidth_hz: v, ..HumParams::default() }).unwrap_err(), ScoreError::NotFinite);
        }
        let click_bad = [
            ClickRepairParams { ar_order: 3, ..ok },
            ClickRepairParams { ar_order: 65, ..ok },
            ClickRepairParams { ar_order: usize::MAX, ..ok },
            ClickRepairParams { threshold: 1.0, ..ok },
            ClickRepairParams { threshold: 1e9, ..ok },
            ClickRepairParams { max_gap_ms: 0.0, ..ok },
            ClickRepairParams { max_gap_ms: 2.7, ..ok },
            ClickRepairParams { max_gap_ms: 1e300, ..ok },
            ClickRepairParams { max_gap_ms: -1.0, ..ok },
        ];
        for p in click_bad {
            assert_eq!(ClickRepair::new(RATE, 1, p).unwrap_err(), ScoreError::OutOfRange, "{p:?}");
        }
        // 1 ms is 768 samples at 768 kHz: above the 128-sample bound.
        assert_eq!(ClickRepair::new(768_000, 1, ok).unwrap_err(), ScoreError::OutOfRange);
        let hum_bad = [
            HumParams { fundamental_hz: 19.0, ..HumParams::default() },
            HumParams { fundamental_hz: 1001.0, ..HumParams::default() },
            HumParams { bandwidth_hz: 0.0, ..HumParams::default() },
            HumParams { harmonics: 0, ..HumParams::default() },
            HumParams { harmonics: 17, ..HumParams::default() },
        ];
        for p in hum_bad {
            assert_eq!(HumRemover::new(RATE, 1, p).unwrap_err(), ScoreError::OutOfRange, "{p:?}");
        }
        for c in [0, 17] {
            assert_eq!(ClickRepair::new(RATE, c, ok).unwrap_err(), ScoreError::InvalidLayout);
            assert_eq!(HumRemover::new(RATE, c, HumParams::default()).unwrap_err(), ScoreError::InvalidLayout);
        }
        assert_eq!(ClickRepair::new(44_101, 1, ok).unwrap_err(), ScoreError::InvalidRate);

        let mut cr = ClickRepair::new(RATE, 1, ok).unwrap();
        let (eng, token) = (engine(), CancellationToken::default());
        let mut out = OutputBlock::with_capacity(mono(), 64).unwrap();
        let mut x = vec![0.1_f32; 65];
        for frames in [MAX_BLOCK_FRAMES + 1, 65] {
            let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames, start_sample: 0 };
            let slices: Vec<&[f32]> = vec![&x];
            assert_eq!(eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut cr, &mut out, &token).unwrap_err(), ScoreError::CapacityExceeded);
        }
        x[1] = f32::NAN;
        let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames: 64, start_sample: 0 };
        let slices: Vec<&[f32]> = vec![&x[..64]];
        assert_eq!(eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut cr, &mut out, &token).unwrap_err(), ScoreError::NotFinite);
        assert_eq!(out.published_frames(), 0);
        // Digital silence and a full-scale square never produce NaN (degenerate AR fits are skipped).
        let square: Vec<f32> = (0..20_000).map(|n| if (n / 50) % 2 == 0 { 1.0 } else { -1.0 }).collect();
        for s in [vec![0.0_f32; 20_000], square] {
            let o = run_blocks(&mut ClickRepair::new(RATE, 1, ok).unwrap(), &mono(), RATE, &[s], &[2048]);
            assert!(o[0].iter().all(|v| v.is_finite()));
        }

        for p in ["inf_threshold", "inf_complexity"] {
            assert!(matches!(bind_spec_row("AutoClickRemover", p), Err(SpecRowError::Undeclared(_))), "{p}");
        }
        assert_eq!(bind_spec_row("DeHummer", "anything"), Err(SpecRowError::UnknownRow));
    });
}
