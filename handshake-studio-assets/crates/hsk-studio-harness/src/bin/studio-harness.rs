//! Studio harness desktop window (feature `desktop`; Operator decision D11: eframe 0.33.3 with
//! its wgpu backend). Usage: `studio-harness [FILE]`: FILE (PSD/PSB, TIFF, camera raw, PDF or a
//! native `.handshake` package) is opened at start; the path field in the window opens more.
//! Optional `--cpu` renders with render-cpu only. All application state lives in
//! `hsk_studio_harness::view::Viewer`; this file only hosts it in a window. Launching it is a
//! manual Operator step (`RUN-UI.md`); tests never open this window.
use hsk_studio_harness::view::{Viewer, display::WgpuDisplay, render::Prefer};

struct App(Viewer);

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.0.ui(ctx);
    }
}

fn main() -> eframe::Result {
    let mut prefer = Prefer::Gpu;
    let mut file = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--cpu" => prefer = Prefer::Cpu,
            "-h" | "--help" => {
                println!("studio-harness [--cpu] [FILE]   desktop viewer (PSD/PSB, TIFF, camera raw, PDF, .handshake)");
                return Ok(());
            }
            _ => file = Some(std::path::PathBuf::from(arg)),
        }
    }
    let mut viewer = Viewer::new(prefer);
    if let Some(path) = file {
        viewer.path_input = path.to_string_lossy().into_owned();
        // A failure is shown in the window's status panel.
        let _ = viewer.open(&path);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("Studio harness").with_inner_size([1400.0, 900.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "hsk-studio-harness",
        options,
        Box::new(move |cc| {
            // Shared device (lane SMB-RENDER-GPU-SHARED-DEVICE): GPU frames render on eframe's own
            // wgpu device straight into the texture egui draws; the read-back path is the fallback.
            if prefer == Prefer::Gpu {
                match cc.wgpu_render_state.as_ref().map(WgpuDisplay::new) {
                    Some(Ok(display)) => {
                        viewer.status_line(format!("render-gpu shares the window's device ({})", display.adapter()));
                        viewer.set_display(Box::new(display));
                    }
                    Some(Err(e)) => viewer.status_line(format!("{e}; read-back path used")),
                    None => viewer.status_line("no wgpu render state; read-back path used".into()),
                }
            }
            Ok(Box::new(App(viewer)))
        }),
    )
}
