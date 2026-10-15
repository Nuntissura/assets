//! Lowering of an evaluated composite plan into pigment's shared render-plan IR. Expected node
//! lists, placements and source tables are derived by hand from the fixture (Kahn order with the
//! (layer_id, operation_key) tie-break, tile grid cells), never produced by the code under test.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    BlendSpace, CompositeError, Exactness, Limits, StepStatus, TileInfo, TileInfoSource, evaluate, lower,
    lower_to_render_plan,
};
use hsk_studio_folio::TileRef;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BackendIdentity, ColourTag, DeviceClass, ExecuteRequest,
    MaskFormat, MaskInput, Operator, OutputTarget, PlanError, PlanNode, PlanNodeId, PlanSources, RenderBackend,
    RenderPlan, RenderReceipt, SampleFormat, SourceKey, StudioBlendMode, TileSink, admit,
};
use serde_json::{Value, json};

mod common;
use common::*;

fn target(space: BlendSpace) -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 128, height: 128 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: space },
    }
}
fn plane_hash(object_key: &str, salt: u8) -> [u8; 32] {
    [object_key.bytes().fold(salt, |a, b| a.wrapping_mul(31).wrapping_add(b)); 32]
}
fn info(tile: &TileRef) -> Option<TileInfo> {
    Some(TileInfo {
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        colour_sha256: plane_hash(&tile.object_key, 1),
        alpha_sha256: plane_hash(&tile.object_key, 2),
    })
}
/// Caller-side resolver double: tile planes and one stitched mask plane per layer mask.
struct Info;
impl TileInfoSource for Info {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        info(tile)
    }
    fn mask_sha256(&self, tiles: &[TileRef]) -> Option<[u8; 32]> {
        Some(plane_hash(&tiles.first()?.object_key, 3))
    }
}
fn appearance(opacity: f64, fill: f64, mode: u16, group: bool) -> Value {
    json!({"opacity":opacity,"fill_opacity":fill,"blend_mode":mode,"clipped":false,
        "isolation": if group {"pass_through"} else {"isolated"}})
}
fn mask_tile(layer_n: u8, key: &str) -> Value {
    let mut t = tile(layer_n, key, 0, 0, 32, 32);
    t["format"] = json!("coverage_u8");
    t
}
/// Group 1 = raster 2 (tiles t00 64x64 at col 0 row 0, t10 64x32 at col 1 row 0) then raster 3
/// (t01 64x64 at col 0 row 1); vector 4 and text 5 are unrelated outputs. All profiles assigned.
fn tiled() -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["layers"][1]["payload"] =
        json!({"payload_kind":"raster","tiles":[tile(2,"t00",0,0,64,64),tile(2,"t10",1,0,64,32)]});
    v["layers"][2]["payload"] = json!({"payload_kind":"raster","tiles":[tile(3,"t01",0,1,64,64)]});
    v
}
fn lowered(
    v: &Value,
    space: BlendSpace,
    root: &hsk_studio_composite::NodeAddr,
) -> Result<hsk_studio_composite::RenderLowering, CompositeError> {
    let snap = open(v);
    let token = CancellationToken::default();
    let plan = lower(&snap, 7, space, &Limits::default(), &token).unwrap();
    let report = evaluate(&plan, &snap, &token).unwrap();
    lower_to_render_plan(&plan, &report, &snap, root, target(space), &Info)
}

#[test]
fn tiles_groups_order_and_hidden_lower_to_pigment_plan() {
    bounded_case("tiles_groups_order_and_hidden_lower_to_pigment_plan", || {
        let root = addr(1, "mix", "out");
        let low = lowered(&tiled(), BlendSpace::LinearLight, &root).unwrap();
        // Emission follows Kahn order 2, 3, 1: layer 2 -> tiles 0,1 + isolated group 2; layer 3 ->
        // tile 3; root group 4 lists them bottom-first by authored input_ordinal.
        let nodes = &low.plan.nodes;
        assert_eq!(nodes.len(), 5);
        let rect = |x, y, width, height| Rect { x, y, width, height };
        let tile_at = |n: usize| match &nodes[n] {
            PlanNode::Tile(t) => *t,
            other => panic!("node {n} is not a tile: {other:?}"),
        };
        assert_eq!(tile_at(0).placement, rect(0, 0, 64, 64));
        assert_eq!(tile_at(1).placement, rect(64, 0, 64, 32));
        assert_eq!(tile_at(3).placement, rect(0, 64, 64, 64));
        assert_eq!(
            [tile_at(0).key, tile_at(1).key, tile_at(3).key],
            [SourceKey(0), SourceKey(1), SourceKey(2)]
        );
        assert_eq!(tile_at(0).colour_sha256, plane_hash("t00", 1));
        assert_eq!(tile_at(3).alpha_sha256, plane_hash("t01", 2));
        let PlanNode::Group(multi) = &nodes[2] else { panic!("node 2 must be the tile group") };
        assert!(multi.isolated);
        assert_eq!(multi.layers.iter().map(|l| l.input).collect::<Vec<_>>(), [PlanNodeId(0), PlanNodeId(1)]);
        let PlanNode::Group(top) = &nodes[4] else { panic!("node 4 must be the root group") };
        assert!(top.isolated);
        assert_eq!(top.layers.iter().map(|l| l.input).collect::<Vec<_>>(), [PlanNodeId(2), PlanNodeId(3)]);
        assert!(top.layers.iter().all(|l| l.mode == StudioBlendMode::Normal
            && l.opacity == 1.0
            && l.fill_opacity == 1.0
            && l.mask.is_none()));
        assert_eq!(low.plan.root, PlanNodeId(4));
        assert_eq!(low.plan.revision, 7);
        assert_eq!(low.plan.target, target(BlendSpace::LinearLight));
        // Source table keeps the authored records, in key order; the vector/text outputs are absent.
        let keys: Vec<(u32, &str)> = low.sources.iter().map(|(k, t)| (k.0, t.object_key.as_str())).collect();
        assert_eq!(keys, [(0, "t00"), (1, "t10"), (2, "t01")]);
        assert_eq!(low.exactness.lowest, Exactness::Exact);
        let used: Vec<&str> = low.exactness.operators.iter().map(|o| o.key).collect();
        assert_eq!(used, ["tile", "normal", "group_isolated"]);

        // Hidden layer 3 is not emitted: no tile, no key, one root layer.
        let mut hidden = tiled();
        hidden["layers"][2]["visible"] = json!(false);
        let low_hidden = lowered(&hidden, BlendSpace::LinearLight, &root).unwrap();
        assert_eq!(low_hidden.plan.nodes.len(), 4);
        assert_eq!(low_hidden.sources.len(), 2);
        let PlanNode::Group(top) = &low_hidden.plan.nodes[3] else { panic!("root group") };
        assert_eq!(top.layers.len(), 1);

        // The digest binds the exact plan: identical lowering, identical digest; hidden differs.
        let again = lowered(&tiled(), BlendSpace::LinearLight, &root).unwrap();
        assert_eq!(low.plan.digest().unwrap(), again.plan.digest().unwrap());
        assert_ne!(low.plan.digest().unwrap(), low_hidden.plan.digest().unwrap());
    });
}

#[test]
fn nested_group_keeps_pass_through_and_fanout_shares_nodes() {
    bounded_case("nested_group_keeps_pass_through_and_fanout_shares_nodes", || {
        // Outer group 7 composites raster 8 (ordinal 0) then group 1 (ordinal 1); raster 2 of group 1
        // also feeds group 9 (fanout), which is a second root candidate.
        let mut v = tiled();
        v["layers"].as_array_mut().unwrap().extend([
            layer(7, "group", json!({"payload_kind":"group"})),
            layer(8, "raster", json!({"payload_kind":"raster","tiles":[tile(8,"t80",0,0,16,16)]})),
            layer(9, "group", json!({"payload_kind":"group"})),
        ]);
        let ops = v["graph"]["operations"].as_array_mut().unwrap();
        ops.extend([source(8, "image"), composite(7), composite(9)]);
        v["graph"]["edges"].as_array_mut().unwrap().extend([
            edge(address(1, "mix", "out"), address(7, "mix", "inputs"), 1),
            edge(address(8, "source", "out"), address(7, "mix", "inputs"), 0),
            edge(address(2, "source", "out"), address(9, "mix", "inputs"), 0),
        ]);
        let low = lowered(&v, BlendSpace::Encoded, &addr(7, "mix", "out")).unwrap();
        // Group 1 is non-root with the default Pass Through: non-isolated, referenced PassThrough.
        let groups: Vec<_> = low
            .plan
            .nodes
            .iter()
            .filter_map(|n| match n {
                PlanNode::Group(g) => Some(g),
                _ => None,
            })
            .collect();
        assert!(groups.iter().any(|g| !g.isolated), "nested group must be non-isolated");
        let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        assert!(root.isolated);
        assert_eq!(root.layers.len(), 2);
        assert_eq!(root.layers[0].mode, StudioBlendMode::Normal);
        assert_eq!(root.layers[1].mode, StudioBlendMode::PassThrough);
        let used: Vec<&str> = low.exactness.operators.iter().map(|o| o.key).collect();
        assert!(used.contains(&"group_pass_through") && !used.contains(&"group_pass_through_opacity"));
        assert_eq!(low.exactness.lowest, Exactness::Exact);
        // Group 9 is not reachable from root 7: its tiles are not duplicated or emitted twice.
        assert_eq!(low.sources.len(), 4);
        // Rendering group 9 alone shares raster 2's two tiles through one group node (no copies).
        let other = lowered(&v, BlendSpace::Encoded, &addr(9, "mix", "out")).unwrap();
        assert_eq!(other.sources.len(), 2);
        let PlanNode::Group(only) = &other.plan.nodes[other.plan.root.0 as usize] else { panic!("root") };
        assert_eq!(only.layers.len(), 1);
    });
}

#[test]
fn refusals_unavailable_mismatch_stale() {
    bounded_case("refusals_unavailable_mismatch_stale", || {
        let token = CancellationToken::default();
        let root = addr(1, "mix", "out");
        let unavailable = |r: Result<hsk_studio_composite::RenderLowering, CompositeError>| match r {
            Err(CompositeError::Unavailable(reason)) => reason,
            other => panic!("expected Unavailable, got {other:?}"),
        };

        // Unassigned blending profile: no plan.
        let mut unbound = tiled();
        unbound["blending_space"] = json!(null);
        assert!(unavailable(lowered(&unbound, BlendSpace::Encoded, &root)).starts_with("blending_space"));

        // Target blend space must be the plan's: never converted.
        let snap = open(&tiled());
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let report = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(
            lower_to_render_plan(&plan, &report, &snap, &root, target(BlendSpace::LinearLight), &info),
            Err(CompositeError::ColourMismatch)
        );

        // The caller cannot describe a tile: Unavailable, hashes are never invented.
        let no_info = |_: &TileRef| -> Option<TileInfo> { None };
        let reason = unavailable(lower_to_render_plan(&plan, &report, &snap, &root, target(BlendSpace::Encoded), &no_info));
        assert!(reason.ends_with("tile_info"), "{reason}");

        // Vector root: no verified image route. An unrelated vector/text output does NOT block group 1.
        let vector_root = addr(4, "source", "out");
        assert!(unavailable(lower_to_render_plan(&plan, &report, &snap, &vector_root, target(BlendSpace::Encoded), &info))
            .contains("vector_to_image_conversion"));
        assert!(lower_to_render_plan(&plan, &report, &snap, &root, target(BlendSpace::Encoded), &info).is_ok());

        // Unresolved references on a reachable layer (Folio disposition) block the plan.
        let unresolved = open_bytes(&serde_json::to_vec(&tiled()).unwrap(), &hsk_studio_folio::Unresolved);
        let uplan = lower(&unresolved, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let ureport = evaluate(&uplan, &unresolved, &token).unwrap();
        assert!(unavailable(lower_to_render_plan(&uplan, &ureport, &unresolved, &root, target(BlendSpace::Encoded), &info))
            .contains("unsupported"));

        // Stale plan or report, unknown root, and an empty root are typed errors.
        let mut old = plan.clone();
        old.revision = 6;
        assert_eq!(
            lower_to_render_plan(&old, &report, &snap, &root, target(BlendSpace::Encoded), &info),
            Err(CompositeError::StaleRevision { expected: 6, actual: 7 })
        );
        assert!(matches!(
            lower_to_render_plan(&plan, &report, &snap, &addr(90, "mix", "out"), target(BlendSpace::Encoded), &info),
            Err(CompositeError::DanglingInput(_))
        ));
        let mut empty = tiled();
        empty["layers"][1]["payload"] = json!({"payload_kind":"raster","tiles":[]});
        let empty_root = addr(2, "source", "out");
        assert_eq!(
            lowered(&empty, BlendSpace::Encoded, &empty_root),
            Err(CompositeError::Plan(PlanError::Empty))
        );
        // An empty layer inside a group is dropped; a hidden-only group still yields an empty root.
        let mut all_hidden = tiled();
        all_hidden["layers"][1]["visible"] = json!(false);
        all_hidden["layers"][2]["visible"] = json!(false);
        let low = lowered(&all_hidden, BlendSpace::Encoded, &root).unwrap();
        assert_eq!(low.plan.nodes.len(), 1);
        assert!(low.sources.is_empty());
    });
}

struct Backend {
    refuses: Option<&'static str>,
}
impl RenderBackend for Backend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "composite-test-double",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, operator: Operator) -> Exactness {
        if Some(operator.key()) == self.refuses {
            Exactness::Unsupported
        } else {
            Exactness::Exact
        }
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("test_double"))
    }
}

#[test]
fn lowered_plan_passes_shared_backend_admission() {
    bounded_case("lowered_plan_passes_shared_backend_admission", || {
        let low = lowered(&tiled(), BlendSpace::LinearLight, &addr(1, "mix", "out")).unwrap();
        let request = ExecuteRequest {
            expected_revision: 7,
            min_exactness: Exactness::Exact,
            deadline: None,
            max_working_bytes: 1 << 20,
        };
        let token = CancellationToken::default();
        let ledger = admit(&low.plan, &Backend { refuses: None }, &request, &token).unwrap();
        assert_eq!(ledger.lowest, Exactness::Exact);
        // A backend that cannot do isolated groups is refused at admission, before any sink call.
        assert_eq!(
            admit(&low.plan, &Backend { refuses: Some("group_isolated") }, &request, &token),
            Err(BackendError::Unsupported { operator: "group_isolated", exactness: Exactness::Unsupported })
        );
        // The shared stale-revision rule applies to the lowered plan.
        let stale = ExecuteRequest { expected_revision: 6, ..request };
        assert_eq!(
            admit(&low.plan, &Backend { refuses: None }, &stale, &token),
            Err(BackendError::StaleRevision { expected: 6, found: 7 })
        );
    });
}

#[test]
fn appearance_lowers_mode_opacity_fill_mask_and_wraps_root() {
    bounded_case("appearance_lowers_mode_opacity_fill_mask_and_wraps_root", || {
        let root = addr(1, "mix", "out");
        let token = CancellationToken::default();
        let unavailable = |r: Result<hsk_studio_composite::RenderLowering, CompositeError>| match r {
            Err(CompositeError::Unavailable(reason)) => reason,
            other => panic!("expected Unavailable, got {other:?}"),
        };

        // Layer 2: multiply, opacity 50 percent, fill 25 percent, enabled mask over one 32x32 tile.
        let mut v = tiled();
        v["layers"][1]["appearance"] = appearance(50.0, 25.0, 5, false);
        v["layers"][1]["appearance"]["mask"] =
            json!({"tiles":[mask_tile(2, "m0")],"enabled":true,"linked":true});
        let low = lowered(&v, BlendSpace::Encoded, &root).unwrap();
        let PlanNode::Group(top) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        let first = top.layers[0];
        assert_eq!(first.mode, StudioBlendMode::Multiply);
        assert_eq!((first.opacity, first.fill_opacity), (0.5, 0.25));
        // Keys: tiles 0,1 (layer 2), 2 (layer 3), then the mask plane 3 (allocated at the group).
        let mask = first.mask.expect("enabled mask must be lowered");
        assert_eq!(mask.key, SourceKey(3));
        assert_eq!(mask.placement, Rect { x: 0, y: 0, width: 32, height: 32 });
        assert_eq!(mask.format, MaskFormat::CoverageU8);
        assert_eq!(mask.sha256, plane_hash("m0", 3));
        assert!(!mask.inverted && mask.density == 1.0);
        assert_eq!(low.mask_sources.len(), 1);
        assert_eq!(low.mask_sources[0].0, SourceKey(3));
        assert_eq!(low.mask_sources[0].1[0].object_key, "m0");
        // The unmodified layer 3 keeps the identity appearance.
        assert_eq!(top.layers[1].mode, StudioBlendMode::Normal);
        assert!(top.layers[1].mask.is_none());
        // Fill opacity with a non-Normal mode is Approximate and says so in both ledgers.
        let used: Vec<&str> = low.exactness.operators.iter().map(|o| o.key).collect();
        for key in ["multiply", "fill_opacity", "coverage_u8"] {
            assert!(used.contains(&key), "{key} missing from {used:?}");
        }
        assert_eq!(low.exactness.lowest, Exactness::Approximate);
        let snap = open(&v);
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        assert_eq!(plan.lowest_exactness(), Exactness::Approximate);
        let report = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(report.receipt.exactness.lowest, Exactness::Approximate);

        // A disabled mask is an authored "no mask": omitted, not an error.
        let mut disabled = v.clone();
        disabled["layers"][1]["appearance"]["mask"]["enabled"] = json!(false);
        let low = lowered(&disabled, BlendSpace::Encoded, &root).unwrap();
        assert!(low.mask_sources.is_empty());
        let PlanNode::Group(top) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        assert!(top.layers[0].mask.is_none());
        // An enabled mask without coverage has unspecified meaning: refused, not guessed.
        let mut empty_mask = v.clone();
        empty_mask["layers"][1]["appearance"]["mask"]["tiles"] = json!([]);
        assert!(unavailable(lowered(&empty_mask, BlendSpace::Encoded, &root)).contains("mask_empty"));

        // The root's own opacity applies: one isolated wrapper layer around the pass-through group.
        let mut faded = tiled();
        faded["layers"][0]["appearance"] = appearance(50.0, 100.0, 1, true);
        let low = lowered(&faded, BlendSpace::Encoded, &root).unwrap();
        let PlanNode::Group(wrapper) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        assert!(wrapper.isolated);
        assert_eq!(wrapper.layers.len(), 1);
        assert_eq!(wrapper.layers[0].mode, StudioBlendMode::PassThrough);
        assert_eq!(wrapper.layers[0].opacity, 0.5);
        let PlanNode::Group(inner) = &low.plan.nodes[wrapper.layers[0].input.0 as usize] else { panic!("inner") };
        assert!(!inner.isolated);
        let used: Vec<&str> = low.exactness.operators.iter().map(|o| o.key).collect();
        assert!(used.contains(&"group_pass_through_opacity"));
        assert_eq!(low.exactness.lowest, Exactness::Approximate);

        // An unrecovered blend mode is refused by evaluation (typed), never lowered as Normal.
        let mut unknown_mode = tiled();
        unknown_mode["layers"][1]["appearance"] = appearance(100.0, 100.0, 31, false);
        let snap = open(&unknown_mode);
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        assert_eq!(
            evaluate(&plan, &snap, &token),
            Err(CompositeError::Blend(hsk_studio_composite::BlendError::Unsupported(
                StudioBlendMode::Average
            )))
        );
    });
}

struct NoClipBackend;
impl RenderBackend for NoClipBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "composite-test-double",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("test_double"))
    }
}

#[test]
fn clipped_layers_lower_to_clip_to_base_and_unsupported_cases_are_refused() {
    bounded_case("clipped_layers_lower_to_clip_to_base_and_unsupported_cases_are_refused", || {
        let root = addr(1, "mix", "out");
        let token = CancellationToken::default();
        let unavailable = |r: Result<hsk_studio_composite::RenderLowering, CompositeError>| match r {
            Err(CompositeError::Unavailable(reason)) => reason,
            other => panic!("expected Unavailable, got {other:?}"),
        };
        let clip = |v: &mut Value, layer_index: usize| {
            v["layers"][layer_index]["appearance"] = appearance(100.0, 100.0, 2, false);
            v["layers"][layer_index]["appearance"]["clipped"] = json!(true);
        };

        // Raster 3 (above base raster 2) and a third raster 6 are both clipped: one shared base.
        let mut v = tiled();
        clip(&mut v, 2);
        v["layers"].as_array_mut().unwrap().push(layer(
            6,
            "raster",
            json!({"payload_kind":"raster","tiles":[tile(6,"t60",0,0,16,16)]}),
        ));
        v["layers"][5]["appearance"] = appearance(100.0, 100.0, 5, false);
        v["layers"][5]["appearance"]["clipped"] = json!(true);
        v["graph"]["operations"].as_array_mut().unwrap().push(source(6, "image"));
        v["graph"]["edges"]
            .as_array_mut()
            .unwrap()
            .push(edge(address(6, "source", "out"), address(1, "mix", "inputs"), 2));
        let low = lowered(&v, BlendSpace::Encoded, &root).unwrap();
        let PlanNode::Group(top) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        assert_eq!(top.layers.len(), 3);
        assert!(top.layers[0].mask.is_none());
        for (n, mode) in [(1, StudioBlendMode::Normal), (2, StudioBlendMode::Multiply)] {
            let layer = top.layers[n];
            assert_eq!(layer.mode, mode);
            assert_eq!(layer.mask, Some(MaskInput::clip_to_base()), "layer {n}");
        }
        assert!(low.mask_sources.is_empty(), "the clip route has no plane to resolve");
        let used: Vec<&str> = low.exactness.operators.iter().map(|o| o.key).collect();
        assert!(used.contains(&"clip_to_base"));
        assert_eq!(low.exactness.lowest, Exactness::Approximate);
        let snap = open(&v);
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let report = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(report.receipt.exactness.lowest, Exactness::Approximate);
        assert_eq!(report.step(&root).unwrap().status, StepStatus::Ready);
        // Clipped content never extends beyond its base: base raster 2 is (0,0)+128x64 and the
        // clipped layers (0,64)+64x64 and (0,0)+16x16 add nothing to the group extent.
        assert_eq!(
            report.step(&root).unwrap().extent,
            Some(Rect { x: 0, y: 0, width: 128, height: 64 })
        );

        // A backend that has not opted in to the route is refused by shared admission, typed.
        let request = ExecuteRequest {
            expected_revision: 7,
            min_exactness: Exactness::Approximate,
            deadline: None,
            max_working_bytes: 1 << 20,
        };
        assert_eq!(
            admit(&low.plan, &NoClipBackend, &request, &token),
            Err(BackendError::Unsupported { operator: "clip_to_base", exactness: Exactness::Unsupported })
        );

        // A hidden base takes its clipped layers with it: only the group's other layers remain.
        let mut hidden_base = tiled();
        clip(&mut hidden_base, 2);
        hidden_base["layers"][1]["visible"] = json!(false);
        let low = lowered(&hidden_base, BlendSpace::Encoded, &root).unwrap();
        let PlanNode::Group(top) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        assert!(top.layers.is_empty());
        assert!(low.sources.is_empty());

        // No base below (clipped bottom layer), own mask, and pass-through group: typed, not ignored.
        let mut bottom = tiled();
        clip(&mut bottom, 1);
        assert!(unavailable(lowered(&bottom, BlendSpace::Encoded, &root)).contains("clip_without_base"));
        let mut own_mask = tiled();
        clip(&mut own_mask, 2);
        own_mask["layers"][2]["appearance"]["mask"] =
            json!({"tiles":[mask_tile(3, "m3")],"enabled":true,"linked":true});
        assert!(unavailable(lowered(&own_mask, BlendSpace::Encoded, &root)).contains("clipped_with_mask_unsupported"));
        let mut group_clip = tiled();
        group_clip["layers"].as_array_mut().unwrap().push(layer(7, "group", json!({"payload_kind":"group"})));
        group_clip["graph"]["operations"].as_array_mut().unwrap().push(composite(7));
        group_clip["graph"]["edges"].as_array_mut().unwrap().extend([
            edge(address(2, "source", "out"), address(7, "mix", "inputs"), 0),
            edge(address(7, "mix", "out"), address(1, "mix", "inputs"), 2),
        ]);
        group_clip["layers"][5]["appearance"] = appearance(100.0, 100.0, 1, true);
        group_clip["layers"][5]["appearance"]["clipped"] = json!(true);
        assert!(unavailable(lowered(&group_clip, BlendSpace::Encoded, &root)).contains("clipped_pass_through_unsupported"));
        // A clipped root has no base either.
        let mut clipped_root = tiled();
        clipped_root["layers"][0]["appearance"] = appearance(100.0, 100.0, 1, true);
        clipped_root["layers"][0]["appearance"]["clipped"] = json!(true);
        assert!(unavailable(lowered(&clipped_root, BlendSpace::Encoded, &root)).contains("clip_without_base"));
    });
}

/// Root group 1 = [group 7 (the base; contains raster 2), raster 3 clipped to it]. Group mode 1 =
/// pass_through (non-isolated), 2 = Normal (isolated).
fn pass_through_base_fixture(mode: u16) -> Value {
    let mut v = tiled();
    v["layers"].as_array_mut().unwrap().push(layer(7, "group", json!({"payload_kind":"group"})));
    v["graph"]["operations"].as_array_mut().unwrap().push(composite(7));
    v["graph"]["edges"] = json!([
        edge(address(2, "source", "out"), address(7, "mix", "inputs"), 0),
        edge(address(7, "mix", "out"), address(1, "mix", "inputs"), 0),
        edge(address(3, "source", "out"), address(1, "mix", "inputs"), 1),
    ]);
    v["layers"][5]["appearance"] = appearance(100.0, 100.0, mode, mode == 1);
    v["layers"][2]["appearance"] = appearance(100.0, 100.0, 2, false);
    v["layers"][2]["appearance"]["clipped"] = json!(true);
    v
}

#[test]
fn pass_through_group_as_clip_base_is_unavailable_and_unsupported_in_the_ledger() {
    bounded_case("pass_through_group_as_clip_base", || {
        // Root group 1 = [group 7 (the base; contains raster 2), raster 3 clipped to it]. Group
        // mode 1 = pass_through (non-isolated), 2 = Normal (isolated). Photoshop semantics for a
        // pass-through clip base are UNVERIFIED (oracle pending): refused, never guessed.
        let root = addr(1, "mix", "out");
        let token = CancellationToken::default();
        let fixture = pass_through_base_fixture;

        // Pass-through base: typed Unavailable at lowering, Unsupported in the evaluation ledger
        // and on the parent step; the key is pigment's PlanError code (test
        // `refusal_keys_are_pigments` shows pigment's validation refuses the rule with it).
        let key = PlanError::ClipBasePassThrough { node: 0 }.code();
        let v = fixture(1);
        match lowered(&v, BlendSpace::Encoded, &root) {
            Err(CompositeError::Unavailable(reason)) => assert!(reason.contains(key), "{reason}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        let snap = open(&v);
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let report = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(report.receipt.exactness.lowest, Exactness::Unsupported);
        assert!(report
            .receipt
            .exactness
            .operators
            .iter()
            .any(|o| o.key == key && o.exactness == Exactness::Unsupported));
        match &report.step(&root).unwrap().status {
            StepStatus::Unavailable(reason) => assert!(reason.contains(key), "{reason}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }

        // A hidden pass-through base takes its clipped layer with it: nothing to refuse.
        let mut hidden = fixture(1);
        hidden["layers"][5]["visible"] = json!(false);
        let low = lowered(&hidden, BlendSpace::Encoded, &root).unwrap();
        let PlanNode::Group(top) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        assert!(top.layers.is_empty());
        let snap = open(&hidden);
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let report = evaluate(&plan, &snap, &token).unwrap();
        assert_ne!(report.receipt.exactness.lowest, Exactness::Unsupported);

        // An isolated (Normal) group base is fine: the clipped layer lowers to the clip route.
        let low = lowered(&fixture(2), BlendSpace::Encoded, &root).unwrap();
        let PlanNode::Group(top) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        assert_eq!(top.layers.len(), 2);
        assert!(top.layers[0].mask.is_none());
        assert_eq!(top.layers[1].mask, Some(MaskInput::clip_to_base()));
    });
}

#[test]
fn hidden_or_empty_clip_base_drops_its_clipped_run_even_when_that_run_is_unavailable() {
    bounded_case("hidden_or_empty_clip_base_drops_its_clipped_run", || {
        // R3: "a hidden or empty base drops its clipped layers" held for lowering but not for
        // evaluation: a clipped layer that is itself Unavailable (here: clipped AND carrying its own
        // mask, one mask slot) blocked the parent although the run is dropped with its base.
        let root = addr(1, "mix", "out");
        let token = CancellationToken::default();
        let clip = |v: &mut Value, i: usize, mask: Option<Value>| {
            v["layers"][i]["appearance"] = appearance(100.0, 100.0, 2, false);
            v["layers"][i]["appearance"]["clipped"] = json!(true);
            if let Some(m) = mask {
                v["layers"][i]["appearance"]["mask"] = m;
            }
        };
        let own_mask = |n: u8, key: &str| json!({"tiles":[mask_tile(n, key)],"enabled":true,"linked":true});
        let reason_of = |step: &hsk_studio_composite::StepEval| match &step.status {
            StepStatus::Unavailable(r) => r.clone(),
            StepStatus::Ready => String::new(),
        };
        let eval = |v: &Value| {
            let snap = open(v);
            let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
            let report = evaluate(&plan, &snap, &token).unwrap();
            (plan, report)
        };

        // Hidden base (raster 2), clipped raster 3 with its own mask: the run is dropped.
        let mut hidden = tiled();
        hidden["layers"][1]["visible"] = json!(false);
        clip(&mut hidden, 2, Some(own_mask(3, "m3")));
        let (_, report) = eval(&hidden);
        // The clipped layer itself is honestly Unavailable, but the parent is not blocked by it
        // and, with every unclipped input hidden, has no extent.
        assert_eq!(
            reason_of(report.step(&addr(3, "source", "out")).unwrap()),
            "clipped_with_mask_unsupported"
        );
        let parent = report.step(&root).unwrap();
        assert_eq!(parent.status, StepStatus::Ready);
        assert_eq!(parent.extent, None);
        let low = lowered(&hidden, BlendSpace::Encoded, &root).unwrap();
        let PlanNode::Group(top) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        assert!(top.layers.is_empty());
        assert!(low.sources.is_empty());

        // Empty base (visible raster 2 without tiles): the same rule, evaluation and lowering.
        let mut empty = tiled();
        empty["layers"][1]["payload"] = json!({"payload_kind":"raster","tiles":[]});
        clip(&mut empty, 2, Some(own_mask(3, "m3")));
        let (_, report) = eval(&empty);
        assert_eq!(report.step(&addr(2, "source", "out")).unwrap().status, StepStatus::Ready);
        assert_eq!(report.step(&addr(2, "source", "out")).unwrap().extent, None);
        assert_eq!(report.step(&root).unwrap().status, StepStatus::Ready);
        assert_eq!(report.step(&root).unwrap().extent, None);
        let low = lowered(&empty, BlendSpace::Encoded, &root).unwrap();
        let PlanNode::Group(top) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        assert!(top.layers.is_empty());

        // Control: a visible, non-empty base keeps the refusal -- the unavailable clipped layer
        // blocks the parent (typed), in evaluation and in lowering.
        let mut live = tiled();
        clip(&mut live, 2, Some(own_mask(3, "m3")));
        let (_, report) = eval(&live);
        let reason = reason_of(report.step(&root).unwrap());
        assert!(reason.starts_with("input_unavailable:") && reason.contains(&identity("SLYR", 3)), "{reason}");
        match lowered(&live, BlendSpace::Encoded, &root) {
            Err(CompositeError::Unavailable(r)) => assert!(r.contains("clipped_with_mask_unsupported"), "{r}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }

        // A later unclipped input starts a new run: [hidden base 2, clipped 3 (unavailable, dropped),
        // visible base 6, clipped 7]. Layer 7 clipped to base 6 is live again.
        let four = |y_mask: Option<Value>| {
            let mut v = tiled();
            v["layers"][1]["visible"] = json!(false);
            clip(&mut v, 2, Some(own_mask(3, "m3")));
            for (n, key, edge_n) in [(6u8, "t60", 2u64), (7, "t70", 3)] {
                v["layers"].as_array_mut().unwrap().push(layer(
                    n,
                    "raster",
                    json!({"payload_kind":"raster","tiles":[tile(n, key, 0, 0, 16, 16)]}),
                ));
                v["graph"]["operations"].as_array_mut().unwrap().push(source(n, "image"));
                v["graph"]["edges"]
                    .as_array_mut()
                    .unwrap()
                    .push(edge(address(n, "source", "out"), address(1, "mix", "inputs"), edge_n));
            }
            clip(&mut v, 6, y_mask);
            v
        };
        // Y (layer 7) with its own mask is Unavailable and its base (layer 6) is live: blocks.
        let (_, report) = eval(&four(Some(own_mask(7, "m7"))));
        let reason = reason_of(report.step(&root).unwrap());
        assert!(reason.contains(&identity("SLYR", 7)) && !reason.contains(&identity("SLYR", 3)), "{reason}");
        // Y without a mask: root Ready with the extent of base 6 only (clipped layers add none and
        // hidden base 2 adds none); lowering keeps [base 6, clipped 7] and drops the clipped 3.
        let v = four(None);
        let (_, report) = eval(&v);
        let parent = report.step(&root).unwrap();
        assert_eq!(parent.status, StepStatus::Ready);
        assert_eq!(parent.extent, Some(Rect { x: 0, y: 0, width: 16, height: 16 }));
        let low = lowered(&v, BlendSpace::Encoded, &root).unwrap();
        let PlanNode::Group(top) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        assert_eq!(top.layers.len(), 2);
        assert!(top.layers[0].mask.is_none());
        assert_eq!(top.layers[1].mask, Some(MaskInput::clip_to_base()));
        assert_eq!(low.sources.len(), 2);
    });
}

// ---------------------------------------------------------------------------------------------
// Adjustment layers, mask parameters, Blend-If, knockout and effect stacks (lane
// SMB-COMPOSITE-ADJ-1). Expected values are hand-derived from the fixtures and the brief; the
// envelope types and operator keys are pigment's, never re-typed here as literals.
// ---------------------------------------------------------------------------------------------
use hsk_studio_composite::{
    EvalReport, ExtendedLowering, LoweredPlan, SourceValue, Step, StepAppearance, eval_outcome,
    evaluate_extended, lower_to_render_plan_extended,
};
use hsk_studio_folio::{Resolution, Resolver, Snapshot, StudioRasterRecords};
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_pigment::adjust_math::{AdjustmentKind, AdjustmentOp};
use hsk_studio_pigment::blend_math::{
    BlendContext, OperatorExactness, PixelSite, Rgba, StackNode, evaluate_stack,
};
use hsk_studio_pigment::coverage_math::BlendChannel;
use hsk_studio_pigment::render_plan::{
    ExtOperator, Knockout, LayerExtension, PASS_THROUGH_FILL_OPERATOR, PASS_THROUGH_MASK_OPERATOR,
    admit_extended,
};

/// Folio resolver that binds the `adjustment` layer kind to its primitive schema.
struct WithAdjustment;
impl Resolver for WithAdjustment {
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
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        kind == "adjustment" && schema_id == "hsk.studio.adjustment@1"
    }
}

/// A backend double that opts in to every envelope operator (pigment still lowers a class by the
/// operator's intrinsic class, so knockout stays refused).
struct ExtBackend;
impl RenderBackend for ExtBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "composite-test-double",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn supports_clip_to_base(&self) -> bool {
        true
    }
    fn extension_exactness(&self, _: ExtOperator) -> Exactness {
        Exactness::Exact
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("test_double"))
    }
}
fn request() -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 7,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}

/// Group 1 (root) composites raster 2 (tile t20, 64x64 at 0,0) then raster 4 (tile t40, 32x32 at
/// 0,0). Adjustment layer 3 lives in the document but, as in Folio today, has no graph operation.
fn adjusted() -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["layers"] = json!([
        layer(1, "group", json!({"payload_kind":"group"})),
        layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2,"t20",0,0,64,64)]})),
        layer(3, "adjustment", json!({"payload_kind":"primitive_reference",
            "schema_id":"hsk.studio.adjustment@1","object_id":identity("SLYR",3)})),
        layer(4, "raster", json!({"payload_kind":"raster","tiles":[tile(4,"t40",0,0,32,32)]})),
    ]);
    v["graph"] = json!({"schema_id":"hsk.studio.layer_graph@1",
        "operations":[source(2,"image"),source(4,"image"),composite(1)],
        "edges":[edge(address(2,"source","out"),address(1,"mix","inputs"),0),
                 edge(address(4,"source","out"),address(1,"mix","inputs"),1)],
        "outputs":[address(1,"mix","out")]});
    v
}
fn in_group_1() -> Value {
    json!({"node_kind":"layer","node_id":identity("SLYR",1)})
}
fn records(layers: Value) -> StudioRasterRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1",
        "document_id":identity("SDOC",1),"layers":layers}))
    .expect("records shape")
}
/// Snapshot, plan and extended evaluation. `with_adjustment_step` splices the adjustment layer
/// into group 1 between its two rasters, as a graph with an adjustment operation would.
fn run_ext(
    v: &Value,
    recs: &StudioRasterRecords,
    with_adjustment_step: bool,
) -> (Snapshot, LoweredPlan, EvalReport) {
    let snap = open_bytes(&serde_json::to_vec(v).unwrap(), &WithAdjustment);
    let mut plan = plan_of(&snap);
    if with_adjustment_step {
        let at = plan.steps.len() - 1;
        plan.steps.insert(
            at,
            Step::Source {
                addr: addr(3, "source", "out"),
                layer_kind: "adjustment".into(),
                value: SourceValue::Image,
                visible: true,
                appearance: StepAppearance::identity(false),
            },
        );
        let Some(Step::Composite { inputs, .. }) = plan.steps.last_mut() else {
            panic!("group step is last")
        };
        inputs.insert(1, addr(3, "source", "out"));
    }
    let report = evaluate_extended(&plan, &snap, recs, &CancellationToken::default()).unwrap();
    (snap, plan, report)
}
fn lower_ext(
    run: &(Snapshot, LoweredPlan, EvalReport),
    recs: &StudioRasterRecords,
) -> Result<ExtendedLowering, CompositeError> {
    lower_to_render_plan_extended(
        &run.1,
        &run.2,
        &run.0,
        recs,
        &addr(1, "mix", "out"),
        target(BlendSpace::Encoded),
        &Info,
    )
}
fn unavailable_reason(r: Result<ExtendedLowering, CompositeError>) -> String {
    match r {
        Err(CompositeError::Unavailable(reason)) => reason,
        other => panic!("expected Unavailable, got {other:?}"),
    }
}
fn assert_close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
}

#[test]
fn adjustment_layer_lowers_to_transparent_placeholder_and_envelope_entry() {
    bounded_case("adjustment_layer_lowers_to_transparent_placeholder_and_envelope_entry", || {
        let recs = records(json!([{"layer_id":identity("SLYR",3),"adjustment":{"kind":"invert"}}]));
        let run = run_ext(&adjusted(), &recs, true);
        let root = addr(1, "mix", "out");
        let adjustment = run.2.step(&addr(3, "source", "out")).unwrap();
        assert_eq!((adjustment.status.clone(), adjustment.extent), (StepStatus::Ready, None));
        // Group extent = union of the 64x64 and 32x32 tiles at the origin = 64x64.
        let area = Rect { x: 0, y: 0, width: 64, height: 64 };
        assert_eq!(run.2.step(&root).unwrap().extent, Some(area));

        let low = lower_ext(&run, &recs).unwrap();
        // Emission order: tile(2) = node 0, tile(4) = node 1, placeholder = node 2, root = node 3.
        assert_eq!(low.plan.nodes.len(), 4);
        assert_eq!(low.plan.nodes[2], PlanNode::Solid { rect: area, rgba: [0.0; 4] });
        let PlanNode::Group(top) = &low.plan.nodes[3] else { panic!("node 3 must be the root") };
        assert!(top.isolated);
        assert_eq!(
            top.layers.iter().map(|l| l.input).collect::<Vec<_>>(),
            [PlanNodeId(0), PlanNodeId(2), PlanNodeId(1)]
        );
        assert_eq!(low.plan.root, PlanNodeId(3));
        assert_eq!(
            low.extensions.layers,
            vec![LayerExtension::new(PlanNodeId(3), 1).with_adjustment(AdjustmentOp::Invert)]
        );
        assert_eq!(
            low.extensions.validate(&low.plan),
            Ok(vec![ExtOperator::AdjustmentLayer, ExtOperator::Adjustment(AdjustmentKind::Invert)])
        );
        assert_eq!(low.sources.len(), 2, "the placeholder has no source");
        assert_eq!(low.exactness.lowest, Exactness::Approximate);
        let admitted = admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &CancellationToken::default());
        assert!(admitted.is_ok(), "{admitted:?}");

        // A pass-through (non-isolated) group acts on the parent backdrop: the placeholder covers
        // the whole 128x128 target, not just the group's own content.
        let mut faded = adjusted();
        faded["layers"][0]["appearance"] = appearance(50.0, 100.0, 1, true);
        let run_faded = run_ext(&faded, &recs, true);
        let low = lower_ext(&run_faded, &recs).unwrap();
        assert_eq!(
            low.plan.nodes[2],
            PlanNode::Solid { rect: Rect { x: 0, y: 0, width: 128, height: 128 }, rgba: [0.0; 4] }
        );
        assert_eq!(low.extensions.layers[0].group, PlanNodeId(3));

        // Without records the v1 evaluation refuses the adjustment layer, typed, never skipped.
        let snap = open_bytes(&serde_json::to_vec(&adjusted()).unwrap(), &WithAdjustment);
        let mut plan = run.1.clone();
        plan.revision = snap.document().revision;
        let v1 = evaluate(&plan, &snap, &CancellationToken::default()).unwrap();
        assert_eq!(
            v1.step(&addr(3, "source", "out")).unwrap().status,
            StepStatus::Unavailable("unsupported_payload".into())
        );

        // An adjustment layer without a graph operation has no stack position (its input_ordinal
        // would be it): a visible one contained in the rendered group is refused, never dropped.
        // (With a graph operation it is placed: `adjustment_layer_in_a_real_folio_graph_...`.)
        let mut contained = adjusted();
        contained["layers"][2]["parent"] = in_group_1();
        let real = run_ext(&contained, &recs, false);
        let reason = unavailable_reason(lower_ext(&real, &recs));
        assert!(reason.ends_with("adjustment_not_in_composition_graph"), "{reason}");
        assert!(reason.starts_with(&identity("SLYR", 3)), "{reason}");

        // A clipped layer directly above an adjustment layer has no base alpha: refused by
        // evaluation (parent step + ledger, pigment's code) and by lowering.
        let mut clipped = adjusted();
        clipped["layers"][3]["appearance"] = appearance(100.0, 100.0, 2, false);
        clipped["layers"][3]["appearance"]["clipped"] = json!(true);
        let run_clipped = run_ext(&clipped, &recs, true);
        match &run_clipped.2.step(&root).unwrap().status {
            StepStatus::Unavailable(reason) => assert!(reason.contains("clip_base_adjustment"), "{reason}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert!(run_clipped.2.receipt.exactness.operators.iter().any(|o| o.key
            == PlanError::AdjustmentClipBase { node: 0 }.code()
            && o.exactness == Exactness::Unsupported));
        assert!(unavailable_reason(lower_ext(&run_clipped, &recs)).contains("clip_base_adjustment"));

        // Brightness/contrast carries `use_legacy`; the modern curve is unpublished, so it is
        // carried too and refused by pigment's admission under the kind's key (never emulated),
        // with the ledger `Unsupported`. The legacy mode is admitted.
        let bc = |legacy: bool| {
            records(json!([{"layer_id":identity("SLYR",3),"adjustment":{"kind":"brightness_contrast",
                "brightness":10.0,"contrast":10.0,"use_legacy":legacy}}]))
        };
        let (modern, legacy) = (bc(false), bc(true));
        let token = CancellationToken::default();
        let low = lower_ext(&run_ext(&adjusted(), &modern, true), &modern).unwrap();
        assert_eq!(
            admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token),
            Err(BackendError::Unsupported { operator: "brightness_contrast", exactness: Exactness::Unsupported })
        );
        assert_eq!(low.exactness.lowest, Exactness::Unsupported);
        let low = lower_ext(&run_ext(&adjusted(), &legacy, true), &legacy).unwrap();
        assert!(admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token).is_ok());
        // Hue bands with a shift and non-default threshold colours have no pigment counterpart;
        // all-zero bands (PSD writes six defaults) are the identity.
        let hs = |band_shift: f64| {
            let zero = json!({"hue":0.0,"saturation":0.0,"lightness":0.0});
            records(json!([{"layer_id":identity("SLYR",3),"adjustment":{"kind":"hue_saturation",
                "master":{"hue":10.0,"saturation":0.0,"lightness":0.0},"colorize":false,"colorization":zero,
                "bands":[{"range":[0.0,0.0,30.0,60.0],"shift":{"hue":band_shift,"saturation":0.0,"lightness":0.0}}]}}]))
        };
        let (flat, shifted) = (hs(0.0), hs(5.0));
        assert!(lower_ext(&run_ext(&adjusted(), &flat, true), &flat).is_ok());
        assert!(unavailable_reason(lower_ext(&run_ext(&adjusted(), &shifted, true), &shifted))
            .ends_with("hue_saturation_bands"));
        let th = records(json!([{"layer_id":identity("SLYR",3),"adjustment":{"kind":"threshold","level":128.0,
            "true_colour":{"r":1.0,"g":0.0,"b":0.0,"colour_profile_id":null}}}]));
        assert!(unavailable_reason(lower_ext(&run_ext(&adjusted(), &th, true), &th)).ends_with("threshold_colours"));
    });
}

#[test]
fn mask_parameters_lower_to_density_invert_and_feather() {
    bounded_case("mask_parameters_lower_to_density_invert_and_feather", || {
        // tiled(): layer 2 (tiles t00, t10) and layer 3 in group 1. Layer 2 gets an enabled mask.
        let mut v = tiled();
        v["layers"][1]["appearance"] = appearance(100.0, 100.0, 2, false);
        v["layers"][1]["appearance"]["mask"] =
            json!({"tiles":[mask_tile(2, "m0")],"enabled":true,"linked":true});
        let mut recs = records(json!([{
            "layer_id":identity("SLYR",2),"mask":{"density":50.0,"feather":4.0,"inverted":true}}]));
        let snap = open(&v);
        let token = CancellationToken::default();
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let report = evaluate_extended(&plan, &snap, &recs, &token).unwrap();
        let root = addr(1, "mix", "out");
        let go = |recs: &StudioRasterRecords| {
            lower_to_render_plan_extended(&plan, &report, &snap, recs, &root, target(BlendSpace::Encoded), &Info)
        };
        let low = go(&recs).unwrap();
        // Nodes: tiles 0,1 -> group 2 (layer 2), tile 3 (layer 3), root 4. Density 50 percent is
        // 0.5, the invert flag is carried, feather 4.0 px rides in the envelope.
        let PlanNode::Group(top) = &low.plan.nodes[4] else { panic!("node 4 must be the root") };
        let mask = top.layers[0].mask.expect("enabled mask");
        assert_eq!((mask.density, mask.inverted), (0.5, true));
        assert_eq!(mask.format, MaskFormat::CoverageU8);
        assert_eq!(
            low.extensions.layers,
            vec![LayerExtension::new(PlanNodeId(4), 0).with_mask_feather(4.0)]
        );
        // Feather is reported before the density capability, which `validate` appends last.
        assert_eq!(
            low.extensions.validate(&low.plan),
            Ok(vec![ExtOperator::MaskFeather, ExtOperator::MaskDensity])
        );
        // v1 admission keeps refusing density (its key is the one both backends report); the
        // envelope path admits it for a backend that opts in.
        assert_eq!(
            admit(&low.plan, &ExtBackend, &request(), &token),
            Err(BackendError::Unsupported { operator: "mask_density", exactness: Exactness::Unsupported })
        );
        assert!(admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token).is_ok());
        assert_eq!(low.exactness.lowest, Exactness::Approximate);

        // The v1 lowering of the same document is unchanged: density 1, not inverted, no envelope.
        let v1 = lower_to_render_plan(&plan, &report, &snap, &root, target(BlendSpace::Encoded), &Info).unwrap();
        let PlanNode::Group(v1_top) = &v1.plan.nodes[4] else { panic!("root") };
        let v1_mask = v1_top.layers[0].mask.unwrap();
        assert_eq!((v1_mask.density, v1_mask.inverted), (1.0, false));

        // A disabled mask is an authored "no mask": its parameters change nothing.
        let mut disabled = v.clone();
        disabled["layers"][1]["appearance"]["mask"]["enabled"] = json!(false);
        let snap_d = open(&disabled);
        let plan_d = lower(&snap_d, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let report_d = evaluate_extended(&plan_d, &snap_d, &recs, &token).unwrap();
        let low_d = lower_to_render_plan_extended(
            &plan_d, &report_d, &snap_d, &recs, &root, target(BlendSpace::Encoded), &Info,
        )
        .unwrap();
        assert!(low_d.extensions.is_empty() && low_d.mask_sources.is_empty());

        // Records of another document are refused, never applied.
        recs.document_id = identity("SDOC", 2);
        assert_eq!(
            go(&recs).unwrap_err(),
            CompositeError::UnsupportedDescriptor("raster_records_document_mismatch".into())
        );
    });
}

#[test]
fn blend_if_ranges_lower_in_pigment_order_and_units() {
    bounded_case("blend_if_ranges_lower_in_pigment_order_and_units", || {
        let ranges = json!({"blend_ranges":[
            {"channel":"gray","source_black":[50.0,100.0],"source_white":[200.0,240.0],
             "destination_black":[0.0,0.0],"destination_white":[255.0,255.0]},
            {"channel":"red","source_black":[0.0,0.0],"source_white":[255.0,255.0],
             "destination_black":[10.0,20.0],"destination_white":[230.0,250.0]},
            {"channel":"blue","source_black":[0.0,0.0],"source_white":[255.0,255.0],
             "destination_black":[0.0,0.0],"destination_white":[255.0,255.0]}],
            "knockout":"disabled","channel_blend":{"red":true,"green":true,"blue":true},
            "blend_interior_effects":false,"blend_clipped_effects":false,"transparency_shapes_layer":true});
        // Layer 3 (second input of group 1) carries the ranges; layer 2 carries only identity
        // ranges, which change nothing and add no entry.
        let identity_only = json!({"blend_ranges":[
            {"channel":"gray","source_black":[0.0,0.0],"source_white":[255.0,255.0],
             "destination_black":[0.0,0.0],"destination_white":[255.0,255.0]}],
            "knockout":"unknown","channel_blend":{"red":true,"green":true,"blue":true},
            "blend_interior_effects":false,"blend_clipped_effects":false,"transparency_shapes_layer":true});
        let recs = records(json!([
            {"layer_id":identity("SLYR",2),"advanced_blending":identity_only},
            {"layer_id":identity("SLYR",3),"advanced_blending":ranges}]));
        let snap = open(&tiled());
        let token = CancellationToken::default();
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let report = evaluate_extended(&plan, &snap, &recs, &token).unwrap();
        let low = lower_to_render_plan_extended(
            &plan, &report, &snap, &recs, &addr(1, "mix", "out"), target(BlendSpace::Encoded), &Info,
        )
        .unwrap();
        // Root group = node 4; layer 3 is its layer 1. Values are level / 255, order
        // [black_start, black_end, white_start, white_end]; the identity blue entry is dropped.
        assert_eq!(low.extensions.layers.len(), 1);
        let e = &low.extensions.layers[0];
        assert_eq!((e.group, e.layer), (PlanNodeId(4), 1));
        assert_eq!(e.blend_if.len(), 2);
        assert_eq!((e.blend_if[0].channel, e.blend_if[1].channel), (BlendChannel::Gray, BlendChannel::Red));
        let want_source = [0.196_078_4_f32, 0.392_156_9, 0.784_313_7, 0.941_176_5]; // 50 100 200 240 / 255
        for (got, want) in e.blend_if[0].source.iter().zip(want_source) {
            assert_close(*got, want);
        }
        assert_eq!(e.blend_if[0].destination, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(e.blend_if[1].source, [0.0, 0.0, 1.0, 1.0]);
        let want_destination = [0.039_215_69_f32, 0.078_431_37, 0.901_960_8, 0.980_392_2]; // 10 20 230 250 / 255
        for (got, want) in e.blend_if[1].destination.iter().zip(want_destination) {
            assert_close(*got, want);
        }
        assert_eq!(low.extensions.validate(&low.plan), Ok(vec![ExtOperator::BlendIf]));
        assert!(admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token).is_ok());
    });
}

#[test]
fn knockout_and_effect_stack_are_refused() {
    bounded_case("knockout_and_effect_stack_are_refused", || {
        let blending = |knockout: &str| {
            json!({"blend_ranges":[],"knockout":knockout,
                "channel_blend":{"red":true,"green":true,"blue":true},
                "blend_interior_effects":false,"blend_clipped_effects":false,"transparency_shapes_layer":true})
        };
        let token = CancellationToken::default();
        let flow = |v: &Value, recs: &StudioRasterRecords| {
            let snap = open(v);
            let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
            let report = evaluate_extended(&plan, &snap, recs, &token).unwrap();
            let low = lower_to_render_plan_extended(
                &plan, &report, &snap, recs, &addr(1, "mix", "out"), target(BlendSpace::Encoded), &Info,
            );
            (report, low)
        };

        // Knockout enabled on layer 3 is carried as Shallow; disabled / unknown on others is None.
        let recs = records(json!([
            {"layer_id":identity("SLYR",2),"advanced_blending":blending("disabled")},
            {"layer_id":identity("SLYR",3),"advanced_blending":blending("enabled")}]));
        let (report, low) = flow(&tiled(), &recs);
        let low = low.unwrap();
        assert_eq!(
            low.extensions.layers,
            vec![LayerExtension::new(PlanNodeId(4), 1).with_knockout(Knockout::Shallow)]
        );
        // Lane SMB-PS-ADJUST-STYLES-2: a shallow knockout in the ROOT group knocks out to the
        // transparent document base, so pigment's admission accepts it (refused before that lane).
        assert!(admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token).is_ok());
        // The lowering and the evaluation report pigment's class for the operator.
        assert_eq!(low.exactness.lowest, Exactness::Approximate);
        assert_eq!(report.receipt.exactness.lowest, Exactness::Approximate);

        // Inherited resolves through containment: layer 2 inherits from group 1 (enabled), the
        // group itself composites into nothing but is wrapped so its own entry is kept. Layer 3
        // (no record) is None. Entries: group node 4 layer 0, wrapper node 5 layer 0.
        let mut nested = tiled();
        nested["layers"][1]["parent"] = in_group_1();
        let recs = records(json!([
            {"layer_id":identity("SLYR",1),"advanced_blending":blending("enabled")},
            {"layer_id":identity("SLYR",2),"advanced_blending":blending("inherited")}]));
        let (_, low) = flow(&nested, &recs);
        assert_eq!(
            low.unwrap().extensions.layers,
            vec![
                LayerExtension::new(PlanNodeId(4), 0).with_knockout(Knockout::Shallow),
                LayerExtension::new(PlanNodeId(5), 0).with_knockout(Knockout::Shallow),
            ]
        );
        // Inherited with nothing to inherit from is not enabled.
        let none = records(json!([{"layer_id":identity("SLYR",2),"advanced_blending":blending("inherited")}]));
        assert!(flow(&nested, &none).1.unwrap().extensions.is_empty());

        // An enabled effect instance of a kind pigment does not render (here a pattern overlay;
        // satin rendered since lane SMB-PS-ADJUST-STYLES) is refused; a
        // disabled stack, or one without enabled instances, is not.
        let stack = |enabled: bool, instance_enabled: bool| {
            json!({"effect_stack_id":"SEFX-019abcde-0000-7000-8000-000000000001","enabled":enabled,"scale":100.0,
                "effects":[{"instance_id":"fx_1","enabled":instance_enabled,"visible_in_editor":true,
                    "blend_mode":5,"opacity":75.0,"colour":{"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":null},
                    "anti_alias":false,"noise":0.0,"angle":120.0,"use_global_light":true,
                    "params":{"effect_kind":"pattern_overlay","pattern":{"schema_id":"hsk.studio.pattern@1","object_id":"p1"},"scale":100.0,"phase_x":0.0,"phase_y":0.0,"link_with_layer":true}}]})
        };
        let with = |enabled, instance_enabled| {
            records(json!([{"layer_id":identity("SLYR",3),"effects":stack(enabled, instance_enabled)}]))
        };
        let (report, low) = flow(&tiled(), &with(true, true));
        let reason = unavailable_reason(low);
        assert!(reason.ends_with("layer_effects_not_rendered"), "{reason}");
        assert!(reason.contains(&identity("SLYR", 3)), "{reason}");
        assert!(matches!(
            &report.step(&addr(3, "source", "out")).unwrap().status,
            StepStatus::Unavailable(r) if r == "layer_effects_not_rendered"
        ));
        assert!(flow(&tiled(), &with(false, true)).1.is_ok());
        assert!(flow(&tiled(), &with(true, false)).1.is_ok());
    });
}

#[test]
fn evaluation_ledger_keys_and_classes_are_pigments() {
    bounded_case("evaluation_ledger_keys_and_classes_are_pigments", || {
        // One document that uses every envelope operator: adjustment 3 (invert), a masked layer 2
        // with density 50 and feather 4, and layer 4 with Blend-If and knockout.
        let mut v = adjusted();
        v["layers"][1]["appearance"] = appearance(100.0, 100.0, 2, false);
        v["layers"][1]["appearance"]["mask"] =
            json!({"tiles":[mask_tile(2, "m0")],"enabled":true,"linked":true});
        let recs = records(json!([
            {"layer_id":identity("SLYR",2),"mask":{"density":50.0,"feather":4.0,"inverted":false}},
            {"layer_id":identity("SLYR",3),"adjustment":{"kind":"invert"}},
            {"layer_id":identity("SLYR",4),"advanced_blending":{
                "blend_ranges":[{"channel":"gray","source_black":[50.0,100.0],"source_white":[200.0,240.0],
                    "destination_black":[0.0,0.0],"destination_white":[255.0,255.0]}],
                "knockout":"enabled","channel_blend":{"red":true,"green":true,"blue":true},
                "blend_interior_effects":false,"blend_clipped_effects":false,"transparency_shapes_layer":true}}]));
        let (_, _, report) = run_ext(&v, &recs, true);
        let ledger = &report.receipt.exactness;
        // Grep-free equality against pigment's own constants: key AND class come from the operator.
        for op in [
            ExtOperator::AdjustmentLayer,
            ExtOperator::Adjustment(AdjustmentKind::Invert),
            ExtOperator::BlendIf,
            ExtOperator::MaskDensity,
            ExtOperator::MaskFeather,
            ExtOperator::Knockout(Knockout::Shallow),
        ] {
            let want = OperatorExactness { key: op.key(), exactness: op.intrinsic() };
            assert!(ledger.operators.contains(&want), "{want:?} missing from {:?}", ledger.operators);
        }
        // Knockout shallow is Approximate since lane SMB-PS-ADJUST-STYLES (pigment knockout_math).
        assert_eq!(ledger.lowest, Exactness::Approximate, "every envelope operator here is Approximate or better");
        assert_ne!(eval_outcome(&Ok(report)), Outcome::Failure(FailureCode::Unsupported));

        // The v1 operators composite used to hand-code are pigment's too: a fill-scaled non-Normal
        // layer, a pass-through group below full opacity, and a clipped layer.
        let mut w = tiled();
        w["layers"][1]["appearance"] = appearance(100.0, 25.0, 5, false);
        w["layers"][0]["appearance"] = appearance(50.0, 100.0, 1, true);
        let snap = open(&w);
        let token = CancellationToken::default();
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let v1 = evaluate(&plan, &snap, &token).unwrap();
        for op in [Operator::FillOpacity, Operator::PassThroughOpacity] {
            let want = OperatorExactness { key: op.key(), exactness: op.intrinsic() };
            assert!(v1.receipt.exactness.operators.contains(&want), "{want:?}");
        }
        // A pass-through group with fill below 1 is refused by every backend: pigment's key,
        // class Unsupported, and the step is Unavailable (no plan is emitted that admit refuses).
        let mut f = tiled();
        f["layers"][0]["appearance"] = appearance(100.0, 25.0, 1, true);
        let snap = open(&f);
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let report = evaluate(&plan, &snap, &token).unwrap();
        assert!(report.receipt.exactness.operators.contains(&OperatorExactness {
            key: PASS_THROUGH_FILL_OPERATOR,
            exactness: Exactness::Unsupported
        }));
        assert_eq!(
            report.step(&addr(1, "mix", "out")).unwrap().status,
            StepStatus::Unavailable(PASS_THROUGH_FILL_OPERATOR.into())
        );
        assert_eq!(plan.lowest_exactness(), Exactness::Unsupported);
    });
}

// ---------------------------------------------------------------------------------------------
// Layer effects wave 1 (drop shadow, colour overlay, solid stroke): lowering of folio
// `StudioEffectStack` records to pigment `EffectOp`s, checked against pigment's own
// `render_layer_reference` on small planes (hand-derived pixels), and the typed refusals.
// ---------------------------------------------------------------------------------------------
use hsk_studio_pigment::effect_math::{
    ColourOverlayOp, DropShadowOp, EffectOp, EffectPlane, LayerParams, StrokeOp,
    StrokePosition as PigmentStrokePosition, render_layer_reference,
};

fn fx_colour(r: f64, g: f64, b: f64) -> Value {
    json!({"r":r,"g":g,"b":b,"colour_profile_id":null})
}
/// A Normal-mode effect instance (blend_mode 2) with the given kind block.
fn fx_effect(id: &str, opacity: f64, colour: Value, global_light: bool, params: Value) -> Value {
    json!({"instance_id":id,"enabled":true,"visible_in_editor":true,"blend_mode":2,"opacity":opacity,
        "colour":colour,"anti_alias":false,"noise":0.0,"angle":0.0,"use_global_light":global_light,
        "params":params})
}
fn fx_stack(effects: Vec<Value>) -> Value {
    json!({"effect_stack_id":"SEFX-019abcde-0000-7000-8000-000000000001","enabled":true,"scale":100.0,
        "effects":effects})
}
fn fx_shadow(distance: f64, spread: f64, size: f64) -> Value {
    json!({"effect_kind":"drop_shadow","distance":distance,"spread":spread,"size":size,"layer_knocks_out":false})
}
fn fx_stroke(size: f64, position: &str) -> Value {
    json!({"effect_kind":"stroke","size":size,"position":position,"fill_type":"solid","overprint":false})
}
fn fx_records(global_light: Option<f64>, layers: Value) -> StudioRasterRecords {
    let mut v = json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),"layers":layers});
    if let Some(angle) = global_light {
        v["global_light"] = json!({"angle":angle,"altitude":30.0});
    }
    serde_json::from_value(v).expect("records shape")
}
fn plane_with(w: u32, h: u32, covered: &[(u32, u32)], rgb: [f32; 3]) -> EffectPlane {
    let n = (w * h) as usize;
    let (mut colours, mut alpha) = (vec![[0.0; 3]; n], vec![0.0; n]);
    for (x, y) in covered {
        let i = (y * w + x) as usize;
        colours[i] = rgb;
        alpha[i] = 1.0;
    }
    EffectPlane::new(w, h, colours, alpha).unwrap()
}
fn assert_pixel(plane: &EffectPlane, x: u32, y: u32, rgb: Option<[f32; 3]>, alpha: f32) {
    let p = plane.pixel(x, y).unwrap();
    assert_close(p.a, alpha);
    if let Some(rgb) = rgb {
        for (got, want) in p.rgb.iter().zip(rgb) {
            assert_close(*got, want);
        }
    }
}
fn flow_space(
    v: &Value,
    recs: &StudioRasterRecords,
    space: BlendSpace,
) -> (EvalReport, Result<ExtendedLowering, CompositeError>) {
    let snap = open_bytes(&serde_json::to_vec(v).unwrap(), &WithAdjustment);
    let token = CancellationToken::default();
    let plan = lower(&snap, 7, space, &Limits::default(), &token).unwrap();
    let report = evaluate_extended(&plan, &snap, recs, &token).unwrap();
    let low = lower_to_render_plan_extended(
        &plan, &report, &snap, recs, &addr(1, "mix", "out"), target(space), &Info,
    );
    (report, low)
}

#[test]
fn effect_stack_lowers_to_pigment_ops_that_render_as_derived() {
    bounded_case("effect_stack_lowers_to_pigment_ops_that_render_as_derived", || {
        // Layer 2: a drop shadow that follows Global Light (180 degrees), distance 2, opacity 50
        // percent, black. Layer 3: a red colour overlay at 50 percent, then a white stroke of 1 px
        // outside, both Normal. Percent become fractions; the list order is preserved.
        let recs = fx_records(
            Some(180.0),
            json!([
                {"layer_id":identity("SLYR",2),"effects":fx_stack(vec![
                    fx_effect("ds_1", 50.0, fx_colour(0.0,0.0,0.0), true, fx_shadow(2.0, 0.0, 0.0))])},
                {"layer_id":identity("SLYR",3),"effects":fx_stack(vec![
                    fx_effect("co_1", 50.0, fx_colour(1.0,0.0,0.0), false, json!({"effect_kind":"colour_overlay"})),
                    fx_effect("st_1", 100.0, fx_colour(1.0,1.0,1.0), false, fx_stroke(1.0, "outside"))])}]),
        );
        let (report, low) = flow_space(&tiled(), &recs, BlendSpace::Encoded);
        let low = low.unwrap();
        let normal = StudioBlendMode::Normal;
        let shadow = EffectOp::DropShadow(DropShadowOp::new([0.0; 3], normal, 0.5).with_offset(180.0, 2.0));
        let overlay = EffectOp::ColourOverlay(ColourOverlayOp::new([1.0, 0.0, 0.0], normal, 0.5));
        let stroke = EffectOp::Stroke(StrokeOp::new([1.0; 3], normal, 1.0, 1.0, PigmentStrokePosition::Outside));
        assert_eq!(
            low.extensions.layers,
            vec![
                LayerExtension::new(PlanNodeId(4), 0).with_effects(vec![shadow]),
                LayerExtension::new(PlanNodeId(4), 1).with_effects(vec![overlay, stroke]),
            ]
        );
        // The envelope validates, a backend that opts in admits it, and both ledgers list the
        // effect kinds under pigment's keys and class (Approximate).
        let token = CancellationToken::default();
        assert_eq!(
            low.extensions.validate(&low.plan),
            Ok(vec![
                ExtOperator::Effect(hsk_studio_pigment::effect_math::EffectKind::DropShadow),
                ExtOperator::Effect(hsk_studio_pigment::effect_math::EffectKind::ColourOverlay),
                ExtOperator::Effect(hsk_studio_pigment::effect_math::EffectKind::Stroke),
            ])
        );
        assert!(admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token).is_ok());
        for ledger in [&low.exactness, &report.receipt.exactness] {
            // Wave 1 only: pigment may know more kinds than composite lowers.
            use hsk_studio_pigment::effect_math::EffectKind;
            for kind in [EffectKind::DropShadow, EffectKind::ColourOverlay, EffectKind::Stroke] {
                let op = ExtOperator::Effect(kind);
                assert!(
                    ledger.operators.iter().any(|o| o.key == op.key() && o.exactness == op.intrinsic()),
                    "{} missing from {:?}",
                    op.key(),
                    ledger.operators
                );
            }
            assert_eq!(ledger.lowest, Exactness::Approximate);
        }
        // A backend without effect support is refused before any sink call, by the effect key.
        assert_eq!(
            admit_extended(&low.plan, &low.extensions, &NoClipBackend, &request(), &token),
            Err(BackendError::Unsupported { operator: "drop_shadow", exactness: Exactness::Unsupported })
        );

        // Pigment's own reference render of the lowered ops (6 x 3 plane, a 2 x 1 blue bar at
        // x = 1..2, y = 1). Angle 180 and distance 2 move the matte by (+2, 0): black at alpha 0.5
        // on x = 3..4, none elsewhere; the bar stays opaque blue.
        let blue = [0.0, 0.0, 1.0];
        let bar = plane_with(6, 3, &[(1, 1), (2, 1)], blue);
        let none = EffectPlane::transparent(6, 3).unwrap();
        let out = render_layer_reference(&none, &bar, LayerParams::plain(), &low.extensions.layers[0].effects).unwrap();
        assert_pixel(&out, 1, 1, Some(blue), 1.0);
        assert_pixel(&out, 2, 1, Some(blue), 1.0);
        assert_pixel(&out, 3, 1, Some([0.0; 3]), 0.5);
        assert_pixel(&out, 4, 1, Some([0.0; 3]), 0.5);
        assert_pixel(&out, 5, 1, None, 0.0);
        assert_pixel(&out, 0, 1, None, 0.0);
        assert_pixel(&out, 3, 0, None, 0.0);

        // Red at 50 percent over an opaque blue pixel: (0.5, 0, 0.5), still opaque.
        let one = plane_with(1, 1, &[(0, 0)], blue);
        let none = EffectPlane::transparent(1, 1).unwrap();
        let out = render_layer_reference(&none, &one, LayerParams::plain(), &low.extensions.layers[1].effects[..1]).unwrap();
        assert_pixel(&out, 0, 0, Some([0.5, 0.0, 0.5]), 1.0);

        // The stroke (1 px outside, white) around a 3 x 3 square at 2..4 of a 7 x 7 plane: the
        // pixel next to an edge is fully white, two pixels away nothing, the square stays blue.
        let covered: Vec<(u32, u32)> = (2..5).flat_map(|y| (2..5).map(move |x| (x, y))).collect();
        let square = plane_with(7, 7, &covered, blue);
        let none = EffectPlane::transparent(7, 7).unwrap();
        let out = render_layer_reference(&none, &square, LayerParams::plain(), &low.extensions.layers[1].effects[1..]).unwrap();
        assert_pixel(&out, 1, 3, Some([1.0; 3]), 1.0);
        assert_pixel(&out, 5, 3, Some([1.0; 3]), 1.0);
        assert_pixel(&out, 0, 3, None, 0.0);
        assert_pixel(&out, 3, 3, Some(blue), 1.0);
    });
}

#[test]
fn effect_refusals_are_typed_and_nothing_is_dropped_or_approximated_silently() {
    bounded_case("effect_refusals_are_typed_and_nothing_is_dropped_or_approximated_silently", || {
        // Every case puts the effects on layer 3 of tiled() unless stated and must be refused with
        // its code by evaluation (the step) and by lowering; the control cases must lower.
        let on_layer_3 = |effect: Value| {
            fx_records(None, json!([{"layer_id":identity("SLYR",3),"effects":fx_stack(vec![effect])}]))
        };
        let refused = |v: &Value, recs: &StudioRasterRecords, space: BlendSpace, code: &str| {
            let (_, low) = flow_space(v, recs, space);
            let reason = unavailable_reason(low);
            assert!(reason.contains(code), "{code} not in {reason}");
        };
        let ok = |v: &Value, recs: &StudioRasterRecords, space: BlendSpace| {
            flow_space(v, recs, space).1.unwrap_or_else(|e| panic!("expected a lowering, got {e}"))
        };
        let black = || fx_colour(0.0, 0.0, 0.0);
        let overlay = || json!({"effect_kind":"colour_overlay"});
        let enc = BlendSpace::Encoded;
        let doc = tiled();

        // Contour: linear (two points, (0,0) and (255,255), or absent) lowers; anything else is refused.
        let with_contour = |points: Value| {
            let mut e = fx_effect("e1", 100.0, black(), false, overlay());
            e["contour"] = json!({"name":"C","points":points});
            on_layer_3(e)
        };
        let linear = json!([{"input":0.0,"output":0.0,"corner":false},{"input":255.0,"output":255.0,"corner":false}]);
        assert_eq!(ok(&doc, &with_contour(linear), enc).extensions.layers.len(), 1);
        let bent = json!([{"input":0.0,"output":0.0,"corner":false},{"input":128.0,"output":255.0,"corner":false},
            {"input":255.0,"output":255.0,"corner":false}]);
        refused(&doc, &with_contour(bent), enc, "effect_contour");
        // Noise above zero.
        let mut noisy = fx_effect("e1", 100.0, black(), false, overlay());
        noisy["noise"] = json!(10.0);
        refused(&doc, &on_layer_3(noisy), enc, "effect_noise");
        // Gradient / pattern stroke fill, and overprint.
        let mut gradient = fx_stroke(2.0, "outside");
        gradient["fill_type"] = json!("gradient");
        gradient["fill"] = json!({"schema_id":"hsk.studio.gradient@1","object_id":"GRAD-1"});
        refused(&doc, &on_layer_3(fx_effect("e1", 100.0, black(), false, gradient)), enc, "effect_stroke_fill");
        let mut overprint = fx_stroke(2.0, "inside");
        overprint["overprint"] = json!(true);
        refused(&doc, &on_layer_3(fx_effect("e1", 100.0, black(), false, overprint)), enc, "effect_stroke_overprint");
        // Global Light: a drop shadow that follows it needs the record; the others do not use the angle.
        let follows = on_layer_3(fx_effect("e1", 100.0, black(), true, fx_shadow(2.0, 0.0, 0.0)));
        refused(&doc, &follows, enc, "effect_global_light_unresolved");
        let resolved = fx_records(
            Some(120.0),
            json!([{"layer_id":identity("SLYR",3),"effects":fx_stack(vec![
                fx_effect("e1", 100.0, black(), true, fx_shadow(2.0, 0.0, 0.0))])}]),
        );
        let low = ok(&doc, &resolved, enc);
        let EffectOp::DropShadow(s) = low.extensions.layers[0].effects[0] else { panic!("drop shadow") };
        assert_eq!(s.angle_deg, 120.0);
        assert!(ok(&doc, &on_layer_3(fx_effect("e1", 100.0, black(), true, overlay())), enc).extensions.layers.len() == 1);
        // A kind pigment does not render, and stack scale other than 100 percent.
        let glow = json!({"effect_kind":"pattern_overlay","pattern":{"schema_id":"hsk.studio.pattern@1","object_id":"p1"},
            "scale":100.0,"phase_x":0.0,"phase_y":0.0,"link_with_layer":true}); // satin renders since SMB-PS-ADJUST-STYLES
        refused(&doc, &on_layer_3(fx_effect("e1", 100.0, black(), false, glow.clone())), enc, "layer_effects_not_rendered");
        let mut scaled = on_layer_3(fx_effect("e1", 100.0, black(), false, overlay()));
        scaled.layers[0].effects.as_mut().unwrap().scale = 150.0;
        refused(&doc, &scaled, enc, "effect_stack_scale");
        // A disabled instance (even of an unsupported kind) and a disabled stack render nothing.
        let mut off = fx_effect("e1", 100.0, black(), false, glow);
        off["enabled"] = json!(false);
        assert!(ok(&doc, &on_layer_3(off), enc).extensions.is_empty());
        let mut disabled = on_layer_3(fx_effect("e1", 100.0, black(), false, overlay()));
        disabled.layers[0].effects.as_mut().unwrap().enabled = false;
        assert!(ok(&doc, &disabled, enc).extensions.is_empty());
        // Colour: another profile is not converted; mid-tones in a linear-light plan are not
        // decoded (black is the same in every encoding); a missing colour is not invented.
        let other_profile = json!({"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":identity("SCPF",2)});
        refused(&doc, &on_layer_3(fx_effect("e1", 100.0, other_profile, false, overlay())), enc, "effect_colour_profile");
        let same_profile = json!({"r":0.5,"g":0.5,"b":0.5,"colour_profile_id":identity("SCPF",1)});
        assert_eq!(ok(&doc, &on_layer_3(fx_effect("e1", 100.0, same_profile.clone(), false, overlay())), enc).extensions.layers.len(), 1);
        refused(&doc, &on_layer_3(fx_effect("e1", 100.0, same_profile, false, overlay())), BlendSpace::LinearLight, "effect_colour_linear_light");
        assert_eq!(ok(&doc, &on_layer_3(fx_effect("e1", 100.0, black(), false, overlay())), BlendSpace::LinearLight).extensions.layers.len(), 1);
        let mut no_colour = fx_effect("e1", 100.0, black(), false, overlay());
        no_colour["colour"] = json!(null);
        refused(&doc, &on_layer_3(no_colour), enc, "effect_colour_missing");
        // A spread above 100 percent is outside pigment's range: refused, not clamped.
        refused(&doc, &on_layer_3(fx_effect("e1", 100.0, black(), false, fx_shadow(1.0, 150.0, 4.0))), enc, "effect_parameter");

        // Layer-level: pass-through (the root group's default mode), clipped, clip base, adjustment.
        let root_fx = fx_records(None, json!([{"layer_id":identity("SLYR",1),"effects":fx_stack(vec![
            fx_effect("e1", 100.0, black(), false, overlay())])}]));
        refused(&doc, &root_fx, enc, "effect_on_pass_through");
        let mut clipped = tiled();
        clipped["layers"][2]["appearance"] = appearance(100.0, 100.0, 2, false);
        clipped["layers"][2]["appearance"]["clipped"] = json!(true);
        refused(&clipped, &on_layer_3(fx_effect("e1", 100.0, black(), false, overlay())), enc, "effect_on_clipped_layer");
        // Layer 2 with effects is the base of clipped layer 3: refused by evaluation (parent step,
        // pigment's code in the ledger) and by lowering. Without effects the same run lowers.
        let base_fx = fx_records(None, json!([{"layer_id":identity("SLYR",2),"effects":fx_stack(vec![
            fx_effect("e1", 100.0, black(), false, overlay())])}]));
        let (report, low) = flow_space(&clipped, &base_fx, enc);
        assert!(unavailable_reason(low).contains("effect_on_clip_base"));
        assert!(report.receipt.exactness.operators.iter().any(|o| o.key
            == PlanError::EffectClipRun { node: 0 }.code()
            && o.exactness == Exactness::Unsupported));
        assert!(matches!(
            &report.step(&addr(1, "mix", "out")).unwrap().status,
            StepStatus::Unavailable(r) if r.starts_with("effect_on_clip_base")
        ));
        assert!(ok(&clipped, &fx_records(None, json!([])), enc).extensions.is_empty());
        // An adjustment layer takes no effects.
        let adj = fx_records(None, json!([{"layer_id":identity("SLYR",3),"adjustment":{"kind":"invert"},
            "effects":fx_stack(vec![fx_effect("e1", 100.0, black(), false, overlay())])}]));
        let run = run_ext(&adjusted(), &adj, true);
        assert_eq!(
            run.2.step(&addr(3, "source", "out")).unwrap().status,
            StepStatus::Unavailable("effect_on_adjustment_layer".into())
        );
        assert!(unavailable_reason(lower_ext(&run, &adj)).contains("effect_on_adjustment_layer"));
    });
}

// ---------------------------------------------------------------------------------------------
// Effects wave 2 (inner shadow, outer glow, inner glow; colour source only).
// ---------------------------------------------------------------------------------------------
use hsk_studio_composite::lower_extensions::{NOTE_GLOW_JITTER_IGNORED, NOTE_SPREAD_CHOKE_PERCENT};
use hsk_studio_pigment::effect_math::{GlowSource, InnerGlowOp, InnerShadowOp, OuterGlowOp};

/// A Softer glow kind block (size 2, range 100, jitter 0) with `fields` overriding.
fn fx_glow(kind: &str, fields: Value) -> Value {
    let mut v = json!({"effect_kind":kind,"technique":"softer","size":2.0,"range":100.0,"jitter":0.0});
    // Kind-specific required fields: an outer glow has a spread, an inner glow a source and a choke.
    if kind == "inner_glow" {
        v["source"] = json!("edge");
        v["choke"] = json!(0.0);
    } else {
        v["spread"] = json!(0.0);
    }
    for (k, x) in fields.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

#[test]
fn inner_shadow_and_glows_lower_to_pigment_ops_that_render_as_derived() {
    bounded_case("inner_shadow_and_glows_lower_to_pigment_ops_that_render_as_derived", || {
        let normal = StudioBlendMode::Normal;
        let black = || fx_colour(0.0, 0.0, 0.0);
        // Layer 2: inner shadow following Global Light (180), distance 1, black. Layer 3: a red
        // outer glow (spread 100 percent, size 2, 50 percent opacity) and a white inner glow
        // from the edge (choke 100 percent, size 1).
        let recs = fx_records(
            Some(180.0),
            json!([
                {"layer_id":identity("SLYR",2),"effects":fx_stack(vec![fx_effect("is_1", 100.0, black(), true,
                    json!({"effect_kind":"inner_shadow","distance":1.0,"choke":0.0,"size":0.0}))])},
                {"layer_id":identity("SLYR",3),"effects":fx_stack(vec![
                    fx_effect("og_1", 50.0, fx_colour(1.0,0.0,0.0), false, fx_glow("outer_glow", json!({"spread":100.0}))),
                    fx_effect("ig_1", 100.0, fx_colour(1.0,1.0,1.0), false,
                        fx_glow("inner_glow", json!({"source":"edge","choke":100.0,"size":1.0})))])}]),
        );
        let (report, low) = flow_space(&tiled(), &recs, BlendSpace::Encoded);
        let low = low.unwrap();
        let inner_shadow = EffectOp::InnerShadow(
            InnerShadowOp::new([0.0; 3], normal, 1.0).with_offset(180.0, 1.0).with_choke(0.0).with_size(0.0),
        );
        let outer_glow = EffectOp::OuterGlow(
            OuterGlowOp::new([1.0, 0.0, 0.0], normal, 0.5).with_spread(1.0).with_size(2.0),
        );
        let inner_glow = EffectOp::InnerGlow(
            InnerGlowOp::new([1.0; 3], normal, 1.0).with_choke(1.0).with_size(1.0).with_source(GlowSource::Edge),
        );
        assert_eq!(
            low.extensions.layers,
            vec![
                LayerExtension::new(PlanNodeId(4), 0).with_effects(vec![inner_shadow]),
                LayerExtension::new(PlanNodeId(4), 1).with_effects(vec![outer_glow, inner_glow]),
            ]
        );
        // Percent become fractions and the reading is reported (outer glow spread, inner glow choke).
        let percent_notes = low.notes.iter().filter(|n| n.key == NOTE_SPREAD_CHOKE_PERCENT).count();
        assert_eq!(percent_notes, 2, "{:?}", low.notes);
        use hsk_studio_pigment::effect_math::EffectKind;
        assert_eq!(
            low.extensions.validate(&low.plan),
            Ok(vec![
                ExtOperator::Effect(EffectKind::InnerShadow),
                ExtOperator::Effect(EffectKind::OuterGlow),
                ExtOperator::Effect(EffectKind::InnerGlow),
            ])
        );
        let token = CancellationToken::default();
        assert!(admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token).is_ok());
        for ledger in [&low.exactness, &report.receipt.exactness] {
            for kind in [EffectKind::InnerShadow, EffectKind::OuterGlow, EffectKind::InnerGlow] {
                let op = ExtOperator::Effect(kind);
                assert!(
                    ledger.operators.iter().any(|o| o.key == op.key() && o.exactness == op.intrinsic()),
                    "{} missing from {:?}",
                    op.key(),
                    ledger.operators
                );
            }
        }

        // Inner shadow, 5 x 1 plane, blue bar at x = 1..2: angle 180 and distance 1 move the
        // inverted matte [1,0,0,1,1] by (+1, 0) to [1,1,0,0,1] (beyond the plane reads 1), clipped
        // to the alpha: only x = 1 is shadowed (black, opacity 1), x = 2 stays blue.
        let blue = [0.0, 0.0, 1.0];
        let bar = plane_with(5, 1, &[(1, 0), (2, 0)], blue);
        let none = EffectPlane::transparent(5, 1).unwrap();
        let out = render_layer_reference(&none, &bar, LayerParams::plain(), &low.extensions.layers[0].effects).unwrap();
        assert_pixel(&out, 1, 0, Some([0.0; 3]), 1.0);
        assert_pixel(&out, 2, 0, Some(blue), 1.0);
        for x in [0, 3, 4] {
            assert_pixel(&out, x, 0, None, 0.0);
        }

        // Outer glow, 5 x 1 plane, one blue pixel at x = 2: spread 100 percent of size 2 grows the
        // matte as a hard disc of radius 2 (coverage 1 at offsets 1 and 2), so every pixel gets red
        // at 50 percent behind the content; the content pixel stays blue.
        let dot = plane_with(5, 1, &[(2, 0)], blue);
        let out = render_layer_reference(&none, &dot, LayerParams::plain(), &low.extensions.layers[1].effects[..1]).unwrap();
        assert_pixel(&out, 2, 0, Some(blue), 1.0);
        for x in [0, 1, 3, 4] {
            assert_pixel(&out, x, 0, Some([1.0, 0.0, 0.0]), 0.5);
        }

        // Inner glow from the edge, 5 x 5 plane, blue 3 x 3 square at 1..3: choke 100 percent of
        // size 1 grows the inverted matte by one pixel, so the 8 square pixels next to the outline
        // are white (opacity 1), the centre stays blue, and nothing is drawn outside the square.
        let covered: Vec<(u32, u32)> = (1..4).flat_map(|y| (1..4).map(move |x| (x, y))).collect();
        let square = plane_with(5, 5, &covered, blue);
        let none5 = EffectPlane::transparent(5, 5).unwrap();
        let out = render_layer_reference(&none5, &square, LayerParams::plain(), &low.extensions.layers[1].effects[1..]).unwrap();
        for (x, y) in covered.iter().filter(|p| **p != (2, 2)) {
            assert_pixel(&out, *x, *y, Some([1.0; 3]), 1.0);
        }
        assert_pixel(&out, 2, 2, Some(blue), 1.0);
        assert_pixel(&out, 0, 0, None, 0.0);
        assert_pixel(&out, 4, 2, None, 0.0);

        // Source centre inverts it: the centre is white, the ring stays blue.
        let centre = fx_records(None, json!([{"layer_id":identity("SLYR",3),"effects":fx_stack(vec![
            fx_effect("ig_2", 100.0, fx_colour(1.0,1.0,1.0), false,
                fx_glow("inner_glow", json!({"source":"centre","choke":100.0,"size":1.0})))])}]));
        let low_c = flow_space(&tiled(), &centre, BlendSpace::Encoded).1.unwrap();
        let out = render_layer_reference(&none5, &square, LayerParams::plain(), &low_c.extensions.layers[0].effects).unwrap();
        assert_pixel(&out, 2, 2, Some([1.0; 3]), 1.0);
        assert_pixel(&out, 1, 1, Some(blue), 1.0);
        assert_pixel(&out, 3, 2, Some(blue), 1.0);

        // Jitter on a solid-colour glow is passed as 0 and reported; the glow still lowers.
        let jittery = fx_records(None, json!([{"layer_id":identity("SLYR",3),"effects":fx_stack(vec![
            fx_effect("og_2", 100.0, black(), false, fx_glow("outer_glow", json!({"spread":0.0,"jitter":20.0})))])}]));
        let low_j = flow_space(&tiled(), &jittery, BlendSpace::Encoded).1.unwrap();
        let EffectOp::OuterGlow(g) = low_j.extensions.layers[0].effects[0] else { panic!("outer glow") };
        assert_eq!(g.jitter, 0.0);
        assert!(low_j.notes.iter().any(|n| n.key == NOTE_GLOW_JITTER_IGNORED && n.layer_id == identity("SLYR", 3)));

        // Pigment's refusals surface under its feature keys; a domain error wins over them.
        let refused = |fields: Value, kind: &str, code: &str| {
            let r = fx_records(None, json!([{"layer_id":identity("SLYR",3),"effects":fx_stack(vec![
                fx_effect("g", 100.0, black(), false, fx_glow(kind, fields))])}]));
            let reason = unavailable_reason(flow_space(&tiled(), &r, BlendSpace::Encoded).1);
            assert!(reason.contains(code), "{code} not in {reason}");
        };
        let gradient = json!({"schema_id":"hsk.studio.gradient@1","object_id":"GRAD-1"});
        refused(json!({"gradient":gradient,"jitter":30.0}), "outer_glow", "glow_gradient");
        refused(json!({"gradient":gradient}), "inner_glow", "glow_gradient");
        refused(json!({"range":50.0}), "outer_glow", "glow_range");
        refused(json!({"technique":"precise"}), "inner_glow", "glow_technique_precise");
        refused(json!({"choke":150.0}), "inner_glow", "effect_parameter");
    });
}

// ---------------------------------------------------------------------------------------------
// Folio ENG-6 (0241877): an adjustment layer is a composition-graph source, so a REAL snapshot
// carries it into the plan at its input_ordinal (no hand-built plan step).
// ---------------------------------------------------------------------------------------------
#[test]
fn adjustment_layer_in_a_real_folio_graph_lowers_at_its_input_ordinal() {
    bounded_case("adjustment_layer_in_a_real_folio_graph_lowers_at_its_input_ordinal", || {
        let graph_with = |ordinals: [u64; 3]| {
            // Group 1 composites layers 2, 3 (the adjustment layer) and 4 at the given ordinals.
            let mut v = adjusted();
            v["graph"]["operations"] =
                json!([source(2, "image"), source(3, "image"), source(4, "image"), composite(1)]);
            v["graph"]["edges"] = json!([
                edge(address(2, "source", "out"), address(1, "mix", "inputs"), ordinals[0]),
                edge(address(3, "source", "out"), address(1, "mix", "inputs"), ordinals[1]),
                edge(address(4, "source", "out"), address(1, "mix", "inputs"), ordinals[2]),
            ]);
            v
        };
        let recs = records(json!([{"layer_id":identity("SLYR",3),"adjustment":{"kind":"invert"}}]));
        let area = Rect { x: 0, y: 0, width: 64, height: 64 };

        // Stack order 2, 3, 4: same plan as the hand-built case, produced by lower() itself.
        let (report, low) = flow_space(&graph_with([0, 1, 2]), &recs, BlendSpace::Encoded);
        let adjustment = report.step(&addr(3, "source", "out")).unwrap();
        assert_eq!((adjustment.status.clone(), adjustment.extent), (StepStatus::Ready, None));
        let low = low.unwrap();
        assert_eq!(low.plan.nodes.len(), 4);
        assert_eq!(low.plan.nodes[2], PlanNode::Solid { rect: area, rgba: [0.0; 4] });
        let PlanNode::Group(top) = &low.plan.nodes[3] else { panic!("node 3 must be the root") };
        assert_eq!(
            top.layers.iter().map(|l| l.input).collect::<Vec<_>>(),
            [PlanNodeId(0), PlanNodeId(2), PlanNodeId(1)]
        );
        assert_eq!(
            low.extensions.layers,
            vec![LayerExtension::new(PlanNodeId(3), 1).with_adjustment(AdjustmentOp::Invert)]
        );

        // The authored input_ordinal is the stack position: ordinal 0 makes it the bottom layer.
        let (_, low) = flow_space(&graph_with([1, 0, 2]), &recs, BlendSpace::Encoded);
        let low = low.unwrap();
        let PlanNode::Group(top) = &low.plan.nodes[3] else { panic!("node 3 must be the root") };
        assert_eq!(
            top.layers.iter().map(|l| l.input).collect::<Vec<_>>(),
            [PlanNodeId(2), PlanNodeId(0), PlanNodeId(1)]
        );
        assert_eq!(low.extensions.layers[0].layer, 0);

        // Evaluated against pigment's single-pixel stack oracle: the lowered stack of a pixel
        // (layer 2 = (0.2, 0.4, 0.6), layer 4 = (0.5, 0.25, 0.0), both opaque Normal) with the
        // adjustment layer (invert) between them is covered by layer 4 -> (0.5, 0.25, 0.0); with
        // the adjustment layer on top (ordinals 2, 4, 3) it inverts the result -> (0.5, 0.75, 1.0).
        let pixel = |ordinals: [u64; 3]| {
            let (_, low) = flow_space(&graph_with(ordinals), &recs, BlendSpace::Encoded);
            let low = low.unwrap();
            let PlanNode::Group(top) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
            let colour_of = |key: SourceKey| {
                let record = &low.sources.iter().find(|(k, _)| *k == key).expect("source key").1;
                match record.object_key.as_str() {
                    "t20" => [0.2, 0.4, 0.6],
                    "t40" => [0.5, 0.25, 0.0],
                    other => panic!("unexpected tile {other}"),
                }
            };
            let stack: Vec<StackNode> = top
                .layers
                .iter()
                .enumerate()
                .map(|(i, l)| match &low.plan.nodes[l.input.0 as usize] {
                    PlanNode::Tile(t) => StackNode::Leaf {
                        source: Rgba::new(colour_of(t.key), 1.0),
                        mode: l.mode,
                        opacity: l.opacity,
                    },
                    PlanNode::Solid { .. } => StackNode::Adjustment {
                        op: low
                            .extensions
                            .extension(low.plan.root, u32::try_from(i).unwrap())
                            .and_then(|e| e.adjustment.clone())
                            .expect("adjustment entry"),
                        mode: l.mode,
                        opacity: l.opacity,
                    },
                    other => panic!("unexpected node {other:?}"),
                })
                .collect();
            let ctx = BlendContext { space: BlendSpace::Encoded, site: PixelSite::default() };
            evaluate_stack(&stack, Rgba::TRANSPARENT, &ctx).unwrap().0
        };
        let between = pixel([0, 1, 2]);
        let on_top = pixel([0, 2, 1]);
        assert_close(between.a, 1.0);
        assert_close(on_top.a, 1.0);
        for (got, want) in between.rgb.iter().zip([0.5, 0.25, 0.0]) {
            assert_close(*got, want);
        }
        for (got, want) in on_top.rgb.iter().zip([0.5, 0.75, 1.0]) {
            assert_close(*got, want);
        }

        // Containment plus a graph position is fine (the refusal is only for containment alone).
        let mut contained = graph_with([0, 1, 2]);
        contained["layers"][2]["parent"] = in_group_1();
        assert!(flow_space(&contained, &recs, BlendSpace::Encoded).1.is_ok());

        // Without records the layer is still refused, typed (the v1 evaluation does not know it).
        let snap = open_bytes(&serde_json::to_vec(&graph_with([0, 1, 2])).unwrap(), &WithAdjustment);
        let token = CancellationToken::default();
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let v1 = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(
            v1.step(&addr(3, "source", "out")).unwrap().status,
            StepStatus::Unavailable("unsupported_payload".into())
        );
        assert!(unavailable_reason(
            lower_to_render_plan_extended(
                &plan, &v1, &snap, &recs, &addr(1, "mix", "out"), target(BlendSpace::Encoded), &Info
            )
        )
        .contains("unsupported_payload"));
    });
}

// ---------------------------------------------------------------------------------------------
// Blend-If on a clip role and Blend Clipped Layers as Group switched off (lane
// SMB-BLENDIF-CLIP-ADOPT). Pigment 0d28c03 refuses a non-identity Blend-If on the base of a clipping
// run and on a clipped layer for every backend (no public definition; keys and class are pigment's),
// and its clip rule is the Blend Clipped Layers as Group ON reading. Expected values below are
// derived from the fixtures and from pigment's own `ExtOperator` constants.
// ---------------------------------------------------------------------------------------------
use hsk_studio_composite::lower_extensions::REFUSE_CLIP_GROUP_BLEND_OFF;
use hsk_studio_pigment::blend_math::ExactnessLedger;

/// Group 1 = raster 2 (the clip base) then raster 3 clipped to it (Normal).
fn clip_doc() -> Value {
    let mut v = tiled();
    v["layers"][2]["appearance"] = appearance(100.0, 100.0, 2, false);
    v["layers"][2]["appearance"]["clipped"] = json!(true);
    v
}
/// Source `[black_start, black_end, white_start, white_end]` in levels of the one gray entry.
const SPLIT: [f64; 4] = [50.0, 100.0, 200.0, 240.0];
const IDENTITY_LEVELS: [f64; 4] = [0.0, 0.0, 255.0, 255.0];
/// Advanced blending of one layer: at most a gray range, Blend Clipped Layers as Group on or off.
fn blending(range: Option<[f64; 4]>, clipped_as_group: bool) -> Value {
    let entries = range.map_or(json!([]), |r| {
        json!([{"channel":"gray","source_black":[r[0], r[1]],"source_white":[r[2], r[3]],
            "destination_black":[0.0, 0.0],"destination_white":[255.0, 255.0]}])
    });
    json!({"blend_ranges":entries,"knockout":"disabled",
        "channel_blend":{"red":true,"green":true,"blue":true},
        "blend_interior_effects":true,"blend_clipped_effects":clipped_as_group,
        "transparency_shapes_layer":true})
}
fn records_of(entries: &[(u8, Value)]) -> StudioRasterRecords {
    records(Value::Array(
        entries
            .iter()
            .map(|(n, b)| json!({"layer_id":identity("SLYR", *n),"advanced_blending":b}))
            .collect(),
    ))
}
fn has_operator(ledger: &ExactnessLedger, op: ExtOperator) -> bool {
    ledger
        .operators
        .contains(&OperatorExactness { key: op.key(), exactness: op.intrinsic() })
}
fn has_key(ledger: &ExactnessLedger, key: &str) -> bool {
    ledger.operators.iter().any(|o| o.key == key)
}
fn any_blend_if(ledger: &ExactnessLedger) -> bool {
    ledger.operators.iter().any(|o| o.key.starts_with("blend_if"))
}

#[test]
fn blend_if_on_clip_roles_is_reported_and_refused_like_pigment() {
    bounded_case("blend_if_on_clip_roles_is_reported_and_refused_like_pigment", || {
        let token = CancellationToken::default();
        let all = [ExtOperator::BlendIf, ExtOperator::BlendIfClipBase, ExtOperator::BlendIfClippedLayer];

        // Layer 2 is the base of the run in `clip_doc` and layer 3 the clipped layer; in `tiled`
        // (no clipping) both are plain. The operator is pigment's choice by role; evaluation and
        // lowering both report it, instead of `blend_if`, with pigment's key and class.
        for (doc, layer, want) in [
            (clip_doc(), 2_u8, ExtOperator::BlendIfClipBase),
            (clip_doc(), 3, ExtOperator::BlendIfClippedLayer),
            (tiled(), 2, ExtOperator::BlendIf),
            (tiled(), 3, ExtOperator::BlendIf),
        ] {
            let recs = records_of(&[(layer, blending(Some(SPLIT), true))]);
            let run = run_ext(&doc, &recs, false);
            let ledger = &run.2.receipt.exactness;
            for op in all {
                assert_eq!(has_operator(ledger, op), op == want, "layer {layer} {op:?} in the evaluation ledger");
            }
            let low = lower_ext(&run, &recs).expect("the ranges are carried to pigment, not dropped");
            assert_eq!(low.extensions.validate(&low.plan), Ok(vec![want]), "layer {layer}");
            for op in all {
                assert_eq!(has_operator(&low.exactness, op), op == want, "layer {layer} {op:?} in the lowering ledger");
            }
            // Shared admission, even for a backend that claims every operator Exact.
            let admitted = admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token);
            if want.intrinsic() == Exactness::Unsupported {
                assert_eq!(ledger.lowest, Exactness::Unsupported, "layer {layer}");
                assert_eq!(
                    admitted,
                    Err(BackendError::Unsupported { operator: want.key(), exactness: Exactness::Unsupported }),
                    "layer {layer}"
                );
            } else {
                assert!(admitted.is_ok(), "layer {layer}: Blend-If on an unclipped layer stays admitted");
            }
        }
        assert_eq!(ExtOperator::BlendIfClipBase.key(), "blend_if_clip_base");
        assert_eq!(ExtOperator::BlendIfClippedLayer.key(), "blend_if_clipped_layer");

        // Identity ranges on both roles are not Blend-If: no operator, no entry, lowering and
        // admission as for the plain run.
        let recs = records_of(&[
            (2, blending(Some(IDENTITY_LEVELS), true)),
            (3, blending(Some(IDENTITY_LEVELS), true)),
        ]);
        let run = run_ext(&clip_doc(), &recs, false);
        assert!(!any_blend_if(&run.2.receipt.exactness));
        let low = lower_ext(&run, &recs).unwrap();
        assert!(low.extensions.is_empty());
        assert!(!any_blend_if(&low.exactness));

        // A hidden clipped layer is not emitted, so the base is a plain layer: BlendIf, not the
        // clip-base operator; the hidden layer's own ranges are no operator at all.
        let mut hidden_clipped = clip_doc();
        hidden_clipped["layers"][2]["visible"] = json!(false);
        let recs = records_of(&[(2, blending(Some(SPLIT), true)), (3, blending(Some(SPLIT), true))]);
        let run = run_ext(&hidden_clipped, &recs, false);
        let ledger = &run.2.receipt.exactness;
        assert!(has_operator(ledger, ExtOperator::BlendIf));
        assert!(!has_operator(ledger, ExtOperator::BlendIfClipBase));
        assert!(!has_operator(ledger, ExtOperator::BlendIfClippedLayer));
        let low = lower_ext(&run, &recs).unwrap();
        assert_eq!(low.extensions.validate(&low.plan), Ok(vec![ExtOperator::BlendIf]));

        // A hidden base drops its clipped run: the clipped layer's ranges never reach the plan.
        let mut hidden_base = clip_doc();
        hidden_base["layers"][1]["visible"] = json!(false);
        let recs = records_of(&[(3, blending(Some(SPLIT), true))]);
        let run = run_ext(&hidden_base, &recs, false);
        assert!(!any_blend_if(&run.2.receipt.exactness));
        let low = lower_ext(&run, &recs).unwrap();
        assert!(low.extensions.is_empty());
    });
}

#[test]
fn blend_clipped_layers_as_group_off_is_refused_on_a_clip_base() {
    bounded_case("blend_clipped_layers_as_group_off_is_refused_on_a_clip_base", || {
        let root = addr(1, "mix", "out");
        let token = CancellationToken::default();
        let lowers_and_is_ready = |doc: &Value, recs: &StudioRasterRecords, what: &str| {
            let run = run_ext(doc, recs, false);
            assert_eq!(run.2.step(&root).unwrap().status, StepStatus::Ready, "{what}");
            assert!(!has_key(&run.2.receipt.exactness, REFUSE_CLIP_GROUP_BLEND_OFF), "{what}");
            lower_ext(&run, recs).unwrap_or_else(|e| panic!("{what}: {e:?}"));
        };

        // Base 2 has the toggle off and a rendered clipped layer 3 sits on it: evaluation says
        // Unsupported and the parent is Unavailable, naming the code and the clipped layer.
        let recs = records_of(&[(2, blending(None, false))]);
        let run = run_ext(&clip_doc(), &recs, false);
        assert!(run.2.receipt.exactness.operators.contains(&OperatorExactness {
            key: REFUSE_CLIP_GROUP_BLEND_OFF,
            exactness: Exactness::Unsupported
        }));
        assert_eq!(run.2.receipt.exactness.lowest, Exactness::Unsupported);
        match &run.2.step(&root).unwrap().status {
            StepStatus::Unavailable(reason) => {
                assert!(reason.starts_with(REFUSE_CLIP_GROUP_BLEND_OFF), "{reason}");
                assert!(reason.contains(&identity("SLYR", 3)), "{reason}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        // Lowering refuses with the same code, from the evaluation status ...
        let reason = unavailable_reason(lower_ext(&run, &recs));
        assert!(reason.contains(REFUSE_CLIP_GROUP_BLEND_OFF), "{reason}");
        // ... and by itself when the report comes from the v1 evaluation, which does not read the
        // raster records and so calls the step Ready (the lowering never relies on that).
        let snap = open(&clip_doc());
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let v1 = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(v1.step(&root).unwrap().status, StepStatus::Ready);
        let reason = unavailable_reason(lower_to_render_plan_extended(
            &plan, &v1, &snap, &recs, &root, target(BlendSpace::Encoded), &Info,
        ));
        assert!(reason.contains(REFUSE_CLIP_GROUP_BLEND_OFF), "{reason}");
        assert!(reason.contains(&identity("SLYR", 3)), "{reason}");

        // The setting changes nothing, and so is not refused, when: it is on (the default the PSD
        // importer writes for an absent clbl); there is no clipping; only the clipped layer has it
        // off (the toggle belongs to the base); the clipped layer is hidden; the base is hidden.
        lowers_and_is_ready(&clip_doc(), &records_of(&[(2, blending(None, true))]), "toggle on");
        lowers_and_is_ready(&tiled(), &recs, "no clipping");
        lowers_and_is_ready(&clip_doc(), &records_of(&[(3, blending(None, false))]), "off on the clipped layer");
        let mut hidden_clipped = clip_doc();
        hidden_clipped["layers"][2]["visible"] = json!(false);
        lowers_and_is_ready(&hidden_clipped, &recs, "clipped layer hidden");
        let mut hidden_base = clip_doc();
        hidden_base["layers"][1]["visible"] = json!(false);
        lowers_and_is_ready(&hidden_base, &recs, "base hidden");
    });
}

use hsk_studio_pigment::render_plan::{Group, Layer, PlanLimits};

/// PLR-006 / lane SMB-REFUSAL-KEYS: the refusal keys composite re-decides (`record_appearance`, the
/// clip-base refusal) are the ones pigment's own validation and admission produce for the same
/// rule, so they cannot drift. Each rule is shown on a hand-built pigment plan first (pigment's
/// answer), then on the composite document (composite's evaluation-ledger key must contain it).
#[test]
fn refusal_keys_are_pigments() {
    bounded_case("refusal_keys_are_pigments", || {
        let token = CancellationToken::default();
        let unsupported_keys = |v: &Value| -> Vec<&'static str> {
            let snap = open(v);
            let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
            let report = evaluate(&plan, &snap, &token).unwrap();
            let operators = report.receipt.exactness.operators;
            operators.iter().filter(|o| o.exactness == Exactness::Unsupported).map(|o| o.key).collect()
        };
        // [solid, non-isolated group over it, isolated root holding `top` over the group].
        let pigment_plan = |top: Vec<Layer>| {
            let rect = Rect { x: 0, y: 0, width: 8, height: 8 };
            let inner = Layer {
                input: PlanNodeId(0),
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                mask: None,
            };
            RenderPlan {
                revision: 7,
                target: target(BlendSpace::Encoded),
                nodes: vec![
                    PlanNode::Solid { rect, rgba: [1.0; 4] },
                    PlanNode::Group(Group { isolated: false, layers: vec![inner] }),
                    PlanNode::Group(Group { isolated: true, layers: top }),
                ],
                root: PlanNodeId(2),
            }
        };
        let over_group = |mode, fill_opacity, mask| Layer {
            input: PlanNodeId(1),
            mode,
            opacity: 1.0,
            fill_opacity,
            mask,
        };

        // A clipping run whose base is a pass-through group: pigment's validation refuses it as
        // PlanError::ClipBasePassThrough (it has no operator key for it).
        let clipped = Layer {
            input: PlanNodeId(0),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            mask: Some(MaskInput::clip_to_base()),
        };
        let refused = pigment_plan(vec![over_group(StudioBlendMode::PassThrough, 1.0, None), clipped])
            .validate(&PlanLimits::default())
            .expect_err("pigment refuses a pass-through clip base");
        assert_eq!(refused, PlanError::ClipBasePassThrough { node: 2 });
        let keys = unsupported_keys(&pass_through_base_fixture(1));
        assert!(keys.contains(&refused.code()), "composite ledger {keys:?} lacks pigment's {}", refused.code());

        // A pass-through layer with fill below 1: pigment's shared admission refuses it as
        // PASS_THROUGH_FILL_OPERATOR for every backend.
        let request = ExecuteRequest {
            expected_revision: 7,
            min_exactness: Exactness::Approximate,
            deadline: None,
            max_working_bytes: 1 << 20,
        };
        let plan = pigment_plan(vec![over_group(StudioBlendMode::PassThrough, 0.25, None)]);
        let Err(BackendError::Unsupported { operator, .. }) =
            admit(&plan, &Backend { refuses: None }, &request, &token)
        else {
            panic!("pigment admission refuses fill below 1 on a pass-through layer")
        };
        assert_eq!(operator, PASS_THROUGH_FILL_OPERATOR);
        let mut filled = tiled();
        filled["layers"][0]["appearance"] = appearance(100.0, 25.0, 1, true);
        let keys = unsupported_keys(&filled);
        assert!(keys.contains(&operator), "composite ledger {keys:?} lacks pigment's {operator}");

        // A coverage mask on a pass-through layer (lane SMB-FX-OPS-ADOPT, pigment b2f5f81): pigment's
        // shared admission refuses it as PASS_THROUGH_MASK_OPERATOR for every backend.
        let mask = MaskInput {
            key: SourceKey(1),
            placement: Rect { x: 0, y: 0, width: 8, height: 8 },
            format: MaskFormat::CoverageU8,
            sha256: [0; 32],
            inverted: false,
            density: 1.0,
        };
        let plan = pigment_plan(vec![over_group(StudioBlendMode::PassThrough, 1.0, Some(mask))]);
        let Err(BackendError::Unsupported { operator, .. }) =
            admit(&plan, &Backend { refuses: None }, &request, &token)
        else {
            panic!("pigment admission refuses a mask on a pass-through layer")
        };
        assert_eq!(operator, PASS_THROUGH_MASK_OPERATOR);
        let keys = unsupported_keys(&masked_group(1, true));
        assert!(keys.contains(&operator), "composite ledger {keys:?} lacks pigment's {operator}");
    });
}

/// `tiled()` with an enabled coverage mask on the root group, whose appearance has `mode` (1 =
/// pass_through, 2 = Normal) and `group` (pass-through isolation).
fn masked_group(mode: u16, group: bool) -> Value {
    let mut v = tiled();
    v["layers"][0]["appearance"] = appearance(100.0, 100.0, mode, group);
    v["layers"][0]["appearance"]["mask"] =
        json!({"tiles":[mask_tile(1, "mg")],"enabled":true,"linked":true});
    v
}

/// Lane SMB-FX-OPS-ADOPT: a mask on a pass-through group has no defined semantics (pigment
/// b2f5f81), so composite treats it like fill on a pass-through layer: the step is Unavailable
/// under pigment's key, the evaluation ledger and the plan's exactness say Unsupported, and
/// lowering refuses it by the same code. The same mask on an isolated group is unaffected.
#[test]
fn mask_on_a_pass_through_group_is_refused_like_the_fill_rule() {
    bounded_case("mask_on_a_pass_through_group_is_refused_like_the_fill_rule", || {
        let token = CancellationToken::default();
        let root = addr(1, "mix", "out");
        let key = PASS_THROUGH_MASK_OPERATOR;

        let doc = masked_group(1, true);
        let snap = open(&doc);
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        assert_eq!(plan.lowest_exactness(), Exactness::Unsupported);
        let report = evaluate(&plan, &snap, &token).unwrap();
        assert!(report.receipt.exactness.operators.contains(&OperatorExactness {
            key,
            exactness: Exactness::Unsupported
        }));
        assert_eq!(report.receipt.exactness.lowest, Exactness::Unsupported);
        assert_eq!(report.step(&root).unwrap().status, StepStatus::Unavailable(key.into()));
        match lowered(&doc, BlendSpace::Encoded, &root) {
            Err(CompositeError::Unavailable(reason)) => assert!(reason.contains(key), "{reason}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }

        // Counterfactuals: the same mask on an isolated Normal group lowers and carries no such
        // key; a pass-through group without a mask, or with a disabled mask, is unchanged.
        let isolated = masked_group(2, false);
        let low = lowered(&isolated, BlendSpace::Encoded, &root).unwrap();
        assert!(low.exactness.operators.iter().all(|o| o.key != key));
        assert_eq!(low.mask_sources.len(), 1);
        let low = lowered(&tiled(), BlendSpace::Encoded, &root).unwrap();
        assert!(low.exactness.operators.iter().all(|o| o.key != key));
        let mut disabled = masked_group(1, true);
        disabled["layers"][0]["appearance"]["mask"]["enabled"] = json!(false);
        let low = lowered(&disabled, BlendSpace::Encoded, &root).unwrap();
        assert!(low.exactness.operators.iter().all(|o| o.key != key));
    });
}

// ---------------------------------------------------------------------------------------------
// Lane SMB-FX-OPS-ADOPT: effects together with Blend-If, and nested effects (pigment d3ce607).
// ---------------------------------------------------------------------------------------------

/// A backend double that evaluates every envelope operator except the nesting of effects (the
/// render-cpu shape: effects supported, `EffectsNested` declared Unsupported).
struct NoNestingBackend;
impl RenderBackend for NoNestingBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "composite-test-double",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn supports_clip_to_base(&self) -> bool {
        true
    }
    fn extension_exactness(&self, op: ExtOperator) -> Exactness {
        match op {
            ExtOperator::EffectsNested => Exactness::Unsupported,
            _ => Exactness::Exact,
        }
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("test_double"))
    }
}

/// Raster records for `(layer, colour overlay effect, Blend-If gray range)` entries.
fn effects_and_ranges(entries: &[(u8, bool, Option<[f64; 4]>)]) -> StudioRasterRecords {
    let layers = entries
        .iter()
        .map(|(n, effects, range)| {
            let mut e = json!({"layer_id":identity("SLYR", *n)});
            if *effects {
                e["effects"] = fx_stack(vec![fx_effect(
                    "co_1",
                    50.0,
                    fx_colour(1.0, 0.0, 0.0),
                    false,
                    json!({"effect_kind":"colour_overlay"}),
                )]);
            }
            if let Some(r) = range {
                e["advanced_blending"] = blending(Some(*r), true);
            }
            e
        })
        .collect::<Vec<_>>();
    records(Value::Array(layers))
}

/// `tiled()` whose root group 1 is an isolated Normal group, so it may carry effects.
fn isolated_root() -> Value {
    let mut v = tiled();
    v["layers"][0]["appearance"] = appearance(100.0, 100.0, 2, false);
    v
}

#[test]
fn effects_with_blend_if_and_nested_effects_are_reported_like_pigment() {
    bounded_case("effects_with_blend_if_and_nested_effects_are_reported_like_pigment", || {
        use hsk_studio_pigment::effect_math::EffectKind;
        let token = CancellationToken::default();
        let overlay = ExtOperator::Effect(EffectKind::ColourOverlay);
        let refused = |key: &'static str| {
            Err(BackendError::Unsupported { operator: key, exactness: Exactness::Unsupported })
        };
        // The evaluation ledger and the lowering (ledger and envelope) of one document.
        let both = |doc: &Value, recs: &StudioRasterRecords| {
            let run = run_ext(doc, recs, false);
            let low = lower_ext(&run, recs).expect("carried to pigment, not dropped");
            let validated = low.extensions.validate(&low.plan).expect("pigment validates it");
            (run.2.receipt.exactness, low, validated)
        };

        // Effects and a non-identity Blend-If on one layer: pigment reports the combination
        // instead of blend_if, and admission refuses it for every backend, even one claiming
        // every operator Exact.
        let recs = effects_and_ranges(&[(3, true, Some(SPLIT))]);
        let (eval, low, validated) = both(&tiled(), &recs);
        assert_eq!(validated, vec![overlay, ExtOperator::EffectsWithBlendIf]);
        for (what, ledger) in [("evaluation", &eval), ("lowering", &low.exactness)] {
            assert!(has_operator(ledger, ExtOperator::EffectsWithBlendIf), "{what}");
            assert!(has_operator(ledger, overlay), "{what}");
            assert!(!has_operator(ledger, ExtOperator::BlendIf), "{what}: instead of blend_if");
        }
        assert_eq!(eval.lowest, Exactness::Unsupported);
        assert_eq!(
            admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token),
            refused("effects_with_blend_if")
        );

        // Counterfactuals: effects and ranges on different layers, and identity ranges.
        let recs = effects_and_ranges(&[(2, false, Some(SPLIT)), (3, true, None)]);
        let (eval, low, validated) = both(&tiled(), &recs);
        assert!(validated.contains(&ExtOperator::BlendIf) && validated.contains(&overlay));
        for ledger in [&eval, &low.exactness] {
            assert!(has_operator(ledger, ExtOperator::BlendIf));
            assert!(!has_operator(ledger, ExtOperator::EffectsWithBlendIf));
        }
        assert!(admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token).is_ok());
        let recs = effects_and_ranges(&[(3, true, Some(IDENTITY_LEVELS))]);
        let (eval, low, validated) = both(&tiled(), &recs);
        assert_eq!(validated, vec![overlay]);
        for ledger in [&eval, &low.exactness] {
            assert!(!has_operator(ledger, ExtOperator::BlendIf));
            assert!(!has_operator(ledger, ExtOperator::EffectsWithBlendIf));
        }

        // A layer with effects whose content holds a layer with effects: root 1 and layer 3.
        let nested_recs = effects_and_ranges(&[(1, true, None), (3, true, None)]);
        let (eval, low, validated) = both(&isolated_root(), &nested_recs);
        assert!(validated.contains(&ExtOperator::EffectsNested), "{validated:?}");
        for (what, ledger) in [("evaluation", &eval), ("lowering", &low.exactness)] {
            assert!(has_operator(ledger, ExtOperator::EffectsNested), "{what}");
        }
        // The class is pigment's (Approximate) and the backend declares it: one that evaluates
        // nesting admits the plan, one that declares it Unsupported is refused before any sink call.
        assert_eq!(ExtOperator::EffectsNested.intrinsic(), Exactness::Approximate);
        assert!(admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token).is_ok());
        assert_eq!(
            admit_extended(&low.plan, &low.extensions, &NoNestingBackend, &request(), &token),
            refused("effects_nested")
        );

        // Counterfactuals: effects on the inner layer only, on the outer group only, and a hidden
        // inner layer (not emitted, so it is not part of the content) are not nested.
        let mut hidden_inner = isolated_root();
        hidden_inner["layers"][2]["visible"] = json!(false);
        for (what, doc, entries) in [
            ("inner only", isolated_root(), vec![(3, true, None)]),
            ("outer only", isolated_root(), vec![(1, true, None)]),
            ("hidden inner", hidden_inner, vec![(1, true, None), (3, true, None)]),
        ] {
            let (eval, low, validated) = both(&doc, &effects_and_ranges(&entries));
            assert!(!validated.contains(&ExtOperator::EffectsNested), "{what}: {validated:?}");
            for ledger in [&eval, &low.exactness] {
                assert!(!has_operator(ledger, ExtOperator::EffectsNested), "{what}");
            }
            assert!(
                admit_extended(&low.plan, &low.extensions, &NoNestingBackend, &request(), &token)
                    .is_ok(),
                "{what}"
            );
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Plan-wide effect limit (R8 FAIL_V2, remediation of lane SMB-COMPOSITE-ADJ). Each layer is within
// pigment's MAX_EFFECTS_PER_LAYER, but `PlanExtensions::validate` also counts the effects of ALL
// layers against MAX_EFFECTS_PER_PLAN, so only the whole envelope can see that rule. Lowering used
// to drop the whole-envelope error and return Ok, which pigment's admission then refused for every
// backend as InvalidPlan(EffectLimit). The limits below are pigment's constants, never literals.
// ---------------------------------------------------------------------------------------------
use hsk_studio_pigment::effect_math::{MAX_EFFECTS_PER_LAYER, MAX_EFFECTS_PER_PLAN};

/// Root group 1 over one raster per entry of `counts` (ids 3.., an 8x8 tile each, in stack order),
/// and with `modern_adjustment` a modern brightness/contrast adjustment layer (id 2, a graph source
/// at the bottom of the stack; pigment refuses its kind at admission and composite carries it).
/// Raster `i` carries `counts[i]` colour overlays. Returns the document and the record entries.
fn limit_fixture(counts: &[usize], modern_adjustment: bool) -> (Value, Vec<Value>) {
    let mut doc = mixed();
    assign_all_profiles(&mut doc);
    let mut layers = vec![layer(1, "group", json!({"payload_kind":"group"}))];
    let mut operations = vec![composite(1)];
    let mut entries = Vec::new();
    let mut stack: Vec<u8> = Vec::new();
    if modern_adjustment {
        layers.push(layer(
            2,
            "adjustment",
            json!({"payload_kind":"primitive_reference","schema_id":"hsk.studio.adjustment@1",
                "object_id":identity("SLYR",2)}),
        ));
        operations.push(source(2, "image"));
        entries.push(json!({"layer_id":identity("SLYR",2),"adjustment":{"kind":"brightness_contrast",
            "brightness":10.0,"contrast":10.0,"use_legacy":false}}));
        stack.push(2);
    }
    for (i, count) in counts.iter().enumerate() {
        let n = u8::try_from(3 + i).expect("fixture layer id fits the identity helper");
        layers.push(layer(
            n,
            "raster",
            json!({"payload_kind":"raster","tiles":[tile(n, &format!("t{n}"), 0, 0, 8, 8)]}),
        ));
        operations.push(source(n, "image"));
        stack.push(n);
        if *count > 0 {
            let effects = (0..*count)
                .map(|k| {
                    fx_effect(
                        &format!("co_{n}_{k}"),
                        50.0,
                        fx_colour(1.0, 0.0, 0.0),
                        false,
                        json!({"effect_kind":"colour_overlay"}),
                    )
                })
                .collect();
            entries.push(json!({"layer_id":identity("SLYR", n),"effects":fx_stack(effects)}));
        }
    }
    let edges: Vec<Value> = stack
        .iter()
        .enumerate()
        .map(|(ordinal, n)| {
            let ordinal = u64::try_from(ordinal).expect("fixture ordinal");
            edge(address(*n, "source", "out"), address(1, "mix", "inputs"), ordinal)
        })
        .collect();
    doc["layers"] = Value::Array(layers);
    doc["graph"] = json!({"schema_id":"hsk.studio.layer_graph@1","operations":operations,
        "edges":edges,"outputs":[address(1,"mix","out")]});
    (doc, entries)
}
/// Evaluate and lower the root of `doc` with the extended path.
fn limit_flow(doc: &Value, entries: Vec<Value>) -> Result<ExtendedLowering, CompositeError> {
    let recs = records(Value::Array(entries));
    let run = run_ext(doc, &recs, false);
    lower_ext(&run, &recs)
}
fn total_effects(low: &ExtendedLowering) -> usize {
    low.extensions.layers.iter().map(|e| e.effects.len()).sum()
}

#[test]
fn plan_wide_effect_limit_is_refused_at_lowering_like_pigment_admission() {
    bounded_case("plan_wide_effect_limit_is_refused_at_lowering_like_pigment_admission", || {
        let token = CancellationToken::default();
        let at_limit = MAX_EFFECTS_PER_PLAN / MAX_EFFECTS_PER_LAYER;
        assert_eq!(at_limit * MAX_EFFECTS_PER_LAYER, MAX_EFFECTS_PER_PLAN, "exact division");
        let full = vec![MAX_EFFECTS_PER_LAYER; at_limit];
        let one_more_layer = [full.clone(), vec![MAX_EFFECTS_PER_LAYER]].concat();

        // Control, exactly at the limit (64 layers x 64 effects): lowers, pigment validates and
        // admits the lowered envelope.
        let (doc, entries) = limit_fixture(&full, false);
        let at = limit_flow(&doc, entries).expect("the limit itself is admissible");
        assert_eq!(total_effects(&at), MAX_EFFECTS_PER_PLAN);
        assert!(at.extensions.validate(&at.plan).is_ok());
        assert!(admit_extended(&at.plan, &at.extensions, &ExtBackend, &request(), &token).is_ok());

        // The reviewed case (65 layers x 64 effects): every layer is within its own limit, the
        // plan is not. A typed lowering refusal with pigment's own error, not an Ok that pigment's
        // admission then refuses.
        let (doc, entries) = limit_fixture(&one_more_layer, false);
        let refused = match limit_flow(&doc, entries) {
            Err(CompositeError::Plan(e @ PlanError::EffectLimit { .. })) => e,
            other => panic!("expected Plan(EffectLimit), got {other:?}"),
        };
        assert_eq!(CompositeError::Plan(refused).code(), PlanError::EffectLimit { node: 0 }.code());

        // Parity with pigment's admission on the smallest overshoot (one effect over the limit):
        // the lowering of that document returns the very error admit_extended gives the envelope
        // of the same plan. That envelope is the at-limit lowering of a document whose 65th layer
        // has no effects, plus one more effect on that layer.
        let mut tail_without_effects = full.clone();
        tail_without_effects.push(0);
        let (doc, entries) = limit_fixture(&tail_without_effects, false);
        let base = limit_flow(&doc, entries).expect("65th layer without effects lowers");
        let mut over = base.extensions.clone();
        let one = base.extensions.layers[0].effects[0];
        over.layers.push(
            LayerExtension::new(base.extensions.layers[0].group, u32::try_from(at_limit).unwrap())
                .with_effects(vec![one]),
        );
        let admission = admit_extended(&base.plan, &over, &ExtBackend, &request(), &token);
        let mut smallest = full.clone();
        smallest.push(1);
        let (doc, entries) = limit_fixture(&smallest, false);
        let Err(CompositeError::Plan(lowering)) = limit_flow(&doc, entries) else {
            panic!("one effect over the plan limit must be a typed lowering refusal")
        };
        assert!(matches!(lowering, PlanError::EffectLimit { .. }));
        assert_eq!(admission, Err(BackendError::InvalidPlan(lowering)));

        // Only the effects of rendered layers count: a hidden layer is not emitted, so 65 layers
        // of which one is hidden are at the limit and lower.
        let (mut doc, entries) = limit_fixture(&one_more_layer, false);
        doc["layers"][1]["visible"] = json!(false);
        let rendered = limit_flow(&doc, entries).expect("a hidden layer's effects are not counted");
        assert_eq!(total_effects(&rendered), MAX_EFFECTS_PER_PLAN);

        // Carried features are not allowed to hide the plan-wide defect behind them: an enabled
        // knockout (carried to pigment, refused there as knockout_shallow) on the first layer, and
        // a modern brightness/contrast adjustment (carried, refused as brightness_contrast) before
        // the effect layers each leave the refusal in place.
        let (doc, mut entries) = limit_fixture(&one_more_layer, false);
        entries[0]["advanced_blending"] = json!({"blend_ranges":[],"knockout":"enabled",
            "channel_blend":{"red":true,"green":true,"blue":true},
            "blend_interior_effects":true,"blend_clipped_effects":true,"transparency_shapes_layer":true});
        assert!(
            matches!(limit_flow(&doc, entries), Err(CompositeError::Plan(PlanError::EffectLimit { .. }))),
            "knockout on the first layer must not hide the limit"
        );
        let (doc, entries) = limit_fixture(&one_more_layer, true);
        assert!(
            matches!(limit_flow(&doc, entries), Err(CompositeError::Plan(PlanError::EffectLimit { .. }))),
            "a refused adjustment before the effect layers must not hide the limit"
        );
        // ... and without the overshoot those same carried features are still carried, not errors.
        let (doc, entries) = limit_fixture(&full, true);
        let carried = limit_flow(&doc, entries).expect("a carried adjustment lowers");
        assert_eq!(total_effects(&carried), MAX_EFFECTS_PER_PLAN);
        assert_eq!(carried.exactness.lowest, Exactness::Unsupported);
        assert_eq!(
            admit_extended(&carried.plan, &carried.extensions, &ExtBackend, &request(), &token),
            Err(BackendError::Unsupported { operator: "brightness_contrast", exactness: Exactness::Unsupported })
        );
    });
}
