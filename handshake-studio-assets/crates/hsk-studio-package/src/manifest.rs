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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub name: String,
    pub kind: EntryKind,
    pub sha256_hex: String,
    pub len: u64,
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
                EntryKind::Asset => entry.name.strip_prefix(ASSET_PREFIX) == Some(&entry.sha256_hex),
                EntryKind::Preview => entry.name.starts_with(PREVIEW_PREFIX),
                EntryKind::Opaque => entry.name != DOCUMENT_NAME,
            };
            if !consistent {
                return Err(PackageError::ManifestInvalid);
            }
        }
        if documents != 1 {
            return Err(PackageError::ManifestInvalid);
        }
        validate_name_set(self.entries.iter().map(|e| e.name.as_str()))
    }
}
