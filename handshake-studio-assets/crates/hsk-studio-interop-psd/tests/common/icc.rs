//! Closed-form synthetic ICC profiles and colour maths for the display-conversion tests: no installed
//! or external profile bytes, so the tests do not depend on any file outside the repository.
#![allow(dead_code)]

pub const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}
fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}
pub fn xyz_tag(v: [f64; 3]) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    for c in v {
        t.extend_from_slice(&s15(c));
    }
    t
}
/// `curv` with a gamma exponent stored as u8Fixed8 (`gamma * 256` must be integral to be exact).
pub fn curv(gamma: f64) -> Vec<u8> {
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend_from_slice(&be32(1));
    t.extend_from_slice(&((gamma * 256.0).round() as u16).to_be_bytes());
    t.extend_from_slice(&[0, 0]);
    t
}
/// Assembles a profile; tags reference blobs by index.
pub fn assemble(version: u32, class: &[u8; 4], space: &[u8; 4], pcs: &[u8; 4], blobs: &[Vec<u8>], tags: &[(&[u8; 4], usize)]) -> Vec<u8> {
    let mut out = vec![0u8; 128];
    out[8..12].copy_from_slice(&version.to_be_bytes());
    out[12..16].copy_from_slice(class);
    out[16..20].copy_from_slice(space);
    out[20..24].copy_from_slice(pcs);
    out[36..40].copy_from_slice(b"acsp");
    for (i, v) in D50.into_iter().enumerate() {
        out[68 + i * 4..72 + i * 4].copy_from_slice(&s15(v));
    }
    out.extend_from_slice(&be32(tags.len() as u32));
    let mut offsets = Vec::new();
    let mut at = 132 + tags.len() * 12;
    for blob in blobs {
        offsets.push(at);
        at += blob.len().div_ceil(4) * 4;
    }
    for (sig, blob) in tags {
        out.extend_from_slice(*sig);
        out.extend_from_slice(&be32(offsets[*blob] as u32));
        out.extend_from_slice(&be32(blobs[*blob].len() as u32));
    }
    for blob in blobs {
        out.extend_from_slice(blob);
        out.resize(out.len().div_ceil(4) * 4, 0);
    }
    let len = out.len() as u32;
    out[..4].copy_from_slice(&be32(len));
    out
}
/// Legacy (v2) 16-bit Lab encoding: L 0xFF00 = 100, a/b 0x8000 = 0 at 1/256 per unit.
fn lab16(l: f64, a: f64, b: f64) -> [u16; 3] {
    let q = |v: f64| v.round().clamp(0.0, 65535.0) as u16;
    [q(l / 100.0 * 65280.0), q((a + 128.0) * 256.0), q((b + 128.0) * 256.0)]
}
/// `mft2` with identity 2-entry input and output tables and an identity matrix.
fn mft2(in_ch: u8, out_ch: u8, grid: u8, clut: &[u16]) -> Vec<u8> {
    assert_eq!(clut.len(), (grid as usize).pow(u32::from(in_ch)) * usize::from(out_ch));
    let mut t = b"mft2\0\0\0\0".to_vec();
    t.extend_from_slice(&[in_ch, out_ch, grid, 0]);
    for v in [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        t.extend_from_slice(&s15(v));
    }
    t.extend_from_slice(&2u16.to_be_bytes());
    t.extend_from_slice(&2u16.to_be_bytes());
    for _ in 0..in_ch {
        t.extend_from_slice(&[0, 0, 0xFF, 0xFF]);
    }
    for v in clut {
        t.extend_from_slice(&v.to_be_bytes());
    }
    for _ in 0..out_ch {
        t.extend_from_slice(&[0, 0, 0xFF, 0xFF]);
    }
    t
}

/// The ink-to-Lab law of the synthetic printer: linear in the inks, so a grid of 2 nodes per axis
/// is exact and the lattice nodes (ink multiples of 1/16 after Prism's 17-point bake) are exact.
/// Cyan reaches far outside sRGB (`a = -110` at `L = 80`).
pub fn printer_lab(c: f64, m: f64, y: f64, k: f64) -> (f64, f64, f64) {
    (100.0 - 60.0 * k - 20.0 * c - 10.0 * m - 5.0 * y, -110.0 * c + 60.0 * m, 40.0 * y)
}
/// ICC v2 CMYK printer profile, Lab PCS, one `mft2` A2B table registered under each tag in `tables`
/// (`A2B0` perceptual, `A2B1` colorimetric, `A2B2` saturation).
pub fn cmyk_profile(tables: &[&[u8; 4]]) -> Vec<u8> {
    let grid = 2usize;
    let mut clut = Vec::new();
    for c in 0..grid {
        for m in 0..grid {
            for y in 0..grid {
                for k in 0..grid {
                    let (l, a, b) = printer_lab(c as f64, m as f64, y as f64, k as f64);
                    clut.extend_from_slice(&lab16(l, a, b));
                }
            }
        }
    }
    let table = mft2(4, 3, grid as u8, &clut);
    let blobs = vec![xyz_tag(D50), table];
    let mut tags: Vec<(&[u8; 4], usize)> = vec![(b"wtpt", 0)];
    for sig in tables {
        tags.push((sig, 1));
    }
    assemble(0x0210_0000, b"prtr", b"CMYK", b"Lab ", &blobs, &tags)
}
/// ICC v2 gray profile with a `kTRC` gamma curve (use an exactly representable gamma such as 2.0).
pub fn gray_profile(gamma: f64) -> Vec<u8> {
    assemble(0x0210_0000, b"mntr", b"GRAY", b"XYZ ", &[xyz_tag(D50), curv(gamma)], &[(b"wtpt", 0), (b"kTRC", 1)])
}
/// `curv` with a sampled table (linear interpolation between the entries, each `v / 65535`).
pub fn curv_table(entries: &[u16]) -> Vec<u8> {
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend_from_slice(&be32(entries.len() as u32));
    for e in entries {
        t.extend_from_slice(&e.to_be_bytes());
    }
    t
}
/// ICC v2 gray profile whose `kTRC` is a sampled table, so `TRC(0) = entries[0] / 65535` can be non-zero:
/// a gray profile's black is not zero by definition.
pub fn gray_profile_table(entries: &[u16]) -> Vec<u8> {
    assemble(0x0210_0000, b"mntr", b"GRAY", b"XYZ ", &[xyz_tag(D50), curv_table(entries)], &[(b"wtpt", 0), (b"kTRC", 1)])
}
/// ICC RGB matrix/TRC profile with the sRGB primaries adapted to D50 and one `trc` blob (any `curv`) on
/// all three channels. `version` is the header word (`0x0210_0000` for v2.1, `0x0430_0000` for v4.3).
pub fn rgb_profile(version: u32, trc: &[u8]) -> Vec<u8> {
    rgb_profile_media(version, b"mntr", trc, D50)
}
/// Media white `wtpt` of an ICC profile whose colorants are D50-adapted but whose measured white is D65 (the
/// value an ICC v2 display profile may carry; `[0.9505, 1.0, 1.0891]` is D65 adapted to the D50 PCS scale).
pub const D65_ADAPTED: [f64; 3] = [0.9505, 1.0, 1.0891];
/// [`rgb_profile`] with an explicit profile class and `wtpt` (the media white that absolute colorimetric scales by).
pub fn rgb_profile_media(version: u32, class: &[u8; 4], trc: &[u8], white: [f64; 3]) -> Vec<u8> {
    let blobs = vec![
        xyz_tag(white),
        xyz_tag([0.4360747, 0.2225045, 0.0139322]),
        xyz_tag([0.3850649, 0.7168786, 0.0971045]),
        xyz_tag([0.1430804, 0.0606169, 0.7141733]),
        trc.to_vec(),
        trc.to_vec(),
        trc.to_vec(),
    ];
    assemble(
        version,
        class,
        b"RGB ",
        b"XYZ ",
        &blobs,
        &[(b"wtpt", 0), (b"rXYZ", 1), (b"gXYZ", 2), (b"bXYZ", 3), (b"rTRC", 4), (b"gTRC", 5), (b"bTRC", 6)],
    )
}

/// Y of a neutral with CIELAB lightness `l`.
pub fn y_of_l(l: f64) -> f64 {
    if l > 8.0 { ((l + 16.0) / 116.0).powi(3) } else { l * ((8.0 + 16.0) / 116.0_f64).powi(3) / 8.0 }
}
/// CIELAB (D50) to unclamped linear sRGB through XYZ (Bruce Lindbloom's D50 inverse matrix).
pub fn lab_to_linear_srgb(l: f64, a: f64, b: f64) -> [f64; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f64| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f64 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    let xyz = [D50[0] * inv(fx), D50[1] * inv(fy), D50[2] * inv(fz)];
    let m = [[3.1338561, -1.6168667, -0.4906146], [-0.9787684, 1.9161415, 0.0334540], [0.0719453, -0.2289914, 1.4052427]];
    m.map(|r| r[0] * xyz[0] + r[1] * xyz[1] + r[2] * xyz[2])
}
