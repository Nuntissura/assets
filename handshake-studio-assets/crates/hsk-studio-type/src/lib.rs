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
/// rule so Type accepts their paragraphs. Constant time (R54 F-R54-4): no provider context and no
/// table lookup per character.
pub fn ends_paragraph(c: char) -> bool {
    terminator::ends_paragraph(c)
}

/// [`ends_paragraph`] outside a request: a constant-time match on the twelve terminators, kept
/// equal to the engine's metered rule (`validate::ends_paragraph`: the vendored bidi table under
/// a provider context) for every Unicode scalar value by
/// `public_ends_paragraph_is_the_metered_rule_for_every_scalar`, which fails if a table update
/// adds or removes a class-B character. Measurement (R54 F-R54-4, lane SMB-R54-FOLLOWUPS-1,
/// `public_ends_paragraph_answers_a_long_story_without_the_metered_lookup`): the metered rule
/// spends one context and about 10 epoch-polling search steps per scalar.
mod terminator {
    pub(crate) fn ends_paragraph(c: char) -> bool {
        matches!(c, '\u{000A}' | '\u{000D}' | '\u{001C}'..='\u{001E}' | '\u{0085}' | '\u{2029}')
            || crate::catalog::is_break_sentinel(c)
    }

    #[cfg(test)]
    mod tests {
        use crate::{AdmissionPort, AllocationSnapshot, EpochPort, Error, Generation, Reservation};
        use hsk_studio_accord::CancellationToken;

        struct Unmetered;
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

        /// A long mixed story (Latin, Arabic, CJK, U+2028, every terminator, the whole sentinel
        /// block): 64 Ki scalars, deterministic.
        fn long_story() -> Vec<char> {
            let unit: Vec<char> = "The quick brown fox jumps over the lazy dog; \u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629} \u{6F22}\u{5B57} \u{2028}"
                .chars()
                .chain(['\n', '\r', '\u{1C}', '\u{1D}', '\u{1E}', '\u{85}', '\u{2029}'])
                .chain((0xFDD0..=0xFDEF).filter_map(char::from_u32))
                .collect();
            unit.iter().copied().cycle().take(65_536).collect()
        }

        /// F-R54-4 measurement (counted operations; wall time printed, never asserted): the
        /// metered rule builds one provider Context and binary-searches the vendored bidi table
        /// per scalar, each search step polling the cancellation token and the epoch port. The
        /// public helper that layout calls per character (paragraph_partition,
        /// replace_story_text) answers every scalar of a long story without that lookup, with the
        /// same answers.
        #[test]
        fn public_ends_paragraph_answers_a_long_story_without_the_metered_lookup() {
            let story = long_story();
            let cancel = CancellationToken::default();
            let (mut units, mut max_units, mut terminators) = (0u64, 0u64, 0u64);
            let started = std::time::Instant::now();
            for &c in &story {
                let ctx = crate::provider::Context::new(&cancel, &Unmetered, 0, &Unmetered, u64::MAX, 1, 1).unwrap();
                terminators += u64::from(crate::validate::ends_paragraph(c, &ctx).unwrap());
                units += ctx.work();
                max_units = max_units.max(ctx.work());
            }
            let metered_ns = started.elapsed().as_nanos();
            let before = crate::validate::METERED_CALLS.with(std::cell::Cell::get);
            let started = std::time::Instant::now();
            let public = story.iter().filter(|c| crate::ends_paragraph(**c)).count() as u64;
            let public_ns = started.elapsed().as_nanos();
            let calls = crate::validate::METERED_CALLS.with(std::cell::Cell::get) - before;
            println!(
                "MEASURE F-R54-4 ends_paragraph scalars={} terminators={} metered: contexts={} search_steps={} steps_per_scalar={:.2} max_steps={} epoch_polls={} wall_ns={} | public: metered_calls={} wall_ns={} (wall time informative only)",
                story.len(),
                terminators,
                story.len(),
                units,
                units as f64 / story.len() as f64,
                max_units,
                units + story.len() as u64,
                metered_ns,
                calls,
                public_ns
            );
            assert_eq!(public, terminators, "same answers on the story");
            assert!(units >= story.len() as u64, "the metered rule spends at least one search step per scalar");
            assert_eq!(calls, 0, "the public helper answered {calls} scalars through the metered lookup");
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
