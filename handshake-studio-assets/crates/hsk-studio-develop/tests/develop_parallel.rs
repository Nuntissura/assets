//! Thread-count independence of the develop engine: tiles, receipts, refusals and converter calls
//! are identical for every `Threads` value; cancellation returns `Canceled` and no partial output.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_pigment::Grid;
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

/// Stands in for the Prism converter: records how its job is prepared and driven, applies a fixed
/// mapping in place.
struct Recorder {
    source: ProfileBinding,
    destination: ProfileBinding,
    caller: ThreadId,
    limits: BandLimits,
    /// Bands converted by finished jobs.
    calls: u32,
    /// Band layout of every `prepare`, in order.
    layouts: Vec<Vec<usize>>,
    /// (band within its job, pixels, converting thread) of every band call, over all jobs.
    log: Mutex<Vec<(usize, usize, ThreadId)>>,
    /// First pixel of each band before conversion, in call order.
    firsts: Mutex<Vec<[f32; 3]>>,
    /// Cancel the token when this many bands (over all jobs) have been converted.
    cancel_on_band: Option<(u32, CancellationToken)>,
    converted: AtomicU32,
}

impl Recorder {
    fn new(recipe: &DevelopRecipe) -> Self {
        Self {
            source: recipe.working_profile.clone(),
            destination: recipe.output_profile.clone(),
            caller: std::thread::current().id(),
            limits: BandLimits::default(),
            calls: 0,
            layouts: Vec::new(),
            log: Mutex::new(Vec::new()),
            firsts: Mutex::new(Vec::new()),
            cancel_on_band: None,
            converted: AtomicU32::new(0),
        }
    }

    fn off_thread(&self) -> bool {
        self.log.lock().unwrap().iter().any(|(_, _, id)| *id != self.caller)
    }
}

struct RecorderJob<'a> {
    sizes: Vec<usize>,
    claimed: Vec<AtomicBool>,
    log: &'a Mutex<Vec<(usize, usize, ThreadId)>>,
    firsts: &'a Mutex<Vec<[f32; 3]>>,
    cancel_on_band: &'a Option<(u32, CancellationToken)>,
    converted: &'a AtomicU32,
    calls: &'a mut u32,
}

impl ConversionJob for RecorderJob<'_> {
    fn convert_band(&self, band: usize, rgb: &mut [[f32; 3]], _cancel: &CancellationToken) -> Result<(), DevelopError> {
        assert_eq!(rgb.len(), self.sizes[band], "band {band} holds exactly its declared pixels");
        assert!(!self.claimed[band].swap(true, Ordering::SeqCst), "band {band} converted twice");
        self.log.lock().unwrap().push((band, rgb.len(), std::thread::current().id()));
        self.firsts.lock().unwrap().push(rgb[0]);
        let done = self.converted.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((n, token)) = self.cancel_on_band
            && done == *n
        {
            token.cancel();
        }
        for p in rgb.iter_mut() {
            *p = [p[0] * 0.5, p[1] * 0.5 + 0.125, p[2]];
        }
        Ok(())
    }
    fn finish(self: Box<Self>, _cancel: &CancellationToken) -> Result<(), DevelopError> {
        if self.claimed.iter().any(|c| !c.load(Ordering::SeqCst)) {
            return Err(DevelopError::Prism("job_incomplete"));
        }
        *self.calls += self.sizes.len() as u32;
        Ok(())
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
    fn band_limits(&self) -> BandLimits {
        self.limits
    }
    fn prepare<'a>(
        &'a mut self,
        bands: &[usize],
        _cancel: &CancellationToken,
    ) -> Result<Box<dyn ConversionJob + 'a>, DevelopError> {
        assert!(!bands.is_empty() && bands.len() <= self.limits.max_bands, "layout within the converter's limits");
        assert!(bands.iter().all(|n| (1..=self.limits.band_pixels).contains(n)), "bands within the converter's limits");
        self.layouts.push(bands.to_vec());
        Ok(Box::new(RecorderJob {
            sizes: bands.to_vec(),
            claimed: bands.iter().map(|_| AtomicBool::new(false)).collect(),
            log: &self.log,
            firsts: &self.firsts,
            cancel_on_band: &self.cancel_on_band,
            converted: &self.converted,
            calls: &mut self.calls,
        }))
    }
    fn calls(&self) -> u32 {
        self.calls
    }
}

#[test]
fn converter_bands_run_once_each_on_the_tile_workers() {
    let plane = noisy(CfaPattern::Bayer(Bayer::Grbg), 150, 130, (2, 1), (1, 0));
    let mut recipe = curve_recipe();
    recipe.output_profile = profile(2);
    let mut reference = None;
    for threads in [1, 2, 3, 8, 16] {
        let mut recorder = Recorder::new(&recipe);
        let (digest, receipt) = run_digest(&plane, &recipe, 16, Some(&mut recorder), threads).unwrap();
        if threads == 1 {
            assert!(!recorder.off_thread(), "one thread converts on the calling thread");
        }
        assert_eq!(receipt.profile_route, "prism_admitted_wide");
        // One job for the whole frame, one band per tile, every band converted exactly once.
        assert_eq!(recorder.layouts.len(), 1, "{threads} threads: one prepare for the frame");
        assert_eq!(recorder.layouts[0].len() as u32, receipt.tiles);
        assert_eq!(receipt.profile_calls, receipt.tiles);
        assert_eq!(recorder.calls, receipt.tiles);
        let mut bands: Vec<usize> = recorder.log.lock().unwrap().iter().map(|(band, ..)| *band).collect();
        assert_eq!(bands.len() as u32, receipt.tiles);
        bands.sort_unstable();
        assert!(bands.iter().copied().eq(0..receipt.tiles as usize), "{threads} threads: bands 0..n once each");
        let workers: std::collections::HashSet<_> = recorder.log.lock().unwrap().iter().map(|(.., id)| *id).collect();
        assert!(workers.len() <= threads, "{threads} threads used {} threads", workers.len());
        // The band inputs do not depend on the schedule: compare them as a set, tile by tile.
        let mut firsts: Vec<[u32; 3]> = recorder.firsts.lock().unwrap().iter().map(|p| p.map(f32::to_bits)).collect();
        firsts.sort_unstable();
        let observed = (digest, receipt, recorder.layouts.clone(), firsts);
        match &reference {
            None => reference = Some(observed),
            Some(first) => assert_eq!(&observed, first, "{threads} threads differ from 1 thread"),
        }
    }
}

/// Tiles above the converter's band size are cut into bands and a layout above its band count is
/// split over several jobs; the pixels do not change.
#[test]
fn oversized_tiles_and_long_layouts_split_into_bands_and_jobs() {
    let plane = noisy(CfaPattern::Bayer(Bayer::Rggb), 150, 130, (0, 0), (0, 0));
    let mut recipe = curve_recipe();
    recipe.output_profile = profile(2);
    for threads in [1, 2, 8] {
        let mut whole = Recorder::new(&recipe);
        let (reference, whole_receipt) = run_digest(&plane, &recipe, 16, Some(&mut whole), threads).unwrap();
        assert_eq!(whole.layouts.len(), 1);

        // 16x16 tiles of 256 pixels in bands of at most 100 (100 + 100 + 56), 40 bands per job.
        let mut cut = Recorder::new(&recipe);
        cut.limits = BandLimits { band_pixels: 100, max_bands: 40 };
        let (digest, receipt) = run_digest(&plane, &recipe, 16, Some(&mut cut), threads).unwrap();
        assert_eq!(digest, reference, "{threads} threads: the cut changes no pixel");
        assert!(cut.layouts.len() > 1, "{threads} threads: {} jobs", cut.layouts.len());
        let total_bands: usize = cut.layouts.iter().map(Vec::len).sum();
        assert!(cut.layouts.iter().all(|layout| layout.len() <= 40));
        assert!(cut.layouts.iter().flatten().all(|n| *n <= 100));
        assert_eq!(cut.layouts.iter().flatten().sum::<usize>() as u64, receipt.pixels);
        assert_eq!(receipt.pixels, whole_receipt.pixels);
        assert_eq!((receipt.profile_calls as usize, cut.calls as usize), (total_bands, total_bands));
        assert_eq!(cut.log.lock().unwrap().len(), total_bands);
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

        // Cancelled from inside the converter on its third band: Canceled, nothing delivered.
        let mut converting = recipe.clone();
        converting.output_profile = profile(2);
        let cancel = CancellationToken::default();
        let mut recorder = Recorder::new(&converting);
        recorder.cancel_on_band = Some((3, cancel.clone()));
        let mut delivered = 0u32;
        let result = develop_with_threads(&request(&plane, &converting, 24), Some(&mut recorder), t, &cancel, &mut |_| {
            delivered += 1;
            Ok(())
        });
        assert_eq!((result.err(), delivered), (Some(DevelopError::Canceled), 0), "converter cancel, {threads} threads");
    }
    assert_eq!(Sha256::digest(plane.bytes()), before, "the plane is never touched");
}
