//! Derived controls; only the host dispatcher can change canonical state.
use crate::*;
use kumihimo_core::{DockIntent, DockPreview, GroupId, TabQuery, TabRow};
use std::collections::{BTreeMap, BTreeSet};

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
    response.clicked()
}
fn page(ui: &mut egui::Ui, state: &mut ChromeState, key: &str, count: usize, size: usize) -> usize {
    let offset = state.pages.entry(key.to_owned()).or_default();
    *offset = (*offset / size * size).min(count.saturating_sub(1) / size * size);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(*offset > 0, egui::Button::new("Previous page"))
            .clicked()
        {
            *offset = offset.saturating_sub(size);
        }
        ui.label(format!(
            "{}–{} / {}",
            if count == 0 { 0 } else { *offset + 1 },
            (*offset + size).min(count),
            count
        ));
        if ui
            .add_enabled(*offset + size < count, egui::Button::new("Next page"))
            .clicked()
        {
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
) -> Option<FrameId> {
    let ids: Vec<_> = controller.snapshot().frames.keys().copied().collect();
    let offset = page(ui, state, key, ids.len(), size);
    let mut selected = None;
    for id in ids.into_iter().skip(offset).take(size) {
        if ui.button(format!("Frame {}", id.raw())).clicked() {
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
                destinations(ui, controller, state, "reopen-destinations", size)
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
        if ui
            .button(if view.pinned { "Unpin" } else { "Pin" })
            .clicked()
        {
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
            if ui.button(label).clicked() {
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
    ui.menu_button("Move tab to…", |ui| {
        if let Some(destination) = destinations(ui, controller, state, "view-destinations", size) {
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
        let offset = page(ui, state, "assign-groups", groups.len(), size);
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
    if host.capabilities().native && ui.button("Native popout tab").clicked() {
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
    if ui
        .button(if record.collapsed {
            "Expand group"
        } else {
            "Collapse group"
        })
        .clicked()
    {
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
        if ui.button(label).clicked() {
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
    ui.menu_button("Move entire canonical group to…", |ui| {
        if let Some(destination) = destinations(ui, controller, state, "group-destinations", size) {
            let target = &controller.snapshot().frames[&destination];
            let index = target.views.len()
                - if destination == record.frame {
                    members
                } else {
                    0
                };
            let group_index = target.groups.len() - usize::from(destination == record.frame);
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
        }
    });
    if host.capabilities().native && ui.button("Native popout group").clicked() {
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
    if ui
        .button(if f.floating {
            "Preview redock"
        } else {
            "Preview in-app float"
        })
        .clicked()
    {
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
        let offset = page(ui, state, "dock-locations", locations.len(), size);
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
    ui.menu_button("Swap with…", |ui| {
        if let Some(second) = destinations(ui, controller, state, "swap-destinations", size) {
            command(
                controller,
                host,
                Command::SwapFrames {
                    first: f.frame,
                    second,
                },
                report,
            );
        }
    });
    if host.capabilities().native && ui.button("Native popout frame").clicked() {
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
        .show(ctx, |ui| {
            ui.menu_button("Panes / reveal (F6)", |ui| {
                ui.label("Canonical pane list — includes hidden and undersized panes");
                let ids = surface_frames(controller, surface.root);
                let offset = page(ui, state, "pane-list", ids.len(), size);
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
                            tab_list(ui, frame, controller, host, state, size, report);
                        });
                    });
                }
            });
            if let Some(preview) = state.preview.clone() {
                ui.horizontal(|ui| {
                    ui.label("Nonmutating dock preview");
                    if ui.button("Commit preview").clicked() {
                        state.preview.take();
                        command(controller, host, Command::CommitPreview(preview), report);
                    }
                    if ui.button("Cancel preview (Escape)").clicked() {
                        state.preview.take();
                    }
                });
            }
        });
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
        if (settings.profile == ChromeProfile::Hidden && !revealed) || contextual {
            let rect = egui::Rect::from_min_size(to_egui(frame.bounds).min, egui::vec2(26.0, 24.0))
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
            ui.menu_button(
                if settings.profile == ChromeProfile::Compact {
                    "⋮"
                } else {
                    "Frame"
                },
                |ui| {
                    frame_menu(ui, frame, controller, surface, host, state, size, report);
                },
            );
            ui.menu_button(
                if settings.profile == ChromeProfile::Compact {
                    "Tabs"
                } else {
                    "Tabs / groups"
                },
                |ui| {
                    tab_list(ui, frame.frame, controller, host, state, size, report);
                },
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
                    ui.menu_button("Dock tool", |ui| {
                        let offset = page(
                            ui,
                            state,
                            "tool-placements",
                            record_tool.policy.allowed.len(),
                            size,
                        );
                        for placement in record_tool.policy.allowed.iter().skip(offset).take(size) {
                            if ui.button(format!("{placement:?}")).clicked() {
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
                });
            });
    }
    crate::input::draw_resize_handles(ctx, controller, projection, surface, &policy, report);
    if let Some(preview) = &state.preview {
        crate::input::paint_preview(ctx, preview);
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
    let offset = page(ui, state, &key, first.matched, size);
    let query = TabQuery { offset, ..query };
    if let Ok(tabs) = controller.snapshot().tab_page(&query) {
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .show(ui, |ui| {
                for row in tabs.rows {
                    ui.horizontal(|ui| {
                        if !row.closed {
                            tab_button(ui, &row, controller, host, true, report);
                        } else {
                            ui.label(format!("{} (closed)", row.label));
                        }
                        ui.menu_button("Actions", |ui| {
                            view_menu(ui, &row, controller, host, state, size, report)
                        });
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
    );
    for group in groups.into_iter().skip(offset).take(size) {
        let label = controller.snapshot().groups[&group].label.clone();
        ui.menu_button(format!("Group: {label}"), |ui| {
            group_menu(ui, group, controller, host, state, size, report)
        });
    }
}
