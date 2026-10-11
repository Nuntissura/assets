//! Reference reading of the URLs in a CSS text, written separately from the crate's `cssurl` for the gate-level CSS
//! check (signature `css-urls`). Structure differs on purpose: the text is preprocessed first into a code-point array with
//! source offsets (CSS Syntax 3, W3C CRD 2026-10-01, 3.3), tokenized with an index cursor following the 4.3 algorithms
//! literally (consume / reconsume), parsed into a component-value tree (5.4 consume a list of component values, simple
//! block, function), and the tree is walked for URLs. The URL rules are the gate's documented policy (`cssurl` module
//! docs): url tokens; strings whose nearest enclosing function is url, src, image-set, -webkit-image-set, image,
//! cross-fade, -webkit-cross-fade, filter or -webkit-filter, or that follow `@import`; the URL right after `@namespace`
//! (and an optional prefix) skipped; the `@import` URL marked; bad-url tokens and bad strings in those places as bad;
//! inside a URL function, the first function outside the value-function list as computed; empty URLs skipped; a `data:`
//! URL whose payload is CSS (or, as an `@import`, a type a style sheet reads as CSS in quirks mode) followed by the URLs of
//! its payload, read by a second, separately written data: URL reader (Fetch data: URL processor, MIME Sniffing, Infra
//! forgiving-base64, CSS Syntax 3 3.2 encodings); the union
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
    /// The URL of an `@import` (a style sheet).
    pub import: bool,
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
    "filter",
    "-webkit-filter",
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
    "-moz-linear-gradient",
    "-moz-radial-gradient",
    "-moz-repeating-linear-gradient",
    "-moz-repeating-radial-gradient",
    "filter",
    "-webkit-filter",
    "blur",
    "brightness",
    "contrast",
    "drop-shadow",
    "grayscale",
    "hue-rotate",
    "invert",
    "opacity",
    "saturate",
    "sepia",
    "alpha",
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

/// What the previous significant item set up: `@import` (its URL is a style sheet), `@namespace` and its optional prefix
/// (their URL is a namespace name, never loaded).
#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    Nothing,
    Import,
    Namespace,
    NamespacePrefix,
}

impl Walk<'_> {
    /// `string_url`: the nearest enclosing function is a URL function; `in_url`: some enclosing function is;
    /// `computed`: inside a function already reported as computed; `mark`: the URL function was opened right after
    /// `@import` or `@namespace`.
    fn items(&mut self, items: &[Cv], string_url: bool, in_url: bool, computed: bool, mark: After) {
        let mut after = After::Nothing;
        for item in items {
            let was = std::mem::replace(&mut after, After::Nothing);
            let namespace = matches!(was, After::Namespace | After::NamespacePrefix);
            let import = was == After::Import || (string_url && mark == After::Import);
            let skip_strings =
                string_url && matches!(mark, After::Namespace | After::NamespacePrefix);
            match item {
                Cv::Token(T::Ws) => after = was,
                Cv::Token(T::AtKeyword(name)) if name.eq_ignore_ascii_case("import") => {
                    after = After::Import
                }
                Cv::Token(T::AtKeyword(name)) if name.eq_ignore_ascii_case("namespace") => {
                    after = After::Namespace
                }
                Cv::Token(T::Ident) if was == After::Namespace => after = After::NamespacePrefix,
                Cv::Token(T::Url { .. } | T::BadUrl { .. } | T::Str { .. } | T::BadStr { .. })
                    if namespace => {}
                Cv::Token(T::Url { value, at }) if !value.is_empty() => {
                    self.push(*at, UrlKind::Url, value, was == After::Import)
                }
                Cv::Token(T::BadUrl { partial, at }) => {
                    self.push(*at, UrlKind::Bad, partial, was == After::Import)
                }
                Cv::Token(T::Str { value, at })
                    if (string_url || was == After::Import)
                        && !skip_strings
                        && !value.is_empty() =>
                {
                    self.push(*at, UrlKind::Url, value, import)
                }
                Cv::Token(T::BadStr { partial, at })
                    if (string_url || was == After::Import) && !skip_strings =>
                {
                    self.push(*at, UrlKind::Bad, partial, import)
                }
                Cv::Token(_) => {}
                Cv::Block(inner) => self.items(inner, string_url, in_url, computed, mark),
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
                        self.push(*at, UrlKind::Computed, &text, false);
                        now_computed = true;
                    }
                    let is_url = URL_FUNCTIONS.contains(&lower.as_str());
                    let inner_mark = if matches!(lower.as_str(), "url" | "src") {
                        was
                    } else {
                        After::Nothing
                    };
                    self.items(inner, is_url, in_url || is_url, now_computed, inner_mark);
                }
            }
        }
    }
    fn push(&mut self, at: usize, kind: UrlKind, value: &str, import: bool) {
        self.out.push(RefUrl {
            at,
            kind,
            value: value.to_owned(),
            import,
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
    walk.items(&items, false, false, false, After::Nothing);
    walk.out
}

/// The URLs in `src` under all three readings (union, always all three), ordered by source offset; each URL that is a
/// `data:` URL whose payload the gate reads as CSS is followed by the URLs of that payload (recursively).
pub fn urls(src: &str) -> Vec<RefUrl> {
    let set: BTreeSet<RefUrl> = read(src, Reading::Spec)
        .into_iter()
        .chain(read(src, Reading::Engines))
        .chain(read(src, Reading::Gecko))
        .collect();
    let mut out = Vec::new();
    for r in set {
        let nested = if r.kind == UrlKind::Url {
            data_payload_css(&r.value, r.import)
        } else {
            None
        };
        out.push(r);
        if let Some(css) = nested {
            out.extend(urls(&css));
        }
    }
    out
}

// ----- data: URLs (a second reading of Fetch's data: URL processor, written separately from the crate's `datauri`) ----

/// The CSS text the gate scans for a `data:` URL found in CSS (`import`: the URL of an `@import`), or `None`.
fn data_payload_css(url: &str, import: bool) -> Option<String> {
    // URL parser: strip C0 controls and spaces at both ends, drop tab/LF/CR everywhere, cut the fragment.
    let chars: Vec<char> = url
        .trim_matches(|c: char| (c as u32) <= 0x20)
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .take_while(|c| *c != '#')
        .collect();
    if chars.len() < 5
        || !chars[..5]
            .iter()
            .zip("data:".chars())
            .all(|(a, b)| a.to_ascii_lowercase() == b)
    {
        return None;
    }
    let rest = &chars[5..];
    let comma = rest.iter().position(|c| *c == ',')?;
    // The gate reads at most 4096 bytes of MIME part; a longer one is an unknown type (never CSS, never inert).
    if rest[..comma].iter().map(|c| c.len_utf8()).sum::<usize>() > 4096 {
        return None;
    }
    // The serialized MIME part: percent-encode what the opaque path / query states encode.
    let mut mime = String::new();
    let mut in_query = false;
    for (k, &c) in rest[..comma].iter().enumerate() {
        if c == '?' && !in_query {
            in_query = true;
            mime.push(c);
            continue;
        }
        let next = rest.get(k + 1).copied();
        let encode = (c as u32) < 0x20
            || (c as u32) > 0x7e
            || (in_query && matches!(c, ' ' | '"' | '<' | '>'))
            || (!in_query && c == ' ' && matches!(next, Some('?')));
        if encode {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                mime.push_str(&format!("%{byte:02X}"));
            }
        } else {
            mime.push(c);
        }
    }
    let mut mime = mime
        .trim_matches(|c: char| c.is_ascii_whitespace())
        .to_owned();
    let lower = mime.to_ascii_lowercase();
    let mut base64 = false;
    if lower.ends_with("base64") {
        let before = lower[..lower.len() - 6].trim_end_matches(' ');
        if before.ends_with(';') {
            base64 = true;
            mime.truncate(before.len() - 1);
        }
    }
    if mime.starts_with(';') {
        mime.insert_str(0, "text/plain");
    }
    let (essence, charset) = mime_essence(&mime);
    let inert = inert_type(&essence);
    if !(essence == "text/css" || (import && inert)) {
        return None;
    }
    // Body: percent-decode the UTF-8 bytes, then forgiving-base64.
    let raw: String = rest[comma + 1..].iter().collect();
    let bytes = raw.as_bytes();
    let mut body = Vec::new();
    let mut k = 0;
    while k < bytes.len() {
        let hexpair = (bytes[k] == b'%')
            .then(|| bytes.get(k + 1..k + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .filter(|h| h.chars().all(|c| c.is_ascii_hexdigit()))
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hexpair {
            Some(v) => {
                body.push(v);
                k += 3;
            }
            None => {
                body.push(bytes[k]);
                k += 1;
            }
        }
    }
    if base64 {
        body = base64_decode(&body)?;
    }
    decode_css(&body, charset.as_deref())
}

/// MIME Sniffing "parse a MIME type": (essence, charset); failure is text/plain;charset=US-ASCII.
fn mime_essence(input: &str) -> (String, Option<String>) {
    let token = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
    };
    let http_ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r');
    let input = input.trim_matches(http_ws);
    let fallback = || ("text/plain".to_owned(), Some("US-ASCII".to_owned()));
    let Some(slash) = input.find('/') else {
        return fallback();
    };
    let ty = &input[..slash];
    let after = &input[slash + 1..];
    let semi = after.find(';').unwrap_or(after.len());
    let sub = after[..semi].trim_end_matches(http_ws);
    if !token(ty) || !token(sub) {
        return fallback();
    }
    let essence = format!("{}/{}", ty.to_ascii_lowercase(), sub.to_ascii_lowercase());
    // Parameters, one at a time.
    let p: Vec<char> = after[semi..].chars().collect();
    let mut i = 0;
    let mut charset = None;
    while i < p.len() {
        i += 1; // the ';'
        while i < p.len() && http_ws(p[i]) {
            i += 1;
        }
        let name_start = i;
        while i < p.len() && p[i] != ';' && p[i] != '=' {
            i += 1;
        }
        let name: String = p[name_start..i]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase();
        if i < p.len() && p[i] == ';' {
            continue;
        }
        if i >= p.len() {
            break;
        }
        i += 1; // the '='
        if i >= p.len() {
            break;
        }
        let value: String = if p[i] == '"' {
            i += 1;
            let mut v = String::new();
            while i < p.len() {
                let c = p[i];
                i += 1;
                if c == '"' {
                    break;
                }
                if c == '\\' {
                    if i < p.len() {
                        v.push(p[i]);
                        i += 1;
                    } else {
                        v.push('\\');
                    }
                } else {
                    v.push(c);
                }
            }
            while i < p.len() && p[i] != ';' {
                i += 1;
            }
            v
        } else {
            let start = i;
            while i < p.len() && p[i] != ';' {
                i += 1;
            }
            let v: String = p[start..i].iter().collect();
            let v = v.trim_end_matches(http_ws).to_owned();
            if v.is_empty() {
                continue;
            }
            v
        };
        let quoted_ok = value
            .chars()
            .all(|c| c == '\t' || (' '..='~').contains(&c) || ('\u{80}'..='\u{ff}').contains(&c));
        if name == "charset" && charset.is_none() && quoted_ok {
            charset = Some(value);
        }
    }
    (essence, charset)
}

fn inert_type(essence: &str) -> bool {
    let (ty, sub) = essence.split_once('/').unwrap_or((essence, ""));
    match (ty, sub) {
        (_, s) if s.ends_with("+xml") => false,
        ("image", _) | ("font", _) => true,
        ("audio" | "video", s) => !s.contains("mpegurl"),
        ("text", "plain") => true,
        ("application", s) => {
            ["octet-stream", "ogg", "vnd.ms-fontobject"].contains(&s)
                || s.starts_with("font-")
                || s.starts_with("x-font-")
        }
        _ => false,
    }
}

/// Infra forgiving-base64 decode.
fn base64_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut d: Vec<u8> = data
        .iter()
        .copied()
        .filter(|b| !b" \t\n\r\x0c".contains(b))
        .collect();
    if d.len().is_multiple_of(4) {
        for _ in 0..2 {
            if d.last() == Some(&b'=') {
                d.pop();
            }
        }
    }
    if d.len() % 4 == 1 {
        return None;
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits: u64 = 0;
    let mut n = 0;
    let mut out = Vec::new();
    for b in d {
        let v = alphabet.iter().position(|a| *a == b)? as u64;
        bits = (bits << 6) | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    Some(out)
}

/// CSS Syntax 3 3.2 + Encoding Standard decode for the labels the gate supports; `None` = refused.
fn decode_css(bytes: &[u8], charset: Option<&str>) -> Option<String> {
    #[derive(PartialEq)]
    enum E {
        Utf8,
        Be,
        Le,
        W1252,
        Repl,
    }
    let label = |l: &str| -> Option<E> {
        let l = l
            .trim_matches(|c: char| c.is_ascii_whitespace())
            .to_ascii_lowercase();
        let any = |list: &[&str]| list.contains(&l.as_str());
        if any(&[
            "unicode-1-1-utf-8",
            "unicode11utf8",
            "unicode20utf8",
            "utf-8",
            "utf8",
            "x-unicode20utf8",
        ]) {
            Some(E::Utf8)
        } else if any(&["unicodefffe", "utf-16be"]) {
            Some(E::Be)
        } else if any(&[
            "csunicode",
            "iso-10646-ucs-2",
            "ucs-2",
            "unicode",
            "unicodefeff",
            "utf-16",
            "utf-16le",
        ]) {
            Some(E::Le)
        } else if any(&[
            "ansi_x3.4-1968",
            "ascii",
            "cp1252",
            "cp819",
            "csisolatin1",
            "ibm819",
            "iso-8859-1",
            "iso-ir-100",
            "iso8859-1",
            "iso88591",
            "iso_8859-1",
            "iso_8859-1:1987",
            "l1",
            "latin1",
            "us-ascii",
            "windows-1252",
            "x-cp1252",
        ]) {
            Some(E::W1252)
        } else if any(&[
            "csiso2022kr",
            "hz-gb-2312",
            "iso-2022-cn",
            "iso-2022-cn-ext",
            "iso-2022-kr",
            "replacement",
        ]) {
            Some(E::Repl)
        } else {
            None
        }
    };
    let (enc, body) = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        (E::Utf8, &bytes[3..])
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (E::Be, &bytes[2..])
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        (E::Le, &bytes[2..])
    } else if let Some(c) = charset {
        (label(c)?, bytes)
    } else if let Some(rest) = bytes.strip_prefix(b"@charset \"") {
        match rest.iter().position(|b| *b == b'"') {
            Some(q) if q + 12 <= 1024 && rest.get(q + 1) == Some(&b';') && rest[..q].is_ascii() => {
                match label(std::str::from_utf8(&rest[..q]).ok()?)? {
                    E::Be | E::Le => (E::Utf8, bytes),
                    e => (e, bytes),
                }
            }
            _ => (E::Utf8, bytes),
        }
    } else {
        (E::Utf8, bytes)
    };
    Some(match enc {
        E::Utf8 => String::from_utf8_lossy(body).into_owned(),
        E::Be | E::Le => {
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| {
                    if enc == E::Be {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        u16::from_le_bytes([c[0], c[1]])
                    }
                })
                .collect();
            let lone_surrogate = units.last().is_some_and(|u| (0xd800..0xdc00).contains(u));
            let mut s: String = char::decode_utf16(units)
                .map(|r| r.unwrap_or('\u{fffd}'))
                .collect();
            if body.len() % 2 == 1 && !lone_surrogate {
                s.push('\u{fffd}');
            }
            s
        }
        E::W1252 => body
            .iter()
            .map(|&b| match b {
                0x80..=0x9f => char::from_u32(
                    [
                        0x20AC, 0x81, 0x201A, 0x192, 0x201E, 0x2026, 0x2020, 0x2021, 0x2C6, 0x2030,
                        0x160, 0x2039, 0x152, 0x8D, 0x17D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C,
                        0x201D, 0x2022, 0x2013, 0x2014, 0x2DC, 0x2122, 0x161, 0x203A, 0x153, 0x9D,
                        0x17E, 0x178,
                    ][usize::from(b - 0x80)],
                )
                .unwrap_or('\u{fffd}'),
                b => char::from(b),
            })
            .collect(),
        E::Repl => {
            if body.is_empty() {
                String::new()
            } else {
                "\u{fffd}".to_owned()
            }
        }
    })
}
