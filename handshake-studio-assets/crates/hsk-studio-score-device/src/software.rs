//! Pure Rust, thread-driven software device. It proves the stream lifecycle (grant, admission,
//! ring, clocked callbacks, loss, stop acknowledgement, join, permit release) and the counters.
//! It is NOT native-device proof: no operating-system audio API is involved (NOT_PROVEN for any
//! real device).
use crate::callback::{capture_callback, render_callback};
use crate::config::{
    DeviceGrant, DeviceId, DeviceInfo, Direction, Limits, RingGeometry, StreamConfig,
};
use crate::counters::Counters;
use crate::error::DeviceError;
use crate::provider::{
    Admission, CloseReport, DeviceProvider, InputStream, OutputStream, Permit, StreamBackend,
};
use crate::ring::{Consumer, Producer, ring};
use hsk_studio_accord::CancellationToken;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Id of the software output device.
pub const SOFTWARE_OUTPUT_DEVICE: &str = "software-clock-out";
/// Id of the software input device (emits the ramp 1.0, 2.0, 3.0, ... one value per sample;
/// exact for the first 2^24 samples).
pub const SOFTWARE_INPUT_DEVICE: &str = "software-clock-in";
/// Widest layout the software device carries.
pub const SOFTWARE_MAX_CHANNELS: u16 = 8;

/// Software device options, fixed at open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SoftwareOptions {
    /// Frames of loopback recording preallocated at open (output only, 0 disables). Recording
    /// stops silently when full; `CloseReport::loopback_samples` holds what was recorded.
    pub loopback_frames: usize,
}

struct Flags {
    stop: AtomicBool,
    lost: AtomicBool,
    acked: AtomicBool,
}

struct ThreadEnd {
    residual_frames: u64,
    loopback: Vec<f32>,
}

/// One OS thread that calls the shared callback body at block cadence from a fixed `Instant`
/// schedule (`n * block_frames / rate` after start; late callbacks catch up, never drift).
/// The thread is ours, so stopping and joining it is allowed; shutdown never runs on it.
pub struct ClockedSoftwareDevice {
    flags: Arc<Flags>,
    counters: Arc<Counters>,
    thread: Option<JoinHandle<ThreadEnd>>,
    permit: Option<Permit>,
}

fn due_after(n: u64, block_frames: u64, rate_hz: u32) -> Duration {
    let nanos = u128::from(n) * u128::from(block_frames) * 1_000_000_000 / u128::from(rate_hz);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

fn clock_loop(flags: &Flags, rate_hz: u32, block_frames: u64, mut tick: impl FnMut()) {
    let start = Instant::now();
    let mut n: u64 = 0;
    loop {
        if flags.stop.load(Ordering::Acquire) || flags.lost.load(Ordering::Acquire) {
            break;
        }
        let due = start + due_after(n, block_frames, rate_hz);
        let now = Instant::now();
        if now < due {
            thread::park_timeout(due - now);
            continue;
        }
        tick();
        n += 1;
    }
    flags.acked.store(true, Ordering::Release);
}

impl ClockedSoftwareDevice {
    /// Starts the output clock. Everything the callback needs (scratch block, loopback buffer)
    /// is allocated here, before the thread exists.
    pub fn spawn_output(
        mut consumer: Consumer,
        geometry: &RingGeometry,
        sample_rate_hz: u32,
        loopback_frames: usize,
        counters: Arc<Counters>,
        permit: Permit,
    ) -> Result<Self, DeviceError> {
        let flags = new_flags();
        let thread_flags = Arc::clone(&flags);
        let block_samples = geometry.block_samples;
        let block_frames = geometry.block_frames as u64;
        let mut scratch = vec![0.0_f32; block_samples];
        let loopback_samples = loopback_frames
            .checked_mul(geometry.channels)
            .ok_or(DeviceError::RingTooLarge)?;
        let mut loopback = vec![0.0_f32; loopback_samples];
        let handle = thread::Builder::new()
            .name("hsk-score-device-software-out".into())
            .spawn(move || {
                let mut written = 0_usize;
                clock_loop(&thread_flags, sample_rate_hz, block_frames, || {
                    render_callback(&mut consumer, &mut scratch);
                    if written + block_samples <= loopback.len() {
                        loopback[written..written + block_samples].copy_from_slice(&scratch);
                        written += block_samples;
                    }
                });
                loopback.truncate(written);
                ThreadEnd {
                    residual_frames: consumer.available_frames(),
                    loopback,
                }
            })
            .map_err(|_| DeviceError::Unavailable)?;
        Ok(Self {
            flags,
            counters,
            thread: Some(handle),
            permit: Some(permit),
        })
    }

    /// Starts the input clock: every tick the fake hardware produces one block of the ramp and
    /// hands it to `capture_callback`.
    pub fn spawn_input(
        mut producer: Producer,
        geometry: &RingGeometry,
        sample_rate_hz: u32,
        counters: Arc<Counters>,
        permit: Permit,
    ) -> Result<Self, DeviceError> {
        let flags = new_flags();
        let thread_flags = Arc::clone(&flags);
        let block_frames = geometry.block_frames as u64;
        let mut scratch = vec![0.0_f32; geometry.block_samples];
        let handle = thread::Builder::new()
            .name("hsk-score-device-software-in".into())
            .spawn(move || {
                let mut next = 0_u64;
                clock_loop(&thread_flags, sample_rate_hz, block_frames, || {
                    for sample in scratch.iter_mut() {
                        next += 1;
                        *sample = next as f32;
                    }
                    capture_callback(&mut producer, &scratch);
                });
                ThreadEnd {
                    residual_frames: producer.occupied_frames(),
                    loopback: Vec::new(),
                }
            })
            .map_err(|_| DeviceError::Unavailable)?;
        Ok(Self {
            flags,
            counters,
            thread: Some(handle),
            permit: Some(permit),
        })
    }

    /// Simulates device loss: counts it once, stops the callbacks and makes the control side's
    /// next push return `PushError::DeviceLost`.
    pub fn inject_device_loss(&self) {
        if !self.flags.lost.swap(true, Ordering::AcqRel) {
            self.counters.add_device_lost();
        }
        self.wake();
    }

    /// Asks the clock thread to stop; confirm with [`Self::wait_stopped`].
    pub fn stop(&self) {
        self.flags.stop.store(true, Ordering::Release);
        self.wake();
    }

    /// Polls (control thread only) until the clock thread acknowledged it stopped calling the
    /// callback, or `timeout` passes.
    pub fn wait_stopped(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.flags.acked.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
        true
    }

    fn wake(&self) {
        if let Some(handle) = &self.thread {
            handle.thread().unpark();
        }
    }

    fn finish(&mut self) -> CloseReport {
        self.flags.stop.store(true, Ordering::Release);
        self.wake();
        let (joined, end) = match self.thread.take() {
            Some(handle) => match handle.join() {
                Ok(end) => (true, end),
                Err(_) => (
                    false,
                    ThreadEnd {
                        residual_frames: 0,
                        loopback: Vec::new(),
                    },
                ),
            },
            None => (
                true,
                ThreadEnd {
                    residual_frames: 0,
                    loopback: Vec::new(),
                },
            ),
        };
        let released = self.permit.as_mut().is_some_and(Permit::release);
        CloseReport::from_counters(
            self.counters.snapshot(),
            released,
            joined,
            end.residual_frames,
            end.loopback,
        )
    }

    /// Stops, joins the thread, releases the permit and reports. Off the callback thread.
    pub fn close(mut self) -> CloseReport {
        self.finish()
    }
}

fn new_flags() -> Arc<Flags> {
    Arc::new(Flags {
        stop: AtomicBool::new(false),
        lost: AtomicBool::new(false),
        acked: AtomicBool::new(false),
    })
}

impl Drop for ClockedSoftwareDevice {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.finish();
        }
    }
}

impl StreamBackend for ClockedSoftwareDevice {
    fn request_stop(&self) {
        self.stop();
    }
    fn stop_acknowledged(&self) -> bool {
        self.flags.acked.load(Ordering::Acquire)
    }
    fn inject_device_loss(&self) -> bool {
        ClockedSoftwareDevice::inject_device_loss(self);
        true
    }
    fn close(self: Box<Self>) -> CloseReport {
        (*self).close()
    }
}

/// The software provider: one output and one input device, bounded admission, grant-checked.
pub struct SoftwareProvider {
    admission: Admission,
    options: SoftwareOptions,
}

impl SoftwareProvider {
    pub fn new(limits: Limits) -> Self {
        Self::with_options(limits, SoftwareOptions::default())
    }
    pub fn with_options(limits: Limits, options: SoftwareOptions) -> Self {
        Self {
            admission: Admission::new(limits),
            options,
        }
    }
    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    /// Refusal order: cancel, grant, device, config, layout, admission. Nothing is allocated and
    /// no permit exists until every check passed.
    fn prepare(
        &self,
        direction: Direction,
        device: &DeviceId,
        config: &StreamConfig,
        grant: Option<&DeviceGrant>,
        cancel: &CancellationToken,
    ) -> Result<(RingGeometry, Permit), DeviceError> {
        cancel.check().map_err(|_| DeviceError::Canceled)?;
        let grant = grant.ok_or(DeviceError::Unauthorized)?;
        if !grant.permits(direction, device) {
            return Err(DeviceError::Unauthorized);
        }
        let known = match direction {
            Direction::Output => SOFTWARE_OUTPUT_DEVICE,
            Direction::Input => SOFTWARE_INPUT_DEVICE,
        };
        if device.as_str() != known {
            return Err(DeviceError::UnknownDevice);
        }
        let geometry = config.validate(self.admission.limits())?;
        if config.layout.channels() > usize::from(SOFTWARE_MAX_CHANNELS) {
            return Err(DeviceError::UnsupportedLayout);
        }
        let loopback_bytes = match direction {
            Direction::Output => self
                .options
                .loopback_frames
                .checked_mul(geometry.channels * crate::config::SAMPLE_BYTES)
                .ok_or(DeviceError::RingTooLarge)?,
            Direction::Input => 0,
        };
        if loopback_bytes > self.admission.limits().max_ring_bytes {
            return Err(DeviceError::RingTooLarge);
        }
        let permit = self.admission.acquire(geometry.ring_bytes + loopback_bytes)?;
        Ok((geometry, permit))
    }
}

impl DeviceProvider for SoftwareProvider {
    fn id(&self) -> &'static str {
        "software-clocked"
    }

    fn enumerate(&self, grant: Option<&DeviceGrant>) -> Result<Vec<DeviceInfo>, DeviceError> {
        if !grant.is_some_and(DeviceGrant::permits_enumerate) {
            return Err(DeviceError::Unauthorized);
        }
        fn info(
            id: &str,
            name: &str,
            outputs: u16,
            inputs: u16,
        ) -> Result<DeviceInfo, DeviceError> {
            Ok(DeviceInfo {
                id: DeviceId::parse(id)?,
                name: name.to_owned(),
                max_output_channels: outputs,
                max_input_channels: inputs,
                min_sample_rate_hz: 8_000,
                max_sample_rate_hz: 192_000,
            })
        }
        Ok(vec![
            info(
                SOFTWARE_OUTPUT_DEVICE,
                "Software clocked output",
                SOFTWARE_MAX_CHANNELS,
                0,
            )?,
            info(
                SOFTWARE_INPUT_DEVICE,
                "Software clocked input",
                0,
                SOFTWARE_MAX_CHANNELS,
            )?,
        ])
    }

    fn open_output(
        &self,
        device: &DeviceId,
        config: &StreamConfig,
        grant: Option<&DeviceGrant>,
        cancel: &CancellationToken,
    ) -> Result<OutputStream, DeviceError> {
        let (geometry, permit) = self.prepare(Direction::Output, device, config, grant, cancel)?;
        let counters = Arc::new(Counters::default());
        let (producer, consumer) = ring(&geometry, Arc::clone(&counters));
        let backend = ClockedSoftwareDevice::spawn_output(
            consumer,
            &geometry,
            config.sample_rate_hz,
            self.options.loopback_frames,
            counters,
            permit,
        )?;
        Ok(OutputStream::new(producer, Box::new(backend), config.clone()))
    }

    fn open_input(
        &self,
        device: &DeviceId,
        config: &StreamConfig,
        grant: Option<&DeviceGrant>,
        cancel: &CancellationToken,
    ) -> Result<InputStream, DeviceError> {
        let (geometry, permit) = self.prepare(Direction::Input, device, config, grant, cancel)?;
        let counters = Arc::new(Counters::default());
        let (producer, consumer) = ring(&geometry, Arc::clone(&counters));
        let backend = ClockedSoftwareDevice::spawn_input(
            producer,
            &geometry,
            config.sample_rate_hz,
            counters,
            permit,
        )?;
        Ok(InputStream::new(consumer, Box::new(backend), config.clone()))
    }
}
