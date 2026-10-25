use super::*;
use rss_contracts::{AcquisitionPage,AcquisitionState,Continuation};
use std::collections::BTreeMap;
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version:u8,connection:HostRef,owner:ScopeRef,viewer_id:String,viewer_sec_uid:String,mode:AccessMode,source_url:String,source_revision:Revision,
    account_id:String,sec_uid:String,page:String,digest:Option<String>,offset:usize,documents:usize,visited:Vec<String>,seen:BTreeMap<String,String>,
}
fn same_target(actual:&str,expected:&str)->bool {
    let (Ok(actual),Ok(expected))=(url::Url::parse(actual),url::Url::parse(expected))else{return false;};
    if actual.scheme()!=expected.scheme()||actual.host_str()!=expected.host_str()||actual.path()!=expected.path()||actual.port()!=expected.port()
        ||!actual.username().is_empty()||actual.password().is_some()||actual.fragment().is_some(){return false;}
    let actual_pairs=actual.query_pairs().collect::<Vec<_>>();let expected_pairs=expected.query_pairs().collect::<Vec<_>>();
    if actual_pairs.iter().map(|(k,_)|k).collect::<BTreeSet<_>>().len()!=actual_pairs.len()||expected_pairs.iter().any(|pair|!actual_pairs.contains(pair)){return false;}
    actual_pairs.iter().all(|(key,_)|expected_pairs.iter().any(|(expected,_)|key==expected)||matches!(key.as_ref(),
        "aid"|"app_language"|"app_name"|"browser_language"|"browser_name"|"browser_online"|"browser_platform"|"browser_version"|"channel"|"cookie_enabled"|
        "device_id"|"odinId"|"WebIdLastTime"|"user_is_login"|"data_collection_enabled"|"fp"|"device_platform"|"focus_state"|"history_len"|"is_fullscreen"|"is_page_visible"|"language"|"os"|"priority_region"|"referer"|"region"|
        "screen_height"|"screen_width"|"tz_name"|"verifyFp"|"webcast_language"|"msToken"|"X-Bogus"|"X-Gnarly"|"_signature"))
}
impl<F:AuthorizedFetchPort,C:CanonicalEntryPort> ConnectedTikTokProvider<F,C> {
    async fn current(&self,r:&SourceRequest)->rss_contracts::Result<()>{self.fetch.current(r,&self.execution_ref,&self.authorization).await}
    async fn get(&self,r:&SourceRequest,endpoint:Endpoint,expected:Option<String>,calls:&mut usize)->rss_contracts::Result<(Vec<u8>,hsscrap::Instant)>{
        if *calls>=self.limits.max_fetches_per_operation{return Err(Error::LimitExceeded {field:"tiktok.fetches".into()});}*calls+=1;
        let url=endpoint.url()?;self.current(r).await?;
        let response=self.fetch.fetch(r,&self.execution_ref,&self.authorization,ConnectedFetchRequest {endpoint,expected_digest:expected.clone(),max_body_bytes:self.limits.max_body_bytes,timeout_millis:self.limits.timeout_millis}).await;
        self.current(r).await?;let response=response?;
        if url::Url::parse(&response.final_url).ok().is_some_and(|url|url.path().starts_with("/login")){return Err(Error::ReconnectRequired);}
        if !same_target(&response.final_url,&url){return Err(Error::StaleResult);}
        let bytes=match response.status{
            200 if response.cached_body.is_none()=>response.body,304 if response.body.is_empty()&&expected.is_some()=>response.cached_body.ok_or_else(||invalid("tiktok.missing_cached_body"))?,
            401=>return Err(Error::Expired),403=>return Err(Error::Denied),404=>return Err(Error::NotFound),410=>return Err(Error::Expired),429=>return Err(Error::RateLimited {retry_after:None}),_=>return Err(Error::Network),
        };
        if bytes.len()>self.limits.max_body_bytes{return Err(Error::LimitExceeded {field:"tiktok.body".into()});}
        if expected.is_some_and(|value|value!=digest(&bytes)){return Err(Error::StaleResult);}Ok((bytes,response.observed_at))
    }
    fn cursor(&self,r:&SourceRequest)->rss_contracts::Result<Cursor>{
        let a=&self.authorization;let c=if let Some(next)=&r.continuation{
            crate::normalize::json(next.token.as_bytes(),r.bounds.max_text_bytes)?;
            serde_json::from_str::<Cursor>(&next.token).map_err(|_|invalid("tiktok.connected_cursor"))?
        }else{Cursor {version:1,connection:a.connection_ref.clone(),owner:a.owner_scope.clone(),viewer_id:a.viewer_id.clone(),viewer_sec_uid:a.viewer_sec_uid.clone(),mode:r.subscription.partition.mode,
            source_url:self.source.original_url.clone(),source_revision:self.source.revision,account_id:String::new(),sec_uid:String::new(),page:"0".into(),digest:None,offset:0,documents:0,visited:vec![],seen:BTreeMap::new()}};
        if c.version!=1||c.connection!=a.connection_ref||c.owner!=a.owner_scope||c.viewer_id!=a.viewer_id||c.viewer_sec_uid!=a.viewer_sec_uid||c.mode!=r.subscription.partition.mode
            ||c.source_url!=self.source.original_url||c.source_revision!=self.source.revision||c.documents>=self.limits.max_documents||c.visited.len()!=c.documents
            ||c.visited.iter().collect::<BTreeSet<_>>().len()!=c.visited.len()||c.visited.contains(&c.page)||(c.offset>0)!=c.digest.is_some()||c.offset>self.limits.max_document_entries
            ||c.seen.len()>self.limits.max_documents.saturating_mul(self.limits.max_document_entries)||c.seen.keys().any(|v|!public::numeric_id(v))
            ||c.digest.as_ref().is_some_and(|v|v.len()!=64||!v.bytes().all(|v|v.is_ascii_hexdigit()))
            ||c.mode==AccessMode::HomeForYou&&(c.page!="0"||c.documents!=0||!c.visited.is_empty()){return Err(Error::StaleResult);}
        for page in c.visited.iter().chain(std::iter::once(&c.page)){cursor_value(page)?;}
        Ok(c)
    }
    fn continuation(&self,r:&SourceRequest,next:Option<Cursor>)->rss_contracts::Result<Option<Continuation>>{
        next.map(|c|{let result=Continuation {source_ref:r.subscription.source_ref.clone(),subscription_ref:r.subscription.subscription_ref.clone(),partition:r.subscription.partition.clone(),
            executor_ref:r.subscription.executor_ref.clone(),generation:r.subscription.generation,token:serde_json::to_string(&c).map_err(|_|invalid("tiktok.connected_cursor"))?};
            result.validate_for(&r.subscription,&r.bounds)?;Ok(result)}).transpose()
    }
    pub(super) async fn acquire_page(&self,r:SourceRequest)->rss_contracts::Result<AcquisitionPage>{
        let name=self.validate(&r)?;self.current(&r).await?;let mut cursor=self.cursor(&r)?;let mut calls=0;let mode=r.subscription.partition.mode;
        let account=if let Some(name)=name{
            let (bytes,_)=self.get(&r,Endpoint::Profile {username:name.clone()},None,&mut calls).await?;
            let account=crate::normalize::connected_account(&bytes,&name,self.limits.max_body_bytes,&r.bounds,mode,&self.authorization.viewer_id,&self.authorization.viewer_sec_uid)?;
            if !cursor.account_id.is_empty()&&(cursor.account_id!=account.id||cursor.sec_uid!=account.sec_uid){return Err(Error::StaleResult);}
            cursor.account_id=account.id.clone();cursor.sec_uid=account.sec_uid.clone();Some(account)
        }else{if !cursor.account_id.is_empty()||!cursor.sec_uid.is_empty(){return Err(Error::StaleResult);}None};
        loop{
            let endpoint=match mode {AccessMode::Following=>Endpoint::Following {cursor:cursor.page.clone()},AccessMode::HomeForYou=>Endpoint::ForYou,
                _=>Endpoint::Posts {sec_uid:cursor.sec_uid.clone(),cursor:cursor.page.clone()}};
            let (bytes,time)=self.get(&r,endpoint,cursor.digest.clone(),&mut calls).await?;
            let doc=normalize::document(&bytes,mode,account.as_ref(),self.limits.max_body_bytes,self.limits.max_document_entries,&r.bounds)?;
            if cursor.offset>0&&cursor.offset>=doc.entries.len(){return Err(invalid("tiktok.connected_offset"));}
            if doc.next.as_ref().is_some_and(|v|v==&cursor.page||cursor.visited.contains(v)){return Err(invalid("tiktok.connected_cycle"));}
            let now=chrono::DateTime::parse_from_rfc3339(time.as_str()).map_err(|_|invalid("tiktok.observed_at"))?.timestamp().max(0) as u64;
            if doc.entries.iter().flat_map(|v|&v.media).any(|m|m.expires_at_unix.is_some_and(|expiry|expiry<=now)){return Err(Error::Expired);}
            for e in &doc.entries{if cursor.seen.get(&e.id).is_some_and(|v|v!=&format!("{}/{}/{}",e.author_id,e.author_sec_uid,e.photo)){return Err(invalid("tiktok.retargeted_item"));}}
            let mut next=cursor.clone();let mut selected=vec![];let mut end=cursor.offset;
            while end<doc.entries.len()&&selected.len()<r.bounds.max_page_items{
                let e=&doc.entries[end];if !next.seen.contains_key(&e.id){
                    if next.seen.len()>=self.limits.max_documents.saturating_mul(self.limits.max_document_entries){return Err(invalid("tiktok.connected_seen"));}
                    next.seen.insert(e.id.clone(),format!("{}/{}/{}",e.author_id,e.author_sec_uid,e.photo));selected.push(e);
                }end+=1;
            }
            let next=if end<doc.entries.len(){next.offset=end;next.digest=Some(doc.digest.clone());Some(next)}
                else if let Some(page)=&doc.next{next.visited.push(cursor.page.clone());next.documents+=1;if next.documents>=self.limits.max_documents{return Err(Error::LimitExceeded {field:"tiktok.documents".into()});}
                    next.page=page.clone();next.offset=0;next.digest=None;Some(next)}else{None};
            if selected.is_empty()&&next.is_some(){cursor=next.unwrap();continue;}
            let verified_empty=selected.is_empty()&&r.continuation.is_none()&&mode==AccessMode::OwnUploads&&doc.terminal&&doc.entries.is_empty()
                &&account.as_ref().is_some_and(|a|a.video_count==Some(0));
            if selected.is_empty()&&r.continuation.is_none()&&!verified_empty{return Err(invalid("tiktok.connected_empty_unverified"));}
            let continuation=self.continuation(&r,next)?;let mut entries=vec![];let mut total=0usize;
            for evidence in selected{
                self.current(&r).await?;let entry=self.canonical.prepare(&r,&self.execution_ref,evidence.clone()).await;self.current(&r).await?;let entry=entry?;
                entry.validate(&r.bounds,&self.supported_extensions)?;let mut expected=entry.observation.clone();evidence.populate(&mut expected);
                if entry.provider_entry_key!=evidence.id||entry.capture.captured_url!=evidence.url()||entry.capture.access_partition!=r.subscription.partition.scope||entry.observation!=expected{return Err(invalid("tiktok.canonical_preparation"));}
                total=total.checked_add(serde_json::to_vec(&entry).map_err(|_|invalid("tiktok.page"))?.len()).ok_or_else(||invalid("tiktok.page_bytes"))?;
                if total>r.bounds.max_operation_bytes{return Err(Error::LimitExceeded {field:"tiktok.page_bytes".into()});}entries.push(entry);
            }
            let state=if verified_empty{AcquisitionState::VerifiedEmpty {observed_at:time,selected_source_identity:format!("tiktok/own-profile/{}/posts",cursor.account_id)}}
                else{AcquisitionState::Partial {observed_at:time,reason_code:if entries.is_empty(){rss_contracts::TERMINAL_SOURCE_DOCUMENT}else if mode==AccessMode::HomeForYou{"tiktok_foryou_retained_current_batch"}else{"tiktok_connected_current_traversal_only"}.into()}};
            let page=AcquisitionPage {subscription_ref:r.subscription.subscription_ref.clone(),source_ref:r.subscription.source_ref.clone(),partition:r.subscription.partition.clone(),
                executor_ref:r.subscription.executor_ref.clone(),generation:r.subscription.generation,operation_id:r.operation_id,entries,state,coverage:if verified_empty{hsscrap::Coverage::Complete}else{hsscrap::Coverage::Partial},continuation};
            page.validate(&r.subscription,&r.bounds,&self.supported_extensions)?;self.current(&r).await?;return Ok(page);
        }
    }
}
