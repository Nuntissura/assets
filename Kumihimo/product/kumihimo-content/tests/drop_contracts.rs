#![cfg(feature = "drops")]
use kumihimo_content::*;
use kumihimo_core::{self as core, Command, EntityId, Envelope};
use std::collections::BTreeSet;

fn command(c: &mut Controller, command: Command) -> core::Change {
    c.execute(Envelope { identity: c.snapshot().identity.clone(), actor: "drop-contract".into(), operation: c.core().ledger_status().high_watermark.checked_next().unwrap(), expected_generation: c.snapshot().generation, command }).unwrap().outcome.result.unwrap()
}
fn create(c: &mut Controller, cmd: Command) -> EntityId { let core::Change::Created(id) = command(c, cmd) else { panic!("creation") }; id }
fn permission() -> HostPermission { HostPermission { allowed: true, eligibility_revision: 0 } }
fn fixture() -> (Controller, DropRequest, DropGrant) {
    let c = core::Core::new(core::Identity::new("drop-test", "one").unwrap(), Default::default(), 32).unwrap();
    let mut c = Controller::new(c, "drop-host", Default::default()).unwrap();
    let EntityId::Workspace(workspace) = create(&mut c, Command::CreateWorkspace { owner: None }) else { unreachable!() };
    let EntityId::Frame(frame) = create(&mut c, Command::CreateFrame { workspace, index: 0 }) else { unreachable!() };
    let EntityId::Provider(provider) = create(&mut c, Command::RegisterProvider { key: "file-host".into(), supports_shared_views: false }) else { unreachable!() };
    let EntityId::Resource(resource) = create(&mut c, Command::RegisterResource { provider, key: "document".into(), restore_reference: "host-document".into() }) else { unreachable!() };
    let EntityId::View(view) = create(&mut c, Command::CreateView { frame, resource, index: 0 }) else { unreachable!() };
    command(&mut c, Command::SelectView { view });
    command(&mut c, Command::SetViewState { view, state: core::ViewState { visible: true, usable: true } });
    c.negotiate(provider, ProtocolVersion::V2, Default::default(), Default::default()).unwrap();
    let mount = c.attach(view, Default::default(), permission(), 0).unwrap();
    let attach = c.dispatch(mount, permission(), 0).unwrap().unwrap();
    c.ack(Ack { ticket: attach.ticket, result: AckResult::Confirmed(Confirmation::Attached { strength: RestorationStrength::LayoutOnly }) }, 0).unwrap();
    let destination = DropDestination { scope: c.inspect().mounts[&mount].scope.clone(), frame, generation: c.snapshot().generation, epoch: 1 };
    let request = DropRequest { offer: DropOffer { kind: DropPayloadKind::BrowserFiles, token: "host-token-one".into(), revision: 1, expires_at_ms: 100, items: vec![DropItem { label: "fixture.txt".into(), media_type: "text/plain".into(), bytes: Some(3), asset: None }] }, destination, action: DropAction::Import };
    let grant = DropGrant { permission: permission(), actions: BTreeSet::from([DropAction::Import, DropAction::Copy]), kinds: vec![DropPayloadKind::BrowserFiles] };
    (c, request, grant)
}
#[test]
fn cancel_and_consumption_cannot_dispatch_twice() {
    let (mut c, request, grant) = fixture();
    let mut drop = DropCoordinator::default();
    drop.hover(&c, request.clone(), &grant, Default::default(), 1).unwrap();
    drop.cancel();
    assert!(drop.release(&mut c, &request.offer, &grant, Default::default(), 1).is_err());
    assert!(c.inspect().pending.is_empty());
    drop.hover(&c, request.clone(), &grant, Default::default(), 2).unwrap();
    let id = drop.release(&mut c, &request.offer, &grant, Default::default(), 2).unwrap();
    assert!(drop.release(&mut c, &request.offer, &grant, Default::default(), 2).is_err());
    let effect = c.dispatch(request.destination.scope.mount, permission(), 2).unwrap().unwrap();
    assert_eq!(effect.ticket.id, id);
    assert_eq!(effect.kind, RequestKind::Drop(request));
    c.ack(Ack { ticket: effect.ticket, result: AckResult::Confirmed(Confirmation::Done) }, 2).unwrap();
    assert_eq!(c.inspect().receipts.last().unwrap().status, RequestStatus::Confirmed);
}
#[test]
fn expiry_payload_revision_and_permission_are_rechecked() {
    let (mut c, request, grant) = fixture();
    for change in 0..3 {
        let mut drop = DropCoordinator::default();
        drop.hover(&c, request.clone(), &grant, Default::default(), 1).unwrap();
        let mut offer = request.offer.clone();
        let mut live_grant = grant.clone();
        if change == 0 { offer.revision += 1; }
        if change == 1 { live_grant.permission.allowed = false; }
        assert!(drop.release(&mut c, &offer, &live_grant, Default::default(), if change == 2 { 100 } else { 1 }).is_err());
        assert!(c.inspect().pending.is_empty());
    }
}
#[test]
fn wrong_target_and_unadvertised_action_reject() {
    let (c, request, grant) = fixture();
    let mut drop = DropCoordinator::default();
    let mut wrong = request.clone(); wrong.destination.frame = core::FrameId::from_raw(999);
    assert!(drop.hover(&c, wrong, &grant, Default::default(), 1).is_err());
    let mut wrong = request.clone(); wrong.action = DropAction::Move;
    assert!(drop.hover(&c, wrong, &grant, Default::default(), 1).is_err());
    let mut wrong = request; wrong.destination.scope.identity.incarnation = "other".into();
    assert!(drop.hover(&c, wrong, &grant, Default::default(), 1).is_err());
}
#[test]
fn multi_file_and_metadata_bounds_are_atomic() {
    let (c, mut request, grant) = fixture();
    let mut drop = DropCoordinator::default();
    request.offer.items.push(request.offer.items[0].clone());
    let limits = DropLimits { items: 1, ..Default::default() };
    assert!(matches!(drop.hover(&c, request.clone(), &grant, limits, 1), Err(Error::Admission { dimension: "drop items", .. })));
    request.offer.token = "C:\\private\\file.txt".into();
    assert!(drop.hover(&c, request, &grant, Default::default(), 1).is_err());
    assert!(drop.preview().is_none());
}
#[test]
fn canonical_generation_invalidates_queued_drop_before_effect() {
    let (mut c, request, grant) = fixture();
    let mut drop = DropCoordinator::default();
    drop.hover(&c, request.clone(), &grant, Default::default(), 1).unwrap();
    let id = drop.release(&mut c, &request.offer, &grant, Default::default(), 1).unwrap();
    command(&mut c, Command::SetViewMetadata { view: request.destination.scope.target.view().unwrap(), label: "changed".into(), pinned: false, unsaved: false });
    assert!(c.dispatch(request.destination.scope.mount, permission(), 1).unwrap().is_none());
    let inspected = c.inspect();
    let receipt = inspected.receipts.iter().find(|r| r.ticket.id == id).unwrap();
    assert_eq!(receipt.status, RequestStatus::Rejected);
    assert_eq!(receipt.cause, Some("stale delivery"));
}
#[test]
fn refusal_and_delayed_completion_keep_existing_receipt_semantics() {
    let (mut c, request, grant) = fixture();
    let mut drop = DropCoordinator::default();
    drop.hover(&c, request.clone(), &grant, Default::default(), 1).unwrap();
    let id = drop.release(&mut c, &request.offer, &grant, Default::default(), 1).unwrap();
    let effect = c.dispatch(request.destination.scope.mount, permission(), 1).unwrap().unwrap();
    assert_eq!(c.inspect().pending[&id].status, RequestStatus::Dispatched);
    let receipt = c.ack(Ack { ticket: effect.ticket, result: AckResult::Rejected }, 2).unwrap();
    assert_eq!(receipt.status, RequestStatus::Rejected);
    assert!(c.inspect().pending.is_empty());
}
