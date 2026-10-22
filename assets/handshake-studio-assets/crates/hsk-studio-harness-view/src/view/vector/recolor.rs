//! Recolour artwork in the viewer (lane SMB-AI-TYPE-PATH-RECOLOR; STU-VEC-052, STU-VEC-142,
//! STU-COL-236, STU-COL-172). The colour maths is Prism's (`hsk_studio_prism::recolor`); this module
//! extracts the colours of the selected vector layers and writes the mapped colours back as ONE
//! undo step.
//!
//! Extraction ([`Viewer::artwork_colours`]): every fill and stroke row of each layer (its own
//! style, a linked graphic style or its synthesised 8-bit rows): a solid paint gives its colour, a
//! gradient each stop colour. A row bound to a GLOBAL or PROCESS RGB swatch (directly or through a
//! tint of it) contributes the swatch's own colour once per binding, tagged with the swatch id.
//! Prominence weight = the bounding-box area of the layer's path (document px^2) per use, split
//! evenly over a gradient's stops (Studio rule: area coverage of the bounds, not the painted
//! area).
//!
//! Apply ([`Viewer::recolor_artwork`]): each colour maps through the table then the sliders
//! (`hsk_studio_prism::recolor`). Unbound rows get the mapped colour in place (the layer's own
//! appearance style; a linked graphic style is replaced by the layer's own copy, as any appearance
//! edit does). A global RGB swatch is redefined to its mapped colour and every row bound to it or
//! to a tint of it follows (global propagation); a process RGB swatch binding is detached and the
//! row recoloured locally (process swatches are not global). Rows bound to CMYK, Lab, Gray, spot
//! or gradient swatches are left unchanged and reported (`swatch_not_recoloured:<model>`): the
//! mapping is defined on the working RGB triple and the harness has no RGB -> CMYK/Lab inverse.
//! Illustrator Recolor Artwork parity is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::style::{current_rows, layer_style_id, node, paint_style, rebind, MAX_STYLES};
use super::swatch::propagate;
use super::VectorEntry;
use hsk_studio_folio::{self as folio, Payload, StudioAppearanceRow, StudioColorModel, StudioDesignRecords, StudioPaint, StudioSwatchKind};
use hsk_studio_nib::PathElement;
use hsk_studio_prism::{RecolorAdjust, RecolorTable, WeightedColour, recolor};
use std::collections::{BTreeMap, BTreeSet};

/// One distinct colour of the selected artwork.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtworkColour {
    pub rgb: [f64; 3],
    /// Summed prominence weight (module docs).
    pub weight: f64,
    /// Row uses (solid rows and gradient stops).
    pub uses: usize,
    /// The global or process RGB swatch the colour is, when it comes from one.
    pub swatch_id: Option<String>,
}

/// What [`Viewer::recolor_artwork`] changed and left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecolorReport {
    pub layers: Vec<String>,
    pub swatches: Vec<String>,
    /// Typed notes `<layer or swatch>: <code>`.
    pub skipped: Vec<String>,
}

/// The swatch whose colour a binding resolves to: the tint base chain's root.
fn root_swatch<'a>(records: &'a folio::StudioVectorRecords, swatch_id: &str) -> Option<&'a folio::StudioSwatch> {
    let mut s = records.swatch(swatch_id)?;
    for _ in 0..16 {
        if s.swatch_kind != StudioSwatchKind::Tint {
            return Some(s);
        }
        s = records.swatch(s.base_swatch_ref.as_deref()?)?;
    }
    None
}

/// An RGB global/process swatch's colour, or why it is not recolourable.
fn swatch_rgb(s: &folio::StudioSwatch) -> Result<[f64; 3], String> {
    match (s.swatch_kind, s.color_model) {
        (StudioSwatchKind::Global | StudioSwatchKind::Process, Some(StudioColorModel::Rgb)) if s.components.len() == 3 => Ok([s.components[0], s.components[1], s.components[2]]),
        (StudioSwatchKind::Gradient, _) => Err("swatch_not_recoloured:gradient".into()),
        (StudioSwatchKind::Spot, _) => Err("swatch_not_recoloured:spot".into()),
        (_, Some(m)) => Err(format!("swatch_not_recoloured:{}", serde_json::to_value(m).ok().and_then(|v| v.as_str().map(str::to_lowercase)).unwrap_or_default())),
        _ => Err("swatch_not_recoloured:unknown".into()),
    }
}

/// Bounding-box area of a layer's path (document px^2).
fn bbox_area(record: &super::PathRecord) -> f64 {
    let Ok(path) = record.path.to_owned_path() else { return 0.0 };
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    let mut add = |x: f64, y: f64| {
        lo = [lo[0].min(x), lo[1].min(y)];
        hi = [hi[0].max(x), hi[1].max(y)];
    };
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(p) | PathElement::LineTo(p) => add(p.x, p.y),
            PathElement::QuadTo(a, p) => {
                add(a.x, a.y);
                add(p.x, p.y);
            }
            PathElement::CubicTo(a, b, p) => {
                add(a.x, a.y);
                add(b.x, b.y);
                add(p.x, p.y);
            }
            PathElement::Close => {}
        }
    }
    let a = (hi[0] - lo[0]) * (hi[1] - lo[1]);
    if a.is_finite() && a > 0.0 { a } else { 0.0 }
}

fn rgb(c: &folio::StudioColor) -> [f64; 3] {
    [c.r, c.g, c.b]
}

/// Colours of one paint with the share of `weight` each gets.
fn paint_colours(paint: &StudioPaint, weight: f64, out: &mut Vec<([f64; 3], f64)>) {
    match paint {
        StudioPaint::Solid { colour } => out.push((rgb(colour), weight)),
        StudioPaint::Gradient { gradient } => {
            let n = gradient.stops.len().max(1) as f64;
            for s in &gradient.stops {
                out.push((rgb(&s.colour), weight / n));
            }
        }
    }
}

fn map_paint(paint: &mut StudioPaint, f: &dyn Fn([f64; 3]) -> [f64; 3]) -> bool {
    let mut changed = false;
    let mut set = |c: &mut folio::StudioColor| {
        let [r, g, b] = f(rgb(c));
        if [r, g, b] != rgb(c) {
            (c.r, c.g, c.b) = (r, g, b);
            changed = true;
        }
    };
    match paint {
        StudioPaint::Solid { colour } => set(colour),
        StudioPaint::Gradient { gradient } => gradient.stops.iter_mut().for_each(|s| set(&mut s.colour)),
    }
    changed
}

fn row_paint(row: &mut StudioAppearanceRow) -> &mut StudioPaint {
    match row {
        StudioAppearanceRow::Fill { paint, .. } | StudioAppearanceRow::Stroke { paint, .. } => paint,
    }
}

impl Viewer {
    /// The vector layers a recolour addresses: `layer_ids`, or every vector layer when empty.
    fn recolor_layers(&self, layer_ids: &[String]) -> Result<Vec<String>, String> {
        let doc = self.vdoc()?;
        let vector = |id: &str| doc.layers.iter().any(|l| l.layer_id == id && matches!(l.payload, Payload::Vector { .. }));
        if layer_ids.is_empty() {
            return Ok(doc.layers.iter().filter(|l| matches!(l.payload, Payload::Vector { .. })).map(|l| l.layer_id.clone()).collect());
        }
        for id in layer_ids {
            if !vector(id) {
                return Err(format!("recolor: {id} is not a vector layer"));
            }
        }
        Ok(layer_ids.to_vec())
    }

    /// Distinct colours of the selected artwork (module docs), most prominent first.
    pub fn artwork_colours(&self, layer_ids: &[String]) -> Result<Vec<ArtworkColour>, String> {
        let doc = self.vdoc()?;
        let entry = self.vector_entry()?;
        let mut uses: Vec<([f64; 3], f64, Option<String>)> = Vec::new();
        for id in self.recolor_layers(layer_ids)? {
            let record = self.vector_path(&id)?;
            let area = bbox_area(&record);
            let (rows, style_id) = current_rows(&entry, doc, &id, &record)?;
            for (i, row) in rows.iter().enumerate() {
                let bound = style_id.as_deref().and_then(|s| entry.records.swatch_paint(s, u32::try_from(i).ok()?));
                if let Some(root) = bound.and_then(|b| root_swatch(&entry.records, &b.swatch_id))
                    && let Ok(c) = swatch_rgb(root)
                {
                    uses.push((c, area, Some(root.swatch_id.clone())));
                    continue;
                }
                let paint = match row {
                    StudioAppearanceRow::Fill { paint, .. } | StudioAppearanceRow::Stroke { paint, .. } => paint,
                };
                let mut found = Vec::new();
                paint_colours(paint, area, &mut found);
                uses.extend(found.into_iter().map(|(c, w)| (c, w, None)));
            }
        }
        let weighted: Vec<WeightedColour> = uses.iter().map(|(c, w, _)| WeightedColour { rgb: *c, weight: *w }).collect();
        let distinct = hsk_studio_prism::extract_colours(&weighted).map_err(|e| format!("recolor: {}", e.code()))?;
        Ok(distinct
            .into_iter()
            .map(|d| {
                let mine = uses.iter().filter(|u| u.0 == d.rgb);
                ArtworkColour { rgb: d.rgb, weight: d.weight, uses: mine.clone().count(), swatch_id: mine.filter_map(|u| u.2.clone()).next() }
            })
            .collect())
    }

    /// Recolours the selected artwork (module docs) in ONE undo step.
    pub fn recolor_artwork(&mut self, layer_ids: &[String], table: &RecolorTable, adjust: &RecolorAdjust) -> Result<RecolorReport, String> {
        let r = (|| {
            table.validate().map_err(|e| format!("recolor: {}", e.code()))?;
            adjust.validate().map_err(|e| format!("recolor: {}", e.code()))?;
            let doc = self.vdoc()?;
            let mut entry: VectorEntry = self.vector_entry()?;
            let f = |c: [f64; 3]| recolor(table, adjust, c);
            let mut report = RecolorReport::default();
            let mut swatches: BTreeSet<String> = BTreeSet::new();
            let mut skipped: BTreeSet<String> = BTreeSet::new();
            for id in self.recolor_layers(layer_ids)? {
                let record = self.vector_path(&id)?;
                let (before, from) = current_rows(&entry, doc, &id, &record)?;
                let mut rows = before.clone();
                let mut changed = false;
                let mut detach: Vec<u32> = Vec::new();
                for (i, row) in rows.iter_mut().enumerate() {
                    let n = u32::try_from(i).map_err(|_| "recolor: rows")?;
                    let bound = from.as_deref().and_then(|s| entry.records.swatch_paint(s, n)).cloned();
                    if let Some(b) = bound {
                        let Some(root) = root_swatch(&entry.records, &b.swatch_id) else {
                            skipped.insert(format!("{id}: swatch_unresolved"));
                            continue;
                        };
                        match swatch_rgb(root) {
                            Ok(_) if root.swatch_kind == StudioSwatchKind::Global => {
                                swatches.insert(root.swatch_id.clone());
                                continue;
                            }
                            // A process swatch row is detached only when its colour changes.
                            Ok(old) if f(old) == old => continue,
                            Ok(_) => detach.push(n),
                            Err(code) => {
                                skipped.insert(format!("{}: {code}", root.swatch_id));
                                continue;
                            }
                        }
                    }
                    changed |= map_paint(row_paint(row), &f);
                }
                if !(changed || !detach.is_empty()) {
                    continue;
                }
                // The layer's own appearance style takes the rows (bindings follow, detached ones drop).
                let style_id = layer_style_id(&id)?;
                let style = paint_style(&style_id, "Layers/appearance", "appearance", &rows)?;
                let design = entry.design.get_or_insert_with(|| StudioDesignRecords::new(doc.document_id.clone()));
                if design.paint_styles.len() >= MAX_STYLES {
                    return Err("recolor: style budget".into());
                }
                design.paint_styles.retain(|s| s.style_id != style_id);
                design.paint_styles.push(style);
                design.nodes.retain(|n| n.node.node_id != id);
                design.nodes.push(node(&id, &style_id)?);
                // Bindings move onto the layer's own style (a shared graphic style keeps its own);
                // detached process-swatch rows then lose theirs on the layer's style only.
                let source = from.clone().unwrap_or_else(|| style_id.clone());
                rebind(&mut entry.records, &source, if from.is_some() { &before } else { &[] }, &style_id, &rows, None);
                entry.records.swatch_paints.retain(|p| !(p.style_id == style_id && detach.contains(&p.row)));
                report.layers.push(id);
            }
            // Global swatches: redefine, then every bound row and tint follows.
            let mut redefined: BTreeMap<String, [f64; 3]> = BTreeMap::new();
            for sid in &swatches {
                let Some(s) = entry.records.swatch(sid) else { continue };
                let old = swatch_rgb(s)?;
                let new = f(old);
                if new != old {
                    redefined.insert(sid.clone(), new);
                }
            }
            for (sid, new) in &redefined {
                if let Some(s) = entry.records.swatches.iter_mut().find(|s| &s.swatch_id == sid) {
                    s.components = new.to_vec();
                }
                propagate(&mut entry, doc, sid)?;
                report.swatches.push(sid.clone());
            }
            report.skipped = skipped.into_iter().collect();
            if report.layers.is_empty() && report.swatches.is_empty() {
                return Err("recolor: nothing changed".into());
            }
            self.validate_records(&entry)?;
            let target = report.layers.first().or(report.swatches.first()).cloned().unwrap_or_default();
            self.commit_entry("recolor_artwork", &target, &entry)?;
            Ok(report)
        })();
        self.report(r)
    }
}
