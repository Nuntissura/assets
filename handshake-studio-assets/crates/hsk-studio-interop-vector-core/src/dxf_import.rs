//! DXF import (lane SMB-DXF-EPS-IMPORT, feature `dxf`; parity row AF-06). ASCII and binary DXF are
//! read by the `dxf` crate (ixmilia/dxf-rs, MIT) and turned into the drawing model the `.ai`
//! import already builds Folio proposals from, so DXF reuses that builder (layers, paint styles,
//! validation):
//!
//! - model space only; one artboard spanning the drawn geometry; DXF y-up user space becomes
//!   document points with `$INSUNITS` (unitless: millimetres for a metric `$MEASUREMENT`, inches
//!   for an English one, reported `dxf_units_unitless_assumed`);
//! - every DXF layer that draws something is a named group layer (layer table order; off layers
//!   hidden); entities keep their order inside their layer (`dxf_draw_order_grouped_by_layer` when
//!   that regroups interleaved layers);
//! - LINE, ARC, CIRCLE, ELLIPSE, LWPOLYLINE and 2D/3D POLYLINE (bulges as arcs) are stroked
//!   paths (arcs as cubic Béziers, `dxf_arc_to_cubic`); SOLID and TRACE are filled quads; SPLINE:
//!   clamped non-rational degree 1..=3 is exact Bézier, anything else sampled
//!   (`dxf_spline_sampled`), fit points only as a polyline (`dxf_spline_fit_points_as_polyline`);
//! - INSERT expands its BLOCK (base point, scale, rotation, column/row arrays) up to the policy
//!   depth, with a cycle check; BYBLOCK colour and weight come from the insert;
//! - colour: true colour, else the ACI palette (`dxf_aci`; ACI 7 is drawn black on the white
//!   artboard, `dxf_aci_7_drawn_black`), BYLAYER from the layer table; weight: lineweight in
//!   hundredths of a millimetre (default 0.25 mm); linetypes other than CONTINUOUS are drawn solid
//!   (`dxf_linetype_drawn_solid`);
//! - typed losses for what is not drawn: text (`dxf_text_not_imported`), dimensions, leaders,
//!   hatches, images, 3D solids, regions, proxies and other entity types
//!   (`dxf_entity_not_imported`), points, paper space, polyline widths, non-planar extrusions.
//!
//! Hostile input: the input size is capped by `max_bytes`; the code-0 records are counted in a
//! pre-scan before the parser allocates entities (`BudgetExceeded("dxf_records")`); a parser
//! panic is caught and reported as `MalformedDxf("parser_panicked")`; insert expansion is bounded by
//! depth, a cycle check and the segment and item budgets (proof: `tests/dxf_import.rs`
//! `hostile_dxf_is_bounded_and_typed`).
#![deny(clippy::arithmetic_side_effects)]
use crate::ai_import::{DrawingSource, PdfDrawItem, PdfDrawing, PdfDrawingPage, PdfDrawingProvider, PdfEl, PdfLayer, PdfPaint, PdfStroke, document_from_drawing};
use crate::cad::{BINARY_DXF_SENTINEL, CadKind, identify_cad};
use crate::dxf_aci::ACI_RGB;
use crate::error::{Result, VectorError};
use crate::ids::IdMint;
use crate::model::{COORDINATE_LIMIT, ImportPolicy, LossKind, VectorProposal};
use dxf::entities::{Entity, EntityType};
use dxf::enums::{DrawingUnits, Units};
use dxf::{Color, Drawing};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};
use std::f64::consts::{FRAC_PI_2, TAU};

/// Points per millimetre.
const PT_PER_MM: f64 = 72.0 / 25.4;
/// Lineweight used for DEFAULT and 0 (hundredths of a millimetre).
const DEFAULT_LINEWEIGHT: i16 = 25;
/// Samples per knot span of a sampled spline.
const SPLINE_SAMPLES: usize = 16;

/// Affine `[a b c d e f]`: `x' = a x + c y + e`, `y' = b x + d y + f`.
type Affine = [f64; 6];

fn apply(m: &Affine, p: [f64; 2]) -> [f64; 2] {
    [m[0] * p[0] + m[2] * p[1] + m[4], m[1] * p[0] + m[3] * p[1] + m[5]]
}

fn compose(a: &Affine, b: &Affine) -> Affine {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

/// Points per drawing unit and the unit label for provenance; `true` when unitless was assumed.
fn unit_scale(drawing: &Drawing) -> (f64, &'static str, bool) {
    let metric = matches!(drawing.header.drawing_units, DrawingUnits::Metric);
    let (mm, label) = match drawing.header.default_drawing_units {
        Units::Inches => (25.4, "in"),
        Units::Feet => (304.8, "ft"),
        Units::Miles => (1_609_344.0, "mi"),
        Units::Millimeters => (1.0, "mm"),
        Units::Centimeters => (10.0, "cm"),
        Units::Meters => (1000.0, "m"),
        Units::Kilometers => (1_000_000.0, "km"),
        Units::Microinches => (25.4e-6, "uin"),
        Units::Mils => (0.0254, "mil"),
        Units::Yards => (914.4, "yd"),
        Units::Angstroms => (1.0e-7, "angstrom"),
        Units::Nanometers => (1.0e-6, "nm"),
        Units::Microns => (1.0e-3, "um"),
        Units::Decimeters => (100.0, "dm"),
        Units::Decameters => (10_000.0, "dam"),
        Units::Hectometers => (100_000.0, "hm"),
        Units::USSurveyFeet => (1200.0 / 3937.0 * 1000.0, "us_survey_ft"),
        Units::USSurveyInch => (100.0 / 3937.0 * 1000.0, "us_survey_in"),
        Units::USSurveyYard => (3600.0 / 3937.0 * 1000.0, "us_survey_yd"),
        Units::USSurveyMile => (6_336_000.0 / 3937.0 * 1000.0, "us_survey_mi"),
        // Astronomical lengths cannot hold a drawing within the coordinate cap: refused by geometry.
        Units::Gigameters => (1.0e12, "Gm"),
        Units::AstronomicalUnits => (1.495_978_707e14, "au"),
        Units::LightYears => (9.460_730_472_580_8e18, "ly"),
        Units::Parsecs => (3.085_677_581_491_367e19, "pc"),
        Units::Unitless => {
            return if metric { (PT_PER_MM, "unitless_as_mm", true) } else { (72.0, "unitless_as_in", true) };
        }
    };
    (mm * PT_PER_MM, label, false)
}

/// Counts code-0 records (entities, table entries, objects) without allocating them: exact pairs
/// for ASCII, the `00 00` + uppercase-name pattern for binary (an overcount at worst).
fn count_records(bytes: &[u8], kind: &CadKind) -> usize {
    match kind {
        CadKind::DxfBinary => {
            let body = bytes.get(BINARY_DXF_SENTINEL.len()..).unwrap_or_default();
            body.windows(3).filter(|w| w[0] == 0 && w[1] == 0 && w[2].is_ascii_uppercase()).count()
        }
        _ => {
            let mut count = 0usize;
            let mut lines = bytes.split(|b| *b == b'\n');
            while let (Some(code), Some(_)) = (lines.next(), lines.next()) {
                if code.trim_ascii() == b"0" {
                    count = count.saturating_add(1);
                }
            }
            count
        }
    }
}

fn rgb_of(v: u32) -> [f32; 4] {
    let c = |shift: u32| f32::from(u8::try_from((v >> shift) & 0xFF).unwrap_or(0)) / 255.0;
    [c(16), c(8), c(0), 1.0]
}

/// Inherited BYBLOCK values of the insert being expanded.
#[derive(Clone, Copy)]
struct Inherit {
    aci: u8,
    true_colour: Option<u32>,
    weight: i16,
}

struct Layered {
    name: String,
    visible: bool,
    items: Vec<PdfDrawItem>,
}

struct Conv<'d> {
    drawing: &'d Drawing,
    policy: ImportPolicy,
    layer_colour: BTreeMap<String, (u8, bool)>,
    layer_weight: BTreeMap<String, i16>,
    layer_linetype: BTreeMap<String, String>,
    layers: Vec<Layered>,
    layer_index: BTreeMap<String, usize>,
    /// Layer of the previous drawn entity (for the regrouping loss).
    last_layer: Option<usize>,
    closed_layers: BTreeSet<usize>,
    regrouped: bool,
    losses: Vec<(LossKind, &'static str)>,
    noted: BTreeSet<&'static str>,
    segments: usize,
    items: usize,
    entities_seen: usize,
    bounds: Option<[f64; 4]>,
    block_stack: Vec<String>,
}

impl Conv<'_> {
    fn loss(&mut self, kind: LossKind, code: &'static str) {
        if self.noted.insert(code) {
            self.losses.push((kind, code));
        }
    }

    fn layer_slot(&mut self, name: &str) -> usize {
        if let Some(i) = self.layer_index.get(name) {
            return *i;
        }
        let visible = self.layer_colour.get(name).is_none_or(|(_, on)| *on);
        let i = self.layers.len();
        self.layers.push(Layered { name: name.to_owned(), visible, items: Vec::new() });
        self.layer_index.insert(name.to_owned(), i);
        i
    }

    fn colour(&mut self, e: &Entity, inherit: Inherit) -> [f32; 4] {
        let c = &e.common;
        if c.color_24_bit != 0 {
            let v = u32::try_from(c.color_24_bit & 0x00FF_FFFF).unwrap_or(0);
            return rgb_of(v);
        }
        let aci = if c.color.is_by_layer() {
            self.layer_colour.get(&c.layer).map_or(7, |(a, _)| *a)
        } else if c.color.is_by_block() {
            if let Some(v) = inherit.true_colour {
                return rgb_of(v);
            }
            inherit.aci
        } else {
            c.color.index().unwrap_or(7)
        };
        self.aci(aci)
    }

    fn aci(&mut self, aci: u8) -> [f32; 4] {
        if aci == 7 {
            self.loss(LossKind::Approximated, "dxf_aci_7_drawn_black");
            return [0.0, 0.0, 0.0, 1.0];
        }
        rgb_of(ACI_RGB[usize::from(aci)])
    }

    fn weight_pt(&self, e: &Entity, inherit: Inherit) -> f64 {
        let raw = match e.common.lineweight_enum_value {
            -1 => self.layer_weight.get(&e.common.layer).copied().unwrap_or(DEFAULT_LINEWEIGHT),
            -2 => inherit.weight,
            v => v,
        };
        let hundredths = if raw <= 0 { DEFAULT_LINEWEIGHT } else { raw };
        f64::from(hundredths) / 100.0 * PT_PER_MM
    }

    fn track(&mut self, p: [f64; 2]) -> Result<[f64; 2]> {
        if !p.iter().all(|v| v.is_finite() && v.abs() <= COORDINATE_LIMIT) {
            return Err(VectorError::InvalidGeometry);
        }
        self.bounds = Some(match self.bounds {
            None => [p[0], p[1], p[0], p[1]],
            Some(b) => [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])],
        });
        Ok(p)
    }

    fn budget(&mut self, segments: usize) -> Result<()> {
        self.segments = self.segments.saturating_add(segments);
        if self.segments > self.policy.limits.max_segments {
            return Err(VectorError::BudgetExceeded("segments"));
        }
        self.items = self.items.saturating_add(1);
        if self.items > self.policy.limits.max_nodes as usize {
            return Err(VectorError::BudgetExceeded("nodes"));
        }
        Ok(())
    }

    /// Transforms a path built in entity space to points and records it on the entity's layer.
    fn emit(&mut self, e: &Entity, m: &Affine, path: Vec<PdfEl>, fill: bool, inherit: Inherit) -> Result<()> {
        if path.len() < 2 {
            return Ok(());
        }
        self.budget(path.len())?;
        let mut out = Vec::with_capacity(path.len());
        for el in path {
            out.push(match el {
                PdfEl::MoveTo(p) => PdfEl::MoveTo(self.track(apply(m, p))?),
                PdfEl::LineTo(p) => PdfEl::LineTo(self.track(apply(m, p))?),
                PdfEl::CubicTo(a, b, c) => PdfEl::CubicTo(self.track(apply(m, a))?, self.track(apply(m, b))?, self.track(apply(m, c))?),
                PdfEl::Close => PdfEl::Close,
            });
        }
        let paint = PdfPaint::Solid(self.colour(e, inherit));
        let item = if fill {
            PdfDrawItem::Fill { path: out, even_odd: false, paint, blend: 0, glyph: None, source: None }
        } else {
            let lt = match e.common.line_type_name.to_ascii_uppercase().as_str() {
                "BYLAYER" | "" => self.layer_linetype.get(&e.common.layer).cloned().unwrap_or_default().to_ascii_uppercase(),
                other => other.to_owned(),
            };
            if !matches!(lt.as_str(), "" | "CONTINUOUS" | "BYBLOCK") {
                self.loss(LossKind::Approximated, "dxf_linetype_drawn_solid");
            }
            let stroke = PdfStroke { width: self.weight_pt(e, inherit), cap: 1, join: 1, miter_limit: 10.0, dash: Vec::new(), dash_offset: 0.0 };
            PdfDrawItem::Stroke { path: out, stroke, paint, blend: 0, glyph: None, source: None }
        };
        let slot = self.layer_slot(&e.common.layer);
        if let Some(last) = self.last_layer
            && last != slot
        {
            self.closed_layers.insert(last);
            if self.closed_layers.contains(&slot) {
                self.regrouped = true;
            }
        }
        self.last_layer = Some(slot);
        if let Some(l) = self.layers.get_mut(slot) {
            l.items.push(item);
        }
        Ok(())
    }

    /// Object coordinate system: an extrusion of (0, 0, -1) mirrors x; any other non-default
    /// normal is projected onto the XY plane and reported.
    fn ocs(&mut self, nz: f64, nx: f64, ny: f64) -> Affine {
        if nx.abs() < 1e-9 && ny.abs() < 1e-9 {
            if nz < 0.0 {
                return [-1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            }
            return [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        }
        self.loss(LossKind::Approximated, "dxf_non_planar_extrusion_projected");
        [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
    }

    fn entity(&mut self, e: &Entity, m: &Affine, inherit: Inherit, depth: usize, cancel: &CancellationToken) -> Result<()> {
        self.entities_seen = self.entities_seen.saturating_add(1);
        if self.entities_seen.is_multiple_of(256) {
            cancel.check().map_err(|_| VectorError::Canceled)?;
        }
        if e.common.is_in_paper_space {
            self.loss(LossKind::Ignored, "dxf_paper_space_ignored");
            return Ok(());
        }
        if !e.common.is_visible {
            self.loss(LossKind::Ignored, "dxf_invisible_entity_ignored");
            return Ok(());
        }
        match &e.specific {
            EntityType::Line(l) => {
                let path = vec![PdfEl::MoveTo([l.p1.x, l.p1.y]), PdfEl::LineTo([l.p2.x, l.p2.y])];
                self.emit(e, m, path, false, inherit)
            }
            EntityType::Circle(c) => {
                let o = self.ocs(c.normal.z, c.normal.x, c.normal.y);
                let path = arc_path(c.center.x, c.center.y, c.radius, 0.0, TAU, true)?;
                self.loss(LossKind::Approximated, "dxf_arc_to_cubic");
                self.emit(e, &compose(m, &o), path, false, inherit)
            }
            EntityType::Arc(a) => {
                let o = self.ocs(a.normal.z, a.normal.x, a.normal.y);
                let start = a.start_angle.to_radians();
                let mut sweep = a.end_angle.to_radians() - start;
                if sweep <= 0.0 {
                    sweep += TAU;
                }
                let path = arc_path(a.center.x, a.center.y, a.radius, start, sweep, true)?;
                self.loss(LossKind::Approximated, "dxf_arc_to_cubic");
                self.emit(e, &compose(m, &o), path, false, inherit)
            }
            EntityType::Ellipse(el) => {
                let (mx, my) = (el.major_axis.x, el.major_axis.y);
                let r = el.minor_axis_ratio;
                let flip = if el.normal.z < 0.0 { -1.0 } else { 1.0 };
                let frame: Affine = [mx, my, -my * r * flip, mx * r * flip, el.center.x, el.center.y];
                let start = el.start_parameter;
                let mut sweep = el.end_parameter - start;
                if sweep <= 0.0 {
                    sweep += TAU;
                }
                let closed = (sweep - TAU).abs() < 1e-9;
                let path = arc_path(0.0, 0.0, 1.0, start, sweep.min(TAU), closed)?;
                self.loss(LossKind::Approximated, "dxf_arc_to_cubic");
                self.emit(e, &compose(m, &frame), path, false, inherit)
            }
            EntityType::LwPolyline(p) => {
                let o = self.ocs(p.extrusion_direction.z, p.extrusion_direction.x, p.extrusion_direction.y);
                if p.constant_width != 0.0 || p.vertices.iter().any(|v| v.starting_width != 0.0 || v.ending_width != 0.0) {
                    self.loss(LossKind::Unsupported, "dxf_polyline_width_not_imported");
                }
                let closed = p.flags & 1 != 0;
                let verts: Vec<([f64; 2], f64)> = p.vertices.iter().map(|v| ([v.x, v.y], v.bulge)).collect();
                let path = self.bulge_path(&verts, closed)?;
                self.emit(e, &compose(m, &o), path, false, inherit)
            }
            EntityType::Polyline(p) => {
                if p.flags & (16 | 64) != 0 {
                    self.loss(LossKind::Unsupported, "dxf_polygon_mesh_not_imported");
                    return Ok(());
                }
                if p.flags & 8 != 0 {
                    self.loss(LossKind::Approximated, "dxf_3d_polyline_projected");
                }
                let o = if p.flags & 8 != 0 { [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] } else { self.ocs(p.normal.z, p.normal.x, p.normal.y) };
                if p.default_starting_width != 0.0 || p.default_ending_width != 0.0 {
                    self.loss(LossKind::Unsupported, "dxf_polyline_width_not_imported");
                }
                let closed = p.flags & 1 != 0;
                let verts: Vec<([f64; 2], f64)> = p.vertices().filter(|v| v.flags & 16 == 0).map(|v| ([v.location.x, v.location.y], v.bulge)).collect();
                let path = self.bulge_path(&verts, closed)?;
                self.emit(e, &compose(m, &o), path, false, inherit)
            }
            EntityType::Spline(s) => {
                let path = self.spline_path(s)?;
                self.emit(e, m, path, false, inherit)
            }
            EntityType::Solid(s) => {
                let o = self.ocs(s.extrusion_direction.z, s.extrusion_direction.x, s.extrusion_direction.y);
                let path = quad([s.first_corner.x, s.first_corner.y], [s.second_corner.x, s.second_corner.y], [s.third_corner.x, s.third_corner.y], [s.fourth_corner.x, s.fourth_corner.y]);
                self.emit(e, &compose(m, &o), path, true, inherit)
            }
            EntityType::Trace(s) => {
                let o = self.ocs(s.extrusion_direction.z, s.extrusion_direction.x, s.extrusion_direction.y);
                let path = quad([s.first_corner.x, s.first_corner.y], [s.second_corner.x, s.second_corner.y], [s.third_corner.x, s.third_corner.y], [s.fourth_corner.x, s.fourth_corner.y]);
                self.emit(e, &compose(m, &o), path, true, inherit)
            }
            EntityType::Insert(ins) => self.insert(e, ins, m, inherit, depth, cancel),
            EntityType::ModelPoint(_) => {
                self.loss(LossKind::Ignored, "dxf_point_ignored");
                Ok(())
            }
            EntityType::Text(_) | EntityType::MText(_) | EntityType::Attribute(_) | EntityType::AttributeDefinition(_) => {
                self.loss(LossKind::Unsupported, "dxf_text_not_imported");
                Ok(())
            }
            EntityType::RotatedDimension(_)
            | EntityType::RadialDimension(_)
            | EntityType::DiameterDimension(_)
            | EntityType::AngularThreePointDimension(_)
            | EntityType::OrdinateDimension(_) => {
                self.loss(LossKind::Unsupported, "dxf_dimension_not_imported");
                Ok(())
            }
            EntityType::Leader(_) => {
                self.loss(LossKind::Unsupported, "dxf_leader_not_imported");
                Ok(())
            }
            EntityType::Image(_) | EntityType::Wipeout(_) => {
                self.loss(LossKind::Unsupported, "dxf_image_not_imported");
                Ok(())
            }
            _ => {
                self.loss(LossKind::Unsupported, "dxf_entity_not_imported");
                Ok(())
            }
        }
    }

    fn insert(&mut self, e: &Entity, ins: &dxf::entities::Insert, m: &Affine, inherit: Inherit, depth: usize, cancel: &CancellationToken) -> Result<()> {
        if ins.attributes().next().is_some() {
            self.loss(LossKind::Unsupported, "dxf_text_not_imported");
        }
        let next_depth = depth.saturating_add(1);
        if next_depth > self.policy.limits.max_depth {
            return Err(VectorError::BudgetExceeded("dxf_insert_depth"));
        }
        if self.block_stack.iter().any(|b| b.eq_ignore_ascii_case(&ins.name)) {
            self.loss(LossKind::Ignored, "dxf_block_reference_cycle_skipped");
            return Ok(());
        }
        let drawing = self.drawing;
        let Some(block) = drawing.blocks().find(|b| b.name.eq_ignore_ascii_case(&ins.name)) else {
            self.loss(LossKind::Ignored, "dxf_block_missing");
            return Ok(());
        };
        let o = self.ocs(ins.extrusion_direction.z, ins.extrusion_direction.x, ins.extrusion_direction.y);
        let child = Inherit {
            aci: if e.common.color.is_by_layer() {
                self.layer_colour.get(&e.common.layer).map_or(7, |(a, _)| *a)
            } else if e.common.color.is_by_block() {
                inherit.aci
            } else {
                e.common.color.index().unwrap_or(7)
            },
            true_colour: if e.common.color_24_bit != 0 { u32::try_from(e.common.color_24_bit & 0x00FF_FFFF).ok() } else if e.common.color.is_by_block() { inherit.true_colour } else { None },
            weight: match e.common.lineweight_enum_value {
                -1 => self.layer_weight.get(&e.common.layer).copied().unwrap_or(DEFAULT_LINEWEIGHT),
                -2 => inherit.weight,
                v => v,
            },
        };
        let (sin, cos) = ins.rotation.to_radians().sin_cos();
        let cols = ins.column_count.max(1);
        let rows = ins.row_count.max(1);
        let copies = usize::try_from(i32::from(cols).saturating_mul(i32::from(rows))).unwrap_or(usize::MAX);
        if copies > self.policy.limits.max_nodes as usize {
            return Err(VectorError::BudgetExceeded("dxf_insert_array"));
        }
        self.block_stack.push(block.name.clone());
        for r in 0..rows {
            for c in 0..cols {
                let ox = f64::from(c) * ins.column_spacing;
                let oy = f64::from(r) * ins.row_spacing;
                // block space -> insert: translate(-base) , scale, (array offset in rotated frame), rotate, translate(location).
                let local: Affine = [ins.x_scale_factor, 0.0, 0.0, ins.y_scale_factor, -block.base_point.x * ins.x_scale_factor + ox, -block.base_point.y * ins.y_scale_factor + oy];
                let rot: Affine = [cos, sin, -sin, cos, ins.location.x, ins.location.y];
                let t = compose(m, &compose(&o, &compose(&rot, &local)));
                for child_entity in &block.entities {
                    let child_entity: &Entity = child_entity;
                    if child_entity.common.layer == "0" {
                        // Entities on layer 0 inside a block take the insert's layer.
                        let mut moved = child_entity.clone();
                        moved.common.layer.clone_from(&e.common.layer);
                        self.entity(&moved, &t, child, next_depth, cancel)?;
                    } else {
                        self.entity(child_entity, &t, child, next_depth, cancel)?;
                    }
                }
            }
        }
        self.block_stack.pop();
        Ok(())
    }

    /// Polyline with bulges (`tan(angle / 4)`, positive counter-clockwise) as lines and arcs.
    fn bulge_path(&mut self, verts: &[([f64; 2], f64)], closed: bool) -> Result<Vec<PdfEl>> {
        let Some(first) = verts.first() else { return Ok(Vec::new()) };
        let mut path = vec![PdfEl::MoveTo(first.0)];
        let n = verts.len();
        let count = if closed { n } else { n.saturating_sub(1) };
        for i in 0..count {
            let (p0, bulge) = verts[i];
            let j = i.saturating_add(1);
            let p1 = verts[if j < n { j } else { 0 }].0;
            if bulge.abs() < 1e-12 || !bulge.is_finite() {
                path.push(PdfEl::LineTo(p1));
                continue;
            }
            self.loss(LossKind::Approximated, "dxf_arc_to_cubic");
            let theta = 4.0 * bulge.atan();
            let (dx, dy) = (p1[0] - p0[0], p1[1] - p0[1]);
            let d = dx.hypot(dy);
            if d < 1e-12 {
                continue;
            }
            let r = d / (2.0 * (theta / 2.0).sin());
            let phi = dy.atan2(dx) + FRAC_PI_2 - theta / 2.0;
            let centre = [p0[0] + r * phi.cos(), p0[1] + r * phi.sin()];
            let start = (p0[1] - centre[1]).atan2(p0[0] - centre[0]);
            let arc = arc_path(centre[0], centre[1], r.abs(), start, theta, false)?;
            path.extend(arc.into_iter().skip(1));
        }
        if closed {
            path.push(PdfEl::Close);
        }
        Ok(path)
    }

    fn spline_path(&mut self, s: &dxf::entities::Spline) -> Result<Vec<PdfEl>> {
        let ctrl: Vec<[f64; 2]> = s.control_points.iter().map(|p| [p.x, p.y]).collect();
        if s.normal.z < 0.0 && s.normal.x.abs() < 1e-9 && s.normal.y.abs() < 1e-9 {
            // Splines are in world coordinates; the normal only states the plane.
        } else if s.normal.x.abs() > 1e-9 || s.normal.y.abs() > 1e-9 {
            self.loss(LossKind::Approximated, "dxf_non_planar_extrusion_projected");
        }
        if ctrl.is_empty() {
            let fit: Vec<[f64; 2]> = s.fit_points.iter().map(|p| [p.x, p.y]).collect();
            if fit.len() < 2 {
                return Ok(Vec::new());
            }
            self.loss(LossKind::Approximated, "dxf_spline_fit_points_as_polyline");
            let mut path = vec![PdfEl::MoveTo(fit[0])];
            path.extend(fit.iter().skip(1).map(|p| PdfEl::LineTo(*p)));
            if s.flags & 1 != 0 {
                path.push(PdfEl::Close);
            }
            return Ok(path);
        }
        let degree = usize::try_from(s.degree_of_curve).map_err(|_| VectorError::MalformedDxf("spline_degree"))?;
        let knots = &s.knot_values;
        let n = ctrl.len();
        if degree == 0 || degree >= n || knots.len() != n.saturating_add(degree).saturating_add(1) || !knots.iter().all(|k| k.is_finite()) || knots.windows(2).any(|w| w[0] > w[1]) {
            return Err(VectorError::MalformedDxf("spline_knots"));
        }
        let weights: Vec<f64> = if s.weight_values.len() == n { s.weight_values.clone() } else { vec![1.0; n] };
        if weights.iter().any(|w| !w.is_finite() || *w <= 0.0) {
            return Err(VectorError::MalformedDxf("spline_weights"));
        }
        let rational = weights.iter().any(|w| (w - 1.0).abs() > 1e-12);
        let clamped = knots[..=degree].iter().all(|k| *k == knots[0]) && knots[n..].iter().all(|k| *k == knots[n]);
        if !rational && clamped && degree <= 3 {
            return Ok(bezier_from_clamped(&ctrl, knots, degree));
        }
        self.loss(LossKind::Approximated, "dxf_spline_sampled");
        let (lo, hi) = (knots[degree], knots[n]);
        if hi <= lo {
            return Err(VectorError::MalformedDxf("spline_domain"));
        }
        let spans = knots[degree..=n].windows(2).filter(|w| w[1] > w[0]).count().max(1);
        let samples = spans.saturating_mul(SPLINE_SAMPLES);
        if samples > self.policy.limits.max_segments {
            return Err(VectorError::BudgetExceeded("segments"));
        }
        let mut path = Vec::with_capacity(samples.saturating_add(1));
        for i in 0..=samples {
            let t = lo + (hi - lo) * (i as f64 / samples as f64);
            let p = de_boor(&ctrl, &weights, knots, degree, t.min(hi));
            path.push(if i == 0 { PdfEl::MoveTo(p) } else { PdfEl::LineTo(p) });
        }
        Ok(path)
    }
}

/// Filled quad of a DXF SOLID/TRACE (corner order 1, 2, 4, 3).
fn quad(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> Vec<PdfEl> {
    vec![PdfEl::MoveTo(a), PdfEl::LineTo(b), PdfEl::LineTo(d), PdfEl::LineTo(c), PdfEl::Close]
}

/// Circular arc from `start` sweeping `sweep` radians (signed) as cubic Béziers of at most 90°.
fn arc_path(cx: f64, cy: f64, r: f64, start: f64, sweep: f64, close_full: bool) -> Result<Vec<PdfEl>> {
    if !(r.is_finite() && r > 0.0 && start.is_finite() && sweep.is_finite()) || sweep.abs() > TAU + 1e-9 {
        return Err(VectorError::InvalidGeometry);
    }
    let pieces = (sweep.abs() / FRAC_PI_2).ceil().clamp(1.0, 4.0);
    let step = sweep / pieces;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let at = |a: f64| [cx + r * a.cos(), cy + r * a.sin()];
    let mut path = vec![PdfEl::MoveTo(at(start))];
    let mut a0 = start;
    let count = pieces as u8;
    for _ in 0..count {
        let a1 = a0 + step;
        let (p0, p3) = (at(a0), at(a1));
        let c1 = [p0[0] - k * r * a0.sin(), p0[1] + k * r * a0.cos()];
        let c2 = [p3[0] + k * r * a1.sin(), p3[1] - k * r * a1.cos()];
        path.push(PdfEl::CubicTo(c1, c2, p3));
        a0 = a1;
    }
    if close_full && (sweep.abs() - TAU).abs() < 1e-9 {
        path.push(PdfEl::Close);
    }
    Ok(path)
}

/// Rational de Boor evaluation at `t` (2D control points, weights).
fn de_boor(ctrl: &[[f64; 2]], weights: &[f64], knots: &[f64], p: usize, t: f64) -> [f64; 2] {
    let n = ctrl.len();
    // Span k with knots[k] <= t < knots[k + 1], clamped to the last non-empty span.
    let mut k = p;
    while k.saturating_add(1) < n && knots[k.saturating_add(1)] <= t {
        k = k.saturating_add(1);
    }
    let base = k.saturating_sub(p);
    let mut d: Vec<[f64; 3]> = (0..=p)
        .map(|j| {
            let i = base.saturating_add(j).min(n.saturating_sub(1));
            let w = weights[i];
            [ctrl[i][0] * w, ctrl[i][1] * w, w]
        })
        .collect();
    for r in 1..=p {
        for j in (r..=p).rev() {
            let i = base.saturating_add(j);
            let lo = knots[i];
            let hi = knots.get(i.saturating_add(p).saturating_add(1).saturating_sub(r)).copied().unwrap_or(lo);
            let a = if hi > lo { (t - lo) / (hi - lo) } else { 0.0 };
            let prev = d[j.saturating_sub(1)];
            let cur = d[j];
            d[j] = [(1.0 - a) * prev[0] + a * cur[0], (1.0 - a) * prev[1] + a * cur[1], (1.0 - a) * prev[2] + a * cur[2]];
        }
    }
    let q = d[p];
    if q[2].abs() < 1e-300 { [q[0], q[1]] } else { [q[0] / q[2], q[1] / q[2]] }
}

/// Exact Bézier pieces of a clamped non-rational B-spline of degree 1..=3: knot insertion (Boehm)
/// until every interior knot has multiplicity `p`; degree 2 pieces are raised to cubics.
fn bezier_from_clamped(ctrl: &[[f64; 2]], knots: &[f64], p: usize) -> Vec<PdfEl> {
    let mut pts = ctrl.to_vec();
    let mut u = knots.to_vec();
    let mut i = p.saturating_add(1);
    loop {
        let last = pts.len();
        if i >= last {
            break;
        }
        let t = u[i];
        let mult = u.iter().filter(|k| **k == t).count();
        if mult < p {
            // Insert t once: span k with u[k] <= t < u[k+1] is the last index holding t.
            let k = u.iter().rposition(|k| *k <= t).unwrap_or(p);
            let mut q = Vec::with_capacity(pts.len().saturating_add(1));
            for j in 0..=pts.len() {
                if j.saturating_add(p) <= k {
                    q.push(pts[j]);
                } else if j > k {
                    q.push(pts[j.saturating_sub(1)]);
                } else {
                    let denom = u[j.saturating_add(p)] - u[j];
                    let a = if denom > 0.0 { (t - u[j]) / denom } else { 0.0 };
                    let (a0, a1) = (pts[j.saturating_sub(1)], pts[j]);
                    q.push([(1.0 - a) * a0[0] + a * a1[0], (1.0 - a) * a0[1] + a * a1[1]]);
                }
            }
            pts = q;
            u.insert(k.saturating_add(1), t);
            continue;
        }
        i = i.saturating_add(mult.max(1));
    }
    let mut path = vec![PdfEl::MoveTo(pts[0])];
    let mut j = 0usize;
    while j.saturating_add(p) < pts.len() {
        match p {
            1 => path.push(PdfEl::LineTo(pts[j.saturating_add(1)])),
            2 => {
                let (a, b, c) = (pts[j], pts[j.saturating_add(1)], pts[j.saturating_add(2)]);
                let c1 = [a[0] + 2.0 / 3.0 * (b[0] - a[0]), a[1] + 2.0 / 3.0 * (b[1] - a[1])];
                let c2 = [c[0] + 2.0 / 3.0 * (b[0] - c[0]), c[1] + 2.0 / 3.0 * (b[1] - c[1])];
                path.push(PdfEl::CubicTo(c1, c2, c));
            }
            _ => path.push(PdfEl::CubicTo(pts[j.saturating_add(1)], pts[j.saturating_add(2)], pts[j.saturating_add(3)])),
        }
        j = j.saturating_add(p);
    }
    path
}

/// The drawing is converted in this module; the builder never asks this provider for a PDF.
struct NoPdf;
impl PdfDrawingProvider for NoPdf {
    fn drawing(&self, _: &[u8], _: &CancellationToken) -> Result<PdfDrawing> {
        Err(VectorError::PdfUnavailable)
    }
}

/// Imports an ASCII or binary DXF drawing as a Folio proposal (module docs). DWG is
/// [`VectorError::DwgUnavailable`]; anything else is `Unsupported("not_a_dxf_file")`.
pub fn import_dxf(bytes: &[u8], policy: &ImportPolicy, mint: &mut dyn IdMint, cancel: &CancellationToken) -> Result<VectorProposal> {
    cancel.check().map_err(|_| VectorError::Canceled)?;
    policy.validate()?;
    if bytes.len() > policy.limits.max_bytes {
        return Err(VectorError::BudgetExceeded("bytes"));
    }
    let kind = identify_cad(bytes);
    match kind {
        CadKind::DxfAscii | CadKind::DxfBinary => {}
        CadKind::Dwg { .. } => {
            crate::cad::import_dwg(bytes, policy, cancel)?;
            return Err(VectorError::Unsupported("not_a_dxf_file"));
        }
        CadKind::NotCad => return Err(VectorError::Unsupported("not_a_dxf_file")),
    }
    let records = count_records(bytes, &kind);
    let record_cap = (policy.limits.max_nodes as usize).saturating_mul(4).saturating_add(4096);
    if records > record_cap {
        return Err(VectorError::BudgetExceeded("dxf_records"));
    }
    let drawing = std::panic::catch_unwind(|| Drawing::load(&mut std::io::Cursor::new(bytes)))
        .map_err(|_| VectorError::MalformedDxf("parser_panicked"))?
        .map_err(|_| VectorError::MalformedDxf("parse"))?;
    cancel.check().map_err(|_| VectorError::Canceled)?;
    let (scale, unit, unitless) = unit_scale(&drawing);
    let mut conv = Conv {
        drawing: &drawing,
        policy: *policy,
        layer_colour: BTreeMap::new(),
        layer_weight: BTreeMap::new(),
        layer_linetype: BTreeMap::new(),
        layers: Vec::new(),
        layer_index: BTreeMap::new(),
        last_layer: None,
        closed_layers: BTreeSet::new(),
        regrouped: false,
        losses: Vec::new(),
        noted: BTreeSet::new(),
        segments: 0,
        items: 0,
        entities_seen: 0,
        bounds: None,
        block_stack: Vec::new(),
    };
    if unitless {
        conv.loss(LossKind::Approximated, "dxf_units_unitless_assumed");
    }
    for layer in drawing.layers() {
        let colour: &Color = &layer.color;
        let aci = colour.index().filter(|a| *a > 0).unwrap_or(7);
        conv.layer_colour.insert(layer.name.clone(), (aci, layer.is_layer_on));
        conv.layer_weight.insert(layer.name.clone(), layer.line_weight.raw_value());
        conv.layer_linetype.insert(layer.name.clone(), layer.line_type_name.clone());
        conv.layer_slot(&layer.name);
    }
    let top: Affine = [scale, 0.0, 0.0, scale, 0.0, 0.0];
    let inherit = Inherit { aci: 7, true_colour: None, weight: DEFAULT_LINEWEIGHT };
    for e in drawing.entities() {
        conv.entity(e, &top, inherit, 0, cancel)?;
    }
    if conv.regrouped {
        conv.loss(LossKind::Approximated, "dxf_draw_order_grouped_by_layer");
    }
    let Some(bounds) = conv.bounds else {
        return Err(VectorError::Unsupported("dxf_without_drawable_entities"));
    };
    // A drawing of one point or one axis-parallel line still gets a non-empty artboard.
    let pad = |lo: f64, hi: f64| if hi - lo < 1.0 { (lo - 0.5, hi + 0.5) } else { (lo, hi) };
    let (x0, x1) = pad(bounds[0], bounds[2]);
    let (y0, y1) = pad(bounds[1], bounds[3]);
    let Conv { layers, losses, .. } = conv;
    let mut items = Vec::new();
    let mut pdf_layers = Vec::new();
    for l in layers.into_iter().filter(|l| !l.items.is_empty()) {
        items.push(PdfDrawItem::BeginLayer { layer: pdf_layers.len() });
        items.extend(l.items);
        items.push(PdfDrawItem::EndLayer);
        pdf_layers.push(PdfLayer { name: l.name, visible: l.visible });
    }
    let page = PdfDrawingPage { media_box: [x0, y0, x1, y1], crop_box: [x0, y0, x1, y1], items, notes: Vec::new() };
    let pdf = PdfDrawing { pages: vec![page], fonts: Vec::new(), layers: pdf_layers, profiles: Vec::new(), output_intent: None };
    let source = DrawingSource {
        format: if matches!(kind, CadKind::DxfBinary) { "dxf_binary" } else { "dxf_ascii" },
        unit,
        root: "/dxf",
        source_len: bytes.len(),
        decoded_len: bytes.len(),
        losses,
    };
    document_from_drawing(&pdf, &source, policy, &NoPdf, mint, cancel).map(|a| a.proposal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SequentialIdMint;
    use dxf::Point as DxfPoint;
    use dxf::entities::{Circle, Line};

    /// The same drawing saved by the `dxf` crate as ASCII and as binary DXF imports to the same paths.
    #[test]
    fn binary_dxf_imports_like_ascii() {
        let mut d = Drawing::new();
        d.add_entity(Entity::new(EntityType::Line(Line::new(DxfPoint::new(0.0, 0.0, 0.0), DxfPoint::new(10.0, 5.0, 0.0)))));
        d.add_entity(Entity::new(EntityType::Circle(Circle::new(DxfPoint::new(5.0, 5.0, 0.0), 2.0))));
        let (mut ascii, mut binary) = (Vec::new(), Vec::new());
        d.save(&mut ascii).expect("ascii");
        d.save_binary(&mut binary).expect("binary");
        assert_eq!(identify_cad(&binary), CadKind::DxfBinary);
        let run = |bytes: &[u8]| import_dxf(bytes, &ImportPolicy::default(), &mut SequentialIdMint::new(2), &CancellationToken::default()).expect("import");
        let (a, b) = (run(&ascii), run(&binary));
        assert_eq!(a.provenance.source_format, "dxf_ascii");
        assert_eq!(b.provenance.source_format, "dxf_binary");
        assert_eq!(a.paths.len(), 2);
        assert_eq!(a.paths, b.paths);
    }
}
