//! Bounded-loading limits. Every untrusted size is checked against these before any allocation
//! or decode; the container is STORE-only, so nothing is ever inflated and no ratio bomb exists.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum number of archive entries (read and write).
    pub max_entries: u32,
    /// Maximum stored size of one entry.
    pub max_entry_bytes: u64,
    /// Maximum sum of all entry sizes.
    pub max_total_bytes: u64,
    /// Maximum entry-name length in bytes.
    pub max_name_len: u16,
    /// Maximum `manifest.json` size.
    pub max_manifest_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 65_536,
            max_entry_bytes: 512 * 1024 * 1024,
            max_total_bytes: 4 * 1024 * 1024 * 1024,
            max_name_len: 255,
            max_manifest_bytes: 16 * 1024 * 1024,
        }
    }
}
