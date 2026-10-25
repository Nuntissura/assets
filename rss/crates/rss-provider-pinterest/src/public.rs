use crate::{
    CanonicalEntryPort, DiscoveryPage, FetchRequest, FetchResponse, PinterestProvider,
    PublicFetchPort, invalid, normalize,
};
use rss_contracts::{
    AcquisitionPage, AcquisitionState, Bounds, Continuation, Error, Result, SourceRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

pub fn numeric_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 30 && value.bytes().all(|v| v.is_ascii_digit())
}
fn segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'_' | b'-'))
}
pub fn public_url(value: &str, bounds: &Bounds) -> Result<()> {
    bounds.text("pinterest.url", value, bounds.max_url_bytes)?;
    let url = Url::parse(value).map_err(|_| invalid("pinterest.url"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
        || !matches!(url.host(),Some(url::Host::Domain(host)) if host.contains('.')&&host!="localhost")
    {
        return Err(invalid("pinterest.url"));
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceTarget {
    Discovery(String),
    AllPins(String),
    Board {
        owner: String,
        slug: String,
    },
    Section {
        owner: String,
        board: String,
        section: String,
    },
    Pin(String),
}
pub fn resolve_url(value: &str, bounds: &Bounds) -> Result<SourceTarget> {
    public_url(value, bounds)?;
    let url = Url::parse(value).map_err(|_| invalid("pinterest.source"))?;
    if !matches!(url.host_str(), Some("pinterest.com" | "www.pinterest.com"))
        || url.query().is_some()
    {
        return Err(invalid("pinterest.source"));
    }
    let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
    if parts.len() == 2 && parts[0] == "pin" && numeric_id(parts[1]) {
        return Ok(SourceTarget::Pin(parts[1].into()));
    }
    if parts.iter().any(|v| !segment(v))
        || matches!(
            parts[0],
            "pin" | "search" | "resource" | "login" | "settings" | "ideas"
        )
    {
        return Err(invalid("pinterest.source"));
    }
    let owner = parts[0].to_ascii_lowercase();
    match parts.as_slice() {
        [_] => Ok(SourceTarget::Discovery(owner)),
        [_, "_saved"] => Ok(SourceTarget::Discovery(owner)),
        [_, "pins"] => Ok(SourceTarget::AllPins(owner)),
        [_, slug] => Ok(SourceTarget::Board {
            owner,
            slug: (*slug).into(),
        }),
        [_, board, section] => Ok(SourceTarget::Section {
            owner,
            board: (*board).into(),
            section: (*section).into(),
        }),
        _ => Err(invalid("pinterest.source")),
    }
}
pub fn canonical_source(target: &SourceTarget) -> Result<String> {
    Ok(format!(
        "https://www.pinterest.com/{}/",
        match target {
            SourceTarget::Discovery(owner) => owner.clone(),
            SourceTarget::AllPins(owner) => format!("{owner}/pins"),
            SourceTarget::Board { owner, slug } => format!("{owner}/{slug}"),
            SourceTarget::Section {
                owner,
                board,
                section,
            } => format!("{owner}/{board}/{section}"),
            SourceTarget::Pin(id) => format!("pin/{id}"),
        }
    ))
}
fn resource(name: &str, options: Value) -> Result<String> {
    let mut url = Url::parse(&format!(
        "https://www.pinterest.com/resource/{name}Resource/get/"
    ))
    .map_err(|_| invalid("pinterest.endpoint"))?;
    url.query_pairs_mut()
        .append_pair("data", &json!({"options":options}).to_string())
        .append_pair("source_url", "");
    Ok(url.to_string())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    discovery: bool,
    source: String,
    revision: hsscrap::Revision,
    identity: String,
    bookmarks: Option<Vec<String>>,
    digest: Option<String>,
    offset: usize,
    documents: usize,
    visited: Vec<Option<Vec<String>>>,
    seen: BTreeMap<String, String>,
}
fn fingerprint(value: &impl Serialize) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|_| invalid("pinterest.fingerprint"))?)
    ))
}
impl<F: PublicFetchPort, C: CanonicalEntryPort> PinterestProvider<F, C> {
    fn admission(
        &self,
        request: &SourceRequest,
        discovery: bool,
    ) -> Result<(SourceTarget, Cursor)> {
        self.limits.validate()?;
        request.bounds.validate()?;
        request.subscription.partition.validate(&request.bounds)?;
        if request.subscription.partition.connection_ref.is_some() {
            return Err(Error::Denied);
        }
        request.subscription.generation.validate()?;
        self.source.revision.validate()?;
        self.source
            .common
            .validate_bounded(&request.bounds, &self.supported_extensions)?;
        request
            .subscription
            .common
            .validate_bounded(&request.bounds, &self.supported_extensions)?;
        for reference in [
            &request.subscription.subscription_ref,
            &request.subscription.source_ref,
            &request.subscription.executor_ref,
            &self.execution_ref,
        ] {
            for value in [
                &reference.host_instance_id,
                &reference.owner_kind,
                &reference.native_id,
            ] {
                if value.is_empty() {
                    return Err(invalid("pinterest.reference"));
                }
                request
                    .bounds
                    .text("pinterest.reference", value, request.bounds.max_text_bytes)?;
            }
        }
        if let Some(next) = &request.continuation {
            next.validate_for(&request.subscription, &request.bounds)?;
        }
        if serde_json::to_vec(request)
            .map_err(|_| invalid("pinterest.request"))?
            .len()
            > request.bounds.max_operation_bytes
        {
            return Err(Error::LimitExceeded {
                field: "pinterest.request".into(),
            });
        }
        request.bounds.text("source.owner_scope", &self.source.owner_scope.0, request.bounds.max_text_bytes)?;
        if self.source.source_ref != request.subscription.source_ref
            || self.source.owner_scope.0.is_empty()
        {
            return Err(Error::Denied);
        }
        let target = resolve_url(&self.source.original_url, &request.bounds)?;
        if matches!(target, SourceTarget::Pin(_)) && request.continuation.is_some() {
            return Err(invalid("pinterest.pin_continuation"));
        }
        let mode = if matches!(
            target,
            SourceTarget::Discovery(_) | SourceTarget::AllPins(_)
        ) {
            rss_contracts::AccessMode::PublicProfile
        } else {
            rss_contracts::AccessMode::PublicBoard
        };
        if self.source.provider != rss_contracts::Provider::Pinterest
            || request.subscription.partition.mode != mode
        {
            return Err(Error::Unsupported {
                provider: rss_contracts::Provider::Pinterest,
                mode: request.subscription.partition.mode,
            });
        }
        if discovery != matches!(target, SourceTarget::Discovery(_)) {
            return Err(invalid("pinterest.intent"));
        }
        if self.source.paused || request.subscription.paused {
            return Err(Error::Cancelled);
        }
        let source = canonical_source(&target)?;
        let cursor = if let Some(next) = &request.continuation {
            serde_json::from_str::<Cursor>(&next.token).map_err(|_| invalid("pinterest.cursor"))?
        } else {
            Cursor {
                version: 1,
                discovery,
                source: source.clone(),
                revision: self.source.revision,
                identity: String::new(),
                bookmarks: None,
                digest: None,
                offset: 0,
                documents: 0,
                visited: vec![],
                seen: BTreeMap::new(),
            }
        };
        if cursor.version != 1
            || cursor.discovery != discovery
            || cursor.source != source
            || cursor.revision != self.source.revision
            || cursor.documents >= self.limits.max_documents
            || cursor.visited.len() != cursor.documents
            || cursor.visited.iter().collect::<BTreeSet<_>>().len() != cursor.visited.len()
            || cursor.visited.contains(&cursor.bookmarks)
            || cursor.offset > self.limits.max_document_entries
            || (cursor.offset > 0) != cursor.digest.is_some()
            || cursor
                .digest
                .as_ref()
                .is_some_and(|v| v.len() != 64 || !v.bytes().all(|c| c.is_ascii_hexdigit()))
            || cursor.seen.len()
                > self
                    .limits
                    .max_documents
                    .saturating_mul(self.limits.max_document_entries)
            || cursor.seen.iter().any(|(id, digest)| {
                !numeric_id(id)
                    || digest.len() != 64
                    || !digest.bytes().all(|v| v.is_ascii_hexdigit())
            })
        {
            return Err(invalid("pinterest.cursor"));
        }
        for bookmarks in cursor
            .visited
            .iter()
            .chain(std::iter::once(&cursor.bookmarks))
            .flatten()
        {
            if bookmarks.is_empty() || bookmarks.len() > request.bounds.max_collection_items {
                return Err(invalid("pinterest.cursor_bookmarks"));
            }
            let mut seen = BTreeSet::new();
            for value in bookmarks {
                request.bounds.text(
                    "pinterest.cursor_bookmark",
                    value,
                    request.bounds.max_text_bytes,
                )?;
                if value.is_empty()
                    || value.chars().any(char::is_control)
                    || value == "-end-"
                    || value.starts_with("Y2JOb25lO")
                    || !seen.insert(value)
                {
                    return Err(invalid("pinterest.cursor_bookmark"));
                }
            }
        }
        Ok((target, cursor))
    }
    async fn get(
        &self,
        request: &SourceRequest,
        url: String,
        digest: Option<String>,
        fetches: &mut usize,
    ) -> Result<(Vec<u8>, hsscrap::Instant)> {
        if *fetches == self.limits.max_fetches_per_operation {
            return Err(Error::LimitExceeded {
                field: "pinterest.fetches".into(),
            });
        }
        *fetches += 1;
        let expected = FetchRequest {
            url,
            expected_digest: digest,
            max_body_bytes: self.limits.max_body_bytes,
            timeout_millis: self.limits.timeout_millis,
        };
        let response = self
            .fetch
            .fetch(request, &self.execution_ref, expected.clone())
            .await?;
        if response.final_url != expected.url {
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
                cached_body.ok_or_else(|| invalid("pinterest.missing_cached_body"))?
            }
            401 | 403 => return Err(Error::Denied),
            404 => return Err(Error::NotFound),
            410 => return Err(Error::Expired),
            429 => return Err(Error::RateLimited { retry_after: None }),
            _ => return Err(Error::Network),
        };
        if bytes.len() > self.limits.max_body_bytes {
            return Err(Error::LimitExceeded {
                field: "pinterest.body".into(),
            });
        }
        if bytes.first() != Some(&b'{')
            && std::str::from_utf8(&bytes).is_ok_and(|v| v.trim_start().starts_with('<'))
        {
            return Err(Error::Challenged);
        }
        Ok((bytes, observed_at))
    }
    fn continuation(&self, request: &SourceRequest, cursor: &Cursor) -> Result<Continuation> {
        let next = Continuation {
            source_ref: request.subscription.source_ref.clone(),
            subscription_ref: request.subscription.subscription_ref.clone(),
            partition: request.subscription.partition.clone(),
            executor_ref: request.subscription.executor_ref.clone(),
            generation: request.subscription.generation,
            token: serde_json::to_string(cursor).map_err(|_| invalid("pinterest.cursor"))?,
        };
        next.validate_for(&request.subscription, &request.bounds)?;
        Ok(next)
    }
    fn advance(
        &self,
        cursor: &Cursor,
        digest: &str,
        length: usize,
        end: usize,
        next: Option<Vec<String>>,
    ) -> Result<Option<Cursor>> {
        if cursor.digest.as_ref().is_some_and(|old| old != digest) {
            return Err(Error::StaleResult);
        }
        if cursor.offset > 0 && cursor.offset >= length {
            return Err(invalid("pinterest.cursor_offset"));
        }
        if next.as_ref().is_some_and(|value| {
            cursor.bookmarks.as_ref() == Some(value)
                || cursor.visited.contains(&Some(value.clone()))
        }) {
            return Err(invalid("pinterest.cursor_cycle"));
        }
        let mut result = cursor.clone();
        if end < length {
            result.offset = end;
            result.digest = Some(digest.into());
            Ok(Some(result))
        } else if next.is_some() {
            result.visited.push(cursor.bookmarks.clone());
            result.documents += 1;
            if result.documents >= self.limits.max_documents {
                return Err(Error::LimitExceeded {
                    field: "pinterest.documents".into(),
                });
            }
            result.bookmarks = next;
            result.offset = 0;
            result.digest = None;
            Ok(Some(result))
        } else {
            Ok(None)
        }
    }
    pub async fn discover_boards(&self, request: SourceRequest) -> Result<DiscoveryPage> {
        let (target, mut cursor) = self.admission(&request, true)?;
        let SourceTarget::Discovery(owner) = target else {
            return Err(invalid("pinterest.discovery"));
        };
        let mut fetches = 0;
        loop {
            let mut options = json!({"username":owner,"sort":"last_pinned_to","field_set_key":"profile_grid_item","filter_stories":false,"page_size":25,"include_archived":true});
            if let Some(bookmarks) = &cursor.bookmarks {
                options["bookmarks"] = json!(bookmarks);
            }
            let (bytes, observed_at) = self
                .get(
                    &request,
                    resource("Boards", options)?,
                    cursor.digest.clone(),
                    &mut fetches,
                )
                .await?;
            let (sources, next, digest) = normalize::discovered_boards(
                &bytes,
                self.limits.max_body_bytes,
                self.limits.max_document_entries,
                &request.bounds,
            )?;
            if sources.iter().any(|board| board.owner != owner) {
                return Err(Error::StaleResult);
            }
            let mut selected = vec![];
            let mut end = cursor.offset;
            let mut seen = cursor.seen.clone();
            for source in &sources {
                let hash = fingerprint(source)?;
                if seen.get(&source.id).is_some_and(|old| old != &hash) {
                    return Err(invalid("pinterest.retargeted_board"));
                }
            }
            while end < sources.len() && selected.len() < request.bounds.max_page_items {
                let source = &sources[end];
                if seen
                    .insert(source.id.clone(), fingerprint(source)?)
                    .is_none()
                {
                    selected.push(source.clone());
                }
                end += 1;
            }
            let next = self
                .advance(&cursor, &digest, sources.len(), end, next)?
                .map(|mut value| {
                    value.seen = seen;
                    value
                });
            if selected.is_empty() {
                if let Some(next) = next {
                    cursor = next;
                    continue;
                }
            }
            return Ok(DiscoveryPage {
                sources: selected,
                observed_at,
                continuation: next
                    .as_ref()
                    .map(|value| self.continuation(&request, value))
                    .transpose()?,
                coverage: hsscrap::Coverage::Partial,
            });
        }
    }
    pub(crate) async fn acquire_page(&self, request: SourceRequest) -> Result<AcquisitionPage> {
        let (target, mut cursor) = self.admission(&request, false)?;
        let mut fetches = 0;
        let (identity, count, resource_name, mut options) = match &target {
            SourceTarget::Pin(id) => (
                format!("pin/{id}"),
                None,
                "Pin",
                json!({"id":id,"field_set_key":"detailed"}),
            ),
            SourceTarget::AllPins(owner) => (
                format!("user/{owner}/pins"),
                None,
                "UserPins",
                json!({"username":owner,"is_own_profile_pins":false,"field_set_key":"grid_item","pin_filter":null}),
            ),
            SourceTarget::Board { owner, slug }
            | SourceTarget::Section {
                owner, board: slug, ..
            } => {
                let (bytes, _) = self
                    .get(
                        &request,
                        resource(
                            "Board",
                            json!({"username":owner,"slug":slug,"field_set_key":"detailed"}),
                        )?,
                        None,
                        &mut fetches,
                    )
                    .await?;
                let board = normalize::board(
                    &bytes,
                    owner,
                    slug,
                    self.limits.max_body_bytes,
                    &request.bounds,
                )?;
                if let SourceTarget::Section { section, .. } = &target {
                    let (bytes, _) = self
                        .get(
                            &request,
                            resource(
                                "BoardSection",
                                json!({"username":owner,"board_slug":slug,"section_slug":section}),
                            )?,
                            None,
                            &mut fetches,
                        )
                        .await?;
                    let section = normalize::section(&bytes, &board, self.limits.max_body_bytes)?;
                    (
                        format!("board/{}/section/{}", board.source.id, section.id),
                        section.pin_count,
                        "BoardSectionPins",
                        json!({"section_id":section.id}),
                    )
                } else {
                    (
                        format!("board/{}", board.source.id),
                        board.pin_count,
                        "BoardFeed",
                        json!({"board_id":board.source.id,"field_set_key":"react_grid_pin","prepend":false,"bookmarks":null}),
                    )
                }
            }
            _ => return Err(invalid("pinterest.intent")),
        };
        if !cursor.identity.is_empty() && cursor.identity != identity {
            return Err(Error::StaleResult);
        }
        cursor.identity = identity.clone();
        loop {
            if let Some(bookmarks) = &cursor.bookmarks {
                options["bookmarks"] = json!(bookmarks);
            }
            let (bytes, observed_at) = self
                .get(
                    &request,
                    resource(resource_name, options.clone())?,
                    cursor.digest.clone(),
                    &mut fetches,
                )
                .await?;
            let document = if let SourceTarget::Pin(id) = &target {
                normalize::pin(&bytes, id, self.limits.max_body_bytes, &request.bounds)?
            } else {
                normalize::page(
                    &bytes,
                    self.limits.max_body_bytes,
                    self.limits.max_document_entries,
                    &request.bounds,
                )?
            };
            let mut seen = cursor.seen.clone();
            let mut selected = vec![];
            let mut end = cursor.offset;
            for entry in &document.entries {
                let mut stable = entry.clone();
                stable.document_digest.clear();
                let hash = fingerprint(&stable)?;
                if seen.get(&entry.id).is_some_and(|old| old != &hash) {
                    return Err(invalid("pinterest.retargeted_pin"));
                }
            }
            while end < document.entries.len() && selected.len() < request.bounds.max_page_items {
                let evidence = &document.entries[end];
                let mut stable = evidence.clone();
                stable.document_digest.clear();
                if seen
                    .insert(evidence.id.clone(), fingerprint(&stable)?)
                    .is_none()
                {
                    selected.push(evidence.clone());
                }
                end += 1;
            }
            let next = self
                .advance(
                    &cursor,
                    &document.digest,
                    document.entries.len(),
                    end,
                    document.next,
                )?
                .map(|mut value| {
                    value.seen = seen;
                    value
                });
            let continuation = next
                .as_ref()
                .map(|value| self.continuation(&request, value))
                .transpose()?;
            if selected.is_empty() {
                if let Some(next) = next {
                    cursor = next;
                    continue;
                }
                if request.continuation.is_none() && count != Some(0) {
                    return Err(invalid("pinterest.empty_unverified"));
                }
            }
            let mut entries = vec![];
            let mut entry_bytes = 0usize;
            for evidence in selected {
                let entry = self
                    .canonical
                    .prepare(&request, &self.execution_ref, evidence.clone())
                    .await?;
                entry.validate(&request.bounds, &self.supported_extensions)?;
                let mut expected = entry.observation.clone();
                evidence.populate(&mut expected);
                if entry.provider_entry_key != evidence.id
                    || entry.capture.captured_url != evidence.url()
                    || entry.capture.access_partition != request.subscription.partition.scope
                    || entry.observation != expected
                {
                    return Err(invalid("pinterest.canonical_preparation"));
                }
                entry_bytes = entry_bytes
                    .checked_add(
                        serde_json::to_vec(&entry)
                            .map_err(|_| invalid("pinterest.page"))?
                            .len(),
                    )
                    .ok_or_else(|| invalid("pinterest.page_bytes"))?;
                if entry_bytes > request.bounds.max_operation_bytes {
                    return Err(Error::LimitExceeded {
                        field: "pinterest.page_bytes".into(),
                    });
                }
                entries.push(entry);
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
                    selected_source_identity: format!("pinterest/{identity}"),
                }
            } else {
                AcquisitionState::Partial {
                    observed_at,
                    reason_code: if continuation.is_some() {
                        "pinterest_public_page_continues"
                    } else {
                        "pinterest_current_public_traversal_only"
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
