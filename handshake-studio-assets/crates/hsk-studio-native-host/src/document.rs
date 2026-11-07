//! Feature `document`: a real document through harness-view's public `Viewer::open` and `render_if_dirty` on render-cpu
//! (`Prefer::Cpu`: `render_if_dirty` creates a GPU backend only for `Prefer::Gpu`, so the viewer opens no device and
//! the process keeps one device, the Vulkan owner's). The viewer runs on the asset-local test host's fixed context
//! (`asset-local-test-host`); this path shows the document and mutates nothing (the `gui` scene edits through
//! [`crate::gui`]).
use crate::error::HostError;
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_vulkan::rgba_len;
use std::path::Path;
use std::sync::Mutex;

pub struct DocFrame {
    pub width: u32,
    pub height: u32,
    /// sRGB-encoded, straight alpha (harness-view `Frame::rgba8`).
    pub rgba8: Vec<u8>,
    pub revision: u64,
    pub backend: &'static str,
    pub layers: u64,
}

/// A stable code: lowercase ASCII, digits, `_`, `.`, `-`, at most 64 bytes (never free text).
pub(crate) fn code_like(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
}

/// Viewer codes as `&'static str` for [`HostError::Document`]: at most 64 distinct codes per process, then `open`.
fn intern(code: &str) -> &'static str {
    static CODES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let Ok(mut codes) = CODES.lock() else { return "open" };
    if let Some(c) = codes.iter().find(|c| **c == code) {
        return c;
    }
    if codes.len() >= 64 {
        return "open";
    }
    let c: &'static str = Box::leak(code.to_owned().into_boxed_str());
    codes.push(c);
    c
}

/// Opens `path` in a fresh Viewer. A refusal keeps the viewer's own code: the run's last op error, else the code at
/// the end of the viewer's message (`tiles <path>: <code>`), else `open`; a missing file is `not_found`.
pub(crate) fn open_viewer(path: &Path) -> Result<Viewer, HostError> {
    if std::fs::metadata(path).is_err() {
        return Err(HostError::Document("not_found"));
    }
    let mut viewer = Viewer::new(Prefer::Cpu);
    if let Err(message) = viewer.open(path) {
        let op = viewer.run().ops().last().and_then(|o| o["error"].as_str()).filter(|c| code_like(c)).map(intern);
        let tail = message.rsplit(": ").next().filter(|c| code_like(c)).map(intern);
        return Err(HostError::Document(op.or(tail).unwrap_or("open")));
    }
    Ok(viewer)
}

pub fn render(path: &Path) -> Result<DocFrame, HostError> {
    let mut viewer = open_viewer(path)?;
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
    // The count only: `layers()` once, not the whole `state_json` projection.
    let layers = viewer.layers().len() as u64;
    Ok(DocFrame {
        width: frame.width,
        height: frame.height,
        rgba8: frame.rgba8.clone(),
        revision: frame.revision,
        backend: frame.backend,
        layers,
    })
}
