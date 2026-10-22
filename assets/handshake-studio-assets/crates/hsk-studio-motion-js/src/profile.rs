//! Admitted-syntax profile validator for the Studio expression profile (STU-MOT-255), run by the
//! provider BEFORE any engine sees the source.
//!
//! What it is: a deterministic, allocation-bounded, NON-recursive lexical validator. It tokenizes the
//! source per the ECMAScript lexical grammar (escapes decoded or refused, comments and strings skipped
//! as the engine skips them), tracks bracket nesting with a bounded stack, and refuses every construct
//! outside the admitted profile with a typed violation carrying the exact 1-based line/column of the
//! offending token. Nothing is evaluated and no engine type is involved.
//!
//! What it is NOT: a full syntactic parser and not a complete sandbox. It does not prove that the
//! admitted source is syntactically valid (the engine still reports `Parse`), and a name assembled at
//! run time (`this[a + b]`-style computed access) is not visible to a lexical check; those cases remain
//! the job of the engine's hardened realm (EXPR-PROVIDER-05 stays NOT_PROVEN).
//!
//! Parser decision (evidence in the lane file `notes.sandbox_2.research_basis`): `boa_parser 0.22.0`
//! (60 packages) and `oxc_parser 0.153.0` (75 packages) were resolved with `cargo tree`; both pull ICU
//! data crates and proc-macro stacks, and `swc_ecma_parser 46.0.0` pulls the native `stacker`. Each
//! package is a steward pin plus license-gate record, and a recursive-descent parser would run hostile
//! text in the host process. A bounded iterative lexical validator needs no dependency.
//!
//! Admitted (profile v1): numeric/string/boolean/null/array/object expressions, `var`/`let`/`const`,
//! conditionals, loops, `try/catch/throw`, functions, arrow functions and closures, member access,
//! calls (including the host functions), the ES2018 operators. Refused: see `ViolationKind`.
//!
//! Positions: line and column are 1-based; a line break is LF, CR, CRLF (one break), U+2028 or U+2029;
//! a column counts Unicode scalar values from the start of the line.
//!
//! U+0000 is refused ANYWHERE in the source (string literal, comment, escape position or outside
//! both): the QuickJS wrapper hands the source to the engine as a NUL-terminated C string and fails
//! with `Error::InvalidString` on an interior NUL (`rquickjs-core-0.14.0/src/context/ctx.rs:144`,
//! `CString::new(src)?`; the only source-derived `InvalidString` site on the evaluation path). The
//! check is a whole-source precondition like the byte bound, run before the scan, so no lexical
//! construct can hide a NUL. A NUL written as an ESCAPE (backslash-zero, `\x00`, `\u0000`) is not a
//! NUL character in the source and stays admitted.
#![deny(clippy::arithmetic_side_effects)]
use crate::{engine::BudgetKind, limits::ConfigError};
use hsk_studio_motion::SourceSpan;

/// Reserved words refused anywhere in the source (also as property names: fail closed).
pub const DENIED_KEYWORDS: &[&str] = &[
    "class", "extends", "super", "import", "export", "async", "await", "yield", "with", "debugger",
    "new", "this", "enum",
];

/// Identifiers (and exact string values) naming ambient authority, dynamic compilation, global-object
/// reachability, prototype escapes, timers or shared memory (STU-MOT-255 UNAVAILABLE list). Refused
/// anywhere an identifier may appear, including after `.`, and as the exact value of a string literal.
pub const DENIED_NAMES: &[&str] = &[
    "eval", "Function", "AsyncFunction", "GeneratorFunction", "AsyncGeneratorFunction",
    "constructor", "__proto__", "__defineGetter__", "__defineSetter__", "__lookupGetter__",
    "__lookupSetter__", "Proxy", "Reflect", "Promise", "globalThis", "window", "self", "global",
    "require", "process", "module", "exports", "setTimeout", "setInterval", "setImmediate",
    "queueMicrotask", "SharedArrayBuffer", "Atomics", "WebAssembly", "WeakRef",
    "FinalizationRegistry",
];

/// Words after which a `/` starts a regular expression (not a division) because an expression or a
/// statement follows.
const EXPRESSION_KEYWORDS: &[&str] = &[
    "return", "typeof", "instanceof", "in", "of", "delete", "void", "throw", "else", "do", "case",
    "default", "function", "var", "let", "const", "if", "while", "for", "switch", "catch", "try",
    "finally", "break", "continue",
];

/// A `(` after one of these opens a statement head: the `/` after its `)` starts a regex.
const CONTROL_KEYWORDS: &[&str] = &["if", "while", "for"];

/// Longest-first punctuator table (ASCII only).
const PUNCTUATORS: &[&str] = &[
    ">>>=", "===", "!==", "**=", "<<=", ">>=", ">>>", "...", "&&=", "||=", "??=", "=>", "==", "!=",
    "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
    "<<", ">>", "**", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%", "&",
    "|", "^", "!", "~", "?", ":", "=", ".",
];

pub const DEFAULT_MAX_TOKENS: usize = 20_000;
pub const DEFAULT_MAX_DEPTH: usize = 64;
pub const MAX_DEPTH_CEILING: usize = 1024;
/// Cooked string values longer than this cannot equal a denied name and are not stored.
const MAX_COOKED_BYTES: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViolationKind {
    // Parse class: the source is not lexically well formed.
    UnterminatedString,
    UnterminatedComment,
    InvalidEscape,
    InvalidNumber,
    LegacyOctal,
    InvalidCharacter,
    /// A raw U+0000 anywhere in the source (the engine takes NUL-terminated source).
    NulCharacter,
    UnbalancedBracket,
    UnexpectedEnd,
    // Unsupported class: well formed but outside the admitted profile.
    DeniedKeyword,
    DeniedName,
    DeniedString,
    TemplateLiteral,
    RegexLiteral,
    HtmlComment,
    IdentifierEscape,
    NonAsciiIdentifier,
    NumericSeparator,
    BigIntLiteral,
    OptionalChaining,
    PostEs2018Operator,
    PrivateName,
    GeneratorFunction,
    Accessor,
    // Budget class: hostile size.
    SourceTooLarge,
    TooManyTokens,
    TooDeep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViolationClass {
    Parse,
    Unsupported,
    Budget(BudgetKind),
}

impl ViolationKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnterminatedString => "unterminated_string",
            Self::UnterminatedComment => "unterminated_comment",
            Self::InvalidEscape => "invalid_escape",
            Self::InvalidNumber => "invalid_number",
            Self::LegacyOctal => "legacy_octal",
            Self::InvalidCharacter => "invalid_character",
            Self::NulCharacter => "nul_character",
            Self::UnbalancedBracket => "unbalanced_bracket",
            Self::UnexpectedEnd => "unexpected_end",
            Self::DeniedKeyword => "denied_keyword",
            Self::DeniedName => "denied_name",
            Self::DeniedString => "denied_string",
            Self::TemplateLiteral => "template_literal",
            Self::RegexLiteral => "regex_literal",
            Self::HtmlComment => "html_comment",
            Self::IdentifierEscape => "identifier_escape",
            Self::NonAsciiIdentifier => "non_ascii_identifier",
            Self::NumericSeparator => "numeric_separator",
            Self::BigIntLiteral => "bigint_literal",
            Self::OptionalChaining => "optional_chaining",
            Self::PostEs2018Operator => "post_es2018_operator",
            Self::PrivateName => "private_name",
            Self::GeneratorFunction => "generator_function",
            Self::Accessor => "accessor",
            Self::SourceTooLarge => "source_too_large",
            Self::TooManyTokens => "too_many_tokens",
            Self::TooDeep => "too_deep",
        }
    }

    pub const fn class(self) -> ViolationClass {
        match self {
            Self::UnterminatedString
            | Self::UnterminatedComment
            | Self::InvalidEscape
            | Self::InvalidNumber
            | Self::LegacyOctal
            | Self::InvalidCharacter
            | Self::NulCharacter
            | Self::UnbalancedBracket
            | Self::UnexpectedEnd => ViolationClass::Parse,
            Self::DeniedKeyword
            | Self::DeniedName
            | Self::DeniedString
            | Self::TemplateLiteral
            | Self::RegexLiteral
            | Self::HtmlComment
            | Self::IdentifierEscape
            | Self::NonAsciiIdentifier
            | Self::NumericSeparator
            | Self::BigIntLiteral
            | Self::OptionalChaining
            | Self::PostEs2018Operator
            | Self::PrivateName
            | Self::GeneratorFunction
            | Self::Accessor => ViolationClass::Unsupported,
            Self::SourceTooLarge | Self::TooManyTokens => {
                ViolationClass::Budget(BudgetKind::SourceBytes)
            }
            Self::TooDeep => ViolationClass::Budget(BudgetKind::Recursion),
        }
    }
}

/// The first violation found: what, where (token start) and how long the offending token is
/// (Unicode scalar values; 0 for an end-of-input position).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileViolation {
    pub kind: ViolationKind,
    pub span: SourceSpan,
    pub len: u32,
}

/// Facts about an accepted source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidatedSource {
    pub bytes: usize,
    pub tokens: usize,
    pub max_depth: usize,
}

/// Validator bounds; checked before and during the scan (hostile sizes, V4-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileLimits {
    pub max_bytes: usize,
    pub max_tokens: usize,
    pub max_depth: usize,
}

impl Default for ProfileLimits {
    fn default() -> Self {
        Self {
            max_bytes: 65_536,
            max_tokens: DEFAULT_MAX_TOKENS,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProfileValidator {
    limits: ProfileLimits,
}

impl ProfileValidator {
    /// Fail-closed: zero or above-ceiling bounds are rejected.
    pub fn new(limits: ProfileLimits) -> Result<Self, ConfigError> {
        let check = |name: &'static str, value: usize, ceiling: usize| {
            if value == 0 {
                Err(ConfigError::ZeroLimit(name))
            } else if value > ceiling {
                Err(ConfigError::AboveCeiling(name))
            } else {
                Ok(())
            }
        };
        check("profile_max_bytes", limits.max_bytes, crate::limits::MAX_SOURCE_BYTES_CEILING)?;
        check("profile_max_tokens", limits.max_tokens, crate::limits::MAX_SOURCE_BYTES_CEILING)?;
        check("profile_max_depth", limits.max_depth, MAX_DEPTH_CEILING)?;
        Ok(Self { limits })
    }

    /// Default token/depth bounds with the byte bound of the provider's quotas.
    pub fn for_max_bytes(max_bytes: usize) -> Self {
        Self {
            limits: ProfileLimits {
                max_bytes,
                ..ProfileLimits::default()
            },
        }
    }

    pub const fn limits(&self) -> ProfileLimits {
        self.limits
    }

    /// Validates `source` against profile v1: the first violation, or the facts of the accepted source.
    pub fn validate(&self, source: &str) -> Result<ValidatedSource, ProfileViolation> {
        let limits = self.limits;
        if source.len() > limits.max_bytes {
            return Err(fail(ViolationKind::SourceTooLarge, FIRST, 0));
        }
        if let Some(span) = nul_span(source) {
            return Err(fail(ViolationKind::NulCharacter, span, 1));
        }
        let mut scan = Scan {
            src: source,
            pos: 0,
            line: 1,
            col: 1,
            line_has_token: false,
        };
        let mut stack: Vec<Open> = Vec::new();
        let mut last = Last::Start;
        let mut tokens = 0_usize;
        let mut deepest = 0_usize;
        let mut accessor: Option<SourceSpan> = None;
        loop {
            scan.skip_trivia()?;
            let Some(token) = scan.token(is_value(last))? else {
                break;
            };
            tokens = tokens.saturating_add(1);
            if tokens > limits.max_tokens {
                return Err(fail(ViolationKind::TooManyTokens, token.span, token.len));
            }
            if let Some(pending) = accessor.take() {
                let follows = match token.tok {
                    Tok::Ident(_) | Tok::Str | Tok::Number => true,
                    Tok::Punct("[") => matches!(stack.last(), Some(Open::Brace)),
                    Tok::Punct(_) => false,
                };
                if follows {
                    return Err(fail(ViolationKind::Accessor, pending, 3));
                }
            }
            match token.tok {
                Tok::Ident(word) => {
                    if DENIED_KEYWORDS.contains(&word) {
                        return Err(fail(ViolationKind::DeniedKeyword, token.span, token.len));
                    }
                    if DENIED_NAMES.contains(&word) {
                        return Err(fail(ViolationKind::DeniedName, token.span, token.len));
                    }
                    let member_start = matches!(last, Last::Punct("{") | Last::Punct(","))
                        && matches!(stack.last(), Some(Open::Brace));
                    if (word == "get" || word == "set") && member_start {
                        accessor = Some(token.span);
                    }
                    last = Last::Ident(word);
                }
                Tok::Number | Tok::Str => last = Last::Literal,
                Tok::Punct(punct) => {
                    match punct {
                        "(" | "[" | "{" => {
                            if stack.len() >= limits.max_depth {
                                return Err(fail(ViolationKind::TooDeep, token.span, token.len));
                            }
                            stack.push(match punct {
                                "(" => Open::Paren {
                                    control: matches!(last, Last::Ident(w) if CONTROL_KEYWORDS.contains(&w)),
                                },
                                "[" => Open::Bracket,
                                _ => Open::Brace,
                            });
                            deepest = deepest.max(stack.len());
                            last = Last::Punct(punct);
                        }
                        ")" => match stack.pop() {
                            Some(Open::Paren { control: true }) => last = Last::CloseControl,
                            Some(Open::Paren { control: false }) => last = Last::Punct(")"),
                            _ => return Err(fail(ViolationKind::UnbalancedBracket, token.span, 1)),
                        },
                        "]" => match stack.pop() {
                            Some(Open::Bracket) => last = Last::Punct("]"),
                            _ => return Err(fail(ViolationKind::UnbalancedBracket, token.span, 1)),
                        },
                        "}" => match stack.pop() {
                            Some(Open::Brace) => last = Last::Punct("}"),
                            _ => return Err(fail(ViolationKind::UnbalancedBracket, token.span, 1)),
                        },
                        "*" => {
                            let generator = matches!(last, Last::Ident("function"))
                                || matches!(last, Last::Punct("{") | Last::Punct(","));
                            if generator {
                                return Err(fail(ViolationKind::GeneratorFunction, token.span, 1));
                            }
                            last = Last::Punct(punct);
                        }
                        "++" | "--" => {
                            last = if is_value(last) {
                                Last::Postfix
                            } else {
                                Last::Punct(punct)
                            };
                        }
                        _ => last = Last::Punct(punct),
                    }
                }
            }
        }
        if !stack.is_empty() {
            return Err(fail(ViolationKind::UnexpectedEnd, scan.here(), 0));
        }
        Ok(ValidatedSource {
            bytes: source.len(),
            tokens,
            max_depth: deepest,
        })
    }
}

const FIRST: SourceSpan = SourceSpan { line: 1, column: 1 };

/// Position (1-based line and column, counted exactly as the scanner counts) of the first raw U+0000
/// in `source`, or `None`. Shared with the QuickJS backend so both layers report the same span.
pub(crate) fn nul_span(source: &str) -> Option<SourceSpan> {
    let end = source.find('\0')?;
    let (mut line, mut column) = (1_u32, 1_u32);
    let mut chars = source.get(..end).unwrap_or("").chars().peekable();
    while let Some(c) = chars.next() {
        if is_line_terminator(c) {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            line = line.saturating_add(1);
            column = 1;
        } else {
            column = column.saturating_add(1);
        }
    }
    Some(SourceSpan { line, column })
}

fn fail(kind: ViolationKind, span: SourceSpan, len: u32) -> ProfileViolation {
    ProfileViolation { kind, span, len }
}

#[derive(Clone, Copy)]
enum Open {
    Paren { control: bool },
    Bracket,
    Brace,
}

/// The previous significant token, as far as the regex/division and member-start decisions need it.
#[derive(Clone, Copy)]
enum Last<'a> {
    Start,
    Ident(&'a str),
    /// Number or string literal.
    Literal,
    /// `++` / `--` directly after a value.
    Postfix,
    Punct(&'static str),
    /// `)` closing the head of `if` / `while` / `for`: a statement follows, so `/` is a regex.
    CloseControl,
}

/// A `/` after a value is a division; after anything else (including `}`) it starts a regex, which the
/// profile refuses, so an ambiguous `/` is always refused (fail closed).
fn is_value(last: Last<'_>) -> bool {
    match last {
        Last::Ident(word) => !EXPRESSION_KEYWORDS.contains(&word),
        Last::Literal | Last::Postfix => true,
        Last::Punct(punct) => punct == ")" || punct == "]",
        Last::Start | Last::CloseControl => false,
    }
}

#[derive(Clone, Copy)]
enum Tok<'a> {
    Ident(&'a str),
    Number,
    Str,
    Punct(&'static str),
}

struct Token<'a> {
    tok: Tok<'a>,
    span: SourceSpan,
    len: u32,
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\u{b}' | '\u{c}' | ' ' | '\u{a0}' | '\u{feff}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
            | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

fn is_ident_part(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

struct Scan<'a> {
    src: &'a str,
    pos: usize,
    line: u32,
    col: u32,
    /// A token was lexed on the current line (decides whether `-->` is an HTML-like comment).
    line_has_token: bool,
}

impl<'a> Scan<'a> {
    fn rest(&self) -> &'a str {
        self.src.get(self.pos..).unwrap_or("")
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        self.rest().chars().nth(n)
    }

    fn here(&self) -> SourceSpan {
        SourceSpan {
            line: self.line,
            column: self.col,
        }
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos = self.pos.saturating_add(c.len_utf8());
        if c == '\r' && self.peek() == Some('\n') {
            self.pos = self.pos.saturating_add(1);
        }
        if is_line_terminator(c) {
            self.line = self.line.saturating_add(1);
            self.col = 1;
            self.line_has_token = false;
        } else {
            self.col = self.col.saturating_add(1);
        }
        Some(c)
    }

    fn len_since(&self, start: usize) -> u32 {
        let text = self.src.get(start..self.pos).unwrap_or("");
        u32::try_from(text.chars().count()).unwrap_or(u32::MAX)
    }

    fn skip_trivia(&mut self) -> Result<(), ProfileViolation> {
        loop {
            match self.peek() {
                Some(c) if is_whitespace(c) || is_line_terminator(c) => {
                    self.bump();
                }
                Some('/') => match self.peek_nth(1) {
                    Some('/') => {
                        while self.peek().is_some_and(|c| !is_line_terminator(c)) {
                            self.bump();
                        }
                    }
                    Some('*') => {
                        let span = self.here();
                        self.bump();
                        self.bump();
                        loop {
                            match self.bump() {
                                None => {
                                    return Err(fail(ViolationKind::UnterminatedComment, span, 2));
                                }
                                Some('*') if self.peek() == Some('/') => {
                                    self.bump();
                                    break;
                                }
                                Some(_) => {}
                            }
                        }
                    }
                    _ => return Ok(()),
                },
                _ => return Ok(()),
            }
        }
    }

    /// Next token, or `None` at the end of the input. `value_before` decides `/` (division vs regex).
    fn token(&mut self, value_before: bool) -> Result<Option<Token<'a>>, ProfileViolation> {
        let Some(c) = self.peek() else {
            return Ok(None);
        };
        let span = self.here();
        let start = self.pos;
        let at_line_start = !self.line_has_token;
        let refuse = |kind, len| Err(fail(kind, span, len));
        let tok = match c {
            'a'..='z' | 'A'..='Z' | '_' | '$' => {
                while self.peek().is_some_and(is_ident_part) {
                    self.bump();
                }
                Tok::Ident(self.src.get(start..self.pos).unwrap_or(""))
            }
            '0'..='9' => self.number(span)?,
            '.' if matches!(self.peek_nth(1), Some('0'..='9')) => self.number(span)?,
            '"' | '\'' => self.string(c, span)?,
            '`' => return refuse(ViolationKind::TemplateLiteral, 1),
            '\\' => return refuse(ViolationKind::IdentifierEscape, 1),
            '#' => return refuse(ViolationKind::PrivateName, 1),
            '/' if !value_before => return refuse(ViolationKind::RegexLiteral, 1),
            '<' if self.rest().starts_with("<!--") => {
                return refuse(ViolationKind::HtmlComment, 4);
            }
            '-' if at_line_start && self.rest().starts_with("-->") => {
                return refuse(ViolationKind::HtmlComment, 3);
            }
            c if c.is_ascii() => {
                let rest = self.rest();
                let Some(mut punct) = PUNCTUATORS.iter().copied().find(|p| rest.starts_with(p)) else {
                    return refuse(ViolationKind::InvalidCharacter, 1);
                };
                if punct == "?." && matches!(rest.chars().nth(2), Some('0'..='9')) {
                    punct = "?";
                }
                let len = u32::try_from(punct.len()).unwrap_or(u32::MAX);
                match punct {
                    "?." => return refuse(ViolationKind::OptionalChaining, len),
                    "??" | "??=" | "&&=" | "||=" => {
                        return refuse(ViolationKind::PostEs2018Operator, len);
                    }
                    _ => {}
                }
                for _ in 0..punct.len() {
                    self.bump();
                }
                Tok::Punct(punct)
            }
            c if c.is_alphabetic() => return refuse(ViolationKind::NonAsciiIdentifier, 1),
            _ => return refuse(ViolationKind::InvalidCharacter, 1),
        };
        self.line_has_token = true;
        Ok(Some(Token {
            tok,
            span,
            len: self.len_since(start),
        }))
    }

    fn number(&mut self, span: SourceSpan) -> Result<Tok<'a>, ProfileViolation> {
        let start = self.pos;
        let first = self.peek();
        let second = self.peek_nth(1);
        let radix = match (first, second) {
            (Some('0'), Some('x' | 'X')) => Some(16),
            (Some('0'), Some('o' | 'O')) => Some(8),
            (Some('0'), Some('b' | 'B')) => Some(2),
            _ => None,
        };
        if let Some(radix) = radix {
            self.bump();
            self.bump();
            let mut digits = 0_usize;
            while self.peek().is_some_and(|d| d.is_digit(radix)) {
                self.bump();
                digits = digits.saturating_add(1);
            }
            if digits == 0 {
                return Err(fail(ViolationKind::InvalidNumber, span, self.len_since(start)));
            }
        } else if first == Some('0') && matches!(second, Some('0'..='9')) {
            return Err(fail(ViolationKind::LegacyOctal, span, 2));
        } else {
            while self.peek().is_some_and(|d| d.is_ascii_digit()) {
                self.bump();
            }
            if self.peek() == Some('.') {
                self.bump();
                while self.peek().is_some_and(|d| d.is_ascii_digit()) {
                    self.bump();
                }
            }
            if matches!(self.peek(), Some('e' | 'E')) {
                let sign = matches!(self.peek_nth(1), Some('+' | '-'));
                let digit_at = if sign { 2 } else { 1 };
                if !matches!(self.peek_nth(digit_at), Some('0'..='9')) {
                    return Err(fail(ViolationKind::InvalidNumber, span, self.len_since(start)));
                }
                self.bump();
                if sign {
                    self.bump();
                }
                while self.peek().is_some_and(|d| d.is_ascii_digit()) {
                    self.bump();
                }
            }
        }
        match self.peek() {
            Some('_') => Err(fail(ViolationKind::NumericSeparator, span, self.len_since(start))),
            Some('n') => Err(fail(ViolationKind::BigIntLiteral, span, self.len_since(start))),
            Some(c) if is_ident_part(c) || c == '\\' || (!c.is_ascii() && c.is_alphabetic()) => {
                Err(fail(ViolationKind::InvalidNumber, span, self.len_since(start)))
            }
            _ => Ok(Tok::Number),
        }
    }

    fn hex(&mut self, count: usize) -> Option<u32> {
        let mut value = 0_u32;
        for _ in 0..count {
            let digit = self.peek()?.to_digit(16)?;
            self.bump();
            value = value.checked_mul(16)?.checked_add(digit)?;
        }
        Some(value)
    }

    /// Reads the escape after a backslash; the cooked character, `None` for a line continuation or a
    /// lone surrogate (never equals a denied name), `Err` for a malformed or legacy escape.
    fn escape(&mut self) -> Result<Option<char>, ()> {
        let Some(c) = self.bump() else {
            return Err(());
        };
        Ok(match c {
            c if is_line_terminator(c) => None,
            'x' => char::from_u32(self.hex(2).ok_or(())?),
            'u' if self.peek() == Some('{') => {
                self.bump();
                let mut value = 0_u32;
                let mut digits = 0_usize;
                while let Some(d) = self.peek().and_then(|d| d.to_digit(16)) {
                    self.bump();
                    digits = digits.saturating_add(1);
                    value = value.checked_mul(16).and_then(|v| v.checked_add(d)).ok_or(())?;
                    if value > 0x0010_ffff {
                        return Err(());
                    }
                }
                if digits == 0 || self.bump() != Some('}') {
                    return Err(());
                }
                char::from_u32(value)
            }
            'u' => char::from_u32(self.hex(4).ok_or(())?),
            '0' if !matches!(self.peek(), Some('0'..='9')) => Some('\0'),
            '0'..='9' => return Err(()),
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            'b' => Some('\u{8}'),
            'f' => Some('\u{c}'),
            'v' => Some('\u{b}'),
            other => Some(other),
        })
    }

    fn string(&mut self, quote: char, span: SourceSpan) -> Result<Tok<'a>, ProfileViolation> {
        let start = self.pos;
        self.bump();
        let mut cooked = String::new();
        let mut overflow = false;
        loop {
            let piece = match self.bump() {
                None | Some('\n' | '\r') => {
                    return Err(fail(ViolationKind::UnterminatedString, span, self.len_since(start)));
                }
                Some(c) if c == quote => break,
                Some('\\') => match self.escape() {
                    Ok(piece) => piece,
                    Err(()) => {
                        return Err(fail(ViolationKind::InvalidEscape, span, self.len_since(start)));
                    }
                },
                Some(c) => Some(c),
            };
            if let Some(c) = piece {
                if cooked.len() < MAX_COOKED_BYTES {
                    cooked.push(c);
                } else {
                    overflow = true;
                }
            }
        }
        // Escapes are decoded first (V4-08): only the exact cooked value is compared. A lone
        // surrogate is skipped (a harmless false positive), a line continuation yields nothing, and
        // a value longer than the cap cannot equal any denied name.
        if !overflow && DENIED_NAMES.contains(&cooked.as_str()) {
            return Err(fail(ViolationKind::DeniedString, span, self.len_since(start)));
        }
        Ok(Tok::Str)
    }
}
