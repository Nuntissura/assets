use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::*;

fn bounded_case(body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("bounded_case exceeded 20 s"),
    }
}

const R: Rect = Rect {
    x: 0,
    y: 0,
    width: 2,
    height: 1,
};

fn target() -> OutputTarget {
    OutputTarget {
        extent: R,
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag {
            profile_sha256: [7; 32],
            blend_space: BlendSpace::LinearLight,
        },
    }
}

fn layer(input: u32, mode: StudioBlendMode) -> Layer {
    Layer {
        input: PlanNodeId(input),
        mode,
        opacity: 1.0,
        fill_opacity: 1.0,
        mask: None,
    }
}

/// tile(0) + solid(1) in an isolated group(2) with `mode` on the solid; pass-through group(3)
/// holding group(2); root group(4) referencing (3) as pass-through.
fn plan(mode: StudioBlendMode) -> RenderPlan {
    RenderPlan {
        revision: 9,
        target: target(),
        nodes: vec![
            PlanNode::Tile(TileInput {
                key: SourceKey(0),
                placement: R,
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                colour_sha256: [1; 32],
                alpha_sha256: [2; 32],
            }),
            PlanNode::Solid {
                rect: R,
                rgba: [1.0, 0.0, 0.0, 1.0],
            },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    layer(0, StudioBlendMode::Normal),
                    Layer {
                        opacity: 0.5,
                        ..layer(1, mode)
                    },
                ],
            }),
            PlanNode::Group(Group {
                isolated: false,
                layers: vec![layer(2, StudioBlendMode::Normal)],
            }),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![layer(3, StudioBlendMode::PassThrough)],
            }),
        ],
        root: PlanNodeId(4),
    }
}

#[test]
fn plan_validation_refuses_malformed_plans() {
    bounded_case(|| {
        let limits = PlanLimits::default();
        let ok = plan(StudioBlendMode::Multiply);
        let ops = ok.validate(&limits).expect("valid plan");
        for op in [
            Operator::Tile,
            Operator::Solid,
            Operator::Blend(StudioBlendMode::Multiply),
            Operator::IsolatedGroup,
            Operator::PassThroughGroup,
        ] {
            assert!(ops.contains(&op), "missing {op:?}");
        }
        // Group(3) is non-isolated and only referenced by the root's PassThrough layer.
        assert!(!ops.contains(&Operator::NonIsolatedBlendedGroup));
        let digest = ok.digest().unwrap();
        assert_eq!(digest, ok.digest().unwrap());
        assert_ne!(digest, plan(StudioBlendMode::Screen).digest().unwrap());

        let mut p = plan(StudioBlendMode::Normal);
        let PlanNode::Group(g) = &mut p.nodes[2] else {
            unreachable!()
        };
        g.layers[0].input = PlanNodeId(3);
        assert_eq!(
            p.validate(&limits),
            Err(PlanError::ForwardReference { node: 2, input: 3 })
        );
        for (mode, err) in [
            (
                StudioBlendMode::PassThrough,
                PlanError::ModeNotApplicable {
                    node: 2,
                    mode: StudioBlendMode::PassThrough,
                },
            ),
            (
                StudioBlendMode::Behind,
                PlanError::ModeNotApplicable {
                    node: 2,
                    mode: StudioBlendMode::Behind,
                },
            ),
        ] {
            assert_eq!(plan(mode).validate(&limits), Err(err));
        }
        let mut p = plan(StudioBlendMode::Normal);
        let PlanNode::Group(g) = &mut p.nodes[3] else {
            unreachable!()
        };
        g.isolated = true;
        assert_eq!(
            p.validate(&limits),
            Err(PlanError::PassThroughOnIsolated { node: 4 })
        );
        let mut p = plan(StudioBlendMode::Normal);
        let PlanNode::Group(g) = &mut p.nodes[2] else {
            unreachable!()
        };
        g.layers[1].opacity = f32::NAN;
        assert_eq!(p.validate(&limits), Err(PlanError::NonFinite { node: 2 }));
        g_set_fill(&mut p);
        assert_eq!(p.validate(&limits), Err(PlanError::OutOfRange { node: 2 }));
        let mut p = plan(StudioBlendMode::Normal);
        p.root = PlanNodeId(5);
        assert_eq!(p.validate(&limits), Err(PlanError::RootOutOfRange));
        let tight = PlanLimits {
            max_depth: 2,
            ..limits
        };
        assert_eq!(
            plan(StudioBlendMode::Normal).validate(&tight),
            Err(PlanError::TooDeep)
        );
    });
}

fn g_set_fill(p: &mut RenderPlan) {
    let PlanNode::Group(g) = &mut p.nodes[2] else {
        unreachable!()
    };
    g.layers[1].opacity = 1.0;
    g.layers[1].fill_opacity = 1.5;
}

/// Minimal CPU-like backend: Normal/Multiply exact, everything else unsupported. Emits one
/// transparent chunk; proves the port is implementable, object safe and admission-first.
struct Probe;
impl RenderBackend for Probe {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "probe",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, operator: Operator) -> Exactness {
        match operator {
            Operator::Blend(StudioBlendMode::Normal | StudioBlendMode::Multiply)
            | Operator::Blend(StudioBlendMode::LinearBurn)
            | Operator::Tile
            | Operator::Solid
            | Operator::IsolatedGroup
            | Operator::PassThroughGroup => Exactness::Exact,
            _ => Exactness::Unsupported,
        }
    }
    fn execute(
        &self,
        plan: &RenderPlan,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        let exactness = admit(plan, self, request, cancel)?;
        for node in &plan.nodes {
            if let PlanNode::Tile(t) = node {
                let planes = sources
                    .tile(t.key)
                    .ok_or(BackendError::UnresolvedSource(t.key))?;
                if !planes.matches(t, plan.target.space) {
                    return Err(BackendError::SourceMismatch(t.key));
                }
            }
        }
        let px = plan.target.extent.area() as usize;
        sink.accept(plan.target.extent, &vec![0; px * 12], &vec![0; px * 4])
            .map_err(BackendError::Sink)?;
        Ok(RenderReceipt {
            plan_revision: plan.revision,
            plan_digest: plan.digest().map_err(|_| BackendError::Overflow)?,
            backend: self.identity(),
            region: plan.target.extent,
            target: plan.target,
            exactness,
            chunks: 1,
            nodes_executed: plan.nodes.len() as u64,
            peak_working_bytes: 0,
        })
    }
}

struct Sources {
    colour: Vec<u8>,
    alpha: Vec<u8>,
    colour_sha256: [u8; 32],
}
impl PlanSources for Sources {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        (key == SourceKey(0)).then_some(TilePlanes {
            width: 2,
            height: 1,
            colour: &self.colour,
            colour_stride_bytes: 24,
            alpha: &self.alpha,
            alpha_stride_bytes: 8,
            colour_sha256: self.colour_sha256,
            alpha_sha256: [2; 32],
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: target().space,
        })
    }
    fn mask(&self, _key: SourceKey) -> Option<hsk_studio_pigment::Mask<'_>> {
        None
    }
}

#[derive(Default)]
struct Chunks(u32);
impl TileSink for Chunks {
    fn accept(&mut self, _rect: Rect, _colour: &[u8], _alpha: &[u8]) -> Result<(), SinkError> {
        self.0 += 1;
        Ok(())
    }
}

#[test]
fn admission_floors_exactness_through_object_safe_port() {
    bounded_case(|| {
        let backend: &dyn RenderBackend = &Probe;
        let cancel = CancellationToken::default();
        let sources = Sources {
            colour: vec![0; 24],
            alpha: vec![0; 8],
            colour_sha256: [1; 32],
        };
        let request = |floor| ExecuteRequest {
            expected_revision: 9,
            min_exactness: floor,
            deadline: None,
            max_working_bytes: 1 << 20,
        };
        let mut sink = Chunks::default();
        let receipt = backend
            .execute(
                &plan(StudioBlendMode::Multiply),
                &request(Exactness::Exact),
                &sources,
                &cancel,
                &mut sink,
            )
            .expect("exact plan renders");
        assert_eq!(sink.0, 1);
        assert_eq!(receipt.plan_revision, 9);
        assert_eq!(receipt.exactness.lowest, Exactness::Exact);
        assert_eq!(receipt.target.space.blend_space.key(), "linear_light");
        assert!(
            receipt
                .exactness
                .operators
                .iter()
                .any(|o| o.key == "multiply")
        );

        // Approximate intrinsic mode: refused under an Exact floor, admitted under Approximate.
        let refused = backend.execute(
            &plan(StudioBlendMode::LinearBurn),
            &request(Exactness::Exact),
            &sources,
            &cancel,
            &mut sink,
        );
        assert_eq!(
            refused,
            Err(BackendError::Unsupported {
                operator: "linear_burn",
                exactness: Exactness::Approximate
            })
        );
        let r = backend
            .execute(
                &plan(StudioBlendMode::LinearBurn),
                &request(Exactness::Approximate),
                &sources,
                &cancel,
                &mut sink,
            )
            .unwrap();
        assert_eq!(r.exactness.lowest, Exactness::Approximate);
        // Backend-unsupported and unrecovered modes refuse under any floor, never Normal.
        for mode in [StudioBlendMode::Screen, StudioBlendMode::Average] {
            let e = backend
                .execute(
                    &plan(mode),
                    &request(Exactness::Approximate),
                    &sources,
                    &cancel,
                    &mut sink,
                )
                .unwrap_err();
            assert_eq!(e.code(), "unsupported_operator");
        }
        let before = sink.0;
        let stale = backend.execute(
            &plan(StudioBlendMode::Normal),
            &ExecuteRequest {
                expected_revision: 8,
                ..request(Exactness::Exact)
            },
            &sources,
            &cancel,
            &mut sink,
        );
        assert_eq!(
            stale,
            Err(BackendError::StaleRevision {
                expected: 8,
                found: 9
            })
        );
        let wrong = Sources {
            colour_sha256: [3; 32],
            ..sources
        };
        assert_eq!(
            backend.execute(
                &plan(StudioBlendMode::Normal),
                &request(Exactness::Exact),
                &wrong,
                &cancel,
                &mut sink
            ),
            Err(BackendError::SourceMismatch(SourceKey(0)))
        );
        cancel.cancel();
        assert_eq!(
            backend.execute(
                &plan(StudioBlendMode::Normal),
                &request(Exactness::Exact),
                &wrong,
                &cancel,
                &mut sink
            ),
            Err(BackendError::Canceled)
        );
        assert_eq!(sink.0, before, "refusals never reach the sink");
    });
}
