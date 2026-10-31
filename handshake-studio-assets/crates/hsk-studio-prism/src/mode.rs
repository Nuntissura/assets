//! Image > Mode contract (lane SMB-PS-MODE-CONVERT-1; STU-COL-106/107, STU-COL-118..121).
//!
//! * Enumerations (STU-COL-118): [`DocumentMode`] (eight values), [`NewDocumentMode`] (the five
//!   creation modes) and [`ChangeMode`] (seven operations). There is no `to_duotone` operation:
//!   [`ChangeMode::from_token`] refuses the token with `to_duotone_not_a_conversion`.
//! * Depth (STU-COL-120/121): [`BitsPerChannel`] and the per-model availability matrix
//!   [`depth_available`]. The matrix is Studio's normative choice, NOT a captured vendor constraint;
//!   every cell reports [`AvailabilitySource::StudioNormative`] and there is no vendor variant
//!   (test `mt_31179_matrix_is_studio_normative_never_vendor`).
//! * Planning (STU-COL-119): [`plan_mode_change`] refuses illegal pairs typed and returns a
//!   [`DiscardReport`] (colour, alpha and spot planes lost). A plan that discards channels yields a
//!   [`ConfirmedModeChange`] only when the caller echoes that exact report
//!   ([`ModeChangePlan::confirm`]); the host commits the change as its one undoable operation
//!   with its own `ActorContext` (the document record and undo live in Folio/host, not here).
//!   [`change_availability`] reports every operation's availability up front (STU-COL-120: the
//!   32-bit reduction is reported per operation rather than discovered by failure).
//! * Parameter rows (STU-COL-106/107): [`ParamRow`] carries the seven fields separately;
//!   `UNKNOWN` is `None`, serialised as the literal token, and never clamps.
//! * Conversions: [`bitmap`] (grayscale -> bitmap), [`indexed`] (RGB -> indexed) and
//!   [`color_table`] (the indexed colour table and its colour-table file codec). Each is a pure
//!   function over a borrowed source buffer that it never writes; a refusal leaves the source
//!   unchanged and allocates no output (refusals are checked before allocation). No conversion
//!   spawns a thread.
//! * Diagnostics: [`emit_mode_outcome`] sends a terminal outcome through observe (fixed failure
//!   code only; no pixels, names or text).
//!
//! Studio readings (Photoshop parity UNVERIFIED, D5): the legal-change routes of
//! [`plan_mode_change`] (bitmap only from 8/16-bit grayscale and only back to grayscale, indexed
//! only from 8-bit RGB) and the discard classification.
#![deny(clippy::arithmetic_side_effects)]

pub mod bitmap;

use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

/// Most pixels one conversion accepts (Studio bound, V4-02; 16384 x 16384). Checked before any
/// output allocation.
pub const MAX_MODE_PIXELS: u64 = 1 << 28;

/// Typed refusals of the mode contract and every conversion. `code()` is stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeError {
    /// A token that names no value of the enumeration.
    UnknownToken,
    /// `to_duotone` is not a conversion (STU-COL-118; duotone is entered through the ink set).
    DuotoneNotAConversion,
    /// The mode is reachable only by conversion (indexed, multichannel, duotone).
    NotACreationMode,
    /// Bits per channel outside 1, 8, 16, 32.
    UnknownBitsPerChannel,
    /// The mode/depth pair is unavailable (STU-COL-120/121).
    DepthUnavailable(DepthRefusal),
    /// The operation targets the source mode.
    SameMode,
    /// Bitmap is reachable only from grayscale.
    BitmapRequiresGrayscaleSource,
    /// Bitmap is reachable only from 8- or 16-bit grayscale.
    BitmapRequires8Or16BitSource,
    /// A bitmap document converts only to grayscale.
    BitmapConvertsOnlyToGrayscale,
    /// Indexed is reachable only from 8-bit RGB.
    IndexedRequiresRgb8Source,
    /// A multichannel source has the wrong channel count for the target model.
    MultichannelChannelCount,
    /// The channel set does not match the source mode's colour channels.
    ChannelSetMismatch,
    /// The plan discards channels and the caller did not echo its discard report.
    DiscardNotConfirmed,
    /// `indexed.colors` is read-only and derived for this palette.
    IndexedColorsReadOnly,
    /// A value outside a declared hard bound (named by its parameter key).
    ParameterOutOfRange(&'static str),
    /// A non-finite value (named by its parameter key).
    NonFiniteParameter(&'static str),
    /// A value the method needs is unset (its default is UNKNOWN; named by key).
    ParameterRequired(&'static str),
    /// A value the computation cannot use (e.g. a non-positive frequency; named by key).
    InvalidParameter(&'static str),
    /// `custom_pattern` without a pattern name or caller tile.
    PatternMissing,
    /// The caller tile is empty, inconsistent or over [`bitmap::MAX_PATTERN_TILE_CELLS`].
    PatternInvalid,
    /// The requested bitmap resolution differs from the source: resampling is pigment's.
    BitmapResampleNotInPrism,
    /// Zero width or height.
    EmptyImage,
    /// More than [`MAX_MODE_PIXELS`] pixels.
    PixelBudgetExceeded,
    /// The buffer length does not equal width x height x samples.
    BufferLengthMismatch,
    /// `exact` palette over 256 distinct colours.
    ExactPaletteOver256,
    /// `master_*` palettes need several images.
    MasterPaletteNeedsSeveralImages,
    /// `mac_os`, `windows`, `previous`: the table entries are not captured in the spec.
    PaletteTableNotCaptured,
    /// A palette of zero colours cannot index a pixel.
    EmptyPalette,
    /// The forced colours do not fit the colour count.
    ForcedColorsExceedCount,
    /// A foreground/background matte without the caller's colour.
    MatteColorRequired,
    /// A colour table over 256 entries.
    ColorTableOver256,
    /// A colour table with no entries.
    ColorTableEmpty,
    /// `transparency_index` not below the entry count.
    TransparencyIndexOutOfRange,
    /// An entry index not below the entry count.
    ColorTableIndexOutOfRange,
    /// A colour-table file that is neither 768 nor 772 bytes.
    ColorTableFileLength,
    /// A colour-table file count of 0 or over 256.
    ColorTableFileCount,
    /// A shipped table whose entries are not in the spec (`mac_os`, `windows`).
    ColorTableNotShipped,
}

/// Why a mode/depth pair is unavailable (one code per refusal row of STU-COL-120/121).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthRefusal {
    /// 1-bit outside bitmap.
    OneBitBitmapOnly,
    /// Bitmap at 8, 16 or 32.
    BitmapRequires1Bit,
    /// Indexed at 16 or 32.
    IndexedRequires8Bit,
    /// CMYK, Lab, duotone or multichannel at 32.
    Depth32Unavailable,
}

impl ModeError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownToken => "unknown_mode_token",
            Self::DuotoneNotAConversion => "to_duotone_not_a_conversion",
            Self::NotACreationMode => "not_a_creation_mode",
            Self::UnknownBitsPerChannel => "unknown_bits_per_channel",
            Self::DepthUnavailable(DepthRefusal::OneBitBitmapOnly) => "one_bit_bitmap_only",
            Self::DepthUnavailable(DepthRefusal::BitmapRequires1Bit) => "bitmap_requires_1_bit",
            Self::DepthUnavailable(DepthRefusal::IndexedRequires8Bit) => "indexed_requires_8_bit",
            Self::DepthUnavailable(DepthRefusal::Depth32Unavailable) => "depth_32_unavailable",
            Self::SameMode => "same_mode",
            Self::BitmapRequiresGrayscaleSource => "bitmap_requires_grayscale_source",
            Self::BitmapRequires8Or16BitSource => "bitmap_requires_8_or_16_bit_source",
            Self::BitmapConvertsOnlyToGrayscale => "bitmap_converts_only_to_grayscale",
            Self::IndexedRequiresRgb8Source => "indexed_requires_rgb_8_bit_source",
            Self::MultichannelChannelCount => "multichannel_channel_count",
            Self::ChannelSetMismatch => "channel_set_mismatch",
            Self::DiscardNotConfirmed => "discard_not_confirmed",
            Self::IndexedColorsReadOnly => "indexed_colors_read_only",
            Self::ParameterOutOfRange(_) => "parameter_out_of_range",
            Self::NonFiniteParameter(_) => "non_finite_parameter",
            Self::ParameterRequired(_) => "parameter_required",
            Self::InvalidParameter(_) => "invalid_parameter",
            Self::PatternMissing => "pattern_missing",
            Self::PatternInvalid => "pattern_invalid",
            Self::BitmapResampleNotInPrism => "bitmap_resample_not_in_prism",
            Self::EmptyImage => "empty_image",
            Self::PixelBudgetExceeded => "pixel_budget_exceeded",
            Self::BufferLengthMismatch => "buffer_length_mismatch",
            Self::ExactPaletteOver256 => "exact_palette_over_256",
            Self::MasterPaletteNeedsSeveralImages => "master_palette_needs_several_images",
            Self::PaletteTableNotCaptured => "palette_table_not_captured",
            Self::EmptyPalette => "empty_palette",
            Self::ForcedColorsExceedCount => "forced_colors_exceed_count",
            Self::MatteColorRequired => "matte_color_required",
            Self::ColorTableOver256 => "color_table_over_256",
            Self::ColorTableEmpty => "color_table_empty",
            Self::TransparencyIndexOutOfRange => "transparency_index_out_of_range",
            Self::ColorTableIndexOutOfRange => "color_table_index_out_of_range",
            Self::ColorTableFileLength => "color_table_file_length",
            Self::ColorTableFileCount => "color_table_file_count",
            Self::ColorTableNotShipped => "color_table_not_shipped",
        }
    }

    /// The fixed observe failure code of this refusal.
    pub const fn failure_code(self) -> FailureCode {
        match self {
            Self::DepthUnavailable(_)
            | Self::BitmapRequiresGrayscaleSource
            | Self::BitmapRequires8Or16BitSource
            | Self::BitmapConvertsOnlyToGrayscale
            | Self::IndexedRequiresRgb8Source
            | Self::BitmapResampleNotInPrism
            | Self::MasterPaletteNeedsSeveralImages
            | Self::PaletteTableNotCaptured
            | Self::ColorTableNotShipped => FailureCode::Unsupported,
            _ => FailureCode::Validation,
        }
    }
}

impl std::fmt::Display for ModeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ModeError {}

/// A closed token enumeration: `ALL` in spec order, `token()` and a refusing `from_token()`.
macro_rules! token_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident => $t:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $v),+ }
        impl $name {
            /// Every value, in spec order.
            pub const ALL: &'static [Self] = &[$(Self::$v),+];
            /// The spec token.
            pub const fn token(self) -> &'static str {
                match self { $(Self::$v => $t),+ }
            }
            /// The value of a spec token (exact, case-sensitive).
            pub fn from_token(token: &str) -> Result<Self, $crate::mode::ModeError> {
                match token {
                    $($t => Ok(Self::$v),)+
                    _ => Err($crate::mode::ModeError::UnknownToken),
                }
            }
        }
    };
}
pub(crate) use token_enum;

token_enum! {
    /// STU-COL-118 `document_mode`.
    DocumentMode {
        Grayscale => "grayscale",
        Rgb => "rgb",
        Cmyk => "cmyk",
        Lab => "lab",
        Bitmap => "bitmap",
        Indexed => "indexed",
        Multichannel => "multichannel",
        Duotone => "duotone",
    }
}

token_enum! {
    /// STU-COL-118 `new_document_mode`: the five modes available at creation.
    NewDocumentMode {
        Grayscale => "grayscale",
        Rgb => "rgb",
        Cmyk => "cmyk",
        Lab => "lab",
        Bitmap => "bitmap",
    }
}

impl NewDocumentMode {
    pub const fn mode(self) -> DocumentMode {
        match self {
            Self::Grayscale => DocumentMode::Grayscale,
            Self::Rgb => DocumentMode::Rgb,
            Self::Cmyk => DocumentMode::Cmyk,
            Self::Lab => DocumentMode::Lab,
            Self::Bitmap => DocumentMode::Bitmap,
        }
    }
    /// The creation mode of `mode`; indexed, multichannel and duotone are refused.
    pub const fn try_from_mode(mode: DocumentMode) -> Result<Self, ModeError> {
        match mode {
            DocumentMode::Grayscale => Ok(Self::Grayscale),
            DocumentMode::Rgb => Ok(Self::Rgb),
            DocumentMode::Cmyk => Ok(Self::Cmyk),
            DocumentMode::Lab => Ok(Self::Lab),
            DocumentMode::Bitmap => Ok(Self::Bitmap),
            DocumentMode::Indexed | DocumentMode::Multichannel | DocumentMode::Duotone => Err(ModeError::NotACreationMode),
        }
    }
}

/// STU-COL-118 `change_mode`: seven operations (no `to_duotone`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeMode {
    ToGrayscale,
    ToRgb,
    ToCmyk,
    ToLab,
    ToBitmap,
    ToIndexed,
    ToMultichannel,
}

impl ChangeMode {
    pub const ALL: &'static [Self] =
        &[Self::ToGrayscale, Self::ToRgb, Self::ToCmyk, Self::ToLab, Self::ToBitmap, Self::ToIndexed, Self::ToMultichannel];
    pub const fn token(self) -> &'static str {
        match self {
            Self::ToGrayscale => "to_grayscale",
            Self::ToRgb => "to_rgb",
            Self::ToCmyk => "to_cmyk",
            Self::ToLab => "to_lab",
            Self::ToBitmap => "to_bitmap",
            Self::ToIndexed => "to_indexed",
            Self::ToMultichannel => "to_multichannel",
        }
    }
    /// `to_duotone` is refused with `to_duotone_not_a_conversion`; other unknown tokens with
    /// `unknown_mode_token`.
    pub fn from_token(token: &str) -> Result<Self, ModeError> {
        if token == "to_duotone" {
            return Err(ModeError::DuotoneNotAConversion);
        }
        Self::ALL.iter().copied().find(|c| c.token() == token).ok_or(ModeError::UnknownToken)
    }
    pub const fn target(self) -> DocumentMode {
        match self {
            Self::ToGrayscale => DocumentMode::Grayscale,
            Self::ToRgb => DocumentMode::Rgb,
            Self::ToCmyk => DocumentMode::Cmyk,
            Self::ToLab => DocumentMode::Lab,
            Self::ToBitmap => DocumentMode::Bitmap,
            Self::ToIndexed => DocumentMode::Indexed,
            Self::ToMultichannel => DocumentMode::Multichannel,
        }
    }
}

/// STU-COL-120 `bits_per_channel`. 32-bit is floating point, linear-light, unbounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BitsPerChannel {
    One,
    Eight,
    Sixteen,
    ThirtyTwo,
}

impl BitsPerChannel {
    pub const ALL: &'static [Self] = &[Self::One, Self::Eight, Self::Sixteen, Self::ThirtyTwo];
    pub const fn bits(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Eight => 8,
            Self::Sixteen => 16,
            Self::ThirtyTwo => 32,
        }
    }
    pub const fn from_bits(bits: u8) -> Result<Self, ModeError> {
        match bits {
            1 => Ok(Self::One),
            8 => Ok(Self::Eight),
            16 => Ok(Self::Sixteen),
            32 => Ok(Self::ThirtyTwo),
            _ => Err(ModeError::UnknownBitsPerChannel),
        }
    }
    /// Floating point, linear-light and unbounded (no clip to 0..1): 32-bit only.
    pub const fn is_float_linear_unbounded(self) -> bool {
        matches!(self, Self::ThirtyTwo)
    }
}

/// Where a depth-availability cell comes from. STU-COL-121: the matrix is Studio's normative
/// choice; there is deliberately no vendor-derived variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvailabilitySource {
    StudioNormative,
}

impl AvailabilitySource {
    pub const fn token(self) -> &'static str {
        match self {
            Self::StudioNormative => "studio_normative",
        }
    }
}

/// One cell of the STU-COL-121 matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthCell {
    pub mode: DocumentMode,
    pub depth: BitsPerChannel,
    pub available: bool,
    pub source: AvailabilitySource,
}

/// The STU-COL-121 refusal of `mode` at `depth`, or `Ok` where the matrix says yes.
pub const fn check_mode_depth(mode: DocumentMode, depth: BitsPerChannel) -> Result<(), ModeError> {
    use BitsPerChannel as B;
    use DocumentMode as M;
    match (mode, depth) {
        (M::Bitmap, B::One) => Ok(()),
        (M::Bitmap, _) => Err(ModeError::DepthUnavailable(DepthRefusal::BitmapRequires1Bit)),
        (_, B::One) => Err(ModeError::DepthUnavailable(DepthRefusal::OneBitBitmapOnly)),
        (M::Indexed, B::Eight) => Ok(()),
        (M::Indexed, _) => Err(ModeError::DepthUnavailable(DepthRefusal::IndexedRequires8Bit)),
        (M::Grayscale | M::Rgb, _) => Ok(()),
        (M::Cmyk | M::Lab | M::Duotone | M::Multichannel, B::ThirtyTwo) => {
            Err(ModeError::DepthUnavailable(DepthRefusal::Depth32Unavailable))
        }
        (M::Cmyk | M::Lab | M::Duotone | M::Multichannel, _) => Ok(()),
    }
}

/// Whether the STU-COL-121 matrix makes `depth` available for `mode`.
pub const fn depth_available(mode: DocumentMode, depth: BitsPerChannel) -> bool {
    check_mode_depth(mode, depth).is_ok()
}

/// The STU-COL-121 cell of `mode` at `depth`, labelled Studio-normative.
pub const fn depth_cell(mode: DocumentMode, depth: BitsPerChannel) -> DepthCell {
    DepthCell { mode, depth, available: depth_available(mode, depth), source: AvailabilitySource::StudioNormative }
}

/// Colour channels of a mode (multichannel: any count, `None`).
pub const fn colour_channels(mode: DocumentMode) -> Option<u16> {
    match mode {
        DocumentMode::Grayscale | DocumentMode::Bitmap | DocumentMode::Indexed | DocumentMode::Duotone => Some(1),
        DocumentMode::Rgb | DocumentMode::Lab => Some(3),
        DocumentMode::Cmyk => Some(4),
        DocumentMode::Multichannel => None,
    }
}

/// The source document's planes as the host reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelSet {
    pub colour_channels: u16,
    pub alpha_channels: u16,
    pub spot_channels: u16,
}

/// What a planned change loses. Channel losses require confirmation (STU-COL-119); the three
/// flags report lossy data changes that keep the channel count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DiscardReport {
    pub colour_channels_lost: u16,
    pub alpha_channels_lost: u16,
    pub spot_channels_lost: u16,
    /// Tone collapses to 1 bit (to bitmap).
    pub tonal_precision_lost: bool,
    /// Colours collapse to a palette of at most 256 (to indexed).
    pub palette_quantised: bool,
    /// Colour channels become spot channels (to multichannel; lossy for composite viewing).
    pub composite_view_lossy: bool,
}

impl DiscardReport {
    /// Any colour, alpha or spot plane is discarded: the host must warn before executing.
    pub const fn discards_channels(&self) -> bool {
        self.colour_channels_lost != 0 || self.alpha_channels_lost != 0 || self.spot_channels_lost != 0
    }
}

/// A legal, not yet confirmed mode change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeChangePlan {
    pub op: ChangeMode,
    pub from: DocumentMode,
    pub to: DocumentMode,
    pub from_depth: BitsPerChannel,
    pub to_depth: BitsPerChannel,
    pub channels: ChannelSet,
    pub discard: DiscardReport,
}

/// A plan whose discard warning was acknowledged (or that discards nothing). Only
/// [`ModeChangePlan::confirm`] builds one; the host executes it as one undoable commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmedModeChange {
    plan: ModeChangePlan,
}

impl ConfirmedModeChange {
    pub const fn plan(&self) -> &ModeChangePlan {
        &self.plan
    }
}

impl ModeChangePlan {
    /// The host must warn before a channel-discarding change (STU-COL-119).
    pub const fn requires_confirmation(&self) -> bool {
        self.discard.discards_channels()
    }
    /// Confirm the plan: a channel-discarding plan needs `acknowledged` equal to its report,
    /// otherwise `discard_not_confirmed`.
    pub fn confirm(&self, acknowledged: Option<&DiscardReport>) -> Result<ConfirmedModeChange, ModeError> {
        if self.requires_confirmation() && acknowledged != Some(&self.discard) {
            return Err(ModeError::DiscardNotConfirmed);
        }
        Ok(ConfirmedModeChange { plan: *self })
    }
}

/// Plan `op` from a `from` document at `depth` with `channels` (module docs). Refusals are typed;
/// nothing executes here.
pub fn plan_mode_change(from: DocumentMode, op: ChangeMode, depth: BitsPerChannel, channels: ChannelSet) -> Result<ModeChangePlan, ModeError> {
    use DocumentMode as M;
    check_mode_depth(from, depth)?;
    match colour_channels(from) {
        Some(n) if n != channels.colour_channels => return Err(ModeError::ChannelSetMismatch),
        None if channels.colour_channels == 0 => return Err(ModeError::ChannelSetMismatch),
        _ => {}
    }
    let to = op.target();
    if to == from {
        return Err(ModeError::SameMode);
    }
    if from == M::Bitmap && to != M::Grayscale {
        return Err(ModeError::BitmapConvertsOnlyToGrayscale);
    }
    let to_depth = match to {
        M::Bitmap => {
            if from != M::Grayscale {
                return Err(ModeError::BitmapRequiresGrayscaleSource);
            }
            if !matches!(depth, BitsPerChannel::Eight | BitsPerChannel::Sixteen) {
                return Err(ModeError::BitmapRequires8Or16BitSource);
            }
            BitsPerChannel::One
        }
        M::Indexed => {
            if from != M::Rgb || depth != BitsPerChannel::Eight {
                return Err(ModeError::IndexedRequiresRgb8Source);
            }
            BitsPerChannel::Eight
        }
        _ if from == M::Bitmap => BitsPerChannel::Eight,
        _ => depth,
    };
    if from == M::Multichannel {
        if let Some(n) = colour_channels(to) {
            if to != M::Grayscale && n != channels.colour_channels {
                return Err(ModeError::MultichannelChannelCount);
            }
        }
    }
    check_mode_depth(to, to_depth)?;
    let mut discard = DiscardReport::default();
    match to {
        M::Bitmap => {
            discard.alpha_channels_lost = channels.alpha_channels;
            discard.spot_channels_lost = channels.spot_channels;
            discard.tonal_precision_lost = true;
        }
        M::Indexed => {
            discard.alpha_channels_lost = channels.alpha_channels;
            discard.spot_channels_lost = channels.spot_channels;
            discard.palette_quantised = true;
        }
        M::Grayscale => discard.colour_channels_lost = channels.colour_channels.saturating_sub(1),
        M::Multichannel => discard.composite_view_lossy = true,
        _ => {}
    }
    Ok(ModeChangePlan { op, from, to, from_depth: depth, to_depth, channels, discard })
}

/// Every operation's availability from `from` at `depth` (STU-COL-120: reported per operation,
/// not discovered by failure): the target depth, or the typed refusal.
pub fn change_availability(from: DocumentMode, depth: BitsPerChannel, channels: ChannelSet) -> [(ChangeMode, Result<BitsPerChannel, ModeError>); 7] {
    let mut out = [(ChangeMode::ToGrayscale, Err(ModeError::SameMode)); 7];
    for (slot, op) in out.iter_mut().zip(ChangeMode::ALL) {
        *slot = (*op, plan_mode_change(from, *op, depth, channels).map(|p| p.to_depth));
    }
    out
}

/// STU-COL-106 seven-field parameter record. `None` is the literal `UNKNOWN` (STU-COL-107): it
/// never clamps and is never replaced by an opposite bound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamRow {
    pub key: &'static str,
    pub hard_min: Option<f64>,
    pub hard_max: Option<f64>,
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
    pub default: Option<f64>,
    pub unit: Option<&'static str>,
    pub precision: Option<u8>,
}

impl ParamRow {
    /// `value` if finite and inside the declared hard bounds (an UNKNOWN side is open); never
    /// clamped.
    pub fn check(&self, value: f64) -> Result<f64, ModeError> {
        if !value.is_finite() {
            return Err(ModeError::NonFiniteParameter(self.key));
        }
        if self.hard_min.is_some_and(|m| value < m) || self.hard_max.is_some_and(|m| value > m) {
            return Err(ModeError::ParameterOutOfRange(self.key));
        }
        Ok(value)
    }
    /// The record as JSON with `"UNKNOWN"` for every undeclared field.
    pub fn to_json(&self) -> String {
        fn num(v: Option<f64>) -> String {
            v.map_or_else(|| "\"UNKNOWN\"".to_owned(), |x| format!("{x:?}"))
        }
        format!(
            "{{\"key\":\"{}\",\"hard_min\":{},\"hard_max\":{},\"soft_min\":{},\"soft_max\":{},\"default\":{},\"unit\":{},\"precision\":{}}}",
            self.key,
            num(self.hard_min),
            num(self.hard_max),
            num(self.soft_min),
            num(self.soft_max),
            num(self.default),
            self.unit.map_or_else(|| "\"UNKNOWN\"".to_owned(), |u| format!("\"{u}\"")),
            self.precision.map_or_else(|| "\"UNKNOWN\"".to_owned(), |p| p.to_string()),
        )
    }
}

const fn unknown_row(key: &'static str, unit: &'static str) -> ParamRow {
    ParamRow { key, hard_min: None, hard_max: None, soft_min: None, soft_max: None, default: None, unit: Some(unit), precision: None }
}

/// STU-COL-119 bitmap table row 1.
pub const BITMAP_RESOLUTION: ParamRow = ParamRow { default: Some(72.0), ..unknown_row("bitmap.resolution", "ppi") };
/// STU-COL-119 bitmap table row 2.
pub const BITMAP_FREQUENCY: ParamRow = unknown_row("bitmap.frequency", "lines_per_inch");
/// STU-COL-119 bitmap table row 3.
pub const BITMAP_ANGLE: ParamRow = unknown_row("bitmap.angle", "degrees");
/// STU-COL-119 bitmap table row 4.
pub const BITMAP_THRESHOLD: ParamRow = unknown_row("bitmap.threshold", "percent");
/// STU-COL-119 indexed table row 1 (`hard_min` not declared).
pub const INDEXED_COLORS: ParamRow =
    ParamRow { hard_max: Some(256.0), precision: Some(0), ..unknown_row("indexed.colors", "count") };
/// STU-COL-119 indexed table row 2.
pub const INDEXED_DITHER_AMOUNT: ParamRow =
    ParamRow { hard_min: Some(1.0), hard_max: Some(100.0), precision: Some(0), ..unknown_row("indexed.dither_amount", "percent") };
/// The bitmap parameter table, in spec order.
pub const BITMAP_PARAM_ROWS: [ParamRow; 4] = [BITMAP_RESOLUTION, BITMAP_FREQUENCY, BITMAP_ANGLE, BITMAP_THRESHOLD];
/// The indexed parameter table, in spec order.
pub const INDEXED_PARAM_ROWS: [ParamRow; 2] = [INDEXED_COLORS, INDEXED_DITHER_AMOUNT];

/// Pixel count of a `width` x `height` x `samples` buffer of `len` elements: `empty_image`,
/// `pixel_budget_exceeded` (before any allocation) or `buffer_length_mismatch`.
pub(crate) fn check_plane(width: u32, height: u32, samples: u64, len: usize) -> Result<usize, ModeError> {
    if width == 0 || height == 0 {
        return Err(ModeError::EmptyImage);
    }
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or(ModeError::PixelBudgetExceeded)?;
    if pixels > MAX_MODE_PIXELS {
        return Err(ModeError::PixelBudgetExceeded);
    }
    let want = pixels.checked_mul(samples).ok_or(ModeError::PixelBudgetExceeded)?;
    if u64::try_from(len).ok() != Some(want) {
        return Err(ModeError::BufferLengthMismatch);
    }
    usize::try_from(pixels).map_err(|_| ModeError::PixelBudgetExceeded)
}

/// Caller-owned attribution of one conversion's terminal outcome.
#[derive(Clone, Debug)]
pub struct ModeObserveContext {
    pub correlation_id: u64,
    pub revision: u64,
    pub resource: DomainId,
    pub actor: ActorContext,
}

/// Emit a conversion's terminal outcome through observe: success, or the refusal's fixed
/// [`ModeError::failure_code`]. No pixels, names or text are serialised.
pub fn emit_mode_outcome(
    ctx: &ModeObserveContext,
    result: Result<(), ModeError>,
    cancel: &CancellationToken,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, hsk_studio_observe::Error> {
    let budget = Budget::new(0, 0)?;
    let mut observe = Observe::new(ctx.correlation_id, ctx.revision, ctx.resource.clone(), ctx.actor.clone(), budget);
    let outcome = match result {
        Ok(()) => Outcome::Success,
        Err(e) => Outcome::Failure(e.failure_code()),
    };
    observe.emit(
        Observation { correlation_id: ctx.correlation_id, revision: ctx.revision, outcome, progress: None, private_project_text: None },
        cancel,
        sink,
    )
}

/// The mode-conversion descriptor (the `mode_conversion` key of the prism DESCRIPTOR points here).
pub const MODE_CONVERSION_DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PRISM","surface":"mode_conversion","lane":"SMB-PS-MODE-CONVERT-1","spec":["STU-COL-106","STU-COL-107","STU-COL-118","STU-COL-119","STU-COL-120","STU-COL-121"],"enumerations":{"document_mode":8,"new_document_mode":5,"change_mode":7,"to_duotone":"refused to_duotone_not_a_conversion"},"depth_matrix":{"source":"studio_normative","vendor_derived":false,"query":"depth_available/depth_cell/check_mode_depth"},"plan":"plan_mode_change(from, op, depth, channels) -> ModeChangePlan with DiscardReport; confirm(Some(&report)) required when channels are discarded; change_availability reports all seven operations","conversions":{"to_bitmap":"bitmap::grayscale_to_bitmap: 8/16-bit grayscale -> packed 1-bit (1 = black, MSB first, rows padded to bytes); half_threshold, pattern_dither (8x8 Bayer), diffusion_dither (Floyd-Steinberg, raster order, single thread), halftone_screen (six spot functions), custom_pattern (caller tile); resolution change refused bitmap_resample_not_in_prism","to_indexed":"indexed::rgb_to_indexed: 8-bit RGB/RGBA -> index plane + colour table (<=256); exact, web, uniform, local_perceptual/selective/adaptive; master_* and mac_os/windows/previous refused typed; forced colours, preserve_exact_colors, transparency, seven mattes, dither none/diffusion/pattern/noise(seeded)","color_table":"color_table::ColorTable + colour-table file codec (768-byte RGB block, optional big-endian u16 count and transparency index; Adobe Photoshop File Formats Specification); shipped black_white and grayscale; mac_os/windows not shipped (spec gap)"},"bounds":{"pixels":268435456,"table_entries":256,"pattern_tile_cells":1048576},"refusals":"typed ModeError::code() before output allocation; the borrowed source is never written","parameters":"ParamRow seven fields, UNKNOWN serialised literally, no clamp","diagnostics":"emit_mode_outcome: observe terminal outcome with fixed failure code only","parity":"dither matrices, diffusion kernel, spot functions, local palette algorithms, mattes and legal-change routes are Studio readings; Photoshop parity UNVERIFIED (D5)","pending":["duotone, multichannel and grayscale tonal mapping","Folio document-mode record and the host's one-undo commit","bitmap resampling (pigment)","master palettes","mac_os/windows tables (spec gap)","Image > Mode menu"]}"#;
