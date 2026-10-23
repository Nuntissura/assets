//! SMB-AU-WAVEFORM-EDITOR slice 3: marquee / lasso spectral selection with attenuate, remove and
//! heal by inverse STFT with weighted overlap-add. Oracles: perfect reconstruction of the identity
//! edit (error bound), exact copy outside the affected samples, a hand-derived interior gain, a
//! brute-force cell count of a triangle, and lock-in amplitudes against the clean signal.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;
use std::f64::consts::PI;

const N: usize = 2048;

fn lock_in(x: &[f32], f: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (n, v) in x.iter().enumerate() {
        let a = 2.0 * PI * f * n as f64 / f64::from(RATE);
        re += f64::from(*v) * a.cos();
        im += f64::from(*v) * a.sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

fn run(x: &[f32], window: SpectralWindow, region: &SpectralRegion, op: SpectralMaskOp) -> (Vec<f32>, SpectralMaskReport) {
    let mut out = vec![0.0_f32; x.len()];
    let r = spectral_mask_edit(x, &mut out, RATE, N, window, region, op, &CancellationToken::default()).unwrap();
    (out, r)
}

#[test]
fn identity_edit_reconstructs_and_lasso_cells_match_a_brute_force_count() {
    bounded_case("spectral_mask_identity", || {
        let x = noise(3, 48_000);
        let marquee = SpectralRegion::Marquee { start_sample: 20_000, end_sample: 26_000, low_hz: 0.0, high_hz: 24_000.0 };
        // Triangle (10100, 100 Hz), (30100, 100 Hz), (10100, 12100 Hz): no grid point on an edge.
        let lasso = SpectralRegion::Lasso(vec![(10_100.0, 100.0), (30_100.0, 100.0), (10_100.0, 12_100.0)]);
        for window in [SpectralWindow::Hann, SpectralWindow::Hamming, SpectralWindow::Blackman, SpectralWindow::BlackmanHarris, SpectralWindow::Rectangular] {
            for region in [&marquee, &lasso] {
                let (out, r) = run(&x, window, region, SpectralMaskOp::Identity);
                let (a, b) = (r.affected_start as usize, r.affected_end as usize);
                assert_eq!(bits(&out[..a]), bits(&x[..a]));
                assert_eq!(bits(&out[b..]), bits(&x[b..]));
                let err = out[a..b].iter().zip(&x[a..b]).map(|(o, i)| (f64::from(*o) - f64::from(*i)).abs()).fold(0.0, f64::max);
                assert!(err <= 1e-6, "{window:?} {region:?}: reconstruction error {err}");
            }
        }
        // Marquee geometry: frames centred 20480 ..= 25600 (hop 512), samples 19456 .. 26624.
        let (_, r) = run(&x, SpectralWindow::Hann, &marquee, SpectralMaskOp::Identity);
        assert_eq!((r.affected_start, r.affected_end, r.frames, r.cells), (19_456, 26_624, 11, 11 * 1025));
        // Lasso cells: brute-force count of grid points strictly inside the triangle.
        let (_, r) = run(&x, SpectralWindow::Hann, &lasso, SpectralMaskOp::Remove);
        let bin_hz = f64::from(RATE) / N as f64;
        let mut count = 0u64;
        for k in 0..=94u64 {
            for b in 0..=1024u64 {
                let (t, f) = ((k * 512) as f64, b as f64 * bin_hz);
                if t > 10_100.0 && f > 100.0 && (t - 10_100.0) / 20_000.0 + (f - 100.0) / 12_000.0 < 1.0 {
                    count += 1;
                }
            }
        }
        assert!(count > 1000);
        assert_eq!(r.cells, count);
        assert_eq!((r.affected_start, r.affected_end), (20 * 512 - 1024, 58 * 512 + 1024)); // frames 20 ..= 58
    });
}

#[test]
fn attenuate_scales_the_interior_and_remove_clears_a_band() {
    bounded_case("spectral_mask_attenuate_remove", || {
        let x = noise(5, 48_000);
        let full = SpectralRegion::Marquee { start_sample: 20_000, end_sample: 26_000, low_hz: 0.0, high_hz: 24_000.0 };
        let (out, _) = run(&x, SpectralWindow::Hann, &full, SpectralMaskOp::Attenuate { gain_db: -20.0 });
        // Samples 22000 ..= 24000 are reached only by edited frames: y = 10^(-1) x.
        for n in 22_000..=24_000 {
            assert!((f64::from(out[n]) - 0.1 * f64::from(x[n])).abs() < 1e-6, "n {n}");
        }
        let low = sine(1000.0, 0.3, 0.0, RATE, 48_000);
        let high = sine(6000.0, 0.3, 0.5, RATE, 48_000);
        let x: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
        let band = SpectralRegion::Marquee { start_sample: 10_000, end_sample: 38_000, low_hz: 5000.0, high_hz: 7000.0 };
        let (out, _) = run(&x, SpectralWindow::Hann, &band, SpectralMaskOp::Remove);
        let (s, e) = (16_000, 32_000);
        let before = lock_in(&x[s..e], 6000.0);
        let after = lock_in(&out[s..e], 6000.0);
        assert!(20.0 * (after / before).log10() < -60.0, "6 kHz {before} -> {after}");
        // Against the 1 kHz tone alone (out - x would hold the removed 6 kHz tone, whose rectangular
        // lock-in leakage at 1 kHz over 16000 samples is about 6e-5 and not a disturbance).
        let diff: Vec<f32> = out[s..e].iter().zip(&low[s..e]).map(|(o, l)| o - l).collect();
        assert!(lock_in(&diff, 1000.0) < 1e-5, "1 kHz disturbed by {}", lock_in(&diff, 1000.0));
    });
}

#[test]
fn heal_fills_a_lasso_defect_from_its_surroundings() {
    bounded_case("spectral_mask_heal", || {
        let tone = sine(1000.0, 0.3, 0.0, RATE, 48_000);
        let mut x = tone.clone();
        let burst_a = sine(4200.0, 0.4, 0.0, RATE, 2000);
        let burst_b = sine(4600.0, 0.4, 0.7, RATE, 2000);
        for i in 0..2000 {
            x[20_000 + i] += burst_a[i] + burst_b[i];
        }
        // A diamond around the burst (samples, Hz).
        let lasso = SpectralRegion::Lasso(vec![(18_500.0, 4400.0), (21_000.0, 3000.0), (23_500.0, 4400.0), (21_000.0, 5800.0)]);
        let (out, r) = run(&x, SpectralWindow::Hann, &lasso, SpectralMaskOp::Heal);
        assert!(r.cells > 0 && r.one_sided == 0, "{r:?}");
        let (s, e) = (19_500, 22_500);
        let resid = |y: &[f32]| -> f64 {
            let d: Vec<f32> = y[s..e].iter().zip(&tone[s..e]).map(|(a, b)| a - b).collect();
            lock_in(&d, 4200.0).max(lock_in(&d, 4600.0))
        };
        let gain_db = 20.0 * (resid(&out) / resid(&x)).log10();
        assert!(gain_db < -20.0, "burst reduced by {gain_db} dB only");
        let d1: Vec<f32> = out[s..e].iter().zip(&x[s..e]).map(|(a, b)| a - b).collect();
        assert!(lock_in(&d1, 1000.0) < 1e-4, "1 kHz disturbed by {}", lock_in(&d1, 1000.0));
    });
}

#[test]
fn hostile_regions_and_parameters_are_refused_and_change_nothing() {
    bounded_case("spectral_mask_hostile", || {
        let x = noise(7, 20_000);
        let t = CancellationToken::default();
        let ok = SpectralRegion::Marquee { start_sample: 1000, end_sample: 5000, low_hz: 100.0, high_hz: 1000.0 };
        let call = |input: &[f32], rate: u32, fft: usize, region: &SpectralRegion, op: SpectralMaskOp, c: &CancellationToken| {
            let mut out = input.to_vec();
            out.iter_mut().for_each(|v| *v = 0.25);
            let r = spectral_mask_edit(input, &mut out, rate, fft, SpectralWindow::Hann, region, op, c).err();
            (r, out)
        };
        let lasso = |p: Vec<(f64, f64)>| SpectralRegion::Lasso(p);
        use ScoreError::*;
        let cases: Vec<(Option<ScoreError>, Option<ScoreError>)> = vec![
            (call(&x, 44_101, N, &ok, SpectralMaskOp::Remove, &t).0, Some(InvalidRate)),
            (call(&x, RATE, 3000, &ok, SpectralMaskOp::Remove, &t).0, Some(OutOfRange)),
            (call(&x, RATE, N, &lasso(vec![(0.0, 0.0), (10.0, 10.0)]), SpectralMaskOp::Remove, &t).0, Some(OutOfRange)),
            (call(&x, RATE, N, &lasso(vec![(1.0, 1.0); MAX_LASSO_POINTS + 1]), SpectralMaskOp::Remove, &t).0, Some(OutOfRange)),
            (call(&x, RATE, N, &lasso(vec![(0.0, 0.0), (10.0, f64::NAN), (5.0, 5.0)]), SpectralMaskOp::Remove, &t).0, Some(NotFinite)),
            (call(&x, RATE, N, &lasso(vec![(0.0, 0.0), (10.0, 24_001.0), (5.0, 5.0)]), SpectralMaskOp::Remove, &t).0, Some(OutOfRange)),
            (call(&x, RATE, N, &lasso(vec![(0.0, 0.0), (20_001.0, 10.0), (5.0, 5.0)]), SpectralMaskOp::Remove, &t).0, Some(OutOfRange)),
            (call(&x, RATE, N, &SpectralRegion::Marquee { start_sample: 0, end_sample: 20_001, low_hz: 0.0, high_hz: 1.0 }, SpectralMaskOp::Remove, &t).0, Some(OutOfRange)),
            (call(&x, RATE, N, &SpectralRegion::Marquee { start_sample: 5, end_sample: 5, low_hz: 0.0, high_hz: 1.0 }, SpectralMaskOp::Remove, &t).0, Some(OutOfRange)),
            (call(&x, RATE, N, &ok, SpectralMaskOp::Attenuate { gain_db: 1.0 }, &t).0, Some(OutOfRange)),
            (call(&x, RATE, N, &ok, SpectralMaskOp::Attenuate { gain_db: f64::INFINITY }, &t).0, Some(NotFinite)),
            (call(&[0.0, f32::NAN, 0.0, 0.0, 0.0, 0.0], RATE, 256, &SpectralRegion::Marquee { start_sample: 0, end_sample: 6, low_hz: 0.0, high_hz: 1.0 }, SpectralMaskOp::Remove, &t).0, Some(NotFinite)),
        ];
        for (i, (got, want)) in cases.iter().enumerate() {
            assert_eq!(got, want, "case {i}");
        }
        let mut short = vec![0.0_f32; 10];
        assert_eq!(spectral_mask_edit(&x, &mut short, RATE, N, SpectralWindow::Hann, &ok, SpectralMaskOp::Remove, &t).err(), Some(LengthMismatch));
        // Cell budget: N = 16384 (8193 bins) over 2^22 samples needs > 2^23 cells.
        let long = vec![0.0_f32; 1 << 22];
        let all = SpectralRegion::Marquee { start_sample: 0, end_sample: 1 << 22, low_hz: 0.0, high_hz: 24_000.0 };
        let mut out = vec![0.0_f32; 1 << 22];
        assert_eq!(spectral_mask_edit(&long, &mut out, RATE, 16_384, SpectralWindow::Hann, &all, SpectralMaskOp::Remove, &t).err(), Some(CapacityExceeded));
        // Cancel: out is the input.
        let c = CancellationToken::default();
        c.cancel();
        let (r, out) = call(&x, RATE, N, &ok, SpectralMaskOp::Heal, &c);
        assert_eq!(r, Some(Canceled));
        assert_eq!(bits(&out), bits(&x));
    });
}
