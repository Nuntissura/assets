//! Disposable reference host, never shipped in the portable library.
use std::{collections::{BTreeMap,BTreeSet},fs::{File,OpenOptions},io::{Read,Write},path::PathBuf};
use serde::{Deserialize,Serialize};
use hsscrap::*;
use hsscrap::ports::OwnerCommand;
use hsscrap::operations::*;

#[derive(Debug,Clone,Serialize,Deserialize)]
pub struct Journal {pub plan:OperationPlan,pub outcome:OperationOutcome,pub receipts:Vec<OwnerStepReceipt>,pub guards:Vec<ExpectedRevision>}
#[derive(Debug,Clone,Serialize,Deserialize)]
pub struct State {
    pub scope:ScopeRef,pub grants:BTreeSet<HostRef>,pub items:BTreeMap<ItemId,Item>,
    pub captures:BTreeMap<CaptureId,URLCapture>,pub members:BTreeMap<MemberId,GalleryMember>,pub tag_catalogue:BTreeSet<TagRef>,
    pub tags:BTreeMap<AssignmentId,TagAssignment>,pub catalog_revision:Revision,pub journal:BTreeMap<OperationId,Journal>,
}
#[derive(Debug,Clone,Copy)]
pub struct Limits {pub max_bytes:usize,pub max_items:usize,pub max_tags:usize,pub max_operations:usize}
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum FailurePoint {None,AfterPending,AfterOwner(usize),AfterCommit,CrashAfterOwner(usize)}
pub struct ReferenceHost {pub path:PathBuf,pub limits:Limits}
fn io_error(e:impl std::fmt::Display)->Error {invalid("reference_storage",&e.to_string())}

pub fn item_ref(id:ItemId)->HostRef {HostRef{host_instance_id:"reference-host".into(),owner_kind:"hsscrap.item".into(),native_id:id.to_string()}}
pub fn catalogue_ref(scope:&ScopeRef)->HostRef {HostRef{host_instance_id:"reference-host".into(),owner_kind:"tags.catalogue".into(),native_id:scope.0.clone()}}

impl ReferenceHost {
    pub fn new(path:PathBuf,limits:Limits)->Result<Self> {
        if [limits.max_bytes,limits.max_items,limits.max_tags,limits.max_operations].contains(&0) || limits.max_bytes.checked_add(1).is_none() {return Err(invalid("reference_limits","positive finite limits required"));}
        Ok(Self{path,limits})
    }
    fn lock(&self)->Result<File> {
        let file=OpenOptions::new().read(true).write(true).create(true).truncate(false).open(self.path.with_extension("lock")).map_err(io_error)?;
        file.try_lock().map_err(|e|match e {std::fs::TryLockError::WouldBlock=>Error::Conflict{field:"reference_lock".into()},std::fs::TryLockError::Error(e)=>io_error(e)})?;
        Ok(file)
    }
    fn read(&self)->Result<State> {
        let file=File::open(&self.path).map_err(io_error)?;
        let mut bytes=vec![];file.take(self.limits.max_bytes as u64+1).read_to_end(&mut bytes).map_err(io_error)?;
        if bytes.len()>self.limits.max_bytes {return Err(Error::LimitExceeded{field:"reference_state".into(),limit:self.limits.max_bytes});}
        let state:State=serde_json::from_slice(&bytes).map_err(io_error)?;
        self.check_limits(&state)?;Ok(state)
    }
    fn check_limits(&self,state:&State)->Result<()> {
        for (field,size,limit) in [("items",state.items.len(),self.limits.max_items),("captures",state.captures.len(),self.limits.max_items),("members",state.members.len(),self.limits.max_items),("tag_catalogue",state.tag_catalogue.len(),self.limits.max_tags),("tags",state.tags.len(),self.limits.max_tags),("operations",state.journal.len(),self.limits.max_operations)] {
            if size>limit {return Err(Error::LimitExceeded{field:field.into(),limit});}
        } Ok(())
    }
    fn write(&self,state:&State)->Result<()> {
        self.check_limits(state)?;
        let bytes=serde_json::to_vec(state).map_err(io_error)?;
        if bytes.len()>self.limits.max_bytes {return Err(Error::LimitExceeded{field:"reference_state".into(),limit:self.limits.max_bytes});}
        let next=self.path.with_extension("next");
        {let mut file=OpenOptions::new().write(true).create(true).truncate(true).open(&next).map_err(io_error)?;file.write_all(&bytes).map_err(io_error)?;file.sync_all().map_err(io_error)?;}
        std::fs::rename(next,&self.path).map_err(io_error)?;
        #[cfg(unix)] File::open(self.path.parent().ok_or_else(||invalid("path","parent required"))?).and_then(|f|f.sync_all()).map_err(io_error)?;
        Ok(())
    }
    pub fn initialize(&self,scope:ScopeRef,actors:BTreeSet<HostRef>,items:Vec<Item>,captures:Vec<URLCapture>,tag_catalogue:BTreeSet<TagRef>)->Result<()> {
        if let Some(parent)=self.path.parent(){std::fs::create_dir_all(parent).map_err(io_error)?;}
        let _lock=self.lock()?;
        if self.path.exists(){return Err(Error::Conflict{field:"reference_initialized".into()});}
        if items.len()>self.limits.max_items{return Err(Error::LimitExceeded{field:"items".into(),limit:self.limits.max_items});}
        let mut resources=BTreeMap::new();
        for item in items {if item.owner_scope!=scope || resources.insert(item.item_id,item).is_some(){return Err(Error::Conflict{field:"reference_item".into()});}}
        let mut capture_map=BTreeMap::new();
        for capture in captures {if !resources.contains_key(&capture.item_id) || capture_map.insert(capture.capture_id,capture).is_some(){return Err(Error::Conflict{field:"reference_capture".into()});}}
        for item in resources.values() {if capture_map.get(&item.first_capture_id).is_none_or(|capture|capture.item_id!=item.item_id){return Err(Error::Conflict{field:"first_capture".into()});}}
        self.write(&State{scope,grants:actors,items:resources,captures:capture_map,members:BTreeMap::new(),tag_catalogue,tags:BTreeMap::new(),catalog_revision:Revision(1),journal:BTreeMap::new()})
    }
    fn authorize(state:&State,actor:&HostRef,scope:&ScopeRef)->Result<()> {
        if &state.scope!=scope || !state.grants.contains(actor){return Err(Error::Denied);}Ok(())
    }
    fn revision(state:&State,owner:&HostRef)->Result<Revision> {
        if owner==&catalogue_ref(&state.scope){return Ok(state.catalog_revision);}
        if owner.host_instance_id=="reference-host" && owner.owner_kind=="hsscrap.item" {
            let id:ItemId=owner.native_id.parse().map_err(io_error)?;
            return state.items.get(&id).map(|item|item.revision).ok_or(Error::NotFound);
        }
        Err(invalid("reference_owner","unsupported owner"))
    }
    fn guards(state:&State,expected:&[ExpectedRevision])->Result<()> {
        for guard in expected {if Self::revision(state,&guard.owner)?!=guard.revision{return Err(Error::Conflict{field:"expected_revision".into()});}}
        Ok(())
    }
    fn command_owners(state:&State,command:&OwnerCommand)->Result<Vec<HostRef>> {
        match command {
            OwnerCommand::ItemPatch{item,..}=>Ok(vec![item_ref(*item)]),
            OwnerCommand::AddTag(tag)=>match tag.target {TagTarget::Item(item)=>Ok(vec![item_ref(item),catalogue_ref(&state.scope)]),_=>Err(invalid("reference_target","reference adapter accepts item tags only"))},
            OwnerCommand::RemoveTag(id)=>{
                let tag=state.tags.get(id).ok_or(Error::NotFound)?;
                match tag.target {TagTarget::Item(item)=>Ok(vec![item_ref(item),catalogue_ref(&state.scope)]),_=>Err(invalid("reference_target","reference adapter accepts item tags only"))}
            },
            _=>Err(invalid("reference_command","resource/tag proof adapter does not execute organization commands")),
        }
    }
    fn active_item<'a>(state:&'a mut State,item:ItemId)->Result<&'a mut Item> {
        let value=state.items.get_mut(&item).ok_or(Error::NotFound)?;
        if value.owner_scope!=state.scope || value.lifecycle!=Lifecycle::Active{return Err(Error::Denied);}Ok(value)
    }
    fn execute(state:&mut State,command:&OwnerCommand,at:&Instant)->Result<()> {
        match command {
            OwnerCommand::ItemPatch{item,patch}=>{
                if let Patch::Set(Override::Set(member_id))=&patch.cover {
                    let member=state.members.get(member_id).ok_or(Error::NotFound)?;
                    if member.item_id!=*item || member.lifecycle!=MemberLifecycle::Active
                        || !state.items.get(item).is_some_and(|value|value.member_order.contains(member_id)) {
                        return Err(Error::Conflict{field:"cover_member".into()});
                    }
                }
                let item=Self::active_item(state,*item)?;
                for (field,value) in [(&mut item.title_override,&patch.title),(&mut item.description_override,&patch.description)] {
                    match value {Patch::Keep=>{},Patch::Set(v)=>*field=v.clone(),Patch::Clear=>*field=Override::Clear}
                }
                match &patch.cover {Patch::Keep=>{},Patch::Set(v)=>item.cover_override=v.clone(),Patch::Clear=>item.cover_override=Override::Clear}
                for (field,value) in [(&mut item.selected_observation_id,&patch.selected_observation),(&mut item.gallery_observation_id,&patch.gallery_observation)] {
                    match value {Patch::Keep=>{},Patch::Set(_)=>return Err(invalid("reference_selection","reference adapter has no observation owner")),Patch::Clear=>*field=None}
                }
                item.revision=item.revision.next()?;item.updated_at=at.clone();Ok(())
            },
            OwnerCommand::AddTag(tag)=>{
                if tag.scope!=state.scope || tag.revision!=Revision(1) || tag.removed_at.is_some(){return Err(Error::Denied);}
                if !state.tag_catalogue.contains(&tag.tag_ref){return Err(Error::NotFound);}
                let TagTarget::Item(id)=tag.target else{return Err(invalid("target","item required"));};
                if let Some(old)=state.tags.get(&tag.assignment_id){if old!=tag{return Err(Error::Conflict{field:"assignment_id".into()});}return Ok(());}
                Self::active_item(state,id)?;
                if state.tags.values().any(|old|old.scope==tag.scope && old.target==tag.target && old.tag_ref==tag.tag_ref && old.removed_at.is_none()){return Ok(());}
                let item=Self::active_item(state,id)?;item.revision=item.revision.next()?;item.updated_at=at.clone();
                state.tags.insert(tag.assignment_id,tag.clone());state.catalog_revision=state.catalog_revision.next()?;Ok(())
            },
            OwnerCommand::RemoveTag(id)=>{
                let tag=state.tags.get(id).ok_or(Error::NotFound)?.clone();
                let TagTarget::Item(item)=tag.target else{return Err(invalid("target","item required"));};
                if tag.scope!=state.scope{return Err(Error::Denied);}if tag.removed_at.is_some(){return Ok(());}
                let resource=Self::active_item(state,item)?;resource.revision=resource.revision.next()?;resource.updated_at=at.clone();
                let current=state.tags.get_mut(id).ok_or(Error::NotFound)?;current.removed_at=Some(at.clone());current.revision=current.revision.next()?;
                state.catalog_revision=state.catalog_revision.next()?;Ok(())
            },
            _=>Err(invalid("reference_command","unsupported command")),
        }
    }
    pub fn commit(&self,plan:OperationPlan,at:Instant,failure:FailurePoint)->Result<OperationOutcome> {
        let plan=plan_operation(plan.context,plan.commands,&BTreeSet::new())?;
        let _lock=self.lock()?;let mut state=self.read()?;
        Self::authorize(&state,&plan.context.actor,&plan.context.scope)?;
        let existing=state.journal.get(&plan.context.operation_id);
        match decide_operation(&plan,existing.map(|j|&j.outcome),true)? {
            OperationDecision::Replay(outcome)=>return Ok(outcome),
            OperationDecision::Begin=>{
                if state.journal.len()>=self.limits.max_operations{return Err(Error::LimitExceeded{field:"operations".into(),limit:self.limits.max_operations});}
                for command in &plan.commands {for owner in Self::command_owners(&state,command)? {
                    if !plan.context.expected_revisions.iter().any(|g|g.owner==owner){return Err(invalid("expected_revisions","missing participating owner fence"));}
                }}
                state.journal.insert(plan.context.operation_id,Journal{plan:plan.clone(),outcome:pending_outcome(&plan),receipts:vec![],guards:plan.context.expected_revisions.clone()});
                self.write(&state)?;
                if failure==FailurePoint::AfterPending{return Err(uncertain_outcome(plan.context.operation_id));}
            },
            OperationDecision::Recover(_)=>{},
        }
        let mut journal=state.journal.get(&plan.context.operation_id).ok_or(Error::NotFound)?.clone();
        if journal.outcome.common.extensions.get("hsscrap.cancel_requested")==Some(&serde_json::Value::Bool(true)){return Ok(journal.outcome);}
        for index in journal.receipts.len()..plan.commands.len() {
            if let Err(error)=Self::guards(&state,&journal.guards) {
                journal.outcome.outcome=OutcomeState::Conflict;state.journal.insert(plan.context.operation_id,journal);self.write(&state)?;return Err(error);
            }
            let owners=Self::command_owners(&state,&plan.commands[index])?;
            let mut next=state.clone();
            if let Err(error)=Self::execute(&mut next,&plan.commands[index],&at) {
                journal.outcome.outcome=if journal.receipts.is_empty(){OutcomeState::Rejected}else{OutcomeState::Conflict};
                state.journal.insert(plan.context.operation_id,journal);self.write(&state)?;return Err(error);
            }
            let owner=owners.first().ok_or_else(||invalid("owner","missing command owner"))?.clone();
            let receipt=OwnerStepReceipt{operation_id:plan.context.operation_id,intent_digest:plan.context.intent_digest.clone(),command_index:index,owner,receipt:HostRef{host_instance_id:"reference-host".into(),owner_kind:"owner.receipt".into(),native_id:format!("{}:{index}",plan.context.operation_id)}};
            journal.receipts.push(receipt.clone());journal.outcome.owner_receipt_refs.push(receipt.receipt);
            for guard in &mut journal.guards {guard.revision=Self::revision(&next,&guard.owner)?;}
            next.journal.insert(plan.context.operation_id,journal.clone());self.write(&next)?;state=next;
            if failure==FailurePoint::CrashAfterOwner(index+1){std::process::exit(77);}
            if failure==FailurePoint::AfterOwner(index+1){return Err(uncertain_outcome(plan.context.operation_id));}
        }
        let resolved_owners:Vec<_>=plan.commands.iter().map(|command|Self::command_owners(&state,command).and_then(|owners|owners.first().cloned().ok_or_else(||invalid("owner","missing command owner")))).collect::<Result<_>>()?;
        journal.outcome=finish_operation(&plan,&journal.receipts,&resolved_owners,journal.guards.iter().map(|g|g.owner.clone()).collect(),at)?;
        let outcome=journal.outcome.clone();state.journal.insert(plan.context.operation_id,journal);self.write(&state)?;
        if failure==FailurePoint::AfterCommit{return Err(uncertain_outcome(plan.context.operation_id));}Ok(outcome)
    }
    pub fn outcome(&self,actor:&HostRef,scope:&ScopeRef,id:OperationId)->Result<OperationOutcome> {
        let _lock=self.lock()?;let state=self.read()?;Self::authorize(&state,actor,scope)?;
        state.journal.get(&id).map(|j|j.outcome.clone()).ok_or(Error::NotFound)
    }
    pub fn revoke(&self,actor:&HostRef)->Result<()> {
        let _lock=self.lock()?;let mut state=self.read()?;state.grants.remove(actor);self.write(&state)
    }
    pub fn cancel(&self,actor:&HostRef,scope:&ScopeRef,id:OperationId)->Result<OperationOutcome> {
        let _lock=self.lock()?;let mut state=self.read()?;Self::authorize(&state,actor,scope)?;
        let journal=state.journal.get_mut(&id).ok_or(Error::NotFound)?;journal.outcome=cancel_operation(&journal.outcome,true)?;
        let outcome=journal.outcome.clone();self.write(&state)?;Ok(outcome)
    }
    pub fn tombstone(&self,actor:&HostRef,scope:&ScopeRef,id:ItemId,expected:Revision,at:Instant)->Result<()> {
        let _lock=self.lock()?;let mut state=self.read()?;Self::authorize(&state,actor,scope)?;
        let item=Self::active_item(&mut state,id)?;if item.revision!=expected{return Err(Error::Conflict{field:"item_revision".into()});}
        item.lifecycle=Lifecycle::Tombstoned;item.deleted_at=Some(at.clone());item.updated_at=at;item.revision=item.revision.next()?;self.write(&state)
    }
}

