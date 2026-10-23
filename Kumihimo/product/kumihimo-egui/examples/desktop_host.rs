//! Explicit visible opt-in. Without --interactive this example refuses to open a window.
#[cfg(feature = "native")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut interactive = false;
    let mut frames = 3usize;
    let mut tabs = 3usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--interactive" => interactive = true,
            "--frames" => frames = args.next().ok_or("missing frame count")?.parse()?,
            "--tabs" => tabs = args.next().ok_or("missing tab count")?.parse()?,
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if !interactive {
        return Err(
            "showing requires explicit --interactive; use native_probe for hidden automation"
                .into(),
        );
    }
    let demo = kumihimo_egui::demo::with_counts(frames, tabs)?;
    let options = kumihimo_egui::native::NativeOptions {
        interactive: true,
        probe: false,
        ..Default::default()
    };
    let outcome = kumihimo_egui::native::run(demo.controller, demo.host, demo.root, options)?;
    println!("{}", outcome.manifest.display());
    Ok(())
}
#[cfg(not(feature = "native"))]
fn main() {
    eprintln!("desktop_host requires the native feature and explicit --interactive");
}
