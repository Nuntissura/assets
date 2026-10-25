//! Own-created Pins selected by an admitted browser principal, never saved Pins.
use super::{digest,token};
use crate::{CanonicalEntryPort,FetchRequest,FetchResponse,ProviderLimits,invalid,public};
use rss_contracts::{AccessMode,AcquisitionPage,AcquisitionState,CaptureIntent,Continuation,Error,HostRef,Pending,Provider,Revision,ScopeRef,Source,SourceRequest};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use std::collections::BTreeSet;
#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserCreatedContext {
    pub connection_ref:HostRef,pub owner_scope:ScopeRef,pub external_identity:String,
    pub lease_generation:Revision,pub capture_intent:CaptureIntent,
}
/// Attest the actual authenticated browser principal, owner, connection, lease
/// attempt and exact own-created intent. Hold its lease through settlement;
/// credentials, refresh and original retained responses remain host-owned.
pub trait BrowserCreatedFetchPort:Send+Sync {
    fn current<'a>(&'a self,r:&'a SourceRequest,execution:&'a HostRef,a:&'a BrowserCreatedContext)->Pending<'a,()>;
    fn fetch<'a>(&'a self,r:&'a SourceRequest,execution:&'a HostRef,a:&'a BrowserCreatedContext,fetch:FetchRequest)->Pending<'a,FetchResponse>;
}
pub struct BrowserCreatedProvider<F,C> {
    pub source:Source,pub execution_ref:HostRef,pub limits:ProviderLimits,pub supported_extensions:BTreeSet<String>,
    pub authorization:BrowserCreatedContext,pub fetch:F,pub canonical:C,
}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {version:u8,connection:HostRef,owner:ScopeRef,identity:String,username:String,source_url:String,source_revision:Revision,
    page:Option<Vec<String>>,digest:Option<String>,offset:usize,documents:usize,visited:Vec<Option<Vec<String>>>,seen:BTreeSet<String>}
impl<F:BrowserCreatedFetchPort,C:CanonicalEntryPort> rss_contracts::ProviderPort for BrowserCreatedProvider<F,C> {
    fn capabilities(&self)->Vec<rss_contracts::Capability>{vec![rss_contracts::Capability {provider:Provider::Pinterest,mode:AccessMode::OwnUploads,status:rss_contracts::CapabilityStatus::Available,paginated:true,media_metadata:true,adapter_version:"pinterest.browser-created.v1".into()}]}
    fn acquire<'a>(&'a self,r:SourceRequest)->Pending<'a,AcquisitionPage>{Box::pin(self.acquire_page(r))}
}
fn resource(name:&str,options:Value)->rss_contracts::Result<String>{let mut url=url::Url::parse(&format!("https://www.pinterest.com/resource/{name}Resource/get/")).map_err(|_|invalid("pinterest.created_endpoint"))?;
    let data=serde_json::to_string(&json!({"options":options})).map_err(|_|invalid("pinterest.created_request"))?;url.query_pairs_mut().append_pair("source_url","").append_pair("data",&data);Ok(url.to_string())}
fn envelope(bytes:&[u8],max:usize)->rss_contracts::Result<Value>{let root=crate::normalize::json(bytes,max)?;let rr=&root["resource_response"];if rr["status"].as_str()!=Some("success")||rr["code"].as_u64()!=Some(0){return Err(Error::Denied);}Ok(root)}
impl<F:BrowserCreatedFetchPort,C:CanonicalEntryPort> BrowserCreatedProvider<F,C> {
    fn validate(&self,r:&SourceRequest)->rss_contracts::Result<String>{
        r.bounds.validate()?;self.limits.validate()?;r.subscription.partition.validate(&r.bounds)?;r.subscription.generation.validate()?;self.source.revision.validate()?;self.authorization.lease_generation.validate()?;
        self.source.common.validate_bounded(&r.bounds,&self.supported_extensions)?;r.subscription.common.validate_bounded(&r.bounds,&self.supported_extensions)?;
        if self.source.provider!=Provider::Pinterest||r.subscription.partition.mode!=AccessMode::OwnUploads{return Err(Error::Unsupported {provider:Provider::Pinterest,mode:r.subscription.partition.mode});}
        if self.source.paused||r.subscription.paused{return Err(Error::Cancelled);}let a=&self.authorization;
        if self.source.source_ref!=r.subscription.source_ref||self.source.owner_scope!=a.owner_scope||a.owner_scope.0.is_empty()||!public::numeric_id(&a.external_identity)||r.subscription.partition.connection_ref.as_ref()!=Some(&a.connection_ref){return Err(Error::Denied);}
        let url=url::Url::parse(&self.source.original_url).map_err(|_|invalid("pinterest.created_source"))?;
        let segments=url.path().trim_matches('/').split('/').collect::<Vec<_>>();
        if url.scheme()!="https"||!matches!(url.host_str(),Some("www.pinterest.com"|"pinterest.com"))||url.query().is_some()||url.fragment().is_some()||!url.username().is_empty()||url.password().is_some()||url.port().is_some()||segments.len()!=2||segments[1]!="_created"{return Err(invalid("pinterest.created_source"));}
        let public::SourceTarget::Discovery(username)=public::resolve_url(&format!("https://www.pinterest.com/{}/",segments[0]),&r.bounds)? else{return Err(invalid("pinterest.created_source"));};
        match &a.capture_intent {CaptureIntent::FollowSource {original_url,provider,partition} if original_url==&self.source.original_url&&*provider==Provider::Pinterest&&partition==&r.subscription.partition=>{},_=>return Err(Error::Denied)}
        for reference in [&a.connection_ref,&self.source.source_ref,&r.subscription.subscription_ref,&r.subscription.executor_ref,&self.execution_ref]{for v in [&reference.host_instance_id,&reference.owner_kind,&reference.native_id]{if v.is_empty(){return Err(invalid("pinterest.created_reference"));}r.bounds.text("pinterest.created_reference",v,r.bounds.max_text_bytes)?;}}
        if serde_json::to_vec(&(&self.source,a,r)).map_err(|_|invalid("pinterest.created_request"))?.len()>r.bounds.max_operation_bytes{return Err(Error::LimitExceeded {field:"pinterest.created_request".into()});}
        if let Some(c)=&r.continuation{c.validate_for(&r.subscription,&r.bounds)?;}Ok(username)
    }
    async fn current(&self,r:&SourceRequest)->rss_contracts::Result<()>{self.fetch.current(r,&self.execution_ref,&self.authorization).await}
    async fn get(&self,r:&SourceRequest,url:String,expected:Option<String>,calls:&mut usize)->rss_contracts::Result<(Vec<u8>,hsscrap::Instant)>{
        r.bounds.text("pinterest.created_endpoint",&url,r.bounds.max_url_bytes)?;
        if *calls>=self.limits.max_fetches_per_operation{return Err(Error::LimitExceeded {field:"pinterest.created_fetches".into()});}*calls+=1;self.current(r).await?;
        let response=self.fetch.fetch(r,&self.execution_ref,&self.authorization,FetchRequest {url:url.clone(),expected_digest:expected.clone(),max_body_bytes:self.limits.max_body_bytes,timeout_millis:self.limits.timeout_millis}).await?;self.current(r).await?;
        if response.final_url!=url{return Err(Error::StaleResult);}let bytes=match response.status {200 if response.cached_body.is_none()=>response.body,304 if response.body.is_empty()=>response.cached_body.ok_or_else(||invalid("pinterest.created_missing_cache"))?,401|440|410=>return Err(Error::Expired),403=>return Err(Error::Denied),404=>return Err(Error::NotFound),429=>return Err(Error::RateLimited {retry_after:None}),_=>return Err(Error::Network)};
        if bytes.len()>self.limits.max_body_bytes{return Err(Error::LimitExceeded {field:"pinterest.created_body".into()});}if expected.is_some_and(|d|d!=digest(&bytes)){return Err(Error::StaleResult);}Ok((bytes,response.observed_at))
    }
    fn cursor(&self,r:&SourceRequest,username:&str)->rss_contracts::Result<Cursor>{
        let a=&self.authorization;let c=if let Some(next)=&r.continuation {handshake_canonical_json::jcs_from_slice(next.token.as_bytes(),r.bounds.max_text_bytes).map_err(|_|invalid("pinterest.created_cursor"))?;serde_json::from_str::<Cursor>(&next.token).map_err(|_|invalid("pinterest.created_cursor"))?}
            else{Cursor {version:1,connection:a.connection_ref.clone(),owner:a.owner_scope.clone(),identity:a.external_identity.clone(),username:username.into(),source_url:self.source.original_url.clone(),source_revision:self.source.revision,page:None,digest:None,offset:0,documents:0,visited:vec![],seen:BTreeSet::new()}};
        if c.version!=1||c.connection!=a.connection_ref||c.owner!=a.owner_scope||c.identity!=a.external_identity||c.username!=username||c.source_url!=self.source.original_url||c.source_revision!=self.source.revision
            ||c.documents>=self.limits.max_documents||c.visited.len()!=c.documents||c.visited.iter().collect::<BTreeSet<_>>().len()!=c.visited.len()||c.visited.contains(&c.page)
            ||(c.offset>0)!=c.digest.is_some()||c.offset>self.limits.max_document_entries||c.seen.len()>self.limits.max_documents.saturating_mul(self.limits.max_document_entries)||c.seen.iter().any(|id|!public::numeric_id(id))
            ||c.digest.as_ref().is_some_and(|v|v.len()!=64||!v.bytes().all(|v|v.is_ascii_hexdigit())){return Err(Error::StaleResult);}
        if let Some(page)=&c.page{if page.is_empty()||page.len()>r.bounds.max_collection_items{return Err(invalid("pinterest.created_bookmarks"));}for v in page{token(v,&r.bounds)?;}}Ok(c)
    }
    async fn acquire_page(&self,r:SourceRequest)->rss_contracts::Result<AcquisitionPage>{
        let username=self.validate(&r)?;self.current(&r).await?;let mut cursor=self.cursor(&r,&username)?;let mut calls=0;
        let (bytes,_)=self.get(&r,resource("User",json!({"username":username,"field_set_key":"profile"}))?,None,&mut calls).await?;
        let root=envelope(&bytes,self.limits.max_body_bytes)?;let profile=&root["resource_response"]["data"];
        if root.pointer("/resource/name").is_some_and(|v|v.as_str()!=Some("UserResource")){return Err(Error::StaleResult);}
        if profile["id"].as_str()!=Some(self.authorization.external_identity.as_str())||!profile["username"].as_str().is_some_and(|v|v.eq_ignore_ascii_case(&username)){return Err(Error::Denied);}
        loop {
            let mut options=json!({"username":username,"exclude_add_pin_rep":true,"field_set_key":"grid_item","is_own_profile_pins":false});if let Some(page)=&cursor.page{options["bookmarks"]=json!(page);}
            let (bytes,time)=self.get(&r,resource("UserActivityPins",options)?,cursor.digest.clone(),&mut calls).await?;
            let root=envelope(&bytes,self.limits.max_body_bytes)?;let data=&root["resource_response"]["data"];let pins=data.as_array().or_else(||data.get("results").and_then(Value::as_array)).ok_or_else(||invalid("pinterest.created_pins"))?;
            if root.pointer("/resource/name").is_some_and(|v|v.as_str()!=Some("UserActivityPinsResource")){return Err(Error::StaleResult);}
            if pins.len()>self.limits.max_document_entries||pins.len()>r.bounds.max_collection_items{return Err(Error::LimitExceeded {field:"pinterest.created_pins".into()});}
            for pin in pins {if let Some(pinner)=pin.get("pinner").filter(|v|!v.is_null()) {
                if !pinner.is_object(){return Err(invalid("pinterest.created_attribution"));}
                if pinner.get("id").filter(|v|!v.is_null()).is_some_and(|id|id.as_str()!=Some(self.authorization.external_identity.as_str()))
                    ||pinner.get("username").filter(|v|!v.is_null()).is_some_and(|v|!v.as_str().is_some_and(|v|v.eq_ignore_ascii_case(&username))){return Err(Error::Denied);}
            }}
            let document=crate::normalize::page(&bytes,self.limits.max_body_bytes,self.limits.max_document_entries.min(r.bounds.max_collection_items),&r.bounds)?;
            if document.entries.is_empty()&&r.continuation.is_none(){return Err(invalid("pinterest.created_empty_unverified"));}
            if cursor.offset>document.entries.len()||cursor.offset>0&&cursor.offset==document.entries.len(){return Err(invalid("pinterest.created_offset"));}
            if document.next.as_ref().is_some_and(|v|cursor.page.as_ref()==Some(v)||cursor.visited.contains(&Some(v.clone()))){return Err(invalid("pinterest.created_cycle"));}
            let mut selected=vec![];let mut end=cursor.offset;
            while end<document.entries.len()&&selected.len()<r.bounds.max_page_items{let entry=&document.entries[end];if !cursor.seen.contains(&entry.id){if cursor.seen.len()>=self.limits.max_documents.saturating_mul(self.limits.max_document_entries){return Err(invalid("pinterest.created_seen"));}cursor.seen.insert(entry.id.clone());selected.push(entry.clone());}end+=1;}
            let next=if end<document.entries.len(){let mut next=cursor.clone();next.offset=end;next.digest=Some(document.digest.clone());Some(next)}else if let Some(page)=document.next {let mut next=cursor.clone();next.visited.push(next.page.clone());next.documents+=1;if next.documents>=self.limits.max_documents{return Err(Error::LimitExceeded {field:"pinterest.created_documents".into()});}next.page=Some(page);next.offset=0;next.digest=None;Some(next)}else{None};
            if selected.is_empty()&&next.is_some(){cursor=next.unwrap();continue;}
            let continuation=next.map(|c|->rss_contracts::Result<_>{let next=Continuation {source_ref:r.subscription.source_ref.clone(),subscription_ref:r.subscription.subscription_ref.clone(),partition:r.subscription.partition.clone(),executor_ref:r.subscription.executor_ref.clone(),generation:r.subscription.generation,token:serde_json::to_string(&c).map_err(|_|invalid("pinterest.created_cursor"))?};next.validate_for(&r.subscription,&r.bounds)?;Ok(next)}).transpose()?;
            let mut entries=vec![];let mut bytes=0usize;
            for evidence in selected {self.current(&r).await?;let entry=self.canonical.prepare(&r,&self.execution_ref,evidence.clone()).await?;self.current(&r).await?;entry.validate(&r.bounds,&self.supported_extensions)?;let mut expected=entry.observation.clone();evidence.populate(&mut expected);
                if entry.provider_entry_key!=evidence.id||entry.capture.captured_url!=evidence.url()||entry.capture.access_partition!=r.subscription.partition.scope||entry.observation!=expected{return Err(invalid("pinterest.created_canonical"));}
                bytes=bytes.checked_add(serde_json::to_vec(&entry).map_err(|_|invalid("pinterest.created_page"))?.len()).ok_or_else(||invalid("pinterest.created_page"))?;if bytes>r.bounds.max_operation_bytes{return Err(Error::LimitExceeded {field:"pinterest.created_page".into()});}entries.push(entry);
            }
            let state=AcquisitionState::Partial {observed_at:time,reason_code:if entries.is_empty(){rss_contracts::TERMINAL_SOURCE_DOCUMENT}else{"pinterest_current_created_only"}.into()};
            let page=AcquisitionPage {subscription_ref:r.subscription.subscription_ref.clone(),source_ref:r.subscription.source_ref.clone(),partition:r.subscription.partition.clone(),executor_ref:r.subscription.executor_ref.clone(),generation:r.subscription.generation,operation_id:r.operation_id,entries,state,coverage:hsscrap::Coverage::Partial,continuation};page.validate(&r.subscription,&r.bounds,&self.supported_extensions)?;self.current(&r).await?;return Ok(page);
        }
    }
}
