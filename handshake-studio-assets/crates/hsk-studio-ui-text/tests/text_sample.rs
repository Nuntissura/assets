//! Real UI text through kumihimo-ui's port over hsk-studio-type (Inter, unhinted grey coverage): the type specimen and
//! the four gallery widgets at the design scales. Asserts real ink, quarter-pixel bins, caching and determinism. With
//! KUMIHIMO_UI_SAMPLE_DIR set, writes the PNGs there for the designer's judgement (build-order B3); that judgement is
//! not something a test can prove.
use hsk_studio_accord::ActorContext;
use hsk_studio_interop_raster::{Colour, PngCompression, PngEncodeRequest, Samples, encode_png};
use hsk_studio_ui_text::TypeText;
use kumihimo_ui::gallery::{self, Scene};
use kumihimo_ui::geom::Scale;
use kumihimo_ui::paint::Canvas;
use kumihimo_ui::text::{Text, TextPort};
use kumihimo_ui::tokens::{ColorToken, TextToken, Theme};
use kumihimo_ui::tree::Interaction;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}
fn inter() -> TypeText {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let mut t = TypeText::new(actor);
    let regular = std::fs::read(root().join("hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf")).unwrap();
    let bold = std::fs::read(root().join("hsk-studio-interop-fig/tests/fixtures/Inter-Bold.ttf")).unwrap();
    t.add_face("Inter", 400, regular);
    t.add_face("Inter", 700, bold);
    t
}
fn text(gamma: f32) -> Text {
    let mut t = Text::new(Box::new(inter()));
    t.set_coverage_gamma(gamma);
    t
}
fn write_png(name: &str, c: &Canvas) {
    let Some(dir) = std::env::var_os("KUMIHIMO_UI_SAMPLE_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let samples = Samples::U8(c.rgba().to_vec());
    let png = encode_png(&PngEncodeRequest { width: c.width(), height: c.height(), colour: Colour::Rgba, samples: &samples, icc: None, compression: PngCompression::Balanced }).unwrap();
    std::fs::write(dir.join(name), png).unwrap();
    println!("wrote {}", dir.join(name).display());
}
/// Pixels that differ from the panel fill: the ink a label drew.
fn ink(c: &Canvas, theme: Theme) -> usize {
    let bg = ColorToken::SurfacePanel.resolve(theme);
    c.rgba().chunks(4).filter(|p| p[0] != bg.r || p[1] != bg.g || p[2] != bg.b).count()
}

#[test]
fn type_port_shapes_inter_with_tnum_and_real_advances() {
    let mut port = inter();
    let body = TextToken::Body.style();
    let a = port.shape("Layers", &body).unwrap();
    assert_eq!(a.glyphs.len(), 6);
    assert!(a.width > 30.0 && a.width < 45.0, "Inter 12px 'Layers' is about 37 logical px wide: {}", a.width);
    assert!(a.cap_height > 8.0 && a.cap_height < 9.5, "Inter cap height at 12px: {}", a.cap_height);
    let value = TextToken::Value.style();
    let ones = port.shape("1111", &value).unwrap().width;
    let eights = port.shape("8888", &value).unwrap().width;
    assert!((ones - eights).abs() < 0.001, "tabular figures: '1111' {ones} vs '8888' {eights}");
    let mask = port.rasterize(kumihimo_ui::text::GlyphKey { face: a.glyphs[0].face, glyph_id: a.glyphs[0].glyph_id, px_size_64: 12 * 64, subpx_x: 0 }).unwrap();
    assert!(mask.width > 0 && mask.height >= 8 && mask.top < 0, "an 'L' at 12px has ink above the baseline");
    let shifted = port.rasterize(kumihimo_ui::text::GlyphKey { face: a.glyphs[0].face, glyph_id: a.glyphs[0].glyph_id, px_size_64: 12 * 64, subpx_x: 2 }).unwrap();
    assert_ne!(mask, shifted, "quarter-pixel bins rasterise differently");
}

#[test]
fn specimen_and_widgets_render_real_text_deterministically() {
    let theme = Theme::Dark;
    for (gamma, tag) in [(1.0f32, "g100"), (1.4, "g140")] {
        for s in [1.0f32, 1.25, 1.5, 2.0] {
            let scale = Scale::new(s).unwrap();
            let mut t = text(gamma);
            let (c, _, st) = gallery::render_specimen(scale, theme, &mut t, 560.0, 260.0);
            assert_eq!(st.glyph_refusals, 0, "every glyph rasterised");
            assert!(st.glyphs > 400, "specimen draws its glyphs: {}", st.glyphs);
            assert!(ink(&c, theme) > 5_000, "specimen has ink at scale {s}");
            let again = gallery::render_specimen(scale, theme, &mut t, 560.0, 260.0).0;
            assert_eq!(c.fnv64(), again.fnv64(), "same frame twice is byte-identical (cached glyphs)");
            assert!(t.stats().mask_hits > 0 && t.stats().shape_hits > 0);
            write_png(&format!("text-specimen-{tag}-s{}.png", (s * 100.0) as u32), &c);
        }
    }
    let ui = Interaction::default();
    for s in [1.0f32, 2.0] {
        let scale = Scale::new(s).unwrap();
        let mut t = text(1.0);
        for scene in Scene::ALL {
            let (c, panel, st) = gallery::render(scene, scale, theme, &ui, &mut t);
            assert_eq!(st.glyph_refusals, 0);
            assert!(kumihimo_ui::checks::clipped_labels(panel.tree(), &mut t).is_empty(), "{}: no clipped label", scene.name());
            write_png(&format!("widget-{}-s{}.png", scene.name(), (s * 100.0) as u32), &c);
        }
    }
}
