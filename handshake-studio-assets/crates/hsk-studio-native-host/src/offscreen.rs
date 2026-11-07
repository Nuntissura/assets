//! Offscreen proof path: Vulkan instance with no surface extension, a pane image, readback, PNG. No window, no HWND.
//! Agents screenshot through this.
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
