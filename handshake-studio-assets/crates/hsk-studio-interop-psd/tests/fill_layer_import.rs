//! Lane SMB-PS-FILL-LAYERS: PSD fill layers (`SoCo` solid colour, `GdFl` gradient fill, `PtFl`
//! pattern fill) import as Folio `fill` layers with a `StudioFillLayer` record instead of an opaque
//! preserve (spec STU-RAS-007 `fill`, STU-RAS-025/160/161). Tests: hand-built descriptors for every
//! mapped field, refusal and hostile shape (V4-01/02: truncation at every length, hostile counts,
//! depth, stop-count ceiling), the unchanged export of the mapped document, and Photoshop-saved
//! psd-tools corpus files (pinned by byte length; NOT_PROVEN when absent). These prove the byte
//! mapping and, through pigment's reference, the gradient semantics of the imported record;
//! Photoshop display parity is UNVERIFIED (no D5 case covers fill layers).
mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, Payload, ProfileBinding, Resolution, Resolver, StudioFill, StudioFillGradientStyle, TileRef,
    validate_raster_records,
};
use hsk_studio_interop_psd::*;
use hsk_studio_pigment::effect_math::{
    EffectOp, EffectPlane, GradientOverlayOp, GradientStopOp, GradientStyle, LayerParams, NoPatterns,
    render_layer_reference_with,
};
use hsk_studio_pigment::render_plan::StudioBlendMode;

struct Mint(u64, bool);
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
    fn pattern_id(&mut self, unique_id: &str) -> Option<String> {
        self.1.then(|| format!("SPAT-{unique_id}"))
    }
}
/// A host that binds the three fill schemas to `fill` layers, or none (`bind == false`).
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
        self.bind
            && kind == "fill"
            && ["hsk.studio.swatch@1", "hsk.studio.gradient@1", "hsk.studio.pattern@1"].contains(&schema_id)
    }
}
fn budget() -> Budget {
    Budget { input_bytes: 8 << 20, payload_bytes: 4 << 20, ..Budget::default() }
}
fn proposal_with(bytes: &[u8], bind: bool, mint_patterns: bool) -> ImportProposal {
    let limits = Limits::default();
    let doc = read(bytes);
    let host = Host { bind };
    let cfg = ProposalConfig::new(&limits, budget(), &host);
    let p = to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0, mint_patterns), None, &CancellationToken::default())
        .expect("proposal");
    if !p.raster_records.layers.is_empty() {
        let issues = validate_raster_records(&p.raster_records, p.snapshot.document(), budget(), &CancellationToken::default(), &host)
            .expect("raster records validate");
        assert!(issues.is_empty());
    }
    p
}
fn proposal(bytes: &[u8]) -> ImportProposal {
    proposal_with(bytes, true, true)
}

// Photoshop descriptor builders (independent of the crate's reader).
fn ds_key(k: &str) -> Vec<u8> {
    let mut v = if k.len() == 4 { be32(0) } else { be32(k.len() as u32) };
    v.extend_from_slice(k.as_bytes());
    v
}
fn ds_object(class: &str, items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = be32(0);
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
fn v_text(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
    let mut v = be32(units.len() as u32);
    for u in units {
        v.extend(be16(u));
    }
    tagged(b"TEXT", v)
}
fn v_list(items: Vec<Vec<u8>>) -> Vec<u8> {
    let mut v = be32(items.len() as u32);
    for i in items {
        v.extend(i);
    }
    tagged(b"VlLs", v)
}
/// A block payload: version 16 and one descriptor.
fn block(items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = be32(16);
    v.extend(ds_object("null", items));
    v
}
fn rgbc(r: f64, g: f64, b: f64) -> Vec<u8> {
    v_objc("RGBC", vec![("Rd  ", v_doub(r)), ("Grn ", v_doub(g)), ("Bl  ", v_doub(b))])
}
fn colour_stop(rgb: (f64, f64, f64), location: i32, midpoint: i32) -> Vec<u8> {
    v_objc(
        "Clrt",
        vec![("Clr ", rgbc(rgb.0, rgb.1, rgb.2)), ("Type", v_enum("Clry", "UsrS")), ("Lctn", v_long(location)), ("Mdpn", v_long(midpoint))],
    )
}
fn opacity_stop(opacity: f64, location: i32, midpoint: i32) -> Vec<u8> {
    v_objc("TrnS", vec![("Opct", v_untf(b"#Prc", opacity)), ("Lctn", v_long(location)), ("Mdpn", v_long(midpoint))])
}
fn grad(colours: Vec<Vec<u8>>, opacities: Vec<Vec<u8>>) -> Vec<u8> {
    v_objc(
        "Grdn",
        vec![
            ("Nm  ", v_text("g")),
            ("GrdF", v_enum("GrdF", "CstS")),
            ("Intr", v_doub(4096.0)),
            ("Clrs", v_list(colours)),
            ("Trns", v_list(opacities)),
        ],
    )
}
fn red_to_blue() -> Vec<u8> {
    grad(
        vec![colour_stop((255.0, 0.0, 0.0), 0, 50), colour_stop((0.0, 0.0, 255.0), 4096, 50)],
        vec![opacity_stop(100.0, 0, 50), opacity_stop(100.0, 4096, 50)],
    )
}
fn gradient_block(style: &str, extra: Vec<(&str, Vec<u8>)>, g: Vec<u8>) -> Vec<u8> {
    let mut items = vec![("Grad", g), ("Type", v_enum("GrdT", style))];
    items.extend(extra);
    block(items)
}

/// A 4 x 2 RGB document with one layer carrying `blocks` (and the global blocks `globals`).
fn doc_with(blocks: Vec<Vec<u8>>, globals: &[Vec<u8>]) -> Vec<u8> {
    let mut l = TLayer::new("fill", [0, 0, 0, 0]);
    l.channels = empty_channels(&[0, 1, 2]);
    l.blocks = blocks;
    let info = layer_info(&[l], false, false);
    Doc {
        version: 1,
        channels: 3,
        height: 2,
        width: 4,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: layer_section(&info, globals, false),
        image: Some((0, vec![1; 24])),
    }
    .bytes()
}
fn fill_doc(key: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    doc_with(vec![layer_block(key, payload, false)], &[])
}
fn codes(p: &ImportProposal, key: &str) -> Vec<&'static str> {
    let path = format!("layer[0]/tagged/{key}");
    p.loss.entries.iter().filter(|e| e.path == path).map(|e| e.code).collect()
}
/// The fill layer's wire kind in the proposal's snapshot (`fill`, or the preserved `psd_fill`).
fn layer_kinds(p: &ImportProposal) -> Vec<String> {
    p.snapshot.document().layers.iter().map(|l| l.kind.clone()).collect()
}
fn fill_of(p: &ImportProposal) -> hsk_studio_folio::StudioFillLayer {
    assert_eq!(layer_kinds(p), ["fill"], "{:?}", p.loss.entries);
    p.raster_records.layers.iter().find_map(|l| l.fill_layer.clone()).expect("fill record")
}
fn refused(key: &[u8; 4], payload: &[u8], code: &str) {
    let p = proposal(&fill_doc(key, payload));
    assert_eq!(layer_kinds(&p), ["psd_fill"], "{code}: {:?}", p.loss.entries);
    assert!(
        p.loss.entries.iter().any(|e| e.path == "layer[0]" && e.code == "fill_layer_unsupported_payload"),
        "{code}: {:?}",
        p.loss.entries
    );
    assert!(p.loss.entries.iter().any(|e| e.code == code), "{code} missing in {:?}", p.loss.entries);
    assert!(p.raster_records.layers.iter().all(|l| l.fill_layer.is_none()), "{code}: no record on a refusal");
}

#[test]
fn solid_fill_maps_to_a_fill_layer_with_its_record_and_exports_unchanged() {
    let bytes = fill_doc(b"SoCo", &block(vec![("Clr ", rgbc(255.0, 128.0, 0.0))]));
    let p = proposal(&bytes);
    let f = fill_of(&p);
    assert_eq!((f.bounds.x, f.bounds.y, f.bounds.width, f.bounds.height), (0, 0, 4, 2));
    let StudioFill::Solid { colour } = &f.fill else { panic!("{f:?}") };
    assert_eq!((colour.r, colour.g, colour.b), (1.0, 128.0 / 255.0, 0.0));
    assert!(matches!(colour.colour_profile_id, ProfileBinding::Unassigned(())));
    let layer = &p.snapshot.document().layers[0];
    let Payload::PrimitiveReference { schema_id, object_id } = &layer.payload else { panic!("{:?}", layer.payload) };
    assert_eq!((schema_id.as_str(), object_id.as_str()), ("hsk.studio.swatch@1", layer.layer_id.as_str()));
    let loss = codes(&p, "SoCo");
    assert!(loss.contains(&"fill_mapped_to_record"), "{loss:?}");
    assert!(!loss.contains(&"fill_layer_not_applied") && !loss.contains(&"fill_layer_unsupported_payload"), "{loss:?}");
    // The block stays byte-exact: an unedited export reproduces the file.
    let doc = read(&bytes);
    let limits = Limits::default();
    let (out, report) = export_psd(&doc, &p, &p.snapshot, &ExportOptions { limits: &limits, tiles: None }).expect("export");
    assert_eq!(out, bytes, "{:?}", report.loss.entries);
}

#[test]
fn gradient_fill_maps_every_field() {
    // Every style, angle, scale, reverse, dither, align, offset; colour stops only (opacity 100).
    for (token, style) in [
        ("Lnr ", StudioFillGradientStyle::Linear),
        ("Rdl ", StudioFillGradientStyle::Radial),
        ("Angl", StudioFillGradientStyle::Angle),
        ("Rflc", StudioFillGradientStyle::Reflected),
        ("Dmnd", StudioFillGradientStyle::Diamond),
    ] {
        let extra = vec![
            ("Angl", v_untf(b"#Ang", -60.0)),
            ("Scl ", v_untf(b"#Prc", 50.0)),
            ("Rvrs", v_bool(true)),
            ("Dthr", v_bool(true)),
            ("Algn", v_bool(false)),
            ("Ofst", v_objc("Pnt ", vec![("Hrzn", v_untf(b"#Prc", 10.0)), ("Vrtc", v_untf(b"#Prc", -20.0))])),
        ];
        let p = proposal(&fill_doc(b"GdFl", &gradient_block(token, extra, red_to_blue())));
        let f = fill_of(&p);
        let StudioFill::Gradient { style: s, stops, angle, scale, reverse, dither, align_with_layer, offset_x, offset_y } = &f.fill
        else {
            panic!("{f:?}")
        };
        assert_eq!(*s, style);
        assert_eq!((*angle, *scale, *reverse, *dither, *align_with_layer, *offset_x, *offset_y), (-60.0, 50.0, true, true, false, 10.0, -20.0));
        assert_eq!(stops.len(), 2);
        assert_eq!((stops[0].position, stops[0].midpoint, stops[0].opacity), (0.0, Some(50.0), 100.0));
        assert_eq!((stops[0].colour.r, stops[0].colour.g, stops[0].colour.b), (1.0, 0.0, 0.0));
        assert_eq!((stops[1].position, stops[1].colour.b), (100.0, 1.0));
        let Payload::PrimitiveReference { schema_id, .. } = &p.snapshot.document().layers[0].payload else { panic!() };
        assert_eq!(schema_id, "hsk.studio.gradient@1");
        assert!(codes(&p, "GdFl").contains(&"fill_mapped_to_record"));
        assert!(!codes(&p, "GdFl").contains(&"gradient_fill_transparency_merged"));
    }
    // Keys at their format default are omitted by Photoshop: angle 0, scale 100, no reverse or dither,
    // aligned, offset 0.
    let p = proposal(&fill_doc(b"GdFl", &gradient_block("Lnr ", vec![], red_to_blue())));
    let StudioFill::Gradient { angle, scale, reverse, dither, align_with_layer, offset_x, offset_y, .. } = fill_of(&p).fill else {
        panic!()
    };
    assert_eq!((angle, scale, reverse, dither, align_with_layer, offset_x, offset_y), (0.0, 100.0, false, false, true, 0.0, 0.0));
}

#[test]
fn colour_and_transparency_stop_lists_merge_at_the_union_of_their_locations() {
    // Colours red (0) and blue (4096); opacities 100 (0), 50 (2048), 100 (4096): three stops, the
    // middle one the colour midpoint at half opacity.
    let g = grad(
        vec![colour_stop((255.0, 0.0, 0.0), 0, 50), colour_stop((0.0, 0.0, 255.0), 4096, 50)],
        vec![opacity_stop(100.0, 0, 50), opacity_stop(50.0, 2048, 50), opacity_stop(100.0, 4096, 50)],
    );
    let p = proposal(&fill_doc(b"GdFl", &gradient_block("Lnr ", vec![], g)));
    let StudioFill::Gradient { stops, .. } = fill_of(&p).fill else { panic!() };
    let seen: Vec<(f64, f64, f64, f64)> = stops.iter().map(|s| (s.position, s.opacity, s.colour.r, s.colour.b)).collect();
    assert_eq!(seen, vec![(0.0, 100.0, 1.0, 0.0), (50.0, 50.0, 0.5, 0.5), (100.0, 100.0, 0.0, 1.0)]);
    assert!(stops.iter().all(|s| s.midpoint == Some(50.0)));
    assert!(codes(&p, "GdFl").contains(&"gradient_fill_transparency_merged"));
    assert!(!codes(&p, "GdFl").contains(&"gradient_fill_midpoints_flattened"));
    // A colour stop away from the ends and a skewed midpoint: flattened with a typed loss.
    let g = grad(
        vec![colour_stop((255.0, 0.0, 0.0), 1024, 30), colour_stop((0.0, 0.0, 255.0), 3072, 50)],
        vec![opacity_stop(100.0, 0, 50), opacity_stop(0.0, 4096, 50)],
    );
    let p = proposal(&fill_doc(b"GdFl", &gradient_block("Lnr ", vec![], g)));
    let StudioFill::Gradient { stops, .. } = fill_of(&p).fill else { panic!() };
    let seen: Vec<(f64, f64)> = stops.iter().map(|s| (s.position, s.opacity)).collect();
    // Opacity falls linearly from 100 at 0 to 0 at 4096: 75 at 1024, 25 at 3072.
    assert_eq!(seen, vec![(0.0, 100.0), (25.0, 75.0), (75.0, 25.0), (100.0, 0.0)]);
    assert!(codes(&p, "GdFl").contains(&"gradient_fill_midpoints_flattened"));
    // Without transparency the colour stops keep their midpoints.
    let g = grad(
        vec![colour_stop((255.0, 0.0, 0.0), 0, 30), colour_stop((0.0, 0.0, 255.0), 4096, 50)],
        vec![opacity_stop(100.0, 0, 50), opacity_stop(100.0, 4096, 50)],
    );
    let p = proposal(&fill_doc(b"GdFl", &gradient_block("Lnr ", vec![], g)));
    let StudioFill::Gradient { stops, .. } = fill_of(&p).fill else { panic!() };
    assert_eq!((stops[0].midpoint, stops[1].midpoint), (Some(30.0), Some(50.0)));
}

fn rgb_2x2(id: &str) -> Vec<u8> {
    let array = |samples: &[u8]| -> Vec<u8> {
        let mut body = be32(8);
        for v in [0u32, 0, 2, 2] {
            body.extend(be32(v));
        }
        body.extend(be16(8));
        body.push(0);
        body.extend_from_slice(samples);
        let mut v = be32(1);
        v.extend(be32(body.len() as u32));
        v.extend(body);
        v
    };
    let mut p = be32(1);
    p.extend(be32(3));
    p.extend(be16(2));
    p.extend(be16(2));
    p.extend(unicode("n\0"));
    p.push(id.len() as u8);
    p.extend_from_slice(id.as_bytes());
    let mut vma = Vec::new();
    for v in [0u32, 0, 2, 2] {
        vma.extend(be32(v));
    }
    vma.extend(be32(3));
    vma.extend(array(&[10, 20, 30, 40]));
    vma.extend(array(&[50, 60, 70, 80]));
    vma.extend(array(&[90, 100, 110, 120]));
    vma.extend(be32(0));
    vma.extend(be32(0));
    p.extend(be32(3));
    p.extend(be32(vma.len() as u32));
    p.extend(vma);
    let mut out = be32(p.len() as u32);
    out.extend(p);
    pad_to(out, 4)
}
fn pattern_block(id: &str, scale: Option<f64>, linked: Option<bool>, phase: Option<(f64, f64)>) -> Vec<u8> {
    let mut items = vec![("Ptrn", v_objc("Ptrn", vec![("Nm  ", v_text("p")), ("Idnt", v_text(id))]))];
    if let Some(s) = scale {
        items.push(("Scl ", v_untf(b"#Prc", s)));
    }
    if let Some(l) = linked {
        items.push(("Algn", v_bool(l)));
    }
    if let Some((x, y)) = phase {
        items.push(("phase", v_objc("Pnt ", vec![("Hrzn", v_doub(x)), ("Vrtc", v_doub(y))])));
    }
    block(items)
}

#[test]
fn pattern_fill_maps_with_its_tile_and_missing_or_unminted_patterns_are_typed() {
    let globals = vec![global_block(b"Patt", &rgb_2x2("p-1"), false)];
    let with = |payload: &[u8], bind: bool, mint: bool| {
        proposal_with(&doc_with(vec![layer_block(b"PtFl", payload, false)], &globals), bind, mint)
    };
    let p = with(&pattern_block("p-1", Some(150.0), Some(false), Some((3.0, -2.0))), true, true);
    let StudioFill::Pattern { pattern, pattern_tile, scale, phase_x, phase_y, link_with_layer } = fill_of(&p).fill else { panic!() };
    assert_eq!((pattern.schema_id.as_str(), pattern.object_id.as_str()), ("hsk.studio.pattern@1", "SPAT-p-1"));
    let tile = pattern_tile.expect("tile");
    assert_eq!((tile.width.value, tile.height.value, tile.format.as_str()), (2.0, 2.0, "rgb_f32le"));
    assert_eq!((scale, phase_x, phase_y, link_with_layer), (150.0, 3.0, -2.0, false));
    assert!(p.pattern_tiles.iter().any(|t| t.pattern_object_id == "SPAT-p-1"));
    // Omitted keys: scale 100, linked, no phase.
    let p = with(&pattern_block("p-1", None, None, None), true, true);
    let StudioFill::Pattern { scale, phase_x, phase_y, link_with_layer, .. } = fill_of(&p).fill else { panic!() };
    assert_eq!((scale, phase_x, phase_y, link_with_layer), (100.0, 0.0, 0.0, true));
    // Typed refusals keep the layer preserved.
    let p = with(&pattern_block("other", None, None, None), true, true);
    assert_eq!(layer_kinds(&p), ["psd_fill"]);
    assert!(p.loss.has_code("pattern_fill_pattern_missing"), "{:?}", p.loss.entries);
    let p = with(&pattern_block("p-1", None, None, None), true, false);
    assert!(p.loss.has_code("pattern_fill_pattern_unminted"), "{:?}", p.loss.entries);
    let p = with(&pattern_block("p-1", Some(0.0), None, None), true, true);
    assert!(p.loss.has_code("pattern_fill_range"));
    let p = with(&block(vec![("Scl ", v_untf(b"#Prc", 100.0))]), true, true);
    assert!(p.loss.has_code("pattern_fill_reference_missing"));
}

#[test]
fn unmapped_fill_content_is_a_typed_loss_and_the_layer_stays_preserved() {
    let solid = |colour: Vec<u8>| block(vec![("Clr ", colour)]);
    let cmyk = v_objc("CMYC", vec![("Cyn ", v_doub(1.0)), ("Mgnt", v_doub(0.0)), ("Ylw ", v_doub(0.0)), ("Blck", v_doub(0.0))]);
    refused(b"SoCo", &solid(cmyk), "fill_colour_model_unmapped");
    refused(b"SoCo", &solid(rgbc(300.0, 0.0, 0.0)), "fill_colour_range");
    refused(b"SoCo", &solid(rgbc(-1.0, 0.0, 0.0)), "fill_colour_range");
    refused(b"SoCo", &solid(rgbc(f64::NAN, 0.0, 0.0)), "fill_value_invalid");
    refused(b"SoCo", &solid(rgbc(f64::INFINITY, 0.0, 0.0)), "fill_value_invalid");
    refused(b"SoCo", &block(vec![]), "fill_colour_missing");
    // A descriptor with the wrong version word.
    let mut wrong = block(vec![("Clr ", rgbc(1.0, 2.0, 3.0))]);
    wrong[3] = 15;
    refused(b"SoCo", &wrong, "descriptor_version_unsupported");

    let noise = v_objc("Grdn", vec![("Nm  ", v_text("n")), ("GrdF", v_enum("GrdF", "ClNs")), ("ShTr", v_bool(false))]);
    refused(b"GdFl", &gradient_block("Lnr ", vec![], noise), "gradient_fill_noise_unmapped");
    refused(b"GdFl", &gradient_block("ShpB", vec![], red_to_blue()), "gradient_fill_style_unmapped");
    refused(b"GdFl", &gradient_block("Lnr ", vec![("Angl", v_untf(b"#Ang", 200.0))], red_to_blue()), "gradient_fill_range");
    refused(b"GdFl", &gradient_block("Lnr ", vec![("Angl", v_doub(10.0))], red_to_blue()), "fill_unit_unexpected");
    refused(b"GdFl", &gradient_block("Lnr ", vec![("Scl ", v_untf(b"#Prc", 0.0))], red_to_blue()), "gradient_fill_range");
    refused(b"GdFl", &gradient_block("Lnr ", vec![("Angl", v_untf(b"#Ang", f64::NAN))], red_to_blue()), "fill_value_invalid");
    refused(b"GdFl", &gradient_block("Lnr ", vec![("Rvrs", v_long(1))], red_to_blue()), "fill_value_invalid");
    // A colour stop midpoint outside Folio's 13..=87.
    let low = grad(
        vec![colour_stop((255.0, 0.0, 0.0), 0, 5), colour_stop((0.0, 0.0, 255.0), 4096, 50)],
        vec![opacity_stop(100.0, 0, 50), opacity_stop(100.0, 4096, 50)],
    );
    refused(b"GdFl", &gradient_block("Lnr ", vec![], low), "gradient_fill_midpoint_range");
    let backwards = grad(
        vec![colour_stop((255.0, 0.0, 0.0), 4096, 50), colour_stop((0.0, 0.0, 255.0), 0, 50)],
        vec![opacity_stop(100.0, 0, 50), opacity_stop(100.0, 4096, 50)],
    );
    refused(b"GdFl", &gradient_block("Lnr ", vec![], backwards), "gradient_fill_order");
    let one = grad(vec![colour_stop((255.0, 0.0, 0.0), 0, 50)], vec![opacity_stop(100.0, 0, 50)]);
    refused(b"GdFl", &gradient_block("Lnr ", vec![], one), "gradient_fill_stop_count");
    let far = grad(
        vec![colour_stop((255.0, 0.0, 0.0), -1, 50), colour_stop((0.0, 0.0, 255.0), 4097, 50)],
        vec![opacity_stop(100.0, 0, 50), opacity_stop(100.0, 4096, 50)],
    );
    refused(b"GdFl", &gradient_block("Lnr ", vec![], far), "gradient_fill_range");
    let over_opacity = grad(
        vec![colour_stop((255.0, 0.0, 0.0), 0, 50), colour_stop((0.0, 0.0, 255.0), 4096, 50)],
        vec![opacity_stop(101.0, 0, 50), opacity_stop(100.0, 4096, 50)],
    );
    refused(b"GdFl", &gradient_block("Lnr ", vec![], over_opacity), "gradient_fill_range");
    // Aggregate bound: more colour stops than the record holds.
    let many = grad(
        (0..300).map(|i| colour_stop((255.0, 0.0, 0.0), i, 50)).collect(),
        vec![opacity_stop(100.0, 0, 50), opacity_stop(100.0, 4096, 50)],
    );
    refused(b"GdFl", &gradient_block("Lnr ", vec![], many), "gradient_fill_stop_count");
    // Missing parts.
    refused(b"GdFl", &block(vec![("Type", v_enum("GrdT", "Lnr "))]), "gradient_fill_gradient_missing");
    refused(b"GdFl", &block(vec![("Grad", red_to_blue())]), "gradient_fill_style_missing");
}

#[test]
fn unknown_fields_are_reported_and_a_vector_mask_keeps_the_fill_preserved() {
    // An unknown top-level key still maps the fields it knows and reports the rest, once.
    let p = proposal(&fill_doc(
        b"SoCo",
        &block(vec![("Clr ", rgbc(0.0, 0.0, 0.0)), ("Nois", v_doub(1.0)), ("Xtra", v_doub(2.0))]),
    ));
    assert_eq!(layer_kinds(&p), ["fill"]);
    assert_eq!(codes(&p, "SoCo").iter().filter(|c| **c == "fill_descriptor_field_unmapped").count(), 1);
    // An unknown gradient field is reported the same way.
    let g = v_objc(
        "Grdn",
        vec![
            ("Nm  ", v_text("g")),
            ("GrdF", v_enum("GrdF", "CstS")),
            ("Intr", v_doub(4096.0)),
            ("Clrs", v_list(vec![colour_stop((1.0, 2.0, 3.0), 0, 50), colour_stop((4.0, 5.0, 6.0), 4096, 50)])),
            ("Trns", v_list(vec![opacity_stop(100.0, 0, 50), opacity_stop(100.0, 4096, 50)])),
            ("Zzzz", v_doub(1.0)),
        ],
    );
    let p = proposal(&fill_doc(b"GdFl", &gradient_block("Lnr ", vec![], g)));
    assert!(codes(&p, "GdFl").contains(&"fill_descriptor_field_unmapped"));
    // A smoothness other than classic is a typed transformation.
    let g = v_objc(
        "Grdn",
        vec![
            ("Nm  ", v_text("g")),
            ("GrdF", v_enum("GrdF", "CstS")),
            ("Intr", v_doub(2048.0)),
            ("Clrs", v_list(vec![colour_stop((1.0, 2.0, 3.0), 0, 50), colour_stop((4.0, 5.0, 6.0), 4096, 50)])),
            ("Trns", v_list(vec![opacity_stop(100.0, 0, 50), opacity_stop(100.0, 4096, 50)])),
        ],
    );
    let p = proposal(&fill_doc(b"GdFl", &gradient_block("Lnr ", vec![], g)));
    assert!(codes(&p, "GdFl").contains(&"gradient_fill_smoothness_as_linear"));
    // A shape layer (a fill with a vector mask) is bounded by its path, which is not applied: the
    // fill must not paint the canvas.
    let blocks = vec![layer_block(b"SoCo", &block(vec![("Clr ", rgbc(255.0, 0.0, 0.0))]), false), layer_block(b"vmsk", &[0u8; 16], false)];
    let p = proposal(&doc_with(blocks, &[]));
    assert_eq!(layer_kinds(&p), ["psd_fill"]);
    assert!(codes(&p, "SoCo").contains(&"fill_layer_vector_mask_not_applied"));
    assert!(p.raster_records.layers.iter().all(|l| l.fill_layer.is_none()));
    // A host that binds no fill primitive keeps the layer preserved with the reason.
    let bytes = fill_doc(b"SoCo", &block(vec![("Clr ", rgbc(255.0, 0.0, 0.0))]));
    let p = proposal_with(&bytes, false, true);
    assert_eq!(layer_kinds(&p), ["psd_fill"]);
    assert!(codes(&p, "SoCo").contains(&"fill_primitive_unresolved"));
}

#[test]
fn truncated_and_hostile_descriptors_never_panic_and_never_map_garbage() {
    // Truncation at every length of a gradient and a solid payload.
    for (key, payload) in [
        (b"GdFl", gradient_block("Rdl ", vec![("Angl", v_untf(b"#Ang", 10.0))], red_to_blue())),
        (b"SoCo", block(vec![("Clr ", rgbc(1.0, 2.0, 3.0))])),
    ] {
        for cut in 0..payload.len() {
            // The block is padded to a multiple of 4 with zero bytes, so a cut within 3 bytes of the
            // end of an item can read as a complete item with zero low bytes; only a cut on a
            // 4-byte boundary is certain to be incomplete. Every cut must import without a panic
            // and every record that does map must validate (`proposal` validates the records).
            let p = proposal(&fill_doc(key, &payload[..cut]));
            if cut % 4 == 0 {
                assert!(
                    p.raster_records.layers.iter().all(|l| l.fill_layer.is_none()),
                    "{} cut {cut} mapped a record",
                    std::str::from_utf8(key).unwrap()
                );
                assert_eq!(layer_kinds(&p), ["psd_fill"], "cut {cut}");
            }
        }
    }
    // A list that claims more items than the block holds.
    let mut lie = block(vec![("Grad", v_objc("Grdn", vec![("GrdF", v_enum("GrdF", "CstS")), ("Clrs", tagged(b"VlLs", be32(u32::MAX)))])), ("Type", v_enum("GrdT", "Lnr "))]);
    let p = proposal(&fill_doc(b"GdFl", &lie));
    assert_eq!(layer_kinds(&p), ["psd_fill"]);
    assert!(p.loss.has_code("descriptor_item_limit"), "{:?}", p.loss.entries);
    // An object claiming more items than bytes.
    lie = be32(16);
    lie.extend(be32(0));
    lie.extend(ds_key("null"));
    lie.extend(be32(u32::MAX));
    let p = proposal(&fill_doc(b"SoCo", &lie));
    assert!(p.loss.has_code("descriptor_item_limit"), "{:?}", p.loss.entries);
    // Nesting beyond the descriptor depth limit.
    let mut nested = v_doub(1.0);
    for _ in 0..12 {
        nested = v_objc("Nest", vec![("Next", nested)]);
    }
    let p = proposal(&fill_doc(b"SoCo", &block(vec![("Clr ", nested)])));
    assert!(p.loss.has_code("descriptor_depth"), "{:?}", p.loss.entries);
    // An empty payload and a payload of version only.
    for payload in [Vec::new(), be32(16)] {
        let p = proposal(&fill_doc(b"SoCo", &payload));
        assert_eq!(layer_kinds(&p), ["psd_fill"]);
    }
}

// ---- Photoshop-saved corpus files (psd-tools, test data only) --------------------------------

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
fn pinned(name: &str, len: usize) -> Option<Vec<u8>> {
    match corpus_file(&format!("psd-tools/psd_files/{name}")) {
        Ok(b) if b.len() == len => Some(b),
        Ok(b) => {
            println!("NOT_PROVEN: photoshop_saved_fill_layers_map {name} reason=length {} != {len}", b.len());
            None
        }
        Err(reason) => {
            println!("NOT_PROVEN: photoshop_saved_fill_layers_map {name} reason={reason}");
            None
        }
    }
}

#[test]
fn photoshop_saved_fill_layers_map() {
    // Byte lengths of the psd-tools files read on 2026-10-22 (the pin of this test); descriptors
    // inspected with an independent decoder.
    for name in ["layers/solid-color-fill.psd", "layers-minimal/solid-color-fill.psd"] {
        let len = if name.starts_with("layers/") { 22_772 } else { 22_662 };
        let Some(bytes) = pinned(name, len) else { continue };
        let p = proposal(&bytes);
        let fills: Vec<_> = p.raster_records.layers.iter().filter_map(|l| l.fill_layer.clone()).collect();
        assert_eq!(fills.len(), 1, "{name}: {:?}", p.loss.entries);
        // Solid red (Rd 255, Grn 0, Bl 0) over the canvas.
        let StudioFill::Solid { colour } = &fills[0].fill else { panic!("{name}") };
        assert_eq!((colour.r, colour.g, colour.b), (1.0, 0.0, 0.0));
        assert!(fills[0].bounds.width > 0 && fills[0].bounds.height > 0);
    }
    for (name, len) in [("layers/gradient-fill.psd", 24_138usize), ("layers-minimal/gradient-fill.psd", 23_370)] {
        let Some(bytes) = pinned(name, len) else { continue };
        let p = proposal(&bytes);
        let fills: Vec<_> = p.raster_records.layers.iter().filter_map(|l| l.fill_layer.clone()).collect();
        assert_eq!(fills.len(), 1, "{name}: {:?}", p.loss.entries);
        // "Color to Transparent": red to red, opacity 100 to 0, linear, angle 90.
        let StudioFill::Gradient { style, stops, angle, scale, reverse, dither, align_with_layer, .. } = &fills[0].fill else {
            panic!("{name}")
        };
        assert_eq!((*style, *angle, *scale, *reverse, *dither, *align_with_layer), (StudioFillGradientStyle::Linear, 90.0, 100.0, false, false, true));
        let seen: Vec<(f64, f64, f64)> = stops.iter().map(|s| (s.position, s.opacity, s.colour.r)).collect();
        assert_eq!(seen, vec![(0.0, 100.0, 1.0), (100.0, 0.0, 1.0)]);
        assert!(p.loss.has_code("gradient_fill_transparency_merged"));
        // The imported record renders as a ramp: pigment's reference of the record's parameters
        // over a transparent backdrop (angle 90 runs bottom to top; the red end is opaque at the
        // bottom and transparent at the top).
        let (w, h) = (6u32, 8u32);
        let ops: Vec<GradientStopOp> =
            stops.iter().map(|s| GradientStopOp { position: (s.position / 100.0) as f32, colour: [s.colour.r as f32, s.colour.g as f32, s.colour.b as f32], opacity: (s.opacity / 100.0) as f32 }).collect();
        let op = GradientOverlayOp::new(StudioBlendMode::Normal, 1.0, GradientStyle::Linear, *angle as f32, &ops).unwrap();
        let n = (w * h) as usize;
        let back = EffectPlane::transparent(w, h).unwrap();
        let shape = EffectPlane::new(w, h, vec![[1.0; 3]; n], vec![1.0; n]).unwrap();
        let out = render_layer_reference_with(
            &back,
            &shape,
            LayerParams { mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 0.0 },
            &[EffectOp::GradientOverlay(op)],
            &NoPatterns,
        )
        .unwrap();
        let (bottom, top) = (out.pixel(2, h - 1).unwrap(), out.pixel(2, 0).unwrap());
        assert!(bottom.a > 0.9 && top.a < 0.1, "bottom {bottom:?} top {top:?}");
        assert!((bottom.rgb[0] - 1.0).abs() < 1e-6 && bottom.rgb[1] == 0.0, "{bottom:?}");
    }
    for (name, len) in [("layers/pattern-fill.psd", 62_934usize), ("layers-minimal/pattern-fill.psd", 58_494)] {
        let Some(bytes) = pinned(name, len) else { continue };
        let p = proposal(&bytes);
        let fills: Vec<_> = p.raster_records.layers.iter().filter_map(|l| l.fill_layer.clone()).collect();
        assert_eq!(fills.len(), 1, "{name}: {:?}", p.loss.entries);
        // "Metallic Flecks" (cf324614-...), 128 x 128; scale, phase and link at their defaults.
        let StudioFill::Pattern { pattern, pattern_tile, scale, phase_x, phase_y, link_with_layer } = &fills[0].fill else {
            panic!("{name}")
        };
        assert!(pattern.object_id.starts_with("SPAT-"), "{}", pattern.object_id);
        assert_eq!(pattern_tile.as_ref().map(|t| (t.width.value, t.height.value)), Some((128.0, 128.0)));
        assert_eq!((*scale, *phase_x, *phase_y, *link_with_layer), (100.0, 0.0, 0.0, true));
    }
    // A shape layer (SoCo + vector mask) and a noise gradient stay preserved with their reasons.
    if let Some(bytes) = pinned("layers-minimal/shape-layer.psd", 24_146) {
        let p = proposal(&bytes);
        assert!(p.loss.has_code("fill_layer_vector_mask_not_applied"), "{:?}", p.loss.entries);
        assert!(p.raster_records.layers.iter().all(|l| l.fill_layer.is_none()));
    }
    if let Some(bytes) = pinned("gradient-styles.psd", 156_939) {
        // Twenty gradient fills, every one with a vector mask (shape layers).
        let p = proposal(&bytes);
        let shapes = p.loss.entries.iter().filter(|e| e.code == "fill_layer_vector_mask_not_applied").count();
        assert_eq!(shapes, 20, "{:?}", p.loss.entries);
    }
    if let Some(bytes) = pinned("gradients/noise-gradient-rgb.psd", 22_680) {
        let p = proposal(&bytes);
        assert!(p.loss.has_code("gradient_fill_noise_unmapped") || p.loss.has_code("fill_layer_vector_mask_not_applied"), "{:?}", p.loss.entries);
        assert!(p.raster_records.layers.iter().all(|l| l.fill_layer.is_none()));
    }
}
