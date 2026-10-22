//! Optional native output backend over `cpal` 0.18.2 (feature `cpal-backend`, default OFF).
//!
//! Only `f32` output is served: a device that cannot give exactly the requested rate, channel
//! count and `f32` is refused with `Unavailable` (no resampling, conversion or downmix). The
//! `cpal` stream lives on one owner thread that this crate spawns, so nothing here depends on
//! whether a platform `Stream` is `Send`; the data callback is `render_callback`, the error
//! callback only flips atomics. Nothing audible is ever produced by this crate: callers push
//! digital silence or pre-rendered data, and an empty ring renders zeros.
//!
//! Native-device proof is NOT_PROVEN by compiling this module; see `tests/native_cpal.rs`.
use crate::callback::render_callback;
use crate::config::{
    DeviceGrant, DeviceId, DeviceInfo, Direction, Limits, RingGeometry, StreamConfig, authorize,
};
use crate::counters::Counters;
use crate::error::DeviceError;
use crate::open_wait::wait_ready;
use crate::provider::{
    Admission, CloseReport, DeviceProvider, InputStream, OutputStream, Permit, StreamBackend,
};
use crate::ring::{Consumer, ring};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hsk_studio_accord::CancellationToken;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Id of the host's default output device (resolved by name each open).
pub const CPAL_DEFAULT_OUTPUT: &str = "cpal-default-output";
const BUILD_TIMEOUT: Duration = Duration::from_secs(5);
const OPEN_WAIT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(20);

/// Default shared-mode format of the default output device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeFormat {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub is_f32: bool,
}

fn map_error(error: &cpal::Error) -> DeviceError {
    match error.kind() {
        cpal::ErrorKind::PermissionDenied => DeviceError::Unauthorized,
        cpal::ErrorKind::DeviceBusy | cpal::ErrorKind::ResourceExhausted => {
            DeviceError::AdmissionDenied
        }
        // Absent device/host, unsupported config and anything unclassified: typed, never
        // silently downgraded.
        _ => DeviceError::Unavailable,
    }
}

fn is_loss(error: &cpal::Error) -> bool {
    matches!(
        error.kind(),
        cpal::ErrorKind::DeviceNotAvailable
            | cpal::ErrorKind::StreamInvalidated
            | cpal::ErrorKind::HostUnavailable
    )
}

fn find_device(host: &cpal::Host, id: &DeviceId) -> Result<cpal::Device, DeviceError> {
    if id.as_str() == CPAL_DEFAULT_OUTPUT {
        return host.default_output_device().ok_or(DeviceError::Unavailable);
    }
    let devices = host.output_devices().map_err(|e| map_error(&e))?;
    for device in devices {
        if device.id().is_ok_and(|cid| cid.to_string() == id.as_str()) {
            return Ok(device);
        }
    }
    Err(DeviceError::UnknownDevice)
}

fn pick_config(
    device: &cpal::Device,
    rate: u32,
    channels: u16,
) -> Result<cpal::StreamConfig, DeviceError> {
    let ranges = device
        .supported_output_configs()
        .map_err(|e| map_error(&e))?;
    for range in ranges {
        if range.channels() == channels
            && range.sample_format() == cpal::SampleFormat::F32
            && range.contains_rate(rate)
        {
            return Ok(cpal::StreamConfig {
                channels,
                sample_rate: rate,
                buffer_size: cpal::BufferSize::Default,
            });
        }
    }
    Err(DeviceError::Unavailable)
}

struct Flags {
    stop: AtomicBool,
    lost: AtomicBool,
    acked: AtomicBool,
}

fn build_stream(
    device_id: &DeviceId,
    rate: u32,
    channels: u16,
    mut consumer: Consumer,
    counters: Arc<Counters>,
    flags: Arc<Flags>,
) -> Result<cpal::Stream, DeviceError> {
    let host = cpal::default_host();
    let device = find_device(&host, device_id)?;
    let config = pick_config(&device, rate, channels)?;
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                render_callback(&mut consumer, data);
            },
            move |error: cpal::Error| {
                if is_loss(&error) && !flags.lost.swap(true, Ordering::AcqRel) {
                    counters.add_device_lost();
                }
            },
            Some(BUILD_TIMEOUT),
        )
        .map_err(|e| map_error(&e))?;
    stream.play().map_err(|e| map_error(&e))?;
    Ok(stream)
}

/// Everything the owner thread needs; moved into the thread whole.
struct Owner {
    device_id: DeviceId,
    rate: u32,
    channels: u16,
    consumer: Consumer,
    counters: Arc<Counters>,
    flags: Arc<Flags>,
    permit: Permit,
    ready: SyncSender<Result<(), DeviceError>>,
}

impl Owner {
    /// Owner thread: builds the stream, reports readiness, waits for stop or loss, then pauses
    /// and drops the stream (cpal joins its audio thread) before acknowledging. Returns whether
    /// the admission permit was released by this exit.
    fn run(mut self) -> bool {
        let flags = Arc::clone(&self.flags);
        match build_stream(
            &self.device_id,
            self.rate,
            self.channels,
            self.consumer,
            self.counters,
            Arc::clone(&flags),
        ) {
            Err(error) => {
                let _ = self.ready.send(Err(error));
                flags.acked.store(true, Ordering::Release);
            }
            Ok(stream) => {
                let _ = self.ready.send(Ok(()));
                while !flags.stop.load(Ordering::Acquire) && !flags.lost.load(Ordering::Acquire) {
                    thread::park_timeout(POLL);
                }
                let _ = stream.pause();
                drop(stream);
                flags.acked.store(true, Ordering::Release);
            }
        }
        self.permit.release()
    }
}

struct CpalBackend {
    flags: Arc<Flags>,
    counters: Arc<Counters>,
    thread: Option<JoinHandle<bool>>,
}

impl CpalBackend {
    fn wake(&self) {
        if let Some(handle) = &self.thread {
            handle.thread().unpark();
        }
    }
}

impl StreamBackend for CpalBackend {
    fn request_stop(&self) {
        self.flags.stop.store(true, Ordering::Release);
        self.wake();
    }
    fn stop_acknowledged(&self) -> bool {
        self.flags.acked.load(Ordering::Acquire)
    }
    fn close(self: Box<Self>) -> CloseReport {
        let mut this = *self;
        this.request_stop();
        let (joined, released) = match this.thread.take() {
            Some(handle) => match handle.join() {
                Ok(released) => (true, released),
                Err(_) => (false, false),
            },
            None => (true, false),
        };
        CloseReport::from_counters(this.counters.snapshot(), released, joined, 0, Vec::new())
    }
}

impl Drop for CpalBackend {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.request_stop();
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
        }
    }
}

/// Native provider over the host's default audio API (WASAPI, ALSA, CoreAudio). Output only in
/// this slice; `open_input` is refused with `Unavailable`.
pub struct CpalProvider {
    admission: Admission,
}

impl CpalProvider {
    pub fn new(limits: Limits) -> Self {
        Self {
            admission: Admission::new(limits),
        }
    }
    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    /// Default shared-mode format of the default output device; needs an output grant. A
    /// `NativeFormat` with `is_f32 == false` will be refused by `open_output`.
    pub fn default_output_format(
        &self,
        grant: Option<&DeviceGrant>,
    ) -> Result<NativeFormat, DeviceError> {
        let id = DeviceId::parse(CPAL_DEFAULT_OUTPUT)?;
        authorize(grant, Direction::Output, &id)?;
        let host = cpal::default_host();
        let device = find_device(&host, &id)?;
        let config = device.default_output_config().map_err(|e| map_error(&e))?;
        Ok(NativeFormat {
            sample_rate_hz: config.sample_rate(),
            channels: config.channels(),
            is_f32: config.sample_format() == cpal::SampleFormat::F32,
        })
    }

    fn prepare(
        &self,
        device: &DeviceId,
        config: &StreamConfig,
        grant: Option<&DeviceGrant>,
        cancel: &CancellationToken,
    ) -> Result<(RingGeometry, Permit), DeviceError> {
        cancel.check().map_err(|_| DeviceError::Canceled)?;
        authorize(grant, Direction::Output, device)?;
        let geometry = config.validate(self.admission.limits())?;
        let permit = self.admission.acquire(geometry.ring_bytes)?;
        Ok((geometry, permit))
    }
}

impl DeviceProvider for CpalProvider {
    fn id(&self) -> &'static str {
        "cpal"
    }

    fn enumerate(&self, grant: Option<&DeviceGrant>) -> Result<Vec<DeviceInfo>, DeviceError> {
        if !grant.is_some_and(DeviceGrant::permits_enumerate) {
            return Err(DeviceError::Unauthorized);
        }
        let host = cpal::default_host();
        let describe = |device: &cpal::Device, id: DeviceId| {
            let (mut channels, mut low, mut high) = (0_u16, u32::MAX, 0_u32);
            if let Ok(ranges) = device.supported_output_configs() {
                for range in ranges.filter(|r| r.sample_format() == cpal::SampleFormat::F32) {
                    channels = channels.max(range.channels());
                    low = low.min(range.min_sample_rate());
                    high = high.max(range.max_sample_rate());
                }
            }
            DeviceInfo {
                id,
                name: device
                    .description()
                    .map(|d| d.name().to_owned())
                    .unwrap_or_default(),
                max_output_channels: channels,
                max_input_channels: 0,
                min_sample_rate_hz: if high == 0 { 0 } else { low },
                max_sample_rate_hz: high,
            }
        };
        let mut devices = Vec::new();
        if let Some(device) = host.default_output_device() {
            devices.push(describe(&device, DeviceId::parse(CPAL_DEFAULT_OUTPUT)?));
        }
        for device in host.output_devices().map_err(|e| map_error(&e))? {
            let parsed = device
                .id()
                .ok()
                .and_then(|cid| DeviceId::parse(&cid.to_string()).ok());
            if let Some(id) = parsed {
                devices.push(describe(&device, id));
            }
        }
        Ok(devices)
    }

    fn open_output(
        &self,
        device: &DeviceId,
        config: &StreamConfig,
        grant: Option<&DeviceGrant>,
        cancel: &CancellationToken,
    ) -> Result<OutputStream, DeviceError> {
        let (geometry, permit) = self.prepare(device, config, grant, cancel)?;
        let channels = u16::try_from(geometry.channels).map_err(|_| DeviceError::UnsupportedLayout)?;
        let counters = Arc::new(Counters::default());
        let (producer, consumer) = ring(&geometry, Arc::clone(&counters));
        let flags = Arc::new(Flags {
            stop: AtomicBool::new(false),
            lost: AtomicBool::new(false),
            acked: AtomicBool::new(false),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let owner = Owner {
            device_id: device.clone(),
            rate: config.sample_rate_hz,
            channels,
            consumer,
            counters: Arc::clone(&counters),
            flags: Arc::clone(&flags),
            permit,
            ready: ready_tx,
        };
        let handle = thread::Builder::new()
            .name("hsk-score-device-cpal".into())
            .spawn(move || owner.run())
            .map_err(|_| DeviceError::Unavailable)?;
        let backend = CpalBackend {
            flags,
            counters,
            thread: Some(handle),
        };
        // On a refusal, cancel or timeout `backend` drops: stop, wake and join (the owner thread
        // holds and releases the permit, so admission is only freed once the stream is gone).
        // The wait polls `cancel` every POLL; the join after a cancel is bounded by cpal's own
        // BUILD_TIMEOUT for a build that never returns (not provable without a hanging device).
        wait_ready(&ready_rx, cancel, OPEN_WAIT, POLL)?;
        Ok(OutputStream::new(producer, Box::new(backend), config.clone()))
    }

    fn open_input(
        &self,
        device: &DeviceId,
        _config: &StreamConfig,
        grant: Option<&DeviceGrant>,
        cancel: &CancellationToken,
    ) -> Result<InputStream, DeviceError> {
        cancel.check().map_err(|_| DeviceError::Canceled)?;
        authorize(grant, Direction::Input, device)?;
        Err(DeviceError::Unavailable)
    }
}
