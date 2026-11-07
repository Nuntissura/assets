//! The presenter: a Win32 surface, its swapchain, and the copy of an owner pane into the swapchain image (no shader,
//! no pipeline, no descriptor). Every Vulkan result goes through [`crate::machine`].
use crate::machine::{Action, Machine, Signal, SwapState};
use crate::owner::{
    Core, HostBuffer, Owner, PaneId, Submitter, color_layers, color_range, host_read_barrier, image_barrier, vk_refusal,
};
use crate::refusal::{MAX_EXTENT, Refusal, rgba_len};
use ash::vk;
use std::sync::Arc;

/// Bound on `vkAcquireNextImageKHR`.
pub const ACQUIRE_TIMEOUT_NS: u64 = 2_000_000_000;

/// The swapchain image format: matches the pane's 8-bit bytes with no second sRGB encode.
pub const SWAPCHAIN_FORMAT: &str = "B8G8R8A8_UNORM";

/// Raw Win32 handles of a window this process owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowHandles {
    pub hinstance: isize,
    pub hwnd: isize,
}

/// What the window reported (the presenter's only external inputs besides Vulkan results).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowSignal {
    Resized,
    Minimized,
    Restored,
}

/// A captured swapchain image, converted to RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

/// One `frame` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    Presented { image_index: u32, capture: Option<Capture> },
    /// No frame this call; the code says why (`minimized`, `acquire_timeout`, `recreated`).
    Skipped(&'static str),
}

/// Counters for the receipt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentCounters {
    pub frames_presented: u64,
    pub swapchain_creates: u64,
    pub surface_recreates: u64,
    pub acquire_timeouts: u64,
    pub skipped_minimized: u64,
    pub suboptimal: u64,
    pub out_of_date: u64,
}

pub struct Presenter {
    core: Arc<Core>,
    surface_fn: ash::khr::surface::Instance,
    win32_fn: ash::khr::win32_surface::Instance,
    swap_fn: ash::khr::swapchain::Device,
    handles: WindowHandles,
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    present_sems: Vec<vk::Semaphore>,
    acquire_sem: vk::Semaphore,
    extent: (u32, u32),
    submit: Submitter,
    capture: Option<HostBuffer>,
    machine: Machine,
    counters: PresentCounters,
    background: [u8; 4],
}

impl Presenter {
    /// Creates the surface for `handles`. The swapchain is created on the first frame. `background` fills what the
    /// pane does not cover.
    pub fn new(owner: &Owner, handles: WindowHandles, background: [u8; 4]) -> Result<Self, Refusal> {
        let core = Arc::clone(owner.core());
        if !core.surface {
            return Err(Refusal::SurfaceUnsupported);
        }
        if handles.hwnd == 0 || handles.hinstance == 0 {
            return Err(Refusal::InvalidWindow);
        }
        let surface_fn = ash::khr::surface::Instance::new(&core.entry, &core.instance);
        let win32_fn = ash::khr::win32_surface::Instance::new(&core.entry, &core.instance);
        let swap_fn = ash::khr::swapchain::Device::new(&core.instance, &core.device);
        let surface = create_surface(&win32_fn, handles)?;
        // SAFETY: surface from this instance; physical device and family from the owner.
        let supported = unsafe { surface_fn.get_physical_device_surface_support(core.physical, core.family, surface) };
        if supported != Ok(true) {
            // SAFETY: no swapchain refers to the surface yet.
            unsafe { surface_fn.destroy_surface(surface, None) };
            return Err(Refusal::SurfaceUnsupported);
        }
        let submit = match Submitter::new(&core) {
            Ok(s) => s,
            Err(e) => {
                // SAFETY: as above.
                unsafe { surface_fn.destroy_surface(surface, None) };
                return Err(e);
            }
        };
        // SAFETY: valid device.
        let acquire_sem = match unsafe { core.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) } {
            Ok(s) => s,
            Err(e) => {
                let mut submit = submit;
                submit.destroy(&core);
                // SAFETY: as above.
                unsafe { surface_fn.destroy_surface(surface, None) };
                return Err(vk_refusal(e));
            }
        };
        Ok(Self {
            core,
            surface_fn,
            win32_fn,
            swap_fn,
            handles,
            surface,
            swapchain: vk::SwapchainKHR::null(),
            images: Vec::new(),
            present_sems: Vec::new(),
            acquire_sem,
            extent: (0, 0),
            submit,
            capture: None,
            machine: Machine::default(),
            counters: PresentCounters::default(),
            background,
        })
    }

    pub fn state(&self) -> SwapState {
        self.machine.state()
    }
    pub fn machine(&self) -> &Machine {
        &self.machine
    }
    pub fn counters(&self) -> PresentCounters {
        self.counters
    }
    pub fn extent(&self) -> (u32, u32) {
        self.extent
    }
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// Feeds a window event into the machine.
    pub fn window_signal(&mut self, signal: WindowSignal) -> Action {
        self.machine.feed(match signal {
            WindowSignal::Resized => Signal::Resized,
            WindowSignal::Minimized => Signal::ZeroExtent,
            WindowSignal::Restored => Signal::Restored,
        })
    }

    fn wait_idle(&self) {
        // SAFETY: valid device; a failed wait is reported by the next submit.
        unsafe {
            let _ = self.core.device.device_wait_idle();
        }
    }

    fn destroy_swapchain(&mut self) {
        // SAFETY: device idle (callers wait first); the images belong to the swapchain.
        unsafe {
            for s in self.present_sems.drain(..) {
                self.core.device.destroy_semaphore(s, None);
            }
            if self.swapchain != vk::SwapchainKHR::null() {
                self.swap_fn.destroy_swapchain(self.swapchain, None);
            }
        }
        self.swapchain = vk::SwapchainKHR::null();
        self.images.clear();
        if let Some(mut c) = self.capture.take() {
            c.destroy(&self.core);
        }
    }

    /// Re-creates the swapchain (oldSwapchain passed), after a device wait-idle (red-team stand-in).
    fn recreate(&mut self) -> Result<(), Refusal> {
        self.wait_idle();
        // SAFETY: surface and physical device are live.
        let caps = match unsafe {
            self.surface_fn.get_physical_device_surface_capabilities(self.core.physical, self.surface)
        } {
            Ok(c) => c,
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                self.machine.feed(Signal::SurfaceLost);
                return Ok(());
            }
            Err(e) => return Err(vk_refusal(e)),
        };
        let (w, h) = (caps.current_extent.width, caps.current_extent.height);
        if w == 0 || h == 0 || w == u32::MAX {
            // u32::MAX means "the swapchain decides", which a Win32 surface never reports; treat it as no size.
            self.machine.feed(Signal::ZeroExtent);
            return Ok(());
        }
        if w > MAX_EXTENT || h > MAX_EXTENT {
            return Err(Refusal::InvalidExtent);
        }
        let need = vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC;
        if !caps.supported_usage_flags.contains(need) {
            return Err(Refusal::SwapchainUsageUnsupported);
        }
        // SAFETY: read-only queries.
        let (formats, features) = unsafe {
            (
                self.surface_fn
                    .get_physical_device_surface_formats(self.core.physical, self.surface)
                    .map_err(vk_refusal)?,
                self.core.instance.get_physical_device_format_properties(self.core.physical, vk::Format::B8G8R8A8_UNORM),
            )
        };
        let format = formats
            .iter()
            .find(|f| f.format == vk::Format::B8G8R8A8_UNORM && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
            .copied()
            .ok_or(Refusal::SwapchainFormatUnsupported)?;
        if !features.optimal_tiling_features.contains(vk::FormatFeatureFlags::BLIT_DST) {
            return Err(Refusal::SwapchainFormatUnsupported);
        }
        let mut count = caps.min_image_count.saturating_add(1);
        if caps.max_image_count != 0 {
            count = count.min(caps.max_image_count);
        }
        let alpha = if caps.supported_composite_alpha.contains(vk::CompositeAlphaFlagsKHR::OPAQUE) {
            vk::CompositeAlphaFlagsKHR::OPAQUE
        } else {
            vk::CompositeAlphaFlagsKHR::INHERIT
        };
        let old = self.swapchain;
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(vk::Extent2D { width: w, height: h })
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | need)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(alpha)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(old);
        // SAFETY: device idle; `old` is retired by this call and destroyed right after.
        let created = unsafe { self.swap_fn.create_swapchain(&info, None) };
        let swapchain = match created {
            Ok(s) => s,
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                self.machine.feed(Signal::SurfaceLost);
                return Ok(());
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                // The size changed again between the query and the create: try on the next frame.
                return Ok(());
            }
            Err(e) => return Err(vk_refusal(e)),
        };
        self.destroy_swapchain();
        self.swapchain = swapchain;
        // SAFETY: swapchain just created.
        self.images = unsafe { self.swap_fn.get_swapchain_images(swapchain) }.map_err(vk_refusal)?;
        for _ in 0..self.images.len() {
            // SAFETY: valid device. Present semaphores are indexed by swapchain image, not by frame.
            let s = unsafe { self.core.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }
                .map_err(vk_refusal)?;
            self.present_sems.push(s);
        }
        self.capture = Some(HostBuffer::new(&self.core, rgba_len(w, h)?, vk::BufferUsageFlags::TRANSFER_DST)?);
        self.extent = (w, h);
        self.counters.swapchain_creates = self.counters.swapchain_creates.saturating_add(1);
        self.machine.feed(Signal::Created);
        Ok(())
    }

    fn recreate_surface(&mut self) -> Result<(), Refusal> {
        self.wait_idle();
        self.destroy_swapchain();
        // SAFETY: no swapchain refers to the surface any more.
        unsafe { self.surface_fn.destroy_surface(self.surface, None) };
        self.surface = vk::SurfaceKHR::null();
        match create_surface(&self.win32_fn, self.handles) {
            Ok(s) => {
                self.surface = s;
                self.counters.surface_recreates = self.counters.surface_recreates.saturating_add(1);
                self.machine.feed(Signal::SurfaceRecreated);
            }
            Err(_) => {
                self.machine.feed(Signal::SurfaceLost);
            }
        }
        Ok(())
    }

    /// Brings the machine to `Running` or reports why no frame is possible now.
    fn settle(&mut self) -> Result<Option<&'static str>, Refusal> {
        for _ in 0..8 {
            match self.machine.state() {
                SwapState::Running => return Ok(None),
                SwapState::Uncreated | SwapState::Stale => {
                    let before = self.counters.swapchain_creates;
                    self.recreate()?;
                    if self.machine.state() == SwapState::Stale && self.counters.swapchain_creates == before {
                        return Ok(Some("recreated"));
                    }
                }
                SwapState::Minimized => {
                    // SAFETY: read-only query.
                    let caps = unsafe {
                        self.surface_fn.get_physical_device_surface_capabilities(self.core.physical, self.surface)
                    };
                    match caps {
                        Ok(c) if c.current_extent.width != 0 && c.current_extent.height != 0 => {
                            self.machine.feed(Signal::Restored);
                        }
                        Ok(_) => {
                            self.counters.skipped_minimized = self.counters.skipped_minimized.saturating_add(1);
                            return Ok(Some("minimized"));
                        }
                        Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                            self.machine.feed(Signal::SurfaceLost);
                        }
                        Err(e) => return Err(vk_refusal(e)),
                    }
                }
                SwapState::SurfaceLost { .. } => self.recreate_surface()?,
                SwapState::DeviceLost => return Err(Refusal::DeviceLost),
                SwapState::SurfaceUnrecoverable => return Err(Refusal::SurfaceUnrecoverable),
            }
        }
        Ok(Some("recreated"))
    }

    /// Copies `pane` into the next swapchain image (centred, 1:1, background around it) and presents. With
    /// `capture`, the swapchain image is read back before present: the genuine frame.
    pub fn frame(&mut self, owner: &Owner, pane: PaneId, capture: bool) -> Result<FrameOutcome, Refusal> {
        let (pane_image, pw, ph) = {
            let p = owner.pane(pane)?;
            if p.layout != vk::ImageLayout::TRANSFER_SRC_OPTIMAL {
                return Err(Refusal::InvalidPane);
            }
            (p.image, p.width, p.height)
        };
        if !Arc::ptr_eq(owner.core(), &self.core) {
            return Err(Refusal::InvalidPane);
        }
        if let Some(why) = self.settle()? {
            return Ok(FrameOutcome::Skipped(why));
        }
        // SAFETY: live swapchain; the acquire semaphore is unsignalled (its last wait completed inside a waited submit).
        let acquired = unsafe {
            self.swap_fn.acquire_next_image(self.swapchain, ACQUIRE_TIMEOUT_NS, self.acquire_sem, vk::Fence::null())
        };
        let (index, suboptimal) = match acquired {
            Ok(v) => v,
            Err(vk::Result::TIMEOUT | vk::Result::NOT_READY) => {
                self.counters.acquire_timeouts = self.counters.acquire_timeouts.saturating_add(1);
                self.machine.feed(Signal::AcquireTimeout);
                return Ok(FrameOutcome::Skipped("acquire_timeout"));
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.counters.out_of_date = self.counters.out_of_date.saturating_add(1);
                self.machine.feed(Signal::OutOfDate);
                return Ok(FrameOutcome::Skipped("recreated"));
            }
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                self.machine.feed(Signal::SurfaceLost);
                return Ok(FrameOutcome::Skipped("recreated"));
            }
            Err(vk::Result::ERROR_DEVICE_LOST) => {
                self.machine.feed(Signal::DeviceLost);
                return Err(Refusal::DeviceLost);
            }
            Err(e) => return Err(vk_refusal(e)),
        };
        self.machine.feed(Signal::Acquired);
        let image = *self.images.get(index as usize).ok_or(Refusal::Vk(vk::Result::ERROR_UNKNOWN.as_raw()))?;
        let signal = *self.present_sems.get(index as usize).ok_or(Refusal::Vk(vk::Result::ERROR_UNKNOWN.as_raw()))?;
        let (sw, sh) = self.extent;
        let copy = blit_region(pw, ph, sw, sh);
        let core = Arc::clone(&self.core);
        self.submit.begin(&core)?;
        let cmd = self.submit.cmd;
        image_barrier(
            &core,
            cmd,
            image,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::AccessFlags::empty(),
            vk::AccessFlags::TRANSFER_WRITE,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::TRANSFER,
        );
        if pw < sw || ph < sh {
            let value = vk::ClearColorValue { float32: self.background.map(|c| f32::from(c) / 255.0) };
            // SAFETY: recording; image in TRANSFER_DST_OPTIMAL.
            unsafe {
                core.device.cmd_clear_color_image(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &value,
                    &[color_range()],
                )
            };
            image_barrier(
                &core,
                cmd,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
            );
        }
        if let Some(region) = copy {
            // SAFETY: the pane is TRANSFER_SRC_OPTIMAL (owner invariant); R8G8B8A8_UNORM BLIT_SRC and
            // B8G8R8A8_UNORM BLIT_DST are checked or mandatory; regions are inside both images (blit_region).
            unsafe {
                core.device.cmd_blit_image(
                    cmd,
                    pane_image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                    vk::Filter::NEAREST,
                )
            };
        }
        let captured = capture && self.capture.is_some();
        if captured {
            image_barrier(
                &core,
                cmd,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::TRANSFER_READ,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
            );
            let buffer = self.capture.as_ref().map(|c| c.buffer).unwrap_or_default();
            let region = vk::BufferImageCopy::default()
                .image_subresource(color_layers())
                .image_extent(vk::Extent3D { width: sw, height: sh, depth: 1 });
            // SAFETY: the capture buffer holds sw*sh*4 bytes (made with this swapchain); image TRANSFER_SRC_OPTIMAL.
            unsafe {
                core.device.cmd_copy_image_to_buffer(cmd, image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, buffer, &[region])
            };
            host_read_barrier(&core, cmd, buffer);
            image_barrier(
                &core,
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::AccessFlags::TRANSFER_READ,
                vk::AccessFlags::empty(),
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            );
        } else {
            image_barrier(
                &core,
                cmd,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::empty(),
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            );
        }
        match self.submit.submit_wait(&core, &[(self.acquire_sem, vk::PipelineStageFlags::TRANSFER)], &[signal]) {
            Ok(()) => {}
            Err(Refusal::DeviceLost) => {
                self.machine.feed(Signal::DeviceLost);
                return Err(Refusal::DeviceLost);
            }
            Err(e) => return Err(e),
        }
        let capture = if captured {
            self.capture.as_ref().map(|c| Capture { width: sw, height: sh, rgba8: bgra_to_rgba(c.read()) })
        } else {
            None
        };
        let sems = [signal];
        let chains = [self.swapchain];
        let indices = [index];
        let present = vk::PresentInfoKHR::default().wait_semaphores(&sems).swapchains(&chains).image_indices(&indices);
        // SAFETY: the image was acquired and its layout is PRESENT_SRC_KHR once `signal` is signalled.
        let presented = unsafe { self.swap_fn.queue_present(core.queue, &present) };
        match presented {
            Ok(sub) => {
                self.counters.frames_presented = self.counters.frames_presented.saturating_add(1);
                if sub || suboptimal {
                    self.counters.suboptimal = self.counters.suboptimal.saturating_add(1);
                    self.machine.feed(Signal::Suboptimal);
                } else {
                    self.machine.feed(Signal::Presented);
                }
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.counters.out_of_date = self.counters.out_of_date.saturating_add(1);
                self.machine.feed(Signal::OutOfDate);
            }
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                self.machine.feed(Signal::SurfaceLost);
            }
            Err(vk::Result::ERROR_DEVICE_LOST) => {
                self.machine.feed(Signal::DeviceLost);
                return Err(Refusal::DeviceLost);
            }
            Err(e) => return Err(vk_refusal(e)),
        }
        Ok(FrameOutcome::Presented { image_index: index, capture })
    }
}

fn create_surface(win32: &ash::khr::win32_surface::Instance, h: WindowHandles) -> Result<vk::SurfaceKHR, Refusal> {
    let info = vk::Win32SurfaceCreateInfoKHR::default().hinstance(h.hinstance).hwnd(h.hwnd);
    // SAFETY: the host passes the handles of a live window it created in this process (checked non-zero above).
    unsafe { win32.create_win32_surface(&info, None) }.map_err(|e| match e {
        vk::Result::ERROR_SURFACE_LOST_KHR | vk::Result::ERROR_NATIVE_WINDOW_IN_USE_KHR => Refusal::InvalidWindow,
        other => vk_refusal(other),
    })
}

/// The centred 1:1 region of a `pw x ph` pane in a `sw x sh` image, clipped to both; `None` when empty.
pub fn centred_region(pw: u32, ph: u32, sw: u32, sh: u32) -> Option<((u32, u32), (u32, u32), (u32, u32))> {
    let w = pw.min(sw);
    let h = ph.min(sh);
    if w == 0 || h == 0 {
        return None;
    }
    let src = ((pw - w) / 2, (ph - h) / 2);
    let dst = ((sw - w) / 2, (sh - h) / 2);
    Some((src, dst, (w, h)))
}

fn blit_region(pw: u32, ph: u32, sw: u32, sh: u32) -> Option<vk::ImageBlit> {
    let (src, dst, (w, h)) = centred_region(pw, ph, sw, sh)?;
    let off = |x: u32, y: u32| -> Option<vk::Offset3D> {
        Some(vk::Offset3D { x: i32::try_from(x).ok()?, y: i32::try_from(y).ok()?, z: 0 })
    };
    let end = |x: u32, y: u32| -> Option<vk::Offset3D> {
        Some(vk::Offset3D { x: i32::try_from(x.checked_add(w)?).ok()?, y: i32::try_from(y.checked_add(h)?).ok()?, z: 1 })
    };
    Some(
        vk::ImageBlit::default()
            .src_subresource(color_layers())
            .src_offsets([off(src.0, src.1)?, end(src.0, src.1)?])
            .dst_subresource(color_layers())
            .dst_offsets([off(dst.0, dst.1)?, end(dst.0, dst.1)?]),
    )
}

fn bgra_to_rgba(mut bytes: Vec<u8>) -> Vec<u8> {
    for px in bytes.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    bytes
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.wait_idle();
        self.destroy_swapchain();
        let core = Arc::clone(&self.core);
        self.submit.destroy(&core);
        // SAFETY: device idle; nothing refers to the semaphore or the surface any more.
        unsafe {
            core.device.destroy_semaphore(self.acquire_sem, None);
            if self.surface != vk::SurfaceKHR::null() {
                self.surface_fn.destroy_surface(self.surface, None);
            }
        }
    }
}
