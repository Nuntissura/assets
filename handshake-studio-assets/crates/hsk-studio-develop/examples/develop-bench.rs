//! Timing probe for the scalar oracle on a synthetic 24 MP RGGB plane (no assertions, no I/O).
//! Run with `--release`; prints one line per stage in milliseconds. `--width W --height H` override.
//! The last stage converts Display P3 -> sRGB (CC0 linear test profiles) through the wide Prism route.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_pigment::Grid;
use hsk_studio_prism::{AdmittedPrism, ProviderAdmission, ProviderReservation, WideEngine, profile_hash};
use std::{env, time::Instant};

const SRGB: &[u8] = include_bytes!("../tests/profiles/sRGB-admitted-linear-v4.icc");
const P3: &[u8] = include_bytes!("../tests/profiles/DisplayP3-admitted-linear-v4.icc");

/// Unbudgeted admission for the timing probe: reservations are recorded nowhere.
#[derive(Clone)]
struct Unmetered;
struct NoLease(u64);
impl ProviderReservation for NoLease {
    fn reserved_bytes(&self) -> u64 {
        self.0
    }
}
impl ProviderAdmission for Unmetered {
    type Reservation = NoLease;
    fn synchronous_retirement(&self) -> bool {
        true
    }
    fn reserve(&self, bytes: u64) -> Result<NoLease, hsk_studio_prism::Error> {
        Ok(NoLease(bytes))
    }
}
struct CountingSink(u64);
impl SinkPort for CountingSink {
    fn try_send(&mut self, _class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0 += 1;
        Ok(())
    }
}

fn arg(args: &[String], flag: &str, default: u32) -> u32 {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let (width, height) = (arg(&args, "--width", 6000), arg(&args, "--height", 4000));
    let (black, white) = (64u16, 4095u16);
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut bytes = Vec::with_capacity(width as usize * height as usize * 2);
    for _ in 0..width as usize * height as usize {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let sample = black + ((state >> 33) % u64::from(white - black + 1)) as u16;
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    let t = Instant::now();
    let plane = CfaPlane::with_limits(
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
            black: [black; 3],
            white: [white; 3],
            source_revision: 1,
            bytes,
        },
        Limits::LARGE,
    )
    .expect("plane");
    println!("plane_admit_ms={} (range check + sha256 over {} bytes)", t.elapsed().as_millis(), plane.bytes().len());

    let profile = ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [0; 32],
    };
    let cancel = CancellationToken::default();
    let mut recipe = DevelopRecipe::native(
        [0.5, 1.0, 0.6],
        [1.6, -0.4, -0.2, -0.3, 1.5, -0.2, -0.1, -0.4, 1.5],
        profile,
    );
    let t = Instant::now();
    let demosaic = demosaic_normalized(&plane, &cancel).expect("demosaic");
    let ms = t.elapsed().as_millis();
    println!("demosaic_only_ms={ms} mpix_per_s={:.1}", (demosaic.len() as f64 / 1.0e6) / (ms.max(1) as f64 / 1000.0));
    drop(demosaic);

    let t = Instant::now();
    let (image, receipt) = develop_to_image(&plane, &recipe, 1, &cancel).expect("develop");
    let ms = t.elapsed().as_millis();
    println!(
        "develop_identity_ms={ms} tiles={} pixels={} mpix_per_s={:.1} route={} limits={}",
        receipt.tiles,
        receipt.pixels,
        (receipt.pixels as f64 / 1.0e6) / (ms.max(1) as f64 / 1000.0),
        receipt.route,
        receipt.limits
    );
    let mean: f64 = image.rgb.iter().map(|v| f64::from(*v)).sum::<f64>() / image.rgb.len() as f64;
    println!("mean_linear={mean:.4}");

    // Curve run: identity colour so values stay inside the 0..1 curve domain (curves refuse outside it).
    recipe.as_shot_neutral = [1.0; 3];
    recipe.camera_to_profile_rgb = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    recipe.enables.enable_tone_curve = true;
    recipe.curves.composite = Curve::new(vec![(0.0, 0.0), (64.0, 48.0), (192.0, 208.0), (255.0, 255.0)]).unwrap();
    recipe.exposure_stops = -1.0;
    let t = Instant::now();
    match develop_to_image(&plane, &recipe, 2, &cancel) {
        Ok((_, receipt)) => println!("develop_curve_ms={} pixels={}", t.elapsed().as_millis(), receipt.pixels),
        Err(e) => println!("develop_curve_refused={e} after_ms={} (curve inputs outside 0..1 refuse, never clip)", t.elapsed().as_millis()),
    }

    // Profile run: identity colour keeps working RGB inside 0..1 (Prism refuses outside); no curves.
    let (srgb, p3) = (
        ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(), sha256: profile_hash(SRGB) },
        ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000002").unwrap(), sha256: profile_hash(P3) },
    );
    let mut recipe = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], p3.clone());
    recipe.output_profile = srgb.clone();
    let actor = ActorContext::new("a", "p", "o", "op", "s", "sess").unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    let prism = AdmittedPrism::new(&actor, 1, Unmetered).unwrap();
    let mut wide = WideEngine::new(actor.clone(), 8).unwrap();
    let mut sink = CountingSink(0);
    let mut converter = PrismConverter::new(
        &prism,
        &mut wide,
        IccProfile { binding: &p3, icc: P3 },
        IccProfile { binding: &srgb, icc: SRGB },
        PrismContext { actor: &actor, resource_id: &resource, correlation_id: 1, revision: 1 },
        &mut sink,
    );
    let t = Instant::now();
    let mut delivered = 0u32;
    let result = develop_with(
        &DevelopRequest {
            plane: &plane,
            recipe: &recipe,
            recipe_revision: 3,
            expected_source_revision: 1,
            grid: Grid { origin_x: 0, origin_y: 0, tile_width: 256, tile_height: 256 },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        Some(&mut converter),
        &cancel,
        &mut |_| {
            delivered += 1;
            Ok(())
        },
    );
    match result {
        Ok(receipt) => {
            let ms = t.elapsed().as_millis();
            println!(
                "develop_profile_p3_to_srgb_ms={ms} mpix_per_s={:.1} route={} calls={} plan_hits={} plan_misses={} plan_parses={} clip_pixels={} tiles={delivered}",
                (receipt.pixels as f64 / 1.0e6) / (ms.max(1) as f64 / 1000.0),
                receipt.profile_route,
                receipt.profile_calls,
                receipt.profile_stats.plan_hits,
                receipt.profile_stats.plan_misses,
                receipt.profile_stats.plan_parses,
                receipt.profile_stats.clip.pixels_clipped,
            );
        }
        Err(e) => println!("develop_profile_refused={e} after_ms={}", t.elapsed().as_millis()),
    }
}
