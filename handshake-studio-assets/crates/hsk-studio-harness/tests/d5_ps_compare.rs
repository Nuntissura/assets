//! D5 Photoshop oracle comparison (lane SMB-D5-PS-COMPARE, WP-KERNEL-STUDIO).
//!
//! Photoshop 27.11.0 rendered the D5 oracle cases (`oracle-session/hsk_oracle_photoshop.jsx`, run
//! 2026-10-16) into `<oracle corpus root>/adobe-session/photoshop`: `manifest.json`, the input
//! fixtures under `inputs/` and one TIFF per case variant. Each test below rebuilds the variants of
//! its case family as a Studio render plan from the same fixtures, renders it through render-cpu
//! (`execute_extended`, encoded blend space as Photoshop's default document) and compares the result
//! with the Photoshop TIFF. Every variant gets a verdict line (`D5 <case>/<variant> <verdict> ...`);
//! the machine-readable results go to `<CARGO_TARGET_TMPDIR>/d5_ps_compare/<test>.json`.
//!
//! A MISMATCH or NOT_SUPPORTED fails the test unless `d5_ps_compare/expectations.json` records it
//! with a reason; an absent corpus prints `NOT_PROVEN:` (never a silent pass). Tolerances and the
//! verdict vocabulary: `d5_ps_compare/support.rs`.
#![cfg(feature = "viewer")]

#[path = "d5_ps_compare/adjust.rs"]
mod adjust;
#[path = "d5_ps_compare/blend.rs"]
mod blend;
#[path = "d5_ps_compare/colour.rs"]
mod colour;
#[path = "d5_ps_compare/effects.rs"]
mod effects;
#[path = "d5_ps_compare/structure.rs"]
mod structure;
#[path = "d5_ps_compare/support.rs"]
mod support;

/// PS-B01: 27 Photoshop blend modes x {8-bit, 16-bit} at 100/100 and 8-bit at opacity 50 and at
/// fill 50 (108 variants).
#[test]
fn ps_oracle_blend_mode_grids() {
    support::run_family("ps_oracle_blend_mode_grids", &["PS-B01"], blend::build);
}

/// PS-C01..PS-C08: adjustment layers (invert in a group over transparency, levels, curves,
/// hue/saturation, brightness/contrast, exposure at 8 and 32 bits, threshold, posterize).
#[test]
fn ps_oracle_adjustment_layers() {
    support::run_family(
        "ps_oracle_adjustment_layers",
        &["PS-C01", "PS-C02", "PS-C03", "PS-C04", "PS-C05", "PS-C06", "PS-C07", "PS-C08"],
        adjust::build,
    );
}

/// Blend-If, mask density/feather, knockout, clipping with Blend-If and pass-through groups
/// (PS-C09..PS-C11, PS-C18..PS-C23, PS-C28, PS-C29).
#[test]
fn ps_oracle_layer_structure() {
    support::run_family(
        "ps_oracle_layer_structure",
        &["PS-C09", "PS-C10", "PS-C11", "PS-C18", "PS-C19", "PS-C20", "PS-C21", "PS-C22", "PS-C23", "PS-C28", "PS-C29"],
        structure::build,
    );
}

/// Layer effects (PS-C12..PS-C17) and effects with Blend-If or on groups (PS-C24..PS-C27).
#[test]
fn ps_oracle_layer_effects() {
    support::run_family(
        "ps_oracle_layer_effects",
        &["PS-C12", "PS-C13", "PS-C14", "PS-C15", "PS-C16", "PS-C17", "PS-C24", "PS-C25", "PS-C26", "PS-C27"],
        effects::build,
    );
}

/// Colour: Lab 16-bit patches to U.S. Web Coated (SWOP) v2 (relative colorimetric, perceptual;
/// PS-L01) and the Photoshop-authored 16-bit Lab PSD opened through the convert flow (PS-L02).
#[test]
fn ps_oracle_colour_conversions() {
    support::run_family("ps_oracle_colour_conversions", &["PS-L01", "PS-L02"], colour::build);
}
