use hsscrap::*;
use rss_contracts::{
    AccessMode, AccessPartition, ObservedEntry, Pending, Provider, ProviderPort, Source,
    SourceRequest, Subscription,
};
use rss_provider_pinterest::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
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
                mode: AccessMode::PublicBoard,
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
fn ok(value: Value) -> Reply {
    raw(serde_json::to_vec(&value).unwrap())
}
#[derive(Default)]
struct Fetch {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<FetchRequest>>,
}
impl PublicFetchPort for Fetch {
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
}
impl CanonicalEntryPort for Canonical {
    fn prepare<'a>(
        &'a self,
        request: &'a SourceRequest,
        _: &'a HostRef,
        evidence: EntryEvidence,
    ) -> Pending<'a, ObservedEntry> {
        Box::pin(async move {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let mut identities = self.identities.lock().unwrap();
            let next = id(1000 + identities.len() as u64);
            let item_id = *identities
                .entry(evidence.reliable_provider_identity())
                .or_insert(next);
            let url = evidence.url();
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
                host_site: "www.pinterest.com".into(),
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
            Ok(ObservedEntry {
                provider_entry_key: evidence.id,
                capture,
                observation,
            })
        })
    }
}
fn provider(replies: Vec<Reply>) -> PinterestProvider<Fetch, Canonical> {
    PinterestProvider {
        source: Source {
            source_ref: reference("source"),
            owner_scope: ScopeRef("account".into()),
            provider: Provider::Pinterest,
            original_url: "https://www.pinterest.com/owner/board/".into(),
            display_name: "Board".into(),
            revision: Revision(1),
            paused: false,
            common: Extensions::default(),
        },
        execution_ref: reference("attempt"),
        limits: ProviderLimits {
            max_body_bytes: 100000,
            max_document_entries: 100,
            max_documents: 20,
            max_fetches_per_operation: 4,
            timeout_millis: 10000,
        },
        supported_extensions: BTreeSet::new(),
        fetch: Fetch {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(vec![]),
        },
        canonical: Canonical::default(),
    }
}
fn board_value() -> Value {
    json!({"id":"700","url":"/owner/board/","name":"Board","owner":{"username":"owner"},"privacy":"public","is_secret":false,"pin_count":5})
}
fn response(data: Value) -> Value {
    json!({"resource_response":{"status":"success","error":null,"data":data}})
}
fn board() -> Reply {
    ok(response(board_value()))
}
fn item(n: u64) -> Value {
    json!({"id":n.to_string(),"type":"pin","title":"Pin","description":"Description","pinner":{"username":"different-author"},"created_at":"2026-10-22T12:13:14.123+02:00",
    "link":"https://example.com/article","images":{"orig":{"url":format!("https://i.pinimg.com/originals/{n}.jpg"),"width":1200,"height":1800}}})
}
fn page(items: Vec<Value>, next: &str) -> Value {
    json!({"resource_response":{"status":"success","data":items},"resource":{"options":{"bookmarks":[next]}}})
}
fn options(request: &FetchRequest) -> Value {
    let url = url::Url::parse(&request.url).unwrap();
    let data = url
        .query_pairs()
        .find(|(key, _)| key == "data")
        .unwrap()
        .1
        .into_owned();
    serde_json::from_str::<Value>(&data).unwrap()["options"].clone()
}

#[test]
fn board_bookmarks_deduplicate_global_ids_and_refresh_without_author_filter() {
    let adapter = provider(vec![
        board(),
        ok(page(vec![item(1), item(2)], "opaque-next")),
        board(),
        ok(page(vec![item(2), item(3)], "-end-")),
        board(),
        ok(page(vec![item(1)], "-end-")),
    ]);
    let first = run(adapter.acquire(request(1))).unwrap();
    let mut next = request(2);
    next.continuation = first.continuation;
    let last = run(adapter.acquire(next)).unwrap();
    assert_eq!(last.entries.len(), 1);
    assert_eq!(last.entries[0].provider_entry_key, "3");
    assert!(last.continuation.is_none());
    assert_eq!(last.coverage, Coverage::Partial);
    let refresh = run(adapter.acquire(request(3))).unwrap();
    assert_eq!(
        refresh.entries[0].capture.item_id,
        first.entries[0].capture.item_id
    );
    assert_eq!(
        refresh.entries[0].observation.author.as_deref(),
        Some("different-author")
    );
    let calls = adapter.fetch.requests.lock().unwrap();
    assert!(calls[1].url.contains("BoardFeedResource"));
    assert_eq!(options(&calls[3])["bookmarks"], json!(["opaque-next"]));
}
#[test]
fn retained_document_offsets_require_exact_snapshot_and_resume_with_304() {
    let data = page(vec![item(1), item(2), item(3)], "-end-");
    let adapter = provider(vec![
        board(),
        ok(data.clone()),
        board(),
        Reply {
            status: 304,
            body: vec![],
            cached: Some(serde_json::to_vec(&data).unwrap()),
        },
    ]);
    let first = run(adapter.acquire(request(1))).unwrap();
    let mut next = request(2);
    next.continuation = first.continuation;
    assert_eq!(
        run(adapter.acquire(next)).unwrap().entries[0].provider_entry_key,
        "3"
    );
    assert!(
        adapter.fetch.requests.lock().unwrap()[3]
            .expected_digest
            .is_some()
    );
    let adapter = provider(vec![
        board(),
        ok(data),
        board(),
        ok(page(vec![item(1), item(2), item(4)], "-end-")),
    ]);
    let first = run(adapter.acquire(request(1))).unwrap();
    let mut next = request(2);
    next.continuation = first.continuation;
    assert!(matches!(
        run(adapter.acquire(next)),
        Err(rss_contracts::Error::StaleResult)
    ));
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 2);
}
#[test]
fn anonymous_discovery_and_allpins_reject_bound_connection_before_callbacks() {
    for discovery in [true, false] {
        let value = if discovery { board_value() } else { item(1) };
        let mut adapter = provider(vec![ok(page(vec![value], "-end-"))]);
        adapter.source.original_url = if discovery {
            "https://www.pinterest.com/owner/"
        } else {
            "https://www.pinterest.com/owner/pins/"
        }.into();
        let mut req = request(1);
        req.subscription.partition.mode = AccessMode::PublicProfile;
        req.subscription.partition.connection_ref = Some(reference("connection"));
        req.subscription.partition.validate(&req.bounds).unwrap();
        if discovery {
            assert!(matches!(run(adapter.discover_boards(req)), Err(rss_contracts::Error::Denied)));
        } else {
            assert!(matches!(run(adapter.acquire(req)), Err(rss_contracts::Error::Denied)));
        }
        assert!(adapter.fetch.requests.lock().unwrap().is_empty());
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
        assert!(adapter.canonical.identities.lock().unwrap().is_empty());
    }
}

#[test]
fn discovery_is_separate_and_returns_bounded_public_board_evidence() {
    let mut second = board_value();
    second["id"] = json!("701");
    second["url"] = json!("/owner/another/");
    second["name"] = json!("Another");
    let mut adapter = provider(vec![ok(page(vec![board_value(), second], "-end-"))]);
    adapter.source.original_url = "https://www.pinterest.com/owner/".into();
    let mut req = request(1);
    req.subscription.partition.mode = AccessMode::PublicProfile;
    let result = run(adapter.discover_boards(req)).unwrap();
    assert_eq!(result.sources.len(), 2);
    assert_eq!(
        result.sources[1].url,
        "https://www.pinterest.com/owner/another/"
    );
    assert_eq!(result.coverage, Coverage::Partial);
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    let mut req = request(2);
    req.subscription.partition.mode = AccessMode::PublicProfile;
    assert!(run(adapter.acquire(req)).is_err());
    let mut private = board_value();
    private["is_secret"] = json!(true);
    assert!(
        normalize::discovered_boards(
            &serde_json::to_vec(&page(vec![private], "-end-")).unwrap(),
            100000,
            100,
            &bounds()
        )
        .is_err()
    );
}
#[test]
fn section_allpins_and_single_pin_keep_distinct_intent_and_global_pin_identity() {
    let mut adapter = provider(vec![
        board(),
        ok(response(
            json!({"id":"800","board":{"id":"700"},"pin_count":2}),
        )),
        ok(page(vec![item(1)], "-end-")),
    ]);
    adapter.source.original_url = "https://www.pinterest.com/owner/board/section/".into();
    assert_eq!(
        run(adapter.acquire(request(1))).unwrap().entries[0].provider_entry_key,
        "1"
    );
    let calls = adapter.fetch.requests.lock().unwrap();
    assert!(calls[2].url.contains("BoardSectionPinsResource"));
    assert_eq!(options(&calls[2])["section_id"], "800");
    drop(calls);
    let mut adapter = provider(vec![
        ok(page(vec![item(1)], "-end-")),
        ok(response(item(1))),
    ]);
    adapter.source.original_url = "https://www.pinterest.com/owner/pins/".into();
    let mut req = request(2);
    req.subscription.partition.mode = AccessMode::PublicProfile;
    let followed = run(adapter.acquire(req)).unwrap();
    adapter.source.original_url = "https://www.pinterest.com/pin/1/".into();
    let captured = run(adapter.acquire(request(3))).unwrap();
    assert_eq!(
        captured.entries[0].capture.item_id,
        followed.entries[0].capture.item_id
    );
    assert_eq!(
        captured.entries[0].capture.captured_url,
        "https://www.pinterest.com/pin/1/"
    );
    assert_ne!(
        captured.entries[0].capture.captured_url,
        "https://example.com/article"
    );
    assert!(matches!(
        public::resolve_url("https://www.pinterest.com/owner/pins/", &bounds()).unwrap(),
        public::SourceTarget::AllPins(_)
    ));
}
#[test]
fn carousel_and_story_preserve_order_actual_keys_and_explicit_date() {
    let mut carousel = item(1);
    carousel["carousel_data"] = json!({"carousel_slots":[{"id":"a","images":{"736x":{"url":"https://i.pinimg.com/736x/a.jpg"}}},{"id":"b","images":{"736x":{"url":"https://i.pinimg.com/736x/b.jpg"}}}]});
    let parsed = normalize::pin(
        &serde_json::to_vec(&response(carousel.clone())).unwrap(),
        "1",
        100000,
        &bounds(),
    )
    .unwrap();
    let evidence = &parsed.entries[0];
    assert_eq!(evidence.media[0].key.media_id.as_deref(), Some("a"));
    assert!(evidence.media[0].url.contains("/736x/"));
    assert_eq!(evidence.media[1].key.media_id.as_deref(), Some("b"));
    assert_eq!(
        evidence.published_at.as_ref().unwrap().raw_value,
        "2026-10-22T12:13:14.123+02:00"
    );
    carousel["carousel_data"]["carousel_slots"][1]["id"] = json!("a");
    assert!(
        normalize::pin(
            &serde_json::to_vec(&response(carousel)).unwrap(),
            "1",
            100000,
            &bounds()
        )
        .is_err()
    );
    let mut story = item(2);
    story["story_pin_data"] = json!({"pages":[{"id":"page1","blocks":[{"type":"story_pin_paragraph_block","text":"Text"},{"type":"story_pin_image_block","image":{"images":{"originals":{"url":"https://i.pinimg.com/a.jpg"}}}},{"type":"story_pin_image_block","image":{"images":{"originals":{"url":"https://i.pinimg.com/b.jpg"}}}}]}]});
    let parsed = normalize::pin(
        &serde_json::to_vec(&response(story)).unwrap(),
        "2",
        100000,
        &bounds(),
    )
    .unwrap();
    assert_eq!(parsed.entries[0].media.len(), 2);
    assert!(
        parsed.entries[0]
            .media
            .iter()
            .all(|media| media.key.media_id.is_none())
    );
    assert!(parsed.entries[0].media[0].url.ends_with("a.jpg"));
    let mut video_pin = item(3);
    let video = json!({"id":"video-id","video_list":{"small":{"url":"https://v.pinimg.com/small.mp4","width":320,"height":180,"duration":12},"large":{"url":"https://v.pinimg.com/large.mp4","width":1280,"height":720,"duration":12}}});
    video_pin["videos"] = video.clone();
    let parsed = normalize::pin(
        &serde_json::to_vec(&response(video_pin.clone())).unwrap(),
        "3",
        100000,
        &bounds(),
    )
    .unwrap();
    assert_eq!(parsed.entries[0].media[0].kind, MediaKind::Video);
    assert_eq!(
        parsed.entries[0].media[0].url,
        "https://v.pinimg.com/large.mp4"
    );
    assert_eq!(parsed.entries[0].media[0].duration_ms, None);
    video_pin["videos"]["video_list"]["large"]["duration_ms"] = json!(12000);
    let parsed = normalize::pin(
        &serde_json::to_vec(&response(video_pin)).unwrap(),
        "3",
        100000,
        &bounds(),
    )
    .unwrap();
    assert_eq!(parsed.entries[0].media[0].duration_ms, Some(12000));
    let mut story_media = item(4);
    story_media["story_pin_data"] = json!({"pages":[{"id":"page-video-audio","blocks":[
        {"type":"story_pin_video_block","video":video},
        {"type":"story_pin_music_block","audio":{"id":"audio-id","audio_url":"https://v.pinimg.com/music.mp3"}}]}]});
    let parsed = normalize::pin(
        &serde_json::to_vec(&response(story_media)).unwrap(),
        "4",
        100000,
        &bounds(),
    )
    .unwrap();
    assert_eq!(
        parsed.entries[0]
            .media
            .iter()
            .map(|media| media.kind)
            .collect::<Vec<_>>(),
        vec![MediaKind::Video, MediaKind::Audio]
    );
    assert_eq!(
        parsed.entries[0].media[0].url,
        "https://v.pinimg.com/large.mp4"
    );
    assert_eq!(
        parsed.entries[0].media[0].key.media_id.as_deref(),
        Some("video-id")
    );
    assert_eq!(
        parsed.entries[0].media[1].url,
        "https://v.pinimg.com/music.mp3"
    );
    assert_eq!(
        parsed.entries[0].media[1].key.media_id.as_deref(),
        Some("audio-id")
    );
    assert!(
        parsed.entries[0]
            .media
            .iter()
            .all(|media| media.key.page_id.as_deref() == Some("page-video-audio"))
    );
    let mut large = item(1);
    large["id"] = json!(9007199254740993u64);
    large.as_object_mut().unwrap().remove("created_at");
    let parsed = normalize::pin(
        &serde_json::to_vec(&response(large)).unwrap(),
        "9007199254740993",
        100000,
        &bounds(),
    )
    .unwrap();
    assert_eq!(
        parsed.entries[0].reliable_provider_identity(),
        "pinterest/pin/9007199254740993"
    );
    assert!(parsed.entries[0].published_at.is_none());
}
#[test]
fn malformed_later_entry_status_missing_cache_and_bookmarks_fail_before_preparation() {
    let mut malformed = item(3);
    malformed["images"]["orig"]["url"] = json!("javascript:bad");
    let mut missing = page(vec![item(1)], "-end-");
    missing.as_object_mut().unwrap().remove("resource");
    for reply in [
        ok(page(vec![item(1), item(2), malformed], "-end-")),
        ok(missing),
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
        ok(json!({"resource_response":{"error":{"message":"login"}}})),
        raw(b"<html>Verify human</html>".to_vec()),
    ] {
        let adapter = provider(vec![board(), reply]);
        assert!(run(adapter.acquire(request(1))).is_err());
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    }
    let mut private = board_value();
    private["privacy"] = json!("secret");
    let adapter = provider(vec![ok(response(private))]);
    assert!(matches!(
        run(adapter.acquire(request(1))),
        Err(rss_contracts::Error::Denied)
    ));
}
#[test]
fn scope_generation_retargeted_board_and_looped_bookmark_are_fenced() {
    let adapter = provider(vec![]);
    let mut foreign = request(1);
    foreign.subscription.partition.scope = ScopeRef("foreign".into());
    assert!(matches!(
        run(adapter.acquire(foreign)),
        Err(rss_contracts::Error::Denied)
    ));
    assert!(adapter.fetch.requests.lock().unwrap().is_empty());
    let mut retarget = board_value();
    retarget["id"] = json!("900");
    let adapter = provider(vec![
        board(),
        ok(page(vec![item(1)], "next")),
        ok(response(retarget)),
    ]);
    let first = run(adapter.acquire(request(1))).unwrap();
    let mut stale = request(2);
    stale.continuation = first.continuation.clone();
    stale.subscription.generation = Revision(2);
    assert!(matches!(
        run(adapter.acquire(stale)),
        Err(rss_contracts::Error::StaleResult)
    ));
    let mut next = request(3);
    next.continuation = first.continuation;
    assert!(matches!(
        run(adapter.acquire(next)),
        Err(rss_contracts::Error::StaleResult)
    ));
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 1);
    let adapter = provider(vec![
        board(),
        ok(page(vec![item(1)], "next")),
        board(),
        ok(page(vec![item(2)], "next")),
    ]);
    let first = run(adapter.acquire(request(4))).unwrap();
    let mut next = request(5);
    next.continuation = first.continuation;
    assert!(run(adapter.acquire(next)).is_err());
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn explicit_zero_board_and_terminal_continuation_are_not_false_full_history() {
    let mut zero = board_value();
    zero["pin_count"] = json!(0);
    let adapter = provider(vec![ok(response(zero)), ok(page(vec![], "-end-"))]);
    assert!(matches!(
        run(adapter.acquire(request(1))).unwrap().state,
        rss_contracts::AcquisitionState::VerifiedEmpty { .. }
    ));
    let adapter = provider(vec![
        board(),
        ok(page(vec![item(1)], "next")),
        board(),
        ok(page(vec![], "Y2JOb25lOterminal")),
    ]);
    let first = run(adapter.acquire(request(2))).unwrap();
    let mut next = request(3);
    next.continuation = first.continuation;
    assert!(
        run(adapter.acquire(next))
            .unwrap()
            .is_terminal_partial_document()
    );
    let adapter = provider(vec![board(), ok(page(vec![], "-end-"))]);
    assert!(run(adapter.acquire(request(4))).is_err());
    assert!(
        normalize::page(
            &serde_json::to_vec(&page(vec![], "next")).unwrap(),
            100000,
            100,
            &bounds()
        )
        .is_err()
    );
}

#[test]
fn account_owned_source_uses_authorized_profile_partition() {
    let adapter = provider(vec![board(), ok(page(vec![item(1)], "-end-"))]);
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

#[path="connected/mod.rs"] mod connected;

#[test]
fn provider_capabilities_public_are_available_without_execution_ports() {
    let expected = vec![
        rss_contracts::Capability { provider: rss_contracts::Provider::Pinterest, mode: AccessMode::PublicBoard, status: rss_contracts::CapabilityStatus::Available,
            paginated: true, media_metadata: true, adapter_version: "pinterest.public.v1".into() },
        rss_contracts::Capability { provider: rss_contracts::Provider::Pinterest, mode: AccessMode::PublicProfile, status: rss_contracts::CapabilityStatus::Available,
            paginated: true, media_metadata: true, adapter_version: "pinterest.public.v1".into() }
    ];
    assert_eq!(rss_provider_pinterest::capabilities(), expected);
    assert_eq!(provider(vec![]).capabilities(), expected);
}
