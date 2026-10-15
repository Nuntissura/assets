//! Folio fill and stroke styles in the viewer (lane SMB-VECTOR-STYLES; STU-VEC-015, STU-VEC-019,
//! STU-VEC-020, STU-VEC-051, STU-VEC-134..137, STU-VEC-143..145).
//!
//! Model: a styled vector layer has a design node (`StudioNodeDesign`) whose `fill_style_id` names
//! the layer's own appearance style (`StudioPaintStyle`, kind `PAINT`, id `SSTY-<layer uuid>`):
//! fill rows (solid, linear or radial gradient) then stroke rows (paint plus the stroke panel:
//! weight, cap, join, miter limit, dash, dash offset, alignment). The records live in the vector
//! entry (`design`); an unstyled layer keeps its 8-bit [`super::VectorStyle`] and is drawn through
//! an equivalent synthesised style ([`effective_design`]). Every vector layer renders through
//! composite's Folio adapter (`prepare_vectors`), colours decoded from 8-bit sRGB (the harness
//! working space) into the linear-light plan by [`plan_colour`].
//!
//! Swatches (lane SMB-ILLUSTRATOR-TOOLS-2, [`super::swatch`]) are Folio `StudioSwatch` records
//! bound to rows by `StudioSwatchPaint`; graphic styles (STU-VEC-024) are published PAINT styles
//! (`publish_key` `graphic-style-<n>`) a layer node links to directly. Swatch and brush bindings
//! follow their rows through every restyle by row kind and per-kind ordinal.
//!
//! Each edit is ONE undo step (an entry-only history step of the session document entry).
//! Hostile input (non-finite numbers, oversize stop or dash lists, out-of-range values) is refused
//! typed before anything is built; every style is re-validated by Folio `validate_paint_style`.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::{ENTRY, MAX_STROKE_WIDTH, PathRecord, VectorEntry, entry_persistable};
use hsk_studio_folio::{
    self as folio, GeometryUnit, Payload, StudioAppearanceRow, StudioDesignRecords, StudioDocument, StudioPaint, StudioPaintStyle,
};
use serde_json::{Value, json};

/// Most gradient stops of one paint.
pub const MAX_GRADIENT_STOPS: usize = 256;
/// Most dash and gap lengths of one stroke.
pub const MAX_DASH: usize = 64;
/// Most paint styles (layer appearances and swatches) one entry holds.
pub const MAX_STYLES: usize = 4096;
/// Publish key prefix of swatch companion styles (one fill row bound to the swatch; network
/// regions name them).
pub const SWATCH_KEY: &str = "swatch-";
/// Publish key prefix of graphic styles.
pub const GRAPHIC_STYLE_KEY: &str = "graphic-style-";

/// One gradient stop: `position` and `opacity` in `0..=1`, the colour 8-bit sRGB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub position: f64,
    pub colour: [u8; 3],
    pub opacity: f64,
}

/// A fill or stroke paint (document pixels for gradient geometry).
#[derive(Clone, Debug, PartialEq)]
pub enum PaintSpec {
    Solid([u8; 3]),
    Linear { from: [f64; 2], to: [f64; 2], stops: Vec<GradientStop> },
    Radial { centre: [f64; 2], radius: f64, stops: Vec<GradientStop> },
}

/// The stroke panel (lengths in document units).
#[derive(Clone, Debug, PartialEq)]
pub struct StrokePanel {
    pub paint: PaintSpec,
    pub weight: f64,
    pub cap: folio::StrokeCap,
    pub join: folio::StrokeJoin,
    pub miter_limit: f64,
    /// Alternating dash and gap lengths (an even count; empty = solid).
    pub dash: Vec<f64>,
    pub dash_offset: f64,
    pub align: folio::StrokeAlign,
}

impl StrokePanel {
    /// A solid stroke: butt cap, miter join (limit 4), centred, no dash.
    pub fn solid(colour: [u8; 3], weight: f64) -> Self {
        Self { paint: PaintSpec::Solid(colour), weight, cap: folio::StrokeCap::Butt, join: folio::StrokeJoin::Miter, miter_limit: 4.0, dash: Vec::new(), dash_offset: 0.0, align: folio::StrokeAlign::Center }
    }
}

/// Which row of a layer's appearance a swatch is linked to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Fill,
    Stroke,
}

impl RowKind {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Stroke => "stroke",
        }
    }
    pub fn from_token(t: &str) -> Option<Self> {
        match t {
            "fill" => Some(Self::Fill),
            "stroke" => Some(Self::Stroke),
            _ => None,
        }
    }
}

/// IEC 61966-2-1 decode into linear light; an exact 8-bit value decodes like the viewer's own
/// 8-bit colours.
fn srgb_decode(v: f64) -> f32 {
    let scaled = v * 255.0;
    if scaled.fract() == 0.0 && (0.0..=255.0).contains(&scaled) {
        let c = f32::from(scaled as u8) / 255.0;
        return if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    }
    let r = if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    r as f32
}

/// Plan colour of a Folio colour for composite (`VectorAssets::plan_colour`): a working-space
/// colour (null profile) is 8-bit-grid sRGB, decoded for a linear-light plan and passed through for
/// an encoded one; an assigned profile is left to composite's own rule.
pub fn plan_colour(c: &folio::StudioColor, space: hsk_studio_composite::BlendSpace) -> Option<[f32; 3]> {
    if !matches!(c.colour_profile_id, folio::ProfileBinding::Unassigned(())) {
        return None;
    }
    Some(match space {
        hsk_studio_composite::BlendSpace::LinearLight => [srgb_decode(c.r), srgb_decode(c.g), srgb_decode(c.b)],
        _ => [c.r as f32, c.g as f32, c.b as f32],
    })
}

fn colour_json(c: [u8; 3]) -> Value {
    json!({"r": f64::from(c[0]) / 255.0, "g": f64::from(c[1]) / 255.0, "b": f64::from(c[2]) / 255.0, "colour_profile_id": null})
}

fn finite(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// The Folio paint of `spec` (hostile values refused typed before anything is built).
pub fn folio_paint(spec: &PaintSpec) -> Result<StudioPaint, String> {
    let v = match spec {
        PaintSpec::Solid(c) => json!({"paint_kind": "solid", "colour": colour_json(*c)}),
        PaintSpec::Linear { from, to, stops } => {
            if !finite(&[from[0], from[1], to[0], to[1]]) || from == to {
                return Err("paint: linear gradient endpoints".into());
            }
            gradient_json(json!({"gradient_kind": "linear", "x1": from[0], "y1": from[1], "x2": to[0], "y2": to[1]}), stops)?
        }
        PaintSpec::Radial { centre, radius, stops } => {
            if !finite(&[centre[0], centre[1], *radius]) || *radius <= 0.0 {
                return Err("paint: radial gradient circle".into());
            }
            gradient_json(json!({"gradient_kind": "radial", "cx": centre[0], "cy": centre[1], "r": radius, "fx": centre[0], "fy": centre[1], "fr": 0.0}), stops)?
        }
    };
    serde_json::from_value(v).map_err(|e| format!("paint: {e}"))
}

fn gradient_json(geometry: Value, stops: &[GradientStop]) -> Result<Value, String> {
    if stops.len() < 2 || stops.len() > MAX_GRADIENT_STOPS {
        return Err("paint: gradient stop count".into());
    }
    let mut last = 0.0;
    let mut out = Vec::with_capacity(stops.len());
    for s in stops {
        if !finite(&[s.position, s.opacity]) || !(0.0..=1.0).contains(&s.position) || !(0.0..=1.0).contains(&s.opacity) || s.position < last {
            return Err("paint: gradient stop".into());
        }
        last = s.position;
        out.push(json!({"position": s.position * 100.0, "midpoint": null, "opacity": s.opacity * 100.0, "colour": colour_json(s.colour)}));
    }
    Ok(json!({"paint_kind": "gradient", "gradient": {"geometry": geometry, "transform": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], "spread": "pad", "interpolation": "classic", "dither": false, "stops": out}}))
}

fn length(unit: GeometryUnit, value: f64) -> Value {
    json!({"value": value, "unit": unit})
}

/// The Folio stroke row of `panel`.
pub fn stroke_row(panel: &StrokePanel, unit: GeometryUnit) -> Result<StudioAppearanceRow, String> {
    let p = panel;
    if !finite(&[p.weight, p.miter_limit, p.dash_offset]) || !(0.0..=MAX_STROKE_WIDTH).contains(&p.weight) || !(1.0..=500.0).contains(&p.miter_limit) {
        return Err("stroke: weight, miter limit or dash offset".into());
    }
    if p.dash.len() > MAX_DASH || !p.dash.len().is_multiple_of(2) || !finite(&p.dash) || p.dash.iter().any(|d| !(0.0..=MAX_STROKE_WIDTH).contains(d)) || (!p.dash.is_empty() && p.dash.iter().all(|d| *d == 0.0)) {
        return Err("stroke: dash".into());
    }
    let stroke = json!({
        "weight": length(unit, p.weight), "cap": p.cap, "join": p.join, "align": p.align, "miter_limit": p.miter_limit,
        "dash": p.dash.iter().map(|d| length(unit, *d)).collect::<Vec<_>>(), "dash_offset": length(unit, p.dash_offset),
    });
    let paint = serde_json::to_value(folio_paint(&p.paint)?).map_err(|e| format!("stroke: {e}"))?;
    serde_json::from_value(json!({"row_kind": "stroke", "paint": paint, "stroke": stroke, "opacity": 100.0, "blend_mode": 2, "visible": true})).map_err(|e| format!("stroke: {e}"))
}

/// A visible Normal fill row at full opacity.
pub fn fill_row(paint: StudioPaint) -> StudioAppearanceRow {
    StudioAppearanceRow::Fill { paint, opacity: 100.0, blend_mode: 2, visible: true }
}

/// The appearance style id of `layer_id` (`SSTY-<layer uuid>`).
pub fn layer_style_id(layer_id: &str) -> Result<String, String> {
    let uuid = layer_id.strip_prefix("SLYR-").ok_or_else(|| format!("style: layer id {layer_id}"))?;
    Ok(format!("SSTY-{uuid}"))
}

pub(super) fn paint_style(id: &str, name: &str, key: &str, rows: &[StudioAppearanceRow]) -> Result<StudioPaintStyle, String> {
    let style: StudioPaintStyle = serde_json::from_value(json!({
        "schema_id": "hsk.studio.style_registry@1", "style_id": id, "kind": "PAINT", "name": name,
        "publishing": {"description": "", "description_markdown": "", "documentation_links": [], "publish_key": key, "is_remote": false},
        "rows": rows,
    }))
    .map_err(|e| format!("style: {e}"))?;
    validate_style(&style)?;
    Ok(style)
}

/// Folio validation of one paint style.
pub fn validate_style(style: &StudioPaintStyle) -> Result<(), String> {
    folio::validate_paint_style(style, folio::Budget::default(), &hsk_studio_accord::CancellationToken::default(), &folio::Unresolved)
        .map(|_| ())
        .map_err(|d| format!("style: {:?} {}", d.code, d.target))
}

pub(super) fn node(layer_id: &str, style_id: &str) -> Result<folio::StudioNodeDesign, String> {
    serde_json::from_value(json!({"node": {"node_kind": "layer", "node_id": layer_id}, "style_refs": {"fill_style_id": style_id}})).map_err(|e| format!("style: {e}"))
}

/// The rows an unstyled layer draws with: its 8-bit fill, and its stroke when it has a width (or a
/// width profile): butt cap, miter join (limit 4), centred, like nib's default stroke.
fn legacy_rows(record: &PathRecord, unit: GeometryUnit) -> Result<Vec<StudioAppearanceRow>, String> {
    let s = record.style;
    let mut rows = Vec::new();
    if let Some(c) = s.fill {
        rows.push(fill_row(folio_paint(&PaintSpec::Solid(c))?));
    }
    if let Some(c) = s.stroke
        && (s.stroke_width > 0.0 || record.width.is_some())
    {
        rows.push(stroke_row(&StrokePanel::solid(c, s.stroke_width), unit)?);
    }
    Ok(rows)
}

/// The paint style a layer's fill style reference names in `design`, if any.
pub(super) fn node_style<'a>(design: &'a StudioDesignRecords, layer_id: &str) -> Option<&'a StudioPaintStyle> {
    let id = design.node_design(layer_id)?.style_refs.get("fill_style_id")?;
    design.paint_styles.iter().find(|s| &s.style_id == id)
}

/// Design records composite draws the document's vector layers with: the entry's records plus a
/// synthesised appearance style for every unstyled vector layer (its 8-bit style).
pub fn effective_design(doc: &StudioDocument, entry: &VectorEntry) -> Result<StudioDesignRecords, String> {
    let mut d = entry.design.clone().unwrap_or_else(|| StudioDesignRecords::new(doc.document_id.clone()));
    d.document_id.clone_from(&doc.document_id);
    for layer in &doc.layers {
        let Payload::Vector { path_ids } = &layer.payload else { continue };
        if d.node_design(&layer.layer_id).is_some() {
            continue;
        }
        let Some(record) = path_ids.first().and_then(|id| entry.path(id)) else { continue };
        let id = layer_style_id(&layer.layer_id)?;
        let rows = legacy_rows(record, doc.geometry_unit)?;
        d.paint_styles.retain(|s| s.style_id != id);
        d.paint_styles.push(paint_style(&id, "Layers/appearance", "appearance", &rows)?);
        d.nodes.push(node(&layer.layer_id, &id)?);
    }
    Ok(d)
}

/// True when the layer's appearance has a visible fill row (styled) or an 8-bit fill (unstyled).
pub(super) fn has_fill(entry: &VectorEntry, layer_id: &str, record: &PathRecord) -> bool {
    match entry.design.as_ref().and_then(|d| node_style(d, layer_id)) {
        Some(style) => style.rows.iter().any(|r| matches!(r, StudioAppearanceRow::Fill { visible: true, .. })),
        None => record.style.fill.is_some(),
    }
}

/// Checks of the entry's `design` member (decode): only layer nodes with a `fill_style_id` naming a
/// style of the set, and Folio-valid paint styles.
pub(super) fn validate_entry_styles(design: Option<&StudioDesignRecords>, document_id: &str) -> Result<(), String> {
    if let Some(d) = design {
        if d.document_id != document_id {
            return Err("vector_entry_design: document".into());
        }
        let others = !d.components.is_empty() || !d.component_sets.is_empty() || !d.component_instances.is_empty() || !d.variable_collections.is_empty() || !d.variables.is_empty() || !d.prototype_pages.is_empty() || !d.frame_overflow.is_empty();
        if others || d.paint_styles.len() > MAX_STYLES || d.nodes.len() > MAX_STYLES {
            return Err("vector_entry_design: members".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for s in &d.paint_styles {
            validate_style(s).map_err(|e| format!("vector_entry_design: {e}"))?;
            if !ids.insert(s.style_id.as_str()) {
                return Err(format!("vector_entry_design: duplicate {}", s.style_id));
            }
        }
        for n in &d.nodes {
            let refs_ok = n.style_refs.len() == 1 && n.style_refs.get("fill_style_id").is_some_and(|s| ids.contains(s.as_str()));
            if serde_json::to_value(&n.node).ok().and_then(|v| v.get("node_kind").cloned()) != Some(json!("layer")) || !refs_ok {
                return Err(format!("vector_entry_design: node {}", n.node.node_id));
            }
        }
    }
    Ok(())
}

/// True for a graphic style (lane SMB-ILLUSTRATOR-TOOLS-2: a published PAINT style under a
/// `graphic-style-` key).
pub fn is_graphic_style(s: &StudioPaintStyle) -> bool {
    s.publishing.publish_key.starts_with(GRAPHIC_STYLE_KEY)
}

/// Kind and per-kind ordinal of every row.
fn ordinals(rows: &[StudioAppearanceRow]) -> Vec<(RowKind, usize)> {
    let (mut f, mut s) = (0_usize, 0_usize);
    rows.iter()
        .map(|r| match r {
            StudioAppearanceRow::Fill { .. } => {
                f = f.saturating_add(1);
                (RowKind::Fill, f)
            }
            StudioAppearanceRow::Stroke { .. } => {
                s = s.saturating_add(1);
                (RowKind::Stroke, s)
            }
        })
        .collect()
}

/// Moves (or, between two styles, copies) the swatch and brush bindings of `from`'s rows `before`
/// onto `to`'s rows `after` by kind and per-kind ordinal; bindings of kind `broken` are dropped.
/// `to`'s own earlier bindings are replaced.
pub(super) fn rebind(records: &mut folio::StudioVectorRecords, from: &str, before: &[StudioAppearanceRow], to: &str, after: &[StudioAppearanceRow], broken: Option<RowKind>) {
    let (old, new) = (ordinals(before), ordinals(after));
    let map = |row: u32| -> Option<u32> {
        let k = old.get(usize::try_from(row).ok()?)?;
        if Some(k.0) == broken {
            return None;
        }
        new.iter().position(|x| x == k).and_then(|i| u32::try_from(i).ok())
    };
    let swatches: Vec<folio::StudioSwatchPaint> = records.swatch_paints.iter().filter(|p| p.style_id == from).filter_map(|p| Some(folio::StudioSwatchPaint { style_id: to.to_string(), row: map(p.row)?, ..p.clone() })).collect();
    let brushes: Vec<folio::StudioBrushStroke> = records.brush_strokes.iter().filter(|p| p.style_id == from).filter_map(|p| Some(folio::StudioBrushStroke { style_id: to.to_string(), row: map(p.row)?, ..p.clone() })).collect();
    records.swatch_paints.retain(|p| p.style_id != to);
    records.brush_strokes.retain(|p| p.style_id != to);
    records.swatch_paints.extend(swatches);
    records.brush_strokes.extend(brushes);
}

/// A binding made together with a restyle: the first row of its kind takes it.
pub(super) enum Bind {
    Swatch { kind: RowKind, swatch_id: String, tint: Option<f64> },
    Brush { brush_id: String, scale_override: Option<f64> },
}

/// The current rows of a layer: its node's style, else the synthesised 8-bit rows; and the style id
/// they come from (`None` for synthesised rows).
pub(super) fn current_rows(entry: &VectorEntry, doc: &StudioDocument, layer_id: &str, record: &PathRecord) -> Result<(Vec<StudioAppearanceRow>, Option<String>), String> {
    match entry.design.as_ref().and_then(|d| node_style(d, layer_id)) {
        Some(style) => Ok((style.rows.clone(), Some(style.style_id.clone()))),
        None => Ok((legacy_rows(record, doc.geometry_unit)?, None)),
    }
}

/// Mints a fresh `SSTY` id with the variant `tag` (`b` swatch companions use the swatch uuid).
pub(super) fn mint_style_id(doc: &StudioDocument, design: &StudioDesignRecords, tag: &str) -> Result<(String, u64), String> {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
    let mut n = 0_u64;
    loop {
        let id = format!("SSTY-{high}-{low}-7ede-{tag}000-{n:012x}");
        if !design.paint_styles.iter().any(|s| s.style_id == id) {
            return Ok((id, n));
        }
        n = n.checked_add(1).ok_or("style: id space")?;
    }
}

impl Viewer {
    /// The appearance style of vector layer `layer_id` as composite draws it (its own style, a
    /// linked graphic style, or the one synthesised from its 8-bit style).
    pub fn layer_paint_style(&self, layer_id: &str) -> Result<StudioPaintStyle, String> {
        let doc = self.vdoc()?;
        let design = effective_design(doc, &self.vector_entry()?)?;
        node_style(&design, layer_id).cloned().ok_or_else(|| format!("style: {layer_id} is not a vector layer"))
    }

    /// Sets the fill of vector layer `layer_id` (`None` removes it): ONE undo step. A swatch link
    /// of the fill is dropped; a linked graphic style is broken into a local copy.
    pub fn set_fill(&mut self, layer_id: &str, paint: Option<&PaintSpec>) -> Result<String, String> {
        let r = (|| {
            let paint = paint.map(folio_paint).transpose()?;
            self.restyle("set_fill", layer_id, None, |rows, _| {
                rows.retain(|r| !matches!(r, StudioAppearanceRow::Fill { .. }));
                if let Some(p) = paint {
                    rows.insert(0, fill_row(p));
                }
                Ok(Some(RowKind::Fill))
            })
        })();
        self.report(r)
    }

    /// Sets the stroke of vector layer `layer_id` from the stroke panel (`None` removes it): ONE
    /// undo step. A swatch or brush binding of the stroke is dropped.
    pub fn set_stroke(&mut self, layer_id: &str, panel: Option<&StrokePanel>) -> Result<String, String> {
        let r = (|| {
            let unit = self.vdoc()?.geometry_unit;
            let row = panel.map(|p| stroke_row(p, unit)).transpose()?;
            self.restyle("set_stroke", layer_id, None, |rows, _| {
                rows.retain(|r| !matches!(r, StudioAppearanceRow::Stroke { .. }));
                rows.extend(row);
                Ok(Some(RowKind::Stroke))
            })
        })();
        self.report(r)
    }

    // ----- graphic styles (STU-VEC-024) ------------------------------------------------------------

    /// Graphic styles of the document: `(style id, name)`.
    pub fn graphic_styles(&self) -> Result<Vec<(String, String)>, String> {
        let entry = self.vector_entry()?;
        Ok(entry.design.iter().flat_map(|d| d.paint_styles.iter()).filter(|s| is_graphic_style(s)).map(|s| (s.style_id.clone(), s.name.clone())).collect())
    }

    /// Creates a graphic style named `name` from `layer_id`'s appearance (its swatch and brush
    /// bindings come along): ONE undo step. Returns the style id.
    pub fn add_graphic_style(&mut self, name: &str, layer_id: &str) -> Result<String, String> {
        let r = (|| {
            let record = self.vector_path(layer_id)?;
            let doc = self.vdoc()?;
            let mut entry = self.vector_entry()?;
            let (rows, from) = current_rows(&entry, doc, layer_id, &record)?;
            let design = entry.design.get_or_insert_with(|| StudioDesignRecords::new(doc.document_id.clone()));
            if design.paint_styles.len() >= MAX_STYLES {
                return Err("graphic_style: style budget".into());
            }
            let (id, n) = mint_style_id(doc, design, "b")?;
            design.paint_styles.push(paint_style(&id, name, &format!("{GRAPHIC_STYLE_KEY}{n}"), &rows)?);
            if let Some(from) = from {
                rebind(&mut entry.records, &from, &rows, &id, &rows, None);
            }
            self.commit_entry("add_graphic_style", &id, &entry)?;
            Ok(id)
        })();
        self.report(r)
    }

    /// Links `layer_id` to graphic style `style_id` (its node names the style; a later redefine
    /// updates the layer): ONE undo step.
    pub fn apply_graphic_style(&mut self, layer_id: &str, style_id: &str) -> Result<String, String> {
        let r = (|| {
            self.vector_path(layer_id)?;
            let mut entry = self.vector_entry()?;
            let design = entry.design.as_mut().ok_or_else(|| format!("graphic_style: unknown {style_id}"))?;
            if !design.paint_styles.iter().any(|s| s.style_id == style_id && is_graphic_style(s)) {
                return Err(format!("graphic_style: unknown {style_id}"));
            }
            design.nodes.retain(|n| n.node.node_id != layer_id);
            design.nodes.push(node(layer_id, style_id)?);
            self.commit_entry("apply_graphic_style", layer_id, &entry)?;
            Ok(layer_id.to_string())
        })();
        self.report(r)
    }

    /// Adds graphic style `style_id`'s rows to `layer_id`'s own appearance (fills after its fills,
    /// strokes after its strokes; additive merge): ONE undo step.
    pub fn merge_graphic_style(&mut self, layer_id: &str, style_id: &str) -> Result<String, String> {
        let r = (|| {
            let entry = self.vector_entry()?;
            let style = entry.design.as_ref().and_then(|d| d.paint_styles.iter().find(|s| s.style_id == style_id && is_graphic_style(s))).cloned().ok_or_else(|| format!("graphic_style: unknown {style_id}"))?;
            self.restyle("merge_graphic_style", layer_id, None, |rows, _| {
                let fills = rows.iter().take_while(|r| matches!(r, StudioAppearanceRow::Fill { .. })).count();
                let (f, s): (Vec<_>, Vec<_>) = style.rows.iter().cloned().partition(|r| matches!(r, StudioAppearanceRow::Fill { .. }));
                let tail = rows.split_off(fills);
                rows.extend(f);
                rows.extend(tail);
                rows.extend(s);
                Ok(None)
            })
        })();
        self.report(r)
    }

    /// Breaks `layer_id`'s link to its graphic style into a local copy: ONE undo step.
    pub fn break_graphic_style(&mut self, layer_id: &str) -> Result<String, String> {
        let r = self.restyle("break_graphic_style", layer_id, None, |_, _| Ok(None));
        self.report(r)
    }

    /// Redefines graphic style `style_id` from `layer_id`'s appearance; every linked layer follows:
    /// ONE undo step.
    pub fn redefine_graphic_style(&mut self, style_id: &str, layer_id: &str) -> Result<String, String> {
        let r = (|| {
            let record = self.vector_path(layer_id)?;
            let doc = self.vdoc()?;
            let mut entry = self.vector_entry()?;
            let (rows, from) = current_rows(&entry, doc, layer_id, &record)?;
            let design = entry.design.as_mut().ok_or_else(|| format!("graphic_style: unknown {style_id}"))?;
            let style = design.paint_styles.iter_mut().find(|s| s.style_id == style_id && is_graphic_style(s)).ok_or_else(|| format!("graphic_style: unknown {style_id}"))?;
            style.rows.clone_from(&rows);
            validate_style(style)?;
            match from {
                Some(from) if from != style_id => rebind(&mut entry.records, &from, &rows, style_id, &rows, None),
                Some(_) => {}
                None => rebind(&mut entry.records, style_id, &[], style_id, &rows, None),
            }
            self.commit_entry("redefine_graphic_style", style_id, &entry)?;
            Ok(style_id.to_string())
        })();
        self.report(r)
    }

    /// Clears `layer_id`'s appearance (no fill, no stroke): ONE undo step.
    pub fn clear_appearance(&mut self, layer_id: &str) -> Result<String, String> {
        let r = self.restyle("clear_appearance", layer_id, None, |rows, _| {
            rows.clear();
            Ok(None)
        });
        self.report(r)
    }

    /// Reduces `layer_id`'s appearance to basic: its first fill and first stroke row: ONE undo step.
    pub fn reduce_to_basic_appearance(&mut self, layer_id: &str) -> Result<String, String> {
        let r = self.restyle("reduce_to_basic_appearance", layer_id, None, |rows, _| {
            let fill = rows.iter().find(|r| matches!(r, StudioAppearanceRow::Fill { .. })).cloned();
            let stroke = rows.iter().find(|r| matches!(r, StudioAppearanceRow::Stroke { .. })).cloned();
            rows.clear();
            rows.extend(fill);
            rows.extend(stroke);
            Ok(None)
        });
        self.report(r)
    }

    /// The layer's current rows, `edit` applied, stored as its own style (a linked graphic style
    /// is broken into the local copy); bindings follow their rows by kind and ordinal, the kind
    /// `edit` returns loses its bindings, then `bind` binds the first row of its kind.
    pub(super) fn restyle(&mut self, op: &str, layer_id: &str, bind: Option<Bind>, edit: impl FnOnce(&mut Vec<StudioAppearanceRow>, GeometryUnit) -> Result<Option<RowKind>, String>) -> Result<String, String> {
        let record = self.vector_path(layer_id)?;
        let doc = self.vdoc()?;
        let mut entry = self.vector_entry()?;
        let id = layer_style_id(layer_id)?;
        let (before, from) = current_rows(&entry, doc, layer_id, &record)?;
        let mut rows = before.clone();
        let broken = edit(&mut rows, doc.geometry_unit)?;
        let style = paint_style(&id, "Layers/appearance", "appearance", &rows)?;
        let design = entry.design.get_or_insert_with(|| StudioDesignRecords::new(doc.document_id.clone()));
        if design.paint_styles.len() >= MAX_STYLES {
            return Err(format!("{op}: style budget"));
        }
        design.paint_styles.retain(|s| s.style_id != id);
        design.paint_styles.push(style);
        design.nodes.retain(|n| n.node.node_id != layer_id);
        design.nodes.push(node(layer_id, &id)?);
        rebind(&mut entry.records, from.as_deref().unwrap_or(&id), if from.is_some() { &before } else { &[] }, &id, &rows, broken);
        if let Some(b) = bind {
            let kind = match &b {
                Bind::Swatch { kind, .. } => *kind,
                Bind::Brush { .. } => RowKind::Stroke,
            };
            let row = ordinals(&rows).iter().position(|o| *o == (kind, 1)).and_then(|i| u32::try_from(i).ok()).ok_or_else(|| format!("{op}: no {} row", kind.token()))?;
            match b {
                Bind::Swatch { swatch_id, tint, .. } => {
                    entry.records.swatch_paints.retain(|p| !(p.style_id == id && p.row == row));
                    entry.records.swatch_paints.push(folio::StudioSwatchPaint { style_id: id.clone(), row, swatch_id, tint });
                }
                Bind::Brush { brush_id, scale_override } => {
                    entry.records.brush_strokes.retain(|p| !(p.style_id == id && p.row == row));
                    entry.records.brush_strokes.push(folio::StudioBrushStroke { style_id: id.clone(), row, brush_id, scale_override });
                }
            }
        }
        self.commit_entry(op, layer_id, &entry)?;
        Ok(layer_id.to_string())
    }

    /// Stores `entry` as ONE entry-only undo step (no chronicle write).
    pub(super) fn commit_entry(&mut self, op: &str, target: &str, entry: &VectorEntry) -> Result<(), String> {
        if !entry_persistable() {
            return Err(format!("{op}: vector_entry_not_persistable"));
        }
        let bytes = entry.to_bytes(&self.vdoc()?.document_id)?;
        let ok = self.run.set_tiles_and_entry(op, target, None, ENTRY, bytes);
        self.after_edit(ok, format!("{op} {target}")).map(|_| ())
    }
}
