//! Lane SMB-LR-HEALING-1: develop-scoped heal / clone spot removal and core distraction removal
//! (STU-RAW-015, STU-RAW-124, STU-RAS-143) over pigment's retouch primitives. Oracles: closed-form
//! pixel copies for integer source offsets, pigment's own `heal` / `content_aware::fill` on the same
//! plane, bit identity for disabled groups and across thread counts.
//! NOT_PROVEN: equality with Lightroom Classic spot removal (no Lightroom oracle, D5); the window's
//! spot tool (no develop GUI hunk in this lane).
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::retouch::{self, MAX_ADAPTER_FILLS, MAX_AREA_MASKS, MAX_DISTRACTION_AREAS, MAX_RETOUCH_AREAS, MAX_STACK_DABS};
use hsk_studio_develop::*;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::brush::Dab;
use hsk_studio_pigment::content_aware::{self, ContentAwareOptions, SamplingArea};
use hsk_studio_pigment::retouch::{CloneMap, RetouchLimits, RgbaPlane, StrokeFootprint, heal};
use hsk_studio_pigment::transform_math::ResampleFilter;

const W: u32 = 64;

fn profile() -> ProfileBinding {
    ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(), sha256: [7; 32] }
}

fn plane_from(width: u32, height: u32, sample: impl Fn(u32, u32) -> u16) -> CfaPlane {
    CfaPlane::with_limits(
        PlaneInit {
            width,
            height,
            stride_bytes: width * 2,
            sensor_origin: (0, 0),
            cfa: Cfa { pattern: CfaPattern::Bayer(Bayer::Rggb), phase_x: 0, phase_y: 0 },
            black: [0; 3],
            white: [4095; 3],
            source_revision: 1,
            bytes: (0..height).flat_map(|y| (0..width).map(move |x| (x, y))).flat_map(|(x, y)| sample(x, y).to_le_bytes()).collect(),
        },
        Limits::LARGE,
    )
    .unwrap()
}

/// A textured frame: every 2x2 CFA block has its own level (values well inside 0..1 after develop).
fn textured() -> CfaPlane {
    plane_from(W, W, |x, y| 300 + (((x / 2) * 37 + (y / 2) * 91 + (x % 2) * 13 + (y % 2) * 29) % 23) as u16 * 120)
}

fn recipe(stack: RetouchStack) -> DevelopRecipe {
    let mut r = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile());
    r.enables.enable_retouch = true;
    r.enables.enable_distraction_removal = true;
    r.adjust.retouch = stack;
    r
}

fn render(plane: &CfaPlane, recipe: &DevelopRecipe, threads: Threads) -> Result<DevelopedImage, DevelopError> {
    develop_to_image_threads(plane, recipe, 1, threads, &CancellationToken::default()).map(|r| r.0)
}

fn px(im: &DevelopedImage, x: i64, y: i64) -> [u32; 3] {
    let i = (y as usize * im.width as usize + x as usize) * 3;
    [im.rgb[i].to_bits(), im.rgb[i + 1].to_bits(), im.rgb[i + 2].to_bits()]
}

fn stack_of(areas: Vec<RetouchArea>) -> RetouchStack {
    RetouchStack { areas, ..RetouchStack::default() }
}

fn brush(dabs: Vec<(f64, f64)>, radius: f64) -> Brush {
    Brush { dabs, radius, flow: 1.0, feather: 0.0 }
}

fn adapter() -> AdapterFillRecord {
    AdapterFillRecord {
        adapter_id: "studio.adapter.genfill".into(),
        model_id: "vendor/remove".into(),
        model_version: "3.1.0".into(),
        ran_locally: false,
        fill_method: "generative".into(),
        image_extent: (6000, 4000),
        search_rect: AdapterRect { x: 100, y: 100, width: 800, height: 600 },
        target_rect: AdapterRect { x: 300, y: 250, width: 120, height: 90 },
        black_level: 512.0,
        gamma_type: "linear".into(),
        input_digest: [1; 32],
        patch_digest: [2; 32],
        remap_info: vec!["remap:v1".into()],
        variations: vec![PatchVariation { id: "v0".into(), patch_digest: [3; 32] }, PatchVariation { id: "v1".into(), patch_digest: [4; 32] }],
        selected_variation: Some(1),
    }
}

#[test]
fn retouch_records_carry_every_spec_field() {
    assert!(Adjustments::default().retouch.is_empty(), "STU-RAW-124 lists default to empty");
    let area = RetouchArea {
        spot_type: SpotType::Clone,
        heal_version: 7,
        feather: 0.25,
        opacity: 0.75,
        seed: 0xDEAD_BEEF_0123,
        method: RetouchMethod::Gaussian,
        source_state: SourceState::SourceSetExplicitly,
        masks: vec![brush(vec![(0.25, 0.5), (0.3, 0.5)], 0.05)],
        source_x: 0.75,
        source_y: 0.5,
        offset_y: 0.125,
        centre_x: 0.25,
        centre_y: 0.5,
        radius: 0.05,
    };
    let r = recipe(RetouchStack { areas: vec![area.clone()], distraction: vec![DistractionArea { masks: vec![brush(vec![(0.5, 0.5)], 0.05)], seed: 9 }], adapter_fills: Vec::new() });
    r.validate().unwrap();
    let kept = &r.adjust.retouch.areas[0];
    assert_eq!(
        (kept.spot_type, kept.heal_version, kept.feather, kept.opacity, kept.seed, kept.method, kept.source_state),
        (SpotType::Clone, 7, 0.25, 0.75, 0xDEAD_BEEF_0123, RetouchMethod::Gaussian, SourceState::SourceSetExplicitly)
    );
    assert_eq!(
        (kept.source_x, kept.source_y, kept.offset_y, kept.centre_x, kept.centre_y, kept.radius),
        (0.75, 0.5, 0.125, 0.25, 0.5, 0.05)
    );
    assert_eq!(kept.masks, area.masks, "target dab masks in the STU-RAW-121 geometry");
    for (k, v) in [("heal", SpotType::Heal), ("clone", SpotType::Clone)] {
        assert_eq!((SpotType::from_key(k), v.key()), (Some(v), k));
    }
    for (k, v) in [("source_set_explicitly", SourceState::SourceSetExplicitly), ("engine_chosen", SourceState::EngineChosen)] {
        assert_eq!((SourceState::from_key(k), v.key()), (Some(v), k));
    }
    assert_eq!((RetouchMethod::from_key("gaussian"), RetouchMethod::from_key("box")), (Some(RetouchMethod::Gaussian), None));
    // The develop run leaves the recipe as authored (immutable value).
    let before = r.clone();
    render(&textured(), &r, Threads::single()).unwrap();
    assert_eq!(r, before);
    // Retouch and distraction removal no longer refuse; red-eye still does (no STU-RAW-124 fields).
    let mut empty = recipe(RetouchStack::default());
    empty.validate().unwrap();
    empty.enables.enable_red_eye = true;
    assert_eq!(empty.validate(), Err(DevelopError::UnsupportedContribution("red_eye")));
}

#[test]
fn hostile_retouch_lists_are_refused_before_allocation() {
    let spot = RetouchArea::circle(SpotType::Heal, (0.25, 0.5), 0.05, (0.75, 0.5));
    let invalid = |s: RetouchStack| s.validate();
    assert_eq!(invalid(stack_of(vec![spot.clone(); MAX_RETOUCH_AREAS + 1])), Err(DevelopError::InvalidMask));
    assert_eq!(invalid(stack_of(vec![spot.clone(); MAX_RETOUCH_AREAS])), Ok(()));
    let hole = DistractionArea { masks: vec![brush(vec![(0.5, 0.5)], 0.05)], seed: 0 };
    assert_eq!(invalid(RetouchStack { distraction: vec![hole; MAX_DISTRACTION_AREAS + 1], ..RetouchStack::default() }), Err(DevelopError::InvalidMask));
    assert_eq!(invalid(RetouchStack { adapter_fills: vec![adapter(); MAX_ADAPTER_FILLS + 1], ..RetouchStack::default() }), Err(DevelopError::InvalidMask));
    let many_masks = RetouchArea { masks: vec![brush(vec![(0.5, 0.5)], 0.05); MAX_AREA_MASKS + 1], ..spot.clone() };
    assert_eq!(invalid(stack_of(vec![many_masks])), Err(DevelopError::InvalidMask));
    // Aggregate dabs: 16 masks of 65536 dabs reach the stack budget exactly; one more dab is refused.
    let full = RetouchArea { masks: vec![brush(vec![(0.5, 0.5); MAX_STACK_DABS / MAX_AREA_MASKS], 0.01); MAX_AREA_MASKS], ..spot.clone() };
    let one_more = RetouchArea { masks: vec![brush(vec![(0.5, 0.5)], 0.01)], ..spot.clone() };
    assert_eq!(invalid(stack_of(vec![full, one_more])), Err(DevelopError::InvalidMask));
    // Field ranges: never clamped.
    let cases: [(RetouchArea, DevelopError); 6] = [
        (RetouchArea { feather: f64::NAN, ..spot.clone() }, DevelopError::NonFinite),
        (RetouchArea { opacity: 1.5, ..spot.clone() }, DevelopError::InvalidMask),
        (RetouchArea { radius: 0.0, ..spot.clone() }, DevelopError::InvalidMask),
        (RetouchArea { centre_x: 100.0, ..spot.clone() }, DevelopError::InvalidMask),
        (RetouchArea { offset_y: f64::INFINITY, ..spot.clone() }, DevelopError::NonFinite),
        (RetouchArea { masks: vec![Brush { dabs: vec![(0.5, 0.5)], radius: 2.0, flow: 1.0, feather: 0.0 }], ..spot.clone() }, DevelopError::InvalidMask),
    ];
    for (area, code) in cases {
        assert_eq!(invalid(stack_of(vec![area.clone()])), Err(code), "{area:?}");
        // Refused by the recipe even while the group is disabled (validation is gate independent).
        let mut r = recipe(stack_of(vec![area]));
        r.enables.enable_retouch = false;
        assert_eq!(r.validate(), Err(code));
    }
}

#[test]
fn adapter_fill_records_are_classified_not_rendered() {
    let record = adapter();
    record.validate().unwrap();
    assert_eq!(AdapterFillRecord::CLASS, "studio_model_adapter");
    let refused = record.render().unwrap_err();
    assert_eq!(refused, DevelopError::ModelUnavailable("distraction_adapter_fill"));
    assert_eq!((refused.code(), refused.to_string()), ("model_unavailable", "model_unavailable:distraction_adapter_fill".to_string()));
    let bad = [
        AdapterFillRecord { adapter_id: String::new(), ..adapter() },
        AdapterFillRecord { model_version: "x".repeat(retouch::MAX_RECORD_TEXT + 1), ..adapter() },
        AdapterFillRecord { selected_variation: Some(2), ..adapter() },
        AdapterFillRecord { target_rect: AdapterRect { x: 5990, y: 0, width: 11, height: 1 }, ..adapter() },
        AdapterFillRecord { search_rect: AdapterRect { x: u32::MAX, y: 0, width: 2, height: 1 }, ..adapter() },
        AdapterFillRecord { black_level: -1.0, ..adapter() },
    ];
    for record in bad {
        assert!(record.validate().is_err(), "{record:?}");
        assert_ne!(record.render(), Err(DevelopError::ModelUnavailable("distraction_adapter_fill")));
    }
    // Retained (and the image unchanged) while distraction removal is disabled.
    let plane = textured();
    let plain = render(&plane, &recipe(RetouchStack::default()), Threads::single()).unwrap();
    let mut r = recipe(RetouchStack { adapter_fills: vec![adapter()], ..RetouchStack::default() });
    r.enables.enable_distraction_removal = false;
    assert_eq!(render(&plane, &r, Threads::single()).unwrap(), plain);
    assert_eq!(r.adjust.retouch.adapter_fills, vec![adapter()]);
}

#[test]
fn adapter_path_reports_model_unavailable() {
    let r = recipe(RetouchStack { adapter_fills: vec![adapter()], ..RetouchStack::default() });
    let err = render(&textured(), &r, Threads::single()).unwrap_err();
    assert_eq!(err, DevelopError::ModelUnavailable("distraction_adapter_fill"));
    assert_eq!(r.validate(), Err(DevelopError::ModelUnavailable("distraction_adapter_fill")));
}

#[test]
fn clone_area_copies_the_source_offset_exactly() {
    let plane = textured();
    // Centre (16, 32) px, source (48, 32) px: the target reads the pixel 32 columns to its right.
    let area = RetouchArea::circle(SpotType::Clone, (0.25, 0.5), 0.1, (0.75, 0.5));
    let cov = retouch::area_coverage(&area, W, W).unwrap().unwrap();
    for scale in [0.0, 3.0] {
        let mut base = recipe(RetouchStack::default());
        base.exposure_stops = scale;
        let plain = render(&plane, &base, Threads::single()).unwrap();
        if scale > 0.0 {
            assert!(plain.rgb.iter().any(|v| *v > 1.0), "scene-linear values above 1 in the frame");
        }
        let mut spot = base.clone();
        spot.adjust.retouch = stack_of(vec![area.clone()]);
        let img = render(&plane, &spot, Threads::single()).unwrap();
        let mut copied = 0;
        for y in 0..W as i64 {
            for x in 0..W as i64 {
                match cov.at(x, y) {
                    0 => assert_eq!(px(&img, x, y), px(&plain, x, y), "outside the mask at ({x}, {y})"),
                    u16::MAX => {
                        assert_eq!(px(&img, x, y), px(&plain, x + 32, y), "exact copy at ({x}, {y}), exposure {scale}");
                        copied += 1;
                    }
                    c => panic!("hard circle has partial coverage {c} at ({x}, {y})"),
                }
            }
        }
        assert!(copied > 100, "{copied} pixels copied");
    }
    // The tile API cannot run the whole-frame stage.
    let r = recipe(stack_of(vec![area.clone()]));
    let mut calls = 0;
    let refused = develop(
        &DevelopRequest {
            plane: &plane,
            recipe: &r,
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: hsk_studio_pigment::Grid { origin_x: 0, origin_y: 0, tile_width: 16, tile_height: 16 },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Scalar,
        },
        &CancellationToken::default(),
        &mut |_| {
            calls += 1;
            Ok(())
        },
    );
    assert_eq!((refused.err(), calls), (Some(DevelopError::UnsupportedContribution("retouch_whole_frame")), 0));
    // A value the [0, 1] plane cannot represent (negative) is refused, never clipped.
    let mut negative = recipe(stack_of(vec![area.clone()]));
    negative.camera_to_profile_rgb = [1.0, 0.0, 0.0, -2.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let mut base = negative.clone();
    base.adjust.retouch = RetouchStack::default();
    assert!(render(&plane, &base, Threads::single()).unwrap().rgb.iter().any(|v| *v < 0.0));
    assert_eq!(render(&plane, &negative, Threads::single()).unwrap_err(), DevelopError::UnsupportedContribution("retouch_negative_value"));
    // A source footprint that leaves the frame is refused.
    let outside = recipe(stack_of(vec![RetouchArea::circle(SpotType::Clone, (0.25, 0.5), 0.1, (0.98, 0.5))]));
    assert_eq!(render(&plane, &outside, Threads::single()).unwrap_err(), DevelopError::UnsupportedContribution("retouch_source_outside_frame"));
}

#[test]
fn heal_area_matches_pigment_heal_on_the_same_plane() {
    let plane = textured();
    let plain = render(&plane, &recipe(RetouchStack::default()), Threads::single()).unwrap();
    assert!(plain.rgb.iter().all(|v| (0.0..=1.0).contains(v)), "fixture inside the plane domain");
    // Centre (16, 24) px, source (48, 40) px: offset (-32, -16); feathered, 80 % opacity.
    let area = RetouchArea { feather: 0.5, opacity: 0.8, ..RetouchArea::circle(SpotType::Heal, (0.25, 0.375), 0.12, (0.75, 0.625)) };
    let img = render(&plane, &recipe(stack_of(vec![area.clone()])), Threads::single()).unwrap();
    let cov = retouch::area_coverage(&area, W, W).unwrap().unwrap();
    assert!(cov.data.iter().any(|c| *c > 0 && *c < u16::MAX), "feather and opacity give partial coverage");
    let full = Rect { x: 0, y: 0, width: W, height: W };
    let rgba = RgbaPlane::new(full, plain.rgb.chunks_exact(3).map(|p| [p[0], p[1], p[2], 1.0]).collect()).unwrap();
    let map = CloneMap::offset(-32.0, -16.0, ResampleFilter::Bilinear).unwrap();
    let dabs = [Dab { x: 32.0, y: 32.0, diameter: 200.0, opacity: 1.0, flow: 1.0, angle: 0.0, roundness: 1.0 }];
    let footprint = StrokeFootprint { dabs: &dabs, hardness_pct: 100.0, selection: Some(&cov) };
    let healed = heal(&rgba, &rgba, &map, &footprint, &RetouchLimits::default()).unwrap();
    assert!(healed.receipt.converged, "{:?}", healed.receipt);
    let mut changed = 0;
    for y in 0..W as i64 {
        for x in 0..W as i64 {
            let expected = if cov.at(x, y) > 0 {
                let p = healed.result.plane.at(x, y);
                [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()]
            } else {
                px(&plain, x, y)
            };
            assert_eq!(px(&img, x, y), expected, "({x}, {y})");
            changed += usize::from(px(&img, x, y) != px(&plain, x, y));
        }
    }
    assert!(changed > 50, "{changed} pixels healed");
}

#[test]
fn disabled_retouch_keeps_areas_and_changes_nothing() {
    let plane = textured();
    let stack = RetouchStack {
        areas: vec![
            RetouchArea::circle(SpotType::Clone, (0.25, 0.5), 0.1, (0.75, 0.5)),
            RetouchArea::circle(SpotType::Heal, (0.5, 0.25), 0.08, (0.5, 0.75)),
        ],
        distraction: vec![DistractionArea { masks: vec![brush(vec![(0.5, 0.5)], 0.06)], seed: 3 }],
        adapter_fills: Vec::new(),
    };
    let plain = render(&plane, &recipe(RetouchStack::default()), Threads::single()).unwrap();
    let mut off = recipe(stack.clone());
    off.enables.enable_retouch = false;
    off.enables.enable_distraction_removal = false;
    let img = render(&plane, &off, Threads::single()).unwrap();
    assert_eq!(img.rgb.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), plain.rgb.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
    assert_eq!(off.adjust.retouch, stack, "areas retained while disabled");
    // The tile API accepts the recipe while the groups are disabled.
    let receipt = develop(
        &DevelopRequest {
            plane: &plane,
            recipe: &off,
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: hsk_studio_pigment::Grid { origin_x: 0, origin_y: 0, tile_width: 16, tile_height: 16 },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Scalar,
        },
        &CancellationToken::default(),
        &mut |_| Ok(()),
    );
    assert!(receipt.is_ok(), "{receipt:?}");
    // Enabled, the same stack changes the frame.
    assert_ne!(render(&plane, &recipe(stack), Threads::single()).unwrap(), plain);
}

#[test]
fn retouch_is_deterministic_across_threads() {
    let plane = textured();
    let engine = |spot_type| RetouchArea { source_state: SourceState::EngineChosen, ..RetouchArea::circle(spot_type, (0.5, 0.5), 0.06, (0.5, 0.5)) };
    let stack = RetouchStack {
        areas: vec![
            RetouchArea { feather: 0.4, ..RetouchArea::circle(SpotType::Clone, (0.2, 0.2), 0.08, (0.37, 0.29)) },
            RetouchArea::circle(SpotType::Heal, (0.8, 0.25), 0.07, (0.6, 0.4)),
            engine(SpotType::Heal),
            RetouchArea { masks: vec![brush(vec![(0.3, 0.8), (0.35, 0.82), (0.4, 0.8)], 0.04)], ..engine(SpotType::Clone) },
        ],
        distraction: vec![DistractionArea { masks: vec![Brush { dabs: vec![(0.75, 0.75)], radius: 0.06, flow: 1.0, feather: 0.3 }], seed: 11 }],
        adapter_fills: Vec::new(),
    };
    let r = recipe(stack);
    let one = render(&plane, &r, Threads::single()).unwrap();
    for n in [2, 4, 7] {
        let many = render(&plane, &r, Threads::new(n).unwrap()).unwrap();
        assert_eq!(many.rgb.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), one.rgb.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), "{n} threads");
    }
    assert_eq!(render(&plane, &r, Threads::single()).unwrap(), one, "repeat run");
    assert_ne!(one, render(&plane, &recipe(RetouchStack::default()), Threads::single()).unwrap());
}

#[test]
fn distraction_area_fills_through_the_native_inpaint() {
    // Flat background with a bright 6x6 blob; the hole covers the blob and its demosaic fringe.
    let plane = plane_from(W, W, |x, y| if (30..36).contains(&x) && (30..36).contains(&y) { 3600 } else { 1000 });
    let area = DistractionArea { masks: vec![brush(vec![(33.0 / 64.0, 33.0 / 64.0)], 0.1)], seed: 5 };
    let plain = render(&plane, &recipe(RetouchStack::default()), Threads::single()).unwrap();
    let img = render(&plane, &recipe(RetouchStack { distraction: vec![area.clone()], ..RetouchStack::default() }), Threads::single()).unwrap();
    let hole = retouch::distraction_coverage(&area, W, W).unwrap().unwrap();
    let background = px(&plain, 2, 2);
    // Oracle 1: pigment's native fill on the same window and options gives the same bytes.
    let margin = hole.bounds.width.max(hole.bounds.height).max(retouch::DISTRACTION_CONTEXT_MIN_PX) as i64;
    let x0 = (hole.bounds.x - margin).max(0);
    let y0 = (hole.bounds.y - margin).max(0);
    let x1 = (hole.bounds.x + i64::from(hole.bounds.width) + margin).min(i64::from(W));
    let y1 = (hole.bounds.y + i64::from(hole.bounds.height) + margin).min(i64::from(W));
    let window = Rect { x: x0, y: y0, width: (x1 - x0) as u32, height: (y1 - y0) as u32 };
    let mut data = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y as usize * W as usize + x as usize) * 3;
            data.push([plain.rgb[i], plain.rgb[i + 1], plain.rgb[i + 2], 1.0]);
        }
    }
    let target = RgbaPlane::new(window, data).unwrap();
    let options = ContentAwareOptions { seed: 5, threads: 1, ..ContentAwareOptions::default() };
    let limits = RetouchLimits { max_bytes: retouch::RETOUCH_MAX_BYTES, ..RetouchLimits::default() };
    let filled = content_aware::fill(&target, &hole, SamplingArea::Auto, &options, &limits).unwrap();
    let mut in_hole = 0;
    for y in 0..W as i64 {
        for x in 0..W as i64 {
            if hole.at(x, y) == 0 {
                assert_eq!(px(&img, x, y), px(&plain, x, y), "outside the hole at ({x}, {y})");
                continue;
            }
            in_hole += 1;
            let p = filled.result.plane.at(x, y);
            assert_eq!(px(&img, x, y), [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()], "pigment fill at ({x}, {y})");
            // Oracle 2 (closed form): sources are only the flat background, so the blob is gone.
            let got = px(&img, x, y).map(f32::from_bits);
            let want = background.map(f32::from_bits);
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() <= 1.0e-5, "({x}, {y}): {got:?} vs background {want:?}");
            }
        }
    }
    assert!(in_hole > 100, "{in_hole} hole pixels");
    assert!(px(&plain, 33, 33) != background, "the blob was visible before the fill");
}
