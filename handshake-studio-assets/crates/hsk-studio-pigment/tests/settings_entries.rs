//! Lane SMB-SETTINGS-SCHEMA-1 slice 2: selection-tool settings entries (RF-09 phase 0). Drift
//! (entry default == option `Default`, field for field) and bounds inside the engine domain (every
//! bound value is fed to the tool and accepted).
use hsk_studio_accord::{SettingConstraint, SettingEntry, SettingValue, SettingsSchema};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::select_tools::ToolLimits;
use hsk_studio_pigment::select_tools::magnetic::{MagneticOptions, magnetic_lasso};
use hsk_studio_pigment::select_tools::wand::{Connectivity, WandOptions, magic_wand};
use hsk_studio_pigment::selection::{ColourView, SelectionContext};
use hsk_studio_pigment::settings::*;

const CANVAS: Rect = Rect { x: 0, y: 0, width: 32, height: 32 };

/// A 32 x 32 image: a red 12 x 12 square at (10, 10) on blue, or all blue when `flat`.
fn image(flat: bool) -> (Vec<u8>, Vec<u8>) {
    let (mut colour, mut alpha) = (Vec::new(), Vec::new());
    for y in 0..32i64 {
        for x in 0..32i64 {
            let inside = !flat && (10..22).contains(&x) && (10..22).contains(&y);
            for c in if inside { [1.0f32, 0.0, 0.0] } else { [0.0, 0.0, 1.0] } {
                colour.extend_from_slice(&c.to_le_bytes());
            }
            alpha.extend_from_slice(&1.0f32.to_le_bytes());
        }
    }
    (colour, alpha)
}

fn schema() -> SettingsSchema {
    SettingsSchema::new(&[SELECTION_SETTING_ENTRIES]).unwrap()
}

fn default_of(id: &str) -> SettingValue<'static> {
    SELECTION_SETTING_ENTRIES.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("{id}")).default
}

/// Every value an entry admits at its edges: min and max, both booleans, every enum member.
fn bound_values(e: &SettingEntry) -> Vec<SettingValue<'static>> {
    match e.constraint {
        SettingConstraint::Bool => vec![SettingValue::Bool(false), SettingValue::Bool(true)],
        SettingConstraint::IntRange { min, max } => vec![SettingValue::Int(min), SettingValue::Int(max)],
        SettingConstraint::FloatRange { min, max } => vec![SettingValue::Float(min), SettingValue::Float(max)],
        SettingConstraint::Enum(members) => members.iter().map(|m| SettingValue::Str(m)).collect(),
    }
}

#[test]
fn selection_entries_build_one_schema_under_module_pigment_feature_selection() {
    let s = schema();
    assert_eq!(s.entries().len(), 10);
    assert!(s.entries().iter().all(|e| e.module == MODULE && e.feature == FEATURE && e.id.starts_with("studio.pigment.selection.")));
    let tree = s.tree();
    assert_eq!(tree.len(), 1);
    assert_eq!((tree[0].module, tree[0].features.len(), tree[0].features[0].feature), ("pigment", 1, "selection"));
    // Values outside the entry bounds are refused by the schema before they reach a tool.
    for (id, v, code) in [
        (WAND_TOLERANCE, SettingValue::Float(255.5), "out_of_range"),
        (WAND_TOLERANCE, SettingValue::Float(-1.0), "out_of_range"),
        (WAND_TOLERANCE, SettingValue::Float(f64::NAN), "not_finite"),
        (WAND_SAMPLE_SIZE, SettingValue::Str("2"), "unknown_enum_member"),
        (WAND_SAMPLE_SIZE, SettingValue::Int(3), "not_string"),
        (WAND_CONNECTIVITY, SettingValue::Str("Four"), "unknown_enum_member"),
        (MAGNETIC_FREQUENCY, SettingValue::Float(0.0), "out_of_range"),
        (MAGNETIC_MARGIN, SettingValue::Int(-1), "out_of_range"),
        (MAGNETIC_MARGIN, SettingValue::Float(16.0), "not_int"),
    ] {
        assert_eq!(s.validate(id, &v).unwrap_err().code(), code, "{id} {v:?}");
    }
}

#[test]
fn entry_defaults_match_the_option_defaults_field_for_field() {
    // Exhaustive destructuring: a new option field fails to compile here until it gets an entry.
    let WandOptions { tolerance, contiguous, sample_merged, sample_size, connectivity } = WandOptions::default();
    assert_eq!(default_of(WAND_TOLERANCE), SettingValue::Float(tolerance));
    assert_eq!(default_of(WAND_CONTIGUOUS), SettingValue::Bool(contiguous));
    assert_eq!(default_of(WAND_SAMPLE_MERGED), SettingValue::Bool(sample_merged));
    assert_eq!(default_of(WAND_SAMPLE_SIZE), SettingValue::Str(&sample_size.to_string()));
    assert_eq!(default_of(WAND_CONNECTIVITY), SettingValue::Str(connectivity_member(connectivity)));
    let MagneticOptions { width, contrast, frequency, sample_merged, margin_px } = MagneticOptions::default();
    assert_eq!(default_of(MAGNETIC_WIDTH), SettingValue::Float(width));
    assert_eq!(default_of(MAGNETIC_CONTRAST), SettingValue::Float(contrast));
    assert_eq!(default_of(MAGNETIC_FREQUENCY), SettingValue::Float(frequency));
    assert_eq!(default_of(MAGNETIC_SAMPLE_MERGED), SettingValue::Bool(sample_merged));
    assert_eq!(default_of(MAGNETIC_MARGIN), SettingValue::Int(i64::from(margin_px)));
    // The value projections of the defaults are the entry defaults and validate.
    let s = schema();
    for (id, v) in wand_values(&WandOptions::default()).into_iter().chain(magnetic_values(&MagneticOptions::default())) {
        assert_eq!(default_of(id), v, "{id}");
        s.validate(id, &v).unwrap();
    }
    assert_eq!(connectivity_member(Connectivity::Eight), "eight");
    let even = WandOptions { sample_size: 2, ..WandOptions::default() };
    assert_eq!(s.validate(WAND_SAMPLE_SIZE, &wand_values(&even)[3].1).unwrap_err().code(), "unknown_enum_member");
}

#[test]
fn every_wand_bound_value_is_accepted_by_the_magic_wand() {
    let (colour, alpha) = image(false);
    let layers = [ColourView { bounds: CANVAS, colour: &colour, alpha: &alpha }];
    let ctx = SelectionContext::new(CANVAS);
    let lim = ToolLimits::default();
    let mut fed = 0;
    for e in SELECTION_SETTING_ENTRIES.iter().filter(|e| e.id.contains(".wand-")) {
        for v in bound_values(e) {
            schema().validate(e.id, &v).unwrap();
            let mut o = WandOptions::default();
            match (e.id, v) {
                (WAND_TOLERANCE, SettingValue::Float(t)) => o.tolerance = t,
                (WAND_CONTIGUOUS, SettingValue::Bool(b)) => o.contiguous = b,
                (WAND_SAMPLE_MERGED, SettingValue::Bool(b)) => o.sample_merged = b,
                (WAND_SAMPLE_SIZE, SettingValue::Str(m)) => o.sample_size = m.parse().unwrap(),
                (WAND_CONNECTIVITY, SettingValue::Str("four")) => o.connectivity = Connectivity::Four,
                (WAND_CONNECTIVITY, SettingValue::Str("eight")) => o.connectivity = Connectivity::Eight,
                other => panic!("unmapped wand entry value {other:?}"),
            }
            let plane = magic_wand(&layers, 0, (15, 15), &o, &ctx, &lim).unwrap_or_else(|err| panic!("{} {v:?}: {}", e.id, err.code()));
            assert!(!plane.data.is_empty(), "{} {v:?}", e.id);
            fed += 1;
        }
    }
    assert_eq!(fed, 2 + 2 + 2 + WAND_SAMPLE_SIZE_MEMBERS.len() + WAND_CONNECTIVITY_MEMBERS.len());
}

#[test]
fn every_magnetic_bound_value_is_accepted_by_the_magnetic_lasso() {
    // A flat image has no edge, so no sample snaps and the anchors are exactly the trace's: the
    // outcome depends on the option values only, never on where an edge pulls the wire.
    let (colour, alpha) = image(true);
    let layers = [ColourView { bounds: CANVAS, colour: &colour, alpha: &alpha }];
    let ctx = SelectionContext::new(CANVAS);
    let lim = ToolLimits::default();
    // 130 samples 5 px apart around a 15 px square (perimeter 60, length 645): at frequency 1 an
    // anchor every 200 px lands at perimeter 0, 20 and 40, three distinct anchors.
    let corners = [[8.0, 8.0], [23.0, 8.0], [23.0, 23.0], [8.0, 23.0]];
    let trace: Vec<[f64; 2]> = (0..130usize)
        .map(|i| {
            let (side, step) = ((i / 3) % 4, (i % 3) as f64 * 5.0);
            let (a, b) = (corners[side], corners[(side + 1) % 4]);
            [a[0] + (b[0] - a[0]) * step / 15.0, a[1] + (b[1] - a[1]) * step / 15.0]
        })
        .collect();
    let mut fed = 0;
    for e in SELECTION_SETTING_ENTRIES.iter().filter(|e| e.id.contains(".magnetic-")) {
        for v in bound_values(e) {
            schema().validate(e.id, &v).unwrap();
            let mut o = MagneticOptions::default();
            match (e.id, v) {
                (MAGNETIC_WIDTH, SettingValue::Float(x)) => o.width = x,
                (MAGNETIC_CONTRAST, SettingValue::Float(x)) => o.contrast = x,
                (MAGNETIC_FREQUENCY, SettingValue::Float(x)) => o.frequency = x,
                (MAGNETIC_SAMPLE_MERGED, SettingValue::Bool(b)) => o.sample_merged = b,
                (MAGNETIC_MARGIN, SettingValue::Int(m)) => o.margin_px = u32::try_from(m).unwrap(),
                other => panic!("unmapped magnetic entry value {other:?}"),
            }
            magnetic_lasso(&layers, 0, &trace, &o, &ctx, &lim).unwrap_or_else(|err| panic!("{} {v:?}: {}", e.id, err.code()));
            fed += 1;
        }
    }
    assert_eq!(fed, 10);
}
