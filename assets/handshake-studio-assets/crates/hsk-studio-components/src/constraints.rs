//! Responsive constraints (STU-DS-151/152): `MIN | CENTER | MAX | STRETCH | SCALE` on both axes,
//! read from Folio's `StudioConstraint` records.
//!
//! All values are in the parent's local coordinate space and in the document's geometry unit
//! (points for a `pt` document). Resizing is a pure function of the old rectangle, the old parent
//! size and the new parent size, so it is deterministic and composes down a hierarchy: a child's new
//! size is the parent size for its own children.
use super::design::Design;
use crate::{ComponentsError as E, R};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{ConstraintType, StudioConstraint};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectPt {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizePt {
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlacedNode {
    pub node_id: String,
    pub rect: RectPt,
}

fn finite_size(s: f64) -> bool {
    s.is_finite() && s >= 0.0
}

/// One axis: `(position, size)` of the child after the parent goes from `old` to `new` points.
fn axis(pos: f64, size: f64, old: f64, new: f64, kind: ConstraintType) -> Result<(f64, f64), E> {
    let (p, s) = match kind {
        ConstraintType::Min => (pos, size),
        // Distance to the far edge is kept: pos' = new - (old - pos - size) - size.
        ConstraintType::Max => (pos + (new - old), size),
        // Offset of the centre from the parent centre is kept: pos' = pos + (new - old) / 2.
        ConstraintType::Center => (pos + (new - old) / 2.0, size),
        // Both edge offsets are kept: size' = size + (new - old).
        ConstraintType::Stretch => (pos, size + (new - old)),
        ConstraintType::Scale => {
            if old <= 0.0 {
                return Err(E::InvalidGeometry);
            }
            (pos * new / old, size * new / old)
        }
    };
    // `+ 0.0` normalises -0.0; a collapsed or non-finite result is refused, never clamped.
    if !p.is_finite() || !s.is_finite() || s < 0.0 {
        return Err(E::InvalidGeometry);
    }
    Ok((p + 0.0, s + 0.0))
}

/// Applies `c` to `child` when its parent goes from `old_parent` to `new_parent`.
pub fn apply_constraints(
    child: RectPt,
    old_parent: SizePt,
    new_parent: SizePt,
    c: &StudioConstraint,
) -> R<RectPt> {
    if !(child.x.is_finite() && child.y.is_finite())
        || !finite_size(child.w)
        || !finite_size(child.h)
        || !finite_size(old_parent.w)
        || !finite_size(old_parent.h)
        || !finite_size(new_parent.w)
        || !finite_size(new_parent.h)
    {
        return Err(E::InvalidGeometry);
    }
    let (x, w) = axis(child.x, child.w, old_parent.w, new_parent.w, c.horizontal)?;
    let (y, h) = axis(child.y, child.h, old_parent.h, new_parent.h, c.vertical)?;
    Ok(RectPt { x, y, w, h })
}

/// Resizes the component definition `component_id` (a set id names its default variant) to
/// `new_size` and places every definition node that carries an authored rectangle (document order,
/// parents before children). A node's constraint is its Folio `StudioConstraint` record, else the
/// STU-DS-152 default (top-left). Nothing is changed. A definition whose container layer has no
/// rectangle (the component size), or a node whose parent has none, is refused.
pub fn resize_component(
    design: &Design,
    component_id: &str,
    new_size: SizePt,
    cancel: &CancellationToken,
) -> R<Vec<PlacedNode>> {
    let component = design
        .component(component_id)
        .ok_or_else(|| E::NoSuchComponent(component_id.to_owned()))?;
    let root = design
        .local()
        .nodes
        .get(&component.layer_id)
        .and_then(|n| n.rect)
        .map(|r| SizePt { w: r.w, h: r.h })
        .ok_or(E::InvalidGeometry)?;
    let default = StudioConstraint::top_left();
    let mut done: BTreeMap<&str, (SizePt, SizePt)> = BTreeMap::new();
    let mut out = Vec::new();
    for n in design.definition_nodes(component) {
        cancel.check().map_err(|_| E::Canceled)?;
        let Some(rect) = n.local.and_then(|l| l.rect) else {
            continue;
        };
        let constraint = n
            .design
            .and_then(|d| d.constraints.as_ref())
            .unwrap_or(&default);
        let (old_parent, new_parent) = match n.parent {
            None => (root, new_size),
            Some(p) => *done.get(p).ok_or(E::InvalidGeometry)?,
        };
        let placed = apply_constraints(rect, old_parent, new_parent, constraint)?;
        done.insert(
            n.node_id,
            (
                SizePt {
                    w: rect.w,
                    h: rect.h,
                },
                SizePt {
                    w: placed.w,
                    h: placed.h,
                },
            ),
        );
        out.push(PlacedNode {
            node_id: n.node_id.to_owned(),
            rect: placed,
        });
    }
    Ok(out)
}
