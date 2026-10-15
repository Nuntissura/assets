//! Prototype play mode (lane SMB-HARNESS-PROTOTYPE-PLAY; STU-PRO-132/138/139): a Figma `.fig` is
//! imported (`hsk-studio-interop-fig`), its first flow starting point opens a
//! `hsk-studio-components` [`PlayerSession`], pointer clicks and hover are hit-tested against the
//! current screen's layers and dispatched as triggers, and the screen is rendered through the
//! interop-fig PDF frame export rendered back by press, with running transitions drawn as an eased
//! cross-fade between the two screens.
//!
//! Smart animate (lane SMB-FIG-SMART-ANIMATE, STU-PRO-117/118): a transition whose layers match
//! (`SMART_ANIMATE`, or a directional one with `match_layers`) is drawn as a real tween. Both
//! top-level frames are snapshotted from the `.fig` nodes (parent-relative position, size,
//! rotation from the transform, opacity, first solid fill, corner radius; an `autoRename` text layer
//! matches by its text style), paired and interpolated by `hsk_studio_components::match_layers` /
//! `smart_animate_frame` at the eased progress, and the in-between frame is a synthetic copy of the
//! `.fig` document (destination nodes moved, resized with their geometry scaled, rounded rectangles
//! rebuilt at the interpolated radius, fills and opacity set; source-only layers and the outgoing
//! content of changed text inserted beneath, dissolving) rendered through the same `to_native` +
//! PDF frame export + press route as every screen. Figma parity UNVERIFIED (oracle D5).
//!
//! Overlays (STU-PRO-122..124): placed by the destination frame's overlay record
//! (`hsk_studio_components::overlay_placement`: eight positions, unset = CENTER, MANUAL = the
//! action's `overlay_relative_position`), drawn over the frame's solid background colour when it has
//! one; a click outside the top overlay closes it when the frame says `CLOSE_ON_CLICK_OUTSIDE`.
//! Everything is window-free: the panel draws into a caller's `egui::Context`, and
//! [`PrototypePlay`] steps headlessly (test `tests/viewer_prototype.rs`).
//!
//! Hit testing: the topmost visible layer (paint order, last first) of the open overlay, else of the
//! screen, whose bounds contain the point. Bounds are the layer's vector paths, its text glyph
//! outlines, or the union of its children; points are frame-local pixels.
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_accord::CancellationToken;
use hsk_studio_components::{
    MAX_SMART_DEPTH, MAX_SMART_LAYERS, PlayerEvent, PlayerInput, PlayerSession, SmartFrame, SmartLayer,
    SmartProps, SmartSide, interpolate, match_layers, overlay_placement, smart_animate_frame,
};
use hsk_studio_folio::{
    Budget, OverlayPosition, Parent, Payload, Resolution, Resolver, StudioOverlayBackground, TileRef,
};
use hsk_studio_interop_fig::{
    Color, FigDocument, FigNode, FigOutcome, Geometry, Limits, Matrix, NativeConfig, NativeProposal,
    NodePayload, PaintKind, PathCommand, SequentialIdMint, SourceId, VersionPolicy, decode_document,
    export_frame_pdf, import_fig, plan_frame_exports, render_frame_export, to_native,
};
use hsk_studio_nib::{OwnedPath, PathElement};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

/// Stable widget ids of the play panel.
pub mod widget {
    pub const PANEL: &str = "prototype";
    pub const SCREEN: &str = "prototype.screen";
    pub const RESTART: &str = "prototype.restart";
}

/// Largest screen raster the play mode renders (pixels).
pub const MAX_SCREEN_PIXELS: u64 = 16 * 1024 * 1024;

/// A typed failure of the play mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayFail {
    pub code: &'static str,
    pub detail: String,
}

fn fail(code: &'static str, detail: impl Into<String>) -> PlayFail {
    PlayFail { code, detail: detail.into() }
}

/// An 8-bit RGB raster, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == hsk_studio_accord::SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}

fn bbox(paths: &[&OwnedPath]) -> Option<[f64; 4]> {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut any = false;
    for p in paths {
        for e in &p.elements {
            let pts: &[&hsk_studio_nib::PathPoint] = match e {
                PathElement::MoveTo(a) | PathElement::LineTo(a) => &[a],
                PathElement::QuadTo(a, c) => &[a, c],
                PathElement::CubicTo(a, c, d) => &[a, c, d],
                PathElement::Close => &[],
            };
            for q in pts {
                any = true;
                b = [b[0].min(q.x), b[1].min(q.y), b[2].max(q.x), b[3].max(q.y)];
            }
        }
    }
    any.then_some(b)
}

fn union(a: Option<[f64; 4]>, b: Option<[f64; 4]>) -> Option<[f64; 4]> {
    match (a, b) {
        (Some(a), Some(b)) => Some([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]),
        (a, None) => a,
        (None, b) => b,
    }
}

const IDENTITY: Matrix = Matrix { m00: 1.0, m01: 0.0, m02: 0.0, m10: 0.0, m11: 1.0, m12: 0.0 };

fn key_of(s: SourceId) -> String {
    format!("{}:{}", s.session, s.local)
}

fn narrow(v: f64) -> f32 {
    v as f32
}

/// The smart-animate properties of one `.fig` node (see the module text).
pub fn smart_props(n: &FigNode) -> SmartProps {
    let m = n.transform.unwrap_or(IDENTITY);
    let (w, h) = n.size.map_or((0.0, 0.0), |s| (f64::from(s.x), f64::from(s.y)));
    let fill = n.fills.iter().find(|p| p.visible != Some(false)).and_then(|p| match &p.kind {
        PaintKind::Solid { color: Some(c) } => Some([
            f64::from(c.r),
            f64::from(c.g),
            f64::from(c.b),
            f64::from(c.a) * f64::from(p.opacity.unwrap_or(1.0)),
        ]),
        _ => None,
    });
    let corner_radius = match &n.payload {
        NodePayload::Rectangle { corner_radius, .. } => corner_radius.map_or(0.0, f64::from),
        NodePayload::Frame(f) | NodePayload::Symbol { frame: f, .. } | NodePayload::Instance { frame: f, .. } => {
            f.corner_radius.map_or(0.0, f64::from)
        }
        _ => 0.0,
    };
    SmartProps {
        x: f64::from(m.m02),
        y: f64::from(m.m12),
        width: w,
        height: h,
        rotation_deg: f64::from(m.m10).atan2(f64::from(m.m00)).to_degrees(),
        opacity: f64::from(n.opacity.unwrap_or(1.0)),
        fill,
        corner_radius,
    }
}

/// The layer snapshot of the top-level frame `frame` of a `.fig` document (pre-order, sibling
/// order kept); layer ids are `session:local` source ids.
pub fn smart_frame(doc: &FigDocument, frame: SourceId) -> Result<SmartFrame, PlayFail> {
    let index: HashMap<SourceId, usize> = doc.nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
    let root = *index.get(&frame).ok_or_else(|| fail("smart_frame_missing", key_of(frame)))?;
    let mut layers = Vec::new();
    // (node index, depth), children pushed in reverse so the pre-order keeps sibling order.
    let mut stack: Vec<(usize, usize)> = doc.nodes[root].children.iter().rev().map(|&c| (c, 1)).collect();
    while let Some((i, depth)) = stack.pop() {
        if depth > MAX_SMART_DEPTH || layers.len() >= MAX_SMART_LAYERS {
            return Err(fail("smart_frame_over_limit", key_of(frame)));
        }
        let Some(n) = doc.nodes.get(i) else { continue };
        let parent = n.parent.filter(|p| *p != frame).map(key_of);
        let (implicit_text_style, content) = match &n.payload {
            NodePayload::Text(t) => {
                let fill = n.fills.iter().find_map(|p| match &p.kind {
                    PaintKind::Solid { color: Some(c) } => Some(*c),
                    _ => None,
                });
                let style = format!("{:?}|{:?}|{:?}|{:?}", t.font_family, t.font_style, t.font_size, fill);
                ((t.auto_rename == Some(true)).then_some(style), Some(t.characters.clone()))
            }
            _ => (
                None,
                n.fills.iter().find_map(|p| match &p.kind {
                    PaintKind::Image { hash, .. } => Some(hash.iter().map(|b| format!("{b:02x}")).collect::<String>()),
                    _ => None,
                }),
            ),
        };
        layers.push(SmartLayer {
            id: key_of(n.id),
            name: n.name.clone().unwrap_or_default(),
            parent,
            implicit_text_style,
            content,
            props: smart_props(n),
        });
        stack.extend(n.children.iter().rev().map(|&c| (c, depth.saturating_add(1))));
    }
    Ok(SmartFrame { frame_id: key_of(frame), layers })
}

fn scale_commands(g: &mut [Geometry], sx: f32, sy: f32) {
    for c in g.iter_mut().filter_map(|g| g.commands.as_mut()).flatten() {
        match c {
            PathCommand::MoveTo { x, y } | PathCommand::LineTo { x, y } => {
                *x *= sx;
                *y *= sy;
            }
            PathCommand::QuadTo { x1, y1, x, y } => {
                *x1 *= sx;
                *y1 *= sy;
                *x *= sx;
                *y *= sy;
            }
            PathCommand::CubicTo { x1, y1, x2, y2, x, y } => {
                *x1 *= sx;
                *y1 *= sy;
                *x2 *= sx;
                *y2 *= sy;
                *x *= sx;
                *y *= sy;
            }
            PathCommand::Close => {}
        }
    }
}

/// A rounded rectangle `w` x `h` with radius `r` (clamped to half the short side), node-local,
/// circular corners as cubic arcs (kappa 0.5522847498).
fn rounded_rect(w: f64, h: f64, r: f64) -> Vec<PathCommand> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let k = r * (1.0 - 0.552_284_749_8);
    let p = |x: f64, y: f64| (narrow(x), narrow(y));
    let mut v = Vec::with_capacity(10);
    let (x, y) = p(r, 0.0);
    v.push(PathCommand::MoveTo { x, y });
    let line = |x: f64, y: f64, v: &mut Vec<PathCommand>| {
        let (x, y) = p(x, y);
        v.push(PathCommand::LineTo { x, y });
    };
    let arc = |a: (f64, f64), b: (f64, f64), e: (f64, f64), v: &mut Vec<PathCommand>| {
        let ((x1, y1), (x2, y2), (x, y)) = (p(a.0, a.1), p(b.0, b.1), p(e.0, e.1));
        v.push(PathCommand::CubicTo { x1, y1, x2, y2, x, y });
    };
    line(w - r, 0.0, &mut v);
    arc((w - k, 0.0), (w, k), (w, r), &mut v);
    line(w, h - r, &mut v);
    arc((w, h - k), (w - k, h), (w - r, h), &mut v);
    line(r, h, &mut v);
    arc((k, h), (0.0, h - k), (0.0, h - r), &mut v);
    line(0.0, r, &mut v);
    arc((0.0, k), (k, 0.0), (r, 0.0), &mut v);
    v.push(PathCommand::Close);
    v
}

/// Writes interpolated properties `p` onto node `n` whose own properties are `own`.
fn apply_props(n: &mut FigNode, own: &SmartProps, p: &SmartProps) {
    let mut m = n.transform.unwrap_or(IDENTITY);
    if (p.rotation_deg - own.rotation_deg).abs() > 1e-9 {
        let (sin, cos) = p.rotation_deg.to_radians().sin_cos();
        m.m00 = narrow(cos);
        m.m01 = narrow(-sin);
        m.m10 = narrow(sin);
        m.m11 = narrow(cos);
    }
    m.m02 = narrow(p.x);
    m.m12 = narrow(p.y);
    n.transform = Some(m);
    let resized = (p.width - own.width).abs() > 1e-9 || (p.height - own.height).abs() > 1e-9;
    let reradius = (p.corner_radius - own.corner_radius).abs() > 1e-9;
    let text = matches!(n.payload, NodePayload::Text(_));
    if resized && !text && own.width > 0.0 && own.height > 0.0 {
        let (sx, sy) = (narrow(p.width / own.width), narrow(p.height / own.height));
        scale_commands(&mut n.fill_geometry, sx, sy);
        scale_commands(&mut n.stroke_geometry, sx, sy);
    }
    if resized {
        n.size = Some(hsk_studio_interop_fig::Vec2 { x: narrow(p.width), y: narrow(p.height) });
    }
    match &mut n.payload {
        NodePayload::Rectangle { corner_radius, corner_radii } if reradius || (resized && p.corner_radius > 0.0) => {
            *corner_radius = Some(narrow(p.corner_radius));
            *corner_radii = None;
            if let Some(g) = n.fill_geometry.first_mut() {
                g.commands = Some(rounded_rect(p.width, p.height, p.corner_radius));
                g.blob = None;
            }
            n.fill_geometry.truncate(1);
        }
        NodePayload::Frame(f) | NodePayload::Symbol { frame: f, .. } | NodePayload::Instance { frame: f, .. } if reradius => {
            f.corner_radius = Some(narrow(p.corner_radius));
            f.corner_radii = None;
        }
        _ => {}
    }
    n.opacity = Some(narrow(p.opacity));
    if p.fill != own.fill
        && let Some(f) = p.fill
        && let Some(paint) = n.fills.iter_mut().find(|q| q.visible != Some(false))
        && let PaintKind::Solid { color } = &mut paint.kind
    {
        *color = Some(Color { r: narrow(f[0]), g: narrow(f[1]), b: narrow(f[2]), a: 1.0 });
        paint.opacity = Some(narrow(f[3]));
    }
}

/// Clones the subtree of node index `src` under the node index `parent` (inserted at child
/// position `at`), with fresh source ids; interactions, flow starts and export settings are not
/// copied. Returns the clone root's index.
fn clone_subtree(doc: &mut FigDocument, src: usize, parent: usize, at: usize, next: &mut u32) -> Result<usize, PlayFail> {
    let mut work = vec![(src, parent, Some(at))];
    let mut root = None;
    let mut guard = 0usize;
    while let Some((s, p, pos)) = work.pop() {
        guard = guard.saturating_add(1);
        if guard > MAX_SMART_LAYERS {
            return Err(fail("smart_clone_over_limit", ""));
        }
        let mut n = doc.nodes.get(s).cloned().ok_or_else(|| fail("smart_clone_missing", ""))?;
        let kids = std::mem::take(&mut n.children);
        n.id = SourceId { session: u32::MAX, local: *next };
        *next = next.checked_add(1).ok_or_else(|| fail("smart_clone_over_limit", ""))?;
        n.parent = doc.nodes.get(p).map(|q| q.id);
        n.interactions.clear();
        n.flow_start = None;
        n.flow_position = None;
        n.export_settings.clear();
        let i = doc.nodes.len();
        doc.nodes.push(n);
        let siblings = &mut doc.nodes[p].children;
        match pos {
            Some(at) => siblings.insert(at.min(siblings.len()), i),
            None => siblings.push(i),
        }
        root.get_or_insert(i);
        // Reversed onto the stack so they pop, and are appended, in sibling order.
        for k in kids.into_iter().rev() {
            work.push((k, i, None));
        }
    }
    Ok(root.unwrap_or(src))
}

/// The `.fig` document of the smart-animate frame between top-level frames `from` and `to` at
/// eased progress `eased` (see the module text), drawn as frame `to`.
pub fn smart_document(doc: &FigDocument, from: SourceId, to: SourceId, eased: f64) -> Result<FigDocument, PlayFail> {
    let fa = smart_frame(doc, from)?;
    let fb = smart_frame(doc, to)?;
    let m = match_layers(&fa, &fb).map_err(|e| fail("smart_match_refused", format!("{e:?}")))?;
    let draw = smart_animate_frame(&fa, &fb, &m, eased).map_err(|e| fail("smart_frame_refused", format!("{e:?}")))?;
    let mut out = doc.clone();
    let index: HashMap<String, usize> = out.nodes.iter().enumerate().map(|(i, n)| (key_of(n.id), i)).collect();
    let at = |k: &str| index.get(k).copied().ok_or_else(|| fail("smart_layer_missing", k));
    let to_of_from: BTreeMap<&str, &str> = m.pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let from_only: std::collections::BTreeSet<&str> = m.from_only.iter().map(String::as_str).collect();
    let own_a: BTreeMap<&str, &SmartLayer> = fa.layers.iter().map(|l| (l.id.as_str(), l)).collect();
    let own_b: BTreeMap<&str, &SmartLayer> = fb.layers.iter().map(|l| (l.id.as_str(), l)).collect();
    // The destination frame's own fill tweens from the source frame's.
    let (fi, ti) = (at(&key_of(from))?, at(&key_of(to))?);
    let (fp, tp) = (smart_props(&doc.nodes[fi]), smart_props(&doc.nodes[ti]));
    let mut frame_props = tp;
    frame_props.fill = interpolate(&fp, &tp, eased).fill;
    apply_props(&mut out.nodes[ti], &tp, &frame_props);
    let mut next = 1u32;
    // Source-only layers go beneath, in source order: the n-th one under a parent at position n.
    let mut beneath: BTreeMap<usize, usize> = BTreeMap::new();
    for d in &draw {
        match &d.side {
            SmartSide::Matched { .. } | SmartSide::ToOnly => {
                let own = own_b.get(d.id.as_str()).ok_or_else(|| fail("smart_layer_missing", d.id.clone()))?;
                let i = at(&d.id)?;
                apply_props(&mut out.nodes[i], &own.props, &d.props);
            }
            SmartSide::FromOnly => {
                let own = own_a.get(d.id.as_str()).ok_or_else(|| fail("smart_layer_missing", d.id.clone()))?;
                // Descendants of a source-only layer travel with its clone.
                if own.parent.as_deref().is_some_and(|p| from_only.contains(p)) {
                    continue;
                }
                let parent = match own.parent.as_deref() {
                    Some(p) => at(to_of_from.get(p).copied().ok_or_else(|| fail("smart_parent_unmatched", p))?)?,
                    None => ti,
                };
                let slot = beneath.entry(parent).or_insert(0);
                let c = clone_subtree(&mut out, at(&d.id)?, parent, *slot, &mut next)?;
                *slot = slot.saturating_add(1);
                apply_props(&mut out.nodes[c], &own.props, &d.props);
            }
            SmartSide::ContentOut { from: f } => {
                let own = own_a.get(f.as_str()).ok_or_else(|| fail("smart_layer_missing", f.clone()))?;
                let dest = at(to_of_from.get(f.as_str()).copied().ok_or_else(|| fail("smart_layer_missing", f.clone()))?)?;
                let parent_id = out.nodes[dest].parent.ok_or_else(|| fail("smart_parent_unmatched", f.clone()))?;
                let parent = at(&key_of(parent_id))?;
                let pos = out.nodes[parent].children.iter().position(|&c| c == dest).unwrap_or(0);
                let c = clone_subtree(&mut out, at(f)?, parent, pos, &mut next)?;
                apply_props(&mut out.nodes[c], &own.props, &d.props);
            }
        }
    }
    Ok(out)
}

/// The play mode over one imported `.fig`.
pub struct PrototypePlay {
    doc: FigDocument,
    proposal: NativeProposal,
    session: PlayerSession,
    start: String,
    /// Layer id -> document-px bounds (x0, y0, x1, y1).
    bounds: BTreeMap<String, [f64; 4]>,
    /// Layer id -> children in paint order.
    children: BTreeMap<String, Vec<String>>,
    visible: BTreeMap<String, bool>,
    hovered: Option<String>,
    rendered: BTreeMap<String, Screen>,
    /// The last smart-animate frame: (from, to, eased progress in 1e-6 steps) -> raster.
    smart_cache: Option<((String, String, i64), Screen)>,
    events: Vec<PlayerEvent>,
    texture: Option<(egui::TextureHandle, Screen)>,
    cancel: CancellationToken,
}

impl PrototypePlay {
    /// Imports `bytes` (a `.fig`) and opens its first flow starting point (else `start_source`).
    pub fn open(bytes: &[u8], start_source: Option<SourceId>) -> Result<Self, PlayFail> {
        let cancel = CancellationToken::default();
        let outcome = import_fig(bytes, &Limits::default(), &VersionPolicy::observed(), None, &cancel)
            .map_err(|e| fail("fig_import_refused", e.code()))?;
        let FigOutcome::Imported(import) = outcome else {
            return Err(fail("fig_version_unsupported", "opaque only"));
        };
        let doc = decode_document(&import, &Limits::default(), &cancel).map_err(|e| fail("fig_decode_refused", e.code()))?;
        Self::open_document(doc, start_source)
    }

    /// Opens an already decoded `.fig` document (a host that edited or synthesised the decoded
    /// nodes; tests set overlay fields this way) at its first flow starting point (else
    /// `start_source`).
    pub fn open_document(doc: FigDocument, start_source: Option<SourceId>) -> Result<Self, PlayFail> {
        let cancel = CancellationToken::default();
        let cfg = NativeConfig { budget: Budget::default(), resolver: &Host };
        let proposal = to_native(&doc, &cfg, &mut SequentialIdMint::new(0xF16), &cancel)
            .map_err(|e| fail("fig_native_refused", e.code()))?;
        let start = match start_source {
            Some(s) => proposal.id_map.iter().find(|e| e.source == s).map(|e| e.node.node_id.clone()),
            None => proposal
                .design
                .prototype_pages
                .first()
                .and_then(|p| p.flow_starting_points.first())
                .map(|f| f.node_id.clone()),
        }
        .ok_or_else(|| fail("no_flow_starting_point", ""))?;
        let d = proposal.snapshot.document();
        let session = PlayerSession::start(d, &proposal.design, &start).ok_or_else(|| fail("start_not_a_node", start.clone()))?;
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut visible = BTreeMap::new();
        for l in &d.layers {
            visible.insert(l.layer_id.clone(), l.visible);
            if let Parent::Node(n) = &l.parent {
                children.entry(n.node_id.clone()).or_default().push(l.layer_id.clone());
            }
        }
        let mut own: BTreeMap<String, Option<[f64; 4]>> = BTreeMap::new();
        for l in &d.layers {
            let b = match &l.payload {
                Payload::Vector { path_ids } => {
                    let paths: Vec<&OwnedPath> = proposal
                        .paths
                        .iter()
                        .filter(|p| path_ids.contains(&p.path_id))
                        .map(|p| &p.geometry)
                        .collect();
                    bbox(&paths)
                }
                Payload::Text { .. } => {
                    let t = proposal.text_layouts.iter().find(|t| t.layer_id == l.layer_id);
                    let paths: Vec<&OwnedPath> = t.map_or(Vec::new(), |t| t.glyphs.iter().filter_map(|g| g.path.as_ref()).collect());
                    bbox(&paths)
                }
                _ => None,
            };
            own.insert(l.layer_id.clone(), b);
        }
        // Groups: the union of their children (layers are parents-first, so walk in reverse).
        let mut bounds: BTreeMap<String, [f64; 4]> = BTreeMap::new();
        for l in d.layers.iter().rev() {
            let mut b = own.get(&l.layer_id).copied().flatten();
            for c in children.get(&l.layer_id).map_or(&[][..], Vec::as_slice) {
                b = union(b, bounds.get(c).copied());
            }
            if let Some(b) = b {
                bounds.insert(l.layer_id.clone(), b);
            }
        }
        Ok(Self {
            doc,
            proposal,
            session,
            start,
            bounds,
            children,
            visible,
            hovered: None,
            rendered: BTreeMap::new(),
            smart_cache: None,
            events: Vec::new(),
            texture: None,
            cancel,
        })
    }

    /// The decoded `.fig` document the play mode runs over.
    pub fn document(&self) -> &FigDocument {
        &self.doc
    }
    pub fn proposal(&self) -> &NativeProposal {
        &self.proposal
    }
    pub fn session(&self) -> &PlayerSession {
        &self.session
    }
    /// Events of the last steps (cleared by [`PrototypePlay::take_events`]).
    pub fn take_events(&mut self) -> Vec<PlayerEvent> {
        std::mem::take(&mut self.events)
    }

    /// Restarts the flow from its starting point.
    pub fn restart(&mut self) {
        if let Some(s) = PlayerSession::start(self.proposal.snapshot.document(), &self.proposal.design, &self.start) {
            self.session = s;
            self.hovered = None;
        }
    }

    /// Document-px origin of a frame (screen or overlay).
    fn origin(&self, frame: &str) -> Option<[f64; 2]> {
        self.bounds.get(frame).map(|b| [b[0], b[1]])
    }

    /// Top-left of `overlay` in the screen's frame-local px (STU-PRO-123 placement through the
    /// frame's overlay record and the opening action's relative position).
    fn overlay_offset(&self, overlay: &str, relative: Option<&OverlayPosition>) -> Option<[f64; 2]> {
        let s = self.bounds.get(&self.session.state().screen)?;
        let o = self.bounds.get(overlay)?;
        let (x, y) = overlay_placement(
            (s[2] - s[0], s[3] - s[1]),
            (o[2] - o[0], o[3] - o[1]),
            self.session.overlay_config(overlay),
            relative,
        );
        Some([x, y])
    }

    fn hit_in(&self, frame: &str, x: f64, y: f64) -> Option<String> {
        self.hit_node(frame, x, y, 0)
    }

    /// Topmost visible leaf (vector or text layer) under the document point inside `id`: later
    /// children are above earlier ones; a group answers only through its children.
    fn hit_node(&self, id: &str, x: f64, y: f64, depth: usize) -> Option<String> {
        if depth > 64 || self.visible.get(id) == Some(&false) {
            return None;
        }
        let b = self.bounds.get(id)?;
        if !(x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]) {
            return None;
        }
        match self.children.get(id) {
            Some(kids) if !kids.is_empty() => kids.iter().rev().find_map(|k| self.hit_node(k, x, y, depth.saturating_add(1))),
            _ => Some(id.to_owned()),
        }
    }

    /// The node under frame-local point (`x`, `y`): the top overlay first, then the screen.
    pub fn hit(&self, x: f64, y: f64) -> Option<String> {
        let state = self.session.state();
        if let Some(top) = state.overlays.last() {
            let off = self.overlay_offset(top, state.overlay_positions.last().and_then(Option::as_ref))?;
            let o = self.origin(top)?;
            if let Some(h) = self.hit_in(top, o[0] + x - off[0], o[1] + y - off[1]) {
                return Some(h);
            }
        }
        let o = self.origin(&state.screen)?;
        self.hit_in(&state.screen, o[0] + x, o[1] + y)
    }

    /// Whether a frame-local point lies outside the top overlay (false when none is open).
    fn outside_top_overlay(&self, x: f64, y: f64) -> bool {
        let state = self.session.state();
        let Some(top) = state.overlays.last() else { return false };
        let (Some(off), Some(b)) =
            (self.overlay_offset(top, state.overlay_positions.last().and_then(Option::as_ref)), self.bounds.get(top))
        else {
            return false;
        };
        !(x >= off[0] && y >= off[1] && x <= off[0] + (b[2] - b[0]) && y <= off[1] + (b[3] - b[1]))
    }

    /// A click at a frame-local point. Outside an open overlay whose frame says
    /// `CLOSE_ON_CLICK_OUTSIDE` the click closes it (STU-PRO-124) and reaches nothing else.
    pub fn click(&mut self, x: f64, y: f64) -> Option<String> {
        if self.outside_top_overlay(x, y)
            && let Some(top) = self.session.state().overlays.last().cloned()
            && self.session.overlay_config(&top).is_some_and(|o| {
                o.overlay_background_interaction == hsk_studio_folio::StudioOverlayBackgroundInteraction::CloseOnClickOutside
            })
        {
            let ev = self.session.click_outside_overlay();
            self.events.extend(ev);
            return None;
        }
        let node = self.hit(x, y)?;
        let ev = self.session.dispatch(&PlayerInput::Click(node.clone()));
        self.events.extend(ev);
        Some(node)
    }

    /// The pointer moved to a frame-local point: hover leave / enter on a change of node.
    pub fn hover(&mut self, x: f64, y: f64) {
        let node = self.hit(x, y);
        if node == self.hovered {
            return;
        }
        if let Some(prev) = self.hovered.take() {
            let ev = self.session.dispatch(&PlayerInput::HoverLeave(prev));
            self.events.extend(ev.into_iter().filter(|e| !matches!(e, PlayerEvent::Diagnostic("no_reaction_for_trigger"))));
        }
        if let Some(n) = &node {
            let ev = self.session.dispatch(&PlayerInput::HoverEnter(n.clone()));
            self.events.extend(ev.into_iter().filter(|e| !matches!(e, PlayerEvent::Diagnostic("no_reaction_for_trigger"))));
        }
        self.hovered = node;
    }

    /// Advances the session clock.
    pub fn advance(&mut self, ms: u64) {
        let ev = self.session.advance(ms);
        self.events.extend(ev);
    }

    /// The frame `layer_id` rendered (PDF frame export rendered back by press), cached.
    pub fn frame_raster(&mut self, layer_id: &str) -> Result<Screen, PlayFail> {
        if let Some(s) = self.rendered.get(layer_id) {
            return Ok(s.clone());
        }
        let source: SourceId = self
            .proposal
            .id_map
            .iter()
            .find(|e| e.node.node_id == layer_id)
            .map(|e| e.source)
            .ok_or_else(|| fail("frame_not_mapped", layer_id))?;
        let (plans, _) = plan_frame_exports(&self.doc, &self.proposal, source);
        let plan = plans.first().ok_or_else(|| fail("frame_not_exportable", layer_id))?;
        let pixels = (plan.bounds[2] * plan.bounds[3]).max(0.0);
        if pixels > MAX_SCREEN_PIXELS as f64 {
            return Err(fail("screen_over_limit", layer_id));
        }
        let pdf = export_frame_pdf(&self.proposal, plan, None, &self.cancel).map_err(|e| fail("frame_export_refused", e.code()))?;
        let page = render_frame_export(&pdf, &self.cancel).map_err(|e| fail("frame_render_refused", e.code()))?;
        let s = Screen { width: page.width, height: page.height, rgb: page.rgb };
        self.rendered.insert(layer_id.to_owned(), s.clone());
        Ok(s)
    }

    /// The smart-animate frame between screens `from` and `to` (Folio layer ids) at eased progress
    /// `eased` (module text), cached for the last progress point.
    pub fn smart_raster(&mut self, from: &str, to: &str, eased: f64) -> Result<Screen, PlayFail> {
        let key = (from.to_owned(), to.to_owned(), (eased * 1e6).round() as i64);
        if let Some((k, s)) = &self.smart_cache
            && *k == key
        {
            return Ok(s.clone());
        }
        let source = |id: &str| {
            self.proposal.id_map.iter().find(|e| e.node.node_id == id).map(|e| e.source).ok_or_else(|| fail("frame_not_mapped", id))
        };
        let (fs, ts) = (source(from)?, source(to)?);
        let doc = smart_document(&self.doc, fs, ts, eased)?;
        let cfg = NativeConfig { budget: Budget::default(), resolver: &Host };
        let proposal = to_native(&doc, &cfg, &mut SequentialIdMint::new(0xF16), &self.cancel)
            .map_err(|e| fail("smart_native_refused", e.code()))?;
        let (plans, _) = plan_frame_exports(&doc, &proposal, ts);
        let plan = plans.first().ok_or_else(|| fail("frame_not_exportable", to))?;
        if (plan.bounds[2] * plan.bounds[3]).max(0.0) > MAX_SCREEN_PIXELS as f64 {
            return Err(fail("screen_over_limit", to));
        }
        let pdf = export_frame_pdf(&proposal, plan, None, &self.cancel).map_err(|e| fail("frame_export_refused", e.code()))?;
        let page = render_frame_export(&pdf, &self.cancel).map_err(|e| fail("frame_render_refused", e.code()))?;
        let s = Screen { width: page.width, height: page.height, rgb: page.rgb };
        self.smart_cache = Some((key, s.clone()));
        Ok(s)
    }

    /// What the play mode shows now: the screen (a running transition: the smart-animate tween when
    /// its layers match, else the eased cross-fade) with the open overlays placed on top.
    pub fn render(&mut self) -> Result<Screen, PlayFail> {
        let state = self.session.state();
        let mut base = match &state.transition {
            Some((t, _, eased)) if t.match_layers => self.smart_raster(&t.from, &t.to, *eased)?,
            Some((t, _, eased)) => {
                let from = self.frame_raster(&t.from)?;
                let to = self.frame_raster(&t.to)?;
                if (from.width, from.height) != (to.width, to.height) {
                    to
                } else {
                    let k = eased.clamp(0.0, 1.0);
                    let rgb = from
                        .rgb
                        .iter()
                        .zip(&to.rgb)
                        .map(|(a, b)| (f64::from(*a) * (1.0 - k) + f64::from(*b) * k).round().clamp(0.0, 255.0) as u8)
                        .collect();
                    Screen { width: to.width, height: to.height, rgb }
                }
            }
            None => self.frame_raster(&state.screen)?,
        };
        for (i, o) in state.overlays.iter().enumerate() {
            let ov = self.frame_raster(o)?;
            let Some(off) = self.overlay_offset(o, state.overlay_positions.get(i).and_then(Option::as_ref)) else { continue };
            // STU-PRO-124 solid background behind the overlay (alpha = background opacity).
            if let Some(StudioOverlayBackground::SolidColor { color }) = self.session.overlay_config(o).map(|c| &c.overlay_background) {
                let a = color.a.clamp(0.0, 1.0);
                let c = [color.r, color.g, color.b].map(|v| v.clamp(0.0, 1.0) * 255.0);
                for px in base.rgb.chunks_mut(3) {
                    for (k, v) in px.iter_mut().enumerate() {
                        *v = (f64::from(*v) * (1.0 - a) + c[k] * a).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
            let (ox, oy) = (off[0].round() as i64, off[1].round() as i64);
            for y in 0..i64::from(ov.height) {
                for x in 0..i64::from(ov.width) {
                    let (bx, by) = (x.saturating_add(ox), y.saturating_add(oy));
                    if bx < 0 || by < 0 || bx >= i64::from(base.width) || by >= i64::from(base.height) {
                        continue;
                    }
                    let si = usize::try_from(y.saturating_mul(i64::from(ov.width)).saturating_add(x)).unwrap_or(0).saturating_mul(3);
                    let di = usize::try_from(by.saturating_mul(i64::from(base.width)).saturating_add(bx)).unwrap_or(0).saturating_mul(3);
                    if let (Some(src), Some(dst)) = (ov.rgb.get(si..si.saturating_add(3)), base.rgb.get_mut(di..di.saturating_add(3))) {
                        dst.copy_from_slice(src);
                    }
                }
            }
        }
        Ok(base)
    }

    /// Machine-readable state (harness report).
    pub fn state_json(&self) -> Value {
        let s = self.session.state();
        let name = |id: &str| self.proposal.snapshot.document().layers.iter().find(|l| l.layer_id == id).map(|l| l.name.clone());
        json!({
            "screen": s.screen, "screen_name": name(&s.screen), "overlays": s.overlays,
            "history_depth": s.history_depth, "scroll_target": s.scroll_target, "clock_ms": s.clock_ms,
            "overlay_positions": s.overlay_positions.iter().map(|p| p.as_ref().map(|p| json!([p.x, p.y]))).collect::<Vec<_>>(),
            "transition": s.transition.as_ref().map(|(t, raw, eased)| json!({"to": t.to, "kind": t.kind, "match_layers": t.match_layers,
                "duration_ms": t.duration_ms, "progress": raw, "eased": eased})),
            "hovered": self.hovered,
        })
    }

    /// Draws the play panel; pointer clicks and moves over the screen image drive the player.
    /// Returns the widget ids drawn.
    pub fn ui(&mut self, ctx: &egui::Context, elapsed_ms: u64) -> Vec<&'static str> {
        let mut drawn = vec![widget::PANEL];
        self.advance(elapsed_ms);
        let screen = self.render().ok();
        if let Some(s) = screen
            && self.texture.as_ref().is_none_or(|(_, shown)| shown != &s)
        {
            let mut rgba = Vec::with_capacity(s.rgb.len().saturating_mul(4) / 3);
            for p in s.rgb.chunks(3) {
                rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([s.width as usize, s.height as usize], &rgba);
            let handle = ctx.load_texture("hsk-studio-prototype", image, egui::TextureOptions::LINEAR);
            self.texture = Some((handle, s));
        }
        let mut restart = false;
        let mut pointer: Option<(f64, f64, bool)> = None;
        egui::Window::new("Prototype").id(egui::Id::new(widget::PANEL)).show(ctx, |ui| {
            if ui.button("Restart").clicked() {
                restart = true;
            }
            drawn.push(widget::RESTART);
            if let Some((t, _)) = &self.texture {
                let response = ui.add(egui::Image::new((t.id(), t.size_vec2())).sense(egui::Sense::click()));
                drawn.push(widget::SCREEN);
                if let Some(pos) = response.hover_pos() {
                    // f32 screen coordinates (no integer arithmetic).
                    let (lx, ly) = (f64::from(pos.x) - f64::from(response.rect.min.x), f64::from(pos.y) - f64::from(response.rect.min.y));
                    pointer = Some((lx, ly, response.clicked()));
                }
            }
        });
        if restart {
            self.restart();
        }
        if let Some((x, y, clicked)) = pointer {
            self.hover(x, y);
            if clicked {
                self.click(x, y);
            }
        }
        if self.session.state().transition.is_some() {
            ctx.request_repaint();
        }
        drawn
    }
}
