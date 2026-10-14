use cabinet::*;

fn wide(size: usize) -> Snapshot {
    let mut entries = vec![Entry::new("folder", None, "Folder", EntryKind::Folder)];
    entries.extend((0..size).map(|i| {
        let mut e = Entry::new(
            format!("leaf-{i}"),
            Some("folder".into()),
            format!("Label-{i:06}"),
            EntryKind::Leaf,
        );
        e.order = i as i64;
        e
    }));
    Snapshot {
        context: Context {
            scope: "scaling-tests".into(),
            generation: 1,
            revision: 1,
        },
        hierarchy_complete: true,
        entries,
    }
}
#[test]
fn viewport_controls_remain_bounded_and_far_focus_is_reachable() {
    use cabinet_egui::CabinetView;
    let ctx = egui::Context::default();
    let mut c = Cabinet::new(wide(100_000)).unwrap();
    c.expand("folder", true).unwrap();
    let mut view = CabinetView::new("independent-large-tree");
    let render = |view: &mut CabinetView, c: &mut Cabinet, time| {
        let mut response = None;
        let _ = ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(700.0, 500.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| response = Some(view.show(ui, c)));
            },
        );
        response.unwrap()
    };
    let first = render(&mut view, &mut c, 0.0);
    assert!(
        first.rows.len() < 100,
        "row controls must scale with viewport, not 100001 canonical nodes"
    );
    assert!(
        first.text_layouts < 100,
        "offscreen label layout cannot bypass control virtualization"
    );
    assert!(!first.rows.contains_key("leaf-99999"));
    let rebuilds = c.diagnostics().projection_rebuilds;
    render(&mut view, &mut c, 0.05);
    assert_eq!(
        c.diagnostics().projection_rebuilds,
        rebuilds,
        "unchanged native frames reuse the projection"
    );
    c.reveal("leaf-99999").unwrap();
    render(&mut view, &mut c, 0.1);
    let after = render(&mut view, &mut c, 0.2);
    assert!(after.rows.len() < 100);
    assert!(
        after.rows.contains_key("leaf-99999"),
        "focused row outside original viewport must become mounted"
    );
    assert_eq!(c.focused(), Some("leaf-99999"));
}

#[test]
fn real_workspace_search_input_survives_frames_and_clears_on_account_switch() {
    use cabinet_egui::{CabinetView, ViewResponse};
    fn frame(
        ctx: &egui::Context,
        view: &mut CabinetView,
        c: &mut Cabinet,
        time: f64,
        events: Vec<egui::Event>,
    ) -> ViewResponse {
        let mut response = None;
        let _ = ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(800.0, 650.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| response = Some(view.show(ui, c)));
            },
        );
        response.unwrap()
    }
    fn click(
        ctx: &egui::Context,
        view: &mut CabinetView,
        c: &mut Cabinet,
        time: f64,
        pos: egui::Pos2,
    ) -> ViewResponse {
        frame(
            ctx,
            view,
            c,
            time,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        frame(
            ctx,
            view,
            c,
            time + 0.01,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        )
    }
    let ctx = egui::Context::default();
    let mut view = CabinetView::new("independent-search");
    let mut c = Cabinet::new(wide(10)).unwrap();
    let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.02,
        layout.controls["host-search-mode"].center(),
    );
    let search = frame(&ctx, &mut view, &mut c, 0.05, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.1,
        search.controls["filter"].center(),
    );
    frame(
        &ctx,
        &mut view,
        &mut c,
        0.15,
        vec![egui::Event::Text("remote-pattern".into())],
    );
    let settled = frame(&ctx, &mut view, &mut c, 0.2, vec![]);
    assert_eq!(
        c.filter(),
        "",
        "host query must not become a loaded-only filter"
    );
    let submitted = click(
        &ctx,
        &mut view,
        &mut c,
        0.25,
        settled.controls["search-submit"].center(),
    );
    assert_eq!(submitted.requests.len(), 1);
    assert_eq!(
        submitted.requests[0].operation,
        Operation::WorkspaceSearch {
            query: "remote-pattern".into()
        }
    );
    let mut next = c.snapshot().clone();
    next.context.scope = "other-account".into();
    c.replace(next).unwrap();
    let switched = frame(&ctx, &mut view, &mut c, 0.3, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.35,
        switched.controls["host-search-mode"].center(),
    );
    let cleared = frame(&ctx, &mut view, &mut c, 0.4, vec![]);
    let disabled = click(
        &ctx,
        &mut view,
        &mut c,
        0.45,
        cleared.controls["search-submit"].center(),
    );
    assert!(
        disabled.requests.is_empty(),
        "previous account's query cannot be submitted after switching"
    );
    assert!(c.pending().is_none());
}

mod native_probe {
    use super::*;
    use cabinet_egui::{CabinetView, ViewResponse};
    pub fn frame(
        ctx: &egui::Context,
        view: &mut CabinetView,
        c: &mut Cabinet,
        time: f64,
        events: Vec<egui::Event>,
    ) -> ViewResponse {
        let mut out = None;
        let _ = ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(800.0, 650.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| out = Some(view.show(ui, c)));
            },
        );
        out.unwrap()
    }
    pub fn click(
        ctx: &egui::Context,
        view: &mut CabinetView,
        c: &mut Cabinet,
        time: f64,
        pos: egui::Pos2,
    ) -> ViewResponse {
        frame(
            ctx,
            view,
            c,
            time,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        frame(
            ctx,
            view,
            c,
            time + 0.01,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        )
    }
}

#[test]
fn native_scheduler_reconfiguration_preserves_active_tokens_and_rejects_invalid_limits_atomically()
{
    use native_probe::frame;
    let ctx = egui::Context::default();
    let mut c = Cabinet::new(wide(1)).unwrap();
    c.expand("folder", true).unwrap();
    let mut view = cabinet_egui::CabinetView::new("reconfigure-active-worker");
    view.enable_scheduler(peak::SchedulerSettings {
        max_concurrent: 1,
        ..Default::default()
    })
    .unwrap();
    let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
    let hover = layout.rows["leaf-0"].left_center() + egui::vec2(50.0, 0.0);
    frame(
        &ctx,
        &mut view,
        &mut c,
        0.1,
        vec![egui::Event::PointerMoved(hover)],
    );
    frame(&ctx, &mut view, &mut c, 0.6, vec![]);
    let dispatch = frame(&ctx, &mut view, &mut c, 1.9, vec![]);
    assert_eq!(dispatch.preview_jobs.len(), 1);
    let token = dispatch.preview_jobs[0].token;
    view.enable_scheduler(peak::SchedulerSettings {
        max_concurrent: 2,
        max_cache_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(view
        .enable_scheduler(peak::SchedulerSettings {
            max_concurrent: 0,
            ..Default::default()
        })
        .is_err());
    assert!(
        view.complete_scheduled(
            token,
            peak::Content::Text {
                body: "owned-worker".into(),
                truncated: false
            }
        ),
        "reconfiguration must preserve the issued worker token"
    );
    assert!(!view.complete_scheduled(
        token,
        peak::Content::Unsupported {
            reason: "duplicate".into()
        }
    ));
    frame(
        &ctx,
        &mut view,
        &mut c,
        2.0,
        vec![egui::Event::PointerMoved(egui::pos2(790.0, 640.0))],
    );
    frame(&ctx, &mut view, &mut c, 2.4, vec![]);
    assert_eq!(view.state.peak.stage(), peak::Stage::Hidden);
    frame(
        &ctx,
        &mut view,
        &mut c,
        2.5,
        vec![egui::Event::PointerMoved(hover)],
    );
    let fresh = frame(&ctx, &mut view, &mut c, 4.3, vec![]);
    assert_eq!(
        fresh.preview_jobs.len(),
        1,
        "rejected configuration must not restore the default cache budget"
    );
    assert_ne!(fresh.preview_jobs[0].token, token);
    assert!(fresh.preview_requests.is_empty());
}

#[test]
fn native_preview_worker_completion_replays_through_scoped_scheduler_cache() {
    use native_probe::frame;
    let ctx = egui::Context::default();
    let mut c = Cabinet::new(wide(2)).unwrap();
    c.expand("folder", true).unwrap();
    let mut view = cabinet_egui::CabinetView::new("independent-scheduled-preview");
    view.enable_scheduler(peak::SchedulerSettings {
        max_concurrent: 1,
        ..Default::default()
    })
    .unwrap();
    let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
    let hover = layout.rows["leaf-0"].left_center() + egui::vec2(50.0, 0.0);
    frame(
        &ctx,
        &mut view,
        &mut c,
        0.1,
        vec![egui::Event::PointerMoved(hover)],
    );
    frame(&ctx, &mut view, &mut c, 0.6, vec![]);
    let dispatched = frame(&ctx, &mut view, &mut c, 1.9, vec![]);
    assert!(
        dispatched.preview_requests.is_empty(),
        "scheduler mode must not duplicate legacy worker dispatch"
    );
    assert_eq!(dispatched.preview_jobs.len(), 1);
    let job = &dispatched.preview_jobs[0];
    assert_eq!(job.request.key.entry_id, "leaf-0");
    assert!(view.complete_scheduled(
        job.token,
        peak::Content::Text {
            body: "worker-produced".into(),
            truncated: false
        }
    ));
    assert!(!view.complete_scheduled(
        job.token,
        peak::Content::Text {
            body: "duplicate".into(),
            truncated: false
        }
    ));
    assert_eq!(
        view.state.peak.content(),
        Some(&peak::Content::Text {
            body: "worker-produced".into(),
            truncated: false
        })
    );
    frame(
        &ctx,
        &mut view,
        &mut c,
        2.0,
        vec![egui::Event::PointerMoved(egui::pos2(790.0, 640.0))],
    );
    frame(&ctx, &mut view, &mut c, 2.4, vec![]);
    assert_eq!(view.state.peak.stage(), peak::Stage::Hidden);
    frame(
        &ctx,
        &mut view,
        &mut c,
        2.5,
        vec![egui::Event::PointerMoved(hover)],
    );
    let cached = frame(&ctx, &mut view, &mut c, 4.3, vec![]);
    assert!(cached.preview_jobs.is_empty());
    assert!(cached.preview_requests.is_empty());
    assert_eq!(
        view.state.peak.content(),
        Some(&peak::Content::Text {
            body: "worker-produced".into(),
            truncated: false
        })
    );
    assert!(
        c.pending().is_none(),
        "preview workers cannot occupy the durable operation slot"
    );
    frame(
        &ctx,
        &mut view,
        &mut c,
        4.4,
        vec![egui::Event::PointerMoved(egui::pos2(790.0, 640.0))],
    );
    let mut other = c.snapshot().clone();
    other.context.scope = "other-account".into();
    c.replace(other).unwrap();
    frame(&ctx, &mut view, &mut c, 4.5, vec![]);
    let mut original = c.snapshot().clone();
    original.context.scope = "scaling-tests".into();
    c.replace(original).unwrap();
    c.expand("folder", true).unwrap();
    let returned_layout = frame(&ctx, &mut view, &mut c, 4.6, vec![]);
    let returned_hover = returned_layout.rows["leaf-0"].left_center() + egui::vec2(50.0, 0.0);
    frame(
        &ctx,
        &mut view,
        &mut c,
        4.7,
        vec![egui::Event::PointerMoved(returned_hover)],
    );
    assert_eq!(
        view.state.peak.key().map(|key| key.entry_id.as_str()),
        Some("leaf-0"),
        "privacy probe must hover an actually mounted leaf after scope reset"
    );
    let returned = frame(&ctx, &mut view, &mut c, 6.5, vec![]);
    assert_eq!(
        returned.preview_jobs.len(),
        1,
        "A -> B -> A without hovering B must purge A's stored cache on the first context switch"
    );
    assert!(view.state.peak.content().is_none());
}

#[test]
fn real_paging_menu_emits_fenced_load_page() {
    use native_probe::{click, frame};
    let mut snapshot = wide(0);
    snapshot.hierarchy_complete = false;
    snapshot.entries[0].children = ChildrenState::Unloaded;
    let ctx = egui::Context::default();
    let mut c = Cabinet::new(snapshot).unwrap();
    let mut view = cabinet_egui::CabinetView::new("independent-page-menu");
    let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.02,
        layout.controls["options:folder"].center(),
    );
    let menu = frame(&ctx, &mut view, &mut c, 0.06, vec![]);
    let page = click(
        &ctx,
        &mut view,
        &mut c,
        0.1,
        menu.controls["menu-load-page:folder"].center(),
    );
    assert_eq!(page.requests.len(), 1);
    assert_eq!(
        page.requests[0].operation,
        Operation::LoadPage {
            id: "folder".into(),
            cursor: None
        }
    );
    assert_eq!(page.requests[0].context, c.snapshot().context);
    assert_eq!(
        page.requests[0].expected_revisions.get("folder"),
        Some(&c.entry("folder").unwrap().revision)
    );
}

#[test]
fn real_incomplete_move_menu_requires_host_grant_before_authorized_dispatch() {
    use native_probe::{click, frame};
    let mut snapshot = wide(2);
    snapshot.hierarchy_complete = false;
    snapshot.entries.push(Entry::new(
        "destination",
        None,
        "Destination",
        EntryKind::Folder,
    ));
    let ctx = egui::Context::default();
    let mut c = Cabinet::new(snapshot).unwrap();
    c.expand("folder", true).unwrap();
    let mut view = cabinet_egui::CabinetView::new("independent-preflight-menu");
    let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.02,
        layout.controls["options:leaf-0"].center(),
    );
    let menu = frame(&ctx, &mut view, &mut c, 0.06, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.1,
        menu.controls["menu-move:leaf-0"].center(),
    );
    let destinations = frame(&ctx, &mut view, &mut c, 0.15, vec![]);
    let preflight = click(
        &ctx,
        &mut view,
        &mut c,
        0.2,
        destinations.controls["move-destination:destination"].center(),
    );
    assert_eq!(preflight.requests.len(), 1);
    let request = &preflight.requests[0];
    let placement = Placement::Into(Some("destination".into()));
    assert_eq!(
        request.operation,
        Operation::MovePreflight {
            ids: vec!["leaf-0".into()],
            placement: placement.clone()
        }
    );
    assert_eq!(c.entry("leaf-0").unwrap().parent, Some("folder".into()));
    c.accept_move_preflight(
        request.id,
        MoveGrant {
            context: request.context.clone(),
            ids: vec!["leaf-0".into()],
            placement: placement.clone(),
            token: "trusted-host-proof".into(),
        },
    )
    .unwrap();
    let approved = frame(&ctx, &mut view, &mut c, 0.25, vec![]);
    assert!(
        approved.requests.is_empty(),
        "grant acceptance alone cannot dispatch a durable move"
    );
    let confirm = approved.controls["confirm-authorized-move"];
    assert!(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::Vec2::new(800.0,650.0)).contains_rect(confirm),"host-approved confirmation must be visible and clickable within the viewport; got {confirm:?}");
    let authorized = click(&ctx, &mut view, &mut c, 0.3, confirm.center());
    assert_eq!(authorized.requests.len(), 1);
    assert_eq!(
        authorized.requests[0].operation,
        Operation::MoveAuthorized {
            ids: vec!["leaf-0".into()],
            placement,
            token: "trusted-host-proof".into()
        }
    );
    assert_eq!(
        c.entry("leaf-0").unwrap().parent,
        Some("folder".into()),
        "authorized intention still awaits host canonical result"
    );
}
