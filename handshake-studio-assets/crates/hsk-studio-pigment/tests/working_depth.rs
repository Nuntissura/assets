//! Lane SMB-PS-ORACLE-PARITY-2: Photoshop's 8-bit document as pigment's `WorkingDepth::Eight` (the
//! running result a later layer reads stored in 8-bit levels, Levels on whole levels), measured on
//! the Photoshop 27.11 D5 oracle (PS-C02 8-bit gamma 1.5, PS-C28 `nomask`, PS-C29 `fill100`).
use hsk_studio_pigment::adjust_math::LevelsRecord;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExtOperator, Group, Layer, NoVector,
    OutputStage, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, RenderPlan, SampleFormat,
    StudioBlendMode, WORKING_DEPTH_EIGHT_OPERATOR, WorkingDepth, quantise_8bit,
};
use hsk_studio_pigment::Rect;

fn record() -> LevelsRecord {
    LevelsRecord {
        input_black: 20.0 / 255.0,
        input_white: 230.0 / 255.0,
        gamma: 1.5,
        output_black: 10.0 / 255.0,
        output_white: 240.0 / 255.0,
    }
}

/// D5 PS-C02 `8bit_comp_in20_230_g1.5_out10_240` (gray ramp, green channel): every stage rounds to
/// a whole level, half up. Ramp 27 -> v8 round(7 * 255 / 210 = 8.5) = 9 -> toe 24 -> 32 (the
/// oracle; the float map gives 29.4 there), ramp 22 -> 15, ramp 20 -> 10, ramp 230 -> 240.
#[test]
fn levels_on_whole_levels_reproduce_the_oracle_ramp() {
    let r = record();
    let level = |g: f32| (r.map_value_8bit(g / 255.0) * 255.0).round();
    assert_eq!(level(27.0), 32.0);
    assert_eq!(level(22.0), 15.0);
    assert_eq!(level(20.0), 10.0);
    assert_eq!(level(0.0), 10.0);
    assert_eq!(level(230.0), 240.0);
    assert_eq!(level(255.0), 240.0);
    // a mid-ramp value: 128 -> v8 round(108 * 255 / 210 = 131.14) = 131 -> 255 (131/255)^(2/3)
    // = 163.6 -> 164 -> round(10 + 164 * 230 / 255 = 157.92) = 158
    assert_eq!(level(128.0), 158.0);
    // every output is a whole level
    for g in 0..=255u16 {
        let v = r.map_value_8bit(f32::from(g) / 255.0) * 255.0;
        assert!((v - v.round()).abs() < 1e-4, "{g}: {v}");
    }
    // the identity record is the identity on whole levels
    for g in 0..=255u16 {
        let x = f32::from(g) / 255.0;
        assert_eq!((LevelsRecord::IDENTITY.map_value_8bit(x) * 255.0).round(), f32::from(g));
    }
}

#[test]
fn quantise_8bit_is_half_up_on_the_level_grid() {
    assert_eq!(quantise_8bit(0.0), 0.0);
    assert_eq!(quantise_8bit(1.0), 1.0);
    assert_eq!(quantise_8bit(77.4 / 255.0), 77.0 / 255.0);
    assert_eq!(quantise_8bit(77.6 / 255.0), 78.0 / 255.0);
    assert_eq!(quantise_8bit(0.3), 77.0 / 255.0);
    for g in 0..=255u16 {
        let v = f32::from(g) / 255.0;
        assert_eq!(quantise_8bit(v), v, "{g}");
    }
}

fn plan() -> RenderPlan<NoVector> {
    let extent = Rect { x: 0, y: 0, width: 4, height: 4 };
    RenderPlan {
        revision: 1,
        target: OutputTarget {
            extent,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [1; 32], blend_space: BlendSpace::Encoded },
        },
        nodes: vec![
            PlanNode::Solid { rect: extent, rgba: [0.5, 0.5, 0.5, 1.0] },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![Layer {
                    input: PlanNodeId(0),
                    mode: StudioBlendMode::Normal,
                    opacity: 1.0,
                    fill_opacity: 1.0,
                    mask: None,
                }],
            }),
        ],
        root: PlanNodeId(1),
    }
}

/// The float depth is the default and absent from the digest; the 8-bit depth is reported as the
/// declarable `working_depth_8bit` (class Approximate) and changes the digest.
#[test]
fn working_depth_is_reported_and_digested_only_when_eight_bit() {
    let p = plan();
    assert_eq!(WorkingDepth::default(), WorkingDepth::Float);
    let float = PlanExtensions::default().with_output(OutputStage::default().with_working_depth(WorkingDepth::Float));
    assert!(float.is_empty());
    assert_eq!(float.digest_with(&p).unwrap(), p.digest().unwrap());
    assert!(float.validate(&p).unwrap().is_empty());
    let eight = PlanExtensions::default().with_output(OutputStage::default().with_working_depth(WorkingDepth::Eight));
    assert!(!eight.is_empty());
    assert!(eight.output.is_eight_bit());
    assert_ne!(eight.digest_with(&p).unwrap(), p.digest().unwrap());
    assert_eq!(eight.validate(&p).unwrap(), vec![ExtOperator::WorkingDepthEight]);
    assert_eq!(ExtOperator::WorkingDepthEight.key(), WORKING_DEPTH_EIGHT_OPERATOR);
    assert_eq!(WORKING_DEPTH_EIGHT_OPERATOR, "working_depth_8bit");
    assert_eq!(ExtOperator::WorkingDepthEight.intrinsic(), Exactness::Approximate);
}
