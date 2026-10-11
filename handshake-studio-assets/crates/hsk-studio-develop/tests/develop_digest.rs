//! Byte-identity proof for the scalar hot-loop rewrite: the digests below were taken from the
//! pre-rewrite oracle (commit 12e3043) and must never move. Synthetic planes only.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_pigment::Grid;
use sha2::{Digest, Sha256};

const BLACK: [u16; 3] = [64, 80, 96];
const WHITE: [u16; 3] = [4095, 4000, 3900];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Deterministic pseudo-random samples inside every channel's [black, white] range, with the
/// extremes (max black, min white) mixed in.
fn plane(pattern: CfaPattern, width: u32, height: u32, origin: (i64, i64), phase: (u8, u8)) -> CfaPlane {
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ (u64::from(width) << 32) ^ u64::from(height);
    let samples: Vec<u16> = (0..width as usize * height as usize)
        .map(|i| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            match i % 41 {
                0 => 96,
                1 => 3900,
                _ => 100 + ((state >> 33) % 2901) as u16,
            }
        })
        .collect();
    CfaPlane::with_limits(
        PlaneInit {
            width,
            height,
            stride_bytes: width * 2,
            sensor_origin: origin,
            cfa: Cfa { pattern, phase_x: phase.0, phase_y: phase.1 },
            black: BLACK,
            white: WHITE,
            source_revision: 1,
            bytes: samples.iter().flat_map(|s| s.to_le_bytes()).collect(),
        },
        Limits::SPEC_V1,
    )
    .unwrap()
}

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [7; 32],
    }
}

/// Positive rows summing to 1, exposure below 0 EV and monotone curves keep every stage inside 0..1.
fn curve_recipe() -> DevelopRecipe {
    let mut r = DevelopRecipe::native([1.0; 3], [0.5, 0.3, 0.2, 0.2, 0.6, 0.2, 0.1, 0.3, 0.6], profile());
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

/// Strong white balance and a mixing matrix: values leave 0..1 (no curves, so nothing refuses).
fn wide_recipe() -> DevelopRecipe {
    DevelopRecipe::native(
        [0.5, 1.0, 0.6],
        [1.6, -0.4, -0.2, -0.3, 1.5, -0.2, -0.1, -0.4, 1.5],
        profile(),
    )
}

fn f32_digest(values: impl Iterator<Item = f32>) -> String {
    let mut hasher = Sha256::new();
    for v in values {
        hasher.update(v.to_le_bytes());
    }
    hex(&hasher.finalize())
}

fn tile_digest(plane: &CfaPlane, recipe: &DevelopRecipe, tile: u32) -> String {
    let mut hasher = Sha256::new();
    develop(
        &DevelopRequest {
            plane,
            recipe,
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: Grid { origin_x: 3, origin_y: -2, tile_width: tile, tile_height: tile },
            output_origin: (5, 1),
            execution_hint: ExecutionHint::Scalar,
        },
        &CancellationToken::default(),
        &mut |t| {
            hasher.update(format!("{:?}{:?}", t.bounds, t.developed).as_bytes());
            hasher.update(&t.colour);
            hasher.update(&t.alpha);
            hasher.update(&t.coverage);
            Ok(())
        },
    )
    .unwrap();
    hex(&hasher.finalize())
}

#[test]
fn scalar_output_is_byte_identical_to_the_pre_rewrite_oracle() {
    let cancel = CancellationToken::default();
    let bayer = |b| CfaPattern::Bayer(b);
    // (name, plane): interior and border sites, odd sizes, non-zero origin and phase, X-Trans,
    // and planes small enough that every site is a border site.
    let cases: Vec<(&str, CfaPlane)> = vec![
        ("gbrg_37x29_o3_-5_p1_0", plane(bayer(Bayer::Gbrg), 37, 29, (3, -5), (1, 0))),
        ("xtrans_41x33_o-7_2_p4_3", plane(CfaPattern::XTrans, 41, 33, (-7, 2), (4, 3))),
        ("rggb_64x48", plane(bayer(Bayer::Rggb), 64, 48, (0, 0), (0, 0))),
        ("bggr_5x5", plane(bayer(Bayer::Bggr), 5, 5, (1, 1), (0, 1))),
        ("grbg_3x2", plane(bayer(Bayer::Grbg), 3, 2, (0, 0), (0, 0))),
        ("xtrans_6x6", plane(CfaPattern::XTrans, 6, 6, (0, 0), (0, 0))),
    ];
    let mut lines = Vec::new();
    for (name, plane) in &cases {
        let demosaic = demosaic_normalized(plane, &cancel).unwrap();
        lines.push(format!("{name} demosaic {}", f32_digest(demosaic.iter().flatten().copied())));
    }
    for (name, plane) in cases.iter().filter(|(n, _)| ["gbrg_37x29_o3_-5_p1_0", "xtrans_41x33_o-7_2_p4_3", "rggb_64x48"].contains(n)) {
        let (image, receipt) = develop_to_image(plane, &curve_recipe(), 1, &cancel).unwrap();
        lines.push(format!("{name} curve_image {} {}x{}", f32_digest(image.rgb.iter().copied()), receipt.output.width, receipt.output.height));
        lines.push(format!("{name} curve_tiles16 {}", tile_digest(plane, &curve_recipe(), 16)));
        let (image, _) = develop_to_image(plane, &wide_recipe(), 1, &cancel).unwrap();
        lines.push(format!("{name} wide_image {}", f32_digest(image.rgb.iter().copied())));
    }
    let actual = lines.join("\n");
    let expected = EXPECTED.join("\n");
    assert_eq!(actual, expected, "digest drift:\n{actual}");
}

const EXPECTED: [&str; 15] = [
    "gbrg_37x29_o3_-5_p1_0 demosaic ef9b5caa2ac515dc08a18d3da331beaf7fb998f8c6d8840db95b24982b3405c5",
    "xtrans_41x33_o-7_2_p4_3 demosaic 76e48a53501752c35658667a2a3b4af76b7d6f28dfa336e33bcff267be80b7e9",
    "rggb_64x48 demosaic 8ed5484156d06e0cccd4e9f32d5fdba860020ecaac0fdf83e03b922498a39fb6",
    "bggr_5x5 demosaic e6cf01c9ca0f907f89e13bf76ccc5104a1d0a80a4f8ca8a972611d9ccc90b0b2",
    "grbg_3x2 demosaic 04aab6eb7e8ebdf4d016a272377dc214973dccd771412a945034565250d906df",
    "xtrans_6x6 demosaic 0b5106c05db7ee6dae638dad0a1a233ec818530a221e3c84128281ffcf7ecb91",
    "gbrg_37x29_o3_-5_p1_0 curve_image a97b791b204779c5866a6b3b68b5827788425fbd52ffe4e52d70af8c24af92d4 31x20",
    "gbrg_37x29_o3_-5_p1_0 curve_tiles16 6635bcd3dba93b1b142281691950fa733f938f0c7e902edf9e9386db87fb1396",
    "gbrg_37x29_o3_-5_p1_0 wide_image 849fdc3e76e75aa8e039d32d3f370ce700aa9f3a6e149930130421b89fe8abb8",
    "xtrans_41x33_o-7_2_p4_3 curve_image a06b294e29693853f98b8592cb149496ea3d2a192c2d86a2ef40e17cf0f5017d 33x23",
    "xtrans_41x33_o-7_2_p4_3 curve_tiles16 ba8f488897f54a39ed4af29f3058176e2673afbe55dedb13b3b1ff1a453f82d0",
    "xtrans_41x33_o-7_2_p4_3 wide_image 9d1bb2fbdbd9d76d55441f96553aa72731ca312a1aff150cae86f5c33e90ab2d",
    "rggb_64x48 curve_image 66cc58a632f4a98bd6abbd4b770dc434c54c2999401eaa6a7da0dd83fae6dbab 52x32",
    "rggb_64x48 curve_tiles16 e7b33967ddcc00669ea3d3d717918fb72bac7ae2f132e43b70851e219177604c",
    "rggb_64x48 wide_image c4ba41594dcd0c72f295aa0284985704323a9222f72de5a9d495476ffca00849",
];
