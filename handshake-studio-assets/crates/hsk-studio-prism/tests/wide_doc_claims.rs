//! SMB-PRISM-ENG-6, R7 pre-review fixes 1 and 3 (V4-06 class: a doc or WIDE_DESCRIPTOR claim stronger than the code).
//!
//! * `recovery` used to say "canceled: nothing was produced" although a canceled job band leaves its converted
//!   chunks in the caller's buffer (the per-edge behaviour is proven by src/wide_job/tests.rs); the descriptor now
//!   scopes it and this file pins the text.
//! * `privacy` used to say that "ids never enter telemetry" although the Observe wire v1 frame carries the caller's
//!   resource and actor attribution ids; the frame is decoded here: attribution ids present, profile ids, profile
//!   bytes and pixel values absent.
//! * The module docs of the admitted route say a refusal found before the engine ran delivers nothing; counted here
//!   with a sink that records every frame (admission denial and a framing refusal).
mod wide_support;
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_prism::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use wide_support::*;

const P3: &[u8] = include_bytes!("profiles/DisplayP3-v4.icc");
const REL: Intent = Intent::RelativeColorimetric;

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn descriptor_scopes_what_a_canceled_call_leaves_and_what_telemetry_carries() {
    let d = WIDE_DESCRIPTOR;
    assert!(!d.contains("canceled: nothing was produced"), "the unscoped cancel claim is back");
    for needle in [
        "canceled: a request (transform, plain or admitted) and finish return no result",
        "a canceled job band keeps the chunks converted before the cancel edge in the caller buffer",
        "an in-place band is converted prefix + untouched rest",
        "the band stays claimed (band_claimed)",
        "channel_range/nonfinite_channel on a band write nothing",
        "nonfinite_output leaves the chunks up to and including the failing one",
    ] {
        assert!(d.contains(needle), "WIDE_DESCRIPTOR recovery lost: {needle}");
    }
    assert!(!d.contains("profile bytes, pixels and ids never enter telemetry"), "the unscoped privacy claim is back");
    assert!(d.contains("caller resource and actor attribution ids"), "privacy must name the ids the frame does carry");
    assert!(!d.contains("its only Prism-owned memory is a stack staging buffer of scratch_bytes"), "the staging claim is back");
    assert!(d.contains("fixed 1024-byte stack array"), "chunking must name the job-band stage");
}

#[test]
fn wide_telemetry_frame_carries_attribution_ids_but_no_profile_or_pixel_data() {
    let ctx = Ctx::new();
    let pixels = [0.123456f32, 0.654321, 0.246813, 0.97531, 0.11111, 0.86420];
    let mut eng = engine(&ctx);
    let mut sink = Collect(Vec::new());
    eng.transform(&ctx.request(SRGB, P3, &pixels, REL, false), &CancellationToken::default(), &mut sink)
        .unwrap_or_else(|f| panic!("{:?}", f.error));
    assert_eq!(sink.0.len(), 1, "one terminal frame per request");
    let frame = &sink.0[0];
    assert!(frame.len() <= 2048, "Observe frame limit");
    assert_eq!(&frame[..4], b"HSKO");
    // Present: the caller's attribution ids (Observe wire v1 header strings).
    for id in ["SDOC-019abcde-0000-7000-8000-000000000003", "account", "principal", "owner-account", "owner-principal", "space", "session"] {
        assert!(contains(frame, id.as_bytes()), "frame lacks the attribution id {id}");
    }
    // Absent: profile ids, profile bytes, pixel values.
    for id in ["SCPF-019abcde-0000-7000-8000-000000000001", "SCPF-019abcde-0000-7000-8000-000000000002"] {
        assert!(!contains(frame, id.as_bytes()), "frame carries the profile id {id}");
    }
    assert!(!contains(frame, &SRGB[..128]) && !contains(frame, &P3[..128]), "frame carries profile bytes");
    for v in pixels {
        assert!(!contains(frame, &v.to_le_bytes()) && !contains(frame, &v.to_be_bytes()), "frame carries the pixel value {v}");
    }
}

struct Counting {
    frames: usize,
}
impl SinkPort for Counting {
    fn try_send(&mut self, _class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        self.frames += 1;
        Ok(())
    }
}
struct CappedLedger {
    live: AtomicU64,
    cap: u64,
}
#[derive(Clone)]
struct Capped(Arc<CappedLedger>);
struct CappedLease(Arc<CappedLedger>, u64);
impl Drop for CappedLease {
    fn drop(&mut self) {
        self.0.live.fetch_sub(self.1, SeqCst);
    }
}
impl ProviderReservation for CappedLease {
    fn reserved_bytes(&self) -> u64 {
        self.1
    }
}
impl ProviderAdmission for Capped {
    type Reservation = CappedLease;
    fn synchronous_retirement(&self) -> bool {
        true
    }
    fn reserve(&self, bytes: u64) -> Result<CappedLease, Error> {
        let live = self.0.live.load(SeqCst);
        if live.checked_add(bytes).is_none_or(|t| t > self.0.cap) {
            return Err(Error::AdmissionDenied);
        }
        self.0.live.fetch_add(bytes, SeqCst);
        Ok(CappedLease(self.0.clone(), bytes))
    }
}

#[test]
fn admitted_refusals_found_before_the_engine_ran_deliver_nothing() {
    if !cfg!(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc")) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "NOT_PROVEN: wide_doc_claims admitted_refusals reason=admission_route_x86_64_windows_msvc_only");
        return;
    }
    let ctx = Ctx::new();
    let token = CancellationToken::default();
    let cmyk = cmyk_k_profile(9, 3, 20.0);
    let pixels = vec![0.25f32; 4 * 64];
    let probe = AdmittedWideEngine::new(&ctx.actor, 2, Capped(Arc::new(CappedLedger { live: AtomicU64::new(0), cap: u64::MAX }))).unwrap();
    let provider = probe.provider_reserved_bytes();
    drop(probe);
    let ledger = Arc::new(CappedLedger { live: AtomicU64::new(0), cap: provider + 64 * 1024 });
    let mut eng = AdmittedWideEngine::new(&ctx.actor, 2, Capped(ledger.clone())).unwrap();
    let mut sink = Counting { frames: 0 };
    // Admission denial: the plan bound of a CMYK + sRGB pair exceeds the cap.
    let denied = eng.transform(&ctx.request(&cmyk, SRGB, &pixels, REL, true), &token, &mut sink).err().expect("over budget");
    assert!(matches!(denied, AdmittedWideError::Admission(Error::AdmissionDenied)), "{denied:?}");
    assert_eq!(sink.frames, 0, "an admission denial delivered a frame");
    // Framing refusal: a truncated destination profile (identity and hash proven for the truncated bytes) is
    // refused from the tag table before any parse.
    let truncated = &SRGB[..100];
    let refused = eng.transform(&ctx.request(SRGB, truncated, &[0.1f32, 0.2, 0.3], REL, false), &token, &mut sink).err().expect("truncated profile");
    assert!(matches!(refused, AdmittedWideError::Refused(_)), "{refused:?}");
    assert_eq!(sink.frames, 0, "a framing refusal delivered a frame");
    assert_eq!((eng.cache_stats().parses, eng.cache_stats().entries), (0, 0));
    assert_eq!(ledger.live.load(SeqCst), provider);
}
