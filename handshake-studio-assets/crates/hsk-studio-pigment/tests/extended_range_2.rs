//! Extended range, second lane (SMB-PIGMENT-EXTENDED-RANGE-2): the remaining blend modes and the
//! envelope operations on scene-referred data. Spec basis: STU-COL-120, STU-FX-006a, STU-RAS-155 (b).
//! Screen and exclusion follow Foundry Nuke's Merge operations (`A or B <= 1 ? A+B-AB : max(A,B)`,
//! `A+B-2AB`; learn.foundry.com merge_operations.html, Nuke 17.1v2).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::adjust_math::{AdjustmentKind, AdjustmentOp, Exposure, composite_adjustment, composite_adjustment_in};
use hsk_studio_pigment::blend_math::{
    BlendError, PixelSite, Rgba, blend_rgb, blend_rgb_extended, composite_over_extended, extended_defined,
    extended_mode_key,
};
use hsk_studio_pigment::blur_math::{BlurLimits, BlurScope, LayerBlur, blur_window, blur_window_in};
use hsk_studio_pigment::content_aware::{ColourAdaptation, ContentAwareOptions, SamplingArea, fill_in};
use hsk_studio_pigment::coverage_math::{BlendChannel, BlendRangeEntry};
use hsk_studio_pigment::filter_math::{FilterKind, FilterLimits, LayerFilter, NoiseDistribution, filter_window, filter_window_in};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::retouch::{RetouchLimits, RgbaPlane};
use hsk_studio_pigment::selection::CoveragePlane;

const R: Rect = Rect { x: 0, y: 0, width: 2, height: 1 };
const SITE: PixelSite = PixelSite { x: 0, y: 0, seed: 0 };
const WIDE: FilterLimits = FilterLimits { max_bytes: 1 << 30, max_work: 1 << 40 };
const BWIDE: BlurLimits = BlurLimits { max_bytes: 1 << 30, max_work: 1 << 40 };

fn exposure(stops: f32, offset: f32, gamma: f32, linearize: f32) -> AdjustmentOp {
    AdjustmentOp::Exposure(Exposure { exposure: stops, offset, gamma, linearize_gamma: linearize })
}
fn plan_with(mode: StudioBlendMode) -> RenderPlan<NoVector> {
    let layer = |input: u32, mode| Layer { input: PlanNodeId(input), mode, opacity: 1.0, fill_opacity: 1.0, mask: None };
    RenderPlan {
        revision: 1,
        target: OutputTarget {
            extent: R,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight },
        },
        nodes: vec![
            PlanNode::Solid { rect: R, rgba: [2.0, 0.4, 0.6, 1.0] },
            PlanNode::Solid { rect: R, rgba: [0.0; 4] },
            PlanNode::Group(Group { isolated: true, layers: vec![layer(0, StudioBlendMode::Normal), layer(1, mode)] }),
        ],
        root: PlanNodeId(2),
    }
}
fn admit_entry(entry: LayerExtension, mode: StudioBlendMode) -> Result<ExactnessLedger, BackendError> {
    let env = PlanExtensions::from_layers(vec![entry]).with_output(OutputStage::new(SampleRange::Extended, None));
    let request = ExecuteRequest { expected_revision: 1, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 20 };
    admit_extended(&plan_with(mode), &env, &Fake, &request, &CancellationToken::default())
}
fn refused(key: &'static str) -> BackendError {
    BackendError::Unsupported { operator: key, exactness: Exactness::Unsupported }
}

/// Modes: screen and exclusion follow the Nuke formulas above 1 (hand values: screen(2, 3) = 3,
/// screen(0.5, 3) = 2, exclusion(2, 3) = -7) and equal pigment's unit formulas on [0, 1]; twelve
/// modes are defined; every other layer mode is refused under its own name. Envelope: exposure on
/// linear values is admitted (approximate); exposure on encoded values, levels, invert, Blend-If,
/// high pass, add noise and content-aware colour adaptation are refused under their own key for
/// every backend, each citing STU-COL-120 and STU-FX-006a; gaussian blur, unsharp mask, median,
/// effects, blur, transform and content-aware without adaptation are defined.
#[test]
fn extended_modes_and_envelope_operations_follow_the_scene_referred_rule() {
    use StudioBlendMode::*;
    assert_eq!(blend_rgb_extended(Screen, [2.0, 0.5, 0.25], [3.0, 3.0, 0.5]).unwrap(), [3.0, 2.0, 0.625]);
    assert_eq!(blend_rgb_extended(Exclusion, [2.0, 0.5, 0.0], [3.0, 0.5, 4.0]).unwrap(), [-7.0, 0.5, 4.0]);
    for b in [0.0f32, 0.1, 0.5, 0.9, 1.0] {
        for s in [0.0f32, 0.3, 0.7, 1.0] {
            for mode in [Screen, Exclusion] {
                assert_eq!(blend_rgb_extended(mode, [b; 3], [s; 3]).unwrap(), blend_rgb(mode, [b; 3], [s; 3]).unwrap());
            }
        }
    }
    let mut defined = 0;
    for facts in &MODES {
        if facts.applicability != Applicability::Layer {
            continue;
        }
        let key = extended_mode_key(facts.mode);
        assert_eq!(key, format!("sample_range_extended_mode_{}", facts.key));
        if extended_defined(facts.mode) {
            defined += 1;
            assert!(admit_entry(LayerExtension::new(PlanNodeId(2), 1), facts.mode).is_ok() || facts.mode == Dissolve);
        } else {
            assert_eq!(composite_over_extended(facts.mode, Rgba::new([2.0; 3], 1.0), Rgba::new([2.0; 3], 1.0), 1.0, SITE),
                Err(BlendError::Unsupported(facts.mode)));
            assert_eq!(admit_entry(LayerExtension::new(PlanNodeId(2), 1), facts.mode).unwrap_err(), refused(key));
        }
    }
    assert_eq!(defined, 12);

    let entry = || LayerExtension::new(PlanNodeId(2), 1);
    let ledger = admit_entry(entry().with_adjustment(exposure(1.0, 0.0, 1.0, 1.0)), Normal).unwrap();
    assert!(ledger.operators.iter().any(|o| o.key == "sample_range_extended_exposure" && o.exactness == Exactness::Approximate));
    let cases: [(LayerExtension, &str); 7] = [
        (entry().with_adjustment(exposure(1.0, 0.0, 1.0, 2.2)), "sample_range_extended_exposure_encoded"),
        (entry().with_adjustment(AdjustmentOp::Invert), "sample_range_extended_invert"),
        (
            entry().with_blend_if(vec![BlendRangeEntry { channel: BlendChannel::Gray, source: [0.0, 0.0, 0.5, 1.0], destination: [0.0, 0.0, 1.0, 1.0] }]),
            "sample_range_extended_blend_if",
        ),
        (entry().with_filters(vec![LayerFilter::HighPass { radius_px: 2.0 }]), "sample_range_extended_filter_high_pass"),
        (
            entry().with_filters(vec![LayerFilter::AddNoise { amount_pct: 10.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 1 }]),
            "sample_range_extended_filter_add_noise",
        ),
        (entry().with_filters(vec![LayerFilter::GaussianBlur { radius_px: 1.0 }, LayerFilter::Median { radius_px: 1 }]), ""),
        (entry().with_filters(vec![LayerFilter::UnsharpMask { amount_pct: 50.0, radius_px: 1.0, threshold_levels: 0 }]), ""),
    ];
    for (e, key) in cases {
        let result = admit_entry(e, Normal);
        if key.is_empty() {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err(), refused(key));
        }
    }
    let ops = [
        ExtendedOp::Adjustment(AdjustmentKind::Levels),
        ExtendedOp::Adjustment(AdjustmentKind::Exposure),
        ExtendedOp::ExposureEncoded,
        ExtendedOp::BlendIf,
        ExtendedOp::Effects,
        ExtendedOp::Filter(FilterKind::GaussianBlur),
        ExtendedOp::Filter(FilterKind::UnsharpMask),
        ExtendedOp::Filter(FilterKind::Median),
        ExtendedOp::Filter(FilterKind::HighPass),
        ExtendedOp::Filter(FilterKind::AddNoise),
        ExtendedOp::ContentAwareFill,
        ExtendedOp::ContentAwareColourAdaptation,
        ExtendedOp::Blur,
        ExtendedOp::Transform,
    ];
    let defined_ops: Vec<bool> = ops.iter().map(|o| o.defined()).collect();
    assert_eq!(defined_ops, [false, true, false, false, true, true, true, true, false, false, true, false, true, true]);
    for op in ops {
        assert_eq!(op.clause(), "STU-COL-120; STU-FX-006a");
        assert_eq!(ExtOperator::ExtendedEnvelope(op).key(), op.key());
        assert_eq!(
            ExtOperator::ExtendedEnvelope(op).intrinsic(),
            if op.defined() { Exactness::Approximate } else { Exactness::Unsupported }
        );
    }
    assert!(!hsk_studio_pigment::content_aware::fill_extended_defined(&ContentAwareOptions::default()));
}

/// Maths: the `_in(Extended, ..)` functions keep values above 1 (hand values: exposure +1 stop,
/// offset 0.1 of 3.0 = 6.1; a radius-0 blur copies 5.0; unsharp of a 4.0 / 0.0 edge overshoots
/// past 4; median of {5, 5, 9} = 5; the adjustment-layer lerp at k 0.5 from 2 to 4.1 = 3.05),
/// a content-aware fill without adaptation over a 32x32 plane at 3.0 fills a 4x4 hole with 3.0 (the only
/// source colour), and the `_in(Unit, ..)` forms equal the unit functions bit for bit. Hostile:
/// NaN and inf inputs and an overflowing exposure are typed `OutOfDomain`.
#[test]
fn extended_envelope_maths_is_unclamped_and_unit_forms_are_unchanged() {
    let prepared = exposure(1.0, 0.1, 1.0, 1.0).prepare().unwrap();
    assert_eq!(prepared.apply_in(SampleRange::Extended, [3.0, 0.5, -1.0]).unwrap(), [6.1, 1.1, -1.9]);
    assert!(prepared.apply_in(SampleRange::Extended, [f32::NAN, 0.0, 0.0]).is_err());
    assert!(exposure(20.0, 0.0, 1.0, 1.0).prepare().unwrap().apply_in(SampleRange::Extended, [3.0e38, 0.0, 0.0]).is_err());
    let out = composite_adjustment_in(SampleRange::Extended, StudioBlendMode::Normal, Rgba::new([2.0, 0.0, 1.0], 1.0), &exposure(1.0, 0.1, 1.0, 1.0).prepare().unwrap(), 0.5).unwrap();
    assert_eq!(out.rgb, [3.05, 0.05, 1.55]);
    let unit = Rgba::new([0.2, 0.4, 0.6], 0.8);
    assert_eq!(
        composite_adjustment_in(SampleRange::Unit, StudioBlendMode::Multiply, unit, &prepared, 0.7).unwrap(),
        composite_adjustment(StudioBlendMode::Multiply, unit, &prepared, 0.7).unwrap()
    );
    assert!(exposure(1.0, 0.0, 1.0, 1.0).prepare().unwrap().apply_in(SampleRange::Extended, [f32::INFINITY, 0.0, 0.0]).is_err());

    // Blur and filters over a 3x1 plane: 4.0 | 0.0 | 4.0 (and a unit plane for the bit check).
    let row = Rect { x: 0, y: 0, width: 3, height: 1 };
    let src = vec![[4.0, 5.0, 0.5, 1.0], [0.0, 5.0, 0.5, 1.0], [4.0, 9.0, 0.5, 1.0]];
    let copy = blur_window_in(SampleRange::Extended, &src, row, row, &[1.0], BWIDE).unwrap();
    assert_eq!(copy, src);
    let taps = LayerBlur::new(BlurScope::Layer, 2.0).taps().unwrap();
    let blurred = blur_window_in(SampleRange::Extended, &src, row, row, &taps, BWIDE).unwrap();
    assert!(blurred.iter().all(|p| p.iter().all(|v| v.is_finite())) && blurred[1][1] > 1.0);
    let sharp = filter_window_in(SampleRange::Extended, &src, row, row, &LayerFilter::UnsharpMask { amount_pct: 100.0, radius_px: 1.0, threshold_levels: 0 }, WIDE).unwrap();
    assert!(sharp[0][0] > 4.0, "{sharp:?}");
    let median = filter_window_in(SampleRange::Extended, &src, row, row, &LayerFilter::Median { radius_px: 1 }, WIDE).unwrap();
    assert_eq!(median[1][1], 5.0);
    assert!(filter_window_in(SampleRange::Extended, &src, row, row, &LayerFilter::HighPass { radius_px: 1.0 }, WIDE).is_err());
    let mut nan = src.clone();
    nan[1][0] = f32::NAN;
    assert!(blur_window_in(SampleRange::Extended, &nan, row, row, &taps, BWIDE).is_err());
    assert!(filter_window_in(SampleRange::Extended, &nan, row, row, &LayerFilter::Median { radius_px: 1 }, WIDE).is_err());
    let unit_src = vec![[0.4, 0.5, 0.1, 1.0], [0.0, 0.5, 0.9, 0.5], [0.8, 0.2, 0.3, 1.0]];
    assert_eq!(blur_window_in(SampleRange::Unit, &unit_src, row, row, &taps, BWIDE).unwrap(), blur_window(&unit_src, row, row, &taps, BWIDE).unwrap());
    for f in [LayerFilter::UnsharpMask { amount_pct: 300.0, radius_px: 1.0, threshold_levels: 3 }, LayerFilter::Median { radius_px: 1 }, LayerFilter::HighPass { radius_px: 1.0 }] {
        assert_eq!(filter_window_in(SampleRange::Unit, &unit_src, row, row, &f, WIDE).unwrap(), filter_window(&unit_src, row, row, &f, WIDE).unwrap());
    }

    // Content-aware fill without adaptation on a 32x32 plane of 3.0 with a 4x4 hole (a 7x7 source
    // patch fits beside it).
    let bounds = Rect { x: 0, y: 0, width: 32, height: 32 };
    let target = RgbaPlane { bounds, data: vec![[3.0, 2.0, 0.5, 1.0]; 1024] };
    let hole = CoveragePlane {
        bounds,
        data: (0..1024u32).map(|i| if (14..18).contains(&(i % 32)) && (14..18).contains(&(i / 32)) { u16::MAX } else { 0 }).collect(),
    };
    let options = ContentAwareOptions { colour_adaptation: ColourAdaptation::None, ..ContentAwareOptions::default() };
    let filled = fill_in(SampleRange::Extended, &target, &hole, SamplingArea::Auto, &options, &RetouchLimits::default()).unwrap();
    assert!(filled.result.plane.data.iter().all(|p| (p[0] - 3.0).abs() < 1e-5 && (p[1] - 2.0).abs() < 1e-5));
    assert!(fill_in(SampleRange::Extended, &target, &hole, SamplingArea::Auto, &ContentAwareOptions::default(), &RetouchLimits::default()).is_err());
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
