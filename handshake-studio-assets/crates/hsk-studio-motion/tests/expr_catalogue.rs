//! Expression identifier catalogue (lane SMB-AE-EXPR-LIBRARY-1; STU-MOT-073, 073a, 073b, 075, 144).
//! Expectations are the spec table of `14-studio-creative-suite.md` v02.225 (:27646-27657 rows,
//! :27664-27700 signatures) copied verbatim below; module names follow the MT ids they serve.
use hsk_studio_motion::*;

/// The spec rows (:27646-27657) as `(category, count cell, identifiers joined by single spaces)`.
const SPEC_ROWS: [(ExprCategory, usize, &str); 12] = [
    (ExprCategory::Global, 18, "comp footage this_comp this_layer this_property thisComp thisProject time color_depth colorDepth posterize_time posterizeTime timeToFrames framesToTime timeToTimecode timeToNTSCTimecode timeToFeetAndFrames timeToCurrentFormat"),
    (ExprCategory::VectorMath, 9, "add mul clamp dot cross normalize length look_at lookAt"),
    (ExprCategory::RandomNumbers, 6, "seed_random seedRandom random gauss_random gaussRandom noise"),
    (ExprCategory::Interpolation, 6, "linear ease ease_in ease_out easeIn easeOut"),
    (ExprCategory::ColorConversion, 5, "rgb_to_hsl hsl_to_rgb rgbToHsl hslToRgb hexToRgb"),
    (ExprCategory::OtherMath, 4, "degrees_to_radians radians_to_degrees degreesToRadians radiansToDegrees"),
    (ExprCategory::SubObjects, 5, "source sourceTime sourceRectAtTime effect mask"),
    (ExprCategory::General, 16, "parent has_parent in_point out_point start_time has_video has_audio hasParent inPoint outPoint startTime hasVideo hasAudio audio_active audioActive sampleImage"),
    (ExprCategory::Properties, 9, "anchor_point anchorPoint scale rotation opacity audio_levels time_remap audioLevels timeRemap"),
    (ExprCategory::ThreeD, 19, "orientation rotationX rotationY rotationZ castsShadows casts_shadows lightTransmission light_transmission acceptsShadows accepts_shadows acceptsLights accepts_lights ambient diffuse specularIntensity specular specularShininess shininess metal"),
    (ExprCategory::SpaceTransforms, 30, "to_comp from_comp to_world from_world to_comp_vec from_comp_vec to_world_vec from_world_vec from_comp_to_surface world_position_to_psd world_rotation_to_psd world_scale_to_psd toComp fromComp toWorld fromWorld toCompVec fromCompVec toWorldVec fromWorldVec fromCompToSurface vec framesPerSecond maxValOrArray valOrArray rgbaArray hslaArray hexString degrees radians"),
    (ExprCategory::Uncategorised, 206, "point_of_interest pointOfInterest zoom depthOfField focusDistance depth_of_field focus_distance aperture blur_level blurLevel irisShape irisRotation irisRoundness irisAspectRatio irisDiffractionFringe highlightGain highlightThreshold highlightSaturation intensity color cone_angle coneAngle cone_feather coneFeather shadow_darkness shadowDarkness shadow_diffusion shadowDiffusion param maskPath path maskOpacity maskFeather feather maskExpansion expansion invert points inTangents outTangents isClosed pointOnPath normalOnPath tangentOnPath createPath velocity velocity_at_time velocityAtTime speedAtTime wiggle temporal_wiggle temporalWiggle smooth loop_in loop_out loop_in_duration loop_out_duration loopIn loopOut loopInDuration loopOutDuration value_at_time valueAtTime speed speed_at_time key nearest_key num_keys nearestKey nextKey previousKey numKeys .index markerName valueOf toString toUpperCase charAt charCodeAt fromCharCode indexOf lastIndexOf match replace search slice split substr substring toLowerCase toLocaleLowerCase toLocaleUpperCase localeCompare layer marker num_layers numLayers layerByComment active_camera activeCamera width height duration frame_duration ntscDropFrame displayStartTime shutter_angle shutter_phase bg_color pixel_aspect frameDuration shutterAngle shutterPhase bgColor pixelAspect index comment sourceText sourceData dataValue dataKeyCount dataKeyTimes dataKeyValues dataPath chapter url frameTarget protectedRegion eventCuePoint cuePointName parameters sub div position Date fullPath linearBlending bitsPerChannel .name numEntries numProperties propertyGroup propertyIndex active enabled cos acos tan atan atan2 sin sqrt exp pow log abs round ceil floor min max text items textAtTime createStyle getStyleAt setAllCaps setApplyFill setApplyStroke setAutoLeading setBaselineDirection setBaselineOption setBaselineShift setDigitSet setFauxBold setFauxItalic setFillColor setFont setFontSize setHorizontalScaling setKerning setKerningType setLeading setLigature setLineJoin replaceText setSmallCaps setStrokeColor setStrokeWidth setText EvalExpr setTracking setTsume setVerticalScaling setDirection setEveryLineComposer setFirstLineIndent setHangingRoman setJustification setLeadingType setLeftMargin setRightMargin setSpaceAfter setSpaceBefore asin BEEp_EvalExprEngines"),
];

fn row(category: ExprCategory) -> Vec<&'static str> {
    let (_, _, ids) = SPEC_ROWS.iter().find(|(c, _, _)| *c == category).expect("spec row");
    ids.split(' ').collect()
}

fn names(category: ExprCategory) -> Vec<&'static str> {
    expr_identifiers().filter(|e| e.category == category).map(|e| e.name).collect()
}

/// Category cell, count cell and identifier list of one row, as read back through the published API.
fn assert_row(category: ExprCategory, label: &str, count: usize, identifiers: &[&str]) {
    assert_eq!(category.label(), label);
    assert_eq!(category.declared_count(), count, "{label} count cell");
    assert_eq!(category.identifiers(), identifiers, "{label} identifiers");
    assert_eq!(names(category), identifiers, "{label} through expr_identifiers");
    for name in identifiers {
        assert_eq!(expr_identifier(name).map(|e| e.category), Ok(category), "{name}");
    }
}

fn bound(name: &str) -> Result<ExprProvenance, ExprCatalogueError> {
    expr_binding(name)
}

const UNAVAILABLE: Result<ExprProvenance, ExprCatalogueError> =
    Err(ExprCatalogueError::SourceFunctionBindingUnavailable);
const UNKNOWN: Result<ExprProvenance, ExprCatalogueError> = Err(ExprCatalogueError::UnknownIdentifier);

/// MT-33911: 333 identifiers in 12 declared rows, every row's identifiers and count cell exactly as the
/// spec table records them, no duplicate spelling, each member found by exact lookup.
mod mt_33911 {
    use super::*;

    #[test]
    fn catalogue_holds_the_333_identifiers_in_the_12_spec_rows() {
        assert_eq!(EXPR_IDENTIFIER_COUNT, 333);
        assert_eq!(expr_identifiers().count(), 333);
        assert_eq!(ExprCategory::ALL.len(), 12);
        let mut total = 0;
        for (category, count, _) in SPEC_ROWS {
            let ids = row(category);
            assert_eq!(ids.len(), count, "{} spec count", category.label());
            assert_row(category, category.label(), count, &ids);
            total += count;
        }
        assert_eq!(total, 333);
        let counts: Vec<usize> = ExprCategory::ALL.iter().map(|c| c.declared_count()).collect();
        assert_eq!(counts, [18, 9, 6, 6, 5, 4, 5, 16, 9, 19, 30, 206]);
        let mut seen = std::collections::HashSet::new();
        for entry in expr_identifiers() {
            assert!(seen.insert(entry.name), "duplicate {}", entry.name);
        }
        assert_eq!(seen.len(), 333);
        // STU-MOT-073b: the leading block keeps its own label; no category is invented for it.
        assert_eq!(ExprCategory::Uncategorised.label(), "(leading block, no category header on disk)");
        assert_eq!(expr_identifier(".index").map(|e| e.category), Ok(ExprCategory::Uncategorised));
        assert_eq!(expr_identifier(".name").map(|e| e.category), Ok(ExprCategory::Uncategorised));
    }

    /// Exact lookup: case, whitespace, a dropped or added underscore, a dropped leading dot and
    /// non-ASCII look-alikes are unknown, never resolved to a near member.
    #[test]
    fn lookup_is_exact_and_near_spellings_are_unknown() {
        for near in [
            "", "Time", "TIME", " time", "time ", "linear\u{0}", "lookat", "look_At", "Look_at", "lookAt()",
            "thisLayer", "thislayer", "thisProperty", "seedrandom", "seed__random", "_seed_random", "seed_random_",
            "index.", "name", "..index", "BEEpEvalExprEngines", "\u{0430}dd", "ａｄｄ", "timeToNtscTimecode",
        ] {
            assert_eq!(expr_identifier(near), Err(ExprCatalogueError::UnknownIdentifier), "{near:?}");
            assert_eq!(bound(near), UNKNOWN, "{near:?}");
            assert_eq!(canonical_spelling(near), Err(ExprCatalogueError::UnknownIdentifier), "{near:?}");
        }
        assert_eq!(ExprCatalogueError::UnknownIdentifier.code(), "UNKNOWN_IDENTIFIER");
        assert_eq!(
            ExprCatalogueError::SourceFunctionBindingUnavailable.code(),
            "SOURCE_FUNCTION_BINDING_UNAVAILABLE"
        );
    }

    /// The bound/unavailable split: exactly the listed identifiers are bound, each with its provenance;
    /// the other members are typed unavailable (G1: no studio_declared binding in this revision).
    #[test]
    fn bound_set_is_exactly_the_recorded_semantics() {
        let mut bound_names: Vec<(&str, ExprProvenance)> = expr_identifiers()
            .filter_map(|e| match e.binding {
                ExprBinding::Bound(p) => Some((e.name, p)),
                ExprBinding::Unavailable => None,
            })
            .collect();
        bound_names.sort_by_key(|(n, _)| *n);
        let oracle = ExprProvenance::Oracle("AE-03");
        let math = ExprProvenance::Mathematical;
        let spec = ExprProvenance::Spec("STU-MOT-255");
        let mut expected: Vec<(&str, ExprProvenance)> = vec![("time", oracle), ("linear", oracle)];
        for n in [
            "add", "mul", "clamp", "dot", "cross", "normalize", "length", "degrees_to_radians",
            "radians_to_degrees", "degreesToRadians", "radiansToDegrees", "rgb_to_hsl", "hsl_to_rgb", "rgbToHsl",
            "hslToRgb", "hexToRgb",
        ] {
            expected.push((n, math));
        }
        for n in [
            "cos", "acos", "tan", "atan", "atan2", "sin", "sqrt", "exp", "pow", "log", "abs", "round", "ceil",
            "floor", "min", "max", "asin", "valueOf", "toString", "toUpperCase", "charAt", "charCodeAt",
            "fromCharCode", "indexOf", "lastIndexOf", "replace", "slice", "split", "substr", "substring",
            "toLowerCase", "localeCompare",
        ] {
            expected.push((n, spec));
        }
        expected.sort_by_key(|(n, _)| *n);
        assert_eq!(bound_names, expected);
        assert_eq!(bound_names.len(), 50);
        let unavailable = expr_identifiers().filter(|e| e.binding == ExprBinding::Unavailable).count();
        assert_eq!(unavailable, 283);
        assert!(expr_identifiers().all(|e| e.binding != ExprBinding::Bound(ExprProvenance::StudioDeclared)));
        assert_eq!(oracle.code(), "oracle");
        assert_eq!(math.code(), "mathematical");
        assert_eq!(spec.code(), "spec");
        assert_eq!(ExprProvenance::StudioDeclared.code(), "studio_declared");
    }
}

/// MT-33915 Global (advanced: registered, only `time` bound by the AE-03 oracle).
mod mt_33915 {
    use super::*;

    #[test]
    fn global_row_is_registered_and_only_time_is_bound() {
        assert_row(ExprCategory::Global, "Global", 18, &row(ExprCategory::Global));
        assert_eq!(bound("time"), Ok(ExprProvenance::Oracle("AE-03")));
        for name in row(ExprCategory::Global).into_iter().filter(|n| *n != "time") {
            assert_eq!(bound(name), UNAVAILABLE, "{name}");
        }
    }
}

/// MT-33916 Vector Math: count 9 and the exact names; lookAt/look_at stay unavailable (orientation
/// convention not fixed by a definition nor recorded).
mod mt_33916 {
    use super::*;

    #[test]
    fn vector_math_row_names_count_and_bindings() {
        let ids = ["add", "mul", "clamp", "dot", "cross", "normalize", "length", "look_at", "lookAt"];
        assert_row(ExprCategory::VectorMath, "Vector Math", 9, &ids);
        for name in &ids[..7] {
            assert_eq!(bound(name), Ok(ExprProvenance::Mathematical), "{name}");
        }
        assert_eq!(bound("look_at"), UNAVAILABLE);
        assert_eq!(bound("lookAt"), UNAVAILABLE);
        assert_eq!(bound("lookat"), UNKNOWN);
    }
}

/// MT-33917 Random Numbers: registered, all six typed unavailable (G1 not confirmed: no generator
/// formula is recorded; STU-MOT-076 records only the seeding rule).
mod mt_33917 {
    use super::*;

    #[test]
    fn random_numbers_row_names_count_and_typed_unavailable() {
        let ids = ["seed_random", "seedRandom", "random", "gauss_random", "gaussRandom", "noise"];
        assert_row(ExprCategory::RandomNumbers, "Random Numbers", 6, &ids);
        for name in ids {
            assert_eq!(bound(name), UNAVAILABLE, "{name}");
        }
        // The other STU-MOT-074 randomness members live in the leading block and stay unavailable too.
        for name in ["wiggle", "temporal_wiggle", "temporalWiggle"] {
            assert_eq!(bound(name), UNAVAILABLE, "{name}");
        }
    }
}

/// MT-33918 Interpolation: `linear` bound by the AE-03 oracle; `ease`, `easeIn`, `easeOut` and their
/// spellings typed unavailable (G1).
mod mt_33918 {
    use super::*;

    #[test]
    fn interpolation_row_names_count_and_bindings() {
        let ids = ["linear", "ease", "ease_in", "ease_out", "easeIn", "easeOut"];
        assert_row(ExprCategory::Interpolation, "Interpolation", 6, &ids);
        assert_eq!(bound("linear"), Ok(ExprProvenance::Oracle("AE-03")));
        for name in &ids[1..] {
            assert_eq!(bound(name), UNAVAILABLE, "{name}");
        }
    }
}

/// MT-33919 Color Conversion: all five bound by the HSL and hex-triplet definitions.
mod mt_33919 {
    use super::*;

    #[test]
    fn color_conversion_row_names_count_and_bindings() {
        let ids = ["rgb_to_hsl", "hsl_to_rgb", "rgbToHsl", "hslToRgb", "hexToRgb"];
        assert_row(ExprCategory::ColorConversion, "Color Conversion", 5, &ids);
        for name in ids {
            assert_eq!(bound(name), Ok(ExprProvenance::Mathematical), "{name}");
        }
    }
}

/// MT-33920 Other Math: all four bound by the degree/radian definitions.
mod mt_33920 {
    use super::*;

    #[test]
    fn other_math_row_names_count_and_bindings() {
        let ids = ["degrees_to_radians", "radians_to_degrees", "degreesToRadians", "radiansToDegrees"];
        assert_row(ExprCategory::OtherMath, "Other Math", 4, &ids);
        for name in ids {
            assert_eq!(bound(name), Ok(ExprProvenance::Mathematical), "{name}");
        }
    }
}

/// MT-33926 leading block (advanced: registered; bound only where the ES2018 profile records the
/// semantics, STU-MOT-255).
mod mt_33926 {
    use super::*;

    #[test]
    fn leading_block_is_registered_with_its_206_members() {
        let ids = row(ExprCategory::Uncategorised);
        assert_row(ExprCategory::Uncategorised, "(leading block, no category header on disk)", 206, &ids);
        assert_eq!(ids.first(), Some(&"point_of_interest"));
        assert_eq!(ids.last(), Some(&"BEEp_EvalExprEngines"));
        for name in ["Date", "match", "search", "toLocaleLowerCase", "toLocaleUpperCase", "sub", "div", ".index"] {
            assert_eq!(bound(name), UNAVAILABLE, "{name}");
        }
    }
}

/// MT-33912 dual spelling (STU-MOT-073a/144): the declared pairs are exactly the 60 rows where both
/// spellings sit in one row; both spellings are accepted, the canonical write spelling is the
/// camelCase one, a lone snake_case member keeps its own spelling, and no alias is guessed.
mod mt_33912 {
    use super::*;

    fn camel(snake: &str) -> String {
        let mut parts = snake.split('_');
        let mut out = parts.next().unwrap().to_owned();
        for p in parts {
            let mut c = p.chars();
            out.extend(c.next().map(|f| f.to_ascii_uppercase()));
            out.push_str(c.as_str());
        }
        out
    }

    #[test]
    fn declared_pairs_are_exact_and_canonical_is_camel_case() {
        // Independent derivation from the spec rows.
        let mut expected = Vec::new();
        for (category, _, _) in SPEC_ROWS {
            let ids = row(category);
            for id in &ids {
                if id.contains('_') && ids.contains(&camel(id).as_str()) {
                    expected.push((id.to_string(), camel(id)));
                }
            }
        }
        let pairs: Vec<(String, String)> = dual_spellings().map(|(s, c)| (s.to_owned(), c.to_owned())).collect();
        assert_eq!(pairs, expected);
        assert_eq!(pairs.len(), 60);
        for (snake, camel_case) in dual_spellings() {
            let s = expr_identifier(snake).unwrap();
            let c = expr_identifier(camel_case).unwrap();
            assert_eq!((s.canonical, c.canonical), (camel_case, camel_case));
            assert_eq!(s.category, c.category, "{snake} pair shares one row");
            assert_eq!(s.binding, c.binding, "{snake} pair shares one binding");
        }
        for (snake, camel_case) in [
            ("this_comp", "thisComp"),
            ("point_of_interest", "pointOfInterest"),
            ("value_at_time", "valueAtTime"),
            ("to_comp", "toComp"),
            ("from_comp_to_surface", "fromCompToSurface"),
            ("degrees_to_radians", "degreesToRadians"),
        ] {
            assert_eq!(canonical_spelling(snake), Ok(camel_case));
            assert_eq!(canonical_spelling(camel_case), Ok(camel_case));
        }
        // Lone snake_case members (no camelCase partner in their row) keep their spelling; the
        // underscore-free guess is not a member.
        for lone in [
            "this_layer", "this_property", "world_position_to_psd", "world_rotation_to_psd", "world_scale_to_psd",
            "BEEp_EvalExprEngines",
        ] {
            assert_eq!(canonical_spelling(lone), Ok(lone));
            assert_eq!(expr_identifier(&camel(lone)), Err(ExprCatalogueError::UnknownIdentifier), "{lone}");
        }
        let lone_count = expr_identifiers().filter(|e| e.name.contains('_') && e.canonical == e.name).count();
        assert_eq!(lone_count, 6);
        // Every member whose canonical differs is the snake half of a declared pair.
        assert_eq!(expr_identifiers().filter(|e| e.canonical != e.name).count(), 60);
    }
}

/// MT-33933 signatures (STU-MOT-075): the 37 rows verbatim with their defaults; attached only where
/// the spec names the function.
mod mt_33933 {
    use super::*;

    fn params(number: u8) -> Vec<(&'static str, Option<&'static str>)> {
        EXPR_SIGNATURES[usize::from(number) - 1].params().into_iter().map(|p| (p.name, p.default)).collect()
    }

    #[test]
    fn the_37_signatures_are_verbatim_with_their_defaults() {
        assert_eq!(EXPR_SIGNATURES.len(), 37);
        for (i, s) in EXPR_SIGNATURES.iter().enumerate() {
            assert_eq!(usize::from(s.number), i + 1);
            let expected = if s.number >= 18 { Some("Space Transforms") } else { None };
            assert_eq!(s.category, expected, "#{}", s.number);
        }
        let text = |n: usize| EXPR_SIGNATURES[n - 1].text;
        assert_eq!(text(1), "percentage = 0.5, t = time");
        assert_eq!(text(2), "points = [[0,0], [100,0], [100,100], [0,100]], inTangents = [], outTangents = [], isClosed = true");
        assert_eq!(text(3), "freq, amp, octaves = 1, amp_mult = .5, t = time");
        assert_eq!(text(4), "width = .2, samples = 5, t = time");
        assert_eq!(text(5), "type = \"cycle\", numKeyframes = 0");
        assert_eq!(text(6), "type = \"cycle\", duration = 0");
        assert_eq!(text(16), "*");
        assert_eq!(text(17), "+");
        assert_eq!(text(23), "seed, timeless = false");
        assert_eq!(text(26), "t, tMin, tMax, value1, value2");
        assert_eq!(text(29), "point, radius = [.5, .5], postEffect = true, t = time");
        assert_eq!(
            text(35),
            "t = time + thisComp.displayStartTime, fps = 1.0 / thisComp.frameDuration, isDuration = false, ntscDropFrame = thisComp.ntscDropFrame"
        );
        assert_eq!(text(37), "t = time, includeExtents = false");
        // Defaults are contract and are kept verbatim.
        assert_eq!(
            params(3),
            [("freq", None), ("amp", None), ("octaves", Some("1")), ("amp_mult", Some(".5")), ("t", Some("time"))]
        );
        assert_eq!(params(4), [("width", Some(".2")), ("samples", Some("5")), ("t", Some("time"))]);
        assert_eq!(params(5), [("type", Some("\"cycle\"")), ("numKeyframes", Some("0"))]);
        assert_eq!(
            params(2),
            [
                ("points", Some("[[0,0], [100,0], [100,100], [0,100]]")),
                ("inTangents", Some("[]")),
                ("outTangents", Some("[]")),
                ("isClosed", Some("true")),
            ]
        );
        assert_eq!(params(14), [("value", None), ("s", Some("start index")), ("n", Some("number of characters"))]);
        assert_eq!(params(29)[1], ("radius", Some("[.5, .5]")));
        assert_eq!(params(16), [("*", None)]);
        assert_eq!(params(34).len(), 4);
        assert_eq!(EXPR_SIGNATURES[2].param("amp_mult").and_then(|p| p.default), Some(".5"));
        assert_eq!(EXPR_SIGNATURES[2].param("ampMult"), None);
        let total: usize = EXPR_SIGNATURES.iter().map(|s| s.params().len()).sum();
        assert_eq!(total, 92);
    }

    #[test]
    fn signatures_attach_only_where_the_spec_names_the_function() {
        let attached: Vec<(&str, u8)> =
            expr_identifiers().filter_map(|e| e.signature.map(|s| (e.name, s.number))).collect();
        assert_eq!(attached, [("createPath", 2), ("wiggle", 3), ("temporalWiggle", 4), ("loopOut", 5)]);
        // A named signature does not bind a function: all four stay typed unavailable.
        for (name, _) in attached {
            assert_eq!(bound(name), UNAVAILABLE, "{name}");
        }
        assert_eq!(expr_identifier("loop_out").map(|e| e.signature), Ok(None));
    }
}

/// The descriptor names the catalogue (STU-MOT-073..076, codes only).
#[test]
fn descriptor_names_the_expression_catalogue() {
    assert!(DESCRIPTOR.contains("\"expression_catalogue\":{"));
    assert!(DESCRIPTOR.contains("SOURCE_FUNCTION_BINDING_UNAVAILABLE"));
}
