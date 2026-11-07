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

/// Golden pixel hashes (FNV-1a 64 over size and RGBA) for the four gallery widgets at scale 1, 1.25 and 1.5, dark theme.
/// A change here is a visible change: open the gallery PNGs before re-pinning.
#[test]
fn golden_widget_pixels() {
    let expected: [(Scene, f32, u64); 12] = [
        (Scene::Toolbar, 1.0, 0x54D96A7D99AC1B3C),
        (Scene::TabRow, 1.0, 0xC28214A37D261F94),
        (Scene::Slider, 1.0, 0x1485673531CE5F4A),
        (Scene::LayerRow, 1.0, 0x4B65DA05AD1A3676),
        (Scene::Toolbar, 1.25, 0xAE9F5AB34FA54BBB),
        (Scene::TabRow, 1.25, 0xF47E6C7AE45200E7),
        (Scene::Slider, 1.25, 0x741DF48AE4B8E761),
        (Scene::LayerRow, 1.25, 0xB26D2A5A246E1ED8),
        (Scene::Toolbar, 1.5, 0xC7ED091386B02267),
        (Scene::TabRow, 1.5, 0xAB58B214AF165743),
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

/// F1: a 1 px edge paints hairline() and a 2 px edge edge_px(2) at any position, inside the owner.
#[test]
fn edges_keep_their_thickness_wherever_they_sit() {
    use kumihimo_ui::display::Side;
    let bg = tok(ColorToken::SurfaceFrame);
    let fg = tok(ColorToken::AccentBase);
    for s in [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.25, 3.0] {
        let scale = Scale::new(s).unwrap();
        for x0 in [0.0f32, 3.0, 72.0, 72.5, 101.0] {
            for (px, side) in [(1u8, Side::Left), (2, Side::Left), (1, Side::Right), (2, Side::Top), (1, Side::Bottom), (2, Side::Bottom)] {
                let owner = Rect::new(x0, x0 / 2.0, x0 + 20.0, x0 / 2.0 + 24.0);
                let mut c = Canvas::new(scale.edge(140.0) as u32, scale.edge(90.0) as u32, bg).unwrap();
                let list = DisplayList { items: vec![Prim::Edge { owner, side, px, color: fg }] };
                paint(&mut c, &list, scale, &mut text(), &mut GalleryIcons::default());
                let o = scale.snap(owner);
                let (along, across) = match side {
                    Side::Left | Side::Right => ((o.y0 + o.y1) / 2, (o.x0..o.x1).filter(|&x| c.pixel(x as u32, ((o.y0 + o.y1) / 2) as u32) == fg).count()),
                    Side::Top | Side::Bottom => ((o.x0 + o.x1) / 2, (o.y0..o.y1).filter(|&y| c.pixel(((o.x0 + o.x1) / 2) as u32, y as u32) == fg).count()),
                };
                let _ = along;
                assert_eq!(across as i32, scale.edge_px(px), "{px} px {side:?} edge at x0 {x0}, scale {s}");
                let outside = c.rgba().chunks(4).enumerate().filter(|(i, p)| {
                    let (x, y) = ((*i as u32 % c.width()) as i32, (*i as u32 / c.width()) as i32);
                    p[..3] == [fg.r, fg.g, fg.b] && !(x >= o.x0 && x < o.x1 && y >= o.y0 && y < o.y1)
                });
                assert_eq!(outside.count(), 0, "edge stays inside its owner");
            }
        }
    }
    assert_eq!(Scale::new(1.5).unwrap().hairline(), 1);
    assert_eq!(Scale::new(1.25).unwrap().edge_px(2), 3);
}

/// F9: ellipsis costs a logarithmic number of shapes and does not flood the line cache.
#[test]
fn ellipsize_binary_searches_without_caching_prefixes() {
    let mut t = text().with_capacity(64, 64);
    let long: String = "Layer copy ".repeat(372); // 4092 bytes
    let before = t.stats();
    let cut = t.ellipsize(&long, TextToken::Body, 0, 200.0);
    let after = t.stats();
    assert!(cut.ends_with('\u{2026}') && t.measure(&cut, TextToken::Body, 0) <= 200.0);
    assert!(cut.chars().count() > 20, "takes the longest fitting prefix: {cut}");
    assert_eq!(after.resets, before.resets, "line cache not flushed");
    assert!(after.shape_misses - before.shape_misses <= 2, "only the full text and the result are cached");
    let huge = "x".repeat(9000);
    let cut = t.ellipsize(&huge, TextToken::Body, 0, 10_000.0);
    assert!(cut.len() <= kumihimo_ui::text::MAX_LABEL_BYTES && cut.ends_with('\u{2026}'), "over-long text is cut, never silently unshaped");
}
