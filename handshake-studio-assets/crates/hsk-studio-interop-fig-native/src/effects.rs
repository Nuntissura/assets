//! Figma shadow and blur effects -> Folio `StudioEffect` (raster records,
//! `StudioEffectKind::DropShadow` / `InnerShadow` / `Blur`).
//!
//! Conventions written here (all UNVERIFIED against Figma rendering, see `NATIVE_NOT_PROVEN`):
//! `size` = Figma radius in px; a Figma offset becomes distance + light angle with
//! `offset = (-distance cos a, distance sin a)` (Photoshop light-angle reading, y down), so
//! `a = atan2(dy, -dx)`; the colour alpha becomes the effect opacity percent; a Figma inner-shadow
//! spread becomes a choke percent of the radius (approximation, reported as a loss note).
//! Shadow blur: a Figma DROP_SHADOW / INNER_SHADOW is written with
//! `blur_kernel: gaussian` (standard deviation radius / 2) so it does not inherit Photoshop's tent
//! kernel (lane SMB-PS-ORACLE-PARITY-1, STUDIO-R36 remediation). Basis: Figma help "Apply effects
//! to layers" (shadow effects translate to CSS `box-shadow`) and CSS Backgrounds 3 section 6.1.2
//! (box-shadow blur = Gaussian of standard deviation half the blur radius); Figma's own rasteriser
//! is UNVERIFIED (D5). Tested by `tests/fig_paints_2.rs` `bold_text_renders_with_the_inter_bold_fixture`
//! and `tests/fig_paints.rs`.
#![deny(clippy::arithmetic_side_effects)]
use crate::native::{blend, tidy, wide};
use crate::nodes::Effect;
use hsk_studio_folio::{
    BLEND_PASS_THROUGH, BlurScope, MAX_BLUR_RADIUS, ProfileBinding, StudioColor, StudioEffect,
    StudioEffectBlurKernel, StudioEffectKind,
};

/// A mapped effect plus the loss codes of the parts that did not survive (approximations).
pub(crate) type Mapped = (StudioEffect, Vec<&'static str>);

/// FOREGROUND_BLUR -> `Blur { layer }`, BACKGROUND_BLUR -> `Blur { below }`. The Figma radius is
/// written as the Folio radius unchanged; whether it equals the Gaussian sigma or twice it is
/// UNVERIFIED (listed in `NATIVE_NOT_PROVEN`). Folio refuses a colour or contour on a blur and
/// bounds the radius to `0..=MAX_BLUR_RADIUS`.
fn blur(e: &Effect, ordinal: usize) -> Result<Mapped, &'static str> {
    let radius = wide(e.radius.unwrap_or(0.0));
    if !(0.0..=MAX_BLUR_RADIUS).contains(&radius) {
        return Err("effect_blur_radius_out_of_range");
    }
    let render_scope = if e.kind == "BACKGROUND_BLUR" {
        BlurScope::Below
    } else {
        BlurScope::Layer
    };
    Ok((
        StudioEffect {
            instance_id: format!("fig-fx-{ordinal}"),
            enabled: e.visible != Some(false),
            visible_in_editor: true,
            blend_mode: blend("NORMAL").ok_or("effect_blend_mode_no_folio_home")?,
            opacity: 100.0,
            colour: None,
            contour: None,
            anti_alias: true,
            noise: 0.0,
            angle: 0.0,
            use_global_light: false,
            params: StudioEffectKind::Blur {
                render_scope,
                radius,
            },
        },
        Vec::new(),
    ))
}

pub(crate) fn effect(e: &Effect, ordinal: usize) -> Result<Mapped, &'static str> {
    let drop = match e.kind.as_str() {
        "DROP_SHADOW" => true,
        "INNER_SHADOW" => false,
        "FOREGROUND_BLUR" | "BACKGROUND_BLUR" => return blur(e, ordinal),
        _ => return Err("effect_kind_not_in_folio"),
    };
    let c = e.color.as_ref().ok_or("effect_colour_missing")?;
    let mut notes = Vec::new();
    let blend_mode = match e.blend_mode.as_deref() {
        None | Some("NORMAL") => blend("NORMAL"),
        Some(name) => blend(name).filter(|m| *m != BLEND_PASS_THROUGH),
    }
    .ok_or("effect_blend_mode_no_folio_home")?;
    let (dx, dy) = e
        .offset
        .map_or((0.0, 0.0), |o| (f64::from(o.x), f64::from(o.y)));
    let distance = tidy(dx.hypot(dy));
    let angle = if distance == 0.0 {
        0.0
    } else {
        tidy(dy.atan2(-dx).to_degrees())
    };
    let size = wide(e.radius.unwrap_or(0.0)).max(0.0);
    let spread = wide(e.spread.unwrap_or(0.0));
    let params = if drop {
        if spread < 0.0 {
            notes.push("effect_negative_spread_clamped");
        }
        StudioEffectKind::DropShadow {
            distance,
            spread: spread.max(0.0),
            size,
            // Figma draws the shadow behind a transparent node only with showShadowBehindNode.
            layer_knocks_out: e.show_shadow_behind_node != Some(true),
            blur_kernel: Some(StudioEffectBlurKernel::Gaussian),
        }
    } else {
        let choke = if spread > 0.0 && size > 0.0 {
            notes.push("inner_shadow_spread_as_choke_percent");
            tidy((spread / size * 100.0).clamp(0.0, 100.0))
        } else {
            if spread != 0.0 {
                notes.push("inner_shadow_spread_dropped");
            }
            0.0
        };
        StudioEffectKind::InnerShadow {
            distance,
            choke,
            size,
            blur_kernel: Some(StudioEffectBlurKernel::Gaussian),
        }
    };
    Ok((
        StudioEffect {
            instance_id: format!("fig-fx-{ordinal}"),
            enabled: e.visible != Some(false),
            visible_in_editor: true,
            blend_mode,
            opacity: tidy(wide(c.a) * 100.0).clamp(0.0, 100.0),
            colour: Some(StudioColor {
                r: wide(c.r).clamp(0.0, 1.0),
                g: wide(c.g).clamp(0.0, 1.0),
                b: wide(c.b).clamp(0.0, 1.0),
                colour_profile_id: ProfileBinding::Unassigned(()),
            }),
            contour: None,
            anti_alias: true,
            noise: 0.0,
            angle,
            use_global_light: false,
            params,
        },
        notes,
    ))
}
