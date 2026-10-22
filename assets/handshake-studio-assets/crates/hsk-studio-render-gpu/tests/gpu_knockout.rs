//! Knockout on the GPU backend (lane SMB-PS-ADJUST-STYLES-3): every placement render-cpu runs
//! (shallow in an isolated group, at the plan root and in a pass-through group; deep through nested
//! isolated groups and a pass-through group), checked against render-cpu's hand values
//! (tests/cpu_knockout_2.rs of hsk-studio-render-cpu) and against pigment's `knockout_math`
//! functions composed per pixel for masked, partial-opacity cases (the functions render-cpu
//! calls). Chunk side 3 over a 6 x 2 extent, so every case crosses chunk borders. The
//! extended-range refusal of an out-of-unit sample is render-cpu's. GPU-execution proofs follow
//! `tests/gpu-proof.toml` (V4-03). Photoshop parity UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::knockout_math::{knockout_clear, knockout_over};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, ExtOperator, Group, Knockout,
    Layer, LayerExtension, MaskFormat, MaskInput, NoVector, OutputStage, OutputTarget, PlanExtensions, PlanNode,
    PlanNodeId, PlanSources, RenderBackend, RenderPlan, SampleFormat, SampleRange, SinkError, SourceKey,
    StudioBlendMode, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const PASS: StudioBlendMode = StudioBlendMode::PassThrough;
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const CLEAR: [f32; 4] = [0.0; 4];
const W: u32 = 6;
const H: u32 = 2;
/// Hand values are exact on render-cpu; the GPU divides in binary32 with 1-2.5 ulp.
const TOLERANCE: f32 = 2e-6;

fn tag() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight }
}
fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}
fn extent() -> Rect {
    rect(0, 0, W, H)
}
/// The square every knockout layer covers: columns 2..=3, both rows (chunk side 3 splits it).
fn square() -> Rect {
    rect(2, 0, 2, H)
}
fn solid(r: Rect, rgba: [f32; 4]) -> PlanNode {
    PlanNode::Solid { rect: r, rgba }
}
fn layer(input: u32, mode: StudioBlendMode, opacity: f32, fill: f32) -> Layer {
    Layer { input: PlanNodeId(input), mode, opacity, fill_opacity: fill, mask: None }
}
fn group(isolated: bool, layers: Vec<Layer>) -> PlanNode {
    PlanNode::Group(Group { isolated, layers })
}
fn plan(nodes: Vec<PlanNode>, root: u32) -> RenderPlan<NoVector> {
    RenderPlan {
        revision: 3,
        target: OutputTarget { extent: extent(), format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() },
        nodes,
        root: PlanNodeId(root),
    }
}
fn ko(group: u32, layer: u32, depth: Knockout) -> PlanExtensions {
    PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(group), layer).with_knockout(depth)])
}

/// One coverage_u8 mask over the square: 255, 153 (0.6) on row 0, 51 (0.2), 0 on row 1.
struct Sources {
    mask: Vec<u8>,
}
impl Sources {
    fn new() -> Self {
        Self { mask: vec![255, 153, 51, 0] }
    }
    fn input(&self) -> MaskInput {
        MaskInput {
            key: SourceKey(5),
            placement: square(),
            format: MaskFormat::CoverageU8,
            sha256: hsk_studio_prism::profile_hash(&self.mask),
            inverted: false,
            density: 1.0,
        }
    }
    fn coverage(&self, x: i64, y: i64) -> f32 {
        f32::from(self.mask[(y * 2 + x - 2) as usize]) / 255.0
    }
}
impl PlanSources for Sources {
    fn tile(&self, _key: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        (key == SourceKey(5)).then(|| Mask {
            bounds: square(),
            sample_format: "coverage_u8",
            bytes: &self.mask,
            stride_bytes: 2,
            sha256: hsk_studio_prism::profile_hash(&self.mask),
        })
    }
}

struct Image {
    px: Vec<[f32; 4]>,
    calls: usize,
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y as usize + y) * W as usize + r.x as usize + x;
                self.px[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        self.calls += 1;
        Ok(())
    }
}

fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| GpuBackend::new(DeviceOptions { chunk_side: 3, ..DeviceOptions::default() }))
}

/// `true` when a device is live; without an adapter FAIL (`NOT_PROVEN: no adapter`) under the
/// committed strict policy.
fn gate(what: &str) -> bool {
    let b = backend();
    if b.device_info().is_some() {
        return true;
    }
    if let Some(failure) = b.failure() {
        panic!("{what}: adapter present but the backend failed: {failure:?}");
    }
    let strict = include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1");
    match b.unavailable_reason() {
        Some(Reason::NoAdapter) if strict => panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)"),
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        other => panic!("{what}: no live device ({other:?})"),
    }
}

fn run(p: &RenderPlan<NoVector>, e: &PlanExtensions, sources: &Sources) -> (Result<(), BackendError>, Image) {
    let mut image = Image { px: vec![[f32::NAN; 4]; (W * H) as usize], calls: 0 };
    let request = ExecuteRequest { expected_revision: 3, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 26 };
    let r = backend().execute_extended(p, e, &request, sources, &CancellationToken::default(), &mut image).map(|_| ());
    (r, image)
}

fn assert_near(name: &str, got: &[[f32; 4]], want: &[[f32; 4]], tolerance: f32) -> f32 {
    let mut worst = 0.0f32;
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        // compare premultiplied colour and alpha (a transparent pixel has no colour)
        worst = worst.max((g[3] - w[3]).abs());
        for c in 0..3 {
            worst = worst.max((g[c] * g[3] - w[c] * w[3]).abs());
        }
        assert!(worst <= tolerance, "{name}: pixel {i} got {g:?} want {w:?}");
    }
    println!("GPU_KNOCKOUT {name:<40} max err {worst:e}");
    worst
}

/// The row pattern `[outside, square, square, outside]` of the CPU hand values over the 6 x 2
/// extent: columns 2..=3 take `inside`, the rest `outside`.
fn expect(outside: [f32; 4], inside: [f32; 4]) -> Vec<[f32; 4]> {
    (0..W * H).map(|i| if (2..4).contains(&(i % W)) { inside } else { outside }).collect()
}

/// Nodes 0 red, 1 green (the extent), 2 blue (the square), as in tests/cpu_knockout_2.rs.
fn leaves() -> Vec<PlanNode> {
    vec![solid(extent(), RED), solid(extent(), GREEN), solid(square(), BLUE)]
}

/// One hand-value case: name, plan, envelope, expected pixels.
type Case = (&'static str, RenderPlan<NoVector>, PlanExtensions, Vec<[f32; 4]>);

#[test]
fn gpu_knockout_placements_match_render_cpu_hand_values() {
    if !gate("gpu_knockout_placements_match_render_cpu_hand_values") {
        return;
    }
    let s = Sources::new();
    let black = [0.0, 0.0, 0.0, 1.0];
    let mut cases: Vec<Case> = Vec::new();
    // shallow at the root: fill 0 punches to the transparent base, fill 1 is the blue over it
    for (fill, want) in [(0.0, CLEAR), (1.0, BLUE)] {
        let mut n = leaves();
        n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(2, NORMAL, 1.0, fill)]));
        cases.push(("shallow root", plan(n, 3), ko(3, 1, Knockout::Shallow), expect(RED, want)));
    }
    // multiply in a pass-through group reads the snapshot (red): black; in an isolated group the
    // transparent start: blue
    for (isolated, want) in [(false, black), (true, BLUE)] {
        let mut n = leaves();
        n.push(group(isolated, vec![layer(1, NORMAL, 1.0, 1.0), layer(2, StudioBlendMode::Multiply, 1.0, 1.0)]));
        let mode = if isolated { NORMAL } else { PASS };
        n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(3, mode, 1.0, 1.0)]));
        cases.push(("shallow pass-through / isolated", plan(n, 4), ko(3, 1, Knockout::Shallow), expect(GREEN, want)));
    }
    // fill 0 in the pass-through group: the snapshot (red) shows
    let mut n = leaves();
    n.push(group(false, vec![layer(1, NORMAL, 1.0, 1.0), layer(2, NORMAL, 1.0, 0.0)]));
    n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(3, PASS, 1.0, 1.0)]));
    cases.push(("shallow pass-through fill 0", plan(n, 4), ko(3, 1, Knockout::Shallow), expect(GREEN, RED)));
    // deep in an isolated group clears the root's red; shallow keeps it
    for (depth, want) in [(Knockout::Deep, CLEAR), (Knockout::Shallow, RED)] {
        let mut n = leaves();
        n.push(group(true, vec![layer(1, NORMAL, 1.0, 1.0), layer(2, NORMAL, 1.0, 0.0)]));
        n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(3, NORMAL, 1.0, 1.0)]));
        cases.push(("deep / shallow isolated", plan(n, 4), ko(3, 1, depth), expect(GREEN, want)));
    }
    // deep at opacity 0.5, fill 1 (hand values of cpu_knockout_2)
    let mut n = leaves();
    n.push(group(true, vec![layer(1, NORMAL, 1.0, 1.0), layer(2, NORMAL, 0.5, 1.0)]));
    n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(3, NORMAL, 1.0, 1.0)]));
    cases.push(("deep opacity 0.5", plan(n, 4), ko(3, 1, Knockout::Deep), expect(GREEN, [0.142_857_1, 0.571_428_6, 0.285_714_3, 0.875])));
    // deep through two isolated levels
    let mut n = leaves();
    n.push(group(true, vec![layer(2, NORMAL, 1.0, 0.0)]));
    n.push(group(true, vec![layer(1, NORMAL, 1.0, 1.0), layer(3, NORMAL, 1.0, 1.0)]));
    n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(4, NORMAL, 1.0, 1.0)]));
    cases.push(("deep two isolated levels", plan(n, 5), ko(3, 0, Knockout::Deep), expect(GREEN, CLEAR)));
    // deep through a pass-through group
    let mut n = leaves();
    n.push(group(false, vec![layer(1, NORMAL, 1.0, 1.0), layer(2, NORMAL, 1.0, 0.0)]));
    n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(3, PASS, 1.0, 1.0)]));
    cases.push(("deep pass-through", plan(n, 4), ko(3, 1, Knockout::Deep), expect(GREEN, CLEAR)));
    // a layer above the deep hole composites normally: half-opaque green over transparent
    let mut n = leaves();
    n.push(group(true, vec![layer(1, NORMAL, 1.0, 1.0), layer(2, NORMAL, 1.0, 0.0)]));
    n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(3, NORMAL, 1.0, 1.0), layer(1, NORMAL, 0.5, 1.0)]));
    cases.push(("deep hole under a layer", plan(n, 4), ko(3, 1, Knockout::Deep), expect(GREEN, [0.0, 1.0, 0.0, 0.5])));
    for (name, p, e, want) in &cases {
        let (r, image) = run(p, e, &s);
        r.unwrap_or_else(|err| panic!("{name}: {err:?}"));
        assert_near(name, &image.px, want, TOLERANCE);
    }
}

/// Pigment's per-pixel composition (the functions render-cpu calls) of the two masked cases.
fn reference_masked(s: &Sources, deep: bool, source: [f32; 4], mode: StudioBlendMode, opacity: f32, fill: f32) -> Vec<[f32; 4]> {
    let rgba = |c: [f32; 4]| Rgba::new([c[0], c[1], c[2]], c[3]);
    let out = |r: Rgba| [r.rgb[0], r.rgb[1], r.rgb[2], r.a];
    let green = [0.1, 0.8, 0.3, 0.5];
    (0..W * H)
        .map(|i| {
            let (x, y) = (i64::from(i % W), i64::from(i / W));
            let site = PixelSite { x, y, seed: 0 };
            let inside = (2..4).contains(&x);
            let shape = if inside { (source[3] * s.coverage(x, y) * opacity).clamp(0.0, 1.0) } else { 0.0 };
            let red = rgba(RED);
            if deep {
                // root = [red, isolated G = [green 0.5, source DEEP]]
                let g0 = composite_over(NORMAL, Rgba::TRANSPARENT, rgba(green), 1.0, site).unwrap();
                let g = if shape > 0.0 {
                    knockout_over(mode, g0, Rgba::TRANSPARENT, rgba(source), opacity * fill, shape, site).unwrap()
                } else {
                    g0
                };
                let cleared = if shape > 0.0 { knockout_clear(red, shape).unwrap() } else { red };
                out(composite_over(NORMAL, cleared, g, 1.0, site).unwrap())
            } else {
                // root = [red, pass-through P = [green 0.5, source SHALLOW]]: initial = red
                let running = composite_over(NORMAL, red, rgba(green), 1.0, site).unwrap();
                if shape > 0.0 {
                    out(knockout_over(mode, running, red, rgba(source), opacity * fill, shape, site).unwrap())
                } else {
                    out(running)
                }
            }
        })
        .collect()
}

#[test]
fn gpu_masked_partial_knockout_matches_pigment_knockout_math() {
    if !gate("gpu_masked_partial_knockout_matches_pigment_knockout_math") {
        return;
    }
    let s = Sources::new();
    let source = [0.2, 0.3, 0.9, 0.8];
    for (deep, mode, opacity, fill) in [
        (false, StudioBlendMode::Screen, 0.7, 0.4),
        (false, StudioBlendMode::Multiply, 1.0, 0.0),
        (true, StudioBlendMode::Multiply, 0.6, 0.5),
        (true, StudioBlendMode::Overlay, 0.9, 1.0),
    ] {
        let mut n = vec![solid(extent(), RED), solid(extent(), [0.1, 0.8, 0.3, 0.5]), solid(square(), source)];
        let masked = Layer { mask: Some(s.input()), ..layer(2, mode, opacity, fill) };
        n.push(group(deep, vec![layer(1, NORMAL, 1.0, 1.0), masked]));
        n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(3, if deep { NORMAL } else { PASS }, 1.0, 1.0)]));
        let depth = if deep { Knockout::Deep } else { Knockout::Shallow };
        let (r, image) = run(&plan(n, 4), &ko(3, 1, depth), &s);
        let name = format!("{depth:?} {mode:?} {opacity} {fill} masked");
        r.unwrap_or_else(|err| panic!("{name}: {err:?}"));
        assert_near(&name, &image.px, &reference_masked(&s, deep, source, mode, opacity, fill), 2e-6);
    }
}

#[test]
fn gpu_knockout_capability_refusals_and_extended_overflow() {
    if !gate("gpu_knockout_capability_refusals_and_extended_overflow") {
        return;
    }
    let b = backend();
    for depth in [Knockout::Shallow, Knockout::Deep] {
        assert_eq!(b.extension_exactness(ExtOperator::Knockout(depth)), Exactness::Approximate);
    }
    let s = Sources::new();
    // pigment's plain-ancestry refusal holds on this backend too: the deep group used twice
    let mut n = leaves();
    n.push(group(true, vec![layer(1, NORMAL, 1.0, 1.0), layer(2, NORMAL, 1.0, 0.0)]));
    n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(3, NORMAL, 1.0, 1.0), layer(3, NORMAL, 0.5, 1.0)]));
    let (r, image) = run(&plan(n, 4), &ko(3, 1, Knockout::Deep), &s);
    assert!(matches!(r, Err(BackendError::Unsupported { operator: "knockout_deep", .. })), "{r:?}");
    assert_eq!(image.calls, 0);
    // extended range: a knockout source outside [0, 1] under a non-zero shape is render-cpu's
    // refusal (knockout_over's composite_over sees an out-of-unit sample), not a silent clamp
    let e = ko(3, 1, Knockout::Shallow).with_output(OutputStage::new(SampleRange::Extended, None));
    let mut n = vec![solid(extent(), RED), solid(extent(), GREEN), solid(square(), [2.0, 0.5, 0.5, 1.0])];
    n.push(group(true, vec![layer(1, NORMAL, 1.0, 1.0), layer(2, NORMAL, 1.0, 1.0)]));
    n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(3, NORMAL, 1.0, 1.0)]));
    let (r, _) = run(&plan(n, 4), &e, &s);
    assert!(matches!(r, Err(BackendError::Unsupported { operator: "sample_range_outside_unit", .. })), "{r:?}");
    // the same plan in range renders (the knockout passes keep the unit form in an extended plan)
    let mut n = leaves();
    n.push(group(true, vec![layer(1, NORMAL, 1.0, 1.0), layer(2, StudioBlendMode::LinearDodgeAdd, 1.0, 1.0)]));
    n.push(group(true, vec![layer(0, NORMAL, 1.0, 1.0), layer(3, NORMAL, 1.0, 1.0)]));
    let (r, image) = run(&plan(n, 4), &e, &s);
    r.unwrap();
    assert_near("extended in range", &image.px, &expect(GREEN, BLUE), TOLERANCE);
}
