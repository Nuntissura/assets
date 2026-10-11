//! Small deterministic generator (SplitMix64, no external crates) of mostly well-formed documents whose delimiters,
//! character references, raw-text bodies, namespaces and attributes are adversarial. The element vocabulary avoids tags
//! whose tree construction restructures well-nested markup (implied end tags, adoption agency, tables, forms), so a
//! disagreement is about how bytes are read, not about optional-tag inference.

use crate::corpus::Cases;

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next_u64() % (n as u64)).expect("below fits usize")
    }
    pub fn pct(&mut self, p: u64) -> bool {
        self.next_u64() % 100 < p
    }
    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

/// What the generated corpus leans towards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Mixed,
    Namespaces,
    RawText,
    CharRefs,
    DupAttrs,
    /// Adds NUL and CR to text and attribute values (a separate test: these are known to differ, see lane notes).
    Noise,
    /// Mixed documents with 1-3 random edits (deleted characters, inserted delimiter tokens, copied chunks): malformed
    /// markup that exercises the tokenizer's error paths.
    Fuzz,
    /// Hostile CSS in every carrier the asset gate reads (style attributes on HTML and SVG elements, HTML and SVG style
    /// bodies, SVG presentation attributes, SVG animation values of CSS and non-CSS targets): number/dot/exponent
    /// prefixes, escapes, comments, quotes, CDO/CDC, non-ASCII code points, CR/FF/CR LF, character references, URL
    /// functions, image-set/src/image/filter strings, @import, @namespace, substitution functions and `data:` payloads
    /// (CSS, base64, UTF-16, quirks-mode types, nested, documents). Compared by the gate-level `css-urls` check.
    Css,
}

const CSS_TOKENS: &[&str] = &[
    "1.",
    "+1.",
    "-1.",
    ".5",
    "1e3",
    "1e+",
    "1E-2",
    "2.5e",
    "1",
    "@",
    "#",
    "<!--",
    "-->",
    "<!-",
    "--",
    "-",
    "+",
    ".",
    "u+1f",
    "U+?",
    "\\75",
    "\\72 ",
    "\\69",
    "\\",
    "\\\n",
    "\\)",
    "\\'",
    "/*",
    "*/",
    "/**/",
    "'",
    "\"",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ";",
    ":",
    ",",
    "%",
    "!",
    "\u{a0}",
    "\u{80}",
    "\u{200b}",
    "é",
    "\u{b7}",
    "&#13;",
    "&#12;",
    "&quot;",
    "&amp;",
    "\r",
    "\r\n",
    "\u{c}",
    "a",
    "x",
    "red",
    "background",
    "1x",
    "2x",
    "--u",
];
const CSS_FUNCTIONS: &[&str] = &[
    "url(",
    "URL(",
    "u\\72l(",
    "\\75rl(",
    "url( ",
    "url(\"",
    "url('",
    "url(  '",
    "src(",
    "SRC(",
    "image-set(",
    "-webkit-image-set(",
    "image(",
    "cross-fade(",
    "type(",
    "var(",
    "attr(",
    "env(",
    "--f(",
    "if(",
    "linear-gradient(",
    "calc(",
    "rgb(",
    "x(",
    "-url(",
    "@import ",
    "@IMPORT",
    "@\\69mport ",
    "@media ",
    // R11: filter() strings are URLs; -moz- gradients and filter functions compute none; @namespace URLs load nothing.
    "filter(",
    "-webkit-filter(",
    "-moz-linear-gradient(",
    "blur(",
    "@namespace ",
    "@NAMESPACE svg ",
];
const CSS_URLS: &[&str] = &[
    "https://e.invalid/a.png",
    "//e.invalid/b",
    "c.png",
    "ht\\74p://e.invalid/x",
    "\\68ttp://e.invalid/y",
    "'https://e.invalid/s'",
    "\"//e.invalid/t\"",
    "'u.png'",
    "\"\"",
    "javascript:x",
    "data:x",
    "#frag",
    "d e",
    // data: payloads (R11-P1): CSS scanned recursively, quirks-mode style sheet types, documents denied.
    "'data:text/css,@import \"//e.invalid/dc.css\";'",
    "\"data:text/css,a{b:url(//e.invalid/dd)}\"",
    "data:text/css,%40import%20'//e.invalid/pc'",
    "'data:text/css;base64,QGltcG9ydCAnLy9lLmludmFsaWQvYic7'",
    "\"data:text/css;charset=utf-16le;base64,QABpAG0AcABvAHIAdAAgAHUAcgBsACgALwAvAGUALgBpAG4AdgBhAGwAaQBkAC8AdwApADsA\"",
    "\"data:text/plain,@import '//e.invalid/p';\"",
    "\"data:,@import url(//e.invalid/u)\"",
    "'DATA:TEXT/CSS ,@IMPORT url(x.css)'",
    "\"data:text/css,@import 'data:text/css,@import url(//e.invalid/n2)';\"",
    "'data:text/html,<b>x</b>'",
    "'data:image/svg+xml,<svg/>'",
    "\"data:text/css;charset=shift_jis,a\"",
];
const CSS_SPACE: &[&str] = &[" ", "  ", "\n", "\t"];
/// `attributeName` values of the SVG animation carrier: CSS targets of the carrier inventory (incl. the `marker`
/// shorthand, case and whitespace variants) and targets that are not CSS (compared as non-carriers).
const ANIMATED: &[&str] = &[
    "fill",
    "mask",
    "marker",
    "MARKER",
    "marker-end",
    "background-image",
    "clip-path",
    "filter",
    " stroke ",
    "style",
    "width",
    "href",
];
const PRESENTATION: &[&str] = &[
    "fill",
    "stroke",
    "mask",
    "filter",
    "cursor",
    "clip-path",
    "marker-start",
    "marker-mid",
    "marker-end",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Html,
    Svg,
    Math,
}

const CONTAINERS: &[&str] = &[
    "div", "span", "section", "b", "i", "em", "strong", "code", "small", "ul", "article", "nav",
    "center",
];
const VOIDS: &[&str] = &[
    "br", "img", "input", "hr", "wbr", "source", "track", "param", "embed", "meta", "link", "area",
    "base",
];
const RAW: &[&str] = &["script", "style", "xmp", "iframe", "noembed", "noframes"];
const RCDATA: &[&str] = &["textarea", "title"];
const SVG_TAGS: &[&str] = &[
    "g",
    "path",
    "rect",
    "circle",
    "text",
    "tspan",
    "style",
    "script",
    "title",
    "desc",
    "foreignObject",
    "a",
    "defs",
    "use",
    "image",
    "set",
    "animate",
    "clipPath",
    "symbol",
    "marker",
    "linearGradient",
    "stop",
    "svg",
    "math",
    "foreignobject",
    "DESC",
];
const MATH_TAGS: &[&str] = &[
    "mi",
    "mo",
    "mn",
    "ms",
    "mtext",
    "mrow",
    "mfrac",
    "annotation-xml",
    "semantics",
    "mglyph",
    "malignmark",
    "mstyle",
    "style",
    "svg",
    "none",
    "mpadded",
    "MI",
    "math",
];
const HTML_ATTRS: &[&str] = &[
    "id", "class", "title", "href", "src", "style", "data-x", "alt", "value", "lang", "tabindex",
    "name", "srcset", "ID", "Href", "SRC",
];
const SVG_ATTRS: &[&str] = &[
    "viewBox",
    "xlink:href",
    "xmlns",
    "xmlns:xlink",
    "xml:lang",
    "fill",
    "href",
    "id",
    "class",
    "style",
    "d",
    "transform",
    "XLINK:HREF",
];
const MATH_ATTRS: &[&str] = &[
    "definitionURL",
    "encoding",
    "display",
    "xlink:href",
    "id",
    "mathvariant",
];
const ENCODINGS: &[&str] = &[
    "text/html",
    "TEXT/HTML",
    "application/xhtml+xml",
    "Application/XHTML+XML",
    "text/plain",
    "",
    " text/html",
    "text/html ",
    "image/svg+xml",
];
const WORDS: &[&str] = &[
    "a", "bc", "x y", " ", "\n", "\t", "hello", "1", "é", "日本", "a-b", "_", "--", "!", "?", "/",
    ".",
];
const URLS: &[&str] = &[
    "//e.invalid/x",
    "http://e.invalid/",
    "javascript:x",
    "a/b.png",
    "#frag",
    "url(//e.invalid/x)",
    "j&#97;vascript:x",
    "//e.invalid/?a=1&b=2",
    "x?a=1&copy=2&amp=3",
];
const REFS: &[&str] = &[
    "&amp;",
    "&amp",
    "&AMP;",
    "&lt",
    "&lt;",
    "&gt;",
    "&quot",
    "&quot;",
    "&QUOT",
    "&apos;",
    "&nbsp",
    "&copy",
    "&copy;",
    "&not",
    "&notin;",
    "&notit;",
    "&not=",
    "&ampx",
    "&amp=",
    "&frac12",
    "&frac1",
    "&Aacute",
    "&aacutex",
    "&#65;",
    "&#65",
    "&#x41;",
    "&#X41",
    "&#128;",
    "&#x81;",
    "&#150",
    "&#xD800;",
    "&#x110000;",
    "&#99999999999;",
    "&#;",
    "&#x;",
    "&",
    "&&",
    "&;",
    "&unknown;",
    "&acE;",
    "&nGt;",
    "&fjlig;",
    "&NewLine;x",
    "&Tab;",
    "&#9;",
    "&#38;amp;",
    "&#0;",
    "&sol;",
    "&colon;",
];
const SEPARATORS: &[&str] = &[" ", " ", " ", "\n", "\t", "  ", " / ", "\u{c}"];
const FUZZ_TOKENS: &[&str] = &[
    "<",
    ">",
    "/",
    "!",
    "?",
    "--",
    "-->",
    "--!>",
    "<!--",
    "&",
    ";",
    "#",
    "\"",
    "'",
    "=",
    " ",
    "\n",
    "\t",
    "</",
    "/>",
    "<script>",
    "</script>",
    "</style>",
    "<style>",
    "<svg>",
    "</svg>",
    "<math>",
    "<mi>",
    "&quot",
    "&#x",
    "&#",
    "<![CDATA[",
    "]]>",
    "<b>",
    "</b>",
    "<title>",
    "<textarea>",
    "<!",
    "<?",
    "</ ",
    "=\"",
    "='",
    "\u{c}",
    "&amp",
    "&lt;",
    "<foreignObject>",
    "<desc>",
    "<annotation-xml encoding=text/html>",
];
const NOISE: &[&str] = &["\0", "\r\n", "\r", "a\0b", "\r\r\n"];

pub struct Generator {
    rng: Rng,
    theme: Theme,
}

impl Generator {
    pub fn new(seed: u64, theme: Theme) -> Self {
        Self {
            rng: Rng::new(seed),
            theme,
        }
    }

    pub fn document(&mut self) -> String {
        if self.theme == Theme::Css {
            return self.css_document();
        }
        let mut out = String::new();
        let items = 1 + self.rng.below(4);
        for _ in 0..items {
            self.item(&mut out, Ctx::Html, 0, false);
        }
        if self.theme == Theme::Fuzz {
            self.mutate(&mut out);
        }
        out
    }

    /// 2-16 CSS fragments (tokens, URL functions, URLs, whitespace).
    fn css_text(&mut self) -> String {
        let mut s = String::new();
        for _ in 0..(2 + self.rng.below(15)) {
            let r = self.rng.below(100);
            s.push_str(if r < 30 {
                self.rng.pick(CSS_FUNCTIONS)
            } else if r < 45 {
                self.rng.pick(CSS_URLS)
            } else if r < 88 {
                self.rng.pick(CSS_TOKENS)
            } else {
                self.rng.pick(CSS_SPACE)
            });
        }
        s
    }

    /// 1-3 CSS carriers. Attribute values escape `"` only (character references in the fragments decode in attributes and
    /// SVG text, never in an HTML style body); SVG style text escapes `<`; no fragment can spell `</style`.
    fn css_document(&mut self) -> String {
        let mut out = String::new();
        for _ in 0..(1 + self.rng.below(3)) {
            let css = self.css_text();
            let attr = css.replace('"', "&quot;");
            match self.rng.below(9) {
                0 | 1 => {
                    let tag = *self.rng.pick(&["div", "span", "b", "em", "section"]);
                    out.push_str(&format!("<{tag} style=\"{attr}\">x</{tag}>"));
                }
                // R11-F1/F2: SVG animation values (split on `;` by the engines) for CSS and non-CSS targets.
                7 | 8 => {
                    let target = *self.rng.pick(ANIMATED);
                    let el =
                        *self
                            .rng
                            .pick(&["animate", "set", "animateMotion", "animateTransform"]);
                    let which = *self
                        .rng
                        .pick(&["values", "values", "values", "to", "from", "by"]);
                    let second = self.css_text().replace('"', "&quot;");
                    out.push_str(&format!(
                        "<svg><rect><{el} attributeName=\"{target}\" {which}=\"{attr};{second}\"/></rect></svg>"
                    ));
                }
                2 => out.push_str(&format!("<style>{css}</style>")),
                3 => out.push_str(&format!(
                    "<style>.a{{{css}}} .b{{background:url(//e.invalid/after)}}</style>"
                )),
                4 => out.push_str(&format!(
                    "<svg><style>{}</style></svg>",
                    css.replace('<', "&lt;")
                )),
                5 => {
                    let name = *self.rng.pick(PRESENTATION);
                    let el = *self.rng.pick(&["rect", "g", "path"]);
                    out.push_str(&format!("<svg><{el} {name}=\"{attr}\"/></svg>"));
                }
                _ => out.push_str(&format!(
                    "<svg><g style=\"{attr}\"></g></svg><div style=\"a:b;{attr}\"></div>"
                )),
            }
        }
        out
    }

    fn mutate(&mut self, doc: &mut String) {
        let mut chars: Vec<char> = doc.chars().collect();
        for _ in 0..(1 + self.rng.below(3)) {
            if chars.is_empty() {
                break;
            }
            let at = self.rng.below(chars.len());
            match self.rng.below(4) {
                0 => {
                    let n = (1 + self.rng.below(3)).min(chars.len() - at);
                    chars.drain(at..at + n);
                }
                1 | 2 => {
                    let token: Vec<char> = self.rng.pick(FUZZ_TOKENS).chars().collect();
                    chars.splice(at..at, token);
                }
                _ => {
                    let n = (1 + self.rng.below(8)).min(chars.len() - at);
                    let chunk: Vec<char> = chars[at..at + n].to_vec();
                    let to = self.rng.below(chars.len() + 1);
                    chars.splice(to..to, chunk);
                }
            }
        }
        *doc = chars.into_iter().collect();
    }

    fn weighted(&mut self, weights: &[u32]) -> usize {
        let total: u32 = weights.iter().sum();
        let mut r = u32::try_from(self.rng.next_u64() % u64::from(total)).expect("below total");
        for (i, w) in weights.iter().enumerate() {
            if r < *w {
                return i;
            }
            r -= *w;
        }
        weights.len() - 1
    }

    fn item(&mut self, out: &mut String, ctx: Ctx, depth: usize, in_a: bool) {
        if out.len() > 480 {
            return;
        }
        let r = self.rng.below(100);
        if depth >= 3 {
            if r < 75 {
                self.text(out, ctx);
            } else {
                self.comment(out, ctx);
            }
            return;
        }
        match r {
            0..=21 => self.text(out, ctx),
            22..=27 => self.comment(out, ctx),
            28..=31 => self.bogus(out, ctx),
            _ => match ctx {
                Ctx::Html => self.html_element(out, depth, in_a),
                Ctx::Svg => self.svg_element(out, depth, in_a),
                Ctx::Math => self.math_element(out, depth, in_a),
            },
        }
    }

    fn items(&mut self, out: &mut String, ctx: Ctx, depth: usize, in_a: bool) {
        let n = self.rng.below(3);
        for _ in 0..n {
            self.item(out, ctx, depth, in_a);
        }
    }

    // ----- text, comments, bogus markup ------------------------------------------------------------------------------

    fn text(&mut self, out: &mut String, ctx: Ctx) {
        let pieces = 1 + self.rng.below(3);
        for _ in 0..pieces {
            let r = self.rng.below(100);
            let ref_rate = match self.theme {
                Theme::CharRefs => 55,
                Theme::Mixed | Theme::Noise | Theme::Fuzz => 25,
                _ => 12,
            };
            if r < ref_rate {
                out.push_str(self.rng.pick(REFS));
            } else if r < ref_rate + 12 {
                out.push_str(self.rng.pick(&[
                    "< ",
                    "<3",
                    "a<b",
                    "<=",
                    "<<",
                    "> ",
                    "a>b",
                    "</ ",
                    "<!",
                    "&lt;b&gt;",
                ]));
            } else if self.theme == Theme::Noise && r < ref_rate + 40 {
                out.push_str(self.rng.pick(NOISE));
            } else {
                out.push_str(self.rng.pick(WORDS));
            }
        }
        let noise = self.theme == Theme::Noise;
        if ctx != Ctx::Html && self.rng.pct(if noise { 30 } else { 8 }) {
            if noise {
                // CR, CR LF and U+0000 inside CDATA sections: the tokenizer preprocesses the input stream before the
                // CDATA state (a CR before `]]>` is an LF), so a CDATA piece and the text after it must not be joined raw
                // (R9 pre-review F1: an LF-led text after a CR-ended section).
                out.push_str(self.rng.pick(&[
                    "<![CDATA[\r]]>",
                    "<![CDATA[x\r]]>",
                    "<![CDATA[a\r\nb]]>",
                    "<![CDATA[\r\r\n]]>",
                    "<![CDATA[a\0b]]>",
                    "<![CDATA[\\\r]]>",
                    "<![CDATA[x<y]]>",
                ]));
                if self.rng.pct(50) {
                    out.push('\n');
                }
            } else {
                out.push_str(self.rng.pick(&[
                    "<![CDATA[x<y]]>",
                    "<![CDATA[]]>",
                    "<![CDATA[a&amp;b]]>",
                    "<![CDATA[x]]]]>",
                ]));
            }
        }
    }

    fn comment(&mut self, out: &mut String, ctx: Ctx) {
        let pool: &[&str] = &[
            "<!-- c -->",
            "<!---->",
            "<!-->",
            "<!--->",
            "<!-- a --!> b -->",
            "<!--a--!-->",
            "<!-- <b> -->",
            "<!-- -- -->",
            "<!--x-->",
            "<!-- a <!-- b -->",
            "<!--a--!>",
            "<!-- - -->",
        ];
        out.push_str(self.rng.pick(pool));
        if ctx == Ctx::Html && self.rng.pct(10) {
            out.push_str("-->");
        }
    }

    fn bogus(&mut self, out: &mut String, ctx: Ctx) {
        let html_pool: &[&str] = &[
            "<!x>",
            "<?pi?>",
            "<![CDATA[x]]>",
            "</ x>",
            "<!>",
            "</>",
            "<!doctype html>",
            "<? a > b ?>",
            "<![CDATA[x>y]]>",
        ];
        let foreign_pool: &[&str] = &["<![CDATA[x<y]]>", "<!x>", "<?pi?>", "</ x>", "<![CDATA[]]>"];
        out.push_str(self.rng.pick(if ctx == Ctx::Html {
            html_pool
        } else {
            foreign_pool
        }));
    }

    // ----- attributes -------------------------------------------------------------------------------------------------

    fn value(&mut self, quote: u8) -> String {
        let mut v = String::new();
        let pieces = 1 + self.rng.below(3);
        for _ in 0..pieces {
            let r = self.rng.below(100);
            let ref_rate = match self.theme {
                Theme::CharRefs => 55,
                Theme::Mixed | Theme::Noise | Theme::Fuzz => 25,
                _ => 12,
            };
            if r < ref_rate {
                v.push_str(self.rng.pick(REFS));
            } else if r < ref_rate + 20 {
                v.push_str(self.rng.pick(URLS));
            } else if self.theme == Theme::Noise && r < ref_rate + 45 {
                v.push_str(self.rng.pick(NOISE));
            } else {
                v.push_str(self.rng.pick(&[
                    "a", "b c", "=", "x=1", "'", "\"", "<", ">", "`", "&", "-", "a;", "1",
                ]));
            }
        }
        match quote {
            0 => v = v.replace('"', "&quot;"),
            1 => v = v.replace('\'', "&#39;"),
            _ => {
                v = v.replace(
                    |c: char| c == '>' || c.is_ascii_whitespace() || c == '\u{c}',
                    "_",
                );
                if v.is_empty() || v.starts_with(['"', '\'']) {
                    // An unquoted value cannot start with a quote (that would open a quoted value).
                    v.insert(0, 'v');
                }
            }
        }
        v
    }

    fn attrs(&mut self, out: &mut String, names: &[&str]) {
        let max = if self.theme == Theme::DupAttrs { 5 } else { 3 };
        let n = self.rng.below(max + 1);
        let mut used: Vec<&str> = Vec::new();
        for _ in 0..n {
            let name = if self.theme == Theme::DupAttrs && !used.is_empty() && self.rng.pct(60) {
                let base = *self.rng.pick(&used);
                match self.rng.below(3) {
                    0 => base.to_owned(),
                    1 => base.to_ascii_uppercase(),
                    _ => base.to_ascii_lowercase(),
                }
            } else {
                (*self.rng.pick(names)).to_owned()
            };
            used.push(
                names
                    .iter()
                    .find(|x| x.eq_ignore_ascii_case(&name))
                    .copied()
                    .unwrap_or(names[0]),
            );
            out.push_str(self.rng.pick(SEPARATORS));
            out.push_str(&name);
            match self.rng.below(4) {
                0 => {
                    let v = self.value(0);
                    out.push_str(&format!("=\"{v}\""));
                }
                1 => {
                    let v = self.value(1);
                    out.push_str(&format!("='{v}'"));
                }
                2 => {
                    let v = self.value(2);
                    out.push_str(&format!("={v}"));
                }
                _ => {}
            }
        }
    }

    // ----- end tags -------------------------------------------------------------------------------------------------

    fn close(&mut self, out: &mut String, name: &str) {
        out.push_str("</");
        match self.rng.below(100) {
            0..=69 => out.push_str(name),
            70..=84 => out.push_str(&name.to_ascii_uppercase()),
            _ => {
                for (i, c) in name.chars().enumerate() {
                    out.push(if i % 2 == 0 {
                        c.to_ascii_uppercase()
                    } else {
                        c.to_ascii_lowercase()
                    });
                }
            }
        }
        if self.rng.pct(15) {
            out.push_str(self.rng.pick(&[" ", "\t", "\n", "\u{c}", "  "]));
        }
        out.push('>');
    }

    // ----- HTML elements --------------------------------------------------------------------------------------------

    fn html_element(&mut self, out: &mut String, depth: usize, in_a: bool) {
        let weights: [u32; 9] = match self.theme {
            // container, void, raw, rcdata, noscript, svg, math, pre, a
            Theme::Mixed | Theme::Fuzz | Theme::Css => [30, 10, 18, 8, 4, 12, 8, 4, 4],
            Theme::Namespaces => [10, 3, 10, 3, 2, 36, 30, 2, 2],
            Theme::RawText => [10, 2, 40, 18, 8, 6, 4, 6, 0],
            Theme::CharRefs => [26, 14, 12, 16, 2, 10, 8, 4, 8],
            Theme::DupAttrs => [30, 18, 10, 6, 2, 14, 10, 2, 8],
            Theme::Noise => [26, 10, 16, 12, 4, 10, 8, 6, 4],
        };
        match self.weighted(&weights) {
            0 => {
                let name = *self.rng.pick(CONTAINERS);
                self.container(out, name, depth, in_a);
            }
            1 => {
                let name = *self.rng.pick(VOIDS);
                out.push('<');
                out.push_str(name);
                self.attrs(out, HTML_ATTRS);
                out.push_str(self.rng.pick(&[">", ">", "/>", " />"]));
            }
            2 => {
                let name = *self.rng.pick(RAW);
                self.raw_element(out, name);
            }
            3 => {
                let name = *self.rng.pick(RCDATA);
                self.raw_element(out, name);
            }
            4 => {
                out.push_str("<noscript");
                self.attrs(out, HTML_ATTRS);
                out.push('>');
                self.items(out, Ctx::Html, depth + 1, in_a);
                self.close(out, "noscript");
            }
            5 => self.svg_root(out, depth, in_a),
            6 => self.math_root(out, depth, in_a),
            7 => {
                let name = *self.rng.pick(&["pre", "listing"]);
                out.push('<');
                out.push_str(name);
                self.attrs(out, HTML_ATTRS);
                out.push('>');
                if matches!(self.theme, Theme::Mixed | Theme::Noise) && self.rng.pct(20) {
                    // `</>` emits no token (13.2.5.7), so a newline after it is still the first token and is dropped (R9
                    // pre-review F3); `</ x>` is a bogus comment token, after which the newline stays.
                    out.push_str(self.rng.pick(&["</>", "</>", "</></>", "</ x>"]));
                    out.push_str(self.rng.pick(&["\n", "\n", "\r\n", "&#10;", "\nx"]));
                }
                self.items(out, Ctx::Html, depth + 1, in_a);
                self.close(out, name);
            }
            _ => {
                if in_a {
                    self.container(out, "span", depth, true);
                } else {
                    self.container(out, "a", depth, true);
                }
            }
        }
    }

    fn container(&mut self, out: &mut String, name: &str, depth: usize, in_a: bool) {
        out.push('<');
        out.push_str(name);
        self.attrs(out, HTML_ATTRS);
        out.push('>');
        self.items(out, Ctx::Html, depth + 1, in_a || name == "a");
        self.close(out, name);
    }

    fn raw_element(&mut self, out: &mut String, name: &str) {
        out.push('<');
        out.push_str(name);
        self.attrs(out, HTML_ATTRS);
        out.push('>');
        let mut body = String::new();
        let pieces = self.rng.below(5);
        let escape_bias = if self.theme == Theme::RawText { 3 } else { 1 };
        for _ in 0..pieces {
            let r = self.rng.below(24 + escape_bias * 4);
            match r {
                0 => body.push_str("a b"),
                1 => body.push_str("x < y"),
                2 => body.push('<'),
                3 => body.push_str("</"),
                4 => body.push_str("<b>"),
                5 => body.push_str("</b>"),
                6 => body.push_str("<!--"),
                7 => body.push_str("-->"),
                8 => body.push_str("--!>"),
                9 => body.push_str("&amp;"),
                10 => body.push_str(&format!("&lt;/{name}&gt;")),
                11 => body.push_str("&#65;&notit;&copy"),
                12 => body.push('\n'),
                13 => body.push('\''),
                14 => body.push('"'),
                15 => body.push_str(&format!("</{name}x>")),
                16 => body.push_str(&format!("</{}", &name[..name.len() - 1])),
                17 => body.push_str(&format!("</ {name}>")),
                18 => body.push_str(&format!("</{name}")),
                19 => body.push_str("<script>"),
                20 => body.push_str("<SCRIPT "),
                21 => body.push_str("</script>"),
                22 => body.push_str("<style>"),
                23 => body.push_str("]]>"),
                // Escape-state biased pieces (script data), more likely under the RawText theme.
                24..=27 => body.push_str("<!--<script>"),
                28..=31 => body.push_str("</script >"),
                32..=35 => body.push_str("-->"),
                _ => body.push_str("<!--"),
            }
        }
        out.push_str(&body);
        if self.rng.pct(3) {
            // A close tag that cannot end the body (never an appropriate end tag).
            out.push_str(&format!("</{name}x>"));
        }
        self.close(out, name);
    }

    // ----- foreign elements -----------------------------------------------------------------------------------------

    fn svg_root(&mut self, out: &mut String, depth: usize, in_a: bool) {
        out.push_str("<svg");
        self.attrs(out, SVG_ATTRS);
        if self.rng.pct(15) {
            out.push_str("/>");
            return;
        }
        out.push('>');
        self.items(out, Ctx::Svg, depth + 1, in_a);
        self.close(out, "svg");
    }

    fn math_root(&mut self, out: &mut String, depth: usize, in_a: bool) {
        out.push_str("<math");
        self.attrs(out, MATH_ATTRS);
        if self.rng.pct(15) {
            out.push_str("/>");
            return;
        }
        out.push('>');
        self.items(out, Ctx::Math, depth + 1, in_a);
        self.close(out, "math");
    }

    fn svg_element(&mut self, out: &mut String, depth: usize, in_a: bool) {
        // Mostly foreign tags; a few HTML breakout start tags (the crate refuses these typed).
        if self.rng.pct(4) {
            let name = *self
                .rng
                .pick(&["b", "div", "span", "p", "img", "br", "em", "table"]);
            out.push_str(&format!("<{name}></{name}>"));
            return;
        }
        let name = *self.rng.pick(SVG_TAGS);
        out.push('<');
        out.push_str(name);
        self.attrs(out, SVG_ATTRS);
        if self.rng.pct(25) {
            out.push_str(self.rng.pick(&["/>", " />"]));
            return;
        }
        out.push('>');
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "foreignobject" | "desc" | "title" => self.items(out, Ctx::Html, depth + 1, in_a),
            "style" | "script" => {
                let n = 1 + self.rng.below(2);
                for _ in 0..n {
                    self.text(out, Ctx::Svg);
                }
            }
            _ => self.items(out, Ctx::Svg, depth + 1, in_a),
        }
        self.close(out, name);
    }

    fn math_element(&mut self, out: &mut String, depth: usize, in_a: bool) {
        if self.rng.pct(4) {
            let name = *self
                .rng
                .pick(&["b", "div", "span", "i", "img", "br", "code"]);
            out.push_str(&format!("<{name}></{name}>"));
            return;
        }
        let name = *self.rng.pick(MATH_TAGS);
        out.push('<');
        out.push_str(name);
        let attr_names: &[&str] = MATH_ATTRS;
        if name.eq_ignore_ascii_case("annotation-xml") && self.rng.pct(80) {
            out.push_str(&format!(" encoding=\"{}\"", self.rng.pick(ENCODINGS)));
        }
        self.attrs(out, attr_names);
        if self.rng.pct(25) {
            out.push_str(self.rng.pick(&["/>", " />"]));
            return;
        }
        out.push('>');
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "mi" | "mo" | "mn" | "ms" | "mtext" => {
                self.items(out, Ctx::Html, depth + 1, in_a);
                if self.rng.pct(25) {
                    out.push_str("<mglyph>");
                    self.items(out, Ctx::Math, depth + 1, in_a);
                    self.close(out, "mglyph");
                }
            }
            "annotation-xml" => {
                let ctx = if self.rng.pct(60) {
                    Ctx::Html
                } else {
                    Ctx::Math
                };
                self.items(out, ctx, depth + 1, in_a);
            }
            "svg" => self.items(out, Ctx::Svg, depth + 1, in_a),
            "style" => {
                let n = 1 + self.rng.below(2);
                for _ in 0..n {
                    self.text(out, Ctx::Math);
                }
            }
            _ => self.items(out, Ctx::Math, depth + 1, in_a),
        }
        self.close(out, name);
    }
}

/// `count` documents from consecutive seeds.
pub fn cases(theme: Theme, first_seed: u64, count: u64) -> Cases {
    (first_seed..first_seed.saturating_add(count))
        .map(|seed| {
            (
                format!("gen:{theme:?}:{seed}"),
                Generator::new(seed, theme).document(),
            )
        })
        .collect()
}
