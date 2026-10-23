use std::{collections::{BTreeMap,BTreeSet,VecDeque},future::Future,pin::pin,sync::{Arc,Mutex,atomic::{AtomicU64,Ordering}},task::{Context,Poll,Wake,Waker}};
use hsscrap::*;
use rss_contracts::{AccessMode,AccessPartition,ObservedEntry,Pending,Provider,ProviderPort,Source,SourceRequest,Subscription};
use rss_provider_tiktok::*;
use serde_json::{Value,json};
const SEC:&str="MS4wLjABAAAAfixturepublicidentity";
const START:u64=1_790_000_000_000;
fn run<T>(future:impl Future<Output=T>)->T {struct Noop;impl Wake for Noop {fn wake(self:Arc<Self>) {}}
    let waker=Waker::from(Arc::new(Noop));let mut context=Context::from_waker(&waker);let mut future=pin!(future);
    match future.as_mut().poll(&mut context) {Poll::Ready(value)=>value,Poll::Pending=>panic!("unexpected asynchronous fixture")}}
fn id<T:std::str::FromStr>(n:u64)->T where T::Err:std::fmt::Debug {format!("00000000-0000-0000-0000-{n:012}").parse().unwrap()}
fn reference(value:&str)->HostRef {HostRef {host_instance_id:"fixture".into(),owner_kind:"fixture".into(),native_id:value.into()}}
fn at()->Instant {Instant::parse("2026-10-23T00:00:00Z").unwrap()}
fn bounds()->Bounds {Bounds {max_url_bytes:4096,max_text_bytes:10000,max_collection_items:100,max_page_items:2,max_operation_bytes:100000}}
fn request(n:u64)->SourceRequest {SourceRequest {subscription:Subscription {subscription_ref:reference("subscription"),source_ref:reference("source"),partition:AccessPartition {
    scope:ScopeRef("scope".into()),mode:AccessMode::PublicProfile,connection_ref:None},executor_ref:reference("executor"),generation:Revision(1),paused:false,common:Extensions::default()},operation_id:id(n),continuation:None,bounds:bounds()}}
fn author()->Value {json!({"id":"123456","secUid":SEC,"uniqueId":"creator","privateAccount":false})}
fn html(scope:Value)->Vec<u8> {format!("<html><script id='__UNIVERSAL_DATA_FOR_REHYDRATION__' type='application/json'>{}</script></html>",json!({"__DEFAULT_SCOPE__":scope})).into_bytes()}
fn profile()->Vec<u8> {html(json!({"webapp.user-detail":{"statusCode":0,"userInfo":{"user":author(),"stats":{"videoCount":9}}}}))}
fn item(n:u64)->Value {json!({"id":n.to_string(),"author":author(),"desc":"Caption","createTime":1_760_000_000-n,
    "video":{"id":format!("remote{n}"),"playAddr":format!("https://media.tiktokcdn.com/{n}.mp4"),"cover":format!("https://media.tiktokcdn.com/{n}.jpg"),"width":1080,"height":1920,"duration":12}})}
fn window(items:Vec<Value>,more:bool)->Value {json!({"statusCode":0,"itemList":items,"hasMorePrevious":more})}
struct Reply {body:Vec<u8>,status:u16,cached:Option<Vec<u8>>}
fn raw(body:Vec<u8>)->Reply {Reply {body,status:200,cached:None}}
fn ok(value:Value)->Reply {raw(serde_json::to_vec(&value).unwrap())}
#[derive(Default)]
struct Fetch {replies:Mutex<VecDeque<Reply>>,requests:Mutex<Vec<FetchRequest>>}
impl PublicFetchPort for Fetch {fn fetch<'a>(&'a self,_:&'a SourceRequest,_:&'a HostRef,request:FetchRequest)->Pending<'a,FetchResponse> {
    Box::pin(async move {let reply=self.replies.lock().unwrap().pop_front().ok_or(rss_contracts::Error::Network)?;
        let response=FetchResponse {final_url:request.url.clone(),status:reply.status,body:reply.body,cached_body:reply.cached,observed_at:at()};self.requests.lock().unwrap().push(request);Ok(response)})}}
#[derive(Default)]
struct Canonical {identities:Mutex<BTreeMap<String,ItemId>>,calls:AtomicU64}
impl CanonicalEntryPort for Canonical {fn prepare<'a>(&'a self,request:&'a SourceRequest,_:&'a HostRef,evidence:EntryEvidence)->Pending<'a,ObservedEntry> {
    Box::pin(async move {let n=self.calls.fetch_add(1,Ordering::SeqCst)+1;let mut identities=self.identities.lock().unwrap();let next=id(1000+identities.len() as u64);
        let item_id=*identities.entry(evidence.reliable_provider_identity()).or_insert(next);let url=evidence.url();let normalized=hsscrap::identity::normalize_url(&url,&request.bounds)?;
        let capture=URLCapture {capture_id:id(100+n),item_id,capture_kind:CaptureKind::SourceIngest,access_partition:request.subscription.partition.scope.clone(),captured_url:url.clone(),original_url:Some(url),
            identity_url:normalized.identity_url,normalization_version:normalized.normalization_version,captured_at:Some(at()),created_here_at:at(),actor_ref:Some(reference("actor")),operation_id:id(10000+n),common:Extensions::default()};
        let mut observation=MetadataObservation {observation_id:id(200+n),item_id,stream_id:id(300+n),sequence:Revision(n),baseline_observation_id:None,capture_id:Some(capture.capture_id),appearance_ref:Some(reference("appearance")),
            producer:"fixture".into(),access_partition:request.subscription.partition.scope.clone(),observed_at:at(),source_revision:None,canonical_url_assertion:None,provider:None,host_site:"www.tiktok.com".into(),
            title:None,description:None,author:None,published_at:None,provider_labels:vec![],gallery_snapshot:None,source_payload_digest:None,status:Availability {state:AvailabilityState::Available,checked_at:Some(at()),reason_code:None,retry_after:None},common:Extensions::default()};
        evidence.populate(&mut observation);Ok(ObservedEntry {provider_entry_key:evidence.id,capture,observation})})}}
fn provider(replies:Vec<Reply>)->TikTokProvider<Fetch,Canonical> {TikTokProvider {source:Source {source_ref:reference("source"),owner_scope:ScopeRef("scope".into()),provider:Provider::Tiktok,
    original_url:"https://www.tiktok.com/@creator".into(),display_name:"Creator".into(),revision:Revision(1),paused:false,common:Extensions::default()},execution_ref:reference("attempt"),initial_cursor_millis:START,
    limits:ProviderLimits {max_body_bytes:100000,max_document_entries:100,max_documents:20,max_fetches_per_operation:4,timeout_millis:10000},supported_extensions:BTreeSet::new(),fetch:Fetch {replies:Mutex::new(replies.into()),requests:Mutex::new(vec![])},canonical:Canonical::default()}}

#[test]
fn creator_windows_paginate_deduplicate_and_refresh_global_identity() {
    let adapter=provider(vec![raw(profile()),ok(window(vec![item(1),item(2)],true)),raw(profile()),ok(window(vec![item(2),item(3)],false)),raw(profile()),ok(window(vec![item(1)],false))]);
    let first=run(adapter.acquire(request(1))).unwrap();let mut next=request(2);next.continuation=first.continuation;let last=run(adapter.acquire(next)).unwrap();
    assert_eq!(last.entries.len(),1);assert_eq!(last.entries[0].provider_entry_key,"3");assert!(last.continuation.is_none());assert_eq!(last.coverage,Coverage::Partial);
    let refresh=run(adapter.acquire(request(3))).unwrap();assert_eq!(refresh.entries[0].capture.item_id,first.entries[0].capture.item_id);assert_eq!(adapter.canonical.identities.lock().unwrap().len(),3);
    let calls=adapter.fetch.requests.lock().unwrap();assert!(calls[1].url.contains("/api/creator/item_list/"));assert!(calls[3].url.contains("cursor=1759999998000"));
}
#[test]
fn same_document_resume_uses_digest_and_changed_or_missing_snapshot_fails() {
    let data=window(vec![item(1),item(2),item(3)],false);let adapter=provider(vec![raw(profile()),ok(data.clone()),raw(profile()),Reply {body:vec![],status:304,cached:Some(serde_json::to_vec(&data).unwrap())}]);
    let first=run(adapter.acquire(request(1))).unwrap();let mut next=request(2);next.continuation=first.continuation;assert_eq!(run(adapter.acquire(next)).unwrap().entries[0].provider_entry_key,"3");
    assert!(adapter.fetch.requests.lock().unwrap()[3].expected_digest.is_some());
    let adapter=provider(vec![raw(profile()),ok(data),raw(profile()),ok(window(vec![item(1),item(2),item(4)],false))]);
    let first=run(adapter.acquire(request(1))).unwrap();let mut next=request(2);next.continuation=first.continuation;assert!(matches!(run(adapter.acquire(next)),Err(rss_contracts::Error::StaleResult)));assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),2);
}
#[test]
fn empty_continuing_windows_consume_bounded_requests_without_fake_checkpoint() {
    let adapter=provider(vec![raw(profile()),ok(window(vec![],true)),ok(window(vec![item(1)],false))]);let result=run(adapter.acquire(request(1))).unwrap();assert_eq!(result.entries.len(),1);
    let calls=adapter.fetch.requests.lock().unwrap();assert_eq!(calls.len(),3);assert!(calls[2].url.contains(&format!("cursor={}",START-7*86_400_000)));
    let mut adapter=provider(vec![raw(profile()),ok(window(vec![],true))]);adapter.limits.max_fetches_per_operation=2;
    assert!(matches!(run(adapter.acquire(request(1))),Err(rss_contracts::Error::LimitExceeded {ref field}) if field=="tiktok.fetches"));assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),0);
}
#[test]
fn photo_order_has_no_invented_member_ids_and_explicit_dates_are_preserved() {
    let account=normalize::account(&profile(),"creator",100000,&bounds()).unwrap();let mut photo=item(10);photo["imagePost"]=json!({"images":[
        {"imageURL":{"urlList":["https://media.tiktokcdn.com/first.jpg"]},"imageWidth":1080,"imageHeight":1350},
        {"imageURL":{"urlList":["https://media.tiktokcdn.com/second.jpg"]},"imageWidth":1080,"imageHeight":1350}]});
    let parsed=normalize::window(&serde_json::to_vec(&window(vec![photo.clone()],false)).unwrap(),&account,START,100000,100,&bounds()).unwrap();
    assert!(parsed.entries[0].photo);assert!(parsed.entries[0].media[0].url.ends_with("first.jpg"));assert!(parsed.entries[0].media[1].url.ends_with("second.jpg"));
    assert!(parsed.entries[0].media.iter().all(|m|m.provider_member_id.is_none()));assert_eq!(parsed.entries[0].published_at.as_ref().unwrap().raw_value,"1759999990");
    photo["imagePost"]["images"][0]["id"]=json!("duplicate");photo["imagePost"]["images"][1]["id"]=json!("duplicate");
    assert!(normalize::window(&serde_json::to_vec(&window(vec![photo],false)).unwrap(),&account,START,100000,100,&bounds()).is_err());
    let mut unknown=item(11);unknown.as_object_mut().unwrap().remove("createTime");let parsed=normalize::window(&serde_json::to_vec(&window(vec![unknown],false)).unwrap(),&account,START,100000,100,&bounds()).unwrap();assert!(parsed.entries[0].published_at.is_none());
    let mut large=item(1);large["id"]=json!(9_007_199_254_740_993u64);let parsed=normalize::window(&serde_json::to_vec(&window(vec![large],false)).unwrap(),&account,START,100000,100,&bounds()).unwrap();
    assert_eq!(parsed.entries[0].reliable_provider_identity(),"tiktok/item/9007199254740993");
}
#[test]
fn individual_item_uses_same_provider_identity_and_rejects_retarget() {
    let item_html=html(json!({"webapp.video-detail":{"statusCode":0,"itemInfo":{"itemStruct":item(1)}}}));
    let mut adapter=provider(vec![raw(item_html.clone())]);adapter.source.original_url="https://www.tiktok.com/@creator/video/1".into();let result=run(adapter.acquire(request(1))).unwrap();
    assert_eq!(result.entries[0].provider_entry_key,"1");assert_eq!(adapter.canonical.identities.lock().unwrap().keys().next().unwrap(),"tiktok/item/1");
    let mut adapter=provider(vec![raw(item_html)]);adapter.source.original_url="https://www.tiktok.com/@creator/video/2".into();assert!(matches!(run(adapter.acquire(request(1))),Err(rss_contracts::Error::StaleResult)));
}
#[test]
fn failures_expiry_private_and_malformed_later_item_have_no_new_effects() {
    let mut expired=item(1);expired["video"]["playAddr"]=json!("https://media.tiktokcdn.com/video.mp4?x-expires=1");
    let mut malformed=item(3);malformed["video"]["playAddr"]=json!("javascript:invalid");
    for reply in [Reply {body:vec![],status:429,cached:None},Reply {body:vec![],status:410,cached:None},Reply {body:vec![],status:304,cached:None},
        ok(json!({"statusCode":10222})),ok(json!({"statusCode":0})),ok(window(vec![expired],false)),ok(window(vec![item(1),item(2),malformed],false))] {
        let adapter=provider(vec![raw(profile()),reply]);assert!(run(adapter.acquire(request(1))).is_err());assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),0);
    }
    let adapter=provider(vec![raw(b"<html>Verify human</html>".to_vec())]);assert!(matches!(run(adapter.acquire(request(1))),Err(rss_contracts::Error::Challenged)));
}
#[test]
fn scope_epoch_source_identity_and_cursor_direction_are_fenced() {
    let adapter=provider(vec![]);let mut foreign=request(1);foreign.subscription.partition.scope=ScopeRef("foreign".into());assert!(matches!(run(adapter.acquire(foreign)),Err(rss_contracts::Error::Denied)));assert!(adapter.fetch.requests.lock().unwrap().is_empty());
    let adapter=provider(vec![raw(profile()),ok(window(vec![item(1)],true)),raw(profile()),ok(window(vec![item(1)],true))]);
    let first=run(adapter.acquire(request(1))).unwrap();let mut stale=request(2);stale.continuation=first.continuation.clone();stale.subscription.generation=Revision(2);
    assert!(matches!(run(adapter.acquire(stale)),Err(rss_contracts::Error::StaleResult)));
    let mut next=request(3);next.continuation=first.continuation;assert!(run(adapter.acquire(next)).is_err());assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),1);
    let account=normalize::account(&profile(),"creator",100000,&bounds()).unwrap();let mut future=item(1);future["createTime"]=json!(START/1000+1);
    assert!(matches!(normalize::window(&serde_json::to_vec(&window(vec![future],true)).unwrap(),&account,START,100000,100,&bounds()),Err(rss_contracts::Error::Invalid {ref field}) if field=="tiktok.cursor_direction"));
}
#[test]
fn explicit_zero_profile_and_terminal_empty_window_are_separate() {
    let empty_profile=html(json!({"webapp.user-detail":{"statusCode":0,"userInfo":{"user":author(),"stats":{"videoCount":0}}}}));
    let adapter=provider(vec![raw(empty_profile),ok(window(vec![],false))]);assert!(matches!(run(adapter.acquire(request(1))).unwrap().state,rss_contracts::AcquisitionState::VerifiedEmpty {..}));
    let adapter=provider(vec![raw(profile()),ok(window(vec![item(1)],true)),raw(profile()),ok(window(vec![],false))]);let first=run(adapter.acquire(request(1))).unwrap();let mut next=request(2);next.continuation=first.continuation;assert!(run(adapter.acquire(next)).unwrap().is_terminal_partial_document());
    let adapter=provider(vec![raw(profile()),ok(window(vec![],false))]);assert!(run(adapter.acquire(request(1))).is_err());
}
