//! Thread-count independence of the develop engine: tiles, receipts, refusals and converter calls
//! are identical for every `Threads` value; cancellation returns `Canceled` and no partial output.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_pigment::Grid;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::ThreadId;

const BLACK: [u16; 3] = [64, 80, 96];
const WHITE: [u16; 3] = [4095, 4000, 3900];
const COUNTS: [usize; 4] = [1, 2, 3, 8];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn limits() -> Limits {
    Limits {
        max_tile_edge: 64,
        ..Limits::LARGE
    }
}

fn plane_from(
    pattern: CfaPattern,
    (width, height): (u32, u32),
    origin: (i64, i64),
    phase: (u8, u8),
    (black, white): ([u16; 3], [u16; 3]),
    sample: impl Fn(u32, u32, u64) -> u16,
) -> CfaPlane {
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ (u64::from(width) << 32) ^ u64::from(height);
    let mut bytes = Vec::with_capacity(width as usize * height as usize * 2);
    for y in 0..height {
        for x in 0..width {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            bytes.extend_from_slice(&sample(x, y, state >> 33).to_le_bytes());
        }
    }
    CfaPlane::with_limits(
        PlaneInit {
            width,
            height,
            stride_bytes: width * 2,
            sensor_origin: origin,
            cfa: Cfa { pattern, phase_x: phase.0, phase_y: phase.1 },
            black,
            white,
            source_revision: 1,
            bytes,
        },
        limits(),
    )
    .unwrap()
}

/// Pseudo-random samples inside every channel's range with the extremes mixed in.
fn noisy(pattern: CfaPattern, width: u32, height: u32, origin: (i64, i64), phase: (u8, u8)) -> CfaPlane {
    plane_from(pattern, (width, height), origin, phase, (BLACK, WHITE), |x, y, r| {
        match (x + y * width) % 41 {
            0 => 96,
            1 => 3900,
            _ => 100 + (r % 2901) as u16,
        }
    })
}

fn profile(n: u8) -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse(&format!("SCPF-019abcde-0000-7000-8000-0000000000{n:02}")).unwrap(),
        sha256: [n; 32],
    }
}

fn curve_recipe() -> DevelopRecipe {
    let mut r = DevelopRecipe::native([1.0; 3], [0.5, 0.3, 0.2, 0.2, 0.6, 0.2, 0.1, 0.3, 0.6], profile(1));
    r.exposure_stops = -0.5;
    r.enables.enable_tone_curve = true;
    r.curves.composite = Curve::new(vec![(0.0, 0.0), (64.0, 48.0), (192.0, 208.0), (255.0, 255.0)]).unwrap();
    r.curves.red = Curve::new(vec![(0.0, 0.0), (100.0, 120.0), (255.0, 255.0)]).unwrap();
    r.crop.left = 0.1;
    r.crop.top = 0.15;
    r.crop.right = 0.9;
    r.crop.bottom = 0.8;
    r
}

fn wide_recipe() -> DevelopRecipe {
    DevelopRecipe::native([0.5, 1.0, 0.6], [1.6, -0.4, -0.2, -0.3, 1.5, -0.2, -0.1, -0.4, 1.5], profile(1))
}

fn request<'a>(plane: &'a CfaPlane, recipe: &'a DevelopRecipe, edge: u32) -> DevelopRequest<'a> {
    DevelopRequest {
        plane,
        recipe,
        recipe_revision: 7,
        expected_source_revision: 1,
        grid: Grid { origin_x: 3, origin_y: -2, tile_width: edge, tile_height: edge },
        output_origin: (5, 1),
        execution_hint: ExecutionHint::Auto,
    }
}

fn f32_digest(values: &[f32]) -> String {
    let mut hasher = Sha256::new();
    for v in values {
        hasher.update(v.to_le_bytes());
    }
    hex(&hasher.finalize())
}

/// Digest of every delivered tile (in delivery order) plus the receipt, or the refusal.
fn run_digest(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    edge: u32,
    converter: Option<&mut dyn ProfileConverter>,
    threads: usize,
) -> Result<(String, DevelopReceipt), DevelopError> {
    let mut hasher = Sha256::new();
    let receipt = develop_with_threads(
        &request(plane, recipe, edge),
        converter,
        Threads::new(threads).unwrap(),
        &CancellationToken::default(),
        &mut |t| {
            hasher.update(format!("{:?}{:?}", t.bounds, t.developed).as_bytes());
            hasher.update(&t.colour);
            hasher.update(&t.alpha);
            hasher.update(&t.coverage);
            Ok(())
        },
    )?;
    Ok((hex(&hasher.finalize()), receipt))
}

#[test]
fn output_is_identical_for_every_thread_count() {
    let bayer = |b| CfaPattern::Bayer(b);
    // Several normalisation/demosaic row bands (400 wide -> 163 rows per band) and many tiles.
    let planes = [
        ("rggb_400x480", noisy(bayer(Bayer::Rggb), 400, 480, (0, 0), (0, 0))),
        ("gbrg_203x251_o-3_7_p1_1", noisy(bayer(Bayer::Gbrg), 203, 251, (-3, 7), (1, 1))),
        ("xtrans_101x99_o5_-2_p3_4", noisy(CfaPattern::XTrans, 101, 99, (5, -2), (3, 4))),
        ("bggr_7x6", noisy(bayer(Bayer::Bggr), 7, 6, (0, 0), (0, 0))),
    ];
    assert_eq!(Threads::single().get(), 1);
    assert!((1..=DEFAULT_THREAD_CAP).contains(&Threads::auto().get()));
    for bad in [0, MAX_THREADS + 1, usize::MAX] {
        assert_eq!(Threads::new(bad), Err(DevelopError::InvalidLimits));
    }
    assert_eq!(Threads::new(MAX_THREADS).unwrap().get(), MAX_THREADS);
    let cancel = CancellationToken::default();
    for (name, plane) in &planes {
        let reference_demosaic = demosaic_normalized_threads(plane, Threads::single(), &cancel).unwrap();
        for recipe in [curve_recipe(), wide_recipe()] {
            let (reference_tiles, reference_receipt) = run_digest(plane, &recipe, 24, None, 1).unwrap();
            let (reference_image, image_receipt) =
                develop_to_image_threads(plane, &recipe, 7, Threads::single(), &cancel).unwrap();
            for threads in COUNTS {
                let t = Threads::new(threads).unwrap();
                let demosaic = demosaic_normalized_threads(plane, t, &cancel).unwrap();
                assert_eq!(
                    f32_digest(demosaic.as_flattened()),
                    f32_digest(reference_demosaic.as_flattened()),
                    "{name} demosaic with {threads} threads"
                );
                let (tiles, receipt) = run_digest(plane, &recipe, 24, None, threads).unwrap();
                assert_eq!((tiles, receipt), (reference_tiles.clone(), reference_receipt.clone()), "{name} tiles with {threads} threads");
                let (image, receipt) = develop_to_image_threads(plane, &recipe, 7, t, &cancel).unwrap();
                assert_eq!(receipt, image_receipt, "{name} image receipt with {threads} threads");
                assert_eq!(
                    (image.width, image.height, f32_digest(&image.rgb)),
                    (reference_image.width, reference_image.height, f32_digest(&reference_image.rgb)),
                    "{name} image with {threads} threads"
                );
            }
        }
        // The default entry points (auto threads) agree with the explicit single-thread result.
        let (default_image, _) = develop_to_image(plane, &wide_recipe(), 7, &cancel).unwrap();
        let (single_image, _) = develop_to_image_threads(plane, &wide_recipe(), 7, Threads::single(), &cancel).unwrap();
        assert_eq!(f32_digest(&default_image.rgb), f32_digest(&single_image.rgb), "{name} default entry point");
    }
}

/// Stands in for the Prism converter: records how it is driven, applies a fixed mapping.
struct Recorder {
    source: ProfileBinding,
    destination: ProfileBinding,
    caller: ThreadId,
    calls: u32,
    off_thread: bool,
    firsts: Vec<([f32; 3], usize)>,
    cancel_on_call: Option<(u32, CancellationToken)>,
}

impl Recorder {
    fn new(recipe: &DevelopRecipe) -> Self {
        Self {
            source: recipe.working_profile.clone(),
            destination: recipe.output_profile.clone(),
            caller: std::thread::current().id(),
            calls: 0,
            off_thread: false,
            firsts: Vec::new(),
            cancel_on_call: None,
        }
    }
}

impl ProfileConverter for Recorder {
    fn source(&self) -> &ProfileBinding {
        &self.source
    }
    fn destination(&self) -> &ProfileBinding {
        &self.destination
    }
    fn inspect(&mut self, _cancel: &CancellationToken) -> Result<(), DevelopError> {
        Ok(())
    }
    fn convert(&mut self, pixels: &[[f32; 3]], _cancel: &CancellationToken) -> Result<Vec<[f32; 3]>, DevelopError> {
        self.off_thread |= std::thread::current().id() != self.caller;
        self.calls += 1;
        self.firsts.push((pixels[0], pixels.len()));
        if let Some((n, token)) = &self.cancel_on_call
            && self.calls == *n
        {
            token.cancel();
        }
        Ok(pixels.iter().map(|p| [p[0] * 0.5, p[1] * 0.5 + 0.125, p[2]]).collect())
    }
    fn calls(&self) -> u32 {
        self.calls
    }
}

#[test]
fn converter_is_driven_in_tile_order_on_the_calling_thread() {
    let plane = noisy(CfaPattern::Bayer(Bayer::Grbg), 150, 130, (2, 1), (1, 0));
    let mut recipe = curve_recipe();
    recipe.output_profile = profile(2);
    let mut reference = None;
    for threads in [1, 2, 3, 8, 16] {
        let mut recorder = Recorder::new(&recipe);
        let (digest, receipt) = run_digest(&plane, &recipe, 16, Some(&mut recorder), threads).unwrap();
        assert!(!recorder.off_thread, "converter ran off the calling thread with {threads} threads");
        assert_eq!(receipt.profile_route, "prism_admitted_wide");
        assert_eq!(receipt.profile_calls, receipt.tiles);
        assert_eq!(recorder.calls, receipt.tiles);
        let observed = (digest, receipt, recorder.firsts);
        match &reference {
            None => reference = Some(observed),
            Some(first) => assert_eq!(&observed, first, "{threads} threads differ from 1 thread"),
        }
    }
}

/// One band of rows normalises to 0.5 and the other to 1.0 (`top_high` says which is on top); a
/// matrix with 1.2e38 row sums keeps the 0.5 band finite (so the curve refuses the value above 1: a
/// stage-C failure) while the 1.0 band overflows binary32 in stage A. In tile order the first
/// failure is the curve refusal of tile 0 (`top_high` false) or the overflow of tile 0 (true).
fn failing_plane(top_high: bool) -> CfaPlane {
    let split = if top_high { 128 } else { 192 };
    plane_from(CfaPattern::Bayer(Bayer::Rggb), (64, 320), (0, 0), (0, 0), ([0; 3], [4000; 3]), |_, y, _| {
        if (y < split) == top_high { 4000 } else { 2000 }
    })
}

fn failing_recipe() -> DevelopRecipe {
    let mut r = DevelopRecipe::native(
        [1.0; 3],
        [1.2e38, 1.2e38, 1.2e38, 1.2e38, 1.2e38, 1.1e38, 1.2e38, 1.1e38, 1.2e38],
        profile(1),
    );
    r.enables.enable_tone_curve = true;
    r.curves.composite = Curve::new(vec![(0.0, 0.0), (64.0, 48.0), (192.0, 208.0), (255.0, 255.0)]).unwrap();
    r
}

#[test]
fn lowest_failing_tile_decides_the_error_for_every_thread_count() {
    let recipe = failing_recipe();
    recipe.validate().unwrap();
    let mut with_converter = recipe.clone();
    with_converter.output_profile = profile(2);
    // 32-pixel tiles: 2 columns x 10 rows. Low band on top: tile 0 fails in stage C and every tile
    // from row 6 on overflows in stage A. High band on top: tile 0 overflows in stage A and the
    // tiles from row 4 on fail in stage C. The lowest tile's error must win in both layouts, in the
    // fused route and in the converter route (stage A errors held back behind earlier stage C ones).
    for (top_high, expected) in [(false, DevelopError::CurveDomain), (true, DevelopError::StageOverflow)] {
        let plane = failing_plane(top_high);
        for threads in [1, 2, 4, 8, 16] {
            for _ in 0..6 {
                let plain = run_digest(&plane, &recipe, 32, None, threads).err();
                assert_eq!(plain, Some(expected), "fused path, top_high {top_high}, {threads} threads");
                let mut recorder = Recorder::new(&with_converter);
                let batched = run_digest(&plane, &with_converter, 32, Some(&mut recorder), threads).err();
                assert_eq!(batched, Some(expected), "converter path, top_high {top_high}, {threads} threads");
                assert!(!recorder.off_thread);
            }
        }
    }
    // Without curves only the stage-A overflow remains: same error everywhere.
    let mut no_curve = recipe.clone();
    no_curve.enables.enable_tone_curve = false;
    for threads in [1, 2, 8] {
        let plane = failing_plane(false);
        assert_eq!(run_digest(&plane, &no_curve, 32, None, threads).err(), Some(DevelopError::StageOverflow));
    }
}

#[test]
fn cancellation_returns_canceled_and_no_partial_output() {
    let plane = noisy(CfaPattern::Bayer(Bayer::Rggb), 400, 480, (0, 0), (0, 0));
    let recipe = curve_recipe();
    let before = Sha256::digest(plane.bytes());
    for threads in [1, 2, 8] {
        let t = Threads::new(threads).unwrap();
        // Cancelled before the call.
        let cancel = CancellationToken::default();
        cancel.cancel();
        let mut delivered = 0u32;
        let result = develop_with_threads(&request(&plane, &recipe, 24), None, t, &cancel, &mut |_| {
            delivered += 1;
            Ok(())
        });
        assert_eq!((result.err(), delivered), (Some(DevelopError::Canceled), 0), "pre-cancel, {threads} threads");
        assert_eq!(demosaic_normalized_threads(&plane, t, &cancel).err(), Some(DevelopError::Canceled));
        assert_eq!(develop_to_image_threads(&plane, &recipe, 1, t, &cancel).err(), Some(DevelopError::Canceled));

        // Cancelled by another thread while the run is in flight (a run takes far longer than the
        // time the canceller needs to start, so the run always observes it).
        let cancel = CancellationToken::default();
        let started = AtomicBool::new(false);
        let (result, delivered) = std::thread::scope(|scope| {
            scope.spawn(|| {
                while !started.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
                cancel.cancel();
            });
            let mut delivered = 0u32;
            started.store(true, Ordering::Release);
            let result = develop_with_threads(&request(&plane, &recipe, 24), None, t, &cancel, &mut |_| {
                delivered += 1;
                Ok(())
            });
            (result, delivered)
        });
        assert_eq!((result.err(), delivered), (Some(DevelopError::Canceled), 0), "in-flight cancel, {threads} threads");

        // Cancelled from inside the converter on its third call: Canceled, nothing delivered.
        let mut converting = recipe.clone();
        converting.output_profile = profile(2);
        let cancel = CancellationToken::default();
        let mut recorder = Recorder::new(&converting);
        recorder.cancel_on_call = Some((3, cancel.clone()));
        let mut delivered = 0u32;
        let result = develop_with_threads(&request(&plane, &converting, 24), Some(&mut recorder), t, &cancel, &mut |_| {
            delivered += 1;
            Ok(())
        });
        assert_eq!((result.err(), delivered), (Some(DevelopError::Canceled), 0), "converter cancel, {threads} threads");
    }
    assert_eq!(Sha256::digest(plane.bytes()), before, "the plane is never touched");
}
