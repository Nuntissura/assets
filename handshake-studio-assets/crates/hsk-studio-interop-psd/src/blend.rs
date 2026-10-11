//! Blend mode keys exactly as stored in layer records. A key that is not in [`BlendMode::ALL`]
//! is a typed loss: it must never be mapped to Normal. The four-byte key is always kept
//! verbatim by the layer record.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendMode {
    PassThrough,
    Normal,
    Dissolve,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    DarkerColor,
    Lighten,
    Screen,
    ColorDodge,
    LinearDodge,
    LighterColor,
    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub const ALL: [BlendMode; 28] = [
        Self::PassThrough,
        Self::Normal,
        Self::Dissolve,
        Self::Darken,
        Self::Multiply,
        Self::ColorBurn,
        Self::LinearBurn,
        Self::DarkerColor,
        Self::Lighten,
        Self::Screen,
        Self::ColorDodge,
        Self::LinearDodge,
        Self::LighterColor,
        Self::Overlay,
        Self::SoftLight,
        Self::HardLight,
        Self::VividLight,
        Self::LinearLight,
        Self::PinLight,
        Self::HardMix,
        Self::Difference,
        Self::Exclusion,
        Self::Subtract,
        Self::Divide,
        Self::Hue,
        Self::Saturation,
        Self::Color,
        Self::Luminosity,
    ];

    /// The four-byte key as stored in a layer record.
    pub fn key(self) -> [u8; 4] {
        *match self {
            Self::PassThrough => b"pass",
            Self::Normal => b"norm",
            Self::Dissolve => b"diss",
            Self::Darken => b"dark",
            Self::Multiply => b"mul ",
            Self::ColorBurn => b"idiv",
            Self::LinearBurn => b"lbrn",
            Self::DarkerColor => b"dkCl",
            Self::Lighten => b"lite",
            Self::Screen => b"scrn",
            Self::ColorDodge => b"div ",
            Self::LinearDodge => b"lddg",
            Self::LighterColor => b"lgCl",
            Self::Overlay => b"over",
            Self::SoftLight => b"sLit",
            Self::HardLight => b"hLit",
            Self::VividLight => b"vLit",
            Self::LinearLight => b"lLit",
            Self::PinLight => b"pLit",
            Self::HardMix => b"hMix",
            Self::Difference => b"diff",
            Self::Exclusion => b"smud",
            Self::Subtract => b"fsub",
            Self::Divide => b"fdiv",
            Self::Hue => b"hue ",
            Self::Saturation => b"sat ",
            Self::Color => b"colr",
            Self::Luminosity => b"lum ",
        }
    }

    pub fn from_key(key: [u8; 4]) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::PassThrough => "pass_through",
            Self::Normal => "normal",
            Self::Dissolve => "dissolve",
            Self::Darken => "darken",
            Self::Multiply => "multiply",
            Self::ColorBurn => "color_burn",
            Self::LinearBurn => "linear_burn",
            Self::DarkerColor => "darker_color",
            Self::Lighten => "lighten",
            Self::Screen => "screen",
            Self::ColorDodge => "color_dodge",
            Self::LinearDodge => "linear_dodge",
            Self::LighterColor => "lighter_color",
            Self::Overlay => "overlay",
            Self::SoftLight => "soft_light",
            Self::HardLight => "hard_light",
            Self::VividLight => "vivid_light",
            Self::LinearLight => "linear_light",
            Self::PinLight => "pin_light",
            Self::HardMix => "hard_mix",
            Self::Difference => "difference",
            Self::Exclusion => "exclusion",
            Self::Subtract => "subtract",
            Self::Divide => "divide",
            Self::Hue => "hue",
            Self::Saturation => "saturation",
            Self::Color => "color",
            Self::Luminosity => "luminosity",
        }
    }
}
