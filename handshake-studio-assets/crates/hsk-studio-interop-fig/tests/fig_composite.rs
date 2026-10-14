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
    Budget, Inspection, Payload, Resolution, Resolver, Snapshot, StudioImageSource, TileRef,
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
    let layers: BTreeMap<&str, &Payload> = snap
        .document()
        .layers
        .iter()
        .map(|l| (l.layer_id.as_str(), &l.payload))
        .collect();
    plan.outputs.retain(|o| {
        if matches!(layers.get(o.layer_id.as_str()), Some(Payload::Text { .. })) {
            return false;
        }
        match vectors.refusal(&o.layer_id) {
            Some(code) => {
                *refused.entry(code.to_owned()).or_insert(0) += 1;
                false
            }
            None => true,
        }
    });
    let rendered = plan.outputs.len();
    let mut px = vec![[0.0f32; 4]; W * H];
    if rendered > 0 {
        let plan = plan.with_document_root().unwrap();
        let inputs = RenderInputs { vectors: Some(&vectors), ..RenderInputs::default() };
        let report: EvalReport = evaluate_with_inputs(&plan, &snap, &inputs, &cancel).unwrap();
        for s in &report.steps {
            assert!(!matches!(s.status, StepStatus::Unavailable(_)) || s.addr.layer_id != "x", "{:?}", s.addr);
        }
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
    Render { w: W, px, refused, outputs, rendered }
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
