use crate::*;
use kumihimo_core::{
    Command, Core, Envelope, ProviderAvailability, ProviderId, ResourceId, Snapshot, ViewId,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
                .find(|m| m.scope.view == view && m.active)
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
        self.transaction(|core, s| {
            s.time(now)?;
            if !permission.allowed {
                return Err(Error::PermissionDenied);
            }
            let snap = core.snapshot();
            let v = snap
                .views
                .get(&view)
                .ok_or(Error::Missing("view", view.raw()))?;
            if v.closed.is_some() {
                return Err(Error::Stale("closed view"));
            }
            let r = &snap.resources[&v.resource];
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
            if s.mounts.values().any(|m| m.active && m.scope.view == view) {
                return Err(Error::Invalid("view already mounted"));
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
                view,
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
            s.permits.remove(&mount);
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
                        .find(|m| m.active && m.scope.view == v.id)
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
        // Only bounded bindings are inspected; canonical saved trees are never copied.
        let before: Vec<_> = self
            .state
            .mounts
            .values()
            .filter(|m| m.active)
            .map(|m| {
                let v = snap.views.get(&m.scope.view);
                (
                    m.id,
                    v.map(|v| (v.pinned, v.unsaved, v.frame)),
                    snap.pin_policy.clone(),
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
            for (id, old, policy) in before {
                let Some(m) = self.state.mounts.get(&id) else {
                    continue;
                };
                let invalid = restore || !self.state.binding_valid(current, &m.scope);
                let new = current
                    .views
                    .get(&m.scope.view)
                    .map(|v| (v.pinned, v.unsaved, v.frame));
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
                if old.map(|x| x.2) != new.map(|x| x.2) {
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
            && snap
                .views
                .get(&scope.view)
                .is_some_and(|v| v.closed.is_none() && v.resource == scope.resource)
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
