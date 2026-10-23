#![deny(clippy::arithmetic_side_effects)]
//! Bounded, non-executing decoder of the Lua table literal that Lightroom stores as smart-collection
//! rules (`AgLibraryCollectionContent.content` for owning module `ag.library.smart_collection`)
//! into a typed rule tree. The literal is tokenized per the Lua 5.1 lexical rules for the subset a
//! table literal can use (quoted strings with escapes decoded first, long brackets, line and long
//! comments, numbers, `true`/`false`/`nil`); nothing is evaluated and no identifier other than a
//! field name or the leading `name =` is accepted. Every size bound is checked before the
//! allocation it guards. The rule vocabulary (criteria, operation and combine names) is
//! UNVERIFIED against a Lightroom-written catalog (D5 oracle); unrecognised names are kept raw and
//! listed as notes, never dropped.

/// Hard ceilings for `RuleLimits` (a configuration above them is refused).
pub const HARD_MAX_RULE_INPUT_BYTES: u32 = 16 * 1024 * 1024;
pub const HARD_MAX_RULE_DEPTH: u32 = 64;
pub const HARD_MAX_RULE_NODES: u32 = 65_536;
pub const HARD_MAX_RULE_STRING_BYTES: u32 = 1024 * 1024;
const MAX_NUMBER_TOKEN: usize = 64;

/// Per-rule-text bounds. `max_nodes` counts every table, scalar and field the parser keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleLimits {
    pub max_input_bytes: u32,
    pub max_depth: u32,
    pub max_nodes: u32,
    pub max_string_bytes: u32,
}

impl Default for RuleLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024,
            max_depth: 16,
            max_nodes: 4096,
            max_string_bytes: 4096,
        }
    }
}

impl RuleLimits {
    pub fn validate(&self) -> Result<(), RuleError> {
        let zero = self.max_input_bytes == 0
            || self.max_depth == 0
            || self.max_nodes == 0
            || self.max_string_bytes == 0;
        let over = self.max_input_bytes > HARD_MAX_RULE_INPUT_BYTES
            || self.max_depth > HARD_MAX_RULE_DEPTH
            || self.max_nodes > HARD_MAX_RULE_NODES
            || self.max_string_bytes > HARD_MAX_RULE_STRING_BYTES;
        if zero || over {
            Err(RuleError::Configuration)
        } else {
            Ok(())
        }
    }
}

/// Typed refusal of one rule text. Codes are stable wire strings and carry no rule text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleError {
    Configuration,
    InputTooLarge,
    DepthExceeded,
    NodesExceeded,
    StringTooLong,
    NotUtf8,
    NotATable,
    Syntax(&'static str),
}

impl RuleError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Configuration => "rule_limit_configuration",
            Self::InputTooLarge => "rule_input_too_large",
            Self::DepthExceeded => "rule_depth_exceeded",
            Self::NodesExceeded => "rule_nodes_exceeded",
            Self::StringTooLong => "rule_string_too_long",
            Self::NotUtf8 => "rule_text_not_utf8",
            Self::NotATable => "rule_not_a_table",
            Self::Syntax(code) => code,
        }
    }
}

/// A scalar rule operand exactly as written (strings decoded, numbers parsed).
#[derive(Clone, Debug, PartialEq)]
pub enum RuleScalar {
    Nil,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
}

/// How the children of a group combine. Raw names: `intersect`, `union`, `exclude` (UNVERIFIED).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Combine {
    All,
    Any,
    NoneOf,
    Unrecognised(String),
}

/// Criteria names the mapping types (raw names UNVERIFIED); anything else stays `known: None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownField {
    Rating,
    Pick,
    LabelColor,
    LabelText,
    Keywords,
    Filename,
    FileFormat,
    CaptureTime,
    Collection,
    Folder,
    CameraModel,
    Lens,
    Iso,
    AnySearchableText,
    HasAdjustments,
}

const FIELDS: &[(&str, KnownField)] = &[
    ("rating", KnownField::Rating),
    ("pick", KnownField::Pick),
    ("labelColor", KnownField::LabelColor),
    ("labelText", KnownField::LabelText),
    ("keywords", KnownField::Keywords),
    ("filename", KnownField::Filename),
    ("fileFormat", KnownField::FileFormat),
    ("captureTime", KnownField::CaptureTime),
    ("collection", KnownField::Collection),
    ("folder", KnownField::Folder),
    ("cameraModel", KnownField::CameraModel),
    ("lens", KnownField::Lens),
    ("isoSpeedRating", KnownField::Iso),
    ("all", KnownField::AnySearchableText),
    ("hasAdjustments", KnownField::HasAdjustments),
];

/// Operation names the mapping types (raw names UNVERIFIED).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    InRange,
    ContainsAny,
    ContainsAll,
    ContainsWords,
    ContainsNone,
    BeginsWith,
    EndsWith,
}

const OPS: &[(&str, KnownOp)] = &[
    ("==", KnownOp::Eq),
    ("!=", KnownOp::Ne),
    (">", KnownOp::Gt),
    (">=", KnownOp::Ge),
    ("<", KnownOp::Lt),
    ("<=", KnownOp::Le),
    ("in", KnownOp::InRange),
    ("any", KnownOp::ContainsAny),
    ("all", KnownOp::ContainsAll),
    ("words", KnownOp::ContainsWords),
    ("noneOf", KnownOp::ContainsNone),
    ("beginsWith", KnownOp::BeginsWith),
    ("endsWith", KnownOp::EndsWith),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named<K> {
    pub known: Option<K>,
    pub raw: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuleCriterion {
    pub field: Named<KnownField>,
    pub operation: Named<KnownOp>,
    pub value: RuleScalar,
    pub value2: Option<RuleScalar>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuleNode {
    Criterion(RuleCriterion),
    Group(RuleGroup),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuleGroup {
    /// `None` when the group carries no `combine` field (note `smart_rule_combine_absent`).
    pub combine: Option<Combine>,
    pub children: Vec<RuleNode>,
}

/// A decoded rule tree plus aggregated notes for everything the typing did not map. The notes
/// are (code, count) pairs in code order; an empty list means every field was typed.
#[derive(Clone, Debug, PartialEq)]
pub struct SmartRules {
    pub root: RuleGroup,
    pub notes: Vec<(&'static str, u32)>,
    /// Accounted bytes of the tree (nodes and kept text), for the caller's retention budget.
    pub retained_bytes: u64,
}

// ---------------------------------------------------------------- literal parser (internal)

#[derive(Debug)]
enum Lv {
    Nil,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Table(Table),
}

#[derive(Debug, Default)]
struct Table {
    array: Vec<Lv>,
    fields: Vec<(String, Lv)>,
    /// Keys that are neither a name/string nor a position (e.g. `[1.5] = x`, `[true] = x`).
    other_keys: u32,
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    limits: RuleLimits,
    nodes: u32,
    text_bytes: u64,
}

const NODE_BYTES: u64 = 48;

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn at(&self, k: usize) -> Option<u8> {
        self.s.get(self.i.saturating_add(k)).copied()
    }
    fn bump(&mut self, n: usize) {
        self.i = self.i.saturating_add(n).min(self.s.len());
    }
    fn node(&mut self) -> Result<(), RuleError> {
        if self.nodes >= self.limits.max_nodes {
            return Err(RuleError::NodesExceeded);
        }
        self.nodes = self.nodes.saturating_add(1);
        Ok(())
    }

    /// Long bracket level at the cursor (`[`, n `=`, `[`), without consuming.
    fn long_open(&self) -> Option<usize> {
        if self.peek() != Some(b'[') {
            return None;
        }
        let mut level = 0usize;
        while self.at(level.saturating_add(1)) == Some(b'=') {
            level = level.saturating_add(1);
        }
        (self.at(level.saturating_add(1)) == Some(b'[')).then_some(level)
    }

    /// Consumes a long bracket body; returns its byte range (content only).
    fn long_body(&mut self, level: usize) -> Result<(usize, usize), RuleError> {
        self.bump(level.saturating_add(2));
        if self.peek() == Some(b'\n') {
            self.bump(1);
        }
        let start = self.i;
        while self.i < self.s.len() {
            if self.peek() == Some(b']') {
                let mut k = 1usize;
                while self.at(k) == Some(b'=') {
                    k = k.saturating_add(1);
                }
                if k.saturating_sub(1) == level && self.at(k) == Some(b']') {
                    let end = self.i;
                    self.bump(k.saturating_add(1));
                    return Ok((start, end));
                }
            }
            self.bump(1);
        }
        Err(RuleError::Syntax("rule_unterminated_long_bracket"))
    }

    /// Skips whitespace and comments (`--` line, `--[[ ]]` long).
    fn skip(&mut self) -> Result<(), RuleError> {
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_whitespace() => self.bump(1),
                Some(b'-') if self.at(1) == Some(b'-') => {
                    self.bump(2);
                    if let Some(level) = self.long_open() {
                        self.long_body(level)?;
                    } else {
                        while self.peek().is_some_and(|b| b != b'\n') {
                            self.bump(1);
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn name(&mut self) -> Option<String> {
        let start = self.i;
        if !self.peek().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') {
            return None;
        }
        while self.peek().is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_') {
            self.bump(1);
        }
        Some(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn keep_text(&mut self, bytes: Vec<u8>) -> Result<String, RuleError> {
        let text = String::from_utf8(bytes).map_err(|_| RuleError::NotUtf8)?;
        self.text_bytes = self.text_bytes.saturating_add(text.len() as u64);
        Ok(text)
    }

    /// Quoted string with escapes decoded; the decoded length is checked before each push.
    fn quoted(&mut self, quote: u8) -> Result<String, RuleError> {
        self.bump(1);
        let cap = self.limits.max_string_bytes as usize;
        let mut out: Vec<u8> = Vec::new();
        let push = |out: &mut Vec<u8>, b: u8| {
            if out.len() >= cap {
                return Err(RuleError::StringTooLong);
            }
            out.push(b);
            Ok(())
        };
        loop {
            let Some(b) = self.peek() else {
                return Err(RuleError::Syntax("rule_unterminated_string"));
            };
            self.bump(1);
            match b {
                _ if b == quote => break,
                b'\n' | b'\r' => return Err(RuleError::Syntax("rule_newline_in_string")),
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return Err(RuleError::Syntax("rule_unterminated_string"));
                    };
                    self.bump(1);
                    match e {
                        b'n' => push(&mut out, b'\n')?,
                        b't' => push(&mut out, b'\t')?,
                        b'r' => push(&mut out, b'\r')?,
                        b'a' => push(&mut out, 0x07)?,
                        b'b' => push(&mut out, 0x08)?,
                        b'f' => push(&mut out, 0x0c)?,
                        b'v' => push(&mut out, 0x0b)?,
                        b'\\' | b'"' | b'\'' | b'\n' => push(&mut out, e)?,
                        b'0'..=b'9' => {
                            let mut v = u32::from(e.wrapping_sub(b'0'));
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'9') => {
                                        v = v.saturating_mul(10).saturating_add(u32::from(d.wrapping_sub(b'0')));
                                        self.bump(1);
                                    }
                                    _ => break,
                                }
                            }
                            let byte = u8::try_from(v).map_err(|_| RuleError::Syntax("rule_bad_escape"))?;
                            push(&mut out, byte)?;
                        }
                        _ => return Err(RuleError::Syntax("rule_bad_escape")),
                    }
                }
                _ => push(&mut out, b)?,
            }
        }
        self.keep_text(out)
    }

    fn number(&mut self, negative: bool) -> Result<Lv, RuleError> {
        let start = self.i;
        let mut prev = 0u8;
        while let Some(b) = self.peek() {
            let sign_ok = (b == b'+' || b == b'-') && matches!(prev, b'e' | b'E');
            if !(b.is_ascii_alphanumeric() || b == b'.' || sign_ok) {
                break;
            }
            if self.i.saturating_sub(start) >= MAX_NUMBER_TOKEN {
                return Err(RuleError::Syntax("rule_number_too_long"));
            }
            prev = b;
            self.bump(1);
        }
        let tok = std::str::from_utf8(&self.s[start..self.i]).map_err(|_| RuleError::Syntax("rule_bad_number"))?;
        let int = if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
            Some(i64::from_str_radix(hex, 16).map_err(|_| RuleError::Syntax("rule_bad_number"))?)
        } else if tok.bytes().all(|b| b.is_ascii_digit()) {
            tok.parse::<i64>().ok()
        } else {
            None
        };
        match int {
            Some(v) => Ok(Lv::Int(if negative {
                v.checked_neg().ok_or(RuleError::Syntax("rule_bad_number"))?
            } else {
                v
            })),
            None => {
                let f: f64 = tok.parse().map_err(|_| RuleError::Syntax("rule_bad_number"))?;
                if !f.is_finite() {
                    return Err(RuleError::Syntax("rule_bad_number"));
                }
                Ok(Lv::Num(if negative { -f } else { f }))
            }
        }
    }

    fn value(&mut self, depth: u32) -> Result<Lv, RuleError> {
        self.skip()?;
        self.node()?;
        match self.peek() {
            Some(b'{') => {
                if depth >= self.limits.max_depth {
                    return Err(RuleError::DepthExceeded);
                }
                self.table(depth.saturating_add(1))
            }
            Some(q @ (b'"' | b'\'')) => Ok(Lv::Str(self.quoted(q)?)),
            Some(b'[') if self.long_open().is_some() => {
                let level = self.long_open().unwrap_or_default();
                let (a, b) = self.long_body(level)?;
                if b.saturating_sub(a) > self.limits.max_string_bytes as usize {
                    return Err(RuleError::StringTooLong);
                }
                let bytes = self.s[a..b].to_vec();
                Ok(Lv::Str(self.keep_text(bytes)?))
            }
            Some(b'-') => {
                self.bump(1);
                self.skip()?;
                if self.peek().is_some_and(|b| b.is_ascii_digit() || b == b'.') {
                    self.number(true)
                } else {
                    Err(RuleError::Syntax("rule_unexpected_token"))
                }
            }
            Some(b) if b.is_ascii_digit() || b == b'.' => self.number(false),
            Some(_) => match self.name().as_deref() {
                Some("true") => Ok(Lv::Bool(true)),
                Some("false") => Ok(Lv::Bool(false)),
                Some("nil") => Ok(Lv::Nil),
                _ => Err(RuleError::Syntax("rule_unexpected_token")),
            },
            None => Err(RuleError::Syntax("rule_unexpected_end")),
        }
    }

    fn table(&mut self, depth: u32) -> Result<Lv, RuleError> {
        self.bump(1);
        let mut t = Table::default();
        loop {
            self.skip()?;
            match self.peek() {
                Some(b'}') => {
                    self.bump(1);
                    return Ok(Lv::Table(t));
                }
                None => return Err(RuleError::Syntax("rule_unterminated_table")),
                Some(b'[') if self.long_open().is_none() => {
                    self.bump(1);
                    let key = self.value(depth)?;
                    self.skip()?;
                    if self.peek() != Some(b']') {
                        return Err(RuleError::Syntax("rule_bad_key"));
                    }
                    self.bump(1);
                    self.expect_assign()?;
                    let v = self.value(depth)?;
                    match key {
                        Lv::Str(k) => t.fields.push((k, v)),
                        Lv::Int(n) if usize::try_from(n).ok() == Some(t.array.len().saturating_add(1)) => {
                            t.array.push(v)
                        }
                        _ => t.other_keys = t.other_keys.saturating_add(1),
                    }
                }
                Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
                    let save = self.i;
                    let name = self.name().unwrap_or_default();
                    self.skip()?;
                    if self.peek() == Some(b'=') && self.at(1) != Some(b'=') {
                        self.bump(1);
                        self.text_bytes = self.text_bytes.saturating_add(name.len() as u64);
                        let v = self.value(depth)?;
                        t.fields.push((name, v));
                    } else {
                        self.i = save;
                        let v = self.value(depth)?;
                        t.array.push(v);
                    }
                }
                Some(_) => {
                    let v = self.value(depth)?;
                    t.array.push(v);
                }
            }
            self.skip()?;
            match self.peek() {
                Some(b',' | b';') => self.bump(1),
                Some(b'}') => {}
                _ => return Err(RuleError::Syntax("rule_missing_separator")),
            }
        }
    }

    fn expect_assign(&mut self) -> Result<(), RuleError> {
        self.skip()?;
        if self.peek() == Some(b'=') && self.at(1) != Some(b'=') {
            self.bump(1);
            Ok(())
        } else {
            Err(RuleError::Syntax("rule_missing_assign"))
        }
    }
}

// ---------------------------------------------------------------- typing

#[derive(Default)]
struct Notes(Vec<(&'static str, u32)>);

impl Notes {
    fn add(&mut self, code: &'static str) {
        match self.0.iter_mut().find(|(c, _)| *c == code) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => self.0.push((code, 1)),
        }
    }
}

fn scalar(v: Lv, notes: &mut Notes) -> RuleScalar {
    match v {
        Lv::Nil => RuleScalar::Nil,
        Lv::Bool(b) => RuleScalar::Bool(b),
        Lv::Int(i) => RuleScalar::Int(i),
        Lv::Num(f) => RuleScalar::Num(f),
        Lv::Str(s) => RuleScalar::Str(s),
        Lv::Table(_) => {
            notes.add("smart_rule_value_not_scalar");
            RuleScalar::Nil
        }
    }
}

fn named<K: Copy>(raw: String, table: &[(&str, K)], notes: &mut Notes, code: &'static str) -> Named<K> {
    let known = table.iter().find(|(n, _)| *n == raw).map(|(_, k)| *k);
    if known.is_none() {
        notes.add(code);
    }
    Named { known, raw }
}

fn string_of(v: Lv, notes: &mut Notes) -> String {
    match v {
        Lv::Str(s) => s,
        _ => {
            notes.add("smart_rule_name_not_string");
            String::new()
        }
    }
}

fn criterion(t: Table, notes: &mut Notes) -> RuleCriterion {
    let (mut field, mut op, mut value, mut value2) = (None, None, RuleScalar::Nil, None);
    for (k, v) in t.fields {
        match k.as_str() {
            "criteria" => field = Some(string_of(v, notes)),
            "operation" => op = Some(string_of(v, notes)),
            "value" => value = scalar(v, notes),
            "value2" => value2 = Some(scalar(v, notes)),
            _ => notes.add("smart_rule_field_unmapped"),
        }
    }
    for _ in 0..t.array.len() {
        notes.add("smart_rule_field_unmapped");
    }
    for _ in 0..t.other_keys {
        notes.add("smart_rule_key_not_string");
    }
    if op.is_none() {
        notes.add("smart_rule_operation_absent");
    }
    RuleCriterion {
        field: named(field.unwrap_or_default(), FIELDS, notes, "smart_rule_criteria_unrecognised"),
        operation: named(op.unwrap_or_default(), OPS, notes, "smart_rule_operation_unrecognised"),
        value,
        value2,
    }
}

fn group(t: Table, notes: &mut Notes) -> RuleGroup {
    let mut combine = None;
    for (k, v) in t.fields {
        if k == "combine" {
            let raw = string_of(v, notes);
            combine = Some(match raw.as_str() {
                "intersect" => Combine::All,
                "union" => Combine::Any,
                "exclude" => Combine::NoneOf,
                _ => {
                    notes.add("smart_rule_combine_unrecognised");
                    Combine::Unrecognised(raw)
                }
            });
        } else {
            notes.add("smart_rule_field_unmapped");
        }
    }
    if combine.is_none() {
        notes.add("smart_rule_combine_absent");
    }
    for _ in 0..t.other_keys {
        notes.add("smart_rule_key_not_string");
    }
    let mut children = Vec::with_capacity(t.array.len());
    for item in t.array {
        match item {
            Lv::Table(inner) if inner.fields.iter().any(|(k, _)| k == "criteria") => {
                children.push(RuleNode::Criterion(criterion(inner, notes)))
            }
            Lv::Table(inner) => children.push(RuleNode::Group(group(inner, notes))),
            _ => notes.add("smart_rule_item_not_table"),
        }
    }
    RuleGroup { combine, children }
}

/// Decodes one smart-collection rule text (`[name =] { ... }`). The input length is checked
/// before anything is read; depth, node count and string length are checked before the
/// allocation each guards. Never evaluates the text.
pub fn parse_smart_rules(bytes: &[u8], limits: &RuleLimits) -> Result<SmartRules, RuleError> {
    limits.validate()?;
    if bytes.len() > limits.max_input_bytes as usize {
        return Err(RuleError::InputTooLarge);
    }
    let mut p = Parser {
        s: bytes,
        i: 0,
        limits: *limits,
        nodes: 0,
        text_bytes: 0,
    };
    p.skip()?;
    let save = p.i;
    if p.name().is_some() {
        p.skip()?;
        if p.peek() == Some(b'=') && p.at(1) != Some(b'=') {
            p.bump(1);
        } else {
            p.i = save;
        }
    }
    let top = p.value(0)?;
    p.skip()?;
    if p.peek() == Some(b';') {
        p.bump(1);
        p.skip()?;
    }
    if p.i < p.s.len() {
        return Err(RuleError::Syntax("rule_trailing_text"));
    }
    let Lv::Table(t) = top else {
        return Err(RuleError::NotATable);
    };
    let mut notes = Notes::default();
    let root = group(t, &mut notes);
    let mut notes = notes.0;
    notes.sort_unstable();
    let retained_bytes = u64::from(p.nodes)
        .saturating_mul(NODE_BYTES)
        .saturating_add(p.text_bytes);
    Ok(SmartRules {
        root,
        notes,
        retained_bytes,
    })
}
