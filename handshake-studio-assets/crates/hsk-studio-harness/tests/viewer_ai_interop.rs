//! SMB-ILLUSTRATOR-INTEROP (feature `viewer`): a PDF-compatible `.ai` file imports through Press's
//! PDF reading and interop-vector into an editable vector document, and the document exports as
//! SVG through interop-vector. The fixture is generated (Press `PdfExport`, page 150 x 75 pt =
//! 200 x 100 px, an Illustrator private-data marker in the info dictionary); an Illustrator-saved
//! file is case AI-NS2 of the attended D5 session.
#![cfg(all(feature = "viewer", feature = "vector-interop"))]

mod common;

use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_press::*;

const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<PathCommand> {
    vec![PathCommand::MoveTo(x0, y0), PathCommand::LineTo(x1, y0), PathCommand::LineTo(x1, y1), PathCommand::LineTo(x0, y1), PathCommand::Close]
}

/// Red rectangle (6..30 x 6..24 pt), a gradient clipped to 60..90 x 6..24 pt, a 50 % multiply
/// group with a green rectangle (105..135 x 6..24 pt), one glyph.
fn fixture() -> Vec<u8> {
    let options = ExportOptions { info_title: Some("AIPrivateData Illustrator fixture"), ..ExportOptions::default() };
    let mut ex = PdfExport::new(options, &Limits::default()).unwrap();
    let p = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 150.0, 75.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    ex.fill_path(p, &rect(6.0, 6.0, 30.0, 24.0), FillRule::NonZero, Colour::DeviceRgb([1.0, 0.0, 0.0])).unwrap();
    ex.push_clip(p, &rect(60.0, 6.0, 90.0, 24.0), FillRule::NonZero).unwrap();
    let stops = [GradientStop::new(0.0, [0.0, 0.0, 1.0]), GradientStop::new(1.0, [0.0, 0.0, 1.0])];
    let g = Gradient { geometry: GradientGeometry::Axial { from: [50.0, 0.0], to: [100.0, 0.0] }, stops: &stops, extend: [true, true], spread: GradientSpread::Pad, matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], profile: None };
    ex.fill_path_gradient(p, &rect(50.0, 0.0, 100.0, 40.0), FillRule::NonZero, &g).unwrap();
    ex.pop_clip(p).unwrap();
    ex.begin_group(p).unwrap();
    ex.fill_path(p, &rect(105.0, 6.0, 135.0, 24.0), FillRule::NonZero, Colour::DeviceRgb([0.0, 1.0, 0.0])).unwrap();
    ex.end_group(p, GroupStyle { opacity: 0.5, blend: BlendMode::Multiply, isolated: true, knockout: false, soft_mask: None }).unwrap();
    let font = ex.embed_truetype(INTER, 0, &[(161, 740)], &[(161, "H")]).unwrap();
    ex.show_glyphs(p, font, 18.0, [10.0, 45.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0)]).unwrap();
    ex.finish(&CancellationToken::default()).unwrap().0
}

/// Straight RGBA of a frame pixel (the PDF has no background: unpainted pixels are transparent).
fn px(f: &hsk_studio_harness::view::render::Frame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * f.width + x) * 4) as usize;
    [f.rgba8[i], f.rgba8[i + 1], f.rgba8[i + 2], f.rgba8[i + 3]]
}

#[test]
fn ai_imports_as_editable_vectors_and_exports_as_svg() {
    let scratch = Scratch::new("ai-interop");
    let src = scratch.0.join("fixture.ai");
    std::fs::write(&src, fixture()).unwrap();
    let dest = scratch.0.join("fixture.handshake");
    let mut v = Viewer::new(Prefer::Cpu);
    // Press embeds a subset (no character map), so the run is laid out with the viewer's Inter,
    // registered under the PDF font's PostScript name.
    v.add_font("Inter-Regular", INTER.to_vec());
    let losses = v.import_ai(&src, &dest, false).unwrap();
    for code in ["ai_native_editability_not_imported", "ai_private_data_not_imported"] {
        assert!(losses.iter().any(|l| l.ends_with(code)), "{code} in {losses:?}");
    }
    // Editable: the vector layers carry Folio styles; pixels where the PDF drew them (y flipped,
    // x 4/3): red at (24, 80) px, the clipped gradient blue only inside 80..120 px, the green group
    // at 50 % opacity, transparent where nothing is drawn (the PDF has no background).
    assert!(v.layers().iter().filter(|r| r.kind == "vector").count() >= 4);
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).clone();
    assert_eq!((f.width, f.height), (200, 100));
    assert_eq!(px(&f, 24, 80), [255, 0, 0, 255]);
    assert_eq!(px(&f, 100, 80), [0, 0, 255, 255], "inside the clip");
    assert_eq!(px(&f, 72, 80)[3], 0, "gradient outside the clip");
    let green = px(&f, 160, 80);
    assert!(green[1] == 255 && green[0] == 0 && (120..=135).contains(&green[3]), "{green:?}");
    // The glyph stays text (Inter's layout puts "H" where the PDF did): a text layer whose
    // story reads "H", drawn opaque near its left stem (baseline 40 px, x from 13.3 px).
    let text = v.text_entry().unwrap();
    assert_eq!(text.stories.iter().filter_map(|st| st.inline_text()).collect::<Vec<_>>(), ["H"]);
    assert!(v.layers().iter().any(|r| r.kind == "text"));
    assert!((14..22).any(|x| (24..38).any(|y| px(&f, x, y)[3] > 200)), "the H is drawn");
    // The imported document exports as SVG (a red path, the clip reported as a loss).
    let (svg, svg_losses) = v.vector_svg().unwrap();
    assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"));
    assert!(svg.contains("#ff0000") || svg.contains("#f00"), "{svg}");
    assert!(svg_losses.iter().any(|l| l.contains("clip")), "{svg_losses:?}");
    let out = scratch.0.join("export.svg");
    v.export_vector_svg(&out, false).unwrap();
    assert!(v.export_vector_svg(&out, false).is_err());
    // Without a font that lays the run out, the glyph stays outlines (typed loss).
    let mut plain = Viewer::new(Prefer::Cpu);
    let losses = plain.import_ai(&src, &scratch.0.join("plain.handshake"), false).unwrap();
    assert!(losses.iter().any(|l| l.ends_with("ai_text_as_outlines")) && plain.text_entry().unwrap().stories.is_empty(), "{losses:?}");
    // Not an AI file, and the package is never written over an existing file.
    std::fs::write(scratch.0.join("plain.txt"), b"not an ai file").unwrap();
    assert!(v.import_ai(&scratch.0.join("plain.txt"), &scratch.0.join("x.handshake"), false).is_err());
    assert!(v.import_ai(&src, &dest, false).is_err());
}

const CMYK_ICC: &[u8] = include_bytes!("../../hsk-studio-prism/tests/oracle/profiles/hsk-synth-cmyk-v2.icc");

/// A hand-assembled PDF (objects numbered from 1, exact xref offsets).
fn assemble(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7
%AI12_FileFormatLevel: 3
".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj
", i + 1).as_bytes());
        out.extend(body);
        out.extend(b"
endobj
");
    }
    let xref = out.len();
    out.extend(format!("xref
0 {}
0000000000 65535 f 
", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n 
").as_bytes());
    }
    out.extend(format!("trailer
<< /Size {} /Root 1 0 R >>
startxref
{xref}
%%EOF
", objects.len() + 1).as_bytes());
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>
stream
", data.len()).into_bytes();
    v.extend(data);
    v.extend(b"
endstream");
    v
}

#[test]
fn ai_layers_and_document_colours_survive_import_save_and_reopen() {
    // Layer "Artwork": a CMYK square in the output-intent space; layer "Hidden" (off): a spot square.
    let content = b"/OC /L1 BDC
0 1 1 0 k 10 10 40 30 re f
EMC
/OC /L2 BDC
/Spot cs 0.5 scn 80 10 30 30 re f
EMC
";
    let pdf = assemble(&[
        b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R] /D << /OFF [6 0 R] >> >> /OutputIntents [<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (CMYK) /DestOutputProfile 7 0 R >>] >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 150 75] /Resources << /Properties << /L1 5 0 R /L2 6 0 R >> /ColorSpace << /Spot [/Separation /PANTONE#20185#20C /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.91 0.76 0] /N 1 >>] >> >> /Contents 4 0 R >>".to_vec(),
        stream("", content),
        b"<< /Type /OCG /Name (Artwork) >>".to_vec(),
        b"<< /Type /OCG /Name (Hidden) >>".to_vec(),
        stream("/N 4", CMYK_ICC),
    ]);
    let scratch = Scratch::new("ai-layers");
    let src = scratch.0.join("layers.ai");
    std::fs::write(&src, pdf).unwrap();
    let dest = scratch.0.join("layers.handshake");
    let mut v = Viewer::new(Prefer::Cpu);
    v.import_ai(&src, &dest, false).unwrap();
    let rows = v.layers();
    let layer = |name: &str| rows.iter().find(|r| r.name == name && r.kind == "group").map(|r| r.visible);
    assert_eq!((layer("Artwork"), layer("Hidden")), (Some(true), Some(false)));
    // CMYK and the spot ink are swatches; the output intent is the document CMYK profile.
    let check = |v: &Viewer| {
        let entry = v.vector_entry().unwrap();
        let r = &entry.records;
        assert!(r.icc_profiles.iter().any(|p| p.working_space && entry.icc.get(&p.content_digest.digest).is_some_and(|b| b == CMYK_ICC)));
        assert!(r.swatches.iter().any(|s| s.components == [0.0, 1.0, 1.0, 0.0]));
        assert!(r.swatches.iter().any(|s| s.spot.as_ref().is_some_and(|i| i.ink_name == "PANTONE 185 C")));
        assert_eq!(r.swatch_paints.len(), 2);
    };
    check(&v);
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).clone();
    // The visible CMYK square is drawn, the hidden layer's square is not.
    assert!(px(&f, 30, 60)[3] > 200 && px(&f, 120, 60)[3] == 0, "{:?} {:?}", px(&f, 30, 60), px(&f, 120, 60));
    // Saved and reopened, the swatches, the profile and its bytes come back.
    let saved = scratch.0.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&saved).unwrap();
    check(&again);
}
