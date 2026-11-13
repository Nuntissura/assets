//! The CSS property surface (lane SMB-WEB-CODE-1; [STU-WEB-060] to [STU-WEB-076]): the five categories, the closed
//! control-type and unit vocabularies, the curated property contracts the spec states, the numeric parameter
//! contracts with every bound labelled declared or Studio-declared, the transition vocabulary, the gradient swatch
//! constant, the SVG web-export defaults, declarative vendor-prefix rules, the media-query record, and the
//! SOURCE / MEDIA / SELECTOR / PROPERTIES edit that creates or changes a rule in a named media query of a named style
//! sheet in one call.
//!
//! Curated set: the spec states contracts for 54 properties (22 layout, 19 text, 4 border, 8 background, plus
//! `box-shadow`, placed in `background` as that category's ninth slot: the spec counts nine background slots and
//! names eight, so the placement is a reading, UNVERIFIED against the vendor (D5)). The vendor's 82 entries and 924
//! options are not all in the spec; the 28 entries it does not state are not invented. The display name of an entry
//! is its property name (the spec names none). The recognised vocabulary is the curated set, its per-side expansions,
//! the transition list and the vendor-prefixed forms here, not the vendor's 369 names; recognition never gates
//! authoring (`more` takes any declaration). The shipped vendor-prefix rule is the spec's transition prefix pair
//! (`-webkit-`, `-o-`, [STU-WEB-073]) on `transition`; the baseline's one-property, four-value-rule set is not in the
//! spec (UNVERIFIED).
//!
//! Style-sheet text is read the way a CSS tokenizer reads it: comments, strings with backslash escapes, escaped
//! characters outside strings and nested blocks are tokens, never substrings. The edit works on an HTML `style` element
//! (RAWTEXT: the source bytes are the sheet's bytes, newlines aside, so spans are exact); a foreign `style`, an
//! external sheet file (the web host holds one document) and nested `@media` levels below one are refused typed.
#![deny(clippy::arithmetic_side_effects)]

use crate::doc_types::{CodeError, Work};
use crate::dom::{ContentModel, Namespace, NodeId, NodeKind, WebDoc};
use crate::source_dom_patch::TextEdit;
use std::ops::Range;

/// [STU-WEB-061]: exactly five categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Layout,
    Text,
    Border,
    Background,
    More,
}

impl Category {
    pub const ALL: [Self; 5] = [Self::Layout, Self::Text, Self::Border, Self::Background, Self::More];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Layout => "layout",
            Self::Text => "text",
            Self::Border => "border",
            Self::Background => "background",
            Self::More => "more",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Layout => "Layout",
            Self::Text => "Text",
            Self::Border => "Border",
            Self::Background => "Background",
            Self::More => "More",
        }
    }

    /// The table's slot count; `None` for `more` (unbounded).
    pub const fn slots(self) -> Option<u32> {
        match self {
            Self::Layout | Self::Text => Some(22),
            Self::Border => Some(4),
            Self::Background => Some(9),
            Self::More => None,
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }
}

/// [STU-WEB-063], closed: exactly fourteen members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlType {
    Menu,
    MenuAndHotText,
    DoubleMenuAndHotText,
    HotTextValue,
    RangeHotText,
    MenuAndUrl,
    FontMenu,
    ColorWellWithTextEdit,
    GradientWellWithTextEdit,
    GroupedPictureButton,
    GroupedBoxControls,
    GroupedBorderControls,
    CompositeControlLayout,
    MultiControlLayout,
}

impl ControlType {
    pub const ALL: [Self; 14] = [
        Self::Menu,
        Self::MenuAndHotText,
        Self::DoubleMenuAndHotText,
        Self::HotTextValue,
        Self::RangeHotText,
        Self::MenuAndUrl,
        Self::FontMenu,
        Self::ColorWellWithTextEdit,
        Self::GradientWellWithTextEdit,
        Self::GroupedPictureButton,
        Self::GroupedBoxControls,
        Self::GroupedBorderControls,
        Self::CompositeControlLayout,
        Self::MultiControlLayout,
    ];

    pub const fn token(self) -> &'static str {
        match self {
            Self::Menu => "Menu",
            Self::MenuAndHotText => "MenuAndHotText",
            Self::DoubleMenuAndHotText => "DoubleMenuAndHotText",
            Self::HotTextValue => "HotTextValue",
            Self::RangeHotText => "RangeHotText",
            Self::MenuAndUrl => "MenuAndUrl",
            Self::FontMenu => "FontMenu",
            Self::ColorWellWithTextEdit => "ColorWellWithTextEdit",
            Self::GradientWellWithTextEdit => "GradientWellWithTextEdit",
            Self::GroupedPictureButton => "GroupedPictureButton",
            Self::GroupedBoxControls => "GroupedBoxControls",
            Self::GroupedBorderControls => "GroupedBorderControls",
            Self::CompositeControlLayout => "CompositeControlLayout",
            Self::MultiControlLayout => "MultiControlLayout",
        }
    }
}

/// [STU-WEB-064]: the fifteen length units, in order.
pub const LENGTH_UNITS: [&str; 15] = ["px", "pt", "pc", "in", "cm", "mm", "%", "em", "rem", "ex", "ch", "vw", "vh", "vmin", "vmax"];
/// [STU-WEB-064]: the separate resolution units.
pub const RESOLUTION_UNITS: [&str; 3] = ["dpi", "dpcm", "dppx"];
/// The first eleven length units (no viewport units).
pub const ELEVEN_UNITS: [&str; 11] = ["px", "pt", "pc", "in", "cm", "mm", "%", "em", "rem", "ex", "ch"];
const TEXT_INDENT_UNITS: [&str; 7] = ["px", "pt", "pc", "in", "cm", "mm", "%"];

/// One option of a curated entry, in declared order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opt {
    Keyword(&'static str),
    /// A unit set, expanded in place.
    Units(&'static [&'static str]),
    /// A group separator: rendered as a divider, never a value ([STU-WEB-062]).
    Separator,
}

/// The table's default: a value, `unset` or `undefined`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultValue {
    Value(&'static str),
    Unset,
    Undefined,
}

impl DefaultValue {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Value(v) => v,
            Self::Unset => "unset",
            Self::Undefined => "undefined",
        }
    }
}

/// [STU-WEB-062]: one curated property entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropertyEntry {
    pub property: &'static str,
    pub category: Category,
    pub control_type: ControlType,
    pub default_value: DefaultValue,
    /// `None` is the table's `—` (not applicable).
    pub supports_negative_values: Option<bool>,
    pub options: &'static [Opt],
}

impl PropertyEntry {
    /// The display name (the spec names none: the property itself).
    pub const fn display_name(&self) -> &'static str {
        self.property
    }

    /// The selectable options in declared order (unit sets expanded, separators kept as `None`).
    pub fn option_list(&self) -> Vec<Option<&'static str>> {
        let mut out = Vec::new();
        for o in self.options {
            match o {
                Opt::Keyword(k) => out.push(Some(*k)),
                Opt::Units(u) => out.extend(u.iter().map(|x| Some(*x))),
                Opt::Separator => out.push(None),
            }
        }
        out
    }

    pub fn option_count(&self) -> usize {
        self.option_list().iter().filter(|o| o.is_some()).count()
    }

    /// The units this entry offers.
    pub fn units(&self) -> Vec<&'static str> {
        self.options.iter().filter_map(|o| if let Opt::Units(u) = o { Some(u.iter().copied()) } else { None }).flatten().collect()
    }

    /// Whether a number with a unit is authorable through this control.
    pub fn numeric(&self) -> bool {
        matches!(self.control_type, ControlType::MenuAndHotText | ControlType::HotTextValue | ControlType::RangeHotText | ControlType::GroupedBoxControls)
    }
}

use Category as C;
use ControlType as T;
use DefaultValue::{Undefined, Unset, Value as V};
use Opt::{Keyword as K, Units as U};

const fn e(property: &'static str, category: Category, control_type: ControlType, default_value: DefaultValue, negative: Option<bool>, options: &'static [Opt]) -> PropertyEntry {
    PropertyEntry { property, category, control_type, default_value, supports_negative_values: negative, options }
}

const NO: Option<bool> = Some(false);
const YES: Option<bool> = Some(true);
const NA: Option<bool> = None;
const AUTO_UNITS: &[Opt] = &[K("auto"), U(&LENGTH_UNITS)];
const NORMAL_ELEVEN: &[Opt] = &[K("normal"), U(&ELEVEN_UNITS)];
const BORDER_STYLES: &[Opt] = &[K("none"), K("dotted"), K("dashed"), K("solid"), K("double"), K("groove"), K("ridge"), K("inset"), K("outset"), K("hidden")];
const BOX_ORIGINS: &[Opt] = &[K("padding-box"), K("border-box"), K("content-box")];
const OVERFLOW: &[Opt] = &[K("visible"), K("hidden"), K("scroll"), K("auto"), K("no-content"), K("no-display")];

/// The curated entries the spec states ([STU-WEB-065], [STU-WEB-068], [STU-WEB-069], [STU-WEB-070]; module docs).
pub static CURATED: [PropertyEntry; 54] = [
    e("width", C::Layout, T::MenuAndHotText, V("auto"), NO, AUTO_UNITS),
    e("height", C::Layout, T::MenuAndHotText, V("auto"), NO, AUTO_UNITS),
    e("min-width", C::Layout, T::MenuAndHotText, V("0px"), NO, &[U(&LENGTH_UNITS)]),
    e("min-height", C::Layout, T::MenuAndHotText, V("0px"), NO, &[U(&LENGTH_UNITS)]),
    e("max-width", C::Layout, T::MenuAndHotText, V("none"), NO, &[K("none"), U(&LENGTH_UNITS)]),
    e("max-height", C::Layout, T::MenuAndHotText, V("none"), NO, &[K("none"), U(&LENGTH_UNITS)]),
    e(
        "display",
        C::Layout,
        T::Menu,
        V("inline"),
        NA,
        &[
            K("inherit"),
            K("none"),
            K("block"),
            K("list-item"),
            K("inline"),
            K("inline-block"),
            K("inline-table"),
            K("table"),
            K("table-caption"),
            K("table-cell"),
            K("table-column"),
            K("table-column-group"),
            K("table-footer-group"),
            K("table-header-group"),
            K("table-row"),
            K("table-row-group"),
            K("run-in"),
            K("compact"),
            K("marker"),
        ],
    ),
    e("box-sizing", C::Layout, T::Menu, V("content-box"), NO, &[K("content-box"), K("border-box"), K("inherit")]),
    e("visibility", C::Layout, T::Menu, V("visible"), NA, &[K("inherit"), K("visible"), K("hidden"), K("collapse")]),
    e("float", C::Layout, T::GroupedPictureButton, Unset, NA, &[K("left"), K("right"), K("none")]),
    e("clear", C::Layout, T::GroupedPictureButton, Unset, NA, &[K("left"), K("right"), K("both"), K("none")]),
    e("overflow-x", C::Layout, T::Menu, V("visible"), NA, OVERFLOW),
    e("overflow-y", C::Layout, T::Menu, V("visible"), NA, OVERFLOW),
    e("position", C::Layout, T::Menu, V("static"), NA, &[K("static"), K("absolute"), K("fixed"), K("relative")]),
    e("z-index", C::Layout, T::HotTextValue, V("auto"), NA, &[K("auto")]),
    e("opacity", C::Layout, T::RangeHotText, V("1"), NA, &[]),
    e("margin", C::Layout, T::GroupedBoxControls, Unset, YES, AUTO_UNITS),
    e("padding", C::Layout, T::GroupedBoxControls, Unset, NO, AUTO_UNITS),
    e("top", C::Layout, T::GroupedBoxControls, Unset, YES, AUTO_UNITS),
    e("right", C::Layout, T::GroupedBoxControls, Unset, YES, AUTO_UNITS),
    e("bottom", C::Layout, T::GroupedBoxControls, Unset, YES, AUTO_UNITS),
    e("left", C::Layout, T::GroupedBoxControls, Unset, YES, AUTO_UNITS),
    e("color", C::Text, T::ColorWellWithTextEdit, Undefined, NA, &[]),
    e("font-family", C::Text, T::FontMenu, V("default font"), NA, &[K("inherit")]),
    e("font-style", C::Text, T::Menu, V("normal"), NA, &[K("normal"), K("italic"), K("oblique")]),
    e("font-variant", C::Text, T::Menu, V("normal"), NA, &[K("normal"), K("small-caps")]),
    e(
        "font-weight",
        C::Text,
        T::Menu,
        V("normal"),
        NA,
        &[K("normal"), K("bold"), K("bolder"), K("lighter"), K("100"), K("200"), K("300"), K("400"), K("500"), K("600"), K("700"), K("800"), K("900")],
    ),
    e(
        "font-size",
        C::Text,
        T::MenuAndHotText,
        V("medium"),
        NO,
        &[U(&ELEVEN_UNITS), K("xx-small"), K("x-small"), K("small"), K("medium"), K("large"), K("x-large"), K("xx-large"), K("smaller"), K("larger")],
    ),
    e("line-height", C::Text, T::MenuAndHotText, V("normal"), NA, &[U(&ELEVEN_UNITS), K("normal")]),
    e("letter-spacing", C::Text, T::MenuAndHotText, V("normal"), NA, NORMAL_ELEVEN),
    e("word-spacing", C::Text, T::MenuAndHotText, V("normal"), NA, NORMAL_ELEVEN),
    e("text-indent", C::Text, T::MenuAndHotText, V("0px"), NA, &[U(&TEXT_INDENT_UNITS)]),
    e("white-space", C::Text, T::Menu, V("normal"), NA, &[K("normal"), K("nowrap"), K("pre"), K("pre-line"), K("pre-wrap")]),
    e(
        "vertical-align",
        C::Text,
        T::MenuAndHotText,
        V("baseline"),
        NA,
        &[U(&ELEVEN_UNITS), K("baseline"), K("sub"), K("super"), K("top"), K("text-top"), K("middle"), K("bottom"), K("text-bottom")],
    ),
    e("text-align", C::Text, T::GroupedPictureButton, Unset, NA, &[K("left"), K("center"), K("right"), K("justify")]),
    e("text-decoration", C::Text, T::GroupedPictureButton, Unset, NA, &[K("none"), K("underline"), K("overline"), K("line-through")]),
    e("text-transform", C::Text, T::GroupedPictureButton, Unset, NA, &[K("none"), K("capitalize"), K("uppercase"), K("lowercase")]),
    e(
        "list-style-type",
        C::Text,
        T::Menu,
        Undefined,
        NA,
        &[
            K("none"),
            K("armenian"),
            K("circle"),
            K("cjk-decimal"),
            K("decimal"),
            K("decimal-leading-zero"),
            K("disc"),
            K("georgian"),
            K("hebrew"),
            K("hiragana"),
            K("hiragana-iroha"),
            K("katakana"),
            K("katakana-iroha"),
            K("lower-alpha"),
            K("lower-greek"),
            K("lower-latin"),
            K("lower-roman"),
            K("square"),
            K("upper-alpha"),
            K("upper-latin"),
            K("upper-roman"),
        ],
    ),
    e("list-style-position", C::Text, T::GroupedPictureButton, Unset, NA, &[K("inside"), K("outside")]),
    e("list-style-image", C::Text, T::MenuAndUrl, V("none"), NA, &[K("url"), K("none")]),
    e("text-shadow", C::Text, T::MultiControlLayout, Unset, NA, &[]),
    e("border", C::Border, T::GroupedBorderControls, Unset, NO, &[K("thin"), K("medium"), K("thick"), U(&ELEVEN_UNITS), Opt::Separator, Opt::Keyword("none"), K("dotted"), K("dashed"), K("solid"), K("double"), K("groove"), K("ridge"), K("inset"), K("outset"), K("hidden")]),
    e("border-radius", C::Border, T::GroupedBoxControls, Unset, NO, &[U(&LENGTH_UNITS)]),
    e("border-collapse", C::Border, T::GroupedPictureButton, Unset, NA, &[K("collapse"), K("separate")]),
    e("border-spacing", C::Border, T::DoubleMenuAndHotText, V("0px"), NO, &[U(&LENGTH_UNITS)]),
    e("background-color", C::Background, T::ColorWellWithTextEdit, Unset, NA, &[]),
    e("background-image", C::Background, T::MultiControlLayout, V("url"), NA, &[K("url"), K("none")]),
    e("background-position", C::Background, T::DoubleMenuAndHotText, V("0% 0%"), NA, &[K("left"), K("right"), K("center"), U(&ELEVEN_UNITS), Opt::Separator, K("top"), K("bottom"), K("center"), U(&ELEVEN_UNITS)]),
    e("background-size", C::Background, T::DoubleMenuAndHotText, V("auto auto"), NA, &[K("auto"), K("cover"), K("contain"), U(&ELEVEN_UNITS)]),
    e("background-clip", C::Background, T::Menu, V("border-box"), NA, BOX_ORIGINS),
    e("background-origin", C::Background, T::Menu, V("padding-box"), NA, BOX_ORIGINS),
    e("background-repeat", C::Background, T::GroupedPictureButton, V("repeat"), NA, &[K("repeat"), K("repeat-x"), K("repeat-y"), K("no-repeat")]),
    e("background-attachment", C::Background, T::Menu, V("scroll"), NA, &[K("scroll"), K("fixed")]),
    e("box-shadow", C::Background, T::MultiControlLayout, Unset, NA, &[]),
];

/// The per-side border width and style families ([STU-WEB-069]): the `border` composite's sub-controls.
pub const BORDER_SIDE_STYLES: &[Opt] = BORDER_STYLES;

pub fn property(name: &str) -> Option<&'static PropertyEntry> {
    CURATED.iter().find(|p| p.property.eq_ignore_ascii_case(name))
}

pub fn properties_in(category: Category) -> Vec<&'static PropertyEntry> {
    CURATED.iter().filter(|p| p.category == category).collect()
}

/// Whether a bound is declared by the source or by Studio ([STU-WEB-067]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundSource {
    Declared,
    StudioDeclared,
}

impl BoundSource {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::StudioDeclared => "studio_declared",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bound {
    pub value: f64,
    pub source: BoundSource,
}

const fn declared(value: f64) -> Bound {
    Bound { value, source: BoundSource::Declared }
}

const fn studio(value: f64) -> Bound {
    Bound { value, source: BoundSource::StudioDeclared }
}

/// A numeric parameter contract: every field stored separately, the unit never fused into the value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumericContract {
    pub hard_min: Bound,
    pub hard_max: Bound,
    pub soft_min: Bound,
    pub soft_max: Bound,
    pub default: DefaultValue,
    pub unit: &'static str,
    /// Decimal places.
    pub precision: u8,
    pub step: f64,
    pub coarse_step: f64,
    pub fine_step: f64,
}

/// [STU-WEB-066].
pub const OPACITY_CONTRACT: NumericContract = NumericContract { hard_min: declared(0.0), hard_max: declared(1.0), soft_min: declared(0.0), soft_max: declared(1.0), default: V("1"), unit: "ratio", precision: 2, step: 0.01, coarse_step: 0.1, fine_step: 0.001 };

/// [STU-WEB-075]: `decimal_precision` of SVG written into a web document.
pub const SVG_DECIMAL_PRECISION_CONTRACT: NumericContract = NumericContract { hard_min: declared(0.0), hard_max: studio(10.0), soft_min: declared(0.0), soft_max: declared(6.0), default: V("1"), unit: "decimal_places", precision: 0, step: 1.0, coarse_step: 1.0, fine_step: 1.0 };

/// [STU-WEB-075] defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SvgWebExportDefaults {
    pub trim_to_art_bounds: bool,
    pub responsive: bool,
    pub decimal_precision: u8,
}

pub const SVG_WEB_EXPORT_DEFAULTS: SvgWebExportDefaults = SvgWebExportDefaults { trim_to_art_bounds: true, responsive: true, decimal_precision: 1 };

/// [STU-WEB-074]: the saved-swatch strip's capacity (the shipped constant, kept).
pub const GRADIENT_SAVED_SWATCHES_MAX: usize = 5;

/// [STU-WEB-067]: the contract of a numeric field of `entry` (`None` for a non-numeric control). `opacity` has its own
/// ([STU-WEB-066]); `z-index` is an integer with no unit and no declared bound (Studio-declared like the generic ones).
pub fn numeric_contract(entry: &PropertyEntry) -> Option<NumericContract> {
    if entry.property == "opacity" {
        return Some(OPACITY_CONTRACT);
    }
    if !matches!(entry.control_type, ControlType::MenuAndHotText | ControlType::DoubleMenuAndHotText | ControlType::GroupedBoxControls | ControlType::GroupedBorderControls | ControlType::HotTextValue) {
        return None;
    }
    let negative = entry.supports_negative_values != Some(false);
    Some(NumericContract {
        hard_min: if negative { studio(-100_000.0) } else { declared(0.0) },
        hard_max: studio(100_000.0),
        soft_min: if negative { studio(-1000.0) } else { declared(0.0) },
        soft_max: studio(1000.0),
        default: entry.default_value,
        unit: if entry.control_type == ControlType::HotTextValue { "integer" } else { "selected_unit" },
        precision: if entry.control_type == ControlType::HotTextValue { 0 } else { 3 },
        step: 1.0,
        coarse_step: 10.0,
        fine_step: 0.1,
    })
}

/// A number and a unit for `entry` as declaration text: the unit must be one the entry offers (none for `opacity` and
/// `z-index`), the number inside the hard bounds, negatives only where the entry permits them; rounded to the
/// contract's precision, trailing zeros dropped. Composite controls take text, not a number (`value_needs_text`).
pub fn compose_number(entry: &PropertyEntry, number: f64, unit: Option<&str>) -> Result<String, CodeError> {
    if !entry.numeric() {
        return Err(CodeError::new("value_needs_text"));
    }
    let contract = numeric_contract(entry).ok_or(CodeError::new("value_needs_text"))?;
    if !number.is_finite() {
        return Err(CodeError::new("value_not_finite"));
    }
    if number < contract.hard_min.value || number > contract.hard_max.value {
        return Err(CodeError::new("value_out_of_range"));
    }
    let unitless = matches!(entry.control_type, ControlType::RangeHotText | ControlType::HotTextValue);
    match (unit, unitless) {
        (Some(_), true) => return Err(CodeError::new("unit_not_offered")),
        (Some(u), false) if !entry.units().contains(&u) => return Err(CodeError::new("unit_not_offered")),
        (None, false) if number != 0.0 => return Err(CodeError::new("unit_missing")),
        _ => {}
    }
    let text = format!("{number:.prec$}", prec = usize::from(contract.precision));
    let text = if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_string() } else { text };
    let text = if text == "-0" { "0".to_string() } else { text };
    Ok(format!("{text}{}", unit.unwrap_or("")))
}

/// [STU-WEB-073]: the four closed lists.
pub const ANIMATABLE_PROPERTIES: [&str; 49] = [
    "background-color",
    "background-image",
    "background-position",
    "border-bottom-color",
    "border-bottom-width",
    "border-color",
    "border-left-color",
    "border-left-width",
    "border-right-color",
    "border-right-width",
    "border-spacing",
    "border-top-color",
    "border-top-width",
    "border-width",
    "bottom",
    "color",
    "crop",
    "font-size",
    "font-weight",
    "height",
    "left",
    "letter-spacing",
    "line-height",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "opacity",
    "outline-color",
    "outline-offset",
    "outline-width",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "right",
    "text-indent",
    "text-shadow",
    "top",
    "transform",
    "vertical-align",
    "visibility",
    "width",
    "word-spacing",
    "z-index",
];
pub const TRANSITION_PSEUDO_CLASSES: [&str; 8] = ["active", "checked", "disabled", "enabled", "focus", "hover", "indeterminate", "target"];
pub const TIMING_FUNCTIONS: [&str; 6] = ["linear", "ease", "ease-in", "ease-out", "ease-in-out", "cubic-bezier(x1,y1,x2,y2)"];
pub const TRANSITION_VENDOR_PREFIXES: [&str; 2] = ["-webkit-", "-o-"];

/// [STU-WEB-072]: one emission of a prefix rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefixEmission {
    pub target_vendor: String,
    pub emitted_property: String,
    /// `None`: the base declaration's value.
    pub emitted_value: Option<String>,
}

/// [STU-WEB-072]: per property (and, when set, per value) the declarations emitted beside it. Removing the base
/// declaration removes its emissions in the same edit (the delete sentinel is that removal, see [`rule_edit`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefixRule {
    pub property: String,
    pub value: Option<String>,
    pub emissions: Vec<PrefixEmission>,
}

/// The shipped rule (module docs): `transition` emits `-webkit-transition` and `-o-transition` with its value.
pub fn shipped_prefix_rules() -> Vec<PrefixRule> {
    vec![PrefixRule {
        property: "transition".into(),
        value: None,
        emissions: TRANSITION_VENDOR_PREFIXES.iter().map(|p| PrefixEmission { target_vendor: p.trim_matches('-').to_string(), emitted_property: format!("{p}transition"), emitted_value: None }).collect(),
    }]
}

/// [STU-WEB-071]: the recognised names (module docs), sorted and unique.
pub fn recognised_properties() -> Vec<String> {
    let mut out: Vec<String> = CURATED.iter().map(|p| p.property.to_string()).collect();
    out.extend(ANIMATABLE_PROPERTIES.iter().filter(|p| **p != "crop").map(|p| (*p).to_string()));
    for side in ["top", "right", "bottom", "left"] {
        for part in ["width", "style", "color"] {
            out.push(format!("border-{side}-{part}"));
        }
        out.push(format!("border-{side}"));
    }
    for corner in ["top-left", "top-right", "bottom-right", "bottom-left"] {
        out.push(format!("border-{corner}-radius"));
    }
    out.extend(["border-style", "transition", "transition-property", "transition-duration", "transition-timing-function", "transition-delay", "background", "font", "list-style", "outline", "outline-style", "overflow"].map(str::to_string));
    for rule in shipped_prefix_rules() {
        out.extend(rule.emissions.into_iter().map(|e| e.emitted_property));
    }
    out.sort();
    out.dedup();
    out
}

pub fn is_recognised(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    recognised_properties().binary_search(&lower).is_ok()
}

// ---------------------------------------------------------------------------------------------------------------------
// Style-sheet scanning (tokens, never substrings).

/// Most items one sheet scan keeps.
const MAX_ITEMS: usize = 8192;
const MAX_CSS_DEPTH: usize = 16;
/// Longest selector, media query or declaration value an edit writes.
pub const MAX_CSS_TEXT: usize = 4096;
/// Most declarations one edit sets or removes.
pub const MAX_EDIT_DECLARATIONS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    /// `@media` with a block (its children scanned).
    Media,
    /// Another at-rule with a block (`@font-face`, `@keyframes`, ...): kept opaque.
    OtherAt,
    /// An at-rule ending in `;` (`@import`, `@charset`).
    Statement,
    /// A qualified rule (selector and declaration block).
    Rule,
}

/// One item of a scanned sheet; ranges are byte offsets in the scanned text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CssItem {
    pub kind: ItemKind,
    /// From the first byte of the item to the end of its prelude (before `{` or `;`), trimmed.
    pub prelude: Range<usize>,
    /// Inside the braces (empty for a statement).
    pub block: Range<usize>,
    /// The whole item: prelude start to the closing brace or `;` included.
    pub whole: Range<usize>,
    pub children: Vec<CssItem>,
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// End of the comment starting at `i` (`/*`), or an error.
fn comment_end(b: &[u8], i: usize) -> Result<usize, CodeError> {
    let mut j = i.saturating_add(2);
    while j.saturating_add(1) < b.len() {
        if b[j] == b'*' && b[j.saturating_add(1)] == b'/' {
            return Ok(j.saturating_add(2));
        }
        j = j.saturating_add(1);
    }
    Err(CodeError::at("css_unterminated_comment", i..b.len()))
}

/// End of the string starting at `i`; a raw newline ends it unterminated (CSS Syntax 3, 4.3.5).
fn string_end(b: &[u8], i: usize) -> Result<usize, CodeError> {
    let q = b[i];
    let mut j = i.saturating_add(1);
    while let Some(&c) = b.get(j) {
        match c {
            b'\\' => j = j.saturating_add(2),
            b'\n' | b'\r' | 0x0c => return Err(CodeError::at("css_unterminated_string", i..j)),
            c if c == q => return Ok(j.saturating_add(1)),
            _ => j = j.saturating_add(1),
        }
    }
    Err(CodeError::at("css_unterminated_string", i..b.len()))
}

/// Position of the first top-level byte in `stops` at or after `from` (outside comments, strings, escapes and
/// parentheses/brackets), before `end`.
fn find_top(b: &[u8], from: usize, end: usize, stops: &[u8]) -> Result<Option<usize>, CodeError> {
    let (mut j, mut depth) = (from, 0usize);
    while j < end {
        let c = b[j];
        match c {
            b'/' if b.get(j.saturating_add(1)) == Some(&b'*') => j = comment_end(b, j)?,
            b'"' | b'\'' => j = string_end(b, j)?,
            b'\\' => j = j.saturating_add(2),
            b'(' | b'[' => {
                depth = depth.saturating_add(1);
                j = j.saturating_add(1);
            }
            b')' | b']' => {
                depth = depth.saturating_sub(1);
                j = j.saturating_add(1);
            }
            c if depth == 0 && stops.contains(&c) => return Ok(Some(j)),
            _ => j = j.saturating_add(1),
        }
    }
    Ok(None)
}

/// The `}` matching the `{` at `open`.
fn block_close(b: &[u8], open: usize, end: usize) -> Result<usize, CodeError> {
    let (mut j, mut depth) = (open.saturating_add(1), 0usize);
    while j < end {
        match b[j] {
            b'/' if b.get(j.saturating_add(1)) == Some(&b'*') => j = comment_end(b, j)?,
            b'"' | b'\'' => j = string_end(b, j)?,
            b'\\' => j = j.saturating_add(2),
            b'{' => {
                depth = depth.saturating_add(1);
                if depth > MAX_CSS_DEPTH {
                    return Err(CodeError::at("css_too_deep", open..j));
                }
                j = j.saturating_add(1);
            }
            b'}' if depth == 0 => return Ok(j),
            b'}' => {
                depth = depth.saturating_sub(1);
                j = j.saturating_add(1);
            }
            _ => j = j.saturating_add(1),
        }
    }
    Err(CodeError::at("css_unterminated_block", open..end))
}

fn trim(b: &[u8], mut r: Range<usize>) -> Range<usize> {
    while r.start < r.end && is_ws(b[r.start]) {
        r.start = r.start.saturating_add(1);
    }
    while r.end > r.start && is_ws(b[r.end.saturating_sub(1)]) {
        r.end = r.end.saturating_sub(1);
    }
    r
}

/// Scans `text[range]` into items (module docs). `budget` bounds the items of the whole scan.
pub fn scan_css(text: &str, range: Range<usize>) -> Result<Vec<CssItem>, CodeError> {
    let mut budget = MAX_ITEMS;
    scan_level(text.as_bytes(), range, 0, &mut budget)
}

fn scan_level(b: &[u8], range: Range<usize>, level: usize, budget: &mut usize) -> Result<Vec<CssItem>, CodeError> {
    if level > MAX_CSS_DEPTH {
        return Err(CodeError::at("css_too_deep", range));
    }
    let mut out = Vec::new();
    let mut i = range.start;
    let end = range.end.min(b.len());
    while i < end {
        let c = b[i];
        if is_ws(c) || c == b';' {
            i = i.saturating_add(1);
            continue;
        }
        if c == b'/' && b.get(i.saturating_add(1)) == Some(&b'*') {
            i = comment_end(b, i)?;
            continue;
        }
        // CDO / CDC tokens at the top level of a sheet are ignored (CSS Syntax 3, 5.4.1).
        if b[i..end].starts_with(b"<!--") {
            i = i.saturating_add(4);
            continue;
        }
        if b[i..end].starts_with(b"-->") {
            i = i.saturating_add(3);
            continue;
        }
        *budget = budget.checked_sub(1).ok_or(CodeError::at("css_too_many_items", i..end))?;
        if c == b'}' {
            return Err(CodeError::at("css_unbalanced_brace", i..i.saturating_add(1)));
        }
        let stop = find_top(b, i, end, if c == b'@' { b"{;" } else { b"{;}" })?;
        let Some(stop) = stop else {
            return Err(CodeError::at("css_unterminated_block", i..end));
        };
        if c != b'@' && b[stop] != b'{' {
            return Err(CodeError::at("css_malformed_rule", i..stop));
        }
        let prelude = trim(b, i..stop);
        if b[stop] == b';' {
            out.push(CssItem { kind: ItemKind::Statement, prelude, block: stop..stop, whole: i..stop.saturating_add(1), children: Vec::new() });
            i = stop.saturating_add(1);
            continue;
        }
        let close = block_close(b, stop, end)?;
        let block = stop.saturating_add(1)..close;
        let kind = if c == b'@' {
            if at_keyword_is(b, i, "media") { ItemKind::Media } else { ItemKind::OtherAt }
        } else {
            ItemKind::Rule
        };
        let children = if kind == ItemKind::Media { scan_level(b, block.clone(), level.saturating_add(1), budget)? } else { Vec::new() };
        out.push(CssItem { kind, prelude, block, whole: i..close.saturating_add(1), children });
        i = close.saturating_add(1);
    }
    Ok(out)
}

/// The at-keyword at `i` (`@...`), escapes decoded, and the index after it.
fn at_keyword(b: &[u8], i: usize) -> (String, usize) {
    let mut j = i.saturating_add(1);
    let mut word = String::new();
    while j < b.len() && word.len() <= 32 {
        let c = b[j];
        if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' {
            word.push(char::from(c));
            j = j.saturating_add(1);
        } else if c == b'\\' {
            let (ch, next) = decode_escape(b, j);
            word.push(ch);
            j = next;
        } else {
            break;
        }
    }
    (word, j)
}

/// Whether the at-keyword at `i` is `name`, ASCII case-insensitive.
fn at_keyword_is(b: &[u8], i: usize, name: &str) -> bool {
    at_keyword(b, i).0.eq_ignore_ascii_case(name)
}

/// The query text of a `@media` item (its prelude after the at-keyword).
pub fn media_query_text<'a>(text: &'a str, item: &CssItem) -> &'a str {
    let (_, after) = at_keyword(text.as_bytes(), item.prelude.start);
    text.get(after.min(item.prelude.end)..item.prelude.end).unwrap_or("")
}

/// The code point of the escape at `i` (`\`) and the index after it (CSS Syntax 3, 4.3.7): up to six hex digits and
/// one following whitespace, else the next character itself.
fn decode_escape(b: &[u8], i: usize) -> (char, usize) {
    let mut j = i.saturating_add(1);
    let mut value: u32 = 0;
    let mut digits = 0usize;
    while digits < 6 && j < b.len() && b[j].is_ascii_hexdigit() {
        value = value.saturating_mul(16).saturating_add(char::from(b[j]).to_digit(16).unwrap_or(0));
        digits = digits.saturating_add(1);
        j = j.saturating_add(1);
    }
    if digits > 0 {
        if j < b.len() && is_ws(b[j]) {
            j = j.saturating_add(1);
        }
        let ch = char::from_u32(value).filter(|c| *c != '\0').unwrap_or('\u{fffd}');
        return (ch, j);
    }
    // One character, read from at most its own four bytes (review F2: never the rest of the buffer).
    let width = match b.get(j) {
        Some(&c) if c < 0x80 => 1,
        Some(&c) if c >= 0xf0 => 4,
        Some(&c) if c >= 0xe0 => 3,
        Some(_) => 2,
        None => return ('\u{fffd}', j),
    };
    match b.get(j..j.saturating_add(width)).and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| s.chars().next()) {
        Some(ch) => (ch, j.saturating_add(ch.len_utf8())),
        None => ('\u{fffd}', j.saturating_add(1)),
    }
}

/// `text` as compared for identity: comments removed, escapes decoded, whitespace runs one space, trimmed, ASCII
/// lower case when `fold` (media queries) and case kept otherwise (selectors: class and id names are case-sensitive).
pub fn normalised(text: &str, fold: bool) -> Result<String, CodeError> {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut space = false;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i.saturating_add(1)) == Some(&b'*') {
            i = comment_end(b, i)?;
            space = true;
            continue;
        }
        if is_ws(c) {
            space = true;
            i = i.saturating_add(1);
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        if c == b'\\' {
            let (ch, next) = decode_escape(b, i);
            out.push(ch);
            i = next;
            continue;
        }
        if c == b'"' || c == b'\'' {
            let end = string_end(b, i)?;
            out.push_str(text.get(i..end).unwrap_or(""));
            i = end;
            continue;
        }
        let ch = text.get(i..).and_then(|s| s.chars().next()).unwrap_or('\u{fffd}');
        out.push(if fold { ch.to_ascii_lowercase() } else { ch });
        i = i.saturating_add(ch.len_utf8().max(1));
    }
    Ok(out)
}

/// One declaration of a block; ranges in the scanned text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationSpan {
    /// The property, ASCII lower case (escapes decoded).
    pub property: String,
    pub property_range: Range<usize>,
    /// The value, trimmed, `!important` included.
    pub value_range: Range<usize>,
    /// From the property's first byte to the end of the value (no `;`).
    pub whole: Range<usize>,
    /// After the terminating `;`, or the value's end when there is none.
    pub end: usize,
    pub important: bool,
}

/// The declarations of `text[block]` (comments skipped; chunks without `:` or a property ignored, as a CSS parser
/// drops them).
pub fn declarations(text: &str, block: Range<usize>) -> Result<Vec<DeclarationSpan>, CodeError> {
    let b = text.as_bytes();
    let end = block.end.min(b.len());
    let mut out = Vec::new();
    let mut i = block.start;
    while i < end {
        while i < end && (is_ws(b[i]) || b[i] == b';') {
            i = i.saturating_add(1);
        }
        if i < end && b[i] == b'/' && b.get(i.saturating_add(1)) == Some(&b'*') {
            i = comment_end(b, i)?;
            continue;
        }
        if i >= end {
            break;
        }
        let stop = find_top(b, i, end, b";{")?.unwrap_or(end);
        if stop < end && b[stop] == b'{' {
            return Err(CodeError::at("css_nested_block", i..stop));
        }
        let chunk = i..stop;
        if let Some(colon) = find_top(b, i, stop, b":")? {
            let name = trim(b, i..colon);
            let property = normalised(text.get(name.clone()).unwrap_or(""), true)?;
            if !property.is_empty() && property.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
                let value = trim(b, colon.saturating_add(1)..stop);
                let raw = text.get(value.clone()).unwrap_or("").to_ascii_lowercase();
                let important = raw.trim_end().ends_with("important") && raw.contains('!');
                out.push(DeclarationSpan { property, property_range: name.clone(), value_range: value.clone(), whole: name.start..value.end.max(name.end), end: if stop < end { stop.saturating_add(1) } else { trim(b, chunk).end }, important });
            }
        }
        i = stop.saturating_add(1);
        if out.len() > MAX_ITEMS {
            return Err(CodeError::at("css_too_many_items", block));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------------------
// Media-query record ([STU-WEB-076] to [STU-WEB-078]).

/// [STU-WEB-077], closed.
pub const MEDIA_TYPES: [&str; 8] = ["screen", "print", "handheld", "aural", "braille", "projection", "tty", "tv"];

/// The value domain of a media feature ([STU-WEB-078]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureDomain {
    MediaType,
    Orientation,
    Length,
    Resolution,
    Ratio,
}

/// [STU-WEB-078], closed: twenty-three features.
pub const MEDIA_FEATURES: [(&str, FeatureDomain); 23] = [
    ("media", FeatureDomain::MediaType),
    ("orientation", FeatureDomain::Orientation),
    ("min-width", FeatureDomain::Length),
    ("max-width", FeatureDomain::Length),
    ("width", FeatureDomain::Length),
    ("min-height", FeatureDomain::Length),
    ("max-height", FeatureDomain::Length),
    ("height", FeatureDomain::Length),
    ("min-device-width", FeatureDomain::Length),
    ("max-device-width", FeatureDomain::Length),
    ("device-width", FeatureDomain::Length),
    ("min-device-height", FeatureDomain::Length),
    ("max-device-height", FeatureDomain::Length),
    ("device-height", FeatureDomain::Length),
    ("min-resolution", FeatureDomain::Resolution),
    ("max-resolution", FeatureDomain::Resolution),
    ("resolution", FeatureDomain::Resolution),
    ("min-aspect-ratio", FeatureDomain::Ratio),
    ("max-aspect-ratio", FeatureDomain::Ratio),
    ("aspect-ratio", FeatureDomain::Ratio),
    ("min-device-aspect-ratio", FeatureDomain::Ratio),
    ("max-device-aspect-ratio", FeatureDomain::Ratio),
    ("device-aspect-ratio", FeatureDomain::Ratio),
];

/// One condition `{feature, operator, value, unit?}`; `operator` is `:` for the colon form or one of `<`, `<=`, `>`,
/// `>=`, `=` for a range form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaCondition {
    pub feature: &'static str,
    pub operator: &'static str,
    pub value: String,
    pub unit: Option<&'static str>,
}

/// [STU-WEB-076]: `{media_type?, conditions[], rules[]}`; `rules` are the normalised selectors of the block's rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaQueryRecord {
    pub media_type: Option<&'static str>,
    pub conditions: Vec<MediaCondition>,
    pub rules: Vec<String>,
}

const MAX_QUERIES: usize = 64;
const MAX_CONDITIONS: usize = 32;

/// The records of a media query list (one per comma-separated query). `all` is an absent media type (it restricts
/// nothing); `only` is accepted; `not`, `or`, nested parentheses and features outside the closed set are refused
/// typed, never guessed.
pub fn parse_media_query_list(text: &str) -> Result<Vec<MediaQueryRecord>, CodeError> {
    if text.len() > MAX_CSS_TEXT {
        return Err(CodeError::new("css_text_too_long"));
    }
    let norm = normalised(text, true)?;
    let mut out = Vec::new();
    for query in norm.split(',') {
        if out.len() >= MAX_QUERIES {
            return Err(CodeError::new("media_query_too_long"));
        }
        out.push(parse_query(query.trim())?);
    }
    Ok(out)
}

/// A query list's records without their rules: media type and conditions per query.
type MediaShape = Vec<(Option<&'static str>, Vec<MediaCondition>)>;

/// The query list's shape (`None` when it is outside the record grammar).
fn media_shape(text: &str) -> Option<MediaShape> {
    parse_media_query_list(text).ok().map(|list| list.into_iter().map(|r| (r.media_type, r.conditions)).collect())
}

fn parse_query(q: &str) -> Result<MediaQueryRecord, CodeError> {
    if q.is_empty() {
        return Err(CodeError::new("media_query_empty"));
    }
    let mut record = MediaQueryRecord { media_type: None, conditions: Vec::new(), rules: Vec::new() };
    let mut rest = q;
    if !rest.starts_with('(') {
        let (word, tail) = rest.split_once(' ').unwrap_or((rest, ""));
        let (word, tail) = if word == "only" { tail.split_once(' ').unwrap_or((tail, "")) } else { (word, tail) };
        match word {
            "not" => return Err(CodeError::new("media_query_negation_unsupported")),
            "all" => {}
            w => record.media_type = Some(MEDIA_TYPES.iter().find(|t| **t == w).copied().ok_or(CodeError::new("media_type_unknown"))?),
        }
        rest = tail.trim_start();
        if rest.is_empty() {
            return Ok(record);
        }
        rest = rest.strip_prefix("and ").ok_or(CodeError::new("media_syntax_unsupported"))?.trim_start();
    }
    loop {
        let body = rest.strip_prefix('(').ok_or(CodeError::new("media_syntax_unsupported"))?;
        let close = body.find(')').ok_or(CodeError::new("media_syntax_unsupported"))?;
        let inner = body.get(..close).unwrap_or("");
        if inner.contains('(') {
            return Err(CodeError::new("media_syntax_unsupported"));
        }
        if record.conditions.len() >= MAX_CONDITIONS {
            return Err(CodeError::new("media_query_too_long"));
        }
        record.conditions.push(parse_condition(inner.trim())?);
        rest = body.get(close.saturating_add(1)..).unwrap_or("").trim_start();
        if rest.is_empty() {
            return Ok(record);
        }
        rest = rest.strip_prefix("and ").ok_or(CodeError::new("media_syntax_unsupported"))?.trim_start();
    }
}

fn parse_condition(inner: &str) -> Result<MediaCondition, CodeError> {
    let (feature, operator, value) = if let Some((f, v)) = inner.split_once(':') {
        (f.trim(), ":", v.trim())
    } else {
        let op_at = inner.find(['<', '>', '=']).ok_or(CodeError::new("media_feature_value_missing"))?;
        let op_len = if inner.get(op_at.saturating_add(1)..).is_some_and(|t| t.starts_with('=')) { 2 } else { 1 };
        let op = inner.get(op_at..op_at.saturating_add(op_len)).unwrap_or("");
        let op = ["<", "<=", ">", ">=", "="].into_iter().find(|o| *o == op).ok_or(CodeError::new("media_syntax_unsupported"))?;
        (inner.get(..op_at).unwrap_or("").trim(), op, inner.get(op_at.saturating_add(op_len)..).unwrap_or("").trim())
    };
    let (name, domain) = MEDIA_FEATURES.iter().find(|(n, _)| *n == feature).copied().ok_or(CodeError::new("media_feature_unknown"))?;
    if operator != ":" && (name.starts_with("min-") || name.starts_with("max-") || matches!(domain, FeatureDomain::Orientation | FeatureDomain::MediaType)) {
        return Err(CodeError::new("media_syntax_unsupported"));
    }
    let (value, unit) = match domain {
        FeatureDomain::MediaType => (MEDIA_TYPES.iter().find(|t| **t == value).ok_or(CodeError::new("media_type_unknown"))?.to_string(), None),
        FeatureDomain::Orientation => (["landscape", "portrait"].into_iter().find(|o| *o == value).ok_or(CodeError::new("media_value_invalid"))?.to_string(), None),
        FeatureDomain::Length => number_unit(value, &LENGTH_UNITS)?,
        FeatureDomain::Resolution => number_unit(value, &RESOLUTION_UNITS)?,
        FeatureDomain::Ratio => {
            let (a, b) = value.split_once('/').ok_or(CodeError::new("media_value_invalid"))?;
            let positive = |s: &str| s.trim().parse::<u32>().ok().filter(|n| *n > 0);
            let (a, b) = (positive(a).ok_or(CodeError::new("media_value_invalid"))?, positive(b).ok_or(CodeError::new("media_value_invalid"))?);
            (format!("{a}/{b}"), None)
        }
    };
    Ok(MediaCondition { feature: name, operator, value, unit })
}

fn number_unit(value: &str, units: &[&'static str]) -> Result<(String, Option<&'static str>), CodeError> {
    let digits = value.bytes().take_while(|c| c.is_ascii_digit() || *c == b'.').count();
    let (number, unit) = value.split_at(digits);
    if number.is_empty() || number.parse::<f64>().is_err() {
        return Err(CodeError::new("media_value_invalid"));
    }
    if unit.is_empty() {
        return if number.parse::<f64>().is_ok_and(|n| n == 0.0) { Ok((number.to_string(), None)) } else { Err(CodeError::new("media_unit_missing")) };
    }
    let unit = units.iter().find(|u| **u == unit).copied().ok_or(CodeError::new("media_unit_unknown"))?;
    Ok((number.to_string(), Some(unit)))
}

/// An HTML `style` element's content range, or a typed refusal.
pub fn style_content(doc: &WebDoc, id: NodeId) -> Result<Range<usize>, CodeError> {
    let node = doc.node(id).ok_or(CodeError::new("no_such_node"))?;
    if node.kind != NodeKind::Element || doc.local_name(id).as_deref() != Some("style") {
        return Err(CodeError::new("not_a_style_element"));
    }
    if node.namespace != Namespace::Html || node.content != ContentModel::RawText {
        return Err(CodeError::new("stylesheet_not_html_style"));
    }
    node.content_span().ok_or(CodeError::new("stylesheet_not_html_style"))
}

/// The HTML `style` elements of a document, in order.
pub fn style_elements(doc: &WebDoc) -> Vec<NodeId> {
    (0..doc.len()).filter(|&id| style_content(doc, id).is_ok()).collect()
}

/// One media block of a document: the style element, the query text and its records or refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaBlockRecord {
    pub style: NodeId,
    pub query_range: Range<usize>,
    pub query: String,
    pub records: Result<Vec<MediaQueryRecord>, CodeError>,
}

/// [STU-WEB-076]: every `@media` block of the document's HTML style elements as records (a sheet that does not scan
/// is one refusal row with the style's whole content as its range).
pub fn document_media_records(doc: &WebDoc, work: &mut Work<'_>) -> Result<Vec<MediaBlockRecord>, CodeError> {
    let src = doc.source();
    let mut out = Vec::new();
    for style in style_elements(doc) {
        let Ok(content) = style_content(doc, style) else { continue };
        work.tick(content.len())?;
        let items = match scan_css(src, content.clone()) {
            Ok(items) => items,
            Err(e) => {
                out.push(MediaBlockRecord { style, query_range: content, query: String::new(), records: Err(e) });
                continue;
            }
        };
        for item in items.iter().filter(|i| i.kind == ItemKind::Media) {
            let query_text = media_query_text(src, item);
            let rules: Vec<String> = item.children.iter().filter(|c| c.kind == ItemKind::Rule).filter_map(|c| selector_key(src.get(c.prelude.clone()).unwrap_or("")).ok()).collect();
            let records = parse_media_query_list(query_text).map(|mut list| {
                for r in &mut list {
                    r.rules.clone_from(&rules);
                }
                list
            });
            out.push(MediaBlockRecord { style, query_range: item.prelude.clone(), query: normalised(query_text, true).unwrap_or_default(), records });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------------------
// The four-level edit ([STU-WEB-060]).

/// One declaration change: a value sets (adds or replaces) the property, `None` removes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationChange {
    pub property: String,
    pub value: Option<String>,
}

/// Text an edit writes must not end the `style` element, open a comment or block, or break the declaration.
fn checked_css_text(text: &str, what: &'static str, block_chars: bool) -> Result<(), CodeError> {
    if text.len() > MAX_CSS_TEXT || text.trim().is_empty() || text.chars().any(|c| c.is_control() && c != '\t') {
        return Err(CodeError::new(what));
    }
    // Review F4: a trailing backslash would escape the byte written after the text (`;`, `}`), and a backslash before a
    // newline is no escape at all (CSS Syntax 3, 4.3.8): either way a neighbour would be swallowed.
    if !text.trim_end().bytes().rev().take_while(|c| *c == b'\\').count().is_multiple_of(2) {
        return Err(CodeError::new(what));
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("</") || lower.contains("/*") || lower.contains("*/") {
        return Err(CodeError::new("css_text_unsafe"));
    }
    let b = text.as_bytes();
    // Quotes and parentheses balance, and no top-level `;`, `{` or `}` (the text stays one token run).
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = string_end(b, i).map_err(|_| CodeError::new(what))?,
            b'\\' => i = i.saturating_add(2),
            b'(' | b'[' => {
                depth = depth.saturating_add(1);
                i = i.saturating_add(1);
            }
            b')' | b']' => {
                depth = depth.saturating_sub(1);
                if depth < 0 {
                    return Err(CodeError::new(what));
                }
                i = i.saturating_add(1);
            }
            b';' | b'{' | b'}' if depth == 0 && block_chars => return Err(CodeError::new(what)),
            b'{' | b'}' => return Err(CodeError::new(what)),
            _ => i = i.saturating_add(1),
        }
    }
    if depth != 0 {
        return Err(CodeError::new(what));
    }
    Ok(())
}

fn checked_property(name: &str) -> Result<String, CodeError> {
    let lower = name.to_ascii_lowercase();
    if lower.is_empty() || lower.len() > 128 || !lower.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') || lower.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(CodeError::new("css_property_invalid"));
    }
    Ok(lower)
}

/// The line ending the document uses (CR LF when its first line break is one).
pub fn line_ending(src: &str) -> &'static str {
    match src.find('\n') {
        Some(i) if i > 0 && src.as_bytes().get(i.saturating_sub(1)) == Some(&b'\r') => "\r\n",
        _ => "\n",
    }
}

/// The whitespace at the start of the line holding byte `at`.
pub fn line_indent(src: &str, at: usize) -> &str {
    let start = src.get(..at).and_then(|s| s.rfind(['\n', '\r'])).map_or(0, |i| i.saturating_add(1));
    let line = src.get(start..).unwrap_or("");
    let n = line.bytes().take_while(|c| *c == b' ' || *c == b'\t').count();
    line.get(..n).unwrap_or("")
}

/// The indent unit of a sheet: a tab when an indented line of it starts with one, else two spaces.
fn indent_unit(src: &str, content: &Range<usize>) -> &'static str {
    let text = src.get(content.clone()).unwrap_or("");
    for line in text.split('\n').skip(1) {
        match line.as_bytes().first() {
            Some(b'\t') => return "\t",
            Some(b' ') => return "  ",
            _ => {}
        }
    }
    "  "
}

/// What [`rule_edit`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleEditOutcome {
    pub edit: TextEdit,
    pub created_media: bool,
    pub created_rule: bool,
    /// Properties set, emitted prefixes included, in order.
    pub set: Vec<String>,
    /// Properties removed (an emission that was not there is not listed).
    pub removed: Vec<String>,
}

/// A selector's identity (review F6): the RAW text, escapes kept escaped (`.a\,b` is one class, never the list
/// `.a,b`), comments removed, whitespace runs one space, no space around the combinators `>`, `+`, `~` and the list
/// comma. Case is kept.
pub fn selector_key(text: &str) -> Result<String, CodeError> {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    let mut i = 0usize;
    let joins = |c: char| matches!(c, '>' | '+' | '~' | ',');
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i.saturating_add(1)) == Some(&b'*') {
            i = comment_end(b, i)?;
            space = true;
            continue;
        }
        if is_ws(c) {
            space = true;
            i = i.saturating_add(1);
            continue;
        }
        let (piece, next) = if c == b'"' || c == b'\'' {
            let end = string_end(b, i)?;
            (text.get(i..end).unwrap_or(""), end)
        } else if c == b'\\' {
            // The escape stays as written: a backslash and the one character it escapes.
            let after = i.saturating_add(1);
            let width = text.get(after..).and_then(|s| s.chars().next()).map_or(0, char::len_utf8);
            (text.get(i..after.saturating_add(width)).unwrap_or("\\"), after.saturating_add(width))
        } else {
            let ch = text.get(i..).and_then(|s| s.chars().next()).unwrap_or('\u{fffd}');
            (text.get(i..i.saturating_add(ch.len_utf8())).unwrap_or(""), i.saturating_add(ch.len_utf8().max(1)))
        };
        let first = piece.chars().next().unwrap_or(' ');
        if space && !out.is_empty() && !joins(first) && !out.ends_with(joins) {
            out.push(' ');
        }
        space = false;
        out.push_str(piece);
        i = next;
    }
    Ok(out)
}

/// One expanded change: its property, value (`None` removes) and whether it must be present to be removed.
struct Change {
    property: String,
    value: Option<String>,
    required: bool,
}

/// [STU-WEB-060]: in the HTML `style` element `style`, inside the `@media` block whose query is `media` (compared as
/// records; created at the end of the sheet when absent; `None` = the sheet's top level), the rule whose raw selector
/// ([`selector_key`]) is `selector` gets `changes`: a value replaces the property's last declaration (its
/// `!important` kept unless the value says otherwise) or appends one, `None` removes every declaration of the
/// property. With several rules of that selector at that level the edit needs `rule_index` (0-based, document order),
/// else it refuses `css_rule_ambiguous` listing them (review F6, lead Q5). A missing rule is created; at the sheet's
/// top level it goes before the first `@media` block, so it never overrides a media rule. A prefix rule
/// ([STU-WEB-072]) writes its emissions with the base; removing the base removes the emissions that are there (an
/// absent emission is no refusal, review F5). The result is ONE text edit: the new rule or media block inserted, or the
/// rule's block rewritten from its own bytes with only the changed declarations touched, in one pass.
#[allow(clippy::too_many_arguments)]
pub fn rule_edit(
    doc: &WebDoc,
    style: NodeId,
    media: Option<&str>,
    selector: &str,
    rule_index: Option<usize>,
    changes: &[DeclarationChange],
    rules: &[PrefixRule],
    work: &mut Work<'_>,
) -> Result<RuleEditOutcome, CodeError> {
    if changes.is_empty() || changes.len() > MAX_EDIT_DECLARATIONS {
        return Err(CodeError::new(if changes.is_empty() { "nothing_to_change" } else { "too_many_declarations" }));
    }
    checked_css_text(selector, "css_selector_invalid", true)?;
    if let Some(m) = media {
        checked_css_text(m, "media_query_invalid", true)?;
        parse_media_query_list(m)?;
    }
    let src = doc.source();
    let content = style_content(doc, style)?;
    work.tick(content.len())?;
    let items = scan_css(src, content.clone())?;
    let want_selector = selector_key(selector)?;
    // A media block is the same query when its records are (type and conditions; spacing, case and comments do not
    // count), or, for a query outside the record grammar, when its normalised text is.
    let want_media = media.map(|m| Ok::<_, CodeError>((normalised(m, true)?, media_shape(m)))).transpose()?;
    let same_media = |item: &CssItem, (text, shape): &(String, Option<MediaShape>)| {
        let have = media_query_text(src, item);
        match shape {
            Some(s) => media_shape(have).as_ref() == Some(s),
            None => normalised(have, true).ok().as_ref() == Some(text),
        }
    };
    let nl = line_ending(src);
    let unit = indent_unit(src, &content);
    let style_open = doc.node(style).and_then(|n| n.open_span.clone()).map_or(content.start, |s| s.start);
    // A level's items sit at the indent of its first item when it has one, else one unit inside their opener.
    let indent_of = |list: &[CssItem], opener: &str| list.first().map(|i| line_indent(src, i.whole.start).to_string()).filter(|i| !i.is_empty()).unwrap_or_else(|| format!("{opener}{unit}"));
    let base = indent_of(&items, line_indent(src, style_open));
    let (level_items, level_range, level_indent, media_item) = match &want_media {
        None => (&items, content.clone(), base.clone(), None),
        Some(q) => match items.iter().find(|i| i.kind == ItemKind::Media && same_media(i, q)) {
            Some(m) => (&m.children, m.block.clone(), indent_of(&m.children, line_indent(src, m.whole.start)), Some(m)),
            None => (&items, content.clone(), base.clone(), None),
        },
    };
    let new_media = want_media.is_some() && media_item.is_none();
    let candidates: Vec<&CssItem> = if new_media {
        Vec::new()
    } else {
        level_items.iter().filter(|i| i.kind == ItemKind::Rule && src.get(i.prelude.clone()).and_then(|p| selector_key(p).ok()).as_ref() == Some(&want_selector)).collect()
    };
    let rule = match (candidates.len(), rule_index) {
        (0, None) => None,
        (0, Some(_)) => return Err(CodeError::new("rule_index_invalid")),
        (1, None | Some(0)) => candidates.first().copied(),
        (_, None) => return Err(CodeError::listing("css_rule_ambiguous", candidates.iter().map(|c| c.whole.clone()).collect())),
        (_, Some(n)) => Some(*candidates.get(n).ok_or(CodeError::new("rule_index_invalid"))?),
    };
    let block_text = rule.map(|r| src.get(r.block.clone()).unwrap_or("")).unwrap_or("");
    let block_decls = if rule.is_some() { declarations(block_text, 0..block_text.len())? } else { Vec::new() };
    // Expand the prefix rules; a later change of one property replaces an earlier one.
    let mut expanded: Vec<Change> = Vec::new();
    let mut push = |c: Change| {
        expanded.retain(|x| x.property != c.property);
        expanded.push(c);
    };
    for c in changes {
        let property = checked_property(&c.property)?;
        if let Some(v) = &c.value {
            checked_css_text(v, "css_value_invalid", true)?;
        }
        // For a removal, a per-value rule matches the value the base has now.
        let current = block_decls.iter().rev().find(|d| d.property == property).and_then(|d| block_text.get(d.value_range.clone())).map(str::to_string);
        let match_value = c.value.clone().or(current);
        for rule in rules.iter().filter(|r| r.property == property && r.value.as_deref().is_none_or(|rv| match_value.as_deref() == Some(rv))) {
            for em in &rule.emissions {
                push(Change { property: em.emitted_property.clone(), value: c.value.as_ref().map(|v| em.emitted_value.clone().unwrap_or_else(|| v.clone())), required: false });
            }
        }
        push(Change { property, value: c.value.clone(), required: true });
    }
    let mut set = Vec::new();
    let mut removed = Vec::new();
    if let Some(rule) = rule {
        let text = block_text;
        let multi_line = text.contains('\n') || text.trim().is_empty();
        let decl_indent = block_decls.first().filter(|_| multi_line).map(|d| line_indent(text, d.property_range.start).to_string()).filter(|i| !i.is_empty()).unwrap_or_else(|| format!("{level_indent}{unit}"));
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        let mut gone = vec![false; block_decls.len()];
        let mut appends: Vec<String> = Vec::new();
        for c in &expanded {
            match &c.value {
                Some(v) => {
                    if let Some(d) = block_decls.iter().rev().find(|d| d.property == c.property) {
                        let keep_important = d.important && !v.to_ascii_lowercase().contains("!important");
                        edits.push((d.value_range.clone(), if keep_important { format!("{v} !important") } else { v.clone() }));
                    } else {
                        appends.push(format!("{}: {v};", c.property));
                    }
                    set.push(c.property.clone());
                }
                None => {
                    let mut any = false;
                    for (i, d) in block_decls.iter().enumerate().filter(|(_, d)| d.property == c.property) {
                        let mut from = d.property_range.start;
                        while from > 0 && is_ws(text.as_bytes()[from.saturating_sub(1)]) {
                            from = from.saturating_sub(1);
                        }
                        edits.push((from..d.end.max(d.whole.end), String::new()));
                        if let Some(g) = gone.get_mut(i) {
                            *g = true;
                        }
                        any = true;
                    }
                    match (any, c.required) {
                        (true, _) => removed.push(c.property.clone()),
                        (false, true) => return Err(CodeError::new("css_property_absent")),
                        (false, false) => {}
                    }
                }
            }
        }
        if !appends.is_empty() {
            let last = block_decls.iter().zip(&gone).filter(|(_, g)| !**g).map(|(d, _)| d).next_back();
            let insert = match last {
                Some(d) => {
                    let semi = if text.as_bytes().get(d.end.saturating_sub(1)) == Some(&b';') { "" } else { ";" };
                    let sep = if multi_line { format!("{nl}{decl_indent}") } else { " ".to_string() };
                    let body = appends.iter().map(|a| format!("{sep}{a}")).collect::<String>();
                    (d.end..d.end, format!("{semi}{body}"))
                }
                // No declaration stays: the new ones on their own lines, the brace back at the rule's indent.
                None => {
                    let tail = trim(text.as_bytes(), 0..text.len()).end;
                    let body = appends.iter().map(|a| format!("{nl}{decl_indent}{a}")).collect::<String>();
                    (tail..text.len(), format!("{body}{nl}{level_indent}"))
                }
            };
            edits.push(insert);
        }
        edits.sort_by_key(|(r, _)| (r.start, r.end));
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0usize;
        for (r, rep) in &edits {
            if r.start < cursor {
                return Err(CodeError::new("css_edit_overlap"));
            }
            out.push_str(text.get(cursor..r.start).unwrap_or(""));
            out.push_str(rep);
            cursor = r.end;
        }
        out.push_str(text.get(cursor..).unwrap_or(""));
        if out == text {
            return Err(CodeError::new("nothing_to_change"));
        }
        return Ok(RuleEditOutcome { edit: TextEdit { span: rule.block.clone(), replacement: out }, created_media: false, created_rule: false, set, removed });
    }
    // A new rule (and media block): only additions make sense.
    if expanded.iter().any(|c| c.value.is_none() && c.required) {
        return Err(CodeError::new("css_rule_absent"));
    }
    let adds: Vec<&Change> = expanded.iter().filter(|c| c.value.is_some()).collect();
    let rule_indent = if new_media { format!("{base}{unit}") } else { level_indent.clone() };
    let mut rule_text = format!("{} {{{nl}", selector.trim());
    for c in &adds {
        rule_text.push_str(&format!("{rule_indent}{unit}{}: {};{nl}", c.property, c.value.as_deref().unwrap_or("")));
        set.push(c.property.clone());
    }
    rule_text.push_str(&format!("{rule_indent}}}"));
    // At the sheet's top level a new rule goes before the first @media block (lead Q5).
    let first_media = if want_media.is_none() { items.iter().find(|i| i.kind == ItemKind::Media) } else { None };
    if let Some(m) = first_media {
        let indent = line_indent(src, m.whole.start);
        return Ok(RuleEditOutcome { edit: TextEdit { span: m.whole.start..m.whole.start, replacement: format!("{rule_text}{nl}{indent}") }, created_media: false, created_rule: true, set, removed });
    }
    let text = if new_media { format!("{base}@media {} {{{nl}{rule_indent}{rule_text}{nl}{base}}}", media.unwrap_or("").trim()) } else { format!("{rule_indent}{rule_text}") };
    // Inserted after the level's last token; the whitespace before the closing `</style>` or `}` is rewritten so the
    // close sits on its own line at its opener's indent.
    let close_indent = media_item.map_or_else(|| line_indent(src, style_open).to_string(), |m| line_indent(src, m.whole.start).to_string());
    let last = trim(src.as_bytes(), level_range.clone()).end;
    let replacement = format!("{nl}{text}{nl}{close_indent}");
    Ok(RuleEditOutcome { edit: TextEdit { span: last..level_range.end, replacement }, created_media: new_media, created_rule: true, set, removed })
}

// ---------------------------------------------------------------------------------------------------------------------
// Code navigator ([STU-WEB-059]).

/// One rule affecting an element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffectingRule {
    pub style: NodeId,
    pub media: Option<String>,
    pub selector: String,
    /// `[ids, classes, types]` of the most specific matching compound (review F6: a tuple, compared in order).
    pub specificity: [u32; 3],
    pub whole: Range<usize>,
    pub declarations: usize,
}

/// The rules of the document's HTML style sheets whose selector matches element `id` (simple selectors: tag, `*`,
/// `.class`, `#id`, compounds and lists, read from the raw selector text, so an escaped selector is never split or
/// decoded into another; `@media` conditions are listed, not evaluated), and the count of rules whose selector is
/// outside that subset (not matched, never guessed).
pub fn affecting_rules(doc: &WebDoc, id: NodeId, work: &mut Work<'_>) -> Result<(Vec<AffectingRule>, usize), CodeError> {
    let node = doc.node(id).ok_or(CodeError::new("no_such_node"))?;
    if node.kind != NodeKind::Element {
        return Err(CodeError::new("not_an_element"));
    }
    let info = crate::css::ElemInfo {
        tag: doc.local_name(id).unwrap_or_default(),
        id: doc.attribute(id, "id").and_then(|a| doc.attr_value(a)),
        classes: doc.attribute(id, "class").and_then(|a| doc.attr_value(a)).map(|v| v.split_ascii_whitespace().map(str::to_string).collect()).unwrap_or_default(),
    };
    let src = doc.source();
    let mut out = Vec::new();
    let mut unsupported = 0usize;
    for style in style_elements(doc) {
        let Ok(content) = style_content(doc, style) else { continue };
        work.tick(content.len())?;
        let Ok(items) = scan_css(src, content) else {
            unsupported = unsupported.saturating_add(1);
            continue;
        };
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let mut visit = |item: &CssItem, media: Option<String>| {
            let selector = src.get(item.prelude.clone()).and_then(|p| selector_key(p).ok()).unwrap_or_default();
            match crate::css::parse_selectors(&selector) {
                Some(list) => {
                    if let Some(spec) = list.iter().filter(|c| c.matches(&info)).map(|c| [count(c.ids.len()), count(c.classes.len()), u32::from(c.tag.is_some())]).max() {
                        let declarations = declarations(src, item.block.clone()).map_or(0, |d| d.len());
                        out.push(AffectingRule { style, media, selector, specificity: spec, whole: item.whole.clone(), declarations });
                    }
                }
                None => unsupported = unsupported.saturating_add(1),
            }
        };
        for item in &items {
            match item.kind {
                ItemKind::Rule => visit(item, None),
                ItemKind::Media => {
                    let q = normalised(media_query_text(src, item), true).ok();
                    for child in item.children.iter().filter(|c| c.kind == ItemKind::Rule) {
                        visit(child, q.clone());
                    }
                }
                _ => {}
            }
        }
    }
    Ok((out, unsupported))
}
