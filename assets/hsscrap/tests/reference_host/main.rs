mod adapter;
mod identity_tests;
use adapter::*;
use std::collections::BTreeSet;
use hsscrap::*;
use hsscrap::operations::*;
use hsscrap::ports::{CommandContext,ItemPatch,OwnerCommand};

fn at()->Instant {Instant::parse("2026-10-23T00:00:00Z").unwrap()}
fn actor()->HostRef {HostRef{host_instance_id:"reference-host".into(),owner_kind:"principal".into(),native_id:"actor".into()}}
fn scope()->ScopeRef {ScopeRef("private".into())}
fn item_id()->ItemId {ItemId(uuid::Uuid::from_u128(1))}
fn tag_ref()->TagRef {TagRef(HostRef{host_instance_id:"reference-host".into(),owner_kind:"tag".into(),native_id:"tag-one".into()})}
fn limits()->Limits {Limits{max_bytes:1024*1024,max_items:100,max_tags:100,max_operations:100}}
fn bounds()->Bounds {Bounds{max_url_bytes:4096,max_text_bytes:4096,max_collection_items:100,max_page_items:25,max_operation_bytes:20000}}
fn initialize(limits:Limits)->(tempfile::TempDir,ReferenceHost) {
    let dir=tempfile::Builder::new().prefix("hsscrap-reference-").tempdir().unwrap();
    let host=ReferenceHost::new(dir.path().join("state.json"),limits,at()).unwrap();
    let capture_id=CaptureId(uuid::Uuid::from_u128(2));
    let item=Item{item_id:item_id(),owner_scope:scope(),revision:Revision(1),first_capture_id:capture_id,selected_observation_id:None,gallery_observation_id:None,member_order:vec![],title_override:Override::Inherit,description_override:Override::Inherit,cover_override:Override::Inherit,created_at:at(),updated_at:at(),lifecycle:Lifecycle::Active,deleted_at:None,common:Extensions::default()};
    let capture=URLCapture{capture_id,item_id:item_id(),capture_kind:CaptureKind::LegacyMigration,access_partition:scope(),captured_url:"https://example.org/".into(),original_url:None,identity_url:"https://example.org/".into(),normalization_version:"hsscrap.url.v1".into(),captured_at:None,created_here_at:at(),actor_ref:None,operation_id:OperationId(uuid::Uuid::from_u128(3)),common:Extensions::default()};
    host.initialize(scope(),BTreeSet::from([actor()]),vec![item],vec![capture],BTreeSet::from([tag_ref()])).unwrap();
    (dir,host)
}
fn plan(id:u128,title:&str,revision:u64,with_tag:bool)->OperationPlan {
    let mut expected=vec![ExpectedRevision{owner:item_ref(item_id()),revision:Revision(revision)}];
    let mut commands=vec![OwnerCommand::ItemPatch{item:item_id(),patch:ItemPatch{title:Patch::Set(Override::Set(title.into())),description:Patch::Keep,cover:Patch::Keep,selected_observation:Patch::Keep,gallery_observation:Patch::Keep}}];
    if with_tag {
        expected.push(ExpectedRevision{owner:catalogue_ref(&scope()),revision:Revision(1)});
        commands.push(OwnerCommand::AddTag(TagAssignment{assignment_id:AssignmentId(uuid::Uuid::from_u128(1000+id)),tag_ref:tag_ref(),target:TagTarget::Item(item_id()),scope:scope(),origin:TagOrigin::User,created_at:at(),removed_at:None,revision:Revision(1),common:Extensions::default()}));
    }
    plan_operation(CommandContext{actor:actor(),scope:scope(),operation_id:OperationId(uuid::Uuid::from_u128(id)),intent_digest:String::new(),expected_revisions:expected,bounds:bounds()},commands,&BTreeSet::new()).unwrap()
}
fn read_canonical(path:&std::path::Path)->State {serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()}

#[test]
fn lost_response_restart_replay_after_deletion_and_revocation() {
    let (_dir,host)=initialize(limits());let request=plan(10,"edited",1,true);
    assert!(matches!(host.commit(request.clone(),at(),FailurePoint::AfterCommit),Err(Error::UnknownOutcome{..})));
    let committed=read_canonical(&host.path);
    assert_eq!(committed.items[&item_id()].revision,Revision(3));assert_eq!(committed.tags.len(),1);
    assert_eq!(committed.journal[&request.context.operation_id].outcome.outcome,OutcomeState::Committed);
    let path=host.path.clone();drop(host);let restarted=ReferenceHost::new(path,limits(),at()).unwrap();
    assert_eq!(restarted.commit(request.clone(),at(),FailurePoint::None).unwrap().outcome,OutcomeState::Committed);
    assert_eq!(read_canonical(&restarted.path).items[&item_id()].revision,Revision(3));
    restarted.tombstone(&actor(),&scope(),item_id(),Revision(3),at()).unwrap();
    restarted.commit(request.clone(),at(),FailurePoint::None).unwrap();
    let after=read_canonical(&restarted.path);assert_eq!(after.items[&item_id()].lifecycle,Lifecycle::Tombstoned);assert_eq!(after.items[&item_id()].revision,Revision(4));
    restarted.revoke(&actor()).unwrap();
    assert_eq!(restarted.outcome(&actor(),&scope(),request.context.operation_id),Err(Error::Denied));
    assert_eq!(restarted.commit(request,at(),FailurePoint::None),Err(Error::Denied));
}

#[test]
fn real_process_loss_preserves_partial_owner_commit_and_recovers_without_reapplying_it() {
    let (dir,host)=initialize(limits());let request=plan(20,"first-owner",1,true);
    let request_path=dir.path().join("request.json");std::fs::write(&request_path,serde_json::to_vec(&request).unwrap()).unwrap();
    let mut child=std::process::Command::new(std::env::current_exe().unwrap());
    child.args(["--ignored","--exact","reference_worker","--nocapture"]).env("HSSCRAP_REFERENCE_DIR",dir.path());
    #[cfg(windows)] {use std::os::windows::process::CommandExt;child.creation_flags(0x08000000);}
    let output=child.output().unwrap();assert_eq!(output.status.code(),Some(77),"{}",String::from_utf8_lossy(&output.stderr));
    let partial=read_canonical(&host.path);
    assert_eq!(partial.items[&item_id()].revision,Revision(2));assert!(partial.tags.is_empty());
    assert_eq!(partial.journal[&request.context.operation_id].outcome.outcome,OutcomeState::Pending);
    assert_eq!(partial.journal[&request.context.operation_id].receipts.len(),1);
    let owners=vec![item_ref(item_id()),item_ref(item_id())];
    assert!(matches!(finish_operation(&request,&partial.journal[&request.context.operation_id].receipts,&owners,vec![],at()),Err(Error::Conflict{field}) if field=="incomplete_owner_commit"));
    let restarted=ReferenceHost::new(host.path.clone(),limits(),at()).unwrap();
    assert_eq!(restarted.commit(request,at(),FailurePoint::None).unwrap().outcome,OutcomeState::Committed);
    let final_state=read_canonical(&host.path);assert_eq!(final_state.items[&item_id()].revision,Revision(3));assert_eq!(final_state.tags.len(),1);
    let journal=final_state.journal.values().next().unwrap();let mut wrong_owner=journal.receipts.clone();wrong_owner[1].owner=catalogue_ref(&scope());
    assert!(matches!(finish_operation(&journal.plan,&wrong_owner,&owners,vec![],at()),Err(Error::Conflict{field}) if field=="owner_receipt"));
}

#[test]
fn stale_recovery_and_same_id_different_intent_preserve_current_canonical_values() {
    let (_dir,host)=initialize(limits());let request=plan(30,"first",1,true);
    assert!(matches!(host.commit(request.clone(),at(),FailurePoint::AfterOwner(1)),Err(Error::UnknownOutcome{..})));
    host.commit(plan(31,"concurrent-edit",2,false),at(),FailurePoint::None).unwrap();
    assert!(matches!(host.commit(request.clone(),at(),FailurePoint::None),Err(Error::Conflict{..})));
    let current=read_canonical(&host.path);assert_eq!(current.items[&item_id()].title_override,Override::Set("concurrent-edit".into()));assert!(current.tags.is_empty());
    let bytes=std::fs::read(&host.path).unwrap();
    assert!(matches!(host.commit(plan(30,"different-intent",3,false),at(),FailurePoint::None),Err(Error::Conflict{..})));
    assert_eq!(std::fs::read(&host.path).unwrap(),bytes);
}

#[test]
fn cancellation_and_capacity_have_explicit_noncommitted_results() {
    let (_dir,host)=initialize(Limits{max_operations:2,..limits()});let first=plan(40,"cancelled",1,false);
    assert!(matches!(host.commit(first.clone(),at(),FailurePoint::AfterPending),Err(Error::UnknownOutcome{..})));
    assert_eq!(host.cancel(&actor(),&scope(),first.context.operation_id).unwrap().outcome,OutcomeState::Rejected);
    assert_eq!(host.commit(first,at(),FailurePoint::None).unwrap().outcome,OutcomeState::Rejected);
    assert_eq!(read_canonical(&host.path).items[&item_id()].revision,Revision(1));
    let second=plan(41,"partial",1,true);
    assert!(matches!(host.commit(second.clone(),at(),FailurePoint::AfterOwner(1)),Err(Error::UnknownOutcome{..})));
    assert_eq!(host.cancel(&actor(),&scope(),second.context.operation_id).unwrap().outcome,OutcomeState::Pending);
    assert_eq!(host.commit(second,at(),FailurePoint::None).unwrap().outcome,OutcomeState::Pending);
    let current=read_canonical(&host.path);assert_eq!(current.items[&item_id()].revision,Revision(2));assert!(current.tags.is_empty());
    let before=std::fs::read(&host.path).unwrap();
    assert!(matches!(host.commit(plan(42,"over-limit",2,false),at(),FailurePoint::None),Err(Error::LimitExceeded{..})));
    assert_eq!(std::fs::read(&host.path).unwrap(),before);
}

#[test]
fn unresolved_cover_member_is_rejected_before_any_item_effect() {
    let (_dir,host)=initialize(limits());
    let mut request=plan(50,"must-not-apply",1,false);
    let OwnerCommand::ItemPatch{patch,..}=&mut request.commands[0] else {panic!("item command")};
    patch.cover=Patch::Set(Override::Set(MemberId(uuid::Uuid::from_u128(999))));
    request.context.intent_digest.clear();
    let request=plan_operation(request.context,request.commands,&BTreeSet::new()).unwrap();
    assert_eq!(host.commit(request.clone(),at(),FailurePoint::None),Err(Error::NotFound));
    let current=read_canonical(&host.path);
    assert_eq!(current.items[&item_id()].revision,Revision(1));
    assert_eq!(current.items[&item_id()].title_override,Override::Inherit);
    assert_eq!(current.items[&item_id()].cover_override,Override::Inherit);
    assert_eq!(current.journal[&request.context.operation_id].outcome.outcome,OutcomeState::Rejected);
}

#[test]
#[ignore = "launched only by the parent process-loss test"]
fn reference_worker() {
    let root=std::path::PathBuf::from(std::env::var_os("HSSCRAP_REFERENCE_DIR").expect("isolated reference directory"));
    let request:OperationPlan=serde_json::from_slice(&std::fs::read(root.join("request.json")).unwrap()).unwrap();
    let host=ReferenceHost::new(root.join("state.json"),limits(),at()).unwrap();
    let result=host.commit(request,at(),FailurePoint::CrashAfterOwner(1));panic!("expected deliberate process exit, observed {result:?}");
}
