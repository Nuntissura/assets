use crate::*;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRequest {
    pub subscription: Subscription,
    pub operation_id: OperationId,
    #[serde(deserialize_with = "hsscrap::types::required_option")]
    pub continuation: Option<Continuation>,
    pub bounds: Bounds,
}
pub trait ProviderPort: Send + Sync {
    fn capabilities(&self) -> Vec<Capability>;
    fn acquire<'a>(&'a self, request: SourceRequest) -> Pending<'a, AcquisitionPage>;
}
pub trait AuthorityPort: Send + Sync {
    fn authorize<'a>(&'a self, scope: &'a ScopeRef, resource: &'a HostRef, action: SourceAction) -> Pending<'a, ()>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAction { Inspect, Follow, Refresh, Pause, Remove, ChangeAppearanceState }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchCommit {
    pub operation_id: OperationId,
    pub subscription_ref: HostRef,
    pub expected_generation: Revision,
    pub item_owner_outcome_refs: Vec<HostRef>,
    pub appearances: Vec<SourceAppearance>,
    #[serde(deserialize_with = "hsscrap::types::required_option")]
    pub continuation: Option<Continuation>,
    pub state: AcquisitionState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommitOutcome {
    /// Accepted subscription/executor fence, equal to this batch's expected_generation.
    /// Page commits do not advance the subscription epoch; host storage revisions are separate.
    Committed { receipt_ref: HostRef, generation: Revision },
    Pending { operation_id: OperationId },
    Conflict,
}
pub trait CheckpointOwner: Send + Sync {
    fn lookup<'a>(&'a self, scope: &'a ScopeRef, operation: OperationId) -> Pending<'a, CommitOutcome>;
    /// Commit accepted owner outcomes, appearances and continuation together, or retain pending recovery.
    /// Atomically bind operation_id to the entire immutable batch and current authorized job/executor/generation.
    /// Every call, including replay after a lost reply, compares that full intent and rejects changed reuse.
    /// Verify referenced canonical owner outcomes before committing. Lookup alone does not prove batch intent.
    fn commit<'a>(&'a self, scope: &'a ScopeRef, batch: BatchCommit) -> Pending<'a, CommitOutcome>;
}
