//! Deterministic container writer. All inputs are gathered and verified before the first byte
//! is written, so a failure (absent asset, limit, name collision) never produces partial output.
use crate::{
    error::{AssetError, PackageError},
    hashing::{check, sha256_hex, sha256_hex_of},
    limits::Limits,
    manifest::{
        ASSET_PREFIX, CONTAINER_VERSION, Codec, DOCUMENT_NAME, EntryKind, MANIFEST_NAME, Manifest,
        ManifestEntry,
    },
    read::{Opened, Quarantined, tile_digests},
    zip::{Zip64Policy, ZipWriter, validate_name, validate_name_set},
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{ContentDigest, Snapshot};
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use std::{borrow::Cow, collections::BTreeMap, io::Write};

/// Caller-granted asset byte provider. The package never owns a catalog or a second CAS: bytes
/// are streamed from whichever resolver the caller grants (ArtifactService port in the host,
/// [`MemoryAssetSource`] in standalone fixtures).
pub trait AssetSource {
    /// Returns the bytes for `digest`, at most `max` bytes.
    fn read(&self, digest: &ContentDigest, max: u64) -> Result<Vec<u8>, AssetError>;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteOptions {
    pub zip64: Zip64Policy,
    /// Codec requested for asset blobs; a blob that would not shrink is stored raw.
    pub asset_codec: Codec,
}

struct Part<'a> {
    name: String,
    kind: EntryKind,
    codec: Codec,
    raw_len: Option<u64>,
    /// SHA-256 of the decoded bytes (content identity).
    sha: String,
    stored: Cow<'a, [u8]>,
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
        &[],
        &WriteOptions::default(),
        limits,
        cancel,
        out,
    )
}

/// As [`write_package`], additionally re-emitting preserved `quarantined` entries verbatim
/// (listed in the manifest as `opaque`) and with explicit [`WriteOptions`].
pub fn write_package_with<W: Write>(
    doc: &Snapshot,
    assets: &dyn AssetSource,
    quarantined: &[Quarantined],
    options: &WriteOptions,
    limits: &Limits,
    cancel: &CancellationToken,
    out: &mut W,
) -> Result<WriteReceipt, PackageError> {
    check(cancel)?;
    let document = doc.document();
    let doc_bytes = doc.encoded_bytes();

    let mut parts: Vec<Part<'_>> = Vec::new();
    for digest in tile_digests(document)? {
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
        let name = format!("{ASSET_PREFIX}{digest}");
        // Per-blob choice: compress only when it actually shrinks the stored bytes.
        let packed = (options.asset_codec == Codec::Zstd)
            .then(|| compress_to_vec(&bytes[..], CompressionLevel::Fastest))
            .filter(|packed| packed.len() < bytes.len());
        parts.push(match packed {
            Some(packed) => Part {
                name,
                kind: EntryKind::Asset,
                codec: Codec::Zstd,
                raw_len: Some(bytes.len() as u64),
                sha: digest,
                stored: Cow::Owned(packed),
            },
            None => Part {
                name,
                kind: EntryKind::Asset,
                codec: Codec::Raw,
                raw_len: None,
                sha: digest,
                stored: Cow::Owned(bytes),
            },
        });
    }
    for held in quarantined {
        parts.push(Part {
            name: held.name.clone(),
            kind: EntryKind::Opaque,
            codec: Codec::Raw,
            raw_len: None,
            sha: sha256_hex(&held.bytes, cancel)?,
            stored: Cow::Borrowed(held.bytes.as_slice()),
        });
    }
    parts.push(Part {
        name: DOCUMENT_NAME.to_owned(),
        kind: EntryKind::Document,
        codec: Codec::Raw,
        raw_len: None,
        sha: sha256_hex(doc_bytes, cancel)?,
        stored: Cow::Borrowed(doc_bytes),
    });
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
