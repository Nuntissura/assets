//! OAuth selected-board acquisition; credentials and grants remain host-owned.
mod normalize;
mod traversal;
pub mod home;
pub mod created;
use crate::{CanonicalEntryPort,FetchRequest,FetchResponse,ProviderLimits,invalid,public};
use rss_contracts::{AccessMode,Bounds,CaptureIntent,Error,HostRef,Pending,Provider,Revision,ScopeRef,Source,SourceRequest};
use serde::{Deserialize,Serialize};
use std::collections::BTreeSet;

#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardBinding {pub board_id:String,pub source_url:String}
#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedContext {
    pub connection_ref:HostRef,pub owner_scope:ScopeRef,pub external_identity:String,
    pub lease_generation:Revision,pub capture_intent:CaptureIntent,
    pub selected_board:BoardBinding,pub granted_scopes:BTreeSet<String>,
}
/// Attest the exact current OAuth principal, account, lease attempt, scopes and
/// numeric board↔source URL binding. Hold the host lease through settlement;
/// secrets, refresh, retained raw responses and revocation stay host-owned.
pub trait AuthorizedFetchPort:Send+Sync {
    fn current<'a>(&'a self,request:&'a SourceRequest,execution:&'a HostRef,authorization:&'a AuthorizedContext)->Pending<'a,()>;
    fn fetch<'a>(&'a self,request:&'a SourceRequest,execution:&'a HostRef,authorization:&'a AuthorizedContext,fetch:FetchRequest)->Pending<'a,FetchResponse>;
}
pub struct AuthorizedPinterestProvider<F,C> {
    pub source:Source,pub execution_ref:HostRef,pub limits:ProviderLimits,pub supported_extensions:BTreeSet<String>,
    pub authorization:AuthorizedContext,pub fetch:F,pub canonical:C,
}
impl<F:AuthorizedFetchPort,C:CanonicalEntryPort> rss_contracts::ProviderPort for AuthorizedPinterestProvider<F,C> {
    fn capabilities(&self)->Vec<rss_contracts::Capability> {
        [AccessMode::PublicBoard,AccessMode::PrivateSource,AccessMode::HomeForYou,AccessMode::Following,AccessMode::OwnUploads,AccessMode::SubscriptionImport].into_iter().map(|mode|rss_contracts::Capability {
            provider:Provider::Pinterest,mode,status:if matches!(mode,AccessMode::PublicBoard|AccessMode::PrivateSource){rss_contracts::CapabilityStatus::Available}else{rss_contracts::CapabilityStatus::Unsupported},
            paginated:true,media_metadata:true,adapter_version:"pinterest.authorized-board.v1".into(),
        }).collect()
    }
    fn acquire<'a>(&'a self,request:SourceRequest)->Pending<'a,rss_contracts::AcquisitionPage>{Box::pin(self.acquire_page(request))}
}
impl<F:AuthorizedFetchPort,C:CanonicalEntryPort> AuthorizedPinterestProvider<F,C> {
    fn validate(&self,r:&SourceRequest)->rss_contracts::Result<()> {
        let b=&r.bounds;b.validate()?;self.limits.validate()?;self.source.revision.validate()?;self.authorization.lease_generation.validate()?;
        r.subscription.generation.validate()?;r.subscription.partition.validate(b)?;
        self.source.common.validate_bounded(b,&self.supported_extensions)?;r.subscription.common.validate_bounded(b,&self.supported_extensions)?;
        if self.source.provider!=Provider::Pinterest||!matches!(r.subscription.partition.mode,AccessMode::PublicBoard|AccessMode::PrivateSource){return Err(Error::Unsupported {provider:Provider::Pinterest,mode:r.subscription.partition.mode});}
        if self.source.paused||r.subscription.paused{return Err(Error::Cancelled);}
        let a=&self.authorization;
        if self.source.source_ref!=r.subscription.source_ref||self.source.owner_scope!=a.owner_scope||a.owner_scope.0.is_empty()
            ||r.subscription.partition.connection_ref.as_ref()!=Some(&a.connection_ref)||!public::numeric_id(&a.external_identity)||!public::numeric_id(&a.selected_board.board_id){return Err(Error::Denied);}
        let target=public::resolve_url(&self.source.original_url,b)?;
        if !matches!(target,public::SourceTarget::Board {..})||public::canonical_source(&target)?!=a.selected_board.source_url{return Err(Error::Denied);}
        match &a.capture_intent {CaptureIntent::FollowSource {original_url,provider,partition} if original_url==&self.source.original_url&&*provider==Provider::Pinterest&&partition==&r.subscription.partition=>{},_=>return Err(Error::Denied)}
        for scope in ["user_accounts:read","boards:read","pins:read"]{if !a.granted_scopes.contains(scope){return Err(Error::Denied);}}
        if a.granted_scopes.len()>b.max_collection_items{return Err(invalid("pinterest.scopes"));}
        for reference in [&self.source.source_ref,&r.subscription.subscription_ref,&r.subscription.executor_ref,&self.execution_ref,&a.connection_ref] {
            for v in [&reference.host_instance_id,&reference.owner_kind,&reference.native_id]{if v.is_empty(){return Err(invalid("pinterest.reference"));}b.text("pinterest.reference",v,b.max_text_bytes)?;}
        }
        if serde_json::to_vec(&(&self.source,a,r)).map_err(|_|invalid("pinterest.request"))?.len()>b.max_operation_bytes{return Err(Error::LimitExceeded {field:"pinterest.request".into()});}
        if let Some(c)=&r.continuation{c.validate_for(&r.subscription,b)?;}Ok(())
    }
}
fn digest(bytes:&[u8])->String{use sha2::{Digest,Sha256};format!("{:x}",Sha256::digest(bytes))}
fn token(value:&str,b:&Bounds)->rss_contracts::Result<()> {if value.is_empty()||value.chars().any(char::is_control){return Err(invalid("pinterest.bookmark"));}b.text("pinterest.bookmark",value,b.max_text_bytes)?;Ok(())}
