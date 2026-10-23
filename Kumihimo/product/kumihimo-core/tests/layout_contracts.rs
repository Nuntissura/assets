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

use std::collections::BTreeMap;

fn s(value: f64) -> Scalar {
    Scalar::new(value).unwrap()
}
fn size(w: f64, h: f64) -> Size {
    Size::new(w, h).unwrap()
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect::new(x, y, w, h).unwrap()
}
fn envelope(core: &Core, command: Command) -> Envelope {
    Envelope {
        identity: core.snapshot().identity.clone(),
        actor: "layout-consumer".into(),
        operation: core.ledger_status().high_watermark.checked_next().unwrap(),
        expected_generation: core.snapshot().generation,
        command,
    }
}
fn apply(core: &mut Core, command: Command) -> Change {
    let e = envelope(core, command);
    core.execute(e).outcome.result.unwrap()
}
fn create(core: &mut Core, command: Command) -> EntityId {
    match apply(core, command) {
        Change::Created(id) => id,
        change => panic!("expected create: {change:?}"),
    }
}
fn workspace(core: &mut Core, owner: Option<FrameId>) -> WorkspaceId {
    match create(core, Command::CreateWorkspace { owner }) {
        EntityId::Workspace(id) => id,
        _ => unreachable!(),
    }
}
fn frame(core: &mut Core, workspace: WorkspaceId) -> FrameId {
    let index = core.snapshot().workspaces[&workspace].frames.len();
    match create(core, Command::CreateFrame { workspace, index }) {
        EntityId::Frame(id) => id,
        _ => unreachable!(),
    }
}
fn reject(core: &mut Core, command: Command) -> Error {
    let before = core.snapshot().clone();
    let e = envelope(core, command);
    let error = core.execute(e).outcome.result.unwrap_err();
    assert_eq!(core.snapshot(), &before);
    error
}
struct Fixture {
    core: Core,
    w: WorkspaceId,
    a: FrameId,
    b: FrameId,
    provider: ProviderId,
    va: ViewId,
    vb: ViewId,
}
fn fixture() -> Fixture {
    let mut core = new_core(Limits::default(), 16);
    let w = workspace(&mut core, None);
    let a = frame(&mut core, w);
    let b = frame(&mut core, w);
    let EntityId::Provider(provider) = create(
        &mut core,
        Command::RegisterProvider {
            key: format!("provider-at-{}", line!()),
            supports_shared_views: true,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Resource(resource) = create(
        &mut core,
        Command::RegisterResource {
            provider,
            key: format!("resource-{}", provider.raw()),
            restore_reference: "opaque-reference".into(),
        },
    ) else {
        unreachable!()
    };
    let EntityId::View(va) = create(
        &mut core,
        Command::CreateView {
            frame: a,
            resource,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Resource(other_resource) = create(
        &mut core,
        Command::RegisterResource {
            provider,
            key: "resource-b".into(),
            restore_reference: "opaque-reference-b".into(),
        },
    ) else {
        unreachable!()
    };
    let EntityId::View(vb) = create(
        &mut core,
        Command::CreateView {
            frame: b,
            resource: other_resource,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    Fixture {
        core,
        w,
        a,
        b,
        provider,
        va,
        vb,
    }
}
fn context(f: &Fixture, width: f64, height: f64) -> GeometryContext {
    let mut c = GeometryContext::new(rect(0.0, 0.0, width, height));
    c.allowed_docks = vec![
        DockLocation::Append { workspace: f.w },
        DockLocation::WorkspaceEdge {
            workspace: f.w,
            edge: Edge::Top,
        },
        DockLocation::Beside {
            frame: f.a,
            edge: Edge::Right,
        },
    ];
    c.allow_floating = true;
    c
}
fn configure(f: &mut Fixture, c: GeometryContext) {
    apply(
        &mut f.core,
        Command::SetGeometryContext {
            workspace: f.w,
            context: c,
        },
    );
}
fn split_plan(a: FrameId, b: FrameId) -> LayoutPlan {
    LayoutPlan {
        root: Some(NodeId(3)),
        nodes: BTreeMap::from([
            (NodeId(1), LayoutNode::Leaf(LayoutItem::Frame(a))),
            (NodeId(2), LayoutNode::Leaf(LayoutItem::Frame(b))),
            (
                NodeId(3),
                LayoutNode::Split {
                    axis: Axis::Horizontal,
                    children: vec![
                        WeightedNode {
                            node: NodeId(1),
                            weight: s(1.0),
                        },
                        WeightedNode {
                            node: NodeId(2),
                            weight: s(3.0),
                        },
                    ],
                },
            ),
        ]),
    }
}
fn grid_plan(a: FrameId, b: FrameId) -> LayoutPlan {
    LayoutPlan {
        root: Some(NodeId(3)),
        nodes: BTreeMap::from([
            (NodeId(1), LayoutNode::Leaf(LayoutItem::Frame(a))),
            (NodeId(2), LayoutNode::Leaf(LayoutItem::Frame(b))),
            (
                NodeId(3),
                LayoutNode::Grid {
                    rows: vec![s(1.0), s(2.0)],
                    columns: vec![s(1.0), s(3.0)],
                    cells: vec![
                        Cell {
                            row: 0,
                            column: 0,
                            node: NodeId(1),
                        },
                        Cell {
                            row: 1,
                            column: 1,
                            node: NodeId(2),
                        },
                    ],
                },
            ),
        ]),
    }
}
fn target(f: &Fixture, view: ViewId) -> EditingTarget {
    EditingTarget {
        view,
        resource: f.core.snapshot().views[&view].resource,
    }
}
fn tool_spec(f: &Fixture, placement: ToolPlacement, preferred: Size) -> ToolSpec {
    let mut policy = ToolPolicy::new(
        preferred,
        size(5.0, 5.0),
        vec![
            placement,
            ToolPlacement::Edge {
                anchor: ToolAnchor::Workspace(f.w),
                edge: Edge::Top,
            },
            ToolPlacement::Edge {
                anchor: ToolAnchor::Workspace(f.w),
                edge: Edge::Left,
            },
        ],
    );
    policy.orientations = vec![Axis::Horizontal, Axis::Vertical];
    policy.compatible_targets = vec![target(f, f.vb)];
    if matches!(
        placement,
        ToolPlacement::Edge {
            edge: Edge::Left | Edge::Right,
            ..
        }
    ) {
        policy.orientation = Axis::Vertical;
    }
    ToolSpec {
        owner: f.provider,
        origin: f.va,
        target: target(f, f.va),
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
    }
}
fn tool(core: &mut Core, spec: ToolSpec) -> ToolId {
    match create(core, Command::CreateTool(spec)) {
        EntityId::Tool(id) => id,
        _ => unreachable!(),
    }
}

#[test]
fn finite_numeric_construction_and_exact_envelope_replay() {
    let mut f = fixture();
    let before = f.core.snapshot().clone();
    let ledger = f.core.ledger_status();
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            Scalar::new(value),
            Err(Error::Geometry(GeometryError::NonFinite))
        );
    }
    assert!(Size::new(-1.0, 0.0).is_err());
    assert!(Rect::new(f64::MAX, 0.0, f64::MAX, 1.0).is_err());
    assert_eq!(s(-0.0), s(0.0));
    assert_eq!(s(-0.0).get().to_bits(), 0.0_f64.to_bits());
    assert_eq!(f.core.snapshot(), &before);
    assert_eq!(f.core.ledger_status(), ledger);
    let c = context(&f, 200.0, 100.0);
    let e = envelope(
        &f.core,
        Command::SetGeometryContext {
            workspace: f.w,
            context: c,
        },
    );
    let response = f.core.execute(e.clone());
    assert!(response.outcome.result.is_ok());
    apply(&mut f.core, Command::SelectView { view: f.vb });
    let replay = f.core.execute(e.clone());
    assert!(replay.replayed);
    assert_eq!(replay.outcome, response.outcome);
    let mut changed = e;
    changed.actor = "other".into();
    assert!(matches!(
        f.core.execute(changed).outcome.result,
        Err(Error::IntentMismatch(_))
    ));
}

#[test]
fn shared_split_edges_minima_and_grid_holes_use_host_weights() {
    let mut f = fixture();
    apply(
        &mut f.core,
        Command::SetLayout {
            workspace: f.w,
            plan: split_plan(f.a, f.b),
        },
    );
    let mut c = context(&f, 210.0, 100.0);
    c.gap = s(10.0);
    c.minimums.insert(f.a, size(20.0, 10.0));
    c.minimums.insert(f.b, size(40.0, 15.0));
    configure(&mut f, c);
    let g = f.core.snapshot().geometry(f.w).unwrap();
    // 60 minima + 10 gap; 140 spare split 1:3 => 55 and 145.
    assert_eq!(g.frames[&f.a], rect(0.0, 0.0, 55.0, 100.0));
    assert_eq!(g.frames[&f.b], rect(65.0, 0.0, 145.0, 100.0));
    assert_eq!(g.frames[&f.a].right() + 10.0, g.frames[&f.b].x());
    assert_eq!(g.required_extent, size(70.0, 15.0));
    apply(
        &mut f.core,
        Command::SetLayout {
            workspace: f.w,
            plan: grid_plan(f.a, f.b),
        },
    );
    let g = f.core.snapshot().geometry(f.w).unwrap();
    assert_eq!(g.frames.len(), 2);
    assert_eq!(g.frames[&f.a].width(), 55.0);
    assert_eq!(g.frames[&f.b].width(), 145.0);
    assert_eq!(g.frames[&f.a].right() + 10.0, g.frames[&f.b].x());
    assert_eq!(g.frames[&f.a].bottom() + 10.0, g.frames[&f.b].y());
    for r in g.frames.values() {
        assert!(
            r.right().is_finite()
                && r.bottom().is_finite()
                && r.width() >= 0.0
                && r.height() >= 0.0
        );
    }
}

#[test]
fn nested_plans_and_legacy_moves_reconcile_without_losing_subtrees() {
    let mut f = fixture();
    let child = workspace(&mut f.core, Some(f.a));
    let leaf = frame(&mut f.core, child);
    let nested = LayoutPlan {
        root: Some(NodeId(10)),
        nodes: BTreeMap::from([(NodeId(10), LayoutNode::Leaf(LayoutItem::Frame(leaf)))]),
    };
    apply(
        &mut f.core,
        Command::SetLayout {
            workspace: child,
            plan: nested.clone(),
        },
    );
    let mut child_context = GeometryContext::new(rect(0.0, 0.0, 100.0, 50.0));
    child_context.minimums.insert(leaf, size(30.0, 20.0));
    apply(
        &mut f.core,
        Command::SetGeometryContext {
            workspace: child,
            context: child_context,
        },
    );
    apply(
        &mut f.core,
        Command::SetLayout {
            workspace: f.w,
            plan: grid_plan(f.a, f.b),
        },
    );
    let c = context(&f, 300.0, 200.0);
    configure(&mut f, c);
    assert!(f.core.snapshot().geometry(f.w).unwrap().frames[&f.a].width() >= 30.0);
    let destination = workspace(&mut f.core, None);
    apply(
        &mut f.core,
        Command::SetLayout {
            workspace: destination,
            plan: LayoutPlan::default(),
        },
    );
    apply(
        &mut f.core,
        Command::SetGeometryContext {
            workspace: destination,
            context: GeometryContext::new(rect(0.0, 0.0, 200.0, 100.0)),
        },
    );
    let views = f.core.snapshot().views.clone();
    apply(
        &mut f.core,
        Command::MoveFrame {
            frame: f.a,
            destination,
            index: 0,
        },
    );
    assert_eq!(f.core.snapshot().layouts[&child], nested);
    assert_eq!(f.core.snapshot().views, views);
    let LayoutNode::Grid {
        rows,
        columns,
        cells,
    } = &f.core.snapshot().layouts[&f.w].nodes[&NodeId(3)]
    else {
        panic!()
    };
    assert_eq!(rows, &vec![s(1.0), s(2.0)]);
    assert_eq!(columns, &vec![s(1.0), s(3.0)]);
    assert_eq!(cells.len(), 1);
    assert_eq!(f.core.snapshot().geometry(f.w).unwrap().frames.len(), 1);
    assert_eq!(
        f.core
            .snapshot()
            .geometry(destination)
            .unwrap()
            .frames
            .len(),
        1
    );
    let added = frame(&mut f.core, f.w);
    let g = f.core.snapshot().geometry(f.w).unwrap();
    assert!(g.frames.contains_key(&added));
    assert!(f.core.snapshot().layouts[&f.w]
        .nodes
        .values()
        .any(|n| matches!(n, LayoutNode::Grid { rows, .. } if rows.len() == 2)));
    let before = f.core.snapshot().clone();
    let index = f.core.snapshot().workspaces[&f.w]
        .frames
        .iter()
        .position(|id| *id == f.b)
        .unwrap();
    assert_eq!(
        apply(
            &mut f.core,
            Command::MoveFrame {
                frame: f.b,
                destination: f.w,
                index
            }
        ),
        Change::Unchanged
    );
    assert_eq!(f.core.snapshot(), &before);
}

#[test]
fn malformed_plans_and_arithmetic_rejections_preserve_allocation() {
    let mut f = fixture();
    let mut control = fixture();
    let identity = f.core.snapshot().identity.clone();
    let control_identity = control.core.snapshot().identity.clone();
    let mut plans = vec![];
    let mut duplicate = split_plan(f.a, f.b);
    duplicate
        .nodes
        .insert(NodeId(2), LayoutNode::Leaf(LayoutItem::Frame(f.a)));
    plans.push(duplicate);
    let mut cycle = split_plan(f.a, f.b);
    let LayoutNode::Split { children, .. } = cycle.nodes.get_mut(&NodeId(3)).unwrap() else {
        panic!()
    };
    children[0].node = NodeId(3);
    plans.push(cycle);
    let mut missing = split_plan(f.a, f.b);
    missing.nodes.insert(
        NodeId(2),
        LayoutNode::Leaf(LayoutItem::Frame(FrameId::from_raw(999))),
    );
    plans.push(missing);
    let mut invalid_cell = grid_plan(f.a, f.b);
    let LayoutNode::Grid { cells, .. } = invalid_cell.nodes.get_mut(&NodeId(3)).unwrap() else {
        panic!()
    };
    cells[0].row = 4;
    plans.push(invalid_cell);
    let mut bad_weight = split_plan(f.a, f.b);
    let LayoutNode::Split { children, .. } = bad_weight.nodes.get_mut(&NodeId(3)).unwrap() else {
        panic!()
    };
    children[0].weight = s(0.0);
    plans.push(bad_weight);
    for plan in plans {
        reject(
            &mut f.core,
            Command::SetLayout {
                workspace: f.w,
                plan,
            },
        );
    }
    let mut c = context(&f, f64::MAX, 100.0);
    c.minimums.insert(f.a, size(f64::MAX, 1.0));
    c.minimums.insert(f.b, size(f64::MAX, 1.0));
    assert_eq!(
        reject(
            &mut f.core,
            Command::SetGeometryContext {
                workspace: f.w,
                context: c
            }
        ),
        Error::Geometry(GeometryError::ArithmeticOverflow)
    );
    let first = create(
        &mut f.core,
        Command::RegisterProvider {
            key: "allocation-control".into(),
            supports_shared_views: false,
        },
    );
    let other = create(
        &mut control.core,
        Command::RegisterProvider {
            key: "allocation-control".into(),
            supports_shared_views: false,
        },
    );
    assert_eq!(first, other);
    assert_eq!(f.core.snapshot().identity, identity);
    assert_eq!(control.core.snapshot().identity, control_identity);
    assert_eq!(identity.lineage, control_identity.lineage);
    assert_ne!(identity.incarnation, control_identity.incarnation);
    let mut expected = control.core.snapshot().clone();
    expected.identity = identity;
    assert_eq!(f.core.snapshot(), &expected);
}

#[test]
fn undersized_policy_reports_extent_and_preserves_every_frame() {
    let mut f = fixture();
    let mut c = context(&f, 100.0, 50.0);
    c.minimums.insert(f.a, size(80.0, 20.0));
    c.minimums.insert(f.b, size(80.0, 20.0));
    assert_eq!(
        reject(
            &mut f.core,
            Command::SetGeometryContext {
                workspace: f.w,
                context: c.clone()
            }
        ),
        Error::Geometry(GeometryError::Infeasible)
    );
    c.overflow = Overflow::Scrollable;
    configure(&mut f, c.clone());
    let g = f.core.snapshot().geometry(f.w).unwrap();
    assert_eq!(g.required_extent.width(), 160.0);
    assert_eq!(g.virtual_extent.width(), 160.0);
    assert_eq!(g.frames.len(), 2);
    assert_eq!(g.clips[&f.b].width(), 20.0);
    c.viewport = rect(0.0, 0.0, 300.0, 80.0);
    c.overflow = Overflow::Reject;
    configure(&mut f, c);
    let g = f.core.snapshot().geometry(f.w).unwrap();
    assert_eq!(g.frames.len(), 2);
    assert_eq!(g.clips, g.frames);
    let mut zero = context(&f, 0.0, 0.0);
    zero.allowed_docks.clear();
    let mut fresh = fixture();
    zero.allowed_docks.clear();
    configure(&mut fresh, zero);
    assert!(fresh
        .core
        .snapshot()
        .geometry(fresh.w)
        .unwrap()
        .frames
        .values()
        .all(|r| r.width() == 0.0 && r.height() == 0.0));
}

#[test]
fn tool_target_focus_visibility_and_release_retain_are_independent() {
    let mut f = fixture();
    let c = context(&f, 300.0, 150.0);
    configure(&mut f, c);
    let origin = ToolPlacement::Edge {
        anchor: ToolAnchor::View(f.va),
        edge: Edge::Top,
    };
    let spec = tool_spec(&f, origin, size(20.0, 10.0));
    let id = tool(&mut f.core, spec);
    let original = f.core.snapshot().tools[&id].clone();
    let outward = ToolPlacement::Edge {
        anchor: ToolAnchor::Workspace(f.w),
        edge: Edge::Top,
    };
    apply(
        &mut f.core,
        Command::Dock(DockIntent::Tool {
            tool: id,
            placement: outward,
        }),
    );
    let record = &f.core.snapshot().tools[&id];
    assert_eq!(record.target, original.target);
    assert_eq!(record.origin, original.origin);
    assert_eq!(record.owner, original.owner);
    let mut state = record.state;
    state.keyboard_focused = true;
    apply(&mut f.core, Command::SetToolState { tool: id, state });
    apply(&mut f.core, Command::SelectView { view: f.vb });
    assert!(f.core.snapshot().tools[&id].presentation().visible);
    assert_eq!(f.core.snapshot().tools[&id].target, original.target);
    assert_eq!(f.core.snapshot().geometry(f.w).unwrap().content.y(), 10.0);
    state.target_visible = false;
    apply(&mut f.core, Command::SetToolState { tool: id, state });
    assert_eq!(f.core.snapshot().geometry(f.w).unwrap().content.y(), 0.0);
    let mut policy = f.core.snapshot().tools[&id].policy.clone();
    policy.hidden_space = HiddenSpace::Retain;
    apply(&mut f.core, Command::ConfigureTool { tool: id, policy });
    assert_eq!(f.core.snapshot().geometry(f.w).unwrap().content.y(), 10.0);
    state.target_visible = true;
    apply(&mut f.core, Command::SetToolState { tool: id, state });
    assert_eq!(f.core.snapshot().tools[&id].placement, outward);
    let incompatible = EditingTarget {
        view: f.vb,
        resource: ResourceId::from_raw(999),
    };
    assert_eq!(
        reject(
            &mut f.core,
            Command::RebindTool {
                tool: id,
                target: incompatible
            }
        ),
        Error::IncompatibleTarget
    );
    let compatible = target(&f, f.vb);
    apply(
        &mut f.core,
        Command::RebindTool {
            tool: id,
            target: compatible,
        },
    );
    assert_eq!(f.core.snapshot().tools[&id].target, compatible);
    assert_eq!(f.core.snapshot().tools[&id].origin, original.origin);
}

#[test]
fn multiple_tool_strips_are_ordered_and_grid_conflicts_are_atomic() {
    let mut f = fixture();
    apply(
        &mut f.core,
        Command::SetLayout {
            workspace: f.w,
            plan: grid_plan(f.a, f.b),
        },
    );
    let c = context(&f, 300.0, 150.0);
    configure(&mut f, c);
    let top = ToolPlacement::Edge {
        anchor: ToolAnchor::Workspace(f.w),
        edge: Edge::Top,
    };
    let mut first = tool_spec(&f, top, size(20.0, 10.0));
    first.policy.order = 5;
    let a = tool(&mut f.core, first);
    let mut second = tool_spec(&f, top, size(20.0, 15.0));
    second.policy.order = -1;
    let b = tool(&mut f.core, second);
    let left = ToolPlacement::Edge {
        anchor: ToolAnchor::Workspace(f.w),
        edge: Edge::Left,
    };
    let spec = tool_spec(&f, left, size(12.0, 20.0));
    let v = tool(&mut f.core, spec);
    let g = f.core.snapshot().geometry(f.w).unwrap();
    assert_eq!(g.tools[&b].y(), 0.0);
    assert_eq!(g.tools[&a].y(), 15.0);
    assert_eq!(g.tools[&v], rect(0.0, 25.0, 12.0, 125.0));
    assert_eq!(g.content, rect(12.0, 25.0, 288.0, 125.0));
    let occupied = ToolPlacement::Cell {
        workspace: f.w,
        grid: NodeId(3),
        row: 0,
        column: 0,
    };
    let mut policy = f.core.snapshot().tools[&a].policy.clone();
    policy.allowed.push(occupied);
    let hole = ToolPlacement::Cell {
        workspace: f.w,
        grid: NodeId(3),
        row: 0,
        column: 1,
    };
    policy.allowed.push(hole);
    apply(&mut f.core, Command::ConfigureTool { tool: a, policy });
    assert!(matches!(
        reject(
            &mut f.core,
            Command::Dock(DockIntent::Tool {
                tool: a,
                placement: occupied
            })
        ),
        Error::Geometry(GeometryError::InvalidPlan(_))
    ));
    apply(
        &mut f.core,
        Command::Dock(DockIntent::Tool {
            tool: a,
            placement: hole,
        }),
    );
    assert_eq!(f.core.snapshot().tools[&a].placement, hole);
    assert_eq!(f.core.snapshot().geometry(f.w).unwrap().frames.len(), 2);
    assert_eq!(
        reject(
            &mut f.core,
            Command::Dock(DockIntent::Tool {
                tool: b,
                placement: ToolPlacement::Edge {
                    anchor: ToolAnchor::Workspace(f.w),
                    edge: Edge::Right
                }
            })
        ),
        Error::ForbiddenDestination
    );
}

#[test]
fn pure_preview_cancel_stale_commit_and_retained_commit_replay() {
    let mut f = fixture();
    let c = context(&f, 300.0, 100.0);
    configure(&mut f, c);
    let intent = DockIntent::Frame {
        frame: f.b,
        location: DockLocation::Beside {
            frame: f.a,
            edge: Edge::Right,
        },
    };
    let before = f.core.snapshot().clone();
    let ledger = f.core.ledger_status();
    let preview = f.core.preview(intent.clone()).unwrap();
    let predicted = preview.geometry()[0].frames.clone();
    preview.clone().cancel();
    assert_eq!(f.core.snapshot(), &before);
    assert_eq!(f.core.ledger_status(), ledger);
    assert!(f
        .core
        .preview(DockIntent::Frame {
            frame: f.b,
            location: DockLocation::WorkspaceEdge {
                workspace: f.w,
                edge: Edge::Right
            }
        })
        .is_err());
    assert_eq!(f.core.snapshot(), &before);
    assert_eq!(f.core.ledger_status(), ledger);
    let e = envelope(&f.core, Command::CommitPreview(preview.clone()));
    let response = f.core.execute(e.clone());
    assert!(response.outcome.result.is_ok());
    assert_eq!(f.core.snapshot().geometry(f.w).unwrap().frames, predicted);
    let committed = f.core.snapshot().clone();
    assert!(f.core.execute(e).replayed);
    assert_eq!(f.core.snapshot(), &committed);
    for change in 0..4 {
        let preview = f.core.preview(intent.clone()).unwrap();
        let mut c = f.core.snapshot().contexts[&f.w].clone();
        match change {
            0 => c.viewport = rect(0.0, 0.0, 301.0, 100.0),
            1 => {
                c.minimums.insert(f.a, size(2.0, 2.0));
            }
            2 => c.gap = s(1.0),
            _ => c.allowed_docks.push(DockLocation::WorkspaceEdge {
                workspace: f.w,
                edge: Edge::Bottom,
            }),
        }
        configure(&mut f, c);
        assert!(matches!(
            reject(&mut f.core, Command::CommitPreview(preview)),
            Error::StalePreview { .. }
        ));
    }
    let preview = f.core.preview(intent).unwrap();
    frame(&mut f.core, f.w);
    assert!(matches!(
        reject(&mut f.core, Command::CommitPreview(preview)),
        Error::StalePreview { .. }
    ));
}

#[test]
fn in_app_float_redock_and_eligible_snap_preserve_identity() {
    let mut f = fixture();
    let c = context(&f, 300.0, 100.0);
    configure(&mut f, c);
    let before = f.core.snapshot().clone();
    assert_eq!(
        reject(
            &mut f.core,
            Command::Dock(DockIntent::Float {
                frame: f.b,
                rect: rect(290.0, 0.0, 20.0, 20.0)
            })
        ),
        Error::Geometry(GeometryError::OutsideViewport)
    );
    apply(
        &mut f.core,
        Command::Dock(DockIntent::Float {
            frame: f.b,
            rect: rect(100.0, 20.0, 80.0, 60.0),
        }),
    );
    assert_eq!(f.core.snapshot().floats[&f.b].kind, SurfaceKind::InAppFloat);
    assert_eq!(f.core.snapshot().frames[&f.b].workspace, f.w);
    assert_eq!(f.core.snapshot().views, before.views);
    assert_eq!(f.core.snapshot().geometry(f.w).unwrap().frames.len(), 2);
    apply(
        &mut f.core,
        Command::Dock(DockIntent::Redock { frame: f.b }),
    );
    assert!(f.core.snapshot().floats.is_empty());
    assert_eq!(f.core.snapshot().views, before.views);
    let candidates = f
        .core
        .snapshot()
        .snap_candidates(f.w, s(0.0), s(0.0), s(1.0))
        .unwrap();
    assert!(!candidates.is_empty());
    assert!(candidates.iter().all(|c| f.core.snapshot().contexts[&f.w]
        .allowed_docks
        .contains(&c.location)));
    assert!(candidates.iter().all(|c| !matches!(
        c.location,
        DockLocation::WorkspaceEdge {
            edge: Edge::Right,
            ..
        }
    )));
    let again = f
        .core
        .snapshot()
        .snap_candidates(f.w, s(0.0), s(0.0), s(1.0))
        .unwrap();
    assert_eq!(again, candidates);
}

#[test]
fn metadata_budget_lowering_and_tool_entity_budget_are_disclosed() {
    let mut f = fixture();
    let plan = split_plan(f.a, f.b);
    let mut limits = f.core.snapshot().limits;
    limits.max_layout_entries = Some(6);
    apply(&mut f.core, Command::SetLimits(limits));
    assert_eq!(
        reject(
            &mut f.core,
            Command::SetLayout {
                workspace: f.w,
                plan: plan.clone()
            }
        ),
        Error::Admission {
            budget: Budget::LayoutEntries,
            limit: 6,
            attempted: 7
        }
    );
    limits.max_layout_entries = Some(7);
    apply(&mut f.core, Command::SetLimits(limits));
    apply(
        &mut f.core,
        Command::SetLayout {
            workspace: f.w,
            plan,
        },
    );
    assert_eq!(f.core.snapshot().layout_entry_count(), 7);
    limits.max_layout_entries = Some(1);
    apply(&mut f.core, Command::SetLimits(limits));
    let before = f.core.snapshot().clone();
    apply(
        &mut f.core,
        Command::SetLayout {
            workspace: f.w,
            plan: before.layouts[&f.w].clone(),
        },
    );
    assert_eq!(f.core.snapshot(), &before);
    let e = reject(
        &mut f.core,
        Command::CreateFrame {
            workspace: f.w,
            index: 2,
        },
    );
    assert!(matches!(
        e,
        Error::Admission {
            budget: Budget::LayoutEntries,
            ..
        }
    ));
    limits.max_layout_entries = Some(65_536);
    apply(&mut f.core, Command::SetLimits(limits));
    let c = context(&f, 300.0, 100.0);
    configure(&mut f, c);
    limits.max_records = Some(f.core.snapshot().record_count());
    apply(&mut f.core, Command::SetLimits(limits));
    let spec = tool_spec(
        &f,
        ToolPlacement::Edge {
            anchor: ToolAnchor::Workspace(f.w),
            edge: Edge::Top,
        },
        size(20.0, 10.0),
    );
    assert!(matches!(
        reject(&mut f.core, Command::CreateTool(spec)),
        Error::Admission {
            budget: Budget::Records,
            ..
        }
    ));
    assert_eq!(Limits::default().max_layout_entries, Some(65_536));
}

#[test]
fn floating_uses_an_eligible_return_and_requires_a_return_path() {
    let mut f = fixture();
    let location = DockLocation::WorkspaceEdge {
        workspace: f.w,
        edge: Edge::Left,
    };
    let mut c = context(&f, 300.0, 100.0);
    c.allowed_docks = vec![location];
    configure(&mut f, c.clone());
    apply(
        &mut f.core,
        Command::Dock(DockIntent::Float {
            frame: f.b,
            rect: rect(100.0, 20.0, 80.0, 60.0),
        }),
    );
    assert_eq!(f.core.snapshot().floats[&f.b].return_location, location);
    apply(
        &mut f.core,
        Command::Dock(DockIntent::Redock { frame: f.b }),
    );
    assert!(f.core.snapshot().floats.is_empty());
    c.allowed_docks.clear();
    configure(&mut f, c);
    assert_eq!(
        reject(
            &mut f.core,
            Command::Dock(DockIntent::Float {
                frame: f.b,
                rect: rect(100.0, 20.0, 80.0, 60.0)
            })
        ),
        Error::ForbiddenDestination
    );
}

#[test]
fn previews_preserve_entity_allocation_and_tool_policy_stales_tokens() {
    let mut f = fixture();
    let mut control = fixture();
    let identity = f.core.snapshot().identity.clone();
    let control_identity = control.core.snapshot().identity.clone();
    let c = context(&f, 300.0, 150.0);
    configure(&mut f, c.clone());
    configure(&mut control, c);
    let location = DockLocation::Beside {
        frame: f.a,
        edge: Edge::Right,
    };
    for _ in 0..8 {
        f.core
            .preview(DockIntent::Frame {
                frame: f.b,
                location,
            })
            .unwrap()
            .cancel();
    }
    let id = create(
        &mut f.core,
        Command::RegisterProvider {
            key: "preview-control".into(),
            supports_shared_views: false,
        },
    );
    let control_id = create(
        &mut control.core,
        Command::RegisterProvider {
            key: "preview-control".into(),
            supports_shared_views: false,
        },
    );
    assert_eq!(id, control_id);
    assert_eq!(f.core.snapshot().identity, identity);
    assert_eq!(control.core.snapshot().identity, control_identity);
    assert_eq!(identity.lineage, control_identity.lineage);
    assert_ne!(identity.incarnation, control_identity.incarnation);
    let mut expected = control.core.snapshot().clone();
    expected.identity = identity;
    assert_eq!(f.core.snapshot(), &expected);
    let placement = ToolPlacement::Edge {
        anchor: ToolAnchor::Workspace(f.w),
        edge: Edge::Top,
    };
    let spec = tool_spec(&f, placement, size(20.0, 10.0));
    let id = tool(&mut f.core, spec);
    let before = f.core.snapshot().clone();
    let ledger = f.core.ledger_status();
    let preview = f
        .core
        .preview(DockIntent::Tool {
            tool: id,
            placement,
        })
        .unwrap();
    assert_eq!(f.core.snapshot(), &before);
    assert_eq!(f.core.ledger_status(), ledger);
    let mut policy = f.core.snapshot().tools[&id].policy.clone();
    policy.order = 1;
    apply(&mut f.core, Command::ConfigureTool { tool: id, policy });
    assert!(matches!(
        reject(&mut f.core, Command::CommitPreview(preview)),
        Error::StalePreview { .. }
    ));
}
