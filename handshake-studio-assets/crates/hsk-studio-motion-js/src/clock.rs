//! Monotonic nanosecond clocks for the cooperative deadline (a resource guard, never a value).
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

pub trait Clock: Send + Sync + std::fmt::Debug {
    fn now_ns(&self) -> u64;
}

#[derive(Debug)]
pub struct SystemClock(Instant);

impl SystemClock {
    pub fn new() -> Self {
        Self(Instant::now())
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// Deterministic clock for tests and replay: time moves only when told to.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance_ms(&self, ms: u64) {
        self.0.fetch_add(ms.saturating_mul(1_000_000), Ordering::AcqRel);
    }
}

impl Clock for ManualClock {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}
