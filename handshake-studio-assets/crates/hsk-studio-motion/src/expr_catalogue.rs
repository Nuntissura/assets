//! Expression identifier catalogue (STU-MOT-073, 073a, 073b, 074, 075, 144; spec module
//! `14-studio-creative-suite.md` v02.225). The 333 identifiers of the category table (:27646-27657)
//! verbatim in their 12 rows, the declared dual spellings, the 37 recovered argument signatures with
//! their defaults (:27659-27701) and one typed binding per identifier. Pure metadata: no engine runs
//! here (MotionJS owns the JavaScript profile and binds what this table marks bound).
//!
//! Lookup is exact. A near spelling is `UnknownIdentifier`, never an alias: dual spellings are only
//! the pairs whose two spellings both appear in ONE row and are equal under the snake/camel rule
//! (`a_b_c` -> `aBC`); an alias is never guessed by removing underscores (STU-MOT-073a), so
//! `this_layer` is a member while `thisLayer` is not. An identifier whose semantics are neither fixed
//! by a mathematical definition, nor recorded in the spec text, nor measured by an oracle is
//! `Unavailable`, and asking for its binding returns `SourceFunctionBindingUnavailable`, never an
//! identity or a no-op (MT-33916 step 3; lead decision G1 2026-10-31: `ease`, `easeIn`, `easeOut`,
//! `random`, `gaussRandom`, `noise`, `wiggle` and the generator stay unavailable).
#![deny(clippy::arithmetic_side_effects)]

use std::sync::OnceLock;

/// Number of identifiers the spec declares (STU-MOT-073).
pub const EXPR_IDENTIFIER_COUNT: usize = 333;

/// The 12 declared rows of the category table, in table order. `Uncategorised` is the leading block
/// of 206 identifiers that carry no category header on disk (STU-MOT-073b); none is invented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExprCategory {
    Global,
    VectorMath,
    RandomNumbers,
    Interpolation,
    ColorConversion,
    OtherMath,
    SubObjects,
    General,
    Properties,
    ThreeD,
    SpaceTransforms,
    Uncategorised,
}

impl ExprCategory {
    pub const ALL: [Self; 12] = [
        Self::Global,
        Self::VectorMath,
        Self::RandomNumbers,
        Self::Interpolation,
        Self::ColorConversion,
        Self::OtherMath,
        Self::SubObjects,
        Self::General,
        Self::Properties,
        Self::ThreeD,
        Self::SpaceTransforms,
        Self::Uncategorised,
    ];

    /// The row's category cell exactly as the spec writes it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::VectorMath => "Vector Math",
            Self::RandomNumbers => "Random Numbers",
            Self::Interpolation => "Interpolation",
            Self::ColorConversion => "Color Conversion",
            Self::OtherMath => "Other Math",
            Self::SubObjects => "Sub-objects",
            Self::General => "General",
            Self::Properties => "Properties",
            Self::ThreeD => "3D",
            Self::SpaceTransforms => "Space Transforms",
            Self::Uncategorised => "(leading block, no category header on disk)",
        }
    }

    /// The row's count cell as the spec declares it (independent of the identifier list).
    pub const fn declared_count(self) -> usize {
        match self {
            Self::Global => 18,
            Self::VectorMath => 9,
            Self::RandomNumbers => 6,
            Self::Interpolation => 6,
            Self::ColorConversion => 5,
            Self::OtherMath => 4,
            Self::SubObjects => 5,
            Self::General => 16,
            Self::Properties => 9,
            Self::ThreeD => 19,
            Self::SpaceTransforms => 30,
            Self::Uncategorised => 206,
        }
    }

    /// The row's identifiers verbatim, in spec order.
    pub const fn identifiers(self) -> &'static [&'static str] {
        match self {
            Self::Global => &GLOBAL,
            Self::VectorMath => &VECTOR_MATH,
            Self::RandomNumbers => &RANDOM_NUMBERS,
            Self::Interpolation => &INTERPOLATION,
            Self::ColorConversion => &COLOR_CONVERSION,
            Self::OtherMath => &OTHER_MATH,
            Self::SubObjects => &SUB_OBJECTS,
            Self::General => &GENERAL,
            Self::Properties => &PROPERTIES,
            Self::ThreeD => &THREE_D,
            Self::SpaceTransforms => &SPACE_TRANSFORMS,
            Self::Uncategorised => &UNCATEGORISED,
        }
    }
}

/// Where a bound identifier's semantics come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExprProvenance {
    /// Measured against the named D5 After Effects oracle case.
    Oracle(&'static str),
    /// Fixed by its mathematical definition; argument shapes and refusals are the Studio reading
    /// (After Effects parity UNVERIFIED, D5).
    Mathematical,
    /// Semantics recorded in the spec text at the named anchor (the ES2018 profile intrinsics and the
    /// UTF-16 `localeCompare` of STU-MOT-255).
    Spec(&'static str),
    /// Studio-declared semantics with After Effects parity UNVERIFIED (D5). No identifier carries it in
    /// this revision: gate G1 was not confirmed.
    StudioDeclared,
}

impl ExprProvenance {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Oracle(_) => "oracle",
            Self::Mathematical => "mathematical",
            Self::Spec(_) => "spec",
            Self::StudioDeclared => "studio_declared",
        }
    }
}

/// Typed binding of one identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExprBinding {
    Bound(ExprProvenance),
    /// No authoritative signature/semantics: invocation is `SourceFunctionBindingUnavailable`.
    Unavailable,
}

/// Typed catalogue refusals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExprCatalogueError {
    /// Not one of the 333 spellings (exact, case-sensitive match).
    UnknownIdentifier,
    /// A member without an authoritative signature or semantics (MT-33916 step 3, MT-33917).
    SourceFunctionBindingUnavailable,
}

impl ExprCatalogueError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownIdentifier => "UNKNOWN_IDENTIFIER",
            Self::SourceFunctionBindingUnavailable => "SOURCE_FUNCTION_BINDING_UNAVAILABLE",
        }
    }
}

/// One recovered argument signature, verbatim (STU-MOT-075: the defaults are contract).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprSignature {
    /// Row number in the spec table (1..=37).
    pub number: u8,
    /// The signature text exactly as recorded.
    pub text: &'static str,
    /// The table's category column (`None` for `--`). Recorded as found: the spec attributes rows
    /// 18-37 to `Space Transforms` although several name other families.
    pub category: Option<&'static str>,
}

/// One parameter of a signature: its name and, when recorded, its default text (verbatim, unparsed:
/// some defaults are expressions such as `time + thisComp.displayStartTime` or prose such as
/// `start index`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprParam {
    pub name: &'static str,
    pub default: Option<&'static str>,
}

impl ExprSignature {
    /// Parameters split at the commas outside brackets and string quotes, each split at its first
    /// ` = `. The operator rows `*` and `+` yield one parameter named after the operator.
    pub fn params(&self) -> Vec<ExprParam> {
        let text = self.text;
        let mut out = Vec::new();
        let mut depth = 0_u32;
        let mut quoted = false;
        let mut start = 0_usize;
        let mut push = |part: &'static str| {
            let part = part.trim();
            let (name, default) = match part.split_once(" = ") {
                Some((name, default)) => (name.trim(), Some(default.trim())),
                None => (part, None),
            };
            out.push(ExprParam { name, default });
        };
        for (index, ch) in text.char_indices() {
            match ch {
                '"' => quoted = !quoted,
                '[' if !quoted => depth = depth.saturating_add(1),
                ']' if !quoted => depth = depth.saturating_sub(1),
                ',' if !quoted && depth == 0 => {
                    push(&text[start..index]);
                    start = index.saturating_add(1);
                }
                _ => {}
            }
        }
        push(&text[start..]);
        out
    }

    /// A parameter by name.
    pub fn param(&self, name: &str) -> Option<ExprParam> {
        self.params().into_iter().find(|p| p.name == name)
    }
}

/// One catalogue member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprIdentifier {
    /// The spelling exactly as the spec row writes it.
    pub name: &'static str,
    pub category: ExprCategory,
    /// Canonical write spelling (STU-MOT-144): the camelCase member of the declared pair for a
    /// snake_case spelling, otherwise `name` itself.
    pub canonical: &'static str,
    pub binding: ExprBinding,
    /// The recovered signature, attached only where the spec names the function (STU-MOT-075
    /// :27720-27724: `createPath` #2, `wiggle` #3, `temporalWiggle` #4, `loopOut` #5).
    pub signature: Option<&'static ExprSignature>,
}

/// Exact lookup of one spelling.
pub fn expr_identifier(name: &str) -> Result<ExprIdentifier, ExprCatalogueError> {
    table()
        .iter()
        .find(|entry| entry.name == name)
        .copied()
        .ok_or(ExprCatalogueError::UnknownIdentifier)
}

/// All 333 members in table order (row by row, spec order inside a row).
pub fn expr_identifiers() -> impl Iterator<Item = ExprIdentifier> {
    table().iter().copied()
}

/// The provenance of a bound identifier; `SourceFunctionBindingUnavailable` for a member without an
/// authoritative binding, `UnknownIdentifier` for a spelling outside the catalogue.
pub fn expr_binding(name: &str) -> Result<ExprProvenance, ExprCatalogueError> {
    match expr_identifier(name)?.binding {
        ExprBinding::Bound(provenance) => Ok(provenance),
        ExprBinding::Unavailable => Err(ExprCatalogueError::SourceFunctionBindingUnavailable),
    }
}

/// Canonical write spelling of an exact member (STU-MOT-073a/144).
pub fn canonical_spelling(name: &str) -> Result<&'static str, ExprCatalogueError> {
    Ok(expr_identifier(name)?.canonical)
}

/// The declared dual-spelling pairs `(snake_case, camelCase)`, in table order.
pub fn dual_spellings() -> impl Iterator<Item = (&'static str, &'static str)> {
    table()
        .iter()
        .filter(|entry| entry.canonical != entry.name)
        .map(|entry| (entry.name, entry.canonical))
}

/// `a_b_c` -> `aBC`; `None` without an underscore or with an empty segment.
fn snake_to_camel(name: &str) -> Option<String> {
    if !name.contains('_') {
        return None;
    }
    let mut segments = name.split('_');
    let mut out = String::from(segments.next().filter(|s| !s.is_empty())?);
    for segment in segments {
        let mut chars = segment.chars();
        let first = chars.next()?;
        out.push(first.to_ascii_uppercase());
        out.push_str(chars.as_str());
    }
    Some(out)
}

const SPEC_PROFILE: &str = "STU-MOT-255";

/// Binding table. Everything not listed is `Unavailable`.
fn binding_of(name: &str) -> ExprBinding {
    use ExprProvenance::{Mathematical, Oracle, Spec};
    let provenance = match name {
        // D5 oracle AE-03 (After Effects 26.5): `time` and `linear()`.
        "time" | "linear" => Oracle("AE-03"),
        // Vector Math (lookAt/look_at excluded: its Euler-angle convention is not fixed by a
        // mathematical definition nor recorded in the spec).
        "add" | "mul" | "clamp" | "dot" | "cross" | "normalize" | "length" => Mathematical,
        // Other Math.
        "degrees_to_radians" | "radians_to_degrees" | "degreesToRadians" | "radiansToDegrees" => Mathematical,
        // Color Conversion: HSL and hex-triplet notation.
        "rgb_to_hsl" | "hsl_to_rgb" | "rgbToHsl" | "hslToRgb" | "hexToRgb" => Mathematical,
        // ES2018 profile intrinsics (STU-MOT-255): Math functions, string and object members, and
        // `localeCompare` as deterministic UTF-16 lexical order.
        "cos" | "acos" | "tan" | "atan" | "atan2" | "sin" | "sqrt" | "exp" | "pow" | "log" | "abs"
        | "round" | "ceil" | "floor" | "min" | "max" | "asin" => Spec(SPEC_PROFILE),
        "valueOf" | "toString" | "toUpperCase" | "charAt" | "charCodeAt" | "fromCharCode" | "indexOf"
        | "lastIndexOf" | "replace" | "slice" | "split" | "substr" | "substring" | "toLowerCase"
        | "localeCompare" => Spec(SPEC_PROFILE),
        _ => return ExprBinding::Unavailable,
    };
    ExprBinding::Bound(provenance)
}

fn signature_number(name: &str) -> Option<u8> {
    match name {
        "createPath" => Some(2),
        "wiggle" => Some(3),
        "temporalWiggle" => Some(4),
        "loopOut" => Some(5),
        _ => None,
    }
}

fn table() -> &'static [ExprIdentifier] {
    static TABLE: OnceLock<Vec<ExprIdentifier>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out = Vec::with_capacity(EXPR_IDENTIFIER_COUNT);
        for category in ExprCategory::ALL {
            let row = category.identifiers();
            for &name in row {
                let canonical = snake_to_camel(name)
                    .and_then(|camel| row.iter().copied().find(|other| *other == camel))
                    .unwrap_or(name);
                let signature = signature_number(name)
                    .and_then(|number| EXPR_SIGNATURES.iter().find(|s| s.number == number));
                out.push(ExprIdentifier {
                    name,
                    category,
                    canonical,
                    binding: binding_of(name),
                    signature,
                });
            }
        }
        out
    })
}

/// Spec row `Global` (18).
const GLOBAL: [&str; 18] = [
    "comp", "footage", "this_comp", "this_layer", "this_property", "thisComp", "thisProject", "time", "color_depth",
    "colorDepth", "posterize_time", "posterizeTime", "timeToFrames", "framesToTime", "timeToTimecode",
    "timeToNTSCTimecode", "timeToFeetAndFrames", "timeToCurrentFormat",
];

/// Spec row `Vector Math` (9).
const VECTOR_MATH: [&str; 9] = [
    "add", "mul", "clamp", "dot", "cross", "normalize", "length", "look_at", "lookAt",
];

/// Spec row `Random Numbers` (6).
const RANDOM_NUMBERS: [&str; 6] = [
    "seed_random", "seedRandom", "random", "gauss_random", "gaussRandom", "noise",
];

/// Spec row `Interpolation` (6).
const INTERPOLATION: [&str; 6] = [
    "linear", "ease", "ease_in", "ease_out", "easeIn", "easeOut",
];

/// Spec row `Color Conversion` (5).
const COLOR_CONVERSION: [&str; 5] = [
    "rgb_to_hsl", "hsl_to_rgb", "rgbToHsl", "hslToRgb", "hexToRgb",
];

/// Spec row `Other Math` (4).
const OTHER_MATH: [&str; 4] = [
    "degrees_to_radians", "radians_to_degrees", "degreesToRadians", "radiansToDegrees",
];

/// Spec row `Sub-objects` (5).
const SUB_OBJECTS: [&str; 5] = [
    "source", "sourceTime", "sourceRectAtTime", "effect", "mask",
];

/// Spec row `General` (16).
const GENERAL: [&str; 16] = [
    "parent", "has_parent", "in_point", "out_point", "start_time", "has_video", "has_audio", "hasParent", "inPoint",
    "outPoint", "startTime", "hasVideo", "hasAudio", "audio_active", "audioActive", "sampleImage",
];

/// Spec row `Properties` (9).
const PROPERTIES: [&str; 9] = [
    "anchor_point", "anchorPoint", "scale", "rotation", "opacity", "audio_levels", "time_remap", "audioLevels",
    "timeRemap",
];

/// Spec row `3D` (19).
const THREE_D: [&str; 19] = [
    "orientation", "rotationX", "rotationY", "rotationZ", "castsShadows", "casts_shadows", "lightTransmission",
    "light_transmission", "acceptsShadows", "accepts_shadows", "acceptsLights", "accepts_lights", "ambient",
    "diffuse", "specularIntensity", "specular", "specularShininess", "shininess", "metal",
];

/// Spec row `Space Transforms` (30).
const SPACE_TRANSFORMS: [&str; 30] = [
    "to_comp", "from_comp", "to_world", "from_world", "to_comp_vec", "from_comp_vec", "to_world_vec",
    "from_world_vec", "from_comp_to_surface", "world_position_to_psd", "world_rotation_to_psd", "world_scale_to_psd",
    "toComp", "fromComp", "toWorld", "fromWorld", "toCompVec", "fromCompVec", "toWorldVec", "fromWorldVec",
    "fromCompToSurface", "vec", "framesPerSecond", "maxValOrArray", "valOrArray", "rgbaArray", "hslaArray",
    "hexString", "degrees", "radians",
];

/// Spec row `(leading block, no category header on disk)` (206).
const UNCATEGORISED: [&str; 206] = [
    "point_of_interest", "pointOfInterest", "zoom", "depthOfField", "focusDistance", "depth_of_field",
    "focus_distance", "aperture", "blur_level", "blurLevel", "irisShape", "irisRotation", "irisRoundness",
    "irisAspectRatio", "irisDiffractionFringe", "highlightGain", "highlightThreshold", "highlightSaturation",
    "intensity", "color", "cone_angle", "coneAngle", "cone_feather", "coneFeather", "shadow_darkness",
    "shadowDarkness", "shadow_diffusion", "shadowDiffusion", "param", "maskPath", "path", "maskOpacity",
    "maskFeather", "feather", "maskExpansion", "expansion", "invert", "points", "inTangents", "outTangents",
    "isClosed", "pointOnPath", "normalOnPath", "tangentOnPath", "createPath", "velocity", "velocity_at_time",
    "velocityAtTime", "speedAtTime", "wiggle", "temporal_wiggle", "temporalWiggle", "smooth", "loop_in", "loop_out",
    "loop_in_duration", "loop_out_duration", "loopIn", "loopOut", "loopInDuration", "loopOutDuration",
    "value_at_time", "valueAtTime", "speed", "speed_at_time", "key", "nearest_key", "num_keys", "nearestKey",
    "nextKey", "previousKey", "numKeys", ".index", "markerName", "valueOf", "toString", "toUpperCase", "charAt",
    "charCodeAt", "fromCharCode", "indexOf", "lastIndexOf", "match", "replace", "search", "slice", "split", "substr",
    "substring", "toLowerCase", "toLocaleLowerCase", "toLocaleUpperCase", "localeCompare", "layer", "marker",
    "num_layers", "numLayers", "layerByComment", "active_camera", "activeCamera", "width", "height", "duration",
    "frame_duration", "ntscDropFrame", "displayStartTime", "shutter_angle", "shutter_phase", "bg_color",
    "pixel_aspect", "frameDuration", "shutterAngle", "shutterPhase", "bgColor", "pixelAspect", "index", "comment",
    "sourceText", "sourceData", "dataValue", "dataKeyCount", "dataKeyTimes", "dataKeyValues", "dataPath", "chapter",
    "url", "frameTarget", "protectedRegion", "eventCuePoint", "cuePointName", "parameters", "sub", "div", "position",
    "Date", "fullPath", "linearBlending", "bitsPerChannel", ".name", "numEntries", "numProperties", "propertyGroup",
    "propertyIndex", "active", "enabled", "cos", "acos", "tan", "atan", "atan2", "sin", "sqrt", "exp", "pow", "log",
    "abs", "round", "ceil", "floor", "min", "max", "text", "items", "textAtTime", "createStyle", "getStyleAt",
    "setAllCaps", "setApplyFill", "setApplyStroke", "setAutoLeading", "setBaselineDirection", "setBaselineOption",
    "setBaselineShift", "setDigitSet", "setFauxBold", "setFauxItalic", "setFillColor", "setFont", "setFontSize",
    "setHorizontalScaling", "setKerning", "setKerningType", "setLeading", "setLigature", "setLineJoin",
    "replaceText", "setSmallCaps", "setStrokeColor", "setStrokeWidth", "setText", "EvalExpr", "setTracking",
    "setTsume", "setVerticalScaling", "setDirection", "setEveryLineComposer", "setFirstLineIndent",
    "setHangingRoman", "setJustification", "setLeadingType", "setLeftMargin", "setRightMargin", "setSpaceAfter",
    "setSpaceBefore", "asin", "BEEp_EvalExprEngines",
];

/// The 37 recovered signatures (:27664-27700), verbatim, with the table's category column (`--` = none).
pub const EXPR_SIGNATURES: [ExprSignature; 37] = [
    ExprSignature { number: 1, text: r#"percentage = 0.5, t = time"#, category: None },
    ExprSignature { number: 2, text: r#"points = [[0,0], [100,0], [100,100], [0,100]], inTangents = [], outTangents = [], isClosed = true"#, category: None },
    ExprSignature { number: 3, text: r#"freq, amp, octaves = 1, amp_mult = .5, t = time"#, category: None },
    ExprSignature { number: 4, text: r#"width = .2, samples = 5, t = time"#, category: None },
    ExprSignature { number: 5, text: r#"type = "cycle", numKeyframes = 0"#, category: None },
    ExprSignature { number: 6, text: r#"type = "cycle", duration = 0"#, category: None },
    ExprSignature { number: 7, text: r#"countUp = 1"#, category: None },
    ExprSignature { number: 8, text: r#"otherLayer, relIndex"#, category: None },
    ExprSignature { number: 9, text: r#"dataPath, t0 = startTime, t1 = endTime"#, category: None },
    ExprSignature { number: 10, text: r#"y, x"#, category: None },
    ExprSignature { number: 11, text: r#"value, exponent"#, category: None },
    ExprSignature { number: 12, text: r#"value1, value2"#, category: None },
    ExprSignature { number: 13, text: r#"charIndex, t = time"#, category: None },
    ExprSignature { number: 14, text: r#"value, s = start index, n = number of characters"#, category: None },
    ExprSignature { number: 15, text: r#"value, s"#, category: None },
    ExprSignature { number: 16, text: r#"*"#, category: None },
    ExprSignature { number: 17, text: r#"+"#, category: None },
    ExprSignature { number: 18, text: r#"vec1, vec2"#, category: Some("Space Transforms") },
    ExprSignature { number: 19, text: r#"vec, amount"#, category: Some("Space Transforms") },
    ExprSignature { number: 20, text: r#"value, limit1, limit2"#, category: Some("Space Transforms") },
    ExprSignature { number: 21, text: r#"point1, point2"#, category: Some("Space Transforms") },
    ExprSignature { number: 22, text: r#"fromPoint, atPoint"#, category: Some("Space Transforms") },
    ExprSignature { number: 23, text: r#"seed, timeless = false"#, category: Some("Space Transforms") },
    ExprSignature { number: 24, text: r#"minValOrArray, maxValOrArray"#, category: Some("Space Transforms") },
    ExprSignature { number: 25, text: r#"t, value1, value2"#, category: Some("Space Transforms") },
    ExprSignature { number: 26, text: r#"t, tMin, tMax, value1, value2"#, category: Some("Space Transforms") },
    ExprSignature { number: 27, text: r#"point, t = time"#, category: Some("Space Transforms") },
    ExprSignature { number: 28, text: r#"vec, t = time"#, category: Some("Space Transforms") },
    ExprSignature { number: 29, text: r#"point, radius = [.5, .5], postEffect = true, t = time"#, category: Some("Space Transforms") },
    ExprSignature { number: 30, text: r#"t = time + thisComp.displayStartTime, fps = 1.0 / thisComp.frameDuration, isDuration = false"#, category: Some("Space Transforms") },
    ExprSignature { number: 31, text: r#"frames, fps = 1.0 / thisComp.frameDuration"#, category: Some("Space Transforms") },
    ExprSignature { number: 32, text: r#"t = time + thisComp.displayStartTime, timecodeBase = 30, isDuration = false"#, category: Some("Space Transforms") },
    ExprSignature { number: 33, text: r#"t = time + thisComp.displayStartTime, ntscDropFrame = false, isDuration = false"#, category: Some("Space Transforms") },
    ExprSignature { number: 34, text: r#"t = time + thisComp.displayStartTime, fps = 1.0 / thisComp.frameDuration, framesPerFoot = 16, isDuration = false"#, category: Some("Space Transforms") },
    ExprSignature { number: 35, text: r#"t = time + thisComp.displayStartTime, fps = 1.0 / thisComp.frameDuration, isDuration = false, ntscDropFrame = thisComp.ntscDropFrame"#, category: Some("Space Transforms") },
    ExprSignature { number: 36, text: r#"t = time"#, category: Some("Space Transforms") },
    ExprSignature { number: 37, text: r#"t = time, includeExtents = false"#, category: Some("Space Transforms") },
];
