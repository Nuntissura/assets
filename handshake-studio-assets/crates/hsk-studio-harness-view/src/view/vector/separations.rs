//! Separations preview of the open vector document (lane SMB-AI-TYPE-PATH-RECOLOR; STU-COL-212,
//! STU-COL-211, STU-COL-020, AI-31). Press owns separation (lane SMB-ID-PREPRESS, d44b0a9: ink
//! manager, `Separator`, per-plate readouts, coverage, ink limits, plate renders); this module feeds
//! it the document the way the viewer draws it.
//!
//! Geometry: composite's `expand_vectors` (as the PDF export: swatches resolved, strokes, brushes,
//! width profiles, networks and envelopes expanded), walked bottom-most first through the layer
//! tree; document px map to page points like the PDF export (1 px = 0.75 pt, y flipped, page = the
//! first artboard). Folio vector clips and VECTOR masks are press clips of their source geometry
//! (an inverted mask is noted, not clipped); hidden layers and clip/mask sources are not drawn.
//!
//! Inks: [`Viewer::vector_ink_manager`] = the four process inks plus one spot ink per Folio spot
//! swatch (its CMYK alternate as the process alternate when it has one). A solid row separates by
//! its SOURCE colour: a row bound to a swatch (matched by its stand-in colour among the layer's
//! style rows) gives CMYK components (times its tint), GRAY `g` as K `1 - g` (times its tint), a
//! spot (or a tint of one) its ink at the tint; an unbound RGB row, an RGB or Lab swatch convert to
//! CMYK through Prism into the document CMYK profile (encoded sRGB v4 source, relative
//! colorimetric), and without that profile the object is not separated (counted, typed note).
//! `overprint_black` (STU-COL-211 document policy) makes 100 % K process objects overprint in
//! nonzero overprint mode (only the K plate changes; the C, M, Y plates below show through).
//! Noted, not separated: gradient rows (`ShadingNotSeparated`), row or group transparency (painted
//! opaque), alpha/luminance masks, inverted vector masks, gradient meshes, raster and text layers
//! (returned as typed losses). Folio carries no per-object overprint attribute yet (declared gap).
//! InDesign/Illustrator Separations Preview parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::masking;
use super::style::{effective_design, node_style};
use super::swatch::cmyk_bytes;
use super::VectorEntry;
use hsk_studio_composite::FillPaint;
use hsk_studio_composite::vector::{ExpandedLayer, VectorResolution, expand_vectors};
use hsk_studio_folio::{self as folio, Payload, StudioAppearanceRow, StudioColorModel, StudioMaskMode, StudioPaint, StudioSwatchKind};
use hsk_studio_nib::{OwnedPath, PathElement};
use hsk_studio_press::{Ink, InkManager, SepDraw, SepLimits, SepNote, SepPaint, SeparationPreview, Separator, VectorEl, VectorMode};
use std::collections::{BTreeMap, BTreeSet};

/// Points per document pixel (as the PDF export).
const PT: f64 = 0.75;

/// Separation options.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorSeparationOptions {
    /// Preview pixels per point.
    pub px_per_pt: f64,
    /// STU-COL-211 `overprint_black`: 100 % K process objects overprint.
    pub overprint_black: bool,
    /// The ink manager; `None` = [`Viewer::vector_ink_manager`].
    pub inks: Option<InkManager>,
    pub limits: SepLimits,
}
impl Default for VectorSeparationOptions {
    fn default() -> Self {
        Self { px_per_pt: 1.0, overprint_black: false, inks: None, limits: SepLimits::default() }
    }
}

/// Encoded sRGB -> CMYK through Prism into `icc` (relative colorimetric).
fn rgb_to_cmyk(icc: &[u8], rgb: [f64; 3]) -> Option<[f64; 4]> {
    use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
    use hsk_studio_prism::{Intent, ProfileInput, WideEngine, WideSource, WideTransformRequest, profile_hash};
    struct Quiet;
    impl hsk_studio_observe::SinkPort for Quiet {
        fn try_send(&mut self, _class: hsk_studio_observe::DeliveryClass, _bytes: &[u8]) -> Result<(), hsk_studio_observe::DeliveryError> {
            Ok(())
        }
    }
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "harness-separations").ok()?;
    let destination_id = DomainId::parse("SCPF-019abcde-0000-7000-8000-00000000a011").ok()?;
    let source_id = DomainId::parse("SCPF-019abcde-0000-7000-8000-00000000a012").ok()?;
    let resource = DomainId::parse("SSWT-019abcde-0000-7000-8000-00000000a013").ok()?;
    let srgb = super::super::develop::SRGB_V4_ICC;
    let pixel = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32];
    let request = WideTransformRequest {
        source: WideSource::Profile(ProfileInput { profile_id: &source_id, bytes: srgb, expected_sha256: profile_hash(srgb) }),
        destination: ProfileInput { profile_id: &destination_id, bytes: icc, expected_sha256: profile_hash(icc) },
        pixels: &pixel,
        intent: Intent::RelativeColorimetric,
        black_point_compensation: false,
        revision: 0,
        expected_revision: 0,
        correlation_id: 1,
        resource_id: &resource,
        actor: &actor,
    };
    let mut engine = WideEngine::new(actor.clone(), 2).ok()?;
    let out = engine.transform(&request, &CancellationToken::default(), &mut Quiet).ok()?;
    match out.pixels.as_slice() {
        [c, m, y, k] => Some([c, m, y, k].map(|v| f64::from(*v).clamp(0.0, 1.0))),
        _ => None,
    }
}

/// Separation paint of a swatch binding (tints resolved), or `None` when it needs Prism.
fn swatch_paint(records: &folio::StudioVectorRecords, swatch_id: &str, tint: f64) -> Option<SepPaint> {
    let mut s = records.swatch(swatch_id)?;
    let mut t = tint;
    for _ in 0..16 {
        if s.swatch_kind != StudioSwatchKind::Tint {
            break;
        }
        t *= s.tint_value.unwrap_or(100.0) / 100.0;
        s = records.swatch(s.base_swatch_ref.as_deref()?)?;
    }
    let t = t.clamp(0.0, 1.0);
    match (s.swatch_kind, s.color_model) {
        (StudioSwatchKind::Spot, _) => Some(SepPaint::Spot { name: s.spot.as_ref()?.ink_name.clone(), tint: t }),
        (StudioSwatchKind::Global | StudioSwatchKind::Process, Some(StudioColorModel::Cmyk)) if s.components.len() == 4 => {
            Some(SepPaint::Process([s.components[0] * t, s.components[1] * t, s.components[2] * t, s.components[3] * t]))
        }
        (StudioSwatchKind::Global | StudioSwatchKind::Process, Some(StudioColorModel::Gray)) if s.components.len() == 1 => Some(SepPaint::Process([0.0, 0.0, 0.0, (1.0 - s.components[0]) * t])),
        _ => None,
    }
}

fn els(paths: &[OwnedPath], even_odd: Option<bool>, page: ([f64; 2], f64)) -> Vec<VectorEl> {
    let (o, h) = page;
    let p = |x: f64, y: f64| [(x - o[0]) * PT, (o[1] + h - y) * PT];
    let mut out = Vec::new();
    for path in paths.iter().filter(|q| even_odd.is_none_or(|e| (q.fill_rule == hsk_studio_nib::FillRule::EvenOdd) == e)) {
        let mut last = [0.0; 2];
        for e in &path.elements {
            match *e {
                PathElement::MoveTo(q) => {
                    last = [q.x, q.y];
                    out.push(VectorEl::MoveTo(p(q.x, q.y)));
                }
                PathElement::LineTo(q) => {
                    last = [q.x, q.y];
                    out.push(VectorEl::LineTo(p(q.x, q.y)));
                }
                PathElement::QuadTo(c, q) => {
                    let c1 = p(last[0] + (c.x - last[0]) * 2.0 / 3.0, last[1] + (c.y - last[1]) * 2.0 / 3.0);
                    let c2 = p(q.x + (c.x - q.x) * 2.0 / 3.0, q.y + (c.y - q.y) * 2.0 / 3.0);
                    last = [q.x, q.y];
                    out.push(VectorEl::CubicTo(c1, c2, p(q.x, q.y)));
                }
                PathElement::CubicTo(a, b, q) => {
                    last = [q.x, q.y];
                    out.push(VectorEl::CubicTo(p(a.x, a.y), p(b.x, b.y), p(q.x, q.y)));
                }
                PathElement::Close => out.push(VectorEl::Close),
            }
        }
    }
    out
}

struct Walk<'a> {
    doc: &'a folio::StudioDocument,
    entry: &'a VectorEntry,
    records: &'a folio::StudioVectorRecords,
    design: &'a folio::StudioDesignRecords,
    expanded: BTreeMap<&'a str, &'a ExpandedLayer>,
    sources: BTreeSet<&'a str>,
    page: ([f64; 2], f64),
    cmyk_icc: Option<Vec<u8>>,
    overprint_black: bool,
}

impl Walk<'_> {
    /// Separation paint of a solid expanded row colour on `layer_id` (module docs).
    fn paint(&self, layer_id: &str, c: [f32; 4]) -> Option<SepPaint> {
        let rgb = [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])];
        let same = |a: &folio::StudioColor| (a.r - rgb[0]).abs() < 1e-6 && (a.g - rgb[1]).abs() < 1e-6 && (a.b - rgb[2]).abs() < 1e-6;
        if let Some(style) = node_style(self.design, layer_id) {
            for (i, row) in style.rows.iter().enumerate() {
                let (StudioAppearanceRow::Fill { paint: StudioPaint::Solid { colour }, .. } | StudioAppearanceRow::Stroke { paint: StudioPaint::Solid { colour }, .. }) = row else { continue };
                if !same(colour) {
                    continue;
                }
                let Some(binding) = u32::try_from(i).ok().and_then(|n| self.records.swatch_paint(&style.style_id, n)) else { continue };
                match swatch_paint(self.records, &binding.swatch_id, binding.tint.unwrap_or(100.0) / 100.0) {
                    Some(p) => return Some(p),
                    None => break,
                }
            }
        }
        self.cmyk_icc.as_deref().and_then(|icc| rgb_to_cmyk(icc, rgb)).map(SepPaint::Process)
    }

    fn rows(&self, s: &mut Separator<'_>, layer_id: &str, losses: &mut Vec<String>) -> Result<(), String> {
        if self.records.gradient_meshes.iter().any(|m| m.layer_id == layer_id) {
            losses.push(format!("{layer_id}: gradient_mesh_not_separated"));
        }
        let Some(layer) = self.expanded.get(layer_id) else { return Ok(()) };
        for row in &layer.rows {
            if row.paths.iter().all(|p| p.elements.is_empty()) {
                continue;
            }
            let c = match &row.paint {
                FillPaint::Solid(c) => *c,
                FillPaint::Gradient(_) => {
                    s.note(SepNote::ShadingNotSeparated);
                    continue;
                }
                _ => {
                    losses.push(format!("{layer_id}: paint_not_separated"));
                    continue;
                }
            };
            if row.opacity < 1.0 || c[3] < 1.0 || row.mode != hsk_studio_pigment::blend_math::StudioBlendMode::Normal {
                s.note(SepNote::TransparencyFlattenedOpaque);
            }
            let Some(paint) = self.paint(layer_id, c) else {
                s.note(SepNote::Unseparated);
                losses.push(format!("{layer_id}: unseparated_no_cmyk_profile"));
                continue;
            };
            let overprint = self.overprint_black && matches!(paint, SepPaint::Process([c0, m0, y0, k]) if c0 == 0.0 && m0 == 0.0 && y0 == 0.0 && k >= 1.0);
            for even_odd in [false, true] {
                let path = els(&row.paths, Some(even_odd), self.page);
                if path.is_empty() {
                    continue;
                }
                s.draw(&SepDraw { path: &path, mode: &VectorMode::Fill { even_odd }, paint: paint.clone(), overprint, overprint_nonzero: overprint }).map_err(|e| format!("separations: {}", e.code()))?;
            }
        }
        Ok(())
    }

    fn walk(&self, s: &mut Separator<'_>, parent: Option<&str>, losses: &mut Vec<String>, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err("separations: layer nesting deeper than 64".into());
        }
        for id in masking::children(self.doc, parent) {
            let Some(layer) = self.doc.layers.iter().find(|l| l.layer_id == id) else { continue };
            if !layer.visible || self.sources.contains(id.as_str()) {
                continue;
            }
            if layer.appearance.as_ref().is_some_and(|a| a.opacity < 100.0 || a.blend_mode != folio::BLEND_NORMAL) {
                s.note(SepNote::TransparencyFlattenedOpaque);
            }
            let (clip, mask) = masking::attachment(self.records, &id);
            let mut pushed = 0_u32;
            if let Some(c) = clip {
                let geo = masking::source_geometry(self.doc, self.entry, &c.source_layer_id)?;
                let path = els(&geo, None, self.page);
                if !path.is_empty() {
                    s.push_clip(&path, c.clip_rule == folio::StudioClipRule::EvenOdd).map_err(|e| format!("separations: {}", e.code()))?;
                    pushed = pushed.saturating_add(1);
                }
            }
            if let Some(m) = mask {
                if m.mode == StudioMaskMode::Vector && !m.invert_mask {
                    let geo = masking::source_geometry(self.doc, self.entry, &m.source_layer_id)?;
                    let path = els(&geo, None, self.page);
                    s.push_clip(&path, false).map_err(|e| format!("separations: {}", e.code()))?;
                    pushed = pushed.saturating_add(1);
                } else {
                    losses.push(format!("{id}: mask_not_separated"));
                }
            }
            match layer.payload {
                Payload::Group => self.walk(s, Some(&id), losses, depth.saturating_add(1))?,
                Payload::Vector { .. } => self.rows(s, &id, losses)?,
                Payload::Raster { .. } => losses.push(format!("{id}: raster_layer_not_separated")),
                Payload::Text { .. } => losses.push(format!("{id}: text_layer_not_separated")),
                _ => {}
            }
            for _ in 0..pushed {
                s.pop_clip();
            }
        }
        Ok(())
    }
}

impl Viewer {
    /// The document's ink manager: the four process inks plus one spot ink per spot swatch (module
    /// docs).
    pub fn vector_ink_manager(&self) -> Result<InkManager, String> {
        let entry = self.vector_entry()?;
        let mut m = InkManager::default();
        for s in entry.records.swatches.iter().filter(|s| s.swatch_kind == StudioSwatchKind::Spot) {
            let Some(ink) = &s.spot else { continue };
            if m.ink(&ink.ink_name).is_some() {
                continue;
            }
            let alternate = (ink.alternate_space == Some(StudioColorModel::Cmyk) && ink.alternate_color_value.len() == 4)
                .then(|| [ink.alternate_color_value[0], ink.alternate_color_value[1], ink.alternate_color_value[2], ink.alternate_color_value[3]]);
            m.inks.push(Ink::spot(&ink.ink_name, alternate));
        }
        m.validate().map_err(|e| format!("separations: {}", e.code()))?;
        Ok(m)
    }

    /// Separates the open vector document (module docs): Press's preview plus the typed losses.
    pub fn vector_separations(&self, options: &VectorSeparationOptions) -> Result<(SeparationPreview, Vec<String>), String> {
        let snap = self.run.snapshot().ok_or("separations: nothing open")?;
        let doc = snap.document();
        let entry = self.vector_entry()?;
        let design = effective_design(doc, &entry)?;
        let records = entry.folio_records(&doc.document_id);
        // Live path and area text (lane SMB-AI-AREA-TYPE-3): the canvas's shaping, fills encoded
        // for this plan; a story that cannot be shaped keeps its typed report as a loss.
        let text = self.text_entry().ok();
        let assets = self.live_text_assets_in(&entry, text.as_ref(), hsk_studio_composite::BlendSpace::Encoded);
        let (layers, refused) = expand_vectors(snap, &design, &records, &assets, hsk_studio_composite::BlendSpace::Encoded, &VectorResolution::default(), self.cancel)
            .map_err(|e| format!("separations: {e:?}"))?;
        let mut losses: Vec<String> = refused.iter().map(|(l, c)| format!("{l}: refused:{c}")).collect();
        losses.extend(assets.losses());
        let board = doc.artboards.first().ok_or("separations: the document has no artboard")?;
        let (o, w, h) = ([board.bounds.x.value, board.bounds.y.value], board.bounds.width.value, board.bounds.height.value);
        let inks = match &options.inks {
            Some(i) => i.clone(),
            None => self.vector_ink_manager()?,
        };
        let mut s = Separator::new([0.0, 0.0, w * PT, h * PT], options.px_per_pt, &inks, options.limits).map_err(|e| format!("separations: {}", e.code()))?;
        let sources: BTreeSet<&str> = records.clips.iter().map(|c| c.source_layer_id.as_str()).chain(records.masks.iter().map(|m| m.source_layer_id.as_str())).collect();
        let walk = Walk {
            doc,
            entry: &entry,
            records: &records,
            design: &design,
            expanded: layers.iter().map(|l| (l.layer_id.as_str(), l)).collect(),
            sources,
            page: (o, h),
            cmyk_icc: cmyk_bytes(&entry, doc, None),
            overprint_black: options.overprint_black,
        };
        walk.walk(&mut s, None, &mut losses, 0)?;
        let mut preview = s.finish();
        let unseparated = losses.iter().filter(|l| l.ends_with(": unseparated_no_cmyk_profile")).count();
        preview.unseparated = preview.unseparated.saturating_add(u64::try_from(unseparated).unwrap_or(u64::MAX));
        Ok((preview, losses))
    }
}
