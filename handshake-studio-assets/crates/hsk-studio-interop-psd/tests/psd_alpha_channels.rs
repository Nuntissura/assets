//! Lane SMB-FOLIO-CHANNEL-RECORD slice 4 (MT-038 AC-003, STU-RAS-022 interchange): PSD extra
//! channels (alpha and spot) import as Folio channel records with exact samples, named from
//! resources 1045/1006, identified by 1053 and typed by DisplayInfo 1077; an unmodified file
//! re-emits identically and edits (name, kind, samples) write back. Byte fixtures are assembled
//! here (tests/common), independently of the crate's encoder; psd-tools corpus files are pinned by
//! byte length (NOT_PROVEN when absent). Resource layouts: Adobe Photoshop File Formats
//! Specification resource table and psd-tools (MIT) readers; Photoshop semantics of the mode byte
//! UNVERIFIED (D5).
mod common;

use common::*;
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CorrelationId};
use hsk_studio_folio::{Budget, Resolution, Resolver, StudioChannelColour, StudioChannelKind, TileRef};
use hsk_studio_interop_psd::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_pigment::selection::{CombineMode, SelectionLimits};
use hsk_studio_pigment::selection_channel_roundtrip::*;

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
struct TileMint;
impl ChannelTileMint for TileMint {
    fn artifact_manifest_id(&mut self, channel_id: &str, object_key: &str, _: &str) -> String {
        format!("edited/{channel_id}/{object_key}")
    }
}
#[derive(Default)]
struct Frames(usize);
impl SinkPort for Frames {
    fn try_send(&mut self, _: DeliveryClass, _: &[u8]) -> std::result::Result<(), DeliveryError> {
        self.0 += 1;
        Ok(())
    }
}
fn cmd() -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(actor, ClientSessionId::parse("cli-1").unwrap(), None, 1, CorrelationId::new(9), None)
}

const W: u32 = 260;
const H: u32 = 3;

fn pascal_list(names: &[&[u8]]) -> Vec<u8> {
    names.iter().flat_map(|n| std::iter::once(n.len() as u8).chain(n.iter().copied())).collect()
}
fn unicode_list(names: &[&str]) -> Vec<u8> {
    names.iter().flat_map(|n| unicode(&format!("{n}\0"))).collect()
}
fn display(entries: &[(u16, [u16; 4], u16, u8)]) -> Vec<u8> {
    let mut v = be32(1);
    for (space, c, opacity, mode) in entries {
        v.extend(be16(*space));
        for x in c {
            v.extend(be16(*x));
        }
        v.extend(be16(*opacity));
        v.push(*mode);
    }
    v
}
/// Sample `x, y` of extra plane `k` at `depth` (big-endian, as stored).
fn sample(k: usize, x: u32, y: u32, depth: u16) -> Vec<u8> {
    let v = (x * 7 + y * 31 + k as u32 * 101) % 256;
    match depth {
        8 => vec![v as u8],
        16 => be16((v * 257 + k as u32) as u16),
        _ => ((v as f32) / 255.0).to_be_bytes().to_vec(),
    }
}
/// A layerless RGB file: colour planes, the merged transparency (alpha id 0), then `extra`
/// channels described by `resources`. Raw image data.
fn doc_with(depth: u16, extra: usize, resources: Vec<u8>) -> Vec<u8> {
    doc_layered(depth, extra, resources, false)
}
/// [`doc_with`], optionally with one empty layer (so no layer tiles are produced).
fn doc_layered(depth: u16, extra: usize, resources: Vec<u8>, layered: bool) -> Vec<u8> {
    let planes = 3 + 1 + extra;
    let mut image = Vec::new();
    for k in 0..planes {
        for y in 0..H {
            for x in 0..W {
                image.extend(if k < 3 { sample(9 + k, x, y, depth) } else { sample(k - 3, x, y, depth) });
            }
        }
    }
    Doc {
        version: 1,
        channels: planes as u16,
        height: H,
        width: W,
        depth,
        mode: 3,
        color_mode_data: Vec::new(),
        resources,
        layer_section: if layered {
            let mut empty = TLayer::new("empty", [0, 0, 0, 0]);
            empty.channels = empty_channels(&[-1, 0, 1, 2]);
            layer_section(&layer_info(&[empty], false, false), &[], false)
        } else {
            Vec::new()
        },
        image: Some((0, image)),
    }
    .bytes()
}
/// Transparency, an alpha channel (Photoshop default options: red, 50 percent, mode 1) and a Lab
/// spot channel (mode 2, solidity 20).
fn standard_resources(unicode_names: bool) -> Vec<u8> {
    let mut r = resource(1006, "", &pascal_list(&[b"Transparency", b"Alpha 1", b"Spot\xe9"]));
    if unicode_names {
        r.extend(resource(1045, "", &unicode_list(&["Transparency", "Alpha One", "Spot ink"])));
    }
    r.extend(resource(1053, "", &[be32(0), be32(5), be32(6)].concat()));
    r.extend(resource(
        1077,
        "",
        &display(&[(0, [65535, 0, 0, 0], 100, 1), (0, [65535, 0, 0, 0], 50, 1), (7, [5000, (-2000i16) as u16, 3000, 0], 20, 2)]),
    ));
    r
}
fn proposal_with(bytes: &[u8], limits: &Limits) -> Result<(PsdDocument, ImportProposal)> {
    let doc = read_psd(bytes, limits, &CancellationToken::default())?;
    let cfg = ProposalConfig::new(limits, Budget { input_bytes: 64 << 20, payload_bytes: 16 << 20, ..Budget::default() }, &Host);
    let p = to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0), None, &CancellationToken::default())?;
    Ok((doc, p))
}
fn proposal(bytes: &[u8]) -> (PsdDocument, ImportProposal) {
    proposal_with(bytes, &Limits::default()).expect("proposal")
}
fn tile_sample(p: &ImportProposal, channel: &str, x: u32, y: u32, per: usize) -> Vec<u8> {
    let (column, row) = (i64::from(x / 256), i64::from(y / 256));
    let t = p.channel_tiles.iter().find(|t| t.channel_id == channel && t.column == column && t.row == row).unwrap();
    let at = (((y % 256) * 256 + x % 256) as usize) * per;
    t.bytes[at..at + per].to_vec()
}
fn codes(p: &ImportProposal) -> Vec<(&str, &str)> {
    p.loss.entries.iter().map(|e| (e.path.as_str(), e.code)).collect()
}

#[test]
fn alpha_and_spot_channels_import_as_channel_records() {
    bounded_case("alpha_and_spot_channels_import_as_channel_records", || {
        let (_, p) = proposal(&doc_with(8, 2, standard_resources(true)));
        let channels = &p.raster_records.channels;
        // The merged transparency (alpha id 0) is not a channel.
        assert_eq!(channels.iter().map(|c| c.channel_id.as_str()).collect::<Vec<_>>(), ["psd-channel-4", "psd-channel-5"]);
        let (alpha, spot) = (&channels[0], &channels[1]);
        assert_eq!((alpha.name.as_str(), alpha.kind, alpha.opacity, alpha.visible), ("Alpha One", StudioChannelKind::MaskedAreaAlpha, 50.0, false));
        assert_eq!(alpha.colour, StudioChannelColour::Rgb { r: 1.0, g: 0.0, b: 0.0 });
        assert_eq!((spot.name.as_str(), spot.kind, spot.opacity), ("Spot ink", StudioChannelKind::SpotColour, 20.0));
        assert_eq!(spot.colour, StudioChannelColour::Lab { l: 50.0, a: -20.0, b: 30.0 });
        assert_eq!((alpha.width, alpha.height, alpha.depth), (W, H, 8));
        // 260 x 3 on the 256 grid: two cells per channel.
        assert_eq!(alpha.tiles.len(), 2);
        assert_eq!(p.channel_tiles.len(), 4);
        for (k, id) in [(1usize, "psd-channel-4"), (2, "psd-channel-5")] {
            for (x, y) in [(0, 0), (255, 2), (256, 1), (259, 2)] {
                assert_eq!(tile_sample(&p, id, x, y, 1), sample(k, x, y, 8), "{id} at ({x},{y})");
            }
        }
        // Edge padding beyond the extent is zero.
        let edge = p.channel_tiles.iter().find(|t| t.channel_id == "psd-channel-4" && t.column == 1).unwrap();
        assert!(edge.bytes[4..256].iter().all(|b| *b == 0));
        // Map back to the file.
        assert_eq!(p.channel_map.iter().map(|e| (e.plane, e.extra_index)).collect::<Vec<_>>(), [(4, 1), (5, 2)]);
        // Without 1045, names come from 1006 (bytes as Latin-1).
        let (_, p) = proposal(&doc_with(8, 2, standard_resources(false)));
        assert_eq!(p.raster_records.channels[1].name, "Spot\u{e9}");
        // The shared mechanism: Pigment loads the imported channel as a selection (masked-area
        // alpha: coverage = tone).
        let alpha = p.raster_records.channel("psd-channel-4").unwrap();
        let plane = load_selection(alpha, p.channel_tiles.as_slice(), None, CombineMode::Replace, &SelectionLimits::default(), &CancellationToken::default()).unwrap();
        assert_eq!(plane.at(3, 1), u16::from(sample(1, 3, 1, 8)[0]) * 257);
    });
}

#[test]
fn sixteen_and_thirty_two_bit_samples_are_exact() {
    bounded_case("sixteen_and_thirty_two_bit_samples_are_exact", || {
        for depth in [16u16, 32] {
            let (_, p) = proposal(&doc_with(depth, 2, standard_resources(true)));
            let per = usize::from(depth / 8);
            let c = &p.raster_records.channels[0];
            assert_eq!(c.depth as u16, depth);
            assert_eq!(c.tiles[0].format, if depth == 16 { "coverage_u16le" } else { "coverage_f32le" });
            for (x, y) in [(0, 0), (100, 2), (259, 1)] {
                let mut want = sample(1, x, y, depth);
                want.reverse();
                assert_eq!(tile_sample(&p, "psd-channel-4", x, y, per), want, "{depth}-bit sample at ({x},{y}) little-endian");
            }
        }
    });
}

fn export(doc: &PsdDocument, p: &ImportProposal, records: &hsk_studio_folio::StudioRasterRecords, tiles: &[ChannelTilePayload]) -> (Vec<u8>, ExportReport) {
    let limits = Limits::default();
    let opts = ExportOptions { limits: &limits, tiles: None };
    export_psd_with_channels(doc, p, &p.snapshot, records, tiles, &opts).expect("export")
}

#[test]
fn unmodified_channels_reexport_byte_identically() {
    bounded_case("unmodified_channels_reexport_byte_identically", || {
        for depth in [8u16, 16, 32] {
            let bytes = doc_with(depth, 2, standard_resources(true));
            let (doc, p) = proposal(&bytes);
            let (out, report) = export(&doc, &p, &p.raster_records, &p.channel_tiles);
            assert_eq!(out, bytes, "{depth}-bit unmodified file re-emits identically");
            assert!(report.applied.is_empty() && report.loss.entries.is_empty(), "{report:?}");
        }
    });
}

#[test]
fn edited_channels_write_back_names_display_and_samples() {
    bounded_case("edited_channels_write_back_names_display_and_samples", || {
        let bytes = doc_with(8, 2, standard_resources(true));
        let (doc, p) = proposal(&bytes);
        let mut frames = Frames::default();
        let cancel = CancellationToken::default();
        // Rename the alpha, convert it to selected-area (inverts the samples), recolour the spot.
        let renamed = rename_channel(&p.raster_records, "psd-channel-4", "Hair mask", &cmd(), &cancel, &mut frames).result.unwrap();
        let converted = convert_channel_kind(&renamed, "psd-channel-4", StudioChannelKind::SelectedAreaAlpha, p.channel_tiles.as_slice(), &mut TileMint, &SelectionLimits::default(), &cmd(), &cancel, &mut frames).result.unwrap();
        let mut records = converted.records;
        records.channels[1].colour = StudioChannelColour::Rgb { r: 0.0, g: 0.5, b: 1.0 };
        records.channels[1].opacity = 75.0;
        let mut tiles = p.channel_tiles.clone();
        tiles.extend(converted.payloads);
        let (out, report) = export(&doc, &p, &records, &tiles);
        for field in ["channel_name", "channel_display", "channel_pixels"] {
            assert!(report.was_applied(field), "{field}: {report:?}");
        }
        assert!(!report.loss.entries.iter().any(|e| e.code == "merged_composite_stale"), "channel edits leave the composite as is");
        // Re-import the exported file: the edits are there, everything else as before.
        let (doc2, q) = proposal(&out);
        let a = q.raster_records.channel("psd-channel-4").unwrap();
        assert_eq!((a.name.as_str(), a.kind), ("Hair mask", StudioChannelKind::SelectedAreaAlpha));
        for (x, y) in [(0, 0), (200, 1), (259, 2)] {
            assert_eq!(tile_sample(&q, "psd-channel-4", x, y, 1), vec![255 - sample(1, x, y, 8)[0]]);
            assert_eq!(tile_sample(&q, "psd-channel-5", x, y, 1), sample(2, x, y, 8));
        }
        let s = q.raster_records.channel("psd-channel-5").unwrap();
        assert_eq!((s.kind, s.opacity, s.name.as_str()), (StudioChannelKind::SpotColour, 75.0, "Spot ink"));
        assert_eq!(s.colour, StudioChannelColour::Rgb { r: 0.0, g: 32768.0 / 65535.0, b: 1.0 });
        // The Pascal list was rewritten too; the transparency keeps its name and entry.
        assert_eq!(
            doc2.resources.alpha_names_pascal(8).unwrap().unwrap(),
            vec![b"Transparency".to_vec(), b"Hair mask".to_vec(), b"Spot\xe9".to_vec()]
        );
        let (_, entries) = doc2.resources.display_info(8).unwrap().unwrap();
        assert_eq!(entries[0], ChannelDisplayInfo { colour_space: 0, colour: [65535, 0, 0, 0], opacity: 100, mode: 1 });
        assert_eq!(entries[1].mode, 0);
        // Colour planes and the transparency plane keep their samples.
        let merged = |d: &PsdDocument| d.decode_merged(&Limits::default(), &cancel).unwrap().unwrap().planes;
        let (before, after) = (merged(&doc), merged(&doc2));
        assert_eq!(before[..4], after[..4]);
    });
}

#[test]
fn channel_edits_psd_cannot_take_are_typed_losses() {
    bounded_case("channel_edits_psd_cannot_take_are_typed_losses", || {
        let bytes = doc_with(8, 2, standard_resources(true));
        let (doc, p) = proposal(&bytes);
        let lost = |records: &hsk_studio_folio::StudioRasterRecords| {
            let (out, report) = export(&doc, &p, records, &p.channel_tiles);
            (out, report.loss.entries.iter().map(|e| e.code).collect::<Vec<_>>())
        };
        let mut removed = p.raster_records.clone();
        removed.channels.remove(1);
        assert_eq!(lost(&removed), (bytes.clone(), vec!["channel_removed_in_edit_not_exported"]));
        let mut added = p.raster_records.clone();
        let mut extra = added.channels[0].clone();
        extra.channel_id = "new".into();
        for t in &mut extra.tiles {
            t.layer_id = "new".into();
        }
        added.channels.push(extra);
        assert_eq!(lost(&added), (bytes.clone(), vec!["channel_added_in_edit_not_exported"]));
        let mut reordered = p.raster_records.clone();
        reordered.channels.swap(0, 1);
        assert_eq!(lost(&reordered), (bytes.clone(), vec!["channel_reorder_not_exported"]));
        let mut shown = p.raster_records.clone();
        shown.channels[0].visible = true;
        assert_eq!(lost(&shown), (bytes.clone(), vec!["channel_visibility_not_exported"]));
        // A tile the caller cannot supply is a typed error, never a partial write.
        let mut frames = Frames::default();
        let converted = convert_channel_kind(&p.raster_records, "psd-channel-4", StudioChannelKind::SelectedAreaAlpha, p.channel_tiles.as_slice(), &mut TileMint, &SelectionLimits::default(), &cmd(), &CancellationToken::default(), &mut frames).result.unwrap();
        let limits = Limits::default();
        let mut target = doc.clone();
        let err = apply_channel_edits(&mut target, &p, &converted.records, &p.channel_tiles, &limits).unwrap_err();
        assert_eq!(err, PsdError::Encode("channel_tile_missing"));
        assert_eq!(target, doc, "all or nothing");
    });
}

#[test]
fn hostile_channel_resources_are_bounded() {
    bounded_case("hostile_channel_resources_are_bounded", || {
        // dims_u32_max: a Unicode name claiming u32::MAX units is refused before allocation; the
        // channels fall back to the Pascal names and the resource is reported.
        let mut r = resource(1006, "", &pascal_list(&[b"Transparency", b"A", b"B"]));
        r.extend(resource(1045, "", &[be32(u32::MAX), vec![0, 65]].concat()));
        r.extend(resource(1053, "", &[be32(0), be32(5), be32(6)].concat()));
        let (_, p) = proposal(&doc_with(8, 2, r));
        assert_eq!(p.raster_records.channels.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["A", "B"]);
        assert!(codes(&p).contains(&("resource[1045]", "channel_resource_unreadable")));
        // Missing DisplayInfo: kinds default to masked-area alpha with a typed loss.
        assert!(codes(&p).contains(&("channel[1]/kind", "channel_display_info_absent")));
        // More identifiers than extra channels: refused before allocation, reported; without
        // usable identifiers only the first extra plane of a negative layer count is transparency,
        // so all three extra planes are channels here.
        let mut r = resource(1053, "", &vec![0u8; 57 * 4]);
        r.extend(resource(1006, "", &pascal_list(&[b"T", b"A", b"B"])));
        let (_, p) = proposal(&doc_with(8, 2, r));
        assert!(codes(&p).contains(&("resource[1053]", "channel_resource_unreadable")));
        assert_eq!(p.raster_records.channels.len(), 3);
        // A truncated 1006 and an unknown DisplayInfo colour space and mode.
        let mut r = resource(1006, "", &[9, b'x']);
        r.extend(resource(1053, "", &[be32(0), be32(5), be32(6)].concat()));
        r.extend(resource(1077, "", &display(&[(0, [0; 4], 100, 1), (9009, [1, 2, 3, 4], 0, 7), (0, [0; 4], 250, 2)])));
        let (_, p) = proposal(&doc_with(8, 2, r));
        let c = codes(&p);
        assert!(c.contains(&("resource[1006]", "channel_resource_unreadable")));
        assert!(c.contains(&("channel[1]/colour", "channel_display_colour_space_unmapped")));
        assert!(c.contains(&("channel[1]/kind", "channel_display_mode_unknown")));
        assert!(c.contains(&("channel[2]/opacity", "channel_opacity_clamped")));
        assert_eq!(p.raster_records.channels[0].name, "Alpha 2", "default name for extra plane 1");
        // An unknown colour space keeps its raw entry on export when the colour is not edited.
        let bytes = doc_with(8, 2, {
            let mut r = resource(1053, "", &[be32(0), be32(5), be32(6)].concat());
            r.extend(resource(1077, "", &display(&[(0, [0; 4], 100, 1), (9009, [1, 2, 3, 4], 0, 1), (0, [0; 4], 20, 2)])));
            r
        });
        let (doc, p) = proposal(&bytes);
        let mut records = p.raster_records.clone();
        records.channels[0].opacity = 60.0;
        let (out, _) = export(&doc, &p, &records, &p.channel_tiles);
        let (_, entries) = read(&out).resources.display_info(8).unwrap().unwrap();
        assert_eq!(entries[1], ChannelDisplayInfo { colour_space: 9009, colour: [1, 2, 3, 4], opacity: 60, mode: 1 });
    });
}

#[test]
fn channel_tiles_aggregate_over_budget_is_refused_before_allocation() {
    bounded_case("channel_tiles_aggregate_over_budget_is_refused_before_allocation", || {
        // aggregate_over_budget: one empty layer (no layer tiles), the merged planes fit (260 x 3 x 6
        // bytes) but the channel cells (2 channels x 2 cells x 65536 bytes) do not.
        let limits = Limits { max_decoded_bytes: 100_000, ..Limits::default() };
        let layered = doc_layered(8, 2, standard_resources(true), true);
        assert_eq!(proposal_with(&layered, &limits).err(), Some(PsdError::PixelLimit));
        let (_, p) = proposal_with(&layered, &Limits { max_decoded_bytes: 300_000, ..Limits::default() }).expect("fits");
        assert_eq!(p.raster_records.channels.len(), 2);
        let bytes = doc_with(8, 2, standard_resources(true));
        // A header beyond 56 channels never parses.
        let mut bad = bytes.clone();
        bad[12..14].copy_from_slice(&57u16.to_be_bytes());
        assert_eq!(read_psd(&bad, &Limits::default(), &CancellationToken::default()).err(), Some(PsdError::BadChannelCount));
        // Only the transparency: no channel records, the proposal is unchanged in shape.
        let mut r = resource(1006, "", &pascal_list(&[b"Transparency"]));
        r.extend(resource(1053, "", &be32(0)));
        let (_, p) = proposal(&doc_with(8, 0, r));
        assert!(p.raster_records.channels.is_empty() && p.channel_tiles.is_empty() && p.channel_map.is_empty());
    });
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

/// A corpus file: name, pinned byte length, expected channel names and kinds.
type Corpus = (&'static str, usize, &'static [(&'static str, StudioChannelKind)]);

#[test]
fn photoshop_saved_alpha_and_spot_channels_map_and_reexport() {
    bounded_case("photoshop_saved_alpha_and_spot_channels_map_and_reexport", || {
        // Byte lengths of the psd-tools files read on 2026-10-31 (the pin of this test). Expected
        // names and kinds from their resources 1006/1045/1077 (inspected 2026-10-31).
        let files: [Corpus; 2] = [
            ("cmyk-alpha-spot.psd", 2_286_497, &[("light teal", StudioChannelKind::SpotColour), ("Alpha 1", StudioChannelKind::MaskedAreaAlpha)]),
            ("cmyk-spot.psd", 402_214, &[("blue", StudioChannelKind::SpotColour), ("fluorescent pink", StudioChannelKind::SpotColour), ("yellow", StudioChannelKind::SpotColour)]),
        ];
        for (name, len, want) in files {
            let bytes = match corpus_file(&format!("psd-tools/psd_files/{name}")) {
                Ok(b) if b.len() == len => b,
                Ok(b) => {
                    println!("NOT_PROVEN: photoshop_saved_alpha_and_spot_channels_map_and_reexport {name} reason=length {} != {len}", b.len());
                    continue;
                }
                Err(reason) => {
                    println!("NOT_PROVEN: photoshop_saved_alpha_and_spot_channels_map_and_reexport {name} reason={reason}");
                    continue;
                }
            };
            let (doc, p) = proposal(&bytes);
            let got: Vec<(&str, StudioChannelKind)> = p.raster_records.channels.iter().map(|c| (c.name.as_str(), c.kind)).collect();
            assert_eq!(got, want.to_vec(), "{name}");
            let alpha = p.raster_records.channels.iter().find(|c| c.kind == StudioChannelKind::MaskedAreaAlpha);
            if let Some(alpha) = alpha {
                assert_eq!((alpha.opacity, &alpha.colour), (50.0, &StudioChannelColour::Rgb { r: 1.0, g: 0.0, b: 0.0 }));
            }
            let (out, report) = export(&doc, &p, &p.raster_records, &p.channel_tiles);
            assert!(out == bytes, "{name}: unmodified re-export is byte-identical");
            assert!(report.applied.is_empty(), "{name}: {report:?}");
            println!("PROVEN: photoshop_saved_alpha_and_spot_channels_map_and_reexport {name} channels={}", got.len());
        }
    });
}
