//! Admitted-syntax profile contract (default features, no engine). Positions are hand-counted from the
//! literal sources (1-based line/column; CRLF, LF, CR, U+2028 and U+2029 each end a line); policy
//! expectations come from STU-MOT-255 (unavailable list) and the V4 rules (escapes decoded first,
//! comment/string decoys, hostile sizes with an iterative scan).
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::*;
use hsk_studio_motion_js::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 60s deadline: {e}"),
    }
}

/// (kind, line, column, token length)
type Violation = (ViolationKind, u32, u32, u32);

fn violation(source: &str) -> Violation {
    let v = ProfileValidator::default()
        .validate(source)
        .expect_err(&format!("expected a violation for {source:?}"));
    (v.kind, v.span.line, v.span.column, v.len)
}

#[test]
fn profile_refuses_disallowed_syntax_with_exact_spans() {
    bounded_case("profile_refuses_disallowed_syntax_with_exact_spans", || {
        use ViolationKind::*;
        let validator = ProfileValidator::default();
        // Admitted profile samples, including decoys that must NOT trip a denial.
        for source in [
            "1+2",
            "a ? 1 : 2",
            "var s = 0; for (let i = 0; i < 3; i++) { s += i } s",
            "function f(x) { return x * 2 } f(2)",
            "[1, 2, 3].map(x => x * 2)",
            "hostRead('comp/layer/transform/position')[0] / 2",
            "(a) / 2",
            "x++ / 2",
            "a-->b",
            "({ get: 1, set: 2 })",
            "({ get() { return 1 } })",
            "({ get, set })",
            "a?.5:1",
            "0xFF + .5 + 1e3 + 1. + 0b101 + 0o17",
            "/* eval(1) */ 1 + 2",
            "// new Function('x')\n1",
            "'/*' + '*/' + 1",
            "\"eval is a word\"",
            "1 /* x */ + 2",
            "ev/**/al",
            "EVAL(1) + Eval",
            "'a\\\nb'",
            "'\\u{1F600}\\x41\\u0041'",
            "'\u{2028}'",
            "[\n1,\r\n2,\r3\n]",
            "try { throw 5 } catch (e) { 7 }",
        ] {
            assert!(validator.validate(source).is_ok(), "{source:?}: {:?}", validator.validate(source));
        }

        // Disallowed constructs: (kind, line, column, length) of the offending token.
        let cases: &[(&str, Violation)] = &[
            ("eval(\"1\")", (DeniedName, 1, 1, 4)),
            ("Function(\"return 1\")", (DeniedName, 1, 1, 8)),
            ("(()=>{}).constructor(\"return 1\")", (DeniedName, 1, 10, 11)),
            ("new Proxy({}, {})", (DeniedKeyword, 1, 1, 3)),
            ("Reflect.get(a, b)", (DeniedName, 1, 1, 7)),
            ("async function f(){}", (DeniedKeyword, 1, 1, 5)),
            ("import(\"x\")", (DeniedKeyword, 1, 1, 6)),
            ("with(a){}", (DeniedKeyword, 1, 1, 4)),
            ("class A {}", (DeniedKeyword, 1, 1, 5)),
            ("this", (DeniedKeyword, 1, 1, 4)),
            ("a.new", (DeniedKeyword, 1, 3, 3)),
            ("setTimeout(f, 1)", (DeniedName, 1, 1, 10)),
            ("globalThis.x", (DeniedName, 1, 1, 10)),
            ("({ get a() { return 1 } })", (Accessor, 1, 4, 3)),
            ("({ a: 1, set b(v) {} })", (Accessor, 1, 10, 3)),
            ("function* g() {}", (GeneratorFunction, 1, 9, 1)),
            ("({ *g() {} })", (GeneratorFunction, 1, 4, 1)),
            ("`x`", (TemplateLiteral, 1, 1, 1)),
            ("f`x`", (TemplateLiteral, 1, 2, 1)),
            ("/a/.test(s)", (RegexLiteral, 1, 1, 1)),
            ("x = /re/", (RegexLiteral, 1, 5, 1)),
            ("if (a) /x/.test(b)", (RegexLiteral, 1, 8, 1)),
            ("a ?. b", (OptionalChaining, 1, 3, 2)),
            ("a ?? b", (PostEs2018Operator, 1, 3, 2)),
            ("#x", (PrivateName, 1, 1, 1)),
            ("@", (InvalidCharacter, 1, 1, 1)),
            ("1_000", (NumericSeparator, 1, 1, 1)),
            ("10n", (BigIntLiteral, 1, 1, 2)),
            ("010", (LegacyOctal, 1, 1, 2)),
            ("3in x", (InvalidNumber, 1, 1, 1)),
            ("1e", (InvalidNumber, 1, 1, 1)),
            ("\"abc", (UnterminatedString, 1, 1, 4)),
            ("'a\nb'", (UnterminatedString, 1, 1, 3)),
            ("'\\1'", (InvalidEscape, 1, 1, 3)),
            ("/* open", (UnterminatedComment, 1, 1, 2)),
            ("(1", (UnexpectedEnd, 1, 3, 0)),
            ("1)", (UnbalancedBracket, 1, 2, 1)),
            ("(]", (UnbalancedBracket, 1, 2, 1)),
            ("\u{e9}", (NonAsciiIdentifier, 1, 1, 1)),
            // V4-08 escapes decoded first: an escaped identifier or string never hides a denied name.
            ("\\u0065val(1)", (IdentifierEscape, 1, 1, 1)),
            ("ev\\u0061l(1)", (IdentifierEscape, 1, 3, 1)),
            ("a[\"con\\x73tructor\"]", (DeniedString, 1, 3, 16)),
            ("a['\\u{63}onstructor']", (DeniedString, 1, 3, 18)),
            ("a[\"__proto__\"]", (DeniedString, 1, 3, 11)),
            ("a[\"con\\\nstructor\"]", (DeniedString, 1, 3, 15)),
            // V4-08 comment and string decoys: positions come from the real token, after the decoy.
            ("'/*' + eval(1)", (DeniedName, 1, 8, 4)),
            ("1 //x\n+ eval(2)", (DeniedName, 2, 3, 4)),
            ("/*\n\n*/ eval(1)", (DeniedName, 3, 4, 4)),
            ("1 /* ' */ + eval(1)", (DeniedName, 1, 13, 4)),
            // V4-08 case, whitespace and line terminators.
            ("1 +\r\n eval(1)", (DeniedName, 2, 2, 4)),
            ("1 +\u{2028}eval(1)", (DeniedName, 2, 1, 4)),
            ("1 +\reval(1)", (DeniedName, 2, 1, 4)),
            ("\u{a0}\t eval(1)", (DeniedName, 1, 4, 4)),
            // HTML-like comments are a lexer difference between engines: refused.
            ("<!-- eval(1)", (HtmlComment, 1, 1, 4)),
            ("1\n--> x", (HtmlComment, 2, 1, 3)),
            ("/* */ -->", (HtmlComment, 1, 7, 3)),
        ];
        for (source, expected) in cases {
            assert_eq!(violation(source), *expected, "{source:?}");
        }

        // Classes decide the Motion code; codes are unique.
        assert_eq!(DeniedName.class(), ViolationClass::Unsupported);
        assert_eq!(UnbalancedBracket.class(), ViolationClass::Parse);
        assert_eq!(TooDeep.class(), ViolationClass::Budget(BudgetKind::Recursion));
        assert_eq!(TooManyTokens.class(), ViolationClass::Budget(BudgetKind::SourceBytes));
        let kinds = [
            UnterminatedString, UnterminatedComment, InvalidEscape, InvalidNumber, LegacyOctal,
            InvalidCharacter, UnbalancedBracket, UnexpectedEnd, DeniedKeyword, DeniedName,
            DeniedString, TemplateLiteral, RegexLiteral, HtmlComment, IdentifierEscape,
            NonAsciiIdentifier, NumericSeparator, BigIntLiteral, OptionalChaining,
            PostEs2018Operator, PrivateName, GeneratorFunction, Accessor, SourceTooLarge,
            TooManyTokens, TooDeep,
        ];
        let mut codes: Vec<_> = kinds.iter().map(|k| k.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), kinds.len());
    });
}

/// Engine double that only counts how often a source reached it.
struct Counting(Arc<AtomicUsize>);

impl Engine for Counting {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity { name: "counting-double", version: "1", native: false }
    }

    fn run(&self, _job: &EvalJob<'_>, _host: &mut dyn HostBridge) -> EngineRun {
        self.0.fetch_add(1, Ordering::AcqRel);
        EngineRun {
            outcome: Ok(JsValue::Number(1.0)),
            realm: RealmReceipt { fresh: true, dropped: true },
        }
    }
}

struct NoHost;

impl HostRead for NoHost {
    fn read(&self, _path: &PropertyPath, _tick: EvalTick, _fuel: &mut Fuel) -> Result<Vec<f64>, ExprError> {
        Err(ExprError::new(ExprErrorCode::ReferenceMissing))
    }
}

fn evaluate(provider: &MotionJsProvider<Counting>, source: &str) -> Result<Vec<f64>, ExprError> {
    let request = ExprRequest { source, revision: 0, tick: EvalTick::new(0), seed: 0, dimension: 1, base: None };
    provider.evaluate(&request, &NoHost, &mut Fuel::new(100), &CancellationToken::default())
}

#[test]
fn profile_bounds_hostile_sizes_and_refusal_precedes_the_engine() {
    bounded_case("profile_bounds_hostile_sizes_and_refusal_precedes_the_engine", || {
        use ViolationKind::*;
        let validator = ProfileValidator::default();
        let limits = validator.limits();
        let kind_at = |source: &str| {
            let v = validator.validate(source).expect_err("expected a violation");
            (v.kind, v.span.line, v.span.column)
        };

        // dims_u32_max-style bounds: the byte, token and depth bounds each fire at an exact position.
        assert_eq!(kind_at(&"1".repeat(limits.max_bytes + 1)), (SourceTooLarge, 1, 1));
        assert_eq!(kind_at(&format!("{}1{}", "(".repeat(10_000), ")".repeat(10_000))), (TooDeep, 1, 65));
        assert_eq!(kind_at(&format!("{}1", "1+".repeat(10_001))), (TooManyTokens, 1, 20_001));
        assert_eq!(kind_at(&"\u{e9}".repeat(30_000)), (NonAsciiIdentifier, 1, 1));
        let exact = format!("{}1{}", "(".repeat(64), ")".repeat(64));
        assert_eq!(validator.validate(&exact).map(|v| v.max_depth), Ok(64));

        // aggregate_over_budget-style: a long operator chain is scanned iteratively (no recursion on
        // hostile depth), refused by the token bound with default bounds and accepted with explicit ones.
        let chain = format!("{}1", "!".repeat(900_000));
        assert_eq!(kind_at(&chain).0, SourceTooLarge);
        let wide = ProfileValidator::new(ProfileLimits { max_bytes: 1 << 20, max_tokens: 1 << 20, max_depth: 1024 })
            .expect("explicit nonzero bounds");
        assert_eq!(wide.validate(&chain).map(|v| v.tokens), Ok(900_001));
        let nested = format!("{}1{}", "[".repeat(1024), "]".repeat(1024));
        assert_eq!(wide.validate(&nested).map(|v| v.max_depth), Ok(1024));
        // Fail closed on configuration: zero and above-ceiling bounds are rejected.
        let zero = ProfileLimits { max_tokens: 0, ..limits };
        assert_eq!(ProfileValidator::new(zero), Err(ConfigError::ZeroLimit("profile_max_tokens")));
        let high = ProfileLimits { max_depth: MAX_DEPTH_CEILING + 1, ..limits };
        assert_eq!(ProfileValidator::new(high), Err(ConfigError::AboveCeiling("profile_max_depth")));

        // Refusal happens in the provider BEFORE the engine: the engine never sees refused source and
        // each refusal is typed with its exact position.
        let runs = Arc::new(AtomicUsize::new(0));
        let provider = MotionJsProvider::new(Counting(Arc::clone(&runs)), Quotas::default());
        assert!(provider.validator().is_some());
        assert_eq!(evaluate(&provider, "1+2"), Ok(vec![1.0]));
        assert_eq!(runs.load(Ordering::Acquire), 1);
        let refused = |source: &str| evaluate(&provider, source).expect_err("expected a refusal");
        assert_eq!(refused("eval(\"1\")"), ExprError::at(ExprErrorCode::Unsupported, 1, 1));
        assert_eq!(refused("1 +\n\n  new Function(\"x\")"), ExprError::at(ExprErrorCode::Unsupported, 3, 3));
        assert_eq!(refused("(1"), ExprError::at(ExprErrorCode::Parse, 1, 3));
        let deep = format!("{}1{}", "(".repeat(100), ")".repeat(100));
        let too_deep = refused(&deep);
        assert_eq!((too_deep.code, too_deep.span.map(|s| s.column)), (ExprErrorCode::Budget, Some(65)));
        assert_eq!(runs.load(Ordering::Acquire), 1, "the engine ran for a refused source");
        let stats = provider.stats();
        assert_eq!((stats.validation_rejected, stats.realms_started, stats.denied(BudgetKind::Recursion)), (4, 1, 1));

        // Opting out is explicit and observable: the same source then reaches the engine.
        let open = MotionJsProvider::new(Counting(Arc::clone(&runs)), Quotas::default()).without_validator();
        assert!(open.validator().is_none());
        assert_eq!(evaluate(&open, "eval(\"1\")"), Ok(vec![1.0]));
        assert_eq!(runs.load(Ordering::Acquire), 2);
        // A non-JavaScript double is never validated as JavaScript.
        assert!(MotionJsProvider::new(ScriptedEngine::new(), Quotas::default()).validator().is_none());
        assert_eq!(ScriptedEngine::new().source_language(), SourceLanguage::Scripted);
        assert_eq!(Counting(runs).source_language(), SourceLanguage::JavaScript);
    });
}

/// STUDIO-R7 pre-review B, SMB-MOTION-JS N1: a raw U+0000 inside a string literal or a comment used to
/// pass the validator and then fail the engine wrapper (`CString::new`), which poisoned the provider.
/// The validator now refuses a raw NUL anywhere with the exact position of the NUL; a NUL written as an
/// ESCAPE is a different source text (no NUL character in it) and stays admitted. Expectations are
/// hand-counted from the literals (1-based; columns count scalar values).
#[test]
fn profile_refuses_a_raw_nul_anywhere_and_keeps_nul_escapes() {
    bounded_case("profile_refuses_a_raw_nul_anywhere_and_keeps_nul_escapes", || {
        let validator = ProfileValidator::default();
        // (source, line, column of the NUL)
        let cases: &[(&str, u32, u32)] = &[
            ("var s = \"a\0b\"; 1", 1, 11),
            ("var s = 'a\0b'; 1", 1, 11),
            ("1 // \0", 1, 6),
            ("1 // \0 trailing", 1, 6),
            ("1 /* \0 */", 1, 6),
            ("/* a\n b\r\n \0 */ 1", 3, 2),
            ("1 +\r\n\"x\0\"", 2, 3),
            ("1;\u{2028}\u{2028}'\0'", 3, 2),
            ("'\\\0'", 1, 3),
            ("1 +\0 2", 1, 4),
            ("ab\0c", 1, 3),
            ("'\u{e9}\0'", 1, 3),
            ("\0", 1, 1),
            ("1\r\0", 2, 1),
            // The NUL check is a whole-source precondition (like the byte bound): it is reported even
            // when an earlier token is also outside the profile.
            ("eval(1) // \0", 1, 12),
        ];
        for (source, line, column) in cases {
            let violation = validator
                .validate(source)
                .expect_err(&format!("a raw NUL must be refused: {source:?}"));
            assert_eq!(
                (violation.kind.code(), violation.span.line, violation.span.column, violation.len),
                ("nul_character", *line, *column, 1),
                "{source:?}"
            );
            assert_eq!(violation.kind.class(), ViolationClass::Parse, "{source:?}");
        }
        // A NUL written as an escape is not a NUL in the source: admitted (and the engine test
        // quickjs_nul_in_source_is_a_refusal_not_a_poison proves it still evaluates).
        for source in ["'a\\0b'.length", "'a\\x00b'", "'a\\u0000b'", "'a\\u{0}b'", "'\\0'", "\"\\0\" + \"\\0\""] {
            assert!(validator.validate(source).is_ok(), "{source:?}: {:?}", validator.validate(source));
        }
        // The byte bound still outranks the NUL scan (hostile size first, nothing scanned).
        let limits = validator.limits();
        let huge = format!("{}\0", "1".repeat(limits.max_bytes));
        assert_eq!(
            validator.validate(&huge).expect_err("oversize").kind.code(),
            "source_too_large"
        );
    });
}

/// The provider refuses a NUL-bearing source BEFORE the engine, typed Parse with the NUL position, and
/// stays usable (not poisoned) after each refusal.
#[test]
fn profile_nul_refusal_precedes_the_engine_and_never_poisons() {
    bounded_case("profile_nul_refusal_precedes_the_engine_and_never_poisons", || {
        let runs = Arc::new(AtomicUsize::new(0));
        let provider = MotionJsProvider::new(Counting(Arc::clone(&runs)), Quotas::default());
        assert_eq!(evaluate(&provider, "1+2"), Ok(vec![1.0]));
        let (mut engine_runs, mut refused) = (1_usize, 0_u64);
        assert_eq!(runs.load(Ordering::Acquire), engine_runs);
        let sources: &[(&str, u32, u32)] = &[("var s = \"a\0b\"; 1", 1, 11), ("1 // \0", 1, 6), ("1 /* \0 */", 1, 6)];
        for (source, line, column) in sources {
            assert_eq!(
                evaluate(&provider, source),
                Err(ExprError::at(ExprErrorCode::Parse, *line, *column)),
                "{source:?}"
            );
            refused += 1;
            assert!(!provider.is_poisoned(), "poisoned by {source:?}");
            assert_eq!(runs.load(Ordering::Acquire), engine_runs, "the engine saw {source:?}");
            assert_eq!(provider.stats().validation_rejected, refused);
            assert_eq!(evaluate(&provider, "1+2"), Ok(vec![1.0]), "provider unusable after {source:?}");
            engine_runs += 1;
        }
        let report = provider.retire();
        assert!(report.is_clean(), "{report:?}");
    });
}
