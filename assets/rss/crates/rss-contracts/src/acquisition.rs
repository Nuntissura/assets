use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Continuation {
    pub source_ref: HostRef,
    pub subscription_ref: HostRef,
    pub partition: AccessPartition,
    pub executor_ref: HostRef,
    pub generation: Revision,
    pub token: String,
}
impl Continuation {
    pub fn validate_for(&self, subscription: &Subscription, bounds: &Bounds) -> Result<()> {
        self.partition.validate(bounds)?;
        self.generation.validate()?;
        if self.source_ref != subscription.source_ref || self.subscription_ref != subscription.subscription_ref
            || self.partition != subscription.partition || self.executor_ref != subscription.executor_ref
            || self.generation != subscription.generation { return Err(Error::StaleResult); }
        if self.token.is_empty() { return Err(invalid("continuation.token")); }
        bounded(&self.token, "continuation.token", bounds.max_text_bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WallCursor(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AcquisitionState {
    NeverStarted,
    Running,
    Ready { last_success_at: Instant },
    VerifiedEmpty { observed_at: Instant, selected_source_identity: String },
    Partial { observed_at: Instant, reason_code: String },
    Reconnect,
    Expired,
    Failed { reason_code: String },
    Unsupported,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CaptureIntent {
    SaveUrl { original_url: String },
    FollowSource { original_url: String, provider: Provider, partition: AccessPartition },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedEntry {
    pub provider_entry_key: String,
    pub capture: hsscrap::URLCapture,
    pub observation: hsscrap::MetadataObservation,
}
impl ObservedEntry {
    pub fn validate(&self, bounds: &Bounds, supported: &BTreeSet<String>) -> Result<()> {
        use hsscrap::Validate;
        if self.provider_entry_key.is_empty() { return Err(invalid("provider_entry_key")); }
        bounded(&self.provider_entry_key, "provider_entry_key", bounds.max_text_bytes)?;
        self.capture.validate(bounds, supported)?;
        self.observation.validate(bounds, supported)?;
        if self.capture.item_id != self.observation.item_id
            || self.capture.access_partition != self.observation.access_partition
            || self.observation.capture_id != Some(self.capture.capture_id) {
            return Err(invalid("entry.capture_observation_identity"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcquisitionPage {
    pub subscription_ref: HostRef,
    pub source_ref: HostRef,
    pub partition: AccessPartition,
    pub executor_ref: HostRef,
    pub generation: Revision,
    pub operation_id: OperationId,
    pub entries: Vec<ObservedEntry>,
    pub state: AcquisitionState,
    pub coverage: hsscrap::Coverage,
    #[serde(deserialize_with = "hsscrap::types::required_option")]
    pub continuation: Option<Continuation>,
}
impl AcquisitionPage {
    pub fn validate(&self, subscription: &Subscription, bounds: &Bounds, supported: &BTreeSet<String>) -> Result<()> {
        bounds.validate()?;
        self.partition.validate(bounds)?;
        self.generation.validate()?;
        if self.subscription_ref != subscription.subscription_ref || self.source_ref != subscription.source_ref
            || self.partition != subscription.partition || self.executor_ref != subscription.executor_ref
            || self.generation != subscription.generation { return Err(Error::StaleResult); }
        if self.entries.len() > bounds.max_page_items { return Err(Error::LimitExceeded { field: "entries".into() }); }
        let mut seen = BTreeSet::new();
        for entry in &self.entries {
            entry.validate(bounds, supported)?;
            if entry.capture.access_partition != subscription.partition.scope { return Err(Error::Denied); }
            if !seen.insert(&entry.provider_entry_key) { return Err(invalid("entries.duplicate_key")); }
        }
        if let Some(next) = &self.continuation {
            next.validate_for(subscription, bounds)?;
            if self.coverage == hsscrap::Coverage::Complete { return Err(invalid("coverage")); }
        }
        match &self.state {
            AcquisitionState::VerifiedEmpty { selected_source_identity, .. } => {
                if !self.entries.is_empty() || self.continuation.is_some() || self.coverage != hsscrap::Coverage::Complete
                    || selected_source_identity.is_empty() { return Err(invalid("verified_empty")); }
                bounded(selected_source_identity, "selected_source_identity", bounds.max_text_bytes)?;
            }
            AcquisitionState::Ready { .. } if self.entries.is_empty() => return Err(invalid("state.empty_requires_verified_source")),
            AcquisitionState::Partial { reason_code, .. } => {
                if self.coverage != hsscrap::Coverage::Partial { return Err(invalid("state.partial_coverage")); }
                bounded(reason_code, "reason_code", bounds.max_text_bytes)?;
            }
            AcquisitionState::NeverStarted | AcquisitionState::Running | AcquisitionState::Reconnect
                | AcquisitionState::Expired | AcquisitionState::Failed { .. } | AcquisitionState::Unsupported
                | AcquisitionState::Cancelled => {
                if !self.entries.is_empty() || self.coverage == hsscrap::Coverage::Complete {
                    return Err(invalid("state.failed_payload"));
                }
                if let AcquisitionState::Failed { reason_code } = &self.state { bounded(reason_code, "reason_code", bounds.max_text_bytes)?; }
            }
            _ => {}
        }
        Ok(())
    }
}
