use std::collections::BTreeSet;
use rss_contracts::{AcquisitionPage,AcquisitionState,Bounds,Continuation,Error,Result,SourceRequest};
use serde::{Deserialize,Serialize};
use scraper::{Html,Selector};
use url::Url;
use crate::{CanonicalEntryPort,FetchPort,FetchRequest,FetchResponse,Strategy,YouTubeProvider,invalid,normalize};

fn token(value:&str,min:usize,max:usize)->bool {value.len()>=min&&value.len()<=max&&value.bytes().all(|v|v.is_ascii_alphanumeric()||v==b'_'||v==b'-')}
pub fn video_id(value:&str)->bool {token(value,11,11)}
pub fn channel_id(value:&str)->bool {value.starts_with("UC")&&token(value,24,24)}
pub fn playlist_id(value:&str)->bool {token(value,10,128)}
pub fn preview_url(value:&str,bounds:&Bounds)->Result<()> {
    bounds.text("youtube.preview_url",value,bounds.max_url_bytes)?;let url=Url::parse(value).map_err(|_|invalid("youtube.preview_url"))?;
    if url.scheme()!="https"||!url.username().is_empty()||url.password().is_some()||url.port().is_some()||url.fragment().is_some()
        ||!matches!(url.host(),Some(url::Host::Domain(host)) if host=="i.ytimg.com"||host.ends_with(".ytimg.com")) {return Err(invalid("youtube.preview_url"));}Ok(())
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub enum SourceTarget { Channel(String),Handle(String),Video(String) }
pub fn resolve_url(raw:&str,bounds:&Bounds)->Result<SourceTarget> {
    bounds.text("youtube.source_url",raw,bounds.max_url_bytes)?;let url=Url::parse(raw).map_err(|_|invalid("youtube.source_url"))?;
    if url.scheme()!="https"||!matches!(url.host_str(),Some("youtube.com"|"www.youtube.com"|"youtu.be"))
        ||!url.username().is_empty()||url.password().is_some()||url.port().is_some()||url.fragment().is_some() {return Err(invalid("youtube.source_url"));}
    let path=url.path().trim_matches('/');
    if url.host_str()==Some("youtu.be") {if url.query().is_none()&&video_id(path) {return Ok(SourceTarget::Video(path.into()));}return Err(invalid("youtube.video_url"));}
    let params:Vec<_>=url.query_pairs().collect();
    if path=="watch"&&params.len()==1&&params[0].0=="v"&&video_id(&params[0].1) {return Ok(SourceTarget::Video(params[0].1.to_string()));}
    if path=="feeds/videos.xml"&&params.len()==1&&params[0].0=="channel_id"&&channel_id(&params[0].1) {return Ok(SourceTarget::Channel(params[0].1.to_string()));}
    if url.query().is_some() {return Err(invalid("youtube.source_query"));}
    let parts:Vec<_>=path.split('/').collect();
    if parts.len()==2&&parts[0]=="channel"&&channel_id(parts[1]) {return Ok(SourceTarget::Channel(parts[1].into()));}
    if parts.len()==2&&matches!(parts[0],"shorts"|"embed"|"live")&&video_id(parts[1]) {return Ok(SourceTarget::Video(parts[1].into()));}
    let name=percent_encoding::percent_decode_str(path).decode_utf8().map_err(|_|invalid("youtube.handle"))?;
    if let Some(handle)=name.strip_prefix('@') {
        if !handle.is_empty()&&handle.chars().count()<=100&&handle.chars().all(|c|c.is_alphanumeric()||matches!(c,'_'|'-'|'.')) {return Ok(SourceTarget::Handle(handle.into()));}
    }
    Err(invalid("youtube.source_target"))
}
fn source_key(source:&SourceTarget)->String {match source {SourceTarget::Channel(id)=>format!("channel/{id}"),SourceTarget::Handle(handle)=>format!("handle/{handle}"),SourceTarget::Video(id)=>format!("video/{id}")}}
fn canonical_html(bytes:&[u8],bounds:&Bounds)->Result<String> {
    let html=std::str::from_utf8(bytes).map_err(|_|invalid("youtube.channel_html"))?;
    let document=Html::parse_document(html);let selector=Selector::parse("link[rel=canonical][href]").map_err(|_|invalid("youtube.selector"))?;
    let mut channels=BTreeSet::new();
    for link in document.select(&selector) {
        if let Some(raw)=link.value().attr("href") {if let Ok(SourceTarget::Channel(id))=resolve_url(raw,bounds) {channels.insert(id);}}
    }
    if channels.len()!=1 {return Err(invalid("youtube.channel_unresolved"));}channels.into_iter().next().ok_or_else(||invalid("youtube.channel_unresolved"))
}
fn api(resource:&str,params:&[(&str,&str)])->Result<String> {
    let mut url=Url::parse(&format!("https://www.googleapis.com/youtube/v3/{resource}")).map_err(|_|invalid("youtube.endpoint"))?;
    for (key,value) in params {url.query_pairs_mut().append_pair(key,value);}Ok(url.to_string())
}
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {version:u8,strategy:Strategy,source_key:String,source_revision:hsscrap::Revision,channel:String,uploads:Option<String>,
    page:Option<String>,digest:Option<String>,offset:usize,documents:usize,visited:Vec<Option<String>>,seen:BTreeSet<String>}
fn validate_request(request:&SourceRequest,execution:&hsscrap::HostRef,supported:&BTreeSet<String>)->Result<()> {
    request.bounds.validate()?;request.subscription.partition.validate(&request.bounds)?;request.subscription.generation.validate()?;
    request.subscription.common.validate_bounded(&request.bounds,supported)?;
    for reference in [&request.subscription.subscription_ref,&request.subscription.source_ref,&request.subscription.executor_ref,execution] {
        for value in [&reference.host_instance_id,&reference.owner_kind,&reference.native_id] {
            if value.is_empty() {return Err(invalid("youtube.request_reference"));}request.bounds.text("youtube.request_reference",value,request.bounds.max_text_bytes)?;
        }
    }
    if let Some(next)=&request.continuation {next.validate_for(&request.subscription,&request.bounds)?;}
    if serde_json::to_vec(request).map_err(|_|invalid("youtube.request"))?.len()>request.bounds.max_operation_bytes {return Err(Error::LimitExceeded {field:"youtube.request".into()});}Ok(())
}
impl<F:FetchPort,C:CanonicalEntryPort> YouTubeProvider<F,C> {
    async fn get(&self,request:&SourceRequest,url:String,digest:Option<String>,fetches:&mut usize)->Result<(Vec<u8>,hsscrap::Instant)> {
        if *fetches==self.limits.max_fetches_per_operation {return Err(Error::LimitExceeded {field:"youtube.fetches".into()});}*fetches+=1;
        let expected=FetchRequest {url,strategy:self.strategy,expected_digest:digest,max_body_bytes:self.limits.max_body_bytes,timeout_millis:self.limits.timeout_millis};
        let response=self.fetch.fetch(request,&self.execution_ref,expected.clone()).await?;
        if response.final_url!=expected.url {return Err(Error::StaleResult);}
        let FetchResponse {status,body,cached_body,observed_at,..}=response;
        let bytes=match status {
            200 if cached_body.is_none()=>body,
            304 if body.is_empty()=>cached_body.ok_or_else(||invalid("youtube.missing_cached_body"))?,
            401=>return Err(Error::Denied),410=>return Err(Error::Expired),429=>return Err(Error::RateLimited {retry_after:None}),404=>return Err(Error::NotFound),
            403=>{
                let value=normalize::json(&body,self.limits.max_body_bytes).ok();
                let quota=value.as_ref().and_then(|v|v["error"]["errors"].as_array()).is_some_and(|errors|errors.iter().any(|v|matches!(v["reason"].as_str(),Some("quotaExceeded"|"dailyLimitExceeded"|"rateLimitExceeded"|"userRateLimitExceeded"))));
                return Err(if quota {Error::RateLimited {retry_after:None}} else {Error::Denied});
            }
            _=>return Err(Error::Network),
        };
        if bytes.len()>self.limits.max_body_bytes {return Err(Error::LimitExceeded {field:"youtube.body".into()});}Ok((bytes,observed_at))
    }
    pub(crate) async fn acquire_page(&self,request:SourceRequest)->Result<AcquisitionPage> {
        self.limits.validate()?;validate_request(&request,&self.execution_ref,&self.supported_extensions)?;
        self.source.revision.validate()?;self.source.common.validate_bounded(&request.bounds,&self.supported_extensions)?;
        request.bounds.text("source.owner_scope", &self.source.owner_scope.0, request.bounds.max_text_bytes)?;
        if self.source.source_ref!=request.subscription.source_ref||self.source.owner_scope.0.is_empty() {return Err(Error::Denied);}
        if self.source.provider!=rss_contracts::Provider::Youtube||request.subscription.partition.mode!=rss_contracts::AccessMode::PublicChannel {
            return Err(Error::Unsupported {provider:rss_contracts::Provider::Youtube,mode:request.subscription.partition.mode});
        }
        if self.source.paused||request.subscription.paused {return Err(Error::Cancelled);}
        let target=resolve_url(&self.source.original_url,&request.bounds)?;let key=source_key(&target);
        let mut cursor=if let Some(next)=&request.continuation {serde_json::from_str::<Cursor>(&next.token).map_err(|_|invalid("youtube.cursor"))?}
            else {Cursor {version:1,strategy:self.strategy,source_key:key.clone(),source_revision:self.source.revision,channel:String::new(),uploads:None,
                page:None,digest:None,offset:0,documents:0,visited:vec![],seen:BTreeSet::new()}};
        if cursor.version!=1||cursor.strategy!=self.strategy||cursor.source_key!=key||cursor.source_revision!=self.source.revision
            ||cursor.documents>=self.limits.max_documents||cursor.visited.len()!=cursor.documents
            ||cursor.visited.iter().collect::<BTreeSet<_>>().len()!=cursor.visited.len()||cursor.visited.contains(&cursor.page)
            ||(cursor.offset>0)!=cursor.digest.is_some()||cursor.offset>self.limits.max_document_entries
            ||cursor.seen.iter().any(|id|!video_id(id))||cursor.seen.len()>self.limits.max_documents.saturating_mul(self.limits.max_document_entries)
            ||cursor.digest.as_ref().is_some_and(|digest|digest.len()!=64||!digest.bytes().all(|v|v.is_ascii_hexdigit())) {return Err(invalid("youtube.cursor"));}
        let mut fetches=0;
        let resolved=match (&self.strategy,&target) {
            (Strategy::PublicApi,SourceTarget::Video(_))=>None,
            (Strategy::PublicApi,source)=>{
                let filter=match source {SourceTarget::Channel(id)=>("id",id.as_str()),SourceTarget::Handle(handle)=>("forHandle",handle.as_str()),_=>return Err(invalid("youtube.source"))};
                let (bytes,_)=self.get(&request,api("channels",&[("part","contentDetails"),filter])?,None,&mut fetches).await?;
                Some(normalize::channel(&bytes,if let SourceTarget::Channel(id)=source {Some(id.as_str())} else {None},self.limits.max_body_bytes)?)
            }
            (Strategy::NoLoginAtom,SourceTarget::Channel(id))=>Some(normalize::Channel {id:id.clone(),uploads:String::new()}),
            (Strategy::NoLoginAtom,SourceTarget::Handle(handle))=>{
                let mut url=Url::parse("https://www.youtube.com").map_err(|_|invalid("youtube.endpoint"))?;url.set_path(&format!("/@{handle}"));
                let (bytes,_)=self.get(&request,url.to_string(),None,&mut fetches).await?;
                Some(normalize::Channel {id:canonical_html(&bytes,&request.bounds)?,uploads:String::new()})
            }
            _=>return Err(invalid("youtube.atom_requires_channel")),
        };
        if let Some(channel)=&resolved {
            let uploads=(!channel.uploads.is_empty()).then(||channel.uploads.clone());
            if !cursor.channel.is_empty()&&(cursor.channel!=channel.id||cursor.uploads!=uploads) {return Err(Error::StaleResult);}
            cursor.channel=channel.id.clone();cursor.uploads=uploads;
        }
        loop {
            let (document,observed_at)=match (&self.strategy,&target) {
                (Strategy::PublicApi,SourceTarget::Video(id))=>{
                    if request.continuation.is_some() {return Err(invalid("youtube.video_continuation"));}
                    let (bytes,time)=self.get(&request,api("videos",&[("part","snippet,status"),("id",id)])?,None,&mut fetches).await?;
                    (normalize::video(&bytes,id,self.limits.max_body_bytes,&request.bounds)?,time)
                }
                (Strategy::PublicApi,_)=>{
                    let channel=resolved.as_ref().ok_or_else(||invalid("youtube.channel"))?;
                    let mut params=vec![("part","snippet,contentDetails,status"),("playlistId",channel.uploads.as_str()),("maxResults","50")];
                    if let Some(token)=&cursor.page {params.push(("pageToken",token));}
                    let (bytes,time)=self.get(&request,api("playlistItems",&params)?,cursor.digest.clone(),&mut fetches).await?;
                    (normalize::playlist(&bytes,channel,self.limits.max_body_bytes,self.limits.max_document_entries,&request.bounds)?,time)
                }
                (Strategy::NoLoginAtom,_)=>{
                    if cursor.page.is_some()||cursor.documents>0 {return Err(invalid("youtube.atom_continuation"));}
                    let channel=resolved.as_ref().ok_or_else(||invalid("youtube.channel"))?;
                    let url=format!("https://www.youtube.com/feeds/videos.xml?channel_id={}",channel.id);
                    let (bytes,time)=self.get(&request,url,cursor.digest.clone(),&mut fetches).await?;
                    (normalize::atom(&bytes,&channel.id,self.limits.max_body_bytes,self.limits.max_document_entries,&request.bounds)?,time)
                }
            };
            if cursor.digest.as_ref().is_some_and(|digest|digest!=&document.digest) {return Err(Error::StaleResult);}
            if cursor.offset>0&&cursor.offset>=document.entries.len() {return Err(invalid("youtube.cursor_offset"));}
            if document.next.as_ref().is_some_and(|next|cursor.page.as_ref()==Some(next)||cursor.visited.contains(&Some(next.clone()))) {return Err(invalid("youtube.cursor_cycle"));}
            let mut next=cursor.clone();let mut selected=vec![];let mut end=cursor.offset;
            while end<document.entries.len()&&selected.len()<request.bounds.max_page_items {
                let evidence=&document.entries[end];if next.seen.insert(evidence.video_id.clone()) {selected.push(evidence.clone());}end+=1;
            }
            let next=if end<document.entries.len() {next.offset=end;next.digest=Some(document.digest.clone());Some(next)}
                else if let Some(token)=document.next {
                    next.visited.push(cursor.page.clone());next.documents+=1;
                    if next.documents>=self.limits.max_documents {return Err(Error::LimitExceeded {field:"youtube.documents".into()});}
                    next.page=Some(token);next.offset=0;next.digest=None;Some(next)
                } else {None};
            let continuation=next.as_ref().map(|next|->Result<_> {
                let continuation=Continuation {source_ref:request.subscription.source_ref.clone(),subscription_ref:request.subscription.subscription_ref.clone(),
                    partition:request.subscription.partition.clone(),executor_ref:request.subscription.executor_ref.clone(),generation:request.subscription.generation,
                    token:serde_json::to_string(next).map_err(|_|invalid("youtube.cursor"))?};continuation.validate_for(&request.subscription,&request.bounds)?;Ok(continuation)
            }).transpose()?;
            if selected.is_empty() {
                if let Some(next)=next {cursor=next;continue;}
                if request.continuation.is_none()&&!document.verified_empty {return Err(invalid("youtube.empty_unverified"));}
            }
            let mut entries=vec![];let mut entry_bytes=0usize;
            for evidence in selected {
                let entry=self.canonical.prepare(&request,&self.execution_ref,evidence.clone()).await?;entry.validate(&request.bounds,&self.supported_extensions)?;
                let mut expected=entry.observation.clone();evidence.populate(&mut expected);
                if entry.provider_entry_key!=evidence.video_id||entry.capture.captured_url!=evidence.url()||entry.capture.access_partition!=request.subscription.partition.scope||entry.observation!=expected {return Err(invalid("youtube.canonical_preparation"));}
                entry_bytes=entry_bytes.checked_add(serde_json::to_vec(&entry).map_err(|_|invalid("youtube.page"))?.len()).ok_or_else(||invalid("youtube.page_bytes"))?;
                if entry_bytes>request.bounds.max_operation_bytes {return Err(Error::LimitExceeded {field:"youtube.page_bytes".into()});}entries.push(entry);
            }
            let empty=entries.is_empty();let terminal=empty&&request.continuation.is_some();
            let state=if terminal {AcquisitionState::Partial {observed_at,reason_code:rss_contracts::TERMINAL_SOURCE_DOCUMENT.into()}}
                else if empty {AcquisitionState::VerifiedEmpty {observed_at,selected_source_identity:format!("youtube/channel/{}/public_uploads",cursor.channel)}}
                else {AcquisitionState::Partial {observed_at,reason_code:if self.strategy==Strategy::NoLoginAtom {"youtube_recent_atom_only"} else if continuation.is_some() {"youtube_uploads_page_continues"} else {"youtube_current_public_traversal_only"}.into()}};
            let page=AcquisitionPage {subscription_ref:request.subscription.subscription_ref.clone(),source_ref:request.subscription.source_ref.clone(),partition:request.subscription.partition.clone(),
                executor_ref:request.subscription.executor_ref.clone(),generation:request.subscription.generation,operation_id:request.operation_id,entries,state,
                coverage:if empty&&!terminal {hsscrap::Coverage::Complete} else {hsscrap::Coverage::Partial},continuation};
            page.validate(&request.subscription,&request.bounds,&self.supported_extensions)?;return Ok(page);
        }
    }
}
