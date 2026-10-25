use super::*;
use rss_contracts::{CaptureIntent,Error};
use rss_provider_tiktok::connected::*;
const VIEWER_SEC:&str="MS4wLjABAAAAfixturevieweridentity";
struct AuthorizedFetch {
    replies:Mutex<VecDeque<Reply>>,requests:Mutex<Vec<ConnectedFetchRequest>>,checks:AtomicU64,
    generation:AtomicU64,login:Mutex<(String,String)>,reject:Mutex<Option<(u64,Error)>>,extra_query:Mutex<Option<(String,String)>>,common_query:Mutex<Vec<(String,String)>>,
}
impl AuthorizedFetchPort for AuthorizedFetch {
    fn current<'a>(&'a self,r:&'a SourceRequest,_:&'a HostRef,a:&'a AuthorizedContext)->Pending<'a,()>{
        Box::pin(async move {
            let check=self.checks.fetch_add(1,Ordering::SeqCst)+1;
            if r.subscription.partition.scope.0!="scope"{return Err(Error::Denied);}
            if let Some((at,error))=&*self.reject.lock().unwrap(){if *at==check{return Err(error.clone());}}
            if a.lease_generation.0!=self.generation.load(Ordering::SeqCst)||*self.login.lock().unwrap()!=(a.viewer_id.clone(),a.viewer_sec_uid.clone()){return Err(Error::StaleResult);}Ok(())
        })
    }
    fn fetch<'a>(&'a self,_:&'a SourceRequest,_:&'a HostRef,_:&'a AuthorizedContext,r:ConnectedFetchRequest)->Pending<'a,FetchResponse>{
        Box::pin(async move {
            let reply=self.replies.lock().unwrap().pop_front().ok_or(Error::Network)?;
            let mut url=url::Url::parse(&r.endpoint.url()?).unwrap();
            let mut common=self.common_query.lock().unwrap().iter().cloned().collect::<BTreeMap<_,_>>();
            common.extend(url.query_pairs().map(|(k,v)|(k.into_owned(),v.into_owned())));
            if !common.is_empty(){url.query_pairs_mut().clear().extend_pairs(common);}
            if let Some((key,value))=&*self.extra_query.lock().unwrap(){url.query_pairs_mut().append_pair(key,value);}
            self.requests.lock().unwrap().push(r);
            Ok(FetchResponse {final_url:url.into(),status:reply.status,body:reply.body,cached_body:reply.cached,observed_at:at()})
        })
    }
}
fn selected(mode:AccessMode,n:u64)->SourceRequest{let mut r=request(n);r.subscription.partition.mode=mode;r.subscription.partition.connection_ref=Some(reference("connection"));r}
fn connected(mode:AccessMode,replies:Vec<Reply>)->ConnectedTikTokProvider<AuthorizedFetch,Canonical>{
    let p=provider(vec![]);let r=selected(mode,1);
    let source_url=match mode{AccessMode::Following=>"https://www.tiktok.com/following",AccessMode::HomeForYou=>"https://www.tiktok.com/foryou",_=>"https://www.tiktok.com/@creator"};
    let mut source=p.source;source.original_url=source_url.into();
    let (viewer_id,viewer_sec)=if mode==AccessMode::OwnUploads{("123456",SEC)}else{("999999",VIEWER_SEC)};
    ConnectedTikTokProvider {authorization:AuthorizedContext {connection_ref:reference("connection"),owner_scope:source.owner_scope.clone(),viewer_id:viewer_id.into(),viewer_sec_uid:viewer_sec.into(),
        lease_generation:Revision(1),capture_intent:CaptureIntent::FollowSource {original_url:source.original_url.clone(),provider:Provider::Tiktok,partition:r.subscription.partition}},
        source,execution_ref:p.execution_ref,limits:p.limits,supported_extensions:p.supported_extensions,canonical:p.canonical,
        fetch:AuthorizedFetch {replies:Mutex::new(replies.into()),requests:Mutex::new(vec![]),checks:AtomicU64::new(0),generation:AtomicU64::new(1),
            login:Mutex::new((viewer_id.into(),viewer_sec.into())),reject:Mutex::new(None),extra_query:Mutex::new(None),common_query:Mutex::new(vec![])}}
}
fn profile_access(private:bool,relation:Option<u64>,count:u64)->Reply{
    let mut user=author();user["privateAccount"]=json!(private);if let Some(relation)=relation{user["relation"]=json!(relation);}
    raw(html(json!({"webapp.user-detail":{"statusCode":0,"userInfo":{"user":user,"stats":{"videoCount":count}}}})))
}
fn feed(items:Vec<Value>,more:bool,cursor:Option<&str>)->Reply{
    let mut value=json!({"statusCode":0,"itemList":items,"hasMore":more});if let Some(cursor)=cursor{value["cursor"]=json!(cursor);}ok(value)
}

#[test]
fn connected_feeds_route_distinct_endpoints_and_preserve_mixed_author_content(){
    for mode in [AccessMode::Following,AccessMode::HomeForYou]{
        let mut other=item(2);other["author"]["id"]=json!("888888");other["author"]["uniqueId"]=json!("another");other["author"]["secUid"]=json!("MS4wLjABAAAAfixtureanotheridentity");
        let p=connected(mode,vec![feed(vec![item(1),other],true,Some("123"))]);let result=run(p.acquire(selected(mode,1))).unwrap();
        assert_eq!(result.entries.len(),2);assert_eq!(result.partition.mode,mode);assert_eq!(result.coverage,Coverage::Partial);
        assert!(result.entries[0].capture.captured_url.contains("@creator"));assert!(result.entries[1].capture.captured_url.contains("@another"));
        let requests=p.fetch.requests.lock().unwrap();let url=requests[0].endpoint.url().unwrap();
        if mode==AccessMode::Following{
            assert!(matches!(&requests[0].endpoint,Endpoint::Following {cursor} if cursor=="0"));
            for pair in ["pullType=1","level=1","isResetCounterUsed=true","from_page=following","cursor=0"]{assert!(url.contains(pair));}
            assert!(result.continuation.is_some());assert!(!url.contains("maxCursor"));
        }else{assert!(matches!(requests[0].endpoint,Endpoint::ForYou));assert!(!url.contains("cursor"));assert!(url.contains("from_page=fyp"));assert!(result.continuation.is_none());}
    }
}
#[test]
fn foryou_splits_only_one_retained_batch_and_renews_without_a_provider_cursor(){
    let body=feed(vec![item(1),item(2),item(3)],true,Some("provider_value_is_not_a_cursor"));
    let original=body.body.clone();
    let mut p=connected(AccessMode::HomeForYou,vec![body,Reply {body:vec![],status:304,cached:Some(original)}]);
    let first=run(p.acquire(selected(AccessMode::HomeForYou,1))).unwrap();let saved=first.continuation.clone().unwrap();
    assert!(!saved.token.contains("lease_generation"));assert!(!saved.token.contains("provider_value_is_not_a_cursor"));
    p.authorization.lease_generation=Revision(2);p.fetch.generation.store(2,Ordering::SeqCst);
    let mut next=selected(AccessMode::HomeForYou,2);next.continuation=first.continuation;
    let last=run(p.acquire(next)).unwrap();assert_eq!(last.entries[0].provider_entry_key,"3");assert!(last.continuation.is_none());assert_eq!(last.coverage,Coverage::Partial);
    let calls=p.fetch.requests.lock().unwrap();assert_eq!(calls.len(),2);assert!(calls[1].expected_digest.is_some());assert!(calls.iter().all(|r|matches!(r.endpoint,Endpoint::ForYou)));
}
#[test]
fn following_uses_exact_cursor_and_deduplicates_without_filtering_to_the_viewer(){
    let p=connected(AccessMode::Following,vec![feed(vec![item(1),item(2)],true,Some("123")),feed(vec![item(2),item(3)],false,None)]);
    let first=run(p.acquire(selected(AccessMode::Following,1))).unwrap();let mut r=selected(AccessMode::Following,2);r.continuation=first.continuation;
    let last=run(p.acquire(r)).unwrap();assert_eq!(last.entries.len(),1);assert_eq!(last.entries[0].provider_entry_key,"3");assert_eq!(p.canonical.calls.load(Ordering::SeqCst),3);
    assert!(matches!(&p.fetch.requests.lock().unwrap()[1].endpoint,Endpoint::Following {cursor} if cursor=="123"));
}
#[test]
fn own_and_private_profile_content_require_exact_identity_and_actual_content(){
    for mode in [AccessMode::OwnUploads,AccessMode::PrivateSource]{
        let p=connected(mode,vec![profile_access(true,Some(1),1),feed(vec![item(1)],false,None)]);
        let page=run(p.acquire(selected(mode,1))).unwrap();assert_eq!(page.entries.len(),1);assert_eq!(page.partition.mode,mode);
        let calls=p.fetch.requests.lock().unwrap();assert!(matches!(&calls[0].endpoint,Endpoint::Profile {username} if username=="creator"));
        assert!(matches!(&calls[1].endpoint,Endpoint::Posts {sec_uid,cursor} if sec_uid==SEC&&cursor=="0"));assert!(calls[1].endpoint.url().unwrap().contains("from_page=user"));
    }
    for relation in [1,3]{let p=connected(AccessMode::PrivateSource,vec![profile_access(true,Some(relation),1),feed(vec![item(1)],false,None)]);assert!(run(p.acquire(selected(AccessMode::PrivateSource,1))).is_ok());}
    for reply in [profile_access(true,None,1),profile_access(true,Some(0),1),profile_access(false,Some(1),1)]{
        let p=connected(AccessMode::PrivateSource,vec![reply]);assert_eq!(run(p.acquire(selected(AccessMode::PrivateSource,1))).unwrap_err(),Error::Denied);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    }
    let p=connected(AccessMode::OwnUploads,vec![{let mut value=author();value["id"]=json!("111111");raw(html(json!({"webapp.user-detail":{"statusCode":0,"userInfo":{"user":value}}})))}]);
    assert_eq!(run(p.acquire(selected(AccessMode::OwnUploads,1))).unwrap_err(),Error::Denied);assert_eq!(p.fetch.requests.lock().unwrap().len(),1);
    let p=connected(AccessMode::PrivateSource,vec![profile_access(true,Some(1),1),feed(vec![{let mut value=item(1);value["author"]["id"]=json!("111111");value}],false,None)]);
    assert!(run(p.acquire(selected(AccessMode::PrivateSource,1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
}
#[test]
fn selected_profile_empty_requires_own_zero_and_cannot_verify_private_or_feed_emptiness(){
    let p=connected(AccessMode::OwnUploads,vec![profile_access(false,None,0),feed(vec![],false,None)]);
    assert!(matches!(run(p.acquire(selected(AccessMode::OwnUploads,1))).unwrap().state,rss_contracts::AcquisitionState::VerifiedEmpty {..}));
    for mode in [AccessMode::Following,AccessMode::HomeForYou,AccessMode::PrivateSource]{
        let replies=if mode==AccessMode::PrivateSource{vec![profile_access(true,Some(1),0),feed(vec![],false,None)]}else{vec![feed(vec![],false,None)]};
        let p=connected(mode,replies);assert!(run(p.acquire(selected(mode,1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    }
    let p=connected(AccessMode::Following,vec![feed(vec![item(1)],true,Some("9")),feed(vec![],false,None)]);
    let first=run(p.acquire(selected(AccessMode::Following,1))).unwrap();let mut r=selected(AccessMode::Following,2);r.continuation=first.continuation;
    assert!(run(p.acquire(r)).unwrap().is_terminal_partial_document());
}
#[test]
fn followed_private_metadata_never_grants_access_without_current_successful_content(){
    let p=connected(AccessMode::PrivateSource,vec![profile_access(true,Some(1),1),Reply {body:vec![],status:403,cached:None}]);
    assert_eq!(run(p.acquire(selected(AccessMode::PrivateSource,1))).unwrap_err(),Error::Denied);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    let p=connected(AccessMode::PrivateSource,vec![profile_access(true,Some(1),1)]);
    assert_eq!(run(p.acquire(selected(AccessMode::PrivateSource,1))).unwrap_err(),Error::Network);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    for (at,captures) in [(4,0),(5,0),(7,1)]{
        let p=connected(AccessMode::PrivateSource,vec![profile_access(true,Some(1),1),feed(vec![item(1),item(2)],false,None)]);
        *p.fetch.reject.lock().unwrap()=Some((at,Error::Denied));
        assert_eq!(run(p.acquire(selected(AccessMode::PrivateSource,1))).unwrap_err(),Error::Denied);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),captures);
    }
}
#[test]
fn malformed_denied_missing_paging_and_challenge_documents_never_prepare(){
    for reply in [ok(json!({})),ok(json!({"statusCode":0,"hasMore":false})),ok(json!({"statusCode":0,"itemList":[item(1)]})),
        ok(json!({"statusCode":0,"itemList":[item(1)],"hasMore":true})),ok(json!({"statusCode":10222,"itemList":[],"hasMore":false})),
        raw(b"<html>Verify human</html>".to_vec()),Reply {body:vec![],status:401,cached:None},Reply {body:vec![],status:403,cached:None}]{
        let p=connected(AccessMode::Following,vec![reply]);assert!(run(p.acquire(selected(AccessMode::Following,1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    }
    let mut malformed=item(2);malformed["video"]["playAddr"]=json!("javascript:invalid");
    let p=connected(AccessMode::Following,vec![feed(vec![item(1),malformed],false,None)]);assert!(run(p.acquire(selected(AccessMode::Following,1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
}
#[test]
fn connection_scope_capture_intent_and_single_item_targets_deny_before_fetch(){
    for changed in 0..7{
        let mut p=connected(AccessMode::PrivateSource,vec![]);let mut r=selected(AccessMode::PrivateSource,1);
        match changed{
            0=>p.authorization.connection_ref=reference("foreign"),
            1=>p.authorization.owner_scope=ScopeRef("foreign".into()),
            2=>r.subscription.partition.scope=ScopeRef("foreign".into()),
            3=>p.authorization.viewer_id="not-numeric".into(),
            4=>p.authorization.viewer_sec_uid="not-secuid".into(),
            5=>p.authorization.capture_intent=CaptureIntent::SaveUrl {original_url:p.source.original_url.clone()},
            _=>{p.source.original_url="https://www.tiktok.com/@creator/video/1".into();if let CaptureIntent::FollowSource {original_url,..}=&mut p.authorization.capture_intent{*original_url=p.source.original_url.clone();}},
        }
        assert!(run(p.acquire(r)).is_err());assert!(p.fetch.requests.lock().unwrap().is_empty());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    }
}
#[test]
fn retained_changes_missing_cache_login_change_and_cursor_cycle_stop_further_effects(){
    for reply in [feed(vec![item(1),item(2),item(4)],false,None),Reply {body:vec![],status:304,cached:None}]{
        let p=connected(AccessMode::HomeForYou,vec![feed(vec![item(1),item(2),item(3)],false,None),reply]);
        let first=run(p.acquire(selected(AccessMode::HomeForYou,1))).unwrap();let mut r=selected(AccessMode::HomeForYou,2);r.continuation=first.continuation;
        assert!(run(p.acquire(r)).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),2);
    }
    let mut p=connected(AccessMode::HomeForYou,vec![feed(vec![item(1),item(2),item(3)],false,None)]);
    let first=run(p.acquire(selected(AccessMode::HomeForYou,1))).unwrap();p.authorization.viewer_id="888888".into();p.fetch.login.lock().unwrap().0="888888".into();
    let mut r=selected(AccessMode::HomeForYou,2);r.continuation=first.continuation;assert_eq!(run(p.acquire(r)).unwrap_err(),Error::StaleResult);assert_eq!(p.fetch.requests.lock().unwrap().len(),1);
    let p=connected(AccessMode::Following,vec![feed(vec![item(1)],true,Some("9")),feed(vec![item(2)],true,Some("9"))]);
    let first=run(p.acquire(selected(AccessMode::Following,1))).unwrap();let mut r=selected(AccessMode::Following,2);r.continuation=first.continuation;
    assert!(run(p.acquire(r)).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),1);
}
#[test]
fn current_lease_fences_expiry_revocation_and_supersession_around_preparation(){
    for (at,error,preparations) in [(1,Error::ReconnectRequired,0),(3,Error::Expired,0),(4,Error::Denied,0),(5,Error::StaleResult,1)]{
        let p=connected(AccessMode::Following,vec![feed(vec![item(1),item(2)],false,None)]);*p.fetch.reject.lock().unwrap()=Some((at,error.clone()));
        assert_eq!(run(p.acquire(selected(AccessMode::Following,1))).unwrap_err(),error);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),preparations);
    }
}
#[test]
fn mandatory_parameters_cannot_be_retargeted_by_browser_context(){
    for key in ["cursor","secUid","maxCursor","from_page"]{
        let p=connected(AccessMode::Following,vec![feed(vec![item(1)],false,None)]);*p.fetch.extra_query.lock().unwrap()=Some((key.into(),"changed".into()));
        assert_eq!(run(p.acquire(selected(AccessMode::Following,1))).unwrap_err(),Error::StaleResult);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    }
    let p=connected(AccessMode::Following,vec![feed(vec![item(1)],false,None)]);*p.fetch.extra_query.lock().unwrap()=Some(("X-Gnarly".into(),"runtime-value".into()));
    assert_eq!(run(p.acquire(selected(AccessMode::Following,1))).unwrap().entries.len(),1);
}
#[test]
fn actual_opentok_common_query_fields_preserve_the_selected_feed_and_no_cursor_foryou(){
    let common=[
        ("device_id","123456"),("odinId","123456"),("WebIdLastTime","1790000000"),("user_is_login","true"),("data_collection_enabled","true"),
        ("aid","1988"),("app_name","tiktok_web"),("app_language","en"),("browser_language","en-US"),("browser_name","Mozilla"),
        ("browser_online","true"),("browser_platform","Win32"),("browser_version","fixture"),("channel","tiktok_web"),("cookie_enabled","true"),
        ("device_platform","web_pc"),("focus_state","true"),("from_page","fyp"),("history_len","3"),("is_fullscreen","false"),("is_page_visible","true"),
        ("language","en"),("os","windows"),("priority_region",""),("referer",""),("region","US"),("screen_height","1080"),("screen_width","1920"),
        ("tz_name","UTC"),("webcast_language","en"),("verifyFp","fixture-fingerprint"),("fp","fixture-fingerprint"),
    ];
    for mode in [AccessMode::Following,AccessMode::HomeForYou]{
        let p=connected(mode,vec![feed(vec![item(1)],false,None)]);
        *p.fetch.common_query.lock().unwrap()=common.iter().map(|(k,v)|(k.to_string(),v.to_string())).collect();
        let page=run(p.acquire(selected(mode,1))).unwrap();assert_eq!(page.entries.len(),1);
        let calls=p.fetch.requests.lock().unwrap();let endpoint=calls[0].endpoint.url().unwrap();
        if mode==AccessMode::Following{assert!(endpoint.contains("from_page=following"));}else{assert!(endpoint.contains("from_page=fyp"));assert!(!endpoint.contains("cursor"));}
    }
    let p=connected(AccessMode::HomeForYou,vec![feed(vec![item(1)],false,None)]);*p.fetch.extra_query.lock().unwrap()=Some(("from_page".into(),"following".into()));
    assert_eq!(run(p.acquire(selected(AccessMode::HomeForYou,1))).unwrap_err(),Error::StaleResult);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
}
#[test]
fn body_media_expiry_and_bounded_empty_windows_fail_without_new_effects(){
    let mut expired=item(1);expired["video"]["playAddr"]=json!("https://media.tiktokcdn.com/video.mp4?x-expires=1");
    let p=connected(AccessMode::Following,vec![feed(vec![expired],false,None)]);assert_eq!(run(p.acquire(selected(AccessMode::Following,1))).unwrap_err(),Error::Expired);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    let mut p=connected(AccessMode::Following,vec![feed(vec![item(1)],false,None)]);p.limits.max_body_bytes=10;assert!(run(p.acquire(selected(AccessMode::Following,1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    let mut p=connected(AccessMode::Following,vec![feed(vec![],true,Some("9")),feed(vec![],true,Some("10"))]);p.limits.max_fetches_per_operation=2;
    assert!(matches!(run(p.acquire(selected(AccessMode::Following,1))),Err(Error::LimitExceeded {..})));assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
}
