use super::{arxiv, CanonicalScientificEntryPort, ScientificProvider};
use crate::{invalid, FetchPort, FetchRequest, FetchResponse};
use rss_contracts::{
    AcquisitionPage, AcquisitionState, Continuation, Error, Result, SourceRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    source: String,
    revision: hsscrap::Revision,
    start: u64,
    total: Option<u64>,
    digest: Option<String>,
    offset: usize,
    documents: usize,
    seen: BTreeMap<String, String>,
}
fn fingerprint(value: &super::ScientificEvidence) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut value = value.clone();
    value.entry.document_digest.clear();
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).map_err(|_| invalid("science.evidence"))?)
    ))
}
impl<F: FetchPort, N: CanonicalScientificEntryPort> ScientificProvider<F, N> {
    pub(super) async fn acquire_page(&self, request: SourceRequest) -> Result<AcquisitionPage> {
        self.limits.validate()?;
        rss_acquisition::validate_request(&request)?;
        self.source.revision.validate()?;
        self.source
            .common
            .validate_bounded(&request.bounds, &self.supported_extensions)?;
        request
            .subscription
            .common
            .validate_bounded(&request.bounds, &self.supported_extensions)?;
        request.bounds.text("source.owner_scope", &self.source.owner_scope.0, request.bounds.max_text_bytes)?;
        if self.source.source_ref != request.subscription.source_ref
            || self.source.owner_scope.0.is_empty()
        {
            return Err(Error::Denied);
        }
        if self.source.provider != rss_contracts::Provider::Syndication
            || request.subscription.partition.mode != rss_contracts::AccessMode::PublicFeed
        {
            return Err(Error::Unsupported {
                provider: rss_contracts::Provider::Syndication,
                mode: request.subscription.partition.mode,
            });
        }
        for reference in [&request.subscription.executor_ref, &self.execution_ref] {
            for value in [
                &reference.host_instance_id,
                &reference.owner_kind,
                &reference.native_id,
            ] {
                if value.is_empty() {
                    return Err(invalid("science.reference"));
                }
                request
                    .bounds
                    .text("science.reference", value, request.bounds.max_text_bytes)?;
            }
        }
        if self.source.paused || request.subscription.paused {
            return Err(Error::Cancelled);
        }
        let source = arxiv::source(
            &self.source.original_url,
            &request.bounds,
            self.limits.max_document_entries,
        )?;
        let initial_start = match &source {
            arxiv::Source::Api { start, .. } => *start,
            _ => 0,
        };
        let mut cursor = if let Some(next) = &request.continuation {
            serde_json::from_str::<Cursor>(&next.token).map_err(|_| invalid("science.cursor"))?
        } else {
            Cursor {
                version: 1,
                source: source.url().into(),
                revision: self.source.revision,
                start: initial_start,
                total: None,
                digest: None,
                offset: 0,
                documents: 0,
                seen: BTreeMap::new(),
            }
        };
        if cursor.version != 1
            || cursor.source != source.url()
            || cursor.revision != self.source.revision
            || cursor.documents >= self.limits.max_documents
            || cursor.start < initial_start
            || cursor.start >= 30_000
            || cursor.offset > self.limits.max_document_entries
            || (cursor.offset > 0) != cursor.digest.is_some()
            || cursor
                .digest
                .as_ref()
                .is_some_and(|v| v.len() != 64 || !v.bytes().all(|v| v.is_ascii_hexdigit()))
            || cursor.seen.len()
                > self
                    .limits
                    .max_document_entries
                    .saturating_mul(self.limits.max_documents)
            || matches!(source, arxiv::Source::Daily { .. })
                && (cursor.start != 0 || cursor.documents != 0 || cursor.total.is_some())
        {
            return Err(invalid("science.cursor"));
        }
        for (key, digest) in &cursor.seen {
            request
                .bounds
                .text("science.cursor_key", key, request.bounds.max_text_bytes)?;
            arxiv::paper_id(key)?;
            if digest.len() != 64 || !digest.bytes().all(|v| v.is_ascii_hexdigit()) {
                return Err(invalid("science.cursor_digest"));
            }
        }
        let mut fetches = 0;
        loop {
            if fetches == self.limits.max_fetches_per_operation {
                return Err(Error::LimitExceeded {
                    field: "science.fetches".into(),
                });
            }
            fetches += 1;
            let url = arxiv::request_url(&source, cursor.start)?;
            let response = self
                .fetch
                .fetch(
                    &request,
                    &self.execution_ref,
                    FetchRequest {
                        url: url.clone(),
                        expected_digest: cursor.digest.clone(),
                        max_body_bytes: self.limits.max_body_bytes,
                        timeout_millis: self.limits.timeout_millis,
                        max_redirects: self.limits.max_redirects,
                    },
                )
                .await?;
            if response.final_url != url {
                return Err(Error::StaleResult);
            }
            let FetchResponse {
                status,
                body,
                cached_body,
                observed_at,
                ..
            } = response;
            let bytes = match status {
                200 if cached_body.is_none() => body,
                304 if body.is_empty() => {
                    cached_body.ok_or_else(|| invalid("science.missing_cached_body"))?
                }
                401 | 403 => return Err(Error::Denied),
                404 => return Err(Error::NotFound),
                410 => return Err(Error::Expired),
                429 => return Err(Error::RateLimited { retry_after: None }),
                _ => return Err(Error::Network),
            };
            let document = arxiv::parse(
                &source,
                cursor.start,
                &bytes,
                self.limits.max_body_bytes,
                self.limits.max_document_entries,
                &request.bounds,
            )?;
            if cursor
                .digest
                .as_ref()
                .is_some_and(|v| v != &document.digest)
                || cursor.total.is_some() && cursor.total != document.total
            {
                return Err(Error::StaleResult);
            }
            if cursor.offset > 0 && cursor.offset >= document.entries.len() {
                return Err(invalid("science.cursor_offset"));
            }
            if document.next.is_some_and(|next| next <= cursor.start) {
                return Err(invalid("science.cursor_cycle"));
            }
            let mut seen = cursor.seen.clone();
            let mut selected = vec![];
            let mut end = cursor.offset;
            for evidence in &document.entries {
                let hash = fingerprint(evidence)?;
                if seen
                    .get(&evidence.paper.original_entry_id)
                    .is_some_and(|old| old != &hash)
                {
                    return Err(invalid("science.retargeted_entry"));
                }
            }
            while end < document.entries.len() && selected.len() < request.bounds.max_page_items {
                let evidence = &document.entries[end];
                if seen
                    .insert(
                        evidence.paper.original_entry_id.clone(),
                        fingerprint(evidence)?,
                    )
                    .is_none()
                {
                    selected.push(evidence.clone());
                }
                end += 1;
            }
            let mut next = cursor.clone();
            next.total = document.total;
            next.seen = seen;
            let next = if end < document.entries.len() {
                next.offset = end;
                next.digest = Some(document.digest);
                Some(next)
            } else if let Some(start) = document.next {
                next.start = start;
                next.documents += 1;
                next.offset = 0;
                next.digest = None;
                if next.documents >= self.limits.max_documents {
                    return Err(Error::LimitExceeded {
                        field: "science.documents".into(),
                    });
                }
                Some(next)
            } else {
                None
            };
            let continuation = next
                .as_ref()
                .map(|next| -> Result<_> {
                    let value = Continuation {
                        source_ref: request.subscription.source_ref.clone(),
                        subscription_ref: request.subscription.subscription_ref.clone(),
                        partition: request.subscription.partition.clone(),
                        executor_ref: request.subscription.executor_ref.clone(),
                        generation: request.subscription.generation,
                        token: serde_json::to_string(next)
                            .map_err(|_| invalid("science.cursor"))?,
                    };
                    value.validate_for(&request.subscription, &request.bounds)?;
                    Ok(value)
                })
                .transpose()?;
            if selected.is_empty() {
                if let Some(next) = next {
                    cursor = next;
                    continue;
                }
                if request.continuation.is_none() && !document.verified_empty {
                    return Err(invalid("science.empty_unverified"));
                }
            }
            let mut entries = vec![];
            let mut entry_bytes = 0usize;
            for evidence in selected {
                let prepared = self
                    .canonical
                    .prepare(
                        &request,
                        &self.execution_ref,
                        source.url(),
                        evidence.clone(),
                    )
                    .await?;
                prepared.validate(&request.bounds, &self.supported_extensions)?;
                let mut expected = prepared.observation.clone();
                evidence.entry.populate(&mut expected);
                if prepared.provider_entry_key != evidence.entry.key
                    || prepared.capture.captured_url != evidence.entry.url
                    || prepared.capture.access_partition != request.subscription.partition.scope
                    || prepared.observation != expected
                {
                    return Err(invalid("science.canonical_preparation"));
                }
                entry_bytes = entry_bytes
                    .checked_add(
                        serde_json::to_vec(&prepared)
                            .map_err(|_| invalid("science.page"))?
                            .len(),
                    )
                    .ok_or_else(|| invalid("science.page_bytes"))?;
                if entry_bytes > request.bounds.max_operation_bytes {
                    return Err(Error::LimitExceeded {
                        field: "science.page_bytes".into(),
                    });
                }
                entries.push(prepared);
            }
            let empty = entries.is_empty();
            let terminal = empty && request.continuation.is_some();
            let state = if terminal {
                AcquisitionState::Partial {
                    observed_at,
                    reason_code: rss_contracts::TERMINAL_SOURCE_DOCUMENT.into(),
                }
            } else if empty {
                AcquisitionState::VerifiedEmpty {
                    observed_at,
                    selected_source_identity: source.url().into(),
                }
            } else {
                AcquisitionState::Partial {
                    observed_at,
                    reason_code: if matches!(source, arxiv::Source::Daily { .. }) {
                        "arxiv_current_announcements_only"
                    } else if continuation.is_some() {
                        "arxiv_query_continues"
                    } else {
                        "arxiv_mutable_query_only"
                    }
                    .into(),
                }
            };
            let page = AcquisitionPage {
                subscription_ref: request.subscription.subscription_ref.clone(),
                source_ref: request.subscription.source_ref.clone(),
                partition: request.subscription.partition.clone(),
                executor_ref: request.subscription.executor_ref.clone(),
                generation: request.subscription.generation,
                operation_id: request.operation_id,
                entries,
                state,
                coverage: if empty && !terminal {
                    hsscrap::Coverage::Complete
                } else {
                    hsscrap::Coverage::Partial
                },
                continuation,
            };
            page.validate(
                &request.subscription,
                &request.bounds,
                &self.supported_extensions,
            )?;
            return Ok(page);
        }
    }
}
