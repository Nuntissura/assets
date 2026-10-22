//! DXF import (lane SMB-DXF-EPS-IMPORT; parity row AF-06). ASCII and binary DXF are read by the
//! crate's own reader ([`crate::dxf_reader`], adapted from the MIT `dxf` crate 0.6.2 with NOTICE
//! attribution; no external dependency, built by default) and turned into the drawing model the
//! `.ai` import already builds Folio proposals from, so DXF reuses that builder (layers, paint
//! styles, validation):
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
//!   depth, with a cycle check; BYBLOCK colour and weight come from the insert; entities on layer
//!   0 inside a block take the insert's layer;
//! - colour: true colour, else the ACI palette (`dxf_aci`; ACI 7 is drawn black on the white
//!   artboard, `dxf_aci_7_drawn_black`), BYLAYER from the layer table; weight: lineweight in
//!   hundredths of a millimetre (default 0.25 mm); linetypes other than CONTINUOUS are drawn solid
//!   (`dxf_linetype_drawn_solid`);
//! - typed losses for what is not drawn: text and attributes (`dxf_text_not_imported`),
//!   dimensions, leaders, images and wipeouts, points, paper space, invisible entities, polyline
//!   widths, polygon meshes, non-planar extrusions, and every other entity type (hatches, 3D faces,
//!   3D solids, regions, proxies, stray VERTEX/SEQEND records: `dxf_entity_not_imported`). No
//!   entity type is skipped without a loss. The DXF thumbnail (a BMP preview) is not read.
//!
//! Hostile input: the input size is capped by `max_bytes`; the code-0 records are counted in a
//! pre-scan before the reader allocates entities (`BudgetExceeded("dxf_records")`); stored
//! vertices, knots, weights and points are capped (`BudgetExceeded("dxf_values")`); damaged or
//! truncated input is `MalformedDxf`; insert expansion is bounded by depth, a cycle check, the
//! array size, the entity-visit budget (`BudgetExceeded("dxf_entity_visits")`) and the segment
//! and item budgets (proofs: `tests/dxf_import.rs` `hostile_dxf_is_bounded_and_typed`,
//! `adapted_reader_rejects_damage`, `insert_fan_out_is_bounded`; unit tests below).
#![deny(clippy::arithmetic_side_effects)]
use crate::ai_import::{DrawingSource, PdfDrawItem, PdfDrawing, PdfDrawingPage, PdfDrawingProvider, PdfEl, PdfLayer, PdfPaint, PdfStroke, document_from_drawing};
use crate::cad::{BINARY_DXF_SENTINEL, CadKind, identify_cad};
use crate::dxf_aci::ACI_RGB;
use crate::dxf_reader::{Drawing, Entity, Kind, P3, read_drawing};
use crate::error::{Result, VectorError};
use crate::ids::IdMint;
use crate::model::{COORDINATE_LIMIT, ImportPolicy, LossKind, VectorProposal};
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
const IDENTITY: Affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

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

fn xy(p: P3) -> [f64; 2] {
    [p[0], p[1]]
}

/// Points per drawing unit and the unit label for provenance; `true` when unitless was assumed.
/// `$INSUNITS` codes as in the DXF reference (and `dxf` 0.6.2 `Units`); an unknown code is unitless.
fn unit_scale(drawing: &Drawing) -> (f64, &'static str, bool) {
    let metric = drawing.measurement == 1;
    let (mm, label) = match drawing.insunits {
        1 => (25.4, "in"),
        2 => (304.8, "ft"),
        3 => (1_609_344.0, "mi"),
        4 => (1.0, "mm"),
        5 => (10.0, "cm"),
        6 => (1000.0, "m"),
        7 => (1_000_000.0, "km"),
        8 => (25.4e-6, "uin"),
        9 => (0.0254, "mil"),
        10 => (914.4, "yd"),
        11 => (1.0e-7, "angstrom"),
        12 => (1.0e-6, "nm"),
        13 => (1.0e-3, "um"),
        14 => (100.0, "dm"),
        15 => (10_000.0, "dam"),
        16 => (100_000.0, "hm"),
        // Astronomical lengths cannot hold a drawing within the coordinate cap: refused by geometry.
        17 => (1.0e12, "Gm"),
        18 => (1.495_978_707e14, "au"),
        19 => (9.460_730_472_580_8e18, "ly"),
        20 => (3.085_677_581_491_367e19, "pc"),
        21 => (1200.0 / 3937.0 * 1000.0, "us_survey_ft"),
        22 => (100.0 / 3937.0 * 1000.0, "us_survey_in"),
        23 => (3600.0 / 3937.0 * 1000.0, "us_survey_yd"),
        24 => (6_336_000.0 / 3937.0 * 1000.0, "us_survey_mi"),
        _ => {
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
    /// Upper-case block name to its index (the first block of a name wins).
    blocks: BTreeMap<String, usize>,
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
    /// Entities visited, every insert copy included (bounded by `max_segments`).
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

    /// `layer` is the entity's effective layer (layer 0 inside a block becomes the insert's).
    fn colour(&mut self, e: &Entity, layer: &str, inherit: Inherit) -> [f32; 4] {
        let c = &e.common;
        if c.true_colour != 0 {
            let v = u32::try_from(c.true_colour & 0x00FF_FFFF).unwrap_or(0);
            return rgb_of(v);
        }
        let aci = if c.colour.is_by_layer() {
            self.layer_colour.get(layer).map_or(7, |(a, _)| *a)
        } else if c.colour.is_by_block() {
            if let Some(v) = inherit.true_colour {
                return rgb_of(v);
            }
            inherit.aci
        } else {
            c.colour.index().unwrap_or(7)
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

    /// Lineweight in hundredths of a millimetre: -1 BYLAYER, -2 BYBLOCK, else the value.
    fn raw_weight(&self, e: &Entity, layer: &str, inherit: Inherit) -> i16 {
        match e.common.lineweight {
            -1 => self.layer_weight.get(layer).copied().unwrap_or(DEFAULT_LINEWEIGHT),
            -2 => inherit.weight,
            v => v,
        }
    }

    fn weight_pt(&self, e: &Entity, layer: &str, inherit: Inherit) -> f64 {
        let raw = self.raw_weight(e, layer, inherit);
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
    fn emit(&mut self, e: &Entity, layer: &str, m: &Affine, path: Vec<PdfEl>, fill: bool, inherit: Inherit) -> Result<()> {
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
        let paint = PdfPaint::Solid(self.colour(e, layer, inherit));
        let item = if fill {
            PdfDrawItem::Fill { path: out, even_odd: false, paint, blend: 0, glyph: None, source: None }
        } else {
            let lt = match e.common.line_type.to_ascii_uppercase().as_str() {
                "BYLAYER" | "" => self.layer_linetype.get(layer).cloned().unwrap_or_default().to_ascii_uppercase(),
                other => other.to_owned(),
            };
            if !matches!(lt.as_str(), "" | "CONTINUOUS" | "BYBLOCK") {
                self.loss(LossKind::Approximated, "dxf_linetype_drawn_solid");
            }
            let stroke = PdfStroke { width: self.weight_pt(e, layer, inherit), cap: 1, join: 1, miter_limit: 10.0, dash: Vec::new(), dash_offset: 0.0 };
            PdfDrawItem::Stroke { path: out, stroke, paint, blend: 0, glyph: None, source: None }
        };
        let slot = self.layer_slot(layer);
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
    fn ocs(&mut self, n: P3) -> Affine {
        let [nx, ny, nz] = n;
        if nx.abs() < 1e-9 && ny.abs() < 1e-9 {
            if nz < 0.0 {
                return [-1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            }
            return IDENTITY;
        }
        self.loss(LossKind::Approximated, "dxf_non_planar_extrusion_projected");
        IDENTITY
    }

    fn entity(&mut self, e: &Entity, layer: &str, m: &Affine, inherit: Inherit, depth: usize, cancel: &CancellationToken) -> Result<()> {
        self.entities_seen = self.entities_seen.saturating_add(1);
        if self.entities_seen > self.policy.limits.max_segments {
            return Err(VectorError::BudgetExceeded("dxf_entity_visits"));
        }
        if self.entities_seen.is_multiple_of(256) {
            cancel.check().map_err(|_| VectorError::Canceled)?;
        }
        if e.common.paper_space {
            self.loss(LossKind::Ignored, "dxf_paper_space_ignored");
            return Ok(());
        }
        if !e.common.visible {
            self.loss(LossKind::Ignored, "dxf_invisible_entity_ignored");
            return Ok(());
        }
        match &e.kind {
            Kind::Line { p1, p2 } => {
                let path = vec![PdfEl::MoveTo(xy(*p1)), PdfEl::LineTo(xy(*p2))];
                self.emit(e, layer, m, path, false, inherit)
            }
            Kind::Circle { centre, radius, normal } => {
                let o = self.ocs(*normal);
                let path = arc_path(centre[0], centre[1], *radius, 0.0, TAU, true)?;
                self.loss(LossKind::Approximated, "dxf_arc_to_cubic");
                self.emit(e, layer, &compose(m, &o), path, false, inherit)
            }
            Kind::Arc { centre, radius, normal, start, end } => {
                let o = self.ocs(*normal);
                let start = start.to_radians();
                let mut sweep = end.to_radians() - start;
                if sweep <= 0.0 {
                    sweep += TAU;
                }
                let path = arc_path(centre[0], centre[1], *radius, start, sweep, true)?;
                self.loss(LossKind::Approximated, "dxf_arc_to_cubic");
                self.emit(e, layer, &compose(m, &o), path, false, inherit)
            }
            Kind::Ellipse { centre, major, normal, ratio, start, end } => {
                let (mx, my) = (major[0], major[1]);
                let r = *ratio;
                let flip = if normal[2] < 0.0 { -1.0 } else { 1.0 };
                let frame: Affine = [mx, my, -my * r * flip, mx * r * flip, centre[0], centre[1]];
                let mut sweep = *end - *start;
                if sweep <= 0.0 {
                    sweep += TAU;
                }
                let closed = (sweep - TAU).abs() < 1e-9;
                let path = arc_path(0.0, 0.0, 1.0, *start, sweep.min(TAU), closed)?;
                self.loss(LossKind::Approximated, "dxf_arc_to_cubic");
                self.emit(e, layer, &compose(m, &frame), path, false, inherit)
            }
            Kind::LwPolyline { flags, constant_width, vertices, extrusion } => {
                let o = self.ocs(*extrusion);
                if *constant_width != 0.0 || vertices.iter().any(|v| v.start_width != 0.0 || v.end_width != 0.0) {
                    self.loss(LossKind::Unsupported, "dxf_polyline_width_not_imported");
                }
                let closed = *flags & 1 != 0;
                let verts: Vec<([f64; 2], f64)> = vertices.iter().map(|v| ([v.x, v.y], v.bulge)).collect();
                let path = self.bulge_path(&verts, closed)?;
                self.emit(e, layer, &compose(m, &o), path, false, inherit)
            }
            Kind::Polyline { flags, start_width, end_width, normal, vertices } => {
                if *flags & (16 | 64) != 0 {
                    self.loss(LossKind::Unsupported, "dxf_polygon_mesh_not_imported");
                    return Ok(());
                }
                if *flags & 8 != 0 {
                    self.loss(LossKind::Approximated, "dxf_3d_polyline_projected");
                }
                let o = if *flags & 8 != 0 { IDENTITY } else { self.ocs(*normal) };
                if *start_width != 0.0 || *end_width != 0.0 || vertices.iter().any(|v| v.start_width != 0.0 || v.end_width != 0.0) {
                    self.loss(LossKind::Unsupported, "dxf_polyline_width_not_imported");
                }
                let closed = *flags & 1 != 0;
                let verts: Vec<([f64; 2], f64)> = vertices.iter().filter(|v| v.flags & 16 == 0).map(|v| (xy(v.location), v.bulge)).collect();
                let path = self.bulge_path(&verts, closed)?;
                self.emit(e, layer, &compose(m, &o), path, false, inherit)
            }
            Kind::Spline { .. } => {
                let path = self.spline_path(&e.kind)?;
                self.emit(e, layer, m, path, false, inherit)
            }
            Kind::Solid { corners, extrusion } | Kind::Trace { corners, extrusion } => {
                let o = self.ocs(*extrusion);
                let path = quad(xy(corners[0]), xy(corners[1]), xy(corners[2]), xy(corners[3]));
                self.emit(e, layer, &compose(m, &o), path, true, inherit)
            }
            Kind::Insert { .. } => self.insert(e, layer, m, inherit, depth, cancel),
            Kind::Point => {
                self.loss(LossKind::Ignored, "dxf_point_ignored");
                Ok(())
            }
            Kind::Text | Kind::Attrib => {
                self.loss(LossKind::Unsupported, "dxf_text_not_imported");
                Ok(())
            }
            Kind::Dimension => {
                self.loss(LossKind::Unsupported, "dxf_dimension_not_imported");
                Ok(())
            }
            Kind::Leader => {
                self.loss(LossKind::Unsupported, "dxf_leader_not_imported");
                Ok(())
            }
            Kind::Image => {
                self.loss(LossKind::Unsupported, "dxf_image_not_imported");
                Ok(())
            }
            Kind::Vertex(_) | Kind::Seqend | Kind::Other => {
                self.loss(LossKind::Unsupported, "dxf_entity_not_imported");
                Ok(())
            }
        }
    }

    fn insert(&mut self, e: &Entity, layer: &str, m: &Affine, inherit: Inherit, depth: usize, cancel: &CancellationToken) -> Result<()> {
        let Kind::Insert { name, location, x_scale, y_scale, rotation, columns, rows, column_spacing, row_spacing, extrusion, attributes, .. } = &e.kind else {
            return Ok(());
        };
        if *attributes > 0 {
            self.loss(LossKind::Unsupported, "dxf_text_not_imported");
        }
        let next_depth = depth.saturating_add(1);
        if next_depth > self.policy.limits.max_depth {
            return Err(VectorError::BudgetExceeded("dxf_insert_depth"));
        }
        if self.block_stack.iter().any(|b| b.eq_ignore_ascii_case(name)) {
            self.loss(LossKind::Ignored, "dxf_block_reference_cycle_skipped");
            return Ok(());
        }
        let drawing = self.drawing;
        let Some(block) = self.blocks.get(&name.to_ascii_uppercase()).and_then(|i| drawing.blocks.get(*i)) else {
            self.loss(LossKind::Ignored, "dxf_block_missing");
            return Ok(());
        };
        let o = self.ocs(*extrusion);
        let c = &e.common;
        let child = Inherit {
            aci: if c.colour.is_by_layer() {
                self.layer_colour.get(layer).map_or(7, |(a, _)| *a)
            } else if c.colour.is_by_block() {
                inherit.aci
            } else {
                c.colour.index().unwrap_or(7)
            },
            true_colour: if c.true_colour != 0 { u32::try_from(c.true_colour & 0x00FF_FFFF).ok() } else if c.colour.is_by_block() { inherit.true_colour } else { None },
            weight: self.raw_weight(e, layer, inherit),
        };
        let (sin, cos) = rotation.to_radians().sin_cos();
        let cols = (*columns).max(1);
        let rows = (*rows).max(1);
        let copies = usize::try_from(i32::from(cols).saturating_mul(i32::from(rows))).unwrap_or(usize::MAX);
        if copies > self.policy.limits.max_nodes as usize {
            return Err(VectorError::BudgetExceeded("dxf_insert_array"));
        }
        self.block_stack.push(block.name.clone());
        for r in 0..rows {
            for col in 0..cols {
                let ox = f64::from(col) * *column_spacing;
                let oy = f64::from(r) * *row_spacing;
                // block space -> insert: translate(-base), scale, (array offset in rotated frame), rotate, translate(location).
                let local: Affine = [*x_scale, 0.0, 0.0, *y_scale, -block.base[0] * *x_scale + ox, -block.base[1] * *y_scale + oy];
                let rot: Affine = [cos, sin, -sin, cos, location[0], location[1]];
                let t = compose(m, &compose(&o, &compose(&rot, &local)));
                for child_entity in &block.entities {
                    // Entities on layer 0 inside a block take the insert's layer.
                    let child_layer = if child_entity.common.layer == "0" { layer } else { child_entity.common.layer.as_str() };
                    self.entity(child_entity, child_layer, &t, child, next_depth, cancel)?;
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

    fn spline_path(&mut self, spline: &Kind) -> Result<Vec<PdfEl>> {
        let Kind::Spline { normal, flags, degree, knots, weights, control, fit } = spline else { return Ok(Vec::new()) };
        let ctrl: Vec<[f64; 2]> = control.iter().map(|p| xy(*p)).collect();
        if normal[0].abs() > 1e-9 || normal[1].abs() > 1e-9 {
            // Splines are in world coordinates; the normal only states the plane.
            self.loss(LossKind::Approximated, "dxf_non_planar_extrusion_projected");
        }
        if ctrl.is_empty() {
            let fit: Vec<[f64; 2]> = fit.iter().map(|p| xy(*p)).collect();
            if fit.len() < 2 {
                return Ok(Vec::new());
            }
            self.loss(LossKind::Approximated, "dxf_spline_fit_points_as_polyline");
            let mut path = vec![PdfEl::MoveTo(fit[0])];
            path.extend(fit.iter().skip(1).map(|p| PdfEl::LineTo(*p)));
            if *flags & 1 != 0 {
                path.push(PdfEl::Close);
            }
            return Ok(path);
        }
        let degree = usize::try_from(*degree).map_err(|_| VectorError::MalformedDxf("spline_degree"))?;
        let n = ctrl.len();
        if degree == 0 || degree >= n || knots.len() != n.saturating_add(degree).saturating_add(1) || !knots.iter().all(|k| k.is_finite()) || knots.windows(2).any(|w| w[0] > w[1]) {
            return Err(VectorError::MalformedDxf("spline_knots"));
        }
        let weights: Vec<f64> = if weights.len() == n { weights.clone() } else { vec![1.0; n] };
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
    let value_cap = policy.limits.max_segments.saturating_mul(2).saturating_add(4096);
    let drawing = read_drawing(bytes, matches!(kind, CadKind::DxfBinary), value_cap, cancel)?;
    cancel.check().map_err(|_| VectorError::Canceled)?;
    let (scale, unit, unitless) = unit_scale(&drawing);
    let mut blocks = BTreeMap::new();
    for (i, b) in drawing.blocks.iter().enumerate() {
        blocks.entry(b.name.to_ascii_uppercase()).or_insert(i);
    }
    let mut conv = Conv {
        drawing: &drawing,
        policy: *policy,
        blocks,
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
    for layer in &drawing.layers {
        let aci = layer.colour.index().unwrap_or(7);
        conv.layer_colour.insert(layer.name.clone(), (aci, layer.on));
        conv.layer_weight.insert(layer.name.clone(), layer.line_weight);
        conv.layer_linetype.insert(layer.name.clone(), layer.line_type.clone());
        conv.layer_slot(&layer.name);
    }
    let top: Affine = [scale, 0.0, 0.0, scale, 0.0, 0.0];
    let inherit = Inherit { aci: 7, true_colour: None, weight: DEFAULT_LINEWEIGHT };
    for e in &drawing.entities {
        conv.entity(e, &e.common.layer, &top, inherit, 0, cancel)?;
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
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::dxf_reader::{ValueType, value_type};
    use crate::ids::SequentialIdMint;

    /// `(code, value)` pairs of a small drawing: a layer table with an escaped non-ASCII name, a
    /// block, a line with true colour, a circle and an insert, plus 64-bit, boolean and
    /// binary-chunk pairs the reader must skip by type.
    fn sample() -> Vec<(i32, &'static str)> {
        vec![
            (0, "SECTION"), (2, "HEADER"), (9, "$INSUNITS"), (70, "4"), (9, "$HANDSEED"), (5, "FF"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "TABLES"), (0, "TABLE"), (2, "LAYER"), (70, "1"),
            (0, "LAYER"), (2, "Te\\U+00DFt"), (70, "0"), (62, "-3"), (6, "CONTINUOUS"), (370, "50"), (290, "1"),
            (0, "ENDTAB"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "BLOCKS"), (0, "BLOCK"), (8, "0"), (2, "B"), (70, "0"), (10, "1"), (20, "1"), (30, "0"),
            (0, "LINE"), (8, "0"), (10, "1"), (20, "1"), (30, "0"), (11, "2"), (21, "2"), (31, "0"), (0, "ENDBLK"), (8, "0"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "ENTITIES"),
            (0, "LINE"), (5, "1A"), (8, "Te\\U+00DFt"), (10, "0"), (20, "0"), (30, "0"), (11, "10"), (21, "5"), (31, "0"), (420, "65280"),
            (0, "CIRCLE"), (8, "Te\\U+00DFt"), (10, "5"), (20, "5"), (30, "0"), (40, "2"), (160, "12345678901"), (310, "00FF"),
            (0, "INSERT"), (8, "0"), (2, "b"), (10, "20"), (20, "0"), (30, "0"),
            (0, "ENDSEC"), (0, "EOF"),
        ]
    }

    fn ascii(pairs: &[(i32, &str)]) -> Vec<u8> {
        let mut s = String::from("999\r\na comment\r\n");
        for (c, v) in pairs {
            s.push_str(&format!("{c:>3}\r\n{v}\r\n"));
        }
        s.into_bytes()
    }

    /// Binary DXF of the same pairs: two-byte codes (R13+) or one-byte codes with the 255 escape
    /// (R12), values encoded by the group-code type table.
    fn binary(pairs: &[(i32, &str)], two_byte: bool) -> Vec<u8> {
        let mut out = BINARY_DXF_SENTINEL.to_vec();
        for (c, v) in pairs {
            if two_byte {
                out.extend_from_slice(&u16::try_from(*c).expect("code").to_le_bytes());
            } else if *c >= 255 {
                out.push(255);
                out.extend_from_slice(&i16::try_from(*c).expect("code").to_le_bytes());
            } else {
                out.push(u8::try_from(*c).expect("code"));
            }
            match value_type(*c).expect("type") {
                ValueType::Str => {
                    out.extend_from_slice(v.as_bytes());
                    out.push(0);
                }
                ValueType::F64 => out.extend_from_slice(&v.parse::<f64>().expect("f64").to_le_bytes()),
                ValueType::I16 => out.extend_from_slice(&v.parse::<i16>().expect("i16").to_le_bytes()),
                ValueType::Bool if two_byte => out.push(v.parse::<u8>().expect("bool")),
                ValueType::Bool => out.extend_from_slice(&v.parse::<i16>().expect("bool").to_le_bytes()),
                ValueType::I32 => out.extend_from_slice(&v.parse::<i32>().expect("i32").to_le_bytes()),
                ValueType::I64 => out.extend_from_slice(&v.parse::<i64>().expect("i64").to_le_bytes()),
                ValueType::Binary => {
                    let bytes: Vec<u8> = (0..v.len() / 2).map(|i| u8::from_str_radix(&v[i * 2..i * 2 + 2], 16).expect("hex")).collect();
                    out.push(u8::try_from(bytes.len()).expect("len"));
                    out.extend_from_slice(&bytes);
                }
            }
        }
        out
    }

    fn run(bytes: &[u8]) -> Result<VectorProposal> {
        import_dxf(bytes, &ImportPolicy::default(), &mut SequentialIdMint::new(2), &CancellationToken::default())
    }

    /// The same drawing as ASCII (CRLF lines, a leading comment), R13+ binary and R12 binary reads
    /// to the same drawing and imports to the same paths: `\U+` escapes in names decoded, an off
    /// layer (negative colour), true colour, the block found case-insensitively, 64-bit, boolean
    /// and binary-chunk pairs skipped by type.
    #[test]
    fn binary_dxf_imports_like_ascii() {
        let pairs = sample();
        let (a, b2, b1) = (ascii(&pairs), binary(&pairs, true), binary(&pairs, false));
        assert_eq!(identify_cad(&b2), CadKind::DxfBinary);
        let token = CancellationToken::default();
        let d = read_drawing(&a, false, 1000, &token).expect("ascii read");
        assert_eq!(d.insunits, 4);
        assert_eq!(d.layers.len(), 1);
        assert_eq!(d.layers[0].name, "Teßt");
        assert!(!d.layers[0].on);
        assert_eq!(d.layers[0].colour.index(), Some(3));
        assert_eq!(d.layers[0].line_weight, 50);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].base, [1.0, 1.0, 0.0]);
        assert_eq!(d.entities.len(), 3);
        assert_eq!(d.entities[0].common.true_colour, 65280);
        assert_eq!(read_drawing(&b2, true, 1000, &token).expect("binary r13"), d);
        assert_eq!(read_drawing(&b1, true, 1000, &token).expect("binary r12"), d);
        let (pa, pb2, pb1) = (run(&a).expect("ascii"), run(&b2).expect("binary r13"), run(&b1).expect("binary r12"));
        assert_eq!(pa.provenance.source_format, "dxf_ascii");
        assert_eq!(pb2.provenance.source_format, "dxf_binary");
        assert_eq!(pa.paths.len(), 3, "line, circle, inserted block line");
        assert_eq!(pa.paths, pb2.paths);
        assert_eq!(pa.paths, pb1.paths);
    }

    /// Damaged binary input is typed: a value cut short at three places, an unknown group code, a
    /// missing sentinel; the value budget is enforced while reading.
    #[test]
    fn damaged_binary_dxf_is_typed() {
        let token = CancellationToken::default();
        let full = binary(&sample(), true);
        for cut in [BINARY_DXF_SENTINEL.len() + 1, BINARY_DXF_SENTINEL.len() + 5, full.len() - 3] {
            let r = read_drawing(&full[..cut], true, 1000, &token);
            assert!(matches!(r, Err(VectorError::MalformedDxf(_))), "cut {cut}: {r:?}");
        }
        let mut unknown = BINARY_DXF_SENTINEL.to_vec();
        unknown.extend_from_slice(&[0, 0]);
        unknown.extend_from_slice(b"SECTION\0");
        unknown.extend_from_slice(&[0xF0, 0x00, 0, 0]);
        assert_eq!(read_drawing(&unknown, true, 1000, &token).err(), Some(VectorError::MalformedDxf("group_code")));
        assert_eq!(read_drawing(b"AutoCAD Binary DXF\r\n", true, 1000, &token).err(), Some(VectorError::MalformedDxf("binary_header")));
        // A polyline with 3 vertices needs 3 values: a cap of 2 is exceeded while reading.
        let poly = [(0, "SECTION"), (2, "ENTITIES"), (0, "LWPOLYLINE"), (10, "0"), (20, "0"), (10, "1"), (20, "0"), (10, "1"), (20, "1"), (0, "ENDSEC"), (0, "EOF")];
        assert_eq!(read_drawing(&binary(&poly, true), true, 2, &token).err(), Some(VectorError::BudgetExceeded("dxf_values")));
        assert!(read_drawing(&binary(&poly, true), true, 3, &token).is_ok());
    }
}
