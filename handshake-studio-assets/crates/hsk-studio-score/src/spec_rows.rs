//! Binding of spec audio contract-table rows (STU-FX-134a) to Studio processor fields.
//!
//! A row is bound only when its meaning follows from the spec text (name, range and mapping) and
//! a Studio processor field implements it. Every other row of a covered effect is a typed
//! refusal ([`SpecRowError::Undeclared`]) naming why, instead of a guessed meaning. Rows of
//! effects not listed here are [`SpecRowError::UnknownRow`]. Units in the sheets are undeclared
//! (STU-FX-136b); where a bound field authors a unit it is named in the field string.
//!
//! Contract records (lane SMB-SCORE-EFFECTS-6, STU-FX-136a/136b): [`SpecParameterRow`] keeps one
//! sheet row as declared, every field separately (STU-FX-107: an unknown stays `None`, never a
//! copy of another field; STU-FX-121: the real default and the normalised default are two forms of
//! one default, related only by the mapping). The graphic equalizer sheets are reproduced row for
//! row ([`GRAPHIC_EQ_10_BANDS_ROWS`], [`GRAPHIC_EQ_20_BANDS_ROWS`], [`GRAPHIC_EQ_30_BANDS_ROWS`]).

/// Why a row is not bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UndeclaredSemantics {
    pub effect: &'static str,
    pub parameter: &'static str,
    pub reason: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecRowError {
    /// The row exists in the spec table but its meaning is not declared (or no Studio field
    /// implements it); it is refused, never guessed.
    Undeclared(UndeclaredSemantics),
    /// No row with this effect and parameter is covered by this table.
    UnknownRow,
}

impl SpecRowError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Undeclared(_) => "spec_row_undeclared_semantics",
            Self::UnknownRow => "spec_row_unknown",
        }
    }
}

const NO_MEANING: &str = "spec row declares a name and range but no semantics";
const RESERVED: &str = "spec row is an unused/reserved slot";
const NO_UNIT: &str = "spec row declares no unit and the unit cannot be derived from its range";

/// `(effect, parameter, Ok(studio field) | Err(reason))`.
pub const SPEC_ROWS: &[(&str, &str, Result<&str, &str>)] = &[
    // Dynamics (sheet DynamicsUI).
    ("Dynamics", "attack_time", Ok("SidechainParams.dynamics.attack_ms (ms authored)")),
    ("Dynamics", "release_time", Ok("SidechainParams.dynamics.release_ms (ms authored; Studio minimum 1)")),
    ("Dynamics", "look_ahead_time", Ok("SidechainParams.lookahead_ms (ms authored)")),
    ("Dynamics", "link_channels", Ok("DynamicsParams.link")),
    ("Dynamics", "use_rms", Ok("DynamicsParams.detector (true = Rms)")),
    ("Dynamics", "envelope_attack_time", Err(NO_MEANING)),
    ("Dynamics", "envelope_release_time", Err(NO_MEANING)),
    ("Dynamics", "input_gain", Err("no Studio dynamics field; use a Gain stage before it in the Chain")),
    ("Dynamics", "output_gain", Err("no Studio dynamics field; use a Gain stage after it in the Chain")),
    ("Dynamics", "low_frequency", Err(NO_MEANING)),
    ("Dynamics", "high_frequency", Err(NO_MEANING)),
    ("Dynamics", "UNUSED1", Err(RESERVED)),
    ("Dynamics", "UNUNSED2", Err(RESERVED)),
    ("Dynamics", "use_splines", Err(NO_MEANING)),
    ("Dynamics", "use_noise_gate", Err(NO_MEANING)),
    ("Dynamics", "makeup_gain", Err(NO_UNIT)),
    // Flanger (sheet FlangerUI).
    ("Flanger", "initial", Ok("FlangerParams.initial_ms (ms authored)")),
    ("Flanger", "final", Ok("FlangerParams.final_ms (ms authored)")),
    ("Flanger", "phase", Ok("FlangerParams.stereo_phase_deg (degrees)")),
    ("Flanger", "mix", Ok("FlangerParams.mix (spec percent / 100)")),
    ("Flanger", "feedback", Ok("FlangerParams.feedback (spec percent / 100; 100 refused as unstable)")),
    ("Flanger", "cycles", Err(NO_MEANING)),
    ("Flanger", "beats", Err(NO_MEANING)),
    // Chorus/Flanger (sheet ChorusFlanger): no ranges and no semantics.
    ("Chorus/Flanger", "width", Err(NO_MEANING)),
    ("Chorus/Flanger", "intensity", Err(NO_MEANING)),
    ("Chorus/Flanger", "transience", Err(NO_MEANING)),
    // Phaser (sheet PhaserUI).
    ("Phaser", "mix", Ok("PhaserParams.mix (spec percent / 100)")),
    ("Phaser", "intensity", Err(NO_MEANING)),
    ("Phaser", "depth", Err(NO_MEANING)),
    // AutoClickRemover (sheet AutoClickRemoverUI): 1..100 scalars without semantics; ClickRepair
    // uses its own authored threshold (robust sigmas) and AR order instead.
    ("AutoClickRemover", "inf_threshold", Err(NO_MEANING)),
    ("AutoClickRemover", "inf_complexity", Err(NO_MEANING)),
    // GraphicEQ10Bands (sheet GraphicEQ__Bands): gain and amp bound; accuracy and range have no declared meaning.
    ("GraphicEQ10Bands", "accuracy", Err(NO_MEANING)),
    ("GraphicEQ10Bands", "gain", Ok("GraphicEqParams.gain_db (master gain, dB authored)")),
    ("GraphicEQ10Bands", "range", Err(NO_MEANING)),
    ("GraphicEQ10Bands", "amp1", Ok("GraphicEqParams.amps_db[0] (band 1 command, dB authored; band centre authored)")),
    ("GraphicEQ10Bands", "amp2", Ok("GraphicEqParams.amps_db[1] (band 2 command, dB authored; band centre authored)")),
    ("GraphicEQ10Bands", "amp3", Ok("GraphicEqParams.amps_db[2] (band 3 command, dB authored; band centre authored)")),
    ("GraphicEQ10Bands", "amp4", Ok("GraphicEqParams.amps_db[3] (band 4 command, dB authored; band centre authored)")),
    ("GraphicEQ10Bands", "amp5", Ok("GraphicEqParams.amps_db[4] (band 5 command, dB authored; band centre authored)")),
    ("GraphicEQ10Bands", "amp6", Ok("GraphicEqParams.amps_db[5] (band 6 command, dB authored; band centre authored)")),
    ("GraphicEQ10Bands", "amp7", Ok("GraphicEqParams.amps_db[6] (band 7 command, dB authored; band centre authored)")),
    ("GraphicEQ10Bands", "amp8", Ok("GraphicEqParams.amps_db[7] (band 8 command, dB authored; band centre authored)")),
    ("GraphicEQ10Bands", "amp9", Ok("GraphicEqParams.amps_db[8] (band 9 command, dB authored; band centre authored)")),
    ("GraphicEQ10Bands", "amp10", Ok("GraphicEqParams.amps_db[9] (band 10 command, dB authored; band centre authored)")),
    // GraphicEQ20Bands (sheet GraphicEQ__Bands): gain and amp bound; accuracy and range have no declared meaning.
    ("GraphicEQ20Bands", "accuracy", Err(NO_MEANING)),
    ("GraphicEQ20Bands", "gain", Ok("GraphicEqParams.gain_db (master gain, dB authored)")),
    ("GraphicEQ20Bands", "range", Err(NO_MEANING)),
    ("GraphicEQ20Bands", "amp1", Ok("GraphicEqParams.amps_db[0] (band 1 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp2", Ok("GraphicEqParams.amps_db[1] (band 2 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp3", Ok("GraphicEqParams.amps_db[2] (band 3 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp4", Ok("GraphicEqParams.amps_db[3] (band 4 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp5", Ok("GraphicEqParams.amps_db[4] (band 5 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp6", Ok("GraphicEqParams.amps_db[5] (band 6 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp7", Ok("GraphicEqParams.amps_db[6] (band 7 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp8", Ok("GraphicEqParams.amps_db[7] (band 8 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp9", Ok("GraphicEqParams.amps_db[8] (band 9 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp10", Ok("GraphicEqParams.amps_db[9] (band 10 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp11", Ok("GraphicEqParams.amps_db[10] (band 11 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp12", Ok("GraphicEqParams.amps_db[11] (band 12 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp13", Ok("GraphicEqParams.amps_db[12] (band 13 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp14", Ok("GraphicEqParams.amps_db[13] (band 14 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp15", Ok("GraphicEqParams.amps_db[14] (band 15 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp16", Ok("GraphicEqParams.amps_db[15] (band 16 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp17", Ok("GraphicEqParams.amps_db[16] (band 17 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp18", Ok("GraphicEqParams.amps_db[17] (band 18 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp19", Ok("GraphicEqParams.amps_db[18] (band 19 command, dB authored; band centre authored)")),
    ("GraphicEQ20Bands", "amp20", Ok("GraphicEqParams.amps_db[19] (band 20 command, dB authored; band centre authored)")),
    // GraphicEQ30Bands (sheet GraphicEQ__Bands): gain and amp bound; accuracy and range have no declared meaning.
    ("GraphicEQ30Bands", "accuracy", Err(NO_MEANING)),
    ("GraphicEQ30Bands", "gain", Ok("GraphicEqParams.gain_db (master gain, dB authored)")),
    ("GraphicEQ30Bands", "range", Err(NO_MEANING)),
    ("GraphicEQ30Bands", "amp1", Ok("GraphicEqParams.amps_db[0] (band 1 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp2", Ok("GraphicEqParams.amps_db[1] (band 2 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp3", Ok("GraphicEqParams.amps_db[2] (band 3 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp4", Ok("GraphicEqParams.amps_db[3] (band 4 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp5", Ok("GraphicEqParams.amps_db[4] (band 5 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp6", Ok("GraphicEqParams.amps_db[5] (band 6 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp7", Ok("GraphicEqParams.amps_db[6] (band 7 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp8", Ok("GraphicEqParams.amps_db[7] (band 8 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp9", Ok("GraphicEqParams.amps_db[8] (band 9 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp10", Ok("GraphicEqParams.amps_db[9] (band 10 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp11", Ok("GraphicEqParams.amps_db[10] (band 11 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp12", Ok("GraphicEqParams.amps_db[11] (band 12 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp13", Ok("GraphicEqParams.amps_db[12] (band 13 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp14", Ok("GraphicEqParams.amps_db[13] (band 14 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp15", Ok("GraphicEqParams.amps_db[14] (band 15 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp16", Ok("GraphicEqParams.amps_db[15] (band 16 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp17", Ok("GraphicEqParams.amps_db[16] (band 17 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp18", Ok("GraphicEqParams.amps_db[17] (band 18 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp19", Ok("GraphicEqParams.amps_db[18] (band 19 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp20", Ok("GraphicEqParams.amps_db[19] (band 20 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp21", Ok("GraphicEqParams.amps_db[20] (band 21 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp22", Ok("GraphicEqParams.amps_db[21] (band 22 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp23", Ok("GraphicEqParams.amps_db[22] (band 23 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp24", Ok("GraphicEqParams.amps_db[23] (band 24 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp25", Ok("GraphicEqParams.amps_db[24] (band 25 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp26", Ok("GraphicEqParams.amps_db[25] (band 26 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp27", Ok("GraphicEqParams.amps_db[26] (band 27 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp28", Ok("GraphicEqParams.amps_db[27] (band 28 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp29", Ok("GraphicEqParams.amps_db[28] (band 29 command, dB authored; band centre authored)")),
    ("GraphicEQ30Bands", "amp30", Ok("GraphicEqParams.amps_db[29] (band 30 command, dB authored; band centre authored)")),
];

/// One row of a spec audio parameter sheet as declared (STU-FX-136a): `None` is the sheet's `--`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpecParameterRow {
    pub parameter: &'static str,
    /// Normalised input slot, e.g. `in4`.
    pub slot: &'static str,
    pub hard_min: Option<f64>,
    pub hard_max: Option<f64>,
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
    /// The derived real default.
    pub default: Option<f64>,
    /// The stored normalised default.
    pub default_normalised: Option<f64>,
    pub unit: Option<&'static str>,
    pub precision: Option<u32>,
    /// The sheet's normalised-to-value expression, verbatim.
    pub mapping: &'static str,
}

impl SpecParameterRow {
    /// `value = normalised * (hard_max - hard_min) + hard_min` for an affine row with both bounds;
    /// `None` when a bound is absent.
    pub fn value_of(&self, normalised: f64) -> Option<f64> {
        Some(crate::dsp::normalised_to_value(normalised, self.hard_min?, self.hard_max?))
    }
}

/// An affine row with hard bounds; soft range, unit and precision absent (STU-FX-136b).
const fn affine(
    parameter: &'static str,
    slot: &'static str,
    hard_min: f64,
    hard_max: f64,
    default: f64,
    default_normalised: f64,
    mapping: &'static str,
) -> SpecParameterRow {
    SpecParameterRow {
        parameter,
        slot,
        hard_min: Some(hard_min),
        hard_max: Some(hard_max),
        soft_min: None,
        soft_max: None,
        default: Some(default),
        default_normalised: Some(default_normalised),
        unit: None,
        precision: None,
        mapping,
    }
}

const AMP_MAPPING: &str = "value = normalised * (amp_max - amp_min) + amp_min";

/// Sheet `GraphicEQ__Bands` as the `GraphicEQ10Bands` table (spec module 14, STU-FX-136a), row for row.
pub const GRAPHIC_EQ_10_BANDS_ROWS: [SpecParameterRow; 13] = [
    affine("accuracy", "in0", 10.0, 8192.0, 999.997454, 0.120997, "value = normalised * (accuracy_max - accuracy_min) + accuracy_min"),
    affine("gain", "in1", -64.0, 64.0, 0.0, 0.5, "value = normalised * (gain_max - gain_min) + gain_min"),
    affine("range", "in2", 1.5, 120.0, 47.808615, 0.39079, "value = normalised * (range_max - range_min) + range_min"),
    affine("amp1", "in4", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp2", "in5", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp3", "in6", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp4", "in7", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp5", "in8", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp6", "in9", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp7", "in10", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp8", "in11", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp9", "in12", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp10", "in13", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
];

/// Sheet `GraphicEQ__Bands` as the `GraphicEQ20Bands` table (spec module 14, STU-FX-136a), row for row.
pub const GRAPHIC_EQ_20_BANDS_ROWS: [SpecParameterRow; 23] = [
    affine("accuracy", "in0", 10.0, 8192.0, 999.997454, 0.120997, "value = normalised * (accuracy_max - accuracy_min) + accuracy_min"),
    affine("gain", "in1", -64.0, 64.0, 0.0, 0.5, "value = normalised * (gain_max - gain_min) + gain_min"),
    affine("range", "in2", 1.5, 120.0, 47.808615, 0.39079, "value = normalised * (range_max - range_min) + range_min"),
    affine("amp1", "in4", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp2", "in5", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp3", "in6", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp4", "in7", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp5", "in8", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp6", "in9", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp7", "in10", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp8", "in11", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp9", "in12", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp10", "in13", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp11", "in14", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp12", "in15", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp13", "in16", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp14", "in17", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp15", "in18", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp16", "in19", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp17", "in20", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp18", "in21", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp19", "in22", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp20", "in23", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
];

/// Sheet `GraphicEQ__Bands` as the `GraphicEQ30Bands` table (spec module 14, STU-FX-136a), row for row.
pub const GRAPHIC_EQ_30_BANDS_ROWS: [SpecParameterRow; 33] = [
    affine("accuracy", "in0", 10.0, 8192.0, 999.997454, 0.120997, "value = normalised * (accuracy_max - accuracy_min) + accuracy_min"),
    affine("gain", "in1", -64.0, 64.0, 0.0, 0.5, "value = normalised * (gain_max - gain_min) + gain_min"),
    affine("range", "in2", 1.5, 120.0, 47.808615, 0.39079, "value = normalised * (range_max - range_min) + range_min"),
    affine("amp1", "in4", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp2", "in5", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp3", "in6", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp4", "in7", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp5", "in8", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp6", "in9", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp7", "in10", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp8", "in11", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp9", "in12", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp10", "in13", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp11", "in14", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp12", "in15", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp13", "in16", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp14", "in17", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp15", "in18", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp16", "in19", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp17", "in20", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp18", "in21", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp19", "in22", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp20", "in23", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp21", "in24", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp22", "in25", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp23", "in26", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp24", "in27", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp25", "in28", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp26", "in29", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp27", "in30", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp28", "in31", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp29", "in32", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
    affine("amp30", "in33", -70.0, 70.0, 0.0, 0.5, AMP_MAPPING),
];

/// The contract table of spec effect `effect` (`GraphicEQ10Bands`, `GraphicEQ20Bands`,
/// `GraphicEQ30Bands`); `None` for an effect whose sheet is not reproduced here.
pub fn spec_parameter_rows(effect: &str) -> Option<&'static [SpecParameterRow]> {
    match effect {
        "GraphicEQ10Bands" => Some(&GRAPHIC_EQ_10_BANDS_ROWS),
        "GraphicEQ20Bands" => Some(&GRAPHIC_EQ_20_BANDS_ROWS),
        "GraphicEQ30Bands" => Some(&GRAPHIC_EQ_30_BANDS_ROWS),
        _ => None,
    }
}

/// The graphic equalizer sheet of a band count.
pub(crate) fn graphic_eq_rows(bands: usize) -> Option<&'static [SpecParameterRow]> {
    match bands {
        10 => Some(&GRAPHIC_EQ_10_BANDS_ROWS),
        20 => Some(&GRAPHIC_EQ_20_BANDS_ROWS),
        30 => Some(&GRAPHIC_EQ_30_BANDS_ROWS),
        _ => None,
    }
}

/// Studio field for spec row `(effect, parameter)`, or the typed refusal.
pub fn bind_spec_row(effect: &str, parameter: &str) -> Result<&'static str, SpecRowError> {
    let row = SPEC_ROWS
        .iter()
        .find(|(e, p, _)| *e == effect && *p == parameter)
        .ok_or(SpecRowError::UnknownRow)?;
    row.2.map_err(|reason| {
        SpecRowError::Undeclared(UndeclaredSemantics { effect: row.0, parameter: row.1, reason })
    })
}
