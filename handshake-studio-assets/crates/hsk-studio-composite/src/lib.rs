//! Static render-plan evaluator consumes immutable snapshot; same semantics for previews/export.
//! Temporal/effect providers attach through typed ports; no unconditional motion/pulse engine
//! dependency in the static render closure.
#![forbid(unsafe_code)]
pub mod evaluate_static;
pub mod lower_extensions;
pub mod lower_plan;
pub mod lower_render_plan;
pub mod temporal;

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
    EvalReport, RenderReceipt, StepEval, StepStatus, Unavailable, evaluate, evaluate_extended,
};
pub use lower_extensions::LoweringNote;
pub use lower_plan::{
    Limits, LoweredPlan, NodeAddr, SourceValue, Step, StepAppearance, layer_projection, lower,
    node_projection,
};
pub use lower_render_plan::{
    ExtendedLowering, MaskTable, RenderLowering, SourceTable, TileInfo, TileInfoSource,
    lower_to_render_plan, lower_to_render_plan_extended,
};
pub use temporal::{
    FrameLowering, FrameRequest, LayerFrameState, LayerTiming, NoTemporalSource, TemporalError,
    TemporalSource, frame_outcome, lower_frame,
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
        // A plan that uses an operator of class `Unsupported` (for example knockout, carried in
        // the envelope and refused by pigment's admission) is not a success either.
        Ok(report)
            if report.require_ready().is_ok()
                && report.receipt.exactness.lowest == Exactness::Unsupported =>
        {
            Outcome::Failure(FailureCode::Unsupported)
        }
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

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-COMPOSITE","version":3,"operation":"lower (Folio snapshot -> disposable revision-bound plan), evaluate (static extents and receipt), lower_to_render_plan (evaluated plan -> pigment RenderPlan + SourceTable), evaluate_extended and lower_to_render_plan_extended (the same with the document's folio::StudioRasterRecords -> pigment RenderPlan + PlanExtensions envelope + lowering notes)","operator":{"intent":"Prove one authored layer graph composites with the same ordered semantics for preview and export","result":"LoweredPlan steps in dependency order, per-step extents, RenderReceipt with declared blend space and exactness ledger, validated pigment RenderPlan with a SourceKey -> TileRef table","recovery":"StaleRevision: re-open the snapshot and lower again; Unavailable: resolve the named target (profile, tile, payload, tile hashes) and retry; ColourMismatch: build the target with the plan's blend space; unsupported blend modes are refused, never replaced by Normal"},"model":{"inputs":"folio::Snapshot, expected revision, BlendSpace (linear_light|encoded, explicit document input), Limits, CancellationToken; for the render plan also root address, OutputTarget and a caller TileInfoSource (plane hashes, sample format, alpha convention); the extended path also the document's StudioRasterRecords","consumer":"composite-consumer --descriptor | --modes | --doc FILE --expected-revision N [--space encoded|linear_light]","order_convention":"input_ordinal 0 = first composited (bottom-most); SPEC GAP, UNVERIFIED against Photoshop","owner_of_blend_maths":"hsk_studio_pigment::blend_math (StudioBlendMode, Exactness, facts, formulas, single-pixel oracle) and hsk_studio_pigment::render_plan (IR, validation, backend port); composite re-exports and defines no second copy","render_plan_mapping":"raster layer -> Tile node (several tiles -> isolated Normal group); composite step -> Group (root isolated, others follow the step mode, default Pass Through); hidden and empty layers dropped; each referencing Layer carries the layer appearance (StudioBlendMode from the u16 discriminant, opacity and fill_opacity percent -> fraction, enabled mask -> MaskInput over the union of its coverage tiles with density 1 and not inverted); a root that is not neutral over transparent is wrapped in one isolated layer; clipped layers lower to pigment clip-to-base masks (Approximate; base = nearest unclipped input below, hidden base drops them) while clipped-without-base, clipped-with-own-mask, clipped pass-through groups, a pass-through group serving as a clip base (plan_clip_base_pass_through, the code of pigment's PlanError::ClipBasePassThrough and the evaluation-ledger key, test refusal_keys_are_pigments; Unsupported in the evaluation ledger, Photoshop semantics UNVERIFIED pending the oracle; a hidden or empty pass-through base drops its clipped layers instead) and enabled masks without coverage are Unavailable (never ignored); plan passes RenderPlan::validate before it is returned; only steps reachable from the requested root must be available"},"extended_lowering":"adjustment layer (kind adjustment, hsk.studio.adjustment@1, record StudioAdjustment) -> a Layer over a transparent Solid placeholder (the group content extent in an isolated group, the target extent in a pass-through group) plus LayerExtension.adjustment; levels and curve points /255 (an absent channel is the identity; each curve corner flag dropped is a lowering note, Approximate), hue_saturation master (or colorization) values, brightness_contrast carries use_legacy (modern mode is refused by pigment admission), exposure linearize_gamma 1.0 for linear_light and 2.2 for encoded (a lowering note, Approximate), invert, threshold level/255, posterize; mask density/100 and inverted on the MaskInput and feather px in LayerExtension.mask_feather_px; Blend-If level/255 as [black_start, black_end, white_start, white_end], source then destination, per gray/red/green/blue entry (identity entries dropped); knockout Enabled (or Inherited resolving to enabled through containment) -> Knockout::Shallow, which pigment admit_extended refuses as knockout_shallow; effects waves 1 and 2 (an enabled stack at scale 100 percent: drop shadow, colour overlay, solid stroke, inner shadow, outer glow and inner glow with a colour source) -> LayerExtension.effects as pigment EffectOps in list order (percent/100; drop and inner shadows resolve Global Light into their angle; spread and choke are read as a percent of the size and noted Approximate; glow jitter on a solid colour is passed as 0 and noted glow_jitter_ignored_solid_colour); refused Unavailable and not dropped: an enabled effect of any other kind (layer_effects_not_rendered: bevel and emboss, satin, gradient and pattern overlay), a gradient glow (glow_gradient), a glow range other than 100 percent (glow_range), the Precise glow technique (glow_technique_precise), effect-stack scale other than 100 (effect_stack_scale), a contour other than linear (effect_contour), noise above zero (effect_noise), a gradient or pattern stroke fill (effect_stroke_fill), stroke overprint (effect_stroke_overprint), a drop shadow that follows an absent Global Light (effect_global_light_unresolved), an effect colour that is missing, in another profile or mid-tone in a linear-light plan (effect_colour_missing, effect_colour_profile, effect_colour_linear_light), a parameter outside pigment's range or limits (effect_parameter, effect_limit), and more than pigment's MAX_EFFECTS_PER_PLAN (4096) effects over the layers a plan renders (CompositeError::Plan(PlanError::EffectLimit), code plan_effect_limit; a rule of the whole plan, so it depends on the requested root and lower_to_render_plan_extended decides it, evaluate_extended does not; test plan_wide_effect_limit_is_refused_at_lowering_like_pigment_admission), effects on an adjustment, pass-through or clipped layer or on a clip base (effect_on_adjustment_layer, effect_on_pass_through, effect_on_clipped_layer, effect_on_clip_base), a hue band with a non-zero shift (hue_saturation_bands), non-default threshold colours (threshold_colours), exposure use_legacy, channel_blend other than all on, an adjustment layer as clip base (clip_base_adjustment), and a visible adjustment layer contained in a rendered group that has no graph operation, hence no authored stack position (adjustment_not_in_composition_graph; an adjustment layer WITH a graph Source operation is placed by its input_ordinal, Folio ENG-6); Blend-If with a non-identity range on the base of a clipping run or on a clipped layer (including a clipped adjustment layer) is carried to pigment, whose admission refuses it for every backend as blend_if_clip_base | blend_if_clipped_layer (no public source defines it); the evaluation ledger records those operators (class Unsupported, eval_outcome Failure(Unsupported)) instead of blend_if, exactly like pigment's validate (test blend_if_on_clip_roles_is_reported_and_refused_like_pigment); effects together with a non-identity Blend-If on one unclipped layer are carried to pigment, whose admission refuses them for every backend as effects_with_blend_if (intrinsically unsupported), and both ledgers record that operator instead of blend_if; a layer with effects whose content group holds an emitted layer with effects is recorded as effects_nested (class approximate; each backend declares it), both exactly like pigment's validate (test effects_with_blend_if_and_nested_effects_are_reported_like_pigment); a coverage mask on a pass-through group has no defined semantics, so the step is unavailable under pigment's key mask_on_pass_through and both ledgers and the step exactness say unsupported, like the fill rule (test mask_on_a_pass_through_group_is_refused_like_the_fill_rule); Blend Clipped Layers as Group switched off (PSD clbl = 0, Folio blend_clipped_effects false) on a clip base that renders a clipped layer is refused Unavailable blend_clipped_layers_as_group_off_not_modelled in evaluation and lowering, never ignored, because pigment's clip rule is the switched-on reading (test blend_clipped_layers_as_group_off_is_refused_on_a_clip_base); the setting on a layer with no rendered clipped layer changes nothing and is not refused; every class is read from pigment (ExtOperator::key and intrinsic); Photoshop parity NOT_PROVEN, all classes are Studio readings on the D5 oracle list","proof":{"file":"tests/render_plan_lowering.rs","adjustment_layer_in_a_real_folio_graph_at_its_input_ordinal":"adjustment_layer_in_a_real_folio_graph_lowers_at_its_input_ordinal","adjustment_placeholder_entry_and_clip_base":"adjustment_layer_lowers_to_transparent_placeholder_and_envelope_entry","mask_density_invert_feather":"mask_parameters_lower_to_density_invert_and_feather","blend_if_order_and_units":"blend_if_ranges_lower_in_pigment_order_and_units","knockout_and_unsupported_effect_kind_refusals":"knockout_and_effect_stack_are_refused","effects_lower_to_pigment_ops_and_render_as_derived":"effect_stack_lowers_to_pigment_ops_that_render_as_derived","effects_wave2_lower_to_pigment_ops_and_render_as_derived":"inner_shadow_and_glows_lower_to_pigment_ops_that_render_as_derived","effect_refusals":"effect_refusals_are_typed_and_nothing_is_dropped_or_approximated_silently","ledger_keys_and_classes_are_pigments":"evaluation_ledger_keys_and_classes_are_pigments","blend_if_on_clip_roles":"blend_if_on_clip_roles_is_reported_and_refused_like_pigment","blend_clipped_layers_as_group_off":"blend_clipped_layers_as_group_off_is_refused_on_a_clip_base","refusal_keys":"refusal_keys_are_pigments","effects_ops":"effects_with_blend_if_and_nested_effects_are_reported_like_pigment","pass_through_mask":"mask_on_a_pass_through_group_is_refused_like_the_fill_rule","plan_wide_effect_limit":"plan_wide_effect_limit_is_refused_at_lowering_like_pigment_admission"},"limits":{"max_steps":4096,"max_depth":64,"case_seconds":30},"unsupported":["matte","backdrop","effect kinds other than drop shadow, colour overlay, solid stroke, inner shadow, outer glow and inner glow (colour source)","precomp","clipped layer with its own mask","pass-through group as clip base","adjustment layer as clip base","Blend-If on a clip base or a clipped layer (blend_if_clip_base, blend_if_clipped_layer; refused by pigment admission)","effects together with Blend-If on one layer (effects_with_blend_if; refused by pigment admission)","mask on a pass-through group (mask_on_pass_through; refused by pigment admission)","more than MAX_EFFECTS_PER_PLAN effects over the rendered layers of one plan (plan_effect_limit; refused at lowering)","Blend Clipped Layers as Group off on a clip base with a rendered clipped layer","adjustment layer contained in a group without a graph operation","hue_saturation bands","threshold colours","3d","time other than temporal (scale, rotation, anchor, parenting, fractional translation: transform_requires_resampling; translation of a non-raster layer: translate_non_raster_layer; motion blur, time remap, frame blending)","vector_or_text_to_image"],"temporal":"lower_frame(FrameRequest, TemporalSource, tick, cancel) -> FrameLowering: layer in/out windows [in_tick, out_tick) in accord ticks (254016000000 per second; frame to tick is the caller's), a TemporalSource port per layer (visible, opacity_scale multiplies the authored opacity, integer translate_px), the frame document re-validated by Folio and lowered by the unchanged static path (an unchanged frame lowers the caller snapshot as is), translation applied after lowering to every TileInput of the layer and to its mask only when linked, plan re-validated by pigment; fractional translation, scale and rotation refused transform_requires_resampling (pigment plan IR has no transform operator), never rounded; feature motion: MotionTemporalSource samples motion Property streams (opacity percent, position px, scale percent, rotation degrees) with Property::sample; tests tests/temporal.rs; After Effects parity NOT_PROVEN","static_closure":"default build depends only on accord folio pigment observe; no motion pulse reel score render-cpu render-gpu; motion only behind the non-default feature motion (decision T1)","persistence":"none; plans are disposable and never authoring authority"}"#;
