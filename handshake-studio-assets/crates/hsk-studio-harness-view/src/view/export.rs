//! Export the composited document as PNG or JPEG (SMB-CONVERT-RASTER slice 2; feature `viewer`).
//!
//! Pixels come from [`super::render::render`] (the same composite the viewer shows: hsk-studio-composite
//! lowered to a render plan, executed by render-cpu or render-gpu), 8-bit sRGB-encoded with straight
//! alpha. The encoders are hsk-studio-interop-raster's (CX-CAP-001: the harness adds no codec):
//! - PNG: RGB when every pixel is opaque, RGBA otherwise; 8-bit; no iCCP (the frame is sRGB-encoded and
//!   no encoded-sRGB profile exists in Prism, so the file is untagged sRGB: note `srgb_untagged`).
//! - JPEG: the clean-room baseline encoder; quality 1..=100 (default 90), 4:4:4 at quality >= 90 and 4:2:0
//!   below; a frame with any transparency is flattened over white in the encoded sRGB values (note
//!   `alpha_flattened_on_white`).
//!
//! Every export is decoded back with interop-raster before it is written (dimensions must match), and the
//! file is written atomically (sibling temp file, then rename). Exports are derived deliverables, never
//! stored back into the document (STU-IO-136).
#![deny(clippy::arithmetic_side_effects)]

use super::render::Frame;
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_raster as raster;
use serde_json::{Value, json};
use std::path::Path;

/// Default JPEG quality of the CLI and the viewer button.
pub const DEFAULT_JPEG_QUALITY: u8 = 90;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Png,
    /// Quality 1..=100.
    Jpeg { quality: u8 },
}

impl ExportFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg { .. } => "jpeg",
        }
    }
}

/// An encoded export.
#[derive(Clone, Debug, PartialEq)]
pub struct Exported {
    pub bytes: Vec<u8>,
    pub format: ExportFormat,
    pub width: u32,
    pub height: u32,
    /// Stable codes describing what the export did (`srgb_untagged`, `alpha_flattened_on_white`, ...).
    pub notes: Vec<&'static str>,
    /// `Rgb`, `Rgba` or `Gray` as written.
    pub colour: &'static str,
    /// JPEG chroma subsampling as written.
    pub subsampling: Option<&'static str>,
}

impl Exported {
    pub fn report(&self) -> Value {
        json!({
            "format": self.format.name(), "width": self.width, "height": self.height, "bytes": self.bytes.len(),
            "colour": self.colour, "subsampling": self.subsampling, "notes": self.notes,
            "quality": match self.format { ExportFormat::Jpeg { quality } => Some(quality), ExportFormat::Png => None },
        })
    }
}

/// Encodes a rendered frame. Errors are stable codes (`frame_size`, interop-raster codes, `export_verify_failed`).
pub fn encode_frame(frame: &Frame, format: ExportFormat) -> Result<Exported, &'static str> {
    let pixels = usize::try_from(u64::from(frame.width).saturating_mul(u64::from(frame.height))).map_err(|_| "frame_size")?;
    if Some(frame.rgba8.len()) != pixels.checked_mul(4) {
        return Err("frame_size");
    }
    let opaque = frame.rgba8.chunks_exact(4).all(|p| p[3] == u8::MAX);
    let mut notes = vec!["srgb_untagged"];
    let (bytes, colour, subsampling) = match format {
        ExportFormat::Png => {
            let (colour, samples, name) = if opaque {
                (raster::Colour::Rgb, rgb_of(frame, false), "Rgb")
            } else {
                (raster::Colour::Rgba, frame.rgba8.clone(), "Rgba")
            };
            let samples = raster::Samples::U8(samples);
            let req = raster::PngEncodeRequest {
                width: frame.width,
                height: frame.height,
                colour,
                samples: &samples,
                icc: None,
                compression: raster::PngCompression::Balanced,
            };
            (raster::encode_png(&req).map_err(|e| e.code())?, name, None)
        }
        ExportFormat::Jpeg { quality } => {
            if !opaque {
                notes.push("alpha_flattened_on_white");
            }
            let samples = raster::Samples::U8(rgb_of(frame, !opaque));
            let (subsampling, name) =
                if quality >= 90 { (raster::JpegSubsampling::S444, "4:4:4") } else { (raster::JpegSubsampling::S420, "4:2:0") };
            let req = raster::JpegEncodeRequest {
                width: frame.width,
                height: frame.height,
                colour: raster::Colour::Rgb,
                samples: &samples,
                quality,
                subsampling,
                icc: None,
            };
            (raster::encode_jpeg(&req).map_err(|e| e.code())?, "Rgb", Some(name))
        }
    };
    // Decode-back check through the codec owner before anything is written.
    let back = raster::decode(&bytes, &raster::Limits::default(), &CancellationToken::default()).map_err(|_| "export_verify_failed")?;
    if (back.width, back.height) != (frame.width, frame.height) {
        return Err("export_verify_failed");
    }
    Ok(Exported { bytes, format, width: frame.width, height: frame.height, notes, colour, subsampling })
}

/// RGB samples of the frame; with `flatten`, each pixel is composited over white in the encoded values.
fn rgb_of(frame: &Frame, flatten: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(frame.rgba8.len());
    for p in frame.rgba8.chunks_exact(4) {
        if flatten {
            let a = f32::from(p[3]) / 255.0;
            for c in &p[..3] {
                let v = f32::from(*c) * a + 255.0 * (1.0 - a);
                // In 0.0..=255.0: a convex combination of two values in that range.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                out.push(v.round().clamp(0.0, 255.0) as u8);
            }
        } else {
            out.extend_from_slice(&p[..3]);
        }
    }
    out
}

/// Writes `bytes` to `path` atomically: a sibling temp file, then a rename over `path`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    let name = path.file_name().ok_or("export_path")?.to_string_lossy().into_owned();
    let temp = path.with_file_name(format!(".{name}.export-tmp"));
    std::fs::write(&temp, bytes).map_err(|_| "export_write")?;
    std::fs::rename(&temp, path).map_err(|_| {
        let _ = std::fs::remove_file(&temp);
        "export_write"
    })
}
