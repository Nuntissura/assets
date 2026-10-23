//! PSD blend-mode keys. The enumeration, its facts and the per-pixel maths belong to Pigment
//! (`blend_math::StudioBlendMode`, single owner, CX-CAP-001); this module is only the PSD key
//! vocabulary that maps a four-byte record key to that enumeration. The key bytes themselves are
//! always kept verbatim by the layer record, so a key without a member (any key outside this
//! table) is a typed loss and is never mapped to Normal.

use hsk_studio_pigment::blend_math::StudioBlendMode as Mode;
pub use hsk_studio_pigment::blend_math::StudioBlendMode;

/// The 28 PSD layer blend keys (specification: Layer records, blend mode key) and their Studio
/// members. `pass` is a group property: Pigment lists it as group-only.
pub const PSD_BLEND_KEYS: [([u8; 4], StudioBlendMode); 28] = [
    (*b"pass", Mode::PassThrough),
    (*b"norm", Mode::Normal),
    (*b"diss", Mode::Dissolve),
    (*b"dark", Mode::Darken),
    (*b"mul ", Mode::Multiply),
    (*b"idiv", Mode::ColourBurn),
    (*b"lbrn", Mode::LinearBurn),
    (*b"dkCl", Mode::DarkerColour),
    (*b"lite", Mode::Lighten),
    (*b"scrn", Mode::Screen),
    (*b"div ", Mode::ColourDodge),
    (*b"lddg", Mode::LinearDodgeAdd),
    (*b"lgCl", Mode::LighterColour),
    (*b"over", Mode::Overlay),
    (*b"sLit", Mode::SoftLight),
    (*b"hLit", Mode::HardLight),
    (*b"vLit", Mode::VividLight),
    (*b"lLit", Mode::LinearLight),
    (*b"pLit", Mode::PinLight),
    (*b"hMix", Mode::HardMix),
    (*b"diff", Mode::Difference),
    (*b"smud", Mode::Exclusion),
    (*b"fsub", Mode::Subtract),
    (*b"fdiv", Mode::Divide),
    (*b"hue ", Mode::Hue),
    (*b"sat ", Mode::Saturation),
    (*b"colr", Mode::Colour),
    (*b"lum ", Mode::Luminosity),
];

/// The Studio member for a PSD key, `None` for a key outside the 28 known ones.
pub fn studio_blend_for_key(key: [u8; 4]) -> Option<StudioBlendMode> {
    PSD_BLEND_KEYS.iter().find(|(k, _)| *k == key).map(|(_, mode)| *mode)
}

/// The PSD key for a Studio member, `None` for members with no PSD layer key (`behind`, `clear`
/// and the STU-RAS-039 members 31..=35).
pub fn psd_key_for(mode: StudioBlendMode) -> Option<[u8; 4]> {
    PSD_BLEND_KEYS.iter().find(|(_, m)| *m == mode).map(|(key, _)| *key)
}
