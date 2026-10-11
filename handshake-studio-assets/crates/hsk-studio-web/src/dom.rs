//! Span-preserving scanner for a documented HTML subset and the DOM projection over one source revision.
//!
//! Authority is the source text (STU-WEB-003): the tree is a projection whose every node keeps its exact
//! byte span. Nothing here executes script, fetches assets or performs I/O.
//!
//! Subset: elements with explicit end tags, attributes (double/single/unquoted/valueless), text, comments,
//! doctype, void elements, raw-text (`script style iframe noembed noframes xmp`) and RCDATA
//! (`textarea title`) bodies kept as one opaque `Raw` child, `svg`/`math` content parsed with the same
//! grammar plus self-closing tags. Unsupported tokens (CDATA, processing instructions, bogus markup) stay
//! as opaque `Unsupported` nodes with a loss entry. Optional end tags, misnesting and unterminated
//! constructs are typed errors; the tree is never guessed or re-nested.
//!
//! Inside the subset the scanner reads bytes the way an HTML parser does (HTML Standard 13.2): the comment end
//! states (`-->`, `--!>`), script data with its escaped and double-escaped states, RAWTEXT/RCDATA end tags, CDATA
//! sections only where the current node is not an HTML element (elsewhere a bogus comment ending at the first `>`),
//! and the tree-construction dispatcher that decides which rules a start tag follows: every element carries its
//! namespace; HTML integration points (SVG `foreignObject desc title`, MathML `annotation-xml` with an HTML
//! `encoding`) and MathML text integration points (`mi mo mn ms mtext`) switch back to HTML rules, so a `style`
//! there is RAWTEXT; inside foreign content a start tag takes the namespace of its parent. Where engines or the
//! scripting flag would read the same bytes differently the document is refused typed instead of picking one
//! reading: HTML start tags that break out of foreign content, a `noscript` whose RAWTEXT reading ends elsewhere,
//! elements read as raw text or ignored inside `select`, and `frameset` documents.

#![deny(clippy::arithmetic_side_effects)]

use crate::charref::{Context, decode};
use crate::error::{LimitKind, LossKind, LossReport, MalformedKind, UnsupportedKind, WebError};
use hsk_studio_accord::CancellationToken;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::ops::Range;

pub type NodeId = usize;

pub(crate) const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
pub(crate) const RAWTEXT: &[&str] = &["script", "style", "iframe", "noembed", "noframes", "xmp"];
const RCDATA: &[&str] = &["textarea", "title"];
/// Elements whose end tag HTML may omit; seeing one unclosed means the document needs HTML5 tree building.
const OPTIONAL_END: &[&str] = &[
    "li", "p", "dt", "dd", "tr", "td", "th", "thead", "tbody", "tfoot", "option", "optgroup",
    "colgroup", "caption", "head", "body", "html", "rp", "rt",
];
/// STU-WEB-020 protected contents: a formatter must keep these verbatim.
const PROTECTED: &[&str] = &["pre", "script", "style", "textarea"];
/// HTML start tags that pop foreign content (HTML Standard 13.2.6.5); `font` only with `color`, `face` or `size`.
const BREAKOUT: &[&str] = &[
    "b",
    "big",
    "blockquote",
    "body",
    "br",
    "center",
    "code",
    "dd",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "hr",
    "i",
    "img",
    "li",
    "listing",
    "menu",
    "meta",
    "nobr",
    "ol",
    "p",
    "pre",
    "ruby",
    "s",
    "small",
    "span",
    "strong",
    "strike",
    "sub",
    "sup",
    "table",
    "tt",
    "u",
    "ul",
    "var",
];
/// Read as raw text by parsers following the current `select` content model, ignored (so their content becomes markup)
/// by the older "in select" insertion mode: refused inside `select`.
const SELECT_DIVERGENT: &[&str] = &[
    "style", "xmp", "iframe", "noembed", "noframes", "noscript", "title", "svg", "math",
];

/// Namespace an HTML parser gives an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Namespace {
    Html,
    Svg,
    MathMl,
}

/// One open element with the parser state its namespace and position imply.
#[derive(Clone, Copy)]
struct Frame {
    id: NodeId,
    ns: Namespace,
    /// SVG `foreignObject desc title`, MathML `annotation-xml` with an HTML `encoding`.
    html_ip: bool,
    /// MathML `mi mo mn ms mtext`.
    mathml_text_ip: bool,
    /// MathML `annotation-xml` (an `svg` start tag below it follows the HTML rules).
    annotation_xml: bool,
    /// HTML `noscript`: start of its RAWTEXT reading's close tag (scripting enabled).
    noscript_close: Option<usize>,
}

pub(crate) fn in_set(set: &[&str], name: &str) -> bool {
    set.iter().any(|s| name.eq_ignore_ascii_case(s))
}

pub(crate) fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_attributes: usize,
    pub max_edits: usize,
    /// CSS rules and `@media` contexts considered by the preview.
    pub max_rules: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 8 << 20,
            max_nodes: 200_000,
            max_depth: 128,
            max_attributes: 256,
            max_edits: 1024,
            max_rules: 2048,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Document,
    Element,
    Text,
    Comment,
    Doctype,
    /// Body of a raw-text or RCDATA element; opaque, never tokenized.
    Raw,
    /// Unsupported construct kept byte-exact; never patched.
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentModel {
    Normal,
    /// Void or self-closed: no content, no close tag.
    Empty,
    RawText,
    Rcdata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quote {
    Double,
    Single,
    Unquoted,
    /// Valueless attribute (`disabled`).
    Absent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attr {
    /// Whole attribute from the name start to the end of the value (closing quote included).
    pub span: Range<usize>,
    pub name_span: Range<usize>,
    /// Value bytes without quotes; `None` for a valueless attribute.
    pub value_span: Option<Range<usize>>,
    pub quote: Quote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    /// The whole node: open tag, children and close tag for elements.
    pub span: Range<usize>,
    pub name_span: Option<Range<usize>>,
    pub open_span: Option<Range<usize>>,
    pub close_span: Option<Range<usize>>,
    pub attrs: Vec<Attr>,
    pub children: Vec<NodeId>,
    pub parent: Option<NodeId>,
    /// Position among the parent's children (all node kinds count).
    pub index: usize,
    pub self_closing: bool,
    pub content: ContentModel,
    pub protected: bool,
    /// Namespace of an element as an HTML parser assigns it; `Html` for every other node kind.
    pub namespace: Namespace,
}
impl Node {
    fn new(kind: NodeKind, span: Range<usize>) -> Self {
        Self {
            kind,
            span,
            name_span: None,
            open_span: None,
            close_span: None,
            attrs: Vec::new(),
            children: Vec::new(),
            parent: None,
            index: 0,
            self_closing: false,
            content: ContentModel::Normal,
            protected: false,
            namespace: Namespace::Html,
        }
    }
    /// Bytes between the open and close tag; `None` for void, self-closed and non-element nodes.
    pub fn content_span(&self) -> Option<Range<usize>> {
        Some(self.open_span.as_ref()?.end..self.close_span.as_ref()?.start)
    }
}

/// Stable path of a node within one revision: `/<index>:<label>/...`, label = lower-cased tag name or
/// `#text #comment #doctype #raw #unsupported`; the document root is `/`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey(String);
impl NodeKey {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for NodeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewRow {
    pub id: NodeId,
    pub depth: usize,
    pub kind: NodeKind,
    pub tag: Option<String>,
    /// Decoded attribute values; `None` for valueless attributes.
    pub attrs: Vec<(String, Option<String>)>,
    /// Decoded text for text nodes, raw text for raw nodes, comment body for comments.
    pub text: Option<String>,
    pub span: Range<usize>,
}

/// One source revision, its projected tree and the findings made while projecting it.
#[derive(Clone, Debug)]
pub struct WebDoc {
    revision: u64,
    source: String,
    sha256_hex: String,
    nodes: Vec<Node>,
    loss: LossReport,
    limits: Limits,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 15) as usize] as char);
    }
    out
}

/// Parse `src` as revision `revision`. Bounded by `limits`; cancellation is checked between chunks.
pub fn parse(
    src: &str,
    revision: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<WebDoc, WebError> {
    cancel.check()?;
    if src.len() > limits.max_bytes {
        return Err(WebError::LimitExceeded {
            limit: LimitKind::Bytes,
            span: limits.max_bytes..src.len(),
        });
    }
    let mut root = Node::new(NodeKind::Document, 0..src.len());
    root.content = ContentModel::Normal;
    let mut parser = Parser {
        src,
        b: src.as_bytes(),
        pos: 0,
        nodes: vec![root],
        stack: vec![Frame {
            id: 0,
            ns: Namespace::Html,
            html_ip: false,
            mathml_text_ip: false,
            annotation_xml: false,
            noscript_close: None,
        }],
        selects: 0,
        loss: LossReport::default(),
        limits: *limits,
        cancel,
        ticks: 0,
    };
    parser.run()?;
    Ok(WebDoc {
        revision,
        sha256_hex: sha256_hex(src.as_bytes()),
        source: src.to_owned(),
        nodes: parser.nodes,
        loss: parser.loss,
        limits: *limits,
    })
}

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    nodes: Vec<Node>,
    stack: Vec<Frame>,
    /// Open HTML `select` elements.
    selects: usize,
    loss: LossReport,
    limits: Limits,
    cancel: &'a CancellationToken,
    ticks: u32,
}

pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `i + n`, saturating (positions never exceed the input length, which is far below `usize::MAX`).
const fn at(i: usize, n: usize) -> usize {
    i.saturating_add(n)
}

/// End of a comment whose `<!--` starts at `start` (comment start, end and end-bang states): `<!-->`, `<!--->`, or the
/// first `--` followed by more `-`, then `>` or `!>`.
fn comment_end(b: &[u8], start: usize) -> Option<usize> {
    let body = at(start, 4);
    match b.get(body) {
        Some(b'>') => return Some(at(body, 1)),
        Some(b'-') if b.get(at(body, 1)) == Some(&b'>') => return Some(at(body, 2)),
        _ => {}
    }
    let mut i = body;
    while let Some(off) = find(b.get(i..)?, b"--") {
        let mut q = at(at(i, off), 2);
        while b.get(q) == Some(&b'-') {
            q = at(q, 1);
        }
        match b.get(q) {
            Some(b'>') => return Some(at(q, 1)),
            Some(b'!') if b.get(at(q, 1)) == Some(&b'>') => return Some(at(q, 2)),
            _ => i = at(at(i, off), 1),
        }
    }
    None
}

/// Length of the ASCII-alpha run at `i` and whether it equals `name` (ASCII case-insensitive) and is followed by
/// whitespace, `/` or `>` (an appropriate end tag, or the double-escape keyword).
fn keyword_at(b: &[u8], i: usize, name: &[u8]) -> (usize, bool) {
    let run = b.get(i..).map_or(0, |r| {
        r.iter().take_while(|c| c.is_ascii_alphabetic()).count()
    });
    let hit = run == name.len()
        && b.get(i..at(i, run))
            .is_some_and(|w| w.eq_ignore_ascii_case(name))
        && b.get(at(i, run))
            .is_some_and(|&c| is_ws(c) || c == b'/' || c == b'>');
    (run, hit)
}

fn tag_terminator(b: &[u8], i: usize) -> bool {
    b.get(i)
        .is_some_and(|&t| is_ws(t) || t == b'/' || t == b'>')
}

/// Start of the `</script` that ends script data begun at `from` (HTML Standard 13.2.5.4-13.2.5.31: escaped `<!--`
/// sections and double-escaped `<script>` inside them, where `</script>` does not end the element).
pub(crate) fn find_script_close(b: &[u8], from: usize) -> Option<usize> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum S {
        Data,
        Escaped,
        EscapedDash,
        EscapedDashDash,
        Double,
        DoubleDash,
        DoubleDashDash,
    }
    const SCRIPT: &[u8] = b"script";
    let mut s = S::Data;
    let mut i = from;
    while let Some(&c) = b.get(i) {
        match s {
            S::Data => {
                if c == b'<' {
                    if b.get(at(i, 1)) == Some(&b'/') {
                        let (run, hit) = keyword_at(b, at(i, 2), SCRIPT);
                        if hit {
                            return Some(i);
                        }
                        i = at(at(i, 2), run);
                        continue;
                    }
                    if b.get(at(i, 1)..at(i, 4)) == Some(b"!--".as_slice()) {
                        s = S::EscapedDashDash;
                        i = at(i, 4);
                        continue;
                    }
                }
                i = at(i, 1);
            }
            S::Escaped | S::EscapedDash | S::EscapedDashDash => match c {
                b'-' => {
                    s = if s == S::Escaped {
                        S::EscapedDash
                    } else {
                        S::EscapedDashDash
                    };
                    i = at(i, 1);
                }
                b'<' => {
                    s = S::Escaped;
                    match b.get(at(i, 1)) {
                        Some(b'/') => {
                            let (run, hit) = keyword_at(b, at(i, 2), SCRIPT);
                            if hit {
                                return Some(i);
                            }
                            i = at(at(i, 2), run);
                        }
                        Some(a) if a.is_ascii_alphabetic() => {
                            // Double escape start: the run and its terminator are consumed either way.
                            let (run, hit) = keyword_at(b, at(i, 1), SCRIPT);
                            let terminated = tag_terminator(b, at(at(i, 1), run));
                            if hit {
                                s = S::Double;
                            }
                            i = at(at(at(i, 1), run), usize::from(terminated));
                        }
                        _ => i = at(i, 1),
                    }
                }
                b'>' if s == S::EscapedDashDash => {
                    s = S::Data;
                    i = at(i, 1);
                }
                _ => {
                    s = S::Escaped;
                    i = at(i, 1);
                }
            },
            S::Double | S::DoubleDash | S::DoubleDashDash => match c {
                b'-' => {
                    s = if s == S::Double {
                        S::DoubleDash
                    } else {
                        S::DoubleDashDash
                    };
                    i = at(i, 1);
                }
                b'<' => {
                    s = S::Double;
                    if b.get(at(i, 1)) == Some(&b'/') {
                        // Double escape end: `</script` + terminator returns to the escaped state.
                        let (run, hit) = keyword_at(b, at(i, 2), SCRIPT);
                        let terminated = tag_terminator(b, at(at(i, 2), run));
                        if hit {
                            s = S::Escaped;
                        }
                        i = at(at(at(i, 2), run), usize::from(terminated));
                    } else {
                        i = at(i, 1);
                    }
                }
                b'>' if s == S::DoubleDashDash => {
                    s = S::Data;
                    i = at(i, 1);
                }
                _ => {
                    s = S::Double;
                    i = at(i, 1);
                }
            },
        }
    }
    None
}

/// Start of the close tag of raw text named `name` begun at `from`: script data rules for `script`, the RAWTEXT /
/// RCDATA appropriate end tag otherwise.
pub(crate) fn find_raw_text_close(b: &[u8], from: usize, name: &str) -> Option<usize> {
    if name.eq_ignore_ascii_case("script") {
        find_script_close(b, from)
    } else {
        find_raw_close(b, from, name.as_bytes())
    }
}

fn is_html_integration_point(ns: Namespace, name: &str, encoding: Option<&str>) -> bool {
    match ns {
        Namespace::Svg => ["foreignObject", "desc", "title"]
            .iter()
            .any(|n| name.eq_ignore_ascii_case(n)),
        Namespace::MathMl => {
            name.eq_ignore_ascii_case("annotation-xml")
                && encoding.is_some_and(|e| {
                    e.eq_ignore_ascii_case("text/html")
                        || e.eq_ignore_ascii_case("application/xhtml+xml")
                })
        }
        Namespace::Html => false,
    }
}

fn markup_start(next: Option<u8>) -> bool {
    matches!(next, Some(c) if c.is_ascii_alphabetic() || matches!(c, b'/' | b'!' | b'?'))
}

/// Position of `</name` (ASCII case-insensitive) followed by whitespace, `/` or `>` (RAWTEXT/RCDATA appropriate end tag).
fn find_raw_close(b: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let mut i = from;
    while let Some(off) = b.get(i..)?.iter().position(|&c| c == b'<') {
        let p = at(i, off);
        if b.get(at(p, 1)) == Some(&b'/') {
            let (_, hit) = keyword_at(b, at(p, 2), name);
            if hit {
                return Some(p);
            }
        }
        i = at(p, 1);
    }
    None
}

impl<'a> Parser<'a> {
    fn tick(&mut self) -> Result<(), WebError> {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks & 0x3ff == 0 {
            self.cancel.check()?;
        }
        Ok(())
    }

    fn char_end(&self, i: usize) -> usize {
        self.src[i..]
            .chars()
            .next()
            .map_or(at(i, 1), |c| at(i, c.len_utf8()))
    }

    fn push_under(&mut self, parent: NodeId, mut node: Node) -> Result<NodeId, WebError> {
        if self.nodes.len() >= self.limits.max_nodes {
            return Err(WebError::LimitExceeded {
                limit: LimitKind::Nodes,
                span: node.span,
            });
        }
        let id = self.nodes.len();
        node.parent = Some(parent);
        node.index = self.nodes[parent].children.len();
        self.nodes[parent].children.push(id);
        self.nodes.push(node);
        Ok(id)
    }

    fn push(&mut self, node: Node) -> Result<NodeId, WebError> {
        let parent = self.top().id;
        self.push_under(parent, node)
    }

    /// The current node (adjusted current node: no fragment parsing here).
    fn top(&self) -> Frame {
        *self.stack.last().expect("root stays on the stack")
    }

    fn name_of(&self, id: NodeId) -> &'a str {
        let span = self.nodes[id]
            .name_span
            .clone()
            .expect("element has a name");
        &self.src[span]
    }

    fn run(&mut self) -> Result<(), WebError> {
        let len = self.b.len();
        while self.pos < len {
            self.tick()?;
            if self.b[self.pos] == b'<' {
                match self.b.get(at(self.pos, 1)).copied() {
                    Some(b'!') => self.bang()?,
                    Some(b'?') => self.bogus(LossKind::ProcessingInstruction)?,
                    Some(b'/') => match self.b.get(at(self.pos, 2)).copied() {
                        Some(c) if c.is_ascii_alphabetic() => self.end_tag()?,
                        None => self.text()?,
                        _ => self.bogus(LossKind::BogusMarkup)?,
                    },
                    Some(c) if c.is_ascii_alphabetic() => self.start_tag()?,
                    _ => self.text()?,
                }
            } else {
                self.text()?;
            }
        }
        if self.stack.len() > 1 {
            let id = self.top().id;
            let open = self.nodes[id]
                .open_span
                .clone()
                .expect("element has an open tag");
            return Err(if in_set(OPTIONAL_END, self.name_of(id)) {
                WebError::Unsupported {
                    kind: UnsupportedKind::ImpliedEndTag,
                    span: open,
                }
            } else {
                WebError::Malformed {
                    kind: MalformedKind::UnclosedElement,
                    span: open,
                }
            });
        }
        Ok(())
    }

    fn text(&mut self) -> Result<(), WebError> {
        let start = self.pos;
        let len = self.b.len();
        let mut i = at(start, 1);
        loop {
            match self.b[i..].iter().position(|&c| c == b'<') {
                None => {
                    i = len;
                    break;
                }
                Some(off) => {
                    let p = at(i, off);
                    if markup_start(self.b.get(at(p, 1)).copied()) {
                        i = p;
                        break;
                    }
                    i = at(p, 1);
                }
            }
        }
        self.push(Node::new(NodeKind::Text, start..i))?;
        self.pos = i;
        Ok(())
    }

    fn leaf(&mut self, kind: NodeKind, end: usize) -> Result<(), WebError> {
        let start = self.pos;
        self.push(Node::new(kind, start..end))?;
        self.pos = end;
        Ok(())
    }

    fn bogus(&mut self, loss: LossKind) -> Result<(), WebError> {
        let start = self.pos;
        let Some(off) = self.b[at(start, 2)..].iter().position(|&c| c == b'>') else {
            return Err(WebError::Malformed {
                kind: MalformedKind::UnterminatedMarkup,
                span: start..self.b.len(),
            });
        };
        let end = at(at(start, 2), at(off, 1));
        self.loss.push(loss, start..end);
        self.leaf(NodeKind::Unsupported, end)
    }

    fn bang(&mut self) -> Result<(), WebError> {
        let start = self.pos;
        let rest = &self.b[start..];
        if rest.starts_with(b"<!--") {
            let Some(end) = comment_end(self.b, start) else {
                return Err(WebError::Malformed {
                    kind: MalformedKind::UnterminatedComment,
                    span: start..self.b.len(),
                });
            };
            self.leaf(NodeKind::Comment, end)
        } else if rest.len() >= 9 && rest[..9].eq_ignore_ascii_case(b"<!doctype") {
            let Some(off) = rest.iter().position(|&c| c == b'>') else {
                return Err(WebError::Malformed {
                    kind: MalformedKind::UnterminatedMarkup,
                    span: start..self.b.len(),
                });
            };
            self.leaf(NodeKind::Doctype, at(at(start, off), 1))
        } else if rest.starts_with(b"<![CDATA[") {
            // A CDATA section exists only where the current node is not an HTML element; in HTML content the same bytes
            // are a bogus comment that ends at the first `>`.
            let end = if self.top().id != 0 && self.top().ns != Namespace::Html {
                find(&rest[9..], b"]]>").map(|off| at(at(start, 9), at(off, 3)))
            } else {
                rest.iter()
                    .position(|&c| c == b'>')
                    .map(|off| at(at(start, off), 1))
            };
            let Some(end) = end else {
                return Err(WebError::Malformed {
                    kind: MalformedKind::UnterminatedMarkup,
                    span: start..self.b.len(),
                });
            };
            self.loss.push(LossKind::Cdata, start..end);
            self.leaf(NodeKind::Unsupported, end)
        } else {
            self.bogus(LossKind::BogusMarkup)
        }
    }

    fn start_tag(&mut self) -> Result<(), WebError> {
        let b = self.b;
        let len = b.len();
        let start = self.pos;
        let name_start = at(start, 1);
        let mut i = name_start;
        while i < len && !is_ws(b[i]) && b[i] != b'/' && b[i] != b'>' {
            i = at(i, 1);
        }
        let name_span = name_start..i;
        let mut attrs: Vec<Attr> = Vec::new();
        let (open_end, self_closing) = loop {
            while i < len && is_ws(b[i]) {
                i = at(i, 1);
            }
            if i >= len {
                return Err(WebError::Malformed {
                    kind: MalformedKind::UnterminatedTag,
                    span: start..len,
                });
            }
            match b[i] {
                b'>' => break (at(i, 1), false),
                b'/' => {
                    if b.get(at(i, 1)) == Some(&b'>') {
                        break (at(i, 2), true);
                    }
                    i = at(i, 1);
                    continue;
                }
                b'=' => {
                    return Err(WebError::Malformed {
                        kind: MalformedKind::InvalidAttribute,
                        span: i..at(i, 1),
                    });
                }
                _ => {}
            }
            if attrs.len() >= self.limits.max_attributes {
                return Err(WebError::LimitExceeded {
                    limit: LimitKind::Attributes,
                    span: start..i,
                });
            }
            self.tick()?;
            let a_start = i;
            while i < len && !is_ws(b[i]) && !matches!(b[i], b'/' | b'>' | b'=') {
                i = at(i, 1);
            }
            let a_name = a_start..i;
            let mut j = i;
            while j < len && is_ws(b[j]) {
                j = at(j, 1);
            }
            if j < len && b[j] == b'=' {
                j = at(j, 1);
                while j < len && is_ws(b[j]) {
                    j = at(j, 1);
                }
                if j >= len {
                    return Err(WebError::Malformed {
                        kind: MalformedKind::UnterminatedTag,
                        span: start..len,
                    });
                }
                match b[j] {
                    q @ (b'"' | b'\'') => {
                        let vs = at(j, 1);
                        let Some(off) = b[vs..].iter().position(|&c| c == q) else {
                            return Err(WebError::Malformed {
                                kind: MalformedKind::UnterminatedValue,
                                span: j..len,
                            });
                        };
                        let ve = at(vs, off);
                        attrs.push(Attr {
                            span: a_start..at(ve, 1),
                            name_span: a_name,
                            value_span: Some(vs..ve),
                            quote: if q == b'"' {
                                Quote::Double
                            } else {
                                Quote::Single
                            },
                        });
                        i = at(ve, 1);
                    }
                    b'>' => {
                        return Err(WebError::Malformed {
                            kind: MalformedKind::InvalidAttribute,
                            span: a_start..at(j, 1),
                        });
                    }
                    _ => {
                        let mut k = j;
                        while k < len && !is_ws(b[k]) && b[k] != b'>' {
                            k = at(k, 1);
                        }
                        attrs.push(Attr {
                            span: a_start..k,
                            name_span: a_name,
                            value_span: Some(j..k),
                            quote: Quote::Unquoted,
                        });
                        i = k;
                    }
                }
            } else {
                attrs.push(Attr {
                    span: a_start..i,
                    name_span: a_name,
                    value_span: None,
                    quote: Quote::Absent,
                });
            }
        };

        let name = &self.src[name_span.clone()];
        // Tree-construction dispatcher (HTML Standard 13.2.6): which rules this start tag follows.
        let top = self.top();
        let html_rules = top.id == 0
            || top.ns == Namespace::Html
            || top.html_ip
            || (top.mathml_text_ip && !in_set(&["mglyph", "malignmark"], name))
            || (top.annotation_xml && name.eq_ignore_ascii_case("svg"));
        let ns = if html_rules {
            if name.eq_ignore_ascii_case("svg") {
                Namespace::Svg
            } else if name.eq_ignore_ascii_case("math") {
                Namespace::MathMl
            } else {
                Namespace::Html
            }
        } else {
            let font_breakout = name.eq_ignore_ascii_case("font")
                && attrs
                    .iter()
                    .any(|a| in_set(&["color", "face", "size"], &self.src[a.name_span.clone()]));
            if in_set(BREAKOUT, name) || font_breakout {
                // A browser pops the foreign elements and re-nests this element; the crate never re-nests.
                return Err(WebError::Unsupported {
                    kind: UnsupportedKind::ForeignBreakout,
                    span: start..open_end,
                });
            }
            top.ns
        };
        let html = ns == Namespace::Html;
        if html && name.eq_ignore_ascii_case("plaintext") {
            return Err(WebError::Unsupported {
                kind: UnsupportedKind::PlaintextElement,
                span: start..open_end,
            });
        }
        if html && name.eq_ignore_ascii_case("frameset") {
            return Err(WebError::Unsupported {
                kind: UnsupportedKind::FramesetDocument,
                span: start..open_end,
            });
        }
        if self.selects > 0 && html_rules && in_set(SELECT_DIVERGENT, name) {
            return Err(WebError::Unsupported {
                kind: UnsupportedKind::SelectContent,
                span: start..open_end,
            });
        }
        let is_void = html && in_set(VOID, name);
        let acknowledged = is_void || !html;
        let empty = is_void || (self_closing && !html);
        if self_closing && !acknowledged {
            self.loss
                .push(LossKind::SelfClosingIgnored, start..open_end);
        }
        if html_rules && !html {
            self.loss.push(LossKind::ForeignContent, start..open_end);
        }
        if attrs.len() > 1 {
            let mut seen: HashSet<String> = HashSet::with_capacity(attrs.len());
            for a in &attrs {
                if !seen.insert(self.src[a.name_span.clone()].to_ascii_lowercase()) {
                    self.loss.push(LossKind::DuplicateAttribute, a.span.clone());
                }
            }
        }
        let content = if empty {
            ContentModel::Empty
        } else if html && in_set(RAWTEXT, name) {
            ContentModel::RawText
        } else if html && in_set(RCDATA, name) {
            ContentModel::Rcdata
        } else {
            ContentModel::Normal
        };
        // The first `encoding` attribute (later duplicates are dropped by the tokenizer), decoded like any value.
        let encoding = attrs
            .iter()
            .find(|a| self.src[a.name_span.clone()].eq_ignore_ascii_case("encoding"))
            .and_then(|a| a.value_span.clone())
            .map(|s| decode(&self.src[s], Context::Attribute).text);
        let noscript = html && name.eq_ignore_ascii_case("noscript");
        let frame_base = Frame {
            id: 0,
            ns,
            html_ip: is_html_integration_point(ns, name, encoding.as_deref()),
            mathml_text_ip: ns == Namespace::MathMl
                && in_set(&["mi", "mo", "mn", "ms", "mtext"], name),
            annotation_xml: ns == Namespace::MathMl && name.eq_ignore_ascii_case("annotation-xml"),
            // With scripting enabled a browser reads `noscript` as RAWTEXT; both readings must end at the same tag.
            noscript_close: if noscript {
                find_raw_close(b, open_end, b"noscript")
            } else {
                None
            },
        };
        let select = html && name.eq_ignore_ascii_case("select");
        let open_span = start..open_end;
        let mut node = Node::new(NodeKind::Element, open_span.clone());
        node.name_span = Some(name_span);
        node.open_span = Some(open_span);
        node.attrs = attrs;
        node.self_closing = self_closing;
        node.content = content;
        node.protected = in_set(PROTECTED, name);
        node.namespace = ns;
        let name_owned = name.to_owned();
        let id = self.push(node)?;
        self.pos = open_end;
        match content {
            ContentModel::Empty => {}
            ContentModel::Normal => {
                if self.stack.len() > self.limits.max_depth {
                    return Err(WebError::LimitExceeded {
                        limit: LimitKind::Depth,
                        span: start..open_end,
                    });
                }
                self.stack.push(Frame { id, ..frame_base });
                if select {
                    self.selects = self.selects.saturating_add(1);
                }
            }
            ContentModel::RawText | ContentModel::Rcdata => self.raw_body(id, &name_owned)?,
        }
        Ok(())
    }

    fn raw_body(&mut self, id: NodeId, name: &str) -> Result<(), WebError> {
        let b = self.b;
        let content_start = self.pos;
        let open = self.nodes[id]
            .open_span
            .clone()
            .expect("element has an open tag");
        let Some(close) = find_raw_text_close(b, content_start, name) else {
            return Err(WebError::Malformed {
                kind: MalformedKind::UnterminatedRawText,
                span: open,
            });
        };
        let mut j = at(at(close, 2), name.len());
        while j < b.len() && is_ws(b[j]) {
            j = at(j, 1);
        }
        if j >= b.len() || b[j] != b'>' {
            let end = if j < b.len() {
                self.char_end(j)
            } else {
                b.len()
            };
            return Err(WebError::Malformed {
                kind: MalformedKind::InvalidEndTag,
                span: close..end,
            });
        }
        let end = at(j, 1);
        if close > content_start {
            self.push_under(id, Node::new(NodeKind::Raw, content_start..close))?;
        }
        self.nodes[id].close_span = Some(close..end);
        self.nodes[id].span.end = end;
        self.pos = end;
        Ok(())
    }

    fn end_tag(&mut self) -> Result<(), WebError> {
        let b = self.b;
        let len = b.len();
        let start = self.pos;
        let name_start = at(start, 2);
        let mut i = name_start;
        while i < len && !is_ws(b[i]) && b[i] != b'/' && b[i] != b'>' {
            i = at(i, 1);
        }
        let name_end = i;
        while i < len && is_ws(b[i]) {
            i = at(i, 1);
        }
        if i >= len {
            return Err(WebError::Malformed {
                kind: MalformedKind::UnterminatedTag,
                span: start..len,
            });
        }
        if b[i] != b'>' {
            return Err(WebError::Malformed {
                kind: MalformedKind::InvalidEndTag,
                span: start..self.char_end(i),
            });
        }
        let end = i.saturating_add(1);
        let name = &self.src[name_start..name_end];
        let frame = self.top();
        let top = frame.id;
        // A foreign element named like an HTML void element (`<svg><source></source>`) has content and an end tag.
        let foreign_match =
            top != 0 && frame.ns != Namespace::Html && self.name_of(top).eq_ignore_ascii_case(name);
        if !foreign_match && in_set(VOID, name) {
            return Err(WebError::Malformed {
                kind: MalformedKind::VoidEndTag,
                span: start..end,
            });
        }
        if top == 0 {
            return Err(WebError::Malformed {
                kind: MalformedKind::StrayEndTag,
                span: start..end,
            });
        }
        let top_name = self.name_of(top);
        if !top_name.eq_ignore_ascii_case(name) {
            let ancestor = self.stack[1..]
                .iter()
                .any(|f| self.name_of(f.id).eq_ignore_ascii_case(name));
            return Err(if !ancestor {
                WebError::Malformed {
                    kind: MalformedKind::StrayEndTag,
                    span: start..end,
                }
            } else if in_set(OPTIONAL_END, top_name) {
                WebError::Unsupported {
                    kind: UnsupportedKind::ImpliedEndTag,
                    span: start..end,
                }
            } else {
                WebError::Malformed {
                    kind: MalformedKind::MisnestedEndTag,
                    span: start..end,
                }
            });
        }
        let html = frame.ns == Namespace::Html;
        if html && top_name.eq_ignore_ascii_case("noscript") && frame.noscript_close != Some(start)
        {
            // Scripting enabled, the RAWTEXT reading ends at an earlier `</noscript`; the two readings disagree.
            let at = frame.noscript_close.unwrap_or(start);
            return Err(WebError::Unsupported {
                kind: UnsupportedKind::ScriptingDependent,
                span: at..at.saturating_add(10),
            });
        }
        let select = html && top_name.eq_ignore_ascii_case("select");
        let node = &mut self.nodes[top];
        node.close_span = Some(start..end);
        node.span.end = end;
        self.stack.pop();
        if select {
            self.selects = self.selects.saturating_sub(1);
        }
        self.pos = end;
        Ok(())
    }
}

/// Pre-order iterator over a subtree (explicit stack; safe at any depth).
pub struct Descendants<'a> {
    doc: &'a WebDoc,
    stack: Vec<NodeId>,
}
impl Iterator for Descendants<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let id = self.stack.pop()?;
        self.stack
            .extend(self.doc.nodes[id].children.iter().rev().copied());
        Some(id)
    }
}

impl WebDoc {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn sha256_hex(&self) -> &str {
        &self.sha256_hex
    }
    pub fn limits(&self) -> &Limits {
        &self.limits
    }
    pub fn loss(&self) -> &LossReport {
        &self.loss
    }
    pub fn root(&self) -> NodeId {
        0
    }
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        false
    }
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }
    pub fn slice(&self, span: &Range<usize>) -> &str {
        &self.source[span.clone()]
    }

    /// Tag name with its original case.
    pub fn tag_name(&self, id: NodeId) -> Option<&str> {
        let node = self.nodes.get(id)?;
        node.name_span.as_ref().map(|s| &self.source[s.clone()])
    }

    pub fn attr_name(&self, attr: &Attr) -> &str {
        &self.source[attr.name_span.clone()]
    }

    /// Value decoded as an HTML tokenizer decodes attribute values; `None` for a valueless attribute.
    pub fn attr_value(&self, attr: &Attr) -> Option<String> {
        attr.value_span
            .as_ref()
            .map(|s| decode(&self.source[s.clone()], Context::Attribute).text)
    }

    /// First attribute with this name (ASCII case-insensitive); later duplicates are ignored as in HTML.
    pub fn attribute(&self, id: NodeId, name: &str) -> Option<&Attr> {
        self.nodes
            .get(id)?
            .attrs
            .iter()
            .find(|a| self.attr_name(a).eq_ignore_ascii_case(name))
    }

    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants {
            doc: self,
            stack: if id < self.nodes.len() {
                vec![id]
            } else {
                Vec::new()
            },
        }
    }

    pub(crate) fn label(&self, id: NodeId) -> String {
        let node = &self.nodes[id];
        match node.kind {
            NodeKind::Element => self.tag_name(id).unwrap_or("").to_ascii_lowercase(),
            NodeKind::Text => "#text".into(),
            NodeKind::Comment => "#comment".into(),
            NodeKind::Doctype => "#doctype".into(),
            NodeKind::Raw => "#raw".into(),
            NodeKind::Unsupported => "#unsupported".into(),
            NodeKind::Document => String::new(),
        }
    }

    pub fn key_of(&self, id: NodeId) -> Option<NodeKey> {
        self.nodes.get(id)?;
        if id == 0 {
            return Some(NodeKey("/".into()));
        }
        let mut segments = Vec::new();
        let mut cur = id;
        while let Some(parent) = self.nodes[cur].parent {
            segments.push(format!("{}:{}", self.nodes[cur].index, self.label(cur)));
            cur = parent;
        }
        segments.reverse();
        Some(NodeKey(format!("/{}", segments.join("/"))))
    }

    pub fn resolve(&self, key: &NodeKey) -> Option<NodeId> {
        let path = key.0.strip_prefix('/')?;
        let mut cur = 0;
        if path.is_empty() {
            return Some(cur);
        }
        for segment in path.split('/') {
            let (index, label) = segment.split_once(':')?;
            let index: usize = index.parse().ok()?;
            cur = *self.nodes[cur].children.get(index)?;
            if self.label(cur) != label {
                return None;
            }
        }
        Some(cur)
    }

    /// Decoded text content of a subtree (script/style bodies and comments excluded).
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        for d in self.descendants(id) {
            let node = &self.nodes[d];
            match node.kind {
                NodeKind::Text => out.push_str(&decode(self.slice(&node.span), Context::Text).text),
                NodeKind::Raw => {
                    let owner = node.parent.map(|p| self.nodes[p].content);
                    if owner == Some(ContentModel::Rcdata) {
                        out.push_str(&decode(self.slice(&node.span), Context::Text).text);
                    }
                }
                _ => {}
            }
        }
        out
    }

    pub fn find_by_tag(&self, tag: &str) -> Vec<NodeId> {
        (0..self.nodes.len())
            .filter(|&id| {
                self.nodes[id].kind == NodeKind::Element
                    && self
                        .tag_name(id)
                        .is_some_and(|n| n.eq_ignore_ascii_case(tag))
            })
            .collect()
    }

    /// First element whose HTML `id` attribute equals `value`.
    pub fn find_by_html_id(&self, value: &str) -> Option<NodeId> {
        (0..self.nodes.len()).find(|&id| {
            self.nodes[id].kind == NodeKind::Element
                && self
                    .attribute(id, "id")
                    .and_then(|a| self.attr_value(a))
                    .as_deref()
                    == Some(value)
        })
    }

    /// Pre-order DOM tree view with depth; use `key_of(row.id)` for the stable key.
    pub fn view(&self) -> Vec<ViewRow> {
        let mut rows = Vec::with_capacity(self.nodes.len());
        let mut stack = vec![(0usize, 0usize)];
        while let Some((id, depth)) = stack.pop() {
            let node = &self.nodes[id];
            let text = match node.kind {
                NodeKind::Text => Some(decode(self.slice(&node.span), Context::Text).text),
                NodeKind::Raw | NodeKind::Doctype | NodeKind::Unsupported => {
                    Some(self.slice(&node.span).to_owned())
                }
                NodeKind::Comment => {
                    let s = self.slice(&node.span);
                    Some(
                        s.strip_prefix("<!--")
                            .map(|s| s.strip_suffix("-->").unwrap_or(s))
                            .unwrap_or(s)
                            .to_owned(),
                    )
                }
                _ => None,
            };
            rows.push(ViewRow {
                id,
                depth,
                kind: node.kind,
                tag: self.tag_name(id).map(str::to_owned),
                attrs: node
                    .attrs
                    .iter()
                    .map(|a| (self.attr_name(a).to_owned(), self.attr_value(a)))
                    .collect(),
                text,
                span: node.span.clone(),
            });
            for &child in node.children.iter().rev() {
                stack.push((child, at(depth, 1)));
            }
        }
        rows
    }
}
