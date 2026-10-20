//! The embedded preview (`Thmb` block) and resource blobs as content-addressed Studio assets.
//!
//! `Thmb` block layout, from the lane's hexdumps of 13 files (9 AFDesignLoad MIT V1 designs, 4 installed
//! Canva V2/V3 fixtures; field roles UNVERIFIED beyond what decoding needs), at the header's thumbnail
//! offset: `FF FF FF FF`, `Thmb`, u32 1, u32 block length (bytes after these 16), u32 image header length
//! (29 = offset of the image from the block start), u32 0, u32 image length, u8 1, image bytes. afread
//! only detects the marker; the afphoto crate (MIT) scans for a PNG signature. A block whose fields do
//! not fit is searched for an image signature in its first 64 bytes, and the report says so.

use crate::container::{Cur, KaContainer};
use crate::error::{AffinityError, Result};
use hsk_studio_accord::CancellationToken;
use sha2::{Digest, Sha256};

/// What an embedded image or resource blob is, by its leading bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobKind {
    Png,
    Jpeg,
    Tiff,
    Gif,
    Webp,
    /// TrueType / OpenType (`00 01 00 00`, `true`, `OTTO`), collection (`ttcf`), WOFF, WOFF2.
    Font,
    Pdf,
    /// A nested KA container (embedded Affinity document or resource).
    KaContainer,
    /// A `00 FF 4B 53` tagged object stream (not decoded).
    ObjectStream,
    /// Anything else (raster tile data, private records): carried, not interpreted.
    Opaque,
}

impl BlobKind {
    pub fn sniff(b: &[u8]) -> Self {
        let at = |m: &[u8]| b.starts_with(m);
        if at(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            Self::Png
        } else if at(&[0xFF, 0xD8, 0xFF]) {
            Self::Jpeg
        } else if at(b"II*\0") || at(b"MM\0*") || at(b"II+\0") || at(b"MM\0+") {
            Self::Tiff
        } else if at(b"GIF87a") || at(b"GIF89a") {
            Self::Gif
        } else if at(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
            Self::Webp
        } else if at(&[0, 1, 0, 0]) || at(b"true") || at(b"OTTO") || at(b"ttcf") || at(b"wOFF") || at(b"wOF2") {
            Self::Font
        } else if at(b"%PDF-") {
            Self::Pdf
        } else if at(&[0x00, 0xFF, 0x4B, 0x41]) {
            Self::KaContainer
        } else if at(&[0x00, 0xFF, 0x4B, 0x53]) {
            Self::ObjectStream
        } else {
            Self::Opaque
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Tiff => "tiff",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Font => "font",
            Self::Pdf => "pdf",
            Self::KaContainer => "ka_container",
            Self::ObjectStream => "object_stream",
            Self::Opaque => "opaque",
        }
    }

    /// Images and fonts are exposed as standalone assets; the rest is carried in the original.
    pub fn is_asset(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg | Self::Tiff | Self::Gif | Self::Webp | Self::Font | Self::Pdf)
    }
}

/// The embedded preview image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    pub kind: BlobKind,
    /// From PNG IHDR or the JPEG SOF; `None` if not read.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// File offset of the image bytes.
    pub offset: u64,
    /// `structured` (block fields) or `scan` (signature search in the first 64 block bytes).
    pub located_by: &'static str,
    pub sha256: [u8; 32],
    pub bytes: Vec<u8>,
}

/// One decoded head stream exposed as a content-addressed asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceBlob {
    pub stream: String,
    pub id: u32,
    pub kind: BlobKind,
    pub sha256: [u8; 32],
    pub bytes: Vec<u8>,
}

/// Every head stream with its kind; `bytes` filled only for asset kinds (images, fonts, PDF).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resources {
    pub assets: Vec<ResourceBlob>,
    /// (stream name, kind, inflated size) of streams that are carried but not exposed: `doc.dat` (the
    /// object graph, not decoded), raster tile data, nested containers.
    pub carried: Vec<(String, BlobKind, u64)>,
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// A structurally complete PNG at the start of `b`: (length through IEND, width, height).
fn png_extent(b: &[u8]) -> Option<(usize, u32, u32)> {
    if !b.starts_with(&PNG_SIG) {
        return None;
    }
    let mut c = Cur { bytes: b, pos: 8 };
    let mut dims = None;
    loop {
        let len = c.take(4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))?;
        let ty = c.take(4)?;
        let data = c.take(usize::try_from(len).ok()?)?;
        c.take(4)?; // CRC
        if dims.is_none() {
            if ty != b"IHDR" || data.len() != 13 {
                return None;
            }
            let w = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            let h = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            dims = Some((w, h));
        }
        if ty == b"IEND" {
            let (w, h) = dims?;
            return Some((c.pos, w, h));
        }
    }
}

/// The image located by the block fields: image header length (from the block start), a zero word,
/// the image length; `None` when any of them does not fit the block.
fn structured_image(block: &[u8], start: usize, fields: usize) -> Option<(usize, &[u8])> {
    let mut f = Cur { bytes: block, pos: fields };
    let header_len = usize::try_from(f.u32()?).ok()?;
    let _zero = f.u32()?;
    let image_len = usize::try_from(f.u32()?).ok()?;
    let image_at = start.checked_add(header_len)?;
    let image = block.get(image_at..image_at.checked_add(image_len)?)?;
    Some((image_at, image))
}

impl KaContainer<'_> {
    /// The embedded preview, validated (PNG: complete chunk walk from IHDR to IEND inside the block;
    /// JPEG: SOI and an end-of-image marker at the block end). `ThumbnailAbsent` when there is no
    /// `FF FF FF FF Thmb` block at the header offset.
    pub fn thumbnail(&self) -> Result<Thumbnail> {
        let bytes = self.bytes;
        let mut c = Cur::at(bytes, self.header.thumbnail_offset).ok_or(AffinityError::ThumbnailAbsent)?;
        if c.take(4) != Some(&[0xFF; 4][..]) || c.take(4) != Some(&b"Thmb"[..]) {
            return Err(AffinityError::ThumbnailAbsent);
        }
        let start = c.pos.checked_sub(8).ok_or(AffinityError::ThumbnailInvalid)?;
        let _block_version = c.u32().ok_or(AffinityError::ThumbnailInvalid)?;
        let block_len = c.u32().ok_or(AffinityError::ThumbnailInvalid)?;
        let block_end = usize::try_from(block_len).ok().and_then(|l| c.pos.checked_add(l)).filter(|&e| e <= bytes.len());
        let block_end = block_end.ok_or(AffinityError::ThumbnailInvalid)?;
        let block = bytes.get(..block_end).ok_or(AffinityError::ThumbnailInvalid)?;
        let structured = structured_image(block, start, c.pos);
        let (image_at, image, located_by) = match structured {
            Some((at, image)) if BlobKind::sniff(image) != BlobKind::Opaque => (at, image, "structured"),
            _ => {
                let window = block.get(start..start.saturating_add(64).min(block_end)).ok_or(AffinityError::ThumbnailInvalid)?;
                let rel = (0..window.len())
                    .find(|&i| window.get(i..).is_some_and(|w| w.starts_with(&PNG_SIG) || w.starts_with(&[0xFF, 0xD8, 0xFF])))
                    .ok_or(AffinityError::ThumbnailInvalid)?;
                let at = start.checked_add(rel).ok_or(AffinityError::ThumbnailInvalid)?;
                (at, block.get(at..).ok_or(AffinityError::ThumbnailInvalid)?, "scan")
            }
        };
        let kind = BlobKind::sniff(image);
        let (image, width, height) = match kind {
            BlobKind::Png => {
                let (len, w, h) = png_extent(image).ok_or(AffinityError::ThumbnailImageInvalid)?;
                (image.get(..len).ok_or(AffinityError::ThumbnailImageInvalid)?, Some(w), Some(h))
            }
            BlobKind::Jpeg if image.ends_with(&[0xFF, 0xD9]) => (image, None, None),
            _ => return Err(AffinityError::ThumbnailImageInvalid),
        };
        Ok(Thumbnail {
            kind,
            width,
            height,
            offset: u64::try_from(image_at).map_err(|_| AffinityError::ThumbnailInvalid)?,
            located_by,
            sha256: sha256(image),
            bytes: image.to_vec(),
        })
    }

    /// Decodes every head stream (bounded by [`crate::Limits::max_total_bytes`] in total, CRC-checked)
    /// and exposes images, fonts and PDFs as content-addressed assets; everything else is listed as
    /// carried. `doc.dat` is decoded and CRC-checked too but never interpreted (object graph not decoded).
    pub fn resources(&self, cancel: &CancellationToken) -> Result<Resources> {
        let mut total: u64 = 0;
        let mut out = Resources { assets: Vec::new(), carried: Vec::new() };
        for s in self.streams() {
            total = total.checked_add(s.entry.size).filter(|&t| t <= self.limits.max_total_bytes).ok_or(AffinityError::TotalBudget)?;
            let bytes = self.read_entry(&s.entry, cancel)?;
            let kind = BlobKind::sniff(&bytes);
            if kind.is_asset() && s.name != "doc.dat" {
                out.assets.push(ResourceBlob { stream: s.name, id: s.entry.id, kind, sha256: sha256(&bytes), bytes });
            } else {
                out.carried.push((s.name, kind, s.entry.size));
            }
        }
        Ok(out)
    }
}
