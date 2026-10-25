//! Connected content and channel import through host-owned credential leases.
mod normalize;
mod traversal;
use std::collections::BTreeSet;
use rss_contracts::{AccessMode, AcquisitionState, Bounds, CaptureIntent, Error, HostRef, Pending, Provider, Revision, ScopeRef, Source, SourceRequest};
use serde::{Deserialize, Serialize};
use crate::{invalid, CanonicalEntryPort, FetchResponse, ProviderLimits};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialKind { DataApiOAuth, BrowserSession }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedContext {
    pub connection_ref: HostRef,
    pub owner_scope: ScopeRef,
    pub external_identity: String,
    pub lease_generation: Revision,
    pub credential_kind: CredentialKind,
    pub capture_intent: CaptureIntent,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    DataApi { url: String },
    Browse { browse_id: Option<String>, continuation: Option<String> },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedFetchRequest {
    pub endpoint: Endpoint,
    pub expected_digest: Option<String>,
    pub max_body_bytes: usize,
    pub timeout_millis: u64,
}
/// Attest the exact current authenticated account, lease generation and capture
/// intent, including after retained-byte replay. Reject anonymous Home fallback.
/// Hold the host credential lease through canonical settlement. Supply bounded
/// raw responses; secrets, account headers, refresh and visitor state stay here.
pub trait ConnectedFetchPort: Send + Sync {
    fn current<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, authorization: &'a AuthorizedContext) -> Pending<'a, ()>;
    fn fetch<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, authorization: &'a AuthorizedContext, fetch: ConnectedFetchRequest) -> Pending<'a, FetchResponse>;
}
pub struct ConnectedYouTubeProvider<F,C> {
    pub source: Source,
    pub execution_ref: HostRef,
    pub limits: ProviderLimits,
    pub supported_extensions: BTreeSet<String>,
    pub authorization: AuthorizedContext,
    pub fetch: F,
    pub canonical: C,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelEvidence { pub channel_id: String, pub title: Option<String>, pub document_digest: String }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionPage {
    pub channels: Vec<ChannelEvidence>,
    pub continuation: Option<rss_contracts::Continuation>,
    pub state: AcquisitionState,
    pub coverage: hsscrap::Coverage,
}
impl<F:ConnectedFetchPort,C:CanonicalEntryPort> rss_contracts::ProviderPort for ConnectedYouTubeProvider<F,C> {
    fn capabilities(&self) -> Vec<rss_contracts::Capability> {
        [AccessMode::Following,AccessMode::HomeForYou,AccessMode::PrivateSource,AccessMode::OwnUploads,AccessMode::SubscriptionImport].into_iter().map(|mode| rss_contracts::Capability {
            provider:Provider::Youtube,mode,status:if supported(mode,self.authorization.credential_kind) {rss_contracts::CapabilityStatus::Available} else {rss_contracts::CapabilityStatus::Unsupported},
            paginated:true,media_metadata:mode!=AccessMode::SubscriptionImport,adapter_version:"youtube.connected.v1".into(),
        }).collect()
    }
    fn acquire<'a>(&'a self,request:SourceRequest)->Pending<'a,rss_contracts::AcquisitionPage> { Box::pin(self.acquire_page(request)) }
}
fn supported(mode:AccessMode,kind:CredentialKind)->bool {
    matches!((mode,kind),(AccessMode::Following|AccessMode::HomeForYou,CredentialKind::BrowserSession)
        |(AccessMode::PrivateSource|AccessMode::OwnUploads|AccessMode::SubscriptionImport,CredentialKind::DataApiOAuth))
}
impl<F:ConnectedFetchPort,C:CanonicalEntryPort> ConnectedYouTubeProvider<F,C> {
    fn validate(&self,request:&SourceRequest)->rss_contracts::Result<()> {
        let b=&request.bounds;b.validate()?;self.limits.validate()?;
        self.source.revision.validate()?;self.authorization.lease_generation.validate()?;request.subscription.generation.validate()?;
        self.source.common.validate_bounded(b,&self.supported_extensions)?;request.subscription.common.validate_bounded(b,&self.supported_extensions)?;
        request.subscription.partition.validate(b)?;
        if self.source.provider!=Provider::Youtube||!supported(request.subscription.partition.mode,self.authorization.credential_kind) {return Err(Error::Unsupported {provider:Provider::Youtube,mode:request.subscription.partition.mode});}
        if self.source.paused||request.subscription.paused {return Err(Error::Cancelled);}
        if self.source.source_ref!=request.subscription.source_ref||self.source.owner_scope!=self.authorization.owner_scope
            ||request.subscription.partition.connection_ref.as_ref()!=Some(&self.authorization.connection_ref)||!crate::public::channel_id(&self.authorization.external_identity) {return Err(Error::Denied);}
        match &self.authorization.capture_intent {
            CaptureIntent::FollowSource {original_url,provider,partition} if original_url==&self.source.original_url&&*provider==Provider::Youtube&&partition==&request.subscription.partition=>{},
            CaptureIntent::SaveUrl {original_url} if request.subscription.partition.mode==AccessMode::PrivateSource&&original_url==&self.source.original_url=>{},
            _=>return Err(Error::Denied),
        }
        for r in [&self.source.source_ref,&request.subscription.subscription_ref,&request.subscription.executor_ref,&self.execution_ref,&self.authorization.connection_ref] {
            for v in [&r.host_instance_id,&r.owner_kind,&r.native_id] {if v.is_empty(){return Err(invalid("youtube.connected_reference"));}b.text("youtube.connected_reference",v,b.max_text_bytes)?;}
        }
        if self.authorization.owner_scope.0.is_empty(){return Err(Error::Denied);}
        b.text("youtube.source_url",&self.source.original_url,b.max_url_bytes)?;
        if serde_json::to_vec(&(&self.authorization,&self.source,request)).map_err(|_|invalid("youtube.connected_request"))?.len()>b.max_operation_bytes {return Err(Error::LimitExceeded {field:"youtube.connected_request".into()});}
        if let Some(next)=&request.continuation {next.validate_for(&request.subscription,b)?;}
        Ok(())
    }
}
fn digest(bytes:&[u8])->String {use sha2::{Digest,Sha256};format!("{:x}",Sha256::digest(bytes))}
fn bounded_token(value:&str,b:&Bounds)->rss_contracts::Result<()> {if value.is_empty()||value.chars().any(char::is_control){return Err(invalid("youtube.connected_token"));}b.text("youtube.connected_token",value,b.max_text_bytes)?;Ok(())}
