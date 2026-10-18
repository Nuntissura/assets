mod claims;
mod cloud;
mod dashboard;
mod identity;
mod protocol;

pub use claims::{ClaimScope, ClaimStatus};
pub use identity::{
    AgentCapability, AgentLaneIdentity, AgentLaneKind, AttributionMode, LocalCloudAttribution,
};

pub use cloud::{
    validate_cloud_assistance_receipt, CloudAssistanceOutputKind, CloudAssistanceReceiptV1,
    CloudFallbackReason, ModelProviderKind, PARALLEL_SWARM_CLOUD_ASSISTANCE_SCHEMA_ID,
};

pub use dashboard::{
    validate_swarm_dashboard_projection, ParallelSwarmDashboardProjectionV1,
    SwarmDashboardAggregateCountV1, SwarmDashboardCheckpointRowV1, SwarmDashboardClaimRowV1,
    SwarmDashboardEventRefV1, SwarmDashboardHandoffRowV1, SwarmDashboardIndexingLeaseRowV1,
    SwarmDashboardLaneRowV1, SwarmDashboardProjectionContractV1, SwarmDashboardProjectionFilters,
    SwarmDashboardQuietWorkRowV1, SwarmDashboardRecoveryReceiptRowV1, SwarmDashboardSourceRefV1,
    SwarmDashboardSourceWatermarkV1, SwarmDashboardTotalsV1, SwarmDashboardWarningV1,
    PARALLEL_SWARM_DASHBOARD_SCHEMA_ID,
};

pub use protocol::{
    validate_handoff_compression_template, HandoffCompressionSourceRefV1,
    HandoffCompressionTemplateV1, RecoveryResumePointer,
    PARALLEL_SWARM_HANDOFF_COMPRESSION_SCHEMA_ID,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod dashboard_tests;

#[cfg(test)]
mod cloud_tests;

#[cfg(test)]
mod identity_tests;

#[cfg(test)]
mod claim_tests;
