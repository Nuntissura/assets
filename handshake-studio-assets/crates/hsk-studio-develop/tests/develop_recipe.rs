use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_pigment::{Grid, Rect};

// Each canonical test case owns its deadline; panic payloads reach the Cargo harness.
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
        Ok(Err(payload)) => resume_unwind(payload),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared 30-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost its deadline worker result")
        }
    }
}

const IDENTITY: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [7; 32],
    }
}

fn recipe() -> DevelopRecipe {
    DevelopRecipe::native([1.0; 3], IDENTITY, profile())
}

fn init(width: u32, height: u32, samples: &[u16]) -> PlaneInit {
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    PlaneInit {
        width,
        height,
        stride_bytes: width * 2,
        sensor_origin: (0, 0),
        cfa: Cfa {
            pattern: CfaPattern::Bayer(Bayer::Rggb),
            phase_x: 0,
            phase_y: 0,
        },
        black: [0; 3],
        white: [64; 3],
        source_revision: 1,
        bytes,
    }
}

fn grid(origin: (i64, i64), tile: u32) -> Grid {
    Grid {
        origin_x: origin.0,
        origin_y: origin.1,
        tile_width: tile,
        tile_height: tile,
    }
}

type Run = (Result<DevelopReceipt, DevelopError>, Vec<DevelopedTile>);

fn run_with(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    grid: Grid,
    origin: (i64, i64),
    hint: ExecutionHint,
    cancel: &CancellationToken,
    mut on_tile: impl FnMut(&DevelopedTile) -> Result<(), SinkError>,
) -> Run {
    let mut tiles = Vec::new();
    let result = develop(
        &DevelopRequest {
            plane,
            recipe,
            recipe_revision: 1,
            expected_source_revision: plane.source_revision(),
            grid,
            output_origin: origin,
            execution_hint: hint,
        },
        cancel,
        &mut |tile| {
            on_tile(&tile)?;
            tiles.push(tile);
            Ok(())
        },
    );
    (result, tiles)
}

fn run(plane: &CfaPlane, recipe: &DevelopRecipe, tile: u32) -> Run {
    run_with(
        plane,
        recipe,
        grid((0, 0), tile),
        (0, 0),
        ExecutionHint::Scalar,
        &CancellationToken::default(),
        |_| Ok(()),
    )
}

fn px(tile: &DevelopedTile, tile_width: u32, x: usize, y: usize) -> [f32; 3] {
    let i = (y * tile_width as usize + x) * 12;
    let f = |o: usize| f32::from_le_bytes(tile.colour[i + o..i + o + 4].try_into().unwrap());
    [f(0), f(4), f(8)]
}

/// Hand-built RGGB 4x4 plane (black 0, white 64): R sites 8/16/24/32, G sites 4/12/20/28/36/44/52/60,
/// B sites 40/48/56/2.
fn matrix_samples() -> Vec<u16> {
    vec![
        8, 4, 16, 12, //
        20, 40, 28, 48, //
        24, 36, 32, 44, //
        52, 56, 60, 2,
    ]
}

#[test]
fn analytic_cfa_matrix() {
    bounded_case("analytic_cfa_matrix", || {
        // (a) uniform plane at the mid-point of per-channel black/white -> exactly 0.5 everywhere.
        let mut uniform = init(4, 4, &[0; 16]);
        uniform.black = [100, 200, 300];
        uniform.white = [1100, 1200, 1300];
        let cell = [[600u16, 700], [700, 800]];
        let samples: Vec<u16> = (0..16).map(|i| cell[(i / 4) % 2][i % 2]).collect();
        uniform.bytes = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let plane = CfaPlane::new(uniform).unwrap();
        let (receipt, tiles) = run(&plane, &recipe(), 4);
        let receipt = receipt.unwrap();
        assert_eq!((receipt.tiles, receipt.pixels, receipt.route), (1, 16, "scalar"));
        for y in 0..4 {
            for x in 0..4 {
                for c in px(&tiles[0], 4, x, y) {
                    assert_eq!(c.to_bits(), 0.5f32.to_bits());
                }
            }
        }

        // (b) hand-computed from the visit rule (dy outer, dx inner, reflect period 2*(extent-1)).
        // (1,1) B site: R = (8+16+24+32)/4/64 = 0.3125, G = (4+20+28+36)/4/64 = 0.34375, B = 40/64.
        // (0,0) R corner: G visits (0,1),(1,0),(1,0),(0,1) = (20+4+4+20)/4/64 = 0.1875, B = (1,1) x4 = 0.625.
        // (3,3) B corner: R = (2,2) x4 = 0.5, G visits (3,2),(2,3),(2,3),(3,2) = 208/4/64 = 0.8125, B = 2/64.
        let plane = CfaPlane::new(init(4, 4, &matrix_samples())).unwrap();
        let (receipt, tiles) = run(&plane, &recipe(), 4);
        receipt.unwrap();
        let expect = |got: [f32; 3], want: [f32; 3]| {
            for i in 0..3 {
                assert_eq!(got[i].to_bits(), want[i].to_bits(), "channel {i}: {got:?} vs {want:?}");
            }
        };
        expect(px(&tiles[0], 4, 1, 1), [0.3125, 0.34375, 0.625]);
        expect(px(&tiles[0], 4, 0, 0), [0.125, 0.1875, 0.625]);
        expect(px(&tiles[0], 4, 3, 3), [0.5, 0.8125, 0.03125]);
        let full = demosaic_normalized(&plane, &CancellationToken::default()).unwrap();
        expect(full[5], [0.3125, 0.34375, 0.625]);
        let baseline = tiles[0].colour.clone();

        // (c) every other Bayer pattern / phase / origin that describes the same sites is bit-identical.
        let variants = [
            (Bayer::Grbg, 1u8, 0u8, (0i64, 0i64)),
            (Bayer::Bggr, 1, 1, (0, 0)),
            (Bayer::Gbrg, 0, 1, (0, 0)),
            (Bayer::Rggb, 1, 0, (1, 0)),
            (Bayer::Rggb, 0, 1, (-2, 3)),
        ];
        for (pattern, phase_x, phase_y, origin) in variants {
            let mut v = init(4, 4, &matrix_samples());
            v.cfa = Cfa {
                pattern: CfaPattern::Bayer(pattern),
                phase_x,
                phase_y,
            };
            v.sensor_origin = origin;
            let plane = CfaPlane::new(v).unwrap();
            let (receipt, tiles) = run(&plane, &recipe(), 4);
            receipt.unwrap();
            assert_eq!(tiles[0].colour, baseline, "{pattern:?} {phase_x} {phase_y} {origin:?}");
        }

        // (d) fixed X-Trans 6x6 plane at mid-level -> 0.5 everywhere (all channels have support).
        let mut xt = init(6, 6, &[50; 36]);
        xt.white = [100; 3];
        xt.cfa.pattern = CfaPattern::XTrans;
        let plane = CfaPlane::new(xt).unwrap();
        let (receipt, tiles) = run(&plane, &recipe(), 6);
        receipt.unwrap();
        for y in 0..6 {
            for x in 0..6 {
                assert_eq!(px(&tiles[0], 6, x, y), [0.5, 0.5, 0.5]);
            }
        }

        // (e) crop does not rephase or re-reflect: crop (0.5..1.0)^2 -> source x0=y0=2, mapped to
        // document (11,21) on a grid with origin (10,20), 2px tiles -> 4 partially covered tiles.
        let plane = CfaPlane::new(init(4, 4, &matrix_samples())).unwrap();
        let mut cropped = recipe();
        cropped.crop.left = 0.5;
        cropped.crop.top = 0.5;
        let (receipt, tiles) = run_with(
            &plane,
            &cropped,
            grid((10, 20), 2),
            (11, 21),
            ExecutionHint::Auto,
            &CancellationToken::default(),
            |_| Ok(()),
        );
        let receipt = receipt.unwrap();
        assert_eq!(receipt.source_bounds, (2, 2, 4, 4));
        assert_eq!(receipt.output, Rect { x: 11, y: 21, width: 2, height: 2 });
        assert_eq!((receipt.tiles, receipt.pixels, receipt.requested_hint), (4, 4, ExecutionHint::Auto));
        let last = &tiles[3];
        assert_eq!(last.bounds, Rect { x: 12, y: 22, width: 2, height: 2 });
        assert_eq!(last.developed, Rect { x: 12, y: 22, width: 1, height: 1 });
        expect(px(last, 2, 0, 0), [0.5, 0.8125, 0.03125]); // source (3,3)
        assert_eq!(last.coverage, vec![255, 0, 0, 0]);
        assert_eq!(&last.alpha[0..4], &1.0f32.to_le_bytes());
        assert!(last.alpha[4..].iter().all(|b| *b == 0));
    });
}

#[test]
fn disabled_group_preserves_recipe_and_pixels() {
    bounded_case("disabled_group_preserves_recipe_and_pixels", || {
        let plane = CfaPlane::new(init(4, 4, &[48; 16])).unwrap(); // 48/64 = 0.75
        let tone = Curve::new(vec![(0.0, 0.0), (127.5, 63.75), (255.0, 255.0)]).unwrap();
        let mut disabled = recipe();
        disabled.curves.composite = tone.clone();
        let before = disabled.clone();
        let (_, with_curve_authored) = run(&plane, &disabled, 4);
        let (_, without) = run(&plane, &recipe(), 4);
        assert_eq!(disabled, before, "recipe value untouched by develop");
        assert_eq!(with_curve_authored[0].colour, without[0].colour);
        assert_eq!(px(&without[0], 4, 2, 2), [0.75; 3]);

        // enabled: composite Hermite at 0.75 -> segment [0.5,1], m = [0.5, 1.0, 1.5], t = 0.5:
        // A = 0.125, B = 0.0625, C = 0.5, D = -0.09375 -> 0.59375 (exact).
        let mut enabled = disabled.clone();
        enabled.enables.enable_tone_curve = true;
        let (receipt, tiles) = run(&plane, &enabled, 4);
        receipt.unwrap();
        assert_eq!(px(&tiles[0], 4, 1, 3), [0.59375; 3]);
        assert_eq!(enabled.curves.composite.points(), tone.points(), "authored points retained");

        // exact control point returns the authored y: 32/64 = 0.5 -> 63.75/255 = 0.25.
        let half = CfaPlane::new(init(4, 4, &[32; 16])).unwrap();
        let (_, tiles) = run(&half, &enabled, 4);
        assert_eq!(px(&tiles[0], 4, 0, 0), [0.25; 3]);

        // an enabled unsupported group refuses before any tile is emitted.
        let mut detail = recipe();
        detail.enables.enable_detail = true;
        let mut calls = 0;
        let (result, tiles) = run_with(
            &plane,
            &detail,
            grid((0, 0), 4),
            (0, 0),
            ExecutionHint::Scalar,
            &CancellationToken::default(),
            |_| {
                calls += 1;
                Ok(())
            },
        );
        assert_eq!(result, Err(DevelopError::UnsupportedContribution("detail")));
        assert_eq!((calls, tiles.len()), (0, 0));
    });
}

#[test]
fn recipe_reedit_preserves_original() {
    bounded_case("recipe_reedit_preserves_original", || {
        let plane = CfaPlane::new(init(4, 4, &[32; 16])).unwrap(); // 0.5
        let original = plane.bytes().to_vec();
        let mut receipts = Vec::new();
        for (exposure, want) in [(0.0, 0.5f32), (1.0, 1.0), (-1.0, 0.25)] {
            let mut r = recipe();
            r.exposure_stops = exposure;
            let (receipt, tiles) = run(&plane, &r, 4);
            receipts.push(receipt.unwrap());
            assert_eq!(px(&tiles[0], 4, 3, 3), [want; 3], "exposure {exposure}");
        }
        assert_eq!(plane.bytes(), &original[..], "original plane bytes unchanged");
        assert!(receipts.iter().all(|r| r.source_sha256 == plane.sha256()));
        assert_ne!(plane.sha256(), [0u8; 32]);
        assert!(receipts.iter().all(|r| r.math_token == MATH_TOKEN && r.process_version == (1, 0)));
    });
}

#[test]
fn unsupported_process_version_and_cancel_stale() {
    bounded_case("unsupported_process_version_and_cancel_stale", || {
        let plane = CfaPlane::new(init(4, 4, &matrix_samples())).unwrap();
        let original = plane.bytes().to_vec();
        let refused = |mutate: &dyn Fn(&mut DevelopRecipe), want: DevelopError| {
            let mut r = recipe();
            mutate(&mut r);
            let mut calls = 0;
            let (result, _) = run_with(
                &plane,
                &r,
                grid((0, 0), 4),
                (0, 0),
                ExecutionHint::Scalar,
                &CancellationToken::default(),
                |_| {
                    calls += 1;
                    Ok(())
                },
            );
            assert_eq!(result, Err(want));
            assert_eq!(calls, 0, "refusal precedes any callback");
        };
        refused(
            &|r| {
                r.process_version = (15, 4);
                r.engine_version = (16, 4);
            },
            DevelopError::UnsupportedProcessVersion,
        );
        refused(&|r| r.math_token = "lightcraft_v2".into(), DevelopError::UnsupportedMathToken);
        refused(&|r| r.demosaic_algorithm = "ahd".into(), DevelopError::UnsupportedDemosaic);

        // stale expected revision.
        let r = recipe();
        let stale = develop(
            &DevelopRequest {
                plane: &plane,
                recipe: &r,
                recipe_revision: 1,
                expected_source_revision: 2,
                grid: grid((0, 0), 4),
                output_origin: (0, 0),
                execution_hint: ExecutionHint::Scalar,
            },
            &CancellationToken::default(),
            &mut |_| Ok(()),
        );
        assert_eq!(stale, Err(DevelopError::StaleSource));

        // pre-cancelled.
        let cancel = CancellationToken::default();
        cancel.cancel();
        let (result, tiles) = run_with(&plane, &r, grid((0, 0), 4), (0, 0), ExecutionHint::Scalar, &cancel, |_| Ok(()));
        assert_eq!((result, tiles.len()), (Err(DevelopError::Canceled), 0));

        // mid-run cancel: 4 tiles of 2x2, the sink cancels after the first -> Canceled, no receipt.
        let cancel = CancellationToken::default();
        let trigger = cancel.clone();
        let mut seen = 0;
        let (result, tiles) = run_with(&plane, &r, grid((0, 0), 2), (0, 0), ExecutionHint::Scalar, &cancel, |_| {
            seen += 1;
            trigger.cancel();
            Ok(())
        });
        assert_eq!(result, Err(DevelopError::Canceled));
        assert_eq!((seen, tiles.len()), (1, 1));

        // sink refusal -> SinkRejected, no receipt.
        let (result, _) = run_with(&plane, &r, grid((0, 0), 4), (0, 0), ExecutionHint::Scalar, &CancellationToken::default(), |_| Err(SinkError));
        assert_eq!(result, Err(DevelopError::SinkRejected));

        assert_eq!(plane.bytes(), &original[..], "plane unchanged after every failure");
        // missing container decoder is explicit.
        assert_eq!(decode_container(b"II*\0").err(), Some(DevelopError::DecoderUnavailable));
        assert_eq!(DevelopError::UnsupportedProcessVersion.code(), "unsupported_process_version");
    });
}

#[test]
fn limits_and_range() {
    bounded_case("limits_and_range", || {
        let bad = |mutate: &dyn Fn(&mut PlaneInit), want: DevelopError| {
            let mut i = init(4, 4, &[32; 16]);
            mutate(&mut i);
            assert_eq!(CfaPlane::new(i).err(), Some(want));
        };
        bad(&|i| i.width = 1, DevelopError::InvalidDimensions);
        bad(&|i| i.width = 257, DevelopError::InvalidDimensions);
        bad(&|i| i.cfa.pattern = CfaPattern::XTrans, DevelopError::InvalidDimensions); // 4 < 6
        bad(&|i| i.stride_bytes = 9, DevelopError::InvalidStride);
        bad(&|i| i.stride_bytes = 6, DevelopError::InvalidStride); // < 2*width
        bad(&|i| i.stride_bytes = 1026, DevelopError::InvalidStride);
        bad(
            &|i| {
                i.bytes.pop();
            },
            DevelopError::InvalidLength,
        );
        bad(&|i| i.cfa.phase_x = 2, DevelopError::InvalidPhase);
        bad(&|i| i.black = [0, 64, 0], DevelopError::InvalidLevels); // white == black
        bad(&|i| i.black = [40, 40, 40], DevelopError::SampleOutOfRange); // 32 < 40
        bad(&|i| i.white = [31, 31, 31], DevelopError::SampleOutOfRange); // 32 > 31
        bad(&|i| i.sensor_origin = (i64::MAX - 2, 0), DevelopError::Overflow);
        // padding is never a sample: stride 10 with garbage padding admits and keeps all bytes hashed.
        let mut padded = init(4, 4, &[32; 16]);
        padded.stride_bytes = 10;
        padded.bytes = (0..4).flat_map(|_| [32u16, 32, 32, 32, 0xFFFF]).flat_map(|s| s.to_le_bytes()).collect();
        let padded = CfaPlane::new(padded).unwrap();
        let (receipt, tiles) = run(&padded, &recipe(), 4);
        receipt.unwrap();
        assert_eq!(px(&tiles[0], 4, 2, 1), [0.5; 3]);

        // numeric refusals (nothing clipped or zeroed).
        let plane = CfaPlane::new(init(4, 4, &[32; 16])).unwrap();
        let refuse = |mutate: &dyn Fn(&mut DevelopRecipe), tile: u32, want: DevelopError| {
            let mut r = recipe();
            mutate(&mut r);
            assert_eq!(run(&plane, &r, tile).0.err(), Some(want));
        };
        refuse(&|r| r.camera_to_profile_rgb[0] = 1e-300, 4, DevelopError::StageUnderflow);
        refuse(&|r| r.camera_to_profile_rgb = [0.0; 9], 4, DevelopError::NotInvertibleMatrix);
        refuse(&|r| r.camera_to_profile_rgb[4] = f64::NAN, 4, DevelopError::NonFinite);
        refuse(&|r| r.exposure_stops = 2000.0, 4, DevelopError::StageOverflow);
        refuse(&|r| r.exposure_stops = f64::INFINITY, 4, DevelopError::NonFinite);
        refuse(&|r| r.as_shot_neutral = [1.0, 0.0, 1.0], 4, DevelopError::NonFinite);
        refuse(&|r| r.white_balance_mode = WbMode::Auto, 4, DevelopError::UnsupportedWhiteBalance);
        refuse(&|r| r.crop.angle = 1.0, 4, DevelopError::UnsupportedCrop);
        refuse(&|r| r.crop.left = 1.0, 4, DevelopError::EmptyCrop);
        refuse(&|r| r.curve_refine_saturation = 50, 4, DevelopError::UnsupportedCurve);
        refuse(&|r| r.output_profile.sha256 = [9; 32], 4, DevelopError::UnsupportedProfileTransform);
        // a 1x1 grid over a 4x4 crop needs 16 tiles (ok); 5x5 would need 25 > 16 -> TooManyTiles.
        assert_eq!(run(&plane, &recipe(), 1).0.unwrap().tiles, 16);
        let wide = CfaPlane::new(init(5, 5, &[32; 25])).unwrap();
        assert_eq!(run(&wide, &recipe(), 1).0.err(), Some(DevelopError::TooManyTiles));
        // SIMD is a typed refusal until a real route lands; Auto records the scalar route.
        let hint = |h| run_with(&plane, &recipe(), grid((0, 0), 4), (0, 0), h, &CancellationToken::default(), |_| Ok(())).0;
        assert_eq!(hint(ExecutionHint::Sse2).err(), Some(DevelopError::UnsupportedExecution));
        assert_eq!(hint(ExecutionHint::Auto).unwrap().route, "scalar");
    });
}
