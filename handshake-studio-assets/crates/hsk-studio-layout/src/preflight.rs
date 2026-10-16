//! Live preflight (lane SMB-LAYOUT-UI-2; STU-LAY-055/057/154): one run validates a document
//! against a profile and returns STRUCTURED findings (rule id, severity, offending object, page,
//! detail), an error/warning/informational count and a status, limitable to a page list. Every rule
//! reads results the layout and Folio engines already produce; nothing is re-derived here:
//!
//! | rule id | category | source |
//! |---|---|---|
//! | `text.overset` | Text | the caller's `LayoutProposal::overset` per story (last frame, its page) |
//! | `text.composition_refused` | Text | the caller's typed layout refusal per story |
//! | `text.missing_style` | Text | paragraph / character style refs absent from the type-style set |
//! | `text.double_space` | Text (text patterns) | two consecutive spaces in story text |
//! | `document.blank_page` | Document | a page with no frame and no applied parent |
//! | `objects.outside_page` | Images and objects (bleed-zone hazard) | a frame not inside its page + bleed |
//! | `data.xref_stale` / `data.xref_broken` | Accessibility and data | `resolve_cross_references` status |
//! | `document.parent_records` | Document | the parent adapter's typed refusal |
//!
//! Severities follow the STU-LAY-154 flag (`disabled`, `error`, `warning`, `informational`).
//! [`PreflightProfile::basic`] enables every rule. Declared gaps: links/resources, colour, ink,
//! image resolution and transparency rules, missing fonts/glyphs, spelling, page size/count rules,
//! book options; profiles are not stored in Folio (no record) and are not exported; InDesign rule
//! equality UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::{LayoutError, LayoutProposal, PageNumber, XrefContext, XrefStatus, XrefStory, resolve_cross_references};
use hsk_studio_folio::{
    Payload, StudioDocument, StudioLayoutFlowRecords, StudioParentPageRecords, StudioTextStory, StudioTypeStyleRecords,
};
use std::collections::BTreeSet;

/// Findings per run (`preflight_budget_exceeded` beyond).
pub const MAX_PREFLIGHT_FINDINGS: usize = 100_000;

/// STU-LAY-154 `preflight_rule_flag`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Disabled,
    Error,
    Warning,
    Informational,
}
impl Severity {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Disabled => "severity.disabled",
            Self::Error => "severity.error",
            Self::Warning => "severity.warning",
            Self::Informational => "severity.informational",
        }
    }
}

/// The registered rules (rule id, default severity).
pub const PREFLIGHT_RULES: [(&str, Severity); 9] = [
    ("text.overset", Severity::Error),
    ("text.composition_refused", Severity::Error),
    ("text.missing_style", Severity::Error),
    ("text.double_space", Severity::Informational),
    ("document.blank_page", Severity::Warning),
    ("objects.outside_page", Severity::Warning),
    ("data.xref_stale", Severity::Warning),
    ("data.xref_broken", Severity::Error),
    ("document.parent_records", Severity::Error),
];

/// A rule instance of a profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreflightRuleInstance {
    pub rule_id: String,
    pub severity: Severity,
}

/// A named set of rule instances.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreflightProfile {
    pub name: String,
    pub rules: Vec<PreflightRuleInstance>,
}
impl PreflightProfile {
    /// Every registered rule at its default severity.
    pub fn basic() -> Self {
        Self {
            name: "[Basic]".into(),
            rules: PREFLIGHT_RULES.iter().map(|(id, s)| PreflightRuleInstance { rule_id: (*id).to_owned(), severity: *s }).collect(),
        }
    }
    fn severity(&self, rule: &str) -> Severity {
        self.rules.iter().find(|r| r.rule_id == rule).map_or(Severity::Disabled, |r| r.severity)
    }
}

/// One finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreflightFinding {
    pub rule_id: String,
    pub severity: Severity,
    /// Offending object: a layer, story, page or cross-reference id.
    pub object: String,
    /// Page key, when the object is on a page.
    pub page: Option<String>,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreflightReport {
    pub profile: String,
    pub findings: Vec<PreflightFinding>,
    pub errors: usize,
    pub warnings: usize,
    pub informational: usize,
}
impl PreflightReport {
    /// `true` when no error-severity finding is present (the status indicator).
    pub fn ok(&self) -> bool {
        self.errors == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreflightError {
    /// A profile names a rule that is not registered.
    UnknownRule(String),
    /// A page in the scope is not in the document.
    UnknownPage(String),
    BudgetExceeded,
}
impl PreflightError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownRule(_) => "preflight_unknown_rule",
            Self::UnknownPage(_) => "preflight_unknown_page",
            Self::BudgetExceeded => "preflight_budget_exceeded",
        }
    }
}

/// What one run reads. `compositions` holds the caller's reflow result per story id (stories
/// without an entry are not composed and skip the composition rules).
pub struct PreflightInput<'a> {
    pub document: &'a StudioDocument,
    pub stories: &'a [StudioTextStory],
    pub compositions: &'a [(String, Result<LayoutProposal, LayoutError>)],
    pub styles: &'a StudioTypeStyleRecords,
    pub flow: &'a StudioLayoutFlowRecords,
    /// Parent records and the adapter's verdict on them (`Err` = its code).
    pub parents: Option<(&'a StudioParentPageRecords, Result<(), &'static str>)>,
    pub pages: &'a [PageNumber],
}

fn story_text(s: &StudioTextStory) -> &str {
    match &s.content {
        hsk_studio_folio::StoryContent::Inline { text_utf8 } => text_utf8,
        hsk_studio_folio::StoryContent::Stored { .. } => "",
    }
}

/// Runs `profile` over the input; `scope` limits page-bound findings to these pages (`None` = all).
pub fn run_preflight(input: &PreflightInput<'_>, profile: &PreflightProfile, scope: Option<&[String]>) -> Result<PreflightReport, PreflightError> {
    for r in &profile.rules {
        if !PREFLIGHT_RULES.iter().any(|(id, _)| *id == r.rule_id) {
            return Err(PreflightError::UnknownRule(r.rule_id.clone()));
        }
    }
    if let Some(pages) = scope {
        for p in pages {
            if input.document.locate_page(p).is_none() {
                return Err(PreflightError::UnknownPage(p.clone()));
            }
        }
    }
    let mut out: Vec<PreflightFinding> = Vec::new();
    let mut push = |rule: &str, object: &str, page: Option<String>, detail: String| -> Result<(), PreflightError> {
        let severity = profile.severity(rule);
        if severity == Severity::Disabled {
            return Ok(());
        }
        if let (Some(scope), Some(p)) = (scope, &page)
            && !scope.contains(p)
        {
            return Ok(());
        }
        if out.len() >= MAX_PREFLIGHT_FINDINGS {
            return Err(PreflightError::BudgetExceeded);
        }
        out.push(PreflightFinding { rule_id: rule.to_owned(), severity, object: object.to_owned(), page, detail });
        Ok(())
    };
    let frame_page = |frame: &str| -> Option<String> {
        input.document.layers.iter().find(|l| l.layer_id == frame).and_then(|l| l.text_frame.as_ref()).map(|f| f.page_id.clone())
    };
    // Text: composition.
    for (story_id, result) in input.compositions {
        match result {
            Ok(p) => {
                if let Some(o) = p.overset {
                    let last = p.frames.last().map(|f| f.frame_id.clone()).unwrap_or_else(|| story_id.clone());
                    push("text.overset", &last, frame_page(&last), format!("story {story_id} bytes {}..{} do not fit", o.start, o.end))?;
                }
            }
            Err(e) => {
                let first = input.document.layers.iter().find(|l| matches!(&l.payload, Payload::Text { story_id: s } if s == story_id) && l.text_frame.is_some());
                let page = first.and_then(|l| l.text_frame.as_ref()).map(|f| f.page_id.clone());
                push("text.composition_refused", story_id, page, e.code().to_owned())?;
            }
        }
    }
    // Text: style references and patterns.
    for s in input.stories {
        for (n, p) in s.paragraphs.iter().enumerate() {
            if let Some(r) = &p.style_ref
                && input.styles.style(r).is_none()
            {
                push("text.missing_style", &s.story_id, None, format!("paragraph {n} style {r}"))?;
            }
        }
        for (n, c) in s.character_ranges.iter().enumerate() {
            if let Some(r) = &c.style_ref
                && input.styles.style(r).is_none()
            {
                push("text.missing_style", &s.story_id, None, format!("range {n} style {r}"))?;
            }
        }
        let text = story_text(s);
        let mut from = 0usize;
        while let Some(i) = text.get(from..).and_then(|t| t.find("  ")) {
            let at = from.checked_add(i).ok_or(PreflightError::BudgetExceeded)?;
            push("text.double_space", &s.story_id, None, format!("byte {at}"))?;
            let run = text[at..].bytes().take_while(|b| *b == b' ').count();
            from = at.checked_add(run.max(2)).ok_or(PreflightError::BudgetExceeded)?;
        }
    }
    // Document: blank pages and frames outside their page.
    let parent_pages: BTreeSet<&str> = input
        .parents
        .as_ref()
        .map(|(r, _)| r.page_bindings.iter().filter(|b| b.applied_parent.is_some()).map(|b| b.page_id.as_str()).collect())
        .unwrap_or_default();
    for spread in &input.document.page_spreads {
        for page in &spread.pages {
            let used = input.document.layers.iter().any(|l| l.text_frame.as_ref().is_some_and(|f| f.page_id == page.page_id));
            if !used && !parent_pages.contains(page.page_id.as_str()) {
                push("document.blank_page", &page.page_id, Some(page.page_id.clone()), "no frame and no parent".into())?;
            }
        }
    }
    for l in &input.document.layers {
        let Some(f) = &l.text_frame else { continue };
        let Some(loc) = input.document.locate_page(&f.page_id) else { continue };
        let bleed = |pick: fn(&hsk_studio_folio::PageBleed) -> f64| loc.page.bleed.as_ref().map_or(0.0, pick);
        let b = &f.bounds;
        let inside = b.x.value >= -bleed(|e| e.inside_or_left.value)
            && b.y.value >= -bleed(|e| e.top.value)
            && b.x.value + b.width.value <= loc.page.width.value + bleed(|e| e.outside_or_right.value)
            && b.y.value + b.height.value <= loc.page.height.value + bleed(|e| e.bottom.value);
        if !inside {
            push("objects.outside_page", &l.layer_id, Some(f.page_id.clone()), "frame extends past the page and bleed".into())?;
        }
    }
    // Data: cross-references, through the xref resolver.
    if !input.flow.cross_references.is_empty() {
        let placements: Vec<(&StudioTextStory, Option<&LayoutProposal>)> = input
            .stories
            .iter()
            .map(|s| (s, input.compositions.iter().find(|(id, _)| *id == s.story_id).and_then(|(_, r)| r.as_ref().ok())))
            .collect();
        let xs: Vec<XrefStory<'_>> = placements.iter().map(|(s, p)| XrefStory { story: s, text: story_text(s), placement: *p }).collect();
        let ctx = XrefContext { file_name: "", chapter_number: None, pages: input.pages, type_styles: Some(input.styles) };
        match resolve_cross_references(input.flow, &xs, &ctx) {
            Ok(res) => {
                for r in res {
                    let rule = match r.status {
                        XrefStatus::Stale => "data.xref_stale",
                        XrefStatus::Broken => "data.xref_broken",
                        XrefStatus::Current | XrefStatus::Unplaced => continue,
                    };
                    push(rule, &r.xref_id, None, format!("{:?}", r.status).to_lowercase())?;
                }
            }
            Err(e) => push("data.xref_broken", "cross_references", None, e.code().to_owned())?,
        }
    }
    if let Some((_, Err(code))) = &input.parents {
        push("document.parent_records", "parent_page_records", None, (*code).to_owned())?;
    }
    let count = |s: Severity| out.iter().filter(|f| f.severity == s).count();
    Ok(PreflightReport {
        profile: profile.name.clone(),
        errors: count(Severity::Error),
        warnings: count(Severity::Warning),
        informational: count(Severity::Informational),
        findings: out,
    })
}
