//! Shared static render-plan IR and backend port (decision: work-folder
//! `diagnostics/render-plan-ir.md`). Composite builds plans; render-cpu and render-gpu implement
//! [`RenderBackend`]. Plain data: no Folio, GPU or device types. Plans are disposable, never
//! persisted (STU-CMP-093), so only `Serialize` (for [`RenderPlan::digest`]) is derived.
use crate::{Mask, Rect, ResolvedTile};
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::ResourceCode;
use serde::Serialize;
use std::time::Instant;

pub use crate::blend_math::{
    Applicability, BlendSpace, Exactness, ExactnessLedger, MODES, ModeFacts, OperatorExactness,
    StudioBlendMode,
};

mod extensions;
mod output;
pub use output::{
    DISPLAY_CLIP_OPERATOR, DisplayTransform, EXTENDED_RANGE_CLAUSE,
    EXTENDED_RANGE_COMBINATION_OPERATOR, EXTENDED_RANGE_MODE_OPERATOR, EXTENDED_RANGE_OPERATOR,
    ExtendedOp, OutputStage, SampleRange,
};
pub use extensions::{
    ALPHA_BOUNDARY_EXACTNESS, ContentAwareFillOp, ExtOperator, Knockout, LayerExtension, PlanExtensions,
    admit_extended, admit_extended_with_limits,
};

/// Blending profile + linearity every plan input must already be in (STU-COL-162/163).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct ColourTag {
    pub profile_sha256: [u8; 32],
    pub blend_space: BlendSpace,
}
/// STU-CMP-022: alpha convention is explicit at every boundary.
///
/// `Premultiplied` at the target or on a tile input is the declarable capability
/// [`ExtOperator::AlphaPremultiplied`]: shared admission refuses it for a backend that does not
/// declare it, before the backend looks at any source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum AlphaConvention {
    Straight,
    Premultiplied,
}
/// The one sample format today. A second variant must come with its admission decision (a
/// declarable operator or an intrinsic refusal) in the same change: the unit test
/// `every_enum_a_backend_refuses_on_has_an_admission_decision` matches this enum exhaustively
/// inside the crate, so adding a variant breaks the build until it is decided. Until then
/// no plan can name another format, so the backends' own `sample_format` refusal cannot be reached.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum SampleFormat {
    /// `rgb_f32le` colour plane plus a separate `f32le` alpha plane.
    RgbF32Le,
}
impl SampleFormat {
    pub const fn key(self) -> &'static str {
        "rgb_f32le"
    }
}
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum MaskFormat {
    CoverageU8,
    CoverageU16Le,
    /// Clip-to-base route (Photoshop clipping group). The coverage is not a plane: it is the
    /// alpha of the layer's clip base, i.e. the nearest EARLIER layer of the same group that is
    /// not itself clipped (consecutive clipped layers share that base). Build it with
    /// [`MaskInput::clip_to_base`]; every other `MaskInput` field must hold its canonical value.
    ///
    /// Evaluation rule (exactness Approximate; Photoshop's exact rule is UNVERIFIED, oracle
    /// pending): the run `[base, c1..cn]` is evaluated as an isolated unit `G`. `G` starts as the
    /// base content with the base's own mask applied (but not its opacity, fill or mode). Each
    /// clipped layer `ci` is composited onto `G` with its own mode, opacity and fill, its source
    /// alpha multiplied by the base alpha, and `G`'s alpha is then held at the base alpha (clipped
    /// content never extends beyond the base). `G` is composited onto the parent's running backdrop
    /// with the BASE layer's mode, opacity and fill. A clipped layer cannot also carry its own
    /// coverage mask (one mask slot), cannot reference a pass-through group, and its base cannot
    /// be one either ([`PlanError::ClipBasePassThrough`]).
    ///
    /// Blend-If is NOT defined inside a clipping run: no public source defines it (decision
    /// work-folder `diagnostics/adjustments-advanced-blending.md` topic `blend-if-clip-roles`), so a
    /// non-identity Blend-If on the base or on a clipped layer is refused by [`admit_extended`] for
    /// every backend as `blend_if_clip_base` / `blend_if_clipped_layer`
    /// ([`ExtOperator::BlendIfClipBase`], [`ExtOperator::BlendIfClippedLayer`]; tests
    /// `tests/blend_if_clip.rs`). The rule above lists only the base's mode, opacity and fill.
    ///
    /// A backend must opt in through [`RenderBackend::supports_clip_to_base`]; [`admit`] refuses
    /// the plan as `Unsupported { operator: "clip_to_base" }` otherwise, so it is never ignored.
    BaseAlpha,
}
impl MaskFormat {
    pub const fn key(self) -> &'static str {
        match self {
            Self::CoverageU8 => "coverage_u8",
            Self::CoverageU16Le => "coverage_u16le",
            Self::BaseAlpha => "clip_to_base",
        }
    }
}

/// Builder-assigned source key; the builder keeps its own key -> document reference table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct SourceKey(pub u32);
/// Index into [`RenderPlan::nodes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct PlanNodeId(pub u32);

/// Content-addressed layer source tile placed in plan pixel space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TileInput {
    pub key: SourceKey,
    pub placement: Rect,
    pub format: SampleFormat,
    pub alpha: AlphaConvention,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}
/// Coverage mask applied to a layer's alpha; resolved to a pigment [`Mask`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct MaskInput {
    pub key: SourceKey,
    pub placement: Rect,
    pub format: MaskFormat,
    pub sha256: [u8; 32],
    pub inverted: bool,
    pub density: f32,
}
impl MaskInput {
    /// The one canonical encoding of a clipped layer: coverage = alpha of the clip base
    /// ([`MaskFormat::BaseAlpha`]). `key`, `placement` and `sha256` carry no information.
    pub const fn clip_to_base() -> Self {
        Self {
            key: SourceKey(0),
            placement: Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            format: MaskFormat::BaseAlpha,
            sha256: [0; 32],
            inverted: false,
            density: 1.0,
        }
    }
    /// Is this the clip-to-base route (as opposed to a coverage plane)?
    pub fn is_clip_to_base(&self) -> bool {
        self.format == MaskFormat::BaseAlpha
    }
}
/// One composited entry of a group. Hidden layers are never emitted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Layer {
    pub input: PlanNodeId,
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub fill_opacity: f32,
    pub mask: Option<MaskInput>,
}
/// `layers` are bottom-first (index 0 composited first). `isolated == false` with a referencing
/// `PassThrough` layer composites onto the parent's running backdrop (W3C s8.2).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Group {
    pub isolated: bool,
    pub layers: Vec<Layer>,
}
/// The role of one layer in a clipping run of its group (the single definition: a clipped layer
/// carries [`MaskInput::clip_to_base`]; its base is the nearest EARLIER unclipped layer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipRole {
    /// Neither clipped nor the base of a clipped layer.
    Unclipped,
    /// Unclipped, and the layer directly above it is clipped (so a run `[base, c1..cn]` follows).
    Base,
    /// Clipped to the nearest earlier unclipped layer.
    Clipped,
}
impl Group {
    /// The clip role of `layers[index]`; `None` when `index` is out of range.
    pub fn clip_role(&self, index: usize) -> Option<ClipRole> {
        let is_clipped = |l: &Layer| l.mask.as_ref().is_some_and(MaskInput::is_clip_to_base);
        let layer = self.layers.get(index)?;
        Some(if is_clipped(layer) {
            ClipRole::Clipped
        } else if self.layers.get(index.saturating_add(1)).is_some_and(is_clipped) {
            ClipRole::Base
        } else {
            ClipRole::Unclipped
        })
    }
}
/// No vector payload (uninhabited). Builders and backends that share a nib payload pick their own `V`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum NoVector {}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum PlanNode<V = NoVector> {
    Tile(TileInput),
    /// Straight RGBA fill.
    Solid {
        rect: Rect,
        rgba: [f32; 4],
    },
    Vector(V),
    Group(Group),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct OutputTarget {
    pub extent: Rect,
    pub format: SampleFormat,
    pub alpha: AlphaConvention,
    pub space: ColourTag,
}
/// Index-ordered DAG: every input references an earlier node (acyclic by construction); `root`
/// renders onto a transparent backdrop over `target.extent`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RenderPlan<V = NoVector> {
    pub revision: u64,
    pub target: OutputTarget,
    pub nodes: Vec<PlanNode<V>>,
    pub root: PlanNodeId,
}

/// The plan budgets [`RenderPlan::validate`] enforces: node count, total layer count over all
/// groups, and group nesting depth. [`PlanLimits::MAXIMUM`] holds the hard maxima and is what
/// `Default` and [`admit`] use. A host may TIGHTEN any field through [`admit_with_limits`] and
/// [`admit_extended_with_limits`] (or by calling [`RenderPlan::validate`]); a field above its
/// maximum is refused as [`PlanError::LimitAboveMaximum`] by every one of those entry points, never
/// clamped (test `tests/plan_limits.rs` `loosening_past_the_maxima_is_a_typed_error_everywhere`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanLimits {
    pub max_nodes: usize,
    pub max_layers: usize,
    pub max_depth: usize,
}
impl PlanLimits {
    /// The hard maxima (`Default`): 65 536 nodes, 65 536 layers, nesting depth 64 (test
    /// `tests/plan_limits.rs` `defaults_are_the_maxima_and_unchanged`).
    pub const MAXIMUM: Self = Self {
        max_nodes: 65_536,
        max_layers: 65_536,
        max_depth: 64,
    };
    /// `Ok` when no field is above [`PlanLimits::MAXIMUM`] (smaller, including zero, only
    /// tightens); otherwise [`PlanError::LimitAboveMaximum`] naming the first offending field in the
    /// order nodes, layers, depth. [`RenderPlan::validate`] runs this first, so the maxima hold
    /// for every caller that passes limits, not only for the admission functions.
    pub fn within_maximum(&self) -> Result<(), PlanError> {
        let fields = [
            (
                PlanLimitKind::Nodes,
                self.max_nodes,
                Self::MAXIMUM.max_nodes,
            ),
            (
                PlanLimitKind::Layers,
                self.max_layers,
                Self::MAXIMUM.max_layers,
            ),
            (
                PlanLimitKind::Depth,
                self.max_depth,
                Self::MAXIMUM.max_depth,
            ),
        ];
        for (kind, requested, maximum) in fields {
            if requested > maximum {
                return Err(PlanError::LimitAboveMaximum {
                    kind,
                    requested: u64::try_from(requested).unwrap_or(u64::MAX),
                    maximum: u64::try_from(maximum).unwrap_or(u64::MAX),
                });
            }
        }
        Ok(())
    }
}
impl Default for PlanLimits {
    fn default() -> Self {
        Self::MAXIMUM
    }
}
/// Which [`PlanLimits`] field a [`PlanError::LimitAboveMaximum`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlanLimitKind {
    /// [`PlanLimits::max_nodes`].
    Nodes,
    /// [`PlanLimits::max_layers`].
    Layers,
    /// [`PlanLimits::max_depth`].
    Depth,
}
impl PlanLimitKind {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Nodes => "max_nodes",
            Self::Layers => "max_layers",
            Self::Depth => "max_depth",
        }
    }
}

/// One feature a plan uses; the unit of capability and exactness accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operator {
    Tile,
    Solid,
    Vector,
    Mask(MaskFormat),
    Blend(StudioBlendMode),
    IsolatedGroup,
    PassThroughGroup,
    /// Pass-through group referenced with opacity < 1 (ill-defined for non-isolated groups).
    PassThroughOpacity,
    /// Non-isolated group composited with a mode other than PassThrough.
    NonIsolatedBlendedGroup,
    /// `fill_opacity < 1` with a non-Normal mode; Photoshop fill semantics unverified.
    FillOpacity,
}
impl Operator {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Tile => "tile",
            Self::Solid => "solid",
            Self::Vector => "vector",
            Self::Mask(f) => f.key(),
            Self::Blend(m) => m.key(),
            Self::IsolatedGroup => "group_isolated",
            Self::PassThroughGroup => "group_pass_through",
            Self::PassThroughOpacity => "group_pass_through_opacity",
            Self::NonIsolatedBlendedGroup => "group_non_isolated_blended",
            Self::FillOpacity => "fill_opacity",
        }
    }
    /// Class independent of any backend (formula provenance).
    pub const fn intrinsic(self) -> Exactness {
        match self {
            Self::Blend(m) => m.facts().exactness,
            Self::PassThroughOpacity | Self::FillOpacity => Exactness::Approximate,
            // Clipping-group rule UNVERIFIED against Photoshop (see `MaskFormat::BaseAlpha`).
            Self::Mask(MaskFormat::BaseAlpha) => Exactness::Approximate,
            _ => Exactness::Exact,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    Empty,
    BudgetExceeded,
    RootOutOfRange,
    ForwardReference { node: u32, input: u32 },
    EmptyRect { node: u32 },
    NonFinite { node: u32 },
    OutOfRange { node: u32 },
    ModeNotApplicable { node: u32, mode: StudioBlendMode },
    PassThroughOnIsolated { node: u32 },
    TooDeep,
    /// A clipped layer has no unclipped layer below it in the same group.
    ClipWithoutBase { node: u32 },
    /// A clip-to-base mask is not the canonical [`MaskInput::clip_to_base`] encoding.
    ClipFields { node: u32 },
    /// A clipped layer references a pass-through group (no isolation to clip within).
    ClipPassThrough { node: u32 },
    /// The base of a clipping run (nearest earlier unclipped layer) composites with the
    /// PassThrough mode: a pass-through group has no isolated content to take a base alpha from,
    /// and the run is composited with the BASE layer's mode, which is undefined for PassThrough.
    ClipBasePassThrough { node: u32 },
    /// Envelope entry names a node that is not a group, or a layer index out of range.
    ExtensionTarget { node: u32 },
    /// Two envelope entries for the same `(group, layer)`.
    ExtensionDuplicate { node: u32 },
    /// Knockout other than `None` (not implemented; refused: `tests/plan_extensions.rs`
    /// `envelope_validation_rules`, `envelope_admission_and_default_route`).
    KnockoutUnsupported { node: u32, depth: Knockout },
    /// An adjustment layer's input is not the canonical transparent `Solid` placeholder.
    AdjustmentInput { node: u32 },
    /// Adjustment parameters out of their declared range.
    AdjustmentParameter { node: u32 },
    /// A recognised adjustment feature without a formula in this slice.
    AdjustmentUnsupported { node: u32, kind: crate::adjust_math::AdjustmentKind },
    /// An adjustment layer is the base of a clipping run (no defined base alpha).
    AdjustmentClipBase { node: u32 },
    /// Blend-If on a pass-through layer (no isolated source to weigh).
    BlendIfPassThrough { node: u32 },
    /// Blend-If range pair unordered or channel listed twice.
    BlendIfRange { node: u32 },
    /// Mask feather on a layer without a coverage mask.
    FeatherWithoutMask { node: u32 },
    /// Layer effects on an adjustment layer (its placeholder has no shape to derive a matte from).
    EffectOnAdjustment { node: u32 },
    /// Layer effects on a pass-through layer (no isolated source whose alpha could be the matte).
    EffectPassThrough { node: u32 },
    /// Layer effects on a clipped layer or on the base of a clipping run (Photoshop's rule for
    /// effects inside a clipping group is UNVERIFIED, so the combination is refused).
    EffectClipRun { node: u32 },
    /// An effect parameter is non-finite or outside its declared range (colour, opacity, angle,
    /// spread), or the effect blend mode is not layer-applicable.
    EffectParameter { node: u32 },
    /// An effect size or distance above its limit, or more effects than the per-layer or
    /// per-envelope budget (`effect_math::MAX_EFFECT_SIZE_PX`, `MAX_EFFECTS_PER_LAYER`, ...).
    EffectLimit { node: u32 },
    /// A recognised effect feature that is refused instead of approximated (gradient glow, glow
    /// range, glow jitter, Precise glow technique): admission reports it as the feature key.
    EffectUnsupported {
        node: u32,
        feature: crate::effect_math::EffectFeature,
    },
    /// A layer transform ([`LayerExtension::transform`]) on a layer whose input is not a `Tile`
    /// node (only raster sources are resampled; a group, solid or vector layer is refused).
    TransformTarget { node: u32 },
    /// A layer transform matrix is non-finite, singular or outside the scale/translation bounds,
    /// or its destination bounds are out of range (`transform_math::TransformError`).
    TransformMatrix {
        node: u32,
        error: crate::transform_math::TransformError,
    },
    /// More transformed layers or more transformed destination area than the envelope budgets
    /// (`transform_math::MAX_TRANSFORMS_PER_PLAN`, `MAX_TRANSFORM_AREA_PER_PLAN`).
    TransformLimit { node: u32 },
    /// A layer transform combined with a feature it is not defined with in this slice: layer
    /// effects on the same layer, or a transformed (linked) mask with a mask feather, or a
    /// transformed mask without a coverage mask.
    TransformCombination { node: u32 },
    /// A layer blur ([`LayerExtension::blur`]) combined with a feature it is not defined with:
    /// an adjustment layer, effects or a non-identity Blend-If on the same layer, a pass-through
    /// layer, a clipped layer or a clip base, or a backdrop blur in a non-isolated group.
    BlurCombination { node: u32 },
    /// A filter list ([`LayerExtension::filters`]) with a parameter outside its range or more than
    /// `filter_math::MAX_FILTERS_PER_LAYER` entries.
    FilterParameter { node: u32 },
    /// Filters combined with a feature they are not defined with: an adjustment layer, effects, a
    /// transform or a blur on the same layer, a pass-through layer, a clipped layer or a clip base.
    FilterCombination { node: u32 },
    /// A content-aware fill ([`LayerExtension::content_aware_fill`]) with options out of range, a
    /// clip-to-base or density-scaled hole mask, or an invalid hole / sampling rectangle.
    ContentAwareParameter { node: u32 },
    /// A content-aware fill combined with an adjustment layer, effects, a transform, a blur or
    /// filters on the same layer, a pass-through layer, a clipped layer or a clip base.
    ContentAwareCombination { node: u32 },
    /// A [`PlanLimits`] field is above its hard maximum ([`PlanLimits::MAXIMUM`]): limits can only
    /// tighten. Not a defect of the plan: the caller's limits are refused before the plan is read.
    LimitAboveMaximum {
        kind: PlanLimitKind,
        requested: u64,
        maximum: u64,
    },
}
impl PlanError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "plan_empty",
            Self::BudgetExceeded => "plan_budget_exceeded",
            Self::RootOutOfRange => "plan_root_out_of_range",
            Self::ForwardReference { .. } => "plan_forward_reference",
            Self::EmptyRect { .. } => "plan_empty_rect",
            Self::NonFinite { .. } => "plan_nonfinite",
            Self::OutOfRange { .. } => "plan_out_of_range",
            Self::ModeNotApplicable { .. } => "plan_mode_not_applicable",
            Self::PassThroughOnIsolated { .. } => "plan_pass_through_on_isolated",
            Self::TooDeep => "plan_too_deep",
            Self::ClipWithoutBase { .. } => "plan_clip_without_base",
            Self::ClipFields { .. } => "plan_clip_fields",
            Self::ClipPassThrough { .. } => "plan_clip_pass_through",
            Self::ClipBasePassThrough { .. } => "plan_clip_base_pass_through",
            Self::ExtensionTarget { .. } => "plan_extension_target",
            Self::ExtensionDuplicate { .. } => "plan_extension_duplicate",
            Self::KnockoutUnsupported { .. } => "plan_knockout_unsupported",
            Self::AdjustmentInput { .. } => "plan_adjustment_input",
            Self::AdjustmentParameter { .. } => "plan_adjustment_parameter",
            Self::AdjustmentUnsupported { .. } => "plan_adjustment_unsupported",
            Self::AdjustmentClipBase { .. } => "plan_adjustment_clip_base",
            Self::BlendIfPassThrough { .. } => "plan_blend_if_pass_through",
            Self::BlendIfRange { .. } => "plan_blend_if_range",
            Self::FeatherWithoutMask { .. } => "plan_feather_without_mask",
            Self::EffectOnAdjustment { .. } => "plan_effect_on_adjustment",
            Self::EffectPassThrough { .. } => "plan_effect_pass_through",
            Self::EffectClipRun { .. } => "plan_effect_clip_run",
            Self::EffectParameter { .. } => "plan_effect_parameter",
            Self::EffectLimit { .. } => "plan_effect_limit",
            Self::EffectUnsupported { .. } => "plan_effect_unsupported",
            Self::LimitAboveMaximum { .. } => "plan_limit_above_maximum",
            Self::TransformTarget { .. } => "plan_transform_target",
            Self::TransformMatrix { .. } => "plan_transform_matrix",
            Self::TransformLimit { .. } => "plan_transform_limit",
            Self::TransformCombination { .. } => "plan_transform_combination",
            Self::BlurCombination { .. } => "plan_blur_combination",
            Self::FilterParameter { .. } => "plan_filter_parameter",
            Self::FilterCombination { .. } => "plan_filter_combination",
            Self::ContentAwareParameter { .. } => "plan_content_aware_parameter",
            Self::ContentAwareCombination { .. } => "plan_content_aware_combination",
        }
    }
}

fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
fn rect_ok(r: Rect, node: u32) -> Result<(), PlanError> {
    if r.width == 0 || r.height == 0 || r.end().is_err() {
        return Err(PlanError::EmptyRect { node });
    }
    Ok(())
}

impl<V> RenderPlan<V> {
    /// Validation shared by every backend, in two parts that read different node sets.
    ///
    /// Structure is checked over EVERY node, reachable or not: the limits, root and input indexes
    /// (a forward reference is also what makes a cycle impossible), rects, finite and unit ranges,
    /// mode applicability and the clipping-run rules. A malformed node is a defect of the plan
    /// whether or not a layer reaches it.
    ///
    /// Capability is collected over the nodes REACHABLE from the root only: the returned operators,
    /// distinct and in first-use order (node index order), are what a backend has to support to
    /// execute the plan. Both backends skip a node no layer reaches before they look at it
    /// (render-cpu and render-gpu `prepare`: `reach`), and every admission predicate reads the same
    /// set ([`RenderPlan::has_pass_through_mask`] and its siblings), so an unreachable node that
    /// uses an operator a backend refuses admits, while the same node reached from the root is
    /// refused with the same key (tests `tests/plan_reachability.rs`
    /// `unreachable_unsupported_operator_admits_reachable_one_is_refused`,
    /// `structure_stays_checked_on_unreachable_nodes`). The same holds for the operators of the
    /// envelope entries of such a node ([`PlanExtensions::validate`], test
    /// `tests/plan_followups.rs` `unreachable_envelope_entry_asks_for_nothing_reachable_one_is_refused`).
    /// Unspecified modes pass here and are refused by [`admit`] as Unsupported.
    ///
    /// `limits` can only tighten the budgets: a field above [`PlanLimits::MAXIMUM`] is refused as
    /// [`PlanError::LimitAboveMaximum`] before the plan is read. The depth budget is compared for
    /// every node, a leaf and a group without layers included (depth 1), so `max_depth = 0`
    /// refuses every plan (test `tests/plan_followups.rs`
    /// `depth_limit_applies_to_every_plan_shape`).
    pub fn validate(&self, limits: &PlanLimits) -> Result<Vec<Operator>, PlanError> {
        limits.within_maximum()?;
        if self.nodes.is_empty() {
            return Err(PlanError::Empty);
        }
        if self.nodes.len() > limits.max_nodes || u32::try_from(self.nodes.len()).is_err() {
            return Err(PlanError::BudgetExceeded);
        }
        if self.root.0 as usize >= self.nodes.len() {
            return Err(PlanError::RootOutOfRange);
        }
        rect_ok(self.target.extent, self.root.0)?;
        // Operators each node uses, in node order. Every check below runs for every node; whether
        // an operator counts is decided once, after the structure passed, from the reachable set
        // (so an over-budget plan is refused before the traversal, and the candidates are bounded
        // by the layer budget already checked).
        let mut used: Vec<(usize, Operator)> = Vec::new();
        let mut depth = vec![1usize; self.nodes.len()];
        let mut layers = 0usize;
        for (i, node) in self.nodes.iter().enumerate() {
            let n = i as u32;
            let first = used.len();
            let mut add = |op: Operator| {
                if !used[first..].contains(&(i, op)) {
                    used.push((i, op));
                }
            };
            match node {
                PlanNode::Tile(t) => {
                    rect_ok(t.placement, n)?;
                    add(Operator::Tile);
                }
                PlanNode::Solid { rect, rgba } => {
                    rect_ok(*rect, n)?;
                    if !rgba.iter().all(|c| c.is_finite()) {
                        return Err(PlanError::NonFinite { node: n });
                    }
                    if !unit(rgba[3]) {
                        return Err(PlanError::OutOfRange { node: n });
                    }
                    add(Operator::Solid);
                }
                PlanNode::Vector(_) => add(Operator::Vector),
                PlanNode::Group(g) => {
                    layers = layers.saturating_add(g.layers.len());
                    if layers > limits.max_layers {
                        return Err(PlanError::BudgetExceeded);
                    }
                    // Index of the nearest earlier unclipped layer of this group: the clip base of
                    // a clipped layer. Tracked while walking the layers (one step per layer), so a
                    // group of `max_layers` clipped layers costs a linear walk, not a backwards
                    // search per layer (PLR-075, tests `tests/plan_followups.rs`
                    // `clip_run_validation_is_linear` and `clip_run_errors_match_the_reference_rule`).
                    let mut clip_base: Option<usize> = None;
                    for (li, l) in g.layers.iter().enumerate() {
                        let input = l.input.0 as usize;
                        if input >= i {
                            return Err(PlanError::ForwardReference {
                                node: n,
                                input: l.input.0,
                            });
                        }
                        if !(l.opacity.is_finite() && l.fill_opacity.is_finite()) {
                            return Err(PlanError::NonFinite { node: n });
                        }
                        if !unit(l.opacity) || !unit(l.fill_opacity) {
                            return Err(PlanError::OutOfRange { node: n });
                        }
                        depth[i] = depth[i].max(depth[input] + 1);
                        if depth[i] > limits.max_depth {
                            return Err(PlanError::TooDeep);
                        }
                        let facts = l.mode.facts();
                        let child_group = match &self.nodes[input] {
                            PlanNode::Group(c) => Some(c.isolated),
                            _ => None,
                        };
                        match (facts.applicability, child_group) {
                            (Applicability::ToolOnly, _) | (Applicability::GroupOnly, None) => {
                                return Err(PlanError::ModeNotApplicable {
                                    node: n,
                                    mode: l.mode,
                                });
                            }
                            (Applicability::GroupOnly, Some(true)) => {
                                return Err(PlanError::PassThroughOnIsolated { node: n });
                            }
                            (Applicability::GroupOnly, Some(false)) => {
                                add(Operator::PassThroughGroup);
                                if l.opacity < 1.0 {
                                    add(Operator::PassThroughOpacity);
                                }
                            }
                            (_, child) => {
                                match child {
                                    Some(true) => add(Operator::IsolatedGroup),
                                    Some(false) => add(Operator::NonIsolatedBlendedGroup),
                                    None => {}
                                }
                                add(Operator::Blend(l.mode));
                            }
                        }
                        // A pass-through layer has no group result for fill to scale: not the
                        // generic FillOpacity operator; `admit` refuses it uniformly (R5).
                        if l.fill_opacity < 1.0
                            && l.mode != StudioBlendMode::Normal
                            && l.mode != StudioBlendMode::PassThrough
                        {
                            add(Operator::FillOpacity);
                        }
                        if let Some(m) = &l.mask
                            && m.is_clip_to_base()
                        {
                            // Photoshop clipping group: the base is the nearest earlier layer of
                            // this group that is not itself clipped, so a clipped layer is valid
                            // exactly when some earlier layer is unclipped.
                            if *m != MaskInput::clip_to_base() {
                                return Err(PlanError::ClipFields { node: n });
                            }
                            let Some(base) = clip_base else {
                                return Err(PlanError::ClipWithoutBase { node: n });
                            };
                            if facts.applicability == Applicability::GroupOnly {
                                return Err(PlanError::ClipPassThrough { node: n });
                            }
                            // The run is composited with the BASE layer's mode, undefined for the
                            // group-only PassThrough mode (no isolated content to clip within).
                            if g.layers[base].mode.facts().applicability == Applicability::GroupOnly {
                                return Err(PlanError::ClipBasePassThrough { node: n });
                            }
                            add(Operator::Mask(MaskFormat::BaseAlpha));
                        } else if let Some(m) = &l.mask {
                            rect_ok(m.placement, n)?;
                            if !m.density.is_finite() {
                                return Err(PlanError::NonFinite { node: n });
                            }
                            if !unit(m.density) {
                                return Err(PlanError::OutOfRange { node: n });
                            }
                            add(Operator::Mask(m.format));
                        }
                        if !l.mask.as_ref().is_some_and(MaskInput::is_clip_to_base) {
                            clip_base = Some(li);
                        }
                    }
                }
            }
            // The depth budget applies to every node, a leaf or a group without layers (depth 1)
            // included: the comparison in the layer loop only sees a group that has a layer
            // (PLR-072), so `max_depth = 0` refuses every plan only with this one.
            if depth[i] > limits.max_depth {
                return Err(PlanError::TooDeep);
            }
        }
        // Capability: only the operators of nodes the root reaches, distinct, in node order.
        let reachable = self.reachable_nodes();
        let mut ops: Vec<Operator> = Vec::new();
        for (i, op) in used {
            if reachable.get(i).copied().unwrap_or(false) && !ops.contains(&op) {
                ops.push(op);
            }
        }
        Ok(ops)
    }
}
/// Operator key of the shared refusal of a pass-through layer with `fill_opacity < 1` (the key
/// render-cpu already reports; one refusal for every backend, made by [`admit`]).
pub const PASS_THROUGH_FILL_OPERATOR: &str = "group_pass_through_fill_opacity";
/// Operator key of the shared refusal of a coverage mask on a pass-through layer (the key
/// render-cpu and render-gpu already report; one refusal for every backend, made by [`admit`] and
/// [`admit_extended`]; test `tests/admit_parity.rs` `pass_through_mask_is_refused_for_every_backend`).
pub const PASS_THROUGH_MASK_OPERATOR: &str = "mask_on_pass_through";
impl<V> RenderPlan<V> {
    /// Does any group layer of a node reachable from the root composite a pass-through group with
    /// `fill_opacity < 1`? Fill on a non-isolated group is undefined (no group result to scale,
    /// like opacity), so it is intrinsically Unsupported and [`admit`] refuses it for every
    /// backend. Only reachable groups count, exactly like the backends, which never look at an
    /// unreachable node (test `tests/admit_parity.rs`
    /// `unreachable_pass_through_fill_is_not_counted_like_the_backends`).
    pub fn has_pass_through_fill(&self) -> bool {
        let reachable = self.reachable_nodes();
        self.nodes.iter().enumerate().any(|(i, n)| match n {
            PlanNode::Group(g) if reachable[i] => g
                .layers
                .iter()
                .any(|l| l.mode == StudioBlendMode::PassThrough && l.fill_opacity < 1.0),
            _ => false,
        })
    }
}
impl<V> RenderPlan<V> {
    /// `flags[n]`: node `n` is reachable from the root through group layer inputs. This is the set
    /// both backends execute: every other node is skipped before its source is verified and before
    /// its layer checks (render-cpu `prepare`, render-gpu `prepare`: `reach`), so a rule that a
    /// backend enforces on a node must be decided here on the same set (the operator collection of
    /// [`RenderPlan::validate`] and of [`PlanExtensions::validate`], and the effect blend modes of
    /// [`PlanExtensions::effect_modes_in`], included). An index outside the plan is ignored
    /// (`validate` refuses it), so this does not panic on an unvalidated plan (tests
    /// `tests/plan_reachability.rs` `structure_stays_checked_on_unreachable_nodes`, a cyclic plan,
    /// and `tests/admit_parity.rs` `unreachable_nodes_are_not_counted_like_the_backends`, a root
    /// and an input out of range).
    fn reachable_nodes(&self) -> Vec<bool> {
        let mut flags = vec![false; self.nodes.len()];
        let mut pending = vec![self.root.0 as usize];
        while let Some(index) = pending.pop() {
            match flags.get_mut(index) {
                Some(seen) if !*seen => *seen = true,
                _ => continue,
            }
            if let Some(PlanNode::Group(g)) = self.nodes.get(index) {
                pending.extend(g.layers.iter().map(|l| l.input.0 as usize));
            }
        }
        flags
    }
    /// Does any pass-through layer of a node reachable from the root carry a coverage mask
    /// (inverted, density and feather included)? A pass-through layer composites its non-isolated
    /// group straight onto the running backdrop, so there is no group result a mask could weigh.
    /// Pigment defines the rule for pass-through opacity (a premultiplied lerp,
    /// `StackNode::Group`) but none for a mask: `evaluate_stack` has no mask input on a group, and
    /// W3C Compositing 1 makes a masked group isolated, which a pass-through group by definition is
    /// not. The combination is therefore intrinsically Unsupported and [`admit`] /
    /// [`admit_extended`] refuse it for every backend as [`PASS_THROUGH_MASK_OPERATOR`] (decision:
    /// work-folder `diagnostics/adjustments-advanced-blending.md` topic `admission-parity`). Only
    /// reachable groups count, exactly like the backends, which never look at an unreachable node
    /// (test `tests/admit_parity.rs` `unreachable_nodes_are_not_counted_like_the_backends`). A
    /// clipped pass-through layer is a structural error ([`PlanError::ClipPassThrough`]) and not
    /// counted here.
    pub fn has_pass_through_mask(&self) -> bool {
        let reachable = self.reachable_nodes();
        self.nodes.iter().enumerate().any(|(i, n)| match n {
            PlanNode::Group(g) if reachable[i] => g.layers.iter().any(|l| {
                l.mode == StudioBlendMode::PassThrough
                    && l.mask.as_ref().is_some_and(|m| !m.is_clip_to_base())
            }),
            _ => false,
        })
    }
    /// Does the plan declare a premultiplied alpha convention at a boundary it executes: the
    /// target, or a tile input reachable from the root? STU-CMP-022 defines internal compositing on
    /// one declared convention with a conversion at the boundary; pigment's maths is straight, so a
    /// backend declares whether it converts ([`ExtOperator::AlphaPremultiplied`]). The target is
    /// always executed; a tile no layer reaches is never read by a backend (it is skipped before
    /// `verify_tile`), so it asks for nothing (test `tests/admit_parity.rs`
    /// `unreachable_nodes_are_not_counted_like_the_backends`).
    pub fn uses_premultiplied_alpha(&self) -> bool {
        if self.target.alpha == AlphaConvention::Premultiplied {
            return true;
        }
        let reachable = self.reachable_nodes();
        self.nodes.iter().enumerate().any(|(i, n)| {
            reachable[i]
                && matches!(n, PlanNode::Tile(t) if t.alpha == AlphaConvention::Premultiplied)
        })
    }
    /// Is the root a leaf (tile, solid or vector) rather than a group? A leaf root is composited
    /// as one Normal, opacity-1 layer over the transparent target backdrop (render-cpu
    /// `compute_chunk`, render-gpu `leaf_root_class`), so the backend's `Blend(Normal)` class
    /// applies to it although the plan lists no layer.
    pub fn has_leaf_root(&self) -> bool {
        !matches!(self.nodes.get(self.root.0 as usize), Some(PlanNode::Group(_)))
    }
}
impl<V: Serialize> RenderPlan<V> {
    /// SHA-256 of the canonical JSON bytes; binds a receipt to the exact plan within one build.
    /// Not a persisted format and not a cross-version identity.
    pub fn digest(&self) -> Result<[u8; 32], crate::Error> {
        let bytes = serde_json::to_vec(self).map_err(|_| crate::Error::InvalidInput)?;
        Ok(hsk_studio_prism::profile_hash(&bytes))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceClass {
    Cpu,
    Gpu,
}
/// Renderer identity for receipts; no GPU API types (adapter is plain text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendIdentity {
    pub name: &'static str,
    pub version: &'static str,
    pub device: DeviceClass,
    pub adapter: String,
    pub generation: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecuteRequest {
    pub expected_revision: u64,
    /// Lowest acceptable class (export typically `Exact`/`Fitted`, preview may accept `Approximate`).
    pub min_exactness: Exactness,
    /// Observed between bounded chunks.
    pub deadline: Option<Instant>,
    pub max_working_bytes: u64,
}
/// Folio-free resolved source planes, already in the plan's colour tag (STU-COL-163).
#[derive(Clone, Copy, Debug)]
pub struct TilePlanes<'a> {
    pub width: u32,
    pub height: u32,
    pub colour: &'a [u8],
    pub colour_stride_bytes: u64,
    pub alpha: &'a [u8],
    pub alpha_stride_bytes: u64,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
    pub format: SampleFormat,
    pub alpha_convention: AlphaConvention,
    pub space: ColourTag,
}
impl TilePlanes<'_> {
    /// Does this resolved source carry exactly what the plan declared?
    pub fn matches(&self, input: &TileInput, space: ColourTag) -> bool {
        self.width == input.placement.width
            && self.height == input.placement.height
            && self.colour_sha256 == input.colour_sha256
            && self.alpha_sha256 == input.alpha_sha256
            && self.format == input.format
            && self.alpha_convention == input.alpha
            && self.space == space
    }
}
impl<'a> ResolvedTile<'a> {
    /// Adapter from the canonical pigment tile (`rgb_f32le`, `linear_light`, straight; shape
    /// checked by `validate_shape`; hashes are checked by the backend at execution).
    pub fn planes(&self) -> Result<TilePlanes<'a>, crate::Error> {
        if self.sample_format != "rgb_f32le"
            || self.transfer != "linear_light"
            || self.alpha_association != "straight"
        {
            return Err(crate::Error::Unsupported);
        }
        Ok(TilePlanes {
            width: self.bounds.width,
            height: self.bounds.height,
            colour: self.colour_bytes,
            colour_stride_bytes: self.colour_stride_bytes,
            alpha: self.alpha_bytes,
            alpha_stride_bytes: self.alpha_stride_bytes,
            colour_sha256: self.colour_sha256,
            alpha_sha256: self.alpha_sha256,
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: ColourTag {
                profile_sha256: self.profile.expected_sha256,
                blend_space: BlendSpace::LinearLight,
            },
        })
    }
}
/// Resolves plan source keys to immutable planes. `None` = unresolved, a typed failure.
pub trait PlanSources: Send + Sync {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>>;
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkError {
    Rejected,
    Unavailable,
}
/// Caller-owned receiver of output chunks: `colour` = `w*h*12` bytes `rgb_f32le`, `alpha` =
/// `w*h*4` bytes `f32le` in the target convention, tightly packed. Provisional until `execute`
/// returns `Ok`; after `Err` everything delivered is unpublished.
pub trait TileSink {
    fn accept(&mut self, rect: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError>;
}

/// Receipt binding output to plan, renderer and colour (STU-ARC-015, STU-CMP-021). Exists only
/// after the last chunk was accepted. Actor/command/grant binding is the host wrapper's job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderReceipt {
    pub plan_revision: u64,
    pub plan_digest: [u8; 32],
    pub backend: BackendIdentity,
    pub region: Rect,
    pub target: OutputTarget,
    pub exactness: ExactnessLedger,
    pub chunks: u64,
    pub nodes_executed: u64,
    pub peak_working_bytes: u64,
}

/// Shared typed port vocabulary; backends keep richer internal errors and map at the port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendError {
    Canceled,
    DeadlineExceeded,
    StaleRevision {
        expected: u64,
        found: u64,
    },
    InvalidPlan(PlanError),
    Unsupported {
        operator: &'static str,
        exactness: Exactness,
    },
    UnresolvedSource(SourceKey),
    SourceMismatch(SourceKey),
    BudgetExceeded {
        requested: u64,
        limit: u64,
    },
    Unavailable(&'static str),
    DeviceLost {
        generation: u64,
    },
    Sink(SinkError),
    Overflow,
}
impl BackendError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::StaleRevision { .. } => "stale_revision",
            Self::InvalidPlan(e) => e.code(),
            Self::Unsupported { .. } => "unsupported_operator",
            Self::UnresolvedSource(_) => "unresolved_source",
            Self::SourceMismatch(_) => "source_mismatch",
            Self::BudgetExceeded { .. } => "budget_exceeded",
            Self::Unavailable(_) => "backend_unavailable",
            Self::DeviceLost { .. } => "device_lost",
            Self::Sink(SinkError::Rejected) => "sink_rejected",
            Self::Sink(SinkError::Unavailable) => "sink_unavailable",
            Self::Overflow => "overflow",
        }
    }
    pub const fn diagnostic(self) -> ResourceCode {
        match self {
            Self::Canceled => ResourceCode::Canceled,
            Self::DeadlineExceeded
            | Self::Unavailable(_)
            | Self::DeviceLost { .. }
            | Self::UnresolvedSource(_)
            | Self::Sink(_) => ResourceCode::Unavailable,
            Self::StaleRevision { .. } => ResourceCode::RevisionConflict,
            Self::Unsupported { .. } => ResourceCode::Unsupported,
            Self::SourceMismatch(_) => ResourceCode::HashMismatch,
            Self::BudgetExceeded { .. } => ResourceCode::BudgetExceeded,
            Self::Overflow => ResourceCode::Overflow,
            Self::InvalidPlan(_) => ResourceCode::Validation,
        }
    }
}

/// Backend port (STU-ARC-002: independently selectable `Send + Sync` providers). GPU API types
/// never appear here; a missing device is `Unavailable`, never a silent CPU fallback.
pub trait RenderBackend<V = NoVector>: Send + Sync {
    fn identity(&self) -> BackendIdentity;
    /// Capability query (STU-RAS-155); `Unsupported` = typed refusal at admission.
    fn exactness(&self, operator: Operator) -> Exactness;
    /// Does this backend implement the clip-to-base route ([`MaskFormat::BaseAlpha`])? Default
    /// `false`: [`admit`] then refuses any plan that uses it, so a backend written before the
    /// route existed cannot render a clipped layer unclipped.
    fn supports_clip_to_base(&self) -> bool {
        false
    }
    /// Capability query for envelope operators ([`PlanExtensions`]). Default: nothing supported,
    /// so [`admit_extended`] refuses any non-empty envelope for a backend written before it.
    fn extension_exactness(&self, operator: ExtOperator) -> Exactness {
        let _ = operator;
        Exactness::Unsupported
    }
    /// Execute a plan together with its envelope. Default: an empty envelope is the v1 plan
    /// ([`RenderBackend::execute`]); otherwise [`admit_extended`] runs and refuses for a backend
    /// that does not support the envelope's operators. A backend that reports support overrides
    /// this method; if it does not, the call is `Unavailable` rather than a render without the
    /// envelope (`tests/plan_extensions.rs` `envelope_admission_and_default_route`). This default
    /// admits with the default [`PlanLimits`] and never renders, so it has no limits to forward; a
    /// backend that renders overrides it and passes its own limits to
    /// [`admit_extended_with_limits`] (render-cpu: `Renderer::with_plan_limits`).
    fn execute_extended(
        &self,
        plan: &RenderPlan<V>,
        extensions: &PlanExtensions,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        if extensions.is_empty() {
            return self.execute(plan, request, sources, cancel, sink);
        }
        admit_extended(plan, extensions, self, request, cancel)?;
        Err(BackendError::Unavailable("execute_extended_not_implemented"))
    }
    /// Must call [`admit`] before any work; emits chunks to `sink`, receipt only on full success.
    fn execute(
        &self,
        plan: &RenderPlan<V>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError>;
}

/// Shared admission: cancel, stale revision, structural validation, then per-operator class =
/// intrinsic lowered by the backend's class; refuses Unsupported or below `min_exactness` before
/// any sink call, and refuses a pass-through layer with `fill_opacity < 1`
/// ([`PASS_THROUGH_FILL_OPERATOR`], intrinsically Unsupported) for every backend. A coverage
/// mask with `density < 1` has no v1 operator, so the v1 path refuses it here as
/// [`MASK_DENSITY_OPERATOR`] (the key both backends report); the envelope path
/// ([`admit_extended`]) admits it as `ExtOperator::MaskDensity` when the backend supports it.
/// Last, the plan-level parity rules of [`admit_plan_rules`] run (a mask on a pass-through layer,
/// the premultiplied alpha capability, the leaf-root Normal class), so a refusal that existed
/// before them keeps its key. Returns the ledger for the receipt.
///
/// Validates with the default (maximum) [`PlanLimits`]; a host that wants tighter budgets calls
/// [`admit_with_limits`], which is this function with the caller's limits.
pub fn admit<V>(
    plan: &RenderPlan<V>,
    backend: &(impl RenderBackend<V> + ?Sized),
    request: &ExecuteRequest,
    cancel: &CancellationToken,
) -> Result<ExactnessLedger, BackendError> {
    admit_with_limits(plan, &PlanLimits::default(), backend, request, cancel)
}

/// [`admit`] with caller-supplied [`PlanLimits`]: every step and refusal key is the same, but
/// [`RenderPlan::validate`] enforces `limits` instead of the maxima, so a host can refuse a plan
/// that the defaults admit (more nodes, more layers or a deeper nesting than it will render):
/// `BackendError::InvalidPlan(PlanError::BudgetExceeded)` for nodes and layers,
/// `PlanError::TooDeep` for depth, before any source lookup or sink call. Limits can only tighten:
/// a field above [`PlanLimits::MAXIMUM`] is refused as
/// `BackendError::InvalidPlan(PlanError::LimitAboveMaximum)`, after the cancel and stale-revision
/// checks and before the plan is read, never clamped (test `tests/plan_limits.rs`
/// `tightened_limits_refuse_what_the_defaults_admit` and
/// `loosening_past_the_maxima_is_a_typed_error_everywhere`). `PlanLimits::default()` gives exactly
/// [`admit`] (test `defaults_are_the_maxima_and_unchanged`).
pub fn admit_with_limits<V>(
    plan: &RenderPlan<V>,
    limits: &PlanLimits,
    backend: &(impl RenderBackend<V> + ?Sized),
    request: &ExecuteRequest,
    cancel: &CancellationToken,
) -> Result<ExactnessLedger, BackendError> {
    let mut ledger = admit_core(plan, limits, backend, request, cancel, SampleRange::Unit)?;
    if plan.has_mask_density() {
        return Err(BackendError::Unsupported {
            operator: MASK_DENSITY_OPERATOR,
            exactness: Exactness::Unsupported,
        });
    }
    admit_plan_rules(plan, backend, request, &mut ledger)?;
    Ok(ledger)
}

/// Plan-level admission rules added by lane SMB-PIGMENT-ADMIT-PARITY (R5/R7 class "admitted, then
/// refused by every backend after admission"; decision: work-folder
/// `diagnostics/adjustments-advanced-blending.md` topic `admission-parity`). They run after every
/// other check of [`admit`] and [`admit_extended`], so a plan refused before these rules existed is
/// refused with the same key, and only a plan that was admitted and then refused locally by the
/// backends changes outcome (to the same key, before any source lookup):
///
/// 1. a coverage mask on a pass-through layer of a reachable group is intrinsically Unsupported
///    ([`PASS_THROUGH_MASK_OPERATOR`], see [`RenderPlan::has_pass_through_mask`]);
/// 2. a premultiplied alpha convention (the target, or a tile input reachable from the root) is a
///    capability each backend declares through [`RenderBackend::extension_exactness`]
///    ([`ExtOperator::AlphaPremultiplied`]); undeclared means Unsupported, which is what both
///    backends do today;
/// 3. a leaf root is composited as a Normal layer, so the backend's class for
///    `Operator::Blend(Normal)` must pass the request floor (checked only: the ledger is not
///    changed, so a receipt is byte-identical to the one before this rule).
///
/// Tests: `tests/admit_parity.rs`.
pub(crate) fn admit_plan_rules<V, B: RenderBackend<V> + ?Sized>(
    plan: &RenderPlan<V>,
    backend: &B,
    request: &ExecuteRequest,
    ledger: &mut ExactnessLedger,
) -> Result<(), BackendError> {
    let refuse = |operator: &'static str, exactness: Exactness| {
        Err(BackendError::Unsupported {
            operator,
            exactness,
        })
    };
    if plan.has_pass_through_mask() {
        return refuse(PASS_THROUGH_MASK_OPERATOR, Exactness::Unsupported);
    }
    if plan.uses_premultiplied_alpha() {
        let op = ExtOperator::AlphaPremultiplied;
        let class = op.intrinsic().lowest_of(backend.extension_exactness(op));
        if class == Exactness::Unsupported || class.rank() > request.min_exactness.rank() {
            return refuse(op.key(), class);
        }
        ledger.record(op.key(), class);
    }
    if plan.has_leaf_root() {
        let op = Operator::Blend(StudioBlendMode::Normal);
        let class = op.intrinsic().lowest_of(backend.exactness(op));
        if class == Exactness::Unsupported || class.rank() > request.min_exactness.rank() {
            return refuse(op.key(), class);
        }
    }
    Ok(())
}

/// Operator key of a coverage mask with `density < 1` (v1 refusal and envelope operator).
pub const MASK_DENSITY_OPERATOR: &str = "mask_density";
/// Operator key of colour samples outside `[0, 1]` (the key render-cpu and render-gpu report);
/// in a unit-range plan (the default) a `Solid` colour outside the unit range is refused by
/// [`admit`] for every backend (R5 PLAUSIBLE item, same failure class); an extended-range output
/// stage ([`OutputStage`], lane SMB-PIGMENT-EXTENDED-RANGE) admits any finite colour.
pub const SAMPLE_RANGE_OPERATOR: &str = "sample_range_outside_unit";
impl<V> RenderPlan<V> {
    /// Does any `Solid` reachable from the root carry a colour channel outside `[0, 1]`? (Alpha
    /// range and finiteness are structural and already refused by [`RenderPlan::validate`].) A
    /// `Solid` no layer reaches is never read by a backend, so it asks for nothing (test
    /// `tests/admit_parity.rs` `unreachable_solid_outside_unit_is_not_counted_like_the_backends`).
    pub fn has_solid_outside_unit(&self) -> bool {
        let reachable = self.reachable_nodes();
        self.nodes.iter().enumerate().any(|(i, n)| match n {
            PlanNode::Solid { rgba, .. } if reachable[i] => {
                rgba[..3].iter().any(|c| !(0.0..=1.0).contains(c))
            }
            _ => false,
        })
    }
    /// Does any group layer of a node reachable from the root carry a coverage mask (not
    /// clip-to-base) with `density < 1`? Unreachable groups do not count (test
    /// `tests/admit_parity.rs` `unreachable_mask_density_is_not_counted_like_the_backends`).
    pub fn has_mask_density(&self) -> bool {
        let reachable = self.reachable_nodes();
        self.nodes.iter().enumerate().any(|(i, n)| match n {
            PlanNode::Group(g) if reachable[i] => g.layers.iter().any(|l| {
                l.mask
                    .as_ref()
                    .is_some_and(|m| m.format != MaskFormat::BaseAlpha && m.density < 1.0)
            }),
            _ => false,
        })
    }
}

/// [`admit`] without the v1-only mask-density refusal (shared with [`admit_extended`]). `range`
/// is the plan's output-stage sample range: the `Solid` outside-unit refusal applies to
/// [`SampleRange::Unit`] only (an extended plan admits any finite solid colour, finiteness being
/// structural in [`RenderPlan::validate`]).
pub(crate) fn admit_core<V, B: RenderBackend<V> + ?Sized>(
    plan: &RenderPlan<V>,
    limits: &PlanLimits,
    backend: &B,
    request: &ExecuteRequest,
    cancel: &CancellationToken,
    range: SampleRange,
) -> Result<ExactnessLedger, BackendError> {
    cancel.check().map_err(|_| BackendError::Canceled)?;
    if plan.revision != request.expected_revision {
        return Err(BackendError::StaleRevision {
            expected: request.expected_revision,
            found: plan.revision,
        });
    }
    let operators = plan.validate(limits).map_err(BackendError::InvalidPlan)?;
    if operators.contains(&Operator::Mask(MaskFormat::BaseAlpha)) && !backend.supports_clip_to_base()
    {
        return Err(BackendError::Unsupported {
            operator: MaskFormat::BaseAlpha.key(),
            exactness: Exactness::Unsupported,
        });
    }
    let mut ledger = ExactnessLedger::default();
    for op in operators {
        let class = op.intrinsic().lowest_of(backend.exactness(op));
        if class == Exactness::Unsupported || class.rank() > request.min_exactness.rank() {
            return Err(BackendError::Unsupported {
                operator: op.key(),
                exactness: class,
            });
        }
        ledger.record(op.key(), class);
    }
    if plan.has_pass_through_fill() {
        return Err(BackendError::Unsupported {
            operator: PASS_THROUGH_FILL_OPERATOR,
            exactness: Exactness::Unsupported,
        });
    }
    if range == SampleRange::Unit && plan.has_solid_outside_unit() {
        return Err(BackendError::Unsupported {
            operator: SAMPLE_RANGE_OPERATOR,
            exactness: Exactness::Unsupported,
        });
    }
    Ok(ledger)
}

pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"render_plan","version":1,"role":"shared static render-plan IR + backend port; composite builds, render-cpu/render-gpu implement RenderBackend","nodes":["tile","solid","vector(V)","group{isolated,layers bottom-first}"],"layer":["input","mode StudioBlendMode 1..=35","opacity","fill_opacity","mask"],"space":"ColourTag{profile_sha256, blend_space linear_light|encoded}; never converted by a backend","alpha":"straight|premultiplied declared","exactness":["exact","fitted","approximate","unsupported"],"admission":"cancel, stale revision, validate, per-operator min(intrinsic, backend) vs min_exactness; refusal before any sink call; admit_with_limits and admit_extended_with_limits take caller PlanLimits (admit and admit_extended use the defaults)","validate":"two parts: structure (limits, indexes, forward references, rects, ranges, mode applicability, clipping-run rules) is checked over every node, reachable or not; the operators a backend must support are collected over the nodes reachable from the root only, like the admission predicates and both backends, so an unreachable node with an operator a backend refuses admits and the same node reached from the root is refused with the same key (tests plan_reachability unreachable_unsupported_operator_admits_reachable_one_is_refused, structure_stays_checked_on_unreachable_nodes); the envelope follows the same rule: PlanExtensions::validate checks the structure of every entry and collects the operators (adjustment, Blend-If, feather, effect, effects_nested) of entries addressed to a group reachable from the root only, so an entry on an unreachable group asks for nothing (test plan_followups unreachable_envelope_entry_asks_for_nothing_reachable_one_is_refused); a knockout, an adjustment kind or an effect feature that pigment itself refuses is a typed error on every entry, reachable or not, because no backend could run it (test plan_followups intrinsic_envelope_refusals_stay_on_every_entry)","plan_limits":"PlanLimits{max_nodes, max_layers, max_depth}; PlanLimits::MAXIMUM (65536, 65536, 64) is Default and what admit uses; a caller may only tighten: admit_with_limits and admit_extended_with_limits refuse a plan above the tightened limit as plan_budget_exceeded (nodes, layers) or plan_too_deep (depth) before any sink call, and a limit above MAXIMUM is refused as plan_limit_above_maximum (PlanError::LimitAboveMaximum, never clamped) by admit_with_limits, admit_extended_with_limits and RenderPlan::validate; zero tightens: max_nodes 0 refuses every plan, max_layers 0 every plan with a layer, and max_depth 0 every plan, because the depth budget is compared for every node (a leaf or a group without layers is depth 1; test plan_followups depth_limit_applies_to_every_plan_shape); the envelope budgets (effects per layer and per plan, effect size) are fixed constants outside PlanLimits (tests plan_limits tightened_limits_refuse_what_the_defaults_admit, loosening_past_the_maxima_is_a_typed_error_everywhere, defaults_are_the_maxima_and_unchanged)","receipt":["plan_revision","plan_digest","backend","region","target","exactness","chunks","nodes_executed","peak_working_bytes"],"persistence":"none; Serialize only for digest","clip_to_base":"Layer.mask = MaskInput::clip_to_base() (MaskFormat::BaseAlpha): coverage is the alpha of the nearest earlier unclipped layer of the group; validate enforces a base, canonical fields, no pass-through reference, no pass-through base; Approximate (Photoshop rule UNVERIFIED); backends opt in via supports_clip_to_base, admit refuses otherwise","pass_through_fill":"a pass-through layer with fill_opacity < 1 in a group reachable from the root is intrinsically unsupported: admit refuses it for every backend as group_pass_through_fill_opacity (validate no longer rates it fill_opacity; the backends skip unreachable nodes, so an unreachable one is not counted; tests plan_extensions pass_through_fill_is_refused_uniformly_by_admit, admit_parity unreachable_pass_through_fill_is_not_counted_like_the_backends)","mask_density":"v1 admit refuses a coverage mask with density < 1 in a group reachable from the root as mask_density for every backend; admit_extended admits it as ExtOperator::MaskDensity when the backend supports it; an unreachable group is not counted (tests plan_extensions mask_density_is_refused_by_v1_admit_and_admitted_through_the_envelope, admit_parity unreachable_mask_density_is_not_counted_like_the_backends)","output_stage":"lane SMB-PIGMENT-EXTENDED-RANGE: PlanExtensions.output = OutputStage{sample_range Unit (default) | Extended, display None | Some(DisplayTransform::Clip)}, in the envelope because OutputTarget is a literal-built v1 type; the default stage is not serialised, so digest_with of every existing plan and envelope is unchanged, and each non-default field changes it (test extended_range plan_digest_covers_the_output_stage_and_the_default_is_absent); Extended admits any finite tile and Solid colour (alpha stays [0,1]) and composites with blend_math::composite_over_extended (W3C source-over with blend, binary32, no unit clamp; a non-finite result is OutOfDomain) over the modes blend_math::extended_defined (normal, dissolve, multiply, darken, lighten, difference, subtract, linear dodge as unclamped a+b, darker colour, lighter colour: formulas without a unit white point, 1 or 0.5 threshold; lane SMB-PIGMENT-EXTENDED-RANGE-2 adds screen = Foundry Nuke Merge 'A or B <= 1 ? A+B-AB : max(A,B)' and exclusion = 'A+B-2AB', equal to the unit formulas on [0,1]); ExtOperator::ExtendedRange key sample_range_extended class approximate (Photoshop 32-bit parity UNVERIFIED), declared by each backend (default unsupported); a reachable layer or layer effect with another mode is ExtOperator::ExtendedRangeMode(mode), key sample_range_extended_mode_<mode key> (blend_math::extended_mode_key), intrinsically unsupported for every backend (STU-COL-120, STU-RAS-155 (b)); each envelope operation of a reachable entry is ExtOperator::ExtendedEnvelope(ExtendedOp) with key sample_range_extended_<operation> and clause EXTENDED_RANGE_CLAUSE (STU-COL-120; STU-FX-006a): defined (class approximate, backend-declared) exposure on linear-light values, layer effects, gaussian blur, unsharp mask, median, layer/backdrop blur, transform, content-aware fill without colour adaptation, computed by the *_in(SampleRange) forms of adjust_math, effect_math::window, filter_math, blur_math, transform_math and content_aware (unclamped, non-finite results typed OutOfDomain, the Unit forms are the unchanged functions); undefined (intrinsically unsupported) levels, curves, hue/saturation, brightness/contrast, invert, threshold, posterize, exposure on encoded values, Blend-If, high pass, add noise, content-aware colour adaptation (tests extended_range_2 extended_modes_and_envelope_operations_follow_the_scene_referred_rule, extended_envelope_maths_is_unclamped_and_unit_forms_are_unchanged); ExtendedRangeCombination (sample_range_extended_combination) is retired and never reported; Clip is ExtOperator::DisplayClip key display_clip class exact, stage 6a of STU-COL-161: the composite colour clamped into [0,1] before the sink (test extended_range extended_over_matches_the_w3c_formula_and_undefined_modes_are_refused)","sample_range":"a Solid colour channel outside [0,1] on a node reachable from the root is refused by shared admission as sample_range_outside_unit unless the output stage is Extended (output_stage; an unreachable Solid is never read by a backend and is not counted; tests plan_extensions solid_outside_unit_is_refused_by_shared_admission, admit_parity unreachable_solid_outside_unit_is_not_counted_like_the_backends)","blend_if_clip":"no public source defines Blend-If on a clip base or on a clipped layer, so a non-identity Blend-If there is intrinsically unsupported: admit_extended refuses it for every backend as blend_if_clip_base | blend_if_clipped_layer (ExtOperator::BlendIfClipBase | BlendIfClippedLayer, class unsupported even for a backend claiming every operator Exact); identity ranges and Blend-If on unclipped layers are unchanged (test blend_if_clip clip_roles_refuse_blend_if_uniformly_for_every_backend)","effects_ops":"effects together with a non-identity Blend-If on one layer have no public composition rule, so admit_extended refuses the layer for every backend as effects_with_blend_if (ExtOperator::EffectsWithBlendIf, intrinsically unsupported even for a backend claiming every operator Exact; reported instead of blend_if); a layer with effects whose content holds another layer with effects is reported as effects_nested (ExtOperator::EffectsNested, class approximate like the effects), which each backend declares through extension_exactness and admission refuses for a backend reporting it unsupported (tests/effects_ops.rs)","admission_parity":"plan-level rules run last in admit and admit_extended (admit_plan_rules), so earlier refusals keep their keys: a coverage mask on a pass-through layer of a group reachable from the root is intrinsically unsupported for every backend as mask_on_pass_through (no group result to weigh; the backends skip unreachable nodes, so an unreachable one is not counted; tests/admit_parity.rs pass_through_mask_is_refused_for_every_backend, unreachable_nodes_are_not_counted_like_the_backends); a premultiplied alpha convention at the target or at a tile input reachable from the root is the declarable ExtOperator::AlphaPremultiplied (key alpha_premultiplied, intrinsic approximate, undeclared = unsupported; tests/admit_parity.rs premultiplied_alpha_is_a_declared_capability, unreachable_nodes_are_not_counted_like_the_backends); a leaf root is checked against the backend class of Blend(Normal) at admission (ledger unchanged; tests/admit_parity.rs leaf_root_normal_class_is_decided_at_admission)","extensions":"PlanExtensions envelope next to the unchanged v1 plan (v1 types are literal-built/exhaustively matched by dependents): per (group, layer) adjustment layer (AdjustmentOp on a transparent Solid placeholder; colour lerp(Cb, B(Cb, f(Cb)), k), alpha kept), Blend-If entries (coverage_math), mask feather px, knockout (anything but none refused); ExtOperator classes; admit_extended; RenderBackend::extension_exactness (default unsupported) and execute_extended (default: empty envelope = execute, else refuse); digest_with","effects":"LayerExtension.effects: Vec<effect_math::EffectOp> (drop shadow and outer glow behind the content; colour overlay, solid stroke, inner shadow and inner glow over it; list order within each placement; Approximate, Photoshop parity NOT_PROVEN, D5 cases 12-17); glow gradient, glow range other than 100 %, glow jitter and the Precise glow technique are refused as glow_gradient|glow_range|glow_jitter|glow_technique_precise (tests/effects_wave2.rs glow_refusals_envelope_admission_and_budget); refused on adjustment, pass-through and clipping-run layers and above the size/count limits (tests/effects.rs effect_limits_validation_and_budget); backends without support are refused as the effect key before any sink call, and an effect blend mode the backend refuses is refused as that mode (tests/effects.rs effect_without_backend_support_is_refused_before_the_sink)","transform":"LayerExtension.transform: Option<transform_math::LayerTransform{affine Affine2 (row-major 2x3 f64, source plan coords -> destination plan coords), filter nearest_neighbour|bilinear|bicubic (STU-RAS-141 values 2|3|4; bicubic = Catmull-Rom, Keys a=-0.5), edge transparent|clamp, transform_mask}> on a layer whose input is a Tile; reference = transform_math::PreparedTransform::sample_rgba / sample_coverage / resample_reference in binary64 (destination centre sampled through the inverse, exact texel on a texel centre, premultiplied colour, alpha min(A,1), colour clamp(P/A,0,1), one binary32 rounding); validate refuses non-finite, singular, |linear| or |inverse linear| above 1024, |translation| above 2^24, destination bounds above 2^20 per side (plan_transform_matrix), a non-tile input (plan_transform_target), effects on the same layer or a transformed mask with feather or without a coverage mask (plan_transform_combination), more than 4096 transformed layers or 2^36 destination pixels per envelope (plan_transform_limit); ExtOperator::Transform(filter) keys transform_nearest_neighbour|transform_bilinear|transform_bicubic, class approximate (Photoshop/After Effects kernels UNVERIFIED), declared by each backend through extension_exactness (tests/transform.rs)","filters":"LayerExtension.filters: Vec<filter_math::LayerFilter> (gaussian_blur, unsharp_mask, high_pass, median, add_noise with seed; <= 16, list order, on the layer content); validate refuses parameters out of range or too many (plan_filter_parameter) and filters with an adjustment, effects, transform or blur on the same layer, a pass-through layer or a clip role (plan_filter_combination); ExtOperator::Filter(kind) keys filter_gaussian_blur|filter_unsharp_mask|filter_high_pass|filter_median|filter_add_noise, class approximate, declared by each backend through extension_exactness (tests/retouch.rs filters::filter_envelope_entries_validate_report_and_admission_refuses_undeclared)","content_aware_fill":"LayerExtension.content_aware_fill: Option<ContentAwareFillOp{hole MaskInput (coverage > 0, not clip-to-base, density 1), sampling None = auto | Some(rect), options content_aware::ContentAwareOptions (threads not digested)}>, the native content_aware::fill of the layer content (STU-RAS-143); validate refuses bad options or hole/sampling (plan_content_aware_parameter) and adjustment, effects, transform, blur or filters on the same layer, a pass-through layer or a clip role (plan_content_aware_combination); ExtOperator::ContentAwareFill key content_aware_fill, class approximate; no backend declares it yet (render-cpu and render-gpu answer unsupported for unlisted operators), so admit_extended refuses it before any sink call (tests/content_aware_plan.rs)","pending":["STU-CMP-020 members without discriminant","Blend-If on a clip base or a clipped layer (refused until D5 cases 18-23 define it)","STU-CMP-020a stencil/silhouette node kinds","knockout shallow/deep, the other layer effects (satin, gradient/pattern overlay, bevel, gradient glows), mattes, time","clipped layer with its own mask","backend implementations of the envelope operators"],"blend_math":"pigment::blend_math owns StudioBlendMode facts, exactness classes and the scalar reference blend/source-over/group oracle"}"#;

#[cfg(test)]
mod admission_decisions {
    use super::*;

    /// Compile-time tripwire (lane SMB-PIGMENT-ADMIT-PARITY): these enums are matched exhaustively
    /// here, inside the defining crate where `#[non_exhaustive]` does not apply, so a new variant
    /// fails the build until its admission decision is written next to the existing ones. The
    /// backends refuse a format or alpha convention they do not know after admission under local
    /// keys (`sample_format`, `alpha_premultiplied`, and for a mask format `Operator::Mask(format)`'s
    /// key); the decision belongs in `admit_plan_rules`, or in the backend's `exactness` for the
    /// operator (render-cpu names the mask formats it reads and answers Unsupported for any other).
    #[test]
    fn every_enum_a_backend_refuses_on_has_an_admission_decision() {
        for format in [SampleFormat::RgbF32Le] {
            match format {
                // The only format: no refusal to decide.
                SampleFormat::RgbF32Le => assert_eq!(format.key(), "rgb_f32le"),
            }
        }
        for format in [
            MaskFormat::CoverageU8,
            MaskFormat::CoverageU16Le,
            MaskFormat::BaseAlpha,
        ] {
            match format {
                // The coverage planes: a backend that reads them answers Exact for the operator
                // under the format's own key; one that does not read a format is refused at
                // admission through `exactness` (R8-review-a, render-cpu `Mask(_) => Exact`).
                MaskFormat::CoverageU8 | MaskFormat::CoverageU16Le => {
                    assert_eq!(Operator::Mask(format).key(), format.key());
                }
                // The clip route: `supports_clip_to_base` in `admit_core`, `validate` for the base.
                MaskFormat::BaseAlpha => assert_eq!(Operator::Mask(format).key(), "clip_to_base"),
            }
        }
        for alpha in [AlphaConvention::Straight, AlphaConvention::Premultiplied] {
            match alpha {
                AlphaConvention::Straight => {}
                // Declared capability, decided by `admit_plan_rules`.
                AlphaConvention::Premultiplied => {
                    assert_eq!(ExtOperator::AlphaPremultiplied.key(), "alpha_premultiplied");
                }
            }
        }
    }
}
