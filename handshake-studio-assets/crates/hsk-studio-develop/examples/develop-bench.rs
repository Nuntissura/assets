//! Timing probe for the scalar oracle on a synthetic 24 MP RGGB plane (no assertions, no I/O).
//! Run with `--release`; prints one line per stage in milliseconds. `--width W --height H` override.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use std::{env, time::Instant};

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
}
