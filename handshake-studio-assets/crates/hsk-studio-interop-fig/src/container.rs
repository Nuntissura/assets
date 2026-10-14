//! Bounded .fig container reader (STU-IO-187): a ZIP (`canvas.fig`, `meta.json`, `thumbnail.png`,
//! `images/<hash>`) or a bare `canvas.fig` stream; the `canvas.fig` prelude (8 ASCII bytes + u32 LE
//! version) and its length-prefixed chunk table. Every ZIP part is kept verbatim with its sha256
//! (opaque preservation is the default); chunks are recorded as ranges of the canvas part.
//!
//! Container facts OBSERVED in three Figma desktop 126.10.7 "Save local copy" files
//! (tests/fixtures, provenance.json): ZIP with `canvas.fig`, `thumbnail.png`, `meta.json` and
//! `images/<40-hex>`; prelude `fig-kiwi`, then u32 LE version (106 in all three), then u32 LE
//! length-prefixed chunks: chunk 0 = Kiwi schema (raw DEFLATE), chunk 1 = message (zstd frame); two
//! chunks, no trailing bytes (test: real_fig_fixtures_container_facts). The `fig-jam.` and
//! `fig-deck` layouts are NOT observed. The reader does not decide what a version
//! means: that is `version_map` (test: unsupported_version_duplicate_key_bomb_cancel opens version 99).
//!
//! ZIP rules: bounded entry count, entry size, total inflated size and expansion ratio; exact and
//! case-folded duplicate names, traversal/absolute/backslash/drive/control names refused; local
//! headers cross-checked against the central directory (encryption flags, method, CRC and sizes; a data
//! descriptor may defer CRC and sizes with all-zero local fields; test:
//! unsupported_version_duplicate_key_bomb_cancel); overlapping entry data refused; CRC-32 verified;
//! DEFLATE data must end exactly at the declared compressed size (bytes after the final block are
//! refused; same test); STORE and DEFLATE only (no ZIP64, encryption or multi-disk). Uses no file
//! system access (no `std::fs` in this crate).
#![deny(clippy::arithmetic_side_effects)]
use crate::{FigError, Result};
use hsk_studio_accord::CancellationToken;
use sha2::{Digest, Sha256};
use hsk_studio_package::{ZipLimits, ZipReadError, read_zip as read_zip_bounded};

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

/// Product family named by the 8-byte `canvas.fig` prelude. `fig-kiwi` is observed in real files;
/// `fig-jam.` and `fig-diff` occur in the installed editor's constants (research/app-inspect-figma.md);
/// `fig-deck` has community-only evidence (no installed-app or file evidence). Anything else is
/// `Unknown` (test: legacy_bare_and_foreign_kinds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FigKind {
    /// `fig-kiwi`
    Design,
    /// `fig-jam.`
    Jam,
    /// `fig-deck`: UNVERIFIED (community notes only; absent from the installed editor bundle).
    Deck,
    /// `fig-diff`: a known editor prelude whose chunk layout is not observed. Known but unsupported:
    /// opens with its identity and bytes preserved, no chunk table, never decoded.
    Diff,
    Unknown([u8; 8]),
}

impl FigKind {
    pub fn from_prelude(prelude: &[u8; 8]) -> Self {
        match prelude {
            b"fig-kiwi" => Self::Design,
            b"fig-jam." => Self::Jam,
            b"fig-deck" => Self::Deck,
            b"fig-diff" => Self::Diff,
            other => Self::Unknown(*other),
        }
    }
    pub const fn prelude(self) -> [u8; 8] {
        match self {
            Self::Design => *b"fig-kiwi",
            Self::Jam => *b"fig-jam.",
            Self::Deck => *b"fig-deck",
            Self::Diff => *b"fig-diff",
            Self::Unknown(p) => p,
        }
    }
    /// Stable label without input-derived text.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Jam => "jam",
            Self::Deck => "deck",
            Self::Diff => "diff",
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
    /// `nodeChanges` elements in the census and in the typed node tree.
    pub max_nodes: u32,
    /// Aggregate decoded values of one value tree (`decode_kiwi`, `decode_message_tree`), plus the
    /// decoded geometry items (path commands, vector-network records) of one typed node tree.
    pub max_tree_values: u64,
    /// Elements of one Kiwi array.
    pub max_array_len: u32,
    /// Bytes of one Kiwi string.
    pub max_string_bytes: usize,
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
            max_tree_values: 16 << 20,
            max_array_len: 16 << 20,
            max_string_bytes: 16 << 20,
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
    /// Empty when the prelude is `Unknown` or `Diff` (the chunk layout is only known for Design/Jam/Deck).
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
const PRELUDE_AND_VERSION: usize = 12;

fn check(cancel: &CancellationToken) -> Result<()> {
    cancel.check().map_err(|_| FigError::Canceled)
}

fn zip_err(e: ZipReadError) -> FigError {
    match e {
        ZipReadError::Canceled => FigError::Canceled,
        ZipReadError::Malformed(code) => FigError::Zip(code),
        ZipReadError::BudgetExceeded(what) => FigError::BudgetExceeded(what),
        ZipReadError::UnsafeName => FigError::UnsafeEntryName,
        ZipReadError::DuplicateName => FigError::DuplicateEntry,
    }
}

/// Reads every file entry as `(name, bytes)` in central-directory order through the bounded reader
/// owned by `hsk-studio-package` (ZIP64 stays refused for `.fig`).
fn read_zip(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Vec<(String, Vec<u8>)>> {
    let zl = ZipLimits {
        max_bytes: limits.max_bytes,
        max_entries: limits.max_entries,
        max_entry_bytes: limits.max_entry_bytes,
        max_inflated_bytes: limits.max_inflated_bytes,
        max_ratio: limits.max_ratio,
        allow_zip64: false,
    };
    read_zip_bounded(bytes, &zl, cancel).map_err(zip_err)
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
    if matches!(kind, FigKind::Unknown(_) | FigKind::Diff) {
        return Ok((kind, version, chunks));
    }
    let mut at = PRELUDE_AND_VERSION;
    while at < canvas.len() {
        let header = at
            .checked_add(4)
            .and_then(|end| canvas.get(at..end))
            .ok_or(FigError::ChunkOverflow)?;
        let len = usize::try_from(u32::from_le_bytes([
            header[0], header[1], header[2], header[3],
        ]))
        .map_err(|_| FigError::ChunkOverflow)?;
        let start = at.checked_add(4).ok_or(FigError::ChunkOverflow)?;
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
