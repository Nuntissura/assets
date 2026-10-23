//! Windowed effect mattes and layer composition (`effect_math::window`): bit identity with the
//! reference on every window split, bounded charged memory, hostile sizes and the large margin.
//! Every comparison is on `f32::to_bits`, not on a tolerance.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::effect_math::window::{
    AlphaPlaneRef, AlphaSource, WindowBudget, WindowError, WindowGeometry, WindowLimits,
    WindowPlane, layer_window_cost, render_layer_window, stack_support_px, window_cost,
    window_matte,
};
use hsk_studio_pigment::effect_math::{
    DropShadowOp, EffectError, EffectOp, EffectPlane, GlowFill, GlowSource, InnerGlowOp,
    InnerShadowOp, LayerParams, OuterGlowOp, StrokeOp, StrokePosition, effect_support_px,
    reference_matte, render_layer_reference,
};
use std::sync::atomic::{AtomicUsize, Ordering};

const BLACK: [f32; 3] = [0.0; 3];
const RED: [f32; 3] = [1.0, 0.0, 0.0];
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const LIMITS: WindowLimits = WindowLimits::new(256 << 20, 4_000_000_000);

fn rng(s: &mut u64) -> u32 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    (*s >> 32) as u32
}

/// Opaque block, soft disc, half-alpha dots and random-alpha dots (values around the 0.5 stroke
/// threshold included).
fn field(w: u32, h: u32, seed: u64) -> Vec<f32> {
    let mut s = seed | 1;
    let mut v = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let mut a = 0.0f32;
            if x >= w / 4 && x < w * 3 / 4 && y >= h / 4 && y < h * 3 / 4 {
                a = 1.0;
            }
            let d = ((x as f32 - w as f32 * 0.8).powi(2) + (y as f32 - h as f32 * 0.3).powi(2))
                .sqrt();
            if d < 6.0 {
                a = a.max(1.0 - d / 6.0);
            }
            let r = rng(&mut s);
            if r.is_multiple_of(17) {
                a = a.max((r >> 8) as f32 / 16_777_216.0);
            }
            if r % 29 == 1 {
                a = 0.5;
            }
            v.push(a);
        }
    }
    v
}

fn coloured(w: u32, h: u32, alpha: &[f32]) -> EffectPlane {
    let mut rgb = Vec::new();
    for y in 0..h {
        for x in 0..w {
            rgb.push([x as f32 / w as f32, y as f32 / h as f32, 0.25]);
        }
    }
    EffectPlane::new(w, h, rgb, alpha.to_vec()).unwrap()
}

fn tiles(r: Rect, tw: u32, th: u32) -> Vec<Rect> {
    let mut out = Vec::new();
    let mut y = 0;
    while y < r.height {
        let h = th.min(r.height - y);
        let mut x = 0;
        while x < r.width {
            let w = tw.min(r.width - x);
            out.push(Rect {
                x: r.x + i64::from(x),
                y: r.y + i64::from(y),
                width: w,
                height: h,
            });
            x += w;
        }
        y += h;
    }
    out
}

fn all_ops() -> Vec<(&'static str, EffectOp)> {
    vec![
        (
            "drop_shadow spread blur offset knockout",
            EffectOp::DropShadow(
                DropShadowOp::new(BLACK, NORMAL, 0.75)
                    .with_size(6.0)
                    .with_spread(0.3)
                    .with_offset(30.0, 4.0)
                    .with_layer_knocks_out(true),
            ),
        ),
        (
            "drop_shadow hard grown",
            EffectOp::DropShadow(
                DropShadowOp::new(BLACK, NORMAL, 0.5)
                    .with_size(5.0)
                    .with_spread(1.0)
                    .with_offset(-120.0, 3.0),
            ),
        ),
        (
            "drop_shadow plain fractional offset",
            EffectOp::DropShadow(DropShadowOp::new(BLACK, NORMAL, 1.0).with_offset(180.0, 2.5)),
        ),
        (
            "colour_overlay",
            EffectOp::ColourOverlay(hsk_studio_pigment::effect_math::ColourOverlayOp::new(
                RED, NORMAL, 0.5,
            )),
        ),
        (
            "stroke outside",
            EffectOp::Stroke(StrokeOp::new(RED, NORMAL, 1.0, 2.0, StrokePosition::Outside)),
        ),
        (
            "stroke inside",
            EffectOp::Stroke(StrokeOp::new(RED, NORMAL, 1.0, 3.5, StrokePosition::Inside)),
        ),
        (
            "stroke centre",
            EffectOp::Stroke(StrokeOp::new(RED, NORMAL, 1.0, 5.0, StrokePosition::Centre)),
        ),
        (
            "stroke zero size",
            EffectOp::Stroke(StrokeOp::new(RED, NORMAL, 1.0, 0.0, StrokePosition::Centre)),
        ),
        (
            "inner_shadow choke blur offset",
            EffectOp::InnerShadow(
                InnerShadowOp::new(BLACK, NORMAL, 0.6)
                    .with_size(4.0)
                    .with_choke(0.5)
                    .with_offset(45.0, 3.0),
            ),
        ),
        (
            "inner_shadow hard ring",
            EffectOp::InnerShadow(
                InnerShadowOp::new(BLACK, NORMAL, 1.0)
                    .with_size(2.0)
                    .with_choke(1.0),
            ),
        ),
        (
            "outer_glow spread blur",
            EffectOp::OuterGlow(
                OuterGlowOp::new(RED, NORMAL, 0.9)
                    .with_size(6.0)
                    .with_spread(0.0),
            ),
        ),
        (
            "outer_glow grown",
            EffectOp::OuterGlow(
                OuterGlowOp::new(RED, NORMAL, 0.9)
                    .with_size(4.0)
                    .with_spread(0.4),
            ),
        ),
        (
            "inner_glow edge",
            EffectOp::InnerGlow(
                InnerGlowOp::new(RED, NORMAL, 0.8)
                    .with_size(5.0)
                    .with_choke(0.4),
            ),
        ),
        (
            "inner_glow centre",
            EffectOp::InnerGlow(
                InnerGlowOp::new(RED, NORMAL, 0.8)
                    .with_size(5.0)
                    .with_source(GlowSource::Centre),
            ),
        ),
    ]
}

fn assert_bits(name: &str, win: Rect, got: &[f32], reference: &[f32], prect: Rect) {
    assert_eq!(got.len(), (win.width * win.height) as usize, "{name} {win:?}");
    for row in 0..win.height {
        for col in 0..win.width {
            let (x, y) = (
                (win.x - prect.x) as usize + col as usize,
                (win.y - prect.y) as usize + row as usize,
            );
            let want = reference[y * prect.width as usize + x];
            let have = got[(row * win.width + col) as usize];
            assert_eq!(
                have.to_bits(),
                want.to_bits(),
                "{name}: window {win:?} pixel ({col},{row}) windowed {have} reference {want}"
            );
        }
    }
}

/// The reference plane edge truncates every intermediate; with `domain == layer == plane` the
/// windowed result reproduces it bit for bit for every kind and every split (ragged tiles, one
/// window, thin strips), at coordinates far from the origin.
#[test]
fn windowed_matte_is_bit_identical_to_the_reference_for_every_kind_and_split() {
    let (pw, ph) = (41u32, 33u32);
    let alpha = field(pw, ph, 0x1234_5678);
    let plane = coloured(pw, ph, &alpha);
    let prect = Rect {
        x: -17,
        y: 1000,
        width: pw,
        height: ph,
    };
    let src = AlphaPlaneRef::new(prect, &alpha).unwrap();
    for (name, op) in all_ops() {
        let reference = reference_matte(&plane, &op).unwrap();
        for (tw, th) in [(7, 5), (16, 16), (41, 33), (1, 9)] {
            for win in tiles(prect, tw, th) {
                let g = WindowGeometry::plane(prect, win);
                let m = window_matte(&src, &g, &op, LIMITS).unwrap();
                assert_bits(name, win, &m.coverage, &reference, prect);
            }
        }
    }
}

/// A layer smaller than the reference plane (window straddling the layer edge): the accessor only
/// covers the layer, the domain is the padded plane (equal to the reference on that plane) or any
/// larger rectangle (uncut: the result does not depend on the domain once it holds the layer grown
/// by the effect support).
#[test]
fn padded_domain_equals_the_reference_and_ignores_extra_domain() {
    let (lw, lh) = (20u32, 15u32);
    let mut layer_alpha = field(lw, lh, 0x9e37_79b9);
    for a in layer_alpha.iter_mut().take(lw as usize) {
        *a = 1.0;
    }
    for y in 0..lh as usize {
        layer_alpha[y * lw as usize] = 0.75;
    }
    let (pw, ph, ox, oy) = (60u32, 55u32, 20u32, 20u32);
    let mut padded = vec![0.0f32; (pw * ph) as usize];
    for y in 0..lh {
        for x in 0..lw {
            padded[((y + oy) * pw + x + ox) as usize] = layer_alpha[(y * lw + x) as usize];
        }
    }
    let plane = coloured(pw, ph, &padded);
    let prect = Rect {
        x: 300,
        y: -40,
        width: pw,
        height: ph,
    };
    let layer = Rect {
        x: prect.x + i64::from(ox),
        y: prect.y + i64::from(oy),
        width: lw,
        height: lh,
    };
    let src = AlphaPlaneRef::new(layer, &layer_alpha).unwrap();
    let wide = Rect {
        x: prect.x - 13,
        y: prect.y - 13,
        width: pw + 26,
        height: ph + 26,
    };
    for (name, op) in all_ops() {
        assert!(effect_support_px(&op).unwrap() <= 19, "{name}");
        let reference = reference_matte(&plane, &op).unwrap();
        for win in tiles(prect, 9, 7) {
            for domain in [prect, wide] {
                let g = WindowGeometry::new(layer, domain, win);
                let m = window_matte(&src, &g, &op, LIMITS).unwrap();
                assert_bits(name, win, &m.coverage, &reference, prect);
            }
        }
    }
}

fn sub<T: Copy>(all: &[T], prect: Rect, win: Rect) -> Vec<T> {
    let mut out = Vec::new();
    for row in 0..win.height {
        for col in 0..win.width {
            let x = (win.x - prect.x) as usize + col as usize;
            let y = (win.y - prect.y) as usize + row as usize;
            out.push(all[y * prect.width as usize + x]);
        }
    }
    out
}

/// The composed contribution of a mixed stack (every kind, several blend modes, translucent backdrop,
/// layer opacity and fill below 1, dissolve noise) equals `render_layer_reference` bit for bit.
#[test]
fn composed_window_is_bit_identical_to_render_layer_reference() {
    let (pw, ph) = (45u32, 38u32);
    let alpha = field(pw, ph, 0xabcd_ef01);
    let content = coloured(pw, ph, &alpha);
    let mut back_rgb = Vec::new();
    let mut back_alpha = Vec::new();
    let mut s = 77u64;
    for y in 0..ph {
        for x in 0..pw {
            back_rgb.push([0.1 + 0.8 * (x as f32 / pw as f32), 0.5, 0.9 - 0.5 * (y as f32 / ph as f32)]);
            back_alpha.push(match rng(&mut s) % 4 {
                0 => 0.0,
                1 => 0.5,
                _ => 1.0,
            });
        }
    }
    let backdrop = EffectPlane::new(pw, ph, back_rgb.clone(), back_alpha.clone()).unwrap();
    let layer = LayerParams {
        mode: StudioBlendMode::Normal,
        opacity: 0.8,
        fill_opacity: 0.6,
    };
    let effects = [
        EffectOp::DropShadow(
            DropShadowOp::new([0.2, 0.2, 0.6], StudioBlendMode::Multiply, 0.7)
                .with_size(5.0)
                .with_spread(0.4)
                .with_offset(135.0, 4.0)
                .with_layer_knocks_out(true),
        ),
        EffectOp::OuterGlow(
            OuterGlowOp::new([1.0, 0.9, 0.2], StudioBlendMode::Screen, 0.6)
                .with_size(5.0)
                .with_spread(0.2),
        ),
        EffectOp::ColourOverlay(hsk_studio_pigment::effect_math::ColourOverlayOp::new(
            [0.9, 0.1, 0.1],
            StudioBlendMode::Dissolve,
            0.5,
        )),
        EffectOp::Stroke(StrokeOp::new(
            [0.0, 0.5, 0.0],
            NORMAL,
            0.9,
            3.0,
            StrokePosition::Outside,
        )),
        EffectOp::InnerShadow(
            InnerShadowOp::new(BLACK, StudioBlendMode::Multiply, 0.6)
                .with_size(4.0)
                .with_choke(0.25)
                .with_offset(60.0, 2.0),
        ),
        EffectOp::InnerGlow(
            InnerGlowOp::new([1.0; 3], StudioBlendMode::Screen, 0.5)
                .with_size(4.0)
                .with_source(GlowSource::Centre),
        ),
    ];
    let reference = render_layer_reference(&backdrop, &content, layer, &effects).unwrap();
    let prect = Rect {
        x: -5,
        y: 7,
        width: pw,
        height: ph,
    };
    let src = AlphaPlaneRef::new(prect, &alpha).unwrap();
    for (tw, th) in [(8, 6), (45, 38), (3, 38)] {
        for win in tiles(prect, tw, th) {
            let g = WindowGeometry::plane(prect, win);
            let back = WindowPlane::new(
                win,
                sub(&back_rgb, prect, win),
                sub(&back_alpha, prect, win),
            )
            .unwrap();
            let content_rgb = sub(content.rgb(), prect, win);
            let out =
                render_layer_window(&src, &g, &content_rgb, &back, layer, &effects, LIMITS).unwrap();
            let predicted = layer_window_cost(&effects, win).unwrap();
            assert_eq!(out.receipt.peak_bytes, predicted.peak_bytes, "{win:?}");
            assert_eq!(out.receipt.work, predicted.work, "{win:?}");
            for row in 0..win.height {
                for col in 0..win.width {
                    let (x, y) = (
                        (win.x - prect.x) as u32 + col,
                        (win.y - prect.y) as u32 + row,
                    );
                    let want = reference.pixel(x, y).unwrap();
                    let i = (row * win.width + col) as usize;
                    assert_eq!(
                        out.plane.alpha()[i].to_bits(),
                        want.a.to_bits(),
                        "alpha {win:?} ({col},{row})"
                    );
                    for c in 0..3 {
                        assert_eq!(
                            out.plane.rgb()[i][c].to_bits(),
                            want.rgb[c].to_bits(),
                            "rgb[{c}] {win:?} ({col},{row})"
                        );
                    }
                }
            }
        }
    }
    // Support margin of the whole stack, for a shared uncut domain.
    assert!(stack_support_px(&effects).unwrap() >= 4);
    assert_eq!(stack_support_px(&[]).unwrap(), 0);
}

/// Periodic dots addressed relative to the layer origin: the same pixels at any coordinates.
fn dots(layer_x: i64, layer_y: i64) -> impl Fn(Rect, &mut [f32]) -> Result<(), WindowError> {
    move |rect, out| {
        let w = rect.width as usize;
        for row in 0..rect.height as usize {
            for col in 0..w {
                let px = (rect.x + col as i64 - layer_x).rem_euclid(64);
                let py = (rect.y + row as i64 - layer_y).rem_euclid(64);
                let d = (((px - 20) * (px - 20) + (py - 24) * (py - 24)) as f32).sqrt();
                out[row * w + col] = if d < 7.0 {
                    1.0
                } else if d < 9.0 {
                    (9.0 - d) / 2.0
                } else {
                    0.0
                };
            }
        }
        Ok(())
    }
}

/// An arbitrarily large layer at huge coordinates is evaluated for a small window in bounded memory,
/// and the result is the same as for a small layer with the same edge (translation invariance at the
/// near and far edge).
#[test]
fn huge_layers_and_far_coordinates_are_bounded_and_translation_invariant() {
    let op = EffectOp::DropShadow(
        DropShadowOp::new(BLACK, NORMAL, 1.0)
            .with_size(12.0)
            .with_spread(0.5)
            .with_offset(135.0, 8.0),
    );
    let support = effect_support_px(&op).unwrap();
    // offset ceil(8) + grow reach of the 6 px spread (floor(7 / w1) = 7, lane
    // SMB-PS-ORACLE-PARITY-2: Photoshop's grow reaches one pixel beyond the spread) + tent support
    // of the 6 px blur (5): 20 (SMB-PS-ORACLE-PARITY-1 had 19 with the disc; Gaussian 23)
    assert_eq!(support, 20);
    let limits = WindowLimits::new(2 << 20, 200_000_000);
    let run = |layer: Rect, win_rel: Rect| {
        let win = Rect {
            x: layer.x + win_rel.x,
            y: layer.y + win_rel.y,
            width: win_rel.width,
            height: win_rel.height,
        };
        let g = WindowGeometry::uncut(layer, support, win).unwrap();
        window_matte(&dots(layer.x, layer.y), &g, &op, limits).unwrap()
    };
    // Near edge: window straddling the layer's top-left corner, tall enough to reach the shadow
    // of the first dot with the effect tent kernel (SMB-PS-ORACLE-PARITY-1: the Gaussian reached
    // farther, so a 16-row window was enough before).
    let rel = Rect {
        x: -8,
        y: -5,
        width: 24,
        height: 28,
    };
    let small = Rect {
        x: 0,
        y: 0,
        width: 128,
        height: 128,
    };
    let big = Rect {
        x: -2_000_000_000,
        y: 3_000_000_000,
        width: 4_000_000_000,
        height: 4_000_000_000,
    };
    let million = Rect {
        x: 9_000_000_000,
        y: -9_000_000_000,
        width: 1_000_000,
        height: 1_000_000,
    };
    let a = run(small, rel);
    assert!(a.coverage.iter().any(|v| *v > 0.0), "test must not be vacuous");
    for layer in [big, million] {
        let b = run(layer, rel);
        assert_eq!(
            a.coverage.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b.coverage.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
        assert!(b.receipt.peak_bytes <= limits.max_bytes);
    }
    // Far edge: the window straddles the right/bottom edge, 3 px past the last disc. Layers whose
    // size is 32 modulo the pattern period (96, 1_000_032) have the same pixels there relative to
    // their end.
    let small_far = Rect {
        x: 0,
        y: 0,
        width: 96,
        height: 96,
    };
    let million_far = Rect {
        x: 9_000_000_000,
        y: -9_000_000_000,
        width: 1_000_032,
        height: 1_000_032,
    };
    let far = Rect {
        x: 96 - 16,
        y: 96 - 10,
        width: 24,
        height: 16,
    };
    let a = run(small_far, far);
    assert!(a.coverage.iter().any(|v| *v > 0.0), "far edge must not be vacuous");
    let far_big = Rect {
        x: 1_000_032 - 16,
        y: 1_000_032 - 10,
        width: 24,
        height: 16,
    };
    let b = run(million_far, far_big);
    assert_eq!(
        a.coverage.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        b.coverage.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
}

/// The accessor is never asked for samples beyond the window grown by `effect_support_px` (nor
/// beyond the layer), and altering the alpha outside that margin does not change the matte.
#[test]
fn source_reads_stay_inside_the_support_margin() {
    let (lw, lh) = (90u32, 80u32);
    let alpha = field(lw, lh, 0x5151_5151);
    let layer = Rect {
        x: 0,
        y: 0,
        width: lw,
        height: lh,
    };
    let win = Rect {
        x: 40,
        y: 30,
        width: 5,
        height: 4,
    };
    for (name, op) in all_ops() {
        let support = effect_support_px(&op).unwrap();
        let reads = std::cell::RefCell::new(Vec::<Rect>::new());
        let plane = AlphaPlaneRef::new(layer, &alpha).unwrap();
        let recording = |rect: Rect, out: &mut [f32]| {
            reads.borrow_mut().push(rect);
            plane.read_alpha(rect, out)
        };
        let g = WindowGeometry::uncut(layer, support, win).unwrap();
        let first = window_matte(&recording, &g, &op, LIMITS).unwrap();
        let margin = Rect {
            x: win.x - i64::from(support),
            y: win.y - i64::from(support),
            width: win.width + 2 * support,
            height: win.height + 2 * support,
        };
        for r in reads.borrow().iter() {
            assert_eq!(r.height, 1, "{name}");
            assert_eq!(Some(*r), r.intersect(margin).unwrap(), "{name}: {r:?} outside {margin:?}");
            assert_eq!(Some(*r), r.intersect(layer).unwrap(), "{name}: {r:?} outside the layer");
        }
        // Scramble everything outside the margin.
        let mut other = alpha.clone();
        let mut s = 99u64;
        for y in 0..lh as i64 {
            for x in 0..lw as i64 {
                let inside = x >= margin.x
                    && x < margin.x + i64::from(margin.width)
                    && y >= margin.y
                    && y < margin.y + i64::from(margin.height);
                if !inside {
                    other[(y * i64::from(lw) + x) as usize] = (rng(&mut s) % 101) as f32 / 100.0;
                }
            }
        }
        let plane2 = AlphaPlaneRef::new(layer, &other).unwrap();
        let second = window_matte(&plane2, &g, &op, LIMITS).unwrap();
        assert_eq!(
            first.coverage.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            second.coverage.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "{name}"
        );
    }
}

/// Predicted cost equals the measured receipt; a limit one below the prediction is refused before
/// the accessor is called; the exact prediction is admitted.
#[test]
fn budgets_are_charged_before_allocation_and_match_the_receipt() {
    let (pw, ph) = (30u32, 26u32);
    let alpha = field(pw, ph, 0x2468_ace0);
    let prect = Rect {
        x: 0,
        y: 0,
        width: pw,
        height: ph,
    };
    let plane = AlphaPlaneRef::new(prect, &alpha).unwrap();
    let calls = AtomicUsize::new(0);
    let counting = |rect: Rect, out: &mut [f32]| {
        calls.fetch_add(1, Ordering::SeqCst);
        plane.read_alpha(rect, out)
    };
    for (name, op) in all_ops() {
        for win in [
            Rect {
                x: 3,
                y: 2,
                width: 11,
                height: 9,
            },
            Rect {
                x: 0,
                y: 0,
                width: 30,
                height: 26,
            },
            Rect {
                x: 29,
                y: 25,
                width: 1,
                height: 1,
            },
        ] {
            let g = WindowGeometry::plane(prect, win);
            let cost = window_cost(&op, win).unwrap();
            calls.store(0, Ordering::SeqCst);
            let exact =
                window_matte(&counting, &g, &op, WindowLimits::new(cost.peak_bytes, cost.work))
                    .unwrap();
            assert_eq!(exact.receipt.peak_bytes, cost.peak_bytes, "{name} {win:?}");
            assert_eq!(exact.receipt.work, cost.work, "{name} {win:?}");
            // A zero-size stroke has nothing to read; every other kind reads the alpha.
            assert_eq!(
                calls.load(Ordering::SeqCst) > 0,
                !name.contains("zero size"),
                "{name}"
            );

            calls.store(0, Ordering::SeqCst);
            let low = WindowLimits::new(cost.peak_bytes - 1, cost.work);
            assert_eq!(
                window_matte(&counting, &g, &op, low).unwrap_err(),
                WindowError::OverBudget {
                    budget: WindowBudget::Bytes,
                    needed: cost.peak_bytes,
                    limit: cost.peak_bytes - 1,
                },
                "{name} {win:?}"
            );
            let low = WindowLimits::new(cost.peak_bytes, cost.work - 1);
            assert_eq!(
                window_matte(&counting, &g, &op, low).unwrap_err(),
                WindowError::OverBudget {
                    budget: WindowBudget::Work,
                    needed: cost.work,
                    limit: cost.work - 1,
                },
                "{name} {win:?}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{name}: refused after reading");
        }
    }
}

/// Hostile sizes and domains: `u32::MAX` windows, windows beyond the side limit, windows outside
/// the domain, coordinate overflow, an aggregate over budget, bad samples and a refusing accessor.
#[test]
fn hostile_sizes_samples_and_stacks_are_typed_refusals() {
    let layer = Rect {
        x: 0,
        y: 0,
        width: 8,
        height: 8,
    };
    let alpha = vec![1.0f32; 64];
    let plane = AlphaPlaneRef::new(layer, &alpha).unwrap();
    let op = EffectOp::DropShadow(DropShadowOp::new(BLACK, NORMAL, 1.0).with_size(2.0));
    let win = |w: u32, h: u32| Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    // dims_u32_max style
    for (w, h) in [(u32::MAX, 1), (1, u32::MAX), (u32::MAX, u32::MAX), (65_537, 1), (0, 4), (4, 0)]
    {
        let g = WindowGeometry::plane(layer, win(w, h));
        assert_eq!(
            window_matte(&plane, &g, &op, LIMITS).unwrap_err(),
            WindowError::WindowShape,
            "{w}x{h}"
        );
        assert_eq!(window_cost(&op, win(w, h)).unwrap_err(), WindowError::WindowShape);
    }
    // The largest allowed side is refused by the byte budget, never allocated.
    let g = WindowGeometry::new(layer, win(65_536, 65_536), win(65_536, 65_536));
    assert!(matches!(
        window_matte(&plane, &g, &op, LIMITS).unwrap_err(),
        WindowError::OverBudget {
            budget: WindowBudget::Bytes,
            ..
        }
    ));
    // Window outside the domain.
    let g = WindowGeometry::plane(layer, Rect { x: 4, y: 4, width: 8, height: 8 });
    assert_eq!(
        window_matte(&plane, &g, &op, LIMITS).unwrap_err(),
        WindowError::OutsideDomain
    );
    // Coordinates that overflow.
    let edge = Rect {
        x: i64::MAX - 5,
        y: 0,
        width: 100,
        height: 4,
    };
    let g = WindowGeometry::plane(edge, win(1, 1));
    assert_eq!(
        window_matte(&plane, &g, &op, LIMITS).unwrap_err(),
        WindowError::Overflow
    );
    assert_eq!(
        WindowGeometry::uncut(
            Rect { x: i64::MIN + 1, y: 0, width: 4, height: 4 },
            10,
            win(1, 1)
        )
        .unwrap_err(),
        WindowError::Overflow
    );
    // Aggregate over budget: each shadow fits, 64 of them in a big window exceed the work budget of
    // the stack, refused before reading.
    let big = Rect {
        x: 0,
        y: 0,
        width: 512,
        height: 512,
    };
    let many: Vec<EffectOp> = (0..64).map(|_| op).collect();
    let cost = layer_window_cost(&many, big).unwrap();
    let one = window_cost(&op, big).unwrap();
    assert!(cost.work > one.work * 63);
    let calls = AtomicUsize::new(0);
    let counting = |rect: Rect, out: &mut [f32]| {
        calls.fetch_add(1, Ordering::SeqCst);
        plane.read_alpha(rect, out)
    };
    let backdrop = WindowPlane::new(
        big,
        vec![[0.0; 3]; 512 * 512],
        vec![0.0; 512 * 512],
    )
    .unwrap();
    let content = vec![[0.0; 3]; 512 * 512];
    let g = WindowGeometry::new(layer, big, big);
    let tight = WindowLimits::new(cost.peak_bytes, cost.work - 1);
    assert!(matches!(
        render_layer_window(&counting, &g, &content, &backdrop, LayerParams::plain(), &many, tight)
            .unwrap_err(),
        WindowError::OverBudget {
            budget: WindowBudget::Work,
            ..
        }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let too_many: Vec<EffectOp> = (0..65).map(|_| op).collect();
    assert_eq!(
        layer_window_cost(&too_many, win(2, 2)).unwrap_err(),
        WindowError::TooManyEffects
    );
    // Effect parameter errors pass through unchanged.
    let huge = EffectOp::DropShadow(DropShadowOp::new(BLACK, NORMAL, 1.0).with_size(1001.0));
    let g = WindowGeometry::plane(layer, win(2, 2));
    assert_eq!(
        window_matte(&plane, &g, &huge, LIMITS).unwrap_err(),
        WindowError::Effect(EffectError::SizeOutOfRange)
    );
    let gradient = EffectOp::OuterGlow(
        OuterGlowOp::new(RED, NORMAL, 1.0).with_fill(GlowFill::Gradient),
    );
    assert_eq!(
        window_matte(&plane, &g, &gradient, LIMITS).unwrap_err(),
        WindowError::Effect(EffectError::Unsupported(
            hsk_studio_pigment::effect_math::EffectFeature::GlowGradient
        ))
    );
    // Samples outside [0, 1] and a refusing accessor.
    for bad in [1.5f32, f32::NAN, -0.25] {
        let bad_source = |_: Rect, out: &mut [f32]| -> Result<(), WindowError> {
            out.fill(bad);
            Ok(())
        };
        assert_eq!(
            window_matte(&bad_source, &g, &op, LIMITS).unwrap_err(),
            WindowError::Effect(EffectError::OutOfDomain)
        );
    }
    let refusing = |_: Rect, _: &mut [f32]| -> Result<(), WindowError> { Err(WindowError::Source) };
    assert_eq!(
        window_matte(&refusing, &g, &op, LIMITS).unwrap_err(),
        WindowError::Source
    );
}

/// The margin the 64 x 64 reference could not serve: a 1000 px blur (the effect tent kernel,
/// nonzero taps within 999 px; cross-lane re-pin by SMB-PS-ORACLE-PARITY-1, the kernel rule measured
/// on the D5 oracle at sizes 2/10/40, size 1000 by the same rule, UNVERIFIED) for a
/// one-pixel window, in a bounded 64 MiB, against hand-derived taps (the matte of a single opaque
/// pixel is the product of two kernel taps, each rounded to f32 after its pass).
#[test]
fn a_one_thousand_pixel_blur_is_evaluated_for_a_one_pixel_window_in_bounded_memory() {
    let op = EffectOp::DropShadow(DropShadowOp::new(BLACK, NORMAL, 1.0).with_size(1000.0));
    let kernel = hsk_studio_pigment::effect_math::effect_blur_kernel(1000.0).unwrap();
    let kb = (kernel.len() / 2) as i64;
    assert_eq!(kb, 999);
    assert_eq!(effect_support_px(&op).unwrap(), 999);
    let layer = Rect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let alpha = [1.0f32];
    let src = AlphaPlaneRef::new(layer, &alpha).unwrap();
    let limits = WindowLimits::new(64 << 20, 200_000_000);
    for (px, py) in [(0i64, 0i64), (700, -300), (-999, 999), (999, -999)] {
        let win = Rect {
            x: px,
            y: py,
            width: 1,
            height: 1,
        };
        let g = WindowGeometry::uncut(layer, 999, win).unwrap();
        let m = window_matte(&src, &g, &op, limits).unwrap();
        let tap = |d: i64| kernel[(kb - d) as usize];
        let want = (f64::from(tap(py)) * f64::from(tap(px))) as f32;
        assert!(want > 0.0);
        assert_eq!(m.coverage[0].to_bits(), want.to_bits(), "({px},{py})");
        assert!(m.receipt.peak_bytes <= limits.max_bytes);
    }
}
