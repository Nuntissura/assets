use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_composite::{
    BlendError, StudioBlendMode, BlendSpace, CompositeError, DESCRIPTOR, Exactness, Limits,
    Step, StepStatus, eval_outcome, evaluate, layer_projection, lower, node_projection,
    outcome_of, report,
};
use hsk_studio_folio::{
    PortAddress, Unresolved,
};
use hsk_studio_observe::{
    Budget as ObserveBudget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};
use serde_json::{Value, json};

mod common;
use common::*;

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
            (StudioBlendMode::Behind, BlendError::ToolOnly(StudioBlendMode::Behind)),
            (StudioBlendMode::Average, BlendError::Unsupported(StudioBlendMode::Average)),
        ] {
            let mut refused = plan.clone();
            for s in &mut refused.steps {
                if let Step::Composite { appearance: a, .. } = s {
                    a.mode = mode;
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
            outcome_of::<()>(&Err(CompositeError::Blend(BlendError::ToolOnly(StudioBlendMode::Clear)))),
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
            "hsk-studio-observe",
            "serde_json",
        ];
        assert!(names.iter().all(|n| allowed.contains(&n.as_str())), "{names:?}");
        // The descriptor is valid JSON and states the same closure and limits.
        let d: Value = serde_json::from_str(DESCRIPTOR).unwrap();
        assert_eq!(d["owner"], "STUDIO-MODULE-COMPOSITE");
        assert_eq!(d["limits"]["case_seconds"], 30);
        assert_eq!(d["version"], 3);
        assert!(d["model"]["owner_of_blend_maths"].as_str().unwrap().contains("hsk_studio_pigment::blend_math"));
        // CX-CAP-001: composite keeps no second copy of the blend enumeration or maths.
        for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            for banned in ["enum StudioBlendMode", "enum BlendMode", "MODE_TABLE", "fn blend_rgb", "fn composite_over"] {
                assert!(!text.contains(banned), "second blend owner: {banned}");
            }
        }
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
        let linear = lower(&ready_snap, 7, BlendSpace::LinearLight, &Limits::default(), &token).unwrap();
        assert!(linear.blend_profile_bound);
        let ready_report = evaluate(&linear, &ready_snap, &token).unwrap();
        assert_eq!(ready_report.require_ready(), Ok(()));
        assert_eq!(eval_outcome(&Ok(ready_report.clone())), Outcome::Success);
        // The declared blend space and lowest fidelity travel in the receipt.
        assert_eq!(ready_report.receipt.blend_space, BlendSpace::LinearLight);
        assert_eq!(ready_report.receipt.exactness.lowest, Exactness::Exact);
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
fn receipt_lowest_exactness_and_observe_report() {
    bounded_case("receipt_lowest_exactness_and_observe_report", || {
        let token = CancellationToken::default();
        let snap = open(&mixed());
        let mut plan = plan_of(&snap);
        // An approximate composite operator lowers the receipt's class; it never reads as Exact.
        for s in &mut plan.steps {
            if let Step::Composite { appearance, .. } = s {
                appearance.mode = StudioBlendMode::LinearBurn;
            }
        }
        let report_data = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(report_data.receipt.exactness.lowest, Exactness::Approximate);
        assert!(report_data
            .receipt
            .exactness.operators
            .iter()
            .any(|o| o.key == "linear_burn" && o.exactness == Exactness::Approximate));
        assert_eq!(plan.lowest_exactness(), Exactness::Approximate);

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
