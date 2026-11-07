//! Offscreen proof path: Vulkan instance with no surface extension, a pane image, readback, PNG. No window, no HWND.
//! Agents screenshot through this. The `gui` scene runs its scripted steps on the live frame first, painting and
//! uploading after each step, then reads back the final frame.
use crate::capture::{Json, device_json, layers_json, sha256_hex, write_png, write_receipt};
use crate::error::HostError;
use crate::scene::{CHROME, Content, DocumentFacts, GuiArgs, Scene, content};
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

/// Renders `args.scene` into a pane and reads it back.
pub fn run(args: &OffscreenArgs) -> Result<OffscreenResult, HostError> {
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
    finish(args, owner, (width, height), rgba8, exact, document, Json::Null)
}

#[cfg(feature = "document")]
fn gui_run(args: &OffscreenArgs, path: &std::path::Path) -> Result<OffscreenResult, HostError> {
    use crate::gui::{GuiFrame, PortPane, drive};
    if crate::script::frame_only(&args.gui.steps) {
        return Err(HostError::Usage("script_frame_only"));
    }
    let mut frame = GuiFrame::open(path, args.width, args.height, Some(args.gui.scale_milli.unwrap_or(1000)))?;
    let mut owner = Owner::open(OwnerConfig { surface: false, allow_overlays: args.allow_overlays, allow_cpu_device: false })?;
    frame.set_device(crate::capture::device_readout(&owner));
    let (width, height) = frame.extent();
    let pane = owner.create_pane(width, height)?;
    drive(&mut frame, &mut PortPane { port: &mut owner, pane }, &args.gui.steps)?;
    let rgba8 = owner.read_pane(pane)?;
    let exact = frame.rgba() == Some(&rgba8[..]);
    if let Some(tree) = &args.gui.tree {
        crate::capture::write_text(tree, &frame.tree_file())?;
    }
    finish(args, owner, (width, height), rgba8, exact, Some(frame.facts()), frame.receipt())
}

#[cfg(not(feature = "document"))]
fn gui_run(_: &OffscreenArgs, _: &std::path::Path) -> Result<OffscreenResult, HostError> {
    Err(HostError::Document("feature_off"))
}

fn finish(
    args: &OffscreenArgs,
    owner: Owner,
    (width, height): (u32, u32),
    rgba8: Vec<u8>,
    exact: bool,
    document: Option<DocumentFacts>,
    gui: Json,
) -> Result<OffscreenResult, HostError> {
    let frame_sha256 = sha256_hex(&rgba8);
    let receipt = Json::obj(vec![
        ("schema", Json::str("hsk.studio.native_host.receipt@1")),
        ("host", Json::str("asset-local host")),
        ("mode", Json::str("offscreen")),
        ("source", Json::str("offscreen")),
        ("backend", Json::str("vulkan")),
        ("issuer", Json::str(owner.issuer().code())),
        ("device", device_json(&owner)),
        ("validation", Json::str(owner.validation())),
        ("layers", layers_json(&owner)),
        ("hwnd_created", Json::Bool(false)),
        ("scene", Json::str(args.scene.code())),
        ("extent", Json::Arr(vec![Json::Num(u64::from(width)), Json::Num(u64::from(height))])),
        ("format", Json::str(PANE_FORMAT)),
        ("frame_sha256", Json::str(&frame_sha256)),
        ("exact", Json::Bool(exact)),
        ("chrome", Json::str(CHROME)),
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
        device: owner.device_info().clone(),
        layers: owner.layers().clone(),
        validation: owner.validation(),
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
