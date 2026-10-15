//! Smart animate (STU-PRO-117, STU-PRO-018; lane SMB-FIG-SMART-ANIMATE): the layer-matching tween
//! between the screen a `SMART_ANIMATE` transition leaves and the screen it opens. Pure functions
//! over two [`SmartFrame`] snapshots (one per top-level frame) that a host builds from whatever
//! carries the geometry (the harness builds them from an imported `.fig`).
//!
//! Matching ([`match_layers`]), the Studio reading of Figma's published rule (help.figma.com
//! articles 360039818874 "Smart animate layers between frames" and 21523793229463 "Identify
//! matching objects", read 2026-10-15; Figma parity UNVERIFIED, oracle D5):
//! - a layer matches when its name, and the names of all its ancestors up to (not including) the
//!   top-level frame, are equal and it sits at the same depth (the key is the name path; the
//!   top-level frame names need not match);
//! - among siblings with the same name the index among those same-named siblings decides
//!   ("the index of the object within the parent frame");
//! - a text layer named implicitly from its content ([`SmartLayer::implicit_text_style`]) matches by
//!   its text style instead of its name, and when several candidates share the style under the same
//!   ancestors the nearest by x/y wins (Studio: greedy in destination order, squared distance, ties
//!   to the earlier source layer);
//! - the spec pairing of STU-PRO-117 is the same (layer name, position in the child hierarchy) rule
//!   as override matching (STU-DS-119).
//!
//! Interpolation ([`interpolate`]): position, size, rotation (shortest arc), opacity, fill colour
//! (RGBA) and corner radius move linearly in the EASED progress of the transition easing (so the
//! curve is the transition's cubic bezier). Eased progress outside 0..=1 (back easings) extrapolates
//! position, size, rotation and radius; opacity and colour channels are clamped to 0..=1, size and
//! radius to >= 0. Unmatched layers dissolve: a source-only layer at `opacity * (1 - e)`, a
//! destination-only layer at `opacity * e` (e clamped to 0..=1), applied to the outermost unmatched
//! layer only because descendants inherit a parent's opacity. A matched pair whose content differs
//! (text characters, image identity: [`SmartLayer::content`]) is drawn twice at the interpolated
//! properties, the source content dissolving out under the destination content dissolving in.
//! Draw order ([`smart_animate_frame`]): source-only layers first (beneath), then the destination
//! frame's layers in its order, each content-out copy directly beneath its destination layer.
//!
//! Bounds: at most [`MAX_SMART_LAYERS`] layers per frame and [`MAX_SMART_DEPTH`] ancestors per layer;
//! a parent id that is not a layer of the frame, a parent cycle or a non-finite property is a typed
//! refusal.
#![deny(clippy::arithmetic_side_effects)]
use crate::{ComponentsError as E, R};
use std::collections::{BTreeMap, BTreeSet};

/// Most layers one frame snapshot may carry.
pub const MAX_SMART_LAYERS: usize = 65_536;
/// Deepest ancestor chain of one layer.
pub const MAX_SMART_DEPTH: usize = 256;

/// The tweened properties of one layer. `x`/`y` are relative to the parent layer (or the frame),
/// `rotation_deg` in degrees, `fill` straight RGBA 0..=1 of the layer's first solid fill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SmartProps {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub rotation_deg: f64,
    pub opacity: f64,
    pub fill: Option<[f64; 4]>,
    pub corner_radius: f64,
}

/// One layer of a frame snapshot. Sibling order is the order of the layers in [`SmartFrame::layers`].
#[derive(Clone, Debug, PartialEq)]
pub struct SmartLayer {
    pub id: String,
    pub name: String,
    /// Parent layer id inside the same frame; `None` is a direct child of the top-level frame.
    pub parent: Option<String>,
    /// `Some(style key)` for a text layer named implicitly from its content: it matches by style.
    pub implicit_text_style: Option<String>,
    /// Content identity (text characters, image hash); matched layers with different content
    /// cross-dissolve in place.
    pub content: Option<String>,
    pub props: SmartProps,
}

/// The layers of one top-level frame.
#[derive(Clone, Debug, PartialEq)]
pub struct SmartFrame {
    pub frame_id: String,
    pub layers: Vec<SmartLayer>,
}

/// The pairing of two frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SmartMatch {
    /// (source layer id, destination layer id), in destination order.
    pub pairs: Vec<(String, String)>,
    /// Source layers without a counterpart, in source order.
    pub from_only: Vec<String>,
    /// Destination layers without a counterpart, in destination order.
    pub to_only: Vec<String>,
}

/// Which side a drawn layer comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmartSide {
    /// A matched pair drawn as the destination layer at interpolated properties.
    Matched { from: String },
    /// The source content of a matched pair whose content differs, dissolving out.
    ContentOut { from: String },
    FromOnly,
    ToOnly,
}

/// One layer to draw at a progress point.
#[derive(Clone, Debug, PartialEq)]
pub struct SmartDrawLayer {
    /// The layer id on its own side (source id for `FromOnly`/`ContentOut`).
    pub id: String,
    pub side: SmartSide,
    pub props: SmartProps,
}

fn finite(p: &SmartProps) -> bool {
    let mut v = vec![p.x, p.y, p.width, p.height, p.rotation_deg, p.opacity, p.corner_radius];
    if let Some(f) = p.fill {
        v.extend_from_slice(&f);
    }
    v.into_iter().all(f64::is_finite)
}

/// Per layer: its key (ancestor name path + own component) and its depth-ordered ancestors.
struct Keys {
    /// Layer id -> match key. Implicit text layers carry the style marker and no index.
    key: BTreeMap<String, Vec<String>>,
    order: Vec<String>,
    by_id: BTreeMap<String, usize>,
}

fn keys(f: &SmartFrame) -> R<Keys> {
    if f.layers.len() > MAX_SMART_LAYERS {
        return Err(E::BudgetExceeded);
    }
    let mut by_id = BTreeMap::new();
    for (i, l) in f.layers.iter().enumerate() {
        if !finite(&l.props) {
            return Err(E::InvalidValue(format!("smart_props@{}", l.id)));
        }
        if by_id.insert(l.id.clone(), i).is_some() {
            return Err(E::Duplicate(l.id.clone()));
        }
    }
    // Own component: name and index among same-named siblings (explicit names), or the style
    // marker (implicit text).
    let mut seen: BTreeMap<(Option<&str>, &str), usize> = BTreeMap::new();
    let mut own: Vec<String> = Vec::with_capacity(f.layers.len());
    for l in &f.layers {
        if let Some(p) = &l.parent
            && !by_id.contains_key(p)
        {
            return Err(E::NoSuchNode(p.clone()));
        }
        match &l.implicit_text_style {
            Some(style) => own.push(format!("text-style:{style}")),
            None => {
                let n = seen.entry((l.parent.as_deref(), l.name.as_str())).or_insert(0);
                own.push(format!("name:{}#{}", l.name, n));
                *n = n.saturating_add(1);
            }
        }
    }
    let mut key = BTreeMap::new();
    for (i, l) in f.layers.iter().enumerate() {
        let mut path = vec![own[i].clone()];
        let mut cur = l.parent.as_deref();
        let mut depth = 0usize;
        while let Some(p) = cur {
            depth = depth.saturating_add(1);
            if depth > MAX_SMART_DEPTH {
                return Err(E::InvalidValue(format!("smart_parent_depth@{}", l.id)));
            }
            let pi = by_id[p];
            path.push(own[pi].clone());
            cur = f.layers[pi].parent.as_deref();
        }
        path.reverse();
        key.insert(l.id.clone(), path);
    }
    Ok(Keys { key, order: f.layers.iter().map(|l| l.id.clone()).collect(), by_id })
}

/// Pairs the layers of `from` and `to` (see the module rule).
pub fn match_layers(from: &SmartFrame, to: &SmartFrame) -> R<SmartMatch> {
    let a = keys(from)?;
    let b = keys(to)?;
    // Explicit keys are unique per frame (name + index); implicit-text keys may repeat.
    let mut explicit: BTreeMap<&[String], &str> = BTreeMap::new();
    let mut implicit: BTreeMap<&[String], Vec<&str>> = BTreeMap::new();
    for id in &a.order {
        let k = a.key[id].as_slice();
        if from.layers[a.by_id[id]].implicit_text_style.is_some() {
            implicit.entry(k).or_default().push(id);
        } else {
            explicit.insert(k, id);
        }
    }
    let mut used: BTreeSet<&str> = BTreeSet::new();
    let mut pairs = Vec::new();
    for id in &b.order {
        let l = &to.layers[b.by_id[id]];
        let k = b.key[id].as_slice();
        let hit = if l.implicit_text_style.is_some() {
            implicit.get(k).and_then(|cands| {
                let mut best: Option<(&str, f64)> = None;
                for c in cands.iter().filter(|c| !used.contains(**c)) {
                    let p = &from.layers[a.by_id[*c]].props;
                    let (dx, dy) = (p.x - l.props.x, p.y - l.props.y);
                    let d = dx * dx + dy * dy;
                    if best.is_none_or(|(_, bd)| d < bd) {
                        best = Some((c, d));
                    }
                }
                best.map(|(c, _)| c)
            })
        } else {
            explicit.get(k).copied().filter(|c| !used.contains(c))
        };
        if let Some(c) = hit {
            used.insert(c);
            pairs.push((c.to_owned(), id.clone()));
        }
    }
    let to_matched: BTreeSet<&str> = pairs.iter().map(|(_, t)| t.as_str()).collect();
    Ok(SmartMatch {
        from_only: a.order.iter().filter(|id| !used.contains(id.as_str())).cloned().collect(),
        to_only: b.order.iter().filter(|id| !to_matched.contains(id.as_str())).cloned().collect(),
        pairs,
    })
}

fn lerp(a: f64, b: f64, e: f64) -> f64 {
    a + (b - a) * e
}

/// The properties of a matched pair at eased progress `e`.
pub fn interpolate(a: &SmartProps, b: &SmartProps, e: f64) -> SmartProps {
    // Shortest arc: the rotation delta is taken in (-180, 180].
    let mut d = (b.rotation_deg - a.rotation_deg) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    let fill = match (a.fill, b.fill) {
        (Some(x), Some(y)) => Some([0, 1, 2, 3].map(|i| lerp(x[i], y[i], e).clamp(0.0, 1.0))),
        // A fill that appears or disappears fades its alpha.
        (None, Some(y)) => Some([y[0], y[1], y[2], (y[3] * e).clamp(0.0, 1.0)]),
        (Some(x), None) => Some([x[0], x[1], x[2], (x[3] * (1.0 - e)).clamp(0.0, 1.0)]),
        (None, None) => None,
    };
    SmartProps {
        x: lerp(a.x, b.x, e),
        y: lerp(a.y, b.y, e),
        width: lerp(a.width, b.width, e).max(0.0),
        height: lerp(a.height, b.height, e).max(0.0),
        rotation_deg: a.rotation_deg + d * e,
        opacity: lerp(a.opacity, b.opacity, e).clamp(0.0, 1.0),
        fill,
        corner_radius: lerp(a.corner_radius, b.corner_radius, e).max(0.0),
    }
}

/// What to draw at eased progress `e`: interpolated matched layers and dissolving unmatched ones,
/// in draw order (see the module rule). `m` is the result of [`match_layers`] over the same frames.
pub fn smart_animate_frame(from: &SmartFrame, to: &SmartFrame, m: &SmartMatch, e: f64) -> R<Vec<SmartDrawLayer>> {
    if !e.is_finite() {
        return Err(E::InvalidValue("smart_progress".into()));
    }
    let a: BTreeMap<&str, &SmartLayer> = from.layers.iter().map(|l| (l.id.as_str(), l)).collect();
    let pair_of: BTreeMap<&str, &str> = m.pairs.iter().map(|(f, t)| (t.as_str(), f.as_str())).collect();
    let from_only: BTreeSet<&str> = m.from_only.iter().map(String::as_str).collect();
    let to_only: BTreeSet<&str> = m.to_only.iter().map(String::as_str).collect();
    let ed = e.clamp(0.0, 1.0);
    let mut out = Vec::new();
    for id in &m.from_only {
        let Some(l) = a.get(id.as_str()) else {
            return Err(E::NoSuchNode(id.clone()));
        };
        let mut props = l.props;
        // Only the outermost unmatched layer fades: descendants inherit it.
        if l.parent.as_deref().is_none_or(|p| !from_only.contains(p)) {
            props.opacity *= 1.0 - ed;
        }
        out.push(SmartDrawLayer { id: id.clone(), side: SmartSide::FromOnly, props });
    }
    for l in &to.layers {
        match pair_of.get(l.id.as_str()) {
            Some(f) => {
                let Some(src) = a.get(f) else {
                    return Err(E::NoSuchNode((*f).to_owned()));
                };
                let mut props = interpolate(&src.props, &l.props, e);
                if src.content != l.content {
                    let mut out_props = props;
                    out_props.opacity *= 1.0 - ed;
                    props.opacity *= ed;
                    out.push(SmartDrawLayer {
                        id: (*f).to_owned(),
                        side: SmartSide::ContentOut { from: (*f).to_owned() },
                        props: out_props,
                    });
                }
                out.push(SmartDrawLayer { id: l.id.clone(), side: SmartSide::Matched { from: (*f).to_owned() }, props });
            }
            None => {
                if !to_only.contains(l.id.as_str()) {
                    return Err(E::NoSuchNode(l.id.clone()));
                }
                let mut props = l.props;
                if l.parent.as_deref().is_none_or(|p| !to_only.contains(p)) {
                    props.opacity *= ed;
                }
                out.push(SmartDrawLayer { id: l.id.clone(), side: SmartSide::ToOnly, props });
            }
        }
    }
    Ok(out)
}
