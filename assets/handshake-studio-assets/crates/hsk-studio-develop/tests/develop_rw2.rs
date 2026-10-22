//! Panasonic RW2 through `decode_container`: synthetic `IIU` files built here (Panasonic IFD0 tags
//! 0x0002/0x0003 size, 0x0009 CFA code, 0x000A bits, 0x002D raw format, 0x0118 raw offset) around
//! (a) a format-4 block written by hand from the documented field layout (known answers) and (b) a
//! format-5 packed stream whose first 0x4000-byte chunk is stored rotated by 0x1FF8.
use hsk_studio_develop::*;

fn entry(tag: u16, ty: u16, count: u32, value: u32) -> Vec<u8> {
    [tag.to_le_bytes().to_vec(), ty.to_le_bytes().to_vec(), count.to_le_bytes().to_vec(), value.to_le_bytes().to_vec()].concat()
}

fn rw2(w: u32, h: u32, cfa: u32, format: u32, data: &[u8]) -> Vec<u8> {
    let entries = [
        entry(0x0002, 3, 1, w),
        entry(0x0003, 3, 1, h),
        entry(0x0009, 3, 1, cfa),
        entry(0x000a, 3, 1, 12),
        entry(0x002d, 3, 1, format),
        entry(0x0118, 4, 1, 256),
        entry(272, 2, 4, u32::from_le_bytes(*b"LX\0\0")),
    ];
    let mut file = b"IIU\0\x08\0\0\0".to_vec();
    file.extend((entries.len() as u16).to_le_bytes());
    for e in entries {
        file.extend(e);
    }
    file.extend(0u32.to_le_bytes());
    file.resize(256, 0);
    file.extend_from_slice(data);
    file
}

/// MSB-first field writer into a 128-bit block (bit 127 down), stored little-endian.
struct Block {
    word: u128,
    left: u32,
}

impl Block {
    fn put(&mut self, v: u32, n: u32) {
        self.left -= n;
        self.word |= u128::from(v) << self.left;
    }
}

/// Seeds 0x10/5 and 0x20/3; scales 0 (step 1), 3 (step 16, absolute), 1 (step 2), 2 (step 4).
fn format4_block() -> ([u8; 16], [u16; 14]) {
    let mut b = Block { word: 0, left: 128 };
    b.put(0x10, 8);
    b.put(0x5, 4); // i0 = 261
    b.put(0x20, 8);
    b.put(0x3, 4); // i1 = 515
    b.put(0, 2);
    for j in [130, 0, 120] {
        b.put(j, 8); // i2 = 263, i3 = 515 (kept), i4 = 255
    }
    b.put(3, 2);
    for j in [40, 20, 0] {
        b.put(j, 8); // i5 = 40*16 + (515 & 15) = 643, i6 = 20*16 + (255 & 15) = 335, i7 = 643
    }
    b.put(1, 2);
    for j in [129, 127, 128] {
        b.put(j, 8); // i8 = 337, i9 = 641, i10 = 337
    }
    b.put(2, 2);
    for j in [130, 126, 0] {
        b.put(j, 8); // i11 = 649, i12 = 126*4 + (337 & 3) = 505 (337 < 128*4: absolute), i13 = 649
    }
    assert_eq!(b.left, 0);
    (b.word.to_le_bytes(), [261, 515, 263, 515, 255, 643, 335, 643, 337, 641, 337, 649, 505, 649])
}

#[test]
fn rw2_format4_known_answers_and_format5_rotated_chunks() {
    // Format 4: one 14 x 1 row in a single (short, unrotated) chunk.
    let (block, want) = format4_block();
    let raw = decode_container(&rw2(14, 1, 2, 4, &block)).unwrap();
    assert_eq!((raw.width, raw.height, raw.bits_per_sample, raw.white_level), (14, 1, 12, 4095));
    assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Grbg));
    assert_eq!(raw.unique_camera_model.as_deref(), Some("LX"));
    assert_eq!((0..14).map(|x| raw.sample(x, 0)).collect::<Vec<_>>(), want);
    assert!(raw.unapplied.contains(&"rw2_black_level_unread"));
    // Format 5: 128 x 100 samples, ten 12-bit values from bit 0 up per 16-byte block, every row
    // starting a new block (13 blocks per row, 2 surplus samples; lane SMB-RAW-CLEANROOM-2: the
    // converter oracle reads blocks row aligned, LX7 width-1900 probe); the first 0x4000 bytes are
    // stored rotated (logical 0x1FF8.. first), the rest is a short final chunk. Values avoid 0,
    // which the packed formats treat as a missing pixel.
    let (w, h) = (128usize, 100usize);
    let value = |i: usize| (1 + (i * 7 + i / 128 * 13) % 4095) as u16;
    let mut logical = Vec::new();
    for y in 0..h {
        for block in 0..w.div_ceil(10) {
            let mut word = 0u128;
            for k in 0..10 {
                let x = block * 10 + k;
                word |= u128::from(if x < w { value(y * w + x) } else { 0 }) << (12 * k);
            }
            logical.extend(word.to_le_bytes());
        }
    }
    let mut stored = logical[0x4000 - 0x1ff8..0x4000].to_vec();
    stored.extend_from_slice(&logical[..0x4000 - 0x1ff8]);
    stored.extend_from_slice(&logical[0x4000..]);
    let raw = decode_container(&rw2(w as u32, h as u32, 1, 5, &stored)).unwrap();
    assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Rggb));
    assert!((0..w * h).all(|i| raw.sample((i % w) as u32, (i / w) as u32) == value(i)), "rotated chunk order");
}

#[test]
fn rw2_hostile_and_unsupported_inputs_are_typed_refusals() {
    let (block, _) = format4_block();
    // Truncated stream: 28 pixels need two blocks.
    assert_eq!(decode_container(&rw2(28, 1, 1, 4, &block)).err(), Some(DevelopError::InvalidContainer));
    // Dimension bomb: 65535 x 4096 declared over one block is refused by the stream-size check
    // before the frame is allocated; past the pixel cap it is invalid_dimensions.
    assert_eq!(decode_container(&rw2(65_535, 4_096, 1, 4, &block)).err(), Some(DevelopError::InvalidContainer));
    assert_eq!(decode_container(&rw2(65_535, 65_535, 1, 4, &block)).err(), Some(DevelopError::InvalidDimensions));
    // Format 8 (prefix-coded strips) and unknown formats: typed, never guessed.
    for format in [8, 9] {
        assert_eq!(decode_container(&rw2(14, 1, 1, format, &block)).err(), Some(DevelopError::DecoderUnavailable("rw2")));
    }
    // Unknown CFA code.
    assert_eq!(decode_container(&rw2(14, 1, 7, 4, &block)).err(), Some(DevelopError::UnsupportedPattern));
}
