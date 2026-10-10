//! Admitted immutable proposals. These ports do not authenticate or commit a document.
use crate::{EpochPort, Error, Limits, Network, Operand, Request, StyleRead, WorkMeter};
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{DeliveryError, SinkPort};
use std::{
    alloc::Layout,
    sync::{Arc, atomic::AtomicUsize},
};
#[derive(Clone, Copy, Debug)]
pub struct GeometryRead<'a> {
    pub revision: u64,
    pub encoded_path_bytes: &'a [u8],
}
#[derive(Clone, Copy, Debug)]
pub struct OrderRead<'a> {
    pub revision: u64,
    pub bottom_to_top: &'a [&'a hsk_studio_accord::DomainId],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleKind {
    Paint,
    Unsupported,
}
#[derive(Clone, Copy, Debug)]
pub struct ResolvedStyle<'a> {
    pub kind: StyleKind,
    pub revision: u64,
    pub encoded_record_bytes: &'a [u8],
}
/// The caller owns membership, current bytes/order, and the PAINT fill stack.
/// Implementations return distinct AbsentRead/UnavailableRead; no default revision.
pub trait ContextPort: EpochPort {
    fn member(&self, request: &Request<'_>, operand: &Operand<'_>) -> Result<(), Error>;
    fn geometry(&self, operand: &Operand<'_>) -> Result<GeometryRead<'_>, Error>;
    fn order(&self, request: &Request<'_>) -> Result<OrderRead<'_>, Error>;
    fn style(&self, read: &StyleRead<'_>) -> Result<ResolvedStyle<'_>, Error>;
    fn target_revision(&self, request: &Request<'_>) -> Result<u64, Error>;
}
#[derive(Clone, Copy, Debug)]
pub struct InputCharge {
    pub lifetime_id: u64,
    pub requested_bytes: u64,
}
/// Already admitted inputs/context/token. Verify the complete request's borrowed allocations
/// once, including alias/shared ownership. The caller keeps this lifetime charged while borrowed.
pub trait InputLifetime: Send + Sync {
    fn verify(&self, request: &Request<'_>) -> Result<InputCharge, Error>;
}
pub trait RetirementPort: Send + Sync {
    fn retire(&self, handle: u64, requested_bytes: u64);
}
pub struct Reservation<'a> {
    port: &'a dyn RetirementPort,
    handle: u64,
    requested_bytes: u64,
}
impl<'a> Reservation<'a> {
    /// The caller has already reserved exactly these bytes. This constructor allocates nothing.
    pub fn new(
        port: &'a dyn RetirementPort,
        handle: u64,
        requested_bytes: u64,
    ) -> Result<Self, Error> {
        if handle == 0 || requested_bytes == 0 {
            return Err(Error::LeaseUnavailable);
        }
        Ok(Self {
            port,
            handle,
            requested_bytes,
        })
    }
    pub fn requested_bytes(&self) -> u64 {
        self.requested_bytes
    }
}
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.port.retire(self.handle, self.requested_bytes);
    }
}
/// A definite synchronous caller reservation. No unknown/zero grant can cover positive work.
pub trait AdmissionPort: Send + Sync {
    fn reserve(
        &self,
        requested_bytes: u64,
        simultaneous_limit: u64,
    ) -> Result<Reservation<'_>, Error>;
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub input_bytes: u64,
    pub input_anchors: u64,
    pub input_segments: u64,
    pub output_vertices: u64,
    pub output_segments: u64,
    pub output_regions: u64,
    pub output_loops: u64,
    pub intersections: u64,
    pub sweep_events: u64,
    pub work_units: u64,
    pub requested_owned_bytes: u64,
    pub precharged_input_bytes: u64,
    pub retained_owned_bytes: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalDisposition {
    Prepared,
    Accepted,
    Rejected,
    ReconciliationRequired,
}
#[derive(Clone, Copy, Debug)]
pub struct Inspection {
    pub disposition: ProposalDisposition,
    pub error: Option<Error>,
    pub counts: Counts,
    pub cancel_epoch: u64,
    pub delivery: Option<DeliveryError>,
    pub diagnostic_delivery: Option<DeliveryError>,
}
#[derive(Clone, Copy, Debug)]
pub struct PrecisionReceipt {
    pub requested_precision: hsk_studio_accord::Length,
    pub provider: &'static str,
    pub effective_tolerance_pt: f64,
    pub maximum_error_pt: f64,
    pub exact: bool,
    pub geometry_loss: bool,
    pub style_loss: bool,
}
struct Retained<'a> {
    request: Request<'a>,
    geometry: Network<'a>,
    inspection: Inspection,
    precision: PrecisionReceipt,
    _input_lifetime: &'a dyn InputLifetime,
    reservation: Reservation<'a>,
}
/// Private Arc ownership, no Weak/mutable reservation escape. The last owner's drop obtains
/// the payload only after the Arc control allocation has been freed, then retires its charge.
pub struct Prepared<'a> {
    inner: Option<Arc<Retained<'a>>>,
}
impl Clone for Prepared<'_> {
    fn clone(&self) -> Self {
        Self {
            inner: Some(Arc::clone(self.inner.as_ref().expect("live proposal"))),
        }
    }
}
impl Drop for Prepared<'_> {
    fn drop(&mut self) {
        if let Some(value) = Arc::into_inner(self.inner.take().expect("live proposal")) {
            drop(value);
        }
    }
}
impl<'a> Prepared<'a> {
    fn data(&self) -> &Retained<'a> {
        self.inner.as_ref().expect("live proposal")
    }
    pub fn request(&self) -> &Request<'a> {
        &self.data().request
    }
    pub fn geometry(&self) -> &Network<'a> {
        &self.data().geometry
    }
    pub fn inspection(&self) -> Inspection {
        self.data().inspection
    }
    pub fn precision_receipt(&self) -> PrecisionReceipt {
        self.data().precision
    }
    pub fn previous_revision(&self) -> u64 {
        self.data().request.target.expected_revision
    }
    pub fn revision(&self) -> u64 {
        self.previous_revision() + 1
    }
    pub fn reserved_bytes(&self) -> u64 {
        self.data().reservation.requested_bytes()
    }
    pub fn source_provenance(&self) -> impl Iterator<Item = &Operand<'a>> {
        self.data().request.z_order.iter().map(|id| {
            self.data()
                .request
                .operands
                .iter()
                .find(|o| o.path.path_id == *id)
                .expect("validated order")
        })
    }
}
#[repr(C, align(2))]
struct ArcHeader {
    strong: AtomicUsize,
    weak: AtomicUsize,
    data: (),
}
pub fn required_owned_bytes() -> Result<u64, Error> {
    let layout = Layout::new::<ArcHeader>()
        .extend(Layout::new::<Retained<'_>>())
        .map_err(|_| Error::Overflow)?
        .0
        .pad_to_align();
    u64::try_from(layout.size()).map_err(|_| Error::Overflow)
}
#[derive(Clone, Copy, Debug)]
pub struct Rejected {
    pub error: Error,
    pub inspection: Inspection,
}
fn rejected(error: Error, counts: Counts, epoch: u64) -> Rejected {
    Rejected {
        error,
        inspection: Inspection {
            disposition: ProposalDisposition::Rejected,
            error: Some(error),
            counts,
            cancel_epoch: epoch,
            delivery: None,
            diagnostic_delivery: None,
        },
    }
}
fn hash(bytes: &[u8], limits: &Limits, work: &mut WorkMeter<'_>) -> Result<[u8; 32], Error> {
    use sha2::{Digest, Sha256};
    let len = u64::try_from(bytes.len()).map_err(|_| Error::Overflow)?;
    if len > limits.input_bytes || len > u64::MAX / 8 {
        return Err(Error::BudgetExceeded);
    }
    let mut hash = Sha256::new();
    for chunk in bytes.chunks(256) {
        work.step()?;
        hash.update(chunk);
    }
    work.step()?;
    Ok(hash.finalize().into())
}
fn read_set(
    request: &Request<'_>,
    context: &(impl ContextPort + ?Sized),
    work: &mut WorkMeter<'_>,
) -> Result<(), Error> {
    work.step()?;
    for operand in request.operands {
        work.step()?;
        context.member(request, operand)?;
        let current = context.geometry(operand)?;
        if current.revision != operand.expected_revision {
            return Err(Error::StaleRevision);
        }
        if hash(current.encoded_path_bytes, &request.limits, work)? != operand.geometry_sha256 {
            return Err(Error::HashMismatch);
        }
    }
    work.step()?;
    let order = context.order(request)?;
    if order.revision != request.expected_z_order_revision || order.bottom_to_top != request.z_order
    {
        return Err(Error::StaleRevision);
    }
    for read in request.style_reads {
        work.step()?;
        let style = context.style(read)?;
        if style.kind != StyleKind::Paint {
            return Err(Error::UnsupportedStyle);
        }
        if style.revision != read.expected_revision {
            return Err(Error::StaleRevision);
        }
        if hash(style.encoded_record_bytes, &request.limits, work)?
            != read.expected_fingerprint_sha256
        {
            return Err(Error::HashMismatch);
        }
    }
    work.step()?;
    if context.target_revision(request)? != request.target.expected_revision {
        return Err(Error::StaleRevision);
    }
    work.check()
}
fn validate(request: &Request<'_>) -> Result<usize, Error> {
    use hsk_studio_accord::{SCHEMA_STUDIO_VECTOR_PATH, Unit};
    if request.transport_version != 1
        || request.document_id.prefix() != "SDOC"
        || !crate::path_contract::valid_key(request.command_id)
        || request.operands.len() != 2
        || request.z_order.len() != 2
    {
        return Err(Error::InvalidRequest);
    }
    if request.limits.operands < 2
        || request.limits.anchors < 8
        || request.limits.segments < 8
        || request.limits.recursion_depth < 5
    {
        return Err(Error::BudgetExceeded);
    }
    request.target.address.validate()?;
    request
        .target
        .expected_revision
        .checked_add(1)
        .ok_or(Error::Overflow)?;
    let options = request.options;
    if options.precision.unit() != Unit::Points
        || !options.precision.value().is_finite()
        || options.precision.value() <= 0.0
    {
        return Err(Error::InvalidRequest);
    }
    if options.remove_redundant_points || options.divide_and_outline_remove_unpainted {
        return Err(Error::UnsupportedOptions);
    }
    if request.result_fill_style_id.prefix() != "SSTY" {
        return Err(Error::InvalidRequest);
    }
    for operand in request.operands {
        let path = operand.path;
        if path.schema_id != SCHEMA_STUDIO_VECTOR_PATH || path.path_id.prefix() != "SVPT" {
            return Err(Error::InvalidGeometry);
        }
        path.address.validate()?;
        if path.fill_style_id.is_none() && path.winding_rule != crate::Winding::None {
            return Err(Error::UnsupportedStyle);
        }
        if path.fill_style_id.is_some_and(|id| id.prefix() != "SSTY") {
            return Err(Error::InvalidRequest);
        }
    }
    if request.operands[0].path.path_id == request.operands[1].path.path_id
        || request.operands[0].path.address == request.operands[1].path.address
        || request.z_order[0] == request.z_order[1]
    {
        return Err(Error::InvalidRequest);
    }
    let bottom = request
        .operands
        .iter()
        .position(|o| o.path.path_id == request.z_order[0])
        .ok_or(Error::InvalidRequest)?;
    if request.operands[1 - bottom].path.path_id != request.z_order[1] {
        return Err(Error::InvalidRequest);
    }
    if request.style_reads.len() > 3 {
        return Err(Error::InvalidRequest);
    }
    for (i, read) in request.style_reads.iter().enumerate() {
        if read.style_id.prefix() != "SSTY"
            || request.style_reads[..i]
                .iter()
                .any(|x| x.style_id == read.style_id)
        {
            return Err(Error::InvalidRequest);
        }
        if read.style_id != request.result_fill_style_id
            && !request
                .operands
                .iter()
                .any(|o| o.path.fill_style_id == Some(read.style_id))
        {
            return Err(Error::InvalidRequest);
        }
    }
    for id in request
        .operands
        .iter()
        .filter_map(|o| o.path.fill_style_id)
        .chain(std::iter::once(request.result_fill_style_id))
    {
        if !request.style_reads.iter().any(|r| r.style_id == id) {
            return Err(Error::UnavailableStyle);
        }
    }
    Ok(bottom)
}
pub fn prepare<'a>(
    request: Request<'a>,
    context: &impl ContextPort,
    input_lifetime: &'a dyn InputLifetime,
    admission: &'a impl AdmissionPort,
    cancel: &CancellationToken,
) -> Result<Prepared<'a>, Rejected> {
    let mut counts = Counts::default();
    let mut work = WorkMeter::new(
        cancel,
        context,
        request.cancel_epoch,
        request.limits.work_units,
    );
    work.set_geometry_limits(request.limits.intersections, request.limits.sweep_events);
    let result = (|| {
        work.step()?;
        let bottom = validate(&request)?;
        for operand in request.operands {
            counts.input_bytes = counts
                .input_bytes
                .checked_add(operand.encoded_path_bytes.len() as u64)
                .ok_or(Error::Overflow)?;
            counts.input_anchors = counts
                .input_anchors
                .checked_add(operand.path.anchors.len() as u64)
                .ok_or(Error::Overflow)?;
        }
        counts.input_segments = counts.input_anchors;
        if counts.input_bytes > request.limits.input_bytes
            || counts.input_anchors > request.limits.anchors
            || counts.input_segments > request.limits.segments
        {
            return Err(Error::BudgetExceeded);
        }
        work.step()?;
        let input = input_lifetime.verify(&request)?;
        if input.lifetime_id == 0 || input.requested_bytes == 0 {
            return Err(Error::LeaseUnavailable);
        }
        counts.precharged_input_bytes = input.requested_bytes;
        for operand in request.operands {
            if crate::encoded::verify_encoded_path(
                operand.encoded_path_bytes,
                &operand.path,
                &request.limits,
                &mut work,
            )? != operand.geometry_sha256
            {
                return Err(Error::HashMismatch);
            }
        }
        read_set(&request, context, &mut work)?;
        let bytes = required_owned_bytes()?;
        if bytes
            .checked_add(input.requested_bytes)
            .ok_or(Error::Overflow)?
            > request.limits.requested_allocation_bytes
        {
            return Err(Error::BudgetExceeded);
        }
        // No allocating provider path is reachable; reserve the one retained Arc layout before construction.
        work.step()?;
        let reservation = admission.reserve(bytes, request.limits.requested_allocation_bytes)?;
        if reservation.requested_bytes() != bytes {
            return Err(Error::LeaseUnavailable);
        }
        counts.requested_owned_bytes = bytes;
        let geometry = crate::provider::evaluate(
            [&request.operands[0].path, &request.operands[1].path],
            request.operation,
            bottom,
            request.result_fill_style_id,
            &request.limits,
            &mut work,
        )?;
        work.check()?;
        counts.output_vertices = geometry.vertices().len() as u64;
        counts.output_segments = geometry.segments().len() as u64;
        counts.output_regions = geometry.regions().len() as u64;
        counts.output_loops = geometry.loops().len() as u64;
        counts.intersections = work.intersections();
        counts.sweep_events = work.sweep_events();
        counts.work_units = work.used();
        counts.retained_owned_bytes = bytes;
        let inspection = Inspection {
            disposition: ProposalDisposition::Prepared,
            error: None,
            counts,
            cancel_epoch: request.cancel_epoch,
            delivery: None,
            diagnostic_delivery: None,
        };
        let precision = PrecisionReceipt {
            requested_precision: request.options.precision,
            provider: "linesweeper0.4.0-exact-axis-adaptation/vectorcraft825d4fdb/kurbo0.13.1",
            effective_tolerance_pt: 0.0,
            maximum_error_pt: 0.0,
            exact: true,
            geometry_loss: false,
            style_loss: false,
        };
        Ok(Prepared {
            inner: Some(Arc::new(Retained {
                request,
                geometry,
                inspection,
                precision,
                _input_lifetime: input_lifetime,
                reservation,
            })),
        })
    })();
    result.map_err(|error| {
        counts.intersections = work.intersections();
        counts.sweep_events = work.sweep_events();
        counts.work_units = work.used();
        counts.retained_owned_bytes = 0;
        rejected(error, counts, request.cancel_epoch)
    })
}

/// Caller serializes the complete read set, sink and result transfer in this boundary.
/// Read/sink callbacks must obey its lock; no document mutation is performed by Nib.
pub trait FinalizationPort {
    fn with_exclusive<T>(
        &mut self,
        call: impl FnOnce(&dyn ContextPort, &mut dyn SinkPort) -> T,
    ) -> Result<T, Error>;
    fn reconcile_delivery(&mut self, token: &PendingKey) -> Result<Reconciliation, Error>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconciliation {
    Accepted,
    Rejected,
    StillIndeterminate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingKey {
    bytes: [u8; 64],
    len: usize,
}
impl PendingKey {
    fn new(request: &Request<'_>) -> Self {
        let mut value = Self {
            bytes: [0; 64],
            len: 0,
        };
        for b in b"nib-pending-" {
            value.bytes[value.len] = *b;
            value.len += 1;
        }
        for n in [
            request.correlation_id,
            request.cancel_epoch,
            request.target.expected_revision,
        ] {
            for shift in (0..16).rev() {
                value.bytes[value.len] = b"0123456789abcdef"[((n >> (shift * 4)) & 15) as usize];
                value.len += 1;
            }
        }
        value
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("fixed ASCII token")
    }
}
/// Each public token clone retains the same admitted result owner; resolving a pending
/// wrapper cannot uncharge buffers still reachable through a token or proposal clone.
pub struct PendingToken<'a> {
    key: PendingKey,
    proposal: Prepared<'a>,
    inspection: Inspection,
}
impl Clone for PendingToken<'_> {
    fn clone(&self) -> Self {
        Self {
            key: self.key,
            proposal: self.proposal.clone(),
            inspection: self.inspection,
        }
    }
}
impl<'a> PendingToken<'a> {
    pub fn key(&self) -> PendingKey {
        self.key
    }
    pub fn inspection(&self) -> Inspection {
        self.inspection
    }
    pub fn proposal(&self) -> &Prepared<'a> {
        &self.proposal
    }
}
pub enum Finalized<'a> {
    Accepted {
        proposal: Prepared<'a>,
        inspection: Inspection,
    },
    Rejected {
        error: Error,
        inspection: Inspection,
    },
    ReconciliationRequired {
        token: PendingToken<'a>,
        inspection: Inspection,
    },
}
impl Finalized<'_> {
    pub fn inspection(&self) -> Inspection {
        match self {
            Self::Accepted { inspection, .. }
            | Self::Rejected { inspection, .. }
            | Self::ReconciliationRequired { inspection, .. } => *inspection,
        }
    }
}
fn geometry_detail<'a>(
    request: &Request<'a>,
    inspection: Inspection,
    delivery_attempts: u64,
) -> Result<hsk_studio_observe::GeometryDetail<'a>, Error> {
    use hsk_studio_observe::{
        GeometryAddress, GeometryBudget, GeometryCode, GeometryCounts, GeometryDetail,
        GeometryDisposition, GeometryRead,
    };
    let counts = inspection.counts;
    let output = if inspection.disposition == ProposalDisposition::Rejected {
        [0; 4]
    } else {
        [
            counts.output_vertices,
            counts.output_segments,
            counts.output_regions,
            counts.output_loops,
        ]
    };
    let budget = GeometryBudget {
        count_limit: 1_048_576,
        work_limit: request.limits.work_units,
        byte_limit: request.limits.requested_allocation_bytes,
        delivery_limit: 1,
    };
    let constructor = if inspection.disposition == ProposalDisposition::Rejected {
        GeometryCounts::for_rejection
    } else {
        GeometryCounts::new
    };
    let counts = constructor(
        [
            counts.input_anchors,
            counts.input_segments,
            output[0],
            output[1],
            output[2],
            output[3],
            counts.intersections,
            counts.work_units,
        ],
        [
            counts.requested_owned_bytes,
            counts.precharged_input_bytes,
            counts.retained_owned_bytes,
        ],
        delivery_attempts,
        budget,
    )
    .map_err(|_| Error::BudgetExceeded)?;
    let disposition = match inspection.disposition {
        ProposalDisposition::Prepared => GeometryDisposition::Prepared,
        ProposalDisposition::Accepted => GeometryDisposition::Accepted,
        ProposalDisposition::Rejected => GeometryDisposition::Rejected,
        ProposalDisposition::ReconciliationRequired => GeometryDisposition::ReconciliationRequired,
    };
    let code = inspection.error.map(|error| match error {
        Error::UnsupportedOperation
        | Error::UnsupportedOptions
        | Error::UnsupportedApproximation
        | Error::UnsupportedStyle => GeometryCode::Unsupported,
        Error::EmptyPathfinderResult => GeometryCode::Empty,
        Error::UnavailableRead | Error::AbsentRead => GeometryCode::UnavailableRead,
        Error::UnavailableStyle => GeometryCode::UnavailableStyle,
        Error::StaleRevision => GeometryCode::RevisionConflict,
        Error::StaleEpoch => GeometryCode::StaleEpoch,
        Error::Overflow => GeometryCode::Overflow,
        Error::Cancelled => GeometryCode::Canceled,
        Error::BudgetExceeded => GeometryCode::BudgetExceeded,
        Error::LeaseUnavailable => GeometryCode::LeaseUnavailable,
        Error::DeliveryRejected => GeometryCode::DeliveryRejected,
        Error::DeliveryUnavailable => GeometryCode::DeliveryUnavailable,
        Error::ReconciliationRequired => GeometryCode::ReconciliationRequired,
        Error::HashMismatch => GeometryCode::HashMismatch,
        _ => GeometryCode::Validation,
    });
    let address = GeometryAddress::new(
        request.target.address.layer_id,
        request.target.address.object_key,
    )
    .ok();
    let read = GeometryRead {
        source_path_id: request
            .operands
            .first()
            .filter(|o| o.path.path_id.prefix() == "SVPT")
            .map(|o| o.path.path_id),
        read_revision: request.operands.first().map(|o| o.expected_revision),
        target_revision: Some(request.target.expected_revision),
        cancel_epoch: Some(request.cancel_epoch),
    };
    GeometryDetail::new(disposition, code, address, read, counts).map_err(|_| Error::InvalidRequest)
}
fn delivery_error(error: hsk_studio_observe::Error) -> Error {
    match error {
        hsk_studio_observe::Error::Delivery(DeliveryError::Indeterminate)
        | hsk_studio_observe::Error::ReconciliationRequired => Error::ReconciliationRequired,
        hsk_studio_observe::Error::Delivery(DeliveryError::Unavailable) => {
            Error::DeliveryUnavailable
        }
        _ => Error::DeliveryRejected,
    }
}
fn diagnostic_error(result: Result<hsk_studio_observe::Receipt, Error>) -> Option<DeliveryError> {
    result.err().map(|error| match error {
        Error::ReconciliationRequired => DeliveryError::Indeterminate,
        Error::DeliveryUnavailable => DeliveryError::Unavailable,
        _ => DeliveryError::Rejected,
    })
}
/// Typed diagnostic projection for every preparation failure; no rejected geometry exposed.
pub fn emit_rejection(
    request: &Request<'_>,
    rejection: Rejected,
    observer: &mut hsk_studio_observe::Observe,
    cancel: &CancellationToken,
    sink: &mut impl SinkPort,
) -> Result<hsk_studio_observe::Receipt, Error> {
    let detail = geometry_detail(request, rejection.inspection, 1)?;
    observer
        .emit_geometry(
            hsk_studio_observe::Observation {
                correlation_id: request.correlation_id,
                revision: request.target.expected_revision,
                outcome: hsk_studio_observe::Outcome::Failure(
                    hsk_studio_observe::FailureCode::Validation,
                ),
                progress: None,
                private_project_text: None,
            },
            &detail,
            cancel,
            sink,
        )
        .map_err(delivery_error)
}
/// A source proposal acceptance, never an authenticated/durable document commit.
/// The exclusive port MUST serialize all context/sink callbacks with foreign context changes.
/// Sink acceptance linearizes the proposal and its retained lease; subsequent cancellation
/// cannot erase it. A separate fallback diagnostic sink reports a boundary-entry failure.
pub fn finalize<'a>(
    proposal: Prepared<'a>,
    port: &mut impl FinalizationPort,
    observer: &mut hsk_studio_observe::Observe,
    diagnostics: &mut hsk_studio_observe::Observe,
    cancel: &CancellationToken,
    fallback_sink: &mut impl SinkPort,
) -> Finalized<'a> {
    let request = *proposal.request();
    let mut inspection = proposal.inspection();
    let result = port
        .with_exclusive(|context, sink| {
            let mut work = WorkMeter::new(
                cancel,
                context,
                request.cancel_epoch,
                request.limits.work_units,
            );
            let fence = work
                .resume(inspection.counts.work_units)
                .and_then(|_| read_set(&request, context, &mut work));
            inspection.counts.work_units = work.used();
            if let Err(error) = fence {
                inspection.disposition = ProposalDisposition::Rejected;
                inspection.error = Some(error);
                inspection.counts.retained_owned_bytes = 0;
                return Err(error);
            }
            inspection.disposition = ProposalDisposition::Accepted;
            let detail = geometry_detail(&request, inspection, 1)?;
            let receipt = observer.emit_geometry(
                hsk_studio_observe::Observation {
                    correlation_id: request.correlation_id,
                    revision: request.target.expected_revision,
                    outcome: hsk_studio_observe::Outcome::Success,
                    progress: None,
                    private_project_text: None,
                },
                &detail,
                cancel,
                sink,
            );
            match receipt {
                Ok(receipt) if receipt.outcome == hsk_studio_observe::Outcome::Canceled => {
                    Err(Error::Cancelled)
                }
                Ok(_) => Ok(()),
                Err(
                    hsk_studio_observe::Error::Delivery(DeliveryError::Indeterminate)
                    | hsk_studio_observe::Error::ReconciliationRequired,
                ) => {
                    inspection.delivery = Some(DeliveryError::Indeterminate);
                    Err(Error::ReconciliationRequired)
                }
                Err(hsk_studio_observe::Error::Delivery(DeliveryError::Unavailable)) => {
                    inspection.delivery = Some(DeliveryError::Unavailable);
                    Err(Error::DeliveryUnavailable)
                }
                Err(_) => {
                    inspection.delivery = Some(DeliveryError::Rejected);
                    Err(Error::DeliveryRejected)
                }
            }
        })
        .and_then(|result| result);
    match result {
        Ok(()) => Finalized::Accepted {
            proposal,
            inspection,
        },
        Err(Error::ReconciliationRequired) => {
            inspection.disposition = ProposalDisposition::ReconciliationRequired;
            inspection.error = Some(Error::ReconciliationRequired);
            // The initial observer's indeterminate state is not retried. This exact typed state
            // is retained for a fresh explicit reconciliation diagnostic below.
            let mut token = PendingToken {
                key: PendingKey::new(&request),
                proposal,
                inspection,
            };
            inspection.diagnostic_delivery =
                diagnostic_error(emit_pending(&token, diagnostics, cancel, fallback_sink));
            token.inspection = inspection;
            Finalized::ReconciliationRequired { token, inspection }
        }
        Err(error) => {
            inspection.disposition = ProposalDisposition::Rejected;
            inspection.error = Some(error);
            inspection.counts.retained_owned_bytes = 0;
            inspection.counts.output_vertices = 0;
            inspection.counts.output_segments = 0;
            inspection.counts.output_regions = 0;
            inspection.counts.output_loops = 0;
            inspection.diagnostic_delivery = diagnostic_error(emit_rejection(
                &request,
                Rejected { error, inspection },
                diagnostics,
                cancel,
                fallback_sink,
            ));
            drop(proposal);
            Finalized::Rejected { error, inspection }
        }
    }
}
/// Records the actual pending state through a fresh caller-preowned observer. This diagnostic
/// transition never resends or recomputes the original geometry delivery.
pub fn emit_pending(
    token: &PendingToken<'_>,
    observer: &mut hsk_studio_observe::Observe,
    cancel: &CancellationToken,
    sink: &mut impl SinkPort,
) -> Result<hsk_studio_observe::Receipt, Error> {
    let request = token.proposal.request();
    let detail = geometry_detail(request, token.inspection, 1)?;
    observer
        .emit_geometry(
            hsk_studio_observe::Observation {
                correlation_id: request.correlation_id,
                revision: request.target.expected_revision,
                outcome: hsk_studio_observe::Outcome::Failure(
                    hsk_studio_observe::FailureCode::Unavailable,
                ),
                progress: None,
                private_project_text: None,
            },
            &detail,
            cancel,
            sink,
        )
        .map_err(delivery_error)
}
/// Uses a fresh observer for the resolved transition, never retries the original sender.
pub fn reconcile<'a>(
    mut token: PendingToken<'a>,
    port: &mut impl FinalizationPort,
    observer: &mut hsk_studio_observe::Observe,
    diagnostics: &mut hsk_studio_observe::Observe,
    cancel: &CancellationToken,
    fallback_sink: &mut impl SinkPort,
) -> Finalized<'a> {
    let decision = port.reconcile_delivery(&token.key);
    let request = *token.proposal.request();
    let mut inspection = token.inspection;
    let resolved = port
        .with_exclusive(|context, sink| {
            let disposition = match decision {
                Ok(Reconciliation::Accepted) => {
                    let mut work = WorkMeter::new(
                        cancel,
                        context,
                        request.cancel_epoch,
                        request.limits.work_units,
                    );
                    let fence = work
                        .resume(inspection.counts.work_units)
                        .and_then(|_| read_set(&request, context, &mut work));
                    inspection.counts.work_units = work.used();
                    fence?;
                    ProposalDisposition::Accepted
                }
                Ok(Reconciliation::Rejected) => ProposalDisposition::Rejected,
                Ok(Reconciliation::StillIndeterminate) | Err(_) => {
                    ProposalDisposition::ReconciliationRequired
                }
            };
            inspection.disposition = disposition;
            inspection.error = match disposition {
                ProposalDisposition::Accepted => None,
                ProposalDisposition::Rejected => Some(Error::DeliveryRejected),
                _ => Some(Error::ReconciliationRequired),
            };
            if disposition == ProposalDisposition::Rejected {
                inspection.counts.retained_owned_bytes = 0;
            }
            let detail = geometry_detail(&request, inspection, 1)?;
            let outcome = if disposition == ProposalDisposition::Accepted {
                hsk_studio_observe::Outcome::Success
            } else {
                hsk_studio_observe::Outcome::Failure(hsk_studio_observe::FailureCode::Unavailable)
            };
            let receipt = observer
                .emit_geometry(
                    hsk_studio_observe::Observation {
                        correlation_id: request.correlation_id,
                        revision: request.target.expected_revision,
                        outcome,
                        progress: None,
                        private_project_text: None,
                    },
                    &detail,
                    cancel,
                    sink,
                )
                .map_err(|_| Error::ReconciliationRequired)?;
            if disposition == ProposalDisposition::Accepted
                && receipt.outcome == hsk_studio_observe::Outcome::Canceled
            {
                return Err(Error::Cancelled);
            }
            Ok(disposition)
        })
        .and_then(|value| value);
    match resolved {
        Ok(ProposalDisposition::Accepted) => Finalized::Accepted {
            proposal: token.proposal,
            inspection,
        },
        Ok(ProposalDisposition::Rejected) => {
            inspection.counts.output_vertices = 0;
            inspection.counts.output_segments = 0;
            inspection.counts.output_regions = 0;
            inspection.counts.output_loops = 0;
            drop(token);
            Finalized::Rejected {
                error: Error::DeliveryRejected,
                inspection,
            }
        }
        _ => {
            inspection.disposition = ProposalDisposition::ReconciliationRequired;
            inspection.error = Some(Error::ReconciliationRequired);
            inspection.counts.retained_owned_bytes = token.proposal.reserved_bytes();
            token.inspection = inspection;
            inspection.diagnostic_delivery =
                diagnostic_error(emit_pending(&token, diagnostics, cancel, fallback_sink));
            token.inspection = inspection;
            Finalized::ReconciliationRequired { token, inspection }
        }
    }
}
