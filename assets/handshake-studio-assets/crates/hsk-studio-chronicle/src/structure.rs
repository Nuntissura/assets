//! Layer-stack structure for [`crate::Property::Placement`] (lane SMB-HARNESS-UI-3): where a layer
//! sits, and the document surgery for create, delete and move. Folio's graph is the stack: a group
//! layer's composite operation takes its children through the `inputs` port ordered by
//! `input_ordinal` (0 = bottom-most) and the document root is `graph.outputs` (index 0 =
//! bottom-most); a stacked layer has exactly one operation whose single output fills exactly one
//! stack slot. Everything else (cardinality, cycles, payload/kind binding, dangling children of a
//! deleted group) is re-validated by Folio on the successor, so surgery that would leave a broken
//! graph is refused there (`FolioRejected`), never published.
#![deny(clippy::arithmetic_side_effects)]
use crate::{Code, LayerCreate, PlaceCommand, PlaceMode, PropertyAddress, Result, failure, layer_addressable};
use hsk_studio_folio::{
    DependencyKind, Edge, NodeKind, NodeRef, OperationKind, Parent, Payload, PortAddress,
    StudioDocument,
};
use std::collections::BTreeSet;

/// The single output port of the layer's single graph operation.
fn out_port(d: &StudioDocument, layer_id: &str) -> Option<PortAddress> {
    let mut ops = d.graph.operations.iter().filter(|o| o.layer_id == layer_id);
    let op = ops.next()?;
    if ops.next().is_some() {
        return None;
    }
    let [port] = op.outputs.as_slice() else {
        return None;
    };
    Some(PortAddress {
        layer_id: op.layer_id.clone(),
        operation_key: op.operation_key.clone(),
        port_key: port.port_key.clone(),
    })
}

/// The ordered input port of a group layer's composite operation.
fn group_port(d: &StudioDocument, group_id: &str) -> Option<PortAddress> {
    let group = d.layers.iter().find(|l| l.layer_id == group_id)?;
    if !(group.kind == "group" && matches!(group.payload, Payload::Group)) {
        return None;
    }
    let mut ops = d
        .graph
        .operations
        .iter()
        .filter(|o| o.layer_id == group_id && o.operation_kind == OperationKind::Composite);
    let op = ops.next()?;
    if ops.next().is_some() {
        return None;
    }
    let [port] = op.inputs.as_slice() else {
        return None;
    };
    Some(PortAddress {
        layer_id: op.layer_id.clone(),
        operation_key: op.operation_key.clone(),
        port_key: port.port_key.clone(),
    })
}

/// The stack a parent holds: `None` = the document root (`graph.outputs`), `Some` = a group's
/// input port. A parent that is not the root or a group layer with one composite operation has
/// no stack.
fn stack_of(d: &StudioDocument, parent: &Parent) -> Option<Option<PortAddress>> {
    match parent {
        Parent::Root(()) => Some(None),
        Parent::Node(n) if n.node_kind == NodeKind::Layer => group_port(d, &n.node_id).map(Some),
        Parent::Node(_) => None,
    }
}

/// Parent and stack index of a stacked layer; `None` when the layer is absent or not stacked.
pub(crate) fn placement_of(d: &StudioDocument, layer_id: &str) -> Option<(Parent, u64)> {
    let layer = d.layers.iter().find(|l| l.layer_id == layer_id)?;
    let out = out_port(d, layer_id)?;
    let in_outputs = d.graph.outputs.iter().filter(|o| **o == out).count();
    let edges: Vec<&Edge> = d.graph.edges.iter().filter(|e| e.source == out).collect();
    let index = match (stack_of(d, &layer.parent)?, in_outputs, edges.as_slice()) {
        (None, 1, []) => u64::try_from(d.graph.outputs.iter().position(|o| *o == out)?).ok()?,
        (Some(port), 0, [edge]) if edge.target == port => edge.input_ordinal,
        _ => return None,
    };
    Some((layer.parent.clone(), index))
}

/// Layer ids of a stack, bottom-most first; `None` when the parent holds no stack.
fn stack_ids(d: &StudioDocument, parent: &Parent) -> Option<Vec<String>> {
    Some(match stack_of(d, parent)? {
        None => d.graph.outputs.iter().map(|o| o.layer_id.clone()).collect(),
        Some(port) => {
            let mut inputs: Vec<(u64, String)> = d
                .graph
                .edges
                .iter()
                .filter(|e| e.target == port)
                .map(|e| (e.input_ordinal, e.source.layer_id.clone()))
                .collect();
            inputs.sort();
            inputs.into_iter().map(|(_, id)| id).collect()
        }
    })
}

fn container(anchor: Option<&String>) -> Parent {
    match anchor {
        None => Parent::Root(()),
        Some(id) => Parent::Node(NodeRef {
            node_kind: NodeKind::Layer,
            node_id: id.clone(),
        }),
    }
}

fn count(n: usize, a: &PropertyAddress) -> Result<u64> {
    u64::try_from(n).map_err(|_| failure(Code::BudgetExceeded, Some(a)))
}

/// Where `command` puts layer `id` in `d`: parent and stack index, measured on the stack without
/// the layer itself. Refusals: an anchor that is the layer itself, absent, not stacked, or (for
/// a container) not a group layer (`InvalidTarget`); an anchor where the mode needs none or
/// none where it needs one (`InvalidInput`).
pub(crate) fn resolve(
    d: &StudioDocument,
    id: &str,
    command: &PlaceCommand,
    a: &PropertyAddress,
) -> Result<(Parent, u64)> {
    let target = |e| failure(e, Some(a));
    if command.anchor.as_deref() == Some(id) {
        return Err(target(Code::InvalidTarget));
    }
    let others = |parent: &Parent| -> Result<Vec<String>> {
        let mut ids = stack_ids(d, parent).ok_or_else(|| target(Code::InvalidTarget))?;
        ids.retain(|l| l != id);
        Ok(ids)
    };
    match command.mode {
        PlaceMode::PlaceInside | PlaceMode::PlaceAtBeginning | PlaceMode::PlaceAtEnd => {
            if command.mode == PlaceMode::PlaceInside && command.anchor.is_none() {
                return Err(target(Code::InvalidInput));
            }
            let parent = container(command.anchor.as_ref());
            let ids = others(&parent)?;
            let index = if command.mode == PlaceMode::PlaceAtEnd {
                0
            } else {
                count(ids.len(), a)?
            };
            Ok((parent, index))
        }
        PlaceMode::PlaceBefore | PlaceMode::PlaceAfter => {
            let anchor = command.anchor.as_deref().ok_or_else(|| target(Code::InvalidInput))?;
            let (parent, _) = placement_of(d, anchor).ok_or_else(|| target(Code::InvalidTarget))?;
            let ids = others(&parent)?;
            let at = ids.iter().position(|l| l == anchor).ok_or_else(|| target(Code::InvalidTarget))?;
            let index = if command.mode == PlaceMode::PlaceBefore {
                at.checked_add(1).ok_or_else(|| target(Code::BudgetExceeded))?
            } else {
                at
            };
            Ok((parent, count(index, a)?))
        }
    }
}

/// The command that puts layer `id` back where it is in `d` once it has left (the inverse of a
/// move or delete): the top or bottom of its container, else directly above its lower neighbour.
pub(crate) fn restore_command(d: &StudioDocument, id: &str) -> Option<PlaceCommand> {
    let (parent, index) = placement_of(d, id)?;
    let mut ids = stack_ids(d, &parent)?;
    ids.retain(|l| l != id);
    let anchor_of = |p: &Parent| match p {
        Parent::Root(()) => None,
        Parent::Node(n) => Some(n.node_id.clone()),
    };
    let index = usize::try_from(index).ok()?;
    let (mode, anchor) = if index == 0 {
        (PlaceMode::PlaceAtEnd, anchor_of(&parent))
    } else if index >= ids.len() {
        (PlaceMode::PlaceAtBeginning, anchor_of(&parent))
    } else {
        (PlaceMode::PlaceBefore, ids.get(index.checked_sub(1)?).cloned())
    };
    Some(PlaceCommand {
        mode,
        anchor,
        create: None,
    })
}

/// Renumbers the inputs of `ports` densely (0..n) in their current ordinal order.
fn compact(d: &mut StudioDocument, ports: &BTreeSet<PortAddress>) {
    for port in ports {
        let mut ordinals: Vec<(u64, usize)> = d
            .graph
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| &e.target == port)
            .map(|(i, e)| (e.input_ordinal, i))
            .collect();
        ordinals.sort_unstable();
        for (n, (_, i)) in ordinals.into_iter().enumerate() {
            if let (Some(edge), Ok(n)) = (d.graph.edges.get_mut(i), u64::try_from(n)) {
                edge.input_ordinal = n;
            }
        }
    }
}

/// Takes the layer out of its stack: its output leaves `graph.outputs`, its edges leave the graph
/// and the stacks it fed are renumbered densely.
fn detach(d: &mut StudioDocument, layer_id: &str) {
    let fed: BTreeSet<PortAddress> = d
        .graph
        .edges
        .iter()
        .filter(|e| e.source.layer_id == layer_id)
        .map(|e| e.target.clone())
        .collect();
    d.graph.edges.retain(|e| e.source.layer_id != layer_id);
    d.graph.outputs.retain(|o| o.layer_id != layer_id);
    compact(d, &fed);
}

/// Removes the layer, its operations and every edge from or into them (a deleted group's children
/// then refer to a missing parent, which Folio refuses unless they were moved out first). Absent
/// layers are left as they are.
pub(crate) fn remove_layer(d: &mut StudioDocument, layer_id: &str) {
    detach(d, layer_id);
    d.graph.edges.retain(|e| e.target.layer_id != layer_id);
    d.graph.operations.retain(|o| o.layer_id != layer_id);
    d.layers.retain(|l| l.layer_id != layer_id);
}

/// Puts `out` at `index` of the parent's stack (siblings at or above move up by one).
fn attach(d: &mut StudioDocument, a: &PropertyAddress, out: PortAddress, p: &(Parent, u64)) -> Result<()> {
    let p = Target { parent: &p.0, index: p.1 };
    let stack = stack_of(d, p.parent).ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
    match stack {
        None => {
            let index = usize::try_from(p.index)
                .ok()
                .filter(|i| *i <= d.graph.outputs.len())
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            d.graph.outputs.insert(index, out);
        }
        Some(port) => {
            let count = d.graph.edges.iter().filter(|e| e.target == port).count();
            if usize::try_from(p.index).map_or(true, |i| i > count) {
                return Err(failure(Code::InvalidTarget, Some(a)));
            }
            for edge in d.graph.edges.iter_mut().filter(|e| e.target == port) {
                if edge.input_ordinal >= p.index {
                    edge.input_ordinal = edge
                        .input_ordinal
                        .checked_add(1)
                        .ok_or_else(|| failure(Code::RevisionOverflow, Some(a)))?;
                }
            }
            d.graph.edges.push(Edge {
                source: out,
                target: port,
                dependency_kind: DependencyKind::Data,
                input_ordinal: p.index,
            });
        }
    }
    Ok(())
}

struct Target<'a> {
    parent: &'a Parent,
    index: u64,
}

/// Applies one placement command to `d` (create, delete, move or nothing).
pub(crate) fn set_placement(
    d: &mut StudioDocument,
    a: &PropertyAddress,
    value: Option<&PlaceCommand>,
) -> Result<()> {
    let id = a.target.node_id.as_str();
    let exists = d.layers.iter().find(|l| l.layer_id == id);
    if exists.is_some_and(|l| !layer_addressable(l)) {
        return Err(failure(Code::InvalidTarget, Some(a)));
    }
    let exists = exists.is_some();
    let target = value.map(|c| resolve(d, id, c, a)).transpose()?;
    match (exists, value, target) {
        (false, None, _) => Ok(()),
        (false, Some(c), Some(p)) => {
            let create = c
                .create
                .as_ref()
                .ok_or_else(|| failure(Code::InvalidInput, Some(a)))?;
            let LayerCreate { layer, operation } = &**create;
            if layer.layer_id != id
                || operation.layer_id != id
                || layer.parent != p.0
                || matches!(layer.payload, Payload::Unsupported { .. })
            {
                return Err(failure(Code::InvalidInput, Some(a)));
            }
            d.layers.push(layer.clone());
            d.graph.operations.push(operation.clone());
            let out = out_port(d, id).ok_or_else(|| failure(Code::InvalidInput, Some(a)))?;
            attach(d, a, out, &p)
        }
        (true, None, _) => {
            placement_of(d, id).ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            remove_layer(d, id);
            Ok(())
        }
        (true, Some(c), Some(p)) => {
            if c.create.is_some() {
                return Err(failure(Code::InvalidInput, Some(a)));
            }
            placement_of(d, id).ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            let out = out_port(d, id).ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            detach(d, id);
            if let Some(layer) = d.layers.iter_mut().find(|l| l.layer_id == id) {
                layer.parent = p.0.clone();
            }
            attach(d, a, out, &p)
        }
        (_, Some(_), None) => Err(failure(Code::InvalidInput, Some(a))),
    }
}

/// The create payload that restores `layer_id` as it is in `d` (the inverse of a delete).
pub(crate) fn capture(d: &StudioDocument, layer_id: &str) -> Option<LayerCreate> {
    let layer = d.layers.iter().find(|l| l.layer_id == layer_id)?.clone();
    let operation = d.graph.operations.iter().find(|o| o.layer_id == layer_id)?.clone();
    Some(LayerCreate { layer, operation })
}
