//! Lane SMB-WEB-CODE-1: [STU-WEB-052] to [STU-WEB-058] and [STU-WEB-023] (rule sets, value map, linear-time patterns,
//! diagnostics, linter configurations and runs).
use hsk_studio_accord::CancellationToken;
use hsk_studio_web_core::doc_types::DtdContext;
use hsk_studio_web_core::validation::{
    self, CSS_LINT_RULES, ContextRule, Language, MARKUP_LINT_RULES, Pattern, QuoteRule, Requirement, RuleSet, SCRIPT_LEVELS, SCRIPT_LINT_DEFAULTS, ScriptDefault, Severity, TagRule, ValueMapEntry,
};
use hsk_studio_web_core::vocabulary::TagRegistry;
use hsk_studio_web_core::{Limits, WebDoc, parse};
use std::collections::BTreeMap;

fn doc(src: &str) -> WebDoc {
    parse(src, 1, &Limits::default(), &CancellationToken::default()).unwrap()
}

fn codes(r: &validation::Report) -> Vec<(&str, &str)> {
    r.diagnostics.iter().map(|d| (d.code, d.rule_id.as_str())).collect()
}

#[test]
fn patterns_match_whole_values_in_linear_time() {
    let p = Pattern::compile("#[0-9a-fA-F]{3}|#[0-9a-fA-F]{6}|[a-z]+").unwrap();
    assert!(p.matches("#fff") && p.matches("#A0b1C2") && p.matches("red"));
    assert!(!p.matches("#ffff") && !p.matches("red!") && !p.matches(""));
    let q = Pattern::compile("^[-+]?\\d+(\\.\\d*)?$").unwrap();
    assert!(q.matches("-12.5") && q.matches("7") && !q.matches("1.2.3"));
    assert!(Pattern::compile("a?b*c+").unwrap().matches("bbcc"));
    assert!(Pattern::compile("[^ ]{2,3}").unwrap().matches("ab") && !Pattern::compile("[^ ]{2,3}").unwrap().matches("a b"));
    // A pattern that backtracks exponentially elsewhere is linear here.
    let evil = Pattern::compile("(a*)*b").unwrap();
    let start = std::time::Instant::now();
    assert!(!evil.matches(&"a".repeat(20_000)));
    assert!(start.elapsed().as_secs() < 5, "linear: {:?}", start.elapsed());
    assert!(!evil.matches(&"a".repeat(validation::MAX_VALUE + 1)), "over the bound: no match, no work");
    let code = |t: &str| Pattern::compile(t).unwrap_err().code;
    assert_eq!(code("(a)\\1"), "pattern_unsupported");
    assert_eq!(code("(?=a)"), "pattern_unsupported");
    assert_eq!(code("a*?"), "pattern_unsupported");
    assert_eq!(code("a{1,1000}"), "pattern_too_large");
    assert_eq!(code(&"(".repeat(40)), "pattern_too_large");
    assert_eq!(code("(ab"), "pattern_syntax");
    assert_eq!(code("[z-a]"), "pattern_syntax");
    assert_eq!(code(&"x".repeat(600)), "pattern_too_large");
}

#[test]
fn the_value_map_keeps_its_four_quote_rules() {
    assert_eq!(QuoteRule::parse("Must Not Be Quoted"), Some(QuoteRule::MustNotBeQuoted));
    assert_eq!(QuoteRule::parse("must be double"), None);
    let key = ValueMapEntry::new("size", "digits", QuoteRule::MustBeDouble, true, "[0-9]+").unwrap();
    assert!(key.accepts("12") && !key.accepts("1x"));
    let literal = ValueMapEntry::new("on", "literal", QuoteRule::MayBeQuoted, false, "on").unwrap();
    assert!(literal.accepts("ON") && !literal.accepts("off"));
    assert_eq!(ValueMapEntry::new("bad", "", QuoteRule::MayBeQuoted, true, "(a").unwrap_err().code, "pattern_syntax");
}

#[test]
fn validation_reports_typed_diagnostics_and_never_rewrites() {
    let src = "<!doctype html><html><head><title>t</title><meta charset=utf-8 content=x></head><body><li>x</li><img alt=a><input type=slider data-x=1 aria-label=y onclick=f()><blink>z</blink><table><tr><td colspan=two>c</td></tr></table><my-el>c</my-el><p bogus=1>q</p></body></html>";
    let d = doc(src);
    let report = validation::validate(&d, &TagRegistry::shipped(), "html", &validation::html_all(), None);
    assert_eq!(d.source(), src, "never a rewrite");
    let got = codes(&report);
    for want in [
        ("attribute_combination_invalid", "meta.invalid"),
        ("tag_context_violation", "li.context"),
        ("attribute_required_set_unsatisfied", "img.requires.all"),
        ("attribute_value_not_allowed", "input.type.values"),
        ("tag_unknown", "blink.known"),
        ("attribute_value_shape", "td.colspan.shape"),
        ("attribute_unknown", "p.bogus.known"),
    ] {
        assert!(got.contains(&want), "{want:?} in {got:?}");
    }
    assert_eq!(got.len(), 7, "data-*, aria-*, event handlers and custom elements are not findings: {got:?}");
    let value = report.diagnostics.iter().find(|x| x.code == "attribute_value_not_allowed").unwrap();
    assert_eq!(value.severity, Severity::Warning);
    assert!(value.message.contains("type=\"slider\"") && value.message.contains("checkbox"), "names the attribute, value and set: {}", value.message);
    assert_eq!(&src[value.byte_range.clone()], "slider");
    let li = report.diagnostics.iter().find(|x| x.code == "tag_context_violation").unwrap();
    assert_eq!((li.severity, li.rule_set_id.as_str(), li.element_path.as_deref().map(|p| p.ends_with("li"))), (Severity::Error, "html_all", Some(true)));
    // A subtree scope.
    let table = d.find_by_tag("table")[0];
    let scoped = validation::validate(&d, &TagRegistry::shipped(), "html", &validation::html_all(), Some(table));
    assert_eq!(codes(&scoped), [("attribute_value_shape", "td.colspan.shape")]);
}

#[test]
fn a_registered_rule_set_drives_quotes_requirements_and_context() {
    let set = RuleSet {
        rule_set_id: "kit".into(),
        dtd_context: DtdContext::Html,
        version: "1".into(),
        tags: vec![TagRule {
            tag: "span".into(),
            versions: vec!["1".into()],
            attributes: vec![("title".into(), "word".into())],
            options: Vec::new(),
            context: Some(ContextRule { parents: Vec::new(), ancestors: vec!["section".into()] }),
            invalid: Vec::new(),
            requirements: vec![(Requirement::ExactlyOne, vec!["id".into(), "class".into()])],
        }],
        value_map: vec![ValueMapEntry::new("word", "letters", QuoteRule::MustBeDouble, true, "[a-z]+").unwrap()],
    };
    let d = doc("<section><div><span id=a title='ok'>x</span></div></section><span id=b class=c title=\"OK\">y</span>");
    let report = validation::validate(&d, &TagRegistry::shipped(), "html", &set, None);
    let got = codes(&report);
    assert_eq!(got, [("attribute_quote_rule", "span.title.quote"), ("attribute_value_shape", "span.title.shape"), ("tag_context_violation", "span.context"), ("attribute_required_set_unsatisfied", "span.requires.exactly_one")]);
}

#[test]
fn the_linter_configurations_are_the_spec_lists() {
    assert_eq!((CSS_LINT_RULES.len(), CSS_LINT_RULES.iter().filter(|r| r.1).count()), (35, 3));
    assert_eq!((MARKUP_LINT_RULES.len(), MARKUP_LINT_RULES.iter().filter(|r| r.1).count()), (23, 9));
    assert_eq!(SCRIPT_LINT_DEFAULTS.len(), 69);
    let get = |k: &str| SCRIPT_LINT_DEFAULTS.iter().find(|(n, _)| *n == k).unwrap().1;
    assert_eq!((get("maxerr"), get("indent"), get("maxlen"), get("globals"), get("mocha"), get("node")), (ScriptDefault::Number(50), ScriptDefault::Number(4), ScriptDefault::Unset, ScriptDefault::EmptyMap, ScriptDefault::Bool(true), ScriptDefault::Bool(false)));
    assert_eq!(SCRIPT_LINT_DEFAULTS.iter().filter(|(_, v)| *v == ScriptDefault::Unset).count(), 5, "five limit options, unset not zero");
    assert_eq!(SCRIPT_LEVELS.map(|l| l.ecma_version), ["3", "5", "6", "current"]);
    assert!(SCRIPT_LEVELS.iter().all(|l| l.source_type == "module" && l.jsx && l.environments == ["browser", "jquery"] && l.error_rules == ["constructor-super"]));
}

#[test]
fn markup_lint_runs_the_nine_default_rules() {
    let src = "<HTML><head><title>t</title></head><body><img src=\"\" id=a><p id='a'>1 > 0</p><area href=x></body></HTML>";
    let d = doc(src);
    let report = validation::lint(&d, Language::Markup, &BTreeMap::new(), None).unwrap();
    let rules: Vec<&str> = report.diagnostics.iter().map(|x| x.rule_id.as_str()).collect();
    for want in ["doctype-first", "tagname-lowercase", "attr-value-double-quotes", "id-unique", "src-not-empty", "alt-require", "spec-char-escape"] {
        assert!(rules.contains(&want), "{want} in {rules:?}");
    }
    assert!(report.not_run.is_empty(), "every default markup rule runs: {:?}", report.not_run);
    assert!(report.diagnostics.iter().all(|x| x.rule_set_id == "markup" && x.code == "lint"));
    // Overrides: one rule off, one disabled rule on; a rule this module does not run is listed, never clean.
    let mut o = BTreeMap::new();
    o.insert("tagname-lowercase".to_string(), false);
    o.insert("inline-style-disabled".to_string(), true);
    o.insert("href-abs-or-rel".to_string(), true);
    let d2 = doc("<!doctype html><p style=\"top:0\">x</p>");
    let r2 = validation::lint(&d2, Language::Markup, &o, None).unwrap();
    assert!(r2.diagnostics.iter().any(|x| x.rule_id == "inline-style-disabled"));
    assert_eq!(r2.not_run, ["href-abs-or-rel"]);
    o.insert("no-such-rule".to_string(), true);
    assert_eq!(validation::lint(&d2, Language::Markup, &o, None).unwrap_err().code, "lint_rule_unknown");
    assert_eq!(validation::lint(&d2, Language::Script, &BTreeMap::new(), None).unwrap_err().code, "script_lint_provider_unselected");
}

#[test]
fn css_lint_runs_its_three_default_rules() {
    let floats: String = (0..10).map(|i| format!(".f{i} {{ float: left }} ")).collect();
    let src = format!("<style>.a {{ color: red; color: red }} .b {{ -webkit-box-sizing: border-box }} .c {{ color: #fff; color: rgba(0,0,0,.5) }} {floats}</style>");
    let d = doc(&src);
    let report = validation::lint(&d, Language::Css, &BTreeMap::new(), None).unwrap();
    let rules: Vec<&str> = report.diagnostics.iter().map(|x| x.rule_id.as_str()).collect();
    assert_eq!(rules.iter().filter(|r| **r == "duplicate-properties").count(), 1, "a fallback directly after is no finding: {rules:?}");
    assert!(rules.contains(&"vendor-prefix") && rules.contains(&"floats"), "{rules:?}");
    assert_eq!(report.diagnostics.iter().find(|x| x.rule_id == "duplicate-properties").map(|x| &src[x.byte_range.clone()]), Some("color: red"));
    let broken = doc("<style>p { color: red</style>");
    let r = validation::lint(&broken, Language::Css, &BTreeMap::new(), None).unwrap();
    assert_eq!(r.diagnostics.iter().map(|x| x.rule_id.as_str()).collect::<Vec<_>>(), ["errors"], "a sheet that does not scan is reported, never skipped");
}
