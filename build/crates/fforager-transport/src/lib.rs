#![forbid(unsafe_code)]
//! Non-shipped WP-007 transport capability and security proof surface.
//!
//! This crate is Phase 0 prerequisite evidence. It is not a product transport.

pub mod adjudication;
pub mod corpus;
mod local_server;
mod policy;

pub use adjudication::{
    AdjudicationError, AdjudicationObservation, AdjudicationRequest, ConvenienceState,
    FingerprintProfile, PolicyConveniences, WireProof, WreqAdjudicationAdapter,
};
pub use corpus::{
    AggregateVerdict, CorpusManifest, CorpusReport, run_corpus, validate_aggregate_evidence,
};
pub use policy::Capability;
