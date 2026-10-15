//! Lane SMB-DEVELOP-LENS-PRESETS slice 4: develop presets (STU-RAW-128) and creative looks from
//! `.cube` tables through Prism's tetrahedral evaluator (STU-RAW-126). Lightroom preset / profile
//! parity is UNVERIFIED (D5); these tests prove the documented semantics against hand values.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_prism::{LutError, parse_cube};

fn profile() -> ProfileBinding {
    ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(), sha256: [7; 32] }
}

fn recipe() -> DevelopRecipe {
    DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile())
}

/// A cube of `n` points per axis whose rows are `f(r, g, b)` (red fastest).
fn cube(n: usize, header: &str, f: impl Fn(f64, f64, f64) -> [f64; 3]) -> String {
    let mut s = format!("{header}LUT_3D_SIZE {n}\n");
    let last = (n - 1) as f64;
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let v = f(r as f64 / last, g as f64 / last, b as f64 / last);
                s.push_str(&format!("{} {} {}\n", v[0], v[1], v[2]));
            }
        }
    }
    s
}

fn close(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < tol)
}

#[test]
fn cube_identity_affine_and_tetrahedral_hand_values() {
    let probes = [[0.1, 0.5, 0.9], [0.0, 1.0, 0.3], [0.77, 0.21, 0.42], [1.0, 1.0, 1.0]];
    let id = parse_cube(cube(17, "TITLE \"identity\"\n", |r, g, b| [r, g, b]).as_bytes()).unwrap();
    assert_eq!((id.title.as_deref(), id.size_3d, id.lut_3d.len()), (Some("identity"), 17, 17 * 17 * 17));
    for p in probes {
        assert!(close(id.apply(p), p, 1e-6), "{p:?}");
    }
    // Tetrahedral interpolation reproduces any affine map exactly (up to the f32 rows).
    let affine = |r: f64, g: f64, b: f64| [0.5 * r + 0.2 * g + 0.1, 0.3 * g - 0.4 * b + 0.5, 0.1 * r + 0.1 * g + 0.6 * b];
    let lut = parse_cube(cube(5, "", affine).as_bytes()).unwrap();
    for p in probes {
        assert!(close(lut.apply(p), affine(p[0], p[1], p[2]), 1e-6));
    }
    // 2^3 cube with only the (1,1,1) corner at 1: at (r, g, b) = (0.5, 0.25, 0.75) the blue >= red >=
    // green tetrahedron gives the smallest fraction, 0.25 (trilinear would give 0.09375).
    let corner = parse_cube(cube(2, "", |r, g, b| [r * g * b; 3]).as_bytes()).unwrap();
    assert!((corner.apply([0.5, 0.25, 0.75])[0] - 0.25).abs() < 1e-12);
    // Domain and a 1D shaper (Resolve order: 1D rows first): domain 0..2, shaper squares.
    let text = "LUT_1D_SIZE 3\nLUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n0 0 0\n0.25 0.25 0.25\n1 1 1\n"
        .to_owned()
        + &cube(2, "", |r, g, b| [r, g, b]).replace("LUT_3D_SIZE 2\n", "");
    let shaped = parse_cube(text.as_bytes()).unwrap();
    assert!(close(shaped.apply([1.0, 2.0, 0.5]), [0.25, 1.0, 0.125], 1e-12), "{:?}", shaped.apply([1.0, 2.0, 0.5]));
    assert!(close(shaped.apply([5.0, -1.0, f64::NAN]), [1.0, 0.0, 0.0], 1e-12), "clamped to the domain");
}

#[test]
fn creative_look_runs_per_pixel_with_amount() {
    let invert = cube(9, "", |r, g, b| [1.0 - r, 1.0 - g, 1.0 - b]);
    let look = CreativeLook::from_cube("negative", invert.as_bytes(), LookDomain::SrgbEncoded).unwrap();
    let mut a = Adjustments::default();
    a.look = Some(look.clone());
    let enc = |c: f64| if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    let v = 0.18;
    let out = apply_pixel(&a, [v; 3]).unwrap();
    assert!((enc(out[0]) - (1.0 - enc(v))).abs() < 1e-6);
    // Amount 0.5 of an inversion is encoded mid grey for any input.
    a.look.as_mut().unwrap().amount = 0.5;
    for x in [0.01, 0.3, 0.9] {
        assert!((enc(apply_pixel(&a, [x; 3]).unwrap()[1]) - 0.5).abs() < 1e-6);
    }
    // Per pixel, so the tile API runs it and agrees with the whole-frame image.
    let plane = CfaPlane::with_limits(
        PlaneInit {
            width: 16,
            height: 16,
            stride_bytes: 32,
            sensor_origin: (0, 0),
            cfa: Cfa { pattern: CfaPattern::Bayer(Bayer::Rggb), phase_x: 0, phase_y: 0 },
            black: [0; 3],
            white: [4095; 3],
            source_revision: 1,
            bytes: (0..256u32).flat_map(|i| (300 + i as u16 * 10).to_le_bytes()).collect(),
        },
        Limits::LARGE,
    )
    .unwrap();
    let mut r = recipe();
    r.adjust.look = Some(look);
    let cancel = CancellationToken::default();
    let img = develop_to_image(&plane, &r, 1, &cancel).unwrap().0;
    let mut tiles = Vec::new();
    develop(
        &DevelopRequest {
            plane: &plane,
            recipe: &r,
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: hsk_studio_pigment::Grid { origin_x: 0, origin_y: 0, tile_width: 16, tile_height: 16 },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        &cancel,
        &mut |t| {
            tiles.push(t);
            Ok(())
        },
    )
    .unwrap();
    let tile_px: Vec<f32> = tiles[0].colour.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
    assert_eq!(tile_px, img.rgb);
    assert_ne!(img, develop_to_image(&plane, &recipe(), 1, &cancel).unwrap().0);
}

#[test]
fn presets_record_their_keys_apply_with_amount_merge_and_round_trip() {
    let mut source = recipe();
    source.adjust.tone.contrast = 40.0;
    source.enables.enable_tone_curve = true;
    source.adjust.geometry.upright_mode = UprightMode::Vertical;
    source.adjust.effects.grain.seed = 2_831_896_825;
    source.adjust.calibration.blue_hue = -12.0;
    let keys = ["contrast", "enable.tone_curve", "geometry.upright", "effects.grain.seed", "calibration.blue_hue"];
    let preset = DevelopPreset::capture(&source, &keys, "Look A").unwrap();
    assert_eq!(preset.settings.len(), 5, "a preset records exactly the keys it sets");
    let mut base = recipe();
    base.adjust.tone.contrast = -20.0;
    base.adjust.tone.shadows = 15.0;
    let full = preset.apply(&base, 1.0).unwrap();
    assert_eq!((full.adjust.tone.contrast, full.adjust.tone.shadows), (40.0, 15.0), "untouched keys keep the edit");
    assert!(full.enables.enable_tone_curve && full.adjust.geometry.upright_mode == UprightMode::Vertical);
    assert_eq!(full.adjust.effects.grain.seed, 2_831_896_825);
    let half = preset.apply(&base, 0.5).unwrap();
    assert_eq!((half.adjust.tone.contrast, half.adjust.calibration.blue_hue), (10.0, -6.0));
    assert_eq!(preset.apply(&base, 0.0).unwrap(), base, "amount 0 changes nothing");
    // Merge: the later preset wins per key.
    let mut over = DevelopPreset::capture(&recipe(), &["contrast"], "Neutral").unwrap();
    over.flags.insert("supports_amount".into(), true);
    let merged = preset.merge(&over);
    assert_eq!(merged.apply(&base, 1.0).unwrap().adjust.tone.contrast, 0.0);
    assert_eq!(merged.metadata["name"], "Neutral");
    // Text round trip is lossless, and every listed key is settable on a recipe with a look.
    let text = merged.to_text();
    assert_eq!(DevelopPreset::parse(text.as_bytes()).unwrap(), merged);
    let mut with_look = recipe();
    with_look.adjust.look = Some(CreativeLook::from_cube("id", cube(2, "", |r, g, b| [r, g, b]).as_bytes(), LookDomain::Linear).unwrap());
    let keys = preset_keys();
    let all: Vec<&str> = keys.iter().map(String::as_str).collect();
    let snapshot_like = DevelopPreset::capture(&with_look, &all, "everything").unwrap();
    assert_eq!(snapshot_like.settings.len(), keys.len());
    assert_eq!(snapshot_like.apply(&with_look, 1.0).unwrap(), with_look);
    let snap = Snapshot { name: "before".into(), recipe: full.clone() };
    assert_eq!(snap.recipe, full);
}

#[test]
fn hostile_presets_are_typed_and_atomic() {
    let bad = |t: &str| DevelopPreset::parse(t.as_bytes()).err();
    let invalid = Some(DevelopError::InvalidPreset);
    assert_eq!(bad("hsk-develop-preset 2\n"), invalid);
    assert_eq!(bad("hsk-develop-preset 1\nset mystery 1\n"), invalid);
    assert_eq!(bad("hsk-develop-preset 1\nset contrast true\n"), invalid, "kind mismatch");
    assert_eq!(bad("hsk-develop-preset 1\nset contrast NaN\n"), invalid);
    assert_eq!(bad("hsk-develop-preset 1\nset contrast 1\nset contrast 2\n"), invalid);
    assert_eq!(bad("hsk-develop-preset 1\nset geometry.upright sideways\n"), invalid);
    assert_eq!(bad("hsk-develop-preset 1\nset effects.grain.seed 1.5\n"), invalid);
    assert_eq!(bad("hsk-develop-preset 1\nmeta nickname x\n"), invalid);
    assert_eq!(bad("hsk-develop-preset 1\nflag supports_amount maybe\n"), invalid);
    assert_eq!(DevelopPreset::parse(&vec![b' '; MAX_PRESET_BYTES + 1]).err(), invalid);
    assert_eq!(DevelopPreset::parse(&[0xc3, 0x28]).err(), invalid, "not UTF-8");
    // Amount bounds and a result develop refuses: typed, and the base is untouched.
    let mut source = recipe();
    source.adjust.tone.contrast = 100.0;
    let p = DevelopPreset::capture(&source, &["contrast"], "x").unwrap();
    let mut base = recipe();
    base.adjust.tone.contrast = -100.0;
    let before = base.clone();
    assert_eq!(p.apply(&base, 3.0).err(), Some(DevelopError::InvalidAdjustment));
    assert_eq!(p.apply(&base, f64::NAN).err(), Some(DevelopError::NonFinite));
    assert_eq!(p.apply(&base, 2.0).err(), Some(DevelopError::InvalidAdjustment), "-100 + 2 * 200 = 300");
    assert_eq!(base, before);
    assert_eq!(DevelopPreset::capture(&base, &["contrast", "contrast"], "x").err(), invalid);
    assert_eq!(DevelopPreset::capture(&base, &["look.amount"], "x").err(), invalid, "no look to read");
}

#[test]
fn hostile_cubes_are_typed_refusals() {
    let err = |t: &str| parse_cube(t.as_bytes()).err();
    assert_eq!(err("LUT_3D_SIZE 1000\n0 0 0\n"), Some(LutError::BadSize), "refused before any allocation");
    assert_eq!(err("LUT_3D_SIZE 129\n0 0 0\n"), Some(LutError::WrongRowCount));
    assert_eq!(err("LUT_3D_SIZE 2\n0 0 0\n"), Some(LutError::WrongRowCount));
    assert_eq!(err(&cube(2, "", |r, g, b| [r, g, b]).replacen("0 0 0", "0 nan 0", 1)), Some(LutError::BadNumber));
    assert_eq!(err(&(cube(2, "", |r, g, b| [r, g, b]) + "TITLE \"late\"\n")), Some(LutError::KeywordAfterData));
    assert_eq!(err(&cube(2, "lut_3d_size 2\n", |r, g, b| [r, g, b])), Some(LutError::UnknownKeyword), "keywords are case-sensitive");
    assert_eq!(err(&cube(2, "DOMAIN_MIN 1 0 0\nDOMAIN_MAX 1 1 1\n", |r, g, b| [r, g, b])), Some(LutError::BadDomain));
    assert_eq!(err(&cube(2, "TITLE \"a\" LUT_3D_SIZE 3\n", |r, g, b| [r, g, b])), Some(LutError::BadTitle), "nothing hides after the title");
    assert_eq!(err("TITLE \"x\"\n"), Some(LutError::MissingSize));
    assert_eq!(parse_cube(&vec![b'#'; hsk_studio_prism::MAX_CUBE_BYTES + 1]).err(), Some(LutError::TooLarge));
    // Decoys that must NOT change the parse: a '#' inside the quoted title, a commented keyword,
    // CR LF line ends and tab separators.
    let decoy = "TITLE \"grade #1\"\r\n# LUT_3D_SIZE 3\r\n".to_owned() + &cube(2, "", |r, g, b| [r, g, b]).replace('\n', "\r\n").replace(' ', "\t");
    let ok = parse_cube(decoy.as_bytes()).unwrap();
    assert_eq!((ok.title.as_deref(), ok.size_3d), (Some("grade #1"), 2));
    assert_eq!(CreativeLook::from_cube("x", b"LUT_3D_SIZE 2\n", LookDomain::Linear).err(), Some(DevelopError::InvalidLut));
    let mut r = recipe();
    let mut look = CreativeLook::from_cube("x", cube(2, "", |r, g, b| [r, g, b]).as_bytes(), LookDomain::Linear).unwrap();
    look.amount = 3.0;
    r.adjust.look = Some(look);
    assert_eq!(r.validate().err(), Some(DevelopError::InvalidAdjustment));
}
