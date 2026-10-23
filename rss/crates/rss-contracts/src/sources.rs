use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider { Syndication, Science, Youtube, Instagram, Tiktok, Pinterest }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    PublicFeed, PublicProfile, PublicReels, PublicChannel, PublicBoard,
    Following, HomeForYou, PrivateSource, OwnUploads, SubscriptionImport,
}
impl AccessMode {
    pub fn requires_connection(self) -> bool {
        matches!(self, Self::Following | Self::HomeForYou | Self::PrivateSource | Self::OwnUploads | Self::SubscriptionImport)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccessPartition {
    pub scope: ScopeRef,
    pub mode: AccessMode,
    #[serde(deserialize_with = "hsscrap::types::required_option")]
    pub connection_ref: Option<HostRef>,
}
impl AccessPartition {
    pub fn validate(&self, bounds: &Bounds) -> Result<()> {
        bounds.validate()?;
        if self.scope.0.is_empty() || self.mode.requires_connection() != self.connection_ref.is_some() {
            return Err(invalid("access_partition"));
        }
        bounded(&self.scope.0, "access_partition.scope", bounds.max_text_bytes)?;
        if let Some(connection) = &self.connection_ref { validate_ref(connection, bounds)?; }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CapabilityStatus {
    Available,
    Unsupported,
    NeedsConnection,
    Expired,
    Restricted,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub provider: Provider,
    pub mode: AccessMode,
    pub status: CapabilityStatus,
    pub paginated: bool,
    pub media_metadata: bool,
    pub adapter_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub source_ref: HostRef,
    pub owner_scope: ScopeRef,
    pub provider: Provider,
    pub original_url: String,
    pub display_name: String,
    pub revision: Revision,
    pub paused: bool,
    #[serde(flatten)] pub common: Extensions,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub subscription_ref: HostRef,
    pub source_ref: HostRef,
    pub partition: AccessPartition,
    pub executor_ref: HostRef,
    pub generation: Revision,
    pub paused: bool,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState { Active, Removed, Unavailable }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAppearance {
    pub appearance_ref: HostRef,
    pub item_id: ItemId,
    pub source_ref: HostRef,
    pub access_partition: ScopeRef,
    pub provider_entry_key: String,
    #[serde(deserialize_with = "hsscrap::types::required_option")]
    pub first_seen_at: Option<Instant>,
    #[serde(deserialize_with = "hsscrap::types::required_option")]
    pub last_seen_at: Option<Instant>,
    #[serde(deserialize_with = "hsscrap::types::required_option")]
    pub latest_observation_id: Option<ObservationId>,
    pub source_state: SourceState,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppearanceState {
    pub state_ref: HostRef,
    pub item_id: ItemId,
    pub appearance_ref: HostRef,
    #[serde(deserialize_with = "hsscrap::types::required_option")]
    pub legacy_item_ref: Option<HostRef>,
    pub scope: ScopeRef,
    pub read: bool,
    pub bookmarked: bool,
    pub legacy_tag_refs: Vec<TagRef>,
    pub revision: Revision,
    #[serde(flatten)] pub common: Extensions,
}

impl SourceAppearance {
    pub fn validate(&self, bounds: &Bounds, supported: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(bounds, supported)?;
        validate_ref(&self.appearance_ref, bounds)?;
        validate_ref(&self.source_ref, bounds)?;
        if self.provider_entry_key.is_empty() { return Err(invalid("provider_entry_key")); }
        bounded(&self.provider_entry_key, "provider_entry_key", bounds.max_text_bytes)?;
        bounded(&self.access_partition.0, "access_partition", bounds.max_text_bytes)?;
        Ok(())
    }
}
