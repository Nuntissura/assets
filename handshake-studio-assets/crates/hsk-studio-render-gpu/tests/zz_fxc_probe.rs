//! TEMPORARY probe (SMB-RENDER-FILTERS): which op of filter.wgsl makes FXC abort. Not committed.
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

fn noop_waker() -> Waker {
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    fn noop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}
fn block_on<F: Future>(f: F, device: Option<&wgpu::Device>) -> F::Output {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut f = pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        if let Some(d) = device {
            let _ = d.poll(wgpu::PollType::Poll);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn probe() {
    let src = include_str!("../src/filter.wgsl");
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::DX12, ..Default::default() });
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()), None).expect("adapter");
    let (device, _queue) = block_on(
        adapter.request_device(&wgpu::DeviceDescriptor { required_limits: wgpu::Limits::downlevel_defaults(), ..Default::default() }),
        None,
    )
    .expect("device");
    let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            entry(0, wgpu::BufferBindingType::Uniform),
            entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
            entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
            entry(3, wgpu::BufferBindingType::Storage { read_only: true }),
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&layout], push_constant_ranges: &[] });
    let a = src.find("        case 0u: {").unwrap();
    let z = src.find("        default: {").unwrap();
    // split the cases
    let marks: Vec<usize> = (0..5).map(|i| src.find(&format!("        case {i}u: {{")).unwrap()).chain([z]).collect();
    let mut variants: Vec<(String, String)> = vec![("all".into(), src.to_string())];
    for keep in 0..5 {
        let mut s = String::new();
        s.push_str(&src[..a]);
        for i in 0..5 {
            if i == keep {
                s.push_str(&src[marks[i]..marks[i + 1]]);
            } else {
                s.push_str(&format!("        case {i}u: {{ dst[i] = src[i]; }}\n"));
            }
        }
        s.push_str(&src[z..]);
        variants.push((format!("only case {keep}"), s));
    }
    for (name, wgsl) in variants {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(wgsl)) });
            let _p = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: Some(&pl),
                module: &module,
                entry_point: Some("filter_pass"),
                compilation_options: Default::default(),
                cache: None,
            });
        }));
        let err: Option<String> = r.err().map(|_| "PANIC (pipeline creation failed)".to_string());
        println!("PROBE {name}: {}", err.map_or("ok".to_string(), |e| format!("{e}").replace('\n', " | ")));
    }
}
