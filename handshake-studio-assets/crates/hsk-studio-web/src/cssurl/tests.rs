//! Tokenizer and URL-rule tests. One group per CSS Syntax 3 algorithm (each with escape, comment/decoy and
//! case/whitespace inputs, V4-08), the CC0 css-parsing-tests corpus (`tests/data/css-parsing-tests`, upstream
//! github.com/CourtBouillon/css-parsing-tests at 203ce36, file last changed in d03e4f4; expected component values written
//! independently of this crate for tinycss2 and rust-cssparser) and the URL rules of `css_references`.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "test code indexes its own fixed inputs; the module lint is for the untrusted-text decoder"
)]

use super::*;

/// Compact rendering of the token stream: `ident:x fn:x @x #id:x #u:x str:x bad-str url:x bad-url delim:x num:R/i|n
/// pct:R/i|n dim:R/i|n:unit urange:S-E ws <!-- --> : ; , [ ] ( ) { }` (`<eof>` after a string or url ended by EOF).
fn render(css: &str, non_ascii: NonAscii, unicode_ranges: bool) -> String {
    let mut t = Tokenizer::new(css, non_ascii, unicode_ranges);
    let mut out = Vec::new();
    let flag = |integer: bool| if integer { "i" } else { "n" };
    let eof = |eof: bool| if eof { "<eof>" } else { "" };
    while let Some(token) = t.next_token() {
        out.push(match token.kind {
            Kind::Ident(v) => format!("ident:{v}"),
            Kind::Function(v) => format!("fn:{v}"),
            Kind::AtKeyword(v) => format!("@{v}"),
            Kind::Hash { value, id } => format!("#{}:{value}", if id { "id" } else { "u" }),
            Kind::String { value, eof: e, .. } => format!("str:{value}{}", eof(e)),
            Kind::BadString { .. } => "bad-str".to_owned(),
            Kind::Url { value, eof: e, .. } => format!("url:{value}{}", eof(e)),
            Kind::BadUrl { .. } => "bad-url".to_owned(),
            Kind::Delim(c) => format!("delim:{c}"),
            Kind::Number { repr, integer } => format!("num:{}/{}", &css[repr], flag(integer)),
            Kind::Percentage { repr, integer } => format!("pct:{}/{}", &css[repr], flag(integer)),
            Kind::Dimension {
                repr,
                integer,
                unit,
            } => format!("dim:{}/{}:{unit}", &css[repr], flag(integer)),
            Kind::UnicodeRange { start, end } => format!("urange:{start:X}-{end:X}"),
            Kind::Whitespace => "ws".to_owned(),
            Kind::Cdo => "<!--".to_owned(),
            Kind::Cdc => "-->".to_owned(),
            Kind::Colon => ":".to_owned(),
            Kind::Semicolon => ";".to_owned(),
            Kind::Comma => ",".to_owned(),
            Kind::OpenSquare => "[".to_owned(),
            Kind::CloseSquare => "]".to_owned(),
            Kind::OpenParen => "(".to_owned(),
            Kind::CloseParen => ")".to_owned(),
            Kind::OpenCurly => "{".to_owned(),
            Kind::CloseCurly => "}".to_owned(),
        });
    }
    out.join(" ")
}

fn t(css: &str) -> String {
    render(css, NonAscii::Spec, false)
}

fn check(cases: &[(&str, &str)]) {
    for (css, want) in cases {
        assert_eq!(t(css), *want, "{css:?}");
    }
}

/// (kind, value) of every reference; `Url` kinds as `u`, `Bad` as `b`, `Computed` as `c`.
fn refs(css: &str) -> Vec<(char, String)> {
    css_references(css, usize::MAX)
        .expect("nesting within the limit")
        .into_iter()
        .map(|r| {
            let k = match r.kind {
                RefKind::Url => 'u',
                RefKind::Bad => 'b',
                RefKind::Computed => 'c',
            };
            (k, r.value)
        })
        .collect()
}

fn u(v: &str) -> (char, String) {
    ('u', v.to_owned())
}
fn b(v: &str) -> (char, String) {
    ('b', v.to_owned())
}
fn c(v: &str) -> (char, String) {
    ('c', v.to_owned())
}

// ----- 3.3 preprocessing ---------------------------------------------------------------------------------------------

#[test]
fn preprocessing_cr_ff_crlf_and_nul() {
    check(&[
        // CR, FF and CR LF are one newline each: a newline ends a string as a bad string (decoy: the url after it is real).
        ("'a\rb' x", "bad-str ws ident:b str: x<eof>"),
        ("'a\u{c}url(u)", "bad-str ws url:u"),
        ("'a\r\nurl(u)", "bad-str ws url:u"),
        // Escaped CR LF is ONE escaped newline (continuation); escaped CR followed by LF is too.
        ("'a\\\r\nb'", "str:ab"),
        ("'a\\\u{c}b'", "str:ab"),
        // A hex escape consumes one whitespace: CR LF is one.
        ("\\61\r\nb", "ident:ab"),
        ("\\61\r\n\r\nb", "ident:a ws ident:b"),
        // U+0000 is U+FFFD (an ident code point in both readings), also inside url and string tokens.
        ("a\0b", "ident:a\u{fffd}b"),
        ("url(\0)", "url:\u{fffd}"),
        ("'\0'", "str:\u{fffd}"),
    ]);
    // Spans stay source byte offsets through the filter (CR LF is one code point of two bytes).
    let mut tk = Tokenizer::new("a\r\n url( x\r\n)", NonAscii::Spec, false);
    assert_eq!(tk.next_token().map(|t| t.span), Some(0..1));
    let ws = tk.next_token().expect("whitespace");
    assert_eq!((ws.kind, ws.span), (Kind::Whitespace, 1..4));
    let url = tk.next_token().expect("url");
    assert!(
        matches!(url.kind, Kind::Url { ref inner, .. } if *inner == (9..10)),
        "{url:?}"
    );
    assert_eq!(url.span, 4..13);
}

// ----- 4.3.2 comments -------------------------------------------------------------------------------------------------

#[test]
fn comments_only_where_a_token_starts() {
    check(&[
        // Not inside a url token (R10-F1 decoy shape), a string or bad-url remnants.
        ("url(/*)x*/", "url:/* ident:x delim:* delim:/"),
        ("'/*'url(a)", "str:/* url:a"),
        ("url(a b/*)c", "bad-url ident:c"),
        // Between tokens, also inside a function's arguments; unterminated runs to EOF.
        ("a/**/b", "ident:a ident:b"),
        ("f(/*)*/x)", "fn:f ident:x )"),
        ("a /* url(x)", "ident:a ws"),
        // `url/**/(` is an ident and a ( (the comment splits the token); case does not matter for the url( check.
        ("url/**/(x)", "ident:url ( ident:x )"),
        ("UrL(x)", "url:x"),
        ("/*/*///** /* **/*//* ", "delim:/ delim:* delim:/"),
    ]);
}

// ----- 4.3.3 / 4.3.13 numeric tokens ----------------------------------------------------------------------------------

#[test]
fn numbers_take_a_dot_or_exponent_only_before_a_digit() {
    check(&[
        // R10-F1: a '.' after a number is a delim unless a digit follows; `url(` then starts a url token.
        ("1.url(/*)", "num:1/i delim:. url:/*"),
        ("+1.url(a)", "num:+1/i delim:. url:a"),
        ("-1.url(a)", "num:-1/i delim:. url:a"),
        (".5.url(a)", "num:.5/n delim:. url:a"),
        ("1.5.url(a)", "num:1.5/n delim:. url:a"),
        // Escaped: `\75` decodes to `u` after the delim.
        ("1.\\75rl(a)", "num:1/i delim:. url:a"),
        // A unit directly after the number is a dimension, never a url (`1url(` stays a dimension and a `(`).
        ("1url(a)", "dim:1/i:url ( ident:a )"),
        ("1\\75rl(a)", "dim:1/i:url ( ident:a )"),
        (".5url(a)", "dim:.5/n:url ( ident:a )"),
        // Exponent only before a digit (after an optional sign).
        ("1e3url(a)", "dim:1e3/n:url ( ident:a )"),
        ("1e+url(a)", "dim:1/i:e delim:+ url:a"),
        ("1e-url(a)", "dim:1/i:e-url ( ident:a )"),
        ("1E+2.url(a)", "num:1E+2/n delim:. url:a"),
        ("3e-2.1", "num:3e-2/n num:.1/n"),
        // Signs and dots that start nothing are delims.
        ("+.url(a)", "delim:+ delim:. url:a"),
        ("-.url(a)", "delim:- delim:. url:a"),
        ("1..2", "num:1/i delim:. num:.2/n"),
        ("12% 3\\%", "pct:12/i ws dim:3/i:%"),
    ]);
}

// ----- 4.3.4 ident-like tokens ----------------------------------------------------------------------------------------

#[test]
fn url_function_or_url_token() {
    check(&[
        // Quote right after `url(` or after whitespace: a function token (the whitespace is kept as one token).
        ("url(\"a\")", "fn:url str:a )"),
        ("url(   'a' )", "fn:url ws str:a ws )"),
        ("url(\n\t\"a\")", "fn:url ws str:a )"),
        // Otherwise a url token, whatever the case or escapes of the name.
        ("URL(  a  )", "url:a"),
        ("u\\72l(a)", "url:a"),
        ("\\75 rl(a)", "url:a"),
        // Not url: whitespace or comment before `(`, other names, escaped `(`.
        ("url (a)", "ident:url ws ( ident:a )"),
        ("xurl(a)", "fn:xurl ident:a )"),
        ("-url(a)", "fn:-url ident:a )"),
        ("url\\((a)", "fn:url( ident:a )"),
        ("--url(a)", "fn:--url ident:a )"),
    ]);
}

// ----- 4.3.5 strings --------------------------------------------------------------------------------------------------

#[test]
fn strings_escapes_newlines_and_eof() {
    check(&[
        ("'a\\'b' \"c\\\"d\"", "str:a'b ws str:c\"d"),
        ("'\\68 ttp'", "str:http"),
        ("'a\\\nb'", "str:ab"),
        ("'a\nb'", "bad-str ws ident:b str:<eof>"),
        ("'a", "str:a<eof>"),
        ("'a\\", "str:a<eof>"),
        // Comments, url( and the other quote are plain text inside a string.
        ("\"/* url(x) '\" url(y)", "str:/* url(x) ' ws url:y"),
    ]);
}

// ----- 4.3.6 / 4.3.15 url tokens --------------------------------------------------------------------------------------

#[test]
fn url_tokens_whitespace_quotes_and_remnants() {
    check(&[
        ("url(  a  )", "url:a"),
        ("url(a\n)", "url:a"),
        ("url(a", "url:a<eof>"),
        ("url(a  ", "url:a<eof>"),
        // Whitespace inside, a quote, `(` or a non-printable code point make a bad url, ended by `)` (escapes skipped).
        ("url(a b)c", "bad-url ident:c"),
        ("url(a\u{c}b)c", "bad-url ident:c"),
        ("url(a\"b)c", "bad-url ident:c"),
        ("url(a'b)c", "bad-url ident:c"),
        ("url(a(b)c", "bad-url ident:c"),
        ("url(a\u{1}b)c", "bad-url ident:c"),
        ("url(a\\\nb)c", "bad-url ident:c"),
        ("url(a b\\)c)d", "bad-url ident:d"),
        // Escapes inside a url token are decoded; an escaped ) or quote does not end or break it.
        ("url(a\\)b)", "url:a)b"),
        ("url(\\\"a)", "url:\"a"),
        ("url(ht\\74p://x)", "url:http://x"),
        ("url(a\\a b)", "url:a\nb"),
    ]);
}

// ----- 4.3.7 escaped code points --------------------------------------------------------------------------------------

#[test]
fn escaped_code_points() {
    check(&[
        ("\\31 23", "ident:123"),
        ("\\0000311", "ident:11"),
        ("\\000031 1", "ident:11"),
        ("\\0 a", "ident:\u{fffd}a"),
        ("\\d800 a", "ident:\u{fffd}a"),
        ("\\110000 a", "ident:\u{fffd}a"),
        ("\\10FFFF", "ident:\u{10ffff}"),
        ("\\", "ident:\u{fffd}"),
        ("a\\", "ident:a\u{fffd}"),
        ("\\\na", "delim:\\ ws ident:a"),
        ("\\G", "ident:G"),
    ]);
}

// ----- 4.3.1 hash, at-keyword, CDO/CDC, delims ------------------------------------------------------------------------

#[test]
fn hash_at_keyword_cdo_cdc_and_delims() {
    check(&[
        // A url( after # or @ is the hash/at-keyword name followed by a ( block (then a comment can open).
        ("#url(/*)*/x", "#id:url ( ident:x"),
        ("@url(/*)*/x", "@url ( ident:x"),
        ("#0a #-1 #\\31 x #", "#u:0a ws #u:-1 ws #id:1x ws delim:#"),
        (
            "@import @\\69mport @-x @1 @",
            "@import ws @import ws @-x ws delim:@ num:1/i ws delim:@",
        ),
        // CDO and CDC are tokens; `-->` is not an ident; `<!-` is not a CDO.
        ("<!--url(a)-->", "<!-- url:a -->"),
        ("-->url(a)", "--> url:a"),
        ("<!-x", "delim:< delim:! ident:-x"),
        ("--->", "ident:--- delim:>"),
        ("a-->", "ident:a-- delim:>"),
    ]);
}

// ----- 4.2 non-ASCII ident code points --------------------------------------------------------------------------------

#[test]
fn non_ascii_ident_code_points_spec_and_engines() {
    let both = |css: &str| {
        (
            render(css, NonAscii::Spec, false),
            render(css, NonAscii::Engines, false),
        )
    };
    // U+00A0 is no ident code point in the specification text but one for the engines.
    assert_eq!(
        both("x\u{a0}url(a)"),
        (
            "ident:x delim:\u{a0} url:a".to_owned(),
            "fn:x\u{a0}url ident:a )".to_owned()
        )
    );
    // Code points both readings accept: U+00B7, U+00C0, U+0300, U+10000, U+FFFD.
    for c in ['\u{b7}', '\u{c0}', '\u{300}', '\u{10000}', '\u{fffd}'] {
        let css = format!("a{c}url(x)");
        let want = format!("fn:a{c}url ident:x )");
        assert_eq!(both(&css), (want.clone(), want), "{c:?}");
    }
    // Code points only the engines accept: U+0080, U+00D7, U+037E, U+200B, U+FFFE.
    for c in ['\u{80}', '\u{d7}', '\u{37e}', '\u{200b}', '\u{fffe}'] {
        assert!(!spec_non_ascii_ident(c), "{c:?}");
        assert_eq!(
            render(&format!("{c}"), NonAscii::Engines, false),
            format!("ident:{c}")
        );
        assert_eq!(
            render(&format!("{c}"), NonAscii::Spec, false),
            format!("delim:{c}")
        );
    }
    // The gate reports the union: the url token the spec reading sees and nothing hidden by the engines' reading.
    assert_eq!(
        refs("x\u{a0}url(/*);b:url(https://e.invalid/a)"),
        [u("/*"), u("https://e.invalid/a")]
    );
}

// ----- 4.3.11 / 4.3.14 unicode-range ----------------------------------------------------------------------------------

#[test]
fn unicode_range_only_when_allowed() {
    let r = |css: &str| render(css, NonAscii::Spec, true);
    assert_eq!(
        r("u+1f-2a U+?? u+12345678"),
        "urange:1F-2A ws urange:0-FF ws urange:123456-123456 num:78/i"
    );
    assert_eq!(r("U+1?2"), "urange:10-1F num:2/i");
    // Without the flag (every place except the unicode-range descriptor, 5.5.11) `u+` is an ident and a number.
    assert_eq!(t("u+1f"), "ident:u dim:+1/i:f");
    // The url after a range: only the descriptor reading has a url token there, and that descriptor takes no URL.
    assert_eq!(r("u+fffurl(x)"), "urange:FFF-FFF url:x");
    assert_eq!(t("u+fffurl(x)"), "ident:u delim:+ fn:fffurl ident:x )");
}

// ----- the CC0 css-parsing-tests corpus -------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum J {
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Other,
}

struct JsonParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl JsonParser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|b| b.is_ascii_whitespace()) {
            self.i += 1;
        }
    }
    fn value(&mut self) -> J {
        self.ws();
        match self.s[self.i] {
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    if self.s[self.i] == b']' {
                        self.i += 1;
                        return J::Arr(items);
                    }
                    items.push(self.value());
                    self.ws();
                    if self.s[self.i] == b',' {
                        self.i += 1;
                    }
                }
            }
            b'"' => J::Str(self.string()),
            b't' | b'f' | b'n' => {
                while self.s.get(self.i).is_some_and(u8::is_ascii_alphabetic) {
                    self.i += 1;
                }
                J::Other
            }
            _ => {
                let start = self.i;
                while self.s.get(self.i).is_some_and(|b| {
                    b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E')
                }) {
                    self.i += 1;
                }
                J::Num(
                    std::str::from_utf8(&self.s[start..self.i])
                        .unwrap()
                        .parse()
                        .unwrap(),
                )
            }
        }
    }
    fn hex4(&mut self) -> u32 {
        let v = u32::from_str_radix(
            std::str::from_utf8(&self.s[self.i..self.i + 4]).unwrap(),
            16,
        )
        .unwrap();
        self.i += 4;
        v
    }
    fn string(&mut self) -> String {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let b = self.s[self.i];
            self.i += 1;
            match b {
                b'"' => return String::from_utf8(out).unwrap(),
                b'\\' => {
                    let e = self.s[self.i];
                    self.i += 1;
                    let c = match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let hi = self.hex4();
                            let cp = if (0xd800..0xdc00).contains(&hi) {
                                self.i += 2;
                                let lo = self.hex4();
                                0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00)
                            } else {
                                hi
                            };
                            char::from_u32(cp).unwrap()
                        }
                        other => char::from(other),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                other => out.push(other),
            }
        }
    }
}

fn s(v: &str) -> J {
    J::Str(v.to_owned())
}

fn numeric(name: &str, css: &str, repr: &Range<usize>, integer: bool) -> Vec<J> {
    let text = &css[repr.clone()];
    vec![
        s(name),
        s(text),
        J::Num(text.parse::<f64>().unwrap()),
        s(if integer { "integer" } else { "number" }),
    ]
}

/// CSS Syntax 3 5.4 consume a list of component values over the token stream, in the corpus's JSON shape.
fn component_values(css: &str) -> Vec<J> {
    let mut tk = Tokenizer::new(css, NonAscii::Engines, true);
    let mut tokens = Vec::new();
    while let Some(token) = tk.next_token() {
        tokens.push(token);
    }
    let mut i = 0;
    let mut out = Vec::new();
    while i < tokens.len() {
        component(css, &tokens, &mut i, None, &mut out);
    }
    out
}

/// Appends one component value (and the corpus's parse-error markers) at `tokens[*i]`; returns true when the token was
/// the `closer` of the enclosing block.
fn component(
    css: &str,
    tokens: &[Token],
    i: &mut usize,
    closer: Option<&Kind>,
    out: &mut Vec<J>,
) -> bool {
    let token = &tokens[*i];
    *i += 1;
    if closer == Some(&token.kind) {
        return true;
    }
    let block = |name: Vec<J>, close: Kind, i: &mut usize| {
        let mut items = name;
        while *i < tokens.len() && !component(css, tokens, i, Some(&close), &mut items) {}
        J::Arr(items)
    };
    let err = |what: &str| J::Arr(vec![s("error"), s(what)]);
    match &token.kind {
        Kind::Ident(v) => out.push(J::Arr(vec![s("ident"), s(v)])),
        Kind::AtKeyword(v) => out.push(J::Arr(vec![s("at-keyword"), s(v)])),
        Kind::Hash { value, id } => out.push(J::Arr(vec![
            s("hash"),
            s(value),
            s(if *id { "id" } else { "unrestricted" }),
        ])),
        Kind::String { value, eof, .. } => {
            out.push(J::Arr(vec![s("string"), s(value)]));
            if *eof {
                out.push(err("eof-in-string"));
            }
        }
        Kind::BadString { .. } => out.push(err("bad-string")),
        Kind::Url { value, eof, .. } => {
            out.push(J::Arr(vec![s("url"), s(value)]));
            if *eof {
                out.push(err("eof-in-url"));
            }
        }
        Kind::BadUrl { .. } => out.push(err("bad-url")),
        Kind::Delim(c) => out.push(J::Str(c.to_string())),
        Kind::Number { repr, integer } => out.push(J::Arr(numeric("number", css, repr, *integer))),
        Kind::Percentage { repr, integer } => {
            out.push(J::Arr(numeric("percentage", css, repr, *integer)))
        }
        Kind::Dimension {
            repr,
            integer,
            unit,
        } => {
            let mut v = numeric("dimension", css, repr, *integer);
            v.push(s(unit));
            out.push(J::Arr(v));
        }
        Kind::UnicodeRange { start, end } => out.push(J::Arr(vec![
            s("unicode-range"),
            J::Num(f64::from(*start)),
            J::Num(f64::from(*end)),
        ])),
        Kind::Whitespace => out.push(s(" ")),
        Kind::Cdo => out.push(s("<!--")),
        Kind::Cdc => out.push(s("-->")),
        Kind::Colon => out.push(s(":")),
        Kind::Semicolon => out.push(s(";")),
        Kind::Comma => out.push(s(",")),
        Kind::Function(name) => out.push(block(vec![s("function"), s(name)], Kind::CloseParen, i)),
        Kind::OpenParen => out.push(block(vec![s("()")], Kind::CloseParen, i)),
        Kind::OpenSquare => out.push(block(vec![s("[]")], Kind::CloseSquare, i)),
        Kind::OpenCurly => out.push(block(vec![s("{}")], Kind::CloseCurly, i)),
        // A closer that ends no open block is a preserved token (the corpus marks it as an error).
        Kind::CloseParen => out.push(err(")")),
        Kind::CloseSquare => out.push(err("]")),
        Kind::CloseCurly => out.push(err("}")),
    }
    false
}

/// The corpus's attribute-selector match tokens (`~=`, `|=`, `^=`, `$=`, `*=`, `||`) are rust-cssparser extensions; CSS
/// Syntax 3 has two delim tokens for each.
fn split_match_tokens(v: &[J]) -> Vec<J> {
    let mut out = Vec::new();
    for item in v {
        match item {
            J::Str(m) if matches!(m.as_str(), "~=" | "|=" | "^=" | "$=" | "*=" | "||") => {
                out.extend(m.chars().map(|c| J::Str(c.to_string())));
            }
            J::Arr(inner) => out.push(J::Arr(split_match_tokens(inner))),
            other => out.push(other.clone()),
        }
    }
    out
}

fn same(a: &J, b: &J) -> bool {
    match (a, b) {
        (J::Num(x), J::Num(y)) => (x - y).abs() <= 1e-9 * x.abs().max(1.0),
        (J::Arr(x), J::Arr(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        _ => a == b,
    }
}

/// Every case of css-parsing-tests `component_value_list.json` (CC0-1.0) reads exactly as expected. The corpus follows
/// the engines' non-ASCII reading (it predates the 2026 ident ranges) and tokenizes unicode-range tokens everywhere, so
/// it runs with `NonAscii::Engines` and unicode ranges allowed; the specification reading and the default flag are pinned
/// by the per-algorithm groups above.
#[test]
fn css_parsing_tests_component_value_list() {
    let data = include_str!("../../tests/data/css-parsing-tests/component_value_list.json");
    let J::Arr(items) = (JsonParser {
        s: data.as_bytes(),
        i: 0,
    })
    .value() else {
        panic!("corpus is a JSON array");
    };
    assert_eq!(items.len(), 100, "50 (input, expected) pairs");
    let mut failures = Vec::new();
    for pair in items.chunks(2) {
        let (J::Str(css), J::Arr(expected)) = (&pair[0], &pair[1]) else {
            panic!("pair shape {pair:?}");
        };
        let expected = J::Arr(split_match_tokens(expected));
        let actual = J::Arr(component_values(css));
        if !same(&actual, &expected) {
            failures.push(format!(
                "{css:?}\n  expected {expected:?}\n  actual   {actual:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ----- URL rules ------------------------------------------------------------------------------------------------------

#[test]
fn r10_f1_payloads_report_the_later_url() {
    for css in [
        "a:1.url(/*);background:url(https://e.invalid/p.png)",
        ".a{x:1.url(/*)}.b{background:url(https://e.invalid/p.png)}",
        "a:+1.url(/*);background:url(https://e.invalid/p.png)",
        "a:-1.url(/*);background:url(https://e.invalid/p.png)",
        "a:.5.url(/*);background:url(https://e.invalid/p.png)",
        "a:1.\\75rl(/*);background:url(https://e.invalid/p.png)",
        "a:1E2.URL( /* );background:url(https://e.invalid/p.png)",
    ] {
        let got = refs(css);
        assert_eq!(
            got.last(),
            Some(&u("https://e.invalid/p.png")),
            "{css:?}: {got:?}"
        );
    }
    // The unterminated-string decoy: `1.url("` is a url function whose string runs to the newline; the later url is real.
    assert_eq!(
        refs("a:1.url(\"/*\n;background:url(https://e.invalid/q.png)"),
        [b("/*"), u("https://e.invalid/q.png")]
    );
    // `1url(` stays a dimension: the parenthesis is a block and the comment inside it swallows the rest, as in a browser.
    assert!(refs("a:1url(/*);background:url(https://e.invalid/p.png)").is_empty());
}

#[test]
fn image_set_src_image_and_import_strings_are_urls() {
    assert_eq!(
        refs("b:image-set(\"//e.invalid/a.png\" 1x, 'b.png' 2x type(\"image/png\"))"),
        [u("//e.invalid/a.png"), u("b.png")]
    );
    assert_eq!(
        refs(
            "b:-WEBKIT-image-set('a.png' 1x);c:Image(ltr 'b.png', red);d:src(\"c.png\" cross-origin(anonymous))"
        ),
        [u("a.png"), u("b.png"), u("c.png")]
    );
    assert_eq!(
        refs("b:cross-fade('a.png' 50%, url(b.png));c:\\69mage-set('\\68ttp://e.invalid/x' 1x)"),
        [u("a.png"), u("b.png"), u("http://e.invalid/x")]
    );
    // Nested image-set inside image(); url() inside image-set(); strings in other functions and plain strings are not.
    assert_eq!(
        refs(
            "b:image(image-set('a.png' 1x));c:image-set(url('b.png') 1x);d:'c.png';e:format('woff');f:local('x')"
        ),
        [u("a.png"), u("b.png")]
    );
    // @import: string after whitespace or a comment, any case, escaped name; not after another token.
    assert_eq!(
        refs(
            "@import 'a.css';@IMPORT/**/\"b.css\";@\\69mport\n'c.css';@import url(d.css);@import x 'e.css';@media 'f'"
        ),
        [u("a.css"), u("b.css"), u("c.css"), u("d.css")]
    );
    // Bad strings where a URL string belongs are reported bad; empty URLs load nothing (CSS Values 4 4.5.2).
    assert_eq!(
        refs("b:image-set('//e.invalid/a\n 1x);@import \"x\n;c:url('');d:url();e:src(\"\")"),
        [b("//e.invalid/a"), b("x")]
    );
    // A string inside a ( block inside image-set is still under image-set (reported, conservatively).
    assert_eq!(refs("b:image-set(('a.png') 1x)"), [u("a.png")]);
}

#[test]
fn computed_urls_inside_url_functions_are_reported() {
    assert_eq!(
        refs("b:image-set(var(--u) 1x);c:src(var(--v, 'x.png'));d:image(attr(data-u))"),
        [c("var(--u)"), c("var(--v, 'x.png')"), c("attr(data-u)")]
    );
    assert_eq!(
        refs("b:image-set(--f('a.png') 1x, ENV(x) 2x);c:url('a.png' if(x))"),
        [c("--f('a.png')"), c("ENV(x)"), u("a.png"), c("if(x)")]
    );
    // Value functions do not compute a URL; var() outside URL functions is not a URL either; unclosed runs to the end.
    assert_eq!(
        refs(
            "b:image-set(linear-gradient(red, blue) 1x, rgb(0 0 0) calc(1x + 1x));c:var(--x);d:image-set(var(--y"
        ),
        [c("var(--y")]
    );
}

#[test]
fn bad_urls_are_reported_and_spans_locate_values() {
    let css = "a:url( x y );b:url( \"q\" );c:u\\72l(  ht\\74p://e.invalid/z  )";
    let at = |needle: &str| css.find(needle).expect("needle");
    let found = css_references(css, 10).unwrap();
    let got: Vec<(RefKind, &str, Range<usize>)> = found
        .iter()
        .map(|r| (r.kind, r.value.as_str(), r.span.clone()))
        .collect();
    assert_eq!(
        got,
        [
            (RefKind::Bad, "x", at("x y")..at("x y") + 1),
            (RefKind::Url, "q", at("q\"")..at("q\"") + 1),
            (
                RefKind::Url,
                "http://e.invalid/z",
                at("ht\\74p")..at("z  )") + 1
            ),
        ]
    );
}

#[test]
fn nesting_beyond_the_limit_is_refused() {
    let deep = "(".repeat(MAX_NESTING) + "url(x)";
    assert!(css_references(&deep, 10).is_ok());
    let deeper = "[".repeat(MAX_NESTING + 1) + "url(x)";
    assert_eq!(
        css_references(&deeper, 10).err(),
        Some(MAX_NESTING..MAX_NESTING + 1)
    );
    // Mismatched closers do not end a block (5.4): `image-set(]` still scans its strings.
    assert_eq!(refs("b:image-set(] 'a.png' } ) 'b.png'"), [u("a.png")]);
}

/// V4-02: the output never exceeds `limit` (the caller passes the room left under `max_nodes` plus one and refuses typed
/// beyond it), on the single-reading path and on the union path of both non-ASCII readings.
#[test]
fn output_is_bounded_by_the_limit() {
    let many = "url(a) ".repeat(50);
    assert_eq!(css_references(&many, 7).unwrap().len(), 7);
    let union = format!("x\u{a0}url(b) {many}");
    let found = css_references(&union, 7).unwrap();
    assert_eq!(found.len(), 7);
    assert_eq!(found[0].value, "b");
    assert_eq!(css_references(&many, 0).unwrap().len(), 0);
}
