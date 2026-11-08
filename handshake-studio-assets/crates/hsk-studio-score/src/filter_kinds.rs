//! Filter-kind table (lane SMB-SCORE-EFFECTS-6): the `filter_kind` keys under which a stored effect
//! instance (`(key, value)` parameters, untrusted) builds a score processor, and how each row binds
//! to the spec catalogue.
//!
//! Every row's parameters are its existing score params struct, field for field (keys are the
//! field names; the graphic equalizer uses its spec sheet names). No row adds a parameter list.
//! Enumerations and flags are stored as whole numbers (`0`/`1`; dynamics mode Compressor 0 /
//! Expander 1, detector Peak 0 / Rms 1), `damping_hz` 0 is "no damping", and the multiband
//! compressor stores `bands`, `crossover_<i>_hz` for `i < bands - 1` and every
//! [`DynamicsParams`](crate::DynamicsParams) field of band `b` as `band_<b>_<field>`.
//!
//! **Binding classes** ([`BindingClass`]): `spec_table` - the row's spec sheet maps onto the params
//! (graphic equalizer sheets; Hard Limiter sheet through `HardLimiterParams::from_normalised`);
//! `spec_table_partial` - some sheet rows bind (`bind_spec_row`), the rest are refused as undeclared
//! (Flanger, Phaser); `authored_on_tabled_row` - the catalogue row has a sheet but none of its rows
//! has a declared meaning, so the score set is authored (Chorus on Chorus/Flanger, ClickRepair on
//! AutoClickRemover; no catalogue row claimed); `authored_existing` - score's own validated set where
//! the catalogue row declares no parameter table or no row binds (STU-FX-128, gap STU-FX-146; no
//! catalogue row claimed). [`ParameterSource::EngineAuthored`] rows whose catalogue row declares 0
//! parameters carry an SD item naming the gap for the spec owner. Only `spec_table` and
//! `spec_table_partial` rows name a catalogue identity and import key.
//!
//! Hostile input: at most [`MAX_AUDIO_FILTER_PARAMS`] parameters, duplicate, missing and unknown
//! keys refused, whole-number fields checked (finite, integral, in range) before any cast, every
//! real value range checked by the processor constructor. Nothing is allocated from a stored value
//! before its bound is checked (the constructors check sizes first).
#![deny(clippy::arithmetic_side_effects)]

use crate::delay::{DelayParams, FeedbackDelay};
use crate::dsp::{HardLimiter, HardLimiterParams};
use crate::dynamics::{Detector, DynamicsMode, DynamicsParams};
use crate::error::ScoreError;
use crate::graphic_eq::{GraphicEq, GraphicEqParams};
use crate::layout::MAX_CHANNELS;
use crate::modulation::{Chorus, ChorusParams, Flanger, FlangerParams, MAX_CHORUS_VOICES, PHASER_STAGES, Phaser, PhaserParams};
use crate::multiband::{MULTIBAND_BANDS, MultibandCompressor, MultibandParams};
use crate::restore::{CLICK_AR_ORDER, ClickRepair, ClickRepairParams, HumParams, HumRemover, MAX_HUM_HARMONICS};
use crate::reverb::{Reverb, ReverbParams};
use crate::session::Automatable;

/// Most parameters one stored instance may carry (Folio's `MAX_AUDIO_PRESET_PARAMS`).
pub const MAX_AUDIO_FILTER_PARAMS: usize = 256;

/// How a row binds to the spec catalogue (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingClass {
    SpecTable,
    SpecTablePartial,
    AuthoredOnTabledRow,
    AuthoredExisting,
}

impl BindingClass {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SpecTable => "spec_table",
            Self::SpecTablePartial => "spec_table_partial",
            Self::AuthoredOnTabledRow => "authored_on_tabled_row",
            Self::AuthoredExisting => "authored_existing",
        }
    }
}

/// Where the row's parameter list comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterSource {
    /// The spec sheet (ranges and mapping).
    SpecTable,
    /// Score's own validated set (ranges authored).
    EngineAuthored,
}

impl ParameterSource {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SpecTable => "spec_table",
            Self::EngineAuthored => "engine_authored",
        }
    }
}

/// One row of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterKindRow {
    pub filter_kind: &'static str,
    /// Prefix of the processor's `identity()` (equal to `filter_kind` except the multiband
    /// compressor, whose score tag is `multiband`).
    pub identity_tag: &'static str,
    pub binding: BindingClass,
    pub parameter_source: ParameterSource,
    /// Spec catalogue name and import key (STU-FX-134a), only for `spec_table` and
    /// `spec_table_partial` rows.
    pub catalogue_name: Option<&'static str>,
    pub import_key: Option<&'static str>,
    /// The SD item raising the STU-FX-146 gap of an engine-authored row whose catalogue row declares
    /// 0 parameters.
    pub sd_item: Option<&'static str>,
    /// Widest channel count the processor accepts.
    pub max_channels: usize,
}

const fn row(
    filter_kind: &'static str,
    identity_tag: &'static str,
    binding: BindingClass,
    parameter_source: ParameterSource,
    catalogue: Option<(&'static str, &'static str)>,
    sd_item: Option<&'static str>,
    max_channels: usize,
) -> FilterKindRow {
    let (catalogue_name, import_key) = match catalogue {
        Some((name, key)) => (Some(name), Some(key)),
        None => (None, None),
    };
    FilterKindRow { filter_kind, identity_tag, binding, parameter_source, catalogue_name, import_key, sd_item, max_channels }
}

use BindingClass::{AuthoredExisting, AuthoredOnTabledRow, SpecTable, SpecTablePartial};
use ParameterSource::{EngineAuthored, SpecTable as FromSheet};

/// The table (module docs).
pub const FILTER_KINDS: [FilterKindRow; 12] = [
    row("graphic_eq_10_bands", "graphic_eq_10_bands", SpecTable, FromSheet, Some(("GraphicEQ10Bands", "AudioFilter:GraphicEQ10Bands")), None, MAX_CHANNELS),
    row("graphic_eq_20_bands", "graphic_eq_20_bands", SpecTable, FromSheet, Some(("GraphicEQ20Bands", "AudioFilter:GraphicEQ20Bands")), None, MAX_CHANNELS),
    row("graphic_eq_30_bands", "graphic_eq_30_bands", SpecTable, FromSheet, Some(("GraphicEQ30Bands", "AudioFilter:GraphicEQ30Bands")), None, MAX_CHANNELS),
    row("hard_limiter", "hard_limiter", SpecTable, FromSheet, Some(("Hard Limiter", "AudioFilter:HardLimiter")), None, MAX_CHANNELS),
    row("flanger", "flanger", SpecTablePartial, EngineAuthored, Some(("Flanger", "AudioFilter:Flanger")), None, MAX_CHANNELS),
    row("phaser", "phaser", SpecTablePartial, EngineAuthored, Some(("Phaser", "AudioFilter:Phaser")), None, MAX_CHANNELS),
    row("chorus", "chorus", AuthoredOnTabledRow, EngineAuthored, None, None, MAX_CHANNELS),
    row("click_repair", "click_repair", AuthoredOnTabledRow, EngineAuthored, None, None, MAX_CHANNELS),
    row("reverb", "reverb", AuthoredExisting, EngineAuthored, None, None, 2),
    row("feedback_delay", "feedback_delay", AuthoredExisting, EngineAuthored, None, Some("SD-FX146-ANALOG-DELAY"), MAX_CHANNELS),
    row("hum_remover", "hum_remover", AuthoredExisting, EngineAuthored, None, Some("SD-FX146-DEHUMMER"), MAX_CHANNELS),
    row("multiband_compressor", "multiband", AuthoredExisting, EngineAuthored, None, Some("SD-FX146-MULTIBAND"), MAX_CHANNELS),
];

/// The row of `filter_kind`.
pub fn audio_filter_kind(filter_kind: &str) -> Option<&'static FilterKindRow> {
    let _ = filter_kind; // red stub
    None
}

/// Typed refusal of a stored parameter set; `code()` strings are wire-stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFilterError {
    /// No row of the table has this `filter_kind`.
    UnknownKind,
    /// A key the row requires is absent.
    MissingParameter,
    /// A key the row does not use is present.
    UnknownParameter,
    /// A key occurs twice.
    DuplicateParameter,
    /// More than [`MAX_AUDIO_FILTER_PARAMS`] parameters.
    TooManyParameters,
    /// A whole-number field (count, enumeration, flag) holds a fraction.
    NotWholeNumber,
    /// The processor refused the values (range, rate, layout, NaN).
    Score(ScoreError),
}

impl AudioFilterError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownKind => "audio_filter_unknown_kind",
            Self::MissingParameter => "audio_filter_missing_parameter",
            Self::UnknownParameter => "audio_filter_unknown_parameter",
            Self::DuplicateParameter => "audio_filter_duplicate_parameter",
            Self::TooManyParameters => "audio_filter_too_many_parameters",
            Self::NotWholeNumber => "audio_filter_not_whole_number",
            Self::Score(e) => e.code(),
        }
    }
}

impl std::fmt::Display for AudioFilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for AudioFilterError {}

impl From<ScoreError> for AudioFilterError {
    fn from(e: ScoreError) -> Self {
        Self::Score(e)
    }
}

/// One stored parameter in the row's canonical order, with its score automation index.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterKindParam {
    pub key: String,
    pub value: f64,
    /// Index into the built processor's `automation_parameters()`; `None`: fixed at construction.
    pub automation: Option<usize>,
}

fn p(key: impl Into<String>, value: f64, automation: Option<usize>) -> FilterKindParam {
    FilterKindParam { key: key.into(), value, automation }
}

fn flag(v: bool) -> f64 {
    if v { 1.0 } else { 0.0 }
}

/// Stored parameters of a decoded set, typed.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Typed {
    GraphicEq(GraphicEqParams),
    HardLimiter(HardLimiterParams),
    Flanger(FlangerParams),
    Phaser(PhaserParams),
    Chorus(ChorusParams),
    ClickRepair(ClickRepairParams),
    Reverb(ReverbParams),
    Delay(DelayParams),
    Hum(HumParams),
    Multiband(MultibandParams),
}

fn dynamics_params(prefix: &str, d: &DynamicsParams, band: usize) -> Vec<FilterKindParam> {
    // Automation of band `b`: threshold 3b, ratio 3b + 1, makeup 3b + 2 (MultibandCompressor).
    let base = band.checked_mul(3);
    let at = |k: usize| base.and_then(|b| b.checked_add(k));
    vec![
        p(format!("{prefix}mode"), if d.mode == DynamicsMode::Expander { 1.0 } else { 0.0 }, None),
        p(format!("{prefix}threshold_db"), d.threshold_db, at(0)),
        p(format!("{prefix}ratio"), d.ratio, at(1)),
        p(format!("{prefix}knee_db"), d.knee_db, None),
        p(format!("{prefix}attack_ms"), d.attack_ms, None),
        p(format!("{prefix}release_ms"), d.release_ms, None),
        p(format!("{prefix}hold_ms"), d.hold_ms, None),
        p(format!("{prefix}range_db"), d.range_db, None),
        p(format!("{prefix}makeup_db"), d.makeup_db, at(2)),
        p(format!("{prefix}detector"), if d.detector == Detector::Rms { 1.0 } else { 0.0 }, None),
        p(format!("{prefix}rms_window_ms"), d.rms_window_ms, None),
        p(format!("{prefix}link"), flag(d.link), None),
    ]
}

impl Typed {
    fn defaults(filter_kind: &str) -> Option<Self> {
        Some(match filter_kind {
            "graphic_eq_10_bands" => Self::GraphicEq(GraphicEqParams::flat(10)),
            "graphic_eq_20_bands" => Self::GraphicEq(GraphicEqParams::flat(20)),
            "graphic_eq_30_bands" => Self::GraphicEq(GraphicEqParams::flat(30)),
            "hard_limiter" => Self::HardLimiter(HardLimiterParams::default()),
            "flanger" => Self::Flanger(FlangerParams::default()),
            "phaser" => Self::Phaser(PhaserParams::default()),
            "chorus" => Self::Chorus(ChorusParams::default()),
            "click_repair" => Self::ClickRepair(ClickRepairParams::default()),
            "reverb" => Self::Reverb(ReverbParams::default()),
            "feedback_delay" => Self::Delay(DelayParams::default()),
            "hum_remover" => Self::Hum(HumParams::default()),
            "multiband_compressor" => Self::Multiband(MultibandParams::three_band()),
            _ => return None,
        })
    }

    /// The canonical stored form (key order = the row's order).
    fn encode(&self) -> Vec<FilterKindParam> {
        match self {
            Self::GraphicEq(g) => {
                let mut v = vec![p("accuracy", g.accuracy, None), p("gain", g.gain_db, Some(0)), p("range", g.range, None)];
                for (k, a) in (1..=g.bands).zip(g.amps_db.iter()) {
                    v.push(p(format!("amp{k}"), *a, Some(k)));
                }
                v
            }
            Self::HardLimiter(h) => vec![
                p("max_amp_db", h.max_amp_db, Some(0)),
                p("input_boost_db", h.input_boost_db, Some(1)),
                p("lookahead_ms", h.lookahead_ms, None),
                p("release_ms", h.release_ms, Some(2)),
            ],
            Self::Flanger(f) => vec![
                p("initial_ms", f.initial_ms, Some(0)),
                p("final_ms", f.final_ms, Some(1)),
                p("rate_hz", f.rate_hz, Some(2)),
                p("stereo_phase_deg", f.stereo_phase_deg, None),
                p("feedback", f.feedback, Some(3)),
                p("mix", f.mix, Some(4)),
            ],
            Self::Phaser(f) => vec![
                p("stages", f.stages as f64, None),
                p("min_hz", f.min_hz, Some(0)),
                p("max_hz", f.max_hz, Some(1)),
                p("rate_hz", f.rate_hz, Some(2)),
                p("stereo_phase_deg", f.stereo_phase_deg, None),
                p("feedback", f.feedback, Some(3)),
                p("mix", f.mix, Some(4)),
            ],
            Self::Chorus(c) => vec![
                p("voices", c.voices as f64, None),
                p("delay_ms", c.delay_ms, Some(0)),
                p("depth_ms", c.depth_ms, Some(1)),
                p("rate_hz", c.rate_hz, Some(2)),
                p("stereo_phase_deg", c.stereo_phase_deg, None),
                p("mix", c.mix, Some(3)),
            ],
            Self::ClickRepair(c) => vec![p("ar_order", c.ar_order as f64, None), p("threshold", c.threshold, Some(0)), p("max_gap_ms", c.max_gap_ms, None)],
            Self::Reverb(r) => vec![
                p("rt60_s", r.rt60_s, Some(0)),
                p("damping", r.damping, Some(1)),
                p("pre_delay_ms", r.pre_delay_ms, None),
                p("width", r.width, Some(2)),
                p("wet", r.wet, Some(3)),
                p("dry", r.dry, Some(4)),
            ],
            Self::Delay(d) => vec![
                p("time_ms", d.time_ms, None),
                p("feedback", d.feedback, Some(0)),
                p("damping_hz", d.damping_hz.unwrap_or(0.0), None),
                p("mix", d.mix, Some(1)),
                p("ping_pong", flag(d.ping_pong), None),
            ],
            Self::Hum(h) => vec![p("fundamental_hz", h.fundamental_hz, Some(0)), p("harmonics", h.harmonics as f64, None), p("bandwidth_hz", h.bandwidth_hz, Some(1))],
            Self::Multiband(m) => {
                let mut v = vec![p("bands", m.bands as f64, None)];
                for (i, f) in m.crossovers_hz.iter().enumerate().take(m.bands.saturating_sub(1)) {
                    v.push(p(format!("crossover_{i}_hz"), *f, None));
                }
                for (b, d) in m.band_params.iter().enumerate().take(m.bands) {
                    v.extend(dynamics_params(&format!("band_{b}_"), d, b));
                }
                v
            }
        }
    }

    fn decode(filter_kind: &str, r: &mut Reader<'_>) -> Result<Self, AudioFilterError> {
        let base = Self::defaults(filter_kind).ok_or(AudioFilterError::UnknownKind)?;
        Ok(match base {
            Self::GraphicEq(mut g) => {
                g.accuracy = r.real("accuracy")?;
                g.gain_db = r.real("gain")?;
                g.range = r.real("range")?;
                for (k, a) in (1..=g.bands).zip(g.amps_db.iter_mut()) {
                    *a = r.real(&format!("amp{k}"))?;
                }
                Self::GraphicEq(g)
            }
            Self::HardLimiter(_) => Self::HardLimiter(HardLimiterParams {
                max_amp_db: r.real("max_amp_db")?,
                input_boost_db: r.real("input_boost_db")?,
                lookahead_ms: r.real("lookahead_ms")?,
                release_ms: r.real("release_ms")?,
            }),
            Self::Flanger(_) => Self::Flanger(FlangerParams {
                initial_ms: r.real("initial_ms")?,
                final_ms: r.real("final_ms")?,
                rate_hz: r.real("rate_hz")?,
                stereo_phase_deg: r.real("stereo_phase_deg")?,
                feedback: r.real("feedback")?,
                mix: r.real("mix")?,
            }),
            Self::Phaser(_) => Self::Phaser(PhaserParams {
                stages: r.whole("stages", PHASER_STAGES.0, PHASER_STAGES.1)?,
                min_hz: r.real("min_hz")?,
                max_hz: r.real("max_hz")?,
                rate_hz: r.real("rate_hz")?,
                stereo_phase_deg: r.real("stereo_phase_deg")?,
                feedback: r.real("feedback")?,
                mix: r.real("mix")?,
            }),
            Self::Chorus(_) => Self::Chorus(ChorusParams {
                voices: r.whole("voices", 1, MAX_CHORUS_VOICES)?,
                delay_ms: r.real("delay_ms")?,
                depth_ms: r.real("depth_ms")?,
                rate_hz: r.real("rate_hz")?,
                stereo_phase_deg: r.real("stereo_phase_deg")?,
                mix: r.real("mix")?,
            }),
            Self::ClickRepair(_) => Self::ClickRepair(ClickRepairParams {
                ar_order: r.whole("ar_order", CLICK_AR_ORDER.0, CLICK_AR_ORDER.1)?,
                threshold: r.real("threshold")?,
                max_gap_ms: r.real("max_gap_ms")?,
            }),
            Self::Reverb(_) => Self::Reverb(ReverbParams {
                rt60_s: r.real("rt60_s")?,
                damping: r.real("damping")?,
                pre_delay_ms: r.real("pre_delay_ms")?,
                width: r.real("width")?,
                wet: r.real("wet")?,
                dry: r.real("dry")?,
            }),
            Self::Delay(_) => {
                let time_ms = r.real("time_ms")?;
                let feedback = r.real("feedback")?;
                let damping = r.real("damping_hz")?;
                let mix = r.real("mix")?;
                let ping_pong = r.flag("ping_pong")?;
                Self::Delay(DelayParams { time_ms, feedback, damping_hz: (damping != 0.0).then_some(damping), mix, ping_pong })
            }
            Self::Hum(_) => Self::Hum(HumParams {
                fundamental_hz: r.real("fundamental_hz")?,
                harmonics: r.whole("harmonics", 1, MAX_HUM_HARMONICS)?,
                bandwidth_hz: r.real("bandwidth_hz")?,
            }),
            Self::Multiband(mut m) => {
                m.bands = r.whole("bands", MULTIBAND_BANDS.0, MULTIBAND_BANDS.1)?;
                let splits = m.bands.saturating_sub(1);
                for (i, f) in m.crossovers_hz.iter_mut().enumerate().take(splits) {
                    *f = r.real(&format!("crossover_{i}_hz"))?;
                }
                let bands = m.bands;
                for (b, d) in m.band_params.iter_mut().enumerate().take(bands) {
                    let k = |field: &str| format!("band_{b}_{field}");
                    let mode = r.whole(&k("mode"), 0, 1)?;
                    *d = DynamicsParams {
                        mode: if mode == 1 { DynamicsMode::Expander } else { DynamicsMode::Compressor },
                        threshold_db: r.real(&k("threshold_db"))?,
                        ratio: r.real(&k("ratio"))?,
                        knee_db: r.real(&k("knee_db"))?,
                        attack_ms: r.real(&k("attack_ms"))?,
                        release_ms: r.real(&k("release_ms"))?,
                        hold_ms: r.real(&k("hold_ms"))?,
                        range_db: r.real(&k("range_db"))?,
                        makeup_db: r.real(&k("makeup_db"))?,
                        detector: if r.whole(&k("detector"), 0, 1)? == 1 { Detector::Rms } else { Detector::Peak },
                        rms_window_ms: r.real(&k("rms_window_ms"))?,
                        link: r.flag(&k("link"))?,
                    };
                }
                Self::Multiband(m)
            }
        })
    }

    fn build(&self, rate: u32, channels: usize) -> Result<Box<dyn Automatable + Send>, ScoreError> {
        Ok(match self {
            Self::GraphicEq(g) => Box::new(GraphicEq::new(rate, channels, *g)?),
            Self::HardLimiter(h) => Box::new(HardLimiter::new(rate, channels, *h)?),
            Self::Flanger(f) => Box::new(Flanger::new(rate, channels, *f)?),
            Self::Phaser(f) => Box::new(Phaser::new(rate, channels, *f)?),
            Self::Chorus(c) => Box::new(Chorus::new(rate, channels, *c)?),
            Self::ClickRepair(c) => Box::new(ClickRepair::new(rate, channels, *c)?),
            Self::Reverb(r) => Box::new(Reverb::new(rate, channels, *r)?),
            Self::Delay(d) => Box::new(FeedbackDelay::new(rate, channels, *d)?),
            Self::Hum(h) => Box::new(HumRemover::new(rate, channels, *h)?),
            Self::Multiband(m) => Box::new(MultibandCompressor::new(rate, channels, *m)?),
        })
    }
}

/// Reads keyed values, refusing duplicate, missing and (at `finish`) unused keys.
struct Reader<'a> {
    params: &'a [(&'a str, f64)],
    used: Vec<bool>,
}

impl<'a> Reader<'a> {
    fn new(params: &'a [(&'a str, f64)]) -> Result<Self, AudioFilterError> {
        if params.len() > MAX_AUDIO_FILTER_PARAMS {
            return Err(AudioFilterError::TooManyParameters);
        }
        for (i, (key, _)) in params.iter().enumerate() {
            if params[..i].iter().any(|(k, _)| k == key) {
                return Err(AudioFilterError::DuplicateParameter);
            }
        }
        Ok(Self { params, used: vec![false; params.len()] })
    }
    fn real(&mut self, key: &str) -> Result<f64, AudioFilterError> {
        let i = self.params.iter().position(|(k, _)| *k == key).ok_or(AudioFilterError::MissingParameter)?;
        self.used[i] = true;
        Ok(self.params[i].1)
    }
    /// A whole number in `lo..=hi`, checked before the cast: NaN/inf `NotFinite`, a fraction
    /// `NotWholeNumber`, outside the range `OutOfRange`.
    fn whole(&mut self, key: &str, lo: usize, hi: usize) -> Result<usize, AudioFilterError> {
        let v = self.real(key)?;
        if !v.is_finite() {
            return Err(AudioFilterError::Score(ScoreError::NotFinite));
        }
        if v.fract() != 0.0 {
            return Err(AudioFilterError::NotWholeNumber);
        }
        if v < lo as f64 || v > hi as f64 {
            return Err(AudioFilterError::Score(ScoreError::OutOfRange));
        }
        Ok(v as usize)
    }
    fn flag(&mut self, key: &str) -> Result<bool, AudioFilterError> {
        Ok(self.whole(key, 0, 1)? == 1)
    }
    fn finish(self) -> Result<(), AudioFilterError> {
        if self.used.iter().all(|u| *u) { Ok(()) } else { Err(AudioFilterError::UnknownParameter) }
    }
}

fn decode(filter_kind: &str, params: &[(&str, f64)]) -> Result<Typed, AudioFilterError> {
    let mut reader = Reader::new(params)?;
    let typed = Typed::decode(filter_kind, &mut reader)?;
    reader.finish()?;
    Ok(typed)
}

/// The default stored parameters of `filter_kind` (canonical order); `None` for an unknown kind.
pub fn audio_filter_defaults(filter_kind: &str) -> Option<Vec<FilterKindParam>> {
    let _ = filter_kind; // red stub
    None
}

/// The stored set `params` in the row's canonical order with automation indices (structure only:
/// keys, duplicates, whole-number fields; real ranges are checked by [`build_audio_filter`]).
pub fn audio_filter_parameters(filter_kind: &str, params: &[(&str, f64)]) -> Result<Vec<FilterKindParam>, AudioFilterError> {
    let _ = (filter_kind, params); // red stub
    Err(AudioFilterError::UnknownKind)
}

/// Builds the processor of a stored set for `rate` and `channels` (refusals: [`AudioFilterError`]).
pub fn build_audio_filter(filter_kind: &str, params: &[(&str, f64)], rate: u32, channels: usize) -> Result<Box<dyn Automatable + Send>, AudioFilterError> {
    let _ = (filter_kind, params, rate, channels, decode as fn(&str, &[(&str, f64)]) -> Result<Typed, AudioFilterError>); // red stub
    Err(AudioFilterError::UnknownKind)
}
