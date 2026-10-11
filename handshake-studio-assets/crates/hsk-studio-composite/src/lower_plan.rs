//! Lower a Folio authored layer/operation graph into a disposable, revision-bound plan
//! (STU-CMP-092/093). The plan keeps the AUTHORED ids (never reminted) and the authored
//! `input_ordinal` order; it is plain data with no Folio types and is never persisted or
//! treated as authoring authority.
//!
//! Order convention (SPEC GAP, UNVERIFIED against Photoshop): `input_ordinal` 0 is composited
//! first, i.e. it is the bottom-most input.
use crate::CompositeError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    OperationKind, PortAddress, ProfileBinding, Snapshot, StudioLayer, ValueType,
};
use hsk_studio_pigment::blend_math::{BlendSpace, Exactness, StudioBlendMode};
use hsk_studio_pigment::render_plan::{MaskFormat, Operator};
use std::collections::{BTreeMap, BTreeSet};

/// Authored address of one output port (`layer_id`, `operation_key`, `port_key`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeAddr {
    pub layer_id: String,
    pub operation_key: String,
    pub port_key: String,
}

impl NodeAddr {
    pub fn text(&self) -> String {
        format!("{}/{}/{}", self.layer_id, self.operation_key, self.port_key)
    }
}

impl From<&PortAddress> for NodeAddr {
    fn from(a: &PortAddress) -> Self {
        Self {
            layer_id: a.layer_id.clone(),
            operation_key: a.operation_key.clone(),
            port_key: a.port_key.clone(),
        }
    }
}

/// Lowering budgets. Exceeding either is `BudgetExceeded`, never a truncated plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_steps: usize,
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_steps: 4096,
            max_depth: 64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceValue {
    Image,
    Vector,
    Text,
}

/// Authored appearance of one layer as lowered (STU-RAS-006): Folio stores opacity and fill
/// opacity as percent, the plan holds fractions. An absent Folio appearance lowers to the kind's
/// identity (group = pass_through, other = normal, 100 percent, not clipped, no mask).
/// `mask` means an ENABLED mask is present; its coverage tiles stay in the snapshot. `linked` is an
/// authoring property (the mask follows the layer when edited) and does not change rendering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepAppearance {
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub fill_opacity: f32,
    pub clipped: bool,
    pub mask: bool,
}

impl StepAppearance {
    pub const fn identity(group: bool) -> Self {
        Self {
            mode: if group {
                StudioBlendMode::PassThrough
            } else {
                StudioBlendMode::Normal
            },
            opacity: 1.0,
            fill_opacity: 1.0,
            clipped: false,
            mask: false,
        }
    }
    /// Does this appearance change nothing when rendered on its own over a transparent backdrop?
    /// (The blend mode is irrelevant there: W3C `ab = 0` gives `Cs' = Cs`.)
    pub fn is_neutral_over_transparent(&self) -> bool {
        self.opacity >= 1.0 && self.fill_opacity >= 1.0 && !self.mask && !self.clipped
    }
    /// Lowest class over everything this appearance uses, read from pigment's `Operator`
    /// accounting (`intrinsic()`): fill opacity with a non-Normal layer mode, pass-through below
    /// full opacity and clipping (pigment's clip-to-base route, Photoshop rule UNVERIFIED) carry
    /// pigment's class. A pass-through layer with fill below 1 has no group result for fill to
    /// scale: pigment's shared admission refuses it for every backend
    /// (`PASS_THROUGH_FILL_OPERATOR`), so it is `Unsupported` here too.
    pub fn exactness(&self) -> Exactness {
        let mut lowest = self.mode.facts().exactness;
        if self.mode == StudioBlendMode::PassThrough {
            if self.fill_opacity < 1.0 {
                lowest = lowest.lowest_of(Exactness::Unsupported);
            }
            if self.opacity < 1.0 {
                lowest = lowest.lowest_of(Operator::PassThroughOpacity.intrinsic());
            }
        } else if self.fill_opacity < 1.0 && self.mode != StudioBlendMode::Normal {
            lowest = lowest.lowest_of(Operator::FillOpacity.intrinsic());
        }
        if self.clipped {
            lowest = lowest.lowest_of(Operator::Mask(MaskFormat::BaseAlpha).intrinsic());
        }
        lowest
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Source {
        addr: NodeAddr,
        layer_kind: String,
        value: SourceValue,
        visible: bool,
        appearance: StepAppearance,
    },
    Composite {
        addr: NodeAddr,
        layer_kind: String,
        /// Output addresses of the composited inputs, ordered by authored `input_ordinal`.
        inputs: Vec<NodeAddr>,
        visible: bool,
        appearance: StepAppearance,
    },
}

impl Step {
    pub fn addr(&self) -> &NodeAddr {
        match self {
            Self::Source { addr, .. } | Self::Composite { addr, .. } => addr,
        }
    }
    pub fn visible(&self) -> bool {
        match self {
            Self::Source { visible, .. } | Self::Composite { visible, .. } => *visible,
        }
    }
    pub fn appearance(&self) -> &StepAppearance {
        match self {
            Self::Source { appearance, .. } | Self::Composite { appearance, .. } => appearance,
        }
    }
    pub fn exactness(&self) -> Exactness {
        self.appearance().exactness()
    }
}

/// Topologically ordered, revision-bound plan. Fanout appears once and is referenced by address.
#[derive(Clone, Debug, PartialEq)]
pub struct LoweredPlan {
    pub revision: u64,
    /// Explicit document-level input; recorded in the render receipt (STU-CMP-021).
    pub blend_space: BlendSpace,
    /// False while the document's blending space profile is unassigned.
    pub blend_profile_bound: bool,
    pub steps: Vec<Step>,
    pub outputs: Vec<NodeAddr>,
}

impl LoweredPlan {
    pub fn lowest_exactness(&self) -> Exactness {
        self.steps
            .iter()
            .fold(Exactness::Exact, |lowest, s| lowest.lowest_of(s.exactness()))
    }
}

/// Node-view projection: input addresses of the group's composite step in composite order.
pub fn node_projection(plan: &LoweredPlan, group_layer_id: &str) -> Option<Vec<NodeAddr>> {
    plan.steps.iter().find_map(|s| match s {
        Step::Composite {
            addr, inputs, ..
        } if addr.layer_id == group_layer_id => Some(inputs.clone()),
        _ => None,
    })
}

/// Layer-view projection: child layer ids of the group in composite order.
pub fn layer_projection(plan: &LoweredPlan, group_layer_id: &str) -> Option<Vec<String>> {
    node_projection(plan, group_layer_id)
        .map(|nodes| nodes.into_iter().map(|n| n.layer_id).collect())
}

fn canceled(cancel: &CancellationToken) -> Result<(), CompositeError> {
    cancel.check().map_err(|_| CompositeError::Canceled)
}

type OpKey<'a> = (&'a str, &'a str);

/// Lower `snap` (which must be at `expected_revision`) into a dependency-ordered plan.
pub fn lower(
    snap: &Snapshot,
    expected_revision: u64,
    blend_space: BlendSpace,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<LoweredPlan, CompositeError> {
    canceled(cancel)?;
    let doc = snap.document();
    if doc.revision != expected_revision {
        return Err(CompositeError::StaleRevision {
            expected: expected_revision,
            actual: doc.revision,
        });
    }
    let ops = &doc.graph.operations;
    if ops.len() > limits.max_steps {
        return Err(CompositeError::BudgetExceeded);
    }
    let layers: BTreeMap<&str, &StudioLayer> =
        doc.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect();
    let index: BTreeMap<OpKey<'_>, usize> = ops
        .iter()
        .enumerate()
        .map(|(i, op)| ((op.layer_id.as_str(), op.operation_key.as_str()), i))
        .collect();

    // Authored order authority: edges grouped by target port and sorted by `input_ordinal`.
    let mut incoming: BTreeMap<&PortAddress, Vec<(u64, &PortAddress)>> = BTreeMap::new();
    for edge in &doc.graph.edges {
        canceled(cancel)?;
        incoming
            .entry(&edge.target)
            .or_default()
            .push((edge.input_ordinal, &edge.source));
    }
    for list in incoming.values_mut() {
        list.sort_by_key(|(ordinal, _)| *ordinal);
    }

    let mut ordered_inputs: Vec<Vec<NodeAddr>> = vec![Vec::new(); ops.len()];
    let mut dependencies: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); ops.len()];
    for (i, op) in ops.iter().enumerate() {
        if op.operation_kind != OperationKind::Composite {
            continue;
        }
        let port = op
            .inputs
            .first()
            .ok_or_else(|| CompositeError::UnsupportedDescriptor(op.layer_id.clone()))?;
        let target = PortAddress {
            layer_id: op.layer_id.clone(),
            operation_key: op.operation_key.clone(),
            port_key: port.port_key.clone(),
        };
        for (_, source) in incoming.get(&target).map(Vec::as_slice).unwrap_or_default() {
            let dependency = index
                .get(&(source.layer_id.as_str(), source.operation_key.as_str()))
                .ok_or_else(|| CompositeError::DanglingInput(NodeAddr::from(*source).text()))?;
            dependencies[i].insert(*dependency);
            ordered_inputs[i].push(NodeAddr::from(*source));
        }
    }

    // Kahn's algorithm with a deterministic (layer_id, operation_key) tie-break.
    let mut remaining: Vec<usize> = dependencies.iter().map(BTreeSet::len).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); ops.len()];
    for (i, deps) in dependencies.iter().enumerate() {
        for d in deps {
            dependents[*d].push(i);
        }
    }
    let mut depth = vec![1usize; ops.len()];
    let mut ready: BTreeSet<(OpKey<'_>, usize)> = BTreeSet::new();
    for (i, op) in ops.iter().enumerate() {
        if remaining[i] == 0 {
            ready.insert(((op.layer_id.as_str(), op.operation_key.as_str()), i));
        }
    }
    let mut steps = Vec::with_capacity(ops.len());
    while let Some((_, i)) = ready.pop_first() {
        canceled(cancel)?;
        if depth[i] > limits.max_depth {
            return Err(CompositeError::BudgetExceeded);
        }
        let op = &ops[i];
        let layer = layers
            .get(op.layer_id.as_str())
            .ok_or_else(|| CompositeError::DanglingInput(op.layer_id.clone()))?;
        let out = op
            .outputs
            .first()
            .ok_or_else(|| CompositeError::UnsupportedDescriptor(op.layer_id.clone()))?;
        let addr = NodeAddr {
            layer_id: op.layer_id.clone(),
            operation_key: op.operation_key.clone(),
            port_key: out.port_key.clone(),
        };
        let authored = layer.effective_appearance();
        let appearance = StepAppearance {
            mode: StudioBlendMode::try_from(authored.blend_mode)?,
            opacity: authored.opacity_fraction() as f32,
            fill_opacity: authored.fill_opacity_fraction() as f32,
            clipped: authored.clipped,
            mask: authored.mask.as_ref().is_some_and(|m| m.enabled),
        };
        steps.push(match op.operation_kind {
            OperationKind::Source => Step::Source {
                addr,
                layer_kind: layer.kind.clone(),
                value: match out.value_type {
                    ValueType::Image => SourceValue::Image,
                    ValueType::Vector => SourceValue::Vector,
                    ValueType::Text => SourceValue::Text,
                },
                visible: layer.visible,
                appearance,
            },
            OperationKind::Composite => Step::Composite {
                addr,
                layer_kind: layer.kind.clone(),
                inputs: std::mem::take(&mut ordered_inputs[i]),
                visible: layer.visible,
                appearance,
            },
        });
        for next in &dependents[i] {
            depth[*next] = depth[*next].max(depth[i] + 1);
            remaining[*next] -= 1;
            if remaining[*next] == 0 {
                let n = &ops[*next];
                ready.insert(((n.layer_id.as_str(), n.operation_key.as_str()), *next));
            }
        }
    }
    if steps.len() != ops.len() {
        let stuck = remaining
            .iter()
            .position(|r| *r > 0)
            .map(|i| format!("{}/{}", ops[i].layer_id, ops[i].operation_key))
            .unwrap_or_default();
        return Err(CompositeError::Cycle(stuck));
    }
    Ok(LoweredPlan {
        revision: doc.revision,
        blend_space,
        blend_profile_bound: matches!(doc.blending_space, ProfileBinding::Assigned(_)),
        steps,
        outputs: doc.graph.outputs.iter().map(NodeAddr::from).collect(),
    })
}
