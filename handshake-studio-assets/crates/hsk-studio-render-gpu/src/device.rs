//! Headless device lifecycle: adapter/device acquire (no surface, no window, no focus), a
//! generation counter, device-loss latching and typed `Unavailable`. wgpu types stay private
//! fields; everything public here is plain data (STU-ARC-002).
use crate::error::{GpuError, Reason};
use crate::kernel::Kernel;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::render_plan::BackendError;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

/// Drive a wgpu future to completion on the calling thread. The native futures are ready on the
/// first poll; the bound exists so a misbehaving driver becomes a typed timeout, not a hang.
pub(crate) fn block_on_bounded<F: Future>(future: F, limit: Option<Duration>) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    let started = Instant::now();
    let limit = limit.unwrap_or(Duration::from_secs(30));
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return Some(value);
        }
        if started.elapsed() >= limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendChoice {
    /// Platform-preferred order: Windows DX12 then Vulkan; Apple Metal; elsewhere Vulkan.
    Auto,
    Vulkan,
    Dx12,
    Metal,
    /// Never acquire a device: `acquire` returns `Unavailable(NoAdapter)` (explicit kill switch,
    /// and the hardware-independent way to exercise the unavailable path).
    None,
}
impl BackendChoice {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vulkan => "vulkan",
            Self::Dx12 => "dx12",
            Self::Metal => "metal",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceOptions {
    pub backend: BackendChoice,
    /// Cap for any single device buffer, further limited by the device's own limits.
    pub max_buffer_bytes: u64,
    /// Budget for all live device allocations of one backend (see `residency`).
    pub residency_budget_bytes: u64,
    /// Longest chunk side (texels) the executor will use; shrinks to fit the budgets.
    pub chunk_side: u32,
    /// Bound on adapter and device requests.
    pub acquire_timeout: Duration,
}
impl Default for DeviceOptions {
    fn default() -> Self {
        Self {
            backend: BackendChoice::Auto,
            max_buffer_bytes: 64 << 20,
            residency_budget_bytes: 256 << 20,
            chunk_side: 512,
            acquire_timeout: Duration::from_secs(20),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterKind {
    Discrete,
    Integrated,
    Virtual,
    Cpu,
    Unknown,
}
impl AdapterKind {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Discrete => "discrete",
            Self::Integrated => "integrated",
            Self::Virtual => "virtual",
            Self::Cpu => "cpu",
            Self::Unknown => "unknown",
        }
    }
}

/// Plain-data description of the acquired device (backend-qualified proof, STU-ARC-015).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub backend: &'static str,
    pub adapter_kind: AdapterKind,
    pub adapter_name: String,
    pub generation: u64,
}
impl DeviceInfo {
    /// Receipt text: `backend:kind:name`.
    pub fn text(&self) -> String {
        format!(
            "{}:{}:{}",
            self.backend,
            self.adapter_kind.key(),
            self.adapter_name
        )
    }
}

/// Latches written by wgpu callbacks (device lost, uncaptured error).
#[derive(Default)]
struct Signals {
    lost: AtomicBool,
    fault: AtomicBool,
}

/// One device generation. All wgpu objects are private; a lost context is never reused: acquire a
/// new one with a higher generation.
pub struct GpuContext {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) kernel: Kernel,
    signals: Arc<Signals>,
    info: DeviceInfo,
    /// Effective cap for one buffer: `min(options, device limits)`.
    pub(crate) max_buffer_bytes: u64,
}

impl GpuContext {
    /// Acquire headless at generation 1.
    pub fn acquire(options: &DeviceOptions, cancel: &CancellationToken) -> Result<Self, GpuError> {
        Self::acquire_next(options, cancel, 0)
    }

    /// Acquire as the successor of `previous_generation` (generation + 1), so handles from the
    /// previous device are refused by generation.
    pub fn acquire_next(
        options: &DeviceOptions,
        cancel: &CancellationToken,
        previous_generation: u64,
    ) -> Result<Self, GpuError> {
        let canceled = |_| GpuError::Backend(BackendError::Canceled);
        cancel.check().map_err(canceled)?;
        let candidates: &[wgpu::Backends] = match options.backend {
            BackendChoice::None => return Err(GpuError::Unavailable(Reason::NoAdapter)),
            // Platform-preferred order, one backend per instance: a single-backend instance
            // initialises in a fraction of the time of a multi-backend one.
            BackendChoice::Auto if cfg!(windows) => &[wgpu::Backends::DX12, wgpu::Backends::VULKAN],
            BackendChoice::Auto if cfg!(target_vendor = "apple") => &[wgpu::Backends::METAL],
            BackendChoice::Auto => &[wgpu::Backends::VULKAN],
            BackendChoice::Vulkan => &[wgpu::Backends::VULKAN],
            BackendChoice::Dx12 => &[wgpu::Backends::DX12],
            BackendChoice::Metal => &[wgpu::Backends::METAL],
        };
        let mut failure = Reason::NoAdapter;
        let mut found = None;
        for &backends in candidates {
            cancel.check().map_err(canceled)?;
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends,
                // No validation layers or debug overlays: quiet, deterministic headless operation.
                flags: wgpu::InstanceFlags::empty(),
                ..wgpu::InstanceDescriptor::default()
            });
            match block_on_bounded(
                instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    force_fallback_adapter: false,
                    compatible_surface: None,
                }),
                Some(options.acquire_timeout),
            ) {
                Some(Ok(adapter)) => {
                    found = Some(adapter);
                    break;
                }
                Some(Err(_)) => failure = Reason::NoAdapter,
                None => failure = Reason::Timeout,
            }
        }
        let adapter = found.ok_or(GpuError::Unavailable(failure))?;
        cancel.check().map_err(canceled)?;

        let limits = wgpu::Limits::downlevel_defaults();
        let (device, queue) = block_on_bounded(
            adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("hsk-studio-render-gpu"),
                required_features: wgpu::Features::empty(),
                required_limits: limits.clone(),
                ..wgpu::DeviceDescriptor::default()
            }),
            Some(options.acquire_timeout),
        )
        .ok_or(GpuError::Unavailable(Reason::Timeout))?
        .map_err(|_| GpuError::Unavailable(Reason::NoDevice))?;

        let signals = Arc::new(Signals::default());
        let lost = Arc::clone(&signals);
        device.set_device_lost_callback(move |_reason, _message| {
            lost.lost.store(true, Ordering::Release);
        });
        let fault = Arc::clone(&signals);
        device.on_uncaptured_error(Arc::new(move |_error: wgpu::Error| {
            fault.fault.store(true, Ordering::Release);
        }));

        let kernel = Kernel::new(&device)?;
        let raw = adapter.get_info();
        let info = DeviceInfo {
            backend: match raw.backend {
                wgpu::Backend::Vulkan => "vulkan",
                wgpu::Backend::Dx12 => "dx12",
                wgpu::Backend::Metal => "metal",
                wgpu::Backend::Gl => "gl",
                _ => "other",
            },
            adapter_kind: match raw.device_type {
                wgpu::DeviceType::DiscreteGpu => AdapterKind::Discrete,
                wgpu::DeviceType::IntegratedGpu => AdapterKind::Integrated,
                wgpu::DeviceType::VirtualGpu => AdapterKind::Virtual,
                wgpu::DeviceType::Cpu => AdapterKind::Cpu,
                wgpu::DeviceType::Other => AdapterKind::Unknown,
            },
            adapter_name: raw.name,
            generation: previous_generation.saturating_add(1),
        };
        let device_limits = device.limits();
        let max_buffer_bytes = options
            .max_buffer_bytes
            .min(device_limits.max_buffer_size)
            .min(u64::from(device_limits.max_storage_buffer_binding_size));
        Ok(Self {
            device,
            queue,
            kernel,
            signals,
            info,
            max_buffer_bytes,
        })
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }
    pub fn generation(&self) -> u64 {
        self.info.generation
    }
    pub fn is_lost(&self) -> bool {
        self.signals.lost.load(Ordering::Acquire)
    }
    /// `Err(DeviceLost{generation})` once the loss latch is set.
    pub fn check_alive(&self) -> Result<(), GpuError> {
        if self.is_lost() {
            Err(GpuError::DeviceLost {
                generation: self.info.generation,
            })
        } else {
            Ok(())
        }
    }
    /// Read and clear the uncaptured-error latch (an API failure that was not a loss).
    pub(crate) fn take_fault(&self) -> bool {
        self.signals.fault.swap(false, Ordering::AcqRel)
    }
    /// Fault injection for supervisors and tests: destroys the device through the real wgpu path
    /// and latches the same loss state the device-lost callback sets.
    pub fn simulate_device_loss(&self) {
        self.device.destroy();
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.signals.lost.store(true, Ordering::Release);
    }
}
