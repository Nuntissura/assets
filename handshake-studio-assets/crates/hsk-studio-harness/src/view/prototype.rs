//! Prototype play mode (lane SMB-HARNESS-PROTOTYPE-PLAY; STU-PRO-132/138/139): a Figma `.fig` is
//! imported (`hsk-studio-interop-fig`), its first flow starting point opens a
//! `hsk-studio-components` [`PlayerSession`], pointer clicks and hover are hit-tested against the
//! current screen's layers and dispatched as triggers, and the screen is rendered through the
//! interop-fig PDF frame export rendered back by press, with running transitions drawn as an eased
//! cross-fade between the two screens (smart animate is shown as that cross-fade: an
//! approximation). Overlays are drawn centred over the screen (STU-PRO-123 unset = CENTER).
//! Everything is window-free: the panel draws into a caller's `egui::Context`, and
//! [`PrototypePlay`] steps headlessly (test `tests/viewer_prototype.rs`).
//!
//! Hit testing: the topmost visible layer (paint order, last first) of the open overlay, else of the
//! screen, whose bounds contain the point. Bounds are the layer's vector paths, its text glyph
//! outlines, or the union of its children; points are frame-local pixels.
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_accord::CancellationToken;
use hsk_studio_components::{PlayerEvent, PlayerInput, PlayerSession};
use hsk_studio_folio::{Budget, Parent, Payload, Resolution, Resolver, TileRef};
use hsk_studio_interop_fig::{
    FigDocument, FigOutcome, Limits, NativeConfig, NativeProposal, SequentialIdMint, SourceId,
    VersionPolicy, decode_document, export_frame_pdf, import_fig, plan_frame_exports,
    render_frame_export, to_native,
};
use hsk_studio_nib::{OwnedPath, PathElement};
use serde_json::{Value, json};
use std::collections::BTreeMap;

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
            events: Vec::new(),
            texture: None,
            cancel,
        })
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

    /// Top-left of `overlay` in the screen's frame-local px (centred over the screen).
    fn overlay_offset(&self, overlay: &str) -> Option<[f64; 2]> {
        let s = self.bounds.get(&self.session.state().screen)?;
        let o = self.bounds.get(overlay)?;
        Some([((s[2] - s[0]) - (o[2] - o[0])) / 2.0, ((s[3] - s[1]) - (o[3] - o[1])) / 2.0])
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
            let off = self.overlay_offset(top)?;
            let o = self.origin(top)?;
            if let Some(h) = self.hit_in(top, o[0] + x - off[0], o[1] + y - off[1]) {
                return Some(h);
            }
        }
        let o = self.origin(&state.screen)?;
        self.hit_in(&state.screen, o[0] + x, o[1] + y)
    }

    /// A click at a frame-local point.
    pub fn click(&mut self, x: f64, y: f64) -> Option<String> {
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

    /// What the play mode shows now: the screen (or the eased cross-fade of a running transition)
    /// with the open overlays centred on top.
    pub fn render(&mut self) -> Result<Screen, PlayFail> {
        let state = self.session.state();
        let mut base = match &state.transition {
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
        for o in &state.overlays {
            let ov = self.frame_raster(o)?;
            let Some(off) = self.overlay_offset(o) else { continue };
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
            "transition": s.transition.as_ref().map(|(t, raw, eased)| json!({"to": t.to, "kind": t.kind,
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
