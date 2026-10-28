//! Compile-only public API consumer. No device creation or observation is executed.
use hsk_studio_render_gpu::{
    DeviceInfo, DeviceMemorySnapshot, GpuContext, MemoryObservationError,
    MemoryObservationUnsupportedReason, VulkanMemoryHeap, VulkanMemorySnapshot, VulkanMemoryType,
};

fn read_contract(snapshot: &DeviceMemorySnapshot) {
    let _: &DeviceInfo = &snapshot.device;
    let _: &VulkanMemorySnapshot = &snapshot.memory;
    let _: &[VulkanMemoryHeap] = &snapshot.memory.heaps;
    let _: &[VulkanMemoryType] = &snapshot.memory.memory_types;
}

fn unsupported_contract(error: &MemoryObservationError) -> Option<MemoryObservationUnsupportedReason> {
    match error {
        MemoryObservationError::Unsupported(reason) => Some(*reason),
        _ => None,
    }
}

fn main() {
    let _observe: fn(&GpuContext) -> Result<DeviceMemorySnapshot, MemoryObservationError> =
        GpuContext::observe_memory;
    let _read: fn(&DeviceMemorySnapshot) = read_contract;
    let _unsupported: fn(&MemoryObservationError) -> Option<MemoryObservationUnsupportedReason> =
        unsupported_contract;
}
