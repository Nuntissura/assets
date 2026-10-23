use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_score::*;
use std::cell::RefCell;

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

fn ctx(revision: u64) -> RequestContext {
    RequestContext {
        resource: DomainId::parse(RESOURCE).unwrap(),
        actor: Some(
            ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
                .unwrap(),
        ),
        expected_revision: revision,
    }
}
fn engine() -> Engine {
    Engine::new(DomainId::parse(RESOURCE).unwrap(), 7)
}
fn mono() -> ChannelLayout {
    ChannelLayout::mono(7)
}

#[test]
fn resampler_alignment_is_exact_for_impulses() {
    bounded_case("resampler_alignment_is_exact_for_impulses", || {
        let (engine, token) = (engine(), CancellationToken::default());
        // (input Hz, output Hz, impulse index, expected output index, pad, drop). The pad/drop
        // values follow from ratio p/q: pad = smallest s with q | 128 + s giving a non-negative
        // lag, drop = (128 + s) * p / q - 1 - floor(128 * p / q); e.g. 147/160 -> 32 and 29.
        let cases = [
            (48_000_u32, 44_100_u32, 1600_usize, 1470_usize, 32_usize, 29_usize),
            (44_100, 48_000, 1470, 1600, 19, 20),
            (32_000, 48_000, 2000, 3000, 2, 2),
            (48_000, 96_000, 2000, 4000, 1, 1),
            (48_000, 48_000, 2000, 2000, 1, 0),
        ];
        for (rate_in, rate_out, at, expected, pad, drop) in cases {
            let frames = 6000;
            let mut impulse = vec![0.0_f32; frames];
            impulse[at] = 1.0;
            let sp = BlockSpec { sample_rate_hz: rate_in, layout: mono(), frames, start_sample: 0 };
            let channels: [&[f32]; 1] = [&impulse];
            let mut resampler = ClipResampler::new(rate_in, rate_out, 1, 1024).unwrap();
            assert_eq!((resampler.pad_input_frames(), resampler.drop_output_frames()), (pad, drop), "{rate_in}->{rate_out}");
            let mut out = OutputBlock::with_capacity(mono(), resampler.output_capacity_for(frames)).unwrap();
            let receipt = engine
                .resample(&ctx(7), &InputBlock::new(&sp, &channels), &mut resampler, &mut out, &token)
                .unwrap();
            let want = (frames as u64 * u64::from(rate_out)).div_ceil(u64::from(rate_in)) as usize;
            assert_eq!((receipt.requested_output_frames, receipt.actual_output_frames()), (want, want));
            let got = out.published_channel(0).unwrap();
            let peak_index = (0..got.len()).max_by(|a, b| got[*a].abs().total_cmp(&got[*b].abs())).unwrap();
            assert_eq!(peak_index, expected, "{rate_in}->{rate_out}: impulse must land exactly on its image");
            // A band-limited impulse response is symmetric about its peak when aligned; a residual
            // offset of delta frames tilts the first neighbours by ~2 * gain * delta (the slope of
            // sinc at one sample is -1). Unaligned rubato output measured 0.4..1.0 frames;
            // the aligned residual measures 0.007..0.010 frames (about 1/128, the sinc table step).
            let peak = f64::from(got[expected]);
            let delta = f64::from(got[expected - 1] - got[expected + 1]) / (2.0 * peak);
            assert!(delta.abs() < 0.02, "{rate_in}->{rate_out}: residual offset {delta} frames");
            // The anti-alias cutoff scales a downsampled impulse; it never exceeds unity.
            assert!((0.8..1.01).contains(&peak), "{rate_in}->{rate_out}: peak {peak}");
        }
    });
}

#[test]
fn resampler_publishes_the_exact_integer_frame_count() {
    bounded_case("resampler_publishes_the_exact_integer_frame_count", || {
        let (engine, token) = (engine(), CancellationToken::default());
        // Independent integer oracle: ceil(frames * out / in); never an f64 ratio.
        let want = |frames: usize, rate_in: u32, rate_out: u32| (frames as u128 * u128::from(rate_out)).div_ceil(u128::from(rate_in)) as usize;
        let run = |resampler: &mut ClipResampler, rate_in: u32, frames: usize, start: u64| {
            let tone: Vec<f32> = (0..frames).map(|n| (0.5 * (std::f64::consts::TAU * 997.0 * n as f64 / f64::from(rate_in)).sin()) as f32).collect();
            let sp = BlockSpec { sample_rate_hz: rate_in, layout: mono(), frames, start_sample: start };
            let channels: [&[f32]; 1] = [&tone];
            // Capacity is exactly the sizing helper's bound.
            let mut out = OutputBlock::with_capacity(mono(), resampler.output_capacity_for(frames)).unwrap();
            let receipt = engine
                .resample(&ctx(7), &InputBlock::new(&sp, &channels), resampler, &mut out, &token)
                .unwrap();
            (receipt, out)
        };
        let pairs = [(44_100_u32, 48_000_u32), (48_000, 44_100), (32_000, 48_000), (48_000, 96_000), (48_000, 48_000)];
        let mut resamplers: Vec<ClipResampler> = pairs.iter().map(|(i, o)| ClipResampler::new(*i, *o, 1, 1024).unwrap()).collect();

        // One second at every measured pair publishes exactly one second (the 44.1k -> 48k case
        // is the one where rubato's f64 ratio rounds 48000 up to 48001).
        for ((rate_in, rate_out), resampler) in pairs.iter().copied().zip(&mut resamplers) {
            let frames = rate_in as usize;
            let exact = rate_out as usize;
            assert_eq!(want(frames, rate_in, rate_out), exact, "{rate_in}->{rate_out}");
            assert_eq!(resampler.output_frames_for(frames), Some(exact));
            let (receipt, out) = run(resampler, rate_in, frames, 0);
            assert_eq!((receipt.requested_output_frames, receipt.actual_output_frames()), (exact, exact), "{rate_in}->{rate_out}");
            assert_eq!((out.published_frames(), out.published_channel(0).unwrap().len()), (exact, exact), "{rate_in}->{rate_out}");
            assert_eq!((receipt.block.start_sample, receipt.block.end_sample), (0, exact as u64), "{rate_in}->{rate_out}");
            assert!(receipt.trimmed_tail_frames <= 2, "{rate_in}->{rate_out}: tail {}", receipt.trimmed_tail_frames);
            assert!(out.backing()[exact..].iter().all(|s| *s == 0.0), "{rate_in}->{rate_out}: nothing past the published count");
        }
        // 44100 frames at 44.1 kHz -> 48 kHz: rubato yields one frame past the exact second; the
        // receipt records that trimmed frame instead of publishing it.
        let (receipt, _) = run(&mut resamplers[0], 44_100, 44_100, 0);
        assert_eq!((receipt.requested_output_frames, receipt.trimmed_tail_frames), (48_000, 1));

        // Adjacent clips on the output timeline tile with no overlap: the second clip starts at
        // sample 147_000 + 44_100 on the input timeline and exactly at the first clip's end.
        let (first, _) = run(&mut resamplers[0], 44_100, 44_100, 147_000);
        let (second, _) = run(&mut resamplers[0], 44_100, 44_100, 147_000 + 44_100);
        assert_eq!((first.block.start_sample, first.block.end_sample), (160_000, 208_000));
        assert_eq!(second.block.start_sample, first.block.end_sample, "no one-sample overlap with the next clip");

        // Length sweep: short lengths and the neighbours of one second land on the exact integer
        // count, so the shortfall guard in Engine::resample never fires.
        for ((rate_in, rate_out), resampler) in pairs[..4].iter().copied().zip(&mut resamplers) {
            for frames in (1..=80).chain([147, 160, 441, 1470, 4799, 4801]) {
                let (receipt, out) = run(resampler, rate_in, frames, 0);
                let exact = want(frames, rate_in, rate_out);
                assert_eq!((receipt.requested_output_frames, receipt.actual_output_frames(), out.published_frames()), (exact, exact, exact), "{rate_in}->{rate_out} x {frames}");
                assert!(out.published_channel(0).unwrap().iter().all(|s| s.is_finite()));
            }
        }
    });
}

fn fixture(frames: usize) -> (Vec<f32>, Vec<f32>) {
    let pattern = [0.0_f32, 0.25, -0.25, 0.5];
    let left: Vec<f32> = (0..frames).map(|n| pattern[n % 4]).collect();
    let right = left.iter().map(|s| -s).collect();
    (left, right)
}

/// `(start_sample, planes)` of one delivered chunk.
type Chunk = (u64, Vec<Vec<f32>>);

fn bounce(
    proc: &mut dyn Processor,
    block_frames: usize,
    rate: u32,
    start_sample: u64,
    channels: &[&[f32]],
) -> (Result<BounceReceipt, ScoreError>, Vec<Chunk>) {
    let engine = engine();
    let layout = ChannelLayout::stereo();
    let request = BounceRequest { sample_rate_hz: rate, layout: &layout, start_sample, channels, block_frames };
    let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
    let chunks = RefCell::new(Vec::new());
    let mut sink = |chunk: BounceChunk<'_>| {
        chunks.borrow_mut().push((chunk.start_sample, chunk.channels.iter().map(|c| c.to_vec()).collect()));
        Ok(())
    };
    let receipt = engine.bounce(&ctx(7), &request, proc, &mut out, &mut sink, &CancellationToken::default());
    (receipt, chunks.into_inner())
}

#[test]
fn bounce_receipt_is_exact_and_digest_matches_independent_oracle() {
    bounded_case("bounce_receipt_is_exact_and_digest_matches_independent_oracle", || {
        // SHA-256 known answers (NIST / hashlib), including a multi-block message.
        assert_eq!(to_hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(to_hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(to_hex(&sha256(&[b'a'; 1000])), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");

        // Gain 0.5 over 1000 stereo frames starting at one second.
        let (left, right) = fixture(1000);
        let channels: [&[f32]; 2] = [&left, &right];
        let gain_digest = "5482ae8cd3b56e89dcd1431560c78a8f77ce9fbd705ea8509aa96af59628a8c0";
        for block in [256_usize, 100, 77, 1] {
            let (receipt, chunks) = bounce(&mut Gain::new(0.5).unwrap(), block, 48_000, 48_000, &channels);
            let receipt = receipt.unwrap();
            assert_eq!((receipt.requested_frames, receipt.actual_frames), (1000, 1000));
            assert_eq!(receipt.digest_hex(), gain_digest, "digest must not depend on block size {block}");
            // Chunks tile the clip contiguously on the input timeline with exact sample values.
            let mut next = 48_000;
            let mut joined = [Vec::new(), Vec::new()];
            for (start, planes) in &chunks {
                assert_eq!(*start, next);
                next += planes[0].len() as u64;
                joined[0].extend_from_slice(&planes[0]);
                joined[1].extend_from_slice(&planes[1]);
            }
            assert_eq!(next, 49_000);
            assert!(joined[0].iter().zip(&left).all(|(g, s)| g.to_bits() == (s * 0.5).to_bits()));
        }
        let (receipt, _) = bounce(&mut Gain::new(0.5).unwrap(), 256, 48_000, 48_000, &channels);
        let receipt = receipt.unwrap();
        assert_eq!((receipt.start_sample, receipt.end_sample, receipt.blocks, receipt.revision), (48_000, 49_000, 4, 7));
        assert_eq!(receipt.start_ticks.value(), 254_016_000_000, "48000 samples at 48 kHz is exactly one second");
        assert_eq!(receipt.end_ticks.value(), 49_000 * 5_292_000);
        assert_eq!(receipt.duration_ticks(), 1000 * 5_292_000);
        assert_eq!((receipt.labels(), receipt.peak(), receipt.latency_dropped_frames), (&[100, 101][..], &[0.25, 0.25][..], 0));
        let expected_rms = 0.153_093_11_f32;
        assert!(receipt.rms().iter().all(|r| (r - expected_rms).abs() < 1e-7), "{:?}", receipt.rms());
        assert_eq!(
            receipt.chain_identity_hex(),
            "bfcb1642cb66efd664eea7fcd872ab27f9c206f9ede82697e9082280118e7283",
            "SHA-256 over domain, crate version 0.1.0, NUL and 'gain:linear=3f000000'"
        );

        // A latency-compensated transparent limiter returns the input exactly: 240 extra silent
        // frames are rendered, the first 240 outputs dropped, the digest equals the input's.
        let transparent = HardLimiterParams { max_amp_db: 0.0, input_boost_db: 0.0, lookahead_ms: 5.0, release_ms: 40.0 };
        let mut limiter = HardLimiter::new(48_000, 2, transparent).unwrap();
        let (receipt, _) = bounce(&mut limiter, 256, 48_000, 0, &channels);
        let receipt = receipt.unwrap();
        assert_eq!((receipt.requested_frames, receipt.actual_frames, receipt.latency_dropped_frames, receipt.blocks), (1000, 1000, 240, 5));
        assert_eq!(receipt.digest_hex(), "70a930167c8ba16dbd9098b15a1e53cfc8f26608fc15e29725d6b91551d81f9e");
        assert_ne!(receipt.chain_identity, bounce(&mut Gain::new(0.5).unwrap(), 256, 48_000, 0, &channels).0.unwrap().chain_identity);

        // Refusals happen before any block runs or any chunk is delivered.
        let mut nan = left.clone();
        nan[999] = f32::NAN;
        let bad: [&[f32]; 2] = [&nan, &right];
        let mono_only: [&[f32]; 1] = [&left];
        let short: [&[f32]; 2] = [&left, &right[..999]];
        let mut gain = Gain::new(0.5).unwrap();
        for (name, expected, result) in [
            ("block over capacity", ScoreError::CapacityExceeded, bounce(&mut gain, 257, 48_000, 0, &channels)),
            ("block zero", ScoreError::OutOfRange, bounce(&mut gain, 0, 48_000, 0, &channels)),
            ("non-finite", ScoreError::NotFinite, bounce(&mut gain, 256, 48_000, 0, &bad)),
            ("channel count", ScoreError::ChannelMismatch, bounce(&mut gain, 256, 48_000, 0, &mono_only)),
            ("length", ScoreError::LengthMismatch, bounce(&mut gain, 256, 48_000, 0, &short)),
            ("rate", ScoreError::InvalidRate, bounce(&mut gain, 256, 47_999, 0, &channels)),
            ("limiter rate", ScoreError::InvalidRate, bounce(&mut limiter, 256, 44_100, 0, &channels)),
            ("position overflow", ScoreError::PositionOverflow, bounce(&mut gain, 256, 48_000, u64::MAX - 5, &channels)),
        ] {
            assert_eq!(result.0.unwrap_err(), expected, "{name}");
            assert!(result.1.is_empty(), "{name}: sink must not be called");
        }

        // Mid-bounce cancel from the sink: no receipt, processor reset, error is Canceled.
        let (engine, token) = (engine(), CancellationToken::default());
        let layout = ChannelLayout::stereo();
        let request = BounceRequest { sample_rate_hz: 48_000, layout: &layout, start_sample: 0, channels: &channels, block_frames: 256 };
        let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
        let delivered = RefCell::new(0);
        let mut cancel_after_first = |_: BounceChunk<'_>| {
            *delivered.borrow_mut() += 1;
            token.cancel();
            Ok(())
        };
        let canceled = engine.bounce(&ctx(7), &request, &mut limiter, &mut out, &mut cancel_after_first, &token);
        assert_eq!(canceled.unwrap_err(), ScoreError::Canceled);
        assert_eq!((*delivered.borrow(), limiter.overshoot_events()), (1, 0));
        let mut failing = |_: BounceChunk<'_>| Err(ScoreError::Busy);
        let sink_error = engine.bounce(&ctx(7), &request, &mut gain, &mut out, &mut failing, &CancellationToken::default());
        assert_eq!(sink_error.unwrap_err(), ScoreError::Busy);
        assert_eq!(engine.reserve().map(|_| ()), Ok(()), "the slot is released on every exit");
    });
}

struct Custom;
impl Processor for Custom {
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, i: &ChannelLayout, o: &ChannelLayout) -> bool {
        i == o
    }
    fn process(&mut self, _: &InputBlock<'_>, _: &mut OutputBlock, _: &CancellationToken) -> Result<(), ScoreError> {
        Ok(())
    }
}

#[test]
fn processor_identities_are_canonical_and_sensitive() {
    bounded_case("processor_identities_are_canonical_and_sensitive", || {
        assert_eq!(Custom.identity(), "unspecified");
        assert_eq!(Unity.identity(), "unity");
        assert_eq!(Gain::new(0.5).unwrap().identity(), "gain:linear=3f000000");
        assert_eq!(Pan::new(0.0).unwrap().identity(), "pan:pan=00000000");
        assert_eq!(ChannelMix::stereo_to_mono().identity(), "channel_mix:2x1:3f000000,3f000000");
        let mut ramp = RampedGain::new(0.0).unwrap();
        ramp.set_target(1.0).unwrap();
        assert_eq!(ramp.identity(), "ramped_gain:current=00000000;target=3f800000");
        assert_eq!(
            HardLimiter::new(48_000, 2, HardLimiterParams::default()).unwrap().identity(),
            "hard_limiter:rate=48000;channels=2;max_amp=c049000000000000;input_boost=4034000000000000;lookahead=401c666666666666;release=4059000000000000"
        );
        let peaking = BiquadSpec { kind: BiquadKind::Peaking, frequency_hz: 1000.0, gain_db: 12.0, q: 1.0 };
        assert_eq!(
            Equalizer::new(48_000, 1, &[peaking], 0.0).unwrap().identity(),
            "equalizer:rate=48000;channels=1;master=0000000000000000;sections=[Peaking:408f400000000000:4028000000000000:3ff0000000000000]"
        );
        let stage = |p: Box<dyn Processor + Send>| ChainStage { processor: p, output: ChannelLayout::stereo() };
        let mut chain = Chain::new(
            ChannelLayout::stereo(),
            64,
            vec![stage(Box::new(Gain::new(0.5).unwrap())), stage(Box::new(Gain::new(2.0).unwrap()))],
        )
        .unwrap();
        let before = chain.identity();
        assert_eq!(before, "chain:[gain:linear=3f000000 > gain:linear=40000000]");
        chain.set_bypass(1, true).unwrap();
        assert_eq!(chain.identity(), "chain:[gain:linear=3f000000 > bypassed:gain:linear=40000000]");
        assert_ne!(chain.identity(), before, "bypass changes the chain identity");
        assert_ne!(Gain::new(0.5).unwrap().identity(), Gain::new(0.25).unwrap().identity());
    });
}
