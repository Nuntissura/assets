//! Lane SMB-PS-ADJUST-STYLES slice 1b: the CPU backend renders the second STU-RAS-151 adjustment
//! set through the envelope path BIT-IDENTICAL to pigment's scalar layer rule
//! (`composite_adjustment`), and reports pigment's class (Approximate). Hand values for the ops:
//! pigment `tests/adjust_colour.rs`; one hand value is repeated here end to end.
#![deny(clippy::arithmetic_side_effects)]

mod common;
use common::*;
use hsk_studio_pigment::adjust_math::{
    AdjustmentKind, AdjustmentOp, BlackAndWhite, ChannelMixer, ColourBalance, ColourLookup,
    GradientMap, GradientMapStop, MatchColour, PhotoFilter, ReplaceColour, SelectiveColour,
    SelectiveColourMethod, ShadowsHighlights, ToneCompression, ToneCompressionMethod, Vibrance,
    composite_adjustment,
};
use hsk_studio_pigment::blend_math::Rgba;
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

fn adjustment_plan(backdrop: [f32; 4], mode: StudioBlendMode, opacity: f32) -> RenderPlan {
    let one = rect(0, 0, 1, 1);
    plan(
        one,
        vec![
            solid(one, backdrop),
            placeholder(one),
            group(vec![
                layer(0, StudioBlendMode::Normal, 1.0),
                layer(1, mode, opacity),
            ]),
        ],
        2,
    )
}

fn ops() -> Vec<AdjustmentOp> {
    let mut lut = Vec::new();
    for b in [1.0f32, 0.0] {
        for g in [1.0f32, 0.0] {
            for r in [1.0f32, 0.0] {
                lut.push([r, g, b]);
            }
        }
    }
    lut.reverse();
    vec![
        AdjustmentOp::Vibrance(Vibrance { vibrance: 100.0, saturation: 20.0 }),
        AdjustmentOp::ColourBalance(ColourBalance {
            shadows: [10.0, 0.0, -5.0],
            midtones: [50.0, 0.0, 0.0],
            highlights: [0.0, 0.0, 30.0],
            preserve_luminosity: true,
        }),
        AdjustmentOp::PhotoFilter(PhotoFilter {
            colour: [1.0, 0.5, 0.0],
            density: 50.0,
            preserve_luminosity: true,
        }),
        AdjustmentOp::BlackAndWhite(BlackAndWhite {
            weights: [40.0, 60.0, 40.0, 60.0, 20.0, 80.0],
            tint: Some([30.0, 40.0]),
        }),
        AdjustmentOp::ChannelMixer(ChannelMixer {
            red: [100.0, 50.0, 0.0, 10.0],
            green: [0.0, 80.0, 20.0, 0.0],
            blue: [0.0, 0.0, 100.0, -5.0],
            monochrome: false,
        }),
        AdjustmentOp::SelectiveColour(SelectiveColour {
            method: SelectiveColourMethod::Relative,
            ranges: [[-25.0, 10.0, 0.0, 5.0]; 9],
        }),
        AdjustmentOp::GradientMap(GradientMap {
            stops: vec![
                GradientMapStop { position: 0.0, colour: [0.0, 0.0, 0.2] },
                GradientMapStop { position: 0.6, colour: [0.9, 0.3, 0.1] },
                GradientMapStop { position: 1.0, colour: [1.0, 1.0, 0.8] },
            ],
            reverse: false,
        }),
        AdjustmentOp::ColourLookup(ColourLookup { side: 2, table: lut }),
        AdjustmentOp::ShadowsHighlights(ShadowsHighlights {
            shadow_amount: 50.0,
            shadow_tone_width: 50.0,
            shadow_radius: 0.0,
            highlight_amount: 30.0,
            highlight_tone_width: 40.0,
            highlight_radius: 0.0,
            colour_correction: 20.0,
            midtone_contrast: 10.0,
            black_clip: 0.01,
            white_clip: 0.01,
        }),
        AdjustmentOp::ToneCompression(ToneCompression {
            method: ToneCompressionMethod::HighlightCompression,
            exposure: 1.0,
            gamma: 1.2,
        }),
        AdjustmentOp::MatchColour(MatchColour {
            luminance: 90.0,
            colour_intensity: 120.0,
            fade: 20.0,
            neutralize: false,
            source_mean: [0.6, 0.5, 0.4],
            source_std: [0.2; 3],
            target_mean: [0.4; 3],
            target_std: [0.1; 3],
        }),
        AdjustmentOp::ReplaceColour(ReplaceColour {
            samples: vec![[0.8, 0.4, 0.2]],
            fuzziness: 80.0,
            hue: 60.0,
            saturation: 10.0,
            lightness: -10.0,
        }),
    ]
}

#[test]
fn second_set_renders_bit_identical_to_pigment_and_reports_its_class() {
    // End-to-end hand value: monochrome-free mixer row 0 on (0.5, 0.2, 0.1) gives 0.5 + 0.1 + 0.1
    // = 0.7 (pigment tests/adjust_colour.rs); at opacity 0.5 over the opaque backdrop the red
    // channel is 0.5 + (0.7 - 0.5) 0.5 = 0.6.
    let mixer = envelope(vec![one_layer(2, 1).with_adjustment(AdjustmentOp::ChannelMixer(
        ChannelMixer {
            red: [100.0, 50.0, 0.0, 10.0],
            green: [0.0, 100.0, 0.0, 0.0],
            blue: [0.0, 0.0, 100.0, 0.0],
            monochrome: false,
        },
    ))]);
    near(
        pixel(&adjustment_plan([0.5, 0.2, 0.1, 1.0], StudioBlendMode::Normal, 0.5), &mixer, &no_sources()),
        [0.6, 0.2, 0.1, 1.0],
        1e-6,
    );

    // Every kind, several backdrops, modes and opacities: the backend's pixel equals pigment's
    // scalar rule bit for bit (f32 to_bits).
    let backdrops = [
        [0.8, 0.4, 0.2, 1.0],
        [0.1, 0.6, 0.9, 1.0],
        [0.5, 0.5, 0.5, 0.5],
        [0.95, 0.9, 0.85, 1.0],
    ];
    let modes = [
        (StudioBlendMode::Normal, 1.0f32),
        (StudioBlendMode::Normal, 0.4),
        (StudioBlendMode::Multiply, 0.7),
        (StudioBlendMode::Screen, 1.0),
    ];
    let renderer: &dyn RenderBackend = &Renderer::new(1).unwrap();
    for op in ops() {
        let prepared = op.prepare().unwrap();
        // Capability: pigment's class, never a better one.
        let operator = ExtOperator::Adjustment(op.kind());
        assert_eq!(renderer.extension_exactness(operator), operator.intrinsic(), "{:?}", op.kind());
        assert_eq!(operator.intrinsic(), Exactness::Approximate);
        let ext = envelope(vec![one_layer(2, 1).with_adjustment(op.clone())]);
        for b in backdrops {
            for (mode, opacity) in modes {
                let got = pixel(&adjustment_plan(b, mode, opacity), &ext, &no_sources());
                let want = composite_adjustment(
                    mode,
                    Rgba::new([b[0], b[1], b[2]], b[3]),
                    &prepared,
                    opacity,
                )
                .unwrap();
                let want = [want.rgb[0], want.rgb[1], want.rgb[2], want.a];
                assert_eq!(
                    got.map(f32::to_bits),
                    want.map(f32::to_bits),
                    "{:?} {mode:?} {opacity} over {b:?}: {got:?} != {want:?}",
                    op.kind()
                );
            }
        }
    }
    // Every new kind is covered above.
    let covered: Vec<AdjustmentKind> = ops().iter().map(AdjustmentOp::kind).collect();
    assert_eq!(covered, AdjustmentKind::ALL[8..].to_vec());
}
