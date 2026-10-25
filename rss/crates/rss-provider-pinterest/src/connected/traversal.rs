use super::*;
use rss_contracts::{AcquisitionPage,AcquisitionState,Continuation};
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version:u8,connection:HostRef,owner:ScopeRef,identity:String,board:BoardBinding,mode:AccessMode,source_revision:Revision,
    page:Option<String>,digest:Option<String>,offset:usize,documents:usize,visited:Vec<Option<String>>,seen:BTreeSet<String>,
}
impl<F:AuthorizedFetchPort,C:CanonicalEntryPort> AuthorizedPinterestProvider<F,C> {
    async fn current(&self,r:&SourceRequest)->rss_contracts::Result<()>{self.fetch.current(r,&self.execution_ref,&self.authorization).await}
    async fn get(&self,r:&SourceRequest,url:String,expected:Option<String>,calls:&mut usize)->rss_contracts::Result<(Vec<u8>,hsscrap::Instant)> {
        if *calls>=self.limits.max_fetches_per_operation{return Err(Error::LimitExceeded {field:"pinterest.fetches".into()});}*calls+=1;self.current(r).await?;
        let response=self.fetch.fetch(r,&self.execution_ref,&self.authorization,FetchRequest {url:url.clone(),expected_digest:expected.clone(),max_body_bytes:self.limits.max_body_bytes,timeout_millis:self.limits.timeout_millis}).await?;
        self.current(r).await?;if response.final_url!=url{return Err(Error::StaleResult);}
        let bytes=match response.status {200 if response.cached_body.is_none()=>response.body,304 if response.body.is_empty()=>response.cached_body.ok_or_else(||invalid("pinterest.missing_cached_body"))?,
            401|410=>return Err(Error::Expired),403=>return Err(Error::Denied),404=>return Err(Error::NotFound),429=>return Err(Error::RateLimited {retry_after:None}),_=>return Err(Error::Network)};
        if bytes.len()>self.limits.max_body_bytes{return Err(Error::LimitExceeded {field:"pinterest.body".into()});}
        if expected.is_some_and(|d|d!=digest(&bytes)){return Err(Error::StaleResult);}Ok((bytes,response.observed_at))
    }
    fn cursor(&self,r:&SourceRequest)->rss_contracts::Result<Cursor> {
        let a=&self.authorization;let c=if let Some(next)=&r.continuation {
            handshake_canonical_json::jcs_from_slice(next.token.as_bytes(),r.bounds.max_text_bytes).map_err(|_|invalid("pinterest.cursor"))?;
            serde_json::from_str::<Cursor>(&next.token).map_err(|_|invalid("pinterest.cursor"))?
        }else{Cursor {version:1,connection:a.connection_ref.clone(),owner:a.owner_scope.clone(),identity:a.external_identity.clone(),board:a.selected_board.clone(),mode:r.subscription.partition.mode,
            source_revision:self.source.revision,page:None,digest:None,offset:0,documents:0,visited:vec![],seen:BTreeSet::new()}};
        if c.version!=1||c.connection!=a.connection_ref||c.owner!=a.owner_scope||c.identity!=a.external_identity||c.board!=a.selected_board||c.mode!=r.subscription.partition.mode||c.source_revision!=self.source.revision
            ||c.documents>=self.limits.max_documents||c.visited.len()!=c.documents||c.visited.iter().collect::<BTreeSet<_>>().len()!=c.visited.len()||c.visited.contains(&c.page)
            ||(c.offset>0)!=c.digest.is_some()||c.offset>self.limits.max_document_entries||c.seen.len()>self.limits.max_documents.saturating_mul(self.limits.max_document_entries)||c.seen.iter().any(|id|!public::numeric_id(id))
            ||c.digest.as_ref().is_some_and(|v|v.len()!=64||!v.bytes().all(|v|v.is_ascii_hexdigit())){return Err(Error::StaleResult);}
        if let Some(next)=&c.page{token(next,&r.bounds)?;}Ok(c)
    }
    fn continuation(&self,r:&SourceRequest,c:Option<Cursor>)->rss_contracts::Result<Option<Continuation>> {
        c.map(|c|{let result=Continuation {source_ref:r.subscription.source_ref.clone(),subscription_ref:r.subscription.subscription_ref.clone(),partition:r.subscription.partition.clone(),executor_ref:r.subscription.executor_ref.clone(),generation:r.subscription.generation,
            token:serde_json::to_string(&c).map_err(|_|invalid("pinterest.cursor"))?};result.validate_for(&r.subscription,&r.bounds)?;Ok(result)}).transpose()
    }
    pub(super) async fn acquire_page(&self,r:SourceRequest)->rss_contracts::Result<AcquisitionPage> {
        self.validate(&r)?;self.current(&r).await?;let mut cursor=self.cursor(&r)?;let mut calls=0;
        let (account,_)=self.get(&r,"https://api.pinterest.com/v5/user_account".into(),None,&mut calls).await?;
        normalize::account(&account,&self.authorization.external_identity,self.limits.max_body_bytes,&r.bounds)?;
        let base=format!("https://api.pinterest.com/v5/boards/{}",self.authorization.selected_board.board_id);
        let (bytes,_)=self.get(&r,base.clone(),None,&mut calls).await?;
        let board=normalize::board(&bytes,&self.authorization,r.subscription.partition.mode,self.limits.max_body_bytes,&r.bounds)?;
        loop {
            let mut url=url::Url::parse(&format!("{base}/pins")).map_err(|_|invalid("pinterest.endpoint"))?;url.query_pairs_mut().append_pair("page_size","100");if let Some(next)=&cursor.page{url.query_pairs_mut().append_pair("bookmark",next);}
            let (bytes,time)=self.get(&r,url.to_string(),cursor.digest.clone(),&mut calls).await?;
            let document=normalize::page(&bytes,&self.authorization.selected_board.board_id,self.limits.max_body_bytes,self.limits.max_document_entries,&r.bounds)?;
            if cursor.offset>document.entries.len()||cursor.offset>0&&cursor.offset==document.entries.len(){return Err(invalid("pinterest.cursor_offset"));}
            if document.next.as_ref().is_some_and(|v|cursor.page.as_ref()==Some(v)||cursor.visited.contains(&Some(v.clone()))){return Err(invalid("pinterest.cursor_cycle"));}
            let mut selected=vec![];let mut end=cursor.offset;
            while end<document.entries.len()&&selected.len()<r.bounds.max_page_items {
                let entry=&document.entries[end];if !cursor.seen.contains(&entry.id){
                    if cursor.seen.len()>=self.limits.max_documents.saturating_mul(self.limits.max_document_entries){return Err(invalid("pinterest.seen"));}
                    cursor.seen.insert(entry.id.clone());selected.push(entry.clone());
                }end+=1;
            }
            let next=if end<document.entries.len(){let mut next=cursor.clone();next.offset=end;next.digest=Some(document.digest.clone());Some(next)}else if let Some(bookmark)=&document.next {
                let mut next=cursor.clone();next.visited.push(next.page.clone());next.documents+=1;if next.documents>=self.limits.max_documents{return Err(Error::LimitExceeded {field:"pinterest.documents".into()});}
                next.page=Some(bookmark.clone());next.offset=0;next.digest=None;Some(next)
            }else{None};
            if selected.is_empty()&&next.is_some(){cursor=next.unwrap();continue;}
            let continuation=self.continuation(&r,next)?;
            let verified_empty=selected.is_empty()&&r.continuation.is_none()&&document.entries.is_empty()&&document.next.is_none()&&board.count==Some(0);
            if selected.is_empty()&&!verified_empty&&r.continuation.is_none(){return Err(invalid("pinterest.empty_unverified"));}
            let mut entries=vec![];let mut entry_bytes=0usize;
            for evidence in selected {
                self.current(&r).await?;let entry=self.canonical.prepare(&r,&self.execution_ref,evidence.clone()).await?;self.current(&r).await?;
                entry.validate(&r.bounds,&self.supported_extensions)?;let mut expected=entry.observation.clone();evidence.populate(&mut expected);
                if entry.provider_entry_key!=evidence.id||entry.capture.captured_url!=evidence.url()||entry.capture.access_partition!=r.subscription.partition.scope||entry.observation!=expected{return Err(invalid("pinterest.canonical_preparation"));}
                entry_bytes=entry_bytes.checked_add(serde_json::to_vec(&entry).map_err(|_|invalid("pinterest.page"))?.len()).ok_or_else(||invalid("pinterest.page"))?;
                if entry_bytes>r.bounds.max_operation_bytes{return Err(Error::LimitExceeded {field:"pinterest.page".into()});}entries.push(entry);
            }
            let state=if verified_empty{AcquisitionState::VerifiedEmpty {observed_at:time,selected_source_identity:format!("pinterest/board/{}/{}",self.authorization.selected_board.board_id,board.privacy)}}else{AcquisitionState::Partial {observed_at:time,reason_code:if entries.is_empty(){rss_contracts::TERMINAL_SOURCE_DOCUMENT}else{"pinterest_authorized_current_board_only"}.into()}};
            let page=AcquisitionPage {subscription_ref:r.subscription.subscription_ref.clone(),source_ref:r.subscription.source_ref.clone(),partition:r.subscription.partition.clone(),executor_ref:r.subscription.executor_ref.clone(),generation:r.subscription.generation,operation_id:r.operation_id,
                entries,state,coverage:if verified_empty{hsscrap::Coverage::Complete}else{hsscrap::Coverage::Partial},continuation};page.validate(&r.subscription,&r.bounds,&self.supported_extensions)?;self.current(&r).await?;return Ok(page);
        }
    }
}
