//! Folio layer graph of an imported Figma document (lane SMB-INTEROP-FIG-LAYER-GRAPH), so the
//! document can be composited. Folio types a vector source `vector` while every group input is
//! `image`, so a vector layer can never feed a group; the mapping is therefore:
//!
//! - a vector layer with paths is a `source` operation (`vector` out) listed as a graph output;
//! - a boolean group (the group carries a boolean record) is a `mix` operation with no inputs:
//!   composite draws it as one leaf, its operand layers get no operation;
//! - a text layer is a `source` operation whose single `out` port is `image` (Folio G1:
//!   rasterised text), so it can feed a group;
//! - a group whose content is only image-typed (text sources, boolean groups, such groups) is a
//!   `mix` operation with its children as ordered edges (ordinal 0 = bottom-most);
//! - any other group is flattened: its children are listed as outputs in paint order. Its own
//!   opacity, blend mode and mask cannot be applied (typed loss `group_appearance_flattened`);
//! - artboards are not graph nodes: their children stand in the canvas order of the artboard;
//! - only the first visible page (CANVAS) is listed in `graph.outputs` (pages share one canvas
//!   space and would overlay); the other pages keep their operations (`page_outside_outputs`).
//!
//! Layers without a graph form (component instances, unmapped kinds, vector layers without paths)
//! are typed losses, never silently dropped.
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_folio::{
    Cardinality, DependencyKind, Edge, GraphSchema, NodeKind, NodeRef, Operation, OperationKind,
    Parent, Payload, Port, PortAddress, Semantic, StudioArtboard, StudioLayer, StudioLayerGraph,
    ValueType,
};
use std::collections::{BTreeMap, HashMap, HashSet};

const MAX_DEPTH: usize = 72;

struct Out {
    addr: PortAddress,
    image: bool,
}

struct Builder<'a> {
    layers: HashMap<&'a str, &'a StudioLayer>,
    children: HashMap<String, Vec<NodeRef>>,
    booleans: &'a HashSet<String>,
    ops: Vec<Operation>,
    edges: Vec<Edge>,
    loss: BTreeMap<(&'static str, &'static str), u32>,
}

fn port(key: &str, value_type: ValueType, cardinality: Cardinality, semantic: Semantic) -> Port {
    Port {
        port_key: key.into(),
        value_type,
        cardinality,
        semantic,
    }
}

impl Builder<'_> {
    fn lose(&mut self, path: &'static str, code: &'static str) {
        let n = self.loss.entry((path, code)).or_insert(0);
        *n = n.saturating_add(1);
    }

    fn source(&mut self, layer_id: &str, ty: ValueType) -> Out {
        self.ops.push(Operation {
            layer_id: layer_id.into(),
            operation_key: "source".into(),
            operation_kind: OperationKind::Source,
            inputs: Vec::new(),
            outputs: vec![port("out", ty, Cardinality::One, Semantic::Source)],
        });
        Out {
            addr: PortAddress {
                layer_id: layer_id.into(),
                operation_key: "source".into(),
                port_key: "out".into(),
            },
            image: ty == ValueType::Image,
        }
    }

    fn mix(&mut self, layer_id: &str, inputs: &[Out]) -> Out {
        self.ops.push(Operation {
            layer_id: layer_id.into(),
            operation_key: "mix".into(),
            operation_kind: OperationKind::Composite,
            inputs: vec![port(
                "inputs",
                ValueType::Image,
                Cardinality::Many,
                Semantic::OrderedComposite,
            )],
            outputs: vec![port(
                "out",
                ValueType::Image,
                Cardinality::One,
                Semantic::CompositeResult,
            )],
        });
        for (ordinal, input) in inputs.iter().enumerate() {
            self.edges.push(Edge {
                source: input.addr.clone(),
                target: PortAddress {
                    layer_id: layer_id.into(),
                    operation_key: "mix".into(),
                    port_key: "inputs".into(),
                },
                dependency_kind: DependencyKind::Data,
                input_ordinal: u64::try_from(ordinal).unwrap_or(u64::MAX),
            });
        }
        Out {
            addr: PortAddress {
                layer_id: layer_id.into(),
                operation_key: "mix".into(),
                port_key: "out".into(),
            },
            image: true,
        }
    }

    fn of_children(&mut self, id: &str, depth: usize) -> Vec<Out> {
        let kids = self.children.get(id).cloned().unwrap_or_default();
        let mut out = Vec::new();
        for kid in &kids {
            out.extend(self.node(kid, depth.saturating_add(1)));
        }
        out
    }

    /// The graph outputs one node contributes, bottom-most first.
    fn node(&mut self, node: &NodeRef, depth: usize) -> Vec<Out> {
        if depth > MAX_DEPTH {
            self.lose("native.graph", "nesting_too_deep_not_composited");
            return Vec::new();
        }
        if node.node_kind == NodeKind::Artboard {
            return self.of_children(&node.node_id, depth);
        }
        let Some(layer) = self.layers.get(node.node_id.as_str()).copied() else {
            return Vec::new();
        };
        let id = layer.layer_id.as_str();
        match (layer.kind.as_str(), &layer.payload) {
            ("group", Payload::Group) => {
                if self.booleans.contains(id) {
                    // One shape: the operands belong to the boolean record, not drawn alone.
                    return vec![self.mix(id, &[])];
                }
                let inner = self.of_children(id, depth);
                if inner.is_empty() {
                    Vec::new()
                } else if inner.iter().all(|o| o.image) {
                    vec![self.mix(id, &inner)]
                } else {
                    if layer.appearance.is_some() {
                        self.lose("native.graph", "group_appearance_flattened");
                    }
                    self.lose("native.graph", "group_flattened_vector_not_group_input");
                    inner
                }
            }
            ("vector", Payload::Vector { path_ids }) if !path_ids.is_empty() => {
                vec![self.source(id, ValueType::Vector)]
            }
            ("vector", _) => {
                self.lose("native.graph", "vector_without_paths_not_composited");
                Vec::new()
            }
            ("text", Payload::Text { .. }) => vec![self.source(id, ValueType::Image)],
            _ => {
                self.lose("native.graph", "layer_kind_not_composited");
                Vec::new()
            }
        }
    }
}

/// The layer graph plus its typed loss counts (`(path, code)` -> count).
pub(crate) struct GraphBuild {
    pub graph: StudioLayerGraph,
    pub loss: BTreeMap<(&'static str, &'static str), u32>,
}

/// Builds the graph from the mapped layers. `order` is the mapper's node order (parents before
/// children, siblings bottom-most first); `booleans` are the layers carrying a boolean record.
pub(crate) fn build(
    layers: &[StudioLayer],
    artboards: &[StudioArtboard],
    order: &[NodeRef],
    booleans: &HashSet<String>,
) -> GraphBuild {
    let parent_of: HashMap<&str, &Parent> = layers
        .iter()
        .map(|l| (l.layer_id.as_str(), &l.parent))
        .chain(artboards.iter().map(|a| (a.artboard_id.as_str(), &a.parent)))
        .collect();
    let mut children: HashMap<String, Vec<NodeRef>> = HashMap::new();
    let mut roots: Vec<NodeRef> = Vec::new();
    for node in order {
        match parent_of.get(node.node_id.as_str()) {
            Some(Parent::Root(())) => roots.push(node.clone()),
            Some(Parent::Node(p)) => children
                .entry(p.node_id.clone())
                .or_default()
                .push(node.clone()),
            None => {}
        }
    }
    let mut b = Builder {
        layers: layers.iter().map(|l| (l.layer_id.as_str(), l)).collect(),
        children,
        booleans,
        ops: Vec::new(),
        edges: Vec::new(),
        loss: BTreeMap::new(),
    };
    // Roots are the pages; the first visible page is the document's canvas.
    let mut outputs: Vec<PortAddress> = Vec::new();
    let mut listed = false;
    for root in &roots {
        let visible = b
            .layers
            .get(root.node_id.as_str())
            .is_some_and(|l| l.visible);
        let contributed = b.node(root, 0);
        if visible && !listed {
            listed = true;
            outputs.extend(contributed.into_iter().map(|o| o.addr));
        } else if visible && !contributed.is_empty() {
            b.lose("native.graph", "page_outside_outputs");
        }
    }
    GraphBuild {
        graph: StudioLayerGraph {
            schema_id: GraphSchema::V1,
            operations: b.ops,
            edges: b.edges,
            outputs,
        },
        loss: b.loss,
    }
}
