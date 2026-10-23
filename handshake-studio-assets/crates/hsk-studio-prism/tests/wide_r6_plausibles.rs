//! SMB-PRISM-ENG-6, two of the R6 review A PLAUSIBLE items (the third, measured lease sizes, is tests/wide_heap.rs).
//!
//! 1. Destination tone curves that cannot be inverted (`Gamma(g)` with g <= 0, `para` exponents <= 0, constant
//!    `curv` tables) are refused at plan time with `WideError::NonInvertibleCurve`; the same curves stay valid as
//!    sources (forward evaluation is defined). Basis: ICC.1:2022 Annex F.2 / F.3 (constant curve cannot be inverted;
//!    the inverse of a non-invertible curve is undefined) and clause 10.18 (values outside 0..1 are clipped).
//! 2. The absolute colorimetric media-white rule and the registered divergence DIV-ABS-V2-DISPLAY-WTPT-001
//!    (LittleCMS 2 reads D50 for the `wtpt` of an ICC v2 display profile, Prism uses the tag as written) are
//!    recorded in `AbsoluteReport` by the one place that applies absolute colorimetric (`wide_plan::build`), so
//!    every route reports the same record: plain and admitted transform, plain and admitted job, fused and staged.
//!    The flag is true exactly when the two readings give different numbers (checked against a profile pair whose
//!    v2 display `wtpt` is set to D50, i.e. the LittleCMS 2 reading).
mod wide_support;
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_prism::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use wide_support::*;

const REL: Intent = Intent::RelativeColorimetric;
const D65_ADAPTED: [f64; 3] = [0.9505, 1.0, 1.0891];

/// `curv` with explicit u16 table entries.
fn curv_table(entries: &[u16]) -> Vec<u8> {
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend_from_slice(&be32(entries.len() as u32));
    for e in entries {
        t.extend_from_slice(&e.to_be_bytes());
    }
    t
}
/// `para` with a function type (0..=4) and s15Fixed16 parameters in table order.
fn para(function_type: u16, params: &[f64]) -> Vec<u8> {
    let mut t = b"para\0\0\0\0".to_vec();
    t.extend_from_slice(&function_type.to_be_bytes());
    t.extend_from_slice(&[0, 0]);
    for p in params {
        t.extend_from_slice(&s15(*p));
    }
    t
}
/// RGB matrix/TRC profile with one shared TRC blob, the given version, class and media white.
fn rgb(version: u32, class: &[u8; 4], wtpt: [f64; 3], trc: Vec<u8>) -> Vec<u8> {
    let blobs = vec![xyz_tag(wtpt), xyz_tag([0.4360, 0.2225, 0.0139]), xyz_tag([0.3851, 0.7169, 0.0971]), xyz_tag([0.1431, 0.0606, 0.7141]), trc];
    assemble(
        version,
        class,
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
    let _ = writeln!(std::io::stderr(), "NOT_PROVEN: wide_r6_plausibles {case} reason=admission_route_supported_on_x86_64_windows_msvc_only");
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

fn job_request<'a>(ctx: &'a Ctx, source: WideSource<'a>, destination: &'a [u8], intent: Intent, bpc: bool, sizes: &'a [usize]) -> WideJobRequest<'a> {
    WideJobRequest {
        source,
        destination: ctx.input(&ctx.destination, destination),
        bands: WideBands::Sizes(sizes),
        intent,
        black_point_compensation: bpc,
        revision: 1,
        expected_revision: 1,
        correlation_id: 7,
        resource_id: &ctx.resource,
        actor: &ctx.actor,
    }
}

// ------------------------------------------------------------------------------------------------------------
// 1. Destination curves that cannot be inverted
// ------------------------------------------------------------------------------------------------------------
/// (label, TRC blob): each is constant or decreasing once ICC.1:2022 10.6 / 10.18 are applied.
fn non_invertible_curves() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("curv_gamma_0", curv(Some(0.0))),
        ("para0_g_0", para(0, &[0.0])),
        ("para0_g_minus_1", para(0, &[-1.0])),
        ("para0_g_minus_2_4", para(0, &[-2.4])),
        ("para1_g_0", para(1, &[0.0, 1.0, 0.0])),
        ("para2_g_minus_1", para(2, &[-1.0, 1.0, 0.0, 0.0])),
        ("para3_g_0", para(3, &[0.0, 0.9479, 0.0521, 0.0774, 0.04045])),
        ("para4_g_minus_2_4", para(4, &[-2.4, 0.9479, 0.0521, 0.0774, 0.04045, 0.0, 0.0])),
        ("curv_table_constant", curv_table(&[0x8000; 4])),
        ("curv_table_constant_zero", curv_table(&[0; 16])),
    ]
}
/// Controls: invertible curves of every encoding must keep working (the refusal is not blanket).
fn invertible_curves() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("curv_gamma_2_2", curv(Some(2.2))),
        ("curv_gamma_1_8", curv(Some(1.8))),
        ("curv_identity", curv(None)),
        ("para0_g_2_2", para(0, &[2.2])),
        ("para3_srgb", para(3, &[2.4, 0.9479, 0.0521, 0.0774, 0.04045])),
        ("curv_table_rising", curv_table(&[0, 0x2000, 0x8000, 0xffff])),
        ("curv_table_falling", curv_table(&[0xffff, 0x8000, 0x2000, 0])),
    ]
}

#[test]
fn destination_tone_curves_that_cannot_be_inverted_are_refused_at_plan_time() {
    let ctx = Ctx::new();
    let rgb_px = [0.2f32, 0.4, 0.6, 0.9, 0.5, 0.1];
    let lab_px = [60.0f32, 20.0, -30.0, 90.0, 0.0, 10.0];
    for (label, trc) in non_invertible_curves() {
        for version in [0x0210_0000u32, 0x0420_0000] {
            let dst = rgb(version, b"mntr", D50, trc.clone());
            // The profile itself is a valid, admitted profile: only its use as a destination is refused.
            let inspected = engine(&ctx).inspect_profile(ctx.input(&ctx.destination, &dst), &CancellationToken::default());
            assert!(inspected.is_ok(), "{label} v{version:#x}: admission must not refuse it ({inspected:?})");
            for fusion in [true, false] {
                let mut eng = engine(&ctx);
                eng.set_matrix_fusion(fusion);
                for (intent, bpc) in [(REL, false), (REL, true), (Intent::Perceptual, true), (Intent::AbsoluteColorimetric, false)] {
                    let failure = run(&mut eng, &ctx, SRGB, &dst, &rgb_px, intent, bpc).err().unwrap_or_else(|| {
                        panic!("{label} v{version:#x} fusion={fusion} {intent:?} bpc={bpc}: a non-invertible destination converted")
                    });
                    assert_eq!(failure.error, WideError::NonInvertibleCurve, "{label} {intent:?} bpc={bpc}");
                    assert_eq!(failure.error.code(), "non_invertible_curve");
                }
                // Lab source into the same destination.
                let failure = run_lab(&mut eng, &ctx, &dst, &lab_px, REL, false).expect_err("Lab source, non-invertible destination");
                assert_eq!(failure.error, WideError::NonInvertibleCurve, "{label}: Lab source");
                // Refused at plan time: no plan cached, the failure is counted.
                let stats = eng.cache_stats();
                assert_eq!(stats.entries, 0, "{label}: a refused plan was cached");
                assert!(stats.rejected >= 5, "{label}: refusals not counted ({stats:?})");
            }
            // The same bytes as a SOURCE: forward evaluation is defined for every finite exponent.
            let mut eng = engine(&ctx);
            let forward = run(&mut eng, &ctx, &dst, SRGB, &rgb_px, REL, false);
            assert!(forward.is_ok(), "{label} v{version:#x}: forward use must stay defined ({:?})", forward.as_ref().err().map(|f| f.error));
        }
        // Gray destination (kTRC): the same refusal, the same forward use.
        let k = gray(trc.clone());
        let mut eng = engine(&ctx);
        let failure = run(&mut eng, &ctx, SRGB, &k, &rgb_px, REL, false).expect_err("gray destination");
        assert_eq!(failure.error, WideError::NonInvertibleCurve, "{label}: gray destination");
        let forward = run(&mut eng, &ctx, &k, SRGB, &[0.25, 0.75], REL, false);
        assert!(forward.is_ok(), "{label}: gray source ({:?})", forward.as_ref().err().map(|f| f.error));
    }
}

#[test]
fn invertible_destination_curves_of_every_encoding_still_convert() {
    let ctx = Ctx::new();
    let px = [0.2f32, 0.4, 0.6, 0.9, 0.5, 0.1];
    for (label, trc) in invertible_curves() {
        let dst = rgb(0x0420_0000, b"mntr", D50, trc.clone());
        let mut eng = engine(&ctx);
        let out = run(&mut eng, &ctx, SRGB, &dst, &px, REL, false).unwrap_or_else(|f| panic!("{label}: {:?}", f.error));
        assert!(out.pixels.iter().all(|v| v.is_finite()), "{label}");
        let k = gray(trc);
        run(&mut eng, &ctx, SRGB, &k, &px, REL, false).unwrap_or_else(|f| panic!("{label} gray: {:?}", f.error));
    }
}

#[test]
fn non_invertible_destination_is_refused_before_any_charge_is_kept_and_for_jobs() {
    let ctx = Ctx::new();
    let token = CancellationToken::default();
    let dst = rgb(0x0210_0000, b"mntr", D50, curv(Some(0.0)));
    let px = [0.2f32, 0.4, 0.6];
    // Plain job: typed refusal when the job is prepared, nothing cached.
    let mut eng = engine(&ctx);
    let refused = eng.prepare_job(&job_request(&ctx, WideSource::Profile(ctx.input(&ctx.source, SRGB)), &dst, REL, false, &[1]), &token);
    assert_eq!(refused.err(), Some(WideError::NonInvertibleCurve));
    assert_eq!(eng.cache_stats().entries, 0);
    // Admitted transform and job: refused after the charges, which are released again; no plan lease is kept.
    if !admitted_supported("admitted_refusal") {
        return;
    }
    let ledger = Arc::new(Ledger::default());
    let mut admitted = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone())).unwrap();
    let provider = ledger.live.load(SeqCst);
    let failure = admitted.transform(&ctx.request(SRGB, &dst, &px, REL, false), &token, &mut Null).err().expect("refused");
    assert!(matches!(&failure, AdmittedWideError::Wide(f) if f.error == WideError::NonInvertibleCurve), "{failure:?}");
    assert_eq!(failure.code(), "non_invertible_curve");
    assert_eq!((admitted.cache_stats().entries, admitted.plan_reserved_bytes(), ledger.live.load(SeqCst)), (0, 0, provider));
    let failure = admitted
        .prepare_job(&job_request(&ctx, WideSource::Profile(ctx.input(&ctx.source, SRGB)), &dst, REL, false, &[1]), &token)
        .err()
        .expect("refused");
    assert!(matches!(failure, AdmittedWideError::Refused(WideError::NonInvertibleCurve)), "{failure:?}");
    assert_eq!((admitted.cache_stats().entries, admitted.plan_reserved_bytes(), ledger.live.load(SeqCst)), (0, 0, provider));
}

// ------------------------------------------------------------------------------------------------------------
// 2. Absolute colorimetric: media-white rule and the registered v2 display divergence
// ------------------------------------------------------------------------------------------------------------
#[derive(Clone, Copy)]
enum Spec {
    Rgb { version: u32, class: [u8; 4], wtpt: [f64; 3] },
    /// K-ramp CMYK printer (v2, class `prtr`) with an explicit media white.
    Cmyk { white: [f64; 3] },
    Lab,
}
impl Spec {
    const fn rgb(version: u32, class: &[u8; 4], wtpt: [f64; 3]) -> Self {
        Self::Rgb { version, class: *class, wtpt }
    }
    /// Profile bytes as written, or with the LittleCMS 2 reading applied (a v2 display profile's `wtpt` is D50).
    fn bytes(self, lcms2_reading: bool) -> Option<Vec<u8>> {
        match self {
            Self::Rgb { version, class, wtpt } => {
                let v2_display = version >> 24 == 2 && class == *b"mntr";
                Some(rgb(version, &class, if lcms2_reading && v2_display { D50 } else { wtpt }, curv(Some(2.2))))
            }
            Self::Cmyk { white } => {
                let a2b = cmyk_a2b(3, |_, _, _, k| (100.0 - 80.0 * k, 0.0, 0.0));
                let b2a = cmyk_b2a(6, |l, _, _| [0.0, 0.0, 0.0, (100.0 - l) / 80.0]);
                Some(cmyk_profile(a2b, Some(b2a), 3, white))
            }
            Self::Lab => None,
        }
    }
    fn pixels(self) -> Vec<f32> {
        match self {
            Self::Rgb { .. } => vec![0.2, 0.4, 0.6, 0.9, 0.8, 0.1, 0.5, 0.5, 0.5, 1.0, 1.0, 1.0],
            Self::Cmyk { .. } => vec![0.0, 0.0, 0.0, 0.0, 0.2, 0.1, 0.3, 0.4, 0.0, 0.0, 0.0, 1.0, 0.5, 0.5, 0.5, 0.2],
            Self::Lab => vec![60.0, 20.0, -30.0, 90.0, 0.0, 10.0, 50.0, 0.0, 0.0, 100.0, 0.0, 0.0],
        }
    }
}

/// One conversion through one route: its absolute record and its pixels.
struct Routed {
    route: &'static str,
    absolute: AbsoluteReport,
    pixels: Vec<f32>,
}
/// Convert through every route the crate has. The admitted routes are `NOT_PROVEN` (printed, not silent) off
/// x86_64 Windows MSVC.
fn all_routes(source: Spec, destination: Spec, lcms2_reading: bool, intent: Intent, bpc: bool) -> Vec<Routed> {
    let ctx = Ctx::new();
    let token = CancellationToken::default();
    let src_bytes = source.bytes(lcms2_reading);
    let dst_bytes = destination.bytes(lcms2_reading).expect("destination is a profile");
    let px = source.pixels();
    let src_input = || src_bytes.as_ref().map(|b| WideSource::Profile(ctx.input(&ctx.source, b))).unwrap_or(WideSource::Lab);
    let request = || {
        let mut r = ctx.request(src_bytes.as_deref().unwrap_or(SRGB), &dst_bytes, &px, intent, bpc);
        r.source = src_input();
        r
    };
    let n = px.len() / if src_bytes.is_none() { 3 } else { src_bytes.as_ref().map_or(3, |b| channels(b)) };
    let sizes = [n];
    let mut routes = Vec::new();
    for fusion in [true, false] {
        let mut eng = engine(&ctx);
        eng.set_matrix_fusion(fusion);
        let out = eng.transform(&request(), &token, &mut Null).unwrap_or_else(|f| panic!("transform fusion={fusion}: {:?}", f.error));
        routes.push(Routed { route: if fusion { "transform_fused" } else { "transform_staged" }, absolute: out.receipt.absolute, pixels: out.pixels });
    }
    {
        let mut eng = engine(&ctx);
        let job = eng.prepare_job(&job_request(&ctx, src_input(), &dst_bytes, intent, bpc, &sizes), &token).unwrap_or_else(|e| panic!("job: {e:?}"));
        let mut out = vec![0.0f32; n * job.output_channels()];
        job.convert_band(0, &px, &mut out, &token).unwrap();
        let done = job.finish(&token, &mut Null).unwrap_or_else(|f| panic!("job finish: {:?}", f.error));
        routes.push(Routed { route: "job", absolute: done.receipt.receipt.absolute, pixels: out });
    }
    if admitted_supported("absolute_routes") {
        let ledger = Arc::new(Ledger::default());
        let mut eng = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone())).unwrap();
        let out = eng.transform(&request(), &token, &mut Null).unwrap_or_else(|e| panic!("admitted transform: {e:?}"));
        routes.push(Routed { route: "admitted_transform", absolute: out.result().receipt.absolute, pixels: out.result().pixels.clone() });
        drop(out);
        let mut eng = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger)).unwrap();
        let job = eng.prepare_job(&job_request(&ctx, src_input(), &dst_bytes, intent, bpc, &sizes), &token).unwrap_or_else(|e| panic!("admitted job: {e:?}"));
        let mut out = vec![0.0f32; n * job.job().output_channels()];
        job.convert_band(0, &px, &mut out, &token).unwrap();
        let done = job.finish(&token, &mut Null).unwrap_or_else(|e| panic!("admitted job finish: {e:?}"));
        routes.push(Routed { route: "admitted_job", absolute: done.result().receipt.receipt.absolute, pixels: out });
    }
    routes
}
fn channels(profile: &[u8]) -> usize {
    match &profile[16..20] {
        b"GRAY" => 1,
        b"CMYK" => 4,
        _ => 3,
    }
}
fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn absolute_receipt_names_the_media_white_rule_and_the_v2_display_divergence_on_every_route() {
    let v2_d65 = Spec::rgb(0x0210_0000, b"mntr", D65_ADAPTED);
    let v2_d50 = Spec::rgb(0x0210_0000, b"mntr", D50);
    let v4_d50 = Spec::rgb(0x0420_0000, b"mntr", D50);
    let v4_d65 = Spec::rgb(0x0420_0000, b"mntr", D65_ADAPTED);
    let v2_scanner_d65 = Spec::rgb(0x0210_0000, b"scnr", D65_ADAPTED);
    let paper = Spec::Cmyk { white: [0.90, 0.93, 0.70] };
    // (label, source, destination, the two readings differ for this pair)
    let cases: [(&str, Spec, Spec, bool); 10] = [
        ("v4_d50_to_v2_display_d65", v4_d50, v2_d65, true),
        ("v2_display_d65_to_v4_d50", v2_d65, v4_d50, true),
        ("v2_display_d65_to_v2_display_d65", v2_d65, v2_d65, false), // as written 1, LittleCMS 2 reading 1: same numbers
        ("lab_to_v2_display_d65", Spec::Lab, v2_d65, true),
        ("lab_to_v2_display_d50", Spec::Lab, v2_d50, false),
        ("v4_d50_to_v4_display_d65", v4_d50, v4_d65, false), // v4: both readings use the tag
        ("v2_scanner_d65_to_v4_d50", v2_scanner_d65, v4_d50, false), // not a display class: both readings use the tag
        ("v2_display_d65_to_v2_scanner_d65", v2_d65, v2_scanner_d65, true), // as written 1, LittleCMS 2 reading D50/D65
        ("cmyk_paper_to_v4_d50", paper, v4_d50, false),
        ("cmyk_paper_to_v2_display_d65", paper, v2_d65, true),
    ];
    for (label, source, destination, differ) in cases {
        let routes = all_routes(source, destination, false, Intent::AbsoluteColorimetric, false);
        let reading = all_routes(source, destination, true, Intent::AbsoluteColorimetric, false);
        let first = &routes[0];
        for r in &routes {
            assert_eq!(r.absolute, first.absolute, "{label}: route {} reports a different absolute record than {}", r.route, first.route);
            // Fused and staged evaluation agree up to f64 rounding (tests/wide_fusion.rs: <= 2e-6); other routes are bit-identical.
            assert!(max_abs_diff(&r.pixels, &first.pixels) < 1e-5, "{label}: route {} converted differently from {}", r.route, first.route);
        }
        let a = first.absolute;
        assert!(a.applied, "{label}");
        assert_eq!(a.media_white_rule, MEDIA_WHITE_RULE_WTPT_AS_WRITTEN, "{label}");
        assert_eq!(a.divergence, differ.then_some(ABS_V2_DISPLAY_WTPT_DIVERGENCE), "{label}: divergence flag");
        // The flag is exactly "the LittleCMS 2 reading gives other numbers": same plan, wtpt of v2 display profiles = D50.
        let lcms = max_abs_diff(&first.pixels, &reading[0].pixels);
        if differ {
            assert!(lcms > 1e-3, "{label}: flagged, but both readings agree (max diff {lcms})");
        } else {
            assert!(lcms < 1e-4, "{label}: not flagged, but the readings differ by {lcms}");
            assert!(reading.iter().all(|r| r.absolute.divergence.is_none()), "{label}: the LittleCMS 2 reading flags itself");
        }
        // The record states the numbers the rule produced.
        let (sw, dw) = (a.source_media_white.unwrap(), a.destination_media_white.unwrap());
        for i in 0..3 {
            assert!((a.scale.unwrap()[i] - sw[i] / dw[i]).abs() < 1e-12, "{label}: scale is source wtpt / destination wtpt");
        }
        let alt = reading[0].absolute;
        eprintln!(
            "ABS_DIVERGENCE {{\"case\":\"{label}\",\"routes\":{},\"as_written_scale\":{:?},\"lcms2_reading_scale\":{:?},\"max_pixel_diff\":{lcms:.5},\"flagged\":{}}}",
            routes.len(),
            a.scale,
            alt.scale,
            a.divergence.is_some()
        );
    }
}

#[test]
fn absolute_record_is_empty_for_every_other_intent_and_bpc_is_ignored_for_absolute() {
    let v2_d65 = Spec::rgb(0x0210_0000, b"mntr", D65_ADAPTED);
    let v4_d50 = Spec::rgb(0x0420_0000, b"mntr", D50);
    for intent in [REL, Intent::Perceptual, Intent::Saturation] {
        for bpc in [false, true] {
            for r in all_routes(v4_d50, v2_d65, false, intent, bpc) {
                assert_eq!(r.absolute, AbsoluteReport::default(), "{} {intent:?} bpc={bpc}: absolute record without absolute intent", r.route);
                assert_eq!((r.absolute.media_white_rule, r.absolute.divergence), ("", None));
            }
        }
    }
    // Absolute colorimetric ignores BPC: same record, same bits.
    let off = all_routes(v4_d50, v2_d65, false, Intent::AbsoluteColorimetric, false);
    let on = all_routes(v4_d50, v2_d65, false, Intent::AbsoluteColorimetric, true);
    assert_eq!(off[0].absolute, on[0].absolute);
    assert_eq!(max_abs_diff(&off[0].pixels, &on[0].pixels), 0.0);
}

#[test]
fn descriptor_reports_profile_class_and_v2_display_membership() {
    let ctx = Ctx::new();
    let token = CancellationToken::default();
    let eng = engine(&ctx);
    for (spec, class, v2_display) in [
        (Spec::rgb(0x0210_0000, b"mntr", D65_ADAPTED), *b"mntr", true),
        (Spec::rgb(0x0420_0000, b"mntr", D50), *b"mntr", false),
        (Spec::rgb(0x0210_0000, b"scnr", D50), *b"scnr", false),
        (Spec::Cmyk { white: D50 }, *b"prtr", false),
    ] {
        let bytes = spec.bytes(false).unwrap();
        let d = eng.inspect_profile(ctx.input(&ctx.source, &bytes), &token).unwrap();
        assert_eq!((d.profile_class, d.is_v2_display()), (class, v2_display));
    }
}
