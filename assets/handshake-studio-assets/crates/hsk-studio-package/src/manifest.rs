//! Typed canonical manifest. Parsing is strict: the exact canonical serialisation of the parsed
//! value must equal the input bytes, which rejects duplicate keys, reordering, unknown fields,
//! whitespace variants and unsorted/duplicate entries in one comparison.
use crate::{
    error::PackageError,
    hashing::is_hex64,
    limits::Limits,
    zip::{validate_name, validate_name_set},
};
use serde::{Deserialize, Serialize};

pub const CONTAINER_VERSION: u32 = 1;
pub const MANIFEST_NAME: &str = "manifest.json";
pub const DOCUMENT_NAME: &str = "document.json";
pub const ASSET_PREFIX: &str = "assets/";
pub const PREVIEW_PREFIX: &str = "previews/";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Document,
    Asset,
    Preview,
    Opaque,
}

/// Blob codec id. `Raw` is the implicit default and is never written to the manifest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    #[default]
    Raw,
    /// One zstd frame over the raw bytes.
    Zstd,
    /// Byte-plane shuffle (element size in `elem_size`) then one zstd frame: groups the
    /// high-entropy and low-entropy bytes of f32/f16/u16 samples so smooth data compresses.
    ShuffleZstd,
}

impl Codec {
    pub fn is_raw(&self) -> bool {
        matches!(self, Self::Raw)
    }
}

/// Pixel layout of a preview image: tightly packed rows, 8 bits per channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    Gray8,
    Rgb8,
    Rgba8,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> u64 {
        match self {
            Self::Gray8 => 1,
            Self::Rgb8 => 3,
            Self::Rgba8 => 4,
        }
    }
}

/// Declared description of a preview image; the pixels are never decoded by the package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewInfo {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    /// Caller-defined colour tag (1..=64 chars of `[A-Za-z0-9._:-]`), e.g. a profile id.
    pub color: String,
}

impl PreviewInfo {
    /// Exact decoded byte size implied by format and dimensions.
    pub fn byte_len(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * self.format.bytes_per_pixel()
    }

    pub(crate) fn valid(&self, limits: &Limits) -> bool {
        let dim = |d: u32| (1..=limits.max_preview_dimension).contains(&d);
        dim(self.width)
            && dim(self.height)
            && self.byte_len() <= limits.max_preview_bytes
            && (1..=64).contains(&self.color.len())
            && self
                .color
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    }
}

/// One row of the blob table. `len` is the stored (archive entry) length; `sha256_hex` is the
/// identity of the decoded bytes; `raw_len` is the decoded length of a compressed blob;
/// `elem_size` is the shuffle element size; `preview` describes a preview image.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub name: String,
    pub kind: EntryKind,
    pub sha256_hex: String,
    pub len: u64,
    #[serde(default, skip_serializing_if = "Codec::is_raw")]
    pub codec: Codec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elem_size: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<PreviewInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub container_version: u32,
    pub document_id: String,
    pub revision: u64,
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    /// Canonical bytes: fixed field order, no whitespace, entries as stored (writer sorts them).
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, PackageError> {
        serde_json::to_vec(self).map_err(|_| PackageError::ManifestInvalid)
    }

    /// Strict parse. An unknown `container_version` is reported before any other interpretation
    /// of the document, so a future manifest layout is never half-read.
    pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Self, PackageError> {
        if bytes.len() as u64 > limits.max_manifest_bytes {
            return Err(PackageError::EntryTooLarge);
        }
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| PackageError::ManifestInvalid)?;
        let version = value
            .get("container_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or(PackageError::ManifestInvalid)?;
        if version != u64::from(CONTAINER_VERSION) {
            return Err(PackageError::UnsupportedVersion(version));
        }
        let manifest: Self =
            serde_json::from_value(value).map_err(|_| PackageError::ManifestInvalid)?;
        if manifest.entries.len() > limits.max_entries as usize {
            return Err(PackageError::TooManyEntries);
        }
        if manifest.to_canonical_bytes()? != bytes {
            return Err(PackageError::ManifestNonCanonical);
        }
        manifest.validate(limits)?;
        Ok(manifest)
    }

    fn validate(&self, limits: &Limits) -> Result<(), PackageError> {
        let mut documents = 0usize;
        let mut decoded_total = 0u64;
        let mut previous: Option<&str> = None;
        for entry in &self.entries {
            validate_name(entry.name.as_bytes(), limits.max_name_len)?;
            if previous.is_some_and(|p| p >= entry.name.as_str()) {
                return Err(PackageError::ManifestNonCanonical);
            }
            previous = Some(&entry.name);
            if !is_hex64(&entry.sha256_hex) {
                return Err(PackageError::InvalidDigest);
            }
            if entry.name == MANIFEST_NAME {
                return Err(PackageError::ManifestInvalid);
            }
            let consistent = match entry.kind {
                EntryKind::Document => {
                    documents += 1;
                    entry.name == DOCUMENT_NAME
                }
                EntryKind::Asset => {
                    entry.name.strip_prefix(ASSET_PREFIX) == Some(&entry.sha256_hex)
                }
                EntryKind::Preview => {
                    entry.name.strip_prefix(PREVIEW_PREFIX) == Some(&entry.sha256_hex)
                }
                EntryKind::Opaque => {
                    // A reserved `import/` name is checked before any body is read.
                    crate::import::check_import(
                        &entry.name,
                        &entry.sha256_hex,
                        entry.raw_len.unwrap_or(entry.len),
                        limits,
                    )?;
                    entry.name != DOCUMENT_NAME
                }
            };
            let raw_ok = |n: u64| n <= limits.max_entry_bytes;
            let codec_ok = match (entry.codec, entry.kind) {
                (Codec::Raw, _) => entry.raw_len.is_none() && entry.elem_size.is_none(),
                (Codec::Zstd, EntryKind::Asset | EntryKind::Preview) => {
                    entry.raw_len.is_some_and(raw_ok) && entry.elem_size.is_none()
                }
                (Codec::ShuffleZstd, EntryKind::Asset) => {
                    entry.raw_len.is_some_and(raw_ok) && matches!(entry.elem_size, Some(2 | 4 | 8))
                }
                _ => false,
            };
            let preview_ok = match (entry.kind, &entry.preview) {
                (EntryKind::Preview, Some(info)) => {
                    let decoded = entry.raw_len.unwrap_or(entry.len);
                    info.valid(limits) && decoded == info.byte_len()
                }
                (EntryKind::Preview, None) => false,
                (_, preview) => preview.is_none(),
            };
            if !consistent || !codec_ok || !preview_ok {
                return Err(PackageError::ManifestInvalid);
            }
            // Aggregate decoded bound, from declarations alone: no entry has been decoded yet.
            decoded_total = decoded_total
                .checked_add(entry.raw_len.unwrap_or(entry.len))
                .filter(|total| *total <= limits.max_decoded_total_bytes)
                .ok_or(PackageError::TotalTooLarge)?;
        }
        if documents != 1 {
            return Err(PackageError::ManifestInvalid);
        }
        validate_name_set(self.entries.iter().map(|e| e.name.as_str()))
    }
}
