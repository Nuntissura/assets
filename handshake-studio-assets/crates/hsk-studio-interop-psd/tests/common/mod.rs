//! Shared byte-level PSD assembly for the contract tests (independent of the crate's encoder).
#![allow(dead_code)]

use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_psd::*;

pub fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(body));
        let _ = sender.send(outcome);
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("case {name} exceeded declared 30-second deadline"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("case {name} lost deadline worker result"),
    }
}

// ---- independent byte assembly -------------------------------------------------------------

pub fn be16(v: u16) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}
pub fn be32(v: u32) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}
pub fn be64(v: u64) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}
pub fn len_field(psb_wide: bool, n: usize) -> Vec<u8> {
    if psb_wide { be64(n as u64) } else { be32(n as u32) }
}
pub fn pad_to(mut v: Vec<u8>, multiple: usize) -> Vec<u8> {
    while !v.len().is_multiple_of(multiple) {
        v.push(0);
    }
    v
}
pub fn unicode(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut v = be32(units.len() as u32);
    for u in units {
        v.extend(be16(u));
    }
    v
}
pub fn packbits(row: &[u8]) -> Vec<u8> {
    if !row.is_empty() && row.iter().all(|&b| b == row[0]) && row.len() <= 128 && row.len() >= 2 {
        return vec![(257 - row.len()) as u8, row[0]];
    }
    let mut v = Vec::new();
    for chunk in row.chunks(128) {
        v.push((chunk.len() - 1) as u8);
        v.extend_from_slice(chunk);
    }
    v
}
/// RLE payload (counts then rows) for rows of one or more planes, PSD (2-byte) or PSB (4-byte) counts.
pub fn rle(rows: &[&[u8]], psb: bool) -> Vec<u8> {
    let packed: Vec<Vec<u8>> = rows.iter().map(|r| packbits(r)).collect();
    let mut v = Vec::new();
    for p in &packed {
        v.extend(if psb { be32(p.len() as u32) } else { be16(p.len() as u16) });
    }
    for p in &packed {
        v.extend_from_slice(p);
    }
    v
}
pub fn zlib(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}
pub fn resource(id: u16, name: &str, data: &[u8]) -> Vec<u8> {
    let mut v = b"8BIM".to_vec();
    v.extend(be16(id));
    let mut pascal = vec![name.len() as u8];
    pascal.extend_from_slice(name.as_bytes());
    v.extend(pad_to(pascal, 2));
    v.extend(be32(data.len() as u32));
    v.extend_from_slice(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}
/// Layer-level block: the length field already includes alignment to 4.
pub fn layer_block(key: &[u8; 4], data: &[u8], psb: bool) -> Vec<u8> {
    let data = pad_to(data.to_vec(), 4);
    let mut v = b"8BIM".to_vec();
    v.extend_from_slice(key);
    v.extend(len_field(psb && matches!(key, b"Lr16" | b"Layr" | b"LMsk"), data.len()));
    v.extend(data);
    v
}
/// File-level block: the length field excludes the alignment to 4.
pub fn global_block(key: &[u8; 4], data: &[u8], psb: bool) -> Vec<u8> {
    let mut v = b"8BIM".to_vec();
    v.extend_from_slice(key);
    v.extend(len_field(psb && matches!(key, b"Lr16" | b"Layr" | b"LMsk"), data.len()));
    v.extend_from_slice(data);
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
    v
}

#[derive(Clone)]
pub struct TLayer {
    pub name: &'static str,
    pub rect: [i32; 4],
    pub channels: Vec<(i16, u16, Vec<u8>)>,
    pub key: [u8; 4],
    pub opacity: u8,
    pub clipping: u8,
    pub flags: u8,
    pub mask: Vec<u8>,
    pub ranges: Vec<u8>,
    pub blocks: Vec<Vec<u8>>,
}
impl TLayer {
    pub fn new(name: &'static str, rect: [i32; 4]) -> Self {
        Self {
            name,
            rect,
            channels: Vec::new(),
            key: *b"norm",
            opacity: 255,
            clipping: 0,
            flags: 0x08,
            mask: Vec::new(),
            ranges: Vec::new(),
            blocks: Vec::new(),
        }
    }
    pub fn record(&self, psb: bool) -> Vec<u8> {
        let mut v = Vec::new();
        for r in self.rect {
            v.extend(r.to_be_bytes());
        }
        v.extend(be16(self.channels.len() as u16));
        for (id, _, payload) in &self.channels {
            v.extend(id.to_be_bytes());
            v.extend(len_field(psb, 2 + payload.len()));
        }
        v.extend_from_slice(b"8BIM");
        v.extend_from_slice(&self.key);
        v.extend([self.opacity, self.clipping, self.flags, 0]);
        let mut extra = be32(self.mask.len() as u32);
        extra.extend_from_slice(&self.mask);
        extra.extend(be32(self.ranges.len() as u32));
        extra.extend_from_slice(&self.ranges);
        let mut pascal = vec![self.name.len() as u8];
        pascal.extend_from_slice(self.name.as_bytes());
        extra.extend(pad_to(pascal, 4));
        for b in &self.blocks {
            extra.extend_from_slice(b);
        }
        v.extend(be32(extra.len() as u32));
        v.extend(extra);
        v
    }
    pub fn channel_data(&self) -> Vec<u8> {
        let mut v = Vec::new();
        for (_, comp, payload) in &self.channels {
            v.extend(be16(*comp));
            v.extend_from_slice(payload);
        }
        v
    }
}
pub fn layer_info(layers: &[TLayer], psb: bool, negative_count: bool) -> Vec<u8> {
    let n = layers.len() as i16;
    let mut v = (if negative_count { -n } else { n }).to_be_bytes().to_vec();
    for l in layers {
        v.extend(l.record(psb));
    }
    for l in layers {
        v.extend(l.channel_data());
    }
    pad_to(v, 2)
}
/// Layer and mask information section: layer info, empty global mask, global blocks.
pub fn layer_section(info: &[u8], blocks: &[Vec<u8>], psb: bool) -> Vec<u8> {
    let mut v = len_field(psb, info.len());
    v.extend_from_slice(info);
    v.extend(be32(0));
    for b in blocks {
        v.extend_from_slice(b);
    }
    v
}
pub struct Doc {
    pub version: u16,
    pub channels: u16,
    pub height: u32,
    pub width: u32,
    pub depth: u16,
    pub mode: u16,
    pub color_mode_data: Vec<u8>,
    pub resources: Vec<u8>,
    pub layer_section: Vec<u8>,
    pub image: Option<(u16, Vec<u8>)>,
}
impl Doc {
    pub fn bytes(&self) -> Vec<u8> {
        let mut v = b"8BPS".to_vec();
        v.extend(be16(self.version));
        v.extend([0; 6]);
        v.extend(be16(self.channels));
        v.extend(be32(self.height));
        v.extend(be32(self.width));
        v.extend(be16(self.depth));
        v.extend(be16(self.mode));
        v.extend(be32(self.color_mode_data.len() as u32));
        v.extend_from_slice(&self.color_mode_data);
        v.extend(be32(self.resources.len() as u32));
        v.extend_from_slice(&self.resources);
        v.extend(len_field(self.version == 2, self.layer_section.len()));
        v.extend_from_slice(&self.layer_section);
        if let Some((comp, data)) = &self.image {
            v.extend(be16(*comp));
            v.extend_from_slice(data);
        }
        v
    }
}

pub fn empty_channels(ids: &[i16]) -> Vec<(i16, u16, Vec<u8>)> {
    ids.iter().map(|&id| (id, 0, Vec::new())).collect()
}


pub fn read(bytes: &[u8]) -> PsdDocument {
    read_psd(bytes, &Limits::default(), &CancellationToken::default()).expect("fixture parses")
}
