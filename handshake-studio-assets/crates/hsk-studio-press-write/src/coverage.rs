//! Total ink coverage contract (lane SMB-PRESS-INK-COVERAGE-1; Master Spec v02.226 STU-LAY-250..254,
//! with STU-LAY-100 unknown bounds, STU-LAY-154 severity, STU-LAY-160 ink types and STU-LAY-166 typed
//! errors). It evaluates the planes of a [`SeparationPreview`] -- the composed and separated device
//! values -- against limits declared at the sites of `ink_limit_source`, and reports the result as
//! typed, serialisable data.
//!
//! - STU-LAY-250: the five parameters as seven-field records ([`CoverageParameter::spec`]); `unknown`
//!   is [`ParamValue::Unknown`], never filled from the other range. The 400 % `hard_max` of
//!   `total_ink_limit` is the four-ink case: [`CoverageParameter::hard_max_for_inks`] scales it to
//!   100 x the plate count of the evaluated separation; a value past it is a typed error, not a clamp.
//! - STU-LAY-251: [`InkLimitSites`] stores one declaration per site; [`InkLimitSites::resolve`] takes,
//!   per field, the first declared value in member order and records the site. `undeclared` is
//!   terminal: an undeclared limit is `check_state.not_checked`, never a pass (also when the rule is
//!   `severity.disabled`). Content the preview could not separate makes a declared limit
//!   `check_state.indeterminate` ([`IndeterminateCause`]).
//! - STU-LAY-252: all three metrics on the separated planes at the separation's own sample grid; the
//!   declared `coverage_sample_resolution` must equal the resolution the separation ran at. Opaque and
//!   opaque-ignore inks contribute per their declared `ink_type` (rule in the separations module docs).
//!   Stages this pipeline performs ([`CoverageInputStage::performed`]): spot-to-process conversion and
//!   ink aliasing (ink manager), overprint composition (separations), authored rich black and
//!   registration swatches, opaque ink handling. Transparency flattening and placed-image separation
//!   are NOT performed: they are typed members that are never reported as `introduced_by`.
//! - STU-LAY-253: [`InkCoverageFinding`] carries the thirteen fields of the table (plus which of the
//!   three limits fired) and serialises as JSON ([`InkCoverageFinding::to_json`]). Reaching the
//!   model-steerable command surface (STU-LAY-057) is the host's: this crate produces the record only.
//! - STU-LAY-254: [`InkLimitSites::default`] declares nothing, so a shipped Studio reports
//!   `not_checked`. [`PUBLISHED_INK_LIMIT_REFERENCE`] keeps the published figures as reference rows
//!   (ranges as ranges) that resolution never reads. The evaluation reads the separation and never
//!   re-separates or changes it.
//!
//! Hostile sizes (V4-01/02): page lists, cause lists, the total sample grid, object references at the
//! worst location and every caller string are bounded with typed refusals before pixels are read;
//! counts use checked arithmetic. Vendor parity is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observe, Receipt, SinkPort};
use crate::separations::{InkLimits, InkReading, LIMIT_EPS, Measure, SepNote, SeparationPreview, Severity};

/// Most pages one evaluation reads.
pub const MAX_COVERAGE_PAGES: usize = 4096;
/// Most indeterminate causes one evaluation carries (caller-declared plus derived).
pub const MAX_INDETERMINATE_CAUSES: usize = 4096;
/// Most samples (canvas pixels over all pages) one evaluation reads.
pub const MAX_COVERAGE_SAMPLES: u64 = 1 << 30;
/// Most object references a finding lists at its worst location.
pub const MAX_COVERAGE_OBJECT_REFS: usize = 256;
/// Longest rule id, bytes.
pub const MAX_RULE_ID_BYTES: usize = 256;
/// Longest page or spread identity, bytes.
pub const MAX_PAGE_REF_BYTES: usize = 256;
/// Longest object reference string (item id or link), bytes.
pub const MAX_OBJECT_REF_BYTES: usize = 1024;

macro_rules! token_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident => $code:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name { $($(#[$vm])* $v),+ }
        impl $name {
            /// Every member, in the spec's member order.
            pub const ALL: [$name; [$(stringify!($v)),+].len()] = [$(Self::$v),+];
            /// The member's token.
            pub const fn code(self) -> &'static str {
                match self { $(Self::$v => $code),+ }
            }
            /// The member with exactly this token; anything else is `None` (never mapped onto a near member).
            pub fn from_code(code: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|m| m.code() == code)
            }
        }
    };
}

token_enum! {
    /// The five parameters of the STU-LAY-250 table.
    CoverageParameter {
        TotalInkLimit => "total_ink_limit",
        SingleInkLimit => "single_ink_limit",
        BlackInkLimit => "black_ink_limit",
        CoverageWarningMargin => "coverage_warning_margin",
        CoverageSampleResolution => "coverage_sample_resolution",
    }
}

token_enum! {
    /// `ink_limit_source` (STU-LAY-251), ordered by precedence; `Undeclared` is terminal.
    InkLimitSource {
        PreflightRuleInstance => "ink_limit_source.preflight_rule_instance",
        ExportRecipe => "ink_limit_source.export_recipe",
        DocumentPolicy => "ink_limit_source.document_policy",
        OutputIntentProfile => "ink_limit_source.output_intent_profile",
        PressConditionPreset => "ink_limit_source.press_condition_preset",
        Undeclared => "ink_limit_source.undeclared",
    }
}

token_enum! {
    /// `ink_limit_scope` (STU-LAY-251).
    InkLimitScope {
        Document => "ink_limit_scope.document",
        Spread => "ink_limit_scope.spread",
        Page => "ink_limit_scope.page",
        Object => "ink_limit_scope.object",
        Plate => "ink_limit_scope.plate",
    }
}

token_enum! {
    /// `ink_limit_check_state` (STU-LAY-251).
    InkLimitCheckState {
        NotChecked => "check_state.not_checked",
        WithinLimit => "check_state.within_limit",
        WithinWarningMargin => "check_state.within_warning_margin",
        OverLimit => "check_state.over_limit",
        Indeterminate => "check_state.indeterminate",
    }
}

token_enum! {
    /// `coverage_input_stage` (STU-LAY-252).
    CoverageInputStage {
        SpotToProcessConversion => "stage.spot_to_process_conversion",
        InkAliasResolution => "stage.ink_alias_resolution",
        OverprintComposition => "stage.overprint_composition",
        TransparencyFlattening => "stage.transparency_flattening",
        PlacedImageSeparation => "stage.placed_image_separation",
        AuthoredRichBlack => "stage.authored_rich_black",
        OpaqueInkHandling => "stage.opaque_ink_handling",
    }
}

token_enum! {
    /// `coverage_metric` (STU-LAY-252).
    CoverageMetric {
        Maximum => "metric.maximum",
        Mean => "metric.mean",
        AreaAboveLimit => "metric.area_above_limit",
    }
}

token_enum! {
    /// Units of the STU-LAY-250 table.
    CoverageUnit {
        Percent => "percent",
        Dpi => "dpi",
    }
}

impl CoverageInputStage {
    /// True for the stages this separation pipeline performs (and can therefore report as
    /// `introduced_by`); transparency flattening and placed-image separation are not performed.
    pub const fn performed(self) -> bool {
        !matches!(self, Self::TransparencyFlattening | Self::PlacedImageSeparation)
    }
    /// Bit of this stage in an object's stage set.
    pub(crate) const fn bit(self) -> u8 {
        match self {
            Self::SpotToProcessConversion => 1,
            Self::InkAliasResolution => 2,
            Self::OverprintComposition => 4,
            Self::TransparencyFlattening => 8,
            Self::PlacedImageSeparation => 16,
            Self::AuthoredRichBlack => 32,
            Self::OpaqueInkHandling => 64,
        }
    }
}

/// One numeric field of a seven-field record: declared or `unknown` (STU-LAY-100).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamValue {
    Unknown,
    Known(f64),
}

/// The `precision` field: decimal places, or `unknown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamPrecision {
    Unknown,
    Digits(u8),
}

/// The seven independent fields of one STU-LAY-250 parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverageParameterSpec {
    pub parameter: CoverageParameter,
    pub hard_min: ParamValue,
    pub hard_max: ParamValue,
    pub soft_min: ParamValue,
    pub soft_max: ParamValue,
    pub default: ParamValue,
    pub unit: CoverageUnit,
    pub precision: ParamPrecision,
}

impl CoverageParameter {
    /// The parameter's row of the STU-LAY-250 table, exactly as stated (unknown stays unknown).
    pub const fn spec(self) -> CoverageParameterSpec {
        use ParamValue::{Known, Unknown};
        let (hard_min, hard_max, unit, precision) = match self {
            Self::TotalInkLimit => (Known(0.0), Known(400.0), CoverageUnit::Percent, ParamPrecision::Unknown),
            Self::SingleInkLimit | Self::BlackInkLimit => (Known(0.0), Known(100.0), CoverageUnit::Percent, ParamPrecision::Unknown),
            Self::CoverageWarningMargin => (Known(0.0), Unknown, CoverageUnit::Percent, ParamPrecision::Unknown),
            Self::CoverageSampleResolution => (Unknown, Unknown, CoverageUnit::Dpi, ParamPrecision::Digits(0)),
        };
        CoverageParameterSpec { parameter: self, hard_min, hard_max, soft_min: Unknown, soft_max: Unknown, default: Unknown, unit, precision }
    }

    /// The hard maximum for a separation of `inks` plates: `total_ink_limit` is 100 x `inks` (the
    /// table's 400 is the four-ink case), every other parameter its table value.
    pub fn hard_max_for_inks(self, inks: usize) -> ParamValue {
        match self {
            Self::TotalInkLimit => ParamValue::Known(inks as f64 * 100.0),
            _ => self.spec().hard_max,
        }
    }

    /// Checks `value` against the hard minimum and `hard_max` (Unknown = no upper bound applies).
    fn check(self, source: InkLimitSource, value: f64, hard_max: ParamValue) -> Result<(), CoverageError> {
        if !value.is_finite() {
            return Err(CoverageError::NotFinite { parameter: self, source });
        }
        let hard_min = self.spec().hard_min;
        let below = matches!(hard_min, ParamValue::Known(lo) if value < lo);
        let above = matches!(hard_max, ParamValue::Known(hi) if value > hi);
        if below || above {
            return Err(CoverageError::OutOfHardBounds { parameter: self, source, value, hard_min, hard_max });
        }
        Ok(())
    }
}

/// One value after resolution: the value and the site that supplied it (`Undeclared` with no value).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedLimit {
    pub value: Option<f64>,
    pub source: InkLimitSource,
}

/// The effective limits of one output operation, each with its source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedInkLimits {
    pub total_ink_limit: ResolvedLimit,
    pub single_ink_limit: ResolvedLimit,
    pub black_ink_limit: ResolvedLimit,
    pub coverage_warning_margin: ResolvedLimit,
}

impl ResolvedInkLimits {
    /// The resolved value of `p`; `coverage_sample_resolution` is declared by the request, not by a
    /// site, so it resolves as undeclared here.
    pub fn get(&self, p: CoverageParameter) -> ResolvedLimit {
        match p {
            CoverageParameter::TotalInkLimit => self.total_ink_limit,
            CoverageParameter::SingleInkLimit => self.single_ink_limit,
            CoverageParameter::BlackInkLimit => self.black_ink_limit,
            CoverageParameter::CoverageWarningMargin => self.coverage_warning_margin,
            CoverageParameter::CoverageSampleResolution => ResolvedLimit { value: None, source: InkLimitSource::Undeclared },
        }
    }
}

/// The stored ink-limit contract: one [`InkLimits`] declaration per site of `ink_limit_source`
/// (the three limits and the margin are separate fields). The default declares nothing (STU-LAY-254).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InkLimitSites {
    declared: [InkLimits; 5],
}

impl InkLimitSource {
    fn site_index(self) -> Option<usize> {
        Self::ALL[..5].iter().position(|s| *s == self)
    }
}

impl InkLimitSites {
    /// Declares the limits of one site (replacing that site's earlier declaration). Each declared
    /// value is checked against its hard bounds (STU-LAY-100: a validation error, never a clamp);
    /// `total_ink_limit`'s maximum depends on the ink count and is checked at evaluation. A refusal
    /// leaves the site unchanged; `undeclared` is not a site.
    pub fn declare(&mut self, source: InkLimitSource, limits: InkLimits) -> Result<(), CoverageError> {
        let slot = source.site_index().ok_or(CoverageError::UndeclaredIsNotASite)?;
        let fields = [
            (CoverageParameter::TotalInkLimit, limits.total_ink_limit, ParamValue::Unknown),
            (CoverageParameter::SingleInkLimit, limits.single_ink_limit, CoverageParameter::SingleInkLimit.spec().hard_max),
            (CoverageParameter::BlackInkLimit, limits.black_ink_limit, CoverageParameter::BlackInkLimit.spec().hard_max),
            (CoverageParameter::CoverageWarningMargin, limits.coverage_warning_margin, ParamValue::Unknown),
        ];
        for (p, v, hard_max) in fields {
            if let Some(v) = v {
                p.check(source, v, hard_max)?;
            }
        }
        if let Some(d) = self.declared.get_mut(slot) {
            *d = limits;
        }
        Ok(())
    }

    /// What `source` declares (`None` for `undeclared`).
    pub fn declared(&self, source: InkLimitSource) -> Option<InkLimits> {
        source.site_index().and_then(|i| self.declared.get(i)).copied()
    }

    /// Per field, the first declared value in `ink_limit_source` member order, with its site.
    pub fn resolve(&self) -> ResolvedInkLimits {
        let pick = |f: fn(&InkLimits) -> Option<f64>| {
            self.declared
                .iter()
                .zip(InkLimitSource::ALL)
                .find_map(|(d, source)| f(d).map(|v| ResolvedLimit { value: Some(v), source }))
                .unwrap_or(ResolvedLimit { value: None, source: InkLimitSource::Undeclared })
        };
        ResolvedInkLimits {
            total_ink_limit: pick(|d| d.total_ink_limit),
            single_ink_limit: pick(|d| d.single_ink_limit),
            black_ink_limit: pick(|d| d.black_ink_limit),
            coverage_warning_margin: pick(|d| d.coverage_warning_margin),
        }
    }
}

/// The ink-limit rule instance being evaluated (STU-LAY-154 rule instance: id, severity, scope).
#[derive(Clone, Debug, PartialEq)]
pub struct InkLimitRule {
    pub rule_id: String,
    pub severity: Severity,
    pub scope: InkLimitScope,
}

/// Page or spread identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageReference {
    pub page: String,
    pub spread: Option<String>,
}

/// A region in document coordinates: the page's PDF user space, points (y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocRegion {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// `page_reference` of a finding: the page and the offending region on it (`None` when no location
/// is past the threshold, e.g. an indeterminate page).
#[derive(Clone, Debug, PartialEq)]
pub struct FindingPageRef {
    pub page: PageReference,
    pub region: Option<DocRegion>,
}

/// A page item composing at a location.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectRef {
    /// Index of the item in paint order ([`crate::Separator::draw`]) or in the vector page's item list
    /// ([`crate::separate_vector_page`]).
    PageItem(u64),
    /// A host page-item identity.
    Named(String),
    /// A placed asset, carried by its link reference.
    Placed { link: String },
}

impl ObjectRef {
    /// Strings are non-blank and at most [`MAX_OBJECT_REF_BYTES`].
    pub fn is_valid(&self) -> bool {
        match self {
            Self::PageItem(_) => true,
            Self::Named(s) | Self::Placed { link: s } => !s.trim().is_empty() && s.len() <= MAX_OBJECT_REF_BYTES,
        }
    }
    fn json(&self, out: &mut String) {
        match self {
            Self::PageItem(n) => out.push_str(&format!("{{\"page_item\":{n}}}")),
            Self::Named(s) => {
                out.push_str("{\"named\":");
                json_str(out, s);
                out.push('}');
            }
            Self::Placed { link } => {
                out.push_str("{\"placed_link\":");
                json_str(out, link);
                out.push('}');
            }
        }
    }
}

/// Why a declared limit could not be checked (`check_state.indeterminate`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndeterminateCause {
    /// A placed asset whose link is missing (declared by the caller).
    MissingLink { object: Option<ObjectRef> },
    /// Content whose colour profile cannot be resolved; the preview derives it for objects it could not
    /// separate (RGB, Lab or ICC content without a colour engine port).
    UnresolvableProfile { object: Option<ObjectRef> },
    /// A passthrough object (declared by the caller).
    Passthrough { object: Option<ObjectRef> },
    /// Shadings are not separated by the preview.
    ShadingNotSeparated,
    /// Transparency was painted opaque: the pipeline has no flattener stage, so the values are not
    /// the flattened composite STU-LAY-252 requires.
    TransparencyNotFlattened,
    /// PDF input: overprint flags are not read (objects knocked out), so overprint sums may be missing.
    PdfOverprintNotRead,
}

impl IndeterminateCause {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingLink { .. } => "missing_link",
            Self::UnresolvableProfile { .. } => "unresolvable_profile",
            Self::Passthrough { .. } => "passthrough",
            Self::ShadingNotSeparated => "shading_not_separated",
            Self::TransparencyNotFlattened => "transparency_not_flattened",
            Self::PdfOverprintNotRead => "pdf_overprint_not_read",
        }
    }
    fn object(&self) -> Option<&ObjectRef> {
        match self {
            Self::MissingLink { object } | Self::UnresolvableProfile { object } | Self::Passthrough { object } => object.as_ref(),
            _ => None,
        }
    }
}

/// One evaluated page.
#[derive(Clone, Debug, PartialEq)]
pub struct CoveragePage<'a> {
    pub reference: PageReference,
    pub preview: &'a SeparationPreview,
    /// Placed assets the caller could not separate for measurement.
    pub unmeasured: Vec<IndeterminateCause>,
}

/// One evaluation of an output operation.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageRequest<'a> {
    pub rule: InkLimitRule,
    pub sites: &'a InkLimitSites,
    /// The declared `coverage_sample_resolution`, dpi: must equal the resolution every page was separated at.
    pub sample_resolution: f64,
    pub pages: &'a [CoveragePage<'a>],
}

/// The three metrics of STU-LAY-252, percent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverageMetrics {
    pub maximum: f64,
    pub mean: f64,
    /// Fraction of the evaluated region above the limit, percent of that region.
    pub area_above_limit: f64,
}

impl CoverageMetrics {
    pub fn get(&self, m: CoverageMetric) -> f64 {
        match m {
            CoverageMetric::Maximum => self.maximum,
            CoverageMetric::Mean => self.mean,
            CoverageMetric::AreaAboveLimit => self.area_above_limit,
        }
    }
}

/// The evaluation of one of the three limits.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitEvaluation {
    pub parameter: CoverageParameter,
    pub effective_limit: Option<f64>,
    pub limit_source: InkLimitSource,
    pub check_state: InkLimitCheckState,
    /// `None` when not checked.
    pub metrics: Option<CoverageMetrics>,
}

/// The STU-LAY-253 ink-limit finding.
#[derive(Clone, Debug, PartialEq)]
pub struct InkCoverageFinding {
    pub rule_id: String,
    /// Which of the three independent limits fired.
    pub limit: CoverageParameter,
    pub severity: Severity,
    pub check_state: InkLimitCheckState,
    pub effective_limit: f64,
    pub limit_source: InkLimitSource,
    pub measured_maximum: f64,
    pub measured_mean: f64,
    pub area_above_limit: f64,
    /// Every plate's tone at the worst location, percent.
    pub per_ink_contribution: Vec<InkReading>,
    pub page_reference: FindingPageRef,
    /// The objects whose ink is present at the worst location, in paint order.
    pub object_references: Vec<ObjectRef>,
    /// The stage that raised the value past the threshold; `None` when no enumerated stage did
    /// (authored process values) or the page is indeterminate. Only performed stages appear.
    pub introduced_by: Option<CoverageInputStage>,
    pub sample_resolution: f64,
}

/// The result of one evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct InkCoverageReport {
    pub rule_id: String,
    pub scope: InkLimitScope,
    pub severity: Severity,
    pub sample_resolution: f64,
    /// The `total_ink_limit` evaluation's state.
    pub check_state: InkLimitCheckState,
    /// Total, single-ink and black, in that order.
    pub evaluations: Vec<LimitEvaluation>,
    pub findings: Vec<InkCoverageFinding>,
    pub indeterminate_causes: Vec<IndeterminateCause>,
}

/// Typed refusals of the coverage contract (STU-LAY-166: stable code, message, field and value).
#[derive(Clone, Debug, PartialEq)]
pub enum CoverageError {
    UndeclaredIsNotASite,
    NotFinite { parameter: CoverageParameter, source: InkLimitSource },
    /// `hard_max` is the bound applied (scaled by the ink count for `total_ink_limit`; `Unknown` when
    /// no upper bound applies).
    OutOfHardBounds { parameter: CoverageParameter, source: InkLimitSource, value: f64, hard_min: ParamValue, hard_max: ParamValue },
    InvalidRuleId,
    InvalidPageReference,
    InvalidObjectReference,
    NoPages,
    TooManyPages,
    TooManyIndeterminateCauses,
    TooManySamples,
    TooManyObjectReferences,
    ScopeNotEvaluated(InkLimitScope),
    ScopeMismatch(InkLimitScope),
    InvalidSampleResolution(f64),
    SampleResolutionMismatch { declared: f64, separated: f64 },
    /// The replay at the worst location went over the separation's work or segment caps.
    Budget,
}

impl CoverageError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UndeclaredIsNotASite => "ink_limit_undeclared_is_not_a_site",
            Self::NotFinite { .. } => "ink_limit_not_finite",
            Self::OutOfHardBounds { .. } => "ink_limit_out_of_hard_bounds",
            Self::InvalidRuleId => "invalid_rule_id",
            Self::InvalidPageReference => "invalid_page_reference",
            Self::InvalidObjectReference => "invalid_object_reference",
            Self::NoPages => "no_coverage_pages",
            Self::TooManyPages => "too_many_coverage_pages",
            Self::TooManyIndeterminateCauses => "too_many_indeterminate_causes",
            Self::TooManySamples => "too_many_coverage_samples",
            Self::TooManyObjectReferences => "too_many_object_references",
            Self::ScopeNotEvaluated(_) => "ink_limit_scope_not_evaluated",
            Self::ScopeMismatch(_) => "ink_limit_scope_mismatch",
            Self::InvalidSampleResolution(_) => "invalid_sample_resolution",
            Self::SampleResolutionMismatch { .. } => "sample_resolution_mismatch",
            Self::Budget => "coverage_budget_exceeded",
        }
    }
    pub const fn message(&self) -> &'static str {
        match self {
            Self::UndeclaredIsNotASite => "ink_limit_source.undeclared is terminal; a value cannot be declared at it",
            Self::NotFinite { .. } => "an ink limit value must be a finite number",
            Self::OutOfHardBounds { .. } => "an ink limit value is outside its hard bounds",
            Self::InvalidRuleId => "the rule id must be non-blank and at most 256 bytes",
            Self::InvalidPageReference => "a page or spread identity must be non-blank and at most 256 bytes",
            Self::InvalidObjectReference => "an object reference must be non-blank and at most 1024 bytes",
            Self::NoPages => "an evaluation needs at least one page",
            Self::TooManyPages => "too many pages for one evaluation",
            Self::TooManyIndeterminateCauses => "too many indeterminate causes for one evaluation",
            Self::TooManySamples => "too many samples for one evaluation",
            Self::TooManyObjectReferences => "too many objects compose at the worst location",
            Self::ScopeNotEvaluated(_) => "this ink limit scope is not evaluated by the separations pipeline",
            Self::ScopeMismatch(_) => "the pages do not match the rule's ink limit scope",
            Self::InvalidSampleResolution(_) => "the sample resolution must be a finite positive dpi value",
            Self::SampleResolutionMismatch { .. } => "a page was separated at a different resolution than the declared sample resolution",
            Self::Budget => "the replay at the worst location exceeded the separation's caps",
        }
    }
    /// The field the refusal is about.
    pub const fn field(&self) -> Option<&'static str> {
        match self {
            Self::UndeclaredIsNotASite => Some("ink_limit_source"),
            Self::NotFinite { parameter, .. } | Self::OutOfHardBounds { parameter, .. } => Some(parameter.code()),
            Self::InvalidRuleId => Some("rule_id"),
            Self::InvalidPageReference => Some("page_reference"),
            Self::InvalidObjectReference | Self::TooManyObjectReferences => Some("object_references"),
            Self::NoPages | Self::TooManyPages | Self::TooManySamples => Some("pages"),
            Self::TooManyIndeterminateCauses => Some("indeterminate_causes"),
            Self::ScopeNotEvaluated(_) | Self::ScopeMismatch(_) => Some("ink_limit_scope"),
            Self::InvalidSampleResolution(_) | Self::SampleResolutionMismatch { .. } => Some("coverage_sample_resolution"),
            Self::Budget => None,
        }
    }
    /// The offending value, when numeric.
    pub const fn value(&self) -> Option<f64> {
        match self {
            Self::OutOfHardBounds { value, .. } => Some(*value),
            Self::InvalidSampleResolution(v) => Some(*v),
            Self::SampleResolutionMismatch { declared, .. } => Some(*declared),
            _ => None,
        }
    }
}

/// One published total ink limit figure (STU-LAY-254): REFERENCE ONLY -- not a default, not a
/// validated recommendation and not a Studio name; resolution never reads it. A range stays a range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PublishedInkLimit {
    pub print_condition: &'static str,
    pub low_percent: f64,
    pub high_percent: f64,
    /// A second figure published beside the first.
    pub variant_percent: Option<f64>,
    pub provenance: &'static str,
}

const fn published(print_condition: &'static str, low: f64, high: f64, variant: Option<f64>, provenance: &'static str) -> PublishedInkLimit {
    PublishedInkLimit { print_condition, low_percent: low, high_percent: high, variant_percent: variant, provenance }
}

/// The STU-LAY-254 reference table, as published.
pub const PUBLISHED_INK_LIMIT_REFERENCE: [PublishedInkLimit; 9] = [
    published(
        "Sheetfed offset, gloss and matte coated, paper types 1 and 2",
        330.0,
        330.0,
        Some(300.0),
        "ECI profiles built on the FOGRA39 characterisation for ISO 12647-2",
    ),
    published("Sheetfed offset, premium coated, later ISO 12647-2 revision", 300.0, 300.0, None, "PSO Coated v3, built on the FOGRA51 characterisation"),
    published("Sheetfed offset, US number 1 coated", 310.0, 310.0, None, "GRACoL 2006"),
    published("Web offset, US number 3 coated publication stock", 300.0, 300.0, None, "SWOP 2006"),
    published(
        "Sheetfed offset, wood-free uncoated",
        260.0,
        300.0,
        None,
        "ECI uncoated profiles on the FOGRA47 and FOGRA52 characterisations; field prepress guidance gives the wider figure",
    ),
    published("Heatset web offset, coated", 300.0, 320.0, None, "field prepress guidance"),
    published("Non-heatset web offset, newsprint", 240.0, 260.0, None, "field prepress guidance"),
    published("Sheetfed offset, Japanese coated", 300.0, 300.0, None, "Japan Color 2011"),
    published("Electrophotographic and inkjet devices", 300.0, 350.0, None, "field prepress guidance; strongly device and substrate dependent"),
];

fn text_ok(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max
}

/// Validates the request shape: rule id, page list and scope, page identities, cause list, sample
/// resolution and the scaled total hard maximum, all before any pixel is read.
fn validate(req: &CoverageRequest<'_>, totals: &ResolvedInkLimits) -> Result<u64, CoverageError> {
    if !text_ok(&req.rule.rule_id, MAX_RULE_ID_BYTES) {
        return Err(CoverageError::InvalidRuleId);
    }
    let pages = req.pages;
    if pages.is_empty() {
        return Err(CoverageError::NoPages);
    }
    if pages.len() > MAX_COVERAGE_PAGES {
        return Err(CoverageError::TooManyPages);
    }
    let scope = req.rule.scope;
    match scope {
        InkLimitScope::Object | InkLimitScope::Plate => return Err(CoverageError::ScopeNotEvaluated(scope)),
        InkLimitScope::Page if pages.len() != 1 => return Err(CoverageError::ScopeMismatch(scope)),
        InkLimitScope::Spread => {
            let first = pages[0].reference.spread.as_ref();
            if first.is_none() || pages.iter().any(|p| p.reference.spread.as_ref() != first) {
                return Err(CoverageError::ScopeMismatch(scope));
            }
        }
        _ => {}
    }
    let mut causes = 0usize;
    let mut samples = 0u64;
    let declared = req.sample_resolution;
    if !(declared.is_finite() && declared > 0.0) {
        return Err(CoverageError::InvalidSampleResolution(declared));
    }
    for p in pages {
        let r = &p.reference;
        if !text_ok(&r.page, MAX_PAGE_REF_BYTES) || r.spread.as_ref().is_some_and(|s| !text_ok(s, MAX_PAGE_REF_BYTES)) {
            return Err(CoverageError::InvalidPageReference);
        }
        causes = causes.checked_add(p.unmeasured.len()).filter(|c| *c <= MAX_INDETERMINATE_CAUSES).ok_or(CoverageError::TooManyIndeterminateCauses)?;
        if p.unmeasured.iter().any(|c| c.object().is_some_and(|o| !o.is_valid())) {
            return Err(CoverageError::InvalidObjectReference);
        }
        let separated = p.preview.sample_resolution();
        if (separated - declared).abs() > declared * 1e-9 {
            return Err(CoverageError::SampleResolutionMismatch { declared, separated });
        }
        let px = u64::try_from(p.preview.pixel_count()).map_err(|_| CoverageError::TooManySamples)?;
        samples = samples.checked_add(px).filter(|s| *s <= MAX_COVERAGE_SAMPLES).ok_or(CoverageError::TooManySamples)?;
        if let ResolvedLimit { value: Some(t), source } = totals.total_ink_limit {
            let hard_max = CoverageParameter::TotalInkLimit.hard_max_for_inks(p.preview.plates.len());
            CoverageParameter::TotalInkLimit.check(source, t, hard_max)?;
        }
    }
    Ok(samples)
}

/// The causes that make declared limits indeterminate: the caller's, then those derived from what
/// each preview noted it could not separate (each derived cause once).
fn indeterminate_causes(pages: &[CoveragePage<'_>]) -> Vec<IndeterminateCause> {
    let mut out: Vec<IndeterminateCause> = pages.iter().flat_map(|p| p.unmeasured.iter().cloned()).collect();
    let mut derived: Vec<IndeterminateCause> = Vec::new();
    for p in pages {
        let notes = &p.preview.notes;
        let found = [
            (p.preview.unseparated > 0 || notes.contains(&SepNote::Unseparated), IndeterminateCause::UnresolvableProfile { object: None }),
            (notes.contains(&SepNote::ShadingNotSeparated), IndeterminateCause::ShadingNotSeparated),
            (notes.contains(&SepNote::TransparencyFlattenedOpaque), IndeterminateCause::TransparencyNotFlattened),
            (notes.contains(&SepNote::PdfOverprintNotRead), IndeterminateCause::PdfOverprintNotRead),
        ];
        for (hit, cause) in found {
            if hit && !derived.contains(&cause) {
                derived.push(cause);
            }
        }
    }
    out.extend(derived);
    out
}

/// Evaluates the separated pages against the effective limits of `req.sites` (STU-LAY-250..254).
/// Read-only: the separations are never changed or re-separated.
pub fn evaluate_ink_coverage(req: &CoverageRequest<'_>) -> Result<InkCoverageReport, CoverageError> {
    let resolved = req.sites.resolve();
    let samples = validate(req, &resolved)?;
    let causes = indeterminate_causes(req.pages);
    let rule = &req.rule;
    let n = samples.max(1) as f64;
    let margin = resolved.coverage_warning_margin.value;
    let mut evaluations = Vec::with_capacity(3);
    let mut findings = Vec::new();
    for (parameter, measure) in [
        (CoverageParameter::TotalInkLimit, Measure::Total),
        (CoverageParameter::SingleInkLimit, Measure::SingleInk),
        (CoverageParameter::BlackInkLimit, Measure::Black),
    ] {
        let lim = resolved.get(parameter);
        let Some(limit) = lim.value.filter(|_| rule.severity != Severity::Disabled) else {
            evaluations.push(LimitEvaluation {
                parameter,
                effective_limit: lim.value,
                limit_source: lim.source,
                check_state: InkLimitCheckState::NotChecked,
                metrics: None,
            });
            continue;
        };
        // Metrics over the evaluated region; the worst location is the first maximum in page order,
        // then row-major.
        let (mut max, mut worst, mut sum, mut above) = (f64::NEG_INFINITY, (0usize, 0usize), 0.0f64, 0u64);
        for (pi, p) in req.pages.iter().enumerate() {
            for i in 0..p.preview.pixel_count() {
                let v = p.preview.measure_at(measure, i);
                if v > max {
                    max = v;
                    worst = (pi, i);
                }
                sum += v;
                if v > limit + LIMIT_EPS {
                    above = above.checked_add(1).ok_or(CoverageError::TooManySamples)?;
                }
            }
        }
        let max = max.max(0.0);
        let metrics = CoverageMetrics { maximum: max, mean: sum / n, area_above_limit: above as f64 / n * 100.0 };
        let in_margin = margin.is_some_and(|m| max > limit - m + LIMIT_EPS && max > LIMIT_EPS);
        let check_state = if !causes.is_empty() {
            InkLimitCheckState::Indeterminate
        } else if max > limit + LIMIT_EPS {
            InkLimitCheckState::OverLimit
        } else if in_margin {
            InkLimitCheckState::WithinWarningMargin
        } else {
            InkLimitCheckState::WithinLimit
        };
        evaluations.push(LimitEvaluation { parameter, effective_limit: Some(limit), limit_source: lim.source, check_state, metrics: Some(metrics) });
        if check_state == InkLimitCheckState::WithinLimit {
            continue;
        }
        let threshold = match check_state {
            InkLimitCheckState::WithinWarningMargin => (limit - margin.unwrap_or(0.0)).max(0.0),
            _ => limit,
        };
        let page = &req.pages[worst.0];
        let preview = page.preview;
        // The offending region: the bounding box of the worst page's locations past the threshold.
        let w = (preview.width as usize).max(1);
        let mut bounds: Option<(usize, usize, usize, usize)> = None;
        for i in 0..preview.pixel_count() {
            if preview.measure_at(measure, i) > threshold + LIMIT_EPS {
                let (x, y) = (i.checked_rem(w).unwrap_or(0), i.checked_div(w).unwrap_or(0));
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
        let region = bounds.map(|b| {
            let [x0, y0, x1, y1] = preview.pixel_box(b);
            DocRegion { x0, y0, x1, y1 }
        });
        let attribution = preview.attribute(worst.1, measure, threshold, MAX_COVERAGE_OBJECT_REFS).map_err(|_| CoverageError::Budget)?;
        if attribution.over_cap {
            return Err(CoverageError::TooManyObjectReferences);
        }
        let introduced_by = if check_state == InkLimitCheckState::Indeterminate { None } else { attribution.stage.filter(|s| s.performed()) };
        findings.push(InkCoverageFinding {
            rule_id: rule.rule_id.clone(),
            limit: parameter,
            severity: if check_state == InkLimitCheckState::WithinWarningMargin { rule.severity.lowered() } else { rule.severity },
            check_state,
            effective_limit: limit,
            limit_source: lim.source,
            measured_maximum: metrics.maximum,
            measured_mean: metrics.mean,
            area_above_limit: metrics.area_above_limit,
            per_ink_contribution: preview.readings_at(worst.1),
            page_reference: FindingPageRef { page: page.reference.clone(), region },
            object_references: attribution.objects,
            introduced_by,
            sample_resolution: req.sample_resolution,
        });
    }
    let check_state = evaluations.first().map_or(InkLimitCheckState::NotChecked, |e| e.check_state);
    Ok(InkCoverageReport {
        rule_id: rule.rule_id.clone(),
        scope: rule.scope,
        severity: rule.severity,
        sample_resolution: req.sample_resolution,
        check_state,
        evaluations,
        findings,
        indeterminate_causes: causes,
    })
}

// ---- Observe (HOST INTEGRATION rule 5, F-R51-7) ----

/// The caller-owned route of one terminal emission: the correlation and revision of the operation,
/// its cancellation token, the one [`Observe`] emitter of this operation and the sink it delivers to.
pub struct ObserveRoute<'a> {
    pub correlation_id: u64,
    pub revision: u64,
    pub cancel: &'a CancellationToken,
    pub observe: &'a mut Observe,
    pub sink: &'a mut dyn SinkPort,
}

/// What the terminal emission of one operation was: the typed terminal code and the delivery receipt.
#[derive(Debug, PartialEq)]
pub struct Emission {
    pub code: &'static str,
    pub receipt: Result<Receipt, hsk_studio_observe::Error>,
}

/// [`evaluate_ink_coverage`] plus one terminal emission through observe.
pub fn evaluate_ink_coverage_observed(
    req: &CoverageRequest<'_>,
    route: ObserveRoute<'_>,
) -> (Result<InkCoverageReport, CoverageError>, Emission) {
    let _ = route;
    (evaluate_ink_coverage(req), Emission { code: "", receipt: Err(hsk_studio_observe::Error::Closed) })
}

// ---- JSON (data form of the records; field names are the spec's) ----

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_num(out: &mut String, v: f64) {
    if v.is_finite() {
        out.push_str(&format!("{v}"));
    } else {
        out.push_str("null");
    }
}

fn json_opt_num(out: &mut String, v: Option<f64>) {
    match v {
        Some(v) => json_num(out, v),
        None => out.push_str("null"),
    }
}

fn json_key(out: &mut String, first: &mut bool, key: &str) {
    if !*first {
        out.push(',');
    }
    *first = false;
    json_str(out, key);
    out.push(':');
}

impl InkCoverageFinding {
    /// The finding as one JSON object with the STU-LAY-253 field names (plus `limit`).
    pub fn to_json(&self) -> String {
        let mut o = String::from("{");
        let f = &mut true;
        json_key(&mut o, f, "rule_id");
        json_str(&mut o, &self.rule_id);
        json_key(&mut o, f, "limit");
        json_str(&mut o, self.limit.code());
        json_key(&mut o, f, "severity");
        json_str(&mut o, self.severity.code());
        json_key(&mut o, f, "check_state");
        json_str(&mut o, self.check_state.code());
        json_key(&mut o, f, "effective_limit");
        json_num(&mut o, self.effective_limit);
        json_key(&mut o, f, "limit_source");
        json_str(&mut o, self.limit_source.code());
        json_key(&mut o, f, "measured_maximum");
        json_num(&mut o, self.measured_maximum);
        json_key(&mut o, f, "measured_mean");
        json_num(&mut o, self.measured_mean);
        json_key(&mut o, f, "area_above_limit");
        json_num(&mut o, self.area_above_limit);
        json_key(&mut o, f, "per_ink_contribution");
        o.push('[');
        for (k, r) in self.per_ink_contribution.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            o.push_str("{\"ink\":");
            json_str(&mut o, &r.ink);
            o.push_str(",\"percent\":");
            json_num(&mut o, r.percent);
            o.push('}');
        }
        o.push(']');
        json_key(&mut o, f, "page_reference");
        o.push_str("{\"page\":");
        json_str(&mut o, &self.page_reference.page.page);
        o.push_str(",\"spread\":");
        match &self.page_reference.page.spread {
            Some(s) => json_str(&mut o, s),
            None => o.push_str("null"),
        }
        o.push_str(",\"region\":");
        match &self.page_reference.region {
            Some(r) => {
                let g = &mut true;
                o.push('{');
                for (k, v) in [("x0", r.x0), ("y0", r.y0), ("x1", r.x1), ("y1", r.y1)] {
                    json_key(&mut o, g, k);
                    json_num(&mut o, v);
                }
                o.push('}');
            }
            None => o.push_str("null"),
        }
        o.push('}');
        json_key(&mut o, f, "object_references");
        o.push('[');
        for (k, r) in self.object_references.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            r.json(&mut o);
        }
        o.push(']');
        json_key(&mut o, f, "introduced_by");
        match self.introduced_by {
            Some(s) => json_str(&mut o, s.code()),
            None => o.push_str("null"),
        }
        json_key(&mut o, f, "sample_resolution");
        json_num(&mut o, self.sample_resolution);
        o.push('}');
        o
    }
}

impl InkCoverageReport {
    /// The report as one JSON object: rule, scope, severity, total check state, sample resolution,
    /// the three limit evaluations, the findings and the indeterminate causes.
    pub fn to_json(&self) -> String {
        let mut o = String::from("{");
        let f = &mut true;
        json_key(&mut o, f, "rule_id");
        json_str(&mut o, &self.rule_id);
        json_key(&mut o, f, "scope");
        json_str(&mut o, self.scope.code());
        json_key(&mut o, f, "severity");
        json_str(&mut o, self.severity.code());
        json_key(&mut o, f, "check_state");
        json_str(&mut o, self.check_state.code());
        json_key(&mut o, f, "sample_resolution");
        json_num(&mut o, self.sample_resolution);
        json_key(&mut o, f, "evaluations");
        o.push('[');
        for (k, e) in self.evaluations.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            let g = &mut true;
            o.push('{');
            json_key(&mut o, g, "parameter");
            json_str(&mut o, e.parameter.code());
            json_key(&mut o, g, "effective_limit");
            json_opt_num(&mut o, e.effective_limit);
            json_key(&mut o, g, "limit_source");
            json_str(&mut o, e.limit_source.code());
            json_key(&mut o, g, "check_state");
            json_str(&mut o, e.check_state.code());
            json_key(&mut o, g, "metrics");
            match &e.metrics {
                Some(m) => {
                    let h = &mut true;
                    o.push('{');
                    for metric in CoverageMetric::ALL {
                        json_key(&mut o, h, metric.code());
                        json_num(&mut o, m.get(metric));
                    }
                    o.push('}');
                }
                None => o.push_str("null"),
            }
            o.push('}');
        }
        o.push(']');
        json_key(&mut o, f, "findings");
        o.push('[');
        for (k, finding) in self.findings.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            o.push_str(&finding.to_json());
        }
        o.push(']');
        json_key(&mut o, f, "indeterminate_causes");
        o.push('[');
        for (k, c) in self.indeterminate_causes.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            o.push_str("{\"cause\":");
            json_str(&mut o, c.code());
            o.push_str(",\"object\":");
            match c.object() {
                Some(r) => r.json(&mut o),
                None => o.push_str("null"),
            }
            o.push('}');
        }
        o.push(']');
        o.push('}');
        o
    }
}
