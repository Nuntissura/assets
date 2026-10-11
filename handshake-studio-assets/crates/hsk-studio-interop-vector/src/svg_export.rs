//! Deterministic SVG 1.1 export of the supported subset. Same input, same text: numbers use the
//! shortest round-trip decimal form (never exponent notation) and attributes have a fixed order.
//! Paint, stroke and appearance rows are read from Folio design records: each vector layer's
//! `StudioNodeDesign.style_refs[LAYER_STYLE_REF]` names a `PAINT` style, validated by Folio before
//! it is read.
use crate::error::{Result, VectorError};
use crate::model::*;
use hsk_studio_accord::{CancellationToken, Length as AccordLength, Unit};
use hsk_studio_folio::{
    BLEND_NORMAL, Budget, GeometryUnit, GradientInterpolation, GradientSpread,
    Length as FolioLength, Parent, Payload, StrokeAlign, StrokeCap, StrokeJoin,
    StudioAppearanceRow, StudioColor, StudioDesignRecords, StudioGradient, StudioGradientGeometry,
    StudioNodeDesign, StudioPaint, StudioPaintStyle, StudioStrokeStyle, Unresolved,
    validate_paint_style,
};
use hsk_studio_nib::Winding;
use std::collections::{BTreeMap, BTreeSet};

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

/// `#rrggbb` of a Folio colour (SVG colours are 8-bit sRGB); a value off the 8-bit grid is
/// quantized and reported. The colour-profile binding is not interpreted.
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
    let count = if p.closed { n } else { n - 1 };
    for s in 0..count {
        let (a, b) = (&segs[s], &segs[(s + 1) % n]);
        let straight = a.cout == a.pos && b.cin == b.pos;
        let closing = p.closed && s + 1 == count;
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

/// Gradient definitions collected while writing; identical gradients share one definition.
struct Defs {
    k: f64,
    ids: BTreeMap<String, String>,
    text: String,
}

impl Defs {
    /// Paint attribute value of one paint (the row opacity is a separate attribute).
    fn paint(&mut self, paint: &StudioPaint, label: &str, loss: &mut Vec<LossEntry>) -> String {
        match paint {
            StudioPaint::Solid { colour } => hex(colour, label, loss),
            StudioPaint::Gradient { gradient } => {
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
                let id = format!("g{}", self.ids.len() + 1);
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
    },
    Stroke {
        paint: &'a StudioPaint,
        stroke: &'a StudioStrokeStyle,
        opacity: f64,
    },
}

type FillRow<'a> = Option<(&'a StudioPaint, f64)>;
type StrokeRow<'a> = Option<(&'a StudioPaint, &'a StudioStrokeStyle, f64)>;

fn fill_of<'r>(row: &Row<'r>) -> FillRow<'r> {
    match row {
        Row::Fill { paint, opacity } => Some((*paint, *opacity)),
        Row::Stroke { .. } => None,
    }
}

fn stroke_of<'r>(row: &Row<'r>) -> StrokeRow<'r> {
    match row {
        Row::Stroke {
            paint,
            stroke,
            opacity,
        } => Some((*paint, *stroke, *opacity)),
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
}

impl<'a> Writer<'a> {
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
            Some((paint, opacity)) => {
                let value = self.defs.paint(paint, label, &mut self.loss);
                out.push_str(&format!(" fill=\"{value}\""));
                if opacity < 100.0 {
                    out.push_str(&format!(" fill-opacity=\"{}\"", num(opacity / 100.0)));
                }
                if rule == Winding::EvenOdd {
                    out.push_str(" fill-rule=\"evenodd\"");
                }
            }
        }
        if let Some((paint, st, opacity)) = stroke {
            let value = self.defs.paint(paint, label, &mut self.loss);
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
        }
        Ok(out)
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
            validate_paint_style(
                style,
                Budget::default(),
                &CancellationToken::default(),
                &Unresolved,
            )
            .map_err(|d| VectorError::Folio(d.code))?;
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
        group: &[(&VectorPath, Vec<Seg>)],
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
            for row in &style.rows {
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
                        rows.push(Row::Fill {
                            paint,
                            opacity: *opacity,
                        });
                    }
                    StudioAppearanceRow::Fill { .. } => dropped_fill = true,
                    StudioAppearanceRow::Stroke {
                        paint,
                        stroke,
                        opacity,
                        ..
                    } => rows.push(Row::Stroke {
                        paint,
                        stroke,
                        opacity: *opacity,
                    }),
                }
            }
        } else if filled {
            // No style: the SVG default fill.
            rows.push(Row::Fill {
                paint: &default_fill,
                opacity: 100.0,
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

fn writer(k: f64, ppi: f64, design: &StudioDesignRecords) -> Writer<'_> {
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
            ids: BTreeMap::new(),
            text: String::new(),
        },
        loss: Vec::new(),
    }
}

/// Flat export: one element per layer (consecutive paths sharing a `layer_id` form one compound
/// `d`), absolute `M`/`L`/`C`/`Z` data, gradients in `<defs>`, `viewBox="0 0 W H"` in px. Paint,
/// stroke and appearance rows come from the Folio `design` records. Group structure, names,
/// visibility and element opacity live in the Folio document; use [`export_proposal`] to keep them.
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
    let mut w = writer(k, opts.pixels_per_inch, design);
    let mut body = String::new();
    let mut items = paths.iter().zip(converted).peekable();
    let mut ordinal = 0usize;
    while let Some(first) = items.next() {
        let layer = &first.0.layer_id;
        let mut group = vec![first];
        while let Some(next) = items.next_if(|(p, _)| &p.layer_id == layer) {
            group.push(next);
        }
        ordinal += 1;
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
pub fn export_proposal_with_loss(
    proposal: &VectorProposal,
    opts: &ExportOptions,
) -> Result<SvgExport> {
    let doc = proposal.snapshot.document();
    let mut by_layer: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, p) in proposal.paths.iter().enumerate() {
        by_layer.entry(p.layer_id.as_str()).or_default().push(i);
    }
    let mut converted: Vec<Option<Vec<Seg>>> = Vec::with_capacity(proposal.paths.len());
    for p in &proposal.paths {
        converted.push(Some(convert(p)?));
    }
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
    let (k, open) = header(&opts, extent(converted.iter().flatten()))?;
    let mut w = writer(k, opts.pixels_per_inch, &proposal.design);
    let mut body = String::new();
    // Children per parent layer id (None = artboard or root), in document order.
    let mut children: BTreeMap<Option<&str>, Vec<usize>> = BTreeMap::new();
    for (i, layer) in doc.layers.iter().enumerate() {
        let parent = match &layer.parent {
            Parent::Node(n) if n.node_kind == hsk_studio_folio::NodeKind::Layer => {
                Some(n.node_id.as_str())
            }
            _ => None,
        };
        children.entry(parent).or_default().push(i);
    }
    // Iterative walk (no recursion): (layer index, entering).
    let mut stack: Vec<(usize, bool)> = children
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|&i| (i, true))
        .collect();
    let mut depth = 1usize;
    while let Some((i, entering)) = stack.pop() {
        let layer = &doc.layers[i];
        if !entering {
            depth -= 1;
            body.push_str(&"  ".repeat(depth));
            body.push_str("</g>\n");
            continue;
        }
        let tag = if matches!(layer.payload, Payload::Group) {
            "g"
        } else {
            "path"
        };
        let mut extra = String::new();
        if layer.name != tag && !layer.name.is_empty() {
            extra.push_str(&format!(" id=\"{}\"", attr_text(&layer.name)));
        }
        if let Some(a) = &layer.appearance
            && a.opacity < 100.0
        {
            extra.push_str(&format!(" opacity=\"{}\"", num(a.opacity / 100.0)));
        }
        if !layer.visible {
            extra.push_str(" display=\"none\"");
        }
        match &layer.payload {
            Payload::Group => {
                body.push_str(&"  ".repeat(depth));
                body.push_str(&format!("<g{extra}>\n"));
                depth += 1;
                stack.push((i, false));
                for &c in children
                    .get(&Some(layer.layer_id.as_str()))
                    .into_iter()
                    .flatten()
                    .rev()
                {
                    stack.push((c, true));
                }
            }
            Payload::Vector { .. } => {
                let Some(indices) = by_layer.get(layer.layer_id.as_str()) else {
                    continue;
                };
                let group: Vec<(&VectorPath, Vec<Seg>)> = indices
                    .iter()
                    .filter_map(|&j| converted[j].take().map(|segs| (&proposal.paths[j], segs)))
                    .collect();
                if !group.is_empty() {
                    let label = format!("/layer[{}]", i + 1);
                    w.layer_paths(&mut body, depth, &group, &extra, &label)?;
                }
            }
            _ => {}
        }
    }
    Ok(finish(open, w, &body))
}
