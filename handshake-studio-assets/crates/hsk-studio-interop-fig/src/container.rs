//! Bounded .fig container reader (STU-IO-187): a ZIP (`canvas.fig`, `meta.json`, `thumbnail.png`,
//! `images/<hash>`) or a bare `canvas.fig` stream; the `canvas.fig` prelude (8 ASCII bytes + u32 LE
//! version) and its length-prefixed chunk table. Every ZIP part is kept verbatim with its sha256
//! (opaque preservation is the default); chunks are recorded as ranges of the canvas part.
//!
//! Format facts come from public community sources, not an official spec, and are UNVERIFIED against
//! a Figma-authored file (none exists on this machine). The reader never decides what a version
//! means: that is `version_map`.
//!
//! ZIP rules: bounded entry count, entry size, total inflated size and expansion ratio; exact and
//! case-folded duplicate names, traversal/absolute/backslash/drive/control names refused; local
//! headers cross-checked against the central directory; overlapping entry data refused; CRC-32
//! verified; STORE and DEFLATE only (no ZIP64, encryption or multi-disk). Never touches the file system.
use crate::{FigError, Result};
use hsk_studio_accord::CancellationToken;
use miniz_oxide::inflate::decompress_to_vec_with_limit;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const CANVAS_PART: &str = "canvas.fig";
pub const META_PART: &str = "meta.json";
pub const THUMBNAIL_PART: &str = "thumbnail.png";
pub const IMAGES_PREFIX: &str = "images/";

pub type Sha256Bytes = [u8; 32];

pub(crate) fn sha256(bytes: &[u8]) -> Sha256Bytes {
    Sha256::digest(bytes).into()
}

/// Lower-case hex of a digest.
pub fn hex(digest: &Sha256Bytes) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Product family named by the 8-byte `canvas.fig` prelude. Only the three preludes seen in public
/// community notes are classified; anything else is `Unknown`, never guessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FigKind {
    /// `fig-kiwi`
    Design,
    /// `fig-jam.`
    Jam,
    /// `fig-deck`
    Deck,
    Unknown([u8; 8]),
}

impl FigKind {
    pub fn from_prelude(prelude: &[u8; 8]) -> Self {
        match prelude {
            b"fig-kiwi" => Self::Design,
            b"fig-jam." => Self::Jam,
            b"fig-deck" => Self::Deck,
            other => Self::Unknown(*other),
        }
    }
    pub const fn prelude(self) -> [u8; 8] {
        match self {
            Self::Design => *b"fig-kiwi",
            Self::Jam => *b"fig-jam.",
            Self::Deck => *b"fig-deck",
            Self::Unknown(p) => p,
        }
    }
    /// Stable label without input-derived text.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Jam => "jam",
            Self::Deck => "deck",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// Declared bounds of one `open_fig` / `import_fig` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Input bytes.
    pub max_bytes: usize,
    pub max_entries: usize,
    pub max_entry_bytes: u64,
    pub max_inflated_bytes: u64,
    /// Expansion ratio, judged for DEFLATE entries above 1 MiB inflated.
    pub max_ratio: u64,
    /// `canvas.fig` chunks.
    pub max_chunks: usize,
    /// `meta.json` bytes parsed for top-level keys, and the number of keys kept.
    pub max_meta_bytes: usize,
    pub max_meta_keys: usize,
    /// Inflated Kiwi schema chunk.
    pub max_schema_bytes: usize,
    /// Inflated Kiwi message chunk.
    pub max_message_bytes: usize,
    pub max_definitions: usize,
    pub max_fields: usize,
    pub max_depth: usize,
    /// Decoded values (array elements and fields) per message; bounds zero-width element loops.
    pub max_values: u64,
    /// `nodeChanges` elements in the census.
    pub max_nodes: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 128 << 20,
            max_entries: 1024,
            max_entry_bytes: 64 << 20,
            max_inflated_bytes: 256 << 20,
            max_ratio: 100,
            max_chunks: 64,
            max_meta_bytes: 1 << 20,
            max_meta_keys: 256,
            max_schema_bytes: 4 << 20,
            max_message_bytes: 256 << 20,
            max_definitions: 4096,
            max_fields: 1024,
            max_depth: 32,
            max_values: 64 << 20,
            max_nodes: 4_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packaging {
    Zip,
    /// Legacy bare `canvas.fig` stream, no ZIP.
    Bare,
}

/// One container part kept byte-exact (a ZIP entry, or the bare canvas stream).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FigPart {
    pub name: String,
    pub bytes: Vec<u8>,
    pub sha256: Sha256Bytes,
}

/// One length-prefixed `canvas.fig` chunk, as a range of the canvas part's bytes (data only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FigChunk {
    pub index: usize,
    pub offset: usize,
    pub len: usize,
    pub sha256: Sha256Bytes,
}

/// `meta.json` handling: only the top-level keys are read; the part itself is kept raw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaKeys {
    Absent,
    Keys(Vec<String>),
    /// Not parseable JSON, or larger than `Limits::max_meta_bytes`.
    Unreadable,
    /// Valid JSON whose top level is not an object.
    NotObject,
}

/// A safely opened container: every part and chunk preserved, nothing interpreted beyond identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FigContainer {
    pub packaging: Packaging,
    pub kind: FigKind,
    pub version: u32,
    /// sha256 of the exact input bytes (stale-source identity).
    pub source_sha256: Sha256Bytes,
    /// ZIP entries in central-directory order, or the single bare canvas part.
    pub parts: Vec<FigPart>,
    canvas: usize,
    /// Empty when the prelude is `Unknown` (the chunk layout is only known for classified kinds).
    pub chunks: Vec<FigChunk>,
    pub meta: MetaKeys,
}

impl FigContainer {
    pub fn canvas(&self) -> &FigPart {
        &self.parts[self.canvas]
    }
    pub fn part(&self, name: &str) -> Option<&FigPart> {
        self.parts.iter().find(|p| p.name == name)
    }
    /// Data bytes of chunk `index` (a slice of the canvas part).
    pub fn chunk_bytes(&self, index: usize) -> Option<&[u8]> {
        let c = self.chunks.get(index)?;
        self.canvas()
            .bytes
            .get(c.offset..c.offset.checked_add(c.len)?)
    }
}

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
/// Expansion ratio is only judged for entries above this inflated size.
const RATIO_FLOOR: u64 = 1 << 20;
const MAX_NAME: usize = 512;
const PRELUDE_AND_VERSION: usize = 12;

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}
static CRC_TABLE: [u32; 256] = crc_table();

pub(crate) fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |c, b| {
        CRC_TABLE[usize::from((c as u8) ^ b)] ^ (c >> 8)
    })
}

fn take(bytes: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    at.checked_add(len)
        .and_then(|end| bytes.get(at..end))
        .ok_or(FigError::Zip("truncated"))
}
fn le16(bytes: &[u8], at: usize) -> Result<u16> {
    let s = take(bytes, at, 2)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}
fn le32(bytes: &[u8], at: usize) -> Result<u32> {
    let s = take(bytes, at, 4)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('/')
        && !name.contains(['\\', ':'])
        && name.chars().all(|c| !c.is_control())
        && name
            .trim_end_matches('/')
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

struct Central {
    name: String,
    method: u16,
    crc: u32,
    comp: u64,
    uncomp: u64,
    local: usize,
}

fn find_eocd(bytes: &[u8]) -> Result<usize> {
    let lowest = bytes.len().saturating_sub(22 + usize::from(u16::MAX));
    let mut pos = bytes.len() - 22;
    loop {
        if le32(bytes, pos)? == SIG_EOCD {
            let comment = usize::from(le16(bytes, pos + 20)?);
            if pos + 22 + comment == bytes.len() {
                return Ok(pos);
            }
        }
        if pos == lowest {
            return Err(FigError::Zip("eocd_missing"));
        }
        pos -= 1;
    }
}

fn check(cancel: &CancellationToken) -> Result<()> {
    cancel.check().map_err(|_| FigError::Canceled)
}

/// Reads every file entry as `(name, bytes)` in central-directory order.
fn read_zip(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Vec<(String, Vec<u8>)>> {
    if bytes.len() < 22 {
        return Err(FigError::Zip("too_short"));
    }
    let eocd = find_eocd(bytes)?;
    if le16(bytes, eocd + 4)? != 0 || le16(bytes, eocd + 6)? != 0 {
        return Err(FigError::Zip("multi_disk"));
    }
    let (n_disk, n_total) = (le16(bytes, eocd + 8)?, le16(bytes, eocd + 10)?);
    let (cd_size, cd_off) = (le32(bytes, eocd + 12)?, le32(bytes, eocd + 16)?);
    if n_total == u16::MAX || cd_size == u32::MAX || cd_off == u32::MAX {
        return Err(FigError::Zip("zip64_unsupported"));
    }
    if n_disk != n_total {
        return Err(FigError::Zip("multi_disk"));
    }
    let (cd_size, cd_off) = (cd_size as usize, cd_off as usize);
    if cd_off.checked_add(cd_size) != Some(eocd) {
        return Err(FigError::Zip("central_directory_bounds"));
    }
    if usize::from(n_total) > limits.max_entries {
        return Err(FigError::BudgetExceeded("entries"));
    }

    let mut entries: Vec<Central> = Vec::new();
    let (mut at, mut inflated) = (cd_off, 0u64);
    let mut seen = BTreeSet::new();
    for _ in 0..n_total {
        check(cancel)?;
        if le32(bytes, at)? != SIG_CENTRAL {
            return Err(FigError::Zip("central_signature"));
        }
        let flags = le16(bytes, at + 8)?;
        if flags & (1 | 0x40 | 0x2000) != 0 {
            return Err(FigError::Zip("encrypted"));
        }
        let method = le16(bytes, at + 10)?;
        let (crc, comp, uncomp) = (
            le32(bytes, at + 16)?,
            le32(bytes, at + 20)?,
            le32(bytes, at + 24)?,
        );
        let (n, m, k) = (
            usize::from(le16(bytes, at + 28)?),
            usize::from(le16(bytes, at + 30)?),
            usize::from(le16(bytes, at + 32)?),
        );
        let local = le32(bytes, at + 42)?;
        if comp == u32::MAX || uncomp == u32::MAX || local == u32::MAX {
            return Err(FigError::Zip("zip64_unsupported"));
        }
        if le16(bytes, at + 34)? != 0 {
            return Err(FigError::Zip("multi_disk"));
        }
        let name = std::str::from_utf8(take(bytes, at + 46, n)?)
            .map_err(|_| FigError::Zip("name_encoding"))?
            .to_owned();
        at = at
            .checked_add(46 + n + m + k)
            .filter(|end| *end <= eocd)
            .ok_or(FigError::Zip("central_directory_bounds"))?;
        if !safe_name(&name) {
            return Err(FigError::UnsafeEntryName);
        }
        if name.ends_with('/') {
            if comp != 0 || uncomp != 0 {
                return Err(FigError::Zip("directory_with_data"));
            }
            continue;
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(FigError::DuplicateEntry);
        }
        if method != 0 && method != 8 {
            return Err(FigError::Zip("method_unsupported"));
        }
        let (comp, uncomp) = (u64::from(comp), u64::from(uncomp));
        if uncomp > limits.max_entry_bytes {
            return Err(FigError::BudgetExceeded("entry_bytes"));
        }
        inflated = inflated.saturating_add(uncomp);
        if inflated > limits.max_inflated_bytes {
            return Err(FigError::BudgetExceeded("inflated_bytes"));
        }
        if method == 0 && comp != uncomp {
            return Err(FigError::Zip("stored_size_mismatch"));
        }
        if method == 8 && uncomp > RATIO_FLOOR && uncomp > comp.saturating_mul(limits.max_ratio) {
            return Err(FigError::BudgetExceeded("ratio"));
        }
        entries.push(Central {
            name,
            method,
            crc,
            comp,
            uncomp,
            local: local as usize,
        });
    }
    if at != eocd {
        return Err(FigError::Zip("central_directory_bounds"));
    }

    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(entries.len());
    let mut out = Vec::with_capacity(entries.len());
    for e in &entries {
        check(cancel)?;
        if le32(bytes, e.local)? != SIG_LOCAL {
            return Err(FigError::Zip("local_signature"));
        }
        let (n, m) = (
            usize::from(le16(bytes, e.local + 26)?),
            usize::from(le16(bytes, e.local + 28)?),
        );
        if take(bytes, e.local + 30, n)? != e.name.as_bytes() {
            return Err(FigError::Zip("local_name_mismatch"));
        }
        let start = e.local + 30 + n + m;
        let end = start
            .checked_add(e.comp as usize)
            .filter(|end| *end <= cd_off)
            .ok_or(FigError::Zip("entry_bounds"))?;
        spans.push((e.local, end));
        let data = take(bytes, start, e.comp as usize)?;
        let content = if e.method == 0 {
            data.to_vec()
        } else {
            decompress_to_vec_with_limit(data, e.uncomp as usize)
                .map_err(|_| FigError::Zip("inflate_failed_or_size_mismatch"))?
        };
        if content.len() as u64 != e.uncomp {
            return Err(FigError::Zip("size_mismatch"));
        }
        if crc32(&content) != e.crc {
            return Err(FigError::Zip("crc_mismatch"));
        }
        out.push((e.name.clone(), content));
    }
    spans.sort_unstable();
    if spans.windows(2).any(|w| w[0].1 > w[1].0) {
        return Err(FigError::Zip("overlapping_entries"));
    }
    Ok(out)
}

/// Prelude, version and chunk table of a `canvas.fig` stream. An unclassified prelude yields no
/// chunks: the chunk layout is only known for the classified kinds.
fn parse_canvas(canvas: &[u8], limits: &Limits) -> Result<(FigKind, u32, Vec<FigChunk>)> {
    if canvas.len() < PRELUDE_AND_VERSION {
        return Err(FigError::NotFig("canvas_too_short"));
    }
    let mut prelude = [0u8; 8];
    prelude.copy_from_slice(&canvas[..8]);
    let kind = FigKind::from_prelude(&prelude);
    let version = u32::from_le_bytes([canvas[8], canvas[9], canvas[10], canvas[11]]);
    let mut chunks = Vec::new();
    if matches!(kind, FigKind::Unknown(_)) {
        return Ok((kind, version, chunks));
    }
    let mut at = PRELUDE_AND_VERSION;
    while at < canvas.len() {
        let header = canvas.get(at..at + 4).ok_or(FigError::ChunkOverflow)?;
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let start = at + 4;
        let end = start
            .checked_add(len)
            .filter(|end| *end <= canvas.len())
            .ok_or(FigError::ChunkOverflow)?;
        if chunks.len() >= limits.max_chunks {
            return Err(FigError::BudgetExceeded("chunks"));
        }
        chunks.push(FigChunk {
            index: chunks.len(),
            offset: start,
            len,
            sha256: sha256(&canvas[start..end]),
        });
        at = end;
    }
    Ok((kind, version, chunks))
}

fn read_meta(part: Option<&FigPart>, limits: &Limits) -> Result<MetaKeys> {
    let Some(part) = part else {
        return Ok(MetaKeys::Absent);
    };
    if part.bytes.len() > limits.max_meta_bytes {
        return Ok(MetaKeys::Unreadable);
    }
    match serde_json::from_slice::<serde_json::Value>(&part.bytes) {
        Err(_) => Ok(MetaKeys::Unreadable),
        Ok(serde_json::Value::Object(map)) => {
            if map.len() > limits.max_meta_keys {
                return Err(FigError::BudgetExceeded("meta_keys"));
            }
            Ok(MetaKeys::Keys(map.keys().cloned().collect()))
        }
        Ok(_) => Ok(MetaKeys::NotObject),
    }
}

fn make_part(name: String, bytes: Vec<u8>) -> FigPart {
    let sha256 = sha256(&bytes);
    FigPart {
        name,
        bytes,
        sha256,
    }
}

/// Opens a .fig container within `limits`: ZIP (local file header magic `PK\x03\x04`) or a bare
/// `canvas.fig` stream. Every part and chunk is preserved verbatim; nothing is decoded and no
/// version judgement is made. A bare stream must carry a classified prelude (an unclassified bare
/// stream is `NotFig`); inside a ZIP, an unclassified `canvas.fig` prelude opens as
/// `FigKind::Unknown` with its bytes preserved and no chunk table.
pub fn open_fig(bytes: &[u8], limits: &Limits, cancel: &CancellationToken) -> Result<FigContainer> {
    check(cancel)?;
    if bytes.len() > limits.max_bytes {
        return Err(FigError::BudgetExceeded("bytes"));
    }
    open_with_hash(bytes, sha256(bytes), limits, cancel)
}

/// `open_fig` after the caller already bounded the input and hashed it.
pub(crate) fn open_with_hash(
    bytes: &[u8],
    source_sha256: Sha256Bytes,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<FigContainer> {
    check(cancel)?;
    let (packaging, parts) = if bytes.starts_with(&SIG_LOCAL.to_le_bytes()) {
        let entries = read_zip(bytes, limits, cancel)?;
        let parts = entries
            .into_iter()
            .map(|(name, content)| make_part(name, content))
            .collect::<Vec<_>>();
        (Packaging::Zip, parts)
    } else {
        (
            Packaging::Bare,
            vec![make_part(CANVAS_PART.to_owned(), bytes.to_vec())],
        )
    };
    check(cancel)?;
    let canvas = parts
        .iter()
        .position(|p| p.name == CANVAS_PART)
        .ok_or(FigError::NotFig("canvas_missing"))?;
    let (kind, version, chunks) = parse_canvas(&parts[canvas].bytes, limits)?;
    if packaging == Packaging::Bare && matches!(kind, FigKind::Unknown(_)) {
        return Err(FigError::NotFig("prelude_unknown"));
    }
    let meta = read_meta(parts.iter().find(|p| p.name == META_PART), limits)?;
    Ok(FigContainer {
        packaging,
        kind,
        version,
        source_sha256,
        parts,
        canvas,
        chunks,
        meta,
    })
}
