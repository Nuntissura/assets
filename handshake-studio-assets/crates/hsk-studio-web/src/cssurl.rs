//! The URLs a browser loads from CSS text, found on the token stream of a CSS Syntax Level 3 tokenizer.
//!
//! Tokenizer: CSS Syntax Module Level 3, W3C Candidate Recommendation Draft 1 October 2026
//! (<https://www.w3.org/TR/css-syntax-3/>), implemented algorithm by algorithm; every function below cites the section it
//! implements: 3.3 (preprocessing, applied on the fly so spans stay source byte offsets), 4.2 (definitions), 4.3.1
//! (consume a token, every case incl. CDO/CDC, hash, at-keyword, the `+`/`-`/`.` number checks and the opt-in
//! unicode-range), 4.3.2 (comments: only where a token starts, never inside a string or url token: test
//! `comments_only_where_a_token_starts`), 4.3.3 (numeric
//! token), 4.3.4 (ident-like token: `url(` with the whitespace/quote rule), 4.3.5 (string), 4.3.6 (url), 4.3.7 (escaped
//! code point), 4.3.8-4.3.11 (checks), 4.3.12 (ident sequence), 4.3.13 (number: a `.` only before a digit, the exponent
//! only before a digit), 4.3.14 (unicode-range) and 4.3.15 (bad-url remnants). Proof: `cssurl::tests` (one group per
//! algorithm, the CC0 css-parsing-tests corpus) and the gate tests in `tests/css_url_gate.rs`.
//!
//! Two readings of "non-ASCII ident code point" exist: the 2026 specification text (4.2: the ranges allowed in HTML custom
//! element names) and the engines (Blink `css_parser_idioms.h` IsNameStartCodePoint, WebKit `CSSParserIdioms.h`
//! isNameStartCodePoint and Servo/Gecko cssparser `consume_name` treat every code point >= U+0080 as a name code point).
//! `css_references` scans the text under both and reports the union, so a URL either reading loads is reported.
//!
//! URL rules on the token stream (the block structure is CSS Syntax 3 5.4: `(`, `[`, `{` and functions nest, and only the
//! matching closer ends a block):
//! - every `<url-token>` is a URL (CSS Values 4 4.5); every `<bad-url-token>` is reported `Bad` (a browser loads nothing,
//!   the gate denies it);
//! - a `<string-token>` is a URL when the nearest enclosing function is `url()` or `src()` (CSS Values 4 4.5: `url(
//!   <string> <url-modifier>* )`, `src( <string> <url-modifier>* )`), `image-set()` or its parse-time alias
//!   `-webkit-image-set()` (CSS Images 4 2.4 "Each <string> inside image-set() represents a <url>", Appendix A),
//!   `image()` (CSS Images 4 2.5 "A <string> used in image() represents a <url>") or `cross-fade()` /
//!   `-webkit-cross-fade()` (no string in its grammar, reported conservatively), and when it directly follows `@import`
//!   (whitespace between); a `<bad-string-token>` there is reported `Bad`;
//! - inside such a function any function outside a fixed list of value functions (gradients, colours, math, `type()`,
//!   URL modifiers, the URL functions themselves) is reported `Computed`: `var()`, `attr()`, `env()`, `if()`,
//!   `inherit()`, custom `--*()` functions and every unknown name can produce a URL string at computed-value time (CSS
//!   Values 4 4.5 `src(var(--foo))`, CSS Values 5 arbitrary substitution functions), which the source does not show;
//! - an empty URL is skipped: it resolves to an invalid resource and loads nothing (CSS Values 4 4.5.2).
//!
//! The reported value is the decoded text; the span is the source bytes holding it (escapes included).
#![deny(clippy::arithmetic_side_effects)]

use std::collections::BTreeSet;
use std::ops::Range;

/// Which code points are non-ASCII ident code points (CSS Syntax 3 4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NonAscii {
    /// The specification text: U+00B7, U+00C0-U+00D6, U+00D8-U+00F6, U+00F8-U+037D, U+037F-U+1FFF, U+200C, U+200D,
    /// U+203F, U+2040, U+2070-U+218F, U+2C00-U+2FEF, U+3001-U+D7FF, U+F900-U+FDCF, U+FDF0-U+FFFD, >= U+10000.
    Spec,
    /// Every code point >= U+0080 (Blink, WebKit, Servo/Gecko).
    Engines,
}

/// One CSS token (CSS Syntax 3 4) with the source bytes it was read from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Token {
    pub kind: Kind,
    pub span: Range<usize>,
}

/// CSS Syntax 3 4 token types. Ranges are source byte offsets: `inner` = the contents of a string or url token (inside
/// the quotes or parentheses, surrounding whitespace excluded), `repr` = the number part of a numeric token. `eof` marks
/// a string or url token that ended at the end of the input (a parse error, 4.3.5 / 4.3.6). The bad tokens carry what
/// was read before the error (`partial`), so the gate can show it.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "numeric details and unicode ranges are read by the tokenizer tests only"
    )
)]
pub(crate) enum Kind {
    Ident(String),
    Function(String),
    AtKeyword(String),
    Hash {
        value: String,
        id: bool,
    },
    String {
        value: String,
        inner: Range<usize>,
        eof: bool,
    },
    BadString {
        partial: String,
        inner: Range<usize>,
    },
    Url {
        value: String,
        inner: Range<usize>,
        eof: bool,
    },
    BadUrl {
        partial: String,
        inner: Range<usize>,
    },
    Delim(char),
    Number {
        repr: Range<usize>,
        integer: bool,
    },
    Percentage {
        repr: Range<usize>,
        integer: bool,
    },
    Dimension {
        repr: Range<usize>,
        integer: bool,
        unit: String,
    },
    UnicodeRange {
        start: u32,
        end: u32,
    },
    Whitespace,
    Cdo,
    Cdc,
    Colon,
    Semicolon,
    Comma,
    OpenSquare,
    CloseSquare,
    OpenParen,
    CloseParen,
    OpenCurly,
    CloseCurly,
}

// ----- 4.2 definitions (applied to code points after 3.3 preprocessing) --------------------------------------------

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}
fn is_hex(c: char) -> bool {
    c.is_ascii_hexdigit()
}
/// 4.2 whitespace: a newline (U+000A after preprocessing), U+0009 or U+0020.
fn is_ws(c: char) -> bool {
    matches!(c, '\n' | '\t' | ' ')
}
/// 4.2 non-printable code point.
fn is_non_printable(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{e}'..='\u{1f}' | '\u{7f}')
}
/// 4.2 non-ASCII ident code point, specification text.
pub(crate) fn spec_non_ascii_ident(c: char) -> bool {
    matches!(u32::from(c), 0xb7 | 0xc0..=0xd6 | 0xd8..=0xf6 | 0xf8..=0x37d | 0x37f..=0x1fff | 0x200c | 0x200d
        | 0x203f | 0x2040 | 0x2070..=0x218f | 0x2c00..=0x2fef | 0x3001..=0xd7ff | 0xf900..=0xfdcf | 0xfdf0..=0xfffd
        | 0x10000..)
}
/// 4.3.8 check if two code points are a valid escape.
fn valid_escape(first: Option<char>, second: Option<char>) -> bool {
    first == Some('\\') && second != Some('\n')
}
/// 4.3.10 check if three code points would start a number.
fn starts_number(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
    match a {
        Some('+' | '-') => b.is_some_and(is_digit) || (b == Some('.') && c.is_some_and(is_digit)),
        Some('.') => b.is_some_and(is_digit),
        Some(a) => is_digit(a),
        None => false,
    }
}
/// 4.3.11 check if three code points would start a unicode-range.
fn starts_unicode_range(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
    matches!(a, Some('u' | 'U')) && b == Some('+') && c.is_some_and(|c| c == '?' || is_hex(c))
}

/// CSS Syntax 3 tokenizer over `&str`, one token per `next_token` call.
pub(crate) struct Tokenizer<'a> {
    s: &'a str,
    pos: usize,
    non_ascii: NonAscii,
    /// 4.3.1 "unicode ranges allowed" (only the unicode-range descriptor's value is tokenized with it, 5.5.11).
    unicode_ranges: bool,
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn new(s: &'a str, non_ascii: NonAscii, unicode_ranges: bool) -> Self {
        Self {
            s,
            pos: 0,
            non_ascii,
            unicode_ranges,
        }
    }

    /// 3.3 filter code points: the code point a CSS parser sees at byte `at`, and how many source bytes it stands for
    /// (CR LF, CR and FF become one LF; U+0000 becomes U+FFFD; a `&str` holds no surrogates).
    fn at(&self, at: usize) -> Option<(char, usize)> {
        let rest = self.s.get(at..)?;
        let c = rest.chars().next()?;
        Some(match c {
            '\r' if rest.as_bytes().get(1) == Some(&b'\n') => ('\n', 2),
            '\r' | '\u{c}' => ('\n', 1),
            '\0' => ('\u{fffd}', 1),
            c => (c, c.len_utf8()),
        })
    }

    /// 4.2 the `n`-th next input code point (0 = next); `None` = EOF.
    fn peek(&self, n: usize) -> Option<char> {
        let mut at = self.pos;
        let mut left = n;
        loop {
            let (c, len) = self.at(at)?;
            if left == 0 {
                return Some(c);
            }
            left = left.saturating_sub(1);
            at = at.saturating_add(len);
        }
    }

    /// 4.2 consume the next input code point.
    fn bump(&mut self) -> Option<char> {
        let (c, len) = self.at(self.pos)?;
        self.pos = self.pos.saturating_add(len);
        Some(c)
    }

    /// 4.2 ident-start code point.
    fn ident_start(&self, c: char) -> bool {
        c.is_ascii_alphabetic()
            || c == '_'
            || match self.non_ascii {
                NonAscii::Spec => spec_non_ascii_ident(c),
                NonAscii::Engines => !c.is_ascii(),
            }
    }
    /// 4.2 ident code point.
    fn ident_cp(&self, c: char) -> bool {
        self.ident_start(c) || is_digit(c) || c == '-'
    }
    /// 4.3.9 check if three code points would start an ident sequence.
    fn starts_ident(&self, a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
        match a {
            Some('-') => b.is_some_and(|b| self.ident_start(b) || b == '-') || valid_escape(b, c),
            Some('\\') => valid_escape(a, b),
            Some(a) => self.ident_start(a),
            None => false,
        }
    }

    /// 4.3.1 consume a token (after 4.3.2 consume comments); `None` = the EOF token.
    pub(crate) fn next_token(&mut self) -> Option<Token> {
        self.comments();
        let start = self.pos;
        let p0 = self.peek(0)?;
        let (p1, p2) = (self.peek(1), self.peek(2));
        let kind = match p0 {
            c if is_ws(c) => {
                while self.peek(0).is_some_and(is_ws) {
                    self.bump();
                }
                Kind::Whitespace
            }
            '"' | '\'' => {
                self.bump();
                self.string(p0)
            }
            '#' => {
                self.bump();
                let (a, b, c) = (self.peek(0), self.peek(1), self.peek(2));
                if a.is_some_and(|a| self.ident_cp(a)) || valid_escape(a, b) {
                    let id = self.starts_ident(a, b, c);
                    Kind::Hash {
                        value: self.ident_sequence(),
                        id,
                    }
                } else {
                    Kind::Delim('#')
                }
            }
            '(' => self.single(Kind::OpenParen),
            ')' => self.single(Kind::CloseParen),
            '+' | '.' if starts_number(Some(p0), p1, p2) => self.numeric(),
            ',' => self.single(Kind::Comma),
            '-' if starts_number(Some(p0), p1, p2) => self.numeric(),
            '-' if p1 == Some('-') && p2 == Some('>') => {
                self.bump();
                self.bump();
                self.bump();
                Kind::Cdc
            }
            '-' if self.starts_ident(Some(p0), p1, p2) => self.ident_like(),
            ':' => self.single(Kind::Colon),
            ';' => self.single(Kind::Semicolon),
            '<' if p1 == Some('!') && p2 == Some('-') && self.peek(3) == Some('-') => {
                for _ in 0..4 {
                    self.bump();
                }
                Kind::Cdo
            }
            '@' => {
                self.bump();
                let (a, b, c) = (self.peek(0), self.peek(1), self.peek(2));
                if self.starts_ident(a, b, c) {
                    Kind::AtKeyword(self.ident_sequence())
                } else {
                    Kind::Delim('@')
                }
            }
            '[' => self.single(Kind::OpenSquare),
            '\\' if valid_escape(Some(p0), p1) => self.ident_like(),
            ']' => self.single(Kind::CloseSquare),
            '{' => self.single(Kind::OpenCurly),
            '}' => self.single(Kind::CloseCurly),
            c if is_digit(c) => self.numeric(),
            'u' | 'U' if self.unicode_ranges && starts_unicode_range(Some(p0), p1, p2) => {
                self.unicode_range()
            }
            c if self.ident_start(c) => self.ident_like(),
            // `+`, `-`, `.`, `<` that start nothing, a `\` that is no valid escape (a parse error), anything else.
            c => self.single(Kind::Delim(c)),
        };
        Some(Token {
            kind,
            span: start..self.pos,
        })
    }

    fn single(&mut self, kind: Kind) -> Kind {
        self.bump();
        kind
    }

    /// 4.3.2 consume comments: `/*` up to and including the first `*/`, or to EOF; repeated.
    fn comments(&mut self) {
        while self.peek(0) == Some('/') && self.peek(1) == Some('*') {
            let body = self.pos.saturating_add(2);
            self.pos = match self.s.get(body..).and_then(|r| r.find("*/")) {
                Some(off) => body.saturating_add(off).saturating_add(2),
                None => self.s.len(),
            };
        }
    }

    /// 4.3.3 consume a numeric token.
    fn numeric(&mut self) -> Kind {
        let (repr, integer) = self.number();
        let (a, b, c) = (self.peek(0), self.peek(1), self.peek(2));
        if self.starts_ident(a, b, c) {
            let unit = self.ident_sequence();
            Kind::Dimension {
                repr,
                integer,
                unit,
            }
        } else if a == Some('%') {
            self.bump();
            Kind::Percentage { repr, integer }
        } else {
            Kind::Number { repr, integer }
        }
    }

    /// 4.3.13 consume a number: optional sign, digits, a `.` only when a digit follows it, an exponent only when a digit
    /// follows the `e`/`E` (after an optional sign). Returns the source bytes of the number and the "integer" type flag.
    fn number(&mut self) -> (Range<usize>, bool) {
        let start = self.pos;
        let mut integer = true;
        if matches!(self.peek(0), Some('+' | '-')) {
            self.bump();
        }
        while self.peek(0).is_some_and(is_digit) {
            self.bump();
        }
        if self.peek(0) == Some('.') && self.peek(1).is_some_and(is_digit) {
            self.bump();
            while self.peek(0).is_some_and(is_digit) {
                self.bump();
            }
            integer = false;
        }
        let (a, b, c) = (self.peek(0), self.peek(1), self.peek(2));
        if matches!(a, Some('e' | 'E'))
            && (b.is_some_and(is_digit)
                || (matches!(b, Some('+' | '-')) && c.is_some_and(is_digit)))
        {
            self.bump();
            if matches!(self.peek(0), Some('+' | '-')) {
                self.bump();
            }
            while self.peek(0).is_some_and(is_digit) {
                self.bump();
            }
            integer = false;
        }
        (start..self.pos, integer)
    }

    /// 4.3.4 consume an ident-like token: `url(` followed (after at most one kept whitespace) by a quote is a function
    /// token, otherwise a url token; any other name followed by `(` is a function token.
    fn ident_like(&mut self) -> Kind {
        let name = self.ident_sequence();
        if name.eq_ignore_ascii_case("url") && self.peek(0) == Some('(') {
            self.bump();
            while self.peek(0).is_some_and(is_ws) && self.peek(1).is_some_and(is_ws) {
                self.bump();
            }
            let quote = |c: Option<char>| matches!(c, Some('"' | '\''));
            let (a, b) = (self.peek(0), self.peek(1));
            if quote(a) || (a.is_some_and(is_ws) && quote(b)) {
                Kind::Function(name)
            } else {
                self.url()
            }
        } else if self.peek(0) == Some('(') {
            self.bump();
            Kind::Function(name)
        } else {
            Kind::Ident(name)
        }
    }

    /// 4.3.5 consume a string token (the opening quote consumed; `ending` closes it).
    fn string(&mut self, ending: char) -> Kind {
        let start = self.pos;
        let mut value = String::new();
        loop {
            let at = self.pos;
            match self.bump() {
                None => {
                    return Kind::String {
                        value,
                        inner: start..at,
                        eof: true,
                    };
                }
                Some(c) if c == ending => {
                    return Kind::String {
                        value,
                        inner: start..at,
                        eof: false,
                    };
                }
                Some('\n') => {
                    // Reconsume the newline: it is not part of the bad string.
                    self.pos = at;
                    return Kind::BadString {
                        partial: value,
                        inner: start..at,
                    };
                }
                Some('\\') => match self.peek(0) {
                    None => {}
                    Some('\n') => {
                        self.bump();
                    }
                    Some(_) => value.push(self.escaped()),
                },
                Some(c) => value.push(c),
            }
        }
    }

    /// 4.3.6 consume a url token (`url(` and the whitespace before the value already consumed by 4.3.4's loop).
    fn url(&mut self) -> Kind {
        while self.peek(0).is_some_and(is_ws) {
            self.bump();
        }
        let start = self.pos;
        let mut value = String::new();
        loop {
            let at = self.pos;
            match self.bump() {
                Some(')') => {
                    return Kind::Url {
                        value,
                        inner: start..at,
                        eof: false,
                    };
                }
                None => {
                    return Kind::Url {
                        value,
                        inner: start..at,
                        eof: true,
                    };
                }
                Some(c) if is_ws(c) => {
                    while self.peek(0).is_some_and(is_ws) {
                        self.bump();
                    }
                    return match self.peek(0) {
                        Some(')') => {
                            self.bump();
                            Kind::Url {
                                value,
                                inner: start..at,
                                eof: false,
                            }
                        }
                        None => Kind::Url {
                            value,
                            inner: start..at,
                            eof: true,
                        },
                        Some(_) => self.bad_url(value, start..at),
                    };
                }
                Some('"' | '\'' | '(') => return self.bad_url(value, start..at),
                Some(c) if is_non_printable(c) => return self.bad_url(value, start..at),
                Some('\\') => {
                    if valid_escape(Some('\\'), self.peek(0)) {
                        value.push(self.escaped());
                    } else {
                        return self.bad_url(value, start..at);
                    }
                }
                Some(c) => value.push(c),
            }
        }
    }

    fn bad_url(&mut self, partial: String, inner: Range<usize>) -> Kind {
        self.remnants();
        Kind::BadUrl { partial, inner }
    }

    /// 4.3.15 consume the remnants of a bad url: up to and including `)` or EOF; an escaped `)` does not end it.
    fn remnants(&mut self) {
        loop {
            match self.bump() {
                None | Some(')') => return,
                Some('\\') if valid_escape(Some('\\'), self.peek(0)) => {
                    self.escaped();
                }
                Some(_) => {}
            }
        }
    }

    /// 4.3.7 consume an escaped code point (the `\` consumed, the escape checked valid): 1-6 hex digits plus one optional
    /// whitespace (0, surrogates and values above U+10FFFF become U+FFFD), EOF becomes U+FFFD, anything else itself.
    fn escaped(&mut self) -> char {
        match self.bump() {
            None => '\u{fffd}',
            Some(c) if is_hex(c) => {
                let mut value = c.to_digit(16).unwrap_or(0);
                let mut digits = 1u8;
                while digits < 6
                    && let Some(d) = self.peek(0).and_then(|c| c.to_digit(16))
                {
                    value = value.saturating_mul(16).saturating_add(d);
                    self.bump();
                    digits = digits.saturating_add(1);
                }
                if self.peek(0).is_some_and(is_ws) {
                    self.bump();
                }
                if value == 0 || (0xd800..=0xdfff).contains(&value) || value > 0x10_ffff {
                    '\u{fffd}'
                } else {
                    char::from_u32(value).unwrap_or('\u{fffd}')
                }
            }
            Some(c) => c,
        }
    }

    /// 4.3.12 consume an ident sequence (decoded).
    fn ident_sequence(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.peek(0) {
                Some(c) if self.ident_cp(c) => {
                    out.push(c);
                    self.bump();
                }
                a if valid_escape(a, self.peek(1)) => {
                    self.bump();
                    out.push(self.escaped());
                }
                _ => return out,
            }
        }
    }

    /// 4.3.14 consume a unicode-range token (the stream starts a unicode-range, 4.3.11).
    fn unicode_range(&mut self) -> Kind {
        self.bump();
        self.bump();
        let mut first = String::new();
        while first.len() < 6
            && let Some(c) = self.peek(0).filter(|c| is_hex(*c))
        {
            first.push(c);
            self.bump();
        }
        while first.len() < 6 && self.peek(0) == Some('?') {
            first.push('?');
            self.bump();
        }
        let hex = |s: &str| u32::from_str_radix(s, 16).unwrap_or(0);
        if first.contains('?') {
            return Kind::UnicodeRange {
                start: hex(&first.replace('?', "0")),
                end: hex(&first.replace('?', "F")),
            };
        }
        let start = hex(&first);
        if self.peek(0) == Some('-') && self.peek(1).is_some_and(is_hex) {
            self.bump();
            let mut second = String::new();
            while second.len() < 6
                && let Some(c) = self.peek(0).filter(|c| is_hex(*c))
            {
                second.push(c);
                self.bump();
            }
            return Kind::UnicodeRange {
                start,
                end: hex(&second),
            };
        }
        Kind::UnicodeRange { start, end: start }
    }
}

// ----- URL references -------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RefKind {
    /// A URL a browser loads.
    Url,
    /// A bad-url token, or a bad-string token where a URL string belongs: a browser loads nothing; never offered to a port
    /// (gate test `url_tokens_escapes_comments_and_bad_urls_follow_the_tokenizer`).
    Bad,
    /// A function inside a URL-taking function whose result the source does not show (`var()`, `attr()`, custom functions,
    /// ...): the URL is computed at style time; denied.
    Computed,
}

pub(crate) struct CssRef {
    /// Source bytes of the reference (inside the quotes or parentheses, escapes undecoded; the whole function for
    /// `Computed`).
    pub span: Range<usize>,
    /// The decoded reference (`Computed`: the function as written).
    pub value: String,
    pub kind: RefKind,
}

/// Blocks and functions open at once before the scan refuses (the caller reports `LimitExceeded(Depth)`).
pub(crate) const MAX_NESTING: usize = 1024;

/// Functions whose string arguments are URLs (see the module docs for the specification of each).
const URL_FUNCTIONS: &[&str] = &[
    "url",
    "src",
    "image-set",
    "-webkit-image-set",
    "image",
    "cross-fade",
    "-webkit-cross-fade",
];

/// Value functions that may appear inside a URL-taking function without computing a URL string: the URL functions, the
/// `image-set()` `type()`, URL modifiers, `element()`, gradients, colours and math. Anything else is `Computed`.
const VALUE_FUNCTIONS: &[&str] = &[
    "url",
    "src",
    "image-set",
    "-webkit-image-set",
    "image",
    "cross-fade",
    "-webkit-cross-fade",
    "type",
    "cross-origin",
    "integrity",
    "referrer-policy",
    "element",
    "-moz-element",
    "linear-gradient",
    "radial-gradient",
    "conic-gradient",
    "repeating-linear-gradient",
    "repeating-radial-gradient",
    "repeating-conic-gradient",
    "-webkit-linear-gradient",
    "-webkit-radial-gradient",
    "-webkit-repeating-linear-gradient",
    "-webkit-repeating-radial-gradient",
    "-webkit-gradient",
    "color-stop",
    "from",
    "to",
    "rgb",
    "rgba",
    "hsl",
    "hsla",
    "hwb",
    "lab",
    "lch",
    "oklab",
    "oklch",
    "color",
    "color-mix",
    "light-dark",
    "contrast-color",
    "device-cmyk",
    "calc",
    "-webkit-calc",
    "min",
    "max",
    "clamp",
    "round",
    "mod",
    "rem",
    "abs",
    "sign",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "pow",
    "sqrt",
    "hypot",
    "log",
    "exp",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// `(`, `[` or `{` block.
    Block,
    /// A function from `URL_FUNCTIONS`.
    Url,
    /// Any other function.
    Other,
}

struct Frame {
    close: Kind,
    class: Class,
    /// Index in the output of the `Computed` reference this function opened.
    computed: Option<usize>,
}

/// The references one reading of `css` holds, at most `limit`; `Err(span)` when blocks nest deeper than `MAX_NESTING`.
fn scan(css: &str, non_ascii: NonAscii, limit: usize) -> Result<Vec<CssRef>, Range<usize>> {
    let mut tokens = Tokenizer::new(css, non_ascii, false);
    let mut out: Vec<CssRef> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut url_functions = 0usize;
    let mut computed_open = false;
    let mut after_import = false;
    // A string is a URL when the nearest enclosing function is a URL function.
    let string_is_url = |frames: &[Frame]| {
        frames
            .iter()
            .rev()
            .find(|f| f.class != Class::Block)
            .is_some_and(|f| f.class == Class::Url)
    };
    while out.len() < limit {
        let Some(token) = tokens.next_token() else {
            break;
        };
        let import = std::mem::replace(&mut after_import, false);
        let open = |frames: &mut Vec<Frame>, close: Kind, class: Class, computed| {
            if frames.len() >= MAX_NESTING {
                return Err(token.span.clone());
            }
            frames.push(Frame {
                close,
                class,
                computed,
            });
            Ok(())
        };
        match token.kind {
            Kind::Whitespace => after_import = import,
            Kind::AtKeyword(ref name) => after_import = name.eq_ignore_ascii_case("import"),
            Kind::Url { value, inner, .. } => {
                if !value.is_empty() {
                    out.push(CssRef {
                        span: inner,
                        value,
                        kind: RefKind::Url,
                    });
                }
            }
            Kind::BadUrl { partial, inner } => out.push(CssRef {
                span: inner,
                value: partial,
                kind: RefKind::Bad,
            }),
            Kind::String { value, inner, .. } => {
                if (import || string_is_url(&frames)) && !value.is_empty() {
                    out.push(CssRef {
                        span: inner,
                        value,
                        kind: RefKind::Url,
                    });
                }
            }
            Kind::BadString { partial, inner } => {
                if import || string_is_url(&frames) {
                    out.push(CssRef {
                        span: inner,
                        value: partial,
                        kind: RefKind::Bad,
                    });
                }
            }
            Kind::Function(ref name) => {
                let lower = name.to_ascii_lowercase();
                let mut computed = None;
                if url_functions > 0 && !computed_open && !VALUE_FUNCTIONS.contains(&lower.as_str())
                {
                    out.push(CssRef {
                        span: token.span.start..css.len(),
                        value: String::new(),
                        kind: RefKind::Computed,
                    });
                    computed = Some(out.len().saturating_sub(1));
                    computed_open = true;
                }
                let class = if URL_FUNCTIONS.contains(&lower.as_str()) {
                    url_functions = url_functions.saturating_add(1);
                    Class::Url
                } else {
                    Class::Other
                };
                open(&mut frames, Kind::CloseParen, class, computed)?;
            }
            Kind::OpenParen => open(&mut frames, Kind::CloseParen, Class::Block, None)?,
            Kind::OpenSquare => open(&mut frames, Kind::CloseSquare, Class::Block, None)?,
            Kind::OpenCurly => open(&mut frames, Kind::CloseCurly, Class::Block, None)?,
            ref closer @ (Kind::CloseParen | Kind::CloseSquare | Kind::CloseCurly) => {
                // 5.4: only the closer that matches the innermost block ends it; any other is a preserved token.
                if frames.last().is_some_and(|f| f.close == *closer)
                    && let Some(frame) = frames.pop()
                {
                    if frame.class == Class::Url {
                        url_functions = url_functions.saturating_sub(1);
                    }
                    if let Some(i) = frame.computed {
                        if let Some(r) = out.get_mut(i) {
                            r.span.end = token.span.end;
                        }
                        computed_open = false;
                    }
                }
            }
            _ => {}
        }
    }
    for r in &mut out {
        if r.kind == RefKind::Computed {
            r.value = css.get(r.span.clone()).unwrap_or_default().to_owned();
        }
    }
    Ok(out)
}

/// References found in `css` (at most `limit`), in source order: url tokens, bad-url tokens, URL strings of `url()`,
/// `src()`, `image-set()`, `-webkit-image-set()`, `image()`, `cross-fade()` and `@import`, and computed URLs (see the
/// module docs). When `css` holds a code point the two non-ASCII readings disagree on, both readings are scanned and the
/// union is reported. `Err(span)`: blocks nest deeper than `MAX_NESTING`.
pub(crate) fn css_references(css: &str, limit: usize) -> Result<Vec<CssRef>, Range<usize>> {
    let mut out = scan(css, NonAscii::Spec, limit)?;
    if css
        .chars()
        .any(|c| !c.is_ascii() && !spec_non_ascii_ident(c))
    {
        let key = |r: &CssRef| (r.span.start, r.span.end, r.kind, r.value.clone());
        let mut seen: BTreeSet<(usize, usize, RefKind, String)> = out.iter().map(key).collect();
        for r in scan(css, NonAscii::Engines, limit)? {
            if seen.insert(key(&r)) {
                out.push(r);
            }
        }
        out.sort_by_key(|r| (r.span.start, r.span.end, r.kind));
        out.truncate(limit);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
