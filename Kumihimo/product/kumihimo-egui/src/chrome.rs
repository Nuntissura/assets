//! Derived controls; only the host dispatcher can change canonical state.
use crate::*;
use kumihimo_core::{DockIntent, DockPreview, GroupId, TabQuery, TabRow};
use std::collections::{BTreeMap, BTreeSet};

/// Caller-selected colors for shared docking, resizing and navigation paint.
/// Stroke widths and interaction geometry remain adapter-owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeTokens {
    pub preview_stroke: egui::Color32,
    pub resize_handle_stroke: egui::Color32,
    pub divider_stroke: egui::Color32,
    pub rail_fill: egui::Color32,
    pub rail_thumb_fill: egui::Color32,
}
impl ThemeTokens {
    pub fn from_visuals(visuals: &egui::Visuals) -> Self {
        Self {
            preview_stroke: visuals.selection.stroke.color,
            resize_handle_stroke: visuals.widgets.inactive.fg_stroke.color,
            divider_stroke: visuals.widgets.noninteractive.bg_stroke.color,
            rail_fill: visuals.extreme_bg_color,
            rail_thumb_fill: visuals.widgets.inactive.bg_fill,
        }
    }
    pub(crate) fn resolve(ctx: &egui::Context, policy: &HostPolicy) -> Self {
        policy
            .theme_tokens
            .unwrap_or_else(|| Self::from_visuals(&ctx.style().visuals))
    }
}

#[derive(Clone, Debug, Default)]
pub struct SettingsPatch {
    pub profile: Option<ChromeProfile>,
    pub header_height: Option<f32>,
    pub hit_size: Option<f32>,
    pub long_press_seconds: Option<f64>,
    pub group_colors: Option<bool>,
    pub frame_shortcuts_first: Option<bool>,
    /// The host must recreate its surface; the adapter never restarts it.
    pub recreation_needed: bool,
}
impl SettingsPatch {
    fn apply(&self, settings: &mut AdapterSettings) {
        macro_rules! apply { ($($field:ident),*) => { $(if let Some(value) = self.$field { settings.$field = value; })* }; }
        apply!(
            profile,
            header_height,
            hit_size,
            long_press_seconds,
            group_colors,
            frame_shortcuts_first
        );
    }
}
#[derive(Default)]
pub struct ChromeState {
    pub revealed: BTreeSet<FrameId>,
    pub workspace_settings: BTreeMap<WorkspaceId, SettingsPatch>,
    pub surface_settings: BTreeMap<egui::ViewportId, SettingsPatch>,
    pub pages: BTreeMap<String, usize>,
    pub search: String,
    pub preview: Option<DockPreview>,
    preview_epoch: u64,
    latest_announcement: Option<(egui::ViewportId, String)>,
}
impl ChromeState {
    pub fn resolve(
        &self,
        policy: &HostPolicy,
        workspace: WorkspaceId,
        viewport: egui::ViewportId,
    ) -> Result<(AdapterSettings, &'static str, bool)> {
        let mut settings = policy.settings.clone();
        let mut source = policy.settings_source;
        let mut recreate = false;
        if let Some(patch) = self.workspace_settings.get(&workspace) {
            patch.apply(&mut settings);
            source = "workspace";
            recreate |= patch.recreation_needed;
        }
        if let Some(patch) = self.surface_settings.get(&viewport) {
            patch.apply(&mut settings);
            source = "surface";
            recreate |= patch.recreation_needed;
        }
        settings.validate()?;
        Ok((settings, source, recreate))
    }
}

pub(crate) fn record(
    report: &mut FrameReport,
    response: &egui::Response,
    id: String,
    label: String,
    target: Option<ViewId>,
) {
    if report.control_limit > 0 && report.controls.len() >= report.control_limit {
        if !report
            .rejected
            .iter()
            .any(|s| s == "control projection budget")
        {
            report.reject("control projection budget");
        }
        return;
    }
    if let Ok(bounds) = from_egui(response.rect) {
        report.controls.push(ControlRecord {
            id,
            label,
            bounds,
            target,
            enabled: response.enabled(),
        });
    }
}
pub(crate) fn command(
    controller: &mut Controller,
    host: &mut impl DesktopHost,
    command: Command,
    report: &mut FrameReport,
) {
    match host.dispatch(controller, command) {
        Ok(response) => {
            if let Err(error) = response.outcome.result {
                report.reject(error);
            }
        }
        Err(error) => report.reject(error),
    }
}
fn button(ui: &mut egui::Ui, key: &str, label: &str, report: &mut FrameReport) -> bool {
    let response = ui.push_id(key, |ui| ui.button(label)).inner;
    record(report, &response, key.into(), label.into(), None);
    #[cfg(feature = "capture")]
    if key.ends_with("/preview-float-redock") || key.starts_with("destination/") {
        let events = ui.input(|input| {
            input
                .events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. }
                    )
                })
                .take(64)
                .cloned()
                .collect::<Vec<_>>()
        });
        let point = events.iter().rev().find_map(|event| match event {
            egui::Event::PointerMoved(point) | egui::Event::PointerButton { pos: point, .. } => {
                Some(*point)
            }
            _ => None,
        });
        if let Some(point) = point.filter(|point| response.rect.contains(*point)) {
            let hit = ui.ctx().layer_id_at(point);
            let trace = serde_json::json!({"control":key,"pass":ui.ctx().cumulative_pass_nr(),
                "report_epoch":report.epoch,"report_generation":report.generation,
                "actual_response_id":format!("{:?}",response.id),"point":[point.x,point.y],
                "rect":[response.rect.min.x,response.rect.min.y,response.rect.max.x,response.rect.max.y],
                "response_layer":format!("{:?}",response.layer_id),"hit_layer":format!("{hit:?}"),
                "hit_same_layer":hit==Some(response.layer_id),"clicked":response.clicked(),
                "hovered":response.hovered(),"contains_pointer":response.contains_pointer(),
                "down":response.is_pointer_button_down_on(),"events":format!("{events:?}")});
            ui.ctx()
                .data_mut(|data| data.insert_temp(egui::Id::new("kumihimo-control-hit"), trace));
        }
    }
    response.clicked()
}
fn native_button(
    ui: &mut egui::Ui,
    key: &str,
    label: &str,
    enabled: bool,
    report: &mut FrameReport,
) -> bool {
    let response = ui
        .push_id(key, |ui| ui.add_enabled(enabled, egui::Button::new(label)))
        .inner;
    record(report, &response, key.into(), label.into(), None);
    let clicked = enabled && response.clicked();
    if !enabled {
        response.on_disabled_hover_text("Native popout unavailable: host capability disabled");
        let reason = ui.label("Native popout unavailable: host capability disabled");
        record(
            report,
            &reason,
            format!("{key}/unavailable"),
            "Native popout unavailable: host capability disabled".into(),
            None,
        );
    }
    clicked
}
fn page(
    ui: &mut egui::Ui,
    state: &mut ChromeState,
    key: &str,
    count: usize,
    size: usize,
    report: &mut FrameReport,
) -> usize {
    let offset = state.pages.entry(key.to_owned()).or_default();
    *offset = (*offset / size * size).min(count.saturating_sub(1) / size * size);
    ui.horizontal(|ui| {
        let previous = ui.add_enabled(*offset > 0, egui::Button::new("Previous page"));
        record(
            report,
            &previous,
            format!("page/{key}/previous"),
            "Previous page".into(),
            None,
        );
        if previous.clicked() {
            *offset = offset.saturating_sub(size);
        }
        ui.label(format!(
            "{}–{} / {}",
            if count == 0 { 0 } else { *offset + 1 },
            (*offset + size).min(count),
            count
        ));
        let next = ui.add_enabled(*offset + size < count, egui::Button::new("Next page"));
        record(
            report,
            &next,
            format!("page/{key}/next"),
            "Next page".into(),
            None,
        );
        if next.clicked() {
            *offset += size;
        }
    });
    *offset
}
fn page_size(controller: &Controller, policy: &HostPolicy) -> usize {
    policy
        .tab_page_size
        .min(
            controller
                .snapshot()
                .limits
                .max_tab_page_size
                .unwrap_or(usize::MAX),
        )
        .max(1)
}
fn preview(
    controller: &Controller,
    intent: DockIntent,
    surface: SurfaceBinding,
    state: &mut ChromeState,
    report: &mut FrameReport,
) {
    match controller.core().preview(intent) {
        Ok(preview) => {
            state.preview = Some(preview);
            state.preview_epoch = surface.epoch;
        }
        Err(error) => report.reject(error),
    }
}
fn destinations(
    ui: &mut egui::Ui,
    controller: &Controller,
    state: &mut ChromeState,
    key: &str,
    size: usize,
    report: &mut FrameReport,
) -> Option<FrameId> {
    let ids: Vec<_> = controller.snapshot().frames.keys().copied().collect();
    let offset = page(ui, state, key, ids.len(), size, report);
    let mut selected = None;
    for id in ids.into_iter().skip(offset).take(size) {
        if button(
            ui,
            &format!("destination/{key}/{}", id.raw()),
            &format!("Frame {}", id.raw()),
            report,
        ) {
            selected = Some(id);
        }
    }
    selected
}
fn view_menu(
    ui: &mut egui::Ui,
    row: &TabRow,
    controller: &mut Controller,
    host: &mut impl DesktopHost,
    state: &mut ChromeState,
    size: usize,
    report: &mut FrameReport,
) {
    if row.closed {
        if button(
            ui,
            &format!("view/{}/reopen", row.id.raw()),
            "Reopen in original frame",
            report,
        ) {
            command(
                controller,
                host,
                Command::ReopenView {
                    view: row.id,
                    destination: row.frame,
                },
                report,
            );
        }
        ui.menu_button("Reopen in…", |ui| {
            if let Some(destination) =
                destinations(ui, controller, state, "reopen-destinations", size, report)
            {
                command(
                    controller,
                    host,
                    Command::ReopenView {
                        view: row.id,
                        destination,
                    },
                    report,
                );
            }
        });
        return;
    }
    if button(
        ui,
        &format!("view/{}/select", row.id.raw()),
        "Select tab",
        report,
    ) {
        command(
            controller,
            host,
            Command::SelectView { view: row.id },
            report,
        );
    }
    if button(
        ui,
        &format!("view/{}/close", row.id.raw()),
        "Close (host/provider approval)",
        report,
    ) {
        command(
            controller,
            host,
            Command::CloseView {
                view: row.id,
                approved: true,
            },
            report,
        );
    }
    if let Some(view) = controller.snapshot().views.get(&row.id).cloned() {
        if button(
            ui,
            &format!("view/{}/pin", view.id.raw()),
            if view.pinned { "Unpin" } else { "Pin" },
            report,
        ) {
            command(
                controller,
                host,
                Command::SetViewMetadata {
                    view: view.id,
                    label: view.label,
                    pinned: !view.pinned,
                    unsaved: view.unsaved,
                },
                report,
            );
        }
    }
    let frame = controller.snapshot().frames[&row.frame].clone();
    if let Some(index) = frame.views.iter().position(|id| *id == row.id) {
        for (label, to) in [
            ("Move tab first", 0),
            ("Move tab previous", index.saturating_sub(1)),
            (
                "Move tab next",
                (index + 1).min(frame.views.len().saturating_sub(1)),
            ),
            ("Move tab last", frame.views.len().saturating_sub(1)),
        ] {
            if button(
                ui,
                &format!("view/{}/{}", row.id.raw(), label.replace(' ', "-")),
                label,
                report,
            ) {
                command(
                    controller,
                    host,
                    Command::MoveView {
                        view: row.id,
                        destination: row.frame,
                        index: to,
                    },
                    report,
                );
            }
        }
    }
    let move_menu = ui.menu_button("Move tab to…", |ui| {
        if let Some(destination) =
            destinations(ui, controller, state, "view-destinations", size, report)
        {
            let index = controller.snapshot().frames[&destination]
                .views
                .len()
                .saturating_sub(usize::from(destination == row.frame));
            command(
                controller,
                host,
                Command::MoveView {
                    view: row.id,
                    destination,
                    index,
                },
                report,
            );
        }
    });
    record(
        report,
        &move_menu.response,
        format!("view/{}/move-menu", row.id.raw()),
        "Move tab to…".into(),
        Some(row.id),
    );
    ui.menu_button("Assign group…", |ui| {
        if ui.button("Ungrouped").clicked() {
            command(
                controller,
                host,
                Command::AssignGroup {
                    view: row.id,
                    group: None,
                },
                report,
            );
        }
        let groups = controller.snapshot().frames[&row.frame].groups.clone();
        let offset = page(ui, state, "assign-groups", groups.len(), size, report);
        for group in groups.into_iter().skip(offset).take(size) {
            let label = controller.snapshot().groups[&group].label.clone();
            if ui.button(label).clicked() {
                command(
                    controller,
                    host,
                    Command::AssignGroup {
                        view: row.id,
                        group: Some(group),
                    },
                    report,
                );
            }
        }
    });
    if native_button(
        ui,
        &format!("view/{}/native-popout", row.id.raw()),
        "Native popout tab",
        host.capabilities().native,
        report,
    ) {
        if let Err(error) = host.native_action(controller, AdapterAction::PopoutView(row.id)) {
            report.reject(error);
        }
    }
}
fn group_menu(
    ui: &mut egui::Ui,
    group: GroupId,
    controller: &mut Controller,
    host: &mut impl DesktopHost,
    state: &mut ChromeState,
    size: usize,
    report: &mut FrameReport,
) {
    let record = controller.snapshot().groups[&group].clone();
    ui.label(format!("Group {}: {}", group.raw(), record.description));
    if button(
        ui,
        &format!("group/{}/collapse", group.raw()),
        if record.collapsed {
            "Expand group"
        } else {
            "Collapse group"
        },
        report,
    ) {
        command(
            controller,
            host,
            Command::ConfigureGroup {
                group,
                label: record.label.clone(),
                color: record.color.clone(),
                description: record.description.clone(),
                collapsed: !record.collapsed,
            },
            report,
        );
    }
    let frame = controller.snapshot().frames[&record.frame].clone();
    let members = frame
        .views
        .iter()
        .filter(|id| controller.snapshot().views[id].group == Some(group))
        .count();
    let group_position = frame.groups.iter().position(|id| *id == group).unwrap_or(0);
    for (label, group_index, index) in [
        ("Group first", 0, 0),
        ("Group previous", group_position.saturating_sub(1), 0),
        (
            "Group next",
            (group_position + 1).min(frame.groups.len().saturating_sub(1)),
            frame.views.len() - members,
        ),
        (
            "Group last",
            frame.groups.len().saturating_sub(1),
            frame.views.len() - members,
        ),
    ] {
        if button(
            ui,
            &format!("group/{}/{}", group.raw(), label.replace(' ', "-")),
            label,
            report,
        ) {
            command(
                controller,
                host,
                Command::MoveGroup {
                    group,
                    destination: record.frame,
                    index,
                    group_index,
                },
                report,
            );
        }
    }
    let move_menu = ui.menu_button("Move entire canonical group to…", |ui| {
        if let Some(destination) =
            destinations(ui, controller, state, "group-destinations", size, report)
        {
            let target = &controller.snapshot().frames[&destination];
            let index = target.views.len()
                - if destination == record.frame {
                    members
                } else {
                    0
                };
            let group_index = target.groups.len() - usize::from(destination == record.frame);
            #[cfg(feature = "capture")]
            let before_generation = controller.snapshot().generation;
            #[cfg(feature = "capture")]
            let rejection_start = report.rejected.len();
            command(
                controller,
                host,
                Command::MoveGroup {
                    group,
                    destination,
                    index,
                    group_index,
                },
                report,
            );
            #[cfg(feature = "capture")]
            let trace = serde_json::json!({"group":group.raw(),"requested_destination":destination.raw(),
                "before_generation":before_generation,"after_generation":controller.snapshot().generation,
                "actual_group_frame":controller.snapshot().groups[&group].frame.raw(),
                "dispatch_rejections":report.rejected.iter().skip(rejection_start).collect::<Vec<_>>()});
            #[cfg(feature = "capture")]
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("kumihimo-group-move-result"), trace));
        }
    });
    crate::chrome::record(
        report,
        &move_menu.response,
        format!("group/{}/move-menu", group.raw()),
        "Move entire canonical group to…".into(),
        None,
    );
    if native_button(
        ui,
        &format!("group/{}/native-popout", group.raw()),
        "Native popout group",
        host.capabilities().native,
        report,
    ) {
        if let Err(error) = host.native_action(controller, AdapterAction::PopoutGroup(group)) {
            report.reject(error);
        }
    }
}
fn frame_menu(
    ui: &mut egui::Ui,
    f: &workspace::FrameProjection,
    controller: &mut Controller,
    surface: SurfaceBinding,
    host: &mut impl DesktopHost,
    state: &mut ChromeState,
    size: usize,
    report: &mut FrameReport,
) {
    for (name, cmd) in [
        (
            "Equalize workspace",
            Command::Equalize {
                workspace: f.workspace,
            },
        ),
        ("Maximize frame", Command::Maximize { frame: f.frame }),
        (
            "Restore workspace",
            Command::RestoreLayout {
                workspace: f.workspace,
            },
        ),
    ] {
        if button(
            ui,
            &format!("frame/{}/{}", f.frame.raw(), name.replace(' ', "-")),
            name,
            report,
        ) {
            command(controller, host, cmd, report);
        }
    }
    if button(
        ui,
        &format!("frame/{}/preview-float-redock", f.frame.raw()),
        if f.floating {
            "Preview redock"
        } else {
            "Preview in-app float"
        },
        report,
    ) {
        preview(
            controller,
            if f.floating {
                DockIntent::Redock { frame: f.frame }
            } else {
                DockIntent::Float {
                    frame: f.frame,
                    rect: f.bounds,
                }
            },
            surface,
            state,
            report,
        );
    }
    ui.menu_button("Preview eligible dock…", |ui| {
        let locations: Vec<_> = controller
            .snapshot()
            .contexts
            .values()
            .flat_map(|c| c.allowed_docks.iter().copied())
            .collect();
        let offset = page(ui, state, "dock-locations", locations.len(), size, report);
        for location in locations.into_iter().skip(offset).take(size) {
            if ui.button(format!("{location:?}")).clicked() {
                preview(
                    controller,
                    DockIntent::Frame {
                        frame: f.frame,
                        location,
                    },
                    surface,
                    state,
                    report,
                );
            }
        }
    });
    let swap_menu = ui.menu_button("Swap with…", |ui| {
        if let Some(second) = destinations(ui, controller, state, "swap-destinations", size, report)
        {
            #[cfg(feature = "capture")]
            let before_generation = controller.snapshot().generation;
            #[cfg(feature = "capture")]
            let rejection_start = report.rejected.len();
            command(
                controller,
                host,
                Command::SwapFrames {
                    first: f.frame,
                    second,
                },
                report,
            );
            #[cfg(feature = "capture")]
            let trace = serde_json::json!({"first":f.frame.raw(),"second":second.raw(),
                "pass":ui.ctx().cumulative_pass_nr(),"before_generation":before_generation,
                "after_generation":controller.snapshot().generation,
                "dispatch_rejections":report.rejected.iter().skip(rejection_start).collect::<Vec<_>>()});
            #[cfg(feature = "capture")]
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("kumihimo-swap-result"), trace));
        }
    });
    record(
        report,
        &swap_menu.response,
        format!("frame/{}/swap-menu", f.frame.raw()),
        "Swap with…".into(),
        None,
    );
    if native_button(
        ui,
        &format!("frame/{}/native-popout", f.frame.raw()),
        "Native popout frame",
        host.capabilities().native,
        report,
    ) {
        if let Err(error) = host.native_action(controller, AdapterAction::PopoutFrame(f.frame)) {
            report.reject(error);
        }
    }
    if ui.button("Hide controls (F6 reveals)").clicked() {
        state.revealed.remove(&f.frame);
        state
            .workspace_settings
            .entry(f.workspace)
            .or_default()
            .profile = Some(ChromeProfile::Hidden);
        ui.ctx().request_repaint();
    }
    ui.menu_button("Workspace controls profile", |ui| {
        for (label, profile) in [
            ("Full", ChromeProfile::Full),
            ("Compact", ChromeProfile::Compact),
            ("Contextual", ChromeProfile::Contextual),
            ("Hidden", ChromeProfile::Hidden),
        ] {
            if ui.button(label).clicked() {
                state
                    .workspace_settings
                    .entry(f.workspace)
                    .or_default()
                    .profile = Some(profile);
                ui.ctx().request_repaint();
            }
        }
    });
}
pub fn draw(
    ctx: &egui::Context,
    controller: &mut Controller,
    projection: &workspace::Projection,
    surface: SurfaceBinding,
    host: &mut impl DesktopHost,
    state: &mut ChromeState,
    report: &mut FrameReport,
) {
    let policy = host.policy();
    let size = page_size(controller, &policy);
    while state.pages.len() > policy.max_pending {
        if let Some(key) = state.pages.keys().next().cloned() {
            state.pages.remove(&key);
        }
    }
    state
        .revealed
        .retain(|id| controller.snapshot().frames.contains_key(id));
    if state.preview.as_ref().is_some_and(|p| {
        p.generation() != controller.snapshot().generation || state.preview_epoch != surface.epoch
    }) {
        state.preview.take();
        report.reject(AdapterError::Stale("chrome preview generation/surface"));
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F6)) {
        state
            .revealed
            .extend(projection.frames.iter().map(|f| f.frame));
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape) || !i.focused) {
        state.preview.take();
    }
    egui::Area::new(egui::Id::new((surface.viewport, "workspace-controls")))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(
            (surface.bounds.right() as f32 - 170.0).max(surface.bounds.x() as f32),
            surface.bounds.y() as f32,
        ))
        .constrain_to(to_egui(surface.bounds))
        .show(ctx, |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(to_egui(surface.bounds)));
            let mut dismissed = false;
            let panes_menu = ui.menu_button("Panes / reveal (F6)", |ui| {
                if button(
                    ui,
                    "workspace/panes-menu/dismiss",
                    "Dismiss pane list",
                    report,
                ) {
                    dismissed = true;
                    ui.close();
                }
                ui.label("Canonical pane list — includes hidden and undersized panes");
                let ids = surface_frames(controller, surface.root);
                let offset = page(ui, state, "pane-list", ids.len(), size, report);
                for frame in ids.into_iter().skip(offset).take(size) {
                    ui.horizontal(|ui| {
                        if ui.button(format!("Reveal frame {}", frame.raw())).clicked() {
                            state.revealed.insert(frame);
                            // Restore every enclosing maximized workspace, not just the leaf.
                            let mut workspace =
                                Some(controller.snapshot().frames[&frame].workspace);
                            while let Some(id) = workspace {
                                let owner = controller.snapshot().workspaces[&id].owner;
                                if controller.snapshot().maximized.contains_key(&id) {
                                    command(
                                        controller,
                                        host,
                                        Command::RestoreLayout { workspace: id },
                                        report,
                                    );
                                }
                                workspace =
                                    owner.map(|f| controller.snapshot().frames[&f].workspace);
                            }
                        }
                        ui.menu_button("Tabs", |ui| {
                            let workspace = controller.snapshot().frames[&frame].workspace;
                            let colors = state
                                .resolve(&policy, workspace, surface.viewport)
                                .map(|(settings, _, _)| settings.group_colors)
                                .unwrap_or(false);
                            tab_list(ui, frame, controller, host, state, size, colors, report);
                        });
                    });
                }
            });
            record(
                report,
                &panes_menu.response,
                "workspace/panes-menu".into(),
                "Panes / reveal (F6)".into(),
                None,
            );
            if dismissed {
                panes_menu.response.request_focus();
                state.latest_announcement = Some((
                    surface.viewport,
                    "Pane list dismissed; logical focus returned to Panes / reveal (F6)".into(),
                ));
                ctx.request_repaint();
            }
            if let Some((viewport, announcement)) = &state.latest_announcement {
                if *viewport == surface.viewport {
                    let response = ui
                        .push_id("pane-menu-announcement", |ui| {
                            let frame = egui::Frame::popup(ui.style())
                                .fill(ui.visuals().window_fill().to_opaque());
                            let width = (panes_menu
                                .response
                                .rect
                                .width()
                                .max(120.0)
                                .min(surface.bounds.width() as f32)
                                - frame.total_margin().sum().x)
                                .max(0.0);
                            frame
                                .show(ui, |ui| {
                                    ui.set_max_width(width);
                                    ui.add(egui::Label::new(announcement).wrap())
                                })
                                .inner
                        })
                        .inner;
                    ctx.accesskit_node_builder(response.id, |node| {
                        node.set_live(egui::accesskit::Live::Polite)
                    });
                    record(
                        report,
                        &response,
                        "workspace/panes-menu/announcement".into(),
                        announcement.clone(),
                        None,
                    );
                }
            }
            if !projection.rejected.is_empty() {
                let label = "Layout unavailable";
                let response = ui
                    .push_id("layout-unavailable", |ui| {
                        ui.label(egui::RichText::new(label).color(egui::Color32::LIGHT_RED))
                    })
                    .inner;
                record(
                    report,
                    &response,
                    "workspace/layout-unavailable".into(),
                    label.into(),
                    None,
                );
                ui.menu_button("Details / recovery", |ui| {
                    ui.set_max_width(240.0);
                    ui.label(
                        "Current geometry rejected. Use Panes / reveal or resize the surface.",
                    );
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .show(ui, |ui| {
                            for rejection in projection.rejected.iter().take(64) {
                                let detail: String = rejection.chars().take(512).collect();
                                ui.label(detail);
                            }
                            if projection.rejected.len() > 64 {
                                ui.label("Additional rejection details omitted.");
                            }
                        });
                });
            }
        });
    if let Some(preview) = state.preview.clone() {
        let bounds = to_egui(surface.bounds);
        let frame = egui::Frame::popup(&ctx.style());
        let margin = frame.total_margin().sum();
        let width = (bounds.width().min(240.0) - margin.x).max(0.0);
        egui::Area::new(egui::Id::new((surface.viewport, "preview-controls")))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(bounds.max)
            .constrain_to(bounds)
            .default_width(width)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(width);
                    egui::ScrollArea::vertical()
                        .max_height((bounds.height() - margin.y).max(0.0))
                        .show(ui, |ui| {
                            ui.add(egui::Label::new("Nonmutating dock preview").wrap());
                            for (key, label, commit) in [
                                ("workspace/preview/commit", "Commit preview", true),
                                ("workspace/preview/cancel", "Cancel preview (Escape)", false),
                            ] {
                                let response = ui
                                    .push_id(key, |ui| {
                                        ui.add_sized(
                                            egui::vec2(width, 18.0),
                                            egui::Button::new(label).wrap(),
                                        )
                                    })
                                    .inner;
                                record(report, &response, key.into(), label.into(), None);
                                if response.clicked() {
                                    state.preview.take();
                                    if commit {
                                        command(
                                            controller,
                                            host,
                                            Command::CommitPreview(preview.clone()),
                                            report,
                                        );
                                    }
                                }
                            }
                        });
                });
            });
    }
    for frame in &projection.frames {
        let settings = &frame.settings;
        let source = frame.settings_source;
        let recreate = frame.recreation_needed;
        let revealed = state.revealed.contains(&frame.frame);
        let contextual = settings.profile == ChromeProfile::Contextual
            && !revealed
            && !ctx.input(|i| {
                i.pointer
                    .hover_pos()
                    .is_some_and(|p| to_egui(frame.bounds).contains(p))
            });
        let hidden_unreserved =
            settings.profile == ChromeProfile::Hidden && frame.usable.y() == frame.content.y();
        if hidden_unreserved && revealed && frame.content.height() > 0.0 {
            // Reveal changes projection next pass; never paint over the old provider slot.
            ctx.request_repaint();
        }
        if hidden_unreserved || contextual {
            let rect =
                egui::Rect::from_min_size(to_egui(frame.content).min, egui::vec2(26.0, 24.0))
                    .intersect(to_egui(frame.content))
                    .intersect(to_egui(frame.clip));
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::Id::new((surface.viewport, frame.frame.raw(), "reveal")),
                egui::UiBuilder::new()
                    .layer_id(frame.layer())
                    .max_rect(rect),
            );
            ui.set_clip_rect(rect);
            let response = ui
                .button("⋯")
                .on_hover_text(format!("Reveal frame {} controls (F6)", frame.frame.raw()));
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!("Reveal frame {} controls", frame.frame.raw()),
                )
            });
            record(
                report,
                &response,
                format!("frame/{}/reveal", frame.frame.raw()),
                format!("Reveal frame {} controls", frame.frame.raw()),
                None,
            );
            #[cfg(feature = "capture")]
            let pointer_events = ctx.input(|input| {
                input
                    .events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            egui::Event::Touch { .. }
                                | egui::Event::PointerMoved(_)
                                | egui::Event::PointerButton { .. }
                        )
                    })
                    .take(64)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            #[cfg(feature = "capture")]
            if let Some(point) = pointer_events
                .iter()
                .rev()
                .find_map(|event| match event {
                    egui::Event::Touch { pos, .. } | egui::Event::PointerButton { pos, .. } => {
                        Some(*pos)
                    }
                    egui::Event::PointerMoved(pos) => Some(*pos),
                    _ => None,
                })
                .filter(|point| response.rect.contains(*point))
            {
                let hit_layers = ctx.memory(|memory| {
                    memory
                        .areas()
                        .visible_layer_ids()
                        .into_iter()
                        .filter(|layer| {
                            memory
                                .area_rect(layer.id)
                                .is_some_and(|rect| rect.contains(point))
                        })
                        .take(64)
                        .map(|layer| format!("{layer:?}"))
                        .collect::<Vec<_>>()
                });
                let trace = serde_json::json!({
                    "viewport":format!("{:?}",surface.viewport),"frame":frame.frame.raw(),"epoch":surface.epoch,
                    "generation":controller.snapshot().generation,"point":[point.x,point.y],
                    "events":format!("{pointer_events:?}"),"actual_response_id":format!("{:?}",response.id),
                    "rect":[response.rect.min.x,response.rect.min.y,response.rect.max.x,response.rect.max.y],
                    "response_layer":format!("{:?}",response.layer_id),"hit_layer":format!("{:?}",ctx.layer_id_at(point)),
                    "hit_areas":hit_layers,"clicked":response.clicked(),"hovered":response.hovered(),
                    "contains_pointer":response.contains_pointer(),"down":response.is_pointer_button_down_on(),
                    "tools_at_point":projection.tools.iter().filter(|tool| to_egui(tool.bounds).contains(point))
                        .map(|tool|serde_json::json!({"tool":tool.tool.raw(),"bounds":format!("{:?}",tool.bounds),
                            "clip":format!("{:?}",tool.clip),"visible":tool.presentation.visible})).collect::<Vec<_>>()
                });
                // One transient slot retains the actual response and hit evidence, never an expected ID.
                ctx.data_mut(|data| data.insert_temp(egui::Id::new("kumihimo-reveal-hit"), trace));
            }
            if response.clicked() {
                state.revealed.insert(frame.frame);
                ctx.request_repaint();
            }
            continue;
        }
        let bounds = to_egui(frame.content);
        let height = settings.header_height.min(bounds.height());
        let header = egui::Rect::from_min_size(bounds.min, egui::vec2(bounds.width(), height));
        if header.intersect(to_egui(frame.clip)).is_negative() {
            continue;
        }
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new((surface.viewport, frame.frame.raw(), "chrome")),
            egui::UiBuilder::new()
                .layer_id(frame.layer())
                .max_rect(header),
        );
        ui.set_clip_rect(header.intersect(to_egui(frame.clip)));
        ui.set_width(header.width());
        if frame.z_root.is_some() {
            ui.painter()
                .rect_filled(header, 0.0, ui.visuals().panel_fill.to_opaque());
        }
        ui.horizontal(|ui| {
            let drag = ui
                .add(
                    egui::Label::new(format!("⠿ {}", frame.frame.raw()))
                        .sense(egui::Sense::click_and_drag()),
                )
                .on_hover_text("Drag frame; touch holds first; Escape cancels");
            drag.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!("Move frame {}", frame.frame.raw()),
                )
            });
            record(
                report,
                &drag,
                format!("frame/{}/drag", frame.frame.raw()),
                format!("Move frame {}", frame.frame.raw()),
                None,
            );
            let frame_menu_response = ui.menu_button(
                if settings.profile == ChromeProfile::Compact {
                    "⋮"
                } else {
                    "Frame"
                },
                |ui| {
                    frame_menu(ui, frame, controller, surface, host, state, size, report);
                },
            );
            record(
                report,
                &frame_menu_response.response,
                format!("frame/{}/menu", frame.frame.raw()),
                "Frame menu".into(),
                None,
            );
            let (tabs_menu_response, _) = egui::containers::menu::MenuButton::new(
                if settings.profile == ChromeProfile::Compact {
                    "Tabs"
                } else {
                    "Tabs / groups"
                },
            )
            .config(
                egui::containers::menu::MenuConfig::new()
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
            )
            .ui(ui, |ui| {
                tab_list(
                    ui,
                    frame.frame,
                    controller,
                    host,
                    state,
                    size,
                    settings.group_colors,
                    report,
                );
            });
            record(
                report,
                &tabs_menu_response,
                format!("frame/{}/tabs-menu", frame.frame.raw()),
                "Tabs and groups menu".into(),
                None,
            );
            if recreate {
                ui.label("Surface recreation required");
            }
            if settings.profile == ChromeProfile::Full {
                ui.label(egui::RichText::new(source).small());
            }
            let remaining = (ui.available_width() / 95.0).floor().max(1.0) as usize;
            let query = TabQuery {
                frame: Some(frame.frame),
                limit: remaining.min(size),
                ..Default::default()
            };
            if let Ok(tabs) = controller.snapshot().tab_page(&query) {
                for row in tabs.rows {
                    if row
                        .group
                        .is_some_and(|g| controller.snapshot().groups[&g].collapsed)
                    {
                        continue;
                    }
                    tab_button(ui, &row, controller, host, settings.group_colors, report);
                }
            }
        });
    }
    for tool in &projection.tools {
        let Some(record_tool) = controller.snapshot().tools.get(&tool.tool).cloned() else {
            continue;
        };
        if !tool.presentation.visible {
            report.reject(format!(
                "tool {} hidden: target unavailable; preserved view {}",
                tool.tool.raw(),
                record_tool.target.view.raw()
            ));
            continue;
        }
        egui::Area::new(egui::Id::new((surface.viewport, "tool", tool.tool.raw())))
            .order(egui::Order::Foreground)
            .fixed_pos(to_egui(tool.bounds).min)
            .show(ctx, |ui| {
                ui.set_clip_rect(to_egui(tool.clip));
                ui.set_width(tool.bounds.width() as f32);
                let layout = if record_tool.policy.orientation == kumihimo_core::Axis::Vertical {
                    egui::Layout::top_down(egui::Align::Min)
                } else {
                    egui::Layout::left_to_right(egui::Align::Center)
                };
                ui.with_layout(layout, |ui| {
                    let drag = ui.add(egui::Label::new("⠿").sense(egui::Sense::click_and_drag()));
                    drag.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            true,
                            format!("Move tool {}", tool.tool.raw()),
                        )
                    });
                    record(
                        report,
                        &drag,
                        format!("tool/{}/drag", tool.tool.raw()),
                        format!("Move tool {}", tool.tool.raw()),
                        Some(record_tool.target.view),
                    );
                    let label = format!(
                        "Tool {} → view {}",
                        tool.tool.raw(),
                        record_tool.target.view.raw()
                    );
                    let response = ui
                        .add_enabled(
                            tool.presentation.applicable && tool.presentation.usable,
                            egui::Button::new(&label),
                        )
                        .on_hover_text(format!(
                            "Pinned target resource {}; origin {}; host owner {}",
                            record_tool.target.resource.raw(),
                            record_tool.origin.raw(),
                            record_tool.owner.raw()
                        ));
                    record(
                        report,
                        &response,
                        format!("tool/{}/action", tool.tool.raw()),
                        label,
                        Some(record_tool.target.view),
                    );
                    if response.clicked() {
                        if let Err(e) = host.tool_action(controller, tool.tool, "activate") {
                            report.reject(e);
                        }
                    }
                    let dock = ui.menu_button("Dock tool", |ui| {
                        let offset = page(
                            ui,
                            state,
                            "tool-placements",
                            record_tool.policy.allowed.len(),
                            size,
                            report,
                        );
                        for (index, placement) in record_tool
                            .policy
                            .allowed
                            .iter()
                            .enumerate()
                            .skip(offset)
                            .take(size)
                        {
                            if button(
                                ui,
                                &format!("tool/{}/placement/{index}", tool.tool.raw()),
                                &format!("{placement:?}"),
                                report,
                            ) {
                                preview(
                                    controller,
                                    DockIntent::Tool {
                                        tool: tool.tool,
                                        placement: *placement,
                                    },
                                    surface,
                                    state,
                                    report,
                                );
                            }
                        }
                    });
                    record(
                        report,
                        &dock.response,
                        format!("tool/{}/dock-menu", tool.tool.raw()),
                        "Dock tool".into(),
                        Some(record_tool.target.view),
                    );
                });
            });
    }
    crate::input::draw_resize_handles(ctx, controller, projection, surface, &policy, report);
    if let Some(preview) = &state.preview {
        crate::input::paint_preview(ctx, preview, ThemeTokens::resolve(ctx, &policy));
    }
}
pub fn surface_frames(controller: &Controller, root: WorkspaceId) -> Vec<FrameId> {
    let mut pending = vec![root];
    let mut seen = BTreeSet::new();
    let mut result = vec![];
    while let Some(workspace) = pending.pop() {
        if !seen.insert(workspace) {
            continue;
        }
        if let Some(record) = controller.snapshot().workspaces.get(&workspace) {
            for frame in &record.frames {
                result.push(*frame);
                if let Some(child) = controller.snapshot().frames[frame].child_workspace {
                    pending.push(child);
                }
            }
        }
    }
    result
}
fn tab_button(
    ui: &mut egui::Ui,
    row: &TabRow,
    controller: &mut Controller,
    host: &mut impl DesktopHost,
    colors: bool,
    report: &mut FrameReport,
) {
    let label = format!(
        "{}{}{}{}{}",
        row.group_label
            .as_ref()
            .map(|g| format!("[{g}] "))
            .unwrap_or_default(),
        if row.pinned { "📌 " } else { "" },
        row.label,
        if row.unsaved { " *" } else { "" },
        if row.placeholder {
            " (unavailable)"
        } else {
            ""
        }
    );
    let mut text = egui::RichText::new(&label);
    if colors {
        if let Some(color) = row.group_color.as_deref().and_then(parse_color) {
            text = text.color(color);
        }
    }
    let response = ui
        .push_id(("tab", row.id.raw()), |ui| {
            ui.selectable_label(row.selected, text)
        })
        .inner;
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::Tab);
        node.set_selected(row.selected);
    });
    record(
        report,
        &response,
        format!("view/{}/tab", row.id.raw()),
        label,
        Some(row.id),
    );
    if response.clicked() {
        command(
            controller,
            host,
            Command::SelectView { view: row.id },
            report,
        );
    }
}
fn parse_color(value: &str) -> Option<egui::Color32> {
    let raw = value.strip_prefix('#')?;
    if raw.len() != 6 {
        return None;
    }
    let color = u32::from_str_radix(raw, 16).ok()?;
    Some(egui::Color32::from_rgb(
        (color >> 16) as u8,
        (color >> 8) as u8,
        color as u8,
    ))
}
fn tab_list(
    ui: &mut egui::Ui,
    frame: FrameId,
    controller: &mut Controller,
    host: &mut impl DesktopHost,
    state: &mut ChromeState,
    size: usize,
    colors: bool,
    report: &mut FrameReport,
) {
    ui.add(
        egui::TextEdit::singleline(&mut state.search)
            .hint_text("Search canonical tabs (including closed)"),
    );
    let key = format!("tabs/{}", frame.raw());
    let query = TabQuery {
        frame: Some(frame),
        include_closed: true,
        search: (!state.search.is_empty()).then(|| state.search.clone()),
        limit: size,
        ..Default::default()
    };
    let first = match controller.snapshot().tab_page(&query) {
        Ok(p) => p,
        Err(e) => {
            report.reject(e);
            return;
        }
    };
    let offset = page(ui, state, &key, first.matched, size, report);
    let query = TabQuery { offset, ..query };
    if let Ok(tabs) = controller.snapshot().tab_page(&query) {
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .show(ui, |ui| {
                for row in tabs.rows {
                    ui.horizontal(|ui| {
                        if !row.closed {
                            tab_button(ui, &row, controller, host, colors, report);
                        } else {
                            ui.label(format!("{} (closed)", row.label));
                        }
                        let actions = ui.menu_button("Actions", |ui| {
                            view_menu(ui, &row, controller, host, state, size, report)
                        });
                        record(
                            report,
                            &actions.response,
                            format!("view/{}/actions-menu", row.id.raw()),
                            "Tab actions".into(),
                            Some(row.id),
                        );
                    });
                }
            });
    }
    let groups = controller.snapshot().frames[&frame].groups.clone();
    let offset = page(
        ui,
        state,
        &format!("groups/{}", frame.raw()),
        groups.len(),
        size,
        report,
    );
    for group in groups.into_iter().skip(offset).take(size) {
        let label = controller.snapshot().groups[&group].label.clone();
        let menu = ui.menu_button(format!("Group: {label}"), |ui| {
            group_menu(ui, group, controller, host, state, size, report)
        });
        record(
            report,
            &menu.response,
            format!("group/{}/menu", group.raw()),
            format!("Group: {label}"),
            None,
        );
    }
}
