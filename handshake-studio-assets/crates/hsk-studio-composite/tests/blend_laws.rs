//! Break-it sweep: algebraic laws that must hold for every layer-settable mode over a lattice of
//! backdrop/source colours (5 values per channel, 125 x 125 pairs). Laws come from the W3C
//! definitions (e.g. Overlay(cb, cs) == HardLight(cs, cb)) and from identity elements; they catch
//! NaN/Inf, range escapes and asymmetric implementations without any expected-value table.
use hsk_studio_composite::blend::{MODE_TABLE, lum};
use hsk_studio_composite::{Applicability, BlendMode, PixelSite, Rgba, blend_rgb, composite_over};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(body));
        let _ = sender.send(outcome);
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared 30-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost deadline worker result")
        }
    }
}

const G: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

fn lattice() -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(125);
    for r in G {
        for g in G {
            for b in G {
                out.push([r, g, b]);
            }
        }
    }
    out
}

fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= eps)
}

#[test]
fn blend_laws_sweep() {
    bounded_case("blend_laws_sweep", || {
        let colours = lattice();
        let site = PixelSite { x: 1, y: 2, seed: 3 };
        let layer_modes: Vec<BlendMode> = MODE_TABLE
            .iter()
            .filter(|r| r.applicability == Applicability::Layer)
            .map(|r| r.mode)
            .collect();
        // 35 members minus pass_through (group-only), behind + clear (tool-only), five unrecovered.
        assert_eq!(layer_modes.len(), 27);
        let symmetric = [
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::LinearDodgeAdd,
        ];
        // (mode, source value that leaves the backdrop unchanged)
        let identities = [
            (BlendMode::Multiply, 1.0),
            (BlendMode::Screen, 0.0),
            (BlendMode::Darken, 1.0),
            (BlendMode::Lighten, 0.0),
            (BlendMode::Difference, 0.0),
            (BlendMode::Exclusion, 0.0),
            (BlendMode::LinearDodgeAdd, 0.0),
            (BlendMode::LinearBurn, 1.0),
            (BlendMode::Subtract, 0.0),
            (BlendMode::Divide, 1.0),
            (BlendMode::Overlay, 0.5),
            (BlendMode::HardLight, 0.5),
            (BlendMode::SoftLight, 0.5),
            (BlendMode::ColourDodge, 0.0),
            (BlendMode::ColourBurn, 1.0),
            (BlendMode::LinearLight, 0.5),
            (BlendMode::VividLight, 0.5),
        ];
        for cb in &colours {
            for cs in &colours {
                for mode in &layer_modes {
                    let out = blend_rgb(*mode, *cb, *cs).unwrap();
                    assert!(
                        out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                        "{} {cb:?} {cs:?} -> {out:?}",
                        mode.key()
                    );
                    // Source with zero alpha never changes an opaque backdrop (no silent effect).
                    let none = composite_over(*mode, Rgba::new(*cb, 1.0), Rgba::new(*cs, 0.0), 1.0, site)
                        .unwrap();
                    assert!(close(none.rgb, *cb, 1e-6) && none.a == 1.0, "{} alpha0", mode.key());
                    // Over a transparent backdrop the blend is ignored: the source shows through.
                    let over = composite_over(*mode, Rgba::TRANSPARENT, Rgba::new(*cs, 1.0), 1.0, site)
                        .unwrap();
                    assert!(close(over.rgb, *cs, 1e-6) && over.a == 1.0, "{} transparent", mode.key());
                }
                // Normal with an opaque source replaces the backdrop.
                let normal = composite_over(BlendMode::Normal, Rgba::new(*cb, 1.0), Rgba::new(*cs, 1.0), 1.0, site)
                    .unwrap();
                assert!(close(normal.rgb, *cs, 1e-6));
                for mode in symmetric {
                    assert!(
                        close(blend_rgb(mode, *cb, *cs).unwrap(), blend_rgb(mode, *cs, *cb).unwrap(), 1e-6),
                        "{} not symmetric at {cb:?} {cs:?}",
                        mode.key()
                    );
                }
                // W3C definition: Overlay(Cb, Cs) = HardLight(Cs, Cb).
                assert!(close(
                    blend_rgb(BlendMode::Overlay, *cb, *cs).unwrap(),
                    blend_rgb(BlendMode::HardLight, *cs, *cb).unwrap(),
                    1e-6
                ));
                // Non-separable modes preserve the luminance their definition promises.
                for (mode, from) in [
                    (BlendMode::Hue, cb),
                    (BlendMode::Saturation, cb),
                    (BlendMode::Colour, cb),
                    (BlendMode::Luminosity, cs),
                ] {
                    let out = blend_rgb(mode, *cb, *cs).unwrap();
                    assert!(
                        (lum(out) - lum(*from)).abs() <= 2e-5,
                        "{} luminance at {cb:?} {cs:?}",
                        mode.key()
                    );
                }
                // Darker/lighter colour pick one whole input pixel.
                for mode in [BlendMode::DarkerColour, BlendMode::LighterColour] {
                    let out = blend_rgb(mode, *cb, *cs).unwrap();
                    assert!(out == *cb || out == *cs);
                }
            }
            for (mode, value) in identities {
                let out = blend_rgb(mode, *cb, [value; 3]).unwrap();
                assert!(close(out, *cb, 1e-6), "{} identity {value} at {cb:?} -> {out:?}", mode.key());
            }
            // Colour / Hue / Saturation / Luminosity with the backdrop as source return the backdrop.
            for mode in [BlendMode::Hue, BlendMode::Saturation, BlendMode::Colour, BlendMode::Luminosity] {
                let out = blend_rgb(mode, *cb, *cb).unwrap();
                assert!(close(out, *cb, 2e-5), "{} idempotent at {cb:?} -> {out:?}", mode.key());
            }
        }
    });
}
