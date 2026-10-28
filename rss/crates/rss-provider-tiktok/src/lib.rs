//! Public TikTok creator-window and item evidence. Network/browser effects and canonical IDs stay host-owned.
pub mod normalize;
pub mod public;
pub mod connected;
use std::collections::BTreeSet;
use rss_contracts::{Bounds,HostRef,Instant,ObservedEntry,Pending,SourceRequest};
use serde::{Deserialize,Serialize};

#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
pub struct MediaEvidence {pub provider_member_id:Option<String>,pub kind:hsscrap::MediaKind,pub url:String,pub preview_url:Option<String>,
    pub width:Option<u32>,pub height:Option<u32>,pub duration_ms:Option<u64>,pub expires_at_unix:Option<u64>}
#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
pub struct EntryEvidence {pub id:String,pub author_id:String,pub author_sec_uid:String,pub author:String,pub description:String,
    pub published_at:Option<hsscrap::PublicationTime>,pub create_time_seconds:Option<u64>,pub media:Vec<MediaEvidence>,pub photo:bool,pub document_digest:String}
impl EntryEvidence {
    pub fn url(&self)->String {format!("https://www.tiktok.com/@{}/{}/{}",self.author,if self.photo {"photo"} else {"video"},self.id)}
    pub fn reliable_provider_identity(&self)->String {format!("tiktok/item/{}",self.id)}
    pub fn populate(&self,observation:&mut hsscrap::MetadataObservation) {
        observation.provider=Some("tiktok".into());observation.host_site="www.tiktok.com".into();observation.title=None;
        observation.description=Some(self.description.clone());observation.author=Some(self.author.clone());observation.published_at=self.published_at.clone();
        observation.provider_labels.clear();observation.canonical_url_assertion=Some(self.url());observation.source_payload_digest=Some(self.document_digest.clone());
        if let Some(gallery)=observation.gallery_snapshot.as_mut() {gallery.coverage=hsscrap::Coverage::Partial;gallery.full_set_digest=None;}
    }
    pub fn validate(&self,bounds:&Bounds)->rss_contracts::Result<()> {
        if !public::numeric_id(&self.id)||!public::numeric_id(&self.author_id)||!public::username(&self.author)||!public::sec_uid(&self.author_sec_uid)
            ||self.document_digest.len()!=64||!self.document_digest.bytes().all(|v|v.is_ascii_hexdigit()) {return Err(invalid("tiktok.entry_identity"));}
        bounds.text("tiktok.description",&self.description,bounds.max_text_bytes)?;bounds.text("tiktok.author",&self.author_sec_uid,bounds.max_text_bytes)?;
        if let Some(date)=&self.published_at {date.validate()?;}
        if self.media.is_empty()||self.media.len()>bounds.max_collection_items {return Err(invalid("tiktok.media"));}
        let mut member_ids=BTreeSet::new();
        for media in &self.media {
            public::media_url(&media.url,bounds)?;if let Some(url)=&media.preview_url {public::media_url(url,bounds)?;}
            if media.width==Some(0)||media.height==Some(0) {return Err(invalid("tiktok.dimensions"));}
            if let Some(id)=&media.provider_member_id {if id.is_empty()||!member_ids.insert(id) {return Err(invalid("tiktok.member_id"));}bounds.text("tiktok.member_id",id,bounds.max_text_bytes)?;}
        }
        if serde_json::to_vec(self).map_err(|_|invalid("tiktok.entry"))?.len()>bounds.max_operation_bytes {return Err(rss_contracts::Error::LimitExceeded {field:"tiktok.entry".into()});}Ok(())
    }
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub enum FetchKind {Profile,CreatorWindow,Item}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct FetchRequest {pub kind:FetchKind,pub url:String,pub expected_digest:Option<String>,pub max_body_bytes:usize,pub timeout_millis:u64}
pub struct FetchResponse {pub final_url:String,pub status:u16,pub body:Vec<u8>,pub cached_body:Option<Vec<u8>>,pub observed_at:Instant}
/// Supply actual authorized public transport/browser context, bounded bytes/deadline and retained raw snapshots.
/// Do not attach connected-user credentials, invent a device identity or replace the Rust parser.
pub trait PublicFetchPort:Send+Sync {fn fetch<'a>(&'a self,request:&'a SourceRequest,execution:&'a HostRef,fetch:FetchRequest)->Pending<'a,FetchResponse>;}
/// Resolve global tiktok/item/<id>, prepare through existing canonical owners and retain exact operation intent.
/// Ordered positions are evidence; host owners allocate stable media IDs and merge Partial gallery observations.
pub trait CanonicalEntryPort:Send+Sync {fn prepare<'a>(&'a self,request:&'a SourceRequest,execution:&'a HostRef,evidence:EntryEvidence)->Pending<'a,ObservedEntry>;}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct ProviderLimits {pub max_body_bytes:usize,pub max_document_entries:usize,pub max_documents:usize,pub max_fetches_per_operation:usize,pub timeout_millis:u64}
impl ProviderLimits {fn validate(&self)->rss_contracts::Result<()> {
    if self.max_body_bytes==0||self.max_body_bytes>16_000_000||self.max_document_entries==0||self.max_document_entries>1000||self.max_documents==0||self.max_documents>1024
        ||self.max_fetches_per_operation<2||self.max_fetches_per_operation>1024||self.timeout_millis==0||self.timeout_millis>300_000 {return Err(invalid("tiktok.limits"));}Ok(())}}
pub struct TikTokProvider<F,C> {pub source:rss_contracts::Source,pub execution_ref:HostRef,pub initial_cursor_millis:u64,pub limits:ProviderLimits,
    pub supported_extensions:BTreeSet<String>,pub fetch:F,pub canonical:C}
pub fn capabilities()->Vec<rss_contracts::Capability> {vec![rss_contracts::Capability {provider:rss_contracts::Provider::Tiktok,mode:rss_contracts::AccessMode::PublicProfile,
    status:rss_contracts::CapabilityStatus::Available,paginated:true,media_metadata:true,adapter_version:"tiktok.public.v1".into()}]}
impl<F:PublicFetchPort,C:CanonicalEntryPort> rss_contracts::ProviderPort for TikTokProvider<F,C> {
    fn capabilities(&self)->Vec<rss_contracts::Capability> { capabilities() }
    fn acquire<'a>(&'a self,request:SourceRequest)->Pending<'a,rss_contracts::AcquisitionPage> {Box::pin(self.acquire_page(request))}
}
pub(crate) fn invalid(field:&str)->rss_contracts::Error {rss_contracts::Error::Invalid {field:field.into()}}
