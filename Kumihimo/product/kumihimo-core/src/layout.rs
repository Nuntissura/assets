use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// Plan-local selector; structural repair can replace it. It is not a stable entity ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeId(pub u64);
#[cfg(feature = "serde")]
impl serde::Serialize for NodeId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        crate::decimal::serialize(&self.0, s)
    }
}
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for NodeId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        crate::decimal::deserialize(d).map(Self)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub enum LayoutItem {
    Frame(FrameId),
    Tool(ToolId),
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct WeightedNode {
    pub node: NodeId,
    pub weight: Scalar,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct Cell {
    pub row: usize,
    pub column: usize,
    pub node: NodeId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub enum LayoutNode {
    Leaf(LayoutItem),
    Split {
        axis: Axis,
        children: Vec<WeightedNode>,
    },
    Grid {
        rows: Vec<Scalar>,
        columns: Vec<Scalar>,
        cells: Vec<Cell>,
    },
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct LayoutPlan {
    pub root: Option<NodeId>,
    pub nodes: BTreeMap<NodeId, LayoutNode>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct GeometryContext {
    pub viewport: Rect,
    pub gap: Scalar,
    pub overflow: Overflow,
    pub minimums: BTreeMap<FrameId, Size>,
    pub frame_weights: BTreeMap<FrameId, Scalar>,
    /// Explicitly eligible frame docking locations; snap candidates are a projection of these.
    pub allowed_docks: Vec<DockLocation>,
    pub allow_floating: bool,
}
impl GeometryContext {
    pub fn new(viewport: Rect) -> Self {
        Self {
            viewport,
            gap: Scalar::new(0.0).unwrap(),
            overflow: Overflow::Reject,
            minimums: BTreeMap::new(),
            frame_weights: BTreeMap::new(),
            allowed_docks: vec![],
            allow_floating: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeometryOutput {
    pub generation: u64,
    pub workspace: WorkspaceId,
    pub required_extent: Size,
    pub virtual_extent: Size,
    pub viewport: Rect,
    pub content: Rect,
    pub frames: BTreeMap<FrameId, Rect>,
    pub tools: BTreeMap<ToolId, Rect>,
    pub frame_content: BTreeMap<FrameId, Rect>,
    pub clips: BTreeMap<FrameId, Rect>,
    pub tool_states: BTreeMap<ToolId, ToolPresentation>,
    pub nodes: BTreeMap<NodeId, Rect>,
    pub cells: BTreeMap<(NodeId, usize, usize), Rect>,
}

fn invalid(reason: &'static str) -> Error {
    GeometryError::InvalidPlan(reason).into()
}
impl LayoutPlan {
    pub(crate) fn entries(&self) -> usize {
        self.nodes
            .values()
            .map(|n| {
                1 + match n {
                    LayoutNode::Leaf(_) => 1,
                    LayoutNode::Split { children, .. } => children.len(),
                    LayoutNode::Grid {
                        rows,
                        columns,
                        cells,
                    } => rows.len() + columns.len() + cells.len(),
                }
            })
            .sum()
    }
    pub(crate) fn children(&self, id: NodeId) -> Result<Vec<NodeId>, Error> {
        Ok(
            match self.nodes.get(&id).ok_or_else(|| invalid("missing node"))? {
                LayoutNode::Leaf(_) => vec![],
                LayoutNode::Split { children, .. } => children.iter().map(|c| c.node).collect(),
                LayoutNode::Grid { cells, .. } => cells.iter().map(|c| c.node).collect(),
            },
        )
    }
    pub(crate) fn order(&self) -> Result<Vec<NodeId>, Error> {
        let Some(root) = self.root else {
            return if self.nodes.is_empty() {
                Ok(vec![])
            } else {
                Err(invalid("nodes in configured-empty plan"))
            };
        };
        let mut order = vec![];
        let mut seen = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                return Err(invalid("cycle or multiple node parents"));
            }
            order.push(id);
            match self.nodes.get(&id).ok_or_else(|| invalid("missing node"))? {
                LayoutNode::Split { children, .. } => {
                    if children.is_empty() {
                        return Err(invalid("empty split"));
                    }
                    for child in children {
                        crate::geometry::positive(child.weight.get())?;
                    }
                }
                LayoutNode::Grid {
                    rows,
                    columns,
                    cells,
                } => {
                    if rows.is_empty() || columns.is_empty() {
                        return Err(invalid("empty grid tracks"));
                    }
                    for weight in rows.iter().chain(columns) {
                        crate::geometry::positive(weight.get())?;
                    }
                    let mut positions = BTreeSet::new();
                    for cell in cells {
                        if cell.row >= rows.len()
                            || cell.column >= columns.len()
                            || !positions.insert((cell.row, cell.column))
                        {
                            return Err(invalid("invalid or occupied cell"));
                        }
                    }
                }
                _ => {}
            }
            pending.extend(self.children(id)?);
        }
        if seen.len() != self.nodes.len() {
            return Err(invalid("orphan packing nodes"));
        }
        Ok(order)
    }
    pub(crate) fn next_node(&self) -> Result<NodeId, Error> {
        self.nodes.keys().next_back().map_or(Ok(NodeId(1)), |id| {
            id.0.checked_add(1)
                .map(NodeId)
                .ok_or_else(|| invalid("local selector exhaustion"))
        })
    }
    pub(crate) fn append(&mut self, item: LayoutItem) -> Result<(), Error> {
        let id = self.next_node()?;
        self.nodes.insert(id, LayoutNode::Leaf(item));
        match self.root {
            None => self.root = Some(id),
            Some(root) => match self.nodes.get_mut(&root).unwrap() {
                LayoutNode::Split {
                    axis: Axis::Horizontal,
                    children,
                } => children.push(WeightedNode {
                    node: id,
                    weight: Scalar::new(1.0)?,
                }),
                _ => {
                    let parent = self.next_node()?;
                    self.nodes.insert(
                        parent,
                        LayoutNode::Split {
                            axis: Axis::Horizontal,
                            children: vec![
                                WeightedNode {
                                    node: root,
                                    weight: Scalar::new(1.0)?,
                                },
                                WeightedNode {
                                    node: id,
                                    weight: Scalar::new(1.0)?,
                                },
                            ],
                        },
                    );
                    self.root = Some(parent);
                }
            },
        }
        Ok(())
    }
    pub(crate) fn remove(&mut self, item: LayoutItem) -> Result<(), Error> {
        let order = self.order()?;
        let mut empty = BTreeSet::new();
        for id in order.into_iter().rev() {
            let node = self.nodes.get_mut(&id).unwrap();
            match node {
                LayoutNode::Leaf(value) if *value == item => {
                    empty.insert(id);
                }
                LayoutNode::Split { children, .. } => {
                    children.retain(|c| !empty.contains(&c.node));
                    if children.is_empty() {
                        empty.insert(id);
                    }
                }
                LayoutNode::Grid { cells, .. } => {
                    cells.retain(|c| !empty.contains(&c.node));
                    // Keep track sizes and holes even when this grid becomes empty.
                }
                _ => {}
            }
        }
        for id in &empty {
            self.nodes.remove(id);
        }
        if self.root.is_some_and(|id| empty.contains(&id)) {
            self.root = None;
        }
        // Unary splits retain their weight metadata and local selectors.
        Ok(())
    }
}

impl Snapshot {
    pub(crate) fn effective_plan(&self, workspace: WorkspaceId) -> Result<LayoutPlan, Error> {
        if let Some(plan) = self.layouts.get(&workspace) {
            return Ok(plan.clone());
        }
        let ws = self
            .workspaces
            .get(&workspace)
            .ok_or(Error::Missing(EntityId::Workspace(workspace)))?;
        let mut plan = LayoutPlan::default();
        for frame in &ws.frames {
            if !self.floats.contains_key(frame) {
                plan.append(LayoutItem::Frame(*frame))?;
            }
        }
        let leaf_frames: BTreeMap<_, _> = plan
            .nodes
            .iter()
            .filter_map(|(id, node)| match node {
                LayoutNode::Leaf(LayoutItem::Frame(frame)) => Some((*id, *frame)),
                _ => None,
            })
            .collect();
        if let Some(root) = plan.root {
            if let LayoutNode::Split { children, .. } = plan.nodes.get_mut(&root).unwrap() {
                for child in children {
                    if let Some(frame) = leaf_frames.get(&child.node) {
                        child.weight = self
                            .contexts
                            .get(&workspace)
                            .and_then(|c| c.frame_weights.get(frame))
                            .copied()
                            .unwrap_or(Scalar::new(1.0)?);
                    }
                }
            }
        }
        Ok(plan)
    }
    pub(crate) fn validate_plan(
        &self,
        workspace: WorkspaceId,
        plan: &LayoutPlan,
    ) -> Result<(), Error> {
        plan.order()?;
        let ws = self
            .workspaces
            .get(&workspace)
            .ok_or(Error::Missing(EntityId::Workspace(workspace)))?;
        let expected: BTreeSet<_> = ws
            .frames
            .iter()
            .filter(|id| !self.floats.contains_key(id))
            .copied()
            .collect();
        let mut frames = BTreeSet::new();
        let mut tools = BTreeSet::new();
        for (id, node) in &plan.nodes {
            match node {
                LayoutNode::Leaf(LayoutItem::Frame(frame)) => {
                    if !frames.insert(*frame) {
                        return Err(invalid("duplicate frame leaf"));
                    }
                }
                LayoutNode::Leaf(LayoutItem::Tool(tool)) => {
                    if !tools.insert(*tool) {
                        return Err(invalid("duplicate tool leaf"));
                    }
                    let record = self
                        .tools
                        .get(tool)
                        .ok_or(Error::Missing(EntityId::Tool(*tool)))?;
                    let ToolPlacement::Cell {
                        workspace: target,
                        grid,
                        row,
                        column,
                    } = record.placement
                    else {
                        return Err(invalid("edge tool in packing"));
                    };
                    if target != workspace {
                        return Err(invalid("tool in wrong workspace"));
                    }
                    let Some(LayoutNode::Grid { cells, .. }) = plan.nodes.get(&grid) else {
                        return Err(invalid("tool grid missing"));
                    };
                    if !cells
                        .iter()
                        .any(|c| c.node == *id && c.row == row && c.column == column)
                    {
                        return Err(invalid("tool cell mismatch"));
                    }
                }
                _ => {}
            }
        }
        if frames != expected {
            return Err(invalid("frame coverage differs from canonical ownership"));
        }
        let expected_tools: BTreeSet<_> = self
            .tools
            .values()
            .filter_map(|t| match t.placement {
                ToolPlacement::Cell {
                    workspace: target, ..
                } if target == workspace => Some(t.id),
                _ => None,
            })
            .collect();
        if tools != expected_tools {
            return Err(invalid("tool coverage mismatch"));
        }
        Ok(())
    }
    pub fn layout_entry_count(&self) -> usize {
        self.layouts
            .values()
            .map(LayoutPlan::entries)
            .sum::<usize>()
            + self
                .contexts
                .values()
                .map(|c| 1 + c.minimums.len() + c.frame_weights.len() + c.allowed_docks.len())
                .sum::<usize>()
            + self
                .tools
                .values()
                .map(|t| {
                    4 + t.policy.allowed.len()
                        + t.policy.compatible_targets.len()
                        + t.policy.orientations.len()
                })
                .sum::<usize>()
            + self.floats.len() * 2
            + self.maximized.len()
    }
}
