//! Lane SMB-RENDER-GPU-SHARED-DEVICE (feature `shared-device`): a plan rendered on a CALLER-OWNED
//! wgpu device straight into a caller texture equals the read-back path of the same backend,
//! encoded for display on the CPU (+-1 per 8-bit channel: GPU `pow`), with no read-back bytes;
//! target refusals happen before device work. GPU proof policy: tests/gpu-proof.toml
//! (`require_gpu = true`: no adapter FAILS with `NOT_PROVEN: no adapter`, never green).
#![cfg(feature = "shared-device")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer,
    NoVector, OutputTarget, PlanNode, PlanNodeId, PlanSources, RenderPlan, SampleFormat,
    SinkError, SourceKey, StudioBlendMode, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuError, SharedDevice, TARGET_FORMAT};

const GPU_PROOF_POLICY: &str = include_str!("gpu-proof.toml");
const EXTENT: Rect = Rect { x: 5, y: 3, width: 40, height: 24 };

fn require_gpu() -> bool {
    let mut require = None;
    for line in GPU_PROOF_POLICY.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
            Some(("require_gpu", "true")) => require = Some(true),
            Some(("require_gpu", "false")) => require = Some(false),
            Some(("schema", v)) => assert_eq!(v, "\"hsk.studio.render-gpu-proof@1\""),
            other => panic!("tests/gpu-proof.toml: unexpected line {other:?}"),
        }
    }
    require.expect("tests/gpu-proof.toml require_gpu") || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}

/// The caller's device, as a host such as egui-wgpu creates it (here headless). `None` = no adapter.
fn host_device(what: &str) -> Option<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)> {
    let backends = if cfg!(windows) { wgpu::Backends::DX12 } else { wgpu::Backends::PRIMARY };
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends, flags: wgpu::InstanceFlags::empty(), ..Default::default() });
    let adapter = match pollster_block(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    })) {
        Ok(a) => a,
        Err(_) if !require_gpu() => {
            println!("NOT_PROVEN: no adapter; {what} not run");
            return None;
        }
        Err(_) => panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml require_gpu = true)"),
    };
    let (device, queue) = pollster_block(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("test host device"),
        required_limits: wgpu::Limits::downlevel_defaults(),
        ..Default::default()
    }))
    .expect("adapter present but no device");
    Some((device, queue, adapter.get_info()))
}

/// wgpu native futures are ready on the first poll after the work is done; spin with a bound.
fn pollster_block<F: std::future::Future>(f: F) -> F::Output {
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

/// Opaque red over the whole extent, a half-transparent blue square at 70 %, a transparent strip
/// (layer 3 covers only the top rows with alpha 0.25 green) so straight alpha, premultiplication
/// and every channel are exercised.
fn plan() -> RenderPlan<NoVector> {
    let r = |x, y, w, h| Rect { x, y, width: w, height: h };
    let layer = |input: u32, opacity: f32| Layer { input: PlanNodeId(input), mode: StudioBlendMode::Normal, opacity, fill_opacity: 1.0, mask: None };
    let nodes = vec![
        PlanNode::Solid { rect: r(5, 3, 30, 24), rgba: [0.9, 0.05, 0.1, 1.0] },
        PlanNode::Solid { rect: r(15, 8, 20, 12), rgba: [0.1, 0.2, 0.95, 0.5] },
        PlanNode::Solid { rect: r(30, 3, 15, 6), rgba: [0.2, 0.8, 0.3, 0.25] },
        PlanNode::Group(Group { isolated: true, layers: vec![layer(0, 1.0), layer(1, 0.7), layer(2, 1.0)] }),
    ];
    RenderPlan {
        revision: 3,
        target: OutputTarget {
            extent: EXTENT,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight },
        },
        root: PlanNodeId(3),
        nodes,
    }
}

fn request() -> ExecuteRequest {
    ExecuteRequest { expected_revision: 3, min_exactness: Exactness::Fitted, deadline: None, max_working_bytes: 64 << 20 }
}

struct NoSources;
impl PlanSources for NoSources {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

/// Straight linear RGBA f32 of the extent, from the read-back path.
struct Image(Vec<[f32; 4]>);
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y - EXTENT.y) as usize * EXTENT.width as usize + y * EXTENT.width as usize + (r.x - EXTENT.x) as usize + x;
                self.0[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}

/// The CPU statement of the module's display encoding (egui: gamma-space premultiplied 8-bit).
fn display(px: [f32; 4]) -> [u8; 4] {
    let unit = |v: f32| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    let enc = |v: f32| {
        let c = unit(v);
        if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
    };
    let byte = |v: f32| (unit(v) * 255.0 + 0.5).floor();
    let a8 = byte(px[3]);
    let pre = |c: f32| ((byte(enc(c)) * a8 / 255.0) + 0.5).floor() as u8;
    [pre(px[0]), pre(px[1]), pre(px[2]), a8 as u8]
}

fn target_texture(device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test target"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn read_texture(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let (w, h) = (texture.width(), texture.height());
    let row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test readback"),
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit(Some(encoder.finish()));
    buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(30)) }).expect("poll");
    let view = buffer.slice(..).get_mapped_range();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let start = (y * row) as usize;
        out.extend_from_slice(&view[start..start + (w * 4) as usize]);
    }
    out
}

#[test]
fn shared_device_renders_into_the_callers_texture_without_readback() {
    let Some((device, queue, info)) = host_device("shared-device texture parity") else { return };
    let shared = SharedDevice::new(&device, &queue, info, DeviceOptions { chunk_side: 16, ..DeviceOptions::default() })
        .unwrap_or_else(|e| panic!("shared device refused the host device: {e} ({e:?})"));
    // Larger than the extent: the plan writes the top-left EXTENT and leaves the rest untouched.
    let usage = wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC;
    let texture = target_texture(&device, EXTENT.width + 3, EXTENT.height + 2, TARGET_FORMAT, usage);
    let cancel = CancellationToken::default();
    let (receipt, stats) = shared
        .render_to_texture(&plan(), None, &request(), &NoSources, &cancel, &texture)
        .unwrap_or_else(|e| panic!("texture render refused: {e} ({e:?})"));
    assert_eq!(stats.bytes_read_back, 0, "the texture path reads nothing back");
    assert!(receipt.chunks > 1, "chunk_side 16 over 40x24 must take several chunks, got {}", receipt.chunks);

    // Reference: the same backend on the same device through the read-back TileSink path.
    let mut image = Image(vec![[f32::NAN; 4]; (EXTENT.width * EXTENT.height) as usize]);
    let (reference_receipt, reference_stats) = shared.backend().execute_detailed(&plan(), &request(), &NoSources, &cancel, &mut image).unwrap();
    assert!(reference_stats.bytes_read_back > 0);
    assert_eq!(reference_receipt.plan_digest, receipt.plan_digest);
    assert_eq!(reference_receipt.chunks, receipt.chunks);

    let bytes = read_texture(&device, &queue, &texture);
    let tw = texture.width() as usize;
    let (mut max_diff, mut transparent, mut partial) = (0u8, 0usize, 0usize);
    for y in 0..EXTENT.height as usize {
        for x in 0..EXTENT.width as usize {
            let px = image.0[y * EXTENT.width as usize + x];
            assert!(px.iter().all(|v| v.is_finite()), "reference pixel ({x},{y}) not written");
            let want = display(px);
            let at = (y * tw + x) * 4;
            let got = &bytes[at..at + 4];
            for c in 0..4 {
                max_diff = max_diff.max(got[c].abs_diff(want[c]));
            }
            transparent += usize::from(want[3] == 0);
            partial += usize::from(want[3] > 0 && want[3] < 255);
        }
    }
    println!("SHARED max 8-bit diff {max_diff} (transparent {transparent}, partial alpha {partial}, chunks {})", receipt.chunks);
    assert!(transparent > 0 && partial > 0, "the plan must cover transparent and partially transparent pixels");
    assert!(max_diff <= 1, "texture vs CPU display encoding of the read-back path differs by {max_diff}");
    // Outside the extent the texture keeps its initial (zero) content.
    assert!(bytes[(tw * 4 - 4)..tw * 4].iter().all(|b| *b == 0));
}

#[test]
fn shared_target_refusals_happen_before_device_work() {
    let Some((device, queue, info)) = host_device("shared-device target refusals") else { return };
    let shared = SharedDevice::new(&device, &queue, info, DeviceOptions::default()).unwrap();
    let cancel = CancellationToken::default();
    let rejected = GpuError::Backend(BackendError::Sink(SinkError::Rejected));
    let good = wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING;
    for (w, h, format, usage) in [
        (EXTENT.width - 1, EXTENT.height, TARGET_FORMAT, good),
        (EXTENT.width, EXTENT.height - 1, TARGET_FORMAT, good),
        (EXTENT.width, EXTENT.height, wgpu::TextureFormat::Rgba8UnormSrgb, wgpu::TextureUsages::TEXTURE_BINDING),
        (EXTENT.width, EXTENT.height, TARGET_FORMAT, wgpu::TextureUsages::TEXTURE_BINDING),
    ] {
        let texture = target_texture(&device, w, h, format, usage);
        assert_eq!(shared.render_to_texture(&plan(), None, &request(), &NoSources, &cancel, &texture).unwrap_err(), rejected, "{w}x{h} {format:?} {usage:?}");
    }
    assert_eq!(shared.backend().resident_bytes(), 0, "a refused target charged no residency");
    assert!(shared.backend().last_stats().is_none(), "a refused target executed nothing");
    // A stale revision is the shared admission's typed refusal on this path too.
    let texture = target_texture(&device, EXTENT.width, EXTENT.height, TARGET_FORMAT, good);
    let stale = ExecuteRequest { expected_revision: 4, ..request() };
    let err = shared.render_to_texture(&plan(), None, &stale, &NoSources, &cancel, &texture).unwrap_err();
    assert!(matches!(err, GpuError::Backend(BackendError::StaleRevision { .. })), "{err:?}");
}
