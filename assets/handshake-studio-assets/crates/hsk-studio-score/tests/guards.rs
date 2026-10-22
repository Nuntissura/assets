//! Admission guards (lane SMB-SCORE-GUARDS, register item PLR-015): the sample-rate gate admits
//! exactly the tick divisors, and nothing that holds a NaN or infinity is published or handed to a
//! sink, even when every input and every parameter is finite. Every expected value is derived by
//! hand in the comments: `TICKS_PER_SECOND = 254_016_000_000`, `f32::MAX = 3.4028235e38`.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_score::*;

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
fn engine() -> Engine {
    Engine::new(DomainId::parse(RESOURCE).unwrap(), 1)
}
fn mono() -> ChannelLayout {
    ChannelLayout::mono(7)
}
fn spec(rate: u32, frames: usize) -> BlockSpec {
    BlockSpec { sample_rate_hz: rate, layout: mono(), frames, start_sample: 0 }
}
fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|s| s.to_bits()).collect()
}
fn stage(processor: impl Processor + Send + 'static, output: ChannelLayout) -> ChainStage {
    ChainStage { processor: Box::new(processor), output }
}
/// One mono block at 48 kHz through `proc`.
fn run(proc: &mut dyn Processor, samples: &[f32], out: &mut OutputBlock) -> Result<BlockReceipt, ScoreError> {
    let sp = spec(48_000, samples.len());
    let channels: [&[f32]; 1] = [samples];
    engine().process(&ctx(), &InputBlock::new(&sp, &channels), proc, out, &CancellationToken::default())
}

#[test]
fn rate_gate_admits_exactly_the_tick_divisors() {
    bounded_case("rate_gate_admits_exactly_the_tick_divisors", || {
        // 254_016_000_000 = 2^12 * 3^4 * 5^6 * 7^2; hand-divided ticks per sample.
        let admitted = [
            (8_000_u32, 31_752_000_u64),
            (11_025, 23_040_000),
            (44_100, 5_760_000),
            (48_000, 5_292_000),
            (192_000, 1_323_000),
            (705_600, 360_000),
            (768_000, 330_750),
        ];
        let silence = [0.0_f32; 8];
        let channels: [&[f32]; 1] = [&silence];
        for (rate, ticks) in admitted {
            assert_eq!(ticks_per_sample(rate), Ok(ticks), "{rate}");
            let mut out = OutputBlock::with_capacity(mono(), 8).unwrap();
            let sp = spec(rate, 8);
            let receipt = engine()
                .process(&ctx(), &InputBlock::new(&sp, &channels), &mut Unity, &mut out, &CancellationToken::default())
                .unwrap_or_else(|e| panic!("{rate} refused: {e}"));
            assert_eq!(receipt.end_ticks().unwrap().value(), 8 * ticks, "{rate}");
            assert!(HardLimiter::new(rate, 1, HardLimiterParams::default()).is_ok(), "{rate}");
            assert!(Equalizer::new(rate, 1, &[], 0.0).is_ok(), "{rate}");
        }

        // Refused: zero; 11_000 (factor 11); 22_051, 44_101, 47_999 (= 7 * 6857) and 96_001, none
        // of which is a product of 2, 3, 5 and 7 alone (odd, not a multiple of 3 or 5, not a power
        // of 7 except for the factor 7 of 47_999); 768_001 and u32::MAX are above the bound, and
        // 1_000_000 = 2^6 * 5^6 divides the tick rate but is above the bound as well.
        let mut out = OutputBlock::with_capacity(mono(), 8).unwrap();
        let sp = spec(48_000, 8);
        engine()
            .process(&ctx(), &InputBlock::new(&sp, &channels), &mut Unity, &mut out, &CancellationToken::default())
            .unwrap();
        let (before, published) = (bits(out.backing()), (out.published_frames(), out.published_start_sample()));
        for rate in [0_u32, 11_000, 22_051, 44_101, 47_999, 96_001, 768_001, 1_000_000, u32::MAX] {
            assert_eq!(ticks_per_sample(rate), Err(ScoreError::InvalidRate), "{rate}");
            let sp = spec(rate, 8);
            let result = engine().process(&ctx(), &InputBlock::new(&sp, &channels), &mut Unity, &mut out, &CancellationToken::default());
            assert_eq!(result.unwrap_err(), ScoreError::InvalidRate, "block at {rate}");
            // A refused block writes nothing and leaves the previous publication alone.
            assert_eq!((bits(out.backing()), (out.published_frames(), out.published_start_sample())), (before.clone(), published));
            assert_eq!(HardLimiter::new(rate, 1, HardLimiterParams::default()).unwrap_err(), ScoreError::InvalidRate, "limiter {rate}");
            assert_eq!(Equalizer::new(rate, 1, &[], 0.0).unwrap_err(), ScoreError::InvalidRate, "equalizer {rate}");
        }
    });
}

#[test]
fn overflowing_stages_are_discarded_not_published() {
    bounded_case("overflowing_stages_are_discarded_not_published", || {
        let mut out = OutputBlock::with_capacity(mono(), 16).unwrap();

        // Gain f32::MAX is a finite parameter; the largest finite products publish unchanged.
        let mut max_gain = Gain::new(f32::MAX).unwrap();
        run(&mut max_gain, &[1.0, -1.0, 0.5, 0.0], &mut out).unwrap();
        assert_eq!(bits(out.published_channel(0).unwrap()), bits(&[f32::MAX, -f32::MAX, f32::MAX * 0.5, 0.0]));
        // 2.0 * f32::MAX is infinity: the whole block is refused and nothing stays published.
        assert_eq!(run(&mut max_gain, &[1.0, 2.0, 1.0, 1.0], &mut out).unwrap_err(), ScoreError::NotFinite);
        assert_eq!((out.published_frames(), out.published_channel(0)), (0, None));

        // Channel mix: frame 0 is MAX + MAX = inf, frame 1 is inf + -inf = NaN.
        let mut mix = ChannelMix::new(2, 1, &[f32::MAX, f32::MAX]).unwrap();
        let stereo = |frames| BlockSpec { sample_rate_hz: 48_000, layout: ChannelLayout::stereo(), frames, start_sample: 0 };
        let token = CancellationToken::default();
        let (left, right) = ([1.0_f32, 2.0], [1.0_f32, -2.0]);
        let channels: [&[f32]; 2] = [&left, &right];
        let refused = engine().process(&ctx(), &InputBlock::new(&stereo(2), &channels), &mut mix, &mut out, &token);
        assert_eq!(refused.unwrap_err(), ScoreError::NotFinite);
        // Control: MAX + -MAX = 0 and 0.5 * MAX + -0.5 * MAX = 0 stay finite and publish.
        let (left, right) = ([1.0_f32, 0.5], [-1.0_f32, -0.5]);
        let channels: [&[f32]; 2] = [&left, &right];
        engine().process(&ctx(), &InputBlock::new(&stereo(2), &channels), &mut mix, &mut out, &token).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[0.0, 0.0]);

        // Equalizer: master +48 dB is 10^2.4 = 251.18864315 (f64 filter, cast to f32 at the end).
        // 1e37 * 251.19 = 2.51e39 is above f32::MAX, 1e36 * 251.19 = 2.51e38 is below it.
        let mut loud = Equalizer::new(48_000, 1, &[], 48.0).unwrap();
        assert_eq!(run(&mut loud, &[1.0e37], &mut out).unwrap_err(), ScoreError::NotFinite);
        run(&mut loud, &[1.0e36, -1.0e36], &mut out).unwrap();
        let published = out.published_channel(0).unwrap();
        assert!((f64::from(published[0]) / 2.511_886_431_509_58e38 - 1.0).abs() < 1.0e-6);
        assert_eq!(published[1], -published[0]);

        // Ramp 1.0 -> f32::MAX over 8 frames of 4.0: frame 3 reads 4 * (1 + MAX * 3/8) = 1.5 * MAX.
        let mut ramp = RampedGain::new(1.0).unwrap();
        ramp.set_target(f32::MAX).unwrap();
        assert_eq!(run(&mut ramp, &[4.0; 8], &mut out).unwrap_err(), ScoreError::NotFinite);
        // The discarded block did not move the ramp: it still starts from 1.0, so a flat target of
        // 1.0 gives 0.5 * 1.0 on every frame (a ramp left at f32::MAX would not).
        assert_eq!((ramp.current(), ramp.target()), (1.0, f32::MAX));
        ramp.set_target(1.0).unwrap();
        run(&mut ramp, &[0.5; 4], &mut out).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[0.5; 4]);
    });
}

#[test]
fn overflow_never_reaches_a_sink_or_a_later_stage() {
    bounded_case("overflow_never_reaches_a_sink_or_a_later_stage", || {
        let token = CancellationToken::default();

        // A chain whose first stage overflows: the limiter after it would clamp the infinity into
        // full-scale looking output (and its 341 sample delay hides it from a 4 frame block), so
        // only the per-stage check can refuse this block.
        let mut chain = Chain::new(
            mono(),
            16,
            vec![
                stage(Gain::new(f32::MAX).unwrap(), mono()),
                stage(HardLimiter::new(48_000, 1, HardLimiterParams::default()).unwrap(), mono()),
            ],
        )
        .unwrap();
        let mut small = OutputBlock::with_capacity(mono(), 16).unwrap();
        assert_eq!(run(&mut chain, &[1.0, 2.0, 1.0, 1.0], &mut small).unwrap_err(), ScoreError::NotFinite);
        assert_eq!(small.published_frames(), 0);

        // Bounce: 768 frames in blocks of 256, frame 600 is 2.0 so block 2 (frames 512..768)
        // overflows. Blocks 0 and 1 are delivered (all f32::MAX), block 2 never reaches the sink.
        let mut input = vec![1.0_f32; 768];
        input[600] = 2.0;
        let layout = mono();
        let channels: [&[f32]; 1] = [&input];
        let request = BounceRequest { sample_rate_hz: 48_000, layout: &layout, start_sample: 0, channels: &channels, block_frames: 256 };
        let mut block = OutputBlock::with_capacity(mono(), 256).unwrap();
        let mut delivered: Vec<f32> = Vec::new();
        let mut sink = |chunk: BounceChunk<'_>| {
            delivered.extend_from_slice(chunk.channels[0]);
            Ok(())
        };
        let mut gain = Gain::new(f32::MAX).unwrap();
        let result = engine().bounce(&ctx(), &request, &mut gain, &mut block, &mut sink, &token);
        assert_eq!(result.unwrap_err(), ScoreError::NotFinite);
        assert_eq!(delivered.len(), 512);
        assert!(delivered.iter().all(|s| s.to_bits() == f32::MAX.to_bits()));

        // Converter graph, streaming and whole-clip: pre gain 1e38 on 2000 frames of 1.0 with a 4.0
        // at frame 1500 (4e38 > f32::MAX). 1e38 leaves the converter's overshoot finite, so the
        // refusal comes from the pre stage block holding frame 1500 (frames 1280..1536); the sink
        // gets only finite frames, fewer than the ceil(2000 * 44100 / 48000) = 1838 requested.
        for whole in [false, true] {
            let mut input = vec![1.0_f32; 2000];
            input[1500] = 4.0;
            let channels: [&[f32]; 1] = [&input];
            let request = BounceRequest { sample_rate_hz: 48_000, layout: &layout, start_sample: 0, channels: &channels, block_frames: 256 };
            let mut pre = Gain::new(1.0e38).unwrap();
            let mut post = Unity;
            let mut resampler = ClipResampler::new(48_000, 44_100, 1, 256).unwrap();
            let mut pre_out = OutputBlock::with_capacity(mono(), 256).unwrap();
            let mut out = OutputBlock::with_capacity(mono(), 256).unwrap();
            let stages = ResampledBounce { pre: &mut pre, pre_out: &mut pre_out, resampler: &mut resampler, post: &mut post, out: &mut out };
            let mut delivered: Vec<f32> = Vec::new();
            let mut sink = |chunk: BounceChunk<'_>| {
                delivered.extend_from_slice(chunk.channels[0]);
                Ok(())
            };
            let engine = engine();
            let result = if whole {
                engine.bounce_resampled_whole_clip(&ctx(), &request, stages, &mut sink, &token)
            } else {
                engine.bounce_resampled(&ctx(), &request, stages, &mut sink, &token)
            };
            assert_eq!(result.unwrap_err(), ScoreError::NotFinite, "whole clip: {whole}");
            assert!(delivered.len() < 1838 && delivered.iter().all(|s| s.is_finite()), "whole clip: {whole}");
            assert_eq!(engine.reserve().map(|_| ()), Ok(()), "the slot is released on every exit");
        }
    });
}

/// Alternating-sign clip that is symmetric around the midpoint between frames `n0` and `n0 + 1`
/// (`n0 = frames / 2`): `+A -A +A ... +A | +A -A +A ...`. The sinc kernel of a 2x upsampler has
/// the same alternating signs at half-integer offsets (`sinc(k + 1/2) = (-1)^k / (pi (k + 1/2))`),
/// so every tap adds up at that midpoint and the output there is `A * sum|h|`, well above `A`.
fn midpoint_pattern(frames: usize, amplitude: f32) -> Vec<f32> {
    let n0 = frames / 2;
    (0..frames)
        .map(|n| {
            let k = if n <= n0 { n0 - n } else { n - n0 - 1 };
            if k % 2 == 0 { amplitude } else { -amplitude }
        })
        .collect()
}

fn stream_all(resampler: &mut ClipResampler, input: &[f32], emitted: &mut Vec<f32>) -> Result<StreamReceipt, ScoreError> {
    resampler.begin_stream(input.len())?;
    let mut emit = |planes: &[&[f32]]| {
        emitted.extend_from_slice(planes[0]);
        Ok(())
    };
    for piece in input.chunks(300) {
        resampler.push_stream(&[piece], &mut emit)?;
    }
    resampler.finish_stream(&mut emit)
}

#[test]
fn mix_converter_and_spectrum_refuse_overflow() {
    bounded_case("mix_converter_and_spectrum_refuse_overflow", || {
        let token = CancellationToken::default();

        // Mix: one send at level f32::MAX publishes; two such sends sum to +inf at frame 0 and
        // to -inf at frame 2.
        let samples = [1.0_f32, 0.5, -1.0, 0.0];
        let sp = spec(48_000, 4);
        let channels: [&[f32]; 1] = [&samples];
        let send = SendRoute { input: InputBlock::new(&sp, &channels), level: f32::MAX };
        let mut out = OutputBlock::with_capacity(mono(), 4).unwrap();
        engine().mix(&ctx(), &[send], &mut out, &token).unwrap();
        assert_eq!(bits(out.published_channel(0).unwrap()), bits(&[f32::MAX, f32::MAX * 0.5, -f32::MAX, 0.0]));
        assert_eq!(engine().mix(&ctx(), &[send, send], &mut out, &token).unwrap_err(), ScoreError::NotFinite);
        assert_eq!(out.published_frames(), 0);

        // Converter, whole clip: the control clip (amplitude 1) must read above 1 at the midpoint,
        // which is what makes the f32::MAX clip overflow by arithmetic, not by luck.
        let mut resampler = ClipResampler::new(24_000, 48_000, 1, 256).unwrap();
        let capacity = resampler.output_capacity_for(1024);
        let mut big = OutputBlock::with_capacity(mono(), capacity).unwrap();
        let sp = spec(24_000, 1024);
        let unit = midpoint_pattern(1024, 1.0);
        let huge = midpoint_pattern(1024, f32::MAX);
        let convert = |resampler: &mut ClipResampler, big: &mut OutputBlock, clip: &[f32]| {
            let channels: [&[f32]; 1] = [clip];
            engine().resample(&ctx(), &InputBlock::new(&sp, &channels), resampler, big, &token)
        };
        let receipt = convert(&mut resampler, &mut big, &unit).unwrap();
        assert_eq!(receipt.actual_output_frames(), 2048);
        assert!(receipt.block.peak()[0] > 1.0, "midpoint gain {}", receipt.block.peak()[0]);
        assert_eq!(convert(&mut resampler, &mut big, &huge).unwrap_err(), ScoreError::NotFinite);
        assert_eq!(big.published_frames(), 0);
        assert!(convert(&mut resampler, &mut big, &unit).is_ok(), "the converter is reusable after a refusal");

        // Converter, streaming: the chunk holding the overflow never reaches the receiver.
        let mut emitted = Vec::new();
        let receipt = stream_all(&mut resampler, &unit, &mut emitted).unwrap();
        assert_eq!((receipt.delivered_frames, emitted.len()), (2048, 2048));
        let mut emitted = Vec::new();
        assert_eq!(stream_all(&mut resampler, &huge, &mut emitted).unwrap_err(), ScoreError::NotFinite);
        assert!(emitted.len() < 2048 && emitted.iter().all(|s| s.is_finite()));
        let mut emitted = Vec::new();
        assert!(stream_all(&mut resampler, &unit, &mut emitted).is_ok(), "the converter is reusable after a refusal");

        // Spectrum, N = 8: x = A * [1, 1, 0, -1, -1, -1, 0, 1] (sign of cos(2 pi n / 8)) against the
        // periodic Hann window [0, .1464, .5, .8536, 1, .8536, .5, .1464] (sum 4) gives
        // X[1] = (1 + sqrt 2) * A, bin 1 = 2 * |X[1]| / 4 = 1.2071068 * A.
        let mut analyzer = SpectrumAnalyzer::new(8).unwrap();
        let square = |a: f32| [a, a, 0.0, -a, -a, -a, 0.0, a];
        let bins = analyzer.analyze(&square(1.0), &token).unwrap();
        assert!((f64::from(bins[1]) - (1.0 + 2.0_f64.sqrt()) / 2.0).abs() < 1.0e-6, "{}", bins[1]);
        // 1.2071068 * 3.0e38 = 3.62e38 is above f32::MAX although every sample is finite.
        assert_eq!(analyzer.analyze(&square(3.0e38), &token).unwrap_err(), ScoreError::NotFinite);
    });
}
