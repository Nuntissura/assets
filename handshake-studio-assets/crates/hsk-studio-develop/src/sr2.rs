//! Sony's encrypted `SR2SubIFD` (black level 0x7310), for the key `11 22 33 44` only.
//!
//! Adapted from LightCraft `crates/raw/src/vendor/sr2.rs` (https://github.com/storytold/lightcraft at
//! af4c5e102c9f97529f582003b99c21b89067e906; MIT OR Apache-2.0; attribution in this crate's `NOTICE`):
//! the keystream for key `11 22 33 44` was recovered by LightCraft through black-box known-plaintext
//! analysis of 126 CC0 ARWs (ExifTool's decrypted dump run as a black-box tool XOR-ed with the file
//! bytes), the recurrence `k[n] = k[n - 63] ^ k[n - 127]` found by exhaustive search, and `SEED` is
//! the measured first 127 words. LightCraft's key-derivation for other keys is a recalled scheme;
//! it is deliberately NOT ported (other keys read as absent, the level stays unread).
//!
//! Layout (ExifTool Sony tag names): IFD0 `DNGPrivateData` (0xC634) holds the offset of the
//! `SR2Private` IFD, whose `SR2SubIFDOffset` (0x7200), `SR2SubIFDLength` (0x7201) and `SR2SubIFDKey`
//! (0x7221) locate the encrypted block: a TIFF directory in the file's byte order whose value
//! offsets are file offsets; whole 4-byte words are XOR-ed with the keystream, most significant
//! keystream byte first; a shorter tail is stored in the clear.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::tiffep::{self, Ifd, Tiff};

const KEY: [u8; 4] = [0x11, 0x22, 0x33, 0x44];
const MAX_LEN: usize = 1 << 20;

/// The first 127 keystream words for `KEY` (LightCraft's measured seed).
const SEED: [u32; 127] = [
    0x54c2c4d6, 0x49c478bc, 0x34ae2f8e, 0x25d80504, 0xc0d9d6b0, 0xd838fb71, 0xe8eff27d, 0xfbc1fcea, 0x506c499a, 0x47f20f37, 0x710777cf, 0x7867e7ba,
    0x42d67caa, 0x7f2bd11a, 0x67a216ca, 0x0e986d40, 0x4ae8d4c0, 0xe36778b4, 0x5a958415, 0xdbfe2be8, 0x20faa1aa, 0x7132a6b8, 0xf4de4b7f, 0x55991aa1,
    0xa849d5aa, 0x49577832, 0xb92f3daa, 0x399cc526, 0x22cdd000, 0xe1977a29, 0x37c5db55, 0xb0177e1e, 0x2a1016aa, 0xa300086e, 0x3bab9bfe, 0x262eece0,
    0x23771aa9, 0x0a5dc91c, 0x31b902ae, 0x58e64bf8, 0x259c300e, 0xa57705c8, 0x284a6541, 0xfb229c60, 0x1bacaa9e, 0xbcab3350, 0x67cd9fbe, 0x8f135e60,
    0xf8c26a40, 0x6770da61, 0x3e1febfd, 0xd0c70803, 0x8dbb037b, 0x6f6fa4c5, 0x6749d10d, 0x7f51598d, 0xd5e5a4ec, 0x207dfa91, 0x6558ebc2, 0xbe594639,
    0x617a9e5d, 0x3c497950, 0x0844eb3f, 0x04207ed2, 0xd27ceac4, 0x70d20f05, 0xb47003f6, 0xe9e4e3ae, 0xcc19d265, 0x326dd956, 0xf0d3a327, 0xb71275f0,
    0x7994e285, 0x0aff594d, 0x128e8345, 0x7bda597a, 0xd634c380, 0xe24a006f, 0x8974818b, 0x3320b22a, 0xbe808417, 0xa2d5648a, 0x6fe80b39, 0x23ebad41,
    0xa2d11e5d, 0x027d9397, 0x9a722ac8, 0x432c7dac, 0x7146692a, 0x82a3dc77, 0xd66887c5, 0x831f43b7, 0x4e5dddde, 0x03793f81, 0x306ab437, 0x00ccf86c,
    0xfc6ed3d2, 0x076b8fdb, 0x9808cfca, 0x0f4eef6e, 0xc8cc3830, 0x104ac16a, 0xa189eff4, 0x3e085c08, 0xd28baf88, 0x5c853ac4, 0xe60480f8, 0xc51acd98,
    0x691e5ee1, 0x333feeb9, 0x1e35bc33, 0xec4a4642, 0xee57c5a5, 0xbeeb51f7, 0xe0c4f32c, 0xa5422f6a, 0x1d266d12, 0x3752fd3b, 0xfbc53c7d, 0x2421a4a3,
    0xcdc6a2de, 0x26e6b330, 0x6c073d46, 0x058e2f27, 0x43833f30, 0x46d1382e, 0x5f0804ec,
];

/// XOR whole 4-byte words of `block` with the keystream (encrypts and decrypts alike).
fn apply_keystream(block: &mut [u8]) {
    let mut ring = SEED;
    for (n, word) in block.chunks_exact_mut(4).enumerate() {
        // ring[i] holds k[n - 127] until replaced by k[n]; k[n - 63] sits at (n - 63) mod 127.
        let i = n % 127;
        if n >= 127 {
            let other = ring.get(i.saturating_add(64) % 127).copied().unwrap_or(0);
            if let Some(r) = ring.get_mut(i) {
                *r ^= other;
            }
        }
        let k = ring.get(i).copied().unwrap_or(0).to_be_bytes();
        word.iter_mut().zip(k).for_each(|(b, k)| *b ^= k);
    }
}

/// The black level (mean of the four 0x7310 values) from the file's `SR2SubIFD`; `None` when the
/// file has none, uses another key, or the values are implausible (outside 64..=4096 or spread
/// over more than 64 codes, LightCraft's plausibility rule).
pub(crate) fn black_level(tiff: &Tiff<'_>, root: &Ifd, bytes: &[u8]) -> Option<f64> {
    let private = tiff.ifd(tiffep::first_u32(tiff, root.find(0xc634)?).ok()?).ok()?;
    let key = tiff.bytes(private.find(0x7221)?).ok()?;
    if key != KEY {
        return None;
    }
    let start = tiffep::first_u32(tiff, private.find(0x7200)?).ok()? as usize;
    let len = tiffep::first_u32(tiff, private.find(0x7201)?).ok()? as usize;
    if len > MAX_LEN {
        return None;
    }
    let mut plain = bytes.get(start..start.checked_add(len)?)?.to_vec();
    apply_keystream(&mut plain);
    let be = tiff.is_big_endian();
    let u16_at = |b: &[u8], at: usize| -> Option<u16> {
        let s: [u8; 2] = b.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if be { u16::from_be_bytes(s) } else { u16::from_le_bytes(s) })
    };
    let u32_at = |b: &[u8], at: usize| -> Option<u32> {
        let s: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if be { u32::from_be_bytes(s) } else { u32::from_le_bytes(s) })
    };
    let entries = usize::from(u16_at(&plain, 0)?);
    let entry = (0..entries.min(1024))
        .map_while(|i| {
            let at = i.checked_mul(12)?.checked_add(2)?;
            plain.get(at..at.checked_add(12)?)
        })
        .find(|e| u16_at(e, 0) == Some(0x7310))?;
    if u16_at(entry, 2)? != 3 || u32_at(entry, 4)? != 4 {
        return None;
    }
    let at = (u32_at(entry, 8)? as usize).checked_sub(start)?;
    let levels: Vec<u16> = (0..4usize).map(|i| u16_at(&plain, at.checked_add(i.checked_mul(2)?)?)).collect::<Option<_>>()?;
    let (lo, hi) = (*levels.iter().min()?, *levels.iter().max()?);
    ((64..=4096).contains(&lo) && hi.saturating_sub(lo) <= 64).then(|| levels.iter().map(|v| f64::from(*v)).sum::<f64>() / 4.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated keystream starts with the measured seed, follows the recurrence
    /// k[n] = k[n - 63] ^ k[n - 127], and applying it twice restores the plaintext.
    #[test]
    fn keystream_round_trips_and_follows_the_recurrence() {
        let mut ks = vec![0u8; 4 * 400];
        apply_keystream(&mut ks);
        let word = |n: usize| u32::from_be_bytes(ks[4 * n..4 * n + 4].try_into().unwrap());
        assert_eq!(word(0), SEED[0]);
        for n in 127..400 {
            assert_eq!(word(n), word(n - 63) ^ word(n - 127), "word {n}");
        }
        let plain: Vec<u8> = (0..64u8).collect();
        let mut b = plain.clone();
        apply_keystream(&mut b);
        assert_ne!(b, plain);
        apply_keystream(&mut b);
        assert_eq!(b, plain);
    }
}
