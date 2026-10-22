//! KA header and the chained stream directory (`#FAT`/`#FT2`/`#FT3`/`#FT4`).
//!
//! Layout provenance (no Affinity binary was decompiled or disassembled):
//! - VMDevCpp/afread (MIT, Copyright (c) 2020-2021 Vladimir Mamonov), `source/affinity/src/Archive.cpp`:
//!   `HeaderStruct`, `InfHeaderStruct`, `FatHeaderStruct`, `FatFileStruct1/2`, `FatDirStruct`, the
//!   directory chain through `next_offset`, head revision = latest directory date, `#Fil` stream tag,
//!   compression byte (`& 3` algorithm, `& 0xC0` predictor, bit 5 interleave), legacy `#FAT`/`#FT2`
//!   compression code remap. Re-expressed here in Rust with bounds and typed refusals.
//! - Lane hexdumps (2026-10-16): the same layout parses the 9 AFDesignLoad MIT test designs (version 9,
//!   `#FAT`, zlib) and 4 installed Canva fixtures (versions 11/12, `#FT4`, zstd), every CRC matching.
//!   Field roles not used for decoding (`some_offset`, `unknown*`) stay UNVERIFIED and are kept raw.

use crate::error::{AffinityError, Result};
use crate::limits::Limits;

pub(crate) const MAGIC: [u8; 4] = [0x00, 0xFF, 0x4B, 0x41];
const TAG_INF: &[u8; 4] = b"#Inf";
const TAG_PROT: &[u8; 4] = b"Prot";
pub(crate) const TAG_FIL: &[u8; 4] = b"#Fil";
/// End of `Prot` (version > 7): the first stream's `#Fil` tag sits here.
pub(crate) const FIXED_HEADER_END: u64 = 0x48;

/// Bounded little-endian cursor over the whole file.
pub(crate) struct Cur<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Cur<'a> {
    pub(crate) fn at(bytes: &'a [u8], pos: u64) -> Option<Self> {
        let pos = usize::try_from(pos).ok()?;
        (pos <= bytes.len()).then_some(Self { bytes, pos })
    }
    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }
    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    pub(crate) fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_le_bytes([s[0], s[1]]))
    }
    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    pub(crate) fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|s| {
            let mut a = [0u8; 8];
            a.copy_from_slice(s);
            u64::from_le_bytes(a)
        })
    }
}

/// The fixed header (0x00..0x48).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KaHeader {
    /// u16 at 0x04: 9 seen in V1 (2015) files, 11 in V2, 12 in V3.
    pub version: u16,
    /// u16 at 0x06 (meaning UNVERIFIED; bits 0 and 1 are refused as in afread).
    pub flags: u16,
    /// Class 4CC in reading order (`Prsn` = document; stored byte-reversed).
    pub class: [u8; 4],
    /// u64 at 0x10: first directory.
    pub directory_offset: u64,
    /// u64 at 0x18: the `FF FF FF FF Thmb` block (equals the end of the directory in every sample).
    pub thumbnail_offset: u64,
    /// u64 at 0x30: Unix seconds (repeated in the directory) [role UNVERIFIED, strong].
    pub created_unix: u64,
    /// u32 at 0x38 (afread: `revision`).
    pub revision: u32,
    /// u32 at 0x44 when version > 7 (`Prot` value; meaning UNVERIFIED).
    pub prot: Option<u32>,
}

impl KaHeader {
    pub fn generation(&self) -> Generation {
        match self.version {
            7..=10 => Generation::V1,
            11 => Generation::V2,
            12 => Generation::V3,
            _ => Generation::Unknown,
        }
    }
    pub fn is_document(&self) -> bool {
        &self.class == b"Prsn"
    }
    pub fn class_text(&self) -> String {
        self.class.iter().map(|&b| if b.is_ascii_graphic() { char::from(b) } else { '?' }).collect()
    }
}

/// Product generation from the format version (V1 = 7..=10 by afread's accepted range; only 9 observed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generation {
    V1,
    V2,
    V3,
    Unknown,
}

impl Generation {
    pub fn label(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
            Self::V3 => "v3",
            Self::Unknown => "unknown",
        }
    }
}

/// Recognises the KA magic and reads the class without validating anything else.
pub fn identify(bytes: &[u8]) -> Option<(u16, [u8; 4])> {
    let h = bytes.get(..12)?;
    (h[..4] == MAGIC).then(|| (u16::from_le_bytes([h[4], h[5]]), [h[11], h[10], h[9], h[8]]))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryKind {
    Fat,
    Ft2,
    Ft3,
    Ft4,
}

impl DirectoryKind {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Fat => "#FAT",
            Self::Ft2 => "#FT2",
            Self::Ft3 => "#FT3",
            Self::Ft4 => "#FT4",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryState {
    /// State 0: carries its name.
    Named,
    /// State 1: a revision of an id named elsewhere.
    Unnamed,
    /// State 2: deleted in this revision (no location record).
    Deleted,
}

/// Stream codec from the compression byte (`& 3`): 0 stored, 1 zlib, 2 zstd, 3 refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Stored,
    Zlib,
    Zstd,
    Unknown(u8),
}

/// One directory record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: u32,
    pub state: EntryState,
    pub name: Option<String>,
    /// File offset of the `#Fil` tag.
    pub offset: u64,
    /// Inflated size.
    pub size: u64,
    pub stored_size: u64,
    pub crc32: u32,
    /// Compression byte after the legacy `#FAT`/`#FT2` remap.
    pub compression: u8,
}

impl Entry {
    pub fn codec(&self) -> Codec {
        match self.compression & 3 {
            0 => Codec::Stored,
            1 => Codec::Zlib,
            2 => Codec::Zstd,
            other => Codec::Unknown(other),
        }
    }
    /// Predictor class (`& 0xC0` >> 6): 0 none, 1 byte delta, 2 u16 delta, 3 left as stored (afread
    /// applies no inverse; the CRC decides).
    pub fn predictor(&self) -> u8 {
        predictor_class(self.compression)
    }
}

/// Bits 6-7 of a compression byte.
pub(crate) fn predictor_class(compression: u8) -> u8 {
    match compression & 0xC0 {
        0x40 => 1,
        0x80 => 2,
        0xC0 => 3,
        _ => 0,
    }
}

/// A folder record (`d/`, `edc/`): name and the entry count it claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub name: String,
    pub entries: u64,
}

/// One saved revision's directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directory {
    pub kind: DirectoryKind,
    pub offset: u64,
    pub next_offset: u64,
    pub created_unix: u64,
    pub entries: Vec<Entry>,
    pub folders: Vec<Folder>,
}

/// A head-revision stream: the newest directory's record for its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub name: String,
    pub entry: Entry,
    /// Created date of the directory the head record came from.
    pub revision_unix: u64,
}

/// A parsed KA container borrowing the file bytes.
#[derive(Clone, Debug)]
pub struct KaContainer<'a> {
    pub(crate) bytes: &'a [u8],
    pub header: KaHeader,
    /// In chain order (first = header's directory).
    pub directories: Vec<Directory>,
    pub(crate) limits: Limits,
}

fn hdr<T>(v: Option<T>) -> Result<T> {
    v.ok_or(AffinityError::HeaderTruncated)
}

fn dir<T>(v: Option<T>) -> Result<T> {
    v.ok_or(AffinityError::DirectoryTruncated)
}

/// Fixed byte size of one located entry record after `id` and `state`.
/// offset u64, size u64, stored size u64, crc u32, compression u8 (29), + u32 from `#FT2`, + u32 in `#FT4`.
fn location_len(kind: DirectoryKind) -> usize {
    match kind {
        DirectoryKind::Fat => 29,
        DirectoryKind::Ft2 | DirectoryKind::Ft3 => 33,
        DirectoryKind::Ft4 => 37,
    }
}

impl<'a> KaContainer<'a> {
    /// Parses the header and every chained directory (no stream is decoded).
    pub fn parse(bytes: &'a [u8], limits: &Limits) -> Result<Self> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limits.max_input_bytes {
            return Err(AffinityError::TotalBudget);
        }
        if bytes.get(..4).is_none_or(|m| m != MAGIC) {
            return Err(if !bytes.is_empty() && bytes.len() < 4 && MAGIC.starts_with(bytes) { AffinityError::HeaderTruncated } else { AffinityError::NotAffinity });
        }
        let mut c = hdr(Cur::at(bytes, 4))?;
        let version = hdr(c.u16())?;
        let flags = hdr(c.u16())?;
        let stored = hdr(c.take(4))?;
        let class = [stored[3], stored[2], stored[1], stored[0]];
        if !(7..=12).contains(&version) {
            return Err(AffinityError::VersionUnsupported(version));
        }
        if flags & 3 != 0 {
            return Err(AffinityError::FlagsUnsupported(flags));
        }
        if hdr(c.take(4))? != TAG_INF {
            return Err(AffinityError::InfoTagMissing);
        }
        let directory_offset = hdr(c.u64())?;
        let thumbnail_offset = hdr(c.u64())?;
        let _first_stream_len = hdr(c.u64())?;
        let _unknown = hdr(c.u64())?;
        let created_unix = hdr(c.u64())?;
        let revision = hdr(c.u32())?;
        let _num2 = hdr(c.u32())?;
        let prot = if version > 7 {
            if hdr(c.take(4))? != TAG_PROT {
                return Err(AffinityError::ProtTagMissing);
            }
            Some(hdr(c.u32())?)
        } else {
            None
        };
        let header = KaHeader { version, flags, class, directory_offset, thumbnail_offset, created_unix, revision, prot };
        let directories = parse_chain(bytes, directory_offset, c.pos, limits)?;
        Ok(Self { bytes, header, directories, limits: *limits })
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Head-revision streams, sorted by name: for each id the record of the directory with the latest
    /// date (chain order breaks ties); ids deleted at the head are absent, unnamed ids keep `#<id>`.
    pub fn streams(&self) -> Vec<Stream> {
        let mut names: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
        let mut head: std::collections::BTreeMap<u32, (u64, &Entry)> = std::collections::BTreeMap::new();
        for d in &self.directories {
            for e in &d.entries {
                if let Some(n) = &e.name {
                    names.entry(e.id).or_insert_with(|| n.clone());
                }
                match head.get(&e.id) {
                    Some((date, _)) if *date > d.created_unix => {}
                    _ => {
                        head.insert(e.id, (d.created_unix, e));
                    }
                }
            }
        }
        let mut out: Vec<Stream> = head
            .into_iter()
            .filter(|(_, (_, e))| e.state != EntryState::Deleted)
            .map(|(id, (date, e))| Stream {
                name: names.get(&id).cloned().unwrap_or_else(|| format!("#{id}")),
                entry: e.clone(),
                revision_unix: date,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// The head stream named `name` (deleted at the head = [`AffinityError::StreamDeleted`]).
    pub fn stream(&self, name: &str) -> Result<Stream> {
        if let Some(s) = self.streams().into_iter().find(|s| s.name == name) {
            return Ok(s);
        }
        let known = self.directories.iter().flat_map(|d| &d.entries).any(|e| e.name.as_deref() == Some(name));
        Err(if known { AffinityError::StreamDeleted } else { AffinityError::StreamNotFound })
    }
}

fn parse_chain(bytes: &[u8], first: u64, header_end: usize, limits: &Limits) -> Result<Vec<Directory>> {
    let mut out: Vec<Directory> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut offset = first;
    let mut budget = limits.max_entries;
    let mut ids = std::collections::BTreeMap::new();
    while offset != 0 {
        if u32::try_from(out.len()).unwrap_or(u32::MAX) >= limits.max_directories {
            return Err(AffinityError::DirectoryBudget);
        }
        if !seen.insert(offset) {
            return Err(AffinityError::DirectoryCycle);
        }
        let in_file = usize::try_from(offset).ok().is_some_and(|o| o >= header_end && o < bytes.len());
        if !in_file {
            return Err(AffinityError::DirectoryOutOfBounds);
        }
        let d = parse_directory(bytes, offset, &mut budget, &mut ids, limits)?;
        offset = d.next_offset;
        out.push(d);
    }
    Ok(out)
}

fn parse_directory(
    bytes: &[u8],
    offset: u64,
    budget: &mut u32,
    ids: &mut std::collections::BTreeMap<String, u32>,
    limits: &Limits,
) -> Result<Directory> {
    let mut c = dir(Cur::at(bytes, offset))?;
    let kind = match dir(c.take(4))? {
        b"#FAT" => DirectoryKind::Fat,
        b"#FT2" => DirectoryKind::Ft2,
        b"#FT3" => DirectoryKind::Ft3,
        b"#FT4" => DirectoryKind::Ft4,
        _ => return Err(AffinityError::DirectoryTagInvalid),
    };
    let next_offset = dir(c.u64())?;
    let created_unix = dir(c.u64())?;
    let _some_offset = dir(c.u64())?;
    let _some_length = dir(c.u64())?;
    let _unknown5 = dir(c.u64())?;
    let files = dir(c.u32())?;
    let _unknown7 = dir(c.u32())?;
    let _unknown8 = dir(c.u32())?;
    let folders = dir(c.u16())?;
    let _unknown10 = dir(c.u8())?;
    // Budget and the minimum record sizes (5-byte entry, 12-byte folder) against the bytes left,
    // before any allocation.
    let claimed = files.checked_add(u32::from(folders)).ok_or(AffinityError::EntryBudget)?;
    *budget = budget.checked_sub(claimed).ok_or(AffinityError::EntryBudget)?;
    let min = u64::from(files).saturating_mul(5).saturating_add(u64::from(folders).saturating_mul(12));
    if min > u64::try_from(c.remaining()).unwrap_or(u64::MAX) {
        return Err(AffinityError::DirectoryTruncated);
    }
    let mut entries = Vec::with_capacity(usize::try_from(files).unwrap_or(0));
    for _ in 0..files {
        let id = dir(c.u32())?;
        let state = match dir(c.u8())? {
            0 => EntryState::Named,
            1 => EntryState::Unnamed,
            2 => EntryState::Deleted,
            _ => return Err(AffinityError::EntryStateInvalid),
        };
        let (mut offset, mut size, mut stored_size, mut crc32, mut compression) = (0, 0, 0, 0, 0);
        if state != EntryState::Deleted {
            let rec = dir(c.take(location_len(kind)))?;
            let mut r = Cur { bytes: rec, pos: 0 };
            offset = dir(r.u64())?;
            size = dir(r.u64())?;
            stored_size = dir(r.u64())?;
            crc32 = dir(r.u32())?;
            compression = dir(r.u8())?;
            if matches!(kind, DirectoryKind::Fat | DirectoryKind::Ft2) {
                // afread: legacy codes 1..4 = zlib with predictor classes 0..3; anything else stored.
                compression = match compression {
                    1 => 0x01,
                    2 => 0x41,
                    3 => 0x81,
                    4 => 0xC1,
                    _ => 0,
                };
            }
        }
        let name = if state == EntryState::Named {
            let len = dir(c.u16())?;
            if len > limits.max_name_bytes {
                return Err(AffinityError::NameInvalid);
            }
            let raw = dir(c.take(usize::from(len)))?;
            Some(String::from_utf8(raw.to_vec()).map_err(|_| AffinityError::NameInvalid)?)
        } else {
            None
        };
        if let Some(n) = &name
            && *ids.entry(n.clone()).or_insert(id) != id
        {
            // afread: one name is bound to one id across every revision.
            return Err(AffinityError::StreamIdConflict);
        }
        entries.push(Entry { id, state, name, offset, size, stored_size, crc32, compression });
    }
    let mut out_folders = Vec::with_capacity(usize::from(folders));
    for _ in 0..folders {
        let len = dir(c.u16())?;
        let second = dir(c.u16())?;
        let count = dir(c.u64())?;
        if second != 0 {
            return Err(AffinityError::FolderInvalid);
        }
        if len > limits.max_name_bytes {
            return Err(AffinityError::NameInvalid);
        }
        let raw = dir(c.take(usize::from(len)))?;
        let name = String::from_utf8(raw.to_vec()).map_err(|_| AffinityError::NameInvalid)?;
        out_folders.push(Folder { name, entries: count });
    }
    Ok(Directory { kind, offset, next_offset, created_unix, entries, folders: out_folders })
}
