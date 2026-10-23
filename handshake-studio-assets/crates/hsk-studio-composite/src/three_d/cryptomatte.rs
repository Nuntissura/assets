//! Cryptomatte (lane SMB-AE-3D-3; STU-CMP-050 "Cryptomatte: Builds anti-aliased, overlapping-aware
//! mattes from hashed-ID channels by picking objects by name").
//!
//! Source: the public Cryptomatte specification v1.2.0 (Psyop, repository `Psyop/Cryptomatte`,
//! BSD-3-Clause, `specification/cryptomatte_specification.pdf`); no reference implementation was
//! read. The MurmurHash3_x86_32 below is written from the public algorithm (Austin Appleby,
//! placed in the public domain) and checked against the specification's worked examples (test
//! `cryptomatte_hashes_exr_and_mattes_match_the_specification` in composite `three_d_adapter`).
//!
//! - ID: the UTF-8 bytes of a name hashed by MurmurHash3_x86_32 with seed 0 ([`murmur3_32`]); the
//!   32-bit hash becomes a float bit for bit, except that an exponent of 0 or 255 has bit 23
//!   toggled so no ID is a subnormal, zero, infinity or NaN ([`hash_to_float`]). "No object" is 0.
//! - Ranks: per pixel, the [`super::coverage::pixel_coverage`] entries (the ranking pixel filter,
//!   transparency included) are merged by ID (names that collide or repeat combine, as the
//!   specification wants), ranked by coverage (ties: the smaller ID bits first, Studio reading)
//!   and the first `levels` (even, 2..=[`MAX_CRYPTO_LEVELS`]; the specification suggests 6) are
//!   kept; missing ranks are ID 0, coverage 0.
//! - Channels: `{typename}NN.R` = ID of rank 2 NN, `.G` its coverage, `.B` = ID of rank 2 NN + 1,
//!   `.A` its coverage. The specification lists the members as `.r .g .b .a`; the upper-case
//!   OpenEXR channel names are a Studio reading (UNVERIFIED against a Cryptomatte decoder, D5).
//!   The deprecated preview channels are not written.
//! - Metadata (EXR `string` attributes): `cryptomatte/{key}/name` = the type name,
//!   `.../hash` = `MurmurHash3_32`, `.../conversion` = `uint32_to_float32`, `.../manifest` = a JSON
//!   object of name -> the 8 hex digits of the ID float's bits; `{key}` = the first 7 hex digits of
//!   the type name's hash (the specification's suggested key).
//! - Matte extraction ([`Cryptomatte::matte`], [`matte_from_ranks`]): per pixel, the sum of the
//!   coverages of every rank whose ID bits equal one of the picked names' IDs.
//!
//! Hostile input is refused typed ([`CRYPTO_PARAMETER`]) before allocation: an empty or
//! non-printable type name, a type name longer than [`MAX_TYPE_NAME_BYTES`], odd or out-of-range
//! `levels`, more than [`MAX_CRYPTO_NAMES`] distinct names, a name longer than
//! [`MAX_NAME_BYTES`], a manifest above [`super::exr::MAX_EXR_ATTRIBUTE_BYTES`], a name list that
//! does not match the planes, mismatched rank planes.
#![deny(clippy::arithmetic_side_effects)]

use super::coverage::PixelCoverage;
use std::collections::BTreeMap;

/// A Cryptomatte parameter the contract refuses (module docs).
pub const CRYPTO_PARAMETER: &str = "composition_3d_cryptomatte_parameter";
/// Most ID ranks per pixel (8 EXR layers).
pub const MAX_CRYPTO_LEVELS: usize = 16;
/// Most distinct names in one Cryptomatte type.
pub const MAX_CRYPTO_NAMES: usize = 65_536;
/// Longest name (bytes).
pub const MAX_NAME_BYTES: usize = 4096;
/// Longest type name (bytes): `{typename}NN.R` must fit OpenEXR's 31-byte channel names.
pub const MAX_TYPE_NAME_BYTES: usize = 26;
/// The specification's suggested number of ranks.
pub const DEFAULT_LEVELS: usize = 6;

/// MurmurHash3_x86_32 of `data` with `seed`.
pub fn murmur3_32(data: &[u8], seed: u32) -> u32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;
    let mix = |mut k: u32| -> u32 {
        k = k.wrapping_mul(C1);
        k = k.rotate_left(15);
        k.wrapping_mul(C2)
    };
    let mut h = seed;
    let mut blocks = data.chunks_exact(4);
    for b in &mut blocks {
        h ^= mix(u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        h = h.rotate_left(13).wrapping_mul(5).wrapping_add(0xe654_6b64);
    }
    let tail = blocks.remainder();
    if !tail.is_empty() {
        let mut k = 0u32;
        for (i, byte) in tail.iter().enumerate() {
            k ^= u32::from(*byte).wrapping_shl(u32::try_from(i & 3).unwrap_or(0).wrapping_mul(8));
        }
        h ^= mix(k);
    }
    // The length enters modulo 2^32, as in the reference algorithm.
    #[allow(clippy::cast_possible_truncation)]
    {
        h ^= data.len() as u32;
    }
    h ^= h.wrapping_shr(16);
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h.wrapping_shr(13);
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^ h.wrapping_shr(16)
}

/// The specification's `uint32_to_float32` conversion: the hash's bits as a float, bit 23 toggled
/// when the exponent is 0 or 255.
pub fn hash_to_float(hash: u32) -> f32 {
    let exponent = hash.wrapping_shr(23) & 255;
    let bits = if exponent == 0 || exponent == 255 { hash ^ 0x0080_0000 } else { hash };
    f32::from_bits(bits)
}

/// The Cryptomatte ID of `name`.
pub fn name_id(name: &str) -> f32 {
    hash_to_float(murmur3_32(name.as_bytes(), 0))
}

/// The metadata key of a type name: the first 7 hex digits of its hash.
pub fn type_key(typename: &str) -> String {
    let mut k = format!("{:08x}", murmur3_32(typename.as_bytes(), 0));
    k.truncate(7);
    k
}

/// One Cryptomatte type of one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Cryptomatte {
    pub typename: String,
    pub width: u32,
    pub height: u32,
    /// Per rank (rank 0 first): the ID plane and the coverage plane, row-major.
    pub ranks: Vec<(Vec<f32>, Vec<f32>)>,
    /// Name -> ID bits, every name of the type (whether or not it is visible in this frame).
    pub manifest: BTreeMap<String, u32>,
}

fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

impl Cryptomatte {
    /// The manifest as the specification's JSON object (names sorted by their UTF-8 bytes).
    pub fn manifest_json(&self) -> String {
        let mut out = String::from("{");
        for (i, (name, bits)) in self.manifest.iter().enumerate() {
            if i != 0 {
                out.push(',');
            }
            json_string(&mut out, name);
            out.push(':');
            out.push_str(&format!("\"{bits:08x}\""));
        }
        out.push('}');
        out
    }
    /// The four metadata entries (attribute name, value) of this type.
    pub fn metadata(&self) -> Vec<(String, String)> {
        let key = type_key(&self.typename);
        vec![
            (format!("cryptomatte/{key}/conversion"), "uint32_to_float32".to_owned()),
            (format!("cryptomatte/{key}/hash"), "MurmurHash3_32".to_owned()),
            (format!("cryptomatte/{key}/manifest"), self.manifest_json()),
            (format!("cryptomatte/{key}/name"), self.typename.clone()),
        ]
    }
    /// `(channel name, plane)` of every rank pair (module docs).
    pub fn channels(&self) -> Vec<(String, &[f32])> {
        let mut out = Vec::with_capacity(self.ranks.len().saturating_mul(2));
        for (k, pair) in self.ranks.chunks(2).enumerate() {
            let suffixes: [(&str, Option<&[f32]>); 4] = [
                ("R", pair.first().map(|r| &r.0[..])),
                ("G", pair.first().map(|r| &r.1[..])),
                ("B", pair.get(1).map(|r| &r.0[..])),
                ("A", pair.get(1).map(|r| &r.1[..])),
            ];
            for (s, plane) in suffixes {
                if let Some(p) = plane {
                    out.push((format!("{}{k:02}.{s}", self.typename), p));
                }
            }
        }
        out
    }
    /// The matte of the objects called `names` (module docs).
    pub fn matte(&self, names: &[&str]) -> Vec<f32> {
        let ranks: Vec<(&[f32], &[f32])> = self.ranks.iter().map(|r| (&r.0[..], &r.1[..])).collect();
        matte_from_ranks(&ranks, names).unwrap_or_default()
    }
}

/// Every type of `types` (with their metadata) as one OpenEXR file.
pub fn to_exr(types: &[Cryptomatte]) -> Result<Vec<u8>, &'static str> {
    let (Some(first), true) = (types.first(), types.iter().all(|t| types.first().is_some_and(|f| f.width == t.width && f.height == t.height))) else {
        return Err(CRYPTO_PARAMETER);
    };
    let names: Vec<(String, &[f32])> = types.iter().flat_map(Cryptomatte::channels).collect();
    let channels: Vec<(&str, &[f32])> = names.iter().map(|(n, p)| (n.as_str(), *p)).collect();
    let meta: Vec<(String, String)> = types.iter().flat_map(Cryptomatte::metadata).collect();
    let strings: Vec<(&str, &str)> = meta.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    super::exr::encode_exr_f32_with_attributes(first.width, first.height, &channels, &strings)
}

/// The matte of `names` from rank planes `(id, coverage)` (as read back from a file): per pixel the
/// sum of the coverages whose ID bits equal a picked name's ID, clamped to `0..=1`. Refused: rank
/// planes of different lengths, a non-finite coverage.
pub fn matte_from_ranks(ranks: &[(&[f32], &[f32])], names: &[&str]) -> Result<Vec<f32>, &'static str> {
    let n = ranks.first().map_or(0, |r| r.0.len());
    if ranks.iter().any(|r| r.0.len() != n || r.1.len() != n || r.1.iter().any(|c| !c.is_finite())) {
        return Err(CRYPTO_PARAMETER);
    }
    let wanted: Vec<u32> = names.iter().map(|s| name_id(s).to_bits()).collect();
    let mut out = vec![0.0f32; n];
    for (id, cov) in ranks {
        for ((o, i), c) in out.iter_mut().zip(id.iter()).zip(cov.iter()) {
            if i.to_bits() != 0 && wanted.contains(&i.to_bits()) {
                *o += *c;
            }
        }
    }
    for o in &mut out {
        *o = o.clamp(0.0, 1.0);
    }
    Ok(out)
}

/// Builds one Cryptomatte type from the ranking filter: `names[i]` names plane `i` of the planes
/// [`super::coverage::pixel_coverage`] ran on (module docs).
pub fn build(coverage: &PixelCoverage, names: &[String], typename: &str, levels: usize) -> Result<Cryptomatte, &'static str> {
    let type_ok = !typename.is_empty() && typename.len() <= MAX_TYPE_NAME_BYTES && typename.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'.');
    if !type_ok || !(2..=MAX_CRYPTO_LEVELS).contains(&levels) || levels & 1 != 0 {
        return Err(CRYPTO_PARAMETER);
    }
    if names.iter().any(|s| s.len() > MAX_NAME_BYTES) {
        return Err(CRYPTO_PARAMETER);
    }
    let mut manifest: BTreeMap<String, u32> = BTreeMap::new();
    let mut manifest_bytes: usize = 2;
    for s in names {
        if manifest.contains_key(s) {
            continue;
        }
        manifest_bytes = manifest_bytes.checked_add(s.len().checked_mul(6).and_then(|b| b.checked_add(14)).ok_or(CRYPTO_PARAMETER)?).ok_or(CRYPTO_PARAMETER)?;
        if manifest.len() >= MAX_CRYPTO_NAMES || manifest_bytes > super::exr::MAX_EXR_ATTRIBUTE_BYTES {
            return Err(CRYPTO_PARAMETER);
        }
        manifest.insert(s.clone(), name_id(s).to_bits());
    }
    let plane_ids: Vec<u32> = names.iter().map(|s| name_id(s).to_bits()).collect();
    let px = coverage.pixels();
    let expected = usize::try_from(u64::from(coverage.width).checked_mul(u64::from(coverage.height)).ok_or(CRYPTO_PARAMETER)?).map_err(|_| CRYPTO_PARAMETER)?;
    if px != expected || coverage.entries.iter().any(|e| usize::try_from(e.plane).map_or(true, |p| p >= names.len())) {
        return Err(CRYPTO_PARAMETER);
    }
    let mut ranks: Vec<(Vec<f32>, Vec<f32>)> = (0..levels).map(|_| (vec![0.0f32; px], vec![0.0f32; px])).collect();
    let mut merged: Vec<(u32, f64)> = Vec::new();
    for p in 0..px {
        merged.clear();
        for e in coverage.pixel(p) {
            let id = usize::try_from(e.plane).ok().and_then(|i| plane_ids.get(i)).copied().unwrap_or(0);
            match merged.iter_mut().find(|m| m.0 == id) {
                Some(m) => m.1 += f64::from(e.coverage),
                None => merged.push((id, f64::from(e.coverage))),
            }
        }
        merged.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        for (rank, (id, cov)) in ranks.iter_mut().zip(&merged) {
            if let (Some(i), Some(c)) = (rank.0.get_mut(p), rank.1.get_mut(p)) {
                *i = f32::from_bits(*id);
                #[allow(clippy::cast_possible_truncation)]
                {
                    *c = *cov as f32;
                }
            }
        }
    }
    Ok(Cryptomatte { typename: typename.to_owned(), width: coverage.width, height: coverage.height, ranks, manifest })
}
