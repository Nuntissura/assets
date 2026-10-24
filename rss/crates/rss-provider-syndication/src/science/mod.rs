//! Scientific provenance over the existing syndication parser and transport contracts.
pub mod arxiv;
mod provider;
mod xml;
use crate::{EntryEvidence, FetchPort, ProviderLimits};
use hsscrap::PublicationTime;
use rss_contracts::{HostRef, ObservedEntry, Pending, SourceRequest};
use serde::{Deserialize, Serialize};
/// The existing FetchPort host enforces this interval across all arXiv API requests and attempts.
pub const API_MIN_INTERVAL_MILLIS: u64 = 3000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicationMeaning {
    FirstSubmission,
    Announcement,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaperEvidence {
    pub paper_id: String,
    pub version: Option<u32>,
    pub original_entry_id: String,
    pub publication_meaning: PublicationMeaning,
    pub updated_literal: Option<String>,
    pub doi: Option<String>,
    pub journal_reference: Option<String>,
    pub comment: Option<String>,
    pub announce_type: Option<String>,
    pub source_links: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScientificEvidence {
    pub entry: EntryEvidence,
    pub paper: PaperEvidence,
}
impl ScientificEvidence {
    pub fn reliable_provider_identity(&self) -> String {
        format!("arxiv/paper/{}", self.paper.paper_id)
    }
    pub fn populate(&self, observation: &mut hsscrap::MetadataObservation) {
        self.entry.populate(observation);
        observation.provider = Some("arxiv".into());
    }
    pub fn publication(&self) -> Option<&PublicationTime> {
        self.entry.published_at.as_ref()
    }
}
/// Existing EntryEvidence has no extension/provenance field for DOI, version, announcement semantics
/// or updated literal. Preserve this wrapper durably beside the existing prepare_entry owner intent.
/// Resolve global arxiv/paper/<id> across sources; canonical IDs and version reconciliation remain host-owned.
pub trait CanonicalScientificEntryPort: Send + Sync {
    fn prepare<'a>(
        &'a self,
        request: &'a SourceRequest,
        execution: &'a HostRef,
        source_url: &'a str,
        evidence: ScientificEvidence,
    ) -> Pending<'a, ObservedEntry>;
}
pub struct ScientificProvider<F, N> {
    pub source: rss_contracts::Source,
    pub execution_ref: HostRef,
    pub limits: ProviderLimits,
    pub supported_extensions: std::collections::BTreeSet<String>,
    pub fetch: F,
    pub canonical: N,
}
impl<F: FetchPort, N: CanonicalScientificEntryPort> rss_contracts::ProviderPort
    for ScientificProvider<F, N>
{
    fn capabilities(&self) -> Vec<rss_contracts::Capability> {
        vec![rss_contracts::Capability {
            provider: rss_contracts::Provider::Syndication,
            mode: rss_contracts::AccessMode::PublicFeed,
            status: rss_contracts::CapabilityStatus::Available,
            paginated: true,
            media_metadata: true,
            adapter_version: "science.arxiv.v1".into(),
        }]
    }
    fn acquire<'a>(
        &'a self,
        request: SourceRequest,
    ) -> Pending<'a, rss_contracts::AcquisitionPage> {
        Box::pin(self.acquire_page(request))
    }
}
