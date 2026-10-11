//! Media index cache keyed by (content id, revision, length, limits), for ISOBMFF/MOV/MP4 (`MediaIndex`)
//! and Matroska/WebM (`MkvIndex`) sources.
//!
//! Index-first seek means the sample index is built once per source revision and reused. The cache is
//! instance-owned and single-owner (`&mut self`); wrap it in a lock to share. A hit still verifies the
//! caller's grant against the live source length, so a changed file is `StaleGrant`, never a stale index.
//! A new revision of a content id drops that id's older entries (counted as `invalidated`). The two index
//! kinds share one capacity and one LRU order; an entry is only returned to a caller asking for its own
//! kind. A failed build caches nothing.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{ReelError, check_cancel};
use crate::isobmff::{IsobmffProvider, MediaIndex, build_index};
use crate::matroska::{MatroskaProvider, MkvIndex, build_matroska_index};
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

#[derive(Clone)]
enum Cached {
    Isobmff(Arc<MediaIndex>),
    Matroska(Arc<MkvIndex>),
}

impl Cached {
    fn approx_bytes(&self) -> usize {
        match self {
            Self::Isobmff(index) => index.approx_bytes(),
            Self::Matroska(index) => index.approx_bytes(),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Self::Isobmff(_) => Kind::Isobmff,
            Self::Matroska(_) => Kind::Matroska,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Isobmff,
    Matroska,
}

struct Entry {
    content_id: String,
    revision: u64,
    len: u64,
    limits: Limits,
    index: Cached,
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
            approx_bytes: self
                .entries
                .iter()
                .fold(0usize, |sum, e| sum.saturating_add(e.index.approx_bytes())),
            ..self.stats
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Return the cached ISOBMFF index for this grant, building (and caching) it on a miss.
    pub fn get_or_build(
        &mut self,
        src: &mut dyn ReadSeek,
        grant: &Grant,
        expected_revision: u64,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<Arc<MediaIndex>, ReelError> {
        match self.lookup_or_build(
            src,
            grant,
            expected_revision,
            limits,
            cancel,
            Kind::Isobmff,
            |src, len| {
                Ok(Cached::Isobmff(Arc::new(build_index(
                    src, len, limits, cancel,
                )?)))
            },
        )? {
            Cached::Isobmff(index) => Ok(index),
            Cached::Matroska(_) => Err(ReelError::Corrupt {
                what: "cache-kind-mismatch",
            }),
        }
    }

    /// Return the cached Matroska/WebM index for this grant, building (and caching) it on a miss.
    pub fn get_or_build_matroska(
        &mut self,
        src: &mut dyn ReadSeek,
        grant: &Grant,
        expected_revision: u64,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<Arc<MkvIndex>, ReelError> {
        match self.lookup_or_build(
            src,
            grant,
            expected_revision,
            limits,
            cancel,
            Kind::Matroska,
            |src, len| {
                Ok(Cached::Matroska(Arc::new(build_matroska_index(
                    src, len, limits, cancel,
                )?)))
            },
        )? {
            Cached::Matroska(index) => Ok(index),
            Cached::Isobmff(_) => Err(ReelError::Corrupt {
                what: "cache-kind-mismatch",
            }),
        }
    }

    /// The shared lookup: verify the grant, serve a hit of the asked `kind`, else build, drop the id's older revisions, evict LRU down to capacity, store.
    #[allow(clippy::too_many_arguments)]
    fn lookup_or_build(
        &mut self,
        src: &mut dyn ReadSeek,
        grant: &Grant,
        expected_revision: u64,
        limits: &Limits,
        cancel: &CancellationToken,
        kind: Kind,
        build: impl FnOnce(&mut dyn ReadSeek, u64) -> Result<Cached, ReelError>,
    ) -> Result<Cached, ReelError> {
        check_cancel(cancel)?;
        let len = verify_grant(src, grant, expected_revision)?;
        self.clock = self.clock.saturating_add(1);
        let id = grant.content_id.as_str();
        let clock = self.clock;
        if let Some(entry) = self.entries.iter_mut().find(|e| {
            e.content_id == id
                && e.revision == grant.revision
                && e.len == len
                && e.limits == *limits
                && e.index.kind() == kind
        }) {
            entry.used = clock;
            self.stats.hits = self.stats.hits.saturating_add(1);
            return Ok(entry.index.clone());
        }
        self.stats.misses = self.stats.misses.saturating_add(1);
        let built = build(src, len)?;
        let before = self.entries.len();
        self.entries
            .retain(|e| e.content_id != id || e.revision == grant.revision);
        self.stats.invalidated = self
            .stats
            .invalidated
            .saturating_add(before.saturating_sub(self.entries.len()) as u64);
        while self.entries.len() >= self.capacity {
            let Some(oldest) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.used)
                .map(|(i, _)| i)
            else {
                break;
            };
            self.entries.swap_remove(oldest);
            self.stats.evictions = self.stats.evictions.saturating_add(1);
        }
        self.entries.push(Entry {
            content_id: id.to_owned(),
            revision: grant.revision,
            len,
            limits: *limits,
            index: built.clone(),
            used: clock,
        });
        Ok(built)
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

impl MatroskaProvider {
    /// Packet or PCM delivery with the Matroska index served from `cache` (built on first use of a
    /// revision). The receipt's `bytes_read` counts payload only, as for `decode_packets`.
    pub fn decode_cached(
        &self,
        cache: &mut IndexCache,
        src: &mut dyn ReadSeek,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        let index =
            cache.get_or_build_matroska(src, &req.grant, req.expected_revision, limits, cancel)?;
        self.decode_packets(src, &index, req, limits, cancel, sink)
    }
}
