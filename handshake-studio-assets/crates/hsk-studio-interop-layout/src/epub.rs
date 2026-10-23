//! Reflowable EPUB 3 export and a bounded EPUB re-read (lane SMB-ID-PREPRESS; STU-LAY-065,
//! STU-LAY-165 handoff payload). The layout side supplies stories, style names and the metadata of
//! the captured EPUB export preference; this module writes the package:
//!
//! - `mimetype` (`application/epub+zip`, first and stored), `META-INF/container.xml`,
//!   `OEBPS/content.opf` (package 3.0: `dc:identifier` bound by `unique-identifier`, `dc:title`,
//!   `dc:language`, `dcterms:modified`, optional publisher/creator/date/description/rights/subject,
//!   the nine accessibility metadata fields as separate `schema:` / `a11y:` properties, manifest,
//!   spine), `OEBPS/nav.xhtml` (`epub:type="toc"`, one entry per chapter and per heading),
//!   `OEBPS/styles.css`, one XHTML file per chapter;
//! - chapters split before every paragraph whose style is the `break_document` paragraph style
//!   (STU-LAY-165 `break_document` + `paragraph_style_name`), otherwise one chapter per story;
//! - paragraphs map to `<p class>` or `<h1>..<h6>` (heading levels from the caller's style map),
//!   character ranges with a style name to `<span class>`; U+2028 to `<br/>`; text is escaped
//!   (`& < > "`), characters XML 1.0 forbids are dropped and counted (`EpubLoss::InvalidXmlChar`);
//!   class names are sanitised to `[A-Za-z0-9_-]`.
//!
//! The ZIP is written by this crate's deterministic container writer (the one IDML uses; no new ZIP
//! code). [`inspect_epub`] re-reads a package through the bounded `hsk-studio-package` reader
//! (entry count, sizes, expansion ratio: zip bombs are refused before inflation) and checks the OCF
//! and OPF structure with DTDs refused. Conformance to EPUB 3.3 is NOT_PROVEN: epubcheck was not
//! run (it is a Java tool and was not downloaded in this lane); the structure checks here are a
//! subset. Fixed-layout EPUB, images, cover, fonts, footnotes and the remaining export-preference
//! properties are not written (typed gap list in the lane notes). InDesign parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::container;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::StudioTextStory;
use hsk_studio_package::{ZipDirectory, ZipLimits};
use roxmltree::{Document, ParsingOptions};
use std::collections::BTreeSet;

/// The EPUB `mimetype` value.
pub const EPUB_MIMETYPE: &str = "application/epub+zip";
/// Most chapters one export writes.
pub const MAX_EPUB_CHAPTERS: usize = 10_000;
/// Most bytes of story text one export takes.
pub const MAX_EPUB_TEXT_BYTES: usize = 256 << 20;

/// Typed refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EpubError {
    /// A required metadata field is empty or malformed (`what` names it).
    Metadata(&'static str),
    /// A story is stored content without resolved text, or its partitions do not fit its text.
    StoryInvalid(String),
    BudgetExceeded(&'static str),
    /// Re-read: the ZIP container was refused (bounded reader code).
    Zip(&'static str),
    /// Re-read: the OCF/OPF structure is wrong (`what` names the check).
    NotEpub(&'static str),
    Canceled,
}
impl EpubError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Metadata(_) => "epub_metadata_invalid",
            Self::StoryInvalid(_) => "epub_story_invalid",
            Self::BudgetExceeded(_) => "budget_exceeded",
            Self::Zip(c) => c,
            Self::NotEpub(_) => "not_epub",
            Self::Canceled => "canceled",
        }
    }
}

/// The nine accessibility metadata fields (STU-LAY-165: never one "accessible" flag).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpubAccessibility {
    pub access_mode: Vec<String>,
    pub access_mode_sufficient: Vec<String>,
    pub accessibility_feature: Vec<String>,
    pub accessibility_hazard: Vec<String>,
    pub accessibility_summary: Option<String>,
    pub conforms_to: Option<String>,
    pub certified_by: Option<String>,
    pub certifier_credential: Option<String>,
    pub certifier_report: Option<String>,
}

/// STU-LAY-165 metadata and structure options (reflowable EPUB 3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpubOptions {
    /// `id` (dc:identifier), e.g. `urn:uuid:...`.
    pub identifier: String,
    pub title: String,
    /// BCP 47 language tag.
    pub language: String,
    /// `dcterms:modified`, `CCYY-MM-DDThh:mm:ssZ` (caller-supplied: no clock is read).
    pub modified: String,
    pub creator: Option<String>,
    pub publisher: Option<String>,
    pub date: Option<String>,
    pub description: Option<String>,
    pub rights: Option<String>,
    pub subject: Option<String>,
    pub accessibility: EpubAccessibility,
    /// Paragraph style id that starts a new chapter (`break_document`).
    pub break_paragraph_style: Option<String>,
    /// Extra CSS appended to the generated style sheet.
    pub css: String,
}

/// One story with its text.
pub struct EpubStory<'a> {
    pub story: &'a StudioTextStory,
    /// The story text (inline content, or the resolved bytes of stored content).
    pub text: &'a str,
}

/// How style ids map to EPUB.
pub struct EpubStyles<'a> {
    /// Class name of a paragraph or character style id (`None` = no class).
    pub class_of: &'a dyn Fn(&str) -> Option<String>,
    /// Heading level 1..=6 of a paragraph style id (`None` = `<p>`).
    pub heading_of: &'a dyn Fn(&str) -> Option<u8>,
}

/// What the export changed or left out.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EpubLoss {
    /// Characters XML 1.0 forbids were dropped (count).
    InvalidXmlChar(u64),
}

/// The written package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpubOutput {
    pub bytes: Vec<u8>,
    /// Chapter file names in spine order.
    pub chapters: Vec<String>,
    pub loss: Vec<EpubLoss>,
}

fn xml_ok(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r') || (c >= ' ' && c != '\u{FFFE}' && c != '\u{FFFF}')
}

/// Escapes text for XML content and attributes; counts dropped characters.
fn esc(s: &str, dropped: &mut u64) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\u{2028}' => o.push_str("<br/>"),
            c if !xml_ok(c) => *dropped = dropped.saturating_add(1),
            c => o.push(c),
        }
    }
    o
}
/// Escapes plain metadata text (no `<br/>`).
fn esc_meta(s: &str, dropped: &mut u64) -> String {
    esc(&s.replace('\u{2028}', " "), dropped)
}

fn class(name: &str) -> String {
    let c: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if c.starts_with(|c: char| c.is_ascii_digit() || c == '-') { format!("s{c}") } else { c }
}

fn modified_ok(m: &str) -> bool {
    let b = m.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

struct Chapter {
    title: String,
    body: String,
    headings: Vec<(u8, String, String)>,
}

/// Writes a reflowable EPUB 3 package of `stories` in order (module docs).
pub fn export_epub(
    stories: &[EpubStory<'_>],
    styles: &EpubStyles<'_>,
    options: &EpubOptions,
    cancel: &CancellationToken,
) -> Result<EpubOutput, EpubError> {
    cancel.check().map_err(|_| EpubError::Canceled)?;
    for (v, what) in [(&options.identifier, "identifier"), (&options.title, "title"), (&options.language, "language")] {
        if v.trim().is_empty() || v.len() > 4096 {
            return Err(EpubError::Metadata(what));
        }
    }
    if !options.language.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(EpubError::Metadata("language"));
    }
    if !modified_ok(&options.modified) {
        return Err(EpubError::Metadata("modified"));
    }
    let total = stories.iter().try_fold(0usize, |n, s| n.checked_add(s.text.len())).ok_or(EpubError::BudgetExceeded("text"))?;
    if total > MAX_EPUB_TEXT_BYTES {
        return Err(EpubError::BudgetExceeded("text"));
    }
    let mut dropped = 0u64;
    let mut chapters: Vec<Chapter> = Vec::new();
    for s in stories {
        cancel.check().map_err(|_| EpubError::Canceled)?;
        let st = s.story;
        let bad = || EpubError::StoryInvalid(st.story_id.clone());
        if st.paragraphs.iter().any(|p| p.start > p.end || p.end > s.text.len() as u64)
            || st.character_ranges.iter().any(|r| r.start > r.end || r.end > s.text.len() as u64)
        {
            return Err(bad());
        }
        let mut current: Option<Chapter> = None;
        for p in &st.paragraphs {
            let (a, b) = (usize::try_from(p.start).map_err(|_| bad())?, usize::try_from(p.end).map_err(|_| bad())?);
            let raw = s.text.get(a..b).ok_or_else(bad)?;
            let raw = raw.strip_suffix('\n').unwrap_or(raw);
            let style = p.style_ref.as_deref();
            let breaks = style.is_some() && style == options.break_paragraph_style.as_deref();
            if current.is_none() || (breaks && current.as_ref().is_some_and(|c| !c.body.is_empty())) {
                if let Some(c) = current.take() {
                    chapters.push(c);
                }
                current = Some(Chapter { title: String::new(), body: String::new(), headings: Vec::new() });
            }
            let ch = current.as_mut().ok_or_else(bad)?;
            // Runs: character ranges clipped to the paragraph; styled ranges become spans.
            let mut inner = String::new();
            let mut at = a;
            let mut runs: Vec<(usize, usize, Option<String>)> = st
                .character_ranges
                .iter()
                .filter_map(|r| {
                    let (rs, re) = (usize::try_from(r.start).ok()?.max(a), usize::try_from(r.end).ok()?.min(a.saturating_add(raw.len())));
                    (rs < re).then(|| (rs, re, r.style_ref.as_deref().and_then(|x| (styles.class_of)(x))))
                })
                .collect();
            runs.sort_by_key(|r| r.0);
            let end = a.saturating_add(raw.len());
            for (rs, re, cls) in runs {
                if rs < at {
                    continue;
                }
                inner.push_str(&esc(s.text.get(at..rs).ok_or_else(bad)?, &mut dropped));
                let t = esc(s.text.get(rs..re).ok_or_else(bad)?, &mut dropped);
                match cls {
                    Some(c) => inner.push_str(&format!("<span class=\"{}\">{t}</span>", class(&c))),
                    None => inner.push_str(&t),
                }
                at = re;
            }
            inner.push_str(&esc(s.text.get(at.min(end)..end).ok_or_else(bad)?, &mut dropped));
            let cls = style.and_then(|x| (styles.class_of)(x)).map(|c| format!(" class=\"{}\"", class(&c))).unwrap_or_default();
            match style.and_then(|x| (styles.heading_of)(x)).filter(|l| (1..=6).contains(l)) {
                Some(level) => {
                    let id = format!("h{}", ch.headings.len().saturating_add(1));
                    let plain = esc_meta(raw, &mut dropped);
                    if ch.title.is_empty() {
                        ch.title = plain.clone();
                    }
                    ch.body.push_str(&format!("<h{level} id=\"{id}\"{cls}>{inner}</h{level}>\n"));
                    ch.headings.push((level, id, plain));
                }
                None => ch.body.push_str(&format!("<p{cls}>{inner}</p>\n")),
            }
        }
        if let Some(c) = current.take() {
            chapters.push(c);
        }
        if chapters.len() > MAX_EPUB_CHAPTERS {
            return Err(EpubError::BudgetExceeded("chapters"));
        }
    }
    if chapters.is_empty() {
        chapters.push(Chapter { title: String::new(), body: String::new(), headings: Vec::new() });
    }
    let title = esc_meta(&options.title, &mut dropped);
    let lang = esc_meta(&options.language, &mut dropped);
    let names: Vec<String> = (1..=chapters.len()).map(|i| format!("chapter-{i:04}.xhtml")).collect();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    files.push(("mimetype".into(), EPUB_MIMETYPE.as_bytes().to_vec()));
    files.push((
        "META-INF/container.xml".into(),
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n<rootfiles>\n<rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/>\n</rootfiles>\n</container>\n".to_vec(),
    ));
    // OPF.
    let mut meta = format!(
        "<dc:identifier id=\"pub-id\">{}</dc:identifier>\n<dc:title>{title}</dc:title>\n<dc:language>{lang}</dc:language>\n<meta property=\"dcterms:modified\">{}</meta>\n",
        esc_meta(&options.identifier, &mut dropped),
        options.modified
    );
    for (tag, v) in [
        ("dc:creator", &options.creator),
        ("dc:publisher", &options.publisher),
        ("dc:date", &options.date),
        ("dc:description", &options.description),
        ("dc:rights", &options.rights),
        ("dc:subject", &options.subject),
    ] {
        if let Some(v) = v {
            meta.push_str(&format!("<{tag}>{}</{tag}>\n", esc_meta(v, &mut dropped)));
        }
    }
    let a = &options.accessibility;
    for (prop, vs) in [
        ("schema:accessMode", &a.access_mode),
        ("schema:accessModeSufficient", &a.access_mode_sufficient),
        ("schema:accessibilityFeature", &a.accessibility_feature),
        ("schema:accessibilityHazard", &a.accessibility_hazard),
    ] {
        for v in vs {
            meta.push_str(&format!("<meta property=\"{prop}\">{}</meta>\n", esc_meta(v, &mut dropped)));
        }
    }
    if let Some(v) = &a.accessibility_summary {
        meta.push_str(&format!("<meta property=\"schema:accessibilitySummary\">{}</meta>\n", esc_meta(v, &mut dropped)));
    }
    if let Some(v) = &a.conforms_to {
        meta.push_str(&format!("<meta property=\"dcterms:conformsTo\" id=\"conf\">{}</meta>\n", esc_meta(v, &mut dropped)));
        for (prop, v) in [("a11y:certifiedBy", &a.certified_by), ("a11y:certifierCredential", &a.certifier_credential)] {
            if let Some(v) = v {
                meta.push_str(&format!("<meta property=\"{prop}\" refines=\"#conf\">{}</meta>\n", esc_meta(v, &mut dropped)));
            }
        }
        if let Some(v) = &a.certifier_report {
            meta.push_str(&format!("<link rel=\"a11y:certifierReport\" refines=\"#conf\" href=\"{}\"/>\n", esc_meta(v, &mut dropped)));
        }
    }
    let mut manifest = String::from(
        "<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n<item id=\"css\" href=\"styles.css\" media-type=\"text/css\"/>\n",
    );
    let mut spine = String::new();
    for (i, n) in names.iter().enumerate() {
        manifest.push_str(&format!("<item id=\"c{i}\" href=\"{n}\" media-type=\"application/xhtml+xml\"/>\n"));
        spine.push_str(&format!("<itemref idref=\"c{i}\"/>\n"));
    }
    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"pub-id\" xml:lang=\"{lang}\" prefix=\"a11y: http://www.idpf.org/epub/vocab/package/a11y/#\">\n<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n{meta}</metadata>\n<manifest>\n{manifest}</manifest>\n<spine>\n{spine}</spine>\n</package>\n"
    );
    files.push(("OEBPS/content.opf".into(), opf.into_bytes()));
    // Navigation.
    let mut nav = String::new();
    for (i, (c, n)) in chapters.iter().zip(&names).enumerate() {
        let label = if c.title.is_empty() { format!("{title} {}", i.saturating_add(1)) } else { c.title.clone() };
        nav.push_str(&format!("<li><a href=\"{n}\">{label}</a>"));
        let subs: Vec<&(u8, String, String)> = c.headings.iter().skip(1).collect();
        if !subs.is_empty() {
            nav.push_str("<ol>");
            for (_, id, text) in subs {
                nav.push_str(&format!("<li><a href=\"{n}#{id}\">{text}</a></li>"));
            }
            nav.push_str("</ol>");
        }
        nav.push_str("</li>\n");
    }
    let page = |head_title: &str, body: &str| {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"{lang}\" lang=\"{lang}\">\n<head>\n<meta charset=\"UTF-8\"/>\n<title>{head_title}</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"styles.css\"/>\n</head>\n<body>\n{body}</body>\n</html>\n"
        )
    };
    files.push(("OEBPS/nav.xhtml".into(), page(&title, &format!("<nav epub:type=\"toc\" id=\"toc\">\n<h1>{title}</h1>\n<ol>\n{nav}</ol>\n</nav>\n")).into_bytes()));
    files.push(("OEBPS/styles.css".into(), format!("body {{ margin: 0; }}\np {{ margin: 0 0 0.5em 0; }}\n{}\n", options.css.replace("</", "<\\/")).into_bytes()));
    for (c, n) in chapters.iter().zip(&names) {
        let t = if c.title.is_empty() { title.clone() } else { c.title.clone() };
        files.push((format!("OEBPS/{n}"), page(&t, &c.body).into_bytes()));
    }
    let parts: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let bytes = container::write(&parts, true).map_err(|e| EpubError::Zip(e.code()))?;
    let mut loss = Vec::new();
    if dropped > 0 {
        loss.push(EpubLoss::InvalidXmlChar(dropped));
    }
    Ok(EpubOutput { bytes, chapters: names, loss })
}

/// What [`inspect_epub`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpubInspection {
    pub version: String,
    pub identifier: String,
    pub title: String,
    pub language: String,
    /// Spine documents, in order (paths inside the package).
    pub spine: Vec<String>,
    pub has_nav: bool,
}

/// Bounded re-read of an EPUB package: the OCF container (mimetype first and stored with the exact
/// value, `META-INF/container.xml` naming a rootfile) and the OPF (package version, unique
/// identifier, title, language, `dcterms:modified`, manifest items present in the archive, spine
/// items in the manifest, one `nav` item). DTDs are refused in the XML; ZIP bounds come from
/// `limits` (zip bombs refused by size and ratio before inflation).
pub fn inspect_epub(bytes: &[u8], limits: &ZipLimits, cancel: &CancellationToken) -> Result<EpubInspection, EpubError> {
    let dir = ZipDirectory::parse(bytes, limits, cancel).map_err(|e| EpubError::Zip(e.code()))?;
    let first = dir.entries().first().ok_or(EpubError::NotEpub("empty"))?;
    if first.name != "mimetype" || first.local_offset() != 0 || first.method != 0 {
        return Err(EpubError::NotEpub("mimetype"));
    }
    let files = dir.read_all(bytes, cancel).map_err(|e| EpubError::Zip(e.code()))?;
    if files[0].1 != EPUB_MIMETYPE.as_bytes() {
        return Err(EpubError::NotEpub("mimetype"));
    }
    let get = |name: &str| files.iter().find(|(n, _)| n == name).map(|(_, b)| b.as_slice());
    let xml = |b: &[u8]| -> Result<String, EpubError> { String::from_utf8(b.to_vec()).map_err(|_| EpubError::NotEpub("utf8")) };
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: 1_000_000, ..ParsingOptions::default() };
    let container = xml(get("META-INF/container.xml").ok_or(EpubError::NotEpub("container"))?)?;
    let cdoc = Document::parse_with_options(&container, opts).map_err(|_| EpubError::NotEpub("container_xml"))?;
    let root_path = cdoc
        .descendants()
        .find(|n| n.has_tag_name("rootfile"))
        .and_then(|n| n.attribute("full-path"))
        .ok_or(EpubError::NotEpub("rootfile"))?
        .to_owned();
    let opf = xml(get(&root_path).ok_or(EpubError::NotEpub("rootfile_missing"))?)?;
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: 1_000_000, ..ParsingOptions::default() };
    let odoc = Document::parse_with_options(&opf, opts).map_err(|_| EpubError::NotEpub("opf_xml"))?;
    let pkg = odoc.root_element();
    if !pkg.has_tag_name("package") {
        return Err(EpubError::NotEpub("package"));
    }
    let version = pkg.attribute("version").ok_or(EpubError::NotEpub("version"))?.to_owned();
    let uid = pkg.attribute("unique-identifier").ok_or(EpubError::NotEpub("unique_identifier"))?;
    let text_of = |tag: &str| odoc.descendants().find(|n| n.tag_name().name() == tag).and_then(|n| n.text()).map(str::to_owned);
    let identifier = odoc
        .descendants()
        .find(|n| n.tag_name().name() == "identifier" && n.attribute("id") == Some(uid))
        .and_then(|n| n.text())
        .ok_or(EpubError::NotEpub("identifier"))?
        .to_owned();
    let title = text_of("title").ok_or(EpubError::NotEpub("title"))?;
    let language = text_of("language").ok_or(EpubError::NotEpub("language"))?;
    if !odoc.descendants().any(|n| n.has_tag_name("meta") && n.attribute("property") == Some("dcterms:modified")) {
        return Err(EpubError::NotEpub("modified"));
    }
    let base = root_path.rsplit_once('/').map_or(String::new(), |(d, _)| format!("{d}/"));
    let names: BTreeSet<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    let mut items = Vec::new();
    let mut has_nav = false;
    for it in odoc.descendants().filter(|n| n.tag_name().name() == "item") {
        let (id, href) = (it.attribute("id").ok_or(EpubError::NotEpub("item_id"))?, it.attribute("href").ok_or(EpubError::NotEpub("item_href"))?);
        let path = format!("{base}{href}");
        if !names.contains(path.as_str()) {
            return Err(EpubError::NotEpub("manifest_item_missing"));
        }
        if it.attribute("properties").is_some_and(|p| p.split_whitespace().any(|x| x == "nav")) {
            has_nav = true;
        }
        items.push((id.to_owned(), path));
    }
    let mut spine = Vec::new();
    for r in odoc.descendants().filter(|n| n.tag_name().name() == "itemref") {
        let idref = r.attribute("idref").ok_or(EpubError::NotEpub("itemref"))?;
        let (_, path) = items.iter().find(|(i, _)| i == idref).ok_or(EpubError::NotEpub("spine_item_missing"))?;
        spine.push(path.clone());
    }
    if spine.is_empty() {
        return Err(EpubError::NotEpub("spine_empty"));
    }
    if version.starts_with('3') && !has_nav {
        return Err(EpubError::NotEpub("nav"));
    }
    Ok(EpubInspection { version, identifier, title, language, spine, has_nav })
}
