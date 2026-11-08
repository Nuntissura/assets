//! The transient device owner (STU-ARC-024, STU-ARC-032, HBR-HOST-003): asset-local, never host acceptance. One
//! device per process. Panes draw into owner images, so a later hand-over to Handshake swaps owner and presenter while
//! pane code stays as it is. Every `unsafe` block names the Vulkan rule it relies on.
use crate::refusal::{Refusal, rgba_len};
use ash::vk::{self, Handle};
use std::ffi::CStr;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Bound on every fence wait (no unbounded wait, with or without the validation layer).
pub const FENCE_TIMEOUT_NS: u64 = 5_000_000_000;

static OWNER_LIVE: AtomicBool = AtomicBool::new(false);

/// Who issued the device. Only the transient asset-local owner exists in this lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issuer {
    TransientAssetLocal,
}

impl Issuer {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TransientAssetLocal => "transient_asset_local",
        }
    }
}

/// How the owner opens its device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerConfig {
    /// Enable `VK_KHR_surface`, `VK_KHR_win32_surface` and `VK_KHR_swapchain`. Offscreen runs leave it off: no
    /// surface extension, no window.
    pub surface: bool,
    /// Keep implicit loader layers (overlays) on. Off by default: the owner sets `VK_LOADER_LAYERS_DISABLE=~implicit~`
    /// in its own process before loading the loader; nothing system-wide changes.
    pub allow_overlays: bool,
    /// Accept a CPU-type device (a software rasteriser). Off by default.
    pub allow_cpu_device: bool,
}

impl Default for OwnerConfig {
    fn default() -> Self {
        Self { surface: false, allow_overlays: false, allow_cpu_device: false }
    }
}

/// The device as the receipt names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: &'static str,
    /// Device API version (major, minor, patch).
    pub api_version: [u32; 3],
    pub driver_version: u32,
    pub driver_name: String,
    pub driver_info: String,
    /// `VkPhysicalDeviceIDProperties::deviceUUID`, lowercase hex.
    pub device_uuid: String,
    pub queue_family: u32,
    /// Device features enabled at creation (Vulkan 1.3 core names).
    pub features: Vec<&'static str>,
}

/// Loader layers seen and disabled in this process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerReport {
    /// The `VK_LOADER_LAYERS_DISABLE` value this process set, or `none` when overlays were allowed.
    pub disabled: &'static str,
    /// Layers the loader still enumerates after the disable (empty here when every layer is implicit).
    pub present: Vec<String>,
    /// Layers enabled on the instance: the Khronos validation layer when [`Owner::validation`] is `on`, else none.
    pub enabled: Vec<String>,
}

/// The Khronos validation layer's name, as the loader lists it.
pub const VALIDATION_LAYER: &str = "VK_LAYER_KHRONOS_validation";

/// One validation message id: the layer's message id name (a VUID or a check name) and number, never message text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationId {
    /// `error` or `warning`.
    pub severity: &'static str,
    pub name: String,
    pub number: i32,
}

/// Validation layer messages counted in this process, over every owner it opened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// `on` (layer enabled), `absent` (debug build, layer not listed), `off` (release build) or `none` (no owner yet).
    pub state: &'static str,
    pub errors: u64,
    /// Validation and performance warnings.
    pub warnings: u64,
    /// Loader-only (general) warnings, e.g. the implicit layers this process disables; not validation findings.
    pub loader_warnings: u64,
    /// The first 16 error and warning ids, in arrival order (loader-only warnings excluded).
    pub first: Vec<ValidationId>,
}

/// Message ids kept per process.
const FIRST_IDS: usize = 16;
/// 0 no owner yet, 1 on, 2 absent, 3 off: the state of the last owner this process opened.
static VALIDATION_STATE: AtomicU8 = AtomicU8::new(0);
static VALIDATION_ERRORS: AtomicU64 = AtomicU64::new(0);
static VALIDATION_WARNINGS: AtomicU64 = AtomicU64::new(0);
static LOADER_WARNINGS: AtomicU64 = AtomicU64::new(0);
static VALIDATION_FIRST: Mutex<Vec<ValidationId>> = Mutex::new(Vec::new());

fn state_code(state: u8) -> &'static str {
    match state {
        1 => "on",
        2 => "absent",
        3 => "off",
        _ => "none",
    }
}

/// The validation report of this process: counts survive the owner, so messages from its destruction are counted.
pub fn process_validation() -> ValidationReport {
    let first = match VALIDATION_FIRST.lock() {
        Ok(v) => v.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    ValidationReport {
        state: state_code(VALIDATION_STATE.load(Ordering::Acquire)),
        errors: VALIDATION_ERRORS.load(Ordering::Acquire),
        warnings: VALIDATION_WARNINGS.load(Ordering::Acquire),
        loader_warnings: LOADER_WARNINGS.load(Ordering::Acquire),
        first,
    }
}

/// The debug-utils messenger: counts errors and warnings and keeps the first ids. The message text goes to stderr for
/// the first ids only (diagnostics), never into a report or receipt. Never panics: it runs inside the driver's call.
unsafe extern "system" fn on_validation_message(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut std::ffi::c_void,
) -> vk::Bool32 {
    let general_only =
        !types.intersects(vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE);
    if general_only && !severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        // Loader notes such as "layer forced disabled" for the implicit layers this process turns off.
        LOADER_WARNINGS.fetch_add(1, Ordering::AcqRel);
        return vk::FALSE;
    }
    let (counter, label) = if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        (&VALIDATION_ERRORS, "error")
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        (&VALIDATION_WARNINGS, "warning")
    } else {
        return vk::FALSE;
    };
    counter.fetch_add(1, Ordering::AcqRel);
    // SAFETY: the layer passes a valid callback data struct, or null, for the duration of this call
    // (VUID-PFN_vkDebugUtilsMessengerCallbackEXT); its strings are NUL-terminated or null.
    let (name, number, text) = match unsafe { data.as_ref() } {
        Some(d) => unsafe {
            (
                d.message_id_name_as_c_str().map(|s| s.to_string_lossy().chars().take(160).collect()).unwrap_or_default(),
                d.message_id_number,
                d.message_as_c_str().map(|s| s.to_string_lossy().chars().take(2048).collect::<String>()).unwrap_or_default(),
            )
        },
        None => (String::new(), 0, String::new()),
    };
    let mut first = match VALIDATION_FIRST.lock() {
        Ok(v) => v,
        Err(poisoned) => poisoned.into_inner(),
    };
    if first.len() < FIRST_IDS {
        let _ = writeln!(std::io::stderr(), "vk-validation {label} {name} {number:#010x}: {text}");
        first.push(ValidationId { severity: label, name, number });
    }
    vk::FALSE
}

fn messenger_info() -> vk::DebugUtilsMessengerCreateInfoEXT<'static> {
    vk::DebugUtilsMessengerCreateInfoEXT::default()
        .message_severity(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR)
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(on_validation_message))
}

/// The instance's messenger, destroyed before the instance.
pub(crate) type Messenger = (ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT);

/// Raw handles a later owner hand-over passes on (Vulkan handles are the C ABI, so the binding does not matter).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceLease {
    pub issuer: Issuer,
    /// Address of `vkGetInstanceProcAddr`.
    pub get_instance_proc_addr: usize,
    pub instance: u64,
    pub physical_device: u64,
    pub device: u64,
    pub queue_family: u32,
    pub api_version: u32,
    pub extensions: Vec<&'static str>,
    pub generation: u32,
}

/// A pane: an owner-allocated image a client draws into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneId(u32);

impl PaneId {
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// Local trait shaped like render-gpu's `GpuClientPort` (red-team decision: the transient adapter of STU-ARC-024 in
/// this crate, behind a local port, evidence asset-local). Clients never see a queue, a swapchain or an allocator.
pub trait GpuClientPort {
    fn issuer(&self) -> Issuer;
    fn lease(&self) -> DeviceLease;
    fn create_pane(&mut self, width: u32, height: u32) -> Result<PaneId, Refusal>;
    fn release_pane(&mut self, pane: PaneId) -> Result<(), Refusal>;
    fn pane_extent(&self, pane: PaneId) -> Result<(u32, u32), Refusal>;
    /// Fill the pane with one colour (`vkCmdClearColorImage`; no pipeline).
    fn clear_pane(&mut self, pane: PaneId, rgba: [u8; 4]) -> Result<(), Refusal>;
    /// Copy a CPU-painted 8-bit RGBA buffer into the pane (`vkCmdCopyBufferToImage`).
    fn upload_pane(&mut self, pane: PaneId, rgba8: &[u8]) -> Result<(), Refusal>;
    /// Read the pane back as 8-bit RGBA, rows top to bottom.
    fn read_pane(&mut self, pane: PaneId) -> Result<Vec<u8>, Refusal>;
}

pub(crate) fn vk_refusal(result: vk::Result) -> Refusal {
    match result {
        vk::Result::ERROR_DEVICE_LOST => Refusal::DeviceLost,
        vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => Refusal::OutOfMemory,
        vk::Result::TIMEOUT => Refusal::FenceTimeout,
        vk::Result::ERROR_EXTENSION_NOT_PRESENT => Refusal::ExtensionMissing,
        vk::Result::ERROR_INCOMPATIBLE_DRIVER | vk::Result::ERROR_INITIALIZATION_FAILED => Refusal::VulkanUnavailable,
        other => Refusal::Vk(other.as_raw()),
    }
}

/// Instance and device, shared by the owner and its presenters; destroyed when the last user drops.
pub(crate) struct Core {
    pub(crate) entry: ash::Entry,
    pub(crate) instance: ash::Instance,
    pub(crate) physical: vk::PhysicalDevice,
    pub(crate) device: ash::Device,
    pub(crate) queue: vk::Queue,
    pub(crate) family: u32,
    pub(crate) memory: vk::PhysicalDeviceMemoryProperties,
    pub(crate) surface: bool,
    /// The validation messenger (item V), when the layer is on.
    pub(crate) messenger: Option<Messenger>,
}

impl Drop for Core {
    fn drop(&mut self) {
        // SAFETY: the last Arc holder; every child object (panes, buffers, pools, fences, presenters) was destroyed by
        // its owner first, and the device is idle (each owner waits idle before destroying its children). The
        // messenger is an instance child: destroyed after the device, before the instance
        // (VUID-vkDestroyInstance-instance-00629). Messages from instance destruction reach the messenger chained
        // into the instance create info.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            if let Some((utils, m)) = self.messenger.take() {
                utils.destroy_debug_utils_messenger(m, None);
            }
            self.instance.destroy_instance(None);
        }
        OWNER_LIVE.store(false, Ordering::Release);
    }
}

impl Core {
    pub(crate) fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Option<u32> {
        (0..self.memory.memory_type_count.min(vk::MAX_MEMORY_TYPES as u32)).find(|&i| {
            let bit = 1u32.checked_shl(i).unwrap_or(0);
            bits & bit != 0 && self.memory.memory_types[i as usize].property_flags.contains(flags)
        })
    }
}

/// A host-visible, coherent buffer kept mapped.
pub(crate) struct HostBuffer {
    pub(crate) buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    ptr: *mut u8,
    pub(crate) len: usize,
}

impl HostBuffer {
    pub(crate) fn new(core: &Core, len: usize, usage: vk::BufferUsageFlags) -> Result<Self, Refusal> {
        let size = u64::try_from(len).map_err(|_| Refusal::InvalidBuffer)?;
        let info = vk::BufferCreateInfo::default().size(size).usage(usage).sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: valid device, create info fully initialised; every failure path destroys what it created.
        unsafe {
            let buffer = core.device.create_buffer(&info, None).map_err(vk_refusal)?;
            let req = core.device.get_buffer_memory_requirements(buffer);
            let Some(kind) = core.memory_type(
                req.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            ) else {
                core.device.destroy_buffer(buffer, None);
                return Err(Refusal::OutOfMemory);
            };
            let alloc = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(kind);
            let memory = match core.device.allocate_memory(&alloc, None) {
                Ok(m) => m,
                Err(e) => {
                    core.device.destroy_buffer(buffer, None);
                    return Err(vk_refusal(e));
                }
            };
            let mapped = core
                .device
                .bind_buffer_memory(buffer, memory, 0)
                .and_then(|()| core.device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty()));
            match mapped {
                Ok(ptr) => Ok(Self { buffer, memory, ptr: ptr.cast::<u8>(), len }),
                Err(e) => {
                    core.device.destroy_buffer(buffer, None);
                    core.device.free_memory(memory, None);
                    Err(vk_refusal(e))
                }
            }
        }
    }
    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        if bytes.len() != self.len {
            return Err(Refusal::InvalidBuffer);
        }
        // SAFETY: `ptr` maps `len` bytes of host-coherent memory owned by this buffer; no GPU work uses it now (the
        // caller writes before recording the submit that reads it).
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr, self.len) };
        Ok(())
    }
    pub(crate) fn read(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.len];
        // SAFETY: as in `write`; the caller read only after the submit's fence signalled and a host-read barrier.
        unsafe { std::ptr::copy_nonoverlapping(self.ptr, out.as_mut_ptr(), self.len) };
        out
    }
    pub(crate) fn destroy(&mut self, core: &Core) {
        // SAFETY: the device is idle for this buffer (the caller waited on the fence of the last submit using it).
        unsafe {
            core.device.unmap_memory(self.memory);
            core.device.destroy_buffer(self.buffer, None);
            core.device.free_memory(self.memory, None);
        }
        self.buffer = vk::Buffer::null();
        self.len = 0;
    }
}

/// One command buffer and fence, submitted and waited with a bound.
pub(crate) struct Submitter {
    pool: vk::CommandPool,
    pub(crate) cmd: vk::CommandBuffer,
    fence: vk::Fence,
    /// Set by the first failed submit or wait: the command buffer may still be pending and the fence in flight, so
    /// every later `begin` returns this refusal instead of resetting them.
    poisoned: std::cell::Cell<Option<Refusal>>,
}

impl Submitter {
    pub(crate) fn new(core: &Core) -> Result<Self, Refusal> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(core.family);
        // SAFETY: valid device and family; partial creations are destroyed on failure.
        unsafe {
            let pool = core.device.create_command_pool(&pool_info, None).map_err(vk_refusal)?;
            let alloc = vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd = match core.device.allocate_command_buffers(&alloc) {
                Ok(v) if !v.is_empty() => v[0],
                Ok(_) => {
                    core.device.destroy_command_pool(pool, None);
                    return Err(Refusal::OutOfMemory);
                }
                Err(e) => {
                    core.device.destroy_command_pool(pool, None);
                    return Err(vk_refusal(e));
                }
            };
            let fence = match core.device.create_fence(&vk::FenceCreateInfo::default(), None) {
                Ok(f) => f,
                Err(e) => {
                    core.device.destroy_command_pool(pool, None);
                    return Err(vk_refusal(e));
                }
            };
            Ok(Self { pool, cmd, fence, poisoned: std::cell::Cell::new(None) })
        }
    }
    /// Begins the command buffer (reset first).
    pub(crate) fn begin(&self, core: &Core) -> Result<(), Refusal> {
        if let Some(r) = self.poisoned.get() {
            return Err(r);
        }
        let info = vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the previous submit of this buffer completed (every submit waits on the fence before returning).
        unsafe {
            core.device.reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty()).map_err(vk_refusal)?;
            core.device.begin_command_buffer(self.cmd, &info).map_err(vk_refusal)
        }
    }
    /// Ends, submits and waits (bounded) for the fence.
    pub(crate) fn submit_wait(
        &self,
        core: &Core,
        waits: &[(vk::Semaphore, vk::PipelineStageFlags)],
        signals: &[vk::Semaphore],
    ) -> Result<(), Refusal> {
        let result = self.submit_wait_inner(core, waits, signals);
        if let Err(r) = result {
            self.poisoned.set(Some(r));
        }
        result
    }
    /// Whether a failed submit or wait poisoned this submitter.
    pub(crate) fn poisoned(&self) -> Option<Refusal> {
        self.poisoned.get()
    }
    fn submit_wait_inner(
        &self,
        core: &Core,
        waits: &[(vk::Semaphore, vk::PipelineStageFlags)],
        signals: &[vk::Semaphore],
    ) -> Result<(), Refusal> {
        let cmds = [self.cmd];
        let wait_sems: Vec<vk::Semaphore> = waits.iter().map(|w| w.0).collect();
        let wait_stages: Vec<vk::PipelineStageFlags> = waits.iter().map(|w| w.1).collect();
        let submit = vk::SubmitInfo::default()
            .command_buffers(&cmds)
            .wait_semaphores(&wait_sems)
            .wait_dst_stage_mask(&wait_stages)
            .signal_semaphores(signals);
        // SAFETY: the buffer is in the recording state from `begin`; the queue is used from this thread only; the
        // fence is unsignalled (reset after every wait).
        unsafe {
            core.device.end_command_buffer(self.cmd).map_err(vk_refusal)?;
            core.device.queue_submit(core.queue, &[submit], self.fence).map_err(vk_refusal)?;
            let waited = core.device.wait_for_fences(&[self.fence], true, FENCE_TIMEOUT_NS);
            match waited {
                Ok(()) => core.device.reset_fences(&[self.fence]).map_err(vk_refusal),
                Err(vk::Result::TIMEOUT) => Err(Refusal::FenceTimeout),
                Err(e) => Err(vk_refusal(e)),
            }
        }
    }
    pub(crate) fn destroy(&mut self, core: &Core) {
        // SAFETY: the device is idle for this pool (every submit was waited on).
        unsafe {
            core.device.destroy_fence(self.fence, None);
            core.device.destroy_command_pool(self.pool, None);
        }
        self.fence = vk::Fence::null();
        self.pool = vk::CommandPool::null();
    }
}

pub(crate) fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1)
}

pub(crate) fn color_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .mip_level(0)
        .base_array_layer(0)
        .layer_count(1)
}

/// Records one image layout barrier.
#[allow(clippy::too_many_arguments)]
pub(crate) fn image_barrier(
    core: &Core,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src_access: vk::AccessFlags,
    dst_access: vk::AccessFlags,
    src_stage: vk::PipelineStageFlags,
    dst_stage: vk::PipelineStageFlags,
) {
    let barrier = vk::ImageMemoryBarrier::default()
        .old_layout(old)
        .new_layout(new)
        .src_access_mask(src_access)
        .dst_access_mask(dst_access)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_range());
    // SAFETY: `cmd` is recording; `image` is a live colour image with one mip and one layer.
    unsafe {
        core.device.cmd_pipeline_barrier(cmd, src_stage, dst_stage, vk::DependencyFlags::empty(), &[], &[], &[barrier]);
    }
}

/// Records the host-read barrier after a copy into a host buffer.
pub(crate) fn host_read_barrier(core: &Core, cmd: vk::CommandBuffer, buffer: vk::Buffer) {
    let barrier = vk::BufferMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .dst_access_mask(vk::AccessFlags::HOST_READ)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .buffer(buffer)
        .offset(0)
        .size(vk::WHOLE_SIZE);
    // SAFETY: `cmd` is recording; `buffer` is live.
    unsafe {
        core.device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::HOST,
            vk::DependencyFlags::empty(),
            &[],
            &[barrier],
            &[],
        );
    }
}

pub(crate) struct Pane {
    pub(crate) image: vk::Image,
    memory: vk::DeviceMemory,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Layout after the last owner submit: `UNDEFINED` before the first write, `TRANSFER_SRC_OPTIMAL` after.
    pub(crate) layout: vk::ImageLayout,
}

/// The pane image format: 8-bit sRGB-encoded bytes stored as UNORM, so nothing encodes twice.
pub const PANE_FORMAT: &str = "R8G8B8A8_UNORM";

/// The transient owner: one instance, one device, one graphics queue per process.
pub struct Owner {
    core: Arc<Core>,
    submit: Submitter,
    panes: Vec<Option<Pane>>,
    info: DeviceInfo,
    layers: LayerReport,
    generation: u32,
    extensions: Vec<&'static str>,
    validation: &'static str,
}

fn c_text(raw: Result<&CStr, std::ffi::FromBytesUntilNulError>) -> String {
    raw.map(|s| s.to_string_lossy().chars().take(256).collect()).unwrap_or_default()
}

fn device_type(kind: vk::PhysicalDeviceType) -> &'static str {
    match kind {
        vk::PhysicalDeviceType::DISCRETE_GPU => "discrete_gpu",
        vk::PhysicalDeviceType::INTEGRATED_GPU => "integrated_gpu",
        vk::PhysicalDeviceType::VIRTUAL_GPU => "virtual_gpu",
        vk::PhysicalDeviceType::CPU => "cpu",
        _ => "other",
    }
}

fn type_rank(kind: vk::PhysicalDeviceType) -> u8 {
    match kind {
        vk::PhysicalDeviceType::DISCRETE_GPU => 0,
        vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
        vk::PhysicalDeviceType::VIRTUAL_GPU => 2,
        vk::PhysicalDeviceType::CPU => 3,
        _ => 4,
    }
}

impl Owner {
    /// Opens the process's one device. Refusals: `owner_already_open`, `vulkan_unavailable`, `api_below_baseline`,
    /// `extension_missing`, `no_suitable_device`, `cpu_device_refused`.
    pub fn open(config: OwnerConfig) -> Result<Self, Refusal> {
        if OWNER_LIVE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Err(Refusal::OwnerAlreadyOpen);
        }
        match Self::open_inner(config) {
            Ok(owner) => Ok(owner),
            Err(e) => {
                // A built Core resets the flag in its Drop; open_inner never returns Err after building one.
                OWNER_LIVE.store(false, Ordering::Release);
                Err(e)
            }
        }
    }

    fn open_inner(config: OwnerConfig) -> Result<Self, Refusal> {
        let disabled = if config.allow_overlays {
            "none"
        } else {
            // SAFETY (edition 2024 `set_var`): called before the loader is loaded, from the thread that opens the
            // process's only owner; the host sets it before it starts worker threads. The loader reads it at instance
            // creation. Only this process's environment changes.
            unsafe { std::env::set_var("VK_LOADER_LAYERS_DISABLE", "~implicit~") };
            "~implicit~"
        };
        // SAFETY: loads vulkan-1.dll through libloading (ash feature `loaded`); a missing loader is an Err, not a crash.
        let entry = unsafe { ash::Entry::load() }.map_err(|_| Refusal::VulkanUnavailable)?;
        // SAFETY: valid entry; these enumerate calls have no preconditions.
        let (version, layer_props, ext_props) = unsafe {
            (
                entry.try_enumerate_instance_version().map_err(vk_refusal)?.unwrap_or(vk::API_VERSION_1_0),
                entry.enumerate_instance_layer_properties().unwrap_or_default(),
                entry.enumerate_instance_extension_properties(None).map_err(vk_refusal)?,
            )
        };
        if version < vk::API_VERSION_1_3 {
            return Err(Refusal::ApiBelowBaseline);
        }
        let present: Vec<String> = layer_props.iter().take(64).map(|l| c_text(l.layer_name_as_c_str())).collect();
        let has_ext = |name: &CStr| ext_props.iter().any(|e| e.extension_name_as_c_str() == Ok(name));
        let mut instance_exts: Vec<*const std::ffi::c_char> = Vec::new();
        if config.surface {
            for name in [ash::khr::surface::NAME, ash::khr::win32_surface::NAME] {
                if !has_ext(name) {
                    return Err(Refusal::ExtensionMissing);
                }
                instance_exts.push(name.as_ptr());
            }
        }
        // Item V: debug builds enable the Khronos validation layer when the loader lists it, with VK_EXT_debug_utils
        // (from the loader or the layer itself) and a counting messenger; release builds never do. The checked-in
        // code decides, not the environment (CX-VAL-007). VK_LOADER_LAYERS_DISABLE=~implicit~ leaves explicit layers.
        const LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";
        let listed = present.iter().any(|l| l == VALIDATION_LAYER);
        let utils = ash::ext::debug_utils::NAME;
        let validate = cfg!(debug_assertions)
            && listed
            && (has_ext(utils)
                // SAFETY: valid entry; the layer name is a static NUL-terminated string.
                || unsafe { entry.enumerate_instance_extension_properties(Some(LAYER)) }
                    .unwrap_or_default()
                    .iter()
                    .any(|e| e.extension_name_as_c_str() == Ok(utils)));
        let state: u8 = if !cfg!(debug_assertions) {
            3
        } else if validate {
            1
        } else {
            2
        };
        let layers: Vec<*const std::ffi::c_char> = if validate { vec![LAYER.as_ptr()] } else { Vec::new() };
        if validate {
            instance_exts.push(utils.as_ptr());
        }
        let app = vk::ApplicationInfo::default()
            .application_name(c"hsk-studio asset-local host")
            .engine_name(c"hsk-studio-vulkan")
            .api_version(vk::API_VERSION_1_3);
        let mut chained = messenger_info();
        let mut create = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_layer_names(&layers)
            .enabled_extension_names(&instance_exts);
        if validate {
            // Instance creation and destruction messages (VK_EXT_debug_utils: a messenger chained to the create info).
            create = create.push_next(&mut chained);
        }
        VALIDATION_STATE.store(state, Ordering::Release);
        // SAFETY: create info, layer and extension name pointers outlive the call (static CStr names).
        let instance = unsafe { entry.create_instance(&create, None) }.map_err(vk_refusal)?;
        let mut messenger: Option<Messenger> = None;
        if validate {
            let utils = ash::ext::debug_utils::Instance::new(&entry, &instance);
            // SAFETY: valid instance with VK_EXT_debug_utils enabled; the callback is a plain fn with no user data.
            match unsafe { utils.create_debug_utils_messenger(&messenger_info(), None) } {
                Ok(m) => messenger = Some((utils, m)),
                Err(e) => {
                    // SAFETY: no child object of this instance exists on this path.
                    unsafe { instance.destroy_instance(None) };
                    return Err(vk_refusal(e));
                }
            }
        }
        let enabled = if validate { vec![VALIDATION_LAYER.to_owned()] } else { Vec::new() };
        let report = LayerReport { disabled, present, enabled };
        match Self::open_device(config, entry, instance, report, &mut messenger, state_code(state)) {
            Ok(owner) => Ok(owner),
            Err((e, instance)) => {
                if let Some(instance) = instance {
                    // SAFETY: the messenger is this instance's only child on this path; destroyed first.
                    unsafe {
                        if let Some((utils, m)) = messenger.take() {
                            utils.destroy_debug_utils_messenger(m, None);
                        }
                        instance.destroy_instance(None);
                    }
                }
                Err(e)
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn open_device(
        config: OwnerConfig,
        entry: ash::Entry,
        instance: ash::Instance,
        layers: LayerReport,
        messenger: &mut Option<Messenger>,
        validation: &'static str,
    ) -> Result<Self, (Refusal, Option<ash::Instance>)> {
        let win32 = config.surface.then(|| ash::khr::win32_surface::Instance::new(&entry, &instance));
        // SAFETY: valid instance; read-only queries.
        let physicals = match unsafe { instance.enumerate_physical_devices() } {
            Ok(v) => v,
            Err(e) => return Err((vk_refusal(e), Some(instance))),
        };
        let mut best: Option<(u8, vk::PhysicalDevice, u32, vk::PhysicalDeviceProperties)> = None;
        let mut saw_cpu_only = false;
        let mut saw_old = false;
        for &pd in physicals.iter().take(64) {
            // SAFETY: `pd` came from this instance.
            let (props, families, dev_exts) = unsafe {
                (
                    instance.get_physical_device_properties(pd),
                    instance.get_physical_device_queue_family_properties(pd),
                    instance.enumerate_device_extension_properties(pd).unwrap_or_default(),
                )
            };
            if props.api_version < vk::API_VERSION_1_3 {
                saw_old = true;
                continue;
            }
            if props.device_type == vk::PhysicalDeviceType::CPU && !config.allow_cpu_device {
                saw_cpu_only = true;
                continue;
            }
            if config.surface && !dev_exts.iter().any(|e| e.extension_name_as_c_str() == Ok(ash::khr::swapchain::NAME)) {
                continue;
            }
            let family = families.iter().enumerate().find_map(|(i, f)| {
                let i = u32::try_from(i).ok()?;
                let graphics = f.queue_flags.contains(vk::QueueFlags::GRAPHICS);
                // SAFETY: `pd` and `i` are valid for this instance.
                let presents = win32.as_ref().is_none_or(|w| unsafe { w.get_physical_device_win32_presentation_support(pd, i) });
                (graphics && presents).then_some(i)
            });
            let Some(family) = family else { continue };
            let rank = type_rank(props.device_type);
            if best.as_ref().is_none_or(|b| rank < b.0) {
                best = Some((rank, pd, family, props));
            }
        }
        let Some((_, physical, family, props)) = best else {
            let e = if saw_cpu_only {
                Refusal::CpuDeviceRefused
            } else if saw_old {
                Refusal::ApiBelowBaseline
            } else {
                Refusal::NoSuitableDevice
            };
            return Err((e, Some(instance)));
        };
        let mut id_props = vk::PhysicalDeviceIDProperties::default();
        let mut driver_props = vk::PhysicalDeviceDriverProperties::default();
        let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut id_props).push_next(&mut driver_props);
        // SAFETY: Vulkan 1.1+ core query on a 1.3 device; the chain structs live for the call.
        unsafe { instance.get_physical_device_properties2(physical, &mut props2) };
        let device_uuid: String = id_props.device_uuid.iter().map(|b| format!("{b:02x}")).collect();
        let info = DeviceInfo {
            name: c_text(props.device_name_as_c_str()),
            vendor_id: props.vendor_id,
            device_id: props.device_id,
            device_type: device_type(props.device_type),
            api_version: [
                vk::api_version_major(props.api_version),
                vk::api_version_minor(props.api_version),
                vk::api_version_patch(props.api_version),
            ],
            driver_version: props.driver_version,
            driver_name: c_text(driver_props.driver_name_as_c_str()),
            driver_info: c_text(driver_props.driver_info_as_c_str()),
            device_uuid,
            queue_family: family,
            features: Vec::new(),
        };
        let priorities = [1.0f32];
        let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&priorities)];
        let mut device_exts: Vec<*const std::ffi::c_char> = Vec::new();
        let mut extensions: Vec<&'static str> = Vec::new();
        if config.surface {
            device_exts.push(ash::khr::swapchain::NAME.as_ptr());
            extensions.extend(["VK_KHR_surface", "VK_KHR_win32_surface", "VK_KHR_swapchain"]);
        }
        let device_info = vk::DeviceCreateInfo::default().queue_create_infos(&queues).enabled_extension_names(&device_exts);
        // SAFETY: valid physical device and family; extension names are static.
        let device = match unsafe { instance.create_device(physical, &device_info, None) } {
            Ok(d) => d,
            Err(e) => return Err((vk_refusal(e), Some(instance))),
        };
        // SAFETY: family `family`, index 0 was requested above.
        let queue = unsafe { device.get_device_queue(family, 0) };
        // SAFETY: valid physical device.
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        let core = Arc::new(Core {
            entry,
            instance,
            physical,
            device,
            queue,
            family,
            memory,
            surface: config.surface,
            messenger: messenger.take(),
        });
        // From here a failure drops `core`, whose Drop destroys device, messenger and instance and resets the owner
        // flag.
        let submit = Submitter::new(&core).map_err(|e| (e, None))?;
        Ok(Self { core, submit, panes: Vec::new(), info, layers, generation: 1, extensions, validation })
    }

    pub fn device_info(&self) -> &DeviceInfo {
        &self.info
    }
    pub fn layers(&self) -> &LayerReport {
        &self.layers
    }
    /// Validation layer state: `on` (debug build, the loader lists VK_LAYER_KHRONOS_validation and it is enabled with
    /// a counting messenger), `absent` (debug build, not listed) or `off` (release build).
    pub fn validation(&self) -> &'static str {
        self.validation
    }
    /// Error and warning counts and the first message ids of this process ([`process_validation`]).
    pub fn validation_report(&self) -> ValidationReport {
        process_validation()
    }
    /// The refusal that stopped this owner's submits (a fence timeout or device loss leaves its command buffer
    /// possibly pending; every later pane operation returns this refusal).
    pub fn poisoned(&self) -> Option<Refusal> {
        self.submit.poisoned()
    }
    pub(crate) fn core(&self) -> &Arc<Core> {
        &self.core
    }
    pub(crate) fn pane(&self, pane: PaneId) -> Result<&Pane, Refusal> {
        self.panes.get(pane.0 as usize).and_then(Option::as_ref).ok_or(Refusal::InvalidPane)
    }

    fn write_pane(&mut self, pane: PaneId, source: PaneWrite<'_>) -> Result<(), Refusal> {
        let (image, width, height, layout) = {
            let p = self.pane(pane)?;
            (p.image, p.width, p.height, p.layout)
        };
        let mut staging = match source {
            PaneWrite::Pixels(bytes) => {
                let len = rgba_len(width, height)?;
                if bytes.len() != len {
                    return Err(Refusal::InvalidBuffer);
                }
                let mut buf = HostBuffer::new(&self.core, len, vk::BufferUsageFlags::TRANSFER_SRC)?;
                buf.write(bytes)?;
                Some(buf)
            }
            PaneWrite::Clear(_) => None,
        };
        let core = &self.core;
        let result = (|| {
            self.submit.begin(core)?;
            let cmd = self.submit.cmd;
            image_barrier(
                core,
                cmd,
                image,
                layout,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
            );
            match (&source, &staging) {
                (PaneWrite::Clear(rgba), _) => {
                    let value = vk::ClearColorValue { float32: rgba.map(|c| f32::from(c) / 255.0) };
                    // SAFETY: `cmd` is recording; the image is in TRANSFER_DST_OPTIMAL after the barrier.
                    unsafe {
                        core.device.cmd_clear_color_image(
                            cmd,
                            image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &value,
                            &[color_range()],
                        )
                    };
                }
                (PaneWrite::Pixels(_), Some(buf)) => {
                    let region = vk::BufferImageCopy::default()
                        .buffer_offset(0)
                        .buffer_row_length(0)
                        .buffer_image_height(0)
                        .image_subresource(color_layers())
                        .image_extent(vk::Extent3D { width, height, depth: 1 });
                    // SAFETY: the buffer holds exactly width*height*4 bytes; the image is TRANSFER_DST_OPTIMAL.
                    unsafe {
                        core.device.cmd_copy_buffer_to_image(
                            cmd,
                            buf.buffer,
                            image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[region],
                        )
                    };
                }
                (PaneWrite::Pixels(_), None) => return Err(Refusal::InvalidBuffer),
            }
            image_barrier(
                core,
                cmd,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::TRANSFER_READ,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
            );
            self.submit.submit_wait(core, &[], &[])
        })();
        if let Some(buf) = staging.as_mut() {
            buf.destroy(core);
        }
        result?;
        if let Some(Some(p)) = self.panes.get_mut(pane.0 as usize) {
            p.layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum PaneWrite<'a> {
    Clear([u8; 4]),
    Pixels(&'a [u8]),
}

impl GpuClientPort for Owner {
    fn issuer(&self) -> Issuer {
        Issuer::TransientAssetLocal
    }

    fn lease(&self) -> DeviceLease {
        DeviceLease {
            issuer: Issuer::TransientAssetLocal,
            get_instance_proc_addr: self.core.entry.static_fn().get_instance_proc_addr as usize,
            instance: self.core.instance.handle().as_raw(),
            physical_device: self.core.physical.as_raw(),
            device: self.core.device.handle().as_raw(),
            queue_family: self.core.family,
            api_version: vk::API_VERSION_1_3,
            extensions: self.extensions.clone(),
            generation: self.generation,
        }
    }

    fn create_pane(&mut self, width: u32, height: u32) -> Result<PaneId, Refusal> {
        rgba_len(width, height)?;
        if self.panes.iter().filter(|p| p.is_some()).count() >= 64 {
            return Err(Refusal::OutOfMemory);
        }
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let core = &self.core;
        // SAFETY: valid device; R8G8B8A8_UNORM optimal tiling with these usages is mandatory format support; failure
        // paths destroy what they created.
        let (image, memory) = unsafe {
            let image = core.device.create_image(&info, None).map_err(vk_refusal)?;
            let req = core.device.get_image_memory_requirements(image);
            let Some(kind) = core.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL) else {
                core.device.destroy_image(image, None);
                return Err(Refusal::OutOfMemory);
            };
            let alloc = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(kind);
            let memory = match core.device.allocate_memory(&alloc, None) {
                Ok(m) => m,
                Err(e) => {
                    core.device.destroy_image(image, None);
                    return Err(vk_refusal(e));
                }
            };
            if let Err(e) = core.device.bind_image_memory(image, memory, 0) {
                core.device.destroy_image(image, None);
                core.device.free_memory(memory, None);
                return Err(vk_refusal(e));
            }
            (image, memory)
        };
        let pane = Pane { image, memory, width, height, layout: vk::ImageLayout::UNDEFINED };
        let slot = self.panes.iter().position(Option::is_none);
        let index = match slot {
            Some(i) => {
                self.panes[i] = Some(pane);
                i
            }
            None => {
                self.panes.push(Some(pane));
                self.panes.len() - 1
            }
        };
        Ok(PaneId(u32::try_from(index).map_err(|_| Refusal::OutOfMemory)?))
    }

    fn release_pane(&mut self, pane: PaneId) -> Result<(), Refusal> {
        let p = self.panes.get_mut(pane.0 as usize).and_then(Option::take).ok_or(Refusal::InvalidPane)?;
        // SAFETY: every submit that used the pane was waited on; a presenter blits only inside its own waited submit.
        unsafe {
            let _ = self.core.device.device_wait_idle();
            self.core.device.destroy_image(p.image, None);
            self.core.device.free_memory(p.memory, None);
        }
        Ok(())
    }

    fn pane_extent(&self, pane: PaneId) -> Result<(u32, u32), Refusal> {
        self.pane(pane).map(|p| (p.width, p.height))
    }

    fn clear_pane(&mut self, pane: PaneId, rgba: [u8; 4]) -> Result<(), Refusal> {
        self.write_pane(pane, PaneWrite::Clear(rgba))
    }

    fn upload_pane(&mut self, pane: PaneId, rgba8: &[u8]) -> Result<(), Refusal> {
        self.write_pane(pane, PaneWrite::Pixels(rgba8))
    }

    fn read_pane(&mut self, pane: PaneId) -> Result<Vec<u8>, Refusal> {
        let (image, width, height, layout) = {
            let p = self.pane(pane)?;
            (p.image, p.width, p.height, p.layout)
        };
        if layout != vk::ImageLayout::TRANSFER_SRC_OPTIMAL {
            // Never written: reading undefined contents is refused rather than returned.
            return Err(Refusal::InvalidPane);
        }
        let len = rgba_len(width, height)?;
        let core = &self.core;
        let mut buf = HostBuffer::new(core, len, vk::BufferUsageFlags::TRANSFER_DST)?;
        let result = (|| {
            self.submit.begin(core)?;
            let cmd = self.submit.cmd;
            let region = vk::BufferImageCopy::default()
                .image_subresource(color_layers())
                .image_extent(vk::Extent3D { width, height, depth: 1 });
            // SAFETY: the image is TRANSFER_SRC_OPTIMAL (owner invariant after a write); the buffer holds len bytes.
            unsafe {
                core.device.cmd_copy_image_to_buffer(cmd, image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, buf.buffer, &[region])
            };
            host_read_barrier(core, cmd, buf.buffer);
            self.submit.submit_wait(core, &[], &[])?;
            Ok(buf.read())
        })();
        buf.destroy(core);
        result
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let core = Arc::clone(&self.core);
        // SAFETY: wait idle, then destroy every pane and the submitter; the Core (device, instance) drops after the
        // last presenter.
        unsafe {
            let _ = core.device.device_wait_idle();
            for p in self.panes.drain(..).flatten() {
                core.device.destroy_image(p.image, None);
                core.device.free_memory(p.memory, None);
            }
        }
        self.submit.destroy(&core);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Item V positive control (GPU group; its own process): with the layer on, a known invalid call is counted as an
    /// error under its VUID, so "on" with zero errors elsewhere is a live messenger, not a silent one. The call is
    /// `vkResetFences` with fenceCount 0 (VUID-vkResetFences-fenceCount-arraylength), which the driver ignores.
    #[test]
    fn validation_messenger_counts_a_known_error() {
        let owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device on this machine");
        if owner.validation() != "on" {
            println!("NOT_PROVEN: validation {} (layer not enabled), positive control skipped", owner.validation());
            return;
        }
        let before = process_validation();
        // SAFETY: an empty fence list is invalid usage the layer reports; no fence is touched, the driver gets count 0.
        let _ = unsafe { owner.core.device.reset_fences(&[]) };
        let after = process_validation();
        println!("validation before {before:?} after {after:?}");
        assert_eq!(after.errors, before.errors + 1, "{after:?}");
        assert!(after.first.iter().any(|id| id.severity == "error" && id.name.contains("vkResetFences")), "{after:?}");
        assert!(after.loader_warnings >= before.loader_warnings);
    }
}
