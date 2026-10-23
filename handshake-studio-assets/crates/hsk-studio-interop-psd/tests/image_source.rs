//! SMB-CONVERT-FOLLOWUPS slice 3: Photoshop layer records carried in a TIFF's ImageSourceData
//! (tag 37724) parse through this crate's own PSD layer readers. The same layer-info bytes are
//! embedded once in a PSD file (primary layer-info area) and once as a `Layr` block of an
//! ImageSourceData run; both parse to identical layer records and map to the same layers.
//! Byte fixtures are assembled here (tests/common), independently of the crate's encoder.

mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Resolution, Resolver, TileRef};
use hsk_studio_interop_psd::*;

struct Mint(u64);
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
impl Mint {
    fn next(&mut self, prefix: &str) -> String {
        self.0 += 1;
        format!("{prefix}-019abcde-0000-7000-8000-{:012x}", self.0)
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

fn layers() -> Vec<TLayer> {
    let mut base = TLayer::new("base", [0, 0, 2, 2]);
    base.channels = vec![(0, 0, vec![10; 4]), (1, 0, vec![20; 4]), (2, 0, vec![30; 4])];
    let mut top = TLayer::new("top", [0, 1, 2, 2]);
    top.key = *b"mul ";
    top.opacity = 128;
    top.channels = vec![(-1, 0, vec![128, 255]), (0, 0, vec![200, 201]), (1, 0, vec![100, 101]), (2, 0, vec![0, 1])];
    vec![base, top]
}

fn canvas() -> ImageSourceCanvas {
    ImageSourceCanvas { width: 2, height: 2, depth: 8, colour_mode: ColorMode::Rgb, icc: None }
}

fn parse(tagged: &[u8], canvas: ImageSourceCanvas) -> Result<PsdDocument> {
    image_source_document(tagged, canvas, &Limits::default(), &CancellationToken::default())
}

fn proposal(doc: &PsdDocument) -> ImportProposal {
    let limits = Limits::default();
    let cfg = ProposalConfig::new(&limits, Budget { input_bytes: 8 << 20, payload_bytes: 4 << 20, ..Budget::default() }, &Host);
    to_proposal(doc, &SourceIdentity::of(b"isd"), &cfg, &mut Mint(0), None, &CancellationToken::default()).expect("proposal")
}

#[test]
fn image_source_layer_records_parse_like_the_same_records_in_a_psd() {
    let info = layer_info(&layers(), false, false);
    let psd = Doc {
        version: 1,
        channels: 3,
        height: 2,
        width: 2,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: layer_section(&info, &[], false),
        image: Some((0, vec![0; 12])),
    }
    .bytes();
    let from_psd = read(&psd);

    // ImageSourceData run: an unknown block, the layer info, the global mask (padded to 4 each).
    let mut tagged = global_block(b"Patt", &[1, 2, 3], false);
    tagged.extend(global_block(b"Layr", &info, false));
    tagged.extend(global_block(b"LMsk", &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], false));
    let icc = vec![7u8; 9];
    let doc = parse(&tagged, ImageSourceCanvas { icc: Some(icc.clone()), ..canvas() }).expect("image source parses");

    assert_eq!(doc.layers(), from_psd.layers(), "same records as the PSD reader's");
    assert_eq!(doc.layers().len(), 2);
    assert_eq!(doc.layers()[1].name, "top");
    assert_eq!(doc.header.width, 2);
    assert_eq!(doc.header.color_mode, ColorMode::Rgb);
    assert!(doc.image_data.is_none());
    assert_eq!(doc.resources.icc_profile(), Some(&icc[..]));
    assert_eq!(doc.layer_section.global_mask.as_ref().map(Vec::len), Some(13));
    assert_eq!(doc.layer_section.blocks.len(), 1, "Patt stays a global tagged block");
    assert!(matches!(doc.layer_section.layer_info.as_ref().unwrap().location, LayerInfoLocation::Tagged { key: [b'L', b'a', b'y', b'r'], .. }));

    // The header-only document maps through the PSD proposal route: same layers, same tiles.
    let from_isd = proposal(&doc);
    let reference = proposal(&from_psd);
    let names = |p: &ImportProposal| p.snapshot.document().layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&from_isd), names(&reference));
    assert_eq!(from_isd.snapshot.document().layers.len(), 2);
    let digests = |p: &ImportProposal| p.tiles.iter().map(|t| t.content_sha256_hex.clone()).collect::<Vec<_>>();
    assert_eq!(digests(&from_isd), digests(&reference));
    assert!(from_isd.loss.entries.iter().any(|e| e.code == "tagged_block_opaque"), "Patt is a typed loss");

    // PSB-style run (`8B64`): Layr carries an 8-byte length.
    let wide_info = layer_info(&layers(), true, false);
    let mut wide = b"8B64Layr".to_vec();
    wide.extend(be64(wide_info.len() as u64));
    wide.extend(&wide_info);
    let wide_doc = parse(&wide, canvas()).expect("8B64 run parses");
    assert_eq!(wide_doc.layers().len(), 2);
    assert!(wide_doc.header.is_psb());
}

#[test]
fn image_source_refusals_are_typed() {
    let info = layer_info(&layers(), false, false);
    let layr = global_block(b"Layr", &info, false);
    // A little-endian signature over big-endian numbers is not misread as a valid run
    // (little-endian runs proper: tests/image_source_le.rs).
    let mut mixed = b"MIB8ryaL".to_vec();
    mixed.extend((info.len() as u32).to_be_bytes());
    mixed.extend(&info);
    assert!(parse(&mixed, canvas()).is_err());
    assert_eq!(parse(b"46B8", canvas()).err(), Some(PsdError::Proposal("image_source_no_layer_info")));
    // No layer-info block.
    assert_eq!(parse(&global_block(b"Patt", &[1], false), canvas()).err(), Some(PsdError::Proposal("image_source_no_layer_info")));
    assert_eq!(parse(b"", canvas()).err(), Some(PsdError::Truncated("image_source_data")));
    assert_eq!(parse(b"XXXXLayr", canvas()).err(), Some(PsdError::BadSignature));
    // A layer-info length past the end, and a hostile record count.
    let mut short = layr.clone();
    short.truncate(layr.len() - 8);
    assert!(parse(&short, canvas()).is_err());
    let mut many = info.clone();
    many[..2].copy_from_slice(&i16::MAX.to_be_bytes());
    assert!(parse(&global_block(b"Layr", &many, false), canvas()).is_err());
    // Canvas checks: dims_u32_max-style and depth/mode.
    let huge = ImageSourceCanvas { width: u32::MAX, height: u32::MAX, ..canvas() };
    assert_eq!(parse(&layr, huge).err(), Some(PsdError::BadDimensions));
    let over = ImageSourceCanvas { width: 30_000, height: 30_000, ..canvas() };
    let tight = Limits { max_pixels: 1 << 20, ..Limits::default() };
    assert_eq!(image_source_document(&layr, over, &tight, &CancellationToken::default()).err(), Some(PsdError::PixelLimit));
    assert_eq!(parse(&layr, ImageSourceCanvas { depth: 1, ..canvas() }).err(), Some(PsdError::BadDepth));
    assert_eq!(
        parse(&layr, ImageSourceCanvas { colour_mode: ColorMode::Indexed, ..canvas() }).err(),
        Some(PsdError::UnsupportedMode("image_source_colour_mode"))
    );
    let input = Limits { max_input_bytes: 4, ..Limits::default() };
    assert_eq!(image_source_document(&layr, canvas(), &input, &CancellationToken::default()).err(), Some(PsdError::InputTooLarge));
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(image_source_document(&layr, canvas(), &Limits::default(), &canceled).err(), Some(PsdError::Canceled));
}
