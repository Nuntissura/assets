//! Media index cache keyed by (content id, revision, length, limits).
//!
//! Index-first seek means the sample index is built once per source revision and reused. The cache is
//! instance-owned and single-owner (`&mut self`); wrap it in a lock to share. A hit still verifies the
//! caller's grant against the live source length, so a changed file is `StaleGrant`, never a stale index.
//! A new revision of a content id drops that id's older entries (counted as `invalidated`).
use crate::error::{ReelError, check_cancel};
use crate::isobmff::{IsobmffProvider, MediaIndex, build_index};
use crate::model::*;
use hsk_studio_accord::CancellationToken;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub approx_bytes: usize,
    pub hits: u64,
    pub misses: u64,
    /// Entries dropped to stay within capacity (least recently used first).
    pub evictions: u64,
    /// Entries dropped because the same content id arrived with a different revision.
    pub invalidated: u64,
}

struct Entry {
    content_id: String,
    revision: u64,
    len: u64,
    limits: Limits,
    index: Arc<MediaIndex>,
    used: u64,
}

pub struct IndexCache {
    capacity: usize,
    entries: Vec<Entry>,
    clock: u64,
    stats: CacheStats,
}

impl IndexCache {
    pub const MAX_CAPACITY: usize = 64;

    pub fn new(capacity: usize) -> Result<Self, ReelError> {
        if capacity == 0 || capacity > Self::MAX_CAPACITY {
            return Err(ReelError::LimitExceeded {
                what: "cache-capacity",
            });
        }
        Ok(Self {
            capacity,
            entries: Vec::new(),
            clock: 0,
            stats: CacheStats::default(),
        })
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.entries.len(),
            approx_bytes: self.entries.iter().map(|e| e.index.approx_bytes()).sum(),
            ..self.stats
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Return the cached index for this grant, building (and caching) it on a miss.
    pub fn get_or_build(
        &mut self,
        src: &mut dyn ReadSeek,
        grant: &Grant,
        expected_revision: u64,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<Arc<MediaIndex>, ReelError> {
        check_cancel(cancel)?;
        let len = verify_grant(src, grant, expected_revision)?;
        self.clock += 1;
        let id = grant.content_id.as_str();
        if let Some(entry) = self.entries.iter_mut().find(|e| {
            e.content_id == id
                && e.revision == grant.revision
                && e.len == len
                && e.limits == *limits
        }) {
            entry.used = self.clock;
            self.stats.hits += 1;
            return Ok(Arc::clone(&entry.index));
        }
        self.stats.misses += 1;
        let index = Arc::new(build_index(src, len, limits, cancel)?);
        let before = self.entries.len();
        self.entries
            .retain(|e| e.content_id != id || e.revision == grant.revision);
        self.stats.invalidated += (before - self.entries.len()) as u64;
        while self.entries.len() >= self.capacity {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.used)
                .map(|(i, _)| i)
                .expect("capacity >= 1");
            self.entries.swap_remove(oldest);
            self.stats.evictions += 1;
        }
        self.entries.push(Entry {
            content_id: id.to_owned(),
            revision: grant.revision,
            len,
            limits: *limits,
            index: Arc::clone(&index),
            used: self.clock,
        });
        Ok(index)
    }
}

impl IsobmffProvider {
    /// Packet or PCM delivery with the index served from `cache` (built on first use of a revision).
    pub fn decode_cached(
        &self,
        cache: &mut IndexCache,
        src: &mut dyn ReadSeek,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        let index = cache.get_or_build(src, &req.grant, req.expected_revision, limits, cancel)?;
        self.decode_packets(src, &index, req, limits, cancel, sink)
    }
}
