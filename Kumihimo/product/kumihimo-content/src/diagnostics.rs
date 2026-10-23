use crate::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Metric {
    InputDuration,
    LayoutDuration,
    RedrawDuration,
    CpuBytes,
    GpuBytes,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Milliseconds,
    Bytes,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    Measured,
    Estimated,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub instance: InstanceId,
    pub metric: Metric,
    pub unit: Unit,
    pub value: Observation<u64>,
    pub provenance: Provenance,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheReservation {
    pub id: CacheId,
    pub instance: InstanceId,
    pub owner: u64,
    pub consumers: BTreeSet<MountId>,
    pub reserved_bytes: Option<usize>,
    pub requested_release: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticRequest {
    pub ticket: Ticket,
    pub status: RequestStatus,
    pub cause: Option<&'static str>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostics {
    #[cfg(feature = "navigation")]
    pub navigation: Vec<NavigationDiagnostic>,
    #[cfg(feature = "backgrounds")]
    pub backgrounds: Vec<BackgroundDiagnostic>,
    pub counts: Counts,
    pub samples: Vec<Sample>,
    pub requests: Vec<DiagnosticRequest>,
}
#[cfg(feature = "navigation")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationDiagnostic {
    pub mount: MountId,
    pub target: MountTarget,
    pub settings_revision: u64,
    pub model_revision: Option<u64>,
    pub overview_revision: Option<u64>,
    pub gesture: Option<u64>,
    pub selected: NavigationMode,
    pub effective: Option<NavigationMode>,
    pub fallback: Option<NavigationDisabled>,
    pub actual: Observation<NavigationObservation>,
    pub request: Option<RequestId>,
}
#[cfg(feature = "backgrounds")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundDiagnostic {
    pub mount: MountId,
    pub instance: InstanceId,
    pub target: MountTarget,
    pub kind: BackgroundKind,
    pub settings_revision: u64,
    pub policy_revision: u64,
    pub available: bool,
    pub fallback: Option<BackgroundFallback>,
    pub transition: Transition,
    pub requested: Option<MediaBudgetRequest>,
    pub actual: Observation<MediaObservation>,
}
impl Controller {
    pub fn record_sample(&mut self, sample: Sample) -> Result<()> {
        self.transaction(|_, s| {
            if !s.instances.contains_key(&sample.instance) {
                return Err(Error::Missing("instance", sample.instance.0));
            }
            let unit = match sample.metric {
                Metric::CpuBytes | Metric::GpuBytes => Unit::Bytes,
                _ => Unit::Milliseconds,
            };
            if sample.unit != unit
                || matches!(sample.value, Observation::Unknown)
                    != (sample.provenance == Provenance::Unknown)
            {
                return Err(Error::Invalid("sample units/provenance"));
            }
            s.samples.insert((sample.instance, sample.metric), sample);
            Ok(())
        })
    }
    /// Redacted projection: opaque input, asset content and anchor strings are omitted.
    pub fn diagnostics(&self) -> Diagnostics {
        let mut requests: Vec<_> = self
            .state
            .pending
            .values()
            .map(|p| DiagnosticRequest {
                ticket: p.request.ticket.clone(),
                status: p.status,
                cause: None,
            })
            .collect();
        requests.extend(self.state.receipts.iter().map(|r| DiagnosticRequest {
            ticket: r.ticket.clone(),
            status: r.status,
            cause: r.cause,
        }));
        for p in self
            .state
            .mounts
            .values()
            .filter_map(|m| m.recovery.as_ref())
            .chain(
                self.state
                    .instances
                    .values()
                    .filter_map(|i| i.recovery.as_ref()),
            )
        {
            requests.push(DiagnosticRequest {
                ticket: p.request.ticket.clone(),
                status: p.status,
                cause: None,
            });
        }
        Diagnostics {
            #[cfg(feature = "navigation")]
            navigation: self
                .state
                .navigation
                .iter()
                .map(|(id, n)| {
                    let (effective, fallback) =
                        n.effective(&self.state.providers[&n.scope.provider].capabilities);
                    NavigationDiagnostic {
                        mount: *id,
                        target: n.scope.target,
                        settings_revision: n.settings_revision,
                        model_revision: n.model.as_ref().map(|m| m.revision),
                        overview_revision: n.overview.as_ref().map(|o| o.revision),
                        gesture: n.gesture.as_ref().map(|g| g.id),
                        selected: n.settings.mode.value,
                        effective,
                        fallback,
                        actual: n.observation.clone(),
                        request: n.last_request,
                    }
                })
                .collect(),
            #[cfg(feature = "backgrounds")]
            backgrounds: self
                .state
                .backgrounds
                .iter()
                .map(|(id, b)| {
                    let m = &self.state.mounts[id];
                    let media = self.state.media.get(&m.instance);
                    BackgroundDiagnostic {
                        mount: *id,
                        instance: m.instance,
                        target: m.scope.target,
                        kind: b.kind,
                        settings_revision: b.settings_revision,
                        policy_revision: b.policy_revision,
                        available: b.settings.effective.is_some(),
                        fallback: b.settings.fallback,
                        transition: b.settings.transition,
                        requested: media.and_then(|m| m.requested),
                        actual: media.map_or(Observation::Unknown, |m| m.actual.clone()),
                    }
                })
                .collect(),
            counts: self.state.counts().expect("admitted accounting"),
            samples: self.state.samples.values().cloned().collect(),
            requests,
        }
    }
    pub fn reserve_cache(
        &mut self,
        mount: MountId,
        owner: u64,
        bytes: Option<usize>,
        share: Option<CacheId>,
        host_share: bool,
        permission: HostPermission,
    ) -> Result<CacheId> {
        self.transaction(|core, s| {
            s.permission(mount, permission)?;
            s.eligible(core, mount, &RequestKind::CacheRelease(CacheId(0)))?;
            if owner == 0 {
                return Err(Error::Invalid("cache owner"));
            }
            if bytes.is_none() && s.limits.cache_bytes.is_some() {
                return Err(Error::Invalid(
                    "finite cache budget requires declared bytes",
                ));
            }
            let m = &s.mounts[&mount];
            if let Some(id) = share {
                let caps = &s.providers[&m.scope.provider].capabilities;
                if !host_share
                    || !caps.sharing
                    || !core.snapshot().providers[&m.scope.provider].supports_shared_views
                {
                    return Err(Error::Unsupported("cache sharing grant"));
                }
                let c = s.caches.get_mut(&id).ok_or(Error::Missing("cache", id.0))?;
                if c.instance != m.instance
                    || c.owner != owner
                    || c.reserved_bytes != bytes
                    || c.requested_release
                {
                    return Err(Error::Stale("cache reservation"));
                }
                c.consumers.insert(mount);
                return Ok(id);
            }
            let instance = m.instance;
            let id = CacheId(s.id()?);
            s.caches.insert(
                id,
                CacheReservation {
                    id,
                    instance,
                    owner,
                    consumers: BTreeSet::from([mount]),
                    reserved_bytes: bytes,
                    requested_release: false,
                },
            );
            Ok(id)
        })
    }
}
