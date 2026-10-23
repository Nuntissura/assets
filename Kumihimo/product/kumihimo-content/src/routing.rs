use crate::*;
use kumihimo_core::{
    Command, Core, Envelope, FrameId, LayoutPlan, ProviderAvailability, ProviderId, ResourceId,
    Snapshot, ViewId, WorkspaceId,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

struct FrameMovement {
    frame: FrameId,
    source: WorkspaceId,
    destination: WorkspaceId,
    old_index: usize,
    new_index: usize,
    floating: bool,
    source_plan: Option<LayoutPlan>,
    destination_plan: Option<LayoutPlan>,
}
#[derive(Clone, Copy, Default)]
struct MovementImpact {
    direct: bool,
    source_packing: bool,
    destination_packing: bool,
}
impl MovementImpact {
    fn changed(self, packing: (bool, bool)) -> bool {
        self.direct || (self.source_packing && packing.0) || (self.destination_packing && packing.1)
    }
}
impl FrameMovement {
    fn capture(snap: &Snapshot, command: &Command) -> Option<Self> {
        let Command::MoveFrame {
            frame,
            destination,
            index,
        } = command
        else {
            return None;
        };
        let source = snap.frames.get(frame)?.workspace;
        let old_index = snap
            .workspaces
            .get(&source)?
            .frames
            .iter()
            .position(|id| id == frame)?;
        Some(Self {
            frame: *frame,
            source,
            destination: *destination,
            old_index,
            new_index: *index,
            floating: snap.floats.contains_key(frame),
            source_plan: snap.layouts.get(&source).cloned(),
            destination_plan: (source != *destination)
                .then(|| snap.layouts.get(destination).cloned())
                .flatten(),
        })
    }
    fn impact(&self, snap: &Snapshot, target: MountTarget) -> MovementImpact {
        let mut impact = MovementImpact::default();
        let mut anchor = State::target_frame(snap, target).map(|(frame, _)| frame);
        while let Some(frame) = anchor {
            let Some(record) = snap.frames.get(&frame) else {
                break;
            };
            let Some(workspace) = snap.workspaces.get(&record.workspace) else {
                break;
            };
            if frame == self.frame {
                impact.direct = true;
            } else if !snap.floats.contains_key(&frame) {
                let source = record.workspace == self.source;
                let destination = record.workspace == self.destination;
                let retained = if source {
                    self.source_plan.is_some()
                } else if destination {
                    self.destination_plan.is_some()
                } else {
                    false
                };
                if retained {
                    impact.source_packing |= source;
                    impact.destination_packing |= destination;
                } else if source || destination {
                    // Membership changes alter default allocation; a pure reorder only shifts
                    // the inclusive interval between its old and final insertion positions.
                    impact.direct |= self.source != self.destination
                        || self.floating
                        || workspace
                            .frames
                            .iter()
                            .position(|id| *id == frame)
                            .is_some_and(|index| {
                                (self.old_index.min(self.new_index)
                                    ..=self.old_index.max(self.new_index))
                                    .contains(&index)
                            });
                }
            }
            anchor = workspace.owner;
        }
        impact
    }
    fn packing_changed(&self, current: &Snapshot) -> (bool, bool) {
        let source = self.source_plan.as_ref() != current.layouts.get(&self.source);
        let destination = if self.source == self.destination {
            source
        } else {
            self.destination_plan.as_ref() != current.layouts.get(&self.destination)
        };
        (source, destination)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachOptions {
    pub mode: ContentMode,
    pub share: Option<InstanceId>,
    pub host_share: bool,
}
impl Default for AttachOptions {
    fn default() -> Self {
        Self {
            mode: ContentMode::Preview,
            share: None,
            host_share: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderConnection {
    pub epoch: u64,
    pub capabilities: Capabilities,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountStatus {
    Pending,
    Attached,
    Retired,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub id: MountId,
    pub instance: InstanceId,
    pub scope: Scope,
    pub mode: ContentMode,
    pub active: bool,
    pub reserved: bool,
    pub status: MountStatus,
    pub requested: Option<LifecycleState>,
    pub confirmed: Observation<LifecycleState>,
    pub restoration: RestorationStrength,
    pub eligibility: Eligibility,
    pub signals: Signals,
    pub desired_geometry: u64,
    pub confirmed_geometry: u64,
    pub desired_focus: u64,
    pub confirmed_focus: u64,
    pub geometry: Option<Geometry>,
    pub animation_deadline: Option<u64>,
    pub invalidated: bool,
    pub(crate) recovery: Option<PendingRequest>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub id: InstanceId,
    pub resource: ResourceId,
    pub provider: ProviderId,
    pub mounts: BTreeSet<MountId>,
    pub reserved: bool,
    pub started: bool,
    pub retired: bool,
    pub busy: Option<RequestId>,
    pub scope: Scope,
    pub capabilities: Capabilities,
    pub has_restore_reference: bool,
    pub(crate) recovery: Option<PendingRequest>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRequest {
    pub request: Request,
    pub status: RequestStatus,
    pub stale: bool,
    pub geometry: u64,
    pub focus: u64,
    pub eligibility_revision: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    #[cfg(feature = "navigation")]
    pub overview_entries: usize,
    pub providers: usize,
    pub mounts: usize,
    pub instances: usize,
    pub leases: usize,
    pub pending: usize,
    pub queue: usize,
    pub receipts: usize,
    pub retained_bytes: usize,
    pub cache_entries: usize,
    pub cache_bytes: usize,
    /// Unknown declared byte amounts are counted separately; cache_bytes is the known subtotal.
    pub cache_unknown_reservations: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentSnapshot {
    #[cfg(feature = "navigation")]
    pub navigation: BTreeMap<MountId, NavigationState>,
    #[cfg(feature = "backgrounds")]
    pub backgrounds: BTreeMap<MountId, BackgroundState>,
    #[cfg(feature = "backgrounds")]
    pub media: BTreeMap<InstanceId, MediaState>,
    pub providers: BTreeMap<ProviderId, ProviderConnection>,
    pub mounts: BTreeMap<MountId, Mount>,
    pub instances: BTreeMap<InstanceId, Instance>,
    pub pending: BTreeMap<RequestId, PendingRequest>,
    pub queue: Vec<RequestId>,
    pub receipts: Vec<Receipt>,
    pub caches: BTreeMap<CacheId, CacheReservation>,
    pub counts: Counts,
    pub now: u64,
    pub next_id: u64,
    pub limits: ContentLimits,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceholderReason {
    MissingProvider,
    ProviderUnavailable,
    NotNegotiated,
    NotAttached,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placeholder {
    pub view: ViewId,
    pub resource: ResourceId,
    pub label: String,
    pub restore_reference: String,
    pub reason: PlaceholderReason,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClosePermit {
    pub eligibility: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct State {
    #[cfg(feature = "navigation")]
    pub navigation: BTreeMap<MountId, NavigationState>,
    #[cfg(feature = "backgrounds")]
    pub backgrounds: BTreeMap<MountId, BackgroundState>,
    #[cfg(feature = "backgrounds")]
    pub media: BTreeMap<InstanceId, MediaState>,
    pub session: String,
    pub limits: ContentLimits,
    pub next_id: u64,
    pub now: u64,
    pub providers: BTreeMap<ProviderId, ProviderConnection>,
    pub mounts: BTreeMap<MountId, Mount>,
    pub instances: BTreeMap<InstanceId, Instance>,
    pub pending: BTreeMap<RequestId, PendingRequest>,
    pub queue: VecDeque<RequestId>,
    pub receipts: VecDeque<Receipt>,
    pub permits: BTreeMap<MountId, ClosePermit>,
    pub caches: BTreeMap<CacheId, CacheReservation>,
    pub samples: BTreeMap<(InstanceId, Metric), Sample>,
}
pub struct Controller {
    pub(crate) core: Core,
    pub(crate) state: State,
}
impl Controller {
    /// A host sequence offset supports deterministic exhaustion proof; session uniqueness still applies.
    pub fn new_with_id_start(
        core: Core,
        session: impl Into<String>,
        limits: ContentLimits,
        first_id: u64,
    ) -> Result<Self> {
        if first_id == 0 {
            return Err(Error::Invalid("initial protocol ID"));
        }
        let mut c = Self::new(core, session, limits)?;
        c.state.next_id = first_id;
        Ok(c)
    }
    pub fn new(core: Core, session: impl Into<String>, limits: ContentLimits) -> Result<Self> {
        let session = session.into();
        if session.is_empty() || session.len() > 128 {
            return Err(Error::Invalid("controller session"));
        }
        Ok(Self {
            core,
            state: State {
                #[cfg(feature = "navigation")]
                navigation: BTreeMap::new(),
                #[cfg(feature = "backgrounds")]
                backgrounds: BTreeMap::new(),
                #[cfg(feature = "backgrounds")]
                media: BTreeMap::new(),
                session,
                limits,
                next_id: 1,
                now: 0,
                providers: BTreeMap::new(),
                mounts: BTreeMap::new(),
                instances: BTreeMap::new(),
                pending: BTreeMap::new(),
                queue: VecDeque::new(),
                receipts: VecDeque::new(),
                permits: BTreeMap::new(),
                caches: BTreeMap::new(),
                samples: BTreeMap::new(),
            },
        })
    }
    pub fn core(&self) -> &Core {
        &self.core
    }
    pub fn snapshot(&self) -> &Snapshot {
        self.core.snapshot()
    }
    pub fn inspect(&self) -> ContentSnapshot {
        let s = &self.state;
        ContentSnapshot {
            #[cfg(feature = "navigation")]
            navigation: s.navigation.clone(),
            #[cfg(feature = "backgrounds")]
            backgrounds: s.backgrounds.clone(),
            #[cfg(feature = "backgrounds")]
            media: s.media.clone(),
            providers: s.providers.clone(),
            mounts: s.mounts.clone(),
            instances: s.instances.clone(),
            pending: s.pending.clone(),
            queue: s.queue.iter().copied().collect(),
            receipts: s.receipts.iter().cloned().collect(),
            caches: s.caches.clone(),
            counts: s.counts().expect("admitted accounting"),
            now: s.now,
            next_id: s.next_id,
            limits: s.limits.clone(),
        }
    }
    pub(crate) fn transaction<T>(
        &mut self,
        f: impl FnOnce(&Core, &mut State) -> Result<T>,
    ) -> Result<T> {
        let before = self.state.counts()?;
        let mut candidate = self.state.clone();
        let result = f(&self.core, &mut candidate)?;
        candidate.admit(&before)?;
        self.state = candidate;
        Ok(result)
    }
    pub fn set_limits(&mut self, limits: ContentLimits) -> Result<()> {
        self.state.limits = limits;
        Ok(())
    }
    pub fn negotiate(
        &mut self,
        provider: ProviderId,
        version: ProtocolVersion,
        host: Capabilities,
        offered: Capabilities,
    ) -> Result<u64> {
        version.validate()?;
        let capabilities = host.intersect(&offered)?;
        self.transaction(|core, s| {
            let p = core
                .snapshot()
                .providers
                .get(&provider)
                .ok_or(Error::Missing("provider", provider.raw()))?;
            if p.availability != ProviderAvailability::Available {
                return Err(Error::Stale("provider unavailable"));
            }
            let epoch = s.id()?;
            s.retire_provider(provider);
            s.providers.insert(
                provider,
                ProviderConnection {
                    epoch,
                    capabilities,
                },
            );
            Ok(epoch)
        })
    }
    pub fn placeholder(&self, view: ViewId) -> Option<Placeholder> {
        let snap = self.snapshot();
        let v = snap.views.get(&view)?;
        let r = snap.resources.get(&v.resource)?;
        let reason = match snap.providers.get(&r.provider) {
            None => PlaceholderReason::MissingProvider,
            Some(p) if p.availability != ProviderAvailability::Available => {
                PlaceholderReason::ProviderUnavailable
            }
            _ if !self.state.providers.contains_key(&r.provider) => {
                PlaceholderReason::NotNegotiated
            }
            _ => match self
                .state
                .mounts
                .values()
                .find(|m| m.scope.target == MountTarget::View(view) && m.active)
            {
                Some(m) if m.status == MountStatus::Attached => return None,
                Some(m) if m.status == MountStatus::Unknown => PlaceholderReason::Unknown,
                _ => PlaceholderReason::NotAttached,
            },
        };
        Some(Placeholder {
            view,
            resource: r.id,
            label: v.label.clone(),
            restore_reference: r.restore_reference.clone(),
            reason,
        })
    }
    pub fn attach(
        &mut self,
        view: ViewId,
        options: AttachOptions,
        permission: HostPermission,
        now: u64,
    ) -> Result<MountId> {
        let resource = self
            .snapshot()
            .views
            .get(&view)
            .ok_or(Error::Missing("view", view.raw()))?
            .resource;
        self.attach_target(
            MountTarget::View(view),
            resource,
            options,
            permission,
            now,
            #[cfg(feature = "backgrounds")]
            None,
        )
    }
    #[cfg(feature = "backgrounds")]
    pub fn attach_background(
        &mut self,
        target: MountTarget,
        resource: ResourceId,
        kind: BackgroundKind,
        options: AttachOptions,
        permission: HostPermission,
        now: u64,
    ) -> Result<MountId> {
        if !target.is_background() {
            return Err(Error::Invalid("background target"));
        }
        self.attach_target(target, resource, options, permission, now, Some(kind))
    }
    fn attach_target(
        &mut self,
        target: MountTarget,
        resource: ResourceId,
        options: AttachOptions,
        permission: HostPermission,
        now: u64,
        #[cfg(feature = "backgrounds")] background: Option<BackgroundKind>,
    ) -> Result<MountId> {
        self.transaction(|core, s| {
            s.time(now)?;
            if !permission.allowed {
                return Err(Error::PermissionDenied);
            }
            let snap = core.snapshot();
            if !State::target_valid(snap, target, resource) {
                return Err(Error::Stale("mount target"));
            }
            let r = snap
                .resources
                .get(&resource)
                .ok_or(Error::Missing("resource", resource.raw()))?;
            let p = &snap.providers[&r.provider];
            if p.availability != ProviderAvailability::Available {
                return Err(Error::Stale("provider unavailable"));
            }
            let conn = s
                .providers
                .get(&r.provider)
                .ok_or(Error::Unsupported("provider not negotiated"))?
                .clone();
            if !conn.capabilities.modes.contains(&options.mode) {
                return Err(Error::Unsupported("content mode"));
            }
            #[cfg(feature = "backgrounds")]
            if background.is_some_and(|kind| !conn.capabilities.backgrounds.supports(kind)) {
                return Err(Error::Unsupported("background kind"));
            }
            if s.mounts
                .values()
                .any(|m| m.active && m.scope.target == target)
            {
                return Err(Error::Invalid("target already mounted"));
            }
            let mount = MountId(s.id()?);
            let instance = if let Some(id) = options.share {
                let i = s
                    .instances
                    .get(&id)
                    .ok_or(Error::Missing("instance", id.0))?;
                if !p.supports_shared_views || !conn.capabilities.sharing || !options.host_share {
                    return Err(Error::Unsupported("shared instance grant"));
                }
                if i.resource != r.id
                    || i.provider != r.provider
                    || !i.reserved
                    || i.retired
                    || i.scope.connection != conn.epoch
                    || i.scope.target.is_background() != target.is_background()
                {
                    return Err(Error::Stale("shared instance"));
                }
                id
            } else {
                InstanceId(s.id()?)
            };
            let scope = Scope {
                identity: snap.identity.clone(),
                session: s.session.clone(),
                provider: r.provider,
                resource: r.id,
                target,
                connection: conn.epoch,
                instance,
                mount,
            };
            let mut eligibility = Eligibility::default();
            eligibility.revision = permission.eligibility_revision;
            if options.share.is_none() {
                s.instances.insert(
                    instance,
                    Instance {
                        id: instance,
                        resource: r.id,
                        provider: r.provider,
                        mounts: BTreeSet::new(),
                        reserved: true,
                        started: false,
                        retired: false,
                        busy: None,
                        scope: scope.clone(),
                        capabilities: conn.capabilities.clone(),
                        has_restore_reference: !r.restore_reference.is_empty(),
                        recovery: None,
                    },
                );
            }
            s.instances.get_mut(&instance).unwrap().mounts.insert(mount);
            s.mounts.insert(
                mount,
                Mount {
                    id: mount,
                    instance,
                    scope,
                    mode: options.mode,
                    active: true,
                    reserved: true,
                    status: MountStatus::Pending,
                    requested: None,
                    confirmed: Observation::Unknown,
                    restoration: RestorationStrength::Unknown,
                    eligibility,
                    signals: Signals::default(),
                    desired_geometry: 0,
                    confirmed_geometry: 0,
                    desired_focus: 0,
                    confirmed_focus: 0,
                    geometry: None,
                    animation_deadline: None,
                    invalidated: false,
                    recovery: None,
                },
            );
            #[cfg(feature = "backgrounds")]
            if let Some(kind) = background {
                s.backgrounds.insert(mount, BackgroundState::new(kind));
                s.media.entry(instance).or_default();
                s.media_changed(instance)?;
            }
            s.enqueue(
                core,
                mount,
                RequestKind::Attach {
                    mode: options.mode,
                    restore_reference: r.restore_reference.clone(),
                },
                permission,
                now,
            )?;
            Ok(mount)
        })
    }
    pub fn request(
        &mut self,
        mount: MountId,
        kind: RequestKind,
        permission: HostPermission,
        now: u64,
    ) -> Result<RequestId> {
        self.transaction(|core, s| {
            s.time(now)?;
            s.enqueue(core, mount, kind, permission, now)
        })
    }
    pub fn set_eligibility(&mut self, mount: MountId, facts: Eligibility) -> Result<()> {
        self.transaction(|_, s| {
            let m = s
                .mounts
                .get_mut(&mount)
                .ok_or(Error::Missing("mount", mount.0))?;
            if facts.revision <= m.eligibility.revision {
                return Err(Error::Stale("eligibility revision"));
            }
            m.eligibility = facts;
            #[cfg(feature = "backgrounds")]
            let instance = m.instance;
            s.permits.remove(&mount);
            #[cfg(feature = "backgrounds")]
            if s.media.contains_key(&instance) {
                s.media_changed(instance)?;
            }
            Ok(())
        })
    }
    pub fn execute(&mut self, envelope: Envelope) -> Result<kumihimo_core::Response> {
        let snap = self.snapshot();
        let fresh = envelope.identity == snap.identity
            && envelope.operation.sequence() != 0
            && envelope.operation > self.core.ledger_status().high_watermark
            && envelope.expected_generation == snap.generation;
        if fresh {
            if let Command::CloseView {
                view,
                approved: true,
            } = &envelope.command
            {
                if let Some(v) = snap
                    .views
                    .get(view)
                    .filter(|v| v.closed.is_none() && !(v.pinned && snap.pin_policy.deny_close))
                {
                    if let Some(m) = self
                        .state
                        .mounts
                        .values()
                        .find(|m| m.active && m.scope.target == MountTarget::View(v.id))
                    {
                        if self
                            .state
                            .providers
                            .get(&m.scope.provider)
                            .is_some_and(|p| p.capabilities.close_mode == CloseMode::ProviderVeto)
                        {
                            if !self
                                .state
                                .permits
                                .get(&m.id)
                                .is_some_and(|p| p.eligibility == m.eligibility.revision)
                            {
                                return Err(Error::Stale("provider close permit"));
                            }
                        }
                    }
                }
            }
        }
        // Capture bounded bindings and at most the two retained plans affected by MoveFrame.
        let movement = if fresh && self.state.mounts.values().any(|m| m.active) {
            FrameMovement::capture(snap, &envelope.command)
        } else {
            None
        };
        let before: Vec<_> = self
            .state
            .mounts
            .values()
            .filter(|m| m.active)
            .map(|m| {
                let v = m.scope.target.view().and_then(|id| snap.views.get(&id));
                (
                    m.id,
                    v.map(|v| (v.pinned, v.unsaved, v.frame)),
                    State::target_frame(snap, m.scope.target),
                    snap.pin_policy.clone(),
                    movement
                        .as_ref()
                        .map_or_else(MovementImpact::default, |movement| {
                            movement.impact(snap, m.scope.target)
                        }),
                )
            })
            .collect();
        let command = envelope.command.clone();
        let response = self.core.execute(envelope);
        if !response.replayed
            && response.outcome.result.is_ok()
            && !matches!(
                response.outcome.result,
                Ok(kumihimo_core::Change::Unchanged)
            )
        {
            let restore = matches!(command, Command::Restore(_));
            let current = self.core.snapshot();
            let packing = movement
                .as_ref()
                .map_or((false, false), |movement| movement.packing_changed(current));
            for (id, old, old_frame, policy, movement) in before {
                let Some(m) = self.state.mounts.get(&id) else {
                    continue;
                };
                let invalid = restore || !self.state.binding_valid(current, &m.scope);
                let new = m
                    .scope
                    .target
                    .view()
                    .and_then(|id| current.views.get(&id))
                    .map(|v| (v.pinned, v.unsaved, v.frame));
                let new_frame = State::target_frame(current, m.scope.target);
                if invalid {
                    if let Some(m) = self.state.mounts.get_mut(&id) {
                        m.eligibility.unsaved |= old.is_some_and(|x| x.1);
                    }
                    self.state.retire_mount(id);
                } else if old.map(|x| (x.0, x.1)) != new.map(|x| (x.0, x.1))
                    || policy != current.pin_policy
                {
                    self.state.permits.remove(&id);
                    self.state.stale_close(id);
                }
                if old_frame != new_frame || movement.changed(packing) {
                    self.state.stale_geometry(id);
                }
            }
            if let Command::RebindTool { tool, .. }
            | Command::ConfigureTool { tool, .. }
            | Command::SetToolState { tool, .. } = command
            {
                self.state.stale_tool(tool);
            }
            self.state.trim_receipts();
        }
        Ok(response)
    }
}
impl State {
    fn target_valid(snap: &Snapshot, target: MountTarget, resource: ResourceId) -> bool {
        match target {
            MountTarget::View(id) => snap
                .views
                .get(&id)
                .is_some_and(|v| v.closed.is_none() && v.resource == resource),
            #[cfg(feature = "backgrounds")]
            MountTarget::FrameBackground(id) => snap.frames.contains_key(&id),
            #[cfg(feature = "backgrounds")]
            MountTarget::WorkspaceBackground(id) => snap.workspaces.contains_key(&id),
        }
    }
    fn target_frame(
        snap: &Snapshot,
        target: MountTarget,
    ) -> Option<(kumihimo_core::FrameId, kumihimo_core::WorkspaceId)> {
        let frame = match target {
            MountTarget::View(id) => snap.views.get(&id).map(|v| v.frame),
            #[cfg(feature = "backgrounds")]
            MountTarget::FrameBackground(id) => Some(id),
            #[cfg(feature = "backgrounds")]
            MountTarget::WorkspaceBackground(id) => snap.workspaces.get(&id).and_then(|w| w.owner),
        }?;
        snap.frames.get(&frame).map(|f| (frame, f.workspace))
    }
    pub(crate) fn id(&mut self) -> Result<u64> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(Error::Exhausted("protocol ID"))?;
        Ok(id)
    }
    pub(crate) fn time(&mut self, now: u64) -> Result<()> {
        if now < self.now {
            return Err(Error::TimeWentBack);
        }
        self.now = now;
        Ok(())
    }
    pub(crate) fn binding_valid(&self, snap: &Snapshot, scope: &Scope) -> bool {
        scope.identity == snap.identity
            && scope.session == self.session
            && self
                .providers
                .get(&scope.provider)
                .is_some_and(|p| p.epoch == scope.connection)
            && Self::target_valid(snap, scope.target, scope.resource)
            && snap
                .resources
                .get(&scope.resource)
                .is_some_and(|r| r.provider == scope.provider)
            && snap
                .providers
                .get(&scope.provider)
                .is_some_and(|p| p.availability == ProviderAvailability::Available)
    }
    pub(crate) fn counts(&self) -> Result<Counts> {
        let mut bytes = 0usize;
        #[cfg(feature = "navigation")]
        let mut overview_entries = 0usize;
        #[cfg(feature = "navigation")]
        for n in self.navigation.values() {
            bytes = bytes
                .checked_add(n.opaque_bytes()?)
                .ok_or(Error::Exhausted("navigation bytes"))?;
            overview_entries = overview_entries
                .checked_add(n.overview_marker_count())
                .ok_or(Error::Exhausted("overview entries"))?;
        }
        #[cfg(feature = "backgrounds")]
        for b in self.backgrounds.values() {
            bytes = bytes
                .checked_add(b.retained_bytes()?)
                .ok_or(Error::Exhausted("background bytes"))?;
        }
        for p in self.pending.values() {
            bytes = bytes
                .checked_add(p.request.kind.opaque_bytes()?)
                .ok_or(Error::Exhausted("payload bytes"))?;
        }
        for r in &self.receipts {
            if let Some(a) = &r.result {
                bytes = bytes
                    .checked_add(a.opaque_bytes()?)
                    .ok_or(Error::Exhausted("payload bytes"))?;
            }
        }
        let mut cache_bytes = 0usize;
        for c in self.caches.values() {
            cache_bytes = cache_bytes
                .checked_add(c.reserved_bytes.unwrap_or(0))
                .ok_or(Error::Exhausted("cache bytes"))?;
        }
        Ok(Counts {
            #[cfg(feature = "navigation")]
            overview_entries,
            providers: self.providers.len(),
            mounts: self.mounts.values().filter(|m| m.reserved).count(),
            instances: self.instances.values().filter(|i| i.reserved).count(),
            leases: self.mounts.len() + self.instances.len() + self.caches.len(),
            pending: self.pending.len(),
            queue: self.queue.len(),
            receipts: self.receipts.len(),
            retained_bytes: bytes,
            cache_entries: self.caches.len(),
            cache_bytes,
            cache_unknown_reservations: self
                .caches
                .values()
                .filter(|c| c.reserved_bytes.is_none())
                .count(),
        })
    }
    pub(crate) fn admit(&self, prior: &Counts) -> Result<()> {
        let c = self.counts()?;
        let l = &self.limits;
        #[cfg(feature = "navigation")]
        if let Some(limit) = l.overview_entries {
            if c.overview_entries > limit && c.overview_entries > prior.overview_entries {
                return Err(Error::Admission {
                    dimension: "overview entries",
                    limit,
                    attempted: c.overview_entries,
                });
            }
        }
        for (dimension, limit, new, old) in [
            ("providers", l.providers, c.providers, prior.providers),
            ("mounts", l.mounts, c.mounts, prior.mounts),
            ("instances", l.instances, c.instances, prior.instances),
            ("leases", l.leases, c.leases, prior.leases),
            ("pending", l.pending, c.pending, prior.pending),
            ("queue", l.queue, c.queue, prior.queue),
            ("receipts", l.receipts, c.receipts, prior.receipts),
            (
                "retained bytes",
                l.retained_bytes,
                c.retained_bytes,
                prior.retained_bytes,
            ),
            (
                "cache entries",
                l.cache_entries,
                c.cache_entries,
                prior.cache_entries,
            ),
            (
                "cache bytes",
                l.cache_bytes,
                c.cache_bytes,
                prior.cache_bytes,
            ),
        ] {
            if let Some(limit) = limit {
                if new > limit && new > old {
                    return Err(Error::Admission {
                        dimension,
                        limit,
                        attempted: new,
                    });
                }
            }
        }
        Ok(())
    }
    pub(crate) fn receipt(
        &mut self,
        p: PendingRequest,
        status: RequestStatus,
        result: Option<AckResult>,
        cause: Option<&'static str>,
    ) -> Receipt {
        let r = Receipt {
            ticket: p.request.ticket,
            status,
            result,
            cause,
        };
        self.receipts.push_back(r.clone());
        self.trim_receipts();
        r
    }
    pub(crate) fn trim_receipts(&mut self) {
        while self
            .limits
            .receipts
            .is_some_and(|n| self.receipts.len() > n)
        {
            self.receipts.pop_front();
        }
    }
}
