//! Static render-plan evaluator consumes immutable snapshot; same semantics for previews/export.
//! Temporal/effect providers attach through typed ports; no unconditional motion/pulse engine
//! dependency in the static render closure.
#![forbid(unsafe_code)]
pub mod blend;
pub mod evaluate_static;
pub mod lower_plan;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{
    Error as ObserveError, FailureCode, Observation, Observe, Outcome, Receipt as ObserveReceipt,
    SinkPort,
};
use std::fmt;

pub use blend::{
    Applicability, BlendContext, BlendError, BlendMode, BlendReceipt, BlendSpace, Fidelity,
    ModeInfo, OperatorTag, PixelSite, Rgba, StackNode, blend_rgb, composite_over, evaluate_stack,
};
pub use evaluate_static::{
    EvalReport, RenderReceipt, StepEval, StepStatus, Unavailable, evaluate,
};
pub use lower_plan::{
    Limits, LoweredPlan, NodeAddr, SourceValue, Step, layer_projection, lower, node_projection,
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
            Self::BudgetExceeded | Self::Canceled | Self::Overflow => f.write_str(self.code()),
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

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-COMPOSITE","version":1,"operation":"lower (Folio snapshot -> disposable revision-bound plan), evaluate (static extents and receipt), evaluate_stack (single-pixel blend oracle)","operator":{"intent":"Prove one authored layer graph composites with the same ordered semantics for preview and export","result":"LoweredPlan steps in dependency order, per-step extents, RenderReceipt with declared blend space and lowest fidelity class","recovery":"StaleRevision: re-open the snapshot and lower again; Unavailable: resolve the named target (profile, tile, payload) and retry; unsupported blend modes are refused, never replaced by Normal"},"model":{"inputs":"folio::Snapshot, expected revision, BlendSpace (encoded|linear, explicit document input), Limits, CancellationToken","consumer":"composite-consumer --descriptor | --modes | --doc FILE --expected-revision N [--space encoded|linear]","order_convention":"input_ordinal 0 = first composited (bottom-most); SPEC GAP, UNVERIFIED against Photoshop","blend":{"space":"declared, never converted here (STU-COL-163); recorded in the receipt (STU-CMP-021)","alpha":"straight in and out (STU-CMP-022); premultiplied helpers provided","domain":"finite channels and alpha in [0,1], else OutOfDomain","modes":35,"fidelity":["exact","fitted","approximate","unsupported"],"fidelity_meaning":"exact = cited published formula verbatim (not an Adobe parity claim); approximate = community formula or deterministic stand-in; unsupported = typed refusal","refusals":["pass_through on a layer","behind and clear (tool-only)","average negation reflect glow erase (no recovered function)"],"groups":"pass_through = non-isolated; any other mode = isolated; pass_through with opacity<1 is approximate"}},"limits":{"max_steps":4096,"max_depth":64,"max_stack_depth":32,"case_seconds":30},"unsupported":["mask","matte","backdrop","effect","precomp","clipping","knockout","blend_if","fill_opacity","3d","time","vector_or_text_to_image"],"static_closure":"depends only on accord folio pigment observe; no motion pulse reel score render-cpu render-gpu","persistence":"none; plans are disposable and never authoring authority"}"#;
