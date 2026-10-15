//! Art, scatter and pattern brushes in the viewer (lane SMB-ILLUSTRATOR-TOOLS-2; STU-VEC-026/027,
//! STU-VEC-150, STU-VEC-152, STU-VEC-162/163). A brush is a Folio `StudioVectorBrush` in the
//! entry's vector records; its art is copied from vector layers at creation (each path a fresh
//! `SVPT` record in the entry, coloured with the layer's first solid fill, else its 8-bit fill,
//! else black). Applying a brush binds the layer's first stroke row to it (`StudioBrushStroke`,
//! with an optional per-path scale override), and composite draws the brush along the stroke
//! through nib. Create, apply and remove are each ONE undo step.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::style::{Bind, StrokePanel, current_rows, stroke_row};
use super::{PathRecord, VectorEntry, mint_path_id};
use hsk_studio_folio::{self as folio, Payload, StudioAppearanceRow, StudioBrushArt, StudioBrushColorization, StudioBrushKind, StudioPatternTiles, StudioVectorBrush};

/// Source layers of the five pattern tile slots.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatternTileLayers {
    pub side: Vec<String>,
    pub outer_corner: Vec<String>,
    pub inner_corner: Vec<String>,
    pub start: Vec<String>,
    pub end: Vec<String>,
}

impl Viewer {
    /// Brushes of the document: `(brush id, name)`.
    pub fn brushes(&self) -> Result<Vec<(String, String)>, String> {
        Ok(self.vector_entry()?.records.brushes.iter().map(|b| (b.brush_id.clone(), b.name.clone())).collect())
    }

    /// Creates a brush named `name` of `kind` (art, scatter or pattern parameters; its art lists
    /// are filled here) from the art of `art_layers` (art and scatter) or `tiles` (pattern): ONE
    /// undo step. Returns the brush id.
    pub fn add_brush(&mut self, name: &str, kind: StudioBrushKind, colorization: StudioBrushColorization, art_layers: &[String], tiles: &PatternTileLayers) -> Result<String, String> {
        let r = (|| {
            let doc = self.vdoc()?.clone();
            let mut entry = self.vector_entry()?;
            if entry.records.brushes.len() >= folio::TOOLS_MAX_ENTRIES {
                return Err("brush: budget".into());
            }
            let copy = |layers: &[String], entry: &mut VectorEntry| -> Result<Vec<StudioBrushArt>, String> {
                let mut out = Vec::new();
                for l in layers {
                    out.extend(art_of(&doc, entry, l)?);
                }
                Ok(out)
            };
            let kind = match kind {
                StudioBrushKind::Art { art_width, art_scale_mode, art_guide_start, art_guide_end, flip_along, flip_across, .. } => {
                    StudioBrushKind::Art { art_width, art_scale_mode, art_guide_start, art_guide_end, flip_along, flip_across, art: copy(art_layers, &mut entry)? }
                }
                StudioBrushKind::Scatter { scatter_size, scatter_spacing, scatter_offset, scatter_rotation, scatter_rotation_relative_to, seed, .. } => {
                    StudioBrushKind::Scatter { scatter_size, scatter_spacing, scatter_offset, scatter_rotation, scatter_rotation_relative_to, seed, art: copy(art_layers, &mut entry)? }
                }
                StudioBrushKind::Pattern { pattern_brush_scale, fit_mode, flip_along, flip_across, show_auto_generated_corner_tiles, .. } => StudioBrushKind::Pattern {
                    pattern_brush_scale,
                    fit_mode,
                    flip_along,
                    flip_across,
                    show_auto_generated_corner_tiles,
                    tiles: StudioPatternTiles {
                        side: copy(&tiles.side, &mut entry)?,
                        outer_corner: copy(&tiles.outer_corner, &mut entry)?,
                        inner_corner: copy(&tiles.inner_corner, &mut entry)?,
                        start: copy(&tiles.start, &mut entry)?,
                        end: copy(&tiles.end, &mut entry)?,
                    },
                },
                other => other,
            };
            let mut n = entry.records.brushes.len();
            let id = loop {
                let id = format!("brush-{n}");
                if entry.records.brush(&id).is_none() {
                    break id;
                }
                n = n.checked_add(1).ok_or("brush: id space")?;
            };
            entry.records.brushes.push(StudioVectorBrush { brush_id: id.clone(), name: name.to_string(), kind, colorization });
            self.validate_records(&entry)?;
            self.commit_entry("add_brush", &id, &entry)?;
            Ok(id)
        })();
        self.report(r)
    }

    /// Paints `layer_id`'s stroke with brush `brush_id` (a layer without a stroke gets a 1 px black
    /// one), with an optional per-path scale override percent: ONE undo step.
    pub fn apply_brush(&mut self, layer_id: &str, brush_id: &str, scale_override: Option<f64>) -> Result<String, String> {
        let r = (|| {
            if self.vector_entry()?.records.brush(brush_id).is_none() {
                return Err(format!("brush: unknown {brush_id}"));
            }
            if scale_override.is_some_and(|v| !(v.is_finite() && v > 0.0 && v <= 100_000.0)) {
                return Err("brush: scale override".into());
            }
            let bind = Bind::Brush { brush_id: brush_id.to_string(), scale_override };
            self.restyle("apply_brush", layer_id, Some(bind), |rows, unit| {
                if !rows.iter().any(|r| matches!(r, StudioAppearanceRow::Stroke { .. })) {
                    rows.push(stroke_row(&StrokePanel::solid([0, 0, 0], 1.0), unit)?);
                }
                Ok(None)
            })
        })();
        self.report(r)
    }

    /// Removes the brush from `layer_id`'s stroke (it strokes plainly again): ONE undo step.
    pub fn remove_brush(&mut self, layer_id: &str) -> Result<String, String> {
        let r = (|| {
            let record = self.vector_path(layer_id)?;
            let doc = self.vdoc()?;
            let mut entry = self.vector_entry()?;
            let (_, style) = current_rows(&entry, doc, layer_id, &record)?;
            let style = style.ok_or_else(|| format!("brush: {layer_id} has no brush"))?;
            let before = entry.records.brush_strokes.len();
            entry.records.brush_strokes.retain(|b| b.style_id != style);
            if entry.records.brush_strokes.len() == before {
                return Err(format!("brush: {layer_id} has no brush"));
            }
            self.commit_entry("remove_brush", layer_id, &entry)?;
            Ok(layer_id.to_string())
        })();
        self.report(r)
    }
}

/// Art pieces copied from vector layer `layer_id` (fresh path records in `entry`).
fn art_of(doc: &folio::StudioDocument, entry: &mut VectorEntry, layer_id: &str) -> Result<Vec<StudioBrushArt>, String> {
    let layer = doc.layers.iter().find(|l| l.layer_id == layer_id).ok_or_else(|| format!("brush: no layer {layer_id}"))?;
    let Payload::Vector { path_ids } = &layer.payload else {
        return Err(format!("brush: {layer_id} is not a vector layer"));
    };
    let first = path_ids.first().and_then(|id| entry.path(id)).cloned().ok_or("brush: art path")?;
    let (rows, _) = current_rows(entry, doc, layer_id, &first)?;
    let colour = rows
        .iter()
        .find_map(|r| match r {
            StudioAppearanceRow::Fill { paint: folio::StudioPaint::Solid { colour }, .. } => Some(colour.clone()),
            _ => None,
        })
        .unwrap_or(folio::StudioColor { r: 0.0, g: 0.0, b: 0.0, colour_profile_id: folio::ProfileBinding::Unassigned(()) });
    let mut out = Vec::new();
    for id in path_ids {
        let record = entry.path(id).cloned().ok_or_else(|| format!("brush: vector_path_unresolved {id}"))?;
        if entry.paths.len() >= super::MAX_PATHS {
            return Err("brush: vector_entry_over_budget".into());
        }
        let new_id = mint_path_id(doc, entry);
        entry.paths.push(PathRecord { path_id: new_id.clone(), path: record.path, style: record.style, network: None, width: None });
        out.push(StudioBrushArt { path_id: new_id, colour: colour.clone() });
    }
    Ok(out)
}
