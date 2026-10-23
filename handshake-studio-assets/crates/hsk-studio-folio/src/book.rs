//! Book records (`hsk.studio.book@1`, lane SMB-LAYOUT-UI; STU-LAY-042, STU-LAY-043, STU-LAY-147):
//! a book binds chapter `StudioDocument`s (by `SDOC` id, in order) into one publication with a
//! style-source document, the seventeen synchronisation booleans (all carried, so a partial
//! synchronisation is expressible), the smart style-group matching rule, repagination
//! (continue / next odd / next even), blank-page insertion and the automatic pagination,
//! conversion and layer-merge flags. Each chapter carries status, page range, date, size and file
//! path as the capture lists them. Folio stores and validates; pagination, the combined document,
//! TOC and export belong to hsk-studio-layout / hsk-studio-interop-layout.
//!
//! Hostile wire JSON: integer arithmetic is denied (workflow V4-01); the chapter count is bounded
//! (`MAX_BOOK_CHAPTERS`) before anything is indexed.
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, Result, StudioDocument, canceled, err, id};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(BookSchema, "hsk.studio.book@1");

/// Chapters per book (a Folio hard ceiling).
pub const MAX_BOOK_CHAPTERS: usize = 256;

fn is_false(b: &bool) -> bool {
    !*b
}

/// STU-LAY-147 seventeen independent synchronisation booleans.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct StudioBookSync {
    pub sync_paragraph_styles: bool,
    pub sync_character_styles: bool,
    pub sync_object_styles: bool,
    pub sync_table_styles: bool,
    pub sync_cell_styles: bool,
    pub sync_toc_styles: bool,
    pub sync_swatches: bool,
    pub sync_text_variables: bool,
    pub sync_bullet_numbering_lists: bool,
    pub sync_cross_reference_formats: bool,
    pub sync_conditional_text: bool,
    pub sync_parent_pages: bool,
    pub sync_trap_styles: bool,
    pub sync_mojikumi_styles: bool,
    pub sync_kinsoku_styles: bool,
    pub sync_composite_fonts: bool,
    pub sync_named_grids: bool,
}

/// STU-LAY-147 `smart_match_style_groups`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioSmartMatch {
    #[default]
    ByStylePath,
    ByStyleName,
}

/// STU-LAY-147 `repagination_option`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioRepagination {
    #[default]
    Continue,
    ContinueOnNextOdd,
    ContinueOnNextEven,
}

/// Book-content status of a chapter (Studio's members; the capture names the enumeration only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBookStatus {
    #[default]
    Normal,
    Missing,
    Modified,
    InUse,
}

/// One chapter document of a book.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBookChapter {
    /// `SDOC` id of the chapter document.
    pub document_id: String,
    #[serde(default)]
    pub status: StudioBookStatus,
    /// e.g. "1-12" (derived by pagination; stored as text, as captured).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub document_page_range: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

/// STU-LAY-042/147 book.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBook {
    pub schema_id: BookSchema,
    /// Document-local key of the book.
    pub book_id: String,
    pub name: String,
    pub chapters: Vec<StudioBookChapter>,
    /// `SDOC` id of the chapter whose styles win.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_source_document: Option<String>,
    #[serde(default)]
    pub sync: StudioBookSync,
    #[serde(default)]
    pub smart_match_style_groups: StudioSmartMatch,
    #[serde(default)]
    pub repagination_option: StudioRepagination,
    #[serde(default, skip_serializing_if = "is_false")]
    pub insert_blank_page: bool,
    /// Off: every chapter keeps its own numbering.
    pub automatic_pagination: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub automatic_document_conversion: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub merge_identical_layers: bool,
}
impl StudioBook {
    /// An empty book with automatic pagination on.
    pub fn new(book_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema_id: BookSchema::V1,
            book_id: book_id.into(),
            name: name.into(),
            chapters: Vec::new(),
            style_source_document: None,
            sync: StudioBookSync::default(),
            smart_match_style_groups: StudioSmartMatch::ByStylePath,
            repagination_option: StudioRepagination::Continue,
            insert_blank_page: false,
            automatic_pagination: true,
            automatic_document_conversion: false,
            merge_identical_layers: false,
        }
    }
    /// Add / remove / move (STU-LAY-042): append a chapter (duplicate refused).
    pub fn add_chapter(&mut self, document_id: &str) -> Result<()> {
        if self.chapters.iter().any(|c| c.document_id == document_id) {
            return Err(err(Code::DuplicateIdentity, document_id));
        }
        if self.chapters.len() >= MAX_BOOK_CHAPTERS {
            return Err(err(Code::BudgetExceeded, "chapters"));
        }
        self.chapters.push(StudioBookChapter {
            document_id: document_id.into(),
            status: StudioBookStatus::Normal,
            document_page_range: String::new(),
            date: None,
            size: None,
            file_path: None,
        });
        Ok(())
    }
    /// Removes a chapter (and the style source when it was that chapter).
    pub fn remove_chapter(&mut self, document_id: &str) -> Result<()> {
        let i = self
            .chapters
            .iter()
            .position(|c| c.document_id == document_id)
            .ok_or_else(|| err(Code::DanglingReference, document_id))?;
        self.chapters.remove(i);
        if self.style_source_document.as_deref() == Some(document_id) {
            self.style_source_document = None;
        }
        Ok(())
    }
    /// Moves a chapter to position `to` (clamped to the end).
    pub fn move_chapter(&mut self, document_id: &str, to: usize) -> Result<()> {
        let i = self
            .chapters
            .iter()
            .position(|c| c.document_id == document_id)
            .ok_or_else(|| err(Code::DanglingReference, document_id))?;
        let c = self.chapters.remove(i);
        let at = to.min(self.chapters.len());
        self.chapters.insert(at, c);
        Ok(())
    }
}

/// JSON schema (draft 2020-12) of the book record.
pub fn book_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<StudioBook>()
}

/// Validates a book: bounded and unique chapters with `SDOC` ids, the style source among them, a
/// non-empty name; with `documents`, every chapter must be one of them (a missing chapter is
/// `DanglingReference`).
pub fn validate_book(b: &StudioBook, documents: Option<&[&StudioDocument]>, token: &CancellationToken) -> Result<()> {
    canceled(token)?;
    if b.name.is_empty() || b.name.chars().count() > 256 || b.book_id.is_empty() || b.book_id.len() > 128 {
        return Err(err(Code::InvalidField, "book"));
    }
    if b.chapters.len() > MAX_BOOK_CHAPTERS {
        return Err(err(Code::BudgetExceeded, "chapters"));
    }
    let mut seen = BTreeSet::new();
    for c in &b.chapters {
        canceled(token)?;
        id(&c.document_id, "SDOC", "chapters/document_id")?;
        if !seen.insert(c.document_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &c.document_id));
        }
        if let Some(docs) = documents
            && !docs.iter().any(|d| d.document_id == c.document_id)
        {
            return Err(err(Code::DanglingReference, &c.document_id));
        }
    }
    if let Some(s) = &b.style_source_document
        && !seen.contains(s.as_str())
    {
        return Err(err(Code::DanglingReference, s));
    }
    Ok(())
}
