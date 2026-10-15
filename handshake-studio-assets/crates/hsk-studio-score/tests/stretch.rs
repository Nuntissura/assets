//! SMB-SCORE-EFFECTS-5 slice 2: phase-vocoder time-stretch (pitch kept) and pitch-shift
//! (duration kept). Frequencies are measured from the lock-in phase drift between two segments.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;
use std::f64::consts::PI;

/// Lock-in `(amplitude, phase)` of `x[start..start + len]` at `f` Hz.
fn lock_in(x: &[f32], start: usize, len: usize, f: f64, rate: u32) -> (f64, f64) {
    let (mut re, mut im) = (0.0, 0.0);
    for (n, v) in x.iter().enumerate().skip(start).take(len) {
        let a = 2.0 * PI * f * n as f64 / f64::from(rate);
        re += f64::from(*v) * a.cos();
        im += f64::from(*v) * a.sin();
    }
    (2.0 * (re * re + im * im).sqrt() / len as f64, im.atan2(re))
}

/// Frequency near `f0` from the lock-in phase drift between two segments `gap` samples apart.
fn measured_frequency(x: &[f32], f0: f64, rate: u32) -> f64 {
    let seg = rate as usize / 10;
    let a = x.len() / 4;
    let gap = x.len() / 2;
    let (_, p1) = lock_in(x, a, seg, f0, rate);
    let (_, p2) = lock_in(x, a + gap, seg, f0, rate);
    let d = (p2 - p1 + PI).rem_euclid(2.0 * PI) - PI;
    // lock_in phase is atan2(sum x sin, sum x cos) = -(phase of x relative to the reference).
    f0 - d * f64::from(rate) / (2.0 * PI * gap as f64)
}

#[test]
fn stretch_keeps_pitch_and_amplitude_and_hits_the_exact_duration() {
    bounded_case("stretch_duration_pitch", || {
        let token = CancellationToken::default();
        for rate in [RATE, 44_100, 96_000] {
            let x = sine(440.0, 0.5, 0.3, rate, 2 * rate as usize);
            for factor in [0.5, 0.8, 1.5, 2.0, 3.0] {
                let y = time_stretch(&x, rate, factor, StretchParams::for_rate(rate), &token).unwrap();
                assert_eq!(y.len(), (x.len() as f64 * factor).round() as usize, "{rate} {factor}");
                let f = measured_frequency(&y, 440.0, rate);
                let (amp, _) = lock_in(&y, y.len() / 3, rate as usize / 10, f, rate);
                println!("rate {rate} factor {factor}: {f:.5} Hz, amplitude {amp:.5}");
                assert!((f - 440.0).abs() < 0.01, "{rate} {factor}: {f}");
                assert!((db(amp / 0.5)).abs() < 0.2, "{rate} {factor}: {amp}");
            }
        }
        assert_eq!(StretchParams::for_rate(48_000).fft_size, 2048);
        assert_eq!(StretchParams::for_rate(96_000).fft_size, 4096);
    });
}

#[test]
fn pitch_shift_moves_the_frequency_by_the_stated_ratio_and_keeps_the_length() {
    bounded_case("pitch_shift", || {
        let token = CancellationToken::default();
        let x = sine(440.0, 0.5, 0.3, RATE, 2 * RATE as usize);
        for semitones in [12.0, -7.0, 3.5, -24.0, 19.0] {
            let ratio = pitch_ratio(semitones).unwrap();
            let cents = 1200.0 * (ratio.ratio() / (semitones / 12.0_f64).exp2()).log2();
            assert!(cents.abs() < 0.23, "{semitones}: {cents} cents");
            let y = pitch_shift(&x, RATE, semitones, StretchParams::for_rate(RATE), &token).unwrap();
            assert_eq!(y.len(), x.len());
            let want = 440.0 * ratio.ratio();
            let f = measured_frequency(&y, want, RATE);
            let (amp, _) = lock_in(&y, y.len() / 3, RATE as usize / 10, f, RATE);
            println!("{semitones} semitones: {f:.5} Hz (want {want:.5}), amplitude {amp:.5}");
            assert!((f - want).abs() < 0.02, "{semitones}: {f} vs {want}");
            assert!((db(amp / 0.5)).abs() < 0.5, "{semitones}: {amp}");
        }
    });
}

#[test]
fn identity_phase_locking_keeps_a_harmonic_tone_cleaner_than_the_plain_vocoder() {
    bounded_case("phase_locking", || {
        // 8 harmonics of 220 Hz (amplitudes 1/h) stretched by 1.37: fit each harmonic by lock-in
        // over the middle and measure what is left (spectral spread = phasiness).
        let token = CancellationToken::default();
        let frames = 2 * RATE as usize;
        let mut x = vec![0.0_f32; frames];
        for h in 1..=8 {
            let s = sine(220.0 * f64::from(h), 0.3 / f64::from(h), 0.7 * f64::from(h), RATE, frames);
            x.iter_mut().zip(s).for_each(|(a, b)| *a += b);
        }
        let residual_db = |locking: bool| {
            let params = StretchParams { phase_locking: locking, ..StretchParams::for_rate(RATE) };
            let y = time_stretch(&x, RATE, 1.37, params, &token).unwrap();
            let (start, len) = (y.len() / 3, RATE as usize / 2);
            let mut fit = vec![0.0_f64; len];
            for h in 1..=8 {
                let f = 220.0 * f64::from(h);
                let (re, im) = (0..len).fold((0.0, 0.0), |(re, im), k| {
                    let a = 2.0 * PI * f * (start + k) as f64 / f64::from(RATE);
                    (re + f64::from(y[start + k]) * a.cos(), im + f64::from(y[start + k]) * a.sin())
                });
                let (c, s) = (2.0 * re / len as f64, 2.0 * im / len as f64);
                for (k, v) in fit.iter_mut().enumerate() {
                    let a = 2.0 * PI * f * (start + k) as f64 / f64::from(RATE);
                    *v += c * a.cos() + s * a.sin();
                }
            }
            let total: f64 = y[start..start + len].iter().map(|v| f64::from(*v).powi(2)).sum();
            let rest: f64 = (0..len).map(|k| (f64::from(y[start + k]) - fit[k]).powi(2)).sum();
            10.0 * (rest / total).log10()
        };
        let (locked, plain) = (residual_db(true), residual_db(false));
        println!("residual beside the harmonics: locked {locked:.2} dB, plain {plain:.2} dB");
        // Measured at the first run: locked -65.81 dB, plain -45.79 dB.
        assert!(locked < plain - 10.0, "locking must be clearly cleaner");
        assert!(locked < -30.0, "{locked}");
    });
}

#[test]
fn hostile_inputs_cancel_silence_and_determinism() {
    bounded_case("stretch_hostile", || {
        let token = CancellationToken::default();
        let p = StretchParams::for_rate(RATE);
        let x = noise(3, 20_000);
        assert_eq!(time_stretch(&x, RATE, f64::NAN, p, &token).err(), Some(ScoreError::NotFinite));
        for f in [0.0, 0.24, 4.01, -1.0] {
            assert_eq!(time_stretch(&x, RATE, f, p, &token).err(), Some(ScoreError::OutOfRange), "{f}");
        }
        for fft in [0, 255, 1000, 32_768] {
            assert_eq!(time_stretch(&x, RATE, 1.5, StretchParams { fft_size: fft, phase_locking: true }, &token).err(), Some(ScoreError::OutOfRange), "{fft}");
        }
        assert_eq!(time_stretch(&[], RATE, 1.5, p, &token).err(), Some(ScoreError::OutOfRange));
        assert_eq!(time_stretch(&x, 44_101, 1.5, p, &token).err(), Some(ScoreError::InvalidRate));
        let mut bad = x.clone();
        bad[7] = f32::INFINITY;
        assert_eq!(time_stretch(&bad, RATE, 1.5, p, &token).err(), Some(ScoreError::NotFinite));
        // Output bound checked before allocating: 10 M samples * 4 > 2^25.
        let long = vec![0.0_f32; 10_000_000];
        assert_eq!(time_stretch(&long, RATE, 4.0, p, &token).err(), Some(ScoreError::CapacityExceeded));
        for s in [f64::NAN, 24.5, -30.0] {
            assert!(pitch_ratio(s).is_err(), "{s}");
            assert!(pitch_shift(&x, RATE, s, p, &token).is_err(), "{s}");
        }
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(time_stretch(&x, RATE, 1.5, p, &canceled).err(), Some(ScoreError::Canceled));
        // Silence stays silent; the same call twice gives the same bits.
        assert!(time_stretch(&vec![0.0; 5000], RATE, 2.0, p, &token).unwrap().iter().all(|v| *v == 0.0));
        let a = pitch_shift(&x, RATE, 5.0, p, &token).unwrap();
        let b = pitch_shift(&x, RATE, 5.0, p, &token).unwrap();
        assert_eq!(bits(&a), bits(&b));
        assert!(a.iter().all(|v| v.is_finite()));
    });
}
