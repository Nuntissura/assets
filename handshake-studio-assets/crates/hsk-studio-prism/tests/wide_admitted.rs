//! SMB-PRISM-ENG-6 (lead decision D13 option b): `AdmittedWideEngine` charges every allocation of the wide
//! path to the caller's admission ledger before it happens and keeps each reservation exactly as long as
//! the allocation (plan lease with the cached plan, output lease with the result). Pixels are bit-identical
//! to the unadmitted `WideEngine`.
mod wide_support;
use hsk_studio_accord::CancellationToken;
use hsk_studio_prism::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use wide_support::*;

#[derive(Default)]
struct Ledger {
    live: AtomicU64,
    calls: AtomicUsize,
    deny_call: AtomicUsize,
    refuse_retire: AtomicBool,
}
#[derive(Clone)]
struct Admission(Arc<Ledger>, bool);
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
        self.1
    }
    fn reserve(&self, bytes: u64) -> Result<Lease, Error> {
        let call = self.0.calls.fetch_add(1, SeqCst) + 1;
        if self.0.deny_call.load(SeqCst) == call {
            return Err(Error::AdmissionDenied);
        }
        self.0.live.fetch_add(bytes, SeqCst);
        Ok(Lease(self.0.clone(), bytes))
    }
}

#[test]
fn admitted_wide_engine_charges_plans_outputs_and_requests_before_allocating() {
    if !cfg!(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc")) {
        eprintln!("NOT_PROVEN: wide_admitted target reason=admission_route_supported_on_x86_64_windows_msvc_only");
        return;
    }
    let ctx = Ctx::new();
    let ledger = Arc::new(Ledger::default());
    let cmyk = cmyk_k_profile(9, 3, 20.0);
    let pixels: Vec<f32> = (0..400).map(|i| (i % 17) as f32 / 16.0).collect();
    let run_admitted = |eng: &mut AdmittedWideEngine<Admission>, bpc: bool| {
        eng.transform(&ctx.request(&cmyk, SRGB, &pixels, Intent::RelativeColorimetric, bpc), &CancellationToken::default(), &mut Collect(Vec::new()))
    };
    // Synchronous retirement is required before anything is charged.
    assert!(matches!(AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone(), false)), Err(Error::UnsupportedAdmission)));
    let mut eng = AdmittedWideEngine::new(&ctx.actor, 4, Admission(ledger.clone(), true)).unwrap();
    let provider = eng.provider_reserved_bytes();
    assert!(provider > 0 && ledger.live.load(SeqCst) == provider);
    // A denied plan reservation (4th reservation of the call: request, output, plan) parses nothing.
    let calls = ledger.calls.load(SeqCst);
    ledger.deny_call.store(calls + 3, SeqCst);
    let denied = run_admitted(&mut eng, true).err().expect("plan reservation denied");
    assert!(matches!(denied, AdmittedWideError::Admission(Error::AdmissionDenied)), "{denied:?}");
    assert_eq!((eng.cache_stats().parses, eng.cache_stats().entries, ledger.live.load(SeqCst)), (0, 0, provider));
    // Miss: plan lease stays with the cached plan, output lease with the result, transients are released.
    let first = run_admitted(&mut eng, true).unwrap();
    let plan = eng.plan_reserved_bytes();
    let out_values = (pixels.len() / 4 * 3) as u64;
    assert!(plan > 0 && first.reserved_bytes() >= out_values * 4);
    assert_eq!(ledger.live.load(SeqCst), provider + plan + first.reserved_bytes());
    // Hit: no new plan lease.
    let second = run_admitted(&mut eng, true).unwrap();
    assert!(second.result().receipt.cache_hit && eng.plan_reserved_bytes() == plan);
    assert_eq!(ledger.live.load(SeqCst), provider + plan + first.reserved_bytes() + second.reserved_bytes());
    // Same bits as the unadmitted engine.
    let mut plain = engine(&ctx);
    let reference = run(&mut plain, &ctx, &cmyk, SRGB, &pixels, Intent::RelativeColorimetric, true).unwrap();
    assert_eq!(first.result().pixels, reference.pixels);
    assert_eq!(first.result().receipt.output_sha256, reference.receipt.output_sha256);
    drop((first, second));
    assert_eq!(ledger.live.load(SeqCst), provider + plan);
    // Plan leases retire with the cache; a lease that cannot retire keeps everything reachable.
    ledger.refuse_retire.store(true, SeqCst);
    assert_eq!(eng.try_clear_cache(), Err(Error::RetirementUnavailable));
    assert_eq!(eng.cache_stats().entries, 1);
    let eng = eng.try_retire().expect("refused retirement returns the engine");
    ledger.refuse_retire.store(false, SeqCst);
    assert!(eng.try_retire().is_none());
    assert_eq!(ledger.live.load(SeqCst), 0);
}
