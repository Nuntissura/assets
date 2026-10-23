//! Lane SMB-PS-ADJUST-STYLES slice 1c: PSD import of the second adjustment set (`blnc`, `selc`,
//! `mixr`, `phfl`, `vibA`, `blwh`) into Folio records. Two tests: Photoshop-saved corpus files
//! (psd-tools `layers/*.psd`, pinned by byte length; NOT_PROVEN when absent) and hand-built hostile
//! and edge records. These prove the byte mapping only; Photoshop display parity is UNVERIFIED (D5).
mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    ADJUSTMENT_SCHEMA, Budget, Resolution, Resolver, StudioAdjustment, StudioSelectiveColourMethod, TileRef,
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
fn v_long(value: i32) -> Vec<u8> {
    let mut v = b"long".to_vec();
    v.extend(value.to_be_bytes());
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

#[test]
fn photoshop_saved_second_set_layers_map() {
    // Byte lengths of the psd-tools files read on 2026-10-15 (the pin of this test).
    let files = [
        ("color-balance.psd", 22_814usize),
        ("selective-color.psd", 22_894),
        ("channel-mixer.psd", 22_838),
        ("photo-filter.psd", 22_810),
        ("vibrance.psd", 22_802),
        ("black-and-white.psd", 23_086),
    ];
    for (name, len) in files {
        let bytes = match corpus_file(&format!("psd-tools/psd_files/layers/{name}")) {
            Ok(b) if b.len() == len => b,
            Ok(b) => {
                println!("NOT_PROVEN: photoshop_saved_second_set_layers_map {name} reason=length {} != {len}", b.len());
                continue;
            }
            Err(reason) => {
                println!("NOT_PROVEN: photoshop_saved_second_set_layers_map {name} reason={reason}");
                continue;
            }
        };
        let p = proposal(&bytes);
        let a = adjustment(&p);
        match name {
            // Default dialog states as saved by Photoshop (bytes inspected 2026-10-15): colour
            // balance all zero with preserve luminosity on; selective colour all zero, method 0 =
            // relative; channel mixer identity rows (100 on the diagonal, constants 0).
            "color-balance.psd" => assert_eq!(
                a,
                Some(StudioAdjustment::ColourBalance {
                    shadows: [0.0; 3],
                    midtones: [0.0; 3],
                    highlights: [0.0; 3],
                    preserve_luminosity: true
                })
            ),
            "selective-color.psd" => {
                assert!(matches!(&a, Some(StudioAdjustment::SelectiveColour { method: StudioSelectiveColourMethod::Relative, reds, blacks, .. })
                    if *reds == [0.0; 4] && *blacks == [0.0; 4]), "{a:?}")
            }
            "channel-mixer.psd" => {
                let Some(StudioAdjustment::ChannelMixer { outputs, monochrome: false }) = &a else { panic!("{a:?}") };
                let rows: Vec<(Vec<f64>, f64)> = outputs.iter().map(|r| (r.weights.clone(), r.constant)).collect();
                assert_eq!(
                    rows,
                    vec![(vec![100.0, 0.0, 0.0], 0.0), (vec![0.0, 100.0, 0.0], 0.0), (vec![0.0, 0.0, 100.0], 0.0)]
                );
            }
            // The file stores its filter colour in Lab (colour space 7): a typed loss, not a guess.
            "photo-filter.psd" => {
                assert_eq!(a, None);
                assert!(p.loss.has_code("photo_filter_colour_model_unmapped"), "{:?}", p.loss.entries);
            }
            // An empty descriptor: both values at their default 0.
            "vibrance.psd" => {
                assert_eq!(a, Some(StudioAdjustment::Vibrance { vibrance: 0.0, saturation: 0.0 }))
            }
            // Weights 40/60/40/60/20/80, tint off; tint colour RGB (225, 211, 179): HSL hue
            // (211 - 179) / (225 - 179) x 60 = 41.739 deg, saturation 46 / (510 - 225 - 179) =
            // 46 / 106 = 43.396 %.
            "black-and-white.psd" => {
                let Some(StudioAdjustment::BlackAndWhite {
                    red,
                    yellow,
                    green,
                    cyan,
                    blue,
                    magenta,
                    tint_enabled,
                    tint_hue,
                    tint_saturation,
                }) = a
                else {
                    panic!("{a:?}")
                };
                assert_eq!([red, yellow, green, cyan, blue, magenta], [40.0, 60.0, 40.0, 60.0, 20.0, 80.0]);
                assert!(!tint_enabled);
                assert!((tint_hue - 41.739).abs() < 0.01 && (tint_saturation - 43.396).abs() < 0.01, "{tint_hue} {tint_saturation}");
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn hand_built_records_map_and_hostile_records_are_typed_losses() {
    let i16s = |v: &[i16]| v.iter().flat_map(|x| x.to_be_bytes()).collect::<Vec<u8>>();
    // Colour balance: midtones cyan_red +50, preserve off.
    let mut blnc = i16s(&[0, 0, 0, 50, 0, -20, 0, 10, 0]);
    blnc.extend([0, 0]);
    assert_eq!(
        adjustment(&proposal(&adjustment_doc(b"blnc", &blnc))),
        Some(StudioAdjustment::ColourBalance {
            shadows: [0.0; 3],
            midtones: [50.0, 0.0, -20.0],
            highlights: [0.0, 10.0, 0.0],
            preserve_luminosity: false
        })
    );
    // Out of range (101) and truncated (8 bytes): typed losses, no record.
    let mut wild = blnc.clone();
    wild[1] = 101;
    for (data, reason) in [(&wild[..], "colour_balance_range"), (&blnc[..8], "colour_balance_truncated")] {
        let p = proposal(&adjustment_doc(b"blnc", data));
        assert_eq!(adjustment(&p), None);
        assert!(codes(&p, "blnc").contains(&reason), "{reason}: {:?}", p.loss.entries);
    }
    // Selective colour absolute, reds cyan -25, blacks black +5; method 2 is refused.
    let mut selc = i16s(&[1, 1, 0, 0, 0, 0, -25, 0, 0, 0]);
    selc.extend(i16s(&[0; 28]));
    selc.extend(i16s(&[0, 0, 0, 5]));
    let Some(StudioAdjustment::SelectiveColour { method, reds, blacks, .. }) = adjustment(&proposal(&adjustment_doc(b"selc", &selc)))
    else {
        panic!("selc")
    };
    assert_eq!((method, reds, blacks), (StudioSelectiveColourMethod::Absolute, [-25.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 5.0]));
    let mut bad_method = selc.clone();
    bad_method[3] = 2;
    assert!(codes(&proposal(&adjustment_doc(b"selc", &bad_method)), "selc").contains(&"selective_colour_method"));
    // Channel mixer: a non-zero fourth input column is dropped with a note; 201 is out of range.
    let mut mixr = i16s(&[1, 0]);
    mixr.extend(i16s(&[100, 50, 0, 7, 10, 0, 100, 0, 0, 0, 0, 0, 100, 0, -5, 0, 0, 0, 100, 0]));
    let p = proposal(&adjustment_doc(b"mixr", &mixr));
    let Some(StudioAdjustment::ChannelMixer { outputs, .. }) = adjustment(&p) else { panic!("mixr") };
    assert_eq!((outputs[0].weights.clone(), outputs[0].constant, outputs[2].constant), (vec![100.0, 50.0, 0.0], 10.0, -5.0));
    assert!(codes(&p, "mixr").contains(&"channel_mixer_fourth_channel_dropped"));
    let mut big = mixr.clone();
    big[5] = 201;
    assert!(codes(&proposal(&adjustment_doc(b"mixr", &big)), "mixr").contains(&"channel_mixer_range"));
    // Photo filter version 2 RGB (65535, 32768, 0) density 25 preserve on; XYZ (version 3) and a
    // density of 101 are typed losses.
    let mut phfl = i16s(&[2, 0]);
    phfl.extend([0xff, 0xff, 0x80, 0x00, 0, 0, 0, 0]);
    phfl.extend(25u32.to_be_bytes());
    phfl.extend([1, 0, 0, 0]);
    let Some(StudioAdjustment::PhotoFilter { filter_colour, density, preserve_luminosity }) =
        adjustment(&proposal(&adjustment_doc(b"phfl", &phfl)))
    else {
        panic!("phfl")
    };
    assert_eq!((filter_colour.r, filter_colour.g, filter_colour.b, density, preserve_luminosity), (1.0, 32768.0 / 65535.0, 0.0, 25.0, true));
    let mut xyz = phfl.clone();
    xyz[1] = 3;
    assert!(codes(&proposal(&adjustment_doc(b"phfl", &xyz)), "phfl").contains(&"photo_filter_xyz_colour_unmapped"));
    let mut dense = phfl.clone();
    dense[15] = 101;
    assert!(codes(&proposal(&adjustment_doc(b"phfl", &dense)), "phfl").contains(&"photo_filter_range"));
    // Vibrance descriptor +30 / -15; a vibrance of 150 is out of range.
    let vib = |v: i32| {
        let mut d = be32(16);
        d.extend(ds_object("null", vec![("vibrance", v_long(v)), ("Strt", v_long(-15))]));
        d
    };
    assert_eq!(
        adjustment(&proposal(&adjustment_doc(b"vibA", &vib(30)))),
        Some(StudioAdjustment::Vibrance { vibrance: 30.0, saturation: -15.0 })
    );
    assert!(codes(&proposal(&adjustment_doc(b"vibA", &vib(150))), "vibA").contains(&"vibrance_range"));
    // A truncated descriptor is a typed loss too.
    assert!(codes(&proposal(&adjustment_doc(b"vibA", &vib(30)[..9])), "vibA").contains(&"vibrance_descriptor_unreadable"));
}
