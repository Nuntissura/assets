use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_score::*;
use std::f64::consts::PI;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 30s deadline: {e}"),
    }
}

const RESOURCE: &str = "STRK-019abcde-0000-7000-8000-000000000001";
const RATE: u32 = 48_000;

fn ctx() -> RequestContext {
    RequestContext {
        resource: DomainId::parse(RESOURCE).unwrap(),
        actor: Some(
            ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
                .unwrap(),
        ),
        expected_revision: 1,
    }
}
fn mono() -> ChannelLayout {
    ChannelLayout::mono(7)
}
fn band(kind: BiquadKind, frequency_hz: f64, gain_db: f64, q: f64) -> BiquadSpec {
    BiquadSpec { kind, frequency_hz, gain_db, q }
}
/// Streams `signal` through `eq` in the given block sizes and returns the concatenated output.
fn run(eq: &mut Equalizer, signal: &[f32], blocks: &[usize]) -> Vec<f32> {
    let engine = Engine::new(DomainId::parse(RESOURCE).unwrap(), 1);
    let token = CancellationToken::default();
    let (mut result, mut at) = (Vec::new(), 0);
    for &len in blocks {
        let sp = BlockSpec { sample_rate_hz: RATE, layout: mono(), frames: len, start_sample: at as u64 };
        let channels: [&[f32]; 1] = [&signal[at..at + len]];
        let mut out = OutputBlock::with_capacity(mono(), len).unwrap();
        engine.process(&ctx(), &InputBlock::new(&sp, &channels), eq, &mut out, &token).unwrap();
        result.extend_from_slice(out.published_channel(0).unwrap());
        at += len;
    }
    assert_eq!(at, signal.len());
    result
}
fn tone(hz: f64, frames: usize) -> Vec<f32> {
    (0..frames).map(|n| (0.1 * (2.0 * PI * hz * n as f64 / f64::from(RATE)).sin()) as f32).collect()
}
/// Steady-state gain in dB: RMS of the last third of the output over RMS of the same input span.
fn measured_db(eq: &mut Equalizer, hz: f64) -> f64 {
    let frames = 72_000;
    let signal = tone(hz, frames);
    let got = run(eq, &signal, &[frames]);
    let rms = |s: &[f32]| (s.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
    20.0 * (rms(&got[48_000..]) / rms(&signal[48_000..])).log10()
}

#[test]
fn equalizer_matches_known_responses() {
    bounded_case("equalizer_matches_known_responses", || {
        let q_butterworth = std::f64::consts::FRAC_1_SQRT_2;

        // Peaking +12 dB at 1 kHz: exactly +12 dB at the centre (cookbook property), and the
        // time-domain filter agrees with the analytic response away from it.
        let peak = [band(BiquadKind::Peaking, 1000.0, 12.0, 1.0)];
        let mut eq = Equalizer::new(RATE, 1, &peak, 0.0).unwrap();
        assert!((eq.magnitude_db(1000.0) - 12.0).abs() < 1e-9);
        for hz in [250.0, 1000.0, 4000.0] {
            let analytic = eq.magnitude_db(hz);
            let measured = measured_db(&mut eq, hz);
            assert!((analytic - measured).abs() < 0.02, "{hz} Hz: analytic {analytic} measured {measured}");
            eq.reset();
        }

        // Shelves and passes against their defining properties.
        let low = Equalizer::new(RATE, 1, &[band(BiquadKind::LowShelf, 200.0, 6.0, q_butterworth)], 0.0).unwrap();
        assert!((low.magnitude_db(0.001) - 6.0).abs() < 1e-3, "low shelf DC gain is its gain");
        assert!(low.magnitude_db(20_000.0).abs() < 0.05);
        let high = Equalizer::new(RATE, 1, &[band(BiquadKind::HighShelf, 4000.0, -9.0, q_butterworth)], 0.0).unwrap();
        assert!((high.magnitude_db(23_999.0) + 9.0).abs() < 0.05, "high shelf Nyquist gain is its gain");
        assert!(high.magnitude_db(50.0).abs() < 0.05);
        let lp = Equalizer::new(RATE, 1, &[band(BiquadKind::LowPass, 1000.0, 0.0, q_butterworth)], 0.0).unwrap();
        let hp = Equalizer::new(RATE, 1, &[band(BiquadKind::HighPass, 1000.0, 0.0, q_butterworth)], 0.0).unwrap();
        assert!((lp.magnitude_db(1000.0) + 3.0103).abs() < 1e-3, "Butterworth corner is -3.01 dB");
        assert!((hp.magnitude_db(1000.0) + 3.0103).abs() < 1e-3);
        assert!(lp.magnitude_db(0.001).abs() < 1e-6 && hp.magnitude_db(20.0) < -60.0);

        // Neutral settings are bit-exact identity, and block boundaries do not change a bit.
        let signal = tone(440.0, 1000);
        let mut identity = Equalizer::new(RATE, 1, &[band(BiquadKind::Peaking, 1000.0, 0.0, 1.0)], 0.0).unwrap();
        let same = run(&mut identity, &signal, &[1000]);
        assert_eq!(same.iter().map(|s| s.to_bits()).collect::<Vec<_>>(), signal.iter().map(|s| s.to_bits()).collect::<Vec<_>>());
        let sections = [band(BiquadKind::Peaking, 880.0, 9.0, 2.0), band(BiquadKind::LowPass, 3000.0, 0.0, q_butterworth)];
        let whole = run(&mut Equalizer::new(RATE, 1, &sections, -3.0).unwrap(), &signal, &[1000]);
        let split = run(&mut Equalizer::new(RATE, 1, &sections, -3.0).unwrap(), &signal, &[1, 255, 256, 488]);
        assert_eq!(whole.iter().map(|s| s.to_bits()).collect::<Vec<_>>(), split.iter().map(|s| s.to_bits()).collect::<Vec<_>>());
        assert!(whole.iter().any(|s| s.abs() > 1e-3), "filter produced signal");

        // Master gain: -6.0206 dB halves the signal.
        let mut half = Equalizer::new(RATE, 1, &[], 20.0 * 0.5_f64.log10()).unwrap();
        let halved = run(&mut half, &signal, &[1000]);
        assert!(halved.iter().zip(&signal).all(|(h, s)| (h - s * 0.5).abs() < 1e-7));

        // Contract ranges and wrong-rate refusal.
        let ok = band(BiquadKind::Peaking, 1000.0, 0.0, 1.0);
        for (name, spec) in [
            ("freq low", BiquadSpec { frequency_hz: 19.9, ..ok }),
            ("freq high", BiquadSpec { frequency_hz: 24_000.1, ..ok }),
            ("amp", BiquadSpec { gain_db: 48.1, ..ok }),
            ("q low", BiquadSpec { q: 0.00009, ..ok }),
            ("q high", BiquadSpec { q: 10_000.1, ..ok }),
        ] {
            assert_eq!(Equalizer::new(RATE, 1, &[spec], 0.0).unwrap_err(), ScoreError::OutOfRange, "{name}");
        }
        assert_eq!(Equalizer::new(RATE, 1, &[ok], 48.1).unwrap_err(), ScoreError::OutOfRange);
        assert_eq!(Equalizer::new(RATE, 1, &[ok], -96.1).unwrap_err(), ScoreError::OutOfRange);
        assert_eq!(Equalizer::new(RATE, 1, &[BiquadSpec { gain_db: f64::NAN, ..ok }], 0.0).unwrap_err(), ScoreError::NotFinite);
        assert!(Equalizer::new(RATE, 1, &[BiquadSpec { frequency_hz: 24_000.0, ..ok }], 0.0).is_ok());
        let engine = Engine::new(DomainId::parse(RESOURCE).unwrap(), 1);
        let sp = BlockSpec { sample_rate_hz: 44_100, layout: mono(), frames: 4, start_sample: 0 };
        let silent = [0.0_f32; 4];
        let channels: [&[f32]; 1] = [&silent];
        let mut out = OutputBlock::with_capacity(mono(), 4).unwrap();
        let refused = engine.process(&ctx(), &InputBlock::new(&sp, &channels), &mut identity, &mut out, &CancellationToken::default());
        assert_eq!(refused.unwrap_err(), ScoreError::InvalidRate);
    });
}
