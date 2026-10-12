//! Bounded hidden native observations. Never requests activation or global input.
#[cfg(feature = "native")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use kumihimo_core::{Change, Command, EntityId};
    use kumihimo_egui::{native::NativeOptions, DesktopHost};
    let mut options = NativeOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => options.output = args.next().ok_or("missing output")?.into(),
            "--observer-script" => {
                options.observer_script = Some(args.next().ok_or("missing observer script")?.into())
            }
            "--no-uia" => options.observer_script = None,
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if options.observer_script.is_none() && !std::env::args().any(|a| a == "--no-uia") {
        // Resolve from compile-time package location; moving/rebuilding the project refreshes it.
        options.observer_script =
            Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/observe-uia.ps1"));
    }
    let mut demo = kumihimo_egui::demo::new()?;
    options.probe_view = Some(demo.text_view);
    options.probe_frame = Some(demo.controller.snapshot().views[&demo.text_view].frame);
    let text_frame = demo.controller.snapshot().views[&demo.text_view].frame;
    options.observer_target = demo.controller.snapshot().frames[&text_frame]
        .views
        .get(1)
        .copied();
    let canvas_frame = demo.controller.snapshot().views[&demo.canvas_view].frame;
    let group = match demo
        .host
        .dispatch(
            &mut demo.controller,
            Command::CreateGroup {
                frame: canvas_frame,
                label: "Native group demonstration".into(),
                color: Some("blue".into()),
            },
        )?
        .outcome
        .result?
    {
        Change::Created(EntityId::Group(id)) => id,
        _ => return Err("group creation response".into()),
    };
    demo.host
        .dispatch(
            &mut demo.controller,
            Command::AssignGroup {
                view: demo.canvas_view,
                group: Some(group),
            },
        )?
        .outcome
        .result?;
    options.probe_group = Some(group);
    options.probe_partial_close = true;
    let result = kumihimo_egui::native::run(demo.controller, demo.host, demo.root, options)?;
    println!(
        "native_manifest={} captures={} returned={} uia_forwarded={}",
        result.manifest.display(),
        result.captures,
        result.returned,
        result.uia_actions
    );
    Ok(())
}
#[cfg(not(feature = "native"))]
fn main() {
    eprintln!("native_probe requires the native feature");
}
