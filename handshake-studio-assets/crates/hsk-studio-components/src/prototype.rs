//! Prototype action graph inside the same document (STU-PRO-102..112, subset): reactions live on
//! nodes, a reaction is `{trigger, actions[]}`, and every destination must be an existing frame of
//! the Folio document. Folio has no design-frame node and no `reactions` field yet, so a frame is a
//! Folio artboard and the flow is a crate-local value (lane notes record the Folio need).
//!
//! Only the triggers and navigations a pointer prototype needs are modelled; everything else in the
//! closed STU-PRO-105/110/111 sets is absent from the types, and `ChangeTo`/`ScrollTo` are refused as
//! unsupported rather than ignored. This module validates and walks the graph; it never plays it
//! (playback and transitions belong to Motion).
use crate::instance_resolution::check_id;
use crate::{ComponentsError as E, R};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{NodeKind, Parent, StudioDocument};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    OnClick,
    OnHover,
    OnPress,
    MouseEnter,
    MouseLeave,
    MouseDown,
    MouseUp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Navigation {
    Navigate,
    Overlay,
    Swap,
    /// Recognised, refused: variant switching from a prototype is not part of this slice.
    ChangeTo,
    /// Recognised, refused.
    ScrollTo,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Back,
    Close,
    Node {
        destination_id: Option<String>,
        navigation: Navigation,
        /// Manual overlay position in points; finite when present.
        overlay_relative_position: Option<(f64, f64)>,
    },
}
/// STU-PRO-102: a null trigger is authored but not armed; it persists and never fires.
#[derive(Clone, Debug, PartialEq)]
pub struct Reaction {
    pub node_id: String,
    pub trigger: Option<Trigger>,
    pub actions: Vec<Action>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct PrototypeFlow {
    /// `SPTF-*`
    pub flow_id: String,
    /// Artboard (frame) the flow starts on.
    pub start_frame_id: String,
    /// Order is authority (evaluation order of reactions sharing a trigger).
    pub reactions: Vec<Reaction>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowWarning {
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

/// Validates the flow against the Folio document and returns reachability. Cycles (A to B to A) are
/// legal in a prototype and are not an error.
pub fn validate_flow(
    flow: &PrototypeFlow,
    doc: &StudioDocument,
    limits: &FlowLimits,
    cancel: &CancellationToken,
) -> R<FlowReport> {
    cancel.check().map_err(|_| E::Canceled)?;
    check_id(&flow.flow_id, "SPTF")?;
    let frames: BTreeSet<&str> = doc.artboards.iter().map(|a| a.artboard_id.as_str()).collect();
    if !frames.contains(flow.start_frame_id.as_str()) {
        return Err(E::PrototypeTarget(flow.start_frame_id.clone()));
    }
    if flow.reactions.len() > limits.max_reactions
        || flow.reactions.iter().map(|r| r.actions.len()).sum::<usize>() > limits.max_actions
    {
        return Err(E::BudgetExceeded);
    }
    // Parent links of every node a reaction can sit on, to find the owning frame.
    let mut parent: BTreeMap<&str, &Parent> = BTreeMap::new();
    for a in &doc.artboards {
        parent.insert(a.artboard_id.as_str(), &a.parent);
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
    for (ri, r) in flow.reactions.iter().enumerate() {
        cancel.check().map_err(|_| E::Canceled)?;
        if !parent.contains_key(r.node_id.as_str()) {
            return Err(E::NoSuchNode(r.node_id.clone()));
        }
        let from = owner(&r.node_id);
        for (ai, a) in r.actions.iter().enumerate() {
            // Every modelled action (BACK, CLOSE, NODE) ends the array (STU-PRO-104).
            if ai > 0 {
                warnings.push(FlowWarning {
                    reaction_index: ri,
                    action_index: ai,
                    reason: "action_after_navigation",
                });
            }
            let Action::Node {
                destination_id,
                navigation,
                overlay_relative_position,
            } = a
            else {
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
            if let Some((x, y)) = overlay_relative_position
                && (!x.is_finite() || !y.is_finite() || *navigation != Navigation::Overlay)
            {
                return Err(E::InvalidValue(format!("overlay_relative_position@{}", r.node_id)));
            }
            if let (Some(from), Some(_)) = (&from, r.trigger) {
                edges.entry(from.clone()).or_default().insert(dest.to_owned());
            }
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
