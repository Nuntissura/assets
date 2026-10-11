//! Working -> output profile conversion through Prism's admitted route (linear-light fixtures).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_pigment::Grid;
use hsk_studio_prism::{
    AdmittedPrism, AdmittedWideEngine, ClipMethod, ProviderAdmission, ProviderReservation,
    profile_hash,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

const SRGB: &[u8] = include_bytes!("profiles/sRGB-admitted-linear-v4.icc");
const P3: &[u8] = include_bytes!("profiles/DisplayP3-admitted-linear-v4.icc");
const ENCODED: &[u8] = include_bytes!("profiles/sRGB-v4.icc");

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => resume_unwind(payload),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}

// ---- caller-owned admission ledger and diagnostics sink ---------------------------------------
/// Live reserved bytes, the highest live total seen, and a cap (`u64::MAX` = unlimited) beyond
/// which `reserve` denies.
struct Ledger {
    live: AtomicU64,
    peak: AtomicU64,
    cap: AtomicU64,
}
impl Default for Ledger {
    fn default() -> Self {
        Self {
            live: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            cap: AtomicU64::new(u64::MAX),
        }
    }
}
#[derive(Clone)]
struct Admission(Arc<Ledger>);
struct Lease {
    ledger: Arc<Ledger>,
    bytes: u64,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.ledger.live.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}
impl ProviderReservation for Lease {
    fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
}
impl ProviderAdmission for Admission {
    type Reservation = Lease;
    fn synchronous_retirement(&self) -> bool {
        true
    }
    fn reserve(&self, bytes: u64) -> Result<Lease, hsk_studio_prism::Error> {
        let after = self.0.live.fetch_add(bytes, Ordering::SeqCst).saturating_add(bytes);
        if after > self.0.cap.load(Ordering::SeqCst) {
            self.0.live.fetch_sub(bytes, Ordering::SeqCst);
            return Err(hsk_studio_prism::Error::AdmissionDenied);
        }
        self.0.peak.fetch_max(after, Ordering::SeqCst);
        Ok(Lease {
            ledger: self.0.clone(),
            bytes,
        })
    }
}
#[derive(Default)]
struct Collect(Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

// ---- independent ICC oracle: RGB->XYZ(D50) from the s15Fixed16 colorant tags only -------------
fn colorants(icc: &[u8]) -> [f64; 9] {
    let be32 = |at: usize| u32::from_be_bytes(icc[at..at + 4].try_into().unwrap());
    let mut columns = [[0.0f64; 3]; 3];
    for i in 0..be32(128) as usize {
        let entry = 132 + 12 * i;
        let column = match &icc[entry..entry + 4] {
            b"rXYZ" => 0,
            b"gXYZ" => 1,
            b"bXYZ" => 2,
            _ => continue,
        };
        let at = be32(entry + 4) as usize;
        assert_eq!(&icc[at..at + 4], b"XYZ ");
        for (row, slot) in columns[column].iter_mut().enumerate() {
            *slot = f64::from(be32(at + 8 + 4 * row) as i32) / 65536.0;
        }
    }
    [
        columns[0][0], columns[1][0], columns[2][0], columns[0][1], columns[1][1], columns[2][1],
        columns[0][2], columns[1][2], columns[2][2],
    ]
}

fn binding(id: &str, icc: &[u8]) -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse(id).unwrap(),
        sha256: profile_hash(icc),
    }
}

/// 16x16 RGGB plane, black 0, white 64, pseudo-random samples 0..=64 (normalised 0..=1).
fn plane() -> CfaPlane {
    let samples: Vec<u16> = (0..256u64)
        .map(|i| ((i.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) % 65) as u16)
        .collect();
    CfaPlane::new(PlaneInit {
        width: 16,
        height: 16,
        stride_bytes: 32,
        sensor_origin: (0, 0),
        cfa: Cfa {
            pattern: CfaPattern::Bayer(Bayer::Rggb),
            phase_x: 0,
            phase_y: 0,
        },
        black: [0; 3],
        white: [64; 3],
        source_revision: 1,
        bytes: samples.iter().flat_map(|s| s.to_le_bytes()).collect(),
    })
    .unwrap()
}

fn recipe(working: &ProfileBinding, output: &ProfileBinding) -> DevelopRecipe {
    let mut r = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], working.clone());
    r.output_profile = output.clone();
    r
}

fn floats(tile: &DevelopedTile) -> Vec<f32> {
    tile.colour.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
}

fn run(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    converter: Option<&mut dyn ProfileConverter>,
    cancel: &CancellationToken,
) -> (Result<DevelopReceipt, DevelopError>, Vec<DevelopedTile>) {
    let mut tiles = Vec::new();
    let result = develop_with(
        &DevelopRequest {
            plane,
            recipe,
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: Grid { origin_x: 0, origin_y: 0, tile_width: 16, tile_height: 16 },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        converter,
        cancel,
        &mut |t| {
            tiles.push(t);
            Ok(())
        },
    );
    (result, tiles)
}

#[test]
fn profile_transform_admitted() {
    bounded_case("profile_transform_admitted", || {
        let actor = ActorContext::new("a", "p", "o", "op", "s", "sess").unwrap();
        let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let ledger = Admission(Arc::new(Ledger::default()));
        let srgb = binding("SCPF-019abcde-0000-7000-8000-000000000001", SRGB);
        let p3 = binding("SCPF-019abcde-0000-7000-8000-000000000002", P3);
        let encoded = binding("SCPF-019abcde-0000-7000-8000-000000000003", ENCODED);
        let plane = plane();
        let cancel = CancellationToken::default();
        let context = |revision| PrismContext { actor: &actor, resource_id: &resource, correlation_id: 5, revision };

        // Baseline: working == output, no converter => identity, uninspected.
        let (receipt, baseline) = run(&plane, &recipe(&srgb, &srgb), None, &cancel);
        let receipt = receipt.unwrap();
        assert_eq!((receipt.profile_route, receipt.profile_calls), ("identity_uninspected", 0));
        let base = floats(&baseline[0]);

        // sRGB-linear -> DisplayP3-linear: admitted inspection, wide transform; 256 pixels = 1 request.
        let prism = AdmittedPrism::new(&actor, 1, ledger.clone()).unwrap();
        let mut wide = AdmittedWideEngine::new(&actor, 8, ledger.clone()).unwrap();
        let mut sink = Collect::default();
        let mut converter = PrismConverter::new(
            &prism,
            &mut wide,
            IccProfile { binding: &srgb, icc: SRGB },
            IccProfile { binding: &p3, icc: P3 },
            context(7),
            &mut sink,
        );
        let (receipt, tiles) = run(&plane, &recipe(&srgb, &p3), Some(&mut converter), &cancel);
        let receipt = receipt.unwrap();
        assert_eq!((receipt.profile_route, receipt.profile_calls, receipt.tiles), ("prism_admitted_wide", 1, 1));
        let converted = floats(&tiles[0]);
        let (m_src, m_dst) = (colorants(SRGB), colorants(P3));
        let inverse = hsk_studio_develop::color::invert(&m_dst).unwrap();
        let to_dst = hsk_studio_develop::color::mul(&inverse, &m_src);
        let mut moved = false;
        for px in 0..256 {
            let rgb = [f64::from(base[px * 3]), f64::from(base[px * 3 + 1]), f64::from(base[px * 3 + 2])];
            for row in 0..3 {
                let want = to_dst[row * 3] * rgb[0] + to_dst[row * 3 + 1] * rgb[1] + to_dst[row * 3 + 2] * rgb[2];
                let got = f64::from(converted[px * 3 + row]);
                assert!((got - want).abs() < 1e-4, "px {px} ch {row}: {got} vs {want}");
                moved |= (got - rgb[row]).abs() > 0.01;
            }
        }
        assert!(moved, "conversion must be non-identity");
        assert_eq!(sink.0.len(), 1, "one terminal observation per Prism request");

        // Exposure is applied AFTER conversion: +1 EV doubles the converted values exactly.
        let mut bright = recipe(&srgb, &p3);
        bright.exposure_stops = 1.0;
        let mut converter = PrismConverter::new(
            &prism,
            &mut wide,
            IccProfile { binding: &srgb, icc: SRGB },
            IccProfile { binding: &p3, icc: P3 },
            context(7),
            &mut sink,
        );
        let (receipt, tiles) = run(&plane, &bright, Some(&mut converter), &cancel);
        receipt.unwrap();
        let doubled = floats(&tiles[0]);
        assert!(converted.iter().zip(&doubled).all(|(a, b)| (a * 2.0).to_bits() == b.to_bits()));

        // Refusals deliver no tile: range (> 1 working RGB), mismatched bindings, bad hash,
        // encoded-transfer destination, pre-cancelled token.
        let mut scaled = recipe(&srgb, &p3);
        scaled.camera_to_profile_rgb[0] = 2.0;
        let mut converter = PrismConverter::new(
            &prism,
            &mut wide,
            IccProfile { binding: &srgb, icc: SRGB },
            IccProfile { binding: &p3, icc: P3 },
            context(7),
            &mut sink,
        );
        let (result, tiles) = run(&plane, &scaled, Some(&mut converter), &cancel);
        assert_eq!((result.err(), tiles.len()), (Some(DevelopError::Prism("channel_range")), 0));
        let (result, _) = run(&plane, &recipe(&srgb, &srgb), Some(&mut converter), &cancel);
        assert_eq!(result.err(), Some(DevelopError::InvalidProfile), "converter bindings must equal the recipe");
        let pre_cancelled = CancellationToken::default();
        pre_cancelled.cancel();
        let (result, _) = run(&plane, &recipe(&srgb, &p3), Some(&mut converter), &pre_cancelled);
        assert_eq!(result.err(), Some(DevelopError::Canceled));
        let wrong_hash = ProfileBinding { sha256: [9; 32], ..p3.clone() };
        let mut bad_hash = PrismConverter::new(
            &prism,
            &mut wide,
            IccProfile { binding: &srgb, icc: SRGB },
            IccProfile { binding: &wrong_hash, icc: P3 },
            context(7),
            &mut sink,
        );
        let (result, _) = run(&plane, &recipe(&srgb, &wrong_hash), Some(&mut bad_hash), &cancel);
        assert_eq!(result.err(), Some(DevelopError::Prism("hash_mismatch")));
        let mut encoded_dst = PrismConverter::new(
            &prism,
            &mut wide,
            IccProfile { binding: &srgb, icc: SRGB },
            IccProfile { binding: &encoded, icc: ENCODED },
            context(7),
            &mut sink,
        );
        let (result, tiles) = run(&plane, &recipe(&srgb, &encoded), Some(&mut encoded_dst), &cancel);
        assert_eq!((result.err(), tiles.len()), (Some(DevelopError::Prism("unsupported_profile")), 0));
        // Without any converter a differing output profile is still a typed refusal.
        let (result, _) = run(&plane, &recipe(&srgb, &p3), None, &cancel);
        assert_eq!(result.err(), Some(DevelopError::UnsupportedProfileTransform));

        // Equal profiles with a converter: both inspected (admitted), transform skipped, same bytes.
        let mut same = PrismConverter::new(
            &prism,
            &mut wide,
            IccProfile { binding: &srgb, icc: SRGB },
            IccProfile { binding: &srgb, icc: SRGB },
            context(7),
            &mut sink,
        );
        let (receipt, tiles) = run(&plane, &recipe(&srgb, &srgb), Some(&mut same), &cancel);
        let receipt = receipt.unwrap();
        assert_eq!((receipt.profile_route, receipt.profile_calls), ("identity_inspected", 0));
        assert_eq!(tiles[0].colour, baseline[0].colour);
        // Every transient lease (inspection, build, request, output) is retired; what remains on the
        // ledger is exactly the two provider leases and the leases of the cached plans.
        let live = ledger.0.live.load(Ordering::SeqCst);
        assert_eq!(
            live,
            prism.provider_reserved_bytes() + wide.provider_reserved_bytes() + wide.plan_reserved_bytes()
        );
        assert!(wide.plan_reserved_bytes() > 0, "the cached plan is accounted");
    });
}

fn run_tiled(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    converter: &mut dyn ProfileConverter,
    tile: u32,
) -> (DevelopReceipt, Vec<DevelopedTile>) {
    let mut tiles = Vec::new();
    let receipt = develop_with(
        &DevelopRequest {
            plane,
            recipe,
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: Grid { origin_x: 0, origin_y: 0, tile_width: tile, tile_height: tile },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        Some(converter),
        &CancellationToken::default(),
        &mut |t| {
            tiles.push(t);
            Ok(())
        },
    )
    .unwrap();
    (receipt, tiles)
}

#[test]
fn wide_requests_are_chunk_invariant_and_report_plan_cache_and_clip() {
    bounded_case("wide_requests", || {
        let actor = ActorContext::new("a", "p", "o", "op", "s", "sess").unwrap();
        let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let ledger = Admission(Arc::new(Ledger::default()));
        let prism = AdmittedPrism::new(&actor, 1, ledger.clone()).unwrap();
        let mut wide = AdmittedWideEngine::new(&actor, 8, ledger).unwrap();
        let srgb = binding("SCPF-019abcde-0000-7000-8000-000000000001", SRGB);
        let p3 = binding("SCPF-019abcde-0000-7000-8000-000000000002", P3);
        let (plane, recipe) = (plane(), recipe(&p3, &srgb));
        let context = PrismContext { actor: &actor, resource_id: &resource, correlation_id: 5, revision: 7 };
        let (src, dst) = (IccProfile { binding: &p3, icc: P3 }, IccProfile { binding: &srgb, icc: SRGB });
        let mut sink = Collect::default();

        // One request for the 256-pixel tile: the plan is built once (both profiles parsed once).
        let mut converter = PrismConverter::new(&prism, &mut wide, src, dst, context, &mut sink);
        let (receipt, tiles) = run_tiled(&plane, &recipe, &mut converter, 16);
        let reference = tiles[0].colour.clone();
        let stats = receipt.profile_stats;
        assert_eq!((receipt.profile_route, receipt.profile_calls), ("prism_admitted_wide", 1));
        assert_eq!((stats.plan_hits, stats.plan_misses, stats.plan_parses), (0, 1, 2));

        // Never silently clipped: Display P3 -> sRGB leaves the gamut, the pixels stay unclamped
        // and the report matches an independent count over the delivered floats.
        let values = floats(&tiles[0]);
        let outside = |v: &f32| *v < -1e-3 || *v > 1.0 + 1e-3;
        assert_eq!(stats.clip.method, ClipMethod::LinearDevice);
        for channel in 0..3 {
            let counted = values.iter().skip(channel).step_by(3).filter(|v| outside(v)).count() as u64;
            assert_eq!(stats.clip.channels_clipped[channel], counted, "channel {channel}");
        }
        let pixels = values.chunks_exact(3).filter(|p| p.iter().any(outside)).count() as u64;
        assert_eq!(stats.clip.pixels_clipped, pixels);
        assert!(pixels > 0 && values.iter().any(|v| *v < 0.0) && stats.clip.max_overshoot > 0.0);

        // Same engine, 7-pixel requests: bit-identical output, 37 requests, every one a plan hit
        // that parses nothing; one terminal observation per request.
        let before = sink.0.len();
        let mut converter = PrismConverter::new(&prism, &mut wide, src, dst, context, &mut sink)
            .with_chunk_pixels(7)
            .unwrap();
        let (receipt, tiles) = run_tiled(&plane, &recipe, &mut converter, 16);
        let stats = receipt.profile_stats;
        assert_eq!(tiles[0].colour, reference);
        assert_eq!((receipt.profile_calls, stats.plan_hits, stats.plan_misses, stats.plan_parses), (37, 37, 0, 0));
        assert_eq!(sink.0.len() - before, 37);

        // Four 64-pixel tiles: four requests on the cached plan.
        let mut converter = PrismConverter::new(&prism, &mut wide, src, dst, context, &mut sink);
        let (receipt, tiles) = run_tiled(&plane, &recipe, &mut converter, 8);
        assert_eq!((tiles.len(), receipt.profile_calls, receipt.profile_stats.plan_hits, receipt.profile_stats.plan_parses), (4, 4, 4, 0));

        // A converter that returns a different pixel count is a typed refusal, never an index panic
        // or silently uncovered pixels (R3 PLAUSIBLE); no tile is delivered.
        struct Resizing<'a>(&'a ProfileBinding, &'a ProfileBinding, isize);
        impl ProfileConverter for Resizing<'_> {
            fn source(&self) -> &ProfileBinding { self.0 }
            fn destination(&self) -> &ProfileBinding { self.1 }
            fn inspect(&mut self, _: &CancellationToken) -> Result<(), DevelopError> { Ok(()) }
            fn convert(&mut self, pixels: &[[f32; 3]], _: &CancellationToken) -> Result<Vec<[f32; 3]>, DevelopError> {
                let mut out = pixels.to_vec();
                if self.2 < 0 { out.pop(); } else { out.push([0.0; 3]); }
                Ok(out)
            }
            fn calls(&self) -> u32 { 0 }
        }
        for delta in [-1isize, 1] {
            let mut tiles = Vec::new();
            let result = develop_with(
                &DevelopRequest {
                    plane: &plane,
                    recipe: &recipe,
                    recipe_revision: 1,
                    expected_source_revision: 1,
                    grid: Grid { origin_x: 0, origin_y: 0, tile_width: 16, tile_height: 16 },
                    output_origin: (0, 0),
                    execution_hint: ExecutionHint::Auto,
                },
                Some(&mut Resizing(&p3, &srgb, delta)),
                &CancellationToken::default(),
                &mut |t| {
                    tiles.push(t);
                    Ok(())
                },
            );
            assert_eq!((result.err(), tiles.len()), (Some(DevelopError::InvalidLength), 0), "delta {delta}");
        }

        // The request cap is bounded by the wide engine's own limit.
        for bad in [0, hsk_studio_prism::MAX_WIDE_PIXELS + 1] {
            let converter = PrismConverter::new(&prism, &mut wide, src, dst, context, &mut sink);
            assert_eq!(converter.with_chunk_pixels(bad).err(), Some(DevelopError::InvalidLimits));
        }
    });
}

/// STU-CON-044 accounting: a ledger that is one byte short of what the conversion needs refuses it
/// with `admission_denied` BEFORE the engine parses a profile or converts a pixel, and a ledger
/// with exactly enough succeeds with the same bytes. A ledger that only covers the two provider
/// leases refuses at inspection, before any pixel is computed.
#[test]
fn short_admission_budget_is_refused_before_any_work() {
    bounded_case("short_admission_budget", || {
        let actor = ActorContext::new("a", "p", "o", "op", "s", "sess").unwrap();
        let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let srgb = binding("SCPF-019abcde-0000-7000-8000-000000000001", SRGB);
        let p3 = binding("SCPF-019abcde-0000-7000-8000-000000000002", P3);
        let (plane, recipe) = (plane(), recipe(&srgb, &p3));
        let cancel = CancellationToken::default();
        let context = PrismContext { actor: &actor, resource_id: &resource, correlation_id: 5, revision: 7 };
        let (src, dst) = (IccProfile { binding: &srgb, icc: SRGB }, IccProfile { binding: &p3, icc: P3 });
        let engines = |ledger: &Admission| {
            (
                AdmittedPrism::new(&actor, 1, ledger.clone()).unwrap(),
                AdmittedWideEngine::new(&actor, 8, ledger.clone()).unwrap(),
            )
        };

        // Unlimited ledger: measure what the conversion needs and keep the output.
        let ledger = Admission(Arc::new(Ledger::default()));
        let (prism, mut wide) = engines(&ledger);
        let base = ledger.0.live.load(Ordering::SeqCst);
        let mut sink = Collect::default();
        let mut converter = PrismConverter::new(&prism, &mut wide, src, dst, context, &mut sink);
        let (receipt, reference) = run(&plane, &recipe, Some(&mut converter), &cancel);
        receipt.unwrap();
        let peak = ledger.0.peak.load(Ordering::SeqCst);
        assert!(peak > base, "the conversion reserves bytes beyond the provider leases");

        // Exactly enough: succeeds, same bytes, transient leases retired.
        let ledger = Admission(Arc::new(Ledger::default()));
        let (prism, mut wide) = engines(&ledger);
        ledger.0.cap.store(peak, Ordering::SeqCst);
        let mut sink = Collect::default();
        let mut converter = PrismConverter::new(&prism, &mut wide, src, dst, context, &mut sink);
        let (receipt, tiles) = run(&plane, &recipe, Some(&mut converter), &cancel);
        receipt.unwrap();
        assert_eq!(tiles[0].colour, reference[0].colour);

        // One byte short of the peak: denied while taking the last lease of the first request,
        // before the plan is built (no parse, no plan, no observation, no tile).
        let ledger = Admission(Arc::new(Ledger::default()));
        let (prism, mut wide) = engines(&ledger);
        let provider = ledger.0.live.load(Ordering::SeqCst);
        ledger.0.cap.store(peak - 1, Ordering::SeqCst);
        let mut sink = Collect::default();
        let mut converter = PrismConverter::new(&prism, &mut wide, src, dst, context, &mut sink);
        let (result, tiles) = run(&plane, &recipe, Some(&mut converter), &cancel);
        assert_eq!((result.err(), tiles.len()), (Some(DevelopError::Prism("admission_denied")), 0));
        assert_eq!(converter.calls(), 0);
        assert!(sink.0.is_empty(), "no Prism request was delivered");
        let stats = wide.cache_stats();
        assert_eq!((stats.entries, stats.hits, stats.misses, stats.parses), (0, 0, 0, 0), "nothing parsed or planned");
        assert_eq!(wide.plan_reserved_bytes(), 0);
        assert_eq!(ledger.0.live.load(Ordering::SeqCst), provider, "every transient lease was released");

        // Only the provider leases fit: refused at inspection, before any pixel is computed.
        let ledger = Admission(Arc::new(Ledger::default()));
        let (prism, mut wide) = engines(&ledger);
        let provider = ledger.0.live.load(Ordering::SeqCst);
        ledger.0.cap.store(provider, Ordering::SeqCst);
        let mut sink = Collect::default();
        let mut converter = PrismConverter::new(&prism, &mut wide, src, dst, context, &mut sink);
        let (result, tiles) = run(&plane, &recipe, Some(&mut converter), &cancel);
        assert_eq!((result.err(), tiles.len()), (Some(DevelopError::Prism("admission_denied")), 0));
        assert_eq!(converter.calls(), 0);
        assert_eq!(ledger.0.live.load(Ordering::SeqCst), provider);
        // The refusal is classified as unavailable, not as a validation failure.
        assert_eq!(
            DevelopError::Prism("admission_denied").outcome(),
            hsk_studio_observe::Outcome::Failure(hsk_studio_observe::FailureCode::Unavailable)
        );
    });
}
