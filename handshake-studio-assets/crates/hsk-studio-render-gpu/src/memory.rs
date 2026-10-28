//! Read-only native heap observations tied to the acquired device's identity and generation.
//! Driver budgets and usage estimates are aggregate, transient observations. They do not
//! identify resource allocations or establish Dedicated/Shared/Staging ownership.

use crate::{DeviceInfo, GpuContext};
pub use hsk_studio_render_gpu_native_memory::{
    ObservationError as MemoryObservationError,
    UnsupportedReason as MemoryObservationUnsupportedReason,
    VulkanMemoryHeap, VulkanMemorySnapshot, VulkanMemoryType,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceMemorySnapshot {
    pub device: DeviceInfo,
    pub memory: VulkanMemorySnapshot,
}

impl GpuContext {
    /// Query this context's Vulkan heaps, preserving its actual device generation.
    /// Missing backend capabilities return a typed error without fallback or zero data.
    pub fn observe_memory(&self) -> Result<DeviceMemorySnapshot, MemoryObservationError> {
        let memory = hsk_studio_render_gpu_native_memory::observe(&self.device)?;
        Ok(DeviceMemorySnapshot { device: self.info().clone(), memory })
    }
}
