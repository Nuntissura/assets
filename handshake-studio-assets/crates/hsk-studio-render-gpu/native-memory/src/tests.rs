use super::*;

fn fixture() -> (vk::PhysicalDeviceMemoryProperties, vk::PhysicalDeviceMemoryBudgetPropertiesEXT<'static>) {
    let properties = vk::PhysicalDeviceMemoryProperties::default()
        .memory_heaps(&[
            vk::MemoryHeap::default().size(8192).flags(vk::MemoryHeapFlags::DEVICE_LOCAL),
            vk::MemoryHeap::default().size(4096),
        ])
        .memory_types(&[
            vk::MemoryType::default().heap_index(1)
                .property_flags(vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT),
            vk::MemoryType::default().heap_index(0)
                .property_flags(vk::MemoryPropertyFlags::DEVICE_LOCAL),
            vk::MemoryType::default().heap_index(1)
                .property_flags(vk::MemoryPropertyFlags::HOST_CACHED),
        ]);
    let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
    budget.heap_budget[..2].copy_from_slice(&[7000, 3000]);
    budget.heap_usage[..2].copy_from_slice(&[6000, 2000]);
    (properties, budget)
}

#[test]
fn copies_heap_and_type_associations_without_classification() {
    let (properties, budget) = fixture();
    let snapshot = snapshot_from_raw(&properties, &budget).unwrap();
    assert_eq!(snapshot, VulkanMemorySnapshot {
        heaps: vec![
            VulkanMemoryHeap { index: 0, size_bytes: 8192,
                flags: vk::MemoryHeapFlags::DEVICE_LOCAL.as_raw(),
                budget_bytes: 7000, estimated_usage_bytes: 6000 },
            VulkanMemoryHeap { index: 1, size_bytes: 4096, flags: 0,
                budget_bytes: 3000, estimated_usage_bytes: 2000 },
        ],
        memory_types: vec![
            VulkanMemoryType { index: 0, heap_index: 1,
                property_flags: (vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT).as_raw() },
            VulkanMemoryType { index: 1, heap_index: 0,
                property_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL.as_raw() },
            VulkanMemoryType { index: 2, heap_index: 1,
                property_flags: vk::MemoryPropertyFlags::HOST_CACHED.as_raw() },
        ],
    });
}

#[test]
fn rejects_empty_or_out_of_bounds_driver_counts() {
    let (properties, budget) = fixture();
    for count in [0, 17, u32::MAX] {
        let mut invalid = properties;
        invalid.memory_heap_count = count;
        assert_eq!(snapshot_from_raw(&invalid, &budget),
            Err(ObservationError::InvalidHeapCount(count)));
    }
    for count in [0, 33, u32::MAX] {
        let mut invalid = properties;
        invalid.memory_type_count = count;
        assert_eq!(snapshot_from_raw(&invalid, &budget),
            Err(ObservationError::InvalidMemoryTypeCount(count)));
    }
}

#[test]
fn rejects_type_references_outside_the_reported_heap_set() {
    let (mut properties, budget) = fixture();
    for heap_index in [2, 16, u32::MAX] {
        properties.memory_types[1].heap_index = heap_index;
        assert_eq!(snapshot_from_raw(&properties, &budget),
            Err(ObservationError::InvalidHeapIndex {
                memory_type_index: 1, heap_index, heap_count: 2,
            }));
    }
}

#[test]
fn ignores_array_slots_outside_driver_counts_and_preserves_estimates() {
    let (mut properties, mut budget) = fixture();
    properties.memory_types[3].heap_index = u32::MAX;
    properties.memory_heaps[2].size = u64::MAX;
    budget.heap_budget[2] = u64::MAX;
    budget.heap_usage[2] = u64::MAX;
    budget.heap_budget[0] = 0;
    budget.heap_usage[0] = 42;
    budget.heap_budget[1] = 0;
    budget.heap_usage[1] = 0;
    let snapshot = snapshot_from_raw(&properties, &budget).unwrap();
    assert_eq!(snapshot.heaps.len(), 2);
    assert_eq!(snapshot.memory_types.len(), 3);
    assert_eq!(snapshot.heaps[0].budget_bytes, 0);
    assert_eq!(snapshot.heaps[0].estimated_usage_bytes, 42);
    assert_eq!(snapshot.heaps[1].budget_bytes, 0);
    assert_eq!(snapshot.heaps[1].estimated_usage_bytes, 0);
}

#[test]
fn accepts_complete_fixed_size_driver_arrays() {
    let mut properties = vk::PhysicalDeviceMemoryProperties {
        memory_heap_count: 16, memory_type_count: 32, ..Default::default()
    };
    properties.memory_types[31].heap_index = 15;
    let snapshot = snapshot_from_raw(&properties,
        &vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default()).unwrap();
    assert_eq!(snapshot.heaps.len(), vk::MAX_MEMORY_HEAPS);
    assert_eq!(snapshot.memory_types.len(), vk::MAX_MEMORY_TYPES);
    assert_eq!(snapshot.memory_types[31].heap_index, 15);
}

#[test]
fn requires_budget_and_a_valid_properties2_dispatch_route() {
    for api in [vk::API_VERSION_1_0, vk::API_VERSION_1_1, vk::API_VERSION_1_3] {
        assert_eq!(query_route(false, api, true),
            Err(ObservationError::Unsupported(UnsupportedReason::MemoryBudgetExtensionUnavailable)));
    }
    assert_eq!(query_route(true, vk::API_VERSION_1_0, false),
        Err(ObservationError::Unsupported(UnsupportedReason::MemoryProperties2Unavailable)));
    assert_eq!(query_route(true, vk::API_VERSION_1_0, true), Ok(QueryRoute::Khr));
    assert_eq!(query_route(true, vk::API_VERSION_1_1, false), Ok(QueryRoute::Core11));
    assert_eq!(query_route(true, vk::API_VERSION_1_3, true), Ok(QueryRoute::Core11));
}
