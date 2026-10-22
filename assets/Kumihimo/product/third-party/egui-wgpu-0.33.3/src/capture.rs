use egui::{UserData, ViewportId};
use epaint::ColorImage;
use std::sync::{Arc, mpsc};
use wgpu::{BindGroupLayout, MultisampleState, StoreOp};

/// A texture and a buffer for reading the rendered frame back to the cpu.
///
/// The texture is required since [`wgpu::TextureUsages::COPY_SRC`] is not an allowed
/// flag for the surface texture on all platforms. This means that anytime we want to
/// capture the frame, we first render it to this texture, and then we can copy it to
/// both the surface texture (via a render pass) and the buffer (via a texture to buffer copy),
/// from where we can pull it back
/// to the cpu.
pub struct CaptureState {
    padding: BufferPadding,
    pub texture: wgpu::Texture,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

pub type CaptureReceiver = mpsc::Receiver<(ViewportId, Vec<UserData>, ColorImage)>;
pub type CaptureSender = mpsc::Sender<(ViewportId, Vec<UserData>, ColorImage)>;
pub use mpsc::channel as capture_channel;

impl CaptureState {
    pub fn new(device: &wgpu::Device, surface_texture: &wgpu::Texture) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("texture_copy.wgsl"));

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("texture_copy"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(surface_texture.format().into())],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let bind_group_layout = pipeline.get_bind_group_layout(0);

        let (texture, padding, bind_group) =
            Self::create_texture(device, surface_texture, &bind_group_layout);

        Self {
            padding,
            texture,
            pipeline,
            bind_group,
        }
    }

    fn create_texture(
        device: &wgpu::Device,
        surface_texture: &wgpu::Texture,
        layout: &BindGroupLayout,
    ) -> (wgpu::Texture, BufferPadding, wgpu::BindGroup) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("egui_screen_capture_texture"),
            size: surface_texture.size(),
            mip_level_count: surface_texture.mip_level_count(),
            sample_count: surface_texture.sample_count(),
            dimension: surface_texture.dimension(),
            format: surface_texture.format(),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let padding = BufferPadding::new(surface_texture.width());

        let view = texture.create_view(&Default::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
            label: None,
        });

        (texture, padding, bind_group)
    }

    /// Updates the [`CaptureState`] if the size of the surface texture has changed
    pub fn update(&mut self, device: &wgpu::Device, texture: &wgpu::Texture) {
        if self.texture.size() != texture.size() {
            let (new_texture, padding, bind_group) =
                Self::create_texture(device, texture, &self.pipeline.get_bind_group_layout(0));
            self.texture = new_texture;
            self.padding = padding;
            self.bind_group = bind_group;
        }
    }

    /// Handles copying from the [`CaptureState`] texture to the surface texture and the buffer.
    /// Pass the returned buffer to [`CaptureState::read_screen_rgba`] to read the data back to the cpu.
    pub fn copy_textures(
        &mut self,
        device: &wgpu::Device,
        output_frame: &wgpu::SurfaceTexture,
        encoder: &mut wgpu::CommandEncoder,
    ) -> wgpu::Buffer {
        debug_assert_eq!(
            self.texture.size(),
            output_frame.texture.size(),
            "Texture sizes must match, `CaptureState::update` was probably not called"
        );

        // It would be more efficient to reuse the Buffer, e.g. via some kind of ring buffer, but
        // for most screenshot use cases this should be fine. When taking many screenshots (e.g. for a video)
        // it might make sense to revisit this and implement a more efficient solution.
        #[allow(clippy::arc_with_non_send_sync, clippy::allow_attributes)] // For wasm
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui_screen_capture_buffer"),
            size: (self.padding.padded_bytes_per_row * self.texture.height()) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let padding = self.padding;
        let tex = &mut self.texture;

        let tex_extent = tex.size();

        encoder.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padding.padded_bytes_per_row),
                    rows_per_image: None,
                },
            },
            tex_extent,
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("texture_copy"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output_frame.texture.create_view(&Default::default()),
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);

        buffer
    }

    /// Handles copying from the [`CaptureState`] texture to the surface texture and the cpu
    /// This function is non-blocking and will send the data to the given sender when it's ready.
    /// Pass in the buffer returned from [`CaptureState::copy_textures`].
    /// Make sure to call this after the encoder has been submitted.
    pub fn read_screen_rgba(
        &self,
        ctx: egui::Context,
        buffer: wgpu::Buffer,
        data: Vec<UserData>,
        tx: CaptureSender,
        viewport_id: ViewportId,
    ) {
        #[allow(clippy::arc_with_non_send_sync, clippy::allow_attributes)] // For wasm
        let buffer = Arc::new(buffer);
        let buffer_clone = buffer.clone();
        let buffer_slice = buffer_clone.slice(..);
        let format = self.texture.format();
        let tex_extent = self.texture.size();
        let padding = self.padding;
        let to_rgba = match format {
            wgpu::TextureFormat::Rgba8Unorm => [0, 1, 2, 3],
            wgpu::TextureFormat::Bgra8Unorm => [2, 1, 0, 3],
            _ => {
                log::error!(
                    "Screen can't be captured unless the surface format is Rgba8Unorm or Bgra8Unorm. Current surface format is {format:?}"
                );
                return;
            }
        };
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            if let Err(err) = result {
                log::error!("Failed to map buffer for reading: {err}");
                return;
            }
            let buffer_slice = buffer.slice(..);

            let mut pixels = Vec::with_capacity((tex_extent.width * tex_extent.height) as usize);
            for padded_row in buffer_slice
                .get_mapped_range()
                .chunks(padding.padded_bytes_per_row as usize)
            {
                let row = &padded_row[..padding.unpadded_bytes_per_row as usize];
                for color in row.chunks(4) {
                    pixels.push(epaint::Color32::from_rgba_premultiplied(
                        color[to_rgba[0]],
                        color[to_rgba[1]],
                        color[to_rgba[2]],
                        color[to_rgba[3]],
                    ));
                }
            }
            buffer.unmap();

            tx.send((
                viewport_id,
                data,
                ColorImage::new(
                    [tex_extent.width as usize, tex_extent.height as usize],
                    pixels,
                ),
            ))
            .ok();
            ctx.request_repaint();
        });
    }
}

#[derive(Copy, Clone)]
struct BufferPadding {
    unpadded_bytes_per_row: u32,
    padded_bytes_per_row: u32,
}

impl BufferPadding {
    fn new(width: u32) -> Self {
        let bytes_per_pixel = std::mem::size_of::<u32>() as u32;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let padded_bytes_per_row =
            wgpu::util::align_to(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        Self {
            unpadded_bytes_per_row,
            padded_bytes_per_row,
        }
    }
}

#[cfg(feature = "winit")]
pub use native::{NativeCaptureFailure, NativeCaptureHandle, NativeCaptureOutcome,
    NativeCaptureReceipt, NativeCaptureRequest, NativeCaptureTarget};

#[cfg(feature = "winit")]
pub(crate) use native::NativeCaptureProducer;

#[cfg(feature = "winit")]
mod native {
    use super::*;
    use std::{collections::HashMap, sync::{Mutex, Weak, atomic::{AtomicU64, AtomicUsize, Ordering}}};
    use winit::window::Window;

    const MAX_REQUESTS: usize = 8;
    const MAX_READBACK_BYTES: u64 = 128 * 1024 * 1024;
    static NEXT_INCARNATION: AtomicU64 = AtomicU64::new(1);

    /// An explicit failure of the private native capture path.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
    pub enum NativeCaptureFailure {
        #[error("No native renderer surface exists for this viewport")]
        UnknownViewport,
        #[error("The surface does not own its native window")]
        UnverifiableWindow,
        #[error("The native capture queue is full")]
        QueueFull,
        #[error("The native readback byte budget is exhausted")]
        ByteBudgetExceeded,
        #[error("The request belongs to a different renderer")]
        ForeignRequest,
        #[error("The request was already consumed")]
        AlreadyConsumed,
        #[error("The request was cancelled")]
        Cancelled,
        #[error("The renderer or native surface was removed")]
        SurfaceRemoved,
        #[error("The native window, size, scale or surface incarnation changed")]
        StaleTarget,
        #[error("The renderer skipped the requested frame")]
        FrameSkipped,
        #[error("The renderer recreated the requested surface")]
        SurfaceRecreated,
        #[error("The native surface cannot be copied for readback")]
        UnsupportedSurfaceCopy,
        #[error("The surface format is unsupported for native capture")]
        UnsupportedFormat,
        #[error("The native GPU readback mapping failed")]
        MapFailed,
        #[error("The native GPU readback has an invalid byte count")]
        InvalidReadback,
    }

    /// A renderer-authenticated snapshot of an owned, configured native surface.
    #[derive(Clone, Debug)]
    pub struct NativeCaptureTarget {
        window: Arc<Window>,
        viewport_id: ViewportId,
        renderer_incarnation: u64,
        surface_incarnation: u64,
        physical_dimensions: [u32; 2],
        scale_factor: f64,
    }

    impl NativeCaptureTarget {
        pub fn window(&self) -> &Arc<Window> { &self.window }
        pub fn viewport_id(&self) -> ViewportId { self.viewport_id }
        pub fn renderer_incarnation(&self) -> u64 { self.renderer_incarnation }
        pub fn surface_incarnation(&self) -> u64 { self.surface_incarnation }
        pub fn physical_dimensions(&self) -> [u32; 2] { self.physical_dimensions }
        pub fn scale_factor(&self) -> f64 { self.scale_factor }

        fn same_binding(&self, other: &Self) -> bool {
            Arc::ptr_eq(&self.window, &other.window)
                && self.viewport_id == other.viewport_id
                && self.renderer_incarnation == other.renderer_incarnation
                && self.surface_incarnation == other.surface_incarnation
                && self.physical_dimensions == other.physical_dimensions
                && self.scale_factor.to_bits() == other.scale_factor.to_bits()
        }

        fn live(&self) -> bool {
            let size = self.window.inner_size();
            [size.width, size.height] == self.physical_dimensions
                && self.window.scale_factor().to_bits() == self.scale_factor.to_bits()
                && size.width > 0 && size.height > 0
        }
    }

    /// A one-shot request. Dropping it cancels any outstanding completion.
    #[derive(Debug)]
    pub struct NativeCaptureRequest {
        target: NativeCaptureTarget,
        request_id: u64,
        state: Weak<Mutex<State>>,
    }

    impl NativeCaptureRequest {
        pub fn request_id(&self) -> u64 { self.request_id }
        pub fn viewport_id(&self) -> ViewportId { self.target.viewport_id }
        pub fn target(&self) -> &NativeCaptureTarget { &self.target }
    }

    impl Drop for NativeCaptureRequest {
        fn drop(&mut self) {
            if let Some(state) = self.state.upgrade() {
                state.lock().unwrap().requests.remove(&self.request_id);
            }
        }
    }

    /// GPU readback from a native surface after its presentation call completed.
    /// This does not assert physical display of the frame.
    #[derive(Debug)]
    pub struct NativeCaptureReceipt {
        target: NativeCaptureTarget,
        request_id: u64,
        submission_id: u64,
        readback_bytes: usize,
        image: Arc<ColorImage>,
        _allocation: Arc<ReadbackAllocation>,
    }

    impl NativeCaptureReceipt {
        pub fn window(&self) -> &Arc<Window> { self.target.window() }
        pub fn viewport_id(&self) -> ViewportId { self.target.viewport_id() }
        pub fn renderer_incarnation(&self) -> u64 { self.target.renderer_incarnation() }
        pub fn surface_incarnation(&self) -> u64 { self.target.surface_incarnation() }
        pub fn request_id(&self) -> u64 { self.request_id }
        pub fn physical_dimensions(&self) -> [u32; 2] { self.target.physical_dimensions() }
        pub fn scale_factor(&self) -> f64 { self.target.scale_factor() }
        pub fn submission_id(&self) -> u64 { self.submission_id }
        pub fn readback_bytes(&self) -> usize { self.readback_bytes }
        pub fn image(&self) -> &ColorImage { &self.image }
    }

    #[derive(Debug)]
    pub enum NativeCaptureOutcome {
        Presented(NativeCaptureReceipt),
        Failed(NativeCaptureFailure),
    }

    /// Requests and consumes native captures without mutable application input.
    #[derive(Clone, Debug)]
    pub struct NativeCaptureHandle {
        state: Arc<Mutex<State>>,
        context: egui::Context,
    }

    #[derive(Debug)]
    struct Pending {
        target: NativeCaptureTarget,
        in_flight: bool,
        presented: bool,
        submission_id: u64,
        image: Option<(Arc<ColorImage>, Arc<ReadbackAllocation>)>,
        outcome: Option<NativeCaptureOutcome>,
    }

    #[derive(Debug)]
    struct State {
        incarnation: u64,
        next_request: u64,
        surfaces: HashMap<ViewportId, Option<NativeCaptureTarget>>,
        requests: HashMap<u64, Pending>,
        readback_bytes: Arc<AtomicU64>,
        readback_count: Arc<AtomicUsize>,
        outstanding_maps: Arc<AtomicUsize>,
    }

    impl NativeCaptureHandle {
        pub fn target(&self, viewport_id: ViewportId) -> Result<NativeCaptureTarget, NativeCaptureFailure> {
            let state = self.state.lock().unwrap();
            let target = state.surfaces.get(&viewport_id)
                .ok_or(NativeCaptureFailure::UnknownViewport)?
                .as_ref().ok_or(NativeCaptureFailure::UnverifiableWindow)?;
            if !target.live() { return Err(NativeCaptureFailure::StaleTarget); }
            Ok(target.clone())
        }

        pub fn request(&self, viewport_id: ViewportId) -> Result<NativeCaptureRequest, NativeCaptureFailure> {
            let mut state = self.state.lock().unwrap();
            if state.requests.len() >= MAX_REQUESTS { return Err(NativeCaptureFailure::QueueFull); }
            let target = state.surfaces.get(&viewport_id)
                .ok_or(NativeCaptureFailure::UnknownViewport)?
                .as_ref().ok_or(NativeCaptureFailure::UnverifiableWindow)?.clone();
            if !target.live() { return Err(NativeCaptureFailure::StaleTarget); }
            let request_id = state.next_request;
            state.next_request = state.next_request.checked_add(1).expect("Native capture request identity exhausted");
            state.requests.insert(request_id, Pending {
                target: target.clone(), in_flight: false, presented: false,
                submission_id: 0, image: None, outcome: None,
            });
            drop(state);
            self.context.request_repaint_of(viewport_id);
            Ok(NativeCaptureRequest { target, request_id, state: Arc::downgrade(&self.state) })
        }

        pub fn poll(&self, request: &NativeCaptureRequest) -> Result<Option<NativeCaptureOutcome>, NativeCaptureFailure> {
            if !Weak::ptr_eq(&request.state, &Arc::downgrade(&self.state)) {
                return Err(NativeCaptureFailure::ForeignRequest);
            }
            let mut state = self.state.lock().unwrap();
            let target_matches = state.surfaces.get(&request.target.viewport_id)
                .and_then(Option::as_ref).is_some_and(|target| target.same_binding(&request.target));
            let pending = state.requests.get_mut(&request.request_id).ok_or(NativeCaptureFailure::AlreadyConsumed)?;
            if (!target_matches || !pending.target.live())
                && !matches!(pending.outcome, Some(NativeCaptureOutcome::Failed(_))) {
                pending.image = None;
                pending.outcome = Some(NativeCaptureOutcome::Failed(NativeCaptureFailure::StaleTarget));
            }
            if pending.outcome.is_some() {
                Ok(state.requests.remove(&request.request_id).unwrap().outcome)
            } else { Ok(None) }
        }

        pub fn cancel(&self, request: &NativeCaptureRequest) -> Result<(), NativeCaptureFailure> {
            if !Weak::ptr_eq(&request.state, &Arc::downgrade(&self.state)) {
                return Err(NativeCaptureFailure::ForeignRequest);
            }
            let mut state = self.state.lock().unwrap();
            let pending = state.requests.get_mut(&request.request_id).ok_or(NativeCaptureFailure::AlreadyConsumed)?;
            pending.image = None;
            pending.outcome = Some(NativeCaptureOutcome::Failed(NativeCaptureFailure::Cancelled));
            Ok(())
        }
    }

    pub(crate) struct NativeCaptureProducer {
        handle: NativeCaptureHandle,
    }

    pub(crate) struct ReadbackTicket {
        state: Arc<Mutex<State>>,
        request_ids: Vec<u64>,
        allocation: Arc<ReadbackAllocation>,
        outstanding_maps: Arc<AtomicUsize>,
    }

    impl Drop for ReadbackTicket {
        fn drop(&mut self) {
            self.outstanding_maps.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[derive(Debug)]
    struct ReadbackAllocation {
        budget: Arc<AtomicU64>,
        count: Arc<AtomicUsize>,
        reserved_bytes: u64,
    }

    impl Drop for ReadbackAllocation {
        fn drop(&mut self) {
            self.budget.fetch_sub(self.reserved_bytes, Ordering::Relaxed);
            self.count.fetch_sub(1, Ordering::Relaxed);
        }
    }

    impl NativeCaptureProducer {
        pub(crate) fn new(context: egui::Context) -> Self {
            Self { handle: NativeCaptureHandle { context, state: Arc::new(Mutex::new(State {
                incarnation: NEXT_INCARNATION.fetch_add(1, Ordering::Relaxed), next_request: 1,
                surfaces: HashMap::new(), requests: HashMap::new(), readback_bytes: Arc::new(AtomicU64::new(0)),
                readback_count: Arc::new(AtomicUsize::new(0)),
                outstanding_maps: Arc::new(AtomicUsize::new(0)),
            })) } }
        }

        pub(crate) fn handle(&self) -> NativeCaptureHandle { self.handle.clone() }

        pub(crate) fn register(&self, viewport_id: ViewportId, window: Option<Arc<Window>>, incarnation: u64, dimensions: [u32; 2]) {
            let mut state = self.handle.state.lock().unwrap();
            let target = window.map(|window| NativeCaptureTarget {
                scale_factor: window.scale_factor(), window, viewport_id,
                renderer_incarnation: state.incarnation, surface_incarnation: incarnation,
                physical_dimensions: dimensions,
            });
            for pending in state.requests.values_mut().filter(|pending| pending.target.viewport_id == viewport_id) {
                if !matches!(pending.outcome, Some(NativeCaptureOutcome::Failed(_)))
                    && !target.as_ref().is_some_and(|target| target.same_binding(&pending.target)) {
                    pending.image = None;
                    pending.outcome = Some(NativeCaptureOutcome::Failed(NativeCaptureFailure::StaleTarget));
                }
            }
            state.surfaces.insert(viewport_id, target);
        }

        pub(crate) fn remove(&self, viewport_id: ViewportId) {
            let mut state = self.handle.state.lock().unwrap();
            state.surfaces.remove(&viewport_id);
            for pending in state.requests.values_mut().filter(|pending| pending.target.viewport_id == viewport_id) {
                if !matches!(pending.outcome, Some(NativeCaptureOutcome::Failed(_))) {
                    pending.image = None;
                    pending.outcome = Some(NativeCaptureOutcome::Failed(NativeCaptureFailure::SurfaceRemoved));
                }
            }
        }

        pub(crate) fn fail(&self, viewport_id: ViewportId, failure: NativeCaptureFailure) {
            let mut state = self.handle.state.lock().unwrap();
            for pending in state.requests.values_mut().filter(|pending| pending.target.viewport_id == viewport_id && pending.outcome.is_none()) {
                pending.image = None;
                pending.outcome = Some(NativeCaptureOutcome::Failed(failure));
            }
        }

        pub(crate) fn needs_poll(&self) -> bool {
            self.handle.state.lock().unwrap().outstanding_maps.load(Ordering::Relaxed) > 0
        }

        pub(crate) fn begin(&self, viewport_id: ViewportId, submission_id: u64, texture: &wgpu::Texture) -> Option<ReadbackTicket> {
            let mut state = self.handle.state.lock().unwrap();
            let target = state.surfaces.get(&viewport_id).and_then(Option::as_ref)?.clone();
            let mut ids = Vec::new();
            for (id, pending) in &mut state.requests {
                if pending.target.viewport_id == viewport_id && !pending.in_flight && pending.outcome.is_none() {
                    if !target.same_binding(&pending.target) || !target.live() {
                        pending.outcome = Some(NativeCaptureOutcome::Failed(NativeCaptureFailure::StaleTarget));
                    } else { ids.push(*id); }
                }
            }
            if ids.is_empty() { return None; }
            let failure = if !matches!(texture.format(), wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb | wgpu::TextureFormat::Bgra8UnormSrgb) {
                Some(NativeCaptureFailure::UnsupportedFormat)
            } else if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
                Some(NativeCaptureFailure::UnsupportedSurfaceCopy)
            } else if [texture.width(), texture.height()] != target.physical_dimensions {
                Some(NativeCaptureFailure::StaleTarget)
            } else { None };
            let padded_bytes = u64::from(BufferPadding::new(texture.width()).padded_bytes_per_row) * u64::from(texture.height());
            let image_bytes = u64::from(texture.width()) * u64::from(texture.height()) * 4;
            let reserved_bytes = padded_bytes + image_bytes;
            let failure = failure.or_else(||
                (state.readback_count.load(Ordering::Relaxed) >= MAX_REQUESTS).then_some(NativeCaptureFailure::QueueFull))
                .or_else(|| (reserved_bytes > MAX_READBACK_BYTES.saturating_sub(state.readback_bytes.load(Ordering::Relaxed))).then_some(NativeCaptureFailure::ByteBudgetExceeded));
            if let Some(failure) = failure {
                for id in ids { state.requests.get_mut(&id).unwrap().outcome = Some(NativeCaptureOutcome::Failed(failure)); }
                return None;
            }
            state.readback_bytes.fetch_add(reserved_bytes, Ordering::Relaxed);
            state.readback_count.fetch_add(1, Ordering::Relaxed);
            state.outstanding_maps.fetch_add(1, Ordering::Relaxed);
            for id in &ids {
                let pending = state.requests.get_mut(id).unwrap();
                pending.in_flight = true;
                pending.submission_id = submission_id;
            }
            Some(ReadbackTicket { state: self.handle.state.clone(), request_ids: ids,
                outstanding_maps: state.outstanding_maps.clone(),
                allocation: Arc::new(ReadbackAllocation { budget: state.readback_bytes.clone(), count: state.readback_count.clone(), reserved_bytes }) })
        }

        pub(crate) fn presented(&self, viewport_id: ViewportId, submission_id: u64) {
            let mut state = self.handle.state.lock().unwrap();
            for (id, pending) in &mut state.requests {
                if pending.target.viewport_id == viewport_id && pending.submission_id == submission_id && pending.in_flight {
                    pending.presented = true;
                    complete(*id, pending);
                }
            }
        }

        pub(crate) fn readback(&self, ctx: egui::Context, device: &wgpu::Device, texture: &wgpu::Texture, encoder: &mut wgpu::CommandEncoder, ticket: ReadbackTicket) -> NativeReadback {
            let padding = BufferPadding::new(texture.width());
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("egui_native_surface_readback"), size: u64::from(padding.padded_bytes_per_row) * u64::from(texture.height()),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(texture.as_image_copy(), wgpu::TexelCopyBufferInfo {
                buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padding.padded_bytes_per_row), rows_per_image: None },
            }, texture.size());
            NativeReadback { buffer, padding, dimensions: [texture.width(), texture.height()], format: texture.format(), ticket, ctx }
        }
    }

    impl Drop for NativeCaptureProducer {
        fn drop(&mut self) {
            let mut state = self.handle.state.lock().unwrap();
            state.surfaces.clear();
            for pending in state.requests.values_mut() {
                pending.image = None;
                pending.outcome = Some(NativeCaptureOutcome::Failed(NativeCaptureFailure::SurfaceRemoved));
            }
        }
    }

    pub(crate) struct NativeReadback {
        buffer: wgpu::Buffer,
        padding: BufferPadding,
        dimensions: [u32; 2],
        format: wgpu::TextureFormat,
        ticket: ReadbackTicket,
        ctx: egui::Context,
    }

    impl NativeReadback {
        pub(crate) fn map(self) {
            let Self { buffer, padding, dimensions, format, ticket, ctx } = self;
            let buffer = Arc::new(buffer);
            let callback_buffer = buffer.clone();
            buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
                let image = if result.is_err() { Err(NativeCaptureFailure::MapFailed) } else {
                    let mapped = callback_buffer.slice(..).get_mapped_range();
                    let expected = u64::from(padding.padded_bytes_per_row) * u64::from(dimensions[1]);
                    let image = if mapped.len() as u64 != expected {
                        Err(NativeCaptureFailure::InvalidReadback)
                    } else {
                        let order = if matches!(format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb) { [2, 1, 0, 3] } else { [0, 1, 2, 3] };
                        let mut pixels = Vec::with_capacity(dimensions[0] as usize * dimensions[1] as usize);
                        for row in mapped.chunks_exact(padding.padded_bytes_per_row as usize) {
                            for pixel in row[..padding.unpadded_bytes_per_row as usize].chunks_exact(4) {
                                pixels.push(epaint::Color32::from_rgba_premultiplied(pixel[order[0]], pixel[order[1]], pixel[order[2]], pixel[order[3]]));
                            }
                        }
                        Ok(Arc::new(ColorImage::new([dimensions[0] as usize, dimensions[1] as usize], pixels)))
                    };
                    drop(mapped);
                    callback_buffer.unmap();
                    image
                };
                {
                    let mut state = ticket.state.lock().unwrap();
                    for id in &ticket.request_ids {
                        if let Some(pending) = state.requests.get_mut(id) {
                            if pending.outcome.is_none() {
                                match &image {
                                    Ok(image) => { pending.image = Some((image.clone(), ticket.allocation.clone())); complete(*id, pending); }
                                    Err(failure) => pending.outcome = Some(NativeCaptureOutcome::Failed(*failure)),
                                }
                            }
                        }
                    }
                }
                drop(ticket);
                ctx.request_repaint();
            });
        }
    }

    fn complete(request_id: u64, pending: &mut Pending) {
        if pending.outcome.is_some() || !pending.presented { return; }
        if !pending.target.live() {
            pending.image = None;
            pending.outcome = Some(NativeCaptureOutcome::Failed(NativeCaptureFailure::StaleTarget));
        } else if let Some((image, allocation)) = pending.image.take() {
            let readback_bytes = image.pixels.len() * 4;
            pending.outcome = Some(NativeCaptureOutcome::Presented(NativeCaptureReceipt {
                target: pending.target.clone(), request_id, submission_id: pending.submission_id,
                readback_bytes, image, _allocation: allocation,
            }));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn native_capture_unknown_and_borrowed_targets_cannot_mint_requests() {
            let producer = NativeCaptureProducer::new(egui::Context::default());
            let handle = producer.handle();
            let viewport = ViewportId::ROOT;
            assert_eq!(handle.request(viewport).unwrap_err(), NativeCaptureFailure::UnknownViewport);
            producer.register(viewport, None, 1, [640, 480]);
            assert_eq!(handle.target(viewport).unwrap_err(), NativeCaptureFailure::UnverifiableWindow);
            assert_eq!(handle.request(viewport).unwrap_err(), NativeCaptureFailure::UnverifiableWindow);
            assert!(handle.state.lock().unwrap().requests.is_empty());
        }

        #[test]
        fn native_capture_removing_one_target_preserves_siblings() {
            let producer = NativeCaptureProducer::new(egui::Context::default());
            let handle = producer.handle();
            let child = ViewportId::from_hash_of("native-capture-child");
            producer.register(ViewportId::ROOT, None, 1, [640, 480]);
            producer.register(child, None, 2, [320, 240]);
            producer.remove(child);
            assert_eq!(handle.target(child).unwrap_err(), NativeCaptureFailure::UnknownViewport);
            assert_eq!(handle.target(ViewportId::ROOT).unwrap_err(), NativeCaptureFailure::UnverifiableWindow);
            drop(producer);
            assert_eq!(handle.target(ViewportId::ROOT).unwrap_err(), NativeCaptureFailure::UnknownViewport);
        }

        #[test]
        fn native_capture_allocation_tracks_shared_inflight_and_receipt_lifetime() {
            let budget = Arc::new(AtomicU64::new(4096));
            let count = Arc::new(AtomicUsize::new(1));
            let allocation = Arc::new(ReadbackAllocation { budget: budget.clone(), count: count.clone(), reserved_bytes: 4096 });
            let completion = allocation.clone();
            drop(allocation);
            assert_eq!(budget.load(Ordering::Relaxed), 4096);
            assert_eq!(count.load(Ordering::Relaxed), 1);
            drop(completion);
            assert_eq!(budget.load(Ordering::Relaxed), 0);
            assert_eq!(count.load(Ordering::Relaxed), 0);
        }

        #[test]
        fn native_capture_polling_outlives_requests_but_stops_before_retained_receipt_release() {
            let producer = NativeCaptureProducer::new(egui::Context::default());
            let state = producer.handle.state.clone();
            let (budget, count, maps) = {
                let state = state.lock().unwrap();
                (state.readback_bytes.clone(), state.readback_count.clone(), state.outstanding_maps.clone())
            };
            budget.store(4096, Ordering::Relaxed);
            count.store(1, Ordering::Relaxed);
            maps.store(1, Ordering::Relaxed);
            let allocation = Arc::new(ReadbackAllocation { budget: budget.clone(), count, reserved_bytes: 4096 });
            let ticket = ReadbackTicket { state, request_ids: Vec::new(), allocation: allocation.clone(), outstanding_maps: maps };
            assert!(producer.handle.state.lock().unwrap().requests.is_empty());
            assert!(producer.needs_poll());
            drop(ticket);
            assert!(!producer.needs_poll());
            assert_eq!(budget.load(Ordering::Relaxed), 4096);
            drop(allocation);
            assert_eq!(budget.load(Ordering::Relaxed), 0);
        }
    }
}
