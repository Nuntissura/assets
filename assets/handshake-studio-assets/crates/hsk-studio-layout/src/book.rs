//! Books (lane SMB-LAYOUT-UI slice 5; STU-LAY-042/043/147): `combine_book` binds the chapter
//! documents of a Folio `StudioBook` into ONE combined document (spreads and layers in chapter
//! order, page keys prefixed `c<k>-`, sections renamed the same way) with the book's sections, so
//! every existing long-document engine runs over the whole book: `number_pages` gives continuous
//! page numbers, `generate_toc` a combined TOC over all chapters' stories, and the PDF export one
//! file (page labels from `page_labels_from_sections`).
//!
//! Pagination (`automatic_pagination` on): a chapter whose first section continues numbering (or
//! that has no sections) continues from the previous chapter's last page; `continue_on_next_odd` /
//! `continue_on_next_even` move a chapter start of the wrong parity by one: with
//! `insert_blank_page` a blank page (the size of the chapter's first page) is inserted before the
//! chapter, otherwise the number skips. With `automatic_pagination` off every chapter keeps its own
//! numbering (a chapter without sections restarts at 1). Chapter sections keep their styles and
//! restarts; prefixes must stay unique across the book (Folio `check_sections`). InDesign book
//! pagination parity is UNVERIFIED (D5).
//!
//! Bounds (hostile books): at most `MAX_BOOK_CHAPTERS` chapters and `MAX_BOOK_PAGES` pages in
//! total, checked before the combined document is built; identity collisions between chapters
//! (layer, spread, artboard or sequence ids) are refused, never renamed.
#![deny(clippy::arithmetic_side_effects)]
use crate::numbering::{NumberingError, PageNumber, number_pages};
use hsk_studio_folio::{
    Bounds, GeometryUnit, Length, PageSpreadSchema, Parent, StudioBook, StudioDocument, StudioPage, StudioPageSpread,
    StudioRepagination, StudioSection,
};
use std::collections::BTreeSet;

/// Pages of a combined book (a layout hard ceiling).
pub const MAX_BOOK_PAGES: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BookError {
    /// STU-LAY-147: the book does not contain one or more documents named for export.
    DocumentNotInBook(String),
    /// STU-LAY-147: the requested export format is unsupported for a book.
    ExportFormatUnsupported(String),
    /// A chapter document was not supplied, or its sections do not belong to it.
    ChapterMissing(String),
    /// Two chapters share a layer, spread, artboard or sequence id.
    IdentityCollision(String),
    Numbering(NumberingError),
    BudgetExceeded,
}
impl BookError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::DocumentNotInBook(_) => "book_document_not_in_book",
            Self::ExportFormatUnsupported(_) => "book_export_format_unsupported",
            Self::ChapterMissing(_) => "book_chapter_missing",
            Self::IdentityCollision(_) => "book_identity_collision",
            Self::Numbering(_) => "book_numbering",
            Self::BudgetExceeded => "budget_exceeded",
        }
    }
}

/// One chapter: its document and its sections (Folio long-document records).
#[derive(Clone, Copy, Debug)]
pub struct BookChapter<'a> {
    pub document: &'a StudioDocument,
    pub sections: &'a [StudioSection],
}

/// The combined book.
#[derive(Clone, Debug, PartialEq)]
pub struct CombinedBook {
    pub document: StudioDocument,
    pub sections: Vec<StudioSection>,
    /// Continuous page numbers of the combined document.
    pub pages: Vec<PageNumber>,
    /// Per chapter (book order): its first and last combined page index (blank pages excluded).
    pub chapter_pages: Vec<(String, usize, usize)>,
    /// Combined page keys of inserted blank pages.
    pub blank_pages: Vec<String>,
}

/// Combined page key of a chapter page.
pub fn book_page_key(chapter: usize, page_id: &str) -> String {
    format!("c{chapter}-{page_id}")
}

fn blank_spread_id(k: usize) -> String {
    // A deterministic SPGS id in the UUIDv7 shape Accord accepts (version 7, variant 8).
    format!("SPGS-019a5a10-b00c-7000-8000-{:012x}", k)
}

/// Builds the combined book (module docs). `chapters` must hold every book chapter's document.
pub fn combine_book(
    book: &StudioBook,
    chapters: &[BookChapter<'_>],
    document_id: &str,
) -> Result<CombinedBook, BookError> {
    if book.chapters.len() > hsk_studio_folio::MAX_BOOK_CHAPTERS {
        return Err(BookError::BudgetExceeded);
    }
    let mut total = 0usize;
    let mut ordered = Vec::with_capacity(book.chapters.len());
    for c in &book.chapters {
        let ch = chapters
            .iter()
            .find(|x| x.document.document_id == c.document_id)
            .ok_or_else(|| BookError::ChapterMissing(c.document_id.clone()))?;
        for s in ch.sections {
            if ch.document.locate_page(&s.page_start).is_none() {
                return Err(BookError::ChapterMissing(c.document_id.clone()));
            }
        }
        let pages: usize = ch.document.page_spreads.iter().map(|s| s.pages.len()).sum();
        // One possible blank page per chapter counts against the budget too.
        total = total.checked_add(pages).and_then(|t| t.checked_add(1)).ok_or(BookError::BudgetExceeded)?;
        if total > MAX_BOOK_PAGES.saturating_add(book.chapters.len()) {
            return Err(BookError::BudgetExceeded);
        }
        ordered.push(*ch);
    }
    let first = ordered.first().ok_or_else(|| BookError::ChapterMissing(String::new()))?;
    let mut doc = StudioDocument {
        document_id: document_id.to_owned(),
        page_spreads: Vec::new(),
        layers: Vec::new(),
        artboards: Vec::new(),
        sequences: Vec::new(),
        ..first.document.clone()
    };
    doc.graph.operations.clear();
    doc.graph.edges.clear();
    doc.graph.outputs.clear();
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut claim = |id: &str| -> Result<(), BookError> {
        if ids.insert(id.to_owned()) { Ok(()) } else { Err(BookError::IdentityCollision(id.to_owned())) }
    };
    let mut sections: Vec<StudioSection> = Vec::new();
    let mut chapter_pages = Vec::new();
    let mut blank_pages = Vec::new();
    let mut page_index = 0usize;
    for (k, ch) in ordered.iter().enumerate() {
        let d = ch.document;
        let first_page = d.page_spreads.iter().flat_map(|s| s.pages.iter()).next();
        // Where this chapter would start (numbers so far).
        let last = if sections.is_empty() && k == 0 {
            0
        } else {
            number_pages(&doc, &sections).map_err(BookError::Numbering)?.last().map_or(0, |p| p.number)
        };
        let mut own: Vec<StudioSection> = ch
            .sections
            .iter()
            .map(|s| StudioSection {
                section_id: book_page_key(k, &s.section_id),
                page_start: book_page_key(k, &s.page_start),
                ..s.clone()
            })
            .collect();
        let starts_own = first_page.is_some_and(|p| ch.sections.iter().any(|s| s.page_start == p.page_id));
        if let Some(p) = first_page
            && !starts_own
        {
            own.insert(
                0,
                StudioSection {
                    section_id: book_page_key(k, "chapter"),
                    name: format!("Chapter {}", k.saturating_add(1)),
                    page_start: book_page_key(k, &p.page_id),
                    continue_numbering: true,
                    page_number_start: None,
                    page_number_style: hsk_studio_folio::StudioNumberStyle::Arabic,
                    include_section_prefix: false,
                    section_prefix: None,
                    marker: None,
                },
            );
        }
        if let Some(s0) = own.first_mut() {
            if !book.automatic_pagination || k == 0 {
                if s0.continue_numbering {
                    s0.continue_numbering = false;
                    s0.page_number_start = Some(1);
                }
            } else if s0.continue_numbering {
                let next = last.checked_add(1).ok_or(BookError::BudgetExceeded)?;
                let wrong = match book.repagination_option {
                    StudioRepagination::Continue => false,
                    StudioRepagination::ContinueOnNextOdd => next & 1 == 0,
                    StudioRepagination::ContinueOnNextEven => next & 1 == 1,
                };
                if wrong {
                    if book.insert_blank_page {
                        let p = first_page.ok_or_else(|| BookError::ChapterMissing(d.document_id.clone()))?;
                        let key = book_page_key(k, "blank");
                        claim(&blank_spread_id(k))?;
                        doc.page_spreads.push(StudioPageSpread {
                            schema_id: PageSpreadSchema::V1,
                            page_spread_id: blank_spread_id(k),
                            name: format!("blank before chapter {}", k.saturating_add(1)),
                            parent: Parent::Root(()),
                            bounds: Bounds {
                                x: Length { value: 0.0, unit: GeometryUnit::Pt },
                                y: Length { value: 0.0, unit: GeometryUnit::Pt },
                                width: p.width.clone(),
                                height: p.height.clone(),
                            },
                            pages: vec![StudioPage { page_id: key.clone(), ..p.clone() }],
                        });
                        blank_pages.push(key);
                        page_index = page_index.checked_add(1).ok_or(BookError::BudgetExceeded)?;
                    } else {
                        s0.continue_numbering = false;
                        s0.page_number_start = Some(next.checked_add(1).ok_or(BookError::BudgetExceeded)?);
                    }
                }
            }
        }
        sections.extend(own);
        let start_index = page_index;
        for s in &d.page_spreads {
            claim(&s.page_spread_id)?;
            let mut s = s.clone();
            for p in &mut s.pages {
                p.page_id = book_page_key(k, &p.page_id);
                page_index = page_index.checked_add(1).ok_or(BookError::BudgetExceeded)?;
            }
            doc.page_spreads.push(s);
        }
        chapter_pages.push((d.document_id.clone(), start_index, page_index.saturating_sub(1)));
        for l in &d.layers {
            claim(&l.layer_id)?;
            let mut l = l.clone();
            if let Some(f) = l.text_frame.as_mut() {
                f.page_id = book_page_key(k, &f.page_id);
            }
            doc.layers.push(l);
        }
        for a in &d.artboards {
            claim(&a.artboard_id)?;
            doc.artboards.push(a.clone());
        }
        for q in &d.sequences {
            doc.sequences.push(q.clone());
        }
        doc.graph.operations.extend(d.graph.operations.iter().cloned());
        doc.graph.edges.extend(d.graph.edges.iter().cloned());
        doc.graph.outputs.extend(d.graph.outputs.iter().cloned());
    }
    let pages = number_pages(&doc, &sections).map_err(BookError::Numbering)?;
    Ok(CombinedBook { document: doc, sections, pages, chapter_pages, blank_pages })
}

/// STU-LAY-043/147 book export request: the chapter indices of `document_ids` (book order) for
/// `format` (only "pdf" is supported for a book).
pub fn book_export_subset(book: &StudioBook, document_ids: &[&str], format: &str) -> Result<Vec<usize>, BookError> {
    if format != "pdf" {
        return Err(BookError::ExportFormatUnsupported(format.to_owned()));
    }
    let mut out = Vec::with_capacity(document_ids.len());
    for d in document_ids {
        let i = book
            .chapters
            .iter()
            .position(|c| c.document_id == *d)
            .ok_or_else(|| BookError::DocumentNotInBook((*d).to_owned()))?;
        out.push(i);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// The page range text of each chapter ("first-last" page labels), for `document_page_range`.
pub fn chapter_page_ranges(combined: &CombinedBook) -> Vec<(String, String)> {
    combined
        .chapter_pages
        .iter()
        .map(|(d, a, b)| {
            let label = |i: usize| combined.pages.get(i).map_or(String::new(), |p| p.label.clone());
            (d.clone(), format!("{}-{}", label(*a), label(*b)))
        })
        .collect()
}
