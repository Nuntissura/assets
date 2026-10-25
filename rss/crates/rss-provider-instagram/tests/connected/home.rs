use super::*;
use rss_provider_instagram::connected::{HomeDocumentKind, HomeFetchPort, HomeFetchRequest,
    HomeFetchResponse, HomeInstagramProvider};

const ROOT: &str = "xdt_api__v1__feed__timeline__connection";
fn connection(items: Vec<Value>, next: Option<&str>) -> Value {
    json!({"edges":items.into_iter().map(|media| json!({"node":{"media":media}})).collect::<Vec<_>>(),
        "page_info":{"has_next_page":next.is_some(),"end_cursor":next}})
}
fn html(value: Value) -> Vec<u8> {
    format!("<!doctype html><html><script type=\"application/json\">{}</script></html>",
        json!({"require":[{"result":{"data":{(ROOT):value}}}]})).into_bytes()
}
struct HomeReply {
    kind: HomeDocumentKind, status: u16, bytes: Vec<u8>, cache: Option<Vec<u8>>, final_url: String,
}
fn reply(kind: HomeDocumentKind, bytes: Vec<u8>) -> HomeReply {
    HomeReply { kind, status: 200, bytes, cache: None, final_url: kind.url().into() }
}
fn initial(value: Value) -> HomeReply { reply(HomeDocumentKind::Html, html(value)) }
fn graphql(value: Value) -> HomeReply {
    reply(HomeDocumentKind::Graphql, serde_json::to_vec(&json!({"data":{(ROOT):value}})).unwrap())
}
struct HomeFetch {
    authority: AuthorizedFetch,
    replies: Mutex<VecDeque<HomeReply>>,
    requests: Mutex<Vec<HomeFetchRequest>>,
}
impl AuthorizedFetchPort for HomeFetch {
    fn current<'a>(&'a self, source: &'a SourceRequest, execution: &'a HostRef, authorization: &'a AuthorizedContext) -> Pending<'a, ()> {
        Box::pin(async move {
            self.authority.current(source, execution, authorization).await?;
            if source.subscription.partition.scope.0 != "scope" || authorization.external_identity != "999999" {
                return Err(rss_contracts::Error::Denied);
            }
            Ok(())
        })
    }
    fn fetch<'a>(&'a self, _: &'a SourceRequest, _: &'a HostRef, _: &'a AuthorizedContext, _: FetchRequest) -> Pending<'a, FetchResponse> {
        Box::pin(async { panic!("Home must not use public/profile REST transport") })
    }
}
impl HomeFetchPort for HomeFetch {
    fn fetch_home<'a>(&'a self, _: &'a SourceRequest, _: &'a HostRef, _: &'a AuthorizedContext, request: HomeFetchRequest)
        -> Pending<'a, HomeFetchResponse> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            let reply = self.replies.lock().unwrap().pop_front().ok_or(rss_contracts::Error::Network)?;
            Ok(HomeFetchResponse { kind: reply.kind, response: FetchResponse {
                final_url: reply.final_url, status: reply.status, body: reply.bytes, cached_body: reply.cache, observed_at: at(),
            } })
        })
    }
}
#[derive(Default)]
struct HomeCanonical { inner: Canonical, inputs: Mutex<Vec<(SourceRequest, EntryEvidence)>> }
impl CanonicalEntryPort for HomeCanonical {
    fn prepare<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, evidence: EntryEvidence) -> Pending<'a, ObservedEntry> {
        self.inputs.lock().unwrap().push((request.clone(), evidence.clone()));
        self.inner.prepare(request, execution, evidence)
    }
}
fn home(replies: Vec<HomeReply>) -> HomeInstagramProvider<HomeFetch, HomeCanonical> {
    let base = authorized(AccessMode::PublicProfile, vec![]);
    let mut source = base.source;
    source.original_url = "https://www.instagram.com/".into();
    HomeInstagramProvider { source, execution_ref: base.execution_ref, limits: base.limits,
        supported_extensions: base.supported_extensions, authorization: base.authorization,
        fetch: HomeFetch { authority: base.fetch, replies: Mutex::new(replies.into()), requests: Mutex::new(vec![]) },
        canonical: HomeCanonical::default() }
}
fn selected_home(operation: u64) -> SourceRequest { selected(AccessMode::HomeForYou, operation) }

#[test]
fn home_html_and_graphql_preserve_actual_media_author_and_home_partition() {
    let mut suggested = item(2);
    suggested["user"] = json!({"pk":"654321","username":"another"});
    suggested["id"] = json!("2_654321");
    let mut feed = connection(vec![item(1), suggested.clone()], Some("next"));
    feed["edges"][1]["node"] = json!({"explore_story":{"media":suggested}});
    let adapter = home(vec![initial(feed),
        graphql(connection(vec![item(1), item(3)], None))]);
    let first = run(adapter.acquire(selected_home(1))).unwrap();
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.coverage, Coverage::Partial);
    let mut resume = selected_home(2); resume.continuation = first.continuation;
    let last = run(adapter.acquire(resume)).unwrap();
    assert_eq!(last.entries[0].provider_entry_key, "3");
    assert_eq!(last.entries.len(), 1);
    assert!(last.continuation.is_none());
    assert!(matches!(last.state, rss_contracts::AcquisitionState::Partial { .. }));
    let inputs = adapter.canonical.inputs.lock().unwrap();
    assert_eq!(inputs[0].0, selected_home(1));
    assert_eq!(inputs[1].1.owner_id, "654321");
    assert_eq!(inputs[1].1.account_id, "654321");
    assert_eq!(inputs[1].1.author, "another");
    assert!(inputs.iter().all(|(request, _)| request.subscription.partition.mode == AccessMode::HomeForYou));
    let requests = adapter.fetch.requests.lock().unwrap();
    assert_eq!(requests[0].kind, HomeDocumentKind::Html);
    assert_eq!(requests[0].after, None);
    assert_eq!(requests[1].kind, HomeDocumentKind::Graphql);
    assert_eq!(requests[1].after.as_deref(), Some("next"));
    assert_eq!(adapter.capabilities()[0].mode, AccessMode::HomeForYou);
}

#[test]
fn home_retained_html_offset_survives_lease_renewal_without_runtime_variables() {
    let bytes = html(connection(vec![item(1), item(2), item(3)], None));
    let mut retained = reply(HomeDocumentKind::Html, vec![]);
    retained.status = 304; retained.cache = Some(bytes.clone());
    let mut adapter = home(vec![reply(HomeDocumentKind::Html, bytes), retained]);
    let first = run(adapter.acquire(selected_home(1))).unwrap();
    let continuation = first.continuation.unwrap();
    assert_eq!(continuation.partition.mode, AccessMode::HomeForYou);
    for private in ["device_id", "doc_id", "cookie", "session", "lease_generation"] {
        assert!(!continuation.token.contains(private));
    }
    adapter.authorization.lease_generation = Revision(2);
    adapter.fetch.authority.generation.store(2, Ordering::SeqCst);
    let mut resume = selected_home(2); resume.continuation = Some(continuation.clone());
    let last = run(adapter.acquire(resume.clone())).unwrap();
    assert_eq!(last.entries[0].provider_entry_key, "3");
    let requests = adapter.fetch.requests.lock().unwrap();
    assert_eq!(requests[1].kind, HomeDocumentKind::Html);
    assert!(requests[1].expected_digest.is_some());
    drop(requests);
    adapter.authorization.external_identity = "888888".into();
    assert_eq!(run(adapter.acquire(resume)), Err(rss_contracts::Error::StaleResult));
    assert_eq!(adapter.fetch.requests.lock().unwrap().len(), 2);
}

#[test]
fn home_empty_is_unverified_but_continued_terminal_document_can_settle() {
    let empty = home(vec![initial(connection(vec![], None))]);
    assert!(matches!(run(empty.acquire(selected_home(1))), Err(rss_contracts::Error::Invalid { field }) if field == "instagram.home_empty_unverified"));
    assert!(empty.canonical.inputs.lock().unwrap().is_empty());
    let adapter = home(vec![initial(connection(vec![item(1)], Some("next"))), graphql(connection(vec![], None))]);
    let first = run(adapter.acquire(selected_home(1))).unwrap();
    let mut resume = selected_home(2); resume.continuation = first.continuation;
    let last = run(adapter.acquire(resume)).unwrap();
    assert!(last.entries.is_empty());
    assert!(last.continuation.is_none());
    assert_eq!(last.coverage, Coverage::Partial);
    assert!(matches!(last.state, rss_contracts::AcquisitionState::Partial { reason_code, .. }
        if reason_code == rss_contracts::TERMINAL_SOURCE_DOCUMENT));
}

#[test]
fn home_rejects_wrong_mode_root_connection_and_profile_before_network() {
    for changed in 0..5 {
        let mut adapter = home(vec![]);
        let mut request = selected_home(1);
        match changed {
            0 => request.subscription.partition.mode = AccessMode::Following,
            1 => adapter.source.original_url = "https://www.instagram.com/kngemma/".into(),
            2 => request.subscription.partition.connection_ref = Some(reference("other")),
            3 => request.subscription.partition.scope = ScopeRef("other".into()),
            _ => adapter.authorization.external_identity = "888888".into(),
        }
        assert!(run(adapter.acquire(request)).is_err());
        assert!(adapter.fetch.requests.lock().unwrap().is_empty());
        assert!(adapter.canonical.inputs.lock().unwrap().is_empty());
    }
}

#[test]
fn home_malformed_or_conflicting_selected_content_has_no_canonical_effect() {
    let valid = connection(vec![item(1)], None);
    let mut no_info = valid.clone(); no_info.as_object_mut().unwrap().remove("page_info");
    let mut no_more = valid.clone(); no_more["page_info"].as_object_mut().unwrap().remove("has_next_page");
    let mut no_cursor = valid.clone(); no_cursor["page_info"].as_object_mut().unwrap().remove("end_cursor");
    let mut unknown = valid.clone(); unknown["edges"][0] = json!({"node":{"unrecognized":{"media":item(2)}}});
    let mut malformed = item(3); malformed["image_versions2"]["candidates"] = json!([]);
    let duplicate = format!("<script type=\"application/json\">{}</script><script type=\"application/json\">{}</script>",
        json!({(ROOT):valid}), json!({(ROOT):connection(vec![item(2)],None)})).into_bytes();
    let mut duplicate_key = br#"<script type="application/json">{"x":1,"x":2}</script>"#.to_vec();
    for bytes in [html(no_info), html(no_more), html(no_cursor), html(unknown), duplicate,
        html(connection(vec![item(1), item(2), malformed], None)),
        b"<html>Login page without selected connection</html>".to_vec(), std::mem::take(&mut duplicate_key)] {
        let adapter = home(vec![reply(HomeDocumentKind::Html, bytes)]);
        assert!(run(adapter.acquire(selected_home(1))).is_err());
        assert!(adapter.canonical.inputs.lock().unwrap().is_empty());
    }
}

#[test]
fn home_html_selected_ancestor_errors_deny_but_unrelated_branch_errors_do_not() {
    let valid = connection(vec![item(1)], None);
    for envelope in [
        json!({"errors":[{"message":"login_required"}],"data":{(ROOT):valid.clone()}}),
        json!({"status":"fail","data":{(ROOT):valid.clone()}}),
        json!({"message":"login_required","data":{(ROOT):valid.clone()}}),
        json!({"require":[{"status":"fail","result":{"data":{(ROOT):valid.clone()}}}]}),
    ] {
        let bytes = format!("<script type=\"application/json\">{envelope}</script>").into_bytes();
        let adapter = home(vec![reply(HomeDocumentKind::Html, bytes)]);
        assert!(run(adapter.acquire(selected_home(1))).is_err());
        assert!(adapter.canonical.inputs.lock().unwrap().is_empty());
    }
    let envelope = json!({"unrelated":{"errors":[{"message":"login_required"}],"status":"fail"},
        "data":{(ROOT):valid}});
    let bytes = format!("<script type=\"application/json\">{envelope}</script>").into_bytes();
    let adapter = home(vec![reply(HomeDocumentKind::Html, bytes)]);
    let page = run(adapter.acquire(selected_home(1))).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(adapter.canonical.inputs.lock().unwrap().len(), 1);
}

#[test]
fn home_retained_kind_digest_and_redirect_changes_fail_without_more_effects() {
    for changed in 0..5 {
        let bytes = html(connection(vec![item(1), item(2), item(3)], None));
        let mut second = reply(HomeDocumentKind::Html, bytes.clone());
        match changed {
            0 => second.bytes.push(b' '),
            1 => second.kind = HomeDocumentKind::Graphql,
            2 => { second.status = 304; second.bytes.clear(); },
            3 => second.final_url = HomeDocumentKind::Graphql.url().into(),
            _ => second.final_url = "https://www.instagram.com/accounts/login/".into(),
        }
        let adapter = home(vec![reply(HomeDocumentKind::Html, bytes), second]);
        let first = run(adapter.acquire(selected_home(1))).unwrap();
        let mut resume = selected_home(2); resume.continuation = first.continuation;
        assert!(run(adapter.acquire(resume)).is_err());
        assert_eq!(adapter.canonical.inputs.lock().unwrap().len(), 2);
    }
}

#[test]
fn home_graphql_failures_and_repeated_cursor_do_not_prepare_later_entries() {
    let mut no_info = connection(vec![item(2)], None); no_info.as_object_mut().unwrap().remove("page_info");
    for second in [
        graphql(connection(vec![item(2)], Some("next"))), graphql(no_info),
        reply(HomeDocumentKind::Graphql, serde_json::to_vec(&json!({"errors":[{"message":"denied"}],"data":{(ROOT):connection(vec![item(2)],None)}})).unwrap()),
        reply(HomeDocumentKind::Graphql, br#"{"status":"fail","message":"login_required"}"#.to_vec()),
    ] {
        let adapter = home(vec![initial(connection(vec![item(1)], Some("next"))), second]);
        let first = run(adapter.acquire(selected_home(1))).unwrap();
        let mut resume = selected_home(2); resume.continuation = first.continuation;
        assert!(run(adapter.acquire(resume)).is_err());
        assert_eq!(adapter.canonical.inputs.lock().unwrap().len(), 1);
    }
}

#[test]
fn home_expired_revoked_or_superseded_lease_does_not_return_success() {
    for (at, error, fetches, preparations) in [
        (1, rss_contracts::Error::Expired, 0, 0), (2, rss_contracts::Error::Denied, 1, 0),
        (3, rss_contracts::Error::Denied, 1, 0), (4, rss_contracts::Error::Denied, 1, 1),
    ] {
        let adapter = home(vec![initial(connection(vec![item(1)], None))]);
        *adapter.fetch.authority.reject.lock().unwrap() = Some((at, error.clone()));
        assert_eq!(run(adapter.acquire(selected_home(1))), Err(error));
        assert_eq!(adapter.fetch.requests.lock().unwrap().len(), fetches);
        assert_eq!(adapter.canonical.inputs.lock().unwrap().len(), preparations);
    }
    let adapter = home(vec![]); adapter.fetch.authority.generation.store(2, Ordering::SeqCst);
    assert_eq!(run(adapter.acquire(selected_home(1))), Err(rss_contracts::Error::StaleResult));
    assert!(adapter.fetch.requests.lock().unwrap().is_empty());
}

#[test]
fn home_document_media_expiry_and_bounds_fail_before_preparation() {
    for changed in 0..4 {
        let mut expired = item(1);
        expired["expiring_at"] = json!(1);
        let mut adapter = home(vec![initial(connection(if changed == 0 { vec![expired] } else { vec![item(1), item(2)] }, Some("next")))]);
        match changed {
            1 => adapter.limits.max_body_bytes = 10,
            2 => adapter.limits.max_document_entries = 1,
            3 => adapter.limits.max_documents = 1,
            _ => {},
        }
        assert!(run(adapter.acquire(selected_home(1))).is_err());
        assert!(adapter.canonical.inputs.lock().unwrap().is_empty());
    }
}
