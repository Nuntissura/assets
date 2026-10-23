use crate::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FloatRepair {
    #[default]
    Reject,
    Clamp,
}
/// Supplied host data only: resolution never loads an engine or interprets a reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderResolution {
    pub key: String,
    pub supports_shared_views: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreBounds {
    pub workspace: WorkspaceId,
    pub viewport: Rect,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreOptions {
    /// Explicit host admission ceiling; saved limits are intersected with this ceiling.
    pub limits: Limits,
    pub providers: Vec<ProviderResolution>,
    pub bounds: Vec<RestoreBounds>,
    pub float_repair: FloatRepair,
}
/// Owns the complete validated candidate and host policy; fields cannot be altered by consumers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedRestore {
    source: PersistedLayout,
    options: RestoreOptions,
    target: Identity,
    generation: u64,
    candidate: Snapshot,
    next_entity: u64,
}
impl PreparedRestore {
    pub fn identity(&self) -> &Identity {
        &self.target
    }
    pub fn source(&self) -> &PersistedLayout {
        &self.source
    }
    pub fn options(&self) -> &RestoreOptions {
        &self.options
    }
    pub fn target_identity(&self) -> &Identity {
        &self.target
    }
    pub fn target_generation(&self) -> u64 {
        self.generation
    }
}
fn invalid(s: &'static str) -> Error {
    Error::InvalidState(s)
}
fn budget(kind: Budget, limit: Option<usize>, n: usize) -> Result<(), Error> {
    if let Some(limit) = limit {
        if n > limit {
            return Err(Error::Admission {
                budget: kind,
                limit,
                attempted: n,
            });
        }
    }
    Ok(())
}
fn ceiling(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}
fn limits(a: Limits, b: Limits) -> Limits {
    Limits {
        max_records: ceiling(a.max_records, b.max_records),
        max_depth: ceiling(a.max_depth, b.max_depth),
        max_views_per_frame: ceiling(a.max_views_per_frame, b.max_views_per_frame),
        max_layout_entries: ceiling(a.max_layout_entries, b.max_layout_entries),
        max_closed_views: ceiling(a.max_closed_views, b.max_closed_views),
        max_tab_page_size: ceiling(a.max_tab_page_size, b.max_tab_page_size),
        max_label_bytes: ceiling(a.max_label_bytes, b.max_label_bytes),
        max_key_bytes: ceiling(a.max_key_bytes, b.max_key_bytes),
    }
}
fn string(s: &str, limit: Option<usize>, kind: Budget) -> Result<(), Error> {
    budget(kind, limit, s.len())
}
fn dimension(limits: Limits, kind: Budget) -> Option<usize> {
    match kind {
        Budget::Records => limits.max_records,
        Budget::Depth => limits.max_depth,
        Budget::ViewsPerFrame => limits.max_views_per_frame,
        Budget::LayoutEntries => limits.max_layout_entries,
        Budget::ClosedViews => limits.max_closed_views,
        Budget::TabPage => limits.max_tab_page_size,
        Budget::LabelBytes => limits.max_label_bytes,
        Budget::KeyBytes => limits.max_key_bytes,
    }
}
struct Admission<'a> {
    limits: Limits,
    baseline: Option<&'a Snapshot>,
}
impl Admission<'_> {
    fn check(&self, kind: Budget, n: usize, prior: Option<usize>) -> Result<(), Error> {
        let limit = dimension(self.limits, kind);
        if self
            .baseline
            .is_some_and(|old| dimension(old.limits, kind) == limit)
            && prior.is_some_and(|old| n <= old)
        {
            return Ok(());
        }
        budget(kind, limit, n)
    }
    fn text(&self, kind: Budget, value: &str, prior: Option<&str>) -> Result<(), Error> {
        self.check(kind, value.len(), prior.map(str::len))
    }
}
fn unique<T: Ord + Copy>(values: &[T]) -> bool {
    values.iter().copied().collect::<BTreeSet<_>>().len() == values.len()
}

impl Core {
    /// Same-ceiling live overage may stay unchanged or decrease. Per-frame, depth and
    /// string allowance belongs to the same entity/field; stricter ceilings stay strict.
    pub fn prepare_restore(
        &self,
        source: PersistedLayout,
        options: RestoreOptions,
    ) -> Result<PreparedRestore, Error> {
        if source.source.lineage != self.model.identity.lineage {
            return Err(Error::IdentityMismatch);
        }
        let mut candidate = build(
            &source,
            &options,
            self.model.identity.clone(),
            Some(&self.model),
        )?;
        immutable(&self.model, &candidate)?;
        candidate.generation = self.next_generation()?;
        Ok(PreparedRestore {
            next_entity: self.next_entity.max(source.next_entity),
            source,
            options,
            target: self.model.identity.clone(),
            generation: self.model.generation,
            candidate,
        })
    }
    pub(crate) fn restore_command(&mut self, token: PreparedRestore) -> Result<Change, Error> {
        if token.target != self.model.identity {
            return Err(Error::IdentityMismatch);
        }
        if token.generation != self.model.generation {
            return Err(Error::RestoreConflict);
        }
        // The token owns all inputs and was validated without touching this Core's ledger.
        self.model = token.candidate;
        self.next_entity = token.next_entity;
        Ok(Change::Restored)
    }
    /// Host must guarantee incarnation freshness across *all* earlier instances, not just this source.
    pub fn from_persisted(
        source: PersistedLayout,
        fresh_incarnation: String,
        options: RestoreOptions,
        receipt_capacity: usize,
    ) -> Result<Self, Error> {
        if fresh_incarnation == source.source.incarnation {
            return Err(Error::IdentityMismatch);
        }
        let identity = Identity::new(source.source.lineage.clone(), fresh_incarnation)?;
        let mut model = build(&source, &options, identity.clone(), None)?;
        model.generation = 1;
        let mut core = Core::new(identity, model.limits, receipt_capacity)?;
        core.model = model;
        core.next_entity = source.next_entity;
        Ok(core)
    }
}
fn build(
    d: &PersistedLayout,
    o: &RestoreOptions,
    identity: Identity,
    baseline: Option<&Snapshot>,
) -> Result<Snapshot, Error> {
    if d.schema != SAVED_SCHEMA {
        return Err(Error::UnsupportedSchema {
            found: d.schema,
            supported: SAVED_SCHEMA,
        });
    }
    d.source.validate()?;
    let mut s = Snapshot::new(identity, limits(d.limits, o.limits));
    let admission = Admission {
        limits: s.limits,
        baseline,
    };
    s.pin_policy = d.pin_policy;
    let mut raw = BTreeSet::new();
    macro_rules! id {
        ($id:expr) => {{
            let n = $id.raw();
            if n == 0 || n == u64::MAX || !raw.insert(n) {
                return Err(invalid("invalid or duplicate generated ID"));
            }
        }};
    }
    macro_rules! put {
        ($field:ident,$key:expr,$value:expr) => {
            if s.$field.insert($key, $value).is_some() {
                return Err(invalid("duplicate logical entry"));
            }
        };
    }
    let mut keys = BTreeSet::new();
    let mut resolved = BTreeMap::new();
    for p in &o.providers {
        string(&p.key, s.limits.max_key_bytes, Budget::KeyBytes)?;
        if p.key.is_empty()
            || resolved
                .insert(p.key.clone(), p.supports_shared_views)
                .is_some()
        {
            return Err(invalid("invalid resolution key"));
        }
    }
    for v in &d.workspaces {
        id!(v.id);
        put!(
            workspaces,
            v.id,
            Workspace {
                id: v.id,
                owner: v.owner,
                frames: v.frames.clone()
            }
        );
    }
    for v in &d.frames {
        id!(v.id);
        put!(
            frames,
            v.id,
            Frame {
                id: v.id,
                workspace: v.workspace,
                child_workspace: v.child_workspace,
                views: v.views.clone(),
                selected_view: v.selected_view,
                groups: v.groups.clone()
            }
        );
    }
    for v in &d.providers {
        id!(v.id);
        admission.text(
            Budget::KeyBytes,
            &v.key,
            baseline
                .and_then(|s| s.providers.get(&v.id))
                .map(|p| p.key.as_str()),
        )?;
        if v.key.is_empty() || !keys.insert(v.key.clone()) {
            return Err(invalid("invalid provider key"));
        }
        let availability = match resolved.get(&v.key) {
            None => ProviderAvailability::Missing,
            Some(shared) if *shared != v.supports_shared_views => {
                ProviderAvailability::Incompatible
            }
            _ => ProviderAvailability::Available,
        };
        put!(
            providers,
            v.id,
            Provider {
                id: v.id,
                key: v.key.clone(),
                supports_shared_views: v.supports_shared_views,
                availability
            }
        );
    }
    keys.clear();
    for v in &d.resources {
        id!(v.id);
        let prior = baseline.and_then(|s| s.resources.get(&v.id));
        admission.text(Budget::KeyBytes, &v.key, prior.map(|r| r.key.as_str()))?;
        admission.text(
            Budget::LabelBytes,
            &v.restore_reference,
            prior.map(|r| r.restore_reference.as_str()),
        )?;
        if v.key.is_empty() || !keys.insert(format!("{}:{}", v.provider.raw(), v.key)) {
            return Err(invalid("invalid resource key"));
        }
        put!(
            resources,
            v.id,
            Resource {
                id: v.id,
                provider: v.provider,
                key: v.key.clone(),
                restore_reference: v.restore_reference.clone()
            }
        );
    }
    for v in &d.views {
        id!(v.id);
        admission.text(
            Budget::LabelBytes,
            &v.label,
            baseline
                .and_then(|s| s.views.get(&v.id))
                .map(|v| v.label.as_str()),
        )?;
        put!(
            views,
            v.id,
            View {
                id: v.id,
                frame: v.frame,
                resource: v.resource,
                group: v.group,
                label: v.label.clone(),
                pinned: v.pinned,
                unsaved: v.unsaved,
                closed: v.closed,
                state: ViewState::default()
            }
        );
    }
    for v in &d.groups {
        id!(v.id);
        let prior = baseline.and_then(|s| s.groups.get(&v.id));
        admission.text(
            Budget::LabelBytes,
            &v.label,
            prior.map(|g| g.label.as_str()),
        )?;
        admission.text(
            Budget::LabelBytes,
            &v.description,
            prior.map(|g| g.description.as_str()),
        )?;
        if let Some(color) = &v.color {
            admission.text(
                Budget::LabelBytes,
                color,
                prior.and_then(|g| g.color.as_deref()),
            )?;
        }
        put!(
            groups,
            v.id,
            Group {
                id: v.id,
                frame: v.frame,
                label: v.label.clone(),
                color: v.color.clone(),
                description: v.description.clone(),
                collapsed: v.collapsed
            }
        );
    }
    for v in &d.tools {
        id!(v.id);
        put!(
            tools,
            v.id,
            Tool {
                id: v.id,
                owner: v.owner,
                origin: v.origin,
                target: v.target,
                placement: v.placement,
                policy: v.policy.clone(),
                state: TargetState::default()
            }
        );
    }
    if d.next_entity == 0 || raw.last().is_some_and(|id| *id >= d.next_entity) {
        return Err(invalid("allocator must exceed every generated ID"));
    }
    for v in &d.layouts {
        put!(layouts, v.workspace, v.plan.clone());
    }
    for v in &d.contexts {
        put!(contexts, v.workspace, v.context.clone());
    }
    for v in &d.floats {
        put!(floats, v.frame, v.floating.clone());
    }
    for v in &d.maximized {
        put!(maximized, v.workspace, v.frame);
    }
    validate_ownership(&s, &admission)?;
    for c in s.contexts.values() {
        for location in &c.allowed_docks {
            dock_reference(&s, *location)?;
        }
    }
    for f in s.floats.values() {
        dock_reference(&s, f.return_location)?;
    }
    let mut bounds = BTreeSet::new();
    for b in &o.bounds {
        if !bounds.insert(b.workspace) {
            return Err(invalid("duplicate restore bounds"));
        }
        let c = s
            .contexts
            .get_mut(&b.workspace)
            .ok_or(invalid("restore bounds lack context"))?;
        c.viewport = b.viewport;
    }
    let mut hidden_floats = BTreeSet::new();
    for frame in s.floats.keys() {
        let workspace = s
            .frames
            .get(frame)
            .ok_or(invalid("floating frame absent"))?
            .workspace;
        if s.maximized.contains_key(&workspace) || !s.geometry_workspace_visible(workspace) {
            hidden_floats.insert(*frame);
        }
    }
    for (frame, float) in &mut s.floats {
        let f = s
            .frames
            .get(frame)
            .ok_or(invalid("floating frame absent"))?;
        let c = s
            .contexts
            .get(&f.workspace)
            .ok_or(GeometryError::MissingContext)?;
        if !c.viewport.contains(float.rect) {
            if o.float_repair == FloatRepair::Reject {
                if hidden_floats.contains(frame) {
                    continue;
                }
                return Err(GeometryError::OutsideViewport.into());
            }
            // Hidden geometry is retained for a later explicit exit proof. Do not let its
            // minima reject an active maximize projection, even during optional repair.
            let minimum = if hidden_floats.contains(frame) {
                Size::zero()
            } else {
                c.minimums.get(frame).copied().unwrap_or(Size::zero())
            };
            if minimum.width() > c.viewport.width() || minimum.height() > c.viewport.height() {
                return Err(GeometryError::Infeasible.into());
            }
            let w = float
                .rect
                .width()
                .min(c.viewport.width())
                .max(minimum.width());
            let h = float
                .rect
                .height()
                .min(c.viewport.height())
                .max(minimum.height());
            let x = float
                .rect
                .x()
                .max(c.viewport.x())
                .min(c.viewport.right() - w);
            let y = float
                .rect
                .y()
                .max(c.viewport.y())
                .min(c.viewport.bottom() - h);
            float.rect = Rect::new(x, y, w, h)?;
        }
    }
    for (workspace, frame) in &s.maximized {
        if !s
            .frames
            .get(frame)
            .is_some_and(|f| f.workspace == *workspace)
            || s.floats.contains_key(frame)
        {
            return Err(invalid("invalid maximize target"));
        }
    }
    admission.check(
        Budget::LayoutEntries,
        s.layout_entry_count(),
        baseline.map(Snapshot::layout_entry_count),
    )?;
    let mut candidate = Core::new(s.identity.clone(), s.limits, 0)?;
    candidate.model = s;
    candidate.validate_arrangement()?;
    Ok(candidate.model)
}

fn dock_reference(s: &Snapshot, location: DockLocation) -> Result<(), Error> {
    match location {
        DockLocation::Append { workspace } | DockLocation::WorkspaceEdge { workspace, .. } => {
            if !s.workspaces.contains_key(&workspace) {
                return Err(invalid("dock workspace absent"));
            }
        }
        DockLocation::Beside { frame, .. } => {
            if !s.frames.contains_key(&frame) {
                return Err(invalid("dock frame absent"));
            }
        }
        DockLocation::Cell {
            workspace,
            grid,
            row,
            column,
        } => {
            let Some(LayoutNode::Grid { rows, columns, .. }) =
                s.layouts.get(&workspace).and_then(|p| p.nodes.get(&grid))
            else {
                return Err(invalid("dock grid absent"));
            };
            if row >= rows.len() || column >= columns.len() {
                return Err(invalid("dock cell outside grid"));
            }
        }
    }
    Ok(())
}

fn validate_ownership(s: &Snapshot, admission: &Admission<'_>) -> Result<(), Error> {
    let baseline = admission.baseline;
    admission.check(
        Budget::Records,
        s.record_count(),
        baseline.map(Snapshot::record_count),
    )?;
    admission.check(
        Budget::ClosedViews,
        s.views.values().filter(|v| v.closed.is_some()).count(),
        baseline.map(|s| s.views.values().filter(|v| v.closed.is_some()).count()),
    )?;
    let mut frames = BTreeSet::new();
    let mut views = BTreeSet::new();
    let mut groups = BTreeSet::new();
    for w in s.workspaces.values() {
        if !unique(&w.frames) {
            return Err(invalid("duplicate workspace frame"));
        }
        if let Some(owner) = w.owner {
            if s.frames.get(&owner).and_then(|f| f.child_workspace) != Some(w.id) {
                return Err(invalid("workspace owner mismatch"));
            }
        }
        for id in &w.frames {
            if !frames.insert(*id) || !s.frames.get(id).is_some_and(|f| f.workspace == w.id) {
                return Err(invalid("frame ownership mismatch"));
            }
        }
    }
    if frames.len() != s.frames.len() {
        return Err(invalid("unowned frame"));
    }
    for f in s.frames.values() {
        admission.check(
            Budget::ViewsPerFrame,
            f.views.len(),
            baseline
                .and_then(|s| s.frames.get(&f.id))
                .map(|f| f.views.len()),
        )?;
        if !unique(&f.views) || !unique(&f.groups) {
            return Err(invalid("duplicate frame member"));
        }
        if let Some(child) = f.child_workspace {
            if s.workspaces.get(&child).and_then(|w| w.owner) != Some(f.id) {
                return Err(invalid("child workspace mismatch"));
            }
        }
        if f.selected_view.is_some_and(|v| !f.views.contains(&v))
            || (f.views.is_empty() != f.selected_view.is_none())
        {
            return Err(invalid("invalid selection"));
        }
        for id in &f.views {
            if !views.insert(*id)
                || !s
                    .views
                    .get(id)
                    .is_some_and(|v| v.frame == f.id && v.closed.is_none())
            {
                return Err(invalid("open view ownership mismatch"));
            }
        }
        for id in &f.groups {
            if !groups.insert(*id) || !s.groups.get(id).is_some_and(|g| g.frame == f.id) {
                return Err(invalid("group ownership mismatch"));
            }
        }
    }
    if groups.len() != s.groups.len() {
        return Err(invalid("unowned group"));
    }
    let mut placements = BTreeMap::<ResourceId, usize>::new();
    for v in s.views.values() {
        if !s.frames.contains_key(&v.frame)
            || !s.resources.contains_key(&v.resource)
            || (v.closed.is_none() != views.contains(&v.id))
        {
            return Err(invalid("invalid view reference or membership"));
        }
        if let Some(group) = v.group {
            if !s.groups.get(&group).is_some_and(|g| g.frame == v.frame) {
                return Err(Error::GroupFrameMismatch { view: v.id, group });
            }
        }
        if v.closed.is_none() {
            *placements.entry(v.resource).or_default() += 1;
        }
    }
    for r in s.resources.values() {
        let p = s
            .providers
            .get(&r.provider)
            .ok_or(invalid("resource provider absent"))?;
        if !p.supports_shared_views && placements.get(&r.id).copied().unwrap_or(0) > 1 {
            return Err(Error::SharingDenied(r.id));
        }
    }
    let mut seen = BTreeSet::new();
    let (workspace_depths, frame_depths) = baseline.map(depth_baseline).unwrap_or_default();
    let mut stack: Vec<_> = s
        .workspaces
        .values()
        .filter(|w| w.owner.is_none())
        .map(|w| (w.id, 0usize))
        .collect();
    while let Some((id, depth)) = stack.pop() {
        if !seen.insert(id) {
            return Err(invalid("cyclic or multiply owned workspace"));
        }
        admission.check(Budget::Depth, depth, workspace_depths.get(&id).copied())?;
        let w = &s.workspaces[&id];
        for frame in &w.frames {
            let next = depth
                .checked_add(1)
                .ok_or(Error::Exhausted(Counter::Depth))?;
            admission.check(Budget::Depth, next, frame_depths.get(frame).copied())?;
            if let Some(child) = s.frames[frame].child_workspace {
                stack.push((child, next));
            }
        }
    }
    if seen.len() != s.workspaces.len() {
        return Err(invalid("cyclic ownership"));
    }
    Ok(())
}

fn depth_baseline(s: &Snapshot) -> (BTreeMap<WorkspaceId, usize>, BTreeMap<FrameId, usize>) {
    let mut workspaces = BTreeMap::new();
    let mut frames = BTreeMap::new();
    let mut pending: Vec<_> = s
        .workspaces
        .values()
        .filter(|w| w.owner.is_none())
        .map(|w| (w.id, 0usize))
        .collect();
    while let Some((id, depth)) = pending.pop() {
        if workspaces.insert(id, depth).is_some() {
            continue;
        }
        let Some(next) = depth.checked_add(1) else {
            continue;
        };
        for frame in &s.workspaces[&id].frames {
            frames.insert(*frame, next);
            if let Some(child) = s.frames[frame].child_workspace {
                pending.push((child, next));
            }
        }
    }
    (workspaces, frames)
}

fn kinds(s: &Snapshot) -> BTreeMap<u64, u8> {
    let mut m = BTreeMap::new();
    macro_rules! add {
        ($field:ident,$kind:expr) => {
            for id in s.$field.keys() {
                m.insert(id.raw(), $kind);
            }
        };
    }
    add!(workspaces, 0);
    add!(frames, 1);
    add!(providers, 2);
    add!(resources, 3);
    add!(views, 4);
    add!(groups, 5);
    add!(tools, 6);
    m
}
fn immutable(old: &Snapshot, new: &Snapshot) -> Result<(), Error> {
    let before = kinds(old);
    for (id, kind) in kinds(new) {
        if before.get(&id).is_some_and(|k| *k != kind) {
            return Err(invalid("extant entity kind changed"));
        }
    }
    for (id, p) in &new.providers {
        if old
            .providers
            .get(id)
            .is_some_and(|v| v.key != p.key || v.supports_shared_views != p.supports_shared_views)
        {
            return Err(invalid("extant provider identity changed"));
        }
    }
    for (id, r) in &new.resources {
        if old
            .resources
            .get(id)
            .is_some_and(|v| v.key != r.key || v.provider != r.provider)
        {
            return Err(invalid("extant resource identity changed"));
        }
    }
    for (id, v) in &new.views {
        if old.views.get(id).is_some_and(|o| o.resource != v.resource) {
            return Err(invalid("extant view resource changed"));
        }
    }
    Ok(())
}
