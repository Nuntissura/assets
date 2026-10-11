//! SVG import into a Folio proposal plus nib anchor geometry, and deterministic SVG export of the
//! supported subset. Input is hostile: DTD internal subsets are refused, no link is ever
//! followed, and node, depth, segment and byte budgets are enforced while decoding.
use crate::error::{Result, VectorError};
use crate::gzip;
use crate::ids::IdMint;
use crate::model::*;
use hsk_studio_accord::{CancellationToken, DomainId, Length as AccordLength, Unit};
use hsk_studio_folio::{
    ArtboardSchema, BLEND_NORMAL, Bounds, Budget, DocumentSchema, GeometryUnit,
    GradientInterpolation, GradientSpread, GraphSchema, Isolation, LayerAppearance, LayerSchema,
    Length as FolioLength, MAX_DASH_ENTRIES, MAX_GRADIENT_STOPS, NodeKind, NodeRef, Parent,
    Payload, ProfileBinding, Resolution, Resolver, StrokeAlign, StrokeCap, StrokeJoin,
    StudioAppearanceRow, StudioArtboard, StudioColor, StudioDesignRecords, StudioDocument,
    StudioGradient, StudioGradientGeometry, StudioGradientStop, StudioLayer, StudioLayerGraph,
    StudioNodeDesign, StudioPaint, StudioPaintStyle, StudioPublishing, StudioStrokeStyle,
    StyleKind, StyleRegistrySchema, TileRef, Unresolved, validate_design_records,
    validate_document,
};
use hsk_studio_nib::{Anchor, Mirroring, Point, Winding};
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::str::FromStr;

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
/// Cubic Bezier control distance for a quarter ellipse: `4/3 * (sqrt(2) - 1)`.
const KAPPA: f64 = 0.552_284_749_830_793_4;

/// Default viewport (CSS replaced-element size) when neither size nor `viewBox` is given.
const DEFAULT_VIEWPORT: [f64; 2] = [300.0, 150.0];

// ----------------------------------------------------------------------------------------------
// Affine transform
// ----------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Affine {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn scale(sx: f64, sy: f64) -> Self {
        Self {
            a: sx,
            d: sy,
            ..Self::IDENTITY
        }
    }

    fn translate(tx: f64, ty: f64) -> Self {
        Self {
            e: tx,
            f: ty,
            ..Self::IDENTITY
        }
    }

    /// `self * child`: the child transform applies first.
    fn then(self, child: Self) -> Self {
        Self {
            a: self.a * child.a + self.c * child.b,
            b: self.b * child.a + self.d * child.b,
            c: self.a * child.c + self.c * child.d,
            d: self.b * child.c + self.d * child.d,
            e: self.a * child.e + self.c * child.f + self.e,
            f: self.b * child.e + self.d * child.f + self.f,
        }
    }

    fn apply(self, p: [f64; 2]) -> [f64; 2] {
        [
            self.a * p[0] + self.c * p[1] + self.e,
            self.b * p[0] + self.d * p[1] + self.f,
        ]
    }

    fn is_identity(self) -> bool {
        self == Self::IDENTITY
    }

    fn finite(self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|v| v.is_finite())
    }

    /// Geometric mean scale, used for stroke widths.
    fn mean_scale(self) -> f64 {
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }

    fn uniform(self) -> bool {
        let sx = self.a.hypot(self.b);
        let sy = self.c.hypot(self.d);
        let dot = self.a * self.c + self.b * self.d;
        (sx - sy).abs() <= 1e-9 * sx.max(sy).max(1.0) && dot.abs() <= 1e-9 * sx * sy
    }
}

// ----------------------------------------------------------------------------------------------
// Raw geometry (absolute control points, user space) and the subpath builder
// ----------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct RawAnchor {
    pos: [f64; 2],
    /// Absolute incoming control point; equal to `pos` when there is no handle.
    cin: [f64; 2],
    /// Absolute outgoing control point; equal to `pos` when there is no handle.
    cout: [f64; 2],
}

impl RawAnchor {
    fn at(pos: [f64; 2]) -> Self {
        Self {
            pos,
            cin: pos,
            cout: pos,
        }
    }
}

#[derive(Debug)]
struct RawSubpath {
    anchors: Vec<RawAnchor>,
    closed: bool,
}

#[derive(Default)]
struct SubpathBuilder {
    done: Vec<RawSubpath>,
    cur: Option<RawSubpath>,
    degenerate: usize,
}

fn near(a: [f64; 2], b: [f64; 2]) -> bool {
    (a[0] - b[0]).abs() <= 1e-9 && (a[1] - b[1]).abs() <= 1e-9
}

impl SubpathBuilder {
    fn push_done(&mut self, sub: RawSubpath) {
        if sub.anchors.len() >= 2 {
            self.done.push(sub);
        } else {
            self.degenerate += 1;
        }
    }

    fn flush(&mut self) {
        if let Some(sub) = self.cur.take() {
            self.push_done(sub);
        }
    }

    fn move_to(&mut self, p: [f64; 2]) {
        self.flush();
        self.cur = Some(RawSubpath {
            anchors: vec![RawAnchor::at(p)],
            closed: false,
        });
    }

    fn line_to(&mut self, p: [f64; 2]) {
        if let Some(cur) = &mut self.cur {
            cur.anchors.push(RawAnchor::at(p));
        }
    }

    fn cubic_to(&mut self, c1: [f64; 2], c2: [f64; 2], p: [f64; 2]) {
        if let Some(cur) = &mut self.cur {
            if let Some(last) = cur.anchors.last_mut() {
                last.cout = c1;
            }
            cur.anchors.push(RawAnchor {
                pos: p,
                cin: c2,
                cout: p,
            });
        }
    }

    /// Quadratic to cubic elevation is exact: `c1 = p0 + 2/3 (q - p0)`, `c2 = p + 2/3 (q - p)`.
    fn quad_to(&mut self, q: [f64; 2], p: [f64; 2]) {
        let Some(p0) = self
            .cur
            .as_ref()
            .and_then(|c| c.anchors.last())
            .map(|a| a.pos)
        else {
            return;
        };
        let k = 2.0 / 3.0;
        let c1 = [p0[0] + k * (q[0] - p0[0]), p0[1] + k * (q[1] - p0[1])];
        let c2 = [p[0] + k * (q[0] - p[0]), p[1] + k * (q[1] - p[1])];
        self.cubic_to(c1, c2, p);
    }

    /// Closes the subpath. A last anchor that coincides with the first is merged into it (the
    /// closing curve's incoming handle moves to the first anchor), as Illustrator stores it.
    fn close(&mut self) {
        let Some(mut cur) = self.cur.take() else {
            return;
        };
        cur.closed = true;
        if cur.anchors.len() >= 2 {
            let first = cur.anchors[0].pos;
            if near(cur.anchors[cur.anchors.len() - 1].pos, first) {
                let last = cur.anchors.pop().expect("length checked");
                cur.anchors[0].cin = last.cin;
            }
        }
        self.push_done(cur);
    }

    fn finish(mut self) -> (Vec<RawSubpath>, usize) {
        self.flush();
        (self.done, self.degenerate)
    }
}

// ----------------------------------------------------------------------------------------------
// Style cascade
// ----------------------------------------------------------------------------------------------

/// Fallback after `url(#id)` when the reference cannot be used.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fallback {
    Unspecified,
    None,
    Current,
    Color(u8, u8, u8, f64),
}

#[derive(Clone, Debug, PartialEq)]
enum PaintValue {
    None,
    Current,
    Color(u8, u8, u8, f64),
    /// `url(#id)`: resolved when the shape is emitted (a gradient needs the bounding box).
    Url(Rc<str>, Fallback),
}

#[derive(Clone, Debug)]
struct Inherited {
    fill: PaintValue,
    stroke: PaintValue,
    /// User units.
    stroke_width: f64,
    fill_rule: Winding,
    fill_opacity: f64,
    stroke_opacity: f64,
    color: (u8, u8, u8, f64),
    visible: bool,
    cap: StrokeCap,
    join: StrokeJoin,
    miter_limit: f64,
    /// User units; empty is solid.
    dash: Vec<f64>,
    dash_offset: f64,
    /// `paint-order` puts the stroke below the fill.
    stroke_first: bool,
}

impl Inherited {
    fn root() -> Self {
        Self {
            fill: PaintValue::Color(0, 0, 0, 1.0),
            stroke: PaintValue::None,
            stroke_width: 1.0,
            fill_rule: Winding::NonZero,
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            color: (0, 0, 0, 1.0),
            visible: true,
            cap: StrokeCap::Butt,
            join: StrokeJoin::Miter,
            miter_limit: 4.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            stroke_first: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Local {
    opacity: f64,
    display_none: bool,
}

#[derive(Clone)]
struct Ctx {
    matrix: Affine,
    st: Inherited,
    parent: Parent,
    depth: usize,
}

fn unit_to_user(l: svgtypes::Length) -> Option<f64> {
    use svgtypes::LengthUnit as U;
    let n = l.number;
    Some(match l.unit {
        U::None | U::Px => n,
        U::In => n * 96.0,
        U::Cm => n * 96.0 / 2.54,
        U::Mm => n * 96.0 / 25.4,
        U::Pt => n * 4.0 / 3.0,
        U::Pc => n * 16.0,
        U::Em | U::Ex | U::Percent => return None,
    })
}

/// A unitless number or a percentage as a fraction clamped to `0..=1`.
fn fraction(text: &str) -> Option<f64> {
    let l = svgtypes::Length::from_str(text.trim()).ok()?;
    let v = match l.unit {
        svgtypes::LengthUnit::None => l.number,
        svgtypes::LengthUnit::Percent => l.number / 100.0,
        _ => return None,
    };
    v.is_finite().then(|| v.clamp(0.0, 1.0))
}

// ----------------------------------------------------------------------------------------------
// Import
// ----------------------------------------------------------------------------------------------

struct LocalResolver<'a> {
    paths: &'a BTreeSet<&'a str>,
}

impl Resolver for LocalResolver<'_> {
    fn primitive(&self, schema_id: &str, object_id: &str) -> Resolution {
        if schema_id == hsk_studio_accord::SCHEMA_STUDIO_VECTOR_PATH && self.paths.contains(object_id)
        {
            Resolution::Available
        } else {
            Resolution::Absent
        }
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

enum DoctypeScan {
    None,
    ExternalIdOnly,
    InternalSubset,
}

/// Finds the first DOCTYPE declaration and tells whether it has an internal subset (`[` outside
/// quotes before the closing `>`). A conservative text scan: roxmltree itself never resolves
/// external identifiers, so only the internal subset (entity declarations) needs refusing.
fn scan_doctype(text: &str) -> DoctypeScan {
    let Some(at) = text.find("<!DOCTYPE") else {
        return DoctypeScan::None;
    };
    let mut quote = 0u8;
    for &b in &text.as_bytes()[at + 9..] {
        match (quote, b) {
            (0, b'"' | b'\'') => quote = b,
            (q, c) if q != 0 && q == c => quote = 0,
            (0, b'[') => return DoctypeScan::InternalSubset,
            (0, b'>') => return DoctypeScan::ExternalIdOnly,
            _ => {}
        }
    }
    DoctypeScan::InternalSubset
}

struct Importer<'a, 'd> {
    limits: Limits,
    cancel: &'a CancellationToken,
    mint: &'a mut dyn IdMint,
    loss: Vec<LossEntry>,
    layers: Vec<StudioLayer>,
    paths: Vec<VectorPath>,
    styles: Vec<StudioPaintStyle>,
    /// Canonical text of a row list -> index into `styles` (dedupe).
    style_index: BTreeMap<String, usize>,
    nodes: Vec<StudioNodeDesign>,
    gradient_provenance: Vec<GradientProvenance>,
    /// Records retained so far, counted the way Folio meters them (`Budget::nodes`).
    design_units: usize,
    segments: usize,
    elements: u32,
    /// Gradient elements by id (first wins), found by one pass before the walk.
    gradients: BTreeMap<&'d str, Node<'d, 'd>>,
    /// Viewport in user units: the basis of percentage lengths in `userSpaceOnUse` gradients.
    user_viewport: [f64; 2],
}

/// Imports an SVG into a Folio proposal (nodes) plus nib anchor geometry, with a typed loss report.
///
/// Supported: `svg` (`viewBox`, `width`, `height`, `preserveAspectRatio`), `g`/`a`, `path` (arcs,
/// quadratics and smooth curves become cubics), `rect`, `circle`, `ellipse`, `line`, `polyline`,
/// `polygon`, `transform` (baked into the points), `fill`, `stroke`, `stroke-width`, `fill-rule`,
/// `fill-opacity`, `stroke-opacity`, `opacity`, `color`, `display`, `visibility`, as attributes or
/// inline `style`. Everything else is reported in `loss`; nothing is executed or fetched.
pub fn import_svg(
    bytes: &[u8],
    policy: &ImportPolicy,
    mint: &mut dyn IdMint,
    cancel: &CancellationToken,
) -> Result<crate::model::VectorProposal> {
    cancel.check().map_err(|_| VectorError::Canceled)?;
    policy.validate()?;
    if bytes.len() > policy.limits.max_bytes {
        return Err(VectorError::BudgetExceeded("bytes"));
    }
    let source_len = bytes.len();
    // SVGZ: one gzip member, inflated under the decompression cap and never unwrapped twice.
    let inflated;
    let (bytes, source_format) = if gzip::is_gzip(bytes) {
        inflated = gzip::gunzip(bytes, policy.limits.max_decompressed_bytes)?;
        cancel.check().map_err(|_| VectorError::Canceled)?;
        (inflated.as_slice(), "svgz")
    } else {
        (bytes, "svg")
    };
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| VectorError::NotSvg)?;
    let mut loss = Vec::new();
    let allow_dtd = match scan_doctype(text) {
        DoctypeScan::None => false,
        DoctypeScan::ExternalIdOnly => {
            loss.push(LossEntry {
                path: "/".into(),
                kind: LossKind::ExternalDenied,
                code: "doctype_external_id_ignored",
            });
            true
        }
        DoctypeScan::InternalSubset => return Err(VectorError::EntityDenied),
    };
    let options = ParsingOptions {
        allow_dtd,
        nodes_limit: policy.limits.max_nodes,
        entity_resolver: None,
    };
    let doc = Document::parse_with_options(text, options).map_err(|e| match e {
        roxmltree::Error::NodesLimitReached
        | roxmltree::Error::AttributesLimitReached
        | roxmltree::Error::NamespacesLimitReached => VectorError::BudgetExceeded("nodes"),
        roxmltree::Error::DtdDetected => VectorError::EntityDenied,
        _ => VectorError::MalformedXml,
    })?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(SVG_NS) {
        return Err(VectorError::NotSvg);
    }
    let px_to_pt = AccordLength::new(1.0, Unit::Pixels)
        .and_then(|l| l.convert(Unit::Points, Some(policy.pixels_per_inch)))
        .map_err(|_| VectorError::InvalidPolicy("pixels_per_inch"))?
        .value();

    let mut gradients = BTreeMap::new();
    for node in doc.descendants() {
        if node.is_element()
            && node.tag_name().namespace() == Some(SVG_NS)
            && matches!(node.tag_name().name(), "linearGradient" | "radialGradient")
            && let Some(id) = plain_attr(node, "id")
        {
            gradients.entry(id).or_insert(node);
        }
    }
    let mut imp = Importer {
        limits: policy.limits,
        cancel,
        mint,
        loss,
        layers: Vec::new(),
        paths: Vec::new(),
        styles: Vec::new(),
        style_index: BTreeMap::new(),
        nodes: Vec::new(),
        gradient_provenance: Vec::new(),
        design_units: 0,
        segments: 0,
        elements: 1,
        gradients,
        user_viewport: DEFAULT_VIEWPORT,
    };
    let document_id = imp.mint_id("SDOC")?;
    let artboard_id = imp.mint_id("SART")?;
    let root_loc = "/svg[1]";
    imp.check_href(root, root_loc);

    // Viewport and viewBox mapping (preserveAspectRatio).
    let view_box = plain_attr(root, "viewBox").and_then(|v| svgtypes::ViewBox::from_str(v).ok());
    if plain_attr(root, "viewBox").is_some() && view_box.is_none_or(|v| !(v.w > 0.0 && v.h > 0.0)) {
        imp.note(root_loc, LossKind::Unsupported, "view_box_invalid");
    }
    let view_box = view_box.filter(|v| v.w > 0.0 && v.h > 0.0 && v.w.is_finite() && v.h.is_finite());
    let size_attr = |name: &str| {
        plain_attr(root, name)
            .and_then(|v| svgtypes::Length::from_str(v.trim()).ok())
            .and_then(unit_to_user)
            .filter(|v| *v > 0.0 && v.is_finite())
    };
    let (vw, vh) = match (size_attr("width"), size_attr("height"), view_box) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some(vb)) => (w, w * vb.h / vb.w),
        (None, Some(h), Some(vb)) => (h * vb.w / vb.h, h),
        (None, None, Some(vb)) => (vb.w, vb.h),
        _ => {
            imp.note(root_loc, LossKind::Approximated, "viewport_defaulted");
            (DEFAULT_VIEWPORT[0], DEFAULT_VIEWPORT[1])
        }
    };
    if vw > COORDINATE_LIMIT || vh > COORDINATE_LIMIT {
        return Err(VectorError::InvalidGeometry);
    }
    imp.user_viewport = view_box.map_or([vw, vh], |vb| [vb.w, vb.h]);
    let mut base = Affine::scale(px_to_pt, px_to_pt);
    if let Some(vb) = view_box {
        let par = plain_attr(root, "preserveAspectRatio")
            .and_then(|v| svgtypes::AspectRatio::from_str(v).ok())
            .unwrap_or_default();
        if par.slice && par.align != svgtypes::Align::None {
            imp.note(root_loc, LossKind::Approximated, "slice_viewport_not_clipped");
        }
        let (sx, sy) = (vw / vb.w, vh / vb.h);
        let view = if par.align == svgtypes::Align::None {
            Affine::scale(sx, sy).then(Affine::translate(-vb.x, -vb.y))
        } else {
            let s = if par.slice { sx.max(sy) } else { sx.min(sy) };
            use svgtypes::Align as A;
            let (fx, fy) = match par.align {
                A::XMinYMin => (0.0, 0.0),
                A::XMidYMin => (0.5, 0.0),
                A::XMaxYMin => (1.0, 0.0),
                A::XMinYMid => (0.0, 0.5),
                A::XMidYMid | A::None => (0.5, 0.5),
                A::XMaxYMid => (1.0, 0.5),
                A::XMinYMax => (0.0, 1.0),
                A::XMidYMax => (0.5, 1.0),
                A::XMaxYMax => (1.0, 1.0),
            };
            Affine::translate((vw - vb.w * s) * fx, (vh - vb.h * s) * fy)
                .then(Affine::scale(s, s))
                .then(Affine::translate(-vb.x, -vb.y))
        };
        base = base.then(view);
    }
    let mut ctx = Ctx {
        matrix: base,
        st: Inherited::root(),
        parent: Parent::Node(NodeRef {
            node_kind: NodeKind::Artboard,
            node_id: artboard_id.clone(),
        }),
        depth: 0,
    };
    let local = imp.cascade(root, root_loc, &mut ctx.st);
    if local.opacity < 1.0 {
        imp.note(root_loc, LossKind::Unsupported, "unsupported_root_opacity");
    }
    if let Some(t) = imp.own_transform(root, root_loc)? {
        ctx.matrix = ctx.matrix.then(t);
    }
    imp.walk_children(root, &ctx, root_loc)?;

    let Importer {
        loss,
        layers,
        paths,
        styles,
        nodes,
        gradient_provenance,
        ..
    } = imp;
    let artboard = StudioArtboard {
        schema_id: ArtboardSchema::V1,
        artboard_id: artboard_id.clone(),
        name: "svg-viewport".into(),
        parent: Parent::Root(()),
        bounds: Bounds {
            x: pt(0.0),
            y: pt(0.0),
            width: pt(vw * px_to_pt),
            height: pt(vh * px_to_pt),
        },
    };
    let mut design = StudioDesignRecords::new(document_id.clone());
    design.paint_styles = styles;
    design.nodes = nodes;
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id,
        revision: 1,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: ProfileBinding::Unassigned(()),
        working_space_cmyk: ProfileBinding::Unassigned(()),
        working_space_gray: ProfileBinding::Unassigned(()),
        working_space_spot: ProfileBinding::Unassigned(()),
        blending_space: ProfileBinding::Unassigned(()),
        artboards: vec![artboard],
        page_spreads: Vec::new(),
        layers,
        graph: StudioLayerGraph {
            schema_id: GraphSchema::V1,
            operations: Vec::new(),
            edges: Vec::new(),
            outputs: Vec::new(),
        },
        sequences: Vec::new(),
    };
    let nodes = policy.limits.max_nodes as usize;
    let budget = Budget {
        input_bytes: nodes.saturating_mul(2048).max(1 << 20),
        nodes: nodes.saturating_add(8),
        edges: 0,
        payload_bytes: nodes.saturating_mul(512).max(1 << 18),
        depth: policy.limits.max_depth.saturating_add(4),
    };
    let known: BTreeSet<&str> = paths.iter().map(|p| p.path_id.as_str()).collect();
    let snapshot = validate_document(document, budget, cancel, &LocalResolver { paths: &known })
        .map_err(folio_error)?;
    drop(known);
    // The design records are Folio's: validated here against the validated document, with the
    // record budget the importer already enforced (`Importer::spend_design`).
    let design_budget = Budget {
        nodes: design_unit_cap(&policy.limits).saturating_add(8),
        edges: 0,
        ..budget
    };
    let issues = validate_design_records(
        &design,
        snapshot.document(),
        design_budget,
        cancel,
        &Unresolved,
    )
    .map_err(folio_error)?;
    if !issues.is_empty() {
        return Err(VectorError::Folio(hsk_studio_folio::Code::DanglingReference));
    }
    Ok(crate::model::VectorProposal {
        snapshot,
        paths,
        design,
        gradient_provenance,
        loss,
        provenance: Provenance {
            source_format,
            source_unit: "px",
            pixels_per_inch: policy.pixels_per_inch,
            px_to_pt,
            source_len,
            decoded_len: bytes.len(),
            view_box: view_box.map(|v| [v.x, v.y, v.w, v.h]),
            viewport_px: [vw, vh],
        },
    })
}

fn folio_error(d: hsk_studio_folio::Diagnostic) -> VectorError {
    if d.code == hsk_studio_folio::Code::Canceled {
        VectorError::Canceled
    } else {
        VectorError::Folio(d.code)
    }
}

/// Most design records (styles, rows, gradient stops, dash pairs, node entries) one import keeps.
fn design_unit_cap(limits: &Limits) -> usize {
    (limits.max_nodes as usize).saturating_mul(16)
}

/// Attribute without a namespace; `foo:d` never stands in for `d`.
fn plain_attr<'a>(el: Node<'a, '_>, name: &str) -> Option<&'a str> {
    el.attributes()
        .find(|a| a.name() == name && a.namespace().is_none())
        .map(|a| a.value())
}

fn pt(value: f64) -> FolioLength {
    FolioLength {
        value,
        unit: GeometryUnit::Pt,
    }
}

impl<'d> Importer<'_, 'd> {
    fn note(&mut self, loc: &str, kind: LossKind, code: &'static str) {
        self.loss.push(LossEntry {
            path: loc.to_owned(),
            kind,
            code,
        });
    }

    fn mint_id(&mut self, prefix: &'static str) -> Result<String> {
        let id = self.mint.mint(prefix);
        match DomainId::parse(&id) {
            Ok(d) if d.prefix() == prefix => Ok(id),
            _ => Err(VectorError::InvalidPolicy("id_mint")),
        }
    }

    fn checkpoint(&self) -> Result<()> {
        self.cancel.check().map_err(|_| VectorError::Canceled)
    }

    fn spend_segments(&mut self, n: usize) -> Result<()> {
        self.segments = self.segments.saturating_add(n);
        if self.segments > self.limits.max_segments {
            return Err(VectorError::BudgetExceeded("segments"));
        }
        Ok(())
    }

    /// Links and embedded resources are never followed: only `#local` and `data:` targets are
    /// accepted (and neither is resolved by this slice).
    fn check_href(&mut self, el: Node, loc: &str) {
        for attr in el.attributes() {
            let linking =
                attr.name() == "href" && matches!(attr.namespace(), None | Some(XLINK_NS));
            if !linking {
                continue;
            }
            let v = attr.value().trim_start();
            if !(v.starts_with('#') || v.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:"))) {
                self.note(loc, LossKind::ExternalDenied, "external_href_denied");
            }
        }
    }

    fn walk_children(&mut self, node: Node, ctx: &Ctx, loc: &str) -> Result<()> {
        if ctx.depth >= self.limits.max_depth {
            return Err(VectorError::BudgetExceeded("depth"));
        }
        let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
        for child in node.children().filter(Node::is_element) {
            self.checkpoint()?;
            self.elements = self.elements.saturating_add(1);
            if self.elements > self.limits.max_nodes {
                return Err(VectorError::BudgetExceeded("nodes"));
            }
            let tag = child.tag_name().name();
            let n = counts.entry(tag).or_insert(0);
            *n += 1;
            let child_loc = format!("{loc}/{tag}[{n}]");
            self.element(child, ctx, &child_loc)?;
        }
        Ok(())
    }

    fn element(&mut self, el: Node, ctx: &Ctx, loc: &str) -> Result<()> {
        self.check_href(el, loc);
        if el.tag_name().namespace() != Some(SVG_NS) {
            self.note(loc, LossKind::Ignored, "foreign_namespace_element");
            return Ok(());
        }
        match el.tag_name().name() {
            "g" | "a" => self.group(el, ctx, loc),
            "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" => {
                self.shape(el, ctx, loc)
            }
            "defs" => self.defs(el, loc),
            // Gradients are resolved where a paint references them (see `gradient`).
            "linearGradient" | "radialGradient" => Ok(()),
            "title" | "desc" | "metadata" => {
                self.note(loc, LossKind::Ignored, "metadata");
                Ok(())
            }
            other => {
                self.note(loc, LossKind::Unsupported, unsupported_code(other));
                if matches!(other, "text" | "tspan")
                    && el.descendants().any(|d| d.has_tag_name("textPath"))
                {
                    self.note(loc, LossKind::Unsupported, "unsupported_text_on_path");
                }
                Ok(())
            }
        }
    }

    /// `defs` draws nothing. Gradients are looked up on use; every other child is reported.
    fn defs(&mut self, el: Node, loc: &str) -> Result<()> {
        let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
        for child in el.children().filter(Node::is_element) {
            self.checkpoint()?;
            self.elements = self.elements.saturating_add(1);
            if self.elements > self.limits.max_nodes {
                return Err(VectorError::BudgetExceeded("nodes"));
            }
            let tag = child.tag_name().name();
            let n = counts.entry(tag).or_insert(0);
            *n += 1;
            let child_loc = format!("{loc}/{tag}[{n}]");
            let svg = child.tag_name().namespace() == Some(SVG_NS);
            if svg && matches!(tag, "linearGradient" | "radialGradient") {
                continue;
            }
            self.check_href(child, &child_loc);
            if !svg {
                self.note(&child_loc, LossKind::Ignored, "foreign_namespace_element");
            } else if matches!(tag, "title" | "desc" | "metadata") {
                self.note(&child_loc, LossKind::Ignored, "metadata");
            } else {
                self.note(&child_loc, LossKind::Unsupported, unsupported_code(tag));
            }
        }
        Ok(())
    }

    /// Parses the element's own `transform`, if any, and records the bake.
    fn own_transform(&mut self, el: Node, loc: &str) -> Result<Option<Affine>> {
        let Some(text) = plain_attr(el, "transform") else {
            return Ok(None);
        };
        match svgtypes::Transform::from_str(text) {
            Ok(t) => {
                let m = Affine {
                    a: t.a,
                    b: t.b,
                    c: t.c,
                    d: t.d,
                    e: t.e,
                    f: t.f,
                };
                if !m.finite() {
                    return Err(VectorError::InvalidGeometry);
                }
                if !m.is_identity() {
                    self.note(loc, LossKind::TransformBaked, "transform_baked");
                }
                Ok(Some(m))
            }
            Err(_) => {
                self.note(loc, LossKind::Unsupported, "transform_unparsed");
                Ok(None)
            }
        }
    }

    /// Applies presentation attributes then inline `style` declarations to `st`.
    fn cascade(&mut self, el: Node, loc: &str, st: &mut Inherited) -> Local {
        let mut local = Local {
            opacity: 1.0,
            display_none: false,
        };
        for (name, value) in collect_props(el) {
            self.apply_prop(name, value, st, &mut local, loc);
        }
        local
    }

    fn paint(&mut self, value: &str, current: PaintValue, loc: &str) -> PaintValue {
        match svgtypes::Paint::from_str(value) {
            Ok(svgtypes::Paint::None) => PaintValue::None,
            Ok(svgtypes::Paint::Inherit) => current,
            Ok(svgtypes::Paint::CurrentColor) => PaintValue::Current,
            Ok(svgtypes::Paint::Color(c)) => {
                PaintValue::Color(c.red, c.green, c.blue, f64::from(c.alpha) / 255.0)
            }
            Ok(svgtypes::Paint::FuncIRI(id, fallback)) => PaintValue::Url(
                Rc::from(id),
                match fallback {
                    None => Fallback::Unspecified,
                    Some(svgtypes::PaintFallback::None) => Fallback::None,
                    Some(svgtypes::PaintFallback::CurrentColor) => Fallback::Current,
                    Some(svgtypes::PaintFallback::Color(c)) => {
                        Fallback::Color(c.red, c.green, c.blue, f64::from(c.alpha) / 255.0)
                    }
                },
            ),
            Ok(_) => {
                self.note(loc, LossKind::Unsupported, "unsupported_paint_context");
                current
            }
            Err(_) => {
                if value.contains("url(") {
                    self.note(loc, LossKind::ExternalDenied, "external_paint_denied");
                } else {
                    self.note(loc, LossKind::Unsupported, "paint_unparsed");
                }
                current
            }
        }
    }

    fn apply_prop(&mut self, name: &str, value: &str, st: &mut Inherited, local: &mut Local, loc: &str) {
        match name {
            "fill" => st.fill = self.paint(value, st.fill.clone(), loc),
            "stroke" => st.stroke = self.paint(value, st.stroke.clone(), loc),
            "stroke-width" => match svgtypes::Length::from_str(value).ok().and_then(unit_to_user) {
                Some(w) if w >= 0.0 && w.is_finite() => st.stroke_width = w,
                _ => self.note(loc, LossKind::Unsupported, "stroke_width_unparsed"),
            },
            "fill-rule" => match value {
                "nonzero" => st.fill_rule = Winding::NonZero,
                "evenodd" => st.fill_rule = Winding::EvenOdd,
                "inherit" => {}
                _ => self.note(loc, LossKind::Unsupported, "fill_rule_unparsed"),
            },
            "fill-opacity" => match fraction(value) {
                Some(v) => st.fill_opacity = v,
                None => self.note(loc, LossKind::Unsupported, "opacity_unparsed"),
            },
            "stroke-opacity" => match fraction(value) {
                Some(v) => st.stroke_opacity = v,
                None => self.note(loc, LossKind::Unsupported, "opacity_unparsed"),
            },
            "opacity" => match fraction(value) {
                Some(v) => local.opacity = v,
                None => self.note(loc, LossKind::Unsupported, "opacity_unparsed"),
            },
            "color" => {
                if let Ok(c) = svgtypes::Color::from_str(value) {
                    st.color = (c.red, c.green, c.blue, f64::from(c.alpha) / 255.0);
                } else {
                    self.note(loc, LossKind::Unsupported, "color_unparsed");
                }
            }
            "display" => local.display_none = value == "none",
            "visibility" => match value {
                "hidden" | "collapse" => st.visible = false,
                "visible" => st.visible = true,
                _ => {}
            },
            "stroke-linecap" => match value {
                "butt" => st.cap = StrokeCap::Butt,
                "round" => st.cap = StrokeCap::Round,
                "square" => st.cap = StrokeCap::Projecting,
                "inherit" => {}
                _ => self.note(loc, LossKind::Unsupported, "stroke_linecap_unparsed"),
            },
            "stroke-linejoin" => match value {
                "miter" => st.join = StrokeJoin::Miter,
                "round" => st.join = StrokeJoin::Round,
                "bevel" => st.join = StrokeJoin::Bevel,
                "miter-clip" | "arcs" => {
                    st.join = StrokeJoin::Miter;
                    self.note(loc, LossKind::Approximated, "stroke_linejoin_fallback_miter");
                }
                "inherit" => {}
                _ => self.note(loc, LossKind::Unsupported, "stroke_linejoin_unparsed"),
            },
            "stroke-miterlimit" => match value.parse::<f64>() {
                Ok(v) if v.is_finite() && v >= 1.0 => {
                    if v > MITER_LIMIT_MAX {
                        st.miter_limit = MITER_LIMIT_MAX;
                        self.note(loc, LossKind::Approximated, "stroke_miterlimit_clamped");
                    } else {
                        st.miter_limit = v;
                    }
                }
                _ if value == "inherit" => {}
                _ => self.note(loc, LossKind::Unsupported, "stroke_miterlimit_unparsed"),
            },
            "stroke-dasharray" => self.dash_array(value, st, loc),
            "stroke-dashoffset" => match svgtypes::Length::from_str(value).ok().and_then(unit_to_user) {
                Some(v) if v.is_finite() => st.dash_offset = v,
                _ if value == "inherit" => {}
                _ => self.note(loc, LossKind::Unsupported, "stroke_dashoffset_unparsed"),
            },
            "paint-order" => match value {
                "inherit" => {}
                _ => match paint_order_stroke_first(value) {
                    Some(first) => st.stroke_first = first,
                    None => self.note(loc, LossKind::Unsupported, "paint_order_unparsed"),
                },
            },
            "filter" if value != "none" => self.note(loc, LossKind::Unsupported, "unsupported_filter"),
            "mask" if value != "none" => self.note(loc, LossKind::Unsupported, "unsupported_mask"),
            "clip-path" if value != "none" => {
                self.note(loc, LossKind::Unsupported, "unsupported_clip_path");
            }
            "mix-blend-mode" if value != "normal" => {
                self.note(loc, LossKind::Unsupported, "unsupported_mix_blend_mode");
            }
            "marker-start" | "marker-mid" | "marker-end" if value != "none" => {
                self.note(loc, LossKind::Unsupported, "unsupported_marker");
            }
            _ => {}
        }
    }

    /// `stroke-dasharray`: lengths in user units. Negative or unparsable lists are invalid and
    /// ignored (solid), percentages are not resolved, an all-zero list is solid, an odd list is
    /// repeated once (the SVG rule).
    fn dash_array(&mut self, value: &str, st: &mut Inherited, loc: &str) {
        if value == "inherit" {
            return;
        }
        st.dash.clear();
        if value == "none" {
            return;
        }
        let mut list = Vec::new();
        for token in value
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
        {
            let parsed = svgtypes::Length::from_str(token).ok();
            if parsed.is_some_and(|l| l.unit == svgtypes::LengthUnit::Percent) {
                self.note(loc, LossKind::Unsupported, "unsupported_stroke_dasharray_percent");
                return;
            }
            match parsed.and_then(unit_to_user) {
                Some(v) if v >= 0.0 && v.is_finite() && list.len() < MAX_DASH_ENTRIES => list.push(v),
                _ => {
                    self.note(loc, LossKind::Unsupported, "stroke_dasharray_invalid");
                    return;
                }
            }
        }
        if list.iter().sum::<f64>() <= 0.0 {
            return;
        }
        if list.len() % 2 == 1 {
            list.extend_from_within(..);
        }
        if list.len() > MAX_DASH_ENTRIES {
            self.note(loc, LossKind::Unsupported, "stroke_dasharray_too_long");
            return;
        }
        st.dash = list;
    }

    fn layer_name(el: Node) -> String {
        let name = plain_attr(el, "id").unwrap_or_else(|| el.tag_name().name());
        name.chars().take(128).collect()
    }

    fn appearance(opacity: f64) -> Option<LayerAppearance> {
        (opacity < 1.0).then_some(LayerAppearance {
            opacity: opacity * 100.0,
            fill_opacity: 100.0,
            blend_mode: BLEND_NORMAL,
            clipped: false,
            isolation: Isolation::Isolated,
            mask: None,
        })
    }

    fn group(&mut self, el: Node, ctx: &Ctx, loc: &str) -> Result<()> {
        let mut inner = ctx.clone();
        let local = self.cascade(el, loc, &mut inner.st);
        if let Some(t) = self.own_transform(el, loc)? {
            inner.matrix = ctx.matrix.then(t);
        }
        let layer_id = self.mint_id("SLYR")?;
        self.layers.push(StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id: layer_id.clone(),
            name: Self::layer_name(el),
            parent: ctx.parent.clone(),
            kind: "group".into(),
            payload: Payload::Group,
            visible: !local.display_none && inner.st.visible,
            appearance: Self::appearance(local.opacity),
            text_frame: None,
        });
        inner.parent = Parent::Node(NodeRef {
            node_kind: NodeKind::Layer,
            node_id: layer_id,
        });
        inner.depth += 1;
        self.walk_children(el, &inner, loc)
    }

    fn len_attr(&mut self, el: Node, name: &str, loc: &str) -> f64 {
        let Some(v) = plain_attr(el, name) else {
            return 0.0;
        };
        match svgtypes::Length::from_str(v.trim()) {
            Ok(l) => unit_to_user(l).unwrap_or_else(|| {
                self.note(loc, LossKind::Unsupported, "unsupported_length_unit");
                0.0
            }),
            Err(_) => {
                self.note(loc, LossKind::Unsupported, "length_unparsed");
                0.0
            }
        }
    }

    fn shape(&mut self, el: Node, ctx: &Ctx, loc: &str) -> Result<()> {
        let mut inner = ctx.clone();
        let local = self.cascade(el, loc, &mut inner.st);
        if let Some(t) = self.own_transform(el, loc)? {
            inner.matrix = ctx.matrix.then(t);
        }
        let tag = el.tag_name().name();
        let mut b = SubpathBuilder::default();
        let mut no_fill = false;
        match tag {
            "path" => self.path_data(el, loc, &mut b)?,
            "rect" => {
                let (x, y) = (self.len_attr(el, "x", loc), self.len_attr(el, "y", loc));
                let (w, h) = (self.len_attr(el, "width", loc), self.len_attr(el, "height", loc));
                let mut rx = self.len_attr(el, "rx", loc).max(0.0);
                let mut ry = self.len_attr(el, "ry", loc).max(0.0);
                if plain_attr(el, "rx").is_none() {
                    rx = ry;
                }
                if plain_attr(el, "ry").is_none() {
                    ry = rx;
                }
                if w > 0.0 && h > 0.0 {
                    rect(&mut b, x, y, w, h, rx.min(w / 2.0), ry.min(h / 2.0));
                    if rx > 0.0 && ry > 0.0 {
                        self.note(loc, LossKind::Approximated, "rounded_rect_cubics");
                    }
                }
            }
            "circle" | "ellipse" => {
                let (cx, cy) = (self.len_attr(el, "cx", loc), self.len_attr(el, "cy", loc));
                let (rx, ry) = if tag == "circle" {
                    let r = self.len_attr(el, "r", loc);
                    (r, r)
                } else {
                    (self.len_attr(el, "rx", loc), self.len_attr(el, "ry", loc))
                };
                if rx > 0.0 && ry > 0.0 {
                    ellipse(&mut b, cx, cy, rx, ry);
                    self.note(loc, LossKind::Approximated, "ellipse_cubics");
                }
            }
            "line" => {
                let p1 = [self.len_attr(el, "x1", loc), self.len_attr(el, "y1", loc)];
                let p2 = [self.len_attr(el, "x2", loc), self.len_attr(el, "y2", loc)];
                b.move_to(p1);
                b.line_to(p2);
                no_fill = true;
            }
            _ => {
                let points = plain_attr(el, "points").unwrap_or("");
                let mut count = 0usize;
                for (x, y) in svgtypes::PointsParser::from(points) {
                    count += 1;
                    if count.is_multiple_of(512) {
                        self.checkpoint()?;
                    }
                    self.spend_segments(1)?;
                    if count == 1 {
                        b.move_to([x, y]);
                    } else {
                        b.line_to([x, y]);
                    }
                }
                if tag == "polygon" {
                    b.close();
                }
            }
        }
        let (subpaths, degenerate) = b.finish();
        if degenerate > 0 {
            self.note(loc, LossKind::Ignored, "degenerate_subpath");
        }
        if subpaths.is_empty() {
            return Ok(());
        }
        self.emit_shape(el, &inner, &local, subpaths, no_fill, loc)
    }

    fn path_data(&mut self, el: Node, loc: &str, b: &mut SubpathBuilder) -> Result<()> {
        use svgtypes::SimplePathSegment as S;
        let Some(d) = plain_attr(el, "d") else {
            return Ok(());
        };
        if d.bytes().any(|c| c == b'a' || c == b'A') {
            self.note(loc, LossKind::Approximated, "arc_to_cubic");
        }
        let mut count = 0usize;
        for seg in svgtypes::SimplifyingPathParser::from(d) {
            count += 1;
            if count.is_multiple_of(256) {
                self.checkpoint()?;
            }
            self.spend_segments(1)?;
            match seg {
                Ok(S::MoveTo { x, y }) => b.move_to([x, y]),
                Ok(S::LineTo { x, y }) => b.line_to([x, y]),
                Ok(S::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x,
                    y,
                }) => b.cubic_to([x1, y1], [x2, y2], [x, y]),
                Ok(S::Quadratic { x1, y1, x, y }) => b.quad_to([x1, y1], [x, y]),
                Ok(S::ClosePath) => b.close(),
                Err(_) => {
                    // SVG error handling: the path is rendered up to the error.
                    self.note(loc, LossKind::Unsupported, "path_data_error");
                    break;
                }
            }
        }
        Ok(())
    }

    /// Resolves a computed paint to a Folio paint plus the row opacity it needs. `opacity` is the
    /// element's fill or stroke opacity: a solid paint carries it as the row opacity, a gradient
    /// folds it into every stop opacity (exact: opacity multiplies the interpolated colour).
    fn resolve_paint(
        &mut self,
        pv: &PaintValue,
        st: &Inherited,
        opacity: f64,
        bbox: Option<[f64; 4]>,
        m: Affine,
        loc: &str,
    ) -> Result<Option<ResolvedPaint>> {
        let solid = |(r, g, b, a): (u8, u8, u8, f64)| {
            Some(ResolvedPaint::solid(
                (r, g, b),
                (a * opacity).clamp(0.0, 1.0),
            ))
        };
        Ok(match pv {
            PaintValue::None => None,
            PaintValue::Current => solid(st.color),
            PaintValue::Color(r, g, b, a) => solid((*r, *g, *b, *a)),
            PaintValue::Url(id, fallback) => match self.gradient(id, opacity, bbox, m, loc)? {
                GradientOutcome::Paint(p) => Some(p),
                GradientOutcome::Nothing => None,
                GradientOutcome::Missing => {
                    self.note(loc, LossKind::Unsupported, "unsupported_paint_server");
                    match fallback {
                        Fallback::Color(r, g, b, a) => solid((*r, *g, *b, *a)),
                        Fallback::Current => solid(st.color),
                        Fallback::None | Fallback::Unspecified => None,
                    }
                }
            },
        })
    }

    /// Resolves `url(#id)` to a gradient. Follows `href` chains inside the file only (depth 8,
    /// cycles refused); attributes and stops are inherited along the chain as SVG defines.
    fn gradient(
        &mut self,
        id: &str,
        opacity: f64,
        bbox: Option<[f64; 4]>,
        m: Affine,
        loc: &str,
    ) -> Result<GradientOutcome> {
        let Some(&first) = self.gradients.get(id) else {
            return Ok(GradientOutcome::Missing);
        };
        let mut chain: Vec<Node<'d, 'd>> = vec![first];
        while chain.len() < MAX_GRADIENT_CHAIN {
            let current = chain[chain.len() - 1];
            let href = current
                .attributes()
                .find(|a| a.name() == "href" && matches!(a.namespace(), None | Some(XLINK_NS)))
                .map(|a| a.value().trim());
            let Some(href) = href else { break };
            let Some(target) = href.strip_prefix('#') else {
                self.note(loc, LossKind::ExternalDenied, "external_href_denied");
                break;
            };
            let Some(&next) = self.gradients.get(target) else {
                break;
            };
            if chain.iter().any(|n| n.id() == next.id()) {
                self.note(loc, LossKind::Unsupported, "gradient_href_cycle");
                break;
            }
            chain.push(next);
        }
        let kind = first.tag_name().name();
        let attr = |name: &str, same_kind: bool| -> Option<&'d str> {
            chain
                .iter()
                .filter(|n| !same_kind || n.tag_name().name() == kind)
                .find_map(|n| plain_attr(*n, name))
        };
        let units = match attr("gradientUnits", false) {
            Some("userSpaceOnUse") => GradientUnits::UserSpaceOnUse,
            _ => GradientUnits::ObjectBoundingBox,
        };
        let spread = match attr("spreadMethod", false) {
            None | Some("pad") => GradientSpread::Pad,
            Some("reflect") => GradientSpread::Reflect,
            Some("repeat") => GradientSpread::Repeat,
            Some(_) => {
                self.note(loc, LossKind::Unsupported, "gradient_spread_unparsed");
                GradientSpread::Pad
            }
        };
        let transform = match attr("gradientTransform", false).map(svgtypes::Transform::from_str) {
            None => Affine::IDENTITY,
            Some(Ok(t)) => Affine {
                a: t.a,
                b: t.b,
                c: t.c,
                d: t.d,
                e: t.e,
                f: t.f,
            },
            Some(Err(_)) => {
                self.note(loc, LossKind::Unsupported, "gradient_transform_unparsed");
                Affine::IDENTITY
            }
        };
        if !transform.finite() {
            return Err(VectorError::InvalidGeometry);
        }
        let stop_owner = chain
            .iter()
            .find(|n| n.children().any(|c| is_svg_stop(c)))
            .copied();
        let stops = match stop_owner {
            Some(owner) => self.stops(owner, opacity, loc)?,
            None => Vec::new(),
        };
        let Some(last) = stops.last().copied() else {
            self.note(loc, LossKind::Ignored, "gradient_no_stops");
            return Ok(GradientOutcome::Nothing);
        };
        if stops.len() == 1 {
            return Ok(GradientOutcome::Paint(last.solid()));
        }
        let view = self.user_viewport;
        let coord = |imp: &mut Self, name: &str, default_percent: f64, axis: usize| -> f64 {
            let text = attr(name, true);
            imp.gradient_coordinate(text, default_percent, units, axis, view, loc)
        };
        enum Geometry {
            Linear([f64; 4]),
            Radial([f64; 6]),
        }
        let geometry = if kind == "linearGradient" {
            let g = [
                coord(self, "x1", 0.0, 0),
                coord(self, "y1", 0.0, 1),
                coord(self, "x2", 100.0, 0),
                coord(self, "y2", 0.0, 1),
            ];
            if g[0] == g[2] && g[1] == g[3] {
                return Ok(GradientOutcome::Paint(last.solid()));
            }
            Geometry::Linear(g)
        } else {
            let (cx, cy) = (coord(self, "cx", 50.0, 0), coord(self, "cy", 50.0, 1));
            let r = coord(self, "r", 50.0, 2);
            let fx = if attr("fx", true).is_some() { coord(self, "fx", 50.0, 0) } else { cx };
            let fy = if attr("fy", true).is_some() { coord(self, "fy", 50.0, 1) } else { cy };
            let fr = coord(self, "fr", 0.0, 2);
            if r < 0.0 || fr < 0.0 {
                self.note(loc, LossKind::Unsupported, "gradient_invalid_radius");
                return Ok(GradientOutcome::Nothing);
            }
            if r == 0.0 {
                return Ok(GradientOutcome::Paint(last.solid()));
            }
            if (fx - cx).hypot(fy - cy) > r + 1e-12 {
                self.note(loc, LossKind::Approximated, "radial_focal_outside_circle");
            }
            Geometry::Radial([cx, cy, r, fx, fy, fr])
        };
        let full = match units {
            GradientUnits::ObjectBoundingBox => {
                let Some([x0, y0, x1, y1]) = bbox.filter(|b| b[2] > b[0] && b[3] > b[1]) else {
                    self.note(loc, LossKind::Unsupported, "gradient_degenerate_bbox");
                    return Ok(GradientOutcome::Nothing);
                };
                let map = Affine {
                    a: x1 - x0,
                    b: 0.0,
                    c: 0.0,
                    d: y1 - y0,
                    e: x0,
                    f: y0,
                };
                m.then(map).then(transform)
            }
            GradientUnits::UserSpaceOnUse => m.then(transform),
        };
        if !full.finite() || (full.a * full.d - full.b * full.c).abs() < 1e-18 {
            self.note(loc, LossKind::Unsupported, "gradient_singular_transform");
            return Ok(GradientOutcome::Nothing);
        }
        let geometry = match geometry {
            Geometry::Linear([x1, y1, x2, y2]) => StudioGradientGeometry::Linear { x1, y1, x2, y2 },
            Geometry::Radial([cx, cy, r, fx, fy, fr]) => StudioGradientGeometry::Radial {
                cx,
                cy,
                r,
                fx,
                fy,
                fr,
            },
        };
        Ok(GradientOutcome::Paint(ResolvedPaint {
            paint: StudioPaint::Gradient {
                gradient: StudioGradient {
                    geometry,
                    transform: [full.a, full.b, full.c, full.d, full.e, full.f],
                    spread,
                    interpolation: GradientInterpolation::Classic,
                    dither: false,
                    stops: stops.iter().map(Stop::record).collect(),
                },
            },
            opacity: 100.0,
            units: Some(units),
        }))
    }

    /// A gradient coordinate in the gradient's own space. Percentages are fractions for
    /// `objectBoundingBox` and a share of the viewport for `userSpaceOnUse`.
    fn gradient_coordinate(
        &mut self,
        text: Option<&str>,
        default_percent: f64,
        units: GradientUnits,
        axis: usize,
        view: [f64; 2],
        loc: &str,
    ) -> f64 {
        let basis = match axis {
            0 => view[0],
            1 => view[1],
            _ => ((view[0] * view[0] + view[1] * view[1]) / 2.0).sqrt(),
        };
        let percent = |p: f64| match units {
            GradientUnits::ObjectBoundingBox => p / 100.0,
            GradientUnits::UserSpaceOnUse => p / 100.0 * basis,
        };
        let Some(text) = text else {
            return percent(default_percent);
        };
        match svgtypes::Length::from_str(text.trim()) {
            Ok(l) if l.unit == svgtypes::LengthUnit::Percent && l.number.is_finite() => percent(l.number),
            Ok(l) if l.unit == svgtypes::LengthUnit::None && l.number.is_finite() => l.number,
            Ok(l) => unit_to_user(l).filter(|v| v.is_finite()).unwrap_or_else(|| {
                self.note(loc, LossKind::Unsupported, "unsupported_length_unit");
                percent(default_percent)
            }),
            Err(_) => {
                self.note(loc, LossKind::Unsupported, "length_unparsed");
                percent(default_percent)
            }
        }
    }

    /// Stops of one gradient element: offsets clamped to `0..=1` and made non-decreasing, colour
    /// alpha folded with `stop-opacity` and the painted element's opacity.
    fn stops(&mut self, owner: Node, opacity: f64, loc: &str) -> Result<Vec<Stop>> {
        let mut out = Vec::new();
        let mut previous = 0.0_f64;
        for stop in owner.children().filter(|c| is_svg_stop(*c)) {
            if out.len() >= MAX_GRADIENT_STOPS {
                return Err(VectorError::BudgetExceeded("gradient_stops"));
            }
            let mut offset = match plain_attr(stop, "offset") {
                Some(text) => fraction(text).unwrap_or_else(|| {
                    self.note(loc, LossKind::Unsupported, "stop_offset_unparsed");
                    0.0
                }),
                None => 0.0,
            };
            offset = offset.max(previous);
            previous = offset;
            let mut color = (0u8, 0u8, 0u8, 1.0_f64);
            let mut stop_opacity = 1.0_f64;
            for (name, value) in collect_props(stop) {
                match name {
                    "stop-color" if value == "currentColor" => {
                        self.note(loc, LossKind::Approximated, "stop_color_current_color");
                    }
                    "stop-color" => match svgtypes::Color::from_str(value) {
                        Ok(c) => color = (c.red, c.green, c.blue, f64::from(c.alpha) / 255.0),
                        Err(_) => self.note(loc, LossKind::Unsupported, "stop_color_unparsed"),
                    },
                    "stop-opacity" => match fraction(value) {
                        Some(v) => stop_opacity = v,
                        None => self.note(loc, LossKind::Unsupported, "opacity_unparsed"),
                    },
                    _ => {}
                }
            }
            out.push(Stop {
                offset,
                rgb: (color.0, color.1, color.2),
                alpha: (color.3 * stop_opacity * opacity).clamp(0.0, 1.0),
            });
        }
        Ok(out)
    }

    /// Charges `n` records against the design budget (Folio's `Budget::nodes` units).
    fn spend_design(&mut self, n: usize) -> Result<()> {
        self.design_units = self.design_units.saturating_add(n);
        if self.design_units > design_unit_cap(&self.limits) {
            return Err(VectorError::BudgetExceeded("design_records"));
        }
        Ok(())
    }

    /// Stores an appearance stack once as a Folio `PAINT` style; identical stacks (and identical
    /// gradient provenance) share one style id.
    fn style_id(
        &mut self,
        rows: Vec<StudioAppearanceRow>,
        units: Vec<(usize, GradientUnits)>,
    ) -> Result<String> {
        let key = format!("{rows:?}{units:?}");
        if let Some(&i) = self.style_index.get(&key) {
            return Ok(self.styles[i].style_id.clone());
        }
        let cost = rows.iter().fold(1, |n, row| n + 1 + row_weight(row));
        self.spend_design(cost)?;
        let style_id = self.mint_id("SSTY")?;
        let ordinal = self.styles.len() + 1;
        self.style_index.insert(key, self.styles.len());
        self.gradient_provenance
            .extend(units.into_iter().map(|(row, units)| GradientProvenance {
                style_id: style_id.clone(),
                row,
                units,
            }));
        self.styles.push(StudioPaintStyle {
            schema_id: StyleRegistrySchema::V1,
            style_id: style_id.clone(),
            kind: StyleKind::Paint,
            name: format!("svg/{ordinal}"),
            publishing: StudioPublishing {
                description: String::new(),
                description_markdown: String::new(),
                documentation_links: Vec::new(),
                publish_key: format!("svg-import/{ordinal}"),
                is_remote: false,
            },
            rows,
        });
        Ok(style_id)
    }

    fn emit_shape(
        &mut self,
        el: Node,
        ctx: &Ctx,
        local: &Local,
        subpaths: Vec<RawSubpath>,
        no_fill: bool,
        loc: &str,
    ) -> Result<()> {
        let m = ctx.matrix;
        if !m.uniform() {
            self.note(loc, LossKind::Approximated, "stroke_width_nonuniform_scale");
        }
        let bbox = user_bbox(&subpaths);
        let st = &ctx.st;
        let fill = if no_fill {
            None
        } else {
            self.resolve_paint(&st.fill, st, st.fill_opacity, bbox, m, loc)?
        };
        let stroke = if st.stroke_width > 0.0 {
            self.resolve_paint(&st.stroke, st, st.stroke_opacity, bbox, m, loc)?
        } else {
            None
        };
        let scale = m.mean_scale();
        let stroke_row = stroke
            .map(|paint| -> Result<_> {
                let length = |v: f64| -> Result<FolioLength> {
                    if v.is_finite() {
                        Ok(pt(v))
                    } else {
                        Err(VectorError::InvalidGeometry)
                    }
                };
                let style = StudioStrokeStyle {
                    weight: length(st.stroke_width * scale)?,
                    cap: st.cap,
                    join: st.join,
                    align: StrokeAlign::Center,
                    miter_limit: st.miter_limit,
                    dash: st
                        .dash
                        .iter()
                        .map(|d| length(d * scale))
                        .collect::<Result<_>>()?,
                    dash_offset: length(st.dash_offset * scale)?,
                };
                let units = paint.units;
                Ok((paint.stroke_row(style), units))
            })
            .transpose()?;
        let winding = if fill.is_some() {
            st.fill_rule
        } else {
            Winding::None
        };
        let fill_row = fill.map(|paint| {
            let units = paint.units;
            (paint.fill_row(), units)
        });
        let mut ordered = Vec::with_capacity(2);
        if st.stroke_first {
            ordered.extend(stroke_row);
            ordered.extend(fill_row);
        } else {
            ordered.extend(fill_row);
            ordered.extend(stroke_row);
        }
        let mut rows = Vec::with_capacity(ordered.len());
        let mut units = Vec::new();
        for (index, (row, row_units)) in ordered.into_iter().enumerate() {
            rows.push(row);
            units.extend(row_units.map(|u| (index, u)));
        }
        let style_id = if rows.is_empty() {
            None
        } else {
            Some(self.style_id(rows, units)?)
        };
        let layer_id = self.mint_id("SLYR")?;
        let mut path_ids = Vec::with_capacity(subpaths.len());
        for sub in subpaths {
            let mut anchors = Vec::with_capacity(sub.anchors.len());
            for a in &sub.anchors {
                let (pos, cin, cout) = (m.apply(a.pos), m.apply(a.cin), m.apply(a.cout));
                for v in [pos, cin, cout].iter().flatten() {
                    if !v.is_finite() || v.abs() > COORDINATE_LIMIT {
                        return Err(VectorError::InvalidGeometry);
                    }
                }
                anchors.push(Anchor {
                    position: point(pos)?,
                    incoming: point([cin[0] - pos[0], cin[1] - pos[1]])?,
                    outgoing: point([cout[0] - pos[0], cout[1] - pos[1]])?,
                    handle_mirroring: Mirroring::None,
                });
            }
            let path_id = self.mint_id("SVPT")?;
            path_ids.push(path_id.clone());
            self.paths.push(VectorPath {
                path_id,
                layer_id: layer_id.clone(),
                closed: sub.closed,
                anchors,
                winding,
            });
        }
        if let Some(style_id) = style_id {
            self.spend_design(1)?;
            self.nodes.push(StudioNodeDesign {
                node: NodeRef {
                    node_kind: NodeKind::Layer,
                    node_id: layer_id.clone(),
                },
                auto_layout: None,
                child_layout: None,
                size_clamps: None,
                constraints: None,
                reactions: Vec::new(),
                component_property_references: BTreeMap::new(),
                explicit_variable_modes: BTreeMap::new(),
                style_refs: BTreeMap::from([(LAYER_STYLE_REF.to_owned(), style_id)]),
            });
        }
        self.layers.push(StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id,
            name: Self::layer_name(el),
            parent: ctx.parent.clone(),
            kind: "vector".into(),
            payload: Payload::Vector { path_ids },
            visible: !local.display_none && ctx.st.visible,
            appearance: Self::appearance(local.opacity),
            text_frame: None,
        });
        Ok(())
    }
}

/// A gradient stop before it becomes a Folio record: offset `0..=1`, straight alpha `0..=1`.
#[derive(Clone, Copy, Debug)]
struct Stop {
    offset: f64,
    rgb: (u8, u8, u8),
    alpha: f64,
}

impl Stop {
    fn record(&self) -> StudioGradientStop {
        StudioGradientStop {
            position: clean(self.offset * 100.0).clamp(0.0, 100.0),
            midpoint: None,
            opacity: clean(self.alpha * 100.0).clamp(0.0, 100.0),
            colour: colour(self.rgb),
        }
    }

    /// A one-stop gradient is that colour.
    fn solid(&self) -> ResolvedPaint {
        ResolvedPaint::solid(self.rgb, self.alpha)
    }
}

/// A paint ready for a Folio appearance row. A gradient keeps the element opacity in its stops, so
/// its row opacity is 100; a solid paint carries it as the row opacity.
struct ResolvedPaint {
    paint: StudioPaint,
    /// Row opacity, percent.
    opacity: f64,
    /// SVG gradient units, when the paint is a gradient.
    units: Option<GradientUnits>,
}

impl ResolvedPaint {
    fn solid(rgb: (u8, u8, u8), alpha: f64) -> Self {
        Self {
            paint: StudioPaint::Solid {
                colour: colour(rgb),
            },
            opacity: clean(alpha * 100.0).clamp(0.0, 100.0),
            units: None,
        }
    }

    fn fill_row(self) -> StudioAppearanceRow {
        StudioAppearanceRow::Fill {
            paint: self.paint,
            opacity: self.opacity,
            blend_mode: BLEND_NORMAL,
            visible: true,
        }
    }

    fn stroke_row(self, stroke: StudioStrokeStyle) -> StudioAppearanceRow {
        StudioAppearanceRow::Stroke {
            paint: self.paint,
            stroke,
            opacity: self.opacity,
            blend_mode: BLEND_NORMAL,
            visible: true,
        }
    }
}

/// An sRGB-intent channel triple as an opaque Folio colour in the document working space.
fn colour((r, g, b): (u8, u8, u8)) -> StudioColor {
    StudioColor {
        r: f64::from(r) / 255.0,
        g: f64::from(g) / 255.0,
        b: f64::from(b) / 255.0,
        colour_profile_id: ProfileBinding::Unassigned(()),
    }
}

/// Rounds away the binary noise of `fraction * 100` (to 1e-10), so a percent re-divided by 100 on
/// export reads back as the same value and the round trip is a fixed point. Monotonic.
fn clean(v: f64) -> f64 {
    (v * 1e10).round() / 1e10
}

/// Folio meters one record per gradient stop and one per dash/gap pair of a row.
fn row_weight(row: &StudioAppearanceRow) -> usize {
    let (paint, dashes) = match row {
        StudioAppearanceRow::Fill { paint, .. } => (paint, 0),
        StudioAppearanceRow::Stroke { paint, stroke, .. } => (paint, stroke.dash.len() / 2),
    };
    dashes
        + match paint {
            StudioPaint::Solid { .. } => 0,
            StudioPaint::Gradient { gradient } => gradient.stops.len(),
        }
}

enum GradientOutcome {
    Paint(ResolvedPaint),
    /// The reference resolves but paints nothing (no stops, degenerate); already reported.
    Nothing,
    /// No gradient has this id.
    Missing,
}

const MITER_LIMIT_MAX: f64 = 500.0;
const MAX_GRADIENT_CHAIN: usize = 8;

fn is_svg_stop(n: Node) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(SVG_NS) && n.tag_name().name() == "stop"
}

/// Presentation attributes followed by inline `style` declarations, in cascade order.
fn collect_props<'a>(el: Node<'a, '_>) -> Vec<(&'a str, &'a str)> {
    let mut props: Vec<(&str, &str)> = el
        .attributes()
        .filter(|a| a.namespace().is_none())
        .map(|a| (a.name(), a.value().trim()))
        .collect();
    if let Some(style) = plain_attr(el, "style") {
        for decl in style.split(';') {
            if let Some((name, value)) = decl.split_once(':') {
                let value = value.trim();
                let value = value.strip_suffix("!important").map_or(value, str::trim_end);
                props.push((name.trim(), value));
            }
        }
    }
    props
}

/// `paint-order`: true when the stroke is painted before the fill. `None` for unknown keywords.
fn paint_order_stroke_first(value: &str) -> Option<bool> {
    let mut order: Vec<&str> = Vec::new();
    for token in value.split_whitespace() {
        if !matches!(token, "normal" | "fill" | "stroke" | "markers") || order.contains(&token) {
            return None;
        }
        order.push(token);
    }
    if order.first() == Some(&"normal") {
        return (order.len() == 1).then_some(false);
    }
    for default in ["fill", "stroke", "markers"] {
        if !order.contains(&default) {
            order.push(default);
        }
    }
    let at = |k: &str| order.iter().position(|t| *t == k);
    Some(at("stroke") < at("fill"))
}

/// Tight bounding box `[x0, y0, x1, y1]` of all subpaths in user space (curve extrema included).
fn user_bbox(subpaths: &[RawSubpath]) -> Option<[f64; 4]> {
    let mut b: Option<[f64; 4]> = None;
    let mut extend = |p: [f64; 2]| {
        b = Some(match b {
            None => [p[0], p[1], p[0], p[1]],
            Some(c) => [c[0].min(p[0]), c[1].min(p[1]), c[2].max(p[0]), c[3].max(p[1])],
        });
    };
    for sub in subpaths {
        let n = sub.anchors.len();
        let segments = if sub.closed { n } else { n.saturating_sub(1) };
        for a in &sub.anchors {
            extend(a.pos);
        }
        for i in 0..segments {
            let (p, q) = (&sub.anchors[i], &sub.anchors[(i + 1) % n]);
            if p.cout == p.pos && q.cin == q.pos {
                continue;
            }
            for axis in 0..2 {
                let (p0, c1, c2, p1) = (p.pos[axis], p.cout[axis], q.cin[axis], q.pos[axis]);
                let (d0, d1, d2) = (c1 - p0, c2 - c1, p1 - c2);
                // B'(t) / 3 = a t^2 + b t + c
                let (a, bq, c) = (d0 - 2.0 * d1 + d2, 2.0 * (d1 - d0), d0);
                let mut roots = [f64::NAN; 2];
                if a.abs() < 1e-12 {
                    if bq.abs() > 1e-12 {
                        roots[0] = -c / bq;
                    }
                } else {
                    let disc = bq * bq - 4.0 * a * c;
                    if disc >= 0.0 {
                        let s = disc.sqrt();
                        roots = [(-bq + s) / (2.0 * a), (-bq - s) / (2.0 * a)];
                    }
                }
                for t in roots {
                    if t > 0.0 && t < 1.0 {
                        let u = 1.0 - t;
                        let at = |k: usize| {
                            let (p0, c1, c2, p1) = (p.pos[k], p.cout[k], q.cin[k], q.pos[k]);
                            u * u * u * p0 + 3.0 * u * u * t * c1 + 3.0 * u * t * t * c2 + t * t * t * p1
                        };
                        extend([at(0), at(1)]);
                    }
                }
            }
        }
    }
    b
}

fn point(v: [f64; 2]) -> Result<Point> {
    Point::pt(v[0], v[1]).map_err(|_| VectorError::InvalidGeometry)
}

fn unsupported_code(tag: &str) -> &'static str {
    match tag {
        "defs" => "unsupported_defs",
        "use" => "unsupported_use",
        "symbol" => "unsupported_symbol",
        "image" => "unsupported_image",
        "text" | "tspan" | "textPath" => "unsupported_text",
        "filter" => "unsupported_filter",
        "mask" => "unsupported_mask",
        "clipPath" => "unsupported_clip_path",
        "linearGradient" | "radialGradient" | "meshgradient" => "unsupported_gradient",
        "pattern" => "unsupported_pattern",
        "marker" => "unsupported_marker",
        "style" => "unsupported_css_stylesheet",
        "script" => "script_not_executed",
        "foreignObject" => "unsupported_foreign_object",
        "switch" => "unsupported_switch",
        "svg" => "unsupported_nested_svg",
        "animate" | "animateTransform" | "animateMotion" | "set" => "unsupported_animation",
        _ => "unsupported_element",
    }
}

fn rect(b: &mut SubpathBuilder, x: f64, y: f64, w: f64, h: f64, rx: f64, ry: f64) {
    if rx > 0.0 && ry > 0.0 {
        let (kx, ky) = (KAPPA * rx, KAPPA * ry);
        let (x1, y1) = (x + w, y + h);
        b.move_to([x + rx, y]);
        b.line_to([x1 - rx, y]);
        b.cubic_to([x1 - rx + kx, y], [x1, y + ry - ky], [x1, y + ry]);
        b.line_to([x1, y1 - ry]);
        b.cubic_to([x1, y1 - ry + ky], [x1 - rx + kx, y1], [x1 - rx, y1]);
        b.line_to([x + rx, y1]);
        b.cubic_to([x + rx - kx, y1], [x, y1 - ry + ky], [x, y1 - ry]);
        b.line_to([x, y + ry]);
        b.cubic_to([x, y + ry - ky], [x + rx - kx, y], [x + rx, y]);
    } else {
        b.move_to([x, y]);
        b.line_to([x + w, y]);
        b.line_to([x + w, y + h]);
        b.line_to([x, y + h]);
    }
    b.close();
}

/// Four cubics starting at `(cx + rx, cy)` and running toward `+y` (SVG circle direction).
fn ellipse(b: &mut SubpathBuilder, cx: f64, cy: f64, rx: f64, ry: f64) {
    let (kx, ky) = (KAPPA * rx, KAPPA * ry);
    b.move_to([cx + rx, cy]);
    b.cubic_to([cx + rx, cy + ky], [cx + kx, cy + ry], [cx, cy + ry]);
    b.cubic_to([cx - kx, cy + ry], [cx - rx, cy + ky], [cx - rx, cy]);
    b.cubic_to([cx - rx, cy - ky], [cx - kx, cy - ry], [cx, cy - ry]);
    b.cubic_to([cx + kx, cy - ry], [cx + rx, cy - ky], [cx + rx, cy]);
    b.close();
}
