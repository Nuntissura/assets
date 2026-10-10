//! Observe: bounded correlation/progress/outcome adapter, not a recorder or authority.
//! Caller-owned ports deliver allowlisted bytes to the existing diagnostic owners.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};

pub const MAX_FRAME_BYTES: usize = 2048;
pub const MAX_PROGRESS_EVENTS: u32 = 1024;
pub const WIRE_VERSION: u8 = 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryClass {
    Progress,
    Terminal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryError {
    Saturated,
    Rejected,
    Unavailable,
    Indeterminate,
}
/// Nonblocking, all-or-none acceptance. Terminal has reserved admission at the destination.
/// An adapter unable to know whether bytes escaped MUST return Indeterminate, never Ok.
/// The caller enforces fresh grants and destination visibility before this callback.
pub trait SinkPort: Send + Sync {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidBudget,
    InvalidProgress,
    InvalidDetail,
    RevisionMismatch,
    Saturated,
    Closed,
    SequenceOverflow,
    FrameLimit,
    Delivery(DeliveryError),
    ReconciliationRequired,
}
impl Error {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidBudget => "invalid_budget",
            Self::InvalidProgress => "invalid_progress",
            Self::InvalidDetail => "invalid_detail",
            Self::RevisionMismatch => "revision_mismatch",
            Self::Saturated => "saturated",
            Self::Closed => "closed",
            Self::SequenceOverflow => "sequence_overflow",
            Self::FrameLimit => "frame_limit",
            Self::Delivery(DeliveryError::Saturated) => "sink_saturated",
            Self::Delivery(DeliveryError::Rejected) => "sink_rejected",
            Self::Delivery(DeliveryError::Unavailable) => "sink_unavailable",
            Self::Delivery(DeliveryError::Indeterminate) => "delivery_indeterminate",
            Self::ReconciliationRequired => "reconciliation_required",
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureCode {
    Validation,
    Unavailable,
    Loss,
    Unsupported,
}
impl FailureCode {
    pub const fn wire(self) -> u8 {
        match self {
            Self::Validation => 1,
            Self::Unavailable => 2,
            Self::Loss => 3,
            Self::Unsupported => 4,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Progress,
    Success,
    Failure(FailureCode),
    Canceled,
}
impl Outcome {
    pub const fn wire(self) -> u8 {
        match self {
            Self::Progress => 1,
            Self::Success => 2,
            Self::Failure(_) => 3,
            Self::Canceled => 4,
        }
    }
    pub const fn class(self) -> DeliveryClass {
        if matches!(self, Self::Progress) {
            DeliveryClass::Progress
        } else {
            DeliveryClass::Terminal
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    completed: u64,
    total: u64,
}
impl Progress {
    pub fn new(completed: u64, total: u64) -> Result<Self, Error> {
        if total == 0 || completed > total {
            Err(Error::InvalidProgress)
        } else {
            Ok(Self { completed, total })
        }
    }
    pub fn completed(self) -> u64 {
        self.completed
    }
    pub fn total(self) -> u64 {
        self.total
    }
}
/// Closed shared diagnostic vocabulary; stable wire discriminants, never raw error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DiagnosticCode {
    InvalidInput = 1,
    UnsupportedVersion,
    DuplicateField,
    InvalidActor,
    InvalidCommand,
    WrongDocument,
    InvalidTarget,
    WrongType,
    DuplicateAddress,
    MissingRead,
    RevisionUnavailable,
    RevisionConflict,
    ValueConflict,
    RevisionOverflow,
    BudgetExceeded,
    Canceled,
    ValidationRejected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Disposition {
    Changed = 1,
    NoChange = 2,
    Rejected = 3,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DiagnosticTarget {
    Layer = 1,
    Artboard = 2,
    PageSpread = 3,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DiagnosticProperty {
    Name = 1,
    Visible = 2,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticAddress {
    target: DiagnosticTarget,
    property: DiagnosticProperty,
    id: DomainId,
}
impl DiagnosticAddress {
    pub fn new(
        target: DiagnosticTarget,
        property: DiagnosticProperty,
        id: DomainId,
    ) -> Result<Self, Error> {
        let prefix = match target {
            DiagnosticTarget::Layer => "SLYR",
            DiagnosticTarget::Artboard => "SART",
            DiagnosticTarget::PageSpread => "SPGS",
        };
        if id.prefix() != prefix
            || (property == DiagnosticProperty::Visible && target != DiagnosticTarget::Layer)
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            target,
            property,
            id,
        })
    }
}
pub const MAX_DETAIL_COUNT: u32 = 1_048_576;
/// Counts are capped, not inferred progress. Rejections have zero admitted/changed counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticDetail {
    disposition: Disposition,
    code: Option<DiagnosticCode>,
    address: Option<DiagnosticAddress>,
    counts: [u32; 5],
}
impl DiagnosticDetail {
    pub fn new(
        disposition: Disposition,
        code: Option<DiagnosticCode>,
        address: Option<DiagnosticAddress>,
        counts: [u32; 5],
    ) -> Result<Self, Error> {
        if (disposition == Disposition::Rejected) != code.is_some()
            || counts.iter().any(|v| *v > MAX_DETAIL_COUNT)
            || counts[2] > counts[0]
            || counts[3] > counts[1]
            || counts[4] > counts[3]
            || (disposition == Disposition::Rejected && counts[2..].iter().any(|v| *v != 0))
            || (disposition == Disposition::NoChange && counts[4] != 0)
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            disposition,
            code,
            address,
            counts,
        })
    }
}
/// Closed tile sample address; the key is a granted stable identity, never a path or label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileAddress {
    layer_id: DomainId,
    object_key: String,
    column: i64,
    row: i64,
}
impl TileAddress {
    pub fn new(layer_id: DomainId, object_key: &str, column: i64, row: i64) -> Result<Self, Error> {
        if layer_id.prefix() != "SLYR"
            || object_key.is_empty()
            || object_key.len() > 128
            || !object_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            layer_id,
            object_key: object_key.into(),
            column,
            row,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ResourceDisposition {
    Changed = 1,
    NoChange = 2,
    Rejected = 3,
    ReconciliationRequired = 4,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ResourceCode {
    Validation = 1,
    Unavailable,
    Unsupported,
    RevisionConflict,
    StaleEpoch,
    LeaseUnavailable,
    UnsupportedFinalization,
    Overflow,
    BudgetExceeded,
    Canceled,
    Loss,
    HashMismatch,
    Unauthorized,
}
/// Tile counts: attempted, admitted to bounded work, proposed changed. Bytes: counted
/// output/scratch/retained-preimage reservation peaks; never current ownership or transfer proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceCounts {
    tiles: [u32; 3],
    tile_limit: u32,
    bytes: [u64; 3],
    byte_limit: u64,
}
impl ResourceCounts {
    pub fn new(
        tiles: [u32; 3],
        tile_limit: u32,
        bytes: [u64; 3],
        byte_limit: u64,
    ) -> Result<Self, Error> {
        let total = bytes[0]
            .checked_add(bytes[1])
            .and_then(|n| n.checked_add(bytes[2]))
            .ok_or(Error::InvalidDetail)?;
        if tile_limit > MAX_DETAIL_COUNT
            || tiles[0] > MAX_DETAIL_COUNT
            || tiles[1] > tiles[0]
            || tiles[1] > tile_limit
            || tiles[2] > tiles[1]
            || total > byte_limit
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            tiles,
            tile_limit,
            bytes,
            byte_limit,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceDetail {
    disposition: ResourceDisposition,
    code: Option<ResourceCode>,
    address: Option<TileAddress>,
    epoch: u64,
    counts: ResourceCounts,
}
impl ResourceDetail {
    pub fn new(
        disposition: ResourceDisposition,
        code: Option<ResourceCode>,
        address: Option<TileAddress>,
        epoch: u64,
        counts: ResourceCounts,
    ) -> Result<Self, Error> {
        let unsuccessful = matches!(
            disposition,
            ResourceDisposition::Rejected | ResourceDisposition::ReconciliationRequired
        );
        if unsuccessful != code.is_some()
            || (matches!(
                disposition,
                ResourceDisposition::Rejected | ResourceDisposition::NoChange
            ) && counts.tiles[2] != 0)
            || (disposition == ResourceDisposition::Changed && counts.tiles[2] == 0)
            || (!unsuccessful && counts.tiles[0] > counts.tile_limit)
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            disposition,
            code,
            address,
            epoch,
            counts,
        })
    }
}
/// Borrowed granted geometry identity, never a label or filesystem path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryAddress<'a> {
    layer_id: &'a DomainId,
    object_key: &'a str,
}
impl<'a> GeometryAddress<'a> {
    pub fn new(layer_id: &'a DomainId, object_key: &'a str) -> Result<Self, Error> {
        if layer_id.prefix() != "SLYR"
            || object_key.is_empty()
            || object_key.len() > 128
            || !object_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            layer_id,
            object_key,
        })
    }
    pub const fn property(self) -> &'static str {
        "geometry"
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum GeometryDisposition {
    Prepared = 1,
    Accepted = 2,
    Rejected = 3,
    ReconciliationRequired = 4,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum GeometryCode {
    Validation = 1,
    Unsupported,
    Empty,
    UnavailableRead,
    UnavailableStyle,
    RevisionConflict,
    StaleEpoch,
    Overflow,
    Canceled,
    BudgetExceeded,
    LeaseUnavailable,
    DeliveryRejected,
    DeliveryUnavailable,
    ReconciliationRequired,
    HashMismatch,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryBudget {
    pub count_limit: u64,
    pub work_limit: u64,
    pub byte_limit: u64,
    pub delivery_limit: u64,
}
/// Counts: input anchors/segments, output vertices/segments/regions/loops,
/// intersections/work units. Bytes: requested owned/precharged input/retained owned.
/// Retained bytes are a subset of requested owned, never an additional peak charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryCounts {
    geometry: [u64; 8],
    bytes: [u64; 3],
    delivery_attempts: u64,
    budget: GeometryBudget,
    rejection_only: bool,
}
impl GeometryCounts {
    pub fn new(
        geometry: [u64; 8],
        bytes: [u64; 3],
        delivery_attempts: u64,
        budget: GeometryBudget,
    ) -> Result<Self, Error> {
        let simultaneous = bytes[0].checked_add(bytes[1]).ok_or(Error::InvalidDetail)?;
        if geometry[..7].iter().any(|n| *n > budget.count_limit)
            || geometry[7] > budget.work_limit
            || bytes[2] > bytes[0]
            || simultaneous > budget.byte_limit
            || delivery_attempts > budget.delivery_limit
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            geometry,
            bytes,
            delivery_attempts,
            budget,
            rejection_only: false,
        })
    }
    /// Observed denial counters retain the actual caller limits, including exceeded limits.
    /// This is diagnostic data only, never proof of admission or a usable output.
    pub fn for_rejection(
        geometry: [u64; 8],
        bytes: [u64; 3],
        delivery_attempts: u64,
        budget: GeometryBudget,
    ) -> Result<Self, Error> {
        if geometry[2..6].iter().any(|n| *n != 0)
            || bytes[2] != 0
            || delivery_attempts > budget.delivery_limit
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            geometry,
            bytes,
            delivery_attempts,
            budget,
            rejection_only: true,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryRead<'a> {
    pub source_path_id: Option<&'a DomainId>,
    pub read_revision: Option<u64>,
    pub target_revision: Option<u64>,
    pub cancel_epoch: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryDetail<'a> {
    disposition: GeometryDisposition,
    code: Option<GeometryCode>,
    address: Option<GeometryAddress<'a>>,
    read: GeometryRead<'a>,
    counts: GeometryCounts,
}
impl<'a> GeometryDetail<'a> {
    pub fn new(
        disposition: GeometryDisposition,
        code: Option<GeometryCode>,
        address: Option<GeometryAddress<'a>>,
        read: GeometryRead<'a>,
        counts: GeometryCounts,
    ) -> Result<Self, Error> {
        let rejected = disposition == GeometryDisposition::Rejected;
        let pending = disposition == GeometryDisposition::ReconciliationRequired;
        if (counts.rejection_only && !rejected)
            || (rejected || pending) != code.is_some()
            || (pending != (code == Some(GeometryCode::ReconciliationRequired)))
            || read.source_path_id.is_some_and(|id| id.prefix() != "SVPT")
            || (rejected && (counts.geometry[2..6].iter().any(|n| *n != 0) || counts.bytes[2] != 0))
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            disposition,
            code,
            address,
            read,
            counts,
        })
    }
}

/// Closed, borrowed shaping diagnostics; source content is never transported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ShapingDisposition {
    Prepared = 1,
    Accepted,
    Rejected,
    ReconciliationRequired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ShapingCode {
    InvalidInput = 1,
    Overflow,
    BudgetExceeded,
    Allocation,
    Canceled,
    StaleEpoch,
    CorruptFont,
    MissingFont,
    MissingGlyph,
    UnsupportedProfile,
    UnsupportedFeature,
    HashMismatch,
    RevokedFont,
    UnavailableFace,
    UnavailableFont,
    UnsupportedFont,
    InvalidAxis,
    AbsentRead,
    UnavailableRead,
    StaleRead,
    UnsupportedFinalization,
    DeliveryRejected,
    DeliveryUnavailable,
    DeliverySaturated,
    DeliveryIndeterminate,
    LeaseUnavailable,
    UnknownComposition,
    UnsupportedNormalization,
    InternalInvariant,
    InvalidBidi,
    InvalidMapping,
    PartialOutput,
    ReconciliationRequired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TextDisposition {
    Shaped = 1,
    EmptyInput,
    ControlOnly,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TextProperty {
    Text = 1,
    StoryMembership,
    Style,
    ShapedRuns,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextAddress<'a> {
    id: &'a DomainId,
    property: TextProperty,
}
impl<'a> TextAddress<'a> {
    pub fn new(id: &'a DomainId, property: TextProperty) -> Result<Self, Error> {
        let valid = match property {
            TextProperty::Text => id.prefix() == "STXT",
            TextProperty::Style => id.prefix() == "STYS",
            TextProperty::StoryMembership | TextProperty::ShapedRuns => {
                matches!(id.prefix(), "STXT" | "SLYR")
            }
        };
        if !valid {
            return Err(Error::InvalidDetail);
        }
        Ok(Self { id, property })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapingBudget {
    pub counts: [u64; 8],
    pub byte_limit: u64,
    pub delivery_limit: u64,
}
/// Counts: input UTF8 bytes/scalars, fonts/runs/glyphs/fallback/work, retained generations.
/// All are this-operation observations, never historical high-water or authority.
/// Bytes are current owned/this-operation peak/retained owned; retained overlaps current.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapingCounts {
    counts: [u64; 8],
    bytes: [u64; 3],
    delivery_attempts: u64,
    budget: ShapingBudget,
    rejection_only: bool,
}
impl ShapingCounts {
    pub fn new(
        counts: [u64; 8],
        bytes: [u64; 3],
        delivery_attempts: u64,
        budget: ShapingBudget,
    ) -> Result<Self, Error> {
        if counts.iter().zip(budget.counts).any(|(n, cap)| *n > cap)
            || bytes[0] > bytes[1]
            || bytes[2] > bytes[0]
            || bytes[1] > budget.byte_limit
            || delivery_attempts > budget.delivery_limit
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            counts,
            bytes,
            delivery_attempts,
            budget,
            rejection_only: false,
        })
    }
    /// Preserve actual observations and caller limits on denial; no live result is represented.
    pub fn for_rejection(
        counts: [u64; 8],
        bytes: [u64; 3],
        delivery_attempts: u64,
        budget: ShapingBudget,
    ) -> Result<Self, Error> {
        if bytes[0] > bytes[1]
            || bytes[2] != 0
            || counts[7] != 0
            || delivery_attempts > budget.delivery_limit
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            counts,
            bytes,
            delivery_attempts,
            budget,
            rejection_only: true,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapingRead {
    pub source_revision: Option<u64>,
    pub resolver_revision: Option<u64>,
    pub cancel_epoch: Option<u64>,
    pub target_revision: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapingDetail<'a> {
    disposition: ShapingDisposition,
    code: Option<ShapingCode>,
    result: Option<TextDisposition>,
    address: Option<TextAddress<'a>>,
    read: ShapingRead,
    counts: ShapingCounts,
}
impl<'a> ShapingDetail<'a> {
    pub fn new(
        disposition: ShapingDisposition,
        code: Option<ShapingCode>,
        result: Option<TextDisposition>,
        address: Option<TextAddress<'a>>,
        read: ShapingRead,
        counts: ShapingCounts,
    ) -> Result<Self, Error> {
        let rejected = disposition == ShapingDisposition::Rejected;
        let pending = disposition == ShapingDisposition::ReconciliationRequired;
        if (rejected == result.is_some())
            || (counts.rejection_only && !rejected)
            || ((rejected || pending) != code.is_some())
            || (pending != (code == Some(ShapingCode::ReconciliationRequired)))
            || (rejected && (counts.bytes[2] != 0 || counts.counts[7] != 0))
        {
            return Err(Error::InvalidDetail);
        }
        Ok(Self {
            disposition,
            code,
            result,
            address,
            read,
            counts,
        })
    }
}

/// Fixed bounded transport storage; no owned frame allocation on any version.
struct Frame {
    bytes: [u8; MAX_FRAME_BYTES],
    len: usize,
}
impl Frame {
    fn new() -> Self {
        Self {
            bytes: [0; MAX_FRAME_BYTES],
            len: 0,
        }
    }
    fn len(&self) -> usize {
        self.len
    }
    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
    fn extend_from_slice(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let end = self.len.checked_add(bytes.len()).ok_or(Error::FrameLimit)?;
        let target = self.bytes.get_mut(self.len..end).ok_or(Error::FrameLimit)?;
        target.copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
    fn push(&mut self, byte: u8) -> Result<(), Error> {
        self.extend_from_slice(&[byte])
    }
}

/// Finite lifetime admission; one terminal is reserved separately from progress budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    progress_events: u32,
    progress_bytes: usize,
}
impl Budget {
    pub fn new(progress_events: u32, progress_bytes: usize) -> Result<Self, Error> {
        if progress_events > MAX_PROGRESS_EVENTS
            || progress_bytes > MAX_FRAME_BYTES * MAX_PROGRESS_EVENTS as usize
        {
            Err(Error::InvalidBudget)
        } else {
            Ok(Self {
                progress_events,
                progress_bytes,
            })
        }
    }
}
/// Closed command payload. Private content is a borrowed input and NEVER read or emitted.
/// No labels, messages, stack traces, arbitrary attributes or source paths are telemetry fields.
#[derive(Clone, Copy)]
pub struct Observation<'a> {
    pub correlation_id: u64,
    pub revision: u64,
    pub outcome: Outcome,
    pub progress: Option<Progress>,
    pub private_project_text: Option<&'a str>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub correlation_id: u64,
    pub revision: u64,
    pub sequence: u64,
    pub outcome: Outcome,
    pub delivered_bytes: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub acknowledged: u64,
    pub progress_events: u32,
    pub progress_bytes: usize,
    pub terminal_closed: bool,
    pub reconciliation_required: bool,
    pub saturation_count: u64,
    pub rejection_count: u64,
    pub unavailable_count: u64,
    pub indeterminate_count: u64,
    pub dropped_attempts: u64,
    pub canceled_delivered: u64,
}
/// One emitter per correlation/resource/revision. Mutable ownership serializes callbacks;
/// it is not a job scheduler, database, session registry, queue or Flight Recorder replacement.
pub struct Observe {
    correlation_id: u64,
    revision: u64,
    resource: DomainId,
    actor: ActorContext,
    budget: Budget,
    state: State,
}
impl Observe {
    pub fn new(
        correlation_id: u64,
        revision: u64,
        resource: DomainId,
        actor: ActorContext,
        budget: Budget,
    ) -> Self {
        Self {
            correlation_id,
            revision,
            resource,
            actor,
            budget,
            state: State {
                acknowledged: 0,
                progress_events: 0,
                progress_bytes: 0,
                terminal_closed: false,
                reconciliation_required: false,
                saturation_count: 0,
                rejection_count: 0,
                unavailable_count: 0,
                indeterminate_count: 0,
                dropped_attempts: 0,
                canceled_delivered: 0,
            },
        }
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn emit(
        &mut self,
        event: Observation<'_>,
        cancel: &CancellationToken,
        sink: &mut (impl SinkPort + ?Sized),
    ) -> Result<Receipt, Error> {
        self.emit_inner(event, None, None, None, None, cancel, sink)
    }
    /// Version 2 terminal detail uses the same admission, cancellation and delivery state machine.
    pub fn emit_detail(
        &mut self,
        event: Observation<'_>,
        detail: &DiagnosticDetail,
        cancel: &CancellationToken,
        sink: &mut (impl SinkPort + ?Sized),
    ) -> Result<Receipt, Error> {
        if event.outcome == Outcome::Progress
            || (detail.disposition == Disposition::Rejected && event.outcome == Outcome::Success)
            || (detail.disposition != Disposition::Rejected
                && matches!(event.outcome, Outcome::Failure(_)))
        {
            return Err(Error::InvalidDetail);
        }
        self.emit_inner(event, Some(detail), None, None, None, cancel, sink)
    }
    /// Version 3 shares the existing delivery state machine; reservations and finalization
    /// remain caller-owned. A pending source result is distinct from diagnostic delivery state.
    pub fn emit_resource(
        &mut self,
        event: Observation<'_>,
        detail: &ResourceDetail,
        cancel: &CancellationToken,
        sink: &mut (impl SinkPort + ?Sized),
    ) -> Result<Receipt, Error> {
        let unsuccessful = matches!(
            detail.disposition,
            ResourceDisposition::Rejected | ResourceDisposition::ReconciliationRequired
        );
        if event.outcome == Outcome::Progress
            || (unsuccessful && event.outcome == Outcome::Success)
            || (!unsuccessful && matches!(event.outcome, Outcome::Failure(_)))
        {
            return Err(Error::InvalidDetail);
        }
        self.emit_inner(event, None, Some(detail), None, None, cancel, sink)
    }
    /// Version 4 borrowed geometry detail shares all legacy delivery/admission states.
    pub fn emit_geometry(
        &mut self,
        event: Observation<'_>,
        detail: &GeometryDetail<'_>,
        cancel: &CancellationToken,
        sink: &mut (impl SinkPort + ?Sized),
    ) -> Result<Receipt, Error> {
        let unsuccessful = matches!(
            detail.disposition,
            GeometryDisposition::Rejected | GeometryDisposition::ReconciliationRequired
        );
        if event.outcome == Outcome::Progress
            || (unsuccessful && event.outcome == Outcome::Success)
            || (!unsuccessful && matches!(event.outcome, Outcome::Failure(_)))
        {
            return Err(Error::InvalidDetail);
        }
        self.emit_inner(event, None, None, Some(detail), None, cancel, sink)
    }
    /// Version 5 uses the existing terminal sink and reconciliation state.
    pub fn emit_shaping(
        &mut self,
        event: Observation<'_>,
        detail: &ShapingDetail<'_>,
        cancel: &CancellationToken,
        sink: &mut (impl SinkPort + ?Sized),
    ) -> Result<Receipt, Error> {
        let unsuccessful = matches!(
            detail.disposition,
            ShapingDisposition::Rejected | ShapingDisposition::ReconciliationRequired
        );
        if event.outcome == Outcome::Progress
            || (unsuccessful && event.outcome == Outcome::Success)
            || (!unsuccessful && matches!(event.outcome, Outcome::Failure(_)))
        {
            return Err(Error::InvalidDetail);
        }
        self.emit_inner(event, None, None, None, Some(detail), cancel, sink)
    }
    fn emit_inner(
        &mut self,
        event: Observation<'_>,
        detail: Option<&DiagnosticDetail>,
        resource: Option<&ResourceDetail>,
        geometry: Option<&GeometryDetail<'_>>,
        shaping: Option<&ShapingDetail<'_>>,
        cancel: &CancellationToken,
        sink: &mut (impl SinkPort + ?Sized),
    ) -> Result<Receipt, Error> {
        if self.state.reconciliation_required {
            return Err(Error::ReconciliationRequired);
        }
        if self.state.terminal_closed {
            return Err(Error::Closed);
        }
        if event.revision != self.revision || event.correlation_id != self.correlation_id {
            return Err(Error::RevisionMismatch);
        }
        // Cancellation uses the reserved terminal path, even when progress admission is full.
        let outcome = if cancel.check().is_err() {
            Outcome::Canceled
        } else {
            event.outcome
        };
        let progress = if outcome == Outcome::Progress {
            Some(event.progress.ok_or(Error::InvalidProgress)?)
        } else {
            None
        };
        let sequence = self
            .state
            .acknowledged
            .checked_add(1)
            .ok_or(Error::SequenceOverflow)?;
        let class = outcome.class();
        if class == DeliveryClass::Progress
            && self.state.progress_events >= self.budget.progress_events
        {
            self.state.saturation_count = self.state.saturation_count.saturating_add(1);
            self.state.dropped_attempts = self.state.dropped_attempts.saturating_add(1);
            return Err(Error::Saturated);
        }
        let frame = self.encode(
            sequence, outcome, progress, detail, resource, geometry, shaping,
        )?;
        let next_bytes = self
            .state
            .progress_bytes
            .checked_add(frame.len())
            .ok_or(Error::FrameLimit)?;
        if class == DeliveryClass::Progress && next_bytes > self.budget.progress_bytes {
            self.state.saturation_count = self.state.saturation_count.saturating_add(1);
            self.state.dropped_attempts = self.state.dropped_attempts.saturating_add(1);
            return Err(Error::Saturated);
        }
        match sink.try_send(class, frame.as_slice()) {
            Ok(()) => {}
            Err(error) => {
                self.state.dropped_attempts = self.state.dropped_attempts.saturating_add(1);
                match error {
                    DeliveryError::Saturated => {
                        self.state.saturation_count = self.state.saturation_count.saturating_add(1)
                    }
                    DeliveryError::Rejected => {
                        self.state.rejection_count = self.state.rejection_count.saturating_add(1)
                    }
                    DeliveryError::Unavailable => {
                        self.state.unavailable_count =
                            self.state.unavailable_count.saturating_add(1)
                    }
                    DeliveryError::Indeterminate => {
                        self.state.indeterminate_count =
                            self.state.indeterminate_count.saturating_add(1)
                    }
                }
                if error == DeliveryError::Indeterminate {
                    self.state.reconciliation_required = true
                }
                return Err(Error::Delivery(error));
            }
        }
        self.state.acknowledged = sequence;
        if outcome == Outcome::Canceled {
            self.state.canceled_delivered = self.state.canceled_delivered.saturating_add(1)
        }
        if class == DeliveryClass::Progress {
            self.state.progress_events += 1;
            self.state.progress_bytes = next_bytes
        } else {
            self.state.terminal_closed = true
        }
        Ok(Receipt {
            correlation_id: self.correlation_id,
            revision: self.revision,
            sequence,
            outcome,
            delivered_bytes: frame.len(),
        })
    }
    fn encode(
        &self,
        sequence: u64,
        outcome: Outcome,
        progress: Option<Progress>,
        detail: Option<&DiagnosticDetail>,
        resource: Option<&ResourceDetail>,
        geometry: Option<&GeometryDetail<'_>>,
        shaping: Option<&ShapingDetail<'_>>,
    ) -> Result<Frame, Error> {
        let strings = [
            self.resource.as_str(),
            self.actor.account_id(),
            self.actor.principal_id(),
            self.actor.owner_account_id(),
            self.actor.owner_principal_id(),
            self.actor.access_space_id(),
            self.actor.session_id(),
        ];
        let extra = detail.map_or(0, |d| {
            23 + d.address.as_ref().map_or(0, |a| 4 + a.id.as_str().len())
        });
        let resource_extra = resource.map_or(0, |d| {
            59 + d
                .address
                .as_ref()
                .map_or(0, |a| 21 + a.layer_id.as_str().len() + a.object_key.len())
        });
        let length =
            48 + strings.iter().map(|s| 2 + s.len()).sum::<usize>() + extra + resource_extra;
        if length > MAX_FRAME_BYTES {
            return Err(Error::FrameLimit);
        }
        let mut out = Frame::new();
        out.extend_from_slice(b"HSKO")?;
        out.extend_from_slice(&[
            if shaping.is_some() {
                5
            } else if geometry.is_some() {
                4
            } else if resource.is_some() {
                3
            } else if detail.is_some() {
                2
            } else {
                WIRE_VERSION
            },
            outcome.wire(),
            if let Outcome::Failure(code) = outcome {
                code.wire()
            } else {
                0
            },
            u8::from(progress.is_some()),
        ])?;
        for value in [
            self.correlation_id,
            self.revision,
            sequence,
            progress.map_or(0, Progress::completed),
            progress.map_or(0, Progress::total),
        ] {
            out.extend_from_slice(&value.to_be_bytes())?
        }
        for string in strings {
            let count = u16::try_from(string.len()).map_err(|_| Error::FrameLimit)?;
            out.extend_from_slice(&count.to_be_bytes())?;
            out.extend_from_slice(string.as_bytes())?
        }
        if let Some(d) = detail {
            out.extend_from_slice(&[
                d.disposition as u8,
                d.code.map_or(0, |c| c as u8),
                u8::from(d.address.is_some()),
            ])?;
            for count in d.counts {
                out.extend_from_slice(&count.to_be_bytes())?;
            }
            if let Some(a) = &d.address {
                out.extend_from_slice(&[a.target as u8, a.property as u8])?;
                let len = u16::try_from(a.id.as_str().len()).map_err(|_| Error::FrameLimit)?;
                out.extend_from_slice(&len.to_be_bytes())?;
                out.extend_from_slice(a.id.as_str().as_bytes())?;
            }
        }
        if let Some(d) = resource {
            out.extend_from_slice(&[
                d.disposition as u8,
                d.code.map_or(0, |c| c as u8),
                u8::from(d.address.is_some()),
            ])?;
            out.extend_from_slice(&d.epoch.to_be_bytes())?;
            for count in d.counts.tiles {
                out.extend_from_slice(&count.to_be_bytes())?;
            }
            out.extend_from_slice(&d.counts.tile_limit.to_be_bytes())?;
            for bytes in d.counts.bytes {
                out.extend_from_slice(&bytes.to_be_bytes())?;
            }
            out.extend_from_slice(&d.counts.byte_limit.to_be_bytes())?;
            if let Some(a) = &d.address {
                out.push(1)?; // closed property=samples
                for string in [a.layer_id.as_str(), a.object_key.as_str()] {
                    let count = u16::try_from(string.len()).map_err(|_| Error::FrameLimit)?;
                    out.extend_from_slice(&count.to_be_bytes())?;
                    out.extend_from_slice(string.as_bytes())?;
                }
                out.extend_from_slice(&a.column.to_be_bytes())?;
                out.extend_from_slice(&a.row.to_be_bytes())?;
            }
        }
        if let Some(d) = geometry {
            let flags = u8::from(d.address.is_some())
                | (u8::from(d.read.source_path_id.is_some()) << 1)
                | (u8::from(d.read.read_revision.is_some()) << 2)
                | (u8::from(d.read.target_revision.is_some()) << 3)
                | (u8::from(d.read.cancel_epoch.is_some()) << 4);
            out.extend_from_slice(&[d.disposition as u8, d.code.map_or(0, |c| c as u8), flags])?;
            for count in d.counts.geometry {
                out.extend_from_slice(&count.to_be_bytes())?;
            }
            for count in d.counts.bytes {
                out.extend_from_slice(&count.to_be_bytes())?;
            }
            for count in [
                d.counts.delivery_attempts,
                d.counts.budget.count_limit,
                d.counts.budget.work_limit,
                d.counts.budget.byte_limit,
                d.counts.budget.delivery_limit,
            ] {
                out.extend_from_slice(&count.to_be_bytes())?;
            }
            for revision in [
                d.read.read_revision,
                d.read.target_revision,
                d.read.cancel_epoch,
            ]
            .into_iter()
            .flatten()
            {
                out.extend_from_slice(&revision.to_be_bytes())?;
            }
            if let Some(address) = d.address {
                out.push(1)?; // closed property=geometry
                for string in [address.layer_id.as_str(), address.object_key] {
                    let count = u16::try_from(string.len()).map_err(|_| Error::FrameLimit)?;
                    out.extend_from_slice(&count.to_be_bytes())?;
                    out.extend_from_slice(string.as_bytes())?;
                }
            }
            if let Some(source) = d.read.source_path_id {
                let count = u16::try_from(source.as_str().len()).map_err(|_| Error::FrameLimit)?;
                out.extend_from_slice(&count.to_be_bytes())?;
                out.extend_from_slice(source.as_str().as_bytes())?;
            }
        }
        if let Some(d) = shaping {
            let flags = u8::from(d.address.is_some())
                | (u8::from(d.read.source_revision.is_some()) << 1)
                | (u8::from(d.read.resolver_revision.is_some()) << 2)
                | (u8::from(d.read.cancel_epoch.is_some()) << 3)
                | (u8::from(d.read.target_revision.is_some()) << 4);
            out.extend_from_slice(&[
                d.disposition as u8,
                d.code.map_or(0, |c| c as u8),
                d.result.map_or(0, |r| r as u8),
                flags,
            ])?;
            for n in d
                .counts
                .counts
                .into_iter()
                .chain(d.counts.bytes)
                .chain([d.counts.delivery_attempts])
                .chain(d.counts.budget.counts)
                .chain([d.counts.budget.byte_limit, d.counts.budget.delivery_limit])
            {
                out.extend_from_slice(&n.to_be_bytes())?;
            }
            for revision in [
                d.read.source_revision,
                d.read.resolver_revision,
                d.read.cancel_epoch,
                d.read.target_revision,
            ]
            .into_iter()
            .flatten()
            {
                out.extend_from_slice(&revision.to_be_bytes())?;
            }
            if let Some(a) = d.address {
                out.push(a.property as u8)?;
                let id = a.id.as_str();
                let len = u16::try_from(id.len()).map_err(|_| Error::FrameLimit)?;
                out.extend_from_slice(&len.to_be_bytes())?;
                out.extend_from_slice(id.as_bytes())?;
            }
        }
        Ok(out)
    }
}

/// Same descriptor for models, human manual and Argus/diagnostic adapters. No new Studio schema ID.
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-OBSERVE","version":1,"operation":"Observe::emit/emit_detail/emit_resource/emit_geometry/emit_shaping closed allowlisted progress/outcome delivery","operator":{"intent":"Track work without exposing project text","result":"Correlation/revision/sequence receipt only after required sink accepts the emitted bytes","recovery":"On full/rejected/unavailable sink correct destination or admission and retry same revision; indeterminate delivery requires caller reconciliation before replacing emitter; stale context must refresh; canceled work emits a reserved terminal outcome"},"model":{"inputs":"Accord DomainId/ActorContext plus local u64 correlation/revision, bounded Budget, Observation progress or fixed success/failure/canceled enum, cancellation token, caller SinkPort","consumer":"observe-consumer [--descriptor] [--mode success|error|cancel|reject|saturate] [--private-text TEXT]","geometry_wire":"emit_geometry terminal HSKO version4: identical v1 header/seven attribution strings then u8 disposition1Prepared/2Accepted/3Rejected/4ReconciliationRequired,u8 code0none/1Validation/2Unsupported/3Empty/4UnavailableRead/5UnavailableStyle/6RevisionConflict/7StaleEpoch/8Overflow/9Canceled/10BudgetExceeded/11LeaseUnavailable/12DeliveryRejected/13DeliveryUnavailable/14ReconciliationRequired/15HashMismatch,u8 optional flags bit0address/bit1source_path_id/bit2read_revision/bit3target_revision/bit4cancel_epoch (bits5..7 reject). Eight BEu64 counts input_anchors,input_segments,output_vertices,output_segments,output_regions,output_loops,intersections,work_units; three BEu64 bytes requested_owned,precharged_input,retained_owned; five BEu64 delivery_attempts,count_limit,work_limit,byte_limit,delivery_limit. Admission counts0..6<=count_limit,work<=work_limit,retained<=requested,checked requested+precharged<=byte_limit,delivery_attempts<=delivery_limit. GeometryCounts::for_rejection preserves actual limits and observed input/intersection/work/requested/precharged counters even above admission bounds; zero outputs/retained are required and the private rejection-only marker forbids Prepared/Accepted/Pending. Rejected frames may report exceeded admission bounds, never inflated limits or clamped observations; delivery_attempts remains bounded. Retained bytes overlap requested owned and never add a second charge; counts describe actual caller reservations, not ownership proof. Optional BEu64 read_revision,target_revision,cancel_epoch in that order, then optional address u8 property_geometry=1,u16-length UTF8 SLYR,u16-length UTF8 nonempty<=128 ASCII alnum/_/- key,then optional u16-length UTF8 SVPT. Reject unknown tags/trailing bytes. Prepared/Accepted have no code; Rejected requires code and zero output counts/retained bytes; pending requires ReconciliationRequired code and may retain output. No raw coordinates/style/path data or private text. Versions1-3 byte-compatible. Same delivery state machine; cancellation changes event outcome without rewriting geometry disposition.","resource_wire":"emit_resource terminal HSKO version3: identical v1 header and seven attribution strings followed by u8 resource disposition1changed/2no_change/3rejected/4reconciliation_required, u8 code0none/1Validation/2Unavailable/3Unsupported/4RevisionConflict/5StaleEpoch/6LeaseUnavailable/7UnsupportedFinalization/8Overflow/9BudgetExceeded/10Canceled/11Loss/12HashMismatch/13Unauthorized, u8 tile_address_present0|1, BEu64 caller epoch, three BEu32 attempted/admitted-to-bounded-work/proposed-changed tile counts and BEu32 caller tile limit; attempted count and tile limit <=1048576, admitted<=attempted and limit, changed<=admitted. Three BEu64 counted output/scratch/retained-preimage reservation peak byte counts and BEu64 caller byte limit; checked byte sum<=limit. Reservation counts may already be released and never prove current ownership or transfer. Rejected and no_change changed counts zero; changed requires nonzero changed count; no_change transfers no output/preimage, enforced by caller finalization. ReconciliationRequired is an unresolved source/finalization result, separate from sink receipt/reconciliation state. Optional address: u8 samples=1, u16-length UTF8 SLYR ID, u16-length UTF8 object key (nonempty<=128 ASCII alnum/_/-), BEi64 column and row. Key is caller-granted stable identity, not a private label/path. Reject unknown tags/trailing bytes. v1/v2 bytes unchanged; cancellation changes terminal outcome without erasing resource disposition; no pixel/profile/mask bytes, raw errors, arbitrary attributes or lease handles","detail_wire":"emit_detail terminal HSKO version2: identical v1 header and seven attribution strings followed by u8 disposition1changed/2no_change/3rejected, u8 diagnostic code0none/1InvalidInput/2UnsupportedVersion/3DuplicateField/4InvalidActor/5InvalidCommand/6WrongDocument/7InvalidTarget/8WrongType/9DuplicateAddress/10MissingRead/11RevisionUnavailable/12RevisionConflict/13ValueConflict/14RevisionOverflow/15BudgetExceeded/16Canceled/17ValidationRejected, u8 address_present0|1, five BE u32 capped counts attempted_reads,attempted_writes,admitted_reads,admitted_writes,changed_writes (cap1048576); optional u8 target1Layer/2Artboard/3PageSpread,u8 property1Name/2Visible,u16-byte-length UTF8 validated prefixed domain ID. Visible is Layer only; reject unknown tags/trailing bytes. Rejected admitted/changed counts zero; postacceptance cancellation changes terminal outcome but preserves source disposition. Legacy emit stays byte-identical version1","wire":"Local binary transport v1, not canonical document or business event: HSKO ASCII, u8 version=1,outcome=1(progress)/2(success)/3(failure)/4(canceled),u8 fixed failure code0..4,u8 progress-present0|1; five big-endian u64 fields correlation,revision,sequence,completed,total; seven u16-byte-length UTF8 strings resource_id,account_id,principal_id,owner_account_id,owner_principal_id,access_space_id,session_id; reject trailing bytes at collector","limits":{"frame_bytes":2048,"progress_events":1024,"progress_lifetime_bytes":2097152,"terminal_reserve":1,"pending_callers_per_mutable_emitter":1},"redaction":"Private project text, raw errors, arbitrary attributes, labels and source paths never serialized; input content is borrowed and not inspected. Only caller-authorized Accord attribution IDs are included. Destination visibility/grants are caller-owned and host conformance remains pending","port":"Nonblocking atomic all-or-none try_send(Progress|Terminal,bytes); terminal reserves independent destination capacity. Indeterminate partial delivery never reports success; emitter blocks further delivery until caller reconciles","cancel":"Canceled token overrides requested outcome to Canceled; progress saturation cannot consume terminal reserve; delivered terminal closes emitter","realtime":"Emitted frames use a checked2048-byte stack buffer; caller sink/setup costs remain caller-owned; never invoke from realtime audio callback; retain existing multiple-writer/SPSC synchronization in host adapter","undo":"No document mutation or undo; this is not an EventLedger acknowledgment","argus":{"inspect":"Observe::state bounded saturating drop/saturation/rejection/unavailable/indeterminate/canceled counters and typed receipt/error","action":"same emit port and cancellation token","capture":"caller captures actual emitted bytes and independently decoded fields under fresh grant"},"diagnostics":"Caller sinks adapt to existing Flight Recorder/internal diagnostics/Palmistry; no private recorder/catalog/DB/session authority or host proof","shaping_wire":"emit_shaping terminal HSKO version5: unchanged v1 header and seven attribution strings; u8 disposition1Prepared/2Accepted/3Rejected/4ReconciliationRequired, u8 closed ShapingCode0none/1InvalidInput/2Overflow/3BudgetExceeded/4Allocation/5Canceled/6StaleEpoch/7CorruptFont/8MissingFont/9MissingGlyph/10UnsupportedProfile/11UnsupportedFeature/12HashMismatch/13RevokedFont/14UnavailableFace/15UnavailableFont/16UnsupportedFont/17InvalidAxis/18AbsentRead/19UnavailableRead/20StaleRead/21UnsupportedFinalization/22DeliveryRejected/23DeliveryUnavailable/24DeliverySaturated/25DeliveryIndeterminate/26LeaseUnavailable/27UnknownComposition/28UnsupportedNormalization/29InternalInvariant/30InvalidBidi/31InvalidMapping/32PartialOutput/33ReconciliationRequired; u8 result0none/1Shaped/2EmptyInput/3ControlOnly; u8 flags bit0address,bit1source_revision,bit2resolver_revision,bit3cancel_epoch,bit4target_revision (bits5..7 reject). Eight BEu64 observed counts input_UTF8_bytes,scalars,fonts,runs,glyphs,fallback_attempts,work_units,retained_generations; three BEu64 bytes current_requested_owned,this_operation_peak_requested_owned,retained_requested_owned; BEu64 delivery_attempts; eight BEu64 corresponding caller count limits then BEu64 byte_limit,delivery_limit. Current<=operation_peak,retained_bytes<=current; peak<=byte_limit and counts<=corresponding limits except rejected observations may exceed actual caller admission limits without clamping or inflating limits; rejection retained bytes/generations zero. Retained bytes overlap current and never double-charge; generations count is separate. These are operation-local requested allocation observations, not historical peak or ownership proof. Delivery attempts always<=delivery_limit. Optional BEu64 source,resolver,epoch,target revisions in flag order; optional u8 property1Text/2StoryMembership/3Style/4ShapedRuns then u16-length UTF8 granted DomainId: Text STXT, Style STYS, membership/result STXT or SLYR. No arbitrary property keys. Result is required on prepared/accepted/pending and absent on rejection; code required only rejected/pending; pending code33 only. Reject unknown tags, invalid IDs, bounds, disposition or trailing bytes. Cancellation changes terminal outcome without changing shaping/result disposition. No source text/font names/bytes/glyph coordinates/private strings. Versions1-4 byte-identical; same2048-byte stack frame, SinkPort admission, receipt and reconciliation state."}}"#;
