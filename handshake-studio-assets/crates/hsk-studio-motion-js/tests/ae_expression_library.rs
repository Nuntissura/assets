#![cfg(feature = "quickjs")]
//! After Effects expression library in the MotionJS profile (lane SMB-AE-EXPR-LIBRARY-1; STU-MOT-073..078,
//! 255). Run with `--features quickjs` (worker process) or `--features quickjs,test-faults` (in-process
//! engine unless `HSK_MOTION_JS_TEST_BACKEND=worker`). Expected values are hand-computed from the
//! mathematical definitions (or the same IEEE operations written in Rust where the JS formula is the
//! definition); After Effects parity beyond the AE-03 oracle is UNVERIFIED (D5). The catalogue walk
//! checks every one of the 333 identifiers against what the engine answers.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_motion::*;
use hsk_studio_motion_js::*;
use std::collections::BTreeMap;

const S: i64 = 254_016_000_000;

type P = MotionJsProvider<QuickJsBackend>;

fn options() -> QuickJsOptions {
    #[cfg(feature = "test-faults")]
    {
        if std::env::var("HSK_MOTION_JS_TEST_BACKEND").as_deref() != Ok("worker") {
            return QuickJsOptions::unsafe_in_process();
        }
    }
    QuickJsOptions::default().with_worker_program(env!("CARGO_BIN_EXE_motion-js-worker"))
}

/// Production shape: the profile validator stays on.
fn provider() -> P {
    let quotas = Quotas::try_from_spec(QuotaSpec::default()).unwrap();
    MotionJsProvider::new(QuickJsBackend::new(&quotas, options()).unwrap(), quotas)
}

fn eval(provider: &P, source: &str, dimension: usize) -> Result<Vec<f64>, ExprError> {
    let request = ExprRequest { source, revision: 0, tick: EvalTick::new(S), seed: 0, dimension, base: None };
    provider.evaluate(&request, &NoHost, &mut Fuel::new(1000), &CancellationToken::default())
}

fn one(provider: &P, source: &str) -> Result<Vec<f64>, ExprError> {
    eval(provider, source, 1)
}

/// `(code, line, column)` of a rejection.
fn refusal(result: Result<Vec<f64>, ExprError>) -> (ExprErrorCode, Option<(u32, u32)>) {
    let error = result.expect_err("expected a typed rejection");
    (error.code, error.span.map(|s| (s.line, s.column)))
}

/// A refusal thrown inside a bound helper: `Unsupported`, no span (the throw site is the prelude line).
const ARGUMENT: (ExprErrorCode, Option<(u32, u32)>) = (ExprErrorCode::Unsupported, None);

fn close(actual: &[f64], expected: &[f64], tolerance: f64) -> bool {
    actual.len() == expected.len() && actual.iter().zip(expected).all(|(a, e)| (a - e).abs() <= tolerance)
}

/// MT-33916 / MT-33920: Vector Math and Other Math bound by their definitions, both declared spellings
/// on one function, hostile shapes refused (never an identity result).
#[test]
fn vector_and_other_math_bind_by_definition() {
    let p = provider();
    assert_eq!(one(&p, "add(1, 2)"), Ok(vec![3.0]));
    assert_eq!(eval(&p, "add([1, 2], [3, 4.5])", 2), Ok(vec![4.0, 6.5]));
    assert_eq!(eval(&p, "mul([1, -2, 3], 2)", 3), Ok(vec![2.0, -4.0, 6.0]));
    assert_eq!(one(&p, "mul(3, 0.5)"), Ok(vec![1.5]));
    assert_eq!(one(&p, "clamp(5, 0, 2)"), Ok(vec![2.0]));
    assert_eq!(one(&p, "clamp(-1, 0, 2)"), Ok(vec![0.0]));
    assert_eq!(one(&p, "clamp(1.5, 0, 2)"), Ok(vec![1.5]));
    assert_eq!(eval(&p, "clamp([5, -1, 1], [0, 0, 0], [2, 2, 2])", 3), Ok(vec![2.0, 0.0, 1.0]));
    assert_eq!(one(&p, "dot([1, 2, 3], [4, 5, 6])"), Ok(vec![32.0]));
    assert_eq!(eval(&p, "cross([1, 0, 0], [0, 1, 0])", 3), Ok(vec![0.0, 0.0, 1.0]));
    assert_eq!(eval(&p, "cross([1, 2, 3], [4, 5, 6])", 3), Ok(vec![-3.0, 6.0, -3.0]));
    assert_eq!(eval(&p, "normalize([3, 4])", 2), Ok(vec![0.6, 0.8]));
    assert_eq!(one(&p, "length([3, 4])"), Ok(vec![5.0]));
    assert_eq!(one(&p, "length([1, 1], [4, 5])"), Ok(vec![5.0]));
    let pi = std::f64::consts::PI;
    assert_eq!(one(&p, "degreesToRadians(180)"), Ok(vec![180.0 * pi / 180.0]));
    assert_eq!(one(&p, "degrees_to_radians(90)"), Ok(vec![90.0 * pi / 180.0]));
    assert_eq!(one(&p, "radiansToDegrees(Math.PI / 2)"), Ok(vec![(pi / 2.0) * 180.0 / pi]));
    assert_eq!(one(&p, "radians_to_degrees(1)"), Ok(vec![180.0 / pi]));
    // Dual spellings are one binding (STU-MOT-073a).
    assert_eq!(
        one(&p, "degrees_to_radians === degreesToRadians && radians_to_degrees === radiansToDegrees ? 1 : 0"),
        Ok(vec![1.0])
    );
    // Hostile shapes: typed refusals, not identity or NaN results.
    for source in [
        "add([1], [1, 2])", "add(1)", "add(1, 2, 3)", "add('1', 2)", "add([1], [NaN])", "add(1e308, 1e308)",
        "add([], [])", "add([1, 'x'], [1, 2])", "add(1, [1])", "mul([1], [2])", "mul([1], Infinity)",
        "clamp(1, 2, 0)", "clamp([1], [0, 0], [1, 1])", "clamp(1, 0)", "dot([1, 2], [1])", "dot(1, 2)",
        "cross([1, 2], [3, 4])", "normalize([0, 0])", "normalize(5)", "length()", "length(3)",
        "length([1], [1, 2])", "degreesToRadians('180')", "degreesToRadians()", "radiansToDegrees(1, 2)",
        "add({ length: 1, 0: 1 }, [1])",
    ] {
        assert_eq!(refusal(one(&p, source)), ARGUMENT, "{source}");
    }
    // lookAt / look_at: no recorded orientation convention, typed unavailable at the user's span.
    assert_eq!(refusal(one(&p, "lookAt([0, 0, 0], [1, 0, 0])")), (ExprErrorCode::Unsupported, Some((1, 1))));
    assert_eq!(refusal(one(&p, "\n  look_at")), (ExprErrorCode::Unsupported, Some((2, 3))));
}

/// MT-33919: HSL by its definition (4 components, alpha passed through: Studio reading), hex triplets
/// as `#RRGGBB` or `RRGGBB` only (quartets, short forms and prefixes UNVERIFIED, refused).
#[test]
fn color_conversion_binds_by_definition() {
    let p = provider();
    let four = |source: &str| eval(&p, source, 4);
    assert_eq!(four("rgbToHsl([1, 0, 0, 1])"), Ok(vec![0.0, 1.0, 0.5, 1.0]));
    assert_eq!(four("rgbToHsl([0, 1, 0, 0.25])"), Ok(vec![1.0 / 3.0, 1.0, 0.5, 0.25]));
    assert_eq!(four("rgbToHsl([0, 0, 1, 1])"), Ok(vec![2.0 / 3.0, 1.0, 0.5, 1.0]));
    assert_eq!(four("rgb_to_hsl([0.5, 0.5, 0.5, 1])"), Ok(vec![0.0, 0.0, 0.5, 1.0]));
    // l = 0.25 <= 0.5: s = d / (max + min) = 0.5 / 0.5 = 1, h = ((g - b) / d) / 6 = 1 / 6.
    assert_eq!(four("rgbToHsl([0.5, 0.25, 0, 1])"), Ok(vec![(0.25 / 0.5) / 6.0, 1.0, 0.25, 1.0]));
    assert_eq!(four("hslToRgb([0, 1, 0.5, 1])"), Ok(vec![1.0, 0.0, 0.0, 1.0]));
    assert_eq!(four("hsl_to_rgb([0, 0, 0.25, 0.5])"), Ok(vec![0.25, 0.25, 0.25, 0.5]));
    assert_eq!(four("hslToRgb([1, 1, 0.5, 1])"), Ok(vec![1.0, 0.0, 0.0, 1.0]));
    // Round trip within 1e-12 over a spread of colours.
    for c in [[0.2, 0.4, 0.6, 1.0], [0.9, 0.1, 0.3, 0.5], [0.05, 0.95, 0.5, 0.0], [0.3, 0.3, 0.31, 1.0]] {
        let source = format!("hslToRgb(rgbToHsl([{}, {}, {}, {}]))", c[0], c[1], c[2], c[3]);
        let back = four(&source).unwrap();
        assert!(close(&back, &c, 1e-12), "{source} -> {back:?}");
    }
    for source in [
        "rgbToHsl([1, 0, 0])", "rgbToHsl([1, 0, 0, 1, 1])", "rgbToHsl([1.5, 0, 0, 1])", "rgbToHsl([-0.1, 0, 0, 1])",
        "rgbToHsl(1)", "hslToRgb([0, 1, NaN, 1])", "hslToRgb([2, 1, 0.5, 1])", "rgbToHsl()",
    ] {
        assert_eq!(refusal(four(source)), ARGUMENT, "{source}");
    }
    let orange = vec![1.0, 128.0 / 255.0, 0.0, 1.0];
    assert_eq!(four("hexToRgb('#FF8000')"), Ok(orange.clone()));
    assert_eq!(four("hexToRgb('ff8000')"), Ok(orange.clone()));
    assert_eq!(four("hexToRgb('#00aAfF')"), Ok(vec![0.0, 170.0 / 255.0, 1.0, 1.0]));
}

/// V4-08 hostile text: `hexToRgb` reads the decoded string value character by character; escapes are
/// decoded by the language first, decoys and comments are not stripped, whitespace and case are
/// exact.
#[test]
fn hex_to_rgb_refuses_hostile_text() {
    let p = provider();
    let four = |source: &str| eval(&p, source, 4);
    let orange = vec![1.0, 128.0 / 255.0, 0.0, 1.0];
    // Escape: a JS escape decodes before the parser sees the text; an undecoded backslash is refused.
    assert_eq!(four(r"hexToRgb('\x23FF8000')"), Ok(orange.clone()));
    assert_eq!(four(r"hexToRgb('#ff8000')"), Ok(orange.clone()));
    for source in [r"hexToRgb('\\x23FF8000')", r"hexToRgb('FF80\u00000')", r"hexToRgb('#FF\x00000')"] {
        assert_eq!(refusal(four(source)), ARGUMENT, "{source}");
    }
    // Comment / decoy: nothing is stripped or substring-matched.
    for source in [
        "hexToRgb('#FF8000/*x*/')", "hexToRgb('/*#FF8000*/')", "hexToRgb('#FF8000;')", "hexToRgb('#FF80 00')",
        "hexToRgb('##FF8000')", "hexToRgb('#FF8000#')", "hexToRgb('#GG8000')", "hexToRgb('#FF800G')",
    ] {
        assert_eq!(refusal(four(source)), ARGUMENT, "{source}");
    }
    // Case / whitespace and other notations: digits are case-insensitive, nothing is trimmed, and
    // forms whose meaning is not recorded (short, quartet, 0x) are refused.
    assert_eq!(four("hexToRgb('#fF8000')"), Ok(orange));
    for source in [
        "hexToRgb(' #FF8000')", "hexToRgb('#FF8000 ')", r"hexToRgb('#FF8000\n')", r"hexToRgb('\t#FF8000')",
        "hexToRgb('#FFF')", "hexToRgb('#FF800000')", "hexToRgb('0xFF8000')", "hexToRgb('')", "hexToRgb('#')",
        "hexToRgb('\u{FF03}FF8000')", "hexToRgb('#\u{FF26}F8000')", "hexToRgb(16744448)", "hexToRgb()",
        "hexToRgb(['#FF8000'])", "hexToRgb('#FF8000', 1)",
    ] {
        assert_eq!(refusal(four(source)), ARGUMENT, "{source}");
    }
    // Hostile size: a long string is refused on its length before any digit is read.
    assert_eq!(refusal(four("hexToRgb('#'.repeat(100000))")), ARGUMENT);
}

/// STU-MOT-255: default `localeCompare` is deterministic UTF-16 code-unit order; locale or options
/// arguments are refused instead of consulting the machine.
#[test]
fn locale_compare_is_utf16_lexical_order() {
    let p = provider();
    assert_eq!(one(&p, "'a'.localeCompare('b')"), Ok(vec![-1.0]));
    assert_eq!(one(&p, "'b'.localeCompare('a')"), Ok(vec![1.0]));
    assert_eq!(one(&p, "'a'.localeCompare('a')"), Ok(vec![0.0]));
    assert_eq!(one(&p, "'a'.localeCompare('ab')"), Ok(vec![-1.0]));
    assert_eq!(one(&p, "'B'.localeCompare('a')"), Ok(vec![-1.0]));
    // U+FF61 sorts after the surrogate U+D83D in code-unit order (code-point order would invert it).
    assert_eq!(one(&p, r"'｡'.localeCompare('😀')"), Ok(vec![1.0]));
    assert_eq!(one(&p, "'a'.localeCompare('b', undefined, undefined)"), Ok(vec![-1.0]));
    assert_eq!(refusal(one(&p, "'a'.localeCompare('b', 'de')")), ARGUMENT);
    assert_eq!(refusal(one(&p, "'a'.localeCompare('b', undefined, {})")), ARGUMENT);
}

/// One probe per bound identifier: a source evaluating to the hand-computed value.
fn bound_probes() -> BTreeMap<&'static str, (&'static str, Vec<f64>)> {
    let pi = std::f64::consts::PI;
    let probes: Vec<(&str, &str, Vec<f64>)> = vec![
        ("time", "time", vec![1.0]),
        ("linear", "linear(0.5, 0, 1, 0, 10)", vec![5.0]),
        ("add", "add(2, 3)", vec![5.0]),
        ("mul", "mul(2, 3)", vec![6.0]),
        ("clamp", "clamp(3, 0, 1)", vec![1.0]),
        ("dot", "dot([1, 2], [3, 4])", vec![11.0]),
        ("cross", "cross([0, 1, 0], [0, 0, 1])", vec![1.0, 0.0, 0.0]),
        ("normalize", "normalize([0, 0, 2])", vec![0.0, 0.0, 1.0]),
        ("length", "length([6, 8])", vec![10.0]),
        ("degrees_to_radians", "degrees_to_radians(360)", vec![360.0 * pi / 180.0]),
        ("degreesToRadians", "degreesToRadians(45)", vec![45.0 * pi / 180.0]),
        ("radians_to_degrees", "radians_to_degrees(2)", vec![2.0 * 180.0 / pi]),
        ("radiansToDegrees", "radiansToDegrees(3)", vec![3.0 * 180.0 / pi]),
        ("rgb_to_hsl", "rgb_to_hsl([1, 1, 1, 1])", vec![0.0, 0.0, 1.0, 1.0]),
        ("rgbToHsl", "rgbToHsl([0, 0, 0, 0.5])", vec![0.0, 0.0, 0.0, 0.5]),
        ("hsl_to_rgb", "hsl_to_rgb([0, 0, 1, 1])", vec![1.0, 1.0, 1.0, 1.0]),
        ("hslToRgb", "hslToRgb([0.5, 0, 0.5, 1])", vec![0.5, 0.5, 0.5, 1.0]),
        ("hexToRgb", "hexToRgb('#000000')", vec![0.0, 0.0, 0.0, 1.0]),
        ("cos", "Math.cos(0)", vec![1.0]),
        ("acos", "Math.acos(1)", vec![0.0]),
        ("tan", "Math.tan(0)", vec![0.0]),
        ("atan", "Math.atan(0)", vec![0.0]),
        ("atan2", "Math.atan2(0, 1)", vec![0.0]),
        ("sin", "Math.sin(0)", vec![0.0]),
        ("sqrt", "Math.sqrt(16)", vec![4.0]),
        ("exp", "Math.exp(0)", vec![1.0]),
        ("pow", "Math.pow(2, 10)", vec![1024.0]),
        ("log", "Math.log(1)", vec![0.0]),
        ("abs", "Math.abs(-3)", vec![3.0]),
        ("round", "Math.round(2.5)", vec![3.0]),
        ("ceil", "Math.ceil(1.2)", vec![2.0]),
        ("floor", "Math.floor(1.8)", vec![1.0]),
        ("min", "Math.min(4, 2, 3)", vec![2.0]),
        ("max", "Math.max(4, 2, 3)", vec![4.0]),
        ("asin", "Math.asin(0)", vec![0.0]),
        ("valueOf", "(7).valueOf()", vec![7.0]),
        ("toString", "(12).toString() === '12' ? 1 : 0", vec![1.0]),
        ("toUpperCase", "'ab'.toUpperCase() === 'AB' ? 1 : 0", vec![1.0]),
        ("charAt", "'abc'.charAt(1) === 'b' ? 1 : 0", vec![1.0]),
        ("charCodeAt", "'abc'.charCodeAt(2)", vec![99.0]),
        ("fromCharCode", "String.fromCharCode(72, 105) === 'Hi' ? 1 : 0", vec![1.0]),
        ("indexOf", "'banana'.indexOf('an')", vec![1.0]),
        ("lastIndexOf", "'banana'.lastIndexOf('an')", vec![3.0]),
        ("replace", "'a-b-c'.replace('-', '+') === 'a+b-c' ? 1 : 0", vec![1.0]),
        ("slice", "'abcdef'.slice(1, -1) === 'bcde' ? 1 : 0", vec![1.0]),
        ("split", "'a,b,c'.split(',').length", vec![3.0]),
        ("substr", "'abcdef'.substr(2, 2) === 'cd' ? 1 : 0", vec![1.0]),
        ("substring", "'abcdef'.substring(4, 1) === 'bcd' ? 1 : 0", vec![1.0]),
        ("toLowerCase", "'AB'.toLowerCase() === 'ab' ? 1 : 0", vec![1.0]),
        ("localeCompare", "'b'.localeCompare('c')", vec![-1.0]),
    ];
    probes.into_iter().map(|(name, source, value)| (name, (source, value))).collect()
}

/// How the engine must answer an unavailable member.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// Plain identifier: a global accessor that throws at the user's span.
    Global,
    /// String member replaced by the profile's refusal.
    StringMember,
    /// Member of a host object (`.index`, `.name`); the host objects themselves are unavailable.
    HostMember,
    /// No binding in the realm at all (the Date profile is outside this lane).
    Absent,
}

fn refusal_kind(name: &str) -> Refusal {
    match name {
        "match" | "search" | "toLocaleLowerCase" | "toLocaleUpperCase" => Refusal::StringMember,
        ".index" | ".name" => Refusal::HostMember,
        "Date" => Refusal::Absent,
        _ => Refusal::Global,
    }
}

/// Every one of the 333 catalogue identifiers against the engine: a bound one answers its probe, an
/// unavailable one is a typed refusal (never identity, `undefined` or a no-op result).
#[test]
fn catalogue_walk_matches_what_the_engine_answers() {
    let p = provider();
    let probes = bound_probes();
    let bound: Vec<&str> = expr_identifiers()
        .filter(|e| matches!(e.binding, ExprBinding::Bound(_)))
        .map(|e| e.name)
        .collect();
    let mut probe_names: Vec<&str> = probes.keys().copied().collect();
    let mut sorted_bound = bound.clone();
    sorted_bound.sort_unstable();
    probe_names.sort_unstable();
    assert_eq!(probe_names, sorted_bound, "one probe per bound identifier");
    let mut counts = BTreeMap::new();
    for entry in expr_identifiers() {
        let name = entry.name;
        match entry.binding {
            ExprBinding::Bound(_) => {
                let (source, expected) = &probes[name];
                assert_eq!(eval(&p, source, expected.len()).as_ref(), Ok(expected), "{name}: {source}");
                *counts.entry("bound").or_insert(0) += 1;
            }
            ExprBinding::Unavailable => {
                let kind = refusal_kind(name);
                match kind {
                    Refusal::Global => {
                        let read = format!("\n  {name}");
                        assert_eq!(refusal(one(&p, &read)), (ExprErrorCode::Unsupported, Some((2, 3))), "{name}");
                        let call = format!("var k = 1;\n{name}(1, 2)");
                        assert_eq!(refusal(one(&p, &call)).0, ExprErrorCode::Unsupported, "{name} call");
                        assert_eq!(refusal(one(&p, &call)).1.map(|s| s.0), Some(2), "{name} call line");
                        let typed = format!("typeof {name} === 'undefined' ? 1 : 0");
                        assert_eq!(refusal(one(&p, &typed)).0, ExprErrorCode::Unsupported, "{name} typeof");
                    }
                    Refusal::StringMember => {
                        // The span points at the member name, after `'abc'.` (column 7).
                        let call = format!("'abc'.{name}('b')");
                        assert_eq!(refusal(one(&p, &call)), (ExprErrorCode::Unsupported, Some((1, 7))), "{name}");
                    }
                    Refusal::HostMember => {
                        let read = format!("thisComp{name}");
                        assert_eq!(refusal(one(&p, &read)), (ExprErrorCode::Unsupported, Some((1, 1))), "{name}");
                    }
                    Refusal::Absent => {
                        assert_eq!(refusal(one(&p, name)), (ExprErrorCode::ReferenceMissing, Some((1, 1))), "{name}");
                    }
                }
                let label = match kind {
                    Refusal::Global => "Global",
                    Refusal::StringMember => "StringMember",
                    Refusal::HostMember => "HostMember",
                    Refusal::Absent => "Absent",
                };
                *counts.entry(label).or_insert(0) += 1;
            }
        }
    }
    let counts: Vec<(&str, i32)> = counts.into_iter().collect();
    assert_eq!(counts, [("Absent", 1), ("Global", 276), ("HostMember", 2), ("StringMember", 4), ("bound", 50)]);
}

/// MT-33917 / MT-33918 (and the randomness family of STU-MOT-074): no generator or easing formula is
/// recorded, so every member is a typed refusal at the user's span; `Math.random` stays denied.
#[test]
fn randomness_and_easing_are_typed_unavailable() {
    let p = provider();
    for name in [
        "seed_random", "seedRandom", "random", "gauss_random", "gaussRandom", "noise", "wiggle", "temporal_wiggle",
        "temporalWiggle", "ease", "ease_in", "ease_out", "easeIn", "easeOut",
    ] {
        let source = format!("var a = 1;\nvar b = a + {name}(0.5, 0, 1, 0, 10);\nb");
        let (code, span) = refusal(one(&p, &source));
        assert_eq!((code, span.map(|s| s.0)), (ExprErrorCode::Unsupported, Some(2)), "{name}");
    }
    assert_eq!(refusal(one(&p, "Math.random()")).0, ExprErrorCode::Unsupported);
    assert_eq!(refusal(one(&p, "seedRandom(1, true); random()")), (ExprErrorCode::Unsupported, Some((1, 1))));
}

/// STU-MOT-073a: user declarations are never renamed or captured: a local named like a catalogue
/// member is the user's; both spellings of a pair resolve to one binding.
#[test]
fn user_declarations_shadow_catalogue_names() {
    let p = provider();
    assert_eq!(one(&p, "var width = 3; width * 2"), Ok(vec![6.0]));
    assert_eq!(eval(&p, "let scale = [1, 2]; scale", 2), Ok(vec![1.0, 2.0]));
    assert_eq!(one(&p, "const length = 4; length"), Ok(vec![4.0]));
    assert_eq!(one(&p, "function wiggle(f, a) { return f + a; } wiggle(1, 2)"), Ok(vec![3.0]));
    assert_eq!(one(&p, "function add(a, b) { return a * b; } add(2, 5)"), Ok(vec![10.0]));
    assert_eq!(one(&p, "var color; color = 5; color"), Ok(vec![5.0]));
    assert_eq!(one(&p, "function f() { var text = 7; return text; } f()"), Ok(vec![7.0]));
    assert_eq!(one(&p, "var o = { index: 2, wiggle: 3 }; o.index + o.wiggle"), Ok(vec![5.0]));
    assert_eq!(one(&p, "'wiggle'.length"), Ok(vec![6.0]));
    // Outside the user's declaration the member is still refused.
    assert_eq!(refusal(one(&p, "function f() { var text = 7; return text; }\nf() + text")), (ExprErrorCode::Unsupported, Some((2, 7))));
}

/// STU-MOT-076/078 through Motion: a bound library call evaluates deterministically through the
/// published Property API; an unavailable one disables the expression with its span and the property
/// falls back to its keyframed value. The prelude costs no engine poll (Motion's 1-unit call charge
/// only).
#[test]
fn library_through_motion_property_is_deterministic_and_unavailable_disables() {
    let p = provider();
    let keys = vec![
        Keyframe::new(Ticks::new(0), vec![0.0, 0.0]),
        Keyframe::new(Ticks::new((2 * S) as u64), vec![20.0, 40.0]),
    ];
    let cancel = CancellationToken::default();
    let mut property =
        Property::new("pos", vec![0.0, 0.0], keys.clone()).unwrap().with_expression("add(value, mul([1, 2], time))").unwrap();
    let ctx = EvalContext { expected_revision: property.revision(), tick: EvalTick::new(S), seed: 7 };
    let first = property.evaluate_with(&ctx, &p, &NoHost, &mut Fuel::new(50), &cancel).unwrap();
    let second = property.evaluate_with(&ctx, &p, &NoHost, &mut Fuel::new(50), &cancel).unwrap();
    // At 1 s the keyframed value is [10, 20]; + [1, 2] * 1 = [11, 22].
    assert_eq!(first.value, vec![11.0, 22.0]);
    assert_eq!(first.value.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), second.value.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
    assert_eq!((first.receipt.disabled_by_error, first.receipt.fuel_used, first.receipt.profile), (None, 1, PROFILE_ID));
    let mut broken = Property::new("pos", vec![0.0, 0.0], keys).unwrap().with_expression("value\n  + wiggle(2, 5)").unwrap();
    let ctx = EvalContext { expected_revision: broken.revision(), tick: EvalTick::new(S), seed: 7 };
    let fallback = broken.evaluate_with(&ctx, &p, &NoHost, &mut Fuel::new(50), &cancel).unwrap();
    assert_eq!(fallback.value, vec![10.0, 20.0]);
    assert_eq!(fallback.receipt.disabled_by_error, Some(ExprErrorCode::Unsupported));
    assert_eq!(fallback.receipt.error_span, Some(SourceSpan { line: 2, column: 5 }));
    assert!(p.retire().is_clean());
}
