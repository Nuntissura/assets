use super::*;
use rss_provider_pinterest::connected::home::*;
struct HomeFetch {replies:Mutex<VecDeque<Reply>>,requests:Mutex<Vec<HomeFetchRequest>>,checks:AtomicU64,reject:Mutex<Option<(u64,Error)>>}
impl BrowserHomeFetchPort for HomeFetch {
    fn current<'a>(&'a self,_:&'a SourceRequest,_:&'a HostRef,_:&'a BrowserHomeContext)->Pending<'a,()>{Box::pin(async move{let check=self.checks.fetch_add(1,Ordering::SeqCst)+1;if let Some((at,error))=&*self.reject.lock().unwrap(){if *at==check{return Err(error.clone());}}Ok(())})}
    fn fetch<'a>(&'a self,_:&'a SourceRequest,_:&'a HostRef,_:&'a BrowserHomeContext,r:HomeFetchRequest)->Pending<'a,FetchResponse>{Box::pin(async move{let reply=self.replies.lock().unwrap().pop_front().ok_or(Error::Network)?;let url=r.url.clone();self.requests.lock().unwrap().push(r);Ok(FetchResponse {final_url:url,status:reply.status,body:reply.body,cached_body:reply.cached,observed_at:at()})})}
}
fn home(replies:Vec<Reply>)->BrowserHomeProvider<HomeFetch,Canonical>{let p=provider(vec![]);let r=selected(AccessMode::HomeForYou,1);let mut source=p.source;source.original_url="https://www.pinterest.com/".into();
    BrowserHomeProvider {authorization:BrowserHomeContext {connection_ref:reference("connection"),owner_scope:source.owner_scope.clone(),external_identity:"99".into(),lease_generation:Revision(1),capture_intent:CaptureIntent::FollowSource {original_url:source.original_url.clone(),provider:Provider::Pinterest,partition:r.subscription.partition}},
        source,execution_ref:p.execution_ref,limits:p.limits,supported_extensions:p.supported_extensions,canonical:p.canonical,fetch:HomeFetch {replies:Mutex::new(replies.into()),requests:Mutex::new(vec![]),checks:AtomicU64::new(0),reject:Mutex::new(None)}}
}
fn envelope(items:Vec<Value>,bookmark:Option<&str>)->Value{json!({"resource_response":{"status":"success","code":0,"endpoint_name":"v3_home_feed","data":items,"bookmark":bookmark}})}
fn home_request(n:u64)->SourceRequest{selected(AccessMode::HomeForYou,n)}
fn decode(r:&HomeFetchRequest)->Value{let form=match &r.body{Some(body)=>url::form_urlencoded::parse(body.as_bytes()).into_owned().collect::<Vec<_>>(),None=>url::Url::parse(&r.url).unwrap().query_pairs().into_owned().collect()};assert_eq!(form.iter().find(|(k,_)|k=="source_url").unwrap().1,"/");serde_json::from_str(&form.iter().find(|(k,_)|k=="data").unwrap().1).unwrap()}
#[test]
fn browser_home_uses_captured_request_shape_and_singular_response_bookmark(){
    let p=home(vec![ok(envelope(vec![item(1)],Some("next"))),ok(envelope(vec![item(2)],None))]);
    let first=run(p.acquire(home_request(1))).unwrap();let mut r=home_request(2);r.continuation=first.continuation;let last=run(p.acquire(r)).unwrap();assert_eq!(last.entries[0].provider_entry_key,"2");assert_eq!(last.coverage,Coverage::Partial);
    let requests=p.fetch.requests.lock().unwrap();assert_eq!(requests[0].method,HomeMethod::Get);assert!(requests[0].body.is_none());assert_eq!(decode(&requests[0]),json!({"options":{"field_set_key":"hf_grid","in_nux":false,"in_news_hub":false,"static_feed":false},"context":{}}));
    assert_eq!(requests[1].method,HomeMethod::Post);assert_eq!(requests[1].url,"https://www.pinterest.com/resource/UserHomefeedResource/get/");assert_eq!(decode(&requests[1])["options"]["bookmarks"],json!(["next"]));
}
#[test]
fn home_envelopes_empty_success_unknown_media_and_wrong_feed_fail_closed(){
    let mut wrong=envelope(vec![item(1)],None);wrong["resource_response"]["endpoint_name"]=json!("v3_search_pins");
    let mut denied=envelope(vec![item(1)],None);denied["resource_response"]["code"]=json!(1);
    let mut bad_media=item(1);bad_media["images"]=Value::Null;
    for body in [wrong,denied,envelope(vec![],None),envelope(vec![],Some("next")),envelope(vec![bad_media],None),json!({"resource_response":{"status":"success","code":0,"endpoint_name":"v3_home_feed"}})]{
        let p=home(vec![ok(body)]);assert!(run(p.acquire(home_request(1))).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),0);
    }
    let p=home(vec![ok(envelope(vec![item(1)],Some("-end-")))]);let page=run(p.acquire(home_request(1))).unwrap();assert!(page.continuation.is_none());assert_eq!(page.coverage,Coverage::Partial);
    for status in [401,440,403]{let p=home(vec![Reply {status,body:vec![],cached:None}]);let error=run(p.acquire(home_request(1))).unwrap_err();assert_eq!(error,if status==403{Error::Denied}else{Error::Expired});}
}
#[test]
fn home_cursor_retains_original_bytes_across_renewal_and_rejects_principal_changes(){
    let body=envelope(vec![item(1),item(2),item(3)],None);let mut p=home(vec![ok(body.clone()),ok(body)]);let first=run(p.acquire(home_request(1))).unwrap();p.authorization.lease_generation=Revision(2);let mut r=home_request(2);r.continuation=first.continuation;
    let last=run(p.acquire(r)).unwrap();assert_eq!(last.entries[0].provider_entry_key,"3");assert!(p.fetch.requests.lock().unwrap()[1].expected_digest.is_some());
    let mut p=home(vec![ok(envelope(vec![item(1),item(2),item(3)],None))]);let first=run(p.acquire(home_request(1))).unwrap();p.authorization.external_identity="98".into();let mut r=home_request(2);r.continuation=first.continuation;assert_eq!(run(p.acquire(r)).unwrap_err(),Error::StaleResult);assert_eq!(p.fetch.requests.lock().unwrap().len(),1);
    let p=home(vec![ok(envelope(vec![item(1),item(2),item(3)],None)),ok(envelope(vec![item(1),item(2),item(4)],None))]);let first=run(p.acquire(home_request(1))).unwrap();let mut r=home_request(2);r.continuation=first.continuation;assert_eq!(run(p.acquire(r)).unwrap_err(),Error::StaleResult);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),2);
}
#[test]
fn home_rejects_anonymous_lease_intent_scope_cycles_and_effect_races(){
    let mut p=home(vec![]);p.authorization.external_identity.clear();assert_eq!(run(p.acquire(home_request(1))).unwrap_err(),Error::Denied);assert!(p.fetch.requests.lock().unwrap().is_empty());
    let p=home(vec![]);let mut r=home_request(1);r.subscription.partition.connection_ref=Some(reference("other"));assert_eq!(run(p.acquire(r)).unwrap_err(),Error::Denied);
    for (at,error,captures) in [(1,Error::ReconnectRequired,0),(3,Error::Expired,0),(4,Error::Denied,0),(5,Error::StaleResult,1)]{
        let p=home(vec![ok(envelope(vec![item(1),item(2)],None))]);*p.fetch.reject.lock().unwrap()=Some((at,error.clone()));assert_eq!(run(p.acquire(home_request(1))).unwrap_err(),error);assert_eq!(p.canonical.calls.load(Ordering::SeqCst),captures);
    }
    let p=home(vec![ok(envelope(vec![item(1)],Some("loop"))),ok(envelope(vec![item(2)],Some("loop")))]);let first=run(p.acquire(home_request(1))).unwrap();let mut r=home_request(2);r.continuation=first.continuation;assert!(run(p.acquire(r)).is_err());assert_eq!(p.canonical.calls.load(Ordering::SeqCst),1);
    let p=home(vec![]);let mut r=home_request(1);r.subscription.partition.mode=AccessMode::Following;assert!(matches!(run(p.acquire(r)),Err(Error::Unsupported {..})));
}
