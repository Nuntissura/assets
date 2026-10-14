//! Working-space profile Prism provides to its consumers (SMB-CONVERT-FOLLOWUPS slice 1, CX-CAP-001):
//! linear-light sRGB as a 480-byte ICC v4 matrix/TRC profile, so importers and other callers that
//! convert into the working space consume one Prism-owned profile instead of shipping private copies.
//!
//! The bytes are the CC0 test derivative `tests/profiles/sRGB-linear-v4.icc` (derived from
//! saucecontrol/Compact-ICC-Profiles `sRGB-v4.icc` at commit bdd84663061bc4ae95ca70decff54f581e27f702,
//! TRC set to `para` type 0 gamma 1; derivation and licence in `tests/profiles/README.md` and
//! `tests/profiles/CC0-LICENSE`). Prism's inspection classifies it `linear_light` (tests
//! `foundation_contract.rs` and `working_profile.rs` `working_profile_is_the_pinned_linear_srgb_fixture`).

/// Linear-light sRGB ICC v4 working-space profile (480 bytes; SHA-256
/// [`WORKING_PROFILE_LINEAR_SRGB_SHA256_HEX`]).
pub const WORKING_PROFILE_LINEAR_SRGB: &[u8] = include_bytes!("../tests/profiles/sRGB-linear-v4.icc");

/// SHA-256 of [`WORKING_PROFILE_LINEAR_SRGB`], lowercase hex (pinned in `tests/profiles/README.md`).
pub const WORKING_PROFILE_LINEAR_SRGB_SHA256_HEX: &str =
    "4813c25a76abcf572f146c5a36b56ed83526c82327ee9f863553bf1e99c8b64d";

/// Accessor form of [`WORKING_PROFILE_LINEAR_SRGB`].
pub const fn working_profile_linear_srgb() -> &'static [u8] {
    WORKING_PROFILE_LINEAR_SRGB
}
