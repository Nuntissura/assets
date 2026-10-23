//! SMB-SCORE-EFFECTS-3 slice 1: Linkwitz-Riley 4th-order crossover (bands sum to flat) and the
//! multiband compressor. Frequency responses are measured by DFT of impulse responses in f64.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;
use std::f64::consts::PI;

const IR_LEN: usize = 32_768;

/// Per-band impulse responses (f64) of a fresh crossover.
fn band_irs(rate: u32, crossovers: &[f64]) -> Vec<Vec<f64>> {
    let mut x = Crossover::new(rate, crossovers).unwrap();
    let bands = x.bands();
    let mut irs = vec![Vec::with_capacity(IR_LEN); bands];
    let mut out = [0.0; MAX_BANDS];
    for n in 0..IR_LEN {
        x.split(if n == 0 { 1.0 } else { 0.0 }, &mut out);
        for (ir, v) in irs.iter_mut().zip(&out) {
            ir.push(*v);
        }
    }
    irs
}

/// Complex response `(re, im)` of `ir` at `f`.
fn dft(ir: &[f64], f: f64, rate: u32) -> (f64, f64) {
    let w = 2.0 * PI * f / f64::from(rate);
    ir.iter().enumerate().fold((0.0, 0.0), |(re, im), (n, v)| {
        let (s, c) = (w * n as f64).sin_cos();
        (re + v * c, im - v * s)
    })
}

/// Amplitude of the `f` Hz component of `x` over a whole number of periods (lock-in).
fn amplitude(x: &[f32], f: f64, rate: u32) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (n, v) in x.iter().enumerate() {
        let (s, c) = (2.0 * PI * f * n as f64 / f64::from(rate)).sin_cos();
        re += f64::from(*v) * c;
        im += f64::from(*v) * s;
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

#[test]
fn linkwitz_riley_bands_sum_to_flat_and_match_the_analytic_band_shapes() {
    bounded_case("lr4_flat_sum", || {
        let cases: [(u32, &[f64]); 4] = [
            (RATE, &[1000.0]),
            (RATE, &[250.0, 4000.0]),
            (44_100, &[120.0, 1500.0, 9000.0]),
            (96_000, &[80.0, 800.0, 20_000.0]),
        ];
        for (rate, crossovers) in cases {
            let irs = band_irs(rate, crossovers);
            let sum: Vec<f64> = (0..IR_LEN).map(|n| irs.iter().map(|ir| ir[n]).sum()).collect();
            let analytic = Crossover::new(rate, crossovers).unwrap();
            let nyquist = f64::from(rate) / 2.0;
            for i in 0..64 {
                // Log-spaced 10 Hz .. 0.98 * Nyquist.
                let f = 10.0 * (0.98 * nyquist / 10.0).powf(f64::from(i) / 63.0);
                let (re, im) = dft(&sum, f, rate);
                let mag = (re * re + im * im).sqrt();
                assert!((mag - 1.0).abs() < 1e-9, "sum {rate} {crossovers:?} at {f} Hz: {mag}");
                for (b, ir) in irs.iter().enumerate() {
                    let (re, im) = dft(ir, f, rate);
                    let want = analytic.band_magnitude(b, f);
                    assert!(((re * re + im * im).sqrt() - want).abs() < 1e-9, "band {b} at {f} Hz");
                }
            }
            // LR4 property: each side of a split is -6.02 dB (0.5) at the crossover frequency.
            assert!((analytic.band_magnitude(0, crossovers[0]) - 0.5).abs() < 1e-9); // 1 - cos(w0) cancellation: 3.5e-12 measured at 80 Hz / 96 kHz
        }
    });
}

#[test]
fn only_the_loud_band_is_compressed_by_its_static_curve() {
    bounded_case("multiband_static", || {
        let quiet = DynamicsParams { knee_db: 0.0, ..DynamicsParams::compressor(0.0, 4.0) };
        let high = DynamicsParams {
            knee_db: 0.0,
            detector: Detector::Rms,
            rms_window_ms: 50.0,
            attack_ms: 5.0,
            release_ms: 50.0,
            ..DynamicsParams::compressor(-30.0, 4.0)
        };
        let params = MultibandParams {
            bands: 3,
            crossovers_hz: [300.0, 3000.0, 10_000.0],
            band_params: [quiet, quiet, high, quiet],
        };
        let frames = 2 * RATE as usize;
        let low_tone = sine(100.0, 0.1, 0.0, RATE, frames);
        let high_tone = sine(6000.0, 0.5, 0.3, RATE, frames);
        let input: Vec<f32> = low_tone.iter().zip(&high_tone).map(|(a, b)| a + b).collect();
        let mut mb = MultibandCompressor::new(RATE, 1, params).unwrap();
        assert_eq!(mb.latency_samples(), 0);
        let out = run_blocks(&mut mb, &mono(), RATE, &[input], &[1024]);

        // Complex band responses at both tones.
        let irs = band_irs(RATE, &[300.0, 3000.0]);
        let h = |b: usize, f: f64| dft(&irs[b], f, RATE);
        // Band-2 RMS of the 6 kHz tone: 0.5 |H2| / sqrt2; reduction (L + 30)(1/4 - 1).
        let (r2, i2) = h(2, 6000.0);
        let level_db = db(0.5 * (r2 * r2 + i2 * i2).sqrt() / 2.0_f64.sqrt());
        let reduction = -0.75 * (level_db + 30.0);
        let g2 = 10f64.powf(reduction / 20.0);
        let combined = |f: f64, g: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for b in 0..3 {
                let (br, bi) = h(b, f);
                let gain = if b == 2 { g } else { 1.0 };
                re += gain * br;
                im += gain * bi;
            }
            (re * re + im * im).sqrt()
        };
        let tail = &out[0][RATE as usize..];
        let measured_high = amplitude(tail, 6000.0, RATE);
        let want_high = 0.5 * combined(6000.0, g2);
        assert!((db(measured_high) - db(want_high)).abs() < 0.02, "{measured_high} vs {want_high}");
        assert!(reduction < -14.0, "the 6 kHz band is compressed by {reduction} dB");
        let measured_low = amplitude(tail, 100.0, RATE);
        assert!((db(measured_low) - db(0.1 * combined(100.0, g2))).abs() < 0.01);
        assert!((db(measured_low) - db(0.1)).abs() < 0.01, "low band untouched: {measured_low}");
        assert!((mb.gain_reduction_db(2, 0).unwrap() - reduction).abs() < 0.05);

        // Every band below threshold: the processor is exactly the allpass chain (bit-equal to the
        // f64 crossover sum rounded once to f32).
        let idle = MultibandParams { band_params: [quiet; 4], ..params };
        let signal = noise(9, 20_000).iter().map(|v| v * 0.5).collect::<Vec<f32>>();
        let got = run_blocks(&mut MultibandCompressor::new(RATE, 1, idle).unwrap(), &mono(), RATE, std::slice::from_ref(&signal), &[999]);
        let mut x = Crossover::new(RATE, &[300.0, 3000.0]).unwrap();
        let mut bands = [0.0; MAX_BANDS];
        for (n, v) in signal.iter().enumerate() {
            x.split(f64::from(*v), &mut bands);
            let want = (0.0 + bands[0] + bands[1] + bands[2]) as f32;
            assert_eq!(got[0][n].to_bits(), want.to_bits(), "frame {n}");
        }
    });
}

#[test]
fn partition_independence_and_unlinked_channels() {
    bounded_case("multiband_partition", || {
        let stereo = ChannelLayout::stereo();
        let signal = vec![noise(1, 20_000), noise(2, 20_000).iter().map(|v| v * 0.05).collect()];
        let band = DynamicsParams { link: false, detector: Detector::Rms, ..DynamicsParams::compressor(-24.0, 6.0) };
        let params = MultibandParams { bands: 4, crossovers_hz: [150.0, 1500.0, 7000.0], band_params: [band; 4] };
        let reference = run_blocks(&mut MultibandCompressor::new(RATE, 2, params).unwrap(), &stereo, RATE, &signal, &[20_000]);
        for blocks in [&[1usize][..], &[7, 255, 256, 257, 1000], &[4099]] {
            let got = run_blocks(&mut MultibandCompressor::new(RATE, 2, params).unwrap(), &stereo, RATE, &signal, blocks);
            assert_eq!(bits(&got[0]), bits(&reference[0]), "{blocks:?}");
            assert_eq!(bits(&got[1]), bits(&reference[1]), "{blocks:?}");
        }
        // Unlinked: the quiet right channel (about -32 dB) is below every threshold, so it is the
        // pure allpass chain and its RMS is preserved; the loud left channel is reduced.
        assert!((rms(&reference[1]) / rms(&signal[1]) - 1.0).abs() < 0.01);
        assert!(rms(&reference[0]) / rms(&signal[0]) < 0.7);
    });
}

#[test]
fn hostile_parameters_blocks_and_undeclared_spec_rows_are_refused() {
    bounded_case("multiband_hostile", || {
        let ok = MultibandParams::three_band();
        let err = |rate, ch, p| MultibandCompressor::new(rate, ch, p).unwrap_err();
        for bands in [0, 1, 5, usize::MAX] {
            assert_eq!(err(RATE, 1, MultibandParams { bands, ..ok }), ScoreError::OutOfRange, "{bands}");
        }
        for x in [[2000.0, 200.0, 0.0], [200.0, 200.0, 0.0], [10.0, 200.0, 0.0], [200.0, 24_000.0, 0.0], [200.0, 1e300, 0.0]] {
            assert_eq!(err(RATE, 1, MultibandParams { crossovers_hz: x, ..ok }), ScoreError::OutOfRange, "{x:?}");
        }
        for v in [f64::NAN, f64::INFINITY] {
            assert_eq!(err(RATE, 1, MultibandParams { crossovers_hz: [v, 2000.0, 0.0], ..ok }), ScoreError::NotFinite);
            let mut p = ok;
            p.band_params[1].threshold_db = v;
            assert_eq!(err(RATE, 1, p), ScoreError::NotFinite);
        }
        let mut p = ok;
        p.band_params[2].ratio = 1e9;
        assert_eq!(err(RATE, 1, p), ScoreError::OutOfRange);
        for ch in [0, 17, usize::MAX] {
            assert_eq!(err(RATE, ch, ok), ScoreError::InvalidLayout);
        }
        for rate in [0, 44_101, 1_000_000] {
            assert_eq!(err(rate, 1, ok), ScoreError::InvalidRate);
        }
        assert_eq!(Crossover::new(RATE, &[]).unwrap_err(), ScoreError::OutOfRange);
        assert_eq!(Crossover::new(RATE, &[100.0, 200.0, 300.0, 400.0]).unwrap_err(), ScoreError::OutOfRange);

        let mut mb = MultibandCompressor::new(RATE, 1, ok).unwrap();
        let (eng, token) = (engine(), CancellationToken::default());
        let mut out = OutputBlock::with_capacity(mono(), 64).unwrap();
        let mut x = vec![0.1_f32; 65];
        for frames in [MAX_BLOCK_FRAMES + 1, 65] {
            let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames, start_sample: 0 };
            let slices: Vec<&[f32]> = vec![&x];
            let got = eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut mb, &mut out, &token);
            assert_eq!(got.unwrap_err(), ScoreError::CapacityExceeded);
        }
        x[3] = f32::NAN;
        let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames: 64, start_sample: 0 };
        let slices: Vec<&[f32]> = vec![&x[..64]];
        let got = eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut mb, &mut out, &token);
        assert_eq!(got.unwrap_err(), ScoreError::NotFinite);
        assert_eq!(out.published_frames(), 0);

        // Spec rows: declared meaning binds, undeclared meaning is a typed refusal, unknown rows
        // are UnknownRow; every refused table row really refuses.
        assert!(bind_spec_row("Dynamics", "look_ahead_time").unwrap().contains("lookahead_ms"));
        match bind_spec_row("Dynamics", "low_frequency") {
            Err(SpecRowError::Undeclared(u)) => assert_eq!((u.effect, u.parameter), ("Dynamics", "low_frequency")),
            other => panic!("{other:?}"),
        }
        assert_eq!(bind_spec_row("Dynamics", "nope"), Err(SpecRowError::UnknownRow));
        assert_eq!(bind_spec_row("Multiband Compressor", "anything"), Err(SpecRowError::UnknownRow));
        for (effect, parameter, row) in SPEC_ROWS {
            assert_eq!(bind_spec_row(effect, parameter).is_ok(), row.is_ok(), "{effect}/{parameter}");
        }
    });
}
