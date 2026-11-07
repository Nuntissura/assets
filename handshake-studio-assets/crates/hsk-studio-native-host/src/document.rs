//! Feature `document`: a real document through harness-view's public `Viewer::open` and `render_if_dirty` on render-cpu
//! (`Prefer::Cpu`: `render_if_dirty` creates a GPU backend only for `Prefer::Gpu`, so the viewer opens no device and
//! the process keeps one device, the Vulkan owner's). The viewer runs on the asset-local test host's fixed context
//! (`asset-local-test-host`); this host shows the document and mutates nothing.
use crate::error::HostError;
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_vulkan::rgba_len;
use std::path::Path;

pub struct DocFrame {
    pub width: u32,
    pub height: u32,
    /// sRGB-encoded, straight alpha (harness-view `Frame::rgba8`).
    pub rgba8: Vec<u8>,
    pub revision: u64,
    pub backend: &'static str,
    pub layers: u64,
}

pub fn render(path: &Path) -> Result<DocFrame, HostError> {
    let mut viewer = Viewer::new(Prefer::Cpu);
    viewer.open(path).map_err(|_| HostError::Document("open"))?;
    // A superseded preview re-marks the viewer dirty; three passes bound that.
    for _ in 0..3 {
        viewer.render_if_dirty();
        if viewer.frame().is_some() || viewer.render_error().is_some() {
            break;
        }
    }
    if let Some(fail) = viewer.render_error() {
        // The viewer's stable code only; its detail text stays out of receipts.
        return Err(HostError::Document(fail.code));
    }
    let frame = viewer.frame().ok_or(HostError::Document("render"))?;
    let len = rgba_len(frame.width, frame.height)?;
    if frame.rgba8.len() != len {
        return Err(HostError::Document("frame"));
    }
    let layers = viewer.state_json().get("layers").and_then(|l| l.as_array()).map_or(0, |a| a.len() as u64);
    Ok(DocFrame {
        width: frame.width,
        height: frame.height,
        rgba8: frame.rgba8.clone(),
        revision: frame.revision,
        backend: frame.backend,
        layers,
    })
}
