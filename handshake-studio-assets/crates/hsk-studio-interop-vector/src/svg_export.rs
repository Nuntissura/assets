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

struct Writer<'a> {
    k: f64,
    styles: BTreeMap<&'a str, &'a StyleProposal>,
}

impl Writer<'_> {
    /// One `<path>` element for all subpaths of one layer (the first path carries the style).
    fn path_element(&self, out: &mut String, depth: usize, group: &[(&VectorPath, Vec<Seg>)], extra: &str) {
        let first = group[0].0;
        let d = group
            .iter()
            .map(|(p, segs)| path_data(p, segs, self.k))
            .collect::<Vec<_>>()
            .join(" ");
        let style = first
            .fill_style_id
            .as_deref()
            .and_then(|id| self.styles.get(id).copied());
        out.push_str(&"  ".repeat(depth));
        out.push_str(&format!("<path d=\"{d}\""));
        match (first.winding, style.and_then(|s| s.fill)) {
            (Winding::None, _) => out.push_str(" fill=\"none\""),
            (rule, fill) => {
                let c = fill.unwrap_or(Rgba {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 1.0,
                });
                out.push_str(&format!(" fill=\"{}\"", hex(c)));
                if c.a < 1.0 {
                    out.push_str(&format!(" fill-opacity=\"{}\"", num(c.a)));
                }
                if rule == Winding::EvenOdd {
                    out.push_str(" fill-rule=\"evenodd\"");
                }
            }
        }
        if let Some(s) = style.and_then(|s| s.stroke) {
            out.push_str(&format!(
                " stroke=\"{}\" stroke-width=\"{}\"",
                hex(s.color),
                num(s.width_pt * self.k)
            ));
            if s.color.a < 1.0 {
                out.push_str(&format!(" stroke-opacity=\"{}\"", num(s.color.a)));
            }
        }
        out.push_str(extra);
        out.push_str("/>\n");
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

/// Flat export: one `<path>` per layer (consecutive paths sharing a `layer_id` form one compound
/// `d`), absolute `M`/`L`/`C`/`Z` data, `viewBox="0 0 W H"` in px. Group structure, names,
/// visibility and element opacity live in the Folio document; use [`export_proposal`] to keep them.
pub fn export_svg(
    paths: &[VectorPath],
    styles: &[StyleProposal],
    opts: &ExportOptions,
) -> Result<String> {
    let converted = paths.iter().map(convert).collect::<Result<Vec<_>>>()?;
    let (k, mut out) = header(opts, extent(converted.iter()))?;
    let writer = Writer {
        k,
        styles: styles.iter().map(|s| (s.style_id.as_str(), s)).collect(),
    };
    let mut items = paths.iter().zip(converted).peekable();
    while let Some(first) = items.next() {
        let layer = &first.0.layer_id;
        let mut group = vec![first];
        while let Some(next) = items.next_if(|(p, _)| &p.layer_id == layer) {
            group.push(next);
        }
        writer.path_element(&mut out, 1, &group, "");
    }
    out.push_str("</svg>\n");
    Ok(out)
}

/// Hierarchical export of a proposal (or an edited copy of its `paths`): the Folio layers become
/// `<g>` and `<path>` elements in document order, with `id` (when the layer name differs from the
/// element name), `opacity` from the layer appearance and `display="none"` for hidden layers.
/// Only paths whose `layer_id` names a vector layer of the snapshot are written. The artboard size
/// comes from the first artboard unless `opts.artboard_pt` overrides it.
pub fn export_proposal(proposal: &VectorProposal, opts: &ExportOptions) -> Result<String> {
    let doc = proposal.snapshot.document();
    let mut by_layer: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, p) in proposal.paths.iter().enumerate() {
        by_layer.entry(p.layer_id.as_str()).or_default().push(i);
    }
    let mut converted: Vec<Option<Vec<Seg>>> = Vec::with_capacity(proposal.paths.len());
    for p in &proposal.paths {
        converted.push(Some(convert(p)?));
    }
    let artboard = doc.artboards.first().map(|a| {
        let to_pt = |l: &hsk_studio_folio::Length| match l.unit {
            GeometryUnit::Pt => Some(l.value),
            _ => None,
        };
        (to_pt(&a.bounds.width), to_pt(&a.bounds.height), a.artboard_id.as_str())
    });
    let content = extent(converted.iter().flatten());
    let opts = ExportOptions {
        artboard_pt: opts.artboard_pt.or(match artboard {
            Some((Some(w), Some(h), _)) if w > 0.0 && h > 0.0 => Some([w, h]),
            _ => None,
        }),
        ..*opts
    };
    let (k, mut out) = header(&opts, content)?;
    let writer = Writer {
        k,
        styles: proposal.styles.iter().map(|s| (s.style_id.as_str(), s)).collect(),
    };
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
            out.push_str(&"  ".repeat(depth));
            out.push_str("</g>\n");
            continue;
        }
        let tag = if matches!(layer.payload, Payload::Group) { "g" } else { "path" };
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
                out.push_str(&"  ".repeat(depth));
                out.push_str(&format!("<g{extra}>\n"));
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
                    .filter_map(|&j| {
                        converted[j]
                            .take()
                            .map(|segs| (&proposal.paths[j], segs))
                    })
                    .collect();
                if !group.is_empty() {
                    writer.path_element(&mut out, depth, &group, &extra);
                }
            }
            _ => {}
        }
    }
    out.push_str("</svg>\n");
    Ok(out)
}
