//! Internal read-only Vulkan memory observer for `hsk-studio-render-gpu`.
//!
//! [`observe`] reads heap budgets and estimated usage through `VK_EXT_memory_budget`,
//! together with the physical device's heap and memory-type descriptions. Values are
//! copied from one driver query and may change immediately. They are neither exact
//! resource accounting nor global machine totals. Zero budget or usage is preserved
//! when the driver reports it; a missing capability is an error, never a zero snapshot.
//!
//! No native handles escape. No Vulkan allocations, device changes or synchronization
//! operations are performed. This helper cannot identify private HAL allocation blocks,
//! connect reflected resource IDs to those blocks, or classify resources as Dedicated,
//! Shared or Staging. Native heap/type indices are scoped to the observed physical device.
//!
//! Callers retain ownership and lifetime of their wgpu device. The parent crate can keep
//! `forbid(unsafe_code)`; the native borrow and query are isolated and documented here.
//! Pinned API basis: wgpu 27.0.1 `Device::as_hal`, wgpu-hal 27.0.4 Vulkan device/instance
//! accessors, and ash 0.38.0+1.3.281 memory-properties2 and memory-budget structures.
#![deny(unsafe_op_in_unsafe_fn)]

use ash::vk;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VulkanMemorySnapshot {
    pub heaps: Vec<VulkanMemoryHeap>,
    pub memory_types: Vec<VulkanMemoryType>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VulkanMemoryHeap {
    pub index: u32,
    pub size_bytes: u64,
    /// Unmodified `VkMemoryHeapFlags` bits; not an application memory classification.
    pub flags: u32,
    pub budget_bytes: u64,
    /// Driver estimate from `VkPhysicalDeviceMemoryBudgetPropertiesEXT::heapUsage`.
    pub estimated_usage_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VulkanMemoryType {
    pub index: u32,
    pub heap_index: u32,
    /// Unmodified `VkMemoryPropertyFlags` bits.
    pub property_flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    VulkanBackendUnavailable,
    MemoryBudgetExtensionUnavailable,
    MemoryProperties2Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationError {
    Unsupported(UnsupportedReason),
    InvalidHeapCount(u32),
    InvalidMemoryTypeCount(u32),
    InvalidHeapIndex {
        memory_type_index: u32,
        heap_index: u32,
        heap_count: u32,
    },
}

impl std::fmt::Display for ObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Vulkan memory observation failed: {self:?}")
    }
}
impl std::error::Error for ObservationError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueryRoute {
    Core11,
    Khr,
}

fn query_route(
    memory_budget_enabled: bool,
    instance_api_version: u32,
    properties2_enabled: bool,
) -> Result<QueryRoute, ObservationError> {
    if !memory_budget_enabled {
        return Err(ObservationError::Unsupported(
            UnsupportedReason::MemoryBudgetExtensionUnavailable,
        ));
    }
    if instance_api_version >= vk::API_VERSION_1_1 {
        Ok(QueryRoute::Core11)
    } else if properties2_enabled {
        Ok(QueryRoute::Khr)
    } else {
        Err(ObservationError::Unsupported(
            UnsupportedReason::MemoryProperties2Unavailable,
        ))
    }
}

/// Read one aggregate heap/type observation from the device's actual Vulkan backend.
/// Unsupported backends and required extensions return typed errors without fallback.
pub fn observe(device: &wgpu::Device) -> Result<VulkanMemorySnapshot, ObservationError> {
    // Match the native Vulkan platforms enabled by wgpu 27.0.1's `vulkan` feature.
    #[cfg(any(windows, target_os = "linux", target_os = "android", target_os = "freebsd"))]
    {
        // SAFETY: Borrow only through wgpu's guard. No native resource is destroyed,
        // mutated or retained beyond that guard, which spans the complete query below.
        let guard = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }
            .ok_or(ObservationError::Unsupported(
                UnsupportedReason::VulkanBackendUnavailable,
            ))?;
        let shared = guard.shared_instance();
        let route = query_route(
            guard.enabled_device_extensions().contains(&ash::ext::memory_budget::NAME),
            shared.instance_api_version(),
            shared.extensions().contains(&ash::khr::get_physical_device_properties2::NAME),
        )?;
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        let properties = {
            let mut properties2 = vk::PhysicalDeviceMemoryProperties2::default()
                .push_next(&mut budget);
            match route {
                QueryRoute::Core11 => {
                    // SAFETY: The guarded physical device belongs to this live instance.
                    // Its API version admits this core entry point. Both output structs
                    // have valid sType values; the stack-owned pNext chain stays live
                    // and exclusively borrowed until this synchronous call returns.
                    unsafe {
                        shared.raw_instance().get_physical_device_memory_properties2(
                            guard.raw_physical_device(), &mut properties2,
                        );
                    }
                }
                QueryRoute::Khr => {
                    let extension = ash::khr::get_physical_device_properties2::Instance::new(
                        shared.entry(), shared.raw_instance(),
                    );
                    // SAFETY: This instance enabled the KHR extension. The same guarded
                    // physical-device lifetime and initialized output-chain conditions
                    // as the core branch apply. No pointers outlive this call.
                    unsafe {
                        extension.get_physical_device_memory_properties2(
                            guard.raw_physical_device(), &mut properties2,
                        );
                    }
                }
            }
            properties2.memory_properties
        };
        // End the pNext borrow before reading budget arrays; return only copied values.
        snapshot_from_raw(&properties, &budget)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "android", target_os = "freebsd")))]
    {
        let _ = device;
        Err(ObservationError::Unsupported(
            UnsupportedReason::VulkanBackendUnavailable,
        ))
    }
}

fn snapshot_from_raw(
    properties: &vk::PhysicalDeviceMemoryProperties,
    budget: &vk::PhysicalDeviceMemoryBudgetPropertiesEXT<'_>,
) -> Result<VulkanMemorySnapshot, ObservationError> {
    let heap_count = usize::try_from(properties.memory_heap_count)
        .ok().filter(|count| (1..=vk::MAX_MEMORY_HEAPS).contains(count))
        .ok_or(ObservationError::InvalidHeapCount(properties.memory_heap_count))?;
    let type_count = usize::try_from(properties.memory_type_count)
        .ok().filter(|count| (1..=vk::MAX_MEMORY_TYPES).contains(count))
        .ok_or(ObservationError::InvalidMemoryTypeCount(properties.memory_type_count))?;
    let mut memory_types = Vec::with_capacity(type_count);
    for (index, entry) in (0..properties.memory_type_count)
        .zip(&properties.memory_types[..type_count])
    {
        if entry.heap_index >= properties.memory_heap_count {
            return Err(ObservationError::InvalidHeapIndex {
                memory_type_index: index,
                heap_index: entry.heap_index,
                heap_count: properties.memory_heap_count,
            });
        }
        memory_types.push(VulkanMemoryType {
            index, heap_index: entry.heap_index,
            property_flags: entry.property_flags.as_raw(),
        });
    }
    let heaps = (0..properties.memory_heap_count)
        .zip(&properties.memory_heaps[..heap_count])
        .zip(budget.heap_budget[..heap_count].iter().zip(&budget.heap_usage[..heap_count]))
        .map(|((index, heap), (&budget_bytes, &estimated_usage_bytes))| VulkanMemoryHeap {
            index, size_bytes: heap.size, flags: heap.flags.as_raw(),
            budget_bytes, estimated_usage_bytes,
        })
        .collect();
    Ok(VulkanMemorySnapshot { heaps, memory_types })
}

#[cfg(test)]
mod tests;
