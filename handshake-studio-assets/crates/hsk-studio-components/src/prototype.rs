//! Prototype action graph inside the same document (STU-PRO-102..112, subset): reactions are
//! Folio's `StudioReaction` records on the per-node design entries (`StudioNodeDesign::reactions`),
//! and every destination must be an existing frame (artboard) of the Folio document. Folio has no
//! `PrototypeFlow` container (`flow_starting_points`, STU-PRO-128/129), so the flow is a crate-local
//! start-frame wrapper over the per-node reactions.
//!
//! All seventeen trigger types and the modelled action types are storable; every armed (non-null)
//! trigger counts for reachability. Navigations `CHANGE_TO` and `SCROLL_TO` are refused as
//! unsupported rather than ignored. Actions that do not navigate (`URL`, `SET_VARIABLE`,
//! `SET_VARIABLE_MODE`) add no edge. This module validates and walks the graph; it never plays it
//! (playback and transitions belong to Motion).
use super::design::Design;
use super::instance_resolution::check_id;
use crate::{ComponentsError as E, R};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Navigation, NodeKind, Parent, StudioAction, StudioReaction};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
pub struct PrototypeFlow {
    /// `SPTF-*`
    pub flow_id: String,
    /// Artboard (frame) the flow starts on.
    pub start_frame_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowWarning {
    /// Node whose reaction list holds the unreachable action.
    pub node_id: String,
    /// Index in the flow's reaction order: the design entries in record order, each node's
    /// reactions in their stored order (the order that is authority per node, STU-PRO-103).
    pub reaction_index: usize,
    pub action_index: usize,
    /// Actions after a navigation, `BACK` or `CLOSE` never execute (STU-PRO-104, a warning).
    pub reason: &'static str,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowReport {
    /// Frames reachable from the start frame through armed navigation actions, sorted.
    pub reachable_frames: Vec<String>,
    pub warnings: Vec<FlowWarning>,
}
#[derive(Clone, Copy, Debug)]
pub struct FlowLimits {
    pub max_reactions: usize,
    pub max_actions: usize,
}
impl Default for FlowLimits {
    fn default() -> Self {
        Self {
            max_reactions: 4096,
            max_actions: 16_384,
        }
    }
}

/// Validates the flow against the Folio document and its reaction records and returns
/// reachability. Cycles (A to B to A) are legal in a prototype and are not an error.
pub fn validate_flow(
    flow: &PrototypeFlow,
    design: &Design,
    limits: &FlowLimits,
    cancel: &CancellationToken,
) -> R<FlowReport> {
    cancel.check().map_err(|_| E::Canceled)?;
    check_id(&flow.flow_id, "SPTF")?;
    let doc = design.document();
    let frames: BTreeSet<&str> = doc
        .artboards
        .iter()
        .map(|a| a.artboard_id.as_str())
        .collect();
    if !frames.contains(flow.start_frame_id.as_str()) {
        return Err(E::PrototypeTarget(flow.start_frame_id.clone()));
    }
    let reactions: Vec<(&str, &StudioReaction)> = design
        .records()
        .nodes
        .iter()
        .flat_map(|n| {
            n.reactions
                .iter()
                .map(move |r| (n.node.node_id.as_str(), r))
        })
        .collect();
    if reactions.len() > limits.max_reactions
        || reactions
            .iter()
            .map(|(_, r)| r.actions.len())
            .sum::<usize>()
            > limits.max_actions
    {
        return Err(E::BudgetExceeded);
    }
    // Parent links of every node a reaction can sit on, to find the owning frame.
    let mut parent: BTreeMap<&str, &Parent> = BTreeMap::new();
    for a in &doc.artboards {
        parent.insert(a.artboard_id.as_str(), &a.parent);
    }
    for s in &doc.page_spreads {
        parent.insert(s.page_spread_id.as_str(), &s.parent);
    }
    for l in &doc.layers {
        parent.insert(l.layer_id.as_str(), &l.parent);
    }
    let owner = |node: &str| -> Option<String> {
        let mut cur = node;
        for _ in 0..=parent.len() {
            if frames.contains(cur) {
                return Some(cur.to_owned());
            }
            match parent.get(cur)? {
                Parent::Node(r) if r.node_kind != NodeKind::PageSpread => cur = r.node_id.as_str(),
                _ => return None,
            }
        }
        None
    };

    let mut warnings = Vec::new();
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (ri, (node_id, r)) in reactions.iter().enumerate() {
        cancel.check().map_err(|_| E::Canceled)?;
        if !parent.contains_key(node_id) {
            return Err(E::NoSuchNode((*node_id).to_owned()));
        }
        let from = owner(node_id);
        // STU-PRO-104: a navigation (`NODE`, `BACK`, `CLOSE`) ends the array; later actions never run.
        let mut ended = false;
        for (ai, a) in r.actions.iter().enumerate() {
            if ended {
                warnings.push(FlowWarning {
                    node_id: (*node_id).to_owned(),
                    reaction_index: ri,
                    action_index: ai,
                    reason: "action_after_navigation",
                });
            }
            let StudioAction::Node {
                destination_id,
                navigation,
                overlay_relative_position,
                ..
            } = a
            else {
                ended |= matches!(a, StudioAction::Back {} | StudioAction::Close {});
                continue;
            };
            if matches!(navigation, Navigation::ChangeTo | Navigation::ScrollTo) {
                return Err(E::UnsupportedPrototype(format!("{navigation:?}")));
            }
            let dest = destination_id
                .as_deref()
                .filter(|d| frames.contains(d))
                .ok_or_else(|| {
                    E::PrototypeTarget(destination_id.clone().unwrap_or_else(|| "null".into()))
                })?;
            if let Some(p) = overlay_relative_position
                && (!p.x.is_finite() || !p.y.is_finite() || *navigation != Navigation::Overlay)
            {
                return Err(E::InvalidValue(format!(
                    "overlay_relative_position@{node_id}"
                )));
            }
            // Only an action that runs adds an edge; later ones are warned about above and add none.
            if let (false, Some(from), Some(_)) = (ended, &from, &r.trigger) {
                edges
                    .entry(from.clone())
                    .or_default()
                    .insert(dest.to_owned());
            }
            ended = true;
        }
    }
    let mut seen: BTreeSet<String> = BTreeSet::from([flow.start_frame_id.clone()]);
    let mut work = vec![flow.start_frame_id.clone()];
    while let Some(f) = work.pop() {
        for next in edges.get(&f).into_iter().flatten() {
            if seen.insert(next.clone()) {
                work.push(next.clone());
            }
        }
    }
    Ok(FlowReport {
        reachable_frames: seen.into_iter().collect(),
        warnings,
    })
}
