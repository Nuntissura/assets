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
use hsk_studio_folio::{Budget, Resolution, Resolver, StudioChannelColour, StudioChannelKind, StudioChannelRecord, StudioRasterRecords, TileRef};
use hsk_studio_interop_psd::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_pigment::selection::{CombineMode, CoveragePlane, SelectionLimits};
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
        // A channel added in edit is written (MT-038 AC-003, MT-30095); the cases PSD cannot carry
        // stay typed (`added_channels_psd_cannot_carry_stay_typed_and_the_rest_write_back`).
        let mut added = p.raster_records.clone();
        let mut extra = added.channels[0].clone();
        extra.channel_id = "new".into();
        for t in &mut extra.tiles {
            t.layer_id = "new".into();
        }
        added.channels.push(extra);
        let (out, report) = export(&doc, &p, &added, &p.channel_tiles);
        assert!(report.loss.entries.is_empty() && report.was_applied("channel_added"), "{report:?}");
        assert_eq!(proposal(&out).1.raster_records.channels.len(), 3);
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

/// R50 finding F-R50-5 (lane SMB-R50-FOLLOWUPS-1): the legacy 1006 Pascal name holds Latin-1 bytes
/// (a character above U+00FF is written `?`) and at most 255 of them. A rename it cannot carry
/// exactly is the typed loss `channel_name_transcoded` (as layer names record
/// `layer_name_truncated`); a rename it carries exactly records none, and the 1045 Unicode name keeps
/// the exact name either way.
#[test]
fn renamed_channel_names_the_pascal_list_cannot_carry_are_typed_losses() {
    bounded_case("renamed_channel_names_the_pascal_list_cannot_carry_are_typed_losses", || {
        let cancel = CancellationToken::default();
        let long = "a".repeat(256);
        for unicode_names in [true, false] {
            let (doc, p) = proposal(&doc_with(8, 2, standard_resources(unicode_names)));
            let renamed = |name: &str| {
                let mut frames = Frames::default();
                rename_channel(&p.raster_records, "psd-channel-4", name, &cmd(), &cancel, &mut frames).result.unwrap()
            };
            let transcoded = |report: &ExportReport| {
                report.loss.entries.iter().filter(|e| e.code == "channel_name_transcoded").map(|e| (e.path.clone(), e.kind)).collect::<Vec<_>>()
            };
            for (name, pascal, exact) in [
                ("Haar \u{6f22}", b"Haar ?".to_vec(), false),
                (long.as_str(), vec![b'a'; 255], false),
                ("Spot\u{e9} two", b"Spot\xe9 two".to_vec(), true),
            ] {
                let (out, report) = export(&doc, &p, &renamed(name), &p.channel_tiles);
                assert!(report.was_applied("channel_name"), "{name}: {report:?}");
                let want = if exact { Vec::new() } else { vec![("channel[1]/name".to_string(), LossKind::Transformed)] };
                assert_eq!(transcoded(&report), want, "{name} (1045 present: {unicode_names})");
                let (doc2, q) = proposal(&out);
                assert_eq!(doc2.resources.alpha_names_pascal(8).unwrap().unwrap()[1], pascal, "{name}: the 1006 bytes");
                // With 1045 the re-import reads the exact name; without it, the Pascal bytes as Latin-1.
                let read_back: String = if unicode_names { name.to_string() } else { pascal.iter().map(|&b| char::from(b)).collect() };
                assert_eq!(q.raster_records.channel("psd-channel-4").unwrap().name, read_back, "{name}");
            }
        }
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

// ---- Channels added in edit (lane SMB-R51-FOLLOWUPS-1; MT-038 AC-003, MT-30095: a channel saved from
// a selection is preserved in round-trippable interchange formats, STU-RAS-022) ----

/// The transparency (alpha id 0) and one alpha channel `Alpha One` (Photoshop default options: red,
/// 50 percent, mode 1).
fn one_alpha_resources() -> Vec<u8> {
    let mut r = resource(1006, "", &pascal_list(&[b"Transparency", b"Alpha 1"]));
    r.extend(resource(1045, "", &unicode_list(&["Transparency", "Alpha One"])));
    r.extend(resource(1053, "", &[be32(0), be32(5)].concat()));
    r.extend(resource(1077, "", &display(&[(0, [65535, 0, 0, 0], 100, 1), (0, [65535, 0, 0, 0], 50, 1)])));
    r
}
/// A layerless RGB file with its three colour planes only (no transparency, no channel resources).
fn doc_colour_only(depth: u16) -> Vec<u8> {
    let mut image = Vec::new();
    for k in 0..3 {
        for y in 0..H {
            for x in 0..W {
                image.extend(sample(9 + k, x, y, depth));
            }
        }
    }
    Doc {
        version: 1,
        channels: 3,
        height: H,
        width: W,
        depth,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: Vec::new(),
        image: Some((0, image)),
    }
    .bytes()
}
/// A selection over the whole raster with every coverage class: 0, full and spread values.
fn selection() -> CoveragePlane {
    let mut data: Vec<u16> = (0..H).flat_map(|y| (0..W).map(move |x| ((x * 2521 + y * 40503 + 17) % 65536) as u16)).collect();
    data[0] = 0;
    data[1] = u16::MAX;
    CoveragePlane { bounds: hsk_studio_pigment::Rect { x: 0, y: 0, width: W, height: H }, data }
}
fn new_channel(id: &str, name: &str, kind: StudioChannelKind, depth: u8) -> NewChannel {
    NewChannel { channel_id: id.into(), name: name.into(), kind, colour: StudioChannelColour::Rgb { r: 0.0, g: 0.0, b: 1.0 }, opacity: 40.0, depth }
}
/// Saves [`selection`] as a new channel on the 256-pixel grid of the importer.
fn save_new(records: &StudioRasterRecords, stored: &[ChannelTilePayload], profile: &str, n: NewChannel) -> ChannelEdit {
    let grid = ChannelGrid { width: W, height: H, tile_edge: 256, colour_profile_id: profile };
    let mut frames = Frames::default();
    let target = SaveTarget::New(n);
    save_selection(records, &selection(), &target, &grid, stored, &mut TileMint, &SelectionLimits::default(), &cmd(), &CancellationToken::default(), &mut frames)
        .result
        .unwrap()
}
/// What an interchange round trip keeps of a channel record: everything but the set-local id and the
/// artifact handles of the host (tiles by grid cell, format and content digest).
type Kept = (String, StudioChannelKind, bool, StudioChannelColour, f64, u8, u32, u32, Vec<(String, i64, i64, String, String)>);
fn kept(c: &StudioChannelRecord) -> Kept {
    let tiles = c.tiles.iter().map(|t| (t.object_key.clone(), t.column, t.row, t.format.clone(), t.content_digest.digest.clone())).collect();
    (c.name.clone(), c.kind, c.visible, c.colour.clone(), c.opacity, c.depth, c.width, c.height, tiles)
}
fn loss_codes(report: &ExportReport) -> Vec<(String, &'static str)> {
    report.loss.entries.iter().map(|e| (e.path.clone(), e.code)).collect()
}
const SCPF: &str = "SCPF-019abcde-0000-7000-8000-0000000000ff";

#[test]
fn channel_saved_from_a_selection_exports_and_reimports_beside_the_imported_one() {
    bounded_case("channel_saved_from_a_selection_exports_and_reimports_beside_the_imported_one", || {
        let cancel = CancellationToken::default();
        let units = Limits::default().max_name_units;
        for depth in [8u16, 16, 32] {
            let bytes = doc_with(depth, 1, one_alpha_resources());
            let (doc, p) = proposal(&bytes);
            assert_eq!(p.raster_records.channels.len(), 1, "{depth}: one imported alpha channel");
            let name = "Saved \u{6f22}";
            let saved = save_new(&p.raster_records, &p.channel_tiles, SCPF, new_channel("saved", name, StudioChannelKind::SelectedAreaAlpha, depth as u8));
            let mut tiles = p.channel_tiles.clone();
            tiles.extend(saved.payloads.clone());
            let (out, report) = export(&doc, &p, &saved.records, &tiles);
            assert!(report.was_applied("channel_added"), "{depth}: {report:?}");
            // The 1006 Pascal entry cannot carry the CJK character (written `?`, the existing rule);
            // the 1045 entry keeps the name exact. Nothing else is lost.
            assert_eq!(loss_codes(&report), [("channel[2]/name".to_string(), "channel_name_transcoded")], "{depth}");
            // Re-import: both channels, the imported one unchanged, the saved one as saved.
            let (doc2, q) = proposal(&out);
            assert_eq!(doc2.header.channels, 6, "{depth}: three colour planes, the transparency, two channels");
            let ids: Vec<&str> = q.raster_records.channels.iter().map(|c| c.channel_id.as_str()).collect();
            assert_eq!(ids, ["psd-channel-4", "psd-channel-5"], "{depth}");
            assert_eq!(kept(&q.raster_records.channels[0]), kept(&p.raster_records.channels[0]), "{depth}: the imported channel");
            assert_eq!(kept(&q.raster_records.channels[1]), kept(saved.records.channel("saved").unwrap()), "{depth}: the saved channel");
            // Pixels: the re-imported channel loads the selection identically at 16 and 32 bits and
            // quantised at 8 by the save rule (tone8 = (tone * 255 + 32767) / 65535, loaded tone8 * 257;
            // a selected-area alpha stores tone = full - coverage).
            let reloaded = load_selection(&q.raster_records.channels[1], q.channel_tiles.as_slice(), None, CombineMode::Replace, &SelectionLimits::default(), &cancel).unwrap();
            let plane = selection();
            for y in 0..i64::from(H) {
                for x in 0..i64::from(W) {
                    let c = plane.at(x, y);
                    let want = if depth == 8 {
                        let tone = u32::from(u16::MAX - c);
                        let t8 = (tone * 255 + 32767) / 65535;
                        u16::MAX - (t8 * 257) as u16
                    } else {
                        c
                    };
                    assert_eq!(reloaded.at(x, y), want, "{depth}-bit coverage at ({x},{y})");
                }
            }
            // Resources 1006, 1045 and 1077 list both channels after the transparency; 1053 gives
            // the new channel its own non-zero identifier.
            assert_eq!(
                doc2.resources.alpha_names_pascal(8).unwrap().unwrap(),
                vec![b"Transparency".to_vec(), b"Alpha 1".to_vec(), b"Saved ?".to_vec()]
            );
            assert_eq!(doc2.resources.alpha_names_unicode(8, units).unwrap().unwrap(), ["Transparency", "Alpha One", name]);
            let (_, entries) = doc2.resources.display_info(8).unwrap().unwrap();
            assert_eq!(
                entries,
                [
                    ChannelDisplayInfo { colour_space: 0, colour: [65535, 0, 0, 0], opacity: 100, mode: 1 },
                    ChannelDisplayInfo { colour_space: 0, colour: [65535, 0, 0, 0], opacity: 50, mode: 1 },
                    ChannelDisplayInfo { colour_space: 0, colour: [0, 0, 65535, 0], opacity: 40, mode: 0 },
                ]
            );
            let ids = doc2.resources.alpha_identifiers(8).unwrap().unwrap();
            assert_eq!((ids.len(), ids[0], ids[1]), (3, 0, 5), "{depth}");
            assert!(ids[2] != 0 && ids[2] != 5, "{depth}: a fresh identifier {ids:?}");
            // The existing planes keep their samples; the new plane is last.
            let merged = |d: &PsdDocument| d.decode_merged(&Limits::default(), &cancel).unwrap().unwrap().planes;
            let (before, after) = (merged(&doc), merged(&doc2));
            assert_eq!((before.len(), after.len()), (5, 6));
            assert_eq!(before[..], after[..5]);
            // The exported file is stable: an unmodified re-export is byte-identical.
            let (again, r2) = export(&doc2, &q, &q.raster_records, &q.channel_tiles);
            assert!(again == out && r2.applied.is_empty() && r2.loss.entries.is_empty(), "{depth}: {r2:?}");
        }
    });
}

#[test]
fn added_channels_over_the_header_channel_limit_are_typed_losses() {
    bounded_case("added_channels_over_the_header_channel_limit_are_typed_losses", || {
        // Six header channels (colour, transparency, two channels); a PSD header holds at most 56, so
        // 50 more fit and the 51st is refused typed.
        let bytes = doc_with(8, 2, standard_resources(true));
        let (doc, p) = proposal(&bytes);
        let (mut records, mut tiles) = (p.raster_records.clone(), p.channel_tiles.clone());
        for k in 0..51 {
            let edit = save_new(&records, &tiles, SCPF, new_channel(&format!("saved-{k}"), &format!("Saved {k}"), StudioChannelKind::MaskedAreaAlpha, 8));
            records = edit.records;
            tiles.extend(edit.payloads);
        }
        let (out, report) = export(&doc, &p, &records, &tiles);
        assert_eq!(report.applied.iter().filter(|(_, f)| *f == "channel_added").count(), 50);
        assert_eq!(loss_codes(&report), [("channels/saved-50".to_string(), "channel_added_in_edit_not_exported")]);
        let (doc2, q) = proposal(&out);
        assert_eq!(doc2.header.channels, 56);
        assert_eq!(q.raster_records.channels.len(), 52);
        assert_eq!(q.raster_records.channels.last().map(|c| c.name.as_str()), Some("Saved 49"));
        assert_eq!(doc2.resources.alpha_names_pascal(56).unwrap().unwrap().len(), 53);
        assert_eq!(doc2.resources.display_info(56).unwrap().unwrap().1.len(), 53);
    });
}

#[test]
fn added_channels_psd_cannot_carry_stay_typed_and_the_rest_write_back() {
    bounded_case("added_channels_psd_cannot_carry_stay_typed_and_the_rest_write_back", || {
        let bytes = doc_with(8, 1, one_alpha_resources());
        let (doc, p) = proposal(&bytes);
        let with = |n: NewChannel| {
            let edit = save_new(&p.raster_records, &p.channel_tiles, SCPF, n);
            let mut tiles = p.channel_tiles.clone();
            tiles.extend(edit.payloads);
            (edit.records, tiles)
        };
        // A depth other than the file depth: every merged plane shares the header depth, so PSD
        // cannot carry it; refused typed, nothing written.
        let (records, tiles) = with(new_channel("deep", "Deep", StudioChannelKind::MaskedAreaAlpha, 16));
        let (out, report) = export(&doc, &p, &records, &tiles);
        assert_eq!(loss_codes(&report), [("channels/deep".to_string(), "channel_added_in_edit_not_exported")]);
        assert!(out == bytes && report.applied.is_empty(), "{report:?}");
        // Placed before an imported channel: written after the existing channels (file order),
        // the order change is typed.
        let (mut records, tiles) = with(new_channel("front", "Front", StudioChannelKind::MaskedAreaAlpha, 8));
        records.channels.rotate_right(1);
        let (out, report) = export(&doc, &p, &records, &tiles);
        assert!(report.was_applied("channel_added"), "{report:?}");
        assert_eq!(loss_codes(&report), [("channels".to_string(), "channel_reorder_not_exported")]);
        let names: Vec<String> = proposal(&out).1.raster_records.channels.iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, ["Alpha One", "Front"]);
        // Shown: written; PSD stores no channel visibility here (it re-imports hidden), typed.
        let (mut records, tiles) = with(new_channel("shown", "Shown", StudioChannelKind::SpotColour, 8));
        records.channels[1].visible = true;
        let (out, report) = export(&doc, &p, &records, &tiles);
        assert!(report.was_applied("channel_added"), "{report:?}");
        assert_eq!(loss_codes(&report), [("channel[2]/visible".to_string(), "channel_visibility_not_exported")]);
        let q = proposal(&out).1;
        assert_eq!((q.raster_records.channels[1].kind, q.raster_records.channels[1].visible), (StudioChannelKind::SpotColour, false));
        // A colour-only file (no extra planes, no channel resources): resources 1006, 1045 and 1077
        // are created for the new channel, 1053 stays absent, and the record re-imports as saved.
        for depth in [8u16, 16, 32] {
            let bytes = doc_colour_only(depth);
            let (doc, p) = proposal(&bytes);
            assert!(p.raster_records.channels.is_empty());
            let edit = save_new(&p.raster_records, &p.channel_tiles, SCPF, new_channel("first", "First", StudioChannelKind::MaskedAreaAlpha, depth as u8));
            let (out, report) = export(&doc, &p, &edit.records, &edit.payloads);
            assert!(report.was_applied("channel_added") && report.loss.entries.is_empty(), "{depth}: {report:?}");
            let (doc2, q) = proposal(&out);
            assert_eq!(doc2.header.channels, 4);
            assert_eq!(q.raster_records.channels.len(), 1);
            assert_eq!(kept(&q.raster_records.channels[0]), kept(&edit.records.channels[0]), "{depth}");
            assert_eq!(doc2.resources.alpha_names_pascal(4).unwrap().unwrap(), vec![b"First".to_vec()]);
            assert_eq!(doc2.resources.alpha_names_unicode(4, Limits::default().max_name_units).unwrap().unwrap(), ["First"]);
            assert_eq!(doc2.resources.display_info(4).unwrap().unwrap().1.len(), 1);
            assert_eq!(doc2.resources.alpha_identifiers(4).unwrap(), None);
            assert!(!codes(&q).iter().any(|(_, c)| c.starts_with("channel_")), "{depth}: {:?}", codes(&q));
        }
    });
}

// ---- R52 finding F-R52-3 (lane SMB-R52-FOLLOWUPS-1): the three `added_channel_fits` branches the
// tests above did not reach, and the opacity the integral 1077 entry cannot hold (STU-RAS-043) ----

/// Exports `edited` over `bytes` and asserts that PSD refused channel `id` with the typed loss
/// `channel_added_in_edit_not_exported` and left the file byte-identical.
fn assert_added_refused(label: &str, bytes: &[u8], doc: &PsdDocument, p: &ImportProposal, edited: &StudioRasterRecords, tiles: &[ChannelTilePayload], id: &str) {
    let (out, report) = export(doc, p, edited, tiles);
    assert_eq!(loss_codes(&report), [(format!("channels/{id}"), "channel_added_in_edit_not_exported")], "{label}");
    assert_eq!(report.loss.entries[0].kind, LossKind::Unsupported, "{label}");
    assert!(report.applied.is_empty(), "{label}: {report:?}");
    assert!(out == bytes, "{label}: the file is unchanged");
}

/// A layerless file of `planes` raw planes at `depth` in colour mode `mode` (no resources); `image`
/// false leaves the merged image data section out.
fn doc_mode(mode: u16, depth: u16, planes: u16, image: bool, layer_section: Vec<u8>) -> Vec<u8> {
    let row = if depth == 1 { W.div_ceil(8) as usize } else { W as usize * usize::from(depth / 8) };
    let data: Vec<u8> = (0..usize::from(planes) * H as usize * row).map(|i| (i * 13 % 251) as u8).collect();
    Doc { version: 1, channels: planes, height: H, width: W, depth, mode, color_mode_data: Vec::new(), resources: Vec::new(), layer_section, image: image.then_some((0, data)) }
        .bytes()
}

#[test]
fn added_channel_in_a_multichannel_or_one_bit_file_is_a_typed_loss() {
    bounded_case("added_channel_in_a_multichannel_or_one_bit_file_is_a_typed_loss", || {
        // Multichannel (mode 7) at 8 bits: every plane is a colour plane, so there is no extra plane
        // to append; extent, depth, image data and transparency would all fit.
        let bytes = doc_mode(7, 8, 3, true, Vec::new());
        let (doc, p) = proposal(&bytes);
        assert_eq!(doc.header.color_mode, ColorMode::Multichannel);
        assert!(p.raster_records.channels.is_empty());
        let edit = save_new(&p.raster_records, &p.channel_tiles, SCPF, new_channel("multi", "Multi", StudioChannelKind::MaskedAreaAlpha, 8));
        assert_added_refused("multichannel", &bytes, &doc, &p, &edit.records, &edit.payloads, "multi");
        // Bitmap (mode 0) at 1 bit: the extent and depth match the file (a 1-bit record), but PSD
        // extra planes are written at 8, 16 or 32 bits only. A 1-bit record is outside Folio's
        // 8/16/32 contract (a hostile record): any valid depth fails the depth-equality branch first.
        let bytes = doc_mode(0, 1, 1, true, Vec::new());
        let (doc, p) = proposal(&bytes);
        assert_eq!(doc.header.depth, 1);
        let mut edit = save_new(&p.raster_records, &p.channel_tiles, SCPF, new_channel("bit", "Bit", StudioChannelKind::MaskedAreaAlpha, 8));
        edit.records.channels[0].depth = 1;
        assert_added_refused("one-bit", &bytes, &doc, &p, &edit.records, &edit.payloads, "bit");
    });
}

#[test]
fn added_channel_without_merged_image_data_is_a_typed_loss() {
    bounded_case("added_channel_without_merged_image_data_is_a_typed_loss", || {
        // An RGB file whose merged image data section is absent: no plane list to append to.
        let bytes = doc_mode(3, 8, 3, false, Vec::new());
        let (doc, p) = proposal(&bytes);
        assert!(doc.image_data.is_none());
        let edit = save_new(&p.raster_records, &p.channel_tiles, SCPF, new_channel("orphan", "Orphan", StudioChannelKind::MaskedAreaAlpha, 8));
        assert_added_refused("no image data", &bytes, &doc, &p, &edit.records, &edit.payloads, "orphan");
    });
}

#[test]
fn added_channel_as_extra_plane_zero_of_a_merged_transparency_file_is_a_typed_loss() {
    bounded_case("added_channel_as_extra_plane_zero_of_a_merged_transparency_file_is_a_typed_loss", || {
        // Three header channels (colour only) and a negative layer count: the first extra plane of
        // this file reads back as the merged transparency, so a channel written there would not
        // re-import as a channel.
        let layered = |negative_count: bool| {
            let mut empty = TLayer::new("empty", [0, 0, 0, 0]);
            empty.channels = empty_channels(&[-1, 0, 1, 2]);
            doc_mode(3, 8, 3, true, layer_section(&layer_info(&[empty], false, negative_count), &[], false))
        };
        let bytes = layered(true);
        let (doc, p) = proposal(&bytes);
        assert!(doc.layer_section.layer_info.as_ref().is_some_and(|l| l.merged_alpha));
        let edit = save_new(&p.raster_records, &p.channel_tiles, SCPF, new_channel("zero", "Zero", StudioChannelKind::MaskedAreaAlpha, 8));
        assert_added_refused("merged transparency", &bytes, &doc, &p, &edit.records, &edit.payloads, "zero");
        // Control: the same file with a positive layer count takes the channel as extra plane 0.
        let (doc, p) = proposal(&layered(false));
        let edit = save_new(&p.raster_records, &p.channel_tiles, SCPF, new_channel("zero", "Zero", StudioChannelKind::MaskedAreaAlpha, 8));
        let (out, report) = export(&doc, &p, &edit.records, &edit.payloads);
        assert!(report.was_applied("channel_added") && report.loss.entries.is_empty(), "{report:?}");
        assert_eq!(proposal(&out).1.raster_records.channels.len(), 1);
    });
}

#[test]
fn channel_opacity_the_display_entry_cannot_hold_is_a_typed_loss() {
    bounded_case("channel_opacity_the_display_entry_cannot_hold_is_a_typed_loss", || {
        let losses = |report: &ExportReport| report.loss.entries.iter().map(|e| (e.path.clone(), e.code, e.kind)).collect::<Vec<_>>();
        let at = |code: &'static str| ("channel[1]/opacity".to_string(), code, LossKind::Transformed);
        // An imported channel edited to an opacity the integral 1077 percent cannot hold: written
        // rounded or clamped, typed Transformed; an integral in-range value records nothing.
        let bytes = doc_with(8, 2, standard_resources(true));
        let (doc, p) = proposal(&bytes);
        for (opacity, stored, want) in [
            (62.4, 62u16, vec![at("channel_opacity_rounded")]),
            (62.5, 63, vec![at("channel_opacity_rounded")]),
            (60.0, 60, Vec::new()),
            (120.0, 100, vec![at("channel_opacity_clamped")]),
            (-0.5, 0, vec![at("channel_opacity_rounded"), at("channel_opacity_clamped")]),
        ] {
            let mut records = p.raster_records.clone();
            records.channels[0].opacity = opacity;
            let (out, report) = export(&doc, &p, &records, &p.channel_tiles);
            assert!(report.was_applied("channel_display"), "{opacity}: {report:?}");
            assert_eq!(losses(&report), want, "{opacity}");
            let (doc2, q) = proposal(&out);
            assert_eq!(doc2.resources.display_info(8).unwrap().unwrap().1[1].opacity, stored, "{opacity}");
            assert_eq!(q.raster_records.channels[0].opacity, f64::from(stored), "{opacity}");
        }
        // A channel saved from a selection with a fractional opacity: appended to 1077 rounded, typed.
        let bytes = doc_with(8, 1, one_alpha_resources());
        let (doc, p) = proposal(&bytes);
        let saved = |opacity: f64| {
            let mut edit = save_new(&p.raster_records, &p.channel_tiles, SCPF, new_channel("saved", "Saved", StudioChannelKind::MaskedAreaAlpha, 8));
            edit.records.channels[1].opacity = opacity;
            let mut tiles = p.channel_tiles.clone();
            tiles.extend(edit.payloads);
            export(&doc, &p, &edit.records, &tiles)
        };
        let (out, report) = saved(33.5);
        assert!(report.was_applied("channel_added"), "{report:?}");
        assert_eq!(losses(&report), [("channel[2]/opacity".to_string(), "channel_opacity_rounded", LossKind::Transformed)]);
        let (doc2, q) = proposal(&out);
        assert_eq!(doc2.resources.display_info(8).unwrap().unwrap().1[2].opacity, 34);
        assert_eq!(q.raster_records.channels[1].opacity, 34.0);
        let (_, report) = saved(40.0);
        assert!(report.was_applied("channel_added") && report.loss.entries.is_empty(), "{report:?}");
    });
}
