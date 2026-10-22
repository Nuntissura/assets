#[cfg(feature = "desktop")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };
    let mut args = std::env::args().skip(1);
    let mut plan = None;
    let mut input = None;
    let mut output = None;
    let mut now = None;
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("argument value missing")?;
        match arg.as_str() {
            "--plan-output" => plan = Some(PathBuf::from(value)),
            "--input-manifest" => input = Some(PathBuf::from(value)),
            "--output-root" => output = Some(PathBuf::from(value)),
            "--now-ms" => now = Some(value.parse::<u64>()?),
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    let now = now.unwrap_or(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?);
    match (plan,input,output) {(Some(path),None,None)=>kumihimo_render_bridge::provider::plan_batch(&path,now)?,(None,Some(input),Some(output))=>kumihimo_render_bridge::provider::compose_batch(&input,&output,now)?,_=>return Err("use --plan-output PATH or --input-manifest PATH --output-root PATH; optional --now-ms N".into())};
    Ok(())
}
#[cfg(not(feature = "desktop"))]
fn main() {
    eprintln!("compose requires optional desktop feature; external actual engine manifest and configured output root are required");
}
