//! Two small actual providers for examples; not application-engine integrations.
use crate::*;
use kumihimo_content as content;
use kumihimo_core::{self as core, EntityId, Envelope, Scalar};
use std::collections::{BTreeMap, VecDeque};

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
    pub policy: HostPolicy,
    pub native_actions: Vec<AdapterAction>,
    pub receipts: VecDeque<ProviderReceipt>,
    pub mounts: BTreeMap<ViewId, content::MountId>,
    documents: BTreeMap<ResourceId, Document>,
    geometries: BTreeMap<ViewId, content::Geometry>,
    last_draw: BTreeMap<ViewId, RenderSlot>,
    pub now_ms: u64,
    #[cfg(feature = "backgrounds")]
    backgrounds: BTreeMap<FrameId, content::MountId>,
    #[cfg(feature = "backgrounds")]
    background_texture: Option<egui::TextureHandle>,
}
impl Default for DemoHost {
    fn default() -> Self {
        Self {
            policy: HostPolicy::default(),
            native_actions: vec![],
            receipts: VecDeque::new(),
            mounts: BTreeMap::new(),
            documents: BTreeMap::new(),
            geometries: BTreeMap::new(),
            last_draw: BTreeMap::new(),
            now_ms: 0,
            #[cfg(feature = "backgrounds")]
            backgrounds: BTreeMap::new(),
            #[cfg(feature = "backgrounds")]
            background_texture: None,
        }
    }
}
impl DemoHost {
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
    }
    pub fn permission(&self) -> content::HostPermission {
        content::HostPermission {
            allowed: true,
            eligibility_revision: 0,
        }
    }
    pub fn pump(&mut self, controller: &mut Controller, mount: content::MountId) -> Result<()> {
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
                | content::RequestKind::Invalidate
                | content::RequestKind::Animation { .. } => (
                    "presentation",
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
        let signals = content::Signals {
            selected: true,
            visible: slot.visible,
            focused: slot.focused,
            minimized: false,
            occlusion: content::Observation::Unknown,
        };
        if controller.inspect().mounts[&mount].signals != signals {
            self.send(
                controller,
                mount,
                content::RequestKind::Presentation(signals),
            )?;
        }
        self.pump(controller, mount)?;
        self.last_draw.insert(slot.view, slot.clone());
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
        self.synchronize(controller, slot)?;
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
        let doc = self
            .documents
            .get_mut(&slot.resource)
            .ok_or(AdapterError::Invalid("demo resource"))?;
        let inner = to_egui(slot.bounds).shrink(10.0);
        let mut body = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        body.set_clip_rect(to_egui(slot.clip).intersect(inner));
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
                visible: visible.contains(&view) && record.closed.is_none(),
                focused: controller
                    .inspect()
                    .mounts
                    .get(&mount)
                    .is_some_and(|m| m.signals.focused)
                    && visible.contains(&view),
                minimized: false,
                occlusion: content::Observation::Unknown,
            };
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
            let canvas = index % 2 == 1 || index > 1;
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
                    label: format!("{} {}", if canvas { "Canvas" } else { "Document" }, tab + 1),
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
