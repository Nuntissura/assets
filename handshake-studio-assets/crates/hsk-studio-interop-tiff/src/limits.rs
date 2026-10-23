//! Caller-owned decode budgets. Every limit is checked before the allocation or read it guards.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_input_bytes: u64,
    pub max_pages: u32,
    pub max_ifd_entries: u32,
    /// Largest accepted ImageWidth / ImageLength.
    pub max_dimension: u32,
    pub max_samples_per_pixel: u16,
    /// Decoded bytes of one page.
    pub max_page_decoded_bytes: u64,
    /// Decoded bytes summed over all pages (checked from declared sizes before any decode).
    pub max_total_decoded_bytes: u64,
    /// One tag value (ICC, tag 37724 and 34377 included).
    pub max_tag_bytes: u64,
    /// Strips/tiles per page.
    pub max_chunks: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1 << 30,
            max_pages: 256,
            max_ifd_entries: 4096,
            max_dimension: 1 << 18,
            max_samples_per_pixel: 64,
            max_page_decoded_bytes: 256 << 20,
            max_total_decoded_bytes: 1 << 30,
            max_tag_bytes: 64 << 20,
            max_chunks: 1 << 22,
        }
    }
}

/// Upper bound the encoder accepts for one image (samples x sample bytes).
pub const MAX_ENCODE_BYTES: u64 = 1 << 30;
