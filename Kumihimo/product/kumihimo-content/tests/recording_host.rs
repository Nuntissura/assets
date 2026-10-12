use kumihimo_content::*;
use kumihimo_core::{
    self as core, Command, Core, EditingTarget, EntityId, Envelope, FrameId, ProviderId,
    ResourceId, ViewId, WorkspaceId,
};

fn permission() -> HostPermission {
    HostPermission {
        allowed: true,
        eligibility_revision: 0,
    }
}
fn envelope(c: &Core, command: Command) -> Envelope {
    Envelope {
        identity: c.snapshot().identity.clone(),
        actor: "recording-host".into(),
        operation: c.ledger_status().high_watermark.checked_next().unwrap(),
        expected_generation: c.snapshot().generation,
        command,
    }
}
fn canonical(c: &mut Core, command: Command) -> core::Change {
    c.execute(envelope(c, command)).outcome.result.unwrap()
}
fn created(c: &mut Core, command: Command) -> EntityId {
    match canonical(c, command) {
        core::Change::Created(id) => id,
        _ => panic!("expected creation"),
    }
}
struct Fixture {
    controller: Controller,
    workspace: WorkspaceId,
    frame: FrameId,
    other_frame: FrameId,
    provider: ProviderId,
    resource: ResourceId,
    views: Vec<ViewId>,
}
fn fixture(count: usize, limits: ContentLimits) -> Fixture {
    let mut c = Core::new(
        core::Identity::new("recording-layout", "core-1").unwrap(),
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
    let EntityId::Frame(other_frame) = created(
        &mut c,
        Command::CreateFrame {
            workspace,
            index: 1,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Provider(provider) = created(
        &mut c,
        Command::RegisterProvider {
            key: "recording-provider".into(),
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
            restore_reference: "host-recovery-reference".into(),
        },
    ) else {
        unreachable!()
    };
    let mut views = vec![];
    for index in 0..count {
        let EntityId::View(view) = created(
            &mut c,
            Command::CreateView {
                frame,
                resource,
                index,
            },
        ) else {
            unreachable!()
        };
        canonical(
            &mut c,
            Command::SetViewMetadata {
                view,
                label: format!("Document {index}"),
                pinned: false,
                unsaved: false,
            },
        );
        views.push(view);
    }
    Fixture {
        controller: Controller::new(c, "host-session-1", limits).unwrap(),
        workspace,
        frame,
        other_frame,
        provider,
        resource,
        views,
    }
}
fn negotiate(f: &mut Fixture, caps: Capabilities) {
    f.controller
        .negotiate(f.provider, ProtocolVersion::V1, caps.clone(), caps)
        .unwrap();
}
fn shared() -> Capabilities {
    Capabilities {
        sharing: true,
        ..Default::default()
    }
}
fn attached(
    f: &mut Fixture,
    host: &mut RecordingHost,
    index: usize,
    share: Option<InstanceId>,
) -> MountId {
    let mount = f
        .controller
        .attach(
            f.views[index],
            AttachOptions {
                mode: ContentMode::FullEditor,
                share,
                host_share: share.is_some(),
            },
            permission(),
            0,
        )
        .unwrap();
    host.complete(&mut f.controller, mount, 0);
    mount
}
#[derive(Default)]
struct RecordingHost {
    observed: Vec<Request>,
}
impl RecordingHost {
    fn pull(&mut self, c: &mut Controller, mount: MountId, now: u64) -> Request {
        let request = c
            .dispatch(mount, permission(), now)
            .unwrap()
            .expect("provider request");
        self.observed.push(request.clone());
        request
    }
    fn acknowledge(
        &mut self,
        c: &mut Controller,
        r: Request,
        result: AckResult,
        now: u64,
    ) -> Receipt {
        if !self.observed.iter().any(|old| old.ticket == r.ticket) {
            self.observed.push(r.clone());
        }
        c.ack(
            Ack {
                ticket: r.ticket,
                result,
            },
            now,
        )
        .unwrap()
    }
    fn complete(&mut self, c: &mut Controller, mount: MountId, now: u64) -> Request {
        let r = self.pull(c, mount, now);
        let result = match &r.kind {
            RequestKind::Attach { .. } => Confirmation::Attached {
                strength: RestorationStrength::LayoutOnly,
            },
            RequestKind::Geometry(g) => Confirmation::Geometry {
                anchor: g.anchor.clone(),
            },
            RequestKind::Lifecycle(state) => Confirmation::Lifecycle(*state),
            RequestKind::CloseDecision { .. } => Confirmation::Close { approved: true },
            _ => Confirmation::Done,
        };
        assert_eq!(
            self.acknowledge(c, r.clone(), AckResult::Confirmed(result), now)
                .status,
            RequestStatus::Confirmed
        );
        r
    }
}
fn geometry(x: f64, anchor: Option<&str>) -> RequestKind {
    RequestKind::Geometry(Geometry {
        rect: core::Rect::new(x, 0., 200., 100.).unwrap(),
        clip: core::Rect::new(x, 0., 200., 100.).unwrap(),
        scale: core::Scalar::new(1.).unwrap(),
        anchor: anchor.map(str::to_owned),
    })
}
fn input(payload: &str) -> RequestKind {
    RequestKind::Input(Input {
        owner: InputOwner::Content,
        payload: payload.into(),
        x: None,
        y: None,
    })
}
fn execute(f: &mut Fixture, command: Command) -> core::Response {
    let e = envelope(f.controller.core(), command);
    f.controller.execute(e).unwrap()
}

#[test]
fn eval_001_capability_version_and_placeholder() {
    let mut f = fixture(2, Default::default());
    let before = f.controller.inspect();
    assert!(matches!(
        f.controller.negotiate(
            f.provider,
            ProtocolVersion { major: 2, minor: 0 },
            shared(),
            shared()
        ),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(f.controller.inspect(), before);
    let placeholder = f.controller.placeholder(f.views[0]).unwrap();
    assert_eq!(placeholder.label, "Document 0");
    assert_eq!(placeholder.restore_reference, "host-recovery-reference");
    let caps = Capabilities {
        modes: [ContentMode::Preview].into_iter().collect(),
        input: false,
        ..shared()
    };
    negotiate(&mut f, caps);
    let before = f.controller.inspect();
    assert!(matches!(
        f.controller.attach(
            f.views[0],
            AttachOptions {
                mode: ContentMode::FullEditor,
                share: None,
                host_share: false
            },
            permission(),
            0
        ),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(f.controller.inspect(), before);
    let m = f
        .controller
        .attach(
            f.views[0],
            AttachOptions {
                mode: ContentMode::Preview,
                share: None,
                host_share: false,
            },
            permission(),
            0,
        )
        .unwrap();
    let mut host = RecordingHost::default();
    host.complete(&mut f.controller, m, 0);
    let before = f.controller.inspect();
    assert!(matches!(
        f.controller.request(m, input("edit"), permission(), 0),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(f.controller.inspect(), before);
    assert_eq!(host.observed.len(), 1);
}

#[test]
fn eval_002_segmented_causality_and_atomic_queue_bounds() {
    let mut f = fixture(2, Default::default());
    negotiate(&mut f, shared());
    let mut host = RecordingHost::default();
    let m = attached(&mut f, &mut host, 0, None);
    let a = f
        .controller
        .request(m, geometry(0., None), permission(), 0)
        .unwrap();
    let focus_in = f
        .controller
        .request(m, RequestKind::Focus(true), permission(), 0)
        .unwrap();
    let action = f
        .controller
        .request(m, input("barrier-input"), permission(), 0)
        .unwrap();
    let b = f
        .controller
        .request(m, geometry(50., None), permission(), 0)
        .unwrap();
    let focus_out = f
        .controller
        .request(m, RequestKind::Focus(false), permission(), 0)
        .unwrap();
    for id in [a, focus_in, action, b, focus_out] {
        assert_eq!(host.complete(&mut f.controller, m, 0).ticket.id, id);
    }
    let before = f.controller.inspect();
    assert!(f
        .controller
        .request(
            m,
            RequestKind::Input(Input {
                owner: InputOwner::FrameChrome,
                payload: "drag".into(),
                x: None,
                y: None
            }),
            permission(),
            0
        )
        .is_err());
    assert_eq!(f.controller.inspect(), before);
    let queued = f
        .controller
        .request(m, input("scoped"), permission(), 0)
        .unwrap();
    let queued_request = f.controller.inspect().pending[&queued].request.clone();
    let before = f.controller.inspect();
    assert!(matches!(
        f.controller.ack(
            Ack {
                ticket: queued_request.ticket.clone(),
                result: AckResult::Confirmed(Confirmation::Done)
            },
            0
        ),
        Err(Error::Stale(_))
    ));
    assert_eq!(f.controller.inspect(), before);
    let request = host.pull(&mut f.controller, m, 0);
    let before = f.controller.inspect();
    for field in 0..3 {
        let mut wrong = request.ticket.clone();
        match field {
            0 => wrong.scope.provider = ProviderId::from_raw(999),
            1 => wrong.scope.view = ViewId::from_raw(999),
            _ => wrong.scope.resource = ResourceId::from_raw(999),
        }
        assert!(matches!(
            f.controller.ack(
                Ack {
                    ticket: wrong,
                    result: AckResult::Confirmed(Confirmation::Done)
                },
                0
            ),
            Err(Error::Conflict(_))
        ));
        assert_eq!(f.controller.inspect(), before);
    }
    host.acknowledge(
        &mut f.controller,
        request,
        AckResult::Confirmed(Confirmation::Done),
        0,
    );
    f.controller
        .set_limits(ContentLimits {
            queue: Some(1),
            pending: Some(1),
            ..Default::default()
        })
        .unwrap();
    f.controller
        .request(m, input("slow"), permission(), 0)
        .unwrap();
    let before = f.controller.inspect();
    assert!(matches!(
        f.controller.request(m, input("overflow"), permission(), 0),
        Err(Error::Admission { .. })
    ));
    assert_eq!(f.controller.inspect(), before);
}

#[test]
fn eval_003_epochs_restore_replay_and_exact_ack() {
    let mut f = fixture(2, Default::default());
    negotiate(&mut f, shared());
    let mut host = RecordingHost::default();
    let m = f
        .controller
        .attach(
            f.views[0],
            AttachOptions {
                mode: ContentMode::FullEditor,
                share: None,
                host_share: false,
            },
            permission(),
            0,
        )
        .unwrap();
    let request = host.pull(&mut f.controller, m, 0);
    let view = f.views[0];
    execute(
        &mut f,
        Command::SetViewMetadata {
            view,
            label: "renamed".into(),
            pinned: false,
            unsaved: false,
        },
    );
    let ack = Ack {
        ticket: request.ticket.clone(),
        result: AckResult::Confirmed(Confirmation::Attached {
            strength: RestorationStrength::LayoutOnly,
        }),
    };
    assert_eq!(host.observed[0], request);
    let receipt = f.controller.ack(ack.clone(), 0).unwrap();
    let before = f.controller.inspect();
    assert_eq!(f.controller.ack(ack.clone(), 0).unwrap(), receipt);
    assert_eq!(f.controller.inspect(), before);
    let mut conflict = ack.clone();
    conflict.result = AckResult::Rejected;
    assert!(matches!(
        f.controller.ack(conflict, 0),
        Err(Error::Conflict(_))
    ));
    assert_eq!(f.controller.inspect(), before);
    let mut foreign = ack;
    foreign.ticket.scope.session = "foreign-session".into();
    assert!(matches!(
        f.controller.ack(foreign, 0),
        Err(Error::SessionMismatch)
    ));
    assert_eq!(f.controller.inspect(), before);
    let old_pending = f
        .controller
        .request(m, RequestKind::Invalidate, permission(), 0)
        .unwrap();
    let old_request = host.pull(&mut f.controller, m, 0);
    let saved = f.controller.core().save_layout();
    let options = core::RestoreOptions {
        providers: vec![core::ProviderResolution {
            key: "recording-provider".into(),
            supports_shared_views: true,
        }],
        ..Default::default()
    };
    let stale_token = f
        .controller
        .core()
        .prepare_restore(saved.clone(), options.clone())
        .unwrap();
    execute(
        &mut f,
        Command::SetViewMetadata {
            view,
            label: "unrelated rename".into(),
            pinned: false,
            unsaved: false,
        },
    );
    let before = f.controller.inspect();
    let rejected = envelope(f.controller.core(), Command::Restore(stale_token));
    assert!(f
        .controller
        .execute(rejected)
        .unwrap()
        .outcome
        .result
        .is_err());
    assert_eq!(f.controller.inspect(), before);
    assert!(f.controller.inspect().mounts[&m].active);
    let saved = f.controller.core().save_layout();
    let token = f.controller.core().prepare_restore(saved, options).unwrap();
    let e = envelope(f.controller.core(), Command::Restore(token));
    assert!(!f.controller.execute(e.clone()).unwrap().replayed);
    assert!(!f.controller.inspect().mounts[&m].active);
    let before = f.controller.inspect();
    assert!(f.controller.execute(e).unwrap().replayed);
    assert_eq!(f.controller.inspect(), before);
    let before = f.controller.inspect();
    assert!(matches!(
        f.controller.ack(
            Ack {
                ticket: old_request.ticket,
                result: AckResult::Confirmed(Confirmation::Done)
            },
            0
        ),
        Err(Error::Stale(_))
    ));
    assert_eq!(f.controller.inspect(), before);
    let replacement = attached(&mut f, &mut host, 0, None);
    let old_instance = f.controller.inspect().mounts[&m].instance;
    let reconciliation = f
        .controller
        .recover_mount(
            m,
            RequestKind::Reconcile {
                uncertain: old_pending,
            },
            permission(),
            0,
        )
        .unwrap();
    let recovery = f
        .controller
        .dispatch_recovery(old_instance, permission(), 0)
        .unwrap()
        .unwrap();
    assert_eq!(recovery.ticket.id, reconciliation);
    let old_ack = Ack {
        ticket: recovery.ticket.clone(),
        result: AckResult::Confirmed(Confirmation::Reconciled {
            effect: Effect::NotApplied,
            lifecycle: None,
        }),
    };
    host.acknowledge(&mut f.controller, recovery, old_ack.result.clone(), 0);
    assert!(!f.controller.inspect().mounts[&m].active);
    assert!(f.controller.inspect().mounts[&replacement].active);
    let cleanup = f
        .controller
        .recover_mount(m, RequestKind::Detach, permission(), 0)
        .unwrap();
    assert_ne!(cleanup, reconciliation);
    let before = f.controller.inspect();
    f.controller.ack(old_ack, 0).unwrap();
    assert_eq!(f.controller.inspect(), before);
    let cleanup = f
        .controller
        .dispatch_recovery(old_instance, permission(), 0)
        .unwrap()
        .unwrap();
    host.acknowledge(
        &mut f.controller,
        cleanup,
        AckResult::Confirmed(Confirmation::Done),
        0,
    );
    assert!(f.controller.inspect().mounts[&replacement].active);
    let mut other = fixture(2, Default::default());
    negotiate(&mut other, shared());
    let before = other.controller.inspect();
    let foreign = Ack {
        ticket: receipt.ticket,
        result: AckResult::Confirmed(Confirmation::Done),
    };
    // Matching numbers cannot replace a different host-supplied session.
    let mut foreign = foreign;
    foreign.ticket.scope.session = "prior-controller-session".into();
    assert!(matches!(
        other.controller.ack(foreign, 0),
        Err(Error::SessionMismatch)
    ));
    assert_eq!(other.controller.inspect(), before);
    let mut c = Core::new(
        core::Identity::new("counter-layout", "counter-core").unwrap(),
        Default::default(),
        8,
    )
    .unwrap();
    let EntityId::Provider(provider) = created(
        &mut c,
        Command::RegisterProvider {
            key: "counter-provider".into(),
            supports_shared_views: false,
        },
    ) else {
        unreachable!()
    };
    let mut exhausted =
        Controller::new_with_id_start(c, "counter-session", Default::default(), u64::MAX).unwrap();
    let before = exhausted.inspect();
    assert!(matches!(
        exhausted.negotiate(
            provider,
            ProtocolVersion::V1,
            Default::default(),
            Default::default()
        ),
        Err(Error::Exhausted(_))
    ));
    assert_eq!(exhausted.inspect(), before);
}

#[test]
fn eval_004_lifecycle_unknown_timeout_and_reconciliation() {
    let mut f = fixture(2, Default::default());
    negotiate(&mut f, shared());
    let mut host = RecordingHost::default();
    let m = attached(&mut f, &mut host, 0, None);
    let id = f
        .controller
        .request(
            m,
            RequestKind::Lifecycle(LifecycleState::Background),
            permission(),
            0,
        )
        .unwrap();
    assert_ne!(
        f.controller.inspect().mounts[&m].confirmed,
        Observation::Known(LifecycleState::Background)
    );
    let request = host.pull(&mut f.controller, m, 0);
    assert_eq!(request.ticket.id, id);
    assert_eq!(request.deadline, 30_000);
    assert!(f.controller.dispatch(m, permission(), 0).unwrap().is_none());
    let independent = attached(&mut f, &mut host, 1, None);
    f.controller
        .request(independent, input("independent instance"), permission(), 0)
        .unwrap();
    let independent_request = host.complete(&mut f.controller, independent, 0);
    assert_ne!(
        independent_request.ticket.scope.instance,
        request.ticket.scope.instance
    );
    f.controller.advance(30_000).unwrap();
    assert_eq!(
        f.controller.inspect().pending[&id].status,
        RequestStatus::Unknown
    );
    let before = f.controller.inspect();
    assert!(matches!(
        f.controller.advance(29_999),
        Err(Error::TimeWentBack)
    ));
    assert_eq!(f.controller.inspect(), before);
    let recovery = f
        .controller
        .recover_mount(
            m,
            RequestKind::Reconcile { uncertain: id },
            permission(),
            30_000,
        )
        .unwrap();
    let instance = f.controller.inspect().mounts[&m].instance;
    let request = f
        .controller
        .dispatch_recovery(instance, permission(), 30_000)
        .unwrap()
        .unwrap();
    assert_eq!(request.ticket.id, recovery);
    host.acknowledge(
        &mut f.controller,
        request,
        AckResult::Confirmed(Confirmation::Reconciled {
            effect: Effect::NotApplied,
            lifecycle: Some(LifecycleState::Visible),
        }),
        30_000,
    );
    assert_ne!(
        f.controller.inspect().mounts[&m].status,
        MountStatus::Unknown
    );
    let mut overflow = fixture(2, Default::default());
    negotiate(&mut overflow, shared());
    let before = overflow.controller.inspect();
    assert!(matches!(
        overflow.controller.attach(
            overflow.views[0],
            Default::default(),
            permission(),
            u64::MAX
        ),
        Err(Error::Exhausted(_))
    ));
    assert_eq!(overflow.controller.inspect(), before);
    for state in [
        LifecycleState::Visible,
        LifecycleState::Background,
        LifecycleState::Throttled,
        LifecycleState::Frozen,
        LifecycleState::Restoring,
        LifecycleState::Failed,
        LifecycleState::Unloaded,
    ] {
        let mut g = fixture(2, Default::default());
        negotiate(&mut g, shared());
        let mut recorder = RecordingHost::default();
        let mount = attached(&mut g, &mut recorder, 0, None);
        g.controller
            .set_eligibility(
                mount,
                Eligibility {
                    revision: 1,
                    can_destroy: true,
                    recoverable: true,
                    host_allowed: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let p = HostPermission {
            allowed: true,
            eligibility_revision: 1,
        };
        g.controller
            .request(mount, RequestKind::Lifecycle(state), p, 0)
            .unwrap();
        assert_eq!(g.controller.inspect().mounts[&mount].requested, Some(state));
        let r = g.controller.dispatch(mount, p, 0).unwrap().unwrap();
        let receipt = recorder.acknowledge(
            &mut g.controller,
            r,
            AckResult::Confirmed(Confirmation::Lifecycle(state)),
            0,
        );
        assert_eq!(receipt.status, RequestStatus::Confirmed);
        if state == LifecycleState::Unloaded {
            assert_eq!(g.controller.inspect().counts.instances, 0);
        } else {
            assert_eq!(
                g.controller.inspect().mounts[&mount].confirmed,
                Observation::Known(state)
            );
        }
    }
    for (result, status) in [
        (AckResult::Unsupported, RequestStatus::Unsupported),
        (AckResult::Rejected, RequestStatus::Rejected),
        (
            AckResult::Failed { ambiguous: false },
            RequestStatus::Failed,
        ),
    ] {
        let mut g = fixture(2, Default::default());
        negotiate(&mut g, shared());
        let mut recorder = RecordingHost::default();
        let mount = attached(&mut g, &mut recorder, 0, None);
        g.controller
            .request(mount, RequestKind::Invalidate, permission(), 0)
            .unwrap();
        let r = recorder.pull(&mut g.controller, mount, 0);
        assert_eq!(
            recorder.acknowledge(&mut g.controller, r, result, 0).status,
            status
        );
        assert_eq!(g.controller.inspect().counts.instances, 1);
    }
    let mut unsent = fixture(2, Default::default());
    negotiate(&mut unsent, shared());
    let mount = unsent
        .controller
        .attach(unsent.views[0], Default::default(), permission(), 0)
        .unwrap();
    let id = unsent.controller.inspect().queue[0];
    unsent.controller.cancel(id, 0).unwrap();
    assert_eq!(unsent.controller.inspect().counts.mounts, 0);
    assert_eq!(unsent.controller.inspect().counts.instances, 0);
    assert_eq!(
        unsent.controller.inspect().receipts.last().unwrap().status,
        RequestStatus::Cancelled
    );
    assert!(unsent.controller.inspect().mounts.get(&mount).is_none());
    for ambiguous in [false, true] {
        let mut g = fixture(2, Default::default());
        negotiate(&mut g, shared());
        let mut recorder = RecordingHost::default();
        let mount = attached(&mut g, &mut recorder, 0, None);
        let id = g
            .controller
            .request(mount, RequestKind::Invalidate, permission(), 0)
            .unwrap();
        let r = recorder.pull(&mut g.controller, mount, 0);
        if ambiguous {
            assert_eq!(
                recorder
                    .acknowledge(
                        &mut g.controller,
                        r.clone(),
                        AckResult::Failed { ambiguous: true },
                        0
                    )
                    .status,
                RequestStatus::Unknown
            );
        } else {
            g.controller.cancel(id, 0).unwrap();
        }
        assert_eq!(
            g.controller.inspect().pending[&id].status,
            RequestStatus::Unknown
        );
        assert_eq!(g.controller.inspect().counts.instances, 1);
        let before = g.controller.inspect();
        assert!(matches!(
            g.controller
                .recover_mount(mount, RequestKind::Detach, permission(), 0),
            Err(Error::Busy(_))
        ));
        assert_eq!(g.controller.inspect(), before);
        recorder.acknowledge(
            &mut g.controller,
            r,
            AckResult::Confirmed(Confirmation::Done),
            0,
        );
        assert_eq!(
            g.controller.inspect().mounts[&mount].status,
            MountStatus::Attached
        );
        assert_eq!(g.controller.inspect().counts.instances, 1);
    }
    let mut evict = fixture(
        2,
        ContentLimits {
            receipts: Some(1),
            ..Default::default()
        },
    );
    negotiate(&mut evict, shared());
    let mut recorder = RecordingHost::default();
    let mount = attached(&mut evict, &mut recorder, 0, None);
    let old = recorder.observed[0].clone();
    evict
        .controller
        .request(mount, RequestKind::Invalidate, permission(), 0)
        .unwrap();
    recorder.complete(&mut evict.controller, mount, 0);
    let before = evict.controller.inspect();
    assert!(matches!(
        evict.controller.ack(
            Ack {
                ticket: old.ticket,
                result: AckResult::Confirmed(Confirmation::Attached {
                    strength: RestorationStrength::LayoutOnly
                })
            },
            0
        ),
        Err(Error::Expired(_))
    ));
    assert_eq!(evict.controller.inspect(), before);
}

#[test]
fn eval_005_shared_moves_close_veto_and_historical_close() {
    for changed in 0..3 {
        let mut g = fixture(2, Default::default());
        negotiate(
            &mut g,
            Capabilities {
                close_mode: CloseMode::ProviderVeto,
                ..shared()
            },
        );
        let mut recorder = RecordingHost::default();
        let mount = attached(&mut g, &mut recorder, 0, None);
        g.controller
            .request(
                mount,
                RequestKind::CloseDecision {
                    eligibility_revision: 0,
                },
                permission(),
                0,
            )
            .unwrap();
        let reply = recorder.pull(&mut g.controller, mount, 0);
        let view = g.views[0];
        match changed {
            0 => {
                execute(
                    &mut g,
                    Command::SetViewMetadata {
                        view,
                        label: "dirty".into(),
                        pinned: false,
                        unsaved: true,
                    },
                );
            }
            1 => {
                execute(
                    &mut g,
                    Command::SetViewMetadata {
                        view,
                        label: "pinned".into(),
                        pinned: true,
                        unsaved: false,
                    },
                );
            }
            _ => {
                g.controller
                    .set_eligibility(
                        mount,
                        Eligibility {
                            revision: 1,
                            protected: true,
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
        }
        let before = g.controller.inspect();
        assert!(matches!(
            g.controller.ack(
                Ack {
                    ticket: reply.ticket,
                    result: AckResult::Confirmed(Confirmation::Close { approved: true })
                },
                0
            ),
            Err(Error::Stale(_))
        ));
        assert_eq!(g.controller.inspect(), before);
        assert!(g.controller.snapshot().views[&view].closed.is_none());
    }
    let mut f = fixture(2, Default::default());
    negotiate(
        &mut f,
        Capabilities {
            close_mode: CloseMode::ProviderVeto,
            ..shared()
        },
    );
    let mut host = RecordingHost::default();
    let first = attached(&mut f, &mut host, 0, None);
    let instance = f.controller.inspect().mounts[&first].instance;
    let second = attached(&mut f, &mut host, 1, Some(instance));
    let before_resource = f.controller.snapshot().views[&f.views[0]].resource;
    let view = f.views[0];
    let mut zero = envelope(
        f.controller.core(),
        Command::CloseView {
            view,
            approved: true,
        },
    );
    zero.operation = core::OperationId::new(0);
    assert_eq!(
        f.controller.execute(zero).unwrap().outcome.result,
        Err(core::Error::ZeroOperation)
    );
    let mut stale = envelope(
        f.controller.core(),
        Command::CloseView {
            view,
            approved: true,
        },
    );
    stale.expected_generation -= 1;
    assert!(matches!(
        f.controller.execute(stale).unwrap().outcome.result,
        Err(core::Error::StaleGeneration { .. })
    ));
    let missing = ViewId::from_raw(999);
    let response = execute(
        &mut f,
        Command::CloseView {
            view: missing,
            approved: false,
        },
    );
    assert_eq!(
        response.outcome.result,
        Err(core::Error::Missing(EntityId::View(missing)))
    );
    let destination = f.other_frame;
    execute(
        &mut f,
        Command::MoveView {
            view,
            destination,
            index: 0,
        },
    );
    assert_eq!(
        f.controller.snapshot().views[&view].resource,
        before_resource
    );
    assert!(f.controller.inspect().mounts[&first].active);
    assert_eq!(f.controller.inspect().mounts[&first].instance, instance);
    let close = envelope(
        f.controller.core(),
        Command::CloseView {
            view,
            approved: true,
        },
    );
    let before = f.controller.inspect();
    let ledger = f.controller.core().ledger_status();
    assert!(f.controller.execute(close.clone()).is_err());
    assert_eq!(f.controller.inspect(), before);
    assert_eq!(f.controller.core().ledger_status(), ledger);
    f.controller
        .request(
            first,
            RequestKind::CloseDecision {
                eligibility_revision: 0,
            },
            permission(),
            0,
        )
        .unwrap();
    host.complete(&mut f.controller, first, 0);
    let denied = envelope(
        f.controller.core(),
        Command::CloseView {
            view,
            approved: false,
        },
    );
    assert_eq!(
        f.controller.execute(denied).unwrap().outcome.result,
        Err(core::Error::CloseVeto(view))
    );
    let close = envelope(
        f.controller.core(),
        Command::CloseView {
            view,
            approved: true,
        },
    );
    assert!(f
        .controller
        .execute(close.clone())
        .unwrap()
        .outcome
        .result
        .is_ok());
    let destination = f.other_frame;
    execute(&mut f, Command::ReopenView { view, destination });
    assert!(f.controller.snapshot().views[&view].closed.is_none());
    assert!(f.controller.execute(close).unwrap().replayed);
    assert!(f.controller.snapshot().views[&view].closed.is_none());
    assert!(f.controller.inspect().mounts[&second].reserved);
    let recorded = host.observed.len();
    drop(f.controller);
    assert_eq!(host.observed.len(), recorded);
    let mut host_only = fixture(2, Default::default());
    negotiate(&mut host_only, shared());
    let mut recorder = RecordingHost::default();
    attached(&mut host_only, &mut recorder, 0, None);
    let target = host_only.views[0];
    assert!(execute(
        &mut host_only,
        Command::CloseView {
            view: target,
            approved: true
        }
    )
    .outcome
    .result
    .is_ok());
    assert_eq!(
        execute(
            &mut host_only,
            Command::CloseView {
                view: target,
                approved: false
            }
        )
        .outcome
        .result,
        Ok(core::Change::Unchanged)
    );
}

#[test]
fn eval_006_saved_live_protection_and_saturated_recovery() {
    for protection in 0..5 {
        let mut g = fixture(2, Default::default());
        negotiate(&mut g, shared());
        let mut recorder = RecordingHost::default();
        let mount = attached(&mut g, &mut recorder, 0, None);
        let mut facts = Eligibility {
            revision: 1,
            can_destroy: true,
            recoverable: true,
            host_allowed: true,
            ..Default::default()
        };
        match protection {
            0 => facts.keep_alive = true,
            1 => facts.unsaved = true,
            2 => facts.audio = true,
            3 => facts.download = true,
            _ => facts.recoverable = false,
        }
        g.controller.set_eligibility(mount, facts).unwrap();
        let before = g.controller.inspect();
        assert!(g
            .controller
            .request(
                mount,
                RequestKind::Lifecycle(LifecycleState::Unloaded),
                HostPermission {
                    allowed: true,
                    eligibility_revision: 1
                },
                0
            )
            .is_err());
        assert_eq!(g.controller.inspect(), before);
        assert_eq!(before.counts.instances, 1);
    }
    let limits = ContentLimits {
        instances: Some(1),
        mounts: Some(2),
        leases: Some(3),
        ..Default::default()
    };
    let mut f = fixture(20, limits);
    negotiate(&mut f, shared());
    let mut host = RecordingHost::default();
    let m = attached(&mut f, &mut host, 0, None);
    let instance = f.controller.inspect().mounts[&m].instance;
    let second = attached(&mut f, &mut host, 1, Some(instance));
    assert_eq!(f.controller.snapshot().views.len(), 20);
    assert_eq!(f.controller.inspect().counts.instances, 1);
    assert_eq!(f.controller.inspect().counts.mounts, 2);
    let before = f.controller.inspect();
    assert!(f
        .controller
        .attach(
            f.views[2],
            AttachOptions {
                mode: ContentMode::Preview,
                share: None,
                host_share: false
            },
            permission(),
            0
        )
        .is_err());
    assert_eq!(f.controller.inspect(), before);
    f.controller
        .set_eligibility(
            m,
            Eligibility {
                revision: 1,
                can_destroy: true,
                recoverable: true,
                host_allowed: true,
                ..Default::default()
            },
        )
        .unwrap();
    f.controller
        .set_eligibility(
            second,
            Eligibility {
                revision: 1,
                audio: true,
                can_destroy: true,
                recoverable: true,
                host_allowed: true,
                ..Default::default()
            },
        )
        .unwrap();
    let current_permission = HostPermission {
        allowed: true,
        eligibility_revision: 1,
    };
    let before = f.controller.inspect();
    assert!(f
        .controller
        .request(
            m,
            RequestKind::Lifecycle(LifecycleState::Unloaded),
            current_permission,
            0
        )
        .is_err());
    assert_eq!(f.controller.inspect(), before);
    f.controller
        .set_limits(ContentLimits {
            pending: Some(0),
            queue: Some(0),
            leases: Some(0),
            ..Default::default()
        })
        .unwrap();
    let before = f.controller.inspect();
    assert!(f
        .controller
        .request(m, input("bounded"), current_permission, 0)
        .is_err());
    assert_eq!(f.controller.inspect(), before);
    let id = f
        .controller
        .recover_mount(m, RequestKind::Detach, current_permission, 0)
        .unwrap();
    let request = f
        .controller
        .dispatch_recovery(instance, current_permission, 0)
        .unwrap()
        .unwrap();
    assert_eq!(request.ticket.id, id);
    host.acknowledge(
        &mut f.controller,
        request,
        AckResult::Confirmed(Confirmation::Done),
        0,
    );
    assert!(!f.controller.inspect().mounts[&m].reserved);
    assert!(f.controller.inspect().mounts[&second].reserved);
    let mut bytes = fixture(2, Default::default());
    negotiate(&mut bytes, shared());
    let mut recorder = RecordingHost::default();
    let mount = attached(&mut bytes, &mut recorder, 0, None);
    bytes
        .controller
        .set_limits(ContentLimits {
            retained_bytes: Some(8),
            payload_bytes: Some(16),
            ..Default::default()
        })
        .unwrap();
    bytes
        .controller
        .request(mount, input("12345"), permission(), 0)
        .unwrap();
    let before = bytes.controller.inspect();
    assert!(matches!(
        bytes
            .controller
            .request(mount, input("67890"), permission(), 0),
        Err(Error::Admission { .. })
    ));
    assert_eq!(bytes.controller.inspect(), before);
    bytes
        .controller
        .set_limits(ContentLimits {
            payload_bytes: Some(4),
            ..Default::default()
        })
        .unwrap();
    let before = bytes.controller.inspect();
    assert!(matches!(
        bytes
            .controller
            .request(mount, input("12345"), permission(), 0),
        Err(Error::Admission { .. })
    ));
    assert_eq!(bytes.controller.inspect(), before);
}

#[test]
fn eval_007_placeholder_recovery_and_restoration_strength() {
    let mut f = fixture(2, Default::default());
    let original = f.controller.snapshot().clone();
    assert!(f.controller.placeholder(f.views[0]).is_some());
    negotiate(&mut f, shared());
    let mut host = RecordingHost::default();
    let mount = f
        .controller
        .attach(
            f.views[0],
            AttachOptions {
                mode: ContentMode::Preview,
                share: None,
                host_share: false,
            },
            permission(),
            0,
        )
        .unwrap();
    let request = host.pull(&mut f.controller, mount, 0);
    host.acknowledge(
        &mut f.controller,
        request,
        AckResult::Confirmed(Confirmation::Attached {
            strength: RestorationStrength::PageReload,
        }),
        0,
    );
    assert_eq!(
        f.controller.inspect().mounts[&mount].restoration,
        RestorationStrength::PageReload
    );
    let provider = f.provider;
    execute(
        &mut f,
        Command::SetProviderAvailability {
            provider,
            availability: core::ProviderAvailability::Missing,
        },
    );
    let placeholder = f.controller.placeholder(f.views[0]).unwrap();
    assert_eq!(placeholder.label, "Document 0");
    assert_eq!(placeholder.restore_reference, "host-recovery-reference");
    assert_eq!(f.controller.snapshot().resources, original.resources);
    assert_eq!(f.controller.snapshot().views.len(), original.views.len());
    let before = f.controller.inspect();
    assert!(f
        .controller
        .attach(
            f.views[0],
            AttachOptions {
                mode: ContentMode::FullEditor,
                share: None,
                host_share: false
            },
            permission(),
            0
        )
        .is_err());
    assert_eq!(f.controller.inspect(), before);
    execute(
        &mut f,
        Command::SetProviderAvailability {
            provider,
            availability: core::ProviderAvailability::Available,
        },
    );
    negotiate(&mut f, shared());
    let new = attached(&mut f, &mut host, 0, None);
    assert_ne!(new, mount);
    assert_eq!(
        f.controller.inspect().mounts[&new].restoration,
        RestorationStrength::LayoutOnly
    );
    for strength in [
        RestorationStrength::ProviderDocumentState,
        RestorationStrength::Unknown,
    ] {
        let mut g = fixture(2, Default::default());
        negotiate(&mut g, shared());
        let mut recorder = RecordingHost::default();
        let m = g
            .controller
            .attach(g.views[0], Default::default(), permission(), 0)
            .unwrap();
        let request = recorder.pull(&mut g.controller, m, 0);
        recorder.acknowledge(
            &mut g.controller,
            request,
            AckResult::Confirmed(Confirmation::Attached { strength }),
            0,
        );
        assert_eq!(g.controller.inspect().mounts[&m].restoration, strength);
        g.controller
            .request(
                m,
                RequestKind::Lifecycle(LifecycleState::Restoring),
                permission(),
                0,
            )
            .unwrap();
        let request = recorder.pull(&mut g.controller, m, 0);
        let receipt = recorder.acknowledge(&mut g.controller, request, AckResult::Rejected, 0);
        assert_eq!(receipt.status, RequestStatus::Rejected);
        assert_eq!(
            g.controller.inspect().mounts[&m].requested,
            Some(LifecycleState::Restoring)
        );
    }
}

#[test]
fn eval_008_assets_and_pinned_tool_delivery() {
    let mut f = fixture(2, Default::default());
    negotiate(&mut f, shared());
    let mut host = RecordingHost::default();
    let m = attached(&mut f, &mut host, 0, None);
    let asset = HostAssetReference {
        identity: "asset-opaque".into(),
        revision: "revision-1".into(),
    };
    for (mode, undo) in [
        (AssetMode::LinkedEdit, UndoScope::HostDocument),
        (AssetMode::IndependentCopy, UndoScope::HostDocument),
        (AssetMode::DomainChange, UndoScope::HostDomain),
    ] {
        f.controller
            .request(
                m,
                RequestKind::AssetAction(AssetAction {
                    asset: asset.clone(),
                    mode,
                    undo,
                    payload: "host action".into(),
                }),
                permission(),
                0,
            )
            .unwrap();
        let r = host.complete(&mut f.controller, m, 0);
        assert!(matches!(r.kind, RequestKind::AssetAction(_)));
    }
    let before = f.controller.inspect();
    assert!(f
        .controller
        .request(
            m,
            RequestKind::AssetAction(AssetAction {
                asset: HostAssetReference {
                    identity: String::new(),
                    revision: "r".into()
                },
                mode: AssetMode::LinkedEdit,
                undo: UndoScope::HostDocument,
                payload: String::new()
            }),
            permission(),
            0
        )
        .is_err());
    assert_eq!(f.controller.inspect(), before);
    let view = f.views[0];
    let resource = f.resource;
    let provider = f.provider;
    let workspace = f.workspace;
    execute(
        &mut f,
        Command::SetGeometryContext {
            workspace,
            context: core::GeometryContext::new(core::Rect::new(0., 0., 800., 600.).unwrap()),
        },
    );
    execute(
        &mut f,
        Command::SetViewState {
            view,
            state: core::ViewState {
                visible: true,
                usable: true,
            },
        },
    );
    let other = f.views[1];
    execute(
        &mut f,
        Command::SetViewState {
            view: other,
            state: core::ViewState {
                visible: true,
                usable: true,
            },
        },
    );
    let target = EditingTarget { view, resource };
    let alternative = EditingTarget {
        view: f.views[1],
        resource,
    };
    let placement = core::ToolPlacement::Edge {
        anchor: core::ToolAnchor::View(view),
        edge: core::Edge::Top,
    };
    let mut policy = core::ToolPolicy::new(
        core::Size::new(100., 10.).unwrap(),
        core::Size::new(10., 10.).unwrap(),
        vec![placement],
    );
    policy.compatible_targets = vec![target, alternative];
    let response = execute(
        &mut f,
        Command::CreateTool(core::ToolSpec {
            owner: provider,
            origin: view,
            target,
            placement,
            policy,
            state: core::TargetState {
                target_visible: true,
                target_usable: true,
                origin_visible: true,
                origin_usable: true,
                applicable: true,
                keyboard_focused: false,
            },
        }),
    );
    let Ok(core::Change::Created(EntityId::Tool(tool))) = response.outcome.result else {
        panic!("tool created")
    };
    f.controller
        .request(
            m,
            RequestKind::ToolAction {
                tool,
                target,
                payload: "apply".into(),
            },
            permission(),
            0,
        )
        .unwrap();
    execute(
        &mut f,
        Command::RebindTool {
            tool,
            target: alternative,
        },
    );
    execute(&mut f, Command::RebindTool { tool, target });
    let delivered = f.controller.dispatch(m, permission(), 0);
    assert!(delivered.is_err() || delivered.unwrap().is_none());
    assert_eq!(f.controller.snapshot().tools[&tool].target, target);
}

#[test]
fn eval_009_geometry_scale_embed_and_anchored_reflow() {
    let mut f = fixture(2, Default::default());
    negotiate(
        &mut f,
        Capabilities {
            anchored_reflow: true,
            embed_input: true,
            ..shared()
        },
    );
    let mut host = RecordingHost::default();
    let m = attached(&mut f, &mut host, 0, None);
    let a = f
        .controller
        .request(
            m,
            geometry(0., Some("selection-scroll-anchor")),
            permission(),
            0,
        )
        .unwrap();
    f.controller
        .request(m, geometry(30., None), permission(), 0)
        .unwrap();
    let r = host.pull(&mut f.controller, m, 0);
    assert_eq!(r.ticket.id, a);
    let before = f.controller.inspect();
    let bad = Ack {
        ticket: r.ticket.clone(),
        result: AckResult::Confirmed(Confirmation::Geometry {
            anchor: Some("different".into()),
        }),
    };
    assert!(f.controller.ack(bad, 0).is_err());
    assert_eq!(f.controller.inspect(), before);
    host.acknowledge(
        &mut f.controller,
        r,
        AckResult::Confirmed(Confirmation::Geometry {
            anchor: Some("selection-scroll-anchor".into()),
        }),
        0,
    );
    host.complete(&mut f.controller, m, 0);
    let mut ordinary = fixture(2, Default::default());
    negotiate(&mut ordinary, shared());
    let mut recorder = RecordingHost::default();
    let mount = attached(&mut ordinary, &mut recorder, 0, None);
    let before = ordinary.controller.inspect();
    assert!(matches!(
        ordinary.controller.request(
            mount,
            geometry(0., Some("unsupported-anchor")),
            permission(),
            0
        ),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(ordinary.controller.inspect(), before);
    let embed = RequestKind::Input(Input {
        owner: InputOwner::Embed("media".into()),
        payload: "embed".into(),
        x: None,
        y: None,
    });
    assert!(matches!(
        ordinary.controller.request(mount, embed, permission(), 0),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(ordinary.controller.inspect(), before);
    ordinary
        .controller
        .request(mount, geometry(0., None), permission(), 0)
        .unwrap();
    recorder.complete(&mut ordinary.controller, mount, 0);
    let bad = RequestKind::Geometry(Geometry {
        rect: core::Rect::new(0., 0., 100., 100.).unwrap(),
        clip: core::Rect::new(0., 0., 100., 100.).unwrap(),
        scale: core::Scalar::new(0.).unwrap(),
        anchor: None,
    });
    let before = f.controller.inspect();
    assert!(f.controller.request(m, bad, permission(), 0).is_err());
    assert_eq!(f.controller.inspect(), before);
    f.controller
        .request(
            m,
            RequestKind::Input(Input {
                owner: InputOwner::Embed("inline-media".into()),
                payload: "embedded input".into(),
                x: None,
                y: None,
            }),
            permission(),
            0,
        )
        .unwrap();
    host.complete(&mut f.controller, m, 0);
}

#[test]
fn eval_010_independent_signals_invalidation_and_animation_deadline() {
    let mut f = fixture(2, Default::default());
    negotiate(&mut f, shared());
    let mut host = RecordingHost::default();
    let m = attached(&mut f, &mut host, 0, None);
    let signals = Signals {
        selected: true,
        visible: false,
        focused: true,
        minimized: true,
        occlusion: Observation::Unknown,
    };
    f.controller
        .request(
            m,
            RequestKind::Presentation(signals.clone()),
            permission(),
            0,
        )
        .unwrap();
    host.complete(&mut f.controller, m, 0);
    assert_eq!(f.controller.inspect().mounts[&m].signals, signals);
    f.controller
        .request(m, RequestKind::Invalidate, permission(), 0)
        .unwrap();
    f.controller
        .request(m, RequestKind::Invalidate, permission(), 0)
        .unwrap();
    assert_eq!(f.controller.inspect().counts.queue, 1);
    host.complete(&mut f.controller, m, 0);
    f.controller
        .request(
            m,
            RequestKind::Animation {
                deadline: Some(200),
            },
            permission(),
            0,
        )
        .unwrap();
    f.controller
        .request(
            m,
            RequestKind::Animation {
                deadline: Some(100),
            },
            permission(),
            0,
        )
        .unwrap();
    assert_eq!(
        f.controller.inspect().mounts[&m].animation_deadline,
        Some(100)
    );
    host.complete(&mut f.controller, m, 0);
    host.complete(&mut f.controller, m, 0);
    let before = f.controller.inspect();
    assert!(f
        .controller
        .request(
            m,
            RequestKind::Presentation(Default::default()),
            HostPermission {
                allowed: false,
                eligibility_revision: 0
            },
            0
        )
        .is_err());
    assert_eq!(f.controller.inspect(), before);
    assert_ne!(
        f.controller.inspect().mounts[&m].confirmed,
        Observation::Known(LifecycleState::Frozen)
    );
}

#[test]
fn eval_011_shared_cache_reservations_and_exact_release() {
    let mut f = fixture(2, Default::default());
    negotiate(&mut f, shared());
    let mut host = RecordingHost::default();
    let first = attached(&mut f, &mut host, 0, None);
    let instance = f.controller.inspect().mounts[&first].instance;
    let second = attached(&mut f, &mut host, 1, Some(instance));
    let before = f.controller.inspect();
    assert!(f
        .controller
        .reserve_cache(first, 17, None, None, false, permission())
        .is_err());
    assert_eq!(f.controller.inspect(), before);
    let cache = f
        .controller
        .reserve_cache(first, 17, Some(128), None, false, permission())
        .unwrap();
    assert_eq!(
        f.controller
            .reserve_cache(second, 17, Some(128), Some(cache), true, permission())
            .unwrap(),
        cache
    );
    assert_eq!(f.controller.inspect().counts.cache_entries, 1);
    assert_eq!(f.controller.inspect().counts.cache_bytes, 128);
    let before = f.controller.inspect();
    assert!(f
        .controller
        .reserve_cache(second, 18, Some(128), Some(cache), true, permission())
        .is_err());
    assert_eq!(f.controller.inspect(), before);
    f.controller
        .request(first, RequestKind::CacheRelease(cache), permission(), 0)
        .unwrap();
    let r = host.complete(&mut f.controller, first, 0);
    assert_eq!(
        f.controller.inspect().caches[&cache].consumers,
        [second].into_iter().collect()
    );
    assert_eq!(f.controller.inspect().counts.cache_bytes, 128);
    let before = f.controller.inspect();
    let ack = Ack {
        ticket: r.ticket,
        result: AckResult::Confirmed(Confirmation::Done),
    };
    f.controller.ack(ack, 0).unwrap();
    assert_eq!(f.controller.inspect(), before);
    f.controller
        .request(second, RequestKind::CacheRelease(cache), permission(), 0)
        .unwrap();
    host.complete(&mut f.controller, second, 0);
    assert_eq!(f.controller.inspect().counts.cache_entries, 0);
    assert_eq!(f.controller.inspect().counts.cache_bytes, 0);
    // Declared admission reaching zero is not a measurement of host GPU memory.
    f.controller
        .record_sample(Sample {
            instance,
            metric: Metric::GpuBytes,
            unit: Unit::Bytes,
            value: Observation::Unknown,
            provenance: Provenance::Unknown,
        })
        .unwrap();
    assert!(f
        .controller
        .diagnostics()
        .samples
        .iter()
        .any(|s| s.metric == Metric::GpuBytes && s.value == Observation::Unknown));
}

#[test]
fn eval_012_attributable_diagnostics_and_sample_rejection() {
    let mut f = fixture(2, Default::default());
    negotiate(&mut f, shared());
    let mut host = RecordingHost::default();
    let m = attached(&mut f, &mut host, 0, None);
    let instance = f.controller.inspect().mounts[&m].instance;
    f.controller
        .request(m, input("secret-host-payload"), permission(), 0)
        .unwrap();
    let r = host.complete(&mut f.controller, m, 0);
    f.controller
        .record_sample(Sample {
            instance,
            metric: Metric::InputDuration,
            unit: Unit::Milliseconds,
            value: Observation::Known(3),
            provenance: Provenance::Measured,
        })
        .unwrap();
    f.controller
        .record_sample(Sample {
            instance,
            metric: Metric::CpuBytes,
            unit: Unit::Bytes,
            value: Observation::Known(4096),
            provenance: Provenance::Estimated,
        })
        .unwrap();
    f.controller
        .record_sample(Sample {
            instance,
            metric: Metric::GpuBytes,
            unit: Unit::Bytes,
            value: Observation::Unknown,
            provenance: Provenance::Unknown,
        })
        .unwrap();
    let before = f.controller.diagnostics();
    assert!(!format!("{before:?}").contains("secret-host-payload"));
    assert!(f
        .controller
        .record_sample(Sample {
            instance,
            metric: Metric::InputDuration,
            unit: Unit::Bytes,
            value: Observation::Known(3),
            provenance: Provenance::Measured
        })
        .is_err());
    assert_eq!(f.controller.diagnostics(), before);
    assert!(f
        .controller
        .record_sample(Sample {
            instance,
            metric: Metric::GpuBytes,
            unit: Unit::Bytes,
            value: Observation::Unknown,
            provenance: Provenance::Measured
        })
        .is_err());
    assert_eq!(f.controller.diagnostics(), before);
    let state = f.controller.inspect();
    let mut wrong = r.ticket;
    wrong.scope.identity.incarnation = "old-core".into();
    assert!(matches!(
        f.controller.ack(
            Ack {
                ticket: wrong,
                result: AckResult::Confirmed(Confirmation::Done)
            },
            0
        ),
        Err(Error::IdentityMismatch)
    ));
    assert_eq!(f.controller.inspect(), state);
    assert_eq!(before.counts.instances, 1);
    assert_eq!(before.samples.len(), 3);
    for metric in [Metric::LayoutDuration, Metric::RedrawDuration] {
        f.controller
            .record_sample(Sample {
                instance,
                metric,
                unit: Unit::Milliseconds,
                value: Observation::Known(2),
                provenance: Provenance::Measured,
            })
            .unwrap();
    }
    assert_eq!(f.controller.diagnostics().samples.len(), 5);
}
