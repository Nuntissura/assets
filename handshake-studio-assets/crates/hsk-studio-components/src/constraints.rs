//! Responsive constraints (STU-DS-151/152): `MIN | CENTER | MAX | STRETCH | SCALE` on both axes.
//!
//! All values are points in the parent's local coordinate space. Resizing is a pure function of the
//! old rectangle, the old parent size and the new parent size, so it is deterministic and composes
//! down a hierarchy: a child's new size is the parent size for its own children.
use crate::{ComponentDef, ComponentsError as E, R};
use hsk_studio_accord::CancellationToken;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConstraintKind {
    /// Pin to left / top.
    Min,
    /// Keep centred, preserve size.
    Center,
    /// Pin to right / bottom.
    Max,
    /// Pin both edges, resize.
    Stretch,
    /// Keep position and size as the same fraction of the parent.
    Scale,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Constraints {
    pub horizontal: ConstraintKind,
    pub vertical: ConstraintKind,
}
impl Default for Constraints {
    /// STU-DS-152 default: top-left.
    fn default() -> Self {
        Self {
            horizontal: ConstraintKind::Min,
            vertical: ConstraintKind::Min,
        }
    }
}

/// Rectangle (parent-local, at the component's authored size) plus its constraints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeGeometry {
    pub rect: RectPt,
    pub constraints: Constraints,
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
fn axis(
    pos: f64,
    size: f64,
    old: f64,
    new: f64,
    kind: ConstraintKind,
) -> Result<(f64, f64), E> {
    let (p, s) = match kind {
        ConstraintKind::Min => (pos, size),
        // Distance to the far edge is kept: pos' = new - (old - pos - size) - size.
        ConstraintKind::Max => (pos + (new - old), size),
        // Offset of the centre from the parent centre is kept: pos' = pos + (new - old) / 2.
        ConstraintKind::Center => (pos + (new - old) / 2.0, size),
        // Both edge offsets are kept: size' = size + (new - old).
        ConstraintKind::Stretch => (pos, size + (new - old)),
        ConstraintKind::Scale => {
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
    c: Constraints,
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

/// Resizes the component definition to `new_size` and places every node that carries geometry
/// (definition order, parents before children). The definition is never changed. A node whose
/// parent has no geometry, or a definition without `size`, is refused.
pub fn resize_component(
    def: &ComponentDef,
    new_size: SizePt,
    cancel: &CancellationToken,
) -> R<Vec<PlacedNode>> {
    let root = def.size.ok_or(E::InvalidGeometry)?;
    let mut done: BTreeMap<&str, (SizePt, SizePt)> = BTreeMap::new();
    let mut out = Vec::new();
    for n in &def.nodes {
        cancel.check().map_err(|_| E::Canceled)?;
        let Some(g) = n.geometry else { continue };
        let (old_parent, new_parent) = match &n.parent {
            None => (root, new_size),
            Some(p) => *done.get(p.as_str()).ok_or(E::InvalidGeometry)?,
        };
        let rect = apply_constraints(g.rect, old_parent, new_parent, g.constraints)?;
        done.insert(
            n.node_id.as_str(),
            (
                SizePt {
                    w: g.rect.w,
                    h: g.rect.h,
                },
                SizePt { w: rect.w, h: rect.h },
            ),
        );
        out.push(PlacedNode {
            node_id: n.node_id.clone(),
            rect,
        });
    }
    Ok(out)
}
