//! Studio harness desktop window (feature `desktop`; Operator decision D11: eframe 0.33.3 with
//! its wgpu backend). A CLI command word (`studio-harness help`) is answered by the CLI with no
//! window, and an argument that is neither a command nor an existing file prints usage and exits
//! with no window (`cli::window_args`). Usage: `studio-harness [--cpu] [--recent LIST] [FILE]`: FILE (PSD/PSB, TIFF,
//! camera raw, PNG/JPEG, PDF, a native `.handshake` package; with feature `media` an audio file) is
//! opened at start; File > Open... (Ctrl+O), File > Open Recent and the path field open more.
//! `--cpu` renders with render-cpu only. `--recent LIST` keeps the recent-files list in the file
//! LIST (read at start when it exists, rewritten atomically whenever the list changes); without it
//! the list lives only for the session (the harness has no settings path of its own). All
//! application state lives in `hsk_studio_harness::view::Viewer`; this file only hosts it in a
//! window. Launching it is a manual Operator step (`RUN-UI.md`); tests never open this window.
use hsk_studio_harness::cli::{self, WindowArgs};
use hsk_studio_harness::view::{Viewer, display::WgpuDisplay, export::write_atomic, render::Prefer};
use std::path::PathBuf;

struct App {
    viewer: Viewer,
    /// Recent-files list file (`--recent`) and the list generation last written.
    recent: Option<(PathBuf, u64)>,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.viewer.ui(ctx);
        if let Some((path, written)) = &mut self.recent {
            let generation = self.viewer.recent_generation();
            if generation != *written {
                *written = generation;
                if let Err(code) = write_atomic(path, &self.viewer.recent_bytes()) {
                    self.viewer.status_line(format!("recent list {}: {code}", path.display()));
                }
            }
        }
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (prefer, recent, file) = match cli::window_args(&args, &|p| p.is_file()) {
        WindowArgs::Cli => std::process::exit(cli::run_streams(&args, &mut std::io::stdin().lock(), &mut std::io::stdout().lock())),
        WindowArgs::Usage(usage) => {
            print!("{}", usage.stdout);
            std::process::exit(usage.exit);
        }
        WindowArgs::Window { cpu, recent, file } => (if cpu { Prefer::Cpu } else { Prefer::Gpu }, recent, file),
    };
    let mut viewer = Viewer::new(prefer);
    let recent = recent.map(|path| {
        // A missing list starts empty; a list that does not decode is reported, starts empty and is
        // rewritten at the next change.
        if let Ok(bytes) = std::fs::read(&path)
            && let Err(code) = viewer.load_recent(&bytes)
        {
            viewer.status_line(format!("recent list {} not loaded: {code}", path.display()));
        }
        let generation = viewer.recent_generation();
        (path, generation)
    });
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
            Ok(Box::new(App { viewer, recent }))
        }),
    )
}
