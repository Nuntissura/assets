//! Explicit stdin JSON session; never launches, enumerates, closes, or activates applications.
#[cfg(windows)]
mod native {
    use kumihimo_core::*;
    use kumihimo_external_apps::{windows::WindowsBackend, *};
    use serde::Deserialize;
    use serde_json::{json, Value};
    use std::io::{BufRead, Read, Write};

    #[derive(Deserialize)]
    #[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
    enum Input {
        Inspect,
        Tile { x: i32, y: i32, width: i32, height: i32 },
        Restore,
        Recovery,
        Recover { record: RecoveryRecord },
        Exit,
    }
    fn apply(core: &mut Core, command: Command) -> Result<Change, kumihimo_core::Error> {
        core.execute(Envelope { identity: core.snapshot().identity.clone(), actor: "selected-window-example".into(), operation: core.ledger_status().high_watermark.checked_next().unwrap(), expected_generation: core.snapshot().generation, command }).outcome.result
    }
    fn projection(width: f64, height: f64) -> Result<(GeometryOutput, FrameId), kumihimo_core::Error> {
        let mut core = Core::new(Identity::new("selected-window-example", "explicit-session")?, Limits::default(), 8)?;
        let Change::Created(EntityId::Workspace(workspace)) = apply(&mut core, Command::CreateWorkspace { owner: None })? else { unreachable!() };
        let Change::Created(EntityId::Frame(frame)) = apply(&mut core, Command::CreateFrame { workspace, index: 0 })? else { unreachable!() };
        apply(&mut core, Command::SetGeometryContext { workspace, context: GeometryContext::new(Rect::new(0.0, 0.0, width, height)?) })?;
        Ok((core.snapshot().geometry(workspace)?, frame))
    }
    fn emit(value: Value) -> Result<(), Box<dyn std::error::Error>> {
        let mut out = std::io::stdout().lock(); serde_json::to_writer(&mut out, &value)?; writeln!(out)?; out.flush()?; Ok(())
    }
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        if args.len() < 6 || args.len() % 2 != 0 { return Err("Use --pid PID --hwnd HWND --semantic REF [--created FILETIME]; stdin accepts inspect/tile/restore/recovery/recover/exit JSON".into()); }
        let mut pid = None; let mut hwnd = None; let mut semantic = None; let mut created = None;
        for pair in args.chunks_exact(2) {
            match pair[0].as_str() {
                "--pid" if pid.is_none() => pid = Some(pair[1].parse::<u32>()?),
                "--hwnd" if hwnd.is_none() => hwnd = Some(if let Some(hex) = pair[1].strip_prefix("0x") { u64::from_str_radix(hex, 16)? } else { pair[1].parse()? }),
                "--semantic" if semantic.is_none() => semantic = Some(pair[1].clone()),
                "--created" if created.is_none() => created = Some(pair[1].parse::<u64>()?),
                _ => return Err("Unknown or repeated selection argument".into()),
            }
        }
        let mut host = ExternalApps::new(WindowsBackend);
        let mut selected = host.bind(WindowTarget { hwnd: hwnd.ok_or("Missing --hwnd")?, pid: pid.ok_or("Missing --pid")?, expected_creation_time: created }, &semantic.ok_or("Missing --semantic")?)?;
        let identity = selected.identity();
        emit(json!({"event":"bound", "identity":{"hwnd":identity.hwnd.to_string(),"pid":identity.pid,"creation_time":identity.creation_time.to_string()}, "recovery":RecoveryRecord::from_selection(&selected)}))?;
        let stdin = std::io::stdin(); let mut reader = stdin.lock();
        loop {
            let mut line = String::new();
            if (&mut reader).take(65537).read_line(&mut line)? == 0 {
                if selected.pending().is_some() {
                    emit(json!({"event":"unresolved_dispatch","recovery":RecoveryRecord::from_selection(&selected)}))?;
                    return Err("Input closed with an unresolved OS request; save the emitted recovery record and explicitly rebind/reconcile".into());
                }
                break;
            }
            if line.len() > 65536 { emit(json!({"event":"rejected","error":"command exceeds 65536 bytes"}))?; break; }
            let input = match serde_json::from_str::<Input>(&line) { Ok(input) => input, Err(error) => { emit(json!({"event":"rejected","error":error.to_string()}))?; continue; } };
            let result: Result<Value, Box<dyn std::error::Error>> = (|| {
                Ok(match input {
                    Input::Inspect => { let (placement, capabilities) = host.inspect(&selected)?; json!({"event":"inspect","placement":placement,"capabilities":capabilities}) },
                    Input::Tile { x, y, width, height } => {
                        PixelBounds { x, y, width, height }.validate()?;
                        let (_, capabilities) = host.inspect(&selected)?;
                        let scale = capabilities.dpi as f64 / 96.0;
                        let (geometry, frame) = projection(width as f64 / scale, height as f64 / scale)?;
                        json!({"event":"tile","receipt":host.tile(&selected, &geometry, frame, DesktopMapping { origin_x:x, origin_y:y, pixels_per_dip:scale }, ObserveBudget::default())})
                    },
                    Input::Restore => json!({"event":"restore","receipt":host.restore(&selected, ObserveBudget::default())}),
                    Input::Recovery => json!({"event":"recovery","record":RecoveryRecord::from_selection(&selected)}),
                    Input::Recover { record } => { record.apply_to(&mut selected)?; json!({"event":"recover","receipt":host.restore(&selected, ObserveBudget::default())}) },
                    Input::Exit => {
                        if selected.pending().is_some() { return Err(Diagnostic::new("pending_exit", "OS request remains unsettled; restore/reconcile before successful session exit").into()); }
                        json!({"event":"exit"})
                    },
                })
            })();
            match result { Ok(value) => { let exiting = value["event"] == "exit"; emit(value)?; if exiting { break; } }, Err(error) => emit(json!({"event":"rejected","error":error.to_string()}))? }
        }
        Ok(())
    }
}
fn main() {
    #[cfg(windows)]
    if let Err(error) = native::run() { println!("{}", serde_json::json!({"event":"failed","error":error.to_string()})); std::process::exit(1); }
    #[cfg(not(windows))]
    { println!("{}", serde_json::json!({"event":"unsupported","error":kumihimo_external_apps::unsupported_platform()})); std::process::exit(1); }
}
