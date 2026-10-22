//! Byte-accounted GPU residency: a hard budget, LRU eviction of unpinned entries, pins for
//! in-flight work and exact counters. Pure Rust (no GPU types), so it is testable on machines
//! without an adapter. The executor charges every real device allocation here before creating it
//! (STU-ARC-008/012/013: bounded admission, device allocations counted, release on every exit).
//! All counter and byte arithmetic is checked or saturating (V4-01).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::GpuError;
use hsk_studio_pigment::Rect;

/// What a reservation is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidentKey {
    /// A source tile in plan pixel space (`source` = plan `SourceKey`).
    Tile { source: u32, bounds: Rect },
    /// A transient working buffer (staging, parameters, upload scratch).
    Scratch(&'static str),
    /// The chunk-sized destination of group nesting level `n` (0 = the root destination).
    Level(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservationId(u64);

/// Outcome of [`Residency::reserve`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub id: ReservationId,
    /// `true` when the key was already resident (no new bytes charged; entry touched and pinned).
    pub hit: bool,
    /// Keys evicted (least recently used first) to make room; the caller drops their payloads.
    pub evicted: Vec<ResidentKey>,
}

#[derive(Clone, Debug)]
struct Entry {
    id: u64,
    key: ResidentKey,
    bytes: u64,
    last_used: u64,
    pinned: bool,
}

#[derive(Clone, Debug)]
pub struct Residency {
    budget_bytes: u64,
    used: u64,
    peak_used: u64,
    evictions: u64,
    next_id: u64,
    tick: u64,
    entries: Vec<Entry>,
}

impl Residency {
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            budget_bytes,
            used: 0,
            peak_used: 0,
            evictions: 0,
            next_id: 1,
            tick: 0,
            entries: Vec::new(),
        }
    }
    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }
    pub fn used(&self) -> u64 {
        self.used
    }
    /// Highest `used` since construction or the last [`Residency::reset_peak`].
    pub fn peak_used(&self) -> u64 {
        self.peak_used
    }
    pub fn evictions(&self) -> u64 {
        self.evictions
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn reset_peak(&mut self) {
        self.peak_used = self.used;
    }
    /// Bytes held by pinned entries (not evictable: test residency_lru_budget_accounting).
    pub fn pinned_bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.pinned)
            .map(|e| e.bytes)
            .fold(0_u64, u64::saturating_add)
    }
    pub fn contains(&self, key: &ResidentKey) -> bool {
        self.entries.iter().any(|e| e.key == *key)
    }

    /// Reserve `bytes` for `key`, pinned. A key already resident is a hit: touched, pinned, not
    /// charged again (a size mismatch is `InvalidInput`). Otherwise least-recently-used UNPINNED
    /// entries are evicted until the bytes fit. Nothing is evicted when the request cannot fit
    /// even after evicting every unpinned entry, and a single entry larger than the budget is
    /// refused outright (`BudgetExceeded`).
    pub fn reserve(&mut self, key: ResidentKey, bytes: u64) -> Result<Reservation, GpuError> {
        self.tick = self.tick.saturating_add(1);
        if let Some(entry) = self.entries.iter_mut().find(|e| e.key == key) {
            if entry.bytes != bytes {
                return Err(GpuError::Gpu("residency_size_mismatch"));
            }
            entry.last_used = self.tick;
            entry.pinned = true;
            return Ok(Reservation {
                id: ReservationId(entry.id),
                hit: true,
                evicted: Vec::new(),
            });
        }
        let exceeded = GpuError::BudgetExceeded {
            requested: bytes,
            budget: self.budget_bytes,
        };
        if bytes > self.budget_bytes {
            return Err(exceeded);
        }
        let reachable = self.budget_bytes.saturating_sub(self.pinned_bytes());
        if bytes > reachable {
            return Err(exceeded);
        }
        // Checked: `used + bytes` beyond u64 never wraps or panics; it simply does not fit.
        let budget = self.budget_bytes;
        let fits = |used: u64| used.checked_add(bytes).is_some_and(|total| total <= budget);
        let mut evicted = Vec::new();
        while !fits(self.used) {
            let victim = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| !e.pinned)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(i, _)| i)
                .ok_or(exceeded)?;
            let gone = self.entries.remove(victim);
            self.used = self.used.saturating_sub(gone.bytes);
            self.evictions = self.evictions.saturating_add(1);
            evicted.push(gone.key);
        }
        let used = self.used.checked_add(bytes).ok_or(exceeded)?;
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.entries.push(Entry {
            id,
            key,
            bytes,
            last_used: self.tick,
            pinned: true,
        });
        self.used = used;
        self.peak_used = self.peak_used.max(self.used);
        Ok(Reservation {
            id: ReservationId(id),
            hit: false,
            evicted,
        })
    }

    /// Mark most recently used without changing its pin state; `false` when not resident.
    pub fn touch(&mut self, key: &ResidentKey) -> bool {
        self.tick = self.tick.saturating_add(1);
        let tick = self.tick;
        self.entries
            .iter_mut()
            .find(|e| e.key == *key)
            .map(|e| e.last_used = tick)
            .is_some()
    }
    /// Make an entry evictable; `false` for an unknown id.
    pub fn unpin(&mut self, id: ReservationId) -> bool {
        self.entries
            .iter_mut()
            .find(|e| e.id == id.0)
            .map(|e| e.pinned = false)
            .is_some()
    }
    /// Drop an entry and return its bytes; `false` for an unknown id.
    pub fn release(&mut self, id: ReservationId) -> bool {
        match self.entries.iter().position(|e| e.id == id.0) {
            Some(at) => {
                let gone = self.entries.remove(at);
                self.used = self.used.saturating_sub(gone.bytes);
                true
            }
            None => false,
        }
    }
    /// Position marker: every reservation made after this call has a higher id.
    pub fn mark(&self) -> ReservationId {
        ReservationId(self.next_id)
    }
    /// Release every entry reserved at or after `mark` (the executor's release-on-every-exit).
    pub fn release_since(&mut self, mark: ReservationId) -> u64 {
        let mut freed = 0;
        self.entries.retain(|e| {
            let drop_it = e.id >= mark.0;
            if drop_it {
                freed = u64::saturating_add(freed, e.bytes);
            }
            !drop_it
        });
        self.used = self.used.saturating_sub(freed);
        freed
    }
    /// Forget everything (a new device generation owns none of the old allocations). Counters stay.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.used = 0;
    }
}
