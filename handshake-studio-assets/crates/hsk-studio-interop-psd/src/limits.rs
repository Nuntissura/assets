//! Caller-supplied bounds. Every allocation derived from file content is checked against these
//! before it happens.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Whole input size.
    pub max_input_bytes: usize,
    pub max_layers: usize,
    /// Pixel count of the header canvas and of any layer or mask rectangle.
    pub max_pixels: u64,
    /// Bytes produced by one decode call (all planes of one channel or of the merged image).
    pub max_decoded_bytes: usize,
    /// Single tagged-block payload.
    pub max_tagged_block_bytes: usize,
    /// Tagged blocks per container (one layer, or the global list).
    pub max_tagged_blocks: usize,
    /// UTF-16 code units in a Unicode string.
    pub max_name_units: usize,
    pub max_resources: usize,
    pub max_group_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 512 * 1024 * 1024,
            max_layers: 8192,
            max_pixels: 1 << 28,
            max_decoded_bytes: 1 << 30,
            max_tagged_block_bytes: 256 * 1024 * 1024,
            max_tagged_blocks: 16_384,
            max_name_units: 1024,
            max_resources: 65_536,
            max_group_depth: 128,
        }
    }
}
