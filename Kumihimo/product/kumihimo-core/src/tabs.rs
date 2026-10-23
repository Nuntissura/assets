use crate::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderAvailability {
    #[default]
    Available,
    Missing,
    Incompatible,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewState {
    pub visible: bool,
    pub usable: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct ClosedView {
    pub index: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct PinPolicy {
    pub deny_close: bool,
    pub deny_reorder: bool,
    pub deny_cross_frame_move: bool,
}

fn admit(budget: Budget, limit: Option<usize>, attempted: usize) -> Result<(), Error> {
    if let Some(limit) = limit {
        if attempted > limit {
            return Err(Error::Admission {
                budget,
                limit,
                attempted,
            });
        }
    }
    Ok(())
}
impl Core {
    pub(crate) fn check_label(&self, value: &str) -> Result<(), Error> {
        admit(
            Budget::LabelBytes,
            self.model.limits.max_label_bytes,
            value.len(),
        )
    }
    pub(crate) fn check_key(&self, value: &str) -> Result<(), Error> {
        if value.is_empty() {
            return Err(Error::InvalidState("empty durable key"));
        }
        admit(
            Budget::KeyBytes,
            self.model.limits.max_key_bytes,
            value.len(),
        )
    }
    pub(crate) fn check_pin_move(
        &self,
        id: ViewId,
        destination: FrameId,
        reorder: bool,
    ) -> Result<(), Error> {
        let view = self
            .model
            .views
            .get(&id)
            .ok_or(Error::Missing(EntityId::View(id)))?;
        if view.pinned
            && ((view.frame != destination && self.model.pin_policy.deny_cross_frame_move)
                || (view.frame == destination && reorder && self.model.pin_policy.deny_reorder))
        {
            return Err(Error::Pinned(id));
        }
        Ok(())
    }
    pub(crate) fn tab_command(&mut self, command: Command) -> Result<Change, Error> {
        match command {
            Command::SetViewMetadata {
                view,
                label,
                pinned,
                unsaved,
            } => {
                let old = self
                    .model
                    .views
                    .get(&view)
                    .ok_or(Error::Missing(EntityId::View(view)))?;
                if old.label == label && old.pinned == pinned && old.unsaved == unsaved {
                    return Ok(Change::Unchanged);
                }
                if label.len() > old.label.len() {
                    self.check_label(&label)?;
                }
                let generation = self.next_generation()?;
                let v = self.model.views.get_mut(&view).unwrap();
                v.label = label;
                v.pinned = pinned;
                v.unsaved = unsaved;
                self.model.generation = generation;
                return Ok(Change::TabChanged(view));
            }
            Command::SetViewState { view, state } => {
                let v = self
                    .model
                    .views
                    .get(&view)
                    .ok_or(Error::Missing(EntityId::View(view)))?;
                if v.state == state {
                    return Ok(Change::Unchanged);
                }
                let generation = self.next_generation()?;
                self.model.views.get_mut(&view).unwrap().state = state;
                self.model.generation = generation;
                return Ok(Change::TabChanged(view));
            }
            Command::CloseView { view, approved } => {
                let v = self
                    .model
                    .views
                    .get(&view)
                    .ok_or(Error::Missing(EntityId::View(view)))?;
                if v.closed.is_some() {
                    return Ok(Change::Unchanged);
                }
                if !approved {
                    return Err(Error::CloseVeto(view));
                }
                if v.pinned && self.model.pin_policy.deny_close {
                    return Err(Error::Pinned(view));
                }
                let frame = v.frame;
                let count = self
                    .model
                    .views
                    .values()
                    .filter(|v| v.closed.is_some())
                    .count();
                admit(
                    Budget::ClosedViews,
                    self.model.limits.max_closed_views,
                    count
                        .checked_add(1)
                        .ok_or(Error::InvalidState("closed count overflow"))?,
                )?;
                let index = self.model.frames[&frame]
                    .views
                    .iter()
                    .position(|id| *id == view)
                    .ok_or(Error::InvalidState("open membership missing"))?;
                let generation = self.next_generation()?;
                let f = self.model.frames.get_mut(&frame).unwrap();
                f.views.remove(index);
                if f.selected_view == Some(view) {
                    f.selected_view = f.views.first().copied();
                }
                let v = self.model.views.get_mut(&view).unwrap();
                v.closed = Some(ClosedView { index });
                v.state = ViewState::default();
                self.model.generation = generation;
                return Ok(Change::TabChanged(view));
            }
            Command::ReopenView { view, destination } => {
                let v = self
                    .model
                    .views
                    .get(&view)
                    .ok_or(Error::Missing(EntityId::View(view)))?;
                let closed = match v.closed {
                    Some(closed) => closed,
                    None if v.frame == destination => return Ok(Change::Unchanged),
                    None => return Err(Error::InvalidState("reopen requires closed view")),
                };
                self.check_pin_move(view, destination, false)?;
                let f = self
                    .model
                    .frames
                    .get(&destination)
                    .ok_or(Error::Missing(EntityId::Frame(destination)))?;
                admit(
                    Budget::ViewsPerFrame,
                    self.model.limits.max_views_per_frame,
                    f.views
                        .len()
                        .checked_add(1)
                        .ok_or(Error::InvalidState("view count overflow"))?,
                )?;
                let resource = &self.model.resources[&v.resource];
                if !self.model.providers[&resource.provider].supports_shared_views
                    && self.model.views.values().any(|other| {
                        other.id != view && other.resource == v.resource && other.closed.is_none()
                    })
                {
                    return Err(Error::SharingDenied(v.resource));
                }
                let index = closed.index.min(f.views.len());
                let same_frame = v.frame == destination;
                let generation = self.next_generation()?;
                let v = self.model.views.get_mut(&view).unwrap();
                v.frame = destination;
                v.closed = None;
                v.state = ViewState::default();
                if !same_frame {
                    v.group = None;
                }
                let f = self.model.frames.get_mut(&destination).unwrap();
                f.views.insert(index, view);
                if f.selected_view.is_none() {
                    f.selected_view = Some(view);
                }
                self.model.generation = generation;
                return Ok(Change::TabChanged(view));
            }
            Command::ConfigureGroup {
                group,
                label,
                color,
                description,
                collapsed,
            } => {
                let old = self
                    .model
                    .groups
                    .get(&group)
                    .ok_or(Error::Missing(EntityId::Group(group)))?;
                if old.label == label
                    && old.color == color
                    && old.description == description
                    && old.collapsed == collapsed
                {
                    return Ok(Change::Unchanged);
                }
                if label.len() > old.label.len() {
                    self.check_label(&label)?;
                }
                if description.len() > old.description.len() {
                    self.check_label(&description)?;
                }
                if color.as_ref().map_or(0, String::len) > old.color.as_ref().map_or(0, String::len)
                {
                    if let Some(c) = &color {
                        self.check_label(c)?;
                    }
                }
                let generation = self.next_generation()?;
                let g = self.model.groups.get_mut(&group).unwrap();
                g.label = label;
                g.color = color;
                g.description = description;
                g.collapsed = collapsed;
                self.model.generation = generation;
                return Ok(Change::GroupChanged(group));
            }
            Command::MoveGroup {
                group,
                destination,
                index,
                group_index,
            } => {
                return self.move_group(group, destination, index, group_index);
            }
            Command::SetPinPolicy(policy) => {
                if policy == self.model.pin_policy {
                    return Ok(Change::Unchanged);
                }
                let generation = self.next_generation()?;
                self.model.pin_policy = policy;
                self.model.generation = generation;
                return Ok(Change::PinPolicyChanged);
            }
            Command::SetProviderAvailability {
                provider,
                availability,
            } => {
                let p = self
                    .model
                    .providers
                    .get(&provider)
                    .ok_or(Error::Missing(EntityId::Provider(provider)))?;
                if p.availability == availability {
                    return Ok(Change::Unchanged);
                }
                let generation = self.next_generation()?;
                self.model
                    .providers
                    .get_mut(&provider)
                    .unwrap()
                    .availability = availability;
                self.model.generation = generation;
                return Ok(Change::ProviderChanged(provider));
            }
            Command::SetResourceReference {
                resource,
                restore_reference,
            } => {
                let r = self
                    .model
                    .resources
                    .get(&resource)
                    .ok_or(Error::Missing(EntityId::Resource(resource)))?;
                if r.restore_reference == restore_reference {
                    return Ok(Change::Unchanged);
                }
                if restore_reference.len() > r.restore_reference.len() {
                    self.check_label(&restore_reference)?;
                }
                let generation = self.next_generation()?;
                self.model
                    .resources
                    .get_mut(&resource)
                    .unwrap()
                    .restore_reference = restore_reference;
                self.model.generation = generation;
                return Ok(Change::ResourceChanged(resource));
            }
            _ => return Err(Error::InvalidState("not a tab command")),
        }
    }
    fn move_group(
        &mut self,
        group: GroupId,
        destination: FrameId,
        index: usize,
        group_index: usize,
    ) -> Result<Change, Error> {
        let source = self
            .model
            .groups
            .get(&group)
            .ok_or(Error::Missing(EntityId::Group(group)))?
            .frame;
        let source_frame = &self.model.frames[&source];
        let target = self
            .model
            .frames
            .get(&destination)
            .ok_or(Error::Missing(EntityId::Frame(destination)))?;
        let members: Vec<_> = source_frame
            .views
            .iter()
            .copied()
            .filter(|id| self.model.views[id].group == Some(group))
            .collect();
        let mut source_views: Vec<_> = source_frame
            .views
            .iter()
            .copied()
            .filter(|id| !members.contains(id))
            .collect();
        let mut target_views = if source == destination {
            source_views.clone()
        } else {
            target.views.clone()
        };
        if index > target_views.len() {
            return Err(Error::InvalidIndex {
                index,
                available: target_views.len(),
            });
        }
        target_views.splice(index..index, members.iter().copied());
        let mut source_groups: Vec<_> = source_frame
            .groups
            .iter()
            .copied()
            .filter(|id| *id != group)
            .collect();
        let mut target_groups = if source == destination {
            source_groups.clone()
        } else {
            target.groups.clone()
        };
        if group_index > target_groups.len() {
            return Err(Error::InvalidIndex {
                index: group_index,
                available: target_groups.len(),
            });
        }
        target_groups.insert(group_index, group);
        if source == destination
            && source_frame.views == target_views
            && source_frame.groups == target_groups
        {
            return Ok(Change::Unchanged);
        }
        let reordered = source_frame.views != target_views;
        for v in self.model.views.values().filter(|v| v.group == Some(group)) {
            self.check_pin_move(v.id, destination, reordered)?;
        }
        if source != destination && !members.is_empty() {
            admit(
                Budget::ViewsPerFrame,
                self.model.limits.max_views_per_frame,
                target_views.len(),
            )?;
        }
        let generation = self.next_generation()?;
        if source == destination {
            let f = self.model.frames.get_mut(&source).unwrap();
            f.views = target_views;
            f.groups = target_groups;
        } else {
            let f = self.model.frames.get_mut(&source).unwrap();
            f.views = std::mem::take(&mut source_views);
            f.groups = std::mem::take(&mut source_groups);
            if f.selected_view.is_some_and(|id| members.contains(&id)) {
                f.selected_view = f.views.first().copied();
            }
            let f = self.model.frames.get_mut(&destination).unwrap();
            f.views = target_views;
            f.groups = target_groups;
            if f.selected_view.is_none() {
                f.selected_view = members.first().copied();
            }
            for v in self
                .model
                .views
                .values_mut()
                .filter(|v| v.group == Some(group))
            {
                v.frame = destination;
            }
            self.model.groups.get_mut(&group).unwrap().frame = destination;
        }
        self.model.generation = generation;
        Ok(Change::GroupChanged(group))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabOrder {
    Canonical,
    Label,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabQuery {
    pub frame: Option<FrameId>,
    pub include_open: bool,
    pub include_closed: bool,
    pub search: Option<String>,
    pub order: TabOrder,
    pub offset: usize,
    pub limit: usize,
}
impl Default for TabQuery {
    fn default() -> Self {
        Self {
            frame: None,
            include_open: true,
            include_closed: false,
            search: None,
            order: TabOrder::Canonical,
            offset: 0,
            limit: 256,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabRow {
    pub id: ViewId,
    pub frame: FrameId,
    pub resource: ResourceId,
    pub label: String,
    pub group: Option<GroupId>,
    pub group_label: Option<String>,
    pub group_description: Option<String>,
    pub group_color: Option<String>,
    pub selected: bool,
    pub closed: bool,
    pub pinned: bool,
    pub unsaved: bool,
    pub placeholder: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabPage {
    pub generation: u64,
    pub identity: Identity,
    pub total: usize,
    pub matched: usize,
    pub rows: Vec<TabRow>,
}
impl Snapshot {
    pub fn tab_page(&self, query: &TabQuery) -> Result<TabPage, Error> {
        admit(Budget::TabPage, self.limits.max_tab_page_size, query.limit)?;
        if let Some(frame) = query.frame {
            if !self.frames.contains_key(&frame) {
                return Err(Error::Missing(EntityId::Frame(frame)));
            }
        }
        if let Some(search) = &query.search {
            admit(
                Budget::LabelBytes,
                self.limits.max_label_bytes,
                search.len(),
            )?;
        }
        let search = query.search.as_ref().map(|s| s.to_lowercase());
        let mut views: Vec<_> = self
            .views
            .values()
            .filter(|v| {
                query.frame.is_none_or(|f| v.frame == f)
                    && if v.closed.is_some() {
                        query.include_closed
                    } else {
                        query.include_open
                    }
            })
            .collect();
        let total = views.len();
        views.retain(|v| {
            search.as_ref().is_none_or(|s| {
                v.label.to_lowercase().contains(s)
                    || self.resources[&v.resource].key.to_lowercase().contains(s)
                    || v.group
                        .and_then(|g| self.groups.get(&g))
                        .is_some_and(|g| g.label.to_lowercase().contains(s))
            })
        });
        match query.order {
            TabOrder::Label => views.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id))),
            TabOrder::Canonical => views.sort_by_key(|v| {
                (
                    v.frame,
                    v.closed.is_some(),
                    v.closed.map(|c| c.index).unwrap_or_else(|| {
                        self.frames[&v.frame]
                            .views
                            .iter()
                            .position(|id| *id == v.id)
                            .unwrap_or(usize::MAX)
                    }),
                    v.id,
                )
            }),
        }
        let matched = views.len();
        let rows = views
            .into_iter()
            .skip(query.offset)
            .take(query.limit)
            .map(|v| {
                let g = v.group.and_then(|id| self.groups.get(&id));
                let r = &self.resources[&v.resource];
                TabRow {
                    id: v.id,
                    frame: v.frame,
                    resource: v.resource,
                    label: if v.label.is_empty() {
                        r.key.clone()
                    } else {
                        v.label.clone()
                    },
                    group: v.group,
                    group_label: g.map(|g| g.label.clone()),
                    group_description: g.map(|g| g.description.clone()),
                    group_color: g.and_then(|g| g.color.clone()),
                    selected: self.frames[&v.frame].selected_view == Some(v.id),
                    closed: v.closed.is_some(),
                    pinned: v.pinned,
                    unsaved: v.unsaved,
                    placeholder: self.providers[&r.provider].availability
                        != ProviderAvailability::Available,
                }
            })
            .collect();
        Ok(TabPage {
            generation: self.generation,
            identity: self.identity.clone(),
            total,
            matched,
            rows,
        })
    }
}
