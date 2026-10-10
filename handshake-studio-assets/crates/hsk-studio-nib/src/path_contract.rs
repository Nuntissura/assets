//! Borrowed Studio transport. Caller attribution/read authority and source leases remain external.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Length, Unit};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidGeometry,
    InvalidEncoding,
    DuplicateField,
    UnknownField,
    MissingField,
    ValueMismatch,
    HashMismatch,
    InvalidRequest,
    UnsupportedOperation,
    UnsupportedOptions,
    UnsupportedApproximation,
    EmptyPathfinderResult,
    UnavailableRead,
    AbsentRead,
    UnavailableStyle,
    UnsupportedStyle,
    StaleRevision,
    StaleEpoch,
    Overflow,
    Cancelled,
    BudgetExceeded,
    LeaseUnavailable,
    ProviderRefused,
    DeliveryRejected,
    DeliveryUnavailable,
    ReconciliationRequired,
    UnsupportedFinalization,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: Length,
    pub y: Length,
}
impl Point {
    pub fn pt(x: f64, y: f64) -> Result<Self, Error> {
        Ok(Self {
            x: Length::new(x, Unit::Points).map_err(|_| Error::InvalidGeometry)?,
            y: Length::new(y, Unit::Points).map_err(|_| Error::InvalidGeometry)?,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mirroring {
    None,
    Angle,
    AngleAndLength,
}
impl Mirroring {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Angle => "ANGLE",
            Self::AngleAndLength => "ANGLE_AND_LENGTH",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub position: Point,
    pub incoming: Point,
    pub outgoing: Point,
    pub handle_mirroring: Mirroring,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Winding {
    NonZero,
    EvenOdd,
    None,
}
impl Winding {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::NonZero => "NONZERO",
            Self::EvenOdd => "EVENODD",
            Self::None => "NONE",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Unite,
    BackMinusFront,
    FrontMinusBack,
}
impl Operation {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Unite => "uniteCommand",
            Self::BackMinusFront => "backMinusFrontCommand",
            Self::FrontMinusBack => "frontMinusBackCommand",
        }
    }
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "uniteCommand" => Ok(Self::Unite),
            "backMinusFrontCommand" => Ok(Self::BackMinusFront),
            "frontMinusBackCommand" => Ok(Self::FrontMinusBack),
            _ => Err(Error::UnsupportedOperation),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryAddress<'a> {
    pub layer_id: &'a DomainId,
    pub object_key: &'a str,
}
impl GeometryAddress<'_> {
    pub fn validate(self) -> Result<(), Error> {
        if self.layer_id.prefix() != "SLYR" || !valid_key(self.object_key) {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
    pub const fn property(self) -> &'static str {
        "geometry"
    }
}
pub(crate) fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
#[derive(Clone, Copy, Debug)]
pub struct Path<'a> {
    pub schema_id: &'a str,
    pub path_id: &'a DomainId,
    pub address: GeometryAddress<'a>,
    pub closed: bool,
    pub anchors: &'a [Anchor],
    pub winding_rule: Winding,
    pub fill_style_id: Option<&'a DomainId>,
}
#[derive(Clone, Copy, Debug)]
pub struct Operand<'a> {
    pub path: Path<'a>,
    pub encoded_path_bytes: &'a [u8],
    pub expected_revision: u64,
    pub geometry_sha256: [u8; 32],
}
#[derive(Clone, Copy, Debug)]
pub struct StyleRead<'a> {
    pub style_id: &'a DomainId,
    pub expected_revision: u64,
    pub expected_fingerprint_sha256: [u8; 32],
}
#[derive(Clone, Copy, Debug)]
pub struct Target<'a> {
    pub address: GeometryAddress<'a>,
    pub expected_revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub precision: Length,
    pub remove_redundant_points: bool,
    pub divide_and_outline_remove_unpainted: bool,
    pub loss_policy: LossPolicy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LossPolicy {
    Reject,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultMode {
    ProposalOnly,
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub input_bytes: u64,
    pub operands: u64,
    pub anchors: u64,
    pub segments: u64,
    pub sweep_events: u64,
    pub intersections: u64,
    pub output_vertices: u64,
    pub output_segments: u64,
    pub output_regions: u64,
    pub output_loops: u64,
    pub work_units: u64,
    pub recursion_depth: u64,
    pub requested_allocation_bytes: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub transport_version: u64,
    pub document_id: &'a DomainId,
    pub command_id: &'a str,
    pub correlation_id: u64,
    pub actor: &'a ActorContext,
    pub base_revision: u64,
    pub cancel_epoch: u64,
    pub result_mode: ResultMode,
    pub operation: Operation,
    pub operands: &'a [Operand<'a>],
    pub z_order: &'a [&'a DomainId],
    pub expected_z_order_revision: u64,
    pub target: Target<'a>,
    pub options: Options,
    pub style_reads: &'a [StyleRead<'a>],
    pub result_fill_style_id: &'a DomainId,
    pub limits: Limits,
}
/// Preowned caller context; reads/checks must not allocate or mutate its authority.
pub trait EpochPort: Send + Sync {
    fn current_epoch(&self) -> Result<u64, Error>;
}
pub struct WorkMeter<'a> {
    token: &'a CancellationToken,
    epoch: &'a dyn EpochPort,
    expected_epoch: u64,
    limit: u64,
    used: u64,
    intersections: u64,
    sweep_events: u64,
    intersection_limit: u64,
    sweep_event_limit: u64,
}
impl<'a> WorkMeter<'a> {
    pub fn new(
        token: &'a CancellationToken,
        epoch: &'a dyn EpochPort,
        expected_epoch: u64,
        limit: u64,
    ) -> Self {
        Self {
            token,
            epoch,
            expected_epoch,
            limit,
            used: 0,
            intersections: 0,
            sweep_events: 0,
            intersection_limit: u64::MAX,
            sweep_event_limit: u64::MAX,
        }
    }
    pub fn check(&self) -> Result<(), Error> {
        self.token.check().map_err(|_| Error::Cancelled)?;
        if self.epoch.current_epoch()? != self.expected_epoch {
            return Err(Error::StaleEpoch);
        }
        Ok(())
    }
    pub fn step(&mut self) -> Result<(), Error> {
        self.check()?;
        let next = self.used.checked_add(1).ok_or(Error::Overflow)?;
        if next > self.limit {
            return Err(Error::BudgetExceeded);
        }
        self.used = next;
        Ok(())
    }
    pub fn used(&self) -> u64 {
        self.used
    }
    pub fn resume(&mut self, used: u64) -> Result<(), Error> {
        if used > self.limit {
            return Err(Error::BudgetExceeded);
        }
        self.used = used;
        self.check()
    }

    pub fn set_geometry_limits(&mut self, intersections: u64, sweep_events: u64) {
        self.intersection_limit = intersections;
        self.sweep_event_limit = sweep_events;
    }
    pub fn record_intersection(&mut self) -> Result<(), Error> {
        self.step()?;
        let next = self.intersections.checked_add(1).ok_or(Error::Overflow)?;
        if next > self.intersection_limit {
            return Err(Error::BudgetExceeded);
        }
        self.intersections = next;
        Ok(())
    }
    pub fn record_sweep_events(&mut self, count: u64) -> Result<(), Error> {
        self.step()?;
        let next = self
            .sweep_events
            .checked_add(count)
            .ok_or(Error::Overflow)?;
        if next > self.sweep_event_limit {
            return Err(Error::BudgetExceeded);
        }
        self.sweep_events = next;
        Ok(())
    }
    pub fn intersections(&self) -> u64 {
        self.intersections
    }
    pub fn sweep_events(&self) -> u64 {
        self.sweep_events
    }
}
