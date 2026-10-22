//! Bounded canonical native destinations. Preparation is durable, never a rollback.
use crate::{
    native::Ready,
    persistence::{NativeBinding, NativeContent, ReturnAnchor},
    *,
};
use kumihimo_core::{
    Change, DockIntent, DockLocation, EntityId, GeometryContext, Identity, Snapshot,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedSlot {
    pub root: WorkspaceId,
    pub carrier: Option<FrameId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingNative {
    pub nonce: u64,
    pub slot: PreparedSlot,
    pub content: NativeContent,
    pub return_anchor: ReturnAnchor,
    pub identity: Identity,
    pub generation: u64,
    pub source_frame: FrameId,
    pub source_workspace: WorkspaceId,
}
#[derive(Default)]
pub struct WindowSlots {
    slots: Vec<PreparedSlot>,
    bindings: Vec<NativeBinding>,
    next_nonce: u64,
    pending: std::collections::BTreeMap<WorkspaceId, PendingNative>,
}
#[derive(Clone, Debug, Default)]
pub struct CloseResult {
    pub closed: Vec<ViewId>,
    pub remaining: Vec<ViewId>,
    pub rejected: Vec<String>,
}
impl WindowSlots {
    pub fn prepared(&self) -> &[PreparedSlot] {
        &self.slots
    }
    pub fn bindings(&self) -> &[NativeBinding] {
        &self.bindings
    }
    pub fn prepare(
        &mut self,
        controller: &mut Controller,
        host: &mut impl DesktopHost,
        content: NativeContent,
    ) -> Result<PendingNative> {
        let (source_frame, return_anchor) = source(controller.snapshot(), &content)?;
        let source_workspace = controller.snapshot().frames[&source_frame].workspace;
        let needs_carrier = !matches!(content, NativeContent::Frame(_));
        let reusable = self
            .slots
            .iter()
            .find(|s| {
                s.carrier.is_some() == needs_carrier && eligible(controller.snapshot(), s).is_ok()
            })
            .cloned();
        let slot = match reusable {
            Some(slot) => slot,
            None => {
                if self.slots.len() >= host.policy().max_native_slots {
                    return Err(AdapterError::Budget("native prepared slots"));
                }
                let root = match host
                    .dispatch(controller, Command::CreateWorkspace { owner: None })?
                    .outcome
                    .result?
                {
                    Change::Created(EntityId::Workspace(id)) => id,
                    _ => return Err(AdapterError::Invalid("workspace creation response")),
                };
                // Retain even if carrier creation fails: Core deliberately has no deletion.
                self.slots.push(PreparedSlot {
                    root,
                    carrier: None,
                });
                if needs_carrier {
                    let frame = match host
                        .dispatch(
                            controller,
                            Command::CreateFrame {
                                workspace: root,
                                index: 0,
                            },
                        )?
                        .outcome
                        .result?
                    {
                        Change::Created(EntityId::Frame(id)) => id,
                        _ => return Err(AdapterError::Invalid("carrier creation response")),
                    };
                    self.slots.last_mut().unwrap().carrier = Some(frame);
                }
                self.slots.last().unwrap().clone()
            }
        };
        self.next_nonce = self
            .next_nonce
            .checked_add(1)
            .ok_or(AdapterError::Budget("native nonce exhausted"))?;
        let pending = PendingNative {
            nonce: self.next_nonce,
            slot,
            content,
            return_anchor,
            identity: controller.snapshot().identity.clone(),
            generation: controller.snapshot().generation,
            source_frame,
            source_workspace,
        };
        self.pending.insert(pending.slot.root, pending.clone());
        Ok(pending)
    }
    /// Complete root context preparation before binding the screenshot to a generation.
    pub fn arm(
        &mut self,
        pending: &mut PendingNative,
        controller: &mut Controller,
        host: &mut impl DesktopHost,
        bounds: Rect,
    ) -> Result<()> {
        self.validate_pending(pending)?;
        self.cancel(pending, controller.snapshot())?;
        eligible(controller.snapshot(), &pending.slot)?;
        let context = GeometryContext::new(bounds);
        if controller.snapshot().contexts.get(&pending.slot.root) != Some(&context) {
            host.dispatch(
                controller,
                Command::SetGeometryContext {
                    workspace: pending.slot.root,
                    context,
                },
            )?
            .outcome
            .result?;
        }
        let (frame, _) = source(controller.snapshot(), &pending.content)?;
        if frame != pending.source_frame
            || controller.snapshot().frames[&frame].workspace != pending.source_workspace
            || controller.snapshot().identity != pending.identity
        {
            return Err(AdapterError::Stale("native source identity"));
        }
        pending.generation = controller.snapshot().generation;
        self.pending.insert(pending.slot.root, pending.clone());
        Ok(())
    }
    pub fn cancel(&self, pending: &PendingNative, snapshot: &Snapshot) -> Result<()> {
        let (frame, _) = source(snapshot, &pending.content)?;
        if frame != pending.source_frame
            || snapshot.frames[&frame].workspace != pending.source_workspace
            || snapshot.identity != pending.identity
        {
            return Err(AdapterError::Stale("source moved before cancellation"));
        }
        Ok(())
    }
    /// Revoke this exact reservation; preparation remains available for a fresh nonce.
    pub fn cancel_pending(&mut self, pending: &PendingNative, snapshot: &Snapshot) -> Result<()> {
        self.validate_pending(pending)?;
        self.cancel(pending, snapshot)?;
        self.pending.remove(&pending.slot.root);
        Ok(())
    }
    pub(crate) fn validate_pending(&self, pending: &PendingNative) -> Result<()> {
        if self.pending.get(&pending.slot.root) != Some(pending) {
            return Err(AdapterError::Stale("native reservation cancelled/replaced/changed"));
        }
        Ok(())
    }
    /// Revoke this failed attempt only; a later reservation at the same root is untouched.
    pub(crate) fn revoke_pending(&mut self, pending: &PendingNative) -> bool {
        if self.pending.get(&pending.slot.root) == Some(pending) {
            self.pending.remove(&pending.slot.root);
            true
        } else { false }
    }
    pub(crate) fn commit(
        &mut self,
        pending: PendingNative,
        ready: Ready,
        controller: &mut Controller,
        host: &mut impl DesktopHost,
    ) -> Result<NativeBinding> {
        self.validate_pending(&pending)?;
        ready.validate(&pending, controller.snapshot())?;
        eligible(controller.snapshot(), &pending.slot)?;
        let (frame, _) = source(controller.snapshot(), &pending.content)?;
        if frame != pending.source_frame
            || controller.snapshot().frames[&frame].workspace != pending.source_workspace
        {
            return Err(AdapterError::Stale("native source moved"));
        }
        let command = match pending.content {
            NativeContent::Frame(frame) => Command::MoveFrame {
                frame,
                destination: pending.slot.root,
                index: 0,
            },
            NativeContent::View(view) => Command::MoveView {
                view,
                destination: pending
                    .slot
                    .carrier
                    .ok_or(AdapterError::Invalid("missing carrier"))?,
                index: 0,
            },
            NativeContent::Group(group) => Command::MoveGroup {
                group,
                destination: pending
                    .slot
                    .carrier
                    .ok_or(AdapterError::Invalid("missing carrier"))?,
                index: 0,
                group_index: 0,
            },
        };
        host.dispatch(controller, command)?.outcome.result?;
        let binding = NativeBinding {
            root: pending.slot.root,
            carrier: pending.slot.carrier,
            content: pending.content,
            return_anchor: pending.return_anchor,
        };
        verify_recipient(controller.snapshot(), &binding)?;
        self.pending.remove(&binding.root);
        self.bindings.push(binding.clone());
        Ok(binding)
    }
    /// Freshly preview each semantic anchor. A fallback must be explicitly supplied by the host.
    pub fn return_content(
        &mut self,
        root: WorkspaceId,
        fallback: Option<ReturnAnchor>,
        controller: &mut Controller,
        host: &mut impl DesktopHost,
    ) -> Result<bool> {
        let binding = self
            .bindings
            .iter()
            .find(|b| b.root == root)
            .cloned()
            .ok_or(AdapterError::Invalid("native binding"))?;
        verify_recipient(controller.snapshot(), &binding)?;
        let original = return_command(controller, &binding.content, &binding.return_anchor);
        let (command, used_fallback) = match original {
            Ok(c) => (c, false),
            Err(error) => match fallback {
                Some(anchor) => (return_command(controller, &binding.content, &anchor)?, true),
                None => return Err(error),
            },
        };
        host.dispatch(controller, command)?.outcome.result?;
        self.bindings.retain(|b| b.root != root);
        Ok(used_fallback)
    }
    /// Close individually through the Controller's provider-permit boundary; preserve vetoed content.
    pub fn close_content(
        &mut self,
        root: WorkspaceId,
        approved: bool,
        controller: &mut Controller,
        host: &mut impl DesktopHost,
    ) -> Result<CloseResult> {
        let binding = self
            .bindings
            .iter()
            .find(|b| b.root == root)
            .ok_or(AdapterError::Invalid("native binding"))?;
        verify_recipient(controller.snapshot(), binding)?;
        let frames = subtree_frames(controller.snapshot(), root);
        let views: Vec<_> = controller
            .snapshot()
            .views
            .values()
            .filter(|v| frames.contains(&v.frame) && v.closed.is_none())
            .map(|v| v.id)
            .collect();
        let mut result = CloseResult::default();
        for view in views {
            let outcome = host
                .dispatch(controller, Command::CloseView { view, approved })
                .and_then(|r| r.outcome.result.map(|_| ()).map_err(Into::into));
            match outcome {
                Ok(()) => result.closed.push(view),
                Err(e) => {
                    result.remaining.push(view);
                    result.rejected.push(e.to_string());
                }
            }
        }
        if result.remaining.is_empty() {
            self.bindings.retain(|b| b.root != root);
        }
        Ok(result)
    }
}
fn subtree_frames(snapshot: &Snapshot, root: WorkspaceId) -> std::collections::BTreeSet<FrameId> {
    let mut frames = std::collections::BTreeSet::new();
    let mut queue = vec![root];
    while let Some(root) = queue.pop() {
        if let Some(w) = snapshot.workspaces.get(&root) {
            for f in &w.frames {
                if frames.insert(*f) {
                    if let Some(child) = snapshot.frames[f].child_workspace {
                        queue.push(child);
                    }
                }
            }
        }
    }
    frames
}
pub fn eligible(snapshot: &Snapshot, slot: &PreparedSlot) -> Result<()> {
    let workspace = snapshot
        .workspaces
        .get(&slot.root)
        .ok_or(AdapterError::Invalid("missing native root"))?;
    if workspace.owner.is_some() || snapshot.maximized.contains_key(&slot.root) {
        return Err(AdapterError::Invalid("owned/maximized native root"));
    }
    match slot.carrier {
        None if workspace.frames.is_empty() => Ok(()),
        Some(carrier) if workspace.frames == [carrier] => {
            let f = snapshot
                .frames
                .get(&carrier)
                .ok_or(AdapterError::Invalid("missing carrier"))?;
            if f.workspace != slot.root
                || f.child_workspace.is_some()
                || !f.views.is_empty()
                || !f.groups.is_empty()
                || snapshot.floats.contains_key(&carrier)
            {
                return Err(AdapterError::Invalid("occupied native carrier"));
            }
            Ok(())
        }
        _ => Err(AdapterError::Invalid("occupied native root")),
    }
}
fn source(snapshot: &Snapshot, content: &NativeContent) -> Result<(FrameId, ReturnAnchor)> {
    let frame = match *content {
        NativeContent::Frame(frame) => frame,
        NativeContent::View(view) => {
            let v = snapshot
                .views
                .get(&view)
                .ok_or(AdapterError::Invalid("native view"))?;
            if v.closed.is_some() {
                return Err(AdapterError::Invalid("closed native view"));
            }
            v.frame
        }
        NativeContent::Group(group) => {
            snapshot
                .groups
                .get(&group)
                .ok_or(AdapterError::Invalid("native group"))?
                .frame
        }
    };
    let f = snapshot
        .frames
        .get(&frame)
        .ok_or(AdapterError::Invalid("native source frame"))?;
    if snapshot.maximized.contains_key(&f.workspace) {
        return Err(AdapterError::Invalid("maximized native source"));
    }
    let anchor = match *content {
        NativeContent::Frame(_) => {
            let neighbor = snapshot.workspaces[&f.workspace]
                .frames
                .iter()
                .copied()
                .find(|id| *id != frame);
            let location = neighbor
                .map(|frame| DockLocation::Beside {
                    frame,
                    edge: kumihimo_core::Edge::Right,
                })
                .unwrap_or(DockLocation::Append {
                    workspace: f.workspace,
                });
            ReturnAnchor::Frame {
                workspace: f.workspace,
                location,
            }
        }
        NativeContent::View(view) => ReturnAnchor::View {
            frame,
            index: f
                .views
                .iter()
                .position(|v| *v == view)
                .ok_or(AdapterError::Invalid("view association"))?,
        },
        NativeContent::Group(group) => ReturnAnchor::Group {
            frame,
            index: f
                .views
                .iter()
                .position(|v| snapshot.views[v].group == Some(group))
                .unwrap_or(f.views.len()),
            group_index: f
                .groups
                .iter()
                .position(|g| *g == group)
                .ok_or(AdapterError::Invalid("group association"))?,
        },
    };
    Ok((frame, anchor))
}
pub fn verify_recipient(snapshot: &Snapshot, binding: &NativeBinding) -> Result<()> {
    let frame = match binding.content {
        NativeContent::Frame(f) => f,
        NativeContent::View(v) => {
            snapshot
                .views
                .get(&v)
                .ok_or(AdapterError::Invalid("recipient view"))?
                .frame
        }
        NativeContent::Group(g) => {
            snapshot
                .groups
                .get(&g)
                .ok_or(AdapterError::Invalid("recipient group"))?
                .frame
        }
    };
    if snapshot
        .frames
        .get(&frame)
        .is_none_or(|f| f.workspace != binding.root)
        || (!matches!(binding.content, NativeContent::Frame(_)) && binding.carrier != Some(frame))
    {
        return Err(AdapterError::Stale("canonical native recipient"));
    }
    Ok(())
}
fn return_command(
    controller: &Controller,
    content: &NativeContent,
    anchor: &ReturnAnchor,
) -> Result<Command> {
    let snapshot = controller.snapshot();
    match (content, anchor) {
        (
            NativeContent::Frame(frame),
            ReturnAnchor::Frame {
                workspace,
                location,
            },
        ) => {
            if !snapshot.workspaces.contains_key(workspace)
                || snapshot.maximized.contains_key(workspace)
                || matches!(location, DockLocation::Cell { .. })
            {
                return Err(AdapterError::Invalid("unreachable native return anchor"));
            }
            let intent = DockIntent::Frame {
                frame: *frame,
                location: *location,
            };
            controller.core().preview(intent.clone())?;
            Ok(Command::Dock(intent))
        }
        (NativeContent::View(view), ReturnAnchor::View { frame, index }) => {
            validate_carrier(snapshot, *frame)?;
            Ok(Command::MoveView {
                view: *view,
                destination: *frame,
                index: (*index).min(snapshot.frames[frame].views.len()),
            })
        }
        (
            NativeContent::Group(group),
            ReturnAnchor::Group {
                frame,
                index,
                group_index,
            },
        ) => {
            validate_carrier(snapshot, *frame)?;
            Ok(Command::MoveGroup {
                group: *group,
                destination: *frame,
                index: (*index).min(snapshot.frames[frame].views.len()),
                group_index: (*group_index).min(snapshot.frames[frame].groups.len()),
            })
        }
        _ => Err(AdapterError::Invalid("return anchor kind")),
    }
}
fn validate_carrier(snapshot: &Snapshot, frame: FrameId) -> Result<()> {
    let f = snapshot
        .frames
        .get(&frame)
        .ok_or(AdapterError::Invalid("return carrier missing"))?;
    if f.child_workspace.is_some() || snapshot.maximized.contains_key(&f.workspace) {
        return Err(AdapterError::Invalid("return carrier ineligible"));
    }
    Ok(())
}
