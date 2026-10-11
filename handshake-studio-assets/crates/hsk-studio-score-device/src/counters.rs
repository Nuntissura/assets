use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Relaxed atomic counters shared by the ring ends and the backend. Counting is the only
/// bookkeeping done on the realtime path.
///
/// Direction semantics: for an output stream the control side submits and the callback consumes;
/// for an input stream the callback submits and the control side consumes. `submitted_frames`
/// always counts frames accepted into the ring and `consumed_frames` frames removed from it.
#[derive(Debug, Default)]
pub struct Counters {
    submitted_frames: AtomicU64,
    consumed_frames: AtomicU64,
    underruns: AtomicU64,
    underrun_frames: AtomicU64,
    overruns: AtomicU64,
    dropped_frames: AtomicU64,
    discontinuities: AtomicU64,
    callbacks: AtomicU64,
    device_lost: AtomicU64,
}

/// Plain copy of [`Counters`] at one instant (each field read separately, not a joint snapshot).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CounterSnapshot {
    pub submitted_frames: u64,
    pub consumed_frames: u64,
    pub underruns: u64,
    pub underrun_frames: u64,
    pub overruns: u64,
    pub dropped_frames: u64,
    pub discontinuities: u64,
    pub callbacks: u64,
    pub device_lost: u64,
}

impl Counters {
    pub fn submitted_frames(&self) -> u64 {
        self.submitted_frames.load(Relaxed)
    }
    pub fn consumed_frames(&self) -> u64 {
        self.consumed_frames.load(Relaxed)
    }
    /// Callbacks that had to zero-fill at least one frame.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Relaxed)
    }
    /// Total frames of safe silence written by underrunning callbacks.
    pub fn underrun_frames(&self) -> u64 {
        self.underrun_frames.load(Relaxed)
    }
    /// Offers or callbacks that could not place everything into the ring.
    pub fn overruns(&self) -> u64 {
        self.overruns.load(Relaxed)
    }
    /// Frames the capture callback could not place into the ring (lost to the stream).
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Relaxed)
    }
    /// Points where the delivered stream is not contiguous with the offered stream.
    pub fn discontinuities(&self) -> u64 {
        self.discontinuities.load(Relaxed)
    }
    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Relaxed)
    }
    pub fn device_lost(&self) -> u64 {
        self.device_lost.load(Relaxed)
    }
    pub fn is_device_lost(&self) -> bool {
        self.device_lost() != 0
    }
    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            submitted_frames: self.submitted_frames(),
            consumed_frames: self.consumed_frames(),
            underruns: self.underruns(),
            underrun_frames: self.underrun_frames(),
            overruns: self.overruns(),
            dropped_frames: self.dropped_frames(),
            discontinuities: self.discontinuities(),
            callbacks: self.callbacks(),
            device_lost: self.device_lost(),
        }
    }

    pub(crate) fn add_submitted(&self, frames: u64) {
        self.submitted_frames.fetch_add(frames, Relaxed);
    }
    pub(crate) fn add_consumed(&self, frames: u64) {
        self.consumed_frames.fetch_add(frames, Relaxed);
    }
    pub(crate) fn add_underrun(&self, silent_frames: u64) {
        self.underruns.fetch_add(1, Relaxed);
        self.underrun_frames.fetch_add(silent_frames, Relaxed);
    }
    /// One overrun event is always one discontinuity: the stream skipped `frames` frames.
    pub(crate) fn add_overrun(&self, dropped_frames: u64) {
        self.overruns.fetch_add(1, Relaxed);
        self.discontinuities.fetch_add(1, Relaxed);
        self.dropped_frames.fetch_add(dropped_frames, Relaxed);
    }
    pub(crate) fn add_callback(&self) {
        self.callbacks.fetch_add(1, Relaxed);
    }
    pub(crate) fn add_device_lost(&self) {
        self.device_lost.fetch_add(1, Relaxed);
    }
}
