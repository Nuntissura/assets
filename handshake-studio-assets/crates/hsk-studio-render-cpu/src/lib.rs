//! CPU render backend/reference; opt-in professional reference cases require external independent
//! oracles.
#![forbid(unsafe_code)]
mod coverage;
pub mod plan_sink;
pub mod report;
pub mod sample_math;

use hsk_studio_observe::{FailureCode, Outcome};
pub use plan_sink::{
    BlendSpace, NoSources, Op, PixelSink, RenderOptions, RenderPlan, RenderReceipt, Renderer,
    SinkError, SourceId, SourceTile, TileSource,
};
pub use report::report;
pub use sample_math::{BlendMode, blend_mode_name, composite_straight};

/// Typed render failure. Every refusal names its cause; none is a silent fallback or a stale
/// result (STU-RAS-155, STU-ARC-015).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderError {
    Canceled,
    DeadlineExceeded,
    /// The plan revision is not the revision the caller expects to publish.
    StaleRevision {
        expected: u64,
        found: u64,
    },
    /// STU-RAS-154/039 discriminant without a recovered, verified function here (or tool-only,
    /// group-only, or outside the enumeration). Never rendered as Normal.
    UnsupportedBlend(u16),
    UnsupportedOp(&'static str),
    InvalidInput(&'static str),
    InvalidGeometry,
    Overflow,
    MalformedStride,
    HashMismatch,
    Nonfinite,
    AlphaRange,
    /// A source tile was produced in a different profile than the plan's working profile.
    ProfileMismatch,
    /// A source tile is in a different blending space than the plan declares.
    SpaceMismatch,
    UnresolvedSource(u32),
    BudgetExceeded,
    Sink(SinkError),
}

impl RenderError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::StaleRevision { .. } => "stale_revision",
            Self::UnsupportedBlend(_) => "unsupported_blend",
            Self::UnsupportedOp(_) => "unsupported_op",
            Self::InvalidInput(_) => "invalid_input",
            Self::InvalidGeometry => "invalid_geometry",
            Self::Overflow => "overflow",
            Self::MalformedStride => "malformed_stride",
            Self::HashMismatch => "hash_mismatch",
            Self::Nonfinite => "nonfinite",
            Self::AlphaRange => "alpha_range",
            Self::ProfileMismatch => "profile_mismatch",
            Self::SpaceMismatch => "space_mismatch",
            Self::UnresolvedSource(_) => "unresolved_source",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Sink(SinkError::Rejected) => "sink_rejected",
            Self::Sink(SinkError::Unavailable) => "sink_unavailable",
        }
    }
    /// Terminal observe outcome for this error.
    pub const fn outcome(self) -> Outcome {
        match self {
            Self::Canceled => Outcome::Canceled,
            Self::UnsupportedBlend(_) | Self::UnsupportedOp(_) => {
                Outcome::Failure(FailureCode::Unsupported)
            }
            Self::Sink(_) | Self::DeadlineExceeded => Outcome::Failure(FailureCode::Unavailable),
            _ => Outcome::Failure(FailureCode::Validation),
        }
    }
}
impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedBlend(code) => write!(
                f,
                "unsupported_blend({code}:{})",
                blend_mode_name(*code).unwrap_or("unknown")
            ),
            Self::UnsupportedOp(what) | Self::InvalidInput(what) => {
                write!(f, "{}({what})", self.code())
            }
            Self::StaleRevision { expected, found } => {
                write!(f, "stale_revision(expected {expected}, plan {found})")
            }
            Self::UnresolvedSource(id) => write!(f, "unresolved_source({id})"),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for RenderError {}

pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-RENDER_CPU","version":1,"renderer":"hsk-studio-render-cpu/0.1","operation":"render_plan_to_sink","api":"Renderer::render(plan, expected_revision, tiles, cancel, sink) -> RenderReceipt; report(result, correlation, revision, cancel, observe, sink) emits one terminal Observe frame","command":"render-cpu-consumer --descriptor | --selftest","plan_ops":["fill","tile","path_fill"],"blend_modes":{"supported":{"normal":2,"darken":4,"multiply":5,"colour_burn":6,"lighten":8,"screen":9,"colour_dodge":10,"overlay":12,"soft_light":13,"hard_light":14,"difference":18,"exclusion":19,"hue":20,"saturation":21,"colour":22,"luminosity":23},"basis":"W3C Compositing 1 published functions (separable 10.1, non-separable 10.2); Adobe parity NOT_PROVEN","other":"typed unsupported_blend naming the STU-RAS-154/039 mode, never Normal"},"formats":{"colour":"rgb_f32le","alpha":"f32le straight, separate plane","chunk":"tightly packed row-major, sink receives rect + both planes","blend_spaces":["linear_light","encoded"]},"limits":{"ops":65536,"anchors_per_path":65536,"chunk_side_max":4096,"default_chunk_side":64,"default_scratch_bytes":16777216,"scratch_bytes_per_chunk_pixel":32,"aa_samples_per_axis":"1..16","case_seconds":30},"math":"W3C Compositing 1 straight-alpha source-over with separable blend; binary64 intermediate, one binary32 rounding; no colour clipping; path coverage = exact winding of true curves at n*n pixel-centre samples","colour_management":"none: buffers arrive in the declared blending space; profile hash and space label must match the plan, never converted (STU-COL-163)","failure":"cancel/deadline between chunks; stale revision, bad stride/hash/value, unresolved or mismatched source, unsupported blend/op refuse before the first sink call; sink error aborts; scratch released on every exit; receipt only after the last chunk","privacy":"no pixel, profile or path bytes in telemetry","pending":["STU-RAS-154 members without a published function (linear_burn, linear_dodge_add, vivid_light, linear_light, pin_light, hard_mix, lighter_colour, darker_colour, subtract, divide) need an Adobe-produced oracle; dissolve needs a seed contract; pass_through is group-only, behind/clear tool-only","strokes, gradients, patterns, layer effects, masks, clipping, groups, knockout","dissolve (needs seed contract)","16/32-bit integer paths, SIMD, parallel chunks","plan lowering from Folio (composite owns it)","external-oracle parity vs Adobe: NOT_PROVEN"]}"#;
