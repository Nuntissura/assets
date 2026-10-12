#![cfg(feature = "backgrounds")]
use kumihimo_content::*;
use kumihimo_core::{
    self as core, Command, Core, EntityId, Envelope, FrameId, ProviderId, ResourceId, Scalar,
    WorkspaceId,
};
use std::collections::{BTreeMap, BTreeSet};

fn permission() -> HostPermission {
    HostPermission {
        allowed: true,
        eligibility_revision: 0,
    }
}
fn envelope(c: &Core, command: Command) -> Envelope {
    Envelope {
        identity: c.snapshot().identity.clone(),
        actor: "background-host".into(),
        operation: c.ledger_status().high_watermark.checked_next().unwrap(),
        expected_generation: c.snapshot().generation,
        command,
    }
}
fn create(c: &mut Core, command: Command) -> EntityId {
    match c.execute(envelope(c, command)).outcome.result.unwrap() {
        core::Change::Created(id) => id,
        _ => panic!("creation"),
    }
}
fn canonical(c: &mut Controller, command: Command) -> core::Change {
    c.execute(envelope(c.core(), command))
        .unwrap()
        .outcome
        .result
        .unwrap()
}
fn create_controlled(c: &mut Controller, command: Command) -> EntityId {
    match canonical(c, command) {
        core::Change::Created(id) => id,
        _ => panic!("creation"),
    }
}
struct Fixture {
    c: Controller,
    workspace: WorkspaceId,
    frame: FrameId,
    provider: ProviderId,
    resource: ResourceId,
}
fn capabilities() -> Capabilities {
    Capabilities {
        sharing: true,
        backgrounds: BackgroundCapabilities {
            image: true,
            video: true,
            compositing: true,
            fits: BTreeSet::from([
                BackgroundFit::Cover,
                BackgroundFit::Contain,
                BackgroundFit::Stretch,
                BackgroundFit::Native,
            ]),
            poster: true,
            audio: true,
            host_contrast: true,
            media_budget: true,
        },
        ..Default::default()
    }
}
fn fixture(limits: ContentLimits) -> Fixture {
    let mut c = Core::new(
        core::Identity::new("background-layout", "one").unwrap(),
        core::Limits::default(),
        128,
    )
    .unwrap();
    let EntityId::Workspace(workspace) = create(&mut c, Command::CreateWorkspace { owner: None })
    else {
        unreachable!()
    };
    let EntityId::Frame(frame) = create(
        &mut c,
        Command::CreateFrame {
            workspace,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Provider(provider) = create(
        &mut c,
        Command::RegisterProvider {
            key: "media-provider".into(),
            supports_shared_views: true,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Resource(resource) = create(
        &mut c,
        Command::RegisterResource {
            provider,
            key: "media".into(),
            restore_reference: "host-poster".into(),
        },
    ) else {
        unreachable!()
    };
    let mut c = Controller::new(c, "background-session", limits).unwrap();
    let caps = capabilities();
    c.negotiate(provider, ProtocolVersion::V2, caps.clone(), caps)
        .unwrap();
    Fixture {
        c,
        workspace,
        frame,
        provider,
        resource,
    }
}
fn ack(c: &mut Controller, request: Request, confirmation: Confirmation, now: u64) -> Receipt {
    c.ack(
        Ack {
            ticket: request.ticket,
            result: AckResult::Confirmed(confirmation),
        },
        now,
    )
    .unwrap()
}
fn attached(f: &mut Fixture, target: MountTarget, share: Option<InstanceId>) -> MountId {
    let id =
        f.c.attach_background(
            target,
            f.resource,
            BackgroundKind::Video,
            AttachOptions {
                share,
                host_share: share.is_some(),
                ..Default::default()
            },
            permission(),
            0,
        )
        .unwrap();
    let request = f.c.dispatch(id, permission(), 0).unwrap().unwrap();
    assert_eq!(
        ack(
            &mut f.c,
            request,
            Confirmation::Attached {
                strength: RestorationStrength::LayoutOnly
            },
            0
        )
        .status,
        RequestStatus::Confirmed
    );
    let scope = f.c.inspect().mounts[&id].scope.clone();
    f.c.background_settings(
        scope,
        SettingsLayers::default(),
        BackgroundPolicy::default(),
        0,
    )
    .unwrap();
    id
}
fn mount_scope(c: &Controller, mount: MountId) -> Scope {
    c.inspect().mounts[&mount].scope.clone()
}
fn policy(work: MediaWork, fps: u16) -> MediaPolicy {
    MediaPolicy {
        work,
        ceilings: MediaCeilings {
            fps: Some(fps),
            ..Default::default()
        },
    }
}
fn apply_budget(c: &mut Controller, mount: MountId, now: u64, obs_revision: u64) -> Request {
    let request = c.dispatch(mount, permission(), now).unwrap().unwrap();
    let RequestKind::MediaBudget(budget) = request.kind else {
        panic!("media request")
    };
    let actual = MediaObservation {
        aggregate_revision: budget.aggregate_revision,
        observation_revision: obs_revision,
        paused: budget.paused,
        fps: if budget.paused {
            0
        } else {
            budget.ceilings.fps.unwrap_or(30)
        },
        width: budget.ceilings.width.unwrap_or(640),
        height: budget.ceilings.height.unwrap_or(480),
    };
    assert_eq!(
        ack(c, request.clone(), Confirmation::MediaBudget(actual), now).status,
        RequestStatus::Confirmed
    );
    request
}

#[test]
fn eval_kum_011_009_real_targets_composition_and_binding() {
    // Exercise actual reorders, descendant placement and retained packing with both delivery states.
    for dispatched in [false, true] {
        for scenario in 0..3 {
            let mut moved = fixture(ContentLimits::default());
            let root = moved.workspace;
            let ancestor = moved.frame;
            let EntityId::Frame(second) = create_controlled(
                &mut moved.c,
                Command::CreateFrame {
                    workspace: root,
                    index: 1,
                },
            ) else {
                unreachable!()
            };
            let mut affected_targets = vec![MountTarget::FrameBackground(ancestor)];
            let mut nested_identity = None;
            let mut packing_sibling = None;
            if scenario == 1 {
                let EntityId::Workspace(child) = create_controlled(
                    &mut moved.c,
                    Command::CreateWorkspace {
                        owner: Some(ancestor),
                    },
                ) else {
                    unreachable!()
                };
                let EntityId::Frame(owner) = create_controlled(
                    &mut moved.c,
                    Command::CreateFrame {
                        workspace: child,
                        index: 0,
                    },
                ) else {
                    unreachable!()
                };
                let EntityId::Workspace(grandchild) = create_controlled(
                    &mut moved.c,
                    Command::CreateWorkspace { owner: Some(owner) },
                ) else {
                    unreachable!()
                };
                let EntityId::Frame(leaf) = create_controlled(
                    &mut moved.c,
                    Command::CreateFrame {
                        workspace: grandchild,
                        index: 0,
                    },
                ) else {
                    unreachable!()
                };
                affected_targets = vec![
                    MountTarget::FrameBackground(leaf),
                    MountTarget::WorkspaceBackground(grandchild),
                ];
                nested_identity = Some((
                    leaf,
                    moved.c.snapshot().frames[&leaf].clone(),
                    grandchild,
                    moved.c.snapshot().workspaces[&grandchild].clone(),
                ));
            } else if scenario == 2 {
                let EntityId::Frame(third) = create_controlled(
                    &mut moved.c,
                    Command::CreateFrame {
                        workspace: root,
                        index: 2,
                    },
                ) else {
                    unreachable!()
                };
                let plan = core::LayoutPlan {
                    root: Some(core::NodeId(4)),
                    nodes: BTreeMap::from([
                        (
                            core::NodeId(1),
                            core::LayoutNode::Leaf(core::LayoutItem::Frame(ancestor)),
                        ),
                        (
                            core::NodeId(2),
                            core::LayoutNode::Leaf(core::LayoutItem::Frame(third)),
                        ),
                        (
                            core::NodeId(3),
                            core::LayoutNode::Leaf(core::LayoutItem::Frame(second)),
                        ),
                        (
                            core::NodeId(4),
                            core::LayoutNode::Split {
                                axis: core::Axis::Horizontal,
                                children: (1..=3)
                                    .map(|id| core::WeightedNode {
                                        node: core::NodeId(id),
                                        weight: Scalar::new(1.0).unwrap(),
                                    })
                                    .collect(),
                            },
                        ),
                    ]),
                };
                canonical(
                    &mut moved.c,
                    Command::SetLayout {
                        workspace: root,
                        plan,
                    },
                );
                affected_targets = vec![MountTarget::FrameBackground(third)];
                packing_sibling = Some((third, moved.c.snapshot().frames[&third].clone()));
            }
            canonical(
                &mut moved.c,
                Command::SetGeometryContext {
                    workspace: root,
                    context: core::GeometryContext::new(
                        core::Rect::new(0.0, 0.0, 900.0, 600.0).unwrap(),
                    ),
                },
            );
            let before_geometry = moved.c.snapshot().geometry(root).unwrap();
            let before_plan = moved.c.snapshot().layouts.get(&root).cloned();
            let root_mount = attached(&mut moved, MountTarget::WorkspaceBackground(root), None);
            let affected: Vec<_> = affected_targets
                .into_iter()
                .map(|target| attached(&mut moved, target, None))
                .collect();
            let host_geometry = Geometry {
                rect: core::Rect::new(0.0, 0.0, 200.0, 100.0).unwrap(),
                clip: core::Rect::new(0.0, 0.0, 200.0, 100.0).unwrap(),
                scale: Scalar::new(1.0).unwrap(),
                anchor: None,
            };
            let root_request = moved
                .c
                .request(
                    root_mount,
                    RequestKind::Geometry(host_geometry.clone()),
                    permission(),
                    0,
                )
                .unwrap();
            let mut old_effects = vec![];
            for mount in &affected {
                let before = moved.c.inspect().mounts[mount].clone();
                let id = moved
                    .c
                    .request(
                        *mount,
                        RequestKind::Geometry(host_geometry.clone()),
                        permission(),
                        0,
                    )
                    .unwrap();
                let delivered =
                    dispatched.then(|| moved.c.dispatch(*mount, permission(), 0).unwrap().unwrap());
                old_effects.push((*mount, before, id, delivered));
            }
            let counts = moved.c.inspect().counts;
            let command = envelope(
                moved.c.core(),
                Command::MoveFrame {
                    frame: ancestor,
                    destination: root,
                    index: 1,
                },
            );
            assert!(
                matches!(moved.c.execute(command.clone()).unwrap().outcome.result.unwrap(),core::Change::Moved(EntityId::Frame(id)) if id==ancestor)
            );
            assert_eq!(moved.c.snapshot().workspaces[&root].frames[1], ancestor);
            let after_geometry = moved.c.snapshot().geometry(root).unwrap();
            assert_ne!(
                before_geometry.frames[&ancestor],
                after_geometry.frames[&ancestor]
            );
            assert_eq!(before_geometry.content, after_geometry.content);
            if let Some((leaf, frame, workspace, record)) = nested_identity {
                assert_eq!(moved.c.snapshot().frames[&leaf], frame);
                assert_eq!(moved.c.snapshot().workspaces[&workspace], record);
            }
            if let Some((sibling, frame)) = packing_sibling {
                assert_eq!(moved.c.snapshot().frames[&sibling], frame);
                assert_eq!(moved.c.snapshot().workspaces[&root].frames[2], sibling);
                assert_ne!(before_plan, moved.c.snapshot().layouts.get(&root).cloned());
                assert_ne!(
                    before_geometry.frames[&sibling],
                    after_geometry.frames[&sibling]
                );
            }
            for (mount, before, id, delivered) in old_effects {
                let state = moved.c.inspect();
                let current = &state.mounts[&mount];
                assert_eq!(current.scope, before.scope);
                assert_eq!(current.instance, before.instance);
                assert!(current.active && current.reserved);
                assert!(state.instances[&current.instance].reserved);
                if let Some(request) = delivered {
                    assert_eq!(
                        state.pending[&id].status,
                        RequestStatus::Unknown,
                        "scenario {scenario}"
                    );
                    assert!(state.pending[&id].stale);
                    assert_eq!(state.instances[&current.instance].busy, Some(id));
                    assert!(moved
                        .c
                        .ack(
                            Ack {
                                ticket: request.ticket,
                                result: AckResult::Confirmed(Confirmation::Geometry {
                                    anchor: None
                                })
                            },
                            0
                        )
                        .is_err());
                } else {
                    assert!(!state.pending.contains_key(&id), "scenario {scenario}");
                    assert!(state
                        .receipts
                        .iter()
                        .any(|r| r.ticket.id == id && r.status == RequestStatus::Rejected));
                }
                assert!(!moved.c.inspect().pending.values().any(|p| p
                    .request
                    .ticket
                    .scope
                    .instance
                    == before.instance
                    && matches!(p.request.kind, RequestKind::Dispose)));
            }
            let state = moved.c.inspect();
            assert_eq!(state.counts.mounts, counts.mounts);
            assert_eq!(state.counts.instances, counts.instances);
            assert_eq!(state.pending[&root_request].status, RequestStatus::Queued);
            assert!(!state.pending[&root_request].stale);
            let request = moved
                .c
                .dispatch(root_mount, permission(), 0)
                .unwrap()
                .unwrap();
            assert_eq!(request.ticket.id, root_request);
            ack(
                &mut moved.c,
                request,
                Confirmation::Geometry { anchor: None },
                0,
            );
            let unchanged = moved.c.inspect();
            assert!(moved.c.execute(command).unwrap().replayed);
            assert_eq!(moved.c.inspect(), unchanged);
            assert_eq!(
                canonical(
                    &mut moved.c,
                    Command::MoveFrame {
                        frame: ancestor,
                        destination: root,
                        index: 1
                    }
                ),
                core::Change::Unchanged
            );
            assert_eq!(moved.c.inspect(), unchanged);
        }
    }
    let mut f = fixture(ContentLimits::default());
    let before = f.c.snapshot().clone();
    let workspace = f.workspace;
    let frame = f.frame;
    assert!(f
        .c
        .negotiate(
            f.provider,
            ProtocolVersion { major: 1, minor: 0 },
            capabilities(),
            capabilities()
        )
        .is_err());
    let a = attached(&mut f, MountTarget::WorkspaceBackground(workspace), None);
    let instance = f.c.inspect().mounts[&a].instance;
    let b = attached(&mut f, MountTarget::FrameBackground(frame), Some(instance));
    assert_eq!(*f.c.snapshot(), before);
    assert!(f.c.snapshot().views.is_empty());
    let unchanged = f.c.inspect();
    assert!(f
        .c
        .attach_background(
            MountTarget::FrameBackground(frame),
            f.resource,
            BackgroundKind::Image,
            AttachOptions::default(),
            permission(),
            0
        )
        .is_err());
    assert_eq!(f.c.inspect(), unchanged);
    let current = mount_scope(&f.c, b);
    let mut layers = SettingsLayers::<BackgroundSettingsPatch>::default();
    layers.host_profile.brightness = Some(Scalar::new(0.8).unwrap());
    layers.workspace.brightness = Some(Scalar::new(1.1).unwrap());
    layers.surface.brightness = Some(Scalar::new(1.3).unwrap());
    layers.surface.fit = Some(BackgroundFit::Contain);
    let resolved =
        f.c.background_settings(current.clone(), layers, BackgroundPolicy::default(), 0)
            .unwrap();
    assert_eq!(
        resolved.effective.as_ref().unwrap().brightness.source,
        SettingSource::Surface
    );
    assert_eq!(
        resolved.effective.unwrap().fit.value,
        BackgroundFit::Contain
    );
    let mut invalid = SettingsLayers::<BackgroundSettingsPatch>::default();
    invalid.host_profile.opacity = Some(Scalar::new(2.0).unwrap());
    invalid.surface.opacity = Some(Scalar::new(0.2).unwrap());
    let unchanged = f.c.inspect();
    assert!(f
        .c
        .background_settings(current.clone(), invalid, BackgroundPolicy::default(), 0)
        .is_err());
    assert_eq!(f.c.inspect(), unchanged);
    let command = Command::MoveFrame {
        frame,
        destination: workspace,
        index: 0,
    };
    f.c.execute(envelope(f.c.core(), command))
        .unwrap()
        .outcome
        .result
        .unwrap();
    assert!(f.c.inspect().mounts[&b].active);
    assert_eq!(f.c.inspect().mounts[&b].scope, current);
    let created =
        f.c.execute(envelope(
            f.c.core(),
            Command::CreateView {
                frame,
                resource: f.resource,
                index: 0,
            },
        ))
        .unwrap()
        .outcome
        .result
        .unwrap();
    let core::Change::Created(EntityId::View(view)) = created else {
        unreachable!()
    };
    let unchanged = f.c.inspect();
    assert!(f
        .c
        .attach(
            view,
            AttachOptions {
                share: Some(instance),
                host_share: true,
                ..Default::default()
            },
            permission(),
            0
        )
        .is_err());
    assert_eq!(f.c.inspect(), unchanged);
    f.c.execute(envelope(
        f.c.core(),
        Command::CloseView {
            view,
            approved: true,
        },
    ))
    .unwrap()
    .outcome
    .result
    .unwrap();
    assert!(f.c.inspect().mounts[&a].active && f.c.inspect().mounts[&b].active);
    let saved = f.c.core().save_layout();
    let options = core::RestoreOptions {
        providers: vec![core::ProviderResolution {
            key: "media-provider".into(),
            supports_shared_views: true,
        }],
        ..Default::default()
    };
    let restore = f.c.core().prepare_restore(saved, options).unwrap();
    let command = envelope(f.c.core(), Command::Restore(restore));
    assert!(!f.c.execute(command.clone()).unwrap().replayed);
    assert!(!f.c.inspect().mounts[&a].active && !f.c.inspect().mounts[&b].active);
    let unchanged = f.c.inspect();
    assert!(f.c.execute(command).unwrap().replayed);
    assert_eq!(f.c.inspect(), unchanged);
    let mut caps = capabilities();
    caps.backgrounds.compositing = false;
    f.c.negotiate(f.provider, ProtocolVersion::V2, caps.clone(), caps)
        .unwrap();
    assert!(!f.c.inspect().mounts[&b].active);
    assert!(f
        .c
        .background_settings(
            current,
            SettingsLayers::default(),
            BackgroundPolicy::default(),
            0
        )
        .is_err());
    let unavailable = resolve_background_settings(
        &SettingsLayers::default(),
        BackgroundPolicy::default(),
        &BackgroundCapabilities::default(),
        BackgroundKind::Video,
    )
    .unwrap();
    assert!(unavailable.effective.is_none());
    assert!(unavailable.fallback.is_some());
}

#[test]
fn eval_kum_011_010_legibility_motion_audio_and_input_denial() {
    let mut f = fixture(ContentLimits::default());
    let target = MountTarget::FrameBackground(f.frame);
    let mount = attached(&mut f, target, None);
    let scope = mount_scope(&f.c, mount);
    let before = f.c.inspect().media[&scope.instance].clone();
    let mut layers = SettingsLayers::<BackgroundSettingsPatch>::default();
    layers.surface.opacity = Some(Scalar::new(0.0).unwrap());
    layers.surface.poster = Some(Some("opaque-poster".into()));
    layers.surface.reduced_motion = Some(true);
    layers.transition = Transition::Recreate;
    let resolved =
        f.c.background_settings(scope.clone(), layers, BackgroundPolicy::default(), 0)
            .unwrap();
    assert_eq!(resolved.transition, Transition::Recreate);
    let effective = resolved.effective.unwrap();
    assert!(effective.muted.value);
    assert_eq!(effective.overlay_opacity.value.get(), 0.35);
    assert!(effective.reduced_motion.value);
    assert_eq!(f.c.inspect().media[&scope.instance], before);
    assert_eq!(resolved.layer, BackgroundLayer::BehindContent);
    assert!(resolved.input_transparent);
    let missing = SettingsLayers::<BackgroundSettingsPatch> {
        surface: BackgroundSettingsPatch {
            reduced_motion: Some(true),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(f
        .c
        .background_settings(scope.clone(), missing, BackgroundPolicy::default(), 0)
        .unwrap()
        .fallback
        .is_some());
    let mut layers = SettingsLayers::<BackgroundSettingsPatch>::default();
    layers.surface.muted = Some(false);
    assert!(f
        .c
        .background_settings(scope.clone(), layers, BackgroundPolicy::default(), 0)
        .unwrap()
        .fallback
        .is_some());
    for request in [
        RequestKind::Focus(true),
        RequestKind::Input(Input {
            owner: InputOwner::Content,
            payload: "ordinary typing".into(),
            x: None,
            y: None,
        }),
        RequestKind::CloseDecision {
            eligibility_revision: 0,
        },
        RequestKind::ToolAction {
            tool: core::ToolId::from_raw(1),
            target: core::EditingTarget {
                view: core::ViewId::from_raw(1),
                resource: f.resource,
            },
            payload: "tool action".into(),
        },
        RequestKind::AssetAction(AssetAction {
            asset: HostAssetReference {
                identity: "asset".into(),
                revision: "1".into(),
            },
            mode: AssetMode::LinkedEdit,
            undo: UndoScope::HostDocument,
            payload: "edit".into(),
        }),
    ] {
        let unchanged = f.c.inspect();
        assert!(f.c.request(mount, request, permission(), 0).is_err());
        assert_eq!(f.c.inspect(), unchanged);
    }
    let bad_crop = BackgroundCrop {
        x: Scalar::new(0.9).unwrap(),
        width: Scalar::new(0.2).unwrap(),
        ..Default::default()
    };
    assert!(bad_crop.validate().is_err());
    assert!(Scalar::new(f64::NAN).is_err());
    let oversized = SettingsLayers::<BackgroundSettingsPatch> {
        surface: BackgroundSettingsPatch {
            poster: Some(Some("x".repeat(257))),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(f
        .c
        .background_settings(scope.clone(), oversized, BackgroundPolicy::default(), 0)
        .is_err());
    let redacted = format!("{:?}", f.c.diagnostics());
    assert!(!redacted.contains("opaque-poster"));
    assert!(!redacted.contains("ordinary typing"));
}

#[test]
fn eval_kum_011_011_shared_min_ceilings_exact_echo_and_unknown() {
    let mut f = fixture(ContentLimits::default());
    let workspace = f.workspace;
    let frame = f.frame;
    let a = attached(&mut f, MountTarget::WorkspaceBackground(workspace), None);
    let instance = f.c.inspect().mounts[&a].instance;
    let b = attached(&mut f, MountTarget::FrameBackground(frame), Some(instance));
    let sa = mount_scope(&f.c, a);
    let sb = mount_scope(&f.c, b);
    f.c.media_policy(sa.clone(), policy(MediaWork::Run, 60), permission(), 0)
        .unwrap();
    apply_budget(&mut f.c, a, 0, 1);
    f.c.media_policy(sb.clone(), policy(MediaWork::Throttle, 15), permission(), 0)
        .unwrap();
    let req = apply_budget(&mut f.c, b, 0, 2);
    let RequestKind::MediaBudget(budget) = req.kind else {
        unreachable!()
    };
    assert_eq!(budget.ceilings.fps, Some(15));
    assert!(!budget.paused);
    assert_eq!(f.c.media_satisfaction(a).unwrap(), Observation::Known(true));
    assert_eq!(f.c.media_satisfaction(b).unwrap(), Observation::Known(true));
    let duplicate = Ack {
        ticket: req.ticket.clone(),
        result: AckResult::Confirmed(Confirmation::MediaBudget(MediaObservation {
            aggregate_revision: budget.aggregate_revision,
            observation_revision: 2,
            paused: false,
            fps: 15,
            width: 1920,
            height: 1080,
        })),
    };
    assert_eq!(
        f.c.ack(duplicate.clone(), 0).unwrap().status,
        RequestStatus::Confirmed
    );
    let mut conflict = duplicate;
    conflict.result = AckResult::Confirmed(Confirmation::Done);
    assert!(f.c.ack(conflict, 0).is_err());
    f.c.media_policy(sa.clone(), policy(MediaWork::Paused, 60), permission(), 0)
        .unwrap();
    let active = apply_budget(&mut f.c, a, 0, 3);
    assert!(matches!(
        active.kind,
        RequestKind::MediaBudget(MediaBudgetRequest { paused: false, .. })
    ));
    f.c.request(
        b,
        RequestKind::Presentation(Signals {
            visible: true,
            ..Default::default()
        }),
        permission(),
        0,
    )
    .unwrap();
    let visibility = f.c.dispatch(b, permission(), 0).unwrap().unwrap();
    ack(&mut f.c, visibility, Confirmation::Done, 0);
    f.c.media_policy(sb.clone(), policy(MediaWork::Paused, 15), permission(), 0)
        .unwrap();
    let paused = apply_budget(&mut f.c, b, 0, 4);
    assert!(matches!(
        paused.kind,
        RequestKind::MediaBudget(MediaBudgetRequest { paused: true, .. })
    ));
    f.c.set_eligibility(
        a,
        Eligibility {
            revision: 1,
            protected: true,
            host_allowed: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(f.c.media_satisfaction(a).unwrap(), Observation::Unknown);
    let pa = HostPermission {
        eligibility_revision: 1,
        ..permission()
    };
    f.c.media_policy(sa.clone(), policy(MediaWork::Paused, 60), pa, 0)
        .unwrap();
    let request = f.c.dispatch(a, pa, 0).unwrap().unwrap();
    let RequestKind::MediaBudget(budget) = request.kind else {
        unreachable!()
    };
    assert!(!budget.paused);
    let wrong = MediaObservation {
        aggregate_revision: budget.aggregate_revision,
        observation_revision: 5,
        paused: true,
        fps: 0,
        width: 1920,
        height: 1080,
    };
    assert!(f
        .c
        .ack(
            Ack {
                ticket: request.ticket.clone(),
                result: AckResult::Confirmed(Confirmation::MediaBudget(wrong))
            },
            0
        )
        .is_err());
    // A dispatched timeout remains an exact effect obligation; generic Applied does not invent actual work.
    f.c.dispatch(a, pa, 30_001).unwrap();
    assert_eq!(
        f.c.inspect().pending[&request.ticket.id].status,
        RequestStatus::Unknown
    );
    f.c.recover_instance(
        instance,
        RequestKind::Reconcile {
            uncertain: request.ticket.id,
        },
        pa,
        30_001,
    )
    .unwrap();
    let recovery =
        f.c.dispatch_recovery(instance, pa, 30_001)
            .unwrap()
            .unwrap();
    ack(
        &mut f.c,
        recovery,
        Confirmation::Reconciled {
            effect: Effect::Applied,
            lifecycle: None,
        },
        30_001,
    );
    assert_eq!(f.c.inspect().media[&instance].actual, Observation::Unknown);
    let current_revision = f.c.inspect().media[&instance].revision;
    f.c.observe_media(
        sa.clone(),
        MediaObservation {
            aggregate_revision: current_revision,
            observation_revision: 5,
            paused: false,
            fps: 10,
            width: 640,
            height: 480,
        },
    )
    .unwrap();
    assert!(f
        .c
        .observe_media(
            sa,
            MediaObservation {
                aggregate_revision: current_revision,
                observation_revision: 4,
                paused: false,
                fps: 10,
                width: 640,
                height: 480
            }
        )
        .is_err());
    assert!(MediaCeilings {
        fps: Some(241),
        ..Default::default()
    }
    .validate()
    .is_err());
    assert!(MediaCeilings {
        width: Some(u32::MAX),
        ..Default::default()
    }
    .validate()
    .is_err());
    // Exact upper host ceilings also apply to unconstrained consumers; lower actual work is allowed.
    let mut single = fixture(ContentLimits::default());
    let target = MountTarget::FrameBackground(single.frame);
    let mount = attached(&mut single, target, None);
    let scope = mount_scope(&single.c, mount);
    single
        .c
        .background_settings(
            scope.clone(),
            SettingsLayers::default(),
            BackgroundPolicy {
                allow_unconstrained: true,
                ..Default::default()
            },
            0,
        )
        .unwrap();
    single
        .c
        .media_host_ceilings(
            scope.clone(),
            MediaCeilings {
                fps: Some(8),
                width: Some(800),
                height: Some(600),
            },
        )
        .unwrap();
    single
        .c
        .media_policy(
            scope.clone(),
            MediaPolicy {
                work: MediaWork::Run,
                ceilings: MediaCeilings::unconstrained(),
            },
            permission(),
            0,
        )
        .unwrap();
    let req = single.c.dispatch(mount, permission(), 0).unwrap().unwrap();
    let RequestKind::MediaBudget(budget) = req.kind else {
        unreachable!()
    };
    assert_eq!(budget.ceilings.fps, Some(8));
    ack(
        &mut single.c,
        req,
        Confirmation::MediaBudget(MediaObservation {
            aggregate_revision: budget.aggregate_revision,
            observation_revision: 1,
            paused: false,
            fps: 5,
            width: 640,
            height: 480,
        }),
        0,
    );
    for result in [
        AckResult::Unsupported,
        AckResult::Rejected,
        AckResult::Failed { ambiguous: false },
    ] {
        single
            .c
            .media_policy(
                scope.clone(),
                policy(MediaWork::Throttle, 6),
                permission(),
                0,
            )
            .unwrap();
        let req = single.c.dispatch(mount, permission(), 0).unwrap().unwrap();
        single
            .c
            .ack(
                Ack {
                    ticket: req.ticket,
                    result,
                },
                0,
            )
            .unwrap();
        assert_eq!(
            single.c.inspect().media[&scope.instance].actual,
            Observation::Unknown
        );
    }
    let queued = single
        .c
        .media_policy(
            scope.clone(),
            policy(MediaWork::Throttle, 6),
            permission(),
            0,
        )
        .unwrap();
    single.c.advance(30_000).unwrap();
    assert!(!single.c.inspect().pending.contains_key(&queued));
    assert!(single
        .c
        .inspect()
        .receipts
        .iter()
        .any(|r| r.ticket.id == queued && r.status == RequestStatus::Cancelled));
    let mut changed = fixture(ContentLimits::default());
    let target = MountTarget::FrameBackground(changed.frame);
    let mount = attached(&mut changed, target, None);
    let scope = mount_scope(&changed.c, mount);
    let old = changed
        .c
        .media_policy(scope.clone(), policy(MediaWork::Run, 30), permission(), 0)
        .unwrap();
    let req = changed.c.dispatch(mount, permission(), 0).unwrap().unwrap();
    let RequestKind::MediaBudget(old_budget) = req.kind else {
        unreachable!()
    };
    changed
        .c
        .background_settings(
            scope.clone(),
            SettingsLayers::default(),
            BackgroundPolicy {
                allow_unconstrained: true,
                ..Default::default()
            },
            0,
        )
        .unwrap();
    assert_eq!(
        changed.c.inspect().pending[&old].status,
        RequestStatus::Unknown
    );
    assert_eq!(
        changed.c.inspect().instances[&scope.instance].busy,
        Some(old)
    );
    assert!(changed
        .c
        .ack(
            Ack {
                ticket: req.ticket,
                result: AckResult::Confirmed(Confirmation::MediaBudget(MediaObservation {
                    aggregate_revision: old_budget.aggregate_revision,
                    observation_revision: 1,
                    paused: false,
                    fps: 30,
                    width: 1920,
                    height: 1080
                }))
            },
            0
        )
        .is_err());
    let newer = changed
        .c
        .media_policy(
            scope.clone(),
            policy(MediaWork::Throttle, 10),
            permission(),
            0,
        )
        .unwrap();
    assert_eq!(
        changed.c.inspect().pending[&newer].status,
        RequestStatus::Queued
    );
    changed
        .c
        .recover_instance(
            scope.instance,
            RequestKind::Reconcile { uncertain: old },
            permission(),
            0,
        )
        .unwrap();
    let req = changed
        .c
        .dispatch_recovery(scope.instance, permission(), 0)
        .unwrap()
        .unwrap();
    ack(
        &mut changed.c,
        req,
        Confirmation::Reconciled {
            effect: Effect::Applied,
            lifecycle: None,
        },
        0,
    );
    assert_eq!(
        changed.c.inspect().media[&scope.instance].actual,
        Observation::Unknown
    );
    assert_eq!(
        changed.c.inspect().pending[&newer].status,
        RequestStatus::Queued
    );
    let newer_request = changed.c.dispatch(mount, permission(), 0).unwrap().unwrap();
    assert_eq!(newer_request.ticket.id, newer);
    let RequestKind::MediaBudget(newer_budget) = newer_request.kind else {
        unreachable!()
    };
    ack(
        &mut changed.c,
        newer_request,
        Confirmation::MediaBudget(MediaObservation {
            aggregate_revision: newer_budget.aggregate_revision,
            observation_revision: 1,
            paused: false,
            fps: 10,
            width: 1920,
            height: 1080,
        }),
        0,
    );
    assert!(matches!(
        changed.c.inspect().media[&scope.instance].actual,
        Observation::Known(_)
    ));
}

#[test]
fn eval_kum_011_012_exact_shared_cache_release_and_bounded_recovery() {
    let mut f = fixture(ContentLimits::default());
    let workspace = f.workspace;
    let frame = f.frame;
    let a = attached(&mut f, MountTarget::WorkspaceBackground(workspace), None);
    let instance = f.c.inspect().mounts[&a].instance;
    let b = attached(&mut f, MountTarget::FrameBackground(frame), Some(instance));
    f.c.media_policy(
        mount_scope(&f.c, a),
        policy(MediaWork::Run, 30),
        permission(),
        0,
    )
    .unwrap();
    apply_budget(&mut f.c, a, 0, 1);
    f.c.media_policy(
        mount_scope(&f.c, b),
        policy(MediaWork::Throttle, 15),
        permission(),
        0,
    )
    .unwrap();
    apply_budget(&mut f.c, b, 0, 2);
    let Observation::Known(old_actual) = f.c.inspect().media[&instance].actual else {
        unreachable!()
    };
    let cache =
        f.c.reserve_cache(a, 1, Some(100), None, false, permission())
            .unwrap();
    assert_eq!(
        f.c.reserve_cache(b, 1, Some(100), Some(cache), true, permission())
            .unwrap(),
        cache
    );
    let unchanged = f.c.inspect();
    let mut foreign = mount_scope(&f.c, a);
    foreign.session = "other-session".into();
    assert!(f
        .c
        .observe_media(
            foreign,
            MediaObservation {
                aggregate_revision: 1,
                observation_revision: 1,
                paused: false,
                fps: 10,
                width: 640,
                height: 480
            }
        )
        .is_err());
    assert_eq!(f.c.inspect(), unchanged);
    f.c.request(a, RequestKind::CacheRelease(cache), permission(), 0)
        .unwrap();
    let request = f.c.dispatch(a, permission(), 0).unwrap().unwrap();
    ack(&mut f.c, request, Confirmation::Done, 0);
    assert_eq!(f.c.inspect().caches[&cache].consumers, BTreeSet::from([b]));
    assert!(f.c.inspect().instances[&instance].reserved);
    let mut lowered = f.c.inspect().limits;
    lowered.pending = Some(0);
    lowered.queue = Some(0);
    lowered.retained_bytes = Some(0);
    f.c.set_limits(lowered).unwrap();
    assert!(f
        .c
        .request(b, RequestKind::Invalidate, permission(), 0)
        .is_err());
    f.c.recover_mount(a, RequestKind::Detach, permission(), 0)
        .unwrap();
    let request =
        f.c.dispatch_recovery(instance, permission(), 0)
            .unwrap()
            .unwrap();
    let old = request.ticket.clone();
    ack(&mut f.c, request, Confirmation::Done, 0);
    assert!(!f.c.inspect().mounts[&a].reserved);
    assert!(f.c.inspect().mounts[&b].reserved);
    assert!(f.c.inspect().instances[&instance].reserved);
    assert!(f.c.inspect().media[&instance].invalidated);
    let unchanged = f.c.inspect();
    assert!(f.c.observe_media(mount_scope(&f.c, b), old_actual).is_err());
    assert_eq!(f.c.inspect(), unchanged);
    assert!(f
        .c
        .recover_instance(instance, RequestKind::Dispose, permission(), 0)
        .is_err());
    let mut foreign = old;
    foreign.scope.session = "foreign".into();
    assert!(f
        .c
        .ack(
            Ack {
                ticket: foreign,
                result: AckResult::Confirmed(Confirmation::Done)
            },
            0
        )
        .is_err());
    f.c.recover_mount(b, RequestKind::CacheRelease(cache), permission(), 0)
        .unwrap();
    let request =
        f.c.dispatch_recovery(instance, permission(), 0)
            .unwrap()
            .unwrap();
    ack(&mut f.c, request, Confirmation::Done, 0);
    assert!(!f.c.inspect().caches.contains_key(&cache));
    assert!(f.c.inspect().instances[&instance].reserved);
    let before = f.c.inspect();
    f.c.recover_mount(b, RequestKind::Detach, permission(), 0)
        .unwrap();
    let request =
        f.c.dispatch_recovery(instance, permission(), 0)
            .unwrap()
            .unwrap();
    ack(&mut f.c, request, Confirmation::Done, 0);
    assert!(f.c.inspect().instances[&instance].reserved);
    assert!(f.c.inspect().counts.instances <= before.counts.instances);
    f.c.set_eligibility(
        a,
        Eligibility {
            revision: 1,
            host_allowed: true,
            can_destroy: true,
            recoverable: true,
            ..Default::default()
        },
    )
    .unwrap();
    let p = HostPermission {
        eligibility_revision: 1,
        ..permission()
    };
    f.c.recover_instance(instance, RequestKind::Dispose, p, 0)
        .unwrap();
    let request = f.c.dispatch_recovery(instance, p, 0).unwrap().unwrap();
    ack(&mut f.c, request, Confirmation::Done, 0);
    assert!(f
        .c
        .inspect()
        .instances
        .get(&instance)
        .is_none_or(|i| !i.reserved));
}
