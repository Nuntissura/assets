//! STU-CON-044 (spec v02.215 l.22588): "Use the admitted Prism transform in bounded chunks of at most 64 pixels,
//! relative-colorimetric/depth32/BPC false, with actual profile/engine/options receipts and accounted lifetime."
//! SMB-PRISM-ENG-6 R6 remediation: every wide conversion (request or job band) runs in chunks of at most
//! `ADMITTED_CHUNK_PIXELS` = 64 pixels, each with its own cancellation check, and reports them in
//! `WideReceipt::chunks` / `WideBandReport::chunks`. This file proves the bound, the accounting, the absence of
//! allocation inside a band, the chunk-granular cancellation (here a pre-canceled token; the mid-band edges are deterministic unit tests in src/wide_job/tests.rs) and bit identity with the plain request path.
//! The ignored bench prices the literal reading (a caller driving one admitted transform call per 64 pixels).
mod wide_support;
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_prism::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use wide_support::*;

const SRGB_LINEAR: &[u8] = include_bytes!("profiles/sRGB-linear-v4.icc");
const P3_LINEAR: &[u8] = include_bytes!("profiles/DisplayP3-linear-v4.icc");
const P3: &[u8] = include_bytes!("profiles/DisplayP3-v4.icc");
const BOUND: usize = ADMITTED_CHUNK_PIXELS;

// Allocation counter of the calling thread (tests run on their own threads, so other tests do not count).
thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}
fn allocs() -> u64 {
    ALLOCS.with(Cell::get)
}
fn count_one() {
    let _ = ALLOCS.try_with(|c| c.set(c.get().wrapping_add(1)));
}
struct Counting;
// SAFETY: every method forwards to the System allocator with the arguments it received; the only addition is a
// thread-local counter increment that never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_one();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_one();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_one();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

/// Deterministic device values in 0..1 (64-bit LCG).
fn device_pixels(channels: usize, count: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    (0..count * channels)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 40) as f32) / 16_777_216.0
        })
        .collect()
}
fn job_request<'a>(ctx: &'a Ctx, source: &'a [u8], destination: &'a [u8], bands: WideBands<'a>, intent: Intent, bpc: bool) -> WideJobRequest<'a> {
    WideJobRequest {
        source: WideSource::Profile(ctx.input(&ctx.source, source)),
        destination: ctx.input(&ctx.destination, destination),
        bands,
        intent,
        black_point_compensation: bpc,
        revision: 1,
        expected_revision: 1,
        correlation_id: 7,
        resource_id: &ctx.resource,
        actor: &ctx.actor,
    }
}
fn expected(pixels: usize, out_channels: usize) -> WideChunkAccount {
    WideChunkAccount {
        bound_pixels: BOUND,
        chunks: pixels.div_ceil(BOUND) as u64,
        pixels: pixels as u64,
        max_chunk_pixels: pixels.min(BOUND),
        scratch_bytes: BOUND * out_channels * 4,
    }
}
/// The account of a JOB band (and of a job receipt): the digest staging buffer of a band is a 1024-byte stack array
/// (64 pixels x 4 channels x 4 bytes) whatever the destination channel count, unlike a request's heap buffer
/// (`expected`: 64 x destination channels x 4).
const BAND_STAGE_BYTES: usize = BOUND * 4 * 4;
fn expected_band(pixels: usize) -> WideChunkAccount {
    WideChunkAccount { scratch_bytes: BAND_STAGE_BYTES, ..expected(pixels, 4) }
}
fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

/// Convert every band with `threads` workers pulling band indices from a shared counter; returns the output and
/// the band reports in band order.
fn convert_all(job: &WideJob<'_>, in_ch: usize, out_ch: usize, input: &[f32], threads: usize, t: &CancellationToken) -> (Vec<f32>, Vec<WideBandReport>) {
    let sizes: Vec<usize> = (0..job.bands()).map(|b| job.band_pixels(b).unwrap()).collect();
    let mut output = vec![f32::NAN; sizes.iter().sum::<usize>() * out_ch];
    let (mut rest_in, mut rest_out) = (input, output.as_mut_slice());
    let mut tasks = Vec::new();
    for (band, &n) in sizes.iter().enumerate() {
        let (i, ri) = rest_in.split_at(n * in_ch);
        let (o, ro) = std::mem::take(&mut rest_out).split_at_mut(n * out_ch);
        tasks.push((band, i, o));
        (rest_in, rest_out) = (ri, ro);
    }
    tasks.reverse();
    let queue = Mutex::new(tasks);
    let reports: Mutex<Vec<Option<WideBandReport>>> = Mutex::new(vec![None; sizes.len()]);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let task = queue.lock().unwrap().pop();
                    let Some((band, i, o)) = task else { break };
                    let report = job.convert_band(band, i, o, t).unwrap();
                    reports.lock().unwrap()[band] = Some(report);
                }
            });
        }
    });
    drop(queue);
    let reports = reports.into_inner().unwrap().into_iter().map(|r| r.expect("every band ran")).collect();
    (output, reports)
}

#[test]
fn chunk_accounting_is_bounded_deterministic_and_allocation_free() {
    // Band sizes around the chunk edge: a single pixel, one below / at / above 64, multiples and a long tail.
    const SIZES: [usize; 9] = [1, 63, 64, 65, 127, 128, 129, 1000, 2049];
    let total: usize = SIZES.iter().sum();
    let ctx = Ctx::new();
    let t = CancellationToken::default();
    let cmyk = cmyk_k_profile(9, 3, 20.0);
    // (name, source, destination, input channels, output channels, intent, BPC)
    type Case<'a> = (&'static str, &'a [u8], &'a [u8], usize, usize, Intent, bool);
    let cases: [Case<'_>; 2] = [
        ("srgb_to_p3", SRGB, P3, 3, 3, Intent::RelativeColorimetric, false),
        ("cmyk_table_to_srgb_bpc", &cmyk, SRGB, 4, 3, Intent::RelativeColorimetric, true),
    ];
    for (name, src, dst, in_ch, out_ch, intent, bpc) in cases {
        let input = device_pixels(in_ch, total, 0x2545_f491_4f6c_dd1d);
        // The plain request path: one whole-frame request, and a request of exactly 63 pixels.
        let mut plain = engine(&ctx);
        let serial = run(&mut plain, &ctx, src, dst, &input, intent, bpc).unwrap();
        assert_eq!(serial.receipt.chunks, expected(total, out_ch), "{name}: whole-frame request");
        let small = run(&mut plain, &ctx, src, dst, &input[..63 * in_ch], intent, bpc).unwrap();
        assert_eq!(small.receipt.chunks, expected(63, out_ch), "{name}: 63-pixel request is one chunk");

        let mut eng = engine(&ctx);
        let mut merged = Vec::new();
        for threads in [1usize, 8] {
            let job = eng.prepare_job(&job_request(&ctx, src, dst, WideBands::Sizes(&SIZES), intent, bpc), &t).unwrap();
            let (out, reports) = convert_all(&job, in_ch, out_ch, &input, threads, &t);
            for (band, report) in reports.iter().enumerate() {
                assert_eq!(report.chunks, expected_band(SIZES[band]), "{name}: band {band} ({threads} threads)");
                assert!(report.chunks.max_chunk_pixels <= BOUND);
            }
            let result = job.finish(&t, &mut Collect(Vec::new())).unwrap();
            let sum_chunks: u64 = SIZES.iter().map(|n| n.div_ceil(BOUND) as u64).sum();
            let account = result.receipt.receipt.chunks;
            assert_eq!(
                account,
                WideChunkAccount { bound_pixels: BOUND, chunks: sum_chunks, pixels: total as u64, max_chunk_pixels: BOUND, scratch_bytes: BAND_STAGE_BYTES },
                "{name}: job account = the bands' chunks ({threads} threads)"
            );
            // The chunk size is not part of the arithmetic: same bits as the whole-frame request.
            assert_eq!(bits(&out), bits(&serial.pixels), "{name}: {threads} threads");
            merged.push(account);
        }
        assert_eq!(merged[0], merged[1], "{name}: account depends on the thread count");

        // No heap allocation inside a band, whichever conversion entry point runs it.
        let job = eng.prepare_job(&job_request(&ctx, src, dst, WideBands::Sizes(&SIZES), intent, bpc), &t).unwrap();
        let mut start = 0usize;
        for (band, &n) in SIZES.iter().enumerate() {
            let px = &input[start * in_ch..(start + n) * in_ch];
            let mut out = vec![0.0f32; n * out_ch];
            let before = allocs();
            let report = job.convert_band(band, px, &mut out, &t).unwrap();
            assert_eq!(allocs(), before, "{name}: band {band} allocated while converting");
            assert_eq!(report.chunks, expected_band(n));
            start += n;
        }
        job.finish(&t, &mut Collect(Vec::new())).unwrap();
        if in_ch == out_ch {
            let job = eng.prepare_job(&job_request(&ctx, src, dst, WideBands::Sizes(&SIZES), intent, bpc), &t).unwrap();
            let mut start = 0usize;
            for (band, &n) in SIZES.iter().enumerate() {
                let mut buffer = input[start * in_ch..(start + n) * in_ch].to_vec();
                let before = allocs();
                let report = job.convert_band_in_place(band, &mut buffer, &t).unwrap();
                assert_eq!(allocs(), before, "{name}: in-place band {band} allocated while converting");
                assert_eq!(report.chunks, expected_band(n));
                start += n;
            }
            job.finish(&t, &mut Collect(Vec::new())).unwrap();
        }
        eprintln!(
            "WIDE_CHUNKS {{\"case\":\"{name}\",\"bound_pixels\":{BOUND},\"bands\":{},\"pixels\":{total},\"job_chunks\":{},\"whole_frame_chunks\":{},\"scratch_bytes\":{},\"bit_identical\":true,\"band_allocations\":0}}",
            SIZES.len(),
            merged[0].chunks,
            serial.receipt.chunks.chunks,
            merged[0].scratch_bytes
        );
    }
}

/// A token that is already canceled stops a band at its first chunk edge: nothing is written, whatever the band
/// size, through both conversion entry points. The mid-band cases (a cancel at chunk k leaves exactly k chunks, an
/// in-place band is converted prefix + untouched rest, a cancel after the last chunk leaves the whole band
/// converted) are deterministic unit tests in src/wide_job/tests.rs (the token is tripped at an exact chunk edge
/// by a cfg(test) hook, no timing).
#[test]
fn a_canceled_token_stops_a_band_before_its_first_chunk() {
    let ctx = Ctx::new();
    let t = CancellationToken::default();
    let canceled = CancellationToken::default();
    canceled.cancel();
    let n = MAX_WIDE_PIXELS;
    let input = device_pixels(3, n, 0x9e37_79b9_7f4a_7c15);
    let mut eng = engine(&ctx);
    let job = eng.prepare_job(&job_request(&ctx, SRGB, P3, WideBands::Sizes(&[n, n]), Intent::RelativeColorimetric, false), &t).unwrap();
    let mut out = vec![f32::NAN; n * 3];
    assert_eq!(job.convert_band(0, &input, &mut out, &canceled).err(), Some(WideError::Canceled));
    assert!(out.iter().all(|v| v.is_nan()), "a band canceled before its first chunk wrote output");
    let mut buffer = input.clone();
    assert_eq!(job.convert_band_in_place(1, &mut buffer, &canceled).err(), Some(WideError::Canceled));
    assert_eq!(bits(&buffer), bits(&input), "an in-place band canceled before its first chunk changed the caller's input");
    assert_eq!(job.finish(&t, &mut Collect(Vec::new())).err().map(|f| f.error), Some(WideError::Canceled));
}

/// The staging record follows the buffer that is really held: a request stages 64 x destination channels x 4 bytes
/// (Gray 256, RGB 768, CMYK 1024), a job band always the 1024-byte stack array, in the band report and the job
/// receipt, for plain and in-place conversion.
#[test]
fn staging_record_matches_the_buffer_a_request_and_a_job_band_really_hold() {
    let ctx = Ctx::new();
    let t = CancellationToken::default();
    let gray = assemble(0x0210_0000, b"mntr", b"GRAY", b"XYZ ", &[xyz_tag(D50), curv(Some(2.2))], &[(b"wtpt", 0), (b"kTRC", 1)]);
    let cmyk = cmyk_k_profile(9, 3, 20.0);
    let n = 200usize;
    // (destination, channels, request staging bytes)
    for (name, destination, out_ch, request_bytes) in [("gray", &gray[..], 1usize, 256usize), ("rgb", P3, 3, 768), ("cmyk", &cmyk[..], 4, 1024)] {
        let input = device_pixels(3, n, 0x1234_5678_9abc_def1);
        let mut eng = engine(&ctx);
        let request = run(&mut eng, &ctx, SRGB, destination, &input, Intent::RelativeColorimetric, false).unwrap();
        assert_eq!(request.receipt.chunks.scratch_bytes, request_bytes, "{name}: request staging buffer");
        assert_eq!(request.receipt.chunks, expected(n, out_ch), "{name}");
        let job = eng.prepare_job(&job_request(&ctx, SRGB, destination, WideBands::Sizes(&[n]), Intent::RelativeColorimetric, false), &t).unwrap();
        let mut out = vec![0.0f32; n * out_ch];
        let band = job.convert_band(0, &input, &mut out, &t).unwrap();
        assert_eq!(band.chunks, expected_band(n), "{name}: job band staging buffer");
        assert_eq!(job.finish(&t, &mut Collect(Vec::new())).unwrap().receipt.receipt.chunks.scratch_bytes, BAND_STAGE_BYTES, "{name}: job receipt");
        if out_ch == 3 {
            let mut eng = engine(&ctx);
            let job = eng.prepare_job(&job_request(&ctx, SRGB, destination, WideBands::Sizes(&[n]), Intent::RelativeColorimetric, false), &t).unwrap();
            let mut buffer = input.clone();
            let band = job.convert_band_in_place(0, &mut buffer, &t).unwrap();
            assert_eq!(band.chunks, expected_band(n), "{name}: in-place band staging buffer");
        }
    }
}

// ---------------------------------------------------------------- the admitted route carries the account
#[derive(Default)]
struct Ledger {
    live: AtomicU64,
    calls: AtomicUsize,
}
#[derive(Clone)]
struct Admission(Arc<Ledger>);
struct Lease(Arc<Ledger>, u64);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.live.fetch_sub(self.1, SeqCst);
    }
}
impl ProviderReservation for Lease {
    fn reserved_bytes(&self) -> u64 {
        self.1
    }
}
impl ProviderAdmission for Admission {
    type Reservation = Lease;
    fn synchronous_retirement(&self) -> bool {
        true
    }
    fn reserve(&self, bytes: u64) -> Result<Lease, Error> {
        self.0.calls.fetch_add(1, SeqCst);
        self.0.live.fetch_add(bytes, SeqCst);
        Ok(Lease(self.0.clone(), bytes))
    }
}
struct Null;
impl SinkPort for Null {
    fn try_send(&mut self, _class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        Ok(())
    }
}
fn admitted_supported() -> bool {
    if cfg!(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc")) {
        return true;
    }
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "NOT_PROVEN: wide_chunks admitted_route reason=admission_route_supported_on_x86_64_windows_msvc_only");
    false
}

#[test]
fn admitted_transform_and_job_report_their_chunks() {
    if !admitted_supported() {
        return;
    }
    let ctx = Ctx::new();
    let t = CancellationToken::default();
    let ledger = Arc::new(Ledger::default());
    let mut eng = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone())).unwrap();
    let pixels = device_pixels(3, 1000, 0x51ed_270b_2f17_8c3d);
    // A request: 1000 pixels = 16 chunks (15 x 64 + 40); the digest staging buffer is one chunk of the request lease.
    let transform = eng.transform(&ctx.request(SRGB, P3, &pixels, Intent::RelativeColorimetric, false), &t, &mut Null).unwrap();
    assert_eq!(transform.result().receipt.chunks, expected(1000, 3));
    assert_eq!(transform.result().receipt.chunks.chunks, 16);
    // A job over the same frame in two bands: 500 pixels = 8 chunks each (7 x 64 + 52).
    let job = eng.prepare_job(&job_request(&ctx, SRGB, P3, WideBands::Sizes(&[500, 500]), Intent::RelativeColorimetric, false), &t).unwrap();
    let live_before = (ledger.live.load(SeqCst), ledger.calls.load(SeqCst));
    let mut out = vec![0.0f32; 1500];
    let r0 = job.convert_band(0, &pixels[..1500], &mut out, &t).unwrap();
    let mut out2 = vec![0.0f32; 1500];
    let r1 = job.convert_band(1, &pixels[1500..], &mut out2, &t).unwrap();
    assert_eq!((r0.chunks, r1.chunks), (expected_band(500), expected_band(500)));
    assert_eq!(transform.result().receipt.chunks.scratch_bytes, 768, "a request stages 64 x 3 channels x 4 bytes");
    assert_eq!(r0.chunks.scratch_bytes, 1024, "a job band stages the fixed 1024-byte stack array");
    assert_eq!((ledger.live.load(SeqCst), ledger.calls.load(SeqCst)), live_before, "chunks charge nothing after prepare");
    let done = job.finish(&t, &mut Null).unwrap();
    assert_eq!(done.result().receipt.receipt.chunks.chunks, 16);
    assert_eq!(done.result().receipt.receipt.chunks.pixels, 1000);
    assert_eq!(done.result().receipt.receipt.chunks.max_chunk_pixels, BOUND);
    assert_eq!(done.result().receipt.receipt.chunks.scratch_bytes, BAND_STAGE_BYTES);
    assert_eq!([&out[..], &out2[..]].concat(), transform.result().pixels, "job and request give the same bits");
}

// ---------------------------------------------------------------- the literal reading, priced
/// What a caller pays when it drives the admitted transform itself in 64-pixel requests (the literal reading of
/// STU-CON-044) against the same pixels in one request and in one job band (internal 64-pixel chunks).
#[test]
#[ignore = "release timing; run: cargo test --release -p hsk-studio-prism --test wide_chunks -- --ignored --nocapture"]
fn bench_literal_64_pixel_admitted_calls_against_internal_chunks() {
    if !admitted_supported() {
        return;
    }
    let ctx = Ctx::new();
    let t = CancellationToken::default();
    let n = MAX_WIDE_PIXELS;
    let input = device_pixels(3, n, 0x2545_f491_4f6c_dd1d);
    let ledger = Arc::new(Ledger::default());
    for (name, src, dst) in [("linear_p3_to_linear_srgb", P3_LINEAR, SRGB_LINEAR), ("p3_to_srgb", P3, SRGB)] {
        let mut eng = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone())).unwrap();
        // Warm the plan so every run below is a cache hit.
        let _ = eng.transform(&ctx.request(src, dst, &input[..3], Intent::RelativeColorimetric, false), &t, &mut Null).unwrap();
        let start = Instant::now();
        let whole = eng.transform(&ctx.request(src, dst, &input, Intent::RelativeColorimetric, false), &t, &mut Null).unwrap();
        let request_ms = start.elapsed().as_secs_f64() * 1e3;
        let start = Instant::now();
        let mut literal = Vec::with_capacity(n * 3);
        for piece in input.chunks(BOUND * 3) {
            literal.extend_from_slice(&eng.transform(&ctx.request(src, dst, piece, Intent::RelativeColorimetric, false), &t, &mut Null).unwrap().result().pixels);
        }
        let literal_ms = start.elapsed().as_secs_f64() * 1e3;
        assert_eq!(bits(&literal), bits(&whole.result().pixels), "{name}: 64-pixel calls differ from one request");
        let start = Instant::now();
        let job = eng.prepare_job(&job_request(&ctx, src, dst, WideBands::Sizes(&[n]), Intent::RelativeColorimetric, false), &t).unwrap();
        let mut out = vec![0.0f32; n * 3];
        job.convert_band(0, &input, &mut out, &t).unwrap();
        job.finish(&t, &mut Null).unwrap();
        let job_ms = start.elapsed().as_secs_f64() * 1e3;
        assert_eq!(bits(&out), bits(&literal), "{name}: job band differs");
        let calls = n.div_ceil(BOUND);
        let scale = 24_000_000.0 / n as f64;
        eprintln!(
            "WIDE_CHUNKS_BENCH {{\"case\":\"{name}\",\"pixels\":{n},\"one_request_ms\":{request_ms:.0},\"literal_64px_calls\":{calls},\"literal_64px_ms\":{literal_ms:.0},\"literal_us_per_call\":{:.1},\"job_one_band_1thread_ms\":{job_ms:.0},\"literal_24mp_1thread_ms_extrapolated\":{:.0},\"job_24mp_1thread_ms_extrapolated\":{:.0}}}",
            literal_ms * 1e3 / calls as f64,
            literal_ms * scale,
            job_ms * scale
        );
    }
}
