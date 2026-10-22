/// Bounds for untrusted KA containers (STU-IO-001: bound all untrusted materialization). Every count is
/// checked against the bytes left before anything is allocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest container accepted (equal to the harness source bound).
    pub max_input_bytes: u64,
    /// Chained directories (one per saved revision).
    pub max_directories: u32,
    /// Entries plus folders across all directories.
    pub max_entries: u32,
    pub max_name_bytes: u16,
    /// Largest declared inflated stream.
    pub max_stream_bytes: u64,
    /// Sum of every decoded stream in one [`crate::KaContainer::resources`] call.
    pub max_total_bytes: u64,
    /// Largest declared inflated/stored ratio (the V1 test designs reach about 160 for blank tiles).
    pub max_ratio: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 536_870_912,
            max_directories: 256,
            max_entries: 1_048_576,
            max_name_bytes: 4096,
            max_stream_bytes: 536_870_912,
            max_total_bytes: 1_073_741_824,
            max_ratio: 4096,
        }
    }
}
