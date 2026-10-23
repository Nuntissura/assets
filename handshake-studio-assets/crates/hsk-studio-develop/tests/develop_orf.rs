//! Olympus ORF two-field 12-bit layout through `decode_container`: a synthetic `IIRS` file built here
//! (width 8, height 5): strips A1 (output rows 0, 2), A2 (row 4), a gap (the field seam), B (rows 1
//! and 3), then the last stored row of the second field (past the picture) right after B, outside
//! the strip table, as the old compacts have it; samples packed MSB first two per three bytes; Exif
//! CFAPattern 2x2 GRBG.
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

/// Little-endian IFD entry.
fn entry(tag: u16, ty: u16, count: u32, value: u32) -> Vec<u8> {
    [tag.to_le_bytes().to_vec(), ty.to_le_bytes().to_vec(), count.to_le_bytes().to_vec(), value.to_le_bytes().to_vec()].concat()
}

/// `gap`: leave the seam gap between the fields; `cfa`: write the Exif CFAPattern; `compression`.
fn orf(gap: bool, cfa: bool, compression: u16, dims: (u32, u32)) -> Vec<u8> {
    let rb = W * 3 / 2;
    // Data area after the IFDs at 512: A1 (rows 0, 2), A2 (row 4), [gap 7 bytes], B (rows 1, 3), then
    // the padding row of the second field.
    let base = 512usize;
    let a1 = base;
    let a2 = a1 + 2 * rb;
    let b = a2 + rb + if gap { 7 } else { 0 };
    let mut data = vec![0xffu8; b + 3 * rb - base];
    for (at, y) in [(a1, 0), (a1 + rb, 2), (a2, 4), (b, 1), (b + rb, 3)] {
        data[at - base..at - base + rb].copy_from_slice(&packed_row(y));
    }
    let offsets_at = 300u32;
    let counts_at = 320u32;
    let exif_at = 400u32;
    let mut ifd = Vec::new();
    let entries = [
        entry(256, 4, 1, dims.0),
        entry(257, 4, 1, dims.1),
        entry(258, 3, 1, 12),
        entry(259, 3, 1, u32::from(compression)),
        entry(262, 3, 1, 32803),
        entry(272, 2, 4, u32::from_le_bytes(*b"T1\0\0")),
        entry(273, 4, 3, offsets_at),
        entry(277, 3, 1, 1),
        entry(278, 4, 1, 2),
        entry(279, 4, 3, counts_at),
        entry(34665, 4, 1, exif_at),
    ];
    ifd.extend((entries.len() as u16).to_le_bytes());
    for e in entries {
        ifd.extend(e);
    }
    ifd.extend(0u32.to_le_bytes());
    let mut file = b"IIRS\x08\0\0\0".to_vec();
    file.extend(ifd);
    file.resize(offsets_at as usize, 0);
    for v in [a1, a2, b] {
        file.extend((v as u32).to_le_bytes());
    }
    file.resize(counts_at as usize, 0);
    for v in [2 * rb, rb, 2 * rb] {
        file.extend((v as u32).to_le_bytes());
    }
    file.resize(exif_at as usize, 0);
    if cfa {
        file.extend(1u16.to_le_bytes());
        file.extend(entry(0xa302, 7, 8, 420));
        file.extend(0u32.to_le_bytes());
        file.resize(420, 0);
        file.extend([2, 0, 2, 0, 1, 0, 2, 1]);
    } else {
        file.extend(0u16.to_le_bytes());
        file.extend(0u32.to_le_bytes());
    }
    file.resize(base, 0);
    file.extend(data);
    file
}

#[test]
fn orf_two_field_layout_decodes_and_hostile_variants_are_typed() {
    let raw = decode_container(&orf(true, true, 1, (W as u32, H as u32))).unwrap();
    assert_eq!((raw.width, raw.height, raw.bits_per_sample, raw.white_level), (8, 5, 12, 4095));
    assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Grbg));
    assert_eq!(raw.unique_camera_model.as_deref(), Some("T1"));
    for y in 0..H {
        for x in 0..W {
            assert_eq!(raw.sample(x as u32, y as u32), px(x, y), "({x}, {y})");
        }
    }
    assert!(raw.unapplied.contains(&"orf_black_level_unread"));
    // No field seam (or any other layout, e.g. Olympus compressed ORF): typed, never guessed.
    assert_eq!(decode_container(&orf(false, true, 1, (8, 5))).err(), Some(DevelopError::DecoderUnavailable("orf")));
    // Dimension bomb: 65534 x 4095 declared over 60 bytes of strips fails the exact strip-total check
    // before any allocation.
    assert_eq!(decode_container(&orf(true, true, 1, (65_534, 4_095))).err(), Some(DevelopError::DecoderUnavailable("orf")));
    // Truncated: the trailing stored row of the second field is cut off.
    let mut cut = orf(true, true, 1, (8, 5));
    cut.truncate(cut.len() - 1);
    assert_eq!(decode_container(&cut).err(), Some(DevelopError::DecoderUnavailable("orf")));
    assert_eq!(decode_container(&orf(true, true, 7, (8, 5))).err(), Some(DevelopError::UnsupportedCompression));
    assert_eq!(decode_container(&orf(true, false, 1, (8, 5))).err(), Some(DevelopError::UnsupportedPattern));
}
