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
pub use extensions::{ExtOperator, Knockout, LayerExtension, PlanExtensions, admit_extended};

/// Blending profile + linearity every plan input must already be in (STU-COL-162/163).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct ColourTag {
    pub profile_sha256: [u8; 32],
    pub blend_space: BlendSpace,
}
/// STU-CMP-022: alpha convention is explicit at every boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum AlphaConvention {
    Straight,
    Premultiplied,
}
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanLimits {
    pub max_nodes: usize,
    pub max_layers: usize,
    pub max_depth: usize,
}
impl Default for PlanLimits {
    fn default() -> Self {
        Self {
            max_nodes: 65_536,
            max_layers: 65_536,
            max_depth: 64,
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
    /// Structural validation shared by every backend. Returns the distinct operators in first-use
    /// order. Unspecified modes pass here and are refused by [`admit`] as Unsupported.
    pub fn validate(&self, limits: &PlanLimits) -> Result<Vec<Operator>, PlanError> {
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
        let mut ops: Vec<Operator> = Vec::new();
        let mut add = |op: Operator| {
            if !ops.contains(&op) {
                ops.push(op);
            }
        };
        let mut depth = vec![1usize; self.nodes.len()];
        let mut layers = 0usize;
        for (i, node) in self.nodes.iter().enumerate() {
            let n = i as u32;
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
                            let Some(base) = g.layers[..li]
                                .iter()
                                .rposition(|b| !b.mask.as_ref().is_some_and(MaskInput::is_clip_to_base))
                            else {
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
                    }
                }
            }
        }
        Ok(ops)
    }
}
/// Operator key of the shared refusal of a pass-through layer with `fill_opacity < 1` (the key
/// render-cpu already reports; one refusal for every backend, made by [`admit`]).
pub const PASS_THROUGH_FILL_OPERATOR: &str = "group_pass_through_fill_opacity";
impl<V> RenderPlan<V> {
    /// Does any group layer composite a pass-through group with `fill_opacity < 1`? Fill on a
    /// non-isolated group is undefined (no group result to scale, like opacity), so it is
    /// intrinsically Unsupported and [`admit`] refuses it for every backend.
    pub fn has_pass_through_fill(&self) -> bool {
        self.nodes.iter().any(|n| match n {
            PlanNode::Group(g) => g
                .layers
                .iter()
                .any(|l| l.mode == StudioBlendMode::PassThrough && l.fill_opacity < 1.0),
            _ => false,
        })
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
    /// envelope (`tests/plan_extensions.rs` `envelope_admission_and_default_route`).
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
/// Returns the ledger for the receipt.
pub fn admit<V>(
    plan: &RenderPlan<V>,
    backend: &(impl RenderBackend<V> + ?Sized),
    request: &ExecuteRequest,
    cancel: &CancellationToken,
) -> Result<ExactnessLedger, BackendError> {
    let ledger = admit_core(plan, backend, request, cancel)?;
    if plan.has_mask_density() {
        return Err(BackendError::Unsupported {
            operator: MASK_DENSITY_OPERATOR,
            exactness: Exactness::Unsupported,
        });
    }
    Ok(ledger)
}

/// Operator key of a coverage mask with `density < 1` (v1 refusal and envelope operator).
pub const MASK_DENSITY_OPERATOR: &str = "mask_density";
/// Operator key of colour samples outside `[0, 1]` (the key render-cpu and render-gpu report);
/// the IR has no HDR operator class yet, so a `Solid` colour outside the unit range is refused by
/// [`admit`] for every backend (R5 PLAUSIBLE item, same failure class).
pub const SAMPLE_RANGE_OPERATOR: &str = "sample_range_outside_unit";
impl<V> RenderPlan<V> {
    /// Does any `Solid` carry a colour channel outside `[0, 1]`? (Alpha range and finiteness are
    /// structural and already refused by [`RenderPlan::validate`].)
    pub fn has_solid_outside_unit(&self) -> bool {
        self.nodes.iter().any(|n| match n {
            PlanNode::Solid { rgba, .. } => rgba[..3].iter().any(|c| !(0.0..=1.0).contains(c)),
            _ => false,
        })
    }
    /// Does any group layer carry a coverage mask (not clip-to-base) with `density < 1`?
    pub fn has_mask_density(&self) -> bool {
        self.nodes.iter().any(|n| match n {
            PlanNode::Group(g) => g.layers.iter().any(|l| {
                l.mask
                    .as_ref()
                    .is_some_and(|m| m.format != MaskFormat::BaseAlpha && m.density < 1.0)
            }),
            _ => false,
        })
    }
}

/// [`admit`] without the v1-only mask-density refusal (shared with [`admit_extended`]).
pub(crate) fn admit_core<V, B: RenderBackend<V> + ?Sized>(
    plan: &RenderPlan<V>,
    backend: &B,
    request: &ExecuteRequest,
    cancel: &CancellationToken,
) -> Result<ExactnessLedger, BackendError> {
    cancel.check().map_err(|_| BackendError::Canceled)?;
    if plan.revision != request.expected_revision {
        return Err(BackendError::StaleRevision {
            expected: request.expected_revision,
            found: plan.revision,
        });
    }
    let operators = plan
        .validate(&PlanLimits::default())
        .map_err(BackendError::InvalidPlan)?;
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
    if plan.has_solid_outside_unit() {
        return Err(BackendError::Unsupported {
            operator: SAMPLE_RANGE_OPERATOR,
            exactness: Exactness::Unsupported,
        });
    }
    Ok(ledger)
}

pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"render_plan","version":1,"role":"shared static render-plan IR + backend port; composite builds, render-cpu/render-gpu implement RenderBackend","nodes":["tile","solid","vector(V)","group{isolated,layers bottom-first}"],"layer":["input","mode StudioBlendMode 1..=35","opacity","fill_opacity","mask"],"space":"ColourTag{profile_sha256, blend_space linear_light|encoded}; never converted by a backend","alpha":"straight|premultiplied declared","exactness":["exact","fitted","approximate","unsupported"],"admission":"cancel, stale revision, validate, per-operator min(intrinsic, backend) vs min_exactness; refusal before any sink call","receipt":["plan_revision","plan_digest","backend","region","target","exactness","chunks","nodes_executed","peak_working_bytes"],"persistence":"none; Serialize only for digest","clip_to_base":"Layer.mask = MaskInput::clip_to_base() (MaskFormat::BaseAlpha): coverage is the alpha of the nearest earlier unclipped layer of the group; validate enforces a base, canonical fields, no pass-through reference, no pass-through base; Approximate (Photoshop rule UNVERIFIED); backends opt in via supports_clip_to_base, admit refuses otherwise","pass_through_fill":"a pass-through layer with fill_opacity < 1 is intrinsically unsupported: admit refuses it for every backend as group_pass_through_fill_opacity (validate no longer rates it fill_opacity; test plan_extensions pass_through_fill_is_refused_uniformly_by_admit)","mask_density":"v1 admit refuses a coverage mask with density < 1 as mask_density for every backend; admit_extended admits it as ExtOperator::MaskDensity when the backend supports it (test plan_extensions mask_density_is_refused_by_v1_admit_and_admitted_through_the_envelope)","sample_range":"a Solid colour channel outside [0,1] is refused by shared admission as sample_range_outside_unit (no HDR operator class yet; test plan_extensions solid_outside_unit_is_refused_by_shared_admission)","extensions":"PlanExtensions envelope next to the unchanged v1 plan (v1 types are literal-built/exhaustively matched by dependents): per (group, layer) adjustment layer (AdjustmentOp on a transparent Solid placeholder; colour lerp(Cb, B(Cb, f(Cb)), k), alpha kept), Blend-If entries (coverage_math), mask feather px, knockout (anything but none refused); ExtOperator classes; admit_extended; RenderBackend::extension_exactness (default unsupported) and execute_extended (default: empty envelope = execute, else refuse); digest_with","effects":"LayerExtension.effects: Vec<effect_math::EffectOp> (drop shadow and outer glow behind the content; colour overlay, solid stroke, inner shadow and inner glow over it; list order within each placement; Approximate, Photoshop parity NOT_PROVEN, D5 cases 12-17); glow gradient, glow range other than 100 %, glow jitter and the Precise glow technique are refused as glow_gradient|glow_range|glow_jitter|glow_technique_precise (tests/effects_wave2.rs glow_refusals_envelope_admission_and_budget); refused on adjustment, pass-through and clipping-run layers and above the size/count limits (tests/effects.rs effect_limits_validation_and_budget); backends without support are refused as the effect key before any sink call, and an effect blend mode the backend refuses is refused as that mode (tests/effects.rs effect_without_backend_support_is_refused_before_the_sink)","pending":["STU-CMP-020 members without discriminant","STU-CMP-020a stencil/silhouette node kinds","knockout shallow/deep, the other layer effects (satin, gradient/pattern overlay, bevel, gradient glows), mattes, time","clipped layer with its own mask","backend implementations of the envelope operators"],"blend_math":"pigment::blend_math owns StudioBlendMode facts, exactness classes and the scalar reference blend/source-over/group oracle"}"#;
