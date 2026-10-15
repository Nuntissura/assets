//! Two small actual providers for examples; not application-engine integrations.
use crate::*;
use kumihimo_content as content;
use kumihimo_core::{self as core, EntityId, Envelope, Scalar};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub struct Demo {
    pub controller: Controller,
    pub host: DemoHost,
    pub root: WorkspaceId,
    pub text_view: ViewId,
    pub canvas_view: ViewId,
    pub tool: ToolId,
}
#[derive(Clone, Debug)]
pub struct ProviderReceipt {
    pub view: ViewId,
    pub resource: ResourceId,
    pub request: u64,
    pub kind: &'static str,
    pub local_point: Option<[f32; 2]>,
    pub close_approved: Option<bool>,
}
struct Document {
    text: String,
    canvas: bool,
    texture: Option<egui::TextureHandle>,
    offset: f64,
    revision: u64,
    clicked: Option<egui::Pos2>,
}
pub struct DemoHost {
    #[cfg(feature = "drops")]
    pub drop_observe: bool,
    #[cfg(feature = "drops")]
    pub drop_enabled: bool,
    #[cfg(feature = "drops")]
    pub drop_status: String,
    #[cfg(feature = "drops")]
    pub drop_preview: Option<content::DropDestination>,
    #[cfg(feature = "drops")]
    drop_undo: VecDeque<(ResourceId, String, String)>,
    #[cfg(feature = "drops")]
    drop_sequence: u64,
    pub policy: HostPolicy,
    pub native_actions: Vec<AdapterAction>,
    pub receipts: VecDeque<ProviderReceipt>,
    pub mounts: BTreeMap<ViewId, content::MountId>,
    documents: BTreeMap<ResourceId, Document>,
    geometries: BTreeMap<ViewId, content::Geometry>,
    last_draw: BTreeMap<ViewId, RenderSlot>,
    pub now_ms: u64,
    delayed: BTreeSet<ViewId>,
    animated: BTreeSet<ViewId>,
    surface_observations: BTreeMap<WorkspaceId, (bool, bool, content::Observation<bool>)>,
    #[cfg(feature = "backgrounds")]
    backgrounds: BTreeMap<FrameId, content::MountId>,
    #[cfg(feature = "backgrounds")]
    background_texture: Option<egui::TextureHandle>,
}
impl Default for DemoHost {
    fn default() -> Self {
        Self {
            #[cfg(feature = "drops")]
            drop_observe: false,
            #[cfg(feature = "drops")]
            drop_enabled: false,
            #[cfg(feature = "drops")]
            drop_status: String::new(),
            #[cfg(feature = "drops")]
            drop_preview: None,
            #[cfg(feature = "drops")]
            drop_undo: VecDeque::new(),
            #[cfg(feature = "drops")]
            drop_sequence: 0,
            policy: HostPolicy::default(),
            native_actions: vec![],
            receipts: VecDeque::new(),
            mounts: BTreeMap::new(),
            documents: BTreeMap::new(),
            geometries: BTreeMap::new(),
            last_draw: BTreeMap::new(),
            now_ms: 0,
            delayed: BTreeSet::new(),
            animated: BTreeSet::new(),
            surface_observations: BTreeMap::new(),
            #[cfg(feature = "backgrounds")]
            backgrounds: BTreeMap::new(),
            #[cfg(feature = "backgrounds")]
            background_texture: None,
        }
    }
}
impl DemoHost {
    #[cfg(feature = "drops")]
    pub fn document_text(&self, resource: ResourceId) -> Option<&str> {
        self.documents.get(&resource).filter(|d| !d.canvas).map(|d| d.text.as_str())
    }
    #[cfg(feature = "drops")]
    pub fn undo_last_file_drop(&mut self) -> Result<()> {
        let (resource, before, after) = self.drop_undo.back().ok_or(AdapterError::Unsupported("no file drop undo"))?;
        let doc = self.documents.get_mut(resource).ok_or(AdapterError::Stale("drop undo document"))?;
        if doc.text != *after { return Err(AdapterError::Stale("document changed after file import")); }
        doc.text = before.clone();
        self.drop_undo.pop_back();
        self.drop_status = "Last file import undone by sample host.".into();
        Ok(())
    }
    #[cfg(all(feature = "native-drops", windows))]
    pub fn handle_native_drop(&mut self, controller: &mut Controller, surface: SurfaceBinding, report: &FrameReport, event: crate::native_drop::NativeDropEvent) -> Result<()> {
        use crate::{drop::{destination, HostDropPoint}, native_drop::NativeDropKind};
        use std::io::Read;
        if !self.drop_enabled { return Err(AdapterError::Unsupported("native drops not enabled")); }
        if matches!(event.kind, NativeDropKind::Leave) { self.drop_preview = None; self.drop_status = "File drop cancelled.".into(); return Ok(()); }
        if let NativeDropKind::Rejected(cause) = event.kind { self.drop_preview = None; self.drop_status = format!("File drop rejected: {cause}"); return Ok(()); }
        let result = (|| -> Result<()> {
            if event.epoch != surface.epoch || event.hwnd == 0 { return Err(AdapterError::Stale("native drop surface")); }
            let pixels = event.client_pixels.ok_or(AdapterError::Unsupported("drop point outside client"))?;
            let target = destination(controller, report, HostDropPoint { point: egui::pos2(surface.bounds.x() as f32 + pixels[0] as f32 / surface.pixels_per_point, surface.bounds.y() as f32 + pixels[1] as f32 / surface.pixels_per_point), epoch: event.epoch })?;
            let doc = self.documents.get(&target.scope.resource).ok_or(AdapterError::Stale("drop document"))?;
            if doc.canvas || self.delayed.contains(&target.scope.target.view().ok_or(AdapterError::Stale("drop view"))?) { return Err(AdapterError::Unsupported("drop text host unavailable")); }
            if matches!(event.kind, NativeDropKind::Hover) {
                self.drop_status = format!("Import text · frame {} · view {} · max 8 files / 8 KiB · source retained", target.frame.raw(), target.scope.target.raw());
                self.drop_preview = Some(target);
                return Ok(());
            }
            let NativeDropKind::Drop(paths) = event.kind else { return Ok(()); };
            if self.drop_preview.take().is_none_or(|preview| preview != target) { return Err(AdapterError::Stale("native drop preview")); }
            if paths.is_empty() || paths.len() > 8 { return Err(AdapterError::Budget("drop file count")); }
            let mut total = 0_u64;
            let mut items = Vec::with_capacity(paths.len());
            for path in &paths {
                let label = path.file_name().and_then(|v| v.to_str()).ok_or(AdapterError::Unsupported("drop filename encoding"))?;
                if !matches!(path.extension().and_then(|v| v.to_str()), Some("txt" | "md")) { return Err(AdapterError::Unsupported("drop text format")); }
                let metadata = std::fs::metadata(path).map_err(|_| AdapterError::Host("drop file metadata refused".into()))?;
                if !metadata.is_file() { return Err(AdapterError::Unsupported("drop non-file")); }
                total = total.checked_add(metadata.len()).ok_or(AdapterError::Budget("drop byte arithmetic"))?;
                if total > 8192 { return Err(AdapterError::Budget("drop payload bytes")); }
                items.push(content::DropItem { label: label.into(), media_type: "text/plain".into(), bytes: Some(metadata.len()), asset: None });
            }
            self.drop_sequence = self.drop_sequence.checked_add(1).ok_or(AdapterError::Budget("drop sequence"))?;
            let offer = content::DropOffer { kind: content::DropPayloadKind::DiskFiles, token: format!("native-drop-{}", self.drop_sequence), revision: 1, expires_at_ms: self.now_ms.checked_add(30_000).ok_or(AdapterError::Budget("drop lifetime"))?, items };
            let revision = controller.inspect().mounts[&target.scope.mount].eligibility.revision;
            let grant = content::DropGrant { permission: content::HostPermission { allowed: true, eligibility_revision: revision }, actions: BTreeSet::from([content::DropAction::Import]), kinds: vec![content::DropPayloadKind::DiskFiles] };
            let limits = content::DropLimits { items: 8, metadata_bytes: 4096, payload_bytes: 8192 };
            self.pump(controller, target.scope.mount)?;
            let mut coordinator = content::DropCoordinator::default();
            coordinator.hover(controller, content::DropRequest { offer: offer.clone(), destination: target.clone(), action: content::DropAction::Import }, &grant, limits, self.now_ms)?;
            let id = coordinator.release(controller, &offer, &grant, limits, self.now_ms)?;
            let request = controller.dispatch(target.scope.mount, grant.permission, self.now_ms)?.ok_or(AdapterError::Stale("drop prerequisites pending"))?;
            if request.ticket.id != id { return Err(AdapterError::Stale("drop dispatch ordering")); }
            // Host content access starts only after admitted dispatch. Read all files before mutation.
            let applied = (|| -> Result<()> {
                let mut contents = Vec::with_capacity(paths.len());
                for (path, item) in paths.iter().zip(&offer.items) {
                    let file = std::fs::File::open(path).map_err(|_| AdapterError::Host("drop open refused".into()))?;
                    let mut bytes = Vec::new();
                    file.take(8193).read_to_end(&mut bytes).map_err(|_| AdapterError::Host("drop read failed".into()))?;
                    if bytes.len() > 8192 || Some(bytes.len() as u64) != item.bytes { return Err(AdapterError::Stale("drop size changed")); }
                    contents.push(String::from_utf8(bytes).map_err(|_| AdapterError::Unsupported("drop UTF-8 text"))?);
                }
                let text = contents.join("\n");
                let doc = self.documents.get_mut(&target.scope.resource).ok_or(AdapterError::Stale("drop document"))?;
                let candidate = format!("{}\n{}", doc.text, text);
                if candidate.len() > 65_536 { return Err(AdapterError::Budget("drop document bytes")); }
                self.drop_undo.push_back((target.scope.resource, doc.text.clone(), candidate.clone()));
                while self.drop_undo.len() > 16 { self.drop_undo.pop_front(); }
                doc.text = candidate;
                self.drop_status = format!("Import completed · view {} · {} bytes · source retained", target.scope.target.raw(), total);
                Ok(())
            })();
            let ack = if applied.is_ok() { content::AckResult::Confirmed(content::Confirmation::Done) } else { content::AckResult::Failed { ambiguous: false } };
            controller.ack(content::Ack { ticket: request.ticket, result: ack }, self.now_ms)?;
            self.receipts.push_back(ProviderReceipt { view: target.scope.target.view().unwrap(), resource: target.scope.resource, request: id.0, kind: if applied.is_ok() { "file-drop" } else { "file-drop-failed" }, local_point: None, close_approved: None });
            while self.receipts.len() > self.policy.max_pending { self.receipts.pop_front(); }
            applied
        })();
        if let Err(error) = result { self.drop_preview = None; self.drop_status = format!("File drop rejected: {error}. Existing document retained."); }
        Ok(())
    }
    fn prune_draw_bindings(&mut self, controller: &Controller) {
        let content = controller.inspect();
        let live = |view: &ViewId| {
            controller
                .snapshot()
                .views
                .get(view)
                .is_some_and(|v| v.closed.is_none())
                && self
                    .mounts
                    .get(view)
                    .and_then(|id| content.mounts.get(id))
                    .is_some_and(|m| m.active && m.status == content::MountStatus::Attached)
        };
        self.last_draw.retain(|view, _| live(view));
        self.geometries.retain(|view, _| live(view));
        self.delayed.retain(|view| live(view));
        self.animated.retain(|view| live(view));
    }
    /// Caller supplies observed native facts or explicitly labelled injected facts.
    pub fn set_surface_observation(
        &mut self,
        controller: &mut Controller,
        surface: SurfaceBinding,
        visible: bool,
        minimized: bool,
        occlusion: content::Observation<bool>,
    ) -> Result<()> {
        if !controller.snapshot().workspaces.contains_key(&surface.root) {
            return Err(AdapterError::Stale("surface observation root"));
        }
        if !self.surface_observations.contains_key(&surface.root)
            && self.surface_observations.len() >= self.policy.max_regions
        {
            return Err(AdapterError::Budget("surface observations"));
        }
        self.surface_observations
            .insert(surface.root, (visible, minimized, occlusion));
        let visible_views = if visible && !minimized {
            self.last_draw.keys().copied().collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        self.content_visibility(controller, surface, &visible_views)
    }
    pub fn delay_provider(&mut self, view: ViewId, delayed: bool) -> Result<()> {
        if !self.mounts.contains_key(&view) {
            return Err(AdapterError::Stale("delayed provider view"));
        }
        if delayed {
            self.delayed.insert(view);
        } else {
            self.delayed.remove(&view);
        }
        Ok(())
    }
    pub fn animate_provider(&mut self, view: ViewId, animated: bool) -> Result<()> {
        if !self.mounts.contains_key(&view) {
            return Err(AdapterError::Stale("animated provider view"));
        }
        if animated {
            self.animated.insert(view);
        } else {
            self.animated.remove(&view);
        }
        Ok(())
    }
    fn surface_facts(
        &self,
        controller: &Controller,
        workspace: WorkspaceId,
    ) -> (bool, bool, content::Observation<bool>) {
        let mut root = workspace;
        while let Some(owner) = controller.snapshot().workspaces[&root].owner {
            root = controller.snapshot().frames[&owner].workspace;
        }
        self.surface_observations.get(&root).cloned().unwrap_or((
            true,
            false,
            content::Observation::Unknown,
        ))
    }
    /// Provider-owned opaque mark, painted after the actual document/canvas content.
    pub fn provider_mark(&self, slot: &RenderSlot) -> Result<(egui::Pos2, egui::Color32)> {
        let doc = self
            .documents
            .get(&slot.resource)
            .ok_or(AdapterError::Stale("mark resource"))?;
        let clip = to_egui(slot.clip);
        let mut point = egui::pos2(
            ((slot.bounds.x() + slot.bounds.width() - 18.0) as f32)
                .clamp(clip.left(), clip.right()),
            ((slot.bounds.y() + 18.0) as f32).clamp(clip.top(), clip.bottom()),
        );
        if (point.x as f64) < slot.clip.x() {
            point.x = (slot.clip.x() as f32).next_up();
        }
        if (point.x as f64) > slot.clip.right() {
            point.x = (slot.clip.right() as f32).next_down();
        }
        if (point.y as f64) < slot.clip.y() {
            point.y = (slot.clip.y() as f32).next_up();
        }
        if (point.y as f64) > slot.clip.bottom() {
            point.y = (slot.clip.bottom() as f32).next_down();
        }
        slot.local_point(point)?;
        Ok((
            point,
            if doc.canvas {
                egui::Color32::from_rgb(30, 240, 70)
            } else {
                egui::Color32::from_rgb(240, 40, 210)
            },
        ))
    }
    pub fn permission(&self) -> content::HostPermission {
        content::HostPermission {
            allowed: true,
            eligibility_revision: 0,
        }
    }
    pub fn pump(&mut self, controller: &mut Controller, mount: content::MountId) -> Result<()> {
        if controller
            .inspect()
            .mounts
            .get(&mount)
            .and_then(|m| m.scope.target.view())
            .is_some_and(|v| self.delayed.contains(&v))
        {
            return Ok(());
        }
        for _ in 0..self.policy.max_pending {
            let revision = controller
                .inspect()
                .mounts
                .get(&mount)
                .ok_or(AdapterError::Stale("provider mount missing"))?
                .eligibility
                .revision;
            let permission = content::HostPermission {
                allowed: true,
                eligibility_revision: revision,
            };
            let Some(request) = controller.dispatch(mount, permission, self.now_ms)? else {
                return Ok(());
            };
            let scope = request.ticket.scope.clone();
            let mut local = None;
            let mut close_approved = None;
            let (kind, result) = match request.kind {
                content::RequestKind::Attach { .. } => (
                    "attach",
                    content::AckResult::Confirmed(content::Confirmation::Attached {
                        strength: content::RestorationStrength::ProviderDocumentState,
                    }),
                ),
                content::RequestKind::Geometry(g) => (
                    "geometry",
                    content::AckResult::Confirmed(content::Confirmation::Geometry {
                        anchor: g.anchor,
                    }),
                ),
                content::RequestKind::Input(input) => {
                    if let (Some(x), Some(y), Some(view)) = (input.x, input.y, scope.target.view())
                    {
                        let g = self
                            .geometries
                            .get(&view)
                            .ok_or(AdapterError::Stale("provider geometry"))?;
                        let point = egui::pos2(
                            (x.get() - g.rect.x()) as f32,
                            (y.get() - g.rect.y()) as f32,
                        );
                        local = Some([point.x, point.y]);
                        self.documents
                            .get_mut(&scope.resource)
                            .ok_or(AdapterError::Invalid("demo resource"))?
                            .clicked = Some(point);
                    }
                    (
                        "input",
                        content::AckResult::Confirmed(content::Confirmation::Done),
                    )
                }
                content::RequestKind::ToolAction {
                    target, payload, ..
                } => {
                    if target.resource != scope.resource || scope.target.view() != Some(target.view)
                    {
                        return Err(AdapterError::Invalid("tool recipient"));
                    }
                    let doc = self
                        .documents
                        .get_mut(&scope.resource)
                        .ok_or(AdapterError::Invalid("tool document"))?;
                    if payload == "demo-append" || payload == "activate" {
                        if doc.text.len() + 22 > 65_536 {
                            return Err(AdapterError::Budget("demo document bytes"));
                        }
                        doc.text.push_str("\nTool action received.");
                    }
                    (
                        "tool",
                        content::AckResult::Confirmed(content::Confirmation::Done),
                    )
                }
                #[cfg(feature = "navigation")]
                content::RequestKind::Navigation(n) => {
                    let doc = self
                        .documents
                        .get_mut(&scope.resource)
                        .ok_or(AdapterError::Invalid("navigation document"))?;
                    let offset = match n.action {
                        content::NavigationAction::AbsoluteScroll(v) => v.get(),
                        content::NavigationAction::SemanticJump { target, .. } => {
                            if target == "end" {
                                900.0
                            } else {
                                0.0
                            }
                        }
                    };
                    doc.offset = offset.clamp(0.0, 900.0);
                    doc.revision = doc
                        .revision
                        .checked_add(1)
                        .ok_or(AdapterError::Budget("navigation revision"))?;
                    (
                        "navigation",
                        content::AckResult::Confirmed(content::Confirmation::Navigation(
                            content::NavigationObservation {
                                model_revision: n.model_revision,
                                revision: doc.revision,
                                offset: Scalar::new(doc.offset)?,
                            },
                        )),
                    )
                }
                content::RequestKind::Lifecycle(state) => (
                    "lifecycle",
                    content::AckResult::Confirmed(content::Confirmation::Lifecycle(state)),
                ),
                content::RequestKind::CloseDecision { .. } => {
                    let view = scope
                        .target
                        .view()
                        .ok_or(AdapterError::Invalid("close target is not a view"))?;
                    let record = controller
                        .snapshot()
                        .views
                        .get(&view)
                        .ok_or(AdapterError::Stale("close view missing"))?;
                    if record.resource != scope.resource || record.closed.is_some() {
                        return Err(AdapterError::Stale("close canonical recipient"));
                    }
                    let approved = !record.unsaved && !record.pinned;
                    close_approved = Some(approved);
                    (
                        "close",
                        content::AckResult::Confirmed(content::Confirmation::Close { approved }),
                    )
                }
                content::RequestKind::Focus(_)
                | content::RequestKind::Presentation(_)
                | content::RequestKind::Invalidate => (
                    "presentation",
                    content::AckResult::Confirmed(content::Confirmation::Done),
                ),
                content::RequestKind::Animation { .. } => (
                    "animation",
                    content::AckResult::Confirmed(content::Confirmation::Done),
                ),
                _ => ("unsupported", content::AckResult::Unsupported),
            };
            if let Some(view) = scope.target.view() {
                self.receipts.push_back(ProviderReceipt {
                    view,
                    resource: scope.resource,
                    request: request.ticket.id.0,
                    kind,
                    local_point: local,
                    close_approved,
                });
                while self.receipts.len() > self.policy.max_pending {
                    self.receipts.pop_front();
                }
            }
            controller.ack(
                content::Ack {
                    ticket: request.ticket,
                    result,
                },
                self.now_ms,
            )?;
        }
        Err(AdapterError::Budget("provider dispatch work"))
    }
    fn send(
        &mut self,
        controller: &mut Controller,
        mount: content::MountId,
        kind: content::RequestKind,
    ) -> Result<()> {
        let revision = controller
            .inspect()
            .mounts
            .get(&mount)
            .ok_or(AdapterError::Stale("provider mount missing"))?
            .eligibility
            .revision;
        controller.request(
            mount,
            kind,
            content::HostPermission {
                allowed: true,
                eligibility_revision: revision,
            },
            self.now_ms,
        )?;
        self.pump(controller, mount)
    }
    pub fn synchronize(&mut self, controller: &mut Controller, slot: &RenderSlot) -> Result<()> {
        self.prune_draw_bindings(controller);
        let current_view = controller
            .snapshot()
            .views
            .get(&slot.view)
            .ok_or(AdapterError::Stale("draw view missing"))?;
        if current_view.closed.is_some()
            || current_view.frame != slot.frame
            || current_view.resource != slot.resource
            || controller
                .snapshot()
                .frames
                .get(&slot.frame)
                .is_none_or(|f| f.workspace != slot.workspace)
            || slot.epoch == 0
        {
            return Err(AdapterError::Stale("draw canonical association/epoch"));
        }
        if !self.last_draw.contains_key(&slot.view)
            && self.last_draw.len() >= self.policy.max_regions
        {
            return Err(AdapterError::Budget("demo last-draw bindings"));
        }
        if self
            .mounts
            .get(&slot.view)
            .and_then(|id| controller.inspect().mounts.get(id).map(|m| m.status))
            .is_none_or(|s| s == content::MountStatus::Retired)
        {
            let mount = controller.attach(
                slot.view,
                content::AttachOptions {
                    mode: content::ContentMode::FullEditor,
                    ..Default::default()
                },
                self.permission(),
                self.now_ms,
            )?;
            self.mounts.insert(slot.view, mount);
            self.pump(controller, mount)?;
        }
        let mount = *self
            .mounts
            .get(&slot.view)
            .ok_or(AdapterError::Invalid("demo mount"))?;
        let geometry = content::Geometry {
            rect: slot.bounds,
            clip: slot.clip,
            scale: Scalar::new(slot.scale as f64)?,
            anchor: None,
        };
        let current = controller
            .inspect()
            .mounts
            .get(&mount)
            .and_then(|m| m.geometry.clone());
        if current.as_ref() != Some(&geometry) {
            self.geometries.insert(slot.view, geometry.clone());
            self.send(controller, mount, content::RequestKind::Geometry(geometry))?;
        }
        let (surface_visible, minimized, occlusion) =
            self.surface_facts(controller, slot.workspace);
        let signals = content::Signals {
            selected: true,
            visible: slot.visible && surface_visible && !minimized,
            focused: slot.focused && surface_visible && !minimized,
            minimized,
            occlusion,
        };
        if controller.inspect().mounts[&mount].signals != signals {
            self.send(
                controller,
                mount,
                content::RequestKind::Presentation(signals),
            )?;
        }
        self.pump(controller, mount)?;
        if surface_visible && !minimized {
            self.last_draw.insert(slot.view, slot.clone());
        } else {
            self.last_draw.remove(&slot.view);
        }
        Ok(())
    }
    #[cfg(feature = "navigation")]
    pub fn navigation_mode(
        &mut self,
        controller: &mut Controller,
        mode: content::NavigationMode,
    ) -> Result<()> {
        for mount in self.mounts.values() {
            let scope = controller.inspect().mounts[mount].scope.clone();
            let revision = controller.inspect().navigation[mount]
                .settings_revision
                .checked_add(1)
                .ok_or(AdapterError::Budget("settings revision"))?;
            let mut layers = content::SettingsLayers::<content::NavigationSettingsPatch>::default();
            layers.host_profile.mode = Some(mode);
            layers.host_profile.profile = Some(content::ControlProfile::Comfortable);
            controller.configure_navigation(&scope, revision, layers, Default::default())?;
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "drops"))]
mod file_drop_undo_contracts {
    use super::*;

    fn imported_document() -> (DemoHost, ResourceId) {
        let resource = ResourceId::from_raw(1);
        let mut host = DemoHost::default();
        host.documents.insert(resource, Document {
            text: "original\nimported".into(), canvas: false, texture: None,
            offset: 0.0, revision: 1, clicked: None,
        });
        host.drop_undo.push_back((resource, "original".into(), "original\nimported".into()));
        (host, resource)
    }

    #[test]
    fn normal_undo_restores_original_and_consumes_history() {
        let (mut host, resource) = imported_document();
        host.undo_last_file_drop().expect("unchanged imported text can undo");
        assert_eq!(host.document_text(resource), Some("original"));
        assert!(host.drop_undo.is_empty());
    }

    #[test]
    fn manual_edit_rejects_undo_and_preserves_text_and_history() {
        let (mut host, resource) = imported_document();
        host.documents.get_mut(&resource).unwrap().text.push_str("\nmanual edit");
        let history = host.drop_undo.clone();
        assert!(matches!(host.undo_last_file_drop(), Err(AdapterError::Stale("document changed after file import"))));
        assert_eq!(host.document_text(resource), Some("original\nimported\nmanual edit"));
        assert_eq!(host.drop_undo, history);
    }
}
impl DesktopHost for DemoHost {
    fn policy(&self) -> HostPolicy {
        self.policy.clone()
    }
    fn dispatch(&mut self, controller: &mut Controller, mut command: Command) -> Result<Response> {
        if let Command::CloseView {
            view,
            approved: true,
        } = command
        {
            if controller
                .snapshot()
                .views
                .get(&view)
                .is_some_and(|v| v.closed.is_none())
            {
                let mount = *self
                    .mounts
                    .get(&view)
                    .ok_or(AdapterError::Stale("close provider mount missing"))?;
                let state = controller.inspect();
                let binding = state
                    .mounts
                    .get(&mount)
                    .ok_or(AdapterError::Stale("close provider mount missing"))?;
                if !binding.active
                    || binding.status != content::MountStatus::Attached
                    || binding.scope.target.view() != Some(view)
                    || binding.scope.resource != controller.snapshot().views[&view].resource
                {
                    return Err(AdapterError::Stale("close provider binding"));
                }
                let permission = content::HostPermission {
                    allowed: true,
                    eligibility_revision: binding.eligibility.revision,
                };
                let request = controller.request(
                    mount,
                    content::RequestKind::CloseDecision {
                        eligibility_revision: binding.eligibility.revision,
                    },
                    permission,
                    self.now_ms,
                )?;
                self.pump(controller, mount)?;
                let outcome = controller
                    .inspect()
                    .receipts
                    .into_iter()
                    .find(|r| r.ticket.id == request && r.ticket.scope.mount == mount)
                    .and_then(|r| r.result);
                match outcome {
                    Some(content::AckResult::Confirmed(content::Confirmation::Close {
                        approved,
                    })) => {
                        command = Command::CloseView { view, approved };
                    }
                    _ => return Err(AdapterError::Stale("close decision not confirmed")),
                }
            }
        }
        let envelope = Envelope {
            identity: controller.snapshot().identity.clone(),
            actor: "desktop-demo-host".into(),
            operation: controller
                .core()
                .ledger_status()
                .high_watermark
                .checked_next()
                .ok_or(AdapterError::Budget("host operation identity"))?,
            expected_generation: controller.snapshot().generation,
            command,
        };
        Ok(controller.execute(envelope)?)
    }
    fn paint_content(
        &mut self,
        ui: &mut egui::Ui,
        slot: &RenderSlot,
        controller: &mut Controller,
    ) -> Result<()> {
        #[cfg(feature = "drops")]
        if self.drop_enabled {
            if let Some(destination) = &self.drop_preview {
                if destination.scope.target.view() == Some(slot.view) && destination.epoch == slot.epoch && destination.generation == controller.snapshot().generation {
                    ui.painter().rect_stroke(to_egui(slot.clip), 0.0, egui::Stroke::new(3.0, egui::Color32::LIGHT_BLUE), egui::StrokeKind::Inside);
                }
            }
        }
        self.synchronize(controller, slot)?;
        let (visible, minimized, occlusion) = self.surface_facts(controller, slot.workspace);
        if !visible || minimized {
            return Ok(());
        }
        if self.delayed.contains(&slot.view) {
            ui.label("Demo provider work delayed; other panes remain responsive.");
            return Ok(());
        }
        let animate =
            self.animated.contains(&slot.view) && occlusion != content::Observation::Known(true);
        if animate {
            self.send(
                controller,
                self.mounts[&slot.view],
                content::RequestKind::Animation {
                    deadline: Some(
                        self.now_ms
                            .checked_add(33)
                            .ok_or(AdapterError::Budget("animation deadline"))?,
                    ),
                },
            )?;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }
        if self
            .documents
            .get(&slot.resource)
            .is_some_and(|d| d.canvas && d.texture.is_none())
        {
            let retained = self
                .documents
                .values()
                .filter(|d| d.texture.is_some())
                .count()
                .checked_add(1)
                .and_then(|n| n.checked_mul(128 * 128 * 4))
                .ok_or(AdapterError::Budget("demo texture arithmetic"))?;
            if retained > self.policy.max_capture_bytes {
                return Err(AdapterError::Budget("demo retained texture bytes"));
            }
        }
        #[cfg(feature = "backgrounds")]
        if let Some(mount) = self.backgrounds.get(&slot.frame) {
            let state = controller.inspect();
            if let Some(b) = state.backgrounds.get(mount) {
                let settings = b
                    .settings
                    .effective
                    .as_ref()
                    .ok_or(AdapterError::Unsupported("background composition"))?;
                let texture = self.background_texture.get_or_insert_with(|| {
                    ui.ctx().load_texture(
                        "demo-still-background",
                        generated_image(64, true),
                        egui::TextureOptions::LINEAR,
                    )
                });
                let rect = to_egui(slot.bounds);
                let (dest, uv) = background_fit(rect, settings.fit.value);
                let brightness = (settings.brightness.value.get() * 255.0).clamp(0.0, 255.0) as u8;
                ui.painter().image(
                    texture.id(),
                    dest,
                    uv,
                    egui::Color32::from_rgba_unmultiplied(
                        brightness,
                        brightness,
                        brightness,
                        (settings.opacity.value.get() * 255.0) as u8,
                    ),
                );
                ui.painter().rect_filled(
                    rect,
                    0.0,
                    egui::Color32::from_black_alpha(
                        (settings.overlay_opacity.value.get() * 255.0) as u8,
                    ),
                );
            }
        }
        let inner = to_egui(slot.bounds).shrink(10.0);
        let mut body = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        body.set_clip_rect(to_egui(slot.clip).intersect(inner));
        #[cfg(feature = "drops")]
        if self.drop_enabled {
            body.label(&self.drop_status);
            if body.add_enabled(!self.drop_undo.is_empty(), egui::Button::new("Undo last file import")).clicked() {
                if let Err(error) = self.undo_last_file_drop() { self.drop_status = format!("Import undo rejected: {error:?}"); }
            }
        }
        let doc = self
            .documents
            .get_mut(&slot.resource)
            .ok_or(AdapterError::Invalid("demo resource"))?;
        body.label(
            egui::RichText::new(if doc.canvas {
                "Generated canvas demo"
            } else {
                "Editable document demo"
            })
            .strong()
            .color(egui::Color32::WHITE),
        );
        body.label(format!(
            "View {} / resource {} · host-owned provider",
            slot.view.raw(),
            slot.resource.raw()
        ));
        body.label(format!("Observed document position {:.0}", doc.offset));
        let mut edited = false;
        if doc.canvas {
            let texture = doc.texture.get_or_insert_with(|| {
                body.ctx().load_texture(
                    format!("demo-canvas-{}", slot.resource.raw()),
                    generated_image(128, false),
                    egui::TextureOptions::LINEAR,
                )
            });
            let side = (inner.width() - 32.0)
                .min((inner.height() - 80.0).max(1.0))
                .max(1.0);
            let uv = egui::Rect::from_min_max(
                egui::pos2(0.0, (doc.offset / 1000.0) as f32),
                egui::pos2(1.0, ((doc.offset + 100.0) / 1000.0) as f32),
            );
            let response = body.add(
                egui::Image::new((texture.id(), egui::vec2(side, side)))
                    .uv(uv)
                    .sense(egui::Sense::click()),
            );
            if let Some(point) = doc.clicked {
                body.label(format!(
                    "Provider-local click {:.1}, {:.1}",
                    point.x, point.y
                ));
            }
            if response.clicked() {
                if let Some(point) = response.interact_pointer_pos() {
                    self.content_input(
                        controller,
                        slot,
                        content::Input {
                            owner: content::InputOwner::Content,
                            payload: "canvas-click".into(),
                            x: Some(Scalar::new(point.x as f64)?),
                            y: Some(Scalar::new(point.y as f64)?),
                        },
                    )?;
                }
            }
        } else {
            let output = egui::ScrollArea::vertical()
                .id_salt(("demo-document-scroll", slot.view.raw()))
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .max_height(100.0)
                .min_scrolled_height(100.0)
                .vertical_scroll_offset(doc.offset as f32)
                .show(&mut body, |ui| {
                    ui.set_min_height(1000.0);
                    ui.add(
                        egui::TextEdit::multiline(&mut doc.text)
                            .id(egui::Id::new(("demo-document", slot.view.raw())))
                            .desired_width((inner.width() - 30.0).max(1.0))
                            .desired_rows(6)
                            .char_limit(16_384),
                    )
                    .changed()
                });
            edited = output.inner;
            #[cfg(feature = "navigation")]
            if (output.state.offset.y as f64 - doc.offset).abs() > 0.01 {
                doc.offset = (output.state.offset.y as f64).clamp(0.0, 900.0);
                doc.revision = doc
                    .revision
                    .checked_add(1)
                    .ok_or(AdapterError::Budget("document observation revision"))?;
                let scope = controller.inspect().mounts[&self.mounts[&slot.view]]
                    .scope
                    .clone();
                controller.observe_navigation(
                    &scope,
                    content::NavigationObservation {
                        model_revision: 1,
                        revision: doc.revision,
                        offset: Scalar::new(doc.offset)?,
                    },
                )?;
            }
        }
        let (mark, color) = self.provider_mark(slot)?;
        ui.painter().rect_filled(
            egui::Rect::from_center_size(mark, egui::vec2(12.0, 12.0)),
            0.0,
            color,
        );
        if animate {
            let time = ui.input(|i| i.time);
            let width = (slot.bounds.width() - 32.0).max(1.0) as f32;
            let x = slot.bounds.x() as f32 + 16.0 + ((time * 80.0) as f32).rem_euclid(width);
            ui.painter().circle_filled(
                egui::pos2(x, (slot.bounds.y() + slot.bounds.height() - 16.0) as f32),
                5.0,
                color,
            );
        }
        if edited {
            self.content_input(
                controller,
                slot,
                content::Input {
                    owner: content::InputOwner::Content,
                    payload: "document-edited".into(),
                    x: None,
                    y: None,
                },
            )?;
        }
        Ok(())
    }
    fn content_input(
        &mut self,
        controller: &mut Controller,
        slot: &RenderSlot,
        input: content::Input,
    ) -> Result<()> {
        let current_view = controller
            .snapshot()
            .views
            .get(&slot.view)
            .ok_or(AdapterError::Stale("input view missing"))?;
        if current_view.closed.is_some()
            || current_view.frame != slot.frame
            || current_view.resource != slot.resource
            || controller
                .snapshot()
                .frames
                .get(&slot.frame)
                .is_none_or(|f| f.workspace != slot.workspace)
        {
            return Err(AdapterError::Stale("input canonical association"));
        }
        let draw = self
            .last_draw
            .get(&slot.view)
            .ok_or(AdapterError::Stale("input before current draw"))?;
        if draw.epoch != slot.epoch
            || draw.view != slot.view
            || draw.resource != slot.resource
            || draw.frame != slot.frame
            || draw.workspace != slot.workspace
            || draw.bounds != slot.bounds
            || draw.clip != slot.clip
            || draw.scale != slot.scale
            || !draw.visible
        {
            return Err(AdapterError::Stale("input draw binding/geometry/epoch"));
        }
        if let (Some(x), Some(y)) = (input.x, input.y) {
            slot.local_point(egui::pos2(x.get() as f32, y.get() as f32))?;
        }
        let mount = *self
            .mounts
            .get(&slot.view)
            .ok_or(AdapterError::Invalid("input mount"))?;
        if controller.inspect().mounts.get(&mount).is_none_or(|m| {
            !m.active
                || m.status != content::MountStatus::Attached
                || !m.signals.visible
                || m.scope.target.view() != Some(slot.view)
                || m.scope.resource != slot.resource
        }) {
            return Err(AdapterError::Stale(
                "input mount no longer visible/attached",
            ));
        }
        self.send(controller, mount, content::RequestKind::Input(input))
    }
    fn tool_action(
        &mut self,
        controller: &mut Controller,
        tool: ToolId,
        payload: &str,
    ) -> Result<()> {
        let target = controller
            .snapshot()
            .tools
            .get(&tool)
            .ok_or(AdapterError::Invalid("tool"))?
            .target;
        let mount = *self
            .mounts
            .get(&target.view)
            .ok_or(AdapterError::Invalid("tool mount"))?;
        self.send(
            controller,
            mount,
            content::RequestKind::ToolAction {
                tool,
                target,
                payload: payload.into(),
            },
        )
    }
    fn native_action(&mut self, _controller: &mut Controller, action: AdapterAction) -> Result<()> {
        if self.native_actions.len() >= self.policy.max_pending {
            return Err(AdapterError::Budget("native action queue"));
        }
        self.native_actions.push(action);
        Ok(())
    }
    fn content_visibility(
        &mut self,
        controller: &mut Controller,
        surface: SurfaceBinding,
        visible: &[ViewId],
    ) -> Result<()> {
        self.prune_draw_bindings(controller);
        let (surface_visible, minimized, occlusion) = self
            .surface_observations
            .get(&surface.root)
            .cloned()
            .unwrap_or((true, false, content::Observation::Unknown));
        for (view, mount) in self.mounts.clone() {
            let Some(record) = controller.snapshot().views.get(&view) else {
                continue;
            };
            let mut workspace = controller.snapshot().frames[&record.frame].workspace;
            while let Some(owner) = controller.snapshot().workspaces[&workspace].owner {
                workspace = controller.snapshot().frames[&owner].workspace;
            }
            if workspace != surface.root {
                continue;
            }
            let selected = controller.snapshot().frames[&record.frame].selected_view == Some(view)
                && record.closed.is_none();
            let signals = content::Signals {
                selected,
                visible: surface_visible
                    && !minimized
                    && visible.contains(&view)
                    && record.closed.is_none(),
                focused: controller
                    .inspect()
                    .mounts
                    .get(&mount)
                    .is_some_and(|m| m.signals.focused)
                    && surface_visible
                    && !minimized
                    && visible.contains(&view),
                minimized,
                occlusion: occlusion.clone(),
            };
            if !signals.visible {
                self.last_draw.remove(&view);
            }
            if controller
                .inspect()
                .mounts
                .get(&mount)
                .is_some_and(|m| m.status == content::MountStatus::Attached && m.signals != signals)
            {
                self.send(
                    controller,
                    mount,
                    content::RequestKind::Presentation(signals),
                )?;
            }
        }
        Ok(())
    }
    #[cfg(feature = "navigation")]
    fn navigation_permission(
        &self,
        _controller: &Controller,
        _scope: &content::Scope,
    ) -> content::HostPermission {
        self.permission()
    }
    #[cfg(feature = "navigation")]
    fn navigation_pending(&mut self, controller: &mut Controller) -> Result<()> {
        for mount in self.mounts.values().copied().collect::<Vec<_>>() {
            self.pump(controller, mount)?;
        }
        Ok(())
    }
}
pub fn generated_image(side: usize, background: bool) -> egui::ColorImage {
    let pixels = (0..side * side)
        .map(|i| {
            let x = i % side;
            let y = i / side;
            if background {
                egui::Color32::from_rgb((25 + x * 50 / side) as u8, (35 + y * 60 / side) as u8, 85)
            } else {
                egui::Color32::from_rgb(
                    (x * 255 / side) as u8,
                    (y * 255 / side) as u8,
                    if (x / 16 + y / 16) % 2 == 0 { 210 } else { 80 },
                )
            }
        })
        .collect();
    egui::ColorImage::new([side, side], pixels)
}
#[cfg(feature = "backgrounds")]
fn background_fit(rect: egui::Rect, fit: content::BackgroundFit) -> (egui::Rect, egui::Rect) {
    let mut uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    match fit {
        content::BackgroundFit::Contain => {
            let side = rect.width().min(rect.height());
            (
                egui::Rect::from_center_size(rect.center(), egui::vec2(side, side)),
                uv,
            )
        }
        content::BackgroundFit::Cover => {
            if rect.width() > rect.height() {
                let h = rect.height() / rect.width();
                uv.min.y = (1.0 - h) * 0.5;
                uv.max.y = 1.0 - uv.min.y;
            } else {
                let w = rect.width() / rect.height().max(1.0);
                uv.min.x = (1.0 - w) * 0.5;
                uv.max.x = 1.0 - uv.min.x;
            }
            (rect, uv)
        }
        content::BackgroundFit::Native => (
            egui::Rect::from_min_size(rect.min, egui::vec2(64.0, 64.0)),
            uv,
        ),
        content::BackgroundFit::Stretch => (rect, uv),
    }
}
fn created(host: &mut DemoHost, controller: &mut Controller, command: Command) -> Result<EntityId> {
    match host.dispatch(controller, command)?.outcome.result? {
        core::Change::Created(id) => Ok(id),
        _ => Err(AdapterError::Invalid("demo creation response")),
    }
}
pub fn new() -> Result<Demo> {
    with_counts(3, 3)
}
pub fn with_counts(frame_count: usize, tabs_per_frame: usize) -> Result<Demo> {
    with_policy_counts(frame_count, tabs_per_frame, HostPolicy::default())
}
pub fn with_policy_counts(
    frame_count: usize,
    tabs_per_frame: usize,
    policy: HostPolicy,
) -> Result<Demo> {
    with_policy_counts_and_text_frames(frame_count, tabs_per_frame, policy, 1)
}
/// Opt-in file-drop fixture: two independently backed editable destinations.
#[cfg(feature = "drops")]
pub fn file_drop_sample() -> Result<Demo> {
    with_policy_counts_and_text_frames(2, 2, HostPolicy::default(), 2)
}
fn with_policy_counts_and_text_frames(
    frame_count: usize,
    tabs_per_frame: usize,
    policy: HostPolicy,
    text_frames: usize,
) -> Result<Demo> {
    let count = frame_count
        .checked_mul(tabs_per_frame)
        .ok_or(AdapterError::Budget("demo provider count arithmetic"))?;
    if frame_count < 2
        || tabs_per_frame == 0
        || count > policy.max_regions
        || frame_count > policy.max_regions
        || policy.max_pending == 0
    {
        return Err(AdapterError::Budget("demo host provider budget"));
    }
    let core = core::Core::new(
        core::Identity::new("desktop-demo-layout", "desktop-demo-core-1")?,
        Default::default(),
        128,
    )?;
    let live_count = count
        .checked_add(frame_count)
        .ok_or(AdapterError::Budget("demo live provider arithmetic"))?;
    let limits = content::ContentLimits {
        mounts: Some(live_count),
        instances: Some(live_count),
        pending: Some(policy.max_pending),
        queue: Some(policy.max_pending),
        receipts: Some(policy.max_pending),
        ..Default::default()
    };
    let mut controller = Controller::new(core, "desktop-demo-session-1", limits)?;
    let mut host = DemoHost {
        policy,
        ..Default::default()
    };
    let EntityId::Workspace(root) = created(
        &mut host,
        &mut controller,
        Command::CreateWorkspace { owner: None },
    )?
    else {
        return Err(AdapterError::Invalid("root"));
    };
    let mut providers = vec![];
    for key in ["editable-document-demo", "generated-canvas-demo"] {
        let EntityId::Provider(provider) = created(
            &mut host,
            &mut controller,
            Command::RegisterProvider {
                key: key.into(),
                supports_shared_views: true,
            },
        )?
        else {
            return Err(AdapterError::Invalid("provider"));
        };
        let mut caps = content::Capabilities::default();
        caps.close_mode = content::CloseMode::ProviderVeto;
        #[cfg(feature = "backgrounds")]
        {
            caps.backgrounds = content::BackgroundCapabilities {
                image: true,
                video: false,
                compositing: true,
                fits: std::collections::BTreeSet::from([
                    content::BackgroundFit::Cover,
                    content::BackgroundFit::Contain,
                    content::BackgroundFit::Stretch,
                    content::BackgroundFit::Native,
                ]),
                poster: false,
                audio: false,
                host_contrast: false,
                media_budget: false,
            };
        }
        controller.negotiate(provider, content::ProtocolVersion::V2, caps.clone(), caps)?;
        providers.push(provider);
    }
    let mut frames = vec![];
    let mut views = vec![];
    for index in 0..frame_count {
        let EntityId::Frame(frame) = created(
            &mut host,
            &mut controller,
            Command::CreateFrame {
                workspace: root,
                index,
            },
        )?
        else {
            return Err(AdapterError::Invalid("frame"));
        };
        frames.push(frame);
    }
    let EntityId::Workspace(nested) = created(
        &mut host,
        &mut controller,
        Command::CreateWorkspace {
            owner: Some(frames[1]),
        },
    )?
    else {
        return Err(AdapterError::Invalid("child workspace"));
    };
    let EntityId::Frame(child) = created(
        &mut host,
        &mut controller,
        Command::CreateFrame {
            workspace: nested,
            index: 0,
        },
    )?
    else {
        return Err(AdapterError::Invalid("child frame"));
    };
    for (index, frame) in frames.iter().copied().enumerate() {
        let target = if index == 1 { child } else { frame };
        for tab in 0..tabs_per_frame {
            let canvas = index >= text_frames;
            let EntityId::Resource(resource) = created(
                &mut host,
                &mut controller,
                Command::RegisterResource {
                    provider: providers[usize::from(canvas)],
                    key: format!("demo-{index}-{tab}"),
                    restore_reference: format!("demo-restore-{index}-{tab}"),
                },
            )?
            else {
                return Err(AdapterError::Invalid("resource"));
            };
            let EntityId::View(view) = created(
                &mut host,
                &mut controller,
                Command::CreateView {
                    frame: target,
                    resource,
                    index: tab,
                },
            )?
            else {
                return Err(AdapterError::Invalid("view"));
            };
            host.dispatch(
                &mut controller,
                Command::SetViewMetadata {
                    view,
                    label: if text_frames > 1 && !canvas {
                        format!("Import target {} · document {}", index + 1, tab + 1)
                    } else {
                        format!("{} {}", if canvas { "Canvas" } else { "Document" }, tab + 1)
                    },
                    pinned: tab == 1,
                    unsaved: tab == 2,
                },
            )?
            .outcome
            .result?;
            host.documents.insert(resource, Document { text: "Kumihimo editable document demo.\n\nType here. Frame controls stay outside this provider.\nThe pinned toolbar targets this document even when another pane has focus.".into(), canvas, texture: None, offset: 0.0, revision: 1, clicked: None });
            let mount = controller.attach(
                view,
                content::AttachOptions {
                    mode: content::ContentMode::FullEditor,
                    ..Default::default()
                },
                host.permission(),
                0,
            )?;
            host.mounts.insert(view, mount);
            host.pump(&mut controller, mount)?;
            #[cfg(feature = "navigation")]
            {
                let scope = controller.inspect().mounts[&mount].scope.clone();
                controller.update_navigation(
                    &scope,
                    content::DocumentModel {
                        revision: 1,
                        semantic: "demo-document-position".into(),
                        unit: "demo-lines".into(),
                        extent: Scalar::new(1000.0)?,
                        viewport: Scalar::new(100.0)?,
                    },
                    content::NavigationObservation {
                        model_revision: 1,
                        revision: 1,
                        offset: Scalar::new(0.0)?,
                    },
                )?;
                controller.update_overview(
                    &scope,
                    content::Overview {
                        revision: 1,
                        model_revision: 1,
                        unit: "demo-lines".into(),
                        markers: vec![
                            content::OverviewMarker {
                                id: "intro".into(),
                                start: Scalar::new(0.0)?,
                                end: Scalar::new(100.0)?,
                                jump_target: Some("start".into()),
                                label: "Introduction".into(),
                                style: "heading".into(),
                            },
                            content::OverviewMarker {
                                id: "end".into(),
                                start: Scalar::new(900.0)?,
                                end: Scalar::new(1000.0)?,
                                jump_target: Some("end".into()),
                                label: "End".into(),
                                style: "heading".into(),
                            },
                        ],
                    },
                )?;
            }
            views.push(view);
        }
        #[cfg(feature = "backgrounds")]
        {
            let resource = controller.snapshot().views[&views[views.len() - 1]].resource;
            let mount = controller.attach_background(
                content::MountTarget::FrameBackground(target),
                resource,
                content::BackgroundKind::Image,
                Default::default(),
                host.permission(),
                0,
            )?;
            host.pump(&mut controller, mount)?;
            host.backgrounds.insert(target, mount);
            let scope = controller.inspect().mounts[&mount].scope.clone();
            let mut layers = content::SettingsLayers::<content::BackgroundSettingsPatch>::default();
            layers.host_profile.brightness = Some(Scalar::new(0.7)?);
            layers.host_profile.opacity = Some(Scalar::new(0.8)?);
            layers.host_profile.overlay_opacity = Some(Scalar::new(0.35)?);
            controller.background_settings(scope, layers, Default::default(), 0)?;
        }
    }
    let text_view = views[0];
    let canvas_view = views[tabs_per_frame];
    // Core validates a tool's owning workspace context during CreateTool.
    let mut context = core::GeometryContext::new(Rect::new(0.0, 0.0, 960.0, 640.0)?);
    context.gap = Scalar::new(6.0)?;
    context.allow_floating = true;
    context.allowed_docks = frames
        .iter()
        .flat_map(|frame| {
            [
                core::Edge::Left,
                core::Edge::Right,
                core::Edge::Top,
                core::Edge::Bottom,
            ]
            .map(|edge| core::DockLocation::Beside {
                frame: *frame,
                edge,
            })
        })
        .collect();
    context
        .allowed_docks
        .push(core::DockLocation::Append { workspace: root });
    host.dispatch(
        &mut controller,
        Command::SetGeometryContext {
            workspace: root,
            context,
        },
    )?
    .outcome
    .result?;
    let resource = controller.snapshot().views[&text_view].resource;
    let placement = core::ToolPlacement::Edge {
        anchor: core::ToolAnchor::View(text_view),
        edge: core::Edge::Top,
    };
    let EntityId::Tool(tool) = created(
        &mut host,
        &mut controller,
        Command::CreateTool(core::ToolSpec {
            owner: providers[0],
            origin: text_view,
            target: core::EditingTarget {
                view: text_view,
                resource,
            },
            placement,
            policy: core::ToolPolicy::new(
                core::Size::new(120.0, 28.0)?,
                core::Size::new(40.0, 24.0)?,
                vec![
                    placement,
                    core::ToolPlacement::Edge {
                        anchor: core::ToolAnchor::Workspace(root),
                        edge: core::Edge::Bottom,
                    },
                ],
            ),
            state: core::TargetState {
                target_visible: true,
                target_usable: true,
                origin_visible: true,
                origin_usable: true,
                applicable: true,
                keyboard_focused: false,
            },
        }),
    )?
    else {
        return Err(AdapterError::Invalid("tool"));
    };
    let mut nodes = BTreeMap::new();
    let mut cells = vec![];
    for (i, frame) in frames.iter().enumerate() {
        let node = core::NodeId(i as u64 + 2);
        nodes.insert(
            node,
            core::LayoutNode::Leaf(core::LayoutItem::Frame(*frame)),
        );
        cells.push(core::Cell {
            row: i / 2,
            column: i % 2,
            node,
        });
    }
    nodes.insert(
        core::NodeId(1),
        core::LayoutNode::Grid {
            rows: vec![Scalar::new(1.0)?; frame_count.div_ceil(2)],
            columns: vec![Scalar::new(1.0)?; 2],
            cells,
        },
    );
    host.dispatch(
        &mut controller,
        Command::SetLayout {
            workspace: root,
            plan: core::LayoutPlan {
                root: Some(core::NodeId(1)),
                nodes,
            },
        },
    )?
    .outcome
    .result?;
    if frames.len() > 2 {
        host.dispatch(
            &mut controller,
            Command::Dock(core::DockIntent::Float {
                frame: frames[2],
                rect: Rect::new(570.0, 320.0, 330.0, 250.0)?,
            }),
        )?
        .outcome
        .result?;
        host.policy.float_order.push(frames[2]);
    }
    Ok(Demo {
        controller,
        host,
        root,
        text_view,
        canvas_view,
        tool,
    })
}
