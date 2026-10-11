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
}
impl MaskFormat {
    pub const fn key(self) -> &'static str {
        match self {
            Self::CoverageU8 => "coverage_u8",
            Self::CoverageU16Le => "coverage_u16le",
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
                    for l in &g.layers {
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
                        if l.fill_opacity < 1.0 && l.mode != StudioBlendMode::Normal {
                            add(Operator::FillOpacity);
                        }
                        if let Some(m) = &l.mask {
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
/// any sink call. Returns the ledger for the receipt.
pub fn admit<V>(
    plan: &RenderPlan<V>,
    backend: &(impl RenderBackend<V> + ?Sized),
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
    Ok(ledger)
}

pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"render_plan","version":1,"role":"shared static render-plan IR + backend port; composite builds, render-cpu/render-gpu implement RenderBackend","nodes":["tile","solid","vector(V)","group{isolated,layers bottom-first}"],"layer":["input","mode StudioBlendMode 1..=35","opacity","fill_opacity","mask"],"space":"ColourTag{profile_sha256, blend_space linear_light|encoded}; never converted by a backend","alpha":"straight|premultiplied declared","exactness":["exact","fitted","approximate","unsupported"],"admission":"cancel, stale revision, validate, per-operator min(intrinsic, backend) vs min_exactness; refusal before any sink call","receipt":["plan_revision","plan_digest","backend","region","target","exactness","chunks","nodes_executed","peak_working_bytes"],"persistence":"none; Serialize only for digest","pending":["STU-CMP-020 members without discriminant","STU-CMP-020a stencil/silhouette node kinds","clipping, knockout, blend-if, effects, mattes, time"],"blend_math":"pigment::blend_math owns StudioBlendMode facts, exactness classes and the scalar reference blend/source-over/group oracle"}"#;
