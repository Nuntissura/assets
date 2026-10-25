use super::*;
use rss_contracts::{AcquisitionPage, Continuation};
use url::Url;
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version:u8,connection:HostRef,owner:ScopeRef,identity:String,kind:CredentialKind,mode:AccessMode,
    source_url:String,source_revision:Revision,target:String,page:Option<String>,digest:Option<String>,offset:usize,
    documents:usize,visited:Vec<Option<String>>,seen:BTreeSet<String>,
}
fn api(resource:&str,params:&[(&str,&str)])->rss_contracts::Result<Endpoint> {
    let mut url=Url::parse(&format!("https://www.googleapis.com/youtube/v3/{resource}")).map_err(|_|invalid("youtube.endpoint"))?;
    for (k,v) in params {url.query_pairs_mut().append_pair(k,v);}Ok(Endpoint::DataApi {url:url.into()})
}
fn private_target(raw:&str,b:&Bounds)->rss_contracts::Result<(String,String)> {
    if let Ok(crate::public::SourceTarget::Video(id))=crate::public::resolve_url(raw,b){return Ok(("video".into(),id));}
    let url=Url::parse(raw).map_err(|_|invalid("youtube.private_target"))?;
    let pairs=url.query_pairs().collect::<Vec<_>>();
    if url.scheme()!="https"||!matches!(url.host_str(),Some("www.youtube.com"|"youtube.com"))||url.path()!="/playlist"
        ||!url.username().is_empty()||url.password().is_some()||url.port().is_some()||url.fragment().is_some()
        ||pairs.len()!=1||pairs[0].0!="list"||!crate::public::playlist_id(&pairs[0].1){return Err(invalid("youtube.private_target"));}
    Ok(("playlistItem".into(),pairs[0].1.to_string()))
}
impl<F:ConnectedFetchPort,C:CanonicalEntryPort> ConnectedYouTubeProvider<F,C> {
    async fn current(&self,r:&SourceRequest)->rss_contracts::Result<()> {self.fetch.current(r,&self.execution_ref,&self.authorization).await}
    async fn get(&self,r:&SourceRequest,endpoint:Endpoint,expected:Option<String>,fetches:&mut usize)->rss_contracts::Result<(Vec<u8>,hsscrap::Instant)> {
        if *fetches>=self.limits.max_fetches_per_operation{return Err(Error::LimitExceeded {field:"youtube.fetches".into()});}*fetches+=1;
        self.current(r).await?;
        let response=self.fetch.fetch(r,&self.execution_ref,&self.authorization,ConnectedFetchRequest {endpoint:endpoint.clone(),expected_digest:expected.clone(),max_body_bytes:self.limits.max_body_bytes,timeout_millis:self.limits.timeout_millis}).await?;
        self.current(r).await?;
        if let Endpoint::DataApi {url}=&endpoint {if &response.final_url!=url{return Err(Error::StaleResult);}}
        if matches!(endpoint,Endpoint::Browse {..})&&response.final_url!="https://www.youtube.com/youtubei/v1/browse" {return Err(Error::StaleResult);}
        let bytes=match response.status {
            200 if response.cached_body.is_none()=>response.body,
            304 if response.body.is_empty()=>response.cached_body.ok_or_else(||invalid("youtube.missing_cached_body"))?,
            401=>return Err(Error::Expired),403=>return Err(Error::Denied),404=>return Err(Error::NotFound),410=>return Err(Error::Expired),
            429=>return Err(Error::RateLimited {retry_after:None}),_=>return Err(Error::Network),
        };
        if bytes.len()>self.limits.max_body_bytes{return Err(Error::LimitExceeded {field:"youtube.body".into()});}
        if expected.is_some_and(|d|d!=digest(&bytes)){return Err(Error::StaleResult);}
        Ok((bytes,response.observed_at))
    }
    fn cursor(&self,r:&SourceRequest)->rss_contracts::Result<Cursor> {
        let c=if let Some(next)=&r.continuation {
            crate::normalize::json(next.token.as_bytes(),r.bounds.max_text_bytes)?;
            serde_json::from_str::<Cursor>(&next.token).map_err(|_|invalid("youtube.connected_cursor"))?
        }else{Cursor {version:1,connection:self.authorization.connection_ref.clone(),owner:self.authorization.owner_scope.clone(),identity:self.authorization.external_identity.clone(),kind:self.authorization.credential_kind,
            mode:r.subscription.partition.mode,source_url:self.source.original_url.clone(),source_revision:self.source.revision,target:String::new(),page:None,digest:None,offset:0,documents:0,visited:vec![],seen:BTreeSet::new()}};
        if c.version!=1||c.connection!=self.authorization.connection_ref||c.owner!=self.authorization.owner_scope||c.identity!=self.authorization.external_identity||c.kind!=self.authorization.credential_kind
            ||c.mode!=r.subscription.partition.mode||c.source_url!=self.source.original_url||c.source_revision!=self.source.revision
            ||c.documents>=self.limits.max_documents||c.visited.len()!=c.documents||c.visited.iter().collect::<BTreeSet<_>>().len()!=c.visited.len()||c.visited.contains(&c.page)
            ||(c.offset>0)!=c.digest.is_some()||c.offset>self.limits.max_document_entries||c.seen.len()>self.limits.max_documents.saturating_mul(self.limits.max_document_entries)
            ||c.digest.as_ref().is_some_and(|v|v.len()!=64||!v.bytes().all(|v|v.is_ascii_hexdigit())) {return Err(Error::StaleResult);}
        if let Some(token)=&c.page{bounded_token(token,&r.bounds)?;}Ok(c)
    }
    fn continuation(&self,r:&SourceRequest,c:Option<Cursor>)->rss_contracts::Result<Option<Continuation>> {
        c.map(|c|{let result=Continuation {source_ref:r.subscription.source_ref.clone(),subscription_ref:r.subscription.subscription_ref.clone(),partition:r.subscription.partition.clone(),executor_ref:r.subscription.executor_ref.clone(),generation:r.subscription.generation,token:serde_json::to_string(&c).map_err(|_|invalid("youtube.connected_cursor"))?};result.validate_for(&r.subscription,&r.bounds)?;Ok(result)}).transpose()
    }
    async fn document(&self,r:&SourceRequest,c:&mut Cursor,fetches:&mut usize)->rss_contracts::Result<(normalize::Document,hsscrap::Instant)> {
        let mode=r.subscription.partition.mode;
        if matches!(mode,AccessMode::Following|AccessMode::HomeForYou) {
            let id=if mode==AccessMode::Following{"FEsubscriptions"}else{"FEwhat_to_watch"};
            if !c.target.is_empty()&&c.target!=id{return Err(Error::StaleResult);}c.target=id.into();
            let (bytes,time)=self.get(r,Endpoint::Browse {browse_id:c.page.is_none().then(||id.into()),continuation:c.page.clone()},c.digest.clone(),fetches).await?;
            return Ok((normalize::browse(&bytes,id,c.page.is_some(),self.limits.max_body_bytes,&r.bounds)?,time));
        }
        let (resource,target):(String,String)=if mode==AccessMode::SubscriptionImport {("subscription".into(),self.authorization.external_identity.clone())}
            else if mode==AccessMode::OwnUploads {
                let (bytes,_)=self.get(r,api("channels",&[("part","contentDetails"),("mine","true")])?,None,fetches).await?;
                let channel=crate::normalize::channel(&bytes,Some(&self.authorization.external_identity),self.limits.max_body_bytes)?;
                ("playlistItem".into(),channel.uploads)
            }else{private_target(&self.source.original_url,&r.bounds)?};
        if !c.target.is_empty()&&c.target!=target{return Err(Error::StaleResult);}c.target=target.clone();
        let mut params=vec![("part","snippet,contentDetails,status")];
        if resource=="subscription"{params=vec![("part","snippet"),("mine","true")];}
        else if resource=="video" {params.push(("id",target.as_str()));}
        else {params.push(("playlistId",target.as_str()));}
        if resource!="video"{params.push(("maxResults","50"));}
        if let Some(token)=&c.page{if resource=="video"{return Err(invalid("youtube.video_continuation"));}params.push(("pageToken",token));}
        let endpoint=api(match resource.as_str(){"subscription"=>"subscriptions","video"=>"videos",_=>"playlistItems"},&params)?;
        let (bytes,time)=self.get(r,endpoint,c.digest.clone(),fetches).await?;
        let own=matches!(mode,AccessMode::OwnUploads|AccessMode::SubscriptionImport).then_some(self.authorization.external_identity.as_str());
        Ok((normalize::api(&bytes,&resource,&target,own,self.limits.max_body_bytes,&r.bounds)?,time))
    }
    async fn batch(&self,r:&SourceRequest)->rss_contracts::Result<(normalize::Document,hsscrap::Instant,Option<Continuation>)> {
        self.validate(r)?;self.current(r).await?;let mut cursor=self.cursor(r)?;let mut fetches=0;
        loop {
            let (mut doc,time)=self.document(r,&mut cursor,&mut fetches).await?;
            let len=if r.subscription.partition.mode==AccessMode::SubscriptionImport{doc.channels.len()}else{doc.entries.len()};
            if len>self.limits.max_document_entries||cursor.offset>len{return Err(invalid("youtube.connected_offset"));}
            if doc.next.as_ref().is_some_and(|v|cursor.page.as_ref()==Some(v)||cursor.visited.contains(&Some(v.clone()))){return Err(invalid("youtube.connected_cycle"));}
            let mut end=cursor.offset;let mut entries=vec![];let mut channels=vec![];
            while end<len&&entries.len()+channels.len()<r.bounds.max_page_items {
                let key=if r.subscription.partition.mode==AccessMode::SubscriptionImport{&doc.channels[end].channel_id}else{&doc.entries[end].video_id};
                if !cursor.seen.contains(key) {
                    if cursor.seen.len()>=self.limits.max_documents.saturating_mul(self.limits.max_document_entries){return Err(invalid("youtube.connected_seen"));}
                    cursor.seen.insert(key.clone());if doc.channels.is_empty(){entries.push(doc.entries[end].clone());}else{channels.push(doc.channels[end].clone());}
                }end+=1;
            }
            let next=if end<len{let mut next=cursor.clone();next.offset=end;next.digest=Some(doc.digest.clone());Some(next)}else if let Some(token)=&doc.next {
                let mut next=cursor.clone();next.visited.push(next.page.clone());next.documents+=1;
                if next.documents>=self.limits.max_documents{return Err(Error::LimitExceeded {field:"youtube.documents".into()});}next.page=Some(token.clone());next.offset=0;next.digest=None;Some(next)
            }else{None};
            if entries.is_empty()&&channels.is_empty()&&next.is_some(){cursor=next.unwrap();continue;}
            doc.entries=entries;doc.channels=channels;let continuation=self.continuation(r,next)?;return Ok((doc,time,continuation));
        }
    }
    pub(super) async fn acquire_page(&self,r:SourceRequest)->rss_contracts::Result<AcquisitionPage> {
        if r.subscription.partition.mode==AccessMode::SubscriptionImport{return Err(Error::Unsupported {provider:Provider::Youtube,mode:AccessMode::SubscriptionImport});}
        let (doc,time,continuation)=self.batch(&r).await?;let mut entries=vec![];let mut bytes=0usize;
        for evidence in &doc.entries {
            self.current(&r).await?;let entry=self.canonical.prepare(&r,&self.execution_ref,evidence.clone()).await?;self.current(&r).await?;
            entry.validate(&r.bounds,&self.supported_extensions)?;let mut expected=entry.observation.clone();evidence.populate(&mut expected);
            if entry.provider_entry_key!=evidence.video_id||entry.capture.captured_url!=evidence.url()||entry.capture.access_partition!=r.subscription.partition.scope||entry.observation!=expected{return Err(invalid("youtube.canonical_preparation"));}
            bytes=bytes.checked_add(serde_json::to_vec(&entry).map_err(|_|invalid("youtube.page"))?.len()).ok_or_else(||invalid("youtube.page_bytes"))?;
            if bytes>r.bounds.max_operation_bytes{return Err(Error::LimitExceeded {field:"youtube.page_bytes".into()});}entries.push(entry);
        }
        let empty=entries.is_empty();let state=state(&r,&doc,time,empty,continuation.is_some());
        let coverage=if matches!(state,AcquisitionState::VerifiedEmpty {..}){hsscrap::Coverage::Complete}else{hsscrap::Coverage::Partial};
        let page=AcquisitionPage {subscription_ref:r.subscription.subscription_ref.clone(),source_ref:r.subscription.source_ref.clone(),partition:r.subscription.partition.clone(),executor_ref:r.subscription.executor_ref.clone(),generation:r.subscription.generation,operation_id:r.operation_id,entries,state,coverage,continuation};
        page.validate(&r.subscription,&r.bounds,&self.supported_extensions)?;self.current(&r).await?;Ok(page)
    }
    pub async fn import_subscriptions(&self,r:SourceRequest)->rss_contracts::Result<SubscriptionPage> {
        if r.subscription.partition.mode!=AccessMode::SubscriptionImport{return Err(Error::Unsupported {provider:Provider::Youtube,mode:r.subscription.partition.mode});}
        let (doc,time,continuation)=self.batch(&r).await?;let state=state(&r,&doc,time,doc.channels.is_empty(),continuation.is_some());
        let coverage=if matches!(state,AcquisitionState::VerifiedEmpty {..}){hsscrap::Coverage::Complete}else{hsscrap::Coverage::Partial};
        let result=SubscriptionPage {channels:doc.channels,continuation,state,coverage};
        if serde_json::to_vec(&result).map_err(|_|invalid("youtube.subscription_page"))?.len()>r.bounds.max_operation_bytes{return Err(invalid("youtube.subscription_page"));}
        self.current(&r).await?;Ok(result)
    }
}
fn state(r:&SourceRequest,doc:&normalize::Document,time:hsscrap::Instant,empty:bool,continues:bool)->AcquisitionState {
    if empty&&r.continuation.is_none()&&doc.verified_empty {AcquisitionState::VerifiedEmpty {observed_at:time,selected_source_identity:format!("youtube/{:?}/{}",r.subscription.partition.mode,doc.digest)}}
    else {AcquisitionState::Partial {observed_at:time,reason_code:if empty&&!continues{rss_contracts::TERMINAL_SOURCE_DOCUMENT}else if doc.omitted{"youtube_unrecognized_content_omitted"}else{"youtube_connected_current_traversal_only"}.into()}}
}
