//! Explicit gesture ownership and global-DIP content routing.
use crate::*;
use kumihimo_core::{
    Axis, DockIntent, DockPreview, LayoutNode, LayoutPlan, NodeId, Scalar, ToolPlacement,
};
use std::collections::{BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PointerSource {
    Mouse,
    Touch,
    HostPen,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPhase {
    Press,
    Move,
    Release,
    Cancel,
}
#[derive(Clone, Copy, Debug)]
pub struct HostPointerEvent {
    pub pointer: u64,
    pub source: PointerSource,
    pub phase: PointerPhase,
    pub position: egui::Pos2,
    pub epoch: u64,
}
#[derive(Clone, Debug)]
enum DragTarget {
    Frame {
        frame: FrameId,
        bounds: Rect,
    },
    FloatResize {
        frame: FrameId,
        bounds: Rect,
    },
    Tool(ToolId),
    Resize {
        workspace: WorkspaceId,
        node: NodeId,
        axis: Axis,
        index: usize,
        plan: LayoutPlan,
        bounds: Rect,
    },
}
#[derive(Clone, Debug)]
pub struct Capture {
    pub pointer: u64,
    pub source: PointerSource,
    pub press: egui::Pos2,
    pub position: egui::Pos2,
    pub generation: u64,
    pub epoch: u64,
    pub pressed_at: f64,
    pub activated: bool,
    hit_size: f32,
    long_press_seconds: f64,
    target: DragTarget,
    preview: Option<DockPreview>,
    resize_plan: Option<LayoutPlan>,
    resize_geometry: Option<kumihimo_core::GeometryOutput>,
}
#[derive(Default)]
pub struct InputState {
    capture: Option<Capture>,
    pub(crate) injected: VecDeque<HostPointerEvent>,
    pub last_settlement: Option<&'static str>,
    pub focused_view: Option<ViewId>,
    pub consumed_pointer: bool,
    pub(crate) consumed_pointers: BTreeSet<(u64, PointerSource)>,
    #[cfg(feature = "navigation")]
    pub(crate) navigation_capture: Option<crate::navigation::NavigationCapture>,
}
impl InputState {
    pub fn active_capture(&self) -> Option<&Capture> {
        self.capture.as_ref()
    }
    /// Host classification is explicit. Pressure never implies pen.
    pub fn feed_pointer(&mut self, event: HostPointerEvent) -> Result<()> {
        if event.pointer == 0
            || event.pointer == u64::MAX
            || event.epoch == 0
            || !event.position.x.is_finite()
            || !event.position.y.is_finite()
        {
            return Err(AdapterError::Invalid("host pointer identity/position"));
        }
        if self.injected.len() >= 64 {
            return Err(AdapterError::Budget("host pointer queue"));
        }
        self.injected.push_back(event);
        Ok(())
    }
    pub fn cancel(&mut self, reason: &'static str) {
        self.capture.take();
        self.last_settlement = Some(reason);
    }
}
pub(crate) fn pointer_events(ctx: &egui::Context, epoch: u64) -> Vec<HostPointerEvent> {
    let events = ctx.input(|i| i.events.clone());
    let touch = events
        .iter()
        .any(|e| matches!(e, egui::Event::Touch { .. }));
    events
        .into_iter()
        .filter_map(|event| match event {
            egui::Event::Touch { id, phase, pos, .. } => Some(HostPointerEvent {
                pointer: id.0.checked_add(2)?,
                source: PointerSource::Touch,
                phase: match phase {
                    egui::TouchPhase::Start => PointerPhase::Press,
                    egui::TouchPhase::Move => PointerPhase::Move,
                    egui::TouchPhase::End => PointerPhase::Release,
                    egui::TouchPhase::Cancel => PointerPhase::Cancel,
                },
                position: pos,
                epoch,
            }),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                ..
            } if !touch => Some(HostPointerEvent {
                pointer: 1,
                source: PointerSource::Mouse,
                phase: if pressed {
                    PointerPhase::Press
                } else {
                    PointerPhase::Release
                },
                position: pos,
                epoch,
            }),
            egui::Event::PointerMoved(pos) if !touch => Some(HostPointerEvent {
                pointer: 1,
                source: PointerSource::Mouse,
                phase: PointerPhase::Move,
                position: pos,
                epoch,
            }),
            _ => None,
        })
        .collect()
}
pub fn pick_frame(projection: &workspace::Projection, point: egui::Pos2) -> Option<FrameId> {
    projection
        .frames
        .iter()
        .rev()
        .find(|f| to_egui(f.bounds).contains(point) && to_egui(f.clip).contains(point))
        .map(|f| f.frame)
}
fn hit_drag(
    controller: &Controller,
    projection: &workspace::Projection,
    point: egui::Pos2,
    report: &FrameReport,
) -> Option<DragTarget> {
    let picked = pick_frame(projection, point);
    for control in report
        .controls
        .iter()
        .rev()
        .filter(|c| c.enabled && to_egui(c.bounds).contains(point))
    {
        let parts: Vec<_> = control.id.split('/').collect();
        match parts.as_slice() {
            ["frame", raw, "drag"] => {
                let frame = FrameId::from_raw(raw.parse().ok()?);
                if picked != Some(frame) {
                    continue;
                }
                let bounds = projection.frames.iter().find(|f| f.frame == frame)?.bounds;
                return Some(DragTarget::Frame { frame, bounds });
            }
            ["frame", raw, "resize-float"] => {
                let frame = FrameId::from_raw(raw.parse().ok()?);
                if picked != Some(frame) {
                    continue;
                }
                let bounds = controller.snapshot().floats.get(&frame)?.rect;
                return Some(DragTarget::FloatResize { frame, bounds });
            }
            ["tool", raw, "drag"] => {
                let tool = ToolId::from_raw(raw.parse().ok()?);
                if !projection.tools.iter().any(|t| {
                    t.tool == tool
                        && t.presentation.visible
                        && to_egui(t.clip).contains(point)
                        && to_egui(t.bounds).contains(point)
                }) {
                    continue;
                }
                return Some(DragTarget::Tool(tool));
            }
            ["resize", workspace, node, axis, index] => {
                let workspace = WorkspaceId::from_raw(workspace.parse().ok()?);
                if picked.is_some_and(|id| controller.snapshot().frames[&id].workspace != workspace)
                {
                    continue;
                }
                let node = NodeId(node.parse().ok()?);
                let axis = if *axis == "x" {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                let plan = controller.snapshot().layouts.get(&workspace)?.clone();
                let bounds = controller.snapshot().geometry(workspace).ok()?.nodes[&node];
                return Some(DragTarget::Resize {
                    workspace,
                    node,
                    axis,
                    index: index.parse().ok()?,
                    plan,
                    bounds,
                });
            }
            _ => {}
        }
    }
    None
}
fn tool_point(
    controller: &Controller,
    projection: &workspace::Projection,
    placement: ToolPlacement,
) -> Option<egui::Rect> {
    use kumihimo_core::{Edge, ToolAnchor};
    let (rect, edge) = match placement {
        ToolPlacement::Cell {
            workspace,
            grid,
            row,
            column,
        } => {
            return controller
                .snapshot()
                .geometry(workspace)
                .ok()?
                .cells
                .get(&(grid, row, column))
                .map(|r| to_egui(*r));
        }
        ToolPlacement::Edge { anchor, edge } => {
            let rect = match anchor {
                ToolAnchor::View(view) => {
                    projection
                        .frames
                        .iter()
                        .find(|f| f.frame == controller.snapshot().views.get(&view)?.frame)?
                        .content
                }
                ToolAnchor::Frame(frame) => {
                    projection.frames.iter().find(|f| f.frame == frame)?.bounds
                }
                ToolAnchor::Workspace(workspace) => {
                    controller.snapshot().geometry(workspace).ok()?.content
                }
            };
            (to_egui(rect), edge)
        }
    };
    Some(match edge {
        Edge::Top => egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + 16.0)),
        Edge::Bottom => {
            egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - 16.0), rect.max)
        }
        Edge::Left => egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + 16.0, rect.max.y)),
        Edge::Right => {
            egui::Rect::from_min_max(egui::pos2(rect.max.x - 16.0, rect.min.y), rect.max)
        }
    })
}
fn update_preview(
    capture: &mut Capture,
    controller: &Controller,
    projection: &workspace::Projection,
    report: &mut FrameReport,
) {
    capture.preview = None;
    capture.resize_plan = None;
    capture.resize_geometry = None;
    let intent = match &capture.target {
        DragTarget::FloatResize { frame, bounds } => {
            let delta = capture.position - capture.press;
            match Rect::new(
                bounds.x(),
                bounds.y(),
                bounds.width() + delta.x as f64,
                bounds.height() + delta.y as f64,
            ) {
                Ok(rect) => DockIntent::Float {
                    frame: *frame,
                    rect,
                },
                Err(error) => {
                    report.reject(error);
                    return;
                }
            }
        }
        DragTarget::Frame { frame, bounds } => {
            let mut closest = None;
            for workspace in projection
                .frames
                .iter()
                .map(|f| f.workspace)
                .collect::<BTreeSet<_>>()
            {
                if let Ok(candidates) = controller.snapshot().snap_candidates(
                    workspace,
                    Scalar::new(capture.position.x as f64).unwrap(),
                    Scalar::new(capture.position.y as f64).unwrap(),
                    Scalar::new(24.0).unwrap(),
                ) {
                    if let Some(candidate) = candidates.first() {
                        if closest
                            .as_ref()
                            .is_none_or(|c: &kumihimo_core::SnapCandidate| {
                                candidate.distance.get() < c.distance.get()
                            })
                        {
                            closest = Some(candidate.clone());
                        }
                    }
                }
            }
            if let Some(candidate) = closest {
                DockIntent::Frame {
                    frame: *frame,
                    location: candidate.location,
                }
            } else {
                let delta = capture.position - capture.press;
                let rect = match Rect::new(
                    bounds.x() + delta.x as f64,
                    bounds.y() + delta.y as f64,
                    bounds.width(),
                    bounds.height(),
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        report.reject(e);
                        return;
                    }
                };
                DockIntent::Float {
                    frame: *frame,
                    rect,
                }
            }
        }
        DragTarget::Tool(tool) => {
            let Some(record) = controller.snapshot().tools.get(tool) else {
                return;
            };
            let Some(placement) = record.policy.allowed.iter().find(|p| {
                tool_point(controller, projection, **p)
                    .is_some_and(|r| r.contains(capture.position))
            }) else {
                return;
            };
            DockIntent::Tool {
                tool: *tool,
                placement: *placement,
            }
        }
        DragTarget::Resize {
            workspace,
            node,
            axis,
            index,
            plan,
            bounds,
        } => {
            let delta = if *axis == Axis::Horizontal {
                capture.position.x - capture.press.x
            } else {
                capture.position.y - capture.press.y
            } as f64;
            let extent = if *axis == Axis::Horizontal {
                bounds.width()
            } else {
                bounds.height()
            };
            if extent <= 0.0 {
                report.reject(AdapterError::Invalid("resize zero extent"));
                return;
            }
            let mut candidate = plan.clone();
            let weights: Vec<f64> = match candidate.nodes.get(node) {
                Some(LayoutNode::Split { children, .. }) => {
                    children.iter().map(|n| n.weight.get()).collect()
                }
                Some(LayoutNode::Grid { rows, columns, .. }) => if *axis == Axis::Horizontal {
                    columns
                } else {
                    rows
                }
                .iter()
                .map(|v| v.get())
                .collect(),
                _ => return,
            };
            if *index + 1 >= weights.len() {
                return;
            }
            let sum: f64 = weights.iter().sum();
            // Keep all other tracks fixed; no canonical writes before release.
            let change = delta / extent * sum;
            let a = weights[*index] + change;
            let b = weights[*index + 1] - change;
            if !a.is_finite() || !b.is_finite() || a <= 0.0 || b <= 0.0 {
                report.reject(AdapterError::Invalid("resize impossible proportions"));
                return;
            }
            match candidate.nodes.get_mut(node) {
                Some(LayoutNode::Split { children, .. }) => {
                    children[*index].weight = Scalar::new(a).unwrap();
                    children[*index + 1].weight = Scalar::new(b).unwrap();
                }
                Some(LayoutNode::Grid { rows, columns, .. }) => {
                    let tracks = if *axis == Axis::Horizontal {
                        columns
                    } else {
                        rows
                    };
                    tracks[*index] = Scalar::new(a).unwrap();
                    tracks[*index + 1] = Scalar::new(b).unwrap();
                }
                _ => return,
            }
            let mut diagnostic = controller.snapshot().clone();
            diagnostic.layouts.insert(*workspace, candidate.clone());
            match diagnostic.geometry(*workspace) {
                Ok(geometry) => {
                    capture.resize_plan = Some(candidate);
                    capture.resize_geometry = Some(geometry);
                }
                Err(e) => report.reject(e),
            }
            return;
        }
    };
    match controller.core().preview(intent) {
        Ok(p) => capture.preview = Some(p),
        Err(error) => report.reject(error),
    }
}
pub fn process(
    ctx: &egui::Context,
    controller: &mut Controller,
    projection: &workspace::Projection,
    surface: SurfaceBinding,
    host: &mut impl DesktopHost,
    state: &mut InputState,
    report: &mut FrameReport,
) {
    let policy = host.policy();
    let now = ctx.input(|i| i.time);
    if state.focused_view.is_some_and(|id| {
        controller
            .snapshot()
            .views
            .get(&id)
            .is_none_or(|v| v.closed.is_some())
    }) {
        state.focused_view = None;
    }
    if state.capture.as_ref().is_some_and(|c| {
        c.epoch != surface.epoch || c.generation != controller.snapshot().generation
    }) {
        state.cancel("invalidated");
        report.reject(AdapterError::Stale("gesture generation/surface"));
    }
    if ctx.input(|i| {
        !i.focused
            || i.key_pressed(egui::Key::Escape)
            || i.events
                .iter()
                .any(|e| matches!(e, egui::Event::WindowFocused(false)))
    }) {
        state.cancel("focus/capture lost or Escape");
    }
    let mut events = pointer_events(ctx, surface.epoch);
    events.extend(state.injected.drain(..));
    if ctx.input(|i| {
        i.events
            .iter()
            .any(|e| matches!(e, egui::Event::PointerGone))
    }) && !events.iter().any(|e| e.phase == PointerPhase::Release)
    {
        state.cancel("capture lost");
    }
    for event in events {
        if event.pointer == 0
            || event.pointer == u64::MAX
            || !event.position.x.is_finite()
            || !event.position.y.is_finite()
        {
            report.reject(AdapterError::Invalid("pointer identity/position"));
            continue;
        }
        if event.epoch != surface.epoch {
            report.reject(AdapterError::Stale("host pointer epoch"));
            continue;
        }
        if state
            .consumed_pointers
            .contains(&(event.pointer, event.source))
        {
            continue;
        }
        match event.phase {
            PointerPhase::Press if state.capture.is_none() => {
                if let Some(target) = hit_drag(controller, projection, event.position, report) {
                    let settings = projection
                        .frames
                        .iter()
                        .find(|f| Some(f.frame) == pick_frame(projection, event.position))
                        .map(|f| &f.settings)
                        .unwrap_or(&policy.settings);
                    state.capture = Some(Capture {
                        pointer: event.pointer,
                        source: event.source,
                        press: event.position,
                        position: event.position,
                        generation: controller.snapshot().generation,
                        epoch: surface.epoch,
                        pressed_at: now,
                        activated: event.source != PointerSource::Touch,
                        hit_size: settings.hit_size,
                        long_press_seconds: settings.long_press_seconds,
                        target,
                        preview: None,
                        resize_plan: None,
                        resize_geometry: None,
                    });
                    state.last_settlement = None;
                } else {
                    route_pointer(controller, projection, host, state, report, event);
                }
            }
            PointerPhase::Move => {
                if let Some(capture) = state
                    .capture
                    .as_mut()
                    .filter(|c| c.pointer == event.pointer && c.source == event.source)
                {
                    capture.position = event.position;
                    if capture.source == PointerSource::Touch && !capture.activated {
                        if capture.position.distance(capture.press) > capture.hit_size {
                            state.cancel("touch scrolling before long press");
                            continue;
                        }
                        capture.activated = now - capture.pressed_at >= capture.long_press_seconds;
                    }
                    if capture.activated {
                        update_preview(capture, controller, projection, report);
                    }
                } else {
                    route_pointer(controller, projection, host, state, report, event);
                }
            }
            PointerPhase::Release => {
                if state
                    .capture
                    .as_ref()
                    .is_some_and(|c| c.pointer == event.pointer && c.source == event.source)
                {
                    let mut capture = state.capture.take().unwrap();
                    capture.position = event.position;
                    if capture.activated && capture.position.distance(capture.press) > 2.0 {
                        update_preview(&mut capture, controller, projection, report);
                        if let Some(preview) = capture.preview {
                            crate::chrome::command(
                                controller,
                                host,
                                Command::CommitPreview(preview),
                                report,
                            );
                        } else if let (DragTarget::Resize { workspace, .. }, Some(plan)) =
                            (capture.target, capture.resize_plan)
                        {
                            crate::chrome::command(
                                controller,
                                host,
                                Command::SetLayout { workspace, plan },
                                report,
                            );
                        }
                    }
                    state.last_settlement = Some("release");
                } else {
                    route_pointer(controller, projection, host, state, report, event);
                }
            }
            PointerPhase::Cancel => {
                if state
                    .capture
                    .as_ref()
                    .is_some_and(|c| c.pointer == event.pointer)
                {
                    state.cancel("pointer cancelled");
                }
            }
            _ => {}
        }
    }
    if let Some(capture) = &mut state.capture {
        if capture.source == PointerSource::Touch && !capture.activated {
            if now - capture.pressed_at >= capture.long_press_seconds {
                capture.activated = true;
                update_preview(capture, controller, projection, report);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                    (capture.long_press_seconds - (now - capture.pressed_at)).max(0.001),
                ));
            }
        }
        if let Some(preview) = &capture.preview {
            paint_preview(ctx, preview);
        }
        if let Some(geometry) = &capture.resize_geometry {
            paint_geometry(ctx, geometry);
        }
    }
    // Frame shortcuts are explicit chords. Plain text, wheel and editor shortcuts retain content ownership.
    keyboard_resize(ctx, controller, projection, surface, host, report, now);
    let active_settings = projection
        .frames
        .iter()
        .find(|f| {
            controller.snapshot().frames[&f.frame].selected_view == state.focused_view
                && state.focused_view.is_some()
        })
        .map(|f| &f.settings)
        .unwrap_or(&policy.settings);
    let frame_shortcuts = active_settings.frame_shortcuts_first || !ctx.wants_keyboard_input();
    if frame_shortcuts {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Tab)) {
            if let Some(view) = state.focused_view.or_else(|| {
                projection
                    .frames
                    .iter()
                    .find_map(|f| controller.snapshot().frames[&f.frame].selected_view)
            }) {
                let frame =
                    &controller.snapshot().frames[&controller.snapshot().views[&view].frame];
                if let Some(index) = frame.views.iter().position(|id| *id == view) {
                    let next = frame.views[(index + 1) % frame.views.len()];
                    crate::chrome::command(
                        controller,
                        host,
                        Command::SelectView { view: next },
                        report,
                    );
                    state.focused_view = Some(next);
                }
            }
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F10)) {
            if let Some(view) = state.focused_view {
                let frame = controller.snapshot().views[&view].frame;
                let workspace = controller.snapshot().frames[&frame].workspace;
                let command = if controller.snapshot().maximized.contains_key(&workspace) {
                    Command::RestoreLayout { workspace }
                } else {
                    Command::Maximize { frame }
                };
                crate::chrome::command(controller, host, command, report);
            }
        }
    }
    let events = ctx.input(|i| i.events.clone());
    for event in events {
        let payload = match event {
            egui::Event::Text(text) => Some(format!("text:{text}")),
            egui::Event::Paste(text) => Some(format!("paste:{text}")),
            egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } => Some(format!("key:{key:?}:{pressed}:{modifiers:?}")),
            egui::Event::MouseWheel { delta, unit, .. }
                if !state.consumed_pointer && state.capture.is_none() =>
            {
                if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                    route_content(
                        controller,
                        projection,
                        host,
                        state,
                        report,
                        pos,
                        format!("wheel:{unit:?}:{}:{}", delta.x, delta.y),
                    );
                }
                None
            }
            _ => None,
        };
        if let Some(payload) = payload {
            if let Some(slot) = report
                .slots
                .iter()
                .find(|s| Some(s.view) == state.focused_view)
                .cloned()
            {
                if let Err(error) = host.content_input(
                    controller,
                    &slot,
                    kumihimo_content::Input {
                        owner: kumihimo_content::InputOwner::Content,
                        payload,
                        x: None,
                        y: None,
                    },
                ) {
                    report.reject(error);
                }
            }
        }
    }
    state.consumed_pointer = false;
    state.consumed_pointers.clear();
}
fn route_pointer(
    controller: &mut Controller,
    projection: &workspace::Projection,
    host: &mut impl DesktopHost,
    state: &mut InputState,
    report: &mut FrameReport,
    event: HostPointerEvent,
) {
    route_content(
        controller,
        projection,
        host,
        state,
        report,
        event.position,
        format!(
            "pointer:{:?}:{}:{:?}",
            event.source, event.pointer, event.phase
        ),
    );
}
fn route_content(
    controller: &mut Controller,
    projection: &workspace::Projection,
    host: &mut impl DesktopHost,
    state: &mut InputState,
    report: &mut FrameReport,
    pos: egui::Pos2,
    payload: String,
) {
    let Some(frame) = pick_frame(projection, pos) else {
        return;
    };
    // A chrome widget cannot forward its click into the editor under it.
    if report
        .controls
        .iter()
        .any(|c| c.enabled && to_egui(c.bounds).contains(pos))
    {
        return;
    }
    let Some(slot) = report
        .slots
        .iter()
        .rev()
        .find(|s| {
            s.frame == frame
                && s.visible
                && to_egui(s.clip).contains(pos)
                && to_egui(s.bounds).contains(pos)
        })
        .cloned()
    else {
        return;
    };
    if payload.ends_with(":Press") {
        state.focused_view = Some(slot.view);
    }
    let (Ok(x), Ok(y)) = (Scalar::new(pos.x as f64), Scalar::new(pos.y as f64)) else {
        report.reject(AdapterError::Invalid("content pointer"));
        return;
    };
    if let Err(error) = host.content_input(
        controller,
        &slot,
        kumihimo_content::Input {
            owner: kumihimo_content::InputOwner::Content,
            payload,
            x: Some(x),
            y: Some(y),
        },
    ) {
        report.reject(error);
    }
}
pub(crate) fn paint_preview(ctx: &egui::Context, preview: &DockPreview) {
    for geometry in preview.geometry() {
        paint_geometry(ctx, geometry);
    }
}
fn keyboard_resize(
    ctx: &egui::Context,
    controller: &mut Controller,
    projection: &workspace::Projection,
    surface: SurfaceBinding,
    host: &mut impl DesktopHost,
    report: &mut FrameReport,
    now: f64,
) {
    let controls: Vec<_> = report
        .controls
        .iter()
        .filter(|c| c.id.starts_with("resize/") && c.enabled)
        .cloned()
        .collect();
    for control in controls {
        let id = egui::Id::new(&control.id);
        let horizontal = control.id.split('/').nth(3) == Some("x");
        let mut pixels = 0.0_f64;
        if ctx.memory(|m| m.has_focus(id)) {
            let backward = if horizontal {
                egui::Key::ArrowLeft
            } else {
                egui::Key::ArrowUp
            };
            let forward = if horizontal {
                egui::Key::ArrowRight
            } else {
                egui::Key::ArrowDown
            };
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, backward)) {
                pixels -= 8.0;
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, forward)) {
                pixels += 8.0;
            }
        }
        pixels += 8.0
            * ctx.input(|i| {
                i.num_accesskit_action_requests(id, egui::accesskit::Action::Increment)
                    .min(16)
            }) as f64;
        pixels -= 8.0
            * ctx.input(|i| {
                i.num_accesskit_action_requests(id, egui::accesskit::Action::Decrement)
                    .min(16)
            }) as f64;
        let value = ctx.input(|i| {
            i.accesskit_action_requests(id, egui::accesskit::Action::SetValue)
                .filter_map(|r| {
                    if let Some(egui::accesskit::ActionData::NumericValue(v)) = r.data {
                        Some(v)
                    } else {
                        None
                    }
                })
                .last()
        });
        if pixels == 0.0 && value.is_none() {
            continue;
        }
        let press = to_egui(control.bounds).center();
        let Some(target @ DragTarget::Resize { .. }) =
            hit_drag(controller, projection, press, report)
        else {
            report.reject(AdapterError::Stale("resize control target"));
            continue;
        };
        if let (
            Some(value),
            DragTarget::Resize {
                node,
                axis,
                index,
                plan,
                bounds,
                ..
            },
        ) = (value, &target)
        {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                report.reject(AdapterError::Invalid("accessible resize value"));
                continue;
            }
            let weights: Vec<_> = match &plan.nodes[node] {
                LayoutNode::Split { children, .. } => {
                    children.iter().map(|v| v.weight.get()).collect()
                }
                LayoutNode::Grid { rows, columns, .. } => if *axis == Axis::Horizontal {
                    columns
                } else {
                    rows
                }
                .iter()
                .map(|v| v.get())
                .collect(),
                _ => vec![],
            };
            let total: f64 = weights.iter().sum();
            let before: f64 = weights.iter().take(*index + 1).sum();
            pixels = (value - before / total)
                * if *axis == Axis::Horizontal {
                    bounds.width()
                } else {
                    bounds.height()
                };
        }
        let position = press
            + if horizontal {
                egui::vec2(pixels as f32, 0.0)
            } else {
                egui::vec2(0.0, pixels as f32)
            };
        let settings = host.policy().settings;
        let mut candidate = Capture {
            pointer: 1,
            source: PointerSource::Mouse,
            press,
            position,
            generation: controller.snapshot().generation,
            epoch: surface.epoch,
            pressed_at: now,
            activated: true,
            hit_size: settings.hit_size,
            long_press_seconds: settings.long_press_seconds,
            target,
            preview: None,
            resize_plan: None,
            resize_geometry: None,
        };
        update_preview(&mut candidate, controller, projection, report);
        if let (DragTarget::Resize { workspace, .. }, Some(plan)) =
            (candidate.target, candidate.resize_plan)
        {
            crate::chrome::command(
                controller,
                host,
                Command::SetLayout { workspace, plan },
                report,
            );
        }
    }
}
fn paint_geometry(ctx: &egui::Context, geometry: &kumihimo_core::GeometryOutput) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("kumihimo-preview"),
    ));
    for bounds in geometry.frames.values().chain(geometry.tools.values()) {
        painter.rect_stroke(
            to_egui(*bounds),
            0.0,
            egui::Stroke::new(2.0, egui::Color32::LIGHT_BLUE),
            egui::StrokeKind::Inside,
        );
    }
}
pub(crate) fn draw_resize_handles(
    ctx: &egui::Context,
    controller: &Controller,
    projection: &workspace::Projection,
    surface: SurfaceBinding,
    policy: &HostPolicy,
    report: &mut FrameReport,
) {
    for frame in projection.frames.iter().filter(|f| f.floating) {
        let bounds = to_egui(frame.bounds);
        let size = frame.settings.hit_size.max(16.0);
        let rect = egui::Rect::from_min_max(bounds.max - egui::vec2(size, size), bounds.max)
            .intersect(to_egui(frame.clip));
        let id = format!("frame/{}/resize-float", frame.frame.raw());
        let ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new((surface.viewport, &id)),
            egui::UiBuilder::new()
                .layer_id(frame.layer())
                .max_rect(rect),
        );
        let response = ui.interact(rect, egui::Id::new(&id), egui::Sense::click_and_drag());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("Resize floating frame {}", frame.frame.raw()),
            )
        });
        ui.painter().line_segment(
            [rect.left_bottom(), rect.right_top()],
            egui::Stroke::new(2.0, egui::Color32::GRAY),
        );
        crate::chrome::record(
            report,
            &response,
            id,
            format!("Resize floating frame {}", frame.frame.raw()),
            None,
        );
    }
    let workspaces: BTreeSet<_> = projection.frames.iter().map(|f| f.workspace).collect();
    for workspace in workspaces {
        if controller.snapshot().maximized.contains_key(&workspace) {
            continue;
        }
        let Some(plan) = controller.snapshot().layouts.get(&workspace) else {
            continue;
        };
        let Ok(geometry) = controller.snapshot().geometry(workspace) else {
            continue;
        };
        let Some(owner) = projection.frames.iter().find(|f| f.workspace == workspace) else {
            continue;
        };
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new((surface.viewport, workspace.raw(), "resize-ui")),
            egui::UiBuilder::new()
                .layer_id(owner.layer())
                .max_rect(to_egui(geometry.viewport)),
        );
        let ancestor_clip = controller.snapshot().workspaces[&workspace]
            .owner
            .and_then(|id| {
                projection
                    .frames
                    .iter()
                    .find(|f| f.frame == id)
                    .map(|f| f.clip)
            })
            .unwrap_or(surface.bounds);
        ui.set_clip_rect(to_egui(geometry.viewport).intersect(to_egui(ancestor_clip)));
        for (node, entry) in &plan.nodes {
            let Some(bounds) = geometry.nodes.get(node).copied() else {
                continue;
            };
            let tracks: Vec<(Axis, Vec<f64>)> = match entry {
                LayoutNode::Split { axis, children } => {
                    vec![(*axis, children.iter().map(|c| c.weight.get()).collect())]
                }
                LayoutNode::Grid { rows, columns, .. } => vec![
                    (Axis::Horizontal, columns.iter().map(|s| s.get()).collect()),
                    (Axis::Vertical, rows.iter().map(|s| s.get()).collect()),
                ],
                _ => vec![],
            };
            for (axis, weights) in tracks {
                let total: f64 = weights.iter().sum();
                let mut cumulative = 0.0;
                for index in 0..weights.len().saturating_sub(1) {
                    if report.controls.len()
                        >= policy
                            .max_regions
                            .saturating_mul(policy.tab_page_size.max(1))
                    {
                        report.reject(AdapterError::Budget("resize controls"));
                        return;
                    }
                    cumulative += weights[index];
                    let fraction = cumulative / total;
                    let gap = controller
                        .snapshot()
                        .contexts
                        .get(&workspace)
                        .map_or(0.0, |c| c.gap.get());
                    let coordinate = match entry {
                        LayoutNode::Split { children, .. } => {
                            geometry.nodes.get(&children[index].node).map(|r| {
                                if axis == Axis::Horizontal {
                                    r.right()
                                } else {
                                    r.bottom()
                                }
                            })
                        }
                        LayoutNode::Grid { .. } => {
                            geometry.cells.iter().find_map(|((grid, row, col), r)| {
                                if *grid == *node
                                    && (if axis == Axis::Horizontal {
                                        *col == index
                                    } else {
                                        *row == index
                                    })
                                {
                                    Some(if axis == Axis::Horizontal {
                                        r.right()
                                    } else {
                                        r.bottom()
                                    })
                                } else {
                                    None
                                }
                            })
                        }
                        _ => None,
                    }
                    .map(|v| v + gap / 2.0)
                    .unwrap_or_else(|| {
                        if axis == Axis::Horizontal {
                            bounds.x() + bounds.width() * fraction
                        } else {
                            bounds.y() + bounds.height() * fraction
                        }
                    });
                    let rect = if axis == Axis::Horizontal {
                        egui::Rect::from_center_size(
                            egui::pos2(
                                coordinate as f32,
                                (bounds.y() + bounds.height() / 2.0) as f32,
                            ),
                            egui::vec2(owner.settings.hit_size, bounds.height() as f32),
                        )
                    } else {
                        egui::Rect::from_center_size(
                            egui::pos2(
                                (bounds.x() + bounds.width() / 2.0) as f32,
                                coordinate as f32,
                            ),
                            egui::vec2(bounds.width() as f32, owner.settings.hit_size),
                        )
                    };
                    let key = format!(
                        "resize/{}/{}/{}/{}",
                        workspace.raw(),
                        node.0,
                        if axis == Axis::Horizontal { "x" } else { "y" },
                        index
                    );
                    let response =
                        ui.interact(rect, egui::Id::new(&key), egui::Sense::click_and_drag());
                    response.widget_info(|| {
                        egui::WidgetInfo::slider(
                            true,
                            fraction,
                            format!(
                                "Resize {} tracks {} and {}",
                                workspace.raw(),
                                index + 1,
                                index + 2
                            ),
                        )
                    });
                    ctx.accesskit_node_builder(response.id, |node| {
                        node.set_min_numeric_value(0.0);
                        node.set_max_numeric_value(1.0);
                        node.add_action(egui::accesskit::Action::Increment);
                        node.add_action(egui::accesskit::Action::Decrement);
                        node.add_action(egui::accesskit::Action::SetValue);
                    });
                    crate::chrome::record(
                        report,
                        &response,
                        key,
                        "Resize adjacent tracks; drag; keyboard arrows".into(),
                        None,
                    );
                    let line = if axis == Axis::Horizontal {
                        [rect.center_top(), rect.center_bottom()]
                    } else {
                        [rect.left_center(), rect.right_center()]
                    };
                    ui.painter().line_segment(
                        line,
                        egui::Stroke::new(
                            if response.hovered() { 3.0 } else { 1.0 },
                            egui::Color32::from_gray(110),
                        ),
                    );
                }
            }
        }
    }
}
