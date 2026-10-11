//! The URL carrier inventory: one table of every place a document hands a browser a URL to load (plus the identifiers and
//! obsolete attributes the gate reads conservatively), derived from the specification indices cited in each row:
//! - HTML Standard, 9 October 2026: the attribute index (`indices.html` "Attributes", every value type that is a URL, a
//!   set of URLs or image candidate strings, `srcdoc`, `meta http-equiv=refresh`, `script type=speculationrules`) and the
//!   obsolete features (`obsolete.html` 16.2 and the `ReflectURL` IDL of 16.3);
//! - WICG Attribution Reporting (HTML monkeypatches 2.1: `attributionsrc` on `a area img script`);
//! - SVG 2 attribute index (`attindex.html`: `href`/`xlink:href`) and presentation attribute table (`styling.html` 6.6);
//! - SVG Animations Level 2 (2.6 `attributeName`: a CSS property name is matched before an attribute name; 2.10
//!   `from`/`to`/`by`/`values`), with the engines' `values` split on every `;` before each piece is parsed (Blink
//!   `SVGAnimationElement::ParseValues` `StringView(value).Split(';')`, Gecko `SMILParserUtils::ParseValues`
//!   (`nsCharSeparatedTokenizer` on `;`), WebKit `SVGAnimationElement` `split(';')`), and the engines' CSS targets (Gecko
//!   `SMILCSSProperty::IsPropertyAnimatable` incl. the `marker` shorthand, Blink `AnimatedPropertyTypeForCSSAttribute`);
//! - the CSS property, descriptor, at-rule and function index as extracted by W3C webref (`@webref/css` 8.7.6, the
//!   CSSWG drafts' definitions): every property, descriptor and at-rule prelude whose value grammar reaches `<url>`,
//!   `<image>` or a URL function, and every function whose `<string>` argument is a URL;
//! - `data:` URLs: Fetch "data: URL processor", MIME Sniffing "parse a MIME type", Infra "forgiving-base64 decode",
//!   Encoding labels, CSS Syntax 3 3.2 (module `datauri`).
//!
//! `collect_assets` dispatches through this table (`attribute_path`, `animation_target`, `css_target`), so a carrier is
//! scanned exactly by the path its row names. Every row names a fixture; `carriers::tests::every_row_has_a_path_and_a_fixture`
//! fails when a row has no fixture, when the dispatch does not reach the row's path, or when the fixture does not report
//! (or, for `Path::NoLoad`, does report) the URL the row carries.
#![deny(clippy::arithmetic_side_effects)]

/// Which part of a document a row describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// An attribute on an HTML element (`elements` empty = any element).
    HtmlAttribute,
    /// An attribute on an SVG (or any foreign) element (`elements` empty = any element).
    SvgAttribute,
    /// `from`/`to`/`by`/`values` of an SVG animation element; the target attribute or property decides the reading.
    SvgAnimation,
    /// The text content of an element (`style`, `script`).
    ElementText,
    /// A CSS property (declarations, presentation attributes, SMIL targets).
    CssProperty,
    /// A CSS at-rule prelude or descriptor (`elements` = the at-rule).
    CssAtRule,
    /// A CSS function whose string argument is a URL.
    CssFunction,
    /// A `data:` URL payload, by MIME type and load context.
    DataPayload,
}

/// What a browser does with what it fetches from the carrier: decides how a `data:` payload is treated (`datauri`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Load {
    /// Decoded as an image; SVG runs in secure (static or animated) mode and loads nothing further.
    Image,
    /// Played as audio or video (a playlist type loads its segments).
    Media,
    /// A text track (WebVTT `STYLE` blocks hold CSS).
    Track,
    /// Rendered in a nested navigable (`iframe`, `frame`, `object`, `embed`) or navigated to (`a`, `area`, `form`,
    /// refresh).
    Document,
    /// An SVG resource document (`use`, paint servers, `filter`/`mask`/`clip-path`/`marker` references), or a CSS value
    /// whose property the scan does not track (the most restrictive CSS reading).
    Resource,
    /// A style sheet (`link`, `@import`; in quirks mode any same-origin type, so a `data:` payload is read as CSS).
    Stylesheet,
    /// A script: it executes.
    Script,
    /// A fire-and-forget request (`ping`, `attributionsrc`): the response is not used.
    Beacon,
    /// A base URL for the other references (`base href`, `xml:base`).
    Base,
    /// An inline document the gate denies unscanned (`srcdoc`, speculation rules).
    Inline,
    /// Not loaded by current engines (identifiers, obsolete attributes); read conservatively.
    None,
}

/// How the gate reads the carrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Path {
    /// One URL (surrounding whitespace ignored).
    Url,
    /// ASCII-whitespace separated URLs.
    UrlList,
    /// Image candidate strings.
    Srcset,
    /// CSS text: `cssurl` (url tokens, URL-function strings, `@import` strings, computed URLs).
    Css,
    /// `<meta http-equiv=refresh>` content.
    Refresh,
    /// A base URL: reported, and every relative reference of the document is denied `BaseHref`.
    Base,
    /// An inline document: reported `Denied(InlineDocument)`, not scanned.
    InlineDoc,
    /// `from`/`to`/`by`: the whole value read as the animation target reads it (URL or CSS).
    AnimationValue,
    /// `values`: split on every `;` (engines), each piece read as the target reads it.
    AnimationValues,
    /// Inside CSS text, read by `cssurl` (a CSS row: the token rules apply in every property and descriptor).
    CssToken,
    /// The `@import` URL: a style sheet (`cssurl` marks it `import`).
    CssImport,
    /// A `data:` payload, decided by `datauri::decide` for its load context.
    Data,
    /// Nothing is loaded; nothing is reported.
    NoLoad,
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "source and fixture document the row; the inventory test reads them"
    )
)]
pub(crate) struct Carrier {
    pub(crate) class: Class,
    /// Lower-case element local names (attributes, animations, element text) or the at-rule (CSS descriptors); empty =
    /// any element.
    pub(crate) elements: &'static [&'static str],
    /// Lower-case attribute names, property names, function names (without `(`), or MIME essences / classes for
    /// `data:` rows.
    pub(crate) names: &'static [&'static str],
    pub(crate) path: Path,
    pub(crate) load: Load,
    /// Specification (and engine) source of the row.
    pub(crate) source: &'static str,
    /// Fixture in `carriers::tests::fixture` that exercises every element and name of the row.
    pub(crate) fixture: &'static str,
}

const HTML: &str = "HTML Standard (9 Oct 2026) indices.html Attributes";
const OBSOLETE: &str =
    "HTML Standard (9 Oct 2026) obsolete.html 16.2 non-conforming features, 16.3 ReflectURL IDL";
const SVG_ATTRS: &str = "SVG 2 attindex.html (href, xlink:href)";
const SVG_PRESENTATION: &str = "SVG 2 styling.html 6.6 presentation attributes (the <url>-taking ones; Blink svg_element.cc, Gecko SVGElement IsNoNamespaceAttrMapped)";
const SMIL: &str = "SVG Animations L2 2.6 attributeName (CSS property first), 2.10 from/to/by/values; Gecko SMILCSSProperty::ValueFromString (whole value), Blink SVGAnimateElement";
const SMIL_VALUES: &str = "SVG Animations L2 2.10 values; Blink SVGAnimationElement::ParseValues Split(';'), Gecko SMILParserUtils::ParseValues (';' tokenizer), WebKit SVGAnimationElement split(';')";
const WEBREF: &str =
    "CSS property index via @webref/css 8.7.6 (grammar reaches <url>/<image>/url())";

/// The animation elements (SVG Animations L2 2.13-2.16; `animateColor` from SVG 1.1, kept conservatively). `discard`
/// carries only `href` (any-element row).
pub(crate) const ANIMATION_ELEMENTS: &[&str] = &[
    "animate",
    "set",
    "animatemotion",
    "animatetransform",
    "animatecolor",
];

/// CSS properties whose grammar reaches `<url>`, `<image>` or a URL function (webref 8.7.6, excluding the colour-only
/// reach through `light-dark()`, listed in its own row), plus the engines' prefixed aliases.
pub(crate) const CSS_URL_PROPERTIES: &[&str] = &[
    // CSS Backgrounds 4 / Borders 4
    "background",
    "background-image",
    "background-tbd",
    "border-image",
    "border-image-source",
    // CSS Lists 3
    "list-style",
    "list-style-image",
    "list-style-type",
    // CSS Content 3 / GCPM 4
    "content",
    "bookmark-label",
    "copy-into",
    // CSS UI 4
    "cursor",
    // CSS Masking 1
    "clip-path",
    "mask",
    "mask-image",
    "mask-border",
    "mask-border-source",
    // Filter Effects 1 / 2
    "filter",
    "backdrop-filter",
    // SVG 2 painting / Fill and Stroke 3
    "fill",
    "stroke",
    "fill-image",
    "stroke-image",
    "marker",
    "marker-start",
    "marker-mid",
    "marker-end",
    // Motion Path 1
    "offset",
    "offset-path",
    // CSS Shapes 1 / 2
    "shape-outside",
    "shape-inside",
    // CSS Speech 1
    "cue",
    "cue-before",
    "cue-after",
    // Compat Standard and engine-prefixed aliases (WebKit/Blink)
    "-webkit-filter",
    "-webkit-mask",
    "-webkit-mask-image",
    "-webkit-mask-box-image",
    "-webkit-mask-box-image-source",
    "-webkit-border-image",
    "-webkit-box-reflect",
    "-webkit-clip-path",
    "-webkit-shape-outside",
    "-webkit-backdrop-filter",
    "-webkit-list-style",
    "-webkit-list-style-image",
    "-webkit-cursor",
    "-webkit-mask-border",
    "-webkit-mask-border-source",
];

/// Properties whose webref grammar reaches `<image>` only through `<color>`'s `light-dark()` (CSS Color 5
/// `<light-dark-image>`, valid only where `<image>` is): no engine loads a URL there; url tokens are reported anyway and
/// they are SMIL CSS targets (over-reporting is safe).
pub(crate) const CSS_COLOR_ONLY_PROPERTIES: &[&str] = &[
    "color",
    "accent-color",
    "background-color",
    "border",
    "border-block",
    "border-block-color",
    "border-block-end",
    "border-block-end-color",
    "border-block-start",
    "border-block-start-color",
    "border-bottom",
    "border-bottom-color",
    "border-color",
    "border-inline",
    "border-inline-color",
    "border-inline-end",
    "border-inline-end-color",
    "border-inline-start",
    "border-inline-start-color",
    "border-left",
    "border-left-color",
    "border-right",
    "border-right-color",
    "border-top",
    "border-top-color",
    "box-shadow",
    "box-shadow-color",
    "caret",
    "caret-color",
    "column-rule",
    "column-rule-color",
    "fill-color",
    "flood-color",
    "lighting-color",
    "outline",
    "outline-color",
    "row-rule",
    "row-rule-color",
    "rule",
    "rule-color",
    "scrollbar-color",
    "stop-color",
    "stroke-color",
    "text-decoration",
    "text-decoration-color",
    "text-emphasis",
    "text-emphasis-color",
    "text-shadow",
    "-webkit-box-shadow",
    "-webkit-text-fill-color",
    "-webkit-text-stroke",
    "-webkit-text-stroke-color",
];

/// SVG presentation attributes whose property takes `<url>` (SVG 2 6.6).
pub(crate) const PRESENTATION_URL_ATTRIBUTES: &[&str] = &[
    "fill",
    "stroke",
    "clip-path",
    "cursor",
    "filter",
    "mask",
    "marker-start",
    "marker-mid",
    "marker-end",
];

/// CSS functions whose `<string>` argument is a URL (the strings directly inside them are reported): CSS Values 4 4.5
/// `url()`, `src()`; CSS Images 4 2.4 `image-set()` and Appendix A `-webkit-image-set()`; CSS Images 4/5 `image()`
/// (`<image-src> = [ <url> | <string> ]`); Filter Effects 1 12 `filter( [ <image> | <string> ], ... )` and WebKit
/// `-webkit-filter()`; `cross-fade()`/`-webkit-cross-fade()` (no string in the grammar; reported conservatively).
pub(crate) const URL_FUNCTIONS: &[&str] = &[
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

pub(crate) const CARRIERS: &[Carrier] = &[
    // ----- HTML attributes (specific elements first: the first matching row is the dispatch) -------------------------
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["base"],
        names: &["href"],
        path: Path::Base,
        load: Load::Base,
        source: HTML,
        fixture: "base_href",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["link"],
        names: &["href"],
        path: Path::Url,
        load: Load::Stylesheet,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["a", "area"],
        names: &["href"],
        path: Path::Url,
        load: Load::Document,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["img", "input"],
        names: &["src"],
        path: Path::Url,
        load: Load::Image,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["audio", "video", "source"],
        names: &["src"],
        path: Path::Url,
        load: Load::Media,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["iframe", "embed"],
        names: &["src"],
        path: Path::Url,
        load: Load::Document,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["script"],
        names: &["src"],
        path: Path::Url,
        load: Load::Script,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["track"],
        names: &["src"],
        path: Path::Url,
        load: Load::Track,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["video"],
        names: &["poster"],
        path: Path::Url,
        load: Load::Image,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["object"],
        names: &["data"],
        path: Path::Url,
        load: Load::Document,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["form"],
        names: &["action"],
        path: Path::Url,
        load: Load::Document,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["button", "input"],
        names: &["formaction"],
        path: Path::Url,
        load: Load::Document,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["blockquote", "del", "ins", "q"],
        names: &["cite"],
        path: Path::Url,
        load: Load::None,
        source: HTML,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["a", "area"],
        names: &["ping"],
        path: Path::UrlList,
        load: Load::Beacon,
        source: HTML,
        fixture: "html_url_list",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["img", "source"],
        names: &["srcset"],
        path: Path::Srcset,
        load: Load::Image,
        source: HTML,
        fixture: "html_srcset",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["link"],
        names: &["imagesrcset"],
        path: Path::Srcset,
        load: Load::Image,
        source: HTML,
        fixture: "html_srcset",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["iframe"],
        names: &["srcdoc"],
        path: Path::InlineDoc,
        load: Load::Inline,
        source: HTML,
        fixture: "html_srcdoc",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["meta"],
        names: &["content"],
        path: Path::Refresh,
        load: Load::Document,
        source: "HTML Standard (9 Oct 2026) indices.html http-equiv \"refresh\"; 4.2.5.3 shared declarative refresh steps",
        fixture: "meta_refresh",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &[],
        names: &["itemid", "itemtype", "itemprop"],
        path: Path::NoLoad,
        load: Load::None,
        source: "HTML Standard (9 Oct 2026) indices.html itemid/itemtype/itemprop: identifiers, never fetched",
        fixture: "no_load_attribute",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["frame"],
        names: &["src"],
        path: Path::Url,
        load: Load::Document,
        source: OBSOLETE,
        fixture: "frameset_refused",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["frame", "iframe", "img"],
        names: &["longdesc"],
        path: Path::Url,
        load: Load::None,
        source: OBSOLETE,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["img"],
        names: &["lowsrc"],
        path: Path::Url,
        load: Load::None,
        source: OBSOLETE,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["object"],
        names: &["codebase", "classid"],
        path: Path::Url,
        load: Load::None,
        source: OBSOLETE,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["object"],
        names: &["archive"],
        path: Path::UrlList,
        load: Load::None,
        source: OBSOLETE,
        fixture: "html_url_list",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["html"],
        names: &["manifest"],
        path: Path::Url,
        load: Load::None,
        source: OBSOLETE,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["head"],
        names: &["profile"],
        path: Path::UrlList,
        load: Load::None,
        source: OBSOLETE,
        fixture: "html_url_list",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["body", "table", "thead", "tbody", "tfoot", "tr", "td", "th"],
        names: &["background"],
        path: Path::Url,
        load: Load::Image,
        source: "HTML Standard (9 Oct 2026) obsolete.html 16.2 background; rendering.html 15.3.10 presentational hints",
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &[],
        names: &["datasrc"],
        path: Path::NoLoad,
        load: Load::None,
        source: OBSOLETE,
        fixture: "no_load_attribute",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["a", "area", "img", "script"],
        names: &["attributionsrc"],
        path: Path::UrlList,
        load: Load::Beacon,
        source: "WICG Attribution Reporting 2.1 HTMLAttributionSrcElementUtils (a, area, img, script): zero or more URLs",
        fixture: "html_url_list",
    },
    // ----- SVG attributes ------------------------------------------------------------------------------------------
    Carrier {
        class: Class::SvgAttribute,
        elements: &["image", "feimage"],
        names: &["href", "xlink:href"],
        path: Path::Url,
        load: Load::Image,
        source: SVG_ATTRS,
        fixture: "svg_url",
    },
    Carrier {
        class: Class::SvgAttribute,
        elements: &["use"],
        names: &["href", "xlink:href"],
        path: Path::Url,
        load: Load::Resource,
        source: SVG_ATTRS,
        fixture: "svg_url",
    },
    Carrier {
        class: Class::SvgAttribute,
        elements: &["script"],
        names: &["href", "xlink:href"],
        path: Path::Url,
        load: Load::Script,
        source: SVG_ATTRS,
        fixture: "svg_url",
    },
    Carrier {
        class: Class::SvgAttribute,
        elements: &["a"],
        names: &["xlink:href"],
        path: Path::Url,
        load: Load::Document,
        source: SVG_ATTRS,
        fixture: "svg_url",
    },
    Carrier {
        class: Class::SvgAttribute,
        elements: &[],
        names: &["href", "xlink:href"],
        path: Path::Url,
        load: Load::Resource,
        source: "SVG 2 attindex.html href (gradients, pattern, textPath, mpath, animation elements; discard per SVG Animations L2); href on any element (MathML links, unknown elements) read conservatively",
        fixture: "svg_url",
    },
    Carrier {
        class: Class::SvgAttribute,
        elements: &[],
        names: &["xml:base"],
        path: Path::Base,
        load: Load::Base,
        source: "XML Base (W3C Recommendation); SVG 1.1 core attribute xml:base (removed in SVG 2; engine support UNVERIFIED): treated like base href",
        fixture: "xml_base",
    },
    Carrier {
        class: Class::SvgAttribute,
        elements: &[],
        names: PRESENTATION_URL_ATTRIBUTES,
        path: Path::Css,
        load: Load::Resource,
        source: SVG_PRESENTATION,
        fixture: "css_attribute",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &[],
        names: &["style"],
        path: Path::Css,
        load: Load::Resource,
        source: "HTML Standard (9 Oct 2026) indices.html style (HTML elements); SVG 2 attindex.html style (every element)",
        fixture: "css_attribute",
    },
    // ----- SVG animation -------------------------------------------------------------------------------------------
    Carrier {
        class: Class::SvgAnimation,
        elements: ANIMATION_ELEMENTS,
        names: &["to", "from", "by"],
        path: Path::AnimationValue,
        load: Load::Resource,
        source: SMIL,
        fixture: "animation",
    },
    Carrier {
        class: Class::SvgAnimation,
        elements: ANIMATION_ELEMENTS,
        names: &["values"],
        path: Path::AnimationValues,
        load: Load::Resource,
        source: SMIL_VALUES,
        fixture: "animation",
    },
    // ----- element text --------------------------------------------------------------------------------------------
    Carrier {
        class: Class::ElementText,
        elements: &["style"],
        names: &["#text"],
        path: Path::Css,
        load: Load::Stylesheet,
        source: "HTML Standard 4.2.6 style (RAWTEXT body); SVG 2 6.4 style element (text and CDATA content)",
        fixture: "style_element",
    },
    Carrier {
        class: Class::ElementText,
        elements: &["script"],
        names: &["speculationrules"],
        path: Path::InlineDoc,
        load: Load::Inline,
        source: "HTML Standard (9 Oct 2026) indices.html script type \"speculationrules\"; 7.6 speculative loading (prefetch/prerender URL lists)",
        fixture: "speculation_rules",
    },
    // ----- CSS ------------------------------------------------------------------------------------------------------
    Carrier {
        class: Class::CssProperty,
        elements: &[],
        names: CSS_URL_PROPERTIES,
        path: Path::CssToken,
        load: Load::Resource,
        source: WEBREF,
        fixture: "css_property",
    },
    Carrier {
        class: Class::CssProperty,
        elements: &[],
        names: CSS_COLOR_ONLY_PROPERTIES,
        path: Path::CssToken,
        load: Load::None,
        source: "CSS property index via @webref/css 8.7.6: <image> reached only through <color> light-dark() (CSS Color 5)",
        fixture: "css_property",
    },
    Carrier {
        class: Class::CssAtRule,
        elements: &["@import"],
        names: &["prelude"],
        path: Path::CssImport,
        load: Load::Stylesheet,
        source: "CSS Cascade 6 @import [ <url> | <string> ] (webref 8.7.6)",
        fixture: "css_at_rule",
    },
    Carrier {
        class: Class::CssAtRule,
        elements: &["@font-face"],
        names: &["src"],
        path: Path::CssToken,
        load: Load::Resource,
        source: "CSS Fonts 4 4.3 src descriptor <font-src-list> (<url> [format()] [tech()] | local())",
        fixture: "css_at_rule",
    },
    Carrier {
        class: Class::CssAtRule,
        elements: &["@counter-style"],
        names: &[
            "symbols",
            "additive-symbols",
            "negative",
            "pad",
            "prefix",
            "suffix",
        ],
        path: Path::CssToken,
        load: Load::Resource,
        source: "CSS Counter Styles 3 <symbol> = <string> | <image> | <custom-ident> (webref 8.7.6)",
        fixture: "css_at_rule",
    },
    Carrier {
        class: Class::CssAtRule,
        elements: &["@color-profile"],
        names: &["src"],
        path: Path::CssToken,
        load: Load::Resource,
        source: "CSS Color 5 @color-profile src <url> (webref 8.7.6)",
        fixture: "css_at_rule",
    },
    Carrier {
        class: Class::CssAtRule,
        elements: &["@property"],
        names: &["initial-value"],
        path: Path::CssToken,
        load: Load::Resource,
        source: "CSS Properties and Values API 1 @property initial-value <declaration-value> (a <url> or <image> syntax)",
        fixture: "css_at_rule",
    },
    Carrier {
        class: Class::CssAtRule,
        elements: &["@namespace"],
        names: &["prelude"],
        path: Path::NoLoad,
        load: Load::None,
        source: "CSS Namespaces 3 @namespace <namespace-prefix>? [ <string> | <url> ]: a namespace name, never fetched",
        fixture: "css_namespace",
    },
    Carrier {
        class: Class::CssFunction,
        elements: &[],
        names: URL_FUNCTIONS,
        path: Path::CssToken,
        load: Load::Resource,
        source: "CSS Values 4 4.5 url()/src(); CSS Images 4 2.4/Appendix A image-set(), 2.5 image(), 2.6 cross-fade(); Filter Effects 1 12 filter(); WebKit -webkit-filter() (webref 8.7.6 functions)",
        fixture: "css_function",
    },
    // ----- data: payloads ------------------------------------------------------------------------------------------
    Carrier {
        class: Class::DataPayload,
        elements: &[],
        names: &["text/css"],
        path: Path::Data,
        load: Load::Stylesheet,
        source: "Fetch data: URL processor; CSS Syntax 3 3.2 decode; a style sheet's references load (every carrier: scanned recursively, bounded)",
        fixture: "data_css",
    },
    Carrier {
        class: Class::DataPayload,
        elements: &[],
        names: &[
            "text/html",
            "application/xhtml+xml",
            "image/svg+xml",
            "text/xml",
            "application/xml",
            "unknown/unknown",
            "text/javascript",
            "application/json",
            "text/vtt",
        ],
        path: Path::Data,
        load: Load::Document,
        source: "Fetch data: URL processor; HTML navigate/object/embed (documents), SVG resource documents, MIME Sniffing 7 (unknown types sniffed as HTML), scripts, playlists, text tracks: denied unless the carrier decodes it as an image",
        fixture: "data_document",
    },
    Carrier {
        class: Class::DataPayload,
        elements: &[],
        names: &[
            "image/png",
            "font/woff2",
            "audio/ogg",
            "text/plain",
            "application/octet-stream",
        ],
        path: Path::Data,
        load: Load::Image,
        source: "Fetch data: URL processor; types no carrier turns into a document, script, playlist or track: inline (style sheet carriers read them as CSS, quirks mode)",
        fixture: "data_inert",
    },
];

fn element_matches(row: &Carrier, tag: &str) -> bool {
    row.elements.is_empty() || row.elements.contains(&tag)
}

/// The path and load context of attribute `name` (lower-case) on element `tag` (lower-case local name): the first
/// attribute row naming both.
pub(crate) fn attribute_path(tag: &str, name: &str) -> Option<(Path, Load)> {
    CARRIERS
        .iter()
        .find(|row| {
            matches!(row.class, Class::HtmlAttribute | Class::SvgAttribute)
                && element_matches(row, tag)
                && row.names.contains(&name)
        })
        .map(|row| (row.path, row.load))
}

/// A CSS property in the inventory (any CSS property row, a presentation attribute, or `style`): an animation of it reads
/// its values as CSS.
pub(crate) fn css_target(name: &str) -> bool {
    CARRIERS.iter().any(|row| {
        row.names.contains(&name)
            && match row.class {
                Class::CssProperty => true,
                Class::HtmlAttribute | Class::SvgAttribute => row.path == Path::Css,
                _ => false,
            }
    })
}

/// How an animation targeting `attribute_name` (trimmed, lower-case) reads its `from`/`to`/`by`/`values`: CSS when it
/// names a CSS property in the inventory (SVG Animations L2 2.6: CSS properties are matched first), a URL when it names
/// an any-element URL attribute (`href`, `xlink:href`) or `src` (kept conservatively), else nothing to read.
pub(crate) fn animation_target(attribute_name: &str) -> Option<Path> {
    if css_target(attribute_name) {
        return Some(Path::Css);
    }
    if attribute_name == "src" {
        return Some(Path::Url);
    }
    match attribute_path("", attribute_name) {
        Some((Path::Url, _)) => Some(Path::Url),
        Some((Path::Css, _)) => Some(Path::Css),
        _ => None,
    }
}

/// The animation row for attribute `name` of animation element `tag`.
pub(crate) fn animation_path(tag: &str, name: &str) -> Option<Path> {
    CARRIERS
        .iter()
        .find(|row| {
            row.class == Class::SvgAnimation
                && element_matches(row, tag)
                && row.names.contains(&name)
        })
        .map(|row| row.path)
}

#[cfg(test)]
mod tests;
