//! Fujifilm compressed RAF through `decode_container`: synthetic RAF files built here (header with
//! pointers and compression mode, record directory with the X-Trans layout, little-endian TIFF raw
//! block whose 0xF000 raw IFD locates the strip) around procedural zero-residual streams (every
//! coded value is the Rice code of 0 under the decoder's adaptive parameter, as in LightCraft's
//! test generator; see crate NOTICE). Two stripes of 768 + 12 columns: the second stripe is mostly
//! padding, which must never reach the output.
use hsk_studio_develop::*;

const XTRANS: [[u8; 6]; 6] = [[1, 1, 0, 1, 1, 2], [1, 1, 2, 1, 1, 0], [2, 0, 1, 0, 2, 1], [1, 1, 2, 1, 1, 0], [1, 1, 0, 1, 1, 2], [0, 2, 1, 2, 0, 1]];
const MASKS: [[u8; 2]; 6] = [[10, 15], [15, 10], [14, 10], [15, 11], [11, 15], [10, 14]];

fn zero_stream(bits: u32, xtrans: bool, quantizers: Option<&[u8]>) -> Vec<u8> {
    let groups = quantizers.map_or(3, <[u8]>::len);
    let seed = (1u32 << (bits - 6), 1u32);
    let mut main = [[seed; 2]; 3];
    let mut fine = main;
    let mut previous = None;
    let mut stream = Vec::new();
    for group in 0..groups {
        let q = quantizers.map_or(0, |q| q[group]);
        if previous != Some(q) {
            main = [[seed; 2]; 3];
            previous = Some(q);
        }
        for (pair, masks) in MASKS.iter().enumerate() {
            let width = if xtrans { 512 } else { 384 };
            for even in (0usize..width + 8).step_by(2) {
                for (parity, column) in [(0, Some(even)), (1, even.checked_sub(7))] {
                    let Some(column) = column.filter(|&x| x < width) else { continue };
                    for mask in masks {
                        if xtrans && (mask >> (column % 4)) & 1 == 0 {
                            continue;
                        }
                        let (sum, count) = &mut if q == 0 { &mut main } else { &mut fine }[pair % 3][parity];
                        let k = 32 - (sum.saturating_sub(1) / *count).leading_zeros();
                        stream.push(true);
                        stream.extend(std::iter::repeat_n(false, k as usize));
                        if *count == 64 {
                            *sum /= 2;
                            *count /= 2;
                        }
                        *count += 1;
                    }
                }
            }
        }
    }
    stream.chunks(8).map(|c| c.iter().enumerate().fold(0, |b, (i, bit)| b | (u8::from(*bit) << (7 - i)))).collect()
}

/// `IS` stream for a 780-wide frame of `groups * 6` rows (two stripes).
fn compressed(bits: u32, xtrans: bool, quantizers: Option<&[u8]>) -> Vec<u8> {
    let groups = quantizers.map_or(3, <[u8]>::len);
    let stream = zero_stream(bits, xtrans, quantizers);
    let mut out = b"IS".to_vec();
    out.extend_from_slice(&[u8::from(quantizers.is_none()), if xtrans { 16 } else { 0 }, bits as u8]);
    for v in [groups as u16 * 6, 1536, 780, 768] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.push(2);
    out.extend_from_slice(&(groups as u16).to_be_bytes());
    for _ in 0..2 {
        out.extend_from_slice(&(stream.len() as u32).to_be_bytes());
    }
    out.resize(32, 0);
    if let Some(q) = quantizers {
        for _ in 0..2 {
            out.extend_from_slice(q);
            out.resize(out.len() + q.len().div_ceil(16) * 16 - q.len(), 0);
        }
    }
    out.extend_from_slice(&stream);
    out.extend_from_slice(&stream);
    out
}

/// A RAF: header (pointers at 84.., mode at 108), records (optional X-Trans layout), raw block.
fn raf(w: u32, h: u32, bits: u32, strip: &[u8], layout: Option<[u8; 36]>, mode: u32) -> Vec<u8> {
    // Raw block: LE TIFF, IFD0 at 8 with one entry 0xF000 (type IFD) -> raw IFD at 26.
    let mut block = b"II*\0\x08\0\0\0".to_vec();
    block.extend(1u16.to_le_bytes());
    block.extend([0x00, 0xf0, 13, 0, 1, 0, 0, 0]);
    block.extend(26u32.to_le_bytes());
    block.extend(0u32.to_le_bytes());
    let entries: [(u16, u32); 5] = [(0xf001, w), (0xf002, h), (0xf003, bits), (0xf007, 26 + 2 + 5 * 12 + 4), (0xf008, strip.len() as u32)];
    block.extend((entries.len() as u16).to_le_bytes());
    for (tag, v) in entries {
        block.extend(tag.to_le_bytes());
        block.extend(4u16.to_le_bytes());
        block.extend(1u32.to_le_bytes());
        block.extend(v.to_le_bytes());
    }
    block.extend(0u32.to_le_bytes());
    block.extend_from_slice(strip);
    let mut records = Vec::new();
    let mut count = 0u32;
    if let Some(l) = layout {
        records.extend(0x0131u16.to_be_bytes());
        records.extend(36u16.to_be_bytes());
        records.extend(l);
        count += 1;
    }
    let mut dir = count.to_be_bytes().to_vec();
    dir.extend(records);
    let mut file = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
    file.resize(28, 0);
    file.extend(b"X-TEST");
    file.resize(112, 0);
    let dir_at = 112u32;
    let raw_at = dir_at + dir.len() as u32;
    for (at, v) in [(92, dir_at), (96, dir.len() as u32), (100, raw_at), (104, block.len() as u32), (108, mode)] {
        file[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }
    file.extend(dir);
    file.extend(block);
    file
}

fn layout() -> [u8; 36] {
    let mut l = [0; 36];
    for (to, from) in l.iter_mut().zip(XTRANS.iter().flatten().rev()) {
        *to = *from;
    }
    l
}

#[test]
fn raf_compressed_bayer_and_xtrans_decode_through_the_container() {
    for bits in [12, 14, 16] {
        for xtrans in [false, true] {
            for q in [None, Some([2u8, 0, 5].as_slice())] {
                let mode = if q.is_none() { 2 } else { 3 };
                let file = raf(780, 18, bits, &compressed(bits, xtrans, q), xtrans.then(layout), mode);
                let raw = decode_container(&file).unwrap_or_else(|e| panic!("{bits} {xtrans} {q:?}: {e}"));
                assert_eq!((raw.width, raw.height, u32::from(raw.bits_per_sample)), (780, 18, bits));
                let want = if xtrans { CfaPattern::XTrans } else { CfaPattern::Bayer(Bayer::Rggb) };
                assert_eq!(raw.cfa_pattern, want);
                assert_eq!(raw.unique_camera_model.as_deref(), Some("X-TEST"));
                assert!((0..18).all(|y| (0..780).all(|x| raw.sample(x, y) == 0)), "zero residuals reconstruct zero");
                assert!(raw.unapplied.contains(&"raf_black_level_unread"));
            }
        }
    }
}

#[test]
fn raf_hostile_and_unsupported_inputs_are_typed_refusals() {
    let strip = compressed(14, true, Some(&[1, 2, 0]));
    let ok = raf(780, 18, 14, &strip, Some(layout()), 3);
    assert!(decode_container(&ok).is_ok());
    // Truncated strips (the raw IFD byte count shrinks with the strip) and header byte flips.
    for len in [0usize, 10, 40, 95, strip.len() / 2, strip.len() - 1] {
        assert!(decode_container(&raf(780, 18, 14, &strip[..len], Some(layout()), 3)).is_err(), "length {len}");
    }
    for at in [2usize, 3, 4, 5, 7, 9, 11, 13, 14, 16] {
        let mut bad = strip.clone();
        bad[at] ^= 0xff;
        assert!(decode_container(&raf(780, 18, 14, &bad, Some(layout()), 3)).is_err(), "header byte {at}");
    }
    // Dimension bombs: a consistent IS header for 16384 x 16380 (22 stripes of 1 byte each) is
    // refused by the payload check before the frame is allocated; 65535 x 65532 exceeds the cap.
    let bomb = |w: u16, h: u16| {
        let count = (w as usize).div_ceil(768);
        let mut s = vec![b'I', b'S', 1, 0, 14];
        for v in [h, (count * 768) as u16, w, 768] {
            s.extend(v.to_be_bytes());
        }
        s.push(count as u8);
        s.extend((h / 6).to_be_bytes());
        for _ in 0..count {
            s.extend(1u32.to_be_bytes());
        }
        s.resize((16 + count * 4).div_ceil(16) * 16, 0);
        s.extend(std::iter::repeat_n(0x80u8, count));
        raf(u32::from(w), u32::from(h), 14, &s, None, 2)
    };
    assert_eq!(decode_container(&bomb(16_384, 16_380)).err(), Some(DevelopError::InvalidContainer));
    assert_eq!(decode_container(&bomb(65_535, 65_532)).err(), Some(DevelopError::InvalidDimensions));
    // Container says lossless, stream says lossy; unknown modes; uncompressed (out of scope).
    assert_eq!(decode_container(&raf(780, 18, 14, &strip, Some(layout()), 2)).err(), Some(DevelopError::InvalidContainer));
    assert_eq!(decode_container(&raf(780, 18, 14, &strip, Some(layout()), 4)).err(), Some(DevelopError::UnsupportedCompression));
    assert_eq!(decode_container(&raf(780, 18, 14, &strip, Some(layout()), 0)).err(), Some(DevelopError::DecoderUnavailable("raf_uncompressed")));
    // A different X-Trans arrangement (red and blue swapped) is not the codec's fixed layout.
    let swapped = layout().map(|c| [2, 1, 0][c as usize]);
    assert_eq!(decode_container(&raf(780, 18, 14, &strip, Some(swapped), 3)).err(), Some(DevelopError::UnsupportedPattern));
    // An older layout without a TIFF raw block (e.g. SuperCCD FinePix).
    let mut old = ok.clone();
    let raw_at = u32::from_be_bytes(old[100..104].try_into().unwrap()) as usize;
    old[raw_at..raw_at + 4].copy_from_slice(b"\0 \0 ");
    assert_eq!(decode_container(&old).err(), Some(DevelopError::DecoderUnavailable("raf")));
}
