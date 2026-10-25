use super::{home::{HomeDocumentKind, HomeFetchRequest, HomeFetchResponse}, home_normalize};
use crate::{authorized::AuthorizedSelection, invalid, public, AuthorizedContext, CanonicalEntryPort,
    FetchRequest, Method, ProviderLimits};
use rss_contracts::{AccessMode, AcquisitionPage, AcquisitionState, Continuation, Error, HostRef,
    Pending, Provider, Result, Source, SourceRequest};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TimelineSelection { Home, Following }
impl TimelineSelection {
    fn mode(self) -> AccessMode { match self { Self::Home => AccessMode::HomeForYou, Self::Following => AccessMode::Following } }
    fn accepts_source(self, source: &str) -> bool { match self {
        Self::Home => matches!(source, "https://www.instagram.com/" | "https://instagram.com/"),
        Self::Following => matches!(source, "https://www.instagram.com/?variant=following" | "https://instagram.com/?variant=following"),
    } }
    fn url(self, kind: HomeDocumentKind) -> &'static str { match (self, kind) {
        (Self::Following, HomeDocumentKind::Html) => "https://www.instagram.com/?variant=following",
        (Self::Following, HomeDocumentKind::Graphql) => super::following::FollowingDocumentKind::Graphql.url(),
        _ => kind.url(),
    } }
}
pub(super) trait TimelineFetch: Send + Sync {
    fn current<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, authorization: &'a AuthorizedContext) -> Pending<'a, ()>;
    fn fetch_timeline<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, authorization: &'a AuthorizedContext, fetch: HomeFetchRequest) -> Pending<'a, HomeFetchResponse>;
}
pub(super) struct Timeline<'a, F, C> {
    pub source: &'a Source, pub execution_ref: &'a HostRef, pub limits: &'a ProviderLimits,
    pub supported_extensions: &'a BTreeSet<String>, pub authorization: &'a AuthorizedContext,
    pub fetch: F, pub canonical: &'a C, pub selection: TimelineSelection,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    source_revision: hsscrap::Revision,
    authorization: AuthorizedSelection,
    mode: AccessMode,
    kind: HomeDocumentKind,
    after: Option<String>,
    digest: Option<String>,
    offset: usize,
    completed_documents: usize,
    visited: Vec<Option<String>>,
    seen: BTreeMap<String, String>,
}
impl<F: TimelineFetch, C: CanonicalEntryPort> Timeline<'_, F, C> {
    fn validate(&self, request: &SourceRequest) -> Result<()> {
        let mode = request.subscription.partition.mode;
        if mode != self.selection.mode() || self.source.provider != Provider::Instagram {
            return Err(Error::Unsupported { provider: Provider::Instagram, mode });
        }
        self.limits.validate()?;
        public::bounded_request(request, &self.execution_ref, &self.supported_extensions)?;
        self.authorization.validate(&self.source, request)?;
        self.source.revision.validate()?;
        self.source.common.validate_bounded(&request.bounds, &self.supported_extensions)?;
        if self.source.source_ref != request.subscription.source_ref
            || !self.selection.accepts_source(&self.source.original_url) {
            return Err(Error::Denied);
        }
        if self.source.paused || request.subscription.paused { return Err(Error::Cancelled); }
        Ok(())
    }
    async fn current(&self, request: &SourceRequest) -> Result<()> {
        self.fetch.current(request, &self.execution_ref, &self.authorization).await
    }
    pub(super) async fn acquire_page(&self, request: SourceRequest) -> Result<AcquisitionPage> {
        self.validate(&request)?;
        if self.selection == TimelineSelection::Following {
            if let Some(continuation) = &request.continuation {
                handshake_canonical_json::jcs_from_slice(continuation.token.as_bytes(), request.bounds.max_text_bytes)
                    .map_err(|_| invalid("instagram.following_cursor"))?;
            }
        }
        let mut cursor: Cursor = if let Some(continuation) = &request.continuation {
            serde_json::from_str(&continuation.token).map_err(|_| invalid("instagram.home_cursor"))?
        } else { Cursor {
            version: 1, source_revision: self.source.revision, authorization: self.authorization.selection(),
            mode: self.selection.mode(), kind: HomeDocumentKind::Html, after: None, digest: None,
            offset: 0, completed_documents: 0, visited: vec![], seen: BTreeMap::new(),
        } };
        if cursor.authorization != self.authorization.selection() || cursor.source_revision != self.source.revision {
            return Err(Error::StaleResult);
        }
        if cursor.version != 1 || cursor.mode != self.selection.mode()
            || (cursor.kind == HomeDocumentKind::Html) != cursor.after.is_none()
            || cursor.completed_documents >= self.limits.max_documents || cursor.visited.len() != cursor.completed_documents
            || cursor.offset > self.limits.max_document_entries || (cursor.offset > 0) != cursor.digest.is_some()
            || cursor.visited.iter().collect::<BTreeSet<_>>().len() != cursor.visited.len()
            || cursor.visited.contains(&cursor.after)
            || cursor.seen.len() > self.limits.max_documents.saturating_mul(self.limits.max_document_entries)
            || cursor.seen.iter().any(|(id, code)| !public::numeric_id(id) || !public::shortcode(code))
            || cursor.digest.as_ref().is_some_and(|value| value.len() != 64 || !value.bytes().all(|v| v.is_ascii_hexdigit())) {
            return Err(invalid("instagram.home_cursor"));
        }
        for token in cursor.after.iter().chain(cursor.visited.iter().flatten()) {
            request.bounds.text("instagram.home_after", token, request.bounds.max_text_bytes)?;
            if token.is_empty() || token.chars().any(char::is_control) { return Err(invalid("instagram.home_after")); }
        }
        for _ in 0..self.limits.max_fetches_per_operation {
            let fetch = HomeFetchRequest { kind: cursor.kind, after: cursor.after.clone(), page_size: 12,
                expected_digest: cursor.digest.clone(), max_body_bytes: self.limits.max_body_bytes,
                timeout_millis: self.limits.timeout_millis };
            self.current(&request).await?;
            let response = self.fetch.fetch_timeline(&request, &self.execution_ref, &self.authorization, fetch.clone()).await;
            self.current(&request).await?;
            let response = response?;
            if response.kind != fetch.kind { return Err(Error::StaleResult); }
            if response.response.status == 304 && cursor.digest.is_none() { return Err(Error::StaleResult); }
            let expected = FetchRequest { method: if fetch.kind == HomeDocumentKind::Html { Method::Get } else { Method::Post },
                url: self.selection.url(fetch.kind).into(), form: vec![], expected_digest: fetch.expected_digest,
                max_body_bytes: fetch.max_body_bytes, timeout_millis: fetch.timeout_millis };
            let (bytes, observed_at) = public::body(response.response, &expected)?;
            let document = home_normalize::document(&bytes, cursor.kind, self.selection == TimelineSelection::Following, self.limits.max_body_bytes,
                self.limits.max_document_entries, &request.bounds)?;
            let now = chrono::DateTime::parse_from_rfc3339(observed_at.as_str()).map_err(|_| invalid("instagram.observed_at"))?.timestamp();
            if document.entries.iter().flat_map(|entry| &entry.media).any(|media| media.expires_at_unix.is_some_and(|expiry| expiry <= now.max(0) as u64)) {
                return Err(Error::Expired);
            }
            if cursor.digest.as_ref().is_some_and(|digest| digest != &document.digest) { return Err(Error::StaleResult); }
            if cursor.offset > 0 && cursor.offset >= document.entries.len() { return Err(invalid("instagram.home_offset")); }
            if document.next.as_ref().is_some_and(|next| cursor.after.as_ref() == Some(next) || cursor.visited.contains(&Some(next.clone()))) {
                return Err(invalid("instagram.home_cycle"));
            }
            for entry in &document.entries {
                if cursor.seen.get(&entry.key).is_some_and(|code| code != &entry.shortcode) { return Err(invalid("instagram.home_retargeted_id")); }
            }
            let mut next = cursor.clone();
            let mut selected = vec![];
            let mut end = cursor.offset;
            while end < document.entries.len() && selected.len() < request.bounds.max_page_items {
                let entry = &document.entries[end];
                if !next.seen.contains_key(&entry.key) { next.seen.insert(entry.key.clone(), entry.shortcode.clone()); selected.push(entry); }
                end += 1;
            }
            let next = if end < document.entries.len() {
                next.offset = end; next.digest = Some(document.digest); Some(next)
            } else if let Some(after) = document.next {
                next.visited.push(cursor.after.clone()); next.completed_documents += 1;
                if next.completed_documents >= self.limits.max_documents { return Err(Error::LimitExceeded { field: "instagram.home_documents".into() }); }
                next.kind = HomeDocumentKind::Graphql; next.after = Some(after); next.offset = 0; next.digest = None; Some(next)
            } else { None };
            let continuation = next.as_ref().map(|cursor| -> Result<_> {
                let result = Continuation { source_ref: request.subscription.source_ref.clone(),
                    subscription_ref: request.subscription.subscription_ref.clone(), partition: request.subscription.partition.clone(),
                    executor_ref: request.subscription.executor_ref.clone(), generation: request.subscription.generation,
                    token: serde_json::to_string(cursor).map_err(|_| invalid("instagram.home_cursor"))? };
                result.validate_for(&request.subscription, &request.bounds)?;
                Ok(result)
            }).transpose()?;
            if selected.is_empty() {
                if let Some(next) = next { cursor = next; continue; }
                if request.continuation.is_none() { return Err(invalid("instagram.home_empty_unverified")); }
            }
            let mut entries = vec![];
            let mut total_bytes = 0usize;
            for evidence in selected {
                self.current(&request).await?;
                let prepared = self.canonical.prepare(&request, &self.execution_ref, (*evidence).clone()).await;
                self.current(&request).await?;
                let prepared = prepared?;
                prepared.validate(&request.bounds, &self.supported_extensions)?;
                let mut expected = prepared.observation.clone(); evidence.populate(&mut expected);
                if prepared.provider_entry_key != evidence.key || prepared.capture.captured_url != evidence.url
                    || prepared.capture.access_partition != request.subscription.partition.scope || prepared.observation != expected {
                    return Err(invalid("instagram.home_canonical_preparation"));
                }
                total_bytes = total_bytes.checked_add(serde_json::to_vec(&prepared).map_err(|_| invalid("instagram.home_entry"))?.len())
                    .ok_or_else(|| invalid("instagram.home_bytes"))?;
                if total_bytes > request.bounds.max_operation_bytes { return Err(Error::LimitExceeded { field: "instagram.home_bytes".into() }); }
                entries.push(prepared);
            }
            let reason_code = if entries.is_empty() { rss_contracts::TERMINAL_SOURCE_DOCUMENT }
                else { match (self.selection, continuation.is_some()) {
                    (TimelineSelection::Home, true) => "instagram_home_page_continues",
                    (TimelineSelection::Home, false) => "instagram_home_current_traversal_only",
                    (TimelineSelection::Following, true) => "instagram_following_page_continues",
                    (TimelineSelection::Following, false) => "instagram_following_current_traversal_only",
                } };
            let page = AcquisitionPage { subscription_ref: request.subscription.subscription_ref.clone(), source_ref: request.subscription.source_ref.clone(),
                partition: request.subscription.partition.clone(), executor_ref: request.subscription.executor_ref.clone(), generation: request.subscription.generation,
                operation_id: request.operation_id, entries, state: AcquisitionState::Partial { observed_at, reason_code: reason_code.into() },
                coverage: hsscrap::Coverage::Partial, continuation };
            page.validate(&request.subscription, &request.bounds, &self.supported_extensions)?;
            return Ok(page);
        }
        Err(Error::LimitExceeded { field: "instagram.home_fetches".into() })
    }
}
