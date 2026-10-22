//! Bounded scoped-thread helpers (std only, safe code). Design: every job is a pure function of
//! its index, outputs land at fixed positions, and when jobs fail the LOWEST failing index decides
//! the error (not the first thread to fail). A cancelled run returns `Canceled` and no partial
//! output; cancellation is checked between jobs. Evidence that the thread count does not change a
//! result: `tests/develop_parallel.rs` (`output_is_identical_for_every_thread_count`,
//! `lowest_failing_tile_decides_the_error_for_every_thread_count`,
//! `cancellation_returns_canceled_and_no_partial_output`) and the `develop-bench` digests.
use crate::DevelopError;
use hsk_studio_accord::CancellationToken;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

/// Hard upper bound for an explicit thread count.
pub const MAX_THREADS: usize = 64;
/// Cap applied to the default derived from `std::thread::available_parallelism`.
pub const DEFAULT_THREAD_CAP: usize = 16;

/// Worker threads a develop call may use (the calling thread counts as one). `Threads::single()`
/// runs everything on the calling thread, without spawning. Any count produces byte-identical
/// tiles, receipts and refusals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Threads(NonZeroUsize);

impl Threads {
    /// One thread: the calling thread only.
    pub const fn single() -> Self {
        Self(NonZeroUsize::MIN)
    }

    /// An explicit count in `1..=MAX_THREADS`; anything else is `InvalidLimits`.
    pub fn new(count: usize) -> Result<Self, DevelopError> {
        match NonZeroUsize::new(count) {
            Some(n) if count <= MAX_THREADS => Ok(Self(n)),
            _ => Err(DevelopError::InvalidLimits),
        }
    }

    /// `min(available_parallelism, DEFAULT_THREAD_CAP)`, at least 1; resolved once per process.
    pub fn auto() -> Self {
        static AUTO: OnceLock<usize> = OnceLock::new();
        let count = *AUTO.get_or_init(|| {
            std::thread::available_parallelism()
                .map_or(1, NonZeroUsize::get)
                .clamp(1, DEFAULT_THREAD_CAP)
        });
        Self(NonZeroUsize::new(count).unwrap_or(NonZeroUsize::MIN))
    }

    pub fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for Threads {
    fn default() -> Self {
        Self::auto()
    }
}

/// Rows per band so a band holds about 64 Ki samples (cancellation and load-balance granularity).
pub(crate) fn band_rows(width: usize) -> usize {
    (65_536 / width.max(1)).max(1)
}

/// Run `job(state, i)` for every `i` in `0..count` on up to `threads` scoped workers (the caller is
/// one of them) and return the outputs in index order. `init` builds each worker's scratch state.
/// On failure the lowest failing index's error is returned; on cancellation `Canceled`.
pub(crate) fn run_indexed<S, T: Send>(
    count: usize,
    threads: Threads,
    cancel: &CancellationToken,
    init: impl Fn() -> S + Sync,
    job: impl Fn(&mut S, usize) -> Result<T, DevelopError> + Sync,
) -> Result<Vec<T>, DevelopError> {
    let workers = threads.get().min(count);
    if workers <= 1 {
        let mut state = init();
        let mut out = Vec::with_capacity(count);
        for index in 0..count {
            cancel.check()?;
            out.push(job(&mut state, index)?);
        }
        return Ok(out);
    }
    let next = AtomicUsize::new(0);
    let first_error = AtomicUsize::new(usize::MAX);
    let work = || {
        let mut state = init();
        let mut done: Vec<(usize, Result<T, DevelopError>)> = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            // Indices above a known failure can never decide the result.
            if index >= count
                || index > first_error.load(Ordering::Relaxed)
                || cancel.check().is_err()
            {
                break;
            }
            let result = job(&mut state, index);
            if result.is_err() {
                first_error.fetch_min(index, Ordering::Relaxed);
            }
            done.push((index, result));
        }
        done
    };
    let mut collected = Vec::with_capacity(workers);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (1..workers).map(|_| scope.spawn(work)).collect();
        collected.push(work());
        for handle in handles {
            match handle.join() {
                Ok(done) => collected.push(done),
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
    });
    let mut slots: Vec<Option<Result<T, DevelopError>>> = (0..count).map(|_| None).collect();
    for (index, result) in collected.into_iter().flatten() {
        slots[index] = Some(result);
    }
    let mut out = Vec::with_capacity(count);
    for slot in slots {
        match slot {
            Some(Ok(value)) => out.push(value),
            Some(Err(error)) => return Err(error),
            // Never computed: only a cancellation (or a lower failure, returned above) skips a job.
            None => return Err(DevelopError::Canceled),
        }
    }
    Ok(out)
}

/// Split `data` into bands of `band_len` elements and run `job(band_index, band)` on each, on up to
/// `threads` scoped workers. The lowest failing band decides the error; cancellation wins over it.
pub(crate) fn for_each_band<T: Send>(
    data: &mut [T],
    band_len: usize,
    threads: Threads,
    cancel: &CancellationToken,
    job: impl Fn(usize, &mut [T]) -> Result<(), DevelopError> + Sync,
) -> Result<(), DevelopError> {
    let band_len = band_len.max(1);
    let bands = data.len().div_ceil(band_len);
    let workers = threads.get().min(bands);
    if workers <= 1 {
        for (index, band) in data.chunks_mut(band_len).enumerate() {
            cancel.check()?;
            job(index, band)?;
        }
        return Ok(());
    }
    let queue = Mutex::new(data.chunks_mut(band_len).enumerate());
    let failure: Mutex<Option<(usize, DevelopError)>> = Mutex::new(None);
    let canceled = AtomicBool::new(false);
    let work = || {
        loop {
            let item = queue.lock().unwrap_or_else(PoisonError::into_inner).next();
            let Some((index, band)) = item else { break };
            let known = failure
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .map(|(i, _)| *i);
            if known.is_some_and(|failed| index > failed) {
                break;
            }
            if cancel.check().is_err() {
                canceled.store(true, Ordering::Relaxed);
                break;
            }
            if let Err(error) = job(index, band) {
                let mut slot = failure.lock().unwrap_or_else(PoisonError::into_inner);
                if slot.as_ref().is_none_or(|(i, _)| index < *i) {
                    *slot = Some((index, error));
                }
            }
        }
    };
    std::thread::scope(|scope| {
        let handles: Vec<_> = (1..workers).map(|_| scope.spawn(work)).collect();
        work();
        for handle in handles {
            if let Err(payload) = handle.join() {
                std::panic::resume_unwind(payload);
            }
        }
    });
    if canceled.load(Ordering::Relaxed) {
        return Err(DevelopError::Canceled);
    }
    match failure.into_inner().unwrap_or_else(PoisonError::into_inner) {
        Some((_, error)) => Err(error),
        None => Ok(()),
    }
}
