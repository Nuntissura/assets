//! Affinity thumbnail preview -> native (lane SMB-AFFINITY-INTEROP-1 slice 3; feature `import-affinity`;
//! lead decision 2026-10-16, AF-02). An explicit opt-in next to the default refusal: the default
//! [`super::import`] still refuses every `.af*` file with `affinity_native_use_export`; only
//! [`super::import_affinity_preview`] opens one, as ONE flattened raster layer from the container's
//! embedded `Thmb` preview (hsk-studio-interop-affinity reads the KA container; interop-raster decodes the
//! PNG/JPEG; interop-psd `raster_proposal` maps it like any raster import).
//!
//! The result is marked lossy everywhere it is reported: receipt `affinity.lossy = true`,
//! `affinity.preview_only = true`, loss entries `affinity_layers_not_decoded` (Rasterized: the document
//! is replaced by the preview pixels; layers, vectors, text and adjustments are not decoded) and
//! `affinity_preview_resolution` (Transformed: the preview's pixel size, not the document's). The
//! original `.af*` bytes are the package's original entry, so nothing is dropped (STU-IO-005); the
//! container's head streams, image/font/PDF resource blobs (content-addressed sha256) and the
//! supported export routes are listed in the receipt. Real Affinity-written previews beyond the V1
//! AFDesignLoad designs are UNVERIFIED (SMB-AFFINITY-ORACLE-OPERATOR).
#![deny(clippy::arithmetic_side_effects)]

use super::with_package::{SourceMint, package_proposal};
use super::{ImportError, ImportedDocument, SourceFormat};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, Resolver};
use hsk_studio_interop_affinity as affinity;
use hsk_studio_interop_psd as psd;
use hsk_studio_interop_raster as raster;
use serde_json::json;

const FORMAT: SourceFormat = SourceFormat::Affinity;

fn refusal(e: affinity::AffinityError) -> ImportError {
    if e == affinity::AffinityError::Canceled { ImportError::Canceled } else { ImportError::Importer { format: FORMAT, code: e.code() } }
}

pub(crate) fn import_preview(bytes: &[u8], resolver: &dyn Resolver, cancel: &CancellationToken) -> Result<ImportedDocument, ImportError> {
    cancel.check().map_err(|_| ImportError::Canceled)?;
    let limits = affinity::Limits::default();
    let container = affinity::KaContainer::parse(bytes, &limits).map_err(refusal)?;
    let header = container.header;
    if !header.is_document() {
        return Err(super::affinity_refusal(bytes));
    }
    let thumb = container.thumbnail().map_err(refusal)?;
    // A stream that does not decode (CRC, codec) does not block the preview: it is a typed loss entry
    // and the receipt names the code; cancellation still stops the import.
    let (resources, resources_error) = match container.resources(cancel) {
        Ok(r) => (r, None),
        Err(affinity::AffinityError::Canceled) => return Err(ImportError::Canceled),
        Err(e) => (affinity::Resources { assets: Vec::new(), carried: Vec::new() }, Some(e.code())),
    };
    let importer = |code: &'static str| ImportError::Importer { format: FORMAT, code };
    let img = raster::decode(&thumb.bytes, &raster::Limits::default(), cancel).map_err(|e| match e {
        raster::RasterError::Canceled => ImportError::Canceled,
        other => importer(other.code()),
    })?;
    let colour_mode = match img.colour {
        raster::Colour::Gray | raster::Colour::GrayA => psd::ColorMode::Grayscale,
        raster::Colour::Rgb | raster::Colour::Rgba => psd::ColorMode::Rgb,
        // A CMYK preview is not expected (PNG previews); refused rather than guessed.
        raster::Colour::Cmyk => return Err(importer("affinity_preview_cmyk_unmapped")),
    };
    let mut planes = img.planes_be().map_err(|e| importer(e.code()))?;
    let alpha = if img.colour.has_alpha() { planes.pop() } else { None };
    let (width, height, depth) = (img.width, img.height, img.depth());
    let plane = |bytes: Vec<u8>| psd::Plane { width, height, depth, bytes };
    let raster_source = psd::RasterSource {
        format: "affinity_preview",
        name: "Affinity preview (flattened)",
        width,
        height,
        depth,
        colour_mode,
        planes: planes.into_iter().map(plane).collect(),
        alpha: alpha.map(plane),
        icc: img.icc.clone(),
    };
    // Identity and the original are the .af* file, not the preview.
    let source = psd::SourceIdentity::of(bytes);
    let mut mint = SourceMint::new(&source.sha256);
    let mut frames = super::colour::FrameCount::default();
    let mut converter = super::colour::converter(FORMAT, &mint, &mut frames)?;
    let psd_limits = psd::Limits::default();
    let cfg = psd::ProposalConfig::new(&psd_limits, folio::Budget::default(), resolver);
    let psd_error = |e: psd::PsdError| if e == psd::PsdError::Canceled { ImportError::Canceled } else { importer(e.code()) };
    let mut proposal = psd::raster_proposal(raster_source, &source, &cfg, &mut mint, Some(&mut converter), cancel).map_err(psd_error)?;
    drop(converter);
    let colour = super::colour::summary(&proposal, frames.0, "embedded preview profile (PNG iCCP / JPEG APP2) -> working space");
    proposal.loss.entries.push(psd::LossEntry {
        path: "document/layers".into(),
        kind: psd::LossKind::Rasterized,
        code: affinity::LOSS_LAYERS_NOT_DECODED,
    });
    proposal.loss.entries.push(psd::LossEntry {
        path: "document/size".into(),
        kind: psd::LossKind::Transformed,
        code: "affinity_preview_resolution",
    });
    if let Some(code) = resources_error {
        proposal.loss.entries.push(psd::LossEntry { path: "document/resources".into(), kind: psd::LossKind::PreservedOpaque, code });
    }
    let streams: Vec<_> = container
        .streams()
        .iter()
        .map(|s| json!({"name": s.name, "id": s.entry.id, "size": s.entry.size, "stored_size": s.entry.stored_size, "codec": format!("{:?}", s.entry.codec())}))
        .collect();
    let assets: Vec<_> = resources
        .assets
        .iter()
        .map(|a| json!({"stream": a.stream, "kind": a.kind.label(), "sha256": affinity::hex(&a.sha256), "bytes": a.bytes.len()}))
        .collect();
    let carried: Vec<_> = resources.carried.iter().map(|(n, k, size)| json!({"stream": n, "kind": k.label(), "size": size})).collect();
    let extra = json!({
        "colour": colour,
        "affinity": {
            "lossy": true,
            "preview_only": true,
            "layers_decoded": false,
            "generation": header.generation().label(),
            "container_version": header.version,
            "container_flags": header.flags,
            "class": header.class_text(),
            "directories": container.directories.iter().map(|d| d.kind.tag()).collect::<Vec<_>>(),
            "thumbnail": {
                "kind": thumb.kind.label(), "width": thumb.width, "height": thumb.height,
                "located_by": thumb.located_by, "sha256": affinity::hex(&thumb.sha256), "bytes": thumb.bytes.len(),
            },
            "streams": streams,
            "resource_assets": assets,
            "carried": carried,
            "resources_error": resources_error,
            "export_routes": affinity::EXPORT_ROUTES,
            "route": "for editable layers export PSD, PDF or SVG from Affinity and open that file",
        },
    });
    package_proposal(FORMAT, "hsk-studio-interop-affinity + hsk-studio-interop-raster + hsk-studio-interop-psd", bytes, proposal, extra)
}
