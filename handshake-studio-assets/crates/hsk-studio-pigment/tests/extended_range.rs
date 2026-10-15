//! Extended sample range and the plan output stage (lane SMB-PIGMENT-EXTENDED-RANGE).
//!
//! Spec basis: STU-COL-120 (32-bit is "floating point, linear-light, unbounded"; reduced tool
//! availability "MUST be reported per operation"), STU-COL-161 (stage 4 blending and compositing,
//! stage 6a display transform). The reference formula is W3C Compositing 1 s5.1 / s9.1.4 source-over
//! with blend, evaluated here independently in binary64 with the mode functions written out.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{
    BlendError, PixelSite, Rgba, composite_over_extended, extended_defined,
};
use hsk_studio_pigment::render_plan::*;

const R: Rect = Rect { x: 0, y: 0, width: 2, height: 1 };
const SITE: PixelSite = PixelSite { x: 0, y: 0, seed: 0 };

/// Every mode the rule admits, with its binary64 reference `B(b, s)` per channel (None for the
/// whole-pixel modes, checked by their own rule below).
fn reference(mode: StudioBlendMode) -> Option<fn(f64, f64) -> f64> {
    use StudioBlendMode::*;
    Some(match mode {
        Normal | Dissolve => |_, s| s,
        Multiply => |b, s| b * s,
        Darken => |b: f64, s: f64| b.min(s),
        Lighten => |b: f64, s: f64| b.max(s),
        Difference => |b: f64, s: f64| (b - s).abs(),
        Subtract => |b: f64, s: f64| (b - s).max(0.0),
        LinearDodgeAdd => |b, s| b + s,
        _ => return None,
    })
}
/// Source-over with blend in binary64 (straight alpha), no clamp.
fn spec_over(blended: [f64; 3], b: [f64; 4], s: [f64; 4], opacity: f64) -> [f64; 4] {
    let a_s = s[3] * opacity;
    let a_b = b[3];
    let a_o = a_s + a_b * (1.0 - a_s);
    let mut out = [0.0; 4];
    for i in 0..3 {
        let cs_prime = (1.0 - a_b) * s[i] + a_b * blended[i];
        out[i] = (a_s * cs_prime + (1.0 - a_s) * a_b * b[i]) / a_o;
    }
    out[3] = a_o;
    out
}

/// `composite_over_extended` equals the W3C formula without the unit clamp for every extended-defined
/// mode on samples far above 1 (relative error <= 4 ulp of binary32), with a hand value; the rule
/// admits exactly ten modes and refuses every other layer mode as `Unsupported(mode)`; pigment
/// admission refuses an extended plan with such a mode as `sample_range_extended_mode` and an
/// extended plan with an envelope layer entry as `sample_range_extended_combination`, for every
/// backend, while a `Solid` above 1 is refused by the unit default and admitted by the extended one.
#[test]
fn extended_over_matches_the_w3c_formula_and_undefined_modes_are_refused() {
    // Hand value: Normal, source 3.0 at alpha 0.5 over opaque 0.5 -> 0.5 * 3 + 0.5 * 0.5 = 1.75.
    let out = composite_over_extended(
        StudioBlendMode::Normal,
        Rgba::new([0.5; 3], 1.0),
        Rgba::new([3.0; 3], 0.5),
        1.0,
        SITE,
    )
    .unwrap();
    assert_eq!(out.rgb, [1.75; 3]);
    assert_eq!(out.a, 1.0);

    let samples: [([f32; 4], [f32; 4], f32); 4] = [
        ([2.5, 0.25, 7.0, 1.0], [4.0, 1.5, 0.5, 0.6], 0.8),
        ([0.0, 12.0, 1.0, 0.3], [9.5, 0.0, 3.25, 1.0], 1.0),
        ([1.0, 1.0, 1.0, 0.0], [6.0, 2.0, 0.1, 0.7], 0.5),
        ([-0.25, 3.0, 64.0, 0.9], [0.75, -0.5, 100.0, 0.4], 0.9),
    ];
    let mut admitted = 0;
    for facts in &MODES {
        let mode = facts.mode;
        if !extended_defined(mode) {
            if facts.applicability == Applicability::Layer {
                assert_eq!(
                    composite_over_extended(mode, Rgba::new([2.0; 3], 1.0), Rgba::new([2.0; 3], 1.0), 1.0, SITE),
                    Err(BlendError::Unsupported(mode)),
                    "{}",
                    facts.key
                );
            }
            continue;
        }
        admitted += 1;
        for (b, s, opacity) in samples {
            let got = composite_over_extended(
                mode,
                Rgba::new([b[0], b[1], b[2]], b[3]),
                Rgba::new([s[0], s[1], s[2]], s[3]),
                opacity,
                SITE,
            )
            .unwrap();
            let (b64, s64) = (b.map(f64::from), s.map(f64::from));
            let blended = match reference(mode) {
                Some(f) => [f(b64[0], s64[0]), f(b64[1], s64[1]), f(b64[2], s64[2])],
                None => {
                    // Darker / lighter colour: the whole source or backdrop by channel sum.
                    let (sum_s, sum_b) = (s64[..3].iter().sum::<f64>(), b64[..3].iter().sum::<f64>());
                    let source = if mode == StudioBlendMode::DarkerColour { sum_s < sum_b } else { sum_s > sum_b };
                    let c = if source { s64 } else { b64 };
                    [c[0], c[1], c[2]]
                }
            };
            let want = spec_over(blended, b64, s64, f64::from(opacity));
            if mode == StudioBlendMode::Dissolve {
                continue; // stochastic alpha; its colour rule is Normal's, checked above
            }
            for i in 0..3 {
                let tolerance = 4.0 * f64::from(f32::EPSILON) * want[i].abs().max(1.0);
                assert!(
                    (f64::from(got.rgb[i]) - want[i]).abs() <= tolerance,
                    "{} {b:?} {s:?}: {:?} vs {want:?}",
                    facts.key,
                    got.rgb
                );
            }
            assert!((f64::from(got.a) - want[3]).abs() <= 4.0 * f64::from(f32::EPSILON));
        }
    }
    assert_eq!(admitted, 10);

    // Plan-level: a Solid above 1 under a Screen layer.
    let plan_with = |mode: StudioBlendMode| RenderPlan::<NoVector> {
        revision: 1,
        target: OutputTarget {
            extent: R,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight },
        },
        nodes: vec![
            PlanNode::Solid { rect: R, rgba: [0.2, 0.4, 0.6, 1.0] },
            PlanNode::Solid { rect: R, rgba: [3.0, 0.5, 2.0, 1.0] },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    Layer { input: PlanNodeId(0), mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 1.0, mask: None },
                    Layer { input: PlanNodeId(1), mode, opacity: 1.0, fill_opacity: 1.0, mask: None },
                ],
            }),
        ],
        root: PlanNodeId(2),
    };
    let extended = PlanExtensions::default().with_output(OutputStage::new(SampleRange::Extended, None));
    let request = ExecuteRequest {
        expected_revision: 1,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 20,
    };
    let cancel = CancellationToken::default();
    let backend = Fake;
    let unit = admit_extended(&plan_with(StudioBlendMode::Multiply), &PlanExtensions::default(), &backend, &request, &cancel);
    assert_eq!(
        unit.unwrap_err(),
        BackendError::Unsupported { operator: SAMPLE_RANGE_OPERATOR, exactness: Exactness::Unsupported }
    );
    let ledger = admit_extended(&plan_with(StudioBlendMode::Multiply), &extended, &backend, &request, &cancel).unwrap();
    assert!(ledger.operators.iter().any(|o| o.key == EXTENDED_RANGE_OPERATOR && o.exactness == Exactness::Approximate));
    assert_eq!(
        admit_extended(&plan_with(StudioBlendMode::Screen), &extended, &backend, &request, &cancel).unwrap_err(),
        BackendError::Unsupported { operator: EXTENDED_RANGE_MODE_OPERATOR, exactness: Exactness::Unsupported }
    );
    let feathered = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1)])
        .with_output(OutputStage::new(SampleRange::Extended, None));
    assert_eq!(
        admit_extended(&plan_with(StudioBlendMode::Multiply), &feathered, &backend, &request, &cancel).unwrap_err(),
        BackendError::Unsupported { operator: EXTENDED_RANGE_COMBINATION_OPERATOR, exactness: Exactness::Unsupported }
    );
    // A backend written before the output stage existed declares nothing: refused, never unclamped.
    assert_eq!(
        admit_extended(&plan_with(StudioBlendMode::Multiply), &extended, &Legacy, &request, &cancel).unwrap_err(),
        BackendError::Unsupported { operator: EXTENDED_RANGE_OPERATOR, exactness: Exactness::Unsupported }
    );
}

/// The output stage is part of the digest: the default stage leaves `digest_with` equal to the
/// plain plan's digest (empty envelope) and to the serialisation without an `output` key (non-empty
/// envelope), so every existing digest is unchanged; each non-default field changes it.
#[test]
fn plan_digest_covers_the_output_stage_and_the_default_is_absent() {
    let plan = RenderPlan::<NoVector> {
        revision: 1,
        target: OutputTarget {
            extent: R,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight },
        },
        nodes: vec![PlanNode::Solid { rect: R, rgba: [0.2, 0.4, 0.6, 1.0] }],
        root: PlanNodeId(0),
    };
    let plain = plan.digest().unwrap();
    let default = PlanExtensions::default();
    assert!(default.is_empty());
    assert_eq!(default.digest_with(&plan).unwrap(), plain);
    assert_eq!(
        PlanExtensions::default().with_output(OutputStage::default()).digest_with(&plan).unwrap(),
        plain
    );
    let layered = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(0), 0).with_mask_feather(1.0)]);
    let json = serde_json::to_string(&layered).unwrap();
    assert!(!json.contains("output"), "{json}");
    // The envelope as serialised before the output stage existed: `{"layers": [...]}`.
    #[derive(serde::Serialize)]
    struct PreStage<'a> {
        layers: &'a [LayerExtension],
    }
    let legacy = serde_json::to_vec(&(&plan, PreStage { layers: &layered.layers })).unwrap();
    assert_eq!(layered.digest_with(&plan).unwrap(), hsk_studio_prism::profile_hash(&legacy));

    let stages = [
        OutputStage::new(SampleRange::Extended, None),
        OutputStage::new(SampleRange::Unit, Some(DisplayTransform::Clip)),
        OutputStage::new(SampleRange::Extended, Some(DisplayTransform::Clip)),
    ];
    let mut digests = vec![plain, layered.digest_with(&plan).unwrap()];
    for stage in stages {
        let env = PlanExtensions::default().with_output(stage);
        assert!(!env.is_empty());
        digests.push(env.digest_with(&plan).unwrap());
        digests.push(layered.clone().with_output(stage).digest_with(&plan).unwrap());
    }
    for (i, a) in digests.iter().enumerate() {
        for b in &digests[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

struct Fake;
impl RenderBackend for Fake {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity { name: "fake", version: "0", device: DeviceClass::Cpu, adapter: String::new(), generation: 0 }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn extension_exactness(&self, op: ExtOperator) -> Exactness {
        op.intrinsic()
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("fake_execute"))
    }
}
/// A backend written before the output stage (all trait defaults).
struct Legacy;
impl RenderBackend for Legacy {
    fn identity(&self) -> BackendIdentity {
        Fake.identity()
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
        Err(BackendError::Unavailable("fake_execute"))
    }
}
