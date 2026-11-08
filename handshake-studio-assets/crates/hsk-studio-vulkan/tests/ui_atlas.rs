//! SMB-VK-UI-PASS-1 item 8(a), no device: the shelf packer, page growth and resets, glyph residency across frames,
//! the 4096-byte label, a 3,000-glyph CJK-like flood through a forced 512 page (pass breaks), a million rects (instance
//! cap, pass breaks, `ui_pass_limit`), the scissor flood and non-finite primitives.
mod support;

use hsk_studio_vulkan::Refusal;
use hsk_studio_vulkan::atlas::{Atlas, Placed, ShelfPacker, Slot};
use hsk_studio_vulkan::ui::{INSTANCE_BYTES, Plan, Recorder, UiLimits};
use kumihimo_ui::display::{DisplayList, Prim};
use kumihimo_ui::geom::{Point, Rect, Rgba8, Scale};
use kumihimo_ui::paint::NoResources;
use kumihimo_ui::text::{BoxText, Text};
use kumihimo_ui::tokens::TextToken;
use std::time::{Duration, Instant};
use support::GradientText;

const INK: Rgba8 = Rgba8::new(230, 230, 230, 255);

fn label(text: &mut Text, s: &str, x: f32, y: f32) -> Prim {
    let line = text.shape(s, TextToken::Body, 0).expect("shape");
    Prim::Glyphs { origin: Point::new(x, y), token: TextToken::Body, glyphs: line.glyphs, color: INK }
}

fn uploads(p: &Plan) -> usize {
    p.passes.iter().map(|q| q.glyph_uploads.len()).sum()
}

#[test]
fn shelf_packer_first_fit_rounds_shelves_to_four() {
    let mut p = ShelfPacker::new(64);
    assert_eq!(p.place(10, 5), Some((0, 0)), "first shelf, height 8");
    assert_eq!(p.place(10, 7), Some((10, 0)), "fits the 8 px shelf");
    assert_eq!(p.place(10, 9), Some((0, 8)), "taller: a new shelf of 12 below");
    assert_eq!(p.place(4, 2), Some((20, 0)), "first shelf it fits, not the last");
    assert_eq!(p.place(60, 3), Some((0, 20)), "too wide for both: a third shelf");
    assert_eq!(p.shelves(), 3);
    assert_eq!(p.place(65, 1), None, "wider than the page");
    assert_eq!(p.place(0, 4), None, "empty entries take no slot");
    assert_eq!(p.place(64, 41), None, "taller than what is left (64 - 24)");
    assert_eq!(p.place(64, 40), Some((0, 24)));
    assert_eq!(p.place(1, 1), Some((24, 0)), "room left on the first shelf");
    p.reset();
    assert_eq!((p.shelves(), p.place(64, 64)), (0, Some((0, 0))));
}

#[test]
fn atlas_grows_to_its_cap_resets_and_keeps_residents() {
    let mut a: Atlas<u32> = Atlas::new(1024, 4096);
    for k in 0..16 {
        assert!(matches!(a.place(k, 256, 256), Placed::New(_)), "{k}");
    }
    assert!(matches!(a.place(3, 256, 256), Placed::Resident(Slot { w: 256, h: 256, .. })));
    assert_eq!(a.place(99, 256, 256), Placed::Full);
    assert!(a.grow());
    assert_eq!((a.side(), a.len(), a.stats().growths), (2048, 0, 1), "growth clears the page");
    assert!(a.grow());
    assert_eq!(a.side(), 4096);
    assert!(!a.grow(), "4096 is the core minimum of maxImageDimension2D: the cap");
    assert!(matches!(a.place(1, 8, 8), Placed::New(_)));
    a.reset();
    assert_eq!((a.side(), a.len(), a.stats().resets), (4096, 0, 1));
    assert!(!a.set_identity(0), "same identity: no reset");
    assert!(a.set_identity(5));
    assert_eq!(a.stats().resets, 2);
}

#[test]
fn glyphs_stay_resident_and_the_page_resets_on_scale_epoch_and_lut() {
    let mut text = Text::new(Box::new(BoxText));
    let mut list = DisplayList::default();
    list.push(label(&mut text, "Layers Channels Paths", 10.0, 20.0));
    let mut rec = Recorder::new(UiLimits::default());
    let lists = [&list];
    let s1 = Scale::ONE;
    let cold = rec.record((300, 40), &lists, s1, &mut text, &mut NoResources, false).expect("record");
    assert!(uploads(&cold) > 0);
    assert!(cold.passes[0].glyph_reset, "a new page");
    let warm = rec.record((300, 40), &lists, s1, &mut text, &mut NoResources, false).unwrap();
    assert_eq!(uploads(&warm), 0, "unchanged text uploads nothing");
    assert!(!warm.passes[0].glyph_reset);
    assert_eq!(warm.passes[0].instances, cold.passes[0].instances, "same quads, same texels");

    let s2 = Scale::new(2.0).unwrap();
    let scaled = rec.record((600, 80), &lists, s2, &mut text, &mut NoResources, false).unwrap();
    assert!(scaled.passes[0].glyph_reset && uploads(&scaled) > 0, "scale change resets the page (monitor_change)");

    text.set_coverage_gamma(1.8);
    let lut = rec.record((600, 80), &lists, s2, &mut text, &mut NoResources, false).unwrap();
    assert!(lut.passes[0].glyph_reset && uploads(&lut) > 0, "a new LUT resets the page");

    let mut other = Text::new(Box::new(BoxText));
    let epoch = rec.record((600, 80), &lists, s2, &mut other, &mut NoResources, false).unwrap();
    assert!(epoch.passes[0].glyph_reset && uploads(&epoch) > 0, "a new text epoch resets the page");
}

#[test]
fn a_4096_byte_label_places_only_its_visible_glyphs() {
    let mut text = Text::new(Box::new(GradientText { cell: 12 }));
    // 1024 distinct CJK-like chars of 4 bytes would be 4096 bytes; 3-byte ideographs: 1365 chars, 4095 bytes.
    let s: String = (0..1365u32).filter_map(|i| char::from_u32(0x4E00 + i)).collect();
    assert!(s.len() <= 4096);
    let mut list = DisplayList::default();
    list.push(label(&mut text, &s, 4.0, 20.0));
    let mut rec = Recorder::new(UiLimits::default());
    let plan = rec.record((800, 40), &[&list], Scale::ONE, &mut text, &mut NoResources, false).unwrap();
    let placed = rec.glyph_atlas().len();
    let visible = (800.0 / (TextToken::Body.style().size * 0.9)).ceil() as usize + 1;
    assert!(placed <= visible, "placed {placed}, visible at most {visible}");
    assert!(plan.passes[0].instances.len() <= visible);
    assert_eq!(plan.stats.glyphs, 1365, "every glyph is seen, only visible ones placed");
}

#[test]
fn a_3000_glyph_flood_breaks_passes_in_a_forced_512_page() {
    let mut text = Text::new(Box::new(GradientText { cell: 32 })).with_capacity(4096, 8192);
    let mut list = DisplayList::default();
    for line in 0..60u32 {
        let s: String = (0..50u32).filter_map(|i| char::from_u32(0x4E00 + line * 50 + i)).collect();
        list.push(label(&mut text, &s, 2.0, 20.0 + line as f32 * 20.0));
    }
    let limits = UiLimits { glyph_min_side: 512, glyph_max_side: 512, ..UiLimits::default() };
    let mut rec = Recorder::new(limits);
    let t0 = Instant::now();
    let plan = rec.record((1400, 2500), &[&list], Scale::new(2.0).unwrap(), &mut text, &mut NoResources, false).expect("within 16 passes");
    assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", t0.elapsed());
    assert_eq!(uploads(&plan), 3000, "each glyph uploaded once");
    let per_page = (512 / 32) * (512 / 32);
    assert!(plan.passes.len() >= 3000 / per_page && plan.passes.len() <= 16, "passes {}", plan.passes.len());
    for (i, p) in plan.passes.iter().enumerate() {
        assert_eq!(p.glyph_side, 512);
        assert!(p.glyph_uploads.len() <= per_page, "pass {i}");
        assert!(p.glyph_reset, "pass {i}: a pass break resets the page");
        for u in &p.glyph_uploads {
            assert!(u.x + u.w <= 512 && u.y + u.h <= 512);
            assert_eq!(u.bytes.len(), (u.w * u.h) as usize);
        }
    }
}

#[test]
fn a_million_rects_respect_the_instance_cap_and_the_pass_limit() {
    let mut list = DisplayList::default();
    for i in 0..1_000_000u32 {
        let (x, y) = ((i % 1000) as f32, (i / 1000) as f32 % 700.0);
        list.push(Prim::Rect { rect: Rect::xywh(x, y, 1.0, 1.0), color: INK });
    }
    let mut text = Text::new(Box::new(BoxText));
    let mut rec = Recorder::new(UiLimits::default());
    let t0 = Instant::now();
    let plan = rec.record((1000, 700), &[&list], Scale::ONE, &mut text, &mut NoResources, false).expect("five passes");
    assert!(t0.elapsed() < Duration::from_secs(30), "stated bound 30 s (debug build): {:?}", t0.elapsed());
    let cap = (16usize << 20) / INSTANCE_BYTES;
    assert!(plan.passes.iter().all(|p| p.instances.len() <= cap));
    assert_eq!(plan.passes.iter().map(|p| p.instances.len()).sum::<usize>(), 1_000_000);
    assert_eq!(plan.passes.len(), 1_000_000usize.div_ceil(cap));
    let tight = UiLimits { instance_cap_bytes: INSTANCE_BYTES * 1000, ..UiLimits::default() };
    let mut rec = Recorder::new(tight);
    assert_eq!(rec.record((1000, 700), &[&list], Scale::ONE, &mut text, &mut NoResources, false).err(), Some(Refusal::UiPassLimit));
}

#[test]
fn scissor_flood_and_unbalanced_pops_are_bounded_and_counted() {
    let mut list = DisplayList::default();
    for i in 0..10_000 {
        list.push(Prim::PushScissor(Rect::xywh((i % 50) as f32, 0.0, 100.0, 100.0)));
    }
    list.push(Prim::Rect { rect: Rect::xywh(0.0, 0.0, 200.0, 200.0), color: INK });
    for _ in 0..10_003 {
        list.push(Prim::PopScissor);
    }
    let mut text = Text::new(Box::new(BoxText));
    let mut rec = Recorder::new(UiLimits::default());
    let plan = rec.record((300, 300), &[&list], Scale::ONE, &mut text, &mut NoResources, false).unwrap();
    assert_eq!(plan.stats.scissor_underflow, 3);
    assert_eq!(plan.passes[0].instances.len(), 1);
    assert_eq!(plan.passes[0].instances[0].quad, [49, 0, 100, 100], "the rect clipped to the intersected stack");
}

#[test]
fn non_finite_prims_are_skipped_and_quads_never_leave_the_pane() {
    let mut list = DisplayList::default();
    list.push(Prim::Rect { rect: Rect { x0: f32::NAN, y0: 0.0, x1: 10.0, y1: 10.0 }, color: INK });
    list.push(Prim::Line { a: Point::new(0.0, 0.0), b: Point::new(f32::INFINITY, 4.0), width: 1.0, color: INK });
    list.push(Prim::RRect { rect: Rect::xywh(0.0, 0.0, 10.0, 10.0), radius: f32::NAN, fill: INK, border: None });
    list.push(Prim::Rect { rect: Rect::xywh(-1.0e9, -1.0e9, 2.0e9, 2.0e9), color: INK });
    list.push(Prim::Rect { rect: Rect::xywh(1.0e9, 0.0, 10.0, 10.0), color: INK });
    let mut text = Text::new(Box::new(BoxText));
    let mut rec = Recorder::new(UiLimits::default());
    let plan = rec.record((64, 32), &[&list], Scale::ONE, &mut text, &mut NoResources, false).unwrap();
    assert_eq!(plan.stats.skipped_non_finite, 3);
    assert_eq!(plan.passes[0].instances.len(), 1, "the off-pane rect draws nothing");
    assert_eq!(plan.passes[0].instances[0].quad, [0, 0, 64, 32], "clipped to the pane");
}

/// Review F3: an image over max_image_pixels (MAX_PIXELS by default, brief bound (f)) is skipped and counted.
#[test]
fn an_image_over_the_pixel_bound_is_skipped_and_counted() {
    assert_eq!(UiLimits::default().max_image_pixels, hsk_studio_vulkan::MAX_PIXELS);
    struct Img(Vec<u8>);
    impl kumihimo_ui::paint::Resources for Img {
        fn icon(&mut self, _: kumihimo_ui::display::IconId, _: u32) -> Option<kumihimo_ui::paint::IconMask> {
            None
        }
        fn image(&self, _: kumihimo_ui::display::ImageId) -> Option<kumihimo_ui::paint::ImageRef<'_>> {
            Some(kumihimo_ui::paint::ImageRef { width: 11, height: 10, rgba: &self.0 })
        }
    }
    let mut list = DisplayList::default();
    list.push(Prim::Image { rect: Rect::xywh(0.0, 0.0, 20.0, 20.0), image: kumihimo_ui::display::ImageId(1) });
    list.push(Prim::Rect { rect: Rect::xywh(0.0, 0.0, 4.0, 4.0), color: INK });
    let mut text = Text::new(Box::new(BoxText));
    let mut rec = Recorder::new(UiLimits { max_image_pixels: 100, ..UiLimits::default() });
    let plan = rec.record((64, 32), &[&list], Scale::ONE, &mut text, &mut Img(vec![255; 11 * 10 * 4]), false).unwrap();
    assert_eq!(plan.stats.image_over_limit, 1);
    assert!(plan.images.is_empty());
    assert_eq!(plan.passes[0].instances.len(), 1, "the rect still draws");
}

/// Review F4: a glyph or an icon larger than its page's maximum is refused alone: counted, no page reset, no extra
/// pass, the rest of the frame draws.
#[test]
fn entries_larger_than_the_page_maximum_are_refused_alone() {
    struct Big;
    impl kumihimo_ui::paint::Resources for Big {
        fn icon(&mut self, _: kumihimo_ui::display::IconId, px: u32) -> Option<kumihimo_ui::paint::IconMask> {
            Some(kumihimo_ui::paint::IconMask { width: px, height: px, rg: vec![[255, 0]; (px * px) as usize] })
        }
        fn image(&self, _: kumihimo_ui::display::ImageId) -> Option<kumihimo_ui::paint::ImageRef<'_>> {
            None
        }
    }
    let mut text = Text::new(Box::new(GradientText { cell: 40 }));
    let mut list = DisplayList::default();
    list.push(label(&mut text, "AB", 2.0, 60.0));
    list.push(Prim::Icon { rect: Rect::xywh(0.0, 0.0, 600.0, 600.0), icon: kumihimo_ui::display::IconId(1), form: INK, hot: INK });
    list.push(Prim::Icon { rect: Rect::xywh(0.0, 0.0, 16.0, 16.0), icon: kumihimo_ui::display::IconId(2), form: INK, hot: INK });
    list.push(Prim::Rect { rect: Rect::xywh(0.0, 0.0, 4.0, 4.0), color: INK });
    let limits = UiLimits { glyph_min_side: 32, glyph_max_side: 32, icon_min_side: 512, icon_max_side: 512, ..UiLimits::default() };
    let mut rec = Recorder::new(limits);
    let plan = rec.record((700, 700), &[&list], Scale::ONE, &mut text, &mut Big, false).expect("the frame draws");
    assert_eq!(plan.passes.len(), 1, "no pass break");
    assert_eq!(plan.stats.glyph_refusals, 2, "both 40 px glyphs exceed the 32 page");
    assert_eq!(plan.stats.icon_over_limit, 1, "the 600 px icon exceeds the 512 page");
    assert_eq!(rec.glyph_atlas().stats().resets, 1, "only the first page identity");
    assert_eq!(rec.icon_atlas().stats().resets, 0);
    assert_eq!(plan.passes[0].instances.len(), 3, "the small icon (2) and the rect");
}
