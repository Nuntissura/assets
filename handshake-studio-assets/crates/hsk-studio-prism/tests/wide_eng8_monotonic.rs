//! SMB-PRISM-ENG-8: monotonicity of DESTINATION `curv` tables (R6 PLAUSIBLE residual of SMB-PRISM-ENG-6).
//!
//! Decision, ICC.1:2022 Annex F (normative; printed pages 101-102): F.1 calls a one-dimensional non-constant curve
//! invertible if it is monotonically increasing or monotonically decreasing; F.2 (grayTRC) and F.3
//! (redTRC/greenTRC/blueTRC) say the inverse of a curve that is not invertible is undefined. Clause 10.6 (curveType)
//! does not require a table to be monotonic, so a non-monotonic table is a valid profile whose forward use is
//! defined and whose use as a destination is refused at plan time with `WideError::NonInvertibleCurve`, exactly
//! like the constant tables of SMB-PRISM-ENG-6. No "envelope" inverse is invented: the standard documents none, and
//! LittleCMS 2 `cmsReverseToneCurveEx` does not refuse but takes the first interval its scan finds.
//!
//! Monotonic tables are inverted per F.1: increasing, decreasing and tables with flat runs (plateaus), where a run
//! [x1, x2] inverts to x2, or to x1 when the run reaches 1 (a), and a value outside the range of the curve takes the
//! inverse of the closest value in the range (b). The expected values below come from `spec_inverse`, a scan of all
//! segments written independently of the engine's bisection. RGB and Gray destinations, across the plain, admitted
//! and job routes.
mod wide_support;
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_prism::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use wide_support::*;

const REL: Intent = Intent::RelativeColorimetric;

/// `curv` with explicit u16 table entries.
fn curv_table(entries: &[u16]) -> Vec<u8> {
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend_from_slice(&be32(entries.len() as u32));
    for e in entries {
        t.extend_from_slice(&e.to_be_bytes());
    }
    t
}
/// RGB matrix/TRC profile with one shared TRC blob (the colorants sum to D50, so Lab neutrals give R = G = B).
fn rgb(version: u32, trc: Vec<u8>) -> Vec<u8> {
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
fn gray(trc: Vec<u8>) -> Vec<u8> {
    assemble(0x0210_0000, b"mntr", b"GRAY", b"XYZ ", &[xyz_tag(D50), trc], &[(b"wtpt", 0), (b"kTRC", 1)])
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
    let _ = writeln!(std::io::stderr(), "NOT_PROVEN: wide_eng8_monotonic {case} reason=admission_route_supported_on_x86_64_windows_msvc_only");
    false
}
#[derive(Default)]
struct Ledger {
    live: AtomicU64,
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
        self.0.live.fetch_add(bytes, SeqCst);
        Ok(Lease(self.0.clone(), bytes))
    }
}
fn job_request<'a>(ctx: &'a Ctx, destination: &'a [u8], pixels: usize) -> WideJobRequest<'a> {
    WideJobRequest {
        source: WideSource::Lab,
        destination: ctx.input(&ctx.destination, destination),
        bands: WideBands::Uniform { pixels, band_pixels: pixels },
        intent: REL,
        black_point_compensation: false,
        revision: 1,
        expected_revision: 1,
        correlation_id: 7,
        resource_id: &ctx.resource,
        actor: &ctx.actor,
    }
}
fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

// ------------------------------------------------------------------------------------------------------------
// Tables (five entries: x = 0, 0.25, 0.5, 0.75, 1, so every x below is exact in binary)
// ------------------------------------------------------------------------------------------------------------
/// Not monotonic and not constant: refused as destinations.
const NON_MONOTONIC: [(&str, [u16; 5]); 6] = [
    ("dip", [0, 0x6000, 0x4000, 0xa000, 0xffff]),
    ("bump_on_a_falling_table", [0xffff, 0x4000, 0x6000, 0x2000, 0]),
    ("late_reversal", [0, 0x4000, 0x8000, 0xffff, 0xfff0]),
    ("early_reversal", [0x0100, 0, 0x4000, 0x8000, 0xffff]),
    ("one_count_against_the_direction", [0, 0x4000, 0x3fff, 0x8000, 0xffff]),
    ("saw", [0, 0xffff, 0, 0xffff, 0]),
];
/// Monotonic (weakly): inverted per F.1.
const MONOTONIC: [(&str, [u16; 5]); 8] = [
    ("increasing", [0, 0x2000, 0x8000, 0xe000, 0xffff]),
    ("decreasing", [0xffff, 0xe000, 0x8000, 0x2000, 0]),
    ("start_plateau", [0x2000, 0x2000, 0x4000, 0x8000, 0xffff]),
    ("middle_plateau", [0, 0x4000, 0x4000, 0x4000, 0xffff]),
    ("end_plateau", [0, 0x4000, 0x8000, 0xc000, 0xc000]),
    ("decreasing_start_plateau", [0xffff, 0xffff, 0x8000, 0x4000, 0]),
    ("decreasing_middle_plateau", [0xffff, 0x8000, 0x8000, 0x8000, 0]),
    ("decreasing_end_plateau", [0xffff, 0x8000, 0x4000, 0x2000, 0x2000]),
];

/// ICC.1:2022 F.1 for a monotonic piecewise-linear table with entries at i / (n - 1), by scanning every segment:
/// every x with f(x) = y is collected; a flat run gives an interval [x1, x2], inverted to x2, or to x1 when x2 = 1
/// (a); a y outside the range of the curve is replaced by the closest y in the range first (b).
fn spec_inverse(t: &[f64], y: f64) -> f64 {
    let last = t.len() - 1;
    let step = 1.0 / last as f64;
    let lo = t.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = t.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let y = y.clamp(lo, hi);
    let (mut x1, mut x2) = (f64::INFINITY, f64::NEG_INFINITY);
    for i in 0..last {
        let (a, b) = (t[i], t[i + 1]);
        let x0 = i as f64 * step;
        if a == b {
            if a == y {
                x1 = x1.min(x0);
                x2 = x2.max(x0 + step);
            }
        } else if a.min(b) <= y && y <= a.max(b) {
            let x = x0 + (y - a) / (b - a) * step;
            x1 = x1.min(x);
            x2 = x2.max(x);
        }
    }
    assert!(x1.is_finite(), "no solution: the table is not monotonic");
    if x1 == x2 || x2 != 1.0 { x2 } else { x1 }
}

/// Neutral Lab points L* = 0..100 (a = b = 0).
const LABS: [f32; 11] = [0.0, 5.0, 12.0, 20.0, 35.0, 50.0, 65.0, 80.0, 92.0, 97.0, 100.0];

// ------------------------------------------------------------------------------------------------------------
// 1. Non-monotonic destination tables are refused (same typed error as constant curves)
// ------------------------------------------------------------------------------------------------------------
#[test]
fn non_monotonic_destination_tables_are_refused_at_plan_time_on_rgb_and_gray() {
    let ctx = Ctx::new();
    let rgb_px = [0.2f32, 0.4, 0.6, 0.9, 0.5, 0.1];
    let lab_px = [60.0f32, 20.0, -30.0, 90.0, 0.0, 10.0];
    for (label, entries) in NON_MONOTONIC {
        let trc = curv_table(&entries);
        for version in [0x0210_0000u32, 0x0420_0000] {
            let dst = rgb(version, trc.clone());
            // A valid, admitted profile: ICC.1:2022 10.6 does not require monotonic tables; only the inverse is undefined.
            let inspected = engine(&ctx).inspect_profile(ctx.input(&ctx.destination, &dst), &CancellationToken::default());
            assert!(inspected.is_ok(), "{label} v{version:#x}: admission must not refuse it ({inspected:?})");
            for fusion in [true, false] {
                let mut eng = engine(&ctx);
                eng.set_matrix_fusion(fusion);
                for (intent, bpc) in [(REL, false), (REL, true), (Intent::Perceptual, true), (Intent::AbsoluteColorimetric, false)] {
                    let failure = run(&mut eng, &ctx, SRGB, &dst, &rgb_px, intent, bpc)
                        .err()
                        .unwrap_or_else(|| panic!("{label} v{version:#x} fusion={fusion} {intent:?} bpc={bpc}: a non-monotonic destination converted"));
                    assert_eq!(failure.error, WideError::NonInvertibleCurve, "{label} {intent:?} bpc={bpc}");
                    assert_eq!(failure.error.code(), "non_invertible_curve");
                }
                let failure = run_lab(&mut eng, &ctx, &dst, &lab_px, REL, false).expect_err("Lab source, non-monotonic destination");
                assert_eq!(failure.error, WideError::NonInvertibleCurve, "{label}: Lab source");
                let stats = eng.cache_stats();
                assert_eq!(stats.entries, 0, "{label}: a refused plan was cached");
                assert!(stats.rejected >= 5, "{label}: refusals not counted ({stats:?})");
            }
            // The same bytes as a SOURCE: the forward curve is defined for any table.
            let mut eng = engine(&ctx);
            let forward = run(&mut eng, &ctx, &dst, SRGB, &rgb_px, REL, false);
            assert!(forward.is_ok(), "{label} v{version:#x}: forward use must stay defined ({:?})", forward.as_ref().err().map(|f| f.error));
        }
        let k = gray(trc.clone());
        let mut eng = engine(&ctx);
        let failure = run(&mut eng, &ctx, SRGB, &k, &rgb_px, REL, false).expect_err("gray destination");
        assert_eq!(failure.error, WideError::NonInvertibleCurve, "{label}: gray destination");
        let forward = run(&mut eng, &ctx, &k, SRGB, &[0.25, 0.75], REL, false);
        assert!(forward.is_ok(), "{label}: gray source ({:?})", forward.as_ref().err().map(|f| f.error));
    }
}

#[test]
fn non_monotonic_destination_is_refused_on_the_job_and_admitted_routes_before_any_charge_is_kept() {
    let ctx = Ctx::new();
    let token = CancellationToken::default();
    let px = [60.0f32, 0.0, 0.0];
    for (label, entries) in [NON_MONOTONIC[0], NON_MONOTONIC[3]] {
        for dst in [rgb(0x0420_0000, curv_table(&entries)), gray(curv_table(&entries))] {
            // Plain job: typed refusal when the job is prepared, nothing cached.
            let mut eng = engine(&ctx);
            let refused = eng.prepare_job(&job_request(&ctx, &dst, 1), &token);
            assert_eq!(refused.err(), Some(WideError::NonInvertibleCurve), "{label}: plain job");
            assert_eq!(eng.cache_stats().entries, 0, "{label}");
            // Admitted transform and job: refused after the charges, which are released again.
            if !admitted_supported("admitted_refusal") {
                continue;
            }
            let ledger = Arc::new(Ledger::default());
            let mut admitted = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone())).unwrap();
            let provider = ledger.live.load(SeqCst);
            let mut request = ctx.request(SRGB, &dst, &px, REL, false);
            request.source = WideSource::Lab;
            let failure = admitted.transform(&request, &token, &mut Null).err().expect("refused");
            assert!(matches!(&failure, AdmittedWideError::Wide(f) if f.error == WideError::NonInvertibleCurve), "{label}: {failure:?}");
            assert_eq!(failure.code(), "non_invertible_curve");
            assert_eq!((admitted.cache_stats().entries, admitted.plan_reserved_bytes(), ledger.live.load(SeqCst)), (0, 0, provider), "{label}");
            let failure = admitted.prepare_job(&job_request(&ctx, &dst, 1), &token).err().expect("refused");
            assert!(matches!(failure, AdmittedWideError::Refused(WideError::NonInvertibleCurve)), "{label}: {failure:?}");
            assert_eq!((admitted.cache_stats().entries, admitted.plan_reserved_bytes(), ledger.live.load(SeqCst)), (0, 0, provider), "{label}");
        }
    }
}

// ------------------------------------------------------------------------------------------------------------
// 2. Monotonic tables (increasing, decreasing, plateaus) are inverted per F.1
// ------------------------------------------------------------------------------------------------------------
fn labs_flat() -> Vec<f32> {
    LABS.iter().flat_map(|l| [*l, 0.0, 0.0]).collect()
}
/// The L* values whose Y is at least 5e-3 away from every table entry (a Y on a breakpoint is a unit-test matter,
/// tests in src/icc_eval.rs): the engine and the reference then agree well inside the f32 output tolerance.
fn comparable(table: &[f64], l: f32) -> Option<f64> {
    let y = y_of_l(f64::from(l));
    table.iter().all(|t| (t - y).abs() >= 5e-3).then_some(y)
}

#[test]
fn monotonic_destination_tables_are_inverted_per_annex_f1_on_gray_and_rgb() {
    let ctx = Ctx::new();
    let labs = labs_flat();
    for (label, entries) in MONOTONIC {
        let table: Vec<f64> = entries.iter().map(|e| f64::from(*e) / 65535.0).collect();
        let trc = curv_table(&entries);
        let mut eng = engine(&ctx);
        // Gray destination (kTRC): device = kTRC^-1(Y of the Lab neutral).
        let k = gray(trc.clone());
        let out = run_lab(&mut eng, &ctx, &k, &labs, REL, false).unwrap_or_else(|f| panic!("{label}: gray {:?}", f.error)).pixels;
        assert_eq!(out.len(), LABS.len());
        let mut compared = 0;
        for (i, l) in LABS.iter().enumerate() {
            let Some(y) = comparable(&table, *l) else { continue };
            let want = spec_inverse(&table, y);
            assert!((f64::from(out[i]) - want).abs() < 1e-5, "{label} gray L*={l}: Y={y:.6} got {} want {want}", out[i]);
            compared += 1;
        }
        assert!(compared >= 6, "{label}: only {compared} comparable points");
        // RGB destination (rTRC = gTRC = bTRC): a neutral gives the same device value in all three channels.
        for version in [0x0210_0000u32, 0x0420_0000] {
            let dst = rgb(version, trc.clone());
            let out = run_lab(&mut eng, &ctx, &dst, &labs, REL, false).unwrap_or_else(|f| panic!("{label}: rgb {:?}", f.error)).pixels;
            assert_eq!(out.len(), LABS.len() * 3);
            for (i, l) in LABS.iter().enumerate() {
                let Some(y) = comparable(&table, *l) else { continue };
                let want = spec_inverse(&table, y);
                for c in 0..3 {
                    // The helper's blue colorant row sums to 0.8251 against a D50 Z of 0.8249: 2e-4 of linear light.
                    assert!((f64::from(out[i * 3 + c]) - want).abs() < 4e-3, "{label} rgb v{version:#x} L*={l} ch{c}: Y={y:.6} got {} want {want}", out[i * 3 + c]);
                }
            }
        }
    }
}

/// The plateau rules of F.1 a) and b), end to end: an end plateau inverts to the START of the run for every Y at or
/// above it (white maps below device 1), a start plateau to the END of the run for every Y at or below it.
#[test]
fn flat_runs_at_the_ends_follow_f1_a_and_b() {
    let ctx = Ctx::new();
    let mut eng = engine(&ctx);
    // Increasing table whose last two entries are equal: values above 0.75 (Y of white is 1) invert to x1 = 0.75.
    let end = gray(curv_table(&[0, 0x4000, 0x8000, 0xc000, 0xc000]));
    let out = run_lab(&mut eng, &ctx, &end, &[100.0, 0.0, 0.0, 97.0, 0.0, 0.0], REL, false).unwrap().pixels;
    assert!((out[0] - 0.75).abs() < 1e-6 && (out[1] - 0.75).abs() < 1e-6, "end plateau: {out:?}");
    // Increasing table whose first two entries are equal: values below 0.125 (black is 0) invert to x2 = 0.25.
    let start = gray(curv_table(&[0x2000, 0x2000, 0x4000, 0x8000, 0xffff]));
    let out = run_lab(&mut eng, &ctx, &start, &[0.0, 0.0, 0.0, 3.0, 0.0, 0.0], REL, false).unwrap().pixels;
    assert!((out[0] - 0.25).abs() < 1e-6 && (out[1] - 0.25).abs() < 1e-6, "start plateau: {out:?}");
    // Decreasing table whose last entries are equal (0x2000): black (Y = 0) is below the range, the closest value in the
    // range is the plateau, and the run reaches 1, so the inverse is its start 0.75 (not 1).
    let dend = gray(curv_table(&[0xffff, 0x8000, 0x4000, 0x2000, 0x2000]));
    let out = run_lab(&mut eng, &ctx, &dend, &[0.0, 0.0, 0.0], REL, false).unwrap().pixels;
    assert!((out[0] - 0.75).abs() < 1e-6, "decreasing end plateau: {out:?}");
}

// ------------------------------------------------------------------------------------------------------------
// 3. The plain, admitted and job routes agree bit for bit on accepted tables
// ------------------------------------------------------------------------------------------------------------
#[test]
fn accepted_tables_give_the_same_bits_on_the_plain_admitted_and_job_routes() {
    let ctx = Ctx::new();
    let token = CancellationToken::default();
    let labs = labs_flat();
    for (label, entries) in MONOTONIC {
        for dst in [gray(curv_table(&entries)), rgb(0x0420_0000, curv_table(&entries))] {
            let mut eng = engine(&ctx);
            let plain = run_lab(&mut eng, &ctx, &dst, &labs, REL, false).unwrap_or_else(|f| panic!("{label}: {:?}", f.error)).pixels;
            // Job route: one band over the same pixels.
            let job = eng.prepare_job(&job_request(&ctx, &dst, LABS.len()), &token).unwrap();
            let mut out = vec![0.0f32; plain.len()];
            job.convert_band(0, &labs, &mut out, &token).unwrap();
            job.finish(&token, &mut Null).unwrap();
            assert_eq!(bits(&out), bits(&plain), "{label}: job vs plain");
            // Admitted route.
            if admitted_supported("admitted_bits") {
                let ledger = Arc::new(Ledger::default());
                let mut admitted = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger)).unwrap();
                let mut request = ctx.request(SRGB, &dst, &labs, REL, false);
                request.source = WideSource::Lab;
                let result = admitted.transform(&request, &token, &mut Null).unwrap_or_else(|e| panic!("{label}: {e:?}"));
                assert_eq!(bits(&result.result().pixels), bits(&plain), "{label}: admitted vs plain");
            }
        }
    }
}

#[test]
fn descriptor_states_the_monotonic_rule_and_the_flat_run_rule() {
    let d = WIDE_DESCRIPTOR;
    for needle in [
        "a curv table that is not monotonic: non_invertible_curve, ICC.1:2022 Annex F.1-F.3",
        "F.1 invertible only if monotonic, F.2/F.3 inverse of a non-invertible curve undefined",
        "a run [x1, x2] inverts to x2, or to x1 when the run reaches 1",
        "LittleCMS 2 cmsReverseToneCurveEx does not refuse a non-monotonic table, Prism does on purpose",
        "a curv table that is not monotonic, choose another destination",
    ] {
        assert!(d.contains(needle), "WIDE_DESCRIPTOR lost: {needle}");
    }
    assert!(!d.contains("(gamma <= 0 or a constant table) is refused"), "the constant-only claim is back");
}
