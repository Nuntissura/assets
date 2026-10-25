use std::collections::{BTreeMap,BTreeSet};
use rss_contracts::{AcquisitionPage,AcquisitionState,Bounds,Continuation,Error,Result,SourceRequest};
use serde::{Deserialize,Serialize};
use url::Url;
use crate::{CanonicalEntryPort,FetchKind,FetchRequest,PublicFetchPort,TikTokProvider,invalid,normalize};

pub fn numeric_id(value:&str)->bool {!value.is_empty()&&value.len()<=32&&value.bytes().all(|v|v.is_ascii_digit())&&!value.starts_with('0')}
pub fn username(value:&str)->bool {!value.is_empty()&&value.len()<=24&&value.bytes().all(|v|v.is_ascii_alphanumeric()||matches!(v,b'_'|b'.'))}
pub fn sec_uid(value:&str)->bool {value.starts_with("MS4wLjAB")&&value.len()>=20&&value.len()<=256&&value.bytes().all(|v|v.is_ascii_alphanumeric()||matches!(v,b'_'|b'-'))}
pub fn media_url(value:&str,bounds:&Bounds)->Result<()> {
    bounds.text("tiktok.media_url",value,bounds.max_url_bytes)?;let url=Url::parse(value).map_err(|_|invalid("tiktok.media_url"))?;
    if url.scheme()!="https"||!url.username().is_empty()||url.password().is_some()||url.port().is_some()||url.fragment().is_some()
        ||!matches!(url.host(),Some(url::Host::Domain(host)) if host.contains('.')&&host!="localhost"&&!host.ends_with(".local")&&!host.ends_with(".localhost")) {return Err(invalid("tiktok.media_url"));}Ok(())
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct SourceTarget {pub name:String,pub item:Option<String>,pub photo:bool}
pub fn resolve_url(raw:&str,bounds:&Bounds)->Result<SourceTarget> {
    bounds.text("tiktok.source_url",raw,bounds.max_url_bytes)?;let url=Url::parse(raw).map_err(|_|invalid("tiktok.source_url"))?;
    if url.scheme()!="https"||!matches!(url.host_str(),Some("tiktok.com"|"www.tiktok.com"))||!url.username().is_empty()||url.password().is_some()
        ||url.port().is_some()||url.query().is_some()||url.fragment().is_some() {return Err(invalid("tiktok.source_url"));}
    let parts:Vec<_>=url.path().trim_matches('/').split('/').collect();let name=parts[0].strip_prefix('@').filter(|name|username(name)).ok_or_else(||invalid("tiktok.username"))?.to_ascii_lowercase();
    if parts.len()==1 {return Ok(SourceTarget {name,item:None,photo:false});}
    if parts.len()==3&&matches!(parts[1],"video"|"photo")&&numeric_id(parts[2]) {return Ok(SourceTarget {name,item:Some(parts[2].into()),photo:parts[1]=="photo"});}
    Err(invalid("tiktok.source_target"))
}
// Actual public browser query context may be added by the host, but it cannot retarget mandatory source/cursor parameters.
fn same_target(actual:&str,expected:&str)->bool {
    let (Ok(actual),Ok(expected))=(Url::parse(actual),Url::parse(expected)) else {return false;};
    if actual.scheme()!=expected.scheme()||actual.host_str()!=expected.host_str()||actual.path()!=expected.path()||actual.port()!=expected.port()
        ||!actual.username().is_empty()||actual.password().is_some()||actual.fragment().is_some() {return false;}
    let actual_pairs:Vec<_>=actual.query_pairs().collect();let expected_pairs:Vec<_>=expected.query_pairs().collect();
    if expected_pairs.iter().any(|(key,value)|actual_pairs.iter().filter(|(other,_)|other==key).count()!=1||!actual_pairs.contains(&(key.clone(),value.clone()))) {return false;}
    actual_pairs.iter().all(|(key,_)|expected_pairs.iter().any(|(expected,_)|key==expected)||matches!(key.as_ref(),
        "app_language"|"app_name"|"browser_language"|"browser_name"|"browser_online"|"browser_platform"|"browser_version"|"channel"|"cookie_enabled"|
        "device_id"|"device_platform"|"focus_state"|"from_page"|"history_len"|"is_fullscreen"|"is_page_visible"|"language"|"os"|"priority_region"|
        "referer"|"region"|"screen_height"|"screen_width"|"tz_name"|"verifyFp"|"webcast_language"|"msToken"|"X-Bogus"|"_signature"))
}
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {version:u8,source_url:String,source_revision:hsscrap::Revision,account_id:String,sec_uid:String,window:u64,digest:Option<String>,offset:usize,
    documents:usize,visited:Vec<u64>,seen:BTreeMap<String,String>}
fn validate_request(request:&SourceRequest,execution:&hsscrap::HostRef,supported:&BTreeSet<String>)->Result<()> {
    request.bounds.validate()?;request.subscription.partition.validate(&request.bounds)?;
    if request.subscription.partition.connection_ref.is_some() { return Err(Error::Denied); }request.subscription.generation.validate()?;request.subscription.common.validate_bounded(&request.bounds,supported)?;
    for reference in [&request.subscription.subscription_ref,&request.subscription.source_ref,&request.subscription.executor_ref,execution] {for value in [&reference.host_instance_id,&reference.owner_kind,&reference.native_id] {
        if value.is_empty() {return Err(invalid("tiktok.request_reference"));}request.bounds.text("tiktok.request_reference",value,request.bounds.max_text_bytes)?;
    }}
    if let Some(next)=&request.continuation {next.validate_for(&request.subscription,&request.bounds)?;}
    if serde_json::to_vec(request).map_err(|_|invalid("tiktok.request"))?.len()>request.bounds.max_operation_bytes {return Err(Error::LimitExceeded {field:"tiktok.request".into()});}Ok(())
}
impl<F:PublicFetchPort,C:CanonicalEntryPort> TikTokProvider<F,C> {
    async fn get(&self,request:&SourceRequest,kind:FetchKind,url:String,digest:Option<String>,fetches:&mut usize)->Result<(Vec<u8>,hsscrap::Instant)> {
        if *fetches==self.limits.max_fetches_per_operation {return Err(Error::LimitExceeded {field:"tiktok.fetches".into()});}*fetches+=1;
        let expected=FetchRequest {kind,url,expected_digest:digest,max_body_bytes:self.limits.max_body_bytes,timeout_millis:self.limits.timeout_millis};
        let response=self.fetch.fetch(request,&self.execution_ref,expected.clone()).await?;
        if Url::parse(&response.final_url).ok().is_some_and(|url|url.path().starts_with("/login")) {return Err(Error::ReconnectRequired);}
        if !same_target(&response.final_url,&expected.url) {return Err(Error::StaleResult);}
        let body=match response.status {
            200 if response.cached_body.is_none()=>response.body,
            304 if response.body.is_empty()=>response.cached_body.ok_or_else(||invalid("tiktok.missing_cached_body"))?,
            401=>return Err(Error::ReconnectRequired),403=>return Err(Error::Denied),404=>return Err(Error::NotFound),410=>return Err(Error::Expired),429=>return Err(Error::RateLimited {retry_after:None}),
            _=>return Err(Error::Network),
        };
        if body.len()>self.limits.max_body_bytes {return Err(Error::LimitExceeded {field:"tiktok.body".into()});}Ok((body,response.observed_at))
    }
    pub(crate) async fn acquire_page(&self,request:SourceRequest)->Result<AcquisitionPage> {
        self.limits.validate()?;validate_request(&request,&self.execution_ref,&self.supported_extensions)?;
        self.source.revision.validate()?;self.source.common.validate_bounded(&request.bounds,&self.supported_extensions)?;
        request.bounds.text("source.owner_scope", &self.source.owner_scope.0, request.bounds.max_text_bytes)?;
        if self.source.source_ref!=request.subscription.source_ref||self.source.owner_scope.0.is_empty() {return Err(Error::Denied);}
        if self.source.provider!=rss_contracts::Provider::Tiktok||request.subscription.partition.mode!=rss_contracts::AccessMode::PublicProfile {return Err(Error::Unsupported {provider:rss_contracts::Provider::Tiktok,mode:request.subscription.partition.mode});}
        if self.source.paused||request.subscription.paused {return Err(Error::Cancelled);}
        let target=resolve_url(&self.source.original_url,&request.bounds)?;
        let source_url=if let Some(id)=&target.item {format!("https://www.tiktok.com/@{}/{}/{id}",target.name,if target.photo {"photo"} else {"video"})} else {format!("https://www.tiktok.com/@{}",target.name)};
        let mut cursor=if let Some(next)=&request.continuation {serde_json::from_str::<Cursor>(&next.token).map_err(|_|invalid("tiktok.cursor"))?}
            else {Cursor {version:1,source_url:source_url.clone(),source_revision:self.source.revision,account_id:String::new(),sec_uid:String::new(),window:self.initial_cursor_millis,
                digest:None,offset:0,documents:0,visited:vec![],seen:BTreeMap::new()}};
        if cursor.version!=1||cursor.source_url!=source_url||cursor.source_revision!=self.source.revision||cursor.window<normalize::MIN_CURSOR||cursor.window>i64::MAX as u64
            ||cursor.documents>=self.limits.max_documents||cursor.visited.len()!=cursor.documents||cursor.visited.iter().collect::<BTreeSet<_>>().len()!=cursor.visited.len()
            ||cursor.visited.contains(&cursor.window)||(cursor.offset>0)!=cursor.digest.is_some()||cursor.offset>self.limits.max_document_entries
            ||cursor.seen.len()>self.limits.max_documents.saturating_mul(self.limits.max_document_entries)||cursor.seen.keys().any(|id|!numeric_id(id))
            ||cursor.digest.as_ref().is_some_and(|digest|digest.len()!=64||!digest.bytes().all(|v|v.is_ascii_hexdigit())) {return Err(invalid("tiktok.cursor"));}
        let mut fetches=0;
        let account=if target.item.is_none() {
            let (bytes,_)=self.get(&request,FetchKind::Profile,source_url.clone(),None,&mut fetches).await?;
            let account=normalize::account(&bytes,&target.name,self.limits.max_body_bytes,&request.bounds)?;
            if !cursor.account_id.is_empty()&&(cursor.account_id!=account.id||cursor.sec_uid!=account.sec_uid) {return Err(Error::StaleResult);}
            cursor.account_id=account.id.clone();cursor.sec_uid=account.sec_uid.clone();Some(account)
        } else {None};
        loop {
            let (document,observed_at)=if let Some(id)=&target.item {
                if request.continuation.is_some() {return Err(invalid("tiktok.item_continuation"));}
                let (bytes,time)=self.get(&request,FetchKind::Item,source_url.clone(),None,&mut fetches).await?;
                (normalize::item_document(&bytes,id,&target.name,self.limits.max_body_bytes,&request.bounds)?,time)
            } else {
                let account=account.as_ref().ok_or_else(||invalid("tiktok.account"))?;
                let mut url=Url::parse("https://www.tiktok.com/api/creator/item_list/").map_err(|_|invalid("tiktok.endpoint"))?;
                url.query_pairs_mut().append_pair("aid","1988").append_pair("count","15").append_pair("cursor",&cursor.window.to_string()).append_pair("secUid",&account.sec_uid).append_pair("type","1");
                let (bytes,time)=self.get(&request,FetchKind::CreatorWindow,url.to_string(),cursor.digest.clone(),&mut fetches).await?;
                (normalize::window(&bytes,account,cursor.window,self.limits.max_body_bytes,self.limits.max_document_entries,&request.bounds)?,time)
            };
            if cursor.digest.as_ref().is_some_and(|digest|digest!=&document.digest) {return Err(Error::StaleResult);}
            if cursor.offset>0&&cursor.offset>=document.entries.len() {return Err(invalid("tiktok.cursor_offset"));}
            let observed_seconds=chrono::DateTime::parse_from_rfc3339(observed_at.as_str()).map_err(|_|invalid("tiktok.observed_at"))?.timestamp().max(0) as u64;
            if document.entries.iter().flat_map(|entry|&entry.media).any(|media|media.expires_at_unix.is_some_and(|expiry|expiry<=observed_seconds)) {return Err(Error::Expired);}
            for entry in &document.entries {
                if cursor.seen.get(&entry.id).is_some_and(|identity|identity!=&format!("{}/{}",entry.author_id,entry.photo)) {return Err(invalid("tiktok.retargeted_item"));}
            }
            let mut next=cursor.clone();let mut selected=vec![];let mut end=cursor.offset;
            while end<document.entries.len()&&selected.len()<request.bounds.max_page_items {
                let entry=&document.entries[end];if !next.seen.contains_key(&entry.id) {next.seen.insert(entry.id.clone(),format!("{}/{}",entry.author_id,entry.photo));selected.push(entry.clone());}end+=1;
            }
            let next=if end<document.entries.len() {next.offset=end;next.digest=Some(document.digest.clone());Some(next)}
                else if let Some(window)=document.next {
                    next.visited.push(cursor.window);next.documents+=1;if next.documents>=self.limits.max_documents {return Err(Error::LimitExceeded {field:"tiktok.documents".into()});}
                    next.window=window;next.digest=None;next.offset=0;Some(next)
                } else {None};
            let continuation=next.as_ref().map(|next|->Result<_> {
                let continuation=Continuation {source_ref:request.subscription.source_ref.clone(),subscription_ref:request.subscription.subscription_ref.clone(),partition:request.subscription.partition.clone(),
                    executor_ref:request.subscription.executor_ref.clone(),generation:request.subscription.generation,token:serde_json::to_string(next).map_err(|_|invalid("tiktok.cursor"))?};
                continuation.validate_for(&request.subscription,&request.bounds)?;Ok(continuation)
            }).transpose()?;
            if selected.is_empty() {
                if let Some(next)=next {cursor=next;continue;}
                if request.continuation.is_none()&&!(document.terminal&&account.as_ref().is_some_and(|account|account.video_count==Some(0))) {return Err(invalid("tiktok.empty_unverified"));}
            }
            let mut entries=vec![];let mut bytes=0usize;
            for evidence in selected {
                let entry=self.canonical.prepare(&request,&self.execution_ref,evidence.clone()).await?;entry.validate(&request.bounds,&self.supported_extensions)?;
                let mut expected=entry.observation.clone();evidence.populate(&mut expected);
                if entry.provider_entry_key!=evidence.id||entry.capture.captured_url!=evidence.url()||entry.capture.access_partition!=request.subscription.partition.scope||entry.observation!=expected {return Err(invalid("tiktok.canonical_preparation"));}
                bytes=bytes.checked_add(serde_json::to_vec(&entry).map_err(|_|invalid("tiktok.page"))?.len()).ok_or_else(||invalid("tiktok.page_bytes"))?;
                if bytes>request.bounds.max_operation_bytes {return Err(Error::LimitExceeded {field:"tiktok.page_bytes".into()});}entries.push(entry);
            }
            let empty=entries.is_empty();let terminal=empty&&request.continuation.is_some();
            let state=if terminal {AcquisitionState::Partial {observed_at,reason_code:rss_contracts::TERMINAL_SOURCE_DOCUMENT.into()}}
                else if empty {AcquisitionState::VerifiedEmpty {observed_at,selected_source_identity:format!("tiktok/creator/{}/public_posts",cursor.account_id)}}
                else {AcquisitionState::Partial {observed_at,reason_code:if continuation.is_some() {"tiktok_creator_window_continues"} else {"tiktok_current_public_traversal_only"}.into()}};
            let page=AcquisitionPage {subscription_ref:request.subscription.subscription_ref.clone(),source_ref:request.subscription.source_ref.clone(),partition:request.subscription.partition.clone(),
                executor_ref:request.subscription.executor_ref.clone(),generation:request.subscription.generation,operation_id:request.operation_id,entries,state,
                coverage:if empty&&!terminal {hsscrap::Coverage::Complete} else {hsscrap::Coverage::Partial},continuation};
            page.validate(&request.subscription,&request.bounds,&self.supported_extensions)?;return Ok(page);
        }
    }
}
