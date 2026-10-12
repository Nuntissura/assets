use crate::routing::State;
use crate::*;
use kumihimo_core::Core;

impl State {
    pub(crate) fn permission(&self, mount: MountId, p: HostPermission) -> Result<()> {
        let m = self
            .mounts
            .get(&mount)
            .ok_or(Error::Missing("mount", mount.0))?;
        if !p.allowed {
            return Err(Error::PermissionDenied);
        }
        if p.eligibility_revision != m.eligibility.revision {
            return Err(Error::Stale("host permission revision"));
        }
        Ok(())
    }
    pub(crate) fn eligible(&self, core: &Core, mount: MountId, kind: &RequestKind) -> Result<()> {
        self.eligible_with_geometry(core, mount, kind, None)
    }
    fn eligible_with_geometry(
        &self,
        core: &Core,
        mount: MountId,
        kind: &RequestKind,
        captured: Option<u64>,
    ) -> Result<()> {
        let m = self
            .mounts
            .get(&mount)
            .ok_or(Error::Missing("mount", mount.0))?;
        if !m.active || !self.binding_valid(core.snapshot(), &m.scope) {
            return Err(Error::Stale("mount binding"));
        }
        let caps = &self.providers[&m.scope.provider].capabilities;
        let supported = match kind {
            RequestKind::Attach { mode, .. } => caps.modes.contains(mode),
            RequestKind::Geometry(g) => {
                caps.geometry && (g.anchor.is_none() || caps.anchored_reflow)
            }
            RequestKind::Focus(_) => caps.focus,
            RequestKind::Presentation(_) => caps.presentation,
            RequestKind::Invalidate => caps.invalidation,
            RequestKind::Animation { .. } => caps.animation,
            RequestKind::Input(i) => {
                caps.input
                    && !matches!(i.owner, InputOwner::FrameChrome)
                    && (!matches!(i.owner, InputOwner::Embed(_)) || caps.embed_input)
            }
            RequestKind::ToolAction { tool, target, .. } => {
                let t = core
                    .snapshot()
                    .tools
                    .get(tool)
                    .ok_or(Error::Missing("tool", tool.raw()))?;
                if t.target != *target
                    || target.view != m.scope.view
                    || target.resource != m.scope.resource
                {
                    return Err(Error::Stale("tool target"));
                }
                let shown = core.snapshot().effective_tool_presentation(t);
                if !shown.visible
                    || !shown.usable
                    || !shown.applicable
                    || !core.snapshot().providers.get(&t.owner).is_some_and(|p| {
                        p.availability == kumihimo_core::ProviderAvailability::Available
                    })
                {
                    return Err(Error::Stale("tool unavailable"));
                }
                caps.tools
            }
            RequestKind::AssetAction(a) => {
                if matches!(a.mode, AssetMode::DomainChange)
                    != matches!(a.undo, UndoScope::HostDomain)
                {
                    return Err(Error::Invalid("asset undo scope"));
                }
                caps.assets
            }
            RequestKind::Lifecycle(l) => caps.lifecycle.contains(l),
            RequestKind::CloseDecision {
                eligibility_revision,
            } => {
                if *eligibility_revision != m.eligibility.revision {
                    return Err(Error::Stale("close eligibility"));
                }
                caps.close_mode == CloseMode::ProviderVeto
            }
            RequestKind::CacheRelease(id) => {
                if id.0 != 0 {
                    let c = self.caches.get(id).ok_or(Error::Missing("cache", id.0))?;
                    if c.instance != m.instance || !c.consumers.contains(&mount) {
                        return Err(Error::Stale("cache consumer"));
                    }
                }
                caps.cache
            }
            RequestKind::Detach | RequestKind::Dispose | RequestKind::Reconcile { .. } => {
                return Err(Error::Invalid("use reserved recovery route"))
            }
        };
        if !supported {
            return Err(Error::Unsupported("request capability"));
        }
        if matches!(
            kind,
            RequestKind::Lifecycle(LifecycleState::Frozen | LifecycleState::Unloaded)
        ) {
            self.destructive(core, m.instance)?;
        }
        if let RequestKind::Input(i) = kind {
            if let (Some(x), Some(y)) = (i.x, i.y) {
                let geometry = self
                    .pending
                    .values()
                    .find(|p| p.request.ticket.id.0 == captured.unwrap_or(m.desired_geometry))
                    .and_then(|p| match &p.request.kind {
                        RequestKind::Geometry(g) => Some(g),
                        _ => None,
                    })
                    .or_else(|| {
                        if m.confirmed_geometry == captured.unwrap_or(m.desired_geometry) {
                            m.geometry.as_ref()
                        } else {
                            None
                        }
                    })
                    .ok_or(Error::Invalid("input requires geometry"))?;
                let c = geometry.clip;
                if x.get() < c.x()
                    || y.get() < c.y()
                    || x.get() > c.x() + c.width()
                    || y.get() > c.y() + c.height()
                {
                    return Err(Error::Invalid("input outside clip"));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn enqueue(
        &mut self,
        core: &Core,
        mount: MountId,
        kind: RequestKind,
        permission: HostPermission,
        now: u64,
    ) -> Result<RequestId> {
        kind.validate(self.limits.payload_bytes)?;
        self.permission(mount, permission)?;
        self.eligible(core, mount, &kind)?;
        let m = self.mounts[&mount].clone();
        if matches!(kind, RequestKind::Lifecycle(_))
            && self.instances[&m.instance].busy.is_some_and(|id| {
                self.get_request(id)
                    .is_some_and(|p| p.status == RequestStatus::Unknown)
            })
        {
            return Err(Error::Busy(m.instance));
        }
        if let RequestKind::CacheRelease(id) = kind {
            let c = self.caches.get(&id).ok_or(Error::Missing("cache", id.0))?;
            if c.instance != m.instance || !c.consumers.contains(&mount) {
                return Err(Error::Stale("cache consumer"));
            }
        }
        if let RequestKind::Animation { deadline: Some(d) } = &kind {
            if *d < now {
                return Err(Error::Invalid("past animation deadline"));
            }
        }
        if kind.coalescible() {
            let mut old = None;
            for id in self.queue.iter().rev() {
                let p = &self.pending[id];
                if p.request.ticket.scope.mount != mount {
                    continue;
                }
                if p.request.kind.barrier() {
                    break;
                }
                if p.request.ticket.channel == kind.channel() {
                    old = Some(*id);
                    break;
                }
            }
            if let Some(id) = old {
                let p = self.pending.remove(&id).unwrap();
                self.queue.retain(|x| *x != id);
                self.receipt(
                    p,
                    RequestStatus::Superseded,
                    None,
                    Some("unsent coalescing"),
                );
            }
        }
        let id = RequestId(self.id()?);
        let deadline = now
            .checked_add(self.limits.ordinary_timeout_ms)
            .ok_or(Error::Exhausted("deadline"))?;
        let ticket = Ticket {
            scope: m.scope.clone(),
            id,
            channel: kind.channel(),
            revision: id.0,
        };
        let p = PendingRequest {
            request: Request {
                ticket,
                kind: kind.clone(),
                deadline,
            },
            status: RequestStatus::Queued,
            stale: false,
            geometry: m.desired_geometry,
            focus: m.desired_focus,
            eligibility_revision: m.eligibility.revision,
        };
        let m = self.mounts.get_mut(&mount).unwrap();
        match kind {
            RequestKind::Geometry(_) => m.desired_geometry = id.0,
            RequestKind::Focus(_) => m.desired_focus = id.0,
            RequestKind::Lifecycle(l) => m.requested = Some(l),
            RequestKind::Invalidate => m.invalidated = true,
            RequestKind::Animation { deadline: Some(d) } => {
                m.animation_deadline = Some(m.animation_deadline.map_or(d, |old| old.min(d)))
            }
            _ => {}
        }
        if let RequestKind::CacheRelease(cache) = kind {
            self.caches.get_mut(&cache).unwrap().requested_release = true;
        }
        self.pending.insert(id, p);
        self.queue.push_back(id);
        Ok(id)
    }
    pub(crate) fn reject_dependents(&mut self, mount: MountId, id: RequestId) {
        let dependents: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| {
                p.status == RequestStatus::Queued
                    && p.request.ticket.scope.mount == mount
                    && (p.geometry == id.0 || p.focus == id.0)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in dependents {
            let p = self.pending.remove(&id).unwrap();
            self.queue.retain(|x| *x != id);
            self.receipt(
                p,
                RequestStatus::Rejected,
                None,
                Some("prerequisite failed"),
            );
        }
    }
    pub(crate) fn stale_where(&mut self, predicate: impl Fn(&PendingRequest) -> bool) {
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| predicate(p))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let Some(current) = self.pending.get(&id) else {
                continue;
            };
            if current.status == RequestStatus::Queued {
                let p = self.pending.remove(&id).unwrap();
                self.queue.retain(|x| *x != id);
                self.reject_dependents(p.request.ticket.scope.mount, id);
                self.receipt(
                    p,
                    RequestStatus::Rejected,
                    None,
                    Some("canonical binding changed"),
                );
            } else {
                let p = self.pending.get_mut(&id).unwrap();
                p.stale = true;
                p.status = RequestStatus::Unknown;
            }
        }
    }
    pub(crate) fn stale_close(&mut self, id: MountId) {
        self.stale_where(|p| {
            p.request.ticket.scope.mount == id
                && matches!(p.request.kind, RequestKind::CloseDecision { .. })
        });
    }
    pub(crate) fn stale_geometry(&mut self, id: MountId) {
        self.stale_where(|p| {
            p.request.ticket.scope.mount == id
                && matches!(
                    p.request.kind,
                    RequestKind::Geometry(_) | RequestKind::Input(_)
                )
        });
    }
    pub(crate) fn stale_tool(&mut self, id: kumihimo_core::ToolId) {
        self.stale_where(
            |p| matches!(p.request.kind,RequestKind::ToolAction {tool,..} if tool==id),
        );
    }
    pub(crate) fn retire_mount(&mut self, id: MountId) {
        if let Some(m) = self.mounts.get_mut(&id) {
            m.active = false;
            m.status = MountStatus::Retired;
            m.confirmed = Observation::Unknown;
            self.permits.remove(&id);
        }
        self.stale_where(|p| p.request.ticket.scope.mount == id);
        for i in self.instances.values_mut() {
            if i.mounts.contains(&id) && i.mounts.iter().all(|id| !self.mounts[id].active) {
                i.retired = true;
            }
        }
    }
    pub(crate) fn retire_provider(&mut self, provider: kumihimo_core::ProviderId) {
        let ids: Vec<_> = self
            .mounts
            .values()
            .filter(|m| m.active && m.scope.provider == provider)
            .map(|m| m.id)
            .collect();
        for id in ids {
            self.retire_mount(id);
        }
    }
}
impl Controller {
    /// Pull one mount's next effect. None reports an unresolved ordering prerequisite.
    pub fn dispatch(
        &mut self,
        mount: MountId,
        permission: HostPermission,
        now: u64,
    ) -> Result<Option<Request>> {
        self.transaction(|core, s| {
            s.time(now)?;
            s.permission(mount, permission)?;
            let m = s
                .mounts
                .get(&mount)
                .ok_or(Error::Missing("mount", mount.0))?
                .clone();
            if s.instances[&m.instance].busy.is_some() {
                return Ok(None);
            }
            loop {
                let Some(id) = s
                    .queue
                    .iter()
                    .copied()
                    .find(|id| s.pending[id].request.ticket.scope.mount == mount)
                else {
                    return Ok(None);
                };
                let p = s.pending[&id].clone();
                if p.stale
                    || s.eligible_with_geometry(core, mount, &p.request.kind, Some(p.geometry))
                        .is_err()
                    || p.eligibility_revision != s.mounts[&mount].eligibility.revision
                {
                    s.pending.remove(&id);
                    s.queue.retain(|x| *x != id);
                    s.reject_dependents(mount, id);
                    s.receipt(p, RequestStatus::Rejected, None, Some("stale delivery"));
                    continue;
                }
                let m = &s.mounts[&mount];
                if matches!(
                    p.request.kind,
                    RequestKind::Input(_)
                        | RequestKind::ToolAction { .. }
                        | RequestKind::AssetAction(_)
                ) && (p.geometry != m.confirmed_geometry || p.focus != m.confirmed_focus)
                {
                    return Ok(None);
                }
                if !matches!(p.request.kind, RequestKind::Attach { .. })
                    && m.status != MountStatus::Attached
                {
                    return Ok(None);
                }
                s.queue.retain(|x| *x != id);
                s.pending.get_mut(&id).unwrap().status = RequestStatus::Dispatched;
                s.instances.get_mut(&m.instance).unwrap().busy = Some(id);
                return Ok(Some(p.request));
            }
        })
    }
    pub fn advance(&mut self, now: u64) -> Result<()> {
        self.transaction(|_, s| {
            s.time(now)?;
            s.expire(now);
            Ok(())
        })
    }
    pub fn cancel(&mut self, id: RequestId, now: u64) -> Result<()> {
        self.transaction(|_, s| {
            s.time(now)?;
            if let Some(p) = s.get_request(id) {
                if p.status == RequestStatus::Queued {
                    s.remove_request(id);
                    s.reject_dependents(p.request.ticket.scope.mount, id);
                    let mount = p.request.ticket.scope.mount;
                    if matches!(p.request.kind, RequestKind::Attach { .. }) {
                        s.release_mount(mount);
                        s.release_empty_uncommitted_instance(p.request.ticket.scope.instance);
                    }
                    if let RequestKind::CacheRelease(cache) = p.request.kind {
                        if let Some(c) = s.caches.get_mut(&cache) {
                            c.requested_release = false;
                        }
                    }
                    s.receipt(
                        p,
                        RequestStatus::Cancelled,
                        None,
                        Some("cancelled before dispatch"),
                    );
                } else {
                    s.unknown(id);
                    if let Some(m) = s.mounts.get_mut(&p.request.ticket.scope.mount) {
                        m.status = MountStatus::Unknown;
                        m.confirmed = Observation::Unknown;
                    }
                }
                return Ok(());
            }
            if s.receipts.iter().any(|r| r.ticket.id == id) {
                return Ok(());
            }
            Err(Error::Expired(id))
        })
    }
}
