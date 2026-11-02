//! Source-only, caller-granted text shaping. No host font discovery or document mutation.
mod admission;
mod area_text;
mod catalog;
mod contract;
mod create_outlines;
mod descriptor;
mod engine;
mod feature_table;
mod features;
mod finalization;
mod fonts;
mod outline;
mod path_text;
mod provider;
mod raster;
mod readset;
mod result;
mod serialize;
mod validate;
pub use admission::{
    AdmissionPort, AllocationSnapshot, EpochPort, Generation, GenerationRetirement, Reservation,
    RetirementPort,
};
pub use area_text::*;
pub use catalog::*;
pub use contract::*;
pub use create_outlines::*;
pub use descriptor::descriptor;
pub use engine::TextEngine;
pub use features::*;
pub use finalization::*;
pub use outline::*;
pub use path_text::*;
pub use raster::*;
pub use result::*;
pub use validate::{COMPOSITION_VERSION, COMPOSITION_VERSION_V1};

/// True for a character that ends its paragraph in Type's paragraph partition (request
/// validation and the shaping engine): Bidi_Class B of the vendored UCD table (U+000A, U+000D,
/// U+001C..U+001E, U+0085, U+2029) or one of the five STU-TYP-207 break sentinels U+FDD8..U+FDDC
/// (SD-29), which end their paragraph exactly as U+2029 does. Hosts (layout) partition with this
/// rule so Type accepts their paragraphs.
pub fn ends_paragraph(c: char) -> bool {
    terminator::ends_paragraph(c)
}

/// [`ends_paragraph`] outside a request: the engine's own rule (`validate::ends_paragraph`) under
/// a fixed epoch, a token that is never cancelled, no admission and a work budget above the
/// vendored table's binary-search depth (about 1 500 rows, at most 12 steps). Neither the context
/// nor the lookup can then fail; `public_ends_paragraph_is_the_metered_rule_for_every_scalar`
/// checks the answer against the metered rule for every Unicode scalar value.
mod terminator {
    use crate::{AdmissionPort, AllocationSnapshot, EpochPort, Error, Generation, Reservation};
    use hsk_studio_accord::CancellationToken;
    use std::sync::OnceLock;

    const WORK_UNITS: u64 = 64;
    pub(crate) fn ends_paragraph(c: char) -> bool {
        static NEVER_CANCELLED: OnceLock<CancellationToken> = OnceLock::new();
        let cancel = NEVER_CANCELLED.get_or_init(CancellationToken::default);
        crate::provider::Context::new(cancel, &Unmetered, 0, &Unmetered, WORK_UNITS, 1, 1)
            .and_then(|ctx| crate::validate::ends_paragraph(c, &ctx))
            .unwrap_or(false)
    }
    pub(crate) struct Unmetered;
    impl EpochPort for Unmetered {
        fn current_epoch(&self) -> Result<u64, Error> {
            Ok(0)
        }
    }
    impl AdmissionPort for Unmetered {
        fn reserve(&self, _: u64, _: u64) -> Result<Reservation<'_>, Error> {
            Err(Error::Allocation)
        }
        fn snapshot(&self) -> Result<AllocationSnapshot, Error> {
            Ok(AllocationSnapshot {
                current_requested_bytes: 0,
                operation_peak_requested_bytes: 0,
                historical_peak_requested_bytes: 0,
                retained_generations: 0,
            })
        }
        fn retain_generation(&self, _: u64) -> Result<Generation<'_>, Error> {
            Err(Error::Allocation)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::Unmetered;
        use hsk_studio_accord::CancellationToken;

        /// No behaviour change: the public helper answers exactly as the metered rule the engine
        /// and request validation use, for every Unicode scalar value.
        #[test]
        fn public_ends_paragraph_is_the_metered_rule_for_every_scalar() {
            let cancel = CancellationToken::default();
            let ctx = crate::provider::Context::new(&cancel, &Unmetered, 0, &Unmetered, u64::MAX, 1, 1).unwrap();
            let mut terminators = 0u32;
            for c in (0..=0x10FFFF_u32).filter_map(char::from_u32) {
                let metered = crate::validate::ends_paragraph(c, &ctx).unwrap();
                assert_eq!(crate::ends_paragraph(c), metered, "U+{:04X}", c as u32);
                terminators += u32::from(metered);
            }
            assert_eq!(terminators, 12, "seven class-B characters and five break sentinels");
        }
    }
}

/// Fixed failure codes contain no source text, font names or private parser strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidInput,
    Overflow,
    Budget,
    Allocation,
    Canceled,
    StaleEpoch,
    CorruptFont,
    MissingFont,
    MissingGlyph,
    UnsupportedProfile,
    UnsupportedFeature,
    HashMismatch,
    RevokedFont,
    UnavailableFace,
    UnavailableFont,
    UnsupportedFont,
    InvalidAxis,
    AbsentRead,
    UnavailableRead,
    StaleRead,
    UnsupportedFinalization,
    RejectedDelivery,
    UnavailableDelivery,
    SaturatedDelivery,
    IndeterminateDelivery,
    LeaseUnavailable,
    UnknownComposition,
    UnsupportedNormalization,
    InternalInvariant,
    InvalidBidi,
    InvalidMapping,
    PartialOutput,
}
