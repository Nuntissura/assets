//! SMB-PARSER-HOSTILE-1 item 1 (red) and item 2 (guard): element nesting in IDML parts and EPUB
//! XML reaches roxmltree 0.21.1, which parses nesting by recursion with no depth limit; the node
//! budgets here (`Limits::xml_nodes` 1,000,000 for IDML, 1,000,000 in `inspect_epub`) are far
//! above a depth that overflows a thread stack, and a stack overflow aborts the process. Own test
//! binary: an abort takes it down, which counts as the test failing. Every case runs on an
//! explicit thread stack, never the default.
//!
//! After item 2, `hsk_studio_package::xml_nesting_within` (cap `XML_MAX_NESTING` 256) runs before
//! every parse: IDML refuses with `IdmlError::BudgetExceeded("xml_depth")`, EPUB with the code its
//! malformed XML already had (`NotEpub("container_xml")`, `NotEpub("opf_xml")`).
//!
//! Note fragments: a `Footnote` is decoded from its story part, so a note nested past the cap is
//! refused by `decode_idml` before `import_longdoc` sees it; `import_longdoc` parses the kept
//! fragment again through the same guard. The fragment walk `idml_longdoc.rs` `walk` (recursive per
//! `ParagraphStyleRange` / `CharacterStyleRange`) is then bounded by the cap: the deepest note the
//! decode accepts walks to the end on a 2 MiB thread (`idml_note_at_the_cap_walks_on_2_mib`).
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_layout::*;
use hsk_studio_package::ZipLimits;

const NS: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";
const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
const CASE_STACK: usize = 2 << 20;

fn on_stack<T: Send + 'static>(stack: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(stack).spawn(f).expect("case thread").join().expect("case thread panicked")
}

fn part(name: &str, content: String) -> IdmlPart {
    IdmlPart::new(name.to_owned(), content.into_bytes())
}

/// A decodable one-story package (shape of `tests/decode_amplification.rs` `hostile_package`)
/// whose story element holds `story_body`.
fn package(story_body: &str) -> IdmlPackage {
    let frame = r#"<TextFrame Self="uf1" ParentStory="ua1" PreviousTextFrame="n" NextTextFrame="n" ItemTransform="1 0 0 1 0 0"><Properties><PathGeometry><GeometryPathType PathOpen="false"><PathPointArray><PathPointType Anchor="10 10" LeftDirection="10 10" RightDirection="10 10"/><PathPointType Anchor="10 300" LeftDirection="10 300" RightDirection="10 300"/><PathPointType Anchor="200 300" LeftDirection="200 300" RightDirection="200 300"/><PathPointType Anchor="200 10" LeftDirection="200 10" RightDirection="200 10"/></PathPointArray></GeometryPathType></PathGeometry></Properties></TextFrame>"#;
    IdmlPackage {
        parts: vec![
            part("mimetype", IDML_MIMETYPE.to_owned()),
            part(
                "designmap.xml",
                format!(
                    "{DECL}<Document xmlns:idPkg=\"{NS}\" DOMVersion=\"20.0\" Self=\"d\" StoryList=\"ua1\">\
                     <idPkg:Preferences src=\"Resources/Preferences.xml\"/>\
                     <idPkg:Spread src=\"Spreads/Spread_us1.xml\"/>\
                     <idPkg:Story src=\"Stories/Story_ua1.xml\"/></Document>"
                ),
            ),
            part(
                "Resources/Preferences.xml",
                format!(
                    "{DECL}<idPkg:Preferences xmlns:idPkg=\"{NS}\" DOMVersion=\"20.0\"><DocumentPreference PageHeight=\"400\" PageWidth=\"300\" FacingPages=\"false\"/><MarginPreference ColumnCount=\"1\" ColumnGutter=\"12\" Top=\"20\" Bottom=\"20\" Left=\"10\" Right=\"10\"/></idPkg:Preferences>"
                ),
            ),
            part(
                "Spreads/Spread_us1.xml",
                format!(
                    "{DECL}<idPkg:Spread xmlns:idPkg=\"{NS}\" DOMVersion=\"20.0\"><Spread Self=\"us1\" PageCount=\"1\" ItemTransform=\"1 0 0 1 0 0\">\
                     <Page Self=\"up1\" Name=\"1\" GeometricBounds=\"0 0 400 300\" ItemTransform=\"1 0 0 1 0 0\"/>{frame}</Spread></idPkg:Spread>"
                ),
            ),
            part(
                "Stories/Story_ua1.xml",
                format!(
                    "{DECL}<idPkg:Story xmlns:idPkg=\"{NS}\" DOMVersion=\"20.0\"><Story Self=\"ua1\">\
                     <StoryPreference StoryDirection=\"LeftToRightDirection\"/>{story_body}</Story></idPkg:Story>"
                ),
            ),
        ],
    }
}

const PSR: &str = "<ParagraphStyleRange AppliedParagraphStyle=\"ParagraphStyle/P\">";
const CSR: &str = "<CharacterStyleRange AppliedCharacterStyle=\"CharacterStyle/C\">";

/// `n` nested paragraph ranges around one character range with text, closed.
fn nested_ranges(n: usize) -> String {
    format!("{}{CSR}<Content>x</Content></CharacterStyleRange>{}", PSR.repeat(n), "</ParagraphStyleRange>".repeat(n))
}

/// A story whose one run holds a footnote of `n` nested paragraph ranges (story part depth:
/// idPkg:Story 1, Story 2, range 3, character range 4, Footnote 5, ranges 6..5+n, then the inner
/// character range and Content: `n + 7` levels).
fn footnote_story(n: usize) -> String {
    format!("{PSR}{CSR}<Content>Note</Content><Footnote Self=\"fn1\">{}</Footnote></CharacterStyleRange></ParagraphStyleRange>", nested_ranges(n))
}

fn decode(story_body: String) -> Result<IdmlImport> {
    on_stack(CASE_STACK, move || decode_idml(package(&story_body), &Limits::default(), &mut SequentialIdMint::new(1), &CancellationToken::default()))
}

#[test]
fn idml_story_is_a_typed_depth_refusal() {
    // 500,000 paragraph-range openers, never closed: 500,000 nodes, inside the 1,000,000 budget.
    let r = decode(PSR.repeat(500_000)).map(|_| ());
    assert_eq!(r, Err(IdmlError::BudgetExceeded("xml_depth")));
}

#[test]
fn idml_note_is_a_typed_depth_refusal() {
    // The note fragment nests 500,000 levels inside its story part: refused at decode.
    let r = decode(footnote_story(500_000)).map(|_| ());
    assert_eq!(r, Err(IdmlError::BudgetExceeded("xml_depth")));
}

/// Caller thread stacks of the within-cap rows (lead ruling 2026-11-08: the same answer from any
/// caller thread; 1 MiB is the Windows default main-thread reserve).
const CALLERS: [usize; 2] = [1 << 20, 2 << 20];

fn decode_from(stack: usize, story_body: String) -> Result<usize> {
    on_stack(stack, move || decode_idml(package(&story_body), &Limits::default(), &mut SequentialIdMint::new(1), &CancellationToken::default()).map(|i| i.loss.len()))
}

#[test]
fn idml_within_cap_decodes() {
    // `nested_ranges(n)` nests n + 4 levels (idPkg:Story, Story, the ranges, the character range,
    // Content): 200 and the cap 256 decode from either caller; 257 is refused before the parse.
    for stack in CALLERS {
        for n in [196usize, 252] {
            let r = decode_from(stack, nested_ranges(n));
            println!("WITHIN_CAP idml_story depth={} stack={stack} answer={r:?}", n + 4);
            assert!(r.is_ok(), "{} levels from a {stack}-byte caller: {r:?}", n + 4);
        }
        assert_eq!(decode_from(stack, nested_ranges(253)), Err(IdmlError::BudgetExceeded("xml_depth")));
    }
}

#[test]
fn idml_note_at_the_cap_walks_from_any_caller() {
    // 249 ranges in the note: the story part nests 256 levels, the cap itself; the longdoc fragment
    // walk (`idml_longdoc.rs` `walk`, recursive) runs on the caller's thread (lead ruling F2,
    // 2026-11-08). This is the deepest input the walk can receive: the fragment it recurses over is
    // the story part minus its 4 wrapper levels (252), and one more level puts the story past the cap,
    // refused before the walk (asserted below). Run from a 1 MiB and a 2 MiB caller.
    let deepest = 249;
    for stack in CALLERS {
        let out = on_stack(stack, move || -> Result<usize> {
            let cancel = CancellationToken::default();
            let limits = Limits::default();
            let mut mint = SequentialIdMint::new(1);
            let i = decode_idml(package(&footnote_story(deepest)), &limits, &mut mint, &cancel)?;
            import_longdoc(&i, &limits, &mut mint, &cancel).map(|l| l.records.footnotes.len())
        });
        println!("WITHIN_CAP idml_note depth={} stack={stack} answer={out:?}", deepest + 7);
        assert!(out.is_ok(), "{out:?}");
        // One more level is past the cap: refused at decode, before the walk.
        assert_eq!(decode_from(stack, footnote_story(deepest + 1)).map(|_| ()), Err(IdmlError::BudgetExceeded("xml_depth")));
    }
}

// ----- EPUB ----------------------------------------------------------------------------------------

/// A STORE-only ZIP of `(name, bytes)` in order (local headers, central directory, end record).
fn stored_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in entries {
        let crc = hsk_studio_package::crc32_of(data);
        let (n, len) = (u16::try_from(name.len()).expect("name"), u32::try_from(data.len()).expect("size"));
        let offset = u32::try_from(out.len()).expect("offset");
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        for v in [20u16, 0, 0, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, len, len] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        for v in [20u16, 20, 0, 0, 0, 0] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, len, len] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [n, 0, 0, 0, 0] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let (cd_off, cd_len) = (u32::try_from(out.len()).expect("cd"), u32::try_from(central.len()).expect("cd"));
    let count = u16::try_from(entries.len()).expect("count");
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    for v in [0u16, 0, count, count] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// `depth` nested `<x>` padding elements, closed.
fn padding(depth: usize) -> String {
    format!("{}{}", "<x>".repeat(depth), "</x>".repeat(depth))
}

fn container(pad: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">{pad}<rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>"
    )
}

fn opf(pad: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"uid\">{pad}\
         <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:identifier id=\"uid\">urn:x</dc:identifier><dc:title>T</dc:title><dc:language>en</dc:language><meta property=\"dcterms:modified\">2026-01-01T00:00:00Z</meta></metadata>\
         <manifest><item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/><item id=\"c1\" href=\"c1.xhtml\" media-type=\"application/xhtml+xml\"/></manifest>\
         <spine><itemref idref=\"c1\"/></spine></package>"
    )
}

fn epub(container_xml: String, opf_xml: String) -> Vec<u8> {
    stored_zip(&[
        ("mimetype", b"application/epub+zip".to_vec()),
        ("META-INF/container.xml", container_xml.into_bytes()),
        ("OEBPS/content.opf", opf_xml.into_bytes()),
        ("OEBPS/nav.xhtml", b"<html/>".to_vec()),
        ("OEBPS/c1.xhtml", b"<html/>".to_vec()),
    ])
}

fn inspect(bytes: Vec<u8>) -> std::result::Result<EpubInspection, EpubError> {
    let limits = ZipLimits { max_bytes: 64 << 20, max_entries: 64, max_entry_bytes: 32 << 20, max_inflated_bytes: 64 << 20, max_ratio: 100, allow_zip64: false };
    on_stack(CASE_STACK, move || inspect_epub(&bytes, &limits, &CancellationToken::default()))
}

#[test]
fn epub_container_is_a_typed_refusal() {
    let deep = format!("{}{}", "<x>".repeat(500_000), "</x>".repeat(500_000));
    assert_eq!(inspect(epub(container(&deep), opf(""))).map(|_| ()), Err(EpubError::NotEpub("container_xml")));
}

#[test]
fn epub_opf_is_a_typed_refusal() {
    let deep = "<x>".repeat(500_000);
    assert_eq!(inspect(epub(container(""), opf(&deep))).map(|_| ()), Err(EpubError::NotEpub("opf_xml")));
}

#[test]
fn epub_within_cap_reads() {
    // container / package root + `padding(n)`: n + 1 levels; 200 and the cap 256 read from either
    // caller, 257 is refused.
    let inspect_from = |stack: usize, bytes: Vec<u8>| {
        let limits = ZipLimits { max_bytes: 64 << 20, max_entries: 64, max_entry_bytes: 32 << 20, max_inflated_bytes: 64 << 20, max_ratio: 100, allow_zip64: false };
        on_stack(stack, move || inspect_epub(&bytes, &limits, &CancellationToken::default()))
    };
    for stack in CALLERS {
        for n in [199usize, 255] {
            for (c, o) in [(padding(n), String::new()), (String::new(), padding(n))] {
                let r = inspect_from(stack, epub(container(&c), opf(&o)));
                let i = r.unwrap_or_else(|e| panic!("{} levels from a {stack}-byte caller: {e:?}", n + 1));
                assert_eq!((i.version.as_str(), i.identifier.as_str(), i.spine.as_slice(), i.has_nav), ("3.0", "urn:x", &["OEBPS/c1.xhtml".to_owned()][..], true));
            }
        }
        assert_eq!(inspect_from(stack, epub(container(&padding(256)), opf(""))).map(|_| ()), Err(EpubError::NotEpub("container_xml")));
        assert_eq!(inspect_from(stack, epub(container(""), opf(&padding(256)))).map(|_| ()), Err(EpubError::NotEpub("opf_xml")));
    }
}
