//! Lane SMB-R50-FOLLOWUPS-1 (R50 finding F-R50-1, lead decision G2: map, recovered first): the
//! recovered STU-FX-131a parameter records of Foam (`APC Foam`, spec v02.225 module 14 :15367, 47
//! rows, MT-32012) and Wave world (`APC Wave World`, :15433, 48 rows, MT-32013). The tables below are
//! the spec rows transcribed cell for cell (`#`, parameter, kind, hard_min, hard_max, soft_min,
//! soft_max, default, unit, precision, flags) with the enumerated option lines verbatim; every row is
//! one assertion of its seven fields kept separate (`--` stored as unknown, never mirrored from the
//! opposite bound), kind, options and flags (STU-FX-113: `CANNOT_TIME_VARY` -> `animatable = false`,
//! `CANNOT_INTERP` -> `interpolable = false`, `COLLAPSE_TWIRLY` -> `collapsed_by_default`). Reading
//! conventions under test (Folio's, UNVERIFIED, D5): a `--` unit is `unitless` (STU-FX-108); an
//! `ANGLE` row carries `--` in all four bound cells since v02.228 (SD-26: the old `0` cells were
//! bytes of another field, not a declared range): all four `None`, `unbounded_in_source`; its
//! default and precision are provisional. The engine mapping (recovered row -> engine scalar, hard
//! bounds enforced, default taken) is asserted for every mapped scalar; vendor parity UNVERIFIED (D5). Lane
//! SMB-R51-FOLLOWUPS-1 (R51 finding F-R51-1, spec v02.229 SD-27): a recovered row with the `unitless`
//! token drives no engine scalar until its scale is declared; the engine scalar of that quantity is
//! its authored `studio_declared` row beside the stored recovered row.
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use hsk_studio_composite::particles::{ParticleError, ParticleLimits};
use hsk_studio_composite::simulations::*;
use hsk_studio_folio::{MAX_STREAM_ABS_VALUE, StudioBoundState, StudioEffectParamSpec, StudioParameterKind, StudioUnitToken};

/// `(#, parameter, kind, hard_min, hard_max, soft_min, soft_max, default, unit, precision, flags)`,
/// verbatim from the spec table.
type Row = (u16, &'static str, &'static str, &'static str, &'static str, &'static str, &'static str, &'static str, &'static str, &'static str, &'static str);
/// `(parameter, default index, options)` of an `Enumerated options for` line, verbatim.
type Options = (&'static str, u16, &'static str);

const FOAM_TABLE: [Row; 47] = [
    (0, "_(unnamed)_", "LAYER", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (2, "View", "POPUP", "--", "--", "--", "--", "index 1", "--", "--", "--"),
    (4, "Producer Point", "POINT", "--", "--", "--", "--", "(50%, 50%) of layer", "--", "--", "--"),
    (5, "Producer", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY,COLLAPSE_TWIRLY"),
    (6, "Producer X Size", "FIX_SLIDER", "0", "0.449997", "0", "0.449997", "0.029999", "--", "3", "COLLAPSE_TWIRLY"),
    (8, "Producer Y Size", "FIX_SLIDER", "0", "0.449997", "0", "0.449997", "0.029999", "--", "3", "COLLAPSE_TWIRLY"),
    (10, "Producer Orientation", "ANGLE", "--", "--", "--", "--", "0", "degrees", "0", "COLLAPSE_TWIRLY"),
    (12, "_(unnamed)_", "CHECKBOX", "--", "--", "--", "--", "true", "--", "--", "--"),
    (14, "Production Rate", "FIX_SLIDER", "0", "100", "0", "10", "1", "--", "3", "COLLAPSE_TWIRLY"),
    (16, "Size", "FIX_SLIDER", "0.049988", "4", "0.049988", "4", "0.5", "--", "3", "COLLAPSE_TWIRLY"),
    (18, "Size Variance", "FIX_SLIDER", "0", "4", "0", "4", "0.5", "--", "3", "COLLAPSE_TWIRLY"),
    (19, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (20, "Lifespan", "FIX_SLIDER", "0", "1000", "0", "1000", "300", "--", "3", "COLLAPSE_TWIRLY"),
    (21, "Bubbles", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY,COLLAPSE_TWIRLY"),
    (22, "Bubble Growth Speed", "FIX_SLIDER", "0.009995", "1", "0.009995", "1", "0.099991", "--", "3", "COLLAPSE_TWIRLY"),
    (24, "Strength", "FIX_SLIDER", "0", "100", "0", "100", "10", "--", "3", "COLLAPSE_TWIRLY"),
    (26, "Initial Speed", "FIX_SLIDER", "0", "10", "0", "10", "0", "--", "3", "COLLAPSE_TWIRLY"),
    (28, "Initial Direction", "ANGLE", "--", "--", "--", "--", "0", "degrees", "0", "COLLAPSE_TWIRLY"),
    (30, "Wind Speed", "FIX_SLIDER", "0", "10", "0", "10", "0.5", "--", "3", "COLLAPSE_TWIRLY"),
    (32, "Wind Direction", "ANGLE", "--", "--", "--", "--", "0", "degrees", "0", "COLLAPSE_TWIRLY"),
    (33, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (34, "Turbulence", "FIX_SLIDER", "0", "2", "0", "2", "0.5", "--", "3", "COLLAPSE_TWIRLY"),
    (35, "Physics", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY,COLLAPSE_TWIRLY"),
    (36, "Wobble Amount", "FIX_SLIDER", "0", "0.399994", "0", "0.399994", "0.049988", "--", "3", "COLLAPSE_TWIRLY"),
    (38, "Repulsion", "FIX_SLIDER", "0", "1", "0", "1", "1", "--", "3", "COLLAPSE_TWIRLY"),
    (40, "Pop Velocity", "FIX_SLIDER", "0", "10", "0", "10", "0", "--", "3", "COLLAPSE_TWIRLY"),
    (42, "Viscosity", "FIX_SLIDER", "0", "4", "0", "4", "0.099991", "--", "3", "COLLAPSE_TWIRLY"),
    (44, "Stickiness", "FIX_SLIDER", "0", "4", "0", "4", "0.75", "--", "3", "COLLAPSE_TWIRLY"),
    (46, "Zoom", "FIX_SLIDER", "0.00499", "10", "0.00499", "5", "1", "--", "3", "COLLAPSE_TWIRLY"),
    (48, "Universe Size", "FIX_SLIDER", "0.099991", "50", "0.099991", "5", "1", "--", "3", "COLLAPSE_TWIRLY"),
    (50, "Blend Mode", "POPUP", "--", "--", "--", "--", "index 1", "--", "--", "--"),
    (52, "Bubble Texture", "POPUP", "--", "--", "--", "--", "index 3", "--", "--", "--"),
    (54, "Bubble Texture Layer", "LAYER", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (56, "Bubble Orientation", "POPUP", "--", "--", "--", "--", "index 1", "--", "--", "--"),
    (57, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (58, "Environment Map", "LAYER", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (60, "Reflection Strength", "FIX_SLIDER", "0", "1", "0", "1", "0", "--", "3", "COLLAPSE_TWIRLY"),
    (62, "Reflection Convergence", "FIX_SLIDER", "0", "1", "0", "1", "0.799988", "--", "3", "COLLAPSE_TWIRLY"),
    (63, "Rendering", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY,COLLAPSE_TWIRLY"),
    (64, "Flow Map", "LAYER", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (66, "Flow Map Steepness", "FIX_SLIDER", "0", "100", "0", "5", "5", "--", "3", "COLLAPSE_TWIRLY"),
    (68, "Flow Map Fits", "POPUP", "--", "--", "--", "--", "index 1", "--", "--", "--"),
    (70, "Simulation Quality", "POPUP", "--", "--", "--", "--", "index 1", "--", "--", "--"),
    (72, "Random Seed", "SLIDER", "0", "16", "0", "16", "1", "--", "--", "COLLAPSE_TWIRLY"),
    (79, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (81, "Flow Map", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY,COLLAPSE_TWIRLY"),
    (89, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
];
const FOAM_OPTIONS: [Options; 6] = [
    ("View", 1, "1=Draft; 2=Draft + Flow Map; 3=Rendered"),
    ("Blend Mode", 1, "1=Transparent; 2=Solid Old on top; 3=Solid New on top"),
    ("Bubble Texture", 3, "1=User Defined; 3=Default Bubble; 4=Amber Bock; 5=Water Beads; 6=Spit; 7=Cartoon Coffee; 8=Winter Stream; 9=Soda Water; 10=Orange Soda; 11=Nuclear Waste; 12=Red Tide; 13=Magma Marbles; 14=Sunset Foam; 15=Pepto; 16=Algae; 17=Blisters; 18=Bubble Wrap; 19=Grape Soda"),
    ("Bubble Orientation", 1, "1=Fixed; 2=Physical Orientation; 3=Bubble Velocity"),
    ("Flow Map Fits", 1, "1=Universe; 2=Screen"),
    ("Simulation Quality", 1, "1=Normal; 2=High; 3=Intense"),
];
const WAVE_WORLD_TABLE: [Row; 48] = [
    (0, "_(unnamed)_", "LAYER", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (2, "View", "POPUP", "--", "--", "--", "--", "index 2", "--", "--", "--"),
    (4, "Horizontal Rotation", "ANGLE", "--", "--", "--", "--", "0", "degrees", "0", "COLLAPSE_TWIRLY"),
    (5, "Wireframe Controls", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (6, "Vertical Rotation", "ANGLE", "--", "--", "--", "--", "0", "degrees", "0", "COLLAPSE_TWIRLY"),
    (8, "Vertical Scale", "FIX_SLIDER", "0", "1", "0", "1", "0.5", "--", "3", "COLLAPSE_TWIRLY"),
    (10, "Brightness", "FIX_SLIDER", "-5", "5", "0", "1", "0.5", "--", "3", "COLLAPSE_TWIRLY"),
    (12, "Contrast", "FIX_SLIDER", "0", "2", "0", "1", "0.25", "--", "3", "COLLAPSE_TWIRLY"),
    (13, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (14, "Gamma Adjustment", "FIX_SLIDER", "0.299988", "3", "0.299988", "3", "1", "--", "3", "COLLAPSE_TWIRLY"),
    (15, "Height Map Controls", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (16, "Render Dry Areas As", "POPUP", "--", "--", "--", "--", "index 1", "--", "--", "--"),
    (18, "Transparency", "FIX_SLIDER", "0", "1", "0", "0.099991", "0.009995", "--", "3", "COLLAPSE_TWIRLY"),
    (20, "Reflect Edges", "POPUP", "--", "--", "--", "--", "index 1", "--", "--", "--"),
    (24, "Pre-roll (seconds)", "FIX_SLIDER", "0", "60", "0", "5", "0", "--", "3", "COLLAPSE_TWIRLY"),
    (26, "Grid Resolution", "SLIDER", "10", "1000", "20", "300", "40", "--", "--", "COLLAPSE_TWIRLY"),
    (27, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (29, "Simulation", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (30, "Wave Speed", "FIX_SLIDER", "0.009995", "10", "0.009995", "1", "0.5", "--", "3", "COLLAPSE_TWIRLY"),
    (32, "Damping", "FIX_SLIDER", "0", "1", "0", "1", "0.009995", "--", "3", "COLLAPSE_TWIRLY"),
    (36, "Ground", "LAYER", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (38, "Steepness", "FIX_SLIDER", "0", "1", "0", "1", "0.25", "--", "3", "COLLAPSE_TWIRLY"),
    (40, "Height", "FIX_SLIDER", "0", "1", "0", "1", "0.299988", "--", "3", "COLLAPSE_TWIRLY"),
    (42, "Type", "POPUP", "--", "--", "--", "--", "index 1", "--", "--", "--"),
    (43, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (44, "Position", "POINT", "--", "--", "--", "--", "(50%, 50%) of layer", "--", "--", "--"),
    (45, "Ground", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (46, "Height/Length", "FIX_SLIDER", "0.000992", "100", "0.000992", "1", "0.099991", "--", "3", "COLLAPSE_TWIRLY"),
    (48, "Width", "FIX_SLIDER", "0.000992", "100", "0.000992", "1", "0.099991", "--", "3", "COLLAPSE_TWIRLY"),
    (50, "Angle", "ANGLE", "--", "--", "--", "--", "0", "degrees", "0", "COLLAPSE_TWIRLY"),
    (52, "Amplitude", "FIX_SLIDER", "0", "5", "0", "1", "0.5", "--", "3", "COLLAPSE_TWIRLY"),
    (54, "Frequency", "FIX_SLIDER", "0", "20", "0", "3", "1", "--", "3", "COLLAPSE_TWIRLY"),
    (55, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (56, "Phase", "ANGLE", "--", "--", "--", "--", "0", "degrees", "0", "COLLAPSE_TWIRLY"),
    (57, "Producer 1", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (58, "Type", "POPUP", "--", "--", "--", "--", "index 1", "--", "--", "--"),
    (60, "Position", "POINT", "--", "--", "--", "--", "(50%, 50%) of layer", "--", "--", "--"),
    (62, "Height/Length", "FIX_SLIDER", "0.000992", "100", "0.000992", "1", "0.5", "--", "3", "COLLAPSE_TWIRLY"),
    (64, "Width", "FIX_SLIDER", "0.000992", "100", "0.000992", "1", "0.5", "--", "3", "COLLAPSE_TWIRLY"),
    (66, "Angle", "ANGLE", "--", "--", "--", "--", "0", "degrees", "0", "COLLAPSE_TWIRLY"),
    (68, "Amplitude", "FIX_SLIDER", "0", "5", "0", "1", "0", "--", "3", "COLLAPSE_TWIRLY"),
    (70, "Frequency", "FIX_SLIDER", "0", "20", "0", "3", "1", "--", "3", "COLLAPSE_TWIRLY"),
    (72, "Phase", "ANGLE", "--", "--", "--", "--", "0", "degrees", "0", "COLLAPSE_TWIRLY"),
    (75, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (77, "Producer 2", "GROUP_START", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
    (90, "Wave Strength", "FIX_SLIDER", "0", "1", "0", "1", "0", "--", "3", "COLLAPSE_TWIRLY"),
    (92, "_(unnamed)_", "CHECKBOX", "--", "--", "--", "--", "true", "--", "--", "--"),
    (95, "_(unnamed)_", "GROUP_END", "--", "--", "--", "--", "--", "--", "--", "CANNOT_TIME_VARY"),
];
const WAVE_WORLD_OPTIONS: [Options; 5] = [
    ("View", 2, "1=Height Map; 2=Wireframe Preview"),
    ("Render Dry Areas As", 1, "1=Solid; 2=Transparent"),
    ("Reflect Edges", 1, "1=None; 2=All; 3=Left; 4=Top; 5=Right; 6=Bottom; 7=Left & Top; 8=Right & Left; 9=Right & Top; 10=Bottom & Left; 11=Bottom & Top; 12=Bottom & Right; 13=Right & Left & Top; 14=Bottom & Left & Top; 15=Bottom & Right & Left; 16=Bottom & Right & Top"),
    ("Type", 1, "1=Ring; 2=Line"),
    ("Type", 1, "1=Ring; 2=Line"),
];

fn cell(s: &str) -> Option<f64> {
    (s != "--").then(|| s.parse().unwrap_or_else(|_| panic!("number cell {s}")))
}

/// The expected stored default of a spec `default` cell (Folio conventions: point = layer
/// fractions, enumeration = the option index, boolean = 0 / 1, `--` = no default).
fn default_of(s: &str) -> Vec<f64> {
    match s {
        "--" => Vec::new(),
        "(50%, 50%) of layer" => vec![0.5, 0.5],
        "true" => vec![1.0],
        "false" => vec![0.0],
        _ => match s.strip_prefix("index ") {
            Some(i) => vec![i.parse().unwrap()],
            None => vec![s.parse().unwrap_or_else(|_| panic!("default cell {s}"))],
        },
    }
}

fn kind_of(s: &str) -> StudioParameterKind {
    match s {
        "FIX_SLIDER" | "SLIDER" => StudioParameterKind::Scalar,
        "ANGLE" => StudioParameterKind::Angle,
        "POPUP" => StudioParameterKind::Enumeration,
        "POINT" => StudioParameterKind::Point2d,
        "CHECKBOX" => StudioParameterKind::Boolean,
        "LAYER" => StudioParameterKind::LayerReference,
        "GROUP_START" | "GROUP_END" => StudioParameterKind::Group,
        _ => panic!("kind {s}"),
    }
}

fn options_of(text: &str) -> Vec<(u16, String)> {
    text.split("; ")
        .map(|p| {
            let (n, label) = p.split_once('=').unwrap();
            (n.parse().unwrap(), label.to_string())
        })
        .collect()
}

/// Every table row against the stored record: one row, one assertion block.
fn assert_record(kind: SimulationKind, table: &[Row], options: &[Options]) {
    let record = kind.recovered_record().expect("a recovered record");
    assert_eq!(record.len(), table.len(), "{kind:?}: every recovered row is stored, nothing else");
    let mut keys: Vec<&str> = record.iter().map(|r| r.param_key).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), record.len(), "{kind:?}: param keys are unique");
    let mut popups = options.iter();
    for (n, &(index, name, kind_cell, hmin, hmax, smin, smax, default, unit, precision, flags)) in table.iter().enumerate() {
        let r: &StudioEffectParamSpec = &record[n];
        let at = format!("{kind:?} row #{index} {name}");
        assert_eq!(r.param_index, index, "{at}: source order");
        assert_eq!(r.display_name, if name == "_(unnamed)_" { "(unnamed)" } else { name }, "{at}");
        assert!(!r.param_key.is_empty() && r.param_key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'), "{at}");
        assert_eq!(r.kind, kind_of(kind_cell), "{at}: kind");
        let bounds = [cell(hmin), cell(hmax), cell(smin), cell(smax)];
        let stored = [r.hard_min, r.hard_max, r.soft_min, r.soft_max];
        if kind_cell == "ANGLE" {
            // v02.228 (SD-26): every bound cell of an ANGLE row is `--` (the old `0` cells were
            // bytes read at the wrong offsets, never a declared range): four unknown bounds.
            assert_eq!(bounds, [None; 4], "{at}: an ANGLE row of these tables records `--` in all four bound cells");
            assert_eq!((stored, r.bound_state), ([None; 4], StudioBoundState::UnboundedInSource), "{at}: unbounded in source");
        } else {
            assert_eq!(stored, bounds, "{at}: hard and soft bounds stored separately, `--` unknown");
            let state = match bounds {
                [Some(_), Some(_), Some(_), Some(_)] => StudioBoundState::DeclaredBoth,
                [None, None, None, None] => StudioBoundState::UnboundedInSource,
                _ => panic!("{at}: partial bounds do not occur in these tables"),
            };
            assert_eq!(r.bound_state, state, "{at}: bound_state");
        }
        assert_eq!(r.default, default_of(default).as_slice(), "{at}: default");
        let want_unit = match unit {
            "--" => StudioUnitToken::Unitless,
            "degrees" => StudioUnitToken::Degrees,
            _ => panic!("{at}: unit {unit}"),
        };
        assert_eq!(r.unit, want_unit, "{at}: unit");
        assert_eq!(r.precision, (precision != "--").then(|| precision.parse().unwrap()), "{at}: precision");
        let flags: Vec<&str> = if flags == "--" { Vec::new() } else { flags.split(',').collect() };
        assert_eq!(r.animatable, !flags.contains(&"CANNOT_TIME_VARY"), "{at}: animatable");
        assert_eq!(r.interpolable, !flags.contains(&"CANNOT_INTERP"), "{at}: interpolable");
        assert_eq!(kind.collapsed_by_default(index), Some(flags.contains(&"COLLAPSE_TWIRLY")), "{at}: collapsed_by_default");
        if kind_cell == "POPUP" {
            let (oname, odefault, otext) = popups.next().expect("an options line per popup");
            assert_eq!(*oname, name, "{at}: options line order");
            assert_eq!(r.default, &[f64::from(*odefault)][..], "{at}: declared default index");
            let want = options_of(otext);
            let got: Vec<(u16, String)> = r.options.iter().map(|(i, l)| (*i, (*l).to_string())).collect();
            assert_eq!(got, want, "{at}: 1-based options, source gaps kept");
            // Every option index is legal, an index between or past them is not.
            for (i, _) in &want {
                assert!(r.accepts(&[f64::from(*i)]), "{at}: option {i}");
            }
            let max = want.iter().map(|(i, _)| *i).max().unwrap();
            assert!(!r.accepts(&[f64::from(max) + 1.0]) && !r.accepts(&[0.0]), "{at}: outside the options");
            for gap in (1..max).filter(|g| want.iter().all(|(i, _)| i != g)) {
                assert!(!r.accepts(&[f64::from(gap)]), "{at}: gap {gap} is not an option");
            }
        } else {
            assert!(r.options.is_empty(), "{at}: options only on enumerations");
        }
        // Validation is the record: inside the hard bounds accepted, outside refused; an unknown
        // bound does not constrain; value-less kinds store nothing.
        match r.kind {
            StudioParameterKind::Scalar => {
                let (lo, hi) = (r.hard_min.unwrap(), r.hard_max.unwrap());
                assert!(r.accepts(&[lo]) && r.accepts(&[hi]) && r.accepts(&r.default[..1]), "{at}");
                assert!(!r.accepts(&[lo.next_down()]) && !r.accepts(&[hi.next_up()]) && !r.accepts(&[f64::NAN]), "{at}");
            }
            StudioParameterKind::Angle => assert!(r.accepts(&[-720.0]) && r.accepts(&[1.0e6]) && !r.accepts(&[f64::INFINITY]), "{at}"),
            StudioParameterKind::LayerReference | StudioParameterKind::Group => {
                assert_eq!(r.kind.components(), 0, "{at}");
                assert!(!r.accepts(&[]) && !r.accepts(&[0.0]), "{at}: no stored value");
            }
            _ => {}
        }
    }
    assert!(popups.next().is_none(), "{kind:?}: every options line belongs to a popup");
    assert_eq!(kind.collapsed_by_default(1), None, "{kind:?}: no row #1");
}

#[test]
fn foam_recovered_record_is_stored_row_for_row() {
    assert_record(SimulationKind::Foam, &FOAM_TABLE, &FOAM_OPTIONS);
    // MT-31728 AC-002: the catalogue's typed parameter count.
    assert_eq!(SimulationKind::Foam.recovered_record().unwrap().len(), 47);
}

#[test]
fn wave_world_recovered_record_is_stored_row_for_row() {
    assert_record(SimulationKind::WaveWorld, &WAVE_WORLD_TABLE, &WAVE_WORLD_OPTIONS);
    // MT-31731 AC-002: the catalogue's typed parameter count.
    assert_eq!(SimulationKind::WaveWorld.recovered_record().unwrap().len(), 48);
}

#[test]
fn kinds_without_a_recovered_record_have_none() {
    for kind in SimulationKind::EVERY {
        let recovered = matches!(kind, SimulationKind::Foam | SimulationKind::WaveWorld);
        assert_eq!(kind.recovered_record().is_some(), recovered, "{kind:?}");
        assert_eq!(kind.recovered_mappings().is_empty(), !recovered, "{kind:?}");
        if !recovered {
            assert_eq!(kind.collapsed_by_default(0), None);
        }
    }
}

/// One mapped engine scalar: its default is the recovered default, its declared hard bounds are
/// enforced (an undeclared bound constrains only to finite values within Folio's ceiling), and it is
/// not also an authored row.
fn assert_mapping<T>(
    record: &'static [StudioEffectParamSpec],
    m: &RecoveredMapping,
    authored: &[ParamRow],
    obj: &mut T,
    get: fn(&T, &str) -> Option<f64>,
    set: fn(&mut T, &str, f64) -> Result<(), ParticleError>,
) {
    let row = record.iter().find(|r| r.param_index == m.param_index).expect("mapped row exists");
    assert!(authored.iter().all(|r| r.key != m.key), "{}: the authored row is gone", m.key);
    assert_eq!(get(obj, m.key), Some(row.default[m.component]), "{}: default from row #{}", m.key, m.param_index);
    let refused = Err(ParticleError::Parameter(m.key));
    match (row.hard_min, row.hard_max) {
        (Some(lo), Some(hi)) => {
            for v in [lo, hi] {
                assert_eq!(set(obj, m.key, v), Ok(()), "{} = {v}", m.key);
                assert_eq!(get(obj, m.key), Some(v));
            }
            for v in [lo.next_down(), hi.next_up()] {
                assert_eq!(set(obj, m.key, v), refused, "{} = {v}", m.key);
            }
        }
        (None, None) => {
            for v in [-MAX_STREAM_ABS_VALUE, MAX_STREAM_ABS_VALUE, 12_345.5] {
                assert_eq!(set(obj, m.key, v), Ok(()), "{} = {v}: no declared bound", m.key);
                assert_eq!(get(obj, m.key), Some(v));
            }
            assert_eq!(set(obj, m.key, MAX_STREAM_ABS_VALUE.next_up()), refused, "{}: Folio ceiling", m.key);
        }
        _ => panic!("{}: partial bounds do not occur", m.key),
    }
    for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(set(obj, m.key, v), refused, "{} = {v}", m.key);
    }
}

#[test]
fn mapped_engine_scalars_take_the_recovered_bounds_and_defaults() {
    // Foam: wind direction <- row 32 (ANGLE, unbounded in source).
    assert_eq!(foam::FOAM_MAPPINGS.map(|m| (m.key, m.param_index, m.component, m.unit)), [("wind_direction", 32, 0, "deg")]);
    for m in &foam::FOAM_MAPPINGS {
        let mut sim = Simulation::new(SimulationKind::Foam);
        assert_mapping(&foam::FOAM_RECORD, m, &foam::Foam::rows(), &mut sim, Simulation::param, Simulation::set_param);
        // A refused value leaves the record unchanged; validate() checks the same rows.
        let mut s = Simulation::new(SimulationKind::Foam);
        let before = s.clone();
        assert!(s.set_param(m.key, f64::NAN).is_err());
        assert_eq!(s, before);
        let mut f = foam::Foam { wind_direction: f64::INFINITY, ..foam::Foam::default() };
        assert_eq!(f.validate(), Err(ParticleError::Parameter("wind_direction")));
        f.wind_direction = 450.0;
        assert_eq!(f.validate(), Ok(()), "no declared bound: any finite direction");
    }
    // Wave world record: preroll <- row 24.
    assert_eq!(wave_world::WAVE_WORLD_MAPPINGS.map(|m| (m.key, m.param_index, m.component, m.unit)), [("preroll", 24, 0, "s")]);
    for m in &wave_world::WAVE_WORLD_MAPPINGS {
        let mut sim = Simulation::new(SimulationKind::WaveWorld);
        assert_mapping(&wave_world::WAVE_WORLD_RECORD, m, &wave_world::WAVE_WORLD_ROWS, &mut sim, Simulation::param, Simulation::set_param);
    }
    // Wave world producers: x, y <- row 44 (point, layer fractions), phase <- 56 (ANGLE, degrees).
    // Amplitude (row 52) and frequency (row 54) carry the `unitless` token and are not mapped (SD-27:
    // `unitless_recovered_rows_stay_beside_their_studio_declared_engine_scalars`).
    assert_eq!(
        wave_world::WAVE_PRODUCER_MAPPINGS.iter().map(|m| (m.key, m.param_index, m.component, m.unit)).collect::<Vec<_>>(),
        [("x", 44, 0, "ratio"), ("y", 44, 1, "ratio"), ("phase", 56, 0, "deg")]
    );
    for m in &wave_world::WAVE_PRODUCER_MAPPINGS {
        let mut p = WaveProducer::default();
        assert_mapping(&wave_world::WAVE_WORLD_RECORD, m, &wave_world::WAVE_PRODUCER_ROWS, &mut p, WaveProducer::param, WaveProducer::set_param);
        // Through the record: validate() walks every producer's recovered rows.
        let mut w = WaveWorld::default();
        w.producers[0].phase = f64::INFINITY;
        assert_eq!(w.validate(), Err(ParticleError::Parameter("phase")));
        w.producers[0].phase = 720.0;
        assert_eq!(w.validate(), Ok(()), "no declared bound: any finite phase");
    }
    // The default records validate and render with the recovered defaults.
    let cancel = CancellationToken::default();
    for kind in [SimulationKind::Foam, SimulationKind::WaveWorld] {
        let s = Simulation::new(kind);
        s.validate().expect("default record valid");
        s.render(32, 18, TICKS_PER_SECOND, TICKS_PER_SECOND / 30, &ParticleLimits::default(), &cancel).expect("default record renders");
    }
}

/// v02.228 (SD-26): an `ANGLE` row stores the full signed degrees, whole extra revolutions included
/// (725 is two turns plus 5 degrees), unchanged: nothing wraps or clamps on store.
#[test]
fn recovered_angle_rows_store_multi_turn_and_negative_degrees_unchanged() {
    for kind in [SimulationKind::Foam, SimulationKind::WaveWorld] {
        let record = kind.recovered_record().unwrap();
        let angles: Vec<&StudioEffectParamSpec> = record.iter().filter(|r| r.kind == StudioParameterKind::Angle || r.unit == StudioUnitToken::Degrees).collect();
        assert_eq!(angles.len(), if kind == SimulationKind::Foam { 3 } else { 6 }, "{kind:?}: the ANGLE rows of the table");
        for r in angles {
            let at = format!("{kind:?} row #{} {}", r.param_index, r.display_name);
            assert_eq!(r.kind, StudioParameterKind::Angle, "{at}");
            assert_eq!(([r.hard_min, r.hard_max, r.soft_min, r.soft_max], r.bound_state), ([None; 4], StudioBoundState::UnboundedInSource), "{at}");
            for v in [725.0, -725.0, 1440.5, 0.0] {
                assert!(r.accepts(&[v]), "{at}: {v}");
            }
        }
    }
    // Foam `Wind Direction` (row 32) through the record.
    let mut foam = Simulation::new(SimulationKind::Foam);
    for v in [725.0, -725.0, 1440.5, 0.0] {
        foam.set_param("wind_direction", v).unwrap();
        assert_eq!(foam.param("wind_direction"), Some(v), "stored as given");
        foam.validate().unwrap();
    }
    // Wave world producer `Phase` (row 56) through the record and through the world's producer.
    let mut p = WaveProducer::default();
    for v in [725.0, -725.0, 1440.5, 0.0] {
        p.set_param("phase", v).unwrap();
        assert_eq!(p.param("phase"), Some(v), "stored as given");
    }
    let mut world = Simulation::new(SimulationKind::WaveWorld);
    if let Simulation::WaveWorld(w) = &mut world {
        w.set_producer_param(0, "phase", -725.0).unwrap();
        assert_eq!(w.producers[0].param("phase"), Some(-725.0));
    } else {
        panic!("a wave world");
    }
    world.validate().unwrap();
    // The Folio ceiling and non-finite values stay refused, and a refusal changes nothing.
    let before = foam.clone();
    for v in [f64::NAN, f64::INFINITY, MAX_STREAM_ABS_VALUE.next_up()] {
        assert!(foam.set_param("wind_direction", v).is_err(), "{v}");
        assert_eq!(foam, before);
    }
}

/// One SD-27 pair: `(engine key, recovered param_index, authored unit, hard, soft, default)`; the
/// authored values are the rows as authored before lane SMB-R50-FOLLOWUPS-1 (63ecf5a6).
type Sd27 = (&'static str, u16, &'static str, (f64, f64), (Option<f64>, Option<f64>), f64);

/// The recovered row is stored (unit token `unitless`) and maps onto nothing; the engine scalar is
/// its authored `studio_declared` row: authored default, authored hard bounds enforced, and a value
/// past the recovered hard bound but inside the authored row accepted (the recovered row does not
/// constrain the engine).
fn assert_sd27<T>(
    record: &'static [StudioEffectParamSpec],
    maps: &[RecoveredMapping],
    authored: &[ParamRow],
    case: Sd27,
    obj: &mut T,
    get: fn(&T, &str) -> Option<f64>,
    set: fn(&mut T, &str, f64) -> Result<(), ParticleError>,
) {
    let (key, index, unit, hard, soft, default) = case;
    let rec = record.iter().find(|r| r.param_index == index).expect("recovered row stored");
    assert_eq!(rec.unit, StudioUnitToken::Unitless, "{key}: row #{index} carries the unitless token");
    assert!(maps.iter().all(|m| m.key != key && m.param_index != index), "{key}: row #{index} drives no engine scalar");
    let row = authored.iter().find(|r| r.key == key).expect("authored row present");
    assert_eq!(
        (row.unit, row.hard_min, row.hard_max, row.soft_min, row.soft_max, row.default, row.provenance),
        (unit, hard.0, hard.1, soft.0, soft.1, default, "studio_declared"),
        "{key}: the authored row"
    );
    assert_eq!(get(obj, key), Some(default), "{key}: authored default");
    let refused = Err(ParticleError::Parameter(key));
    for v in [hard.0, hard.1] {
        assert_eq!(set(obj, key, v), Ok(()), "{key} = {v}");
        assert_eq!(get(obj, key), Some(v));
    }
    for v in [hard.0.next_down(), hard.1.next_up(), f64::NAN, f64::INFINITY] {
        assert_eq!(set(obj, key, v), refused, "{key} = {v}");
    }
    let past = rec.hard_max.expect("recovered hard_max").next_up();
    assert!(past < hard.1, "{key}: the authored row is wider than the recovered row");
    assert_eq!(set(obj, key, past), Ok(()), "{key} = {past}: past the recovered bound, inside the authored row");
}

/// SD-27 (spec v02.229 STU-FX-131a reading note, STU-CMP-075b; R51 finding F-R51-1): a recovered
/// row whose unit cell is `--` is the `unitless` token, its engine scale is a SPEC GAP that is never
/// guessed, and until it is declared the engine's own scalar stays a `studio_declared` row under
/// STU-FX-146 beside the stored recovered row. Wave world `Wave Speed` (30), `Damping` (32),
/// `Amplitude` (52) and `Frequency` (54), Foam `Viscosity` (42).
#[test]
fn unitless_recovered_rows_stay_beside_their_studio_declared_engine_scalars() {
    let wave_record: [Sd27; 2] = [
        ("wave_speed", 30, "cells/s", (0.0, 1.0e4), (Some(1.0), Some(60.0)), 20.0),
        ("damping", 32, "1/s", (0.0, 1000.0), (Some(0.0), Some(5.0)), 0.5),
    ];
    for case in wave_record {
        let mut sim = Simulation::new(SimulationKind::WaveWorld);
        let maps = [wave_world::WAVE_WORLD_MAPPINGS.as_slice(), wave_world::WAVE_PRODUCER_MAPPINGS.as_slice()].concat();
        assert_sd27(&wave_world::WAVE_WORLD_RECORD, &maps, &wave_world::WAVE_WORLD_ROWS, case, &mut sim, Simulation::param, Simulation::set_param);
    }
    let wave_producer: [Sd27; 2] = [
        ("amplitude", 52, "height", (0.0, 1.0e6), (Some(0.0), Some(2.0)), 0.5),
        ("frequency", 54, "Hz", (0.0, 1000.0), (Some(0.0), Some(5.0)), 1.0),
    ];
    for case in wave_producer {
        let mut p = WaveProducer::default();
        let maps = [wave_world::WAVE_WORLD_MAPPINGS.as_slice(), wave_world::WAVE_PRODUCER_MAPPINGS.as_slice()].concat();
        assert_sd27(&wave_world::WAVE_WORLD_RECORD, &maps, &wave_world::WAVE_PRODUCER_ROWS, case, &mut p, WaveProducer::param, WaveProducer::set_param);
    }
    let foam: [Sd27; 1] = [("viscosity", 42, "1/s", (0.0, 1000.0), (Some(0.0), Some(10.0)), 1.0)];
    for case in foam {
        let mut sim = Simulation::new(SimulationKind::Foam);
        assert_sd27(&foam::FOAM_RECORD, &foam::FOAM_MAPPINGS, &foam::Foam::rows(), case, &mut sim, Simulation::param, Simulation::set_param);
    }
    // Through the record: validate() walks the authored producer rows, not the recovered rows 52/54.
    let mut w = WaveWorld::default();
    w.producers[0].amplitude = 5.0_f64.next_up();
    w.producers[0].frequency = 20.0_f64.next_up();
    assert_eq!(w.validate(), Ok(()), "past the recovered bounds, inside the authored rows");
    w.producers[0].amplitude = 1.0e6_f64.next_up();
    assert_eq!(w.validate(), Err(ParticleError::Parameter("amplitude")));
    w.producers[0].amplitude = 0.5;
    w.producers[0].frequency = 1000.0_f64.next_up();
    assert_eq!(w.validate(), Err(ParticleError::Parameter("frequency")));
}
