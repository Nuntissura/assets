//! Image resources section: a sequence of `8BIM` blocks. Every byte (names, data, padding,
//! unparsed tail) is kept so an unmodified section re-emits identically.

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

/// Unicode string: u32 UTF-16 code-unit count, then that many big-endian units.
pub(crate) fn read_unicode(r: &mut Reader<'_>, max_units: usize) -> Result<String> {
    let units = usize::try_from(r.u32("unicode")?).map_err(|_| PsdError::Overflow)?;
    if units > max_units {
        return Err(PsdError::NameLimit);
    }
    let raw = r.bytes(units.checked_mul(2).ok_or(PsdError::Overflow)?, "unicode")?;
    let decoded = char::decode_utf16(raw.chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])))
        .map(|c| c.unwrap_or('\u{fffd}'))
        .collect::<String>();
    // Photoshop terminates most strings with a NUL unit; it is not part of the name.
    Ok(decoded.trim_end_matches('\0').to_owned())
}
