//! CPU painter: every primitive draws, scissor clips, flat fills equal tokens (AC-COL-01), the text caches hit, and
//! golden pixel hashes pin the toolbar, tab row, slider and layer row (BoxText stand-in glyphs, procedural icons).
use kumihimo_ui::display::{Border, DisplayList, IconId, ImageId, Prim};
use kumihimo_ui::gallery::{self, GalleryIcons, Scene};
use kumihimo_ui::geom::{Point, Rect, Rgba8, Scale};
use kumihimo_ui::paint::{Canvas, paint};
use kumihimo_ui::text::{BoxText, FaceId, GlyphKey, Text};
use kumihimo_ui::tokens::{ColorToken, TextToken, Theme};
use kumihimo_ui::tree::Interaction;
use std::collections::HashMap;

fn text() -> Text {
    Text::new(Box::new(BoxText))
}
fn tok(t: ColorToken) -> Rgba8 {
    t.resolve(Theme::Dark)
}

#[test]
fn every_primitive_draws_and_scissor_clips() {
    let bg = tok(ColorToken::SurfaceFrame);
    let fg = tok(ColorToken::AccentBase);
    let mut t = text();
    let line = t.shape("Ab", TextToken::Body, 0).unwrap();
    let r = Rect::xywh(4.0, 4.0, 24.0, 24.0);
    let prims = vec![
        Prim::Rect { rect: r, color: fg },
        Prim::RRect { rect: r, radius: 6.0, fill: fg, border: Some(Border { width: 1.0, color: tok(ColorToken::EdgeControl) }) },
        Prim::Line { a: Point::new(4.0, 4.0), b: Point::new(28.0, 28.0), width: 2.0, color: fg },
        Prim::Dash { a: Point::new(4.0, 16.0), b: Point::new(28.0, 16.0), width: 1.0, dash: 4.0, phase: 0.0, on: fg, off: Some(tok(ColorToken::OverlayAntsB)) },
        Prim::Glyphs { origin: Point::new(6.0, 22.0), token: TextToken::Body, glyphs: line.glyphs.clone(), color: fg },
        Prim::Icon { rect: r, icon: IconId(2), form: fg, hot: tok(ColorToken::TextPrimary) },
        Prim::Image { rect: r, image: gallery::THUMB_IMAGE },
        Prim::Checker { rect: r, a: tok(ColorToken::CanvasCheckerA), b: tok(ColorToken::CanvasCheckerB), cell: 8.0 },
        Prim::Ramp { rect: r, corners: [fg, bg, bg, fg] },
        Prim::Shadow { rect: r, radius: 6.0, offset_y: 2.0, blur: 8.0, color: Rgba8::new(0, 0, 0, 102) },
    ];
    for p in prims {
        let kind = p.kind();
        let mut c = Canvas::new(32, 32, bg).unwrap();
        let mut list = DisplayList::default();
        list.push(p.clone());
        let st = paint(&mut c, &list, Scale::ONE, &mut t, &mut GalleryIcons::default());
        assert_eq!(st.missing_resources + st.glyph_refusals, 0, "{kind}");
        let changed = c.rgba().chunks(4).filter(|px| px[..3] != [bg.r, bg.g, bg.b]).count();
        assert!(changed > 8, "{kind} drew {changed} px");
        // The same primitive under a scissor that excludes it draws nothing.
        let mut c = Canvas::new(32, 32, bg).unwrap();
        let clipped = DisplayList { items: vec![Prim::PushScissor(Rect::xywh(30.0, 30.0, 2.0, 2.0)), p, Prim::PopScissor] };
        paint(&mut c, &clipped, Scale::ONE, &mut t, &mut GalleryIcons::default());
        let inside = (0..30u32).flat_map(|y| (0..30u32).map(move |x| (x, y))).filter(|&(x, y)| c.pixel(x, y) != bg).count();
        assert_eq!(inside, 0, "{kind} leaked outside its scissor");
    }
    let mut c = Canvas::new(8, 8, bg).unwrap();
    let st = paint(&mut c, &DisplayList { items: vec![Prim::PopScissor, Prim::Image { rect: Rect::xywh(0.0, 0.0, 8.0, 8.0), image: ImageId(99) }] }, Scale::ONE, &mut t, &mut GalleryIcons::default());
    assert_eq!((st.scissor_underflow, st.missing_resources), (1, 1), "typed counters, no panic");
    assert!(Canvas::new(0, 4, bg).is_none() && Canvas::new(20_000, 4, bg).is_none());
}

/// AC-COL-01 frame half: every colour that fills more than a few pixels of a widget frame is a theme token.
#[test]
fn flat_fills_equal_tokens() {
    let tokens: Vec<Rgba8> = ColorToken::ALL.iter().map(|t| tok(*t)).collect();
    struct NoGlyphs;
    impl kumihimo_ui::text::TextPort for NoGlyphs {
        fn shape(&mut self, s: &str, st: &kumihimo_ui::tokens::TextStyle) -> Result<kumihimo_ui::text::ShapedLine, kumihimo_ui::text::TextError> {
            BoxText.shape(s, st)
        }
        fn rasterize(&mut self, _: GlyphKey) -> Result<kumihimo_ui::text::GlyphMask, kumihimo_ui::text::TextError> {
            Ok(Default::default())
        }
    }
    for scene in Scene::ALL {
        let mut t = Text::new(Box::new(NoGlyphs));
        let (c, _, _) = gallery::render(scene, Scale::ONE, Theme::Dark, &Interaction::default(), &mut t);
        let mut hist: HashMap<[u8; 3], usize> = HashMap::new();
        for px in c.rgba().chunks(4) {
            *hist.entry([px[0], px[1], px[2]]).or_default() += 1;
        }
        for (rgb, n) in hist {
            // Icons and the thumbnail are content, not chrome; their pixels are excluded by count (AA edges are few).
            if n > 24 && !tokens.iter().any(|t| [t.r, t.g, t.b] == rgb) {
                let gallery_grey = rgb[0] == rgb[1] && rgb[1] == rgb[2] && n <= 64;
                assert!(gallery_grey, "{}: non-token colour {rgb:?} on {n} px", scene.name());
            }
        }
    }
}

#[test]
fn text_caches_hit_and_ellipsize_fits() {
    let mut t = text().with_capacity(2, 2);
    t.shape("Layers", TextToken::Body, 0).unwrap();
    t.shape("Layers", TextToken::Body, 0).unwrap();
    assert_eq!((t.stats().shape_hits, t.stats().shape_misses), (1, 1));
    let key = GlyphKey { face: FaceId(0), glyph_id: u32::from('L'), px_size_64: 12 * 64, subpx_x: 1 };
    t.mask(key).unwrap();
    t.mask(key).unwrap();
    assert_eq!((t.stats().mask_hits, t.stats().mask_misses), (1, 1));
    t.shape("a", TextToken::Body, 0).unwrap();
    t.shape("b", TextToken::Body, 0).unwrap();
    assert_eq!(t.stats().resets, 1, "a full cache resets whole");
    let w = t.measure("Portrait retouch", TextToken::Body, 0);
    let cut = t.ellipsize("Portrait retouch", TextToken::Body, 0, w / 2.0);
    assert!(cut.ends_with('\u{2026}') && t.measure(&cut, TextToken::Body, 0) <= w / 2.0);
    assert_eq!(t.ellipsize("Portrait retouch", TextToken::Body, 0, w), "Portrait retouch");
    assert!(t.shape(&"x".repeat(5000), TextToken::Body, 0).is_err(), "over the label bound is typed");
    // Tracking: menu titles gain 0.4 px per glyph.
    let plain = t.measure("FILE", TextToken::Body, 0);
    let menu = t.measure("FILE", TextToken::MenuTitle, 0);
    assert!(menu > plain * 11.0 / 12.0, "menu_title tracking applied");
}

/// Golden pixel hashes (FNV-1a 64 over size and RGBA) for the four gallery widgets at scale 1 and 1.5, dark theme.
/// A change here is a visible change: open the gallery PNGs before re-pinning.
#[test]
fn golden_widget_pixels() {
    let expected: [(Scene, f32, u64); 8] = [
        (Scene::Toolbar, 1.0, 0x54D96A7D99AC1B3C),
        (Scene::TabRow, 1.0, 0x18AF722E07D1B0A4),
        (Scene::Slider, 1.0, 0x1485673531CE5F4A),
        (Scene::LayerRow, 1.0, 0x4B65DA05AD1A3676),
        (Scene::Toolbar, 1.5, 0x7D381C22B79F8C1F),
        (Scene::TabRow, 1.5, 0x872A8F97B4925B4B),
        (Scene::Slider, 1.5, 0x62E096C82065C7D0),
        (Scene::LayerRow, 1.5, 0x2BA08F0AD8A6D691),
    ];
    let mut got = Vec::new();
    for (scene, s, want) in expected {
        let (c, _, _) = gallery::render(scene, Scale::new(s).unwrap(), Theme::Dark, &Interaction::default(), &mut text());
        got.push(format!("({:?}, {s}, 0x{:016X})", scene, c.fnv64()));
        if c.fnv64() != want {
            println!("golden {} at {s}: got 0x{:016X}", scene.name(), c.fnv64());
        }
    }
    let all_match = expected.iter().zip(&got).all(|((_, _, w), g)| g.ends_with(&format!("0x{w:016X})")));
    assert!(all_match, "golden mismatch:\n{}", got.join("\n"));
}
