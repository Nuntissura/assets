//! Tree-construction parity (HTML Standard 13.2.6.4, https://html.spec.whatwg.org/multipage/parsing.html#tree-construction).
//!
//! The crate keeps the authored tree. A browser builds the same tree except in two ways:
//!
//! - It inserts implied elements: `html`, `head` and `body` when their start tags are absent (13.2.6.4.2, .3, .6 "anything
//!   else"), `tbody` for a `tr` directly in a `table` (13.2.6.4.9 "td, th, tr"), `colgroup` for a `col` directly in a
//!   `table` (13.2.6.4.9 "col"). No authored element changes its parent or order; this is a documented normalisation
//!   (`WebDoc` docs), not a refusal. Whitespace-only text at the document skeleton level is the one non-element case:
//!   the "initial", "before html" and "before head" modes drop it, "after body" and "after after body" insert it into
//!   the body element (13.2.6.4.1-.3, .19, .22).
//! - It re-parents, reorders or drops authored nodes. Every such case is refused typed, by the detection rule below,
//!   before the node is added. The rule is conservative: where the HTML Standard and html5ever 0.40.1 (the differential
//!   oracle) use different element sets (scope: annotation-xml; special: keygen, search, isindex) the smaller boundary set
//!   is used, so a token either engine would restructure is refused.
//!
//! Detection rule (the crate already requires every non-void element to be closed by its own end tag, properly nested):
//!
//! | Context (crate current node) | Refused start tags / tokens | Spec step |
//! |---|---|---|
//! | any, HTML rules | `html`, `head`, `body` outside their skeleton position; `caption col colgroup frame tbody td tfoot th thead tr` | 13.2.6.4.7 (ignored) |
//! | any | DOCTYPE after the "initial" mode | 13.2.6.4.2-.22 (ignored) |
//! | `p` in button scope | `address article aside blockquote center details dialog dir div dl fieldset figcaption figure footer header hgroup main menu nav ol p search section summary ul h1-h6 pre listing form li dd dt plaintext table hr xmp` | 13.2.6.4.7 "close a p element" (table: both quirks readings) |
//! | current node `h1`-`h6` | `h1`-`h6` | 13.2.6.4.7 |
//! | open `form` | `form` | 13.2.6.4.7 (ignored) |
//! | `li` (or `dd`/`dt`) open below the nearest special element other than `address div p` | `li` (`dd`, `dt`) | 13.2.6.4.7 |
//! | `button` in scope | `button` | 13.2.6.4.7 |
//! | `a` open since the last marker (`applet marquee object td th caption template`) | `a` | 13.2.6.4.7, adoption agency |
//! | `nobr` in scope | `nobr` | 13.2.6.4.7, adoption agency |
//! | `select` in scope | `select`, `input`; `hr`, `option`, `optgroup` when the current node would be closed by "generate implied end tags" | 13.2.6.4.7 |
//! | current node `option` (no `select` in scope) | `option`, `optgroup` | 13.2.6.4.7 |
//! | `ruby` in scope and an implied-end-tag current node | `rb rtc` (`rp rt`, `rtc` exempt) | 13.2.6.4.7 |
//! | anywhere | `selectedcontent` (engines clone option content into it after parsing) | 4.10.7 |
//! | `table`, `tbody`, `thead`, `tfoot`, `tr` | everything except `caption colgroup col tbody thead tfoot tr` (in `table`), `tr` (in sections), `td th` (in `tr`), `style script template`, `input type=hidden`; non-whitespace text | 13.2.6.4.9-.15 (foster parenting, implied `tr`, closing table parts) |
//! | `colgroup` | everything except `col`, `template`, whitespace | 13.2.6.4.12 |
//! | `noscript` in `head` (scripting disabled) | everything except `basefont bgsound link meta noframes style`, whitespace, comments | 13.2.6.4.5 |
//! | authored `head` open | anything the "in head" mode does not insert into the head | 13.2.6.4.4 |
//! | after `</head>` | `base basefont bgsound link meta noframes script style template title` (moved into the head) | 13.2.6.4.6 |
//! | after `</body>` / `</html>` | anything but whitespace, comments and `</html>` (moved into the body) | 13.2.6.4.19, .22 |

#![deny(clippy::arithmetic_side_effects)]

use super::{Attr, Frame, Namespace, Parser, in_set, is_ws};
use crate::charref::{Context, decode};
use crate::error::{UnsupportedKind, WebError};
use std::ops::Range;

/// Skeleton insertion mode (13.2.6.4.1-13.2.6.4.6, .7, .19, .22) as far as the crate needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    AfterHead,
    InBody,
    AfterBody,
    AfterAfterBody,
}

/// A token as the skeleton modes see it.
#[derive(Clone, Copy)]
enum Tok<'s> {
    /// Whitespace-only character run.
    Space,
    /// Character run with a non-whitespace character (U+0000 included).
    Chars,
    Start(&'s str),
}

enum Flow {
    /// The token stays at the skeleton level (no body-content check applies).
    Done,
    /// The token is body content; the "in body" rules decide.
    Body,
}

/// Start tags the "in head" mode inserts into the head (13.2.6.4.4).
const HEAD_CONTENT: &[&str] = &[
    "base", "basefont", "bgsound", "link", "meta", "title", "noscript", "noframes", "style",
    "script", "template",
];
/// Start tags the "after head" mode moves into the head element (13.2.6.4.6).
const MOVED_INTO_HEAD: &[&str] = &[
    "base", "basefont", "bgsound", "link", "meta", "noframes", "script", "style", "template",
    "title",
];
/// Allowed in `noscript` inside `head` when scripting is disabled (13.2.6.4.5).
const HEAD_NOSCRIPT: &[&str] = &["basefont", "bgsound", "link", "meta", "noframes", "style"];
/// Start tags the "in body" mode ignores (13.2.6.4.7; `html`, `head`, `body` are handled by the skeleton).
const IGNORED_IN_BODY: &[&str] = &[
    "caption", "col", "colgroup", "frame", "head", "tbody", "td", "tfoot", "th", "thead", "tr",
];
/// Start tags that close a `p` element in button scope (13.2.6.4.7).
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "search",
    "section",
    "summary",
    "ul",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "pre",
    "listing",
    "form",
    "li",
    "dd",
    "dt",
    "plaintext",
    "table",
    "hr",
    "xmp",
];
const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
/// "Generate implied end tags" pops these (13.2.6.3).
const IMPLIED_END: &[&str] = &[
    "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc",
];
/// HTML scope boundaries of "has an element in scope" present in both the HTML Standard (13.2.4.3) and html5ever 0.40.1.
const SCOPE_HTML: &[&str] = &[
    "applet", "caption", "html", "table", "td", "th", "marquee", "object", "select", "template",
];
const SCOPE_MATHML: &[&str] = &["mi", "mo", "mn", "ms", "mtext"];
const SCOPE_SVG: &[&str] = &["foreignObject", "desc", "title"];
/// The "special" category (13.2.4.3) restricted to the HTML names both the Standard and html5ever list.
const SPECIAL: &[&str] = &[
    "address",
    "applet",
    "area",
    "article",
    "aside",
    "base",
    "basefont",
    "bgsound",
    "blockquote",
    "body",
    "br",
    "button",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "iframe",
    "img",
    "input",
    "li",
    "link",
    "listing",
    "main",
    "marquee",
    "menu",
    "meta",
    "nav",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "ol",
    "p",
    "param",
    "plaintext",
    "pre",
    "script",
    "section",
    "select",
    "source",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
    "wbr",
    "xmp",
];
/// Elements that put a marker on the list of active formatting elements (13.2.4.4).
const MARKERS: &[&str] = &[
    "applet", "marquee", "object", "td", "th", "caption", "template",
];
/// Current nodes of the table insertion modes whose character tokens go to "in table text" (13.2.6.4.9).
const TABLE_TEXT: &[&str] = &["table", "tbody", "thead", "tfoot", "tr"];
const SECTIONS: &[&str] = &["tbody", "thead", "tfoot"];
const IN_TABLE_HEAD: &[&str] = &["style", "script", "template"];

fn refuse(kind: UnsupportedKind, span: &Range<usize>) -> WebError {
    WebError::Unsupported {
        kind,
        span: span.clone(),
    }
}

impl Parser<'_> {
    /// `f` is an HTML element named one of `names`.
    fn html_is(&self, f: &Frame, names: &[&str]) -> bool {
        f.id != 0 && f.ns == Namespace::Html && in_set(names, self.name_of(f.id))
    }

    fn current_is(&self, names: &[&str]) -> bool {
        let top = self.top();
        self.html_is(&top, names)
    }

    fn open(&self, names: &[&str]) -> bool {
        self.stack.iter().any(|f| self.html_is(f, names))
    }

    /// "Has an element in scope" (13.2.4.3) with the shared boundary set plus `extra` (button scope: `button`).
    fn in_scope(&self, target: &[&str], extra: &[&str]) -> bool {
        for f in self.stack.iter().rev() {
            if self.html_is(f, target) {
                return true;
            }
            if f.id == 0 {
                return false;
            }
            let name = self.name_of(f.id);
            let boundary = match f.ns {
                Namespace::Html => in_set(SCOPE_HTML, name) || in_set(extra, name),
                Namespace::MathMl => in_set(SCOPE_MATHML, name),
                Namespace::Svg => in_set(SCOPE_SVG, name),
            };
            if boundary {
                return false;
            }
        }
        false
    }

    /// The `li` / `dd dt` start-tag loop (13.2.6.4.7): an element named one of `names` is reached before a special
    /// element other than `address`, `div`, `p`, so the browser closes it.
    fn list_item_would_close(&self, names: &[&str]) -> bool {
        for f in self.stack.iter().rev() {
            if f.id == 0 {
                return false;
            }
            if self.html_is(f, names) {
                return true;
            }
            if self.html_is(f, SPECIAL) && !self.html_is(f, &["address", "div", "p"]) {
                return false;
            }
        }
        false
    }

    /// An `a` (or other formatting element) on the list of active formatting elements after the last marker. In the
    /// accepted subset every formatting element is closed by its own end tag, so that list equals the open formatting
    /// elements above the nearest marker element.
    fn formatting_open_since_marker(&self, name: &str) -> bool {
        for f in self.stack.iter().rev() {
            if f.id == 0 {
                return false;
            }
            if self.html_is(f, &[name]) {
                return true;
            }
            if self.html_is(f, MARKERS) {
                return false;
            }
        }
        false
    }

    /// The crate's current node is the document root or an authored `html` / `head` element: tokens there are placed by
    /// the skeleton modes.
    pub(super) fn at_skeleton(&self) -> bool {
        let top = self.top();
        top.id == 0 || self.html_is(&top, &["html", "head"])
    }

    fn skeleton(&mut self, tok: Tok<'_>, span: &Range<usize>) -> Result<Flow, WebError> {
        use Mode::*;
        loop {
            match (self.mode, tok) {
                (Initial | BeforeHtml | BeforeHead | InHead | AfterHead, Tok::Space) => {
                    return Ok(Flow::Done);
                }
                (Initial, _) => self.mode = BeforeHtml,
                (BeforeHtml, Tok::Start(n)) if n.eq_ignore_ascii_case("html") => {
                    self.mode = BeforeHead;
                    return Ok(Flow::Done);
                }
                (BeforeHtml, _) => self.mode = BeforeHead,
                (BeforeHead, Tok::Start(n)) if n.eq_ignore_ascii_case("head") => {
                    self.mode = InHead;
                    return Ok(Flow::Done);
                }
                (BeforeHead, Tok::Start(n)) if n.eq_ignore_ascii_case("html") => {
                    return Err(refuse(UnsupportedKind::IgnoredToken, span));
                }
                (BeforeHead, _) => self.mode = InHead,
                (InHead, Tok::Start(n)) if in_set(HEAD_CONTENT, n) => return Ok(Flow::Done),
                (InHead, Tok::Start(n)) if in_set(&["html", "head"], n) => {
                    return Err(refuse(UnsupportedKind::IgnoredToken, span));
                }
                (InHead, _) => {
                    if self.current_is(&["head"]) {
                        // The "in head" mode pops the head and the token lands after it.
                        return Err(refuse(UnsupportedKind::DocumentStructure, span));
                    }
                    self.mode = AfterHead;
                }
                (AfterHead, Tok::Start(n)) if n.eq_ignore_ascii_case("body") => {
                    self.mode = InBody;
                    return Ok(Flow::Done);
                }
                (AfterHead, Tok::Start(n)) if in_set(&["html", "head"], n) => {
                    return Err(refuse(UnsupportedKind::IgnoredToken, span));
                }
                (AfterHead, Tok::Start(n)) if in_set(MOVED_INTO_HEAD, n) => {
                    return Err(refuse(UnsupportedKind::DocumentStructure, span));
                }
                (AfterHead, _) => self.mode = InBody,
                (InBody, Tok::Start(n)) if in_set(&["html", "head", "body"], n) => {
                    return Err(refuse(UnsupportedKind::IgnoredToken, span));
                }
                (InBody, _) => return Ok(Flow::Body),
                (AfterBody | AfterAfterBody, Tok::Space) => return Ok(Flow::Done),
                (AfterBody | AfterAfterBody, _) => {
                    // Parse error: back to "in body", so the token lands in the body element.
                    return Err(refuse(UnsupportedKind::DocumentStructure, span));
                }
            }
        }
    }

    /// Checks for a character run `span` before it becomes a text node under the current node.
    pub(super) fn tree_text(&mut self, span: &Range<usize>) -> Result<(), WebError> {
        let bytes = self.b.get(span.clone()).unwrap_or_default();
        let top = self.top();
        if self.at_skeleton() {
            let lead = bytes.iter().take_while(|&&c| is_ws(c)).count();
            if lead > 0 {
                self.skeleton(Tok::Space, span)?;
            }
            if lead < bytes.len() {
                self.skeleton(Tok::Chars, span)?;
            }
            return Ok(());
        }
        if top.head_noscript || self.html_is(&top, &["colgroup"]) {
            if bytes.iter().all(|&c| is_ws(c)) {
                return Ok(());
            }
            let kind = if top.head_noscript {
                UnsupportedKind::DocumentStructure
            } else {
                UnsupportedKind::TableContent
            };
            return Err(refuse(kind, span));
        }
        if self.html_is(&top, TABLE_TEXT) && !bytes.iter().all(|&c| is_ws(c) || c == 0) {
            // "in table text": non-whitespace characters are foster-parented before the table.
            return Err(refuse(UnsupportedKind::TableContent, span));
        }
        Ok(())
    }

    /// A DOCTYPE is a node only in the "initial" mode; everywhere else the tree builder ignores it.
    pub(super) fn tree_doctype(&mut self, span: &Range<usize>) -> Result<(), WebError> {
        if self.mode == Mode::Initial && self.top().id == 0 {
            self.mode = Mode::BeforeHtml;
            return Ok(());
        }
        Err(refuse(UnsupportedKind::IgnoredToken, span))
    }

    /// Checks for a start tag following the HTML rules (`name`: the name the tree builder sees, `image` already `img`).
    /// Returns true when the element is a `noscript` the "in head" mode inserts (scripting disabled: restricted content).
    pub(super) fn tree_start(
        &mut self,
        name: &str,
        attrs: &[Attr],
        span: &Range<usize>,
    ) -> Result<bool, WebError> {
        if self.at_skeleton() {
            if let Flow::Done = self.skeleton(Tok::Start(name), span)? {
                return Ok(self.mode == Mode::InHead && name.eq_ignore_ascii_case("noscript"));
            }
        } else if in_set(&["html", "head", "body"], name) {
            return Err(refuse(UnsupportedKind::IgnoredToken, span));
        }
        let top = self.top();
        if top.head_noscript {
            if in_set(HEAD_NOSCRIPT, name) {
                return Ok(false);
            }
            return Err(refuse(UnsupportedKind::DocumentStructure, span));
        }
        if self.html_is(&top, &["table"])
            || self.html_is(&top, SECTIONS)
            || self.html_is(&top, &["tr"])
        {
            return self.table_start(name, attrs, span).map(|()| false);
        }
        if self.html_is(&top, &["colgroup"]) {
            if in_set(&["col", "template"], name) {
                return Ok(false);
            }
            return Err(refuse(UnsupportedKind::TableContent, span));
        }
        self.body_start(name, span).map(|()| false)
    }

    /// 13.2.6.4.9 "in table", 13.2.6.4.13 "in table body", 13.2.6.4.14 "in row".
    fn table_start(&self, name: &str, attrs: &[Attr], span: &Range<usize>) -> Result<(), WebError> {
        let top = self.top();
        let allowed = if self.html_is(&top, &["table"]) {
            &[
                "caption", "colgroup", "col", "tbody", "thead", "tfoot", "tr",
            ][..]
        } else if self.html_is(&top, SECTIONS) {
            &["tr"][..]
        } else {
            &["td", "th"][..]
        };
        if in_set(allowed, name) || in_set(IN_TABLE_HEAD, name) {
            return Ok(());
        }
        if name.eq_ignore_ascii_case("input") {
            let hidden = attrs
                .iter()
                .find(|a| self.src[a.name_span.clone()].eq_ignore_ascii_case("type"))
                .and_then(|a| a.value_span.clone())
                .is_some_and(|v| {
                    decode(&self.src[v], Context::Attribute)
                        .text
                        .eq_ignore_ascii_case("hidden")
                });
            if hidden {
                return Ok(());
            }
        }
        Err(refuse(UnsupportedKind::TableContent, span))
    }

    /// 13.2.6.4.7 "in body" (also "in cell", "in caption" and template contents, whose table-part start tags are
    /// refused here as well).
    fn body_start(&self, name: &str, span: &Range<usize>) -> Result<(), WebError> {
        let implied = || refuse(UnsupportedKind::ImpliedEndTag, span);
        if in_set(IGNORED_IN_BODY, name) {
            let kind = if self.open(&["td", "th", "caption"]) {
                UnsupportedKind::TableContent
            } else {
                UnsupportedKind::IgnoredToken
            };
            return Err(refuse(kind, span));
        }
        if in_set(CLOSES_P, name) && self.in_scope(&["p"], &["button"]) {
            return Err(implied());
        }
        let select = self.in_scope(&["select"], &[]);
        let refused = match name.to_ascii_lowercase().as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.current_is(HEADINGS),
            "form" => {
                return if self.open(&["form"]) {
                    Err(refuse(UnsupportedKind::IgnoredToken, span))
                } else {
                    Ok(())
                };
            }
            "li" => self.list_item_would_close(&["li"]),
            "dd" | "dt" => self.list_item_would_close(&["dd", "dt"]),
            "button" => self.in_scope(&["button"], &[]),
            "a" | "nobr" => {
                let nested = if name.eq_ignore_ascii_case("a") {
                    self.formatting_open_since_marker("a")
                } else {
                    self.in_scope(&["nobr"], &[])
                };
                return if nested {
                    Err(refuse(UnsupportedKind::AdoptionAgency, span))
                } else {
                    Ok(())
                };
            }
            "select" | "input" => select,
            "hr" => select && self.current_is(IMPLIED_END),
            "option" => {
                if select {
                    self.current_is(IMPLIED_END) && !self.current_is(&["optgroup"])
                } else {
                    self.current_is(&["option"])
                }
            }
            "optgroup" => {
                if select {
                    self.current_is(IMPLIED_END)
                } else {
                    self.current_is(&["option"])
                }
            }
            "rb" | "rtc" => self.in_scope(&["ruby"], &[]) && self.current_is(IMPLIED_END),
            "rp" | "rt" => {
                self.in_scope(&["ruby"], &[])
                    && self.current_is(IMPLIED_END)
                    && !self.current_is(&["rtc"])
            }
            "selectedcontent" => return Err(refuse(UnsupportedKind::SelectContent, span)),
            _ => false,
        };
        if refused { Err(implied()) } else { Ok(()) }
    }

    /// Mode changes when the crate closes `closing` with its own end tag; content after `</body>` / `</html>` other than
    /// whitespace and comments is refused (it would be moved into the body).
    pub(super) fn tree_end(
        &mut self,
        closing: &Frame,
        span: &Range<usize>,
    ) -> Result<(), WebError> {
        let html = self.html_is(closing, &["html"]);
        match self.mode {
            Mode::AfterBody | Mode::AfterAfterBody if !html => {
                return Err(refuse(UnsupportedKind::DocumentStructure, span));
            }
            _ => {}
        }
        if html {
            self.mode = Mode::AfterAfterBody;
        } else if self.html_is(closing, &["head"]) {
            self.mode = Mode::AfterHead;
        } else if self.html_is(closing, &["body"]) {
            self.mode = Mode::AfterBody;
        }
        Ok(())
    }

    /// An end tag the tree builder ignores (`</image>`: no element ever has that name in HTML content) is a node only
    /// where it changes no insertion mode.
    pub(super) fn tree_ignored_end(&self, span: &Range<usize>) -> Result<(), WebError> {
        if matches!(self.mode, Mode::AfterBody | Mode::AfterAfterBody) {
            return Err(refuse(UnsupportedKind::DocumentStructure, span));
        }
        Ok(())
    }
}
