#![forbid(unsafe_code)]
//! Non-shipped WP-007 transport capability and security proof surface.
//!
//! This crate is Phase 0 prerequisite evidence. It is not a product transport.

#[cfg(feature = "wreq-adjudication")]
pub mod adjudication;
pub mod corpus;
mod local_server;
#[cfg(feature = "ordinary-transport")]
pub mod ordinary;
mod policy;

#[cfg(feature = "wreq-adjudication")]
pub use adjudication::{
    AdjudicationError, AdjudicationObservation, EvidenceState, FingerprintProfile,
    FingerprintReceipt, LiveNetworkAuthorization, LiveProbeOptions, LiveProbeVerdict,
    LiveWireProbeReport, PersistedLiveProbeReport, PolicyConveniences, ProfileParityStatus,
    StructuralWireEvidence, WreqAdjudicationAdapter, WreqAdjudicationReport,
    WreqAdjudicationVerdict, run_wreq_adjudication, validate_live_wire_probe_report,
    validate_persisted_live_probe_report, validate_wreq_adjudication_report,
};
pub use corpus::{
    AggregateVerdict, CorpusManifest, CorpusReport, run_corpus, validate_aggregate_evidence,
};
#[cfg(feature = "ordinary-transport")]
pub use ordinary::{
    OrdinaryTransportDecisionError, OrdinaryTransportDecisionReport,
    run_ordinary_transport_decision, validate_ordinary_transport_decision_report,
};
pub use policy::Capability;
