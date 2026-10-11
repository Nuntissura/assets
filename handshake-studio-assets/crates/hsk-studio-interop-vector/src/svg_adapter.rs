//! SVG import into a Folio proposal plus nib anchor geometry, and deterministic SVG export of the
//! supported subset. Input is hostile: DTD internal subsets are refused, no link is ever
//! followed, and node, depth, segment and byte budgets are enforced while decoding.
use crate::error::{Result, VectorError};
use crate::ids::IdMint;
use crate::model::*;
use hsk_studio_accord::{CancellationToken, DomainId, Length as AccordLength, Unit};
use hsk_studio_folio::{
    ArtboardSchema, Bounds, Budget, DocumentSchema, GeometryUnit, GraphSchema, Isolation,
    LayerAppearance, LayerSchema, Length as FolioLength, NodeKind, NodeRef, Parent, Payload,
    ProfileBinding, Resolution, Resolver, StudioArtboard,
    StudioDocument, StudioLayer, StudioLayerGraph, TileRef, validate_document,
};
use hsk_studio_nib::{Anchor, Mirroring, Point, Winding};
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
/// Cubic Bezier control distance for a quarter ellipse: `4/3 * (sqrt(2) - 1)`.
const KAPPA: f64 = 0.552_284_749_830_793_4;

const BLEND_NORMAL: u16 = 2;
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

#[derive(Clone, Copy, Debug, PartialEq)]
enum PaintValue {
    None,
    Current,
    Color(u8, u8, u8, f64),
}

#[derive(Clone, Copy, Debug)]
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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StyleKey {
    fill: Option<(u8, u8, u8, u64)>,
    stroke: Option<(u8, u8, u8, u64, u64)>,
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

struct Importer<'a> {
    limits: Limits,
    cancel: &'a CancellationToken,
    mint: &'a mut dyn IdMint,
    loss: Vec<LossEntry>,
    layers: Vec<StudioLayer>,
    paths: Vec<VectorPath>,
    styles: Vec<StyleProposal>,
    style_index: BTreeMap<StyleKey, usize>,
    segments: usize,
    elements: u32,
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

    let mut imp = Importer {
        limits: policy.limits,
        cancel,
        mint,
        loss,
        layers: Vec::new(),
        paths: Vec::new(),
        styles: Vec::new(),
        style_index: BTreeMap::new(),
        segments: 0,
        elements: 1,
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
    let mut base = Affine::scale(px_to_pt, px_to_pt);
    if let Some(vb) = view_box {
        let par = plain_attr(root, "preserveAspectRatio")
            .and_then(|v| svgtypes::AspectRatio::from_str(v).ok())
            .unwrap_or_default();
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
        .map_err(|d| {
            if d.code == hsk_studio_folio::Code::Canceled {
                VectorError::Canceled
            } else {
                VectorError::Folio(d.code)
            }
        })?;
    drop(known);
    Ok(crate::model::VectorProposal {
        snapshot,
        paths,
        styles,
        loss,
        provenance: Provenance {
            source_format: "svg",
            source_unit: "px",
            pixels_per_inch: policy.pixels_per_inch,
            px_to_pt,
            source_len: bytes.len(),
            view_box: view_box.map(|v| [v.x, v.y, v.w, v.h]),
            viewport_px: [vw, vh],
        },
    })
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

impl Importer<'_> {
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
        let mut props: Vec<(&str, &str)> = el
            .attributes()
            .filter(|a| a.namespace().is_none())
            .map(|a| (a.name(), a.value()))
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
        for (name, value) in props {
            self.apply_prop(name, value.trim(), st, &mut local, loc);
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
            Ok(svgtypes::Paint::FuncIRI(..)) => {
                self.note(loc, LossKind::Unsupported, "unsupported_paint_server");
                PaintValue::None
            }
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
            "fill" => st.fill = self.paint(value, st.fill, loc),
            "stroke" => st.stroke = self.paint(value, st.stroke, loc),
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
            "stroke-linecap" if value != "butt" => {
                self.note(loc, LossKind::Unsupported, "unsupported_stroke_linecap");
            }
            "stroke-linejoin" if value != "miter" => {
                self.note(loc, LossKind::Unsupported, "unsupported_stroke_linejoin");
            }
            "stroke-miterlimit" if value != "4" => {
                self.note(loc, LossKind::Unsupported, "unsupported_stroke_miterlimit");
            }
            "stroke-dasharray" if value != "none" => {
                self.note(loc, LossKind::Unsupported, "unsupported_stroke_dasharray");
            }
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
            "paint-order" if value != "normal" => {
                self.note(loc, LossKind::Unsupported, "unsupported_paint_order");
            }
            _ => {}
        }
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

    fn resolve(&self, p: PaintValue, st: &Inherited, opacity: f64) -> Option<Rgba> {
        let (r, g, b, a) = match p {
            PaintValue::None => return None,
            PaintValue::Current => st.color,
            PaintValue::Color(r, g, b, a) => (r, g, b, a),
        };
        Some(Rgba {
            r,
            g,
            b,
            a: (a * opacity).clamp(0.0, 1.0),
        })
    }

    fn style_id(&mut self, data: (Option<Rgba>, Option<StrokePaint>)) -> Result<String> {
        let key = StyleKey {
            fill: data.0.map(|c| (c.r, c.g, c.b, c.a.to_bits())),
            stroke: data
                .1
                .map(|s| (s.color.r, s.color.g, s.color.b, s.color.a.to_bits(), s.width_pt.to_bits())),
        };
        if let Some(&i) = self.style_index.get(&key) {
            return Ok(self.styles[i].style_id.clone());
        }
        let style_id = self.mint_id("SSTY")?;
        self.style_index.insert(key, self.styles.len());
        self.styles.push(StyleProposal {
            style_id: style_id.clone(),
            fill: data.0,
            stroke: data.1,
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
        let fill = if no_fill {
            None
        } else {
            self.resolve(ctx.st.fill, &ctx.st, ctx.st.fill_opacity)
        };
        let stroke = self
            .resolve(ctx.st.stroke, &ctx.st, ctx.st.stroke_opacity)
            .filter(|_| ctx.st.stroke_width > 0.0)
            .map(|color| StrokePaint {
                color,
                width_pt: ctx.st.stroke_width * m.mean_scale(),
            });
        let winding = if fill.is_some() {
            ctx.st.fill_rule
        } else {
            Winding::None
        };
        let style_id = if fill.is_some() || stroke.is_some() {
            Some(self.style_id((fill, stroke))?)
        } else {
            None
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
                fill_style_id: style_id.clone(),
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
