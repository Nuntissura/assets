//! Typed GPU backend failures. Richer than the shared port vocabulary; mapped to
//! `pigment::render_plan::BackendError` at the port (`From<GpuError> for BackendError`).
//! Plain data only: no wgpu type appears here (STU-ARC-002).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_pigment::render_plan::BackendError;

/// Why no device could be acquired. Determinate typed outcome, not a panic or a skip (tests
/// admission_refusals_run_without_adapter, no_adapter_is_not_a_green_gpu_proof_when_the_gpu_is_required).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// No adapter on the requested backend(s), or the backend was set to `None`.
    NoAdapter,
    /// An adapter exists but refused to create a device with the required limits.
    NoDevice,
    /// Adapter or device request did not complete within the acquire timeout.
    Timeout,
}
impl Reason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoAdapter => "no_adapter",
            Self::NoDevice => "no_device",
            Self::Timeout => "adapter_timeout",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuError {
    /// Shared port failure (admission, source verification, cancel, sink, ...).
    Backend(BackendError),
    Unavailable(Reason),
    /// The device of `generation` was lost; a re-acquire is required (stale handles are refused).
    DeviceLost {
        generation: u64,
    },
    BudgetExceeded {
        requested: u64,
        budget: u64,
    },
    /// Unexpected GPU-side failure (validation error, failed mapping, timeout).
    Gpu(&'static str),
    /// An adapter and a device were acquired, but the WGSL shader or the compute pipeline failed
    /// validation (`shader_validation`, `shader_validation_timeout`). A defect of this crate, not an
    /// environment condition: distinct from `Unavailable` (no adapter/device), so a test or a
    /// supervisor can tell it from "no GPU here" (test pipeline_failure_is_typed_and_fails_the_gpu_gate).
    Pipeline(&'static str),
}
impl From<BackendError> for GpuError {
    fn from(error: BackendError) -> Self {
        Self::Backend(error)
    }
}
impl From<GpuError> for BackendError {
    fn from(error: GpuError) -> Self {
        match error {
            GpuError::Backend(e) => e,
            GpuError::Unavailable(reason) => BackendError::Unavailable(reason.code()),
            GpuError::DeviceLost { generation } => BackendError::DeviceLost { generation },
            GpuError::BudgetExceeded { requested, budget } => BackendError::BudgetExceeded {
                requested,
                limit: budget,
            },
            GpuError::Gpu(what) => BackendError::Unavailable(what),
            // The port vocabulary has no internal-defect variant; the code (`shader_validation`)
            // keeps it distinct from `no_adapter` / `no_device` at the port.
            GpuError::Pipeline(what) => BackendError::Unavailable(what),
        }
    }
}
impl GpuError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Backend(e) => e.code(),
            Self::Unavailable(_) => "backend_unavailable",
            Self::DeviceLost { .. } => "device_lost",
            Self::BudgetExceeded { .. } => "budget_exceeded",
            Self::Gpu(what) | Self::Pipeline(what) => what,
        }
    }
}
impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "backend_unavailable({})", reason.code()),
            Self::DeviceLost { generation } => write!(f, "device_lost(generation {generation})"),
            Self::Pipeline(what) => write!(f, "pipeline_failed({what})"),
            other => f.write_str(other.code()),
        }
    }
}
impl std::error::Error for GpuError {}
