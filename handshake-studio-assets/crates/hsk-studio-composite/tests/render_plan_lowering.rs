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

#[test]
fn pass_through_group_as_clip_base_is_unavailable_and_unsupported_in_the_ledger() {
    bounded_case("pass_through_group_as_clip_base", || {
        // Root group 1 = [group 7 (the base; contains raster 2), raster 3 clipped to it]. Group
        // mode 1 = pass_through (non-isolated), 2 = Normal (isolated). Photoshop semantics for a
        // pass-through clip base are UNVERIFIED (oracle pending): refused, never guessed.
        let root = addr(1, "mix", "out");
        let token = CancellationToken::default();
        let fixture = |mode: u16| {
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
        };

        // Pass-through base: typed Unavailable at lowering, Unsupported in the evaluation ledger
        // and on the parent step.
        let v = fixture(1);
        match lowered(&v, BlendSpace::Encoded, &root) {
            Err(CompositeError::Unavailable(reason)) => assert!(reason.contains("clip_base_pass_through"), "{reason}"),
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
            .any(|o| o.key == "clip_base_pass_through" && o.exactness == Exactness::Unsupported));
        match &report.step(&root).unwrap().status {
            StepStatus::Unavailable(reason) => assert!(reason.contains("clip_base_pass_through"), "{reason}"),
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
use hsk_studio_pigment::blend_math::OperatorExactness;
use hsk_studio_pigment::coverage_math::BlendChannel;
use hsk_studio_pigment::render_plan::{
    ExtOperator, Knockout, LayerExtension, PASS_THROUGH_FILL_OPERATOR, admit_extended,
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

        // A real Folio graph has no operation for the adjustment layer, so no stack position: a
        // visible adjustment layer contained in the rendered group is refused, never dropped.
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
        // Pigment refuses it at admission (typed, before any sink call), never renders without it.
        assert_eq!(
            admit_extended(&low.plan, &low.extensions, &ExtBackend, &request(), &token),
            Err(BackendError::Unsupported { operator: "knockout_shallow", exactness: Exactness::Unsupported })
        );
        assert_eq!(low.exactness.lowest, Exactness::Unsupported);
        // Evaluation says so too: Unsupported in the receipt (the outcome mapping is checked in
        // `evaluation_ledger_keys_and_classes_are_pigments`, on a document without unrelated
        // vector/text outputs, which would make this report Unavailable for another reason).
        assert_eq!(report.receipt.exactness.lowest, Exactness::Unsupported);

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

        // An enabled effect stack with an enabled instance is refused (effects wave 1 pending);
        // a disabled stack, or one without enabled instances, is not.
        let stack = |enabled: bool, instance_enabled: bool| {
            json!({"effect_stack_id":"SEFX-019abcde-0000-7000-8000-000000000001","enabled":enabled,"scale":100.0,
                "effects":[{"instance_id":"fx_1","enabled":instance_enabled,"visible_in_editor":true,
                    "blend_mode":5,"opacity":75.0,"colour":{"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":null},
                    "anti_alias":false,"noise":0.0,"angle":120.0,"use_global_light":true,
                    "params":{"effect_kind":"colour_overlay"}}]})
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
        assert_eq!(ledger.lowest, Exactness::Unsupported, "knockout is intrinsically unsupported");
        assert_eq!(eval_outcome(&Ok(report)), Outcome::Failure(FailureCode::Unsupported));

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
