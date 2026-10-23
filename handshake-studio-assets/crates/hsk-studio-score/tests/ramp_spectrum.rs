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

/// Cancels its token the moment a block reaches it, then forwards to the ramp, so the ramp's own
/// first poll sees the cancel (the early-return path before any sample work).
struct CancelAtEntry<'a>(&'a mut RampedGain, &'a CancellationToken);
impl Processor for CancelAtEntry<'_> {
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, i: &ChannelLayout, o: &ChannelLayout) -> bool {
        self.0.accepts(i, o)
    }
    fn process(&mut self, input: &InputBlock<'_>, out: &mut OutputBlock, cancel: &CancellationToken) -> Result<(), ScoreError> {
        self.1.cancel();
        self.0.process(input, out, cancel)
    }
    fn reset(&mut self) {
        self.0.reset();
    }
    fn commit(&mut self) {
        self.0.commit();
    }
}

/// Passes audio through; when armed it cancels and refuses, so every stage after it never runs.
struct Fuse(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Processor for Fuse {
    fn identity(&self) -> String {
        String::from("fuse")
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, i: &ChannelLayout, o: &ChannelLayout) -> bool {
        i == o
    }
    fn process(&mut self, input: &InputBlock<'_>, out: &mut OutputBlock, cancel: &CancellationToken) -> Result<(), ScoreError> {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            cancel.cancel();
            return Err(ScoreError::Canceled);
        }
        Unity.process(input, out, cancel)
    }
}

#[test]
fn canceled_ramp_block_never_steps_alone_or_in_a_chain() {
    bounded_case("canceled_ramp_block_never_steps_alone_or_in_a_chain", || {
        let engine = Engine::new(DomainId::parse(RESOURCE).unwrap(), 1);
        let ones = [1.0_f32; 4];
        let sp = BlockSpec { sample_rate_hz: 48_000, layout: mono(), frames: 4, start_sample: 0 };
        let channels: [&[f32]; 1] = [&ones];
        let input = InputBlock::new(&sp, &channels);
        let mut out = OutputBlock::with_capacity(mono(), 4).unwrap();

        // Alone: A ramps 0 -> 1 and publishes; B is canceled at the ramp's first poll (before the
        // pre-block value used to be saved); C must continue flat at 1.0, not re-ramp from 0.
        let mut ramp = RampedGain::new(0.0).unwrap();
        ramp.set_target(1.0).unwrap();
        engine.process(&ctx(), &input, &mut ramp, &mut out, &CancellationToken::default()).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[0.0, 0.25, 0.5, 0.75]);
        ramp.set_target(0.0).unwrap();
        let before = ramp.identity();
        let token = CancellationToken::default();
        let refused = engine.process(&ctx(), &input, &mut CancelAtEntry(&mut ramp, &token), &mut out, &token);
        assert_eq!(refused.unwrap_err(), ScoreError::Canceled);
        assert_eq!(ramp.identity(), before, "a canceled block leaves the ramp exactly as before the block");
        assert_eq!((ramp.current(), ramp.target()), (1.0, 0.0));
        engine.process(&ctx(), &input, &mut ramp, &mut out, &CancellationToken::default()).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[1.0, 0.75, 0.5, 0.25], "ramp resumes from the published value");

        // A direct reset with nothing in flight (bounce start, bypass toggle) changes nothing.
        let settled = ramp.identity();
        ramp.reset();
        assert_eq!(ramp.identity(), settled);

        // In a chain [ramp, fuse, ramp]: the canceled block runs the first ramp but never reaches
        // the second. Chain::reset touches every stage; the second must keep its published value.
        let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (mut first, mut last) = (RampedGain::new(0.0).unwrap(), RampedGain::new(0.0).unwrap());
        first.set_target(1.0).unwrap();
        last.set_target(1.0).unwrap();
        let stage = |processor: Box<dyn Processor + Send>| ChainStage { processor, output: mono() };
        let mut chain = Chain::new(
            mono(),
            4,
            vec![stage(Box::new(first)), stage(Box::new(Fuse(armed.clone()))), stage(Box::new(last))],
        )
        .unwrap();
        engine.process(&ctx(), &input, &mut chain, &mut out, &CancellationToken::default()).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[0.0, 0.0625, 0.25, 0.5625], "both ramps 0 -> 1: (n/4)^2");
        let after_a = chain.identity();
        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let refused = engine.process(&ctx(), &input, &mut chain, &mut out, &CancellationToken::default());
        assert_eq!(refused.unwrap_err(), ScoreError::Canceled);
        assert_eq!(chain.identity(), after_a, "canceled block: every stage keeps its pre-block state");
        armed.store(false, std::sync::atomic::Ordering::SeqCst);
        engine.process(&ctx(), &input, &mut chain, &mut out, &CancellationToken::default()).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[1.0; 4], "no gain step after the canceled block");

        // A block discarded at the publication gate still reverts the ramp that ran in it
        // (covered above by CancelAfter); a published block is never reverted by a later reset.
        let published = chain.identity();
        chain.reset();
        assert_eq!(chain.identity(), published);
    });
}
