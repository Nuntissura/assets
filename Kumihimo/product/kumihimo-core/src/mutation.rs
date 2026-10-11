use crate::*;

fn index_check(index: usize, available: usize) -> Result<(), Error> {
    if index <= available {
        Ok(())
    } else {
        Err(Error::InvalidIndex { index, available })
    }
}
fn budget_check(budget: Budget, limit: Option<usize>, attempted: usize) -> Result<(), Error> {
    match limit {
        Some(limit) if attempted > limit => Err(Error::Admission {
            budget,
            limit,
            attempted,
        }),
        _ => Ok(()),
    }
}

impl Core {
    fn workspace(&self, id: WorkspaceId) -> Result<&Workspace, Error> {
        self.model
            .workspaces
            .get(&id)
            .ok_or(Error::Missing(EntityId::Workspace(id)))
    }
    fn frame(&self, id: FrameId) -> Result<&Frame, Error> {
        self.model
            .frames
            .get(&id)
            .ok_or(Error::Missing(EntityId::Frame(id)))
    }
    fn view(&self, id: ViewId) -> Result<&View, Error> {
        self.model
            .views
            .get(&id)
            .ok_or(Error::Missing(EntityId::View(id)))
    }
    fn creation(&self) -> Result<(u64, u64, u64), Error> {
        budget_check(
            Budget::Records,
            self.model.limits.max_records,
            self.model.record_count() + 1,
        )?;
        let (id, next) = self.allocation()?;
        Ok((id, next, self.next_generation()?))
    }
    fn workspace_depth(&self, mut workspace: WorkspaceId) -> Result<usize, Error> {
        let mut depth = 0usize;
        while let Some(owner) = self.workspace(workspace)?.owner {
            depth = depth
                .checked_add(1)
                .ok_or(Error::Exhausted(Counter::Depth))?;
            workspace = self.frame(owner)?.workspace;
        }
        Ok(depth)
    }
    fn frame_depth(&self, frame: FrameId) -> Result<usize, Error> {
        self.workspace_depth(self.frame(frame)?.workspace)?
            .checked_add(1)
            .ok_or(Error::Exhausted(Counter::Depth))
    }
    /// Maximum relative frame depth of a flat subtree. No recursive call or recursive ownership.
    fn subtree_height(&self, root: FrameId) -> Result<usize, Error> {
        let mut pending = vec![(root, 0usize)];
        let mut height = 0;
        while let Some((frame, depth)) = pending.pop() {
            height = height.max(depth);
            if let Some(child) = self.frame(frame)?.child_workspace {
                let next = depth
                    .checked_add(1)
                    .ok_or(Error::Exhausted(Counter::Depth))?;
                pending.extend(self.workspace(child)?.frames.iter().map(|id| (*id, next)));
            }
        }
        Ok(height)
    }
    pub(crate) fn mutate_ownership(&mut self, command: Command) -> Result<Change, Error> {
        match command {
            Command::CreateWorkspace { owner } => {
                if let Some(frame) = owner {
                    if self.frame(frame)?.child_workspace.is_some() {
                        return Err(Error::ChildWorkspaceExists(frame));
                    }
                }
                let (raw, next, generation) = self.creation()?;
                let id = WorkspaceId::from_raw(raw);
                self.model.workspaces.insert(
                    id,
                    Workspace {
                        id,
                        owner,
                        frames: vec![],
                    },
                );
                if let Some(frame) = owner {
                    self.model.frames.get_mut(&frame).unwrap().child_workspace = Some(id);
                }
                self.next_entity = next;
                self.model.generation = generation;
                Ok(Change::Created(EntityId::Workspace(id)))
            }
            Command::CreateFrame { workspace, index } => {
                index_check(index, self.workspace(workspace)?.frames.len())?;
                let depth = self
                    .workspace_depth(workspace)?
                    .checked_add(1)
                    .ok_or(Error::Exhausted(Counter::Depth))?;
                budget_check(Budget::Depth, self.model.limits.max_depth, depth)?;
                let (raw, next, generation) = self.creation()?;
                let id = FrameId::from_raw(raw);
                self.model.frames.insert(
                    id,
                    Frame {
                        id,
                        workspace,
                        child_workspace: None,
                        views: vec![],
                        selected_view: None,
                        groups: vec![],
                    },
                );
                self.model
                    .workspaces
                    .get_mut(&workspace)
                    .unwrap()
                    .frames
                    .insert(index, id);
                self.next_entity = next;
                self.model.generation = generation;
                Ok(Change::Created(EntityId::Frame(id)))
            }
            Command::RegisterProvider {
                supports_shared_views,
            } => {
                let (raw, next, generation) = self.creation()?;
                let id = ProviderId::from_raw(raw);
                self.model.providers.insert(
                    id,
                    Provider {
                        id,
                        supports_shared_views,
                    },
                );
                self.next_entity = next;
                self.model.generation = generation;
                Ok(Change::Created(EntityId::Provider(id)))
            }
            Command::RegisterResource { provider } => {
                if !self.model.providers.contains_key(&provider) {
                    return Err(Error::Missing(EntityId::Provider(provider)));
                }
                let (raw, next, generation) = self.creation()?;
                let id = ResourceId::from_raw(raw);
                self.model.resources.insert(id, Resource { id, provider });
                self.next_entity = next;
                self.model.generation = generation;
                Ok(Change::Created(EntityId::Resource(id)))
            }
            Command::CreateView {
                frame,
                resource,
                index,
            } => {
                let count = self.frame(frame)?.views.len();
                index_check(index, count)?;
                let provider = self
                    .model
                    .resources
                    .get(&resource)
                    .ok_or(Error::Missing(EntityId::Resource(resource)))?
                    .provider;
                if !self.model.providers[&provider].supports_shared_views
                    && self.model.views.values().any(|v| v.resource == resource)
                {
                    return Err(Error::SharingDenied(resource));
                }
                budget_check(
                    Budget::ViewsPerFrame,
                    self.model.limits.max_views_per_frame,
                    count + 1,
                )?;
                let (raw, next, generation) = self.creation()?;
                let id = ViewId::from_raw(raw);
                self.model.views.insert(
                    id,
                    View {
                        id,
                        frame,
                        resource,
                        group: None,
                    },
                );
                let target = self.model.frames.get_mut(&frame).unwrap();
                target.views.insert(index, id);
                if target.selected_view.is_none() {
                    target.selected_view = Some(id);
                }
                self.next_entity = next;
                self.model.generation = generation;
                Ok(Change::Created(EntityId::View(id)))
            }
            Command::CreateGroup {
                frame,
                label,
                color,
            } => {
                self.frame(frame)?;
                let (raw, next, generation) = self.creation()?;
                let id = GroupId::from_raw(raw);
                self.model.groups.insert(
                    id,
                    Group {
                        id,
                        frame,
                        label,
                        color,
                    },
                );
                self.model.frames.get_mut(&frame).unwrap().groups.push(id);
                self.next_entity = next;
                self.model.generation = generation;
                Ok(Change::Created(EntityId::Group(id)))
            }
            Command::AssignGroup { view, group } => {
                let record = self.view(view)?;
                if let Some(id) = group {
                    let target = self
                        .model
                        .groups
                        .get(&id)
                        .ok_or(Error::Missing(EntityId::Group(id)))?;
                    if target.frame != record.frame {
                        return Err(Error::GroupFrameMismatch { view, group: id });
                    }
                }
                if record.group == group {
                    return Ok(Change::Unchanged);
                }
                let generation = self.next_generation()?;
                self.model.views.get_mut(&view).unwrap().group = group;
                self.model.generation = generation;
                Ok(Change::Grouped(view))
            }
            Command::SelectView { view } => {
                let frame = self.view(view)?.frame;
                if self.frame(frame)?.selected_view == Some(view) {
                    return Ok(Change::Unchanged);
                }
                let generation = self.next_generation()?;
                self.model.frames.get_mut(&frame).unwrap().selected_view = Some(view);
                self.model.generation = generation;
                Ok(Change::Selected(view))
            }
            Command::MoveFrame {
                frame,
                destination,
                index,
            } => {
                let source = self.frame(frame)?.workspace;
                let target = self.workspace(destination)?;
                let available = target.frames.len() - usize::from(source == destination);
                index_check(index, available)?;
                let mut ancestor = destination;
                while let Some(owner) = self.workspace(ancestor)?.owner {
                    if owner == frame {
                        return Err(Error::Cycle { frame, destination });
                    }
                    ancestor = self.frame(owner)?.workspace;
                }
                let old_index = self
                    .workspace(source)?
                    .frames
                    .iter()
                    .position(|id| *id == frame)
                    .unwrap();
                if source == destination && index == old_index {
                    return Ok(Change::Unchanged);
                }
                let old_depth = self.frame_depth(frame)?;
                let new_depth = self
                    .workspace_depth(destination)?
                    .checked_add(1)
                    .ok_or(Error::Exhausted(Counter::Depth))?;
                if new_depth > old_depth {
                    let deepest = new_depth
                        .checked_add(self.subtree_height(frame)?)
                        .ok_or(Error::Exhausted(Counter::Depth))?;
                    budget_check(Budget::Depth, self.model.limits.max_depth, deepest)?;
                }
                let generation = self.next_generation()?;
                self.model
                    .workspaces
                    .get_mut(&source)
                    .unwrap()
                    .frames
                    .remove(old_index);
                self.model
                    .workspaces
                    .get_mut(&destination)
                    .unwrap()
                    .frames
                    .insert(index, frame);
                self.model.frames.get_mut(&frame).unwrap().workspace = destination;
                self.model.generation = generation;
                Ok(Change::Moved(EntityId::Frame(frame)))
            }
            Command::MoveView {
                view,
                destination,
                index,
            } => {
                let source = self.view(view)?.frame;
                let count = self.frame(destination)?.views.len();
                index_check(index, count - usize::from(source == destination))?;
                let old_index = self
                    .frame(source)?
                    .views
                    .iter()
                    .position(|id| *id == view)
                    .unwrap();
                if source == destination && old_index == index {
                    return Ok(Change::Unchanged);
                }
                if source != destination {
                    budget_check(
                        Budget::ViewsPerFrame,
                        self.model.limits.max_views_per_frame,
                        count + 1,
                    )?;
                }
                let generation = self.next_generation()?;
                self.model
                    .frames
                    .get_mut(&source)
                    .unwrap()
                    .views
                    .remove(old_index);
                self.model
                    .frames
                    .get_mut(&destination)
                    .unwrap()
                    .views
                    .insert(index, view);
                if source != destination {
                    let origin = self.model.frames.get_mut(&source).unwrap();
                    if origin.selected_view == Some(view) {
                        origin.selected_view = origin.views.first().copied();
                    }
                    let target = self.model.frames.get_mut(&destination).unwrap();
                    if target.selected_view.is_none() {
                        target.selected_view = Some(view);
                    }
                    let record = self.model.views.get_mut(&view).unwrap();
                    record.frame = destination;
                    record.group = None;
                }
                self.model.generation = generation;
                Ok(Change::Moved(EntityId::View(view)))
            }
            Command::SetLimits(limits) => {
                if limits == self.model.limits {
                    return Ok(Change::Unchanged);
                }
                let generation = self.next_generation()?;
                self.model.limits = limits;
                self.model.generation = generation;
                Ok(Change::LimitsChanged)
            }
            _ => unreachable!("layout command routed through arrangement transaction"),
        }
    }
}
