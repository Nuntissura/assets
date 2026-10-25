use super::*;
use rss_provider_youtube::connected::*;
use rss_contracts::CaptureIntent;
struct ConnectedFetch {
    replies:Mutex<VecDeque<Reply>>,requests:Mutex<Vec<ConnectedFetchRequest>>,checks:AtomicU64,reject_at:Mutex<Option<(u64,rss_contracts::Error)>>,
}
impl ConnectedFetchPort for ConnectedFetch {
    fn current<'a>(&'a self,_:&'a SourceRequest,_:&'a HostRef,_:&'a AuthorizedContext)->Pending<'a,()> {
        Box::pin(async move {let n=self.checks.fetch_add(1,Ordering::SeqCst)+1;if let Some((at,error))=&*self.reject_at.lock().unwrap(){if *at==n{return Err(error.clone());}}Ok(())})
    }
    fn fetch<'a>(&'a self,_:&'a SourceRequest,_:&'a HostRef,_:&'a AuthorizedContext,request:ConnectedFetchRequest)->Pending<'a,FetchResponse> {
        Box::pin(async move {let reply=self.replies.lock().unwrap().pop_front().ok_or(rss_contracts::Error::Network)?;
            let url=match &request.endpoint {Endpoint::DataApi {url}=>url.clone(),Endpoint::Browse {..}=>"https://www.youtube.com/youtubei/v1/browse".into()};
            self.requests.lock().unwrap().push(request);Ok(FetchResponse {final_url:url,status:reply.status,body:reply.body,cached_body:reply.cached,observed_at:at()})})
    }
}
fn selected(mode:AccessMode,n:u64)->SourceRequest {let mut r=request(n);r.subscription.partition.mode=mode;r.subscription.partition.connection_ref=Some(reference("connection"));r}
fn connected(mode:AccessMode,replies:Vec<Reply>)->ConnectedYouTubeProvider<ConnectedFetch,Canonical> {
    let p=provider(Strategy::PublicApi,vec![]);let r=selected(mode,1);
    ConnectedYouTubeProvider {authorization:AuthorizedContext {connection_ref:reference("connection"),owner_scope:p.source.owner_scope.clone(),external_identity:CHANNEL.into(),lease_generation:Revision(1),
        credential_kind:if matches!(mode,AccessMode::Following|AccessMode::HomeForYou){CredentialKind::BrowserSession}else{CredentialKind::DataApiOAuth},
        capture_intent:CaptureIntent::FollowSource {original_url:p.source.original_url.clone(),provider:Provider::Youtube,partition:r.subscription.partition}},
        source:p.source,execution_ref:p.execution_ref,limits:p.limits,supported_extensions:p.supported_extensions,canonical:p.canonical,
        fetch:ConnectedFetch {replies:Mutex::new(replies.into()),requests:Mutex::new(vec![]),checks:AtomicU64::new(0),reject_at:Mutex::new(None)}}
}
fn video(n:u64)->Value {json!({"videoRenderer":{"videoId":video_id(n),"title":{"simpleText":"Video"},"ownerText":{"runs":[{"text":"Creator","navigationEndpoint":{"browseEndpoint":{"browseId":CHANNEL}}}]},"thumbnail":{"thumbnails":[{"url":"https://i.ytimg.com/vi/Video000001/default.jpg"}]}}})}
fn browse(mode:AccessMode,items:Vec<Value>,next:Option<&str>,continued:bool)->Value {
    let mut items=items;if let Some(token)=next{items.push(json!({"continuationItemRenderer":{"continuationEndpoint":{"continuationCommand":{"token":token}}}}));}
    if continued {json!({"onResponseReceivedActions":[{"appendContinuationItemsAction":{"continuationItems":items}}]})}
    else {json!({"contents":{"twoColumnBrowseResultsRenderer":{"tabs":[{"tabRenderer":{"selected":true,"tabIdentifier":if mode==AccessMode::Following{"FEsubscriptions"}else{"FEwhat_to_watch"},"content":{"richGridRenderer":{"contents":items}}}}]}}})}
}
fn import(items:Vec<Value>,next:Option<&str>)->Value {let mut v=json!({"kind":"youtube#subscriptionListResponse","pageInfo":{"totalResults":items.len()},"items":items});if let Some(t)=next{v["nextPageToken"]=json!(t);}v}
fn subscription(channel:&str)->Value {json!({"kind":"youtube#subscription","snippet":{"channelId":CHANNEL,"title":"Creator","resourceId":{"kind":"youtube#channel","channelId":channel}}})}
fn private_adapter(url:&str,replies:Vec<Reply>)->ConnectedYouTubeProvider<ConnectedFetch,Canonical> {let mut p=connected(AccessMode::PrivateSource,replies);p.source.original_url=url.into();p.authorization.capture_intent=CaptureIntent::SaveUrl {original_url:url.into()};p}

#[test]
fn connected_modes_route_actual_feeds_and_never_import_as_content() {
    for mode in [AccessMode::Following,AccessMode::HomeForYou] {
        let p=connected(mode,vec![ok(browse(mode,vec![video(1)],None,false))]);let page=run(p.acquire(selected(mode,1))).unwrap();
        assert_eq!(page.entries.len(),1);assert_eq!(page.partition.mode,mode);assert_eq!(page.coverage,Coverage::Partial);
        assert!(matches!(&p.fetch.requests.lock().unwrap()[0].endpoint,Endpoint::Browse {browse_id:Some(id),continuation:None} if id==if mode==AccessMode::Following{"FEsubscriptions"}else{"FEwhat_to_watch"}));
    }
    let p=connected(AccessMode::SubscriptionImport,vec![ok(import(vec![subscription(CHANNEL)],None))]);
    assert!(matches!(run(p.acquire(selected(AccessMode::SubscriptionImport,1))),Err(rss_contracts::Error::Unsupported {..})));
    let page=run(p.import_subscriptions(selected(AccessMode::SubscriptionImport,2))).unwrap();assert_eq!(page.channels[0].channel_id,CHANNEL);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    assert!(matches!(&p.fetch.requests.lock().unwrap()[0].endpoint,Endpoint::DataApi {url} if url.contains("subscriptions")&&url.contains("mine=true")));
}
#[test]
fn connected_browse_replays_digest_and_binds_selection_and_body_continuation() {
    let mode=AccessMode::Following;let body=browse(mode,vec![video(1),video(2),video(3)],Some("next"),false);
    let p=connected(mode,vec![ok(body.clone()),ok(body),ok(browse(mode,vec![video(4)],None,true))]);
    let first=run(p.acquire(selected(mode,1))).unwrap();let mut next=selected(mode,2);next.continuation=first.continuation;
    let second=run(p.acquire(next)).unwrap();assert_eq!(second.entries[0].provider_entry_key,video_id(3));
    let mut next=selected(mode,3);next.continuation=second.continuation;let third=run(p.acquire(next)).unwrap();assert_eq!(third.entries[0].provider_entry_key,video_id(4));
    let requests=p.fetch.requests.lock().unwrap();assert!(requests[1].expected_digest.is_some());assert!(matches!(&requests[2].endpoint,Endpoint::Browse {browse_id:None,continuation:Some(token)} if token=="next"));
    let p=connected(mode,vec![ok(browse(mode,vec![video(1)],None,false))]);let mut r=selected(mode,1);r.subscription.partition.connection_ref=Some(reference("foreign"));assert!(matches!(run(p.acquire(r)),Err(rss_contracts::Error::Denied)));assert!(p.fetch.requests.lock().unwrap().is_empty());
}
#[test]
fn oauth_own_uploads_and_private_video_keep_visibility_and_scope() {
    let mut value=item(1);value["snippet"]["videoOwnerChannelId"]=json!(CHANNEL);value["status"]["privacyStatus"]=json!("private");
    let mut values=page(vec![value],None);values["pageInfo"]=json!({"totalResults":1});
    let p=connected(AccessMode::OwnUploads,vec![ok(channel()),ok(values)]);let page=run(p.acquire(selected(AccessMode::OwnUploads,1))).unwrap();assert_eq!(page.entries[0].capture.access_partition,ScopeRef("scope".into()));
    assert!(matches!(&p.fetch.requests.lock().unwrap()[0].endpoint,Endpoint::DataApi {url} if url.contains("mine=true")));
    let value=json!({"kind":"youtube#videoListResponse","pageInfo":{"totalResults":1},"items":[{"kind":"youtube#video","id":video_id(1),"snippet":{"channelId":CHANNEL,"title":"Private","publishedAt":"2026-01-01T00:00:00Z","thumbnails":{}},"status":{"privacyStatus":"private"}}]});
    let p=private_adapter("https://www.youtube.com/watch?v=Video000001",vec![ok(value)]);let page=run(p.acquire(selected(AccessMode::PrivateSource,1))).unwrap();assert_eq!(page.entries.len(),1);assert_eq!(page.partition.connection_ref,Some(reference("connection")));
}
#[test]
fn connected_empty_requires_selected_explicit_provider_zero_and_errors_preserve_no_effects() {
    let p=connected(AccessMode::SubscriptionImport,vec![ok(import(vec![],None))]);let page=run(p.import_subscriptions(selected(AccessMode::SubscriptionImport,1))).unwrap();assert!(matches!(page.state,rss_contracts::AcquisitionState::VerifiedEmpty {..}));
    for body in [json!({}),json!({"kind":"youtube#subscriptionListResponse","items":[]}),json!({"error":{"code":403}})] {
        let p=connected(AccessMode::SubscriptionImport,vec![ok(body)]);assert!(run(p.import_subscriptions(selected(AccessMode::SubscriptionImport,1))).is_err());
    }
    let p=connected(AccessMode::HomeForYou,vec![ok(browse(AccessMode::HomeForYou,vec![],None,false))]);assert!(run(p.acquire(selected(AccessMode::HomeForYou,1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    for (status,error) in [(401,rss_contracts::Error::Expired),(403,rss_contracts::Error::Denied)] {let p=connected(AccessMode::Following,vec![Reply {status,body:vec![],cached:None}]);assert_eq!(run(p.acquire(selected(AccessMode::Following,1))).unwrap_err(),error);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);}
}
#[test]
fn connected_lease_expiry_revocation_and_post_prepare_races_stop_further_effects() {
    for (at,error,expected) in [(1,rss_contracts::Error::ReconnectRequired,0),(3,rss_contracts::Error::Expired,0),(4,rss_contracts::Error::Denied,0),(5,rss_contracts::Error::StaleResult,1)] {
        let p=connected(AccessMode::Following,vec![ok(browse(AccessMode::Following,vec![video(1),video(2)],None,false))]);*p.fetch.reject_at.lock().unwrap()=Some((at,error.clone()));
        assert_eq!(run(p.acquire(selected(AccessMode::Following,1))).unwrap_err(),error);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),expected);
    }
}
#[test]
fn connected_rejects_wrong_page_credential_mode_and_changed_retained_bytes() {
    let mut p=connected(AccessMode::HomeForYou,vec![]);p.authorization.credential_kind=CredentialKind::DataApiOAuth;
    assert!(matches!(run(p.acquire(selected(AccessMode::HomeForYou,1))),Err(rss_contracts::Error::Unsupported {..})));
    let p=connected(AccessMode::Following,vec![ok(browse(AccessMode::HomeForYou,vec![video(1)],None,false))]);assert!(matches!(run(p.acquire(selected(AccessMode::Following,1))),Err(rss_contracts::Error::StaleResult)));
    let p=connected(AccessMode::Following,vec![ok(browse(AccessMode::Following,vec![video(1),video(2),video(3)],None,false)),ok(browse(AccessMode::Following,vec![video(1),video(2),video(4)],None,false))]);
    let first=run(p.acquire(selected(AccessMode::Following,1))).unwrap();let mut r=selected(AccessMode::Following,2);r.continuation=first.continuation;assert!(matches!(run(p.acquire(r)),Err(rss_contracts::Error::StaleResult)));assert_eq!(p.canonical.calls.load(Ordering::SeqCst),2);
}

#[test]
fn connected_partial_unknown_renderers_cycles_and_bounds_never_become_complete() {
    let mode=AccessMode::Following;
    let p=connected(mode,vec![ok(browse(mode,vec![video(1),json!({"unknownRenderer":{}})],None,false))]);
    let page=run(p.acquire(selected(mode,1))).unwrap();assert!(matches!(page.state,rss_contracts::AcquisitionState::Partial {ref reason_code,..} if reason_code=="youtube_unrecognized_content_omitted"));
    let p=connected(mode,vec![ok(browse(mode,vec![video(1)],Some("loop"),false)),ok(browse(mode,vec![video(2)],Some("loop"),true))]);
    let first=run(p.acquire(selected(mode,1))).unwrap();let mut r=selected(mode,2);r.continuation=first.continuation;assert!(run(p.acquire(r)).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),1);
    let mut p=connected(mode,vec![ok(browse(mode,vec![video(1),video(2)],None,false))]);p.limits.max_body_bytes=10;
    assert!(matches!(run(p.acquire(selected(mode,1))),Err(rss_contracts::Error::LimitExceeded {..})));assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    let p=connected(mode,vec![ok(browse(mode,vec![video(1),video(2)],None,false))]);let mut r=selected(mode,1);r.bounds.max_collection_items=1;
    assert!(run(p.acquire(r)).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
}

#[test]
fn private_playlist_and_import_pagination_do_not_create_substitute_captures() {
    let mut v=item(1);v["snippet"]["videoOwnerChannelId"]=json!(CHANNEL);v["status"]["privacyStatus"]=json!("unlisted");
    let mut body=page(vec![v],None);body["pageInfo"]=json!({"totalResults":1});
    let p=private_adapter(&format!("https://www.youtube.com/playlist?list={UPLOADS}"),vec![ok(body)]);
    assert_eq!(run(p.acquire(selected(AccessMode::PrivateSource,1))).unwrap().entries.len(),1);
    let other="UCabcdefghijklmnopqrstuv";
    let p=connected(AccessMode::SubscriptionImport,vec![ok(import(vec![subscription(CHANNEL)],Some("next"))),ok(import(vec![subscription(other)],None))]);
    let first=run(p.import_subscriptions(selected(AccessMode::SubscriptionImport,1))).unwrap();let mut r=selected(AccessMode::SubscriptionImport,2);r.continuation=first.continuation;
    let last=run(p.import_subscriptions(r)).unwrap();assert_eq!(last.channels[0].channel_id,other);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
}

#[test]
fn current_lockup_video_uses_bounded_attributed_channel_identity() {
    let lockup=json!({"lockupViewModel":{"contentId":video_id(1),"contentType":"LOCKUP_CONTENT_TYPE_VIDEO",
        "contentImage":{"thumbnailViewModel":{"image":{"sources":[{"url":"https://i.ytimg.com/vi/Video000001/default.jpg"}]}}},
        "metadata":{"lockupMetadataViewModel":{"title":{"content":"Current video"},"metadata":{"contentMetadataViewModel":{"metadataRows":[{"metadataParts":[{"text":{"content":"Creator","commandRuns":[{"onTap":{"innertubeCommand":{"browseEndpoint":{"browseId":CHANNEL}}}}]}}]}]}}}}}});
    let p=connected(AccessMode::HomeForYou,vec![ok(browse(AccessMode::HomeForYou,vec![lockup],None,false))]);
    assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);let page=run(p.acquire(selected(AccessMode::HomeForYou,1))).unwrap();assert_eq!(page.entries[0].provider_entry_key,video_id(1));assert_eq!(page.entries[0].observation.title.as_deref(),Some("Current video"));
}
