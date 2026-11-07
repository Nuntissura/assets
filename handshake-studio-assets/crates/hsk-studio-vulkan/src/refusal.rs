//! Typed refusals (O8: no Vulkan means a typed refusal, never a fallback that calls itself Vulkan).

/// Why the Vulkan owner or presenter refused. `code()` is stable; nothing here carries driver text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// `vulkan-1.dll` could not be loaded, or the instance could not be created.
    VulkanUnavailable,
    /// The loader or every device is below the Vulkan 1.3 baseline.
    ApiBelowBaseline,
    /// No physical device with a graphics queue (and, for a window, presentation support).
    NoSuitableDevice,
    /// Only a CPU-type device was found and `allow_cpu_device` is off.
    CpuDeviceRefused,
    /// One device per process: an owner is already open.
    OwnerAlreadyOpen,
    /// A required instance or device extension is missing (surface, win32 surface, swapchain).
    ExtensionMissing,
    /// Host or device memory ran out, or no memory type fits.
    OutOfMemory,
    /// `VK_ERROR_DEVICE_LOST`.
    DeviceLost,
    /// A bounded fence wait ran out.
    FenceTimeout,
    /// The owner was opened without surface support, or the queue cannot present to this surface.
    SurfaceUnsupported,
    /// The surface came back lost more than `machine::MAX_SURFACE_ATTEMPTS` times.
    SurfaceUnrecoverable,
    /// No `B8G8R8A8_UNORM` surface format.
    SwapchainFormatUnsupported,
    /// The surface does not allow transfer-destination and transfer-source swapchain images.
    SwapchainUsageUnsupported,
    /// Extent outside 1..=`MAX_EXTENT` per side or above `MAX_PIXELS`.
    InvalidExtent,
    /// Unknown or released pane id.
    InvalidPane,
    /// A pixel buffer whose length is not `width * height * 4`.
    InvalidBuffer,
    /// The window handles are not a live Win32 window of this process.
    InvalidWindow,
    /// Any other Vulkan error (raw `VkResult` code, never text).
    Vk(i32),
}

impl Refusal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::VulkanUnavailable => "vulkan_unavailable",
            Self::ApiBelowBaseline => "api_below_baseline",
            Self::NoSuitableDevice => "no_suitable_device",
            Self::CpuDeviceRefused => "cpu_device_refused",
            Self::OwnerAlreadyOpen => "owner_already_open",
            Self::ExtensionMissing => "extension_missing",
            Self::OutOfMemory => "out_of_memory",
            Self::DeviceLost => "device_lost",
            Self::FenceTimeout => "fence_timeout",
            Self::SurfaceUnsupported => "surface_unsupported",
            Self::SurfaceUnrecoverable => "surface_unrecoverable",
            Self::SwapchainFormatUnsupported => "swapchain_format_unsupported",
            Self::SwapchainUsageUnsupported => "swapchain_usage_unsupported",
            Self::InvalidExtent => "invalid_extent",
            Self::InvalidPane => "invalid_pane",
            Self::InvalidBuffer => "invalid_buffer",
            Self::InvalidWindow => "invalid_window",
            Self::Vk(_) => "vk_error",
        }
    }
    /// The observe failure class: unavailable (no device or surface), loss (device or surface gone), unsupported
    /// (the platform lacks a capability), validation (bad input).
    pub const fn class(self) -> &'static str {
        match self {
            Self::VulkanUnavailable | Self::NoSuitableDevice | Self::OwnerAlreadyOpen | Self::OutOfMemory | Self::FenceTimeout => "unavailable",
            Self::DeviceLost | Self::SurfaceUnrecoverable => "loss",
            Self::ApiBelowBaseline
            | Self::CpuDeviceRefused
            | Self::ExtensionMissing
            | Self::SurfaceUnsupported
            | Self::SwapchainFormatUnsupported
            | Self::SwapchainUsageUnsupported => "unsupported",
            Self::InvalidExtent | Self::InvalidPane | Self::InvalidBuffer | Self::InvalidWindow => "validation",
            Self::Vk(_) => "unavailable",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Vk(code) => write!(f, "vk_error({code})"),
            other => f.write_str(other.code()),
        }
    }
}

impl std::error::Error for Refusal {}

/// Largest pane or swapchain side.
pub const MAX_EXTENT: u32 = 16384;
/// Largest pane in pixels (64 Mpx, harness-view's `MAX_VIEW_PIXELS`).
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// Byte length of a `width x height` RGBA8 buffer after the extent checks, or `InvalidExtent`.
pub fn rgba_len(width: u32, height: u32) -> Result<usize, Refusal> {
    if width == 0 || height == 0 || width > MAX_EXTENT || height > MAX_EXTENT {
        return Err(Refusal::InvalidExtent);
    }
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or(Refusal::InvalidExtent)?;
    if pixels > MAX_PIXELS {
        return Err(Refusal::InvalidExtent);
    }
    let bytes = pixels.checked_mul(4).ok_or(Refusal::InvalidExtent)?;
    usize::try_from(bytes).map_err(|_| Refusal::InvalidExtent)
}
