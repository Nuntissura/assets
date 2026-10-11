use crate::config::RingGeometry;
use crate::counters::Counters;
use crate::error::PushError;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// Keeps the two cursors on separate cache lines.
#[repr(align(64))]
struct Cursor(AtomicUsize);

/// Fixed-capacity single-producer single-consumer sample ring without `unsafe`.
///
/// Storage is a preallocated slice of `AtomicU32` holding `f32::to_bits`. Cursors run in
/// `[0, 2 * capacity)` so empty (equal) and full (`capacity` apart) stay distinguishable and no
/// counter wraps mid-ring on any target. The producer publishes with a Release store on `tail`,
/// the consumer acquires it; the consumer frees space with a Release store on `head`.
struct Shared {
    data: Box<[AtomicU32]>,
    head: Cursor,
    tail: Cursor,
    capacity: usize,
    channels: usize,
    counters: Arc<Counters>,
}

impl Shared {
    fn wrap(&self) -> usize {
        self.capacity * 2
    }
    fn occupied(&self, head: usize, tail: usize) -> usize {
        if tail >= head {
            tail - head
        } else {
            tail + self.wrap() - head
        }
    }
}

/// Creates the ring pair. All allocation happens here, off the realtime path.
pub fn ring(geometry: &RingGeometry, counters: Arc<Counters>) -> (Producer, Consumer) {
    let data: Vec<AtomicU32> = (0..geometry.capacity_samples)
        .map(|_| AtomicU32::new(0))
        .collect();
    let shared = Arc::new(Shared {
        data: data.into_boxed_slice(),
        head: Cursor(AtomicUsize::new(0)),
        tail: Cursor(AtomicUsize::new(0)),
        capacity: geometry.capacity_samples,
        channels: geometry.channels,
        counters,
    });
    (
        Producer {
            shared: Arc::clone(&shared),
            block_samples: geometry.block_samples,
            accepted_frames: 0,
        },
        Consumer {
            shared,
            popped_frames: 0,
        },
    )
}

/// Write end. Owned by exactly one thread (the control thread for output, the capture callback
/// for input).
pub struct Producer {
    shared: Arc<Shared>,
    block_samples: usize,
    accepted_frames: u64,
}

impl Producer {
    pub fn counters(&self) -> &Counters {
        &self.shared.counters
    }
    pub fn channels(&self) -> usize {
        self.shared.channels
    }
    /// Samples in one block (`block_frames * channels`).
    pub fn block_samples(&self) -> usize {
        self.block_samples
    }
    /// Ring capacity in samples.
    pub fn capacity_samples(&self) -> usize {
        self.shared.capacity
    }
    /// Frames this end accepted, tallied locally and independently of the shared counters.
    pub fn accepted_frames(&self) -> u64 {
        self.accepted_frames
    }
    /// Frames currently held in the ring.
    pub fn occupied_frames(&self) -> u64 {
        let head = self.shared.head.0.load(Ordering::Acquire);
        let tail = self.shared.tail.0.load(Ordering::Relaxed);
        (self.shared.occupied(head, tail) / self.shared.channels) as u64
    }

    fn free_samples(&self) -> usize {
        let head = self.shared.head.0.load(Ordering::Acquire);
        let tail = self.shared.tail.0.load(Ordering::Relaxed);
        self.shared.capacity - self.shared.occupied(head, tail)
    }

    fn write(&mut self, samples: &[f32]) {
        let shared = &*self.shared;
        let tail = shared.tail.0.load(Ordering::Relaxed);
        let mut index = tail % shared.capacity;
        for sample in samples {
            shared.data[index].store(sample.to_bits(), Ordering::Relaxed);
            index += 1;
            if index == shared.capacity {
                index = 0;
            }
        }
        shared
            .tail
            .0
            .store((tail + samples.len()) % shared.wrap(), Ordering::Release);
        let frames = (samples.len() / shared.channels) as u64;
        self.accepted_frames += frames;
        shared.counters.add_submitted(frames);
    }

    /// Offers exactly one block. `Full` is a refusal: the caller keeps the block, nothing is
    /// dropped silently, and one overrun plus one discontinuity are counted for the refused
    /// offer. `DeviceLost` is checked first and counts nothing.
    pub fn try_push_block(&mut self, block: &[f32]) -> Result<(), PushError> {
        if self.shared.counters.is_device_lost() {
            return Err(PushError::DeviceLost);
        }
        if block.len() != self.block_samples {
            return Err(PushError::WrongLength);
        }
        if self.free_samples() < block.len() {
            self.shared.counters.add_overrun(0);
            return Err(PushError::Full);
        }
        self.write(block);
        Ok(())
    }

    /// Capture path: pushes as many whole frames of `input` as fit and returns the frames
    /// accepted. Frames that do not fit are lost to the stream: one overrun, one discontinuity
    /// and the dropped frame count are recorded.
    pub fn push_frames(&mut self, input: &[f32]) -> u64 {
        let channels = self.shared.channels;
        let offered = input.len() / channels;
        let fit = self.free_samples() / channels;
        let take = offered.min(fit);
        if take > 0 {
            self.write(&input[..take * channels]);
        }
        if take < offered {
            self.shared.counters.add_overrun((offered - take) as u64);
        }
        take as u64
    }
}

/// Read end. Owned by exactly one thread (the callback for output, control for input).
pub struct Consumer {
    shared: Arc<Shared>,
    popped_frames: u64,
}

impl Consumer {
    pub fn counters(&self) -> &Counters {
        &self.shared.counters
    }
    pub fn channels(&self) -> usize {
        self.shared.channels
    }
    /// Frames this end removed, tallied locally and independently of the shared counters.
    pub fn popped_frames(&self) -> u64 {
        self.popped_frames
    }
    pub fn available_samples(&self) -> usize {
        let tail = self.shared.tail.0.load(Ordering::Acquire);
        let head = self.shared.head.0.load(Ordering::Relaxed);
        self.shared.occupied(head, tail)
    }
    pub fn available_frames(&self) -> u64 {
        (self.available_samples() / self.shared.channels) as u64
    }

    /// Copies up to `out.len()` samples (whole frames only) and returns the samples copied.
    /// Never blocks, never allocates; a short return means the ring ran dry.
    pub fn pop_into(&mut self, out: &mut [f32]) -> usize {
        let shared = &*self.shared;
        let tail = shared.tail.0.load(Ordering::Acquire);
        let head = shared.head.0.load(Ordering::Relaxed);
        let whole = out.len() / shared.channels * shared.channels;
        let take = shared.occupied(head, tail).min(whole);
        let mut index = head % shared.capacity;
        for slot in &mut out[..take] {
            *slot = f32::from_bits(shared.data[index].load(Ordering::Relaxed));
            index += 1;
            if index == shared.capacity {
                index = 0;
            }
        }
        if take > 0 {
            shared
                .head
                .0
                .store((head + take) % shared.wrap(), Ordering::Release);
            let frames = (take / shared.channels) as u64;
            self.popped_frames += frames;
            shared.counters.add_consumed(frames);
        }
        take
    }
}
