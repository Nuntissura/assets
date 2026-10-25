use std::{collections::{BTreeMap,BTreeSet,VecDeque}, future::Future,pin::pin,sync::{Arc,Mutex,atomic::{AtomicU64,Ordering}},task::{Context,Poll,Wake,Waker}};
use hsscrap::*;
use rss_contracts::{AccessMode,AccessPartition,ObservedEntry,Pending,Provider,ProviderPort,Source,SourceRequest,Subscription};
use rss_provider_instagram::*;
use serde_json::{Value,json};

fn run<T>(future: impl Future<Output=T>) -> T {
    struct Noop; impl Wake for Noop { fn wake(self:Arc<Self>) {} }
    let waker = Waker::from(Arc::new(Noop)); let mut context = Context::from_waker(&waker); let mut future = pin!(future);
    match future.as_mut().poll(&mut context) { Poll::Ready(value) => value,Poll::Pending => panic!("unexpected asynchronous fixture") }
}
fn id<T:std::str::FromStr>(n:u64) -> T where T::Err:std::fmt::Debug { format!("00000000-0000-0000-0000-{n:012}").parse().unwrap() }
fn reference(value:&str) -> HostRef { HostRef { host_instance_id:"fixture".into(),owner_kind:"fixture".into(),native_id:value.into() } }
fn at() -> Instant { Instant::parse("2026-10-23T00:00:00Z").unwrap() }
fn bounds() -> Bounds { Bounds { max_url_bytes:4096,max_text_bytes:10000,max_collection_items:100,max_page_items:2,max_operation_bytes:100000 } }
fn request(mode:AccessMode,n:u64) -> SourceRequest { SourceRequest { subscription:Subscription { subscription_ref:reference("subscription"),source_ref:reference("source"),
    partition:AccessPartition { scope:ScopeRef("scope".into()),mode,connection_ref:None },executor_ref:reference("executor"),generation:Revision(1),paused:false,common:Extensions::default() },
    operation_id:id(n),continuation:None,bounds:bounds() } }
fn identity() -> Value { json!({"status":"ok","data":{"user":{"id":"123456","username":"kngemma","is_private":false,"media_count":9}}}) }
fn member(n:u64,video:bool) -> Value { json!({"pk":n.to_string(),"media_type":if video {2} else {1},
    "image_versions2":{"candidates":[{"url":format!("https://media.cdninstagram.com/{n}.jpg"),"width":1080,"height":1350}]},
    "video_versions":if video {json!([{"url":format!("https://media.cdninstagram.com/{n}.mp4"),"width":1080,"height":1920}])} else {Value::Null}}) }
fn item(n:u64) -> Value { let mut value = member(n,false); value["code"] = json!(format!("Code{n}")); value["id"] = json!(format!("{n}_123456"));
    value["user"] = json!({"pk":"123456","username":"kngemma"}); value["taken_at"] = json!(1760000000); value["caption"] = json!({"text":"Caption"}); value }
fn page(mode:AccessMode,items:Vec<Value>,next:Option<&str>) -> Value {
    if mode == AccessMode::PublicProfile { json!({"status":"ok","items":items,"more_available":next.is_some(),"next_max_id":next}) }
    else { json!({"status":"ok","items":items.into_iter().map(|media| json!({"media":media})).collect::<Vec<_>>(),"paging_info":{"more_available":next.is_some(),"max_id":next}}) }
}
struct Reply { value:Value,status:u16 }
fn ok(value:Value) -> Reply { Reply {value,status:200} }
#[derive(Default)]
struct Fetch { replies:Mutex<VecDeque<Reply>>,requests:Mutex<Vec<FetchRequest>> }
impl PublicFetchPort for Fetch {
    fn fetch<'a>(&'a self,source:&'a SourceRequest,_:&'a HostRef,request:FetchRequest) -> Pending<'a,FetchResponse> {
        Box::pin(async move { if source.subscription.partition.scope.0 != "scope" { return Err(rss_contracts::Error::Denied); } let reply = self.replies.lock().unwrap().pop_front().ok_or(rss_contracts::Error::Network)?;
            let result = FetchResponse {final_url:request.url.clone(),status:reply.status,body:serde_json::to_vec(&reply.value).unwrap(),cached_body:None,observed_at:at()};
            self.requests.lock().unwrap().push(request); Ok(result) })
    }
}
#[derive(Default)]
struct Canonical { identities:Mutex<BTreeMap<String,ItemId>>,calls:AtomicU64 }
impl CanonicalEntryPort for Canonical {
    fn prepare<'a>(&'a self,request:&'a SourceRequest,_:&'a HostRef,evidence:EntryEvidence) -> Pending<'a,ObservedEntry> {
        Box::pin(async move {
            let n = self.calls.fetch_add(1,Ordering::SeqCst)+1;
            let mut identities = self.identities.lock().unwrap(); let next = id(1000+identities.len() as u64);
            let item_id = *identities.entry(evidence.reliable_provider_identity()).or_insert(next);
            let normalized = hsscrap::identity::normalize_url(&evidence.url,&request.bounds)?;
            let capture = URLCapture {capture_id:id(100+n),item_id,capture_kind:CaptureKind::SourceIngest,access_partition:request.subscription.partition.scope.clone(),
                captured_url:evidence.url.clone(),original_url:Some(evidence.url.clone()),identity_url:normalized.identity_url,normalization_version:normalized.normalization_version,
                captured_at:Some(at()),created_here_at:at(),actor_ref:Some(reference("actor")),operation_id:id(10000+n),common:Extensions::default()};
            let mut observation = MetadataObservation {observation_id:id(200+n),item_id,stream_id:id(300+n),sequence:Revision(n),baseline_observation_id:None,capture_id:Some(capture.capture_id),
                appearance_ref:Some(reference("appearance")),producer:"fixture".into(),access_partition:request.subscription.partition.scope.clone(),observed_at:at(),source_revision:None,
                canonical_url_assertion:None,provider:None,host_site:"www.instagram.com".into(),title:None,description:None,author:None,published_at:None,provider_labels:vec![],
                gallery_snapshot:None,source_payload_digest:None,status:Availability {state:AvailabilityState::Available,checked_at:Some(at()),reason_code:None,retry_after:None},common:Extensions::default()};
            evidence.populate(&mut observation); Ok(ObservedEntry {provider_entry_key:evidence.key,capture,observation})
        })
    }
}
fn provider(mode:AccessMode,replies:Vec<Reply>) -> InstagramProvider<Fetch,Canonical> {
    InstagramProvider {source:Source {source_ref:reference("source"),owner_scope:ScopeRef("account".into()),provider:Provider::Instagram,
        original_url:if mode == AccessMode::PublicProfile {"https://www.instagram.com/kngemma"} else {"https://www.instagram.com/kngemma/reels/"}.into(),display_name:"kngemma".into(),
        revision:Revision(1),paused:false,common:Extensions::default()},execution_ref:reference("attempt"),limits:ProviderLimits {max_body_bytes:100000,max_document_entries:100,max_documents:20,max_fetches_per_operation:4,timeout_millis:10000},
        supported_extensions:BTreeSet::new(),fetch:Fetch {replies:Mutex::new(replies.into()),requests:Mutex::new(vec![])},canonical:Canonical::default()}
}

#[test]
fn resolve_source_normalizes_profile_and_reels() {
    for (input, mode, expected) in [
        ("https://instagram.com/KngEmma", AccessMode::PublicProfile, "https://www.instagram.com/kngemma/"),
        ("https://www.instagram.com/KngEmma/", AccessMode::PublicProfile, "https://www.instagram.com/kngemma/"),
        ("https://instagram.com/KngEmma/reels", AccessMode::PublicReels, "https://www.instagram.com/kngemma/reels/"),
        ("https://www.instagram.com/KngEmma/reels/", AccessMode::PublicReels, "https://www.instagram.com/kngemma/reels/"),
    ] {
        assert_eq!(public::resolve_source(input, mode, &bounds()).unwrap(), (expected.into(), "kngemma".into()));
    }
}

#[test]
fn resolve_source_rejects_wrong_mode_and_other_targets() {
    for (input, mode) in [
        ("https://www.instagram.com/kngemma/reels/", AccessMode::PublicProfile),
        ("https://www.instagram.com/kngemma/", AccessMode::PublicReels),
        ("https://www.instagram.com/kngemma/", AccessMode::PublicFeed),
        ("https://www.instagram.com/kngemma/posts/", AccessMode::PublicProfile),
        ("https://www.instagram.com/", AccessMode::PublicProfile),
    ] {
        assert!(public::resolve_source(input, mode, &bounds()).is_err(), "{input}: {mode:?}");
    }
}

#[test]
fn resolve_source_rejects_foreign_authority_credentials_and_query() {
    for input in [
        "https://example.org/kngemma/",
        "https://www.instagram.com.example.org/kngemma/",
        "http://www.instagram.com/kngemma/",
        "https://user@www.instagram.com/kngemma/",
        "https://user:password@www.instagram.com/kngemma/",
        "https://www.instagram.com:8443/kngemma/",
        "https://www.instagram.com/kngemma/?page=2",
        "https://www.instagram.com/kngemma/#reels",
    ] {
        assert!(public::resolve_source(input, AccessMode::PublicProfile, &bounds()).is_err(), "{input}");
    }
}

#[test]
fn profile_and_reels_preserve_global_media_identity_with_distinct_modes() {
    let post = item(900001);
    let p = normalize::account(&serde_json::to_vec(&identity()).unwrap(),"kngemma",100000,&bounds()).unwrap();
    let profile = normalize::document(&serde_json::to_vec(&page(AccessMode::PublicProfile,vec![post.clone()],None)).unwrap(),AccessMode::PublicProfile,&p,100000,100,&bounds()).unwrap();
    let reels = normalize::document(&serde_json::to_vec(&page(AccessMode::PublicReels,vec![post],None)).unwrap(),AccessMode::PublicReels,&p,100000,100,&bounds()).unwrap();
    assert_eq!(profile.entries[0].reliable_provider_identity(),reels.entries[0].reliable_provider_identity());
    assert_eq!(profile.entries[0].url,reels.entries[0].url);
    for mode in [AccessMode::PublicProfile,AccessMode::PublicReels] {
        let adapter = provider(mode,vec![ok(identity()),ok(page(mode,vec![item(900001)],None))]);
        let result = run(adapter.acquire(request(mode,1))).unwrap(); assert_eq!(result.entries.len(),1); assert_eq!(result.coverage,Coverage::Partial);
        let calls = adapter.fetch.requests.lock().unwrap();
        assert_eq!(calls[1].method,if mode == AccessMode::PublicProfile {Method::Get} else {Method::Post});
        if mode == AccessMode::PublicReels { assert!(calls[1].form.contains(&("target_user_id".into(),"123456".into()))); }
    }
}

#[test]
fn both_pagination_schemas_deduplicate_and_finish_without_full_history_claim() {
    for mode in [AccessMode::PublicProfile,AccessMode::PublicReels] {
        let adapter = provider(mode,vec![ok(identity()),ok(page(mode,vec![item(1),item(2)],Some("next"))),ok(identity()),ok(page(mode,vec![item(2),item(3)],None))]);
        let first = run(adapter.acquire(request(mode,1))).unwrap();
        let mut next = request(mode,2); next.continuation = first.continuation; let last = run(adapter.acquire(next)).unwrap();
        assert_eq!(last.entries.len(),1); assert_eq!(last.entries[0].provider_entry_key,"3"); assert!(last.continuation.is_none());
        assert!(matches!(last.state,rss_contracts::AcquisitionState::Partial {ref reason_code,..} if reason_code == "instagram_current_traversal_only"));
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),3);
    }
}

#[test]
fn retained_snapshot_resume_refresh_and_missing_snapshot_are_fenced() {
    let mode = AccessMode::PublicProfile; let bytes = page(mode,vec![item(1),item(2),item(3)],None);
    let adapter = provider(mode,vec![ok(identity()),ok(bytes.clone()),ok(identity()),ok(bytes.clone()),ok(identity()),ok(bytes)]);
    let first = run(adapter.acquire(request(mode,1))).unwrap(); let mut next = request(mode,2); next.continuation = first.continuation;
    let resumed = run(adapter.acquire(next)).unwrap(); assert_eq!(resumed.entries[0].provider_entry_key,"3");
    let refresh = run(adapter.acquire(request(mode,3))).unwrap(); assert_eq!(refresh.entries[0].capture.item_id,first.entries[0].capture.item_id);
    assert!(adapter.fetch.requests.lock().unwrap()[3].expected_digest.is_some());
    let changed = provider(mode,vec![ok(identity()),ok(page(mode,vec![item(1),item(2),item(3)],None)),ok(identity()),ok(page(mode,vec![item(1),item(2),item(4)],None))]);
    let first = run(changed.acquire(request(mode,4))).unwrap(); let mut next = request(mode,5); next.continuation = first.continuation;
    assert!(matches!(run(changed.acquire(next)),Err(rss_contracts::Error::StaleResult))); assert_eq!(changed.canonical.calls.load(Ordering::SeqCst),2);
}

#[test]
fn collaborations_separate_selected_account_and_primary_author() {
    let account = normalize::account(&serde_json::to_vec(&identity()).unwrap(),"kngemma",100000,&bounds()).unwrap();
    let mut post = item(99); post["user"] = json!({"pk":"654321","username":"collaborator"}); post["id"] = json!("99_654321");
    post["coauthor_producers"] = json!([{"id":"123456","username":"kngemma"}]);
    let result = normalize::document(&serde_json::to_vec(&page(AccessMode::PublicProfile,vec![post],None)).unwrap(),AccessMode::PublicProfile,&account,100000,100,&bounds()).unwrap();
    assert_eq!(result.entries[0].author,"collaborator"); assert_eq!(result.entries[0].owner_id,"654321"); assert_eq!(result.entries[0].account_id,"123456");
    assert_eq!(result.entries[0].reliable_provider_identity(),"instagram/media/99");
    let mut post = item(99); post["user"] = json!({"pk":"654321","username":"collaborator"}); post["id"] = json!("99_654321");
    // The selected source endpoint is authoritative for membership even when optional coauthors are omitted.
    let result = normalize::document(&serde_json::to_vec(&page(AccessMode::PublicProfile,vec![post],None)).unwrap(),AccessMode::PublicProfile,&account,100000,100,&bounds()).unwrap();
    assert_eq!(result.entries[0].author,"collaborator");
}

#[test]
fn ordered_carousels_normalize_video_duration_expiry_and_publication() {
    let account = normalize::account(&serde_json::to_vec(&identity()).unwrap(),"kngemma",100000,&bounds()).unwrap();
    let mut post = item(99); post["media_type"] = json!(8); let mut video = member(12,true); video["video_duration"] = json!(1.25); video["expiring_at"] = json!(1999999999);
    post["carousel_media"] = json!([member(11,false),video]); post["carousel_media_count"] = json!(2);
    let result = normalize::document(&serde_json::to_vec(&page(AccessMode::PublicProfile,vec![post],None)).unwrap(),AccessMode::PublicProfile,&account,100000,100,&bounds()).unwrap();
    let evidence = &result.entries[0]; assert_eq!(evidence.media.iter().map(|m|m.provider_media_id.as_str()).collect::<Vec<_>>(),vec!["11","12"]);
    assert_eq!(evidence.media[1].duration_ms,Some(1250)); assert_eq!(evidence.media[1].expires_at_unix,Some(1999999999));
    assert_eq!(evidence.published_at.as_ref().unwrap().offset_minutes,Some(0)); assert_eq!(evidence.published_at.as_ref().unwrap().raw_value,"1760000000");
}

#[test]
fn login_challenge_rate_expiry_malformed_and_empty_reels_never_succeed() {
    let mode = AccessMode::PublicReels;
    for reply in [Reply {value:json!({}),status:429},Reply {value:json!({}),status:410},ok(json!({"status":"fail","message":"login_required"})),
        ok(json!({"status":"fail","message":"challenge_required"})),ok(json!({"status":"ok","items":[]})),ok(page(mode,vec![],None))] {
        let adapter = provider(mode,vec![ok(identity()),reply]); assert!(run(adapter.acquire(request(mode,1))).is_err()); assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),0);
    }
    assert!(normalize::parse_json(br#"{"status":"ok","status":"fail"}"#,10000).is_err());
    let numeric = normalize::parse_json(br#"{"status":"ok","pk":9007199254740993}"#,10000).unwrap();
    assert_eq!(numeric["pk"].as_u64(),Some(9_007_199_254_740_993));
    let account = normalize::account(&serde_json::to_vec(&identity()).unwrap(),"kngemma",100000,&bounds()).unwrap();
    let mut large = item(9_007_199_254_740_993); large["pk"] = json!(9_007_199_254_740_993u64);
    let parsed = normalize::document(&serde_json::to_vec(&page(AccessMode::PublicProfile,vec![large],None)).unwrap(),AccessMode::PublicProfile,&account,100000,100,&bounds()).unwrap();
    assert_eq!(parsed.entries[0].reliable_provider_identity(),"instagram/media/9007199254740993");
    let mut expired = item(1); expired["expiring_at"] = json!(1);
    let adapter = provider(mode,vec![ok(identity()),ok(page(mode,vec![expired],None))]);
    assert!(matches!(run(adapter.acquire(request(mode,1))),Err(rss_contracts::Error::Expired)));
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),0);
}

#[test]
fn scope_mode_account_and_cursor_cycle_fail_before_owner_preparation() {
    let mode = AccessMode::PublicProfile;
    let adapter = provider(mode,vec![]); let mut foreign = request(mode,1); foreign.subscription.partition.scope = ScopeRef("foreign".into());
    assert!(matches!(run(adapter.acquire(foreign)),Err(rss_contracts::Error::Denied))); assert!(adapter.fetch.requests.lock().unwrap().is_empty());
    let mut adapter = provider(mode,vec![]); adapter.source.original_url = "https://www.instagram.com/kngemma/reels/".into(); assert!(run(adapter.acquire(request(mode,1))).is_err());
    let mut retargeted = identity(); retargeted["data"]["user"]["id"] = json!("987654");
    let adapter = provider(mode,vec![ok(identity()),ok(page(mode,vec![item(1)],Some("next"))),ok(retargeted)]);
    let first = run(adapter.acquire(request(mode,1))).unwrap(); let mut next = request(mode,2); next.continuation = first.continuation;
    assert!(matches!(run(adapter.acquire(next)),Err(rss_contracts::Error::StaleResult))); assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),1);
    let adapter = provider(mode,vec![ok(identity()),ok(page(mode,vec![item(1)],Some("next"))),ok(identity()),ok(page(mode,vec![item(2)],Some("next")))]);
    let first = run(adapter.acquire(request(mode,1))).unwrap(); let mut next = request(mode,2); next.continuation = first.continuation.clone();
    assert!(run(adapter.acquire(next)).is_err()); assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),1);
    for changed in ["generation","executor","source"] {
        let mut stale = request(mode,3); stale.continuation = first.continuation.clone();
        match changed { "generation" => stale.subscription.generation = Revision(2),"executor" => stale.subscription.executor_ref = reference("other"),_ => stale.subscription.source_ref = reference("other") }
        assert!(run(adapter.acquire(stale)).is_err());
    }
    assert_eq!(adapter.fetch.requests.lock().unwrap().len(),4);
}

#[test]
fn malformed_later_entry_and_retargeted_id_fail_before_any_new_effects() {
    let mode = AccessMode::PublicProfile;
    let mut malformed = item(3); malformed["image_versions2"] = json!({"candidates":[]});
    let adapter = provider(mode,vec![ok(identity()),ok(page(mode,vec![item(1),item(2),malformed],None))]);
    assert!(run(adapter.acquire(request(mode,1))).is_err()); assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),0);
    let mut changed = item(1); changed["code"] = json!("Changed");
    let adapter = provider(mode,vec![ok(identity()),ok(page(mode,vec![item(1)],Some("next"))),ok(identity()),ok(page(mode,vec![changed],None))]);
    let first = run(adapter.acquire(request(mode,1))).unwrap(); let mut next = request(mode,2); next.continuation = first.continuation;
    assert!(run(adapter.acquire(next)).is_err()); assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),1);
}

#[test]
fn explicit_zero_profile_and_terminal_empty_archive_have_distinct_meanings() {
    let mode = AccessMode::PublicProfile; let mut empty_identity = identity(); empty_identity["data"]["user"]["media_count"] = json!(0);
    let empty = provider(mode,vec![ok(empty_identity),ok(page(mode,vec![],None))]); let result = run(empty.acquire(request(mode,1))).unwrap();
    assert!(matches!(result.state,rss_contracts::AcquisitionState::VerifiedEmpty {..}));
    let adapter = provider(mode,vec![ok(identity()),ok(page(mode,vec![item(1)],Some("next"))),ok(identity()),ok(page(mode,vec![],None))]);
    let first = run(adapter.acquire(request(mode,1))).unwrap(); let mut next = request(mode,2); next.continuation = first.continuation;
    assert!(run(adapter.acquire(next)).unwrap().is_terminal_partial_document());
}

#[test]
fn account_owned_source_uses_authorized_profile_partition() {
    let adapter = provider(AccessMode::PublicProfile, vec![ok(identity()), ok(page(AccessMode::PublicProfile, vec![item(1)], None))]);
    let authorized = request(AccessMode::PublicProfile, 90);
    assert_ne!(adapter.source.owner_scope, authorized.subscription.partition.scope);
    let page = run(adapter.acquire(authorized.clone())).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(adapter.source.owner_scope, ScopeRef("account".into()));
    assert_eq!(page.partition, authorized.subscription.partition);
    assert_eq!(page.entries[0].capture.access_partition, authorized.subscription.partition.scope);
    assert_eq!(page.entries[0].observation.access_partition, authorized.subscription.partition.scope);
    for invalid in 0..4 {
        let mut adapter = provider(AccessMode::PublicProfile, vec![]);
        let mut request = request(AccessMode::PublicProfile, 90);
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

#[test]
fn public_request_metadata_is_fixed_anonymous_and_honest() {
    let request = FetchRequest {
        method: Method::Get,
        url: "https://www.instagram.com/api/v1/users/web_profile_info/?username=fixture".into(),
        form: vec![],
        expected_digest: None,
        max_body_bytes: 100_000,
        timeout_millis: 10_000,
    };
    let metadata = request.public_metadata();
    metadata.validate().unwrap();
    let headers = metadata.header_pairs();
    assert_eq!(headers, [
        ("User-Agent", concat!("rss-provider-instagram/", env!("CARGO_PKG_VERSION"))),
        ("Accept", "*/*"),
        ("X-IG-App-ID", "936619743392459"),
        ("Referer", "https://www.instagram.com/"),
        ("X-Requested-With", "XMLHttpRequest"),
    ]);
    let value = serde_json::to_value(&metadata).unwrap();
    let decoded: PublicRequestMetadata = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded, metadata);
    decoded.validate().unwrap();
    for field in ["user_agent", "accept", "app_id", "referer", "requested_with"] {
        let mut changed = value.clone();
        changed[field] = json!("changed");
        let changed: PublicRequestMetadata = serde_json::from_value(changed).unwrap();
        assert!(changed.validate().is_err());
    }
    for field in ["cookie", "authorization", "csrf_token", "claim"] {
        let mut changed = value.clone();
        changed[field] = json!("forbidden");
        assert!(serde_json::from_value::<PublicRequestMetadata>(changed).is_err());
    }
}
