//! Plain TIFF -> native (SMB-CONVERT-TIFF; feature `import-tiff`). `hsk-studio-interop-tiff` decodes
//! the container (bounded pre-scan, typed refusals passed through unchanged), the first page goes
//! through interop-psd's `raster_proposal` as one raster layer (the single owner of the raster ->
//! layer mapping, colour route, tiling and digests), and the shared tail writes tile assets,
//! records, the import receipt and the original bytes.
//!
//! What is not imported is a typed loss entry, never dropped silently (test
//! `tiff_corpus_files_import_with_typed_losses_or_pass_through_refusals`):
//! - pages after the first: `page[n]` `tiff_pages_not_imported` (held in the original entry);
//! - Photoshop layers in tag 37724 (ImageSourceData): interop-psd exposes no public layer-record
//!   parser for that block, so the composite is imported and `tiff_photoshop_layers_not_imported`
//!   is recorded (the envelope check result goes to the receipt);
//! - tag 34377 (Photoshop image resources): `tiff_image_resources_not_imported`;
//! - orientation other than 1: `tiff_orientation_not_applied` (interop-tiff records, never applies);
//! - extra samples that are not the alpha: `tiff_extra_samples_not_imported`;
//! - associated (premultiplied) alpha is un-premultiplied for the straight-alpha layer:
//!   `tiff_associated_alpha_unpremultiplied`;
//! - CIELab (PI 8, signed a/b) is re-encoded to the ICC Lab offset encoding the Lab route reads:
//!   `tiff_cielab_reencoded_offset`.
//!
//! u32 and f64 samples have no raster mapping (depth 8/16 integer and 32 float only): typed refusal
//! `tiff_sample_type_unmapped`.
#![deny(clippy::arithmetic_side_effects)]

use super::with_package::{SourceMint, package_proposal};
use super::{ImportError, ImportedDocument, SourceFormat};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, Resolver};
use hsk_studio_interop_psd as psd;
use hsk_studio_interop_tiff as tiff;
use serde_json::json;

const FORMAT: SourceFormat = SourceFormat::Tiff;

fn importer(code: &'static str) -> ImportError {
    ImportError::Importer { format: FORMAT, code }
}

/// Imports the first page of a plain TIFF. `camera_raw_check` names who decided this is not a
/// camera raw: `develop_unsupported_container` (develop refused and [`camera_raw_marker`] found no
/// marker) or `container_markers_absent` (no `import-raw`; [`camera_raw_marker`] found no marker).
pub(crate) fn import_tiff(
    bytes: &[u8],
    resolver: &dyn Resolver,
    cancel: &CancellationToken,
    camera_raw_check: &'static str,
) -> Result<ImportedDocument, ImportError> {
    let file = tiff::decode(bytes, &tiff::Limits::default(), cancel).map_err(|e| match e {
        tiff::TiffError::Canceled => ImportError::Canceled,
        other => importer(other.code()),
    })?;
    let page = file.pages.first().ok_or_else(|| importer("tiff_no_pages"))?;
    let raster = raster_of(page)?;
    let mut losses: Vec<(String, psd::LossKind, &'static str)> = raster.losses.clone();
    for index in 1..file.pages.len() {
        losses.push((format!("page[{index}]"), psd::LossKind::PreservedOpaque, "tiff_pages_not_imported"));
    }
    let image_source = page.image_source_data.as_ref().map(|block| {
        losses.push(("page[0]/tag/37724".into(), psd::LossKind::PreservedOpaque, "tiff_photoshop_layers_not_imported"));
        match tiff::image_source_data(block) {
            Ok(isd) => json!({
                "envelope": "valid", "byte_order": order(isd.byte_order), "wide_sizes": isd.wide_sizes,
                "block_bytes": isd.blocks.len(), "layers": "not_imported (no public interop-psd layer-record parser)",
            }),
            Err(e) => json!({"envelope": e.code(), "layers": "not_imported"}),
        }
    });
    if page.image_resources.is_some() {
        losses.push(("page[0]/tag/34377".into(), psd::LossKind::PreservedOpaque, "tiff_image_resources_not_imported"));
    }
    if page.orientation.is_some_and(|o| o != 1) {
        losses.push(("page[0]/tag/274".into(), psd::LossKind::Unsupported, "tiff_orientation_not_applied"));
    }

    let source = psd::SourceIdentity::of(bytes);
    let mut mint = SourceMint::new(&source.sha256);
    let mut frames = super::colour::FrameCount::default();
    let mut converter = super::colour::converter(FORMAT, &mint, &mut frames)?;
    let limits = psd::Limits::default();
    let cfg = psd::ProposalConfig::new(&limits, folio::Budget::default(), resolver);
    let (width, height) = (page.width, page.height);
    let raster_source = psd::RasterSource {
        format: "tiff",
        name: "Background",
        width,
        height,
        depth: raster.depth,
        colour_mode: raster.mode,
        planes: raster.planes,
        alpha: raster.alpha,
        icc: page.icc.clone(),
    };
    let mut proposal = psd::raster_proposal(raster_source, &source, &cfg, &mut mint, Some(&mut converter), cancel)
        .map_err(|e| if e == psd::PsdError::Canceled { ImportError::Canceled } else { importer(e.code()) })?;
    drop(converter);
    let colour = super::colour::summary(&proposal, frames.0, "embedded TIFF profile (tag 34675) -> working space");
    for (path, kind, code) in losses {
        proposal.loss.entries.push(psd::LossEntry { path, kind, code });
    }
    let resolution = page.resolution.map(|r| json!({"x": [r.x.0, r.x.1], "y": [r.y.0, r.y.1], "unit": r.unit}));
    let extra = json!({
        "colour": colour,
        "tiff": {
            "byte_order": order(file.byte_order), "big_tiff": file.big_tiff, "pages": file.pages.len(),
            "imported_page": 0, "camera_raw_check": camera_raw_check,
            "page": {
                "width": width, "height": height, "source_photometric": page.source_photometric,
                "bits_per_sample": page.bits_per_sample, "source_bits_per_sample": page.source_bits_per_sample,
                "sample_format": format!("{:?}", page.sample_format), "samples_per_pixel": page.samples_per_pixel,
                "planar_in_file": page.planar_in_file, "alpha": format!("{:?}", page.alpha),
                "extra_samples": page.extra_samples, "compression": page.compression, "predictor": page.predictor,
                "orientation": page.orientation, "resolution": resolution,
                "icc_bytes": page.icc.as_ref().map(Vec::len),
            },
            "image_source_data": image_source,
        },
    });
    package_proposal(FORMAT, "hsk-studio-interop-tiff + hsk-studio-interop-psd", bytes, proposal, extra)
}

/// Why a TIFF container is a camera raw, or `None`: DNGVersion (tag 50706) in any walked IFD, or a
/// CFA (32803) / LinearRaw (34892) PhotometricInterpretation in any IFD of the bounded walk (IFD
/// chain plus SubIFDs, where TIFF/EP raws such as NEF and ARW keep the sensor image behind a
/// reduced-resolution IFD0 with NewSubfileType 1). An unwalkable chain falls back to IFD0 alone; an
/// unparseable header is no marker (the plain decoder then refuses with its own code).
pub(crate) fn camera_raw_marker(bytes: &[u8]) -> Option<&'static str> {
    const DNG_VERSION: u16 = 50706;
    const PHOTOMETRIC: u16 = 262;
    const CFA: u64 = 32803;
    const LINEAR_RAW: u64 = 34892;
    let container = tiff::Container::parse(bytes, &tiff::Limits::default()).ok()?;
    let ifds = match container.walk() {
        Ok(ifds) => ifds,
        Err(_) => vec![container.ifd(container.first_ifd_offset()).ok()?],
    };
    if ifds.iter().any(|ifd| ifd.find(DNG_VERSION).is_some()) {
        return Some("dng_version");
    }
    ifds.iter().find_map(|ifd| {
        let entry = ifd.find(PHOTOMETRIC)?;
        match container.uints(entry).ok()?.first().copied()? {
            CFA => Some("cfa_photometric"),
            LINEAR_RAW => Some("linear_raw_photometric"),
            _ => None,
        }
    })
}

fn order(o: tiff::ByteOrder) -> &'static str {
    match o {
        tiff::ByteOrder::Little => "little",
        tiff::ByteOrder::Big => "big",
    }
}

/// Planes of the first page in the PSD sample layout `raster_proposal` reads.
struct Raster {
    mode: psd::ColorMode,
    depth: u16,
    planes: Vec<psd::Plane>,
    alpha: Option<psd::Plane>,
    losses: Vec<(String, psd::LossKind, &'static str)>,
}

/// Sample domain of one page: integer samples with their full-scale value, or 32-bit floats.
enum Domain<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
    F32(&'a [f32]),
}

fn raster_of(page: &tiff::TiffPage) -> Result<Raster, ImportError> {
    let (mode, colour) = match page.photometric {
        tiff::Photometric::BlackIsZero => (psd::ColorMode::Grayscale, 1usize),
        tiff::Photometric::Rgb => (psd::ColorMode::Rgb, 3),
        tiff::Photometric::Cmyk => (psd::ColorMode::Cmyk, 4),
        tiff::Photometric::IccLab | tiff::Photometric::CieLab => (psd::ColorMode::Lab, 3),
    };
    let (domain, depth) = match page.pixels() {
        tiff::Samples::U8(v) => (Domain::U8(v), 8u16),
        tiff::Samples::U16(v) => (Domain::U16(v), 16),
        tiff::Samples::F32(v) => (Domain::F32(v), 32),
        tiff::Samples::U32(_) | tiff::Samples::F64(_) => return Err(importer("tiff_sample_type_unmapped")),
    };
    let pixels = usize::try_from(u64::from(page.width).checked_mul(u64::from(page.height)).ok_or_else(|| importer("dims_overflow"))?)
        .map_err(|_| importer("dims_overflow"))?;
    let total = page.pixels().len();
    if total.checked_rem(pixels) != Some(0) {
        return Err(importer("tiff_sample_count_mismatch"));
    }
    let spp = total.checked_div(pixels).unwrap_or(0);
    if spp < colour {
        return Err(importer("tiff_sample_count_mismatch"));
    }
    let alpha_at = (page.alpha != tiff::Alpha::None && spp > colour).then_some(colour);
    let used = if alpha_at.is_some() { colour.saturating_add(1) } else { colour };
    let mut losses = Vec::new();
    if spp > used {
        losses.push(("page[0]/extra_samples".into(), psd::LossKind::PreservedOpaque, "tiff_extra_samples_not_imported"));
    }
    let associated = alpha_at.is_some() && page.alpha == tiff::Alpha::Associated;
    if associated {
        losses.push(("page[0]/alpha".into(), psd::LossKind::Transformed, "tiff_associated_alpha_unpremultiplied"));
    }
    let cielab = page.photometric == tiff::Photometric::CieLab;
    if cielab {
        if depth == 32 {
            return Err(importer("tiff_cielab_float_unmapped"));
        }
        losses.push(("page[0]/photometric".into(), psd::LossKind::Transformed, "tiff_cielab_reencoded_offset"));
    }
    let cmyk = mode == psd::ColorMode::Cmyk;
    let width_bytes = usize::from(depth.checked_div(8).unwrap_or(0));
    let plane_len = pixels.checked_mul(width_bytes).ok_or_else(|| importer("dims_overflow"))?;
    let mut out: Vec<Vec<u8>> = (0..used).map(|_| Vec::with_capacity(plane_len)).collect();
    match domain {
        Domain::U8(v) => split_int(v.iter().map(|&s| u32::from(s)), spp, colour, alpha_at, 255, associated, cielab, cmyk, &mut out, |p, s| {
            p.push(u8::try_from(s).unwrap_or(u8::MAX));
        }),
        Domain::U16(v) => split_int(v.iter().map(|&s| u32::from(s)), spp, colour, alpha_at, 65_535, associated, cielab, cmyk, &mut out, |p, s| {
            p.extend_from_slice(&u16::try_from(s).unwrap_or(u16::MAX).to_be_bytes());
        }),
        Domain::F32(v) => split_float(v, spp, colour, alpha_at, associated, cmyk, &mut out),
    }
    let mut planes: Vec<psd::Plane> =
        out.into_iter().map(|bytes| psd::Plane { width: page.width, height: page.height, depth, bytes }).collect();
    let alpha = alpha_at.and_then(|_| planes.pop());
    Ok(Raster { mode, depth, planes, alpha, losses })
}

/// Splits interleaved integer samples into planes. Associated alpha is divided out (rounded,
/// clamped; zero alpha gives zero colour); CIELab a/b flip their sign bit (two's complement -> offset
/// encoding); CMYK is stored inverted as in PSD (`max - ink`).
#[allow(clippy::too_many_arguments)]
fn split_int(
    samples: impl Iterator<Item = u32>,
    spp: usize,
    colour: usize,
    alpha_at: Option<usize>,
    max: u32,
    associated: bool,
    cielab: bool,
    cmyk: bool,
    out: &mut [Vec<u8>],
    push: impl Fn(&mut Vec<u8>, u32),
) {
    let half = max.div_ceil(2);
    let mut px: Vec<u32> = Vec::with_capacity(spp);
    let mut samples = samples.peekable();
    while samples.peek().is_some() {
        px.clear();
        px.extend(samples.by_ref().take(spp));
        let a = alpha_at.and_then(|i| px.get(i).copied());
        for (c, plane) in out.iter_mut().enumerate().take(colour) {
            let mut v = px.get(c).copied().unwrap_or(0);
            if let (true, Some(a)) = (associated, a) {
                v = match a {
                    0 => 0,
                    a => {
                        let rounding = u64::from(a.checked_div(2).unwrap_or(0));
                        let scaled = u64::from(v).saturating_mul(u64::from(max)).saturating_add(rounding).checked_div(u64::from(a));
                        u32::try_from(scaled.unwrap_or(0).min(u64::from(max))).unwrap_or(max)
                    }
                };
            }
            if cielab && c > 0 {
                v ^= half;
            }
            if cmyk {
                v = max.saturating_sub(v);
            }
            push(plane, v);
        }
        if let (Some(a), Some(plane)) = (a, out.get_mut(colour)) {
            push(plane, a);
        }
    }
}

/// Float counterpart of [`split_int`] (big-endian IEEE planes; CMYK ink inverted as `1 - ink`).
fn split_float(samples: &[f32], spp: usize, colour: usize, alpha_at: Option<usize>, associated: bool, cmyk: bool, out: &mut [Vec<u8>]) {
    for px in samples.chunks_exact(spp) {
        let a = alpha_at.and_then(|i| px.get(i).copied());
        for (c, plane) in out.iter_mut().enumerate().take(colour) {
            let mut v = px.get(c).copied().unwrap_or(0.0);
            if let (true, Some(a)) = (associated, a) {
                v = if a > 0.0 { v / a } else { 0.0 };
            }
            if cmyk {
                v = 1.0 - v;
            }
            plane.extend_from_slice(&v.to_be_bytes());
        }
        if let (Some(a), Some(plane)) = (a, out.get_mut(colour)) {
            plane.extend_from_slice(&a.to_be_bytes());
        }
    }
}
