//! Prototype page and frame records (lane SMB-INTEROP-FIG-PROTOTYPE): the STU-PRO-128 flow
//! starting points and STU-PRO-129 default entry frame of a page, and the STU-PRO-138
//! `frame.set_overflow_direction` value of a frame. They live in [`crate::StudioDesignRecords`]
//! (`prototype_pages`, `frame_overflow`), keyed by node, because pages and frames are document
//! nodes whose structs are built by literal across the workspace. Both members are serde-default and
//! skipped when empty, so a record set without them encodes byte-identically to before (test
//! `prototype_records_contract::empty_members_keep_records_byte_identical`).
//!
//! Declared gaps, not invented here: the prototype background paints of STU-PRO-129 and the
//! fixed-children count of STU-PRO-138 are not stored yet; STU-PRO-128 names the page as the
//! carrier but a Studio page is any node the caller treats as one (a root `group` layer for imported
//! Figma pages, an artboard, or a page spread), so the record names a node.
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, NodeRef, Result, err, values::{Ctx, label}};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One flow starting point (STU-PRO-128): the frame a flow starts at and the flow's name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFlowStart {
    pub node_id: String,
    pub name: String,
}

/// The prototype fields of one page (STU-PRO-128/129). Order of `flow_starting_points` is
/// authority (the flow sidebar order); a node id appearing twice is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPrototypePage {
    pub page: NodeRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flow_starting_points: Vec<StudioFlowStart>,
    /// STU-PRO-129 default entry frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prototype_start_node_id: Option<String>,
}

/// STU-PRO-138 `frame.set_overflow_direction`: how a frame scrolls in a running prototype.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StudioOverflowDirection {
    None,
    Horizontal,
    Vertical,
    Both,
}

/// The overflow (scroll) behaviour of one frame node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFrameOverflow {
    pub node: NodeRef,
    pub overflow_direction: StudioOverflowDirection,
}

/// Checks the prototype members against the document node index (`node id -> kind`): every page,
/// flow start and start node exists; a flow start node id appears once per page; names are labels;
/// one overflow entry per frame node of the right kind.
pub(crate) fn check_prototype(
    pages: &[StudioPrototypePage],
    overflow: &[StudioFrameOverflow],
    nodes: &BTreeMap<&str, crate::NodeKind>,
    c: &mut Ctx,
) -> Result<()> {
    let exists = |r: &NodeRef| nodes.get(r.node_id.as_str()) == Some(&r.node_kind);
    let mut seen_pages = BTreeSet::new();
    for p in pages {
        c.tick()?;
        c.nodes(1, &p.page.node_id)?;
        if !exists(&p.page) {
            return Err(err(Code::DanglingReference, &p.page.node_id));
        }
        if !seen_pages.insert(p.page.node_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &p.page.node_id));
        }
        let mut seen = BTreeSet::new();
        for f in &p.flow_starting_points {
            c.nodes(1, &f.node_id)?;
            c.text(&f.name, &f.node_id)?;
            if !nodes.contains_key(f.node_id.as_str()) {
                return Err(err(Code::DanglingReference, &f.node_id));
            }
            if !label(&f.name) {
                return Err(err(Code::InvalidField, "flow_starting_points/name"));
            }
            if !seen.insert(f.node_id.as_str()) {
                return Err(err(Code::DuplicateIdentity, &f.node_id));
            }
        }
        if let Some(s) = &p.prototype_start_node_id
            && !nodes.contains_key(s.as_str())
        {
            return Err(err(Code::DanglingReference, s));
        }
    }
    let mut seen = BTreeSet::new();
    for o in overflow {
        c.tick()?;
        c.nodes(1, &o.node.node_id)?;
        if !exists(&o.node) {
            return Err(err(Code::DanglingReference, &o.node.node_id));
        }
        if !seen.insert(o.node.node_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &o.node.node_id));
        }
    }
    Ok(())
}

/// STU-PRO-120 named easing presets: Studio-declared four-number tuples over the cubic-bezier
/// shape of STU-PRO-119 (the record stores the tuple, never the name). Values are the common CSS /
/// easings.net curves; whether Figma's same-named presets use exactly these numbers is UNVERIFIED
/// (oracle D5). The four spring presets have no published parameter set here and are not listed.
pub const EASING_PRESETS: [(&str, crate::CubicBezier); 7] = [
    ("LINEAR", crate::CubicBezier { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 }),
    ("EASE_IN", crate::CubicBezier { x1: 0.42, y1: 0.0, x2: 1.0, y2: 1.0 }),
    ("EASE_OUT", crate::CubicBezier { x1: 0.0, y1: 0.0, x2: 0.58, y2: 1.0 }),
    ("EASE_IN_AND_OUT", crate::CubicBezier { x1: 0.42, y1: 0.0, x2: 0.58, y2: 1.0 }),
    ("EASE_IN_BACK", crate::CubicBezier { x1: 0.36, y1: 0.0, x2: 0.66, y2: -0.56 }),
    ("EASE_OUT_BACK", crate::CubicBezier { x1: 0.34, y1: 1.56, x2: 0.64, y2: 1.0 }),
    ("EASE_IN_AND_OUT_BACK", crate::CubicBezier { x1: 0.68, y1: -0.6, x2: 0.32, y2: 1.6 }),
];

/// The tuple of a named preset of [`EASING_PRESETS`].
pub fn easing_preset(name: &str) -> Option<crate::CubicBezier> {
    EASING_PRESETS.iter().find(|(n, _)| *n == name).map(|(_, b)| b.clone())
}
