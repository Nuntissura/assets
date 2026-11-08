//! SMB-PARSER-HOSTILE-1 item 6, IDML target: a deterministic mutation sweep
//! (`hsk-studio-interop-tiff/tests/support/hostile_mutator.rs`) over the long-document fixture of
//! `tests/idml_longdoc.rs` `fixture()` (copied below, shape unchanged), with `Limits::default()`.
//! Damage goes into one part's bytes inside the `IdmlPackage`, so ZIP CRCs do not stop it; each
//! mutant runs `decode_idml` then `import_longdoc`. A smaller sweep damages the whole container
//! bytes through `import_idml`.
#[path = "../../hsk-studio-interop-tiff/tests/support/hostile_mutator.rs"]
mod hostile_mutator;

use hostile_mutator::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_layout::*;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const CAP: usize = 256 << 20;

// ----- fixture (copied from tests/idml_longdoc.rs, returns the package) ------------------------------

const NS: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";
const CSR: &str = "<CharacterStyleRange AppliedCharacterStyle=\"CharacterStyle/$ID/[No character style]\">";

fn wrap(element: &str, body: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<idPkg:{element} xmlns:idPkg=\"{NS}\" DOMVersion=\"16.0\">\n{body}\n</idPkg:{element}>\n")
}
fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    let pts: String = [(x0, y0), (x0, y1), (x1, y1), (x1, y0)]
        .iter()
        .map(|(x, y)| format!("<PathPointType Anchor=\"{x} {y}\" LeftDirection=\"{x} {y}\" RightDirection=\"{x} {y}\"/>"))
        .collect();
    format!("<Properties><PathGeometry><GeometryPathType PathOpen=\"false\"><PathPointArray>{pts}</PathPointArray></GeometryPathType></PathGeometry></Properties>")
}
fn run(inner: &str) -> String {
    format!("{CSR}{inner}</CharacterStyleRange>")
}
fn para(style: &str, inner: &str) -> String {
    format!("<ParagraphStyleRange AppliedParagraphStyle=\"ParagraphStyle/{style}\">{inner}</ParagraphStyleRange>")
}

/// A one-page IDML package (300 x 400 pt) with every long-document feature plus hostile and
/// unmappable members. Host story text (proposal rule) is "Hi\nNote end": table at byte 2,
/// footnote at 7, endnote at 11.
fn fixture_package() -> IdmlPackage {
    let designmap = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Document xmlns:idPkg=\"{NS}\" DOMVersion=\"16.0\" Self=\"d\">\n\
<idPkg:Graphic src=\"Resources/Graphic.xml\"/>\n<idPkg:Styles src=\"Resources/Styles.xml\"/>\n<idPkg:Preferences src=\"Resources/Preferences.xml\"/>\n\
<Section Self=\"sec1\" Name=\"Front\" PageStart=\"up1\" ContinueNumbering=\"false\" PageNumberStart=\"5\" IncludeSectionPrefix=\"true\" SectionPrefix=\"A\" Marker=\"Intro\"><Properties><PageNumberStyle type=\"enumeration\">LowerRoman</PageNumberStyle></Properties></Section>\n\
<TextVariable Self=\"dTV1\" Name=\"Chapter\" VariableType=\"ChapterNumberType\"><ChapterNumberVariablePreference TextBefore=\"Ch \" TextAfter=\"\" Format=\"UpperRoman\"/></TextVariable>\n\
<TextVariable Self=\"dTV2\" Name=\"Brand\" VariableType=\"CustomTextType\"><CustomTextVariablePreference Contents=\"Studio\"/></TextVariable>\n\
<TextVariable Self=\"dTV3\" Name=\"Fancy\" VariableType=\"FancyType\"/>\n\
<idPkg:Spread src=\"Spreads/Spread_us1.xml\"/>\n<idPkg:Story src=\"Stories/Story_ua1.xml\"/>\n<idPkg:Story src=\"Stories/Story_ua2.xml\"/>\n</Document>\n"
    );
    let prefs = wrap(
        "Preferences",
        "<DocumentPreference PageWidth=\"300\" PageHeight=\"400\" FacingPages=\"false\"/>\n<MarginPreference Top=\"0\" Bottom=\"0\" Left=\"0\" Right=\"0\"/>\n\
<ChapterNumberPreference ChapterNumber=\"3\"/>\n\
<FootnoteOption FootnoteNumberingStyle=\"LowerLetters\" StartAt=\"2\" RestartNumbering=\"PageRestart\" Prefix=\"[\" Suffix=\"]\" SpaceBetween=\"4\" Spacer=\"6\" RuleOn=\"true\" RuleLineWeight=\"0.5\" RuleColor=\"Color/Black\" RuleTint=\"100\" RuleLeftIndent=\"0\" RuleWidth=\"72\" RuleOffset=\"2\" NoSplitting=\"true\"/>\n\
<EndnoteOption EndnoteTitle=\"Notes\" EndnoteTitleStyle=\"ParagraphStyle/Notes Title\" EndnoteNumberingStyle=\"Arabic\" StartEndnoteNumberAt=\"1\" RestartEndnoteNumbering=\"EndnoteContinuous\" ScopeValue=\"EndnoteScopeDocument\"/>",
    );
    let styles = wrap(
        "Styles",
        "<RootTOCStyleGroup Self=\"u9\"><TOCStyle Self=\"TOCStyle/$ID/DefaultTOCStyleName\" Name=\"[Default]\" Title=\"Contents\"/>\
<TOCStyle Self=\"TOCStyle/Contents\" Name=\"Contents\" Title=\"Contents\" TitleStyle=\"ParagraphStyle/TOC Title\"><TOCStyleEntry Name=\"Heading 1\" FormatStyle=\"ParagraphStyle/TOC 1\" Level=\"1\" PageNumberPosition=\"AfterEntry\" Separator=\"^t\"/></TOCStyle></RootTOCStyleGroup>",
    );
    let graphic = wrap(
        "Graphic",
        "<Color Self=\"Color/Red\" Model=\"Process\" Space=\"RGB\" ColorValue=\"255 0 0\" Name=\"Red\"/>\n<Color Self=\"Color/Black\" Model=\"Process\" Space=\"CMYK\" ColorValue=\"0 0 0 100\" Name=\"Black\"/>",
    );
    let spread = wrap(
        "Spread",
        &format!(
            "<Spread Self=\"us1\" PageCount=\"1\" ItemTransform=\"1 0 0 1 0 0\">\
<Page Self=\"up1\" Name=\"1\" GeometricBounds=\"0 0 400 300\" ItemTransform=\"1 0 0 1 0 0\"/>\
<TextFrame Self=\"uf1\" ParentStory=\"ua1\" PreviousTextFrame=\"n\" NextTextFrame=\"n\" ItemTransform=\"1 0 0 1 0 0\">{}</TextFrame>\
<Rectangle Self=\"ur1\" Name=\"$ID/\" ItemTransform=\"1 0 0 1 0 0\">{}<FrameFittingOption FittingOnEmptyFrame=\"Proportionally\" FittingAlignment=\"CenterAnchor\"/>\
<Image Self=\"ui1\" ItemTransform=\"0.5 0 0 0.5 70 150\"><Properties><GraphicBounds Left=\"0\" Top=\"0\" Right=\"200\" Bottom=\"200\"/></Properties></Image></Rectangle>\
<Oval Self=\"uo1\" ItemTransform=\"1 0 0 1 0 0\">{}<EPS Self=\"ue1\"/></Oval></Spread>",
            rect_path(20.0, 20.0, 220.0, 120.0),
            rect_path(20.0, 150.0, 220.0, 250.0),
            rect_path(20.0, 300.0, 120.0, 380.0),
        ),
    );
    let cell = |name: &str, extra: &str, body: &str| format!("<Cell Name=\"{name}\"{extra}>{body}</Cell>");
    let table = format!(
        "<Table Self=\"ut1\" HeaderRowCount=\"1\" BodyRowCount=\"1\" FooterRowCount=\"1\" ColumnCount=\"2\">\
<Row Self=\"ut1r0\" Name=\"0\"/><Row Self=\"ut1r1\" Name=\"1\" AutoGrow=\"false\" SingleRowHeight=\"20\"/><Row Self=\"ut1r2\" Name=\"2\" AutoGrow=\"true\" MinimumHeight=\"10\" MaximumHeight=\"40\"/>\
<Column Name=\"0\" SingleColumnWidth=\"100\"/><Column Name=\"1\" SingleColumnWidth=\"50\"/>{}{}{}{}{}{}</Table>",
        cell(
            "0:0",
            " ColumnSpan=\"2\" FillColor=\"Color/Red\" FillTint=\"50\" TopEdgeStrokeWeight=\"1\" TopEdgeStrokeColor=\"Color/Black\" VerticalJustification=\"CenterAlign\" TopInset=\"2\"",
            &para("Cell", &run("<Content>Head</Content>"))
        ),
        cell("1:0", "", ""),
        cell("0:1", "", &para("Cell", &run("<Content>A</Content><Br/><Content>B</Content>"))),
        cell("1:1", "", ""),
        cell("0:2", " ColumnSpan=\"2\"", &para("Cell", &run("<Content>F</Content><Footnote><ParagraphStyleRange/></Footnote>"))),
        cell("1:2", "", ""),
    );
    let big = format!("<Table Self=\"big\" ColumnCount=\"1\">{}<Column Name=\"0\"/></Table>", "<Row/>".repeat(10_001));
    let overflow = "<Table Self=\"ovf\" ColumnCount=\"1\"><Row Name=\"0\"/><Column Name=\"0\"/><Cell Name=\"0:0\" RowSpan=\"4294967295\"/></Table>";
    let footnote = format!("<Footnote Self=\"fn1\">{}</Footnote>", para("Footnote", &run("<Content><?ACE 4?>\tSee page 2</Content>")));
    let host = wrap(
        "Story",
        &format!(
            "<Story Self=\"ua1\"><StoryPreference StoryDirection=\"LeftToRightDirection\"/>{}{}</Story>",
            para("Body", &[run("<Content>Hi</Content>"), run(&table), run("<Br/>")].concat()),
            para(
                "Body",
                &[
                    run("<Content>Note</Content>"),
                    run(&footnote),
                    run("<Content> end</Content>"),
                    run("<Endnote Self=\"en1\"/>"),
                    run("<TextVariableInstance Self=\"tvi1\" ResultText=\"Studio\" AssociatedTextVariable=\"dTV2\"/>"),
                    run(&big),
                    run(overflow),
                ]
                .concat()
            ),
        ),
    );
    let range = |id: &str, src: &str, text: &str| {
        format!("<EndnoteRange Self=\"{id}\" SourceEndnote=\"{src}\">{}</EndnoteRange>", para("Endnote", &run(&format!("<Content><?ACE 4?>\t{text}</Content>"))))
    };
    let endnotes = wrap(
        "Story",
        &format!(
            "<Story Self=\"ua2\" IsEndnoteStory=\"true\"><StoryPreference StoryDirection=\"LeftToRightDirection\"/>{}</Story>",
            para("Endnote", &[run(&range("er1", "en1", "Later.")), run("<Br/>"), run(&range("er9", "en9", "Orphan."))].concat())
        ),
    );
    let parts = [
        ("mimetype", IDML_MIMETYPE.to_owned()),
        ("META-INF/container.xml", "<?xml version=\"1.0\" encoding=\"UTF-8\"?><container/>".to_owned()),
        ("designmap.xml", designmap),
        ("Resources/Preferences.xml", prefs),
        ("Resources/Styles.xml", styles),
        ("Resources/Graphic.xml", graphic),
        ("Spreads/Spread_us1.xml", spread),
        ("Stories/Story_ua1.xml", host),
        ("Stories/Story_ua2.xml", endnotes),
    ];
    IdmlPackage { parts: parts.into_iter().map(|(n, b)| IdmlPart::new(n.into(), b.into_bytes())).collect() }
}

// ----- sweeps ---------------------------------------------------------------------------------------

const PROFILE: Profile<'static> = Profile {
    dictionary: &[
        b"<ParagraphStyleRange>", b"</ParagraphStyleRange>", b"<CharacterStyleRange>", b"<Content>", b"<Br/>", b"<Footnote>", b"</Footnote>",
        b"<Table ColumnCount=\"4294967295\">", b"<Cell Name=\"0:0\" RowSpan=\"99\"/>", b"Self=\"", b"ParentStory=\"ua1\"",
        b"NextTextFrame=\"uf1\"", b"src=\"", b"ItemTransform=\"", b"<?ACE 18?>", b"&#xFDD0;", b"<!DOCTYPE x>", b"&nope;", b"\xC3",
    ],
    text: true,
};

fn decode_all(package: IdmlPackage) -> Answer {
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let mut mint = SequentialIdMint::new(7);
    let i = decode_idml(package, &limits, &mut mint, &cancel).map_err(|e| e.code())?;
    import_longdoc(&i, &limits, &mut mint, &cancel).map(|_| ()).map_err(|e| e.code())
}

#[test]
fn idml_mutation_sweep_parts() {
    let base = fixture_package();
    assert_eq!(decode_all(base.clone()), Ok(()), "seed decodes");
    let mut total_ok = 0;
    let mut codes = std::collections::BTreeSet::new();
    for (index, name) in ["Stories/Story_ua1.xml", "Stories/Story_ua2.xml", "Spreads/Spread_us1.xml", "designmap.xml", "Resources/Styles.xml", "Resources/Graphic.xml", "Resources/Preferences.xml"].into_iter().enumerate() {
        let at = base.parts.iter().position(|p| p.name == name).expect("part");
        let seeds = vec![base.parts[at].bytes.clone()];
        let report = sweep("idml_part", 0x1D31_0000_0000_0100 + index as u64, &seeds, 120, PROFILE, CAP, |b| {
            let mut p = base.clone();
            p.parts[at] = IdmlPart::new(name.to_owned(), b.to_vec());
            decode_all(p)
        });
        println!("PART {name}");
        // Per part: no panic, no cap breach, deterministic; reach is judged over all parts.
        assert!(report.panics.is_empty() && report.over_cap.is_empty() && report.nondeterministic.is_empty(), "{}", report.line());
        println!("{}", report.line());
        total_ok += report.ok;
        codes.extend(report.codes.keys().copied());
    }
    println!("MUTATION target=idml_parts total_ok={total_ok} codes={codes:?}");
    assert!(total_ok >= 23 && codes.len() >= 3, "reach: ok {total_ok}, codes {codes:?}");
}

#[test]
fn idml_mutation_sweep_container() {
    let bytes = write_idml(&fixture_package()).expect("write fixture");
    let answer = |b: &[u8]| -> Answer {
        let mut mint = SequentialIdMint::new(7);
        import_idml(b, &Limits::default(), &mut mint, &CancellationToken::default()).map(|_| ()).map_err(|e| match e {
            IdmlError::Zip(w) | IdmlError::BudgetExceeded(w) | IdmlError::Mimetype(w) | IdmlError::NotIdml(w) => w,
            other => other.code(),
        })
    };
    assert_eq!(answer(&bytes), Ok(()));
    let report = sweep("idml_container", 0x1D31_0000_0000_0200, &[bytes], 200, Profile { dictionary: &[b"PK\x03\x04", b"PK\x01\x02", b"mimetype"], text: false }, CAP, answer);
    // Every mutant changes a CRC-covered or structural byte, so none reads (measured ok 0): the part
    // sweep above is where damage reaches the decoder. Here: no panic, no cap breach, deterministic,
    // and the ZIP reader's refusals stay distinct.
    println!("{}", report.line());
    assert!(report.panics.is_empty() && report.over_cap.is_empty() && report.nondeterministic.is_empty(), "{}", report.line());
    assert!(report.codes.len() >= 3, "{}", report.line());
}
