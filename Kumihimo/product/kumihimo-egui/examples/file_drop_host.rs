//! Opt-in native host: small UTF-8 .txt/.md imports; source files remain untouched.
#[cfg(all(feature = "native-drops", windows))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut options = kumihimo_egui::native::NativeOptions { probe: false, timeout: std::time::Duration::from_secs(120), ..Default::default() };
    let mut observe_host = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--interactive" => options.interactive = true,
            "--observe-host" => observe_host = true,
            "--output" => options.output = args.next().ok_or("missing output path")?.into(),
            "--timeout-seconds" => {
                let seconds = args.next().ok_or("missing timeout seconds")?.parse::<u64>()?;
                if !(1..=120).contains(&seconds) { return Err("timeout seconds must be 1..=120".into()); }
                options.timeout = std::time::Duration::from_secs(seconds);
            },
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if !options.interactive {
        return Err("file_drop_host opens a controlled window only with --interactive".into());
    }
    let mut demo = kumihimo_egui::demo::file_drop_sample()?;
    demo.host.drop_enabled = true;
    demo.host.drop_observe = observe_host;
    demo.host.drop_status = "Import UTF-8 .txt/.md into a text pane · up to 8 files / 8 KiB · source retained".into();
    let outcome = kumihimo_egui::native::run(demo.controller, demo.host, demo.root, options)?;
    println!("{}", outcome.manifest.display());
    Ok(())
}
#[cfg(not(all(feature = "native-drops", windows)))]
fn main() { eprintln!("file_drop_host requires native,drops on Windows and --interactive"); }
