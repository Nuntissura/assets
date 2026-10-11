//! Bounded, non-blocking admission: slots AND bytes. `try_acquire` never waits; a `Permit`
//! releases on drop, so every exit path (success, error, cancel, panic-free early return) frees
//! it. Cloning an `Admission` shares one budget, so a host can bound several sessions together.
use crate::{error::SessionError, ports::JobPort, ports::JobTicket};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Debug)]
struct Inner {
    capacity: u32,
    byte_budget: u64,
    retry_after_ms: u64,
    used: Mutex<(u32, u64)>,
}

#[derive(Clone, Debug)]
pub struct Admission {
    inner: Arc<Inner>,
}

impl Admission {
    pub fn new(capacity: u32, byte_budget: u64, retry_after_ms: u64) -> Result<Self, SessionError> {
        if capacity == 0 || byte_budget == 0 {
            return Err(SessionError::InvalidBudget);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                capacity,
                byte_budget,
                retry_after_ms,
                used: Mutex::new((0, 0)),
            }),
        })
    }

    /// Takes one slot and `bytes` of budget, or refuses at once.
    pub fn try_acquire(&self, bytes: u64) -> Result<Permit, SessionError> {
        if bytes > self.inner.byte_budget {
            return Err(SessionError::CostExceedsBudget);
        }
        let mut used = self
            .inner
            .used
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let fits = used.0 < self.inner.capacity
            && used
                .1
                .checked_add(bytes)
                .is_some_and(|total| total <= self.inner.byte_budget);
        if !fits {
            return Err(SessionError::Overloaded {
                retry_after_ms: self.inner.retry_after_ms,
            });
        }
        used.0 += 1;
        used.1 += bytes;
        Ok(Permit {
            inner: Arc::clone(&self.inner),
            bytes,
        })
    }

    /// (slots, bytes) currently held.
    pub fn in_flight(&self) -> (u32, u64) {
        *self
            .inner
            .used
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for Admission {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner {
                capacity: 16,
                byte_budget: 256 * 1024 * 1024,
                retry_after_ms: 50,
                used: Mutex::new((0, 0)),
            }),
        }
    }
}

#[derive(Debug)]
pub struct Permit {
    inner: Arc<Inner>,
    bytes: u64,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut used = self
            .inner
            .used
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        used.0 = used.0.saturating_sub(1);
        used.1 = used.1.saturating_sub(self.bytes);
    }
}

/// Releases a Job Runtime ticket when dropped.
pub(crate) struct JobGuard {
    pub(crate) port: Arc<dyn JobPort>,
    pub(crate) ticket: Option<JobTicket>,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            self.port.release(ticket);
        }
    }
}
