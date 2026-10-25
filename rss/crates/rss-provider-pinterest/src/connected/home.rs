//! Authenticated browser Home, separate from OAuth selected-board acquisition.
use super::{digest,token};
use crate::{CanonicalEntryPort,EntryEvidence,FetchResponse,ProviderLimits,invalid};
use rss_contracts::{AccessMode,AcquisitionPage,AcquisitionState,Bounds,CaptureIntent,Continuation,Error,HostRef,Pending,Provider,Revision,ScopeRef,Source,SourceRequest};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use std::collections::BTreeSet;
const ENDPOINT:&str="https://www.pinterest.com/resource/UserHomefeedResource/get/";
#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserHomeContext {
    pub connection_ref:HostRef,pub owner_scope:ScopeRef,pub external_identity:String,
    pub lease_generation:Revision,pub capture_intent:CaptureIntent,
}
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum HomeMethod {Get,Post}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct HomeFetchRequest {
    pub method:HomeMethod,pub url:String,pub body:Option<String>,pub expected_digest:Option<String>,pub max_body_bytes:usize,pub timeout_millis:u64,
}
/// Attest the exact authenticated browser session's numeric account, owner,
/// connection, lease attempt and Home intent. Reject anonymous/OAuth fallback.
/// Hold the session lease through canonical settlement. Credentials, CSRF,
/// refresh and retained response storage never enter the portable context.
pub trait BrowserHomeFetchPort:Send+Sync {
    fn current<'a>(&'a self,request:&'a SourceRequest,execution:&'a HostRef,context:&'a BrowserHomeContext)->Pending<'a,()>;
    fn fetch<'a>(&'a self,request:&'a SourceRequest,execution:&'a HostRef,context:&'a BrowserHomeContext,fetch:HomeFetchRequest)->Pending<'a,FetchResponse>;
}
pub struct BrowserHomeProvider<F,C> {
    pub source:Source,pub execution_ref:HostRef,pub limits:ProviderLimits,pub supported_extensions:BTreeSet<String>,
    pub authorization:BrowserHomeContext,pub fetch:F,pub canonical:C,
}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {version:u8,connection:HostRef,owner:ScopeRef,identity:String,source_revision:Revision,source_url:String,
    page:Option<String>,digest:Option<String>,offset:usize,documents:usize,visited:Vec<Option<String>>,seen:BTreeSet<String>}
struct Document {entries:Vec<EntryEvidence>,next:Option<String>,digest:String}
impl<F:BrowserHomeFetchPort,C:CanonicalEntryPort> rss_contracts::ProviderPort for BrowserHomeProvider<F,C> {
    fn capabilities(&self)->Vec<rss_contracts::Capability>{vec![rss_contracts::Capability {provider:Provider::Pinterest,mode:AccessMode::HomeForYou,status:rss_contracts::CapabilityStatus::Available,paginated:true,media_metadata:true,adapter_version:"pinterest.browser-home.v1".into()}]}
    fn acquire<'a>(&'a self,request:SourceRequest)->Pending<'a,AcquisitionPage>{Box::pin(self.acquire_page(request))}
}
impl<F:BrowserHomeFetchPort,C:CanonicalEntryPort> BrowserHomeProvider<F,C> {
    fn validate(&self,r:&SourceRequest)->rss_contracts::Result<()> {
        r.bounds.validate()?;self.limits.validate()?;r.subscription.partition.validate(&r.bounds)?;r.subscription.generation.validate()?;
        self.source.revision.validate()?;self.authorization.lease_generation.validate()?;
        self.source.common.validate_bounded(&r.bounds,&self.supported_extensions)?;r.subscription.common.validate_bounded(&r.bounds,&self.supported_extensions)?;
        if self.source.provider!=Provider::Pinterest||r.subscription.partition.mode!=AccessMode::HomeForYou{return Err(Error::Unsupported {provider:Provider::Pinterest,mode:r.subscription.partition.mode});}
        if self.source.paused||r.subscription.paused{return Err(Error::Cancelled);}
        let a=&self.authorization;
        if self.source.source_ref!=r.subscription.source_ref||self.source.owner_scope!=a.owner_scope||a.owner_scope.0.is_empty()
            ||!crate::public::numeric_id(&a.external_identity)||r.subscription.partition.connection_ref.as_ref()!=Some(&a.connection_ref){return Err(Error::Denied);}
        let url=url::Url::parse(&self.source.original_url).map_err(|_|invalid("pinterest.home_source"))?;
        if url.scheme()!="https"||!matches!(url.host_str(),Some("www.pinterest.com"|"pinterest.com"))||url.path()!="/"||url.query().is_some()||url.fragment().is_some()||!url.username().is_empty()||url.password().is_some()||url.port().is_some(){return Err(invalid("pinterest.home_source"));}
        match &a.capture_intent {CaptureIntent::FollowSource {original_url,provider,partition} if original_url==&self.source.original_url&&*provider==Provider::Pinterest&&partition==&r.subscription.partition=>{},_=>return Err(Error::Denied)}
        for reference in [&a.connection_ref,&self.source.source_ref,&r.subscription.subscription_ref,&r.subscription.executor_ref,&self.execution_ref]{for v in [&reference.host_instance_id,&reference.owner_kind,&reference.native_id]{if v.is_empty(){return Err(invalid("pinterest.home_reference"));}r.bounds.text("pinterest.home_reference",v,r.bounds.max_text_bytes)?;}}
        if serde_json::to_vec(&(&self.source,a,r)).map_err(|_|invalid("pinterest.home_request"))?.len()>r.bounds.max_operation_bytes{return Err(Error::LimitExceeded {field:"pinterest.home_request".into()});}
        if let Some(c)=&r.continuation{c.validate_for(&r.subscription,&r.bounds)?;}Ok(())
    }
    async fn current(&self,r:&SourceRequest)->rss_contracts::Result<()>{self.fetch.current(r,&self.execution_ref,&self.authorization).await}
    fn cursor(&self,r:&SourceRequest)->rss_contracts::Result<Cursor>{
        let a=&self.authorization;let c=if let Some(next)=&r.continuation {
            handshake_canonical_json::jcs_from_slice(next.token.as_bytes(),r.bounds.max_text_bytes).map_err(|_|invalid("pinterest.home_cursor"))?;
            serde_json::from_str::<Cursor>(&next.token).map_err(|_|invalid("pinterest.home_cursor"))?
        }else{Cursor {version:1,connection:a.connection_ref.clone(),owner:a.owner_scope.clone(),identity:a.external_identity.clone(),source_revision:self.source.revision,source_url:self.source.original_url.clone(),page:None,digest:None,offset:0,documents:0,visited:vec![],seen:BTreeSet::new()}};
        if c.version!=1||c.connection!=a.connection_ref||c.owner!=a.owner_scope||c.identity!=a.external_identity||c.source_revision!=self.source.revision||c.source_url!=self.source.original_url
            ||c.documents>=self.limits.max_documents||c.visited.len()!=c.documents||c.visited.iter().collect::<BTreeSet<_>>().len()!=c.visited.len()||c.visited.contains(&c.page)
            ||(c.offset>0)!=c.digest.is_some()||c.offset>self.limits.max_document_entries||c.seen.len()>self.limits.max_documents.saturating_mul(self.limits.max_document_entries)||c.seen.iter().any(|id|!crate::public::numeric_id(id))
            ||c.digest.as_ref().is_some_and(|v|v.len()!=64||!v.bytes().all(|v|v.is_ascii_hexdigit())){return Err(Error::StaleResult);}
        if let Some(next)=&c.page{token(next,&r.bounds)?;}Ok(c)
    }
    async fn get(&self,r:&SourceRequest,c:&Cursor,calls:&mut usize)->rss_contracts::Result<(Document,hsscrap::Instant)>{
        if *calls>=self.limits.max_fetches_per_operation{return Err(Error::LimitExceeded {field:"pinterest.home_fetches".into()});}*calls+=1;
        let mut options=json!({"field_set_key":"hf_grid","in_nux":false,"in_news_hub":false,"static_feed":false});
        if let Some(next)=&c.page{options["bookmarks"]=json!([next]);}
        let data=serde_json::to_string(&json!({"options":options,"context":{}})).map_err(|_|invalid("pinterest.home_request"))?;
        let form=url::form_urlencoded::Serializer::new(String::new()).append_pair("source_url","/").append_pair("data",&data).finish();
        let fetch=HomeFetchRequest {method:if c.page.is_some(){HomeMethod::Post}else{HomeMethod::Get},url:if c.page.is_some(){ENDPOINT.into()}else{format!("{ENDPOINT}?{form}")},body:c.page.is_some().then_some(form),expected_digest:c.digest.clone(),max_body_bytes:self.limits.max_body_bytes,timeout_millis:self.limits.timeout_millis};
        self.current(r).await?;let response=self.fetch.fetch(r,&self.execution_ref,&self.authorization,fetch.clone()).await?;self.current(r).await?;
        if response.final_url!=fetch.url{return Err(Error::StaleResult);}
        let bytes=match response.status {200 if response.cached_body.is_none()=>response.body,304 if response.body.is_empty()=>response.cached_body.ok_or_else(||invalid("pinterest.home_missing_cached_body"))?,401|440|410=>return Err(Error::Expired),403=>return Err(Error::Denied),429=>return Err(Error::RateLimited {retry_after:None}),_=>return Err(Error::Network)};
        if bytes.len()>self.limits.max_body_bytes{return Err(Error::LimitExceeded {field:"pinterest.home_body".into()});}
        if c.digest.as_ref().is_some_and(|expected|expected!=&digest(&bytes)){return Err(Error::StaleResult);}
        Ok((document(&bytes,self.limits.max_body_bytes,self.limits.max_document_entries,&r.bounds)?,response.observed_at))
    }
    async fn acquire_page(&self,r:SourceRequest)->rss_contracts::Result<AcquisitionPage>{
        self.validate(&r)?;self.current(&r).await?;let mut cursor=self.cursor(&r)?;let mut calls=0;
        loop {
            let (document,time)=self.get(&r,&cursor,&mut calls).await?;
            if document.entries.is_empty()&&r.continuation.is_none(){return Err(invalid("pinterest.home_empty_unverified"));}
            if cursor.offset>document.entries.len()||cursor.offset>0&&cursor.offset==document.entries.len(){return Err(invalid("pinterest.home_cursor_offset"));}
            if document.next.as_ref().is_some_and(|v|cursor.page.as_ref()==Some(v)||cursor.visited.contains(&Some(v.clone()))){return Err(invalid("pinterest.home_cursor_cycle"));}
            let mut selected=vec![];let mut end=cursor.offset;
            while end<document.entries.len()&&selected.len()<r.bounds.max_page_items {let entry=&document.entries[end];if !cursor.seen.contains(&entry.id){
                if cursor.seen.len()>=self.limits.max_documents.saturating_mul(self.limits.max_document_entries){return Err(invalid("pinterest.home_seen"));}cursor.seen.insert(entry.id.clone());selected.push(entry.clone());}end+=1;}
            let next=if end<document.entries.len(){let mut next=cursor.clone();next.offset=end;next.digest=Some(document.digest.clone());Some(next)}else if let Some(bookmark)=&document.next {
                let mut next=cursor.clone();next.visited.push(next.page.clone());next.documents+=1;if next.documents>=self.limits.max_documents{return Err(Error::LimitExceeded {field:"pinterest.home_documents".into()});}next.page=Some(bookmark.clone());next.offset=0;next.digest=None;Some(next)
            }else{None};
            if selected.is_empty()&&next.is_some(){cursor=next.unwrap();continue;}
            if selected.is_empty()&&r.continuation.is_none(){return Err(invalid("pinterest.home_empty_unverified"));}
            let continuation=next.map(|c|->rss_contracts::Result<_>{let result=Continuation {source_ref:r.subscription.source_ref.clone(),subscription_ref:r.subscription.subscription_ref.clone(),partition:r.subscription.partition.clone(),executor_ref:r.subscription.executor_ref.clone(),generation:r.subscription.generation,token:serde_json::to_string(&c).map_err(|_|invalid("pinterest.home_cursor"))?};result.validate_for(&r.subscription,&r.bounds)?;Ok(result)}).transpose()?;
            let mut entries=vec![];let mut bytes=0usize;
            for evidence in selected {self.current(&r).await?;let entry=self.canonical.prepare(&r,&self.execution_ref,evidence.clone()).await?;self.current(&r).await?;entry.validate(&r.bounds,&self.supported_extensions)?;
                let mut expected=entry.observation.clone();evidence.populate(&mut expected);
                if entry.provider_entry_key!=evidence.id||entry.capture.captured_url!=evidence.url()||entry.capture.access_partition!=r.subscription.partition.scope||entry.observation!=expected{return Err(invalid("pinterest.home_canonical_preparation"));}
                bytes=bytes.checked_add(serde_json::to_vec(&entry).map_err(|_|invalid("pinterest.home_page"))?.len()).ok_or_else(||invalid("pinterest.home_page"))?;if bytes>r.bounds.max_operation_bytes{return Err(Error::LimitExceeded {field:"pinterest.home_page".into()});}entries.push(entry);
            }
            let state=AcquisitionState::Partial {observed_at:time,reason_code:if entries.is_empty(){rss_contracts::TERMINAL_SOURCE_DOCUMENT}else{"pinterest_current_browser_home_only"}.into()};
            let page=AcquisitionPage {subscription_ref:r.subscription.subscription_ref.clone(),source_ref:r.subscription.source_ref.clone(),partition:r.subscription.partition.clone(),executor_ref:r.subscription.executor_ref.clone(),generation:r.subscription.generation,operation_id:r.operation_id,entries,state,coverage:hsscrap::Coverage::Partial,continuation};
            page.validate(&r.subscription,&r.bounds,&self.supported_extensions)?;self.current(&r).await?;return Ok(page);
        }
    }
}
fn document(bytes:&[u8],max:usize,max_entries:usize,b:&Bounds)->rss_contracts::Result<Document>{
    let root=crate::normalize::json(bytes,max)?;let response=&root["resource_response"];
    if response["endpoint_name"].as_str()!=Some("v3_home_feed")||response["status"].as_str()!=Some("success")||response["code"].as_u64()!=Some(0){return Err(Error::Denied);}
    let pins=response.get("data").and_then(Value::as_array).ok_or_else(||invalid("pinterest.home_pins"))?;
    if pins.len()>max_entries||pins.len()>b.max_collection_items{return Err(Error::LimitExceeded {field:"pinterest.home_pins".into()});}
    let next=match response.get("bookmark"){None|Some(Value::Null)=>None,Some(Value::String(value))=>{
        token(value,b)?;let normalized=crate::normalize::bookmarks(&json!({"resource":{"options":{"bookmarks":[value]}}}),b)?;normalized.and_then(|v|v.into_iter().next())
    },_=>return Err(invalid("pinterest.home_bookmark"))};
    let hash=digest(bytes);let mut entries:Vec<EntryEvidence>=vec![];
    for pin in pins {let entry=crate::normalize::entry(pin,&hash,b)?;if let Some(prior)=entries.iter().find(|old|old.id==entry.id){if prior!=&entry{return Err(invalid("pinterest.home_repeated_pin"));}}else{entries.push(entry);}}
    Ok(Document {entries,next,digest:hash})
}
