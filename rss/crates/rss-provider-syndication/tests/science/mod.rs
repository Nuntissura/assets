use hsscrap::*;
use rss_contracts::{
    AccessMode, AccessPartition, ObservedEntry, Pending, Provider, ProviderPort, Source,
    SourceRequest, Subscription,
};
use rss_provider_syndication::science::*;
use rss_provider_syndication::*;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    pin::pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll, Wake, Waker},
};
fn run<T>(future: impl Future<Output = T>) -> T {
    struct Noop;
    impl Wake for Noop {
        fn wake(self: Arc<Self>) {}
    }
    let waker = Waker::from(Arc::new(Noop));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("unexpected asynchronous fixture"),
    }
}
fn id<T: std::str::FromStr>(n: u64) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("00000000-0000-0000-0000-{n:012}").parse().unwrap()
}
fn reference(value: &str) -> HostRef {
    HostRef {
        host_instance_id: "fixture".into(),
        owner_kind: "fixture".into(),
        native_id: value.into(),
    }
}
fn at() -> Instant {
    Instant::parse("2026-10-23T00:00:00Z").unwrap()
}
fn bounds() -> Bounds {
    Bounds {
        max_url_bytes: 4096,
        max_text_bytes: 10000,
        max_collection_items: 100,
        max_page_items: 2,
        max_operation_bytes: 100000,
    }
}
fn request(n: u64) -> SourceRequest {
    SourceRequest {
        subscription: Subscription {
            subscription_ref: reference("subscription"),
            source_ref: reference("source"),
            partition: AccessPartition {
                scope: ScopeRef("scope".into()),
                mode: AccessMode::PublicFeed,
                connection_ref: None,
            },
            executor_ref: reference("executor"),
            generation: Revision(1),
            paused: false,
            common: Extensions::default(),
        },
        operation_id: id(n),
        continuation: None,
        bounds: bounds(),
    }
}
struct Reply {
    body: Vec<u8>,
    status: u16,
    cached: Option<Vec<u8>>,
}
fn raw(body: Vec<u8>) -> Reply {
    Reply {
        body,
        status: 200,
        cached: None,
    }
}
#[derive(Default)]
struct Fetch {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<FetchRequest>>,
}
impl FetchPort for Fetch {
    fn fetch<'a>(
        &'a self,
        source: &'a SourceRequest,
        _: &'a HostRef,
        request: FetchRequest,
    ) -> Pending<'a, FetchResponse> {
        Box::pin(async move {
            if source.subscription.partition.scope.0 != "scope" { return Err(rss_contracts::Error::Denied); }
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(rss_contracts::Error::Network)?;
            let response = FetchResponse {
                final_url: request.url.clone(),
                status: reply.status,
                body: reply.body,
                cached_body: reply.cached,
                observed_at: at(),
            };
            self.requests.lock().unwrap().push(request);
            Ok(response)
        })
    }
}
#[derive(Default)]
struct Canonical {
    identities: Mutex<BTreeMap<String, ItemId>>,
    calls: AtomicU64,
    evidence: Mutex<Vec<ScientificEvidence>>,
    legacy_provider: bool,
}
impl CanonicalScientificEntryPort for Canonical {
    fn prepare<'a>(
        &'a self,
        request: &'a SourceRequest,
        _: &'a HostRef,
        _: &'a str,
        evidence: ScientificEvidence,
    ) -> Pending<'a, ObservedEntry> {
        Box::pin(async move {
            self.evidence.lock().unwrap().push(evidence.clone());
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let mut identities = self.identities.lock().unwrap();
            let next = id(1000 + identities.len() as u64);
            let item_id = *identities
                .entry(evidence.reliable_provider_identity())
                .or_insert(next);
            let url = evidence.entry.url.clone();
            let normalized = hsscrap::identity::normalize_url(&url, &request.bounds)?;
            let capture = URLCapture {
                capture_id: id(100 + n),
                item_id,
                capture_kind: CaptureKind::SourceIngest,
                access_partition: request.subscription.partition.scope.clone(),
                captured_url: url.clone(),
                original_url: Some(url),
                identity_url: normalized.identity_url,
                normalization_version: normalized.normalization_version,
                captured_at: Some(at()),
                created_here_at: at(),
                actor_ref: Some(reference("actor")),
                operation_id: id(10000 + n),
                common: Extensions::default(),
            };
            let mut observation = MetadataObservation {
                observation_id: id(200 + n),
                item_id,
                stream_id: id(300 + n),
                sequence: Revision(n),
                baseline_observation_id: None,
                capture_id: Some(capture.capture_id),
                appearance_ref: Some(reference("appearance")),
                producer: "fixture".into(),
                access_partition: request.subscription.partition.scope.clone(),
                observed_at: at(),
                source_revision: None,
                canonical_url_assertion: None,
                provider: None,
                host_site: "arxiv.org".into(),
                title: None,
                description: None,
                author: None,
                published_at: None,
                provider_labels: vec![],
                gallery_snapshot: None,
                source_payload_digest: None,
                status: Availability {
                    state: AvailabilityState::Available,
                    checked_at: Some(at()),
                    reason_code: None,
                    retry_after: None,
                },
                common: Extensions::default(),
            };
            evidence.populate(&mut observation);
            if self.legacy_provider {
                observation.provider = Some("syndication".into());
            }
            Ok(ObservedEntry {
                provider_entry_key: evidence.entry.key,
                capture,
                observation,
            })
        })
    }
}
fn provider(replies: Vec<Reply>) -> ScientificProvider<Fetch, Canonical> {
    ScientificProvider {source:Source {source_ref:reference("source"),owner_scope:ScopeRef("account".into()),provider:Provider::Syndication,
    original_url:"https://export.arxiv.org/api/query?search_query=cat:cs.AI&max_results=3&sortBy=submittedDate&sortOrder=descending".into(),display_name:"Papers".into(),revision:Revision(1),paused:false,common:Extensions::default()},execution_ref:reference("attempt"),
    limits:ProviderLimits {max_body_bytes:100000,max_document_entries:100,max_documents:20,max_fetches_per_operation:2,timeout_millis:10000,max_redirects:0},supported_extensions:BTreeSet::new(),fetch:Fetch {replies:Mutex::new(replies.into()),requests:Mutex::new(vec![])},canonical:Canonical::default()}
}
fn entry(id: &str) -> String {
    format!("<entry><id>http://arxiv.org/abs/{id}</id><title>Paper</title><summary>Abstract</summary><author><name>Author</name></author><published>2024-08-23T00:00:00.123-04:00</published><updated>2025-01-02T00:00:00Z</updated><category term='cs.AI'/><link href='https://arxiv.org/abs/{id}' rel='alternate' type='text/html'/><link href='https://arxiv.org/pdf/{id}?download=1&amp;view=full' rel='related' type='application/pdf'/><arxiv:doi>10.1234/example</arxiv:doi><arxiv:journal_ref>Journal, 2025</arxiv:journal_ref><arxiv:comment>Eight pages &amp; figures</arxiv:comment></entry>")
}
fn api(start: u64, total: u64, entries: &[&str]) -> Vec<u8> {
    format!("<?xml version='1.0'?><feed xmlns='http://www.w3.org/2005/Atom' xmlns:os='http://a9.com/-/spec/opensearch/1.1/' xmlns:arxiv='http://arxiv.org/schemas/atom'><id>https://export.arxiv.org/api/query</id><title>Query</title><updated>2026-10-23T00:00:00Z</updated><os:totalResults>{total}</os:totalResults><os:startIndex>{start}</os:startIndex><os:itemsPerPage>3</os:itemsPerPage>{}</feed>",entries.iter().map(|id|entry(id)).collect::<String>()).into_bytes()
}
fn daily_atom(id: &str) -> Vec<u8> {
    format!("<feed xmlns='http://www.w3.org/2005/Atom' xmlns:arxiv='http://arxiv.org/schemas/atom'><id>http://rss.arxiv.org/atom/</id><title>Daily</title><updated>2026-10-23T00:00:00Z</updated><entry><id>oai:arXiv.org:{id}</id><title>Paper</title><link href='https://arxiv.org/abs/{}' rel='alternate'/><published>2024-08-23T00:00:00-04:00</published><updated>2026-10-23T00:00:00Z</updated><summary>Daily announcement</summary><arxiv:announce_type>replace-cross</arxiv:announce_type></entry></feed>",id.split('v').next().unwrap()).into_bytes()
}

#[test]
fn api_offsets_and_within_document_resume_preserve_paper_identity_and_provenance() {
    let first = api(0, 4, &["2408.12345v2", "2408.12346v1", "2408.12347v1"]);
    let adapter = provider(vec![
        raw(first.clone()),
        Reply {
            status: 304,
            body: vec![],
            cached: Some(first),
        },
        raw(api(3, 4, &["2408.12348v1"])),
        raw(api(0, 1, &["2408.12345v2"])),
    ]);
    let a = run(adapter.acquire(request(1))).unwrap();
    assert_eq!(a.entries.len(), 2);
    assert_eq!(a.entries[0].observation.provider.as_deref(), Some("arxiv"));
    assert_eq!(a.entries[0].provider_entry_key, "http://arxiv.org/abs/2408.12345v2");
    let mut req = request(2);
    req.continuation = a.continuation;
    let b = run(adapter.acquire(req)).unwrap();
    assert_eq!(
        b.entries[0].provider_entry_key,
        "http://arxiv.org/abs/2408.12347v1"
    );
    let mut req = request(3);
    req.continuation = b.continuation;
    let c = run(adapter.acquire(req)).unwrap();
    assert_eq!(c.entries.len(), 1);
    assert!(c.continuation.is_none());
    assert_eq!(c.coverage, Coverage::Partial);
    let refreshed = run(adapter.acquire(request(4))).unwrap();
    assert_eq!(
        refreshed.entries[0].capture.item_id,
        a.entries[0].capture.item_id
    );
    let fetched = adapter.fetch.requests.lock().unwrap();
    assert!(fetched[1].expected_digest.is_some());
    assert!(fetched[2].url.contains("start=3"));
    assert!(fetched
        .iter()
        .all(|request| request.max_body_bytes == 100000
            && request.timeout_millis == 10000
            && request.max_redirects == 0));
    let evidence = adapter.canonical.evidence.lock().unwrap();
    let paper = &evidence[0];
    assert_eq!(paper.reliable_provider_identity(), "arxiv/paper/2408.12345");
    assert_eq!(paper.paper.version, Some(2));
    assert_eq!(
        paper.paper.original_entry_id,
        "http://arxiv.org/abs/2408.12345v2"
    );
    assert_eq!(paper.paper.doi.as_deref(), Some("10.1234/example"));
    assert_eq!(
        paper.paper.updated_literal.as_deref(),
        Some("2025-01-02T00:00:00Z")
    );
    assert_eq!(paper.paper.source_links.len(), 2);
    assert_eq!(
        paper.paper.comment.as_deref(),
        Some("Eight pages & figures")
    );
    assert_eq!(
        paper.paper.source_links[1],
        "https://arxiv.org/pdf/2408.12345v2?download=1&view=full"
    );
    assert_eq!(
        paper.publication().unwrap().raw_value,
        "2024-08-23T00:00:00.123-04:00"
    );
    assert_eq!(
        paper.publication().unwrap().precision,
        PublicationPrecision::Subsecond
    );
}
#[test]
fn scientific_canonical_port_must_populate_the_paper_provider() {
    let mut adapter = provider(vec![raw(api(0, 1, &["2408.12345v2"]))]);
    adapter.canonical.legacy_provider = true;
    assert!(matches!(
        run(adapter.acquire(request(1))),
        Err(rss_contracts::Error::Invalid { field }) if field == "science.canonical_preparation"
    ));
}

#[test]
fn daily_and_api_dates_have_different_meaning_and_versions_share_global_paper_identity() {
    let mut adapter = provider(vec![
        raw(daily_atom("2408.12345v2")),
        raw(api(0, 1, &["2408.12345v3"])),
    ]);
    adapter.source.original_url = "https://rss.arxiv.org/atom/cs.AI".into();
    let daily = run(adapter.acquire(request(1))).unwrap();
    assert_eq!(daily.coverage, Coverage::Partial);
    adapter.source.original_url =
        "https://export.arxiv.org/api/query?id_list=2408.12345v3&max_results=3".into();
    let api = run(adapter.acquire(request(2))).unwrap();
    assert_eq!(
        api.entries[0].capture.item_id,
        daily.entries[0].capture.item_id
    );
    let evidence = adapter.canonical.evidence.lock().unwrap();
    assert_eq!(
        evidence[0].paper.publication_meaning,
        PublicationMeaning::Announcement
    );
    assert_eq!(
        evidence[0].paper.announce_type.as_deref(),
        Some("replace-cross")
    );
    assert_eq!(
        evidence[1].paper.publication_meaning,
        PublicationMeaning::FirstSubmission
    );
    assert_eq!(evidence[1].paper.version, Some(3));
    assert_eq!(API_MIN_INTERVAL_MILLIS, 3000);
}
#[test]
fn legacy_rss_ids_and_original_date_precision_remain_available_without_history_mutation() {
    let bytes=b"<rss version='2.0' xmlns:arxiv='http://arxiv.org/schemas/atom'><channel><title>Daily</title><link>https://rss.arxiv.org/rss/cs.AI</link><description>Papers</description><item><guid isPermaLink='false'>oai:arXiv.org:hep-th/9901001v2</guid><link>https://arxiv.org/abs/hep-th/9901001</link><title>Paper</title><pubDate>Fri, 23 Aug 2024 00:00:00 -0400</pubDate><arxiv:DOI>10.1234/legacy</arxiv:DOI></item></channel></rss>";
    let source = arxiv::source("https://rss.arxiv.org/rss/cs.AI", &bounds(), 100).unwrap();
    let parsed = arxiv::parse(&source, 0, bytes, 100000, 100, &bounds()).unwrap();
    let evidence = &parsed.entries[0];
    assert_eq!(evidence.entry.key, "oai:arXiv.org:hep-th/9901001v2");
    assert_eq!(evidence.paper.paper_id, "hep-th/9901001");
    assert_eq!(evidence.paper.version, Some(2));
    assert_eq!(evidence.paper.doi.as_deref(), Some("10.1234/legacy"));
    assert_eq!(evidence.publication().unwrap().offset_minutes, Some(-240));
    assert_eq!(
        evidence.publication().unwrap().raw_value,
        "Fri, 23 Aug 2024 00:00:00 -0400"
    );
}
#[test]
fn changed_snapshot_or_mutable_result_count_rejects_before_new_owner_effects() {
    let adapter = provider(vec![
        raw(api(0, 3, &["2408.12345v1", "2408.12346v1", "2408.12347v1"])),
        raw(api(0, 3, &["2408.12345v1", "2408.12346v1", "2408.12348v1"])),
    ]);
    let first = run(adapter.acquire(request(1))).unwrap();
    let mut next = request(2);
    next.continuation = first.continuation;
    assert!(matches!(
        run(adapter.acquire(next)),
        Err(rss_contracts::Error::StaleResult)
    ));
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 2);
    let adapter = provider(vec![
        raw(api(0, 2, &["2408.12345v1"])),
        raw(api(1, 3, &["2408.12346v1"])),
    ]);
    let first = run(adapter.acquire(request(3))).unwrap();
    let mut next = request(4);
    next.continuation = first.continuation;
    assert!(matches!(
        run(adapter.acquire(next)),
        Err(rss_contracts::Error::StaleResult)
    ));
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn wrong_counters_unexpected_empty_and_error_entry_are_unavailable_not_complete() {
    let mut missing = String::from_utf8(api(0, 1, &["2408.12345v1"])).unwrap();
    missing = missing.replace("<os:totalResults>1</os:totalResults>", "");
    for bytes in [
        api(1, 2, &["2408.12345v1"]),
        api(0, 1, &[]),
        missing.into_bytes(),
        api(0, 1, &["not-a-paper"]),
    ] {
        let adapter = provider(vec![raw(bytes)]);
        assert!(run(adapter.acquire(request(1))).is_err());
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    }
    assert!(arxiv::paper_id("http://arxiv.org/api/errors#bad").is_err());
    assert!(arxiv::paper_id("https://example.org/abs/2408.12345").is_err());
}
#[test]
fn rate_expiry_missing_retained_bytes_and_source_authority_fail_truthfully() {
    for reply in [
        Reply {
            status: 429,
            body: vec![],
            cached: None,
        },
        Reply {
            status: 410,
            body: vec![],
            cached: None,
        },
        Reply {
            status: 304,
            body: vec![],
            cached: None,
        },
        raw(b"<html>login</html>".to_vec()),
    ] {
        let adapter = provider(vec![reply]);
        assert!(run(adapter.acquire(request(1))).is_err());
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    }
    let adapter = provider(vec![]);
    let mut foreign = request(2);
    foreign.subscription.partition.scope = ScopeRef("foreign".into());
    assert!(matches!(
        run(adapter.acquire(foreign)),
        Err(rss_contracts::Error::Denied)
    ));
    assert!(adapter.fetch.requests.lock().unwrap().is_empty());
    let adapter = provider(vec![raw(api(0, 2, &["2408.12345v1"]))]);
    let first = run(adapter.acquire(request(3))).unwrap();
    let mut stale = request(4);
    stale.continuation = first.continuation;
    stale.subscription.generation = Revision(2);
    assert!(matches!(
        run(adapter.acquire(stale)),
        Err(rss_contracts::Error::StaleResult)
    ));
}
#[test]
fn explicit_zero_query_is_distinct_from_mutable_or_daily_coverage() {
    let adapter = provider(vec![raw(api(0, 0, &[]))]);
    assert!(matches!(
        run(adapter.acquire(request(1))).unwrap().state,
        rss_contracts::AcquisitionState::VerifiedEmpty { .. }
    ));
    let adapter = provider(vec![raw(api(0, 1, &["2408.12345v1"]))]);
    assert_eq!(
        run(adapter.acquire(request(2))).unwrap().coverage,
        Coverage::Partial
    );
    let mut adapter=provider(vec![raw(b"<rss version='2.0'><channel><title>Daily</title><link>https://rss.arxiv.org/rss/cs.AI</link><description>Daily</description></channel></rss>".to_vec())]);
    adapter.source.original_url = "https://rss.arxiv.org/rss/cs.AI".into();
    assert!(run(adapter.acquire(request(3))).is_err());
}
#[test]
fn approved_queries_namespaces_bounds_and_malformed_later_paper_are_checked_before_effects() {
    assert!(arxiv::source(
        "https://export.arxiv.org/api/query?search_query=cat:cs.AI&max_results=3000",
        &bounds(),
        10000
    )
    .is_err());
    assert!(arxiv::source(
        "https://example.org/api/query?search_query=x",
        &bounds(),
        100
    )
    .is_err());
    assert!(arxiv::source(
        "https://export.arxiv.org/api/query?search_query=x&search_query=y",
        &bounds(),
        100
    )
    .is_err());
    let mut invalid =
        String::from_utf8(api(0, 3, &["2408.12345v1", "2408.12346v1", "2408.12347v1"])).unwrap();
    invalid = invalid.replace(
        "https://arxiv.org/abs/2408.12347v1",
        "https://arxiv.org/abs/2408.99999v1",
    );
    let adapter = provider(vec![raw(invalid.into_bytes())]);
    assert!(run(adapter.acquire(request(1))).is_err());
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    let wrong_namespace = String::from_utf8(api(0, 0, &[])).unwrap().replace(
        "http://a9.com/-/spec/opensearch/1.1/",
        "https://wrong.example/",
    );
    let adapter = provider(vec![raw(wrong_namespace.into_bytes())]);
    assert!(run(adapter.acquire(request(2))).is_err());
}

#[test]
fn account_owned_source_uses_authorized_profile_partition() {
    let adapter = provider(vec![raw(api(0, 1, &["2408.12345v1"]))]);
    let authorized = request(90);
    assert_ne!(adapter.source.owner_scope, authorized.subscription.partition.scope);
    let page = run(adapter.acquire(authorized.clone())).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(adapter.source.owner_scope, ScopeRef("account".into()));
    assert_eq!(page.partition, authorized.subscription.partition);
    assert_eq!(page.entries[0].capture.access_partition, authorized.subscription.partition.scope);
    assert_eq!(page.entries[0].observation.access_partition, authorized.subscription.partition.scope);
    for invalid in 0..4 {
        let mut adapter = provider(vec![]);
        let mut request = request(90);
        match invalid {
            0 => request.subscription.source_ref = reference("unselected-source"),
            1 => request.subscription.partition.scope = ScopeRef("foreign".into()),
            2 => adapter.source.owner_scope = ScopeRef(String::new()),
            _ => adapter.source.owner_scope = ScopeRef("x".repeat(request.bounds.max_text_bytes + 1)),
        }
        assert!(matches!(run(adapter.acquire(request)), Err(rss_contracts::Error::Denied | rss_contracts::Error::LimitExceeded { .. } | rss_contracts::Error::ItemOwner { error: hsscrap::Error::LimitExceeded { .. } })));
        assert!(adapter.fetch.requests.lock().unwrap().is_empty());
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    }
}
