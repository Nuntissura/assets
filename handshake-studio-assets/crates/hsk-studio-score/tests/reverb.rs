//! SMB-SCORE-DSP slice 3: algorithmic reverb (Schroeder-Moorer / Freeverb tunings).
mod common_dsp;
use common_dsp::*;
use hsk_studio_score::*;

fn impulse(frames: usize) -> Vec<f32> {
    let mut v = vec![0.0; frames];
    v[0] = 1.0;
    v
}
fn wet_only(rt60_s: f64) -> ReverbParams {
    ReverbParams { rt60_s, damping: 0.0, pre_delay_ms: 0.0, width: 1.0, wet: 1.0, dry: 0.0 }
}
/// RT60 from the Schroeder backward integral: time between -5 and -35 dB (T30) doubled.
fn measured_rt60(ir: &[f32], rate: u32) -> f64 {
    let mut energy = vec![0.0_f64; ir.len() + 1];
    for n in (0..ir.len()).rev() {
        energy[n] = energy[n + 1] + f64::from(ir[n]).powi(2);
    }
    let curve: Vec<f64> = energy[..ir.len()].iter().map(|e| 10.0 * (e / energy[0]).log10()).collect();
    let at = |level: f64| curve.iter().position(|v| *v <= level).expect("decay reaches level") as f64;
    2.0 * (at(-35.0) - at(-5.0)) / f64::from(rate)
}

#[test]
fn decay_time_follows_rt60_at_two_rates() {
    bounded_case("rt60", || {
        for (rate, rt60) in [(48_000u32, 1.0), (48_000, 2.5), (44_100, 0.6), (96_000, 1.2)] {
            let frames = (rt60 * 2.0 * f64::from(rate)) as usize + 1000;
            let mut reverb = Reverb::new(rate, 1, wet_only(rt60)).unwrap();
            let ir = run_blocks(&mut reverb, &mono(), rate, &[impulse(frames)], &[4096]);
            let measured = measured_rt60(&ir[0], rate);
            // Every comb loses exactly 60 dB per RT60 (analytic); the allpasses are unity gain.
            // T30 on the sparse summed response agrees within 10 percent.
            assert!((measured / rt60 - 1.0).abs() < 0.10, "rate {rate} rt60 {rt60}: measured {measured}");
            assert_eq!(reverb.latency_samples(), 0);
            assert_eq!(reverb.tail_frames(), (rt60 * f64::from(rate)).ceil() as u64);
        }
        // Damping shortens the tail (high frequencies see less than the DC loop gain).
        let rate = 48_000;
        let frames = 5 * rate as usize;
        let ir = |damping: f64| {
            let mut r = Reverb::new(rate, 1, ReverbParams { damping, ..wet_only(2.0) }).unwrap();
            run_blocks(&mut r, &mono(), rate, &[impulse(frames)], &[4096]).remove(0)
        };
        assert!(measured_rt60(&ir(0.6), rate) < 0.8 * measured_rt60(&ir(0.0), rate));
    });
}

#[test]
fn delay_structure_and_dry_wet_routing_are_exact() {
    bounded_case("structure", || {
        let rate = 48_000;
        // Shortest comb: round(1116 * 48000 / 44100) = 1215 samples. The first wet sample appears
        // exactly there without pre-delay, and 480 samples later with 10 ms pre-delay.
        for (pre_ms, offset) in [(0.0, 0usize), (10.0, 480)] {
            let mut r = Reverb::new(rate, 1, ReverbParams { pre_delay_ms: pre_ms, ..wet_only(1.0) }).unwrap();
            let ir = run_blocks(&mut r, &mono(), rate, &[impulse(6000)], &[1000]);
            let first = 1215 + offset;
            assert!(ir[0][..first].iter().all(|v| *v == 0.0), "pre {pre_ms}");
            assert!(ir[0][first] != 0.0, "pre {pre_ms}");
        }
        // dry = 1, wet = 0: bit-exact pass-through.
        let signal = vec![noise(9, 3000), noise(10, 3000)];
        let stereo = ChannelLayout::stereo();
        let params = ReverbParams { wet: 0.0, dry: 1.0, ..ReverbParams::default() };
        let out = run_blocks(&mut Reverb::new(rate, 2, params).unwrap(), &stereo, rate, &signal, &[500]);
        assert_eq!(bits(&out[0]), bits(&signal[0]));
        assert_eq!(bits(&out[1]), bits(&signal[1]));
        // width 0 gives identical wet channels for identical inputs; width 1 decorrelates them.
        let same = vec![noise(3, 6000), noise(3, 6000)];
        let narrow = ReverbParams { width: 0.0, ..wet_only(1.0) };
        let out = run_blocks(&mut Reverb::new(rate, 2, narrow).unwrap(), &stereo, rate, &same, &[999]);
        assert_eq!(bits(&out[0]), bits(&out[1]));
        let wide = run_blocks(&mut Reverb::new(rate, 2, wet_only(1.0)).unwrap(), &stereo, rate, &same, &[999]);
        assert_ne!(bits(&wide[0]), bits(&wide[1]));
        // Mono layout refuses a stereo reverb and vice versa.
        let eng = engine();
        let token = hsk_studio_accord::CancellationToken::default();
        let sp = BlockSpec { sample_rate_hz: rate, layout: mono(), frames: 8, start_sample: 0 };
        let channels: [&[f32]; 1] = [&signal[0][..8]];
        let mut out = OutputBlock::with_capacity(mono(), 8).unwrap();
        let mut stereo_reverb = Reverb::new(rate, 2, ReverbParams::default()).unwrap();
        assert_eq!(
            eng.process(&ctx(), &InputBlock::new(&sp, &channels), &mut stereo_reverb, &mut out, &token).err(),
            Some(ScoreError::ChannelMismatch)
        );
    });
}

#[test]
fn block_partition_cannot_change_a_single_bit() {
    bounded_case("partition", || {
        let rate = 44_100;
        let stereo = ChannelLayout::stereo();
        let signal = vec![noise(21, 30_000), noise(22, 30_000)];
        let params = ReverbParams { pre_delay_ms: 10.0, damping: 0.4, ..ReverbParams::default() };
        let reference = run_blocks(&mut Reverb::new(rate, 2, params).unwrap(), &stereo, rate, &signal, &[30_000]);
        for blocks in [&[1usize][..], &[7, 255, 256, 257], &[4099]] {
            let got = run_blocks(&mut Reverb::new(rate, 2, params).unwrap(), &stereo, rate, &signal, blocks);
            assert_eq!(bits(&got[0]), bits(&reference[0]), "{blocks:?}");
            assert_eq!(bits(&got[1]), bits(&reference[1]), "{blocks:?}");
        }
        // reset returns to the initial state (output equals a fresh instance).
        let mut reused = Reverb::new(rate, 2, params).unwrap();
        run_blocks(&mut reused, &stereo, rate, &signal, &[1000]);
        reused.reset();
        let again = run_blocks(&mut reused, &stereo, rate, &signal, &[1000]);
        assert_eq!(bits(&again[0]), bits(&reference[0]));
    });
}

#[test]
fn hostile_parameters_extreme_input_and_silent_tail() {
    bounded_case("hostile", || {
        let base = ReverbParams::default();
        for (p, want) in [
            (ReverbParams { rt60_s: f64::NAN, ..base }, ScoreError::NotFinite),
            (ReverbParams { wet: f64::INFINITY, ..base }, ScoreError::NotFinite),
            (ReverbParams { rt60_s: 0.0, ..base }, ScoreError::OutOfRange),
            (ReverbParams { rt60_s: 31.0, ..base }, ScoreError::OutOfRange),
            (ReverbParams { damping: 1.1, ..base }, ScoreError::OutOfRange),
            (ReverbParams { pre_delay_ms: 201.0, ..base }, ScoreError::OutOfRange),
            (ReverbParams { width: -0.1, ..base }, ScoreError::OutOfRange),
            (ReverbParams { dry: 2.0, ..base }, ScoreError::OutOfRange),
        ] {
            assert_eq!(Reverb::new(48_000, 2, p).err(), Some(want), "{p:?}");
        }
        assert_eq!(Reverb::new(44_101, 1, base).err(), Some(ScoreError::InvalidRate));
        assert_eq!(Reverb::new(48_000, 0, base).err(), Some(ScoreError::InvalidLayout));
        assert_eq!(Reverb::new(48_000, 3, base).err(), Some(ScoreError::InvalidLayout));
        // Longest, least damped tail on loud noise stays bounded and finite.
        let rate = 48_000;
        let extreme = ReverbParams { rt60_s: 30.0, damping: 0.0, wet: 1.0, dry: 1.0, ..base };
        let out = run_blocks(&mut Reverb::new(rate, 1, extreme).unwrap(), &mono(), rate, &[noise(1, 5 * rate as usize)], &[4096]);
        assert!(out[0].iter().all(|v| v.is_finite() && v.abs() < 1000.0));
        // f32::MAX input is finite for the engine; overflowing output is refused, never published.
        let eng = engine();
        let token = hsk_studio_accord::CancellationToken::default();
        let mut r = Reverb::new(rate, 1, extreme).unwrap();
        let big = vec![f32::MAX; 4096];
        let sp = BlockSpec { sample_rate_hz: rate, layout: mono(), frames: 4096, start_sample: 0 };
        let channels: [&[f32]; 1] = [&big];
        let mut o = OutputBlock::with_capacity(mono(), 4096).unwrap();
        match eng.process(&ctx(), &InputBlock::new(&sp, &channels), &mut r, &mut o, &token) {
            Ok(_) => assert!(o.published_channel(0).unwrap().iter().all(|v| v.is_finite())),
            Err(e) => assert_eq!(e, ScoreError::NotFinite),
        }
        // Silent tail: after an impulse with RT60 0.1 s the state flushes to exact zero.
        let mut r = Reverb::new(rate, 1, wet_only(0.1)).unwrap();
        let ir = run_blocks(&mut r, &mono(), rate, &[impulse(4 * rate as usize)], &[4096]);
        assert!(ir[0][3 * rate as usize..].iter().all(|v| *v == 0.0));
        assert!(ir[0].iter().any(|v| *v != 0.0));
    });
}
