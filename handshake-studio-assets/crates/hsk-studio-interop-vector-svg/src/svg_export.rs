//! Deterministic SVG 1.1 export of the supported subset. Same input, same text: numbers use the
//! shortest round-trip decimal form (never exponent notation) and attributes have a fixed order.
//! Paint, stroke and appearance rows are read from Folio design records: each vector layer's
//! `StudioNodeDesign.style_refs[LAYER_STYLE_REF]` names a `PAINT` style, validated by Folio before
//! it is read. `export_proposal` also writes the Folio vector records (`records`): patterns,
//! clips and masks become `<pattern>`, `<clipPath>` and `<mask>` definitions in `<defs>`, and the
//! clip/mask attachment of a layer becomes its `clip-path` and `mask` attributes.
//!
//! Stroke markers (lane SMB-AI-ARROWHEADS-2, STU-VEC-146): `export_proposal` writes each end of a
//! Folio `stroke_markers` entry on a solid stroke row as a `marker-start` / `marker-end` reference
//! to a `<marker>` definition holding nib's marker outline (`hsk_studio_nib::markers`) in units of
//! the marker size: `markerUnits="strokeWidth"`, `orient="auto-start-reverse"`, the `viewBox` the
//! outline's box, `markerWidth` / `markerHeight` that box times scale / 100, `refX` the tip
//! (`inside_path`) or the back (`outside_path`). The recognised import shapes are written in the
//! form the import reads back (`arrow.triangle` and `arrow.simple` as nib's polygon, circles as
//! `<circle>`, squares as `<rect>`), so export then import keeps their tokens, scales and alignment
//! (test svg_markers::export_then_import_gives_the_same_markers). Typed losses: an `ERD_*`
//! terminator (`marker_shape_unbuilt`, nib's code) or any marker nib refuses, a marker on a
//! non-solid or pattern-bound row (`marker_paint_not_exported`), a collaborative-suite token whose
//! name SVG cannot hold (`marker_token_not_exported`, the shape is exact) and the inside-path stroke
//! trim SVG does not apply (`marker_stroke_not_trimmed`).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{Result, VectorError};
use crate::model::*;
use hsk_studio_accord::{CancellationToken, Length as AccordLength, Unit};
use hsk_studio_folio::{
    BLEND_NORMAL, BLEND_PASS_THROUGH, Budget, GeometryUnit, GradientInterpolation, GradientSpread,
    Length as FolioLength, Parent, Payload, ProfileBinding, Resolution, StrokeAlign, StrokeCap,
    StrokeJoin, StudioAppearanceRow, StudioColor, StudioDesignRecords, StudioDocument,
    StudioGradient, StudioGradientGeometry, StudioLayer, StudioMaskAttachment, StudioNodeDesign,
    StudioPaint, StudioPaintStyle, StudioPattern, StudioStrokeMarkers, StudioStrokeStyle, StudioVectorClip,
    StudioVectorMask, StudioVectorRecords, Unresolved, validate_paint_style,
    validate_vector_records,
};
use hsk_studio_nib::Winding;
use hsk_studio_nib::markers::{MarkerAlign, MarkerEnd, MarkerOptions, MarkerShape, MarkerSpec, marker_outlines};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

mod records;

fn num(v: f64) -> String {
    if v == 0.0 { "0".into() } else { format!("{v}") }
}

/// 8-bit channel of a `0..=1` value, and whether the value was already on the 8-bit grid.
fn channel(v: f64) -> (u8, bool) {
    let scaled = v * 255.0;
    let rounded = scaled.round();
    (
        rounded.clamp(0.0, 255.0) as u8,
        (scaled - rounded).abs() <= 1e-6,
    )
}

/// Reports the colour spaces a paint use leaves unconverted. SVG paint is sRGB and this export has
/// no way to obtain a profile (Folio's `Resolver::profile` answers availability only, no bytes),
/// so it cannot convert through Prism: the channels are written unchanged as sRGB and that is a
/// typed loss, never a silent conversion. A colour with an `Assigned` profile reports
/// `colour_profile_not_converted`; an `Unassigned` colour is in the document working space, so
/// when the document assigns an RGB working space (`working_assigned`) it reports
/// `working_space_not_converted` (the profile may be sRGB itself, which this export cannot tell).
fn note_profiles<'c>(
    colours: impl IntoIterator<Item = &'c StudioColor>,
    working_assigned: bool,
    label: &str,
    loss: &mut Vec<LossEntry>,
) {
    let (mut assigned, mut working) = (false, false);
    for c in colours {
        match c.colour_profile_id {
            ProfileBinding::Assigned(_) => assigned = true,
            ProfileBinding::Unassigned(()) => working |= working_assigned,
        }
    }
    if assigned {
        note(
            loss,
            label,
            LossKind::Unsupported,
            "colour_profile_not_converted",
        );
    }
    if working {
        note(
            loss,
            label,
            LossKind::Unsupported,
            "working_space_not_converted",
        );
    }
}

/// `#rrggbb` of a Folio colour (SVG colours are 8-bit sRGB); a value off the 8-bit grid is
/// quantized and reported. The colour-profile binding is not interpreted here: the paint writer
/// reports unconverted profiles once per paint (`note_profiles`).
fn hex(c: &StudioColor, label: &str, loss: &mut Vec<LossEntry>) -> String {
    let ((r, rx), (g, gx), (b, bx)) = (channel(c.r), channel(c.g), channel(c.b));
    if !(rx && gx && bx) {
        loss.push(LossEntry {
            path: label.to_owned(),
            kind: LossKind::Approximated,
            code: "color_quantized_8bit",
        });
    }
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Escapes text for a double-quoted XML attribute value; control characters become `_`.
fn attr_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() => out.push('_'),
            c => out.push(c),
        }
    }
    out
}

/// A Folio length in points (`ppi` resolves pixel lengths); finite and within the coordinate cap.
fn to_pt(l: &FolioLength, ppi: f64) -> Result<f64> {
    let unit = match l.unit {
        GeometryUnit::Mm => Unit::Millimetres,
        GeometryUnit::In => Unit::Inches,
        GeometryUnit::Px => Unit::Pixels,
        GeometryUnit::Pt => Unit::Points,
    };
    let v = AccordLength::new(l.value, unit)
        .and_then(|a| a.convert(Unit::Points, Some(ppi)))
        .map_err(|_| VectorError::InvalidGeometry)?
        .value();
    if v.is_finite() && v.abs() <= COORDINATE_LIMIT {
        Ok(v)
    } else {
        Err(VectorError::InvalidGeometry)
    }
}

/// Absolute control points of one anchor, in points.
struct Seg {
    pos: [f64; 2],
    cin: [f64; 2],
    cout: [f64; 2],
}

fn convert(p: &VectorPath) -> Result<Vec<Seg>> {
    if p.anchors.len() < 2 {
        return Err(VectorError::InvalidGeometry);
    }
    let value = |l: AccordLength| -> Result<f64> {
        let v = l
            .convert(Unit::Points, None)
            .map_err(|_| VectorError::InvalidGeometry)?
            .value();
        if v.is_finite() && v.abs() <= COORDINATE_LIMIT {
            Ok(v)
        } else {
            Err(VectorError::InvalidGeometry)
        }
    };
    p.anchors
        .iter()
        .map(|a| {
            let pos = [value(a.position.x)?, value(a.position.y)?];
            Ok(Seg {
                pos,
                cin: [pos[0] + value(a.incoming.x)?, pos[1] + value(a.incoming.y)?],
                cout: [pos[0] + value(a.outgoing.x)?, pos[1] + value(a.outgoing.y)?],
            })
        })
        .collect()
}

/// A segment is a cubic when either of its two handles is non-zero, a line otherwise. The
/// last-to-first segment of a closed path is written as a final `C` when curved, then `Z`.
fn path_data(p: &VectorPath, segs: &[Seg], k: f64) -> String {
    let at = |v: [f64; 2]| format!("{} {}", num(v[0] * k), num(v[1] * k));
    let mut d = format!("M{}", at(segs[0].pos));
    let n = segs.len();
    let count = if p.closed { n } else { n.saturating_sub(1) };
    for s in 0..count {
        let next = s.saturating_add(1);
        let (a, b) = (&segs[s], &segs[if next < n { next } else { 0 }]);
        let straight = a.cout == a.pos && b.cin == b.pos;
        let closing = p.closed && next == count;
        if straight {
            if !closing {
                d.push_str(&format!(" L{}", at(b.pos)));
            }
        } else {
            d.push_str(&format!(" C{} {} {}", at(a.cout), at(b.cin), at(b.pos)));
        }
    }
    if p.closed {
        d.push_str(" Z");
    }
    d
}

/// Text plus the losses the export itself had to take (SVG expresses one fill and one stroke
/// per element, so longer appearance stacks are written as duplicated path elements).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SvgExport {
    pub text: String,
    pub loss: Vec<LossEntry>,
}

fn note(loss: &mut Vec<LossEntry>, label: &str, kind: LossKind, code: &'static str) {
    loss.push(LossEntry {
        path: label.to_owned(),
        kind,
        code,
    });
}

/// Layer appearance that SVG cannot hold (Folio raster mask coverage, clip-to-base, blend modes,
/// fill opacity), reported so an export never drops it silently. Element opacity and visibility
/// are written; a mask is the Folio `StudioMask` (coverage tiles) and has no SVG counterpart.
fn appearance_losses(layer: &StudioLayer, label: &str, loss: &mut Vec<LossEntry>) {
    let Some(a) = &layer.appearance else {
        return;
    };
    if let Some(mask) = &a.mask {
        if mask.enabled {
            note(loss, label, LossKind::Unsupported, "layer_mask_not_exported");
        } else {
            note(loss, label, LossKind::Ignored, "layer_mask_disabled_not_exported");
        }
    }
    if a.clipped {
        note(
            loss,
            label,
            LossKind::Unsupported,
            "layer_clipped_to_base_not_exported",
        );
    }
    if a.blend_mode == BLEND_PASS_THROUGH && matches!(layer.payload, Payload::Group) {
        // An SVG group with opacity is always isolated.
        if a.opacity < 100.0 {
            note(
                loss,
                label,
                LossKind::Approximated,
                "group_pass_through_isolated",
            );
        }
    } else if a.blend_mode != BLEND_NORMAL {
        note(
            loss,
            label,
            LossKind::Unsupported,
            "layer_blend_mode_not_exported",
        );
    }
    if a.fill_opacity < 100.0 {
        note(
            loss,
            label,
            LossKind::Unsupported,
            "layer_fill_opacity_not_exported",
        );
    }
}

/// Gradient definitions collected while writing; identical gradients share one definition.
struct Defs {
    k: f64,
    /// The document assigns an RGB working space (always false for the flat export, which has no
    /// document); an `Unassigned` colour is then in that space and is not converted.
    working_assigned: bool,
    ids: BTreeMap<String, String>,
    text: String,
    /// Ids already taken by layer names (written as `id`) and by earlier definitions.
    taken: BTreeSet<String>,
    counters: BTreeMap<&'static str, usize>,
}

impl Defs {
    /// A definition id `<prefix><n>` that no layer name or earlier definition uses.
    fn fresh(&mut self, prefix: &'static str) -> String {
        loop {
            let n = self.counters.entry(prefix).or_insert(0);
            *n = n.saturating_add(1);
            let id = format!("{prefix}{n}");
            if self.taken.insert(id.clone()) {
                return id;
            }
        }
    }

    /// Paint attribute value of one paint (the row opacity is a separate attribute).
    fn paint(&mut self, paint: &StudioPaint, label: &str, loss: &mut Vec<LossEntry>) -> String {
        match paint {
            StudioPaint::Solid { colour } => {
                note_profiles([colour], self.working_assigned, label, loss);
                hex(colour, label, loss)
            }
            StudioPaint::Gradient { gradient } => {
                // Per use, also when the definition is shared with an earlier element.
                note_profiles(
                    gradient.stops.iter().map(|s| &s.colour),
                    self.working_assigned,
                    label,
                    loss,
                );
                if gradient.stops.iter().any(|s| s.midpoint.is_some()) {
                    note(
                        loss,
                        label,
                        LossKind::Approximated,
                        "gradient_midpoint_dropped",
                    );
                }
                if gradient.interpolation != GradientInterpolation::Classic {
                    note(
                        loss,
                        label,
                        LossKind::Approximated,
                        "gradient_interpolation_classic_assumed",
                    );
                }
                if gradient.dither {
                    note(loss, label, LossKind::Ignored, "gradient_dither_dropped");
                }
                let key = format!("{gradient:?}");
                if let Some(id) = self.ids.get(&key) {
                    return format!("url(#{id})");
                }
                let id = self.fresh("g");
                self.write_gradient(&id, gradient, label, loss);
                self.ids.insert(key, id.clone());
                format!("url(#{id})")
            }
        }
    }

    fn write_gradient(
        &mut self,
        id: &str,
        g: &StudioGradient,
        label: &str,
        loss: &mut Vec<LossEntry>,
    ) {
        let (tag, geometry) = match &g.geometry {
            StudioGradientGeometry::Linear { x1, y1, x2, y2 } => (
                "linearGradient",
                format!(
                    "x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"",
                    num(*x1),
                    num(*y1),
                    num(*x2),
                    num(*y2)
                ),
            ),
            StudioGradientGeometry::Radial {
                cx,
                cy,
                r,
                fx,
                fy,
                fr,
            } => {
                let mut geometry = format!(
                    "cx=\"{}\" cy=\"{}\" r=\"{}\" fx=\"{}\" fy=\"{}\"",
                    num(*cx),
                    num(*cy),
                    num(*r),
                    num(*fx),
                    num(*fy)
                );
                if *fr != 0.0 {
                    geometry.push_str(&format!(" fr=\"{}\"", num(*fr)));
                }
                ("radialGradient", geometry)
            }
        };
        // Gradient space -> points -> px: scaling every matrix entry by `k` applies `scale(k)`.
        let [a, b, c, d, e, f] = g.transform.map(|v| v * self.k);
        let mut open =
            format!("    <{tag} id=\"{id}\" gradientUnits=\"userSpaceOnUse\" {geometry}");
        if [a, b, c, d, e, f] != [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] {
            open.push_str(&format!(
                " gradientTransform=\"matrix({} {} {} {} {} {})\"",
                num(a),
                num(b),
                num(c),
                num(d),
                num(e),
                num(f)
            ));
        }
        match g.spread {
            GradientSpread::Pad => {}
            GradientSpread::Reflect => open.push_str(" spreadMethod=\"reflect\""),
            GradientSpread::Repeat => open.push_str(" spreadMethod=\"repeat\""),
        }
        self.text.push_str(&open);
        self.text.push_str(">\n");
        for s in &g.stops {
            self.text.push_str(&format!(
                "      <stop offset=\"{}\" stop-color=\"{}\"",
                num(s.position / 100.0),
                hex(&s.colour, label, loss)
            ));
            if s.opacity < 100.0 {
                self.text
                    .push_str(&format!(" stop-opacity=\"{}\"", num(s.opacity / 100.0)));
            }
            self.text.push_str("/>\n");
        }
        self.text.push_str(&format!("    </{tag}>\n"));
    }
}

/// One visible appearance row, borrowed from a Folio style (or the SVG default fill).
enum Row<'a> {
    Fill {
        paint: &'a StudioPaint,
        opacity: f64,
        /// Id of the `<pattern>` this row paints with; `paint` is then its fallback colour.
        pattern: Option<String>,
    },
    Stroke {
        paint: &'a StudioPaint,
        stroke: &'a StudioStrokeStyle,
        opacity: f64,
        pattern: Option<String>,
        /// The row's stroke markers (module docs).
        markers: Option<&'a StudioStrokeMarkers>,
    },
}

type FillRow<'a> = Option<(&'a StudioPaint, f64, Option<&'a str>)>;
type StrokeRow<'a> = Option<(&'a StudioPaint, &'a StudioStrokeStyle, f64, Option<&'a str>, Option<&'a StudioStrokeMarkers>)>;

fn fill_of<'r>(row: &'r Row<'_>) -> FillRow<'r> {
    match row {
        Row::Fill {
            paint,
            opacity,
            pattern,
        } => Some((*paint, *opacity, pattern.as_deref())),
        Row::Stroke { .. } => None,
    }
}

fn stroke_of<'r>(row: &'r Row<'_>) -> StrokeRow<'r> {
    match row {
        Row::Stroke {
            paint,
            stroke,
            opacity,
            pattern,
            markers,
        } => Some((*paint, *stroke, *opacity, pattern.as_deref(), *markers)),
        Row::Fill { .. } => None,
    }
}

/// Where and how one `<path>` element is written.
#[derive(Clone, Copy)]
struct Emit<'e> {
    pad: &'e str,
    d: &'e str,
    rule: Winding,
    label: &'e str,
}

/// The Folio vector records of the exported document, indexed for the writer.
struct Recs<'a> {
    clips: BTreeMap<&'a str, &'a StudioVectorClip>,
    masks: BTreeMap<&'a str, &'a StudioVectorMask>,
    patterns: BTreeMap<&'a str, &'a StudioPattern>,
    /// Pattern binding by `(style id, row)`.
    paints: BTreeMap<(&'a str, u32), &'a hsk_studio_folio::StudioPatternPaint>,
    /// Stroke markers by `(style id, row)`.
    markers: BTreeMap<(&'a str, u32), &'a StudioStrokeMarkers>,
    attach: BTreeMap<&'a str, &'a StudioMaskAttachment>,
    /// Points per document unit (matrix translations are in the document unit).
    unit_pt: f64,
}

/// A definition whose element is written after the body, in `<defs>`.
enum Pending<'a> {
    Clip(&'a StudioVectorClip, String),
    Mask(&'a StudioVectorMask, String),
    Pattern(&'a StudioPattern, String, [f64; 6]),
}

struct Writer<'a> {
    k: f64,
    /// Pixels per inch of the export, for pixel-unit Folio lengths.
    ppi: f64,
    nodes: BTreeMap<&'a str, &'a StudioNodeDesign>,
    styles: BTreeMap<&'a str, &'a StudioPaintStyle>,
    /// Styles Folio has validated for this export.
    checked: BTreeSet<&'a str>,
    defs: Defs,
    loss: Vec<LossEntry>,
    recs: Option<Recs<'a>>,
    /// Definitions referenced so far and not yet written (first referenced, first written).
    pending: VecDeque<Pending<'a>>,
    clip_ids: BTreeMap<&'a str, String>,
    /// `None`: the mask cannot be written (reported once), its layers stay unmasked.
    mask_ids: BTreeMap<&'a str, Option<String>>,
    pattern_ids: BTreeMap<(&'a str, [u64; 6]), String>,
    /// Layer names already written as `id`; definition content skips a name used before.
    emitted: BTreeSet<String>,
    in_def: bool,
    /// `<marker>` definition ids by shape, scale, alignment and paint.
    marker_ids: BTreeMap<String, String>,
}

impl<'a> Writer<'a> {
    /// A fill or stroke paint value. A row bound to a pattern writes `url(#pattern)` followed by
    /// its own solid colour as the SVG fallback.
    fn paint_value(&mut self, paint: &StudioPaint, pattern: Option<&str>, label: &str) -> String {
        let Some(id) = pattern else {
            return self.defs.paint(paint, label, &mut self.loss);
        };
        match paint {
            StudioPaint::Solid { colour } => {
                note_profiles([colour], self.defs.working_assigned, label, &mut self.loss);
                format!("url(#{id}) {}", hex(colour, label, &mut self.loss))
            }
            StudioPaint::Gradient { .. } => {
                note(
                    &mut self.loss,
                    label,
                    LossKind::Ignored,
                    "pattern_stand_in_gradient_not_exported",
                );
                format!("url(#{id})")
            }
        }
    }

    /// Paint attributes of one element: an optional fill row, an optional stroke row.
    fn attrs(
        &mut self,
        fill: FillRow<'_>,
        stroke: StrokeRow<'_>,
        rule: Winding,
        label: &str,
    ) -> Result<String> {
        let mut out = String::new();
        match fill {
            None => out.push_str(" fill=\"none\""),
            Some((paint, opacity, pattern)) => {
                let value = self.paint_value(paint, pattern, label);
                out.push_str(&format!(" fill=\"{value}\""));
                if opacity < 100.0 {
                    out.push_str(&format!(" fill-opacity=\"{}\"", num(opacity / 100.0)));
                }
                if rule == Winding::EvenOdd {
                    out.push_str(" fill-rule=\"evenodd\"");
                }
            }
        }
        if let Some((paint, st, opacity, pattern, markers)) = stroke {
            let value = self.paint_value(paint, pattern, label);
            let width = to_pt(&st.weight, self.ppi)?;
            out.push_str(&format!(
                " stroke=\"{value}\" stroke-width=\"{}\"",
                num(width * self.k)
            ));
            if opacity < 100.0 {
                out.push_str(&format!(" stroke-opacity=\"{}\"", num(opacity / 100.0)));
            }
            match st.cap {
                StrokeCap::Butt => {}
                StrokeCap::Round => out.push_str(" stroke-linecap=\"round\""),
                StrokeCap::Projecting => out.push_str(" stroke-linecap=\"square\""),
            }
            match st.join {
                StrokeJoin::Miter => {}
                StrokeJoin::Round => out.push_str(" stroke-linejoin=\"round\""),
                StrokeJoin::Bevel => out.push_str(" stroke-linejoin=\"bevel\""),
            }
            if st.miter_limit != 4.0 {
                out.push_str(&format!(" stroke-miterlimit=\"{}\"", num(st.miter_limit)));
            }
            if !st.dash.is_empty() {
                let list = st
                    .dash
                    .iter()
                    .map(|d| to_pt(d, self.ppi).map(|v| num(v * self.k)))
                    .collect::<Result<Vec<_>>>()?;
                out.push_str(&format!(" stroke-dasharray=\"{}\"", list.join(" ")));
                let offset = to_pt(&st.dash_offset, self.ppi)?;
                if offset != 0.0 {
                    out.push_str(&format!(" stroke-dashoffset=\"{}\"", num(offset * self.k)));
                }
            }
            if st.align != StrokeAlign::Center {
                note(
                    &mut self.loss,
                    label,
                    LossKind::Approximated,
                    "stroke_align_center_assumed",
                );
            }
            if let Some(m) = markers {
                let refs = self.marker_attrs(m, paint, opacity, pattern, label);
                out.push_str(&refs);
            }
        }
        Ok(out)
    }

    /// ` marker-start="url(#..)" marker-end="url(#..)"` of a stroke row's markers (module docs).
    fn marker_attrs(
        &mut self,
        m: &StudioStrokeMarkers,
        paint: &StudioPaint,
        opacity: f64,
        pattern: Option<&str>,
        label: &str,
    ) -> String {
        let mut out = String::new();
        let colour = match (paint, pattern) {
            (StudioPaint::Solid { colour }, None) => colour,
            _ => {
                note(&mut self.loss, label, LossKind::Unsupported, "marker_paint_not_exported");
                return out;
            }
        };
        let ink = hex(colour, label, &mut self.loss);
        for (token, scale, attr) in [(&m.start, m.start_scale, "marker-start"), (&m.end, m.end_scale, "marker-end")] {
            match MarkerShape::from_token(token) {
                Ok(None) => {}
                Err(e) => note(&mut self.loss, label, LossKind::Unsupported, e.code()),
                Ok(Some(shape)) => {
                    let collaborative = hsk_studio_folio::StudioArrowhead::from_token(token)
                        .is_some_and(|a| a.suite() == hsk_studio_folio::StudioArrowheadSuite::Collaborative);
                    if collaborative {
                        note(&mut self.loss, label, LossKind::Ignored, "marker_token_not_exported");
                    }
                    match self.marker_def(shape, scale, m.alignment, &ink, opacity) {
                        Some(id) => out.push_str(&format!(" {attr}=\"url(#{id})\"")),
                        None => note(&mut self.loss, label, LossKind::Unsupported, "marker_shape_not_exported"),
                    }
                }
            }
        }
        if !out.is_empty() && m.alignment == hsk_studio_folio::StudioArrowAlignment::InsidePath {
            note(&mut self.loss, label, LossKind::Approximated, "marker_stroke_not_trimmed");
        }
        out
    }

    /// Id of the `<marker>` definition of one marker end, written on first use.
    fn marker_def(
        &mut self,
        shape: MarkerShape,
        scale: f64,
        alignment: hsk_studio_folio::StudioArrowAlignment,
        ink: &str,
        opacity: f64,
    ) -> Option<String> {
        let key = format!("{}|{scale}|{alignment:?}|{ink}|{opacity}", shape.token());
        if let Some(id) = self.marker_ids.get(&key) {
            return Some(id.clone());
        }
        // Nib's outline at marker size 1: the end of a horizontal stroke of width 1 at scale 100,
        // tip at the origin pointing +x, so local (u, v) = (x, y).
        let probe = hsk_studio_nib::OwnedPath {
            elements: vec![
                hsk_studio_nib::PathElement::MoveTo(hsk_studio_nib::PathPoint::new(-10.0, 0.0)),
                hsk_studio_nib::PathElement::LineTo(hsk_studio_nib::PathPoint::new(0.0, 0.0)),
            ],
            fill_rule: hsk_studio_nib::FillRule::NonZero,
        };
        let spec = MarkerSpec { start: None, end: Some(MarkerEnd { shape, scale: 100.0 }), align: MarkerAlign::InsidePath };
        let outline = marker_outlines(&probe, &spec, 1.0, &MarkerOptions::default()).ok()?.markers.into_iter().next()?.outline;
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        let mut d = Vec::with_capacity(outline.elements.len());
        for e in &outline.elements {
            use hsk_studio_nib::PathElement as E;
            let (cmd, pts): (&str, Vec<hsk_studio_nib::PathPoint>) = match *e {
                E::MoveTo(a) => ("M", vec![a]),
                E::LineTo(a) => ("L", vec![a]),
                E::QuadTo(c, a) => ("Q", vec![c, a]),
                E::CubicTo(c1, c2, a) => ("C", vec![c1, c2, a]),
                E::Close => ("Z", Vec::new()),
            };
            for q in &pts {
                b = [b[0].min(q.x), b[1].min(q.y), b[2].max(q.x), b[3].max(q.y)];
            }
            let coords: Vec<String> = pts.iter().map(|q| format!("{} {}", num(q.x), num(q.y))).collect();
            d.push(format!("{cmd}{}", coords.join(" ")));
        }
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
            return None;
        }
        let f = scale / 100.0;
        let fill_opacity = if opacity < 100.0 { format!(" fill-opacity=\"{}\"", num(opacity / 100.0)) } else { String::new() };
        let stroke_opacity = if opacity < 100.0 { format!(" stroke-opacity=\"{}\"", num(opacity / 100.0)) } else { String::new() };
        let fill = format!(" fill=\"{ink}\"{fill_opacity}");
        let hollow = format!(" fill=\"none\" stroke=\"{ink}\" stroke-width=\"0.5\"{stroke_opacity}");
        let body = match shape {
            MarkerShape::CircleSolid => format!("<circle cx=\"-1.5\" cy=\"0\" r=\"1.5\"{fill}/>"),
            MarkerShape::Circle => format!("<circle cx=\"-1.5\" cy=\"0\" r=\"1.25\"{hollow}/>"),
            MarkerShape::SquareSolid => format!("<rect x=\"-3\" y=\"-1.5\" width=\"3\" height=\"3\"{fill}/>"),
            MarkerShape::Square => format!("<rect x=\"-2.75\" y=\"-1.25\" width=\"2.5\" height=\"2.5\"{hollow}/>"),
            _ => {
                let rule = if outline.fill_rule == hsk_studio_nib::FillRule::EvenOdd { " fill-rule=\"evenodd\"" } else { "" };
                format!("<path d=\"{}\"{fill}{rule}/>", d.join(" "))
            }
        };
        let ref_x = match alignment {
            hsk_studio_folio::StudioArrowAlignment::InsidePath => 0.0,
            hsk_studio_folio::StudioArrowAlignment::OutsidePath => -shape.length(),
        };
        let id = self.defs.fresh("m");
        self.defs.text.push_str(&format!(
            "    <marker id=\"{id}\" viewBox=\"{} {} {} {}\" refX=\"{}\" refY=\"0\" markerWidth=\"{}\" markerHeight=\"{}\" markerUnits=\"strokeWidth\" orient=\"auto-start-reverse\">\n      {body}\n    </marker>\n",
            num(b[0]),
            num(b[1]),
            num(w),
            num(h),
            num(ref_x),
            num(w * f),
            num(h * f),
        ));
        self.marker_ids.insert(key, id.clone());
        Some(id)
    }

    fn element(
        &mut self,
        out: &mut String,
        at: Emit<'_>,
        rows: (FillRow<'_>, StrokeRow<'_>),
        tail: &str,
    ) -> Result<()> {
        let attrs = self.attrs(rows.0, rows.1, at.rule, at.label)?;
        let (pad, d) = (at.pad, at.d);
        out.push_str(&format!("{pad}<path d=\"{d}\"{attrs}{tail}/>\n"));
        Ok(())
    }

    /// The style a layer names, validated by Folio on first use. A reference to a style the
    /// record set does not hold is reported and the layer falls back to the SVG default.
    fn style_of(&mut self, layer_id: &str, label: &str) -> Result<Option<&'a StudioPaintStyle>> {
        let Some(&node) = self.nodes.get(layer_id) else {
            return Ok(None);
        };
        if node.style_refs.keys().any(|k| k != LAYER_STYLE_REF) {
            note(
                &mut self.loss,
                label,
                LossKind::Ignored,
                "style_ref_not_exported",
            );
        }
        let Some(id) = node.style_refs.get(LAYER_STYLE_REF) else {
            return Ok(None);
        };
        let Some(&style) = self.styles.get(id.as_str()) else {
            note(
                &mut self.loss,
                label,
                LossKind::Unsupported,
                "style_ref_unresolved",
            );
            return Ok(None);
        };
        if self.checked.insert(style.style_id.as_str()) {
            let issues = validate_paint_style(
                style,
                Budget::default(),
                &CancellationToken::default(),
                &Unresolved,
            )
            .map_err(|d| VectorError::Folio(d.code))?;
            // Folio's resolution issues (an assigned colour profile this export cannot resolve)
            // are losses of this export, one per distinct profile of the style.
            for issue in issues {
                let code = match issue.disposition {
                    Resolution::Available => continue,
                    Resolution::Absent => "colour_profile_unresolved",
                    Resolution::HashMismatch => "colour_profile_hash_mismatch",
                    Resolution::Unauthorized => "colour_profile_unauthorized",
                    Resolution::Unsupported => "colour_profile_unsupported",
                };
                note(&mut self.loss, label, LossKind::Unsupported, code);
            }
        }
        Ok(Some(style))
    }

    /// All subpaths of one layer as `<path>` elements (the first path carries style and winding).
    /// One fill and one stroke (either order) fit one element; any other stack is written as one
    /// element per row, wrapped in a `<g>` that carries `extra`, and reported.
    fn layer_paths(
        &mut self,
        out: &mut String,
        depth: usize,
        group: &[(&VectorPath, &[Seg])],
        extra: &str,
        label: &str,
    ) -> Result<()> {
        let first = group[0].0;
        let rule = first.winding;
        let d = group
            .iter()
            .map(|(p, segs)| path_data(p, segs, self.k))
            .collect::<Vec<_>>()
            .join(" ");
        let style = self.style_of(&first.layer_id, label)?;
        let filled = rule != Winding::None;
        let default_fill = StudioPaint::Solid {
            colour: StudioColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                colour_profile_id: hsk_studio_folio::ProfileBinding::Unassigned(()),
            },
        };
        let mut rows: Vec<Row> = Vec::new();
        let mut dropped_fill = false;
        if let Some(style) = style {
            for (index, row) in style.rows.iter().enumerate() {
                let (visible, blend_mode) = match row {
                    StudioAppearanceRow::Fill {
                        visible,
                        blend_mode,
                        ..
                    }
                    | StudioAppearanceRow::Stroke {
                        visible,
                        blend_mode,
                        ..
                    } => (*visible, *blend_mode),
                };
                if !visible {
                    note(
                        &mut self.loss,
                        label,
                        LossKind::Ignored,
                        "appearance_row_hidden_dropped",
                    );
                    continue;
                }
                if blend_mode != BLEND_NORMAL {
                    note(
                        &mut self.loss,
                        label,
                        LossKind::Unsupported,
                        "appearance_row_blend_mode_dropped",
                    );
                }
                match row {
                    StudioAppearanceRow::Fill { paint, opacity, .. } if filled => {
                        let pattern = self.ref_pattern(&style.style_id, index, label);
                        rows.push(Row::Fill {
                            paint,
                            opacity: *opacity,
                            pattern,
                        });
                    }
                    StudioAppearanceRow::Fill { .. } => dropped_fill = true,
                    StudioAppearanceRow::Stroke {
                        paint,
                        stroke,
                        opacity,
                        ..
                    } => {
                        let pattern = self.ref_pattern(&style.style_id, index, label);
                        let markers = u32::try_from(index).ok().and_then(|row| {
                            self.recs.as_ref().and_then(|r| r.markers.get(&(style.style_id.as_str(), row)).copied())
                        });
                        rows.push(Row::Stroke {
                            paint,
                            stroke,
                            opacity: *opacity,
                            pattern,
                            markers,
                        });
                    }
                }
            }
        } else if filled {
            // No style: the SVG default fill.
            rows.push(Row::Fill {
                paint: &default_fill,
                opacity: 100.0,
                pattern: None,
            });
        }
        if dropped_fill {
            note(
                &mut self.loss,
                label,
                LossKind::Ignored,
                "fill_ignored_winding_none",
            );
        }
        let pad = "  ".repeat(depth);
        let at = Emit {
            pad: &pad,
            d: &d,
            rule,
            label,
        };
        match rows.as_slice() {
            [] => self.element(out, at, (None, None), extra)?,
            [f @ Row::Fill { .. }] => self.element(out, at, (fill_of(f), None), extra)?,
            [s @ Row::Stroke { .. }] => self.element(out, at, (None, stroke_of(s)), extra)?,
            [f @ Row::Fill { .. }, s @ Row::Stroke { .. }] => {
                self.element(out, at, (fill_of(f), stroke_of(s)), extra)?;
            }
            [s @ Row::Stroke { .. }, f @ Row::Fill { .. }] => {
                let tail = format!(" paint-order=\"stroke\"{extra}");
                self.element(out, at, (fill_of(f), stroke_of(s)), &tail)?;
            }
            _ => {
                note(
                    &mut self.loss,
                    label,
                    LossKind::Approximated,
                    "appearance_stack_duplicated",
                );
                let wrap = !extra.is_empty();
                let inner = if wrap {
                    out.push_str(&format!("{pad}<g{extra}>\n"));
                    format!("{pad}  ")
                } else {
                    pad.clone()
                };
                let at = Emit { pad: &inner, ..at };
                for row in &rows {
                    self.element(out, at, (fill_of(row), stroke_of(row)), "")?;
                }
                if wrap {
                    out.push_str(&format!("{pad}</g>\n"));
                }
            }
        }
        Ok(())
    }
}

fn header(opts: &ExportOptions, content_pt: [f64; 2]) -> Result<(f64, String)> {
    if !opts.pixels_per_inch.is_finite() || opts.pixels_per_inch <= 0.0 {
        return Err(VectorError::InvalidPolicy("pixels_per_inch"));
    }
    let k = opts.pixels_per_inch / 72.0;
    let [w_pt, h_pt] = match opts.artboard_pt {
        Some([w, h]) if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 => [w, h],
        Some(_) => return Err(VectorError::InvalidPolicy("artboard_pt")),
        None => [content_pt[0].max(1.0), content_pt[1].max(1.0)],
    };
    let (w, h) = (num(w_pt * k), num(h_pt * k));
    Ok((
        k,
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}px\" height=\"{h}px\" viewBox=\"0 0 {w} {h}\">\n"
        ),
    ))
}

fn extent<'a>(converted: impl Iterator<Item = &'a Vec<Seg>>) -> [f64; 2] {
    let (mut x, mut y) = (0.0_f64, 0.0_f64);
    for segs in converted {
        for s in segs {
            for v in [s.pos, s.cin, s.cout] {
                x = x.max(v[0]);
                y = y.max(v[1]);
            }
        }
    }
    [x, y]
}

fn finish(open: String, writer: Writer<'_>, body: &str) -> SvgExport {
    let mut text = open;
    if !writer.defs.text.is_empty() {
        text.push_str("  <defs>\n");
        text.push_str(&writer.defs.text);
        text.push_str("  </defs>\n");
    }
    text.push_str(body);
    text.push_str("</svg>\n");
    SvgExport {
        text,
        loss: writer.loss,
    }
}

fn writer(
    k: f64,
    ppi: f64,
    design: &StudioDesignRecords,
    working_assigned: bool,
) -> Writer<'_> {
    Writer {
        k,
        ppi,
        nodes: design
            .nodes
            .iter()
            .map(|n| (n.node.node_id.as_str(), n))
            .collect(),
        styles: design
            .paint_styles
            .iter()
            .map(|s| (s.style_id.as_str(), s))
            .collect(),
        checked: BTreeSet::new(),
        defs: Defs {
            k,
            working_assigned,
            ids: BTreeMap::new(),
            text: String::new(),
            taken: BTreeSet::new(),
            counters: BTreeMap::new(),
        },
        loss: Vec::new(),
        recs: None,
        pending: VecDeque::new(),
        clip_ids: BTreeMap::new(),
        mask_ids: BTreeMap::new(),
        pattern_ids: BTreeMap::new(),
        emitted: BTreeSet::new(),
        in_def: false,
        marker_ids: BTreeMap::new(),
    }
}

/// The layers of an exported document with the geometry of their paths, for the writer.
struct Tree<'a> {
    doc: &'a StudioDocument,
    paths: &'a [VectorPath],
    segs: Vec<Vec<Seg>>,
    /// Indices into `paths` per layer id.
    by_layer: BTreeMap<&'a str, Vec<usize>>,
    /// Indices into `doc.layers` per parent layer id (`None`: artboard or root), document order.
    children: BTreeMap<Option<&'a str>, Vec<usize>>,
    /// Layer id -> index into `doc.layers`.
    index: BTreeMap<&'a str, usize>,
}

impl<'a> Tree<'a> {
    fn new(doc: &'a StudioDocument, paths: &'a [VectorPath]) -> Result<Self> {
        let segs = paths.iter().map(convert).collect::<Result<Vec<_>>>()?;
        let mut by_layer: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, p) in paths.iter().enumerate() {
            by_layer.entry(p.layer_id.as_str()).or_default().push(i);
        }
        let mut children: BTreeMap<Option<&str>, Vec<usize>> = BTreeMap::new();
        let mut index = BTreeMap::new();
        for (i, layer) in doc.layers.iter().enumerate() {
            let parent = match &layer.parent {
                Parent::Node(n) if n.node_kind == hsk_studio_folio::NodeKind::Layer => {
                    Some(n.node_id.as_str())
                }
                _ => None,
            };
            children.entry(parent).or_default().push(i);
            index.insert(layer.layer_id.as_str(), i);
        }
        Ok(Self {
            doc,
            paths,
            segs,
            by_layer,
            children,
            index,
        })
    }
}

/// Flat export: one element per layer (consecutive paths sharing a `layer_id` form one compound
/// `d`), absolute `M`/`L`/`C`/`Z` data, gradients in `<defs>`, `viewBox="0 0 W H"` in px. Paint,
/// stroke and appearance rows come from the Folio `design` records. Group structure, names,
/// visibility and element opacity live in the Folio document; use [`export_proposal`] to keep
/// them. This function has no document and no vector records: the paths of a proposal that holds
/// patterns, clips or masks include the hidden geometry of their source layers, which a flat
/// export draws as ordinary paths; export such a proposal with [`export_proposal`].
pub fn export_svg(
    paths: &[VectorPath],
    design: &StudioDesignRecords,
    opts: &ExportOptions,
) -> Result<String> {
    export_svg_with_loss(paths, design, opts).map(|e| e.text)
}

/// [`export_svg`] plus the losses the export took.
pub fn export_svg_with_loss(
    paths: &[VectorPath],
    design: &StudioDesignRecords,
    opts: &ExportOptions,
) -> Result<SvgExport> {
    let converted = paths.iter().map(convert).collect::<Result<Vec<_>>>()?;
    let (k, open) = header(opts, extent(converted.iter()))?;
    let mut w = writer(k, opts.pixels_per_inch, design, false);
    let mut body = String::new();
    let mut items = paths.iter().zip(converted.iter()).peekable();
    let mut ordinal = 0usize;
    while let Some(first) = items.next() {
        let layer = &first.0.layer_id;
        let mut group: Vec<(&VectorPath, &[Seg])> = vec![(first.0, first.1.as_slice())];
        while let Some(next) = items.next_if(|(p, _)| &p.layer_id == layer) {
            group.push((next.0, next.1.as_slice()));
        }
        ordinal = ordinal.saturating_add(1);
        w.layer_paths(&mut body, 1, &group, "", &format!("/layer[{ordinal}]"))?;
    }
    Ok(finish(open, w, &body))
}

/// Hierarchical export of a proposal (or an edited copy of its `paths` and `design`): the Folio
/// layers become `<g>` and `<path>` elements in document order, with `id` (when the layer name
/// differs from the element name), `opacity` from the layer appearance and `display="none"` for
/// hidden layers. Only paths whose `layer_id` names a vector layer of the snapshot are written.
/// The artboard size comes from the first artboard unless `opts.artboard_pt` overrides it.
pub fn export_proposal(proposal: &VectorProposal, opts: &ExportOptions) -> Result<String> {
    export_proposal_with_loss(proposal, opts).map(|e| e.text)
}

/// [`export_proposal`] plus the losses the export took.
///
/// The vector records of the proposal are validated by Folio first (a record set that does not
/// match the document, a dangling reference or a reference loop is `VectorError::Folio`). Each
/// pattern, clip and mask a layer or paint row uses is written once to `<defs>` as
/// `<pattern>`, `<clipPath>` and `<mask>` with `userSpaceOnUse` units in the export's pixel
/// space; the hidden source layers of those records are not written as elements of their own.
pub fn export_proposal_with_loss(
    proposal: &VectorProposal,
    opts: &ExportOptions,
) -> Result<SvgExport> {
    let doc = proposal.snapshot.document();
    let records = &proposal.vector_records;
    let record_count = records
        .patterns
        .len()
        .saturating_add(records.pattern_paints.len())
        .saturating_add(records.clips.len())
        .saturating_add(records.masks.len())
        .saturating_add(records.attachments.len())
        .saturating_add(records.stroke_markers.len());
    if record_count > 0 {
        let default = Budget::default();
        let budget = Budget {
            nodes: record_count.saturating_add(8).max(default.nodes),
            ..default
        };
        validate_vector_records(
            records,
            doc,
            Some(&proposal.design),
            budget,
            &CancellationToken::default(),
        )
        .map_err(|d| VectorError::Folio(d.code))?;
    }
    let tree = Tree::new(doc, &proposal.paths)?;
    let artboard_pt = doc.artboards.first().and_then(|a| {
        let pt = |l: &hsk_studio_folio::Length| (l.unit == GeometryUnit::Pt).then_some(l.value);
        match (pt(&a.bounds.width), pt(&a.bounds.height)) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some([w, h]),
            _ => None,
        }
    });
    let opts = ExportOptions {
        artboard_pt: opts.artboard_pt.or(artboard_pt),
        ..*opts
    };
    let (k, open) = header(&opts, extent(tree.segs.iter()))?;
    let working_assigned = matches!(doc.working_space_rgb, ProfileBinding::Assigned(_));
    let mut w = writer(k, opts.pixels_per_inch, &proposal.design, working_assigned);
    w.defs.taken = doc.layers.iter().map(|l| l.name.clone()).collect();
    let mut skip = BTreeSet::new();
    if record_count > 0 {
        let unit = FolioLength {
            value: 1.0,
            unit: doc.geometry_unit,
        };
        w.recs = Some(records::recs_of(records, to_pt(&unit, opts.pixels_per_inch)?));
        skip = records::definition_roots(&tree, records);
    }
    let mut body = String::new();
    let roots = tree.children.get(&None).cloned().unwrap_or_default();
    w.write_layers(&tree, &roots, 1, None, &skip, &mut body)?;
    w.drain_defs(&tree, &skip)?;
    Ok(finish(open, w, &body))
}

