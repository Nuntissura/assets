//! EPSF 3.0 export (lane SMB-DXF-EPS-IMPORT; parity row AF-07): DSC-conforming encapsulated
//! PostScript written from the same path model and Folio design records as the SVG export, with
//! no interpreter. Deterministic: same input, same text.
//!
//! - header: `%!PS-Adobe-3.0 EPSF-3.0`, `%%BoundingBox` (whole points, rounded outward) and
//!   `%%HiResBoundingBox` of the artboard, `%%LanguageLevel: 3`, `%%Pages: 1`, `%%EndComments`;
//!   no `showpage` side effects beyond the one the EPSF rules allow, wrapped in `save`/`restore`
//!   by the importing application as usual;
//! - geometry: document points (y down) under one `0 H translate 1 -1 scale`; each vector layer
//!   is one compound path (`moveto`/`lineto`/`curveto`/`closepath`);
//! - paint rows bottom to top: solid fill (`setrgbcolor` + `fill`/`eofill`), linear and radial
//!   gradient fills (LanguageLevel 3 `shfill`, shading types 2 and 3, stitched type 2 functions
//!   over the stops, inside `clip`/`eoclip`, the gradient transform with `concat`), solid
//!   strokes (weight, cap, join, miter limit, dash in points);
//! - typed losses: row, stop and layer opacity below 100 % (`opacity_not_exported`; PostScript has
//!   no transparency), blend modes (`blend_mode_not_exported`), gradient strokes
//!   (`gradient_stroke_not_exported`, drawn with the first stop colour), midpoints, spread
//!   reflect/repeat, perceptual interpolation, colour profiles (`colour_profile_not_converted`),
//!   stroke alignment other than centre, and from a proposal: hidden layers (`hidden_layer_skipped`,
//!   ignored), clips, masks and patterns (`clip_not_exported`, `mask_not_exported`,
//!   `pattern_not_exported`), text and raster layers (`layer_kind_not_exported`).
//!   No preview is embedded (`eps_preview_not_written`, ignored).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{Result, VectorError};
use crate::model::*;
use hsk_studio_accord::{Length as AccordLength, Unit};
use hsk_studio_folio::{
    BLEND_NORMAL, GeometryUnit, GradientInterpolation, GradientSpread, Length as FolioLength, Payload, ProfileBinding, StrokeAlign, StrokeCap, StrokeJoin, StudioAppearanceRow,
    StudioColor, StudioDesignRecords, StudioGradient, StudioGradientGeometry, StudioPaint, StudioStrokeStyle,
};
use hsk_studio_nib::Winding;
use std::collections::BTreeSet;

/// EPS text plus the losses the export took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpsExport {
    pub text: String,
    pub loss: Vec<LossEntry>,
}

/// Shortest decimal with at most 6 fraction digits, never exponent notation (PostScript reals).
fn num(v: f64) -> String {
    let r = (v * 1.0e6).round() / 1.0e6;
    if r == 0.0 {
        return "0".into();
    }
    let s = format!("{r:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_owned()
}

fn to_pt(l: &FolioLength, ppi: f64) -> Result<f64> {
    let unit = match l.unit {
        GeometryUnit::Mm => Unit::Millimetres,
        GeometryUnit::In => Unit::Inches,
        GeometryUnit::Px => Unit::Pixels,
        GeometryUnit::Pt => Unit::Points,
    };
    let v = AccordLength::new(l.value, unit).and_then(|a| a.convert(Unit::Points, Some(ppi))).map_err(|_| VectorError::InvalidGeometry)?.value();
    if v.is_finite() && v.abs() <= COORDINATE_LIMIT { Ok(v) } else { Err(VectorError::InvalidGeometry) }
}

/// Absolute anchor positions and controls in points.
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
        let v = l.convert(Unit::Points, None).map_err(|_| VectorError::InvalidGeometry)?.value();
        if v.is_finite() && v.abs() <= COORDINATE_LIMIT { Ok(v) } else { Err(VectorError::InvalidGeometry) }
    };
    p.anchors
        .iter()
        .map(|a| {
            let pos = [value(a.position.x)?, value(a.position.y)?];
            Ok(Seg { pos, cin: [pos[0] + value(a.incoming.x)?, pos[1] + value(a.incoming.y)?], cout: [pos[0] + value(a.outgoing.x)?, pos[1] + value(a.outgoing.y)?] })
        })
        .collect()
}

fn path_ops(p: &VectorPath, segs: &[Seg], out: &mut String) {
    let at = |v: [f64; 2]| format!("{} {}", num(v[0]), num(v[1]));
    out.push_str(&format!("{} m\n", at(segs[0].pos)));
    let n = segs.len();
    let count = if p.closed { n } else { n.saturating_sub(1) };
    for s in 0..count {
        let next = s.saturating_add(1);
        let (a, b) = (&segs[s], &segs[if next < n { next } else { 0 }]);
        let straight = a.cout == a.pos && b.cin == b.pos;
        let closing = p.closed && next == count;
        if straight {
            if !closing {
                out.push_str(&format!("{} l\n", at(b.pos)));
            }
        } else {
            out.push_str(&format!("{} {} {} c\n", at(a.cout), at(b.cin), at(b.pos)));
        }
    }
    if p.closed {
        out.push_str("h\n");
    }
}

struct Writer<'a> {
    design: &'a StudioDesignRecords,
    ppi: f64,
    loss: Vec<LossEntry>,
    noted: BTreeSet<(String, &'static str)>,
    body: String,
}

impl Writer<'_> {
    fn note(&mut self, label: &str, kind: LossKind, code: &'static str) {
        if self.noted.insert((label.to_owned(), code)) {
            self.loss.push(LossEntry { path: label.to_owned(), kind, code });
        }
    }

    fn colour(&mut self, c: &StudioColor, label: &str) -> String {
        if matches!(c.colour_profile_id, ProfileBinding::Assigned(_)) {
            self.note(label, LossKind::Unsupported, "colour_profile_not_converted");
        }
        format!("{} {} {}", num(c.r.clamp(0.0, 1.0)), num(c.g.clamp(0.0, 1.0)), num(c.b.clamp(0.0, 1.0)))
    }

    /// `<< /ShadingType .. >>` of a Folio gradient in gradient space (PostScript LanguageLevel 3).
    fn shading(&mut self, g: &StudioGradient, label: &str) -> Result<String> {
        if g.stops.len() < 2 {
            return Err(VectorError::InvalidGeometry);
        }
        if g.stops.iter().any(|s| s.opacity < 100.0) {
            self.note(label, LossKind::Unsupported, "opacity_not_exported");
        }
        if g.stops.iter().any(|s| s.midpoint.is_some()) {
            self.note(label, LossKind::Approximated, "gradient_midpoint_not_exported");
        }
        if !matches!(g.spread, GradientSpread::Pad) {
            self.note(label, LossKind::Approximated, "gradient_spread_not_exported");
        }
        if !matches!(g.interpolation, GradientInterpolation::Classic) {
            self.note(label, LossKind::Approximated, "gradient_interpolation_not_exported");
        }
        let (kind, coords) = match &g.geometry {
            StudioGradientGeometry::Linear { x1, y1, x2, y2 } => (2, [*x1, *y1, *x2, *y2].map(num).join(" ")),
            StudioGradientGeometry::Radial { cx, cy, r, fx, fy, fr } => (3, [*fx, *fy, *fr, *cx, *cy, *r].map(num).join(" ")),
        };
        let mut functions = Vec::new();
        let mut bounds = Vec::new();
        let mut last = 0.0_f64;
        for (i, pair) in g.stops.windows(2).enumerate() {
            let (a, b) = (&pair[0], &pair[1]);
            let (ca, cb) = (self.colour(&a.colour, label), self.colour(&b.colour, label));
            functions.push(format!("<< /FunctionType 2 /Domain [0 1] /C0 [{ca}] /C1 [{cb}] /N 1 >>"));
            if i > 0 {
                // Bounds must increase strictly inside the domain: equal positions are nudged.
                let p = (a.position / 100.0).clamp(0.0, 1.0).max(last + 1.0e-6).min(1.0 - 1.0e-6);
                bounds.push(num(p));
                last = p;
            }
        }
        let encode: Vec<&str> = functions.iter().map(|_| "0 1").collect();
        let first = (g.stops[0].position / 100.0).clamp(0.0, 1.0);
        let lastp = (g.stops[g.stops.len().saturating_sub(1)].position / 100.0).clamp(0.0, 1.0);
        // Stops inside [first, last] map onto the shading's 0..1 parameter; outside is padded.
        let span = lastp - first;
        let domain = if span > 1.0e-9 { format!("{} {}", num(first), num(lastp)) } else { "0 1".into() };
        let bounds = if span > 1.0e-9 {
            bounds
        } else {
            // All stops at one position: equal-width pieces (the gradient is a hard step).
            (1..functions.len()).map(|i| num(i as f64 / functions.len() as f64)).collect()
        };
        let function = format!(
            "<< /FunctionType 3 /Domain [{domain}] /Functions [{}] /Bounds [{}] /Encode [{}] >>",
            functions.join(" "),
            bounds.join(" "),
            encode.join(" ")
        );
        Ok(format!("<< /ShadingType {kind} /ColorSpace /DeviceRGB /Coords [{coords}] /Domain [{domain}] /Extend [true true] /Function {function} >>"))
    }

    fn stroke_ops(&mut self, st: &StudioStrokeStyle, label: &str) -> Result<String> {
        if !matches!(st.align, StrokeAlign::Center) {
            self.note(label, LossKind::Approximated, "stroke_align_not_exported");
        }
        let width = to_pt(&st.weight, self.ppi)?;
        let cap = match st.cap {
            StrokeCap::Butt => 0,
            StrokeCap::Round => 1,
            StrokeCap::Projecting => 2,
        };
        let join = match st.join {
            StrokeJoin::Miter => 0,
            StrokeJoin::Round => 1,
            StrokeJoin::Bevel => 2,
        };
        let dash = st.dash.iter().map(|d| to_pt(d, self.ppi).map(num)).collect::<Result<Vec<_>>>()?;
        let offset = to_pt(&st.dash_offset, self.ppi)?;
        Ok(format!("{} w {cap} J {join} j {} M [{}] {} d\n", num(width), num(st.miter_limit.max(1.0)), dash.join(" "), num(offset)))
    }

    /// One layer's compound path painted with its style rows (bottom to top).
    fn layer(&mut self, layer_id: &str, group: &[(&VectorPath, Vec<Seg>)], label: &str) -> Result<()> {
        let mut path = String::new();
        for (p, segs) in group {
            path_ops(p, segs, &mut path);
        }
        let even_odd = group.iter().any(|(p, _)| p.winding == Winding::EvenOdd);
        let filled = group.iter().any(|(p, _)| p.winding != Winding::None);
        let Some(style) = layer_style(self.design, layer_id) else {
            // No style: SVG's default paint is a black fill; keep the same default.
            if filled {
                self.body.push_str(&format!("gsave\n{path}0 0 0 rg {}\ngrestore\n", if even_odd { "eofill" } else { "fill" }));
            }
            return Ok(());
        };
        for row in &style.rows {
            match row {
                StudioAppearanceRow::Fill { paint, opacity, blend_mode, visible } => {
                    if !visible || !filled {
                        continue;
                    }
                    if *opacity < 100.0 {
                        self.note(label, LossKind::Unsupported, "opacity_not_exported");
                    }
                    if *blend_mode != BLEND_NORMAL {
                        self.note(label, LossKind::Unsupported, "blend_mode_not_exported");
                    }
                    match paint {
                        StudioPaint::Solid { colour } => {
                            let c = self.colour(colour, label);
                            self.body.push_str(&format!("gsave\n{path}{c} rg {}\ngrestore\n", if even_odd { "eofill" } else { "fill" }));
                        }
                        StudioPaint::Gradient { gradient } => {
                            let sh = self.shading(gradient, label)?;
                            let t = gradient.transform;
                            let m = t.map(num).join(" ");
                            self.body.push_str(&format!("gsave\n{path}{}\n[{m}] concat\n{sh} shfill\ngrestore\n", if even_odd { "eoclip newpath" } else { "clip newpath" }));
                        }
                    }
                }
                StudioAppearanceRow::Stroke { paint, stroke, opacity, blend_mode, visible } => {
                    if !visible {
                        continue;
                    }
                    if *opacity < 100.0 {
                        self.note(label, LossKind::Unsupported, "opacity_not_exported");
                    }
                    if *blend_mode != BLEND_NORMAL {
                        self.note(label, LossKind::Unsupported, "blend_mode_not_exported");
                    }
                    let c = match paint {
                        StudioPaint::Solid { colour } => self.colour(colour, label),
                        StudioPaint::Gradient { gradient } => {
                            self.note(label, LossKind::Approximated, "gradient_stroke_not_exported");
                            let first = gradient.stops.first().map(|s| s.colour.clone());
                            match first {
                                Some(colour) => self.colour(&colour, label),
                                None => "0 0 0".into(),
                            }
                        }
                    };
                    let ops = self.stroke_ops(stroke, label)?;
                    self.body.push_str(&format!("gsave\n{path}{ops}{c} RG S\ngrestore\n"));
                }
            }
        }
        Ok(())
    }
}

fn header(width: f64, height: f64) -> Result<String> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 && width <= COORDINATE_LIMIT && height <= COORDINATE_LIMIT) {
        return Err(VectorError::InvalidGeometry);
    }
    Ok(format!(
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 {} {}\n%%HiResBoundingBox: 0 0 {} {}\n%%Creator: Handshake Studio\n%%LanguageLevel: 3\n%%Pages: 1\n%%EndComments\n%%BeginProlog\n/m {{moveto}} bind def /l {{lineto}} bind def /c {{curveto}} bind def /h {{closepath}} bind def\n/rg {{setrgbcolor}} bind def /RG {{setrgbcolor}} bind def /S {{stroke}} bind def\n/w {{setlinewidth}} bind def /J {{setlinecap}} bind def /j {{setlinejoin}} bind def /M {{setmiterlimit}} bind def /d {{setdash}} bind def\n%%EndProlog\n%%Page: 1 1\ngsave\n0 {} translate 1 -1 scale\n",
        num(width.ceil()),
        num(height.ceil()),
        num(width),
        num(height),
        num(height)
    ))
}

fn finish(open: String, w: Writer<'_>) -> EpsExport {
    let mut loss = w.loss;
    loss.push(LossEntry { path: "/eps".into(), kind: LossKind::Ignored, code: "eps_preview_not_written" });
    EpsExport { text: format!("{open}{}grestore\nshowpage\n%%Trailer\n%%EOF\n", w.body), loss }
}

fn extent(groups: &[(&VectorPath, Vec<Seg>)]) -> [f64; 2] {
    let mut e = [1.0_f64, 1.0_f64];
    for (_, segs) in groups {
        for s in segs {
            for p in [s.pos, s.cin, s.cout] {
                e = [e[0].max(p[0]), e[1].max(p[1])];
            }
        }
    }
    e
}

/// Flat export: consecutive paths sharing a `layer_id` form one compound path painted with that
/// layer's style (module docs). The bounding box is `opts.artboard_pt`, else the extent of all
/// anchors and handles (at least 1 pt).
pub fn export_eps(paths: &[VectorPath], design: &StudioDesignRecords, opts: &ExportOptions) -> Result<EpsExport> {
    let converted: Vec<(&VectorPath, Vec<Seg>)> = paths.iter().map(|p| convert(p).map(|s| (p, s))).collect::<Result<_>>()?;
    let [w, h] = opts.artboard_pt.unwrap_or_else(|| extent(&converted));
    let open = header(w, h)?;
    let mut writer = Writer { design, ppi: opts.pixels_per_inch, loss: Vec::new(), noted: BTreeSet::new(), body: String::new() };
    let mut i = 0usize;
    let mut ordinal = 0usize;
    while i < converted.len() {
        let layer = &converted[i].0.layer_id;
        let mut j = i;
        while j < converted.len() && &converted[j].0.layer_id == layer {
            j = j.saturating_add(1);
        }
        ordinal = ordinal.saturating_add(1);
        writer.layer(layer, &converted[i..j], &format!("/layer[{ordinal}]"))?;
        i = j;
    }
    Ok(finish(open, writer))
}

/// Export of a proposal (or an edited copy): only paths of visible vector layers are written, in
/// document order; the first artboard sets the bounding box unless `opts.artboard_pt` overrides
/// it. Layer opacity, clips, masks and patterns are typed losses (module docs).
pub fn export_eps_proposal(proposal: &VectorProposal, opts: &ExportOptions) -> Result<EpsExport> {
    let doc = proposal.snapshot.document();
    let mut hidden = BTreeSet::new();
    let mut loss = Vec::new();
    let note = |loss: &mut Vec<LossEntry>, path: String, kind, code| {
        if !loss.iter().any(|l: &LossEntry| l.path == path && l.code == code) {
            loss.push(LossEntry { path, kind, code });
        }
    };
    // A layer is hidden when it or any ancestor is.
    let by_id: std::collections::BTreeMap<&str, &hsk_studio_folio::StudioLayer> = doc.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect();
    for (ordinal, l) in doc.layers.iter().enumerate() {
        let mut cur = Some(l);
        let mut depth = 0usize;
        while let Some(layer) = cur {
            let visible = doc_layer_visible(layer);
            if !visible {
                hidden.insert(l.layer_id.as_str());
                break;
            }
            depth = depth.saturating_add(1);
            if depth > 256 {
                break;
            }
            cur = match &layer.parent {
                hsk_studio_folio::Parent::Node(n) => by_id.get(n.node_id.as_str()).copied(),
                hsk_studio_folio::Parent::Root(()) => None,
            };
        }
        if !matches!(l.payload, Payload::Group | Payload::Vector { .. }) {
            note(&mut loss, format!("/layer[{}]", ordinal.saturating_add(1)), LossKind::Unsupported, "layer_kind_not_exported");
        }
        if doc_layer_opacity(l) < 100.0 {
            note(&mut loss, "/eps".into(), LossKind::Unsupported, "opacity_not_exported");
        }
    }
    let rec = &proposal.vector_records;
    if !rec.clips.is_empty() {
        note(&mut loss, "/eps".into(), LossKind::Unsupported, "clip_not_exported");
    }
    if !rec.masks.is_empty() {
        note(&mut loss, "/eps".into(), LossKind::Unsupported, "mask_not_exported");
    }
    if !rec.patterns.is_empty() || !rec.pattern_paints.is_empty() {
        note(&mut loss, "/eps".into(), LossKind::Unsupported, "pattern_not_exported");
    }
    let visible: Vec<VectorPath> = proposal.paths.iter().filter(|p| !hidden.contains(p.layer_id.as_str())).cloned().collect();
    if visible.len() < proposal.paths.len() {
        note(&mut loss, "/eps".into(), LossKind::Ignored, "hidden_layer_skipped");
    }
    let artboard = doc.artboards.first().and_then(|a| {
        let pt = |l: &FolioLength| (l.unit == GeometryUnit::Pt).then_some(l.value);
        match (pt(&a.bounds.width), pt(&a.bounds.height)) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some([w, h]),
            _ => None,
        }
    });
    let opts = ExportOptions { artboard_pt: opts.artboard_pt.or(artboard), ..*opts };
    let mut out = export_eps(&visible, &proposal.design, &opts)?;
    loss.append(&mut out.loss);
    out.loss = loss;
    Ok(out)
}

fn doc_layer_visible(l: &hsk_studio_folio::StudioLayer) -> bool {
    l.visible
}

fn doc_layer_opacity(l: &hsk_studio_folio::StudioLayer) -> f64 {
    l.appearance.as_ref().map_or(100.0, |a| a.opacity)
}
