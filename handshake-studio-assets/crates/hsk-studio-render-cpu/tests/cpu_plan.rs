use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Length, Unit};
use hsk_studio_nib::{Anchor, Mirroring, Point, Winding};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::{Grid, Mask, Rect, wire};
use hsk_studio_prism::profile_hash;
use hsk_studio_render_cpu::plan_sink::SCRATCH_BYTES_PER_PIXEL;
use hsk_studio_render_cpu::*;
use std::time::{Duration, Instant};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost 30s deadline: {e}"),
    }
}

// ---- fixtures --------------------------------------------------------------------------------

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}
fn tag() -> ColourTag {
    ColourTag {
        profile_sha256: profile_hash(b"working-profile"),
        blend_space: BlendSpace::LinearLight,
    }
}
fn target(extent: Rect, space: ColourTag) -> OutputTarget {
    OutputTarget {
        extent,
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space,
    }
}
fn plan<V>(extent: Rect, nodes: Vec<PlanNode<V>>, root: u32) -> RenderPlan<V> {
    RenderPlan {
        revision: 7,
        target: target(extent, tag()),
        nodes,
        root: PlanNodeId(root),
    }
}
fn layer(input: u32, mode: StudioBlendMode, opacity: f32) -> Layer {
    Layer {
        input: PlanNodeId(input),
        mode,
        opacity,
        fill_opacity: 1.0,
        mask: None,
    }
}
fn group<V>(isolated: bool, layers: Vec<Layer>) -> PlanNode<V> {
    PlanNode::Group(Group { isolated, layers })
}
fn solid<V>(rect: Rect, rgba: [f32; 4]) -> PlanNode<V> {
    PlanNode::Solid { rect, rgba }
}
fn one(planes: TilePlanes<'_>) -> Sources<'_> {
    Sources {
        tiles: vec![(1, planes)],
        masks: vec![],
    }
}
fn req(min: Exactness) -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 7,
        min_exactness: min,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}
fn px(x: f64, y: f64) -> Point {
    Point {
        x: Length::new(x, Unit::Pixels).unwrap(),
        y: Length::new(y, Unit::Pixels).unwrap(),
    }
}
fn anchor(position: Point, incoming: Point, outgoing: Point) -> Anchor {
    Anchor {
        position,
        incoming,
        outgoing,
        handle_mirroring: Mirroring::None,
    }
}
fn vector(points: &[(f64, f64)], winding: Winding, aa_samples: u8) -> PlanNode<VectorFill> {
    PlanNode::Vector(VectorFill {
        anchors: points
            .iter()
            .map(|&(x, y)| anchor(px(x, y), px(0.0, 0.0), px(0.0, 0.0)))
            .collect(),
        closed: true,
        winding,
        rgba: [1.0, 0.0, 0.0, 1.0],
        aa_samples,
        pixels_per_inch: None,
    })
}

fn white(mut node: PlanNode<VectorFill>) -> PlanNode<VectorFill> {
    if let PlanNode::Vector(v) = &mut node {
        v.rgba = [1.0; 4];
    }
    node
}

/// Owned planes for a source tile at the working tag.
struct TileData {
    width: u32,
    height: u32,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl TileData {
    fn new(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..height {
            for x in 0..width {
                let p = pixel(x, y);
                for v in &p[..3] {
                    colour.extend_from_slice(&v.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self {
            width,
            height,
            colour,
            alpha,
        }
    }
    fn planes(&self) -> TilePlanes<'_> {
        TilePlanes {
            width: self.width,
            height: self.height,
            colour: &self.colour,
            colour_stride_bytes: u64::from(self.width) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(self.width) * 4,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        }
    }
    fn input(&self, key: u32, x: i64, y: i64) -> TileInput {
        TileInput {
            key: SourceKey(key),
            placement: rect(x, y, self.width, self.height),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
        }
    }
}
struct MaskData {
    bounds: Rect,
    wide: bool,
    bytes: Vec<u8>,
}
impl MaskData {
    fn u8(bounds: Rect, value: impl Fn(i64, i64) -> u8) -> Self {
        let mut bytes = Vec::new();
        for y in bounds.y..bounds.y + i64::from(bounds.height) {
            for x in bounds.x..bounds.x + i64::from(bounds.width) {
                bytes.push(value(x, y));
            }
        }
        Self {
            bounds,
            wide: false,
            bytes,
        }
    }
    fn u16(bounds: Rect, values: &[u16]) -> Self {
        Self {
            bounds,
            wide: true,
            bytes: values.iter().flat_map(|v| v.to_le_bytes()).collect(),
        }
    }
    fn format(&self) -> MaskFormat {
        if self.wide {
            MaskFormat::CoverageU16Le
        } else {
            MaskFormat::CoverageU8
        }
    }
    fn input(&self, key: u32, inverted: bool) -> MaskInput {
        MaskInput {
            key: SourceKey(key),
            placement: self.bounds,
            format: self.format(),
            sha256: profile_hash(&self.bytes),
            inverted,
            density: 1.0,
        }
    }
}
#[derive(Default)]
struct Sources<'a> {
    tiles: Vec<(u32, TilePlanes<'a>)>,
    masks: Vec<(u32, &'a MaskData)>,
}
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        self.tiles.iter().find(|(k, _)| *k == key.0).map(|(_, t)| *t)
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        let (_, m) = self.masks.iter().find(|(k, _)| *k == key.0)?;
        Some(Mask {
            bounds: m.bounds,
            sample_format: m.format().key(),
            bytes: &m.bytes,
            stride_bytes: u64::from(m.bounds.width) * if m.wide { 2 } else { 1 },
            sha256: profile_hash(&m.bytes),
        })
    }
}

/// Assembles chunks into full-extent planes and counts writes per pixel.
struct Collect {
    extent: Rect,
    colour: Vec<u8>,
    alpha: Vec<u8>,
    writes: Vec<u8>,
    rects: Vec<Rect>,
    calls: usize,
    fail_at: Option<usize>,
    cancel_at: Option<(usize, CancellationToken)>,
}
impl Collect {
    fn new(extent: Rect) -> Self {
        let n = extent.width as usize * extent.height as usize;
        Self {
            extent,
            colour: vec![0; n * 12],
            alpha: vec![0; n * 4],
            writes: vec![0; n],
            rects: vec![],
            calls: 0,
            fail_at: None,
            cancel_at: None,
        }
    }
    fn index(&self, x: i64, y: i64) -> usize {
        (y - self.extent.y) as usize * self.extent.width as usize + (x - self.extent.x) as usize
    }
    fn pixel(&self, x: i64, y: i64) -> [f32; 4] {
        let i = self.index(x, y);
        let f = |bytes: &[u8], at: usize| {
            f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        [
            f(&self.colour, i * 12),
            f(&self.colour, i * 12 + 4),
            f(&self.colour, i * 12 + 8),
            f(&self.alpha, i * 4),
        ]
    }
}
impl TileSink for Collect {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let call = self.calls;
        self.calls += 1;
        if self.fail_at == Some(call) {
            return Err(SinkError::Rejected);
        }
        let (w, h) = (r.width as usize, r.height as usize);
        assert_eq!(colour.len(), w * h * 12, "tightly packed colour plane");
        assert_eq!(alpha.len(), w * h * 4, "tightly packed alpha plane");
        for row in 0..h {
            for col in 0..w {
                let i = self.index(r.x + col as i64, r.y + row as i64);
                let s = row * w + col;
                self.colour[i * 12..i * 12 + 12].copy_from_slice(&colour[s * 12..s * 12 + 12]);
                self.alpha[i * 4..i * 4 + 4].copy_from_slice(&alpha[s * 4..s * 4 + 4]);
                self.writes[i] += 1;
            }
        }
        self.rects.push(r);
        if let Some((at, token)) = &self.cancel_at
            && *at == call
        {
            token.cancel();
        }
        Ok(())
    }
}

fn close(got: [f32; 4], want: [f32; 4]) {
    for i in 0..4 {
        assert!(
            (got[i] - want[i]).abs() < 1e-6,
            "channel {i}: got {got:?} want {want:?}"
        );
    }
}
fn run<V: VectorSource + serde::Serialize + Send + Sync>(
    renderer: &Renderer,
    plan: &RenderPlan<V>,
    min: Exactness,
    sources: &dyn PlanSources,
    sink: &mut Collect,
) -> Result<RenderReceipt, BackendError> {
    renderer.execute(plan, &req(min), sources, &CancellationToken::default(), sink)
}
fn render<V: VectorSource + serde::Serialize + Send + Sync>(
    plan: &RenderPlan<V>,
    chunk: u32,
    sources: &dyn PlanSources,
) -> Collect {
    let mut sink = Collect::new(plan.target.extent);
    run(
        &Renderer::new(chunk).unwrap(),
        plan,
        Exactness::Approximate,
        sources,
        &mut sink,
    )
    .unwrap();
    sink
}

// ---- tests -----------------------------------------------------------------------------------

#[test]
fn known_alpha_mask_sample_math() {
    bounded_case("known_alpha_mask_sample_math", || {
        // Hand-computed through the backend (f32 per pigment blend_math; tolerance 1e-6 where the
        // old binary64 oracle was exact to one rounding): backdrop (.2,.4,.6,1), red, opacity .5.
        let one = rect(0, 0, 1, 1);
        for (mode, want) in [
            (StudioBlendMode::Normal, [0.6, 0.2, 0.3, 1.0]),
            (StudioBlendMode::Multiply, [0.2, 0.2, 0.3, 1.0]),
            // Screen: B = (1, .4, .6); .5*cb + .5*B.
            (StudioBlendMode::Screen, [0.6, 0.4, 0.6, 1.0]),
        ] {
            let p: RenderPlan = plan(
                one,
                vec![
                    solid(one, [0.2, 0.4, 0.6, 1.0]),
                    solid(one, [1.0, 0.0, 0.0, 1.0]),
                    group(
                        true,
                        vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, mode, 0.5)],
                    ),
                ],
                2,
            );
            let sink = render(&p, 1, &Sources::default());
            close(sink.pixel(0, 0), want);
        }
        // fill_opacity folds in as a product: .5 * .5 = .25 -> R .75*.2+.25*1 = .4, G .3, B .45.
        let mut l = layer(1, StudioBlendMode::Normal, 0.5);
        l.fill_opacity = 0.5;
        let p: RenderPlan = plan(
            one,
            vec![
                solid(one, [0.2, 0.4, 0.6, 1.0]),
                solid(one, [1.0, 0.0, 0.0, 1.0]),
                group(true, vec![layer(0, StudioBlendMode::Normal, 1.0), l]),
            ],
            2,
        );
        close(render(&p, 1, &Sources::default()).pixel(0, 0), [0.4, 0.3, 0.45, 1.0]);
        // Transparent source adds nothing; opacity 0 adds nothing.
        for (a, o) in [(0.0_f32, 1.0_f32), (1.0, 0.0)] {
            let p: RenderPlan = plan(
                one,
                vec![
                    solid(one, [0.2, 0.4, 0.6, 1.0]),
                    solid(one, [1.0, 0.0, 0.0, a]),
                    group(
                        true,
                        vec![
                            layer(0, StudioBlendMode::Normal, 1.0),
                            layer(1, StudioBlendMode::Multiply, o),
                        ],
                    ),
                ],
                2,
            );
            let sink = render(&p, 1, &Sources::default());
            let [r, g, b, alpha] = sink.pixel(0, 0);
            assert_eq!([r, g, b, alpha].map(f32::to_bits), [0.2_f32, 0.4, 0.6, 1.0].map(f32::to_bits));
        }

        // Mask plane as coverage (u8: v/255; inverted: 1-c; u16: v/65535).
        let row = rect(0, 0, 4, 1);
        let m8 = MaskData::u8(row, |x, _| [0, 51, 255, 128][x as usize]);
        let m16 = MaskData::u16(row, &[65535, 0, 32768, 0]);
        let mut masked = layer(1, StudioBlendMode::Normal, 1.0);
        masked.mask = Some(m8.input(5, false));
        let mut inverted = layer(1, StudioBlendMode::Normal, 1.0);
        inverted.mask = Some(m8.input(5, true));
        let mut wide = layer(1, StudioBlendMode::Normal, 1.0);
        wide.mask = Some(m16.input(6, false));
        let sources = Sources {
            tiles: vec![],
            masks: vec![(5, &m8), (6, &m16)],
        };
        let lerp = |c: f32| [0.5 * (1.0 - c) + c, 0.5 * (1.0 - c), 0.5 * (1.0 - c), 1.0];
        for (l, coverage) in [
            (masked, [0.0, 51.0 / 255.0, 1.0, 128.0 / 255.0]),
            (inverted, [1.0, 1.0 - 51.0 / 255.0, 0.0, 1.0 - 128.0 / 255.0]),
            (wide, [1.0, 0.0, 32768.0 / 65535.0, 0.0]),
        ] {
            let p: RenderPlan = plan(
                row,
                vec![
                    solid(row, [0.5, 0.5, 0.5, 1.0]),
                    solid(row, [1.0, 0.0, 0.0, 1.0]),
                    group(true, vec![layer(0, StudioBlendMode::Normal, 1.0), l]),
                ],
                2,
            );
            let sink = render(&p, 3, &sources);
            for (x, c) in coverage.into_iter().enumerate() {
                close(sink.pixel(x as i64, 0), lerp(c));
            }
        }

        // Path coverage as the alpha mask: rect x in [0.5, 3], y in [0, 3] on a 5x4 extent, as a
        // leaf root (one Normal layer over transparent black).
        let extent = rect(0, 0, 5, 4);
        let p = plan(
            extent,
            vec![vector(
                &[(0.5, 0.0), (3.0, 0.0), (3.0, 3.0), (0.5, 3.0)],
                Winding::NonZero,
                4,
            )],
            0,
        );
        let sink = render(&p, 3, &Sources::default());
        for y in 0..4 {
            for x in 0..5 {
                let coverage = match (x, y) {
                    (_, 3) => 0.0,
                    (0, _) => 0.5,
                    (1 | 2, _) => 1.0,
                    _ => 0.0,
                };
                let want = if coverage > 0.0 {
                    [1.0, 0.0, 0.0, coverage]
                } else {
                    [0.0; 4]
                };
                close(sink.pixel(x, y), want);
            }
        }

        // Winding rule: pentagram, centre winding 2 -> NonZero filled, EvenOdd hole.
        let star: Vec<(f64, f64)> = [0_usize, 2, 4, 1, 3]
            .iter()
            .map(|k| {
                let angle = (-90.0_f64 + 72.0 * *k as f64).to_radians();
                (10.0 + 8.0 * angle.cos(), 10.0 + 8.0 * angle.sin())
            })
            .collect();
        let extent = rect(0, 0, 20, 20);
        for (winding, want) in [(Winding::NonZero, 1.0), (Winding::EvenOdd, 0.0)] {
            let sink = render(&plan(extent, vec![vector(&star, winding, 4)], 0), 8, &Sources::default());
            assert_eq!(sink.pixel(10, 10)[3], want, "{winding:?} centre");
            assert_eq!(sink.pixel(9, 9)[3], want, "{winding:?} centre");
            assert_eq!(sink.pixel(0, 0)[3], 0.0, "{winding:?} outside");
        }

        // Curves: four cubics (kappa = 0.5522847498, handles relative to anchors) approximate a
        // circle of r = 8 at (10, 10); summed coverage equals pi r^2 within sampling error.
        let (r, k) = (8.0_f64, 0.552_284_749_8 * 8.0);
        let circle = PlanNode::Vector(VectorFill {
            anchors: vec![
                anchor(px(10.0, 2.0), px(-k, 0.0), px(k, 0.0)),
                anchor(px(18.0, 10.0), px(0.0, -k), px(0.0, k)),
                anchor(px(10.0, 18.0), px(k, 0.0), px(-k, 0.0)),
                anchor(px(2.0, 10.0), px(0.0, k), px(0.0, -k)),
            ],
            closed: true,
            winding: Winding::NonZero,
            rgba: [1.0, 0.0, 0.0, 1.0],
            aa_samples: 8,
            pixels_per_inch: None,
        });
        let sink = render(&plan(extent, vec![circle], 0), 7, &Sources::default());
        let area: f64 = (0..20)
            .flat_map(|y| (0..20).map(move |x| (x, y)))
            .map(|(x, y)| f64::from(sink.pixel(x, y)[3]))
            .sum();
        let want = std::f64::consts::PI * r * r;
        assert!((area - want).abs() < 1.0, "circle area {area} vs {want}");
        assert_eq!(sink.pixel(10, 10), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(sink.pixel(0, 0)[3], 0.0);
    });
}

#[test]
fn chunk_boundary_equals_independent_reference() {
    bounded_case("chunk_boundary_equals_independent_reference", || {
        let extent = rect(-2, 1, 5, 3);
        let tile_rgb = |tx: u32, ty: u32| {
            [
                (tx + 1) as f32 / 8.0,
                (ty + 1) as f32 / 4.0,
                (tx + 2 * ty) as f32 / 8.0,
            ]
        };
        let tile = TileData::new(3, 2, |x, y| {
            let c = tile_rgb(x, y);
            [c[0], c[1], c[2], 0.5]
        });
        let checker = MaskData::u8(extent, |x, y| if (x + y).rem_euclid(2) == 1 { 255 } else { 0 });
        let (r1, r2) = (rect(-2, 1, 3, 2), rect(0, 2, 3, 2));
        let (a1, b2) = ([0.5_f32, 0.25, 0.125], [0.75_f32, 0.5, 0.25]);
        let base = [0.25_f32, 0.5, 0.75];
        let mut masked_group = layer(6, StudioBlendMode::Screen, 0.5);
        masked_group.mask = Some(checker.input(9, false));
        let p: RenderPlan<VectorFill> = plan(
            extent,
            vec![
                solid(extent, [base[0], base[1], base[2], 1.0]),
                solid(rect(-1, 1, 3, 2), [0.5, 0.25, 1.0, 0.5]),
                PlanNode::Tile(tile.input(3, 0, 2)),
                white(vector(
                    &[(0.5, 1.5), (2.5, 1.5), (2.5, 3.5), (0.5, 3.5)],
                    Winding::NonZero,
                    4,
                )),
                solid(r1, [a1[0], a1[1], a1[2], 1.0]),
                solid(r2, [b2[0], b2[1], b2[2], 0.5]),
                group(
                    true,
                    vec![
                        layer(4, StudioBlendMode::Normal, 1.0),
                        layer(5, StudioBlendMode::Multiply, 1.0),
                    ],
                ),
                group(
                    true,
                    vec![
                        layer(0, StudioBlendMode::Normal, 1.0),
                        layer(1, StudioBlendMode::Multiply, 0.5),
                        layer(2, StudioBlendMode::Screen, 1.0),
                        layer(3, StudioBlendMode::Difference, 1.0),
                        masked_group,
                    ],
                ),
            ],
            7,
        );
        let sources = Sources {
            tiles: vec![(3, tile.planes())],
            masks: vec![(9, &checker)],
        };

        // Independent reference: opaque backdrop, so each layer is (1-as)*cb + as*B(cb, cs); path
        // coverage is the analytic pixel/rect overlap; the isolated group is folded by hand.
        // Dyadic inputs keep every step exact in f32, so the planes must be byte-equal.
        let step = |c: &mut [f64; 3], cs: [f64; 3], a: f64, b: fn(f64, f64) -> f64| {
            for i in 0..3 {
                c[i] = (1.0 - a) * c[i] + a * b(c[i], cs[i]);
            }
        };
        let inside = |r: Rect, x: i64, y: i64| {
            x >= r.x && x < r.x + i64::from(r.width) && y >= r.y && y < r.y + i64::from(r.height)
        };
        let overlap = |p: i64, lo: f64, hi: f64| {
            ((p as f64 + 1.0).min(hi) - (p as f64).max(lo)).clamp(0.0, 1.0)
        };
        let (mut want_colour, mut want_alpha) = (Vec::new(), Vec::new());
        for y in extent.y..extent.y + 3 {
            for x in extent.x..extent.x + 5 {
                let mut c = base.map(f64::from);
                if inside(rect(-1, 1, 3, 2), x, y) {
                    step(&mut c, [0.5, 0.25, 1.0], 0.25, |cb, cs| cb * cs);
                }
                if inside(rect(0, 2, 3, 2), x, y) {
                    let t = tile_rgb(x as u32, (y - 2) as u32).map(f64::from);
                    step(&mut c, t, 0.5, |cb, cs| cb + cs - cb * cs);
                }
                let cov = overlap(x, 0.5, 2.5) * overlap(y, 1.5, 3.5);
                step(&mut c, [1.0; 3], cov, |cb, cs| (cb - cs).abs());
                // Isolated group G over transparent black.
                let (mut gc, mut ga) = ([0.0_f64; 3], 0.0_f64);
                if inside(r1, x, y) {
                    gc = a1.map(f64::from);
                    ga = 1.0;
                }
                if inside(r2, x, y) {
                    if ga == 0.0 {
                        gc = b2.map(f64::from);
                        ga = 0.5;
                    } else {
                        step(&mut gc, b2.map(f64::from), 0.5, |cb, cs| cb * cs);
                    }
                }
                if ga > 0.0 && (x + y).rem_euclid(2) == 1 {
                    step(&mut c, gc, ga * 0.5, |cb, cs| cb + cs - cb * cs);
                }
                for v in c {
                    want_colour.extend_from_slice(&(v as f32).to_le_bytes());
                }
                want_alpha.extend_from_slice(&1.0_f32.to_le_bytes());
            }
        }
        for (chunk, chunks) in [(1_u32, 15_u64), (2, 6), (3, 2), (4, 2), (64, 1)] {
            let renderer = Renderer::new(chunk).unwrap();
            let mut sink = Collect::new(extent);
            let receipt = run(&renderer, &p, Exactness::Approximate, &sources, &mut sink).unwrap();
            assert_eq!(receipt.chunks, chunks, "chunk {chunk}");
            assert_eq!(receipt.nodes_executed, 8);
            assert!(sink.writes.iter().all(|w| *w == 1), "each pixel exactly once");
            assert!(sink.rects.iter().all(|r| r.width <= chunk && r.height <= chunk));
            assert_eq!(sink.colour, want_colour, "colour plane, chunk {chunk}");
            assert_eq!(sink.alpha, want_alpha, "alpha plane, chunk {chunk}");
            assert_eq!(renderer.live_scratch_bytes(), 0);
        }

        // Pigment canonical tile through its own adapter (`ResolvedTile::planes`).
        let px_colour: Vec<u8> = [
            [0.5_f32, 0.25, 0.125],
            [1.0, 0.0, 0.5],
            [0.0, 0.75, 0.25],
            [0.125, 0.5, 1.0],
        ]
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
        let px_alpha: Vec<u8> = [1.0_f32, 0.5, 0.25, 1.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let profile_bytes = b"scpf-profile-bytes".to_vec();
        let profile_id = DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000003").unwrap();
        let wire_tile: wire::Tile = serde_json::from_value(serde_json::json!({
            "tile_ref": {
                "object_key": "tile-0-0", "layer_id": "SLYR-019abcde-0000-7000-8000-000000000001",
                "column": 0, "row": 0,
                "width": {"value": 2.0, "unit": "px"}, "height": {"value": 2.0, "unit": "px"},
                "format": "rgb_f32le", "colour_profile_id": profile_id.as_str(),
                "schema_id": "hsk.studio.raster_tile@1",
                "content_digest": {"algorithm": "sha256", "digest": "00"},
                "artifact_manifest_id": "manifest-1"
            },
            "bounds": {"x": 0, "y": 0, "width": 2, "height": 2},
            "sample_format": "rgb_f32le",
            "colour_path": "-", "colour_stride_bytes": 24, "colour_sha256": wire::hex(&profile_hash(&px_colour)),
            "alpha_path": "-", "alpha_stride_bytes": 8, "alpha_sha256": wire::hex(&profile_hash(&px_alpha)),
            "transfer": "linear_light", "alpha_association": "straight",
            "profile_path": "-", "profile_sha256": wire::hex(&profile_hash(&profile_bytes))
        }))
        .unwrap();
        let loaded = wire::LoadedTile {
            colour: px_colour.clone(),
            alpha: px_alpha.clone(),
            profile: profile_bytes.clone(),
            id: profile_id,
        };
        let resolved = loaded.borrowed(&wire_tile).unwrap();
        resolved
            .validate_shape(Grid {
                origin_x: 0,
                origin_y: 0,
                tile_width: 2,
                tile_height: 2,
            })
            .unwrap();
        let planes = resolved.planes().unwrap();
        let extent = rect(0, 0, 4, 4);
        let mut p: RenderPlan = RenderPlan {
            revision: 7,
            target: target(extent, planes.space),
            nodes: vec![
                PlanNode::Tile(TileInput {
                    key: SourceKey(0),
                    placement: rect(1, 1, 2, 2),
                    format: SampleFormat::RgbF32Le,
                    alpha: AlphaConvention::Straight,
                    colour_sha256: planes.colour_sha256,
                    alpha_sha256: planes.alpha_sha256,
                }),
                group(true, vec![layer(0, StudioBlendMode::Normal, 1.0)]),
            ],
            root: PlanNodeId(1),
        };
        let sources = Sources {
            tiles: vec![(0, planes)],
            masks: vec![],
        };
        let sink = render(&p, 3, &sources);
        close(sink.pixel(1, 1), [0.5, 0.25, 0.125, 1.0]);
        close(sink.pixel(2, 1), [1.0, 0.0, 0.5, 0.5]);
        close(sink.pixel(1, 2), [0.0, 0.75, 0.25, 0.25]);
        close(sink.pixel(2, 2), [0.125, 0.5, 1.0, 1.0]);
        assert_eq!(sink.pixel(0, 0), [0.0; 4]);
        assert_eq!(sink.pixel(3, 3), [0.0; 4]);
        // A plan whose working tag is not the tile's tag is refused, not converted.
        p.target.space.profile_sha256 = profile_hash(b"another-profile");
        let mut sink = Collect::new(extent);
        let error = Renderer::new(3)
            .unwrap()
            .execute_detailed(&p, &req(Exactness::Exact), &sources, &CancellationToken::default(), &mut sink)
            .unwrap_err();
        assert_eq!(
            error,
            RenderError::Source {
                key: SourceKey(0),
                fault: SourceFault::Declared
            }
        );
        assert_eq!(sink.calls, 0);
    });
}

#[test]
fn sink_error_cancel_releases_bytes() {
    bounded_case("sink_error_cancel_releases_bytes", || {
        let extent = rect(0, 0, 5, 3);
        let p: RenderPlan = plan(
            extent,
            vec![
                solid(extent, [0.5, 0.5, 0.5, 1.0]),
                group(true, vec![layer(0, StudioBlendMode::Normal, 1.0)]),
            ],
            1,
        );
        let renderer = Renderer::new(2).unwrap();
        let token = CancellationToken::default();
        let none = Sources::default();
        let go = |renderer: &Renderer, request: &ExecuteRequest, token: &CancellationToken, sink: &mut Collect| {
            renderer.execute(&p, request, &none, token, sink)
        };

        // Sink fails on its second chunk: typed error, no receipt, scratch released.
        let mut sink = Collect::new(extent);
        sink.fail_at = Some(1);
        assert_eq!(
            go(&renderer, &req(Exactness::Exact), &token, &mut sink).unwrap_err(),
            BackendError::Sink(SinkError::Rejected)
        );
        assert_eq!(sink.calls, 2, "aborted at the failing chunk");
        assert_eq!(renderer.live_scratch_bytes(), 0);
        assert_eq!(renderer.peak_scratch_bytes(), 4 * SCRATCH_BYTES_PER_PIXEL);

        // Pre-cancelled: nothing reaches the sink.
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        let mut sink = Collect::new(extent);
        assert_eq!(
            go(&renderer, &req(Exactness::Exact), &cancelled, &mut sink).unwrap_err(),
            BackendError::Canceled
        );
        assert_eq!(sink.calls, 0);
        assert_eq!(renderer.live_scratch_bytes(), 0);

        // Cancelled by the sink after chunk 2: observed before chunk 3, not mid-chunk.
        let mid = CancellationToken::default();
        let mut sink = Collect::new(extent);
        sink.cancel_at = Some((1, mid.clone()));
        assert_eq!(
            go(&renderer, &req(Exactness::Exact), &mid, &mut sink).unwrap_err(),
            BackendError::Canceled
        );
        assert_eq!(sink.calls, 2);
        assert_eq!(renderer.live_scratch_bytes(), 0);

        // Deadline in the past is a typed error before the first chunk.
        let mut late = req(Exactness::Exact);
        late.deadline = Instant::now().checked_sub(Duration::from_secs(1));
        let mut sink = Collect::new(extent);
        assert_eq!(
            go(&renderer, &late, &token, &mut sink).unwrap_err(),
            BackendError::DeadlineExceeded
        );
        assert_eq!(sink.calls, 0);

        // Budget is enforced before allocation: 4 px x 32 B = 128 needed.
        let tight = Renderer::new(2).unwrap();
        let mut small = req(Exactness::Exact);
        small.max_working_bytes = 127;
        let mut sink = Collect::new(extent);
        assert_eq!(
            go(&tight, &small, &token, &mut sink).unwrap_err(),
            BackendError::BudgetExceeded {
                requested: 128,
                limit: 127
            }
        );
        assert_eq!((sink.calls, tight.live_scratch_bytes(), tight.peak_scratch_bytes()), (0, 0, 0));

        // Each nested isolated group adds one RGBA scratch buffer: depth 2 -> 4 px x (32 + 2*16).
        let nested: RenderPlan = plan(
            extent,
            vec![
                solid(extent, [0.5, 0.5, 0.5, 1.0]),
                group(true, vec![layer(0, StudioBlendMode::Normal, 1.0)]),
                group(true, vec![layer(1, StudioBlendMode::Normal, 1.0)]),
                group(true, vec![layer(2, StudioBlendMode::Normal, 1.0)]),
            ],
            3,
        );
        let deep = Renderer::new(2).unwrap();
        let mut sink = Collect::new(extent);
        let receipt = deep.execute(&nested, &req(Exactness::Exact), &none, &token, &mut sink).unwrap();
        assert_eq!(receipt.peak_working_bytes, 4 * (32 + 2 * 16));
        assert_eq!(deep.live_scratch_bytes(), 0);
        assert_eq!(sink.pixel(4, 2), [0.5, 0.5, 0.5, 1.0]);

        assert_eq!(Renderer::new(0).err(), Some(RenderError::InvalidConfig("chunk")));
        assert_eq!(
            Renderer::new(MAX_CHUNK_SIDE + 1).err(),
            Some(RenderError::InvalidConfig("chunk"))
        );
    });
}

#[test]
fn unsupported_op_stale_result() {
    bounded_case("unsupported_op_stale_result", || {
        let extent = rect(0, 0, 2, 2);
        let renderer = Renderer::new(2).unwrap();
        let none = Sources::default();
        let check = |p: &RenderPlan, min: Exactness, sources: &dyn PlanSources| {
            let mut sink = Collect::new(extent);
            let result = renderer.execute_detailed(p, &req(min), sources, &CancellationToken::default(), &mut sink);
            assert_eq!(sink.calls, 0, "refused before the first sink call: {result:?}");
            assert_eq!(renderer.live_scratch_bytes(), 0);
            result.unwrap_err()
        };
        let backend = |e: BackendError| RenderError::Backend(e);
        let unsupported = |operator: &'static str, exactness: Exactness| {
            backend(BackendError::Unsupported { operator, exactness })
        };
        let two = |mode: StudioBlendMode, opacity: f32| -> RenderPlan {
            plan(
                extent,
                vec![
                    solid(extent, [0.5, 0.5, 0.5, 1.0]),
                    solid(extent, [1.0, 0.0, 0.0, 1.0]),
                    group(
                        true,
                        vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, mode, opacity)],
                    ),
                ],
                2,
            )
        };

        // Stale revision.
        let ok = two(StudioBlendMode::Multiply, 1.0);
        let mut stale = req(Exactness::Exact);
        stale.expected_revision = 8;
        let mut sink = Collect::new(extent);
        assert_eq!(
            renderer.execute(&ok, &stale, &none, &CancellationToken::default(), &mut sink).unwrap_err(),
            BackendError::StaleRevision { expected: 8, found: 7 }
        );
        assert_eq!(sink.calls, 0);

        // Modes: community-formula mode needs a relaxed floor, no-function mode is never run,
        // tool-only mode is an invalid layer (STU-RAS-155) - never silently Normal.
        let hard_mix = two(StudioBlendMode::HardMix, 1.0);
        assert_eq!(
            check(&hard_mix, Exactness::Exact, &none),
            unsupported("hard_mix", Exactness::Approximate)
        );
        let mut sink = Collect::new(extent);
        run(&renderer, &hard_mix, Exactness::Approximate, &none, &mut sink).unwrap();
        assert_eq!(
            check(&two(StudioBlendMode::Average, 1.0), Exactness::Approximate, &none),
            unsupported("average", Exactness::Unsupported)
        );
        assert!(matches!(
            check(&two(StudioBlendMode::Behind, 1.0), Exactness::Approximate, &none),
            RenderError::Backend(BackendError::InvalidPlan(PlanError::ModeNotApplicable { .. }))
        ));
        // fill_opacity < 1 on a non-Normal mode is Approximate (unverified Photoshop semantics).
        let mut fill = layer(1, StudioBlendMode::Multiply, 1.0);
        fill.fill_opacity = 0.5;
        let filled: RenderPlan = plan(
            extent,
            vec![
                solid(extent, [0.5, 0.5, 0.5, 1.0]),
                solid(extent, [1.0, 0.0, 0.0, 1.0]),
                group(true, vec![layer(0, StudioBlendMode::Normal, 1.0), fill]),
            ],
            2,
        );
        assert_eq!(
            check(&filled, Exactness::Exact, &none),
            unsupported("fill_opacity", Exactness::Approximate)
        );

        // Group semantics this backend does not guess.
        let pass = |opacity: f32| -> RenderPlan {
            plan(
                extent,
                vec![
                    solid(extent, [1.0, 0.0, 0.0, 1.0]),
                    group(false, vec![layer(0, StudioBlendMode::Normal, 1.0)]),
                    group(true, vec![layer(1, StudioBlendMode::PassThrough, opacity)]),
                ],
                2,
            )
        };
        let mut sink = Collect::new(extent);
        run(&renderer, &pass(1.0), Exactness::Exact, &none, &mut sink).unwrap();
        assert_eq!(sink.pixel(1, 1), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            check(&pass(0.5), Exactness::Approximate, &none),
            unsupported("group_pass_through_opacity", Exactness::Unsupported)
        );
        let blended_non_isolated: RenderPlan = plan(
            extent,
            vec![
                solid(extent, [1.0, 0.0, 0.0, 1.0]),
                group(false, vec![layer(0, StudioBlendMode::Normal, 1.0)]),
                group(true, vec![layer(1, StudioBlendMode::Normal, 1.0)]),
            ],
            2,
        );
        assert_eq!(
            check(&blended_non_isolated, Exactness::Approximate, &none),
            unsupported("group_non_isolated_blended", Exactness::Unsupported)
        );

        // Source faults: each is a typed refusal naming the key, never a rendered guess.
        let tile = TileData::new(2, 2, |x, y| [0.5, 0.25 * x as f32, 0.125 * y as f32, 1.0]);
        let with_tile = |input: TileInput| -> RenderPlan {
            plan(
                extent,
                vec![
                    PlanNode::Tile(input),
                    group(true, vec![layer(0, StudioBlendMode::Normal, 1.0)]),
                ],
                1,
            )
        };
        let good = with_tile(tile.input(1, 0, 0));
        let key = SourceKey(1);
        let faulty = |fault| RenderError::Source { key, fault };
        let mut sink = Collect::new(extent);
        run(&renderer, &good, Exactness::Exact, &one(tile.planes()), &mut sink).unwrap();
        assert_eq!(sink.pixel(1, 1), [0.5, 0.25, 0.125, 1.0]);

        assert_eq!(
            check(&good, Exactness::Exact, &none),
            backend(BackendError::UnresolvedSource(key))
        );
        let planes = tile.planes();
        for (broken, fault) in [
            (TilePlanes { colour_stride_bytes: 20, ..planes }, SourceFault::MalformedStride),
            (TilePlanes { alpha_stride_bytes: 10, ..planes }, SourceFault::MalformedStride),
            (TilePlanes { colour_sha256: [9; 32], ..planes }, SourceFault::Declared),
            (
                TilePlanes {
                    space: ColourTag {
                        blend_space: BlendSpace::Encoded,
                        ..tag()
                    },
                    ..planes
                },
                SourceFault::Declared,
            ),
            (TilePlanes { width: 3, ..planes }, SourceFault::Declared),
        ] {
            assert_eq!(check(&good, Exactness::Exact, &one(broken)), faulty(fault));
        }
        // Bytes that do not hash to the declared digest (plan and planes agree on the digest).
        let mut tampered = tile.colour.clone();
        tampered[0] ^= 1;
        assert_eq!(
            check(&good, Exactness::Exact, &one(TilePlanes { colour: &tampered, ..planes })),
            faulty(SourceFault::HashMismatch)
        );
        let bad_value = |index: usize, value: f32, alpha: bool| {
            let mut colour = tile.colour.clone();
            let mut alpha_bytes = tile.alpha.clone();
            if alpha {
                alpha_bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
            } else {
                colour[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
            let data = TileData {
                width: 2,
                height: 2,
                colour,
                alpha: alpha_bytes,
            };
            let p = with_tile(data.input(1, 0, 0));
            check(&p, Exactness::Exact, &one(data.planes()))
        };
        assert_eq!(bad_value(0, f32::NAN, false), faulty(SourceFault::Nonfinite));
        assert_eq!(bad_value(0, 1.5, true), faulty(SourceFault::AlphaRange));
        assert_eq!(
            bad_value(0, 1.5, false),
            unsupported("sample_range_outside_unit", Exactness::Unsupported)
        );
        // Premultiplied declaration is not evaluated by this backend.
        let mut premultiplied = tile.input(1, 0, 0);
        premultiplied.alpha = AlphaConvention::Premultiplied;
        assert_eq!(
            check(&with_tile(premultiplied), Exactness::Exact, &one(tile.planes())),
            unsupported("alpha_premultiplied", Exactness::Unsupported)
        );

        // Mask faults.
        let mask = MaskData::u8(extent, |_, _| 255);
        let with_mask = |input: MaskInput| -> RenderPlan {
            let mut l = layer(1, StudioBlendMode::Normal, 1.0);
            l.mask = Some(input);
            plan(
                extent,
                vec![
                    solid(extent, [0.5, 0.5, 0.5, 1.0]),
                    solid(extent, [1.0, 0.0, 0.0, 1.0]),
                    group(true, vec![layer(0, StudioBlendMode::Normal, 1.0), l]),
                ],
                2,
            )
        };
        let masks = Sources {
            tiles: vec![],
            masks: vec![(4, &mask)],
        };
        let mkey = SourceKey(4);
        let mut sink = Collect::new(extent);
        run(&renderer, &with_mask(mask.input(4, false)), Exactness::Exact, &masks, &mut sink).unwrap();
        assert_eq!(sink.pixel(0, 0), [1.0, 0.0, 0.0, 1.0]);
        let mut half = mask.input(4, false);
        half.density = 0.5;
        assert_eq!(
            check(&with_mask(half), Exactness::Exact, &masks),
            unsupported("mask_density", Exactness::Unsupported)
        );
        assert_eq!(
            check(&with_mask(mask.input(5, false)), Exactness::Exact, &masks),
            backend(BackendError::UnresolvedSource(SourceKey(5)))
        );
        let mut wrong_hash = mask.input(4, false);
        wrong_hash.sha256 = [3; 32];
        assert_eq!(
            check(&with_mask(wrong_hash), Exactness::Exact, &masks),
            RenderError::Source { key: mkey, fault: SourceFault::Declared }
        );
        // A mask that does not cover the whole layer inside the target is a gap, not a 0 or 1.
        let small = MaskData::u8(rect(0, 0, 1, 2), |_, _| 255);
        let gap = Sources {
            tiles: vec![],
            masks: vec![(4, &small)],
        };
        assert_eq!(
            check(&with_mask(small.input(4, false)), Exactness::Exact, &gap),
            RenderError::Source { key: mkey, fault: SourceFault::MaskGap }
        );
    });
}

struct Frames(Vec<(DeliveryClass, Vec<u8>)>);
impl SinkPort for Frames {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.to_vec()));
        Ok(())
    }
}

#[test]
fn receipt_binds_revision_profile_and_observe() {
    bounded_case("receipt_binds_revision_profile_and_observe", || {
        let extent = rect(10, 20, 5, 3);
        let p: RenderPlan<VectorFill> = plan(
            extent,
            vec![
                solid(extent, [0.5, 0.5, 0.5, 1.0]),
                vector(&[(10.0, 20.0), (14.0, 20.0), (12.0, 22.0)], Winding::NonZero, 4),
                group(
                    true,
                    vec![
                        layer(0, StudioBlendMode::Normal, 1.0),
                        layer(1, StudioBlendMode::Multiply, 1.0),
                    ],
                ),
            ],
            2,
        );
        let renderer = Renderer::new(2).unwrap();
        let token = CancellationToken::default();
        let mut sink = Collect::new(extent);
        let receipt = renderer
            .execute(&p, &req(Exactness::Approximate), &Sources::default(), &token, &mut sink)
            .unwrap();
        assert_eq!(receipt.plan_revision, 7);
        assert_eq!(receipt.plan_digest, p.digest().unwrap());
        let mut moved = p.clone();
        if let PlanNode::Vector(v) = &mut moved.nodes[1] {
            v.anchors[0] = anchor(px(11.0, 20.0), px(0.0, 0.0), px(0.0, 0.0));
        }
        assert_ne!(receipt.plan_digest, moved.digest().unwrap(), "digest binds the vector payload");
        assert_eq!(receipt.target.space, tag());
        assert_eq!(receipt.target.space.profile_sha256, profile_hash(b"working-profile"));
        assert_eq!((receipt.target.format, receipt.target.alpha), (SampleFormat::RgbF32Le, AlphaConvention::Straight));
        assert_eq!(receipt.region, extent);
        assert_eq!(receipt.backend.name, "hsk-studio-render-cpu");
        assert_eq!(receipt.backend.version, "0.1");
        assert_eq!(receipt.backend.device, DeviceClass::Cpu);
        assert_eq!((receipt.chunks, receipt.nodes_executed), (6, 3));
        assert_eq!(receipt.peak_working_bytes, 4 * 32);
        // Exactness ledger: vector coverage is supersampled, so the lowest class is Approximate
        // and every distinct operator appears with its effective class.
        assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
        let class = |key: &str| {
            receipt
                .exactness
                .operators
                .iter()
                .find(|o| o.key == key)
                .map(|o| o.exactness)
        };
        assert_eq!(class("vector"), Some(Exactness::Approximate));
        assert_eq!(class("solid"), Some(Exactness::Exact));
        assert_eq!(class("multiply"), Some(Exactness::Exact));
        assert_eq!(class("group_isolated"), None, "root is not referenced by a layer");
        // The same plan is refused when the caller demands an Exact floor.
        let mut refused = Collect::new(extent);
        assert_eq!(
            renderer
                .execute(&p, &req(Exactness::Exact), &Sources::default(), &token, &mut refused)
                .unwrap_err(),
            BackendError::Unsupported { operator: "vector", exactness: Exactness::Approximate }
        );
        assert_eq!(refused.calls, 0);

        let actor = || ActorContext::new("a", "p", "oa", "op", "space", "session").unwrap();
        let resource = || DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let observe = || Observe::new(42, 7, resource(), actor(), Budget::new(0, 0).unwrap());

        // Success: exactly one terminal frame; a second terminal emit is refused.
        let mut observe_ok = observe();
        let mut frames = Frames(vec![]);
        let ok = Ok(receipt);
        let emitted = report(&ok, 42, 7, &token, &mut observe_ok, &mut frames).unwrap();
        assert_eq!((emitted.outcome, emitted.revision, emitted.correlation_id), (Outcome::Success, 7, 42));
        assert_eq!(frames.0.len(), 1);
        assert_eq!(frames.0[0].0, DeliveryClass::Terminal);
        assert!(observe_ok.state().terminal_closed);
        assert!(report(&ok, 42, 7, &token, &mut observe_ok, &mut frames).is_err());
        assert_eq!(frames.0.len(), 1);

        // Error mapping.
        for (error, want) in [
            (BackendError::Canceled, Outcome::Canceled),
            (
                BackendError::Unsupported { operator: "x", exactness: Exactness::Unsupported },
                Outcome::Failure(FailureCode::Unsupported),
            ),
            (BackendError::StaleRevision { expected: 8, found: 7 }, Outcome::Failure(FailureCode::Validation)),
            (BackendError::SourceMismatch(SourceKey(1)), Outcome::Failure(FailureCode::Validation)),
            (BackendError::Sink(SinkError::Unavailable), Outcome::Failure(FailureCode::Unavailable)),
            (BackendError::DeadlineExceeded, Outcome::Failure(FailureCode::Unavailable)),
            (BackendError::UnresolvedSource(SourceKey(1)), Outcome::Failure(FailureCode::Unavailable)),
        ] {
            let mut observe = observe();
            let mut frames = Frames(vec![]);
            let emitted = report(&Err(error), 42, 7, &token, &mut observe, &mut frames).unwrap();
            assert_eq!(emitted.outcome, want, "{}", error.code());
            assert_eq!(frames.0.len(), 1);
        }

        // Descriptor is valid JSON and its stated limits equal the code constants.
        let descriptor: serde_json::Value = serde_json::from_str(hsk_studio_render_cpu::DESCRIPTOR).unwrap();
        assert_eq!(descriptor["module"], "STUDIO-MODULE-RENDER_CPU");
        assert_eq!(descriptor["renderer"], format!("{RENDERER_NAME}/{RENDERER_VERSION}"));
        assert_eq!(descriptor["limits"]["anchors_per_path"], MAX_ANCHORS);
        assert_eq!(descriptor["limits"]["chunk_side_max"], MAX_CHUNK_SIDE);
        assert_eq!(descriptor["limits"]["default_chunk_side"], DEFAULT_CHUNK_SIDE);
        assert_eq!(descriptor["limits"]["scratch_bytes_per_chunk_pixel"], SCRATCH_BYTES_PER_PIXEL);
        assert_eq!(
            descriptor["limits"]["scratch_bytes_per_isolated_group_depth_pixel"],
            plan_sink::GROUP_SCRATCH_BYTES_PER_PIXEL
        );
    });
}
