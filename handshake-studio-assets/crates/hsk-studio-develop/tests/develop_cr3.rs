//! Canon CR3 through `decode_container`: synthetic ISO-BMFF files built here (ftyp `crx `, moov with
//! the Canon uuid box holding CMT1, one raw `trak` whose `CRAW` entry carries `CMP1`, sample located
//! by stsz/co64) around a hand-built CRX version 0x100 lossless sample (4 planes of 2x2 at
//! coefficient 1: zero-run flag 0, Rice(2,k0)=001, Rice(0,k0)=1, then a nonzero run; LightCraft's
//! test stream, see crate NOTICE). Every sensor sample is the 14-bit midpoint + 1 = 8193.
use hsk_studio_develop::*;

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

fn full(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 4];
    b.extend_from_slice(body);
    bx(kind, &b)
}

/// CMP1 payload: version, sensor/tile size, bit depth, planes<<4|cfa, levels, header size.
fn cmp1(version: u16, w: u32, h: u32, levels: u8, cfa: u8, header: u32) -> Vec<u8> {
    let mut b = vec![0u8; 52];
    b[0..2].copy_from_slice(&0xff00u16.to_be_bytes());
    b[2..4].copy_from_slice(&48u16.to_be_bytes());
    b[4..6].copy_from_slice(&version.to_be_bytes());
    for (at, v) in [(8, w), (12, h), (16, w), (20, h), (28, header)] {
        b[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }
    b[24] = 14;
    b[25] = 0x40 | cfa;
    b[26] = levels;
    b
}

fn tiff_model(model: &str) -> Vec<u8> {
    // Little-endian TIFF, one IFD with Model (ASCII, out of line).
    let mut t = b"II*\0\x08\0\0\0".to_vec();
    t.extend(1u16.to_le_bytes());
    t.extend(272u16.to_le_bytes());
    t.extend(2u16.to_le_bytes());
    t.extend(((model.len() + 1) as u32).to_le_bytes());
    t.extend(26u32.to_le_bytes());
    t.extend(0u32.to_le_bytes());
    t.extend(model.as_bytes());
    t.push(0);
    t
}

/// A CR3 file whose raw track holds `sample` (stsz may claim `claimed` bytes instead).
fn cr3(coding: &[u8], sample: &[u8], claimed: Option<u32>) -> Vec<u8> {
    let mut craw = vec![0u8; 82];
    craw[24..26].copy_from_slice(&4u16.to_be_bytes());
    craw[26..28].copy_from_slice(&4u16.to_be_bytes());
    craw.extend(bx(b"CMP1", coding));
    let mut canon_uuid = vec![0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
    canon_uuid.extend(bx(b"CMT1", &tiff_model("Canon EOS Test")));
    let canon = bx(b"uuid", &canon_uuid);
    let build = |offset: u64| {
        let stsd = full(b"stsd", &[1u32.to_be_bytes().to_vec(), bx(b"CRAW", &craw)].concat());
        let stsz = full(b"stsz", &[0u32.to_be_bytes(), 1u32.to_be_bytes(), claimed.unwrap_or(sample.len() as u32).to_be_bytes()].concat());
        let co64 = full(b"co64", &[1u32.to_be_bytes().to_vec(), offset.to_be_bytes().to_vec()].concat());
        let trak = bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &[stsd, stsz, co64].concat()))));
        let mut file = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
        file.extend(bx(b"moov", &[canon.clone(), trak].concat()));
        file
    };
    let head = build(0).len() as u64 + 8;
    let mut file = build(head);
    file.extend(bx(b"mdat", sample));
    file
}

fn marker(out: &mut Vec<u8>, code: u16, size: u32, flags: u32) {
    out.extend(code.to_be_bytes());
    out.extend(8u16.to_be_bytes());
    out.extend(size.to_be_bytes());
    out.extend(flags.to_be_bytes());
}

fn flat_sample() -> Vec<u8> {
    let mut out = Vec::new();
    marker(&mut out, 0xff01, 4, 0);
    for p in 0..4u32 {
        marker(&mut out, 0xff02, 1, (p << 28) | 0x0800_0000);
        marker(&mut out, 0xff03, 1, 0x0020_0000);
    }
    out.extend([0x1e; 4]);
    out
}

#[test]
fn cr3_lossless_crx_decodes_the_sensor_mosaic_and_refuses_hostile_input() {
    let raw = decode_container(&cr3(&cmp1(0x100, 4, 4, 0, 3, 108), &flat_sample(), None)).unwrap();
    assert_eq!((raw.width, raw.height, raw.bits_per_sample, raw.white_level), (4, 4, 14, 16383));
    assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Bggr), "CMP1 CFA code 3");
    assert_eq!(raw.unique_camera_model.as_deref(), Some("Canon EOS Test"));
    assert!((0..4).all(|y| (0..4).all(|x| raw.sample(x, y) == 8193)));
    assert!(raw.unapplied.contains(&"cr3_black_level_unread"));
    assert_eq!(raw.solve_colour().err(), Some(DevelopError::MissingColourData));
    // Truncated sample (stsz claims more than the file holds), and every cut of the CRX stream.
    let e = decode_container(&cr3(&cmp1(0x100, 4, 4, 0, 0, 108), &flat_sample(), Some(4096)));
    assert_eq!(e.err(), Some(DevelopError::InvalidContainer));
    let sample = flat_sample();
    for cut in 0..sample.len() {
        assert!(decode_container(&cr3(&cmp1(0x100, 4, 4, 0, 0, 108), &sample[..cut], None)).is_err(), "cut {cut}");
    }
    // Bad entropy: alignment bits set in the last byte.
    let mut bad = flat_sample();
    *bad.last_mut().unwrap() = 0x1f;
    assert_eq!(decode_container(&cr3(&cmp1(0x100, 4, 4, 0, 0, 108), &bad, None)).err(), Some(DevelopError::InvalidContainer));
    // Dimension bomb: 65534 x 65534 declared over the 112-byte sample is refused before allocation.
    let e = decode_container(&cr3(&cmp1(0x100, 65_534, 65_534, 0, 0, 108), &flat_sample(), None));
    assert_eq!(e.err(), Some(DevelopError::InvalidDimensions));
    // Unsupported coding variants are typed: 2 wavelet levels, version 0x300, CFA code 5.
    for coding in [cmp1(0x100, 4, 4, 2, 0, 108), cmp1(0x300, 4, 4, 0, 0, 108)] {
        assert_eq!(decode_container(&cr3(&coding, &flat_sample(), None)).err(), Some(DevelopError::UnsupportedCompression));
    }
    assert_eq!(decode_container(&cr3(&cmp1(0x100, 4, 4, 0, 5, 108), &flat_sample(), None)).err(), Some(DevelopError::UnsupportedPattern));
}
