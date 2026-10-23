//! Lane SMB-RENDER-GPU-SHARED-DEVICE (feature `desktop`): the viewer renders on the HOST's wgpu
//! device (an egui-wgpu `RenderState` built headlessly, as eframe builds it for the window) straight
//! into the texture egui draws: no read-back, no upload, no second device. The texture holds the
//! same pixels as the read-back frame (egui's gamma-space premultiplied convention, +-1). Export of
//! a display-only frame renders again on the read-back path. GPU proof policy: render-gpu
//! tests/gpu-proof.toml (`require_gpu = true`: no adapter FAILS with `NOT_PROVEN: no adapter`).
#![cfg(feature = "desktop")]

mod common;

use common::{Scratch, two_layer_psd};
use eframe::{egui_wgpu, wgpu};
use hsk_studio_harness::view::display::WgpuDisplay;
use hsk_studio_harness::view::export::ExportFormat;
use hsk_studio_harness::view::render::{GpuDisplay, Prefer};
use hsk_studio_harness::view::Viewer;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

const GPU_PROOF_POLICY: &str = include_str!("../../hsk-studio-render-gpu/tests/gpu-proof.toml");

fn require_gpu() -> bool {
    GPU_PROOF_POLICY.lines().map(str::trim).any(|l| l.replace(' ', "") == "require_gpu=true") || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let start = std::time::Instant::now();
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        assert!(start.elapsed().as_secs() < 30, "wgpu request timed out");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// The host render state as eframe creates it (default limits), without a window or surface.
fn host_state() -> Option<egui_wgpu::RenderState> {
    let backends = if cfg!(windows) { wgpu::Backends::DX12 } else { wgpu::Backends::PRIMARY };
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends, flags: wgpu::InstanceFlags::empty(), ..Default::default() });
    let adapter = match block(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(a) => a,
        Err(_) if !require_gpu() => {
            println!("NOT_PROVEN: no adapter; viewer shared display not run");
            return None;
        }
        Err(_) => panic!("NOT_PROVEN: no adapter; viewer shared display requires a GPU (render-gpu tests/gpu-proof.toml require_gpu = true)"),
    };
    let (device, queue) = block(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("test host"), required_limits: wgpu::Limits::default(), ..Default::default() })).expect("device");
    let target_format = wgpu::TextureFormat::Bgra8Unorm;
    let renderer = egui_wgpu::Renderer::new(&device, target_format, egui_wgpu::RendererOptions::default());
    Some(egui_wgpu::RenderState { adapter, available_adapters: Vec::new(), device, queue, target_format, renderer: Arc::new(egui::mutex::RwLock::new(renderer)) })
}

/// Lets the test read the display texture after handing the display to the viewer.
struct Probe(Rc<RefCell<WgpuDisplay>>);
impl GpuDisplay for Probe {
    fn present(
        &mut self,
        plan: &hsk_studio_pigment::render_plan::RenderPlan<hsk_studio_pigment::render_plan::NoVector>,
        request: &hsk_studio_pigment::render_plan::ExecuteRequest,
        sources: &dyn hsk_studio_pigment::render_plan::PlanSources,
        cancel: &hsk_studio_accord::CancellationToken,
    ) -> Result<egui::TextureId, String> {
        self.0.borrow_mut().present(plan, request, sources, cancel)
    }
}

fn read_texture(state: &egui_wgpu::RenderState, texture: &wgpu::Texture) -> Vec<u8> {
    let (w, h) = (texture.width(), texture.height());
    let row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = state.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: u64::from(row * h), usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut encoder = state.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    state.queue.submit(Some(encoder.finish()));
    buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    state.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(30)) }).expect("poll");
    let view = buffer.slice(..).get_mapped_range();
    (0..h).flat_map(|y| view[(y * row) as usize..(y * row + w * 4) as usize].to_vec()).collect()
}

#[test]
fn viewer_renders_on_the_host_device_into_the_display_texture() {
    let Some(state) = host_state() else { return };
    let scratch = Scratch::new("shared");
    let source = scratch.0.join("two.psd");
    std::fs::write(&source, two_layer_psd()).unwrap();

    // Reference: the read-back frame (render-cpu) of the same document.
    let mut cpu = Viewer::new(Prefer::Cpu);
    cpu.open(&source).unwrap();
    cpu.render_if_dirty();
    let reference = cpu.frame().unwrap().rgba8.clone();

    let display = Rc::new(RefCell::new(WgpuDisplay::new(&state).unwrap_or_else(|e| panic!("{e}"))));
    let mut v = Viewer::new(Prefer::Gpu);
    v.set_display(Box::new(Probe(Rc::clone(&display))));
    v.open(&source).unwrap();
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("{}", v.state_json()));
    assert_eq!(f.backend, "gpu-shared", "{:?}", f.notes);
    assert!(f.texture.is_some() && f.rgba8.is_empty(), "no read-back bytes for a display frame");
    assert_eq!(v.state_json()["frame"]["display_texture"], true);

    let bytes = read_texture(&state, display.borrow().texture().expect("display texture"));
    assert_eq!(bytes.len(), reference.len());
    for (i, (got, want)) in bytes.chunks_exact(4).zip(reference.chunks_exact(4)).enumerate() {
        // egui convention: gamma-space colour premultiplied by alpha.
        let a = f32::from(want[3]) / 255.0;
        let expect = [0, 1, 2].map(|c| (f32::from(want[c]) * a + 0.5).floor() as u8);
        for c in 0..3 {
            assert!(got[c].abs_diff(expect[c]) <= 1, "pixel {i} channel {c}: {got:?} vs {expect:?} (straight {want:?})");
        }
        assert!(got[3].abs_diff(want[3]) <= 1, "pixel {i} alpha: {got:?} vs {want:?}");
    }

    // Edits re-render into the same texture (same egui id: no re-registration per frame).
    let id = f.texture;
    let top = v.layers().into_iter().find(|r| r.name == "top").unwrap().layer_id;
    v.set_visible(&top, false).unwrap();
    v.render_if_dirty();
    assert_eq!(v.frame().unwrap().texture, id);
    let bytes = read_texture(&state, display.borrow().texture().unwrap());
    assert!(bytes.chunks_exact(4).all(|p| p[0].abs_diff(10) <= 1 && p[1].abs_diff(20) <= 1 && p[2].abs_diff(30) <= 1 && p[3] == 255), "{bytes:?}");

    // Export of a display-only frame renders on the read-back path.
    let report = v.export(&scratch.0.join("two.png"), ExportFormat::Png, false).expect("export");
    assert_eq!(report["backend"], "cpu");
}
