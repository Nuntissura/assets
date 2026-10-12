use crate::routing::{ClosePermit, State};
use crate::*;
use kumihimo_core::Core;

impl State {
    pub(crate) fn destructive(&self, core: &Core, instance: InstanceId) -> Result<()> {
        let i = self
            .instances
            .get(&instance)
            .ok_or(Error::Missing("instance", instance.0))?;
        for id in &i.mounts {
            let m = &self.mounts[id];
            if !m.reserved {
                continue;
            }
            let e = &m.eligibility;
            if e.protected()
                || !e.can_destroy
                || !e.recoverable
                || !e.host_allowed
                || (m.active
                    && core
                        .snapshot()
                        .views
                        .get(&m.scope.view)
                        .is_some_and(|v| v.unsaved))
            {
                return Err(Error::PermissionDenied);
            }
            if !i.has_restore_reference
                || (m.active
                    && core
                        .snapshot()
                        .resources
                        .get(&m.scope.resource)
                        .is_none_or(|r| r.restore_reference.is_empty()))
            {
                return Err(Error::Invalid("missing recoverable reference"));
            }
        }
        Ok(())
    }
    pub(crate) fn release_mount(&mut self, id: MountId) {
        if let Some(m) = self.mounts.get_mut(&id) {
            m.active = false;
            m.reserved = false;
            m.status = MountStatus::Retired;
            m.confirmed = Observation::Unknown;
            self.permits.remove(&id);
        }
    }
    pub(crate) fn release_empty_uncommitted_instance(&mut self, id: InstanceId) {
        if let Some(i) = self.instances.get_mut(&id) {
            if i.mounts.iter().all(|id| !self.mounts[id].reserved)
                && i.busy.is_none()
                && !self.caches.values().any(|c| c.instance == id)
            {
                i.reserved = false;
                i.retired = true;
            }
        }
        self.prune(id);
    }
    fn prune(&mut self, id: InstanceId) {
        if self.instances.get(&id).is_some_and(|i| {
            !i.reserved
                && i.busy.is_none()
                && i.recovery.is_none()
                && i.mounts
                    .iter()
                    .all(|m| !self.mounts[m].reserved && self.mounts[m].recovery.is_none())
                && !self.caches.values().any(|c| c.instance == id)
                && !self
                    .pending
                    .values()
                    .any(|p| p.request.ticket.scope.instance == id)
        }) {
            let i = self.instances.remove(&id).unwrap();
            for m in i.mounts {
                self.mounts.remove(&m);
                self.permits.remove(&m);
            }
            self.samples.retain(|(instance, _), _| *instance != id);
        }
    }
    fn validate_confirmation(&self, p: &PendingRequest, result: &AckResult) -> Result<()> {
        let AckResult::Confirmed(c) = result else {
            return Ok(());
        };
        let valid = match (&p.request.kind, c) {
            (RequestKind::Attach { .. }, Confirmation::Attached { strength }) => self
                .providers
                .get(&p.request.ticket.scope.provider)
                .is_some_and(|c| c.capabilities.restoration.contains(strength)),
            (RequestKind::Geometry(g), Confirmation::Geometry { anchor }) => g.anchor == *anchor,
            (RequestKind::Lifecycle(requested), Confirmation::Lifecycle(actual)) => {
                requested == actual
            }
            (RequestKind::CloseDecision { .. }, Confirmation::Close { .. }) => true,
            (RequestKind::Reconcile { .. }, Confirmation::Reconciled { .. }) => true,
            (
                RequestKind::Focus(_)
                | RequestKind::Presentation(_)
                | RequestKind::Invalidate
                | RequestKind::Animation { .. }
                | RequestKind::Input(_)
                | RequestKind::ToolAction { .. }
                | RequestKind::AssetAction(_)
                | RequestKind::Detach
                | RequestKind::Dispose
                | RequestKind::CacheRelease(_),
                Confirmation::Done,
            ) => true,
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Invalid("acknowledgment result kind/content"))
        }
    }
    fn apply(&mut self, p: &PendingRequest, c: &Confirmation) -> Result<()> {
        let id = p.request.ticket.scope.mount;
        let instance = p.request.ticket.scope.instance;
        match (&p.request.kind, c) {
            (RequestKind::Attach { .. }, Confirmation::Attached { strength }) => {
                let m = self
                    .mounts
                    .get_mut(&id)
                    .ok_or(Error::Missing("mount", id.0))?;
                m.status = if m.active {
                    MountStatus::Attached
                } else {
                    MountStatus::Retired
                };
                m.restoration = *strength;
                m.confirmed = Observation::Known(LifecycleState::Visible);
            }
            (RequestKind::Geometry(g), Confirmation::Geometry { .. }) => {
                let m = self.mounts.get_mut(&id).unwrap();
                m.confirmed_geometry = p.request.ticket.revision;
                let mut g = g.clone();
                g.anchor = None;
                m.geometry = Some(g);
            }
            (RequestKind::Focus(f), _) => {
                let m = self.mounts.get_mut(&id).unwrap();
                m.confirmed_focus = p.request.ticket.revision;
                m.signals.focused = *f;
            }
            (RequestKind::Presentation(signals), _) => {
                self.mounts.get_mut(&id).unwrap().signals = signals.clone()
            }
            (RequestKind::Invalidate, _) => self.mounts.get_mut(&id).unwrap().invalidated = false,
            (RequestKind::Animation { deadline }, _) => {
                let next = self
                    .pending
                    .values()
                    .filter(|q| {
                        q.request.ticket.scope.mount == id
                            && q.request.ticket.id != p.request.ticket.id
                    })
                    .filter_map(|q| match q.request.kind {
                        RequestKind::Animation { deadline } => deadline,
                        _ => None,
                    })
                    .min();
                let m = self.mounts.get_mut(&id).unwrap();
                if m.animation_deadline == *deadline {
                    m.animation_deadline = next;
                }
            }
            (RequestKind::Lifecycle(l), Confirmation::Lifecycle(actual)) => {
                let ids = self.instances[&instance].mounts.clone();
                for id in ids {
                    let m = self.mounts.get_mut(&id).unwrap();
                    m.confirmed = Observation::Known(*actual);
                }
                if *l == LifecycleState::Unloaded {
                    self.release_instance(instance);
                }
            }
            (
                RequestKind::CloseDecision {
                    eligibility_revision,
                },
                Confirmation::Close { approved: true },
            ) => {
                self.permits.insert(
                    id,
                    ClosePermit {
                        eligibility: *eligibility_revision,
                    },
                );
            }
            (RequestKind::CloseDecision { .. }, Confirmation::Close { approved: false }) => {
                self.permits.remove(&id);
            }
            (RequestKind::Detach, _) => self.release_mount(id),
            (RequestKind::Dispose, _) => self.release_instance(instance),
            (RequestKind::CacheRelease(cache), _) => {
                let c = self
                    .caches
                    .get_mut(cache)
                    .ok_or(Error::Missing("cache", cache.0))?;
                c.consumers.remove(&id);
                c.requested_release = false;
                if c.consumers.is_empty() {
                    self.caches.remove(cache);
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn release_instance(&mut self, instance: InstanceId) {
        let ids = self.instances[&instance].mounts.clone();
        for id in ids {
            self.release_mount(id);
        }
        let i = self.instances.get_mut(&instance).unwrap();
        i.reserved = false;
        i.retired = true;
    }
    pub(crate) fn get_request(&self, id: RequestId) -> Option<PendingRequest> {
        self.pending.get(&id).cloned().or_else(|| {
            self.mounts
                .values()
                .filter_map(|m| m.recovery.as_ref())
                .chain(self.instances.values().filter_map(|i| i.recovery.as_ref()))
                .find(|p| p.request.ticket.id == id)
                .cloned()
        })
    }
    pub(crate) fn remove_request(&mut self, id: RequestId) {
        self.pending.remove(&id);
        self.queue.retain(|x| *x != id);
        for m in self.mounts.values_mut() {
            if m.recovery
                .as_ref()
                .is_some_and(|p| p.request.ticket.id == id)
            {
                m.recovery = None;
            }
        }
        for i in self.instances.values_mut() {
            if i.recovery
                .as_ref()
                .is_some_and(|p| p.request.ticket.id == id)
            {
                i.recovery = None;
            }
        }
    }
    pub(crate) fn unknown(&mut self, id: RequestId) {
        if let Some(p) = self.pending.get_mut(&id) {
            p.status = RequestStatus::Unknown;
        }
        for p in self
            .mounts
            .values_mut()
            .filter_map(|m| m.recovery.as_mut())
            .chain(
                self.instances
                    .values_mut()
                    .filter_map(|i| i.recovery.as_mut()),
            )
        {
            if p.request.ticket.id == id {
                p.status = RequestStatus::Unknown;
            }
        }
    }
    pub(crate) fn expire(&mut self, now: u64) {
        let ids: Vec<_> = self
            .pending
            .values()
            .chain(self.mounts.values().filter_map(|m| m.recovery.as_ref()))
            .chain(self.instances.values().filter_map(|i| i.recovery.as_ref()))
            .filter(|p| p.status == RequestStatus::Dispatched && now >= p.request.deadline)
            .map(|p| (p.request.ticket.id, p.request.ticket.scope.mount))
            .collect();
        for (id, mount) in ids {
            self.unknown(id);
            if let Some(m) = self.mounts.get_mut(&mount) {
                m.status = MountStatus::Unknown;
                m.confirmed = Observation::Unknown;
            }
        }
    }
    fn recover(
        &mut self,
        core: &Core,
        mount: MountId,
        kind: RequestKind,
        p: HostPermission,
        now: u64,
        instance_slot: bool,
    ) -> Result<RequestId> {
        self.time(now)?;
        self.permission(mount, p)?;
        let m = self
            .mounts
            .get(&mount)
            .ok_or(Error::Missing("mount", mount.0))?
            .clone();
        let i = &self.instances[&m.instance];
        let previous = if instance_slot {
            i.recovery.as_ref()
        } else {
            m.recovery.as_ref()
        };
        let replace_query = previous.is_some_and(|p| {
            p.status == RequestStatus::Unknown
                && matches!(p.request.kind, RequestKind::Reconcile { .. })
                && matches!(kind, RequestKind::Reconcile { .. })
        });
        if previous.is_some() && !replace_query {
            return Err(Error::Busy(m.instance));
        }
        match kind {
            RequestKind::Reconcile { uncertain } => {
                let old = self
                    .get_request(uncertain)
                    .ok_or(Error::Expired(uncertain))?;
                if old.status != RequestStatus::Unknown
                    || old.request.ticket.scope.instance != m.instance
                    || matches!(old.request.kind, RequestKind::Reconcile { .. })
                {
                    return Err(Error::Stale("uncertain request"));
                }
            }
            RequestKind::Dispose if instance_slot => {
                if i.busy.is_some_and(|id| {
                    self.get_request(id)
                        .is_some_and(|p| p.status == RequestStatus::Unknown)
                }) {
                    return Err(Error::Busy(m.instance));
                }
                self.destructive(core, m.instance)?;
                if !i.capabilities.lifecycle.contains(&LifecycleState::Unloaded) {
                    return Err(Error::Unsupported("disposal capability"));
                }
                if self.caches.values().any(|c| c.instance == m.instance) {
                    return Err(Error::Invalid("cache cleanup required"));
                }
            }
            RequestKind::Detach if !instance_slot => {
                if i.busy.is_some_and(|id| {
                    self.get_request(id)
                        .is_some_and(|p| p.status == RequestStatus::Unknown)
                }) {
                    return Err(Error::Busy(m.instance));
                }
            }
            RequestKind::CacheRelease(cache) if !instance_slot => {
                let c = self
                    .caches
                    .get(&cache)
                    .ok_or(Error::Missing("cache", cache.0))?;
                if !i.capabilities.cache
                    || c.instance != m.instance
                    || !c.consumers.contains(&mount)
                {
                    return Err(Error::Stale("cache consumer"));
                }
            }
            _ => return Err(Error::Invalid("reserved recovery kind")),
        }
        if replace_query {
            let old = previous.unwrap().clone();
            self.remove_request(old.request.ticket.id);
            self.receipt(
                old,
                RequestStatus::Superseded,
                None,
                Some("reconciliation observation superseded"),
            );
        }
        let id = RequestId(self.id()?);
        let deadline = now
            .checked_add(self.limits.ordinary_timeout_ms)
            .ok_or(Error::Exhausted("recovery deadline"))?;
        let pending = PendingRequest {
            request: Request {
                ticket: Ticket {
                    scope: m.scope,
                    channel: kind.channel(),
                    id,
                    revision: id.0,
                },
                kind,
                deadline,
            },
            status: RequestStatus::Queued,
            stale: false,
            geometry: 0,
            focus: 0,
            eligibility_revision: m.eligibility.revision,
        };
        if instance_slot {
            self.instances.get_mut(&m.instance).unwrap().recovery = Some(pending)
        } else {
            self.mounts.get_mut(&mount).unwrap().recovery = Some(pending)
        }
        Ok(id)
    }
}
impl Controller {
    pub fn recover_mount(
        &mut self,
        mount: MountId,
        kind: RequestKind,
        p: HostPermission,
        now: u64,
    ) -> Result<RequestId> {
        self.transaction(|core, s| s.recover(core, mount, kind, p, now, false))
    }
    pub fn recover_instance(
        &mut self,
        instance: InstanceId,
        kind: RequestKind,
        p: HostPermission,
        now: u64,
    ) -> Result<RequestId> {
        self.transaction(|core, s| {
            let i = s
                .instances
                .get(&instance)
                .ok_or(Error::Missing("instance", instance.0))?;
            let mount = i
                .mounts
                .iter()
                .next()
                .copied()
                .ok_or(Error::Invalid("instance has no lease anchor"))?;
            s.recover(core, mount, kind, p, now, true)
        })
    }
    pub fn dispatch_recovery(
        &mut self,
        instance: InstanceId,
        permission: HostPermission,
        now: u64,
    ) -> Result<Option<Request>> {
        self.transaction(|core, s| {
            s.time(now)?;
            let i = s
                .instances
                .get(&instance)
                .ok_or(Error::Missing("instance", instance.0))?;
            let p = i
                .recovery
                .as_ref()
                .filter(|p| p.status == RequestStatus::Queued)
                .or_else(|| {
                    i.mounts
                        .iter()
                        .filter_map(|id| s.mounts[id].recovery.as_ref())
                        .find(|p| p.status == RequestStatus::Queued)
                })
                .cloned();
            let Some(p) = p else { return Ok(None) };
            s.permission(p.request.ticket.scope.mount, permission)?;
            if p.eligibility_revision
                != s.mounts[&p.request.ticket.scope.mount].eligibility.revision
            {
                return Err(Error::Stale("recovery eligibility"));
            }
            let reconciliation = matches!(p.request.kind, RequestKind::Reconcile { .. });
            if i.busy.is_some() && !reconciliation {
                return Ok(None);
            }
            if matches!(p.request.kind, RequestKind::Dispose) {
                s.destructive(core, instance)?;
            }
            let id = p.request.ticket.id;
            for q in s
                .mounts
                .values_mut()
                .filter_map(|m| m.recovery.as_mut())
                .chain(s.instances.values_mut().filter_map(|i| i.recovery.as_mut()))
            {
                if q.request.ticket.id == id {
                    q.status = RequestStatus::Dispatched;
                }
            }
            if !reconciliation {
                s.instances.get_mut(&instance).unwrap().busy = Some(id)
            }
            Ok(Some(p.request))
        })
    }
    pub fn ack(&mut self, ack: Ack, now: u64) -> Result<Receipt> {
        self.transaction(|core, s| {
            if ack.ticket.scope.identity != core.snapshot().identity {
                return Err(Error::IdentityMismatch);
            }
            if ack.ticket.scope.session != s.session {
                return Err(Error::SessionMismatch);
            }
            ack.ticket.validate()?;
            s.time(now)?;
            if let Some(r) = s.receipts.iter().find(|r| r.ticket.id == ack.ticket.id) {
                return if r.ticket == ack.ticket && r.result.as_ref() == Some(&ack.result) {
                    Ok(r.clone())
                } else {
                    Err(Error::Conflict(ack.ticket.id))
                };
            }
            let p = s
                .get_request(ack.ticket.id)
                .ok_or(Error::Expired(ack.ticket.id))?;
            // An exact old geometry echo can decrease/finish an obligation after a field limit is lowered.
            let exact_anchor=matches!((&p.request.kind,&ack.result),(RequestKind::Geometry(g),AckResult::Confirmed(Confirmation::Geometry {anchor})) if g.anchor==*anchor);
            ack.result.validate(if exact_anchor {None}else {s.limits.payload_bytes})?;
            if p.request.ticket != ack.ticket {
                return Err(Error::Conflict(ack.ticket.id));
            }
            if !matches!(p.status, RequestStatus::Dispatched | RequestStatus::Unknown) {
                return Err(Error::Stale("undispatched acknowledgment"));
            }
            let recovery = matches!(
                p.request.kind,
                RequestKind::Detach | RequestKind::Dispose | RequestKind::Reconcile { .. }
            );
            if !recovery && (p.stale || !s.binding_valid(core.snapshot(), &p.request.ticket.scope))
            {
                return Err(Error::Stale("acknowledgment binding"));
            }
            if matches!(
                p.request.kind,
                RequestKind::CloseDecision { .. } | RequestKind::Lifecycle(_)
            ) && s.mounts[&p.request.ticket.scope.mount].eligibility.revision
                != p.eligibility_revision
            {
                return Err(Error::Stale("eligibility acknowledgment"));
            }
            s.validate_confirmation(&p, &ack.result)?;
            if matches!(ack.result, AckResult::Failed { ambiguous: true }) {
                s.unknown(ack.ticket.id);
                let m = s.mounts.get_mut(&p.request.ticket.scope.mount).unwrap();
                m.status = MountStatus::Unknown;
                m.confirmed = Observation::Unknown;
                return Ok(Receipt {
                    ticket: ack.ticket,
                    status: RequestStatus::Unknown,
                    result: Some(ack.result),
                    cause: Some("ambiguous provider effect"),
                });
            }
            let instance = p.request.ticket.scope.instance;
            let status = match &ack.result {
                AckResult::Confirmed(_) => RequestStatus::Confirmed,
                AckResult::Unsupported => RequestStatus::Unsupported,
                AckResult::Rejected => RequestStatus::Rejected,
                AckResult::Failed { .. } => RequestStatus::Failed,
            };
            if let AckResult::Confirmed(c) = &ack.result {
                if let (
                    RequestKind::Reconcile { uncertain },
                    Confirmation::Reconciled { effect, lifecycle },
                ) = (&p.request.kind, c)
                {
                    let old = s
                        .get_request(*uncertain)
                        .ok_or(Error::Expired(*uncertain))?;
                    if old.status != RequestStatus::Unknown {
                        return Err(Error::Stale("reconciliation outcome"));
                    }
                    if *effect == Effect::Applied {
                        let c = match &old.request.kind {
                            RequestKind::Attach { .. } => Confirmation::Attached {
                                strength: RestorationStrength::Unknown,
                            },
                            RequestKind::Lifecycle(l) => {
                                Confirmation::Lifecycle(lifecycle.unwrap_or(*l))
                            }
                            RequestKind::Geometry(g) => Confirmation::Geometry {
                                anchor: g.anchor.clone(),
                            },
                            RequestKind::CloseDecision { .. } => {
                                return Err(Error::Invalid(
                                    "reconciliation cannot invent close approval",
                                ))
                            }
                            _ => Confirmation::Done,
                        };
                        if !old.stale
                            || matches!(
                                old.request.kind,
                                RequestKind::Attach { .. }
                                    | RequestKind::Detach
                                    | RequestKind::Dispose
                                    | RequestKind::CacheRelease(_)
                                    | RequestKind::Lifecycle(_)
                            )
                        {
                            s.apply(&old, &c)?;
                        }
                    } else if matches!(old.request.kind, RequestKind::Attach { .. }) {
                        s.release_mount(old.request.ticket.scope.mount);
                    }
                    let old_mount = old.request.ticket.scope.mount;
                    let current = s.mounts.get(&old_mount).is_some_and(|m| {
                        m.active && m.reserved && s.binding_valid(core.snapshot(), &m.scope)
                    });
                    if let Some(m) = s.mounts.get_mut(&old_mount) {
                        if current {
                            m.status = MountStatus::Attached;
                            m.confirmed =
                                lifecycle.map_or(Observation::Unknown, Observation::Known);
                        }
                    }
                    s.remove_request(*uncertain);
                    s.reject_dependents(old_mount, *uncertain);
                    s.receipt(
                        old,
                        RequestStatus::Confirmed,
                        None,
                        Some("effect reconciled"),
                    );
                    if let Some(i) = s.instances.get_mut(&instance) {
                        if i.busy == Some(*uncertain) {
                            i.busy = None
                        }
                    }
                    // Reconciliation establishes only this old effect, never a replacement attachment.
                } else {
                    s.apply(&p, c)?;
                }
            } else {
                s.reject_dependents(p.request.ticket.scope.mount, ack.ticket.id);
                if matches!(p.request.kind, RequestKind::Attach { .. }) {
                    s.release_mount(p.request.ticket.scope.mount);
                }
                if let RequestKind::CacheRelease(cache) = p.request.kind {
                    if let Some(c) = s.caches.get_mut(&cache) {
                        c.requested_release = false;
                    }
                }
            }
            s.remove_request(ack.ticket.id);
            if let Some(i) = s.instances.get_mut(&instance) {
                if i.busy == Some(ack.ticket.id) {
                    i.busy = None
                }
            }
            if !matches!(p.request.kind,RequestKind::Attach {..}|RequestKind::Detach|RequestKind::Dispose|RequestKind::Reconcile {..}|RequestKind::Lifecycle(LifecycleState::Unloaded)) {
                if let Some(m)=s.mounts.get_mut(&p.request.ticket.scope.mount) {if m.active && m.reserved {m.status=MountStatus::Attached;}}
            }
            let receipt = s.receipt(p, status, Some(ack.result), None);
            s.release_empty_uncommitted_instance(instance);
            Ok(receipt)
        })
    }
}
