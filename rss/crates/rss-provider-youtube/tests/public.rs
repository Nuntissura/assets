use std::{collections::{BTreeMap,BTreeSet,VecDeque},future::Future,pin::pin,sync::{Arc,Mutex,atomic::{AtomicU64,Ordering}},task::{Context,Poll,Wake,Waker}};
use hsscrap::*;
use rss_contracts::{AccessMode,AccessPartition,ObservedEntry,Pending,Provider,ProviderPort,Source,SourceRequest,Subscription};
use rss_provider_youtube::*;
use serde_json::{Value,json};
const CHANNEL:&str="UC1234567890123456789012";
const UPLOADS:&str="UU1234567890123456789012";
fn run<T>(future:impl Future<Output=T>)->T {
    struct Noop;impl Wake for Noop {fn wake(self:Arc<Self>) {}}
    let waker=Waker::from(Arc::new(Noop));let mut context=Context::from_waker(&waker);let mut future=pin!(future);
    match future.as_mut().poll(&mut context) {Poll::Ready(value)=>value,Poll::Pending=>panic!("unexpected asynchronous fixture")}
}
fn id<T:std::str::FromStr>(n:u64)->T where T::Err:std::fmt::Debug {format!("00000000-0000-0000-0000-{n:012}").parse().unwrap()}
fn reference(value:&str)->HostRef {HostRef {host_instance_id:"fixture".into(),owner_kind:"fixture".into(),native_id:value.into()}}
fn at()->Instant {Instant::parse("2026-10-23T00:00:00Z").unwrap()}
fn bounds()->Bounds {Bounds {max_url_bytes:4096,max_text_bytes:10000,max_collection_items:100,max_page_items:2,max_operation_bytes:100000}}
fn request(n:u64)->SourceRequest {SourceRequest {subscription:Subscription {subscription_ref:reference("subscription"),source_ref:reference("source"),partition:AccessPartition {
    scope:ScopeRef("scope".into()),mode:AccessMode::PublicChannel,connection_ref:None},executor_ref:reference("executor"),generation:Revision(1),paused:false,common:Extensions::default()},
    operation_id:id(n),continuation:None,bounds:bounds()}}
fn channel()->Value {json!({"kind":"youtube#channelListResponse","items":[{"id":CHANNEL,"contentDetails":{"relatedPlaylists":{"uploads":UPLOADS}}}]})}
fn video_id(n:u64)->String {format!("Video{n:06}")}
fn item(n:u64)->Value {let video=video_id(n);json!({"kind":"youtube#playlistItem","id":format!("playlist-item-{n}"),"snippet":{"playlistId":UPLOADS,"channelId":CHANNEL,
    "title":format!("Video {n}"),"description":"Description","publishedAt":"2026-10-22T00:00:00Z","videoOwnerChannelId":CHANNEL,"videoOwnerChannelTitle":"Creator",
    "resourceId":{"kind":"youtube#video","videoId":video},"thumbnails":{"high":{"url":format!("https://i.ytimg.com/vi/{video}/hqdefault.jpg"),"width":480,"height":360}}},
    "contentDetails":{"videoId":video,"videoPublishedAt":"2026-01-01T12:30:00+05:30"},"status":{"privacyStatus":"public"}})}
fn page(items:Vec<Value>,next:Option<&str>)->Value {
    let count=items.len();let mut value=json!({"kind":"youtube#playlistItemListResponse","items":items,"pageInfo":{"totalResults":count,"resultsPerPage":count}});
    if let Some(next)=next {value["nextPageToken"]=json!(next);}value
}
fn atom(count:usize)->Vec<u8> {format!("<feed xmlns=\"http://www.w3.org/2005/Atom\" xmlns:yt=\"http://www.youtube.com/xml/schemas/2015\"><id>yt:channel:{CHANNEL}</id><title>Creator</title>{}</feed>",
    (1..=count).map(|n|format!("<entry><id>yt:video:{}</id><title>Video {n}</title><published>2026-01-01T12:30:00+05:30</published><link rel=\"alternate\" href=\"https://www.youtube.com/watch?v={}\"/><author><name>Creator</name></author></entry>",video_id(n as u64),video_id(n as u64))).collect::<String>()).into_bytes()}
struct Reply {body:Vec<u8>,status:u16,cached:Option<Vec<u8>>}
fn ok(value:Value)->Reply {Reply {body:serde_json::to_vec(&value).unwrap(),status:200,cached:None}}
fn raw(body:Vec<u8>)->Reply {Reply {body,status:200,cached:None}}
#[derive(Default)]
struct Fetch {replies:Mutex<VecDeque<Reply>>,requests:Mutex<Vec<FetchRequest>>}
impl FetchPort for Fetch {
    fn fetch<'a>(&'a self,source:&'a SourceRequest,_:&'a HostRef,request:FetchRequest)->Pending<'a,FetchResponse> {
        Box::pin(async move {if source.subscription.partition.scope.0 != "scope" {return Err(rss_contracts::Error::Denied);} let reply=self.replies.lock().unwrap().pop_front().ok_or(rss_contracts::Error::Network)?;
            let result=FetchResponse {final_url:request.url.clone(),status:reply.status,body:reply.body,cached_body:reply.cached,observed_at:at()};self.requests.lock().unwrap().push(request);Ok(result)})
    }
}
#[derive(Default)]
struct Canonical {identities:Mutex<BTreeMap<String,ItemId>>,calls:AtomicU64}
impl CanonicalEntryPort for Canonical {
    fn prepare<'a>(&'a self,request:&'a SourceRequest,_:&'a HostRef,evidence:EntryEvidence)->Pending<'a,ObservedEntry> {
        Box::pin(async move {
            let n=self.calls.fetch_add(1,Ordering::SeqCst)+1;let mut identities=self.identities.lock().unwrap();let next=id(1000+identities.len() as u64);
            let item_id=*identities.entry(evidence.reliable_provider_identity()).or_insert(next);let url=evidence.url();let normalized=hsscrap::identity::normalize_url(&url,&request.bounds)?;
            let capture=URLCapture {capture_id:id(100+n),item_id,capture_kind:CaptureKind::SourceIngest,access_partition:request.subscription.partition.scope.clone(),captured_url:url.clone(),
                original_url:Some(url),identity_url:normalized.identity_url,normalization_version:normalized.normalization_version,captured_at:Some(at()),created_here_at:at(),
                actor_ref:Some(reference("actor")),operation_id:id(10000+n),common:Extensions::default()};
            let mut observation=MetadataObservation {observation_id:id(200+n),item_id,stream_id:id(300+n),sequence:Revision(n),baseline_observation_id:None,capture_id:Some(capture.capture_id),
                appearance_ref:Some(reference("appearance")),producer:"fixture".into(),access_partition:request.subscription.partition.scope.clone(),observed_at:at(),source_revision:None,
                canonical_url_assertion:None,provider:None,host_site:"www.youtube.com".into(),title:None,description:None,author:None,published_at:None,provider_labels:vec![],gallery_snapshot:None,
                source_payload_digest:None,status:Availability {state:AvailabilityState::Available,checked_at:Some(at()),reason_code:None,retry_after:None},common:Extensions::default()};
            evidence.populate(&mut observation);Ok(ObservedEntry {provider_entry_key:evidence.video_id,capture,observation})
        })
    }
}
fn provider(strategy:Strategy,replies:Vec<Reply>)->YouTubeProvider<Fetch,Canonical> {YouTubeProvider {source:Source {source_ref:reference("source"),owner_scope:ScopeRef("account".into()),
    provider:Provider::Youtube,original_url:format!("https://www.youtube.com/channel/{CHANNEL}"),display_name:"Creator".into(),revision:Revision(1),paused:false,common:Extensions::default()},
    execution_ref:reference("attempt"),strategy,limits:ProviderLimits {max_body_bytes:100000,max_document_entries:100,max_documents:20,max_fetches_per_operation:4,timeout_millis:10000},
    supported_extensions:BTreeSet::new(),fetch:Fetch {replies:Mutex::new(replies.into()),requests:Mutex::new(vec![])},canonical:Canonical::default()}}

#[test]
fn uploads_paginate_deduplicate_and_retain_global_video_identity() {
    let adapter=provider(Strategy::PublicApi,vec![ok(channel()),ok(page(vec![item(1),item(2)],Some("next"))),ok(channel()),ok(page(vec![item(2),item(3)],None)),ok(channel()),ok(page(vec![item(1)],None))]);
    let first=run(adapter.acquire(request(1))).unwrap();let mut next=request(2);next.continuation=first.continuation;let last=run(adapter.acquire(next)).unwrap();
    assert_eq!(last.entries.len(),1);assert_eq!(last.entries[0].provider_entry_key,video_id(3));assert!(last.continuation.is_none());assert_eq!(last.coverage,Coverage::Partial);
    let refresh=run(adapter.acquire(request(3))).unwrap();assert_eq!(refresh.entries[0].capture.item_id,first.entries[0].capture.item_id);assert_eq!(adapter.canonical.identities.lock().unwrap().len(),3);
    assert!(adapter.fetch.requests.lock().unwrap()[3].url.contains("pageToken=next"));
    let published=refresh.entries[0].observation.published_at.as_ref().unwrap();assert_eq!(published.raw_value,"2026-01-01T12:30:00+05:30");assert_eq!(published.offset_minutes,Some(330));
}
#[test]
fn digest_resume_requires_exact_retained_page_and_cache() {
    let data=page(vec![item(1),item(2),item(3)],None);let cached=Reply {body:vec![],status:304,cached:Some(serde_json::to_vec(&data).unwrap())};
    let adapter=provider(Strategy::PublicApi,vec![ok(channel()),ok(data),ok(channel()),cached]);
    let first=run(adapter.acquire(request(1))).unwrap();let mut next=request(2);next.continuation=first.continuation;let last=run(adapter.acquire(next)).unwrap();assert_eq!(last.entries[0].provider_entry_key,video_id(3));
    assert!(adapter.fetch.requests.lock().unwrap()[3].expected_digest.is_some());
    let adapter=provider(Strategy::PublicApi,vec![ok(channel()),ok(page(vec![item(1),item(2),item(3)],None)),ok(channel()),ok(page(vec![item(1),item(2),item(4)],None))]);
    let first=run(adapter.acquire(request(1))).unwrap();let mut next=request(2);next.continuation=first.continuation;
    assert!(matches!(run(adapter.acquire(next)),Err(rss_contracts::Error::StaleResult)));assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),2);
}
#[test]
fn no_login_atom_is_recent_partial_and_never_silently_uses_api() {
    let adapter=provider(Strategy::NoLoginAtom,vec![raw(atom(3)),raw(atom(3))]);let first=run(adapter.acquire(request(1))).unwrap();let mut next=request(2);next.continuation=first.continuation;
    let last=run(adapter.acquire(next)).unwrap();assert_eq!(last.entries[0].provider_entry_key,video_id(3));assert!(last.continuation.is_none());
    assert!(matches!(last.state,rss_contracts::AcquisitionState::Partial {ref reason_code,..} if reason_code=="youtube_recent_atom_only"));
    assert!(adapter.fetch.requests.lock().unwrap().iter().all(|r|r.strategy==Strategy::NoLoginAtom&&r.url.contains("/feeds/videos.xml?channel_id=")));
    let empty=provider(Strategy::NoLoginAtom,vec![raw(atom(0))]);assert!(run(empty.acquire(request(1))).is_err());
}
#[test]
fn handles_and_video_urls_resolve_without_confusing_playlist_item_ids() {
    for url in ["https://youtu.be/Video000001","https://www.youtube.com/watch?v=Video000001","https://www.youtube.com/shorts/Video000001","https://www.youtube.com/embed/Video000001"] {
        assert_eq!(public::resolve_url(url,&bounds()).unwrap(),public::SourceTarget::Video(video_id(1)));
    }
    let mut adapter=provider(Strategy::NoLoginAtom,vec![raw(format!("<html><head><link rel='canonical' href='https://www.youtube.com/channel/{CHANNEL}'></head></html>").into_bytes()),raw(atom(1))]);
    adapter.source.original_url="https://www.youtube.com/@Creator".into();assert_eq!(run(adapter.acquire(request(1))).unwrap().entries.len(),1);
    let response=json!({"kind":"youtube#videoListResponse","items":[{"kind":"youtube#video","id":video_id(1),"snippet":{"channelId":CHANNEL,"channelTitle":"Creator","title":"Video","description":"Description","publishedAt":"2026-01-01T00:00:00Z","thumbnails":{}},"status":{"privacyStatus":"public"}}]});
    let mut adapter=provider(Strategy::PublicApi,vec![ok(response)]);adapter.source.original_url="https://youtu.be/Video000001".into();let page=run(adapter.acquire(request(1))).unwrap();
    assert_eq!(page.entries[0].provider_entry_key,"Video000001");assert_eq!(adapter.canonical.identities.lock().unwrap().keys().next().unwrap(),"youtube/video/Video000001");
}

#[test]
fn retained_handle_html_resolves_one_bounded_canonical_channel() {
    let html=format!("<link href='https://www.youtube.com/channel/{CHANNEL}' rel='canonical'>");
    assert_eq!(public::resolve_channel_html(html.as_bytes(),&bounds(),html.len()).unwrap(),CHANNEL);
    let duplicated=format!("{html}{html}");
    assert_eq!(public::resolve_channel_html(duplicated.as_bytes(),&bounds(),duplicated.len()).unwrap(),CHANNEL);
    let other="UCzyxwvutsrqponmlkjihgfe";
    let ambiguous=format!("{html}<link rel='canonical' href='https://www.youtube.com/channel/{other}'>");
    for bytes in [
        ambiguous.as_bytes(),
        b"<html><title>Channel</title></html>".as_slice(),
        b"<link rel='canonical' href='https://example.org/channel/UC1234567890123456789012'>".as_slice(),
        b"<link rel='canonical' href='https://www.youtube.com/@Creator'>".as_slice(),
        b"<script>var channel='https://www.youtube.com/channel/UC1234567890123456789012'</script>".as_slice(),
        &[0xff],
    ] {
        assert!(public::resolve_channel_html(bytes,&bounds(),100_000).is_err());
    }
    assert!(matches!(public::resolve_channel_html(html.as_bytes(),&bounds(),html.len()-1),Err(rss_contracts::Error::LimitExceeded {field}) if field=="youtube.body"));
    for limit in [0,16_000_001] {
        assert!(matches!(public::resolve_channel_html(html.as_bytes(),&bounds(),limit),Err(rss_contracts::Error::Invalid {field}) if field=="youtube.limits"));
    }
}
#[test]
fn malformed_later_entries_and_foreign_uploads_reject_before_effects() {
    let mut malformed=item(3);malformed["snippet"]["resourceId"]["videoId"]=json!(video_id(4));
    let adapter=provider(Strategy::PublicApi,vec![ok(channel()),ok(page(vec![item(1),item(2),malformed],None))]);
    assert!(run(adapter.acquire(request(1))).is_err());assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),0);
    let mut foreign=item(1);foreign["snippet"]["playlistId"]=json!("DifferentPlaylist");
    let adapter=provider(Strategy::PublicApi,vec![ok(channel()),ok(page(vec![foreign],None))]);assert!(run(adapter.acquire(request(1))).is_err());
    assert!(normalize::json(br#"{"kind":"a","kind":"b"}"#,10000).is_err());
}
#[test]
fn rate_quota_expiry_missing_cache_and_private_content_never_report_empty() {
    let mut private=item(1);private["status"]["privacyStatus"]=json!("private");
    for reply in [Reply {body:vec![],status:429,cached:None},Reply {body:vec![],status:410,cached:None},Reply {body:vec![],status:304,cached:None},
        Reply {body:serde_json::to_vec(&json!({"error":{"errors":[{"reason":"quotaExceeded"}]}})).unwrap(),status:403,cached:None},ok(page(vec![private],None))] {
        let adapter=provider(Strategy::PublicApi,vec![ok(channel()),reply]);assert!(run(adapter.acquire(request(1))).is_err());assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),0);
    }
}
#[test]
fn scope_epoch_identity_and_pagination_cycles_are_fenced() {
    let adapter=provider(Strategy::PublicApi,vec![]);let mut foreign=request(1);foreign.subscription.partition.scope=ScopeRef("foreign".into());
    assert!(matches!(run(adapter.acquire(foreign)),Err(rss_contracts::Error::Denied)));assert!(adapter.fetch.requests.lock().unwrap().is_empty());
    let adapter=provider(Strategy::PublicApi,vec![ok(channel()),ok(page(vec![item(1)],Some("next"))),ok(channel()),ok(page(vec![item(2)],Some("next")))]);
    let first=run(adapter.acquire(request(1))).unwrap();let mut next=request(2);next.continuation=first.continuation.clone();assert!(run(adapter.acquire(next)).is_err());
    let mut stale=request(3);stale.continuation=first.continuation;stale.subscription.generation=Revision(2);assert!(matches!(run(adapter.acquire(stale)),Err(rss_contracts::Error::StaleResult)));
    assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst),1);assert_eq!(adapter.fetch.requests.lock().unwrap().len(),4);
}
#[test]
fn verified_empty_public_uploads_and_terminal_empty_page_are_distinct() {
    let empty=provider(Strategy::PublicApi,vec![ok(channel()),ok(page(vec![],None))]);assert!(matches!(run(empty.acquire(request(1))).unwrap().state,rss_contracts::AcquisitionState::VerifiedEmpty {..}));
    let adapter=provider(Strategy::PublicApi,vec![ok(channel()),ok(page(vec![item(1)],Some("next"))),ok(channel()),ok(page(vec![],None))]);
    let first=run(adapter.acquire(request(1))).unwrap();let mut next=request(2);next.continuation=first.continuation;assert!(run(adapter.acquire(next)).unwrap().is_terminal_partial_document());
}

#[test]
fn account_owned_source_uses_authorized_profile_partition() {
    let adapter = provider(Strategy::PublicApi, vec![ok(channel()), ok(page(vec![item(1)], None))]);
    let authorized = request(90);
    assert_ne!(adapter.source.owner_scope, authorized.subscription.partition.scope);
    let page = run(adapter.acquire(authorized.clone())).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(adapter.source.owner_scope, ScopeRef("account".into()));
    assert_eq!(page.partition, authorized.subscription.partition);
    assert_eq!(page.entries[0].capture.access_partition, authorized.subscription.partition.scope);
    assert_eq!(page.entries[0].observation.access_partition, authorized.subscription.partition.scope);
    for invalid in 0..4 {
        let mut adapter = provider(Strategy::PublicApi, vec![]);
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


#[test]
fn atom_accepts_bound_channel_tail_and_same_video_shorts_without_changing_identity() {
    let video=video_id(1);
    for channel_id in [CHANNEL, &CHANNEL[2..]] {
        let body=format!(r#"<feed xmlns="http://www.w3.org/2005/Atom" xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns:media="http://search.yahoo.com/mrss/"><id>yt:channel:{channel_id}</id><yt:channelId>{channel_id}</yt:channelId><title>Creator</title><link rel="alternate" href="https://www.youtube.com/channel/{CHANNEL}"/><entry><id>yt:video:{video}</id><yt:channelId>{CHANNEL}</yt:channelId><title>Short video</title><published>2026-10-21T23:00:08+00:00</published><link rel="alternate" href="https://www.youtube.com/shorts/{video}"/><media:group><media:content url="https://www.youtube.com/v/{video}?version=3" type="application/x-shockwave-flash" width="640" height="390"/><media:description>Actual feed shape</media:description><media:thumbnail url="https://i4.ytimg.com/vi/{video}/hqdefault.jpg" width="480" height="360"/></media:group></entry></feed>"#).into_bytes();
        let adapter=provider(Strategy::NoLoginAtom,vec![raw(body.clone())]);
        let page=run(adapter.acquire(request(95))).unwrap();
        assert_eq!(page.entries.len(),1);
        assert!(page.continuation.is_none());
        assert!(matches!(page.state,rss_contracts::AcquisitionState::Partial {ref reason_code,..} if reason_code=="youtube_recent_atom_only"));
        let entry=&page.entries[0];
        assert_eq!(entry.provider_entry_key,video);
        assert_eq!(entry.capture.captured_url,format!("https://www.youtube.com/watch?v={video}"));
        assert_eq!(entry.observation.provider.as_deref(),Some("youtube"));
        assert_eq!(entry.observation.description.as_deref(),Some("Actual feed shape"));
        assert_eq!(entry.observation.published_at.as_ref().unwrap().raw_value,"2026-10-21T23:00:08+00:00");
        let document=normalize::atom(&body,CHANNEL,100000,100,&bounds()).unwrap();
        assert_eq!(document.entries[0].reliable_provider_identity(),format!("youtube/video/{video}"));
        assert_eq!(document.entries[0].previews[0].url,format!("https://i4.ytimg.com/vi/{video}/hqdefault.jpg"));
        assert_eq!(adapter.fetch.requests.lock().unwrap()[0].url,format!("https://www.youtube.com/feeds/videos.xml?channel_id={CHANNEL}"));
    }
}

#[test]
fn atom_channel_tail_requires_exact_canonical_binding_and_permalink_video() {
    let video=video_id(1);
    let body=|channel_id:&str,alternate:&str,permalink:&str| format!(r#"<feed xmlns="http://www.w3.org/2005/Atom"><id>yt:channel:{channel_id}</id><title>Creator</title>{alternate}<entry><id>yt:video:{video}</id><title>Video</title><link rel="alternate" href="{permalink}"/></entry></feed>"#).into_bytes();
    let channel_link=format!(r#"<link rel="alternate" href="https://www.youtube.com/channel/{CHANNEL}"/>"#);
    let shorts=format!("https://www.youtube.com/shorts/{video}");
    for bytes in [
        body("2234567890123456789012",&channel_link,&shorts),
        body(&CHANNEL[2..],"",&shorts),
        body(&CHANNEL[2..],r#"<link rel="alternate" href="https://www.youtube.com/channel/UC2234567890123456789012"/>"#,&shorts),
    ] {
        assert!(matches!(normalize::atom(&bytes,CHANNEL,100000,100,&bounds()),Err(rss_contracts::Error::Invalid {field}) if field=="youtube.atom_channel"));
    }
    for permalink in [
        "https://www.youtube.com/shorts/Video000002",
        "https://www.youtube.com.evil.example/shorts/Video000001",
        "https://www.youtube.com/shorts/Video000001?other=1",
    ] {
        assert!(matches!(normalize::atom(&body(&CHANNEL[2..],&channel_link,permalink),CHANNEL,100000,100,&bounds()),Err(rss_contracts::Error::Invalid {field}) if field=="youtube.atom_permalink"));
    }
}
#[path="connected/mod.rs"] mod connected;

#[test]
fn provider_capabilities_preserve_public_api_and_atom_strategy() {
    for (strategy, paginated) in [(Strategy::PublicApi, true), (Strategy::NoLoginAtom, false)] {
        let expected = vec![rss_contracts::Capability {
            provider: Provider::Youtube, mode: AccessMode::PublicChannel,
            status: rss_contracts::CapabilityStatus::Available, paginated,
            media_metadata: true, adapter_version: "youtube.public.v1".into(),
        }];
        assert_eq!(rss_provider_youtube::capabilities(strategy), expected);
        assert_eq!(provider(strategy, vec![]).capabilities(), expected);
    }
}
