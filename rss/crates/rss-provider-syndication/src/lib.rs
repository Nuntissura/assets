//! RSS/Atom evidence parsing and bounded host ports. Hosts retain canonical ownership and transport policy.
pub mod blogs;
pub mod rss;
pub mod science;

use hsscrap::{MediaKind, MetadataObservation, PublicationTime};
use rss_contracts::{Bounds, HostRef, Instant, ObservedEntry, Pending, SourceRequest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaEvidence {
    pub url: String,
    pub kind: MediaKind,
    pub mime: Option<String>,
    pub caption: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub size_bytes: Option<u64>,
    pub thumbnail: bool,
}

/// Unowned provider evidence; all canonical item, observation, member and reference IDs come from the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryEvidence {
    pub key: String,
    pub url: String,
    pub reliable_provider_id: Option<String>,
    pub host_site: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<PublicationTime>,
    pub labels: Vec<String>,
    pub media: Vec<MediaEvidence>,
    pub media_coverage: hsscrap::Coverage,
    pub document_digest: String,
}
impl EntryEvidence {
    /// Populate only source metadata on a reservation from rss-acquisition::prepare_entry.
    /// The host separately allocates ordered media members/references from `media`; no IDs are inferred here.
    pub fn populate(&self, observation: &mut MetadataObservation) {
        observation.title = self.title.clone();
        observation.description = self.description.clone();
        observation.author = self.author.clone();
        observation.published_at = self.published_at.clone();
        observation.provider_labels = self.labels.clone();
        observation.provider = Some("syndication".into());
        observation.host_site = self.host_site.clone();
        observation.canonical_url_assertion = Some(self.url.clone());
        observation.source_payload_digest = Some(self.document_digest.clone());
        if let Some(gallery) = observation.gallery_snapshot.as_mut() {
            gallery.coverage = self.media_coverage;
            gallery.full_set_digest = None;
        }
    }
    pub fn validate(&self, bounds: &Bounds) -> rss_contracts::Result<()> {
        bounds.validate()?;
        if self.media_coverage != hsscrap::Coverage::Partial { return Err(invalid("entry.media_coverage")); }
        if self.key.is_empty() || self.document_digest.len() != 64 || !self.document_digest.bytes().all(|value| value.is_ascii_hexdigit()) {
            return Err(invalid("entry.identity"));
        }
        for (field, value) in [("entry.key", &self.key), ("entry.url", &self.url)] {
            bounds.text(field, value, bounds.max_text_bytes)?;
        }
        let url = blogs::public_url(&self.url, bounds)?;
        if url.host_str() != Some(self.host_site.as_str()) { return Err(invalid("entry.host_site")); }
        for value in self.title.iter().chain(self.description.iter()).chain(self.author.iter()).chain(self.labels.iter()) {
            bounds.text("entry.metadata", value, bounds.max_text_bytes)?;
        }
        if let Some(date) = &self.published_at {
            date.validate()?;
            bounds.text("entry.publication.raw", &date.raw_value, bounds.max_text_bytes)?;
            bounds.text("entry.publication.local", &date.local_value, bounds.max_text_bytes)?;
        }
        if self.reliable_provider_id.as_ref().is_some_and(|value| value.is_empty() || value != &self.key) {
            return Err(invalid("entry.provider_id"));
        }
        if self.media.len() > bounds.max_collection_items || self.labels.len() > bounds.max_collection_items {
            return Err(rss_contracts::Error::LimitExceeded { field: "entry.collections".into() });
        }
        for media in &self.media {
            blogs::public_url(&media.url, bounds)?;
            for value in media.mime.iter().chain(media.caption.iter()) { bounds.text("media.metadata", value, bounds.max_text_bytes)?; }
        }
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("entry"))?;
        if bytes.len() > bounds.max_operation_bytes { return Err(rss_contracts::Error::LimitExceeded { field: "entry".into() }); }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    pub url: String,
    /// Resume the exact retained document snapshot; never substitute newer bytes under this digest.
    pub expected_digest: Option<String>,
    pub max_body_bytes: usize,
    pub timeout_millis: u64,
    pub max_redirects: u8,
}
pub struct FetchResponse {
    pub final_url: String,
    pub status: u16,
    pub body: Vec<u8>,
    /// For 304 only: exact previously committed bytes for this source, URL and access partition.
    /// A missing cache is uncertainty, never evidence of an empty feed.
    pub cached_body: Option<Vec<u8>>,
    pub observed_at: Instant,
}

/// Enforce current authority/attempt, DNS/address policy, redirects, byte limits and deadline while fetching.
/// Reauthorize every redirect; credentials and caches remain in the host. Never treat a scope handle as a grant.
/// Before preparation, durably bind the response to (page operation, document URL); retries reuse that intent.
/// A requested digest selects the authorized retained snapshot for interrupted within-document pagination.
pub trait FetchPort: Send + Sync {
    fn fetch<'a>(&'a self, source: &'a SourceRequest, execution: &'a HostRef, request: FetchRequest) -> Pending<'a, FetchResponse>;
}

/// Prepare through the existing rss-acquisition canonical owner/allocator boundary.
/// Durably retain immutable allocations and PreparedEntry intent for commit_page and exact retry.
/// Reliable identity namespace is the selected feed URL; URL-only evidence uses no SourceIdentity.
/// Persist advertised feed media only as Partial gallery evidence without a full-set digest;
/// apply it through the owning gallery boundary while preserving unmentioned canonical members.
/// Proposed observation fields and returned ObservedEntry alone do not prove that host behavior.
pub trait CanonicalEntryPort: Send + Sync {
    fn prepare<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef,
        feed_url: &'a str, evidence: EntryEvidence) -> Pending<'a, ObservedEntry>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLimits {
    pub max_body_bytes: usize,
    pub max_document_entries: usize,
    pub max_documents: usize,
    pub max_fetches_per_operation: usize,
    pub timeout_millis: u64,
    pub max_redirects: u8,
}
impl ProviderLimits {
    fn validate(&self) -> rss_contracts::Result<()> {
        if self.max_body_bytes == 0 || self.max_body_bytes > 16_000_000 || self.max_document_entries == 0
            || self.max_document_entries > 10_000 || self.max_documents == 0 || self.max_documents > 1024
            || self.max_fetches_per_operation == 0 || self.max_fetches_per_operation > 32
            || self.timeout_millis == 0 || self.timeout_millis > 300_000 || self.max_redirects > 16 {
            return Err(invalid("provider.limits"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    source_url: String,
    document_url: String,
    document_request_url: String,
    digest: Option<String>,
    offset: usize,
    visited: Vec<String>,
    completed_documents: usize,
}

/// Construct from the canonical selected source. FetchPort must reject changed source configuration
/// under the request's subscription epoch, and the normalizer rechecks authority before actual owner effects.
pub struct SyndicationProvider<F, N> {
    pub source: rss_contracts::Source,
    pub execution_ref: HostRef,
    pub limits: ProviderLimits,
    pub supported_extensions: std::collections::BTreeSet<String>,
    pub fetch: F,
    pub canonical: N,
}
impl<F: FetchPort, N: CanonicalEntryPort> SyndicationProvider<F, N> {
    async fn acquire_page(&self, request: SourceRequest) -> rss_contracts::Result<rss_contracts::AcquisitionPage> {
        use std::collections::BTreeSet;
        use rss_contracts::{AcquisitionState, Continuation, Error, Provider, AccessMode};
        self.limits.validate()?;
        rss_acquisition::validate_request(&request)?;
        self.source.revision.validate()?;
        self.source.common.validate_bounded(&request.bounds, &self.supported_extensions)?;
        request.bounds.text("source.owner_scope", &self.source.owner_scope.0, request.bounds.max_text_bytes)?;
        if self.source.source_ref != request.subscription.source_ref || self.source.owner_scope.0.is_empty() {
            return Err(Error::Denied);
        }
        if self.source.provider != Provider::Syndication || request.subscription.partition.mode != AccessMode::PublicFeed {
            return Err(Error::Unsupported { provider: Provider::Syndication, mode: request.subscription.partition.mode });
        }
        if self.source.paused || request.subscription.paused { return Err(Error::Cancelled); }
        let source_url = blogs::public_url(&self.source.original_url, &request.bounds)?.to_string();
        let mut cursor = if let Some(continuation) = &request.continuation {
            let cursor: Cursor = serde_json::from_str(&continuation.token).map_err(|_| invalid("continuation.syndication"))?;
            if cursor.version != 1 || cursor.source_url != source_url || cursor.completed_documents >= self.limits.max_documents
                || cursor.visited.len() > cursor.completed_documents.saturating_mul(2) || cursor.visited.len() < cursor.completed_documents
                || (cursor.offset > 0) != cursor.digest.is_some()
                || cursor.offset == 0 && cursor.document_url != cursor.document_request_url { return Err(invalid("continuation.syndication")); }
            blogs::public_url(&cursor.document_url, &request.bounds)?;
            blogs::public_url(&cursor.document_request_url, &request.bounds)?;
            let mut seen = BTreeSet::new();
            for previous in &cursor.visited {
                blogs::public_url(previous, &request.bounds)?;
                if !seen.insert(previous) { return Err(invalid("continuation.cycle")); }
            }
            cursor
        } else { Cursor { version: 1, source_url: source_url.clone(), document_url: source_url.clone(), document_request_url: source_url.clone(),
            digest: None, offset: 0, visited: vec![], completed_documents: 0 } };
        let mut fetches = 0;
        let mut discovery_allowed = request.continuation.is_none();
        loop {
            if fetches == self.limits.max_fetches_per_operation { return Err(Error::LimitExceeded { field: "provider.fetches".into() }); }
            if cursor.visited.contains(&cursor.document_url) || cursor.visited.contains(&cursor.document_request_url) { return Err(invalid("continuation.cycle")); }
            fetches += 1;
            let response = self.fetch.fetch(&request, &self.execution_ref, FetchRequest { url: cursor.document_url.clone(), expected_digest: cursor.digest.clone(),
                max_body_bytes: self.limits.max_body_bytes, timeout_millis: self.limits.timeout_millis, max_redirects: self.limits.max_redirects }).await?;
            let final_url = blogs::public_url(&response.final_url, &request.bounds)?.to_string();
            if cursor.visited.contains(&final_url) { return Err(invalid("continuation.cycle")); }
            if cursor.digest.is_some() && final_url != cursor.document_url { return Err(Error::StaleResult); }
            let body = match response.status {
                200 => { if response.cached_body.is_some() { return Err(invalid("fetch.cache")); } response.body }
                304 if response.body.is_empty() => response.cached_body.ok_or_else(|| invalid("fetch.missing_cached_body"))?,
                401 | 403 => return Err(Error::Denied),
                404 | 410 => return Err(Error::NotFound),
                429 => return Err(Error::RateLimited { retry_after: None }),
                _ => return Err(Error::Network),
            };
            if body.len() > self.limits.max_body_bytes { return Err(Error::LimitExceeded { field: "feed.body".into() }); }
            let document = match rss::parse(&final_url, &body, self.limits.max_body_bytes, self.limits.max_document_entries, &request.bounds) {
                Ok(document) => document,
                Err(Error::Invalid { ref field }) if discovery_allowed && field == "feed.parse" => {
                    discovery_allowed = false;
                    let candidates = blogs::discover(&final_url, &body, self.limits.max_body_bytes, &request.bounds)?;
                    if candidates.len() != 1 { return Err(invalid("source.select_feed")); }
                    cursor.document_url = candidates[0].clone();
                    cursor.document_request_url = cursor.document_url.clone();
                    continue;
                }
                Err(error) => return Err(error),
            };
            if cursor.digest.as_ref().is_some_and(|digest| digest != &document.digest) { return Err(Error::StaleResult); }
            if cursor.offset > 0 && cursor.offset >= document.entries.len() { return Err(invalid("continuation.offset")); }
            if document.next_document.as_ref().is_some_and(|next| next == &final_url || next == &cursor.document_request_url || cursor.visited.contains(next)) {
                return Err(invalid("continuation.cycle"));
            }
            let end = cursor.offset.saturating_add(request.bounds.max_page_items).min(document.entries.len());
            let next = if end < document.entries.len() {
                Some(Cursor { version: 1, source_url: source_url.clone(), document_url: final_url.clone(),
                    document_request_url: cursor.document_request_url.clone(), digest: Some(document.digest.clone()), offset: end,
                    visited: cursor.visited.clone(), completed_documents: cursor.completed_documents })
            } else if let Some(next_url) = &document.next_document {
                let mut visited = cursor.visited.clone(); visited.push(cursor.document_request_url.clone());
                if final_url != cursor.document_request_url { visited.push(final_url.clone()); }
                let completed_documents = cursor.completed_documents + 1;
                if completed_documents >= self.limits.max_documents { return Err(Error::LimitExceeded { field: "feed.documents".into() }); }
                Some(Cursor { version: 1, source_url: source_url.clone(), document_url: next_url.clone(), document_request_url: next_url.clone(),
                    digest: None, offset: 0, visited, completed_documents })
            } else { None };
            if document.entries.is_empty() {
                if let Some(next) = next.as_ref() { cursor = next.clone(); discovery_allowed = false; continue; }
            }
            let continuation = next.map(|next| -> rss_contracts::Result<Continuation> {
                let token = serde_json::to_string(&next).map_err(|_| invalid("continuation"))?;
                let value = Continuation { source_ref: request.subscription.source_ref.clone(), subscription_ref: request.subscription.subscription_ref.clone(),
                    partition: request.subscription.partition.clone(), executor_ref: request.subscription.executor_ref.clone(), generation: request.subscription.generation, token };
                value.validate_for(&request.subscription, &request.bounds)?;
                Ok(value)
            }).transpose()?;
            // Validate the complete next cursor before canonical preparation can perform any owner effects.
            let mut entries = vec![];
            for evidence in document.entries.into_iter().skip(cursor.offset).take(end - cursor.offset) {
                let prepared = self.canonical.prepare(&request, &self.execution_ref, &source_url, evidence.clone()).await?;
                prepared.validate(&request.bounds, &self.supported_extensions)?;
                let mut expected = prepared.observation.clone(); evidence.populate(&mut expected);
                if prepared.provider_entry_key != evidence.key || prepared.capture.captured_url != evidence.url || expected != prepared.observation {
                    return Err(invalid("entry.canonical_preparation"));
                }
                entries.push(prepared);
            }
            let empty = entries.is_empty();
            let terminal_document = empty && request.continuation.is_some();
            let state = if terminal_document { AcquisitionState::Partial { observed_at: response.observed_at.clone(), reason_code: rss_contracts::TERMINAL_SOURCE_DOCUMENT.into() } }
                else if empty { AcquisitionState::VerifiedEmpty { observed_at: response.observed_at.clone(), selected_source_identity: source_url } }
                else { AcquisitionState::Partial { observed_at: response.observed_at, reason_code: if continuation.is_some() { "feed_document_continues" } else { "current_feed_document_only" }.into() } };
            return Ok(rss_contracts::AcquisitionPage { subscription_ref: request.subscription.subscription_ref.clone(), source_ref: request.subscription.source_ref.clone(),
                partition: request.subscription.partition.clone(), executor_ref: request.subscription.executor_ref.clone(), generation: request.subscription.generation,
                operation_id: request.operation_id, entries, state, coverage: if empty && !terminal_document { hsscrap::Coverage::Complete } else { hsscrap::Coverage::Partial }, continuation });
        }
    }
}
pub fn capabilities() -> Vec<rss_contracts::Capability> {
    vec![rss_contracts::Capability { provider: rss_contracts::Provider::Syndication, mode: rss_contracts::AccessMode::PublicFeed,
        status: rss_contracts::CapabilityStatus::Available, paginated: true, media_metadata: true, adapter_version: "syndication.v1".into() }]
}
impl<F: FetchPort, N: CanonicalEntryPort> rss_contracts::ProviderPort for SyndicationProvider<F, N> {
    fn capabilities(&self) -> Vec<rss_contracts::Capability> { capabilities() }
    fn acquire<'a>(&'a self, request: SourceRequest) -> Pending<'a, rss_contracts::AcquisitionPage> {
        Box::pin(self.acquire_page(request))
    }
}

pub(crate) fn invalid(field: &str) -> rss_contracts::Error { rss_contracts::Error::Invalid { field: field.into() } }
