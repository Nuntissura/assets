//! Public Pinterest source discovery and pin evidence. Hosts own transport, authority and canonical IDs.
pub mod normalize;
pub mod public;
use rss_contracts::{Bounds, HostRef, Instant, ObservedEntry, Pending, SourceRequest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MemberKey {
    pub page_id: Option<String>,
    pub media_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaEvidence {
    pub key: MemberKey,
    pub kind: hsscrap::MediaKind,
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryEvidence {
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<hsscrap::PublicationTime>,
    pub outbound_url: Option<String>,
    pub media: Vec<MediaEvidence>,
    pub document_digest: String,
}
impl EntryEvidence {
    pub fn url(&self) -> String {
        format!("https://www.pinterest.com/pin/{}/", self.id)
    }
    pub fn reliable_provider_identity(&self) -> String {
        format!("pinterest/pin/{}", self.id)
    }
    pub fn populate(&self, observation: &mut hsscrap::MetadataObservation) {
        observation.provider = Some("pinterest".into());
        observation.host_site = "www.pinterest.com".into();
        observation.title = self.title.clone();
        observation.description = self.description.clone();
        observation.author = self.author.clone();
        observation.published_at = self.published_at.clone();
        observation.provider_labels.clear();
        observation.canonical_url_assertion = Some(self.url());
        observation.source_payload_digest = Some(self.document_digest.clone());
        if let Some(gallery) = observation.gallery_snapshot.as_mut() {
            gallery.coverage = hsscrap::Coverage::Partial;
            gallery.full_set_digest = None;
        }
    }
    pub fn validate(&self, bounds: &Bounds) -> rss_contracts::Result<()> {
        if !public::numeric_id(&self.id)
            || self.document_digest.len() != 64
            || !self.document_digest.bytes().all(|v| v.is_ascii_hexdigit())
        {
            return Err(invalid("pinterest.entry_identity"));
        }
        for value in self
            .title
            .iter()
            .chain(self.description.iter())
            .chain(self.author.iter())
        {
            bounds.text("pinterest.metadata", value, bounds.max_text_bytes)?;
        }
        if let Some(date) = &self.published_at {
            date.validate()?;
        }
        if let Some(link) = &self.outbound_url {
            public::public_url(link, bounds)?;
        }
        if self.media.is_empty() || self.media.len() > bounds.max_collection_items {
            return Err(invalid("pinterest.media"));
        }
        let mut keys = BTreeSet::new();
        for media in &self.media {
            public::public_url(&media.url, bounds)?;
            if media.width == Some(0) || media.height == Some(0) {
                return Err(invalid("pinterest.dimensions"));
            }
            for value in media.key.page_id.iter().chain(media.key.media_id.iter()) {
                if value.is_empty() {
                    return Err(invalid("pinterest.member_key"));
                }
                bounds.text("pinterest.member_key", value, bounds.max_text_bytes)?;
            }
            if media.key.media_id.is_some() && !keys.insert(&media.key) {
                return Err(invalid("pinterest.duplicate_member_key"));
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| invalid("pinterest.entry"))?
            .len()
            > bounds.max_operation_bytes
        {
            return Err(rss_contracts::Error::LimitExceeded {
                field: "pinterest.entry".into(),
            });
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardEvidence {
    pub id: String,
    pub owner: String,
    pub url: String,
    pub title: Option<String>,
}
pub struct DiscoveryPage {
    pub sources: Vec<BoardEvidence>,
    pub observed_at: Instant,
    pub continuation: Option<rss_contracts::Continuation>,
    pub coverage: hsscrap::Coverage,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    pub url: String,
    pub expected_digest: Option<String>,
    pub max_body_bytes: usize,
    pub timeout_millis: u64,
}
pub struct FetchResponse {
    pub final_url: String,
    pub status: u16,
    pub body: Vec<u8>,
    pub cached_body: Option<Vec<u8>>,
    pub observed_at: Instant,
}
/// Authorize current source/account/profile/attempt and enforce public network/browser policy and exact retained snapshots.
/// Host anti-CSRF/browser context must not borrow a connected-user session or replace Rust normalization.
pub trait PublicFetchPort: Send + Sync {
    fn fetch<'a>(
        &'a self,
        request: &'a SourceRequest,
        execution: &'a HostRef,
        fetch: FetchRequest,
    ) -> Pending<'a, FetchResponse>;
}
/// Resolve global pinterest/pin/<id>, prepare through existing item owners and allocate all canonical IDs.
pub trait CanonicalEntryPort: Send + Sync {
    fn prepare<'a>(
        &'a self,
        request: &'a SourceRequest,
        execution: &'a HostRef,
        evidence: EntryEvidence,
    ) -> Pending<'a, ObservedEntry>;
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLimits {
    pub max_body_bytes: usize,
    pub max_document_entries: usize,
    pub max_documents: usize,
    pub max_fetches_per_operation: usize,
    pub timeout_millis: u64,
}
impl ProviderLimits {
    fn validate(&self) -> rss_contracts::Result<()> {
        if self.max_body_bytes == 0
            || self.max_body_bytes > 16_000_000
            || self.max_document_entries == 0
            || self.max_document_entries > 1000
            || self.max_documents == 0
            || self.max_documents > 1024
            || self.max_fetches_per_operation < 3
            || self.max_fetches_per_operation > 32
            || self.timeout_millis == 0
            || self.timeout_millis > 300_000
        {
            return Err(invalid("pinterest.limits"));
        }
        Ok(())
    }
}
pub struct PinterestProvider<F, C> {
    pub source: rss_contracts::Source,
    pub execution_ref: HostRef,
    pub limits: ProviderLimits,
    pub supported_extensions: BTreeSet<String>,
    pub fetch: F,
    pub canonical: C,
}
impl<F: PublicFetchPort, C: CanonicalEntryPort> rss_contracts::ProviderPort
    for PinterestProvider<F, C>
{
    fn capabilities(&self) -> Vec<rss_contracts::Capability> {
        [
            rss_contracts::AccessMode::PublicBoard,
            rss_contracts::AccessMode::PublicProfile,
        ]
        .into_iter()
        .map(|mode| rss_contracts::Capability {
            provider: rss_contracts::Provider::Pinterest,
            mode,
            status: rss_contracts::CapabilityStatus::Available,
            paginated: true,
            media_metadata: true,
            adapter_version: "pinterest.public.v1".into(),
        })
        .collect()
    }
    fn acquire<'a>(
        &'a self,
        request: SourceRequest,
    ) -> Pending<'a, rss_contracts::AcquisitionPage> {
        Box::pin(self.acquire_page(request))
    }
}
pub(crate) fn invalid(field: &str) -> rss_contracts::Error {
    rss_contracts::Error::Invalid {
        field: field.into(),
    }
}
