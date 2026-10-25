//! Public Instagram profile/Reels evidence. Hosts authorize transport and own canonical records.
pub mod normalize;
pub mod public;

use std::collections::BTreeSet;
use hsscrap::{Coverage, MetadataObservation, PublicationTime};
use rss_contracts::{Bounds, HostRef, Instant, ObservedEntry, Pending, SourceRequest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaEvidence {
    pub provider_media_id: String,
    pub kind: hsscrap::MediaKind,
    pub url: String,
    pub preview_url: Option<String>,
    pub width: u32,
    pub height: u32,
    pub duration_ms: Option<u64>,
    pub expires_at_unix: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryEvidence {
    pub key: String,
    pub account_id: String,
    pub owner_id: String,
    pub url: String,
    pub shortcode: String,
    pub author: String,
    pub description: Option<String>,
    pub published_at: Option<PublicationTime>,
    pub media: Vec<MediaEvidence>,
    pub document_digest: String,
}
impl EntryEvidence {
    pub fn reliable_provider_identity(&self) -> String { format!("instagram/media/{}", self.key) }
    pub fn populate(&self, observation: &mut MetadataObservation) {
        observation.provider = Some("instagram".into());
        observation.host_site = "www.instagram.com".into();
        observation.author = Some(self.author.clone());
        observation.title = None;
        observation.provider_labels.clear();
        observation.description = self.description.clone();
        observation.published_at = self.published_at.clone();
        observation.canonical_url_assertion = Some(self.url.clone());
        observation.source_payload_digest = Some(self.document_digest.clone());
        if let Some(gallery) = observation.gallery_snapshot.as_mut() {
            gallery.coverage = Coverage::Partial;
            gallery.full_set_digest = None;
        }
    }
    pub fn validate(&self, bounds: &Bounds) -> rss_contracts::Result<()> {
        if !public::numeric_id(&self.key) || !public::numeric_id(&self.account_id) || !public::numeric_id(&self.owner_id)
            || !public::shortcode(&self.shortcode) || !public::username(&self.author)
            || self.url != format!("https://www.instagram.com/p/{}/", self.shortcode)
            || self.document_digest.len() != 64 || !self.document_digest.bytes().all(|v| v.is_ascii_hexdigit()) {
            return Err(invalid("entry.identity"));
        }
        for value in [&self.key, &self.account_id, &self.owner_id, &self.author, &self.shortcode] { bounds.text("entry.identity", value, bounds.max_text_bytes)?; }
        bounds.text("entry.url", &self.url, bounds.max_url_bytes)?;
        if let Some(text) = &self.description { bounds.text("entry.description", text, bounds.max_text_bytes)?; }
        if let Some(time) = &self.published_at { time.validate()?; }
        if self.media.is_empty() || self.media.len() > bounds.max_collection_items { return Err(invalid("entry.media")); }
        let mut ids = BTreeSet::new();
        for media in &self.media {
            if !public::numeric_id(&media.provider_media_id) || !ids.insert(&media.provider_media_id)
                || media.width == 0 || media.height == 0 { return Err(invalid("entry.media_identity")); }
            public::media_url(&media.url, bounds)?;
            if let Some(url) = &media.preview_url { public::media_url(url, bounds)?; }
        }
        if serde_json::to_vec(self).map_err(|_| invalid("entry"))?.len() > bounds.max_operation_bytes {
            return Err(rss_contracts::Error::LimitExceeded { field: "entry.bytes".into() });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method { Get, Post }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    pub method: Method,
    pub url: String,
    pub form: Vec<(String, String)>,
    pub expected_digest: Option<String>,
    pub max_body_bytes: usize,
    pub timeout_millis: u64,
}
/// Fixed anonymous request metadata; contains no account or session values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicRequestMetadata {
    user_agent: String,
    accept: String,
    app_id: String,
    referer: String,
    requested_with: String,
}
impl PublicRequestMetadata {
    fn web() -> Self {
        Self {
            user_agent: concat!("rss-provider-instagram/", env!("CARGO_PKG_VERSION")).into(),
            accept: "*/*".into(),
            app_id: "936619743392459".into(),
            referer: "https://www.instagram.com/".into(),
            requested_with: "XMLHttpRequest".into(),
        }
    }
    pub fn validate(&self) -> rss_contracts::Result<()> {
        if self != &Self::web() {
            return Err(invalid("instagram.public_request_metadata"));
        }
        Ok(())
    }
    pub fn header_pairs(&self) -> [(&str, &str); 5] {
        [
            ("User-Agent", &self.user_agent),
            ("Accept", &self.accept),
            ("X-IG-App-ID", &self.app_id),
            ("Referer", &self.referer),
            ("X-Requested-With", &self.requested_with),
        ]
    }
}
impl FetchRequest {
    pub fn public_metadata(&self) -> PublicRequestMetadata {
        PublicRequestMetadata::web()
    }
}
pub struct FetchResponse {
    pub final_url: String,
    pub status: u16,
    pub body: Vec<u8>,
    pub cached_body: Option<Vec<u8>>,
    pub observed_at: Instant,
}
/// Host public-only policy, current authority/attempt, byte/deadline bounds and exact retained snapshots.
/// Never attach a connected user's credentials or silently substitute another response for a digest.
pub trait PublicFetchPort: Send + Sync {
    fn fetch<'a>(&'a self, source: &'a SourceRequest, execution: &'a HostRef, request: FetchRequest) -> Pending<'a, FetchResponse>;
}
/// Prepare through existing rss-acquisition and canonical identity owners; never allocate IDs in this adapter.
/// Retain exact preparation intent and merge Partial gallery evidence through the gallery owner.
pub trait CanonicalEntryPort: Send + Sync {
    fn prepare<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, evidence: EntryEvidence) -> Pending<'a, ObservedEntry>;
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
        if self.max_body_bytes == 0 || self.max_body_bytes > 16_000_000
            || self.max_document_entries == 0 || self.max_document_entries > 1000
            || self.max_documents == 0 || self.max_documents > 1024
            || self.max_fetches_per_operation < 2 || self.max_fetches_per_operation > 32
            || self.timeout_millis == 0 || self.timeout_millis > 300_000 { return Err(invalid("provider.limits")); }
        Ok(())
    }
}
pub struct InstagramProvider<F, C> {
    pub source: rss_contracts::Source,
    pub execution_ref: HostRef,
    pub limits: ProviderLimits,
    pub supported_extensions: BTreeSet<String>,
    pub fetch: F,
    pub canonical: C,
}
impl<F: PublicFetchPort, C: CanonicalEntryPort> rss_contracts::ProviderPort for InstagramProvider<F, C> {
    fn capabilities(&self) -> Vec<rss_contracts::Capability> {
        [rss_contracts::AccessMode::PublicProfile, rss_contracts::AccessMode::PublicReels].into_iter().map(|mode| rss_contracts::Capability {
            provider: rss_contracts::Provider::Instagram, mode, status: rss_contracts::CapabilityStatus::Available,
            paginated: true, media_metadata: true, adapter_version: "instagram.public.v1".into(),
        }).collect()
    }
    fn acquire<'a>(&'a self, request: SourceRequest) -> Pending<'a, rss_contracts::AcquisitionPage> { Box::pin(self.acquire_page(request)) }
}
pub(crate) fn invalid(field: &str) -> rss_contracts::Error { rss_contracts::Error::Invalid { field: field.into() } }
