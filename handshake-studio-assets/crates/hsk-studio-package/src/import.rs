//! Typed import entries (SMB-CONVERT-NATIVE CN-2, STU-IO-001/005): what a converted document keeps
//! of its import next to the document: the standalone records the importer produced, the import
//! receipt (loss report and provenance) and the original source bytes.
//!
//! They travel as manifest `opaque` entries under the reserved name `import/<role>.<sha256hex>`, so
//! the manifest schema, the canonical-form rules and every existing consumer stay unchanged (a
//! consumer that does not know them preserves them verbatim like any opaque entry, which keeps them
//! across its re-save). The package recognises the reserved names and enforces, on write and on
//! read before any body is decoded: the name grammar (`manifest_invalid`), the name digest equal to
//! the entry's SHA-256 (`hash_mismatch`), and records/receipt at most `Limits::max_manifest_bytes`
//! (`entry_too_large`); the body hash is verified like every entry. A reserved entry that passes is
//! returned in `Opened::imports`, not in `Opened::quarantined`, and adds no loss note (tests in
//! `tests/import_entries.rs`). The package never interprets the bytes: their schemas belong to the
//! producer (records: Folio; receipt and original: the importer that wrote them).
use crate::{
    error::PackageError, hashing::{is_hex64, sha256_hex_of}, limits::Limits, read::Quarantined,
};

/// Reserved name prefix of typed import entries.
pub const IMPORT_PREFIX: &str = "import/";

/// What an import entry holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImportRole {
    /// Standalone records of the import (for example Folio `StudioRasterRecords` JSON).
    Records,
    /// The import receipt: loss report and provenance.
    Receipt,
    /// The original source file, byte-identical.
    Original,
}

impl ImportRole {
    /// Name token: `records`, `receipt` or `original`.
    pub const fn token(self) -> &'static str {
        match self {
            Self::Records => "records",
            Self::Receipt => "receipt",
            Self::Original => "original",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "records" => Some(Self::Records),
            "receipt" => Some(Self::Receipt),
            "original" => Some(Self::Original),
            _ => None,
        }
    }

    /// Records and receipts are metadata: bounded like the manifest. The original is bounded by
    /// `Limits::max_entry_bytes` like every entry.
    const fn is_metadata(self) -> bool {
        !matches!(self, Self::Original)
    }
}

/// One typed import entry with its verified bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportEntry {
    pub role: ImportRole,
    /// Lowercase SHA-256 hex of `bytes`.
    pub sha256_hex: String,
    pub bytes: Vec<u8>,
}

impl ImportEntry {
    pub fn new(role: ImportRole, bytes: Vec<u8>) -> Self {
        Self { role, sha256_hex: sha256_hex_of(&bytes), bytes }
    }

    /// The reserved archive name `import/<role>.<sha256hex>`.
    pub fn name(&self) -> String {
        format!("{IMPORT_PREFIX}{}.{}", self.role.token(), self.sha256_hex)
    }

    /// The entry as an opaque entry for [`crate::Extras::quarantined`] (how it is written).
    pub fn to_opaque(&self) -> Quarantined {
        Quarantined { name: self.name(), bytes: self.bytes.clone(), sha256_hex: self.sha256_hex.clone() }
    }
}

/// `Ok(None)` for a name outside the reserved prefix; `Ok(Some((role, digest)))` for a well-formed
/// reserved name; `manifest_invalid` for any other name under the prefix.
pub(crate) fn parse_import_name(name: &str) -> Result<Option<(ImportRole, &str)>, PackageError> {
    let Some(rest) = name.strip_prefix(IMPORT_PREFIX) else {
        return Ok(None);
    };
    let (token, digest) = rest.split_once('.').ok_or(PackageError::ManifestInvalid)?;
    let role = ImportRole::from_token(token).ok_or(PackageError::ManifestInvalid)?;
    if !is_hex64(digest) {
        return Err(PackageError::ManifestInvalid);
    }
    Ok(Some((role, digest)))
}

/// Checks an opaque entry against the reserved-name rules; `Some(role)` when it is an import entry.
/// `sha256_hex` is the entry's content identity and `len` its decoded length.
pub(crate) fn check_import(
    name: &str,
    sha256_hex: &str,
    len: u64,
    limits: &Limits,
) -> Result<Option<ImportRole>, PackageError> {
    let Some((role, digest)) = parse_import_name(name)? else {
        return Ok(None);
    };
    if digest != sha256_hex {
        return Err(PackageError::HashMismatch);
    }
    if role.is_metadata() && len > limits.max_manifest_bytes {
        return Err(PackageError::EntryTooLarge);
    }
    Ok(Some(role))
}
