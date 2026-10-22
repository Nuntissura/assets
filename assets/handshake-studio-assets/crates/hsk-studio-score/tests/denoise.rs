//! SMB-SCORE-DSP slice 4: spectral noise reduction (noise-profile capture, Wiener-style
//! subtraction gain, smoothing).
mod common_dsp;
use common_dsp::*;
use hsk_studio_score::*;
use std::f64::consts::PI;

const FFT: usize = 1024;

fn scaled(seed: u64, frames: usize, amplitude: f32) -> Vec<f32> {
    noise(seed, frames).iter().map(|v| v * amplitude).collect()
}
fn profile_of(channels: &[Vec<f32>], fft: usize) -> NoiseProfile {
    let mut profiler = NoiseProfiler::new(RATE, channels.len(), fft).unwrap();
    let slices: Vec<&[f32]> = channels.iter().map(|c| c.as_slice()).collect();
    profiler.push(&slices).unwrap();
    profiler.finish().unwrap()
}
/// Amplitude of the `hz` component of `s` by quadrature correlation (whole cycles in the span).
fn tone_amplitude(s: &[f32], hz: f64) -> f64 {
    let (mut c, mut q) = (0.0, 0.0);
    for (n, v) in s.iter().enumerate() {
        let w = 2.0 * PI * hz * n as f64 / f64::from(RATE);
        c += f64::from(*v) * w.cos();
        q += f64::from(*v) * w.sin();
    }
    2.0 * c.hypot(q) / s.len() as f64
}

#[test]
fn unit_gain_reconstructs_the_input_delayed_by_the_reported_latency() {
    bounded_case("identity", || {
        let profile = profile_of(&[scaled(1, 2 * RATE as usize, 0.1)], FFT);
        let params = NoiseReductionParams { reduction_db: 0.0, ..NoiseReductionParams::default() };
        let mut reducer = NoiseReducer::new(&profile, 1, params).unwrap();
        assert_eq!(reducer.latency_samples(), (FFT - 1) as u32);
        let signal: Vec<f32> = sine(440.0, 0.4, 0.3, RATE, 20_000).iter().zip(noise(5, 20_000)).map(|(a, b)| a + 0.1 * b).collect();
        let out = run_blocks(&mut reducer, &mono(), RATE, std::slice::from_ref(&signal), &[777]);
        let lat = FFT - 1;
        let worst = (0..signal.len() - lat).map(|n| (f64::from(out[0][n + lat]) - f64::from(signal[n])).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-5, "max reconstruction error {worst}");
        assert!(out[0][..lat].iter().all(|v| v.abs() < 1e-5), "start-up delay region must be silent");
    });
}

#[test]
fn noise_is_reduced_and_the_signal_is_kept() {
    bounded_case("reduction", || {
        let profile = profile_of(&[scaled(100, 2 * RATE as usize, 0.1)], FFT);
        assert!(profile.frames() > 100);
        let params = NoiseReductionParams { reduction_db: 30.0, over_subtraction: 3.0, smoothing_up_ms: 20.0, ..NoiseReductionParams::default() };
        let mut reducer = NoiseReducer::new(&profile, 1, params).unwrap();
        // 0..2 s noise only, 2..4 s the same kind of noise plus a 1 kHz tone of amplitude 0.3.
        let hiss = scaled(200, 4 * RATE as usize, 0.1);
        let signal: Vec<f32> = hiss
            .iter()
            .enumerate()
            .map(|(n, h)| if n < 2 * RATE as usize { *h } else { h + 0.3 * (2.0 * PI * 1000.0 * n as f64 / f64::from(RATE)).sin() as f32 })
            .collect();
        let out = run_blocks(&mut reducer, &mono(), RATE, std::slice::from_ref(&signal), &[1000]);
        let lat = FFT - 1;
        let span = RATE as usize..2 * RATE as usize - 2000;
        let before = rms(&signal[span.start - lat..span.end - lat]);
        let after = rms(&out[0][span]);
        let reduction = db(before / after);
        // Tone span: whole 1 kHz cycles (1 s = 1000 cycles), 3..4 s output shifted by the latency.
        let kept = tone_amplitude(&out[0][3 * RATE as usize + lat..4 * RATE as usize + lat - lat], 1000.0);
        let kept_in = tone_amplitude(&signal[3 * RATE as usize..4 * RATE as usize - lat], 1000.0);
        eprintln!("noise-only reduction {reduction:.2} dB; tone out {kept:.4} in {kept_in:.4}");
        assert!(reduction > 12.0, "noise-only segment reduced by only {reduction} dB");
        assert!((kept / kept_in - 1.0).abs() < 0.03, "tone amplitude changed: {kept} vs {kept_in}");
    });
}

#[test]
fn smoothing_limits_isolated_passing_bins() {
    bounded_case("musical_noise", || {
        let rate_frames = 4 * RATE as usize;
        let profile = profile_of(&[scaled(7, 2 * RATE as usize, 0.1)], FFT);
        let hiss = scaled(8, rate_frames, 0.1);
        // Fraction of time-frequency cells whose gain exceeds 0.5 on noise that matches the profile:
        // these are the bins that "pass" and, being isolated, are heard as musical noise.
        let leaked = |params: NoiseReductionParams| -> f64 {
            let mut reducer = NoiseReducer::new(&profile, 1, params).unwrap();
            let hop = FFT / 4;
            let (mut cells, mut passing) = (0u64, 0u64);
            let mut at = 0;
            let mut frame = 0;
            while at + hop <= hiss.len() {
                run_hop(&mut reducer, &hiss[at..at + hop], at);
                frame += 1;
                if frame > 40 {
                    let g = reducer.last_gains(0).unwrap();
                    cells += g.len() as u64;
                    passing += g.iter().filter(|v| **v > 0.5).count() as u64;
                }
                at += hop;
            }
            passing as f64 / cells as f64
        };
        let raw = NoiseReductionParams { reduction_db: 30.0, over_subtraction: 2.0, smoothing_up_ms: 0.0, smoothing_down_ms: 0.0, freq_smoothing_bins: 0 };
        let smoothed = NoiseReductionParams { smoothing_up_ms: 60.0, smoothing_down_ms: 5.0, freq_smoothing_bins: 3, ..raw };
        let (a, b) = (leaked(raw), leaked(smoothed));
        eprintln!("passing-cell fraction raw {a:.4} smoothed {b:.4}");
        assert!(a > 0.02, "raw gain must show the musical-noise effect for the test to mean anything: {a}");
        assert!(b < 0.5 * a, "smoothing did not at least halve passing cells: {a} -> {b}");
    });
}

/// Feeds one hop of mono audio through `reducer` as its own block.
fn run_hop(reducer: &mut NoiseReducer, block: &[f32], start: usize) {
    let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames: block.len(), start_sample: start as u64 };
    let channels: [&[f32]; 1] = [block];
    let mut out = OutputBlock::with_capacity(mono(), block.len()).unwrap();
    engine()
        .process(&ctx(), &InputBlock::new(&sp, &channels), reducer, &mut out, &hsk_studio_accord::CancellationToken::default())
        .unwrap();
}

#[test]
fn partition_independence_profile_chunking_and_reset() {
    bounded_case("partition", || {
        let stereo = ChannelLayout::stereo();
        let noise2 = vec![scaled(31, RATE as usize, 0.1), scaled(32, RATE as usize, 0.1)];
        let whole = profile_of(&noise2, 512);
        // Profile capture is independent of how the noise clip is chunked.
        let mut chunked = NoiseProfiler::new(RATE, 2, 512).unwrap();
        for at in (0..RATE as usize).step_by(1237) {
            let end = (at + 1237).min(RATE as usize);
            chunked.push(&[&noise2[0][at..end], &noise2[1][at..end]]).unwrap();
        }
        let chunked = chunked.finish().unwrap();
        assert_eq!(chunked, whole);
        assert_eq!(chunked.digest(), whole.digest());
        let signal = vec![scaled(41, 30_000, 0.2), scaled(42, 30_000, 0.2)];
        let params = NoiseReductionParams::default();
        let reference = run_blocks(&mut NoiseReducer::new(&whole, 2, params).unwrap(), &stereo, RATE, &signal, &[30_000]);
        for blocks in [&[1usize][..], &[7, 255, 256, 257], &[128, 129, 4099]] {
            let got = run_blocks(&mut NoiseReducer::new(&whole, 2, params).unwrap(), &stereo, RATE, &signal, blocks);
            assert_eq!(bits(&got[0]), bits(&reference[0]), "{blocks:?}");
            assert_eq!(bits(&got[1]), bits(&reference[1]), "{blocks:?}");
        }
        let mut reused = NoiseReducer::new(&whole, 2, params).unwrap();
        run_blocks(&mut reused, &stereo, RATE, &signal, &[1000]);
        reused.reset();
        let again = run_blocks(&mut reused, &stereo, RATE, &signal, &[1000]);
        assert_eq!(bits(&again[1]), bits(&reference[1]));
        // Identity depends on the profile content.
        let other = profile_of(&[scaled(99, RATE as usize, 0.3), scaled(98, RATE as usize, 0.3)], 512);
        assert_ne!(NoiseReducer::new(&other, 2, params).unwrap().identity(), NoiseReducer::new(&whole, 2, params).unwrap().identity());
    });
}

#[test]
fn hostile_inputs_are_refused_or_finite() {
    bounded_case("hostile", || {
        assert_eq!(NoiseProfiler::new(RATE, 1, 1000).err(), Some(ScoreError::OutOfRange));
        assert_eq!(NoiseProfiler::new(RATE, 1, 128).err(), Some(ScoreError::OutOfRange));
        assert_eq!(NoiseProfiler::new(RATE, 1, 16384).err(), Some(ScoreError::OutOfRange));
        assert_eq!(NoiseProfiler::new(44_101, 1, 1024).err(), Some(ScoreError::InvalidRate));
        assert_eq!(NoiseProfiler::new(RATE, 0, 1024).err(), Some(ScoreError::InvalidLayout));
        let mut profiler = NoiseProfiler::new(RATE, 1, FFT).unwrap();
        assert_eq!(profiler.push(&[&[f32::NAN]]).err(), Some(ScoreError::NotFinite));
        assert_eq!(profiler.push(&[&[0.0], &[0.0]]).err(), Some(ScoreError::ChannelMismatch));
        profiler.push(&[&vec![0.0; FFT]]).unwrap();
        assert_eq!(profiler.finish().err(), Some(ScoreError::OutOfRange), "one clip of 1 frame is too short");

        let profile = profile_of(&[scaled(1, RATE as usize, 0.1)], FFT);
        let base = NoiseReductionParams::default();
        for (p, want) in [
            (NoiseReductionParams { reduction_db: f64::NAN, ..base }, ScoreError::NotFinite),
            (NoiseReductionParams { over_subtraction: f64::INFINITY, ..base }, ScoreError::NotFinite),
            (NoiseReductionParams { reduction_db: 61.0, ..base }, ScoreError::OutOfRange),
            (NoiseReductionParams { over_subtraction: 0.5, ..base }, ScoreError::OutOfRange),
            (NoiseReductionParams { smoothing_up_ms: -1.0, ..base }, ScoreError::OutOfRange),
            (NoiseReductionParams { freq_smoothing_bins: 9, ..base }, ScoreError::OutOfRange),
        ] {
            assert_eq!(NoiseReducer::new(&profile, 1, p).err(), Some(want), "{p:?}");
        }
        assert_eq!(NoiseReducer::new(&profile, 2, base).err(), Some(ScoreError::ChannelMismatch));

        // An all-silent profile (zero noise power) must not divide by zero: the signal passes.
        let silent = profile_of(&[vec![0.0; RATE as usize]], FFT);
        let mut reducer = NoiseReducer::new(&silent, 1, base).unwrap();
        let tone = sine(300.0, 0.5, 0.0, RATE, 8000);
        let out = run_blocks(&mut reducer, &mono(), RATE, std::slice::from_ref(&tone), &[1000]);
        assert!(out[0].iter().all(|v| v.is_finite()));
        assert!((out[0][4000 + FFT - 1] - tone[4000]).abs() < 1e-4);
        // Silence in, silence out; f32::MAX swings are finite for the engine and either come out
        // finite or are refused with NotFinite.
        let mut reducer = NoiseReducer::new(&profile, 1, base).unwrap();
        let quiet = run_blocks(&mut reducer, &mono(), RATE, &[vec![0.0; 5000]], &[999]);
        assert!(quiet[0].iter().all(|v| *v == 0.0));
        let swing: Vec<f32> = (0..4096).map(|n| if n % 2 == 0 { f32::MAX } else { -f32::MAX }).collect();
        let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames: 4096, start_sample: 0 };
        let channels: [&[f32]; 1] = [&swing];
        let mut o = OutputBlock::with_capacity(mono(), 4096).unwrap();
        match engine().process(&ctx(), &InputBlock::new(&sp, &channels), &mut reducer, &mut o, &hsk_studio_accord::CancellationToken::default()) {
            Ok(_) => assert!(o.published_channel(0).unwrap().iter().all(|v| v.is_finite())),
            Err(e) => assert_eq!(e, ScoreError::NotFinite),
        }
        // A wrong block rate is refused by the engine.
        let mut reducer = NoiseReducer::new(&profile, 1, base).unwrap();
        let sp = BlockSpec { sample_rate_hz: 96_000, layout: mono(), frames: 8, start_sample: 0 };
        let channels: [&[f32]; 1] = [&tone[..8]];
        let mut o = OutputBlock::with_capacity(mono(), 8).unwrap();
        assert_eq!(
            engine().process(&ctx(), &InputBlock::new(&sp, &channels), &mut reducer, &mut o, &hsk_studio_accord::CancellationToken::default()).err(),
            Some(ScoreError::InvalidRate)
        );
    });
}
