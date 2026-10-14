//! SMB-CONVERT-NATIVE CN-1: the raster -> layer mapping is public (`raster_proposal` over a
//! `RasterSource`) and a layerless PSD (merged image only) maps to one raster layer through the same
//! code as layer pixels. Synthetic byte fixtures built here, independently of the crate's encoder.

mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Resolution, Resolver, TileRef, validate_raster_records};
use hsk_studio_interop_psd::*;

struct Mint(u64);
impl Mint {
    fn next(&mut self, prefix: &str) -> String {
        self.0 += 1;
        format!("{prefix}-019abcde-0000-7000-8000-{:012x}", self.0)
    }
}
impl IdMint for Mint {
    fn document_id(&mut self) -> String {
        self.next("SDOC")
    }
    fn artboard_id(&mut self) -> String {
        self.next("SART")
    }
    fn layer_id(&mut self) -> String {
        self.next("SLYR")
    }
    fn linear_working_profile_id(&mut self) -> String {
        self.next("SCPF")
    }
    fn tile_manifest_id(&mut self, layer_id: &str, object_key: &str) -> String {
        format!("manifest/{layer_id}/{object_key}")
    }
}
struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

fn budget() -> Budget {
    Budget { input_bytes: 8 << 20, payload_bytes: 4 << 20, ..Budget::default() }
}

fn psd_proposal(bytes: &[u8]) -> ImportProposal {
    let limits = Limits::default();
    let doc = read(bytes);
    let cfg = ProposalConfig::new(&limits, budget(), &Host);
    to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0), None, &CancellationToken::default()).expect("proposal")
}

fn raster(source: RasterSource<'_>, limits: &Limits) -> Result<ImportProposal> {
    let cfg = ProposalConfig::new(limits, budget(), &Host);
    raster_proposal(source, &SourceIdentity::of(b"raster"), &cfg, &mut Mint(0), None, &CancellationToken::default())
}

fn f32_at(bytes: &[u8], index: usize) -> f32 {
    f32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
}

const W: u32 = 3;
const H: u32 = 2;
const R: [u8; 6] = [0, 40, 80, 120, 200, 255];
const G: [u8; 6] = [255, 10, 20, 30, 40, 50];
const B: [u8; 6] = [7, 7, 7, 128, 64, 32];

#[test]
fn layerless_psd_maps_its_merged_image_as_one_raster_layer() {
    // Merged image only: empty layer and mask section, raw image data R, G, B planes.
    let mut planes = R.to_vec();
    planes.extend_from_slice(&G);
    planes.extend_from_slice(&B);
    let layerless = Doc {
        version: 1,
        channels: 3,
        height: H,
        width: W,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: Vec::new(),
        image: Some((0, planes.clone())),
    }
    .bytes();
    let merged = psd_proposal(&layerless);
    let doc = merged.snapshot.document();
    assert_eq!(doc.layers.len(), 1, "one raster layer from the merged image");
    assert_eq!(doc.layers[0].kind, "raster");
    assert_eq!(doc.layers[0].name, "Background");
    assert!(merged.loss.has_code("merged_image_as_single_layer"));
    assert!(merged.layer_map.is_empty() && merged.extras.is_empty(), "no PSD record behind the merged layer");
    assert_eq!(merged.provenance.source_format, "psd");

    // The same pixels as one canvas-covering layer without an alpha channel.
    let mut layer = TLayer::new("Layer", [0, 0, H as i32, W as i32]);
    layer.channels = vec![(0, 0, R.to_vec()), (1, 0, G.to_vec()), (2, 0, B.to_vec())];
    let info = layer_info(&[layer], false, false);
    let layered = Doc {
        version: 1,
        channels: 3,
        height: H,
        width: W,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: layer_section(&info, &[], false),
        image: Some((0, planes)),
    }
    .bytes();
    let per_layer = psd_proposal(&layered);
    assert_eq!(merged.tiles.len(), 1);
    assert_eq!(per_layer.tiles.len(), 1);
    let (a, b) = (&merged.tiles[0], &per_layer.tiles[0]);
    assert_eq!((a.column, a.row), (b.column, b.row));
    assert_eq!(a.colour, b.colour, "colour plane equals the per-layer path");
    assert_eq!(a.alpha, b.alpha, "alpha plane equals the per-layer path (opaque)");
    assert_eq!(a.content_sha256_hex, b.content_sha256_hex);
    assert_eq!(f32_at(&a.alpha, 0), 1.0);
}

#[test]
fn raster_source_sixteen_bit_gray_and_float_rgb_map_to_rgb_f32le_tiles() {
    let limits = Limits::default();
    // 16-bit gray, 2x1: 0 and 65535 (big-endian samples).
    let gray = Plane { width: 2, height: 1, depth: 16, bytes: vec![0, 0, 0xff, 0xff] };
    let p = raster(
        RasterSource {
            format: "tiff",
            name: "page 1",
            width: 2,
            height: 1,
            depth: 16,
            colour_mode: ColorMode::Grayscale,
            planes: vec![gray],
            alpha: None,
            icc: None,
        },
        &limits,
    )
    .expect("gray proposal");
    assert_eq!(p.snapshot.document().layers.len(), 1);
    assert_eq!(p.provenance.source_format, "tiff");
    assert_eq!(p.provenance.psd_version, 0);
    assert!(p.loss.has_code("profile_assumed_srgb") && p.loss.has_code("gray_replicated_to_rgb"));
    assert!(!p.loss.has_code("image_data_missing") && !p.loss.has_code("layer_extras_sidecar_only"));
    let tile = &p.tiles[0];
    assert_eq!(tile.colour.len(), 256 * 256 * 12);
    assert_eq!([f32_at(&tile.colour, 0), f32_at(&tile.colour, 3), f32_at(&tile.colour, 5)], [0.0, 1.0, 1.0]);
    validate_raster_records(&p.raster_records, p.snapshot.document(), budget(), &CancellationToken::default(), &Host)
        .expect("raster records validate");
    let wire = format!("{:?}", p.snapshot.document().layers[0].payload);
    assert!(wire.contains("rgb_f32le"), "tile format: {wire}");

    // 32-bit float RGB with a straight alpha plane, 1x1: floats are linear light as stored.
    let f = |v: f32| v.to_be_bytes().to_vec();
    let plane = |v: f32| Plane { width: 1, height: 1, depth: 32, bytes: f(v) };
    let p = raster(
        RasterSource {
            format: "dng",
            name: "developed",
            width: 1,
            height: 1,
            depth: 32,
            colour_mode: ColorMode::Rgb,
            planes: vec![plane(0.25), plane(0.5), plane(0.75)],
            alpha: Some(plane(0.5)),
            icc: None,
        },
        &limits,
    )
    .expect("float proposal");
    assert!(p.loss.has_code("float32_assumed_linear_light"));
    let tile = &p.tiles[0];
    assert_eq!([f32_at(&tile.colour, 0), f32_at(&tile.colour, 1), f32_at(&tile.colour, 2)], [0.25, 0.5, 0.75]);
    assert_eq!(f32_at(&tile.alpha, 0), 0.5);
    validate_raster_records(&p.raster_records, p.snapshot.document(), budget(), &CancellationToken::default(), &Host)
        .expect("raster records validate");
}

#[test]
fn raster_source_shape_errors_are_typed() {
    let limits = Limits::default();
    let ok_plane = || Plane { width: 2, height: 2, depth: 8, bytes: vec![1; 4] };
    let base = || RasterSource {
        format: "tiff",
        name: "x",
        width: 2,
        height: 2,
        depth: 8,
        colour_mode: ColorMode::Rgb,
        planes: vec![ok_plane(), ok_plane(), ok_plane()],
        alpha: None,
        icc: None,
    };
    assert!(raster(base(), &limits).is_ok());
    let err = |s: RasterSource<'static>, l: &Limits| raster(s, l).expect_err("refused");
    assert_eq!(err(RasterSource { planes: vec![ok_plane()], ..base() }, &limits), PsdError::BadChannelCount);
    let short = Plane { bytes: vec![1; 3], ..ok_plane() };
    assert_eq!(err(RasterSource { planes: vec![ok_plane(), ok_plane(), short], ..base() }, &limits), PsdError::ChannelSize);
    let wrong_alpha = Plane { width: 1, ..ok_plane() };
    assert_eq!(err(RasterSource { alpha: Some(wrong_alpha), ..base() }, &limits), PsdError::ChannelSize);
    assert_eq!(err(RasterSource { depth: 1, ..base() }, &limits), PsdError::BadDepth);
    assert_eq!(
        err(RasterSource { colour_mode: ColorMode::Bitmap, ..base() }, &limits),
        PsdError::UnsupportedMode("raster_colour_mode")
    );
    // dims_u32_max: refused before any allocation from the dimensions.
    assert_eq!(err(RasterSource { width: u32::MAX, height: u32::MAX, ..base() }, &limits), PsdError::BadDimensions);
    assert_eq!(err(RasterSource { width: 0, ..base() }, &limits), PsdError::BadDimensions);
    let few_pixels = Limits { max_pixels: 3, ..Limits::default() };
    assert_eq!(err(base(), &few_pixels), PsdError::PixelLimit);
    // aggregate_over_budget: every plane fits alone, the planes together do not.
    let few_bytes = Limits { max_decoded_bytes: 11, ..Limits::default() };
    assert_eq!(err(base(), &few_bytes), PsdError::PixelLimit);
}
