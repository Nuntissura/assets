//! Image resources section: a sequence of `8BIM` blocks. Every byte (names, data, padding,
//! unparsed tail) is kept so an unmodified section re-emits identically.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PsdError, Result};
use crate::limits::Limits;
use crate::reader::Reader;

pub const RESOURCE_ICC_PROFILE: u16 = 1039;
pub const RESOURCE_ICC_UNTAGGED: u16 = 1041;
pub const RESOURCE_VERSION_INFO: u16 = 1057;

const KNOWN_SIGNATURES: [&[u8; 4]; 5] = [b"8BIM", b"MeSa", b"AgHg", b"PHUT", b"DCSR"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageResource {
    pub signature: [u8; 4],
    pub id: u16,
    /// Pascal-string content (without the length byte), raw.
    pub name: Vec<u8>,
    pub name_pad: Vec<u8>,
    pub data: Vec<u8>,
    pub data_pad: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageResources {
    pub blocks: Vec<ImageResource>,
    /// Bytes after the last well-formed block (non-empty means the section was malformed there).
    pub trailing: Vec<u8>,
}

/// Resource 1057, "Version Info".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionInfo {
    pub version: u32,
    /// `hasRealMergedData`: false means the merged image is a placeholder, not an oracle.
    pub has_real_merged_data: bool,
    pub writer_name: Option<String>,
    pub reader_name: Option<String>,
    pub file_version: Option<u32>,
}

impl ImageResources {
    pub(crate) fn parse(bytes: &[u8], limits: &Limits) -> Result<Self> {
        let mut r = Reader::new(bytes);
        let mut blocks = Vec::new();
        loop {
            let at = r.clone();
            match Self::parse_block(&mut r) {
                Some(block) => {
                    if blocks.len() >= limits.max_resources {
                        return Err(PsdError::ResourceLimit);
                    }
                    blocks.push(block);
                }
                None => {
                    return Ok(Self { blocks, trailing: at.rest().to_vec() });
                }
            }
            if r.remaining() == 0 {
                return Ok(Self { blocks, trailing: Vec::new() });
            }
        }
    }

    fn parse_block(r: &mut Reader<'_>) -> Option<ImageResource> {
        let mut c = r.clone();
        let signature = c.array4("resource").ok()?;
        if !KNOWN_SIGNATURES.contains(&&signature) {
            return None;
        }
        let id = c.u16("resource").ok()?;
        let name_len = usize::from(c.u8("resource").ok()?);
        let name = c.bytes(name_len, "resource").ok()?.to_vec();
        let name_pad = c.skip_up_to((1 + name_len) % 2).to_vec();
        if name_pad.len() != (1 + name_len) % 2 {
            return None;
        }
        let size = usize::try_from(c.u32("resource").ok()?).ok()?;
        let data = c.bytes(size, "resource").ok()?.to_vec();
        let data_pad = c.skip_up_to(size % 2).to_vec();
        *r = c;
        Some(ImageResource { signature, id, name, name_pad, data, data_pad })
    }

    pub fn find(&self, id: u16) -> Option<&ImageResource> {
        self.blocks.iter().find(|b| b.id == id)
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        for b in &self.blocks {
            out.extend_from_slice(&b.signature);
            out.extend_from_slice(&b.id.to_be_bytes());
            out.push(b.name.len() as u8);
            out.extend_from_slice(&b.name);
            out.extend_from_slice(&b.name_pad);
            out.extend_from_slice(&(b.data.len() as u32).to_be_bytes());
            out.extend_from_slice(&b.data);
            out.extend_from_slice(&b.data_pad);
        }
        out.extend_from_slice(&self.trailing);
    }

    /// Resources holding only `icc` as resource 1039 (even-padded), or none: the header-only
    /// documents of [`crate::raster_proposal`] and [`crate::image_source_document`].
    pub(crate) fn with_icc_profile(icc: Option<Vec<u8>>) -> Self {
        let mut resources = Self::default();
        if let Some(icc) = icc {
            let pad = if icc.len() % 2 == 1 { vec![0] } else { Vec::new() };
            resources.blocks.push(ImageResource {
                signature: *b"8BIM",
                id: RESOURCE_ICC_PROFILE,
                name: Vec::new(),
                name_pad: vec![0],
                data: icc,
                data_pad: pad,
            });
        }
        resources
    }

    /// Embedded ICC profile bytes (resource 1039), byte-exact.
    pub fn icc_profile(&self) -> Option<&[u8]> {
        self.find(RESOURCE_ICC_PROFILE).map(|b| b.data.as_slice())
    }

    /// Resource 1041: true when the image is intentionally untagged.
    pub fn icc_untagged(&self) -> bool {
        self.find(RESOURCE_ICC_UNTAGGED).and_then(|b| b.data.first()).is_some_and(|&v| v == 1)
    }

    pub fn version_info(&self, max_name_units: usize) -> Option<VersionInfo> {
        let data = &self.find(RESOURCE_VERSION_INFO)?.data;
        let mut r = Reader::new(data);
        let version = r.u32("version_info").ok()?;
        let has_real_merged_data = r.u8("version_info").ok()? != 0;
        let writer_name = read_unicode(&mut r, max_name_units).ok();
        let reader_name = read_unicode(&mut r, max_name_units).ok();
        let file_version = r.u32("version_info").ok();
        Some(VersionInfo { version, has_real_merged_data, writer_name, reader_name, file_version })
    }
}

/// Resource 1006 (0x03EE): names of the alpha channels as a series of Pascal strings.
pub const RESOURCE_ALPHA_NAMES_PASCAL: u16 = 1006;
/// Resource 1045 (0x0415): Unicode alpha names, a series of Unicode strings.
pub const RESOURCE_ALPHA_NAMES_UNICODE: u16 = 1045;
/// Resource 1053 (0x041D): alpha identifiers, one u32 per extra channel.
pub const RESOURCE_ALPHA_IDENTIFIERS: u16 = 1053;
/// Resource 1077 (0x0435): DisplayInfo, u32 version then one entry per extra channel.
pub const RESOURCE_DISPLAY_INFO: u16 = 1077;
/// Bytes of one 1077 entry: colour space u16, four colour u16, opacity u16, mode u8.
pub const DISPLAY_INFO_ENTRY_BYTES: usize = 13;

/// One DisplayInfo (1077) entry, as stored. Layout per the Adobe Photoshop File Formats
/// Specification resource table (ids 1006/1045/1053/1077) and the psd-tools reader (MIT,
/// `AlphaNamesPascal`, `AlphaNamesUnicode`, `AlphaIdentifiers`, `DisplayInfo`), confirmed on the
/// psd-tools corpus files `cmyk-alpha-spot.psd`, `cmyk-spot.psd` and `transparentbg.psd`
/// (inspected 2026-10-31). `mode`: 0, 1 or 2 (psd-tools names them alpha, inverted alpha, spot);
/// what Photoshop's "Color Indicates" options mean for them is UNVERIFIED (D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelDisplayInfo {
    pub colour_space: u16,
    pub colour: [u16; 4],
    /// Percent `0..=100` (overlay opacity, or spot solidity).
    pub opacity: u16,
    pub mode: u8,
}

impl ChannelDisplayInfo {
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.colour_space.to_be_bytes());
        for c in self.colour {
            out.extend_from_slice(&c.to_be_bytes());
        }
        out.extend_from_slice(&self.opacity.to_be_bytes());
        out.push(self.mode);
    }
}

impl ImageResources {
    /// Resource 1006: at most `max` Pascal names (MacRoman bytes, unpadded), or `None` when absent.
    /// More names than `max`, or a name running past the data, is a typed error.
    pub fn alpha_names_pascal(&self, max: usize) -> Result<Option<Vec<Vec<u8>>>> {
        let Some(block) = self.find(RESOURCE_ALPHA_NAMES_PASCAL) else {
            return Ok(None);
        };
        let mut r = Reader::new(&block.data);
        let mut names = Vec::new();
        while r.remaining() > 0 {
            if names.len() >= max {
                return Err(PsdError::NameLimit);
            }
            let len = usize::from(r.u8("alpha_names")?);
            names.push(r.bytes(len, "alpha_names")?.to_vec());
        }
        Ok(Some(names))
    }

    /// Resource 1045: at most `max` Unicode names of at most `max_units` UTF-16 units each (a
    /// trailing NUL is not part of the name), or `None` when absent.
    pub fn alpha_names_unicode(&self, max: usize, max_units: usize) -> Result<Option<Vec<String>>> {
        let Some(block) = self.find(RESOURCE_ALPHA_NAMES_UNICODE) else {
            return Ok(None);
        };
        let mut r = Reader::new(&block.data);
        let mut names = Vec::new();
        // psd-tools reads strings while any byte remains; a trailing fragment shorter than a
        // length field is padding.
        while r.remaining() >= 4 {
            if names.len() >= max {
                return Err(PsdError::NameLimit);
            }
            names.push(read_unicode(&mut r, max_units)?);
        }
        Ok(Some(names))
    }

    /// Resource 1053: one u32 per extra channel (at most `max`, checked before allocating), or
    /// `None` when absent. Identifier 0 marks the merged transparency channel (psd-tools
    /// `get_transparency_index`; corpus `transparentbg.psd`).
    pub fn alpha_identifiers(&self, max: usize) -> Result<Option<Vec<u32>>> {
        let Some(block) = self.find(RESOURCE_ALPHA_IDENTIFIERS) else {
            return Ok(None);
        };
        let count = block.data.len().checked_div(4).unwrap_or(0);
        if count > max {
            return Err(PsdError::BadChannelCount);
        }
        Ok(Some(block.data.chunks_exact(4).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect()))
    }

    /// Resource 1077: the version and at most `max` entries (checked before allocating), or `None`
    /// when absent. Bytes after the last whole entry are ignored here (kept byte-exact in the block).
    pub fn display_info(&self, max: usize) -> Result<Option<(u32, Vec<ChannelDisplayInfo>)>> {
        let Some(block) = self.find(RESOURCE_DISPLAY_INFO) else {
            return Ok(None);
        };
        let mut r = Reader::new(&block.data);
        let version = r.u32("display_info")?;
        let count = r.remaining().checked_div(DISPLAY_INFO_ENTRY_BYTES).unwrap_or(0);
        if count > max {
            return Err(PsdError::BadChannelCount);
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let colour_space = r.u16("display_info")?;
            let colour = [r.u16("display_info")?, r.u16("display_info")?, r.u16("display_info")?, r.u16("display_info")?];
            let opacity = r.u16("display_info")?;
            let mode = r.u8("display_info")?;
            entries.push(ChannelDisplayInfo { colour_space, colour, opacity, mode });
        }
        Ok(Some((version, entries)))
    }

    /// Replaces the data of resource `id` (padding recomputed for the new length) when present;
    /// returns whether it was.
    pub(crate) fn replace_data(&mut self, id: u16, data: Vec<u8>) -> bool {
        let Some(block) = self.blocks.iter_mut().find(|b| b.id == id) else {
            return false;
        };
        block.data_pad = if data.len().is_multiple_of(2) { Vec::new() } else { vec![0] };
        block.data = data;
        true
    }
}

/// Resource 1006 data: Pascal strings, unpadded (psd-tools `AlphaNamesPascal`, padding 1).
pub(crate) fn encode_alpha_names_pascal(names: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for n in names {
        let n = &n[..n.len().min(255)];
        out.push(n.len() as u8);
        out.extend_from_slice(n);
    }
    out
}

/// Resource 1045 data: Unicode strings, each NUL-terminated as the corpus files store them.
pub(crate) fn encode_alpha_names_unicode(names: &[String], max_units: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for n in names {
        let mut units: Vec<u16> = n.encode_utf16().take(max_units.saturating_sub(1)).collect();
        units.push(0);
        out.extend_from_slice(&(units.len() as u32).to_be_bytes());
        for u in units {
            out.extend_from_slice(&u.to_be_bytes());
        }
    }
    out
}

/// Unicode string: u32 UTF-16 code-unit count, then that many big-endian units.
pub(crate) fn read_unicode(r: &mut Reader<'_>, max_units: usize) -> Result<String> {
    let little = r.is_little();
    let units = usize::try_from(r.u32("unicode")?).map_err(|_| PsdError::Overflow)?;
    if units > max_units {
        return Err(PsdError::NameLimit);
    }
    let raw = r.bytes(units.checked_mul(2).ok_or(PsdError::Overflow)?, "unicode")?;
    let decoded = char::decode_utf16(raw.chunks_exact(2).map(|p| {
        if little { u16::from_le_bytes([p[0], p[1]]) } else { u16::from_be_bytes([p[0], p[1]]) }
    }))
    .map(|c| c.unwrap_or('\u{fffd}'))
    .collect::<String>();
    // Photoshop terminates most strings with a NUL unit; it is not part of the name.
    Ok(decoded.trim_end_matches('\0').to_owned())
}
