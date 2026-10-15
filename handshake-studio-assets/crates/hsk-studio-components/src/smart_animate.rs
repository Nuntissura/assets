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
//! Gradient fills (lane SMB-FIG-SMART-ANIMATE-2, Studio reading, Figma parity UNVERIFIED): two
//! gradients of the same kind with the same stop count tween stop by stop (position and RGBA
//! colour) and their handle transforms entry by entry; with different stop counts both are
//! resampled at the union of their stop positions (piecewise-linear colour, ends held) and the
//! colours tween; a solid fill against a gradient acts as a uniform gradient with the other side's
//! kind, positions and transform; gradients of different kinds (linear vs radial) do not tween and
//! are cross-dissolved like changed content.
//!
//! Text by content (lane SMB-FIG-SMART-ANIMATE-2, a Studio reading, OFF by default because
//! STU-PRO-117 and Figma's rule pair explicitly named layers by name only): with
//! [`SmartMatchOptions::text_content_min_similarity`] set, text layers still unmatched after the
//! name / style pass pair when they sit under the same ancestor path (equal ancestor keys, so the
//! same parents up to the top-level frame) and their content similarity (1 - Levenshtein distance /
//! longer length, over Unicode scalars, the first [`MAX_TEXT_COMPARE`] of each; two empty texts
//! score 1) is at least the threshold; pairs are taken greedily by highest similarity, then nearest
//! x/y, then source and destination order. At most [`MAX_TEXT_PAIR_EVALUATIONS`] candidate pairs
//! are scored; the rest stay unmatched and are counted in [`SmartMatch::text_pairs_not_scored`].
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
/// Most stops of one gradient.
pub const MAX_GRADIENT_STOPS: usize = 256;
/// Unicode scalars of each text compared by the content-similarity rule.
pub const MAX_TEXT_COMPARE: usize = 512;
/// Most candidate text pairs the content-similarity rule scores per match.
pub const MAX_TEXT_PAIR_EVALUATIONS: usize = 4096;

/// A gradient fill: the stored kind (`GRADIENT_LINEAR`, ...), stops `(position 0..=1, RGBA)` in
/// order, and the handle transform `[m00, m01, m02, m10, m11, m12]`.
#[derive(Clone, Debug, PartialEq)]
pub struct SmartGradient {
    pub kind: String,
    pub stops: Vec<(f64, [f64; 4])>,
    pub transform: [f64; 6],
}

/// Matching options. The default is the spec rule (STU-PRO-117) only.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SmartMatchOptions {
    /// `Some(threshold)` turns on the Studio text-by-content rule (module text), `0..=1`.
    pub text_content_min_similarity: Option<f64>,
}

/// The tweened properties of one layer. `x`/`y` are relative to the parent layer (or the frame),
/// `rotation_deg` in degrees, `fill` straight RGBA 0..=1 of the layer's first solid fill,
/// `gradient` its first gradient fill (a layer carries at most one of the two).
#[derive(Clone, Debug, PartialEq)]
pub struct SmartProps {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub rotation_deg: f64,
    pub opacity: f64,
    pub fill: Option<[f64; 4]>,
    pub corner_radius: f64,
    pub gradient: Option<SmartGradient>,
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
    /// A text layer (`content` holds its characters); used by the text-by-content rule.
    pub is_text: bool,
    pub props: SmartProps,
}

/// The layers of one top-level frame.
#[derive(Clone, Debug, PartialEq)]
pub struct SmartFrame {
    pub frame_id: String,
    pub layers: Vec<SmartLayer>,
}

/// The pairing of two frames.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SmartMatch {
    /// (source layer id, destination layer id), in destination order.
    pub pairs: Vec<(String, String)>,
    /// Source layers without a counterpart, in source order.
    pub from_only: Vec<String>,
    /// Destination layers without a counterpart, in destination order.
    pub to_only: Vec<String>,
    /// The subset of `pairs` made by the text-by-content rule, with their similarity.
    pub text_similarity_pairs: Vec<(String, String, f64)>,
    /// Candidate text pairs left unscored by [`MAX_TEXT_PAIR_EVALUATIONS`].
    pub text_pairs_not_scored: usize,
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
    if let Some(g) = &p.gradient {
        if g.stops.is_empty() || g.stops.len() > MAX_GRADIENT_STOPS {
            return false;
        }
        v.extend_from_slice(&g.transform);
        for (pos, c) in &g.stops {
            v.push(*pos);
            v.extend_from_slice(c);
        }
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
    match_layers_with(from, to, &SmartMatchOptions::default())
}

/// Levenshtein similarity `1 - d / max(len)` over the first [`MAX_TEXT_COMPARE`] scalars.
pub fn text_similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().take(MAX_TEXT_COMPARE).collect();
    let b: Vec<char> = b.chars().take(MAX_TEXT_COMPARE).collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len().saturating_add(1)];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i.saturating_add(1);
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j].saturating_add(usize::from(ca != cb));
            cur[j.saturating_add(1)] = sub.min(prev[j.saturating_add(1)].saturating_add(1)).min(cur[j].saturating_add(1));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - prev[b.len()] as f64 / longest as f64
}

/// [`match_layers`] with options (the Studio text-by-content rule, module text).
pub fn match_layers_with(from: &SmartFrame, to: &SmartFrame, opts: &SmartMatchOptions) -> R<SmartMatch> {
    if let Some(t) = opts.text_content_min_similarity
        && !(0.0..=1.0).contains(&t)
    {
        return Err(E::InvalidValue("text_content_min_similarity".into()));
    }
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
    let mut text_similarity_pairs = Vec::new();
    let mut text_pairs_not_scored = 0usize;
    if let Some(min) = opts.text_content_min_similarity {
        let parent_key = |k: &[String]| k[..k.len().saturating_sub(1)].to_vec();
        let matched_to: BTreeSet<String> = pairs.iter().map(|(_, t)| t.clone()).collect();
        let free_a: Vec<(usize, &SmartLayer)> = from
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.is_text && !used.contains(l.id.as_str()))
            .collect();
        // (similarity, distance, source order, destination order)
        let mut scored: Vec<(f64, f64, usize, usize)> = Vec::new();
        let mut evaluated = 0usize;
        for (bi, l) in to.layers.iter().enumerate().filter(|(_, l)| l.is_text && !matched_to.contains(&l.id)) {
            let pk = parent_key(&b.key[&l.id]);
            for (ai, c) in &free_a {
                if parent_key(&a.key[&c.id]) != pk {
                    continue;
                }
                if evaluated >= MAX_TEXT_PAIR_EVALUATIONS {
                    text_pairs_not_scored = text_pairs_not_scored.saturating_add(1);
                    continue;
                }
                evaluated = evaluated.saturating_add(1);
                let sim = text_similarity(c.content.as_deref().unwrap_or(""), l.content.as_deref().unwrap_or(""));
                if sim >= min {
                    let (dx, dy) = (c.props.x - l.props.x, c.props.y - l.props.y);
                    scored.push((sim, dx * dx + dy * dy, *ai, bi));
                }
            }
        }
        scored.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.total_cmp(&y.1)).then(x.2.cmp(&y.2)).then(x.3.cmp(&y.3)));
        let mut taken_b = BTreeSet::new();
        for (sim, _, ai, bi) in scored {
            let (fa, tb) = (&from.layers[ai].id, &to.layers[bi].id);
            if used.contains(fa.as_str()) || !taken_b.insert(bi) {
                continue;
            }
            used.insert(fa.as_str());
            pairs.push((fa.clone(), tb.clone()));
            text_similarity_pairs.push((fa.clone(), tb.clone(), sim));
        }
        // Keep `pairs` in destination order.
        let pos: BTreeMap<&str, usize> = b.order.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
        pairs.sort_by_key(|(_, t)| pos.get(t.as_str()).copied().unwrap_or(usize::MAX));
    }
    let to_matched: BTreeSet<&str> = pairs.iter().map(|(_, t)| t.as_str()).collect();
    Ok(SmartMatch {
        from_only: a.order.iter().filter(|id| !used.contains(id.as_str())).cloned().collect(),
        to_only: b.order.iter().filter(|id| !to_matched.contains(id.as_str())).cloned().collect(),
        pairs,
        text_similarity_pairs,
        text_pairs_not_scored,
    })
}

fn lerp(a: f64, b: f64, e: f64) -> f64 {
    a + (b - a) * e
}

/// Colour of a gradient at `t` (piecewise linear between stops, ends held).
fn sample(g: &SmartGradient, t: f64) -> [f64; 4] {
    let Some(first) = g.stops.first() else { return [0.0; 4] };
    if t <= first.0 {
        return first.1;
    }
    for w in g.stops.windows(2) {
        let ((p0, c0), (p1, c1)) = (w[0], w[1]);
        if t <= p1 {
            let k = if p1 > p0 { (t - p0) / (p1 - p0) } else { 1.0 };
            return [0, 1, 2, 3].map(|i| lerp(c0[i], c1[i], k));
        }
    }
    g.stops.last().map_or([0.0; 4], |s| s.1)
}

/// Whether two gradient fills can tween (same kind, or one side solid).
pub fn gradients_tween(a: &SmartProps, b: &SmartProps) -> bool {
    match (&a.gradient, &b.gradient) {
        (Some(x), Some(y)) => x.kind == y.kind,
        _ => true,
    }
}

fn tween_gradient(a: &SmartProps, b: &SmartProps, e: f64) -> Option<SmartGradient> {
    let uniform = |c: [f64; 4], like: &SmartGradient| SmartGradient {
        kind: like.kind.clone(),
        stops: like.stops.iter().map(|(p, _)| (*p, c)).collect(),
        transform: like.transform,
    };
    let clear = [0.0, 0.0, 0.0, 0.0];
    let (x, y) = match (&a.gradient, &b.gradient) {
        (None, None) => return None,
        (Some(x), Some(y)) if x.kind != y.kind => return Some(y.clone()),
        (Some(x), Some(y)) => (x.clone(), y.clone()),
        (Some(x), None) => (x.clone(), uniform(b.fill.unwrap_or(clear), x)),
        (None, Some(y)) => (uniform(a.fill.unwrap_or(clear), y), y.clone()),
    };
    let ch = |c: [f64; 4], d: [f64; 4]| [0, 1, 2, 3].map(|i| lerp(c[i], d[i], e).clamp(0.0, 1.0));
    let stops = if x.stops.len() == y.stops.len() {
        x.stops.iter().zip(&y.stops).map(|((p, c), (q, d))| (lerp(*p, *q, e).clamp(0.0, 1.0), ch(*c, *d))).collect()
    } else {
        let mut at: Vec<f64> = x.stops.iter().chain(&y.stops).map(|s| s.0).collect();
        at.sort_by(f64::total_cmp);
        at.dedup_by(|p, q| (*p - *q).abs() <= 1e-9);
        at.into_iter().map(|t| (t, ch(sample(&x, t), sample(&y, t)))).collect()
    };
    Some(SmartGradient {
        kind: y.kind.clone(),
        stops,
        transform: [0, 1, 2, 3, 4, 5].map(|i| lerp(x.transform[i], y.transform[i], e)),
    })
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
    let gradient = tween_gradient(a, b, e);
    let fill = if gradient.is_some() { None } else { match (a.fill, b.fill) {
        (Some(x), Some(y)) => Some([0, 1, 2, 3].map(|i| lerp(x[i], y[i], e).clamp(0.0, 1.0))),
        // A fill that appears or disappears fades its alpha.
        (None, Some(y)) => Some([y[0], y[1], y[2], (y[3] * e).clamp(0.0, 1.0)]),
        (Some(x), None) => Some([x[0], x[1], x[2], (x[3] * (1.0 - e)).clamp(0.0, 1.0)]),
        (None, None) => None,
    } };
    SmartProps {
        x: lerp(a.x, b.x, e),
        y: lerp(a.y, b.y, e),
        width: lerp(a.width, b.width, e).max(0.0),
        height: lerp(a.height, b.height, e).max(0.0),
        rotation_deg: a.rotation_deg + d * e,
        opacity: lerp(a.opacity, b.opacity, e).clamp(0.0, 1.0),
        fill,
        corner_radius: lerp(a.corner_radius, b.corner_radius, e).max(0.0),
        gradient,
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
        let mut props = l.props.clone();
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
                if src.content != l.content || !gradients_tween(&src.props, &l.props) {
                    let mut out_props = if gradients_tween(&src.props, &l.props) {
                        props.clone()
                    } else {
                        // The outgoing layer keeps its own fill while it dissolves out.
                        SmartProps { gradient: src.props.gradient.clone(), fill: src.props.fill, ..props.clone() }
                    };
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
                let mut props = l.props.clone();
                if l.parent.as_deref().is_none_or(|p| !to_only.contains(p)) {
                    props.opacity *= ed;
                }
                out.push(SmartDrawLayer { id: l.id.clone(), side: SmartSide::ToOnly, props });
            }
        }
    }
    Ok(out)
}
