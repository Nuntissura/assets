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
//! Swatches: a swatch is a published `PAINT` style with one fill row (`publish_key` `swatch-<n>`).
//! Applying a swatch copies its paint into the layer's fill or stroke row and records a link
//! (`swatch_links` in the entry); editing the swatch updates the swatch and every linked row in one
//! step (edit-updates-all-uses); detaching drops the link and keeps the local copy (STU-VEC-051). A
//! network region painted with a swatch names the swatch style itself (`fill_style_id`), which
//! composite paints. Folio rows carry no swatch reference: the row link is viewer state (gap).
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
/// Most swatch links one entry holds.
pub const MAX_LINKS: usize = 4096;
/// Publish key prefix of swatches.
pub const SWATCH_KEY: &str = "swatch-";

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

/// A swatch link: the `kind` row of layer `layer_id` holds a copy of swatch `swatch_id`'s paint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwatchLink {
    pub layer_id: String,
    pub kind: RowKind,
    pub swatch_id: String,
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

fn paint_style(id: &str, name: &str, key: &str, rows: &[StudioAppearanceRow]) -> Result<StudioPaintStyle, String> {
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

fn node(layer_id: &str, style_id: &str) -> Result<folio::StudioNodeDesign, String> {
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
fn node_style<'a>(design: &'a StudioDesignRecords, layer_id: &str) -> Option<&'a StudioPaintStyle> {
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

/// Checks of the entry's `design` and `swatch_links` members (decode): only layer nodes with a
/// `fill_style_id` naming a style of the set, Folio-valid paint styles, links to swatches.
pub(super) fn validate_entry_styles(design: Option<&StudioDesignRecords>, links: &[SwatchLink], document_id: &str) -> Result<(), String> {
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
    if links.len() > MAX_LINKS {
        return Err("vector_entry_swatch_links: count".into());
    }
    for l in links {
        let swatch = design.and_then(|d| d.paint_styles.iter().find(|s| s.style_id == l.swatch_id));
        if !swatch.is_some_and(is_swatch) {
            return Err(format!("vector_entry_swatch_links: {}", l.swatch_id));
        }
    }
    Ok(())
}

/// True for a swatch style (published under a `swatch-` key, one fill row).
pub fn is_swatch(s: &StudioPaintStyle) -> bool {
    s.publishing.publish_key.starts_with(SWATCH_KEY) && matches!(s.rows.as_slice(), [StudioAppearanceRow::Fill { .. }])
}

fn swatch_paint(s: &StudioPaintStyle) -> Result<StudioPaint, String> {
    match s.rows.as_slice() {
        [StudioAppearanceRow::Fill { paint, .. }] if is_swatch(s) => Ok(paint.clone()),
        _ => Err(format!("swatch: {} is not a swatch", s.style_id)),
    }
}

/// Replaces the paint of every `kind` row of `rows` (a stroke row keeps its geometry).
fn repaint(rows: &mut [StudioAppearanceRow], kind: RowKind, with: &StudioPaint) {
    for r in rows {
        match (r, kind) {
            (StudioAppearanceRow::Fill { paint, .. }, RowKind::Fill) | (StudioAppearanceRow::Stroke { paint, .. }, RowKind::Stroke) => paint.clone_from(with),
            _ => {}
        }
    }
}

impl Viewer {
    /// The appearance style of vector layer `layer_id` as composite draws it (its own style, or
    /// the one synthesised from its 8-bit style).
    pub fn layer_paint_style(&self, layer_id: &str) -> Result<StudioPaintStyle, String> {
        let doc = self.vdoc()?;
        let design = effective_design(doc, &self.vector_entry()?)?;
        node_style(&design, layer_id).cloned().ok_or_else(|| format!("style: {layer_id} is not a vector layer"))
    }

    /// Swatches of the document: `(style id, name)`, in creation order.
    pub fn swatches(&self) -> Result<Vec<(String, String)>, String> {
        let entry = self.vector_entry()?;
        Ok(entry.design.iter().flat_map(|d| d.paint_styles.iter()).filter(|s| is_swatch(s)).map(|s| (s.style_id.clone(), s.name.clone())).collect())
    }

    /// Sets the fill of vector layer `layer_id` (`None` removes it): ONE undo step. A swatch link
    /// of the fill is dropped.
    pub fn set_fill(&mut self, layer_id: &str, paint: Option<&PaintSpec>) -> Result<String, String> {
        let r = (|| {
            let paint = paint.map(folio_paint).transpose()?;
            self.restyle("set_fill", layer_id, |rows, _| {
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
    /// undo step. A swatch link of the stroke is dropped.
    pub fn set_stroke(&mut self, layer_id: &str, panel: Option<&StrokePanel>) -> Result<String, String> {
        let r = (|| {
            let unit = self.vdoc()?.geometry_unit;
            let row = panel.map(|p| stroke_row(p, unit)).transpose()?;
            self.restyle("set_stroke", layer_id, |rows, _| {
                rows.retain(|r| !matches!(r, StudioAppearanceRow::Stroke { .. }));
                rows.extend(row);
                Ok(Some(RowKind::Stroke))
            })
        })();
        self.report(r)
    }

    /// Creates a swatch named `name` (a slash path) holding `paint`: ONE undo step. Returns its
    /// style id.
    pub fn add_swatch(&mut self, name: &str, paint: &PaintSpec) -> Result<String, String> {
        let r = (|| {
            let paint = folio_paint(paint)?;
            let doc = self.vdoc()?;
            let mut entry = self.vector_entry()?;
            let design = entry.design.get_or_insert_with(|| StudioDesignRecords::new(doc.document_id.clone()));
            if design.paint_styles.len() >= MAX_STYLES {
                return Err("swatch: style budget".into());
            }
            let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
            let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
            let mut n = 0_u64;
            let id = loop {
                let id = format!("SSTY-{high}-{low}-7ede-b000-{n:012x}");
                if !design.paint_styles.iter().any(|s| s.style_id == id) {
                    break id;
                }
                n = n.checked_add(1).ok_or("swatch: id space")?;
            };
            let style = paint_style(&id, name, &format!("{SWATCH_KEY}{n}"), &[fill_row(paint)])?;
            design.paint_styles.push(style);
            self.commit_entry("add_swatch", &id, &entry)?;
            Ok(id)
        })();
        self.report(r)
    }

    /// Applies swatch `swatch_id` to the `kind` row of layer `layer_id` (a layer without a stroke
    /// gets a stroke of its 8-bit width, else 1 px) and links them: ONE undo step.
    pub fn apply_swatch(&mut self, layer_id: &str, swatch_id: &str, kind: RowKind) -> Result<String, String> {
        let r = (|| {
            let entry = self.vector_entry()?;
            let swatch = entry.design.as_ref().and_then(|d| d.paint_styles.iter().find(|s| s.style_id == swatch_id)).ok_or_else(|| format!("swatch: unknown {swatch_id}"))?;
            let paint = swatch_paint(swatch)?;
            let width = self.vector_path(layer_id)?.style.stroke_width;
            let id = swatch_id.to_string();
            self.restyle_inner("apply_swatch", layer_id, Some(SwatchLink { layer_id: layer_id.to_string(), kind, swatch_id: id }), |rows, unit| {
                match kind {
                    RowKind::Fill => {
                        rows.retain(|r| !matches!(r, StudioAppearanceRow::Fill { .. }));
                        rows.insert(0, fill_row(paint.clone()));
                    }
                    RowKind::Stroke => {
                        if !rows.iter().any(|r| matches!(r, StudioAppearanceRow::Stroke { .. })) {
                            let weight = if width > 0.0 { width } else { 1.0 };
                            rows.push(stroke_row(&StrokePanel::solid([0, 0, 0], weight), unit)?);
                        }
                        repaint(rows, RowKind::Stroke, &paint);
                    }
                }
                Ok(Some(kind))
            })
        })();
        self.report(r)
    }

    /// Drops the swatch link of the `kind` row of `layer_id` (its paint stays as a local copy):
    /// ONE undo step.
    pub fn detach_swatch(&mut self, layer_id: &str, kind: RowKind) -> Result<String, String> {
        let r = (|| {
            let mut entry = self.vector_entry()?;
            let before = entry.links.len();
            entry.links.retain(|l| !(l.layer_id == layer_id && l.kind == kind));
            if entry.links.len() == before {
                return Err(format!("swatch: {layer_id} {} is not linked", kind.token()));
            }
            self.commit_entry("detach_swatch", layer_id, &entry)?;
            Ok(layer_id.to_string())
        })();
        self.report(r)
    }

    /// Edits swatch `swatch_id`: the swatch and every linked row take `paint` (network regions
    /// that name the swatch follow by reference): ONE undo step.
    pub fn edit_swatch(&mut self, swatch_id: &str, paint: &PaintSpec) -> Result<String, String> {
        let r = (|| {
            let paint = folio_paint(paint)?;
            let mut entry = self.vector_entry()?;
            let links = entry.links.clone();
            let design = entry.design.as_mut().ok_or_else(|| format!("swatch: unknown {swatch_id}"))?;
            let swatch = design.paint_styles.iter_mut().find(|s| s.style_id == swatch_id).ok_or_else(|| format!("swatch: unknown {swatch_id}"))?;
            swatch_paint(swatch)?;
            swatch.rows = vec![fill_row(paint.clone())];
            for l in links.iter().filter(|l| l.swatch_id == swatch_id) {
                let Ok(id) = layer_style_id(&l.layer_id) else { continue };
                if let Some(style) = design.paint_styles.iter_mut().find(|s| s.style_id == id) {
                    repaint(&mut style.rows, l.kind, &paint);
                    validate_style(style)?;
                }
            }
            self.commit_entry("edit_swatch", swatch_id, &entry)?;
            Ok(swatch_id.to_string())
        })();
        self.report(r)
    }

    /// The layer's current rows, `edit` applied, stored as its own style; `edit` returns the row
    /// kind whose swatch link it breaks.
    fn restyle(&mut self, op: &str, layer_id: &str, edit: impl FnOnce(&mut Vec<StudioAppearanceRow>, GeometryUnit) -> Result<Option<RowKind>, String>) -> Result<String, String> {
        self.restyle_inner(op, layer_id, None, edit)
    }

    /// [`Viewer::restyle`] that also records `link` (after breaking the old link of its kind).
    fn restyle_inner(&mut self, op: &str, layer_id: &str, link: Option<SwatchLink>, edit: impl FnOnce(&mut Vec<StudioAppearanceRow>, GeometryUnit) -> Result<Option<RowKind>, String>) -> Result<String, String> {
        let record = self.vector_path(layer_id)?;
        let doc = self.vdoc()?;
        let mut entry = self.vector_entry()?;
        let id = layer_style_id(layer_id)?;
        let mut rows = match entry.design.as_ref().and_then(|d| node_style(d, layer_id)) {
            Some(style) => style.rows.clone(),
            None => legacy_rows(&record, doc.geometry_unit)?,
        };
        let broken = edit(&mut rows, doc.geometry_unit)?;
        let style = paint_style(&id, "Layers/appearance", "appearance", &rows)?;
        let design = entry.design.get_or_insert_with(|| StudioDesignRecords::new(doc.document_id.clone()));
        if design.paint_styles.len() >= MAX_STYLES {
            return Err(format!("{op}: style budget"));
        }
        design.paint_styles.retain(|s| s.style_id != id);
        design.paint_styles.push(style);
        if design.node_design(layer_id).is_none() {
            design.nodes.push(node(layer_id, &id)?);
        }
        if let Some(kind) = broken {
            entry.links.retain(|l| !(l.layer_id == layer_id && l.kind == kind));
        }
        if let Some(l) = link {
            if entry.links.len() >= MAX_LINKS {
                return Err(format!("{op}: link budget"));
            }
            entry.links.push(l);
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
