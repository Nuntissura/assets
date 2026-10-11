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

struct CancelAfter<'a>(RampedGain, &'a CancellationToken);
impl Processor for CancelAfter<'_> {
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, i: &ChannelLayout, o: &ChannelLayout) -> bool {
        self.0.accepts(i, o)
    }
    fn process(&mut self, input: &InputBlock<'_>, out: &mut OutputBlock, cancel: &CancellationToken) -> Result<(), ScoreError> {
        self.0.process(input, out, cancel)?;
        self.1.cancel();
        Ok(())
    }
    fn reset(&mut self) {
        self.0.reset();
    }
}

#[test]
fn ramp_is_exact_continuous_and_revertible_and_receipt_meters() {
    bounded_case("ramp_is_exact_continuous_and_revertible_and_receipt_meters", || {
        let engine = Engine::new(DomainId::parse(RESOURCE).unwrap(), 1);
        let token = CancellationToken::default();
        let ones = [1.0_f32; 4];
        let sp = BlockSpec { sample_rate_hz: 48_000, layout: mono(), frames: 4, start_sample: 0 };
        let channels: [&[f32]; 1] = [&ones];
        let input = InputBlock::new(&sp, &channels);
        let mut out = OutputBlock::with_capacity(mono(), 4).unwrap();
        let run = |gain: &mut dyn Processor, out: &mut OutputBlock| {
            engine.process(&ctx(), &input, gain, out, &token).unwrap()
        };

        // 0 -> 1 over 4 frames: g(n) = n / 4; the next block starts exactly on the target.
        let mut ramp = RampedGain::new(0.0).unwrap();
        ramp.set_target(1.0).unwrap();
        run(&mut ramp, &mut out);
        assert_eq!(out.published_channel(0).unwrap(), &[0.0, 0.25, 0.5, 0.75]);
        assert_eq!((ramp.current(), ramp.target()), (1.0, 1.0));
        run(&mut ramp, &mut out);
        assert_eq!(out.published_channel(0).unwrap(), &[1.0; 4], "steady state after the ramp");
        ramp.set_target(0.0).unwrap();
        let receipt = run(&mut ramp, &mut out);
        assert_eq!(out.published_channel(0).unwrap(), &[1.0, 0.75, 0.5, 0.25]);
        // Receipt metering: peak and RMS of the published block, exact for these binary fractions.
        assert_eq!(receipt.peak(), &[1.0]);
        let expected_rms = ((1.0_f64 + 0.5625 + 0.25 + 0.0625) / 4.0).sqrt() as f32;
        assert!((receipt.rms()[0] - expected_rms).abs() < 1e-7);
        assert_eq!(RampedGain::new(f32::NAN).unwrap_err(), ScoreError::NotFinite);
        assert_eq!(ramp.set_target(f32::INFINITY).unwrap_err(), ScoreError::NotFinite);

        // A block discarded after processing (cancel at the publication gate) reverts the ramp.
        let mut ramp = RampedGain::new(0.0).unwrap();
        ramp.set_target(1.0).unwrap();
        let mid = CancellationToken::default();
        let mut wrapped = CancelAfter(ramp, &mid);
        let refused = engine.process(&ctx(), &input, &mut wrapped, &mut out, &mid);
        assert_eq!(refused.unwrap_err(), ScoreError::Canceled);
        assert_eq!((wrapped.0.current(), wrapped.0.target()), (0.0, 1.0));
    });
}

#[test]
fn spectrum_matches_direct_dft_and_known_bins() {
    bounded_case("spectrum_matches_direct_dft_and_known_bins", || {
        let token = CancellationToken::default();
        let size = 1024;
        let mut analyzer = SpectrumAnalyzer::new(size).unwrap();
        assert_eq!((analyzer.bin_count(), analyzer.bin_frequency_hz(64, 48_000)), (513, 3000.0));

        // A bin-centred sine of amplitude 0.5 reads 0.5 in its bin and 0.25 in each Hann neighbour.
        let sine: Vec<f32> = (0..size).map(|n| (0.5 * (2.0 * PI * 64.0 * n as f64 / size as f64).sin()) as f32).collect();
        let bins = analyzer.analyze(&sine, &token).unwrap().to_vec();
        assert!((bins[64] - 0.5).abs() < 1e-5, "peak {}", bins[64]);
        assert!((bins[63] - 0.25).abs() < 1e-5 && (bins[65] - 0.25).abs() < 1e-5);
        assert!(bins.iter().enumerate().all(|(k, v)| k.abs_diff(64) <= 1 || *v < 1e-5), "no leakage beyond the Hann lobe");

        // A constant reads its own level at DC and nothing elsewhere.
        let dc = vec![0.25_f32; size];
        let bins = analyzer.analyze(&dc, &token).unwrap();
        assert!((bins[0] - 0.25).abs() < 1e-6 && bins[2..].iter().all(|v| *v < 1e-6));

        // Independent check: a naive O(N^2) DFT of a deterministic pseudo-random signal.
        let size = 64;
        let mut analyzer = SpectrumAnalyzer::new(size).unwrap();
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let signal: Vec<f32> = (0..size)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                ((seed >> 40) as f64 / (1u64 << 24) as f64 * 2.0 - 1.0) as f32
            })
            .collect();
        let fast = analyzer.analyze(&signal, &token).unwrap().to_vec();
        let window: Vec<f64> = (0..size).map(|n| 0.5 - 0.5 * (2.0 * PI * n as f64 / size as f64).cos()).collect();
        let window_sum: f64 = window.iter().sum();
        for (k, got) in fast.iter().enumerate() {
            let (mut re, mut im) = (0.0, 0.0);
            for (n, (s, w)) in signal.iter().zip(&window).enumerate() {
                let angle = -2.0 * PI * (k * n) as f64 / size as f64;
                re += f64::from(*s) * w * angle.cos();
                im += f64::from(*s) * w * angle.sin();
            }
            let scale = if k == 0 || k == size / 2 { 1.0 } else { 2.0 };
            let direct = scale * re.hypot(im) / window_sum;
            assert!((f64::from(*got) - direct).abs() < 1e-6, "bin {k}: fft {got} dft {direct}");
        }

        // Refusals.
        assert_eq!(SpectrumAnalyzer::new(100).unwrap_err(), ScoreError::OutOfRange);
        assert_eq!(SpectrumAnalyzer::new(4).unwrap_err(), ScoreError::OutOfRange);
        assert_eq!(analyzer.analyze(&signal[..63], &token).unwrap_err(), ScoreError::LengthMismatch);
        let mut bad = signal.clone();
        bad[5] = f32::NAN;
        assert_eq!(analyzer.analyze(&bad, &token).unwrap_err(), ScoreError::NotFinite);
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(analyzer.analyze(&signal, &canceled).unwrap_err(), ScoreError::Canceled);
    });
}
