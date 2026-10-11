//! URL references in CSS text, found by tokenizing per CSS Syntax Level 3 instead of matching raw bytes.
//!
//! The scanner decodes escapes first (4.3.7 "consume an escaped code point": 1-6 hex digits plus one optional
//! whitespace, 0 / surrogates / above U+10FFFF become U+FFFD, any other escaped character stands for itself;
//! an escaped newline inside a string is a line continuation), then decides what a token is:
//! - an identifier whose decoded text is `url` (ASCII case-insensitive) immediately followed by `(` is a url
//!   function: `u\72l(`, `\75rl(`, `URL(` all qualify, `xurl(`, `1url(`, `url (` and `url/**/(` do not;
//! - quoted forms (`url("..")`, `url( '..' )`) and `@import "..."` hold a string token;
//! - unquoted forms hold a url token: backslash escapes are decoded, whitespace may surround the value, and a
//!   token that breaks the grammar (quote, `(`, control character, text after whitespace) is a bad-url, which
//!   is still reported (`bad`) so callers deny it instead of ignoring it;
//! - comments, strings, hash names, numbers/units and every other identifier are skipped.
//!
//! The reported value is the decoded text; the span is the source bytes holding it (escapes included).
#![deny(clippy::arithmetic_side_effects)]

use std::ops::Range;

pub(crate) struct CssRef {
    /// Source bytes of the reference (inside the quotes or parentheses, escapes undecoded).
    pub span: Range<usize>,
    /// The decoded reference.
    pub value: String,
    /// Bad-url or bad-string: a browser would not fetch it; the caller must not offer it to a port.
    pub bad: bool,
}

struct Cur<'a> {
    s: &'a str,
    pos: usize,
}

fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{c}')
}
fn is_ws(c: char) -> bool {
    is_newline(c) || matches!(c, ' ' | '\t')
}
fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}
fn is_name_char(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-'
}
fn is_non_printable(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{e}'..='\u{1f}' | '\u{7f}')
}
fn clean(c: char) -> char {
    if c == '\0' { '\u{fffd}' } else { c }
}
fn valid_escape(first: Option<char>, second: Option<char>) -> bool {
    first == Some('\\') && !second.is_some_and(is_newline)
}

impl<'a> Cur<'a> {
    fn rest(&self) -> &'a str {
        self.s.get(self.pos..).unwrap_or("")
    }
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }
    fn peek2(&self) -> Option<char> {
        self.rest().chars().nth(1)
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos = self.pos.saturating_add(c.len_utf8());
        Some(c)
    }
    fn skip_comment(&mut self) {
        // positioned at `/*`
        let body = self.rest().get(2..).unwrap_or("");
        self.pos = match body.find("*/") {
            Some(off) => self.pos.saturating_add(2).saturating_add(off).saturating_add(2),
            None => self.s.len(),
        };
    }
    fn skip_ws_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(c) if is_ws(c) => {
                    self.bump();
                }
                Some('/') if self.peek2() == Some('*') => self.skip_comment(),
                _ => break,
            }
        }
    }
    fn starts_ident(&self) -> bool {
        let mut it = self.rest().chars();
        let (a, b, c) = (it.next(), it.next(), it.next());
        match a {
            Some('-') => b.is_some_and(|b| is_name_start(b) || b == '-') || valid_escape(b, c),
            Some(a) if is_name_start(a) => true,
            Some('\\') => valid_escape(a, b),
            _ => false,
        }
    }
    /// After the backslash was consumed (4.3.7).
    fn escaped(&mut self) -> char {
        match self.bump() {
            None => '\u{fffd}',
            Some(c) if c.is_ascii_hexdigit() => {
                let mut value = c.to_digit(16).unwrap_or(0);
                let mut digits = 1u8;
                while digits < 6
                    && let Some(d) = self.peek().and_then(|c| c.to_digit(16))
                {
                    value = value.saturating_mul(16).saturating_add(d);
                    self.bump();
                    digits = digits.saturating_add(1);
                }
                if let Some(w) = self.peek().filter(|c| is_ws(*c)) {
                    self.bump();
                    if w == '\r' && self.peek() == Some('\n') {
                        self.bump();
                    }
                }
                char::from_u32(value).filter(|c| *c != '\0').unwrap_or('\u{fffd}')
            }
            Some(c) => clean(c),
        }
    }
    /// 4.3.11 consume an ident sequence (decoded).
    fn ident(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.peek() {
                Some(c) if is_name_char(c) => {
                    out.push(clean(c));
                    self.bump();
                }
                Some('\\') if valid_escape(Some('\\'), self.peek2()) => {
                    self.bump();
                    out.push(self.escaped());
                }
                _ => break,
            }
        }
        out
    }
    /// 4.3.5 consume a string token after its opening quote. Returns (value, content span, bad).
    fn string(&mut self, quote: char) -> (String, Range<usize>, bool) {
        let start = self.pos;
        let mut value = String::new();
        let mut bad = false;
        let end = loop {
            let at = self.pos;
            match self.bump() {
                None => break at,
                Some(c) if c == quote => break at,
                Some(c) if is_newline(c) => {
                    self.pos = at;
                    bad = true;
                    break at;
                }
                Some('\\') => match self.peek() {
                    None => {}
                    Some(n) if is_newline(n) => {
                        self.bump();
                        if n == '\r' && self.peek() == Some('\n') {
                            self.bump();
                        }
                    }
                    Some(_) => value.push(self.escaped()),
                },
                Some(c) => value.push(clean(c)),
            }
        };
        (value, start..end, bad)
    }
    /// 4.3.15 consume the remnants of a bad url.
    fn remnants(&mut self) {
        loop {
            match self.bump() {
                None | Some(')') => break,
                Some('\\') if valid_escape(Some('\\'), self.peek()) => {
                    self.escaped();
                }
                Some(_) => {}
            }
        }
    }
    /// 4.3.6 consume a url token (whitespace already skipped, `url(` consumed).
    fn url_token(&mut self) -> CssRef {
        let start = self.pos;
        let mut value = String::new();
        let mut bad = false;
        let end = loop {
            let at = self.pos;
            match self.bump() {
                None | Some(')') => break at,
                Some(c) if is_ws(c) => {
                    while self.peek().is_some_and(is_ws) {
                        self.bump();
                    }
                    match self.peek() {
                        None => break at,
                        Some(')') => {
                            self.bump();
                            break at;
                        }
                        Some(_) => {
                            bad = true;
                            self.remnants();
                            break at;
                        }
                    }
                }
                Some('"' | '\'' | '(') => {
                    bad = true;
                    self.remnants();
                    break at;
                }
                Some(c) if is_non_printable(c) => {
                    bad = true;
                    self.remnants();
                    break at;
                }
                Some('\\') => {
                    if valid_escape(Some('\\'), self.peek()) {
                        value.push(self.escaped());
                    } else {
                        bad = true;
                        self.remnants();
                        break at;
                    }
                }
                Some(c) => value.push(clean(c)),
            }
        };
        CssRef { span: start..end, value, bad }
    }
}

/// References found in `css` (at most `limit`), in source order: `url(...)` functions and `@import "..."`.
pub(crate) fn css_references(css: &str, limit: usize) -> Vec<CssRef> {
    let mut cur = Cur { s: css, pos: 0 };
    let mut out: Vec<CssRef> = Vec::new();
    while out.len() < limit {
        let Some(c) = cur.peek() else { break };
        match c {
            '/' if cur.peek2() == Some('*') => cur.skip_comment(),
            '"' | '\'' => {
                cur.bump();
                cur.string(c);
            }
            '0'..='9' => {
                while cur.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
                    cur.bump();
                }
                if cur.starts_ident() {
                    cur.ident();
                }
            }
            '@' => {
                cur.bump();
                if cur.starts_ident() && cur.ident().eq_ignore_ascii_case("import") {
                    cur.skip_ws_and_comments();
                    if let Some(q @ ('"' | '\'')) = cur.peek() {
                        cur.bump();
                        let (value, span, bad) = cur.string(q);
                        out.push(CssRef { span, value, bad });
                    }
                }
            }
            '#' => {
                cur.bump();
                cur.ident();
            }
            _ if cur.starts_ident() => {
                let name = cur.ident();
                if cur.peek() == Some('(') {
                    cur.bump();
                    if name.eq_ignore_ascii_case("url") {
                        while cur.peek().is_some_and(is_ws) {
                            cur.bump();
                        }
                        if let Some(q @ ('"' | '\'')) = cur.peek() {
                            cur.bump();
                            let (value, span, bad) = cur.string(q);
                            out.push(CssRef { span, value, bad });
                        } else {
                            let token = cur.url_token();
                            if !(token.span.is_empty() && token.value.is_empty()) {
                                out.push(token);
                            }
                        }
                    }
                }
            }
            _ => {
                cur.bump();
            }
        }
    }
    out
}
