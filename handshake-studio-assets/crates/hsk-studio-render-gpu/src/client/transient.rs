//! The transient in-process adapter (lane SMB-GPU-CLIENT-PORT-1, MT-36248 AC-HOST-005): the
//! asset-local stand-in for the Handshake GPU owner until MT-203 exists. It opens one headless
//! device per admitted grant (Vulkan first on Windows), declares the grant's residency budget
//! itself (`studio_declared`) and keeps the settle-once ledger. Its results are evidence, never
//! host acceptance; the first adoption of the real owner replaces it behind the same port.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    AdmissionRequest, BudgetSource, ClientRefusal, DeviceRef, GpuClientPort, GpuGrant, GrantIssuer,
    GrantLedger, GrantParts, PortCounters, SettleError, SettleReceipt, Settlement,
};
use crate::device::{DeviceOptions, GpuApi, acquire_lease};
use crate::error::{GpuError, Reason};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::render_plan::BackendError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Machine-readable facts of the transient adapter (the crate DESCRIPTOR's `device` section
/// points here).
pub const TRANSIENT_DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-RENDER_GPU.client.transient","lane":"SMB-GPU-CLIENT-PORT-1","asset_local":true,"host_acceptance":"excluded (MT-36248 AC-HOST-005): the results of this adapter are asset-local evidence, never host acceptance; the Handshake GPU owner (WP-KERNEL-012 MT-203) replaces it behind GpuClientPort without changing any module's public contract","issuer":"transient_asset_local","budget":{"source":"studio_declared","default_bytes":268435456,"from":"TransientOptions.residency_budget_bytes (DeviceOptions.residency_budget_bytes through GpuBackend::new)"},"candidates":{"windows":["vulkan","dx12"],"apple":["metal"],"other":["vulkan"],"rule":"one backend per instance, first with an adapter wins; the backend actually used is DeviceInfo.backend and the grant backend key, and a dx12 or gl result never proves a vulkan row (GUI-DESKTOP-026)","proven_by":"unit test client::transient::tests::candidate_order_is_vulkan_first_on_windows"},"devices":"one headless device per admitted grant (no surface, window or focus), device-lost and uncaptured-error latches installed by this adapter; rebind opens a successor device at generation + 1","selection":"feature asset-local-backends only (test hosts): DeviceOptions.backend = auto|vulkan|dx12|metal|none; no product build enables it"}"#;

/// Where the transient adapter runs (its candidate order is a pure function of it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    Windows,
    Apple,
    Other,
}
impl Platform {
    pub(crate) const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_vendor = "apple") {
            Self::Apple
        } else {
            Self::Other
        }
    }
    /// The transient adapter's candidate order: Windows Vulkan then DX12 (Vulkan is the only
    /// product backend; DX12 stays a recorded fallback, never a Vulkan proof), Apple Metal,
    /// elsewhere Vulkan.
    pub(crate) const fn candidates(self) -> &'static [GpuApi] {
        match self {
            Self::Windows => &[GpuApi::Vulkan, GpuApi::Dx12],
            Self::Apple => &[GpuApi::Metal],
            Self::Other => &[GpuApi::Vulkan],
        }
    }
}

/// Options of the transient adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransientOptions {
    /// Residency budget declared on every grant (`studio_declared`, asset-local; default 256 MiB).
    pub residency_budget_bytes: u64,
    /// Bound on adapter and device requests.
    pub acquire_timeout: Duration,
    /// Asset-local backend selection (feature `asset-local-backends`, test hosts only).
    #[cfg(feature = "asset-local-backends")]
    pub backend: crate::device::BackendChoice,
}
impl Default for TransientOptions {
    fn default() -> Self {
        Self::from(&DeviceOptions::default())
    }
}
impl From<&DeviceOptions> for TransientOptions {
    fn from(options: &DeviceOptions) -> Self {
        Self {
            residency_budget_bytes: options.residency_budget_bytes,
            acquire_timeout: options.acquire_timeout,
            #[cfg(feature = "asset-local-backends")]
            backend: options.backend,
        }
    }
}
impl TransientOptions {
    fn candidates(&self) -> &'static [GpuApi] {
        #[cfg(feature = "asset-local-backends")]
        {
            self.backend.candidates(Platform::current())
        }
        #[cfg(not(feature = "asset-local-backends"))]
        {
            Platform::current().candidates()
        }
    }
}

/// The asset-local GPU owner stand-in (see the module docs and [`TRANSIENT_DESCRIPTOR`]).
#[derive(Debug)]
pub struct TransientAdapter {
    options: TransientOptions,
    ledger: GrantLedger,
    next_device: AtomicU64,
}
impl TransientAdapter {
    pub fn new(options: TransientOptions) -> Self {
        Self {
            options,
            ledger: GrantLedger::new(),
            next_device: AtomicU64::new(1),
        }
    }
    pub fn options(&self) -> TransientOptions {
        self.options
    }
    fn refusal_of(&self, error: GpuError) -> ClientRefusal {
        let refusal = match error {
            GpuError::Unavailable(reason) => ClientRefusal::Unavailable(reason),
            GpuError::Backend(BackendError::Canceled) => ClientRefusal::Canceled,
            GpuError::DeviceLost { generation } => ClientRefusal::Lost { generation },
            // acquire_lease returns no other error; keep any other as "no device here"
            _ => ClientRefusal::Unavailable(Reason::NoDevice),
        };
        self.ledger.refuse(refusal)
    }
    fn parts(&self, device: DeviceRef, generation: u64, cancel: &CancellationToken) -> Result<GrantParts, ClientRefusal> {
        let lease = acquire_lease(self.options.candidates(), self.options.acquire_timeout, cancel)
            .map_err(|e| self.refusal_of(e))?;
        Ok(GrantParts {
            generation,
            budget_bytes: self.options.residency_budget_bytes,
            budget_source: BudgetSource::StudioDeclared,
            device,
            backend: lease.backend_key(),
            issuer: GrantIssuer::TransientAssetLocal,
            lease: Some(Box::new(lease)),
        })
    }
    /// A grant at `generation` whose device is already lost (feature `test-faults`): the state the
    /// real device-lost callback latches, without hardware (`GpuBackend::with_injected_loss`).
    #[cfg(feature = "test-faults")]
    pub(crate) fn inject_lost_grant(&self, request: &AdmissionRequest, generation: u64) -> GpuGrant {
        self.ledger.issue(
            request,
            GrantParts {
                generation,
                budget_bytes: self.options.residency_budget_bytes,
                budget_source: BudgetSource::StudioDeclared,
                device: DeviceRef::Id(self.next_device.fetch_add(1, Ordering::Relaxed)),
                backend: "",
                issuer: GrantIssuer::TransientAssetLocal,
                lease: None,
            },
        )
    }
}

impl GpuClientPort for TransientAdapter {
    /// Order: cancel, device selection (only `Any`: the transient adapter shares no device), the
    /// budget, then one headless device on the candidate order.
    fn admit(
        &self,
        request: &AdmissionRequest,
        cancel: &CancellationToken,
    ) -> Result<GpuGrant, ClientRefusal> {
        if cancel.check().is_err() {
            return Err(self.ledger.refuse(ClientRefusal::Canceled));
        }
        if request.device != DeviceRef::Any {
            return Err(self.ledger.refuse(ClientRefusal::Unavailable(Reason::NoDevice)));
        }
        self.ledger.check_budget(request, self.options.residency_budget_bytes)?;
        let device = DeviceRef::Id(self.next_device.fetch_add(1, Ordering::Relaxed));
        let parts = self.parts(device, 1, cancel)?;
        Ok(self.ledger.issue(request, parts))
    }
    fn authorize(&self, grant: &GpuGrant) -> Result<(), ClientRefusal> {
        self.ledger.authorize(grant)
    }
    fn settle(&self, grant: &GpuGrant, settlement: Settlement) -> Result<SettleReceipt, SettleError> {
        self.ledger.settle(grant, settlement)
    }
    /// A successor device at generation + 1 with the grant's attribution; a failed acquisition
    /// keeps the old grant open (a lost device stays lost, stale generations stay refused).
    fn rebind(
        &self,
        grant: &GpuGrant,
        cancel: &CancellationToken,
    ) -> Result<GpuGrant, ClientRefusal> {
        if cancel.check().is_err() {
            return Err(self.ledger.refuse(ClientRefusal::Canceled));
        }
        let generation = self.ledger.check_rebind(grant)?;
        let parts = self.parts(grant.device(), generation.saturating_add(1), cancel)?;
        self.ledger.rebind_with(grant, parts)
    }
    fn counters(&self) -> PortCounters {
        self.ledger.counters()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The candidate order per target is a pure function: Vulkan first on Windows (DX12 only as the
    /// recorded fallback), Metal on Apple, Vulkan elsewhere (MT-36248 AC-HOST-005).
    #[test]
    fn candidate_order_is_vulkan_first_on_windows() {
        assert_eq!(Platform::Windows.candidates(), &[GpuApi::Vulkan, GpuApi::Dx12]);
        assert_eq!(Platform::Apple.candidates(), &[GpuApi::Metal]);
        assert_eq!(Platform::Other.candidates(), &[GpuApi::Vulkan]);
        if cfg!(windows) {
            assert_eq!(Platform::current(), Platform::Windows);
            assert_eq!(TransientOptions::default().candidates()[0], GpuApi::Vulkan);
        }
        assert!(TRANSIENT_DESCRIPTOR.contains(r#""windows":["vulkan","dx12"]"#));
        assert!(TRANSIENT_DESCRIPTOR.contains(r#""asset_local":true"#));
    }
}
