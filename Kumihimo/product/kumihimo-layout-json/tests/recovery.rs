use kumihimo_core::*;
use kumihimo_layout_json::{self as wire, CodecOptions};

fn core() -> Core {
    Core::new(
        Identity::new("test-layout", "live-1").unwrap(),
        Limits::default(),
        32,
    )
    .unwrap()
}
fn run(c: &mut Core, command: Command) -> Result<Change, Error> {
    c.execute(Envelope {
        identity: c.snapshot().identity.clone(),
        actor: "test".into(),
        operation: c.ledger_status().high_watermark.checked_next().unwrap(),
        expected_generation: c.snapshot().generation,
        command,
    })
    .outcome
    .result
}
fn created(c: &mut Core, cmd: Command) -> EntityId {
    match run(c, cmd).unwrap() {
        Change::Created(id) => id,
        _ => panic!("expected entity"),
    }
}
fn populated() -> (Core, WorkspaceId, FrameId, ViewId) {
    let mut c = core();
    let EntityId::Workspace(w) = created(&mut c, Command::CreateWorkspace { owner: None }) else {
        unreachable!()
    };
    let EntityId::Frame(f) = created(
        &mut c,
        Command::CreateFrame {
            workspace: w,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Provider(p) = created(
        &mut c,
        Command::RegisterProvider {
            key: "test-provider".into(),
            supports_shared_views: true,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Resource(r) = created(
        &mut c,
        Command::RegisterResource {
            provider: p,
            key: "test-document".into(),
            restore_reference: "opaque-reference".into(),
        },
    ) else {
        unreachable!()
    };
    let EntityId::View(v) = created(
        &mut c,
        Command::CreateView {
            frame: f,
            resource: r,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    (c, w, f, v)
}

#[test]
fn real_wire_and_fresh_identity_keep_semantics_reset_runtime() {
    let (mut c, _, _, v) = populated();
    run(
        &mut c,
        Command::SetViewMetadata {
            view: v,
            label: "Document".into(),
            pinned: false,
            unsaved: true,
        },
    )
    .unwrap();
    run(
        &mut c,
        Command::SetViewState {
            view: v,
            state: ViewState {
                visible: true,
                usable: true,
            },
        },
    )
    .unwrap();
    let dto = c.save_layout();
    let bytes = wire::encode(&dto).unwrap();
    assert_eq!(wire::decode(&bytes).unwrap(), dto);
    let restored =
        Core::from_persisted(dto, "fresh-2".into(), RestoreOptions::default(), 8).unwrap();
    assert_eq!(restored.snapshot().views[&v].label, "Document");
    assert!(restored.snapshot().views[&v].unsaved);
    assert_eq!(restored.snapshot().views[&v].state, ViewState::default());
    assert_eq!(
        restored
            .snapshot()
            .providers
            .values()
            .next()
            .unwrap()
            .availability,
        ProviderAvailability::Missing
    );
    assert_eq!(restored.snapshot().generation, 1);
    assert_eq!(restored.ledger_status().high_watermark.sequence(), 0);
    assert_ne!(restored.snapshot().identity, c.snapshot().identity);
}

#[test]
fn older_live_restore_omits_newer_frames_preserves_ledger_allocator() {
    let (mut c, w, f, _) = populated();
    run(
        &mut c,
        Command::SetGeometryContext {
            workspace: w,
            context: GeometryContext::new(Rect::new(0., 0., 800., 600.).unwrap()),
        },
    )
    .unwrap();
    let saved = c.save_layout();
    let EntityId::Frame(later) = created(
        &mut c,
        Command::CreateFrame {
            workspace: w,
            index: 1,
        },
    ) else {
        unreachable!()
    };
    let gen = c.snapshot().generation;
    let token = c.prepare_restore(saved, RestoreOptions::default()).unwrap();
    run(&mut c, Command::Restore(token)).unwrap();
    assert!(c.snapshot().frames.contains_key(&f));
    assert!(!c.snapshot().frames.contains_key(&later));
    assert_eq!(c.snapshot().generation, gen + 1);
    let EntityId::Frame(next) = created(
        &mut c,
        Command::CreateFrame {
            workspace: w,
            index: 1,
        },
    ) else {
        unreachable!()
    };
    assert!(next.raw() > later.raw());
}

#[test]
fn source_target_conflicts_and_immutable_aliases_reject_atomically() {
    let (mut c, _, _, v) = populated();
    let saved = c.save_layout();
    let token = c
        .prepare_restore(saved.clone(), RestoreOptions::default())
        .unwrap();
    run(
        &mut c,
        Command::SetViewMetadata {
            view: v,
            label: "new".into(),
            pinned: false,
            unsaved: false,
        },
    )
    .unwrap();
    let before = c.snapshot().clone();
    assert_eq!(
        run(&mut c, Command::Restore(token)),
        Err(Error::RestoreConflict)
    );
    assert_eq!(c.snapshot(), &before);
    let mut alias = saved.clone();
    alias.resources[0].key = "different".into();
    assert!(c.prepare_restore(alias, RestoreOptions::default()).is_err());
    let mut duplicate = saved.clone();
    duplicate.views[0].id = ViewId::from_raw(duplicate.frames[0].id.raw());
    assert!(c
        .prepare_restore(duplicate, RestoreOptions::default())
        .is_err());
    let mut cycle = saved;
    cycle.workspaces[0].owner = Some(cycle.frames[0].id);
    cycle.frames[0].child_workspace = Some(cycle.workspaces[0].id);
    assert!(c.prepare_restore(cycle, RestoreOptions::default()).is_err());
    assert_eq!(c.snapshot(), &before);
}

#[test]
fn wire_rejects_duplicates_unknown_trailing_schema_and_limits() {
    let c = core();
    let dto = c.save_layout();
    let bytes = wire::encode(&dto).unwrap();
    assert!(wire::decode_with_options(
        &bytes,
        CodecOptions {
            max_bytes: bytes.len() - 1
        }
    )
    .is_err());
    assert!(wire::encode_with_options(
        &dto,
        CodecOptions {
            max_bytes: bytes.len() - 1
        }
    )
    .is_err());
    let text = String::from_utf8(bytes).unwrap();
    let duplicate = text.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1);
    assert!(wire::decode(duplicate.as_bytes()).is_err());
    let nested = text.replacen(
        "\"lineage\":\"test-layout\"",
        "\"lineage\":\"test-layout\",\"lineage\":\"test-layout\"",
        1,
    );
    assert!(wire::decode(nested.as_bytes()).is_err());
    let unknown = text.replacen("{", "{\"mystery\":true,", 1);
    assert!(wire::decode(unknown.as_bytes()).is_err());
    assert!(wire::decode(format!("{text} null").as_bytes()).is_err());
    for schema in [0, 2, u32::MAX] {
        let modified = text.replacen("\"schema\":1", &format!("\"schema\":{schema}"), 1);
        assert!(matches!(
            wire::decode(modified.as_bytes()),
            Err(wire::Error::UnsupportedSchema { .. })
        ));
    }
    for id in ["01", "+1", "-1", "18446744073709551616"] {
        let bad = text.replacen(
            "\"next_entity\":\"1\"",
            &format!("\"next_entity\":\"{id}\""),
            1,
        );
        assert!(wire::decode(bad.as_bytes()).is_err());
    }
}

#[test]
fn exact_u64_allocator_sentinel_and_stamp_are_observable() {
    let mut c = core();
    let mut dto = c.save_layout();
    dto.next_entity = u64::MAX;
    dto.source_generation = u64::MAX;
    let bytes = wire::encode(&dto).unwrap();
    assert!(String::from_utf8(bytes.clone())
        .unwrap()
        .contains("\"18446744073709551615\""));
    let mut fresh = Core::from_persisted(
        wire::decode(&bytes).unwrap(),
        "fresh-2".into(),
        Default::default(),
        2,
    )
    .unwrap();
    assert_eq!(
        run(&mut fresh, Command::CreateWorkspace { owner: None }),
        Err(Error::Exhausted(Counter::EntityId))
    );
    let stamped = wire::encode_core(&c, Default::default()).unwrap();
    assert!(stamped.is_current(&c));
    run(&mut c, Command::CreateWorkspace { owner: None }).unwrap();
    assert!(!stamped.is_current(&c));
}

#[test]
fn foreign_restore_and_old_envelopes_consume_no_ledger() {
    let (c, _, _, _) = populated();
    let token = c
        .prepare_restore(c.save_layout(), Default::default())
        .unwrap();
    let mut fresh =
        Core::from_persisted(c.save_layout(), "fresh-2".into(), Default::default(), 8).unwrap();
    let before = fresh.snapshot().clone();
    let ledger = fresh.ledger_status();
    let foreign = Envelope {
        identity: c.snapshot().identity.clone(),
        actor: "old".into(),
        operation: OperationId::new(1),
        expected_generation: before.generation,
        command: Command::CreateWorkspace { owner: None },
    };
    assert_eq!(
        fresh.execute(foreign).outcome.result,
        Err(Error::IdentityMismatch)
    );
    let embedded = Envelope {
        identity: before.identity.clone(),
        actor: "old".into(),
        operation: OperationId::new(1),
        expected_generation: before.generation,
        command: Command::Restore(token),
    };
    assert_eq!(
        fresh.execute(embedded).outcome.result,
        Err(Error::IdentityMismatch)
    );
    assert_eq!(fresh.snapshot(), &before);
    assert_eq!(fresh.ledger_status(), ledger);
}

#[test]
fn explicit_float_repair_and_host_admission_are_atomic() {
    let (mut c, w, f, _) = populated();
    let mut context = GeometryContext::new(Rect::new(0., 0., 800., 600.).unwrap());
    context.allow_floating = true;
    context
        .allowed_docks
        .push(DockLocation::Append { workspace: w });
    run(
        &mut c,
        Command::SetGeometryContext {
            workspace: w,
            context,
        },
    )
    .unwrap();
    run(
        &mut c,
        Command::Dock(DockIntent::Float {
            frame: f,
            rect: Rect::new(400., 200., 300., 200.).unwrap(),
        }),
    )
    .unwrap();
    let saved = c.save_layout();
    let before = c.snapshot().clone();
    let bounds = vec![RestoreBounds {
        workspace: w,
        viewport: Rect::new(0., 0., 200., 100.).unwrap(),
    }];
    let options = RestoreOptions {
        bounds: bounds.clone(),
        ..Default::default()
    };
    assert!(c.prepare_restore(saved.clone(), options).is_err());
    assert_eq!(c.snapshot(), &before);
    let options = RestoreOptions {
        bounds,
        float_repair: FloatRepair::Clamp,
        ..Default::default()
    };
    let token = c.prepare_restore(saved.clone(), options).unwrap();
    run(&mut c, Command::Restore(token)).unwrap();
    assert!(c.snapshot().contexts[&w]
        .viewport
        .contains(c.snapshot().floats[&f].rect));
    let options = RestoreOptions {
        limits: Limits {
            max_records: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(Core::from_persisted(saved, "fresh-2".into(), options, 0).is_err());
}

#[test]
fn nested_group_closed_tool_float_and_max_projection_roundtrip() {
    let (mut c, w, f, v) = populated();
    let EntityId::Frame(sibling) = created(
        &mut c,
        Command::CreateFrame {
            workspace: w,
            index: 1,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Workspace(child) = created(&mut c, Command::CreateWorkspace { owner: Some(f) })
    else {
        unreachable!()
    };
    let EntityId::Frame(_child_frame) = created(
        &mut c,
        Command::CreateFrame {
            workspace: child,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Group(g) = created(
        &mut c,
        Command::CreateGroup {
            frame: f,
            label: "Editors".into(),
            color: Some("blue".into()),
        },
    ) else {
        unreachable!()
    };
    run(
        &mut c,
        Command::AssignGroup {
            view: v,
            group: Some(g),
        },
    )
    .unwrap();
    run(
        &mut c,
        Command::ConfigureGroup {
            group: g,
            label: "Editors".into(),
            color: Some("blue".into()),
            description: "Named group".into(),
            collapsed: true,
        },
    )
    .unwrap();
    let mut context = GeometryContext::new(Rect::new(0., 0., 1000., 800.).unwrap());
    context.allow_floating = true;
    context
        .allowed_docks
        .push(DockLocation::Append { workspace: w });
    context.frame_weights.insert(f, Scalar::new(3.).unwrap());
    run(
        &mut c,
        Command::SetGeometryContext {
            workspace: w,
            context,
        },
    )
    .unwrap();
    let r = c.snapshot().views[&v].resource;
    let p = c.snapshot().resources[&r].provider;
    let placement = ToolPlacement::Edge {
        anchor: ToolAnchor::View(v),
        edge: Edge::Top,
    };
    let mut policy = ToolPolicy::new(
        Size::new(100., 20.).unwrap(),
        Size::new(10., 10.).unwrap(),
        vec![placement],
    );
    policy.hidden_space = HiddenSpace::Retain;
    let EntityId::Tool(t) = created(
        &mut c,
        Command::CreateTool(ToolSpec {
            owner: p,
            origin: v,
            target: EditingTarget {
                view: v,
                resource: r,
            },
            placement,
            policy,
            state: TargetState {
                target_visible: true,
                target_usable: true,
                origin_visible: true,
                origin_usable: true,
                applicable: true,
                keyboard_focused: true,
            },
        }),
    ) else {
        unreachable!()
    };
    run(
        &mut c,
        Command::CloseView {
            view: v,
            approved: true,
        },
    )
    .unwrap();
    run(
        &mut c,
        Command::Dock(DockIntent::Float {
            frame: sibling,
            rect: Rect::new(200., 100., 300., 250.).unwrap(),
        }),
    )
    .unwrap();
    run(&mut c, Command::Maximize { frame: f }).unwrap();
    let saved = c.save_layout();
    let dto = wire::decode(&wire::encode(&saved).unwrap()).unwrap();
    assert_eq!(dto, saved);
    let options = RestoreOptions {
        providers: vec![ProviderResolution {
            key: "test-provider".into(),
            supports_shared_views: true,
        }],
        ..Default::default()
    };
    let restored = Core::from_persisted(dto, "fresh-2".into(), options, 16).unwrap();
    assert_eq!(restored.snapshot().frames[&f].child_workspace, Some(child));
    assert_eq!(restored.snapshot().groups[&g].description, "Named group");
    assert!(restored.snapshot().groups[&g].collapsed);
    assert!(restored.snapshot().views[&v].closed.is_some());
    assert_eq!(restored.snapshot().maximized[&w], f);
    assert_eq!(
        restored.snapshot().floats[&sibling],
        c.snapshot().floats[&sibling]
    );
    let presentation = restored
        .snapshot()
        .effective_tool_presentation(&restored.snapshot().tools[&t]);
    assert!(!presentation.usable);
    assert!(!presentation.applicable);
    assert!(!presentation.reserves_space);
    assert_eq!(restored.snapshot().tools[&t].state, TargetState::default());
}

#[test]
fn malformed_record_ids_and_constructor_geometry_reject() {
    let (mut c, w, _, _) = populated();
    run(
        &mut c,
        Command::SetGeometryContext {
            workspace: w,
            context: GeometryContext::new(Rect::new(0., 0., 100., 100.).unwrap()),
        },
    )
    .unwrap();
    let text = String::from_utf8(wire::encode(&c.save_layout()).unwrap()).unwrap();
    for value in ["0", "18446744073709551615", "01"] {
        let bad = text.replacen("\"id\":\"1\"", &format!("\"id\":\"{value}\""), 1);
        assert!(wire::decode(bad.as_bytes()).is_err());
    }
    let bad = text.replacen("\"width\":100.0", "\"width\":-1.0", 1);
    assert_ne!(bad, text);
    assert!(wire::decode(bad.as_bytes()).is_err());
    let nonfinite = text.replacen("\"gap\":0.0", "\"gap\":1e999", 1);
    assert_ne!(nonfinite, text);
    assert!(wire::decode(nonfinite.as_bytes()).is_err());
    let deep = format!("{}0{}", "[".repeat(150), "]".repeat(150));
    assert!(wire::decode(deep.as_bytes()).is_err());
}

#[test]
fn live_restore_grandfathers_only_matching_current_ceilings() {
    let (mut c, w, f, v) = populated();
    let resource = c.snapshot().views[&v].resource;
    let EntityId::View(second) = created(
        &mut c,
        Command::CreateView {
            frame: f,
            resource,
            index: 1,
        },
    ) else {
        unreachable!()
    };
    run(
        &mut c,
        Command::SetViewMetadata {
            view: v,
            label: "long-label".into(),
            pinned: false,
            unsaved: false,
        },
    )
    .unwrap();
    run(
        &mut c,
        Command::SetGeometryContext {
            workspace: w,
            context: GeometryContext::new(Rect::new(0., 0., 500., 400.).unwrap()),
        },
    )
    .unwrap();
    let low = Limits {
        max_records: Some(1),
        max_depth: Some(0),
        max_views_per_frame: Some(0),
        max_layout_entries: Some(0),
        max_label_bytes: Some(3),
        max_key_bytes: Some(4),
        ..Default::default()
    };
    run(&mut c, Command::SetLimits(low)).unwrap();
    let saved = wire::decode(&wire::encode(&c.save_layout()).unwrap()).unwrap();
    let options = RestoreOptions {
        limits: low,
        ..Default::default()
    };
    let token = c.prepare_restore(saved.clone(), options.clone()).unwrap();
    run(&mut c, Command::Restore(token)).unwrap();
    assert_eq!(c.snapshot().limits, low);
    assert!(Core::from_persisted(saved.clone(), "fresh-2".into(), options.clone(), 0).is_err());
    let mut decreasing = saved.clone();
    decreasing.views.retain(|view| view.id != second);
    decreasing
        .frames
        .iter_mut()
        .find(|frame| frame.id == f)
        .unwrap()
        .views
        .retain(|id| *id != second);
    let token = c.prepare_restore(decreasing, options.clone()).unwrap();
    run(&mut c, Command::Restore(token)).unwrap();
    assert_eq!(c.snapshot().frames[&f].views, vec![v]);
    let before = c.snapshot().clone();
    assert!(matches!(
        c.prepare_restore(saved.clone(), options.clone()),
        Err(Error::Admission {
            budget: Budget::Records,
            ..
        })
    ));
    let mut stricter = c.save_layout();
    stricter.limits.max_records = Some(0);
    assert!(matches!(
        c.prepare_restore(stricter, options.clone()),
        Err(Error::Admission {
            budget: Budget::Records,
            ..
        })
    ));
    let stricter_options = RestoreOptions {
        limits: Limits {
            max_key_bytes: Some(3),
            ..low
        },
        ..Default::default()
    };
    assert!(matches!(
        c.prepare_restore(c.save_layout(), stricter_options),
        Err(Error::Admission {
            budget: Budget::KeyBytes,
            ..
        })
    ));
    assert_eq!(c.snapshot(), &before);
}

#[test]
fn live_restore_string_allowance_stays_with_entity_and_field() {
    let (mut c, _, f, v) = populated();
    let r = c.snapshot().views[&v].resource;
    let EntityId::View(other) = created(
        &mut c,
        Command::CreateView {
            frame: f,
            resource: r,
            index: 1,
        },
    ) else {
        unreachable!()
    };
    run(
        &mut c,
        Command::SetViewMetadata {
            view: v,
            label: "long-label".into(),
            pinned: false,
            unsaved: false,
        },
    )
    .unwrap();
    let EntityId::Group(group) = created(
        &mut c,
        Command::CreateGroup {
            frame: f,
            label: "long-group-label".into(),
            color: None,
        },
    ) else {
        unreachable!()
    };
    let low = Limits {
        max_label_bytes: Some(3),
        ..Default::default()
    };
    run(&mut c, Command::SetLimits(low)).unwrap();
    let options = RestoreOptions {
        limits: low,
        ..Default::default()
    };
    let mut same = c.save_layout();
    same.views
        .iter_mut()
        .find(|view| view.id == v)
        .unwrap()
        .label = "another".into();
    assert!(c.prepare_restore(same, options.clone()).is_ok());
    let mut growing = c.save_layout();
    growing
        .views
        .iter_mut()
        .find(|view| view.id == v)
        .unwrap()
        .label = "longer-than-current-label".into();
    assert!(matches!(
        c.prepare_restore(growing, options.clone()),
        Err(Error::Admission {
            budget: Budget::LabelBytes,
            ..
        })
    ));
    let mut borrowed = c.save_layout();
    borrowed
        .views
        .iter_mut()
        .find(|view| view.id == other)
        .unwrap()
        .label = "long".into();
    assert!(matches!(
        c.prepare_restore(borrowed, options.clone()),
        Err(Error::Admission {
            budget: Budget::LabelBytes,
            ..
        })
    ));
    let mut cross_field = c.save_layout();
    cross_field
        .groups
        .iter_mut()
        .find(|g| g.id == group)
        .unwrap()
        .description = "long".into();
    assert!(matches!(
        c.prepare_restore(cross_field, options),
        Err(Error::Admission {
            budget: Budget::LabelBytes,
            ..
        })
    ));
}

#[test]
fn live_restore_per_frame_and_depth_allowances_cannot_be_borrowed() {
    let (mut c, w, f, v) = populated();
    let r = c.snapshot().views[&v].resource;
    let EntityId::View(second) = created(
        &mut c,
        Command::CreateView {
            frame: f,
            resource: r,
            index: 1,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Frame(empty) = created(
        &mut c,
        Command::CreateFrame {
            workspace: w,
            index: 1,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Workspace(child) = created(&mut c, Command::CreateWorkspace { owner: Some(f) })
    else {
        unreachable!()
    };
    let EntityId::Frame(deep) = created(
        &mut c,
        Command::CreateFrame {
            workspace: child,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    let low = Limits {
        max_views_per_frame: Some(0),
        max_depth: Some(0),
        ..Default::default()
    };
    run(&mut c, Command::SetLimits(low)).unwrap();
    let options = RestoreOptions {
        limits: low,
        ..Default::default()
    };
    assert!(c.prepare_restore(c.save_layout(), options.clone()).is_ok());
    let mut tabs = c.save_layout();
    let source = tabs.frames.iter_mut().find(|frame| frame.id == f).unwrap();
    source.views.clear();
    source.selected_view = None;
    let destination = tabs
        .frames
        .iter_mut()
        .find(|frame| frame.id == empty)
        .unwrap();
    destination.views = vec![v, second];
    destination.selected_view = Some(v);
    for view in &mut tabs.views {
        view.frame = empty;
    }
    assert!(matches!(
        c.prepare_restore(tabs, options.clone()),
        Err(Error::Admission {
            budget: Budget::ViewsPerFrame,
            ..
        })
    ));
    let mut deeper = c.save_layout();
    deeper
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == w)
        .unwrap()
        .frames
        .retain(|frame| *frame != empty);
    deeper
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == child)
        .unwrap()
        .frames
        .push(empty);
    deeper
        .frames
        .iter_mut()
        .find(|frame| frame.id == empty)
        .unwrap()
        .workspace = child;
    assert!(matches!(
        c.prepare_restore(deeper, options.clone()),
        Err(Error::Admission {
            budget: Budget::Depth,
            ..
        })
    ));
    // A new entity cannot inherit the deeper existing frame's over-limit admission.
    let mut new_frame = c.save_layout();
    let raw = new_frame.next_entity;
    new_frame.next_entity += 1;
    let id = FrameId::from_raw(raw);
    new_frame.frames.push(SavedFrame {
        id,
        workspace: child,
        child_workspace: None,
        views: vec![],
        selected_view: None,
        groups: vec![],
    });
    new_frame
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == child)
        .unwrap()
        .frames
        .push(id);
    assert!(matches!(
        c.prepare_restore(new_frame, options),
        Err(Error::Admission {
            budget: Budget::Depth,
            ..
        })
    ));
    assert_eq!(c.snapshot().frames[&deep].workspace, child);
}

#[test]
fn live_restore_closed_retention_overage_may_shrink_not_grow() {
    let (mut c, _, f, v) = populated();
    let resource = c.snapshot().views[&v].resource;
    let EntityId::View(second) = created(
        &mut c,
        Command::CreateView {
            frame: f,
            resource,
            index: 1,
        },
    ) else {
        unreachable!()
    };
    run(
        &mut c,
        Command::CloseView {
            view: v,
            approved: true,
        },
    )
    .unwrap();
    let low = Limits {
        max_closed_views: Some(0),
        ..Default::default()
    };
    run(&mut c, Command::SetLimits(low)).unwrap();
    let options = RestoreOptions {
        limits: low,
        ..Default::default()
    };
    let saved = c.save_layout();
    assert!(c.prepare_restore(saved.clone(), options.clone()).is_ok());
    let mut growing = saved.clone();
    growing
        .views
        .iter_mut()
        .find(|view| view.id == second)
        .unwrap()
        .closed = Some(ClosedView { index: 0 });
    let frame = growing
        .frames
        .iter_mut()
        .find(|frame| frame.id == f)
        .unwrap();
    frame.views.clear();
    frame.selected_view = None;
    assert!(matches!(
        c.prepare_restore(growing, options.clone()),
        Err(Error::Admission {
            budget: Budget::ClosedViews,
            ..
        })
    ));
    let mut decreasing = saved;
    decreasing.views.retain(|view| view.id != v);
    let token = c.prepare_restore(decreasing, options).unwrap();
    run(&mut c, Command::Restore(token)).unwrap();
    assert!(!c.snapshot().views.contains_key(&v));
    assert!(c.snapshot().views.contains_key(&second));
}
