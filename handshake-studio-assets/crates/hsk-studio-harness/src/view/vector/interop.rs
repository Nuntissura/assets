//! Illustrator `.ai` import and SVG export of vector documents in the viewer (lane
//! SMB-ILLUSTRATOR-INTEROP; feature `vector-interop`). The harness is the host adapter the
//! interop-vector PDF port names: Press `read_vector_pages` supplies each page's drawing to
//! interop-vector `import_ai_with` ([`PressDrawing`]); the proposal (points) becomes a native
//! package in document pixels (x 96/72: paths as anchor path records, stroke weights, dashes and
//! gradient transforms scaled; design records and vector clips kept) whose vector entry rides as
//! the session document entry `vector`. SVG export writes composite's `expand_vectors` (swatches
//! resolved, brushes, width profiles, networks and envelopes expanded) through interop-vector
//! `export_svg_with_loss`: one path element per painted shape, in points.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::style::effective_design;
use super::{ENTRY, EntryAssets, PathRecord, VectorEntry, VectorStyle};
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::FillPaint;
use hsk_studio_composite::vector::{VectorResolution, expand_vectors};
use hsk_studio_folio::{self as folio, GeometryUnit, Length, StudioAppearanceRow, StudioPaint};
use hsk_studio_interop_vector as iv;
use hsk_studio_nib::anchor_path::{AnchorPath, Contour, EditAnchor};
use hsk_studio_nib::{FillRule, Mirroring, OwnedPath, PathElement, PathPoint};
use std::path::Path;

/// Document pixels per point.
const PX: f64 = 96.0 / 72.0;

/// Press's PDF reading as the interop-vector drawing port.
pub struct PressDrawing;

fn els(path: &[hsk_studio_press::VectorEl]) -> Vec<iv::PdfEl> {
    path.iter()
        .map(|e| match *e {
            hsk_studio_press::VectorEl::MoveTo(a) => iv::PdfEl::MoveTo(a),
            hsk_studio_press::VectorEl::LineTo(a) => iv::PdfEl::LineTo(a),
            hsk_studio_press::VectorEl::CubicTo(a, b, c) => iv::PdfEl::CubicTo(a, b, c),
            hsk_studio_press::VectorEl::Close => iv::PdfEl::Close,
        })
        .collect()
}

fn paint(p: &hsk_studio_press::VectorPaint) -> iv::PdfPaint {
    match p {
        hsk_studio_press::VectorPaint::Solid(c) => iv::PdfPaint::Solid(*c),
        hsk_studio_press::VectorPaint::Gradient { axial, coords, matrix, stops, extend } => iv::PdfPaint::Gradient { axial: *axial, coords: *coords, matrix: *matrix, stops: stops.clone(), extend: *extend },
    }
}

impl iv::PdfDrawingProvider for PressDrawing {
    fn pages(&self, pdf: &[u8], cancel: &CancellationToken) -> iv::Result<Vec<iv::PdfDrawingPage>> {
        let pages = hsk_studio_press::read_vector_pages(pdf, iv::Limits::default().max_bytes, cancel).map_err(|e| match e {
            hsk_studio_press::PressError::Canceled => iv::VectorError::Canceled,
            _ => iv::VectorError::PdfUnavailable,
        })?;
        Ok(pages
            .into_iter()
            .map(|p| iv::PdfDrawingPage {
                media_box: p.media_box,
                crop_box: p.crop_box,
                items: p
                    .items
                    .iter()
                    .map(|i| match i {
                        hsk_studio_press::VectorItem::Draw { path, mode: hsk_studio_press::VectorMode::Fill { even_odd }, paint: q, blend, glyph } => iv::PdfDrawItem::Fill { path: els(path), even_odd: *even_odd, paint: paint(q), blend: *blend, glyph: *glyph },
                        hsk_studio_press::VectorItem::Draw { path, mode: hsk_studio_press::VectorMode::Stroke(s), paint: q, blend, glyph } => iv::PdfDrawItem::Stroke {
                            path: els(path),
                            stroke: iv::PdfStroke { width: s.width, cap: s.cap, join: s.join, miter_limit: s.miter_limit, dash: s.dash.clone(), dash_offset: s.dash_offset },
                            paint: paint(q),
                            blend: *blend,
                            glyph: *glyph,
                        },
                        hsk_studio_press::VectorItem::PushClip { path, even_odd } => iv::PdfDrawItem::PushClip { path: els(path), even_odd: *even_odd },
                        hsk_studio_press::VectorItem::PopClip => iv::PdfDrawItem::PopClip,
                        hsk_studio_press::VectorItem::PushGroup { opacity, blend } => iv::PdfDrawItem::PushGroup { opacity: *opacity, blend: *blend },
                        hsk_studio_press::VectorItem::PopGroup => iv::PdfDrawItem::PopGroup,
                    })
                    .collect(),
                notes: p.notes.iter().map(|n| n.code()).collect(),
            })
            .collect())
    }
}

/// Deterministic ids from the source digest (`uuid_v7`-shaped; the 4th group starts with 8).
struct DigestMint {
    stem: String,
    next: u64,
}
impl iv::IdMint for DigestMint {
    fn mint(&mut self, prefix: &'static str) -> String {
        let n = self.next;
        self.next = self.next.saturating_add(1);
        format!("{prefix}-{}-{}-7a10-8000-{n:012x}", self.stem.get(0..8).unwrap_or("00000000"), self.stem.get(8..12).unwrap_or("0000"))
    }
}

fn scale_len(l: &Length) -> Length {
    Length { value: l.value * PX, unit: GeometryUnit::Px }
}

/// Points -> document pixels for a PAINT style (stroke lengths, gradient transforms).
fn scale_rows(rows: &mut [StudioAppearanceRow]) {
    let scale_paint = |p: &mut StudioPaint| {
        if let StudioPaint::Gradient { gradient } = p {
            gradient.transform = gradient.transform.map(|v| v * PX);
        }
    };
    for r in rows {
        match r {
            StudioAppearanceRow::Fill { paint, .. } => scale_paint(paint),
            StudioAppearanceRow::Stroke { paint, stroke, .. } => {
                scale_paint(paint);
                stroke.weight = scale_len(&stroke.weight);
                stroke.dash = stroke.dash.iter().map(scale_len).collect();
                stroke.dash_offset = scale_len(&stroke.dash_offset);
            }
        }
    }
}

/// The proposal's anchor paths (points) as one anchor path record (document pixels).
fn record(p: &iv::VectorPath) -> Result<PathRecord, String> {
    let anchors = p
        .anchors
        .iter()
        .map(|a| EditAnchor {
            position: PathPoint::new(a.position.x.value() * PX, a.position.y.value() * PX),
            incoming: PathPoint::new(a.incoming.x.value() * PX, a.incoming.y.value() * PX),
            outgoing: PathPoint::new(a.outgoing.x.value() * PX, a.outgoing.y.value() * PX),
            mirroring: Mirroring::None,
        })
        .collect();
    let rule = if p.winding == hsk_studio_nib::Winding::EvenOdd { FillRule::EvenOdd } else { FillRule::NonZero };
    let path = AnchorPath { contours: vec![Contour { anchors, closed: p.closed }], fill_rule: rule };
    path.validate().map_err(|e| format!("ai_import: path {}", e.code()))?;
    Ok(PathRecord { path_id: p.path_id.clone(), path, style: VectorStyle { fill: None, stroke: None, stroke_width: 0.0 }, network: None, width: None })
}

/// A native package of the `.ai` file and the import losses (module docs).
pub fn ai_to_package(bytes: &[u8], cancel: &CancellationToken) -> Result<(Vec<u8>, Vec<String>), String> {
    let digest = hsk_studio_package::sha256_hex_of(bytes);
    let mut mint = DigestMint { stem: digest.clone(), next: 1 };
    let proposal = iv::import_ai_with(bytes, &iv::ImportPolicy::default(), &PressDrawing, &mut mint, cancel).map_err(|e| format!("ai_import: {e:?}"))?;
    let losses: Vec<String> = proposal.loss.iter().map(|l| format!("{}: {}", l.path, l.code)).collect();
    let mut doc = proposal.snapshot.document().clone();
    doc.geometry_unit = GeometryUnit::Px;
    let mut losses = losses;
    for a in &mut doc.artboards {
        // The viewer frames whole pixels: artboard origins round, sizes round up.
        let raw = [a.bounds.x.value * PX, a.bounds.y.value * PX, a.bounds.width.value * PX, a.bounds.height.value * PX];
        let whole = [raw[0].round(), raw[1].round(), raw[2].ceil().max(1.0), raw[3].ceil().max(1.0)];
        if raw != whole {
            losses.push(format!("{}: artboard_rounded_to_pixels", a.artboard_id));
        }
        for (l, v) in [&mut a.bounds.x, &mut a.bounds.y, &mut a.bounds.width, &mut a.bounds.height].into_iter().zip(whole) {
            *l = Length { value: v, unit: GeometryUnit::Px };
        }
    }
    // The viewer composites through the layer graph and frames in the RGB working space: every
    // layer gets its operation (groups composite, the rest are sources), children feed their
    // parent bottom first, root layers are the outputs; the working space is the harness's.
    let mut wire_doc = serde_json::to_value(&doc).map_err(|e| format!("ai_import: {e}"))?;
    let (mut operations, mut edges, mut outputs) = (Vec::new(), Vec::new(), Vec::new());
    let mut ordinal: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for l in &doc.layers {
        let group = matches!(l.payload, folio::Payload::Group);
        operations.push(serde_json::to_value(super::super::layers::operation_for(&l.layer_id, group)?).map_err(|e| format!("ai_import: {e}"))?);
        let key = if group { "mix" } else { "source" };
        let out = serde_json::json!({"layer_id": l.layer_id, "operation_key": key, "port_key": "out"});
        match &l.parent {
            folio::Parent::Node(n) => {
                let next = ordinal.entry(n.node_id.clone()).or_insert(0);
                edges.push(serde_json::json!({"source": out, "target": {"layer_id": n.node_id, "operation_key": "mix", "port_key": "inputs"}, "dependency_kind": "data", "input_ordinal": *next}));
                *next = next.saturating_add(1);
            }
            _ => outputs.push(out),
        }
    }
    wire_doc["graph"] = serde_json::json!({"schema_id": "hsk.studio.layer_graph@1", "operations": operations, "edges": edges, "outputs": outputs});
    wire_doc["working_space_rgb"] = serde_json::json!(format!("SCPF-{}-{}-7a10-8000-000000000001", digest.get(0..8).unwrap_or("00000000"), digest.get(8..12).unwrap_or("0000")));
    let wire = serde_json::to_vec(&wire_doc).map_err(|e| format!("ai_import: {e}"))?;
    let snapshot = match folio::inspect_bytes(&wire, folio::Budget::default(), cancel, &crate::harness::TrustAll) {
        Ok(folio::Inspection::Editable(s)) => s,
        Ok(folio::Inspection::ReadOnly(_)) => return Err("ai_import: folio_read_only".into()),
        Err(d) => return Err(format!("ai_import: folio {:?} {}", d.code, d.target)),
    };
    let mut design = proposal.design.clone();
    for s in &mut design.paint_styles {
        scale_rows(&mut s.rows);
    }
    let mut entry = VectorEntry::empty();
    entry.paths = proposal.paths.iter().map(record).collect::<Result<_, _>>()?;
    entry.design = Some(design);
    entry.records = proposal.vector_records.clone();
    entry.validate(snapshot.document(), cancel)?;
    entry.validate_records(snapshot.document(), cancel)?;
    let entry_bytes = entry.to_bytes(&snapshot.document().document_id)?;
    let entry_sha = hsk_studio_package::sha256_hex_of(&entry_bytes);
    let original = hsk_studio_package::ImportEntry::new(hsk_studio_package::ImportRole::Original, bytes.to_vec());
    let receipt = serde_json::json!({"receipt": "import", "receipt_version": 1, "source_format": "ai", "importer": "hsk-studio-interop-vector import_ai_with over hsk-studio-press read_vector_pages", "source": {"sha256": digest, "byte_len": bytes.len(), "entry": original.name()}, "loss": losses});
    let receipt = hsk_studio_package::ImportEntry::new(hsk_studio_package::ImportRole::Receipt, serde_json::to_vec(&receipt).map_err(|e| format!("ai_import: {e}"))?);
    let held = vec![
        hsk_studio_package::Quarantined { name: format!("{}{ENTRY}.{entry_sha}", hsk_studio_session::DOCUMENT_ENTRY_PREFIX), bytes: entry_bytes, sha256_hex: entry_sha },
        receipt.to_opaque(),
        original.to_opaque(),
    ];
    let mut out = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &hsk_studio_package::MemoryAssetSource::new(),
        &hsk_studio_package::Extras { quarantined: &held, previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        cancel,
        &mut out,
    )
    .map_err(|e| format!("ai_import: package {}", e.code()))?;
    Ok((out, losses))
}

/// An expanded shape as anchor paths in points (lines and cubics; quadratics raised).
fn anchors_pt(path: &OwnedPath, layer_id: &str, ids: &mut dyn FnMut() -> String) -> Result<Vec<iv::VectorPath>, String> {
    let k = 0.75;
    let pt = |x: f64, y: f64| hsk_studio_nib::Point::pt(x * k, y * k).map_err(|_| "svg_export: geometry".to_string());
    let mut out = Vec::new();
    let mut cur: Vec<(PathPoint, PathPoint, PathPoint)> = Vec::new();
    let mut closed = false;
    let winding = if path.fill_rule == FillRule::EvenOdd { hsk_studio_nib::Winding::EvenOdd } else { hsk_studio_nib::Winding::NonZero };
    let mut flush = |cur: &mut Vec<(PathPoint, PathPoint, PathPoint)>, closed: bool, out: &mut Vec<iv::VectorPath>| -> Result<(), String> {
        if cur.len() >= 2 || (closed && !cur.is_empty()) {
            let anchors = cur
                .iter()
                .map(|(p, cin, cout)| {
                    Ok(hsk_studio_nib::Anchor { position: pt(p.x, p.y)?, incoming: pt(cin.x - p.x, cin.y - p.y)?, outgoing: pt(cout.x - p.x, cout.y - p.y)?, handle_mirroring: Mirroring::None })
                })
                .collect::<Result<Vec<_>, String>>()?;
            out.push(iv::VectorPath { path_id: ids(), layer_id: layer_id.to_string(), closed, anchors, winding });
        }
        cur.clear();
        Ok(())
    };
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(q) => {
                flush(&mut cur, closed, &mut out)?;
                closed = false;
                cur.push((q, q, q));
            }
            PathElement::LineTo(q) => cur.push((q, q, q)),
            PathElement::QuadTo(c, q) => {
                let last = cur.last().map_or(q, |a| a.0);
                let c1 = PathPoint::new(last.x + (c.x - last.x) * 2.0 / 3.0, last.y + (c.y - last.y) * 2.0 / 3.0);
                let c2 = PathPoint::new(q.x + (c.x - q.x) * 2.0 / 3.0, q.y + (c.y - q.y) * 2.0 / 3.0);
                if let Some(a) = cur.last_mut() {
                    a.2 = c1;
                }
                cur.push((q, c2, q));
            }
            PathElement::CubicTo(a, b, q) => {
                if let Some(l) = cur.last_mut() {
                    l.2 = a;
                }
                cur.push((q, b, q));
            }
            PathElement::Close => {
                closed = true;
                if cur.len() > 1 && cur.first().map(|f| f.0) == cur.last().map(|l| l.0) {
                    let last = cur.pop();
                    if let (Some(f), Some(l)) = (cur.first_mut(), last) {
                        f.1 = l.1;
                    }
                }
            }
        }
    }
    flush(&mut cur, closed, &mut out)?;
    Ok(out)
}

/// The folio paint of an expanded row paint (plan colours are encoded sRGB here).
fn folio_paint(p: &FillPaint, opacity: f32) -> Option<(StudioPaint, f64)> {
    let colour = |c: [f32; 4]| folio::StudioColor { r: f64::from(c[0]), g: f64::from(c[1]), b: f64::from(c[2]), colour_profile_id: folio::ProfileBinding::Unassigned(()) };
    match p {
        FillPaint::Solid(c) => Some((StudioPaint::Solid { colour: colour(*c) }, f64::from(c[3] * opacity) * 100.0)),
        FillPaint::Gradient(g) => {
            let geometry = match g.geometry {
                hsk_studio_nib::raster::GradientGeometry::Linear { x1, y1, x2, y2 } => folio::StudioGradientGeometry::Linear { x1, y1, x2, y2 },
                hsk_studio_nib::raster::GradientGeometry::Radial { cx, cy, r, fx, fy, fr } => folio::StudioGradientGeometry::Radial { cx, cy, r, fx, fy, fr },
            };
            let stops = g.stops.iter().map(|s| folio::StudioGradientStop { position: s.offset * 100.0, midpoint: s.midpoint.map(|m| m * 100.0), opacity: f64::from(s.colour[3]) * 100.0, colour: colour(s.colour) }).collect();
            let spread = match g.spread {
                hsk_studio_nib::raster::Spread::Pad => folio::GradientSpread::Pad,
                hsk_studio_nib::raster::Spread::Repeat => folio::GradientSpread::Repeat,
                hsk_studio_nib::raster::Spread::Reflect => folio::GradientSpread::Reflect,
            };
            // Gradient space -> px, then px -> pt.
            let transform = g.transform.map(|v| v * 0.75);
            Some((StudioPaint::Gradient { gradient: folio::StudioGradient { geometry, transform, spread, interpolation: folio::GradientInterpolation::Classic, dither: false, stops } }, f64::from(opacity) * 100.0))
        }
        _ => None,
    }
}

impl Viewer {
    /// Converts the `.ai` file `source` into a native package at `dest` (never over an existing
    /// file without `overwrite`) and opens it. Returns the import losses.
    pub fn import_ai(&mut self, source: &Path, dest: &Path, overwrite: bool) -> Result<Vec<String>, String> {
        let r = (|| {
            if dest.exists() && !overwrite {
                return Err(format!("ai_import: {} exists", dest.display()));
            }
            let bytes = std::fs::read(source).map_err(|e| format!("ai_import: {e}"))?;
            let (package, losses) = ai_to_package(&bytes, self.cancel)?;
            super::super::export::write_atomic(dest, &package).map_err(|e| format!("ai_import: {e}"))?;
            self.open(dest)?;
            Ok(losses)
        })();
        self.report(r)
    }

    /// The open vector document as SVG text (points) and the typed losses (module docs).
    pub fn vector_svg(&self) -> Result<(String, Vec<String>), String> {
        let snap = self.run.snapshot().ok_or("svg_export: nothing open")?;
        let doc = snap.document();
        let entry = self.vector_entry()?;
        let design = effective_design(doc, &entry)?;
        let records = entry.folio_records(&doc.document_id);
        let (layers, refused) = expand_vectors(snap, &design, &records, &EntryAssets(&entry), hsk_studio_composite::BlendSpace::Encoded, &VectorResolution::default(), self.cancel)
            .map_err(|e| format!("svg_export: {e:?}"))?;
        let mut losses: Vec<String> = refused.iter().map(|(l, c)| format!("{l}: refused:{c}")).collect();
        for a in &records.attachments {
            losses.push(format!("{}: vector_clip_or_mask_not_exported", a.layer_id));
        }
        let mut out_design = folio::StudioDesignRecords::new(doc.document_id.clone());
        let mut paths = Vec::new();
        let mut n = 0_u64;
        for layer in &layers {
            if !doc.layers.iter().any(|l| l.layer_id == layer.layer_id && l.visible) {
                continue;
            }
            for row in &layer.rows {
                let Some((paint, opacity)) = folio_paint(&row.paint, row.opacity) else {
                    losses.push(format!("{}: paint_not_exported", layer.layer_id));
                    continue;
                };
                n = n.saturating_add(1);
                let id = |p: &str| format!("{p}-019abcde-0000-7000-8000-{n:012x}");
                let shape_layer = id("SLYR");
                let style_id = id("SSTY");
                let mut k = 0_u64;
                let mut path_id = || {
                    k = k.saturating_add(1);
                    format!("SVPT-019abcde-{:04x}-7000-8000-{k:012x}", n & 0xffff)
                };
                for p in &row.paths {
                    paths.extend(anchors_pt(p, &shape_layer, &mut path_id)?);
                }
                let fill = StudioAppearanceRow::Fill { paint, opacity: opacity.clamp(0.0, 100.0), blend_mode: row.mode as u16, visible: true };
                out_design.paint_styles.push(serde_json::from_value(serde_json::json!({
                    "schema_id": "hsk.studio.style_registry@1", "style_id": style_id, "kind": "PAINT", "name": format!("export/{n}"),
                    "publishing": {"description": "", "description_markdown": "", "documentation_links": [], "publish_key": format!("export-{n}"), "is_remote": false},
                    "rows": [fill],
                })).map_err(|e| format!("svg_export: {e}"))?);
                out_design.nodes.push(serde_json::from_value(serde_json::json!({"node": {"node_kind": "layer", "node_id": shape_layer}, "style_refs": {"fill_style_id": style_id}})).map_err(|e| format!("svg_export: {e}"))?);
            }
        }
        let board = doc.artboards.first().ok_or("svg_export: the document has no artboard")?;
        let opts = iv::ExportOptions { pixels_per_inch: 96.0, artboard_pt: Some([board.bounds.width.value * 0.75, board.bounds.height.value * 0.75]) };
        let svg = iv::export_svg_with_loss(&paths, &out_design, &opts).map_err(|e| format!("svg_export: {e:?}"))?;
        losses.extend(svg.loss.iter().map(|l| format!("{}: {}", l.path, l.code)));
        Ok((svg.text, losses))
    }

    /// Writes [`Viewer::vector_svg`] to `path` (atomically; never over an existing file without
    /// `overwrite`). Returns the losses.
    pub fn export_vector_svg(&mut self, path: &Path, overwrite: bool) -> Result<Vec<String>, String> {
        let r = (|| {
            if path.exists() && !overwrite {
                return Err(format!("svg_export: {} exists", path.display()));
            }
            let (text, losses) = self.vector_svg()?;
            super::super::export::write_atomic(path, text.as_bytes()).map_err(|e| format!("svg_export: {e}"))?;
            Ok(losses)
        })();
        self.report(r)
    }
}
