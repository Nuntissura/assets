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
            assert!(report.was_applied("pixels") && report.loss.entries.is_empty());
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

        // refused (over Prism's 64 KiB cap): assumed sRGB plus the refusal code
        let (p, _) = with_prism(&rgb(Some(&vec![7u8; 70_000])));
        assert!(p.loss.has_code("icc_refused_profile_limit") && p.provenance.assumed_profile && !p.provenance.icc_converted);
        expect_assumed(&p);

        // CMYK: Prism has no 4-channel path; with or without a profile the layers are unmapped
        let planes: Vec<Vec<u8>> = (0..4u8).map(|c| vec![c * 10 + 1, c * 10 + 2]).collect();
        let cmyk = |icc: Option<&[u8]>| {
            let resources = icc.map_or_else(Vec::new, |bytes| resource(1039, "", bytes));
            flat_doc(4, 4, 8, (1, 2), resources, planes.iter().cloned().enumerate().map(|(i, d)| (i as i16, d)).collect(), planes.concat())
        };
        let (p, _) = with_prism(&cmyk(Some(&vec![7u8; 70_000])));
        assert!(p.loss.has_code("cmyk_display_conversion_unavailable") && p.loss.has_code("icc_refused_profile_limit"));
        assert_eq!(reason_of(&p), "cmyk_display_conversion_unavailable");
        assert!(p.tiles.is_empty() && p.snapshot.document().colour_mode == 3);
        let (p, _) = with_prism(&cmyk(None));
        assert!(p.loss.has_code("cmyk_profile_missing") && p.loss.has_code("cmyk_display_conversion_unavailable"));

        // Lab has no display conversion either
        let lab = flat_doc(9, 3, 8, (1, 2), Vec::new(), (0..3i16).map(|i| (i, vec![100, 128])).collect(), vec![100, 128, 100, 128, 100, 128]);
        let (p, _) = with_prism(&lab);
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
        assert_eq!(lost, [("layer[3]/blend", "blend_mode_not_representable_in_psd")]);

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
        assert_eq!(back.resources, doc.resources, "ICC profile and every other resource");
        assert_eq!(back.image_data, doc.image_data);
        assert_eq!(back.layer_section.blocks, doc.layer_section.blocks);
    });
}
