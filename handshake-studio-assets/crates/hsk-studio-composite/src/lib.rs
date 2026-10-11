//! Static render-plan evaluator consumes immutable snapshot; same semantics for previews/export.
//! Temporal/effect providers attach through typed ports; no unconditional motion/pulse engine
//! dependency in the static render closure.
#![forbid(unsafe_code)]
pub mod evaluate_static;
pub mod lower_plan;
pub mod lower_render_plan;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{
    Error as ObserveError, FailureCode, Observation, Observe, Outcome, Receipt as ObserveReceipt,
    SinkPort,
};
use hsk_studio_pigment::render_plan::PlanError;
use std::fmt;

// Blend maths, the exactness classes and the render-plan IR are owned by pigment (decision
// `diagnostics/render-plan-ir.md`); these are re-exports, never definitions.
pub use hsk_studio_pigment::blend_math::{
    Applicability, BlendError, BlendSpace, Exactness, ExactnessLedger, MODES, ModeFacts,
    StudioBlendMode,
};
pub use evaluate_static::{
    EvalReport, RenderReceipt, StepEval, StepStatus, Unavailable, evaluate,
};
pub use lower_plan::{
    Limits, LoweredPlan, NodeAddr, SourceValue, Step, StepAppearance, layer_projection, lower,
    node_projection,
};
pub use lower_render_plan::{
    MaskTable, RenderLowering, SourceTable, TileInfo, TileInfoSource, lower_to_render_plan,
};

/// Composite failures. Every variant is a typed, stable code; none degrades silently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompositeError {
    /// Render dependency cycle (defence in depth; Folio rejects authored cycles).
    Cycle(String),
    StaleRevision {
        expected: u64,
        actual: u64,
    },
    BudgetExceeded,
    Canceled,
    UnsupportedDescriptor(String),
    DanglingInput(String),
    Unavailable(String),
    Overflow,
    Blend(BlendError),
    /// The lowered render plan failed pigment's shared structural validation.
    Plan(PlanError),
    /// The output target's declared blend space differs from the plan's (never converted).
    ColourMismatch,
}

impl CompositeError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Cycle(_) => "composite_cycle",
            Self::StaleRevision { .. } => "composite_stale_revision",
            Self::BudgetExceeded => "composite_budget_exceeded",
            Self::Canceled => "composite_canceled",
            Self::UnsupportedDescriptor(_) => "composite_unsupported_descriptor",
            Self::DanglingInput(_) => "composite_dangling_input",
            Self::Unavailable(_) => "composite_unavailable",
            Self::Overflow => "composite_overflow",
            Self::Blend(e) => e.code(),
            Self::Plan(e) => e.code(),
            Self::ColourMismatch => "composite_colour_mismatch",
        }
    }
}

impl From<BlendError> for CompositeError {
    fn from(e: BlendError) -> Self {
        Self::Blend(e)
    }
}

impl fmt::Display for CompositeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cycle(t)
            | Self::UnsupportedDescriptor(t)
            | Self::DanglingInput(t)
            | Self::Unavailable(t) => write!(f, "{}:{t}", self.code()),
            Self::StaleRevision { expected, actual } => {
                write!(f, "{}:expected={expected},actual={actual}", self.code())
            }
            Self::Blend(e) => e.fmt(f),
            Self::BudgetExceeded
            | Self::Canceled
            | Self::Overflow
            | Self::Plan(_)
            | Self::ColourMismatch => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for CompositeError {}

/// Observe outcome for a lowering or other fallible step.
pub fn outcome_of<T>(result: &Result<T, CompositeError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(CompositeError::Canceled) => Outcome::Canceled,
        Err(CompositeError::Unavailable(_)) => Outcome::Failure(FailureCode::Unavailable),
        Err(CompositeError::Blend(
            BlendError::Unsupported(_) | BlendError::ToolOnly(_) | BlendError::GroupOnly(_),
        )) => Outcome::Failure(FailureCode::Unsupported),
        Err(_) => Outcome::Failure(FailureCode::Validation),
    }
}

/// Observe outcome for an evaluation: an `Ok` report with unavailable parts is NOT success.
pub fn eval_outcome(result: &Result<EvalReport, CompositeError>) -> Outcome {
    match result {
        Ok(report) => outcome_of(&report.require_ready().map(|()| report)),
        Err(e) => outcome_of::<EvalReport>(&Err(e.clone())),
    }
}

/// Deliver one terminal outcome through the caller-owned sink (no project text is sent).
pub fn report(
    outcome: Outcome,
    correlation_id: u64,
    revision: u64,
    observe: &mut Observe,
    cancel: &CancellationToken,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<ObserveReceipt, ObserveError> {
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-COMPOSITE","version":2,"operation":"lower (Folio snapshot -> disposable revision-bound plan), evaluate (static extents and receipt), lower_to_render_plan (evaluated plan -> pigment RenderPlan + SourceTable)","operator":{"intent":"Prove one authored layer graph composites with the same ordered semantics for preview and export","result":"LoweredPlan steps in dependency order, per-step extents, RenderReceipt with declared blend space and exactness ledger, validated pigment RenderPlan with a SourceKey -> TileRef table","recovery":"StaleRevision: re-open the snapshot and lower again; Unavailable: resolve the named target (profile, tile, payload, tile hashes) and retry; ColourMismatch: build the target with the plan's blend space; unsupported blend modes are refused, never replaced by Normal"},"model":{"inputs":"folio::Snapshot, expected revision, BlendSpace (linear_light|encoded, explicit document input), Limits, CancellationToken; for the render plan also root address, OutputTarget and a caller TileInfoSource (plane hashes, sample format, alpha convention)","consumer":"composite-consumer --descriptor | --modes | --doc FILE --expected-revision N [--space encoded|linear_light]","order_convention":"input_ordinal 0 = first composited (bottom-most); SPEC GAP, UNVERIFIED against Photoshop","owner_of_blend_maths":"hsk_studio_pigment::blend_math (StudioBlendMode, Exactness, facts, formulas, single-pixel oracle) and hsk_studio_pigment::render_plan (IR, validation, backend port); composite re-exports and defines no second copy","render_plan_mapping":"raster layer -> Tile node (several tiles -> isolated Normal group); composite step -> Group (root isolated, others follow the step mode, default Pass Through); hidden and empty layers dropped; each referencing Layer carries the layer appearance (StudioBlendMode from the u16 discriminant, opacity and fill_opacity percent -> fraction, enabled mask -> MaskInput over the union of its coverage tiles with density 1 and not inverted); a root that is not neutral over transparent is wrapped in one isolated layer; clipped layers lower to pigment clip-to-base masks (Approximate; base = nearest unclipped input below, hidden base drops them) while clipped-without-base, clipped-with-own-mask, clipped pass-through groups, a pass-through group serving as a clip base (clip_base_pass_through, Unsupported in the evaluation ledger, Photoshop semantics UNVERIFIED pending the oracle; a hidden or empty pass-through base drops its clipped layers instead) and enabled masks without coverage are Unavailable (never ignored); plan passes RenderPlan::validate before it is returned; only steps reachable from the requested root must be available"},"limits":{"max_steps":4096,"max_depth":64,"case_seconds":30},"unsupported":["matte","backdrop","effect","precomp","clipped layer with its own mask","pass-through group as clip base","knockout","blend_if","mask density feather invert","3d","time","vector_or_text_to_image"],"static_closure":"depends only on accord folio pigment observe; no motion pulse reel score render-cpu render-gpu","persistence":"none; plans are disposable and never authoring authority"}"#;
