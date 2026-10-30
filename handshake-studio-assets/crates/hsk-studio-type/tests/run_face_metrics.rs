//! Lane SMB-ID-FRAME-AUTOSIZE-2: `Run::cap_height_pt` / `Run::x_height_pt`, the resolved face's OS/2
//! `sCapHeight` / `sxHeight` scaled by font size / unitsPerEm. Expectations come from a test-side
//! reader of the fixture bytes (big-endian values at the table offsets of the sfnt directory), never
//! from Type's own parser. Fixtures come only from tests/fixtures and the research folder (hash-pinned
//! fonts, V4-03).
#![allow(dead_code)]
include!("support/mod.rs");

use serde_json::json;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn case(text: &str, font: usize, script: &str, language: &str) -> Value {
    json!({
        "case": "run_face_metrics",
        "text": text,
        "text_sha256": hex(&hash(text.as_bytes())),
        "text_utf8_hex": hex(text.as_bytes()),
        "font_index": font,
        "script": script,
        "language": language,
    })
}
fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
/// Offset of the table record of `tag` in the sfnt directory.
fn record(font: &[u8], tag: &[u8; 4]) -> usize {
    (0..usize::from(be16(font, 4)))
        .map(|i| 12 + 16 * i)
        .find(|&r| &font[r..r + 4] == tag)
        .unwrap_or_else(|| panic!("table {tag:?}"))
}
/// `(unitsPerEm, sCapHeight, sxHeight)` read independently: `head` offset 18, OS/2 offsets 88 and 86.
/// Cap and x height exist only from OS/2 version 2 (table length 96).
fn reference(font: &[u8]) -> (u16, Option<i16>, Option<i16>) {
    let head = be32(font, record(font, b"head") + 8) as usize;
    let r = record(font, b"OS/2");
    let (o, len) = (be32(font, r + 8) as usize, be32(font, r + 12) as usize);
    let upem = be16(font, head + 18);
    if be16(font, o) >= 2 && len >= 96 {
        (upem, Some(be16(font, o + 88) as i16), Some(be16(font, o + 86) as i16))
    } else {
        (upem, None, None)
    }
}
/// `(font size, unitsPerEm, cap height, x height)` of every run Type produced for `c`.
fn run_metrics<const N: usize>(fonts: &Fonts<N>, c: &Value) -> Vec<(f64, u32, Option<f64>, Option<f64>)> {
    let mut out = Vec::new();
    with_case(fonts, c, None, |request, ports, _, _, _| {
        let result = prepared(request, ports);
        for p in result.paragraphs() {
            for r in p.runs() {
                out.push((r.font_size_pt(), r.units_per_em(), r.cap_height_pt(), r.x_height_pt()));
            }
        }
    });
    out
}
fn scaled(v: Option<i16>, size: f64, upem: u16) -> Option<f64> {
    v.map(|v| f64::from(v) * size / f64::from(upem))
}

/// Inter Regular (OS/2 v4: unitsPerEm 2816, sCapHeight 2048, sxHeight 1536) at the harness 12 pt:
/// cap 2048 x 12 / 2816 = 96/11 pt, x 1536 x 12 / 2816 = 72/11 pt. Noto Sans SC (CFF) and the
/// variable Inter at its default instance take their own OS/2 values the same way.
#[test]
fn run_metrics_are_the_face_os2_values_scaled() {
    bounded_case("run_metrics_are_the_face_os2_values_scaled", || {
        let fonts = Fonts::load();
        assert_eq!(reference(&fonts.data[0]), (2816, Some(2048), Some(1536)));
        let inter = run_metrics(&fonts, &case("Hi", 0, "Latn", "en"));
        assert_eq!(inter.len(), 1);
        let (size, upem, cap, x) = inter[0];
        assert_eq!((size, upem), (12.0, 2816));
        assert!((cap.expect("cap") - 96.0 / 11.0).abs() < 1e-12, "{cap:?}");
        assert!((x.expect("x") - 72.0 / 11.0).abs() < 1e-12, "{x:?}");
        for (font, text, script, language) in [(0, "Hi", "Latn", "en"), (2, "中", "Hani", "zh-CN"), (3, "Hi", "Latn", "en")] {
            let (u, c, xh) = reference(&fonts.data[font]);
            assert!(c.is_some() && xh.is_some(), "fixture {font} has OS/2 v2+");
            let runs = run_metrics(&fonts, &case(text, font, script, language));
            assert!(!runs.is_empty());
            for (size, upem, cap, x) in runs {
                assert_eq!(upem, u32::from(u), "font {font}");
                assert_eq!((cap, x), (scaled(c, size, u), scaled(xh, size, u)), "font {font}");
            }
        }
    });
}

/// A face whose OS/2 table is version 1 has no cap or x height: `None`, not an estimate. The same
/// for a hostile version-4 OS/2 whose directory length (86) is shorter than the version needs (96):
/// the table is not read past its stated end. Shaping itself is unaffected.
#[test]
fn run_metrics_are_none_without_os2_v2_fields() {
    bounded_case("run_metrics_are_none_without_os2_v2_fields", || {
        let base = Fonts::load();
        let r = record(&base.data[0], b"OS/2");
        let o = be32(&base.data[0], r + 8) as usize;
        let mut v1 = base.data[0].clone();
        v1[o..o + 2].copy_from_slice(&1u16.to_be_bytes());
        let mut short = base.data[0].clone();
        short[r + 12..r + 16].copy_from_slice(&86u32.to_be_bytes());
        assert_eq!(reference(&v1), (2816, None, None));
        assert_eq!(reference(&short), (2816, None, None));
        for bytes in [v1, short] {
            let Fonts { data, hashes } = Fonts::load();
            let [_, d1, d2, d3] = data;
            let fonts = Fonts {
                hashes: [hash(&bytes), hashes[1], hashes[2], hashes[3]],
                data: [bytes, d1, d2, d3],
            };
            let runs = run_metrics(&fonts, &case("Hi", 0, "Latn", "en"));
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0], (12.0, 2816, None, None));
        }
    });
}
