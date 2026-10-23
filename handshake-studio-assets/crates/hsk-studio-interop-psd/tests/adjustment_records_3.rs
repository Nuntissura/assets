//! Lane SMB-PS-ADJUST-STYLES-2: PSD import of `grdm` (gradient map) and `clrL` (colour lookup)
//! into Folio records. Two tests: Photoshop-saved corpus files (psd-tools `layers/*.psd`, pinned by
//! byte length; NOT_PROVEN when absent) and hand-built hostile records, including the `.cube` text
//! of a selected lookup table (escape / comment-decoy / case / whitespace cases, V4-08). These prove
//! the byte mapping only; Photoshop display parity is UNVERIFIED (D5), and the descriptor keys of a
//! selected lookup table are UNVERIFIED (no corpus file carries one).
mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    ADJUSTMENT_SCHEMA, Budget, Resolution, Resolver, StudioAdjustment, StudioLutKind, TileRef,
    validate_raster_records,
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
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        kind == "adjustment" && schema_id == ADJUSTMENT_SCHEMA
    }
}
fn budget() -> Budget {
    Budget { input_bytes: 8 << 20, payload_bytes: 4 << 20, ..Budget::default() }
}
fn proposal(bytes: &[u8]) -> ImportProposal {
    let limits = Limits::default();
    let doc = read(bytes);
    let cfg = ProposalConfig::new(&limits, budget(), &Host);
    let p = to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0), None, &CancellationToken::default())
        .expect("proposal");
    let issues = validate_raster_records(&p.raster_records, p.snapshot.document(), budget(), &CancellationToken::default(), &Host)
        .expect("raster records validate");
    assert!(issues.is_empty());
    p
}
/// The adjustment record of the proposal (any layer).
fn adjustment(p: &ImportProposal) -> Option<StudioAdjustment> {
    p.raster_records.layers.iter().find_map(|l| l.adjustment.clone())
}
fn adjustment_doc(key: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut l = TLayer::new("adj", [0, 0, 0, 0]);
    l.channels = empty_channels(&[0, 1, 2]);
    l.blocks = vec![layer_block(key, data, false)];
    let info = layer_info(&[l], false, false);
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
fn codes(p: &ImportProposal, key: &str) -> Vec<&'static str> {
    let path = format!("layer[0]/tagged/{key}");
    p.loss.entries.iter().filter(|e| e.path == path).map(|e| e.code).collect()
}

// Photoshop descriptor builders (independent of the crate's reader; as in adjustment_records.rs).
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
fn v_enum(type_id: &str, value: &str) -> Vec<u8> {
    let mut v = b"enum".to_vec();
    v.extend(ds_key(type_id));
    v.extend(ds_key(value));
    v
}
fn v_data(bytes: &[u8]) -> Vec<u8> {
    let mut v = b"tdta".to_vec();
    v.extend(be32(bytes.len() as u32));
    v.extend_from_slice(bytes);
    v
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


fn rgb(r: f64, g: f64, b: f64) -> hsk_studio_folio::StudioColor {
    hsk_studio_folio::StudioColor { r, g, b, colour_profile_id: hsk_studio_folio::ProfileBinding::Unassigned(()) }
}
fn stop(position: f64, colour: hsk_studio_folio::StudioColor) -> hsk_studio_folio::StudioGradientStop {
    hsk_studio_folio::StudioGradientStop { position, midpoint: Some(50.0), opacity: 100.0, colour }
}
/// The side-2 identity cube, red fastest.
fn identity_cube() -> Vec<[f64; 3]> {
    let mut v = Vec::new();
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                v.push([f64::from(r), f64::from(g), f64::from(b)]);
            }
        }
    }
    v
}

#[test]
fn photoshop_saved_gradient_maps_and_colour_lookup_map() {
    // Byte lengths of the psd-tools files read on 2026-10-15 (the pin of this test). Every
    // gradient-map file holds the "Foreground to Background" gradient (bytes inspected 2026-10-15):
    // RGB (65535, 0, 0) at location 0 and (65535, 65535, 65535) at 4096, midpoints 50, smoothness
    // 4096, two opaque transparency stops; v3 adds the method key.
    let files = [
        ("gradient-map.psd", 22_954usize, Some("gradient_map_smoothness_as_linear")),
        ("gradient-map-v3-classic.psd", 22_958, Some("gradient_map_smoothness_as_linear")),
        ("gradient-map-v3-linear.psd", 22_958, None),
        ("gradient-map-v3-perceptual.psd", 22_958, Some("gradient_map_perceptual_as_linear")),
    ];
    for (name, len, note) in files {
        let bytes = match corpus_file(&format!("psd-tools/psd_files/layers/{name}")) {
            Ok(b) if b.len() == len => b,
            Ok(b) => {
                println!("NOT_PROVEN: photoshop_saved_gradient_maps_and_colour_lookup_map {name} reason=length {} != {len}", b.len());
                continue;
            }
            Err(reason) => {
                println!("NOT_PROVEN: photoshop_saved_gradient_maps_and_colour_lookup_map {name} reason={reason}");
                continue;
            }
        };
        let p = proposal(&bytes);
        assert_eq!(
            adjustment(&p),
            Some(StudioAdjustment::GradientMap {
                stops: vec![stop(0.0, rgb(1.0, 0.0, 0.0)), stop(100.0, rgb(1.0, 1.0, 1.0))],
                reverse: false,
                dither: false,
            }),
            "{name}"
        );
        let got = codes(&p, "grdm");
        for code in ["gradient_map_smoothness_as_linear", "gradient_map_perceptual_as_linear"] {
            assert_eq!(got.contains(&code), note == Some(code), "{name}: {got:?}");
        }
        assert!(!got.contains(&"gradient_map_transparency_ignored"));
    }
    // The colour-lookup file selects no table (an empty descriptor): the identity cube.
    match corpus_file("psd-tools/psd_files/layers/color-lookup.psd") {
        Ok(bytes) if bytes.len() == 22_806 => {
            let p = proposal(&bytes);
            assert_eq!(
                adjustment(&p),
                Some(StudioAdjustment::ColourLookup {
                    lut: "psd-clrL".into(),
                    lut_kind: StudioLutKind::Lut3d,
                    dither: false,
                    table: Some(hsk_studio_folio::StudioLutTable { side: 2, values: identity_cube() }),
                })
            );
            assert!(codes(&p, "clrL").contains(&"colour_lookup_unset_as_identity"));
        }
        Ok(b) => println!("NOT_PROVEN: photoshop_saved_gradient_maps_and_colour_lookup_map color-lookup.psd reason=length {}", b.len()),
        Err(reason) => println!("NOT_PROVEN: photoshop_saved_gradient_maps_and_colour_lookup_map color-lookup.psd reason={reason}"),
    }
}

/// A `grdm` block: version 1, flags, the name "g", colour stops `(location, midpoint, space, rgb)`,
/// transparency stops `(location, opacity)`, then expansion 2, smoothness, length 32, `mode`.
fn grdm(colour: &[(u32, u32, u16, [u16; 3])], alpha: &[(u32, u16)], mode: u16) -> Vec<u8> {
    let mut v = be16(1);
    v.extend([1, 0]); // reversed, not dithered
    v.extend(be32(1));
    v.extend(be16(u16::from(b'g')));
    v.extend(be16(colour.len() as u16));
    for (location, midpoint, space, c) in colour {
        v.extend(be32(*location));
        v.extend(be32(*midpoint));
        v.extend(be16(*space));
        for x in c {
            v.extend(be16(*x));
        }
        v.extend([0, 0, 0, 0]);
    }
    v.extend(be16(alpha.len() as u16));
    for (location, opacity) in alpha {
        v.extend(be32(*location));
        v.extend(be32(50));
        v.extend(be16(*opacity));
    }
    v.extend(be16(2));
    v.extend(be16(0)); // smoothness 0
    v.extend(be16(32));
    v.extend(be16(mode));
    v.extend([0; 4 + 2 + 2 + 4 + 2 + 8 + 8 + 2]);
    v
}
/// A `clrL` block: version 1, descriptor version 16, a descriptor with `items`.
fn clrl(items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = be16(1);
    v.extend(be32(16));
    v.extend(ds_object("null", items));
    v
}
fn cube_clrl(cube: &str) -> Vec<u8> {
    clrl(vec![
        ("lookupType", v_enum("colorLookupType", "3DLUT")),
        ("LUTFormat", v_enum("LUTFormatType", "LUTFormatCUBE")),
        ("LUT3DFileData", v_data(cube.as_bytes())),
    ])
}

#[test]
fn hand_built_gradient_maps_and_lookup_tables_map_and_hostile_ones_are_typed() {
    // Three stops, reversed, one translucent transparency stop (ignored, noted): positions 0, 25
    // (location 1024), 100; midpoint 30 kept.
    let g = grdm(&[(0, 50, 0, [0, 0, 0]), (1024, 30, 0, [65535, 32768, 0]), (4096, 50, 0, [0, 0, 65535])], &[(0, 255), (4096, 128)], 0);
    let p = proposal(&adjustment_doc(b"grdm", &g));
    let Some(StudioAdjustment::GradientMap { stops, reverse: true, dither: false }) = adjustment(&p) else { panic!("{:?}", p.loss.entries) };
    assert_eq!(stops.iter().map(|s| (s.position, s.midpoint)).collect::<Vec<_>>(), vec![(0.0, Some(50.0)), (25.0, Some(30.0)), (100.0, Some(50.0))]);
    assert_eq!((stops[1].colour.r, stops[1].colour.g), (1.0, 32768.0 / 65535.0));
    assert!(codes(&p, "grdm").contains(&"gradient_map_transparency_ignored"));
    // Hostile and unmapped gradient maps: typed losses, no record.
    let mut huge = grdm(&[(0, 50, 0, [0; 3]), (4096, 50, 0, [0; 3])], &[], 0);
    let at = 4 + 4 + 2; // the colour-stop count after version, flags and the one-unit name
    huge[at..at + 2].copy_from_slice(&be16(0xffff));
    for (data, reason) in [
        (huge, "gradient_map_stop_count"),
        (grdm(&[(0, 50, 0, [0; 3])], &[], 0), "gradient_map_stop_count"),
        (grdm(&[(0, 50, 0, [0; 3]), (5000, 50, 0, [0; 3])], &[], 0), "gradient_map_range"),
        (grdm(&[(0, 5, 0, [0; 3]), (4096, 50, 0, [0; 3])], &[], 0), "gradient_map_midpoint_range"),
        (grdm(&[(0, 50, 7, [0; 3]), (4096, 50, 0, [0; 3])], &[], 0), "gradient_map_colour_space_unmapped"),
        (grdm(&[(4096, 50, 0, [0; 3]), (0, 50, 0, [0; 3])], &[], 0), "gradient_map_order"),
        (grdm(&[(0, 50, 0, [0; 3]), (4096, 50, 0, [0; 3])], &[], 1), "gradient_map_noise_unmapped"),
        (grdm(&[(0, 50, 0, [0; 3]), (4096, 50, 0, [0; 3])], &[], 0)[..30].to_vec(), "gradient_map_truncated"),
    ] {
        let p = proposal(&adjustment_doc(b"grdm", &data));
        assert_eq!(adjustment(&p), None, "{reason}");
        assert!(codes(&p, "grdm").contains(&reason), "{reason}: {:?}", p.loss.entries);
    }

    // A selected .cube table (side 2, a red-blue swap), with a title holding '#', comments, a
    // comment line that looks like a keyword (decoy), CRLF line ends and tab separators.
    let cube = "TITLE \"swap # not a comment\"\r\n# LUT_3D_SIZE 33\r\nLUT_3D_SIZE 2\r\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n\
                0 0 0\n0 0 1\n0 1 0\n0 1 1 # trailing comment\n1 0 0\n1\t0\t1\n1 1 0\n1 1 1\n";
    let p = proposal(&adjustment_doc(b"clrL", &cube_clrl(cube)));
    let Some(StudioAdjustment::ColourLookup { lut_kind: StudioLutKind::Lut3d, table: Some(t), dither: false, .. }) = adjustment(&p) else {
        panic!("{:?}", p.loss.entries)
    };
    assert_eq!(t.side, 2);
    assert_eq!(t.values[1], [0.0, 0.0, 1.0], "red input -> blue output (red fastest)");
    assert_eq!(t.values[4], [1.0, 0.0, 0.0]);
    // Hostile tables: size above 65 before any entry is read, a short table, lower-case keyword
    // (keywords are case-sensitive), an escaped title outside the header, a value above 1, a 1D
    // table, another domain, a non-number; other lookup kinds and formats.
    let body8 = "0 0 0\n0 0 1\n0 1 0\n0 1 1\n1 0 0\n1 0 1\n1 1 0\n1 1 1\n";
    for (data, reason) in [
        (cube_clrl("LUT_3D_SIZE 66\n0 0 0\n"), "colour_lookup_cube_size"),
        (cube_clrl("LUT_3D_SIZE 4294967295\n"), "colour_lookup_cube_invalid"),
        (cube_clrl("LUT_3D_SIZE 2\n0 0 0\n"), "colour_lookup_cube_invalid"),
        (cube_clrl(&format!("lut_3d_size 2\n{body8}")), "colour_lookup_cube_invalid"),
        (cube_clrl(&format!("LUT_3D_SIZE 2\n0 0 0\nTITLE \"late\"\n{body8}")), "colour_lookup_cube_invalid"),
        (cube_clrl(&format!("LUT_3D_SIZE 2\n{}", body8.replace("1 1 1", "1 1 1.5"))), "colour_lookup_cube_range"),
        (cube_clrl("LUT_1D_SIZE 2\n0 0 0\n1 1 1\n"), "colour_lookup_cube_1d"),
        (cube_clrl(&format!("DOMAIN_MAX 2 2 2\nLUT_3D_SIZE 2\n{body8}")), "colour_lookup_cube_domain"),
        (cube_clrl(&format!("LUT_3D_SIZE 2\n{}", body8.replace("0 1 0", "0 x 0"))), "colour_lookup_cube_invalid"),
        (cube_clrl(&format!("LUT_3D_SIZE 2\n{body8}0 0 0\n")), "colour_lookup_cube_invalid"),
        (clrl(vec![("lookupType", v_enum("colorLookupType", "abstractProfile"))]), "colour_lookup_profile_unmapped"),
        (
            clrl(vec![("lookupType", v_enum("colorLookupType", "3DLUT")), ("LUTFormat", v_enum("LUTFormatType", "LUTFormat3DL"))]),
            "colour_lookup_format_unmapped",
        ),
        (clrl(vec![("lookupType", v_enum("colorLookupType", "3DLUT")), ("LUTFormat", v_enum("LUTFormatType", "LUTFormatCUBE"))]), "colour_lookup_table_missing"),
        (cube_clrl(body8)[..20].to_vec(), "colour_lookup_descriptor_unreadable"),
    ] {
        let p = proposal(&adjustment_doc(b"clrL", &data));
        assert_eq!(adjustment(&p), None, "{reason}");
        assert!(codes(&p, "clrL").contains(&reason), "{reason}: {:?}", p.loss.entries);
    }
}
