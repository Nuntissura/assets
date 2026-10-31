//! Test host attribution (lane SMB-ATTRIBUTION-1): these tests are their own host, so they build
//! their own `RunContext` (a test actor, client session `harness-core-tests`, fresh in-memory
//! correlations) instead of the asset-local test host's feature-gated one, and run in every round
//! unit without a feature (rule 4e).
#![allow(dead_code)]
use hsk_studio_accord::ActorContext;
use hsk_studio_harness_core::{ClientSessionId, RunContext, TransientCorrelation};
use std::sync::Arc;

pub fn test_context() -> RunContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    RunContext::new(actor, ClientSessionId::parse("harness-core-tests").unwrap(), Arc::new(TransientCorrelation::new()))
}
