//! SMB-SCORE-DSP slice 2: BS.1770-4 / R128 loudness meter. Expected values come from the
//! standard's closed-form definitions (filter response, power mean, gating arithmetic), not from
//! the meter. The EBU Tech 3341/3342 test files could not be downloaded (see lane notes):
//! the EBU-vector claim is NOT_PROVEN.
mod common_dsp;
use common_dsp::*;
use hsk_studio_score::*;
use std::f64::consts::PI;

/// |H| of a biquad `(b, a)` (a without leading 1) at `hz`.
fn magnitude(b: [f64; 3], a: [f64; 2], hz: f64, rate: f64) -> f64 {
    let w = 2.0 * PI * hz / rate;
    let z1 = (w.cos(), -w.sin());
    let z2 = ((2.0 * w).cos(), -(2.0 * w).sin());
    let num = (b[0] + b[1] * z1.0 + b[2] * z2.0, b[1] * z1.1 + b[2] * z2.1);
    let den = (1.0 + a[0] * z1.0 + a[1] * z2.0, a[0] * z1.1 + a[1] * z2.1);
    num.0.hypot(num.1) / den.0.hypot(den.1)
}
fn k_gain(rate: u32, hz: f64) -> f64 {
    let (sb, sa, rb, ra) = k_weighting_coefficients(rate);
    magnitude(sb, sa, hz, f64::from(rate)) * magnitude(rb, ra, hz, f64::from(rate))
}
fn meter_for(rate: u32, signal: &[Vec<f32>], chunk: usize) -> LoudnessSnapshot {
    let mut meter = LoudnessMeter::unweighted(rate, signal.len()).unwrap();
    let frames = signal[0].len();
    let mut at = 0;
    while at < frames {
        let end = (at + chunk).min(frames);
        let slices: Vec<&[f32]> = signal.iter().map(|c| &c[at..end]).collect();
        meter.feed(&slices).unwrap();
        at = end;
    }
    meter.snapshot()
}
/// Stereo sine segments `(seconds, loudness_lufs)`: both channels carry the same 997 Hz sine whose
/// amplitude gives exactly that BS.1770 loudness (two equal channels: z = A^2 |K|^2).
fn segments(rate: u32, parts: &[(f64, f64)]) -> Vec<Vec<f32>> {
    let k = k_gain(rate, 997.0);
    let mut channel = Vec::new();
    for &(seconds, lufs) in parts {
        // L = -0.691 + 10 log10(2 * A^2/2 * k^2) -> A = 10^((L + 0.691)/20) / k
        let amplitude = 10f64.powf((lufs + 0.691) / 20.0) / k;
        let frames = (seconds * f64::from(rate)) as usize;
        let offset = channel.len();
        channel.extend((0..frames).map(|n| (amplitude * (2.0 * PI * 997.0 * (offset + n) as f64 / f64::from(rate)).sin()) as f32));
    }
    vec![channel.clone(), channel]
}

#[test]
fn k_weighting_matches_the_standard_table_and_calibration_holds() {
    bounded_case("k_weighting", || {
        // BS.1770-4 Table 1/2 (48 kHz) values, transcribed; the meter derives them from the analogue
        // prototypes (bilinear transform), so equality to ~1e-12 is independent evidence.
        let (sb, sa, rb, ra) = k_weighting_coefficients(48_000);
        let close = |x: f64, y: f64| (x - y).abs() < 1e-10;
        assert!(close(sb[0], 1.53512485958697) && close(sb[1], -2.69169618940638) && close(sb[2], 1.19839281085285));
        assert!(close(sa[0], -1.69065929318241) && close(sa[1], 0.73248077421585));
        assert!(rb == [1.0, -2.0, 1.0]);
        assert!(close(ra[0], -1.99004745483398) && close(ra[1], 0.99007225036621));

        // Calibration: one channel, 0 dBFS 997 Hz sine -> -3.01 LKFS (BS.1770-4 §2); every rate.
        for rate in [44_100, 48_000, 96_000] {
            let tone = vec![sine(997.0, 1.0, 0.0, rate, 6 * rate as usize)];
            let snap = meter_for(rate, &tone, 4096);
            let expected = -0.691 + 10.0 * (0.5 * k_gain(rate, 997.0).powi(2)).log10();
            assert!((expected - (-3.01)).abs() < 0.02, "rate {rate}: analytic {expected}");
            for v in [snap.momentary_lufs, snap.short_term_lufs, snap.integrated_lufs] {
                let v = v.unwrap();
                assert!((v - expected).abs() < 0.01, "rate {rate}: {v} vs {expected}");
            }
            assert_eq!(snap.frames, 6 * u64::from(rate));
            assert!(snap.loudness_range_lu.unwrap() < 0.05);
        }
        // Stereo -23 LUFS by construction reads -23.
        let snap = meter_for(48_000, &segments(48_000, &[(20.0, -23.0)]), 1000);
        assert!((snap.integrated_lufs.unwrap() + 23.0).abs() < 0.01);
        assert!((snap.momentary_lufs.unwrap() + 23.0).abs() < 0.01);
        assert!((snap.short_term_lufs.unwrap() + 23.0).abs() < 0.01);
    });
}

#[test]
fn gating_and_loudness_range_follow_the_definitions() {
    bounded_case("gating", || {
        let rate = 48_000;
        // A loud programme with a long quiet passage and a near-silent one: the quiet passages
        // sit under the relative gate (-20 - 10 = -30) or the absolute gate (-70) and must not move
        // the integrated value.
        let gated = segments(rate, &[(30.0, -20.0), (30.0, -45.0), (30.0, -80.0), (30.0, -20.0)]);
        let snap = meter_for(rate, &gated, 8192);
        assert!((snap.integrated_lufs.unwrap() + 20.0).abs() < 0.05, "{:?}", snap.integrated_lufs);
        // Two levels both above the relative gate: integrated is their power mean.
        let two = segments(rate, &[(30.0, -20.0), (30.0, -28.0)]);
        let snap = meter_for(rate, &two, 5000);
        let mean = 10.0 * ((10f64.powf(-2.0) + 10f64.powf(-2.8)) / 2.0).log10();
        assert!((snap.integrated_lufs.unwrap() - mean).abs() < 0.1, "{:?} vs {mean}", snap.integrated_lufs);
        // Loudness range of two 30 s plateaus 10 LU apart: 95th percentile at the loud plateau,
        // 10th percentile at the quiet one -> 10 LU (transition windows are < 10 percent of values).
        let steps = segments(rate, &[(30.0, -20.0), (30.0, -30.0)]);
        let snap = meter_for(rate, &steps, 4800);
        assert!((snap.loudness_range_lu.unwrap() - 10.0).abs() < 0.3, "{:?}", snap.loudness_range_lu);
        assert!((snap.max_momentary_lufs.unwrap() + 20.0).abs() < 0.05);
        assert!((snap.max_short_term_lufs.unwrap() + 20.0).abs() < 0.05);
        // Not enough audio yet: documented None values, never a fabricated number.
        let short = meter_for(rate, &segments(rate, &[(0.3, -23.0)]), 4800);
        assert!(short.momentary_lufs.is_none() && short.integrated_lufs.is_none() && short.short_term_lufs.is_none());
        let two_s = meter_for(rate, &segments(rate, &[(2.0, -23.0)]), 4800);
        assert!(two_s.momentary_lufs.is_some() && two_s.short_term_lufs.is_none() && two_s.loudness_range_lu.is_none());
        // Digital silence: -inf momentary, nothing above the absolute gate.
        let silence = meter_for(rate, &[vec![0.0; 5 * rate as usize]], 4800);
        assert_eq!(silence.momentary_lufs, Some(f64::NEG_INFINITY));
        assert!(silence.integrated_lufs.is_none() && silence.gated_blocks == 0);
        assert_eq!(silence.true_peak_dbtp, vec![f64::NEG_INFINITY]);
    });
}

#[test]
fn true_peak_sees_what_the_sample_peak_misses() {
    bounded_case("true_peak", || {
        let rate = 48_000;
        // A hard-onset tone has a real Gibbs overshoot at its start (measured +0.10 dB for the
        // unshaped fs/4 tone), so the tones are Hann-shaped: the continuous peak is then the
        // envelope maximum, 1.0 at the centre to within the envelope slope.
        let shaped = |hz: f64, amplitude: f64| -> Vec<Vec<f32>> {
            let frames = 9600;
            let mut t = sine(hz, amplitude, PI / 4.0, rate, frames);
            for (n, v) in t.iter_mut().enumerate() {
                *v *= (0.5 - 0.5 * (2.0 * PI * (n as f64 + 0.5) / frames as f64).cos()) as f32;
            }
            vec![t]
        };
        // fs/4 sine, 45 degrees: samples are +/-0.7071 (-3.01 dBFS), the continuous peak is 1.0.
        let tone = shaped(f64::from(rate) / 4.0, 1.0);
        let snap = meter_for(rate, &tone, 1000);
        assert!(snap.sample_peak_dbfs[0] < -3.0 && snap.sample_peak_dbfs[0] > -3.1);
        assert!(snap.true_peak_dbtp[0] > -0.1 && snap.true_peak_dbtp[0] < 0.05, "fs/4: {} dBTP", snap.true_peak_dbtp[0]);
        // Same tone at -6 dBFS amplitude reads -6 dBTP (Tech 3341 style: tolerance +0.2/-0.4 dB).
        let quiet = shaped(f64::from(rate) / 4.0, 0.5);
        let snap = meter_for(rate, &quiet, 1000);
        assert!((snap.true_peak_dbtp[0] + 6.0206).abs() < 0.2, "{}", snap.true_peak_dbtp[0]);
        // A peak in the very last samples is not lost to the interpolator delay.
        let mut tail = vec![0.0_f32; 2000];
        tail[1999] = 1.0;
        let snap = meter_for(rate, &[tail], 500);
        assert!(snap.true_peak_dbtp[0] >= -0.001 && snap.true_peak_dbtp[0] < 0.5);
        // Measured accuracy near the band edge (documented, not hidden): 0.45 fs, 45 degrees.
        let edge = shaped(f64::from(rate) * 0.45, 1.0);
        let snap = meter_for(rate, &edge, 1000);
        eprintln!("true-peak 0.45 fs amplitude 1.0 reads {} dBTP (sample peak {})", snap.true_peak_dbtp[0], snap.sample_peak_dbfs[0]);
        assert!(snap.true_peak_dbtp[0] > -0.5 && snap.true_peak_dbtp[0] < 0.3, "{}", snap.true_peak_dbtp[0]);
    });
}

#[test]
fn partition_independence_non_decimal_rate_and_surround_weights() {
    bounded_case("partition", || {
        // 11025 Hz: 100 ms is 1102.5 samples, so sub-block edges alternate 1102/1103.
        let rate = 11_025;
        let signal = vec![noise(1, 12 * rate as usize), noise(2, 12 * rate as usize)];
        let whole = meter_for(rate, &signal, signal[0].len());
        for chunk in [1usize, 7, 1102, 1103, 4097] {
            let part = meter_for(rate, &signal, chunk);
            assert_eq!(part, whole, "chunk {chunk}");
        }
        assert!(whole.integrated_lufs.is_some() && whole.loudness_range_lu.is_some());
        // 5.1 weights: LFE (weight 0) is ignored, surrounds count 1.41 (+1.5 dB).
        let lfe = vec![sine(997.0, 1.0, 0.0, 48_000, 48_000 * 4)];
        let silent = vec![0.0_f32; 48_000 * 4];
        let mut six = vec![silent.clone(); 6];
        six[3] = lfe[0].clone();
        let mut meter = LoudnessMeter::new(48_000, &LoudnessMeter::weights_5_1()).unwrap();
        let slices: Vec<&[f32]> = six.iter().map(|c| c.as_slice()).collect();
        meter.feed(&slices).unwrap();
        assert_eq!(meter.snapshot().integrated_lufs, None, "LFE with weight 0 must contribute nothing");
        let mut ls = vec![silent.clone(); 6];
        ls[4] = lfe[0].clone();
        let mut meter = LoudnessMeter::new(48_000, &LoudnessMeter::weights_5_1()).unwrap();
        let slices: Vec<&[f32]> = ls.iter().map(|c| c.as_slice()).collect();
        meter.feed(&slices).unwrap();
        let mono = meter_for(48_000, &lfe, 4800);
        let delta = meter.snapshot().integrated_lufs.unwrap() - mono.integrated_lufs.unwrap();
        assert!((delta - 10.0 * 1.41f64.log10()).abs() < 0.01, "{delta}");
    });
}

#[test]
fn chain_readout_reset_and_hostile_input() {
    bounded_case("chain_hostile", || {
        // Constructor refusals.
        assert_eq!(LoudnessMeter::new(48_000, &[]).err(), Some(ScoreError::InvalidLayout));
        assert_eq!(LoudnessMeter::new(48_000, &[1.0; 17]).err(), Some(ScoreError::InvalidLayout));
        assert_eq!(LoudnessMeter::new(48_000, &[f64::NAN]).err(), Some(ScoreError::NotFinite));
        assert_eq!(LoudnessMeter::new(48_000, &[5.0]).err(), Some(ScoreError::OutOfRange));
        assert_eq!(LoudnessMeter::new(44_101, &[1.0]).err(), Some(ScoreError::InvalidRate));
        let mut meter = LoudnessMeter::unweighted(48_000, 1).unwrap();
        assert_eq!(meter.feed(&[&[1.0], &[1.0]]).err(), Some(ScoreError::ChannelMismatch));
        assert_eq!(meter.feed(&[&[f32::NAN]]).err(), Some(ScoreError::NotFinite));
        // Through a Chain: audio unchanged bit for bit, readout handle sees the measurement.
        let meter = LoudnessMeter::unweighted(48_000, 1).unwrap();
        let readout = meter.readout();
        let mut chain = Chain::new(
            mono(),
            48_000,
            vec![ChainStage { processor: Box::new(meter), output: mono() }],
        )
        .unwrap();
        let tone = vec![sine(997.0, 0.25, 0.0, 48_000, 48_000)];
        let out = run_blocks(&mut chain, &mono(), 48_000, &tone, &[480]);
        assert_eq!(bits(&out[0]), bits(&tone[0]));
        let snap = readout.snapshot();
        assert_eq!(snap.frames, 48_000);
        let expected = -0.691 + 10.0 * (0.25f64.powi(2) / 2.0 * k_gain(48_000, 997.0).powi(2)).log10();
        assert!((snap.momentary_lufs.unwrap() - expected).abs() < 0.01);
        assert!((snap.sample_peak_dbfs[0] - db(0.25)).abs() < 0.01);
        // A discarded block resets the whole measurement and the readout.
        chain.reset();
        assert_eq!(readout.snapshot().frames, 0);
        assert!(readout.snapshot().momentary_lufs.is_none());
        // Extreme finite input: no panic, loudness stays a number or +inf, never NaN.
        let mut loud = LoudnessMeter::unweighted(48_000, 1).unwrap();
        let swing: Vec<f32> = (0..48_000).map(|n| if n % 2 == 0 { f32::MAX } else { -f32::MAX }).collect();
        loud.feed(&[&swing]).unwrap();
        let s = loud.snapshot();
        assert!(!s.momentary_lufs.unwrap().is_nan() && !s.true_peak_dbtp[0].is_nan());
    });
}
