//! Explicitly granted, immutable byte sources and the hostile-input limits.
use crate::error::{LimitKind, LrcatError, Result, SourceError};

/// Hard ceilings no `Limits` value may exceed.
pub const HARD_MAX_MAIN_BYTES: u64 = 1 << 30;
pub const HARD_MAX_WAL_BYTES: u64 = 1 << 30;

/// A stable, immutable, read-only byte range granted by the caller. The decoder never writes,
/// seeks a shared cursor or opens anything by path: it only calls `read_at`.
pub trait ByteSource: Send + Sync {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Fills `buf` exactly from `offset`; a short range is `OutOfRange`.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::result::Result<(), SourceError>;
}

/// In-memory source (tests, already-loaded bytes).
#[derive(Clone, Debug, Default)]
pub struct SliceSource(Vec<u8>);

impl SliceSource {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl ByteSource for SliceSource {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::result::Result<(), SourceError> {
        let start = usize::try_from(offset).map_err(|_| SourceError::OutOfRange)?;
        let end = start
            .checked_add(buf.len())
            .ok_or(SourceError::OutOfRange)?;
        let src = self.0.get(start..end).ok_or(SourceError::OutOfRange)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// The caller states honestly whether a `-wal` sibling of the main file exists.
pub enum WalGrant {
    /// No WAL sibling exists.
    None,
    /// A WAL sibling exists and its bytes are granted.
    Granted(Box<dyn ByteSource>),
    /// A WAL sibling exists but is NOT granted: decoding fails closed (`UngrantedWal`).
    PresentButNotGranted,
}

pub struct Grant {
    pub main: Box<dyn ByteSource>,
    pub wal: WalGrant,
}

impl Grant {
    pub fn main_only(main: Box<dyn ByteSource>) -> Self {
        Self {
            main,
            wal: WalGrant::None,
        }
    }
}

/// Every limit is checked BEFORE the allocation or read it guards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_main_bytes: u64,
    pub max_wal_bytes: u64,
    pub max_pages: u32,
    /// Total page reads (main + WAL + overflow) in one extraction.
    pub max_page_reads: u64,
    /// Rows per table walk.
    pub max_rows: u64,
    /// Cells (leaf + interior) over the whole extraction.
    pub max_cells: u64,
    /// Declared payload bytes of one record.
    pub max_payload: u64,
    pub max_overflow_pages: u32,
    pub max_btree_depth: u32,
    pub max_columns: u32,
    /// Unmapped tables listed individually (the rest are aggregated).
    pub max_unmapped_tables: u32,
    /// Opaque develop-setting bytes retained in a proposal (hash + length are always kept).
    pub max_retained_opaque_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_main_bytes: HARD_MAX_MAIN_BYTES,
            max_wal_bytes: 1 << 28,
            max_pages: 1 << 21,
            max_page_reads: 16_000_000,
            max_rows: 4_000_000,
            max_cells: 16_000_000,
            max_payload: 16 << 20,
            max_overflow_pages: 4096,
            max_btree_depth: 64,
            max_columns: 2048,
            max_unmapped_tables: 256,
            max_retained_opaque_bytes: 256 << 20,
        }
    }
}

impl Limits {
    /// Rejects a configuration that exceeds the hard ceilings or disables a bound.
    pub fn validate(&self) -> Result<()> {
        let bad = self.max_main_bytes > HARD_MAX_MAIN_BYTES
            || self.max_wal_bytes > HARD_MAX_WAL_BYTES
            || self.max_pages == 0
            || self.max_page_reads == 0
            || self.max_rows == 0
            || self.max_cells == 0
            || self.max_payload == 0
            || self.max_btree_depth == 0
            || self.max_columns == 0;
        if bad {
            Err(LrcatError::Limit(LimitKind::Configuration))
        } else {
            Ok(())
        }
    }
}
