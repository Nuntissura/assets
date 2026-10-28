use super::*;
use rss_provider_instagram::connected::{FollowingContext, FollowingDocumentKind, FollowingFetchPort,
    FollowingFetchRequest, FollowingFetchResponse, FollowingInstagramProvider};
use rss_contracts::CaptureIntent;

const ROOT: &str = "xdt_api__v1__feed__timeline__connection";
fn post(n: u64) -> Value {
    let mut value = item(n);
    value["user"] = json!({"pk":"123456","username":"fixture_creator"});
    if n == 2 { value["media_type"] = json!(2); value["video_versions"] = member(n, true)["video_versions"].clone(); }
    if n == 3 {
        value["media_type"] = json!(8);
        value["carousel_media"] = json!((100..118).map(|id| member(id, false)).collect::<Vec<_>>());
        value["carousel_media_count"] = json!(18);
    }
    value
}
fn edge(value: Value) -> Value { json!({"node":{"__typename":"XDTFeedItem","media":value}}) }
fn connection(edges: Vec<Value>, next: Option<&str>) -> Value {
    json!({"pagination_source":"following","edges":edges,
        "page_info":{"has_next_page":next.is_some(),"end_cursor":next,"has_previous_page":false,"start_cursor":null}})
}
fn html(value: Value) -> Vec<u8> {
    let result = json!({"data":{(ROOT):value}});
    let inner = json!({"__bbox":{"result":result}});
    let outer = json!({"__bbox":{"require":[[0,0,0,[0,inner]]]}});
    format!("<html><script type=\"application/json\">{}</script></html>",
        json!({"require":[[0,0,0,[outer]]]})).into_bytes()
}
struct FollowingReply { kind: FollowingDocumentKind, status: u16, bytes: Vec<u8>, cache: Option<Vec<u8>>, url: String }
fn reply(kind: FollowingDocumentKind, bytes: Vec<u8>) -> FollowingReply {
    FollowingReply { kind, status: 200, bytes, cache: None, url: kind.url().into() }
}
fn initial(value: Value) -> FollowingReply { reply(FollowingDocumentKind::Html, html(value)) }
fn graphql(value: Value) -> FollowingReply {
    let mut response = reply(FollowingDocumentKind::Graphql, serde_json::to_vec(&json!({"data":{(ROOT):value}})).unwrap());
    response.url = "https://www.instagram.com/graphql/query".into();
    response
}
struct FollowingFetch { replies: Mutex<VecDeque<FollowingReply>>, requests: Mutex<Vec<FollowingFetchRequest>>,
    checks: AtomicU64, generation: AtomicU64, reject: Mutex<Option<(u64, rss_contracts::Error)>> }
impl FollowingFetchPort for FollowingFetch {
    fn current<'a>(&'a self, request: &'a SourceRequest, _: &'a HostRef, context: &'a FollowingContext) -> Pending<'a, ()> {
        Box::pin(async move {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if request.subscription.partition.scope.0 != "scope" || context.authorization.external_identity != "999999" {
                return Err(rss_contracts::Error::Denied);
            }
            if context.authorization.lease_generation.0 != self.generation.load(Ordering::SeqCst) { return Err(rss_contracts::Error::StaleResult); }
            if let Some((at, error)) = &*self.reject.lock().unwrap() { if *at == check { return Err(error.clone()); } }
            Ok(())
        })
    }
    fn fetch_following<'a>(&'a self, _: &'a SourceRequest, _: &'a HostRef, _: &'a FollowingContext, request: FollowingFetchRequest) -> Pending<'a, FollowingFetchResponse> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            let value = self.replies.lock().unwrap().pop_front().ok_or(rss_contracts::Error::Network)?;
            Ok(FollowingFetchResponse { kind: value.kind, response: FetchResponse { final_url: value.url,
                status: value.status, body: value.bytes, cached_body: value.cache, observed_at: at() } })
        })
    }
}
#[derive(Default)]
struct FollowingCanonical { inner: Canonical, evidence: Mutex<Vec<EntryEvidence>> }
impl CanonicalEntryPort for FollowingCanonical {
    fn prepare<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, evidence: EntryEvidence) -> Pending<'a, ObservedEntry> {
        self.evidence.lock().unwrap().push(evidence.clone()); self.inner.prepare(request, execution, evidence)
    }
}
fn following(replies: Vec<FollowingReply>) -> FollowingInstagramProvider<FollowingFetch, FollowingCanonical> {
    let base = authorized(AccessMode::PublicProfile, vec![]);
    let mut source = base.source; source.original_url = "https://www.instagram.com/?variant=following".into();
    FollowingInstagramProvider { authorization: FollowingContext { authorization: base.authorization,
        capture_intent: CaptureIntent::FollowSource { original_url: source.original_url.clone(), provider: Provider::Instagram,
            partition: selected(AccessMode::Following, 1).subscription.partition } },
        source, execution_ref: base.execution_ref, limits: base.limits, supported_extensions: base.supported_extensions,
        fetch: FollowingFetch { replies: Mutex::new(replies.into()), requests: Mutex::new(vec![]), checks: AtomicU64::new(0),
            generation: AtomicU64::new(1), reject: Mutex::new(None) }, canonical: FollowingCanonical::default() }
}
fn selected_following(n: u64) -> SourceRequest { selected(AccessMode::Following, n) }

#[test]
fn following_initial_html_and_graphql_keep_selected_stream_and_media_authors() {
    let mut other = post(2); other["user"] = json!({"pk":"654321","username":"fixture_other"}); other["id"] = json!("2_654321");
    let adapter = following(vec![initial(connection(vec![edge(post(1)), edge(other)], Some("synthetic-next"))),
        graphql(connection(vec![edge(post(1)), edge(post(3))], Some("synthetic-last"))), graphql(connection(vec![edge(post(4))], None))]);
    let first = run(adapter.acquire(selected_following(1))).unwrap();
    assert_eq!(first.entries.len(), 2); assert_eq!(first.partition.mode, AccessMode::Following); assert_eq!(first.coverage, Coverage::Partial);
    let mut resume = selected_following(2); resume.continuation = first.continuation;
    let second = run(adapter.acquire(resume)).unwrap(); assert_eq!(second.entries[0].provider_entry_key, "3");
    let mut resume = selected_following(3); resume.continuation = second.continuation;
    let last = run(adapter.acquire(resume)).unwrap(); assert_eq!(last.entries[0].provider_entry_key, "4"); assert!(last.continuation.is_none());
    let inputs = adapter.canonical.evidence.lock().unwrap(); assert_eq!(inputs[1].author, "fixture_other"); assert_eq!(inputs[1].account_id, "654321");
    assert_eq!(inputs[1].media[0].kind, MediaKind::Video); assert_eq!(inputs[2].media.len(), 18);
    assert_eq!(inputs[2].media[0].provider_media_id, "100"); assert_eq!(inputs[2].media[17].provider_media_id, "117");
    let requests = adapter.fetch.requests.lock().unwrap(); assert_eq!(requests[0].url(), "https://www.instagram.com/?variant=following");
    assert_eq!(requests[0].kind, FollowingDocumentKind::Html); assert_eq!(requests[0].after, None);
    assert_eq!(requests[1].kind, FollowingDocumentKind::Graphql); assert_eq!(requests[1].after.as_deref(), Some("synthetic-next"));
    assert_eq!(requests[1].url(), "https://www.instagram.com/graphql/query");
    assert_eq!(requests[2].url(), "https://www.instagram.com/graphql/query");
    assert_eq!(requests[2].after.as_deref(), Some("synthetic-last")); assert!(requests.iter().all(|v| v.page_size == 12));
}

#[test]
fn following_nonmedia_units_advance_without_importing_ads_suggestions_or_false_terminal() {
    let units = ["ad", "explore_story", "end_of_feed_demarcator", "stories_netego", "suggested_users", "bloks_netego", "abra_icebreakers_in_feed_unit", "ad4ad_in_webfeed"]
        .into_iter().map(|kind| json!({"node":{"__typename":"XDTFeedItem",(kind):{"synthetic":true},"media":null}})).collect();
    let adapter = following(vec![initial(connection(units, Some("synthetic-next"))), graphql(connection(vec![edge(post(1))], None))]);
    let page = run(adapter.acquire(selected_following(1))).unwrap(); assert_eq!(page.entries.len(), 1); assert_eq!(page.coverage, Coverage::Partial);
    assert_eq!(adapter.fetch.requests.lock().unwrap().len(), 2); assert_eq!(adapter.canonical.evidence.lock().unwrap().len(), 1);
    let mut limited = following(vec![initial(connection(vec![json!({"node":{"ad":{},"media":null}})], Some("synthetic-next"))),
        graphql(connection(vec![json!({"node":{"ad":{},"media":null}})], Some("synthetic-last")))]);
    limited.limits.max_fetches_per_operation = 2;
    assert!(matches!(run(limited.acquire(selected_following(1))), Err(rss_contracts::Error::LimitExceeded { field }) if field == "instagram.home_fetches"));
    assert_eq!(limited.fetch.requests.lock().unwrap().len(), 2); assert!(limited.canonical.evidence.lock().unwrap().is_empty());
}

#[test]
fn following_retained_html_offset_renews_lease_without_rebinding_original_bytes() {
    let bytes = html(connection(vec![edge(post(1)), edge(post(2)), edge(post(3))], None));
    let mut cached = reply(FollowingDocumentKind::Html, vec![]); cached.status = 304; cached.cache = Some(bytes.clone());
    let mut adapter = following(vec![reply(FollowingDocumentKind::Html, bytes), cached]);
    let first = run(adapter.acquire(selected_following(1))).unwrap(); let next = first.continuation.unwrap();
    for secret in ["device_id", "doc_id", "csrf", "cookie", "lease_generation", "feed_view_info"] { assert!(!next.token.contains(secret)); }
    adapter.authorization.authorization.lease_generation = Revision(2); adapter.fetch.generation.store(2, Ordering::SeqCst);
    let mut resume = selected_following(2); resume.continuation = Some(next.clone());
    assert_eq!(run(adapter.acquire(resume.clone())).unwrap().entries[0].provider_entry_key, "3");
    assert!(adapter.fetch.requests.lock().unwrap()[1].expected_digest.is_some());
    adapter.authorization.authorization.external_identity = "888888".into();
    assert_eq!(run(adapter.acquire(resume)), Err(rss_contracts::Error::StaleResult)); assert_eq!(adapter.fetch.requests.lock().unwrap().len(), 2);
}

#[test]
fn following_intent_mode_source_connection_and_current_profile_fail_before_fetch() {
    for changed in 0..6 {
        let mut adapter = following(vec![]); let mut request = selected_following(1);
        match changed {
            0 => adapter.authorization.capture_intent = CaptureIntent::SaveUrl { original_url: adapter.source.original_url.clone() },
            1 => adapter.source.original_url = "https://www.instagram.com/".into(),
            2 => request.subscription.partition.mode = AccessMode::HomeForYou,
            3 => request.subscription.partition.connection_ref = Some(reference("foreign")),
            4 => request.subscription.partition.scope = ScopeRef("foreign".into()),
            _ => adapter.authorization.authorization.external_identity = "888888".into(),
        }
        assert!(run(adapter.acquire(request)).is_err()); assert!(adapter.fetch.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn following_wrong_stream_malformed_media_or_error_envelope_has_no_effect() {
    let valid = connection(vec![edge(post(1))], None);
    let mut home = valid.clone(); home["pagination_source"] = json!("home");
    let mut missing = valid.clone(); missing.as_object_mut().unwrap().remove("pagination_source");
    let mut bad_info = valid.clone(); bad_info["page_info"].as_object_mut().unwrap().remove("has_next_page");
    let mut bad_media = post(2); bad_media["image_versions2"]["candidates"] = json!([]);
    let wrapped_error = format!("<script type=\"application/json\">{}</script>", json!({"errors":[{"message":"denied"}],"data":{(ROOT):valid}})).into_bytes();
    for bytes in [html(home), html(missing), html(bad_info), html(connection(vec![edge(post(1)), edge(bad_media)], None)), wrapped_error,
        html(connection(vec![json!({"node":{"unknown_unit":{}}})], None))] {
        let adapter = following(vec![reply(FollowingDocumentKind::Html, bytes)]);
        assert!(run(adapter.acquire(selected_following(1))).is_err()); assert!(adapter.canonical.evidence.lock().unwrap().is_empty());
    }
}

#[test]
fn following_changed_retained_body_kind_redirect_and_missing_cache_cannot_continue() {
    for changed in 0..4 {
        let bytes = html(connection(vec![edge(post(1)), edge(post(2)), edge(post(3))], None));
        let mut second = reply(FollowingDocumentKind::Html, bytes.clone());
        match changed { 0 => second.bytes.push(b' '), 1 => second.kind = FollowingDocumentKind::Graphql,
            2 => second.url = "https://www.instagram.com/".into(), _ => { second.status = 304; second.bytes.clear(); } }
        let adapter = following(vec![reply(FollowingDocumentKind::Html, bytes), second]);
        let first = run(adapter.acquire(selected_following(1))).unwrap(); let mut resume = selected_following(2); resume.continuation = first.continuation;
        assert!(run(adapter.acquire(resume)).is_err()); assert_eq!(adapter.canonical.evidence.lock().unwrap().len(), 2);
    }
}

#[test]
fn following_empty_terminal_cycles_and_lease_expiry_preserve_partial_boundary() {
    let empty = following(vec![initial(connection(vec![], None))]); assert!(run(empty.acquire(selected_following(1))).is_err());
    let adapter = following(vec![initial(connection(vec![edge(post(1))], Some("synthetic-next"))), graphql(connection(vec![], None))]);
    let first = run(adapter.acquire(selected_following(1))).unwrap(); let mut resume = selected_following(2); resume.continuation = first.continuation;
    assert!(run(adapter.acquire(resume)).unwrap().is_terminal_partial_document());
    let adapter = following(vec![initial(connection(vec![edge(post(1))], Some("synthetic-next"))), graphql(connection(vec![edge(post(2))], Some("synthetic-next")))]);
    let first = run(adapter.acquire(selected_following(1))).unwrap(); let mut resume = selected_following(2); resume.continuation = first.continuation;
    assert!(run(adapter.acquire(resume)).is_err()); assert_eq!(adapter.canonical.evidence.lock().unwrap().len(), 1);
    for (check, prepared) in [(1, 0), (2, 0), (3, 0), (4, 1)] {
        let adapter = following(vec![initial(connection(vec![edge(post(1))], None))]);
        *adapter.fetch.reject.lock().unwrap() = Some((check, rss_contracts::Error::Denied));
        assert_eq!(run(adapter.acquire(selected_following(1))), Err(rss_contracts::Error::Denied)); assert_eq!(adapter.canonical.evidence.lock().unwrap().len(), prepared);
    }
}

#[test]
fn following_body_document_media_and_cursor_bounds_reject_before_preparation() {
    for changed in 0..4 {
        let mut value = post(1); if changed == 3 { value["expiring_at"] = json!(1); }
        let mut adapter = following(vec![initial(connection(vec![edge(value), edge(post(2))], Some("synthetic-next")))]);
        match changed { 0 => adapter.limits.max_body_bytes = 10, 1 => adapter.limits.max_document_entries = 1,
            2 => adapter.limits.max_documents = 1, _ => {} }
        assert!(run(adapter.acquire(selected_following(1))).is_err()); assert!(adapter.canonical.evidence.lock().unwrap().is_empty());
    }
    let adapter = following(vec![initial(connection(vec![edge(post(1))], Some("synthetic-next")))]);
    let first = run(adapter.acquire(selected_following(1))).unwrap(); let mut cursor = first.continuation.unwrap();
    cursor.token = cursor.token.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    let mut resume = selected_following(2); resume.continuation = Some(cursor);
    assert!(run(adapter.acquire(resume)).is_err()); assert_eq!(adapter.fetch.requests.lock().unwrap().len(), 1);
}

#[test]
fn provider_capabilities_following_are_available_without_execution_ports() {
    let expected = vec![
        rss_contracts::Capability { provider: rss_contracts::Provider::Instagram, mode: AccessMode::Following, status: rss_contracts::CapabilityStatus::Available,
            paginated: true, media_metadata: true, adapter_version: "instagram.following.v1".into() }
    ];
    assert_eq!(rss_provider_instagram::connected::following_capabilities(), expected);
    assert_eq!(following(vec![]).capabilities(), expected);
}
