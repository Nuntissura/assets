//! What a pane shows: one clear colour, a CPU-painted RGBA buffer, or a real document through harness-view's Viewer
//! on render-cpu. Chrome is the CPU painter; the Vulkan UI pass is pending (no shader in this lane).
use crate::error::HostError;
use hsk_studio_vulkan::rgba_len;
use std::path::PathBuf;

/// The receipt and window-title statement of what draws the chrome.
pub const CHROME: &str = "cpu painter, Vulkan UI pass pending";

/// Default clear colour (exact 8-bit values survive the UNORM round trip).
pub const DEFAULT_CLEAR: [u8; 4] = [0x2b, 0x6c, 0xb0, 0xff];
/// Window background around a pane smaller than the window.
pub const BACKGROUND: [u8; 4] = [0x1e, 0x1e, 0x1e, 0xff];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scene {
    Clear([u8; 4]),
    Painted,
    Document(PathBuf),
}

impl Scene {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Clear(_) => "clear",
            Self::Painted => "painted",
            Self::Document(_) => "document",
        }
    }
}

/// Pane content ready for the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Clear { width: u32, height: u32, rgba: [u8; 4] },
    Pixels { width: u32, height: u32, rgba8: Vec<u8> },
}

impl Content {
    pub fn extent(&self) -> (u32, u32) {
        match self {
            Self::Clear { width, height, .. } | Self::Pixels { width, height, .. } => (*width, *height),
        }
    }
    /// The bytes an exact readback must produce.
    pub fn expected(&self) -> Result<Vec<u8>, HostError> {
        match self {
            Self::Clear { width, height, rgba } => {
                let len = rgba_len(*width, *height)?;
                Ok(rgba.iter().copied().cycle().take(len).collect())
            }
            Self::Pixels { rgba8, .. } => Ok(rgba8.clone()),
        }
    }
}

/// Document facts for the receipt.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DocumentFacts {
    pub file_name: String,
    pub revision: u64,
    pub backend: String,
    pub layers: u64,
}

/// Builds the content for `scene` at `width x height` (a document uses its own frame size).
pub fn content(scene: &Scene, width: u32, height: u32) -> Result<(Content, Option<DocumentFacts>), HostError> {
    match scene {
        Scene::Clear(rgba) => {
            rgba_len(width, height)?;
            Ok((Content::Clear { width, height, rgba: *rgba }, None))
        }
        Scene::Painted => Ok((Content::Pixels { width, height, rgba8: painted(width, height)? }, None)),
        Scene::Document(path) => document(path),
    }
}

#[cfg(feature = "document")]
fn document(path: &std::path::Path) -> Result<(Content, Option<DocumentFacts>), HostError> {
    let doc = crate::document::render(path)?;
    let facts = DocumentFacts {
        file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        revision: doc.revision,
        backend: doc.backend.to_string(),
        layers: doc.layers,
    };
    Ok((Content::Pixels { width: doc.width, height: doc.height, rgba8: over_checker(doc.width, &doc.rgba8)? }, Some(facts)))
}

#[cfg(not(feature = "document"))]
fn document(_path: &std::path::Path) -> Result<(Content, Option<DocumentFacts>), HostError> {
    Err(HostError::Document("feature_off"))
}

/// A deterministic CPU-painted test card: horizontal red ramp, vertical green ramp, an 8 px checker in blue, and a
/// 2 px white border, all opaque.
pub fn painted(width: u32, height: u32) -> Result<Vec<u8>, HostError> {
    let len = rgba_len(width, height)?;
    let mut out = Vec::with_capacity(len);
    let wd = u64::from(width.saturating_sub(1).max(1));
    let hd = u64::from(height.saturating_sub(1).max(1));
    for y in 0..height {
        for x in 0..width {
            let border = x < 2 || y < 2 || x.saturating_add(2) >= width || y.saturating_add(2) >= height;
            if border {
                out.extend_from_slice(&[255, 255, 255, 255]);
                continue;
            }
            let r = (u64::from(x) * 255 / wd).min(255) as u8;
            let g = (u64::from(y) * 255 / hd).min(255) as u8;
            let b = if ((x / 8) + (y / 8)) % 2 == 0 { 0x30 } else { 0xd0 };
            out.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Ok(out)
}

/// Flattens straight-alpha sRGB bytes over a light 8 px checker (the CPU painter's transparency grid), opaque out.
pub fn over_checker(width: u32, rgba8: &[u8]) -> Result<Vec<u8>, HostError> {
    if width == 0 || rgba8.len() % 4 != 0 || (rgba8.len() / 4) % width as usize != 0 {
        return Err(HostError::Document("frame"));
    }
    let w = width as usize;
    let mut out = Vec::with_capacity(rgba8.len());
    for (i, px) in rgba8.chunks_exact(4).enumerate() {
        let (x, y) = (i % w, i / w);
        let bg: u32 = if ((x / 8) + (y / 8)) % 2 == 0 { 0xff } else { 0xcc };
        let a = u32::from(px[3]);
        for &c in &px[..3] {
            let v = (u32::from(c) * a + bg * (255 - a) + 127) / 255;
            out.push(v.min(255) as u8);
        }
        out.push(255);
    }
    Ok(out)
}
