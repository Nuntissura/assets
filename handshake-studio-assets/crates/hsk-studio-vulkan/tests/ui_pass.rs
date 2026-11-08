//! SMB-VK-UI-PASS-1 items 3 to 6 and 8(b), on the real device (GPU test group, validation on in debug builds): every
//! primitive class drawn by the UI pass against `kumihimo_ui::paint::paint` on the same list at scales 1, 1.25, 1.5
//! and 2; kumihimo-ui's gallery scenes and specimen; cold, warm, reset and growth frames give the same bytes. Bounds:
//! the scope's defaults (flat exact; glyph, icon, translucent image and ramp 1; AA 2, stacked 4) tightened to the
//! maxima measured on the RTX 3090 (lead ruling 2026-11-08): 0 for flat, translucent rect and image, box glyphs,
//! icons, the gallery and the specimen; 1 for gradient glyphs, rrect, line, dash, shadow and ramp. Each test ends with
//! zero validation errors.
mod support;

use hsk_studio_vulkan::ui::UiLimits;
use hsk_studio_vulkan::{GpuClientPort, Owner, OwnerConfig, UiFrame, UiPass};
use kumihimo_ui::display::{Border, DisplayList, IconId, ImageId, Prim, Side};
use kumihimo_ui::gallery::{self, GalleryIcons, Scene};
use kumihimo_ui::geom::{Point, Rect, Rgba8, Scale};
use kumihimo_ui::paint::{Canvas, IconMask, ImageRef, Resources, paint};
use kumihimo_ui::text::{BoxText, Text};
use kumihimo_ui::tokens::{ColorToken, TextToken, Theme};
use kumihimo_ui::tree::Interaction;
use support::{GradientText, assert_validation_clean, diff};

const SCALES: [f32; 4] = [1.0, 1.25, 1.5, 2.0];
const CLEAR: Rgba8 = Rgba8::new(19, 19, 19, 255);
const W: f32 = 160.0;
const H: f32 = 120.0;

/// Test resources: opaque and translucent images, a short buffer, and generated two-tint icons.
struct Res {
    opaque: Vec<u8>,
    translucent: Vec<u8>,
}
impl Res {
    fn new() -> Self {
        let opaque = (0..16 * 12).flat_map(|i: u32| [(i * 7 % 256) as u8, (i * 13 % 256) as u8, (i * 29 % 256) as u8, 255]).collect();
        let translucent = (0..9 * 7).flat_map(|i: u32| [(i * 31 % 256) as u8, 200, (i * 3 % 256) as u8, (i * 37 % 256) as u8]).collect();
        Self { opaque, translucent }
    }
}
impl Resources for Res {
    fn icon(&mut self, icon: IconId, px: u32) -> Option<IconMask> {
        (icon.0 != 0).then(|| IconMask {
            width: px,
            height: px,
            rg: (0..px * px).map(|i| [(i.wrapping_mul(7) % 256) as u8, if icon.0 == 2 { (i.wrapping_mul(13) % 256) as u8 } else { 0 }]).collect(),
        })
    }
    fn image(&self, image: ImageId) -> Option<ImageRef<'_>> {
        match image.0 {
            1 => Some(ImageRef { width: 16, height: 12, rgba: &self.opaque }),
            2 => Some(ImageRef { width: 9, height: 7, rgba: &self.translucent }),
            3 => Some(ImageRef { width: 16, height: 12, rgba: &self.opaque[..40] }),
            _ => None,
        }
    }
}

fn c(r: u8, g: u8, b: u8, a: u8) -> Rgba8 {
    Rgba8::new(r, g, b, a)
}

/// One synthetic list per class, with its bound (max channel difference).
fn classes(text: &mut Text, grad: &mut Text) -> Vec<(&'static str, u8, DisplayList, bool)> {
    let mut out = Vec::new();
    let mut flat = DisplayList::default();
    flat.push(Prim::Rect { rect: Rect::xywh(3.3, 4.6, 50.2, 30.1), color: c(200, 40, 40, 255) });
    flat.push(Prim::Rect { rect: Rect::xywh(20.0, 10.0, 60.0, 40.0), color: c(40, 200, 90, 255) });
    for (i, side) in [Side::Left, Side::Top, Side::Right, Side::Bottom].into_iter().enumerate() {
        flat.push(Prim::Edge { owner: Rect::xywh(90.0, 10.0 + i as f32 * 20.0, 60.0, 15.0), side, px: 1 + (i as u8 % 2), color: c(240, 240, 240, 255) });
    }
    out.push(("rect_edge", 0, flat, false));
    let mut tr = DisplayList::default();
    tr.push(Prim::Rect { rect: Rect::xywh(10.0, 10.0, 80.0, 60.0), color: c(10, 120, 250, 128) });
    tr.push(Prim::Rect { rect: Rect::xywh(30.0, 30.0, 80.0, 60.0), color: c(250, 120, 10, 77) });
    out.push(("rect_translucent", 0, tr, false));
    let mut ch = DisplayList::default();
    ch.push(Prim::Checker { rect: Rect::xywh(5.3, 7.7, 120.0, 90.0), a: c(204, 204, 204, 255), b: c(255, 255, 255, 255), cell: 7.0 });
    out.push(("checker", 0, ch, false));
    let mut im = DisplayList::default();
    im.push(Prim::Image { rect: Rect::xywh(4.0, 4.0, 100.0, 77.0), image: ImageId(1) });
    im.push(Prim::Image { rect: Rect::xywh(110.0, 4.0, 3.0, 3.0), image: ImageId(3) });
    im.push(Prim::Image { rect: Rect::xywh(110.0, 4.0, 3.0, 3.0), image: ImageId(9) });
    out.push(("image_opaque", 0, im, false));
    let mut it = DisplayList::default();
    it.push(Prim::Image { rect: Rect::xywh(10.0, 10.0, 90.0, 70.0), image: ImageId(2) });
    out.push(("image_translucent", 0, it, false));
    let mut gl = DisplayList::default();
    for (i, s) in ["Layers  Channels  Paths", "Opacity 72 %", "Hamburgefonstiv 0123"].iter().enumerate() {
        let line = text.shape(s, TextToken::Body, 0).unwrap();
        gl.push(Prim::Glyphs { origin: Point::new(4.3 + i as f32 * 0.25, 20.0 + i as f32 * 18.0), token: TextToken::Body, glyphs: line.glyphs, color: c(230, 230, 230, 255) });
    }
    out.push(("glyph_box", 0, gl, false));
    let mut gg = DisplayList::default();
    let line = grad.shape("ABCDEFG", TextToken::Body, 0).unwrap();
    gg.push(Prim::Glyphs { origin: Point::new(6.6, 40.0), token: TextToken::Body, glyphs: line.glyphs.clone(), color: c(250, 200, 30, 220) });
    out.push(("glyph_gradient", 1, gg, true));
    let mut ic = DisplayList::default();
    ic.push(Prim::Icon { rect: Rect::xywh(10.0, 10.0, 20.0, 20.0), icon: IconId(1), form: c(220, 220, 220, 255), hot: c(255, 80, 80, 255) });
    ic.push(Prim::Icon { rect: Rect::xywh(40.0, 10.0, 16.0, 16.0), icon: IconId(2), form: c(220, 220, 220, 255), hot: c(80, 160, 255, 255) });
    ic.push(Prim::Icon { rect: Rect::xywh(70.0, 10.0, 16.0, 16.0), icon: IconId(0), form: c(220, 220, 220, 255), hot: c(80, 160, 255, 255) });
    out.push(("icon", 0, ic, false));
    let mut rr = DisplayList::default();
    rr.push(Prim::RRect { rect: Rect::xywh(5.0, 5.0, 60.0, 30.0), radius: 6.0, fill: c(60, 60, 70, 255), border: None });
    rr.push(Prim::RRect { rect: Rect::xywh(70.2, 5.7, 70.0, 40.0), radius: 8.0, fill: c(50, 90, 200, 255), border: Some(Border { width: 1.0, color: c(230, 230, 230, 255) }) });
    rr.push(Prim::RRect { rect: Rect::xywh(10.0, 50.0, 90.0, 50.0), radius: 20.0, fill: c(200, 60, 60, 180), border: Some(Border { width: 2.0, color: c(20, 20, 20, 200) }) });
    out.push(("rrect", 1, rr, false));
    let mut ln = DisplayList::default();
    ln.push(Prim::Line { a: Point::new(5.0, 5.0), b: Point::new(150.0, 100.0), width: 1.0, color: c(240, 240, 240, 255) });
    ln.push(Prim::Line { a: Point::new(5.0, 110.0), b: Point::new(150.0, 110.0), width: 3.0, color: c(240, 90, 90, 200) });
    ln.push(Prim::Line { a: Point::new(80.0, 10.0), b: Point::new(80.0, 10.0), width: 4.0, color: c(90, 240, 90, 255) });
    out.push(("line", 1, ln, false));
    let mut da = DisplayList::default();
    da.push(Prim::Dash { a: Point::new(5.0, 20.0), b: Point::new(150.0, 20.0), width: 1.0, dash: 4.0, phase: 0.0, on: c(255, 255, 255, 255), off: Some(c(0, 0, 0, 255)) });
    da.push(Prim::Dash { a: Point::new(5.0, 40.0), b: Point::new(140.0, 100.0), width: 2.0, dash: 3.0, phase: 1.5, on: c(255, 200, 0, 255), off: None });
    out.push(("dash", 1, da, false));
    let mut sh = DisplayList::default();
    sh.push(Prim::Shadow { rect: Rect::xywh(30.0, 30.0, 80.0, 50.0), radius: 6.0, offset_y: 4.0, blur: 12.0, color: c(0, 0, 0, 160) });
    sh.push(Prim::Rect { rect: Rect::xywh(30.0, 30.0, 80.0, 50.0), color: c(45, 45, 45, 255) });
    out.push(("shadow", 1, sh, false));
    let mut ra = DisplayList::default();
    ra.push(Prim::Ramp { rect: Rect::xywh(4.0, 4.0, 150.0, 100.0), corners: [c(255, 0, 0, 255), c(0, 255, 0, 255), c(0, 0, 255, 255), c(255, 255, 255, 128)] });
    out.push(("ramp", 1, ra, false));
    let mut sc = DisplayList::default();
    sc.push(Prim::PushScissor(Rect::xywh(10.5, 10.5, 100.0, 80.0)));
    sc.push(Prim::Rect { rect: Rect::xywh(0.0, 0.0, W, H), color: c(70, 70, 200, 255) });
    sc.push(Prim::PushScissor(Rect::xywh(50.0, 40.0, 100.0, 100.0)));
    sc.push(Prim::Rect { rect: Rect::xywh(0.0, 0.0, W, H), color: c(200, 200, 70, 255) });
    sc.push(Prim::PopScissor);
    sc.push(Prim::PushScissor(Rect::xywh(200.0, 200.0, 10.0, 10.0)));
    sc.push(Prim::Rect { rect: Rect::xywh(0.0, 0.0, W, H), color: c(255, 0, 255, 255) });
    sc.push(Prim::PopScissor);
    sc.push(Prim::PopScissor);
    sc.push(Prim::PopScissor);
    sc.push(Prim::Rect { rect: Rect::xywh(0.0, 100.0, W, 20.0), color: c(70, 200, 70, 255) });
    out.push(("scissor", 0, sc, false));
    out
}

fn cpu(w: u32, h: u32, lists: &[&DisplayList], scale: Scale, text: &mut Text, res: &mut dyn Resources) -> Vec<u8> {
    let mut canvas = Canvas::new(w, h, CLEAR).unwrap();
    for l in lists {
        paint(&mut canvas, l, scale, text, res);
    }
    canvas.rgba().to_vec()
}

#[allow(clippy::too_many_arguments)]
fn gpu(owner: &mut Owner, pass: &mut UiPass, w: u32, h: u32, lists: &[&DisplayList], scale: Scale, text: &mut Text, res: &mut dyn Resources) -> Vec<u8> {
    let pane = owner.create_pane(w, h).unwrap();
    pass.draw(owner, pane, UiFrame { clear: CLEAR, lists, scale, text, res }).expect("ui pass draw");
    let px = owner.read_pane(pane).expect("read back");
    owner.release_pane(pane).unwrap();
    px
}

#[test]
fn every_class_matches_the_cpu_painter_at_four_scales() {
    let mut owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device");
    assert!(owner.device_info().features.contains(&"dynamicRendering"), "{:?}", owner.device_info().features);
    let mut pass = UiPass::new(&owner).expect("ui pass");
    let mut text = Text::new(Box::new(BoxText));
    let mut grad = Text::new(Box::new(GradientText { cell: 9 }));
    grad.set_coverage_gamma(1.4);
    let mut res = Res::new();
    let mut report = Vec::new();
    for s in SCALES {
        let scale = Scale::new(s).unwrap();
        let (w, h) = (scale.edge(W) as u32, scale.edge(H) as u32);
        for (name, bound, list, uses_grad) in classes(&mut text, &mut grad) {
            let t = if uses_grad { &mut grad } else { &mut text };
            let want = cpu(w, h, &[&list], scale, t, &mut res);
            let got = gpu(&mut owner, &mut pass, w, h, &[&list], scale, t, &mut res);
            let (max, n) = diff(&want, &got);
            report.push(format!("{name}@{s}: max {max} differing {n}"));
            assert!(max <= bound, "{name} at scale {s}: max {max} > {bound} ({n} px)");
        }
    }
    println!("parity {}", report.join("; "));
    drop(pass);
    drop(owner);
    assert_validation_clean();
}

#[test]
fn gallery_scenes_and_the_specimen_match_the_cpu_painter() {
    let mut owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device");
    let mut pass = UiPass::new(&owner).expect("ui pass");
    let mut text = Text::new(Box::new(BoxText));
    let ui = Interaction::default();
    let mut report = Vec::new();
    for s in SCALES {
        let scale = Scale::new(s).unwrap();
        for scene in Scene::ALL {
            let (want, panel, _) = gallery::render(scene, scale, Theme::Dark, &ui, &mut text);
            let (w, h) = (want.width(), want.height());
            let pane = owner.create_pane(w, h).unwrap();
            let lists = [panel.display_list()];
            let frame = UiFrame { clear: ColorToken::SurfacePanel.resolve(Theme::Dark), lists: &lists, scale, text: &mut text, res: &mut GalleryIcons::default() };
            pass.draw(&mut owner, pane, frame).expect("draw");
            let got = owner.read_pane(pane).unwrap();
            owner.release_pane(pane).unwrap();
            let (max, n) = diff(want.rgba(), &got);
            report.push(format!("{}@{s}: max {max} differing {n}", scene.name()));
            assert_eq!(max, 0, "{} at {s}: {max} ({n} px)", scene.name());
        }
        let (want, panel, _) = gallery::render_specimen(scale, Theme::Dark, &mut text, 520.0, 260.0);
        let pane = owner.create_pane(want.width(), want.height()).unwrap();
        let lists = [panel.display_list()];
        let frame = UiFrame { clear: ColorToken::SurfacePanel.resolve(Theme::Dark), lists: &lists, scale, text: &mut text, res: &mut GalleryIcons::default() };
        pass.draw(&mut owner, pane, frame).expect("draw specimen");
        let got = owner.read_pane(pane).unwrap();
        owner.release_pane(pane).unwrap();
        let (max, n) = diff(want.rgba(), &got);
        report.push(format!("specimen@{s}: max {max} differing {n}"));
        assert_eq!(max, 0, "specimen at {s}: {max} ({n} px)");
    }
    println!("gallery parity {}", report.join("; "));
    drop(pass);
    drop(owner);
    assert_validation_clean();
}

#[test]
fn cold_warm_reset_and_growth_frames_draw_the_same_bytes() {
    let mut owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device");
    let mut text = Text::new(Box::new(GradientText { cell: 24 }));
    let mut list = DisplayList::default();
    for row in 0..12u32 {
        let s: String = (0..20u32).filter_map(|i| char::from_u32(0x4E00 + row * 20 + i)).collect();
        let line = text.shape(&s, TextToken::Body, 0).unwrap();
        list.push(Prim::Glyphs { origin: Point::new(2.0, 28.0 + row as f32 * 26.0), token: TextToken::Body, glyphs: line.glyphs, color: c(240, 240, 240, 255) });
    }
    let scale = Scale::ONE;
    let (w, h) = (400u32, 340u32);
    let mut res = Res::new();
    let want = cpu(w, h, &[&list], scale, &mut text, &mut res);

    let mut pass = UiPass::new(&owner).unwrap();
    let cold = gpu(&mut owner, &mut pass, w, h, &[&list], scale, &mut text, &mut res);
    let uploads = pass.stats().glyph_uploads;
    assert!(uploads > 0);
    let warm = gpu(&mut owner, &mut pass, w, h, &[&list], scale, &mut text, &mut res);
    assert_eq!(pass.stats().glyph_uploads, uploads, "a warm frame uploads nothing");
    assert_eq!(cold, warm);
    // A page of 128 texels holds 25 of these 24 px glyphs: 240 glyphs force pass breaks with resets.
    let mut small = UiPass::with_limits(&owner, UiLimits { glyph_min_side: 128, glyph_max_side: 128, ..UiLimits::default() }).unwrap();
    let broken = gpu(&mut owner, &mut small, w, h, &[&list], scale, &mut text, &mut res);
    assert!(small.stats().passes >= 9 && small.stats().glyph_resets >= 9, "{:?}", small.stats());
    assert_eq!(broken, cold, "pass breaks and resets change nothing");
    // A 128 page that may grow to 512: growth re-places the frame's set.
    let mut growing = UiPass::with_limits(&owner, UiLimits { glyph_min_side: 128, glyph_max_side: 512, ..UiLimits::default() }).unwrap();
    let grown = gpu(&mut owner, &mut growing, w, h, &[&list], scale, &mut text, &mut res);
    assert!(growing.stats().glyph_growths >= 2 && growing.stats().glyph_side == 512, "{:?}", growing.stats());
    assert_eq!(grown, cold, "growth changes nothing");
    let (max, n) = diff(&want, &cold);
    println!("cold/warm/reset/growth vs cpu: max {max} differing {n}");
    assert_eq!(max, 0);
    drop((pass, small, growing));
    drop(owner);
    assert_validation_clean();
}

/// Review F2: a frame refused with ui_pass_limit leaves no resident entry without its upload: the next frame on the
/// same pass draws the bytes of a cold draw.
#[test]
fn a_refused_frame_leaves_no_unuploaded_residents() {
    let mut owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device");
    let mut text = Text::new(Box::new(GradientText { cell: 12 }));
    let line = |text: &mut Text, n: u32| {
        let s: String = (0..n).filter_map(|i| char::from_u32(0x4E00 + i)).collect();
        let l = text.shape(&s, TextToken::Body, 0).unwrap();
        let mut d = DisplayList::default();
        d.push(Prim::Glyphs { origin: Point::new(2.0, 20.0), token: TextToken::Body, glyphs: l.glyphs, color: c(240, 240, 240, 255) });
        d
    };
    let flood = line(&mut text, 40);
    let small = line(&mut text, 8);
    let (w, h) = (480u32, 40u32);
    let mut res = Res::new();
    let limits = UiLimits { instance_cap_bytes: hsk_studio_vulkan::ui::INSTANCE_BYTES * 10, max_passes: 2, ..UiLimits::default() };
    let mut pass = UiPass::with_limits(&owner, limits).unwrap();
    let pane = owner.create_pane(w, h).unwrap();
    let refused = pass.draw(&mut owner, pane, UiFrame { clear: CLEAR, lists: &[&flood], scale: Scale::ONE, text: &mut text, res: &mut res });
    assert_eq!(refused.err(), Some(hsk_studio_vulkan::Refusal::UiPassLimit));
    owner.release_pane(pane).unwrap();
    let after = gpu(&mut owner, &mut pass, w, h, &[&small], Scale::ONE, &mut text, &mut res);
    let mut fresh = UiPass::new(&owner).unwrap();
    let cold = gpu(&mut owner, &mut fresh, w, h, &[&small], Scale::ONE, &mut text, &mut res);
    assert_eq!(diff(&cold, &after), (0, 0), "the frame after the refusal equals a cold draw");
    drop((pass, fresh));
    drop(owner);
    assert_validation_clean();
}

/// Review F3: uploads over the staging cap go out in several waited submits and still draw at parity.
#[test]
fn uploads_over_the_staging_cap_split_and_draw_at_parity() {
    let mut owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device");
    let mut list = DisplayList::default();
    list.push(Prim::Image { rect: Rect::xywh(4.0, 4.0, 64.0, 48.0), image: ImageId(1) });
    list.push(Prim::Image { rect: Rect::xywh(80.0, 4.0, 45.0, 35.0), image: ImageId(2) });
    let mut text = Text::new(Box::new(BoxText));
    let mut res = Res::new();
    let (w, h) = (160u32, 120u32);
    let want = cpu(w, h, &[&list], Scale::ONE, &mut text, &mut res);
    // 16x12x4 = 768 and 9x7x4 = 252 bytes: together over a 1000-byte cap.
    let mut pass = UiPass::with_limits(&owner, UiLimits { staging_cap_bytes: 1000, ..UiLimits::default() }).unwrap();
    let got = gpu(&mut owner, &mut pass, w, h, &[&list], Scale::ONE, &mut text, &mut res);
    assert!(pass.stats().upload_submits >= 1, "{:?}", pass.stats());
    assert_eq!(diff(&want, &got).0, 0);
    drop(pass);
    drop(owner);
    assert_validation_clean();
}
