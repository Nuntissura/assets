//! Folio-to-IDML export for documents whose pages, frames and stories were created or edited in
//! Folio (not only imported). The Folio document is validated first, then page spreads, pages,
//! threaded text-frame layers and `StudioTextStory` objects are written as a complete, self-contained
//! IDML package through the same canonical writer as [`crate::export_idml`]. Folio ids are not
//! written: IDML ids are minted deterministically in document order (`us1`, `up1`, `uf1`, `ua1`, ...).
//!
//! Folio holds style and language references by id only, so the caller supplies the IDML names
//! ([`ExportNames`]; `IdmlImport::export_names` returns them for an imported-then-edited document).
//! A reference without a name is written as the built-in "no style" and reported. Anything Folio
//! can hold that IDML export here does not write (artboards, non-text layers, appearance, sequences,
//! per-page bleed differences, ...) is a typed `Unsupported`/`Approximated` loss entry, never a
//! silent drop.
//!
//! UNVERIFIED against InDesign: the package shape follows the public IDML structure and the way
//! `Resources/Preferences.xml`, `META-INF/container.xml` and `DOMVersion` are laid out in the Craft
//! case study; no InDesign has opened a package written here.
use crate::idml_decode::{AS, FB, LossBook, VJ, canceled, classify};
use crate::idml_encode::{export_model, num};
use crate::model::*;
use crate::proposal::{Known, folio_err};
use crate::{IDML_MIMETYPE, IdmlError, Result};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AutoSizing, PageSide, Payload, StoryDirection, StoryParagraphDirection, StoryRange, StoryTextKind,
    StudioDocument, StudioTextStory, validate_document,
};
use std::collections::{BTreeMap, BTreeSet};

const DOM_VERSION: &str = "16.0";
const CONTAINER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n<rootfiles>\n<rootfile full-path=\"designmap.xml\" media-type=\"text/xml\"/>\n</rootfiles>\n</container>\n";
const NO_PARAGRAPH_STYLE: &str = "ParagraphStyle/$ID/[No paragraph style]";
const NO_CHARACTER_STYLE: &str = "CharacterStyle/$ID/[No character style]";
const PREFERENCES_PART: &str = "Resources/Preferences.xml";

/// A story and, for stored content, its resolved bytes.
#[derive(Clone, Copy, Debug)]
pub struct StoryInput<'a> {
    pub story: &'a StudioTextStory,
    /// Required when `story.content` is `Stored` (exactly `byte_len` bytes); `None` for inline.
    pub resolved_text: Option<&'a str>,
}

/// IDML names for the references Folio holds by id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportNames {
    /// `STYS` id -> `ParagraphStyle/...` name.
    pub paragraph_styles: BTreeMap<String, String>,
    /// `STYS` id -> `CharacterStyle/...` name.
    pub character_styles: BTreeMap<String, String>,
    /// Language key (as Folio holds it) -> `AppliedLanguage` value, for example `$ID/English: USA`.
    pub languages: BTreeMap<String, String>,
}

impl IdmlImport {
    /// The names this import mapped to Folio ids, so an edited Folio document exports back to the
    /// same style and language names.
    pub fn export_names(&self) -> ExportNames {
        let mut names = ExportNames::default();
        for s in &self.proposal.style_refs {
            let map = if s.paragraph {
                &mut names.paragraph_styles
            } else {
                &mut names.character_styles
            };
            map.insert(s.folio.clone(), s.idml_name.clone());
        }
        for st in &self.model.stories {
            for run in st.paragraphs.iter().flat_map(|p| &p.runs) {
                if let Some(raw) = &run.language
                    && let Some(key) = crate::proposal::language_key(raw)
                {
                    names
                        .languages
                        .entry(key)
                        .or_insert_with(|| raw.to_string());
                }
            }
        }
        names
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FolioExportInput<'a> {
    pub document: &'a StudioDocument,
    pub stories: &'a [StoryInput<'a>],
    pub names: &'a ExportNames,
}

#[derive(Clone, Debug)]
pub struct FolioExport {
    pub bytes: Vec<u8>,
    /// The typed model that was written (`part_sha256` is zero: there is no source part).
    pub model: IdmlModel,
    pub loss: Vec<LossEntry>,
}

pub(crate) fn opaque(element: &str, xml: String) -> Opaque {
    Opaque {
        element: element.to_owned(),
        xml,
    }
}

fn run_of(
    c: &StoryRange,
    names: &ExportNames,
    part: &str,
    loss: &mut LossBook,
    composition: &mut BTreeSet<&'static str>,
) -> RunModel {
    let char_style = match &c.style_ref {
        None => NO_CHARACTER_STYLE.to_owned(),
        Some(r) => names.character_styles.get(r).cloned().unwrap_or_else(|| {
            loss.note(
                part,
                "CharacterStyleRange",
                LossKind::Approximated,
                "style_name_unmapped",
            );
            NO_CHARACTER_STYLE.to_owned()
        }),
    };
    let language = c.applied_language.as_ref().and_then(|key| {
        let name = names.languages.get(key).cloned();
        if name.is_none() {
            loss.note(
                part,
                "CharacterStyleRange",
                LossKind::Approximated,
                "language_name_unmapped",
            );
        }
        name
    });
    let mut attrs = Attrs::new();
    if let Some(l) = &language {
        attrs.push(("AppliedLanguage".into(), l.clone()));
        classify("AppliedLanguage", l, composition);
    }
    RunModel {
        char_style: char_style.into(),
        attrs: attrs.into(),
        props: None,
        text: String::new(),
        br: false,
        special: false,
        language: language.map(Into::into),
        span: None,
        inline: None,
    }
}

fn story_model(
    input: &StoryInput,
    id: &str,
    names: &ExportNames,
    loss: &mut LossBook,
) -> Result<StoryModel> {
    let s = input.story;
    if s.text_kind != StoryTextKind::Area {
        return Err(IdmlError::Unsupported("story_text_kind_not_area"));
    }
    let text = match (&s.content, input.resolved_text) {
        (hsk_studio_folio::StoryContent::Inline { text_utf8 }, None) => text_utf8.as_str(),
        (hsk_studio_folio::StoryContent::Stored { byte_len, .. }, Some(t))
            if *byte_len == t.len() as u64 =>
        {
            t
        }
        _ => return Err(IdmlError::Unsupported("story_content_not_resolved")),
    };
    let part = format!("Stories/Story_{id}.xml");
    let direction = match s.story_direction {
        StoryDirection::RightToLeft => "RightToLeftDirection",
        StoryDirection::LeftToRight => "LeftToRightDirection",
        StoryDirection::Unknown => {
            loss.note(
                &part,
                "Story",
                LossKind::Approximated,
                "story_direction_unknown_defaulted_ltr",
            );
            "LeftToRightDirection"
        }
    };
    let mut composition = BTreeSet::new();
    classify("StoryDirection", direction, &mut composition);
    let at = |o: u64| {
        usize::try_from(o)
            .ok()
            .filter(|o| text.is_char_boundary(*o))
    };
    let mut paragraphs = Vec::with_capacity(s.paragraphs.len());
    for p in &s.paragraphs {
        let (start, end) = (
            at(p.start).ok_or(IdmlError::InvalidEdit("story_range_not_on_char_boundary"))?,
            at(p.end).ok_or(IdmlError::InvalidEdit("story_range_not_on_char_boundary"))?,
        );
        let br = text[start..end].ends_with('\n');
        let content_end = end - usize::from(br);
        if text[start..content_end].contains('\n') {
            loss.note(
                &part,
                "Content",
                LossKind::Approximated,
                "interior_newline_kept_in_paragraph",
            );
        }
        let style = match &p.style_ref {
            None => NO_PARAGRAPH_STYLE.to_owned(),
            Some(r) => names.paragraph_styles.get(r).cloned().unwrap_or_else(|| {
                loss.note(
                    &part,
                    "ParagraphStyleRange",
                    LossKind::Approximated,
                    "style_name_unmapped",
                );
                NO_PARAGRAPH_STYLE.to_owned()
            }),
        };
        let mut attrs = Attrs::new();
        if p.paragraph_direction == Some(StoryParagraphDirection::RightToLeft) {
            attrs.push(("ParagraphDirection".into(), "RightToLeftDirection".into()));
            classify(
                "ParagraphDirection",
                "RightToLeftDirection",
                &mut composition,
            );
        }
        let mut runs: Vec<RunModel> = Vec::new();
        for c in &s.character_ranges {
            let (from, to) = (
                (c.start as usize).max(start),
                (c.end as usize).min(content_end),
            );
            if from < to {
                let mut run = run_of(c, names, &part, loss, &mut composition);
                run.text = text[from..to].to_owned();
                runs.push(run);
            }
        }
        if br {
            match runs.last_mut() {
                Some(last) => last.br = true,
                None => {
                    // Empty paragraph: the break carries the style of the range that holds it.
                    let holder = s.character_ranges.iter().find(|c| {
                        (c.start as usize) <= content_end && content_end < c.end as usize
                    });
                    let mut run = holder.map_or_else(
                        || RunModel {
                            char_style: NO_CHARACTER_STYLE.into(),
                            attrs: Attrs::new().into(),
                            props: None,
                            text: String::new(),
                            br: false,
                            special: false,
                            language: None,
                            span: None,
                            inline: None,
                        },
                        |c| run_of(c, names, &part, loss, &mut composition),
                    );
                    run.br = true;
                    runs.push(run);
                }
            }
        }
        paragraphs.push(ParaModel {
            style: style.into(),
            attrs: attrs.into(),
            props: None,
            runs,
        });
    }
    Ok(StoryModel {
        id: id.to_owned(),
        part,
        part_sha256: [0; 32],
        attrs: Attrs::new(),
        head: vec![opaque(
            "StoryPreference",
            format!("<StoryPreference StoryDirection=\"{direction}\"/>"),
        )],
        right_to_left: s.story_direction == StoryDirection::RightToLeft,
        paragraphs,
        composition: composition.into_iter().collect(),
    })
}

fn find<T: Copy + PartialEq>(table: &[(&'static str, T)], value: T) -> &'static str {
    table
        .iter()
        .find(|(_, t)| *t == value)
        .map_or("", |(k, _)| k)
}

/// The STU-LAY-118 auto-size rows that state Studio's readings for an auto-sizing frame (no minimum
/// height or width; lines unwrapped for width only and height and width), written with the names and
/// values InDesign used in the oracle corpus ID-03 exports (lane SMB-ID-FRAME-AUTOSIZE-2). `Off`
/// writes none (unchanged output). The reference point is not written: no value naming Studio's fixed
/// top/left edge is attested, so the export reports `auto_sizing_reference_point_not_exported`.
fn auto_size_rows(kind: AutoSizing) -> String {
    if kind == AutoSizing::Off {
        return String::new();
    }
    let no_breaks = matches!(kind, AutoSizing::WidthOnly | AutoSizing::HeightAndWidth);
    format!(" UseMinimumHeightForAutoSizing=\"false\" UseMinimumWidthForAutoSizing=\"false\" UseNoLineBreaksForAutoSizing=\"{no_breaks}\"")
}

fn frame_children(path: &[PathPoint], p: &FramePrefs) -> Vec<Opaque> {
    let points: String = path
        .iter()
        .map(|q| {
            let a = format!("{} {}", num(q.anchor[0]), num(q.anchor[1]));
            format!("<PathPointType Anchor=\"{a}\" LeftDirection=\"{a}\" RightDirection=\"{a}\"/>")
        })
        .collect();
    let [t, l, b, r] = p.inset_pt;
    let inset = [t, l, b, r]
        .iter()
        .map(|v| format!("<ListItem type=\"unit\">{}</ListItem>", num(*v)))
        .collect::<String>();
    vec![
        opaque(
            "Properties",
            format!(
                "<Properties><PathGeometry><GeometryPathType PathOpen=\"false\"><PathPointArray>{points}</PathPointArray></GeometryPathType></PathGeometry></Properties>"
            ),
        ),
        opaque(
            "TextFramePreference",
            format!(
                "<TextFramePreference TextColumnCount=\"{}\" TextColumnGutter=\"{}\" VerticalJustification=\"{}\" FirstBaselineOffset=\"{}\" MinimumFirstBaselineOffset=\"{}\" AutoSizingType=\"{}\"{}><Properties><InsetSpacing type=\"list\">{inset}</InsetSpacing></Properties></TextFramePreference>",
                p.column_count,
                num(p.column_gutter_pt),
                find(&VJ, p.vertical_justification),
                find(&FB, p.first_baseline),
                num(p.min_first_baseline_pt),
                find(&AS, p.auto_size),
                auto_size_rows(p.auto_size),
            ),
        ),
    ]
}

pub(crate) fn preferences_xml(doc: &DocumentSettings, extra: &str) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    s.push_str(&format!(
        "<idPkg:Preferences xmlns:idPkg=\"{}\" DOMVersion=\"{DOM_VERSION}\">\n<DocumentPreference",
        crate::idml_decode::IDPKG_NS
    ));
    if let Some(w) = doc.page_width_pt {
        s.push_str(&format!(" PageWidth=\"{}\"", num(w)));
    }
    if let Some(h) = doc.page_height_pt {
        s.push_str(&format!(" PageHeight=\"{}\"", num(h)));
    }
    // InDesign creates this many pages before reading the spreads (Craft case study): keep 1.
    s.push_str(&format!(
        " PagesPerDocument=\"1\" FacingPages=\"{}\"",
        doc.facing_pages
    ));
    if let Some(b) = doc.bleed_pt {
        for (k, v) in ["Top", "Bottom", "InsideOrLeft", "OutsideOrRight"]
            .iter()
            .zip(b)
        {
            s.push_str(&format!(" DocumentBleed{k}Offset=\"{}\"", num(v)));
        }
    }
    s.push_str("/>\n");
    if let Some(m) = doc.margins_pt {
        s.push_str(&margin_xml(m));
        s.push('\n');
    }
    s.push_str(extra);
    s.push_str("</idPkg:Preferences>\n");
    s
}

pub(crate) fn margin_xml(m: [f64; 4]) -> String {
    format!(
        "<MarginPreference ColumnCount=\"1\" ColumnGutter=\"12\" Top=\"{}\" Bottom=\"{}\" Left=\"{}\" Right=\"{}\" ColumnDirection=\"Horizontal\"/>",
        num(m[0]),
        num(m[1]),
        num(m[2]),
        num(m[3])
    )
}

/// The written model before serialisation, with the id maps the long-document export needs.
pub(crate) struct Built {
    pub model: IdmlModel,
    pub loss: LossBook,
    /// Folio story id -> IDML story `Self`.
    pub story_ids: BTreeMap<String, String>,
    /// Folio page key -> (IDML page `Self`, spread index, page top-left in IDML spread coordinates).
    pub pages: BTreeMap<String, (String, usize, (f64, f64))>,
}

/// Exports a Folio layout document and its stories as a deterministic IDML package.
pub fn export_folio(
    input: &FolioExportInput,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<FolioExport> {
    let built = folio_model(input, &BTreeSet::new(), limits, cancel)?;
    finish(built, "", Vec::new(), cancel)
}

/// Serialises a built model: the fixed parts, `Resources/Preferences.xml` with `extra_prefs`
/// appended inside it, and `extra_parts`.
pub(crate) fn finish(built: Built, extra_prefs: &str, extra_parts: Vec<IdmlPart>, cancel: &CancellationToken) -> Result<FolioExport> {
    let Built { model, loss, .. } = built;
    let mut parts = vec![
        IdmlPart::new("mimetype".into(), IDML_MIMETYPE.as_bytes().to_vec()),
        IdmlPart::new("META-INF/container.xml".into(), CONTAINER.as_bytes().to_vec()),
        IdmlPart::new("designmap.xml".into(), Vec::new()),
        IdmlPart::new(PREFERENCES_PART.into(), preferences_xml(&model.document, extra_prefs).into_bytes()),
    ];
    parts.extend(extra_parts);
    let package = IdmlPackage { parts };
    let bytes = export_model(&model, &package, cancel)?;
    Ok(FolioExport { bytes, model, loss: loss.finish() })
}

/// Builds the IDML model of a Folio document. Layers in `handled` are written by the caller (the
/// long-document export) and are not reported as unexported.
pub(crate) fn folio_model(
    input: &FolioExportInput,
    handled: &BTreeSet<&str>,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Built> {
    canceled(cancel)?;
    let d = input.document;
    let ids: BTreeSet<String> = input
        .stories
        .iter()
        .map(|s| s.story.story_id.clone())
        .collect();
    validate_document(d.clone(), limits.folio, cancel, &Known(&ids)).map_err(folio_err)?;
    let mut loss = LossBook::default();

    // Stories.
    let mut story_ids: BTreeMap<&str, String> = BTreeMap::new();
    let mut stories = Vec::with_capacity(input.stories.len());
    for (n, s) in input.stories.iter().enumerate() {
        canceled(cancel)?;
        let id = format!("ua{:x}", n + 1);
        stories.push(story_model(s, &id, input.names, &mut loss)?);
        story_ids.insert(s.story.story_id.as_str(), id);
    }

    // Pages: spine at the origin for facing pages, spread centre otherwise.
    let facing = d
        .page_spreads
        .iter()
        .flat_map(|s| &s.pages)
        .any(|p| p.side != PageSide::SingleSided);
    let mut page_at: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    let mut page_map: BTreeMap<String, (String, usize, (f64, f64))> = BTreeMap::new();
    let mut spreads: Vec<SpreadModel> = Vec::with_capacity(d.page_spreads.len());
    let mut page_n = 0usize;
    let mut bleeds = Vec::new();
    for (si, sp) in d.page_spreads.iter().enumerate() {
        canceled(cancel)?;
        let part = format!("Spreads/Spread_us{:x}.xml", si + 1);
        let left: f64 = sp
            .pages
            .iter()
            .filter(|p| p.side == PageSide::LeftHand)
            .map(|p| p.width.value)
            .sum();
        let total: f64 = sp.pages.iter().map(|p| p.width.value).sum();
        let mut x = if facing { -left } else { -total / 2.0 };
        let mut m = SpreadModel {
            id: format!("us{:x}", si + 1),
            part: part.clone(),
            item_transform: IDENTITY,
            attrs: vec![("AppliedMaster".into(), "n".into())],
            pages: Vec::new(),
            frames: Vec::new(),
            items: Vec::new(),
        };
        for p in &sp.pages {
            page_n += 1;
            let (w, h) = (p.width.value, p.height.value);
            if facing && p.side == PageSide::SingleSided {
                loss.note(
                    &part,
                    "Page",
                    LossKind::Approximated,
                    "single_sided_page_in_facing_document",
                );
            }
            let margins = [
                p.margins.top.value,
                p.margins.bottom.value,
                p.margins.left.value,
                p.margins.right.value,
            ];
            bleeds.push(p.bleed.as_ref().map(|b| {
                [
                    b.top.value,
                    b.bottom.value,
                    b.inside_or_left.value,
                    b.outside_or_right.value,
                ]
            }));
            page_at.insert(p.page_id.as_str(), (x, -h / 2.0));
            page_map.insert(p.page_id.clone(), (format!("up{page_n:x}"), si, (x, -h / 2.0)));
            m.items.push(SpreadItem::Page(m.pages.len()));
            m.pages.push(PageModel {
                id: format!("up{page_n:x}"),
                name: page_n.to_string(),
                bounds_pt: [0.0, 0.0, h, w],
                item_transform: [1.0, 0.0, 0.0, 1.0, x, -h / 2.0],
                attrs: vec![("AppliedMaster".into(), "n".into())],
                children: vec![opaque("MarginPreference", margin_xml(margins))],
                margins_pt: Some(margins),
            });
            x += w;
        }
        spreads.push(m);
    }

    // Frames in layer order; everything else Folio can hold is reported.
    let mut frame_ids: BTreeMap<&str, String> = BTreeMap::new();
    for l in &d.layers {
        if l.text_frame.is_some() && matches!(l.payload, Payload::Text { .. }) {
            frame_ids.insert(l.layer_id.as_str(), format!("uf{:x}", frame_ids.len() + 1));
        }
    }
    for l in &d.layers {
        canceled(cancel)?;
        let (Some(f), Payload::Text { story_id }) = (&l.text_frame, &l.payload) else {
            if handled.contains(l.layer_id.as_str()) {
                continue;
            }
            loss.note(
                "document",
                &l.kind,
                LossKind::Unsupported,
                "folio_layer_not_exported",
            );
            continue;
        };
        if l.appearance.is_some() {
            loss.note(
                "document",
                &l.kind,
                LossKind::Unsupported,
                "layer_appearance_not_exported",
            );
        }
        let story = story_ids
            .get(story_id.as_str())
            .ok_or(IdmlError::DanglingStory)?;
        let loc = d
            .locate_page(&f.page_id)
            .ok_or(IdmlError::Folio(hsk_studio_folio::Code::DanglingReference))?;
        let (px, py) = page_at[f.page_id.as_str()];
        let (x0, y0) = (px + f.bounds.x.value, py + f.bounds.y.value);
        let (x1, y1) = (x0 + f.bounds.width.value, y0 + f.bounds.height.value);
        let path: Vec<PathPoint> = [[x0, y0], [x0, y1], [x1, y1], [x1, y0]]
            .into_iter()
            .map(|a| PathPoint {
                anchor: a,
                left: a,
                right: a,
            })
            .collect();
        let i = &f.inset_spacing;
        let prefs = FramePrefs {
            column_count: f.text_column_count,
            column_gutter_pt: f.text_column_gutter.value,
            inset_pt: [i.top.value, i.left.value, i.bottom.value, i.right.value],
            vertical_justification: f.vertical_justification,
            first_baseline: f.first_baseline_offset,
            min_first_baseline_pt: f.minimum_first_baseline_offset.value,
            auto_size: f.auto_sizing_type,
        };
        if prefs.auto_size != AutoSizing::Off {
            loss.note(
                "document",
                "TextFramePreference",
                LossKind::Approximated,
                "auto_sizing_reference_point_not_exported",
            );
        }
        let link = |l: &Option<String>| l.as_deref().and_then(|id| frame_ids.get(id)).cloned();
        let mut attrs = vec![("ContentType".to_owned(), "TextType".to_owned())];
        if !l.name.is_empty() {
            attrs.push(("Name".into(), l.name.clone()));
        }
        let sp = &mut spreads[loc.spread_index];
        sp.items.push(SpreadItem::Frame(sp.frames.len()));
        sp.frames.push(FrameModel {
            id: frame_ids[l.layer_id.as_str()].clone(),
            story_id: story.clone(),
            previous: link(&f.previous_text_frame),
            next: link(&f.next_text_frame),
            item_transform: IDENTITY,
            attrs,
            children: frame_children(&path, &prefs),
            path,
            path_open: false,
            prefs,
        });
    }
    if !d.artboards.is_empty() {
        loss.note(
            "document",
            "artboard",
            LossKind::Unsupported,
            "artboard_not_exported",
        );
    }
    if !d.sequences.is_empty() {
        loss.note(
            "document",
            "sequence",
            LossKind::Unsupported,
            "sequence_not_exported",
        );
    }

    // Document settings: first page defines the default size and margins; bleed is document-wide.
    let first = d.page_spreads.iter().flat_map(|s| &s.pages).next();
    let bleed_pt = bleeds.first().copied().flatten();
    if bleeds.iter().any(|b| *b != bleeds[0]) {
        loss.note(
            "document",
            "Page",
            LossKind::Approximated,
            "per_page_bleed_collapsed_to_document",
        );
    }
    let document = DocumentSettings {
        page_width_pt: first.map(|p| p.width.value),
        page_height_pt: first.map(|p| p.height.value),
        facing_pages: facing,
        bleed_pt,
        margins_pt: first.map(|p| {
            [
                p.margins.top.value,
                p.margins.bottom.value,
                p.margins.left.value,
                p.margins.right.value,
            ]
        }),
    };
    let mut designmap = vec![DesignmapItem::Opaque(opaque(
        "Preferences",
        format!("<idPkg:Preferences src=\"{PREFERENCES_PART}\"/>"),
    ))];
    designmap.extend((0..spreads.len()).map(DesignmapItem::Spread));
    designmap.extend((0..stories.len()).map(DesignmapItem::Story));
    let model = IdmlModel {
        dom_version: DOM_VERSION.to_owned(),
        document_attrs: vec![("Self".into(), "d".into())],
        designmap,
        document,
        spreads,
        stories,
    };
    let story_ids = story_ids.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    Ok(Built { model, loss, story_ids, pages: page_map })
}
