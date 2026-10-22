//! Ported unchanged in substance from composite `tests/blend_oracles.rs` and `tests/blend_laws.rs`
//! (composite commits 4eb5cec/e362ba6/a6f2c7e) when pigment became the single owner of blend math
//! (CX-CAP-001; work-folder `diagnostics/render-plan-ir.md`). Renames only: BlendMode ->
//! StudioBlendMode, Fidelity -> Exactness, MODE_TABLE -> MODES, info() -> facts(),
//! BlendSpace::Linear -> LinearLight, receipt.{lowest,operators} -> receipt.exactness.{..}.
//!
//! Hand-computed oracle values per blend formula family. Each expected number below was derived
//! on paper from the cited formula (W3C Compositing 1 s10) before being typed; the code under test
//! is never used to produce an expectation. These are analytic vectors, NOT Photoshop parity
//! evidence: Photoshop oracle comparison is a separate, Operator-approved step.
use hsk_studio_pigment::blend_math::{
    Applicability, BlendContext, BlendError, BlendSpace, Exactness as Fidelity, MAX_STACK_DEPTH,
    MODES as MODE_TABLE, PixelSite, Rgba, StackNode, StudioBlendMode as BlendMode, blend_rgb,
    composite_over, evaluate_stack, lum, sat, set_lum, set_sat, site_unit,
};

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

fn near(actual: [f32; 3], expected: [f32; 3], what: &str) {
    for i in 0..3 {
        assert!(
            (actual[i] - expected[i]).abs() < 2e-6,
            "{what}: channel {i} got {} expected {}",
            actual[i],
            expected[i]
        );
    }
}
fn near_rgba(actual: Rgba, expected: Rgba, what: &str) {
    near(actual.rgb, expected.rgb, what);
    assert!(
        (actual.a - expected.a).abs() < 2e-6,
        "{what}: alpha {} vs {}",
        actual.a,
        expected.a
    );
}
fn blend(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    blend_rgb(mode, cb, cs).unwrap()
}
const SITE: PixelSite = PixelSite {
    x: 3,
    y: 5,
    seed: 11,
};
fn ctx(space: BlendSpace) -> BlendContext {
    BlendContext { space, site: SITE }
}
fn opaque(c: f32) -> Rgba {
    Rgba::new([c; 3], 1.0)
}
fn leaf(c: f32, mode: BlendMode) -> StackNode {
    StackNode::Leaf {
        source: opaque(c),
        mode,
        opacity: 1.0,
    }
}

#[test]
fn separable_oracles_hand_computed() {
    bounded_case("separable_oracles_hand_computed", || {
        let cb = [0.25, 0.5, 1.0];
        let cs = [0.5, 0.5, 0.25];
        near(
            blend(BlendMode::Multiply, cb, cs),
            [0.125, 0.25, 0.25],
            "multiply",
        );
        // screen = cb + cs - cb*cs: 0.25+0.5-0.125, 0.5+0.5-0.25, 1+0.25-0.25
        near(
            blend(BlendMode::Screen, cb, cs),
            [0.625, 0.75, 1.0],
            "screen",
        );
        // overlay(cb,cs) = hard_light(cs, cb): cb<=.5 -> cs*2cb ; cb>.5 -> screen(cs, 2cb-1)
        near(
            blend(BlendMode::Overlay, cb, cs),
            [0.25, 0.5, 1.0],
            "overlay",
        );
        // hard_light: cs<=.5 -> cb*2cs ; else screen(cb, 2cs-1)
        near(
            blend(BlendMode::HardLight, cb, cs),
            [0.25, 0.5, 0.5],
            "hard_light",
        );
        near(
            blend(BlendMode::Darken, cb, cs),
            [0.25, 0.5, 0.25],
            "darken",
        );
        near(
            blend(BlendMode::Lighten, cb, cs),
            [0.5, 0.5, 1.0],
            "lighten",
        );
        near(
            blend(BlendMode::Difference, cb, cs),
            [0.25, 0.0, 0.75],
            "difference",
        );
        // exclusion = cb + cs - 2 cb cs
        near(
            blend(BlendMode::Exclusion, cb, cs),
            [0.5, 0.5, 0.75],
            "exclusion",
        );
        // colour_dodge = min(1, cb/(1-cs)): 0.25/0.5, 0.5/0.5, min(1, 1/0.75)
        near(
            blend(BlendMode::ColourDodge, cb, cs),
            [0.5, 1.0, 1.0],
            "colour_dodge",
        );
        // colour_dodge special cases: cb==0 -> 0 ; cs==1 -> 1
        near(
            blend(BlendMode::ColourDodge, [0.0, 0.5, 0.5], [1.0, 1.0, 0.0]),
            [0.0, 1.0, 0.5],
            "colour_dodge edges",
        );
        // colour_burn = 1 - min(1, (1-cb)/cs): 1-0.25/0.5 ; cb==0 -> 1-min(1,2)=0 ; cb==1 -> 1
        near(
            blend(BlendMode::ColourBurn, [0.75, 0.0, 1.0], [0.5, 0.5, 0.0]),
            [0.5, 0.0, 1.0],
            "colour_burn",
        );
        // colour_burn cs==0 (cb<1) -> 0
        near(
            blend(BlendMode::ColourBurn, [0.5; 3], [0.0; 3]),
            [0.0; 3],
            "colour_burn cs0",
        );
        // soft_light (W3C): cs<=.5: cb-(1-2cs)cb(1-cb) = 0.25-0.5*0.25*0.75 = 0.15625
        // cs>.5, cb>.25: cb+(2cs-1)(sqrt(cb)-cb) = 0.64+0.5*(0.8-0.64) = 0.72 ; cs=1, cb=.5 -> sqrt(.5)
        near(
            blend(BlendMode::SoftLight, [0.25, 0.64, 0.5], [0.25, 0.75, 1.0]),
            [0.15625, 0.72, std::f32::consts::FRAC_1_SQRT_2],
            "soft_light",
        );
        // soft_light cb<=.25 branch: D=((16cb-12)cb+4)cb = ((2-12)*.125+4)*.125 = .34375 ; result = D at cs=1
        near(
            blend(BlendMode::SoftLight, [0.125; 3], [1.0; 3]),
            [0.343_75; 3],
            "soft_light low",
        );
        // Normal returns the source colour unchanged.
        near(blend(BlendMode::Normal, cb, cs), cs, "normal");
    });
}

#[test]
fn nonseparable_oracles_hand_computed() {
    bounded_case("nonseparable_oracles_hand_computed", || {
        // Lum = 0.3R + 0.59G + 0.11B ; Sat = max - min.
        assert!((lum([1.0, 0.0, 0.0]) - 0.3).abs() < 1e-6);
        assert!((lum([0.0, 1.0, 0.0]) - 0.59).abs() < 1e-6);
        assert!((lum([0.0, 0.0, 1.0]) - 0.11).abs() < 1e-6);
        assert!((sat([0.6, 0.3, 0.0]) - 0.6).abs() < 1e-6);
        // SetSat([0,.2,.4], .6): min->0, max->.6, mid = .2*.6/.4 = .3
        near(set_sat([0.0, 0.2, 0.4], 0.6), [0.0, 0.3, 0.6], "set_sat");
        near(set_sat([0.5; 3], 0.6), [0.0; 3], "set_sat flat");
        // SetLum([0,.3,.6], .357): Lum=.243, d=.114
        near(
            set_lum([0.0, 0.3, 0.6], 0.357),
            [0.114, 0.414, 0.714],
            "set_lum",
        );

        let cb = [0.6, 0.3, 0.0]; // Lum .357, Sat .6
        let cs = [0.0, 0.2, 0.4]; // Lum .162, Sat .4
        // hue = SetLum(SetSat(cs, Sat(cb)), Lum(cb)) = SetLum([0,.3,.6], .357)
        near(blend(BlendMode::Hue, cb, cs), [0.114, 0.414, 0.714], "hue");
        // saturation = SetLum(SetSat(cb, Sat(cs)), Lum(cb)) = SetLum([.4,.2,0], .357): Lum .238, d .119
        near(
            blend(BlendMode::Saturation, cb, cs),
            [0.519, 0.319, 0.119],
            "saturation",
        );
        // colour = SetLum(cs, Lum(cb)): d = .357-.162 = .195
        near(
            blend(BlendMode::Colour, cb, cs),
            [0.195, 0.395, 0.595],
            "colour",
        );
        // luminosity = SetLum(cb, Lum(cs)): d=-.195 -> [.405,.105,-.195]; ClipColor n<0:
        // l=.162, k=l/(l-n)=.162/.357 ; C = l + (C-l)*k -> [.272268908,.136134454,0]
        near(
            blend(BlendMode::Luminosity, cb, cs),
            [0.272_268_9, 0.136_134_45, 0.0],
            "luminosity (clip n<0)",
        );
        // colour of pure red onto 50% grey: SetLum -> [1.2,.2,.2]; ClipColor x>1:
        // l=.5 ; C = l + (C-l)*(1-l)/(x-l) = .5 + (C-.5)*.5/.7 -> [1,.285714286,.285714286]
        near(
            blend(BlendMode::Colour, [0.5; 3], [1.0, 0.0, 0.0]),
            [1.0, 0.285_714_3, 0.285_714_3],
            "colour (clip x>1)",
        );
        // hue/saturation of a flat (zero-saturation) operand leave only luminosity: grey 0.5.
        near(
            blend(BlendMode::Hue, [0.5; 3], [1.0, 0.0, 0.0]),
            [0.5; 3],
            "hue flat backdrop",
        );
        near(set_lum([0.2, 0.2, 0.2], 0.5), [0.5; 3], "set_lum flat");
    });
}

#[test]
fn source_over_alpha_and_groups_hand_computed() {
    bounded_case("source_over_alpha_and_groups_hand_computed", || {
        let backdrop = Rgba::new([0.2, 0.4, 0.6], 0.5);
        let red = Rgba::new([1.0, 0.0, 0.0], 0.5);
        // Normal: ao = .5+.5*.5 = .75 ; co = .5*red + .5*.5*cb = [.55,.1,.15] ; Cr = co/ao
        let out = composite_over(BlendMode::Normal, backdrop, red, 1.0, SITE).unwrap();
        near_rgba(
            out,
            Rgba::new([0.733_333_34, 0.133_333_33, 0.2], 0.75),
            "normal alpha",
        );
        // opacity .5 -> as=.25: ao = .25+.5*.75 = .625 ; co = [.325,.15,.225] ; Cr = [.52,.24,.36]
        let out = composite_over(BlendMode::Normal, backdrop, red, 0.5, SITE).unwrap();
        near_rgba(out, Rgba::new([0.52, 0.24, 0.36], 0.625), "normal opacity");
        // Multiply over a half-transparent backdrop: B=.25 ; Cs'=(1-.5)*.5+.5*.25=.375 ; as=1 -> .375
        let out = composite_over(
            BlendMode::Multiply,
            Rgba::new([0.5; 3], 0.5),
            opaque(0.5),
            1.0,
            SITE,
        )
        .unwrap();
        near_rgba(out, Rgba::new([0.375; 3], 1.0), "multiply partial backdrop");
        // Transparent backdrop: the blend is ignored (ab=0 -> Cs'=Cs).
        let out = composite_over(
            BlendMode::Multiply,
            Rgba::TRANSPARENT,
            opaque(0.5),
            1.0,
            SITE,
        )
        .unwrap();
        near_rgba(out, opaque(0.5), "multiply on transparent");
        // Premultiplied helpers: [.2,.4,.6]*.5 and back; zero alpha has no colour.
        let p = backdrop.premultiplied();
        assert_eq!(p, [0.1, 0.2, 0.3, 0.5]);
        near_rgba(
            Rgba::from_premultiplied(p),
            backdrop,
            "premultiply roundtrip",
        );
        assert_eq!(
            Rgba::from_premultiplied([0.0, 0.0, 0.0, 0.0]),
            Rgba::TRANSPARENT
        );

        // Stack: red then multiply [.5,.5,1] on transparent -> red a=1, then B=[.5,0,0] at ab=1.
        let stack = [
            StackNode::Leaf {
                source: Rgba::new([1.0, 0.0, 0.0], 1.0),
                mode: BlendMode::Normal,
                opacity: 1.0,
            },
            StackNode::Leaf {
                source: Rgba::new([0.5, 0.5, 1.0], 1.0),
                mode: BlendMode::Multiply,
                opacity: 1.0,
            },
        ];
        let (out, receipt) =
            evaluate_stack(&stack, Rgba::TRANSPARENT, &ctx(BlendSpace::Encoded)).unwrap();
        near_rgba(out, Rgba::new([0.5, 0.0, 0.0], 1.0), "two-layer stack");
        assert_eq!(receipt.exactness.lowest, Fidelity::Exact);

        // Groups on an opaque 0.5 grey backdrop, child = multiply 0.5:
        let base = opaque(0.5);
        let child = || vec![leaf(0.5, BlendMode::Multiply)];
        // Pass Through: child multiplies the real backdrop -> 0.25.
        let pass = StackNode::Group {
            children: child(),
            mode: BlendMode::PassThrough,
            opacity: 1.0,
        };
        let (out, receipt) = evaluate_stack(&[pass], base, &ctx(BlendSpace::Encoded)).unwrap();
        near_rgba(out, opaque(0.25), "pass-through group");
        assert_eq!(receipt.exactness.lowest, Fidelity::Exact);
        // Isolated (Normal) group: child multiplies a TRANSPARENT backdrop -> stays 0.5 -> Normal -> 0.5.
        let isolated = StackNode::Group {
            children: child(),
            mode: BlendMode::Normal,
            opacity: 1.0,
        };
        let (out, _) = evaluate_stack(&[isolated], base, &ctx(BlendSpace::Encoded)).unwrap();
        near_rgba(out, opaque(0.5), "isolated normal group");
        // Isolated group blended with Multiply: inner .5 -> multiply with backdrop .5 -> .25.
        let isolated_mul = StackNode::Group {
            children: child(),
            mode: BlendMode::Multiply,
            opacity: 1.0,
        };
        let (out, _) = evaluate_stack(&[isolated_mul], base, &ctx(BlendSpace::Encoded)).unwrap();
        near_rgba(out, opaque(0.25), "isolated multiply group");
        // Pass Through at 50% opacity: premultiplied mix of before (.5) and after (.25) = .375,
        // tagged Approximate (Photoshop mapping pending oracle) and recorded as such.
        let half = StackNode::Group {
            children: child(),
            mode: BlendMode::PassThrough,
            opacity: 0.5,
        };
        let (out, receipt) = evaluate_stack(&[half], base, &ctx(BlendSpace::LinearLight)).unwrap();
        near_rgba(out, opaque(0.375), "pass-through 50%");
        assert_eq!(receipt.exactness.lowest, Fidelity::Approximate);
        assert_eq!(receipt.space, BlendSpace::LinearLight);

        // Nesting is bounded.
        let mut deep = leaf(0.5, BlendMode::Normal);
        for _ in 0..=MAX_STACK_DEPTH + 1 {
            deep = StackNode::Group {
                children: vec![deep],
                mode: BlendMode::PassThrough,
                opacity: 1.0,
            };
        }
        assert_eq!(
            evaluate_stack(&[deep], base, &ctx(BlendSpace::Encoded)).unwrap_err(),
            BlendError::TooDeep
        );
    });
}

#[test]
fn approximate_formulas_and_dissolve() {
    bounded_case("approximate_formulas_and_dissolve", || {
        // community-documented formulas (Approximate): a = backdrop, b = source
        near(
            blend(BlendMode::LinearBurn, [0.75; 3], [0.5; 3]),
            [0.25; 3],
            "linear_burn a+b-1",
        );
        near(
            blend(BlendMode::LinearBurn, [0.25; 3], [0.5; 3]),
            [0.0; 3],
            "linear_burn clamp",
        );
        near(
            blend(BlendMode::LinearDodgeAdd, [0.75; 3], [0.5; 3]),
            [1.0; 3],
            "linear_dodge clamp",
        );
        near(
            blend(BlendMode::LinearDodgeAdd, [0.25; 3], [0.5; 3]),
            [0.75; 3],
            "linear_dodge",
        );
        near(
            blend(BlendMode::LinearLight, [0.25; 3], [0.75; 3]),
            [0.75; 3],
            "linear_light a+2b-1",
        );
        // vivid_light: b<.5 -> 1-(1-a)/(2b) ; b>=.5 -> a/(2(1-b))
        near(
            blend(BlendMode::VividLight, [0.5; 3], [0.25; 3]),
            [0.0; 3],
            "vivid_light low",
        );
        near(
            blend(BlendMode::VividLight, [0.25; 3], [0.75; 3]),
            [0.5; 3],
            "vivid_light high",
        );
        // pin_light: b<.5 -> min(a,2b) ; else max(a,2b-1)
        near(
            blend(BlendMode::PinLight, [0.5; 3], [0.2; 3]),
            [0.4; 3],
            "pin_light low",
        );
        near(
            blend(BlendMode::PinLight, [0.25; 3], [0.75; 3]),
            [0.5; 3],
            "pin_light high",
        );
        near(
            blend(BlendMode::HardMix, [0.5; 3], [0.5; 3]),
            [1.0; 3],
            "hard_mix on",
        );
        near(
            blend(BlendMode::HardMix, [0.25; 3], [0.5; 3]),
            [0.0; 3],
            "hard_mix off",
        );
        near(
            blend(BlendMode::Subtract, [0.75; 3], [0.25; 3]),
            [0.5; 3],
            "subtract",
        );
        near(
            blend(BlendMode::Subtract, [0.25; 3], [0.75; 3]),
            [0.0; 3],
            "subtract clamp",
        );
        near(
            blend(BlendMode::Divide, [0.25; 3], [0.5; 3]),
            [0.5; 3],
            "divide",
        );
        near(
            blend(BlendMode::Divide, [0.5; 3], [0.25; 3]),
            [1.0; 3],
            "divide clamp",
        );
        near(
            blend(BlendMode::Divide, [0.5, 0.0, 0.5], [0.0, 0.0, 1.0]),
            [1.0, 0.0, 0.5],
            "divide zero",
        );
        // darker/lighter colour pick a WHOLE pixel by channel sum (1.5 vs 1.2), never a channel mix.
        let cb = [0.5, 0.5, 0.5];
        let cs = [0.2, 0.9, 0.1];
        assert_eq!(blend(BlendMode::DarkerColour, cb, cs), cs);
        assert_eq!(blend(BlendMode::LighterColour, cb, cs), cb);

        // Dissolve: deterministic per site, never partial alpha, alpha 1 always kept, 0 never.
        let src = Rgba::new([1.0, 0.0, 0.0], 0.5);
        let mut kept = 0u32;
        for x in 0..64 {
            for y in 0..64 {
                let site = PixelSite { x, y, seed: 7 };
                let a =
                    composite_over(BlendMode::Dissolve, Rgba::TRANSPARENT, src, 1.0, site).unwrap();
                let b =
                    composite_over(BlendMode::Dissolve, Rgba::TRANSPARENT, src, 1.0, site).unwrap();
                assert_eq!(a, b);
                assert!(a.a == 0.0 || a.a == 1.0);
                assert!((0.0..1.0).contains(&site_unit(site)));
                kept += u32::from(a.a == 1.0);
                let full = composite_over(
                    BlendMode::Dissolve,
                    Rgba::TRANSPARENT,
                    opaque(0.3),
                    1.0,
                    site,
                )
                .unwrap();
                assert_eq!(full.a, 1.0);
                let none =
                    composite_over(BlendMode::Dissolve, Rgba::TRANSPARENT, src, 0.0, site).unwrap();
                assert_eq!(none.a, 0.0);
            }
        }
        // 4096 sites at alpha 0.5: the hash is uniform enough to land within 40..60 percent.
        assert!((1638..=2458).contains(&kept), "kept {kept} of 4096");
    });
}

#[test]
fn fidelity_table_refusals_and_receipt() {
    bounded_case("fidelity_table_refusals_and_receipt", || {
        // Canonical discriminants 1..=35 are contiguous and round-trip.
        for (i, row) in MODE_TABLE.iter().enumerate() {
            let d = u16::try_from(i + 1).unwrap();
            assert_eq!(row.mode.discriminant(), d);
            assert_eq!(row.mode.facts(), *row);
            assert_eq!(BlendMode::try_from(d), Ok(row.mode));
        }
        assert_eq!(
            BlendMode::try_from(0),
            Err(BlendError::UnknownDiscriminant(0))
        );
        assert_eq!(
            BlendMode::try_from(36),
            Err(BlendError::UnknownDiscriminant(36))
        );
        assert_eq!(BlendMode::Multiply.discriminant(), 5);
        assert_eq!(BlendMode::Divide.discriminant(), 30);

        use BlendMode::*;
        for m in [
            Normal,
            Darken,
            Multiply,
            ColourBurn,
            Lighten,
            Screen,
            ColourDodge,
            Overlay,
            SoftLight,
            HardLight,
            Difference,
            Exclusion,
            Hue,
            Saturation,
            Colour,
            Luminosity,
            PassThrough,
        ] {
            assert_eq!(m.facts().exactness, Fidelity::Exact, "{}", m.key());
        }
        for m in [
            Dissolve,
            LinearBurn,
            LinearDodgeAdd,
            VividLight,
            LinearLight,
            PinLight,
            HardMix,
            LighterColour,
            DarkerColour,
            Subtract,
            Divide,
        ] {
            assert_eq!(m.facts().exactness, Fidelity::Approximate, "{}", m.key());
        }
        for m in [Average, Negation, Reflect, Glow, Erase] {
            assert_eq!(m.facts().exactness, Fidelity::Unsupported, "{}", m.key());
            assert_eq!(m.facts().applicability, Applicability::Unspecified);
        }
        assert_eq!(
            Fidelity::Exact.lowest_of(Fidelity::Approximate),
            Fidelity::Approximate
        );
        assert_eq!(
            Fidelity::Unsupported.lowest_of(Fidelity::Exact),
            Fidelity::Unsupported
        );

        // Typed refusals, raised even when the source is invisible (never a silent skip/Normal).
        let invisible = Rgba::new([0.5; 3], 0.0);
        for (mode, expected) in [
            (PassThrough, BlendError::GroupOnly(PassThrough)),
            (Behind, BlendError::ToolOnly(Behind)),
            (Clear, BlendError::ToolOnly(Clear)),
            (Average, BlendError::Unsupported(Average)),
            (Erase, BlendError::Unsupported(Erase)),
        ] {
            assert_eq!(
                composite_over(mode, opaque(0.5), invisible, 1.0, SITE),
                Err(expected),
                "{}",
                mode.key()
            );
            assert_eq!(blend_rgb(mode, [0.5; 3], [0.5; 3]), Err(expected));
        }
        // Domain: out-of-range, NaN and bad opacity are errors, not clamped.
        assert_eq!(
            blend_rgb(Multiply, [1.5; 3], [0.5; 3]),
            Err(BlendError::OutOfDomain)
        );
        assert_eq!(
            blend_rgb(Multiply, [0.5; 3], [f32::NAN; 3]),
            Err(BlendError::OutOfDomain)
        );
        assert_eq!(
            composite_over(Normal, opaque(0.5), opaque(0.5), 1.5, SITE),
            Err(BlendError::OutOfDomain)
        );
        // Refusal inside a nested stack aborts the whole evaluation.
        let stack = [StackNode::Group {
            children: vec![leaf(0.5, Average)],
            mode: PassThrough,
            opacity: 1.0,
        }];
        assert_eq!(
            evaluate_stack(&stack, opaque(0.5), &ctx(BlendSpace::Encoded)).unwrap_err(),
            BlendError::Unsupported(Average)
        );

        // The receipt carries the declared space and the LOWEST class of every operator used.
        let stack = [
            leaf(0.3, Multiply),
            leaf(0.4, LinearBurn),
            leaf(0.2, Screen),
        ];
        for space in [BlendSpace::Encoded, BlendSpace::LinearLight] {
            let (_, receipt) = evaluate_stack(&stack, opaque(0.5), &ctx(space)).unwrap();
            assert_eq!(receipt.space, space);
            assert_eq!(receipt.exactness.lowest, Fidelity::Approximate);
            let keys: Vec<_> = receipt.exactness.operators.iter().map(|o| o.key).collect();
            assert_eq!(keys, ["multiply", "linear_burn", "screen"]);
        }
        // The host's validated prism transfer classification maps onto the declared blend space.
        assert_eq!(
            BlendSpace::from(hsk_studio_prism::Transfer::LinearLight),
            BlendSpace::LinearLight
        );
        assert_eq!(
            BlendSpace::from(hsk_studio_prism::Transfer::Encoded),
            BlendSpace::Encoded
        );
        assert_eq!(
            BlendSpace::parse("linear_light"),
            Some(BlendSpace::LinearLight)
        );
        assert_eq!(BlendSpace::parse("srgb"), None);
    });
}

// ---- algebraic law sweep (from composite tests/blend_laws.rs) ----

const G: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

fn lattice() -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(125);
    for r in G {
        for g in G {
            for b in G {
                out.push([r, g, b]);
            }
        }
    }
    out
}

fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= eps)
}

#[test]
fn blend_laws_sweep() {
    bounded_case("blend_laws_sweep", || {
        let colours = lattice();
        let site = PixelSite {
            x: 1,
            y: 2,
            seed: 3,
        };
        let layer_modes: Vec<BlendMode> = MODE_TABLE
            .iter()
            .filter(|r| r.applicability == Applicability::Layer)
            .map(|r| r.mode)
            .collect();
        // 35 members minus pass_through (group-only), behind + clear (tool-only), five unrecovered.
        assert_eq!(layer_modes.len(), 27);
        let symmetric = [
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::LinearDodgeAdd,
        ];
        // (mode, source value that leaves the backdrop unchanged)
        let identities = [
            (BlendMode::Multiply, 1.0),
            (BlendMode::Screen, 0.0),
            (BlendMode::Darken, 1.0),
            (BlendMode::Lighten, 0.0),
            (BlendMode::Difference, 0.0),
            (BlendMode::Exclusion, 0.0),
            (BlendMode::LinearDodgeAdd, 0.0),
            (BlendMode::LinearBurn, 1.0),
            (BlendMode::Subtract, 0.0),
            (BlendMode::Divide, 1.0),
            (BlendMode::Overlay, 0.5),
            (BlendMode::HardLight, 0.5),
            (BlendMode::SoftLight, 0.5),
            (BlendMode::ColourDodge, 0.0),
            (BlendMode::ColourBurn, 1.0),
            (BlendMode::LinearLight, 0.5),
            (BlendMode::VividLight, 0.5),
        ];
        for cb in &colours {
            for cs in &colours {
                for mode in &layer_modes {
                    let out = blend_rgb(*mode, *cb, *cs).unwrap();
                    assert!(
                        out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                        "{} {cb:?} {cs:?} -> {out:?}",
                        mode.key()
                    );
                    // Source with zero alpha never changes an opaque backdrop (no silent effect).
                    let none =
                        composite_over(*mode, Rgba::new(*cb, 1.0), Rgba::new(*cs, 0.0), 1.0, site)
                            .unwrap();
                    assert!(
                        close(none.rgb, *cb, 1e-6) && none.a == 1.0,
                        "{} alpha0",
                        mode.key()
                    );
                    // Over a transparent backdrop the blend is ignored: the source shows through.
                    let over =
                        composite_over(*mode, Rgba::TRANSPARENT, Rgba::new(*cs, 1.0), 1.0, site)
                            .unwrap();
                    assert!(
                        close(over.rgb, *cs, 1e-6) && over.a == 1.0,
                        "{} transparent",
                        mode.key()
                    );
                }
                // Normal with an opaque source replaces the backdrop.
                let normal = composite_over(
                    BlendMode::Normal,
                    Rgba::new(*cb, 1.0),
                    Rgba::new(*cs, 1.0),
                    1.0,
                    site,
                )
                .unwrap();
                assert!(close(normal.rgb, *cs, 1e-6));
                for mode in symmetric {
                    assert!(
                        close(
                            blend_rgb(mode, *cb, *cs).unwrap(),
                            blend_rgb(mode, *cs, *cb).unwrap(),
                            1e-6
                        ),
                        "{} not symmetric at {cb:?} {cs:?}",
                        mode.key()
                    );
                }
                // W3C definition: Overlay(Cb, Cs) = HardLight(Cs, Cb).
                assert!(close(
                    blend_rgb(BlendMode::Overlay, *cb, *cs).unwrap(),
                    blend_rgb(BlendMode::HardLight, *cs, *cb).unwrap(),
                    1e-6
                ));
                // Non-separable modes preserve the luminance their definition promises.
                for (mode, from) in [
                    (BlendMode::Hue, cb),
                    (BlendMode::Saturation, cb),
                    (BlendMode::Colour, cb),
                    (BlendMode::Luminosity, cs),
                ] {
                    let out = blend_rgb(mode, *cb, *cs).unwrap();
                    assert!(
                        (lum(out) - lum(*from)).abs() <= 2e-5,
                        "{} luminance at {cb:?} {cs:?}",
                        mode.key()
                    );
                }
                // Darker/lighter colour pick one whole input pixel.
                for mode in [BlendMode::DarkerColour, BlendMode::LighterColour] {
                    let out = blend_rgb(mode, *cb, *cs).unwrap();
                    assert!(out == *cb || out == *cs);
                }
            }
            for (mode, value) in identities {
                let out = blend_rgb(mode, *cb, [value; 3]).unwrap();
                assert!(
                    close(out, *cb, 1e-6),
                    "{} identity {value} at {cb:?} -> {out:?}",
                    mode.key()
                );
            }
            // Colour / Hue / Saturation / Luminosity with the backdrop as source return the backdrop.
            for mode in [
                BlendMode::Hue,
                BlendMode::Saturation,
                BlendMode::Colour,
                BlendMode::Luminosity,
            ] {
                let out = blend_rgb(mode, *cb, *cb).unwrap();
                assert!(
                    close(out, *cb, 2e-5),
                    "{} idempotent at {cb:?} -> {out:?}",
                    mode.key()
                );
            }
        }
    });
}
