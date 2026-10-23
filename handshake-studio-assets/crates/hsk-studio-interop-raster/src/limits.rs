//! Caller-owned decode budgets. Every limit is checked before the allocation or read it guards.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_input_bytes: u64,
    /// Largest accepted width or height (checked from the header before any decode).
    pub max_dimension: u32,
    /// Running aggregate of every buffer this crate or the codec allocates for one decode, computed from
    /// the declared header values before decode (output samples, plus the codec's internal coefficient /
    /// row buffers where they scale with the image).
    pub max_decoded_bytes: u64,
    /// Embedded ICC profile (PNG iCCP inflated, JPEG APP2 reassembled).
    pub max_icc_bytes: u64,
    /// Other ancillary metadata the codec buffers (PNG ancillary chunks, JPEG APP1 Exif).
    pub max_metadata_bytes: u64,
    /// JPEG scans (SOS segments); progressive files use several.
    pub max_jpeg_scans: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1 << 30,
            max_dimension: 1 << 16,
            max_decoded_bytes: 1 << 30,
            max_icc_bytes: 16 << 20,
            max_metadata_bytes: 16 << 20,
            max_jpeg_scans: 256,
        }
    }
}

/// Upper bound the encoders accept for one image (samples x sample bytes).
pub const MAX_ENCODE_BYTES: u64 = 1 << 30;
