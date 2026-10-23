use kumihimo_core::*;

fn envelope(c: &Core, command: Command) -> Envelope {
    Envelope {
        identity: c.snapshot().identity.clone(),
        actor: "tab-consumer".into(),
        operation: c.ledger_status().high_watermark.checked_next().unwrap(),
        expected_generation: c.snapshot().generation,
        command,
    }
}
fn apply(c: &mut Core, command: Command) -> Change {
    let e = envelope(c, command);
    c.execute(e).outcome.result.unwrap()
}
fn reject(c: &mut Core, command: Command, error: Error) {
    let before = c.snapshot().clone();
    let e = envelope(c, command);
    assert_eq!(c.execute(e).outcome.result, Err(error));
    assert_eq!(c.snapshot(), &before);
}
fn ws(c: &mut Core) -> WorkspaceId {
    match apply(c, Command::CreateWorkspace { owner: None }) {
        Change::Created(EntityId::Workspace(id)) => id,
        _ => panic!(),
    }
}
fn frame(c: &mut Core, w: WorkspaceId, index: usize) -> FrameId {
    match apply(
        c,
        Command::CreateFrame {
            workspace: w,
            index,
        },
    ) {
        Change::Created(EntityId::Frame(id)) => id,
        _ => panic!(),
    }
}
fn view(c: &mut Core, f: FrameId, r: ResourceId, index: usize) -> ViewId {
    match apply(
        c,
        Command::CreateView {
            frame: f,
            resource: r,
            index,
        },
    ) {
        Change::Created(EntityId::View(id)) => id,
        _ => panic!(),
    }
}
fn group(c: &mut Core, f: FrameId) -> GroupId {
    match apply(
        c,
        Command::CreateGroup {
            frame: f,
            label: "Named group".into(),
            color: Some("blue".into()),
        },
    ) {
        Change::Created(EntityId::Group(id)) => id,
        _ => panic!(),
    }
}
fn metadata(c: &mut Core, id: ViewId, label: &str, pinned: bool, unsaved: bool) {
    apply(
        c,
        Command::SetViewMetadata {
            view: id,
            label: label.into(),
            pinned,
            unsaved,
        },
    );
}
struct F {
    c: Core,
    w: WorkspaceId,
    a: FrameId,
    b: FrameId,
    p: ProviderId,
    r: ResourceId,
    x: ViewId,
    y: ViewId,
    z: ViewId,
}
fn fixture(shared: bool) -> F {
    let mut c = Core::new(
        Identity::new("tabs-test", "one-explicit-instance").unwrap(),
        Limits::default(),
        32,
    )
    .unwrap();
    let w = ws(&mut c);
    let a = frame(&mut c, w, 0);
    let b = frame(&mut c, w, 1);
    let p = match apply(
        &mut c,
        Command::RegisterProvider {
            key: "provider".into(),
            supports_shared_views: shared,
        },
    ) {
        Change::Created(EntityId::Provider(id)) => id,
        _ => panic!(),
    };
    let r = match apply(
        &mut c,
        Command::RegisterResource {
            provider: p,
            key: "resource".into(),
            restore_reference: "opaque".into(),
        },
    ) {
        Change::Created(EntityId::Resource(id)) => id,
        _ => panic!(),
    };
    let x = view(&mut c, a, r, 0);
    let (y, z) = if shared {
        (view(&mut c, a, r, 1), view(&mut c, a, r, 2))
    } else {
        (x, x)
    };
    F {
        c,
        w,
        a,
        b,
        p,
        r,
        x,
        y,
        z,
    }
}
#[test]
fn dirty_veto_and_selected_unselected_close_preserve_reopen_identity() {
    let mut f = fixture(true);
    metadata(&mut f.c, f.y, "dirty", false, true);
    reject(
        &mut f.c,
        Command::CloseView {
            view: f.y,
            approved: false,
        },
        Error::CloseVeto(f.y),
    );
    apply(
        &mut f.c,
        Command::CloseView {
            view: f.y,
            approved: true,
        },
    );
    assert_eq!(f.c.snapshot().frames[&f.a].selected_view, Some(f.x));
    assert!(f.c.snapshot().views[&f.y].unsaved);
    apply(
        &mut f.c,
        Command::CloseView {
            view: f.x,
            approved: true,
        },
    );
    assert_eq!(f.c.snapshot().frames[&f.a].selected_view, Some(f.z));
    apply(
        &mut f.c,
        Command::ReopenView {
            view: f.y,
            destination: f.a,
        },
    );
    assert_eq!(f.c.snapshot().frames[&f.a].views, vec![f.z, f.y]);
    assert_eq!(f.c.snapshot().views[&f.y].resource, f.r);
    assert_eq!(f.c.snapshot().views[&f.y].state, ViewState::default());
    assert_eq!(f.c.snapshot().frames[&f.a].selected_view, Some(f.z));
    reject(
        &mut f.c,
        Command::SelectView { view: f.x },
        Error::ClosedView(f.x),
    );
    apply(
        &mut f.c,
        Command::CloseView {
            view: f.z,
            approved: true,
        },
    );
    apply(
        &mut f.c,
        Command::CloseView {
            view: f.y,
            approved: true,
        },
    );
    assert!(f.c.snapshot().frames[&f.a].views.is_empty());
    assert_eq!(f.c.snapshot().frames[&f.a].selected_view, None);
    apply(
        &mut f.c,
        Command::ReopenView {
            view: f.x,
            destination: f.a,
        },
    );
    assert_eq!(f.c.snapshot().frames[&f.a].selected_view, Some(f.x));
}
#[test]
fn retention_is_bounded_and_closed_history_does_not_consume_open_sharing() {
    let mut f = fixture(false);
    apply(
        &mut f.c,
        Command::SetLimits(Limits {
            max_closed_views: Some(1),
            ..Limits::default()
        }),
    );
    apply(
        &mut f.c,
        Command::CloseView {
            view: f.x,
            approved: true,
        },
    );
    let new = view(&mut f.c, f.a, f.r, 0);
    reject(
        &mut f.c,
        Command::ReopenView {
            view: f.x,
            destination: f.a,
        },
        Error::SharingDenied(f.r),
    );
    reject(
        &mut f.c,
        Command::CloseView {
            view: new,
            approved: true,
        },
        Error::Admission {
            budget: Budget::ClosedViews,
            limit: 1,
            attempted: 2,
        },
    );
    apply(
        &mut f.c,
        Command::SetLimits(Limits {
            max_closed_views: Some(0),
            ..Limits::default()
        }),
    );
    assert!(f.c.snapshot().views[&f.x].closed.is_some());
    apply(
        &mut f.c,
        Command::SetLimits(Limits {
            max_closed_views: Some(2),
            ..Limits::default()
        }),
    );
    apply(
        &mut f.c,
        Command::CloseView {
            view: new,
            approved: true,
        },
    );
    apply(
        &mut f.c,
        Command::SetLimits(Limits {
            max_closed_views: Some(0),
            ..Limits::default()
        }),
    );
    apply(
        &mut f.c,
        Command::ReopenView {
            view: f.x,
            destination: f.a,
        },
    );
    assert_eq!(f.c.snapshot().frames[&f.a].views, vec![f.x]);
}
#[test]
fn groups_move_interleaved_open_and_closed_members_atomically_under_pin_policy() {
    let mut f = fixture(true);
    let g = group(&mut f.c, f.a);
    for v in [f.x, f.z] {
        apply(
            &mut f.c,
            Command::AssignGroup {
                view: v,
                group: Some(g),
            },
        );
    }
    apply(
        &mut f.c,
        Command::ConfigureGroup {
            group: g,
            label: "Recognizable".into(),
            color: None,
            description: "Keyboard label".into(),
            collapsed: true,
        },
    );
    metadata(&mut f.c, f.z, "z", true, false);
    apply(
        &mut f.c,
        Command::SetPinPolicy(PinPolicy {
            deny_cross_frame_move: true,
            ..PinPolicy::default()
        }),
    );
    reject(
        &mut f.c,
        Command::MoveGroup {
            group: g,
            destination: f.b,
            index: 0,
            group_index: 0,
        },
        Error::Pinned(f.z),
    );
    apply(
        &mut f.c,
        Command::CloseView {
            view: f.z,
            approved: true,
        },
    );
    reject(
        &mut f.c,
        Command::MoveGroup {
            group: g,
            destination: f.b,
            index: 0,
            group_index: 0,
        },
        Error::Pinned(f.z),
    );
    metadata(&mut f.c, f.z, "z", false, false);
    apply(
        &mut f.c,
        Command::MoveGroup {
            group: g,
            destination: f.b,
            index: 0,
            group_index: 0,
        },
    );
    assert_eq!(f.c.snapshot().frames[&f.a].views, vec![f.y]);
    assert_eq!(f.c.snapshot().frames[&f.a].selected_view, Some(f.y));
    assert_eq!(f.c.snapshot().frames[&f.b].selected_view, Some(f.x));
    assert_eq!(f.c.snapshot().views[&f.z].frame, f.b);
    assert_eq!(f.c.snapshot().views[&f.z].group, Some(g));
    apply(
        &mut f.c,
        Command::ReopenView {
            view: f.z,
            destination: f.b,
        },
    );
    assert_eq!(f.c.snapshot().frames[&f.b].views, vec![f.x, f.z]);
    assert!(f.c.snapshot().groups[&g].collapsed);
    assert_eq!(f.c.snapshot().groups[&g].description, "Keyboard label");
    let before = f.c.snapshot().generation;
    assert_eq!(
        apply(
            &mut f.c,
            Command::MoveGroup {
                group: g,
                destination: f.b,
                index: 0,
                group_index: 0
            }
        ),
        Change::Unchanged
    );
    assert_eq!(f.c.snapshot().generation, before);
}
#[test]
fn pinned_routes_reject_with_reasons_and_unpin_restores_movement() {
    let mut f = fixture(true);
    metadata(&mut f.c, f.x, "pinned", true, false);
    apply(
        &mut f.c,
        Command::SetPinPolicy(PinPolicy {
            deny_close: true,
            deny_reorder: true,
            deny_cross_frame_move: true,
        }),
    );
    reject(
        &mut f.c,
        Command::CloseView {
            view: f.x,
            approved: true,
        },
        Error::Pinned(f.x),
    );
    reject(
        &mut f.c,
        Command::MoveView {
            view: f.x,
            destination: f.a,
            index: 2,
        },
        Error::Pinned(f.x),
    );
    reject(
        &mut f.c,
        Command::MoveView {
            view: f.x,
            destination: f.b,
            index: 0,
        },
        Error::Pinned(f.x),
    );
    assert_eq!(
        apply(
            &mut f.c,
            Command::MoveView {
                view: f.x,
                destination: f.a,
                index: 0
            }
        ),
        Change::Unchanged
    );
    metadata(&mut f.c, f.x, "unpinned", false, false);
    apply(
        &mut f.c,
        Command::MoveView {
            view: f.x,
            destination: f.b,
            index: 0,
        },
    );
    assert_eq!(f.c.snapshot().frames[&f.b].views, vec![f.x]);
}
#[test]
fn canonical_search_reaches_records_beyond_first_page_without_dropping_history() {
    let mut f = fixture(true);
    metadata(&mut f.c, f.x, "Alpha", false, false);
    metadata(&mut f.c, f.y, "Beta", false, true);
    metadata(&mut f.c, f.z, "Zulu", false, false);
    let g = group(&mut f.c, f.a);
    apply(
        &mut f.c,
        Command::AssignGroup {
            view: f.z,
            group: Some(g),
        },
    );
    let q = TabQuery {
        limit: 1,
        ..TabQuery::default()
    };
    let page = f.c.snapshot().tab_page(&q).unwrap();
    assert_eq!((page.total, page.matched, page.rows.len()), (3, 3, 1));
    let found =
        f.c.snapshot()
            .tab_page(&TabQuery {
                search: Some("zulu".into()),
                ..q.clone()
            })
            .unwrap();
    assert_eq!((found.total, found.matched), (3, 1));
    assert_eq!(found.rows[0].id, f.z);
    assert_eq!(found.rows[0].group_label.as_deref(), Some("Named group"));
    apply(
        &mut f.c,
        Command::CloseView {
            view: f.y,
            approved: true,
        },
    );
    let history =
        f.c.snapshot()
            .tab_page(&TabQuery {
                include_open: false,
                include_closed: true,
                ..q.clone()
            })
            .unwrap();
    assert_eq!(history.total, 1);
    assert!(history.rows[0].closed && history.rows[0].unsaved);
    apply(
        &mut f.c,
        Command::SetLimits(Limits {
            max_tab_page_size: Some(1),
            ..Limits::default()
        }),
    );
    assert!(matches!(
        f.c.snapshot().tab_page(&TabQuery { limit: 2, ..q }),
        Err(Error::Admission {
            budget: Budget::TabPage,
            ..
        })
    ));
    assert_eq!(f.c.snapshot().views.len(), 3);
}
#[test]
fn closed_and_missing_targets_constrain_every_tool_output_and_preserve_retained_space() {
    let mut f = fixture(true);
    apply(
        &mut f.c,
        Command::SetGeometryContext {
            workspace: f.w,
            context: GeometryContext::new(Rect::new(0.0, 0.0, 640.0, 360.0).unwrap()),
        },
    );
    let root = ToolPlacement::Edge {
        anchor: ToolAnchor::Workspace(f.w),
        edge: Edge::Top,
    };
    let anchor = ToolPlacement::Edge {
        anchor: ToolAnchor::View(f.x),
        edge: Edge::Top,
    };
    let create = |c: &mut Core, placement| {
        let mut policy = ToolPolicy::new(
            Size::new(10.0, 10.0).unwrap(),
            Size::new(1.0, 1.0).unwrap(),
            vec![placement],
        );
        policy.hidden_space = HiddenSpace::Retain;
        let change = apply(
            c,
            Command::CreateTool(ToolSpec {
                owner: f.p,
                origin: f.x,
                target: EditingTarget {
                    view: f.x,
                    resource: f.r,
                },
                placement,
                policy,
                state: TargetState {
                    target_visible: true,
                    target_usable: true,
                    origin_visible: true,
                    origin_usable: true,
                    applicable: true,
                    keyboard_focused: false,
                },
            }),
        );
        match change {
            Change::Created(EntityId::Tool(id)) => id,
            _ => panic!(),
        }
    };
    let root_tool = create(&mut f.c, root);
    let view_tool = create(&mut f.c, anchor);
    apply(
        &mut f.c,
        Command::CloseView {
            view: f.x,
            approved: true,
        },
    );
    let state = TargetState {
        target_visible: true,
        target_usable: true,
        origin_visible: true,
        origin_usable: true,
        applicable: true,
        keyboard_focused: true,
    };
    for id in [root_tool, view_tool] {
        apply(&mut f.c, Command::SetToolState { tool: id, state });
    }
    let s = f.c.snapshot();
    let root_state = s.effective_tool_presentation(&s.tools[&root_tool]);
    assert!(!root_state.usable && !root_state.applicable && root_state.reserves_space);
    let view_state = s.effective_tool_presentation(&s.tools[&view_tool]);
    assert!(!view_state.visible && !view_state.reserves_space);
    let geometry = s.geometry(f.w).unwrap();
    assert!(geometry.tools.contains_key(&root_tool));
    assert!(!geometry.tools.contains_key(&view_tool));
    apply(
        &mut f.c,
        Command::ReopenView {
            view: f.x,
            destination: f.a,
        },
    );
    assert!(!f.c.snapshot().view_usable(f.x));
    apply(
        &mut f.c,
        Command::SetViewState {
            view: f.x,
            state: ViewState {
                visible: true,
                usable: true,
            },
        },
    );
    assert!(
        f.c.snapshot()
            .effective_tool_presentation(&f.c.snapshot().tools[&root_tool])
            .usable
    );
    apply(
        &mut f.c,
        Command::SetProviderAvailability {
            provider: f.p,
            availability: ProviderAvailability::Missing,
        },
    );
    assert!(
        !f.c.snapshot()
            .effective_tool_presentation(&f.c.snapshot().tools[&root_tool])
            .usable
    );
    assert!(f
        .c
        .snapshot()
        .tab_page(&TabQuery::default())
        .unwrap()
        .rows
        .iter()
        .all(|r| r.placeholder));
}
#[test]
fn identity_utf8_metadata_admission_and_foreign_replay_leave_canonical_state_safe() {
    assert!(Identity::new("", "run").is_err());
    assert!(Identity::new("lineage", "x".repeat(129)).is_err());
    let mut f = fixture(true);
    apply(
        &mut f.c,
        Command::SetLimits(Limits {
            max_label_bytes: Some(3),
            ..Limits::default()
        }),
    );
    reject(
        &mut f.c,
        Command::SetViewMetadata {
            view: f.x,
            label: "éé".into(),
            pinned: false,
            unsaved: false,
        },
        Error::Admission {
            budget: Budget::LabelBytes,
            limit: 3,
            attempted: 4,
        },
    );
    metadata(&mut f.c, f.x, "é", false, false);
    let e = envelope(&f.c, Command::SelectView { view: f.x });
    let response = f.c.execute(e.clone());
    assert!(response.outcome.result.is_ok());
    let ledger = f.c.ledger_status();
    let before = f.c.snapshot().clone();
    let mut foreign = e;
    foreign.identity.incarnation = "foreign-run".into();
    assert_eq!(
        f.c.execute(foreign).outcome.result,
        Err(Error::IdentityMismatch)
    );
    assert_eq!(f.c.ledger_status(), ledger);
    assert_eq!(f.c.snapshot(), &before);
}
