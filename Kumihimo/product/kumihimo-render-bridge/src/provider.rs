//! Canonical host dispatcher and optional composed provider experiment.
use crate::{
    frame::{
        BridgeLimits, ContentScope, FrameBinding, FrameRequest, FrameStore, ScopeIdentity,
        ScopeTarget,
    },
    gpu::{self, Scene3d, TextureUpload},
};
use kumihimo_content as content;
use kumihimo_core::{
    self as core, Command, EntityId, FrameId, Rect, ResourceId, Scalar, ViewId, WorkspaceId,
};
use kumihimo_egui::{
    capture::OffscreenRenderer, AdapterAction, AdapterError, DesktopAdapter, DesktopHost,
    HostPolicy, RenderSlot, Result, SurfaceBinding,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Component, Path, PathBuf},
};

fn host_error(e: impl std::fmt::Display) -> AdapterError {
    AdapterError::Host(e.to_string())
}
fn bridge<T>(result: crate::frame::Result<T>) -> Result<T> {
    result.map_err(host_error)
}

#[derive(Clone, Debug, Deserialize)]
pub struct HostCaptureReceipt {
    pub issued_ms: u64,
    pub received_ms: u64,
    pub request_sha256: String,
    pub png_sha256: String,
    pub evidence_ref: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct CaptureSpec {
    pub version: u16,
    pub binding: FrameBinding,
    pub request_id: u64,
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
}
impl From<&FrameRequest> for CaptureSpec {
    fn from(r: &FrameRequest) -> Self {
        Self {
            version: r.version,
            binding: r.binding.clone(),
            request_id: r.request_id,
            sequence: r.sequence,
            width: r.width,
            height: r.height,
        }
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct BrowserCapture {
    pub case_name: String,
    pub view_key: String,
    pub request: FrameRequest,
    pub png_path: PathBuf,
    pub png_sha256: String,
    pub width: u32,
    pub height: u32,
    pub issued_ms: u64,
    pub received_ms: u64,
    pub host_receipt: HostCaptureReceipt,
    pub engine: serde_json::Value,
    pub dom: serde_json::Value,
}
#[derive(Deserialize)]
pub struct BrowserInputManifest {
    pub protocol: String,
    pub browsers: Vec<BrowserCapture>,
    pub unload_request_id: String,
    pub unload_receipt: serde_json::Value,
}
fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(host_error)?;
    if file.metadata().map_err(host_error)?.len() > limit as u64 {
        return Err(AdapterError::Budget("input file bytes"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(host_error)?;
    if bytes.len() > limit {
        return Err(AdapterError::Budget("input file changed past budget"));
    }
    Ok(bytes)
}
pub fn load_browser_manifest(path: &Path, limits: &BridgeLimits) -> Result<BrowserInputManifest> {
    let manifest: BrowserInputManifest =
        serde_json::from_slice(&read_bounded(path, limits.max_message_bytes)?)
            .map_err(host_error)?;
    if manifest.protocol != "kumihimo.servo-batch.v1" || manifest.browsers.len() > 32 {
        return Err(AdapterError::Invalid(
            "engine manifest protocol/capture count",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for capture in &manifest.browsers {
        if !seen.insert((capture.case_name.clone(), capture.view_key.clone()))
            || capture.width != capture.request.width
            || capture.height != capture.request.height
        {
            return Err(AdapterError::Invalid(
                "duplicate capture/receipt dimensions",
            ));
        }
        bridge(capture.request.validate(limits))?;
        if capture.png_path.is_absolute()
            || capture
                .png_path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(AdapterError::Invalid(
                "engine PNG path must stay beneath manifest root",
            ));
        }
    }
    Ok(manifest)
}

#[derive(Clone, Debug, Serialize)]
pub struct InputReceipt {
    pub view: u64,
    pub provider: String,
    pub local_dip: [f32; 2],
    pub normalized: [f32; 2],
    pub hit: bool,
}
enum Provider {
    Browser {
        key: String,
        upload: Option<TextureUpload>,
        engine_incarnation: u64,
        loaded: bool,
    },
    Vector {
        selected: bool,
    },
    Scene {
        selected: bool,
        gpu: Option<Scene3d>,
    },
}
pub struct BridgeDesktopHost {
    pub policy: HostPolicy,
    pub limits: BridgeLimits,
    pub mounts: BTreeMap<ViewId, content::MountId>,
    pub slots: BTreeMap<ViewId, RenderSlot>,
    pub frames: FrameStore,
    pub dirty: crate::scheduler::DirtyScheduler,
    providers: BTreeMap<ResourceId, Provider>,
    pub renderer_binding: Option<kumihimo_egui::content::GpuBinding>,
    pub input_receipts: Vec<InputReceipt>,
    pub texture_uploads: u64,
    pub pending_browser_inputs: Vec<serde_json::Value>,
}
impl BridgeDesktopHost {
    pub fn new(limits: BridgeLimits) -> Result<Self> {
        let frames = bridge(FrameStore::new(limits.clone()))?;
        let dirty = bridge(crate::scheduler::DirtyScheduler::new(&limits))?;
        Ok(Self {
            policy: HostPolicy {
                max_regions: limits.max_host_frames,
                max_pending: 32,
                ..Default::default()
            },
            limits,
            mounts: BTreeMap::new(),
            slots: BTreeMap::new(),
            frames,
            dirty,
            providers: BTreeMap::new(),
            renderer_binding: None,
            input_receipts: vec![],
            texture_uploads: 0,
            pending_browser_inputs: vec![],
        })
    }
    fn pump(
        &mut self,
        controller: &mut content::Controller,
        mount: content::MountId,
    ) -> Result<()> {
        for _ in 0..self.policy.max_pending {
            let revision = controller
                .inspect()
                .mounts
                .get(&mount)
                .ok_or(AdapterError::Stale("provider mount"))?
                .eligibility
                .revision;
            let Some(request) = controller.dispatch(
                mount,
                content::HostPermission {
                    allowed: true,
                    eligibility_revision: revision,
                },
                0,
            )?
            else {
                return Ok(());
            };
            let result = match request.kind {
                content::RequestKind::Attach { .. } => {
                    content::AckResult::Confirmed(content::Confirmation::Attached {
                        strength: content::RestorationStrength::PageReload,
                    })
                }
                content::RequestKind::Geometry(g) => {
                    content::AckResult::Confirmed(content::Confirmation::Geometry {
                        anchor: g.anchor,
                    })
                }
                content::RequestKind::Input(_)
                | content::RequestKind::Focus(_)
                | content::RequestKind::Presentation(_)
                | content::RequestKind::Invalidate => {
                    content::AckResult::Confirmed(content::Confirmation::Done)
                }
                _ => content::AckResult::Unsupported,
            };
            controller.ack(
                content::Ack {
                    ticket: request.ticket,
                    result,
                },
                0,
            )?;
        }
        Err(AdapterError::Budget("bounded provider dispatcher"))
    }
    fn send(
        &mut self,
        controller: &mut content::Controller,
        mount: content::MountId,
        kind: content::RequestKind,
    ) -> Result<()> {
        let revision = controller.inspect().mounts[&mount].eligibility.revision;
        controller.request(
            mount,
            kind,
            content::HostPermission {
                allowed: true,
                eligibility_revision: revision,
            },
            0,
        )?;
        self.pump(controller, mount)
    }
    fn synchronize(
        &mut self,
        controller: &mut content::Controller,
        slot: &RenderSlot,
    ) -> Result<()> {
        let view = controller
            .snapshot()
            .views
            .get(&slot.view)
            .ok_or(AdapterError::Stale("slot view"))?;
        if view.closed.is_some()
            || view.frame != slot.frame
            || view.resource != slot.resource
            || controller.snapshot().frames[&slot.frame].workspace != slot.workspace
        {
            return Err(AdapterError::Stale("slot canonical ownership"));
        }
        if !self.mounts.contains_key(&slot.view) {
            let mount = controller.attach(
                slot.view,
                content::AttachOptions::default(),
                content::HostPermission {
                    allowed: true,
                    eligibility_revision: 0,
                },
                0,
            )?;
            self.mounts.insert(slot.view, mount);
            self.pump(controller, mount)?;
        }
        let mount = self.mounts[&slot.view];
        let geometry = content::Geometry {
            rect: slot.bounds,
            clip: slot.clip,
            scale: Scalar::new(slot.scale as f64)?,
            anchor: None,
        };
        if controller.inspect().mounts[&mount].geometry.as_ref() != Some(&geometry) {
            self.send(controller, mount, content::RequestKind::Geometry(geometry))?;
        }
        if matches!(
            self.providers.get(&slot.resource),
            Some(Provider::Browser { .. })
        ) {
            let target = ScopeTarget::View(slot.view.raw());
            if let Some(old) = self.slots.get(&slot.view) {
                if old.bounds != slot.bounds || old.scale != slot.scale {
                    bridge(self.dirty.mark_dirty(target))?;
                }
            } else {
                bridge(self.dirty.register(target, slot.visible, true))?;
            }
            bridge(self.dirty.set_visible(target, slot.visible))?;
            let binding = self.frame_binding(controller, slot.view, slot.epoch)?;
            bridge(
                self.frames
                    .bind(binding, slot.physical_size[0], slot.physical_size[1]),
            )?;
        }
        self.slots.insert(slot.view, slot.clone());
        Ok(())
    }
    pub fn frame_binding(
        &self,
        controller: &content::Controller,
        view: ViewId,
        epoch: u64,
    ) -> Result<FrameBinding> {
        let state = controller.inspect();
        let mount = state
            .mounts
            .get(&self.mounts[&view])
            .ok_or(AdapterError::Stale("current scope"))?;
        let scope = &mount.scope;
        Ok(FrameBinding {
            scope: ContentScope {
                identity: ScopeIdentity {
                    lineage: scope.identity.lineage.clone(),
                    incarnation: scope.identity.incarnation.clone(),
                },
                session: scope.session.clone(),
                provider: scope.provider.raw(),
                resource: scope.resource.raw(),
                target: ScopeTarget::View(view.raw()),
                connection: scope.connection,
                instance: scope.instance.0,
                mount: scope.mount.0,
            },
            engine_incarnation: match self.providers.get(&scope.resource) {
                Some(Provider::Browser {
                    engine_incarnation, ..
                }) => *engine_incarnation,
                _ => 1,
            },
            geometry_revision: mount.confirmed_geometry,
            surface_epoch: epoch,
        })
    }
    pub fn prepare_gpu(
        &mut self,
        renderer: &mut OffscreenRenderer,
        surface: SurfaceBinding,
        recovery: bool,
    ) -> Result<()> {
        let binding = renderer.gpu_binding(surface)?;
        self.renderer_binding = Some(binding.clone());
        let (device, queue, egui_renderer) = renderer.gpu_resources();
        for (view, slot) in &self.slots {
            let provider = self
                .providers
                .get_mut(&slot.resource)
                .ok_or(AdapterError::Invalid("slot provider"))?;
            match provider {
                Provider::Browser { upload, loaded, .. } => {
                    if !*loaded {
                        continue;
                    }
                    let frame = if recovery {
                        bridge(self.frames.recovery_frame(ScopeTarget::View(view.raw())))?.captured
                    } else {
                        match self.frames.current_frame(ScopeTarget::View(view.raw())) {
                            Ok(f) => f,
                            Err(_) => continue,
                        }
                    };
                    let same = upload.as_ref().is_some_and(|u| {
                        u.binding == binding
                            && u.sequence == frame.request.sequence
                            && u.rgba_sha256 == gpu::sha256(&frame.rgba)
                    });
                    if !same {
                        let next = TextureUpload::new(
                            device,
                            queue,
                            egui_renderer,
                            binding.clone(),
                            frame,
                            &self.limits,
                        )?;
                        if !bridge(self.dirty.record_upload(
                            ScopeTarget::View(view.raw()),
                            frame.request.request_id,
                            surface.epoch,
                        ))? {
                            return Err(AdapterError::Stale("duplicate scheduler texture upload"));
                        }
                        if let Some(old) = upload.replace(next) {
                            egui_renderer.free_texture(&old.texture_id);
                        }
                        self.texture_uploads += 1;
                    }
                }
                Provider::Scene { selected, gpu } => {
                    if gpu
                        .as_ref()
                        .is_none_or(|g| g.binding != binding || g.size != slot.physical_size)
                    {
                        *gpu = Some(Scene3d::new(
                            device,
                            queue,
                            binding.clone(),
                            slot.physical_size,
                            *selected,
                        )?);
                    } else if let Some(scene) = gpu {
                        scene.set_selected(queue, *selected);
                    }
                }
                Provider::Vector { .. } => {}
            }
        }
        Ok(())
    }
    pub fn accept_capture(
        &mut self,
        capture: &BrowserCapture,
        root: &Path,
        expected: &FrameRequest,
        now_ms: u64,
    ) -> Result<()> {
        if &capture.request != expected {
            return Err(AdapterError::Stale("engine complete request mismatch"));
        }
        let receipt = &capture.host_receipt;
        let request_hash = gpu::sha256(
            &serde_json::to_vec(&serde_json::to_value(expected).map_err(host_error)?)
                .map_err(host_error)?,
        );
        if receipt.issued_ms == 0
            || receipt.received_ms < receipt.issued_ms
            || receipt.received_ms > now_ms
            || receipt.received_ms - receipt.issued_ms > 15_000
            || receipt.request_sha256 != request_hash
            || receipt.png_sha256 != capture.png_sha256
            || receipt.evidence_ref.is_empty()
            || expected.deadline_ms
                != receipt
                    .issued_ms
                    .checked_add(15_000)
                    .ok_or(AdapterError::Budget("capture deadline"))?
        {
            return Err(AdapterError::Invalid(
                "root host capture issue/receipt deadline",
            ));
        }
        bridge(self.frames.validate_response(expected, receipt.received_ms))?;
        let root = root.canonicalize().map_err(host_error)?;
        let path = root
            .join(&capture.png_path)
            .canonicalize()
            .map_err(host_error)?;
        if !path.starts_with(&root) {
            return Err(AdapterError::Invalid("engine PNG escapes manifest root"));
        }
        let bytes = read_bounded(&path, self.limits.max_png_bytes)?;
        let pixels =
            gpu::decode_engine_png(&bytes, expected.clone(), &capture.png_sha256, &self.limits)?;
        bridge(self.frames.accept(pixels, receipt.received_ms))?;
        bridge(
            self.dirty
                .finish_capture(expected.binding.scope.target, expected.request_id, true),
        )
    }
    pub fn scene_render_count(&self) -> u64 {
        self.providers
            .values()
            .filter_map(|p| {
                if let Provider::Scene { gpu: Some(g), .. } = p {
                    Some(g.render_count())
                } else {
                    None
                }
            })
            .sum()
    }
    fn confirm_engine_exit(
        &mut self,
        controller: &content::Controller,
        view: ViewId,
        original: &BrowserCapture,
        receipt: &serde_json::Value,
        operation_request_id: &str,
        now_ms: u64,
    ) -> Result<()> {
        let scope = self
            .frame_binding(controller, view, self.slots[&view].epoch)?
            .scope;
        if receipt["protocol"] != "kumihimo.independent-engine-exit.v1"
            || receipt["scope"] != serde_json::to_value(&scope).map_err(host_error)?
            || receipt["engine_incarnation"] != 1
            || operation_request_id.is_empty()
            || receipt["operation_request_id"].as_str() != Some(operation_request_id)
            || receipt["engine"] != original.engine
            || receipt["absence_confirmed"] != true
            || receipt["no_live_descendants"] != true
            || receipt["evidence_ref"].as_str().is_none_or(str::is_empty)
            || receipt["observed_unix_ms"]
                .as_u64()
                .is_none_or(|t| t < original.host_receipt.received_ms || t > now_ms)
        {
            return Err(AdapterError::Invalid(
                "independent root exact-engine exit receipt",
            ));
        }
        let resource = controller.snapshot().views[&view].resource;
        match self.providers.get_mut(&resource) {
            Some(Provider::Browser { loaded, .. }) => *loaded = false,
            _ => return Err(AdapterError::Invalid("unload browser provider")),
        };
        bridge(self.dirty.set_loaded(ScopeTarget::View(view.raw()), false))
    }
    fn restore_engine(&mut self, controller: &content::Controller, view: ViewId) -> Result<()> {
        let resource = controller.snapshot().views[&view].resource;
        match self.providers.get_mut(&resource) {
            Some(Provider::Browser {
                loaded,
                engine_incarnation,
                ..
            }) if !*loaded => {
                *engine_incarnation = engine_incarnation
                    .checked_add(1)
                    .ok_or(AdapterError::Budget("engine incarnation"))?;
                *loaded = true;
            }
            _ => {
                return Err(AdapterError::Invalid(
                    "restore requires confirmed unloaded browser",
                ))
            }
        };
        bridge(self.dirty.set_loaded(ScopeTarget::View(view.raw()), true))
    }
}
impl DesktopHost for BridgeDesktopHost {
    fn policy(&self) -> HostPolicy {
        self.policy.clone()
    }
    fn dispatch(
        &mut self,
        controller: &mut content::Controller,
        command: Command,
    ) -> Result<core::Response> {
        let operation = controller
            .core()
            .ledger_status()
            .high_watermark
            .checked_next()
            .ok_or(AdapterError::Budget("host operation"))?;
        Ok(controller.execute(core::Envelope {
            identity: controller.snapshot().identity.clone(),
            actor: "render-bridge-host".into(),
            operation,
            expected_generation: controller.snapshot().generation,
            command,
        })?)
    }
    fn paint_content(
        &mut self,
        ui: &mut egui::Ui,
        slot: &RenderSlot,
        controller: &mut content::Controller,
    ) -> Result<()> {
        self.synchronize(controller, slot)?;
        let rect = kumihimo_egui::to_egui(slot.bounds);
        let painter = ui
            .painter()
            .with_clip_rect(kumihimo_egui::to_egui(slot.clip));
        match self
            .providers
            .get(&slot.resource)
            .ok_or(AdapterError::Stale("content provider"))?
        {
            Provider::Browser {
                key,
                upload: Some(upload),
                loaded: true,
                ..
            } if self
                .frames
                .current_frame(ScopeTarget::View(slot.view.raw()))
                .is_ok_and(|frame| {
                    frame.request.sequence == upload.sequence
                        && frame.request.binding.surface_epoch == slot.epoch
                })
                || self
                    .frames
                    .recovery_frame(ScopeTarget::View(slot.view.raw()))
                    .is_ok_and(|frame| {
                        frame.captured.request.sequence == upload.sequence
                            && frame.current_binding.surface_epoch == slot.epoch
                    }) =>
            {
                let renderer = self
                    .renderer_binding
                    .clone()
                    .ok_or(AdapterError::Stale("renderer missing"))?;
                kumihimo_egui::content::paint_gpu_texture(
                    ui,
                    slot,
                    upload.binding.clone(),
                    renderer,
                    upload.texture_id,
                    false,
                )?;
                let _ = key;
            }
            Provider::Browser { key, loaded, .. } => {
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(32, 40, 55));
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!(
                        "{key}: {}\nPage reload available; freeze unsupported",
                        if *loaded {
                            "awaiting actual engine pixels"
                        } else {
                            "owned engine unloaded; logical view retained"
                        }
                    ),
                    egui::FontId::proportional(15.0),
                    egui::Color32::WHITE,
                );
            }
            Provider::Vector { selected } => {
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(15, 35, 46));
                let points = [
                    egui::pos2(
                        rect.left() + rect.width() * 0.2,
                        rect.top() + rect.height() * 0.7,
                    ),
                    egui::pos2(
                        rect.left() + rect.width() * 0.5,
                        rect.top() + rect.height() * 0.2,
                    ),
                    egui::pos2(
                        rect.left() + rect.width() * 0.8,
                        rect.top() + rect.height() * 0.7,
                    ),
                ];
                painter.add(egui::Shape::convex_polygon(
                    points.to_vec(),
                    if *selected {
                        egui::Color32::from_rgb(255, 80, 150)
                    } else {
                        egui::Color32::from_rgb(35, 180, 235)
                    },
                    egui::Stroke::new(3.0, egui::Color32::WHITE),
                ));
                painter.circle_filled(rect.center(), 7.0, egui::Color32::WHITE);
            }
            Provider::Scene {
                gpu: Some(scene), ..
            } => kumihimo_egui::content::paint_gpu_callback(
                ui,
                slot,
                scene.binding.clone(),
                self.renderer_binding
                    .clone()
                    .ok_or(AdapterError::Stale("renderer missing"))?,
                scene.callback(),
                false,
            )?,
            Provider::Scene { .. } => {
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(12, 20, 38));
            }
        }
        let response = ui.interact(
            rect,
            egui::Id::new(("bridge-provider", slot.view.raw())),
            egui::Sense::click(),
        );
        if response.clicked() {
            if let Some(point) = response.interact_pointer_pos() {
                self.content_input(
                    controller,
                    slot,
                    content::Input {
                        owner: content::InputOwner::Content,
                        payload: "pointer".into(),
                        x: Some(Scalar::new(point.x as f64)?),
                        y: Some(Scalar::new(point.y as f64)?),
                    },
                )?;
            }
        }
        Ok(())
    }
    fn content_input(
        &mut self,
        controller: &mut content::Controller,
        slot: &RenderSlot,
        input: content::Input,
    ) -> Result<()> {
        if input.owner != content::InputOwner::Content {
            return Err(AdapterError::Unsupported("provider input owner"));
        }
        let current = self
            .slots
            .get(&slot.view)
            .ok_or(AdapterError::Stale("input slot"))?;
        if current.epoch != slot.epoch
            || current.bounds != slot.bounds
            || current.resource != slot.resource
        {
            return Err(AdapterError::Stale("input current geometry"));
        }
        let (x, y) = (
            input.x.ok_or(AdapterError::Invalid("input x"))?,
            input.y.ok_or(AdapterError::Invalid("input y"))?,
        );
        let local = slot.local_point(egui::pos2(x.get() as f32, y.get() as f32))?;
        let normalized = [
            local.x / slot.bounds.width() as f32,
            local.y / slot.bounds.height() as f32,
        ];
        let browser_scope = self.frame_binding(controller, slot.view, slot.epoch)?.scope;
        let (kind, hit) = match self
            .providers
            .get_mut(&slot.resource)
            .ok_or(AdapterError::Stale("input provider"))?
        {
            Provider::Browser { key, .. } => {
                if self.pending_browser_inputs.len() >= self.policy.max_pending {
                    return Err(AdapterError::Budget("external browser inputs"));
                }
                // Engine DPR is pinned to one: host DIP * host scale is engine CSS px once.
                let point = bridge(crate::mapping::local_dip_to_engine_css(
                    [local.x as f64, local.y as f64],
                    slot.scale as f64,
                    slot.physical_size,
                ))?;
                self.pending_browser_inputs.push(serde_json::json!({"view_key":key,"scope":browser_scope,"engine_css_point":point,"payload":input.payload}));
                return Err(AdapterError::Unsupported("browser input queued for owned external WebDriver dispatcher; no execution claimed"));
            }
            Provider::Vector { selected } => {
                let hit = vector_pick(normalized);
                if hit {
                    *selected = !*selected;
                }
                ("vector-2d", hit)
            }
            Provider::Scene { selected, gpu } => {
                let hit = gpu.as_ref().is_some_and(|scene| scene.pick(normalized));
                if hit {
                    *selected = !*selected;
                }
                ("gpu-3d", hit)
            }
        };
        if self.input_receipts.len() >= 128 {
            self.input_receipts.remove(0);
        }
        self.input_receipts.push(InputReceipt {
            view: slot.view.raw(),
            provider: kind.into(),
            local_dip: [local.x, local.y],
            normalized,
            hit,
        });
        self.send(
            controller,
            self.mounts[&slot.view],
            content::RequestKind::Input(input),
        )
    }
    fn native_action(&mut self, _: &mut content::Controller, _: AdapterAction) -> Result<()> {
        Err(AdapterError::Unsupported("raw engine/GPU context native popout unsupported; source retained before canonical commit"))
    }
}

pub fn vector_pick(point: [f32; 2]) -> bool {
    if !point.iter().all(|v| v.is_finite()) {
        return false;
    }
    let cross = |a: [f32; 2], b: [f32; 2]| {
        (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0])
    };
    let signs = [
        cross([0.2, 0.7], [0.5, 0.2]),
        cross([0.5, 0.2], [0.8, 0.7]),
        cross([0.8, 0.7], [0.2, 0.7]),
    ];
    signs.iter().all(|v| *v >= 0.0) || signs.iter().all(|v| *v <= 0.0)
}

pub struct Composition {
    pub controller: content::Controller,
    pub host: BridgeDesktopHost,
    pub root: WorkspaceId,
    pub frames: [FrameId; 4],
    pub views: [ViewId; 4],
}
fn created(
    host: &mut BridgeDesktopHost,
    controller: &mut content::Controller,
    command: Command,
) -> Result<EntityId> {
    match host.dispatch(controller, command)?.outcome.result? {
        core::Change::Created(id) => Ok(id),
        _ => Err(AdapterError::Invalid("created entity")),
    }
}
pub fn new_composition() -> Result<Composition> {
    let mut controller = content::Controller::new(
        core::Core::new(
            core::Identity::new("render-bridge-layout", "render-bridge-core-1")?,
            Default::default(),
            128,
        )?,
        "render-bridge-session-1",
        content::ContentLimits {
            mounts: Some(4),
            instances: Some(4),
            pending: Some(32),
            queue: Some(32),
            ..Default::default()
        },
    )?;
    let mut host = BridgeDesktopHost::new(BridgeLimits::default())?;
    let EntityId::Workspace(root) = created(
        &mut host,
        &mut controller,
        Command::CreateWorkspace { owner: None },
    )?
    else {
        return Err(AdapterError::Invalid("root"));
    };
    let mut frames = vec![];
    let mut views = vec![];
    for (index, key) in ["browser-a", "browser-b", "vector-2d", "gpu-3d"]
        .into_iter()
        .enumerate()
    {
        let EntityId::Provider(provider) = created(
            &mut host,
            &mut controller,
            Command::RegisterProvider {
                key: key.into(),
                supports_shared_views: false,
            },
        )?
        else {
            return Err(AdapterError::Invalid("provider"));
        };
        let mut caps = content::Capabilities::default();
        caps.animation = false;
        caps.tools = false;
        caps.assets = false;
        caps.cache = false;
        caps.lifecycle = std::collections::BTreeSet::from([
            content::LifecycleState::Visible,
            content::LifecycleState::Background,
        ]);
        caps.restoration =
            std::collections::BTreeSet::from([content::RestorationStrength::PageReload]);
        controller.negotiate(provider, content::ProtocolVersion::V2, caps.clone(), caps)?;
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
        let EntityId::Resource(resource) = created(
            &mut host,
            &mut controller,
            Command::RegisterResource {
                provider,
                key: key.into(),
                restore_reference: format!("fixture:{key}"),
            },
        )?
        else {
            return Err(AdapterError::Invalid("resource"));
        };
        let EntityId::View(view) = created(
            &mut host,
            &mut controller,
            Command::CreateView {
                frame,
                resource,
                index: 0,
            },
        )?
        else {
            return Err(AdapterError::Invalid("view"));
        };
        host.dispatch(
            &mut controller,
            Command::SetViewMetadata {
                view,
                label: key.into(),
                pinned: false,
                unsaved: false,
            },
        )?
        .outcome
        .result?;
        host.providers.insert(
            resource,
            match index {
                0 | 1 => Provider::Browser {
                    key: key.into(),
                    upload: None,
                    engine_incarnation: 1,
                    loaded: true,
                },
                2 => Provider::Vector { selected: false },
                _ => Provider::Scene {
                    selected: false,
                    gpu: None,
                },
            },
        );
        frames.push(frame);
        views.push(view);
    }
    let mut nodes = BTreeMap::new();
    for (i, frame) in frames.iter().enumerate() {
        nodes.insert(
            core::NodeId(i as u64 + 2),
            core::LayoutNode::Leaf(core::LayoutItem::Frame(*frame)),
        );
    }
    nodes.insert(
        core::NodeId(1),
        core::LayoutNode::Grid {
            rows: vec![Scalar::new(1.0)?, Scalar::new(1.0)?],
            columns: vec![Scalar::new(3.0)?, Scalar::new(2.0)?],
            cells: (0..4)
                .map(|i| core::Cell {
                    row: i / 2,
                    column: i % 2,
                    node: core::NodeId(i as u64 + 2),
                })
                .collect(),
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
    Ok(Composition {
        controller,
        host,
        root,
        frames: frames
            .try_into()
            .map_err(|_| AdapterError::Invalid("frames"))?,
        views: views
            .try_into()
            .map_err(|_| AdapterError::Invalid("views"))?,
    })
}
#[derive(Clone, Copy)]
pub struct Case {
    pub name: &'static str,
    pub width: f64,
    pub height: f64,
    pub scale: f32,
    pub moved: bool,
}
pub const CASES: [Case; 4] = [
    Case {
        name: "normal",
        width: 1000.0,
        height: 720.0,
        scale: 1.0,
        moved: false,
    },
    Case {
        name: "narrow",
        width: 700.0,
        height: 640.0,
        scale: 1.0,
        moved: false,
    },
    Case {
        name: "scale-1-5",
        width: 1000.0,
        height: 720.0,
        scale: 1.5,
        moved: false,
    },
    Case {
        name: "moved-resized",
        width: 1000.0,
        height: 720.0,
        scale: 1.0,
        moved: true,
    },
];
fn surface(root: WorkspaceId, case: Case, epoch: u64) -> Result<SurfaceBinding> {
    Ok(SurfaceBinding {
        root,
        viewport: egui::ViewportId::ROOT,
        epoch,
        bounds: Rect::new(0.0, 0.0, case.width, case.height)?,
        pixels_per_point: case.scale,
    })
}
fn prepare_case(composition: &mut Composition, case: Case) -> Result<()> {
    if case.moved {
        // The canonical Controller owns both translation and resize. Other slot geometry stays unchanged.
        let mut context = composition.controller.snapshot().contexts[&composition.root].clone();
        context.allow_floating = true;
        context.allowed_docks.push(core::DockLocation::Append {
            workspace: composition.root,
        });
        composition
            .host
            .dispatch(
                &mut composition.controller,
                Command::SetGeometryContext {
                    workspace: composition.root,
                    context,
                },
            )?
            .outcome
            .result?;
        composition
            .host
            .dispatch(
                &mut composition.controller,
                Command::Dock(core::DockIntent::Float {
                    frame: composition.frames[0],
                    rect: Rect::new(35.0, 65.0, 460.0, 300.0)?,
                }),
            )?
            .outcome
            .result?;
    }
    Ok(())
}
fn draw(
    renderer: &mut OffscreenRenderer,
    adapter: &mut DesktopAdapter,
    composition: &mut Composition,
    surface: SurfaceBinding,
) -> Result<kumihimo_egui::capture::CapturedFrame> {
    renderer.capture(
        adapter,
        &mut composition.controller,
        surface,
        &mut composition.host,
        egui::RawInput::default(),
    )
}
pub fn plan_batch(path: &Path, now_ms: u64) -> Result<()> {
    let mut composition = new_composition()?;
    let mut renderer = OffscreenRenderer::new(64 * 1024 * 1024)?;
    let mut adapter = DesktopAdapter::default();
    let mut cases = vec![];
    for (index, case) in CASES.into_iter().enumerate() {
        prepare_case(&mut composition, case)?;
        let current_surface = surface(composition.root, case, 1)?;
        let frame = draw(
            &mut renderer,
            &mut adapter,
            &mut composition,
            current_surface,
        )?;
        if !frame.report.rejected.is_empty() {
            return Err(host_error(format!(
                "plan rejects: {:?}",
                frame.report.rejected
            )));
        }
        let mut browsers = vec![];
        for (i, key) in ["browser-a", "browser-b"].into_iter().enumerate() {
            let view = composition.views[i];
            let request = bridge(
                composition.host.frames.request(
                    ScopeTarget::View(view.raw()),
                    index as u64 * 2 + i as u64 + 1,
                    now_ms
                        .checked_add(15_000)
                        .ok_or(AdapterError::Budget("plan deadline"))?,
                    now_ms,
                ),
            )?;
            let slot = &composition.host.slots[&view];
            let point = bridge(crate::mapping::local_dip_to_engine_css(
                [slot.bounds.width() * 0.5, slot.bounds.height() * 0.5],
                slot.scale as f64,
                slot.physical_size,
            ))?;
            browsers.push(serde_json::json!({"view_key":key,"intent":CaptureSpec::from(&request),"width":slot.physical_size[0],"height":slot.physical_size[1],"slot_dip":{"x":slot.bounds.x(),"y":slot.bounds.y(),"width":slot.bounds.width(),"height":slot.bounds.height()},"clip_dip":{"x":slot.clip.x(),"y":slot.clip.y(),"width":slot.clip.width(),"height":slot.clip.height()},"scale":slot.scale,"engine_device_pixel_ratio":1,"input_css_point":point}));
            bridge(
                composition
                    .host
                    .frames
                    .cancel(ScopeTarget::View(view.raw()), request.request_id),
            )?;
        }
        cases.push(serde_json::json!({"name":case.name,"width":case.width,"height":case.height,"scale":case.scale,"browsers":browsers}));
    }
    let view = composition.views[0];
    let slot = &composition.host.slots[&view];
    let mut binding = composition
        .host
        .frame_binding(&composition.controller, view, 1)?;
    binding.engine_incarnation = 2;
    bridge(
        composition
            .host
            .frames
            .bind(binding, slot.physical_size[0], slot.physical_size[1]),
    )?;
    let request = bridge(
        composition.host.frames.request(
            ScopeTarget::View(view.raw()),
            9,
            now_ms
                .checked_add(15_000)
                .ok_or(AdapterError::Budget("plan deadline"))?,
            now_ms,
        ),
    )?;
    let reload_a = serde_json::json!({"view_key":"browser-a","intent":CaptureSpec::from(&request),"width":slot.physical_size[0],"height":slot.physical_size[1],"slot_dip":{"x":slot.bounds.x(),"y":slot.bounds.y(),"width":slot.bounds.width(),"height":slot.bounds.height()},"clip_dip":{"x":slot.clip.x(),"y":slot.clip.y(),"width":slot.clip.width(),"height":slot.clip.height()},"scale":slot.scale});
    write_json(
        path,
        &serde_json::json!({"protocol":"kumihimo.compose-plan.v1","cases":cases,"lifecycle":{"reload_a":reload_a},"capabilities":{"engine_frame_ready":false,"freeze":false,"native_context_transfer":false,"engine_dpr":1}}),
    )
}
fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(value).map_err(host_error)?).map_err(host_error)
}
fn sample_slot(image: &image::RgbaImage, slot: &RenderSlot, point: [f64; 2]) -> Result<[u8; 4]> {
    let x = ((slot.bounds.x() + slot.bounds.width() * point[0]) * slot.scale as f64).round() as u32;
    let y =
        ((slot.bounds.y() + slot.bounds.height() * point[1]) * slot.scale as f64).round() as u32;
    if x >= image.width() || y >= image.height() {
        return Err(AdapterError::Invalid("known point outside readback"));
    }
    Ok(image.get_pixel(x, y).0)
}
pub fn compose_batch(manifest_path: &Path, output_root: &Path, now_ms: u64) -> Result<()> {
    let limits = BridgeLimits::default();
    let manifest = load_browser_manifest(manifest_path, &limits)?;
    let input_root = manifest_path
        .parent()
        .ok_or(AdapterError::Invalid("manifest parent"))?;
    std::fs::create_dir_all(output_root).map_err(host_error)?;
    let mut composition = new_composition()?;
    let mut renderer = OffscreenRenderer::new(64 * 1024 * 1024)?;
    let mut adapter = DesktopAdapter::default();
    let mut diagnostics = vec![];
    for (index, case) in CASES.into_iter().enumerate() {
        prepare_case(&mut composition, case)?;
        let surface = surface(composition.root, case, 1)?;
        let _ = draw(&mut renderer, &mut adapter, &mut composition, surface)?;
        let mut provenance = vec![];
        for (i, key) in ["browser-a", "browser-b"].into_iter().enumerate() {
            let capture = manifest
                .browsers
                .iter()
                .find(|c| c.case_name == case.name && c.view_key == key)
                .ok_or(AdapterError::Invalid("missing actual engine capture"))?;
            let expected = bridge(composition.host.frames.request(
                ScopeTarget::View(composition.views[i].raw()),
                index as u64 * 2 + i as u64 + 1,
                capture.request.deadline_ms,
                capture.host_receipt.issued_ms,
            ))?;
            if !bridge(
                composition
                    .host
                    .dirty
                    .begin_capture(expected.binding.scope.target, expected.request_id),
            )? {
                return Err(AdapterError::Invalid("engine capture was not dirty"));
            }
            composition
                .host
                .accept_capture(capture, input_root, &expected, now_ms)?;
            provenance.push(serde_json::json!({"view_key":key,"request":expected,"png_sha256":capture.png_sha256,"rgba_sha256":gpu::sha256(&composition.host.frames.current_frame(expected.binding.scope.target).map_err(host_error)?.rgba),"engine":capture.engine,"dom":capture.dom}));
        }
        composition
            .host
            .prepare_gpu(&mut renderer, surface, false)?;
        let composed = draw(&mut renderer, &mut adapter, &mut composition, surface)?;
        let scene_pixel = sample_slot(
            &composed.image,
            &composition.host.slots[&composition.views[3]],
            [0.5, 0.5],
        )?;
        if scene_pixel[1] < 190 || scene_pixel[0] > 60 || scene_pixel[2] > 140 {
            return Err(AdapterError::Invalid(
                "near triangle did not win actual GPU depth readback",
            ));
        }
        composed.write(&output_root.join(case.name), 64 * 1024 * 1024)?;
        let before = composition.host.texture_uploads;
        let scene_before = composition.host.scene_render_count();
        composition
            .host
            .prepare_gpu(&mut renderer, surface, false)?;
        let static_frame = draw(&mut renderer, &mut adapter, &mut composition, surface)?;
        if composition.host.texture_uploads != before
            || composition.host.scene_render_count() != scene_before
            || static_frame.image != composed.image
        {
            return Err(AdapterError::Invalid("static duplicate work/pixels"));
        }
        let mut pixels = vec![];
        for view in &composition.views {
            let slot = &composition.host.slots[view];
            let x =
                ((slot.bounds.x() + slot.bounds.width() * 0.5) * slot.scale as f64).round() as u32;
            let y =
                ((slot.bounds.y() + slot.bounds.height() * 0.5) * slot.scale as f64).round() as u32;
            if x < composed.image.width() && y < composed.image.height() {
                pixels.push(serde_json::json!({"view":view.raw(),"point":[x,y],"rgba":composed.image.get_pixel(x,y).0}));
            }
        }
        diagnostics.push(serde_json::json!({"case":case.name,"provenance":provenance,"texture_uploads":before,"static_uploads_added":composition.host.texture_uploads-before,"scene_depth_renders":scene_before,"pixel_readback":pixels,"slots":kumihimo_egui::diagnostics::report_json(&composed.report)}));
    }
    // Route scene input through exactly the current canonical slot and existing host dispatcher.
    let case = CASES[3];
    let old_surface = surface(composition.root, case, 1)?;
    let original = manifest
        .browsers
        .iter()
        .find(|c| c.case_name == case.name && c.view_key == "browser-a")
        .ok_or(AdapterError::Invalid("pre-unload capture"))?;
    composition.host.confirm_engine_exit(
        &composition.controller,
        composition.views[0],
        original,
        &manifest.unload_receipt,
        &manifest.unload_request_id,
        now_ms,
    )?;
    let logical_before = composition.controller.snapshot().clone();
    draw(&mut renderer, &mut adapter, &mut composition, old_surface)?
        .write(&output_root.join("unloaded-placeholder"), 64 * 1024 * 1024)?;
    composition
        .host
        .restore_engine(&composition.controller, composition.views[0])?;
    draw(&mut renderer, &mut adapter, &mut composition, old_surface)?;
    let reloaded = manifest
        .browsers
        .iter()
        .find(|c| c.case_name == "reloaded-a" && c.view_key == "browser-a")
        .ok_or(AdapterError::Invalid(
            "missing actual restored engine capture",
        ))?;
    let expected = bridge(composition.host.frames.request(
        ScopeTarget::View(composition.views[0].raw()),
        9,
        reloaded.request.deadline_ms,
        reloaded.host_receipt.issued_ms,
    ))?;
    if !bridge(
        composition
            .host
            .dirty
            .begin_capture(expected.binding.scope.target, expected.request_id),
    )? {
        return Err(AdapterError::Invalid("reload capture not dirty"));
    }
    composition
        .host
        .accept_capture(reloaded, input_root, &expected, now_ms)?;
    composition
        .host
        .prepare_gpu(&mut renderer, old_surface, false)?;
    draw(&mut renderer, &mut adapter, &mut composition, old_surface)?
        .write(&output_root.join("engine-reloaded"), 64 * 1024 * 1024)?;
    if logical_before.views != composition.controller.snapshot().views
        || logical_before.resources != composition.controller.snapshot().resources
    {
        return Err(AdapterError::Invalid(
            "engine lifecycle changed logical identities",
        ));
    }
    diagnostics.push(serde_json::json!({"case":"reloaded-a","request":expected,"png_sha256":reloaded.png_sha256,"rgba_sha256":gpu::sha256(&composition.host.frames.current_frame(expected.binding.scope.target).map_err(host_error)?.rgba),"engine":reloaded.engine,"dom":reloaded.dom,"independent_unload_receipt":manifest.unload_receipt,"restoration_strength":"PageReload"}));
    for i in [2usize, 3] {
        let before_input = draw(&mut renderer, &mut adapter, &mut composition, old_surface)?;
        let slot = composition.host.slots[&composition.views[i]].clone();
        let point = egui::pos2(
            (slot.bounds.x() + slot.bounds.width() * 0.5) as f32,
            (slot.bounds.y() + slot.bounds.height() * 0.5) as f32,
        );
        composition.host.content_input(
            &mut composition.controller,
            &slot,
            content::Input {
                owner: content::InputOwner::Content,
                payload: "known-center-pick".into(),
                x: Some(Scalar::new(point.x as f64)?),
                y: Some(Scalar::new(point.y as f64)?),
            },
        )?;
        composition
            .host
            .prepare_gpu(&mut renderer, old_surface, false)?;
        let after_input = draw(&mut renderer, &mut adapter, &mut composition, old_surface)?;
        for j in 0..4 {
            let sample = &composition.host.slots[&composition.views[j]];
            let before = sample_slot(&before_input.image, sample, [0.5, 0.4])?;
            let after = sample_slot(&after_input.image, sample, [0.5, 0.4])?;
            if (i == j) == (before == after) {
                return Err(AdapterError::Invalid(
                    "input readback changed wrong provider or missed target",
                ));
            }
        }
        after_input.write(
            &output_root.join(if i == 2 {
                "vector-input"
            } else {
                "scene-input"
            }),
            64 * 1024 * 1024,
        )?;
    }
    let snapshot = composition.controller.snapshot().clone();
    let old_binding = renderer.gpu_binding(old_surface)?;
    let replacement_surface = surface(composition.root, case, 2)?;
    renderer = OffscreenRenderer::new(64 * 1024 * 1024)?;
    // Prevent use of old GPU resources before the fresh draw establishes the replacement slots.
    for provider in composition.host.providers.values_mut() {
        match provider {
            Provider::Browser { upload, .. } => *upload = None,
            Provider::Scene { gpu, .. } => *gpu = None,
            _ => {}
        }
    }
    composition.host.renderer_binding = None;
    draw(
        &mut renderer,
        &mut adapter,
        &mut composition,
        replacement_surface,
    )?;
    let fresh_binding = renderer.gpu_binding(replacement_surface)?;
    let replacement_slot = &composition.host.slots[&composition.views[0]];
    let old_rejected = old_binding
        .validate(&fresh_binding, replacement_slot, false)
        .is_err();
    if !old_rejected
        || snapshot.views != composition.controller.snapshot().views
        || snapshot.resources != composition.controller.snapshot().resources
    {
        return Err(AdapterError::Invalid("renderer recovery IDs/binding"));
    }
    composition
        .host
        .prepare_gpu(&mut renderer, replacement_surface, true)?;
    draw(
        &mut renderer,
        &mut adapter,
        &mut composition,
        replacement_surface,
    )?
    .write(&output_root.join("renderer-recovery"), 64 * 1024 * 1024)?;
    let before = composition.controller.snapshot().clone();
    let rejected = composition.host.native_action(
        &mut composition.controller,
        AdapterAction::PopoutView(composition.views[0]),
    );
    if !matches!(rejected, Err(AdapterError::Unsupported(_)))
        || before != *composition.controller.snapshot()
    {
        return Err(AdapterError::Invalid(
            "unsupported popout changed canonical source",
        ));
    }
    write_json(
        &output_root.join("composition-evidence.json"),
        &serde_json::json!({"protocol":"kumihimo.composition-evidence.v1","cases":diagnostics,"input_receipts":composition.host.input_receipts,"renderer_replacement":{"old_binding_rejected":old_rejected,"retained_ids":true,"reupload_source":"original exact retained engine CPU RGBA; original capture request preserved"},"unsupported_popout":"raw engine/GPU context native popout unsupported before commit; source retained","limitations":["external process software PNG copy; no native Windows Servo/zero-copy","no animation frame-ready, freeze/throttle or arbitrary page-state restoration","headless owned WebDriver input external; 2D/3D host input injected","normal/narrow/injected-scale1.5; no physical display/input proof"]}),
    )
}
