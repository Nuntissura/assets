use crate::*;

impl Core {
    pub(crate) fn layout_action(&mut self, command: Command) -> Result<Change, Error> {
        let before = self.model.clone();
        let workspace = match command {
            Command::Equalize { workspace } => {
                self.require_workspace(workspace)?;
                let one = Scalar::new(1.0)?;
                if let Some(plan) = self.model.layouts.get_mut(&workspace) {
                    for node in plan.nodes.values_mut() {
                        match node {
                            LayoutNode::Split { children, .. } => {
                                for child in children {
                                    child.weight = one;
                                }
                            }
                            LayoutNode::Grid { rows, columns, .. } => {
                                for weight in rows.iter_mut().chain(columns.iter_mut()) {
                                    *weight = one;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(context) = self.model.contexts.get_mut(&workspace) {
                    for weight in context.frame_weights.values_mut() {
                        *weight = one;
                    }
                }
                workspace
            }
            Command::SwapFrames { first, second } => {
                let workspace = self.frame_workspace(first)?;
                if self.frame_workspace(second)? != workspace
                    || self.model.floats.contains_key(&first)
                    || self.model.floats.contains_key(&second)
                {
                    return Err(Error::ForbiddenDestination);
                }
                if first == second {
                    return Ok(Change::Unchanged);
                }
                self.ensure_plan(workspace)?;
                let plan = self.model.layouts.get_mut(&workspace).unwrap();
                let mut found = [false; 2];
                for node in plan.nodes.values_mut() {
                    match node {
                        LayoutNode::Leaf(LayoutItem::Frame(frame)) if *frame == first => {
                            *frame = second;
                            found[0] = true;
                        }
                        LayoutNode::Leaf(LayoutItem::Frame(frame)) if *frame == second => {
                            *frame = first;
                            found[1] = true;
                        }
                        _ => {}
                    }
                }
                if found != [true, true] {
                    return Err(Error::InvalidState("swap needs docked frame leaves"));
                }
                workspace
            }
            Command::Maximize { frame } => {
                let workspace = self.frame_workspace(frame)?;
                if self.model.floats.contains_key(&frame) {
                    return Err(Error::ForbiddenDestination);
                }
                let plan = self.model.effective_plan(workspace)?;
                self.model.validate_plan(workspace, &plan)?;
                if !plan
                    .nodes
                    .values()
                    .any(|node| *node == LayoutNode::Leaf(LayoutItem::Frame(frame)))
                {
                    return Err(Error::InvalidState("maximize needs docked frame leaf"));
                }
                self.model.maximized.insert(workspace, frame);
                workspace
            }
            Command::RestoreLayout { workspace } => {
                self.require_workspace(workspace)?;
                self.model.maximized.remove(&workspace);
                workspace
            }
            _ => return Err(Error::InvalidState("not a layout action")),
        };
        if before == self.model {
            return Ok(Change::Unchanged);
        }
        self.model.generation = self.next_generation()?;
        Ok(Change::LayoutChanged(workspace))
    }

    fn require_workspace(&self, workspace: WorkspaceId) -> Result<(), Error> {
        if self.model.workspaces.contains_key(&workspace) {
            Ok(())
        } else {
            Err(Error::Missing(EntityId::Workspace(workspace)))
        }
    }
    fn frame_workspace(&self, frame: FrameId) -> Result<WorkspaceId, Error> {
        self.model
            .frames
            .get(&frame)
            .map(|f| f.workspace)
            .ok_or(Error::Missing(EntityId::Frame(frame)))
    }
    fn placement_workspace(&self, placement: ToolPlacement) -> Result<WorkspaceId, Error> {
        match placement {
            ToolPlacement::Cell { workspace, .. }
            | ToolPlacement::Edge {
                anchor: ToolAnchor::Workspace(workspace),
                ..
            } => Ok(workspace),
            ToolPlacement::Edge {
                anchor: ToolAnchor::Frame(frame),
                ..
            } => self.frame_workspace(frame),
            ToolPlacement::Edge {
                anchor: ToolAnchor::View(view),
                ..
            } => {
                let frame = self
                    .model
                    .views
                    .get(&view)
                    .ok_or(Error::Missing(EntityId::View(view)))?
                    .frame;
                self.frame_workspace(frame)
            }
        }
    }
    fn reject_maximized(&self, workspace: WorkspaceId) -> Result<(), Error> {
        if self.model.maximized.contains_key(&workspace) {
            Err(Error::Maximized(workspace))
        } else {
            Ok(())
        }
    }
    fn equal_weights(&self, workspace: WorkspaceId) -> bool {
        self.model.layouts.get(&workspace).is_none_or(|plan| {
            plan.nodes.values().all(|node| match node {
                LayoutNode::Split { children, .. } => {
                    children.iter().all(|c| c.weight.get() == 1.0)
                }
                LayoutNode::Grid { rows, columns, .. } => {
                    rows.iter().chain(columns).all(|w| w.get() == 1.0)
                }
                _ => true,
            })
        }) && self
            .model
            .contexts
            .get(&workspace)
            .is_none_or(|c| c.frame_weights.values().all(|w| w.get() == 1.0))
    }
    pub(crate) fn guard_maximized(&self, command: &Command) -> Result<(), Error> {
        match command {
            Command::CreateWorkspace { owner: Some(frame) } => {
                self.reject_maximized(self.frame_workspace(*frame)?)
            }
            Command::CreateFrame { workspace, .. } => self.reject_maximized(*workspace),
            Command::MoveFrame {
                frame,
                destination,
                index,
            } => {
                let source = self.frame_workspace(*frame)?;
                if source == *destination
                    && self.model.workspaces[&source].frames.get(*index) == Some(frame)
                {
                    return Ok(());
                }
                self.reject_maximized(source)?;
                self.reject_maximized(*destination)
            }
            Command::SetLayout { workspace, plan } => {
                if self.model.layouts.get(workspace) == Some(plan) {
                    return Ok(());
                }
                self.reject_maximized(*workspace)
            }
            Command::Equalize { workspace } => {
                if self.equal_weights(*workspace) {
                    return Ok(());
                }
                self.reject_maximized(*workspace)
            }
            Command::SwapFrames { first, second } => {
                if first == second {
                    return Ok(());
                }
                self.reject_maximized(self.frame_workspace(*first)?)?;
                self.reject_maximized(self.frame_workspace(*second)?)
            }
            Command::CreateTool(spec) => {
                self.reject_maximized(self.placement_workspace(spec.placement)?)
            }
            Command::Dock(intent) => self.guard_dock_maximized(intent),
            Command::CommitPreview(preview) => {
                let intent = Self::preview_command(
                    preview.clone(),
                    self.model.generation,
                    &self.model.identity,
                )?;
                self.guard_dock_maximized(&intent)
            }
            _ => Ok(()),
        }
    }
    fn guard_dock_maximized(&self, intent: &DockIntent) -> Result<(), Error> {
        match intent {
            DockIntent::Frame { frame, location } => {
                // A same-position dock is harmless only if the complete retained packing is unchanged.
                // Probe the already detached primitive; this does not touch canonical allocation or receipts.
                let source = self.frame_workspace(*frame)?;
                let destination = self.location_workspace(*location)?;
                if self.model.maximized.contains_key(&source)
                    || self.model.maximized.contains_key(&destination)
                {
                    let mut candidate = self.model_candidate();
                    if candidate.dock(intent.clone()) == Ok(Change::Unchanged) {
                        return Ok(());
                    }
                }
                self.reject_maximized(source)?;
                self.reject_maximized(destination)
            }
            DockIntent::Tool { tool, placement } => {
                let record = self
                    .model
                    .tools
                    .get(tool)
                    .ok_or(Error::Missing(EntityId::Tool(*tool)))?;
                if record.placement == *placement {
                    return Ok(());
                }
                self.reject_maximized(self.placement_workspace(record.placement)?)?;
                self.reject_maximized(self.placement_workspace(*placement)?)
            }
            DockIntent::Float { frame, rect } => {
                if self
                    .model
                    .floats
                    .get(frame)
                    .is_some_and(|f| f.rect == *rect)
                {
                    return Ok(());
                }
                self.reject_maximized(self.frame_workspace(*frame)?)
            }
            DockIntent::Redock { frame } => {
                self.reject_maximized(self.frame_workspace(*frame)?)?;
                if let Some(float) = self.model.floats.get(frame) {
                    self.reject_maximized(self.location_workspace(float.return_location)?)?;
                }
                Ok(())
            }
        }
    }
}
