//! Deterministic SVG 1.1 export of the supported subset. Same input, same text: numbers use the
//! shortest round-trip decimal form (never exponent notation) and attributes have a fixed order.
use crate::error::{Result, VectorError};
use crate::model::*;
use hsk_studio_accord::{Length as AccordLength, Unit};
use hsk_studio_folio::{GeometryUnit, Parent, Payload};
use hsk_studio_nib::Winding;
use std::collections::BTreeMap;

fn num(v: f64) -> String {
    if v == 0.0 {
        "0".into()
    } else {
        format!("{v}")
    }
}

fn hex(c: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
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

/// Gradient definitions collected while writing; identical gradients share one definition.
struct Defs {
    k: f64,
    ids: BTreeMap<String, String>,
    text: String,
}

impl Defs {
    /// Paint attribute value and the separate opacity attribute a solid colour needs.
    fn paint(&mut self, paint: &Paint) -> (String, Option<f64>) {
        match paint {
            Paint::Solid(c) => (hex(*c), (c.a < 1.0).then_some(c.a)),
            Paint::Linear(_) | Paint::Radial(_) => {
                let key = format!("{paint:?}");
                if let Some(id) = self.ids.get(&key) {
                    return (format!("url(#{id})"), None);
                }
                let id = format!("g{}", self.ids.len() + 1);
                self.write_gradient(&id, paint);
                self.ids.insert(key, id.clone());
                (format!("url(#{id})"), None)
            }
        }
    }

    fn write_gradient(&mut self, id: &str, paint: &Paint) {
        let (tag, geometry, transform, spread, stops) = match paint {
            Paint::Linear(g) => (
                "linearGradient",
                format!(
                    "x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"",
                    num(g.x1),
                    num(g.y1),
                    num(g.x2),
                    num(g.y2)
                ),
                g.transform,
                g.spread,
                &g.stops,
            ),
            Paint::Radial(g) => {
                let mut geometry = format!(
                    "cx=\"{}\" cy=\"{}\" r=\"{}\" fx=\"{}\" fy=\"{}\"",
                    num(g.cx),
                    num(g.cy),
                    num(g.r),
                    num(g.fx),
                    num(g.fy)
                );
                if g.fr != 0.0 {
                    geometry.push_str(&format!(" fr=\"{}\"", num(g.fr)));
                }
                ("radialGradient", geometry, g.transform, g.spread, &g.stops)
            }
            Paint::Solid(_) => return,
        };
        // Gradient space -> points -> px: scaling every matrix entry by `k` applies `scale(k)`.
        let [a, b, c, d, e, f] = transform.map(|v| v * self.k);
        let mut open = format!("    <{tag} id=\"{id}\" gradientUnits=\"userSpaceOnUse\" {geometry}");
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
        match spread {
            SpreadMethod::Pad => {}
            SpreadMethod::Reflect => open.push_str(" spreadMethod=\"reflect\""),
            SpreadMethod::Repeat => open.push_str(" spreadMethod=\"repeat\""),
        }
        self.text.push_str(&open);
        self.text.push_str(">\n");
        for s in stops {
            self.text.push_str(&format!(
                "      <stop offset=\"{}\" stop-color=\"{}\"",
                num(s.offset),
                hex(s.color)
            ));
            if s.color.a < 1.0 {
                self.text
                    .push_str(&format!(" stop-opacity=\"{}\"", num(s.color.a)));
            }
            self.text.push_str("/>\n");
        }
        self.text.push_str(&format!("    </{tag}>\n"));
    }
}

enum Row<'a> {
    Fill(&'a Paint),
    Stroke(&'a StrokeAppearance),
}

const DEFAULT_FILL: Paint = Paint::Solid(Rgba {
    r: 0,
    g: 0,
    b: 0,
    a: 1.0,
});

struct Writer<'a> {
    k: f64,
    styles: BTreeMap<&'a str, &'a StyleProposal>,
    defs: Defs,
    loss: Vec<LossEntry>,
}

impl Writer<'_> {
    /// Paint attributes of one element: an optional fill row, an optional stroke row.
    fn attrs(
        &mut self,
        fill: Option<&Paint>,
        stroke: Option<&StrokeAppearance>,
        rule: Winding,
    ) -> String {
        let mut out = String::new();
        match fill {
            None => out.push_str(" fill=\"none\""),
            Some(paint) => {
                let (value, opacity) = self.defs.paint(paint);
                out.push_str(&format!(" fill=\"{value}\""));
                if let Some(a) = opacity {
                    out.push_str(&format!(" fill-opacity=\"{}\"", num(a)));
                }
                if rule == Winding::EvenOdd {
                    out.push_str(" fill-rule=\"evenodd\"");
                }
            }
        }
        if let Some(s) = stroke {
            let (value, opacity) = self.defs.paint(&s.paint);
            let st = &s.style;
            out.push_str(&format!(
                " stroke=\"{value}\" stroke-width=\"{}\"",
                num(st.width_pt * self.k)
            ));
            if let Some(a) = opacity {
                out.push_str(&format!(" stroke-opacity=\"{}\"", num(a)));
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
            if !st.dash_pt.is_empty() {
                let list: Vec<String> = st.dash_pt.iter().map(|d| num(d * self.k)).collect();
                out.push_str(&format!(" stroke-dasharray=\"{}\"", list.join(" ")));
                if st.dash_offset_pt != 0.0 {
                    out.push_str(&format!(
                        " stroke-dashoffset=\"{}\"",
                        num(st.dash_offset_pt * self.k)
                    ));
                }
            }
        }
        out
    }

    fn element(
        &mut self,
        out: &mut String,
        pad: &str,
        d: &str,
        rule: Winding,
        rows: (Option<&Paint>, Option<&StrokeAppearance>),
        tail: &str,
    ) {
        let attrs = self.attrs(rows.0, rows.1, rule);
        out.push_str(&format!("{pad}<path d=\"{d}\"{attrs}{tail}/>\n"));
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
    ) {
        let first = group[0].0;
        let rule = first.winding;
        let d = group
            .iter()
            .map(|(p, segs)| path_data(p, segs, self.k))
            .collect::<Vec<_>>()
            .join(" ");
        let style = first
            .fill_style_id
            .as_deref()
            .and_then(|id| self.styles.get(id).copied());
        let filled = rule != Winding::None;
        let mut rows: Vec<Row> = Vec::new();
        let mut dropped_fill = false;
        if let Some(style) = style {
            for item in &style.items {
                match item {
                    AppearanceItem::Fill(p) if filled => rows.push(Row::Fill(p)),
                    AppearanceItem::Fill(_) => dropped_fill = true,
                    AppearanceItem::Stroke(s) => rows.push(Row::Stroke(s)),
                }
            }
        } else if filled {
            // No style: the SVG default fill.
            rows.push(Row::Fill(&DEFAULT_FILL));
        }
        if dropped_fill {
            self.loss.push(LossEntry {
                path: label.to_owned(),
                kind: LossKind::Ignored,
                code: "fill_ignored_winding_none",
            });
        }
        let pad = "  ".repeat(depth);
        match rows.as_slice() {
            [] => self.element(out, &pad, &d, rule, (None, None), extra),
            [Row::Fill(f)] => self.element(out, &pad, &d, rule, (Some(f), None), extra),
            [Row::Stroke(s)] => self.element(out, &pad, &d, rule, (None, Some(s)), extra),
            [Row::Fill(f), Row::Stroke(s)] => {
                self.element(out, &pad, &d, rule, (Some(f), Some(s)), extra);
            }
            [Row::Stroke(s), Row::Fill(f)] => {
                let tail = format!(" paint-order=\"stroke\"{extra}");
                self.element(out, &pad, &d, rule, (Some(f), Some(s)), &tail);
            }
            _ => {
                self.loss.push(LossEntry {
                    path: label.to_owned(),
                    kind: LossKind::Approximated,
                    code: "appearance_stack_duplicated",
                });
                let wrap = !extra.is_empty();
                let inner = if wrap {
                    out.push_str(&format!("{pad}<g{extra}>\n"));
                    format!("{pad}  ")
                } else {
                    pad.clone()
                };
                for row in &rows {
                    match row {
                        Row::Fill(f) => self.element(out, &inner, &d, rule, (Some(f), None), ""),
                        Row::Stroke(s) => self.element(out, &inner, &d, rule, (None, Some(s)), ""),
                    }
                }
                if wrap {
                    out.push_str(&format!("{pad}</g>\n"));
                }
            }
        }
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

fn writer(k: f64, styles: &[StyleProposal]) -> Writer<'_> {
    Writer {
        k,
        styles: styles.iter().map(|s| (s.style_id.as_str(), s)).collect(),
        defs: Defs {
            k,
            ids: BTreeMap::new(),
            text: String::new(),
        },
        loss: Vec::new(),
    }
}

/// Flat export: one element per layer (consecutive paths sharing a `layer_id` form one compound
/// `d`), absolute `M`/`L`/`C`/`Z` data, gradients in `<defs>`, `viewBox="0 0 W H"` in px.
/// Group structure, names, visibility and element opacity live in the Folio document; use
/// [`export_proposal`] to keep them.
pub fn export_svg(
    paths: &[VectorPath],
    styles: &[StyleProposal],
    opts: &ExportOptions,
) -> Result<String> {
    export_svg_with_loss(paths, styles, opts).map(|e| e.text)
}

/// [`export_svg`] plus the losses the export took.
pub fn export_svg_with_loss(
    paths: &[VectorPath],
    styles: &[StyleProposal],
    opts: &ExportOptions,
) -> Result<SvgExport> {
    let converted = paths.iter().map(convert).collect::<Result<Vec<_>>>()?;
    let (k, open) = header(opts, extent(converted.iter()))?;
    let mut w = writer(k, styles);
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
        w.layer_paths(&mut body, 1, &group, "", &format!("/layer[{ordinal}]"));
    }
    Ok(finish(open, w, &body))
}

/// Hierarchical export of a proposal (or an edited copy of its `paths` and `styles`): the Folio
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
    let mut w = writer(k, &proposal.styles);
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
                    w.layer_paths(&mut body, depth, &group, &extra, &label);
                }
            }
            _ => {}
        }
    }
    Ok(finish(open, w, &body))
}
