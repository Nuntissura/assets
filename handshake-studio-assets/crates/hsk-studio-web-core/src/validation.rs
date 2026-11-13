//! Markup validation and linting (lane SMB-WEB-CODE-1; [STU-WEB-052] to [STU-WEB-058], [STU-WEB-023]): declarative
//! validator rule sets with the value map, a linear-time pattern engine for the map's regular-expression keys, typed
//! diagnostics (never a rewrite), the three linters' exact shipped default configurations and the markup and CSS
//! rules this module runs.
//!
//! Shipped rule set: `html_all`, Studio-authored from the WHATWG HTML content models (CC BY 4.0) for the HTML elements
//! of the shipped vocabulary: tag-context rules, attribute-requirement sets, one invalid combination and a small value
//! map. The vendor's rule sets (the union `validator`, the three XHTML 1.0 sets, `cfml`, `wml`, `smil`, the 433-key value
//! map and the 20 version keys) are not in the spec and are not reproduced; their counts are not reached. The record
//! shapes take any of them ([`RuleSet`], [`ValueMapEntry`]).
//!
//! Linters: the configurations are the spec's exact lists ([`CSS_LINT_RULES`], [`MARKUP_LINT_RULES`],
//! [`SCRIPT_LINT_DEFAULTS`], [`SCRIPT_LEVELS`]). The markup linter runs all nine rules enabled by default and eight of
//! the disabled ones; the CSS linter runs its three enabled rules and eight disabled ones; a rule that does not run is
//! listed in `not_run`, never reported as clean. Rule semantics are Studio's reading of each rule name (thresholds
//! labelled Studio-declared); the vendor's linter behaviour is UNVERIFIED (D5). The script linter needs a pinned Rust
//! lint provider, which [STU-WEB-057] leaves unselected: a script lint run is refused `script_lint_provider_unselected`.
//! Site-wide batch linting ([STU-WEB-058]) is not here: a run covers a document or one element's subtree.
#![deny(clippy::arithmetic_side_effects)]

use crate::css_properties::{self, ItemKind};
use crate::doc_types::{CodeError, DtdContext};
use crate::dom::{ContentModel, Namespace, NodeId, NodeKind, Quote, WebDoc};
use crate::vocabulary::{TagRegistry, ValueType};
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

// ---------------------------------------------------------------------------------------------------------------------
// Linear-time patterns ([STU-WEB-054]).

/// Longest pattern text and compiled program a value-map key may have.
const MAX_PATTERN: usize = 512;
const MAX_PROGRAM: usize = 4096;
const MAX_REPEAT: u32 = 100;
const MAX_NESTING: usize = 32;
/// Longest value a pattern is run over.
pub const MAX_VALUE: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Inst {
    Char(char),
    Any,
    Class(Vec<(char, char)>, bool),
    Split(usize, usize),
    Jmp(usize),
    Match,
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class(Vec<(char, char)>, bool),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat(Box<Node>, u32, Option<u32>),
}

/// A compiled pattern: a subset of regular expressions (literals, `.`, classes, `\d \w \s` and their negations,
/// groups, `|`, `* + ?` and `{m}`, `{m,}`, `{m,n}` up to 100), always matched against the whole value by a Pike VM in
/// time linear in the value for a fixed pattern. Back-references, look-around and lazy quantifiers are refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    prog: Vec<Inst>,
}

struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    depth: usize,
    _src: &'a str,
}

fn perr() -> CodeError {
    CodeError::new("pattern_syntax")
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        self.at = self.at.saturating_add(1);
        c
    }

    fn alt(&mut self) -> Result<Node, CodeError> {
        self.depth = self.depth.saturating_add(1);
        if self.depth > MAX_NESTING {
            return Err(CodeError::new("pattern_too_large"));
        }
        let mut alts = vec![self.concat()?];
        while self.peek() == Some('|') {
            self.bump();
            alts.push(self.concat()?);
        }
        self.depth = self.depth.saturating_sub(1);
        Ok(if alts.len() == 1 { alts.pop().unwrap_or(Node::Empty) } else { Node::Alt(alts) })
    }

    fn concat(&mut self) -> Result<Node, CodeError> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            items.push(self.repeat(atom)?);
        }
        Ok(Node::Concat(items))
    }

    fn number(&mut self) -> Result<u32, CodeError> {
        let mut n: u32 = 0;
        let mut seen = false;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            self.bump();
            seen = true;
            n = n.saturating_mul(10).saturating_add(d);
        }
        if !seen {
            return Err(perr());
        }
        Ok(n)
    }

    fn repeat(&mut self, atom: Node) -> Result<Node, CodeError> {
        let (min, max) = match self.peek() {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') => {
                self.bump();
                let min = self.number()?;
                let max = if self.peek() == Some(',') {
                    self.bump();
                    if self.peek() == Some('}') { None } else { Some(self.number()?) }
                } else {
                    Some(min)
                };
                if self.peek() != Some('}') {
                    return Err(perr());
                }
                if min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT || m < min) {
                    return Err(CodeError::new("pattern_too_large"));
                }
                (min, max)
            }
            _ => return Ok(atom),
        };
        self.bump();
        if matches!(self.peek(), Some('?' | '+' | '*' | '{')) {
            return Err(CodeError::new("pattern_unsupported"));
        }
        Ok(Node::Repeat(Box::new(atom), min, max))
    }

    fn escape_class(c: char) -> Option<(Vec<(char, char)>, bool)> {
        let digits = vec![('0', '9')];
        let word = vec![('0', '9'), ('A', 'Z'), ('_', '_'), ('a', 'z')];
        let space = vec![('\t', '\r'), (' ', ' ')];
        match c {
            'd' => Some((digits, false)),
            'D' => Some((digits, true)),
            'w' => Some((word, false)),
            'W' => Some((word, true)),
            's' => Some((space, false)),
            'S' => Some((space, true)),
            _ => None,
        }
    }

    fn escaped(&mut self) -> Result<Node, CodeError> {
        let c = self.bump().ok_or(perr())?;
        if let Some((ranges, neg)) = Self::escape_class(c) {
            return Ok(Node::Class(ranges, neg));
        }
        match c {
            'n' => Ok(Node::Char('\n')),
            't' => Ok(Node::Char('\t')),
            'r' => Ok(Node::Char('\r')),
            c if c.is_ascii_digit() || c.is_ascii_alphabetic() => Err(CodeError::new("pattern_unsupported")),
            c => Ok(Node::Char(c)),
        }
    }

    fn atom(&mut self) -> Result<Node, CodeError> {
        match self.bump().ok_or(perr())? {
            '(' => {
                if self.peek() == Some('?') {
                    return Err(CodeError::new("pattern_unsupported"));
                }
                let inner = self.alt()?;
                if self.bump() != Some(')') {
                    return Err(perr());
                }
                Ok(inner)
            }
            '.' => Ok(Node::Any),
            '\\' => self.escaped(),
            '[' => self.class(),
            '^' if self.at == 1 => Ok(Node::Empty),
            '$' if self.at == self.chars.len() => Ok(Node::Empty),
            '*' | '+' | '?' | '{' | ')' | '^' | '$' => Err(perr()),
            c => Ok(Node::Char(c)),
        }
    }

    fn class(&mut self) -> Result<Node, CodeError> {
        let negated = self.peek() == Some('^');
        if negated {
            self.bump();
        }
        let mut ranges = Vec::new();
        let mut first = true;
        loop {
            let c = self.bump().ok_or(perr())?;
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' {
                let e = self.bump().ok_or(perr())?;
                if let Some((r, false)) = Self::escape_class(e) {
                    ranges.extend(r);
                    continue;
                }
                if e.is_ascii_alphanumeric() {
                    return Err(CodeError::new("pattern_unsupported"));
                }
                e
            } else {
                c
            };
            if self.peek() == Some('-') && self.chars.get(self.at.saturating_add(1)).is_some_and(|n| *n != ']') {
                self.bump();
                let hi = match self.bump().ok_or(perr())? {
                    '\\' => self.bump().ok_or(perr())?,
                    h => h,
                };
                if hi < lo {
                    return Err(perr());
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
            if ranges.len() > 256 {
                return Err(CodeError::new("pattern_too_large"));
            }
        }
        Ok(Node::Class(ranges, negated))
    }
}

fn emit(prog: &mut Vec<Inst>, inst: Inst) -> Result<usize, CodeError> {
    if prog.len() >= MAX_PROGRAM {
        return Err(CodeError::new("pattern_too_large"));
    }
    prog.push(inst);
    Ok(prog.len().saturating_sub(1))
}

fn compile(node: &Node, prog: &mut Vec<Inst>) -> Result<(), CodeError> {
    match node {
        Node::Empty => {}
        Node::Char(c) => {
            emit(prog, Inst::Char(*c))?;
        }
        Node::Any => {
            emit(prog, Inst::Any)?;
        }
        Node::Class(r, n) => {
            emit(prog, Inst::Class(r.clone(), *n))?;
        }
        Node::Concat(items) => {
            for i in items {
                compile(i, prog)?;
            }
        }
        Node::Alt(alts) => {
            let mut jumps = Vec::new();
            for (i, a) in alts.iter().enumerate() {
                if i.saturating_add(1) < alts.len() {
                    let split = emit(prog, Inst::Split(0, 0))?;
                    let l1 = prog.len();
                    compile(a, prog)?;
                    jumps.push(emit(prog, Inst::Jmp(0))?);
                    let l2 = prog.len();
                    prog[split] = Inst::Split(l1, l2);
                } else {
                    compile(a, prog)?;
                }
            }
            let end = prog.len();
            for j in jumps {
                prog[j] = Inst::Jmp(end);
            }
        }
        Node::Repeat(inner, min, max) => {
            for _ in 0..*min {
                compile(inner, prog)?;
            }
            match max {
                None => {
                    let split = emit(prog, Inst::Split(0, 0))?;
                    compile(inner, prog)?;
                    emit(prog, Inst::Jmp(split))?;
                    let out = prog.len();
                    prog[split] = Inst::Split(split.saturating_add(1), out);
                }
                Some(max) => {
                    let mut splits = Vec::new();
                    for _ in *min..*max {
                        splits.push(emit(prog, Inst::Split(0, 0))?);
                        compile(inner, prog)?;
                    }
                    let out = prog.len();
                    for s in splits {
                        prog[s] = Inst::Split(s.saturating_add(1), out);
                    }
                }
            }
        }
    }
    Ok(())
}

impl Pattern {
    pub fn compile(text: &str) -> Result<Self, CodeError> {
        if text.len() > MAX_PATTERN {
            return Err(CodeError::new("pattern_too_large"));
        }
        let mut p = Parser { chars: text.chars().collect(), at: 0, depth: 0, _src: text };
        let node = p.alt()?;
        if p.at < p.chars.len() {
            return Err(perr());
        }
        let mut prog = Vec::new();
        compile(&node, &mut prog)?;
        emit(&mut prog, Inst::Match)?;
        Ok(Self { prog })
    }

    /// Whether the whole of `value` matches (a value over [`MAX_VALUE`] bytes never does).
    pub fn matches(&self, value: &str) -> bool {
        if value.len() > MAX_VALUE {
            return false;
        }
        let n = self.prog.len();
        let mut clist: Vec<usize> = Vec::with_capacity(n);
        let mut nlist: Vec<usize> = Vec::with_capacity(n);
        let mut mark = vec![usize::MAX; n];
        let mut step = 0usize;
        self.add(&mut clist, &mut mark, step, 0);
        for c in value.chars() {
            step = step.saturating_add(1);
            nlist.clear();
            for &pc in &clist {
                let ok = match &self.prog[pc] {
                    Inst::Char(x) => *x == c,
                    Inst::Any => true,
                    Inst::Class(r, neg) => r.iter().any(|(lo, hi)| *lo <= c && c <= *hi) != *neg,
                    _ => false,
                };
                if ok {
                    self.add(&mut nlist, &mut mark, step, pc.saturating_add(1));
                }
            }
            std::mem::swap(&mut clist, &mut nlist);
            if clist.is_empty() {
                return false;
            }
        }
        clist.iter().any(|&pc| self.prog[pc] == Inst::Match)
    }

    /// Adds `pc` and everything its splits and jumps reach (once per step).
    fn add(&self, list: &mut Vec<usize>, mark: &mut [usize], step: usize, pc: usize) {
        let mut stack = vec![pc];
        while let Some(pc) = stack.pop() {
            if pc >= self.prog.len() || mark[pc] == step {
                continue;
            }
            mark[pc] = step;
            match self.prog[pc] {
                Inst::Split(a, b) => {
                    stack.push(b);
                    stack.push(a);
                }
                Inst::Jmp(a) => stack.push(a),
                _ => list.push(pc),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Rule sets ([STU-WEB-052] to [STU-WEB-054]).

/// [STU-WEB-054] `quote_rule`, closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuoteRule {
    MayBeDouble,
    MayBeQuoted,
    MustBeDouble,
    MustNotBeQuoted,
}

impl QuoteRule {
    pub const fn token(self) -> &'static str {
        match self {
            Self::MayBeDouble => "May Be Double",
            Self::MayBeQuoted => "May Be Quoted",
            Self::MustBeDouble => "Must Be Double",
            Self::MustNotBeQuoted => "Must Not Be Quoted",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        [Self::MayBeDouble, Self::MayBeQuoted, Self::MustBeDouble, Self::MustNotBeQuoted].into_iter().find(|q| q.token() == token)
    }

    /// Whether an attribute written with `quote` keeps the rule.
    fn allows(self, quote: Quote) -> bool {
        match self {
            Self::MayBeDouble => matches!(quote, Quote::Double | Quote::Unquoted | Quote::Absent),
            Self::MayBeQuoted => true,
            Self::MustBeDouble => quote == Quote::Double,
            Self::MustNotBeQuoted => matches!(quote, Quote::Unquoted | Quote::Absent),
        }
    }
}

/// [STU-WEB-054]: one value-map key. `pattern` is compiled when `is_regex` (a pattern the engine cannot bound is
/// refused at load, never admitted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueMapEntry {
    pub key_id: String,
    pub note: String,
    pub quote_rule: QuoteRule,
    pub is_regex: bool,
    pub value_type: String,
    pattern: Option<Pattern>,
}

impl ValueMapEntry {
    pub fn new(key_id: &str, note: &str, quote_rule: QuoteRule, is_regex: bool, value_type: &str) -> Result<Self, CodeError> {
        let pattern = if is_regex { Some(Pattern::compile(value_type)?) } else { None };
        Ok(Self { key_id: key_id.into(), note: note.into(), quote_rule, is_regex, value_type: value_type.into(), pattern })
    }

    /// Whether `value` has this key's shape (a literal compares ASCII case-insensitively).
    pub fn accepts(&self, value: &str) -> bool {
        match &self.pattern {
            Some(p) => p.matches(value),
            None => self.value_type.eq_ignore_ascii_case(value),
        }
    }
}

/// [STU-WEB-053] attribute-requirement kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requirement {
    AtLeastOne,
    ExactlyOne,
    All,
}

impl Requirement {
    pub const fn token(self) -> &'static str {
        match self {
            Self::AtLeastOne => "at_least_one",
            Self::ExactlyOne => "exactly_one",
            Self::All => "all",
        }
    }
}

/// [STU-WEB-053]: a tag-context rule: legal when the parent is one of `parents` or an ancestor one of `ancestors`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextRule {
    pub parents: Vec<String>,
    pub ancestors: Vec<String>,
}

/// [STU-WEB-053]: one tag's rules in a rule set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagRule {
    pub tag: String,
    /// Version applicability (the set's version keys this tag is valid in).
    pub versions: Vec<String>,
    /// Attribute name and the value-map key its value must have.
    pub attributes: Vec<(String, String)>,
    pub options: Vec<String>,
    pub context: Option<ContextRule>,
    /// Attribute combinations that must not occur together.
    pub invalid: Vec<Vec<String>>,
    pub requirements: Vec<(Requirement, Vec<String>)>,
}

/// [STU-WEB-052]: a rule set for one DTD context and version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleSet {
    pub rule_set_id: String,
    pub dtd_context: DtdContext,
    pub version: String,
    pub tags: Vec<TagRule>,
    pub value_map: Vec<ValueMapEntry>,
}

/// [STU-WEB-055] severities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// [STU-WEB-055], [STU-WEB-058]: one diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub rule_set_id: String,
    pub rule_id: String,
    pub byte_range: Range<usize>,
    pub element_path: Option<String>,
    pub message: String,
    pub code: &'static str,
}

/// Diagnostics one run keeps; the rest are counted.
pub const MAX_DIAGNOSTICS: usize = 2000;

/// A validation or lint run's result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub diagnostics: Vec<Diagnostic>,
    pub truncated: usize,
    /// Rules enabled for the run that this module does not run (never reported as clean).
    pub not_run: Vec<String>,
}

impl Report {
    fn push(&mut self, d: Diagnostic) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics.push(d);
        } else {
            self.truncated = self.truncated.saturating_add(1);
        }
    }
}

fn s(list: &[&str]) -> Vec<String> {
    list.iter().map(|x| (*x).to_string()).collect()
}

/// The shipped `html_all` rule set (module docs).
pub fn html_all() -> RuleSet {
    let entry = |k: &str, note: &str, re: &str| ValueMapEntry::new(k, note, QuoteRule::MayBeQuoted, true, re).unwrap_or_else(|_| ValueMapEntry { key_id: k.into(), note: note.into(), quote_rule: QuoteRule::MayBeQuoted, is_regex: false, value_type: String::new(), pattern: None });
    let value_map = vec![
        entry("non_negative_integer", "digits", "[0-9]+"),
        entry("integer", "optional sign and digits", "[-+]?[0-9]+"),
        entry("color", "#rgb, #rrggbb or a colour keyword", "#[0-9a-fA-F]{3}|#[0-9a-fA-F]{6}|[a-zA-Z]+"),
        entry("cdata", "any text", ".*"),
    ];
    let ctx = |parents: &[&str], ancestors: &[&str]| Some(ContextRule { parents: s(parents), ancestors: s(ancestors) });
    let rule = |tag: &str, context: Option<ContextRule>| TagRule { tag: tag.into(), versions: s(&["html5"]), attributes: Vec::new(), options: Vec::new(), context, invalid: Vec::new(), requirements: Vec::new() };
    let mut tags = vec![
        rule("li", ctx(&["ul", "ol", "menu"], &[])),
        rule("dt", ctx(&["dl", "div"], &[])),
        rule("dd", ctx(&["dl", "div"], &[])),
        rule("tr", ctx(&["table", "thead", "tbody", "tfoot"], &[])),
        rule("td", ctx(&["tr"], &[])),
        rule("th", ctx(&["tr"], &[])),
        rule("thead", ctx(&["table"], &[])),
        rule("tbody", ctx(&["table"], &[])),
        rule("tfoot", ctx(&["table"], &[])),
        rule("caption", ctx(&["table"], &[])),
        rule("colgroup", ctx(&["table"], &[])),
        rule("col", ctx(&["colgroup", "table"], &[])),
        rule("option", ctx(&["select", "datalist", "optgroup"], &[])),
        rule("optgroup", ctx(&["select"], &[])),
        rule("legend", ctx(&["fieldset"], &[])),
        rule("figcaption", ctx(&["figure"], &[])),
        rule("summary", ctx(&["details"], &[])),
        rule("source", ctx(&["audio", "video", "picture"], &[])),
        rule("track", ctx(&["audio", "video"], &[])),
        rule("area", ctx(&[], &["map"])),
        rule("head", ctx(&["html"], &[])),
        rule("body", ctx(&["html"], &[])),
        rule("title", ctx(&["head"], &[])),
        rule("base", ctx(&["head"], &[])),
        rule("rt", ctx(&["ruby"], &[])),
        rule("rp", ctx(&["ruby"], &[])),
    ];
    let mut add = |tag: &str, f: &dyn Fn(&mut TagRule)| {
        if let Some(t) = tags.iter_mut().find(|t| t.tag == tag) {
            f(t);
        } else {
            let mut t = rule(tag, None);
            f(&mut t);
            tags.push(t);
        }
    };
    add("img", &|t| t.requirements.push((Requirement::All, s(&["src"]))));
    add("meta", &|t| {
        t.requirements.push((Requirement::ExactlyOne, s(&["name", "http-equiv", "charset", "itemprop"])));
        t.invalid.push(s(&["charset", "content"]));
    });
    add("link", &|t| {
        t.requirements.push((Requirement::All, s(&["href"])));
        t.requirements.push((Requirement::AtLeastOne, s(&["rel", "itemprop"])));
    });
    add("base", &|t| t.requirements.push((Requirement::AtLeastOne, s(&["href", "target"]))));
    add("optgroup", &|t| t.requirements.push((Requirement::All, s(&["label"]))));
    add("track", &|t| t.requirements.push((Requirement::All, s(&["src"]))));
    add("object", &|t| t.requirements.push((Requirement::AtLeastOne, s(&["data", "type"]))));
    add("map", &|t| t.requirements.push((Requirement::All, s(&["name"]))));
    for (tag, attr, key) in [
        ("td", "colspan", "non_negative_integer"),
        ("td", "rowspan", "non_negative_integer"),
        ("th", "colspan", "non_negative_integer"),
        ("th", "rowspan", "non_negative_integer"),
        ("col", "span", "non_negative_integer"),
        ("colgroup", "span", "non_negative_integer"),
        ("textarea", "rows", "non_negative_integer"),
        ("textarea", "cols", "non_negative_integer"),
        ("canvas", "width", "non_negative_integer"),
        ("canvas", "height", "non_negative_integer"),
        ("ol", "start", "integer"),
        ("li", "value", "integer"),
        ("link", "color", "color"),
    ] {
        add(tag, &|t| t.attributes.push((attr.to_string(), key.to_string())));
    }
    RuleSet { rule_set_id: "html_all".into(), dtd_context: DtdContext::Html, version: "html5".into(), tags, value_map }
}

fn path(doc: &WebDoc, id: NodeId) -> Option<String> {
    doc.key_of(id).map(|k| k.as_str().to_string())
}

/// The HTML elements of `scope` (the whole document when `None`), in document order.
fn elements(doc: &WebDoc, scope: Option<NodeId>) -> Vec<NodeId> {
    let range = scope.and_then(|s| doc.node(s)).map(|n| n.span.clone());
    (0..doc.len()).filter(|&id| doc.node(id).is_some_and(|n| n.kind == NodeKind::Element && n.namespace == Namespace::Html && range.as_ref().is_none_or(|r| r.start <= n.span.start && n.span.end <= r.end))).collect()
}

/// An attribute name is outside every vocabulary on purpose: `data-*`, `aria-*`, `role`, and event handlers (`on`
/// followed by letters) are open families.
fn open_family(name: &str) -> bool {
    name.starts_with("data-") || name.starts_with("aria-") || name == "role" || (name.len() > 2 && name.starts_with("on") && name.bytes().skip(2).all(|c| c.is_ascii_lowercase()))
}

/// [STU-WEB-052] to [STU-WEB-055]: validates the HTML elements of `doc` (or of element `scope`'s subtree) against the
/// vocabulary for `type_id` and `rules`. Diagnostics only; the document is never changed.
pub fn validate(doc: &WebDoc, vocabulary: &TagRegistry, type_id: &str, rules: &RuleSet, scope: Option<NodeId>) -> Report {
    let mut report = Report::default();
    let set = rules.rule_set_id.clone();
    let diag = |report: &mut Report, severity: Severity, rule_id: String, range: Range<usize>, id: NodeId, code: &'static str, message: String| {
        report.push(Diagnostic { severity, rule_set_id: set.clone(), rule_id, byte_range: range, element_path: path(doc, id), message, code });
    };
    for id in elements(doc, scope) {
        let Some(node) = doc.node(id) else { continue };
        let tag = doc.local_name(id).unwrap_or_default();
        let open = node.open_span.clone().unwrap_or_else(|| node.span.clone());
        let def = vocabulary.tag(type_id, &tag);
        if def.is_none() && !tag.contains('-') {
            diag(&mut report, Severity::Warning, format!("{tag}.known"), open.clone(), id, "tag_unknown", format!("<{tag}> is not in the tag libraries of document type {type_id}"));
        }
        let present: Vec<String> = node.attrs.iter().map(|a| doc.attr_local_name(a)).collect();
        let tag_rule = rules.tags.iter().find(|t| t.tag == tag);
        for a in &node.attrs {
            let name = doc.attr_local_name(a);
            let value = doc.attr_value(a);
            if let Some(def) = def {
                match vocabulary.attribute(def, &name) {
                    None if !open_family(&name) => diag(&mut report, Severity::Warning, format!("{tag}.{name}.known"), a.span.clone(), id, "attribute_unknown", format!("{name} is not an attribute of <{tag}>")),
                    Some(attr) if attr.value_type == ValueType::Enumerated => {
                        if let Some(v) = value.as_deref().filter(|v| !attr.allows(v)) {
                            let allowed: Vec<&str> = attr.allowed_values.iter().map(|x| x.value.as_str()).collect();
                            diag(&mut report, Severity::Warning, format!("{tag}.{name}.values"), a.value_span.clone().unwrap_or_else(|| a.span.clone()), id, "attribute_value_not_allowed", format!("{name}=\"{v}\" is not one of {}", allowed.join(", ")));
                        }
                    }
                    _ => {}
                }
            }
            if let Some((_, key)) = tag_rule.and_then(|t| t.attributes.iter().find(|(n, _)| *n == name))
                && let Some(entry) = rules.value_map.iter().find(|e| &e.key_id == key)
            {
                {
                    if let Some(v) = value.as_deref().filter(|v| !entry.accepts(v)) {
                        diag(&mut report, Severity::Warning, format!("{tag}.{name}.shape"), a.value_span.clone().unwrap_or_else(|| a.span.clone()), id, "attribute_value_shape", format!("{name}=\"{v}\" does not have the shape {} ({})", entry.key_id, entry.note));
                    }
                    if !entry.quote_rule.allows(a.quote) {
                        diag(&mut report, Severity::Warning, format!("{tag}.{name}.quote"), a.span.clone(), id, "attribute_quote_rule", format!("{name} breaks the quote rule {}", entry.quote_rule.token()));
                    }
                }
            }
        }
        let Some(t) = tag_rule else { continue };
        if let Some(c) = &t.context {
            let parent = node.parent.and_then(|p| doc.local_name(p));
            let mut ancestors = Vec::new();
            let mut at = node.parent;
            while let Some(p) = at {
                if let Some(n) = doc.local_name(p) {
                    ancestors.push(n);
                }
                at = doc.node(p).and_then(|n| n.parent);
            }
            let ok = parent.as_ref().is_some_and(|p| c.parents.contains(p)) || ancestors.iter().any(|a| c.ancestors.contains(a));
            if !ok {
                let legal: Vec<&str> = c.parents.iter().chain(&c.ancestors).map(String::as_str).collect();
                diag(&mut report, Severity::Error, format!("{tag}.context"), open.clone(), id, "tag_context_violation", format!("<{tag}> is not legal inside <{}>; legal in {}", parent.unwrap_or_default(), legal.join(", ")));
            }
        }
        for (kind, names) in &t.requirements {
            let n = names.iter().filter(|x| present.contains(x)).count();
            let ok = match kind {
                Requirement::AtLeastOne => n >= 1,
                Requirement::ExactlyOne => n == 1,
                Requirement::All => n == names.len(),
            };
            if !ok {
                diag(&mut report, Severity::Error, format!("{tag}.requires.{}", kind.token()), open.clone(), id, "attribute_required_set_unsatisfied", format!("<{tag}> needs {} of {}", kind.token(), names.join(", ")));
            }
        }
        for combo in &t.invalid {
            if combo.iter().all(|x| present.contains(x)) {
                diag(&mut report, Severity::Error, format!("{tag}.invalid"), open.clone(), id, "attribute_combination_invalid", format!("<{tag}> must not carry {} together", combo.join(" and ")));
            }
        }
    }
    report
}

// ---------------------------------------------------------------------------------------------------------------------
// Linters ([STU-WEB-056] to [STU-WEB-058]).

/// [STU-WEB-056] CSS: 35 rules, `(rule, enabled by default)`.
pub const CSS_LINT_RULES: [(&str, bool); 35] = [
    ("duplicate-properties", true),
    ("floats", true),
    ("vendor-prefix", true),
    ("important", false),
    ("adjoining-classes", false),
    ("known-properties", false),
    ("box-sizing", false),
    ("box-model", false),
    ("overqualified-elements", false),
    ("display-property-grouping", false),
    ("bulletproof-font-face", false),
    ("compatible-vendor-prefixes", false),
    ("regex-selectors", false),
    ("errors", false),
    ("duplicate-background-images", false),
    ("empty-rules", false),
    ("selector-max-approaching", false),
    ("gradients", false),
    ("fallback-colors", false),
    ("font-sizes", false),
    ("font-faces", false),
    ("star-property-hack", false),
    ("outline-none", false),
    ("import", false),
    ("ids", false),
    ("underscore-property-hack", false),
    ("rules-count", false),
    ("qualified-headings", false),
    ("selector-max", false),
    ("shorthand", false),
    ("text-indent", false),
    ("unique-headings", false),
    ("universal-selector", false),
    ("unqualified-attributes", false),
    ("zero-units", false),
];

/// [STU-WEB-056] markup: 23 rules.
pub const MARKUP_LINT_RULES: [(&str, bool); 23] = [
    ("tagname-lowercase", true),
    ("attr-value-double-quotes", true),
    ("attr-no-duplication", true),
    ("doctype-first", true),
    ("tag-pair", true),
    ("spec-char-escape", true),
    ("id-unique", true),
    ("src-not-empty", true),
    ("alt-require", true),
    ("attr-lowercase", false),
    ("attr-value-not-empty", false),
    ("tag-self-close", false),
    ("title-require", false),
    ("head-script-disabled", false),
    ("doctype-html5", false),
    ("id-class-value", false),
    ("style-disabled", false),
    ("inline-style-disabled", false),
    ("inline-script-disabled", false),
    ("space-tab-mixed-disabled", false),
    ("id-class-ad-disabled", false),
    ("href-abs-or-rel", false),
    ("attr-unsafe-chars", false),
];

/// A script option's default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptDefault {
    Bool(bool),
    Number(u32),
    /// A limit option left unset: no limit (not zero).
    Unset,
    /// `globals`: the declared map, initially empty.
    EmptyMap,
}

/// [STU-WEB-056] script: the closed 69-key default record, in the spec's order.
pub const SCRIPT_LINT_DEFAULTS: [(&str, ScriptDefault); 69] = {
    use ScriptDefault::{Bool, EmptyMap, Number, Unset};
    [
        ("maxerr", Number(50)),
        ("bitwise", Bool(true)),
        ("camelcase", Bool(false)),
        ("curly", Bool(true)),
        ("eqeqeq", Bool(true)),
        ("forin", Bool(true)),
        ("freeze", Bool(true)),
        ("immed", Bool(false)),
        ("indent", Number(4)),
        ("latedef", Bool(false)),
        ("newcap", Bool(false)),
        ("noarg", Bool(true)),
        ("noempty", Bool(false)),
        ("nonbsp", Bool(true)),
        ("nonew", Bool(false)),
        ("plusplus", Bool(false)),
        ("quotmark", Bool(false)),
        ("undef", Bool(true)),
        ("unused", Bool(true)),
        ("strict", Bool(true)),
        ("maxparams", Unset),
        ("maxdepth", Unset),
        ("maxstatements", Unset),
        ("maxcomplexity", Unset),
        ("maxlen", Unset),
        ("asi", Bool(false)),
        ("boss", Bool(false)),
        ("debug", Bool(false)),
        ("eqnull", Bool(false)),
        ("es5", Bool(false)),
        ("esnext", Bool(false)),
        ("moz", Bool(false)),
        ("evil", Bool(false)),
        ("expr", Bool(false)),
        ("funcscope", Bool(false)),
        ("globalstrict", Bool(false)),
        ("iterator", Bool(false)),
        ("lastsemic", Bool(false)),
        ("laxbreak", Bool(false)),
        ("laxcomma", Bool(false)),
        ("loopfunc", Bool(false)),
        ("multistr", Bool(false)),
        ("noyield", Bool(false)),
        ("notypeof", Bool(false)),
        ("proto", Bool(false)),
        ("scripturl", Bool(false)),
        ("shadow", Bool(false)),
        ("sub", Bool(false)),
        ("supernew", Bool(false)),
        ("validthis", Bool(false)),
        ("browser", Bool(true)),
        ("browserify", Bool(false)),
        ("couch", Bool(false)),
        ("devel", Bool(true)),
        ("dojo", Bool(false)),
        ("jasmine", Bool(false)),
        ("jquery", Bool(true)),
        ("mocha", Bool(true)),
        ("mootools", Bool(false)),
        ("node", Bool(false)),
        ("nonstandard", Bool(false)),
        ("prototypejs", Bool(false)),
        ("qunit", Bool(false)),
        ("rhino", Bool(false)),
        ("shelljs", Bool(false)),
        ("worker", Bool(true)),
        ("wsh", Bool(false)),
        ("yui", Bool(false)),
        ("globals", EmptyMap),
    ]
};

/// [STU-WEB-057]: one script-language level configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptLevel {
    /// `3`, `5`, `6` or `current` (an immutable profile pin of the module release, never the wall clock).
    pub ecma_version: &'static str,
    pub source_type: &'static str,
    pub jsx: bool,
    pub environments: [&'static str; 2],
    /// The one rule set to error; every other rule is off (opt in per site).
    pub error_rules: [&'static str; 1],
}

pub const SCRIPT_LEVELS: [ScriptLevel; 4] = [
    ScriptLevel { ecma_version: "3", source_type: "module", jsx: true, environments: ["browser", "jquery"], error_rules: ["constructor-super"] },
    ScriptLevel { ecma_version: "5", source_type: "module", jsx: true, environments: ["browser", "jquery"], error_rules: ["constructor-super"] },
    ScriptLevel { ecma_version: "6", source_type: "module", jsx: true, environments: ["browser", "jquery"], error_rules: ["constructor-super"] },
    ScriptLevel { ecma_version: "current", source_type: "module", jsx: true, environments: ["browser", "jquery"], error_rules: ["constructor-super"] },
];

/// The lint languages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Markup,
    Css,
    Script,
}

impl Language {
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "markup" => Some(Self::Markup),
            "css" => Some(Self::Css),
            "script" => Some(Self::Script),
            _ => None,
        }
    }

    pub const fn token(self) -> &'static str {
        match self {
            Self::Markup => "markup",
            Self::Css => "css",
            Self::Script => "script",
        }
    }
}

/// The markup rules this module runs.
const MARKUP_RUN: [&str; 17] = [
    "tagname-lowercase",
    "attr-value-double-quotes",
    "attr-no-duplication",
    "doctype-first",
    "tag-pair",
    "spec-char-escape",
    "id-unique",
    "src-not-empty",
    "alt-require",
    "attr-lowercase",
    "attr-value-not-empty",
    "tag-self-close",
    "title-require",
    "doctype-html5",
    "style-disabled",
    "inline-style-disabled",
    "inline-script-disabled",
];
/// The CSS rules this module runs.
const CSS_RUN: [&str; 11] = ["duplicate-properties", "floats", "vendor-prefix", "important", "empty-rules", "ids", "zero-units", "universal-selector", "import", "outline-none", "errors"];
/// `floats`: Studio-declared threshold (the vendor's is UNVERIFIED).
pub const FLOATS_THRESHOLD: usize = 10;

/// The rules enabled for a run: the language's defaults with `overrides` applied (an unknown rule is refused).
pub fn enabled_rules(language: Language, overrides: &BTreeMap<String, bool>) -> Result<Vec<&'static str>, CodeError> {
    let defaults: &[(&'static str, bool)] = match language {
        Language::Markup => &MARKUP_LINT_RULES,
        Language::Css => &CSS_LINT_RULES,
        Language::Script => return Err(CodeError::new("script_lint_provider_unselected")),
    };
    if overrides.keys().any(|k| !defaults.iter().any(|(r, _)| r == k)) {
        return Err(CodeError::new("lint_rule_unknown"));
    }
    Ok(defaults.iter().filter(|(r, on)| overrides.get(*r).copied().unwrap_or(*on)).map(|(r, _)| *r).collect())
}

/// [STU-WEB-056], [STU-WEB-058]: lints the document (or element `scope`'s subtree) in `language`.
pub fn lint(doc: &WebDoc, language: Language, overrides: &BTreeMap<String, bool>, scope: Option<NodeId>) -> Result<Report, CodeError> {
    let enabled = enabled_rules(language, overrides)?;
    let mut report = Report::default();
    let run: &[&str] = if language == Language::Markup { &MARKUP_RUN } else { &CSS_RUN };
    report.not_run = enabled.iter().filter(|r| !run.contains(r)).map(|r| (*r).to_string()).collect();
    let on = |r: &str| enabled.contains(&r);
    match language {
        Language::Markup => lint_markup(doc, &on, scope, &mut report),
        Language::Css => lint_css(doc, &on, scope, &mut report),
        Language::Script => {}
    }
    Ok(report)
}

fn lint_diag(report: &mut Report, language: Language, rule: &str, severity: Severity, range: Range<usize>, path: Option<String>, message: String) {
    report.push(Diagnostic { severity, rule_set_id: language.token().into(), rule_id: rule.into(), byte_range: range, element_path: path, message, code: "lint" });
}

fn lint_markup(doc: &WebDoc, on: &dyn Fn(&str) -> bool, scope: Option<NodeId>, report: &mut Report) {
    let m = Language::Markup;
    let els = elements(doc, scope);
    if scope.is_none() {
        let first = doc.node(doc.root()).and_then(|r| r.children.iter().copied().find(|&c| doc.node(c).is_some_and(|n| !(n.kind == NodeKind::Text && doc.slice(&n.span).trim().is_empty()))));
        let first_kind = first.and_then(|c| doc.node(c)).map(|n| n.kind);
        if on("doctype-first") && first_kind != Some(NodeKind::Doctype) {
            lint_diag(report, m, "doctype-first", Severity::Error, 0..0, None, "the document does not start with a doctype".into());
        }
        if on("doctype-html5") {
            let html5 = first.filter(|_| first_kind == Some(NodeKind::Doctype)).and_then(|c| doc.node(c)).is_some_and(|n| doc.slice(&n.span).eq_ignore_ascii_case("<!doctype html>"));
            if !html5 {
                lint_diag(report, m, "doctype-html5", Severity::Warning, 0..0, None, "the doctype is not <!DOCTYPE html>".into());
            }
        }
        if on("title-require") {
            let titled = doc.find_by_tag("title").into_iter().any(|t| !doc.text_content(t).trim().is_empty());
            if !titled {
                lint_diag(report, m, "title-require", Severity::Error, 0..0, None, "no non-empty <title>".into());
            }
        }
    }
    let mut ids: HashMap<String, Range<usize>> = HashMap::new();
    for id in els {
        let Some(node) = doc.node(id) else { continue };
        let tag = doc.local_name(id).unwrap_or_default();
        let p = path(doc, id);
        if on("tagname-lowercase") && let Some(ns) = &node.name_span && doc.slice(ns).bytes().any(|c| c.is_ascii_uppercase()) {
            lint_diag(report, m, "tagname-lowercase", Severity::Error, ns.clone(), p.clone(), format!("tag name {} is not lower case", doc.slice(ns)));
        }
        if on("tag-pair") && node.content == ContentModel::Normal && !node.self_closing && node.close_span.is_none() {
            lint_diag(report, m, "tag-pair", Severity::Error, node.span.clone(), p.clone(), format!("<{tag}> has no end tag"));
        }
        if on("tag-self-close") && node.content == ContentModel::Empty && !node.self_closing {
            lint_diag(report, m, "tag-self-close", Severity::Warning, node.open_span.clone().unwrap_or_else(|| node.span.clone()), p.clone(), format!("<{tag}> is not written self-closing"));
        }
        if on("style-disabled") && tag == "style" {
            lint_diag(report, m, "style-disabled", Severity::Warning, node.span.clone(), p.clone(), "<style> is used".into());
        }
        let mut seen: Vec<String> = Vec::new();
        for a in &node.attrs {
            let name = doc.attr_local_name(a);
            let written = doc.slice(&a.name_span);
            let value = doc.attr_value(a);
            if on("attr-lowercase") && written.bytes().any(|c| c.is_ascii_uppercase()) {
                lint_diag(report, m, "attr-lowercase", Severity::Error, a.name_span.clone(), p.clone(), format!("attribute name {written} is not lower case"));
            }
            if on("attr-value-double-quotes") && matches!(a.quote, Quote::Single | Quote::Unquoted) {
                lint_diag(report, m, "attr-value-double-quotes", Severity::Error, a.span.clone(), p.clone(), format!("{name} is not double-quoted"));
            }
            if on("attr-no-duplication") && seen.contains(&name) {
                lint_diag(report, m, "attr-no-duplication", Severity::Error, a.span.clone(), p.clone(), format!("{name} is given twice"));
            }
            seen.push(name.clone());
            if on("attr-value-not-empty") && value.as_deref().is_none_or(str::is_empty) {
                lint_diag(report, m, "attr-value-not-empty", Severity::Warning, a.span.clone(), p.clone(), format!("{name} has no value"));
            }
            if on("id-unique") && name == "id" && let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
                if ids.contains_key(v) {
                    lint_diag(report, m, "id-unique", Severity::Error, a.span.clone(), p.clone(), format!("id {v} is used more than once"));
                } else {
                    ids.insert(v.to_string(), a.span.clone());
                }
            }
            let src_like = name == "src" || (name == "href" && tag == "link") || (name == "data" && tag == "object");
            if on("src-not-empty") && src_like && value.as_deref().is_none_or(|v| v.trim().is_empty()) {
                lint_diag(report, m, "src-not-empty", Severity::Error, a.span.clone(), p.clone(), format!("{name} of <{tag}> is empty"));
            }
            if on("inline-style-disabled") && name == "style" {
                lint_diag(report, m, "inline-style-disabled", Severity::Warning, a.span.clone(), p.clone(), "inline style attribute".into());
            }
            let script_url = matches!(name.as_str(), "href" | "src") && value.as_deref().is_some_and(|v| v.trim_start().get(..11).is_some_and(|s| s.eq_ignore_ascii_case("javascript:")));
            if on("inline-script-disabled") && ((name.starts_with("on") && name.len() > 2) || script_url) {
                lint_diag(report, m, "inline-script-disabled", Severity::Warning, a.span.clone(), p.clone(), format!("inline script in {name}"));
            }
        }
        if on("alt-require") {
            let has = |n: &str| node.attrs.iter().any(|a| doc.attr_local_name(a) == n);
            let image_input = tag == "input" && node.attrs.iter().any(|a| doc.attr_local_name(a) == "type" && doc.attr_value(a).is_some_and(|v| v.eq_ignore_ascii_case("image")));
            if (tag == "img" || (tag == "area" && has("href")) || image_input) && !has("alt") {
                lint_diag(report, m, "alt-require", Severity::Warning, node.open_span.clone().unwrap_or_else(|| node.span.clone()), p.clone(), format!("<{tag}> has no alt"));
            }
        }
    }
    if on("spec-char-escape") {
        let scope_range = scope.and_then(|s| doc.node(s)).map(|n| n.span.clone());
        for id in 0..doc.len() {
            let Some(n) = doc.node(id) else { continue };
            if n.kind != NodeKind::Text || scope_range.as_ref().is_some_and(|r| n.span.start < r.start || r.end < n.span.end) {
                continue;
            }
            let text = doc.slice(&n.span);
            if let Some(at) = text.find(['<', '>']) {
                let start = n.span.start.saturating_add(at);
                lint_diag(report, m, "spec-char-escape", Severity::Error, start..start.saturating_add(1), path(doc, id), "a raw < or > in text".into());
            }
        }
    }
}

fn lint_css(doc: &WebDoc, on: &dyn Fn(&str) -> bool, scope: Option<NodeId>, report: &mut Report) {
    let src = doc.source();
    let c = Language::Css;
    let mut floats: Vec<Range<usize>> = Vec::new();
    for style in css_properties::style_elements(doc) {
        if scope.is_some_and(|s| doc.node(s).zip(doc.node(style)).is_none_or(|(a, b)| b.span.start < a.span.start || a.span.end < b.span.end)) {
            continue;
        }
        let p = path(doc, style);
        let Ok(content) = css_properties::style_content(doc, style) else { continue };
        let items = match css_properties::scan_css(src, content) {
            Ok(items) => items,
            Err(e) => {
                lint_diag(report, c, "errors", Severity::Error, e.at.clone().unwrap_or(0..0), p.clone(), format!("the style sheet does not scan: {}", e.code));
                continue;
            }
        };
        let mut rules = Vec::new();
        for item in &items {
            match item.kind {
                ItemKind::Rule => rules.push(item.clone()),
                ItemKind::Media => rules.extend(item.children.iter().filter(|x| x.kind == ItemKind::Rule).cloned()),
                ItemKind::Statement if on("import") && src.get(item.prelude.clone()).is_some_and(|t| t.get(..7).is_some_and(|k| k.eq_ignore_ascii_case("@import"))) => {
                    lint_diag(report, c, "import", Severity::Warning, item.whole.clone(), p.clone(), "@import is used".into());
                }
                _ => {}
            }
        }
        for rule in rules {
            let selector = src.get(rule.prelude.clone()).unwrap_or("");
            let Ok(decls) = css_properties::declarations(src, rule.block.clone()) else { continue };
            if on("empty-rules") && decls.is_empty() {
                lint_diag(report, c, "empty-rules", Severity::Warning, rule.whole.clone(), p.clone(), format!("{selector} has no declarations"));
            }
            if on("ids") && selector.contains('#') {
                lint_diag(report, c, "ids", Severity::Warning, rule.prelude.clone(), p.clone(), format!("{selector} uses an id selector"));
            }
            if on("universal-selector") && selector.split(',').any(|part| part.trim().rsplit(|ch: char| ch.is_whitespace() || ch == '>' || ch == '+' || ch == '~').next() == Some("*")) {
                lint_diag(report, c, "universal-selector", Severity::Warning, rule.prelude.clone(), p.clone(), format!("{selector} ends in the universal selector"));
            }
            for (i, d) in decls.iter().enumerate() {
                let value = src.get(d.value_range.clone()).unwrap_or("").trim();
                if on("duplicate-properties") {
                    // A repeat is a fallback when it directly follows the same property with another value.
                    let previous = decls[..i].iter().rposition(|x| x.property == d.property);
                    if let Some(pi) = previous {
                        let same_value = src.get(decls[pi].value_range.clone()).unwrap_or("").trim() == value;
                        if same_value || pi.saturating_add(1) != i {
                            lint_diag(report, c, "duplicate-properties", Severity::Warning, d.whole.clone(), p.clone(), format!("{} is declared twice in {selector}", d.property));
                        }
                    }
                }
                if d.property == "float" && !value.eq_ignore_ascii_case("none") {
                    floats.push(d.whole.clone());
                }
                if on("vendor-prefix") && d.property.starts_with('-') {
                    let standard = d.property.splitn(3, '-').nth(2).unwrap_or("");
                    if !standard.is_empty() && !decls.iter().skip(i).any(|x| x.property == standard) {
                        lint_diag(report, c, "vendor-prefix", Severity::Warning, d.whole.clone(), p.clone(), format!("{} has no standard {standard} after it", d.property));
                    }
                }
                if on("important") && d.important {
                    lint_diag(report, c, "important", Severity::Warning, d.whole.clone(), p.clone(), format!("!important on {}", d.property));
                }
                if on("zero-units") && value.split_ascii_whitespace().any(|t| t.len() > 1 && t.starts_with('0') && t.trim_start_matches('0').chars().next().is_some_and(char::is_alphabetic) && !t.ends_with("s") && !t.ends_with("deg")) {
                    lint_diag(report, c, "zero-units", Severity::Warning, d.value_range.clone(), p.clone(), format!("a zero with a unit in {}", d.property));
                }
                if on("outline-none") && d.property == "outline" && (value.eq_ignore_ascii_case("none") || value == "0") {
                    lint_diag(report, c, "outline-none", Severity::Warning, d.whole.clone(), p.clone(), "outline removed".into());
                }
            }
        }
    }
    if on("floats") && floats.len() >= FLOATS_THRESHOLD {
        let range = floats.first().cloned().unwrap_or(0..0);
        lint_diag(report, c, "floats", Severity::Warning, range, None, format!("{} float declarations (threshold {FLOATS_THRESHOLD}, Studio-declared)", floats.len()));
    }
}
