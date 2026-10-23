//! Immersive output of the planar 3D renderer (lane SMB-AE-3D-2; spec 14.27.3 section 5).
//!
//! Spec basis: STU-CMP-055 "Immersive compositing is projection-aware ... Each is computed in the
//! SPHERICAL domain so that the result has no seam at the wrap and no distortion at the poles";
//! the 20 named capabilities ([`IMMERSIVE_CAPABILITIES`]); STU-CMP-055a "The composition carries
//! the immersive frame parameters ([STU-VID-018]'s record shape ...)"; STU-CMP-106 / STU-VID-082:
//! the projection-type enumeration is a declared gap whose members are authored deliberately.
//!
//! Built (Studio authoring of the gap, UNVERIFIED against After Effects, D5):
//! - projection type 1 = EQUIRECTANGULAR (Folio `IMMERSIVE_PROJECTION_EQUIRECTANGULAR`; 0 = flat):
//!   output pixel `(x, y)` of a `w x h` frame looks along longitude `lon = 2 pi ((x + 0.5) / w -
//!   0.5)` and latitude `lat = pi (0.5 - (y + 0.5) / h)`; in camera space (x right, y down, z
//!   forward) the direction is `(cos lat sin lon, -sin lat, cos lat cos lon)`, turned to world by
//!   the camera rotation. The frame centre looks along the camera's forward axis; the top row
//!   looks up (-y).
//! - "Plane to sphere" ([`crate::composition::render_equirect`]): every 3D plane of the scene is
//!   ray-cast per output pixel from the camera position (exact ray-plane solve through the plane's
//!   world map, parents included), its content sampled bilinearly in premultiplied colour at the
//!   hit (texel centres at half pixels, transparent outside its rectangle), and the hits
//!   composited farthest first (equal distance: stack order) with the normal blend at each
//!   plane's opacity over the background. Intersecting planes cut per pixel, as in a 3D run. One
//!   sample per pixel (no supersampling; the poles are as sharp as the equator in angle, not in
//!   texels).
//! - "Sphere rotate" ([`sphere_rotate`]): an equirectangular frame turned about X, Y and Z
//!   (`layer_rotation` order); output direction `d` reads the source at `R^T d`, bilinear in
//!   premultiplied colour with the longitude wrapping (no seam) and the latitude clamped.
//!
//! Refused typed: every other STU-CMP-055 capability ([`IMMERSIVE_UNSUPPORTED`]), an unknown name
//! ([`IMMERSIVE_UNKNOWN`]), a frame without immersive settings or with a projection other than
//! equirectangular, stereoscopic output, captured views other than the full 360 x 180 sphere
//! ([`IMMERSIVE_PARAMETERS`]), a 2D content layer, a track matte, a non-normal blend, a camera
//! with depth of field ([`IMMERSIVE_LAYER`]) and oversized frames (`BudgetExceeded`).
#![deny(clippy::arithmetic_side_effects)]

use crate::CompositeError;
use crate::temporal::TemporalError;

/// The 20 named STU-CMP-055 capabilities (the 21st is recovered only as a count, STU-CMP-123).
pub const IMMERSIVE_CAPABILITIES: [&str; 20] = [
    "projection_convert",
    "plane_to_sphere",
    "sphere_to_plane",
    "sphere_rotate",
    "mobius_zoom",
    "immersive_blur",
    "immersive_spherical_blur",
    "immersive_sharpen",
    "immersive_denoise",
    "immersive_glow",
    "immersive_chromatic_aberration",
    "immersive_chroma_leaks",
    "immersive_digital_glitch",
    "immersive_fractal_noise",
    "immersive_colour_gradients",
    "immersive_light_leaks",
    "immersive_light_rays",
    "immersive_random_blocks",
    "immersive_gradient_wipe",
    "immersive_iris_wipe",
];
/// The capabilities this lane builds.
pub const IMMERSIVE_BUILT: [&str; 2] = ["plane_to_sphere", "sphere_rotate"];
pub const IMMERSIVE_UNSUPPORTED: &str = "composition_immersive_capability_unsupported";
pub const IMMERSIVE_UNKNOWN: &str = "composition_immersive_capability_unknown";
pub const IMMERSIVE_PARAMETERS: &str = "composition_immersive_parameters";
pub const IMMERSIVE_LAYER: &str = "composition_immersive_layer";
/// Largest equirectangular frame side.
pub const MAX_IMMERSIVE_SIDE: u32 = 16_384;

/// `Ok` for a built capability, the typed refusal otherwise.
pub fn immersive_capability(name: &str) -> Result<(), TemporalError> {
    if IMMERSIVE_BUILT.contains(&name) {
        Ok(())
    } else if IMMERSIVE_CAPABILITIES.contains(&name) {
        Err(TemporalError::Unsupported { layer_id: name.to_owned(), feature: IMMERSIVE_UNSUPPORTED })
    } else {
        Err(TemporalError::Unsupported { layer_id: name.to_owned(), feature: IMMERSIVE_UNKNOWN })
    }
}

/// The camera-space direction of equirectangular output pixel `(x, y)` (module docs).
pub fn equirect_direction(x: u32, y: u32, width: u32, height: u32) -> [f64; 3] {
    let lon = std::f64::consts::TAU * ((f64::from(x) + 0.5) / f64::from(width) - 0.5);
    let lat = std::f64::consts::PI * (0.5 - (f64::from(y) + 0.5) / f64::from(height));
    [lat.cos() * lon.sin(), -lat.sin(), lat.cos() * lon.cos()]
}

/// "Sphere rotate" (module docs) of a straight RGB + alpha equirectangular frame (`w x h`,
/// row-major). Refused: a shape mismatch, a zero or oversized side, non-finite angles.
#[allow(clippy::type_complexity)]
pub fn sphere_rotate(rgb: &[f32], alpha: &[f32], w: u32, h: u32, rx: f64, ry: f64, rz: f64) -> Result<(Vec<f32>, Vec<f32>), TemporalError> {
    let refuse = || TemporalError::Unsupported { layer_id: "sphere_rotate".to_owned(), feature: IMMERSIVE_PARAMETERS };
    if w == 0 || h == 0 || w > MAX_IMMERSIVE_SIDE || h > MAX_IMMERSIVE_SIDE || ![rx, ry, rz].iter().all(|v| v.is_finite()) {
        return Err(refuse());
    }
    let n = usize::try_from(u64::from(w).checked_mul(u64::from(h)).ok_or(CompositeError::Overflow)?).map_err(|_| CompositeError::Overflow)?;
    if alpha.len() != n || Some(rgb.len()) != n.checked_mul(3) {
        return Err(refuse());
    }
    let r = crate::three_d::layer_rotation([0.0; 3], rx, ry, rz);
    let rt = crate::three_d::transpose(&r);
    let (wf, hf) = (f64::from(w), f64::from(h));
    let wi = i64::from(w);
    let hi = i64::from(h);
    let mut out_rgb = vec![0.0f32; rgb.len()];
    let mut out_a = vec![0.0f32; n];
    let texel = |ix: i64, iy: i64| -> ([f64; 3], f64) {
        let ix = ix.rem_euclid(wi);
        let iy = iy.clamp(0, hi.saturating_sub(1));
        let k = usize::try_from(iy.saturating_mul(wi).saturating_add(ix)).unwrap_or(0);
        let a = f64::from(alpha.get(k).copied().unwrap_or(0.0));
        let c = |o: usize| f64::from(k.checked_mul(3).and_then(|b| b.checked_add(o)).and_then(|b| rgb.get(b)).copied().unwrap_or(0.0)) * a;
        ([c(0), c(1), c(2)], a)
    };
    let mut at = 0usize;
    for y in 0..h {
        for x in 0..w {
            let d = crate::three_d::mat_vec(&rt, equirect_direction(x, y, w, h));
            let lon = d[0].atan2(d[2]);
            let lat = (-d[1]).clamp(-1.0, 1.0).asin();
            let u = (lon / std::f64::consts::TAU + 0.5) * wf - 0.5;
            let v = (0.5 - lat / std::f64::consts::PI) * hf - 0.5;
            let (u0, v0) = (u.floor(), v.floor());
            let (fu, fv) = (u - u0, v - v0);
            #[allow(clippy::cast_possible_truncation)]
            let (iu, iv) = (u0 as i64, v0 as i64);
            let (mut c, mut a) = ([0.0f64; 3], 0.0f64);
            for (dy, wy) in [(0i64, 1.0 - fv), (1, fv)] {
                for (dx, wx) in [(0i64, 1.0 - fu), (1, fu)] {
                    let (tc, ta) = texel(iu.saturating_add(dx), iv.saturating_add(dy));
                    let wt = wx * wy;
                    a += wt * ta;
                    for k in 0..3 {
                        c[k] += wt * tc[k];
                    }
                }
            }
            #[allow(clippy::cast_possible_truncation)]
            {
                out_a[at] = a as f32;
                for k in 0..3 {
                    out_rgb[at.saturating_mul(3).saturating_add(k)] = if a > 0.0 { (c[k] / a) as f32 } else { 0.0 };
                }
            }
            at = at.checked_add(1).ok_or(CompositeError::Overflow)?;
        }
    }
    Ok((out_rgb, out_a))
}

/// One immersive frame's inputs, evaluated by the composition adapter
/// (`CompositionTemporalSource::immersive_scene_at`) and rendered by
/// [`crate::composition::render_equirect`].
#[derive(Clone, Debug, PartialEq)]
pub struct ImmersiveScene {
    pub width: u32,
    pub height: u32,
    pub background: Option<[f32; 4]>,
    /// Bottom first.
    pub planes: Vec<crate::three_d::Plane3d>,
    pub materials: Vec<crate::three_d::Material>,
    pub lights: Vec<crate::three_d::Light>,
    pub camera: crate::three_d::Camera,
}
