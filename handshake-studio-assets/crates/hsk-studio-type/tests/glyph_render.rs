//! SMB-TYPE-GLYPH-RENDER slice 1: outline extraction and the deterministic fixed-point rasteriser.
//! Fixtures come only from tests/fixtures (hash-pinned fonts, V4-03).
#![allow(dead_code)]
include!("support/mod.rs");

use serde_json::json;

/// "Hi" shaped with Inter-Regular at 12 pt (the shared harness size), rasterised at 32 px/em
/// (= 24 pt at 96 ppi), origin (10.25, 40.0). Determinism anchor for STU-TYP-131 on this host.
const HI_DIGEST: &str = "2b66ea525d0abc933ff810dc5d5be4016d0837a745c633ffae52271d74bc47ac";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn case(text: &str, font: usize, script: &str, language: &str) -> Value {
    json!({
        "case": "glyph_render",
        "text": text,
        "text_sha256": hex(&hash(text.as_bytes())),
        "text_utf8_hex": hex(text.as_bytes()),
        "font_index": font,
        "script": script,
        "language": language,
    })
}
fn resource(fonts: &Fonts, i: usize) -> FontResource<'static> {
    FontResource {
        identity: NAMES[i],
        location: NAMES[i],
        content_hash: fonts.hashes[i],
        face_index: 0,
        resource_revision: 1,
        grant_revision: 1,
    }
}
fn shaped_ids(fonts: &Fonts, c: &Value) -> Vec<u32> {
    let mut ids = Vec::new();
    with_case(fonts, c, None, |request, ports, _, _, _| {
        let result = prepared(request, ports);
        for p in result.paragraphs() {
            for r in p.runs() {
                ids.extend(r.glyphs().iter().map(|g| g.glyph_id));
            }
        }
    });
    ids
}
fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn table<'a>(font: &'a [u8], tag: &[u8; 4]) -> &'a [u8] {
    for i in 0..be16(font, 4) as usize {
        let r = 12 + 16 * i;
        if &font[r..r + 4] == tag {
            let (off, len) = (be32(font, r + 8) as usize, be32(font, r + 12) as usize);
            return &font[off..off + len];
        }
    }
    panic!("table {tag:?}")
}
/// Header bbox of a glyf glyph, read independently of the vendored parser.
fn glyf_bbox(font: &[u8], glyph: u32) -> [i16; 4] {
    let long = be16(table(font, b"head"), 50) == 1;
    let loca = table(font, b"loca");
    let g = glyph as usize;
    let off = if long {
        be32(loca, 4 * g) as usize
    } else {
        2 * be16(loca, 2 * g) as usize
    };
    let glyf = &table(font, b"glyf")[off..];
    [2, 4, 6, 8].map(|at| be16(glyf, at) as i16)
}
fn num_glyphs(font: &[u8]) -> u32 {
    u32::from(be16(table(font, b"maxp"), 4))
}
fn ink(c: &GlyphCoverage) -> u64 {
    c.data().iter().map(|v| u64::from(*v)).sum()
}
fn synthetic(points: &[[f32; 2]]) -> GlyphOutline {
    let mut commands = vec![OutlineCommand::MoveTo(points[0])];
    commands.extend(points[1..].iter().map(|p| OutlineCommand::LineTo(*p)));
    commands.push(OutlineCommand::Close);
    GlyphOutline::synthetic(1000, commands)
}
fn px_per_pt() -> f64 {
    96.0 / 72.0 * 2.0
}

#[test]
fn outlines_extract_typed() {
    bounded_case("outlines_extract_typed", || {
        let fonts = Fonts::load();
        let cancel = CancellationToken::default();
        let limits = OutlineLimits::default();
        let inter = FontBytes {
            bytes: &fonts.data[0],
            resource: resource(&fonts, 0),
        };
        let ids = shaped_ids(&fonts, &case("l ", 0, "Latn", "en"));
        assert_eq!(ids.len(), 2);
        let out = glyph_outlines(&inter, &ids, &limits, &cancel).unwrap();
        let l = &out[0];
        assert_eq!(l.glyph_id(), ids[0]);
        assert_eq!(l.units_per_em(), 2816);
        assert_eq!(l.font_hash(), fonts.hashes[0]);
        assert!(matches!(l.commands().first(), Some(OutlineCommand::MoveTo(_))));
        assert_eq!(l.commands().last(), Some(&OutlineCommand::Close));
        assert_eq!(l.bounds(), Some(glyf_bbox(&fonts.data[0], ids[0])));
        assert!(out[1].commands().is_empty(), "space has no outline");
        assert_eq!(out[1].bounds(), None);
        let n = num_glyphs(&fonts.data[0]);
        assert_eq!(
            glyph_outlines(&inter, &[n], &limits, &cancel),
            Err(GlyphError::GlyphOutlineUnavailable)
        );
        assert_eq!(
            glyph_outlines(&inter, &[70_000], &limits, &cancel),
            Err(GlyphError::GlyphOutlineUnavailable)
        );
        assert_eq!(
            glyph_outlines(&inter, &[n - 1], &limits, &cancel).map(|v| v.len()),
            Ok(1)
        );
        // CFF (NotoSansSC): cubic outlines only.
        let han = shaped_ids(&fonts, &case("心", 2, "Hani", "zh-CN"));
        let sc = FontBytes {
            bytes: &fonts.data[2],
            resource: resource(&fonts, 2),
        };
        let out = glyph_outlines(&sc, &han, &limits, &cancel).unwrap();
        let commands = out[0].commands();
        assert!(commands.iter().any(|c| matches!(c, OutlineCommand::CurveTo(..))));
        assert!(!commands.iter().any(|c| matches!(c, OutlineCommand::QuadTo(..))));
        // Same admission rules as shaping: hash, CFF2.
        let mut wrong = resource(&fonts, 0);
        wrong.content_hash = [0; 32];
        let bad = FontBytes {
            bytes: &fonts.data[0],
            resource: wrong,
        };
        assert_eq!(
            glyph_outlines(&bad, &ids, &limits, &cancel),
            Err(GlyphError::Type(Error::HashMismatch))
        );
        let mut cff2 = fonts.data[0].clone();
        assert_eq!(&cff2[12..16], b"GDEF");
        cff2[12..16].copy_from_slice(b"CFF2"); // still sorted: "CFF2" < "GPOS"
        let mut flagged = resource(&fonts, 0);
        flagged.content_hash = hash(&cff2);
        let cff2_font = FontBytes {
            bytes: &cff2,
            resource: flagged,
        };
        assert_eq!(
            glyph_outlines(&cff2_font, &ids, &limits, &cancel),
            Err(GlyphError::Type(Error::UnsupportedProfile))
        );
    });
}

#[test]
fn raster_hand_values() {
    bounded_case("raster_hand_values", || {
        let cancel = CancellationToken::default();
        let limits = RasterLimits::default();
        let square = synthetic(&[[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]);
        let c = rasterize_glyph(&square, [0.0, 10.0], 1.0, &limits, &cancel).unwrap();
        assert_eq!((c.x(), c.y(), c.width(), c.height()), (0, 0, 10, 10));
        assert!(c.data().iter().all(|v| *v == 255));
        // Opposite winding (clockwise in y-up) fills identically under non-zero.
        let reversed = synthetic(&[[0.0, 0.0], [0.0, 10.0], [10.0, 10.0], [10.0, 0.0]]);
        let r = rasterize_glyph(&reversed, [0.0, 10.0], 1.0, &limits, &cancel).unwrap();
        assert_eq!(r, c);
        // Half-pixel x offset: edge columns 128 exactly (round half up of 127.5).
        let c = rasterize_glyph(&square, [0.5, 10.0], 1.0, &limits, &cancel).unwrap();
        assert_eq!((c.x(), c.y(), c.width(), c.height()), (0, 0, 11, 10));
        for row in c.data().chunks(11) {
            assert_eq!(row[0], 128);
            assert!(row[1..10].iter().all(|v| *v == 255));
            assert_eq!(row[10], 128);
        }
        // Triangle area 150 px^2 at a subpixel origin.
        let tri = synthetic(&[[0.0, 0.0], [20.0, 0.0], [0.0, 15.0]]);
        let c = rasterize_glyph(&tri, [0.25, 20.5], 1.0, &limits, &cancel).unwrap();
        let area = ink(&c) as f64 / 255.0;
        assert!((area - 150.0).abs() <= 0.75, "triangle area {area}");
        // Parabolic segment (quad and its exact cubic elevation): area 2/3 * 20 * 10.
        let quad = GlyphOutline::synthetic(
            1000,
            vec![
                OutlineCommand::MoveTo([0.0, 0.0]),
                OutlineCommand::QuadTo([10.0, 20.0], [20.0, 0.0]),
                OutlineCommand::Close,
            ],
        );
        let cubic = GlyphOutline::synthetic(
            1000,
            vec![
                OutlineCommand::MoveTo([0.0, 0.0]),
                OutlineCommand::CurveTo([20.0 / 3.0, 40.0 / 3.0], [40.0 / 3.0, 40.0 / 3.0], [20.0, 0.0]),
                OutlineCommand::Close,
            ],
        );
        for shape in [&quad, &cubic] {
            let c = rasterize_glyph(shape, [0.125, 12.0], 1.0, &limits, &cancel).unwrap();
            let area = ink(&c) as f64 / 255.0;
            let exact = 2.0 / 3.0 * 20.0 * 10.0;
            // Chords lie inside a convex curve, so flattening (1/16 px) can only lose area:
            // here at most 1 % on a 20 px shape, never more than the exact area.
            assert!(area <= exact && exact - area <= exact * 0.01, "curve area {area}");
        }
        assert_eq!(c.data().len(), (c.width() * c.height()) as usize);
    });
}

#[test]
fn run_raster_digest_and_mixed_scripts() {
    bounded_case("run_raster_digest_and_mixed_scripts", || {
        let fonts = Fonts::load();
        let cancel = CancellationToken::default();
        let ol = OutlineLimits::default();
        let rl = RasterLimits::default();
        with_case(&fonts, &case("Hi", 0, "Latn", "en"), None, |request, ports, _, _, _| {
            let result = prepared(request, ports);
            let run = &result.paragraphs()[0].runs()[0];
            let ids: Vec<u32> = run.glyphs().iter().map(|g| g.glyph_id).collect();
            let font = FontBytes {
                bytes: &fonts.data[0],
                resource: run.resolved(),
            };
            let outlines = glyph_outlines(&font, &ids, &ol, &cancel).unwrap();
            let origin = [10.25, 40.0];
            let c = rasterize_run(run, &outlines, origin, px_per_pt(), &rl, &cancel).unwrap();
            assert!(ink(&c) > 0);
            assert_eq!(c.stride(), c.width());
            assert_eq!(c.data().len(), (c.width() * c.height()) as usize);
            // Ink extent from the pen model: H left bound .. pen(H advance) + i right bound.
            let scale = run.font_size_pt() * px_per_pt() / f64::from(run.units_per_em());
            let h = outlines[0].bounds().unwrap();
            let i = outlines[1].bounds().unwrap();
            let left = origin[0] + f64::from(h[0]) * scale;
            let right = origin[0] + run.glyphs()[0].x_advance_pt * px_per_pt() + f64::from(i[2]) * scale;
            assert!((c.x() as f64 - left.floor()).abs() <= 1.0, "left {} vs {left}", c.x());
            let plane_right = (c.x() + i64::from(c.width())) as f64;
            assert!((plane_right - right.ceil()).abs() <= 1.0, "right {plane_right} vs {right}");
            let advances: f64 = run.glyphs().iter().map(|g| g.x_advance_pt * px_per_pt()).sum();
            assert!(f64::from(c.width()) <= advances + 1.0);
            let digest = hex(&hash(c.data()));
            println!(
                "hi_plane x={} y={} w={} h={} sha256={digest} raster_version={RASTER_VERSION}",
                c.x(),
                c.y(),
                c.width(),
                c.height()
            );
            assert_eq!(digest, HI_DIGEST, "determinism anchor");
            // Same bytes on 4 concurrent threads (no thread-count dependence).
            std::thread::scope(|s| {
                let handles: Vec<_> = (0..4)
                    .map(|_| {
                        s.spawn(|| {
                            rasterize_run(run, &outlines, origin, px_per_pt(), &rl, &cancel).unwrap()
                        })
                    })
                    .collect();
                for h in handles {
                    assert_eq!(h.join().unwrap(), c);
                }
            });
            println!("NOT_PROVEN: cross-host raster identity (STU-TYP-131) needs a second host running this digest");
        });
        // Latin + Arabic + Han in one shaped paragraph: every run renders without panic.
        let recipe = json(&research().join("type-exact-oracle-recipe.json"));
        let mixed = &recipe["cases"][6];
        assert_eq!(mixed["case"], "mixed_ltr_isolate_and_trailing_space");
        with_case(&fonts, mixed, None, |request, ports, _, _, _| {
            let result = prepared(request, ports);
            let mut inked = Vec::new();
            for run in result.paragraphs()[0].runs() {
                let i = NAMES.iter().position(|n| *n == run.resolved().identity).unwrap();
                let ids: Vec<u32> = run.glyphs().iter().map(|g| g.glyph_id).collect();
                let font = FontBytes {
                    bytes: &fonts.data[i],
                    resource: run.resolved(),
                };
                let outlines = glyph_outlines(&font, &ids, &ol, &cancel).unwrap();
                let c = rasterize_run(run, &outlines, [0.0, 50.0], px_per_pt(), &rl, &cancel).unwrap();
                assert_eq!(c.data().len(), (c.width() * c.height()) as usize);
                if ink(&c) > 0 {
                    inked.push(run.script());
                }
            }
            for script in [*b"Latn", *b"Arab", *b"Hani"] {
                assert!(inked.contains(&script), "{} inked", String::from_utf8_lossy(&script));
            }
        });
    });
}

#[test]
fn hostile_inputs_refused_typed() {
    bounded_case("hostile_inputs_refused_typed", || {
        let fonts = Fonts::load();
        let cancel = CancellationToken::default();
        let ol = OutlineLimits::default();
        let rl = RasterLimits::default();
        let inter = FontBytes {
            bytes: &fonts.data[0],
            resource: resource(&fonts, 0),
        };
        // aggregate_over_budget: 10^6 glyph ids refused before hashing or parsing.
        let many = vec![1u32; 1_000_000];
        assert_eq!(glyph_outlines(&inter, &many, &ol, &cancel), Err(GlyphError::OutlineOverLimit));
        let ids = shaped_ids(&fonts, &case("H", 0, "Latn", "en"));
        let tight = OutlineLimits { max_segments: 3, ..ol };
        assert_eq!(glyph_outlines(&inter, &ids, &tight, &cancel), Err(GlyphError::OutlineOverLimit));
        let tight = OutlineLimits { max_total_segments: 3, ..ol };
        assert_eq!(glyph_outlines(&inter, &ids, &tight, &cancel), Err(GlyphError::OutlineOverLimit));
        // Truncated font bytes: typed CorruptFont, never a panic.
        let cut = &fonts.data[0][..100];
        let mut r = resource(&fonts, 0);
        r.content_hash = hash(cut);
        let truncated = FontBytes { bytes: cut, resource: r };
        assert_eq!(glyph_outlines(&truncated, &ids, &ol, &cancel), Err(GlyphError::Type(Error::CorruptFont)));
        let half = &fonts.data[0][..fonts.data[0].len() / 2];
        let mut r = resource(&fonts, 0);
        r.content_hash = hash(half);
        let _ = glyph_outlines(&FontBytes { bytes: half, resource: r }, &ids, &ol, &cancel);
        // dims_u32_max and aggregate raster budget: refused before the plane is allocated.
        let square = synthetic(&[[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]);
        assert_eq!(rasterize_glyph(&square, [0.0, 0.0], 1e12, &rl, &cancel), Err(GlyphError::RasterOverLimit));
        let small = RasterLimits { max_pixels: 1 << 20, ..rl };
        assert_eq!(rasterize_glyph(&square, [0.0, 0.0], 600.0, &small, &cancel), Err(GlyphError::RasterOverLimit));
        let narrow = RasterLimits { max_width: 9, ..rl };
        assert_eq!(rasterize_glyph(&square, [0.0, 10.0], 1.0, &narrow, &cancel), Err(GlyphError::RasterOverLimit));
        // Horizontal edges carry no coverage and are not stored: the square keeps 2 segments.
        let few = RasterLimits { max_segments: 1, ..rl };
        assert_eq!(rasterize_glyph(&square, [0.0, 10.0], 1.0, &few, &cancel), Err(GlyphError::RasterOverLimit));
        let nan = synthetic(&[[0.0, 0.0], [f32::NAN, 0.0], [0.0, 10.0]]);
        assert_eq!(rasterize_glyph(&nan, [0.0, 10.0], 1.0, &rl, &cancel), Err(GlyphError::Type(Error::InvalidInput)));
        assert_eq!(rasterize_glyph(&square, [f64::NAN, 0.0], 1.0, &rl, &cancel), Err(GlyphError::Type(Error::InvalidInput)));
        assert_eq!(rasterize_glyph(&square, [0.0, 0.0], f64::INFINITY, &rl, &cancel), Err(GlyphError::Type(Error::InvalidInput)));
        let sc_ids = shaped_ids(&fonts, &case("中", 2, "Hani", "zh-CN"));
        let sc = glyph_outlines(
            &FontBytes { bytes: &fonts.data[2], resource: resource(&fonts, 2) },
            &sc_ids,
            &ol,
            &cancel,
        )
        .unwrap();
        with_case(&fonts, &case("H", 0, "Latn", "en"), None, |request, ports, _, _, _| {
            let result = prepared(request, ports);
            let run = &result.paragraphs()[0].runs()[0];
            let outlines = glyph_outlines(&inter, &ids, &ol, &cancel).unwrap();
            assert_eq!(rasterize_run(run, &outlines, [0.0, 0.0], 1e12, &rl, &cancel), Err(GlyphError::RasterOverLimit));
            assert_eq!(rasterize_run(run, &outlines, [f64::NAN, 0.0], 1.0, &rl, &cancel), Err(GlyphError::Type(Error::InvalidInput)));
            assert_eq!(rasterize_run(run, &outlines, [0.0, 0.0], f64::NAN, &rl, &cancel), Err(GlyphError::Type(Error::InvalidInput)));
            assert_eq!(rasterize_run(run, &[], [0.0, 0.0], 1.0, &rl, &cancel), Err(GlyphError::GlyphOutlineUnavailable));
            assert_eq!(rasterize_run(run, &sc, [0.0, 0.0], 1.0, &rl, &cancel), Err(GlyphError::Type(Error::HashMismatch)));
            let stopped = CancellationToken::default();
            stopped.cancel();
            assert_eq!(rasterize_run(run, &outlines, [0.0, 0.0], 1.0, &rl, &stopped), Err(GlyphError::Type(Error::Canceled)));
            assert_eq!(glyph_outlines(&inter, &ids, &ol, &stopped), Err(GlyphError::Type(Error::Canceled)));
        });
        // A run with explicit (non-default) axes: slice 1 outlines are default-instance only.
        let recipe = json(&research().join("type-exact-oracle-recipe.json"));
        let changed = &recipe["cases"][5];
        assert_eq!(changed["case"], "variable_changed");
        with_case(&fonts, changed, None, |request, ports, _, _, _| {
            let result = prepared(request, ports);
            let run = &result.paragraphs()[0].runs()[0];
            assert_eq!(
                rasterize_run(run, &[], [0.0, 0.0], 1.0, &rl, &cancel),
                Err(GlyphError::VariableOutlineUnsupported)
            );
        });
    });
}
