//! External-device adapter (feature `shared-device`, lane SMB-GPU-CLIENT-PORT-1): the port behind
//! `SharedDevice`. The device and queue are the caller's (eframe's in the asset-local test host);
//! this adapter opens nothing. Its grants are `Host`-shaped (the device belongs to the host) with
//! the budget the caller's `DeviceOptions` declare (`studio_declared`) until a host supplies one.
//! Loss detection and recovery stay the host's: `rebind` is `Lost { generation }`, never a device
//! of our own.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    AdmissionRequest, BudgetSource, ClientRefusal, DeviceRef, GpuClientPort, GpuGrant, GrantIssuer,
    GrantLedger, GrantParts, PortCounters, SettleError, SettleReceipt, Settlement,
};
use crate::device::DeviceLease;
use crate::error::Reason;
use hsk_studio_accord::CancellationToken;

pub(crate) struct ExternalPort {
    ledger: GrantLedger,
    lease: DeviceLease,
    budget_bytes: u64,
}
impl ExternalPort {
    pub(crate) fn new(lease: DeviceLease, budget_bytes: u64) -> Self {
        Self {
            ledger: GrantLedger::new(),
            lease,
            budget_bytes,
        }
    }
}
/// The one external device of this port.
const DEVICE: DeviceRef = DeviceRef::Id(1);

impl GpuClientPort for ExternalPort {
    fn admit(
        &self,
        request: &AdmissionRequest,
        cancel: &CancellationToken,
    ) -> Result<GpuGrant, ClientRefusal> {
        if cancel.check().is_err() {
            return Err(self.ledger.refuse(ClientRefusal::Canceled));
        }
        if !matches!(request.device, DeviceRef::Any) && request.device != DEVICE {
            return Err(self.ledger.refuse(ClientRefusal::Unavailable(Reason::NoDevice)));
        }
        self.ledger.check_budget(request, self.budget_bytes)?;
        Ok(self.ledger.issue(
            request,
            GrantParts {
                generation: 1,
                budget_bytes: self.budget_bytes,
                budget_source: BudgetSource::StudioDeclared,
                device: DEVICE,
                backend: self.lease.backend_key(),
                issuer: GrantIssuer::Host,
                lease: Some(Box::new(self.lease.clone())),
            },
        ))
    }
    fn authorize(&self, grant: &GpuGrant) -> Result<(), ClientRefusal> {
        self.ledger.authorize(grant)
    }
    fn settle(&self, grant: &GpuGrant, settlement: Settlement) -> Result<SettleReceipt, SettleError> {
        self.ledger.settle(grant, settlement)
    }
    fn rebind(&self, grant: &GpuGrant, _: &CancellationToken) -> Result<GpuGrant, ClientRefusal> {
        let generation = self.ledger.check_rebind(grant)?;
        Err(self.ledger.refuse(ClientRefusal::Lost { generation }))
    }
    fn counters(&self) -> PortCounters {
        self.ledger.counters()
    }
}
