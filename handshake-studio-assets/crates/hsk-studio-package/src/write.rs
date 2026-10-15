//! Deterministic container writer. All inputs are gathered and verified before the first byte
//! is written, so a failure (absent asset, limit, name collision) never produces partial output.
use crate::{
    error::{AssetError, PackageError},
    hashing::{check, sha256_hex, sha256_hex_of},
    limits::Limits,
    manifest::{
        ASSET_PREFIX, CONTAINER_VERSION, Codec, DOCUMENT_NAME, EntryKind, MANIFEST_NAME, Manifest,
        ManifestEntry, PREVIEW_PREFIX, PreviewInfo,
    },
    read::{Opened, PreviewImage, Quarantined},
    references::referenced_assets,
    shuffle::byte_shuffle,
    zip::{Zip64Policy, ZipWriter, validate_name, validate_name_set},
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{ContentDigest, Snapshot};
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    io::{Read, Write},
};

/// Caller-granted asset byte provider. The package never owns a catalog or a second CAS: bytes
/// are streamed from whichever resolver the caller grants (ArtifactService port in the host,
/// [`MemoryAssetSource`] in standalone fixtures).
pub trait AssetSource {
    /// Returns the bytes for `digest`, at most `max` bytes.
    fn read(&self, digest: &ContentDigest, max: u64) -> Result<Vec<u8>, AssetError>;
}

/// One asset opened for streaming: its exact byte length and a reader over its bytes.
///
/// The length is a declaration the writer holds the reader to: a stream that yields more or fewer
/// bytes fails with `length_mismatch`, and the bytes must hash to the requested SHA-256 digest
/// (`hash_mismatch` otherwise). Nothing is trusted from the provider.
pub struct AssetReader<'a> {
    /// Exact number of bytes `reader` yields.
    pub len: u64,
    pub reader: Box<dyn Read + 'a>,
}

/// Streaming counterpart of [`AssetSource`] for [`crate::write_package_streaming`] and
/// [`crate::save_atomic_streaming`]: bytes are pulled through a bounded reader instead of being
/// returned as one buffer, so a raw blob is never resident in the writer.
///
/// `open` may be called more than once for the same digest (a codec-compressed blob is read once
/// to plan the manifest and once to write it); every call must yield the same bytes. The writer
/// has one reader open at a time.
pub trait AssetStream {
    /// Opens `digest` for reading; `max` is the largest stored entry the writer will accept.
    fn open(&self, digest: &ContentDigest, max: u64) -> Result<AssetReader<'_>, AssetError>;
}

/// Scoped in-memory resolver input for standalone fixtures.
#[derive(Clone, Debug, Default)]
pub struct MemoryAssetSource {
    blobs: BTreeMap<String, Vec<u8>>,
}

impl MemoryAssetSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `bytes` and returns the SHA-256 identity to reference from a tile.
    pub fn insert(&mut self, bytes: Vec<u8>) -> ContentDigest {
        let digest = sha256_hex_of(&bytes);
        self.blobs.insert(digest.clone(), bytes);
        ContentDigest {
            algorithm: "sha256".to_owned(),
            digest,
        }
    }

    /// Re-grants the verified assets of an opened package (reopen then re-save).
    pub fn from_opened(opened: &Opened) -> Self {
        Self {
            blobs: opened.assets.clone(),
        }
    }

    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }
}

impl AssetSource for MemoryAssetSource {
    fn read(&self, digest: &ContentDigest, max: u64) -> Result<Vec<u8>, AssetError> {
        if digest.algorithm != "sha256" {
            return Err(AssetError::Absent);
        }
        match self.blobs.get(&digest.digest) {
            None => Err(AssetError::Absent),
            Some(b) if b.len() as u64 > max => Err(AssetError::TooLarge),
            Some(b) => Ok(b.clone()),
        }
    }
}

impl AssetStream for MemoryAssetSource {
    fn open(&self, digest: &ContentDigest, max: u64) -> Result<AssetReader<'_>, AssetError> {
        if digest.algorithm != "sha256" {
            return Err(AssetError::Absent);
        }
        match self.blobs.get(&digest.digest) {
            None => Err(AssetError::Absent),
            Some(b) if b.len() as u64 > max => Err(AssetError::TooLarge),
            Some(b) => Ok(AssetReader {
                len: b.len() as u64,
                reader: Box::new(b.as_slice()),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteOptions {
    pub zip64: Zip64Policy,
    /// Codec requested for asset blobs. The smallest of raw, zstd and (for `ShuffleZstd`, when the
    /// tile format names a 2/4/8-byte element such as `rgb_f32le` or `coverage_u16le`) shuffle+zstd
    /// is chosen per blob; previews use plain zstd when this is not `Raw`.
    pub asset_codec: Codec,
}

/// Optional entries written next to the document and its assets.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extras<'a> {
    /// Preserved unknown entries, re-emitted verbatim as `opaque`.
    pub quarantined: &'a [Quarantined],
    /// Optional preview images (derived data, content-addressed, never decoded by readers that
    /// do not ask for them).
    pub previews: &'a [PreviewImage],
}

struct Part<'a> {
    name: String,
    kind: EntryKind,
    codec: Codec,
    raw_len: Option<u64>,
    elem_size: Option<u8>,
    preview: Option<PreviewInfo>,
    /// SHA-256 of the decoded bytes (content identity).
    sha: String,
    stored: Cow<'a, [u8]>,
}

impl<'a> Part<'a> {
    fn raw(name: String, kind: EntryKind, sha: String, stored: Cow<'a, [u8]>) -> Self {
        Self {
            name,
            kind,
            codec: Codec::Raw,
            raw_len: None,
            elem_size: None,
            preview: None,
            sha,
            stored,
        }
    }
}

pub(crate) struct Encoded {
    pub(crate) codec: Codec,
    pub(crate) raw_len: Option<u64>,
    pub(crate) elem_size: Option<u8>,
    pub(crate) stored: Vec<u8>,
}

/// Smallest of raw / zstd / shuffle+zstd for `bytes` (ties keep the simpler codec).
pub(crate) fn encode_blob(
    requested: Codec,
    elem: Option<u8>,
    bytes: Vec<u8>,
    cancel: &CancellationToken,
) -> Result<Encoded, PackageError> {
    let raw_len = bytes.len() as u64;
    let mut best_len = bytes.len();
    let mut best: Option<(Codec, Option<u8>, Vec<u8>)> = None;
    if requested != Codec::Raw {
        check(cancel)?;
        let plain = compress_to_vec(&bytes[..], CompressionLevel::Fastest);
        if plain.len() < best_len {
            best_len = plain.len();
            best = Some((Codec::Zstd, None, plain));
        }
        if let (Codec::ShuffleZstd, Some(size)) = (requested, elem) {
            check(cancel)?;
            let shuffled = byte_shuffle(&bytes, usize::from(size));
            let packed = compress_to_vec(&shuffled[..], CompressionLevel::Fastest);
            if packed.len() < best_len {
                best = Some((Codec::ShuffleZstd, Some(size), packed));
            }
        }
    }
    Ok(match best {
        Some((codec, elem_size, stored)) => Encoded {
            codec,
            raw_len: Some(raw_len),
            elem_size,
            stored,
        },
        None => Encoded {
            codec: Codec::Raw,
            raw_len: None,
            elem_size: None,
            stored: bytes,
        },
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteReceipt {
    pub entries: u32,
    pub bytes_written: u64,
    pub doc_revision: u64,
    /// Whether ZIP64 end records were emitted.
    pub zip64: bool,
}

/// Writes `doc` (verbatim canonical bytes) plus every raster tile asset it references.
pub fn write_package<W: Write>(
    doc: &Snapshot,
    assets: &dyn AssetSource,
    limits: &Limits,
    cancel: &CancellationToken,
    out: &mut W,
) -> Result<WriteReceipt, PackageError> {
    write_package_with(
        doc,
        assets,
        &Extras::default(),
        &WriteOptions::default(),
        limits,
        cancel,
        out,
    )
}

/// As [`write_package`], additionally writing [`Extras`] (preserved unknown entries, previews)
/// under explicit [`WriteOptions`].
pub fn write_package_with<W: Write>(
    doc: &Snapshot,
    assets: &dyn AssetSource,
    extras: &Extras<'_>,
    options: &WriteOptions,
    limits: &Limits,
    cancel: &CancellationToken,
    out: &mut W,
) -> Result<WriteReceipt, PackageError> {
    check(cancel)?;
    let document = doc.document();
    let doc_bytes = doc.encoded_bytes();

    let mut parts: Vec<Part<'_>> = Vec::new();
    for (digest, referenced) in referenced_assets(document, extras.quarantined)? {
        check(cancel)?;
        let reference = ContentDigest {
            algorithm: "sha256".to_owned(),
            digest: digest.clone(),
        };
        let bytes = assets
            .read(&reference, limits.max_entry_bytes)
            .map_err(PackageError::Asset)?;
        if bytes.len() as u64 > limits.max_entry_bytes {
            return Err(PackageError::Asset(AssetError::TooLarge));
        }
        if sha256_hex(&bytes, cancel)? != digest {
            return Err(PackageError::HashMismatch);
        }
        let encoded = encode_blob(options.asset_codec, referenced.elem_size, bytes, cancel)?;
        parts.push(Part {
            name: format!("{ASSET_PREFIX}{digest}"),
            kind: EntryKind::Asset,
            codec: encoded.codec,
            raw_len: encoded.raw_len,
            elem_size: encoded.elem_size,
            preview: None,
            sha: digest,
            stored: Cow::Owned(encoded.stored),
        });
    }
    for held in extras.quarantined {
        let sha = sha256_hex(&held.bytes, cancel)?;
        crate::import::check_import(&held.name, &sha, held.bytes.len() as u64, limits)?;
        parts.push(Part::raw(
            held.name.clone(),
            EntryKind::Opaque,
            sha,
            Cow::Borrowed(held.bytes.as_slice()),
        ));
    }
    for image in extras.previews {
        check(cancel)?;
        if !image.info.valid(limits) || image.pixels.len() as u64 != image.info.byte_len() {
            return Err(PackageError::PreviewMismatch);
        }
        let sha = sha256_hex(&image.pixels, cancel)?;
        let plain = (options.asset_codec != Codec::Raw)
            .then(|| compress_to_vec(&image.pixels[..], CompressionLevel::Fastest))
            .filter(|plain| plain.len() < image.pixels.len());
        let (codec, raw_len, stored) = match plain {
            Some(plain) => (
                Codec::Zstd,
                Some(image.pixels.len() as u64),
                Cow::Owned(plain),
            ),
            None => (Codec::Raw, None, Cow::Borrowed(image.pixels.as_slice())),
        };
        parts.push(Part {
            name: format!("{PREVIEW_PREFIX}{sha}"),
            kind: EntryKind::Preview,
            codec,
            raw_len,
            elem_size: None,
            preview: Some(image.info.clone()),
            sha,
            stored,
        });
    }
    parts.push(Part::raw(
        DOCUMENT_NAME.to_owned(),
        EntryKind::Document,
        sha256_hex(doc_bytes, cancel)?,
        Cow::Borrowed(doc_bytes),
    ));
    parts.sort_by(|a, b| a.name.cmp(&b.name));

    validate_name_set(
        parts
            .iter()
            .map(|p| p.name.as_str())
            .chain(std::iter::once(MANIFEST_NAME)),
    )?;

    let entries = parts
        .iter()
        .map(|p| ManifestEntry {
            name: p.name.clone(),
            kind: p.kind,
            sha256_hex: p.sha.clone(),
            len: p.stored.len() as u64,
            codec: p.codec,
            raw_len: p.raw_len,
            elem_size: p.elem_size,
            preview: p.preview.clone(),
        })
        .collect();
    let manifest = Manifest {
        container_version: CONTAINER_VERSION,
        document_id: document.document_id.clone(),
        revision: document.revision,
        entries,
    };
    let manifest_bytes = manifest.to_canonical_bytes()?;
    if manifest_bytes.len() as u64 > limits.max_manifest_bytes {
        return Err(PackageError::EntryTooLarge);
    }
    if parts.len() + 1 > limits.max_entries as usize {
        return Err(PackageError::TooManyEntries);
    }
    // Pre-validate everything the zip writer would check so failure never leaves partial output.
    let mut total = manifest_bytes.len() as u64;
    for part in &parts {
        validate_name(part.name.as_bytes(), limits.max_name_len)?;
        let len = part.stored.len() as u64;
        if len > limits.max_entry_bytes {
            return Err(PackageError::EntryTooLarge);
        }
        total = total
            .checked_add(len)
            .filter(|t| *t <= limits.max_total_bytes)
            .ok_or(PackageError::TotalTooLarge)?;
    }

    let mut zip = ZipWriter::new(out, *limits, options.zip64);
    zip.add(MANIFEST_NAME, &manifest_bytes, cancel)?;
    // Physical order: manifest first, then document, then the remaining entries by name.
    for part in parts.iter().filter(|p| p.name == DOCUMENT_NAME) {
        zip.add(&part.name, &part.stored, cancel)?;
    }
    for part in parts.iter().filter(|p| p.name != DOCUMENT_NAME) {
        zip.add(&part.name, &part.stored, cancel)?;
    }
    let stats = zip.finish(cancel)?;
    Ok(WriteReceipt {
        entries: stats.entries,
        bytes_written: stats.bytes_written,
        doc_revision: document.revision,
        zip64: stats.zip64,
    })
}
