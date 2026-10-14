//! Little-endian (`MIB8` / `46B8`) TIFF ImageSourceData: the byte-swapped layer-and-mask records
//! parse through the SAME readers as big-endian ones (byte order is a reader mode, not a second
//! parser). Fixtures are assembled here byte by byte, independently of the crate; the real-file
//! case reads the Pillow Photoshop-CS sample through the checked-in corpus-root config.

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

fn reversed(tag: &[u8; 4]) -> [u8; 4] {
    [tag[3], tag[2], tag[1], tag[0]]
}

fn le_unicode(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut v = (units.len() as u32).to_le_bytes().to_vec();
    for u in units {
        v.extend(u.to_le_bytes());
    }
    v
}

/// Layer-level block, byte-swapped: reversed signature and key, little-endian length (aligned to 4).
fn le_layer_block(key: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let data = pad_to(data.to_vec(), 4);
    let mut v = b"MIB8".to_vec();
    v.extend(reversed(key));
    v.extend((data.len() as u32).to_le_bytes());
    v.extend(data);
    v
}

/// File-level block, byte-swapped; `wide` selects `46B8` and 8-byte lengths for the wide keys.
fn le_global_block(key: &[u8; 4], data: &[u8], wide: bool) -> Vec<u8> {
    let wide_key = wide && matches!(key, b"Layr" | b"Lr16" | b"LMsk");
    let mut v = if wide { b"46B8".to_vec() } else { b"MIB8".to_vec() };
    v.extend(reversed(key));
    if wide_key {
        v.extend((data.len() as u64).to_le_bytes());
    } else {
        v.extend((data.len() as u32).to_le_bytes());
    }
    v.extend_from_slice(data);
    pad_to(v, 4)
}

/// Little-endian RLE payload: u16 LE row counts, then the PackBits rows.
fn le_rle(rows: &[&[u8]]) -> Vec<u8> {
    let packed: Vec<Vec<u8>> = rows.iter().map(|r| packbits(r)).collect();
    let mut v = Vec::new();
    for p in &packed {
        v.extend((p.len() as u16).to_le_bytes());
    }
    for p in &packed {
        v.extend_from_slice(p);
    }
    v
}

/// One layer record plus its channel data, every number little-endian and every tag reversed.
/// `blocks` are already-swapped layer-level blocks; `wide` writes 8-byte channel lengths (PSB).
fn le_record(l: &TLayer, blocks: &[Vec<u8>], wide: bool) -> (Vec<u8>, Vec<u8>) {
    let mut v = Vec::new();
    for r in l.rect {
        v.extend(r.to_le_bytes());
    }
    v.extend((l.channels.len() as u16).to_le_bytes());
    let mut data = Vec::new();
    for (id, comp, payload) in &l.channels {
        v.extend(id.to_le_bytes());
        let len = 2 + payload.len();
        if wide {
            v.extend((len as u64).to_le_bytes());
        } else {
            v.extend((len as u32).to_le_bytes());
        }
        data.extend(comp.to_le_bytes());
        data.extend_from_slice(payload);
    }
    v.extend(reversed(b"8BIM"));
    v.extend(reversed(&l.key));
    v.extend([l.opacity, l.clipping, l.flags, 0]);
    let mut extra = (l.mask.len() as u32).to_le_bytes().to_vec();
    extra.extend_from_slice(&l.mask);
    extra.extend((l.ranges.len() as u32).to_le_bytes());
    extra.extend_from_slice(&l.ranges);
    let mut pascal = vec![l.name.len() as u8];
    pascal.extend_from_slice(l.name.as_bytes());
    extra.extend(pad_to(pascal, 4));
    for b in blocks {
        extra.extend_from_slice(b);
    }
    v.extend((extra.len() as u32).to_le_bytes());
    v.extend(extra);
    (v, data)
}

fn le_layer_info(layers: &[(TLayer, Vec<Vec<u8>>)], wide: bool) -> Vec<u8> {
    let mut v = (layers.len() as i16).to_le_bytes().to_vec();
    let mut data = Vec::new();
    for (l, blocks) in layers {
        let (record, channels) = le_record(l, blocks, wide);
        v.extend(record);
        data.extend(channels);
    }
    v.extend(data);
    pad_to(v, 2)
}

/// The big-endian fixture (RLE channel, `luni`, `lyid`, group divider) and its byte-swapped twin.
type LeLayer = (TLayer, Vec<Vec<u8>>);

fn twin_layers() -> (Vec<TLayer>, Vec<LeLayer>) {
    let rows: [&[u8]; 2] = [&[20, 20], &[20, 20]];
    let mut base = TLayer::new("base", [0, 0, 2, 2]);
    base.channels = vec![(0, 0, vec![10; 4]), (1, 1, rle(&rows, false)), (2, 0, vec![30; 4])];
    let mut top = TLayer::new("top", [0, 1, 2, 2]);
    top.key = *b"mul ";
    top.opacity = 128;
    top.channels = vec![(-1, 0, vec![128, 255]), (0, 0, vec![200, 201]), (1, 0, vec![100, 101]), (2, 0, vec![0, 1])];
    let mut group = TLayer::new("grp", [0, 0, 0, 0]);
    group.key = *b"pass";

    let mut be_base = base.clone();
    be_base.blocks = vec![layer_block(b"luni", &unicode("Base \u{fc}"), false), layer_block(b"lyid", &be32(7), false)];
    let mut be_group = group.clone();
    let mut divider = be32(1);
    divider.extend_from_slice(b"8BIM");
    divider.extend_from_slice(b"mul ");
    be_group.blocks = vec![layer_block(b"lsct", &divider, false), layer_block(b"luni", &unicode("Group"), false)];

    let mut le_base = base;
    le_base.channels[1].2 = le_rle(&rows);
    let mut le_divider = 1u32.to_le_bytes().to_vec();
    le_divider.extend_from_slice(b"MIB8");
    le_divider.extend(reversed(b"mul "));
    let le = vec![
        (
            le_base,
            vec![le_layer_block(b"luni", &le_unicode("Base \u{fc}")), le_layer_block(b"lyid", &7u32.to_le_bytes())],
        ),
        (top.clone(), Vec::new()),
        (group, vec![le_layer_block(b"lsct", &le_divider), le_layer_block(b"luni", &le_unicode("Group"))]),
    ];
    (vec![be_base, top, be_group], le)
}

type LayerView = (String, Rect, Vec<(i16, u16, Vec<u8>)>, [u8; 4], u8, u8, GroupRole, Option<u32>, Option<[u8; 4]>, Vec<[u8; 4]>);

/// Everything a consumer sees of a layer except the byte-order marker.
fn view(doc: &PsdDocument) -> Vec<LayerView> {
    doc.layers()
        .iter()
        .map(|l| {
            (
                l.name.clone(),
                l.rect,
                l.channels.iter().map(|c| (c.id, c.compression, c.data.clone())).collect(),
                l.blend_key,
                l.opacity,
                l.flags,
                l.group_role(),
                l.layer_id(),
                l.group_blend_key(),
                l.blocks.iter().map(|b| b.key).collect(),
            )
        })
        .collect()
}

#[test]
fn little_endian_image_source_matches_the_big_endian_twin() {
    let (be_layers, le_layers) = twin_layers();
    let mut be_run = global_block(b"Patt", &[1, 2, 3], false);
    be_run.extend(global_block(b"Layr", &layer_info(&be_layers, false, false), false));
    let be_doc = parse(&be_run, canvas()).expect("big-endian twin parses");

    let mut le_run = le_global_block(b"Patt", &[1, 2, 3], false);
    le_run.extend(le_global_block(b"Layr", &le_layer_info(&le_layers, false), false));
    let le_doc = parse(&le_run, canvas()).expect("MIB8 run parses through the same readers");

    // The RLE channel differs in bytes (count table order), so compare after normalising that one
    // payload to its decoded plane; every other field is identical.
    let limits = Limits::default();
    let cancel = CancellationToken::default();
    let mut be_view = view(&be_doc);
    let mut le_view = view(&le_doc);
    for v in [&mut be_view, &mut le_view] {
        v[0].2[1].2.clear();
    }
    assert_eq!(le_view, be_view, "identical layers from either byte order");
    assert!(le_doc.layers().iter().all(|l| l.little_endian) && be_doc.layers().iter().all(|l| !l.little_endian));
    let names: Vec<_> = le_doc.layers().iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Base \u{fc}", "top", "Group"]);
    assert_eq!(le_doc.layers()[0].layer_id(), Some(7));
    assert_eq!(le_doc.layers()[2].group_role(), GroupRole::OpenFolder);
    assert_eq!(le_doc.layers()[2].group_blend_key(), Some(*b"mul "));
    assert_eq!(le_doc.layers()[1].blend_key, *b"mul ");
    // Channel pixels, including the little-endian RLE count table, decode to the same planes.
    for (le_layer, be_layer) in le_doc.layers().iter().zip(be_doc.layers()) {
        for channel in &be_layer.channels {
            let want = be_layer.decode_channel(channel.id, &be_doc.header, &limits, &cancel).expect("be decode");
            let got = le_layer.decode_channel(channel.id, &le_doc.header, &limits, &cancel).expect("le decode");
            assert_eq!(got.map(|p| p.bytes), want.map(|p| p.bytes), "channel {}", channel.id);
        }
    }
    // The PSD proposal route maps the byte-swapped record set like the big-endian one.
    let digests = |p: &ImportProposal| p.tiles.iter().map(|t| t.content_sha256_hex.clone()).collect::<Vec<_>>();
    // (without the unpaired group divider, which the proposal route rightly rejects).
    let be_two = parse(&global_block(b"Layr", &layer_info(&be_layers[..2], false, false), false), canvas()).expect("be two");
    let le_two = parse(&le_global_block(b"Layr", &le_layer_info(&le_layers[..2], false), false), canvas()).expect("le two");
    assert_eq!(digests(&proposal(&le_two)), digests(&proposal(&be_two)));
    assert_eq!(proposal(&le_two).snapshot.document().layers.len(), 2);

    // `46B8` (PSB-style, byte-swapped): 8-byte Layr length and 8-byte channel lengths.
    let two: Vec<(TLayer, Vec<Vec<u8>>)> = le_layers.iter().take(2).cloned().collect();
    let wide_doc = parse(&le_global_block(b"Layr", &le_layer_info(&two, true), true), canvas()).expect("46B8 run parses");
    assert!(wide_doc.header.is_psb());
    let wide_names: Vec<_> = wide_doc.layers().iter().map(|l| l.name.as_str()).collect();
    assert_eq!(wide_names, ["Base \u{fc}", "top"]);
}

#[test]
fn little_endian_image_source_hostile_and_unsupported_inputs_are_typed() {
    let (_, le_layers) = twin_layers();
    let info = le_layer_info(&le_layers, false);
    let run = le_global_block(b"Layr", &info, false);
    let doc = parse(&run, canvas()).expect("baseline parses");

    // Import-only: the writer refuses byte-swapped layers rather than emitting mixed-order bytes.
    assert_eq!(write_psd(&doc, &Limits::default()).err(), Some(PsdError::UnsupportedMode("little_endian_layer_write")));
    // Depth other than 8 is a typed refusal at parse time (samples would be little-endian).
    assert_eq!(
        parse(&run, ImageSourceCanvas { depth: 16, ..canvas() }).err(),
        Some(PsdError::UnsupportedMode("image_source_little_endian_depth"))
    );
    // dims_u32_max-style: hostile record count and channel length.
    let mut many = info.clone();
    many[..2].copy_from_slice(&i16::MAX.to_le_bytes());
    assert!(parse(&le_global_block(b"Layr", &many, false), canvas()).is_err());
    let mut huge_channel = info.clone();
    // count(2) + rect(16) + channel count(2) + channel id(2) -> first channel length
    huge_channel[22..26].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(parse(&le_global_block(b"Layr", &huge_channel, false), canvas()).is_err());
    // aggregate_over_budget-style: decoded bytes over the limit on the byte-swapped route.
    let tight = Limits { max_decoded_bytes: 2, ..Limits::default() };
    let cancel = CancellationToken::default();
    let over = doc.layers()[0].decode_channel(0, &doc.header, &tight, &cancel);
    assert_eq!(over.err(), Some(PsdError::PixelLimit));
    // Truncation at every length of the leading bytes errors or parses, never panics.
    for cut in 0..info.len().min(160) {
        let _ = parse(&le_global_block(b"Layr", &info[..cut], false), canvas());
    }
    // Adjustment payloads are big-endian descriptors/curves: refused, not misread.
    let adjusted = TLayer::new("adj", [0, 0, 0, 0]);
    let with_levl = le_layer_info(&[(adjusted, vec![le_layer_block(b"levl", &[0, 0, 0, 0])])], false);
    assert_eq!(
        parse(&le_global_block(b"Layr", &with_levl, false), canvas()).err(),
        Some(PsdError::UnsupportedMode("image_source_little_endian_adjustment"))
    );
    // Reversed tags only: a big-endian block after a little-endian one ends the run.
    let mut stray = le_global_block(b"Patt", &[1, 2, 3, 4], false);
    stray.extend(global_block(b"Layr", &layer_info(&twin_layers().0, false, false), false));
    assert_eq!(parse(&stray, canvas()).err(), Some(PsdError::Proposal("image_source_no_layer_info")));
}

/// Data of the TIFF ImageSourceData tag (37724) in a little-endian classic TIFF.
fn image_source_tag(tiff: &[u8]) -> Option<&[u8]> {
    let u16_at = |at: usize| tiff.get(at..at.checked_add(2)?).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| tiff.get(at..at.checked_add(4)?).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if tiff.get(..4)? != b"II*\0" {
        return None;
    }
    let ifd = usize::try_from(u32_at(4)?).ok()?;
    for i in 0..usize::from(u16_at(ifd)?) {
        let entry = ifd.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
        if u16_at(entry)? == 37724 {
            let count = usize::try_from(u32_at(entry + 4)?).ok()?;
            let offset = usize::try_from(u32_at(entry + 8)?).ok()?;
            return tiff.get(offset..offset.checked_add(count)?);
        }
    }
    None
}

/// A corpus file through the one corpus-root convention (`.config/oracle-corpus.toml`).
fn corpus_file(relative: &str) -> std::result::Result<Vec<u8>, String> {
    let ws = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().map_err(|e| e.to_string())?;
    let config =
        std::fs::read_to_string(ws.join(".config/oracle-corpus.toml")).map_err(|e| format!("corpus_convention_absent error={e}"))?;
    let value = |key: &str| {
        config.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    let search = value("search").ok_or("corpus_convention_malformed")?;
    let max: usize = value("max-ancestors").and_then(|v| v.parse().ok()).ok_or("corpus_convention_malformed")?;
    let root = ws.ancestors().take(max + 1).map(|a| a.join(&search)).find(|c| c.is_dir()).ok_or("corpus_root_absent")?;
    std::fs::read(root.join(relative)).map_err(|e| format!("file_absent error={e}"))
}

/// The Pillow `tiff_adobe_deflate.tif` (Photoshop CS Windows, little-endian `MIB8ryaL` ImageSourceData).
/// Expected values come from the file's own records (byte inspection, 2026-10-14): 2 layers,
/// "Background" (R,G,B) and "Layer 1" (alpha,R,G,B), both rect 0,0,374,278, blend `norm`, opacity 255.
#[test]
fn pillow_photoshop_cs_little_endian_layers_parse() {
    let real = match corpus_file("tiff-layered/pillow-tiff-adobe-deflate-photoshop-cs.tif") {
        Ok(bytes) => bytes,
        Err(reason) => {
            println!("NOT_PROVEN: pillow_photoshop_cs_little_endian_layers_parse reason={reason}");
            return;
        }
    };
    if real.len() != 313_740 {
        // The sha256 pin lives in the harness import test (this crate has no hash dependency).
        println!("NOT_PROVEN: pillow_photoshop_cs_little_endian_layers_parse reason=length_mismatch");
        return;
    }
    let tag = image_source_tag(&real).expect("tag 37724");
    assert_eq!(tag.len(), 188_236);
    let tagged = tag.strip_prefix(b"Adobe Photoshop Document Data Block\0").expect("document data block signature");
    assert_eq!(&tagged[..8], b"MIB8ryaL");
    let canvas = ImageSourceCanvas { width: 278, height: 374, depth: 8, colour_mode: ColorMode::Rgb, icc: None };
    let doc = parse(tagged, canvas).expect("little-endian Photoshop layer records parse through the shared readers");

    let layers = doc.layers();
    assert_eq!(layers.len(), 2);
    assert_eq!(layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Background", "Layer 1"]);
    let channel_ids: Vec<Vec<i16>> = layers.iter().map(|l| l.channels.iter().map(|c| c.id).collect()).collect();
    assert_eq!(channel_ids, [vec![0, 1, 2], vec![-1, 0, 1, 2]]);
    for l in layers {
        assert_eq!((l.rect.top, l.rect.left, l.rect.bottom, l.rect.right), (0, 0, 374, 278));
        assert_eq!((l.blend_key, l.opacity, l.little_endian), (*b"norm", 255, true));
        assert!(l.layer_id().is_some());
        assert_eq!(l.group_role(), GroupRole::None);
    }
    let sizes: Vec<Vec<usize>> = layers.iter().map(|l| l.channels.iter().map(|c| c.data.len() + 2).collect()).collect();
    assert_eq!(sizes, [vec![2994, 2994, 2994], vec![2994, 58636, 58533, 58418]]);
    assert_eq!(layers.iter().map(|l| l.flags).collect::<Vec<_>>(), [0x29, 0x28]);
    // Every channel decodes to a full plane with the compression it states.
    let (limits, cancel) = (Limits::default(), CancellationToken::default());
    for l in layers {
        for c in &l.channels {
            let plane = l.decode_channel(c.id, &doc.header, &limits, &cancel).expect("decodes").expect("plane");
            assert_eq!(plane.bytes.len(), 278 * 374, "layer {} channel {} compression {}", l.name, c.id, c.compression);
        }
    }
    assert_eq!(proposal(&doc).snapshot.document().layers.len(), 2);
    println!("NOT_PROVEN: pillow_photoshop_cs_parity reason=no_independent_photoshop_oracle (layers parsed from file bytes only)");
}
