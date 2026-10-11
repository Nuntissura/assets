//! Working -> output profile conversion through Prism's admitted route (linear-light fixtures).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_pigment::Grid;
use hsk_studio_prism::{AdmittedPrism, ProviderAdmission, ProviderReservation, profile_hash};
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
#[derive(Default)]
struct Ledger {
    live: AtomicU64,
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
        self.0.live.fetch_add(bytes, Ordering::SeqCst);
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

        // sRGB-linear -> DisplayP3-linear through the admitted route: 256 pixels = 4 chunks of 64.
        let mut prism = AdmittedPrism::new(&actor, 1, ledger.clone()).unwrap();
        let reserved = prism.provider_reserved_bytes();
        let mut sink = Collect::default();
        let mut converter = PrismConverter::new(
            &mut prism,
            IccProfile { binding: &srgb, icc: SRGB },
            IccProfile { binding: &p3, icc: P3 },
            context(7),
            &mut sink,
        );
        let (receipt, tiles) = run(&plane, &recipe(&srgb, &p3), Some(&mut converter), &cancel);
        let receipt = receipt.unwrap();
        assert_eq!((receipt.profile_route, receipt.profile_calls, receipt.tiles), ("prism_admitted", 4, 1));
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
        assert_eq!(sink.0.len(), 4, "one terminal observation per Prism call");

        // Exposure is applied AFTER conversion: +1 EV doubles the converted values exactly.
        let mut bright = recipe(&srgb, &p3);
        bright.exposure_stops = 1.0;
        let mut converter = PrismConverter::new(
            &mut prism,
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
            &mut prism,
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
            &mut prism,
            IccProfile { binding: &srgb, icc: SRGB },
            IccProfile { binding: &wrong_hash, icc: P3 },
            context(7),
            &mut sink,
        );
        let (result, _) = run(&plane, &recipe(&srgb, &wrong_hash), Some(&mut bad_hash), &cancel);
        assert_eq!(result.err(), Some(DevelopError::Prism("hash_mismatch")));
        let mut encoded_dst = PrismConverter::new(
            &mut prism,
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
            &mut prism,
            IccProfile { binding: &srgb, icc: SRGB },
            IccProfile { binding: &srgb, icc: SRGB },
            context(7),
            &mut sink,
        );
        let (receipt, tiles) = run(&plane, &recipe(&srgb, &srgb), Some(&mut same), &cancel);
        let receipt = receipt.unwrap();
        assert_eq!((receipt.profile_route, receipt.profile_calls), ("identity_inspected", 0));
        assert_eq!(tiles[0].colour, baseline[0].colour);
        // Every lease taken for inspection / transform is retired; only the provider lease remains
        // plus the one cached executor.
        let live = ledger.0.live.load(Ordering::SeqCst);
        assert_eq!(live, reserved + hsk_studio_prism::analytical_executor_bytes().unwrap());
    });
}
