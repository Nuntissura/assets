use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_composite::{
    BlendError, BlendMode, BlendSpace, CompositeError, DESCRIPTOR, Fidelity, Limits, LoweredPlan,
    NodeAddr, Step, StepStatus, eval_outcome, evaluate, layer_projection, lower, node_projection,
    outcome_of, report,
};
use hsk_studio_folio::{
    Budget, Inspection, PortAddress, Resolution, Resolver, Snapshot, TileRef, Unresolved,
    inspect_bytes,
};
use hsk_studio_observe::{
    Budget as ObserveBudget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};
use serde_json::{Value, json};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(body));
        let _ = sender.send(outcome);
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared 30-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost deadline worker result")
        }
    }
}

fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn address(n: u8, op: &str, port: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":op,"port_key":port})
}
fn source(n: u8, ty: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":ty,"cardinality":"one","semantic":"source"}]})
}
fn composite(n: u8) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"mix","operation_kind":"composite","inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]})
}
fn layer(n: u8, kind: &str, payload: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("layer-{n}"),"parent":null,"kind":kind,"payload":payload,"visible":true})
}
fn edge(source: Value, target: Value, n: u64) -> Value {
    json!({"source":source,"target":target,"dependency_kind":"data","input_ordinal":n})
}
fn tile(layer_n: u8, key: &str, column: i64, row: i64, w: u32, h: u32) -> Value {
    json!({"object_key":key,"layer_id":identity("SLYR",layer_n),"column":column,"row":row,
        "width":{"value":w,"unit":"px"},"height":{"value":h,"unit":"px"},"format":"caller-format",
        "colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":"00"},"artifact_manifest_id":"manifest-1"})
}
/// Folio foundation fixture: group 1 composites rasters 2 then 3 (edges stored out of order);
/// vector 4 and text 5 are standalone sources.
fn mixed() -> Value {
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"mm","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[layer(1,"group",json!({"payload_kind":"group"})),layer(2,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(3,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(4,"vector",json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})),layer(5,"text",json!({"payload_kind":"text","story_id":identity("STXT",1)}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2,"image"),source(3,"image"),source(4,"vector"),source(5,"text"),composite(1)],"edges":[edge(address(3,"source","out"),address(1,"mix","inputs"),1),edge(address(2,"source","out"),address(1,"mix","inputs"),0)],"outputs":[address(1,"mix","out"),address(4,"source","out"),address(5,"source","out")]}
    })
}
fn assign_all_profiles(v: &mut Value) {
    for name in [
        "working_space_rgb",
        "working_space_cmyk",
        "working_space_gray",
        "working_space_spot",
        "blending_space",
    ] {
        v[name] = json!(identity("SCPF", 1));
    }
}

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
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}
fn open_bytes(bytes: &[u8], resolver: &dyn Resolver) -> Snapshot {
    match inspect_bytes(bytes, Budget::default(), &CancellationToken::default(), resolver).unwrap()
    {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("typed document became read-only"),
    }
}
fn open(v: &Value) -> Snapshot {
    open_bytes(&serde_json::to_vec(v).unwrap(), &Available)
}
fn plan_of(s: &Snapshot) -> LoweredPlan {
    lower(
        s,
        s.document().revision,
        BlendSpace::Encoded,
        &Limits::default(),
        &CancellationToken::default(),
    )
    .unwrap()
}
fn addr(n: u8, op: &str, port: &str) -> NodeAddr {
    NodeAddr {
        layer_id: identity("SLYR", n),
        operation_key: op.into(),
        port_key: port.into(),
    }
}

#[test]
fn layer_node_projection_same_authored_ids() {
    bounded_case("layer_node_projection_same_authored_ids", || {
        // Third input stored FIRST with ordinal 2: composite order must follow ordinals, not storage.
        let mut v = mixed();
        v["layers"]
            .as_array_mut()
            .unwrap()
            .push(layer(6, "raster", json!({"payload_kind":"raster","tiles":[]})));
        v["graph"]["operations"].as_array_mut().unwrap().push(source(6, "image"));
        v["graph"]["edges"]
            .as_array_mut()
            .unwrap()
            .insert(0, edge(address(6, "source", "out"), address(1, "mix", "inputs"), 2));
        let original = open(&v);
        let before = original.encoded_bytes().to_vec();
        let plan = plan_of(&original);

        // Kahn order with (layer_id, operation_key) tie-break, derived by hand:
        // ready {2,3,4,5,6} -> 2, 3, 4, 5, 6 pop in id order; composite 1 becomes ready only
        // after 2, 3 and 6 are emitted and sorts first among what remains.
        let order: Vec<u8> = plan
            .steps
            .iter()
            .map(|s| u8::from_str_radix(&s.addr().layer_id[s.addr().layer_id.len() - 2..], 16).unwrap())
            .collect();
        assert_eq!(order, vec![2, 3, 4, 5, 6, 1]);

        let group = identity("SLYR", 1);
        let target: PortAddress = serde_json::from_value(address(1, "mix", "inputs")).unwrap();
        let authored: Vec<String> = original
            .ordered_inputs(&target)
            .iter()
            .map(|a| a.layer_id.clone())
            .collect();
        assert_eq!(
            authored,
            vec![identity("SLYR", 2), identity("SLYR", 3), identity("SLYR", 6)]
        );
        // layer view == authored ids; node view == the same ids by address; layer->node->layer identity.
        let layer_view = layer_projection(&plan, &group).unwrap();
        let node_view = node_projection(&plan, &group).unwrap();
        assert_eq!(layer_view, authored);
        assert_eq!(
            node_view,
            vec![addr(2, "source", "out"), addr(3, "source", "out"), addr(6, "source", "out")]
        );
        let round: Vec<String> = node_view.iter().map(|n| n.layer_id.clone()).collect();
        assert_eq!(round, layer_view);
        assert!(layer_projection(&plan, &identity("SLYR", 99)).is_none());
        assert_eq!(plan.outputs.len(), 3);
        assert_eq!(plan.revision, 7);

        // Authored record equality: lowering never touches the snapshot, and a reopen of the
        // encoded bytes lowers to an identical plan (STU-CMP-094 minimal).
        assert_eq!(original.encoded_bytes(), before.as_slice());
        let reopened = open_bytes(original.encoded_bytes(), &Available);
        assert_eq!(plan_of(&reopened), plan);
        assert_eq!(reopened.encoded_bytes(), before.as_slice());
    });
}

#[test]
fn fanout_cycle_refusal_cancel_stale() {
    bounded_case("fanout_cycle_refusal_cancel_stale", || {
        // Fanout: source 2 feeds group 1 AND group 7; it appears once, referenced twice.
        let mut v = mixed();
        v["layers"].as_array_mut().unwrap().push(layer(7, "group", json!({"payload_kind":"group"})));
        v["graph"]["operations"].as_array_mut().unwrap().push(composite(7));
        v["graph"]["edges"]
            .as_array_mut()
            .unwrap()
            .push(edge(address(2, "source", "out"), address(7, "mix", "inputs"), 0));
        let snap = open(&v);
        let plan = plan_of(&snap);
        let source_steps = plan.steps.iter().filter(|s| s.addr() == &addr(2, "source", "out")).count();
        assert_eq!(source_steps, 1);
        let references = plan
            .steps
            .iter()
            .filter(|s| matches!(s, Step::Composite { inputs, .. } if inputs.contains(&addr(2, "source", "out"))))
            .count();
        assert_eq!(references, 2);

        // Cycle (defence in depth): a hand-built plan whose composite precedes its inputs.
        let token = CancellationToken::default();
        let mut cyclic = plan.clone();
        cyclic.steps.reverse();
        assert!(matches!(evaluate(&cyclic, &snap, &token), Err(CompositeError::Cycle(_))));
        // Dangling: an input that is not a step of the plan.
        let mut dangling = plan.clone();
        for s in &mut dangling.steps {
            if let Step::Composite { inputs, .. } = s {
                inputs.push(addr(90, "source", "out"));
            }
        }
        assert!(matches!(evaluate(&dangling, &snap, &token), Err(CompositeError::DanglingInput(_))));

        // Refusals: tool-only / unrecovered composite modes are typed errors, never Normal.
        for (mode, expected) in [
            (BlendMode::Behind, BlendError::ToolOnly(BlendMode::Behind)),
            (BlendMode::Average, BlendError::Unsupported(BlendMode::Average)),
        ] {
            let mut refused = plan.clone();
            for s in &mut refused.steps {
                if let Step::Composite { mode: m, .. } = s {
                    *m = mode;
                }
            }
            assert_eq!(evaluate(&refused, &snap, &token), Err(CompositeError::Blend(expected)));
        }

        // Cancel: both entry points honour a pre-cancelled token.
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert_eq!(
            lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &cancelled),
            Err(CompositeError::Canceled)
        );
        assert_eq!(evaluate(&plan, &snap, &cancelled), Err(CompositeError::Canceled));

        // Stale: wrong expected revision at lowering, and a plan older than the snapshot.
        assert_eq!(
            lower(&snap, 6, BlendSpace::Encoded, &Limits::default(), &token),
            Err(CompositeError::StaleRevision { expected: 6, actual: 7 })
        );
        let mut old = plan.clone();
        old.revision = 6;
        assert_eq!(
            evaluate(&old, &snap, &token),
            Err(CompositeError::StaleRevision { expected: 6, actual: 7 })
        );

        // Budgets are errors, never truncation. Depth 2 (source -> composite) exceeds max_depth 1.
        assert_eq!(
            lower(&snap, 7, BlendSpace::Encoded, &Limits { max_steps: 2, max_depth: 64 }, &token),
            Err(CompositeError::BudgetExceeded)
        );
        assert_eq!(
            lower(&snap, 7, BlendSpace::Encoded, &Limits { max_steps: 64, max_depth: 1 }, &token),
            Err(CompositeError::BudgetExceeded)
        );

        // Outcome mapping used by the observability port.
        assert_eq!(outcome_of::<()>(&Err(CompositeError::Canceled)), Outcome::Canceled);
        assert_eq!(
            outcome_of::<()>(&Err(CompositeError::Cycle("x".into()))),
            Outcome::Failure(FailureCode::Validation)
        );
        assert_eq!(
            outcome_of::<()>(&Err(CompositeError::Blend(BlendError::ToolOnly(BlendMode::Clear)))),
            Outcome::Failure(FailureCode::Unsupported)
        );
    });
}

#[test]
fn static_closure_no_temporal_provider() {
    bounded_case("static_closure_no_temporal_provider", || {
        let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
        let mut in_deps = false;
        let mut names = Vec::new();
        for line in manifest.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_deps = line == "[dependencies]" || line == "[dev-dependencies]";
                continue;
            }
            if in_deps && let Some((name, _)) = line.split_once('=') {
                names.push(name.trim().to_owned());
            }
        }
        assert!(!names.is_empty());
        for name in &names {
            for banned in ["motion", "pulse", "reel", "score", "render-", "handshake_"] {
                assert!(!name.contains(banned), "static closure depends on {name}");
            }
        }
        let allowed = [
            "hsk-studio-accord",
            "hsk-studio-folio",
            "hsk-studio-pigment",
            "hsk-studio-prism",
            "hsk-studio-observe",
            "serde_json",
        ];
        assert!(names.iter().all(|n| allowed.contains(&n.as_str())), "{names:?}");
        // The descriptor is valid JSON and states the same closure and limits.
        let d: Value = serde_json::from_str(DESCRIPTOR).unwrap();
        assert_eq!(d["owner"], "STUDIO-MODULE-COMPOSITE");
        assert_eq!(d["limits"]["case_seconds"], 30);
        assert_eq!(d["model"]["blend"]["modes"], 35);
    });
}

#[test]
fn extents_hidden_and_unavailable() {
    bounded_case("extents_hidden_and_unavailable", || {
        // Layer 2: tiles (0,0) 64x64 and (1,0) 64x32 -> bounding union (0,0)+128x64.
        // Layer 3: tile (0,1) 64x64 -> grid cell y=64 -> (0,64)+64x64. Group union = (0,0)+128x128.
        let mut v = mixed();
        v["layers"][1]["payload"] = json!({"payload_kind":"raster","tiles":[tile(2,"t00",0,0,64,64),tile(2,"t10",1,0,64,32)]});
        v["layers"][2]["payload"] = json!({"payload_kind":"raster","tiles":[tile(3,"t01",0,1,64,64)]});
        let token = CancellationToken::default();
        let snap = open(&v);
        let plan = plan_of(&snap);
        let report_all = evaluate(&plan, &snap, &token).unwrap();
        let ext = |r: &hsk_studio_composite::EvalReport, n: u8, op: &str, port: &str| {
            r.step(&addr(n, op, port)).unwrap().extent
        };
        let rect = |x, y, width, height| Some(hsk_studio_pigment::Rect { x, y, width, height });
        assert_eq!(ext(&report_all, 2, "source", "out"), rect(0, 0, 128, 64));
        assert_eq!(ext(&report_all, 3, "source", "out"), rect(0, 64, 64, 64));
        assert_eq!(ext(&report_all, 1, "mix", "out"), rect(0, 0, 128, 128));

        // Vector and text sources have no verified image route: Unavailable, extent None.
        for (n, reason) in [(4, "vector_to_image_conversion"), (5, "text_to_image_conversion")] {
            let s = report_all.step(&addr(n, "source", "out")).unwrap();
            assert_eq!(s.status, StepStatus::Unavailable(reason.into()));
            assert_eq!(s.extent, None);
        }

        // Hidden layer 3 stays in the plan (node view) but is excluded from the parent's extent.
        let mut hidden = v.clone();
        hidden["layers"][2]["visible"] = json!(false);
        let hidden_snap = open(&hidden);
        let hidden_plan = plan_of(&hidden_snap);
        assert!(hidden_plan.steps.iter().any(|s| s.addr() == &addr(3, "source", "out") && !s.visible()));
        assert_eq!(
            layer_projection(&hidden_plan, &identity("SLYR", 1)).unwrap().len(),
            2
        );
        let hidden_report = evaluate(&hidden_plan, &hidden_snap, &token).unwrap();
        assert_eq!(ext(&hidden_report, 1, "mix", "out"), rect(0, 0, 128, 64));

        // Unassigned profiles block the blend profile and are listed, not hidden.
        assert!(!plan.blend_profile_bound);
        assert_eq!(
            report_all.unavailable.iter().filter(|u| u.reason == "unassigned_profile").count(),
            5
        );
        assert!(matches!(report_all.require_ready(), Err(CompositeError::Unavailable(_))));
        assert_eq!(eval_outcome(&Ok(report_all.clone())), Outcome::Failure(FailureCode::Unavailable));

        // Unresolved references carry Folio's own dispositions (tile format unsupported, vector
        // and text primitives absent); nothing is silently accepted.
        let unresolved = open_bytes(&serde_json::to_vec(&v).unwrap(), &Unresolved);
        let unresolved_report = evaluate(&plan_of(&unresolved), &unresolved, &token).unwrap();
        assert!(unresolved_report.unavailable.iter().any(|u| u.reason == "unsupported"));
        assert!(unresolved_report.unavailable.iter().any(|u| u.reason == "absent"));

        // Image-only document with every profile assigned is fully ready, blend profile bound.
        let mut ready = v.clone();
        assign_all_profiles(&mut ready);
        ready["layers"].as_array_mut().unwrap().truncate(3);
        ready["graph"]["operations"] = json!([source(2, "image"), source(3, "image"), composite(1)]);
        ready["graph"]["outputs"] = json!([address(1, "mix", "out")]);
        let ready_snap = open(&ready);
        let linear = lower(&ready_snap, 7, BlendSpace::Linear, &Limits::default(), &token).unwrap();
        assert!(linear.blend_profile_bound);
        let ready_report = evaluate(&linear, &ready_snap, &token).unwrap();
        assert_eq!(ready_report.require_ready(), Ok(()));
        assert_eq!(eval_outcome(&Ok(ready_report.clone())), Outcome::Success);
        // The declared blend space and lowest fidelity travel in the receipt.
        assert_eq!(ready_report.receipt.blend.space, BlendSpace::Linear);
        assert_eq!(ready_report.receipt.blend.lowest, Fidelity::Exact);
        assert!(ready_report.receipt.blend_profile_bound);
        assert!(ready_report.receipt.static_closure);
    });
}

struct Collector(Vec<(DeliveryClass, Vec<u8>)>);
impl SinkPort for Collector {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.to_vec()));
        Ok(())
    }
}

#[test]
fn receipt_lowest_fidelity_and_observe_report() {
    bounded_case("receipt_lowest_fidelity_and_observe_report", || {
        let token = CancellationToken::default();
        let snap = open(&mixed());
        let mut plan = plan_of(&snap);
        // An approximate composite operator lowers the receipt's class; it never reads as Exact.
        for s in &mut plan.steps {
            if let Step::Composite { mode, .. } = s {
                *mode = BlendMode::LinearBurn;
            }
        }
        let report_data = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(report_data.receipt.blend.lowest, Fidelity::Approximate);
        assert!(report_data
            .receipt
            .blend
            .operators
            .iter()
            .any(|o| o.key == "linear_burn" && o.fidelity == Fidelity::Approximate));
        assert_eq!(plan.lowest_fidelity(), Fidelity::Approximate);

        // Terminal outcome delivery through the caller-owned sink; no project text is sent.
        let actor = ActorContext::new("a", "p", "o", "op", "s", "sess").unwrap();
        let resource = DomainId::parse(&identity("SDOC", 1)).unwrap();
        let mut emitter = Observe::new(42, 7, resource, actor, ObserveBudget::new(0, 0).unwrap());
        let mut sink = Collector(Vec::new());
        let result = lower(&snap, 6, BlendSpace::Encoded, &Limits::default(), &token);
        let outcome = outcome_of(&result);
        assert_eq!(outcome, Outcome::Failure(FailureCode::Validation));
        let receipt = report(outcome, 42, 7, &mut emitter, &token, &mut sink).unwrap();
        assert_eq!(receipt.outcome, outcome);
        assert_eq!(sink.0.len(), 1);
        assert_eq!(sink.0[0].0, DeliveryClass::Terminal);
        assert!(sink.0[0].1.starts_with(b"HSKO"));
    });
}
