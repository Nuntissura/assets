use kumihimo_render_bridge::*;

#[cfg(feature = "desktop")]
#[path = "support/desktop_cases.rs"]
mod desktop_cases;

fn scope(view: u64) -> ContentScope {
    ContentScope {
        identity: ScopeIdentity {
            lineage: "layout".into(),
            incarnation: "content-generation".into(),
        },
        session: "session".into(),
        provider: 2,
        resource: view + 10,
        target: ScopeTarget::View(view),
        connection: 1,
        instance: view,
        mount: view,
    }
}
fn binding(view: u64) -> FrameBinding {
    FrameBinding {
        scope: scope(view),
        engine_incarnation: 1,
        geometry_revision: 1,
        surface_epoch: 1,
    }
}
fn pixels(request: FrameRequest, byte: u8) -> PixelFrame {
    PixelFrame {
        row_stride: request.width * 4,
        rgba: vec![byte; (request.width * request.height * 4) as usize],
        request,
    }
}
fn accepted_store() -> FrameStore {
    let mut store = FrameStore::new(BridgeLimits::default()).unwrap();
    store.bind(binding(1), 2, 2).unwrap();
    let request = store.request(ScopeTarget::View(1), 1, 100, 1).unwrap();
    store.accept(pixels(request, 11), 2).unwrap();
    store
}

#[test]
fn complete_scope_and_separate_epochs_are_admission_boundaries() {
    let mut store = accepted_store();
    let target = ScopeTarget::View(1);
    let request = store.request(target, 2, 100, 3).unwrap();
    let mut alterations = Vec::new();
    macro_rules! changed {
        ($field:ident, $value:expr) => {{
            let mut r = request.clone();
            r.binding.scope.$field = $value;
            alterations.push(r);
        }};
    }
    changed!(
        identity,
        ScopeIdentity {
            lineage: "other".into(),
            incarnation: "content-generation".into()
        }
    );
    changed!(
        identity,
        ScopeIdentity {
            lineage: "layout".into(),
            incarnation: "other".into()
        }
    );
    changed!(session, "other".into());
    changed!(provider, 3);
    changed!(resource, 99);
    changed!(target, ScopeTarget::View(99));
    changed!(target, ScopeTarget::FrameBackground(1));
    changed!(connection, 2);
    changed!(instance, 2);
    changed!(mount, 2);
    for field in 0..3 {
        let mut r = request.clone();
        match field {
            0 => r.binding.engine_incarnation += 1,
            1 => r.binding.geometry_revision += 1,
            _ => r.binding.surface_epoch += 1,
        }
        alterations.push(r);
    }
    for r in alterations {
        assert!(store.accept(pixels(r, 99), 4).is_err());
        assert_eq!(store.last_good(target).unwrap().rgba, vec![11; 16]);
        assert_eq!(store.pending(target), Some(&request));
    }
    store.accept(pixels(request.clone(), 22), 4).unwrap();
    assert_eq!(
        store.accept(pixels(request, 33), 5),
        Err(BridgeError::Duplicate)
    );
    assert_eq!(store.current_frame(target).unwrap().rgba, vec![22; 16]);
}

#[test]
fn request_dimension_deadline_and_pixel_failures_preserve_last_good() {
    let mut store = accepted_store();
    let target = ScopeTarget::View(1);
    let request = store.request(target, 2, 10, 3).unwrap();
    assert_eq!(store.request(target, 3, 20, 3), Err(BridgeError::Busy));
    for field in 0..5 {
        let mut r = request.clone();
        match field {
            0 => r.request_id += 1,
            1 => r.sequence += 1,
            2 => r.width += 1,
            3 => r.height += 1,
            _ => r.deadline_ms += 1,
        }
        assert!(store.accept(pixels(r, 90), 4).is_err());
    }
    let mut bad_stride = pixels(request.clone(), 90);
    bad_stride.row_stride += 1;
    assert_eq!(
        store.accept(bad_stride, 4),
        Err(BridgeError::Invalid("RGBA row stride"))
    );
    let mut bad_length = pixels(request.clone(), 90);
    bad_length.rgba.pop();
    assert_eq!(
        store.accept(bad_length, 4),
        Err(BridgeError::Invalid("RGBA exact length"))
    );
    assert_eq!(
        store.accept(pixels(request, 90), 10),
        Err(BridgeError::Expired)
    );
    assert_eq!(store.last_good(target).unwrap().rgba, vec![11; 16]);
    store.cancel(target, 2).unwrap();
    assert_eq!(
        store.request(target, 2, 30, 10),
        Err(BridgeError::Duplicate)
    );
    assert_eq!(store.request(target, 3, 10, 10), Err(BridgeError::Expired));
}

#[test]
fn resize_reload_and_renderer_recovery_keep_capture_provenance() {
    let mut store = accepted_store();
    let target = ScopeTarget::View(1);
    let pending = store.request(target, 2, 100, 3).unwrap();
    let mut replacement = binding(1);
    replacement.surface_epoch = 2;
    store.bind(replacement.clone(), 2, 2).unwrap();
    assert!(store.current_frame(target).is_err());
    assert!(store.accept(pixels(pending, 99), 4).is_err());
    let recovery = store.recovery_frame(target).unwrap();
    assert_eq!(recovery.current_binding.surface_epoch, 2);
    assert_eq!(recovery.captured.request.binding.surface_epoch, 1);
    assert_eq!(recovery.captured.rgba, vec![11; 16]);
    assert!(store.bind(binding(1), 2, 2).is_err());
    assert!(store.bind(replacement.clone(), 3, 2).is_err());
    replacement.geometry_revision = 2;
    store.bind(replacement.clone(), 3, 2).unwrap();
    assert!(store.recovery_frame(target).is_err());
    let request = store.request(target, 3, 100, 5).unwrap();
    store.accept(pixels(request, 44), 6).unwrap();
    replacement.engine_incarnation = 2;
    store.bind(replacement, 3, 2).unwrap();
    assert!(store.recovery_frame(target).is_err());
    assert_eq!(store.last_good(target).unwrap().rgba, vec![44; 24]);
}

#[test]
fn allocation_and_admission_budgets_are_checked_before_storage() {
    let limits = BridgeLimits::default();
    assert!(limits.check_message(limits.max_message_bytes + 1).is_err());
    assert!(limits.check_png(limits.max_png_bytes + 1).is_err());
    assert!(limits.check_png(0).is_err());
    assert_eq!(
        limits.rgba_layout(2048, 2048).unwrap(),
        (8192, 16 * 1024 * 1024)
    );
    assert!(limits.rgba_layout(2049, 1).is_err());
    assert!(limits.rgba_layout(0, 1).is_err());
    let permissive_side = BridgeLimits {
        max_side: u32::MAX,
        max_rgba_bytes: usize::MAX,
        max_host_frames: 1,
        ..limits.clone()
    };
    assert!(permissive_side.rgba_layout(u32::MAX, 2).is_err());
    let low_raw = BridgeLimits {
        max_rgba_bytes: 15,
        ..limits.clone()
    };
    assert!(low_raw.rgba_layout(2, 2).is_err());
    let mut store = FrameStore::new(limits.clone()).unwrap();
    for view in 1..=limits.max_host_frames as u64 {
        store.bind(binding(view), 1, 1).unwrap();
    }
    assert_eq!(
        store.bind(binding(20), 1, 1),
        Err(BridgeError::Budget("host frames"))
    );
    store.remove(ScopeTarget::View(1));
    store.bind(binding(20), 1, 1).unwrap();
    let increased = BridgeLimits {
        max_host_frames: 7,
        max_live_engines: 5,
        ..limits
    };
    let mut larger = FrameStore::new(increased).unwrap();
    for view in 1..=7 {
        larger.bind(binding(view), 1, 1).unwrap();
    }
}

#[test]
fn scheduling_coalesces_and_never_recaptures_an_unchanged_sibling() {
    let mut scheduler = DirtyScheduler::new(&BridgeLimits::default()).unwrap();
    let a = ScopeTarget::View(1);
    let b = ScopeTarget::View(2);
    for target in [a, b] {
        scheduler.register(target, true, true).unwrap();
        assert!(scheduler.begin_capture(target, 1).unwrap());
        assert_eq!(scheduler.begin_capture(target, 2), Err(BridgeError::Busy));
        scheduler.finish_capture(target, 1, true).unwrap();
        assert!(scheduler.record_upload(target, 1, 1).unwrap());
        assert!(!scheduler.record_upload(target, 1, 1).unwrap());
        assert_eq!(scheduler.state(target).unwrap(), WorkState::Static);
        assert!(!scheduler.begin_capture(target, 2).unwrap());
    }
    for _ in 0..8 {
        scheduler.mark_dirty(a).unwrap();
    }
    assert!(scheduler.begin_capture(a, 2).unwrap());
    scheduler.mark_dirty(a).unwrap();
    scheduler.finish_capture(a, 2, true).unwrap();
    assert_eq!(scheduler.state(a).unwrap(), WorkState::Dirty);
    assert!(scheduler.begin_capture(a, 3).unwrap());
    scheduler.finish_capture(a, 3, true).unwrap();
    assert!(!scheduler.begin_capture(a, 4).unwrap());
    assert!(!scheduler.begin_capture(b, 2).unwrap());
    assert_eq!(scheduler.counts(a).unwrap().capture_requests, 3);
    assert_eq!(scheduler.counts(b).unwrap().capture_requests, 1);
    assert!(scheduler.record_upload(b, 1, 2).unwrap());
    assert!(!scheduler.record_upload(b, 1, 2).unwrap());
    assert!(scheduler.record_upload(b, 1, 1).is_err());
    assert_eq!(scheduler.counts(b).unwrap().texture_uploads, 2);
}

#[test]
fn hidden_unloaded_and_failed_capture_are_distinct() {
    let mut scheduler = DirtyScheduler::new(&BridgeLimits::default()).unwrap();
    let target = ScopeTarget::View(1);
    scheduler.register(target, false, true).unwrap();
    assert_eq!(scheduler.state(target).unwrap(), WorkState::Hidden);
    assert!(!scheduler.begin_capture(target, 1).unwrap());
    scheduler.set_visible(target, true).unwrap();
    assert!(scheduler.begin_capture(target, 1).unwrap());
    scheduler.finish_capture(target, 1, false).unwrap();
    assert_eq!(scheduler.state(target).unwrap(), WorkState::Dirty);
    assert!(scheduler.begin_capture(target, 2).unwrap());
    scheduler.set_loaded(target, false).unwrap();
    assert_eq!(scheduler.state(target).unwrap(), WorkState::Unloaded);
    assert!(scheduler.finish_capture(target, 2, true).is_err());
    assert!(!scheduler.begin_capture(target, 3).unwrap());
    scheduler.set_loaded(target, true).unwrap();
    assert_eq!(scheduler.state(target).unwrap(), WorkState::Dirty);
    assert!(scheduler.begin_capture(target, 3).unwrap());
}

fn lifecycle() -> LifecycleManager {
    let mut manager = LifecycleManager::new(&BridgeLimits::default()).unwrap();
    manager
        .register(
            scope(1),
            1,
            ProcessIdentity {
                pid: 10,
                starttime: 50,
            },
            "file:///page.html".into(),
            true,
        )
        .unwrap();
    manager
}
fn receipt(manager: &LifecycleManager, absent: bool) -> IndependentProcessReceipt {
    let v = manager.record(ScopeTarget::View(1)).unwrap();
    IndependentProcessReceipt {
        scope: v.scope.clone(),
        engine_incarnation: v.engine_incarnation,
        process: v.process,
        operation_id: v.operation_id,
        observed_ms: v.requested_ms + 1,
        inspection_id: "independent-kernel-read".into(),
        process_absent: absent,
        descendants_absent: absent,
    }
}

#[test]
fn protected_and_unsupported_lifecycle_requests_preserve_live_state() {
    let mut manager = lifecycle();
    let target = ScopeTarget::View(1);
    for protection in [(true, false), (false, true)] {
        manager.protect(target, protection.0, protection.1).unwrap();
        assert_eq!(
            manager.request_unload(target, 1, 2),
            Err(BridgeError::Protected)
        );
        assert_eq!(manager.record(target).unwrap().state, EngineState::Visible);
    }
    manager.set_visible(target, false).unwrap();
    let before = manager.record(target).unwrap().clone();
    assert!(matches!(
        manager.request_freeze(target),
        Err(BridgeError::Unsupported(_))
    ));
    assert!(matches!(
        manager.request_throttle(target),
        Err(BridgeError::Unsupported(_))
    ));
    assert!(matches!(
        manager.request_context_popout(target),
        Err(BridgeError::Unsupported(_))
    ));
    assert_eq!(manager.record(target).unwrap(), &before);
    assert_eq!(before.state, EngineState::Hidden);
}

#[test]
fn unknown_unload_requires_current_independent_absence_then_fresh_reload() {
    let mut manager = lifecycle();
    let target = ScopeTarget::View(1);
    manager.request_unload(target, 1, 10).unwrap();
    manager.outcome_unknown(target, 1).unwrap();
    assert_eq!(
        manager.request_unload(target, 2, 20),
        Err(BridgeError::ReconciliationRequired)
    );
    let good = receipt(&manager, true);
    for field in 0..6 {
        let mut r = good.clone();
        match field {
            0 => r.process.starttime += 1,
            1 => r.scope.mount += 1,
            2 => r.engine_incarnation += 1,
            3 => r.operation_id += 1,
            4 => r.observed_ms = 9,
            _ => r.descendants_absent = false,
        }
        assert!(manager.confirm_unloaded(&r).is_err());
        assert_eq!(manager.record(target).unwrap().state, EngineState::Unknown);
    }
    manager.confirm_unloaded(&good).unwrap();
    assert_eq!(manager.record(target).unwrap().scope, scope(1));
    assert_eq!(
        manager.record(target).unwrap().page_reference,
        "file:///page.html"
    );
    assert!(manager
        .begin_restore(
            target,
            2,
            1,
            ProcessIdentity {
                pid: 11,
                starttime: 60
            },
            20,
            "fresh namespace admission"
        )
        .is_err());
    assert!(manager
        .begin_restore(target, 2, 2, good.process, 20, "fresh namespace admission")
        .is_err());
    assert_eq!(
        manager
            .begin_restore(
                target,
                2,
                2,
                ProcessIdentity {
                    pid: 11,
                    starttime: 60
                },
                20,
                "fresh namespace admission"
            )
            .unwrap(),
        RestorationStrength::PageReload
    );
    manager.complete_restore(target, 2).unwrap();
    assert_eq!(manager.record(target).unwrap().state, EngineState::Visible);
    assert_eq!(manager.record(target).unwrap().engine_incarnation, 2);
    assert_eq!(manager.record(target).unwrap().scope, scope(1));
}

#[test]
fn reconciliation_and_engine_budget_precede_retry() {
    let mut manager = lifecycle();
    let target = ScopeTarget::View(1);
    assert!(manager
        .register(
            scope(2),
            1,
            ProcessIdentity {
                pid: 10,
                starttime: 50
            },
            "page-b".into(),
            true
        )
        .is_err());
    manager
        .register(
            scope(2),
            1,
            ProcessIdentity {
                pid: 20,
                starttime: 50,
            },
            "page-b".into(),
            true,
        )
        .unwrap();
    assert!(manager
        .register(
            scope(3),
            1,
            ProcessIdentity {
                pid: 30,
                starttime: 50
            },
            "page-c".into(),
            true
        )
        .is_err());
    manager.request_unload(target, 1, 10).unwrap();
    manager.outcome_unknown(target, 1).unwrap();
    manager.reconcile_live(&receipt(&manager, false)).unwrap();
    assert_eq!(manager.record(target).unwrap().state, EngineState::Visible);
    manager.request_unload(target, 2, 20).unwrap();
    manager.confirm_unloaded(&receipt(&manager, true)).unwrap();
    for attempt in 0..3 {
        let operation = 3 + attempt;
        manager
            .begin_restore(
                target,
                operation,
                2 + attempt,
                ProcessIdentity {
                    pid: 30 + attempt as u32,
                    starttime: 70 + attempt,
                },
                30 + attempt,
                &format!("corrected-cause-{attempt}"),
            )
            .unwrap();
        manager.outcome_unknown(target, operation).unwrap();
        assert!(manager
            .begin_restore(
                target,
                operation + 1,
                10,
                ProcessIdentity {
                    pid: 40,
                    starttime: 90
                },
                50,
                "other"
            )
            .is_err());
        manager.confirm_unloaded(&receipt(&manager, true)).unwrap();
    }
    assert_eq!(
        manager.begin_restore(
            target,
            9,
            10,
            ProcessIdentity {
                pid: 40,
                starttime: 90
            },
            50,
            "new-cause"
        ),
        Err(BridgeError::Budget("restore attempts"))
    );
    assert_eq!(
        manager.record(ScopeTarget::View(2)).unwrap().state,
        EngineState::Visible
    );
}

#[test]
fn local_input_applies_injected_scale_once_and_rejects_edges() {
    assert_eq!(
        local_dip_to_engine_css([10.5, 20.0], 1.5, [100, 100]).unwrap(),
        [16, 30]
    );
    for (point, scale) in [
        ([0.0, 0.0], 0.0),
        ([0.0, 0.0], f64::NAN),
        ([-1.0, 0.0], 1.0),
        ([f64::INFINITY, 0.0], 1.0),
        ([100.0, 0.0], 1.0),
        ([99.9, 0.0], 1.0),
    ] {
        assert!(local_dip_to_engine_css(point, scale, [100, 100]).is_err());
    }
}

#[cfg(feature = "desktop")]
#[test]
fn unknown_wire_fields_reject_at_every_nested_identity_boundary() {
    let mut store = accepted_store();
    let request = store.request(ScopeTarget::View(1), 2, 100, 3).unwrap();
    let original = serde_json::to_value(&request).unwrap();
    for path in [
        vec![],
        vec!["binding"],
        vec!["binding", "scope"],
        vec!["binding", "scope", "identity"],
        vec!["binding", "scope", "target"],
    ] {
        let mut value = original.clone();
        let mut object = &mut value;
        for key in path {
            object = object.get_mut(key).unwrap();
        }
        object
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), serde_json::json!(1));
        assert!(serde_json::from_value::<FrameRequest>(value).is_err());
    }
}
