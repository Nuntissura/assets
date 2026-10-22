//! SMB-COMPOSITE-TEXT: text layers lower to a Solid fill masked by Type's coverage_u8 plane.
//! Expected shapes are derived by hand from the fixture; fonts are the hash-pinned OFL fixtures of
//! hsk-studio-type (V4-03). render-cpu and render-gpu execute the same plan (text_layer_render_plan)
//! in their tests/text_render.rs (decision B1: renderers dev-depend on composite).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::*;
use hsk_studio_folio::TileRef;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BackendIdentity, ColourTag, DeviceClass, ExecuteRequest, Group,
    MaskFormat, Operator, OutputTarget, PlanNode, PlanSources, RenderBackend, RenderPlan,
    RenderReceipt, SampleFormat, TileSink, admit,
};
use hsk_studio_type::{Axis, FontBytes, Prepared};
use serde_json::{Value, json};

mod common;
use common::*;
#[path = "common/text_shaping.rs"]
mod shaping;
use shaping::{FontFile, Seg, shape};

/// Same inputs as hsk-studio-type tests/glyph_render.rs (Inter "Hi", 32 px/em, origin
/// (10.25, 40.0)): composite must pass Type's plane through unchanged.
const TYPE_HI_DIGEST: &str = "2b66ea525d0abc933ff810dc5d5be4016d0837a745c633ffae52271d74bc47ac";
const PLAN_DIGEST: &str = "b0cce052d6eb290bfdc42185a76eaa9bb00984e8bf7036e5d46f7190efb9ddbd";
const FILL: [f32; 4] = [0.8, 0.1, 0.2, 1.0];

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 128, height: 128 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
fn info(tile: &TileRef) -> Option<TileInfo> {
    Some(TileInfo {
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        colour_sha256: [tile.object_key.len() as u8; 32],
        alpha_sha256: [9; 32],
    })
}
/// Group 1 composites raster 2; text 3 is its own graph output. Folio types every edge
/// (`source.value_type == target.value_type`) and a group input is `image`, so a `text` source
/// cannot be a group input today (Folio gap, lane note G1): text renders as its own root.
fn doc(text_appearance: Option<Value>) -> Value {
    let mut text = layer(3, "text", json!({"payload_kind":"text","story_id":identity("STXT",1)}));
    if let Some(a) = text_appearance {
        text["appearance"] = a;
    }
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"mm","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[layer(1, "group", json!({"payload_kind":"group"})), layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2, "t00", 0, 0, 64, 64)]})), text],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2, "image"), composite(1), source(3, "text")],
            "edges":[edge(address(2, "source", "out"), address(1, "mix", "inputs"), 0)],
            "outputs":[address(1, "mix", "out"), address(3, "source", "out")]}
    });
    assign_all_profiles(&mut v);
    v
}
fn text_root() -> NodeAddr {
    addr(3, "source", "out")
}
fn resolution() -> TextResolution {
    TextResolution::at_ppi(192.0) // 192 / 72 = 2 * 96 / 72: 24 pt at 96 ppi for 12 pt type
}
fn latin(text: &str) -> Vec<Seg> {
    vec![Seg { start: 0, end: text.len() as u64, font: 0, script: *b"Latn", language: "en", axes: vec![] }]
}
/// Every run of the paragraph placed left to right from `origin` (pen advances in pt -> px).
fn layer_of(
    p: &Prepared<'_>,
    files: &[FontFile],
    origin: [f64; 2],
    fill: [f32; 4],
    res: &TextResolution,
    cancel: &CancellationToken,
) -> Result<PreparedText, CompositeError> {
    let mut x = origin[0];
    let mut runs = Vec::new();
    for run in p.paragraphs()[0].runs() {
        let file = files.iter().find(|f| f.identity == run.resolved().identity).unwrap();
        runs.push(TextRun { run, font: FontBytes { bytes: &file.bytes, resource: run.resolved() }, origin_px: [x, origin[1]] });
        x += run.glyphs().iter().map(|g| g.x_advance_pt).sum::<f64>() * res.px_per_pt;
    }
    let id = identity("SLYR", 3);
    prepare_text(&[TextLayerInput { layer_id: &id, runs: &runs, fill_rgba: fill }], res, cancel)
}
fn hi(res: &TextResolution, fill: [f32; 4]) -> Result<PreparedText, CompositeError> {
    let files = [FontFile::inter()];
    shape(&files, "Hi", &latin("Hi"), false, 12.0, |p| {
        layer_of(p, &files, [10.25, 40.0], fill, res, &CancellationToken::default())
    })
}
fn lowered(v: &Value, text: &PreparedText) -> (TextRenderLowering, EvalReport) {
    let snap = open(v);
    let plan = plan_of(&snap);
    let report = evaluate_with_text(&plan, &snap, text, &CancellationToken::default()).unwrap();
    report.require_ready().unwrap();
    let low = lower_to_render_plan_with_text(&plan, &report, &snap, &text_root(), target(), &info, text).unwrap();
    (low, report)
}
struct Backend;
impl RenderBackend for Backend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity { name: "composite-text-double", version: "0", device: DeviceClass::Cpu, adapter: String::new(), generation: 0 }
    }
    fn exactness(&self, operator: Operator) -> Exactness {
        operator.intrinsic()
    }
    fn execute(&self, _: &RenderPlan, _: &ExecuteRequest, _: &dyn PlanSources, _: &CancellationToken, _: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("test_double"))
    }
}
fn admitted(plan: &RenderPlan) -> Exactness {
    let request = ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Exact, deadline: None, max_working_bytes: 1 << 24 };
    admit(plan, &Backend, &request, &CancellationToken::default()).unwrap().lowest
}
/// The text content node: an isolated group of one Normal layer, Solid masked by coverage_u8.
fn text_group(plan: &RenderPlan) -> (&Group, Rect, [f32; 4]) {
    for node in &plan.nodes {
        if let PlanNode::Group(g) = node
            && let [l] = g.layers.as_slice()
            && let Some(m) = l.mask
            && m.format == MaskFormat::CoverageU8
            && let PlanNode::Solid { rect, rgba } = plan.nodes[l.input.0 as usize]
        {
            assert!(g.isolated);
            assert_eq!(l.mode, StudioBlendMode::Normal);
            assert_eq!((l.opacity, l.fill_opacity, m.inverted, m.density), (1.0, 1.0, false, 1.0));
            assert_eq!(m.placement, rect);
            return (g, rect, rgba);
        }
    }
    panic!("no text group in {:?}", plan.nodes)
}

#[test]
fn text_layer_lowers_with_pinned_digest() {
    bounded_case("text_layer_lowers_with_pinned_digest", || {
        let text = hi(&resolution(), FILL).unwrap();
        let plane = text.plane(&identity("SLYR", 3)).unwrap();
        assert_eq!(hex(&plane.sha256), TYPE_HI_DIGEST, "Type's plane passes through unchanged");
        let placement = plane.placement.unwrap();
        assert_eq!((placement.x, placement.y, placement.width, placement.height), (13, 16, 27, 24));
        let (low, report) = lowered(&doc(None), &text);
        assert_eq!(report.step(&addr(3, "source", "out")).unwrap().extent, Some(placement));
        let plan = &low.lowering.plan;
        let (group, rect, rgba) = text_group(plan);
        assert_eq!((rect, rgba), (placement, FILL));
        let key = group.layers[0].mask.unwrap().key;
        assert_eq!(low.text_masks, vec![(key, identity("SLYR", 3))]);
        assert_eq!(group.layers[0].mask.unwrap().sha256, plane.sha256);
        let mask = low.text_mask(key, &text).unwrap();
        mask.validate(placement).unwrap();
        assert!(low.lowering.sources.is_empty() && low.lowering.mask_sources.is_empty());
        assert_eq!(admitted(plan), Exactness::Exact);
        let digest = hex(&plan.digest().unwrap());
        println!("text_plan digest={digest} plane_sha256={}", hex(&plane.sha256));
        assert_eq!(digest, PLAN_DIGEST, "lowered plan anchor");
        // The Folio-free entry point the renderer tests execute is the same plan, byte for byte.
        assert_eq!(text_layer_render_plan(&text, &identity("SLYR", 3), 7, target()).unwrap(), low);
        // Deterministic: a second preparation and lowering give identical bytes.
        let again = hi(&resolution(), FILL).unwrap();
        assert_eq!(again, text);
        assert_eq!(lowered(&doc(None), &again).0, low);
        // Executed by render-cpu tests/text_render.rs (pinned digest) and render-gpu tests/text_render.rs.
    });
}

#[test]
fn static_path_unchanged_without_text() {
    bounded_case("static_path_unchanged_without_text", || {
        let cancel = CancellationToken::default();
        // A raster-only document: the text entry points with no text equal the static path.
        let mut v = doc(None);
        v["layers"].as_array_mut().unwrap().remove(2);
        v["graph"]["operations"].as_array_mut().unwrap().remove(2);
        v["graph"]["outputs"].as_array_mut().unwrap().remove(1);
        let snap = open(&v);
        let plan = plan_of(&snap);
        let report = evaluate(&plan, &snap, &cancel).unwrap();
        let none = PreparedText::default();
        assert_eq!(evaluate_with_text(&plan, &snap, &none, &cancel).unwrap(), report);
        let root = addr(1, "mix", "out");
        let base = lower_to_render_plan(&plan, &report, &snap, &root, target(), &info).unwrap();
        let with = lower_to_render_plan_with_text(&plan, &report, &snap, &root, target(), &info, &none).unwrap();
        assert_eq!(with.lowering, base);
        assert!(with.text_masks.is_empty());
        // A text layer: the static path keeps refusing it; the text path needs its prepared plane.
        let snap = open(&doc(None));
        let plan = plan_of(&snap);
        let status = |r: &EvalReport| r.step(&addr(3, "source", "out")).unwrap().status.clone();
        assert_eq!(
            status(&evaluate(&plan, &snap, &cancel).unwrap()),
            StepStatus::Unavailable("text_to_image_conversion".into())
        );
        assert_eq!(
            status(&evaluate_with_text(&plan, &snap, &none, &cancel).unwrap()),
            StepStatus::Unavailable("text_layer_not_prepared".into())
        );
        // An inkless text layer (spaces) is Ready, empty and emits no node.
        let files = [FontFile::inter()];
        let blank = shape(&files, "   ", &latin("   "), false, 12.0, |p| {
            layer_of(p, &files, [0.0, 20.0], FILL, &resolution(), &cancel)
        })
        .unwrap();
        let blank_report = evaluate_with_text(&plan, &snap, &blank, &cancel).unwrap();
        blank_report.require_ready().unwrap();
        let step = blank_report.step(&text_root()).unwrap();
        assert_eq!((step.extent, &step.status), (None, &StepStatus::Ready));
        assert!(blank.planes()[0].placement.is_none() && blank.planes()[0].coverage.is_empty());
    });
}

#[test]
fn mixed_script_paragraph_has_ink() {
    bounded_case("mixed_script_paragraph_has_ink", || {
        let files = [FontFile::inter(), FontFile::arabic(), FontFile::han()];
        // Recipe case mixed_ltr_isolate_and_trailing_space (hsk-studio-type fixtures).
        let text = "A \u{2067}العربية\u{2069} 中文 123   ";
        let seg = |start, end, font, script: &[u8; 4], language| Seg { start, end, font, script: *script, language, axes: vec![] };
        let segs = [
            seg(0, 2, 0, b"Latn", "en"),
            seg(2, 5, 0, b"Zyyy", "und"),
            seg(5, 19, 1, b"Arab", "ar"),
            seg(19, 22, 0, b"Zyyy", "und"),
            seg(22, 30, 2, b"Hani", "zh-CN"),
            seg(30, 36, 0, b"Latn", "en"),
        ];
        let res = resolution();
        let cancel = CancellationToken::default();
        let (whole, inked) = shape(&files, text, &segs, false, 12.0, |p| {
            let whole = layer_of(p, &files, [2.0, 60.0], FILL, &res, &cancel).unwrap();
            let mut inked = Vec::new();
            for run in p.paragraphs()[0].runs() {
                let file = files.iter().find(|f| f.identity == run.resolved().identity).unwrap();
                let runs = [TextRun { run, font: FontBytes { bytes: &file.bytes, resource: run.resolved() }, origin_px: [0.0, 60.0] }];
                let one = prepare_text(&[TextLayerInput { layer_id: "one", runs: &runs, fill_rgba: FILL }], &res, &cancel).unwrap();
                if one.planes()[0].coverage.iter().any(|v| *v > 0) {
                    inked.push(run.script());
                }
            }
            (whole, inked)
        });
        for script in [*b"Latn", *b"Arab", *b"Hani"] {
            assert!(inked.contains(&script), "{} inked", String::from_utf8_lossy(&script));
        }
        let plane = whole.plane(&identity("SLYR", 3)).unwrap();
        assert!(plane.coverage.iter().map(|v| u64::from(*v)).sum::<u64>() > 0);
        let (low, _) = lowered(&doc(None), &whole);
        let (_, rect, _) = text_group(&low.lowering.plan);
        assert_eq!(Some(rect), plane.placement);
        assert_eq!(admitted(&low.lowering.plan), Exactness::Exact);
    });
}

#[test]
fn text_with_layer_opacity_and_blend() {
    bounded_case("text_with_layer_opacity_and_blend", || {
        let text = hi(&resolution(), FILL).unwrap();
        // Text 3 multiplies (discriminant 5) at 50 percent: a non-neutral root is wrapped in one
        // isolated group whose layer carries the authored appearance; the text group is unchanged.
        let a = json!({"opacity":50.0,"fill_opacity":100.0,"blend_mode":5,"clipped":false,"isolation":"isolated"});
        let (low, _) = lowered(&doc(Some(a)), &text);
        let plan = &low.lowering.plan;
        let (_, rect, rgba) = text_group(plan);
        assert_eq!(rgba, FILL);
        let PlanNode::Group(root) = &plan.nodes[plan.root.0 as usize] else { panic!("root group") };
        assert!(root.isolated);
        let [wrap] = root.layers.as_slice() else { panic!("one wrapper layer") };
        let PlanNode::Group(g) = &plan.nodes[wrap.input.0 as usize] else { panic!("text group") };
        assert_eq!(g.layers[0].mask.unwrap().placement, rect);
        assert_eq!((wrap.mode, wrap.opacity, wrap.fill_opacity, wrap.mask), (StudioBlendMode::Multiply, 0.5, 1.0, None));
        assert_eq!(admitted(plan), Exactness::Exact);
        // The appearance never changes the plane, and the raster group renders without the text.
        let (plain, _) = lowered(&doc(None), &text);
        assert_eq!(text_group(&plain.lowering.plan).1, rect);
        assert_eq!(plain.lowering.plan.root, text_group_id(&plain.lowering.plan));
        let snap = open(&doc(None));
        let plan = plan_of(&snap);
        let report = evaluate_with_text(&plan, &snap, &text, &CancellationToken::default()).unwrap();
        let group = lower_to_render_plan_with_text(&plan, &report, &snap, &addr(1, "mix", "out"), target(), &info, &text).unwrap();
        assert!(group.text_masks.is_empty());
        assert!(!group.lowering.plan.nodes.iter().any(|n| matches!(n, PlanNode::Solid { .. })));
    });
}

fn text_group_id(plan: &RenderPlan) -> hsk_studio_pigment::render_plan::PlanNodeId {
    let (g, _, _) = text_group(plan);
    let at = plan.nodes.iter().position(|n| matches!(n, PlanNode::Group(x) if std::ptr::eq(x, g))).unwrap();
    hsk_studio_pigment::render_plan::PlanNodeId(at as u32)
}

#[test]
fn hostile_text_refused_typed() {
    bounded_case("hostile_text_refused_typed", || {
        let reason = |r: Result<PreparedText, CompositeError>| match r {
            Err(CompositeError::Unavailable(s)) => s.rsplit(':').next().unwrap().to_owned(),
            other => panic!("expected Unavailable, got {other:?}"),
        };
        let huge = TextResolution { px_per_pt: 1e12, ..resolution() };
        assert_eq!(reason(hi(&huge, FILL)), "raster_over_limit");
        let tight = TextResolution { max_layer_pixels: 100, ..resolution() };
        assert_eq!(reason(hi(&tight, FILL)), "raster_over_limit");
        let nan = TextResolution { px_per_pt: f64::NAN, ..resolution() };
        assert_eq!(hi(&nan, FILL), Err(CompositeError::UnsupportedDescriptor("text_resolution".into())));
        assert_eq!(reason(hi(&resolution(), [2.0, 0.0, 0.0, 1.0])), "text_fill");
        assert_eq!(reason(hi(&resolution(), [f32::NAN, 0.0, 0.0, 1.0])), "text_fill");
        // A colour face (COLR table) is refused by Type before any outline work.
        let files = [FontFile::inter()];
        let mut colr = files[0].bytes.clone();
        assert_eq!(&colr[12..16], b"GDEF");
        colr[12..16].copy_from_slice(b"COLR"); // table directory stays sorted
        let colr_hash = hsk_studio_prism::profile_hash(&colr);
        let cancel = CancellationToken::default();
        let refused = shape(&files, "Hi", &latin("Hi"), false, 12.0, |p| {
            let run = &p.paragraphs()[0].runs()[0];
            let mut resource = run.resolved();
            resource.content_hash = colr_hash;
            let runs = [TextRun { run, font: FontBytes { bytes: &colr, resource }, origin_px: [0.0, 20.0] }];
            prepare_text(&[TextLayerInput { layer_id: "colour", runs: &runs, fill_rgba: FILL }], &resolution(), &cancel)
        });
        assert_eq!(reason(refused), "type_UnsupportedProfile");
        // Explicit variation axes: Type outlines the default instance only.
        let var = [FontFile::inter_variable()];
        let segs = [Seg { start: 0, end: 2, font: 0, script: *b"Latn", language: "en", axes: vec![Axis { tag: *b"wght", value: 700.0 }] }];
        let refused = shape(&var, "Hi", &segs, false, 12.0, |p| layer_of(p, &var, [0.0, 20.0], FILL, &resolution(), &cancel));
        assert_eq!(reason(refused), "variable_outline_unsupported");
        // Cancellation is typed.
        let stopped = CancellationToken::default();
        stopped.cancel();
        let canceled = shape(&files, "Hi", &latin("Hi"), false, 12.0, |p| layer_of(p, &files, [0.0, 20.0], FILL, &resolution(), &stopped));
        assert_eq!(canceled, Err(CompositeError::Canceled));
    });
}
