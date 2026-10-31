//! Lane SMB-SETTINGS-SCHEMA-1 slice 3: export-stage settings entries (RF-09 phase 0). Drift (entry
//! default == stage `Default`, field for field; enum members == the stage's member strings) and
//! bounds inside the stage domain (every bound value passes `validate_export_stage`).
use hsk_studio_accord::{BoundSource, SettingConstraint, SettingEntry, SettingValue, SettingsSchema};
use hsk_studio_develop::export_stage::*;
use hsk_studio_develop::settings::*;

const RESIZE: [ResizeType; 6] =
    [ResizeType::WidthHeight, ResizeType::Dimensions, ResizeType::LongEdge, ResizeType::ShortEdge, ResizeType::Megapixels, ResizeType::Percentage];
const UNITS: [SizeUnit; 3] = [SizeUnit::Pixels, SizeUnit::Inches, SizeUnit::Centimetres];
const RES_UNITS: [ResolutionUnit; 2] = [ResolutionUnit::PixelsPerInch, ResolutionUnit::PixelsPerCm];
const MEDIA: [SharpenMedia; 3] = [SharpenMedia::Screen, SharpenMedia::Glossy, SharpenMedia::Matte];
const ANCHORS: [WatermarkAnchor; 9] = [
    WatermarkAnchor::TopLeft,
    WatermarkAnchor::Top,
    WatermarkAnchor::TopRight,
    WatermarkAnchor::Left,
    WatermarkAnchor::Centre,
    WatermarkAnchor::Right,
    WatermarkAnchor::BottomLeft,
    WatermarkAnchor::Bottom,
    WatermarkAnchor::BottomRight,
];
const ROTATIONS: [WatermarkRotation; 4] = [WatermarkRotation::R0, WatermarkRotation::R90, WatermarkRotation::R180, WatermarkRotation::R270];

/// Exhaustive matches: a new stage variant fails to compile here until the arrays above and the
/// entry members cover it.
#[allow(dead_code)]
fn variant_guard(r: ResizeType, u: SizeUnit, ru: ResolutionUnit, m: SharpenMedia, a: WatermarkAnchor, t: WatermarkRotation, s: WatermarkSize) {
    match r {
        ResizeType::WidthHeight | ResizeType::Dimensions | ResizeType::LongEdge | ResizeType::ShortEdge | ResizeType::Megapixels | ResizeType::Percentage => {}
    }
    match u {
        SizeUnit::Pixels | SizeUnit::Inches | SizeUnit::Centimetres => {}
    }
    match ru {
        ResolutionUnit::PixelsPerInch | ResolutionUnit::PixelsPerCm => {}
    }
    match m {
        SharpenMedia::Screen | SharpenMedia::Glossy | SharpenMedia::Matte => {}
    }
    match a {
        WatermarkAnchor::TopLeft
        | WatermarkAnchor::Top
        | WatermarkAnchor::TopRight
        | WatermarkAnchor::Left
        | WatermarkAnchor::Centre
        | WatermarkAnchor::Right
        | WatermarkAnchor::BottomLeft
        | WatermarkAnchor::Bottom
        | WatermarkAnchor::BottomRight => {}
    }
    match t {
        WatermarkRotation::R0 | WatermarkRotation::R90 | WatermarkRotation::R180 | WatermarkRotation::R270 => {}
    }
    match s {
        WatermarkSize::Proportional(_) | WatermarkSize::Fit | WatermarkSize::Fill => {}
    }
}

fn schema() -> SettingsSchema {
    SettingsSchema::new(&[EXPORT_SETTING_ENTRIES]).unwrap()
}

fn get(id: &str) -> &'static SettingEntry {
    EXPORT_SETTING_ENTRIES.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("{id}"))
}

fn members(id: &str) -> &'static [&'static str] {
    match get(id).constraint {
        SettingConstraint::Enum(m) => m,
        other => panic!("{id} is {other:?}"),
    }
}

fn graphic() -> WatermarkGraphic {
    WatermarkGraphic { width: 1, height: 1, rgba: vec![1.0, 1.0, 1.0, 0.5], transfer: MarkTransfer::Linear }
}

/// Sets entry `id` to `v` on `stage` (the mapping a host applies; panics on an unmapped pair).
fn apply(stage: &mut ExportStage, id: &str, v: SettingValue<'_>) {
    let pick = |all: &[&'static str], m: &str| all.iter().position(|x| *x == m).unwrap_or_else(|| panic!("{id} {m}"));
    let (sz, sh, wm) = (&mut stage.sizing, &mut stage.sharpening, &mut stage.watermark);
    match (id, v) {
        (CONSTRAIN_SIZE, SettingValue::Bool(b)) => sz.constrain_size = b,
        (RESIZE_TYPE, SettingValue::Str(m)) => sz.resize_type = RESIZE[pick(RESIZE_TYPE_MEMBERS, m)],
        (MAX_WIDTH, SettingValue::Float(x)) => sz.max_width = x,
        (MAX_HEIGHT, SettingValue::Float(x)) => sz.max_height = x,
        (SIZE_UNIT, SettingValue::Str(m)) => sz.size_unit = UNITS[pick(SIZE_UNIT_MEMBERS, m)],
        (RESIZE_PERCENTAGE, SettingValue::Float(x)) => sz.percentage = x,
        (MEGAPIXELS, SettingValue::Float(x)) => sz.megapixels = x,
        (RESOLUTION, SettingValue::Float(x)) => sz.resolution = x,
        (RESOLUTION_UNIT, SettingValue::Str(m)) => sz.resolution_unit = RES_UNITS[pick(RESOLUTION_UNIT_MEMBERS, m)],
        (DO_NOT_ENLARGE, SettingValue::Bool(b)) => sz.do_not_enlarge = b,
        (SHARPENING_ENABLED, SettingValue::Bool(b)) => sh.enabled = b,
        (SHARPENING_MEDIA, SettingValue::Str(m)) => sh.media = MEDIA[pick(SHARPEN_MEDIA_MEMBERS, m)],
        (SHARPENING_LEVEL, SettingValue::Int(l)) => sh.level = u8::try_from(l).unwrap(),
        (USE_WATERMARK, SettingValue::Bool(b)) => {
            wm.use_watermark = b;
            // The source is caller-supplied (no entry): a watermark that is on needs one.
            wm.source = if b { WatermarkSource::Graphic(graphic()) } else { WatermarkSource::None };
        }
        (WATERMARK_OPACITY, SettingValue::Float(x)) => wm.opacity_percent = x,
        (WATERMARK_SIZE, SettingValue::Str("proportional")) => wm.size = WatermarkSize::Proportional(25.0),
        (WATERMARK_SIZE, SettingValue::Str("fit")) => wm.size = WatermarkSize::Fit,
        (WATERMARK_SIZE, SettingValue::Str("fill")) => wm.size = WatermarkSize::Fill,
        (WATERMARK_SIZE_PERCENT, SettingValue::Float(x)) => wm.size = WatermarkSize::Proportional(x),
        (WATERMARK_ANCHOR, SettingValue::Str(m)) => wm.anchor = ANCHORS[pick(WATERMARK_ANCHOR_MEMBERS, m)],
        (WATERMARK_INSET_X, SettingValue::Float(x)) => wm.inset_x_percent = x,
        (WATERMARK_INSET_Y, SettingValue::Float(x)) => wm.inset_y_percent = x,
        (WATERMARK_ROTATION, SettingValue::Str(m)) => wm.rotation = ROTATIONS[pick(WATERMARK_ROTATION_MEMBERS, m)],
        other => panic!("unmapped export entry value {other:?}"),
    }
}

#[test]
fn export_entries_build_one_schema_and_members_are_the_stage_strings() {
    let s = schema();
    assert_eq!(s.entries().len(), 21);
    assert!(s.entries().iter().all(|e| e.module == MODULE && e.feature == FEATURE && e.id.starts_with("studio.develop.export.")));
    let tree = s.tree();
    assert_eq!((tree.len(), tree[0].module, tree[0].features.len(), tree[0].features[0].feature), (1, "develop", 1, "export"));
    // Every key the stage reads has an entry (watermark_size as rule + percent; the source none).
    for k in EXPORT_STAGE_KEYS.iter().filter(|k| k.key != "watermark_source") {
        let id = format!("studio.develop.export.{}", k.key.replace('_', "-"));
        assert!(s.get(&id).is_some(), "{} has no entry", k.key);
    }
    assert_eq!(RESIZE.map(ResizeType::spec_member).as_slice(), members(RESIZE_TYPE));
    assert_eq!(UNITS.map(SizeUnit::spec_member).as_slice(), members(SIZE_UNIT));
    assert_eq!(RES_UNITS.map(ResolutionUnit::spec_member).as_slice(), members(RESOLUTION_UNIT));
    assert_eq!(MEDIA.map(SharpenMedia::spec_member).as_slice(), members(SHARPENING_MEDIA));
    assert_eq!(ANCHORS.map(anchor_member).as_slice(), members(WATERMARK_ANCHOR));
    assert_eq!(ROTATIONS.map(rotation_member).as_slice(), members(WATERMARK_ROTATION));
    assert_eq!(ROTATIONS.map(|r| r.degrees().to_string()).as_slice(), members(WATERMARK_ROTATION));
    assert_eq!([WatermarkSize::Proportional(1.0), WatermarkSize::Fit, WatermarkSize::Fill].map(WatermarkSize::rule).as_slice(), members(WATERMARK_SIZE));
    // Spec-fixed domains carry bound_source spec; the Studio upper bounds are studio_declared.
    for id in [RESIZE_TYPE, SIZE_UNIT, RESOLUTION_UNIT, SHARPENING_MEDIA, SHARPENING_LEVEL, CONSTRAIN_SIZE, DO_NOT_ENLARGE, USE_WATERMARK] {
        assert_eq!(get(id).bound_source, BoundSource::Spec, "{id}");
    }
    for id in [MAX_WIDTH, MAX_HEIGHT, RESIZE_PERCENTAGE, MEGAPIXELS, RESOLUTION, WATERMARK_OPACITY, WATERMARK_SIZE_PERCENT] {
        assert_eq!(get(id).bound_source, BoundSource::StudioDeclared, "{id}");
    }
}

#[test]
fn entry_defaults_match_the_stage_defaults_field_for_field() {
    let OutputSizing {
        constrain_size,
        resize_type,
        max_width,
        max_height,
        size_unit,
        percentage,
        megapixels,
        resolution,
        resolution_unit,
        do_not_enlarge,
    } = OutputSizing::default();
    let d = |id| get(id).default;
    assert_eq!(d(CONSTRAIN_SIZE), SettingValue::Bool(constrain_size));
    assert_eq!(d(RESIZE_TYPE), SettingValue::Str(resize_type.spec_member()));
    assert_eq!(d(MAX_WIDTH), SettingValue::Float(max_width));
    assert_eq!(d(MAX_HEIGHT), SettingValue::Float(max_height));
    assert_eq!(d(SIZE_UNIT), SettingValue::Str(size_unit.spec_member()));
    assert_eq!(d(RESIZE_PERCENTAGE), SettingValue::Float(percentage));
    assert_eq!(d(MEGAPIXELS), SettingValue::Float(megapixels));
    assert_eq!(d(RESOLUTION), SettingValue::Float(resolution));
    assert_eq!(d(RESOLUTION_UNIT), SettingValue::Str(resolution_unit.spec_member()));
    assert_eq!(d(DO_NOT_ENLARGE), SettingValue::Bool(do_not_enlarge));
    let OutputSharpening { enabled, media, level } = OutputSharpening::default();
    assert_eq!(d(SHARPENING_ENABLED), SettingValue::Bool(enabled));
    assert_eq!(d(SHARPENING_MEDIA), SettingValue::Str(media.spec_member()));
    assert_eq!(d(SHARPENING_LEVEL), SettingValue::Int(i64::from(level)));
    let WatermarkSettings { use_watermark, source, opacity_percent, size, anchor, inset_x_percent, inset_y_percent, rotation } =
        WatermarkSettings::default();
    assert_eq!(source, WatermarkSource::None, "the source has no entry");
    assert_eq!(d(USE_WATERMARK), SettingValue::Bool(use_watermark));
    assert_eq!(d(WATERMARK_OPACITY), SettingValue::Float(opacity_percent));
    assert_eq!(d(WATERMARK_SIZE), SettingValue::Str(size.rule()));
    assert_eq!(size, WatermarkSize::Proportional(25.0));
    assert_eq!(d(WATERMARK_SIZE_PERCENT), SettingValue::Float(25.0));
    assert_eq!(d(WATERMARK_ANCHOR), SettingValue::Str(anchor_member(anchor)));
    assert_eq!(d(WATERMARK_INSET_X), SettingValue::Float(inset_x_percent));
    assert_eq!(d(WATERMARK_INSET_Y), SettingValue::Float(inset_y_percent));
    assert_eq!(d(WATERMARK_ROTATION), SettingValue::Str(rotation_member(rotation)));
    // The value projections of the default stage are the entry defaults, every entry covered.
    let stage = ExportStage::default();
    let mut values: Vec<_> = sizing_values(&stage.sizing).to_vec();
    values.extend(sharpening_values(&stage.sharpening));
    values.extend(watermark_values(&stage.watermark));
    assert_eq!(values.len(), EXPORT_SETTING_ENTRIES.len());
    for (e, (id, v)) in EXPORT_SETTING_ENTRIES.iter().zip(values) {
        assert_eq!((e.id, e.default), (id, v));
    }
    assert!(watermark_values(&WatermarkSettings { size: WatermarkSize::Fit, ..WatermarkSettings::default() }).iter().all(|(id, _)| *id != WATERMARK_SIZE_PERCENT));
}

#[test]
fn every_bound_value_passes_validate_export_stage() {
    let s = schema();
    let mut fed = 0;
    for e in EXPORT_SETTING_ENTRIES {
        let values: Vec<SettingValue<'static>> = match e.constraint {
            SettingConstraint::Bool => vec![SettingValue::Bool(false), SettingValue::Bool(true)],
            SettingConstraint::IntRange { min, max } => vec![SettingValue::Int(min), SettingValue::Int(max)],
            SettingConstraint::FloatRange { min, max } => vec![SettingValue::Float(min), SettingValue::Float(max)],
            SettingConstraint::Enum(m) => m.iter().map(|x| SettingValue::Str(x)).collect(),
        };
        for v in values {
            s.validate(e.id, &v).unwrap();
            let mut stage = ExportStage::default();
            apply(&mut stage, e.id, v);
            validate_export_stage(&stage).unwrap_or_else(|err| panic!("{} {v:?}: {}", e.id, err.code()));
            fed += 1;
        }
    }
    assert!(fed >= 2 * EXPORT_SETTING_ENTRIES.len(), "{fed}");
}

#[test]
fn resolution_maximum_keeps_every_sharpening_kernel_inside_the_stage_limit() {
    let SettingConstraint::FloatRange { min, max } = get(RESOLUTION).constraint else { panic!("resolution is a float range") };
    for resolution in [min, max] {
        for unit in RES_UNITS {
            for media in MEDIA {
                for level in 1..=3u8 {
                    let mut stage = ExportStage::default();
                    stage.sizing.resolution = resolution;
                    stage.sizing.resolution_unit = unit;
                    stage.sharpening = OutputSharpening { enabled: true, media, level };
                    let row = validate_export_stage(&stage).unwrap_or_else(|e| panic!("{resolution} {unit:?} {media:?} {level}: {}", e.code())).unwrap();
                    assert!(row.sigma_px <= MAX_SHARPEN_SIGMA_PX);
                }
            }
        }
    }
    // Just past the widest case the stage refuses: the bound is the stage's, not a guess.
    let mut stage = ExportStage::default();
    stage.sizing.resolution = 4032.0;
    stage.sizing.resolution_unit = ResolutionUnit::PixelsPerCm;
    stage.sharpening = OutputSharpening { enabled: true, media: SharpenMedia::Matte, level: 2 };
    assert_eq!(validate_export_stage(&stage).unwrap_err().code(), "export_setting_out_of_range");
    let s = schema();
    assert_eq!(s.validate(RESOLUTION, &SettingValue::Float(4032.0)).unwrap_err().code(), "out_of_range");
    for (id, v, code) in [
        (MAX_WIDTH, SettingValue::Float(0.5), "out_of_range"),
        (MEGAPIXELS, SettingValue::Float(0.0), "out_of_range"),
        (SHARPENING_LEVEL, SettingValue::Int(4), "out_of_range"),
        (SHARPENING_LEVEL, SettingValue::Float(2.0), "not_int"),
        (WATERMARK_SIZE_PERCENT, SettingValue::Float(0.0), "out_of_range"),
        (RESIZE_TYPE, SettingValue::Str("widthheight"), "unknown_enum_member"),
        (WATERMARK_ROTATION, SettingValue::Str("45"), "unknown_enum_member"),
        (WATERMARK_OPACITY, SettingValue::Float(f64::INFINITY), "not_finite"),
    ] {
        assert_eq!(s.validate(id, &v).unwrap_err().code(), code, "{id} {v:?}");
    }
}
