use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Length, Unit};
use hsk_studio_nib::{Anchor, Mirroring, Point, Winding};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};
use hsk_studio_pigment::{Grid, Rect, wire};
use hsk_studio_prism::profile_hash;
use hsk_studio_render_cpu::plan_sink::{MAX_ANCHORS, MAX_CHUNK_SIDE, MAX_OPS};
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

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}
fn pxl(x: f64, y: f64) -> Point {
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
fn polygon(points: &[(f64, f64)]) -> Vec<Anchor> {
    points
        .iter()
        .map(|&(x, y)| anchor(pxl(x, y), pxl(0.0, 0.0), pxl(0.0, 0.0)))
        .collect()
}
fn with(tile: SourceTile<'_>) -> Vec<(SourceId, SourceTile<'_>)> {
    vec![(SourceId(1), tile)]
}
fn fill(rect: Rect, rgba: [f32; 4], blend: BlendMode, opacity: f32) -> Op {
    Op::Fill {
        rect,
        rgba,
        blend,
        opacity,
    }
}
fn path_op(anchors: Vec<Anchor>, winding: Winding, rgba: [f32; 4], blend: BlendMode) -> Op {
    Op::PathFill {
        anchors,
        closed: true,
        winding,
        rgba,
        blend,
        opacity: 1.0,
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
impl PixelSink for Collect {
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
fn render_into(plan: &RenderPlan, chunk: u32, sink: &mut Collect) -> RenderReceipt {
    let renderer = Renderer::new(RenderOptions {
        chunk,
        ..RenderOptions::default()
    })
    .unwrap();
    renderer
        .render(plan, plan.revision, &NoSources, &CancellationToken::default(), sink)
        .unwrap()
}
fn plan_of(extent: Rect, space: BlendSpace, ops: Vec<Op>) -> RenderPlan {
    RenderPlan::new(7, extent, profile_hash(b"working-profile"), space, ops)
}

#[test]
fn known_alpha_mask_sample_math() {
    bounded_case("known_alpha_mask_sample_math", || {
        let backdrop = [0.2_f32, 0.4, 0.6, 1.0];
        let red = [1.0_f32, 0.0, 0.0, 1.0];
        // Hand-computed: as = 0.5, ab = 1, ao = 1.
        close(
            composite_straight(BlendMode::Normal, backdrop, red, 0.5, 1.0),
            [0.6, 0.2, 0.3, 1.0],
        );
        close(
            composite_straight(BlendMode::Multiply, backdrop, red, 0.5, 1.0),
            [0.2, 0.2, 0.3, 1.0],
        );
        // Zero contribution returns the backdrop bit-for-bit.
        for (source, opacity, coverage) in [
            (red, 0.0, 1.0),
            ([1.0, 0.0, 0.0, 0.0], 1.0, 1.0),
            (red, 1.0, 0.0),
        ] {
            let out = composite_straight(BlendMode::Multiply, backdrop, source, opacity, coverage);
            assert_eq!(out.map(f32::to_bits), backdrop.map(f32::to_bits));
        }
        // Opaque source, opaque backdrop: result is exactly B(Cb, Cs) per channel.
        for (mode, want) in [
            (BlendMode::Screen, [1.0, 0.4, 0.6]),
            (BlendMode::Darken, [0.2, 0.0, 0.0]),
            (BlendMode::Lighten, [1.0, 0.4, 0.6]),
            (BlendMode::Difference, [0.8, 0.4, 0.6]),
            // Overlay: backdrop decides multiply (<=.5: 2*cb*cs) or screen (>.5: cs+2cb-1-cs(2cb-1)).
            (BlendMode::Overlay, [0.4, 0.0, 0.2]),
        ] {
            let out = composite_straight(mode, backdrop, red, 1.0, 1.0);
            close(out, [want[0], want[1], want[2], 1.0]);
        }
        // Partial backdrop alpha: ab=.5, as=.5, ao=.75, C=(0.1875+0.0625+0.0625)/0.75.
        close(
            composite_straight(
                BlendMode::Normal,
                [0.75, 0.75, 0.75, 0.5],
                [0.25, 0.25, 0.25, 1.0],
                0.5,
                1.0,
            ),
            [0.416_666_67, 0.416_666_67, 0.416_666_67, 0.75],
        );
        // Over transparent black the source colour is kept and alpha = as.
        close(
            composite_straight(BlendMode::Multiply, [0.0; 4], [0.25, 0.5, 0.75, 1.0], 1.0, 0.5),
            [0.25, 0.5, 0.75, 0.5],
        );
        // Coverage and opacity are interchangeable alpha factors.
        let a = composite_straight(BlendMode::Normal, backdrop, red, 1.0, 0.5);
        let b = composite_straight(BlendMode::Normal, backdrop, red, 0.5, 1.0);
        assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits));

        // Path coverage as the alpha mask: rect x in [0.5, 3], y in [0, 3] on a 5x4 extent.
        let extent = rect(0, 0, 5, 4);
        let plan = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![path_op(
                polygon(&[(0.5, 0.0), (3.0, 0.0), (3.0, 3.0), (0.5, 3.0)]),
                Winding::NonZero,
                red,
                BlendMode::Normal,
            )],
        );
        let mut sink = Collect::new(extent);
        render_into(&plan, 3, &mut sink);
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
                assert_eq!(sink.pixel(x, y), want, "pixel ({x},{y})");
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
            let plan = plan_of(
                extent,
                BlendSpace::LinearLight,
                vec![path_op(polygon(&star), winding, red, BlendMode::Normal)],
            );
            let mut sink = Collect::new(extent);
            render_into(&plan, 8, &mut sink);
            assert_eq!(sink.pixel(10, 10)[3], want, "{winding:?} centre");
            assert_eq!(sink.pixel(9, 9)[3], want, "{winding:?} centre");
            assert_eq!(sink.pixel(0, 0)[3], 0.0, "{winding:?} outside");
        }

        // Curves: four cubics (kappa = 0.5522847498, handles relative to anchors) approximate a
        // circle of r = 8 at (10, 10); summed coverage equals pi r^2 within sampling error.
        let (r, k) = (8.0_f64, 0.552_284_749_8 * 8.0);
        let circle = vec![
            anchor(pxl(10.0, 2.0), pxl(-k, 0.0), pxl(k, 0.0)),
            anchor(pxl(18.0, 10.0), pxl(0.0, -k), pxl(0.0, k)),
            anchor(pxl(10.0, 18.0), pxl(k, 0.0), pxl(-k, 0.0)),
            anchor(pxl(2.0, 10.0), pxl(0.0, k), pxl(0.0, -k)),
        ];
        let mut plan = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![path_op(circle, Winding::NonZero, red, BlendMode::Normal)],
        );
        plan.aa_samples = 8;
        let mut sink = Collect::new(extent);
        render_into(&plan, 7, &mut sink);
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
        // Tile 3x2, dyadic values, alpha 0.5.
        let (tw, th) = (3_u32, 2_u32);
        let mut colour = Vec::new();
        let mut alpha = Vec::new();
        let tile_rgb = |tx: u32, ty: u32| {
            [
                (tx + 1) as f32 / 8.0,
                (ty + 1) as f32 / 4.0,
                (tx + 2 * ty) as f32 / 8.0,
            ]
        };
        for ty in 0..th {
            for tx in 0..tw {
                for v in tile_rgb(tx, ty) {
                    colour.extend_from_slice(&v.to_le_bytes());
                }
                alpha.extend_from_slice(&0.5_f32.to_le_bytes());
            }
        }
        let profile = profile_hash(b"working-profile");
        let tile = SourceTile {
            width: tw,
            height: th,
            colour: &colour,
            colour_stride_bytes: u64::from(tw) * 12,
            alpha: &alpha,
            alpha_stride_bytes: u64::from(tw) * 4,
            colour_sha256: profile_hash(&colour),
            alpha_sha256: profile_hash(&alpha),
            profile_sha256: profile,
            space: BlendSpace::LinearLight,
        };
        let tiles = vec![(SourceId(3), tile)];
        let base = [0.25_f32, 0.5, 0.75];
        let plan = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![
                fill(extent, [base[0], base[1], base[2], 1.0], BlendMode::Normal, 1.0),
                fill(rect(-1, 1, 3, 2), [0.5, 0.25, 1.0, 0.5], BlendMode::Multiply, 0.5),
                Op::Tile {
                    source: SourceId(3),
                    dst_x: 0,
                    dst_y: 2,
                    blend: BlendMode::Screen,
                    opacity: 1.0,
                },
                path_op(
                    polygon(&[(0.5, 1.5), (2.5, 1.5), (2.5, 3.5), (0.5, 3.5)]),
                    Winding::NonZero,
                    [1.0, 1.0, 1.0, 1.0],
                    BlendMode::Difference,
                ),
            ],
        );

        // Independent reference: opaque backdrop, so each op is (1-as)*cb + as*B(cb, cs); path
        // coverage is the analytic pixel/rect overlap area. Dyadic inputs keep it exact in f32.
        let mut want_colour = Vec::new();
        let mut want_alpha = Vec::new();
        for y in extent.y..extent.y + 3 {
            for x in extent.x..extent.x + 5 {
                let mut c = base.map(f64::from);
                let step = |c: &mut [f64; 3], cs: [f64; 3], a: f64, b: fn(f64, f64) -> f64| {
                    for i in 0..3 {
                        c[i] = (1.0 - a) * c[i] + a * b(c[i], cs[i]);
                    }
                };
                if (-1..2).contains(&x) && (1..3).contains(&y) {
                    step(&mut c, [0.5, 0.25, 1.0], 0.25, |cb, cs| cb * cs);
                }
                if (0..3).contains(&x) && (2..4).contains(&y) {
                    let t = tile_rgb(x as u32, (y - 2) as u32).map(f64::from);
                    step(&mut c, t, 0.5, |cb, cs| cb + cs - cb * cs);
                }
                let overlap = |p: i64, lo: f64, hi: f64| {
                    ((p as f64 + 1.0).min(hi) - (p as f64).max(lo)).clamp(0.0, 1.0)
                };
                let cov = overlap(x, 0.5, 2.5) * overlap(y, 1.5, 3.5);
                step(&mut c, [1.0; 3], cov, |cb, cs| (cb - cs).abs());
                for v in c {
                    want_colour.extend_from_slice(&(v as f32).to_le_bytes());
                }
                want_alpha.extend_from_slice(&1.0_f32.to_le_bytes());
            }
        }

        for (chunk, chunks) in [(1_u32, 15_u64), (2, 6), (3, 2), (4, 2), (64, 1)] {
            let renderer = Renderer::new(RenderOptions {
                chunk,
                ..RenderOptions::default()
            })
            .unwrap();
            let mut sink = Collect::new(extent);
            let receipt = renderer
                .render(&plan, 7, &tiles, &CancellationToken::default(), &mut sink)
                .unwrap();
            assert_eq!(receipt.chunks, chunks, "chunk {chunk}");
            assert_eq!(receipt.ops_executed, 4);
            assert!(sink.writes.iter().all(|w| *w == 1), "each pixel exactly once");
            assert!(sink.rects.iter().all(|r| r.width <= chunk && r.height <= chunk));
            assert_eq!(sink.colour, want_colour, "colour plane, chunk {chunk}");
            assert_eq!(sink.alpha, want_alpha, "alpha plane, chunk {chunk}");
            assert_eq!(renderer.live_scratch_bytes(), 0);
        }

        // Pigment canonical tile through the adapter: 2x2 at grid (0,0), placed at (1,1).
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
        let grid = Grid {
            origin_x: 0,
            origin_y: 0,
            tile_width: 2,
            tile_height: 2,
        };
        let source = SourceTile::from_resolved(&resolved, grid).unwrap();
        let extent = rect(0, 0, 4, 4);
        let mut plan = RenderPlan::new(
            1,
            extent,
            profile_hash(&profile_bytes),
            BlendSpace::LinearLight,
            vec![Op::Tile {
                source: SourceId(0),
                dst_x: 1,
                dst_y: 1,
                blend: BlendMode::Normal,
                opacity: 1.0,
            }],
        );
        let tiles = vec![(SourceId(0), source)];
        let renderer = Renderer::new(RenderOptions {
            chunk: 3,
            ..RenderOptions::default()
        })
        .unwrap();
        let mut sink = Collect::new(extent);
        renderer
            .render(&plan, 1, &tiles, &CancellationToken::default(), &mut sink)
            .unwrap();
        assert_eq!(sink.pixel(1, 1), [0.5, 0.25, 0.125, 1.0]);
        assert_eq!(sink.pixel(2, 1), [1.0, 0.0, 0.5, 0.5]);
        assert_eq!(sink.pixel(1, 2), [0.0, 0.75, 0.25, 0.25]);
        assert_eq!(sink.pixel(2, 2), [0.125, 0.5, 1.0, 1.0]);
        assert_eq!(sink.pixel(0, 0), [0.0; 4]);
        assert_eq!(sink.pixel(3, 3), [0.0; 4]);
        // A plan whose working profile is not the tile's profile is refused, not converted.
        plan.profile_sha256 = profile_hash(b"another-profile");
        assert_eq!(
            renderer
                .render(&plan, 1, &tiles, &CancellationToken::default(), &mut Collect::new(extent))
                .unwrap_err(),
            RenderError::ProfileMismatch
        );
    });
}

#[test]
fn sink_error_cancel_releases_bytes() {
    bounded_case("sink_error_cancel_releases_bytes", || {
        let extent = rect(0, 0, 5, 3);
        let plan = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![fill(extent, [0.5, 0.5, 0.5, 1.0], BlendMode::Normal, 1.0)],
        );
        let renderer = Renderer::new(RenderOptions {
            chunk: 2,
            ..RenderOptions::default()
        })
        .unwrap();
        let token = CancellationToken::default();

        // Sink fails on its second chunk: typed error, no receipt, scratch released.
        let mut sink = Collect::new(extent);
        sink.fail_at = Some(1);
        let error = renderer.render(&plan, 7, &NoSources, &token, &mut sink).unwrap_err();
        assert_eq!(error, RenderError::Sink(SinkError::Rejected));
        assert_eq!(sink.calls, 2, "aborted at the failing chunk");
        assert_eq!(renderer.live_scratch_bytes(), 0);
        assert_eq!(renderer.peak_scratch_bytes(), 4 * 32, "2x2 chunk x 32 B/px");

        // Pre-cancelled: nothing reaches the sink.
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        let mut sink = Collect::new(extent);
        assert_eq!(
            renderer.render(&plan, 7, &NoSources, &cancelled, &mut sink).unwrap_err(),
            RenderError::Canceled
        );
        assert_eq!(sink.calls, 0);
        assert_eq!(renderer.live_scratch_bytes(), 0);

        // Cancelled by the sink after chunk 2: observed before chunk 3, not mid-chunk.
        let mid = CancellationToken::default();
        let mut sink = Collect::new(extent);
        sink.cancel_at = Some((1, mid.clone()));
        assert_eq!(
            renderer.render(&plan, 7, &NoSources, &mid, &mut sink).unwrap_err(),
            RenderError::Canceled
        );
        assert_eq!(sink.calls, 2);
        assert_eq!(renderer.live_scratch_bytes(), 0);

        // Deadline in the past is a typed error before the first chunk.
        let late = Renderer::new(RenderOptions {
            chunk: 2,
            deadline: Instant::now().checked_sub(Duration::from_secs(1)),
            ..RenderOptions::default()
        })
        .unwrap();
        let mut sink = Collect::new(extent);
        assert_eq!(
            late.render(&plan, 7, &NoSources, &token, &mut sink).unwrap_err(),
            RenderError::DeadlineExceeded
        );
        assert_eq!(sink.calls, 0);

        // Scratch budget is enforced before allocation: 4 px x 32 B = 128 needed.
        let tight = Renderer::new(RenderOptions {
            chunk: 2,
            max_scratch_bytes: 127,
            ..RenderOptions::default()
        })
        .unwrap();
        let mut sink = Collect::new(extent);
        assert_eq!(
            tight.render(&plan, 7, &NoSources, &token, &mut sink).unwrap_err(),
            RenderError::BudgetExceeded
        );
        assert_eq!((sink.calls, tight.live_scratch_bytes(), tight.peak_scratch_bytes()), (0, 0, 0));
        assert_eq!(
            Renderer::new(RenderOptions {
                chunk: MAX_CHUNK_SIDE + 1,
                ..RenderOptions::default()
            })
            .err(),
            Some(RenderError::InvalidInput("chunk"))
        );
    });
}

#[test]
fn unsupported_op_stale_result() {
    bounded_case("unsupported_op_stale_result", || {
        // Every non-implemented STU-RAS-154/039 member is a typed error naming the mode.
        for code in (0_u16..=40).filter(|c| BlendMode::SUPPORTED.iter().all(|m| m.code() != *c)) {
            assert_eq!(BlendMode::try_from(code), Err(RenderError::UnsupportedBlend(code)));
        }
        for mode in BlendMode::SUPPORTED {
            assert_eq!(BlendMode::try_from(mode.code()), Ok(mode));
            assert_eq!(blend_mode_name(mode.code()), Some(mode.name()));
        }
        assert_eq!(blend_mode_name(26), Some("hard_mix"));
        assert_eq!(blend_mode_name(0), None);
        assert_eq!(blend_mode_name(36), None);
        assert_eq!(
            BlendMode::try_from(26).unwrap_err().to_string(),
            "unsupported_blend(26:hard_mix)"
        );

        let extent = rect(0, 0, 2, 2);
        let renderer = Renderer::new(RenderOptions::default()).unwrap();
        let token = CancellationToken::default();
        let run = |plan: &RenderPlan, expected: u64, tiles: &dyn TileSource| {
            let mut sink = Collect::new(extent);
            let result = renderer.render(plan, expected, tiles, &token, &mut sink);
            assert_eq!(sink.calls, 0, "refused before the first sink call: {result:?}");
            assert_eq!(renderer.live_scratch_bytes(), 0);
            result.unwrap_err()
        };
        let solid = vec![fill(extent, [1.0, 0.0, 0.0, 1.0], BlendMode::Normal, 1.0)];
        let plan = plan_of(extent, BlendSpace::LinearLight, solid.clone());

        // Stale revision.
        assert_eq!(
            run(&plan, 8, &NoSources),
            RenderError::StaleRevision {
                expected: 8,
                found: 7
            }
        );
        // Unsupported / invalid ops.
        let none_winding = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![path_op(
                polygon(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0)]),
                Winding::None,
                [1.0; 4],
                BlendMode::Normal,
            )],
        );
        assert!(matches!(run(&none_winding, 7, &NoSources), RenderError::UnsupportedOp(_)));
        let one_anchor = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![path_op(polygon(&[(0.0, 0.0)]), Winding::NonZero, [1.0; 4], BlendMode::Normal)],
        );
        assert_eq!(run(&one_anchor, 7, &NoSources), RenderError::InvalidGeometry);
        let bad_opacity = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![fill(extent, [1.0; 4], BlendMode::Normal, 1.5)],
        );
        assert_eq!(run(&bad_opacity, 7, &NoSources), RenderError::InvalidInput("opacity"));
        let nan = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![fill(extent, [f32::NAN, 0.0, 0.0, 1.0], BlendMode::Normal, 1.0)],
        );
        assert_eq!(run(&nan, 7, &NoSources), RenderError::Nonfinite);
        let empty = plan_of(rect(0, 0, 0, 2), BlendSpace::LinearLight, solid.clone());
        assert_eq!(run(&empty, 7, &NoSources), RenderError::InvalidInput("extent"));
        let too_many = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![solid[0].clone(); MAX_OPS + 1],
        );
        assert_eq!(run(&too_many, 7, &NoSources), RenderError::BudgetExceeded);
        let many_anchors = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![path_op(
                polygon(&vec![(0.0, 0.0); MAX_ANCHORS + 1]),
                Winding::NonZero,
                [1.0; 4],
                BlendMode::Normal,
            )],
        );
        assert_eq!(run(&many_anchors, 7, &NoSources), RenderError::BudgetExceeded);

        // Tile source faults: each is a typed refusal, never a rendered guess.
        let profile = profile_hash(b"working-profile");
        let good_colour: Vec<u8> = (0..4)
            .flat_map(|_| [0.5_f32, 0.25, 0.125])
            .flat_map(f32::to_le_bytes)
            .collect();
        let good_alpha: Vec<u8> = [1.0_f32; 4].iter().flat_map(|v| v.to_le_bytes()).collect();
        let tile_op = Op::Tile {
            source: SourceId(1),
            dst_x: 0,
            dst_y: 0,
            blend: BlendMode::Normal,
            opacity: 1.0,
        };
        let tile_plan = plan_of(extent, BlendSpace::LinearLight, vec![tile_op]);
        let base = SourceTile {
            width: 2,
            height: 2,
            colour: &good_colour,
            colour_stride_bytes: 24,
            alpha: &good_alpha,
            alpha_stride_bytes: 8,
            colour_sha256: profile_hash(&good_colour),
            alpha_sha256: profile_hash(&good_alpha),
            profile_sha256: profile,
            space: BlendSpace::LinearLight,
        };
        // Sanity: the unmodified tile renders.
        let mut sink = Collect::new(extent);
        renderer
            .render(&tile_plan, 7, &vec![(SourceId(1), base)], &token, &mut sink)
            .unwrap();
        assert_eq!(sink.pixel(1, 1), [0.5, 0.25, 0.125, 1.0]);

        assert_eq!(run(&tile_plan, 7, &NoSources), RenderError::UnresolvedSource(1));
        assert_eq!(
            run(&tile_plan, 7, &with(SourceTile { colour_stride_bytes: 20, ..base })),
            RenderError::MalformedStride
        );
        assert_eq!(
            run(&tile_plan, 7, &with(SourceTile { alpha_stride_bytes: 10, ..base })),
            RenderError::MalformedStride
        );
        assert_eq!(
            run(&tile_plan, 7, &with(SourceTile { colour_sha256: [9; 32], ..base })),
            RenderError::HashMismatch
        );
        assert_eq!(
            run(&tile_plan, 7, &with(SourceTile { profile_sha256: [1; 32], ..base })),
            RenderError::ProfileMismatch
        );
        assert_eq!(
            run(&tile_plan, 7, &with(SourceTile { space: BlendSpace::Encoded, ..base })),
            RenderError::SpaceMismatch
        );
        let mut nan_colour = good_colour.clone();
        nan_colour[0..4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert_eq!(
            run(
                &tile_plan,
                7,
                &with(SourceTile {
                    colour: &nan_colour,
                    colour_sha256: profile_hash(&nan_colour),
                    ..base
                })
            ),
            RenderError::Nonfinite
        );
        let mut bad_alpha = good_alpha.clone();
        bad_alpha[0..4].copy_from_slice(&1.5_f32.to_le_bytes());
        assert_eq!(
            run(
                &tile_plan,
                7,
                &with(SourceTile {
                    alpha: &bad_alpha,
                    alpha_sha256: profile_hash(&bad_alpha),
                    ..base
                })
            ),
            RenderError::AlphaRange
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
        let plan = plan_of(
            extent,
            BlendSpace::LinearLight,
            vec![fill(extent, [0.5, 0.5, 0.5, 1.0], BlendMode::Normal, 1.0)],
        );
        let renderer = Renderer::new(RenderOptions {
            chunk: 2,
            ..RenderOptions::default()
        })
        .unwrap();
        let token = CancellationToken::default();
        let mut sink = Collect::new(extent);
        let result = renderer.render(&plan, 7, &NoSources, &token, &mut sink);
        let receipt = result.unwrap();
        assert_eq!(receipt.revision, 7);
        assert_eq!(receipt.profile_sha256, profile_hash(b"working-profile"));
        assert_eq!(receipt.blend_space, BlendSpace::LinearLight);
        assert_eq!(receipt.extent, extent);
        assert_eq!((receipt.sample_format, receipt.alpha), ("rgb_f32le", "straight"));
        assert_eq!(receipt.renderer, "hsk-studio-render-cpu/0.1");
        assert_eq!((receipt.chunks, receipt.ops_executed), (6, 1));
        assert_eq!(receipt.peak_scratch_bytes, 128);

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
            (RenderError::Canceled, Outcome::Canceled),
            (RenderError::UnsupportedBlend(26), Outcome::Failure(FailureCode::Unsupported)),
            (RenderError::UnsupportedOp("x"), Outcome::Failure(FailureCode::Unsupported)),
            (
                RenderError::StaleRevision { expected: 8, found: 7 },
                Outcome::Failure(FailureCode::Validation),
            ),
            (RenderError::HashMismatch, Outcome::Failure(FailureCode::Validation)),
            (RenderError::Sink(SinkError::Unavailable), Outcome::Failure(FailureCode::Unavailable)),
            (RenderError::DeadlineExceeded, Outcome::Failure(FailureCode::Unavailable)),
        ] {
            let mut observe = observe();
            let mut frames = Frames(vec![]);
            let emitted = report(&Err(error), 42, 7, &token, &mut observe, &mut frames).unwrap();
            assert_eq!(emitted.outcome, want, "{error}");
            assert_eq!(frames.0.len(), 1);
            assert!(!error.code().is_empty());
        }

        // Descriptor is valid JSON and its stated limits equal the code constants.
        let descriptor: serde_json::Value = serde_json::from_str(DESCRIPTOR).unwrap();
        assert_eq!(descriptor["module"], "STUDIO-MODULE-RENDER_CPU");
        assert_eq!(descriptor["renderer"], receipt.renderer);
        assert_eq!(descriptor["limits"]["ops"], MAX_OPS);
        assert_eq!(descriptor["limits"]["anchors_per_path"], MAX_ANCHORS);
        assert_eq!(descriptor["limits"]["chunk_side_max"], MAX_CHUNK_SIDE);
        assert_eq!(
            descriptor["limits"]["scratch_bytes_per_chunk_pixel"],
            plan_sink::SCRATCH_BYTES_PER_PIXEL
        );
        for mode in BlendMode::SUPPORTED {
            assert_eq!(descriptor["blend_modes"]["supported"][mode.name()], mode.code());
        }
    });
}
