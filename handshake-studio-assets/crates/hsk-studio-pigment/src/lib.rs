//! Pigment source-local CPU masked replacement. Host acceptance and richer raster work remain pending.
#![forbid(unsafe_code)]
pub mod allocation;
pub mod caller;
pub mod masked_patch;
pub mod tile;
pub mod wire;
use hsk_studio_observe::ResourceCode;
pub use masked_patch::*;
pub use tile::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidInput,
    Unsupported,
    Overflow,
    MalformedStride,
    HashMismatch,
    CoverageGap,
    DuplicateTarget,
    Nonfinite,
    AlphaRange,
    RevisionUnavailable,
    RevisionConflict,
    MembershipUnavailable,
    StaleEpoch,
    Canceled,
    BudgetExceeded,
    LeaseUnavailable,
    UnsupportedFinalization,
    DeliveryFailed,
    ReconciliationRequired,
    Prism(hsk_studio_prism::Error),
}
impl Error {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::Unsupported => "unsupported",
            Self::Overflow => "overflow",
            Self::MalformedStride => "malformed_stride",
            Self::HashMismatch => "hash_mismatch",
            Self::CoverageGap => "coverage_gap",
            Self::DuplicateTarget => "duplicate_target",
            Self::Nonfinite => "nonfinite",
            Self::AlphaRange => "alpha_range",
            Self::RevisionUnavailable => "revision_unavailable",
            Self::RevisionConflict => "revision_conflict",
            Self::MembershipUnavailable => "membership_unavailable",
            Self::StaleEpoch => "stale_epoch",
            Self::Canceled => "canceled",
            Self::BudgetExceeded => "budget_exceeded",
            Self::LeaseUnavailable => "lease_unavailable",
            Self::UnsupportedFinalization => "unsupported_finalization",
            Self::DeliveryFailed => "delivery_failed",
            Self::ReconciliationRequired => "reconciliation_required",
            Self::Prism(e) => e.code(),
        }
    }
    pub const fn diagnostic(self) -> ResourceCode {
        match self {
            Self::Unsupported => ResourceCode::Unsupported,
            Self::Overflow => ResourceCode::Overflow,
            Self::HashMismatch => ResourceCode::HashMismatch,
            Self::RevisionConflict => ResourceCode::RevisionConflict,
            Self::RevisionUnavailable | Self::MembershipUnavailable => ResourceCode::Unavailable,
            Self::StaleEpoch => ResourceCode::StaleEpoch,
            Self::Canceled => ResourceCode::Canceled,
            Self::BudgetExceeded => ResourceCode::BudgetExceeded,
            Self::LeaseUnavailable => ResourceCode::LeaseUnavailable,
            Self::UnsupportedFinalization => ResourceCode::UnsupportedFinalization,
            Self::DeliveryFailed | Self::ReconciliationRequired => ResourceCode::Loss,
            Self::Prism(_) => ResourceCode::Unsupported,
            _ => ResourceCode::Validation,
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}
pub const MAX_TILES: usize = 16;
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","version":1,"operation":"masked_replace","ownership":"opaque immutable AcceptedPatch owns charged Box before its outer metadata retirement token; fixed inline PendingToken has no heap lifetime; every Lease clone uses pinned Arc::into_inner to free Arc and boxed port before inline retirement; transferred leases cannot reduce; current-operation generation slots retain meter until final provider reservation drops","api":"immutable Folio TileRef and resolved rgb_f32le colour/separate straight f32le alpha, actual admitted metadata-free Prism linear matrix profiles; caller context/epoch/admission/serialized finalization ports","command":"pigment-consumer --input FILE [--cancel]","wire":"bounded JSON supporting transport version1, complete TileRef; plane/profile/mask paths resolved as bounded external files; all hashes required; six-field Accord actor; explicit current caller membership/revision/epoch/budget/sink configuration","output":"JSON changed/no_change/rejected/reconciliation_required; updated planes as hex and hashes/inverse preimage bytes, exact damage/revisions, actual Prism receipts, Observe v3 bytes/counters; pending token retains caller lease/result","formats":["rgb_f32le","coverage_u8","coverage_u16le"],"limits":{"tiles":16,"byte_limit":16777216,"input_json_bytes":262144,"case_seconds":20},"reservation_unit":"conservative requested owned allocation layout/capacity bytes including Arc layout/padding, Vec-to-Arc overlap, diagnostics, receipt containers and ownership lifetime; shared aggregate caller ledger also admits Prism parser/executor/cache/scratch/output; caller-owned existing context/sink internals are separately pre-admitted by caller; not RSS/physical heap","counter_semantics":"one simultaneous admitted reservation peak tuple across compute/provider/finalization phases; provider component shares exact caller ledger, released peaks remain observable; never sum independent phase maxima","math":"binary64 alpha-aware masked replacement, once binary32 rounding; no colour clipping; zero coverage and unchanged samples retain bytes/padding","consumer_pending":"one-shot CLI refuses indeterminate destination before work; embeddable SerializedOwner retains actual pending result/lease until explicit reconciliation","failure":"missing membership/revision, stale epoch/property, bad hash/stride/extent/coverage, overflow/nonfinite/unsupported profile, lease/sink denial refuse publication; indeterminate retains named pending token","recovery":"correct immutable inputs and fresh caller preconditions; resolve pending token with same owner before retry; retain accepted result until lease owner drops it","manual_route":"same descriptor","argus_route":"same descriptor, typed result/Observe counters/lease accounting","privacy":"no raw pixel/mask/profile/path/private label bytes in telemetry","pending":["host grants/storage/commit/idempotency/restart","full HDR","richer masks/brushes/blends","original-produced brush/blend fidelity","native GUI"]}"#;
