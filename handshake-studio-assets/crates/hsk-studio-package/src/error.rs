//! Typed, content-free errors: stable codes only, never entry names, paths or document text.
use hsk_studio_folio as folio;
use std::fmt;

/// Failure of the caller-granted asset resolver port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetError {
    Absent,
    Unauthorized,
    TooLarge,
}

impl AssetError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Absent => "asset_absent",
            Self::Unauthorized => "asset_unauthorized",
            Self::TooLarge => "asset_too_large",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackageError {
    Canceled,
    Io(std::io::ErrorKind),
    TooManyEntries,
    EntryTooLarge,
    TotalTooLarge,
    NameTooLong,
    /// Structurally invalid archive; the detail is a fixed diagnostic token.
    Malformed(&'static str),
    UnsafeName,
    DuplicateName,
    CaseFoldDuplicate,
    PathConflict,
    UnsupportedCompression,
    UnsupportedFeature,
    CrcMismatch,
    LengthMismatch,
    HashMismatch,
    ManifestInvalid,
    ManifestNonCanonical,
    UnsupportedVersion(u64),
    MissingEntry,
    MissingAsset,
    DocumentMismatch,
    InvalidDigest,
    Asset(AssetError),
    Document(folio::Diagnostic),
    ReadbackMismatch,
    DestinationInvalid,
}

impl PackageError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::Io(_) => "io",
            Self::TooManyEntries => "too_many_entries",
            Self::EntryTooLarge => "entry_too_large",
            Self::TotalTooLarge => "total_too_large",
            Self::NameTooLong => "name_too_long",
            Self::Malformed(_) => "malformed_archive",
            Self::UnsafeName => "unsafe_name",
            Self::DuplicateName => "duplicate_name",
            Self::CaseFoldDuplicate => "case_fold_duplicate",
            Self::PathConflict => "path_conflict",
            Self::UnsupportedCompression => "unsupported_compression",
            Self::UnsupportedFeature => "unsupported_feature",
            Self::CrcMismatch => "crc_mismatch",
            Self::LengthMismatch => "length_mismatch",
            Self::HashMismatch => "hash_mismatch",
            Self::ManifestInvalid => "manifest_invalid",
            Self::ManifestNonCanonical => "manifest_non_canonical",
            Self::UnsupportedVersion(_) => "unsupported_version",
            Self::MissingEntry => "missing_entry",
            Self::MissingAsset => "missing_asset",
            Self::DocumentMismatch => "document_mismatch",
            Self::InvalidDigest => "invalid_digest",
            Self::Asset(e) => e.code(),
            Self::Document(_) => "document_rejected",
            Self::ReadbackMismatch => "readback_mismatch",
            Self::DestinationInvalid => "destination_invalid",
        }
    }
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for PackageError {}
