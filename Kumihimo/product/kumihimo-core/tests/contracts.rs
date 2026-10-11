use kumihimo_core::*;

fn new_core(limits: Limits, receipts: usize) -> Core {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let run = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Core::new(
        Identity::new("test-layout", format!("test-run-{run}")).unwrap(),
        limits,
        receipts,
    )
    .unwrap()
}

fn envelope(core: &Core, command: Command) -> Envelope {
    Envelope {
        identity: core.snapshot().identity.clone(),
        actor: "consumer".into(),
        operation: core.ledger_status().high_watermark.checked_next().unwrap(),
        expected_generation: core.snapshot().generation,
        command,
    }
}
fn apply(core: &mut Core, command: Command) -> Change {
    let request = envelope(core, command);
    core.execute(request).outcome.result.unwrap()
}
fn created(core: &mut Core, command: Command) -> EntityId {
    match apply(core, command) {
        Change::Created(id) => id,
        other => panic!("expected created: {other:?}"),
    }
}
fn workspace(core: &mut Core, owner: Option<FrameId>) -> WorkspaceId {
    match created(core, Command::CreateWorkspace { owner }) {
        EntityId::Workspace(id) => id,
        _ => unreachable!(),
    }
}
fn frame(core: &mut Core, workspace: WorkspaceId) -> FrameId {
    let index = core.snapshot().workspaces[&workspace].frames.len();
    match created(core, Command::CreateFrame { workspace, index }) {
        EntityId::Frame(id) => id,
        _ => unreachable!(),
    }
}
fn provider(core: &mut Core, supports_shared_views: bool) -> ProviderId {
    let key = format!("provider-{}", core.snapshot().providers.len());
    match created(
        core,
        Command::RegisterProvider {
            key,
            supports_shared_views,
        },
    ) {
        EntityId::Provider(id) => id,
        _ => unreachable!(),
    }
}
fn resource(core: &mut Core, provider: ProviderId) -> ResourceId {
    let key = format!("resource-{}", core.snapshot().resources.len());
    match created(
        core,
        Command::RegisterResource {
            provider,
            key,
            restore_reference: "opaque-reference".into(),
        },
    ) {
        EntityId::Resource(id) => id,
        _ => unreachable!(),
    }
}
fn view(core: &mut Core, frame: FrameId, resource: ResourceId) -> ViewId {
    let index = core.snapshot().frames[&frame].views.len();
    match created(
        core,
        Command::CreateView {
            frame,
            resource,
            index,
        },
    ) {
        EntityId::View(id) => id,
        _ => unreachable!(),
    }
}
fn group(core: &mut Core, frame: FrameId) -> GroupId {
    match created(
        core,
        Command::CreateGroup {
            frame,
            label: "Research".into(),
            color: Some("blue".into()),
        },
    ) {
        EntityId::Group(id) => id,
        _ => unreachable!(),
    }
}
fn reject(core: &mut Core, command: Command, expected: Error) {
    let before = core.snapshot().clone();
    let request = envelope(core, command);
    assert_eq!(core.execute(request).outcome.result, Err(expected));
    assert_eq!(core.snapshot(), &before);
}

struct Fixture {
    core: Core,
    root: WorkspaceId,
    a: FrameId,
    b: FrameId,
    c: FrameId,
    child: WorkspaceId,
    leaf: FrameId,
    res: ResourceId,
    first: ViewId,
    group: GroupId,
}
fn fixture() -> Fixture {
    let mut core = new_core(Limits::default(), 8);
    let root = workspace(&mut core, None);
    let a = frame(&mut core, root);
    let b = frame(&mut core, root);
    let c = frame(&mut core, root);
    let child = workspace(&mut core, Some(a));
    let leaf = frame(&mut core, child);
    let p = provider(&mut core, false);
    let res = resource(&mut core, p);
    let first = view(&mut core, leaf, res);
    let group = group(&mut core, leaf);
    apply(
        &mut core,
        Command::AssignGroup {
            view: first,
            group: Some(group),
        },
    );
    Fixture {
        core,
        root,
        a,
        b,
        c,
        child,
        leaf,
        res,
        first,
        group,
    }
}

#[test]
fn subtree_move_preserves_descendants_groups_and_content() {
    let mut f = fixture();
    let grand = workspace(&mut f.core, Some(f.leaf));
    let deep = frame(&mut f.core, grand);
    let dest = workspace(&mut f.core, Some(f.b));
    let sibling = frame(&mut f.core, dest);
    let before = f.core.snapshot().clone();
    assert_eq!(
        apply(
            &mut f.core,
            Command::MoveFrame {
                frame: f.a,
                destination: dest,
                index: 0
            }
        ),
        Change::Moved(EntityId::Frame(f.a))
    );
    let after = f.core.snapshot();
    assert_eq!(after.workspaces[&f.root].frames, vec![f.b, f.c]);
    assert_eq!(after.workspaces[&dest].frames, vec![f.a, sibling]);
    assert_eq!(after.frames[&f.a].workspace, dest);
    for id in [f.leaf, deep] {
        assert_eq!(after.frames[&id], before.frames[&id]);
    }
    for id in [f.child, grand] {
        assert_eq!(after.workspaces[&id], before.workspaces[&id]);
    }
    assert_eq!(after.views, before.views);
    assert_eq!(after.resources, before.resources);
    assert_eq!(after.providers, before.providers);
    assert_eq!(after.groups, before.groups);
    assert_eq!(after.views[&f.first].group, Some(f.group));
}

#[test]
fn rejection_preserves_full_model_and_next_entity_allocation() {
    for case in 0..7 {
        let mut f = fixture();
        let mut control = fixture();
        let (command, error) = match case {
            0 => (
                Command::CreateWorkspace { owner: Some(f.a) },
                Error::ChildWorkspaceExists(f.a),
            ),
            1 => (
                Command::CreateFrame {
                    workspace: WorkspaceId::from_raw(999),
                    index: 0,
                },
                Error::Missing(EntityId::Workspace(WorkspaceId::from_raw(999))),
            ),
            2 => (
                Command::MoveFrame {
                    frame: f.a,
                    destination: f.child,
                    index: 0,
                },
                Error::Cycle {
                    frame: f.a,
                    destination: f.child,
                },
            ),
            3 => (
                Command::MoveFrame {
                    frame: f.b,
                    destination: f.root,
                    index: 3,
                },
                Error::InvalidIndex {
                    index: 3,
                    available: 2,
                },
            ),
            4 => (
                Command::CreateView {
                    frame: f.b,
                    resource: f.res,
                    index: 0,
                },
                Error::SharingDenied(f.res),
            ),
            5 => (
                Command::CreateView {
                    frame: f.b,
                    resource: ResourceId::from_raw(999),
                    index: 0,
                },
                Error::Missing(EntityId::Resource(ResourceId::from_raw(999))),
            ),
            _ => (
                Command::MoveView {
                    view: ViewId::from_raw(999),
                    destination: f.b,
                    index: 0,
                },
                Error::Missing(EntityId::View(ViewId::from_raw(999))),
            ),
        };
        reject(&mut f.core, command, error);
        assert_eq!(
            provider(&mut f.core, true),
            provider(&mut control.core, true)
        );
        assert_eq!(f.core.snapshot(), control.core.snapshot());
    }
    let mut f = fixture();
    let mut control = fixture();
    let mut request = envelope(
        &f.core,
        Command::RegisterProvider {
            key: format!("provider-at-{}", line!()),
            supports_shared_views: false,
        },
    );
    request.expected_generation -= 1;
    let before = f.core.snapshot().clone();
    assert_eq!(
        f.core.execute(request).outcome.result,
        Err(Error::StaleGeneration {
            expected: before.generation - 1,
            actual: before.generation
        })
    );
    assert_eq!(f.core.snapshot(), &before);
    assert_eq!(
        provider(&mut f.core, true),
        provider(&mut control.core, true)
    );
}

#[test]
fn insertion_indexes_are_measured_after_removal_for_frames_and_views() {
    for (index, order) in [(0, [1, 0, 2]), (1, [0, 1, 2]), (2, [0, 2, 1])] {
        let mut f = fixture();
        let ids = [f.a, f.b, f.c];
        let generation = f.core.snapshot().generation;
        let change = apply(
            &mut f.core,
            Command::MoveFrame {
                frame: f.b,
                destination: f.root,
                index,
            },
        );
        assert_eq!(
            f.core.snapshot().workspaces[&f.root].frames,
            order.map(|i| ids[i])
        );
        assert_eq!(
            f.core.snapshot().generation,
            generation + u64::from(index != 1)
        );
        if index == 1 {
            assert_eq!(change, Change::Unchanged);
        }
        let p = provider(&mut f.core, true);
        let r = resource(&mut f.core, p);
        let v = [
            view(&mut f.core, f.b, r),
            view(&mut f.core, f.b, r),
            view(&mut f.core, f.b, r),
        ];
        let before = f.core.snapshot().generation;
        apply(
            &mut f.core,
            Command::MoveView {
                view: v[1],
                destination: f.b,
                index,
            },
        );
        assert_eq!(f.core.snapshot().frames[&f.b].views, order.map(|i| v[i]));
        assert_eq!(f.core.snapshot().generation, before + u64::from(index != 1));
        reject(
            &mut f.core,
            Command::MoveView {
                view: v[1],
                destination: f.b,
                index: 3,
            },
            Error::InvalidIndex {
                index: 3,
                available: 2,
            },
        );
    }
    let mut f = fixture();
    apply(
        &mut f.core,
        Command::MoveView {
            view: f.first,
            destination: f.b,
            index: 0,
        },
    );
    assert!(f.core.snapshot().frames[&f.leaf].views.is_empty());
    assert_eq!(f.core.snapshot().frames[&f.leaf].selected_view, None);
    assert_eq!(f.core.snapshot().frames[&f.b].selected_view, Some(f.first));
    assert_eq!(f.core.snapshot().views[&f.first].group, None);
    assert_eq!(f.core.snapshot().views[&f.first].resource, f.res);
}

#[test]
fn practical_record_limit_can_be_lowered_without_deleting_or_blocking_moves() {
    let mut core = new_core(
        Limits {
            max_records: Some(3),
            ..Limits::default()
        },
        4,
    );
    let w = workspace(&mut core, None);
    let a = frame(&mut core, w);
    assert_eq!(core.snapshot().record_count(), 2);
    let b = frame(&mut core, w);
    assert_eq!(core.snapshot().record_count(), 3);
    reject(
        &mut core,
        Command::CreateFrame {
            workspace: w,
            index: 2,
        },
        Error::Admission {
            budget: Budget::Records,
            limit: 3,
            attempted: 4,
        },
    );
    apply(
        &mut core,
        Command::SetLimits(Limits {
            max_records: Some(1),
            ..Limits::default()
        }),
    );
    assert_eq!(core.snapshot().record_count(), 3);
    apply(
        &mut core,
        Command::MoveFrame {
            frame: a,
            destination: w,
            index: 1,
        },
    );
    assert_eq!(core.snapshot().workspaces[&w].frames, vec![b, a]);
    reject(
        &mut core,
        Command::RegisterProvider {
            key: format!("provider-at-{}", line!()),
            supports_shared_views: true,
        },
        Error::Admission {
            budget: Budget::Records,
            limit: 1,
            attempted: 4,
        },
    );
    apply(&mut core, Command::SetLimits(Limits::default()));
    let p = provider(&mut core, true);
    assert_eq!(p.raw(), 4);
}

#[test]
fn default_admission_is_finite_and_unbounded_policy_is_explicit() {
    assert_eq!(Limits::default().max_records, Some(16_384));
    assert_eq!(
        Limits::unbounded(),
        Limits {
            max_records: None,
            max_depth: None,
            max_views_per_frame: None,
            max_layout_entries: None,
            max_closed_views: None,
            max_tab_page_size: None,
            max_label_bytes: None,
            max_key_bytes: None,
        }
    );
    let core = new_core(Limits::default(), 2);
    assert_eq!(core.snapshot().limits, Limits::default());
}

#[test]
fn depth_limit_covers_descendants_and_allows_recovery_after_lowering() {
    let mut core = new_core(
        Limits {
            max_depth: Some(2),
            ..Limits::default()
        },
        4,
    );
    let root = workspace(&mut core, None);
    let a = frame(&mut core, root);
    let child = workspace(&mut core, Some(a));
    let b = frame(&mut core, child);
    let grand = workspace(&mut core, Some(b));
    reject(
        &mut core,
        Command::CreateFrame {
            workspace: grand,
            index: 0,
        },
        Error::Admission {
            budget: Budget::Depth,
            limit: 2,
            attempted: 3,
        },
    );
    let peer = frame(&mut core, root);
    let target = workspace(&mut core, Some(peer));
    reject(
        &mut core,
        Command::MoveFrame {
            frame: a,
            destination: target,
            index: 0,
        },
        Error::Admission {
            budget: Budget::Depth,
            limit: 2,
            attempted: 3,
        },
    );
    apply(
        &mut core,
        Command::SetLimits(Limits {
            max_depth: Some(0),
            ..Limits::default()
        }),
    );
    apply(
        &mut core,
        Command::MoveFrame {
            frame: b,
            destination: root,
            index: 2,
        },
    );
    assert_eq!(core.snapshot().frames[&b].workspace, root);
    let before = core.snapshot().clone();
    apply(
        &mut core,
        Command::MoveFrame {
            frame: b,
            destination: root,
            index: 2,
        },
    );
    assert_eq!(core.snapshot(), &before);
    reject(
        &mut core,
        Command::MoveFrame {
            frame: b,
            destination: child,
            index: 0,
        },
        Error::Admission {
            budget: Budget::Depth,
            limit: 0,
            attempted: 2,
        },
    );
}

#[test]
fn view_budget_sharing_selection_and_group_ownership_are_explicit() {
    let mut core = new_core(
        Limits {
            max_views_per_frame: Some(2),
            ..Limits::default()
        },
        8,
    );
    let w = workspace(&mut core, None);
    let a = frame(&mut core, w);
    let b = frame(&mut core, w);
    let p = provider(&mut core, true);
    let r = resource(&mut core, p);
    let first = view(&mut core, a, r);
    let second = view(&mut core, a, r);
    assert_ne!(first, second);
    assert_eq!(
        core.snapshot().views[&first].resource,
        core.snapshot().views[&second].resource
    );
    reject(
        &mut core,
        Command::CreateView {
            frame: a,
            resource: r,
            index: 2,
        },
        Error::Admission {
            budget: Budget::ViewsPerFrame,
            limit: 2,
            attempted: 3,
        },
    );
    let g = group(&mut core, a);
    apply(
        &mut core,
        Command::AssignGroup {
            view: second,
            group: Some(g),
        },
    );
    apply(&mut core, Command::SelectView { view: second });
    apply(
        &mut core,
        Command::MoveView {
            view: second,
            destination: b,
            index: 0,
        },
    );
    assert_eq!(core.snapshot().frames[&a].selected_view, Some(first));
    assert_eq!(core.snapshot().views[&second].group, None);
    reject(
        &mut core,
        Command::AssignGroup {
            view: second,
            group: Some(g),
        },
        Error::GroupFrameMismatch {
            view: second,
            group: g,
        },
    );
    apply(
        &mut core,
        Command::SetLimits(Limits {
            max_views_per_frame: Some(0),
            ..Limits::default()
        }),
    );
    reject(
        &mut core,
        Command::MoveView {
            view: first,
            destination: b,
            index: 1,
        },
        Error::Admission {
            budget: Budget::ViewsPerFrame,
            limit: 0,
            attempted: 2,
        },
    );
    apply(
        &mut core,
        Command::MoveView {
            view: first,
            destination: a,
            index: 0,
        },
    );
    assert_eq!(core.snapshot().frames[&a].views, vec![first]);
}

#[test]
fn replay_precedes_generation_check_and_expired_operations_never_execute() {
    let mut core = new_core(Limits::default(), 2);
    let original = envelope(&core, Command::CreateWorkspace { owner: None });
    let first = core.execute(original.clone());
    apply(
        &mut core,
        Command::RegisterProvider {
            key: format!("provider-at-{}", line!()),
            supports_shared_views: true,
        },
    );
    let before = core.snapshot().clone();
    let replay = core.execute(original.clone());
    assert!(replay.replayed);
    assert_eq!(replay.outcome, first.outcome);
    assert_eq!(core.snapshot(), &before);
    for changed in 0..3 {
        let mut request = original.clone();
        match changed {
            0 => request.actor = "other".into(),
            1 => request.expected_generation = 42,
            _ => {
                request.command = Command::RegisterProvider {
                    key: format!("provider-at-{}", line!()),
                    supports_shared_views: true,
                }
            }
        }
        assert_eq!(
            core.execute(request).outcome.result,
            Err(Error::IntentMismatch(original.operation))
        );
        assert_eq!(core.snapshot(), &before);
    }
    let mut stale = envelope(&core, Command::CreateWorkspace { owner: None });
    stale.expected_generation = 0;
    let denied = core.execute(stale.clone());
    assert_eq!(
        denied.outcome.result,
        Err(Error::StaleGeneration {
            expected: 0,
            actual: before.generation
        })
    );
    assert_eq!(core.execute(stale).outcome, denied.outcome);
    assert_eq!(
        core.execute(original.clone()).outcome.result,
        Err(Error::ExpiredOperation(original.operation))
    );
    assert_eq!(core.snapshot(), &before);
    assert_eq!(core.ledger_status().retained_operations.len(), 2);
    let mut zero = original;
    zero.operation = OperationId::new(0);
    assert_eq!(core.execute(zero).outcome.result, Err(Error::ZeroOperation));
    let mut no_receipts = new_core(Limits::default(), 0);
    let e = envelope(&no_receipts, Command::CreateWorkspace { owner: None });
    no_receipts.execute(e.clone());
    assert_eq!(
        no_receipts.execute(e.clone()).outcome.result,
        Err(Error::ExpiredOperation(e.operation))
    );
    assert!(no_receipts.ledger_status().retained_operations.is_empty());
    let mut last = envelope(
        &no_receipts,
        Command::RegisterProvider {
            key: format!("provider-at-{}", line!()),
            supports_shared_views: true,
        },
    );
    last.operation = OperationId::new(u64::MAX);
    assert!(no_receipts.execute(last.clone()).outcome.result.is_ok());
    assert_eq!(
        no_receipts.ledger_status().high_watermark.checked_next(),
        None
    );
    assert_eq!(
        no_receipts.execute(last).outcome.result,
        Err(Error::ExpiredOperation(OperationId::new(u64::MAX)))
    );
}

#[test]
fn host_counts_are_variable_and_deep_containment_is_flat() {
    let mut core = new_core(Limits::unbounded(), 4);
    let root = workspace(&mut core, None);
    let p = provider(&mut core, true);
    let r = resource(&mut core, p);
    for _ in 0..21 {
        let f = frame(&mut core, root);
        for _ in 0..31 {
            view(&mut core, f, r);
        }
    }
    assert_eq!(core.snapshot().workspaces[&root].frames.len(), 21);
    assert!(core.snapshot().frames.values().all(|f| f.views.len() == 31));
    let branch = core.snapshot().workspaces[&root].frames[0];
    let mut owner = branch;
    for _ in 0..1024 {
        let child = workspace(&mut core, Some(owner));
        owner = frame(&mut core, child);
    }
    let leaf_child = workspace(&mut core, Some(owner));
    let before = core.snapshot().clone();
    reject(
        &mut core,
        Command::MoveFrame {
            frame: branch,
            destination: leaf_child,
            index: 0,
        },
        Error::Cycle {
            frame: branch,
            destination: leaf_child,
        },
    );
    assert_eq!(core.snapshot(), &before);
    let destination = workspace(&mut core, None);
    let views = core.snapshot().views.clone();
    apply(
        &mut core,
        Command::MoveFrame {
            frame: branch,
            destination,
            index: 0,
        },
    );
    assert_eq!(core.snapshot().views, views);
    assert_eq!(
        core.snapshot().frames[&owner].child_workspace,
        Some(leaf_child)
    );
    assert_eq!(
        core.snapshot().workspaces[&destination].frames,
        vec![branch]
    );
}
