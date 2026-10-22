//! SMB-CONVERT-FOLLOWUPS slice 1: Prism's public working-space profile is the pinned CC0 linear sRGB
//! fixture and Prism's own inspection classifies it linear light.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_prism::{
    ColorEngine, Prism, ProfileInput, Transfer, WORKING_PROFILE_LINEAR_SRGB, WORKING_PROFILE_LINEAR_SRGB_SHA256_HEX, hash_hex,
    profile_hash, working_profile_linear_srgb,
};

const FIXTURE: &[u8] = include_bytes!("profiles/sRGB-linear-v4.icc");

#[test]
fn working_profile_is_the_pinned_linear_srgb_fixture() {
    assert_eq!(WORKING_PROFILE_LINEAR_SRGB, FIXTURE);
    assert_eq!(working_profile_linear_srgb(), FIXTURE);
    assert_eq!(WORKING_PROFILE_LINEAR_SRGB.len(), 480);
    assert_eq!(hash_hex(&profile_hash(WORKING_PROFILE_LINEAR_SRGB)), WORKING_PROFILE_LINEAR_SRGB_SHA256_HEX);

    let actor = ActorContext::new("account", "principal", "owner-account", "owner-principal", "space", "session").unwrap();
    let id = DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap();
    let engine = Prism::new(actor, 1).unwrap();
    let input = ProfileInput {
        profile_id: &id,
        bytes: WORKING_PROFILE_LINEAR_SRGB,
        expected_sha256: profile_hash(WORKING_PROFILE_LINEAR_SRGB),
    };
    let descriptor = engine.inspect_profile(input, &CancellationToken::default()).unwrap();
    assert_eq!(descriptor.transfer(), Transfer::LinearLight);
}

/// Minimal caller ledger for the admitted route (counts live bytes; never denies).
#[derive(Clone, Default)]
struct Ledger(std::sync::Arc<std::sync::atomic::AtomicU64>);
struct Lease(std::sync::Arc<std::sync::atomic::AtomicU64>, u64);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_sub(self.1, std::sync::atomic::Ordering::SeqCst);
    }
}
impl hsk_studio_prism::ProviderReservation for Lease {
    fn reserved_bytes(&self) -> u64 {
        self.1
    }
}
impl hsk_studio_prism::ProviderAdmission for Ledger {
    type Reservation = Lease;
    fn synchronous_retirement(&self) -> bool {
        true
    }
    fn reserve(&self, bytes: u64) -> Result<Lease, hsk_studio_prism::Error> {
        self.0.fetch_add(bytes, std::sync::atomic::Ordering::SeqCst);
        Ok(Lease(self.0.clone(), bytes))
    }
}

/// SMB-PRISM-ADMITTED-WORKING-PROFILE: the metadata-free working profile is the pinned fixture,
/// the admitted engine accepts it as linear light, and refuses the text-carrying working profile.
#[test]
fn admitted_working_profile_is_the_pinned_fixture_and_admitted_linear_light() {
    use hsk_studio_prism::{AdmittedPrism, WORKING_PROFILE_LINEAR_SRGB_ADMITTED, WORKING_PROFILE_LINEAR_SRGB_ADMITTED_SHA256_HEX};
    const ADMITTED_FIXTURE: &[u8] = include_bytes!("profiles/sRGB-admitted-linear-v4.icc");
    assert_eq!(WORKING_PROFILE_LINEAR_SRGB_ADMITTED, ADMITTED_FIXTURE);
    assert_eq!(WORKING_PROFILE_LINEAR_SRGB_ADMITTED.len(), 368);
    assert_eq!(hash_hex(&profile_hash(WORKING_PROFILE_LINEAR_SRGB_ADMITTED)), WORKING_PROFILE_LINEAR_SRGB_ADMITTED_SHA256_HEX);

    let actor = ActorContext::new("account", "principal", "owner-account", "owner-principal", "space", "session").unwrap();
    let id = DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap();
    let ledger = Ledger::default();
    let engine = AdmittedPrism::new(&actor, 1, ledger.clone()).unwrap();
    let input = |bytes: &'static [u8]| ProfileInput { profile_id: &id, bytes, expected_sha256: profile_hash(bytes) };
    let admitted = engine.inspect_profile(input(WORKING_PROFILE_LINEAR_SRGB_ADMITTED), &CancellationToken::default()).unwrap();
    assert_eq!(admitted.descriptor().transfer(), Transfer::LinearLight);
    assert!(engine.inspect_profile(input(WORKING_PROFILE_LINEAR_SRGB), &CancellationToken::default()).is_err(), "text tags are refused on the admitted route");
}
