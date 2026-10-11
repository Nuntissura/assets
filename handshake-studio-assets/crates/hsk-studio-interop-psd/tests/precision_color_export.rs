//! Slice 3: full-precision 16/32-bit tiles, colour modes routed through Prism (or typed loss), and
//! export of an edited Folio document. All inputs are small synthetic files.

mod common;

use common::*;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{Budget, Isolation, Payload, ProfileBinding, Resolution, Resolver, StudioDocument, TileRef, validate_document};
use hsk_studio_interop_psd::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_prism::Prism;

const SRGB_V4: &[u8] = include_bytes!("fixtures/icc/sRGB-v4.icc");
const SRGB_LINEAR_V4: &[u8] = include_bytes!("fixtures/icc/sRGB-linear-v4.icc");

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
struct Open;
impl Resolver for Open {
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
#[derive(Default)]
struct Frames(Vec<DeliveryClass>);
impl SinkPort for Frames {
    fn try_send(&mut self, class: DeliveryClass, _: &[u8]) -> std::result::Result<(), DeliveryError> {
        self.0.push(class);
        Ok(())
    }
}

fn budget() -> Budget {
    Budget { input_bytes: 8 << 20, payload_bytes: 4 << 20, ..Budget::default() }
}
fn proposal_of(bytes: &[u8], scale: SixteenBitScale, converter: Option<&mut dyn IccConverter>) -> (PsdDocument, ImportProposal) {
    let limits = Limits::default();
    let doc = read(bytes);
    let mut cfg = ProposalConfig::new(&limits, budget(), &Open);
    cfg.scale = scale;
    let proposal = to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0), converter, &CancellationToken::default())
        .expect("proposal");
    (doc, proposal)
}
fn be16s(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}
fn bef32(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}
fn f32_at(buf: &[u8], i: usize) -> f32 {
    f32::from_le_bytes([buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2], buf[i * 4 + 3]])
}
fn eotf(x: f64) -> f64 {
    let a = x.abs();
    (if a <= 0.04045 { a / 12.92 } else { ((a + 0.055) / 1.055).powf(2.4) }).copysign(x)
}
fn close(actual: f32, expected: f64, tolerance: f64) -> bool {
    (f64::from(actual) - expected).abs() <= tolerance * expected.abs().max(1.0)
}
/// One layer covering the whole canvas, raw merged image; `layer_channels` are `(id, payload)`.
fn flat_doc(mode: u16, channels: u16, depth: u16, (width, height): (u32, u32), resources: Vec<u8>, layer_channels: Vec<(i16, Vec<u8>)>, merged: Vec<u8>) -> Vec<u8> {
    let mut layer = TLayer::new("layer", [0, 0, height as i32, width as i32]);
    layer.channels = layer_channels.into_iter().map(|(id, data)| (id, 0, data)).collect();
    let info = layer_info(&[layer], false, false);
    Doc {
        version: 1,
        channels,
        height,
        width,
        depth,
        mode,
        color_mode_data: Vec::new(),
        resources,
        layer_section: layer_section(&info, &[], false),
        image: Some((0, merged)),
    }
    .bytes()
}

/// 16-bit samples keep full precision under both scale interpretations, and so do float samples;
/// tiles survive an edit and export without 8-bit quantization.
#[test]
fn sixteen_and_thirty_two_bit_tiles_keep_full_precision_and_round_trip() {
    bounded_case("sixteen_and_thirty_two_bit_tiles_keep_full_precision_and_round_trip", || {
        let limits = Limits::default();
        let cancel = CancellationToken::default();
        let (r, g, b, a) = ([0x0000u16, 0xFFFF], [0x0001u16, 0x8000], [0x7FFFu16, 0x8001], [0xFFFFu16, 0x4000]);
        let merged = [be16s(&r), be16s(&g), be16s(&b)].concat();
        let bytes = flat_doc(3, 3, 16, (2, 1), Vec::new(), vec![(0, be16s(&r)), (1, be16s(&g)), (2, be16s(&b)), (-1, be16s(&a))], merged);
        for scale in [SixteenBitScale::Full65535, SixteenBitScale::Photoshop32768] {
            let d = scale.divisor();
            let (doc, p) = proposal_of(&bytes, scale, None);
            assert_eq!(p.provenance.sixteen_bit_scale, scale);
            assert!(p.loss.has_code(match scale {
                SixteenBitScale::Full65535 => "sixteen_bit_scale_assumed_65535",
                SixteenBitScale::Photoshop32768 => "sixteen_bit_scale_assumed_32768",
            }));
            let tile = &p.tiles[0];
            for (i, (rv, gv, bv)) in [(r[0], g[0], b[0]), (r[1], g[1], b[1])].into_iter().enumerate() {
                for (k, raw) in [rv, gv, bv].into_iter().enumerate() {
                    let expected = eotf(f64::from(raw) / d);
                    assert!(close(f32_at(&tile.colour, i * 3 + k), expected, 1e-6), "{scale:?} px{i} ch{k}");
                }
            }
            let (g0, b0, b1) = (f32_at(&tile.colour, 1), f32_at(&tile.colour, 2), f32_at(&tile.colour, 5));
            assert!(g0 > 0.0 && g0 < 1e-4, "raw 1 is not quantized to 0 as an 8-bit pipeline would");
            assert!(b1 > b0, "raw 0x7FFF and 0x8001 stay distinct (both are 128 in 8 bits)");
            assert_eq!(f32_at(&tile.alpha, 0), 1.0, "alpha above 1.0 is clamped");
            assert!(close(f32_at(&tile.alpha, 1), 16384.0 / d, 1e-6));

            // numeric round trip of the conversion pair, both interpretations
            for raw in [0u32, 1, 2, 255, 256, 32767, 32768, 32769, 65534, 65535] {
                let unit = sample_to_unit(raw, 16, scale);
                assert_eq!(unit_to_sample(unit, 16, scale), raw, "{scale:?} {raw}");
                assert_eq!(unit_to_sample(linear_to_srgb(srgb_to_linear(unit)), 16, scale), raw, "{scale:?} through the transfer {raw}");
            }

            // edit one pixel in the tile, export, re-read: the edit lands at full precision
            let mut tiles = p.tiles.clone();
            tiles[0].colour[4..8].copy_from_slice(&(eotf(0.25) as f32).to_le_bytes());
            let opts = ExportOptions { limits: &limits, tiles: Some(&tiles) };
            let (out, report) = export_psd(&doc, &p, &p.snapshot, &opts).expect("export");
            assert!(report.was_applied("pixels"));
            assert_eq!(report.loss.entries.iter().map(|e| e.code).collect::<Vec<_>>(), ["merged_composite_stale"], "a pixel edit makes the stored composite stale");
            let back = read(&out);
            let channel = |id: i16| -> Vec<u16> {
                let plane = back.layers()[0].decode_channel(id, &back.header, &limits, &cancel).unwrap().unwrap();
                plane.bytes.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
            };
            assert_eq!(channel(0), [0x0000, 0xFFFF]);
            assert_eq!(channel(1), [(0.25 * d).round() as u16, 0x8000]);
            assert_eq!(channel(2), [0x7FFF, 0x8001]);
            let alpha0 = if scale == SixteenBitScale::Full65535 { 0xFFFF } else { 0x8000 };
            assert_eq!(channel(-1), [alpha0, 0x4000], "alpha above 1.0 stays clamped on export");
            // untouched tiles leave the file byte-identical
            let same = ExportOptions { limits: &limits, tiles: Some(&p.tiles) };
            assert_eq!(export_psd(&doc, &p, &p.snapshot, &same).unwrap().0, bytes);
        }

        // 32-bit floats: taken as stored (assumed linear light), including values above 1 and below 0
        let (fr, fg, fb) = ([0.25f32, 2.0], [0.0f32, 1.0], [0.5f32, -0.5]);
        let merged = [bef32(&fr), bef32(&fg), bef32(&fb)].concat();
        let bytes = flat_doc(3, 3, 32, (2, 1), Vec::new(), vec![(0, bef32(&fr)), (1, bef32(&fg)), (2, bef32(&fb))], merged);
        let (doc, p) = proposal_of(&bytes, SixteenBitScale::default(), None);
        let tile = &p.tiles[0];
        assert_eq!([f32_at(&tile.colour, 0), f32_at(&tile.colour, 3)], [0.25, 2.0]);
        assert_eq!([f32_at(&tile.colour, 1), f32_at(&tile.colour, 4)], [0.0, 1.0]);
        assert_eq!([f32_at(&tile.colour, 2), f32_at(&tile.colour, 5)], [0.5, -0.5]);
        assert!(p.loss.has_code("float32_assumed_linear_light") && p.provenance.assumed_profile);
        let merged = doc.decode_merged(&limits, &cancel).unwrap().unwrap();
        assert_eq!(merged.to_linear_rgba_f32(SixteenBitScale::default()).unwrap(), [0.25, 0.0, 0.5, 1.0, 2.0, 1.0, -0.5, 1.0]);
        let mut tiles = p.tiles.clone();
        tiles[0].colour[0..4].copy_from_slice(&0.75f32.to_le_bytes());
        let (out, _) = export_psd(&doc, &p, &p.snapshot, &ExportOptions { limits: &limits, tiles: Some(&tiles) }).unwrap();
        let back = read(&out);
        let floats = |id: i16| -> Vec<f32> {
            let plane = back.layers()[0].decode_channel(id, &back.header, &limits, &cancel).unwrap().unwrap();
            plane.bytes.chunks(4).map(|c| f32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect()
        };
        assert_eq!((floats(0), floats(1), floats(2)), (vec![0.75, 2.0], vec![0.0, 1.0], vec![0.5, -0.5]));
    });
}

fn prism_route() -> PrismRoute {
    PrismRoute {
        actor: ActorContext::new("a", "b", "c", "d", "e", "f").unwrap(),
        source_profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000101").unwrap(),
        destination_profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000102").unwrap(),
        destination_icc: SRGB_LINEAR_V4.to_vec(),
        resource: DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000103").unwrap(),
        correlation_id: 9,
        revision: 1,
    }
}
/// Imports through a Prism-backed converter; returns the proposal and the number of frames Prism sent.
fn with_prism(bytes: &[u8]) -> (ImportProposal, usize) {
    let mut sink = Frames::default();
    let route = prism_route();
    let prism = Prism::new(route.actor.clone(), 4).unwrap();
    let mut converter = PrismConverter::new(prism, route, &mut sink);
    let (_, proposal) = proposal_of(bytes, SixteenBitScale::default(), Some(&mut converter));
    drop(converter);
    (proposal, sink.0.len())
}
fn reason_of(p: &ImportProposal) -> String {
    match &p.snapshot.document().layers[0].payload {
        Payload::Unsupported { reason, .. } => reason.clone(),
        other => panic!("expected an unsupported payload, got {other:?}"),
    }
}

/// RGB with an embedded profile Prism accepts is converted by Prism; every other mode or refusal
/// is a typed loss with a stated reason, never a silent fallback to a wrong colour.
#[test]
fn embedded_profiles_go_through_prism_and_refusals_are_typed_losses() {
    bounded_case("embedded_profiles_go_through_prism_and_refusals_are_typed_losses", || {
        let (r, g, b) = ([64u8, 200], [128u8, 0], [255u8, 32]);
        let rgb = |icc: Option<&[u8]>| {
            let resources = icc.map_or_else(Vec::new, |bytes| resource(1039, "", bytes));
            let merged = [r.to_vec(), g.to_vec(), b.to_vec()].concat();
            flat_doc(3, 3, 8, (1, 2), resources, vec![(0, r.to_vec()), (1, g.to_vec()), (2, b.to_vec())], merged)
        };
        // the layer is one pixel wide and two high: pixel i sits at tile cell i * 256
        let cell = |i: usize| i * 256 * 3;
        let expect_assumed = |p: &ImportProposal| {
            for (i, raw) in [r[0], r[1]].into_iter().enumerate() {
                assert!(close(f32_at(&p.tiles[0].colour, cell(i)), eotf(f64::from(raw) / 255.0), 1e-6));
            }
        };

        // accepted: converted into the converter's working space
        let (p, frames) = with_prism(&rgb(Some(SRGB_V4)));
        assert!(p.provenance.icc_converted && !p.provenance.assumed_profile);
        assert!(p.loss.has_code("icc_profile_applied_by_converter"));
        assert!(!p.loss.has_code("icc_profile_not_applied") && !p.loss.has_code("profile_assumed_srgb"));
        let destination = prism_route().destination_profile_id.as_str().to_owned();
        assert_eq!(p.snapshot.document().working_space_rgb, ProfileBinding::Assigned(destination.clone()));
        let Payload::Raster { tiles } = &p.snapshot.document().layers[0].payload else { panic!("raster") };
        assert_eq!(tiles[0].colour_profile_id, destination);
        for (i, (rv, gv, bv)) in [(r[0], g[0], b[0]), (r[1], g[1], b[1])].into_iter().enumerate() {
            for (k, raw) in [rv, gv, bv].into_iter().enumerate() {
                assert!(close(f32_at(&p.tiles[0].colour, cell(i) + k), eotf(f64::from(raw) / 255.0), 5e-4), "px{i} ch{k}");
            }
        }
        assert!(frames >= 1, "Prism delivered its terminal frames to the caller's sink");

        // no converter: sRGB assumed, profile reported as not applied
        let (_, p) = proposal_of(&rgb(Some(SRGB_V4)), SixteenBitScale::default(), None);
        assert!(p.loss.has_code("icc_profile_not_applied") && p.loss.has_code("profile_assumed_srgb") && !p.provenance.icc_converted);
        expect_assumed(&p);

        // refused (70 000 bytes that are no ICC profile; RGB now goes through the wide path, whose cap is
        // 2 MiB, so the refusal is the framing check, not the legacy 64 KiB cap): assumed sRGB plus the refusal code
        let (p, _) = with_prism(&rgb(Some(&vec![7u8; 70_000])));
        assert!(p.loss.has_code("icc_refused_malformed_profile") && p.provenance.assumed_profile && !p.provenance.icc_converted);
        expect_assumed(&p);
        // over the wide path's 2 MiB cap: the size refusal itself is typed
        let (p, _) = with_prism(&rgb(Some(&vec![7u8; (2 << 20) + 1])));
        assert!(p.loss.has_code("icc_refused_profile_limit") && p.provenance.assumed_profile && !p.provenance.icc_converted);

        // CMYK: a profile the wide path refuses (here malformed) or no profile at all leaves the
        // layers unmapped with a typed reason; no default CMYK profile is invented
        let planes: Vec<Vec<u8>> = (0..4u8).map(|c| vec![c * 10 + 1, c * 10 + 2]).collect();
        let cmyk = |icc: Option<&[u8]>| {
            let resources = icc.map_or_else(Vec::new, |bytes| resource(1039, "", bytes));
            flat_doc(4, 4, 8, (1, 2), resources, planes.iter().cloned().enumerate().map(|(i, d)| (i as i16, d)).collect(), planes.concat())
        };
        let (p, _) = with_prism(&cmyk(Some(&vec![7u8; 70_000])));
        assert!(p.loss.has_code("icc_refused_malformed_profile") && !p.loss.has_code("cmyk_display_conversion_unavailable"));
        assert_eq!(reason_of(&p), "cmyk_profile_refused");
        assert!(p.tiles.is_empty() && p.snapshot.document().colour_mode == 3);
        let (p, _) = with_prism(&cmyk(None));
        assert!(p.loss.has_code("cmyk_profile_missing") && !p.loss.has_code("cmyk_display_conversion_unavailable"));
        assert_eq!(reason_of(&p), "cmyk_profile_missing");
        // without any converter the conversion itself is unavailable
        let (_, p) = proposal_of(&cmyk(None), SixteenBitScale::default(), None);
        assert!(p.loss.has_code("cmyk_profile_missing") && p.loss.has_code("cmyk_display_conversion_unavailable"));
        assert_eq!(reason_of(&p), "cmyk_display_conversion_unavailable");

        // Lab is PCS-native: it maps through the converter, and stays unmapped without one
        let lab = flat_doc(9, 3, 8, (1, 2), Vec::new(), (0..3i16).map(|i| (i, vec![100, 128])).collect(), vec![100, 128, 100, 128, 100, 128]);
        let (p, _) = with_prism(&lab);
        assert!(p.loss.has_code("lab_channels_assumed_icc_encoding") && !p.loss.has_code("lab_display_conversion_unavailable"));
        assert!(!p.tiles.is_empty() && p.provenance.display.is_some());
        let (_, p) = proposal_of(&lab, SixteenBitScale::default(), None);
        assert!(p.loss.has_code("lab_display_conversion_unavailable"));
        assert_eq!(reason_of(&p), "lab_display_conversion_unavailable");

        // gray with a profile Prism cannot read: replicated and assumed, with the refusal recorded
        let gray = flat_doc(1, 1, 8, (1, 2), resource(1039, "", &[0u8; 200]), vec![(0, r.to_vec())], r.to_vec());
        let (p, _) = with_prism(&gray);
        assert!(p.loss.has_code("icc_refused_malformed_profile") && p.loss.has_code("gray_replicated_to_rgb"));
        assert_eq!([f32_at(&p.tiles[0].colour, 0), f32_at(&p.tiles[0].colour, 1)], [f32_at(&p.tiles[0].colour, 1), f32_at(&p.tiles[0].colour, 2)]);
        assert!(close(f32_at(&p.tiles[0].colour, 0), eotf(64.0 / 255.0), 1e-6));
    });
}

fn export_fixture() -> Vec<u8> {
    let mut base = TLayer::new("base", [0, 0, 1, 2]);
    base.channels = vec![(0, 1, rle(&[&[10, 20]], false)), (1, 1, rle(&[&[30, 30]], false)), (2, 1, rle(&[&[40, 50]], false))];
    base.blocks = vec![layer_block(b"ABCD", &[7, 7], false), layer_block(b"TySh", &[0, 1, 2, 3], false)];
    let mut adjustment = TLayer::new("lvl", [0, 0, 0, 0]);
    adjustment.channels = empty_channels(&[0, 1, 2]);
    adjustment.blocks = vec![layer_block(b"levl", &[0; 4], false)];
    let mut divider = TLayer::new("</Layer group>", [0, 0, 0, 0]);
    divider.channels = empty_channels(&[0, 1, 2]);
    divider.blocks = vec![layer_block(b"lsct", &be32(3), false)];
    let mut inner = TLayer::new("inner", [0, 0, 1, 2]);
    inner.channels = vec![(0, 0, vec![1, 2]), (1, 0, vec![3, 4]), (2, 0, vec![5, 6]), (-2, 0, vec![255, 0])];
    inner.mask = {
        let mut m: Vec<u8> = [0i32, 0, 1, 2].iter().flat_map(|v| v.to_be_bytes()).collect();
        m.extend([255, 1, 0, 0]); // default colour, flags (relative to layer), padding
        m
    };
    let mut folder = TLayer::new("Grp", [0, 0, 0, 0]);
    folder.key = *b"pass";
    folder.channels = empty_channels(&[0, 1, 2]);
    let mut lsct = be32(1);
    lsct.extend_from_slice(b"8BIM");
    lsct.extend_from_slice(b"pass");
    folder.blocks = vec![layer_block(b"lsct", &lsct, false)];
    let info = layer_info(&[base, adjustment, divider, inner, folder], false, false);
    let mut version_info = be32(1);
    version_info.push(1);
    version_info.extend(unicode("Adobe Photoshop"));
    version_info.extend(unicode("Adobe Photoshop"));
    version_info.extend(be32(1));
    let mut resources = resource(1005, "", &[1, 2, 3]);
    resources.extend(resource(1039, "", SRGB_V4));
    resources.extend(resource(1057, "", &version_info));
    Doc {
        version: 1,
        channels: 3,
        height: 1,
        width: 2,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources,
        layer_section: layer_section(&info, &[], false),
        image: Some((0, vec![10, 20, 30, 30, 40, 50])),
    }
    .bytes()
}

fn layer_named<'a>(d: &'a StudioDocument, name: &str) -> &'a hsk_studio_folio::StudioLayer {
    d.layers.iter().find(|l| l.name == name).unwrap_or_else(|| panic!("no layer {name}"))
}

/// Import, edit the Folio document, export, import again: the supported properties are equal, the
/// unrepresentable edit is a typed loss, and everything the importer does not own is byte-identical.
#[test]
fn export_of_an_edited_folio_document_round_trips_supported_fields_and_preserves_the_rest() {
    bounded_case("export_of_an_edited_folio_document_round_trips_supported_fields_and_preserves_the_rest", || {
        let limits = Limits::default();
        let cancel = CancellationToken::default();
        let bytes = export_fixture();
        let (doc, p) = proposal_of(&bytes, SixteenBitScale::default(), None);
        let opts = ExportOptions { limits: &limits, tiles: None };

        // no edits: the original bytes come back and nothing is reported
        let (same, report) = export_psd(&doc, &p, &p.snapshot, &opts).unwrap();
        assert_eq!(same, bytes);
        assert!(report.applied.is_empty() && report.loss.entries.is_empty());

        // edit the Folio document
        let mut d = p.snapshot.document().clone();
        d.revision = 2;
        for l in &mut d.layers {
            let mut a = l.effective_appearance();
            match l.name.as_str() {
                "base" => {
                    l.name = "Base2".into();
                    l.visible = false;
                    (a.opacity, a.fill_opacity, a.blend_mode, a.clipped) = (40.0, 50.2, 9, true);
                }
                "inner" => {
                    (a.opacity, a.blend_mode) = (60.0, 33); // 33 (average) has no PSD key
                    let mask = a.mask.as_mut().expect("imported mask");
                    (mask.enabled, mask.linked) = (false, false);
                }
                "Grp" => {
                    l.name = "Grp2".into();
                    (a.blend_mode, a.isolation) = (5, Isolation::Isolated);
                }
                _ => continue,
            }
            l.appearance = Some(a);
        }
        let edited = validate_document(d, budget(), &cancel, &Open).expect("the edit is a valid Folio document");
        let (out, report) = export_psd(&doc, &p, &edited, &opts).unwrap();
        for field in ["name", "visible", "opacity", "fill_opacity", "blend_mode", "clipped", "mask_flags"] {
            assert!(report.was_applied(field), "{field} was written back");
        }
        let lost: Vec<(&str, &str)> = report.loss.entries.iter().map(|e| (e.path.as_str(), e.code)).collect();
        assert_eq!(lost, [("layer[3]/blend", "blend_mode_not_representable_in_psd"), ("merged_image", "merged_composite_stale")]);

        // import the exported file again: supported fields equal the edit
        let (back, p2) = proposal_of(&out, SixteenBitScale::default(), None);
        let (before, after) = (edited.document(), p2.snapshot.document());
        for name in ["Base2", "Grp2"] {
            assert_eq!(layer_named(before, name).appearance, layer_named(after, name).appearance, "{name}");
            assert_eq!(layer_named(before, name).visible, layer_named(after, name).visible, "{name}");
        }
        assert!(!layer_named(after, "Base2").visible);
        let (inner_before, inner_after) = (layer_named(before, "inner").effective_appearance(), layer_named(after, "inner").effective_appearance());
        assert_eq!((inner_after.opacity, inner_after.blend_mode), (60.0, 2), "opacity exported; the unrepresentable blend kept the original");
        let (mb, ma) = (inner_before.mask.unwrap(), inner_after.mask.unwrap());
        assert_eq!((ma.enabled, ma.linked), (mb.enabled, mb.linked));
        assert_eq!(ma.tiles.iter().map(|t| &t.content_digest).collect::<Vec<_>>(), mb.tiles.iter().map(|t| &t.content_digest).collect::<Vec<_>>());

        // unsupported and untouched content is the original bytes
        let (old, new) = (doc.layers(), back.layers());
        assert_eq!(new[0].channels, old[0].channels, "RLE channel payloads untouched");
        for key in [b"ABCD", b"TySh"] {
            assert_eq!(new[0].block(key), old[0].block(key));
        }
        assert_eq!((new[1].clone(), new[2].clone()), (old[1].clone(), old[2].clone()), "adjustment layer and group divider");
        assert_eq!(new[3].channels, old[3].channels);
        assert_eq!(new[3].blend_key, *b"norm");
        assert_eq!(new[3].mask().unwrap().flags, 0x02, "disabled, not linked");
        assert_eq!((new[0].blend_key, new[0].clipping, new[0].opacity, new[0].fill_opacity()), (*b"scrn", 1, 102, Some(128)));
        assert_eq!((new[4].blend_key, new[4].group_blend_key()), (*b"mul ", Some(*b"mul ")));
        assert_eq!(doc.merged_status(&Limits::default()), MergedStatus::Real);
        assert_eq!(back.merged_status(&Limits::default()), MergedStatus::Placeholder, "the edit made the stored composite stale");
        let mut stale_cleared = doc.resources.clone();
        stale_cleared.blocks.iter_mut().find(|b| b.id == RESOURCE_VERSION_INFO).unwrap().data[4] = 0;
        assert_eq!(back.resources, stale_cleared, "ICC profile and every other resource; only hasRealMergedData changed");
        assert_eq!(back.image_data, doc.image_data);
        assert_eq!(back.layer_section.blocks, doc.layer_section.blocks);
    });
}

// ---- CMYK / Lab / gray display conversion through the Prism wide path -------------------------

/// Imports through a Prism-backed converter with explicit display settings.
fn prism_import(bytes: &[u8], scale: SixteenBitScale, intent: Option<RenderingIntent>, bpc: Option<bool>) -> (PsdDocument, ImportProposal) {
    let limits = Limits::default();
    let doc = read(bytes);
    let mut cfg = ProposalConfig::new(&limits, budget(), &Open);
    cfg.scale = scale;
    cfg.rendering_intent = intent;
    cfg.black_point_compensation = bpc;
    let route = prism_route();
    let prism = Prism::new(route.actor.clone(), 4).unwrap();
    let mut sink = Frames::default();
    let mut converter = PrismConverter::new(prism, route, &mut sink);
    let proposal = to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0), Some(&mut converter), &CancellationToken::default())
        .expect("proposal");
    (doc, proposal)
}
const RELATIVE: Option<RenderingIntent> = Some(RenderingIntent::RelativeColorimetric);
fn assert_tile_matches(tile: &TileBlob, pixel: usize, want: [f64; 3], tolerance: f64, what: &str) {
    for (k, w) in want.iter().enumerate() {
        let got = f32_at(&tile.colour, pixel * 3 + k);
        assert!((f64::from(got) - w.clamp(0.0, 1.0)).abs() <= tolerance, "{what} px{pixel} ch{k}: {got} vs {}", w.clamp(0.0, 1.0));
        assert!((0.0..=1.0).contains(&got), "{what} px{pixel} ch{k} leaves 0..=1 after the recorded clamp");
    }
}
fn channel_plane(doc: &PsdDocument, id: i16) -> Vec<u8> {
    doc.layers()[0].decode_channel(id, &doc.header, &Limits::default(), &CancellationToken::default()).unwrap().unwrap().bytes
}
/// Unedited export is byte-identical; an edited projection tile is a typed loss and the original
/// channel bytes survive.
fn assert_projection_survives_export(doc: &PsdDocument, p: &ImportProposal, original: &[u8], loss_code: &str) {
    let limits = Limits::default();
    let same = ExportOptions { limits: &limits, tiles: Some(&p.tiles) };
    assert_eq!(export_psd(doc, p, &p.snapshot, &same).unwrap().0, original, "unedited export is byte-identical");
    let mut edited = p.tiles.clone();
    edited[0].colour[0..4].copy_from_slice(&0.123f32.to_le_bytes());
    let (out, report) = export_psd(doc, p, &p.snapshot, &ExportOptions { limits: &limits, tiles: Some(&edited) }).unwrap();
    assert_eq!(out, original, "a display projection is not a rewrite: original channels stay as read");
    assert!(report.loss.has_code(loss_code) && !report.was_applied("pixels"));
}

/// CMYK with an embedded profile: inverted storage is undone, the output is checked against the
/// closed form of the synthetic printer, out-of-gamut values are clamped with counts and loss entries,
/// intent and black point compensation are recorded as assumptions unless configured, and the
/// original channels are never rewritten.
#[test]
fn cmyk_display_conversion_is_a_clamped_projection_and_keeps_the_original_channels() {
    bounded_case("cmyk_display_conversion_is_a_clamped_projection_and_keeps_the_original_channels", || {
        let profile = icc::cmyk_profile(&[b"A2B0", b"A2B1", b"A2B2"]);
        // inks (c, m, y, k): white, K only, cyan only, cyan+magenta; stored inverted (255 = no ink)
        let inks: [[f64; 4]; 4] = [[0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 0.0], [1.0, 1.0, 0.0, 0.0]];
        let planes: Vec<Vec<u8>> = (0..4).map(|c| inks.iter().map(|ink| (255.0 * (1.0 - ink[c])).round() as u8).collect()).collect();
        let bytes = flat_doc(4, 4, 8, (4, 1), resource(1039, "", &profile), planes.iter().cloned().enumerate().map(|(i, d)| (i as i16, d)).collect(), planes.concat());

        // configured relative colorimetric, no BPC: closed form
        let (doc, p) = prism_import(&bytes, SixteenBitScale::default(), RELATIVE, Some(false));
        let tile = &p.tiles[0];
        let wants: Vec<[f64; 3]> = inks
            .iter()
            .map(|i| {
                let (l, a, b) = icc::printer_lab(i[0], i[1], i[2], i[3]);
                icc::lab_to_linear_srgb(l, a, b)
            })
            .collect();
        for (i, want) in wants.iter().enumerate() {
            assert_tile_matches(tile, i, *want, 4e-3, "cmyk");
        }
        let d = p.provenance.display.as_ref().expect("display conversion record");
        assert_eq!((d.source_space, d.intent, d.intent_assumed), ("cmyk", RenderingIntent::RelativeColorimetric, false));
        assert_eq!((d.black_point_compensation, d.black_point_compensation_assumed, d.pixels_converted), (false, false, 4));
        let overshoot = |w: &[f64; 3]| w.iter().map(|v| (-v).max(v - 1.0).max(0.0)).fold(0.0, f64::max);
        let expected_clipped = wants.iter().filter(|w| overshoot(w) > f64::from(GAMUT_CLAMP_TOLERANCE)).count() as u64;
        assert!(expected_clipped >= 2, "cyan and cyan+magenta are outside sRGB by construction");
        assert_eq!(d.pixels_out_of_gamut, expected_clipped);
        assert!(d.pixels_clamped >= d.pixels_out_of_gamut && d.channels_clamped[0] >= 1);
        assert!((d.max_overshoot - wants.iter().map(overshoot).fold(0.0, f64::max)).abs() < 5e-3);
        assert_eq!((d.converter_clip.method, d.converter_clip.pixels_clipped), (ClipMethodKind::LinearDevice, expected_clipped));
        assert!(p.provenance.icc_converted && !p.provenance.assumed_profile);
        for code in ["cmyk_channels_assumed_inverted", "icc_profile_applied_by_converter", "display_out_of_gamut_clamped"] {
            assert!(p.loss.has_code(code), "{code}");
        }
        for code in ["color_mode_unmapped", "bit_depth_unmapped", "rendering_intent_assumed_perceptual", "black_point_compensation_assumed_on", "icc_profile_not_applied"] {
            assert!(!p.loss.has_code(code), "{code}");
        }
        assert!(matches!(p.snapshot.document().layers[0].payload, Payload::Raster { .. }));
        assert_eq!(channel_plane(&doc, 3), planes[3], "stored (inverted) K plane is untouched");
        assert_projection_survives_export(&doc, &p, &bytes, "pixel_export_after_icc_conversion_unsupported");

        // defaults: perceptual and BPC on are ASSUMED and recorded; BPC deepens K-only black, white stays
        let (_, q) = prism_import(&bytes, SixteenBitScale::default(), None, None);
        let d = q.provenance.display.as_ref().unwrap();
        assert_eq!((d.intent, d.intent_assumed, d.black_point_compensation, d.black_point_compensation_assumed), (RenderingIntent::Perceptual, true, true, true));
        assert!(q.loss.has_code("rendering_intent_assumed_perceptual") && q.loss.has_code("black_point_compensation_assumed_on"));
        for k in 0..3 {
            assert!(close(f32_at(&q.tiles[0].colour, k), 1.0, 3e-3), "white stays white with BPC");
        }
        // ISO 18619 on a matrix destination: source black = darkest vertex (all inks, L* 5), destination black 0,
        // so Y' = 1 - (1 - Y) / (1 - Y(5)); the K-only black (L* 40) is neutral and lands on that value
        let (y_source_black, y_k) = (5.0 * 27.0 / 24389.0, (56.0f64 / 116.0).powi(3));
        let with_bpc = 1.0 - (1.0 - y_k) / (1.0 - y_source_black);
        for k in 3..6 {
            assert!(close(f32_at(&q.tiles[0].colour, k), with_bpc, 1.5e-3), "BPC is applied exactly once: ch{k} {} vs {with_bpc}", f32_at(&q.tiles[0].colour, k));
        }
        assert!(with_bpc < y_k - 4e-3, "the compensation is measurable at this fixture's black depth");
    });
}

/// 16-bit CMYK honours the configured 16-bit scale and clamps inputs that leave range (recorded);
/// Lab (8 and 16 bit) needs no profile and maps through the PCS-native path with explicit decode.
#[test]
fn sixteen_bit_cmyk_and_lab_use_the_scale_and_the_pcs_native_path() {
    bounded_case("sixteen_bit_cmyk_and_lab_use_the_scale_and_the_pcs_native_path", || {
        // CMYK 16-bit, scale 32768: raw 32768 = no ink, raw 16384 = half ink; raw 40000 exceeds 1.0
        let profile = icc::cmyk_profile(&[b"A2B0", b"A2B1", b"A2B2"]);
        let (c, m, y, k) = ([32768u16; 3], [32768u16; 3], [32768u16; 3], [16384u16, 32768, 40000]);
        let planes = [be16s(&c), be16s(&m), be16s(&y), be16s(&k)];
        let bytes = flat_doc(4, 4, 16, (3, 1), resource(1039, "", &profile), planes.iter().cloned().enumerate().map(|(i, d)| (i as i16, d)).collect(), planes.concat());
        let (doc, p) = prism_import(&bytes, SixteenBitScale::Photoshop32768, RELATIVE, Some(false));
        let lab_srgb = |(l, a, b): (f64, f64, f64)| icc::lab_to_linear_srgb(l, a, b);
        let half_k = lab_srgb(icc::printer_lab(0.0, 0.0, 0.0, 0.5));
        let paper = lab_srgb(icc::printer_lab(0.0, 0.0, 0.0, 0.0));
        assert_tile_matches(&p.tiles[0], 0, half_k, 4e-3, "cmyk16 half K");
        assert_tile_matches(&p.tiles[0], 1, paper, 4e-3, "cmyk16 paper");
        assert_tile_matches(&p.tiles[0], 2, paper, 4e-3, "cmyk16 over-range ink clamps to no ink");
        assert!(p.loss.has_code("sixteen_bit_scale_assumed_32768") && p.loss.has_code("icc_input_range_clamped"));
        assert!(p.provenance.display.as_ref().unwrap().input_range_clamped);
        assert_eq!(channel_plane(&doc, 3), planes[3], "16-bit K plane bytes are untouched");
        assert_projection_survives_export(&doc, &p, &bytes, "pixel_export_after_icc_conversion_unsupported");

        // Lab 16-bit (full scale): L = raw/65535*100, a/b = raw/65535*255 - 128; raw 32896 is the neutral axis
        let lab_raw = |l: f64, a: f64, b: f64| [(l / 100.0 * 65535.0).round() as u16, ((a + 128.0) * 257.0) as u16, ((b + 128.0) * 257.0) as u16];
        let px16 = [lab_raw(100.0, 0.0, 0.0), lab_raw(0.0, 0.0, 0.0), lab_raw(60.0, 20.0, -25.0)];
        let lab_planes: Vec<Vec<u8>> = (0..3).map(|c| be16s(&px16.map(|p| p[c]))).collect();
        let bytes16 = flat_doc(9, 3, 16, (3, 1), Vec::new(), lab_planes.iter().cloned().enumerate().map(|(i, d)| (i as i16, d)).collect(), lab_planes.concat());
        let (doc16, p16) = prism_import(&bytes16, SixteenBitScale::Full65535, None, None);
        for (i, raw) in px16.iter().enumerate() {
            let (l, a, b) = (f64::from(raw[0]) / 65535.0 * 100.0, f64::from(raw[1]) / 65535.0 * 255.0 - 128.0, f64::from(raw[2]) / 65535.0 * 255.0 - 128.0);
            assert_tile_matches(&p16.tiles[0], i, icc::lab_to_linear_srgb(l, a, b), 3e-3, "lab16");
        }
        let d = p16.provenance.display.as_ref().unwrap();
        assert_eq!((d.source_space, d.pixels_converted), ("lab", 3));
        assert!(!p16.provenance.icc_converted && !p16.provenance.assumed_profile, "PCS-native: no profile, no sRGB assumption");
        assert!(p16.loss.has_code("lab_channels_assumed_icc_encoding") && !p16.loss.has_code("profile_assumed_srgb"));
        assert_projection_survives_export(&doc16, &p16, &bytes16, "pixel_export_after_display_conversion_unsupported");

        // Lab 8-bit: L = v*100/255, a/b = v - 128
        let px8 = [[255u8, 128, 128], [0, 128, 128], [128, 168, 88]];
        let planes8: Vec<Vec<u8>> = (0..3).map(|c| px8.iter().map(|p| p[c]).collect()).collect();
        let bytes8 = flat_doc(9, 3, 8, (3, 1), Vec::new(), planes8.iter().cloned().enumerate().map(|(i, d)| (i as i16, d)).collect(), planes8.concat());
        let (_, p8) = prism_import(&bytes8, SixteenBitScale::default(), None, None);
        for (i, raw) in px8.iter().enumerate() {
            let lab = (f64::from(raw[0]) * 100.0 / 255.0, f64::from(raw[1]) - 128.0, f64::from(raw[2]) - 128.0);
            assert_tile_matches(&p8.tiles[0], i, icc::lab_to_linear_srgb(lab.0, lab.1, lab.2), 3e-3, "lab8");
        }
    });
}

/// Gray with a `kTRC` profile converts through the wide path (gamma 2 here, closed form); a gray
/// document without a profile keeps its recorded sRGB assumption and a profile of the wrong device
/// space is a typed note.
#[test]
fn gray_ktrc_profile_converts_and_other_gray_cases_stay_typed() {
    bounded_case("gray_ktrc_profile_converts_and_other_gray_cases_stay_typed", || {
        let profile = icc::gray_profile(2.0);
        let g8 = [0u8, 64, 200, 255];
        let bytes = flat_doc(1, 1, 8, (4, 1), resource(1039, "", &profile), vec![(0, g8.to_vec())], g8.to_vec());
        let (doc, p) = prism_import(&bytes, SixteenBitScale::default(), None, None);
        for (i, raw) in g8.iter().enumerate() {
            let v = (f64::from(*raw) / 255.0).powi(2);
            assert_tile_matches(&p.tiles[0], i, [v; 3], 3e-3, "gray8");
        }
        let d = p.provenance.display.as_ref().unwrap();
        assert_eq!((d.source_space, d.intent_assumed), ("gray", true));
        assert!(p.provenance.icc_converted && !p.provenance.assumed_profile);
        assert!(p.loss.has_code("icc_profile_applied_by_converter") && p.loss.has_code("gray_replicated_to_rgb"));
        assert!(!p.loss.has_code("profile_assumed_srgb") && !p.loss.has_code("icc_profile_not_applied"));
        assert_eq!(channel_plane(&doc, 0), g8.to_vec());
        assert_projection_survives_export(&doc, &p, &bytes, "pixel_export_after_icc_conversion_unsupported");

        // 16-bit gray, full scale
        let g16 = [0u16, 16384, 65535];
        let bytes16 = flat_doc(1, 1, 16, (3, 1), resource(1039, "", &profile), vec![(0, be16s(&g16))], be16s(&g16));
        let (_, p16) = prism_import(&bytes16, SixteenBitScale::Full65535, None, None);
        for (i, raw) in g16.iter().enumerate() {
            assert_tile_matches(&p16.tiles[0], i, [(f64::from(*raw) / 65535.0).powi(2); 3], 3e-3, "gray16");
        }

        // no profile: unchanged recorded assumption (sRGB), no display conversion record
        let bare = flat_doc(1, 1, 8, (4, 1), Vec::new(), vec![(0, g8.to_vec())], g8.to_vec());
        let (_, p) = prism_import(&bare, SixteenBitScale::default(), None, None);
        assert!(p.loss.has_code("profile_assumed_srgb") && p.provenance.display.is_none() && p.provenance.assumed_profile);
        assert!(close(f32_at(&p.tiles[0].colour, 3), eotf(64.0 / 255.0), 1e-6));

        // an RGB profile on a gray document does not apply: typed note, assumption stays
        let rgb_on_gray = flat_doc(1, 1, 8, (4, 1), resource(1039, "", SRGB_V4), vec![(0, g8.to_vec())], g8.to_vec());
        let (_, p) = prism_import(&rgb_on_gray, SixteenBitScale::default(), None, None);
        assert!(p.loss.has_code("icc_profile_space_mismatch") && p.loss.has_code("profile_assumed_srgb") && p.provenance.display.is_none());
    });
}

/// Every way CMYK cannot be displayed is a typed unmapped layer with a specific reason: wrong device
/// space, a profile lacking the chosen intent's table (no other intent is substituted), and a
/// converter without a wide path. No default CMYK profile is ever used.
#[test]
fn cmyk_without_a_usable_profile_is_a_typed_unmapped_layer_never_defaulted() {
    bounded_case("cmyk_without_a_usable_profile_is_a_typed_unmapped_layer_never_defaulted", || {
        let planes: Vec<Vec<u8>> = (0..4u8).map(|c| vec![200 - c, 100 + c]).collect();
        let doc_with = |icc: &[u8]| {
            flat_doc(4, 4, 8, (2, 1), resource(1039, "", icc), planes.iter().cloned().enumerate().map(|(i, d)| (i as i16, d)).collect(), planes.concat())
        };
        // RGB profile on a CMYK document
        let (_, p) = prism_import(&doc_with(SRGB_V4), SixteenBitScale::default(), None, None);
        assert_eq!(reason_of(&p), "cmyk_profile_space_mismatch");
        assert!(p.loss.has_code("icc_profile_space_mismatch") && p.tiles.is_empty() && p.provenance.display.is_none());
        // a colorimetric-only printer profile cannot serve the assumed perceptual intent
        let colorimetric_only = icc::cmyk_profile(&[b"A2B1"]);
        let (_, p) = prism_import(&doc_with(&colorimetric_only), SixteenBitScale::default(), None, None);
        assert_eq!(reason_of(&p), "cmyk_intent_table_missing");
        assert!(p.loss.has_code("icc_refused_missing_intent_table") && p.tiles.is_empty());
        // ...but it maps once the colorimetric intent is configured
        let (_, p) = prism_import(&doc_with(&colorimetric_only), SixteenBitScale::default(), RELATIVE, None);
        assert!(!p.tiles.is_empty() && p.provenance.display.is_some() && !p.loss.has_code("rendering_intent_assumed_perceptual"));
        // a converter without a wide path: conversion unavailable, legacy refusal still recorded
        struct LegacyOnly;
        impl IccConverter for LegacyOnly {
            fn destination_profile_id(&self) -> String {
                "SCPF-019abcde-0000-7000-8000-0000000000aa".into()
            }
            fn inspect(&mut self, _: &[u8], _: &CancellationToken) -> std::result::Result<(), IccRefusal> {
                Err(IccRefusal("unsupported_profile"))
            }
            fn convert_rgb(&mut self, _: &[u8], _: &[[f32; 3]], _: &CancellationToken) -> std::result::Result<Vec<[f32; 3]>, IccRefusal> {
                Err(IccRefusal("unsupported_profile"))
            }
        }
        let bytes = doc_with(&icc::cmyk_profile(&[b"A2B0", b"A2B1", b"A2B2"]));
        let (_, p) = proposal_of(&bytes, SixteenBitScale::default(), Some(&mut LegacyOnly));
        assert_eq!(reason_of(&p), "cmyk_display_conversion_unavailable");
        assert!(p.loss.has_code("cmyk_display_conversion_unavailable") && p.loss.has_code("icc_refused_unsupported_profile"));
    });
}

// ---- R3 review regressions (CONFIRMED findings) ------------------------------------------------

fn edited_snapshot(p: &ImportProposal, edit: impl Fn(&mut hsk_studio_folio::StudioLayer)) -> hsk_studio_folio::Snapshot {
    let mut d = p.snapshot.document().clone();
    d.revision = 2;
    for l in &mut d.layers {
        edit(l);
    }
    validate_document(d, budget(), &CancellationToken::default(), &Open).expect("the edit is a valid Folio document")
}

/// R3 finding 1: tile blobs are caller data (public fields). A truncated, emptied or oversized blob is
/// a typed error from the public export, never an out-of-bounds panic; intact blobs still export.
#[test]
fn export_with_malformed_tile_blobs_is_a_typed_error_never_a_panic() {
    bounded_case("export_with_malformed_tile_blobs_is_a_typed_error_never_a_panic", || {
        let limits = Limits::default();
        let bytes = export_fixture();
        let (doc, p) = proposal_of(&bytes, SixteenBitScale::default(), None);
        assert!(!p.tiles.is_empty());
        for damage in 0..5 {
            let mut tiles = p.tiles.clone();
            match damage {
                0 => tiles[0].colour.truncate(4),
                1 => tiles[0].alpha.truncate(3),
                2 => tiles[0].colour.clear(),
                3 => tiles[0].alpha.clear(),
                _ => tiles[0].colour.push(0),
            }
            let result = export_psd(&doc, &p, &p.snapshot, &ExportOptions { limits: &limits, tiles: Some(&tiles) });
            assert_eq!(result.unwrap_err(), PsdError::Encode("tile_blob_size"), "damage {damage}");
        }
        let mut edited = p.tiles.clone();
        edited[0].colour[0..4].copy_from_slice(&0.5f32.to_le_bytes());
        let (_, report) = export_psd(&doc, &p, &p.snapshot, &ExportOptions { limits: &limits, tiles: Some(&edited) }).unwrap();
        assert!(report.was_applied("pixels"), "a well-formed edit still exports");
    });
}

/// R3 finding 2: after an edit that changes the picture the stored merged composite no longer matches
/// the layers. It is never left marked real: `hasRealMergedData` is cleared (no oracle is served) and
/// `merged_composite_stale` is reported; its bytes stay as read. A name-only edit changes nothing.
#[test]
fn edits_that_change_the_picture_mark_the_stored_composite_stale_and_name_edits_do_not() {
    bounded_case("edits_that_change_the_picture_mark_the_stored_composite_stale_and_name_edits_do_not", || {
        let limits = Limits::default();
        let cancel = CancellationToken::default();
        let opts = ExportOptions { limits: &limits, tiles: None };
        let bytes = export_fixture();
        let (doc, p) = proposal_of(&bytes, SixteenBitScale::default(), None);
        assert_eq!(doc.merged_status(&limits), MergedStatus::Real);
        assert!(doc.merged_oracle_rgba8(&limits, &cancel).unwrap().is_some(), "the unedited composite is a usable oracle");

        // name only: the picture is unchanged, the flag stays
        let renamed = edited_snapshot(&p, |l| {
            if l.name == "base" {
                l.name = "renamed".into();
            }
        });
        let (out, report) = export_psd(&doc, &p, &renamed, &opts).unwrap();
        assert!(report.was_applied("name") && !report.loss.has_code("merged_composite_stale"));
        let back = read(&out);
        assert_eq!(back.merged_status(&limits), MergedStatus::Real);
        assert!(back.merged_oracle_rgba8(&limits, &cancel).unwrap().is_some());

        // visibility (or any appearance or pixel edit): stale
        let hidden = edited_snapshot(&p, |l| {
            if l.name == "base" {
                l.visible = false;
            }
        });
        let (out, report) = export_psd(&doc, &p, &hidden, &opts).unwrap();
        assert!(report.loss.has_code("merged_composite_stale"));
        assert!(report.applied.iter().any(|(path, field)| path == "resource[1057]" && *field == "has_real_merged_data_cleared"));
        let back = read(&out);
        assert_eq!(back.merged_status(&limits), MergedStatus::Placeholder);
        assert_eq!(back.merged_oracle_rgba8(&limits, &cancel).unwrap(), None, "a stale composite is never served as an oracle");
        assert_eq!(back.image_data, doc.image_data, "the composite bytes themselves are untouched");
        assert!(back.decode_merged(&limits, &cancel).unwrap().is_some());

        // a file that never said (no resource 1057) still reports the staleness
        let bare = flat_doc(3, 3, 8, (2, 1), Vec::new(), vec![(0, vec![10, 20]), (1, vec![30, 40]), (2, vec![50, 60])], vec![10, 20, 30, 40, 50, 60]);
        let (bare_doc, bare_p) = proposal_of(&bare, SixteenBitScale::default(), None);
        let hidden = edited_snapshot(&bare_p, |l| l.visible = false);
        let (_, report) = export_psd(&bare_doc, &bare_p, &hidden, &opts).unwrap();
        assert!(report.loss.has_code("merged_composite_stale") && !report.applied.iter().any(|(path, _)| path == "resource[1057]"));
        // nothing applied, nothing stale
        let (same, report) = export_psd(&bare_doc, &bare_p, &bare_p.snapshot, &opts).unwrap();
        assert_eq!(same, bare);
        assert!(report.loss.entries.is_empty());
    });
}

// ---- R5 review regressions (SMB-INTEROP-PSD-WIDE CONFIRMED + class sweep + carried PLAUSIBLE) ---------

/// A gray profile's black is not zero by definition: a `kTRC` table can lift it (TRC(0) = v0 > 0). Then
/// ISO 18619 maps the source black onto the destination black (sRGB-linear, 0). Oracle, derived on paper:
/// TRC(g) = v0 + (1 - v0) g, so with compensation Y' = (Y - v0) / (1 - v0) = g on the neutral axis, and
/// without it Y = v0 + (1 - v0) g. The old importer dropped the flag for every gray profile (no loss entry
/// about the lifted black, output = the uncompensated values).
#[test]
fn gray_black_point_compensation_is_applied_when_the_profile_black_is_not_zero() {
    bounded_case("gray_black_point_compensation_is_applied_when_the_profile_black_is_not_zero", || {
        let v0 = 6554.0 / 65535.0;
        let g8 = [0u8, 64, 200, 255];
        let unit = |raw: u8| f64::from(raw) / 255.0;
        let doc_of = |profile: &[u8]| flat_doc(1, 1, 8, (4, 1), resource(1039, "", profile), vec![(0, g8.to_vec())], g8.to_vec());
        let lifted = doc_of(&icc::gray_profile_table(&[6554, 65535]));

        // configured on: the lifted black lands on the destination black, the neutral axis is preserved
        let (_, on) = prism_import(&lifted, SixteenBitScale::default(), RELATIVE, Some(true));
        for (i, raw) in g8.iter().enumerate() {
            assert_tile_matches(&on.tiles[0], i, [unit(*raw); 3], 3e-3, "lifted gray, BPC on");
        }
        let d = on.provenance.display.as_ref().expect("display record");
        assert_eq!((d.source_space, d.black_point_compensation, d.black_point_compensation_assumed), ("gray", true, false));
        assert!(!on.loss.has_code("black_point_compensation_skipped_gray"), "the flag is not dropped for gray");
        assert!(!on.loss.has_code("black_point_compensation_assumed_on"), "it was configured, not assumed");

        // configured off: the uncompensated closed form, so the compensation above is measurable
        let (_, off) = prism_import(&lifted, SixteenBitScale::default(), RELATIVE, Some(false));
        for (i, raw) in g8.iter().enumerate() {
            assert_tile_matches(&off.tiles[0], i, [v0 + (1.0 - v0) * unit(*raw); 3], 3e-3, "lifted gray, BPC off");
        }
        assert!(f64::from(f32_at(&off.tiles[0].colour, 0)) > 0.09, "black stays lifted without compensation");
        assert!(!off.provenance.display.as_ref().unwrap().black_point_compensation);

        // default: assumed on, recorded, and the same pixels as configured on
        let (_, assumed) = prism_import(&lifted, SixteenBitScale::default(), RELATIVE, None);
        assert_eq!(assumed.tiles[0].colour, on.tiles[0].colour);
        assert!(assumed.loss.has_code("black_point_compensation_assumed_on"));
        assert!(assumed.provenance.display.as_ref().unwrap().black_point_compensation_assumed);

        // zero black (gamma 2 curve, TRC(0) = 0): compensation is the identity, pixels unchanged by the flag
        let zero = doc_of(&icc::gray_profile(2.0));
        let (_, zero_on) = prism_import(&zero, SixteenBitScale::default(), RELATIVE, Some(true));
        let (_, zero_off) = prism_import(&zero, SixteenBitScale::default(), RELATIVE, Some(false));
        assert_eq!(zero_on.tiles[0].colour, zero_off.tiles[0].colour);
        for (i, raw) in g8.iter().enumerate() {
            assert_tile_matches(&zero_on.tiles[0], i, [unit(*raw).powi(2); 3], 3e-3, "zero-black gray");
        }
    });
}

/// Same failure class (a colour-management step silently skipped) on the RGB path: the legacy converter is
/// relative colorimetric without black point compensation, so RGB with an embedded profile now takes the
/// wide path and the configured (or assumed) intent and compensation reach Prism. A converter without a wide
/// path says in the loss report that neither was applied.
#[test]
fn rgb_intent_and_black_point_compensation_reach_prism_or_are_recorded() {
    bounded_case("rgb_intent_and_black_point_compensation_reach_prism_or_are_recorded", || {
        let (r, g, b) = ([64u8, 10], [128u8, 200], [255u8, 30]);
        let rgb_doc = |profile: &[u8]| {
            let merged = [r.to_vec(), g.to_vec(), b.to_vec()].concat();
            flat_doc(3, 3, 8, (2, 1), resource(1039, "", profile), vec![(0, r.to_vec()), (1, g.to_vec()), (2, b.to_vec())], merged)
        };
        let unit = |raw: u8| f64::from(raw) / 255.0;

        // an ICC v2 gamma-2 matrix profile (the legacy path refused v2 profiles): converted by the wide path
        let gamma2 = rgb_doc(&icc::rgb_profile(0x0210_0000, &icc::curv(2.0)));
        let (_, p) = prism_import(&gamma2, SixteenBitScale::default(), RELATIVE, Some(false));
        for (i, (rv, gv, bv)) in [(r[0], g[0], b[0]), (r[1], g[1], b[1])].into_iter().enumerate() {
            assert_tile_matches(&p.tiles[0], i, [unit(rv).powi(2), unit(gv).powi(2), unit(bv).powi(2)], 3e-3, "rgb v2 gamma 2");
        }
        let d = p.provenance.display.as_ref().expect("display record");
        assert_eq!(d.source_space, "rgb");
        assert!(p.provenance.icc_converted && !p.provenance.assumed_profile && p.loss.has_code("icc_profile_applied_by_converter"));

        // an explicit non-default intent is the one Prism was asked for, not silently replaced
        let (_, sat) = prism_import(&gamma2, SixteenBitScale::default(), Some(RenderingIntent::Saturation), Some(false));
        let d = sat.provenance.display.as_ref().unwrap();
        assert_eq!((d.intent, d.intent_assumed), (RenderingIntent::Saturation, false));

        // lifted RGB black (TRC(0) = v0): same paper oracle as gray on the neutral axis
        let v0 = 6554.0 / 65535.0;
        let grey = [0u8, 64, 200, 255];
        let neutral = |profile: &[u8]| {
            flat_doc(3, 3, 8, (4, 1), resource(1039, "", profile), (0..3i16).map(|c| (c, grey.to_vec())).collect(), grey.repeat(3))
        };
        let lifted = neutral(&icc::rgb_profile(0x0210_0000, &icc::curv_table(&[6554, 65535])));
        let (_, on) = prism_import(&lifted, SixteenBitScale::default(), RELATIVE, Some(true));
        let (_, off) = prism_import(&lifted, SixteenBitScale::default(), RELATIVE, Some(false));
        for (i, raw) in grey.iter().enumerate() {
            assert_tile_matches(&on.tiles[0], i, [unit(*raw); 3], 3e-3, "lifted rgb, BPC on");
            assert_tile_matches(&off.tiles[0], i, [v0 + (1.0 - v0) * unit(*raw); 3], 3e-3, "lifted rgb, BPC off");
        }
        let (_, assumed) = prism_import(&lifted, SixteenBitScale::default(), None, None);
        assert!(assumed.loss.has_code("black_point_compensation_assumed_on") && assumed.loss.has_code("rendering_intent_assumed_perceptual"));

        // a converter with only the legacy path: relative colorimetric, no compensation, and the report says so
        struct LegacyIdentity;
        impl IccConverter for LegacyIdentity {
            fn destination_profile_id(&self) -> String {
                "SCPF-019abcde-0000-7000-8000-0000000000aa".into()
            }
            fn inspect(&mut self, _: &[u8], _: &CancellationToken) -> std::result::Result<(), IccRefusal> {
                Ok(())
            }
            fn convert_rgb(&mut self, _: &[u8], pixels: &[[f32; 3]], _: &CancellationToken) -> std::result::Result<Vec<[f32; 3]>, IccRefusal> {
                Ok(pixels.to_vec())
            }
        }
        let limits = Limits::default();
        let bytes = rgb_doc(SRGB_V4);
        let doc = read(&bytes);
        let mut cfg = ProposalConfig::new(&limits, budget(), &Open);
        let legacy = |cfg: &ProposalConfig<'_>| {
            to_proposal(&doc, &SourceIdentity::of(&bytes), cfg, &mut Mint(0), Some(&mut LegacyIdentity), &CancellationToken::default()).unwrap()
        };
        let defaults = legacy(&cfg);
        assert!(defaults.loss.has_code("rendering_intent_not_applied_legacy_rgb") && defaults.loss.has_code("black_point_compensation_not_applied_legacy_rgb"));
        assert!(defaults.provenance.icc_converted, "the legacy path still converted");
        cfg.rendering_intent = RELATIVE;
        cfg.black_point_compensation = Some(false);
        let honoured = legacy(&cfg);
        assert!(!honoured.loss.has_code("rendering_intent_not_applied_legacy_rgb") && !honoured.loss.has_code("black_point_compensation_not_applied_legacy_rgb"));
    });
}

/// R5 carried PLAUSIBLE items on the export path. `apply_edits` is all-or-nothing (a later layer's error
/// leaves the caller's document untouched); colour under alpha 0 in a cell that never had a tile survives a
/// pixel export (only a cell the caller removed is erased); non-finite tile values are a typed error; an
/// alpha outside 0..=1 is clamped with a typed loss; a rename longer than the name limit is reported.
#[test]
fn apply_edits_is_all_or_nothing_and_export_keeps_untiled_cells() {
    bounded_case("apply_edits_is_all_or_nothing_and_export_keeps_untiled_cells", || {
        let limits = Limits::default();
        let cancel = CancellationToken::default();

        // -- all-or-nothing: layer 0 renames fine, layer 3 then fails on a malformed tile blob
        let bytes = export_fixture();
        let (doc, p) = proposal_of(&bytes, SixteenBitScale::default(), None);
        let renamed = edited_snapshot(&p, |l| {
            if l.name == "base" {
                l.name = "Base2".into();
            }
        });
        let inner_id = layer_named(p.snapshot.document(), "inner").layer_id.clone();
        let mut damaged = p.tiles.clone();
        damaged.iter_mut().filter(|t| t.layer_id == inner_id).for_each(|t| t.colour.truncate(4));
        let mut target = doc.clone();
        let before = write_psd(&target, &limits).unwrap();
        let failed = apply_edits(&mut target, &p, &renamed, &ExportOptions { limits: &limits, tiles: Some(&damaged) });
        assert_eq!(failed.unwrap_err(), PsdError::Encode("tile_blob_size"));
        assert_eq!(target.layers()[0].name, "base", "the rename of the earlier layer was not left half-applied");
        assert_eq!(write_psd(&target, &limits).unwrap(), before);
        let applied = apply_edits(&mut target, &p, &renamed, &ExportOptions { limits: &limits, tiles: Some(&p.tiles) }).unwrap();
        assert!(applied.was_applied("name") && target.layers()[0].name == "Base2");

        // -- non-finite tile values are refused, an out-of-range alpha is clamped and reported
        let base_id = layer_named(p.snapshot.document(), "base").layer_id.clone();
        let mut nan = p.tiles.clone();
        nan.iter_mut().filter(|t| t.layer_id == base_id).for_each(|t| t.colour[0..4].copy_from_slice(&f32::NAN.to_le_bytes()));
        let result = export_psd(&doc, &p, &p.snapshot, &ExportOptions { limits: &limits, tiles: Some(&nan) });
        assert_eq!(result.unwrap_err(), PsdError::Encode("non_finite_sample"));
        let mut bright = p.tiles.clone();
        bright.iter_mut().filter(|t| t.layer_id == base_id).for_each(|t| t.alpha[0..4].copy_from_slice(&2.0f32.to_le_bytes()));
        let (_, report) = export_psd(&doc, &p, &p.snapshot, &ExportOptions { limits: &limits, tiles: Some(&bright) }).unwrap();
        assert!(report.was_applied("pixels") && report.loss.has_code("alpha_range_clamped"));

        // -- a rename past the name limit is truncated by the writer and says so
        let long = "n".repeat(limits.max_name_units + 76);
        let long_name = edited_snapshot(&p, |l| {
            if l.name == "base" {
                l.name.clone_from(&long);
            }
        });
        let (out, report) = export_psd(&doc, &p, &long_name, &ExportOptions { limits: &limits, tiles: None }).unwrap();
        assert!(report.loss.entries.iter().any(|e| e.code == "layer_name_truncated" && e.path == "layer[0]/name"));
        assert_eq!(read(&out).layers()[0].name.encode_utf16().count(), limits.max_name_units);

        // -- untiled cells: a 512x1 layer whose right cell is fully transparent but carries colour
        let colour = |left: &dyn Fn(usize) -> u8, right: u8| -> Vec<u8> { (0..512).map(|x| if x < 256 { left(x) } else { right }).collect() };
        let (r, g, b) = (colour(&|x| 10 + (x % 100) as u8, 77), colour(&|x| 20 + (x % 90) as u8, 88), colour(&|x| 30 + (x % 80) as u8, 99));
        let alpha = colour(&|_| 255, 0);
        let merged = [r.clone(), g.clone(), b.clone()].concat();
        let wide = flat_doc(3, 3, 8, (512, 1), Vec::new(), vec![(0, r.clone()), (1, g.clone()), (2, b.clone()), (-1, alpha.clone())], merged);
        let (wide_doc, wp) = proposal_of(&wide, SixteenBitScale::default(), None);
        assert_eq!(wp.tiles.len(), 1, "only the left cell has a tile; the transparent right cell has none");
        let plane = |d: &PsdDocument, id: i16| d.layers()[0].decode_channel(id, &d.header, &limits, &cancel).unwrap().unwrap().bytes;
        let mut edited = wp.tiles.clone();
        edited[0].colour[0..4].copy_from_slice(&0.5f32.to_le_bytes());
        let (out, report) = export_psd(&wide_doc, &wp, &wp.snapshot, &ExportOptions { limits: &limits, tiles: Some(&edited) }).unwrap();
        assert!(report.was_applied("pixels"));
        let back = read(&out);
        let (nr, ng, nb, na) = (plane(&back, 0), plane(&back, 1), plane(&back, 2), plane(&back, -1));
        assert_eq!(nr[0], unit_to_sample(linear_to_srgb(0.5), 8, SixteenBitScale::default()) as u8, "the edit landed");
        assert_eq!(&nr[1..], &r[1..], "untouched left pixels keep their bytes");
        assert_eq!((&nr[256..], &ng[256..], &nb[256..], &na[256..]), (&r[256..], &g[256..], &b[256..], &alpha[256..]), "colour under alpha 0 survives");
        // removing the tile is an explicit erase of that cell only; the untiled cell still keeps its samples
        let (out, _) = export_psd(&wide_doc, &wp, &wp.snapshot, &ExportOptions { limits: &limits, tiles: Some(&[]) }).unwrap();
        let back = read(&out);
        let (nr, na) = (plane(&back, 0), plane(&back, -1));
        assert!(nr[..256].iter().all(|&v| v == 0) && na[..256].iter().all(|&v| v == 0), "the removed cell is transparent black");
        assert_eq!((&nr[256..], &na[256..]), (&r[256..], &alpha[256..]));
    });
}

/// R5 carried PLAUSIBLE items on import: a NaN or infinite float sample never enters a tile (zeroed, typed
/// loss), and an alpha outside 0..=1 (float alpha, or 16-bit alpha above 1.0 on the 32768 scale) is clamped
/// with a typed loss instead of silently.
#[test]
fn non_finite_and_out_of_range_samples_are_typed_losses_on_import() {
    bounded_case("non_finite_and_out_of_range_samples_are_typed_losses_on_import", || {
        let (r, g, b) = ([f32::NAN, 0.5], [f32::INFINITY, 0.5], [0.25f32, 0.5]);
        let a = [f32::NEG_INFINITY, 2.0];
        let merged = [bef32(&r), bef32(&g), bef32(&b)].concat();
        let bytes = flat_doc(3, 3, 32, (2, 1), Vec::new(), vec![(0, bef32(&r)), (1, bef32(&g)), (2, bef32(&b)), (-1, bef32(&a))], merged);
        let (_, p) = proposal_of(&bytes, SixteenBitScale::default(), None);
        let tile = &p.tiles[0];
        assert_eq!([f32_at(&tile.colour, 0), f32_at(&tile.colour, 1), f32_at(&tile.colour, 2)], [0.0, 0.0, 0.25], "non-finite colour becomes 0, finite stays");
        assert_eq!([f32_at(&tile.alpha, 0), f32_at(&tile.alpha, 1)], [0.0, 1.0], "non-finite alpha is transparent, 2.0 clamps to 1");
        assert!(p.loss.has_code("non_finite_samples_zeroed") && p.loss.has_code("alpha_range_clamped"));
        assert!(tile.colour.chunks_exact(4).all(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]).is_finite()));

        // 16-bit on the 32768 scale: raw 65535 alpha is 1.99997 and is clamped, reported
        let plane = be16s(&[0x4000, 0xFFFF]);
        let bytes = flat_doc(3, 3, 16, (2, 1), Vec::new(), vec![(0, plane.clone()), (1, plane.clone()), (2, plane.clone()), (-1, plane.clone())], plane.repeat(3));
        let (_, scaled) = proposal_of(&bytes, SixteenBitScale::Photoshop32768, None);
        assert!(scaled.loss.has_code("alpha_range_clamped") && !scaled.loss.has_code("non_finite_samples_zeroed"));
        let (_, full) = proposal_of(&bytes, SixteenBitScale::Full65535, None);
        assert!(!full.loss.has_code("alpha_range_clamped"), "nothing leaves 0..=1 on the full scale");
    });
}
