//! Single-illuminant colour opt-in (lane SMB-DEVELOP-CAMERA-COLOUR-2, lead decision 2026-10-15:
//! STU-COL-218's two illuminants are the default; a vendor file matrix is one calibration and is
//! refused unless the caller opts in with `DngRaw::allow_approximate_colour`, which the host records
//! as the loss note `approximate_colour_single_illuminant`). A synthetic Olympus two-field ORF
//! (8 x 5, RGGB) carries the old-style maker note: 0x1011 ColorMatrix (rows summing to 256),
//! 0x1017 / 0x1018 red and blue balance.
use hsk_studio_accord::DomainId;
use hsk_studio_develop::*;

const W: usize = 8;
const H: usize = 5;

fn px(x: usize, y: usize) -> u16 {
    (y * 640 + x * 37 + 5) as u16
}

fn packed_row(y: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for x in (0..W).step_by(2) {
        let (a, b) = (px(x, y), px(x + 1, y));
        out.extend([(a >> 4) as u8, (((a & 15) << 4) | (b >> 8)) as u8, b as u8]);
    }
    out
}

fn entry(tag: u16, ty: u16, count: u32, value: u32) -> Vec<u8> {
    [tag.to_le_bytes().to_vec(), ty.to_le_bytes().to_vec(), count.to_le_bytes().to_vec(), value.to_le_bytes().to_vec()].concat()
}

fn ifd(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut out = (entries.len() as u16).to_le_bytes().to_vec();
    for e in entries {
        out.extend(e);
    }
    out.extend(0u32.to_le_bytes());
    out
}

/// The two-field layout of tests/develop_orf.rs (strips A1, A2, seam gap, B, padding row), plus
/// Make, the Exif CFAPattern and the old-style maker note; `matrix` writes 0x1011 (as SHORTs).
fn orf(matrix: Option<[i16; 9]>) -> Vec<u8> {
    let rb = W * 3 / 2;
    let base = 1024usize;
    let a1 = base;
    let a2 = a1 + 2 * rb;
    let b = a2 + rb + 7;
    let mut data = vec![0xffu8; b + 3 * rb - base];
    for (at, y) in [(a1, 0), (a1 + rb, 2), (a2, 4), (b, 1), (b + rb, 3)] {
        data[at - base..at - base + rb].copy_from_slice(&packed_row(y));
    }
    let (offsets_at, counts_at, exif_at, note_at, cm_at) = (300u32, 320u32, 400u32, 600u32, 800u32);
    let mut file = b"IIRS\x08\0\0\0".to_vec();
    file.extend(ifd(&[
        entry(256, 4, 1, W as u32),
        entry(257, 4, 1, H as u32),
        entry(258, 3, 1, 12),
        entry(259, 3, 1, 1),
        entry(262, 3, 1, 32803),
        entry(271, 2, 4, u32::from_le_bytes(*b"OLY\0")),
        entry(272, 2, 4, u32::from_le_bytes(*b"T1\0\0")),
        entry(273, 4, 3, offsets_at),
        entry(277, 3, 1, 1),
        entry(278, 4, 1, 2),
        entry(279, 4, 3, counts_at),
        entry(34665, 4, 1, exif_at),
    ]));
    file.resize(offsets_at as usize, 0);
    for v in [a1, a2, b] {
        file.extend((v as u32).to_le_bytes());
    }
    file.resize(counts_at as usize, 0);
    for v in [2 * rb, rb, 2 * rb] {
        file.extend((v as u32).to_le_bytes());
    }
    file.resize(exif_at as usize, 0);
    file.extend(ifd(&[entry(0xa302, 7, 8, 500), entry(0x927c, 7, 160, note_at)]));
    file.resize(500, 0);
    file.extend([2, 0, 2, 0, 0, 1, 1, 2]);
    file.resize(note_at as usize, 0);
    file.extend(b"OLYMP\0\x01\0");
    let mut note = vec![entry(0x1017, 3, 2, 437 | (62 << 16)), entry(0x1018, 3, 2, 411 | (64 << 16))];
    if matrix.is_some() {
        note.insert(0, entry(0x1011, 3, 9, cm_at));
    }
    file.extend(ifd(&note));
    file.resize(cm_at as usize, 0);
    for v in matrix.unwrap_or_default() {
        file.extend(v.to_le_bytes());
    }
    file.resize(base, 0);
    file.extend(data);
    file
}

const OLYMPUS: [i16; 9] = [372, -116, 0, -58, 404, -90, -2, -135, 393];

fn binding() -> ProfileBinding {
    ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(), sha256: [0; 32] }
}

#[test]
fn single_illuminant_file_matrix_is_refused_by_default_and_developed_on_opt_in() {
    let to_srgb = color::xyz_d50_to_linear_srgb().unwrap();
    let mut raw = decode_container(&orf(Some(OLYMPUS))).unwrap();
    assert_eq!(raw.colour_route, ColourRoute::FileMatrix { tag: "olympus_makernote_0x1011" });
    assert!(raw.colour_route.single_illuminant() && !raw.approximate_colour);
    assert_eq!(raw.color.as_shot_neutral.map(|n| n.map(|v| (v * 1e6).round())), Some([256.0f64 / 437.0, 1.0, 256.0 / 411.0].map(|v| (v * 1e6).round())));
    // Default: every colour entry point refuses, typed; nothing developed, no loss note.
    let refused = Some(DevelopError::ApproximateColourNotAllowed);
    assert_eq!(raw.solve_colour().err(), refused);
    assert_eq!(raw.recipe(&to_srgb, binding(), BaselineExposure::Ignore).err(), refused);
    assert_eq!(raw.recipe_for_white(&to_srgb, binding(), BaselineExposure::Ignore, 5000.0, 0.0).err(), refused);
    assert_eq!(raw.colour_loss(), None);
    assert_eq!(DevelopError::ApproximateColourNotAllowed.code(), "approximate_colour_not_allowed");
    // Opt-in: solved, a recipe exists, and the loss note names the approximation.
    raw.allow_approximate_colour();
    let s = raw.solve_colour().unwrap();
    assert!((s.cct - 6504.0).abs() < 60.0, "{}", s.cct);
    assert_eq!(s.weight, 0.0, "one calibration: nothing to interpolate");
    assert!(raw.recipe(&to_srgb, binding(), BaselineExposure::Ignore).is_ok());
    assert!(raw.recipe_for_white(&to_srgb, binding(), BaselineExposure::Ignore, 5000.0, 0.0).is_ok());
    assert_eq!(raw.colour_loss(), Some(APPROXIMATE_COLOUR_LOSS));
    assert_eq!(APPROXIMATE_COLOUR_LOSS, "approximate_colour_single_illuminant");
    // A matrix whose rows do not sum to 256 is not a colour route at all: missing_colour_data.
    let mut bad = OLYMPUS;
    bad[0] = 400;
    let mut raw = decode_container(&orf(Some(bad))).unwrap();
    raw.allow_approximate_colour();
    assert_eq!(raw.colour_route, ColourRoute::None);
    assert_eq!(raw.solve_colour().err(), Some(DevelopError::MissingColourData));
    let raw = decode_container(&orf(None)).unwrap();
    assert_eq!((raw.colour_route, raw.solve_colour().err()), (ColourRoute::None, Some(DevelopError::MissingColourData)));
}
