//! Lane SMB-PIGMENT-ADMIT-PARITY (decision: work-folder
//! `diagnostics/adjustments-advanced-blending.md` topic `admission-parity`): plan features that
//! pigment admitted and both backends then refused locally are decided by shared admission, so a
//! host that runs `admit` sees the refusal that `execute` would give. Node and layer indexes and
//! the expected keys and classes are derived by hand from the plans built in this file.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::coverage_math::feather_kernel;
use hsk_studio_pigment::effect_math::MAX_EFFECT_SIZE_PX;
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::{Mask, Rect};

const R: Rect = Rect {
    x: 0,
    y: 0,
    width: 2,
    height: 1,
};

fn layer(input: u32, mode: StudioBlendMode) -> Layer {
    Layer {
        input: PlanNodeId(input),
        mode,
        opacity: 1.0,
        fill_opacity: 1.0,
        mask: None,
    }
}
fn masked(input: u32, mode: StudioBlendMode, mask: MaskInput) -> Layer {
    Layer {
        mask: Some(mask),
        ..layer(input, mode)
    }
}
fn mask(format: MaskFormat, inverted: bool, density: f32) -> MaskInput {
    MaskInput {
        key: SourceKey(9),
        placement: R,
        format,
        sha256: [5; 32],
        inverted,
        density,
    }
}
fn solid(rgba: [f32; 4]) -> PlanNode {
    PlanNode::Solid { rect: R, rgba }
}
fn group(isolated: bool, layers: Vec<Layer>) -> PlanNode {
    PlanNode::Group(Group { isolated, layers })
}
fn tile(alpha: AlphaConvention) -> PlanNode {
    PlanNode::Tile(TileInput {
        key: SourceKey(1),
        placement: R,
        format: SampleFormat::RgbF32Le,
        alpha,
        colour_sha256: [1; 32],
        alpha_sha256: [2; 32],
    })
}
fn plan_with(alpha: AlphaConvention, nodes: Vec<PlanNode>) -> RenderPlan {
    let root = u32::try_from(nodes.len() - 1).unwrap();
    RenderPlan {
        revision: 3,
        target: OutputTarget {
            extent: R,
            format: SampleFormat::RgbF32Le,
            alpha,
            space: ColourTag {
                profile_sha256: [7; 32],
                blend_space: BlendSpace::Encoded,
            },
        },
        nodes,
        root: PlanNodeId(root),
    }
}
fn plan(nodes: Vec<PlanNode>) -> RenderPlan {
    plan_with(AlphaConvention::Straight, nodes)
}
/// Nodes: 0 solid | 1 non-isolated group {0} | 2 root {layer 0: pass-through over node 1 with `l`}.
fn pass_through_plan(l: impl FnOnce(Layer) -> Layer) -> RenderPlan {
    plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![l(layer(1, StudioBlendMode::PassThrough))]),
    ])
}
fn with_mask(m: MaskInput) -> impl FnOnce(Layer) -> Layer {
    move |l| Layer { mask: Some(m), ..l }
}

/// A backend that claims `Exact` for everything except what the fields say. `Claims::all(Exact)`
/// is the most permissive backend that can exist: an intrinsic refusal must not depend on it.
struct Claims {
    normal: Exactness,
    alpha: Exactness,
    rest: Exactness,
}
impl Claims {
    fn all(class: Exactness) -> Self {
        Self {
            normal: class,
            alpha: class,
            rest: class,
        }
    }
}
impl RenderBackend for Claims {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "claims",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, op: Operator) -> Exactness {
        if op == Operator::Blend(StudioBlendMode::Normal) {
            self.normal
        } else {
            self.rest
        }
    }
    fn supports_clip_to_base(&self) -> bool {
        true
    }
    fn extension_exactness(&self, op: ExtOperator) -> Exactness {
        match op {
            ExtOperator::AlphaPremultiplied => self.alpha,
            _ => self.rest,
        }
    }
    /// Like the real backends: shared admission first, then (admitted) one chunk.
    fn execute(
        &self,
        plan: &RenderPlan,
        request: &ExecuteRequest,
        _: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        admit(plan, self, request, cancel)?;
        sink.accept(R, &[0; 24], &[0; 8])
            .map_err(BackendError::Sink)?;
        Err(BackendError::Unavailable("claims_execute"))
    }
}
struct NoSources;
impl PlanSources for NoSources {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}
struct CountingSink(u32);
impl TileSink for CountingSink {
    fn accept(&mut self, _: Rect, _: &[u8], _: &[u8]) -> Result<(), SinkError> {
        self.0 += 1;
        Ok(())
    }
}
fn req(floor: Exactness) -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 3,
        min_exactness: floor,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}
fn refusal(key: &'static str, class: Exactness) -> Result<(), BackendError> {
    Err(BackendError::Unsupported {
        operator: key,
        exactness: class,
    })
}
fn v1(p: &RenderPlan, b: &Claims, floor: Exactness) -> Result<ExactnessLedger, BackendError> {
    admit(p, b, &req(floor), &CancellationToken::default())
}
fn extended(
    p: &RenderPlan,
    ext: &PlanExtensions,
    b: &Claims,
    floor: Exactness,
) -> Result<ExactnessLedger, BackendError> {
    admit_extended(p, ext, b, &req(floor), &CancellationToken::default())
}
fn executes(p: &RenderPlan, b: &Claims) -> (Result<(), BackendError>, u32) {
    let mut sink = CountingSink(0);
    let run = b.execute(
        p,
        &req(Exactness::Approximate),
        &NoSources,
        &CancellationToken::default(),
        &mut sink,
    );
    (run.map(|_| ()), sink.0)
}
fn has(ledger: &ExactnessLedger, key: &str, class: Exactness) -> bool {
    ledger
        .operators
        .iter()
        .any(|o| o.key == key && o.exactness == class)
}
fn all_backends() -> [Claims; 2] {
    [
        Claims::all(Exactness::Exact),
        Claims::all(Exactness::Approximate),
    ]
}

#[test]
fn pass_through_mask_is_refused_for_every_backend() {
    assert_eq!(PASS_THROUGH_MASK_OPERATOR, "mask_on_pass_through");
    let key = "mask_on_pass_through";
    let variants = [
        ("plain u8", mask(MaskFormat::CoverageU8, false, 1.0)),
        ("inverted", mask(MaskFormat::CoverageU8, true, 1.0)),
        ("u16", mask(MaskFormat::CoverageU16Le, false, 1.0)),
    ];
    for backend in all_backends() {
        for (name, m) in variants {
            let p = pass_through_plan(with_mask(m));
            assert!(p.has_pass_through_mask(), "{name}");
            assert_eq!(
                v1(&p, &backend, Exactness::Approximate).map(|_| ()),
                refusal(key, Exactness::Unsupported),
                "{name}: v1 admit"
            );
            assert_eq!(
                extended(&p, &PlanExtensions::default(), &backend, Exactness::Approximate)
                    .map(|_| ()),
                refusal(key, Exactness::Unsupported),
                "{name}: admit_extended"
            );
            // Refused before the backend produces any output.
            assert_eq!(
                executes(&p, &backend),
                (refusal(key, Exactness::Unsupported), 0),
                "{name}: execute"
            );
        }
        // Feather is an envelope field of the same layer: the layer is refused the same way.
        let feathered = PlanExtensions::from_layers(vec![
            LayerExtension::new(PlanNodeId(2), 0).with_mask_feather(2.0),
        ]);
        let p = pass_through_plan(with_mask(mask(MaskFormat::CoverageU8, false, 1.0)));
        assert_eq!(
            extended(&p, &feathered, &backend, Exactness::Approximate).map(|_| ()),
            refusal(key, Exactness::Unsupported)
        );
    }
}

#[test]
fn pass_through_mask_counterfactuals_are_admitted() {
    let backend = Claims::all(Exactness::Exact);
    let m = mask(MaskFormat::CoverageU8, false, 1.0);
    // The same mask on a Normal layer of an isolated group, and a pass-through layer without a
    // mask, are not the combination.
    let normal_masked = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(true, vec![masked(0, StudioBlendMode::Normal, m)]),
    ]);
    assert!(!normal_masked.has_pass_through_mask());
    assert!(v1(&normal_masked, &backend, Exactness::Approximate).is_ok());
    let unmasked = pass_through_plan(|l| l);
    assert!(!unmasked.has_pass_through_mask());
    assert!(v1(&unmasked, &backend, Exactness::Approximate).is_ok());
    assert_eq!(executes(&unmasked, &backend), (refusal_unavailable(), 1));
    // A clipped pass-through layer is a structural error, not this key (and is not counted).
    let clipped = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        group(
            true,
            vec![
                layer(0, StudioBlendMode::Normal),
                masked(
                    1,
                    StudioBlendMode::PassThrough,
                    MaskInput::clip_to_base(),
                ),
            ],
        ),
    ]);
    assert!(!clipped.has_pass_through_mask());
    assert_eq!(
        v1(&clipped, &backend, Exactness::Approximate).map(|_| ()),
        Err(BackendError::InvalidPlan(PlanError::ClipPassThrough {
            node: 2
        }))
    );
}
/// `RenderPlan::isolate_masked_pass_through` (lane SMB-COMPOSITE-VECTOR-MASK-2, pigment DESCRIPTOR
/// `pass_through_mask_isolation`): the W3C Compositing 1 section 3.2 reading of a masked
/// pass-through group. The rewritten plan is exactly the plan that authors the same mask on a
/// Normal layer over the same group made isolated, and every backend admits it where the
/// pass-through original is refused `mask_on_pass_through`. Misuse is `PlanError::OutOfRange`
/// on the named group and leaves the plan untouched; the input group's references are counted
/// over every group of the plan, reachable from the root or not.
#[test]
fn pass_through_mask_isolation() {
    let plain = mask(MaskFormat::CoverageU8, false, 1.0);
    // Inverted with density 0.75 (an envelope operator): the rewrite keeps every mask field.
    let m = mask(MaskFormat::CoverageU16Le, true, 0.75);
    let before = pass_through_plan(with_mask(m));
    let mut p = before.clone();
    assert_eq!(p.isolate_masked_pass_through(PlanNodeId(2), 0), Ok(()));
    // The reading: node 1 isolated, the layer Normal, input and mask unchanged.
    let authored = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(true, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![masked(1, StudioBlendMode::Normal, m)]),
    ]);
    assert_eq!(p, authored);
    assert!(before.has_pass_through_mask() && !p.has_pass_through_mask());
    assert_eq!(PASS_THROUGH_MASK_ISOLATED, "mask_isolates_pass_through");
    let plain_before = pass_through_plan(with_mask(plain));
    let mut q = plain_before.clone();
    q.isolate_masked_pass_through(PlanNodeId(2), 0).unwrap();
    for backend in all_backends() {
        assert_eq!(
            v1(&plain_before, &backend, Exactness::Approximate).map(|_| ()),
            refusal("mask_on_pass_through", Exactness::Unsupported)
        );
        assert!(v1(&q, &backend, Exactness::Approximate).is_ok());
        assert_eq!(
            extended(&before, &PlanExtensions::default(), &backend, Exactness::Approximate)
                .map(|_| ()),
            refusal("mask_on_pass_through", Exactness::Unsupported)
        );
        assert!(
            extended(&p, &PlanExtensions::default(), &backend, Exactness::Approximate).is_ok()
        );
    }

    // Misuse: (what, plan, group, layer); the error names `group`.
    let shared_in_unreachable = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        // Node 2 is not reachable from the root (node 3) but reads node 1 as well.
        group(true, vec![layer(1, StudioBlendMode::Normal)]),
        group(true, vec![masked(1, StudioBlendMode::PassThrough, plain)]),
    ]);
    let shared_in_same_group = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        group(
            true,
            vec![
                masked(1, StudioBlendMode::PassThrough, plain),
                layer(1, StudioBlendMode::PassThrough),
            ],
        ),
    ]);
    let not_pass_through = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![masked(1, StudioBlendMode::Normal, plain)]),
    ]);
    let over_solid = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(true, vec![masked(0, StudioBlendMode::PassThrough, plain)]),
    ]);
    let cases: Vec<(&str, RenderPlan, u32, usize)> = vec![
        ("not a group", before.clone(), 0, 0),
        ("no such node", before.clone(), 99, 0),
        ("no such layer", before.clone(), 2, 1),
        ("not pass-through", not_pass_through, 2, 0),
        ("no mask", pass_through_plan(|l| l), 2, 0),
        (
            "clip-to-base route",
            pass_through_plan(with_mask(MaskInput::clip_to_base())),
            2,
            0,
        ),
        ("input is not a group", over_solid, 1, 0),
        ("input also read by an unreachable group", shared_in_unreachable, 3, 0),
        ("input read twice in the same group", shared_in_same_group, 2, 0),
    ];
    for (what, plan, g, li) in cases {
        let mut rewritten = plan.clone();
        assert_eq!(
            rewritten.isolate_masked_pass_through(PlanNodeId(g), li),
            Err(PlanError::OutOfRange { node: g }),
            "{what}"
        );
        assert_eq!(rewritten, plan, "{what}: the plan is untouched");
    }
    // Counterfactual of the reference count: the same plan without the unreachable reader
    // rewrites.
    let mut alone = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![masked(1, StudioBlendMode::PassThrough, plain)]),
    ]);
    assert_eq!(alone.isolate_masked_pass_through(PlanNodeId(3), 0), Ok(()));
}
fn refusal_unavailable() -> Result<(), BackendError> {
    Err(BackendError::Unavailable("claims_execute"))
}

#[test]
fn earlier_refusals_keep_their_keys_before_the_new_rules() {
    let backend = Claims::all(Exactness::Exact);
    // Density below 1 on a pass-through layer: the v1 path refuses it as `mask_density` first
    // (as before this lane); the envelope path admits density and then refuses the combination.
    let dense = pass_through_plan(with_mask(mask(MaskFormat::CoverageU8, false, 0.5)));
    assert_eq!(
        v1(&dense, &backend, Exactness::Approximate).map(|_| ()),
        refusal("mask_density", Exactness::Unsupported)
    );
    assert_eq!(
        extended(&dense, &PlanExtensions::default(), &backend, Exactness::Approximate)
            .map(|_| ()),
        refusal("mask_on_pass_through", Exactness::Unsupported)
    );
    // Fill below 1 on the masked pass-through layer: the fill refusal is the older one.
    let filled = pass_through_plan(|l| Layer {
        fill_opacity: 0.5,
        mask: Some(mask(MaskFormat::CoverageU8, false, 1.0)),
        ..l
    });
    assert_eq!(
        v1(&filled, &backend, Exactness::Approximate).map(|_| ()),
        refusal(PASS_THROUGH_FILL_OPERATOR, Exactness::Unsupported)
    );
    // Colour outside [0, 1] on a premultiplied leaf root: the sample-range refusal is older.
    let hot = plan_with(
        AlphaConvention::Premultiplied,
        vec![solid([2.0, 0.0, 0.0, 1.0])],
    );
    let no_alpha = Claims {
        alpha: Exactness::Unsupported,
        ..Claims::all(Exactness::Exact)
    };
    assert_eq!(
        v1(&hot, &no_alpha, Exactness::Approximate).map(|_| ()),
        refusal(SAMPLE_RANGE_OPERATOR, Exactness::Unsupported)
    );
    // The new rules among themselves: mask, then alpha, then the leaf-root class.
    let leaf_premultiplied = plan_with(
        AlphaConvention::Premultiplied,
        vec![solid([0.2, 0.4, 0.6, 1.0])],
    );
    let neither = Claims {
        normal: Exactness::Unsupported,
        alpha: Exactness::Unsupported,
        rest: Exactness::Exact,
    };
    assert_eq!(
        v1(&leaf_premultiplied, &neither, Exactness::Approximate).map(|_| ()),
        refusal("alpha_premultiplied", Exactness::Unsupported)
    );
}

#[test]
fn premultiplied_alpha_is_a_declared_capability() {
    assert_eq!(
        ExtOperator::AlphaPremultiplied.key(),
        "alpha_premultiplied"
    );
    assert_eq!(
        ExtOperator::AlphaPremultiplied.intrinsic(),
        ALPHA_BOUNDARY_EXACTNESS
    );
    assert_eq!(ALPHA_BOUNDARY_EXACTNESS, Exactness::Approximate);

    // Nodes: 0 premultiplied tile | 1 root group {0}. And the same with a premultiplied target.
    let premultiplied_tile = plan(vec![
        tile(AlphaConvention::Premultiplied),
        group(true, vec![layer(0, StudioBlendMode::Normal)]),
    ]);
    let premultiplied_target = plan_with(
        AlphaConvention::Premultiplied,
        vec![
            solid([0.2, 0.4, 0.6, 1.0]),
            group(true, vec![layer(0, StudioBlendMode::Normal)]),
        ],
    );
    let straight = plan(vec![
        tile(AlphaConvention::Straight),
        group(true, vec![layer(0, StudioBlendMode::Normal)]),
    ]);
    assert!(premultiplied_tile.uses_premultiplied_alpha());
    assert!(premultiplied_target.uses_premultiplied_alpha());
    assert!(!straight.uses_premultiplied_alpha());

    // Undeclared (the render-cpu and render-gpu shape): refused on both admission paths, even
    // though every other operator is claimed Exact.
    let undeclared = Claims {
        alpha: Exactness::Unsupported,
        ..Claims::all(Exactness::Exact)
    };
    for p in [&premultiplied_tile, &premultiplied_target] {
        assert_eq!(
            v1(p, &undeclared, Exactness::Approximate).map(|_| ()),
            refusal("alpha_premultiplied", Exactness::Unsupported)
        );
        assert_eq!(
            extended(p, &PlanExtensions::default(), &undeclared, Exactness::Approximate)
                .map(|_| ()),
            refusal("alpha_premultiplied", Exactness::Unsupported)
        );
        assert_eq!(
            executes(p, &undeclared),
            (refusal("alpha_premultiplied", Exactness::Unsupported), 0)
        );
    }
    // A tile that no layer reaches is never read by a backend (both skip unreachable nodes before
    // verify_tile), so it declares nothing: admission follows the backends, reachable nodes only
    // (SMB-PARITY-FOLLOWUP; the premultiplied target above is always executed and still counts).
    let unreachable_tile = plan(vec![
        tile(AlphaConvention::Premultiplied),
        solid([0.2, 0.4, 0.6, 1.0]),
        group(true, vec![layer(1, StudioBlendMode::Normal)]),
    ]);
    assert!(!unreachable_tile.uses_premultiplied_alpha());
    assert!(v1(&unreachable_tile, &undeclared, Exactness::Approximate).is_ok());
    // Counterfactual: a straight plan does not ask for the capability at all.
    let ledger = v1(&straight, &undeclared, Exactness::Approximate).unwrap();
    assert!(ledger.operators.iter().all(|o| o.key != "alpha_premultiplied"));

    // Declared by a backend that converts: admitted at pigment's class, never a better one.
    for claim in [Exactness::Exact, Exactness::Fitted, Exactness::Approximate] {
        let converting = Claims {
            alpha: claim,
            ..Claims::all(Exactness::Exact)
        };
        let ledger = v1(&premultiplied_tile, &converting, Exactness::Approximate).unwrap();
        assert!(has(&ledger, "alpha_premultiplied", Exactness::Approximate));
        assert_eq!(ledger.lowest, Exactness::Approximate);
        let ledger = extended(
            &premultiplied_target,
            &PlanExtensions::default(),
            &converting,
            Exactness::Approximate,
        )
        .unwrap();
        assert!(has(&ledger, "alpha_premultiplied", Exactness::Approximate));
        // A request that demands better than Approximate refuses the plan with that class.
        assert_eq!(
            v1(&premultiplied_tile, &converting, Exactness::Fitted).map(|_| ()),
            refusal("alpha_premultiplied", Exactness::Approximate)
        );
    }

    // The envelope validator describes the envelope: the convention is not one of its operators.
    assert_eq!(
        PlanExtensions::default().validate(&premultiplied_tile),
        Ok(Vec::new())
    );
}

#[test]
fn leaf_root_normal_class_is_decided_at_admission() {
    // A leaf root lists no layer, but both backends composite it as a Normal layer over the
    // transparent backdrop, so the backend's class for Blend(Normal) applies to it.
    let solid_leaf = plan(vec![solid([0.3, 0.5, 0.7, 0.25])]);
    let tile_leaf = plan(vec![tile(AlphaConvention::Straight)]);
    assert!(solid_leaf.has_leaf_root());
    assert!(tile_leaf.has_leaf_root());
    let fitted = Claims {
        normal: Exactness::Fitted,
        ..Claims::all(Exactness::Exact)
    };
    for p in [&solid_leaf, &tile_leaf] {
        // The render-gpu shape (Normal is Fitted): an Exact floor is refused at admission, a
        // Fitted floor is admitted, on both paths.
        assert_eq!(
            v1(p, &fitted, Exactness::Exact).map(|_| ()),
            refusal("normal", Exactness::Fitted)
        );
        assert_eq!(
            extended(p, &PlanExtensions::default(), &fitted, Exactness::Exact).map(|_| ()),
            refusal("normal", Exactness::Fitted)
        );
        assert!(v1(p, &fitted, Exactness::Fitted).is_ok());
        // Check only: the receipt ledger is the one that existed before this rule.
        let ledger = v1(p, &fitted, Exactness::Fitted).unwrap();
        assert!(ledger.operators.iter().all(|o| o.key != "normal"));
        // A backend that does not evaluate Normal is refused with the Unsupported class.
        let none = Claims {
            normal: Exactness::Unsupported,
            ..Claims::all(Exactness::Exact)
        };
        assert_eq!(
            v1(p, &none, Exactness::Approximate).map(|_| ()),
            refusal("normal", Exactness::Unsupported)
        );
        // The render-cpu shape (Normal is Exact) is unaffected.
        assert!(v1(p, &Claims::all(Exactness::Exact), Exactness::Exact).is_ok());
    }
    // Counterfactual: a group root with a Multiply layer uses no Normal blend, so a backend that
    // cannot evaluate Normal still admits it.
    let group_root = plan(vec![
        solid([0.3, 0.5, 0.7, 1.0]),
        group(true, vec![layer(0, StudioBlendMode::Multiply)]),
    ]);
    assert!(!group_root.has_leaf_root());
    let none = Claims {
        normal: Exactness::Unsupported,
        ..Claims::all(Exactness::Exact)
    };
    assert!(v1(&group_root, &none, Exactness::Approximate).is_ok());
}

/// Lane SMB-PARITY-FOLLOWUP (R8-review-a CONFIRMED): both backends execute only the nodes
/// reachable from the root (render-cpu and render-gpu `prepare`: `reach`, unreachable nodes are
/// skipped before `verify_tile` and before any layer check), so the two predicates that read
/// nodes decide on the same set. Before, a masked pass-through layer or a premultiplied tile that
/// only an unreachable node held was refused here although the backends executed the plan.
#[test]
fn unreachable_nodes_are_not_counted_like_the_backends() {
    let m = mask(MaskFormat::CoverageU8, false, 1.0);
    let undeclared = Claims {
        alpha: Exactness::Unsupported,
        ..Claims::all(Exactness::Exact)
    };
    let pt_key = "mask_on_pass_through";
    let alpha_key = "alpha_premultiplied";

    // Nodes: 0 solid | 1 non-isolated group {0} | 2 isolated group {pass-through over 1, masked}
    // | 3 solid | 4 root {Normal over 3}. Node 2 is referenced by nobody: unreachable.
    let unreachable_masked = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![masked(1, StudioBlendMode::PassThrough, m)]),
        solid([0.1, 0.2, 0.3, 1.0]),
        group(true, vec![layer(3, StudioBlendMode::Normal)]),
    ]);
    assert!(unreachable_masked.validate(&PlanLimits::default()).is_ok());
    assert!(!unreachable_masked.has_pass_through_mask());
    for backend in all_backends() {
        assert!(v1(&unreachable_masked, &backend, Exactness::Approximate).is_ok());
        assert!(
            extended(
                &unreachable_masked,
                &PlanExtensions::default(),
                &backend,
                Exactness::Approximate
            )
            .is_ok()
        );
        // Admitted, so the backend double runs (one chunk), like the real backends do.
        assert_eq!(
            executes(&unreachable_masked, &backend),
            (refusal_unavailable(), 1)
        );
    }

    // Counterfactuals: the same masked layer is refused when the root reaches it, directly and
    // through a chain of groups.
    // direct nodes:  0 solid | 1 non-isolated {0} | 2 isolated {masked pass-through over 1} |
    //                3 root {Normal over 2}.
    // chained nodes: the same, then 3 isolated {Normal over 2} | 4 root {Normal over 3}.
    let direct = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![masked(1, StudioBlendMode::PassThrough, m)]),
        group(true, vec![layer(2, StudioBlendMode::Normal)]),
    ]);
    let chained = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![masked(1, StudioBlendMode::PassThrough, m)]),
        group(true, vec![layer(2, StudioBlendMode::Normal)]),
        group(true, vec![layer(3, StudioBlendMode::Normal)]),
    ]);
    for (what, p) in [("direct", &direct), ("chained", &chained)] {
        assert!(p.has_pass_through_mask(), "{what}");
        for backend in all_backends() {
            assert_eq!(
                v1(p, &backend, Exactness::Approximate).map(|_| ()),
                refusal(pt_key, Exactness::Unsupported),
                "{what}"
            );
            assert_eq!(
                executes(p, &backend),
                (refusal(pt_key, Exactness::Unsupported), 0),
                "{what}"
            );
        }
    }

    // A premultiplied tile that no layer reaches declares nothing; a reachable one (directly or
    // through a chain of groups) does; so does the target, which is always executed.
    // Nodes: 0 premultiplied tile | 1 straight tile | 2 root {Normal over 1}.
    let unreachable_tile = plan(vec![
        tile(AlphaConvention::Premultiplied),
        tile(AlphaConvention::Straight),
        group(true, vec![layer(1, StudioBlendMode::Normal)]),
    ]);
    assert!(!unreachable_tile.uses_premultiplied_alpha());
    assert!(v1(&unreachable_tile, &undeclared, Exactness::Approximate).is_ok());
    assert!(
        extended(
            &unreachable_tile,
            &PlanExtensions::default(),
            &undeclared,
            Exactness::Approximate
        )
        .is_ok()
    );
    assert_eq!(
        executes(&unreachable_tile, &undeclared),
        (refusal_unavailable(), 1)
    );
    // Nodes: 0 premultiplied tile | 1 isolated {Normal over 0} | 2 root {Normal over 1}.
    let chained_tile = plan(vec![
        tile(AlphaConvention::Premultiplied),
        group(true, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![layer(1, StudioBlendMode::Normal)]),
    ]);
    let target_only = plan_with(
        AlphaConvention::Premultiplied,
        vec![
            tile(AlphaConvention::Straight),
            group(true, vec![layer(0, StudioBlendMode::Normal)]),
        ],
    );
    for (what, p) in [("chained tile", &chained_tile), ("target", &target_only)] {
        assert!(p.uses_premultiplied_alpha(), "{what}");
        assert_eq!(
            v1(p, &undeclared, Exactness::Approximate).map(|_| ()),
            refusal(alpha_key, Exactness::Unsupported),
            "{what}"
        );
        assert_eq!(
            executes(p, &undeclared),
            (refusal(alpha_key, Exactness::Unsupported), 0),
            "{what}"
        );
    }

    // Hostile indexes never panic the predicates: a root outside the plan reaches nothing
    // (validate refuses the plan first), a layer input outside the plan is ignored.
    let mut bad_root = plan(vec![
        tile(AlphaConvention::Premultiplied),
        group(true, vec![layer(0, StudioBlendMode::Normal)]),
    ]);
    bad_root.root = PlanNodeId(7);
    assert!(!bad_root.has_pass_through_mask() && !bad_root.uses_premultiplied_alpha());
    assert_eq!(
        v1(&bad_root, &undeclared, Exactness::Approximate).map(|_| ()),
        Err(BackendError::InvalidPlan(PlanError::RootOutOfRange))
    );
    let bad_input = plan(vec![
        tile(AlphaConvention::Premultiplied),
        group(true, vec![layer(9, StudioBlendMode::Normal)]),
    ]);
    assert!(!bad_input.has_pass_through_mask() && !bad_input.uses_premultiplied_alpha());
}

/// Lane SMB-PARITY-FOLLOWUP, lead decision "one rule, no exceptions": the older shared refusals
/// read the same set the backends execute. render-cpu and render-gpu check the pass-through fill,
/// the Solid colour range and the mask density inside their reachable-node loops (`if !reach[i]`),
/// so a node no layer reaches asks for nothing. Each test: the unreachable node admits (v1,
/// extended, execute), the reached node is refused with the same key as before.
#[test]
fn unreachable_pass_through_fill_is_not_counted_like_the_backends() {
    let filled = |l: Layer| Layer {
        fill_opacity: 0.5,
        ..l
    };
    let key = PASS_THROUGH_FILL_OPERATOR;
    // Nodes: 0 solid | 1 non-isolated {0} | 2 isolated {pass-through over 1, fill 0.5}
    // | 3 solid | 4 root {Normal over 3}. Node 2 is referenced by nobody.
    let unreachable = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![filled(layer(1, StudioBlendMode::PassThrough))]),
        solid([0.1, 0.2, 0.3, 1.0]),
        group(true, vec![layer(3, StudioBlendMode::Normal)]),
    ]);
    assert!(unreachable.validate(&PlanLimits::default()).is_ok());
    assert!(!unreachable.has_pass_through_fill());
    for backend in all_backends() {
        assert!(v1(&unreachable, &backend, Exactness::Approximate).is_ok());
        assert!(
            extended(
                &unreachable,
                &PlanExtensions::default(),
                &backend,
                Exactness::Approximate
            )
            .is_ok()
        );
        assert_eq!(executes(&unreachable, &backend), (refusal_unavailable(), 1));
    }
    // Reached by the root directly, and through a chain of groups: refused as before.
    // direct: 0 solid | 1 non-isolated {0} | 2 root {pass-through over 1, fill 0.5}.
    let direct = pass_through_plan(filled);
    let chained = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(false, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![filled(layer(1, StudioBlendMode::PassThrough))]),
        group(true, vec![layer(2, StudioBlendMode::Normal)]),
        group(true, vec![layer(3, StudioBlendMode::Normal)]),
    ]);
    for (what, p) in [("direct", &direct), ("chained", &chained)] {
        assert!(p.has_pass_through_fill(), "{what}");
        for backend in all_backends() {
            assert_eq!(
                v1(p, &backend, Exactness::Approximate).map(|_| ()),
                refusal(key, Exactness::Unsupported),
                "{what}"
            );
            assert_eq!(
                executes(p, &backend),
                (refusal(key, Exactness::Unsupported), 0),
                "{what}"
            );
        }
    }
}

#[test]
fn unreachable_solid_outside_unit_is_not_counted_like_the_backends() {
    let key = SAMPLE_RANGE_OPERATOR;
    let hdr = [2.0, 0.5, 0.5, 1.0];
    // Nodes: 0 solid outside [0, 1] (referenced by nobody) | 1 solid | 2 root {Normal over 1}.
    let unreachable = plan(vec![
        solid(hdr),
        solid([0.2, 0.4, 0.6, 1.0]),
        group(true, vec![layer(1, StudioBlendMode::Normal)]),
    ]);
    assert!(unreachable.validate(&PlanLimits::default()).is_ok());
    assert!(!unreachable.has_solid_outside_unit());
    for backend in all_backends() {
        assert!(v1(&unreachable, &backend, Exactness::Approximate).is_ok());
        assert!(
            extended(
                &unreachable,
                &PlanExtensions::default(),
                &backend,
                Exactness::Approximate
            )
            .is_ok()
        );
        assert_eq!(executes(&unreachable, &backend), (refusal_unavailable(), 1));
    }
    // Reached through a layer, through a chain of groups, and as the leaf root itself.
    let direct = plan(vec![
        solid(hdr),
        group(true, vec![layer(0, StudioBlendMode::Normal)]),
    ]);
    let chained = plan(vec![
        solid(hdr),
        group(true, vec![layer(0, StudioBlendMode::Normal)]),
        group(true, vec![layer(1, StudioBlendMode::Normal)]),
    ]);
    let leaf_root = plan(vec![solid(hdr)]);
    for (what, p) in [
        ("direct", &direct),
        ("chained", &chained),
        ("leaf root", &leaf_root),
    ] {
        assert!(p.has_solid_outside_unit(), "{what}");
        for backend in all_backends() {
            assert_eq!(
                v1(p, &backend, Exactness::Approximate).map(|_| ()),
                refusal(key, Exactness::Unsupported),
                "{what}"
            );
            assert_eq!(
                executes(p, &backend),
                (refusal(key, Exactness::Unsupported), 0),
                "{what}"
            );
        }
    }
}

#[test]
fn unreachable_mask_density_is_not_counted_like_the_backends() {
    let key = MASK_DENSITY_OPERATOR;
    let half = mask(MaskFormat::CoverageU8, false, 0.5);
    // Nodes: 0 solid | 1 isolated {Normal over 0, mask density 0.5} (referenced by nobody)
    // | 2 solid | 3 root {Normal over 2}.
    let unreachable = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(true, vec![masked(0, StudioBlendMode::Normal, half)]),
        solid([0.1, 0.2, 0.3, 1.0]),
        group(true, vec![layer(2, StudioBlendMode::Normal)]),
    ]);
    assert!(unreachable.validate(&PlanLimits::default()).is_ok());
    assert!(!unreachable.has_mask_density());
    for backend in all_backends() {
        assert!(v1(&unreachable, &backend, Exactness::Approximate).is_ok());
        // The envelope path does not even list the operator: no reached layer uses it.
        let ledger = extended(
            &unreachable,
            &PlanExtensions::default(),
            &backend,
            Exactness::Approximate,
        )
        .unwrap();
        assert!(ledger.operators.iter().all(|o| o.key != key));
        assert_eq!(executes(&unreachable, &backend), (refusal_unavailable(), 1));
    }
    // Reached directly and through a chain of groups: the v1 path refuses it as before, the
    // envelope path admits it as the MaskDensity operator.
    let direct = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(true, vec![masked(0, StudioBlendMode::Normal, half)]),
    ]);
    let chained = plan(vec![
        solid([0.2, 0.4, 0.6, 1.0]),
        group(true, vec![masked(0, StudioBlendMode::Normal, half)]),
        group(true, vec![layer(1, StudioBlendMode::Normal)]),
    ]);
    for (what, p) in [("direct", &direct), ("chained", &chained)] {
        assert!(p.has_mask_density(), "{what}");
        for backend in all_backends() {
            assert_eq!(
                v1(p, &backend, Exactness::Approximate).map(|_| ()),
                refusal(key, Exactness::Unsupported),
                "{what}"
            );
            let ledger = extended(
                p,
                &PlanExtensions::default(),
                &backend,
                Exactness::Approximate,
            )
            .unwrap();
            assert!(ledger.operators.iter().any(|o| o.key == key), "{what}");
        }
    }
}

/// Evidence for the GPU's `effect_size` refusal being unreachable after admission: the widest
/// effect blur pigment admits (`MAX_EFFECT_SIZE_PX`, an effect's blur is at most its size) needs
/// 3001 taps, and render-gpu `fx.rs` `MAX_TAPS` is 3004. If pigment raises the limit, this test
/// fails and render-gpu's bound must follow.
#[test]
fn widest_admitted_effect_blur_kernel_fits_the_gpu_tap_bound() {
    let taps = feather_kernel(MAX_EFFECT_SIZE_PX).unwrap().len();
    assert_eq!(taps, 3001);
    assert!(taps <= 3004);
}
