//! Typed, payload-free errors. Codes are fixed snake_case strings; no source bytes or names ever
//! reach an error value.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PsdError {
    Canceled,
    InputTooLarge,
    Truncated(&'static str),
    BadSignature,
    UnsupportedVersion(u16),
    BadReserved,
    BadChannelCount,
    BadDimensions,
    BadDepth,
    BadColorMode,
    SectionLength(&'static str),
    LayerLimit,
    PixelLimit,
    TaggedBlockLimit,
    NameLimit,
    ResourceLimit,
    LayerRect,
    ChannelLength,
    BadCompression(u16),
    RleRowCounts,
    RleOverflow,
    RleShort,
    ZipFailed,
    ChannelSize,
    GroupUnbalanced,
    GroupDepth,
    Overflow,
    UnsupportedMode(&'static str),
    Encode(&'static str),
}

impl PsdError {
    /// Stable code for diagnostics. Never contains input-derived text.
    pub fn code(self) -> &'static str {
        match self {
            Self::Canceled => "psd_canceled",
            Self::InputTooLarge => "psd_input_too_large",
            Self::Truncated(_) => "psd_truncated",
            Self::BadSignature => "psd_bad_signature",
            Self::UnsupportedVersion(_) => "psd_unsupported_version",
            Self::BadReserved => "psd_bad_reserved",
            Self::BadChannelCount => "psd_bad_channel_count",
            Self::BadDimensions => "psd_bad_dimensions",
            Self::BadDepth => "psd_bad_depth",
            Self::BadColorMode => "psd_bad_color_mode",
            Self::SectionLength(_) => "psd_section_length",
            Self::LayerLimit => "psd_layer_limit",
            Self::PixelLimit => "psd_pixel_limit",
            Self::TaggedBlockLimit => "psd_tagged_block_limit",
            Self::NameLimit => "psd_name_limit",
            Self::ResourceLimit => "psd_resource_limit",
            Self::LayerRect => "psd_layer_rect",
            Self::ChannelLength => "psd_channel_length",
            Self::BadCompression(_) => "psd_bad_compression",
            Self::RleRowCounts => "psd_rle_row_counts",
            Self::RleOverflow => "psd_rle_overflow",
            Self::RleShort => "psd_rle_short",
            Self::ZipFailed => "psd_zip_failed",
            Self::ChannelSize => "psd_channel_size",
            Self::GroupUnbalanced => "psd_group_unbalanced",
            Self::GroupDepth => "psd_group_depth",
            Self::Overflow => "psd_overflow",
            Self::UnsupportedMode(_) => "psd_unsupported_mode",
            Self::Encode(_) => "psd_encode",
        }
    }
}

impl fmt::Display for PsdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated(w) | Self::SectionLength(w) | Self::UnsupportedMode(w) | Self::Encode(w) => {
                write!(f, "{} ({w})", self.code())
            }
            Self::UnsupportedVersion(v) => write!(f, "{} ({v})", self.code()),
            Self::BadCompression(c) => write!(f, "{} ({c})", self.code()),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for PsdError {}

impl From<hsk_studio_accord::ValidationError> for PsdError {
    fn from(_: hsk_studio_accord::ValidationError) -> Self {
        Self::Canceled
    }
}

pub type Result<T> = std::result::Result<T, PsdError>;
