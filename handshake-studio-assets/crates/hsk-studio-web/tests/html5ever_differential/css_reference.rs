//! Reference reading of the URLs in a CSS text, written separately from the crate's `cssurl` for the gate-level CSS
//! check (signature `css-urls`). Structure differs on purpose: the text is preprocessed first into a code-point array with
//! source offsets (CSS Syntax 3, W3C CRD 2026-10-01, 3.3), tokenized with an index cursor following the 4.3 algorithms
//! literally (consume / reconsume), parsed into a component-value tree (5.4 consume a list of component values, simple
//! block, function), and the tree is walked for URLs. The URL rules are the gate's documented policy (`cssurl` module
//! docs): url tokens; strings whose nearest enclosing function is url, src, image-set, -webkit-image-set, image,
//! cross-fade or -webkit-cross-fade, or that follow `@import`; bad-url tokens and bad strings in those places as bad;
//! inside a URL function, the first function outside the value-function list as computed; empty URLs skipped; the union
//! of three readings: the specification's, the engines' non-ASCII ident code points (>= U+0080), and Gecko's (engines'
//! code points, and after whitespace in an unquoted url the next code point is consumed before the bad-url remnants, as
//! Servo cssparser 0.38 `consume_url_end` does).

use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reading {
    Spec,
    Engines,
    Gecko,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum UrlKind {
    Url,
    Bad,
    Computed,
}

/// One URL the reference reads: source offset of its token (or function), kind, value.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct RefUrl {
    pub at: usize,
    pub kind: UrlKind,
    pub value: String,
}

#[derive(Clone, Debug)]
enum T {
    Ws,
    Str { value: String, at: usize },
    BadStr { partial: String, at: usize },
    Url { value: String, at: usize },
    BadUrl { partial: String, at: usize },
    Ident,
    Function(String),
    AtKeyword(String),
    Other,
    Open(char),
    Close(char),
}

struct Lexer {
    cp: Vec<char>,
    /// Source byte offset of each preprocessed code point; `off[cp.len()]` = source length.
    off: Vec<usize>,
    i: usize,
    reading: Reading,
}

fn preprocess(src: &str) -> (Vec<char>, Vec<usize>) {
    let mut cp = Vec::new();
    let mut off = Vec::new();
    let mut it = src.char_indices().peekable();
    while let Some((at, c)) = it.next() {
        off.push(at);
        cp.push(match c {
            '\r' => {
                if it.peek().map(|p| p.1) == Some('\n') {
                    it.next();
                }
                '\n'
            }
            '\u{c}' => '\n',
            '\0' => '\u{fffd}',
            c => c,
        });
    }
    off.push(src.len());
    (cp, off)
}

impl Lexer {
    fn at(&self, k: usize) -> Option<char> {
        self.cp.get(self.i + k).copied()
    }
    fn non_ascii_ident(&self, c: char) -> bool {
        match self.reading {
            Reading::Engines | Reading::Gecko => c as u32 >= 0x80,
            Reading::Spec => {
                let v = c as u32;
                v == 0xb7
                    || (0xc0..=0xd6).contains(&v)
                    || (0xd8..=0xf6).contains(&v)
                    || (0xf8..=0x37d).contains(&v)
                    || (0x37f..=0x1fff).contains(&v)
                    || v == 0x200c
                    || v == 0x200d
                    || v == 0x203f
                    || v == 0x2040
                    || (0x2070..=0x218f).contains(&v)
                    || (0x2c00..=0x2fef).contains(&v)
                    || (0x3001..=0xd7ff).contains(&v)
                    || (0xf900..=0xfdcf).contains(&v)
                    || (0xfdf0..=0xfffd).contains(&v)
                    || v >= 0x10000
            }
        }
    }
    fn start(&self, c: Option<char>) -> bool {
        c.is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || self.non_ascii_ident(c))
    }
    fn name(&self, c: Option<char>) -> bool {
        self.start(c) || c.is_some_and(|c| c.is_ascii_digit() || c == '-')
    }
    fn ws(c: Option<char>) -> bool {
        matches!(c, Some('\n' | '\t' | ' '))
    }
    fn escape(a: Option<char>, b: Option<char>) -> bool {
        a == Some('\\') && b != Some('\n')
    }
    fn would_ident(&self, k: usize) -> bool {
        let (a, b, c) = (self.at(k), self.at(k + 1), self.at(k + 2));
        if a == Some('-') {
            self.start(b) || b == Some('-') || Self::escape(b, c)
        } else if a == Some('\\') {
            Self::escape(a, b)
        } else {
            self.start(a)
        }
    }
    fn would_number(&self, k: usize) -> bool {
        let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
        let (a, b, c) = (self.at(k), self.at(k + 1), self.at(k + 2));
        match a {
            Some('+' | '-') => digit(b) || (b == Some('.') && digit(c)),
            Some('.') => digit(b),
            _ => digit(a),
        }
    }
    /// 4.3.7, after the backslash.
    fn escaped(&mut self) -> char {
        let Some(c) = self.at(0) else {
            return '\u{fffd}';
        };
        self.i += 1;
        if !c.is_ascii_hexdigit() {
            return c;
        }
        let mut hex = String::from(c);
        while hex.len() < 6 && self.at(0).is_some_and(|c| c.is_ascii_hexdigit()) {
            hex.push(self.at(0).unwrap());
            self.i += 1;
        }
        if Self::ws(self.at(0)) {
            self.i += 1;
        }
        let v = u32::from_str_radix(&hex, 16).unwrap();
        if v == 0 || (0xd800..=0xdfff).contains(&v) || v > 0x10ffff {
            '\u{fffd}'
        } else {
            char::from_u32(v).unwrap()
        }
    }
    /// 4.3.12
    fn ident_seq(&mut self) -> String {
        let mut s = String::new();
        loop {
            if self.name(self.at(0)) {
                s.push(self.at(0).unwrap());
                self.i += 1;
            } else if Self::escape(self.at(0), self.at(1)) {
                self.i += 1;
                s.push(self.escaped());
            } else {
                return s;
            }
        }
    }
    fn number(&mut self) {
        let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
        if matches!(self.at(0), Some('+' | '-')) {
            self.i += 1;
        }
        while digit(self.at(0)) {
            self.i += 1;
        }
        if self.at(0) == Some('.') && digit(self.at(1)) {
            self.i += 2;
            while digit(self.at(0)) {
                self.i += 1;
            }
        }
        if matches!(self.at(0), Some('e' | 'E'))
            && (digit(self.at(1)) || (matches!(self.at(1), Some('+' | '-')) && digit(self.at(2))))
        {
            self.i += 2;
            while digit(self.at(0)) {
                self.i += 1;
            }
        }
        if self.would_ident(0) {
            self.ident_seq();
        } else if self.at(0) == Some('%') {
            self.i += 1;
        }
    }
    fn remnants(&mut self) {
        while let Some(c) = self.at(0) {
            self.i += 1;
            if c == ')' {
                return;
            }
            if c == '\\' && Self::escape(Some(c), self.at(0)) {
                self.escaped();
            }
        }
    }
    /// 4.3.6, after `url(` and the whitespace loop.
    fn url(&mut self) -> T {
        while Self::ws(self.at(0)) {
            self.i += 1;
        }
        let at = self.off[self.i];
        let mut value = String::new();
        loop {
            let Some(c) = self.at(0) else {
                return T::Url { value, at };
            };
            self.i += 1;
            match c {
                ')' => return T::Url { value, at },
                '\n' | '\t' | ' ' => {
                    while Self::ws(self.at(0)) {
                        self.i += 1;
                    }
                    if self.at(0).is_none() {
                        return T::Url { value, at };
                    }
                    if self.at(0) == Some(')') {
                        self.i += 1;
                        return T::Url { value, at };
                    }
                    if self.reading == Reading::Gecko {
                        self.i += 1;
                    }
                    self.remnants();
                    return T::BadUrl { partial: value, at };
                }
                '"' | '\'' | '(' | '\u{0}'..='\u{8}' | '\u{b}' | '\u{e}'..='\u{1f}' | '\u{7f}' => {
                    self.remnants();
                    return T::BadUrl { partial: value, at };
                }
                '\\' => {
                    if Self::escape(Some('\\'), self.at(0)) {
                        value.push(self.escaped());
                    } else {
                        self.remnants();
                        return T::BadUrl { partial: value, at };
                    }
                }
                c => value.push(c),
            }
        }
    }
    /// 4.3.5, after the opening quote.
    fn string(&mut self, quote: char) -> T {
        let at = self.off[self.i];
        let mut value = String::new();
        while let Some(c) = self.at(0) {
            if c == '\n' {
                return T::BadStr { partial: value, at };
            }
            self.i += 1;
            if c == quote {
                return T::Str { value, at };
            }
            if c == '\\' {
                match self.at(0) {
                    None => {}
                    Some('\n') => self.i += 1,
                    Some(_) => value.push(self.escaped()),
                }
            } else {
                value.push(c);
            }
        }
        T::Str { value, at }
    }
    fn ident_like(&mut self) -> T {
        let name = self.ident_seq();
        if self.at(0) != Some('(') {
            return T::Ident;
        }
        self.i += 1;
        if !name.eq_ignore_ascii_case("url") {
            return T::Function(name);
        }
        while Self::ws(self.at(0)) && Self::ws(self.at(1)) {
            self.i += 1;
        }
        let q = |c: Option<char>| matches!(c, Some('"' | '\''));
        if q(self.at(0)) || (Self::ws(self.at(0)) && q(self.at(1))) {
            T::Function(name)
        } else {
            self.url()
        }
    }
    /// 4.3.1 (comments first, 4.3.2); `None` at EOF. Returns the token and the source offset where it starts.
    fn token(&mut self) -> Option<(T, usize)> {
        while self.at(0) == Some('/') && self.at(1) == Some('*') {
            self.i += 2;
            while self.at(0).is_some() && !(self.at(0) == Some('*') && self.at(1) == Some('/')) {
                self.i += 1;
            }
            if self.at(0).is_some() {
                self.i += 2;
            }
        }
        let c = self.at(0)?;
        let begin = self.off[self.i];
        let t = if Self::ws(Some(c)) {
            while Self::ws(self.at(0)) {
                self.i += 1;
            }
            T::Ws
        } else if c == '"' || c == '\'' {
            self.i += 1;
            self.string(c)
        } else if c == '#' {
            self.i += 1;
            if self.name(self.at(0)) || Self::escape(self.at(0), self.at(1)) {
                self.ident_seq();
            }
            T::Other
        } else if matches!(c, '(' | '[' | '{') {
            self.i += 1;
            T::Open(c)
        } else if matches!(c, ')' | ']' | '}') {
            self.i += 1;
            T::Close(c)
        } else if (c == '+' || c == '.' || c == '-') && self.would_number(0) {
            self.number();
            T::Other
        } else if c == '-' && self.at(1) == Some('-') && self.at(2) == Some('>') {
            self.i += 3;
            T::Other
        } else if c == '-' && self.would_ident(0) {
            self.ident_like()
        } else if c == '<'
            && self.at(1) == Some('!')
            && self.at(2) == Some('-')
            && self.at(3) == Some('-')
        {
            self.i += 4;
            T::Other
        } else if c == '@' {
            self.i += 1;
            if self.would_ident(0) {
                T::AtKeyword(self.ident_seq())
            } else {
                T::Other
            }
        } else if c == '\\' && Self::escape(Some(c), self.at(1)) {
            self.ident_like()
        } else if c.is_ascii_digit() {
            self.number();
            T::Other
        } else if self.start(Some(c)) {
            self.ident_like()
        } else {
            self.i += 1;
            T::Other
        };
        Some((t, begin))
    }
}

/// 5.4 component values: a token, a simple block or a function.
#[derive(Debug)]
enum Cv {
    Token(T),
    Block(Vec<Cv>),
    Function {
        name: String,
        at: usize,
        end: usize,
        items: Vec<Cv>,
    },
}

fn closer(open: char) -> char {
    match open {
        '(' => ')',
        '[' => ']',
        _ => '}',
    }
}

/// Consume component values until `close` (the closer is consumed) or EOF; returns the items and the source offset after
/// the closer (or the source end).
fn list(lx: &mut Lexer, close: Option<char>) -> (Vec<Cv>, usize) {
    let mut items = Vec::new();
    while let Some((t, begin)) = lx.token() {
        match t {
            T::Close(c) if Some(c) == close => return (items, lx.off[lx.i]),
            T::Open(o) => {
                let (inner, _) = list(lx, Some(closer(o)));
                items.push(Cv::Block(inner));
            }
            T::Function(name) => {
                let (inner, end) = list(lx, Some(')'));
                items.push(Cv::Function {
                    name,
                    at: begin,
                    end,
                    items: inner,
                });
            }
            t => items.push(Cv::Token(t)),
        }
    }
    let end = *lx.off.last().unwrap();
    (items, end)
}

const URL_FUNCTIONS: &[&str] = &[
    "url",
    "src",
    "image-set",
    "-webkit-image-set",
    "image",
    "cross-fade",
    "-webkit-cross-fade",
];

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

struct Walk<'a> {
    src: &'a str,
    out: Vec<RefUrl>,
}

impl Walk<'_> {
    /// `string_url`: the nearest enclosing function is a URL function; `in_url`: some enclosing function is;
    /// `computed`: inside a function already reported as computed.
    fn items(&mut self, items: &[Cv], string_url: bool, in_url: bool, computed: bool) {
        let mut import = false;
        for item in items {
            let was_import = std::mem::replace(&mut import, false);
            match item {
                Cv::Token(T::Ws) => import = was_import,
                Cv::Token(T::AtKeyword(name)) => import = name.eq_ignore_ascii_case("import"),
                Cv::Token(T::Url { value, at }) if !value.is_empty() => {
                    self.push(*at, UrlKind::Url, value)
                }
                Cv::Token(T::BadUrl { partial, at }) => self.push(*at, UrlKind::Bad, partial),
                Cv::Token(T::Str { value, at })
                    if (string_url || was_import) && !value.is_empty() =>
                {
                    self.push(*at, UrlKind::Url, value)
                }
                Cv::Token(T::BadStr { partial, at }) if string_url || was_import => {
                    self.push(*at, UrlKind::Bad, partial)
                }
                Cv::Token(_) => {}
                Cv::Block(inner) => self.items(inner, string_url, in_url, computed),
                Cv::Function {
                    name,
                    at,
                    end,
                    items: inner,
                } => {
                    let lower = name.to_ascii_lowercase();
                    let mut now_computed = computed;
                    if in_url && !computed && !VALUE_FUNCTIONS.contains(&lower.as_str()) {
                        let text = self.src[*at..*end].to_owned();
                        self.push(*at, UrlKind::Computed, &text);
                        now_computed = true;
                    }
                    let is_url = URL_FUNCTIONS.contains(&lower.as_str());
                    self.items(inner, is_url, in_url || is_url, now_computed);
                }
            }
        }
    }
    fn push(&mut self, at: usize, kind: UrlKind, value: &str) {
        self.out.push(RefUrl {
            at,
            kind,
            value: value.to_owned(),
        });
    }
}

fn read(src: &str, reading: Reading) -> Vec<RefUrl> {
    let (cp, off) = preprocess(src);
    let mut lx = Lexer {
        cp,
        off,
        i: 0,
        reading,
    };
    let (items, _) = list(&mut lx, None);
    let mut walk = Walk {
        src,
        out: Vec::new(),
    };
    walk.items(&items, false, false, false);
    walk.out
}

/// The URLs in `src` under all three readings (union, always all three), ordered by source offset.
pub fn urls(src: &str) -> Vec<RefUrl> {
    let set: BTreeSet<RefUrl> = read(src, Reading::Spec)
        .into_iter()
        .chain(read(src, Reading::Engines))
        .chain(read(src, Reading::Gecko))
        .collect();
    set.into_iter().collect()
}
