//! Binding of spec audio contract-table rows (STU-FX-134a) to Studio processor fields.
//!
//! A row is bound only when its meaning follows from the spec text (name, range and mapping) and
//! a Studio processor field implements it. Every other row of a covered effect is a typed
//! refusal ([`SpecRowError::Undeclared`]) naming why, instead of a guessed meaning. Rows of
//! effects not listed here are [`SpecRowError::UnknownRow`]. Units in the sheets are undeclared
//! (STU-FX-136b); where a bound field authors a unit it is named in the field string.

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
];

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
