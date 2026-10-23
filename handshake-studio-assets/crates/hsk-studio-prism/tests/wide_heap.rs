//! SMB-PRISM-ENG-6 R6 review A, PLAUSIBLE item "lease sizes are reasoned bounds, not measured heap" (V4-07).
//!
//! `AdmittedWideEngine` charges the caller's ledger from factors derived from the ICC tag table (parse 2x, plan 8x
//! stored bytes, BPC 4 extra stages, ...). This file measures what the allocator is actually asked for with a
//! counting global allocator (requested bytes, the unit the leases are documented in; not RSS, not allocator
//! bookkeeping) and compares it with the leases of the same call, per lease kind and per profile family:
//!
//! * provider lease   <- heap kept by `AdmittedWideEngine::new`, and its peak;
//! * parse lease      <- peak heap of `inspect_profile`;
//! * request lease    <- transient heap of a transform on a cached plan (digest staging, Observe, strings);
//! * output lease     <- heap that stays alive with the returned result;
//! * plan lease       <- heap that stays alive with the cached plan, once the result is dropped;
//! * build lease      <- transient heap above plan and output while a plan is built (parse + BPC stages);
//! * work/receipt     <- the same for a job (`prepare_job` ... `finish`).
//!
//! Total check per call: peak heap above the call's start <= the leases the call holds.
//!
//! The allocator counts per thread (a test runs on its own thread), and every measured operation is single-threaded.
//! Families: matrix/TRC pair (fused), v2 `curv` gamma, `curv` tables at the parse limit, 16-bit and 8-bit CLUT
//! tables (CMYK source and destination, BPC on, ramp probes), `mAB `; plus installed CMYK profiles from the oracle
//! corpus when present (a missing one prints `NOT_PROVEN:`). One `ADMITTED_HEAP {json}` line per case reports the
//! measured / leased ratios.
mod wide_support;
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_prism::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use wide_support::*;

const P3: &[u8] = include_bytes!("profiles/DisplayP3-v4.icc");

// ---------------------------------------------------------------- counting allocator (requested bytes, per thread)
thread_local! {
    static LIVE: Cell<i64> = const { Cell::new(0) };
    static PEAK: Cell<i64> = const { Cell::new(0) };
}
fn live() -> i64 {
    LIVE.with(Cell::get)
}
fn reset_peak() {
    PEAK.with(|p| p.set(live()));
}
fn peak() -> i64 {
    PEAK.with(Cell::get)
}
fn grow(n: usize) {
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    let _ = LIVE.try_with(|l| {
        let v = l.get().saturating_add(n);
        l.set(v);
        let _ = PEAK.try_with(|p| {
            if v > p.get() {
                p.set(v);
            }
        });
    });
}
fn shrink(n: usize) {
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    let _ = LIVE.try_with(|l| l.set(l.get().saturating_sub(n)));
}
struct Counting;
// SAFETY: every method forwards to the System allocator with the arguments it received; the only addition is a
// thread-local counter update that never allocates (try_with, const-initialised cells).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        shrink(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // New block counted before the old one is released: the conservative reading of an allocate-copy-free realloc.
        grow(new_size);
        shrink(layout.size());
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

/// Heap above the start of a measured closure: the highest point it reached and what is still alive after it
/// (the returned value included).
#[derive(Clone, Copy, Debug)]
struct Heap {
    peak: i64,
    kept: i64,
}
fn measure<T>(f: impl FnOnce() -> T) -> (T, Heap) {
    let start = live();
    reset_peak();
    let value = f();
    let heap = Heap { peak: peak() - start, kept: live() - start };
    (value, heap)
}
/// Bytes released by dropping `value`.
fn freed_by_drop<T>(value: T) -> i64 {
    let before = live();
    drop(value);
    before - live()
}

// ---------------------------------------------------------------- ledger that logs each reservation in order
struct Ledger {
    live: AtomicU64,
    /// Reservation sizes in call order. Pre-sized and cleared in place, so logging a reservation never allocates
    /// inside a measured region (the mock must not charge itself to the engine).
    log: Mutex<Vec<u64>>,
}
impl Ledger {
    fn new() -> Arc<Self> {
        Arc::new(Self { live: AtomicU64::new(0), log: Mutex::new(Vec::with_capacity(64)) })
    }
    fn take(&self) -> Vec<i64> {
        let mut log = self.log.lock().unwrap();
        let out = log.iter().map(|b| *b as i64).collect();
        log.clear();
        out
    }
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
        self.0.log.lock().unwrap().push(bytes);
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
fn admitted_supported(case: &str) -> bool {
    if cfg!(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc")) {
        return true;
    }
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "NOT_PROVEN: wide_heap {case} reason=admission_route_supported_on_x86_64_windows_msvc_only");
    false
}

// ---------------------------------------------------------------- profile builders (synthetic, no installed bytes)
fn trc_table(entries: usize) -> Vec<u8> {
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend_from_slice(&be32(entries as u32));
    for i in 0..entries {
        let v = ((i as f64 / (entries - 1) as f64).powf(2.2) * 65535.0).round() as u16;
        t.extend_from_slice(&v.to_be_bytes());
    }
    t
}
/// RGB matrix/TRC profile whose three TRC tags share one `curv` blob (stored bytes are counted per tag).
fn rgb_matrix(version: u32, trc: Vec<u8>) -> Vec<u8> {
    let blobs = vec![xyz_tag(D50), xyz_tag([0.4360, 0.2225, 0.0139]), xyz_tag([0.3851, 0.7169, 0.0971]), xyz_tag([0.1431, 0.0606, 0.7141]), trc];
    assemble(
        version,
        b"mntr",
        b"RGB ",
        b"XYZ ",
        &blobs,
        &[(b"wtpt", 0), (b"rXYZ", 1), (b"gXYZ", 2), (b"bXYZ", 3), (b"rTRC", 4), (b"gTRC", 4), (b"bTRC", 4)],
    )
}
/// `mft1` (8-bit tables): identity 256-entry input and output tables, the given u8 CLUT.
fn mft1(in_ch: u8, out_ch: u8, grid: u8, clut: &[u8]) -> Vec<u8> {
    assert_eq!(clut.len(), usize::from(grid).pow(u32::from(in_ch)) * usize::from(out_ch));
    let mut t = b"mft1\0\0\0\0".to_vec();
    t.extend_from_slice(&[in_ch, out_ch, grid, 0]);
    for v in [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        t.extend_from_slice(&s15(v));
    }
    for _ in 0..in_ch {
        t.extend((0..=255u8).collect::<Vec<_>>());
    }
    t.extend_from_slice(clut);
    for _ in 0..out_ch {
        t.extend((0..=255u8).collect::<Vec<_>>());
    }
    t
}
/// 8-bit K-ramp CMYK printer (L* 100 at K 0, 20 at K 1), the `mft1` counterpart of `cmyk_k_profile`.
fn cmyk_k_profile_u8(grid: u8) -> Vec<u8> {
    let g = usize::from(grid);
    let mut a2b = Vec::with_capacity(g.pow(4) * 3);
    for _c in 0..g {
        for _m in 0..g {
            for _y in 0..g {
                for k in 0..g {
                    let l = 255.0 * (1.0 - 0.8 * k as f64 / (g - 1) as f64);
                    a2b.extend_from_slice(&[l.round() as u8, 128, 128]);
                }
            }
        }
    }
    let b2a_grid = 9usize;
    let mut b2a = Vec::with_capacity(b2a_grid.pow(3) * 4);
    for li in 0..b2a_grid {
        for _a in 0..b2a_grid {
            for _b in 0..b2a_grid {
                let l = li as f64 / (b2a_grid - 1) as f64;
                let k = ((1.0 - l) / 0.8).clamp(0.0, 1.0);
                b2a.extend_from_slice(&[0, 0, 0, (k * 255.0).round() as u8]);
            }
        }
    }
    cmyk_profile(mft1(4, 3, grid, &a2b), Some(mft1(3, 4, b2a_grid as u8, &b2a)), 3, D50)
}

// ---------------------------------------------------------------- the measurement
struct Case {
    name: String,
    source: Vec<u8>,
    destination: Vec<u8>,
    in_ch: usize,
    intent: Intent,
    bpc: bool,
}
fn pixels(in_ch: usize, count: usize) -> Vec<f32> {
    (0..count * in_ch).map(|i| ((i * 7 + 3) % 101) as f32 / 100.0).collect()
}
fn ratio(measured: i64, leased: i64) -> f64 {
    measured.max(0) as f64 / leased.max(1) as f64
}

/// Per kind: measured bytes must be <= leased bytes; returns the JSON member `"kind":[measured,leased]`.
fn check(case: &str, kind: &str, measured: i64, leased: i64, members: &mut Vec<String>) {
    assert!(
        measured <= leased,
        "{case}: {kind} lease {leased} is below the measured heap {measured} (ratio {:.2}): the ledger under-reports",
        ratio(measured, leased)
    );
    members.push(format!("\"{kind}\":[{measured},{leased}]"));
}

fn run_case(c: &Case) {
    let ctx = Ctx::new();
    let t = CancellationToken::default();
    let name = c.name.as_str();
    let mut members: Vec<String> = Vec::new();

    // Provider lease: the engine's own vectors and actor strings.
    let ledger = Ledger::new();
    let (eng, h) = measure(|| AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone())).unwrap());
    let log = ledger.take();
    assert_eq!(log.len(), 1, "{name}: construction reserves once");
    check(name, "provider_kept", h.kept, log[0], &mut members);
    check(name, "provider_peak", h.peak, log[0], &mut members);
    let mut eng = eng;

    // Parse lease: admission of one profile (parsed by moxcms, then dropped).
    let (descriptor, h) = measure(|| eng.inspect_profile(ctx.input(&ctx.source, &c.source), &t).unwrap());
    let log = ledger.take();
    assert_eq!(log.len(), 1, "{name}: inspect reserves the parse lease once");
    check(name, "parse_peak", h.peak, log[0], &mut members);
    drop(descriptor);

    // A transform that builds the plan (miss): request, output, plan, build.
    let px = pixels(c.in_ch, 100);
    let (result, miss) = measure(|| eng.transform(&ctx.request(&c.source, &c.destination, &px, c.intent, c.bpc), &t, &mut Null).unwrap());
    let log = ledger.take();
    assert_eq!(log.len(), 4, "{name}: a miss reserves request, output, plan and build");
    let (request, output, plan, build) = (log[0], log[1], log[2], log[3]);
    assert_eq!(eng.plan_reserved_bytes() as i64, plan, "{name}: the engine keeps the plan lease it charged");
    assert!(!result.result().receipt.cache_hit);
    let output_kept = freed_by_drop(result);
    let plan_kept = miss.kept - output_kept;
    check(name, "output_kept", output_kept, output, &mut members);
    check(name, "plan_kept", plan_kept, plan, &mut members);
    // What the plan build needs above the plan it keeps and the output it hands back must fit the build + request
    // leases: the build lease cannot borrow slack from the output lease.
    check(name, "build_transient", miss.peak - plan_kept - output_kept, build + request, &mut members);
    check(name, "miss_total_peak", miss.peak, request + output + plan + build, &mut members);

    // The same request on the cached plan (hit): request + output only.
    let (result, hit) = measure(|| eng.transform(&ctx.request(&c.source, &c.destination, &px, c.intent, c.bpc), &t, &mut Null).unwrap());
    let log = ledger.take();
    assert_eq!(log.len(), 2, "{name}: a hit reserves request and output only");
    assert!(result.result().receipt.cache_hit);
    let output_kept = freed_by_drop(result);
    check(name, "hit_output_kept", output_kept, log[1], &mut members);
    check(name, "request_transient", hit.peak - output_kept, log[0], &mut members);
    check(name, "hit_total_peak", hit.peak, log[0] + log[1], &mut members);

    // Retiring the cache frees what the plan lease covered.
    let before_clear = live();
    eng.try_clear_cache().unwrap();
    let freed = before_clear - live();
    assert!(freed > 0 && freed <= plan, "{name}: clearing freed {freed} bytes, plan lease {plan}");
    members.push(format!("\"cleared\":[{freed},{plan}]"));
    drop(eng);

    // A job on a fresh engine: work, receipt, plan, build before the job exists; bands allocate nothing.
    let ledger = Ledger::new();
    let mut eng = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone())).unwrap();
    ledger.take();
    let base = live();
    let job_request = WideJobRequest {
        source: WideSource::Profile(ctx.input(&ctx.source, &c.source)),
        destination: ctx.input(&ctx.destination, &c.destination),
        bands: WideBands::Sizes(&[100]),
        intent: c.intent,
        black_point_compensation: c.bpc,
        revision: 1,
        expected_revision: 1,
        correlation_id: 7,
        resource_id: &ctx.resource,
        actor: &ctx.actor,
    };
    let (job, prepared) = measure(|| eng.prepare_job(&job_request, &t).unwrap());
    let log = ledger.take();
    assert_eq!(log.len(), 4, "{name}: a job miss reserves work, receipt, plan and build");
    let (work, receipt, job_plan, job_build) = (log[0], log[1], log[2], log[3]);
    check(name, "job_prepare_peak", prepared.peak, work + receipt + job_plan + job_build, &mut members);
    check(name, "job_prepare_kept", prepared.kept, work + receipt + job_plan, &mut members);
    let mut out = vec![0.0f32; 100 * job.job().output_channels()];
    let band_start = live();
    job.convert_band(0, &px, &mut out, &t).unwrap();
    assert_eq!(live(), band_start, "{name}: a band left heap behind");
    let (finished, fin) = measure(|| job.finish(&t, &mut Null).unwrap());
    check(name, "job_finish_peak", fin.peak, work + receipt, &mut members);
    let finish_kept = freed_by_drop(finished);
    check(name, "job_receipt_kept", finish_kept, receipt, &mut members);
    // What the job leaves behind is the cached plan (the caller's own band buffer is not Prism's).
    let after_job = live() - base - (out.len() * std::mem::size_of::<f32>()) as i64;
    check(name, "job_plan_kept", after_job, job_plan, &mut members);
    drop(out);

    let worst = [
        ratio(miss.peak, request + output + plan + build),
        ratio(plan_kept, plan),
        ratio(miss.peak - plan_kept - output_kept, build + request),
        ratio(hit.peak - output_kept, request),
    ]
    .into_iter()
    .fold(0.0f64, f64::max);
    eprintln!("ADMITTED_HEAP {{\"case\":\"{name}\",\"measured_leased\":{{{}}},\"worst_ratio\":{worst:.3}}}", members.join(","));
}

fn synthetic_cases() -> Vec<Case> {
    let rel = Intent::RelativeColorimetric;
    let case = |name: &str, source: Vec<u8>, destination: Vec<u8>, in_ch: usize, intent: Intent, bpc: bool| Case {
        name: name.to_string(),
        source,
        destination,
        in_ch,
        intent,
        bpc,
    };
    let big_trc = rgb_matrix(0x0210_0000, trc_table(MAX_WIDE_TRC_ENTRIES));
    vec![
        case("srgb_v4_para_to_p3_v4_fused", SRGB.to_vec(), P3.to_vec(), 3, rel, false),
        case("matrix_v2_curv_gamma_to_srgb", matrix_v2(1.8), SRGB.to_vec(), 3, rel, false),
        case("trc_tables_65536_both_sides_bpc", big_trc.clone(), rgb_matrix(0x0210_0000, trc_table(MAX_WIDE_TRC_ENTRIES - 1)), 3, rel, true),
        case("mab_rgb_33_to_srgb", mab_rgb_profile(33, |r, g, b| [0.9642 * r, g, 0.8249 * b]), SRGB.to_vec(), 3, rel, false),
        case("cmyk_u16_17_to_srgb_bpc_relative", cmyk_k_profile(17, 3, 20.0), SRGB.to_vec(), 4, rel, true),
        case("srgb_to_cmyk_u16_17_bpc_relative", SRGB.to_vec(), cmyk_k_profile(17, 3, 20.0), 3, rel, true),
        case("srgb_to_cmyk_u16_17_bpc_perceptual", SRGB.to_vec(), cmyk_k_profile(17, 3, 20.0), 3, Intent::Perceptual, true),
        case("cmyk_u8_17_to_srgb_bpc_relative", cmyk_k_profile_u8(17), SRGB.to_vec(), 4, rel, true),
        case("srgb_to_cmyk_u8_17_bpc_perceptual", SRGB.to_vec(), cmyk_k_profile_u8(17), 3, Intent::Perceptual, true),
        case("cmyk_u8_17_to_cmyk_u16_17_bpc_relative", cmyk_k_profile_u8(17), cmyk_k_profile(17, 3, 20.0), 4, rel, true),
    ]
}

#[test]
fn leased_bytes_cover_the_measured_heap_per_lease_kind_for_synthetic_families() {
    if !admitted_supported("synthetic_families") {
        return;
    }
    for case in synthetic_cases() {
        run_case(&case);
    }
}

/// Installed CMYK printer profiles of the oracle corpus (SWOP 0.56 MB, FOGRA39, GRACoL): the realistic size class.
#[test]
fn leased_bytes_cover_the_measured_heap_for_installed_cmyk_profiles() {
    if !admitted_supported("installed_cmyk") {
        return;
    }
    let rel = Intent::RelativeColorimetric;
    let mut ran = 0;
    for (name, file, sha) in [
        ("swop", "USWebCoatedSWOP.icc", None),
        ("fogra39", "CoatedFOGRA39.icc", None),
        ("gracol2006", "CoatedGRACoL2006.icc", None),
    ] {
        match corpus_read(file, sha) {
            Ok(cmyk) => {
                for (label, source, destination, in_ch, intent) in [
                    ("to_srgb_bpc_relative", cmyk.clone(), SRGB.to_vec(), 4, rel),
                    ("to_srgb_bpc_perceptual", cmyk.clone(), SRGB.to_vec(), 4, Intent::Perceptual),
                    ("from_srgb_bpc_relative", SRGB.to_vec(), cmyk.clone(), 3, rel),
                    ("from_srgb_bpc_perceptual", SRGB.to_vec(), cmyk.clone(), 3, Intent::Perceptual),
                ] {
                    run_case(&Case { name: format!("installed_{name}_{label}"), source, destination, in_ch, intent, bpc: true });
                    ran += 1;
                }
            }
            Err(reason) => not_proven("wide_heap", &format!("installed_{name}"), &reason),
        }
    }
    eprintln!("ADMITTED_HEAP_INSTALLED {{\"cases_run\":{ran}}}");
}
