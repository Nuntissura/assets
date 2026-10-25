use super::*;
use rss_provider_pinterest::connected::*;
use rss_contracts::{CaptureIntent,Error};
mod home;
struct AuthorizedFetch {replies:Mutex<VecDeque<Reply>>,requests:Mutex<Vec<FetchRequest>>,checks:AtomicU64,reject:Mutex<Option<(u64,Error)>>}
impl AuthorizedFetchPort for AuthorizedFetch {
    fn current<'a>(&'a self,_:&'a SourceRequest,_:&'a HostRef,_:&'a AuthorizedContext)->Pending<'a,()> {Box::pin(async move {let check=self.checks.fetch_add(1,Ordering::SeqCst)+1;if let Some((at,error))=&*self.reject.lock().unwrap(){if *at==check{return Err(error.clone());}}Ok(())})}
    fn fetch<'a>(&'a self,_:&'a SourceRequest,_:&'a HostRef,_:&'a AuthorizedContext,r:FetchRequest)->Pending<'a,FetchResponse> {Box::pin(async move {let reply=self.replies.lock().unwrap().pop_front().ok_or(Error::Network)?;let url=r.url.clone();self.requests.lock().unwrap().push(r);Ok(FetchResponse {final_url:url,status:reply.status,body:reply.body,cached_body:reply.cached,observed_at:at()})})}
}
fn selected(mode:AccessMode,n:u64)->SourceRequest{let mut r=request(n);r.subscription.partition.mode=mode;r.subscription.partition.connection_ref=Some(reference("connection"));r}
fn authorized(mode:AccessMode,replies:Vec<Reply>)->AuthorizedPinterestProvider<AuthorizedFetch,Canonical>{
    let p=provider(vec![]);let r=selected(mode,1);
    AuthorizedPinterestProvider {authorization:AuthorizedContext {connection_ref:reference("connection"),owner_scope:p.source.owner_scope.clone(),external_identity:"99".into(),lease_generation:Revision(1),
        selected_board:BoardBinding {board_id:"700".into(),source_url:p.source.original_url.clone()},granted_scopes:["user_accounts:read","boards:read","pins:read","boards:read_secret","pins:read_secret"].into_iter().map(str::to_owned).collect(),
        capture_intent:CaptureIntent::FollowSource {original_url:p.source.original_url.clone(),provider:Provider::Pinterest,partition:r.subscription.partition}},
        source:p.source,execution_ref:p.execution_ref,limits:p.limits,supported_extensions:p.supported_extensions,canonical:p.canonical,
        fetch:AuthorizedFetch {replies:Mutex::new(replies.into()),requests:Mutex::new(vec![]),checks:AtomicU64::new(0),reject:Mutex::new(None)}}
}
fn account()->Reply{ok(json!({"id":"99","username":"owner"}))}
fn grant(privacy:&str,count:u64)->Reply{ok(json!({"id":"700","owner":{"username":"owner"},"privacy":privacy,"pin_count":count,"name":"Board"}))}
fn pin(n:u64)->Value{json!({"id":n.to_string(),"board_id":"700","title":"Pin","created_at":"2026-10-22T12:13:14.123+02:00","media":{"media_type":"image","images":{"1200x":{"url":format!("https://i.pinimg.com/originals/{n}.jpg"),"width":1200,"height":1800}}}})}
fn pins(items:Vec<Value>,next:Option<&str>)->Reply{ok(json!({"items":items,"bookmark":next}))}

#[test]
fn oauth_selected_owned_collaborative_secret_and_protected_boards_keep_modes_and_scope(){
    for (mode,privacy,collaborative) in [(AccessMode::PublicBoard,"PUBLIC",false),(AccessMode::PublicBoard,"PUBLIC",true),(AccessMode::PrivateSource,"SECRET",false),(AccessMode::PrivateSource,"PROTECTED",true)]{
        let account=if collaborative{ok(json!({"id":"99","username":"collaborator"}))}else{account()};
        let p=authorized(mode,vec![account,grant(privacy,1),pins(vec![pin(1)],None)]);
        assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);let page=run(p.acquire(selected(mode,1))).unwrap();assert_eq!(page.partition.mode,mode);assert_eq!(page.entries[0].capture.access_partition,ScopeRef("scope".into()));assert_eq!(page.coverage,Coverage::Partial);
        let requests=p.fetch.requests.lock().unwrap();assert_eq!(requests[0].url,"https://api.pinterest.com/v5/user_account");assert_eq!(requests[1].url,"https://api.pinterest.com/v5/boards/700");assert!(requests[2].url.contains("/boards/700/pins?"));assert!(requests.iter().all(|r|!r.url.contains("ad_account_id")));
    }
}
#[test]
fn oauth_principal_board_url_and_pin_membership_must_all_match(){
    for replies in [vec![ok(json!({"id":"98","username":"owner"}))],vec![account(),ok(json!({"id":"701","owner":{"username":"owner"},"privacy":"PUBLIC"}))],vec![account(),ok(json!({"id":"700","owner":{"username":"other"},"privacy":"PUBLIC"}))],vec![account(),grant("PUBLIC",1),{let mut pin=pin(1);pin["board_id"]=json!("701");pins(vec![pin],None)}]]{
        let p=authorized(AccessMode::PublicBoard,replies);assert!(run(p.acquire(selected(AccessMode::PublicBoard,1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    }
    let mut p=authorized(AccessMode::PublicBoard,vec![]);p.authorization.selected_board.source_url="https://www.pinterest.com/owner/other/".into();assert_eq!(run(p.acquire(selected(AccessMode::PublicBoard,1))).unwrap_err(),Error::Denied);assert!(p.fetch.requests.lock().unwrap().is_empty());
}
#[test]
fn scopes_privacy_missing_permission_and_unsupported_feeds_fail_without_captures(){
    let mut p=authorized(AccessMode::PrivateSource,vec![account(),grant("SECRET",1)]);p.authorization.granted_scopes.remove("pins:read_secret");assert_eq!(run(p.acquire(selected(AccessMode::PrivateSource,1))).unwrap_err(),Error::Denied);
    let mut p=authorized(AccessMode::PublicBoard,vec![]);p.authorization.granted_scopes.remove("boards:read");assert_eq!(run(p.acquire(selected(AccessMode::PublicBoard,1))).unwrap_err(),Error::Denied);assert!(p.fetch.requests.lock().unwrap().is_empty());
    for (mode,privacy) in [(AccessMode::PublicBoard,"SECRET"),(AccessMode::PrivateSource,"PUBLIC")] {let p=authorized(mode,vec![account(),grant(privacy,1)]);assert_eq!(run(p.acquire(selected(mode,1))).unwrap_err(),Error::Denied);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);}
    let p=authorized(AccessMode::PublicBoard,vec![account(),Reply {status:403,body:vec![],cached:None}]);assert_eq!(run(p.acquire(selected(AccessMode::PublicBoard,1))).unwrap_err(),Error::Denied);
    for mode in [AccessMode::Following,AccessMode::HomeForYou,AccessMode::OwnUploads,AccessMode::SubscriptionImport]{let p=authorized(mode,vec![]);assert!(matches!(run(p.acquire(selected(mode,1))),Err(Error::Unsupported {..})));assert!(p.fetch.requests.lock().unwrap().is_empty());}
}
#[test]
fn verified_zero_is_distinct_from_denied_malformed_and_unverified_empty(){
    let p=authorized(AccessMode::PublicBoard,vec![account(),grant("PUBLIC",0),pins(vec![],None)]);let page=run(p.acquire(selected(AccessMode::PublicBoard,1))).unwrap();assert!(matches!(page.state,rss_contracts::AcquisitionState::VerifiedEmpty {..}));
    for reply in [pins(vec![],None),ok(json!({"bookmark":null})),ok(json!({"code":403,"message":"Denied"}))]{let p=authorized(AccessMode::PublicBoard,vec![account(),grant("PUBLIC",5),reply]);assert!(run(p.acquire(selected(AccessMode::PublicBoard,1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);}
}
#[test]
fn retained_board_pages_resume_across_lease_renewal_and_reject_changed_bytes(){
    let p=authorized(AccessMode::PublicBoard,vec![account(),grant("PUBLIC",3),pins(vec![pin(1),pin(2),pin(3)],None),account(),grant("PUBLIC",3),pins(vec![pin(1),pin(2),pin(3)],None)]);
    let first=run(p.acquire(selected(AccessMode::PublicBoard,1))).unwrap();let mut p=p;p.authorization.lease_generation=Revision(2);let mut r=selected(AccessMode::PublicBoard,2);r.continuation=first.continuation;
    let last=run(p.acquire(r)).unwrap();assert_eq!(last.entries[0].provider_entry_key,"3");assert!(p.fetch.requests.lock().unwrap()[5].expected_digest.is_some());
    let p=authorized(AccessMode::PublicBoard,vec![account(),grant("PUBLIC",3),pins(vec![pin(1),pin(2),pin(3)],None),account(),grant("PUBLIC",3),pins(vec![pin(1),pin(2),pin(4)],None)]);
    let first=run(p.acquire(selected(AccessMode::PublicBoard,1))).unwrap();let mut r=selected(AccessMode::PublicBoard,2);r.continuation=first.continuation;assert_eq!(run(p.acquire(r)).unwrap_err(),Error::StaleResult);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),2);
}
#[test]
fn bookmark_cycles_and_lease_fences_stop_before_further_preparation(){
    let p=authorized(AccessMode::PublicBoard,vec![account(),grant("PUBLIC",2),pins(vec![pin(1)],Some("loop")),account(),grant("PUBLIC",2),pins(vec![pin(2)],Some("loop"))]);
    let first=run(p.acquire(selected(AccessMode::PublicBoard,1))).unwrap();let mut r=selected(AccessMode::PublicBoard,2);r.continuation=first.continuation;assert!(run(p.acquire(r)).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),1);
    for (check,error,captures) in [(1,Error::ReconnectRequired,0),(3,Error::Expired,0),(7,Error::Denied,0),(9,Error::StaleResult,1)]{
        let p=authorized(AccessMode::PublicBoard,vec![account(),grant("PUBLIC",2),pins(vec![pin(1),pin(2)],None)]);*p.fetch.reject.lock().unwrap()=Some((check,error.clone()));assert_eq!(run(p.acquire(selected(AccessMode::PublicBoard,1))).unwrap_err(),error);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),captures);
    }
}
#[test]
fn unsupported_media_and_bounded_payloads_cannot_fabricate_galleries(){
    let mut unknown=pin(1);unknown["media"]=json!({"media_type":"unknown"});
    let p=authorized(AccessMode::PublicBoard,vec![account(),grant("PUBLIC",1),pins(vec![unknown],None)]);assert!(run(p.acquire(selected(AccessMode::PublicBoard,1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    let mut p=authorized(AccessMode::PublicBoard,vec![account()]);p.limits.max_body_bytes=5;assert!(matches!(run(p.acquire(selected(AccessMode::PublicBoard,1))),Err(Error::LimitExceeded {..})));
    let mut movie=pin(1);movie["media"]=json!({"media_type":"video","video_url":"https://v.pinimg.com/videos/movie.mp4","duration":1000,"width":1280,"height":720});
    let p=authorized(AccessMode::PublicBoard,vec![account(),grant("PUBLIC",1),pins(vec![movie],None)]);assert_eq!(run(p.acquire(selected(AccessMode::PublicBoard,1))).unwrap().entries.len(),1);
}

#[test]
fn oauth_video_duration_preserves_integral_milliseconds_and_omits_submillisecond_precision(){
    struct DurationCanonical {inner:Canonical,observed:Mutex<Vec<Option<u64>>>}
    impl CanonicalEntryPort for DurationCanonical {
        fn prepare<'a>(&'a self,r:&'a SourceRequest,e:&'a HostRef,evidence:EntryEvidence)->Pending<'a,ObservedEntry>{
            self.observed.lock().unwrap().push(evidence.media[0].duration_ms);self.inner.prepare(r,e,evidence)
        }
    }
    for (duration,expected) in [(json!(1000),Some(1000)),(json!(1000.0),Some(1000)),(json!(1000.5),None),(Value::Null,None),(json!(u64::MAX),Some(u64::MAX))]{
        let mut movie=pin(1);movie["media"]=json!({"media_type":"video","video_url":"https://v.pinimg.com/videos/movie.mp4","duration":duration});
        let p=authorized(AccessMode::PublicBoard,vec![account(),grant("PUBLIC",1),pins(vec![movie],None)]);
        let p=AuthorizedPinterestProvider {source:p.source,execution_ref:p.execution_ref,limits:p.limits,supported_extensions:p.supported_extensions,authorization:p.authorization,fetch:p.fetch,canonical:DurationCanonical {inner:p.canonical,observed:Mutex::new(vec![])}};
        run(p.acquire(selected(AccessMode::PublicBoard,1))).unwrap();assert_eq!(*p.canonical.observed.lock().unwrap(),vec![expected]);
    }
    for duration in [json!(-1),json!(-0.5),json!("1000"),json!(true),json!(18_446_744_073_709_551_616.0)]{
        let mut movie=pin(1);movie["media"]=json!({"media_type":"video","video_url":"https://v.pinimg.com/videos/movie.mp4","duration":duration});
        let p=authorized(AccessMode::PublicBoard,vec![account(),grant("PUBLIC",1),pins(vec![movie],None)]);assert!(run(p.acquire(selected(AccessMode::PublicBoard,1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    }
}
