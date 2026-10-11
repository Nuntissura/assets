use crate::*;

impl Core {
    pub(crate) fn model_candidate(&self) -> Self {
        let mut core = Self::new(self.model.limits, 0);
        core.model = self.model.clone();
        core.next_entity = self.next_entity;
        core
    }
    pub(crate) fn mutate(&mut self, command: Command) -> Result<Change, Error> {
        let layout_command = matches!(
            command,
            Command::SetLayout { .. }
                | Command::SetGeometryContext { .. }
                | Command::CreateTool(_)
                | Command::ConfigureTool { .. }
                | Command::SetToolState { .. }
                | Command::RebindTool { .. }
                | Command::Dock(_)
                | Command::CommitPreview(_)
        );
        if !layout_command
            && self.model.layouts.is_empty()
            && self.model.contexts.is_empty()
            && self.model.tools.is_empty()
            && self.model.floats.is_empty()
        {
            return self.mutate_ownership(command);
        }
        let mut candidate = self.model_candidate();
        let old_entries = self.model.layout_entry_count();
        let change = candidate.apply_arrangement(command, &self.model)?;
        candidate.migrate_measurements(&self.model);
        let entries = candidate.model.layout_entry_count();
        if entries > old_entries {
            if let Some(limit) = candidate.model.limits.max_layout_entries {
                if entries > limit {
                    return Err(Error::Admission {
                        budget: Budget::LayoutEntries,
                        limit,
                        attempted: entries,
                    });
                }
            }
        }
        candidate.validate_arrangement()?;
        self.model = candidate.model;
        self.next_entity = candidate.next_entity;
        Ok(change)
    }
    fn apply_arrangement(&mut self, command: Command, before: &Snapshot) -> Result<Change, Error> {
        match command {
            Command::SetLayout { workspace, plan } => {
                if self.model.layouts.get(&workspace) == Some(&plan) {
                    return Ok(Change::Unchanged);
                }
                let generation = self.next_generation()?;
                self.model.layouts.insert(workspace, plan);
                self.model.generation = generation;
                Ok(Change::LayoutChanged(workspace))
            }
            Command::SetGeometryContext { workspace, context } => {
                for location in &context.allowed_docks {
                    self.validate_dock_reference(*location)?;
                }
                if self.model.contexts.get(&workspace) == Some(&context) {
                    return Ok(Change::Unchanged);
                }
                let generation = self.next_generation()?;
                self.model.contexts.insert(workspace, context);
                self.model.generation = generation;
                Ok(Change::ContextChanged(workspace))
            }
            Command::CreateTool(spec) => {
                if let Some(limit) = self.model.limits.max_records {
                    let attempted = self.model.record_count() + 1;
                    if attempted > limit {
                        return Err(Error::Admission {
                            budget: Budget::Records,
                            limit,
                            attempted,
                        });
                    }
                }
                let (raw, next) = self.allocation()?;
                let generation = self.next_generation()?;
                let id = ToolId::from_raw(raw);
                let tool = Tool {
                    id,
                    owner: spec.owner,
                    origin: spec.origin,
                    target: spec.target,
                    placement: spec.placement,
                    policy: spec.policy,
                    state: spec.state,
                };
                self.model.validate_tool(&tool)?;
                self.model.tools.insert(id, tool);
                if let ToolPlacement::Cell {
                    workspace,
                    grid,
                    row,
                    column,
                } = spec.placement
                {
                    self.insert_cell(workspace, grid, row, column, LayoutItem::Tool(id))?;
                }
                self.next_entity = next;
                self.model.generation = generation;
                Ok(Change::Created(EntityId::Tool(id)))
            }
            Command::ConfigureTool { tool, policy } => {
                let record = self
                    .model
                    .tools
                    .get(&tool)
                    .ok_or(Error::Missing(EntityId::Tool(tool)))?;
                if record.policy == policy {
                    return Ok(Change::Unchanged);
                }
                let generation = self.next_generation()?;
                self.model.tools.get_mut(&tool).unwrap().policy = policy;
                self.model.generation = generation;
                Ok(Change::ToolChanged(tool))
            }
            Command::SetToolState { tool, state } => {
                let record = self
                    .model
                    .tools
                    .get(&tool)
                    .ok_or(Error::Missing(EntityId::Tool(tool)))?;
                if record.state == state {
                    return Ok(Change::Unchanged);
                }
                let generation = self.next_generation()?;
                self.model.tools.get_mut(&tool).unwrap().state = state;
                self.model.generation = generation;
                Ok(Change::ToolChanged(tool))
            }
            Command::RebindTool { tool, target } => {
                let record = self
                    .model
                    .tools
                    .get(&tool)
                    .ok_or(Error::Missing(EntityId::Tool(tool)))?;
                if record.target == target {
                    return Ok(Change::Unchanged);
                }
                if !record.policy.compatible_targets.contains(&target) {
                    return Err(Error::IncompatibleTarget);
                }
                self.model.target_valid(target)?;
                let generation = self.next_generation()?;
                self.model.tools.get_mut(&tool).unwrap().target = target;
                self.model.generation = generation;
                Ok(Change::ToolChanged(tool))
            }
            Command::Dock(intent) => self.dock(intent),
            Command::CommitPreview(preview) => {
                let intent = Self::preview_command(preview, self.model.generation)?;
                self.dock(intent)
            }
            command => {
                let change = self.mutate_ownership(command)?;
                if change != Change::Unchanged {
                    // Departures before arrivals preserve custom descendants, tracks and holes.
                    for (workspace, old) in &before.workspaces {
                        if let Some(plan) = self.model.layouts.get_mut(workspace) {
                            for frame in &old.frames {
                                let moved = self.model.frames[frame].workspace != *workspace
                                    || self.model.workspaces[workspace]
                                        .frames
                                        .iter()
                                        .position(|f| f == frame)
                                        != old.frames.iter().position(|f| f == frame);
                                // Only the explicitly moved item is repaired, not siblings shifted by insertion.
                                if moved
                                    && matches!(change, Change::Moved(EntityId::Frame(id)) if id == *frame)
                                {
                                    plan.remove(LayoutItem::Frame(*frame))?;
                                }
                            }
                        }
                    }
                    match change {
                        Change::Created(EntityId::Frame(frame))
                        | Change::Moved(EntityId::Frame(frame)) => {
                            self.model.floats.remove(&frame);
                            let destination = self.model.frames[&frame].workspace;
                            if let Some(plan) = self.model.layouts.get_mut(&destination) {
                                plan.append(LayoutItem::Frame(frame))?;
                            }
                        }
                        _ => {}
                    }
                }
                Ok(change)
            }
        }
    }
    fn migrate_measurements(&mut self, before: &Snapshot) {
        for (id, old) in &before.frames {
            let destination = self.model.frames[id].workspace;
            if old.workspace == destination {
                continue;
            }
            let source = self.model.contexts.get_mut(&old.workspace);
            let (minimum, weight) = source
                .map(|c| (c.minimums.remove(id), c.frame_weights.remove(id)))
                .unwrap_or((None, None));
            if let Some(target) = self.model.contexts.get_mut(&destination) {
                if let Some(minimum) = minimum {
                    target.minimums.insert(*id, minimum);
                }
                if let Some(weight) = weight {
                    target.frame_weights.insert(*id, weight);
                }
            }
        }
    }
    fn validate_arrangement(&self) -> Result<(), Error> {
        for (workspace, plan) in &self.model.layouts {
            self.model.validate_plan(*workspace, plan)?;
        }
        for (workspace, context) in &self.model.contexts {
            if !self.model.workspaces.contains_key(workspace) {
                return Err(Error::Missing(EntityId::Workspace(*workspace)));
            }
            crate::geometry::nonnegative(context.gap.get())?;
            for frame in context.minimums.keys().chain(context.frame_weights.keys()) {
                if !self
                    .model
                    .frames
                    .get(frame)
                    .is_some_and(|f| f.workspace == *workspace)
                {
                    return Err(
                        GeometryError::InvalidPlan("measurement target outside workspace").into(),
                    );
                }
            }
            for weight in context.frame_weights.values() {
                crate::geometry::positive(weight.get())?;
            }
            // Allowed destinations may be stale after a topology change; attempts still revalidate their actual target.
        }
        for tool in self.model.tools.values() {
            self.model.validate_tool(tool)?;
        }
        for workspace in self.model.contexts.keys() {
            self.model.geometry(*workspace)?;
        }
        Ok(())
    }
    fn validate_dock_reference(&self, location: DockLocation) -> Result<(), Error> {
        match location {
            DockLocation::Append { workspace } | DockLocation::WorkspaceEdge { workspace, .. } => {
                if !self.model.workspaces.contains_key(&workspace) {
                    return Err(Error::Missing(EntityId::Workspace(workspace)));
                }
            }
            DockLocation::Beside { frame, .. } => {
                if !self.model.frames.contains_key(&frame) {
                    return Err(Error::Missing(EntityId::Frame(frame)));
                }
            }
            DockLocation::Cell {
                workspace,
                grid,
                row,
                column,
            } => {
                let plan = self
                    .model
                    .layouts
                    .get(&workspace)
                    .ok_or(GeometryError::InvalidPlan("configured destination absent"))?;
                let Some(LayoutNode::Grid { rows, columns, .. }) = plan.nodes.get(&grid) else {
                    return Err(GeometryError::InvalidPlan("destination grid absent").into());
                };
                if row >= rows.len() || column >= columns.len() {
                    return Err(GeometryError::InvalidPlan("destination cell outside grid").into());
                }
            }
        }
        Ok(())
    }
}
