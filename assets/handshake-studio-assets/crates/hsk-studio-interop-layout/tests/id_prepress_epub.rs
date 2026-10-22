//! Lane SMB-ID-PREPRESS slice 5: reflowable EPUB 3 export and the bounded re-read (STU-LAY-065,
//! STU-LAY-165). Expected strings are hand-written. epubcheck was not run: EPUB 3.3 conformance is
//! NOT_PROVEN (the structure checks of inspect_epub are a subset).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_interop_layout::*;
use hsk_studio_package::{ZipLimits, read_zip};

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

/// A story of `text` with paragraphs `(start, end, style)` and character ranges `(start, end, style)`.
fn story(n: u32, text: &str, paras: &[(u64, u64, Option<&str>)], chars: &[(u64, u64, Option<&str>)]) -> StudioTextStory {
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", n),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: paras.iter().map(|(s, e, st)| StoryParagraph { start: *s, end: *e, style_ref: st.map(str::to_owned), paragraph_direction: None }).collect(),
        character_ranges: chars.iter().map(|(s, e, st)| StoryRange { start: *s, end: *e, style_ref: st.map(str::to_owned), applied_language: None }).collect(),
    }
}

fn options() -> EpubOptions {
    EpubOptions {
        identifier: "urn:uuid:019abcde-0000-7000-8000-000000000001".into(),
        title: "Tom & Jerry".into(),
        language: "en-GB".into(),
        modified: "2026-10-16T00:00:00Z".into(),
        creator: Some("Studio".into()),
        accessibility: EpubAccessibility {
            access_mode: vec!["textual".into()],
            access_mode_sufficient: vec!["textual".into()],
            accessibility_feature: vec!["structuralNavigation".into(), "tableOfContents".into()],
            accessibility_hazard: vec!["none".into()],
            accessibility_summary: Some("Text only.".into()),
            conforms_to: Some("EPUB Accessibility 1.1 - WCAG 2.1 Level AA".into()),
            certified_by: Some("Self".into()),
            certifier_credential: None,
            certifier_report: None,
        },
        break_paragraph_style: Some("chapter".into()),
        ..EpubOptions::default()
    }
}

fn class_of(s: &str) -> Option<String> {
    match s {
        "chapter" => Some("Chapter Head".into()),
        "em" => Some("em".into()),
        _ => None,
    }
}
fn heading_of(s: &str) -> Option<u8> {
    match s {
        "chapter" => Some(1),
        "sub" => Some(2),
        _ => None,
    }
}

fn file<'a>(files: &'a [(String, Vec<u8>)], name: &str) -> &'a str {
    std::str::from_utf8(&files.iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("{name}")).1).unwrap()
}

#[test]
fn epub_package_structure_chapters_nav_and_metadata() {
    // "One\nA <b> & c\nTwo\nSub\nEnd": chapter heads at 0 and 14 split the book in two chapters.
    let text = "One\nA <b> & c\nTwo\nSub\nEnd";
    let st = story(
        1,
        text,
        &[(0, 4, Some("chapter")), (4, 14, None), (14, 18, Some("chapter")), (18, 22, Some("sub")), (22, 25, None)],
        &[(0, 6, None), (6, 9, Some("em")), (9, 25, None)],
    );
    let styles = EpubStyles { class_of: &class_of, heading_of: &heading_of };
    let out = export_epub(&[EpubStory { story: &st, text }], &styles, &options(), &CancellationToken::default()).unwrap();
    assert_eq!(out.chapters, vec!["chapter-0001.xhtml", "chapter-0002.xhtml"]);
    assert!(out.loss.is_empty());
    let files = read_zip(&out.bytes, &ZipLimits { max_bytes: 1 << 20, max_entries: 64, max_entry_bytes: 1 << 20, max_inflated_bytes: 1 << 22, max_ratio: 100, allow_zip64: false }, &CancellationToken::default()).unwrap();
    assert_eq!((files[0].0.as_str(), files[0].1.as_slice()), ("mimetype", b"application/epub+zip".as_slice()));
    let c1 = file(&files, "OEBPS/chapter-0001.xhtml");
    assert!(c1.contains("<h1 id=\"h1\" class=\"Chapter_Head\">One</h1>"), "{c1}");
    // Escaping and the character-style span (bytes 6..9 = "<b>").
    assert!(c1.contains("<p>A <span class=\"em\">&lt;b&gt;</span> &amp; c</p>"), "{c1}");
    let c2 = file(&files, "OEBPS/chapter-0002.xhtml");
    assert!(c2.contains("<h2 id=\"h2\">Sub</h2>") && c2.contains("<p>End</p>"), "{c2}");
    let nav = file(&files, "OEBPS/nav.xhtml");
    assert!(nav.contains("<li><a href=\"chapter-0001.xhtml\">One</a></li>"), "{nav}");
    assert!(nav.contains("<li><a href=\"chapter-0002.xhtml\">Two</a><ol><li><a href=\"chapter-0002.xhtml#h2\">Sub</a></li></ol></li>"), "{nav}");
    let opf = file(&files, "OEBPS/content.opf");
    for needle in [
        "<dc:title>Tom &amp; Jerry</dc:title>",
        "<meta property=\"dcterms:modified\">2026-10-16T00:00:00Z</meta>",
        "<meta property=\"schema:accessibilityFeature\">tableOfContents</meta>",
        "<meta property=\"schema:accessibilityHazard\">none</meta>",
        "<meta property=\"a11y:certifiedBy\" refines=\"#conf\">Self</meta>",
        "properties=\"nav\"",
        "<itemref idref=\"c1\"/>",
    ] {
        assert!(opf.contains(needle), "{needle}\n{opf}");
    }
    let i = inspect_epub(&out.bytes, &ZipLimits { max_bytes: 1 << 20, max_entries: 64, max_entry_bytes: 1 << 20, max_inflated_bytes: 1 << 22, max_ratio: 100, allow_zip64: false }, &CancellationToken::default()).unwrap();
    assert_eq!((i.version.as_str(), i.title.as_str(), i.language.as_str(), i.has_nav), ("3.0", "Tom & Jerry", "en-GB", true));
    assert_eq!(i.spine, vec!["OEBPS/chapter-0001.xhtml", "OEBPS/chapter-0002.xhtml"]);
    // Deterministic bytes.
    let again = export_epub(&[EpubStory { story: &st, text }], &styles, &options(), &CancellationToken::default()).unwrap();
    assert_eq!(again.bytes, out.bytes);
}

#[test]
fn epub_refusals_and_hostile_rereads() {
    let styles = EpubStyles { class_of: &class_of, heading_of: &heading_of };
    let tok = CancellationToken::default();
    let text = "a\u{1}b\u{2028}c";
    let st = story(1, text, &[(0, text.len() as u64, None)], &[]);
    let out = export_epub(&[EpubStory { story: &st, text }], &styles, &options(), &tok).unwrap();
    assert_eq!(out.loss, vec![EpubLoss::InvalidXmlChar(1)]);
    let lim = ZipLimits { max_bytes: 1 << 20, max_entries: 64, max_entry_bytes: 1 << 20, max_inflated_bytes: 1 << 22, max_ratio: 100, allow_zip64: false };
    let files = read_zip(&out.bytes, &lim, &tok).unwrap();
    assert!(file(&files, "OEBPS/chapter-0001.xhtml").contains("<p>ab<br/>c</p>"));
    // Metadata and story checks.
    let mut bad = options();
    bad.modified = "2026-10-16".into();
    assert_eq!(export_epub(&[], &styles, &bad, &tok).unwrap_err().code(), "epub_metadata_invalid");
    let mut bad = options();
    bad.language = "en GB".into();
    assert_eq!(export_epub(&[], &styles, &bad, &tok).unwrap_err().code(), "epub_metadata_invalid");
    let broken = story(2, "abc", &[(0, 9, None)], &[]);
    assert_eq!(export_epub(&[EpubStory { story: &broken, text: "abc" }], &styles, &options(), &tok).unwrap_err().code(), "epub_story_invalid");
    // Zip bomb: 8 MiB of one letter deflates to a few KiB; the re-read refuses it on the ratio
    // (and on the inflated total) before inflating.
    let big = "a".repeat(8 << 20);
    let bs = story(3, &big, &[(0, big.len() as u64, None)], &[]);
    let bomb = export_epub(&[EpubStory { story: &bs, text: &big }], &styles, &options(), &tok).unwrap();
    assert!(bomb.bytes.len() < 1 << 20, "{}", bomb.bytes.len());
    let roomy = ZipLimits { max_bytes: 1 << 24, max_entries: 64, max_entry_bytes: 1 << 26, max_inflated_bytes: 1 << 26, max_ratio: 100, allow_zip64: false };
    assert!(inspect_epub(&bomb.bytes, &roomy, &tok).is_err());
    let tight = ZipLimits { max_inflated_bytes: 1 << 20, max_ratio: u64::MAX, ..roomy.clone() };
    assert!(inspect_epub(&bomb.bytes, &tight, &tok).is_err());
    // Not a ZIP; a ZIP that is not an EPUB (an IDML package written by this crate's IDML path is
    // covered elsewhere; here: truncated bytes).
    assert!(inspect_epub(b"not a zip", &lim, &tok).is_err());
    assert!(inspect_epub(&out.bytes[..out.bytes.len() / 2], &lim, &tok).is_err());
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(export_epub(&[], &styles, &options(), &canceled).unwrap_err().code(), "canceled");
}
