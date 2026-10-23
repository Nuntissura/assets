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

fn capture_request()->(CommandContext,OwnerCommand) {
    let context=CommandContext{actor:actor(),scope:scope(),operation_id:OperationId(uuid::Uuid::from_u128(60)),intent_digest:String::new(),expected_revisions:vec![],bounds:bounds()};
    let url="HTTPS://EXAMPLE.ORG:443/Case/%2f?q=1&q=2#Part";
    let normalized=hsscrap::identity::normalize_url(url,&context.bounds).unwrap();
    let capture=URLCapture{capture_id:CaptureId(uuid::Uuid::from_u128(61)),item_id:item_id(),capture_kind:CaptureKind::UserInput,access_partition:context.scope.clone(),captured_url:url.into(),original_url:Some(url.into()),identity_url:normalized.identity_url,normalization_version:normalized.normalization_version,captured_at:Some(at()),created_here_at:at(),actor_ref:Some(context.actor.clone()),operation_id:context.operation_id,common:Extensions::default()};
    (context,OwnerCommand::Capture{item:item_id(),capture})
}

#[test]
fn capture_command_admits_only_matching_context_and_normalized_identity() {
    let (context,command)=capture_request();
    let plan=plan_operation(context.clone(),vec![command.clone()],&BTreeSet::new()).unwrap();
    let wire=serde_json::to_value(&plan.commands[0]).unwrap();
    assert_eq!(wire["kind"],"capture");
    assert!(wire["value"].get("context").is_none());
    assert_eq!(serde_json::from_value::<OwnerCommand>(wire).unwrap(),command);
    for field in ["item","scope","operation","actor","identity","version","history","limit"] {
        let mut command=command.clone();
        let OwnerCommand::Capture{capture,..}=&mut command else {unreachable!()};
        match field {
            "item"=>capture.item_id=ItemId(uuid::Uuid::from_u128(99)),
            "scope"=>capture.access_partition=ScopeRef("foreign".into()),
            "operation"=>capture.operation_id=OperationId(uuid::Uuid::from_u128(99)),
            "actor"=>capture.actor_ref=Some(HostRef{native_id:"other".into(),..actor()}),
            "identity"=>capture.identity_url="https://example.org/different".into(),
            "version"=>capture.normalization_version="future".into(),
            "history"=>capture.actor_ref=None,
            "limit"=>capture.captured_url="x".repeat(context.bounds.max_url_bytes+1),
            _=>unreachable!(),
        }
        assert!(plan_operation(context.clone(),vec![command],&BTreeSet::new()).is_err(),"{field}");
    }
    let mut legacy=command;
    let OwnerCommand::Capture{capture,..}=&mut legacy else {unreachable!()};
    capture.capture_kind=CaptureKind::LegacyMigration;capture.actor_ref=None;capture.original_url=None;capture.captured_at=None;
    assert!(plan_operation(context,vec![legacy],&BTreeSet::new()).is_ok());
}

#[test]
fn capture_intent_rejects_changed_url_id_scope_and_actor_on_replay() {
    let (context,command)=capture_request();
    let original=plan_operation(context.clone(),vec![command.clone()],&BTreeSet::new()).unwrap();
    let existing=pending_outcome(&original);
    assert!(matches!(decide_operation(&original,Some(&existing),true).unwrap(),OperationDecision::Recover(_)));
    for field in ["url","capture_id","item_id","scope","actor"] {
        let mut context=context.clone();let mut command=command.clone();
        let OwnerCommand::Capture{item,capture}=&mut command else {unreachable!()};
        match field {
            "url"=>{
                capture.captured_url="https://example.org/changed".into();capture.original_url=Some(capture.captured_url.clone());
                capture.identity_url=hsscrap::identity::normalize_url(&capture.captured_url,&context.bounds).unwrap().identity_url;
            },
            "capture_id"=>capture.capture_id=CaptureId(uuid::Uuid::from_u128(99)),
            "item_id"=>{*item=ItemId(uuid::Uuid::from_u128(99));capture.item_id=*item;},
            "scope"=>{context.scope=ScopeRef("other".into());capture.access_partition=context.scope.clone();},
            "actor"=>{context.actor.native_id="other".into();capture.actor_ref=Some(context.actor.clone());},
            _=>unreachable!(),
        }
        context.intent_digest=original.context.intent_digest.clone();
        assert!(matches!(plan_operation(context.clone(),vec![command.clone()],&BTreeSet::new()),Err(Error::Conflict{field}) if field=="intent_digest"));
        context.intent_digest.clear();
        let changed=plan_operation(context,vec![command],&BTreeSet::new()).unwrap();
        assert_ne!(changed.context.intent_digest,original.context.intent_digest);
        assert!(matches!(decide_operation(&changed,Some(&existing),true),Err(Error::Conflict{..})),"{field}");
    }
    assert_eq!(decide_operation(&original,Some(&existing),false),Err(Error::Denied));
}

#[test]
fn staged_capture_remains_pending_until_complete_owner_receipts() {
    let (context,command)=capture_request();
    let plan=plan_operation(context,vec![command],&BTreeSet::new()).unwrap();
    let staged=pending_outcome(&plan);
    assert_eq!(staged.outcome,OutcomeState::Pending);
    assert!(staged.committed_at.is_none());assert!(staged.owner_receipt_refs.is_empty());assert!(staged.result_refs.is_empty());
    assert!(matches!(finish_operation(&plan,&[],&[],vec![],at()),Err(Error::Conflict{field}) if field=="incomplete_owner_commit"));
    let owner=item_ref(item_id());
    let receipt=OwnerStepReceipt{operation_id:plan.context.operation_id,intent_digest:plan.context.intent_digest.clone(),command_index:0,owner:owner.clone(),receipt:HostRef{host_instance_id:"reference-host".into(),owner_kind:"receipt".into(),native_id:"capture".into()}};
    let committed=finish_operation(&plan,&[receipt],&[owner.clone()],vec![owner],at()).unwrap();
    assert_eq!(committed.outcome,OutcomeState::Committed);assert_eq!(committed.committed_at,Some(at()));
    assert!(matches!(decide_operation(&plan,Some(&committed),true).unwrap(),OperationDecision::Replay(value) if value==committed));
    assert_eq!(staged.outcome,OutcomeState::Pending);
}

fn observation_request()->MetadataObservation {
    MetadataObservation{observation_id:ObservationId(uuid::Uuid::from_u128(70)),item_id:item_id(),stream_id:StreamId(uuid::Uuid::from_u128(71)),sequence:Revision(1),
        baseline_observation_id:None,capture_id:Some(CaptureId(uuid::Uuid::from_u128(61))),appearance_ref:None,producer:"reference-provider".into(),access_partition:scope(),
        observed_at:at(),source_revision:None,canonical_url_assertion:None,provider:Some("test-provider".into()),host_site:"example.org".into(),
        title:Some("Observed title".into()),description:None,author:None,published_at:None,provider_labels:vec![],gallery_snapshot:None,source_payload_digest:None,
        status:Availability{state:AvailabilityState::Available,checked_at:Some(at()),reason_code:None,retry_after:None},common:Extensions::default()}
}

#[test]
fn observation_gallery_and_tombstone_intents_bind_payload_and_ids() {
    let (context,_)=capture_request();
    let observation=observation_request();
    let accepted=OwnerCommand::AcceptObservation{observation:observation.clone()};
    let mut changed_title=observation.clone();changed_title.title=Some("Different title".into());
    let mut changed_id=observation.clone();changed_id.observation_id=ObservationId(uuid::Uuid::from_u128(99));
    let mut changed_stream=observation.clone();changed_stream.stream_id=StreamId(uuid::Uuid::from_u128(99));
    let gallery=OwnerCommand::ApplyGallerySnapshot{item:item_id(),observation:observation.observation_id};
    for (original,changed) in [
        (accepted.clone(),OwnerCommand::AcceptObservation{observation:changed_title}),
        (accepted.clone(),OwnerCommand::AcceptObservation{observation:changed_id}),
        (accepted,OwnerCommand::AcceptObservation{observation:changed_stream}),
        (gallery.clone(),OwnerCommand::ApplyGallerySnapshot{item:item_id(),observation:ObservationId(uuid::Uuid::from_u128(99))}),
        (gallery,OwnerCommand::ApplyGallerySnapshot{item:ItemId(uuid::Uuid::from_u128(99)),observation:observation.observation_id}),
        (OwnerCommand::Tombstone{item:item_id()},OwnerCommand::Tombstone{item:ItemId(uuid::Uuid::from_u128(99))}),
    ] {
        let plan=plan_operation(context.clone(),vec![original.clone()],&BTreeSet::new()).unwrap();
        let wire=serde_json::to_value(&original).unwrap();
        assert!(wire["value"].get("context").is_none());
        assert_eq!(serde_json::from_value::<OwnerCommand>(wire).unwrap(),original);
        let mut stale=plan.context.clone();
        assert!(matches!(plan_operation(stale.clone(),vec![changed.clone()],&BTreeSet::new()),Err(Error::Conflict{field}) if field=="intent_digest"));
        stale.intent_digest.clear();
        let changed=plan_operation(stale,vec![changed],&BTreeSet::new()).unwrap();
        assert_ne!(changed.context.intent_digest,plan.context.intent_digest);
        assert!(matches!(decide_operation(&changed,Some(&pending_outcome(&plan)),true),Err(Error::Conflict{field}) if field=="operation_intent_reuse"));
    }
}

#[test]
fn observation_command_rejects_invalid_payload_and_foreign_scope_before_planning() {
    let (context,_)=capture_request();
    let observation=observation_request();
    for field in ["scope","provenance","sequence","title","feature"] {
        let mut observation=observation.clone();
        match field {
            "scope"=>observation.access_partition=ScopeRef("foreign".into()),
            "provenance"=>observation.capture_id=None,
            "sequence"=>observation.sequence=Revision(0),
            "title"=>observation.title=Some("x".repeat(context.bounds.max_text_bytes+1)),
            "feature"=>{observation.common.required_features.insert("vendor.required".into());},
            _=>unreachable!(),
        }
        assert!(plan_operation(context.clone(),vec![OwnerCommand::AcceptObservation{observation}],&BTreeSet::new()).is_err(),"{field}");
    }
}

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
