//! Browser-session content through host-issued credential leases.
mod normalize;
mod traversal;
use crate::{invalid,CanonicalEntryPort,FetchResponse,ProviderLimits,public};
use rss_contracts::{AccessMode,Bounds,CaptureIntent,Error,HostRef,Pending,Provider,Revision,ScopeRef,Source,SourceRequest};
use serde::{Deserialize,Serialize};
use std::collections::BTreeSet;

#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedContext {
    pub connection_ref:HostRef,pub owner_scope:ScopeRef,pub viewer_id:String,pub viewer_sec_uid:String,
    pub lease_generation:Revision,pub capture_intent:CaptureIntent,
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub enum Endpoint {
    Following {cursor:String},ForYou,Profile {username:String},Posts {sec_uid:String,cursor:String},
}
impl Endpoint {
    pub fn url(&self)->rss_contracts::Result<String> {
        let (path,params):(&str,Vec<(&str,String)>)=match self {
            Self::Following {cursor}=>{cursor_value(cursor)?;("/api/following/item_list/",vec![("count","9".into()),("cursor",cursor.clone()),("pullType","1".into()),("level","1".into()),("isResetCounterUsed","true".into()),("from_page","following".into())])},
            Self::ForYou=>("/api/recommend/item_list/",vec![("count","16".into()),("pull_type","0".into()),("coverFormat","2".into()),("from_page","fyp".into())]),
            Self::Profile {username}=>{if !public::username(username){return Err(invalid("tiktok.username"));}return Ok(format!("https://www.tiktok.com/@{username}"));},
            Self::Posts {sec_uid,cursor}=>{cursor_value(cursor)?;if !public::sec_uid(sec_uid){return Err(invalid("tiktok.sec_uid"));}("/api/post/item_list/",vec![("count","30".into()),("secUid",sec_uid.clone()),("cursor",cursor.clone()),("from_page","user".into())])},
        };
        let mut url=url::Url::parse(&format!("https://www.tiktok.com{path}")).map_err(|_|invalid("tiktok.endpoint"))?;
        for (key,value) in params{url.query_pairs_mut().append_pair(key,&value);}Ok(url.into())
    }
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct ConnectedFetchRequest {
    pub endpoint:Endpoint,pub expected_digest:Option<String>,pub max_body_bytes:usize,pub timeout_millis:u64,
}
/// Attest exact current viewer, account/profile, connection, lease generation and
/// capture intent before every effect. No anonymous fallback. Retain original raw
/// request/body provenance and hold the host lease through canonical settlement.
/// Cookies, signing, device/browser parameters and expiry remain host-owned.
pub trait AuthorizedFetchPort:Send+Sync {
    fn current<'a>(&'a self,request:&'a SourceRequest,execution:&'a HostRef,authorization:&'a AuthorizedContext)->Pending<'a,()>;
    fn fetch<'a>(&'a self,request:&'a SourceRequest,execution:&'a HostRef,authorization:&'a AuthorizedContext,fetch:ConnectedFetchRequest)->Pending<'a,FetchResponse>;
}
pub struct ConnectedTikTokProvider<F,C> {
    pub source:Source,pub execution_ref:HostRef,pub limits:ProviderLimits,pub supported_extensions:BTreeSet<String>,
    pub authorization:AuthorizedContext,pub fetch:F,pub canonical:C,
}
impl<F:AuthorizedFetchPort,C:CanonicalEntryPort> rss_contracts::ProviderPort for ConnectedTikTokProvider<F,C> {
    fn capabilities(&self)->Vec<rss_contracts::Capability>{
        [AccessMode::Following,AccessMode::HomeForYou,AccessMode::OwnUploads,AccessMode::PrivateSource].into_iter().map(|mode|rss_contracts::Capability {
            provider:Provider::Tiktok,mode,status:rss_contracts::CapabilityStatus::Available,paginated:mode!=AccessMode::HomeForYou,
            media_metadata:true,adapter_version:"tiktok.connected.v1".into(),
        }).collect()
    }
    fn acquire<'a>(&'a self,request:SourceRequest)->Pending<'a,rss_contracts::AcquisitionPage>{Box::pin(self.acquire_page(request))}
}
impl<F:AuthorizedFetchPort,C:CanonicalEntryPort> ConnectedTikTokProvider<F,C> {
    fn validate(&self,r:&SourceRequest)->rss_contracts::Result<Option<String>> {
        let b=&r.bounds;b.validate()?;self.limits.validate()?;self.source.revision.validate()?;self.authorization.lease_generation.validate()?;
        r.subscription.generation.validate()?;r.subscription.partition.validate(b)?;
        self.source.common.validate_bounded(b,&self.supported_extensions)?;r.subscription.common.validate_bounded(b,&self.supported_extensions)?;
        let mode=r.subscription.partition.mode;
        if self.source.provider!=Provider::Tiktok||!matches!(mode,AccessMode::Following|AccessMode::HomeForYou|AccessMode::OwnUploads|AccessMode::PrivateSource){return Err(Error::Unsupported {provider:Provider::Tiktok,mode});}
        if self.source.paused||r.subscription.paused{return Err(Error::Cancelled);}
        let a=&self.authorization;
        if self.source.source_ref!=r.subscription.source_ref||self.source.owner_scope!=a.owner_scope||a.owner_scope.0.is_empty()
            ||r.subscription.partition.connection_ref.as_ref()!=Some(&a.connection_ref)||!public::numeric_id(&a.viewer_id)||!public::sec_uid(&a.viewer_sec_uid){return Err(Error::Denied);}
        match &a.capture_intent {CaptureIntent::FollowSource {original_url,provider,partition} if original_url==&self.source.original_url&&*provider==Provider::Tiktok&&partition==&r.subscription.partition=>{},_=>return Err(Error::Denied)}
        for reference in [&self.source.source_ref,&r.subscription.subscription_ref,&r.subscription.executor_ref,&self.execution_ref,&a.connection_ref]{
            for value in [&reference.host_instance_id,&reference.owner_kind,&reference.native_id]{if value.is_empty(){return Err(invalid("tiktok.reference"));}b.text("tiktok.reference",value,b.max_text_bytes)?;}
        }
        b.text("tiktok.owner_scope",&a.owner_scope.0,b.max_text_bytes)?;b.text("tiktok.viewer_sec_uid",&a.viewer_sec_uid,b.max_text_bytes)?;
        b.text("tiktok.source_url",&self.source.original_url,b.max_url_bytes)?;
        if serde_json::to_vec(&(&self.source,a,r)).map_err(|_|invalid("tiktok.request"))?.len()>b.max_operation_bytes{return Err(Error::LimitExceeded {field:"tiktok.request".into()});}
        if let Some(c)=&r.continuation{c.validate_for(&r.subscription,b)?;}
        if matches!(mode,AccessMode::OwnUploads|AccessMode::PrivateSource){
            let target=public::resolve_url(&self.source.original_url,b)?;
            if target.item.is_some(){return Err(Error::Unsupported {provider:Provider::Tiktok,mode});}return Ok(Some(target.name));
        }
        let url=url::Url::parse(&self.source.original_url).map_err(|_|invalid("tiktok.feed_url"))?;
        let route_ok=match mode {AccessMode::Following=>matches!(url.path(),"/following"|"/following/"),_=>matches!(url.path(),"/"|"/foryou"|"/foryou/")};
        if url.scheme()!="https"||!matches!(url.host_str(),Some("www.tiktok.com"|"tiktok.com"))||!url.username().is_empty()
            ||url.password().is_some()||url.port().is_some()||url.query().is_some()||url.fragment().is_some()
            ||!route_ok{return Err(invalid("tiktok.feed_url"));}
        Ok(None)
    }
}
fn cursor_value(value:&str)->rss_contracts::Result<u64>{
    if value.is_empty()||!value.bytes().all(|v|v.is_ascii_digit())||(value.len()>1&&value.starts_with('0')){return Err(invalid("tiktok.connected_cursor"));}
    value.parse().map_err(|_|invalid("tiktok.connected_cursor"))
}
fn digest(bytes:&[u8])->String{use sha2::{Digest,Sha256};format!("{:x}",Sha256::digest(bytes))}
