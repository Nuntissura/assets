//! Public YouTube source evidence through injected host transport and canonical owners.
pub mod normalize;
pub mod public;
use std::collections::BTreeSet;
use rss_contracts::{Bounds,HostRef,Instant,ObservedEntry,Pending,SourceRequest};
use serde::{Deserialize,Serialize};

#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
pub enum Strategy { PublicApi, NoLoginAtom }
#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
pub struct Preview { pub url:String,pub width:Option<u32>,pub height:Option<u32> }
#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
pub struct EntryEvidence {
    pub video_id:String,pub channel_id:String,pub title:Option<String>,pub description:Option<String>,pub author:Option<String>,
    pub published_at:Option<hsscrap::PublicationTime>,pub previews:Vec<Preview>,pub document_digest:String,
}
impl EntryEvidence {
    pub fn url(&self) -> String { format!("https://www.youtube.com/watch?v={}",self.video_id) }
    pub fn embed_url(&self) -> String { format!("https://www.youtube.com/embed/{}",self.video_id) }
    pub fn reliable_provider_identity(&self) -> String { format!("youtube/video/{}",self.video_id) }
    pub fn populate(&self,observation:&mut hsscrap::MetadataObservation) {
        observation.provider=Some("youtube".into()); observation.host_site="www.youtube.com".into();
        observation.title=self.title.clone(); observation.description=self.description.clone(); observation.author=self.author.clone();
        observation.published_at=self.published_at.clone(); observation.provider_labels.clear(); observation.canonical_url_assertion=Some(self.url());
        observation.source_payload_digest=Some(self.document_digest.clone());
        if let Some(gallery)=observation.gallery_snapshot.as_mut() { gallery.coverage=hsscrap::Coverage::Partial; gallery.full_set_digest=None; }
    }
    pub fn validate(&self,bounds:&Bounds) -> rss_contracts::Result<()> {
        if !public::video_id(&self.video_id) || !public::channel_id(&self.channel_id) || self.document_digest.len()!=64
            || !self.document_digest.bytes().all(|v|v.is_ascii_hexdigit()) { return Err(invalid("youtube.entry_identity")); }
        for value in self.title.iter().chain(self.description.iter()).chain(self.author.iter()) { bounds.text("youtube.metadata",value,bounds.max_text_bytes)?; }
        if let Some(date)=&self.published_at { date.validate()?; }
        if self.previews.len()>bounds.max_collection_items { return Err(invalid("youtube.previews")); }
        for preview in &self.previews {
            public::preview_url(&preview.url,bounds)?;
            if preview.width==Some(0)||preview.height==Some(0) { return Err(invalid("youtube.preview_dimensions")); }
        }
        if serde_json::to_vec(self).map_err(|_|invalid("youtube.entry"))?.len()>bounds.max_operation_bytes {
            return Err(rss_contracts::Error::LimitExceeded {field:"youtube.entry".into()});
        }
        Ok(())
    }
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct FetchRequest { pub url:String,pub strategy:Strategy,pub expected_digest:Option<String>,pub max_body_bytes:usize,pub timeout_millis:u64 }
pub struct FetchResponse { pub final_url:String,pub status:u16,pub body:Vec<u8>,pub cached_body:Option<Vec<u8>>,pub observed_at:Instant }
/// Enforce address/deadline/byte limits and current source authority. PublicApi keys stay in host headers.
/// NoLoginAtom must not borrow a connected account. Retain exact immutable responses for digest resumes.
pub trait FetchPort:Send+Sync {
    fn fetch<'a>(&'a self,request:&'a SourceRequest,execution:&'a HostRef,fetch:FetchRequest)->Pending<'a,FetchResponse>;
}
/// Resolve youtube/video/<id> through canonical identity ports and prepare through existing rss-acquisition.
/// The host allocates every canonical ID, merges Partial media references and retains exact operation intent.
pub trait CanonicalEntryPort:Send+Sync {
    fn prepare<'a>(&'a self,request:&'a SourceRequest,execution:&'a HostRef,evidence:EntryEvidence)->Pending<'a,ObservedEntry>;
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct ProviderLimits { pub max_body_bytes:usize,pub max_document_entries:usize,pub max_documents:usize,pub max_fetches_per_operation:usize,pub timeout_millis:u64 }
impl ProviderLimits {
    fn validate(&self)->rss_contracts::Result<()> {
        if self.max_body_bytes==0||self.max_body_bytes>16_000_000||self.max_document_entries==0||self.max_document_entries>1000
            ||self.max_documents==0||self.max_documents>1024||self.max_fetches_per_operation<2||self.max_fetches_per_operation>32
            ||self.timeout_millis==0||self.timeout_millis>300_000 { return Err(invalid("youtube.limits")); } Ok(())
    }
}
pub struct YouTubeProvider<F,C> { pub source:rss_contracts::Source,pub execution_ref:HostRef,pub strategy:Strategy,pub limits:ProviderLimits,
    pub supported_extensions:BTreeSet<String>,pub fetch:F,pub canonical:C }
impl<F:FetchPort,C:CanonicalEntryPort> rss_contracts::ProviderPort for YouTubeProvider<F,C> {
    fn capabilities(&self)->Vec<rss_contracts::Capability> { vec![rss_contracts::Capability {provider:rss_contracts::Provider::Youtube,
        mode:rss_contracts::AccessMode::PublicChannel,status:rss_contracts::CapabilityStatus::Available,paginated:self.strategy==Strategy::PublicApi,
        media_metadata:true,adapter_version:"youtube.public.v1".into()}] }
    fn acquire<'a>(&'a self,request:SourceRequest)->Pending<'a,rss_contracts::AcquisitionPage> { Box::pin(self.acquire_page(request)) }
}
pub(crate) fn invalid(field:&str)->rss_contracts::Error { rss_contracts::Error::Invalid {field:field.into()} }
