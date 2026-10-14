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
