//! Offscreen proof path: Vulkan instance with no surface extension, a pane image, readback, PNG. No window, no HWND.
//! Agents screenshot through this. The `gui` scene runs its scripted steps on the live frame first, painting and
//! uploading (or, with `--chrome vulkan`, drawing with the UI pass) after each step, then reads back the final frame.
//! In Vulkan mode it also paints the CPU reference of that frame through the same compose call and writes `parity`
//! into the receipt's `ui_pass`; with `--out X.png` the reference and a difference image go beside it (X.cpu.png,
//! X.diff.png), and `--ui-trace` writes the pass's per-primitive trace.
use crate::capture::{Json, device_json, layers_json, sha256_hex, validation_json, write_png, write_receipt};
use crate::diag::HostRun;
use crate::error::HostError;
use crate::scene::{CHROME, Content, DocumentFacts, GuiArgs, Scene, content};
#[cfg(feature = "document")]
use crate::scene::ChromeMode;
use hsk_studio_vulkan::{DeviceInfo, GpuClientPort, LayerReport, Owner, OwnerConfig, PANE_FORMAT};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OffscreenArgs {
    pub width: u32,
    pub height: u32,
    pub scene: Scene,
    pub out: Option<PathBuf>,
    pub receipt: Option<PathBuf>,
    pub allow_overlays: bool,
    /// Scene `gui` only.
    pub gui: GuiArgs,
}

#[derive(Clone, Debug)]
pub struct OffscreenResult {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
    pub frame_sha256: String,
    /// The readback equals the bytes the scene asked for.
    pub exact: bool,
    pub device: DeviceInfo,
    pub layers: LayerReport,
    pub validation: &'static str,
    pub document: Option<DocumentFacts>,
    pub receipt: Json,
}

/// Renders `args.scene` into a pane and reads it back, as the caller `host` names. Sends exactly one terminal frame
/// through `host` on every exit path, a panic included (F-R60-22).
pub fn run(args: &OffscreenArgs, host: &mut HostRun<'_>) -> Result<OffscreenResult, HostError> {
    crate::diag::observed(host, || run_scene(args))
}

fn run_scene(args: &OffscreenArgs) -> Result<OffscreenResult, HostError> {
    if let Scene::Gui(path) = &args.scene {
        return gui_run(args, path);
    }
    let (content, document) = content(&args.scene, args.width, args.height)?;
    let mut owner = Owner::open(OwnerConfig { surface: false, allow_overlays: args.allow_overlays, allow_cpu_device: false })?;
    let (width, height) = content.extent();
    let pane = owner.create_pane(width, height)?;
    match &content {
        Content::Clear { rgba, .. } => owner.clear_pane(pane, *rgba)?,
        Content::Pixels { rgba8, .. } => owner.upload_pane(pane, rgba8)?,
    }
    let rgba8 = owner.read_pane(pane)?;
    let exact = rgba8 == content.expected()?;
    finish(args, owner, (width, height), rgba8, exact, document, Json::Null, (CHROME, Json::Null))
}

#[cfg(feature = "document")]
fn gui_run(args: &OffscreenArgs, path: &std::path::Path) -> Result<OffscreenResult, HostError> {
    use crate::gui::{CpuChrome, GuiFrame, PortPane, VulkanChrome, drive, drive_chrome, parity};
    if crate::script::frame_only(&args.gui.steps) {
        return Err(HostError::Usage("script_frame_only"));
    }
    let mut frame = GuiFrame::open(path, args.width, args.height, Some(args.gui.scale_milli.unwrap_or(1000)))?;
    frame.set_chrome(args.gui.chrome);
    let mut owner = Owner::open(OwnerConfig { surface: false, allow_overlays: args.allow_overlays, allow_cpu_device: false })?;
    frame.set_device(crate::capture::device_readout(&owner));
    let (width, height) = frame.extent();
    let pane = owner.create_pane(width, height)?;
    let (rgba8, exact, ui) = match args.gui.chrome {
        ChromeMode::Cpu => {
            drive(&mut frame, &mut PortPane { port: &mut owner, pane }, &args.gui.steps)?;
            let rgba8 = owner.read_pane(pane)?;
            let exact = frame.rgba() == Some(&rgba8[..]);
            (rgba8, exact, Json::Null)
        }
        ChromeMode::Vulkan => {
            let mut pass = hsk_studio_vulkan::UiPass::new(&owner)?;
            pass.set_trace(true);
            drive_chrome(&mut frame, &mut VulkanChrome { pass: &mut pass, owner: &mut owner, pane }, &args.gui.steps)?;
            let rgba8 = owner.read_pane(pane)?;
            // The CPU reference of the same frame, from the same compose call.
            let mut cpu = CpuChrome::default();
            frame.compose(&mut cpu)?;
            let want = cpu.canvas.map(|c| c.rgba().to_vec()).unwrap_or_default();
            let p = parity(&want, &rgba8, width, pass.trace());
            if let Some(out) = &args.out
                && want.len() == rgba8.len()
            {
                write_png(&out.with_extension("cpu.png"), width, height, &want)?;
                let diff: Vec<u8> = want
                    .chunks_exact(4)
                    .zip(rgba8.chunks_exact(4))
                    .flat_map(|(a, b)| {
                        let d = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
                        if d == 0 { [0, 0, 0, 255] } else { [d.saturating_mul(64).max(128), 0, 0, 255] }
                    })
                    .collect();
                write_png(&out.with_extension("diff.png"), width, height, &diff)?;
            }
            if let Some(t) = &args.gui.ui_trace {
                crate::capture::write_text(t, &crate::capture::ui_trace_json(pass.trace()).render())?;
            }
            let ui = crate::capture::ui_pass_json(&pass.stats(), &owner.device_info().features, Some(p));
            drop(pass);
            (rgba8, p.0 == 0, ui)
        }
    };
    if let Some(tree) = &args.gui.tree {
        crate::capture::write_text(tree, &frame.tree_file())?;
    }
    finish(args, owner, (width, height), rgba8, exact, Some(frame.facts()), frame.receipt(), (args.gui.chrome.code(), ui))
}

#[cfg(not(feature = "document"))]
fn gui_run(_: &OffscreenArgs, _: &std::path::Path) -> Result<OffscreenResult, HostError> {
    Err(HostError::Document("feature_off"))
}

#[allow(clippy::too_many_arguments)]
fn finish(
    args: &OffscreenArgs,
    owner: Owner,
    (width, height): (u32, u32),
    rgba8: Vec<u8>,
    exact: bool,
    document: Option<DocumentFacts>,
    gui: Json,
    (chrome, ui_pass): (&'static str, Json),
) -> Result<OffscreenResult, HostError> {
    let frame_sha256 = sha256_hex(&rgba8);
    let (issuer, device, layers, validation) = (owner.issuer().code(), device_json(&owner), layers_json(&owner), owner.validation());
    let (device_info, layer_report) = (owner.device_info().clone(), owner.layers().clone());
    // The owner goes first, so the validation counts include its destruction.
    drop(owner);
    let receipt = Json::obj(vec![
        ("schema", Json::str("hsk.studio.native_host.receipt@1")),
        ("host", Json::str("asset-local host")),
        ("mode", Json::str("offscreen")),
        ("source", Json::str("offscreen")),
        ("backend", Json::str("vulkan")),
        ("issuer", Json::str(issuer)),
        ("device", device),
        ("validation", Json::str(validation)),
        ("validation_report", validation_json()),
        ("layers", layers),
        ("hwnd_created", Json::Bool(false)),
        ("scene", Json::str(args.scene.code())),
        ("extent", Json::Arr(vec![Json::Num(u64::from(width)), Json::Num(u64::from(height))])),
        ("format", Json::str(PANE_FORMAT)),
        ("frame_sha256", Json::str(&frame_sha256)),
        ("exact", Json::Bool(exact)),
        ("chrome", Json::str(chrome)),
        ("ui_pass", ui_pass),
        ("document", document_json(document.as_ref())),
        ("gui", gui),
    ]);
    if let Some(out) = &args.out {
        write_png(out, width, height, &rgba8)?;
    }
    if let Some(path) = &args.receipt {
        write_receipt(path, &receipt)?;
    }
    Ok(OffscreenResult {
        width,
        height,
        rgba8,
        frame_sha256,
        exact,
        device: device_info,
        layers: layer_report,
        validation,
        document,
        receipt,
    })
}

pub(crate) fn document_json(facts: Option<&DocumentFacts>) -> Json {
    match facts {
        None => Json::Null,
        Some(f) => Json::obj(vec![
            ("file_name", Json::str(&f.file_name)),
            ("revision", Json::Num(f.revision)),
            ("viewer_backend", Json::str(&f.backend)),
            ("layers", Json::Num(f.layers)),
        ]),
    }
}
