//! Figma geometry -> Nib owned paths: node transforms composed down the tree and baked into the
//! points (Folio layers carry no transform), decoded path commands, vector networks. Coordinates
//! are document px; every result is validated by Nib (`OwnedPath::validate`) before it is used.
#![deny(clippy::arithmetic_side_effects)]
use crate::native::{tidy, wide};
use crate::nodes::{Matrix, NetSegment, PathCommand as C, VectorNetwork};
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathLimits, PathPoint};

/// `[m00 m01 m02 m10 m11 m12]`: point -> parent space.
pub(crate) type Affine = [f64; 6];
pub(crate) const IDENTITY: Affine = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

/// `parent` composed with a node's local matrix.
pub(crate) fn compose(parent: &Affine, local: Option<&Matrix>) -> Affine {
    let Some(l) = local else { return *parent };
    let l = [
        wide(l.m00),
        wide(l.m01),
        wide(l.m02),
        wide(l.m10),
        wide(l.m11),
        wide(l.m12),
    ];
    let p = parent;
    [
        p[0] * l[0] + p[1] * l[3],
        p[0] * l[1] + p[1] * l[4],
        p[0] * l[2] + p[1] * l[5] + p[2],
        p[3] * l[0] + p[4] * l[3],
        p[3] * l[1] + p[4] * l[4],
        p[3] * l[2] + p[4] * l[5] + p[5],
    ]
}

pub(crate) fn apply(m: &Affine, x: f64, y: f64) -> PathPoint {
    PathPoint::new(
        tidy(m[0] * x + m[1] * y + m[2]),
        tidy(m[3] * x + m[4] * y + m[5]),
    )
}

/// Figma winding rule token -> Nib fill rule; absent = NONZERO, unknown = `None`.
pub(crate) fn fill_rule(name: Option<&str>) -> Option<FillRule> {
    match name {
        None | Some("NONZERO") => Some(FillRule::NonZero),
        Some("EVENODD") => Some(FillRule::EvenOdd),
        _ => None,
    }
}

/// A node-local straight segment from (0,0) to (`width`,0) (a LINE has no geometry blob).
pub(crate) fn line_path(abs: &Affine, width: f64) -> Option<OwnedPath> {
    if !(width.is_finite() && width > 0.0) {
        return None;
    }
    let mut p = OwnedPath::new(FillRule::NonZero);
    p.elements.push(PathElement::MoveTo(apply(abs, 0.0, 0.0)));
    p.elements.push(PathElement::LineTo(apply(abs, width, 0.0)));
    p.validate(&PathLimits::default()).ok()?;
    Some(p)
}

/// The node-local rectangle (0,0)-(`w`,`h`) as a closed path (a frame without decoded fill
/// geometry); `None` for an empty or non-finite size.
pub(crate) fn rect_path(abs: &Affine, w: f64, h: f64) -> Option<OwnedPath> {
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let mut p = OwnedPath::new(FillRule::NonZero);
    p.elements.push(PathElement::MoveTo(apply(abs, 0.0, 0.0)));
    p.elements.push(PathElement::LineTo(apply(abs, w, 0.0)));
    p.elements.push(PathElement::LineTo(apply(abs, w, h)));
    p.elements.push(PathElement::LineTo(apply(abs, 0.0, h)));
    p.elements.push(PathElement::Close);
    p.validate(&PathLimits::default()).ok()?;
    Some(p)
}

/// Decoded Figma path commands -> Nib path in document px; `None` when Nib refuses the result
/// (non-finite or out-of-range coordinate, structure) or the path is empty.
pub(crate) fn path_from_commands(cmds: &[C], abs: &Affine, rule: FillRule) -> Option<OwnedPath> {
    let pt = |x: f32, y: f32| apply(abs, wide(x), wide(y));
    let mut path = OwnedPath::new(rule);
    for c in cmds {
        path.elements.push(match *c {
            C::MoveTo { x, y } => PathElement::MoveTo(pt(x, y)),
            C::LineTo { x, y } => PathElement::LineTo(pt(x, y)),
            C::QuadTo { x1, y1, x, y } => PathElement::QuadTo(pt(x1, y1), pt(x, y)),
            C::CubicTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => PathElement::CubicTo(pt(x1, y1), pt(x2, y2), pt(x, y)),
            C::Close => PathElement::Close,
        });
    }
    path.validate(&PathLimits::default()).ok()?;
    if path.elements.is_empty() {
        return None;
    }
    Some(path)
}

/// Centerline paths of a vector network (`scale` maps normalized -> node space): every region
/// loop as a closed path (the region's fill rule), every segment outside a region chained into
/// open paths. Used only for VECTOR nodes that carry no decoded fill geometry.
pub(crate) fn network_paths(
    net: &VectorNetwork,
    scale: (f64, f64),
    abs: &Affine,
) -> Vec<OwnedPath> {
    let vert = |i: u32| net.vertices.get(usize::try_from(i).ok()?);
    let seg = |i: u32| -> Option<&NetSegment> { net.segments.get(usize::try_from(i).ok()?) };
    let at = |x: f32, y: f32| apply(abs, wide(x) * scale.0, wide(y) * scale.1);
    let chain = |idx: &[u32], closed: bool, rule: FillRule| -> Option<OwnedPath> {
        let first = seg(*idx.first()?)?;
        let mut cur = match idx.get(1).and_then(|i| seg(*i)) {
            Some(s2) if first.start == s2.start || first.start == s2.end => first.end,
            _ => first.start,
        };
        let mut path = OwnedPath::new(rule);
        let v0 = vert(cur)?;
        path.elements.push(PathElement::MoveTo(at(v0.x, v0.y)));
        for i in idx {
            let s = seg(*i)?;
            let (from, to, t_from, t_to) = if s.start == cur {
                (s.start, s.end, s.tangent_start, s.tangent_end)
            } else if s.end == cur {
                (s.end, s.start, s.tangent_end, s.tangent_start)
            } else {
                return None;
            };
            let (a, b) = (vert(from)?, vert(to)?);
            let flat = t_from.x == 0.0 && t_from.y == 0.0 && t_to.x == 0.0 && t_to.y == 0.0;
            let end = at(b.x, b.y);
            path.elements.push(if flat {
                PathElement::LineTo(end)
            } else {
                PathElement::CubicTo(
                    at(a.x + t_from.x, a.y + t_from.y),
                    at(b.x + t_to.x, b.y + t_to.y),
                    end,
                )
            });
            cur = to;
        }
        if closed {
            path.elements.push(PathElement::Close);
        }
        path.validate(&PathLimits::default()).ok()?;
        Some(path)
    };
    let mut out = Vec::new();
    let mut used = vec![false; net.segments.len()];
    for r in &net.regions {
        let rule = if r.nonzero {
            FillRule::NonZero
        } else {
            FillRule::EvenOdd
        };
        for l in &r.loops {
            for i in l {
                if let Some(u) = usize::try_from(*i).ok().and_then(|i| used.get_mut(i)) {
                    *u = true;
                }
            }
            out.extend(chain(l, true, rule));
        }
    }
    for start in 0..net.segments.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut idx = vec![u32::try_from(start).unwrap_or(0)];
        let mut end = net.segments[start].end;
        'grow: loop {
            for (j, s) in net.segments.iter().enumerate() {
                if !used[j] && (s.start == end || s.end == end) {
                    used[j] = true;
                    idx.push(u32::try_from(j).unwrap_or(0));
                    end = if s.start == end { s.end } else { s.start };
                    continue 'grow;
                }
            }
            break;
        }
        out.extend(chain(&idx, false, FillRule::NonZero));
    }
    out
}
