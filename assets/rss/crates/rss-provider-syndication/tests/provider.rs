use std::{collections::{BTreeMap, BTreeSet, VecDeque}, future::Future, pin::pin, sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}}, task::{Context, Poll, Wake, Waker}};
use hsscrap::*;
use rss_contracts::{AccessMode, AccessPartition, AcquisitionState, ObservedEntry, Pending, Provider, ProviderPort, Source, SourceRequest, Subscription};
use rss_provider_syndication::*;

fn run<T>(future: impl Future<Output = T>) -> T {
    struct Noop; impl Wake for Noop { fn wake(self: Arc<Self>) {} }
    let waker = Waker::from(Arc::new(Noop)); let mut context = Context::from_waker(&waker); let mut future = pin!(future);
    match future.as_mut().poll(&mut context) { Poll::Ready(value) => value, Poll::Pending => panic!("fixture requires an unexpected runtime") }
}
fn id<T: std::str::FromStr>(value: u64) -> T where T::Err: std::fmt::Debug { format!("00000000-0000-0000-0000-{value:012}").parse().unwrap() }
fn reference(value: &str) -> HostRef { HostRef { host_instance_id: "fixture".into(), owner_kind: "fixture".into(), native_id: value.into() } }
fn at() -> Instant { Instant::parse("2026-10-23T00:00:00Z").unwrap() }
fn bounds() -> Bounds { Bounds { max_url_bytes: 4096, max_text_bytes: 4096, max_collection_items: 100, max_page_items: 2, max_operation_bytes: 100_000 } }
fn request(operation: u64) -> SourceRequest { SourceRequest { subscription: Subscription { subscription_ref: reference("subscription"), source_ref: reference("source"),
    partition: AccessPartition { scope: ScopeRef("account".into()), mode: AccessMode::PublicFeed, connection_ref: None },
    executor_ref: reference("executor"), generation: Revision(1), paused: false, common: Extensions::default() }, operation_id: id(operation), continuation: None, bounds: bounds() } }
fn feed(count: usize) -> Vec<u8> {
    format!("<rss version=\"2.0\"><channel><title>Blog</title><link>https://example.org/</link><description>Blog</description>{}</channel></rss>",
        (0..count).map(|i| format!("<item><guid isPermaLink=\"false\">entry-{i}</guid><link>https://example.org/post-{i}</link><title>Post {i}</title></item>")).collect::<String>()).into_bytes()
}
fn response(body: Vec<u8>) -> FetchResponse { FetchResponse { final_url: "https://example.org/feed".into(), status: 200, body, cached_body: None, observed_at: at() } }

#[derive(Default)]
struct Fetch { responses: Mutex<VecDeque<FetchResponse>>, requests: Mutex<Vec<FetchRequest>> }
impl FetchPort for Fetch {
    fn fetch<'a>(&'a self, _: &'a SourceRequest, _: &'a HostRef, request: FetchRequest) -> Pending<'a, FetchResponse> {
        Box::pin(async move { self.requests.lock().unwrap().push(request); self.responses.lock().unwrap().pop_front().ok_or(rss_contracts::Error::Network) })
    }
}
/// A disposable typed callback fixture, not an implementation of a durable host owner.
#[derive(Default)]
struct Canonical { items: Mutex<BTreeMap<String, ItemId>>, calls: AtomicU64 }
impl CanonicalEntryPort for Canonical {
    fn prepare<'a>(&'a self, request: &'a SourceRequest, _: &'a HostRef, _: &'a str, evidence: EntryEvidence) -> Pending<'a, ObservedEntry> {
        Box::pin(async move {
            let serial = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let normalized = hsscrap::identity::normalize_url(&evidence.url, &request.bounds)?;
            let mut items = self.items.lock().unwrap(); let next_id = id(500 + items.len() as u64);
            let item = *items.entry(normalized.identity_url.clone()).or_insert(next_id);
            let capture = URLCapture { capture_id: id(100 + serial), item_id: item, capture_kind: CaptureKind::SourceIngest,
                access_partition: request.subscription.partition.scope.clone(), captured_url: evidence.url.clone(), original_url: Some(evidence.url.clone()),
                identity_url: normalized.identity_url, normalization_version: normalized.normalization_version, captured_at: Some(at()), created_here_at: at(),
                actor_ref: Some(reference("actor")), operation_id: id(10_000 + serial), common: Extensions::default() };
            let mut observation = MetadataObservation { observation_id: id(200 + serial), item_id: item, stream_id: id(300 + serial), sequence: Revision(serial),
                baseline_observation_id: None, capture_id: Some(capture.capture_id), appearance_ref: Some(reference("appearance")), producer: "fixture".into(),
                access_partition: request.subscription.partition.scope.clone(), observed_at: at(), source_revision: None, canonical_url_assertion: None,
                provider: None, host_site: "example.org".into(), title: None, description: None, author: None, published_at: None, provider_labels: vec![],
                gallery_snapshot: None, source_payload_digest: None, status: Availability { state: AvailabilityState::Available, checked_at: Some(at()), reason_code: None, retry_after: None }, common: Extensions::default() };
            evidence.populate(&mut observation);
            Ok(ObservedEntry { provider_entry_key: evidence.key, capture, observation })
        })
    }
}
fn provider(responses: Vec<FetchResponse>) -> SyndicationProvider<Fetch, Canonical> {
    SyndicationProvider { source: Source { source_ref: reference("source"), owner_scope: ScopeRef("account".into()), provider: Provider::Syndication,
        original_url: "https://example.org/feed".into(), display_name: "Blog".into(), revision: Revision(1), paused: false, common: Extensions::default() },
        execution_ref: reference("attempt"), limits: ProviderLimits { max_body_bytes: 100_000, max_document_entries: 100, max_documents: 10,
            max_fetches_per_operation: 4, timeout_millis: 10_000, max_redirects: 3 }, supported_extensions: BTreeSet::new(),
        fetch: Fetch { responses: Mutex::new(responses.into()), requests: Mutex::new(vec![]) }, canonical: Canonical::default() }
}

#[test]
fn bounded_pages_resume_same_document_and_unchanged_refresh_reuses_item_identity() {
    let bytes = feed(3); let adapter = provider(vec![response(bytes.clone()), response(bytes.clone()), response(bytes.clone()), response(bytes)]);
    let first = run(adapter.acquire(request(1))).unwrap(); assert_eq!(first.entries.len(), 2);
    first.validate(&request(1).subscription, &bounds(), &BTreeSet::new()).unwrap();
    let mut second_request = request(2); second_request.continuation = first.continuation.clone();
    let second = run(adapter.acquire(second_request)).unwrap(); assert_eq!(second.entries.len(), 1); assert!(second.continuation.is_none());
    let mut items: BTreeSet<_> = first.entries.iter().map(|entry| entry.capture.item_id).collect(); items.insert(second.entries[0].capture.item_id);
    assert_eq!(items.len(), 3);
    let refresh = run(adapter.acquire(request(3))).unwrap(); assert_eq!(refresh.entries[0].capture.item_id, first.entries[0].capture.item_id);
    let mut refresh_request = request(4); refresh_request.continuation = refresh.continuation;
    run(adapter.acquire(refresh_request)).unwrap(); assert_eq!(adapter.canonical.items.lock().unwrap().len(), 3);
    assert!(matches!(second.state, AcquisitionState::Partial { ref reason_code, .. } if reason_code == "current_feed_document_only"));
    let calls = adapter.fetch.requests.lock().unwrap(); assert!(calls.iter().all(|value| value.max_body_bytes == 100_000 && value.timeout_millis == 10_000 && value.max_redirects == 3));
}

#[test]
fn changing_a_document_during_resume_fails_before_more_owner_effects() {
    let first_body = feed(3); let changed = String::from_utf8(first_body.clone()).unwrap().replace("Post 2", "Changed post").into_bytes();
    let adapter = provider(vec![response(first_body), response(changed)]);
    let first = run(adapter.acquire(request(1))).unwrap(); let mut next = request(2); next.continuation = first.continuation;
    assert!(matches!(run(adapter.acquire(next)), Err(rss_contracts::Error::StaleResult)));
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn malformed_fetch_failure_and_missing_cache_never_report_empty_success() {
    for scenario in 0..4 {
        let mut fetched = response(feed(0));
        match scenario { 0 => fetched.status = 403, 1 => fetched.body = b"<invalid".to_vec(),
            2 => { fetched.status = 304; fetched.body.clear(); }, _ => fetched.body = vec![b'x'; 100_001] }
        let adapter = provider(vec![fetched]);
        assert!(run(adapter.acquire(request(1))).is_err()); assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn conditional_refresh_requires_cached_bytes_and_keeps_stable_entry_keys() {
    let bytes = feed(1); let mut cached = response(vec![]); cached.status = 304; cached.cached_body = Some(bytes.clone());
    let adapter = provider(vec![response(bytes), cached]);
    let first = run(adapter.acquire(request(1))).unwrap(); let unchanged = run(adapter.acquire(request(2))).unwrap();
    assert_eq!(first.entries[0].provider_entry_key, unchanged.entries[0].provider_entry_key);
    assert_eq!(first.entries[0].capture.item_id, unchanged.entries[0].capture.item_id);
}

#[test]
fn blog_discovery_fetches_only_the_single_advertised_feed() {
    let html = br#"<html><head><link rel="alternate" type="application/rss+xml" href="/feed"></head></html>"#.to_vec();
    let mut site = response(html); site.final_url = "https://example.org/blog".into();
    let mut adapter = provider(vec![site, response(feed(1))]); adapter.source.original_url = "https://example.org/blog".into();
    let page = run(adapter.acquire(request(1))).unwrap(); assert_eq!(page.entries.len(), 1);
    let calls = adapter.fetch.requests.lock().unwrap(); assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].url, "https://example.org/blog"); assert_eq!(calls[1].url, "https://example.org/feed");
}

#[test]
fn ambiguous_discovery_requires_source_selection_before_entry_effects() {
    let html = br#"<link rel="alternate" type="application/rss+xml" href="/one"><link rel="alternate" type="application/atom+xml" href="/two">"#.to_vec();
    let adapter = provider(vec![response(html)]);
    assert!(run(adapter.acquire(request(1))).is_err()); assert_eq!(adapter.fetch.requests.lock().unwrap().len(), 1);
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cyclic_provider_documents_reject_before_canonical_preparation() {
    let xml = br#"<feed xmlns="http://www.w3.org/2005/Atom"><id>urn:feed</id><title>Blog</title><updated>2026-10-23T00:00:00Z</updated><link rel="next" href="https://example.org/feed"/><entry><id>urn:post</id><title>Post</title><updated>2026-10-23T00:00:00Z</updated><link href="https://example.org/post"/></entry></feed>"#.to_vec();
    for redirected in [false, true] {
        let mut fetched = response(xml.clone());
        if redirected { fetched.final_url = "https://example.org/canonical-feed".into(); }
        let adapter = provider(vec![fetched]);
        assert!(run(adapter.acquire(request(1))).is_err()); assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn empty_archive_after_nonempty_page_cannot_claim_the_selected_source_is_empty() {
    let first = br#"<feed xmlns="http://www.w3.org/2005/Atom"><id>urn:feed</id><title>Blog</title><updated>2026-10-23T00:00:00Z</updated><link rel="next" href="https://example.org/archive"/><entry><id>urn:post</id><title>Post</title><updated>2026-10-23T00:00:00Z</updated><link href="https://example.org/post"/></entry></feed>"#.to_vec();
    let mut empty_archive = response(feed(0)); empty_archive.final_url = "https://example.org/archive".into();
    let adapter = provider(vec![response(first), empty_archive]);
    let first = run(adapter.acquire(request(1))).unwrap(); assert_eq!(first.entries.len(), 1);
    let mut next = request(2); next.continuation = first.continuation;
    let terminal = run(adapter.acquire(next.clone())).unwrap();
    terminal.validate(&next.subscription, &next.bounds, &BTreeSet::new()).unwrap();
    assert!(terminal.is_terminal_partial_document()); assert_eq!(terminal.coverage, Coverage::Partial);
    assert!(matches!(terminal.state, AcquisitionState::Partial { ref reason_code, .. } if reason_code == rss_contracts::TERMINAL_SOURCE_DOCUMENT));
    assert!(terminal.entries.is_empty()); assert!(terminal.continuation.is_none());
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.canonical.items.lock().unwrap().len(), 1);
}

#[test]
fn only_an_actually_parsed_empty_document_reports_verified_empty() {
    let adapter = provider(vec![response(feed(0))]); let request = request(1);
    let page = run(adapter.acquire(request.clone())).unwrap(); page.validate(&request.subscription, &request.bounds, &BTreeSet::new()).unwrap();
    assert!(matches!(page.state, AcquisitionState::VerifiedEmpty { .. })); assert!(page.entries.is_empty());
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn foreign_source_and_stale_scoped_continuation_fail_before_transport() {
    let adapter = provider(vec![]); let mut foreign = request(1); foreign.subscription.source_ref = reference("foreign");
    assert!(matches!(run(adapter.acquire(foreign)), Err(rss_contracts::Error::Denied)));
    let mut stale = request(1); stale.continuation = Some(rss_contracts::Continuation { source_ref: reference("source"), subscription_ref: reference("subscription"),
        partition: stale.subscription.partition.clone(), executor_ref: reference("old-executor"), generation: Revision(1), token: "{}".into() });
    assert!(matches!(run(adapter.acquire(stale)), Err(rss_contracts::Error::StaleResult)));
    assert!(adapter.fetch.requests.lock().unwrap().is_empty());
}

#[test]
fn unknown_required_source_feature_fails_before_fetch_or_owner_preparation() {
    let mut adapter = provider(vec![]);
    adapter.source.common.required_features.insert("syndication.future_projection.v9".into());
    assert!(matches!(run(adapter.acquire(request(1))), Err(rss_contracts::Error::ItemOwner {
        error: Error::UnsupportedRequiredFeature { feature }
    }) if feature == "syndication.future_projection.v9"));
    assert!(adapter.fetch.requests.lock().unwrap().is_empty());
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
}
