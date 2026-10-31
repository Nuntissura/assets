//! R44 finding F-R44-1 (lane SMB-R44-FOLLOWUPS-1, feature `viewer`): the Gradient tool on a layer
//! holding scene-linear values the display encoding cannot carry (1.5 above white, 0.002 inside
//! the sRGB linear segment) rewrites only the pixels it paints. Oracles: the layer's tile bytes
//! before the fill (unpainted pixels must stay bit-identical); pigment's own `gradient_fill` on the
//! Prism-encoded layer decoded through Prism's `DisplayEncoding::Srgb.decode` (painted pixels);
//! the undo history (undo restores the tiles byte-identical).
#![cfg(feature = "viewer")]

mod support_context;

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;

use common::Scratch;
use hsk_studio_folio::{Payload, TileRef};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::fill_tools::FillSettings;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::fill_tools::{self as pf, ColourStop, DragLine, FillPaint, GradientForm, GradientGeometry, RasterGradient, SMOOTHNESS_FULL, TransparencyStop};
use hsk_studio_pigment::select_tools::ToolLimits;
use hsk_studio_pigment::selection::ColourView;
use hsk_studio_prism::DisplayEncoding;

const W: u32 = 64;
/// Linear-light layer values: above white and inside the sRGB moncurve linear segment.
const HDR: [f32; 3] = [1.5, 0.002, 0.5];

/// RGB 32-bit float PSD (raw channels, big-endian floats taken as linear) with one layer `hdr`
/// covering the canvas: colour `HDR`, alpha 1.
fn hdr_psd() -> Vec<u8> {
    let n = (W * W) as usize;
    let plane = |v: f32| -> Vec<u8> { std::iter::repeat_n(v.to_be_bytes(), n).flatten().collect() };
    let channels: Vec<(i16, Vec<u8>)> = vec![(-1, plane(1.0)), (0, plane(HDR[0])), (1, plane(HDR[1])), (2, plane(HDR[2]))];
    let mut v = b"8BPS".to_vec();
    v.extend(1u16.to_be_bytes());
    v.extend([0u8; 6]);
    v.extend(3u16.to_be_bytes());
    v.extend(W.to_be_bytes());
    v.extend(W.to_be_bytes());
    v.extend(32u16.to_be_bytes());
    v.extend(3u16.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    let mut info = 1i16.to_be_bytes().to_vec();
    for r in [0i32, 0, W as i32, W as i32] {
        info.extend(r.to_be_bytes());
    }
    info.extend((channels.len() as u16).to_be_bytes());
    for (id, data) in &channels {
        info.extend(id.to_be_bytes());
        info.extend(((2 + data.len()) as u32).to_be_bytes());
    }
    info.extend_from_slice(b"8BIMnorm");
    info.extend([255u8, 0, 0x08, 0]);
    let mut extra = 0u32.to_be_bytes().to_vec();
    extra.extend(0u32.to_be_bytes());
    let mut pascal = vec![3u8];
    pascal.extend_from_slice(b"hdr");
    extra.extend(pascal);
    info.extend((extra.len() as u32).to_be_bytes());
    info.extend(extra);
    for (_, data) in &channels {
        info.extend(0u16.to_be_bytes());
        info.extend_from_slice(data);
    }
    if info.len() % 2 == 1 {
        info.push(0);
    }
    let mut section = (info.len() as u32).to_be_bytes().to_vec();
    section.extend(info);
    section.extend(0u32.to_be_bytes());
    v.extend((section.len() as u32).to_be_bytes());
    v.extend(section);
    v.extend(0u16.to_be_bytes());
    for c in HDR {
        v.extend(plane(c));
    }
    v
}

fn tiles(v: &Viewer, layer: &str) -> Vec<TileRef> {
    let snap = v.run().snapshot().unwrap();
    let l = snap.document().layers.iter().find(|l| l.layer_id == layer).unwrap();
    match &l.payload {
        Payload::Raster { tiles } => tiles.clone(),
        other => panic!("not raster: {other:?}"),
    }
}

/// Straight linear RGBA of every canvas pixel of the layer, read from its tile bytes.
fn pixels(v: &Viewer, layer: &str) -> Vec<[f32; 4]> {
    let assets = v.run().render_assets().unwrap();
    let ts = tiles(v, layer);
    let gw = ts.iter().map(|t| t.width.value as i64).max().unwrap();
    let gh = ts.iter().map(|t| t.height.value as i64).max().unwrap();
    let mut out = vec![[f32::NAN; 4]; (W * W) as usize];
    for t in &ts {
        let body = &assets[&t.content_digest.digest];
        let (tw, th) = (t.width.value as i64, t.height.value as i64);
        let colour_len = (tw * th * 12) as usize;
        let f = |k: usize| f32::from_le_bytes(body[k..k + 4].try_into().unwrap());
        for y in 0..th {
            for x in 0..tw {
                let (dx, dy) = (t.column * gw + x, t.row * gh + y);
                if dx < 0 || dy < 0 || dx >= i64::from(W) || dy >= i64::from(W) {
                    continue;
                }
                let i = (y * tw + x) as usize;
                out[(dy * i64::from(W) + dx) as usize] = [f(i * 12), f(i * 12 + 4), f(i * 12 + 8), f(colour_len + i * 4)];
            }
        }
    }
    out
}

#[test]
fn gradient_keeps_unpainted_hdr_bytes_and_decodes_through_prism() {
    let scratch = Scratch::new("fill-hdr");
    let path = scratch.0.join("hdr.psd");
    std::fs::write(&path, hdr_psd()).unwrap();
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}"));
    let layer = v.layers().into_iter().find(|r| r.name == "hdr").unwrap_or_else(|| panic!("{}", v.state_json())).layer_id;
    let before_tiles = tiles(&v, &layer);
    let before = pixels(&v, &layer);
    assert!(before.iter().all(|p| p[..3] == HDR && p[3] == 1.0), "the 32-bit layer imports as stored: {:?}", before[0]);

    // Foreground to transparent over x 0..32: columns 32..64 lie past the drag end (alpha 0).
    let fg = [0.2f32, 0.4, 0.6];
    let colour = vec![ColourStop { location: 0, midpoint: 50.0, colour: fg }, ColourStop { location: 4096, midpoint: 50.0, colour: fg }];
    let alpha = vec![TransparencyStop { location: 0, midpoint: 50.0, opacity: 100.0 }, TransparencyStop { location: 4096, midpoint: 50.0, opacity: 0.0 }];
    let settings = FillSettings { stops: Some((colour.clone(), alpha.clone())), ..FillSettings::default() };
    let (from, to) = ([0.0, 8.0], [32.0, 8.0]);
    let r0 = v.run().snapshot().unwrap().document().revision;
    v.fill_gradient(&layer, from, to, &settings).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(v.run().snapshot().unwrap().document().revision, r0 + 1, "one chronicle step");
    let after = pixels(&v, &layer);

    // The oracle: pigment's gradient on the Prism-encoded layer, decoded by Prism's inverse.
    let canvas = Rect { x: 0, y: 0, width: W, height: W };
    let enc = DisplayEncoding::Srgb.encode(HDR).unwrap();
    let mut c_bytes = Vec::new();
    let mut a_bytes = Vec::new();
    for _ in 0..W * W {
        enc.iter().for_each(|c| c_bytes.extend_from_slice(&c.to_le_bytes()));
        a_bytes.extend_from_slice(&1.0f32.to_le_bytes());
    }
    let view = ColourView { bounds: canvas, colour: &c_bytes, alpha: &a_bytes };
    let g = RasterGradient { form: GradientForm::Stops, geometry: GradientGeometry::Linear, colour_stops: &colour, transparency_stops: &alpha, smoothness: SMOOTHNESS_FULL, reverse: false, dither: false, dither_seed: 0 };
    let oracle = pf::gradient_fill(&view, None, canvas, DragLine { from, to }, &g, &FillPaint::default(), &ToolLimits::default()).unwrap();

    let mut painted = 0;
    for y in 0..W as i64 {
        for x in 0..W as i64 {
            let p = after[(y * i64::from(W) + x) as usize];
            if x >= 32 {
                // Unpainted: the exact bytes, 1.5 not clamped, 0.002 not rescaled.
                assert_eq!(p.map(f32::to_bits), before[(y * i64::from(W) + x) as usize].map(f32::to_bits), "({x}, {y}) unpainted keeps its bytes");
                assert_eq!(oracle.coverage.at(x, y), 0);
                continue;
            }
            painted += 1;
            let i = (y as usize) * W as usize + x as usize;
            let f = |b: &[u8], k: usize| f32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap());
            let rgb = [f(&oracle.colour, i * 3), f(&oracle.colour, i * 3 + 1), f(&oracle.colour, i * 3 + 2)];
            let want = DisplayEncoding::Srgb.decode(rgb).unwrap();
            for c in 0..3 {
                assert!((p[c] - want[c]).abs() <= 1.0e-6 * want[c].abs().max(1.0e-3), "({x}, {y}) channel {c}: {} vs Prism round trip {}", p[c], want[c]);
            }
            assert!((p[3] - f(&oracle.alpha, i)).abs() <= 1.0e-6, "({x}, {y}) alpha");
        }
    }
    assert_eq!(painted, 32 * W as usize);

    // Undo restores the tiles byte-identical; redo returns the fill.
    v.undo().unwrap();
    assert_eq!(tiles(&v, &layer), before_tiles);
    assert_eq!(pixels(&v, &layer).iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>(), before.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>());
    v.redo().unwrap();
    assert_eq!(pixels(&v, &layer).iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>(), after.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>());
}
