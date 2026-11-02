//! Lane SMB-SENTINELS-NATIVE-1, item 3: the STU-TYP-207 stream sentinels (Master Spec v02.229,
//! the v02.228 SD-29 assignment and its import / export rule) to and from IDML.
//!
//! Mappings are the ones the spec's rule and the SD-29 research record (research/
//! sd26-30-research-20261101.json) state: a page-number marker is processing instruction `ACE 18`
//! with `PageNumberType` on its CharacterStyleRange (absent = current, `NextPageNumber`,
//! `PreviousPageNumber`), the section marker is `ACE 19` (idml.paged.media text-variables page);
//! a break is a `Br` with `ParagraphBreakType` on its range, `NextFrame` / `NextPage` /
//! `NextOddPage` / `NextEvenPage` (the record's practitioner source, UNVERIFIED against
//! InDesign). No value is recorded for the column break, nor any IDML form for flush space or the
//! footnote, endnote, cross-reference and index markers: on export those are typed losses with
//! the spec's plain-text fallback (a break ends the paragraph, flush space becomes U+0020, a marker
//! with no resolved value writes nothing); an unknown attribute value on import is a typed loss,
//! never a guessed sentinel. A foreign U+FDD0..U+FDEF arriving in IDML text is replaced by U+FFFD
//! and reported (SD-37 reading, as hsk_studio_type::import_foreign_text). Synthetic snippets;
//! vendor parity UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_interop_layout::*;

const NS: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";
const CS: &str = "AppliedCharacterStyle=\"CharacterStyle/$ID/[No character style]\"";
const CUR: char = '\u{FDD0}';
const NEXT: char = '\u{FDD1}';
const PREV: char = '\u{FDD2}';
const SECTION: char = '\u{FDD3}';
const FOOTNOTE: char = '\u{FDD4}';
const ENDNOTE: char = '\u{FDD5}';
const XREF: char = '\u{FDD6}';
const INDEX: char = '\u{FDD7}';
const COLUMN: char = '\u{FDD8}';
const FRAME: char = '\u{FDD9}';
const PAGE: char = '\u{FDDA}';
const ODD: char = '\u{FDDB}';
const EVEN: char = '\u{FDDC}';
const FLUSH: char = '\u{FDDD}';

fn wrap(element: &str, body: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<idPkg:{element} xmlns:idPkg=\"{NS}\" DOMVersion=\"16.0\">\n{body}\n</idPkg:{element}>\n")
}
/// One CharacterStyleRange with extra attributes `attrs` and inner XML `body`.
fn csr(attrs: &str, body: &str) -> String {
    format!("<CharacterStyleRange {CS}{attrs}>{body}</CharacterStyleRange>")
}
fn psr(body: &str) -> String {
    format!("<ParagraphStyleRange AppliedParagraphStyle=\"ParagraphStyle/$ID/NormalParagraphStyle\">{body}</ParagraphStyleRange>")
}
/// A one-page, one-frame package whose story `ua1` holds `paragraphs` (ParagraphStyleRange XML).
fn package(paragraphs: &str) -> Vec<u8> {
    let designmap = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Document xmlns:idPkg=\"{NS}\" DOMVersion=\"16.0\" Self=\"d\">\n\
<idPkg:Preferences src=\"Resources/Preferences.xml\"/>\n<idPkg:Spread src=\"Spreads/Spread_us1.xml\"/>\n<idPkg:Story src=\"Stories/Story_ua1.xml\"/>\n</Document>\n"
    );
    let prefs = wrap(
        "Preferences",
        "<DocumentPreference PageWidth=\"300\" PageHeight=\"400\" FacingPages=\"false\"/>\n<MarginPreference Top=\"0\" Bottom=\"0\" Left=\"0\" Right=\"0\"/>",
    );
    let pts: String = [(0.0, 0.0), (0.0, 390.0), (290.0, 390.0), (290.0, 0.0)]
        .iter()
        .map(|(x, y)| format!("<PathPointType Anchor=\"{x} {y}\" LeftDirection=\"{x} {y}\" RightDirection=\"{x} {y}\"/>"))
        .collect();
    let spread = wrap(
        "Spread",
        &format!(
            "<Spread Self=\"us1\" PageCount=\"1\" ItemTransform=\"1 0 0 1 0 0\">\
<Page Self=\"up1\" Name=\"1\" GeometricBounds=\"0 0 400 300\" ItemTransform=\"1 0 0 1 0 0\"/>\
<TextFrame Self=\"uf1\" ParentStory=\"ua1\" PreviousTextFrame=\"n\" NextTextFrame=\"n\" ItemTransform=\"1 0 0 1 0 0\">\
<Properties><PathGeometry><GeometryPathType PathOpen=\"false\"><PathPointArray>{pts}</PathPointArray></GeometryPathType></PathGeometry></Properties>\
</TextFrame></Spread>"
        ),
    );
    let story = wrap(
        "Story",
        &format!("<Story Self=\"ua1\"><StoryPreference StoryDirection=\"LeftToRightDirection\"/>{paragraphs}</Story>"),
    );
    let parts = [
        ("mimetype", IDML_MIMETYPE.to_owned()),
        ("META-INF/container.xml", "<?xml version=\"1.0\" encoding=\"UTF-8\"?><container/>".to_owned()),
        ("designmap.xml", designmap),
        ("Resources/Preferences.xml", prefs),
        ("Spreads/Spread_us1.xml", spread),
        ("Stories/Story_ua1.xml", story),
    ];
    write_idml(&IdmlPackage { parts: parts.into_iter().map(|(n, b)| IdmlPart::new(n.into(), b.into_bytes())).collect() }).expect("write fixture")
}
fn import(bytes: &[u8]) -> IdmlImport {
    let mut mint = SequentialIdMint::new(3);
    import_idml(bytes, &Limits::default(), &mut mint, &CancellationToken::default()).expect("import")
}
fn story(i: &IdmlImport) -> &StudioTextStory {
    &i.proposal.stories[0].story
}
fn text(st: &StudioTextStory) -> &str {
    match &st.content {
        StoryContent::Inline { text_utf8 } => text_utf8,
        StoryContent::Stored { .. } => panic!("stored"),
    }
}
fn spans(st: &StudioTextStory) -> Vec<(u64, u64)> {
    st.paragraphs.iter().map(|p| (p.start, p.end)).collect()
}
fn loss_count(loss: &[LossEntry], code: &str) -> u32 {
    loss.iter().filter(|e| e.code == code).map(|e| e.count).sum()
}
/// The story part XML of an IDML package.
fn story_xml(bytes: &[u8]) -> String {
    let p = open_idml(bytes, &Limits::default(), &CancellationToken::default()).expect("open");
    let part = p.parts.iter().find(|p| p.name.starts_with("Stories/")).expect("story part");
    String::from_utf8(part.bytes.clone()).unwrap()
}
fn no_sentinel_block(s: &str) -> bool {
    !s.chars().any(|c| ('\u{FDD0}'..='\u{FDEF}').contains(&c))
}
/// Layout's (= Type's) paragraph partition of `text`.
fn partition(text: &str) -> Vec<(u64, u64)> {
    layout::paragraph_partition(text).iter().map(|s| (s.start, s.end)).collect()
}

#[test]
fn mt_31007_idml_page_number_and_section_markers_import_as_sentinels_and_round_trip() {
    let body = psr(&[
        csr("", "<Content>p</Content>"),
        csr("", "<Content><?ACE 18?></Content>"),
        csr(" PageNumberType=\"NextPageNumber\"", "<Content><?ACE 18?></Content>"),
        csr(" PageNumberType=\"PreviousPageNumber\"", "<Content>q <?ACE 18?></Content>"),
        csr("", "<Content>s <?ACE 19?></Content><Br/>"),
    ]
    .concat())
        + &psr(&csr("", "<Content>end</Content>"));
    let i = import(&package(&body));
    let want = format!("p{CUR}{NEXT}q {PREV}s {SECTION}\nend");
    assert_eq!(text(story(&i)), want);
    assert_eq!(spans(story(&i)), partition(&want));
    assert_eq!(loss_count(&i.loss, "special_character_dropped"), 0, "{:?}", i.loss);
    // Canonical export writes the instructions back (never a raw noncharacter) and re-imports equal.
    let out = export_idml(&i, &CancellationToken::default()).expect("export");
    let xml = story_xml(&out);
    assert_eq!(xml.matches("<?ACE 18?>").count(), 3, "{xml}");
    assert_eq!(xml.matches("<?ACE 19?>").count(), 1);
    assert_eq!(xml.matches("PageNumberType=\"NextPageNumber\"").count(), 1);
    assert_eq!(xml.matches("PageNumberType=\"PreviousPageNumber\"").count(), 1);
    assert!(no_sentinel_block(&xml));
    assert_eq!(text(story(&import(&out))), want);
}

#[test]
fn mt_31008_idml_br_with_paragraph_break_type_imports_as_break_sentinels_and_round_trips() {
    let br = |attrs: &str, t: &str| psr(&csr(attrs, &format!("<Content>{t}</Content><Br/>")));
    let body = [
        br(" ParagraphBreakType=\"NextFrame\"", "a"),
        br(" ParagraphBreakType=\"NextPage\"", "b"),
        br(" ParagraphBreakType=\"NextOddPage\"", "c"),
        br(" ParagraphBreakType=\"NextEvenPage\"", "d"),
        br("", "e"),
        // No value is recorded for the column break: an unknown value ends the paragraph as a
        // plain Br and is reported, never mapped.
        br(" ParagraphBreakType=\"NextColumn\"", "f"),
        // A Br alone in its range carries the range's break type.
        psr(&[csr("", "<Content>g</Content>"), csr(" ParagraphBreakType=\"NextFrame\"", "<Br/>")].concat()),
        psr(&csr("", "<Content>h</Content>")),
    ]
    .concat();
    let i = import(&package(&body));
    let want = format!("a{FRAME}b{PAGE}c{ODD}d{EVEN}e\nf\ng{FRAME}h");
    assert_eq!(text(story(&i)), want);
    assert_eq!(spans(story(&i)), partition(&want), "Folio paragraphs are Type's partition");
    assert_eq!(loss_count(&i.loss, "paragraph_break_type_unmapped"), 1, "{:?}", i.loss);
    let out = export_idml(&i, &CancellationToken::default()).expect("export");
    let xml = story_xml(&out);
    for v in ["NextFrame", "NextPage", "NextOddPage", "NextEvenPage"] {
        assert!(xml.contains(&format!("ParagraphBreakType=\"{v}\"")), "{v}");
    }
    assert!(no_sentinel_block(&xml));
    assert_eq!(text(story(&import(&out))), want);
}

#[test]
fn mt_31005_foreign_noncharacters_in_idml_text_are_replaced_and_reported() {
    // Escaped (character reference) and literal forms, the block ends, and Studio's own sentinels:
    // never read as a sentinel.
    let body = psr(&csr("", "<Content>x&#xFDD0;y\u{FDDA}z&#xFDEF;w\u{FDDD}</Content>"));
    let i = import(&package(&body));
    assert_eq!(text(story(&i)), "x\u{FFFD}y\u{FFFD}z\u{FFFD}w\u{FFFD}");
    assert_eq!(loss_count(&i.loss, "foreign_stream_sentinel_replaced"), 4);
    // Neighbours of the block are real text and pass unchanged.
    let i = import(&package(&psr(&csr("", "<Content>&#xFDCF;&#xFDF0;&#xFFFC;</Content>"))));
    assert_eq!(text(story(&i)), "\u{FDCF}\u{FDF0}\u{FFFC}");
    assert_eq!(loss_count(&i.loss, "foreign_stream_sentinel_replaced"), 0);
    // Hostile volume: the count stays exact.
    let many = "&#xFDD0;".repeat(20_000);
    let i = import(&package(&psr(&csr("", &format!("<Content>{many}</Content>")))));
    assert_eq!(text(story(&i)).chars().count(), 20_000);
    assert!(no_sentinel_block(text(story(&i))));
    assert_eq!(loss_count(&i.loss, "foreign_stream_sentinel_replaced"), 20_000);
}

#[test]
fn mt_31007_idml_marker_instructions_are_tokenised_not_substring_matched() {
    let one = |attrs: &str, content: &str| text(story(&import(&package(&psr(&csr(attrs, content)))))).to_owned();
    // Escape: the attribute value is unescaped before it is compared.
    assert_eq!(one(" PageNumberType=\"Next&#x50;ageNumber\"", "<Content><?ACE 18?></Content>"), NEXT.to_string());
    // Comment and decoy text: neither is an instruction.
    assert_eq!(one("", "<Content>a<!--<?ACE 18?>-->b</Content>"), "ab");
    assert_eq!(one("", "<Content>&lt;?ACE 18?&gt;</Content>"), "<?ACE 18?>");
    // Case and whitespace: the instruction value is trimmed; target and attribute names are
    // case-sensitive XML names (a lower-case attribute is not PageNumberType).
    assert_eq!(one("", "<Content><?ACE   18  ?></Content>"), CUR.to_string());
    assert_eq!(one(" pagenumbertype=\"NextPageNumber\"", "<Content><?ACE 18?></Content>"), CUR.to_string());
    let i = import(&package(&psr(&csr("", "<Content>a<?ace 18?>b</Content>"))));
    assert_eq!(text(story(&i)), "ab");
    assert_eq!(loss_count(&i.loss, "special_character_dropped"), 1);
    // An unknown PageNumberType value is a typed loss, never a guessed marker; other ACE codes stay
    // the existing special-character loss.
    let i = import(&package(&psr(&csr(" PageNumberType=\"AutoPageNumber\"", "<Content>a<?ACE 18?>b<?ACE 4?></Content>"))));
    assert_eq!(text(story(&i)), "ab");
    assert_eq!(loss_count(&i.loss, "page_number_type_unmapped"), 1);
    assert_eq!(loss_count(&i.loss, "special_character_dropped"), 1);
}

#[test]
fn mt_31008_folio_story_sentinels_export_to_idml_with_typed_losses() {
    // An imported document whose story is edited in Folio to hold every sentinel member.
    let i = import(&package(&psr(&csr("", "<Content>seed</Content>"))));
    let edited_text = [
        format!("A{CUR}{NEXT}{PREV}{SECTION}{FOOTNOTE}{ENDNOTE}{XREF}{INDEX}{FLUSH}Z{COLUMN}"),
        format!("b{FRAME}"),
        format!("c{PAGE}"),
        format!("d{ODD}"),
        format!("e{EVEN}"),
        "f\n".to_owned(),
        "g".to_owned(),
    ]
    .concat();
    let mut st = story(&i).clone();
    st.content = StoryContent::Inline { text_utf8: edited_text.clone() };
    st.paragraphs = layout::paragraph_partition(&edited_text)
        .iter()
        .map(|p| StoryParagraph { start: p.start, end: p.end, style_ref: None, paragraph_direction: None })
        .collect();
    st.character_ranges = vec![StoryRange { start: 0, end: edited_text.len() as u64, style_ref: None, applied_language: None }];
    let doc = i.proposal.snapshot.document().clone();
    let stories = [StoryInput { story: &st, resolved_text: None }];
    let names = i.export_names();
    let out = export_folio(&FolioExportInput { document: &doc, stories: &stories, names: &names }, &Limits::default(), &CancellationToken::default())
        .expect("export");
    let xml = story_xml(&out.bytes);
    assert!(no_sentinel_block(&xml), "{xml}");
    assert_eq!(xml.matches("<?ACE 18?>").count(), 3, "{xml}");
    assert_eq!(xml.matches("<?ACE 19?>").count(), 1);
    assert_eq!(xml.matches("PageNumberType=\"NextPageNumber\"").count(), 1);
    assert_eq!(xml.matches("PageNumberType=\"PreviousPageNumber\"").count(), 1);
    for v in ["NextFrame", "NextPage", "NextOddPage", "NextEvenPage"] {
        assert_eq!(xml.matches(&format!("ParagraphBreakType=\"{v}\"")).count(), 1, "{v}");
    }
    // Typed losses for the members IDML (as the spec and research record state it) cannot carry.
    assert_eq!(loss_count(&out.loss, "stream_marker_unavailable_in_idml"), 4, "{:?}", out.loss);
    assert_eq!(loss_count(&out.loss, "flush_space_unavailable_in_idml"), 1);
    assert_eq!(loss_count(&out.loss, "column_break_unavailable_in_idml"), 1);
    // Re-import: markers and the four typed breaks return; the losses are the spec's fallbacks.
    let back = import(&out.bytes);
    let want = format!("A{CUR}{NEXT}{PREV}{SECTION} Z\nb{FRAME}c{PAGE}d{ODD}e{EVEN}f\ng");
    assert_eq!(text(story(&back)), want);
    assert_eq!(spans(story(&back)), partition(&want));
}

#[test]
fn mt_31005_write_paths_refuse_a_sentinel_they_cannot_carry() {
    let i = import(&package(&psr(&csr("", "<Content>abc</Content>"))));
    let st = &i.model.stories[0];
    let run = &st.paragraphs[0].runs[0];
    // Byte-exact path: a plain Content swap cannot carry a sentinel.
    let edit = ContentEdit { part: &st.part, expected_part_sha256: st.part_sha256, span: run.span.clone().unwrap(), new_text: "a\u{FDD0}" };
    assert_eq!(
        replace_content(&i.package, &edit, &Limits::default(), &CancellationToken::default()).unwrap_err(),
        IdmlError::InvalidEdit("stream_sentinel_in_content")
    );
    // Canonical path: a marker whose range attribute disagrees, or a member with no IDML form, is
    // refused rather than written as a raw noncharacter.
    let mut model = i.model.clone();
    model.stories[0].paragraphs[0].runs[0].text = format!("a{NEXT}");
    assert_eq!(export_model(&model, &i.package, &CancellationToken::default()).unwrap_err(), IdmlError::InvalidEdit("page_number_type_mismatch"));
    model.stories[0].paragraphs[0].runs[0].text = format!("a{XREF}");
    assert_eq!(export_model(&model, &i.package, &CancellationToken::default()).unwrap_err(), IdmlError::InvalidEdit("stream_sentinel_unmapped"));
    model.stories[0].paragraphs[0].runs[0].text = format!("a{CUR}{SECTION}");
    assert!(export_model(&model, &i.package, &CancellationToken::default()).is_ok());
}

#[test]
fn mt_31005_foreign_noncharacters_in_idml_note_text_are_replaced_and_reported() {
    // Note and cell fragments are foreign text too (import_longdoc).
    let note = format!(
        "<Footnote Self=\"fn1\">{}</Footnote>",
        psr(&csr("", "<Content>a&#xFDD0;b\u{FDD9}c</Content>"))
    );
    let body = psr(&[csr("", "<Content>Host</Content>"), csr("", &note), csr("", "<Content> end</Content>")].concat());
    let i = import(&package(&body));
    let mut mint = SequentialIdMint::new(900);
    let l = import_longdoc(&i, &Limits::default(), &mut mint, &CancellationToken::default()).expect("longdoc import");
    let note_text: Vec<&str> = l.stories.iter().map(text).collect();
    assert_eq!(note_text, vec!["a\u{FFFD}b\u{FFFD}c"]);
    assert_eq!(loss_count(&l.loss, "foreign_stream_sentinel_replaced"), 2, "{:?}", l.loss);
}
