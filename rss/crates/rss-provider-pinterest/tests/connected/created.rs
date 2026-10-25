use super::*;
use rss_provider_pinterest::connected::created::*;
struct CreatedFetch {replies:Mutex<VecDeque<Reply>>,requests:Mutex<Vec<FetchRequest>>,checks:AtomicU64,reject:Mutex<Option<(u64,Error)>>}
impl BrowserCreatedFetchPort for CreatedFetch {
    fn current<'a>(&'a self,_:&'a SourceRequest,_:&'a HostRef,_:&'a BrowserCreatedContext)->Pending<'a,()>{Box::pin(async move{let n=self.checks.fetch_add(1,Ordering::SeqCst)+1;if let Some((at,e))=&*self.reject.lock().unwrap(){if *at==n{return Err(e.clone());}}Ok(())})}
    fn fetch<'a>(&'a self,_:&'a SourceRequest,_:&'a HostRef,_:&'a BrowserCreatedContext,r:FetchRequest)->Pending<'a,FetchResponse>{Box::pin(async move{let reply=self.replies.lock().unwrap().pop_front().ok_or(Error::Network)?;let url=r.url.clone();self.requests.lock().unwrap().push(r);Ok(FetchResponse {final_url:url,status:reply.status,body:reply.body,cached_body:reply.cached,observed_at:at()})})}
}
fn created(replies:Vec<Reply>)->BrowserCreatedProvider<CreatedFetch,Canonical>{let p=provider(vec![]);let r=selected(AccessMode::OwnUploads,1);let mut source=p.source;source.original_url="https://www.pinterest.com/owner/_created/".into();
    BrowserCreatedProvider {authorization:BrowserCreatedContext {connection_ref:reference("connection"),owner_scope:source.owner_scope.clone(),external_identity:"99".into(),lease_generation:Revision(1),capture_intent:CaptureIntent::FollowSource {original_url:source.original_url.clone(),provider:Provider::Pinterest,partition:r.subscription.partition}},
        source,execution_ref:p.execution_ref,limits:p.limits,supported_extensions:p.supported_extensions,canonical:p.canonical,fetch:CreatedFetch {replies:Mutex::new(replies.into()),requests:Mutex::new(vec![]),checks:AtomicU64::new(0),reject:Mutex::new(None)}}
}
fn created_request(n:u64)->SourceRequest{selected(AccessMode::OwnUploads,n)}
fn profile(id:&str,username:&str)->Reply{ok(json!({"resource_response":{"status":"success","code":0,"data":{"id":id,"username":username}},"resource":{"name":"UserResource"}}))}
fn own(n:u64)->Value{let mut v=item(n);v["pinner"]=json!({"id":"99","username":"owner"});v}
fn body(items:Vec<Value>,next:&str,wrapped:bool)->Value{let data=if wrapped{json!({"results":items})}else{json!(items)};json!({"resource_response":{"status":"success","code":0,"data":data},"resource":{"name":"UserActivityPinsResource","options":{"bookmarks":[next]}}})}
fn request_options(r:&FetchRequest)->Value{let url=url::Url::parse(&r.url).unwrap();let data=url.query_pairs().find(|(k,_)|k=="data").unwrap().1.into_owned();serde_json::from_str::<Value>(&data).unwrap()["options"].clone()}
#[test]
fn own_created_routes_profile_then_created_resource_instead_of_saved_board_or_home(){
    for wrapped in [false,true]{let p=created(vec![profile("99","owner"),ok(body(vec![own(1)],"-end-",wrapped))]);let page=run(p.acquire(created_request(1))).unwrap();assert_eq!(page.entries[0].provider_entry_key,"1");assert_eq!(page.coverage,Coverage::Partial);assert_eq!(page.partition.mode,AccessMode::OwnUploads);
        let calls=p.fetch.requests.lock().unwrap();assert!(calls[0].url.contains("/UserResource/get/"));assert_eq!(request_options(&calls[0]),json!({"username":"owner","field_set_key":"profile"}));assert!(calls[1].url.contains("/UserActivityPinsResource/get/"));assert_eq!(request_options(&calls[1]),json!({"username":"owner","exclude_add_pin_rep":true,"field_set_key":"grid_item","is_own_profile_pins":false}));assert!(calls.iter().all(|r|!r.url.contains("/UserPinsResource/")&&!r.url.contains("/BoardFeedResource/")&&!r.url.contains("/UserHomefeedResource/")));
    }
}
#[test]
fn requested_profile_and_present_creator_evidence_must_match_numeric_session_principal(){
    for first in [profile("98","owner"),profile("99","other")]{let p=created(vec![first]);assert_eq!(run(p.acquire(created_request(1))).unwrap_err(),Error::Denied);assert_eq!(p.fetch.requests.lock().unwrap().len(),1);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);}
    for pinner in [json!({"id":"98","username":"owner"}),json!({"id":"99","username":"other"})]{let mut v=own(1);v["pinner"]=pinner;let p=created(vec![profile("99","owner"),ok(body(vec![v],"-end-",false))]);assert_eq!(run(p.acquire(created_request(1))).unwrap_err(),Error::Denied);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);}
    let mut no_attribution=own(1);no_attribution.as_object_mut().unwrap().remove("pinner");let p=created(vec![profile("99","owner"),ok(body(vec![no_attribution],"-end-",false))]);let page=run(p.acquire(created_request(1))).unwrap();assert!(page.entries[0].observation.author.is_none());assert_eq!(page.coverage,Coverage::Partial);
}
#[test]
fn created_empty_wrong_resource_and_failed_envelopes_never_become_verified_empty(){
    let mut wrong=body(vec![own(1)],"-end-",false);wrong["resource"]["name"]=json!("UserPinsResource");
    let mut failed=body(vec![own(1)],"-end-",false);failed["resource_response"]["status"]=json!("error");
    for value in [wrong,failed,body(vec![],"-end-",false),json!({"resource_response":{"status":"success","code":0,"data":{}}})]{let p=created(vec![profile("99","owner"),ok(value)]);assert!(run(p.acquire(created_request(1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);}
    let mut p=created(vec![]);p.source.original_url="https://www.pinterest.com/owner/pins/".into();assert!(run(p.acquire(created_request(1))).is_err());assert!(p.fetch.requests.lock().unwrap().is_empty());
}
#[test]
fn created_bookmarks_and_retained_offsets_bind_original_account_bytes_and_renewed_lease(){
    let original=body(vec![own(1),own(2),own(3)],"next",false);let mut p=created(vec![profile("99","owner"),ok(original.clone()),profile("99","owner"),ok(original),profile("99","owner"),ok(body(vec![own(4)],"-end-",true))]);
    let first=run(p.acquire(created_request(1))).unwrap();p.authorization.lease_generation=Revision(2);let mut r=created_request(2);r.continuation=first.continuation;let second=run(p.acquire(r)).unwrap();assert_eq!(second.entries[0].provider_entry_key,"3");let mut r=created_request(3);r.continuation=second.continuation;let last=run(p.acquire(r)).unwrap();assert_eq!(last.entries[0].provider_entry_key,"4");let calls=p.fetch.requests.lock().unwrap();assert!(calls[3].expected_digest.is_some());assert_eq!(request_options(&calls[5])["bookmarks"],json!(["next"]));
    let p=created(vec![profile("99","owner"),ok(body(vec![own(1),own(2),own(3)],"-end-",false)),profile("99","owner"),ok(body(vec![own(1),own(2),own(4)],"-end-",false))]);let first=run(p.acquire(created_request(1))).unwrap();let mut r=created_request(2);r.continuation=first.continuation;assert_eq!(run(p.acquire(r)).unwrap_err(),Error::StaleResult);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),2);
}
#[test]
fn created_lease_races_cycles_expiry_and_bounds_stop_further_owner_effects(){
    for (check,error,captures) in [(1,Error::ReconnectRequired,0),(3,Error::Expired,0),(5,Error::Denied,0),(7,Error::StaleResult,1)]{let p=created(vec![profile("99","owner"),ok(body(vec![own(1),own(2)],"-end-",false))]);*p.fetch.reject.lock().unwrap()=Some((check,error.clone()));assert_eq!(run(p.acquire(created_request(1))).unwrap_err(),error);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),captures);}
    let p=created(vec![profile("99","owner"),ok(body(vec![own(1)],"loop",false)),profile("99","owner"),ok(body(vec![own(2)],"loop",false))]);let first=run(p.acquire(created_request(1))).unwrap();let mut r=created_request(2);r.continuation=first.continuation;assert!(run(p.acquire(r)).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),1);
    let mut p=created(vec![profile("99","owner")]);p.limits.max_body_bytes=5;assert!(matches!(run(p.acquire(created_request(1))),Err(Error::LimitExceeded {..})));
    let p=created(vec![Reply {status:440,body:vec![],cached:None}]);assert_eq!(run(p.acquire(created_request(1))).unwrap_err(),Error::Expired);
}
