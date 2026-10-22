//! SMB-INTEROP-PSD-ADJ-1: adjustment descriptors, blending ranges, mask parameters, knockout and
//! `lfx2` effects mapped to `StudioRasterRecords`. All inputs are small synthetic byte fixtures built
//! here, independently of the crate's encoder. Photoshop display parity of the mapped records is
//! NOT_PROVEN until a D5 oracle exists (these tests prove the mapping of the bytes, not Photoshop's
//! rendering).

mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    ADJUSTMENT_SCHEMA, Budget, Payload, Resolution, Resolver, StudioAdjustment, StudioAdjustmentChannel,
    StudioBlendChannel, StudioEffectKind, StudioKnockout, TileRef, validate_raster_records,
};
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
    fn effect_stack_id(&mut self, _layer_id: &str) -> Option<String> {
        Some(self.next("SEFX"))
    }
}
/// A host resolver that binds the adjustment primitive (or, with `bind: false`, does not).
struct Host {
    bind: bool,
}
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
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        self.bind && kind == "adjustment" && schema_id == ADJUSTMENT_SCHEMA
    }
}

fn budget() -> Budget {
    Budget { input_bytes: 8 << 20, payload_bytes: 4 << 20, ..Budget::default() }
}
fn proposal_with(bytes: &[u8], bind: bool) -> (PsdDocument, ImportProposal) {
    let limits = Limits::default();
    let doc = read(bytes);
    let host = Host { bind };
    let cfg = ProposalConfig::new(&limits, budget(), &host);
    let p = to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0), None, &CancellationToken::default())
        .expect("proposal");
    (doc, p)
}
fn proposal(bytes: &[u8]) -> (PsdDocument, ImportProposal) {
    proposal_with(bytes, true)
}
/// The records must validate against the proposal's own document.
fn assert_valid(p: &ImportProposal) {
    let issues = validate_raster_records(&p.raster_records, p.snapshot.document(), budget(), &CancellationToken::default(), &Host { bind: true })
        .expect("raster records validate");
    assert!(issues.is_empty());
}

/// A 1x1 RGB 8-bit document holding `layer`.
fn doc_with(layer: TLayer) -> Vec<u8> {
    let info = layer_info(&[layer], false, false);
    Doc {
        version: 1,
        channels: 3,
        height: 1,
        width: 1,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: layer_section(&info, &[], false),
        image: Some((0, vec![1, 2, 3])),
    }
    .bytes()
}
fn raster(blocks: Vec<Vec<u8>>) -> TLayer {
    let mut l = TLayer::new("px", [0, 0, 1, 1]);
    l.channels = vec![(0, 0, vec![10]), (1, 0, vec![20]), (2, 0, vec![30])];
    l.blocks = blocks;
    l
}
fn adjustment_layer(key: &[u8; 4], data: &[u8]) -> TLayer {
    let mut l = TLayer::new("adj", [0, 0, 0, 0]);
    l.channels = empty_channels(&[0, 1, 2]);
    l.blocks = vec![layer_block(key, data, false)];
    l
}
fn adjustment_doc(key: &[u8; 4], data: &[u8]) -> Vec<u8> {
    doc_with(adjustment_layer(key, data))
}
fn first_layer(p: &ImportProposal) -> &hsk_studio_folio::StudioLayer {
    &p.snapshot.document().layers[0]
}
fn codes(p: &ImportProposal, path: &str) -> Vec<&'static str> {
    p.loss.entries.iter().filter(|e| e.path == path).map(|e| e.code).collect()
}

fn levl(records: &[[u16; 5]]) -> Vec<u8> {
    let mut v = be16(2);
    for i in 0..29 {
        for x in records.get(i).copied().unwrap_or([0, 255, 0, 255, 100]) {
            v.extend(be16(x));
        }
    }
    v
}
fn curv(version: u16, map: u32, curves: &[&[(u16, u16)]]) -> Vec<u8> {
    let mut v = vec![0u8];
    v.extend(be16(version));
    v.extend(be32(map));
    for points in curves {
        v.extend(be16(points.len() as u16));
        for (output, input) in *points {
            v.extend(be16(*output));
            v.extend(be16(*input));
        }
    }
    v
}

// ---- Photoshop descriptor builders (independent of the crate's reader) ----------------------

fn ds_key(k: &str) -> Vec<u8> {
    let mut v = if k.len() == 4 { be32(0) } else { be32(k.len() as u32) };
    v.extend_from_slice(k.as_bytes());
    v
}
fn ds_object(class: &str, items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = be32(0); // empty Unicode class name
    v.extend(ds_key(class));
    v.extend(be32(items.len() as u32));
    for (key, tagged) in items {
        v.extend(ds_key(key));
        v.extend(tagged);
    }
    v
}
fn tagged(tag: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let mut v = tag.to_vec();
    v.extend(payload);
    v
}
fn v_objc(class: &str, items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    tagged(b"Objc", ds_object(class, items))
}
fn v_list(items: Vec<Vec<u8>>) -> Vec<u8> {
    let mut v = be32(items.len() as u32);
    for item in items {
        v.extend(item);
    }
    tagged(b"VlLs", v)
}
fn v_untf(unit: &[u8; 4], value: f64) -> Vec<u8> {
    let mut v = unit.to_vec();
    v.extend(value.to_be_bytes());
    tagged(b"UntF", v)
}
fn v_doub(value: f64) -> Vec<u8> {
    tagged(b"doub", value.to_be_bytes().to_vec())
}
fn v_bool(value: bool) -> Vec<u8> {
    tagged(b"bool", vec![u8::from(value)])
}
fn v_long(value: i32) -> Vec<u8> {
    tagged(b"long", value.to_be_bytes().to_vec())
}
fn v_enum(type_id: &str, value: &str) -> Vec<u8> {
    let mut v = ds_key(type_id);
    v.extend(ds_key(value));
    tagged(b"enum", v)
}
/// Object-effects version 0, descriptor version 16, then the root descriptor.
fn lfx2(items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = be32(0);
    v.extend(be32(16));
    v.extend(ds_object("null", items));
    v
}
fn shadow(distance: f64) -> Vec<u8> {
    v_objc(
        "DrSh",
        vec![
            ("enab", v_bool(true)),
            ("Md  ", v_enum("BlnM", "Mltp")),
            ("Opct", v_untf(b"#Prc", 75.0)),
            ("Clr ", v_objc("RGBC", vec![("Rd  ", v_doub(255.0)), ("Grn ", v_doub(0.0)), ("Bl  ", v_doub(51.0))])),
            ("uglg", v_bool(true)),
            ("lagl", v_untf(b"#Ang", 120.0)),
            ("Dstn", v_untf(b"#Pxl", distance)),
            ("Ckmt", v_untf(b"#Prc", 10.0)),
            ("blur", v_untf(b"#Pxl", 4.0)),
            ("Nose", v_untf(b"#Prc", 0.0)),
            ("AntA", v_bool(false)),
        ],
    )
}
/// Proposal of a raster layer carrying `data` as its `lfx2` block; the loss codes of that block.
fn lfx2_codes(data: &[u8]) -> (ImportProposal, Vec<&'static str>) {
    let (_, p) = proposal(&doc_with(raster(vec![layer_block(b"lfx2", data, false)])));
    let codes = codes(&p, "layer[0]/tagged/lfx2");
    (p, codes)
}

// ---- tests ----------------------------------------------------------------------------------

/// `levl`: composite (10, 245, 0, 255, gamma 150) maps to input_black 10/255, input_white 245/255 and
/// gamma 1.5 (STU-RAS-152 normalized_0_1). The layer is an `adjustment` layer hosting its own record;
/// the mapped key leaves the `adjustment_layer_not_applied` class; a truncated block, a wrong
/// version and an invalid record keep the layer a typed unsupported layer with a reason.
#[test]
fn levels_block_maps_to_normalized_record_and_bad_blocks_stay_typed() {
    bounded_case("levels_block_maps_to_normalized_record_and_bad_blocks_stay_typed", || {
        let data = levl(&[[10, 245, 0, 255, 150]]);
        let bytes = adjustment_doc(b"levl", &data);
        let (doc, p) = proposal(&bytes);
        let layer = first_layer(&p);
        assert_eq!(layer.kind, "adjustment");
        assert_eq!(layer.payload, Payload::PrimitiveReference { schema_id: ADJUSTMENT_SCHEMA.into(), object_id: layer.layer_id.clone() });
        let record = p.raster_records.layer(&layer.layer_id).expect("record");
        let Some(StudioAdjustment::Levels { records }) = &record.adjustment else { panic!("levels record") };
        assert_eq!(records.len(), 4, "composite, red, green, blue of an RGB document");
        let composite = &records[0];
        assert_eq!(composite.channel, StudioAdjustmentChannel::Composite);
        assert_eq!((composite.input_black, composite.input_white), (10.0 / 255.0, 245.0 / 255.0));
        assert_eq!((composite.output_black, composite.output_white, composite.gamma), (0.0, 1.0, 1.5));
        assert!(records[1..].iter().all(|r| r.input_black == 0.0 && r.input_white == 1.0 && r.gamma == 1.0));
        assert_valid(&p);
        assert!(!p.loss.has_code("adjustment_layer_not_applied"), "the mapped key leaves the loss class");
        assert!(p.loss.has_code("adjustment_mapped_to_record"));
        assert_eq!(write_psd(&doc, &Limits::default()).unwrap(), bytes, "the block stays byte-exact");

        // truncated, wrong version, invalid record: unsupported layer, reason recorded, class kept
        let mut bad_version = data.clone();
        bad_version[1] = 3;
        let bad_record = levl(&[[200, 100, 0, 255, 100]]);
        for (block, reason) in [(data[..100].to_vec(), "levels_truncated"), (bad_version, "levels_version_unsupported"), (bad_record, "levels_record_invalid")] {
            let (_, p) = proposal(&adjustment_doc(b"levl", &block));
            assert_eq!(first_layer(&p).kind, "psd_adjustment", "{reason}");
            assert!(codes(&p, "layer[0]/tagged/levl").contains(&reason), "{reason}");
            assert!(p.loss.has_code("adjustment_layer_not_applied") && p.raster_records.layers.is_empty(), "{reason}");
        }
        // a host resolver that binds no adjustment primitive keeps the old typed behaviour
        let (_, unbound) = proposal_with(&bytes, false);
        assert_eq!(first_layer(&unbound).kind, "psd_adjustment");
        assert!(codes(&unbound, "layer[0]/tagged/levl").contains(&"adjustment_primitive_unresolved"));
    });
}

/// `curv` (leading byte, version 1, channel bitmap, then (output, input) pairs): an inverting composite
/// curve maps to points (input 0 -> output 255), (input 255 -> output 0). A point count of 20, inputs that
/// do not ascend and the 256-entry map format are refused.
#[test]
fn curves_block_maps_composite_curve_and_refuses_bad_point_data() {
    bounded_case("curves_block_maps_composite_curve_and_refuses_bad_point_data", || {
        let (_, p) = proposal(&adjustment_doc(b"curv", &curv(1, 0b1, &[&[(255, 0), (0, 255)]])));
        let layer = first_layer(&p);
        let record = p.raster_records.layer(&layer.layer_id).expect("record");
        let Some(StudioAdjustment::Curves { curves }) = &record.adjustment else { panic!("curves record") };
        assert_eq!(curves.len(), 1);
        assert_eq!(curves[0].channel, StudioAdjustmentChannel::Composite);
        let points: Vec<(f64, f64)> = curves[0].points.iter().map(|q| (q.input, q.output)).collect();
        assert_eq!(points, [(0.0, 255.0), (255.0, 0.0)]);
        assert_valid(&p);

        let twenty: Vec<(u16, u16)> = (0..20u16).map(|i| (i, i)).collect();
        let mut map_format = curv(1, 0b1, &[]);
        map_format[0] = 1;
        for (block, reason) in [
            (curv(1, 0b1, &[&twenty]), "curves_point_count"),
            (curv(1, 0b1, &[&[(10, 5), (20, 5)]]), "curves_points_not_ascending"),
            (curv(1, 0b1, &[&[(256, 0), (0, 255)]]), "curves_point_range"),
            (curv(2, 0b1, &[&[(0, 0), (255, 255)]]), "curves_version_unsupported"),
            (map_format, "curves_map_format_unsupported"),
        ] {
            let (_, p) = proposal(&adjustment_doc(b"curv", &block));
            assert_eq!(first_layer(&p).kind, "psd_adjustment", "{reason}");
            assert!(codes(&p, "layer[0]/tagged/curv").contains(&reason), "{reason}");
        }
    });
}

/// Blend-If ranges, mask density/feather, knockout depth 2 and the other small adjustment keys.
#[test]
fn blending_ranges_mask_parameters_and_knockout_map_to_records() {
    bounded_case("blending_ranges_mask_parameters_and_knockout_map_to_records", || {
        // gray source [50, 100, 200, 240], everything else default; red/green/blue pairs follow
        let mut ranges = vec![50, 100, 200, 240, 0, 0, 255, 255];
        for _ in 0..3 {
            ranges.extend([0, 0, 255, 255, 0, 0, 255, 255]);
        }
        // mask: rectangle, default colour 255, flags 0x10 (parameters), parameter flags 3 (density, feather)
        let mut mask: Vec<u8> = [0i32, 0, 1, 1].iter().flat_map(|v| v.to_be_bytes()).collect();
        mask.extend([255, 0x10, 3, 128]);
        mask.extend(12.5f64.to_be_bytes());
        let mut layer = raster(vec![layer_block(b"knko", &[2, 0, 0, 0], false)]);
        layer.ranges = ranges;
        layer.mask = mask;
        layer.channels.push((-2, 0, vec![255]));
        let bytes = doc_with(layer);
        let (doc, p) = proposal(&bytes);
        let id = first_layer(&p).layer_id.clone();
        let record = p.raster_records.layer(&id).expect("record");

        let blending = record.advanced_blending.as_ref().expect("advanced blending");
        assert_eq!(blending.blend_ranges.len(), 1, "only the non-default gray entry is recorded");
        let gray = &blending.blend_ranges[0];
        assert_eq!(gray.channel, StudioBlendChannel::Gray);
        assert_eq!((gray.source_black, gray.source_white), ([50.0, 100.0], [200.0, 240.0]));
        assert_eq!((gray.destination_black, gray.destination_white), ([0.0, 0.0], [255.0, 255.0]));
        assert_eq!(blending.knockout, StudioKnockout::Enabled);
        // knko 2 = deep: carried as the Folio knockout depth since lane SMB-PS-ADJUST-STYLES.
        assert_eq!(blending.knockout_depth, Some(hsk_studio_folio::StudioKnockoutDepth::Deep));
        assert!(!p.loss.has_code("knockout_depth_not_represented"));
        assert_eq!(doc.layers()[0].block(b"knko").unwrap().data[0], 2, "the raw knockout byte is preserved");
        assert!(!p.loss.has_code("blend_if_not_applied"), "Blend-If is recorded now");

        let mask = record.mask.as_ref().expect("mask parameters");
        assert!((mask.density - 128.0 / 255.0 * 100.0).abs() < 1e-12 && (mask.density - 50.196).abs() < 1e-3);
        assert_eq!((mask.feather, mask.inverted), (12.5, false));
        assert!(!p.loss.has_code("mask_density_feather_not_stored"));
        assert_valid(&p);
        assert_eq!(write_psd(&doc, &Limits::default()).unwrap(), bytes);

        // a blending-ranges block shorter than 8 bytes is refused with a typed reason
        let mut short = raster(Vec::new());
        short.ranges = vec![50, 100, 200, 240];
        let (_, p) = proposal(&doc_with(short));
        assert!(codes(&p, "layer[0]/blend_ranges").contains(&"blend_ranges_malformed"));
        assert!(p.raster_records.layers.is_empty());

        // exposure / threshold / posterize / invert / hue-saturation / brightness-contrast
        let mut exp = be16(1);
        for v in [1.5f32, 0.1, 1.2] {
            exp.extend(v.to_bits().to_be_bytes());
        }
        let (_, p) = proposal(&adjustment_doc(b"expA", &exp));
        let Some(StudioAdjustment::Exposure { exposure, offset, gamma, .. }) = p.raster_records.layers[0].adjustment.clone() else { panic!("exposure") };
        assert_eq!((exposure as f32, offset as f32, gamma as f32), (1.5, 0.1, 1.2));
        let (_, p) = proposal(&adjustment_doc(b"thrs", &[0, 128, 0, 0]));
        assert!(matches!(p.raster_records.layers[0].adjustment, Some(StudioAdjustment::Threshold { level, .. }) if level == 128.0));
        let (_, p) = proposal(&adjustment_doc(b"post", &[0, 4, 0, 0]));
        assert!(matches!(p.raster_records.layers[0].adjustment, Some(StudioAdjustment::Posterize { levels: 4 })));
        let (_, p) = proposal(&adjustment_doc(b"nvrt", &[]));
        assert!(matches!(p.raster_records.layers[0].adjustment, Some(StudioAdjustment::Invert)));
        // brightness/contrast: signed values (-30 brightness) and the legacy assumption recorded
        let mut brit = (-30i16).to_be_bytes().to_vec();
        brit.extend(20i16.to_be_bytes());
        brit.extend([0, 0, 0, 0]);
        let (_, p) = proposal(&adjustment_doc(b"brit", &brit));
        assert!(matches!(
            p.raster_records.layers[0].adjustment,
            Some(StudioAdjustment::BrightnessContrast { brightness, contrast, use_legacy: true }) if brightness == -30.0 && contrast == 20.0
        ));
        assert!(p.loss.has_code("brightness_contrast_legacy_assumed"));
        // ... and `CgEd` takes precedence over `brit`, carrying useLegacy
        let cged = {
            let mut v = be32(16);
            v.extend(ds_object(
                "null",
                vec![("Vrsn", v_long(1)), ("Brgh", v_long(40)), ("Cntr", v_long(-10)), ("useLegacy", v_bool(false))],
            ));
            v
        };
        let mut l = adjustment_layer(b"brit", &brit);
        l.blocks.push(layer_block(b"CgEd", &cged, false));
        let (_, p) = proposal(&doc_with(l));
        assert!(matches!(
            p.raster_records.layers[0].adjustment,
            Some(StudioAdjustment::BrightnessContrast { brightness, contrast, use_legacy: false }) if brightness == 40.0 && contrast == -10.0
        ));
        // hue/saturation: master +20/-10/+5, one band shifted, the documented 100-byte record
        let mut hue = be16(2);
        hue.extend([0, 0]);
        for v in [0i16, 0, 0, 20, -10, 5] {
            hue.extend(v.to_be_bytes());
        }
        for band in 0..6i16 {
            for v in [315i16, 345, 15, 45] {
                hue.extend(v.to_be_bytes());
            }
            for v in if band == 2 { [30i16, 0, 0] } else { [0, 0, 0] } {
                hue.extend(v.to_be_bytes());
            }
        }
        assert_eq!(hue.len(), 100);
        let (_, p) = proposal(&adjustment_doc(b"hue2", &hue));
        let Some(StudioAdjustment::HueSaturation { master, colorize, bands, .. }) = p.raster_records.layers[0].adjustment.clone() else { panic!("hue") };
        assert_eq!((master.hue, master.saturation, master.lightness, colorize), (20.0, -10.0, 5.0, false));
        assert_eq!(bands.len(), 1, "only the band with a shift is recorded");
        assert_eq!(bands[0].shift.hue, 30.0);
        assert_valid(&p);
    });
}

/// `lfx2`: a single `DrSh` and a `dropShadowMulti` list of two give a three-instance stack in source order;
/// the stack validates; effects stay "not rendered"; gradient content and an unmapped blend mode are skipped
/// with typed losses, never defaulted.
#[test]
fn lfx2_stack_merges_single_and_multi_slots_in_source_order() {
    bounded_case("lfx2_stack_merges_single_and_multi_slots_in_source_order", || {
        let data = lfx2(vec![
            ("Scl ", v_untf(b"#Prc", 100.0)),
            ("masterFXSwitch", v_bool(true)),
            ("DrSh", shadow(1.0)),
            ("dropShadowMulti", v_list(vec![shadow(2.0), shadow(3.0)])),
        ]);
        let bytes = doc_with(raster(vec![layer_block(b"lfx2", &data, false)]));
        let (doc, p) = proposal(&bytes);
        let id = first_layer(&p).layer_id.clone();
        let stack = p.raster_records.layer(&id).and_then(|r| r.effects.as_ref()).expect("effect stack");
        assert!(stack.effect_stack_id.starts_with("SEFX-") && stack.enabled && stack.scale == 100.0);
        assert_eq!(stack.effects.len(), 3);
        let distances: Vec<f64> = stack
            .effects
            .iter()
            .map(|e| match &e.params {
                StudioEffectKind::DropShadow { distance, .. } => *distance,
                other => panic!("drop shadow expected, got {other:?}"),
            })
            .collect();
        assert_eq!(distances, [1.0, 2.0, 3.0], "single slot first, then the multi list, in descriptor order");
        let ids: Vec<&str> = stack.effects.iter().map(|e| e.instance_id.as_str()).collect();
        assert_eq!(ids, ["drop_shadow-0", "drop_shadow-1", "drop_shadow-2"]);
        let first = &stack.effects[0];
        assert_eq!((first.opacity, first.angle, first.noise, first.use_global_light), (75.0, 120.0, 0.0, true));
        assert_eq!(first.blend_mode, StudioBlendMode::Multiply.discriminant());
        let colour = first.colour.as_ref().expect("colour");
        assert_eq!((colour.r, colour.g, colour.b), (1.0, 0.0, 51.0 / 255.0));
        match &first.params {
            StudioEffectKind::DropShadow { spread, size, layer_knocks_out, .. } => assert_eq!((*spread, *size, *layer_knocks_out), (10.0, 4.0, true)),
            other => panic!("{other:?}"),
        }
        assert_valid(&p);
        assert!(p.loss.has_code("effect_single_and_multi_slots_merged"));
        assert!(p.loss.has_code("layer_effects_not_rendered"), "effects are recorded, not rendered");
        assert_eq!(write_psd(&doc, &Limits::default()).unwrap(), bytes);

        // gradient overlay content and a blend mode outside the mapped set are skipped, typed, never Normal
        let odd_mode = {
            let mut s = shadow(1.0);
            let at = s.windows(4).position(|w| w == b"Mltp").expect("mode id");
            s[at..at + 4].copy_from_slice(b"Dslv");
            s
        };
        let gradient = v_objc("GrFl", vec![("enab", v_bool(true)), ("Md  ", v_enum("BlnM", "Nrml")), ("Opct", v_untf(b"#Prc", 100.0))]);
        let (p, codes) = lfx2_codes(&lfx2(vec![("GrFl", gradient), ("DrSh", odd_mode), ("DrSh", shadow(7.0))]));
        assert!(codes.contains(&"effect_fill_reference_unavailable") && codes.contains(&"effect_blend_mode_unmapped"), "{codes:?}");
        let stack = p.raster_records.layers[0].effects.as_ref().expect("the mappable effect still maps");
        assert_eq!(stack.effects.len(), 1);
        assert_valid(&p);
        // a host that mints no effect stacks keeps the block preserved and says so
        struct NoStacks(Mint);
        impl IdMint for NoStacks {
            fn document_id(&mut self) -> String {
                self.0.document_id()
            }
            fn artboard_id(&mut self) -> String {
                self.0.artboard_id()
            }
            fn layer_id(&mut self) -> String {
                self.0.layer_id()
            }
            fn linear_working_profile_id(&mut self) -> String {
                self.0.linear_working_profile_id()
            }
            fn tile_manifest_id(&mut self, layer_id: &str, object_key: &str) -> String {
                self.0.tile_manifest_id(layer_id, object_key)
            }
        }
        let limits = Limits::default();
        let host = Host { bind: true };
        let cfg = ProposalConfig::new(&limits, budget(), &host);
        let doc = read(&bytes);
        let p = to_proposal(&doc, &SourceIdentity::of(&bytes), &cfg, &mut NoStacks(Mint(0)), None, &CancellationToken::default()).unwrap();
        assert!(p.raster_records.layers.is_empty() && codes_of(&p).contains(&"effect_stack_id_unavailable"));
    });
}
fn codes_of(p: &ImportProposal) -> Vec<&'static str> {
    codes(p, "layer[0]/tagged/lfx2")
}

/// Hostile sizes (workflow V4-02): a declared count, key, string or nesting that the block cannot hold is a
/// typed refusal before any allocation, and the running total of descriptor items is bounded (aggregate over
/// budget). Oversized `levl`/`curv`/`hue2` counts are refused the same way. No input panics.
#[test]
fn hostile_descriptor_and_adjustment_sizes_are_typed_refusals() {
    bounded_case("hostile_descriptor_and_adjustment_sizes_are_typed_refusals", || {
        let root_with = |tail: Vec<u8>| {
            let mut v = be32(0);
            v.extend(be32(16));
            v.extend(be32(0)); // class name
            v.extend(ds_key("null"));
            v.extend(tail);
            v
        };
        // count u32::MAX items, key length u32::MAX, string units u32::MAX
        let mut count_max = be32(u32::MAX);
        count_max.extend([0; 16]);
        let mut key_max = be32(1);
        key_max.extend(be32(u32::MAX));
        let mut text_max = be32(1);
        text_max.extend(ds_key("Nm  "));
        text_max.extend(b"TEXT");
        text_max.extend(be32(u32::MAX));
        // nesting 12 deep
        let mut deep = tagged(b"long", be32(1));
        for _ in 0..12 {
            deep = v_objc("null", vec![("k", deep)]);
        }
        let nested = {
            let mut v = be32(1);
            v.extend(ds_key("k"));
            v.extend(deep);
            v
        };
        // aggregate: 17 lists of 512 `long` items = 8704 > the 8192 total
        let mut aggregate = be32(17);
        for n in 0..17 {
            aggregate.extend(ds_key(&format!("list{n:02}")));
            aggregate.extend(v_list((0..512).map(v_long).collect()));
        }
        let mut unknown_type = be32(1);
        unknown_type.extend(ds_key("k"));
        unknown_type.extend(b"zzzz");
        let mut cut_short = be32(1);
        cut_short.extend(ds_key("k"));
        cut_short.extend(b"long");
        cut_short.extend([0, 0]); // half of the i32
        for (tail, reason) in [
            (count_max, "descriptor_item_limit"),
            (be32(1), "descriptor_item_limit"), // one item declared, no bytes left to hold it
            (key_max, "descriptor_key_limit"),
            (text_max, "descriptor_string_limit"),
            (nested, "descriptor_depth"),
            (aggregate, "descriptor_item_limit"),
            (unknown_type, "descriptor_type_unsupported"),
            (cut_short, "descriptor_truncated"),
        ] {
            let (p, codes) = lfx2_codes(&root_with(tail));
            assert!(codes.contains(&reason), "{reason}: {codes:?}");
            assert!(p.raster_records.layers.is_empty(), "{reason}");
        }
        // wrong versions
        let (_, version_codes) = lfx2_codes(&[0, 0, 0, 1, 0, 0, 0, 16]);
        assert!(version_codes.contains(&"descriptor_version_unsupported"));
        // a bool item of another type is a refusal of that effect, not a silent default; a list holding a
        // non-object is reported
        let wrong_type = v_objc("DrSh", vec![("enab", v_long(1)), ("Md  ", v_enum("BlnM", "Mltp"))]);
        let (_, c) = lfx2_codes(&lfx2(vec![("DrSh", wrong_type)]));
        assert!(c.contains(&"effect_value_invalid"), "{c:?}");
        let (_, c) = lfx2_codes(&lfx2(vec![("dropShadowMulti", v_list(vec![v_long(1), shadow(2.0)]))]));
        assert!(c.contains(&"effect_slot_malformed"), "{c:?}");

        // adjustment counts: curves v4 with u32::MAX curves, a v1 bitmap of 32 curves in a short block,
        // a hue record one byte short, a levels block that claims 29 records but holds fewer
        let reason_of = |key: &[u8; 4], data: &[u8]| -> Vec<&'static str> {
            let (_, p) = proposal(&adjustment_doc(key, data));
            assert_eq!(first_layer(&p).kind, "psd_adjustment", "{} with {} bytes", String::from_utf8_lossy(key), data.len());
            codes(&p, &format!("layer[0]/tagged/{}", String::from_utf8_lossy(key)))
        };
        assert!(reason_of(b"curv", &curv(4, u32::MAX, &[])).contains(&"curves_count_limit"));
        assert!(reason_of(b"curv", &curv(1, u32::MAX, &[&[(0, 0), (255, 255)]])).contains(&"curves_truncated"));
        assert!(reason_of(b"hue2", &[0, 2, 0, 0, 0]).contains(&"hue_saturation_truncated"));
        // (a layer-level block is padded to a multiple of 4 inside its length, so cut on a multiple of 4)
        assert!(reason_of(b"levl", &levl(&[])[..280]).contains(&"levels_truncated"));
        assert!(reason_of(b"expA", &[0, 1, 0, 0]).contains(&"exposure_truncated"));
        let mut nan = be16(1);
        for v in [f32::NAN, 0.0, 1.0] {
            nan.extend(v.to_bits().to_be_bytes());
        }
        assert!(reason_of(b"expA", &nan).contains(&"exposure_range"));
        assert!(reason_of(b"thrs", &[1, 0, 0, 0]).contains(&"threshold_range"));
        assert!(reason_of(b"post", &[0, 1, 0, 0]).contains(&"posterize_range"));
        // a mask whose feather is NaN or beyond 10000 px is refused; the layer keeps its other records
        for feather in [f64::NAN, 20000.0, -1.0] {
            let mut mask: Vec<u8> = [0i32, 0, 1, 1].iter().flat_map(|v| v.to_be_bytes()).collect();
            mask.extend([255, 0x10, 2]);
            mask.extend(feather.to_be_bytes());
            let mut layer = raster(Vec::new());
            layer.mask = mask;
            layer.channels.push((-2, 0, vec![255]));
            let (_, p) = proposal(&doc_with(layer));
            assert!(codes(&p, "layer[0]/mask").contains(&"mask_feather_range"));
            assert!(codes(&p, "layer[0]/mask").contains(&"mask_density_feather_not_stored"));
            assert!(p.raster_records.layers.is_empty());
        }
    });
}

// ---- R7 review B: a hostile descriptor number is a per-layer typed loss, never an import failure ----

fn text_item(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut v = be32(units.len() as u32);
    for unit in units {
        v.extend(be16(unit));
    }
    tagged(b"TEXT", v)
}
/// A `DrSh` effect whose `key` item is replaced by (or, if absent, gets) `value`.
fn shadow_with(key: &str, value: Vec<u8>) -> Vec<u8> {
    let mut items: Vec<(&str, Vec<u8>)> = vec![
        ("enab", v_bool(true)),
        ("Md  ", v_enum("BlnM", "Mltp")),
        ("Opct", v_untf(b"#Prc", 75.0)),
        ("Clr ", v_objc("RGBC", vec![("Rd  ", v_doub(255.0)), ("Grn ", v_doub(0.0)), ("Bl  ", v_doub(51.0))])),
        ("uglg", v_bool(true)),
        ("lagl", v_untf(b"#Ang", 120.0)),
        ("Dstn", v_untf(b"#Pxl", 1.0)),
        ("Ckmt", v_untf(b"#Prc", 10.0)),
        ("blur", v_untf(b"#Pxl", 4.0)),
        ("Nose", v_untf(b"#Prc", 0.0)),
        ("AntA", v_bool(false)),
    ];
    match items.iter_mut().find(|(k, _)| *k == key) {
        Some(item) => item.1 = value,
        None => items.push((key, value)),
    }
    v_objc("DrSh", items)
}
fn rgb_colour(red: f64, green: f64, blue: f64) -> Vec<u8> {
    v_objc("RGBC", vec![("Rd  ", v_doub(red)), ("Grn ", v_doub(green)), ("Bl  ", v_doub(blue))])
}
fn contour_with(second_input: f64) -> Vec<u8> {
    let point = |h: f64, v: f64| v_objc("CrPt", vec![("Hrzn", v_doub(h)), ("Vrtc", v_doub(v))]);
    v_objc("ShpC", vec![("Nm  ", text_item("Linear")), ("Crv ", v_list(vec![point(0.0, 0.0), point(second_input, 255.0)]))])
}
/// A document of `layers` (file order), 1x1 RGB 8-bit.
fn doc_with_layers(layers: Vec<TLayer>) -> Vec<u8> {
    let info = layer_info(&layers, false, false);
    Doc {
        version: 1,
        channels: 3,
        height: 1,
        width: 1,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: layer_section(&info, &[], false),
        image: Some((0, vec![1, 2, 3])),
    }
    .bytes()
}
fn brit_bytes(brightness: i16, contrast: i16) -> Vec<u8> {
    let mut v = brightness.to_be_bytes().to_vec();
    v.extend(contrast.to_be_bytes());
    v.extend([0, 0, 0, 0]);
    v
}
/// An adjustment layer holding a valid legacy `brit` record (a fallback that must never be used) and a
/// `CgEd` descriptor with the given `Brgh` / `Cntr` items.
fn cged_doc(brightness: Vec<u8>, contrast: Vec<u8>) -> Vec<u8> {
    let mut cged = be32(16);
    cged.extend(ds_object("null", vec![("Vrsn", v_long(1)), ("Brgh", brightness), ("Cntr", contrast), ("useLegacy", v_bool(false))]));
    let mut l = adjustment_layer(b"brit", &brit_bytes(10, 20));
    l.blocks.push(layer_block(b"CgEd", &cged, false));
    doc_with(l)
}
fn hue_record(colorize: bool, colorization: [i16; 3], master: [i16; 3], band_two: [i16; 3]) -> Vec<u8> {
    let mut v = be16(2);
    v.extend([u8::from(colorize), 0]);
    for x in colorization.into_iter().chain(master) {
        v.extend(x.to_be_bytes());
    }
    for band in 0..6 {
        for x in [315i16, 345, 15, 45] {
            v.extend(x.to_be_bytes());
        }
        for x in if band == 2 { band_two } else { [0, 0, 0] } {
            v.extend(x.to_be_bytes());
        }
    }
    assert_eq!(v.len(), 100);
    v
}
fn exposure_record(exposure: f32, offset: f32, gamma: f32) -> Vec<u8> {
    let mut v = be16(1);
    for x in [exposure, offset, gamma] {
        v.extend(x.to_bits().to_be_bytes());
    }
    v
}
/// The import SUCCEEDS (`proposal` unwraps `to_proposal`), the adjustment layer stays a typed
/// unsupported layer carrying `reason` at `path`, and no raster record was produced.
fn assert_adjustment_refused(bytes: &[u8], key: &str, reason: &'static str, what: &str) {
    let (_, p) = proposal(bytes);
    let path = format!("layer[0]/tagged/{key}");
    assert_eq!(first_layer(&p).kind, "psd_adjustment", "{what}");
    assert!(codes(&p, &path).contains(&reason), "{what}: {reason} not in {:?}", codes(&p, &path));
    assert!(p.loss.has_code("adjustment_layer_not_applied") && p.raster_records.layers.is_empty(), "{what}");
}

/// A numeric item of a descriptor (`doub` / `UntF` hold any bit pattern: NaN and the infinities) or an
/// out-of-range number refuses THAT adjustment or effect with a fixed reason and a typed loss entry; the
/// import itself always succeeds, the record set validates, valid siblings still map, and the source
/// bytes stay exact. The `CgEd` brightness/contrast case is the R7 review B finding: it used to reach
/// Folio as `BrightnessContrast { brightness: NaN }` and fail the WHOLE import (`raster_records_rejected`).
#[test]
fn non_finite_descriptor_numbers_are_typed_losses_not_import_failures() {
    bounded_case("non_finite_descriptor_numbers_are_typed_losses_not_import_failures", || {
        // positive controls: in-range values map, the bounds themselves (+-150) are inside
        for (b, c, bf, cf) in [(40, -10, 40.0, -10.0), (150, -150, 150.0, -150.0), (-150, 150, -150.0, 150.0)] {
            let (_, p) = proposal(&cged_doc(v_long(b), v_long(c)));
            assert!(matches!(
                p.raster_records.layers[0].adjustment,
                Some(StudioAdjustment::BrightnessContrast { brightness, contrast, use_legacy: false }) if brightness == bf && contrast == cf
            ));
            assert_valid(&p);
        }

        // CgEd Brgh / Cntr: NaN (also with a payload), +inf, -inf as `doub` and `UntF`, out of the +-150 range
        // as `doub`, `UntF` and `long`. The valid `brit` fallback must not be used (no silent substitution).
        let payload_nan = f64::from_bits(0x7ff8_dead_beef_0001);
        let hostile: Vec<(&str, Vec<u8>)> = vec![
            ("doub NaN", v_doub(f64::NAN)),
            ("doub NaN payload", v_doub(payload_nan)),
            ("doub +inf", v_doub(f64::INFINITY)),
            ("doub -inf", v_doub(f64::NEG_INFINITY)),
            ("UntF NaN", v_untf(b"#Nne", f64::NAN)),
            ("UntF +inf", v_untf(b"#Nne", f64::INFINITY)),
            ("UntF -inf", v_untf(b"#Nne", f64::NEG_INFINITY)),
            ("doub 150.5", v_doub(150.5)),
            ("doub -150.5", v_doub(-150.5)),
            ("doub f64::MAX", v_doub(f64::MAX)),
            ("long 1000", v_long(1000)),
            ("long -1000", v_long(-1000)),
            ("long i32::MIN", v_long(i32::MIN)),
        ];
        for (what, value) in &hostile {
            assert_adjustment_refused(&cged_doc(value.clone(), v_long(0)), "brit", "brightness_contrast_range", &format!("Brgh {what}"));
            assert_adjustment_refused(&cged_doc(v_long(0), value.clone()), "brit", "brightness_contrast_range", &format!("Cntr {what}"));
        }
        // the legacy i16 record is held to the same range
        for (b, c) in [(151i16, 0i16), (0, -151), (i16::MIN, 0), (0, i16::MAX)] {
            assert_adjustment_refused(&adjustment_doc(b"brit", &brit_bytes(b, c)), "brit", "brightness_contrast_range", &format!("brit {b} {c}"));
        }

        // exposure (f32 bit patterns): every slot NaN / +inf / -inf, and a gamma that is not > 0
        for (slot, name) in ["exposure", "offset", "gamma"].into_iter().enumerate() {
            for x in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let mut values = [0.0f32, 0.0, 1.0];
                values[slot] = x;
                let record = exposure_record(values[0], values[1], values[2]);
                assert_adjustment_refused(&adjustment_doc(b"expA", &record), "expA", "exposure_range", &format!("expA {name} {x}"));
            }
        }
        for gamma in [0.0f32, -1.0] {
            assert_adjustment_refused(&adjustment_doc(b"expA", &exposure_record(0.0, 0.0, gamma)), "expA", "exposure_range", &format!("expA gamma {gamma}"));
        }

        // hue/saturation (i16 fields): out of the hue, saturation and lightness ranges, colorize saturation below 0
        for (what, record) in [
            ("master hue 181", hue_record(false, [0; 3], [181, 0, 0], [0; 3])),
            ("master saturation 101", hue_record(false, [0; 3], [0, 101, 0], [0; 3])),
            ("master saturation -101", hue_record(false, [0; 3], [0, -101, 0], [0; 3])),
            ("master lightness -101", hue_record(false, [0; 3], [0, 0, -101], [0; 3])),
            ("colorize saturation -1", hue_record(true, [0, -1, 0], [0; 3], [0; 3])),
            ("colorization hue -181", hue_record(false, [-181, 0, 0], [0; 3], [0; 3])),
            ("band saturation 200", hue_record(false, [0; 3], [0; 3], [0, 200, 0])),
            ("band lightness i16::MIN", hue_record(false, [0; 3], [0; 3], [0, 0, i16::MIN])),
        ] {
            assert_adjustment_refused(&adjustment_doc(b"hue2", &record), "hue2", "hue_saturation_range", what);
        }

        // levels: gamma outside 0.10..9.99 (stored x100) and an output past 255
        for (what, row) in [("gamma 9", [0u16, 255, 0, 255, 9]), ("gamma 1000", [0, 255, 0, 255, 1000]), ("output white 256", [0, 255, 0, 256, 100])] {
            assert_adjustment_refused(&adjustment_doc(b"levl", &levl(&[row])), "levl", "levels_record_invalid", what);
        }

        // lfx2: one bad effect value skips THAT effect (typed loss), the next effect still maps; the stack validates
        let effect_cases: Vec<(&str, &str, Vec<u8>, &str)> = vec![
            ("opacity NaN", "Opct", v_untf(b"#Prc", f64::NAN), "effect_value_invalid"),
            ("opacity 101", "Opct", v_untf(b"#Prc", 101.0), "effect_value_invalid"),
            ("distance +inf", "Dstn", v_untf(b"#Pxl", f64::INFINITY), "effect_value_invalid"),
            ("distance -1", "Dstn", v_untf(b"#Pxl", -1.0), "effect_value_invalid"),
            ("size -inf", "blur", v_untf(b"#Pxl", f64::NEG_INFINITY), "effect_value_invalid"),
            ("spread NaN", "Ckmt", v_untf(b"#Prc", f64::NAN), "effect_value_invalid"),
            ("noise +inf", "Nose", v_untf(b"#Prc", f64::INFINITY), "effect_value_invalid"),
            ("angle NaN", "lagl", v_untf(b"#Ang", f64::NAN), "effect_value_invalid"),
            ("angle 181", "lagl", v_untf(b"#Ang", 181.0), "effect_value_invalid"),
            ("colour red NaN", "Clr ", rgb_colour(f64::NAN, 0.0, 0.0), "effect_colour_range"),
            ("colour green -inf", "Clr ", rgb_colour(0.0, f64::NEG_INFINITY, 0.0), "effect_colour_range"),
            ("colour blue 256", "Clr ", rgb_colour(0.0, 0.0, 256.0), "effect_colour_range"),
        ];
        for (what, key, value, reason) in effect_cases {
            let data = lfx2(vec![("DrSh", shadow_with(key, value)), ("DrSh", shadow(7.0))]);
            let (p, c) = lfx2_codes(&data);
            assert!(c.contains(&reason), "{what}: {c:?}");
            let stack = p.raster_records.layers[0].effects.as_ref().unwrap_or_else(|| panic!("{what}: the valid effect still maps"));
            assert_eq!(stack.effects.len(), 1, "{what}");
            assert!(matches!(&stack.effects[0].params, StudioEffectKind::DropShadow { distance, .. } if *distance == 7.0), "{what}");
            assert_valid(&p);
        }
        // a hostile contour point drops the contour (typed loss) and keeps the effect; a good one is kept
        let (p, c) = lfx2_codes(&lfx2(vec![("DrSh", shadow_with("TrnS", contour_with(255.0)))]));
        assert!(!c.contains(&"effect_contour_unmapped") && p.raster_records.layers[0].effects.as_ref().is_some_and(|s| s.effects[0].contour.is_some()), "{c:?}");
        assert_valid(&p);
        for (what, x) in [("NaN", f64::NAN), ("+inf", f64::INFINITY), ("-inf", f64::NEG_INFINITY), ("256", 256.0)] {
            let (p, c) = lfx2_codes(&lfx2(vec![("DrSh", shadow_with("TrnS", contour_with(x)))]));
            assert!(c.contains(&"effect_contour_unmapped"), "contour {what}: {c:?}");
            let stack = p.raster_records.layers[0].effects.as_ref().expect("the effect maps without its contour");
            assert!(stack.effects.len() == 1 && stack.effects[0].contour.is_none(), "contour {what}");
            assert_valid(&p);
        }
        // the stack scale: NaN, +inf, 0 and negative refuse the stack, not the import
        for scale in [f64::NAN, f64::INFINITY, 0.0, -5.0] {
            let (p, c) = lfx2_codes(&lfx2(vec![("Scl ", v_untf(b"#Prc", scale)), ("DrSh", shadow(1.0))]));
            assert!(c.contains(&"effect_scale_invalid"), "scale {scale}: {c:?}");
            assert!(p.raster_records.layers.is_empty(), "scale {scale}");
        }

        // one document: a hostile CgEd layer next to a good layer with effects. The import succeeds, the bad
        // layer is a typed unsupported layer, the good layer keeps its record, and every block stays exact.
        let mut cged = be32(16);
        cged.extend(ds_object("null", vec![("Vrsn", v_long(1)), ("Brgh", v_doub(f64::NAN)), ("Cntr", v_doub(f64::INFINITY))]));
        let mut bad = adjustment_layer(b"brit", &brit_bytes(0, 0));
        bad.blocks.push(layer_block(b"CgEd", &cged, false));
        let good = raster(vec![layer_block(b"lfx2", &lfx2(vec![("DrSh", shadow(3.0))]), false)]);
        let bytes = doc_with_layers(vec![bad, good]);
        let (doc, p) = proposal(&bytes);
        let layers = &p.snapshot.document().layers;
        let bad_layer = layers.iter().find(|l| l.name == "adj").expect("hostile adjustment layer");
        let good_layer = layers.iter().find(|l| l.name == "px").expect("good raster layer");
        assert_eq!(bad_layer.kind, "psd_adjustment");
        assert!(codes(&p, "layer[0]/tagged/brit").contains(&"brightness_contrast_range"));
        let stack = p.raster_records.layer(&good_layer.layer_id).and_then(|r| r.effects.as_ref()).expect("the good layer keeps its effect record");
        assert_eq!(stack.effects.len(), 1);
        assert_valid(&p);
        assert_eq!(write_psd(&doc, &Limits::default()).unwrap(), bytes, "the hostile block stays byte-exact");
    });
}

/// A channel table that names one channel id twice, with a non-default Blend-If range on both entries,
/// would give Folio two ranges for one channel; Folio refuses that and the whole import used to fail
/// (`raster_records_rejected`). Now the blend-ranges record is a typed loss on the layer and the import
/// succeeds; a repeated id whose second range is the default adds nothing and is not refused.
#[test]
fn duplicate_channel_ids_in_blend_ranges_are_a_typed_loss_not_an_import_failure() {
    bounded_case("duplicate_channel_ids_in_blend_ranges_are_a_typed_loss_not_an_import_failure", || {
        const FULL: [u8; 4] = [0, 0, 255, 255];
        const NARROW: [u8; 4] = [10, 20, 200, 210];
        let ranges = |pairs: [[u8; 4]; 3]| -> Vec<u8> {
            let mut v = [FULL, FULL].concat();
            for source in pairs {
                v.extend(source);
                v.extend(FULL);
            }
            v
        };
        let layer_with = |ids: [i16; 3], ranges: Vec<u8>| {
            let mut l = raster(Vec::new());
            l.channels = ids.iter().map(|id| (*id, 0, vec![10])).collect();
            l.ranges = ranges;
            doc_with(l)
        };
        // ids 0, 0, 2 with the first two ranges narrowed: refused, typed, import succeeds
        let (_, p) = proposal(&layer_with([0, 0, 2], ranges([NARROW, NARROW, FULL])));
        assert!(codes(&p, "layer[0]/blend_ranges").contains(&"blend_ranges_duplicate_channel"));
        assert!(p.raster_records.layers.iter().all(|r| r.advanced_blending.is_none()));
        assert_valid(&p);
        // the same ids with only the first narrowed: nothing collides, the range maps
        let (_, p) = proposal(&layer_with([0, 0, 2], ranges([NARROW, FULL, FULL])));
        assert!(!p.loss.has_code("blend_ranges_duplicate_channel"));
        let blending = p.raster_records.layers[0].advanced_blending.as_ref().expect("the red range maps");
        assert_eq!(blending.blend_ranges.len(), 1);
        assert_valid(&p);
    });
}
