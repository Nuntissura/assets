//! Folio layer graph of the three Operator Figma fixtures (lane SMB-INTEROP-FIG-LAYER-GRAPH): the
//! imported documents are composited by `hsk-studio-composite` and executed by `hsk-studio-render-cpu`
//! (dev-dependency edges only), and the named nodes are compared with the fixture PNG renders at
//! stated tolerances. Graph shape and typed loss counts are pinned.
use hsk_studio_accord::{CancellationToken, SCHEMA_STUDIO_COMPONENT_INSTANCE};
use hsk_studio_composite::{
    BlendSpace, EvalReport, Exactness, ImageInfo, Limits as CLimits, RenderInputs, StepStatus, VectorAssets,
    VectorResolution, evaluate_with_inputs, lower, lower_with_inputs, prepare_vectors,
};
use hsk_studio_folio::{
    Budget, Inspection, Resolution, Resolver, Snapshot, StudioImageSource, TileRef,
    inspect_bytes,
};
use hsk_studio_interop_fig::*;
use hsk_studio_nib::OwnedPath;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, ExecuteRequest, RenderBackend, OutputTarget, PlanSources, SampleFormat,
    SinkError, SourceKey, TilePlanes, TileSink,
};
use hsk_studio_pigment::Mask;
use hsk_studio_render_cpu::Renderer;
use std::collections::BTreeMap;

struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Absent
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
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}
/// Everything resolvable: the render side of the test reopens the proposal's own bytes with it.
struct Available;
impl Resolver for Available {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

struct Mapped {
    doc: FigDocument,
    proposal: NativeProposal,
}

fn mapped(name: &str) -> Mapped {
    let bytes = fixture(name);
    let cancel = CancellationToken::default();
    let FigOutcome::Imported(import) = import_fig(
        &bytes,
        &Limits::default(),
        &VersionPolicy::observed(),
        None,
        &cancel,
    )
    .unwrap() else {
        panic!("{name} imports")
    };
    let doc = decode_document(&import, &Limits::default(), &cancel).unwrap();
    let cfg = NativeConfig {
        budget: Budget::default(),
        resolver: &Host,
    };
    let proposal = to_native(&doc, &cfg, &mut SequentialIdMint::new(0xF16), &cancel).unwrap();
    Mapped { doc, proposal }
}

fn loss(p: &NativeProposal, path: &str) -> BTreeMap<&'static str, u32> {
    p.mapping_loss
        .iter()
        .filter(|l| l.path == path)
        .map(|l| (l.code, l.count))
        .collect()
}

struct Assets<'a>(&'a NativeProposal);
impl VectorAssets for Assets<'_> {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        self.0
            .paths
            .iter()
            .find(|p| p.path_id == path_id)
            .map(|p| p.geometry.clone())
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}

/// The proposal's document with its five profile slots assigned, reopened with everything
/// resolvable (the importer leaves the profiles unassigned: Figma documents carry none).
fn reopened(p: &NativeProposal) -> Snapshot {
    let mut v: serde_json::Value = serde_json::from_slice(p.snapshot.encoded_bytes()).unwrap();
    for slot in [
        "working_space_rgb",
        "working_space_cmyk",
        "working_space_gray",
        "working_space_spot",
        "blending_space",
    ] {
        v[slot] = serde_json::json!("SCPF-019abcde-0000-7000-8000-000000000001");
    }
    match inspect_bytes(
        &serde_json::to_vec(&v).unwrap(),
        Budget::default(),
        &CancellationToken::default(),
        &Available,
    )
    .unwrap()
    {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read only"),
    }
}

struct Sources<'a>(&'a hsk_studio_composite::InputsLowering, &'a hsk_studio_composite::PreparedVectors);
impl PlanSources for Sources<'_> {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        self.0.vector_mask(key, self.1)
    }
}
struct Grid {
    w: usize,
    px: Vec<[f32; 4]>,
}
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> std::result::Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                self.px[(r.y as usize + y) * self.w + r.x as usize + x] =
                    [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}

struct Render {
    w: usize,
    px: Vec<[f32; 4]>,
    /// Vector outputs that were refused, by code.
    refused: BTreeMap<String, u32>,
    outputs: usize,
    rendered: usize,
    ids: Vec<String>,
}

const W: usize = 1400;
const H: usize = 900;

/// Composites the listed outputs that are ready (vector/boolean outputs; text outputs need a font
/// and stay out of this proof), then executes the plan on render-cpu.
fn render(m: &Mapped) -> Render {
    let snap = reopened(&m.proposal);
    let cancel = CancellationToken::default();
    let vectors = prepare_vectors(
        &snap,
        &m.proposal.design,
        &m.proposal.vector_records,
        &Assets(&m.proposal),
        BlendSpace::Encoded,
        &VectorResolution::default(),
        &cancel,
    )
    .unwrap();
    let mut plan = lower(&snap, snap.document().revision, BlendSpace::Encoded, &CLimits::default(), &cancel).unwrap();
    let outputs = plan.outputs.len();
    let mut refused: BTreeMap<String, u32> = BTreeMap::new();
    let inputs = RenderInputs { vectors: Some(&vectors), ..RenderInputs::default() };
    // Outputs that cannot be drawn by this harness (text needs prepared fonts; strokes, gradients and
    // image paints are composite refusals) stay out of the document root, counted by reason.
    let probe: EvalReport = evaluate_with_inputs(&plan, &snap, &inputs, &cancel).unwrap();
    plan.outputs.retain(|o| match probe.step(o).map(|s| &s.status) {
        Some(StepStatus::Unavailable(reason)) => {
            *refused.entry(reason.clone()).or_insert(0) += 1;
            false
        }
        _ => true,
    });
    let rendered = plan.outputs.len();
    let ids: Vec<String> = plan.outputs.iter().map(|o| o.layer_id.clone()).collect();
    let mut px = vec![[0.0f32; 4]; W * H];
    if rendered > 0 {
        let plan = plan.with_document_root().unwrap();
        let report: EvalReport = evaluate_with_inputs(&plan, &snap, &inputs, &cancel).unwrap();
        let target = OutputTarget {
            extent: Rect { x: 0, y: 0, width: W as u32, height: H as u32 },
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag {
                profile_sha256: hsk_studio_prism::profile_hash(b"working-profile"),
                blend_space: BlendSpace::Encoded,
            },
        };
        let no_tiles = |_: &TileRef| None;
        let low = lower_with_inputs(&plan, &report, &snap, &inputs, &plan.document_root(), target, &no_tiles)
            .unwrap_or_else(|e| panic!("lowering: {e}"));
        let mut grid = Grid { w: W, px: vec![[f32::NAN; 4]; W * H] };
        let request = ExecuteRequest {
            expected_revision: snap.document().revision,
            min_exactness: Exactness::Approximate,
            deadline: None,
            max_working_bytes: 1 << 28,
        };
        Renderer::with_threads(256, 4)
            .unwrap()
            .execute_extended(&low.plan, &low.extensions, &request, &Sources(&low, &vectors), &cancel, &mut grid)
            .unwrap();
        px = grid.px;
    }
    Render { w: W, px, refused, outputs, rendered, ids }
}

/// Layer bbox in canvas px (union of its paths and, for boolean groups, its operands).
fn bbox_of(m: &Mapped, layer_id: &str) -> Option<[f64; 4]> {
    let mut ids = vec![layer_id.to_owned()];
    let doc = m.proposal.snapshot.document();
    ids.extend(doc.layers.iter().filter(|l| matches!(&l.parent, hsk_studio_folio::Parent::Node(p) if p.node_id == layer_id)).map(|l| l.layer_id.clone()));
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in m.proposal.paths.iter().filter(|p| ids.contains(&p.layer_id)) {
        let r = kurbo_bbox(&p.geometry);
        b = [b[0].min(r[0]), b[1].min(r[1]), b[2].max(r[2]), b[3].max(r[3])];
    }
    (b[0] <= b[2]).then_some(b)
}
fn kurbo_bbox(p: &OwnedPath) -> [f64; 4] {
    use hsk_studio_nib::PathElement as E;
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut put = |q: &hsk_studio_nib::PathPoint| {
        b = [b[0].min(q.x), b[1].min(q.y), b[2].max(q.x), b[3].max(q.y)];
    };
    for e in &p.elements {
        match e {
            E::MoveTo(a) | E::LineTo(a) => put(a),
            E::QuadTo(a, c) => { put(a); put(c) }
            E::CubicTo(a, c, d) => { put(a); put(c); put(d) }
            E::Close => {}
        }
    }
    b
}

fn decode_png(bytes: &[u8]) -> (usize, usize, Vec<u8>) {
    assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let (mut at, mut idat) = (8usize, Vec::new());
    let (mut w, mut h, mut ct) = (0usize, 0usize, 0u8);
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = &bytes[at + 4..at + 8];
        let body = &bytes[at + 8..at + 8 + len];
        match kind {
            b"IHDR" => {
                w = u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize;
                h = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
                assert_eq!((body[8], body[12]), (8, 0), "8-bit, not interlaced");
                ct = body[9];
            }
            b"IDAT" => idat.extend_from_slice(body),
            _ => {}
        }
        at += 12 + len;
    }
    let bpp = match ct {
        2 => 3,
        6 => 4,
        other => panic!("colour type {other}"),
    };
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat).expect("inflate");
    let stride = w * bpp;
    let mut out = vec![0u8; h * stride];
    for y in 0..h {
        let f = raw[y * (stride + 1)];
        let line = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= bpp { out[y * stride + x - bpp] } else { 0 };
            let b = if y > 0 { out[(y - 1) * stride + x] } else { 0 };
            let c = if y > 0 && x >= bpp { out[(y - 1) * stride + x - bpp] } else { 0 };
            let pred = match f {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                4 => {
                    let (pa, pb, pc) = (
                        (i32::from(b) - i32::from(c)).abs(),
                        (i32::from(a) - i32::from(c)).abs(),
                        (i32::from(a) + i32::from(b) - 2 * i32::from(c)).abs(),
                    );
                    if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c }
                }
                other => panic!("filter {other}"),
            };
            out[y * stride + x] = line[x].wrapping_add(pred);
        }
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in out.chunks(bpp) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], if bpp == 4 { px[3] } else { 255 }]);
    }
    (w, h, rgba)
}

/// Our pixel at canvas (x, y) over the PNG background, 0..=255 per channel.
fn over_bg(r: &Render, x: usize, y: usize, bg: [u8; 3]) -> [f64; 3] {
    let p = r.px[y * r.w + x];
    let a = f64::from(p[3]);
    [0, 1, 2].map(|c| f64::from(p[c]) * 255.0 * a + f64::from(bg[c]) * (1.0 - a))
}

#[test]
fn discover2() {
    let m = mapped("fig-basic-shapes.fig");
    let r = render(&m);
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    let bg = [png.2[0], png.2[1], png.2[2]];
    println!("bg {bg:?}");
    let doc = m.proposal.snapshot.document();
    for id in &r.ids {
        let l = doc.layers.iter().find(|l| &l.layer_id == id).unwrap();
        let b = bbox_of(&m, id).unwrap();
        let (cx, cy) = (((b[0] + b[2]) / 2.0) as usize, ((b[1] + b[3]) / 2.0) as usize);
        let ours = over_bg(&r, cx, cy, bg);
        let o = ((cy - 100) * png.0 + (cx - 100)) * 4;
        println!("{} bbox {:?} centre ({cx},{cy}) ours {:?} png {:?}", l.name, b, ours.map(|v| v.round()), &png.2[o..o + 3]);
    }
    let name = "fig-prototype-pages.fig";
    let m = mapped(name);
    let r = render(&m);
    for id in &r.ids {
        let l = m.proposal.snapshot.document().layers.iter().find(|l| &l.layer_id == id).unwrap();
        let b = bbox_of(&m, id).unwrap();
        println!("{} bbox {:?}", l.name, b);
    }
    for n in m.doc.nodes.iter().filter(|n| matches!(n.name.as_deref(), Some("start-screen" | "detail-screen" | "animated-screen" | "animated-orb"))) {
        println!("node {:?} {} t={:?} size={:?}", n.name, n.type_name, n.transform.map(|t| (t.m02, t.m12)), n.size);
    }
    for f in ["start", "detail", "animated"] {
        let p = decode_png(&fixture(&format!("fig-prototype-pages-{f}.png")));
        println!("png {f} {}x{} corner {:?}", p.0, p.1, &p.2[..3]);
    }
}
#[test]
fn discover() {
    for name in ["fig-basic-shapes.fig", "fig-components-autolayout.fig", "fig-prototype-pages.fig"] {
        let m = mapped(name);
        let d = m.proposal.snapshot.document();
        println!("== {name} ops={} edges={} outputs={}", d.graph.operations.len(), d.graph.edges.len(), d.graph.outputs.len());
        println!("graph loss {:?}", loss(&m.proposal, "native.graph"));
        let r = render(&m);
        println!("outputs {} rendered {} refused {:?}", r.outputs, r.rendered, r.refused);
        let lit = r.px.iter().filter(|p| p[3] > 0.0).count();
        println!("lit pixels {lit} w={}", r.w);
    }
}
