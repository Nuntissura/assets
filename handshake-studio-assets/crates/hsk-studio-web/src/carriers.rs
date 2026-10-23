//! The URL carrier inventory: one table of every place a document hands a browser a URL to load (plus the identifiers and
//! obsolete attributes the gate reads conservatively), derived from the specification indices cited in each row. Every row
//! (and, for `link`, every `rel` keyword: `LINK_TYPES`, `PRELOAD_AS`) names the carrier's load context (`Load`): what a
//! browser does with the fetched resource. The load context is decided first, the URL's scheme and a `data:` payload's type
//! only then (`datauri::decide`); a reference to the document itself is harmless only where the context does not read the
//! document's text (`Load::reads_text`).
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

/// What a browser does with what it fetches from the carrier: decides how a `data:` payload is treated (`datauri`) and
/// whether a reference to the document itself is harmless (`Load::reads_text`). The load context is decided first; the
/// URL's scheme and the payload's type only then (`datauri::decide`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    /// Decoded as an image; SVG runs in secure (static or animated) mode and loads nothing further.
    Image,
    /// Decoded as a font (`link rel=preload as=font`): font data, never CSS, a document or a script.
    Font,
    /// Played as audio or video (a playlist type loads its segments).
    Media,
    /// A text track (WebVTT `STYLE` blocks hold CSS; the document itself would be read as WebVTT).
    Track,
    /// Rendered in a nested navigable (`iframe`, `frame`, `object`, `embed`) or navigated to (`a`, `area`, `form`,
    /// refresh).
    Document,
    /// An SVG resource document (`use`, paint servers, `filter`/`mask`/`clip-path`/`marker` references), a prefetched
    /// resource, or a CSS value whose property the scan does not track (the most restrictive CSS reading).
    Resource,
    /// A style sheet (`link rel=stylesheet`, `@import`; in quirks mode any same-origin type, so a `data:` payload is
    /// read as CSS).
    Stylesheet,
    /// A script: it executes whatever its type says (Fetch blocks only `audio/*`, `image/*`, `video/*` and `text/csv`
    /// for script-like destinations).
    Script,
    /// A structured document parsed whatever its type says, whose own URLs a browser loads (`link rel=manifest`:
    /// JSON; `rel=search`: an OpenSearch description).
    Manifest,
    /// A fire-and-forget request or connection hint (`ping`, `attributionsrc`, `rel=preconnect`): the response is not
    /// used.
    Beacon,
    /// A base URL for the other references (`base href`, `xml:base`, `object codebase`).
    Base,
    /// An inline document the gate denies unscanned (`srcdoc`, speculation rules, `meta http-equiv=link`).
    Inline,
    /// Not loaded by current engines (identifiers, obsolete attributes); read conservatively.
    None,
}

impl Load {
    pub const ALL: [Load; 13] = [
        Load::Image,
        Load::Font,
        Load::Media,
        Load::Track,
        Load::Document,
        Load::Resource,
        Load::Stylesheet,
        Load::Script,
        Load::Manifest,
        Load::Beacon,
        Load::Base,
        Load::Inline,
        Load::None,
    ];

    /// A browser that loads the document's own URL here reads the document's source text as the resource and acts on
    /// it: as a style sheet (quirks mode: the HTML Standard "Quirk" of the `stylesheet` link type and CSS Cascade 5
    /// `@import`: a same-origin resource whose type is not a style sheet type is assumed `text/css`), as a classic
    /// script (HTML "fetch a classic script": no MIME type check), as WebVTT, or as a manifest. A reference to the
    /// document itself in such a carrier is therefore never harmless (`Collector::decide`).
    pub const fn reads_text(self) -> bool {
        matches!(
            self,
            Load::Stylesheet | Load::Script | Load::Track | Load::Manifest
        )
    }
}

/// The load contexts of one carrier: a `link` whose `rel` names several keywords is processed once per keyword, an
/// animated URL attribute may belong to several candidate elements. Every context applies (the strictest decision wins:
/// `datauri::decide_all`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loads(u16);

impl Loads {
    pub const EMPTY: Loads = Loads(0);

    pub const fn of(load: Load) -> Self {
        Loads(1u16.wrapping_shl(load as u32))
    }

    #[must_use]
    pub const fn and(self, load: Load) -> Self {
        Loads(self.0 | Loads::of(load).0)
    }

    #[must_use]
    pub const fn union(self, other: Loads) -> Self {
        Loads(self.0 | other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `load` is one of the contexts.
    pub const fn contains(self, load: Load) -> bool {
        self.0 & Loads::of(load).0 != 0
    }

    pub fn iter(self) -> impl Iterator<Item = Load> {
        Load::ALL.into_iter().filter(move |l| self.contains(*l))
    }

    /// Any context reads the document's text (`Load::reads_text`: style sheet, script, text track, manifest).
    pub fn reads_text(self) -> bool {
        self.iter().any(Load::reads_text)
    }
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
    /// `<meta http-equiv=link>` content (a `Link` header value; engine-only, legacy): denied as an inline document.
    MetaLink,
    /// `<param value>` of a legacy plugin when its `name` is one of `PLUGIN_URL_PARAMS`: a URL handed to a plugin the
    /// gate cannot read, so it is reported and denied (relative: `NotGranted`, `data:`: `InlineDocument`).
    PluginParam,
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

/// How a `link` keyword loads its `href`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkLoad {
    Fixed(Load),
    /// `rel=preload`: the `as` attribute decides (`PRELOAD_AS`).
    ByAs,
}

/// The `link` keywords that load their `href` (HTML Standard 9 Oct 2026, links.html 4.6.8: "External Resource" types
/// `dns-prefetch icon manifest modulepreload pingback preconnect prefetch preload stylesheet`), the load context each one
/// fetches with, and the source. Every other keyword is a hyperlink, an annotation or the internal `expect`: nothing is
/// fetched until a navigation, so the `href` is read as `Load::Document`. A `link` with several keywords is processed
/// once per keyword (all contexts apply, `Loads`). Engine-only keywords are marked; their load is read conservatively.
pub(crate) const LINK_TYPES: &[(&str, LinkLoad, &str)] = &[
    (
        "stylesheet",
        LinkLoad::Fixed(Load::Stylesheet),
        "HTML 4.6.8.23 stylesheet (fetch and process: Quirk paragraph, quirks-mode same-origin resources are assumed text/css)",
    ),
    (
        "icon",
        LinkLoad::Fixed(Load::Image),
        "HTML 4.6.8.9 icon (linked resource fetch setup steps: destination \"image\")",
    ),
    (
        "apple-touch-icon",
        LinkLoad::Fixed(Load::Image),
        "HTML semantics.html registered extension apple-touch-icon (an icon)",
    ),
    (
        "apple-touch-icon-precomposed",
        LinkLoad::Fixed(Load::Image),
        "engine-only (WebKit): an icon",
    ),
    (
        "mask-icon",
        LinkLoad::Fixed(Load::Image),
        "HTML semantics.html registered extension mask-icon (an icon)",
    ),
    (
        "fluid-icon",
        LinkLoad::Fixed(Load::Image),
        "engine-only: an icon",
    ),
    (
        "manifest",
        LinkLoad::Fixed(Load::Manifest),
        "HTML 4.6.8.11 manifest (fetch setup: destination \"manifest\", mode cors; parsed as JSON whatever the type)",
    ),
    (
        "payment-method-manifest",
        LinkLoad::Fixed(Load::Manifest),
        "engine-only (payment handlers): a JSON manifest",
    ),
    (
        "search",
        LinkLoad::Fixed(Load::Manifest),
        "HTML 4.6.8.22 search (OpenSearch description documents are autodiscovered: XML parsed whatever the type, its URLs load)",
    ),
    (
        "modulepreload",
        LinkLoad::Fixed(Load::Script),
        "HTML 4.6.8.12 modulepreload (a module script fetch: script-like destination)",
    ),
    (
        "serviceworker",
        LinkLoad::Fixed(Load::Script),
        "engine-only (historical): a service worker script",
    ),
    (
        "preload",
        LinkLoad::ByAs,
        "HTML 4.6.8.20 preload (the as attribute is the destination)",
    ),
    (
        "prefetch",
        LinkLoad::Fixed(Load::Resource),
        "HTML 4.6.8.19 prefetch (cached for a followup navigation)",
    ),
    (
        "compression-dictionary",
        LinkLoad::Fixed(Load::Resource),
        "engine-only (Compression Dictionary Transport): a binary dictionary",
    ),
    (
        "dns-prefetch",
        LinkLoad::Fixed(Load::Beacon),
        "HTML 4.6.8.5 dns-prefetch (resolves the origin; nothing is fetched)",
    ),
    (
        "preconnect",
        LinkLoad::Fixed(Load::Beacon),
        "HTML 4.6.8.18 preconnect (connects to the origin; nothing is fetched)",
    ),
];

/// The preload destinations of `link rel=preload as=...`: HTML semantics.html (`as`: the preload destinations `fetch font
/// image script style track` and the module preload destinations, the script-like ones) and the Fetch destinations the
/// engines accept in addition (`audio video document embed object`: engine-only, read by their destination). A missing or
/// unknown value fetches nothing (HTML: an error for preload links) and is read as `Load::Resource`.
pub(crate) const PRELOAD_AS: &[(&str, Load)] = &[
    ("fetch", Load::Resource),
    ("font", Load::Font),
    ("image", Load::Image),
    ("script", Load::Script),
    ("style", Load::Stylesheet),
    ("track", Load::Track),
    ("worker", Load::Script),
    ("sharedworker", Load::Script),
    ("serviceworker", Load::Script),
    ("audioworklet", Load::Script),
    ("paintworklet", Load::Script),
    ("audio", Load::Media),
    ("video", Load::Media),
    ("document", Load::Document),
    ("embed", Load::Document),
    ("object", Load::Document),
];

/// `param name` values whose `value` is a URL a legacy plugin loads (Blink `HTMLObjectElement::ParametersForPlugin` hands
/// every `param` and `MapDataParamToSrc` to the plugin; Gecko `nsObjectLoadingContent`; the HTML Standard keeps `param`
/// only in obsolete.html as `HTMLParamElement`). Engine-only: the value is reported and denied.
pub(crate) const PLUGIN_URL_PARAMS: &[&str] = &["src", "movie", "code", "url", "data", "filename"];

/// HTML "ASCII whitespace": space, tab, LF, FF, CR.
fn is_ascii_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{c}' | '\r')
}

/// The load contexts of a `link` element's `href` from its `rel` and `as` values (as the attributes read: decoded). The
/// keywords are an ASCII case-insensitive set of ASCII-whitespace separated tokens; a `link` with none of the loading
/// keywords is a hyperlink (`Load::Document`).
pub(crate) fn link_loads(rel: &str, as_value: Option<&str>) -> Loads {
    let mut loads = Loads::EMPTY;
    for token in rel.split(is_ascii_ws).filter(|t| !t.is_empty()) {
        let token = token.to_ascii_lowercase();
        if let Some((_, how, _)) = LINK_TYPES.iter().find(|(rel, _, _)| *rel == token) {
            loads = loads.and(match how {
                LinkLoad::Fixed(load) => *load,
                LinkLoad::ByAs => preload_load(as_value),
            });
        }
    }
    if loads.is_empty() {
        Loads::of(Load::Document)
    } else {
        loads
    }
}

/// The load context of `rel=preload` with this `as` value (stripped of ASCII whitespace: a browser that does not strip
/// would fetch nothing, one that does is read the same way).
fn preload_load(as_value: Option<&str>) -> Load {
    let value = as_value
        .map(|v| v.trim_matches(is_ascii_ws).to_ascii_lowercase())
        .unwrap_or_default();
    PRELOAD_AS
        .iter()
        .find(|(name, _)| *name == value)
        .map_or(Load::Resource, |(_, load)| *load)
}

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
        // The default for a `link` without a loading keyword (a hyperlink); `rel` and `as` decide (`link_loads`, `LINK_TYPES`).
        load: Load::Document,
        source: "HTML Standard (9 Oct 2026) indices.html href (link); the load context follows rel and as (links.html 4.6.8: LINK_TYPES, PRELOAD_AS)",
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
        elements: &["img", "input", "image"],
        names: &["src"],
        path: Path::Url,
        load: Load::Image,
        source: "HTML Standard (9 Oct 2026) indices.html src (img; input); `image` (HTML: parsed as img; SVG `<image src>`: no engine loads it, kept from the R7 scan conservatively)",
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
        elements: &["meta"],
        names: &["content"],
        path: Path::MetaLink,
        load: Load::Inline,
        source: "engine-only, denied: <meta http-equiv=link content=\"<url>; rel=...\"> is not an HTML Standard pragma directive (4.2.5.3 lists content-language, content-type, default-style, refresh, set-cookie, x-ua-compatible, content-security-policy); legacy engines processed it as a Link header (current Gecko reads the Link header only from the HTTP channel: nsContentSink.cpp, HTMLMetaElement.cpp)",
        fixture: "meta_link",
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
        names: &["classid"],
        path: Path::Url,
        load: Load::None,
        source: OBSOLETE,
        fixture: "html_url",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["object"],
        names: &["codebase"],
        path: Path::Base,
        load: Load::Base,
        source: "HTML Standard (9 Oct 2026) obsolete.html 16.3 HTMLObjectElement codeBase (ReflectURL); engine-only as a base: Gecko nsObjectLoadingContent resolves the object's data against it (pref dom.object_embed.codebase.enabled), Blink treats it as a URL attribute; denied as a base like base href",
        fixture: "object_codebase",
    },
    Carrier {
        class: Class::HtmlAttribute,
        elements: &["param"],
        names: &["value"],
        path: Path::PluginParam,
        load: Load::Document,
        source: "HTML Standard (9 Oct 2026) obsolete.html HTMLParamElement (name, value); engine-only, denied: Blink HTMLObjectElement::ParametersForPlugin hands every param (data mapped to src) to the plugin, which loads it; read only when name is one of PLUGIN_URL_PARAMS",
        fixture: "plugin_param",
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
        elements: &["mglyph"],
        names: &["src"],
        path: Path::Url,
        load: Load::Image,
        source: "MathML 3 (W3C Recommendation) 3.2.1.2 mglyph src (URI, required: the glyph image); not in MathML Core, engine-only (Gecko renders it): read as an image URL",
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
    Carrier {
        class: Class::DataPayload,
        elements: &[],
        names: &[
            "text/css",
            "text/plain",
            "text/html",
            "text/javascript",
            "application/json",
            "application/octet-stream",
        ],
        path: Path::Data,
        load: Load::Script,
        source: "Fetch Standard 2.10 should response to request be blocked due to its MIME type (script-like destinations block only audio/*, image/*, video/* and text/csv); HTML \"fetch a classic script\" has no MIME type check: a script carrier executes every other type, text/css included (script src, svg script href, modulepreload, preload as=script)",
        fixture: "data_script",
    },
    Carrier {
        class: Class::DataPayload,
        elements: &[],
        names: &["text/css", "text/plain", "text/vtt", "image/svg+xml"],
        path: Path::Data,
        load: Load::Track,
        source: "HTML track element / WebVTT: a text track is read as WebVTT whatever the type could be (a STYLE block holds CSS that the CSS scan of a text/css reading would comment out)",
        fixture: "data_track",
    },
    Carrier {
        class: Class::DataPayload,
        elements: &[],
        names: &["text/css", "text/plain", "image/png", "application/json"],
        path: Path::Data,
        load: Load::Manifest,
        source: "HTML 4.6.8.11 manifest, 4.6.8.22 search (OpenSearch): a structured document parsed whatever the type, whose own URLs load and cannot be scanned: every data: payload denied",
        fixture: "data_manifest",
    },
    Carrier {
        class: Class::DataPayload,
        elements: &[],
        names: &["image/png", "font/woff2", "text/html", "text/plain"],
        path: Path::Data,
        load: Load::Font,
        source: "HTML 4.6.8.9 icon (destination image), 4.6.8.20 preload as=font|image: decoded as an image or a font, never as CSS (a binary payload is not tokenized)",
        fixture: "data_link_inert",
    },
];

fn element_matches(row: &Carrier, tag: &str) -> bool {
    row.elements.is_empty() || row.elements.contains(&tag)
}

/// Every attribute row naming attribute `name` (lower-case) on element `tag` (lower-case local name), in table order. An
/// attribute has several rows only where the value decides (`meta content`: `http-equiv` refresh or link); the dispatch
/// applies the row whose gate holds.
pub(crate) fn attribute_paths<'a>(
    tag: &'a str,
    name: &'a str,
) -> impl Iterator<Item = (Path, Load)> + 'a {
    CARRIERS
        .iter()
        .filter(move |row| {
            matches!(row.class, Class::HtmlAttribute | Class::SvgAttribute)
                && element_matches(row, tag)
                && row.names.contains(&name)
        })
        .map(|row| (row.path, row.load))
}

/// The path and load context of attribute `name` (lower-case) on element `tag` (lower-case local name): the first
/// attribute row naming both.
pub(crate) fn attribute_path(tag: &str, name: &str) -> Option<(Path, Load)> {
    attribute_paths(tag, name).next()
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
