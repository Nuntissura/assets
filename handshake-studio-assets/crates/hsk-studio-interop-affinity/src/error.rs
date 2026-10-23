/// Why a KA container (or one of its streams) was refused. Codes are stable snake_case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AffinityError {
    /// The bytes do not start with `00 FF 4B 41`.
    NotAffinity,
    /// Shorter than the fixed header (magic, version, flags, class, `#Inf` block).
    HeaderTruncated,
    /// A format version outside 7..=12 (afread reads 7-11; 12 is V3).
    VersionUnsupported(u16),
    /// Header flag bits 0 or 1 set (afread refuses them as an unknown layout).
    FlagsUnsupported(u16),
    /// The `#Inf` tag is not at 0x0C.
    InfoTagMissing,
    /// Version > 7 without the `Prot` tag at 0x40.
    ProtTagMissing,
    /// A directory offset outside the file or inside the fixed header.
    DirectoryOutOfBounds,
    /// A directory offset seen before in the same chain (cyclic offsets).
    DirectoryCycle,
    /// More chained directories than [`crate::Limits::max_directories`].
    DirectoryBudget,
    /// The directory tag is not `#FAT`, `#FT2`, `#FT3` or `#FT4`.
    DirectoryTagInvalid,
    /// A directory record runs past the end of the file.
    DirectoryTruncated,
    /// More entries or folders than the limits allow (checked before allocation).
    EntryBudget,
    /// An entry state other than 0 (named), 1 (unnamed revision) or 2 (deleted).
    EntryStateInvalid,
    /// A name longer than [`crate::Limits::max_name_bytes`] or not UTF-8.
    NameInvalid,
    /// One name bound to two different stream ids.
    StreamIdConflict,
    /// A folder record with a non-zero second length (afread: unknown layout).
    FolderInvalid,
    /// No head stream of that name.
    StreamNotFound,
    /// The stream is deleted at the head revision.
    StreamDeleted,
    /// The stream's bytes lie outside the file (or overlap the fixed header).
    StreamOutOfBounds,
    /// The stream does not start with `#Fil`.
    StreamTagInvalid,
    /// Declared inflated size above [`crate::Limits::max_stream_bytes`].
    StreamTooLarge,
    /// Declared inflated/stored ratio above [`crate::Limits::max_ratio`] (bomb guard).
    StreamRatio,
    /// The sum of decoded streams would exceed [`crate::Limits::max_total_bytes`].
    TotalBudget,
    /// A compression code whose algorithm bits are 3.
    CompressionUnsupported(u8),
    /// zlib stream invalid.
    ZlibInvalid,
    /// The zlib or zstd stream inflates past its declared size.
    InflatedTooLarge,
    /// zstd frame invalid.
    ZstdInvalid,
    /// Inflated length differs from the declared size.
    SizeMismatch,
    /// CRC-32 of the decoded stream differs from the directory record.
    CrcMismatch,
    /// No `FF FF FF FF` + `Thmb` block at the header's thumbnail offset.
    ThumbnailAbsent,
    /// A `Thmb` block whose lengths do not fit the file, or no image in it.
    ThumbnailInvalid,
    /// The embedded image is not a structurally complete PNG/JPEG.
    ThumbnailImageInvalid,
    Canceled,
}

impl AffinityError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAffinity => "affinity_not_ka_container",
            Self::HeaderTruncated => "affinity_header_truncated",
            Self::VersionUnsupported(_) => "affinity_version_unsupported",
            Self::FlagsUnsupported(_) => "affinity_flags_unsupported",
            Self::InfoTagMissing => "affinity_info_tag_missing",
            Self::ProtTagMissing => "affinity_prot_tag_missing",
            Self::DirectoryOutOfBounds => "affinity_directory_out_of_bounds",
            Self::DirectoryCycle => "affinity_directory_cycle",
            Self::DirectoryBudget => "affinity_directory_budget",
            Self::DirectoryTagInvalid => "affinity_directory_tag_invalid",
            Self::DirectoryTruncated => "affinity_directory_truncated",
            Self::EntryBudget => "affinity_entry_budget",
            Self::EntryStateInvalid => "affinity_entry_state_invalid",
            Self::NameInvalid => "affinity_name_invalid",
            Self::StreamIdConflict => "affinity_stream_id_conflict",
            Self::FolderInvalid => "affinity_folder_invalid",
            Self::StreamNotFound => "affinity_stream_not_found",
            Self::StreamDeleted => "affinity_stream_deleted",
            Self::StreamOutOfBounds => "affinity_stream_out_of_bounds",
            Self::StreamTagInvalid => "affinity_stream_tag_invalid",
            Self::StreamTooLarge => "affinity_stream_too_large",
            Self::StreamRatio => "affinity_stream_ratio",
            Self::TotalBudget => "affinity_total_budget",
            Self::CompressionUnsupported(_) => "affinity_compression_unsupported",
            Self::ZlibInvalid => "affinity_zlib_invalid",
            Self::InflatedTooLarge => "affinity_inflated_too_large",
            Self::ZstdInvalid => "affinity_zstd_invalid",
            Self::SizeMismatch => "affinity_size_mismatch",
            Self::CrcMismatch => "affinity_crc_mismatch",
            Self::ThumbnailAbsent => "affinity_thumbnail_absent",
            Self::ThumbnailInvalid => "affinity_thumbnail_invalid",
            Self::ThumbnailImageInvalid => "affinity_thumbnail_image_invalid",
            Self::Canceled => "canceled",
        }
    }
}

impl std::fmt::Display for AffinityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for AffinityError {}

pub type Result<T> = std::result::Result<T, AffinityError>;
