use crate::config::{DeviceGrant, DeviceId, DeviceInfo, Limits, StreamConfig};
use crate::counters::{CounterSnapshot, Counters};
use crate::error::DeviceError;
use crate::ring::{Consumer, Producer};
use hsk_studio_accord::CancellationToken;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

struct Held {
    open_streams: u32,
    bytes_in_flight: usize,
}

struct AdmissionState {
    limits: Limits,
    held: Mutex<Held>,
}

impl AdmissionState {
    /// Admission runs off the realtime path, so a plain lock keeps both bounds exact under
    /// concurrent acquires; a poisoned lock still holds consistent counters (each update is a
    /// single assignment).
    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Bounded admission for one provider: open streams and ring bytes in flight. A [`Permit`] is
/// taken after validation and released only after the backend is quiescent (STU-ARC-008/012).
#[derive(Clone)]
pub struct Admission {
    state: Arc<AdmissionState>,
}

impl Admission {
    pub fn new(limits: Limits) -> Self {
        Self {
            state: Arc::new(AdmissionState {
                limits,
                held: Mutex::new(Held {
                    open_streams: 0,
                    bytes_in_flight: 0,
                }),
            }),
        }
    }
    pub fn limits(&self) -> &Limits {
        &self.state.limits
    }
    pub fn open_streams(&self) -> u32 {
        self.state.held().open_streams
    }
    pub fn bytes_in_flight(&self) -> usize {
        self.state.held().bytes_in_flight
    }
    /// Takes one stream slot and `ring_bytes` of ring budget, or refuses with `AdmissionDenied`
    /// when either `Limits::max_open_streams` or `Limits::max_bytes_in_flight` would be
    /// exceeded. Nothing is taken on a refusal.
    pub fn acquire(&self, ring_bytes: usize) -> Result<Permit, DeviceError> {
        let mut held = self.state.held();
        let bytes = held
            .bytes_in_flight
            .checked_add(ring_bytes)
            .filter(|total| *total <= self.state.limits.max_bytes_in_flight);
        match bytes {
            Some(total) if held.open_streams < self.state.limits.max_open_streams => {
                held.open_streams += 1;
                held.bytes_in_flight = total;
            }
            _ => return Err(DeviceError::AdmissionDenied),
        }
        drop(held);
        Ok(Permit {
            state: Some(Arc::clone(&self.state)),
            ring_bytes,
        })
    }
}

/// RAII admission permit. Releasing is idempotent; `Drop` releases if still held.
pub struct Permit {
    state: Option<Arc<AdmissionState>>,
    ring_bytes: usize,
}

impl Permit {
    /// Returns true when this call released the permit, false when it was already released.
    pub fn release(&mut self) -> bool {
        match self.state.take() {
            Some(state) => {
                let mut held = state.held();
                held.bytes_in_flight = held.bytes_in_flight.saturating_sub(self.ring_bytes);
                held.open_streams = held.open_streams.saturating_sub(1);
                true
            }
            None => false,
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.release();
    }
}

/// What `close` observed once the backend was quiescent. Direction semantics are those of
/// [`Counters`]: `frames_submitted` counts frames accepted into the ring, `frames_consumed`
/// frames removed from it, `residual_frames` frames still inside it at close.
#[derive(Clone, Debug, PartialEq)]
pub struct CloseReport {
    /// The admission permit was released by this close (after the thread/stream stopped).
    pub permits_released: bool,
    /// The backend thread (software device) or stream (native) is gone.
    pub thread_joined: bool,
    pub device_lost: bool,
    pub callbacks: u64,
    pub frames_submitted: u64,
    pub frames_consumed: u64,
    pub residual_frames: u64,
    pub underruns: u64,
    pub underrun_frames: u64,
    pub overruns: u64,
    pub dropped_frames: u64,
    pub discontinuities: u64,
    /// Offers the ring refused for lack of space (backpressure; the producer kept the block).
    /// Not a loss: they do not count into `overruns`, `discontinuities` or `dropped_frames`
    /// (test `backpressured_stream_with_retries_is_lossless`).
    pub refused_offers: u64,
    /// Interleaved samples the software device handed to its fake hardware, in callback order,
    /// including underrun silence; empty for backends without a loopback recorder.
    pub loopback_samples: Vec<f32>,
}

impl CloseReport {
    pub(crate) fn from_counters(
        c: CounterSnapshot,
        permits_released: bool,
        thread_joined: bool,
        residual_frames: u64,
        loopback_samples: Vec<f32>,
    ) -> Self {
        Self {
            permits_released,
            thread_joined,
            device_lost: c.device_lost != 0,
            callbacks: c.callbacks,
            frames_submitted: c.submitted_frames,
            frames_consumed: c.consumed_frames,
            residual_frames,
            underruns: c.underruns,
            underrun_frames: c.underrun_frames,
            overruns: c.overruns,
            dropped_frames: c.dropped_frames,
            discontinuities: c.discontinuities,
            refused_offers: c.refused_offers,
            loopback_samples,
        }
    }
    /// Frame accounting identity: everything accepted into the ring is either consumed or still
    /// inside it.
    pub fn reconciles(&self) -> bool {
        self.frames_submitted == self.frames_consumed + self.residual_frames
    }
    /// A clean close: backend quiescent, permit released, nothing lost (no device loss and no
    /// dropped frames; refused offers are backpressure, not loss).
    pub fn is_clean(&self) -> bool {
        self.thread_joined
            && self.permits_released
            && !self.device_lost
            && self.dropped_frames == 0
            && self.reconciles()
    }
}

/// Backend behind a stream: owns the realtime thread or native stream plus the admission permit.
pub trait StreamBackend: Send {
    /// Asks the backend to stop calling the callback. Never blocks.
    fn request_stop(&self);
    /// True once the backend confirmed it no longer calls the callback.
    fn stop_acknowledged(&self) -> bool;
    /// Simulated loss; only the software device supports it (returns false elsewhere).
    fn inject_device_loss(&self) -> bool {
        false
    }
    /// Stops, joins/destroys the backend, releases the permit and reports. Runs off the callback.
    fn close(self: Box<Self>) -> CloseReport;
}

fn closed_without_backend(counters: &Counters, residual: u64) -> CloseReport {
    CloseReport::from_counters(counters.snapshot(), false, true, residual, Vec::new())
}

/// Control-side handle of an output stream: the control thread pushes blocks through
/// [`OutputStream::producer`]; the backend callback drains them.
pub struct OutputStream {
    producer: Producer,
    backend: Option<Box<dyn StreamBackend>>,
    config: StreamConfig,
}

impl OutputStream {
    pub fn new(producer: Producer, backend: Box<dyn StreamBackend>, config: StreamConfig) -> Self {
        Self {
            producer,
            backend: Some(backend),
            config,
        }
    }
    pub fn producer(&mut self) -> &mut Producer {
        &mut self.producer
    }
    pub fn config(&self) -> &StreamConfig {
        &self.config
    }
    pub fn counters(&self) -> &Counters {
        self.producer.counters()
    }
    pub fn device_lost(&self) -> bool {
        self.counters().is_device_lost()
    }
    pub fn stop(&self) {
        if let Some(b) = &self.backend {
            b.request_stop();
        }
    }
    pub fn stop_acknowledged(&self) -> bool {
        self.backend.as_ref().is_none_or(|b| b.stop_acknowledged())
    }
    pub fn inject_device_loss(&self) -> bool {
        self.backend.as_ref().is_some_and(|b| b.inject_device_loss())
    }
    /// Stops, quiesces and reports; `residual_frames` is re-read from the ring after the join.
    pub fn close(mut self) -> CloseReport {
        let mut report = match self.backend.take() {
            Some(b) => b.close(),
            None => closed_without_backend(self.producer.counters(), 0),
        };
        report.residual_frames = self.producer.occupied_frames();
        report
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        if let Some(b) = self.backend.take() {
            let _ = b.close();
        }
    }
}

/// Control-side handle of an input stream: the backend callback pushes captured blocks; the
/// control thread drains them through [`InputStream::consumer`].
pub struct InputStream {
    consumer: Consumer,
    backend: Option<Box<dyn StreamBackend>>,
    config: StreamConfig,
}

impl InputStream {
    pub fn new(consumer: Consumer, backend: Box<dyn StreamBackend>, config: StreamConfig) -> Self {
        Self {
            consumer,
            backend: Some(backend),
            config,
        }
    }
    pub fn consumer(&mut self) -> &mut Consumer {
        &mut self.consumer
    }
    pub fn config(&self) -> &StreamConfig {
        &self.config
    }
    pub fn counters(&self) -> &Counters {
        self.consumer.counters()
    }
    pub fn device_lost(&self) -> bool {
        self.counters().is_device_lost()
    }
    pub fn stop(&self) {
        if let Some(b) = &self.backend {
            b.request_stop();
        }
    }
    pub fn stop_acknowledged(&self) -> bool {
        self.backend.as_ref().is_none_or(|b| b.stop_acknowledged())
    }
    pub fn inject_device_loss(&self) -> bool {
        self.backend.as_ref().is_some_and(|b| b.inject_device_loss())
    }
    pub fn close(mut self) -> CloseReport {
        let mut report = match self.backend.take() {
            Some(b) => b.close(),
            None => closed_without_backend(self.consumer.counters(), 0),
        };
        report.residual_frames = self.consumer.available_frames();
        report
    }
}

impl Drop for InputStream {
    fn drop(&mut self) {
        if let Some(b) = self.backend.take() {
            let _ = b.close();
        }
    }
}

/// Optional native device I/O provider port. Every method is off the realtime path and takes the
/// caller's grant explicitly; `None` is `Unauthorized` and nothing is created.
pub trait DeviceProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn enumerate(&self, grant: Option<&DeviceGrant>) -> Result<Vec<DeviceInfo>, DeviceError>;
    fn open_output(
        &self,
        device: &DeviceId,
        config: &StreamConfig,
        grant: Option<&DeviceGrant>,
        cancel: &CancellationToken,
    ) -> Result<OutputStream, DeviceError>;
    fn open_input(
        &self,
        device: &DeviceId,
        config: &StreamConfig,
        grant: Option<&DeviceGrant>,
        cancel: &CancellationToken,
    ) -> Result<InputStream, DeviceError>;
}
