//! ICC named-colour profiles (`nmcl` class, `ncl2` tag): STU-COL-126 names `ncl2` among the tags
//! Studio MUST parse, and STU-COL-188 binds a colour book supplied as a named-colour profile through
//! that parser rather than a bespoke one (lane SMB-COL-SWATCH-GROUPS-1).
//!
//! Admission reuses the wide path's identity and framing (`wide::verify`: `SCPF` id, at most
//! `MAX_WIDE_PROFILE_BYTES`, exact sha256; `wide::frame`: header size field, `acsp`, bounded tag
//! table, every tag inside the file, no duplicate signature). Then: version 2 or 4, class `nmcl`,
//! PCS `Lab ` (an XYZ PCS is refused, see below), an `ncl2` tag of type `ncl2`.
//!
//! `namedColor2Type` layout (ICC.1:2022 10.17, field order and sizes as LittleCMS 2
//! `Type_NamedColor_Read` reads them): type signature, 4 reserved bytes, vendor flag (u32),
//! count (u32), device coordinate count (u32), prefix (32 bytes), suffix (32 bytes), then per colour
//! a 32-byte root name, 3 PCS words (u16) and `device coordinate count` device words (u16). Names
//! are 7-bit ASCII ended by the first NUL (at most 31 characters); a byte at or above 0x80 or a
//! control character is refused.
//!
//! PCS words decode as the legacy 16-bit Lab encoding (L* = w / 0xFF00 * 100, a*/b* = w / 256 -
//! 128), the encoding LittleCMS 2 applies to named-colour PCS values (`_cmsReadInputLUT` appends
//! its Lab V2 -> V4 stage to every named-colour pipeline). Because LittleCMS applies that stage even
//! to an XYZ-PCS profile, the meaning of an XYZ named-colour PCS differs between readers; Prism
//! refuses it (`unsupported_pcs`) instead of choosing. ICC text parity UNVERIFIED (D5). Device
//! words decode as w / 65535.
//!
//! Hostile input (V4-01/02): integer arithmetic is denied; the colour and device counts are bounded
//! and the whole table length is checked against the tag size before any colour is stored.
#![deny(clippy::arithmetic_side_effects)]
use crate::ProfileInput;
use crate::wide::{WideError, frame, verify};
use hsk_studio_accord::CancellationToken;

/// Colours of one named-colour profile.
pub const MAX_NAMED_COLOURS: usize = 16_384;
/// Device coordinates per colour (ICC colour spaces have at most 15 channels).
pub const MAX_NAMED_DEVICE_COORDS: usize = 15;
const HEADER: usize = 84;
const NAME: usize = 32;
/// Root name plus the three PCS words.
const ENTRY_FIXED: usize = 38;

/// Typed refusals of named-colour parsing. Codes are stable snake_case strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedColourError {
    /// Identity, size, hash or framing refusal of the shared profile admission.
    Wide(WideError),
    UnsupportedVersion,
    /// The profile class is not `nmcl`.
    NotNamedColourClass,
    UnsupportedPcs,
    MissingNamedColourTag,
    /// The `ncl2` tag is not of type `ncl2`.
    TagType,
    /// The tag is shorter than its declared colour table.
    Truncated,
    CountLimit,
    DeviceCoordLimit,
    /// A prefix, suffix or root name is not 7-bit printable ASCII.
    InvalidName,
    Canceled,
}
impl NamedColourError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Wide(e) => e.code(),
            Self::UnsupportedVersion => "unsupported_version",
            Self::NotNamedColourClass => "not_named_colour_class",
            Self::UnsupportedPcs => "unsupported_pcs",
            Self::MissingNamedColourTag => "missing_named_colour_tag",
            Self::TagType => "named_colour_tag_type",
            Self::Truncated => "named_colour_truncated",
            Self::CountLimit => "named_colour_count_limit",
            Self::DeviceCoordLimit => "named_colour_device_coord_limit",
            Self::InvalidName => "named_colour_invalid_name",
            Self::Canceled => "canceled",
        }
    }
}
impl std::fmt::Display for NamedColourError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for NamedColourError {}
impl From<WideError> for NamedColourError {
    fn from(e: WideError) -> Self {
        Self::Wide(e)
    }
}

/// One named colour: its root name, measured PCS Lab (D50) and device coordinates (0..1).
#[derive(Clone, Debug, PartialEq)]
pub struct NamedColourEntry {
    pub root_name: String,
    pub pcs_lab: [f64; 3],
    pub device: Vec<f64>,
}

/// The decoded `ncl2` table of a named-colour profile.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedColourTable {
    /// sha256 of the profile bytes (the identity a colour-book binding records).
    pub sha256: [u8; 32],
    pub vendor_flag: u32,
    pub prefix: String,
    pub suffix: String,
    pub device_coords: usize,
    pub entries: Vec<NamedColourEntry>,
}
impl NamedColourTable {
    /// `prefix + root name + suffix` of entry `index`.
    pub fn display_name(&self, index: usize) -> Option<String> {
        let e = self.entries.get(index)?;
        Some([self.prefix.as_str(), e.root_name.as_str(), self.suffix.as_str()].concat())
    }
    /// The first entry with root name `root_name`.
    pub fn entry(&self, root_name: &str) -> Option<&NamedColourEntry> {
        self.entries.iter().find(|e| e.root_name == root_name)
    }
}

fn be16(b: &[u8], at: usize) -> u16 {
    b.get(at..at.saturating_add(2)).and_then(|s| <[u8; 2]>::try_from(s).ok()).map_or(0, u16::from_be_bytes)
}
fn be32(b: &[u8], at: usize) -> u32 {
    b.get(at..at.saturating_add(4)).and_then(|s| <[u8; 4]>::try_from(s).ok()).map_or(0, u32::from_be_bytes)
}
fn name(field: &[u8]) -> Result<String, NamedColourError> {
    let end = field.iter().position(|&c| c == 0).unwrap_or(field.len()).min(NAME.saturating_sub(1));
    let bytes = &field[..end];
    if bytes.iter().any(|&c| !(0x20..0x7f).contains(&c)) {
        return Err(NamedColourError::InvalidName);
    }
    Ok(bytes.iter().map(|&c| char::from(c)).collect())
}

/// Parses the `ncl2` table of an ICC named-colour profile (STU-COL-126, STU-COL-188).
pub fn parse_named_colour_profile(input: ProfileInput<'_>, token: &CancellationToken) -> Result<NamedColourTable, NamedColourError> {
    token.check().map_err(|_| NamedColourError::Canceled)?;
    verify(input)?;
    let b = input.bytes;
    let tags = frame(b)?;
    if !matches!(b[8], 2 | 4) {
        return Err(NamedColourError::UnsupportedVersion);
    }
    if &b[12..16] != b"nmcl" {
        return Err(NamedColourError::NotNamedColourClass);
    }
    if &b[20..24] != b"Lab " {
        return Err(NamedColourError::UnsupportedPcs);
    }
    let tag = tags.iter().find(|t| &t.sig == b"ncl2").ok_or(NamedColourError::MissingNamedColourTag)?;
    if &tag.kind != b"ncl2" {
        return Err(NamedColourError::TagType);
    }
    let end = tag.offset.checked_add(tag.size).ok_or(NamedColourError::Truncated)?;
    let data = b.get(tag.offset..end).ok_or(NamedColourError::Truncated)?;
    if data.len() < HEADER {
        return Err(NamedColourError::Truncated);
    }
    let vendor_flag = be32(data, 8);
    let count = usize::try_from(be32(data, 12)).map_err(|_| NamedColourError::CountLimit)?;
    let device_coords = usize::try_from(be32(data, 16)).map_err(|_| NamedColourError::DeviceCoordLimit)?;
    if count > MAX_NAMED_COLOURS {
        return Err(NamedColourError::CountLimit);
    }
    if device_coords > MAX_NAMED_DEVICE_COORDS {
        return Err(NamedColourError::DeviceCoordLimit);
    }
    // 32 name bytes + 3 PCS words + device words; the whole table must fit in the tag.
    let entry_size = device_coords.checked_mul(2).and_then(|d| d.checked_add(ENTRY_FIXED)).ok_or(NamedColourError::DeviceCoordLimit)?;
    let table = count.checked_mul(entry_size).and_then(|t| t.checked_add(HEADER)).ok_or(NamedColourError::Truncated)?;
    if table > data.len() {
        return Err(NamedColourError::Truncated);
    }
    let prefix = name(&data[20..52])?;
    let suffix = name(&data[52..84])?;
    let mut entries = Vec::with_capacity(count);
    let mut at = HEADER;
    for i in 0..count {
        if i.is_multiple_of(256) {
            token.check().map_err(|_| NamedColourError::Canceled)?;
        }
        let root_name = name(&data[at..at.saturating_add(NAME)])?;
        let p = at.saturating_add(NAME);
        let lab = [
            f64::from(be16(data, p)) * 100.0 / 65280.0,
            f64::from(be16(data, p.saturating_add(2))) / 256.0 - 128.0,
            f64::from(be16(data, p.saturating_add(4))) / 256.0 - 128.0,
        ];
        let dev = p.saturating_add(6);
        let device = (0..device_coords).map(|k| f64::from(be16(data, dev.saturating_add(k.saturating_mul(2)))) / 65535.0).collect();
        entries.push(NamedColourEntry { root_name, pcs_lab: lab, device });
        at = at.saturating_add(entry_size);
    }
    Ok(NamedColourTable { sha256: input.expected_sha256, vendor_flag, prefix, suffix, device_coords, entries })
}

/// Same descriptor for models, the manual and Argus adapters.
pub const NAMED_COLOUR_DESCRIPTOR: &str = r##"{"module":"STUDIO-MODULE-PRISM","path":"named_colour","descriptor_version":1,"spec":["STU-COL-126","STU-COL-188"],"api":"parse_named_colour_profile(ProfileInput, &CancellationToken) -> NamedColourTable {sha256, vendor_flag, prefix, suffix, device_coords, entries: NamedColourEntry {root_name, pcs_lab D50, device 0..1}} | NamedColourError; display_name(index) = prefix + root name + suffix; entry(root_name)","admission":"wide-path verify (SCPF id, at most 2097152 bytes, exact sha256) and frame (tag table bounds) before any field is read; version 2|4, class nmcl, PCS Lab, ncl2 tag of type ncl2","decode":"namedColor2Type: vendor flag, count, device coordinate count, 32-byte prefix and suffix, per colour 32-byte root name + 3 PCS words + device words; legacy 16-bit Lab (L = w*100/65280, a/b = w/256 - 128) as LittleCMS 2 reads named-colour PCS; device w/65535; names 7-bit printable ASCII up to the first NUL, at most 31 characters","bounds":{"colours":16384,"device_coords":15,"profile_bytes":2097152},"reject":"Wide(invalid_profile_id|profile_limit|hash_mismatch|malformed_profile) | unsupported_version | not_named_colour_class | unsupported_pcs (XYZ PCS: readers disagree, LittleCMS 2 applies its Lab V2 to V4 stage regardless) | missing_named_colour_tag | named_colour_tag_type | named_colour_truncated | named_colour_count_limit | named_colour_device_coord_limit | named_colour_invalid_name | canceled","binding":"hsk-studio-folio StudioColourBook::from_named_colours(book_id, title, license_state, license_note, StudioNamedColourProfileRef {SCPF id, sha256}, prefix, suffix, [(root name, pcs_lab)]) builds the colour book; the host joins Prism and Folio (no crate edge)","not_in_scope":["XYZ-PCS named-colour profiles","proofing through a named-colour profile as a transform","Observe emission (pure parse; the caller reports the outcome)"],"manual_route":"same descriptor","argus_route":"same descriptor"}"##;
