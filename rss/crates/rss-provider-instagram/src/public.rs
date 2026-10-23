use std::collections::BTreeMap;
use rss_contracts::{AccessMode, AcquisitionPage, AcquisitionState, Bounds, Continuation, Error, Result, SourceRequest};
use serde::{Deserialize, Serialize};
use url::{Url, Host};
use crate::{CanonicalEntryPort, FetchRequest, FetchResponse, InstagramProvider, Method, PublicFetchPort, invalid, normalize};

pub fn numeric_id(value: &str) -> bool { !value.is_empty() && value.len() <= 40 && value.bytes().all(|v| v.is_ascii_digit()) && !value.starts_with('0') }
pub fn username(value: &str) -> bool { !value.is_empty() && value.len() <= 30 && value.bytes().all(|v| v.is_ascii_alphanumeric() || v == b'_' || v == b'.') }
pub fn shortcode(value: &str) -> bool { !value.is_empty() && value.len() <= 32 && value.bytes().all(|v| v.is_ascii_alphanumeric() || v == b'_' || v == b'-') }
pub fn media_url(value: &str, bounds: &Bounds) -> Result<()> {
    bounds.text("instagram.media_url",value,bounds.max_url_bytes)?;
    let url = Url::parse(value).map_err(|_| invalid("instagram.media_url"))?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.port().is_some() || url.fragment().is_some()
        || !matches!(url.host(),Some(Host::Domain(host)) if host.contains('.') && host != "localhost" && !host.ends_with(".localhost") && !host.ends_with(".local")) {
        return Err(invalid("instagram.media_url"));
    }
    Ok(())
}
fn source(value: &str, mode: AccessMode, bounds: &Bounds) -> Result<(String,String)> {
    bounds.text("instagram.source_url",value,bounds.max_url_bytes)?;
    let mut url = Url::parse(value).map_err(|_| invalid("instagram.source_url"))?;
    if url.scheme() != "https" || !matches!(url.host_str(),Some("instagram.com" | "www.instagram.com"))
        || !url.username().is_empty() || url.password().is_some() || url.port().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("instagram.source_url"));
    }
    let path: Vec<_> = url.path().trim_end_matches('/').trim_start_matches('/').split('/').collect();
    if !username(path[0]) || match mode { AccessMode::PublicProfile => path.len() != 1,
        AccessMode::PublicReels => path.len() != 2 || path[1] != "reels", _ => true } { return Err(invalid("instagram.source_mode")); }
    let name = path[0].to_ascii_lowercase();
    url.set_host(Some("www.instagram.com")).map_err(|_| invalid("instagram.source_url"))?;
    url.set_path(&format!("/{name}/{}",if mode == AccessMode::PublicReels { "reels/" } else { "" }));
    Ok((url.to_string(),name))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    source_url: String,
    source_revision: hsscrap::Revision,
    mode: AccessMode,
    account_id: String,
    page: Option<String>,
    digest: Option<String>,
    offset: usize,
    completed_documents: usize,
    visited: Vec<Option<String>>,
    seen: BTreeMap<String,String>,
}
fn bounded_request(request: &SourceRequest, execution: &hsscrap::HostRef, supported: &std::collections::BTreeSet<String>) -> Result<()> {
    request.bounds.validate()?;
    request.subscription.partition.validate(&request.bounds)?;
    request.subscription.generation.validate()?;
    request.subscription.common.validate_bounded(&request.bounds,supported)?;
    for reference in [&request.subscription.subscription_ref,&request.subscription.source_ref,&request.subscription.executor_ref,execution] {
        for value in [&reference.host_instance_id,&reference.owner_kind,&reference.native_id] {
            if value.is_empty() { return Err(invalid("instagram.request_reference")); }
            request.bounds.text("instagram.request_reference",value,request.bounds.max_text_bytes)?;
        }
    }
    if let Some(cursor) = &request.continuation { cursor.validate_for(&request.subscription,&request.bounds)?; }
    if serde_json::to_vec(request).map_err(|_| invalid("instagram.request"))?.len() > request.bounds.max_operation_bytes {
        return Err(Error::LimitExceeded { field: "instagram.request".into() });
    }
    Ok(())
}
fn body(response: FetchResponse, expected: &FetchRequest) -> Result<(Vec<u8>, hsscrap::Instant)> {
    if response.final_url.contains("/accounts/login") { return Err(Error::ReconnectRequired); }
    if response.final_url.contains("/challenge/") || response.final_url.contains("/checkpoint/") { return Err(Error::Challenged); }
    if response.final_url != expected.url { return Err(Error::StaleResult); }
    let bytes = match response.status {
        200 if response.cached_body.is_none() => response.body,
        304 if response.body.is_empty() => response.cached_body.ok_or_else(|| invalid("instagram.missing_cached_body"))?,
        401 => return Err(Error::ReconnectRequired),
        403 => return Err(Error::Denied),
        404 => return Err(Error::NotFound),
        410 => return Err(Error::Expired),
        429 => return Err(Error::RateLimited { retry_after: None }),
        _ => return Err(Error::Network),
    };
    if bytes.len() > expected.max_body_bytes { return Err(Error::LimitExceeded { field: "instagram.body".into() }); }
    Ok((bytes,response.observed_at))
}
impl<F: PublicFetchPort, C: CanonicalEntryPort> InstagramProvider<F,C> {
    fn fetch_request(&self, method: Method, url: String, form: Vec<(String,String)>, digest: Option<String>) -> FetchRequest {
        FetchRequest { method,url,form,expected_digest:digest,max_body_bytes:self.limits.max_body_bytes,timeout_millis:self.limits.timeout_millis }
    }
    pub(crate) async fn acquire_page(&self, request: SourceRequest) -> Result<AcquisitionPage> {
        self.limits.validate()?;
        bounded_request(&request,&self.execution_ref,&self.supported_extensions)?;
        self.source.revision.validate()?;
        self.source.common.validate_bounded(&request.bounds,&self.supported_extensions)?;
        request.bounds.text("source.owner_scope", &self.source.owner_scope.0, request.bounds.max_text_bytes)?;
        if self.source.source_ref != request.subscription.source_ref || self.source.owner_scope.0.is_empty() { return Err(Error::Denied); }
        let mode = request.subscription.partition.mode;
        if self.source.provider != rss_contracts::Provider::Instagram || !matches!(mode,AccessMode::PublicProfile | AccessMode::PublicReels) {
            return Err(Error::Unsupported { provider:rss_contracts::Provider::Instagram, mode });
        }
        if self.source.paused || request.subscription.paused { return Err(Error::Cancelled); }
        let (source_url,name) = source(&self.source.original_url,mode,&request.bounds)?;
        let mut cursor: Cursor = if let Some(continuation) = &request.continuation {
            serde_json::from_str(&continuation.token).map_err(|_| invalid("instagram.cursor"))?
        } else { Cursor { version:1,source_url:source_url.clone(),source_revision:self.source.revision,mode,account_id:String::new(),page:None,digest:None,offset:0,
            completed_documents:0,visited:vec![],seen:BTreeMap::new() } };
        if cursor.version != 1 || cursor.source_url != source_url || cursor.source_revision != self.source.revision || cursor.mode != mode
            || cursor.completed_documents >= self.limits.max_documents || cursor.visited.len() != cursor.completed_documents
            || cursor.offset > self.limits.max_document_entries || (cursor.offset > 0) != cursor.digest.is_some()
            || cursor.seen.len() > self.limits.max_documents.saturating_mul(self.limits.max_document_entries)
            || cursor.visited.iter().collect::<std::collections::BTreeSet<_>>().len() != cursor.visited.len()
            || cursor.visited.contains(&cursor.page)
            || cursor.digest.as_ref().is_some_and(|v| v.len() != 64 || !v.bytes().all(|v| v.is_ascii_hexdigit()))
            || cursor.seen.iter().any(|(key,code)| !numeric_id(key) || !shortcode(code)) { return Err(invalid("instagram.cursor")); }
        let mut identity_url = Url::parse("https://www.instagram.com/api/v1/users/web_profile_info/").map_err(|_| invalid("instagram.endpoint"))?;
        identity_url.query_pairs_mut().append_pair("username",&name);
        let identity_request = self.fetch_request(Method::Get,identity_url.to_string(),vec![],None);
        let identity_response = self.fetch.fetch(&request,&self.execution_ref,identity_request.clone()).await?;
        let (identity_bytes,_) = body(identity_response,&identity_request)?;
        let account = normalize::account(&identity_bytes,&name,self.limits.max_body_bytes,&request.bounds)?;
        if !cursor.account_id.is_empty() && cursor.account_id != account.id { return Err(Error::StaleResult); }
        cursor.account_id = account.id.clone();
        let mut fetches = 1;
        loop {
            if fetches == self.limits.max_fetches_per_operation { return Err(Error::LimitExceeded { field: "instagram.fetches".into() }); }
            fetches += 1;
            let page_request = if mode == AccessMode::PublicProfile {
                let mut url = Url::parse(&format!("https://www.instagram.com/api/v1/feed/user/{}/",account.id)).map_err(|_| invalid("instagram.endpoint"))?;
                url.query_pairs_mut().append_pair("count","30");
                if let Some(page) = &cursor.page { url.query_pairs_mut().append_pair("max_id",page); }
                self.fetch_request(Method::Get,url.to_string(),vec![],cursor.digest.clone())
            } else {
                let mut form = vec![("target_user_id".into(),account.id.clone()),("page_size".into(),"50".into()),("include_feed_video".into(),"true".into())];
                if let Some(page) = &cursor.page { form.push(("max_id".into(),page.clone())); }
                self.fetch_request(Method::Post,"https://www.instagram.com/api/v1/clips/user/".into(),form,cursor.digest.clone())
            };
            let response = self.fetch.fetch(&request,&self.execution_ref,page_request.clone()).await?;
            let (bytes,observed_at) = body(response,&page_request)?;
            let document = normalize::document(&bytes,mode,&account,self.limits.max_body_bytes,self.limits.max_document_entries,&request.bounds)?;
            let observed_seconds = chrono::DateTime::parse_from_rfc3339(observed_at.as_str()).map_err(|_| invalid("instagram.observed_at"))?.timestamp();
            if document.entries.iter().flat_map(|entry| &entry.media).any(|media| media.expires_at_unix.is_some_and(|expiry| expiry <= observed_seconds.max(0) as u64)) {
                return Err(Error::Expired);
            }
            if cursor.digest.as_ref().is_some_and(|digest| digest != &document.digest) { return Err(Error::StaleResult); }
            if cursor.offset > 0 && cursor.offset >= document.entries.len() { return Err(invalid("instagram.cursor_offset")); }
            if document.next.as_ref().is_some_and(|next| cursor.page.as_ref() == Some(next) || cursor.visited.contains(&Some(next.clone()))) {
                return Err(invalid("instagram.cursor_cycle"));
            }
            // Check every entry, including entries outside the selected bounded output slice, before owner effects.
            for entry in &document.entries {
                if cursor.seen.get(&entry.key).is_some_and(|code| code != &entry.shortcode) { return Err(invalid("instagram.retargeted_id")); }
            }
            let mut next = cursor.clone();
            let mut selected = vec![];
            let mut end = cursor.offset;
            while end < document.entries.len() && selected.len() < request.bounds.max_page_items {
                let entry = &document.entries[end];
                if !next.seen.contains_key(&entry.key) { next.seen.insert(entry.key.clone(),entry.shortcode.clone()); selected.push(entry.clone()); }
                end += 1;
            }
            let next = if end < document.entries.len() { next.offset = end; next.digest = Some(document.digest.clone()); Some(next) }
                else if let Some(page) = document.next {
                    next.visited.push(cursor.page.clone()); next.completed_documents += 1;
                    if next.completed_documents >= self.limits.max_documents { return Err(Error::LimitExceeded { field: "instagram.documents".into() }); }
                    next.page = Some(page); next.offset = 0; next.digest = None; Some(next)
                } else { None };
            let continuation = next.as_ref().map(|cursor| -> Result<_> {
                let continuation = Continuation { source_ref:request.subscription.source_ref.clone(),subscription_ref:request.subscription.subscription_ref.clone(),
                    partition:request.subscription.partition.clone(),executor_ref:request.subscription.executor_ref.clone(),generation:request.subscription.generation,
                    token:serde_json::to_string(cursor).map_err(|_| invalid("instagram.cursor"))? };
                continuation.validate_for(&request.subscription,&request.bounds)?;
                Ok(continuation)
            }).transpose()?;
            if selected.is_empty() {
                if let Some(next) = next { cursor = next; continue; }
                if request.continuation.is_none() && !(mode == AccessMode::PublicProfile && account.media_count == Some(0) && document.entries.is_empty()) {
                    return Err(invalid("instagram.empty_unverified"));
                }
            }
            let mut entries = vec![];
            let mut entry_bytes = 0usize;
            for evidence in selected {
                let prepared = self.canonical.prepare(&request,&self.execution_ref,evidence.clone()).await?;
                prepared.validate(&request.bounds,&self.supported_extensions)?;
                let mut expected = prepared.observation.clone(); evidence.populate(&mut expected);
                if prepared.provider_entry_key != evidence.key || prepared.capture.captured_url != evidence.url
                    || prepared.capture.access_partition != request.subscription.partition.scope || prepared.observation != expected { return Err(invalid("instagram.canonical_preparation")); }
                entry_bytes = entry_bytes.checked_add(serde_json::to_vec(&prepared).map_err(|_| invalid("instagram.entry"))?.len()).ok_or_else(|| invalid("instagram.entry_bytes"))?;
                if entry_bytes > request.bounds.max_operation_bytes { return Err(Error::LimitExceeded { field: "instagram.page_bytes".into() }); }
                entries.push(prepared);
            }
            let empty = entries.is_empty();
            let terminal = empty && request.continuation.is_some();
            let state = if terminal { AcquisitionState::Partial { observed_at,reason_code:rss_contracts::TERMINAL_SOURCE_DOCUMENT.into() } }
                else if empty { AcquisitionState::VerifiedEmpty { observed_at,selected_source_identity:format!("instagram/account/{}/profile",account.id) } }
                else { AcquisitionState::Partial { observed_at,reason_code:if continuation.is_some() { "instagram_page_continues" } else { "instagram_current_traversal_only" }.into() } };
            let page = AcquisitionPage { subscription_ref:request.subscription.subscription_ref.clone(),source_ref:request.subscription.source_ref.clone(),
                partition:request.subscription.partition.clone(),executor_ref:request.subscription.executor_ref.clone(),generation:request.subscription.generation,
                operation_id:request.operation_id,entries,state,coverage:if empty && !terminal { hsscrap::Coverage::Complete } else { hsscrap::Coverage::Partial },continuation };
            page.validate(&request.subscription,&request.bounds,&self.supported_extensions)?;
            return Ok(page);
        }
    }
}
