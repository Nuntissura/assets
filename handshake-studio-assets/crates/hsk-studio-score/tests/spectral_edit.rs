//! SMB-SCORE-SPECTRAL slice 2: spectral selection with attenuate and heal; exact passthrough
//! outside the selection's affected samples.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;
use std::f64::consts::PI;

const N: usize = 2048;

fn lock_in(x: &[f64], f: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (n, v) in x.iter().enumerate() {
        let a = 2.0 * PI * f * n as f64 / f64::from(RATE);
        re += v * a.cos();
        im += v * a.sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

fn edit(input: &[f32], sel: SpectralSelection, op: SpectralOp) -> (Vec<f32>, SpectralEditReport) {
    let mut out = vec![0.0_f32; input.len()];
    let report = spectral_edit(input, &mut out, RATE, N, sel, op, &CancellationToken::default()).unwrap();
    (out, report)
}

#[test]
fn outside_the_affected_samples_the_output_is_the_input_bit_for_bit() {
    bounded_case("spectral_passthrough", || {
        let tone = sine(500.0, 0.3, 0.0, RATE, 48_000);
        let x: Vec<f32> = noise(2, 48_000).iter().zip(&tone).map(|(n, t)| 0.2 * n + t).collect();
        let sel = SpectralSelection { start_sample: 20_000, end_sample: 26_000, low_hz: 3000.0, high_hz: 6000.0 };
        for op in [SpectralOp::Attenuate { gain_db: -60.0 }, SpectralOp::Heal] {
            let (out, r) = edit(&x, sel, op);
            // Frames centred on 20_480 .. 25_600 (hop 512) edit samples 19_456 .. 26_624.
            assert_eq!((r.affected_start, r.affected_end, r.frames), (19_456, 26_624, 11));
            assert_eq!(r.cells, 11 * 129); // bins 128 (3000 Hz) ..= 256 (6000 Hz)
            assert_eq!(bits(&out[..19_456]), bits(&x[..19_456]));
            assert_eq!(bits(&out[26_624..]), bits(&x[26_624..]));
            // Inside, the 500 Hz tone (outside the band) is untouched: the difference holds no
            // 500 Hz component above 1e-7 of full scale.
            let diff: Vec<f64> = (19_456..26_624).map(|n| f64::from(out[n]) - f64::from(x[n])).collect();
            let leak = lock_in(&diff, 500.0);
            println!("{op:?}: 500 Hz in the difference {leak:.2e}");
            assert!(leak < 1e-7, "{op:?}: {leak}");
        }
    });
}

#[test]
fn attenuation_is_exact_full_band_and_band_limited_otherwise() {
    bounded_case("spectral_attenuate", || {
        let x = noise(3, 48_000);
        // Full band: interior samples (all four overlapping frames edited) become exactly g x.
        let sel = SpectralSelection { start_sample: 10_000, end_sample: 30_000, low_hz: 0.0, high_hz: 24_000.0 };
        let (out, _) = edit(&x, sel, SpectralOp::Attenuate { gain_db: -20.0 });
        let g = 0.1_f64;
        let worst = (11_264..28_672).map(|n| (f64::from(out[n]) - g * f64::from(x[n])).abs()).fold(0.0, f64::max);
        println!("full-band -20 dB: worst deviation from g x {worst:.2e}");
        assert!(worst < 1e-6, "{worst}");
        // Band 4..8 kHz at -20 dB, measured on the display grid in interior frames.
        let sel = SpectralSelection { start_sample: 10_000, end_sample: 30_000, low_hz: 4000.0, high_hz: 8000.0 };
        let (out, _) = edit(&x, sel, SpectralOp::Attenuate { gain_db: -20.0 });
        let mut s = Spectrogram::new(RATE, SpectrogramParams { fft_size: N, hop: 512 }).unwrap();
        let grid = |v: &[f32], s: &mut Spectrogram| {
            let frames = s.frame_count(v.len() as u64) as usize;
            let mut g = vec![0.0_f32; frames * 1025];
            s.tile(v, 0, v.len() as u64, 0, frames, &mut g).unwrap();
            g
        };
        let (gi, go) = (grid(&x, &mut s), grid(&out, &mut s));
        let energy = |g: &[f32], bins: std::ops::Range<usize>| (24..54).map(|k| bins.clone().map(|b| f64::from(g[k * 1025 + b]).powi(2)).sum::<f64>()).sum::<f64>();
        let inside = 10.0 * (energy(&go, 180..330) / energy(&gi, 180..330)).log10();
        let below = 10.0 * (energy(&go, 20..160) / energy(&gi, 20..160)).log10();
        let above = 10.0 * (energy(&go, 360..1000) / energy(&gi, 360..1000)).log10();
        println!("band attenuation: inside {inside:.3} dB, below {below:.4} dB, above {above:.4} dB");
        assert!((inside + 20.0).abs() < 0.3, "{inside}");
        assert!(below.abs() < 0.05 && above.abs() < 0.05, "{below} {above}");
    });
}

#[test]
fn heal_replaces_a_time_frequency_burst_by_its_surroundings() {
    bounded_case("spectral_heal", || {
        let frames = 48_000;
        let clean: Vec<f32> = sine(1000.0, 0.3, 0.0, RATE, frames).iter().zip(noise(5, frames)).map(|(t, n)| t + 0.01 * n).collect();
        // Burst: 4200 + 4600 Hz under a Hann envelope over 24_000 .. 26_400.
        let mut corrupted = clean.clone();
        for (n, v) in corrupted.iter_mut().enumerate().take(26_400).skip(24_000) {
            let e = 0.5 - 0.5 * (2.0 * PI * (n - 24_000) as f64 / 2400.0).cos();
            let t = n as f64 / f64::from(RATE);
            *v += (0.3 * e * ((2.0 * PI * 4200.0 * t).sin() + (2.0 * PI * 4600.0 * t).sin())) as f32;
        }
        let sel = SpectralSelection { start_sample: 23_000, end_sample: 27_400, low_hz: 3800.0, high_hz: 5000.0 };
        let (out, r) = edit(&corrupted, sel, SpectralOp::Heal);
        let span = r.affected_start as usize..r.affected_end as usize;
        let err = |y: &[f32]| span.clone().map(|n| (f64::from(y[n]) - f64::from(clean[n])).powi(2)).sum::<f64>();
        let improvement = 10.0 * (err(&corrupted) / err(&out)).log10();
        let diff: Vec<f64> = span.clone().map(|n| f64::from(out[n]) - f64::from(clean[n])).collect();
        let tone_err = lock_in(&diff, 1000.0);
        println!("heal: error reduced by {improvement:.2} dB; 1 kHz error {tone_err:.2e}");
        assert!(improvement > 20.0, "{improvement}");
        assert!(tone_err < 1e-4, "{tone_err}");
        assert_eq!(bits(&out[..span.start]), bits(&corrupted[..span.start]));
    });
}

#[test]
fn hostile_selections_and_parameters_are_refused() {
    bounded_case("spectral_hostile", || {
        let x = noise(1, 10_000);
        let mut out = vec![0.0_f32; 10_000];
        let ok = SpectralSelection { start_sample: 1000, end_sample: 2000, low_hz: 100.0, high_hz: 200.0 };
        let token = CancellationToken::default();
        let run = |x: &[f32], out: &mut [f32], fft, sel, op| spectral_edit(x, out, RATE, fft, sel, op, &token);
        let att = SpectralOp::Attenuate { gain_db: -6.0 };
        for sel in [
            SpectralSelection { end_sample: 1000, ..ok },
            SpectralSelection { end_sample: 10_001, ..ok },
            SpectralSelection { low_hz: -1.0, ..ok },
            SpectralSelection { high_hz: 50.0, ..ok },
            SpectralSelection { high_hz: 24_001.0, ..ok },
        ] {
            assert_eq!(run(&x, &mut out, N, sel, att), Err(ScoreError::OutOfRange), "{sel:?}");
        }
        assert_eq!(run(&x, &mut out, N, SpectralSelection { low_hz: f64::NAN, ..ok }, att), Err(ScoreError::NotFinite));
        assert_eq!(run(&x, &mut out, N, ok, SpectralOp::Attenuate { gain_db: f64::INFINITY }), Err(ScoreError::NotFinite));
        assert_eq!(run(&x, &mut out, N, ok, SpectralOp::Attenuate { gain_db: 3.0 }), Err(ScoreError::OutOfRange));
        assert_eq!(run(&x, &mut out, 1000, ok, att), Err(ScoreError::OutOfRange));
        assert_eq!(run(&x, &mut out[..9], N, ok, att), Err(ScoreError::LengthMismatch));
        assert_eq!(spectral_edit(&x, &mut out, 44_101, N, ok, att, &token), Err(ScoreError::InvalidRate));
        let mut bad = x.clone();
        bad[3] = f32::NAN;
        assert_eq!(run(&bad, &mut out, N, ok, att), Err(ScoreError::NotFinite));
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(spectral_edit(&x, &mut out, RATE, N, ok, att, &canceled), Err(ScoreError::Canceled));
        // A band between bins selects nothing and copies the input.
        let r = run(&x, &mut out, N, SpectralSelection { low_hz: 101.0, high_hz: 105.0, ..ok }, att).unwrap();
        assert_eq!((r.cells, bits(&out) == bits(&x)), (0, true));
    });
}
