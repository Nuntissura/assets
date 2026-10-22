//! Streaming sample-rate conversion: bit-identity with the whole-clip oracle, bounces that are not
//! capped at `MAX_BLOCK_FRAMES`, exact counts and a measured, length-independent working set.
//! Sample counts are hand computed (`ceil(frames * out / in)`); the oracle for sample values is the
//! whole-clip `Engine::resample` / `Engine::bounce_resampled_whole_clip` path, which this change
//! does not touch.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_score::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::time::Instant;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

/// Counts allocations and live bytes per thread so a test can measure the bytes a call keeps
/// at its high-water mark and prove that a long run allocates exactly like a short one.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE.with(|live| {
                live.set(live.get() + layout.size() as isize);
                PEAK.with(|peak| peak.set(peak.get().max(live.get())));
            });
            ALLOCS.with(|n| n.set(n.get() + 1));
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.with(|live| live.set(live.get() - layout.size() as isize));
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// `(result, peak bytes held above the starting level, allocation calls)` of `body`.
fn measure<R>(body: impl FnOnce() -> R) -> (R, isize, usize) {
    let base = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(base));
    let before = ALLOCS.with(Cell::get);
    let result = body();
    let peak = PEAK.with(Cell::get) - base;
    (result, peak, ALLOCS.with(Cell::get) - before)
}

fn bounded_case(name: &'static str, seconds: u64, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(seconds)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared {seconds}s deadline: {e}"),
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

fn tone(frames: usize, rate: u32, hz: f64, amplitude: f64) -> Vec<f32> {
    (0..frames).map(|n| (amplitude * (std::f64::consts::TAU * hz * n as f64 / f64::from(rate)).sin()) as f32).collect()
}

fn stereo(frames: usize, rate: u32) -> [Vec<f32>; 2] {
    [tone(frames, rate, 997.0, 0.5), tone(frames, rate, 1500.0, 0.25)]
}

/// Whole-clip conversion through `Engine::resample` (the oracle).
fn whole_clip(resampler: &mut ClipResampler, rate_in: u32, input: &[&[f32]]) -> (ResampleReceipt, Vec<Vec<f32>>) {
    let frames = input[0].len();
    let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), resampler.output_capacity_for(frames)).unwrap();
    let spec = BlockSpec { sample_rate_hz: rate_in, layout: ChannelLayout::stereo(), frames, start_sample: 0 };
    let receipt = engine()
        .resample(&ctx(7), &InputBlock::new(&spec, input), resampler, &mut out, &CancellationToken::default())
        .unwrap();
    (receipt, (0..2).map(|ch| out.published_channel(ch).unwrap().to_vec()).collect())
}

/// Streaming conversion with the input pushed in `pieces`-sized, cycling slices.
fn streamed(resampler: &mut ClipResampler, input: &[&[f32]], pieces: &[usize]) -> (StreamReceipt, Vec<Vec<f32>>) {
    let frames = input[0].len();
    resampler.begin_stream(frames).unwrap();
    let mut got = vec![Vec::new(), Vec::new()];
    let mut emit = |planes: &[&[f32]]| {
        for (dst, src) in got.iter_mut().zip(planes) {
            dst.extend_from_slice(src);
        }
        Ok(())
    };
    let (mut at, mut k) = (0, 0);
    while at < frames {
        let n = pieces[k % pieces.len()].min(frames - at);
        let slices: [&[f32]; 2] = [&input[0][at..at + n], &input[1][at..at + n]];
        resampler.push_stream(&slices, &mut emit).unwrap();
        at += n;
        k += 1;
    }
    let receipt = resampler.finish_stream(&mut emit).unwrap();
    (receipt, got)
}

fn assert_bit_identical(name: &str, whole: &(ResampleReceipt, Vec<Vec<f32>>), stream: &(StreamReceipt, Vec<Vec<f32>>)) {
    let ((w, wd), (s, sd)) = (whole, stream);
    assert_eq!((s.requested_output_frames, s.delivered_frames), (w.requested_output_frames, w.actual_output_frames()), "{name}");
    assert_eq!((s.pad_input_frames, s.drop_output_frames, s.trimmed_tail_frames, s.delay_trimmed_frames), (w.pad_input_frames, w.drop_output_frames, w.trimmed_tail_frames, w.delay_trimmed_frames), "{name}");
    for ch in 0..2 {
        assert_eq!(sd[ch].len(), wd[ch].len(), "{name} channel {ch}");
        assert!(sd[ch].iter().zip(&wd[ch]).all(|(a, b)| a.to_bits() == b.to_bits()), "{name} channel {ch}: streamed samples differ from the whole-clip oracle");
    }
    assert_eq!((s.peak(), s.rms()), (w.block.peak(), w.block.rms()), "{name}: meters");
}

#[test]
fn streaming_resampler_is_bit_identical_to_the_whole_clip_oracle() {
    bounded_case("streaming_resampler_is_bit_identical_to_the_whole_clip_oracle", 60, || {
        let pairs = [(44_100_u32, 48_000_u32), (48_000, 44_100), (32_000, 48_000), (48_000, 96_000), (48_000, 48_000)];
        // Short lengths, the chunk boundaries (1024 input frames per chunk incl. the alignment
        // pad) and awkward piece sizes: 1, 7, 255 and 1000 frames cycling.
        let lengths = (1..=40).chain([63, 64, 65, 1005, 1023, 1024, 1025, 2047, 2048, 2049, 4799, 4800, 4801]);
        for (rate_in, rate_out) in pairs {
            let mut oracle = ClipResampler::new(rate_in, rate_out, 2, 1024).unwrap();
            let mut resampler = ClipResampler::new(rate_in, rate_out, 2, 1024).unwrap();
            for frames in lengths.clone() {
                let [left, right] = stereo(frames, rate_in);
                let input: [&[f32]; 2] = [&left, &right];
                let whole = whole_clip(&mut oracle, rate_in, &input);
                let stream = streamed(&mut resampler, &input, &[1, 7, 255, 1000]);
                // Hand-checked counts: ceil(frames * out / in), e.g. 4800 @ 44.1k -> 48k = 5225.
                assert_eq!(stream.0.requested_output_frames, (frames as u128 * u128::from(rate_out)).div_ceil(u128::from(rate_in)) as usize);
                assert_bit_identical(&format!("{rate_in}->{rate_out} x {frames}"), &whole, &stream);
            }
        }
        // Four seconds (176_400 frames, 172 chunks): 176400 * 48000 / 44100 = 192000 exactly.
        let [left, right] = stereo(176_400, 44_100);
        let input: [&[f32]; 2] = [&left, &right];
        let whole = whole_clip(&mut ClipResampler::new(44_100, 48_000, 2, 1024).unwrap(), 44_100, &input);
        let mut resampler = ClipResampler::new(44_100, 48_000, 2, 1024).unwrap();
        let stream = streamed(&mut resampler, &input, &[4096, 333]);
        assert_eq!(stream.0.requested_output_frames, 192_000);
        assert_bit_identical("4 s 44.1k->48k", &whole, &stream);
        // Streaming buffers: input chunk (2 * 1024 f32) plus the output chunk, independent of length.
        assert_eq!(stream.0.buffer_bytes, resampler.stream_buffer_bytes());
        assert!(stream.0.buffer_bytes >= 2 * 1024 * 4 && stream.0.buffer_bytes < 64 * 1024, "{}", stream.0.buffer_bytes);
    });
}

fn graph_stereo(rate_in: u32, rate_out: u32) -> (ClipResampler, OutputBlock, OutputBlock) {
    (
        ClipResampler::new(rate_in, rate_out, 2, 1024).unwrap(),
        OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap(),
        OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap(),
    )
}

/// One stateful graph run: limiter (latency 341 at 48 kHz) -> converter -> peaking EQ.
fn run_graph(whole: bool, block: usize, input: &[&[f32]], token: &CancellationToken, sink: &mut dyn FnMut(BounceChunk<'_>) -> Result<(), ScoreError>) -> Result<ResampledBounceReceipt, ScoreError> {
    let (mut resampler, mut pre_out, mut out) = graph_stereo(48_000, 44_100);
    let mut pre = HardLimiter::new(48_000, 2, HardLimiterParams { max_amp_db: -6.0, input_boost_db: 6.0, lookahead_ms: 7.1, release_ms: 100.0 }).unwrap();
    let peaking = BiquadSpec { kind: BiquadKind::Peaking, frequency_hz: 1000.0, gain_db: 6.0, q: 1.0 };
    let mut post = Equalizer::new(44_100, 2, &[peaking], 0.0).unwrap();
    let layout = ChannelLayout::stereo();
    let request = BounceRequest { sample_rate_hz: 48_000, layout: &layout, start_sample: 0, channels: input, block_frames: block };
    let stages = ResampledBounce { pre: &mut pre, pre_out: &mut pre_out, resampler: &mut resampler, post: &mut post, out: &mut out };
    let engine = engine();
    let result = if whole {
        engine.bounce_resampled_whole_clip(&ctx(7), &request, stages, sink, token)
    } else {
        engine.bounce_resampled(&ctx(7), &request, stages, sink, token)
    };
    assert_eq!(engine.reserve().map(|_| ()), Ok(()), "the slot is released on every exit");
    result
}

fn collect(whole: bool, block: usize, input: &[&[f32]]) -> (ResampledBounceReceipt, [Vec<f32>; 2], usize) {
    let planes = RefCell::new([Vec::new(), Vec::new()]);
    let largest = Cell::new(0);
    let mut sink = |chunk: BounceChunk<'_>| {
        largest.set(largest.get().max(chunk.channels[0].len()));
        let mut planes = planes.borrow_mut();
        planes[0].extend_from_slice(chunk.channels[0]);
        planes[1].extend_from_slice(chunk.channels[1]);
        Ok(())
    };
    let receipt = run_graph(whole, block, input, &CancellationToken::default(), &mut sink).unwrap();
    (receipt, planes.into_inner(), largest.get())
}

#[test]
fn streaming_bounce_equals_the_whole_clip_bounce() {
    bounded_case("streaming_bounce_equals_the_whole_clip_bounce", 60, || {
        // 6 s at 48 kHz -> 44.1 kHz: 288000 * 44100 / 48000 = 264600 frames exactly. The tone
        // (0.9 peak) is limited to the -6 dB ceiling by the pre stage, then shaped by the post EQ.
        let [left, right] = stereo(288_000, 48_000);
        let loud: Vec<Vec<f32>> = [left, right].iter().map(|c| c.iter().map(|s| s * 1.8).collect()).collect();
        let input: [&[f32]; 2] = [&loud[0], &loud[1]];
        for block in [256_usize, 77] {
            let (whole, whole_samples, _) = collect(true, block, &input);
            let (stream, stream_samples, largest) = collect(false, block, &input);
            assert_eq!((stream.requested_input_frames, stream.requested_output_frames, stream.actual_output_frames), (288_000, 264_600, 264_600), "block {block}");
            assert!(largest <= block, "chunks never exceed the block size");
            for ch in 0..2 {
                assert_eq!(stream_samples[ch].len(), 264_600);
                assert!(stream_samples[ch].iter().zip(&whole_samples[ch]).all(|(a, b)| a.to_bits() == b.to_bits()), "block {block} channel {ch}");
            }
            // The limiter really ran (ceiling 10^(-6/20) = 0.501) and latency was compensated.
            assert!(stream.pre.peak()[0] > 0.4 && stream.pre.peak()[0] < 0.51, "{:?}", stream.pre.peak());
            assert_eq!((stream.stages[0].latency_samples, stream.pre.latency_dropped_frames), (341, 341));
            // Whole receipt equality except the working set, which is the point of streaming.
            assert!(stream.working_set_bytes * 10 < whole.working_set_bytes, "{} vs {}", stream.working_set_bytes, whole.working_set_bytes);
            let mut same = stream;
            same.working_set_bytes = whole.working_set_bytes;
            assert_eq!(same, whole, "block {block}");
        }
    });
}

/// Bounces `seconds` of a mono tone from `rate_in` to `rate_out` and returns
/// `(receipt, worst tone deviation, peak extra bytes, allocation calls)`.
fn long_bounce(rate_in: u32, rate_out: u32, seconds: usize, hz: f64) -> (ResampledBounceReceipt, f64, isize, usize) {
    let frames = rate_in as usize * seconds;
    let want = rate_out as usize * seconds;
    let input = tone(frames, rate_in, hz, 0.5);
    let layout = ChannelLayout::mono(7);
    let channels: [&[f32]; 1] = [&input];
    let request = BounceRequest { sample_rate_hz: rate_in, layout: &layout, start_sample: 0, channels: &channels, block_frames: 256 };
    let mut resampler = ClipResampler::new(rate_in, rate_out, 1, 1024).unwrap();
    let mut pre_out = OutputBlock::with_capacity(ChannelLayout::mono(7), 256).unwrap();
    let mut out = OutputBlock::with_capacity(ChannelLayout::mono(7), 256).unwrap();
    // The tone is checked on the fly (no chunk is kept): frame m of the output must be
    // 0.5 sin(2 pi hz m / out_rate); the first and last 300 frames hold the edge transients.
    let (next, worst, delivered) = (Cell::new(0_u64), Cell::new(0.0_f64), Cell::new(0_usize));
    let mut sink = |chunk: BounceChunk<'_>| {
        assert_eq!(chunk.start_sample, next.get(), "chunks tile the output timeline");
        let samples = chunk.channels[0];
        next.set(next.get() + samples.len() as u64);
        let mut deviation = worst.get();
        for (i, sample) in samples.iter().enumerate() {
            let m = delivered.get() + i;
            if (300..want - 300).contains(&m) {
                let expected = 0.5 * (std::f64::consts::TAU * hz * m as f64 / f64::from(rate_out)).sin();
                deviation = deviation.max((f64::from(*sample) - expected).abs());
            }
        }
        worst.set(deviation);
        delivered.set(delivered.get() + samples.len());
        Ok(())
    };
    let (engine, ctx, token) = (engine(), ctx(7), CancellationToken::default());
    let started = Instant::now();
    let (result, peak, allocs) = measure(|| {
        let stages = ResampledBounce { pre: &mut Unity, pre_out: &mut pre_out, resampler: &mut resampler, post: &mut Unity, out: &mut out };
        engine.bounce_resampled(&ctx, &request, stages, &mut sink, &token)
    });
    let receipt = result.unwrap();
    println!("{seconds} s {rate_in}->{rate_out} mono: {frames} -> {} frames in {:.1} s, working set {} B, peak extra {peak} B, {allocs} allocation calls", receipt.actual_output_frames, started.elapsed().as_secs_f64(), receipt.working_set_bytes);
    assert_eq!(delivered.get(), want);
    (receipt, worst.get(), peak, allocs)
}

fn assert_long_clip(rate_in: u32, rate_out: u32, seconds: usize, hz: f64) {
    // Exact by construction: seconds * rate frames in, seconds * out_rate frames out.
    let (receipt, worst, peak, allocs) = long_bounce(rate_in, rate_out, seconds, hz);
    let (frames, want) = ((rate_in as usize * seconds) as u64, (rate_out as usize * seconds) as u64);
    assert!(frames > MAX_BLOCK_FRAMES as u64, "the clip is longer than the whole-clip bound");
    assert_eq!((receipt.requested_input_frames, receipt.requested_output_frames, receipt.actual_output_frames), (frames, want, want));
    assert_eq!((receipt.output_start_sample, receipt.output_end_sample), (0, want));
    assert_eq!(receipt.ceil_excess_ticks(), 0, "a whole number of seconds converts without a ceil excess");
    assert_eq!((receipt.pre.actual_frames, receipt.post.actual_frames), (frames, want));
    assert!(worst < 0.003, "tone deviation {worst} after {seconds} s: alignment holds over the whole clip");
    // The working set is a few blocks and chunk buffers, whatever the clip length: a 2 s clip of
    // the same graph holds exactly the same bytes and makes exactly the same allocation calls.
    let (short, _, short_peak, short_allocs) = long_bounce(rate_in, rate_out, 2, hz);
    assert_eq!(receipt.working_set_bytes, short.working_set_bytes);
    assert!(receipt.working_set_bytes < 64 * 1024, "{}", receipt.working_set_bytes);
    assert_eq!(allocs, short_allocs, "no per-block allocation");
    assert!(peak <= short_peak + 4096 && peak < 256 * 1024, "peak extra {peak} B vs {short_peak} B for 2 s");
}

#[test]
fn five_minutes_bounce_exactly_in_bounded_memory() {
    // 5 minutes at 4.41 kHz -> 4.8 kHz (the 147:160 ratio of 44.1k -> 48k, same alignment, a
    // 200 Hz tone so the alignment residual stays inside the tone bound at the lower rate):
    // 300 * 4410 = 1_323_000 frames -> 300 * 4800 = 1_440_000 frames, both over the whole-clip
    // bound of 1_048_576. The same check at the real rates is the ignored test below.
    bounded_case("five_minutes_bounce_exactly_in_bounded_memory", 100, || assert_long_clip(4_410, 4_800, 300, 200.0));
}

#[test]
#[ignore = "5 minutes of 44.1 kHz audio (13.2 M frames); run with --ignored"]
fn five_minutes_at_44k_to_48k() {
    // 300 * 44100 = 13_230_000 frames -> 300 * 48000 = 14_400_000 frames.
    bounded_case("five_minutes_at_44k_to_48k", 900, || assert_long_clip(44_100, 48_000, 300, 997.0));
}

#[test]
fn stream_misuse_and_failures_end_the_stream_and_leave_it_reusable() {
    bounded_case("stream_misuse_and_failures_end_the_stream_and_leave_it_reusable", 60, || {
        let data = [0.25_f32; 100];
        let planes: [&[f32]; 2] = [&data, &data];
        let mut ok = |_: &[&[f32]]| Ok(());
        let mut r = ClipResampler::new(48_000, 44_100, 2, 1024).unwrap();
        assert_eq!(r.push_stream(&planes, &mut ok), Err(ScoreError::OutOfRange), "not begun");
        assert_eq!(r.finish_stream(&mut ok), Err(ScoreError::OutOfRange), "not begun");
        assert_eq!(r.begin_stream(0), Err(ScoreError::OutOfRange));
        assert_eq!(r.stream_buffer_bytes(), 0, "nothing is allocated until a stream begins");

        r.begin_stream(150).unwrap();
        let uneven: [&[f32]; 2] = [&data, &data[..51]];
        assert_eq!(r.push_stream(&uneven, &mut ok), Err(ScoreError::LengthMismatch));
        assert_eq!(r.push_stream(&planes, &mut ok), Err(ScoreError::OutOfRange), "any error ends the stream");

        r.begin_stream(150).unwrap();
        r.push_stream(&planes, &mut ok).unwrap();
        assert_eq!(r.push_stream(&planes, &mut ok), Err(ScoreError::LengthMismatch), "more than declared");

        r.begin_stream(150).unwrap();
        r.push_stream(&planes, &mut ok).unwrap();
        assert_eq!(r.finish_stream(&mut ok), Err(ScoreError::LengthMismatch), "fewer than declared");

        r.begin_stream(150).unwrap();
        let one: [&[f32]; 1] = [&data];
        assert_eq!(r.push_stream(&one, &mut ok), Err(ScoreError::ChannelMismatch));

        r.begin_stream(150).unwrap();
        let mut nan = data;
        nan[99] = f32::NAN;
        let bad: [&[f32]; 2] = [&data, &nan];
        assert_eq!(r.push_stream(&bad, &mut ok), Err(ScoreError::NotFinite));

        // A receiver error is returned as is, after the converter produced output.
        let long = vec![0.25_f32; 3000];
        let long_planes: [&[f32]; 2] = [&long, &long];
        r.begin_stream(3000).unwrap();
        let mut failing = |_: &[&[f32]]| Err(ScoreError::Busy);
        assert_eq!(r.push_stream(&long_planes, &mut failing), Err(ScoreError::Busy));

        // After every failure the converter is clean: a full stream equals a fresh converter's.
        let [left, right] = stereo(5000, 48_000);
        let input: [&[f32]; 2] = [&left, &right];
        let again = streamed(&mut r, &input, &[999]);
        let fresh = streamed(&mut ClipResampler::new(48_000, 44_100, 2, 1024).unwrap(), &input, &[999]);
        assert_eq!(again.0, fresh.0);
        assert_eq!(again.1, fresh.1);
        assert_eq!(again.0.requested_output_frames, 4594, "5000 * 44100 / 48000 = 4593.75");

        // A streaming bounce stopped mid-clip by cancellation leaves the graph reusable and the
        // slot free; the next run is the same bounce an untouched graph gives.
        let [left, right] = stereo(96_000, 48_000);
        let input: [&[f32]; 2] = [&left, &right];
        let token = CancellationToken::default();
        let seen = Cell::new(0);
        let mut cancel_after_three = |_: BounceChunk<'_>| {
            seen.set(seen.get() + 1);
            if seen.get() == 3 {
                token.cancel();
            }
            Ok(())
        };
        assert_eq!(run_graph(false, 256, &input, &token, &mut cancel_after_three).unwrap_err(), ScoreError::Canceled);
        assert_eq!(seen.get(), 3, "no chunk after the cancellation was observed");
        let (clean, clean_samples, _) = collect(false, 256, &input);
        let (oracle, oracle_samples, _) = collect(true, 256, &input);
        assert_eq!((clean.requested_output_frames, clean.actual_output_frames), (88_200, 88_200), "96000 * 44100 / 48000");
        assert_eq!(clean_samples, oracle_samples);
        assert_eq!(clean.digest(), oracle.digest());
    });
}
