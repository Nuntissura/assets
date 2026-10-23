//! Synthetic KA containers built from the documented layout (afread MIT field order + the lane's
//! hexdump facts). Test data only: these bytes were never opened in Affinity.
#![allow(dead_code)]

pub const STORED: u8 = 0;
pub const ZLIB: u8 = 1;
pub const ZSTD: u8 = 2;

#[derive(Clone)]
pub struct Spec {
    pub id: u32,
    /// `None` = state 1 (unnamed revision).
    pub name: Option<&'static str>,
    pub deleted: bool,
    /// Decoded bytes.
    pub data: Vec<u8>,
    /// Compression byte for `#FT2+` directories (codec in bits 0-1, predictor in 6-7); for `#FAT` the
    /// legacy code is derived (codec 1 -> 1, predictor class k -> 1 + k).
    pub compression: u8,
    /// Encode `data` with the byte-delta predictor first (class 1).
    pub byte_delta: bool,
}

impl Spec {
    pub fn new(id: u32, name: &'static str, data: Vec<u8>, compression: u8) -> Self {
        Self { id, name: Some(name), deleted: false, data, compression, byte_delta: false }
    }
}

pub struct Revision {
    pub created: u64,
    pub streams: Vec<Spec>,
}

pub struct Build {
    pub version: u16,
    pub flags: u16,
    pub class: [u8; 4],
    pub dir_tag: [u8; 4],
    pub revisions: Vec<Revision>,
    pub folders: Vec<(&'static str, u64)>,
    /// Raw image placed in a `Thmb` block after the last directory.
    pub thumbnail: Option<Vec<u8>>,
}

impl Build {
    pub fn doc(version: u16, dir_tag: &[u8; 4], streams: Vec<Spec>) -> Self {
        Self {
            version,
            flags: 0,
            class: *b"Prsn",
            dir_tag: *dir_tag,
            revisions: vec![Revision { created: 1_760_000_000, streams }],
            folders: Vec::new(),
            thumbnail: None,
        }
    }
}

pub fn encode(data: &[u8], codec: u8) -> Vec<u8> {
    match codec {
        ZLIB => miniz_oxide::deflate::compress_to_vec_zlib(data, 6),
        ZSTD => ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest),
        _ => data.to_vec(),
    }
}

/// Byte-delta forward transform (inverse of the class-1 predictor).
pub fn delta(data: &[u8]) -> Vec<u8> {
    let mut prev = 0u8;
    data.iter()
        .map(|&v| {
            let d = v.wrapping_sub(prev);
            prev = v;
            d
        })
        .collect()
}

pub fn build(b: &Build) -> Vec<u8> {
    let mut out = vec![0u8; 0x48];
    out[..4].copy_from_slice(&[0x00, 0xFF, 0x4B, 0x41]);
    out[4..6].copy_from_slice(&b.version.to_le_bytes());
    out[6..8].copy_from_slice(&b.flags.to_le_bytes());
    out[8..12].copy_from_slice(&[b.class[3], b.class[2], b.class[1], b.class[0]]);
    out[12..16].copy_from_slice(b"#Inf");
    out[0x30..0x38].copy_from_slice(&b.revisions[0].created.to_le_bytes());
    out[0x38..0x3C].copy_from_slice(&2u32.to_le_bytes());
    out[0x40..0x44].copy_from_slice(b"Prot");
    // Stream payloads: (offset, stored size) per revision and spec.
    let mut placed: Vec<Vec<(u64, u64)>> = Vec::new();
    for rev in &b.revisions {
        let mut row = Vec::new();
        for s in &rev.streams {
            if s.deleted {
                row.push((0, 0));
                continue;
            }
            let raw = if s.byte_delta { delta(&s.data) } else { s.data.clone() };
            let payload = encode(&raw, s.compression & 3);
            let at = out.len() as u64;
            out.extend_from_slice(b"#Fil");
            out.extend_from_slice(&payload);
            row.push((at, payload.len() as u64));
        }
        placed.push(row);
    }
    // Directories, newest last in the file, chained first -> next.
    let mut dir_offsets = Vec::new();
    let mut dirs: Vec<Vec<u8>> = Vec::new();
    for (ri, rev) in b.revisions.iter().enumerate() {
        let mut d = Vec::new();
        d.extend_from_slice(&b.dir_tag);
        d.extend_from_slice(&0u64.to_le_bytes()); // next, patched below
        d.extend_from_slice(&rev.created.to_le_bytes());
        d.extend_from_slice(&[0u8; 24]);
        d.extend_from_slice(&(rev.streams.len() as u32).to_le_bytes());
        d.extend_from_slice(&[0u8; 8]);
        let folders: &[(&str, u64)] = if ri == 0 { &b.folders } else { &[] };
        d.extend_from_slice(&(folders.len() as u16).to_le_bytes());
        d.push(0);
        for (si, s) in rev.streams.iter().enumerate() {
            d.extend_from_slice(&s.id.to_le_bytes());
            d.push(if s.deleted { 2 } else if s.name.is_some() { 0 } else { 1 });
            if !s.deleted {
                let (at, stored) = placed[ri][si];
                d.extend_from_slice(&at.to_le_bytes());
                d.extend_from_slice(&(s.data.len() as u64).to_le_bytes());
                d.extend_from_slice(&stored.to_le_bytes());
                d.extend_from_slice(&crc32fast::hash(&s.data).to_le_bytes());
                if &b.dir_tag == b"#FAT" || &b.dir_tag == b"#FT2" {
                    let legacy = if s.compression & 3 == ZLIB { 1 + (s.compression >> 6) } else { 0 };
                    d.push(legacy);
                } else {
                    d.push(s.compression);
                }
                if &b.dir_tag != b"#FAT" {
                    d.extend_from_slice(&20u32.to_le_bytes());
                }
                if &b.dir_tag == b"#FT4" {
                    d.extend_from_slice(&0xA5A5_0001u32.to_le_bytes());
                }
            }
            if let (Some(n), false) = (s.name, s.deleted) {
                d.extend_from_slice(&(n.len() as u16).to_le_bytes());
                d.extend_from_slice(n.as_bytes());
            }
        }
        for (name, count) in folders {
            d.extend_from_slice(&(name.len() as u16).to_le_bytes());
            d.extend_from_slice(&0u16.to_le_bytes());
            d.extend_from_slice(&count.to_le_bytes());
            d.extend_from_slice(name.as_bytes());
        }
        dir_offsets.push(out.len() as u64 + dirs.iter().map(|x| x.len() as u64).sum::<u64>());
        dirs.push(d);
    }
    for (i, d) in dirs.iter_mut().enumerate() {
        let next = dir_offsets.get(i + 1).copied().unwrap_or(0);
        d[4..12].copy_from_slice(&next.to_le_bytes());
    }
    for d in &dirs {
        out.extend_from_slice(d);
    }
    let thumb_at = out.len() as u64;
    out[0x10..0x18].copy_from_slice(&dir_offsets[0].to_le_bytes());
    out[0x18..0x20].copy_from_slice(&thumb_at.to_le_bytes());
    if let Some(img) = &b.thumbnail {
        out.extend_from_slice(&[0xFF; 4]);
        out.extend_from_slice(b"Thmb");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&((img.len() + 13) as u32).to_le_bytes());
        out.extend_from_slice(&29u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.push(1);
        out.extend_from_slice(img);
    }
    out
}

/// A structurally complete (not decodable) PNG: signature, IHDR, IDAT, IEND.
pub fn png_shell(w: u32, h: u32) -> Vec<u8> {
    let mut p = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let chunk = |p: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]| {
        p.extend_from_slice(&(data.len() as u32).to_be_bytes());
        p.extend_from_slice(ty);
        p.extend_from_slice(data);
        let mut c = crc32fast::Hasher::new();
        c.update(ty);
        c.update(data);
        p.extend_from_slice(&c.finalize().to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut p, b"IHDR", &ihdr);
    chunk(&mut p, b"IDAT", &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
    chunk(&mut p, b"IEND", &[]);
    p
}
