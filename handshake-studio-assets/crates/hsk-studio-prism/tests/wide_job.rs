//! SMB-PRISM-ENG-7: thread-shared wide jobs. A job prepared once (`WideEngine::prepare_job` /
//! `AdmittedWideEngine::prepare_job`) converts disjoint bands from any number of threads; the output must be
//! bit-identical to the serial `WideEngine::transform` path for every thread count, the admitted route must charge
//! everything before the bands (same ledger trace for every thread count, nothing reserved or released while bands
//! run), and band misuse / band failures must be typed and decided by band order, not by the schedule.
//! The ignored bench prints the 24 MP P3 -> sRGB timings per thread count (`--release -- --ignored --nocapture`).
mod wide_support;
use hsk_studio_accord::CancellationToken;
use hsk_studio_prism::*;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use wide_support::*;

const SRGB_LINEAR: &[u8] = include_bytes!("profiles/sRGB-linear-v4.icc");
const P3_LINEAR: &[u8] = include_bytes!("profiles/DisplayP3-linear-v4.icc");
const P3: &[u8] = include_bytes!("profiles/DisplayP3-v4.icc");
const THREADS: [usize; 4] = [1, 2, 8, 16];
/// Uneven bands (single pixel, chunk edges at 1023/1024/1025, odd sizes); 30 000 pixels in total.
const SIZES: [usize; 8] = [1, 1023, 1024, 1025, 4097, 7000, 3333, 12_497];

const fn _shared<T: Send + Sync>() {}
const _: () = _shared::<WideJob<'static>>();

/// Deterministic device values in 0..1 (64-bit LCG) with exact 0 and 1 corners.
fn device_pixels(channels: usize, count: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    let mut out: Vec<f32> = (0..count * channels)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 40) as f32) / 16_777_216.0
        })
        .collect();
    for (i, v) in [0.0f32, 1.0, 0.0, 1.0, 1.0, 0.0].iter().cycle().take(channels * 4).enumerate() {
        out[i] = *v;
    }
    out
}
/// PCS Lab triples: L 0..100, a/b -128..128.
fn lab_pixels(count: usize) -> Vec<f32> {
    device_pixels(3, count, 0x9e37_79b9_7f4a_7c15)
        .chunks_exact(3)
        .flat_map(|p| [p[0] * 100.0, p[1] * 256.0 - 128.0, p[2] * 256.0 - 128.0])
        .collect()
}

fn job_request<'a>(
    ctx: &'a Ctx,
    source: Option<&'a [u8]>,
    destination: &'a [u8],
    bands: WideBands<'a>,
    intent: Intent,
    bpc: bool,
) -> WideJobRequest<'a> {
    WideJobRequest {
        source: source.map_or(WideSource::Lab, |s| WideSource::Profile(ctx.input(&ctx.source, s))),
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

type BandOutcome = Result<WideBandReport, WideError>;
/// (name, source profile or None for a Lab source, destination, input channels, intent, BPC).
type Case<'a> = (&'static str, Option<&'a [u8]>, &'a [u8], usize, Intent, bool);

/// `threads` workers pop (band, input, output) tasks from a shared queue (band 0 first) and convert them with
/// `convert`; outcomes are returned in band order.
fn run_bands<F>(threads: usize, sizes: &[usize], ch: (usize, usize), input: &[f32], output: &mut [f32], convert: F) -> Vec<BandOutcome>
where
    F: Fn(usize, &[f32], &mut [f32]) -> BandOutcome + Sync,
{
    let mut tasks = Vec::new();
    let (mut rest_in, mut rest_out) = (input, output);
    for (band, &n) in sizes.iter().enumerate() {
        let (i, ri) = rest_in.split_at(n * ch.0);
        let (o, ro) = std::mem::take(&mut rest_out).split_at_mut(n * ch.1);
        tasks.push((band, i, o));
        (rest_in, rest_out) = (ri, ro);
    }
    tasks.reverse();
    let queue = Mutex::new(tasks);
    let results: Mutex<Vec<Option<BandOutcome>>> = Mutex::new(vec![None; sizes.len()]);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let task = queue.lock().unwrap().pop();
                    let Some((band, i, o)) = task else { break };
                    let r = convert(band, i, o);
                    results.lock().unwrap()[band] = Some(r);
                }
            });
        }
    });
    results.into_inner().unwrap().into_iter().map(|r| r.expect("every band ran")).collect()
}
/// In-place variant of `run_bands` over `buffer`.
fn run_bands_in_place<F>(threads: usize, sizes: &[usize], ch: usize, buffer: &mut [f32], convert: F) -> Vec<BandOutcome>
where
    F: Fn(usize, &mut [f32]) -> BandOutcome + Sync,
{
    let mut tasks = Vec::new();
    let mut rest = buffer;
    for (band, &n) in sizes.iter().enumerate() {
        let (b, r) = std::mem::take(&mut rest).split_at_mut(n * ch);
        tasks.push((band, b));
        rest = r;
    }
    tasks.reverse();
    let queue = Mutex::new(tasks);
    let results: Mutex<Vec<Option<BandOutcome>>> = Mutex::new(vec![None; sizes.len()]);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let task = queue.lock().unwrap().pop();
                    let Some((band, b)) = task else { break };
                    let r = convert(band, b);
                    results.lock().unwrap()[band] = Some(r);
                }
            });
        }
    });
    results.into_inner().unwrap().into_iter().map(|r| r.expect("every band ran")).collect()
}
fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}
fn sizes_of(job: &WideJob<'_>) -> Vec<usize> {
    (0..job.bands()).map(|b| job.band_pixels(b).unwrap()).collect()
}

#[test]
fn job_bands_are_bit_identical_to_the_serial_path_for_every_thread_count() {
    let ctx = Ctx::new();
    let cmyk = cmyk_k_profile(9, 3, 20.0);
    let total: usize = SIZES.iter().sum();
    let cases: [Case<'_>; 5] = [
        ("linear_p3_to_linear_srgb", Some(P3_LINEAR), SRGB_LINEAR, 3, Intent::RelativeColorimetric, false),
        ("p3_to_srgb", Some(P3), SRGB, 3, Intent::RelativeColorimetric, false),
        ("srgb_to_p3_perceptual_bpc", Some(SRGB), P3, 3, Intent::Perceptual, true),
        ("cmyk_table_to_srgb_bpc", Some(&cmyk), SRGB, 4, Intent::RelativeColorimetric, true),
        ("lab_to_cmyk_table", None, &cmyk, 3, Intent::RelativeColorimetric, false),
    ];
    let t = CancellationToken::default();
    for (name, src, dst, in_ch, intent, bpc) in cases {
        let input = if src.is_some() { device_pixels(in_ch, total, 0x2545_f491_4f6c_dd1d) } else { lab_pixels(total) };
        // Serial reference: one request over the whole frame.
        let mut serial_engine = engine(&ctx);
        let mut request = ctx.request(src.unwrap_or(SRGB), dst, &input, intent, bpc);
        if src.is_none() {
            request.source = WideSource::Lab;
        }
        let serial = serial_engine.transform(&request, &t, &mut Collect(Vec::new())).unwrap();
        let out_ch = serial.pixels.len() / total;
        // Serial digests of two single bands (band 2 = 1024 px at offset 1024, the last band).
        let mut band_digest = |band: usize| {
            let start: usize = SIZES[..band].iter().sum();
            let px = &input[start * in_ch..(start + SIZES[band]) * in_ch];
            let mut r = ctx.request(src.unwrap_or(SRGB), dst, px, intent, bpc);
            if src.is_none() {
                r.source = WideSource::Lab;
            }
            serial_engine.transform(&r, &t, &mut Collect(Vec::new())).unwrap().receipt.output_sha256
        };
        let expected_band = [(2, band_digest(2)), (SIZES.len() - 1, band_digest(SIZES.len() - 1))];
        let mut eng = engine(&ctx);
        let mut job_digests = Vec::new();
        for (round, threads) in THREADS.into_iter().enumerate() {
            let job = eng.prepare_job(&job_request(&ctx, src, dst, WideBands::Sizes(&SIZES), intent, bpc), &t).unwrap();
            assert_eq!((job.bands(), job.total_pixels(), job.input_channels(), job.output_channels()), (SIZES.len(), total as u64, in_ch, out_ch));
            assert_eq!(job.cache_hit(), round > 0, "{name}: plan cached after the first job");
            let mut out = vec![f32::NAN; total * out_ch];
            let outcomes = run_bands(threads, &SIZES, (in_ch, out_ch), &input, &mut out, |b, i, o| job.convert_band(b, i, o, &t));
            let reports: Vec<WideBandReport> = outcomes.into_iter().map(|r| r.unwrap()).collect();
            for (band, digest) in expected_band {
                assert_eq!(reports[band].output_sha256, digest, "{name}: band {band} digest = serial request digest");
                assert_eq!(reports[band].pixel_count, SIZES[band]);
            }
            let result = job.finish(&t, &mut Collect(Vec::new())).unwrap();
            assert_eq!(bits(&out), bits(&serial.pixels), "{name}: {threads} threads differ from the serial path");
            let (jr, sr) = (&result.receipt.receipt, &serial.receipt);
            assert_eq!(jr.clip, sr.clip, "{name}: merged clip report = serial clip report");
            assert_eq!((jr.bpc, jr.absolute, jr.intent, jr.engine_build), (sr.bpc, sr.absolute, sr.intent, sr.engine_build), "{name}");
            assert_eq!((jr.source_sha256, jr.destination_sha256, jr.source_space, jr.pixel_count), (sr.source_sha256, sr.destination_sha256, sr.source_space, total));
            assert_eq!((result.receipt.bands, result.receipt.total_pixels, result.receipt.digest_rule), (SIZES.len(), total as u64, JOB_DIGEST_RULE));
            job_digests.push(jr.output_sha256);
        }
        assert!(job_digests.windows(2).all(|w| w[0] == w[1]), "{name}: job digest depends on the thread count");
        // In place (same channel counts) and a uniform layout give the same bits.
        if in_ch == out_ch {
            let job = eng.prepare_job(&job_request(&ctx, src, dst, WideBands::Sizes(&SIZES), intent, bpc), &t).unwrap();
            let mut buffer = input.clone();
            let outcomes = run_bands_in_place(8, &SIZES, in_ch, &mut buffer, |b, buf| job.convert_band_in_place(b, buf, &t));
            assert!(outcomes.iter().all(|r| r.is_ok()));
            let result = job.finish(&t, &mut Collect(Vec::new())).unwrap();
            assert_eq!(bits(&buffer), bits(&serial.pixels), "{name}: in place");
            assert_eq!(result.receipt.receipt.output_sha256, job_digests[0], "{name}: in-place job digest");
        }
        let job = eng
            .prepare_job(&job_request(&ctx, src, dst, WideBands::Uniform { pixels: total, band_pixels: 4096 }, intent, bpc), &t)
            .unwrap();
        let sizes = sizes_of(&job);
        assert_eq!((sizes.len(), sizes[sizes.len() - 1]), (8, total - 7 * 4096));
        let mut out = vec![0.0f32; total * out_ch];
        let outcomes = run_bands(16, &sizes, (in_ch, out_ch), &input, &mut out, |b, i, o| job.convert_band(b, i, o, &t));
        assert!(outcomes.iter().all(|r| r.is_ok()));
        job.finish(&t, &mut Collect(Vec::new())).unwrap();
        assert_eq!(bits(&out), bits(&serial.pixels), "{name}: uniform layout");
        eprintln!("WIDE_JOB_IDENTITY {{\"case\":\"{name}\",\"pixels\":{total},\"bands\":{},\"threads\":{THREADS:?},\"bit_identical\":true}}", SIZES.len());
    }
}

#[derive(Default)]
struct Ledger {
    live: AtomicU64,
    calls: AtomicUsize,
    deny_call: AtomicUsize,
    trace: Mutex<Vec<u64>>,
    refuse_retire: AtomicBool,
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
    fn retirement_ready(&self) -> Result<(), Error> {
        if self.0.refuse_retire.load(SeqCst) { Err(Error::RetirementUnavailable) } else { Ok(()) }
    }
}
impl ProviderAdmission for Admission {
    type Reservation = Lease;
    fn synchronous_retirement(&self) -> bool {
        true
    }
    fn reserve(&self, bytes: u64) -> Result<Lease, Error> {
        let call = self.0.calls.fetch_add(1, SeqCst) + 1;
        if self.0.deny_call.load(SeqCst) == call {
            return Err(Error::AdmissionDenied);
        }
        self.0.trace.lock().unwrap().push(bytes);
        self.0.live.fetch_add(bytes, SeqCst);
        Ok(Lease(self.0.clone(), bytes))
    }
}

#[test]
fn admitted_job_charges_everything_before_the_bands_for_every_thread_count() {
    if !cfg!(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc")) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "NOT_PROVEN: wide_job admitted_target reason=admission_route_supported_on_x86_64_windows_msvc_only");
        return;
    }
    let ctx = Ctx::new();
    let t = CancellationToken::default();
    let cmyk = cmyk_k_profile(9, 3, 20.0);
    let total = 20_000usize;
    let input = device_pixels(4, total, 0x51ed_270b_2f17_8c3d);
    let bands = WideBands::Uniform { pixels: total, band_pixels: 3000 };
    let reference = {
        let mut plain = engine(&ctx);
        run(&mut plain, &ctx, &cmyk, SRGB, &input, Intent::RelativeColorimetric, true).unwrap()
    };
    let mut traces = Vec::new();
    for threads in THREADS {
        let ledger = Arc::new(Ledger::default());
        let mut eng = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone())).unwrap();
        let provider = eng.provider_reserved_bytes();
        let calls_before = ledger.calls.load(SeqCst);
        let job = eng.prepare_job(&job_request(&ctx, Some(&cmyk), SRGB, bands, Intent::RelativeColorimetric, true), &t).unwrap();
        // Miss: work, receipt, plan and build leases were reserved; the build lease is already released.
        assert_eq!(ledger.calls.load(SeqCst) - calls_before, 4, "work, receipt, plan, build");
        let after_prepare = (ledger.live.load(SeqCst), ledger.calls.load(SeqCst));
        let plan = after_prepare.0 - provider - job.reserved_bytes();
        assert!(plan > 0 && job.reserved_bytes() > 0);
        let sizes = sizes_of(job.job());
        let mut out = vec![0.0f32; total * 3];
        let outcomes = run_bands(threads, &sizes, (4, 3), &input, &mut out, |b, i, o| {
            // Nothing is reserved or released while bands run, on any thread.
            assert_eq!((ledger.live.load(SeqCst), ledger.calls.load(SeqCst)), after_prepare);
            job.convert_band(b, i, o, &t)
        });
        assert!(outcomes.iter().all(|r| r.is_ok()));
        assert_eq!((ledger.live.load(SeqCst), ledger.calls.load(SeqCst)), after_prepare, "{threads} threads: no charge during bands");
        let result = job.finish(&t, &mut Collect(Vec::new())).unwrap();
        // The work lease is released; the receipt lease travels with the result.
        assert_eq!(ledger.live.load(SeqCst), provider + plan + result.reserved_bytes());
        assert_eq!(bits(&out), bits(&reference.pixels), "{threads} threads: admitted job differs from the serial path");
        assert_eq!(result.result().receipt.receipt.clip, reference.receipt.clip);
        drop(result);
        assert_eq!((ledger.live.load(SeqCst), eng.plan_reserved_bytes()), (provider + plan, plan));
        // Hit: only the work and receipt leases.
        let calls = ledger.calls.load(SeqCst);
        let job = eng.prepare_job(&job_request(&ctx, Some(&cmyk), SRGB, bands, Intent::RelativeColorimetric, true), &t).unwrap();
        assert!(job.job().cache_hit());
        assert_eq!(ledger.calls.load(SeqCst) - calls, 2, "hit: work + receipt");
        drop(job);
        assert_eq!(ledger.live.load(SeqCst), provider + plan, "a dropped job releases its leases");
        traces.push(ledger.trace.lock().unwrap().clone());
        assert!(eng.try_retire().is_none());
        assert_eq!(ledger.live.load(SeqCst), 0);
    }
    assert!(traces.windows(2).all(|w| w[0] == w[1]), "ledger trace depends on the thread count: {traces:?}");
    // A denied plan reservation (3rd of the call: work, receipt, plan) parses nothing and caches nothing.
    let ledger = Arc::new(Ledger::default());
    let mut eng = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone())).unwrap();
    let provider = eng.provider_reserved_bytes();
    ledger.deny_call.store(ledger.calls.load(SeqCst) + 3, SeqCst);
    let denied = eng.prepare_job(&job_request(&ctx, Some(&cmyk), SRGB, bands, Intent::RelativeColorimetric, true), &t).err().expect("denied");
    assert!(matches!(denied, AdmittedWideError::Admission(Error::AdmissionDenied)), "{denied:?}");
    assert_eq!((eng.cache_stats().parses, eng.cache_stats().entries, ledger.live.load(SeqCst)), (0, 0, provider));
    // A layout refusal is found before anything is charged.
    let calls = ledger.calls.load(SeqCst);
    let refused = eng.prepare_job(&job_request(&ctx, Some(&cmyk), SRGB, WideBands::Sizes(&[]), Intent::RelativeColorimetric, true), &t).err().expect("refused");
    assert!(matches!(refused, AdmittedWideError::Refused(WideError::ChannelCount)), "{refused:?}");
    assert_eq!(ledger.calls.load(SeqCst), calls);
}

#[test]
fn band_contract_is_typed_and_finish_reports_the_lowest_failed_band() {
    let ctx = Ctx::new();
    let t = CancellationToken::default();
    let sizes = [100usize, 100, 100, 100];
    let input = device_pixels(3, 400, 0x1234_5678_9abc_def1);
    let request = |bands| job_request(&ctx, Some(SRGB), P3, bands, Intent::RelativeColorimetric, false);
    let mut eng = engine(&ctx);
    // Layout and request refusals: typed, before any plan.
    for (bands, want) in [
        (WideBands::Uniform { pixels: 0, band_pixels: 10 }, WideError::ChannelCount),
        (WideBands::Uniform { pixels: 10, band_pixels: MAX_WIDE_PIXELS + 1 }, WideError::PixelLimit),
        (WideBands::Uniform { pixels: MAX_WIDE_JOB_BANDS + 1, band_pixels: 1 }, WideError::PixelLimit),
        (WideBands::Sizes(&[1, 0]), WideError::ChannelCount),
        (WideBands::Sizes(&[1, MAX_WIDE_PIXELS + 1]), WideError::PixelLimit),
    ] {
        assert_eq!(eng.prepare_job(&request(bands), &t).err(), Some(want), "{bands:?}");
    }
    let mut stale = request(WideBands::Sizes(&sizes));
    stale.expected_revision = 2;
    assert_eq!(eng.prepare_job(&stale, &t).err(), Some(WideError::RevisionMismatch));
    assert_eq!(eng.cache_stats().parses, 0, "refusals parse nothing");

    // Misuse of one job: index, lengths (no claim), double claim, failure recorded and claimed.
    let job = eng.prepare_job(&request(WideBands::Sizes(&sizes)), &t).unwrap();
    let mut out = vec![0.0f32; 300];
    assert_eq!(job.convert_band(4, &input[..300], &mut out, &t).err(), Some(WideError::BandIndex));
    assert_eq!(job.convert_band(0, &input[..299], &mut out, &t).err(), Some(WideError::BandLength));
    assert_eq!(job.convert_band(0, &input[..300], &mut out[..297], &t).err(), Some(WideError::BandLength));
    assert!(job.convert_band(0, &input[..300], &mut out, &t).is_ok(), "length refusals do not claim");
    assert_eq!(job.convert_band(0, &input[..300], &mut out, &t).err(), Some(WideError::BandClaimed));
    let mut bad = input[300..600].to_vec();
    bad[5] = f32::NAN;
    assert_eq!(job.convert_band(1, &bad, &mut out, &t).err(), Some(WideError::NonfiniteChannel));
    assert_eq!(job.convert_band(1, &input[300..600], &mut out, &t).err(), Some(WideError::BandClaimed), "a failed band stays claimed");
    let rejected = |e: &mut WideEngine| e.cache_stats().rejected;
    // Band 1 failed and band 2 was never converted: the lowest of them (band 1) decides.
    let failure = job.finish(&t, &mut Collect(Vec::new())).expect_err("band 1 failed");
    assert_eq!((failure.error, failure.operation_error), (WideError::NonfiniteChannel, Some(WideError::NonfiniteChannel)));
    assert!(failure.delivery.is_ok(), "the failure is delivered");
    let rejected_after = rejected(&mut eng);
    assert!(rejected_after >= 1);

    // Same verdict for every thread count: band 1 non-finite, band 3 out of range.
    let mut bad_frame = input.clone();
    bad_frame[100 * 3 + 7] = f32::INFINITY;
    bad_frame[300 * 3 + 2] = 1.5;
    for threads in THREADS {
        let job = eng.prepare_job(&request(WideBands::Sizes(&sizes)), &t).unwrap();
        let mut out = vec![0.0f32; 1200];
        let outcomes = run_bands(threads, &sizes, (3, 3), &bad_frame, &mut out, |b, i, o| job.convert_band(b, i, o, &t));
        let errors: Vec<Option<WideError>> = outcomes.iter().map(|r| r.err()).collect();
        assert_eq!(errors, [None, Some(WideError::NonfiniteChannel), None, Some(WideError::ChannelRange)], "{threads} threads");
        assert_eq!(job.finish(&t, &mut Collect(Vec::new())).err().map(|f| f.error), Some(WideError::NonfiniteChannel), "{threads} threads");
    }
    // A band never converted: JobIncomplete.
    let job = eng.prepare_job(&request(WideBands::Sizes(&sizes)), &t).unwrap();
    for b in [0usize, 1, 3] {
        job.convert_band(b, &input[b * 300..(b + 1) * 300], &mut out, &t).unwrap();
    }
    assert_eq!(job.finish(&t, &mut Collect(Vec::new())).err().map(|f| f.error), Some(WideError::JobIncomplete));
    // Cancellation: the band refuses, finish reports it.
    let canceled = CancellationToken::default();
    let job = eng.prepare_job(&request(WideBands::Sizes(&sizes)), &t).unwrap();
    canceled.cancel();
    assert_eq!(job.convert_band(0, &input[..300], &mut out, &canceled).err(), Some(WideError::Canceled));
    assert_eq!(job.finish(&t, &mut Collect(Vec::new())).err().map(|f| f.error), Some(WideError::Canceled));
    // In place needs equal channel counts (RGB -> CMYK has 3 in, 4 out).
    let cmyk = cmyk_k_profile(5, 3, 20.0);
    let job = eng.prepare_job(&job_request(&ctx, Some(SRGB), &cmyk, WideBands::Sizes(&sizes), Intent::RelativeColorimetric, false), &t).unwrap();
    let mut buffer = input[..300].to_vec();
    assert_eq!(job.convert_band_in_place(0, &mut buffer, &t).err(), Some(WideError::ChannelCount));
    drop(job);
    assert!(rejected(&mut eng) > rejected_after);
}

/// 24 MP (6000 x 4000) P3 -> sRGB: serial `transform` in MAX_WIDE_PIXELS requests (one thread) against a job of
/// 65 536-pixel bands (develop's 256 x 256 tiles) at 1/2/8/16 threads; every run is checked bit-identical.
#[test]
#[ignore = "release timing; run: cargo test --release -p hsk-studio-prism --test wide_job -- --ignored --nocapture"]
fn bench_24mp_p3_to_srgb_by_thread_count() {
    let ctx = Ctx::new();
    let t = CancellationToken::default();
    let total = 6000 * 4000;
    let input = device_pixels(3, total, 0x2545_f491_4f6c_dd1d);
    for (name, src, dst) in [("linear_p3_to_linear_srgb", P3_LINEAR, SRGB_LINEAR), ("p3_to_srgb", P3, SRGB)] {
        let mut eng = engine(&ctx);
        let _ = run(&mut eng, &ctx, src, dst, &input[..3], Intent::RelativeColorimetric, false).unwrap();
        let start = Instant::now();
        let mut serial = Vec::with_capacity(total * 3);
        for chunk in input.chunks(MAX_WIDE_PIXELS * 3) {
            serial.extend_from_slice(&run(&mut eng, &ctx, src, dst, chunk, Intent::RelativeColorimetric, false).unwrap().pixels);
        }
        let serial_ms = start.elapsed().as_secs_f64() * 1e3;
        let mut line = format!("WIDE_JOB_BENCH {{\"case\":\"{name}\",\"pixels\":{total},\"serial_transform_ms\":{serial_ms:.0}");
        for threads in THREADS {
            let mut out = vec![0.0f32; total * 3];
            let start = Instant::now();
            let job = eng
                .prepare_job(&job_request(&ctx, Some(src), dst, WideBands::Uniform { pixels: total, band_pixels: 65_536 }, Intent::RelativeColorimetric, false), &t)
                .unwrap();
            let sizes = sizes_of(&job);
            let outcomes = run_bands(threads, &sizes, (3, 3), &input, &mut out, |b, i, o| job.convert_band(b, i, o, &t));
            let result = job.finish(&t, &mut Collect(Vec::new())).unwrap();
            let ms = start.elapsed().as_secs_f64() * 1e3;
            assert!(outcomes.iter().all(|r| r.is_ok()) && result.receipt.bands == sizes.len());
            assert!(bits(&out) == bits(&serial), "{name}: {threads} threads not bit-identical");
            line.push_str(&format!(",\"job_{threads}t_ms\":{ms:.0}"));
        }
        eprintln!("{line},\"bit_identical\":true}}");
    }
}
