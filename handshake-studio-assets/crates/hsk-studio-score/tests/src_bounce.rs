//! Bounce over a sample-rate-converting stage (STU-ARC-021): exact requested vs actual counts,
//! per-stage rate/latency facts, refusals before any sink call. Sample counts are hand computed
//! from `ceil(frames * out / in)`; no oracle value comes from the code under test.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Timescale};
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

/// `(start_sample, planes)` of one delivered chunk.
type Chunk = (u64, Vec<Vec<f32>>);

struct Graph {
    resampler: ClipResampler,
    pre_out: OutputBlock,
    out: OutputBlock,
}

fn graph(rate_in: u32, rate_out: u32, chunk_frames: usize) -> Graph {
    Graph {
        resampler: ClipResampler::new(rate_in, rate_out, 2, chunk_frames).unwrap(),
        pre_out: OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap(),
        out: OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap(),
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    engine: &Engine,
    token: &CancellationToken,
    g: &mut Graph,
    pre: &mut dyn Processor,
    post: &mut dyn Processor,
    rate_in: u32,
    start_sample: u64,
    channels: &[&[f32]],
    block_frames: usize,
    sink: &mut dyn FnMut(BounceChunk<'_>) -> Result<(), ScoreError>,
) -> Result<ResampledBounceReceipt, ScoreError> {
    let layout = ChannelLayout::stereo();
    let request = BounceRequest { sample_rate_hz: rate_in, layout: &layout, start_sample, channels, block_frames };
    let stages = ResampledBounce { pre, pre_out: &mut g.pre_out, resampler: &mut g.resampler, post, out: &mut g.out };
    engine.bounce_resampled(&ctx(7), &request, stages, sink, token)
}

/// Runs one bounce with a fresh engine and collects every delivered chunk.
fn bounce(
    g: &mut Graph,
    pre: &mut dyn Processor,
    post: &mut dyn Processor,
    rate_in: u32,
    start_sample: u64,
    channels: &[&[f32]],
    block_frames: usize,
) -> (Result<ResampledBounceReceipt, ScoreError>, Vec<Chunk>) {
    let chunks = RefCell::new(Vec::new());
    let mut sink = |chunk: BounceChunk<'_>| {
        chunks.borrow_mut().push((chunk.start_sample, chunk.channels.iter().map(|c| c.to_vec()).collect()));
        Ok(())
    };
    let engine = engine();
    let result = run(&engine, &CancellationToken::default(), g, pre, post, rate_in, start_sample, channels, block_frames, &mut sink);
    assert_eq!(engine.reserve().map(|_| ()), Ok(()), "the slot is released on every exit");
    (result, chunks.into_inner())
}

fn tone(frames: usize, rate: u32, hz: f64, amplitude: f64) -> Vec<f32> {
    (0..frames).map(|n| (amplitude * (std::f64::consts::TAU * hz * n as f64 / f64::from(rate)).sin()) as f32).collect()
}

fn stereo_tones(frames: usize, rate: u32) -> (Vec<f32>, Vec<f32>) {
    (tone(frames, rate, 997.0, 0.5), tone(frames, rate, 1500.0, 0.25))
}

/// Joins delivered chunks per channel, asserting they tile the output timeline from `start`.
fn join(chunks: &[Chunk], start: u64, max_chunk: usize) -> [Vec<f32>; 2] {
    let mut next = start;
    let mut joined = [Vec::new(), Vec::new()];
    for (at, planes) in chunks {
        assert_eq!(*at, next, "chunks are contiguous on the output timeline");
        assert!(planes[0].len() <= max_chunk && planes[0].len() == planes[1].len());
        next += planes[0].len() as u64;
        joined[0].extend_from_slice(&planes[0]);
        joined[1].extend_from_slice(&planes[1]);
    }
    joined
}

#[test]
fn counts_are_exact_for_44k_to_48k_and_back() {
    bounded_case("counts_are_exact_for_44k_to_48k_and_back", || {
        // (rate in, rate out, input frames, input start, expected output frames, output start,
        //  expected ceil excess ticks, (pad, drop)). Hand computed:
        //  44100 -> 48000: 1000 * 48000 / 44100 = 1088.43..  -> 1089; 44100 -> 48000; 441 -> 480; 1 -> 2
        //  48000 -> 44100: 1000 * 44100 / 48000 =  918.75    ->  919; 48000 -> 44100; 480 -> 441; 1 -> 1
        //  ticks/sample: 44100 Hz = 5_760_000, 48000 Hz = 5_292_000; excess = out*tps_out - in*tps_in:
        //  1089 * 5_292_000 - 1000 * 5_760_000 = 2_988_000; 919 * 5_760_000 - 1000 * 5_292_000 = 1_440_000.
        //  Starts: 147_000 @44.1k = 160_000 @48k (both 846_720_000_000 ticks).
        type Count = (u32, u32, usize, u64, usize, u64, u64, (usize, usize));
        let cases: [Count; 8] = [
            (44_100, 48_000, 1000, 147_000, 1089, 160_000, 2_988_000, (19, 20)),
            (44_100, 48_000, 44_100, 0, 48_000, 0, 0, (19, 20)),
            (44_100, 48_000, 441, 0, 480, 0, 0, (19, 20)),
            (44_100, 48_000, 1, 0, 2, 0, 5_292_000 * 2 - 5_760_000, (19, 20)),
            (48_000, 44_100, 1000, 160_000, 919, 147_000, 1_440_000, (32, 29)),
            (48_000, 44_100, 48_000, 0, 44_100, 0, 0, (32, 29)),
            (48_000, 44_100, 480, 0, 441, 0, 0, (32, 29)),
            (48_000, 44_100, 1, 0, 1, 0, 5_760_000 - 5_292_000, (32, 29)),
        ];
        for (rate_in, rate_out, frames, start, want, out_start, excess, (pad, drop)) in cases {
            let (left, right) = stereo_tones(frames, rate_in);
            let channels: [&[f32]; 2] = [&left, &right];
            let mut g = graph(rate_in, rate_out, 1024);
            let (receipt, chunks) = bounce(&mut g, &mut Unity, &mut Unity, rate_in, start, &channels, 256);
            let name = format!("{rate_in}->{rate_out} x {frames}");
            let r = receipt.unwrap_or_else(|e| panic!("{name}: {e}"));
            let delivered: usize = chunks.iter().map(|(_, planes)| planes[0].len()).sum();
            assert_eq!((r.requested_input_frames, r.requested_output_frames, r.actual_output_frames), (frames as u64, want as u64, want as u64), "{name}");
            assert_eq!(delivered, want, "{name}: the sink received exactly the requested frames, none dropped");
            assert_eq!((r.resample.requested_output_frames, r.resample.actual_output_frames()), (want, want), "{name}");
            assert_eq!((r.resample.pad_input_frames, r.resample.drop_output_frames), (pad, drop), "{name}");
            assert_eq!((r.input_start_sample, r.input_end_sample), (start, start + frames as u64), "{name}");
            assert_eq!((r.output_start_sample, r.output_end_sample), (out_start, out_start + want as u64), "{name}");
            // hand-written ticks per sample: 254_016_000_000 / 48_000 and / 44_100
            let tps = |rate: u32| if rate == 48_000 { 5_292_000_u64 } else { 5_760_000 };
            let (tps_in, tps_out) = (tps(rate_in), tps(rate_out));
            assert_eq!((r.input_start_ticks.value(), r.output_start_ticks.value()), (start * tps_in, out_start * tps_out), "{name}");
            assert_eq!((r.input_duration_ticks(), r.output_duration_ticks()), (frames as u64 * tps_in, want as u64 * tps_out), "{name}");
            assert_eq!(r.ceil_excess_ticks(), excess, "{name}");
            assert!(r.ceil_excess_ticks() < tps_out, "{name}: less than one output sample");
            // ticks per sample is accord's own conversion
            for rate in [rate_in, rate_out] {
                assert_eq!(Some(ticks_per_sample(rate).unwrap()), Timescale::new(rate).unwrap().exact_ticks_per_unit());
            }
            // Per-stage facts: counts and rates at each boundary, converter latency net of alignment.
            let [pre, src, post] = r.stages;
            assert_eq!((pre.kind, pre.input_rate_hz, pre.output_rate_hz, pre.input_frames, pre.output_frames, pre.latency_samples), (StageKind::Processor, rate_in, rate_in, frames as u64, frames as u64, 0), "{name}");
            assert_eq!((src.kind, src.input_rate_hz, src.output_rate_hz, src.input_frames, src.output_frames, src.latency_samples), (StageKind::Resampler, rate_in, rate_out, frames as u64, want as u64, 0), "{name}");
            assert_eq!((post.kind, post.input_rate_hz, post.output_rate_hz, post.input_frames, post.output_frames, post.latency_samples), (StageKind::Processor, rate_out, rate_out, want as u64, want as u64, 0), "{name}");
            assert_eq!((r.labels(), r.revision), (&[100, 101][..], 7), "{name}");
            let joined = join(&chunks, out_start, 256);
            assert_eq!((joined[0].len(), joined[1].len()), (want, want), "{name}");
            assert!(joined.iter().flatten().all(|s| s.is_finite()), "{name}");
        }
        // One second at 44.1 kHz: the converter itself yields one frame past the exact second;
        // the receipt reports the trimmed frame, the sink never sees it.
        let (left, right) = stereo_tones(44_100, 44_100);
        let channels: [&[f32]; 2] = [&left, &right];
        let mut g = graph(44_100, 48_000, 1024);
        let (receipt, chunks) = bounce(&mut g, &mut Unity, &mut Unity, 44_100, 0, &channels, 256);
        let r = receipt.unwrap();
        assert_eq!((r.resample.trimmed_tail_frames, r.actual_output_frames), (1, 48_000));
        assert_eq!(chunks.iter().map(|(_, p)| p[0].len()).sum::<usize>(), 48_000);
    });
}

#[test]
fn stage_latency_is_compensated_and_reported_per_stage() {
    bounded_case("stage_latency_is_compensated_and_reported_per_stage", || {
        // 1000 frames at 48 kHz -> 44.1 kHz: 1000 * 44100 / 48000 = 918.75 -> 919 frames.
        // Pre limiter at 48 kHz, 5 ms lookahead: round(5 * 48000 / 1000) = 240 samples.
        // Post limiter at 44.1 kHz, 5 ms lookahead: round(5 * 44100 / 1000) = round(220.5) = 221.
        // Blocks of 256: pre renders 1000 + 240 = 1240 frames = 5 blocks; post renders
        // 919 + 221 = 1140 frames = 5 blocks. Latency is compensated: counts stay 1000 and 919.
        let (left, right) = stereo_tones(1000, 48_000);
        let channels: [&[f32]; 2] = [&left, &right];
        let transparent = |ms: f64| HardLimiterParams { max_amp_db: 0.0, input_boost_db: 0.0, lookahead_ms: ms, release_ms: 40.0 };
        let mut pre = HardLimiter::new(48_000, 2, transparent(5.0)).unwrap();
        let mut post = HardLimiter::new(44_100, 2, transparent(5.0)).unwrap();
        let mut g = graph(48_000, 44_100, 1024);
        let (receipt, chunks) = bounce(&mut g, &mut pre, &mut post, 48_000, 0, &channels, 256);
        let r = receipt.unwrap();
        assert_eq!((r.requested_input_frames, r.requested_output_frames, r.actual_output_frames), (1000, 919, 919));
        assert_eq!((r.stages[0].latency_samples, r.stages[1].latency_samples, r.stages[2].latency_samples), (240, 0, 221));
        assert_eq!((r.pre.latency_dropped_frames, r.pre.blocks, r.pre.actual_frames), (240, 5, 1000));
        assert_eq!((r.post.latency_dropped_frames, r.post.blocks, r.post.actual_frames), (221, 5, 919));
        assert_eq!((r.stages[0].input_frames, r.stages[0].output_frames), (1000, 1000));
        assert_eq!((r.stages[2].input_frames, r.stages[2].output_frames), (919, 919));
        assert_eq!(chunks.iter().map(|(_, p)| p[0].len()).sum::<usize>(), 919);
        assert_eq!(chunks.first().map(|c| c.0), Some(0));

        // A transparent, latency-compensated limiter pair leaves the converted stream untouched:
        // the same clip through Unity -> SRC -> Unity is bit-identical.
        let mut g2 = graph(48_000, 44_100, 1024);
        let (plain, plain_chunks) = bounce(&mut g2, &mut Unity, &mut Unity, 48_000, 0, &channels, 256);
        let plain = plain.unwrap();
        let (with, without) = (join(&chunks, 0, 256), join(&plain_chunks, 0, 256));
        for ch in 0..2 {
            assert!(with[ch].iter().zip(&without[ch]).all(|(a, b)| a.to_bits() == b.to_bits()), "channel {ch}");
        }
        assert_eq!(r.digest(), plain.digest(), "latency compensation neither shifts nor drops frames");
        assert_ne!(r.graph_identity, plain.graph_identity, "different stages give a different graph identity");
        assert_ne!(r.stages[0].identity, plain.stages[0].identity);
        assert_eq!(r.stages[1].identity, plain.stages[1].identity, "same converter, same identity");
    });
}

#[test]
fn tone_lands_on_the_output_timeline() {
    bounded_case("tone_lands_on_the_output_timeline", || {
        // 4410 frames at 44.1 kHz (0.1 s) -> 4800 frames at 48 kHz: 4410 * 48000 / 44100 = 4800.
        // Start 44_100 (1 s) -> 48_000. Output frame m must equal the analytic value of the same
        // tone at output time m / 48000 (start offset removed); the first/last 300 frames hold the
        // sinc edge transients and are excluded.
        let (left, right) = stereo_tones(4410, 44_100);
        let channels: [&[f32]; 2] = [&left, &right];
        let mut g = graph(44_100, 48_000, 1024);
        let (receipt, chunks) = bounce(&mut g, &mut Unity, &mut Unity, 44_100, 44_100, &channels, 256);
        let r = receipt.unwrap();
        assert_eq!((r.requested_output_frames, r.actual_output_frames), (4800, 4800));
        assert_eq!((r.output_start_sample, r.output_end_sample), (48_000, 52_800));
        let joined = join(&chunks, 48_000, 256);
        let want = [tone(4800, 48_000, 997.0, 0.5), tone(4800, 48_000, 1500.0, 0.25)];
        for ch in 0..2 {
            let worst = (300..4500)
                .map(|m| f64::from((joined[ch][m] - want[ch][m]).abs()))
                .fold(0.0, f64::max);
            // A whole-frame misalignment would deviate by up to 0.065 (0.5 * 2 * pi * 997 / 48000);
            // the measured alignment residual of 0.01 frame gives about 0.0007.
            assert!(worst < 0.003, "channel {ch}: worst deviation {worst} from the analytic tone");
        }
        // The receipt meters describe the delivered stream (tone peaks reached inside the clip).
        assert!((r.peak()[0] - 0.5).abs() < 0.01 && (r.peak()[1] - 0.25).abs() < 0.01, "{:?}", r.peak());
        assert!((r.rms()[0] - 0.5 / 2.0_f32.sqrt()).abs() < 0.01, "{:?}", r.rms());
    });
}

#[test]
fn refusals_happen_before_any_sink_call() {
    bounded_case("refusals_happen_before_any_sink_call", || {
        let (left, right) = stereo_tones(1000, 48_000);
        let channels: [&[f32]; 2] = [&left, &right];
        let transparent = HardLimiterParams { max_amp_db: 0.0, input_boost_db: 0.0, lookahead_ms: 5.0, release_ms: 40.0 };
        let mut nan = left.clone();
        nan[999] = f32::NAN;
        let bad: [&[f32]; 2] = [&nan, &right];
        let short: [&[f32]; 2] = [&left, &right[..999]];
        let one: [&[f32]; 1] = [&left];
        let empty: [&[f32]; 2] = [&[], &[]];
        let huge = vec![0.0_f32; 1 << 20];
        let huge_channels: [&[f32]; 2] = [&huge, &huge];
        let over = vec![0.0_f32; (1 << 20) + 1];
        let over_channels: [&[f32]; 2] = [&over, &over];
        let mut limiter_44k = HardLimiter::new(44_100, 2, transparent).unwrap();
        let mut limiter_48k = HardLimiter::new(48_000, 2, transparent).unwrap();
        let mut mono_mix = ChannelMix::stereo_to_mono();

        // (name, expected, rate in, resampler (in, out), start, channels, block, pre, post)
        type Case<'a> = (&'a str, ScoreError, u32, (u32, u32), u64, &'a [&'a [f32]], usize, bool, u8);
        // pre: true = 44.1 kHz limiter as pre (wrong rate); post: 0 Unity, 1 48 kHz limiter, 2 mono mix
        let cases: [Case<'_>; 14] = [
            ("non-finite", ScoreError::NotFinite, 48_000, (48_000, 44_100), 0, &bad, 256, false, 0),
            ("length", ScoreError::LengthMismatch, 48_000, (48_000, 44_100), 0, &short, 256, false, 0),
            ("channel count", ScoreError::ChannelMismatch, 48_000, (48_000, 44_100), 0, &one, 256, false, 0),
            ("zero frames", ScoreError::OutOfRange, 48_000, (48_000, 44_100), 0, &empty, 256, false, 0),
            ("block zero", ScoreError::OutOfRange, 48_000, (48_000, 44_100), 0, &channels, 0, false, 0),
            ("block over capacity", ScoreError::CapacityExceeded, 48_000, (48_000, 44_100), 0, &channels, 257, false, 0),
            ("rate differs from the converter", ScoreError::InvalidRate, 44_100, (48_000, 44_100), 0, &channels, 256, false, 0),
            ("rate not a tick divisor", ScoreError::InvalidRate, 47_999, (48_000, 44_100), 0, &channels, 256, false, 0),
            ("pre bound to another rate", ScoreError::InvalidRate, 48_000, (48_000, 44_100), 0, &channels, 256, true, 0),
            ("post bound to the input rate", ScoreError::InvalidRate, 48_000, (48_000, 44_100), 0, &channels, 256, false, 1),
            ("post layout", ScoreError::ChannelMismatch, 48_000, (48_000, 44_100), 0, &channels, 256, false, 2),
            ("start not whole at the output rate", ScoreError::OutOfRange, 44_100, (44_100, 48_000), 1, &channels, 256, false, 0),
            ("converted clip over the block bound", ScoreError::CapacityExceeded, 44_100, (44_100, 48_000), 0, &huge_channels, 256, false, 0),
            ("input clip over the block bound", ScoreError::CapacityExceeded, 48_000, (48_000, 48_000), 0, &over_channels, 256, false, 0),
        ];
        for (name, expected, rate_in, (src_in, src_out), start, input, block, wrong_pre, post_kind) in cases {
            let mut g = graph(src_in, src_out, 1024);
            let post: &mut dyn Processor = match post_kind {
                0 => &mut Unity,
                1 => &mut limiter_48k,
                _ => &mut mono_mix,
            };
            let pre: &mut dyn Processor = if wrong_pre { &mut limiter_44k } else { &mut Unity };
            let (result, chunks) = bounce(&mut g, pre, post, rate_in, start, input, block);
            assert_eq!(result.unwrap_err(), expected, "{name}");
            assert!(chunks.is_empty(), "{name}: the sink must not be called");
        }
        // Position overflow (identity rate so the output start maps exactly).
        let mut g = graph(48_000, 48_000, 1024);
        let (result, chunks) = bounce(&mut g, &mut Unity, &mut Unity, 48_000, u64::MAX - 5, &channels, 256);
        assert_eq!(result.unwrap_err(), ScoreError::PositionOverflow);
        assert!(chunks.is_empty());

        // Request-level admission, shared with Engine::bounce.
        let (engine, token) = (engine(), CancellationToken::default());
        let ignore = |_: BounceChunk<'_>| Ok(());
        let mut sink = ignore;
        let mut g = graph(48_000, 44_100, 1024);
        let mut no_actor = ctx(7);
        no_actor.actor = None;
        let layout = ChannelLayout::stereo();
        let request = BounceRequest { sample_rate_hz: 48_000, layout: &layout, start_sample: 0, channels: &channels, block_frames: 256 };
        let attempt = |ctx: &RequestContext, g: &mut Graph, sink: &mut dyn FnMut(BounceChunk<'_>) -> Result<(), ScoreError>| {
            let stages = ResampledBounce { pre: &mut Unity, pre_out: &mut g.pre_out, resampler: &mut g.resampler, post: &mut Unity, out: &mut g.out };
            engine.bounce_resampled(ctx, &request, stages, sink, &token)
        };
        assert_eq!(attempt(&no_actor, &mut g, &mut sink).unwrap_err(), ScoreError::MissingContext);
        assert_eq!(attempt(&ctx(6), &mut g, &mut sink).unwrap_err(), ScoreError::StaleRevision);
        {
            let _held = engine.reserve().unwrap();
            assert_eq!(attempt(&ctx(7), &mut g, &mut sink).unwrap_err(), ScoreError::Busy);
        }
        token.cancel();
        assert_eq!(attempt(&ctx(7), &mut g, &mut sink).unwrap_err(), ScoreError::Canceled);
        assert_eq!(engine.reserve().map(|_| ()), Ok(()), "the slot is released on every exit");

        // Mid-bounce: cancel and revision change from the sink stop the final stage between
        // blocks, a failing sink error is returned as is; no receipt, slot released each time.
        let engine = self::engine();
        let token = CancellationToken::default();
        let mut g = graph(48_000, 44_100, 1024);
        let delivered = RefCell::new(0);
        let mut cancel_after_first = |_: BounceChunk<'_>| {
            *delivered.borrow_mut() += 1;
            token.cancel();
            Ok(())
        };
        let result = run(&engine, &token, &mut g, &mut Unity, &mut Unity, 48_000, 0, &channels, 256, &mut cancel_after_first);
        assert_eq!((result.unwrap_err(), *delivered.borrow()), (ScoreError::Canceled, 1));
        let token = CancellationToken::default();
        let mut bump = |_: BounceChunk<'_>| {
            engine.set_revision(8);
            Ok(())
        };
        let result = run(&engine, &token, &mut g, &mut Unity, &mut Unity, 48_000, 0, &channels, 256, &mut bump);
        assert_eq!(result.unwrap_err(), ScoreError::StaleRevision);
        engine.set_revision(7);
        let mut failing = |_: BounceChunk<'_>| Err(ScoreError::Busy);
        let result = run(&engine, &token, &mut g, &mut Unity, &mut Unity, 48_000, 0, &channels, 256, &mut failing);
        assert_eq!(result.unwrap_err(), ScoreError::Busy);
        assert_eq!(engine.reserve().map(|_| ()), Ok(()), "the slot is released on every exit");
        // The graph is reusable after every failure (converter and processors were reset).
        let mut ok = |_: BounceChunk<'_>| Ok(());
        let receipt = run(&engine, &token, &mut g, &mut Unity, &mut Unity, 48_000, 0, &channels, 256, &mut ok).unwrap();
        assert_eq!((receipt.requested_output_frames, receipt.actual_output_frames), (919, 919));
    });
}

#[test]
fn digest_and_graph_identity_follow_the_documented_schemes() {
    bounded_case("digest_and_graph_identity_follow_the_documented_schemes", || {
        let (left, right) = stereo_tones(1000, 44_100);
        let channels: [&[f32]; 2] = [&left, &right];
        let mut g = graph(44_100, 48_000, 1024);
        let (receipt, chunks) = bounce(&mut g, &mut Unity, &mut Unity, 44_100, 0, &channels, 256);
        let r = receipt.unwrap();
        // Digest oracle from sha256 alone: SHA-256(domain || channels u16 LE || per channel:
        // label u16 LE || frames u64 LE || SHA-256(f32 LE samples delivered to the sink)).
        let joined = join(&chunks, 0, 256);
        let mut message = b"hsk-studio-score/bounce-digest@1".to_vec();
        message.extend_from_slice(&2_u16.to_le_bytes());
        for (label, samples) in [100_u16, 101].into_iter().zip(&joined) {
            let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            message.extend_from_slice(&label.to_le_bytes());
            message.extend_from_slice(&1089_u64.to_le_bytes());
            message.extend_from_slice(&sha256(&bytes));
        }
        assert_eq!(r.digest(), sha256(&message), "digest covers exactly the delivered 1089 frames per channel");
        assert_eq!(r.digest_hex(), to_hex(&r.digest()));

        // Graph identity oracle: domain, crate version, NUL, canonical graph string (Unity is
        // 'unity'; the converter string carries rates, channels, chunk size, pad 19 and drop 20).
        let mut expected = b"hsk-studio-score/src-bounce-identity@1".to_vec();
        expected.extend_from_slice(b"0.1.0");
        expected.push(0);
        expected.extend_from_slice(b"src_graph:[unity > clip_resampler:sinc_len=256;window=blackman_harris2;in=44100;out=48000;channels=2;chunk=1024;pad=19;drop=20 > unity]");
        assert_eq!(r.graph_identity, sha256(&expected));
        assert_eq!(r.graph_identity_hex(), to_hex(&r.graph_identity));

        // The digest does not depend on the block size; the identity does depend on the converter.
        let mut g77 = graph(44_100, 48_000, 1024);
        let (other, _) = bounce(&mut g77, &mut Unity, &mut Unity, 44_100, 0, &channels, 77);
        let other = other.unwrap();
        assert_eq!((other.digest(), other.graph_identity), (r.digest(), r.graph_identity));
        assert_ne!(other.post.blocks, r.post.blocks);
        let mut g512 = graph(44_100, 48_000, 512);
        let (chunked, _) = bounce(&mut g512, &mut Unity, &mut Unity, 44_100, 0, &channels, 256);
        let chunked = chunked.unwrap();
        assert_ne!(chunked.graph_identity, r.graph_identity, "converter chunk size is part of the identity");
        assert_eq!(chunked.requested_output_frames, 1089);
    });
}
