use crate::{projection, wire::*, WebError};
use kumihimo_content as content;
use kumihimo_core as core;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const LINEAGE: &str = "kumihimo-portal-pilot";
const RECEIPTS: usize = 64;
const PREVIEWS: usize = 16;
const STORE_BYTES: usize = 1024 * 1024;

/// Browser effects remain explicit host decisions, outside portable arrangement.
pub trait HostDispatcher {
    fn dispatch(&mut self, request: &content::Request) -> content::AckResult;
}

/// Reusable adapter borrows its canonical Controller and effect owner.
pub struct WebAdapter<'a, D: HostDispatcher> {
    pub controller: &'a mut content::Controller,
    pub dispatcher: &'a mut D,
}
impl<D: HostDispatcher> WebAdapter<'_, D> {
    pub fn execute(&mut self, envelope: core::Envelope) -> Result<core::Response, WebError> {
        Ok(self.controller.execute(envelope)?)
    }
    pub fn flush(&mut self, mount: content::MountId) -> Result<content::Receipt, WebError> {
        let permission = permission();
        let request = self
            .controller
            .dispatch(mount, permission, 0)?
            .ok_or_else(|| WebError::new("not_dispatched"))?;
        let result = self.dispatcher.dispatch(&request);
        let receipt = self.controller.ack(
            content::Ack {
                ticket: request.ticket,
                result,
            },
            0,
        )?;
        if receipt.status != content::RequestStatus::Confirmed {
            return Err(WebError::new("host_rejected").scoped(mount.0.to_string()));
        }
        Ok(receipt)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PilotConfig {
    pub task_count: usize,
    pub detail_count: usize,
    pub nesting_depth: usize,
    pub width: f64,
    pub height: f64,
    pub max_records: usize,
}
impl Default for PilotConfig {
    fn default() -> Self {
        Self {
            task_count: 3,
            detail_count: 2,
            nesting_depth: 1,
            width: 1100.0,
            height: 700.0,
            max_records: 4096,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    id: String,
    title: String,
    body: String,
    column: String,
    status: String,
    resource: Decimal,
    provider: Decimal,
    views: Vec<Decimal>,
    veto: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskStore {
    version: u32,
    revision: Decimal,
    tasks: Vec<Task>,
    #[serde(skip)]
    last_recipient: Option<Value>,
}
impl HostDispatcher for TaskStore {
    fn dispatch(&mut self, request: &content::Request) -> content::AckResult {
        use content::{AckResult::*, Confirmation::*, RequestKind::*};
        let scope = &request.ticket.scope;
        if matches!(
            request.kind,
            content::RequestKind::Input(_) | content::RequestKind::AssetAction(_)
        ) {
            self.last_recipient = Some(json!({"provider":scope.provider.raw().to_string(),
                "resource":scope.resource.raw().to_string(),"view":scope.target.view().map(|v|v.raw().to_string()),
                "mount":scope.mount.0.to_string(),"request":request.ticket.id.0.to_string()}));
        }
        match &request.kind {
            Attach { .. } => Confirmed(Attached {
                strength: content::RestorationStrength::LayoutOnly,
            }),
            content::RequestKind::Geometry(geometry) => {
                Confirmed(content::Confirmation::Geometry {
                    anchor: geometry.anchor.clone(),
                })
            }
            Input(input) => {
                if let Some(recipient) = self.last_recipient.as_mut() {
                    recipient["local"] =
                        json!({"x":input.x.map(|x|x.get()),"y":input.y.map(|y|y.get())});
                }
                Confirmed(Done)
            }
            AssetAction(action) => {
                if action.asset.revision != self.revision.0.to_string() {
                    return Rejected;
                }
                let Some(index) = self.tasks.iter().position(|task| {
                    task.id == action.asset.identity
                        && task.resource.0 == scope.resource.raw()
                        && task.provider.0 == scope.provider.raw()
                        && scope
                            .target
                            .view()
                            .is_some_and(|view| task.views.iter().any(|v| v.0 == view.raw()))
                }) else {
                    return Rejected;
                };
                let Ok(next) = serde_json::from_str::<HostAction>(&action.payload) else {
                    return Rejected;
                };
                let Some(revision) = self.revision.0.checked_add(1) else {
                    return Rejected;
                };
                let mut candidate = self.clone();
                let task = &mut candidate.tasks[index];
                match next {
                    HostAction::Edit { title, body } => {
                        task.title = title;
                        task.body = body;
                    }
                    HostAction::Domain { column, status } => {
                        task.column = column;
                        task.status = status;
                    }
                    HostAction::Veto { enabled } => {
                        task.veto = enabled;
                    }
                }
                candidate.revision = Decimal(revision);
                match serde_json::to_vec(&candidate) {
                    Ok(bytes) if bytes.len() <= STORE_BYTES => {}
                    _ => return Rejected,
                }
                *self = candidate;
                Confirmed(Done)
            }
            Focus(_) | Presentation(_) | Invalidate | Detach | Dispose => Confirmed(Done),
            _ => Unsupported,
        }
    }
}

struct Preview {
    identity: core::Identity,
    generation: u64,
    command: core::Command,
    dock: Option<core::DockPreview>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutSession {
    version: u32,
    layout: String,
    expanded: Vec<Decimal>,
}

/// Demonstration host: a single controller, independent tasks, bounded transient state.
pub struct PilotHost {
    controller: content::Controller,
    tasks: TaskStore,
    config: PilotConfig,
    root: core::WorkspaceId,
    scale: f64,
    epoch: u64,
    mounts: BTreeMap<core::ViewId, content::MountId>,
    previews: BTreeMap<u64, Preview>,
    next_preview: u64,
    converted: VecDeque<(u64, Value, core::Envelope)>,
}

fn permission() -> content::HostPermission {
    content::HostPermission {
        allowed: true,
        eligibility_revision: 0,
    }
}
fn limits(config: &PilotConfig) -> core::Limits {
    core::Limits {
        max_records: Some(config.max_records),
        max_layout_entries: Some(config.max_records.saturating_mul(16)),
        max_label_bytes: Some(8192),
        max_key_bytes: Some(256),
        ..Default::default()
    }
}
fn content_limits(config: &PilotConfig) -> content::ContentLimits {
    content::ContentLimits {
        mounts: Some(config.max_records),
        instances: Some(config.max_records),
        leases: Some(config.max_records.saturating_mul(2)),
        payload_bytes: Some(8192),
        receipts: Some(RECEIPTS),
        ..Default::default()
    }
}
fn next_envelope(
    controller: &content::Controller,
    command: core::Command,
) -> Result<core::Envelope, WebError> {
    Ok(core::Envelope {
        identity: controller.snapshot().identity.clone(),
        actor: "pilot-host".into(),
        operation: controller
            .core()
            .ledger_status()
            .high_watermark
            .checked_next()
            .ok_or_else(|| WebError::new("operation_exhausted"))?,
        expected_generation: controller.snapshot().generation,
        command,
    })
}
fn execute_internal(
    controller: &mut content::Controller,
    command: core::Command,
) -> Result<core::Change, WebError> {
    let envelope = next_envelope(controller, command)?;
    Ok(controller.execute(envelope)?.outcome.result?)
}
fn create(
    controller: &mut content::Controller,
    command: core::Command,
) -> Result<core::EntityId, WebError> {
    match execute_internal(controller, command)? {
        core::Change::Created(id) => Ok(id),
        _ => Err(WebError::new("creation_failed")),
    }
}
fn create_frame(
    controller: &mut content::Controller,
    workspace: core::WorkspaceId,
) -> Result<core::FrameId, WebError> {
    let index = controller.snapshot().workspaces[&workspace].frames.len();
    match create(controller, core::Command::CreateFrame { workspace, index })? {
        core::EntityId::Frame(id) => Ok(id),
        _ => Err(WebError::new("creation_failed")),
    }
}
fn create_workspace(
    controller: &mut content::Controller,
    owner: Option<core::FrameId>,
) -> Result<core::WorkspaceId, WebError> {
    match create(controller, core::Command::CreateWorkspace { owner })? {
        core::EntityId::Workspace(id) => Ok(id),
        _ => Err(WebError::new("creation_failed")),
    }
}
fn add_view(
    controller: &mut content::Controller,
    frame: core::FrameId,
    task: &mut Task,
    label: String,
) -> Result<core::ViewId, WebError> {
    let index = controller.snapshot().frames[&frame].views.len();
    let core::EntityId::View(view) = create(
        controller,
        core::Command::CreateView {
            frame,
            resource: core::ResourceId::from_raw(task.resource.0),
            index,
        },
    )?
    else {
        return Err(WebError::new("creation_failed"));
    };
    execute_internal(
        controller,
        core::Command::SetViewMetadata {
            view,
            label,
            pinned: false,
            unsaved: false,
        },
    )?;
    execute_internal(
        controller,
        core::Command::SetViewState {
            view,
            state: core::ViewState {
                visible: true,
                usable: true,
            },
        },
    )?;
    task.views.push(Decimal(view.raw()));
    Ok(view)
}
fn plan(
    controller: &mut content::Controller,
    workspace: core::WorkspaceId,
    grid: bool,
) -> Result<(), WebError> {
    let frames = controller.snapshot().workspaces[&workspace].frames.clone();
    if frames.is_empty() {
        return Ok(());
    }
    let mut nodes = BTreeMap::new();
    let mut children = vec![];
    for (index, frame) in frames.iter().enumerate() {
        let node = core::NodeId(index as u64 + 2);
        nodes.insert(
            node,
            core::LayoutNode::Leaf(core::LayoutItem::Frame(*frame)),
        );
        children.push(core::WeightedNode {
            node,
            weight: core::Scalar::new(1.0)?,
        });
    }
    let root = core::NodeId(1);
    let container = if grid {
        let columns = (frames.len() as f64).sqrt().ceil() as usize;
        let rows = frames.len().div_ceil(columns);
        core::LayoutNode::Grid {
            rows: vec![core::Scalar::new(1.0)?; rows],
            columns: vec![core::Scalar::new(1.0)?; columns],
            cells: children
                .iter()
                .enumerate()
                .map(|(i, child)| core::Cell {
                    row: i / columns,
                    column: i % columns,
                    node: child.node,
                })
                .collect(),
        }
    } else {
        core::LayoutNode::Split {
            axis: core::Axis::Horizontal,
            children,
        }
    };
    nodes.insert(root, container);
    execute_internal(
        controller,
        core::Command::SetLayout {
            workspace,
            plan: core::LayoutPlan {
                root: Some(root),
                nodes,
            },
        },
    )?;
    Ok(())
}

impl PilotHost {
    pub fn new(incarnation: impl Into<String>, config: PilotConfig) -> Result<Self, WebError> {
        if config.task_count == 0
            || config.max_records == 0
            || config.task_count > config.max_records
            || config.detail_count > config.max_records
            || config.nesting_depth > config.max_records
        {
            return Err(WebError::new("config_admission"));
        }
        let viewport = core::Rect::new(0.0, 0.0, config.width, config.height)?;
        let identity = core::Identity::new(LINEAGE, incarnation)?;
        let core = core::Core::new(identity.clone(), limits(&config), RECEIPTS)?;
        let mut controller =
            content::Controller::new(core, identity.incarnation.clone(), content_limits(&config))?;
        let root = create_workspace(&mut controller, None)?;
        let core::EntityId::Provider(provider) = create(
            &mut controller,
            core::Command::RegisterProvider {
                key: "portal-demo".into(),
                supports_shared_views: true,
            },
        )?
        else {
            return Err(WebError::new("creation_failed"));
        };
        let mut tasks = TaskStore {
            version: VERSION,
            revision: Decimal(0),
            tasks: vec![],
            last_recipient: None,
        };
        for index in 0..config.task_count {
            let frame = create_frame(&mut controller, root)?;
            let id = format!("task-{}", index + 1);
            let core::EntityId::Resource(resource) = create(
                &mut controller,
                core::Command::RegisterResource {
                    provider,
                    key: id.clone(),
                    restore_reference: id.clone(),
                },
            )?
            else {
                return Err(WebError::new("creation_failed"));
            };
            let mut task = Task {
                id,
                title: format!("Task {}", index + 1),
                body: "Editable host-owned task notes".into(),
                column: ["Backlog", "Doing", "Done"][index % 3].into(),
                status: "open".into(),
                resource: Decimal(resource.raw()),
                provider: Decimal(provider.raw()),
                views: vec![],
                veto: false,
            };
            let first = add_view(&mut controller, frame, &mut task, "Card".into())?;
            let second = add_view(&mut controller, frame, &mut task, "Notes".into())?;
            let core::EntityId::Group(group) = create(
                &mut controller,
                core::Command::CreateGroup {
                    frame,
                    label: "Task views".into(),
                    color: Some("#5b9cc6".into()),
                },
            )?
            else {
                return Err(WebError::new("creation_failed"));
            };
            for view in [first, second] {
                execute_internal(
                    &mut controller,
                    core::Command::AssignGroup {
                        view,
                        group: Some(group),
                    },
                )?;
            }
            execute_internal(&mut controller, core::Command::SelectView { view: first })?;
            let mut pending = VecDeque::from([(frame, 0usize)]);
            while let Some((owner, depth)) = pending.pop_front() {
                if depth >= config.nesting_depth || config.detail_count == 0 {
                    continue;
                }
                let child = create_workspace(&mut controller, Some(owner))?;
                for detail in 0..config.detail_count {
                    let detail_frame = create_frame(&mut controller, child)?;
                    add_view(
                        &mut controller,
                        detail_frame,
                        &mut task,
                        format!("Detail {}.{}", depth + 1, detail + 1),
                    )?;
                    pending.push_back((detail_frame, depth + 1));
                }
                plan(&mut controller, child, true)?;
            }
            tasks.tasks.push(task);
        }
        plan(&mut controller, root, false)?;
        projection::synchronize_contexts(&mut controller, root, viewport)?;
        let mut host = Self {
            controller,
            tasks,
            config,
            root,
            scale: 1.0,
            epoch: 1,
            mounts: BTreeMap::new(),
            previews: BTreeMap::new(),
            next_preview: 1,
            converted: VecDeque::new(),
        };
        host.attach_views()?;
        Ok(host)
    }
    fn attach_views(&mut self) -> Result<(), WebError> {
        self.mounts.clear();
        let providers: Vec<_> = self
            .controller
            .snapshot()
            .providers
            .values()
            .filter(|p| p.availability == core::ProviderAvailability::Available)
            .map(|p| p.id)
            .collect();
        let capabilities = content::Capabilities {
            sharing: true,
            animation: false,
            anchored_reflow: false,
            tools: false,
            cache: false,
            embed_input: false,
            lifecycle: BTreeSet::new(),
            restoration: BTreeSet::from([content::RestorationStrength::LayoutOnly]),
            ..Default::default()
        };
        for provider in providers {
            self.controller.negotiate(
                provider,
                content::ProtocolVersion::V2,
                capabilities.clone(),
                capabilities.clone(),
            )?;
        }
        let views: Vec<_> = self
            .controller
            .snapshot()
            .views
            .values()
            .filter(|v| v.closed.is_none())
            .map(|v| (v.id, v.resource))
            .collect();
        let mut instances = BTreeMap::new();
        for (view, resource) in views {
            let provider = self.controller.snapshot().resources[&resource].provider;
            if self.controller.snapshot().providers[&provider].availability
                != core::ProviderAvailability::Available
            {
                continue;
            }
            let mount = self.controller.attach(
                view,
                content::AttachOptions {
                    share: instances.get(&resource).copied(),
                    host_share: true,
                    ..Default::default()
                },
                permission(),
                0,
            )?;
            WebAdapter {
                controller: &mut self.controller,
                dispatcher: &mut self.tasks,
            }
            .flush(mount)?;
            instances.insert(
                resource,
                self.controller.inspect().mounts[&mount].scope.instance,
            );
            self.mounts.insert(view, mount);
        }
        self.publish_geometry()
    }
    fn snapshot(&self) -> Result<Value, WebError> {
        let mut result = projection::project(&self.controller, self.root, self.scale, self.epoch)?;
        result["capabilities"] = json!({"adapter_version":"0.1.0","rust_wasm":"implemented",
            "layout":"rust_core","input":"pointer_keyboard","storage":"host_localStorage",
            "popup_migration":"unsupported","physical_touch_pen":"unproven","performance":"unproven"});
        Ok(result)
    }
    fn publish_geometry(&mut self) -> Result<(), WebError> {
        let snapshot = self.snapshot()?;
        let frames = snapshot["frames"]
            .as_array()
            .ok_or_else(|| WebError::new("projection_shape"))?;
        for frame in frames {
            let Some(selected) = frame["selected_view"]
                .as_str()
                .and_then(|v| v.parse::<u64>().ok())
            else {
                continue;
            };
            let view = core::ViewId::from_raw(selected);
            let Some(mount) = self.mounts.get(&view).copied() else {
                continue;
            };
            let rect = rect_from_value(&frame["content"])?;
            let clip = intersect(rect, rect_from_value(&frame["clip"])?)?;
            // Provider input and geometry share one local coordinate system.
            // Projection rectangles remain global browser logical DIPs.
            let local_rect = core::Rect::new(0.0, 0.0, rect.width(), rect.height())?;
            let (x, width) = local_clip_axis(clip.x(), clip.right(), rect.x(), rect.width());
            let (y, height) = local_clip_axis(clip.y(), clip.bottom(), rect.y(), rect.height());
            let local_clip = core::Rect::new(x, y, width, height)?;
            self.controller.request(
                mount,
                content::RequestKind::Geometry(content::Geometry {
                    rect: local_rect,
                    clip: local_clip,
                    scale: core::Scalar::new(self.scale)?,
                    anchor: None,
                }),
                permission(),
                0,
            )?;
            WebAdapter {
                controller: &mut self.controller,
                dispatcher: &mut self.tasks,
            }
            .flush(mount)?;
        }
        Ok(())
    }
    fn sync(&mut self) -> Result<(), WebError> {
        let viewport = self
            .controller
            .snapshot()
            .contexts
            .get(&self.root)
            .ok_or_else(|| WebError::new("missing_context"))?
            .viewport;
        projection::synchronize_contexts(&mut self.controller, self.root, viewport)?;
        self.publish_geometry()
    }
    pub fn route_json(&mut self, request: &str) -> String {
        let parsed = parse_request(request);
        let (route, outcome) = match parsed {
            Ok(request) => self.route(request),
            Err(error) => ("boundary", Err(error)),
        };
        let response = match outcome {
            Ok(result) => {
                let ok = result
                    .get("receipt")
                    .and_then(|receipt| receipt.get("ok"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if ok {
                    json!({"version":VERSION,"route":route,"ok":true,"result":result})
                } else {
                    json!({"version":VERSION,"route":route,"ok":false,"error":result["receipt"]["error"],"result":result})
                }
            }
            Err(error) => json!({"version":VERSION,"route":route,"ok":false,"error":error}),
        };
        serde_json::to_string(&response).unwrap_or_else(|_|"{\"version\":1,\"route\":\"boundary\",\"ok\":false,\"error\":{\"code\":\"serialization\"}}".into())
    }
    fn route(&mut self, request: Request) -> (&'static str, Result<Value, WebError>) {
        match request {
            Request::Snapshot { .. } => ("snapshot", self.snapshot()),
            Request::HostSnapshot { .. } => (
                "host_snapshot",
                Ok(
                    json!({"revision":self.tasks.revision,"tasks":self.tasks.tasks,
                "last_recipient":self.tasks.last_recipient}),
                ),
            ),
            Request::Save { .. } => ("save", self.save()),
            Request::Load {
                layout,
                host,
                fresh_incarnation,
                ..
            } => ("load", self.load(&layout, &host, fresh_incarnation)),
            Request::Preview {
                identity,
                expected_generation,
                action,
                ..
            } => (
                "preview",
                self.preview(identity, expected_generation.0, action),
            ),
            Request::CancelPreview { token, .. } => (
                "cancel_preview",
                if self.previews.remove(&token.0).is_some() {
                    Ok(
                        json!({"cancelled":true,"generation":self.controller.snapshot().generation.to_string()}),
                    )
                } else {
                    Err(WebError::new("missing_preview"))
                },
            ),
            Request::Command {
                version,
                identity,
                actor,
                operation,
                expected_generation,
                action,
            } => {
                let request = Request::Command {
                    version,
                    identity: identity.clone(),
                    actor: actor.clone(),
                    operation,
                    expected_generation,
                    action: action.clone(),
                };
                (
                    "command",
                    self.command(
                        identity,
                        actor,
                        operation.0,
                        expected_generation.0,
                        action,
                        request,
                    ),
                )
            }
            Request::HostCommand {
                identity,
                actor,
                expected_revision,
                view,
                resource,
                provider,
                action,
                ..
            } => (
                "host_command",
                self.host_command(
                    identity,
                    &actor,
                    expected_revision.0,
                    view.0,
                    resource.0,
                    provider.0,
                    action,
                ),
            ),
            Request::Input {
                identity,
                epoch,
                view,
                resource,
                provider,
                x,
                y,
                scale,
                payload,
                ..
            } => (
                "input",
                self.input(
                    identity, epoch.0, view.0, resource.0, provider.0, x, y, scale, payload,
                ),
            ),
        }
    }
    fn preview(
        &mut self,
        identity: core::Identity,
        generation: u64,
        action: Action,
    ) -> Result<Value, WebError> {
        if identity != self.controller.snapshot().identity {
            return Err(WebError::new("identity_mismatch"));
        }
        if generation != self.controller.snapshot().generation {
            return Err(WebError::new("stale_generation"));
        }
        if self.previews.len() >= PREVIEWS {
            return Err(WebError::new("preview_admission"));
        }
        let token = self.next_preview;
        self.next_preview = token
            .checked_add(1)
            .ok_or_else(|| WebError::new("preview_exhausted"))?;
        let command = action.to_command(self.controller.snapshot())?;
        let dock = action
            .dock_intent()
            .ok()
            .map(|intent| self.controller.core().preview(intent))
            .transpose()?;
        // A detached real Core validates all non-docking preview commands as well.
        let candidate = core::Core::from_persisted(
            self.controller.core().save_layout(),
            format!("preview-{token}"),
            self.restore_options(),
            RECEIPTS,
        )?;
        let mut controller = content::Controller::new(
            candidate,
            format!("preview-{token}"),
            content_limits(&self.config),
        )?;
        for view in self.controller.snapshot().views.values() {
            execute_internal(
                &mut controller,
                core::Command::SetViewState {
                    view: view.id,
                    state: view.state,
                },
            )?;
        }
        execute_internal(&mut controller, command.clone())?;
        let viewport = controller.snapshot().contexts[&self.root].viewport;
        projection::synchronize_contexts(&mut controller, self.root, viewport)?;
        let geometry = projection::project(&controller, self.root, self.scale, self.epoch)?;
        self.previews.insert(
            token,
            Preview {
                identity,
                generation,
                command,
                dock,
            },
        );
        Ok(
            json!({"token":token.to_string(),"generation":generation.to_string(),"geometry":geometry}),
        )
    }
    fn command(
        &mut self,
        identity: core::Identity,
        actor: String,
        operation: u64,
        generation: u64,
        action: Action,
        request: Request,
    ) -> Result<Value, WebError> {
        let fingerprint =
            serde_json::to_value(&request).map_err(|_| WebError::new("invalid_envelope"))?;
        let original = self
            .converted
            .iter()
            .find(|(id, intent, _)| *id == operation && intent == &fingerprint)
            .map(|(_, _, envelope)| envelope.clone());
        let mut scale = None;
        let envelope = if let Some(envelope) = original {
            envelope
        } else {
            let command = match action {
                Action::Viewport {
                    rect,
                    scale: next_scale,
                } => {
                    if !next_scale.is_finite() || next_scale <= 0.0 {
                        return Err(WebError::new("invalid_scale"));
                    }
                    scale = Some(next_scale);
                    let mut context = self.controller.snapshot().contexts[&self.root].clone();
                    context.viewport = rect.to_rect()?;
                    core::Command::SetGeometryContext {
                        workspace: self.root,
                        context,
                    }
                }
                Action::CommitPreview { token } => {
                    let preview = self
                        .previews
                        .get(&token.0)
                        .ok_or_else(|| WebError::new("missing_preview"))?;
                    if preview.identity != identity {
                        return Err(WebError::new("identity_mismatch"));
                    }
                    if preview.generation != generation
                        || generation != self.controller.snapshot().generation
                    {
                        return Err(WebError::new("stale_preview"));
                    }
                    preview
                        .dock
                        .clone()
                        .map(core::Command::CommitPreview)
                        .unwrap_or_else(|| preview.command.clone())
                }
                Action::Close { view, approved } => {
                    let veto = self
                        .tasks
                        .tasks
                        .iter()
                        .any(|task| task.veto && task.views.iter().any(|v| v.0 == view.0));
                    core::Command::CloseView {
                        view: core::ViewId::from_raw(view.0),
                        approved: approved && !veto,
                    }
                }
                _ => action.to_command(self.controller.snapshot())?,
            };
            let envelope = core::Envelope {
                identity,
                actor,
                operation: core::OperationId::new(operation),
                expected_generation: generation,
                command,
            };
            if !self.converted.iter().any(|(id, _, _)| *id == operation) {
                if self.converted.len() == RECEIPTS {
                    self.converted.pop_front();
                }
                self.converted
                    .push_back((operation, fingerprint, envelope.clone()));
            }
            envelope
        };
        let provider_changed = matches!(
            envelope.command,
            core::Command::SetProviderAvailability { .. }
        );
        let response = WebAdapter {
            controller: &mut self.controller,
            dispatcher: &mut self.tasks,
        }
        .execute(envelope)?;
        let accepted = response.outcome.result.is_ok();
        if accepted && !response.replayed {
            if let Some(scale) = scale {
                self.scale = scale;
            }
            // Postcommit projection/host failure is reported separately; it never means rollback.
            let synced = if provider_changed {
                self.attach_views().and_then(|_| self.sync())
            } else {
                self.sync()
            };
            if let Err(error) = synced {
                return Ok(
                    json!({"receipt":receipt(&response),"postcommit_error":error,"committed":true,
                    "current_generation":self.controller.snapshot().generation.to_string()}),
                );
            }
            self.previews
                .retain(|_, preview| preview.generation == self.controller.snapshot().generation);
        }
        let mut result = json!({"receipt":receipt(&response),"current":self.snapshot()?});
        result["accepted"] = json!(accepted);
        // Rejections retain their real Core receipt and independently current snapshot.
        Ok(result)
    }
    fn recipient(
        &self,
        identity: &core::Identity,
        view: u64,
        resource: u64,
        provider: u64,
    ) -> Result<content::MountId, WebError> {
        if identity != &self.controller.snapshot().identity {
            return Err(WebError::new("identity_mismatch"));
        }
        let id = core::ViewId::from_raw(view);
        let record = self
            .controller
            .snapshot()
            .views
            .get(&id)
            .filter(|v| v.closed.is_none())
            .ok_or_else(|| WebError::new("missing_target").scoped(view.to_string()))?;
        let actual = &self.controller.snapshot().resources[&record.resource];
        if record.resource.raw() != resource || actual.provider.raw() != provider {
            return Err(WebError::new("recipient_mismatch").scoped(view.to_string()));
        }
        if self.controller.snapshot().providers[&actual.provider].availability
            != core::ProviderAvailability::Available
        {
            return Err(WebError::new("missing_provider").scoped(provider.to_string()));
        }
        self.mounts
            .get(&id)
            .copied()
            .ok_or_else(|| WebError::new("missing_mount").scoped(view.to_string()))
    }
    fn host_command(
        &mut self,
        identity: core::Identity,
        actor: &str,
        revision: u64,
        view: u64,
        resource: u64,
        provider: u64,
        action: HostAction,
    ) -> Result<Value, WebError> {
        let mount = self.recipient(&identity, view, resource, provider)?;
        if revision != self.tasks.revision.0 {
            return Err(WebError::new("stale_host_revision"));
        }
        let task = self
            .tasks
            .tasks
            .iter()
            .find(|task| task.resource.0 == resource && task.views.iter().any(|v| v.0 == view))
            .ok_or_else(|| WebError::new("missing_host_task"))?;
        let payload = serde_json::to_string(&action).map_err(|_| WebError::new("host_payload"))?;
        let domain = matches!(action, HostAction::Domain { .. });
        let kind = content::RequestKind::AssetAction(content::AssetAction {
            asset: content::HostAssetReference {
                identity: task.id.clone(),
                revision: revision.to_string(),
            },
            mode: if domain {
                content::AssetMode::DomainChange
            } else {
                content::AssetMode::LinkedEdit
            },
            undo: if domain {
                content::UndoScope::HostDomain
            } else {
                content::UndoScope::HostDocument
            },
            payload,
        });
        self.controller.request(mount, kind, permission(), 0)?;
        let ack = WebAdapter {
            controller: &mut self.controller,
            dispatcher: &mut self.tasks,
        }
        .flush(mount)?;
        Ok(
            json!({"actor":actor,"revision":self.tasks.revision,"request":ack.ticket.id.0.to_string(),
            "recipient":self.tasks.last_recipient,"confirmed":true}),
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn input(
        &mut self,
        identity: core::Identity,
        epoch: u64,
        view: u64,
        resource: u64,
        provider: u64,
        x: f64,
        y: f64,
        scale: f64,
        payload: String,
    ) -> Result<Value, WebError> {
        if epoch != self.epoch {
            return Err(WebError::new("stale_surface"));
        }
        if scale != self.scale || !x.is_finite() || !y.is_finite() {
            return Err(WebError::new("invalid_scale"));
        }
        let mount = self.recipient(&identity, view, resource, provider)?;
        let snapshot = self.snapshot()?;
        let frame = snapshot["frames"]
            .as_array()
            .and_then(|frames| {
                frames
                    .iter()
                    .find(|frame| frame["selected_view"].as_str() == Some(&view.to_string()))
            })
            .ok_or_else(|| WebError::new("hidden_content"))?;
        if frame["visible"] != true || frame["expanded"] != true {
            return Err(WebError::new("hidden_content"));
        }
        let content = rect_from_value(&frame["content"])?;
        let clip = intersect(content, rect_from_value(&frame["clip"])?)?;
        if x < clip.x() || y < clip.y() || x >= clip.right() || y >= clip.bottom() {
            return Err(WebError::new("clipped_input"));
        }
        let local_x = x - content.x();
        let local_y = y - content.y();
        self.controller.request(
            mount,
            content::RequestKind::Input(content::Input {
                owner: content::InputOwner::Content,
                payload,
                x: Some(core::Scalar::new(local_x)?),
                y: Some(core::Scalar::new(local_y)?),
            }),
            permission(),
            0,
        )?;
        WebAdapter {
            controller: &mut self.controller,
            dispatcher: &mut self.tasks,
        }
        .flush(mount)?;
        Ok(
            json!({"local":{"x":local_x,"y":local_y},"recipient":self.tasks.last_recipient,"epoch":self.epoch.to_string()}),
        )
    }
    fn save(&self) -> Result<Value, WebError> {
        let bytes = kumihimo_layout_json::encode_core(
            self.controller.core(),
            kumihimo_layout_json::CodecOptions {
                max_bytes: STORE_BYTES,
            },
        )
        .map_err(|_| WebError::new("save_codec"))?;
        if !bytes.is_current(self.controller.core()) {
            return Err(WebError::new("stale_save"));
        }
        let session = LayoutSession {
            version: VERSION,
            layout: String::from_utf8(bytes.bytes().to_vec())
                .map_err(|_| WebError::new("save_encoding"))?,
            expanded: self
                .controller
                .snapshot()
                .views
                .values()
                .filter(|view| view.state.visible)
                .map(|view| Decimal(view.id.raw()))
                .collect(),
        };
        let layout = serde_json::to_string(&session).map_err(|_| WebError::new("save_encoding"))?;
        let host =
            serde_json::to_string(&self.tasks).map_err(|_| WebError::new("save_encoding"))?;
        if layout.len() > STORE_BYTES || host.len() > STORE_BYTES {
            return Err(WebError::new("store_admission"));
        }
        // A successful save must fit the very same bounded route used to reload it,
        // including JSON escaping and the longest admitted fresh incarnation token.
        let reload = Request::Load {
            version: VERSION,
            layout: layout.clone(),
            host: host.clone(),
            fresh_incarnation: "x".repeat(128),
        };
        if serde_json::to_vec(&reload)
            .map_err(|_| WebError::new("save_encoding"))?
            .len()
            > MAX_ROUTE_BYTES
        {
            return Err(WebError::new("save_route_admission"));
        }
        Ok(
            json!({"layout":layout,"host":host,"identity":self.controller.snapshot().identity,
            "generation":self.controller.snapshot().generation.to_string()}),
        )
    }
    fn restore_options(&self) -> core::RestoreOptions {
        core::RestoreOptions {
            limits: limits(&self.config),
            providers: vec![core::ProviderResolution {
                key: "portal-demo".into(),
                supports_shared_views: true,
            }],
            ..Default::default()
        }
    }
    fn load(
        &mut self,
        layout: &str,
        host: &str,
        fresh_incarnation: String,
    ) -> Result<Value, WebError> {
        if layout.len() > STORE_BYTES || host.len() > STORE_BYTES {
            return Err(WebError::new("store_admission"));
        }
        let session: LayoutSession =
            serde_json::from_str(layout).map_err(|_| WebError::new("invalid_session"))?;
        let tasks: TaskStore =
            serde_json::from_str(host).map_err(|_| WebError::new("invalid_host_store"))?;
        if session.version != VERSION || tasks.version != VERSION {
            return Err(WebError::new("unsupported_schema"));
        }
        if fresh_incarnation == self.controller.snapshot().identity.incarnation {
            return Err(WebError::new("stale_incarnation"));
        }
        let saved = kumihimo_layout_json::decode_with_options(
            session.layout.as_bytes(),
            kumihimo_layout_json::CodecOptions {
                max_bytes: STORE_BYTES,
            },
        )
        .map_err(|_| WebError::new("invalid_layout"))?;
        if saved.source.lineage != LINEAGE {
            return Err(WebError::new("identity_mismatch"));
        }
        let candidate = core::Core::from_persisted(
            saved,
            fresh_incarnation.clone(),
            self.restore_options(),
            RECEIPTS,
        )?;
        validate_tasks(&tasks, candidate.snapshot(), &self.config)?;
        let mut controller =
            content::Controller::new(candidate, fresh_incarnation, content_limits(&self.config))?;
        let root = controller
            .snapshot()
            .workspaces
            .values()
            .find(|workspace| workspace.owner.is_none())
            .map(|workspace| workspace.id)
            .ok_or_else(|| WebError::new("missing_root"))?;
        let expanded: BTreeSet<_> = session.expanded.iter().map(|value| value.0).collect();
        if expanded.len() != session.expanded.len()
            || expanded.iter().any(|id| {
                !controller
                    .snapshot()
                    .views
                    .contains_key(&core::ViewId::from_raw(*id))
            })
        {
            return Err(WebError::new("invalid_expansion"));
        }
        let views: Vec<_> = controller.snapshot().views.keys().copied().collect();
        for view in views {
            execute_internal(
                &mut controller,
                core::Command::SetViewState {
                    view,
                    state: core::ViewState {
                        visible: expanded.contains(&view.raw()),
                        usable: true,
                    },
                },
            )?;
        }
        let mut replacement = Self {
            controller,
            tasks,
            config: self.config.clone(),
            root,
            scale: self.scale,
            epoch: self
                .epoch
                .checked_add(1)
                .ok_or_else(|| WebError::new("surface_exhausted"))?,
            mounts: BTreeMap::new(),
            previews: BTreeMap::new(),
            next_preview: 1,
            converted: VecDeque::new(),
        };
        replacement.sync()?;
        replacement.attach_views()?;
        let result = replacement.snapshot()?;
        *self = replacement;
        Ok(result)
    }
}

fn receipt(response: &core::Response) -> Value {
    let error = response
        .outcome
        .result
        .as_ref()
        .err()
        .map(|error| WebError::from(error.clone()));
    json!({"identity":response.outcome.identity,"actor":response.outcome.actor,
        "operation":response.outcome.operation.sequence().to_string(),"generation":response.outcome.generation.to_string(),
        "replayed":response.replayed,"ok":response.outcome.result.is_ok(),"error":error})
}
fn rect_from_value(value: &Value) -> Result<core::Rect, WebError> {
    let number = |field: &str| {
        value[field]
            .as_f64()
            .ok_or_else(|| WebError::new("projection_rect"))
    };
    Ok(core::Rect::new(
        number("x")?,
        number("y")?,
        number("width")?,
        number("height")?,
    )?)
}
fn intersect(rect: core::Rect, clip: core::Rect) -> Result<core::Rect, WebError> {
    let x = rect.x().max(clip.x()).min(rect.right());
    let y = rect.y().max(clip.y()).min(rect.bottom());
    Ok(core::Rect::new(
        x,
        y,
        (rect.right().min(clip.right()) - x).max(0.0),
        (rect.bottom().min(clip.bottom()) - y).max(0.0),
    )?)
}
fn local_clip_axis(start: f64, end: f64, origin: f64, extent: f64) -> (f64, f64) {
    // Global subtraction may round beyond the original provider-local extent.
    let start = (start - origin).clamp(0.0, extent);
    let end = (end - origin).clamp(start, extent);
    let mut length = end - start;
    if start + length > extent {
        // Keep exact containment when reconstructing the endpoint also rounds up.
        length = extent.next_down().max(start) - start;
    }
    (start, length)
}
fn validate_tasks(
    tasks: &TaskStore,
    snapshot: &core::Snapshot,
    config: &PilotConfig,
) -> Result<(), WebError> {
    if tasks.tasks.len() > config.max_records {
        return Err(WebError::new("store_admission"));
    }
    let mut ids = BTreeSet::new();
    let mut resources = BTreeSet::new();
    let mut views = BTreeSet::new();
    for task in &tasks.tasks {
        if task.id.is_empty()
            || task.id.len() > 256
            || !ids.insert(&task.id)
            || !resources.insert(task.resource.0)
            || [&task.title, &task.body, &task.column, &task.status]
                .iter()
                .any(|text| text.len() > 8192)
        {
            return Err(WebError::new("invalid_host_store"));
        }
        let resource = snapshot
            .resources
            .get(&core::ResourceId::from_raw(task.resource.0))
            .ok_or_else(|| WebError::new("host_resource_mismatch"))?;
        if resource.key != task.id || resource.provider.raw() != task.provider.0 {
            return Err(WebError::new("host_resource_mismatch"));
        }
        for view in &task.views {
            if !views.insert(view.0)
                || snapshot
                    .views
                    .get(&core::ViewId::from_raw(view.0))
                    .is_none_or(|view| view.resource != resource.id)
            {
                return Err(WebError::new("host_view_mismatch"));
            }
        }
    }
    if snapshot
        .views
        .values()
        .any(|view| !views.contains(&view.id.raw()))
    {
        return Err(WebError::new("host_view_mismatch"));
    }
    Ok(())
}
