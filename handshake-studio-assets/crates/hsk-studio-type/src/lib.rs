//! Source-only, caller-granted text shaping. No host font discovery or document mutation.
mod admission;
mod contract;
mod descriptor;
mod engine;
mod finalization;
mod fonts;
mod provider;
mod readset;
mod result;
mod serialize;
mod validate;
pub use admission::{
    AdmissionPort, AllocationSnapshot, EpochPort, Generation, GenerationRetirement, Reservation,
    RetirementPort,
};
pub use contract::*;
pub use descriptor::descriptor;
pub use engine::TextEngine;
pub use finalization::*;
pub use result::*;
pub use validate::COMPOSITION_VERSION;

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
