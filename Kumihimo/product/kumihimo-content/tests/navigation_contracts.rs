//! The eight frozen WP011 navigation dimensions, through the public recording-host boundary.
#![cfg(feature = "navigation")]
use kumihimo_content::*;
use kumihimo_core::{self as core, Command, Core, EntityId, Envelope, ProviderId, Scalar, ViewId};

fn v(n: f64) -> Scalar {
    Scalar::new(n).unwrap()
}
fn permit() -> HostPermission {
    HostPermission {
        allowed: true,
        eligibility_revision: 0,
    }
}
fn envelope(c: &Core, command: Command) -> Envelope {
    Envelope {
        identity: c.snapshot().identity.clone(),
        actor: "navigation-host".into(),
        operation: c.ledger_status().high_watermark.checked_next().unwrap(),
        expected_generation: c.snapshot().generation,
        command,
    }
}
fn created(c: &mut Core, command: Command) -> EntityId {
    match c.execute(envelope(c, command)).outcome.result.unwrap() {
        core::Change::Created(id) => id,
        _ => panic!("creation"),
    }
}
struct Host {
    c: Controller,
    scope: Scope,
    provider: ProviderId,
    view: ViewId,
}
fn host(caps: Capabilities) -> Host {
    let mut c = Core::new(
        core::Identity::new("navigation", "core-1").unwrap(),
        core::Limits::default(),
        128,
    )
    .unwrap();
    let EntityId::Workspace(workspace) = created(&mut c, Command::CreateWorkspace { owner: None })
    else {
        unreachable!()
    };
    let EntityId::Frame(frame) = created(
        &mut c,
        Command::CreateFrame {
            workspace,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Provider(provider) = created(
        &mut c,
        Command::RegisterProvider {
            key: "nav-host".into(),
            supports_shared_views: true,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Resource(resource) = created(
        &mut c,
        Command::RegisterResource {
            provider,
            key: "document".into(),
            restore_reference: "host-state".into(),
        },
    ) else {
        unreachable!()
    };
    let EntityId::View(view) = created(
        &mut c,
        Command::CreateView {
            frame,
            resource,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    let mut c = Controller::new(c, "navigation-host-session", ContentLimits::default()).unwrap();
    c.negotiate(provider, ProtocolVersion::V2, caps.clone(), caps)
        .unwrap();
    let mount = c
        .attach(view, AttachOptions::default(), permit(), 0)
        .unwrap();
    let request = c.dispatch(mount, permit(), 0).unwrap().unwrap();
    c.ack(
        Ack {
            ticket: request.ticket.clone(),
            result: AckResult::Confirmed(Confirmation::Attached {
                strength: RestorationStrength::Unknown,
            }),
        },
        0,
    )
    .unwrap();
    let scope = request.ticket.scope;
    c.update_navigation(&scope, model(1, 1000.0, 100.0), obs(1, 1, 450.0))
        .unwrap();
    c.update_navigation_rail(&scope, 1, rail(100.0)).unwrap();
    Host {
        c,
        scope,
        provider,
        view,
    }
}
fn model(revision: u64, extent: f64, viewport: f64) -> DocumentModel {
    DocumentModel {
        revision,
        semantic: "host-document".into(),
        unit: "host-units".into(),
        extent: v(extent),
        viewport: v(viewport),
    }
}
fn obs(model_revision: u64, revision: u64, offset: f64) -> NavigationObservation {
    NavigationObservation {
        model_revision,
        revision,
        offset: v(offset),
    }
}
fn rail(length: f64) -> RailGeometry {
    RailGeometry {
        origin: v(10.0),
        length: v(length),
        map_origin: v(30.0),
        map_length: v(200.0),
        document_rect: core::Rect::new(0.0, 0.0, 600.0, 400.0).unwrap(),
    }
}
fn marker(id: &str) -> OverviewMarker {
    OverviewMarker {
        id: id.into(),
        start: v(0.0),
        end: v(100.0),
        jump_target: Some(format!("opaque:{id}")),
        label: "Host heading".into(),
        style: "host-style".into(),
    }
}
fn overview(revision: u64) -> Overview {
    Overview {
        revision,
        model_revision: 1,
        unit: "host-units".into(),
        markers: vec![marker("one")],
    }
}
fn configure(h: &mut Host, revision: u64, mode: NavigationMode) {
    let mut layers = SettingsLayers::<NavigationSettingsPatch>::default();
    layers.surface.mode = Some(mode);
    h.c.configure_navigation(&h.scope, revision, layers, NavigationPolicy::default())
        .unwrap();
}
fn press(h: &mut Host, gesture: u64, kind: PointerKind, grab: f64) -> Scalar {
    let thumb = h.c.navigation_projection(&h.scope).unwrap().thumb.unwrap();
    let position = v(thumb.start.get() + thumb.length.get() * grab);
    h.c.navigation_press(&h.scope, gesture, 7, kind, position)
        .unwrap();
    position
}
fn next(h: &mut Host) -> Request {
    h.c.dispatch(h.scope.mount, permit(), 0).unwrap().unwrap()
}
fn ack_offset(h: &mut Host, request: Request, revision: u64, offset: f64) -> Receipt {
    h.c.ack(
        Ack {
            ticket: request.ticket,
            result: AckResult::Confirmed(Confirmation::Navigation(obs(1, revision, offset))),
        },
        0,
    )
    .unwrap()
}
fn absolute(request: &Request) -> f64 {
    match &request.kind {
        RequestKind::Navigation(NavigationRequest {
            action: NavigationAction::AbsoluteScroll(offset),
            ..
        }) => offset.get(),
        _ => panic!("absolute scroll"),
    }
}

#[test]
fn eval_001_modes_precedence_and_declared_fallback() {
    let mut h = host(Capabilities::default());
    let identity = h.c.snapshot().identity.clone();
    let generation = h.c.snapshot().generation;
    configure(&mut h, 2, NavigationMode::ScrubToMinimap);
    let p = h.c.navigation_projection(&h.scope).unwrap();
    assert_eq!(p.selected, NavigationMode::ScrubToMinimap);
    assert_eq!(p.effective, Some(NavigationMode::ScrollbarOnly));
    assert_eq!(p.fallback_reason, Some(NavigationDisabled::OverviewMissing));
    h.c.update_overview(&h.scope, overview(1)).unwrap();
    for (index, mode) in [
        NavigationMode::ScrollbarOnly,
        NavigationMode::MinimapAlways,
        NavigationMode::ScrollbarAndMinimap,
        NavigationMode::ScrubToMinimap,
    ]
    .into_iter()
    .enumerate()
    {
        configure(&mut h, index as u64 + 3, mode);
        let p = h.c.navigation_projection(&h.scope).unwrap();
        assert_eq!(p.effective, Some(mode));
        assert_eq!(
            p.minimap_visible,
            matches!(
                mode,
                NavigationMode::MinimapAlways | NavigationMode::ScrollbarAndMinimap
            )
        );
        assert_eq!(p.rail_visible, mode != NavigationMode::MinimapAlways);
    }
    let layers = SettingsLayers {
        host_profile: NavigationSettingsPatch {
            profile: Some(ControlProfile::Comfortable),
            line_step: Some(v(20.0)),
            ..Default::default()
        },
        workspace: NavigationSettingsPatch {
            line_step: Some(v(50.0)),
            ..Default::default()
        },
        surface: NavigationSettingsPatch {
            rail_width: Some(v(30.0)),
            ..Default::default()
        },
        transition: Transition::Recreate,
        ..Default::default()
    };
    h.c.configure_navigation(&h.scope, 8, layers, NavigationPolicy::default())
        .unwrap();
    let state = &h.c.inspect().navigation[&h.scope.mount];
    assert_eq!(state.settings.rail_width.value, v(30.0));
    assert_eq!(state.settings.rail_width.source, SettingSource::Surface);
    assert_eq!(state.settings.line_step.source, SettingSource::Workspace);
    assert_eq!(state.settings.minimum_thumb.value, v(48.0));
    assert_eq!(
        state.settings.minimum_thumb.source,
        SettingSource::HostProfile
    );
    assert_eq!(
        h.c.navigation_projection(&h.scope).unwrap().transition,
        Transition::Recreate
    );
    let before = h.c.inspect();
    let mut bad = SettingsLayers::<NavigationSettingsPatch>::default();
    bad.host_profile.minimum_thumb = Some(v(-1.0));
    bad.surface.minimum_thumb = Some(v(20.0));
    assert!(h
        .c
        .configure_navigation(&h.scope, 9, bad, NavigationPolicy::default())
        .is_err());
    assert_eq!(h.c.inspect(), before);
    let mut bad = SettingsLayers::<NavigationSettingsPatch>::default();
    bad.version = 1;
    assert!(h
        .c
        .configure_navigation(&h.scope, 9, bad, NavigationPolicy::default())
        .is_err());
    assert_eq!(h.c.inspect(), before);
    assert_eq!(h.c.snapshot().identity, identity);
    assert_eq!(h.c.snapshot().generation, generation);
    let mut caps = Capabilities::default();
    caps.navigation.overview = false;
    let mut unavailable = host(caps);
    let mut layers = SettingsLayers::<NavigationSettingsPatch>::default();
    layers.surface.mode = Some(NavigationMode::MinimapAlways);
    unavailable
        .c
        .configure_navigation(
            &unavailable.scope,
            2,
            layers,
            NavigationPolicy {
                fallback: NavigationFallback::Unavailable,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        unavailable
            .c
            .navigation_projection(&unavailable.scope)
            .unwrap()
            .effective,
        None
    );
    let mut caps = Capabilities::default();
    caps.input = false;
    let unsupported = host(caps);
    assert_eq!(
        unsupported
            .c
            .navigation_projection(&unsupported.scope)
            .unwrap()
            .fallback_reason,
        Some(NavigationDisabled::InputUnsupported)
    );
}

#[test]
fn eval_002_enlarged_thumb_full_range_and_degenerate_metrics() {
    let m = model(1, 1000.0, 100.0);
    let start = navigation_thumb(&m, v(0.0), v(10.0), v(100.0), v(48.0)).unwrap();
    let middle = navigation_thumb(&m, v(450.0), v(10.0), v(100.0), v(48.0)).unwrap();
    let end = navigation_thumb(&m, v(900.0), v(10.0), v(100.0), v(48.0)).unwrap();
    assert_eq!(start.start, v(10.0));
    assert_eq!(middle.start, v(36.0));
    assert_eq!(end.start, v(62.0));
    assert_eq!(end.length, v(48.0));
    assert_eq!(end.progress, v(1.0));
    for (c, vp) in [(0.0, 0.0), (10.0, 100.0), (100.0, 100.0)] {
        let t = navigation_thumb(&model(1, c, vp), v(-0.0), v(0.0), v(20.0), v(48.0)).unwrap();
        assert_eq!(t.length, v(20.0));
        assert!(!t.pointer_available);
    }
    assert!(
        navigation_thumb(&m, v(1.0), v(0.0), v(0.0), v(48.0))
            .unwrap()
            .travel
            == v(0.0)
    );
    let huge =
        navigation_thumb(&model(1, 1e308, 1e307), v(9e307), v(0.0), v(1e308), v(24.0)).unwrap();
    assert!(huge.start.get().is_finite());
    assert_eq!(huge.progress, v(1.0));
    assert!(Scalar::new(f64::NAN).is_err());
    assert!(Scalar::new(f64::INFINITY).is_err());
    assert!(navigation_thumb(&m, v(0.0), v(f64::MAX), v(f64::MAX), v(48.0)).is_err());
    assert!(navigation_thumb(&model(1, -1.0, 0.0), v(0.0), v(0.0), v(100.0), v(24.0)).is_err());
    let mut h = host(Capabilities::default());
    h.c.update_navigation_rail(&h.scope, 2, rail(12.0)).unwrap();
    assert_eq!(
        h.c.navigation_projection(&h.scope).unwrap().disabled_reason,
        Some(NavigationDisabled::NoPointerTravel)
    );
    assert!(h
        .c
        .navigation_press(&h.scope, 1, 1, PointerKind::Touch, v(15.0))
        .is_err());
    for (revision, key, expected) in [
        (2, NavigationKey::Home, 0.0),
        (3, NavigationKey::End, 900.0),
        (4, NavigationKey::PageBackward, 800.0),
        (5, NavigationKey::LineBackward, 760.0),
        (6, NavigationKey::LineForward, 800.0),
        (7, NavigationKey::PageForward, 900.0),
    ] {
        h.c.navigation_key(&h.scope, key, permit(), 0).unwrap();
        let r = next(&mut h);
        assert_eq!(absolute(&r), expected);
        ack_offset(&mut h, r, revision, expected);
    }
    // Map-only pointer availability follows its viewport, independently of the hidden short rail.
    h.c.update_overview(&h.scope, overview(1)).unwrap();
    configure(&mut h, 2, NavigationMode::MinimapAlways);
    let p = h.c.navigation_projection(&h.scope).unwrap();
    assert_eq!(p.disabled_reason, None);
    assert!(!p.thumb.unwrap().pointer_available);
    let viewport = p.viewport_rectangle.unwrap();
    assert!(viewport.pointer_available);
    h.c.navigation_press(&h.scope, 1, 7, PointerKind::Mouse, viewport.start)
        .unwrap();
    assert!(h.c.navigation_projection(&h.scope).unwrap().held_viewport);
}

#[test]
fn eval_003_stationary_press_and_grab_anchor_preservation() {
    let mut h = host(Capabilities::default());
    h.c.update_overview(&h.scope, overview(1)).unwrap();
    configure(&mut h, 2, NavigationMode::ScrubToMinimap);
    let document = h.c.snapshot().clone();
    let rect = rail(100.0).document_rect;
    for (index, kind) in [PointerKind::Mouse, PointerKind::Touch, PointerKind::Pen]
        .into_iter()
        .enumerate()
    {
        let before = h.c.inspect().pending.len();
        let position = press(&mut h, index as u64 + 1, kind, index as f64 / 2.0);
        let p = h.c.navigation_projection(&h.scope).unwrap();
        assert!(p.held_viewport && p.minimap_visible && !p.rail_visible);
        assert_eq!(p.document_rect, Some(rect));
        let anchor = p.presentation_anchor;
        assert!(h
            .c
            .navigation_move(&h.scope, index as u64 + 1, 7, position, permit(), 0)
            .unwrap()
            .is_none());
        assert_eq!(h.c.inspect().pending.len(), before);
        assert_eq!(
            h.c.navigation_projection(&h.scope)
                .unwrap()
                .presentation_anchor,
            anchor
        );
        h.c.navigation_settle(&h.scope, index as u64 + 1, 7, GestureSettlement::Release, 0)
            .unwrap();
    }
    assert_eq!(h.c.snapshot(), &document);
    assert!(h.c.inspect().queue.is_empty());
}

#[test]
fn eval_004_one_continuous_transformed_drag_and_typed_actual() {
    let mut h = host(Capabilities::default());
    h.c.update_overview(&h.scope, overview(1)).unwrap();
    configure(&mut h, 2, NavigationMode::ScrubToMinimap);
    let position = press(&mut h, 1, PointerKind::Mouse, 0.75);
    let initial = h.c.navigation_projection(&h.scope).unwrap();
    let first =
        h.c.navigation_move(&h.scope, 1, 7, v(position.get() + 18.0), permit(), 0)
            .unwrap()
            .unwrap();
    let second =
        h.c.navigation_move(&h.scope, 1, 7, v(position.get() + 36.0), permit(), 0)
            .unwrap()
            .unwrap();
    assert_ne!(first, second);
    let state = h.c.inspect();
    assert_eq!(state.queue.len(), 1);
    assert_eq!(
        state.receipts.last().unwrap().status,
        RequestStatus::Superseded
    );
    let r = next(&mut h);
    assert_eq!(absolute(&r), 630.0);
    assert_eq!(
        h.c.navigation_projection(&h.scope).unwrap().value,
        Some(v(450.0))
    );
    ack_offset(&mut h, r, 2, 620.0);
    let p = h.c.navigation_projection(&h.scope).unwrap();
    assert_eq!(p.value, Some(v(620.0)));
    assert_eq!(p.requested_offset, Some(v(630.0)));
    assert_eq!(p.presentation_anchor, initial.presentation_anchor);
    assert_eq!(p.document_rect, initial.document_rect);
    h.c.navigation_move(&h.scope, 1, 7, v(-1000.0), permit(), 0)
        .unwrap();
    let r = next(&mut h);
    assert_eq!(absolute(&r), 0.0);
    ack_offset(&mut h, r, 3, 0.0);
    h.c.navigation_move(&h.scope, 1, 7, v(10000.0), permit(), 0)
        .unwrap();
    let r = next(&mut h);
    assert_eq!(absolute(&r), 900.0);
    ack_offset(&mut h, r, 4, 900.0);
    let before = h.c.inspect();
    assert!(h
        .c
        .navigation_move(&h.scope, 1, 8, v(50.0), permit(), 0)
        .is_err());
    assert_eq!(h.c.inspect(), before);
    h.c.navigation_settle(&h.scope, 1, 7, GestureSettlement::Release, 0)
        .unwrap();
    assert_eq!(
        h.c.navigation_projection(&h.scope)
            .unwrap()
            .thumb
            .unwrap()
            .progress,
        v(1.0)
    );
}

#[test]
fn eval_005_revision_freshness_restore_and_reconcile_unknown() {
    let mut h = host(Capabilities::default());
    h.c.update_overview(&h.scope, overview(1)).unwrap();
    configure(&mut h, 2, NavigationMode::ScrubToMinimap);
    let position = press(&mut h, 1, PointerKind::Mouse, 0.5);
    h.c.navigation_move(&h.scope, 1, 7, v(position.get() + 20.0), permit(), 0)
        .unwrap();
    let old = next(&mut h);
    h.c.update_navigation(&h.scope, model(2, 1200.0, 100.0), obs(2, 2, 500.0))
        .unwrap();
    assert!(h
        .c
        .navigation_projection(&h.scope)
        .unwrap()
        .gesture
        .is_none());
    assert!(h
        .c
        .ack(
            Ack {
                ticket: old.ticket.clone(),
                result: AckResult::Confirmed(Confirmation::Navigation(obs(1, 3, 550.0)))
            },
            0
        )
        .is_err());
    assert!(h.c.observe_navigation(&h.scope, obs(1, 3, 550.0)).is_err());
    let before = h.c.inspect();
    assert!(h
        .c
        .update_navigation(&h.scope, model(2, 1300.0, 100.0), obs(2, 3, 500.0))
        .is_err());
    assert_eq!(h.c.inspect(), before);
    h.c.recover_instance(
        h.scope.instance,
        RequestKind::Reconcile {
            uncertain: old.ticket.id,
        },
        permit(),
        0,
    )
    .unwrap();
    let r =
        h.c.dispatch_recovery(h.scope.instance, permit(), 0)
            .unwrap()
            .unwrap();
    h.c.ack(
        Ack {
            ticket: r.ticket,
            result: AckResult::Confirmed(Confirmation::Reconciled {
                effect: Effect::Applied,
                lifecycle: None,
            }),
        },
        0,
    )
    .unwrap();
    assert_eq!(
        h.c.navigation_projection(&h.scope).unwrap().observation,
        Observation::Unknown
    );
    h.c.observe_navigation(&h.scope, obs(2, 3, 510.0)).unwrap();
    assert_eq!(
        h.c.navigation_projection(&h.scope).unwrap().value,
        Some(v(510.0))
    );
    h.c.observe_navigation(&h.scope, obs(2, 3, 510.0)).unwrap();
    assert!(h.c.observe_navigation(&h.scope, obs(2, 3, 511.0)).is_err());
    assert!(h.c.observe_navigation(&h.scope, obs(2, 2, 500.0)).is_err());
    // Unrelated view labels do not blanket-stale the exact target/model.
    h.c.execute(envelope(
        h.c.core(),
        Command::SetViewMetadata {
            view: h.view,
            label: "renamed".into(),
            pinned: false,
            unsaved: false,
        },
    ))
    .unwrap()
    .outcome
    .result
    .unwrap();
    assert_eq!(
        h.c.navigation_projection(&h.scope).unwrap().value,
        Some(v(510.0))
    );
    // Exact same-ID restore is observed through the private controller boundary.
    let saved = h.c.core().save_layout();
    let token =
        h.c.core()
            .prepare_restore(saved, core::RestoreOptions::default())
            .unwrap();
    let e = envelope(h.c.core(), Command::Restore(token));
    h.c.execute(e.clone()).unwrap().outcome.result.unwrap();
    assert!(h.c.observe_navigation(&h.scope, obs(2, 4, 520.0)).is_err());
    let after = h.c.inspect();
    h.c.execute(e).unwrap();
    assert_eq!(h.c.inspect(), after);
    let mut foreign = h.scope.clone();
    foreign.session = "other-session".into();
    assert!(h.c.observe_navigation(&foreign, obs(2, 5, 530.0)).is_err());
    // Independent rail/settings changes cancel an active drag without scroll or rollback.
    let mut h = host(Capabilities::default());
    let position = press(&mut h, 1, PointerKind::Mouse, 0.5);
    h.c.update_navigation_rail(&h.scope, 2, rail(120.0))
        .unwrap();
    assert!(h
        .c
        .navigation_projection(&h.scope)
        .unwrap()
        .gesture
        .is_none());
    assert!(h.c.inspect().queue.is_empty());
    assert!(h
        .c
        .navigation_move(&h.scope, 1, 7, position, permit(), 0)
        .is_err());
    press(&mut h, 2, PointerKind::Mouse, 0.5);
    configure(&mut h, 2, NavigationMode::ScrollbarOnly);
    assert!(h
        .c
        .navigation_projection(&h.scope)
        .unwrap()
        .gesture
        .is_none());
    assert_eq!(
        h.c.navigation_projection(&h.scope).unwrap().value,
        Some(v(450.0))
    );
    h.c.negotiate(
        h.provider,
        ProtocolVersion::V2,
        Capabilities::default(),
        Capabilities::default(),
    )
    .unwrap();
    assert!(h.c.navigation_projection(&h.scope).is_err());
}

#[test]
fn eval_006_release_cancellation_focus_and_idempotent_settlement() {
    let mut h = host(Capabilities::default());
    h.c.update_overview(&h.scope, overview(1)).unwrap();
    configure(&mut h, 2, NavigationMode::ScrubToMinimap);
    let position = press(&mut h, 1, PointerKind::Pen, 0.5);
    h.c.navigation_move(&h.scope, 1, 7, v(position.get() + 10.0), permit(), 0)
        .unwrap();
    h.c.navigation_settle(&h.scope, 1, 7, GestureSettlement::Release, 0)
        .unwrap();
    h.c.navigation_settle(&h.scope, 1, 7, GestureSettlement::Release, 0)
        .unwrap();
    let p = h.c.navigation_projection(&h.scope).unwrap();
    assert!(!p.minimap_visible && p.rail_visible);
    assert_eq!(p.value, Some(v(450.0)));
    assert_eq!(p.request_status, Some(RequestStatus::Queued));
    assert!(h
        .c
        .navigation_settle(&h.scope, 1, 7, GestureSettlement::Cancel, 0)
        .is_err());
    assert!(h
        .c
        .navigation_move(&h.scope, 1, 7, position, permit(), 0)
        .is_err());
    let r = next(&mut h);
    ack_offset(&mut h, r, 2, 495.0);
    for (index, reason) in [
        GestureSettlement::Cancel,
        GestureSettlement::Escape,
        GestureSettlement::CaptureLost,
        GestureSettlement::FocusLost,
    ]
    .into_iter()
    .enumerate()
    {
        let gesture = index as u64 + 2;
        let position = press(&mut h, gesture, PointerKind::Touch, 0.5);
        h.c.navigation_move(&h.scope, gesture, 7, v(position.get() + 10.0), permit(), 0)
            .unwrap();
        h.c.navigation_settle(&h.scope, gesture, 7, reason, 0)
            .unwrap();
        assert!(h.c.inspect().queue.is_empty());
        assert_eq!(
            h.c.navigation_projection(&h.scope).unwrap().value,
            Some(v(495.0))
        );
        h.c.navigation_settle(&h.scope, gesture, 7, reason, 0)
            .unwrap();
    }
    let position = press(&mut h, 6, PointerKind::Mouse, 0.5);
    h.c.navigation_move(&h.scope, 6, 7, v(position.get() + 10.0), permit(), 0)
        .unwrap();
    let dispatched = next(&mut h);
    h.c.navigation_settle(&h.scope, 6, 7, GestureSettlement::Cancel, 0)
        .unwrap();
    assert_eq!(
        h.c.inspect().pending[&dispatched.ticket.id].status,
        RequestStatus::Unknown
    );
    assert_eq!(
        h.c.navigation_projection(&h.scope).unwrap().observation,
        Observation::Unknown
    );
    assert!(h.c.inspect().instances[&h.scope.instance].reserved);
    assert!(h.c.inspect().instances[&h.scope.instance].busy.is_some());
    // Actual focus/geometry request revisions invalidate the gesture immediately.
    let mut focused = host(Capabilities::default());
    focused
        .c
        .update_overview(&focused.scope, overview(1))
        .unwrap();
    configure(&mut focused, 2, NavigationMode::ScrubToMinimap);
    press(&mut focused, 1, PointerKind::Mouse, 0.5);
    focused
        .c
        .request(focused.scope.mount, RequestKind::Focus(false), permit(), 0)
        .unwrap();
    let p = focused.c.navigation_projection(&focused.scope).unwrap();
    assert!(p.gesture.is_none() && !p.minimap_visible && p.rail_visible);
    assert_eq!(p.value, Some(v(450.0)));
    // Starting another drag retains the released final intent in its original causal position.
    let mut released = host(Capabilities::default());
    let position = press(&mut released, 1, PointerKind::Mouse, 0.5);
    released
        .c
        .navigation_move(&released.scope, 1, 7, v(position.get() + 5.0), permit(), 0)
        .unwrap();
    released
        .c
        .navigation_settle(&released.scope, 1, 7, GestureSettlement::Release, 0)
        .unwrap();
    press(&mut released, 2, PointerKind::Mouse, 0.5);
    let old_final = next(&mut released);
    assert!(matches!(
        old_final.kind,
        RequestKind::Navigation(NavigationRequest {
            gesture: Some(1),
            ..
        })
    ));
}

#[test]
fn eval_007_sparse_overview_limits_semantic_barriers_and_lowering() {
    let mut h = host(Capabilities::default());
    let original = overview(1);
    h.c.update_overview(&h.scope, original.clone()).unwrap();
    h.c.update_overview(&h.scope, original.clone()).unwrap();
    let before = h.c.inspect();
    let mut conflict = original.clone();
    conflict.markers[0].label = "different".into();
    assert!(h.c.update_overview(&h.scope, conflict).is_err());
    assert_eq!(h.c.inspect(), before);
    // Lowering the field cap cannot erase a semantic effect admitted under the old cap.
    let admitted = h.c.navigation_jump(&h.scope, "one", permit(), 0).unwrap();
    let mut reduced = ContentLimits::default();
    reduced.payload_bytes = Some(1);
    h.c.set_limits(reduced).unwrap();
    let jump = next(&mut h);
    assert_eq!(jump.ticket.id, admitted);
    ack_offset(&mut h, jump, 2, 450.0);
    h.c.set_limits(ContentLimits::default()).unwrap();
    let before = h.c.inspect();
    let mut duplicate = overview(2);
    duplicate.markers.push(duplicate.markers[0].clone());
    assert!(h.c.update_overview(&h.scope, duplicate).is_err());
    let mut invalid = overview(2);
    invalid.markers[0].end = v(1001.0);
    assert!(h.c.update_overview(&h.scope, invalid).is_err());
    let mut bytes = overview(2);
    bytes.markers[0].label = "x".repeat(257);
    assert!(h.c.update_overview(&h.scope, bytes).is_err());
    assert_eq!(h.c.inspect(), before);
    let mut limits = ContentLimits::default();
    limits.overview_markers = Some(1);
    limits.overview_entries = Some(1);
    limits.overview_bytes = Some(original.opaque_bytes().unwrap());
    h.c.set_limits(limits.clone()).unwrap();
    let mut two = overview(2);
    two.markers.push(marker("two"));
    assert!(h.c.update_overview(&h.scope, two).is_err());
    limits.overview_markers = Some(0);
    limits.overview_entries = Some(0);
    limits.overview_bytes = Some(0);
    limits.marker_bytes = Some(0);
    h.c.set_limits(limits).unwrap();
    let unchanged = overview(2);
    h.c.update_overview(&h.scope, unchanged).unwrap();
    let mut grow = overview(3);
    grow.markers[0].style.push('x');
    assert!(h.c.update_overview(&h.scope, grow).is_err());
    h.c.set_limits(ContentLimits::default()).unwrap();
    configure(&mut h, 2, NavigationMode::ScrubToMinimap);
    let position = press(&mut h, 1, PointerKind::Mouse, 0.5);
    h.c.navigation_move(&h.scope, 1, 7, v(position.get() + 5.0), permit(), 0)
        .unwrap();
    h.c.navigation_jump(&h.scope, "one", permit(), 0).unwrap();
    h.c.navigation_move(&h.scope, 1, 7, v(position.get() + 10.0), permit(), 0)
        .unwrap();
    assert_eq!(h.c.inspect().queue.len(), 3);
    let r = next(&mut h);
    ack_offset(&mut h, r, 3, 475.0);
    let jump = next(&mut h);
    assert!(
        matches!(&jump.kind,RequestKind::Navigation(NavigationRequest{action:NavigationAction::SemanticJump{overview_revision:2,target},..}) if target=="opaque:one")
    );
    ack_offset(&mut h, jump, 4, 100.0);
    let r = next(&mut h);
    ack_offset(&mut h, r, 5, 500.0);
    let model_before = h.c.navigation_projection(&h.scope).unwrap().value;
    h.c.update_overview(&h.scope, overview(3)).unwrap();
    assert_eq!(
        h.c.navigation_projection(&h.scope).unwrap().value,
        model_before
    );
    assert_eq!(
        h.c.navigation_projection(&h.scope).unwrap().gesture,
        Some(1)
    );
    let before = h.c.inspect();
    let mut huge = overview(4);
    huge.markers = (0..4097).map(|i| marker(&i.to_string())).collect();
    assert!(h.c.update_overview(&h.scope, huge).is_err());
    assert_eq!(h.c.inspect(), before);
}

#[test]
fn eval_008_keyboard_density_accessibility_and_input_prerequisites() {
    let mut h = host(Capabilities::default());
    let mut layers = SettingsLayers::<NavigationSettingsPatch>::default();
    layers.surface.profile = Some(ControlProfile::Comfortable);
    layers.surface.visibility = Some(RailVisibility::AutoHide);
    layers.surface.accessible_name = Some("Document Y".into());
    layers.surface.touch_threshold = Some(v(8.0));
    layers.surface.pen_threshold = Some(v(3.0));
    h.c.configure_navigation(&h.scope, 2, layers, NavigationPolicy::default())
        .unwrap();
    let p = h.c.navigation_projection(&h.scope).unwrap();
    assert_eq!(
        (p.rail_width, p.thumb_width, p.hit_width),
        (v(24.0), v(16.0), v(40.0))
    );
    assert!(!p.rail_visible);
    assert_eq!(p.orientation, "vertical");
    assert_eq!(p.role, "scrollbar");
    assert_eq!(p.accessible_name, "Document Y");
    assert_eq!(p.minimum, v(0.0));
    assert_eq!(p.maximum, Some(v(900.0)));
    assert_eq!(p.value, Some(v(450.0)));
    h.c.navigation_reveal(&h.scope, false, true, false).unwrap();
    assert!(h.c.navigation_projection(&h.scope).unwrap().rail_visible);
    assert_eq!(
        h.c.inspect().navigation[&h.scope.mount]
            .settings
            .touch_threshold
            .value,
        v(8.0)
    );
    assert_eq!(
        h.c.inspect().navigation[&h.scope.mount]
            .settings
            .pen_threshold
            .value,
        v(3.0)
    );
    let rect = core::Rect::new(0.0, 0.0, 600.0, 400.0).unwrap();
    h.c.request(
        h.scope.mount,
        RequestKind::Geometry(Geometry {
            rect,
            clip: rect,
            scale: v(1.0),
            anchor: None,
        }),
        permit(),
        0,
    )
    .unwrap();
    h.c.request(h.scope.mount, RequestKind::Focus(true), permit(), 0)
        .unwrap();
    h.c.navigation_key(&h.scope, NavigationKey::End, permit(), 0)
        .unwrap();
    let geometry = next(&mut h);
    assert!(matches!(geometry.kind, RequestKind::Geometry(_)));
    assert!(h.c.dispatch(h.scope.mount, permit(), 0).unwrap().is_none());
    h.c.ack(
        Ack {
            ticket: geometry.ticket,
            result: AckResult::Confirmed(Confirmation::Geometry { anchor: None }),
        },
        0,
    )
    .unwrap();
    let focus = next(&mut h);
    assert!(matches!(focus.kind, RequestKind::Focus(true)));
    h.c.ack(
        Ack {
            ticket: focus.ticket,
            result: AckResult::Confirmed(Confirmation::Done),
        },
        0,
    )
    .unwrap();
    let key = next(&mut h);
    assert_eq!(absolute(&key), 900.0);
    ack_offset(&mut h, key, 2, 900.0);
    let before = h.c.inspect();
    let denied = HostPermission {
        allowed: false,
        eligibility_revision: 0,
    };
    assert!(h
        .c
        .navigation_key(&h.scope, NavigationKey::Home, denied, 0)
        .is_err());
    assert_eq!(h.c.inspect(), before);
    assert!(h
        .c
        .request(
            h.scope.mount,
            RequestKind::Input(Input {
                owner: InputOwner::FrameChrome,
                payload: "typing".into(),
                x: None,
                y: None
            }),
            permit(),
            0
        )
        .is_err());
    let stale = HostPermission {
        allowed: true,
        eligibility_revision: 99,
    };
    assert!(h
        .c
        .navigation_key(&h.scope, NavigationKey::Home, stale, 0)
        .is_err());
}
