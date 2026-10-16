//! Lane SMB-PS-ADJUST-STYLES-3: PSD patterns (`Patt` / `Pat2` / `Pat3` global blocks) decoded into
//! the Folio pattern overlay's `pattern_tile` (STU-RAS-157 `pattern_overlay`; STU-IO-006 row 1), so
//! an imported `patternFill` effect carries its pixels. Two tests: Photoshop-saved corpus files
//! (psd-tools, pinned by byte length; NOT_PROVEN when absent), with the imported overlay rendered by
//! pigment's reference (the evaluator render-cpu matches bit for bit), and hand-built patterns,
//! hostile ones and unresolved references. Photoshop display parity is UNVERIFIED (D5: no oracle
//! case covers pattern overlays).
mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    ADJUSTMENT_SCHEMA, Budget, PATTERN_SCHEMA, Resolution, Resolver, StudioEffectKind, TileRef, validate_raster_records,
};
use hsk_studio_interop_psd::*;
use hsk_studio_pigment::effect_math::{
    EffectOp, EffectPlane, LayerParams, PatternOverlayOp, PatternPixels, PatternRef, render_layer_reference_with,
};
use hsk_studio_pigment::render_plan::{SourceKey, StudioBlendMode};

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
    fn effect_stack_id(&mut self, _layer_id: &str) -> Option<String> {
        Some(self.next("SEFX"))
    }
    fn pattern_id(&mut self, unique_id: &str) -> Option<String> {
        self.1.then(|| format!("SPAT-{unique_id}"))
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
fn proposal_with(bytes: &[u8], mint: bool, limits: &Limits) -> ImportProposal {
    let doc = read(bytes);
    let cfg = ProposalConfig::new(limits, budget(), &Host);
    let p = to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0, mint), None, &CancellationToken::default())
        .expect("proposal");
    if !p.raster_records.layers.is_empty() {
        let issues = validate_raster_records(&p.raster_records, p.snapshot.document(), budget(), &CancellationToken::default(), &Host)
            .expect("raster records validate");
        assert!(issues.is_empty());
    }
    p
}
fn proposal(bytes: &[u8]) -> ImportProposal {
    proposal_with(bytes, true, &Limits::default())
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

/// The linear-light value of an 8-bit display-encoded sRGB sample.
fn lin(v: u8) -> f32 {
    srgb_to_linear(sample_to_unit(u32::from(v), 8, SixteenBitScale::default()))
}
fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}
/// Pixel `i` of a pattern blob as straight RGBA.
fn blob_px(b: &PatternTileBlob, i: usize) -> [f32; 4] {
    let (c, a) = (f32s(&b.colour), f32s(&b.alpha));
    [c[i * 3], c[i * 3 + 1], c[i * 3 + 2], a[i]]
}
fn blob<'a>(p: &'a ImportProposal, unique_id: &str) -> &'a PatternTileBlob {
    let id = format!("SPAT-{unique_id}");
    p.pattern_tiles.iter().find(|b| b.pattern_object_id == id).unwrap_or_else(|| panic!("pattern {unique_id}: {:?}", p.loss.entries))
}
/// Every pattern overlay of the proposal's records.
fn overlays(p: &ImportProposal) -> Vec<StudioEffectKind> {
    p.raster_records
        .layers
        .iter()
        .filter_map(|l| l.effects.as_ref())
        .flat_map(|s| s.effects.iter())
        .filter(|e| matches!(e.params, StudioEffectKind::PatternOverlay { .. }))
        .map(|e| e.params.clone())
        .collect()
}
fn all_codes(p: &ImportProposal) -> Vec<&'static str> {
    p.loss.entries.iter().map(|e| e.code).collect()
}

struct Px(Vec<[f32; 4]>);
impl PatternPixels for Px {
    fn pattern(&self, key: SourceKey) -> Option<&[[f32; 4]]> {
        (key == SourceKey(1)).then_some(self.0.as_slice())
    }
}

#[test]
fn photoshop_saved_patterns_decode_and_overlays_import_with_their_tiles() {
    let read = |name: &str, len: usize| match corpus_file(&format!("psd-tools/psd_files/{name}")) {
        Ok(b) if b.len() == len => Some(b),
        Ok(b) => {
            println!("NOT_PROVEN: photoshop_saved_patterns_decode_and_overlays_import_with_their_tiles {name} reason=length {} != {len}", b.len());
            None
        }
        Err(reason) => {
            println!("NOT_PROVEN: photoshop_saved_patterns_decode_and_overlays_import_with_their_tiles {name} reason={reason}");
            None
        }
    };
    // layer_effects.psd (bytes inspected 2026-10-16): Patt holds "Water", 80 x 80 RGB 8-bit, id
    // 2ab8ff7f-..., red raw (first 64), green RLE (first 1), blue raw (first 167); one layer's lfx2
    // has an enabled patternFill naming it (Scl 100 %, Algn on, phase 0, 0).
    if let Some(bytes) = read("layer_effects.psd", 735_149) {
        let p = proposal(&bytes);
        let water = blob(&p, "2ab8ff7f-7c39-11d4-97f0-ce0e81abb313");
        assert_eq!((water.width, water.height), (80, 80));
        assert_eq!(blob_px(water, 0), [lin(64), lin(1), lin(167), 1.0]);
        let found = overlays(&p);
        assert!(!found.is_empty(), "{:?}", p.loss.entries);
        let StudioEffectKind::PatternOverlay { pattern, scale, phase_x, phase_y, link_with_layer, pattern_tile } = &found[0] else {
            unreachable!()
        };
        assert_eq!((pattern.schema_id.as_str(), pattern.object_id.as_str()), (PATTERN_SCHEMA, water.pattern_object_id.as_str()));
        assert_eq!((*scale, *phase_x, *phase_y, *link_with_layer), (100.0, 0.0, 0.0, true));
        let tile = pattern_tile.as_ref().expect("pattern tile");
        assert_eq!((tile.width.value, tile.height.value, tile.format.as_str()), (80.0, 80.0, "rgb_f32le"));
        assert_eq!(tile.content_digest.digest, water.content_sha256_hex);
        assert!(!all_codes(&p).contains(&"effect_fill_reference_unavailable"), "{:?}", p.loss.entries);
        // The imported overlay renders: pigment's reference with the record's parameters over an
        // opaque 5 x 4 layer at (0, 0) shows the pattern pixel at every site (Normal, 100 %, linked,
        // phase 0, scale 1: pattern texel (x mod 80, y mod 80)).
        let pixels: Vec<[f32; 4]> = (0..80 * 80).map(|i| blob_px(water, i)).collect();
        let op = PatternOverlayOp::new(
            StudioBlendMode::Normal,
            1.0,
            PatternRef { key: SourceKey(1), width: 80, height: 80, colour_sha256: [0; 32], alpha_sha256: [0; 32] },
        )
        .with_scale((*scale / 100.0) as f32)
        .with_phase([*phase_x as f32, *phase_y as f32])
        .with_link(*link_with_layer);
        let (w, h) = (5u32, 4u32);
        let back = EffectPlane::new(w, h, vec![[0.0; 3]; 20], vec![1.0; 20]).unwrap();
        let content = EffectPlane::new(w, h, vec![[0.5; 3]; 20], vec![1.0; 20]).unwrap();
        let params = LayerParams { mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 1.0 };
        let out = render_layer_reference_with(&back, &content, params, &[EffectOp::PatternOverlay(op)], &Px(pixels.clone())).unwrap();
        for (x, y) in [(0u32, 0u32), (1, 2), (4, 3)] {
            let got = out.pixel(x, y).unwrap();
            let want = pixels[(y * 80 + x) as usize];
            for c in 0..3 {
                assert!((got.rgb[c] - want[c]).abs() <= 1e-6, "({x}, {y}): {got:?} vs {want:?}");
            }
        }
    }
    // layers/pattern-fill.psd: "Metallic Flecks" 128 x 128, red and blue RLE (first 255, 254),
    // green raw (first 252). No overlay uses it (a pattern fill layer): its tile is still minted.
    if let Some(bytes) = read("layers/pattern-fill.psd", 62_934) {
        let p = proposal(&bytes);
        let flecks = blob(&p, "cf324614-b915-11d7-b003-ad2608ed939e");
        assert_eq!((flecks.width, flecks.height, blob_px(flecks, 0)), (128, 128, [lin(255), lin(252), lin(254), 1.0]));
    }
    // stroke.psd: two patterns, "Yellow Mums" 265 x 219 raw (193, 183, 5) and "Dots 1" 64 x 64 RLE
    // (3, 4, 4).
    if let Some(bytes) = read("stroke.psd", 254_031) {
        let p = proposal(&bytes);
        let mums = blob(&p, "b2fdfd29-de85-11d5-838b-ff55e75fb875");
        assert_eq!((mums.width, mums.height, blob_px(mums, 0)), (265, 219, [lin(193), lin(183), lin(5), 1.0]));
        let dots = blob(&p, "a98b877a-688d-1177-b2e5-b715d4e2a635");
        assert_eq!((dots.width, dots.height, blob_px(dots, 0)), (64, 64, [lin(3), lin(4), lin(4), 1.0]));
    }
    // multichannel-pattern-fill.psd: a multichannel (mode 7) pattern is a typed loss, no tile.
    if let Some(bytes) = read("multichannel-pattern-fill.psd", 23_426) {
        let p = proposal(&bytes);
        assert!(p.pattern_tiles.is_empty());
        assert!(all_codes(&p).contains(&"pattern_mode_unmapped"), "{:?}", p.loss.entries);
    }
}

// ---- hand-built patterns -------------------------------------------------------------------

/// One array of a virtual memory array list (`depth`, compression, samples) over `h` x `w`.
fn array(h: u16, w: u16, depth: u32, compression: u8, samples: &[u8]) -> Vec<u8> {
    let mut body = be32(depth);
    for v in [0u32, 0, u32::from(h), u32::from(w)] {
        body.extend(be32(v));
    }
    body.extend(be16(depth as u16));
    body.push(compression);
    body.extend_from_slice(samples);
    let mut v = be32(1);
    v.extend(be32(body.len() as u32));
    v.extend(body);
    v
}
/// One pattern record: `count` colour-channel slots (+ user mask + transparency) from `arrays`
/// (`None` = not written), length-prefixed and padded to 4.
fn pattern(mode: u32, (w, h): (u16, u16), id: &str, table: Option<&[u8]>, count: u32, arrays: Vec<Option<Vec<u8>>>) -> Vec<u8> {
    let mut p = be32(1);
    p.extend(be32(mode));
    p.extend(be16(h));
    p.extend(be16(w));
    p.extend(unicode("n\0"));
    p.push(id.len() as u8);
    p.extend_from_slice(id.as_bytes());
    if let Some(t) = table {
        p.extend_from_slice(t);
    }
    let mut vma = Vec::new();
    for v in [0u32, 0, u32::from(h), u32::from(w)] {
        vma.extend(be32(v));
    }
    vma.extend(be32(count));
    for a in arrays {
        match a {
            None => vma.extend(be32(0)),
            Some(bytes) => vma.extend(bytes),
        }
    }
    p.extend(be32(3));
    p.extend(be32(vma.len() as u32));
    p.extend(vma);
    let mut out = be32(p.len() as u32);
    out.extend(p);
    pad_to(out, 4)
}
/// A 2 x 2 RGB 8-bit pattern: red raw, green RLE, blue raw, transparency raw.
fn rgb_2x2(id: &str) -> Vec<u8> {
    let red = array(2, 2, 8, 0, &[10, 20, 30, 40]);
    let green = array(2, 2, 8, 1, &rle(&[&[50, 50], &[60, 70]], false));
    let blue = array(2, 2, 8, 0, &[80, 90, 100, 110]);
    let alpha = array(2, 2, 8, 0, &[255, 128, 0, 255]);
    pattern(3, (2, 2), id, None, 3, vec![Some(red), Some(green), Some(blue), None, Some(alpha)])
}

// Photoshop descriptor builders (independent of the crate's reader; as in adjustment_records.rs).
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
/// An `lfx2` block holding one `patternFill` that names `id` (`Scl` percent, phase, `Algn`).
fn pattern_fill(id: Option<&str>, scale: f64, phase: (f64, f64), linked: bool) -> Vec<u8> {
    let mut items = vec![
        ("enab", v_bool(true)),
        ("Md  ", v_enum("BlnM", "Nrml")),
        ("Opct", v_untf(b"#Prc", 80.0)),
    ];
    if let Some(id) = id {
        items.push(("Ptrn", v_objc("Ptrn", vec![("Nm  ", v_text("p")), ("Idnt", v_text(id))])));
    }
    items.push(("Scl ", v_untf(b"#Prc", scale)));
    items.push(("Algn", v_bool(linked)));
    items.push(("phase", v_objc("Pnt ", vec![("Hrzn", v_doub(phase.0)), ("Vrtc", v_doub(phase.1))])));
    let mut v = be32(0);
    v.extend(be32(16));
    v.extend(ds_object("null", vec![("Scl ", v_untf(b"#Prc", 100.0)), ("masterFXSwitch", v_bool(true)), ("patternFill", v_objc("patternFill", items))]));
    v
}
/// A 2 x 2 RGB document: one raster layer with `lfx2`, the global blocks `globals`.
fn doc(lfx2: &[u8], globals: &[Vec<u8>]) -> Vec<u8> {
    let mut l = TLayer::new("px", [0, 0, 2, 2]);
    l.channels = vec![(-1, 0, vec![255; 4]), (0, 0, vec![10; 4]), (1, 0, vec![20; 4]), (2, 0, vec![30; 4])];
    l.blocks = vec![layer_block(b"lfx2", lfx2, false)];
    let info = layer_info(&[l], false, false);
    Doc {
        version: 1,
        channels: 3,
        height: 2,
        width: 2,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: layer_section(&info, globals, false),
        image: Some((0, vec![1; 12])),
    }
    .bytes()
}

#[test]
fn hand_built_patterns_map_and_hostile_or_unresolved_ones_are_typed() {
    // RGB with transparency, gray and indexed patterns; the overlay names the RGB one.
    let mut table = vec![0u8; 768];
    (table[7], table[256 + 7], table[512 + 7]) = (200, 100, 50);
    let gray = pattern(1, (1, 1), "g-1", None, 1, vec![Some(array(1, 1, 8, 0, &[128])), None, None]);
    let indexed = pattern(2, (1, 1), "i-1", Some(&table), 1, vec![Some(array(1, 1, 8, 0, &[7])), None, None]);
    let sixteen = pattern(3, (1, 1), "s-1", None, 3, vec![
        Some(array(1, 1, 16, 0, &[0xff, 0xff])),
        Some(array(1, 1, 16, 0, &[0x80, 0x00])),
        Some(array(1, 1, 16, 0, &[0, 0])),
        None,
        None,
    ]);
    let mut patt = rgb_2x2("p-1");
    patt.extend(gray);
    patt.extend(indexed);
    patt.extend(sixteen);
    let bytes = doc(&pattern_fill(Some("p-1"), 150.0, (3.0, -2.0), false), &[global_block(b"Patt", &patt, false)]);
    let p = proposal(&bytes);
    let rgb = blob(&p, "p-1");
    assert_eq!(blob_px(rgb, 0), [lin(10), lin(50), lin(80), 1.0]);
    assert_eq!(blob_px(rgb, 1), [lin(20), lin(50), lin(90), sample_to_unit(128, 8, SixteenBitScale::default())]);
    assert_eq!(blob_px(rgb, 3), [lin(40), lin(70), lin(110), 1.0]);
    assert_eq!(blob_px(blob(&p, "g-1"), 0), [lin(128), lin(128), lin(128), 1.0]);
    assert_eq!(blob_px(blob(&p, "i-1"), 0), [lin(200), lin(100), lin(50), 1.0]);
    let s = blob_px(blob(&p, "s-1"), 0);
    assert_eq!((s[0], s[2], s[3]), (1.0, 0.0, 1.0));
    assert_eq!(s[1], srgb_to_linear(sample_to_unit(32768, 16, SixteenBitScale::default())), "{s:?}");
    let found = overlays(&p);
    match &found[..] {
        [StudioEffectKind::PatternOverlay { scale, phase_x, phase_y, link_with_layer, pattern_tile: Some(t), .. }] => {
            assert_eq!((*scale, *phase_x, *phase_y, *link_with_layer), (150.0, 3.0, -2.0, false));
            assert_eq!((t.width.value, t.height.value, t.content_digest.digest.as_str()), (2.0, 2.0, rgb.content_sha256_hex.as_str()));
        }
        other => panic!("{other:?} {:?}", p.loss.entries),
    }
    assert!(all_codes(&p).contains(&"pattern_colour_assumed_srgb"));

    // Typed losses on the effect: no Ptrn, an unknown id, a host minting no pattern ids, a
    // non-positive scale.
    let patt_block = || vec![global_block(b"Patt", &rgb_2x2("p-1"), false)];
    let effect_code = |lfx2: Vec<u8>, globals: Vec<Vec<u8>>, mint: bool, code: &str| {
        let p = proposal_with(&doc(&lfx2, &globals), mint, &Limits::default());
        assert!(overlays(&p).is_empty(), "{code}");
        assert!(all_codes(&p).contains(&code), "{code}: {:?}", p.loss.entries);
    };
    effect_code(pattern_fill(None, 100.0, (0.0, 0.0), true), patt_block(), true, "pattern_overlay_reference_missing");
    effect_code(pattern_fill(Some("nope"), 100.0, (0.0, 0.0), true), patt_block(), true, "pattern_overlay_pattern_missing");
    effect_code(pattern_fill(Some("p-1"), 100.0, (0.0, 0.0), true), patt_block(), false, "pattern_overlay_unminted");
    effect_code(pattern_fill(Some("p-1"), -5.0, (0.0, 0.0), true), patt_block(), true, "effect_value_invalid");

    // Hostile patterns: each a typed loss on the pattern, never a failed import; the overlay that
    // names it is lost with pattern_overlay_pattern_missing.
    let hostile = |patt: Vec<u8>, limits: &Limits, code: &str| {
        let p = proposal_with(&doc(&pattern_fill(Some("h"), 100.0, (0.0, 0.0), true), &[global_block(b"Patt", &patt, false)]), true, limits);
        assert!(p.pattern_tiles.is_empty(), "{code}");
        let codes = all_codes(&p);
        assert!(codes.contains(&code) && codes.contains(&"pattern_overlay_pattern_missing"), "{code}: {codes:?}");
    };
    let d = Limits::default();
    // 5000 x 10: over the 4096 side bound, refused before any sample is read
    hostile(pattern(3, (5000, 10), "h", None, 3, vec![None, None, None, None, None]), &d, "pattern_over_limit");
    // a pattern length far beyond the block
    let mut long = be32(u32::MAX);
    long.extend([0; 12]);
    hostile(long, &d, "pattern_truncated");
    // a channel count past the bound
    hostile(pattern(3, (1, 1), "h", None, 1_000_000, vec![]), &d, "pattern_channels_over_limit");
    // RLE row counts claiming more than the array holds
    let short_rle = array(2, 2, 8, 1, &[0, 200, 0, 200, 1]);
    hostile(pattern(3, (2, 2), "h", None, 3, vec![Some(short_rle.clone()), Some(short_rle.clone()), Some(short_rle), None, None]), &d, "pattern_channel_undecodable");
    // 32-bit samples are not mapped
    let deep = array(1, 1, 32, 0, &[0; 4]);
    hostile(pattern(3, (1, 1), "h", None, 3, vec![Some(deep.clone()), Some(deep.clone()), Some(deep), None, None]), &d, "pattern_depth_unsupported");
    // the decoded bytes of all patterns stay under max_decoded_bytes: one layer tile (1 MiB) fits,
    // a 260 x 260 pattern (16 bytes per pixel, 1081600 bytes) is refused before its arrays are read
    let tight = Limits { max_decoded_bytes: 1 << 20, ..Limits::default() };
    hostile(pattern(3, (260, 260), "h", None, 3, vec![None, None, None, None, None]), &tight, "patterns_over_budget");
    // a channel array whose rectangle is not the pattern's
    let mut skewed = array(2, 2, 8, 0, &[1; 4]);
    skewed[15] = 1; // top = 1
    hostile(pattern(3, (2, 2), "h", None, 3, vec![Some(skewed.clone()), Some(skewed.clone()), Some(skewed), None, None]), &d, "pattern_channel_rect");
}
