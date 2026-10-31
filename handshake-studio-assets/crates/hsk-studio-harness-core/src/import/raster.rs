//! PNG / JPEG -> native (SMB-CONVERT-RASTER; feature `import-raster`). `hsk-studio-interop-raster`
//! decodes the container (bounded pre-scan, typed refusals passed through unchanged, WebP refused
//! `webp_decoder_unavailable`), the image goes through interop-psd's `raster_proposal` as one raster
//! layer (the single owner of the raster -> layer mapping, colour route, tiling and digests), and the
//! shared tail writes tile assets, the import receipt and the original bytes.
//!
//! Mapping: Gray/GrayA -> Grayscale, Rgb/Rgba -> Rgb, Cmyk -> Cmyk (planes from
//! `RasterImage::planes_be`, the interop-psd `Plane` layout; alpha is straight). CMYK planes follow the
//! PSD convention (0 = full ink): JPEGs with an Adobe APP14 segment already store inverted CMYK
//! (`JpegInfo::cmyk_inverted`) and pass unchanged; CMYK without APP14 is inverted here and recorded as
//! the loss `jpeg_cmyk_inverted_to_psd_convention`. The embedded ICC profile (PNG iCCP, JPEG APP2) is
//! routed by interop-psd like a PSD profile.
//!
//! What is not imported is a typed loss entry (tests in `tests/import_raster.rs`): Exif orientation other
//! than 1 `raster_orientation_not_applied` (recorded, never applied, as TIFF tag 274), APNG frames after
//! the default image `png_animation_not_imported`, unreadable Exif `jpeg_exif_unreadable`. PNG
//! gAMA/sRGB/cHRM chunks are recorded in the receipt (`raster.png`) and not applied.
#![deny(clippy::arithmetic_side_effects)]

use super::with_package::{SourceMint, package_proposal};
use super::{ImportError, ImportedDocument, SourceFormat};
use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_folio::{self as folio, Resolver};
use hsk_studio_interop_psd as psd;
use hsk_studio_interop_raster as raster;
use serde_json::json;

pub(crate) fn import_raster(
    bytes: &[u8],
    format: SourceFormat,
    resolver: &dyn Resolver,
    actor: &ActorContext,
    cancel: &CancellationToken,
) -> Result<ImportedDocument, ImportError> {
    let importer = |code: &'static str| ImportError::Importer { format, code };
    let img = raster::decode(bytes, &raster::Limits::default(), cancel).map_err(|e| match e {
        raster::RasterError::Canceled => ImportError::Canceled,
        other => importer(other.code()),
    })?;
    let mut losses: Vec<(String, psd::LossKind, &'static str)> = Vec::new();
    let colour_mode = match img.colour {
        raster::Colour::Gray | raster::Colour::GrayA => psd::ColorMode::Grayscale,
        raster::Colour::Rgb | raster::Colour::Rgba => psd::ColorMode::Rgb,
        raster::Colour::Cmyk => psd::ColorMode::Cmyk,
    };
    let mut planes = img.planes_be().map_err(|e| importer(e.code()))?;
    let alpha = if img.colour.has_alpha() { planes.pop() } else { None };
    if img.colour == raster::Colour::Cmyk && !img.jpeg.as_ref().is_some_and(|j| j.cmyk_inverted) {
        for plane in &mut planes {
            plane.iter_mut().for_each(|v| *v = u8::MAX.wrapping_sub(*v));
        }
        losses.push(("image/cmyk".into(), psd::LossKind::Transformed, "jpeg_cmyk_inverted_to_psd_convention"));
    }
    if img.exif_orientation.is_some_and(|o| o != 1) {
        losses.push(("image/exif/orientation".into(), psd::LossKind::Unsupported, "raster_orientation_not_applied"));
    }
    for code in img.losses.iter().copied() {
        let kind = if code == "png_animation_not_imported" { psd::LossKind::PreservedOpaque } else { psd::LossKind::Unsupported };
        losses.push(("image".into(), kind, code));
    }
    let (width, height, depth) = (img.width, img.height, img.depth());
    let plane = |bytes: Vec<u8>| psd::Plane { width, height, depth, bytes };
    let raster_source = psd::RasterSource {
        format: img.format.label(),
        name: "Background",
        width,
        height,
        depth,
        colour_mode,
        planes: planes.into_iter().map(plane).collect(),
        alpha: alpha.map(plane),
        icc: img.icc.clone(),
    };

    let source = psd::SourceIdentity::of(bytes);
    let mut mint = SourceMint::new(&source.sha256);
    let mut frames = super::colour::FrameCount::default();
    let mut converter = super::colour::converter(format, &mint, &mut frames, actor)?;
    let limits = psd::Limits::default();
    let cfg = psd::ProposalConfig::new(&limits, folio::Budget::default(), resolver);
    let psd_error = |e: psd::PsdError| if e == psd::PsdError::Canceled { ImportError::Canceled } else { importer(e.code()) };
    let mut proposal =
        psd::raster_proposal(raster_source, &source, &cfg, &mut mint, Some(&mut converter), cancel).map_err(psd_error)?;
    drop(converter);
    let route = match format {
        SourceFormat::Png => "embedded PNG profile (iCCP) -> working space",
        _ => "embedded JPEG profile (APP2 ICC_PROFILE) -> working space",
    };
    let colour = super::colour::summary(&proposal, frames.0, route);
    for (path, kind, code) in losses {
        proposal.loss.entries.push(psd::LossEntry { path, kind, code });
    }
    let png = img.png.as_ref().map(|p| {
        json!({
            "source_colour_type": p.source_colour_type, "source_bit_depth": p.source_bit_depth, "interlaced": p.interlaced,
            "transparency_chunk": p.transparency_chunk, "gamma": p.gamma, "srgb_intent": p.srgb_intent,
            "chromaticities": p.chromaticities, "animation_frames": p.animation_frames,
        })
    });
    let jpeg = img.jpeg.as_ref().map(|j| {
        json!({
            "process": j.process, "components": j.components, "sampling": j.sampling, "scans": j.scans,
            "adobe_transform": j.adobe_transform, "jfif": j.jfif, "cmyk_inverted": j.cmyk_inverted,
            "ycck_converted": j.ycck_converted, "exif_bytes": j.exif_bytes,
        })
    });
    let extra = json!({
        "colour": colour,
        "raster": {
            "format": img.format.label(), "width": width, "height": height, "colour": format!("{:?}", img.colour),
            "depth": depth, "source_bit_depth": img.source_bit_depth, "icc_bytes": img.icc.as_ref().map(Vec::len),
            "exif_orientation": img.exif_orientation, "png": png, "jpeg": jpeg,
        },
    });
    package_proposal(format, "hsk-studio-interop-raster + hsk-studio-interop-psd", bytes, proposal, extra)
}
