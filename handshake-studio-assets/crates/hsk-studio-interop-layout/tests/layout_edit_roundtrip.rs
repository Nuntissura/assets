//! Focused proof for the IDML slice (MT-35551 case names). Every package is synthetic and built in
//! the test with its own ZIP writer (independent of the crate's reader/writer): a threaded
//! two-frame story on a facing-pages spread, an unknown graphics part, a graphic page item and a
//! table the decoder must report instead of dropping silently.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{PageSide, StoryDirection, StoryParagraphDirection};
use hsk_studio_interop_layout::*;
use miniz_oxide::deflate::compress_to_vec;

const NS: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";
const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

// ---- independent ZIP builder (oracle for the crate's reader) -------------------------------------

fn crc(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in data {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

#[derive(Clone)]
struct Z {
    name: String,
    data: Vec<u8>,
    deflate: bool,
    declared: Option<u32>,
}
impl Z {
    fn stored(name: &str, data: &[u8]) -> Self {
        Self {
            name: name.into(),
            data: data.to_vec(),
            deflate: false,
            declared: None,
        }
    }
    fn deflated(name: &str, data: &[u8]) -> Self {
        Self {
            deflate: true,
            ..Self::stored(name, data)
        }
    }
}

fn zip(entries: &[Z]) -> Vec<u8> {
    let (mut out, mut cd) = (Vec::new(), Vec::new());
    for e in entries {
        let body = if e.deflate {
            compress_to_vec(&e.data, 6)
        } else {
            e.data.clone()
        };
        let (method, size) = (
            if e.deflate { 8u16 } else { 0 },
            e.declared.unwrap_or(e.data.len() as u32),
        );
        let off = out.len() as u32;
        let fields = |v: &mut Vec<u8>, central: bool| {
            v.extend_from_slice(
                &(if central { 0x0201_4b50u32 } else { 0x0403_4b50 }).to_le_bytes(),
            );
            if central {
                v.extend_from_slice(&20u16.to_le_bytes());
            }
            for x in [20u16, 0, method, 0, 0x21] {
                v.extend_from_slice(&x.to_le_bytes());
            }
            for x in [crc(&e.data), body.len() as u32, size] {
                v.extend_from_slice(&x.to_le_bytes());
            }
            v.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
            if central {
                v.extend_from_slice(&[0; 10]);
                v.extend_from_slice(&off.to_le_bytes());
            }
            v.extend_from_slice(e.name.as_bytes());
        };
        fields(&mut out, false);
        out.extend_from_slice(&body);
        fields(&mut cd, true);
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

// ---- synthetic IDML ------------------------------------------------------------------------------

type Parts = Vec<(String, Vec<u8>)>;

fn frame(id: &str, story: &str, prev: &str, next: &str, x0: i32, x1: i32) -> String {
    let pt = |x: i32, y: i32| {
        format!(
            r#"<PathPointType Anchor="{x} {y}" LeftDirection="{x} {y}" RightDirection="{x} {y}"/>"#
        )
    };
    format!(
        r#"<TextFrame Self="{id}" ParentStory="{story}" PreviousTextFrame="{prev}" NextTextFrame="{next}" ContentType="TextType" ItemTransform="1 0 0 1 0 0" Name="{id}-name"><Properties><PathGeometry><GeometryPathType PathOpen="false"><PathPointArray>{}{}{}{}</PathPointArray></GeometryPathType></PathGeometry></Properties><TextFramePreference TextColumnCount="1" TextColumnGutter="12" VerticalJustification="TopAlign" FirstBaselineOffset="AscentOffset"><Properties><InsetSpacing type="list"><ListItem type="unit">6</ListItem><ListItem type="unit">5</ListItem><ListItem type="unit">4</ListItem><ListItem type="unit">3</ListItem></InsetSpacing></Properties></TextFramePreference></TextFrame>"#,
        pt(x0, -180),
        pt(x0, 120),
        pt(x1, 120),
        pt(x1, -180)
    )
}

fn spread(frames: &str) -> String {
    format!(
        r#"{DECL}<idPkg:Spread xmlns:idPkg="{NS}" DOMVersion="20.0"><Spread Self="us1" PageCount="2" BindingLocation="1" ItemTransform="1 0 0 1 0 0" AppliedMaster="n">
<Page Self="up1" Name="2" GeometricBounds="0 0 400 300" ItemTransform="1 0 0 1 -300 -200" AppliedMaster="n"><MarginPreference ColumnCount="1" ColumnGutter="12" Top="20" Bottom="20" Left="10" Right="10"/></Page>
<Page Self="up2" Name="3" GeometricBounds="0 0 400 300" ItemTransform="1 0 0 1 0 -200" AppliedMaster="n"/>
<Rectangle Self="ur1" ItemTransform="1 0 0 1 0 0"/>
{frames}
</Spread></idPkg:Spread>"#
    )
}

fn threaded() -> String {
    spread(&format!(
        "{}\n{}",
        frame("uf1", "ua1", "n", "uf2", -280, -20),
        frame("uf2", "ua1", "uf1", "n", 20, 280)
    ))
}

const STORY1: &str = r#"<Story Self="ua1" AppliedTOCStyle="n" TrackChanges="false" StoryTitle="$ID/">
<StoryPreference OpticalMarginAlignment="false" StoryOrientation="Horizontal" StoryDirection="LeftToRightDirection"/>
<InCopyExportOption IncludeGraphicProxies="true"/>
<ParagraphStyleRange AppliedParagraphStyle="ParagraphStyle/$ID/NormalParagraphStyle">
<CharacterStyleRange AppliedCharacterStyle="CharacterStyle/$ID/[No character style]" AppliedLanguage="$ID/English: USA"><Content>Hello &amp; welcome</Content><Br/></CharacterStyleRange>
</ParagraphStyleRange>
<ParagraphStyleRange AppliedParagraphStyle="ParagraphStyle/Body" Justification="LeftAlign">
<Properties><Leading type="unit">14</Leading></Properties>
<CharacterStyleRange AppliedCharacterStyle="CharacterStyle/Bold" FontStyle="Bold"><Content>Second </Content></CharacterStyleRange>
<CharacterStyleRange AppliedCharacterStyle="CharacterStyle/$ID/[No character style]"><Content>paragraph here.</Content></CharacterStyleRange>
<Table Self="utbl"/>
</ParagraphStyleRange>
</Story>"#;

fn story_part(inner: &str) -> String {
    format!("{DECL}<idPkg:Story xmlns:idPkg=\"{NS}\" DOMVersion=\"20.0\">{inner}</idPkg:Story>")
}

fn designmap(stories: &[&str]) -> String {
    let refs: String = stories
        .iter()
        .map(|s| format!("<idPkg:Story src=\"Stories/Story_{s}.xml\"/>\n"))
        .collect();
    format!(
        r#"{DECL}<Document xmlns:idPkg="{NS}" DOMVersion="20.0" Self="d" StoryList="{}">
<Language Self="Language/$ID/English%3a USA" Name="$ID/English: USA"/>
<idPkg:Graphic src="Resources/Graphic.xml"/>
<idPkg:Preferences src="Resources/Preferences.xml"/>
<idPkg:Spread src="Spreads/Spread_us1.xml"/>
{refs}</Document>"#,
        stories.join(" ")
    )
}

fn parts() -> Parts {
    let prefs = format!(
        r#"{DECL}<idPkg:Preferences xmlns:idPkg="{NS}" DOMVersion="20.0"><DocumentPreference PageHeight="400" PageWidth="300" FacingPages="true" DocumentBleedTopOffset="9" DocumentBleedBottomOffset="9" DocumentBleedInsideOrLeftOffset="9" DocumentBleedOutsideOrRightOffset="9"/><MarginPreference ColumnCount="1" ColumnGutter="12" Top="36" Bottom="40" Left="30" Right="32"/></idPkg:Preferences>"#
    );
    let graphic = format!(
        "{DECL}<idPkg:Graphic xmlns:idPkg=\"{NS}\"><!--UNKNOWN-PART-MARKER--><Color Self=\"Color/Black\"/></idPkg:Graphic>"
    );
    [
        ("mimetype", IDML_MIMETYPE.to_owned()),
        ("META-INF/container.xml", format!("{DECL}<container><rootfiles><rootfile full-path=\"designmap.xml\"/></rootfiles></container>")),
        ("designmap.xml", designmap(&["ua1"])),
        ("Resources/Graphic.xml", graphic),
        ("Resources/Preferences.xml", prefs),
        ("Spreads/Spread_us1.xml", threaded()),
        ("Stories/Story_ua1.xml", story_part(STORY1)),
    ]
    .into_iter()
    .map(|(n, c)| (n.to_owned(), c.into_bytes()))
    .collect()
}

fn set(parts: &mut Parts, name: &str, content: String) {
    match parts.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = content.into_bytes(),
        None => parts.push((name.to_owned(), content.into_bytes())),
    }
}

fn build(parts: &Parts) -> Vec<u8> {
    let entries: Vec<Z> = parts
        .iter()
        .map(|(n, b)| {
            if n == "mimetype" {
                Z::stored(n, b)
            } else {
                Z::deflated(n, b)
            }
        })
        .collect();
    zip(&entries)
}

fn limits() -> Limits {
    Limits::default()
}
fn import_with(bytes: &[u8], limits: &Limits) -> Result<IdmlImport> {
    import_idml(
        bytes,
        limits,
        &mut SequentialIdMint::new(1),
        &CancellationToken::default(),
    )
}
fn import(bytes: &[u8]) -> Result<IdmlImport> {
    import_with(bytes, &limits())
}
fn has(
    loss: &[LossEntry],
    part: &str,
    element: &str,
    kind: LossKind,
    code: &str,
    count: u32,
) -> bool {
    loss.iter().any(|l| {
        l.part == part
            && l.element == element
            && l.kind == kind
            && l.code == code
            && l.count == count
    })
}
fn len(v: &[f64]) -> Vec<f64> {
    v.to_vec()
}

// ---- tests ---------------------------------------------------------------------------------------

#[test]
fn idml_threaded_story_style_edit_reopen_twice() {
    let cancel = CancellationToken::default();
    let original = build(&parts());
    let a = import(&original).expect("import");

    // Pages per spread, margins (page override vs document default), bleed, side.
    let doc = a.proposal.snapshot.document();
    assert_eq!(doc.page_spreads.len(), 1);
    let spread = &doc.page_spreads[0];
    assert_eq!((spread.name.as_str(), spread.pages.len()), ("2-3", 2));
    let (l, r) = (&spread.pages[0], &spread.pages[1]);
    assert_eq!((l.side, r.side), (PageSide::LeftHand, PageSide::RightHand));
    assert_eq!((l.width.value, l.height.value), (300.0, 400.0));
    let m = |p: &hsk_studio_folio::StudioPage| {
        len(&[
            p.margins.top.value,
            p.margins.right.value,
            p.margins.bottom.value,
            p.margins.left.value,
        ])
    };
    assert_eq!(m(l), [20.0, 10.0, 20.0, 10.0]);
    assert_eq!(m(r), [36.0, 32.0, 40.0, 30.0]);
    assert_eq!(l.bleed.as_ref().map(|b| b.inside_or_left.value), Some(9.0));

    // Text frames and the thread.
    assert_eq!(doc.layers.len(), 2);
    let threads = doc.text_threads();
    assert_eq!(threads.len(), 1);
    let (f1, f2) = (&doc.layers[0], &doc.layers[1]);
    assert_eq!(
        threads[0].frames(),
        [f1.layer_id.clone(), f2.layer_id.clone()]
    );
    let (t1, t2) = (
        f1.text_frame.as_ref().expect("frame"),
        f2.text_frame.as_ref().expect("frame"),
    );
    assert_eq!(
        (t1.page_id.as_str(), t2.page_id.as_str()),
        (l.page_id.as_str(), r.page_id.as_str())
    );
    for t in [t1, t2] {
        let b = &t.bounds;
        assert_eq!(
            [b.x.value, b.y.value, b.width.value, b.height.value],
            [20.0, 20.0, 260.0, 300.0]
        );
        let i = &t.inset_spacing;
        assert_eq!(
            [i.top.value, i.right.value, i.bottom.value, i.left.value],
            [6.0, 3.0, 4.0, 5.0]
        );
    }
    assert_eq!(t1.next_text_frame.as_deref(), Some(f2.layer_id.as_str()));
    assert_eq!(
        t2.previous_text_frame.as_deref(),
        Some(f1.layer_id.as_str())
    );
    assert!(
        a.proposal
            .ids
            .iter()
            .any(|i| i.kind == "frame" && i.idml == "uf1" && i.folio == f1.layer_id)
    );

    // Story: text, paragraph/character ranges, style references (unresolved, definitions not imported).
    let ps = &a.proposal.stories[0];
    assert!(ps.reflow_certified);
    assert_eq!(
        ps.story.inline_text(),
        Some("Hello & welcome\nSecond paragraph here.")
    );
    let paras: Vec<_> = ps
        .story
        .paragraphs
        .iter()
        .map(|p| (p.start, p.end))
        .collect();
    assert_eq!(paras, [(0, 16), (16, 38)]);
    let ranges: Vec<_> = ps
        .story
        .character_ranges
        .iter()
        .map(|c| {
            (
                c.start,
                c.end,
                c.style_ref.is_some(),
                c.applied_language.clone(),
            )
        })
        .collect();
    assert_eq!(
        ranges,
        [
            (0, 16, false, Some("English__USA".to_owned())),
            (16, 23, true, None),
            (23, 38, false, None)
        ]
    );
    let named = |n: &str| {
        a.proposal
            .style_refs
            .iter()
            .find(|s| s.idml_name == n)
            .map(|s| s.folio.clone())
    };
    assert_eq!(a.proposal.style_refs.len(), 3);
    assert_eq!(
        ps.story.paragraphs[1].style_ref,
        named("ParagraphStyle/Body")
    );
    assert_eq!(
        ps.story.character_ranges[1].style_ref,
        named("CharacterStyle/Bold")
    );
    assert_eq!(a.proposal.unresolved.len(), 3);

    // The loss report names what was not interpreted.
    use LossKind::*;
    assert!(has(
        &a.loss,
        "Stories/Story_ua1.xml",
        "Table",
        Unsupported,
        "paragraph_range_element_not_interpreted",
        1
    ));
    assert!(has(
        &a.loss,
        "Spreads/Spread_us1.xml",
        "Rectangle",
        PreservedOpaque,
        "graphic_page_item_not_interpreted",
        1
    ));
    assert!(has(
        &a.loss,
        "Resources/Graphic.xml",
        "part",
        PreservedOpaque,
        "part_preserved_verbatim",
        1
    ));

    // Byte-exact edit of the first Content element, then reopen twice.
    let story = &a.model.stories[0];
    let edit = ContentEdit {
        part: &story.part,
        expected_part_sha256: story.part_sha256,
        span: story.paragraphs[0].runs[0].span.clone().expect("span"),
        new_text: "Hello <&> brave world",
    };
    let edited = replace_content(&a.package, &edit, &limits(), &cancel).expect("edit");
    for p in &a.package.parts {
        let q = edited.part(&p.name).expect("part kept");
        assert_eq!(p.name == story.part, p.bytes != q.bytes, "{}", p.name);
    }
    let edited_text =
        std::str::from_utf8(&edited.part(&story.part).expect("story").bytes).expect("utf8");
    assert!(edited_text.contains("<Content>Hello &lt;&amp;&gt; brave world</Content>"));
    assert!(edited_text.contains("<Table Self=\"utbl\"/>"));

    let b1 = write_idml(&edited).expect("write");
    let r1 = import(&b1).expect("reopen 1");
    let b2 = write_idml(&r1.package).expect("write again");
    assert_eq!(b1, b2, "container is deterministic");
    let r2 = import(&b2).expect("reopen 2");
    for r in [&r1, &r2] {
        assert_eq!(
            r.proposal.stories[0].story.inline_text(),
            Some("Hello <&> brave world\nSecond paragraph here.")
        );
        assert_eq!(r.loss, a.loss, "loss list identical after the edit");
        assert_eq!(
            r.proposal.snapshot.document(),
            a.proposal.snapshot.document()
        );
        assert_eq!(
            r.package.part("Resources/Graphic.xml"),
            a.package.part("Resources/Graphic.xml")
        );
        assert_eq!(r.proposal.snapshot.document().text_threads().len(), 1);
    }
    // An unedited package re-emits byte-equal parts.
    let again = import(&write_idml(&a.package).expect("write")).expect("reopen");
    assert_eq!(again.package.parts, a.package.parts);
}

#[test]
fn idml_export_roundtrip_is_deterministic_and_reports_loss() {
    let cancel = CancellationToken::default();
    let a = import(&build(&parts())).expect("import");
    let e1 = export_idml(&a, &cancel).expect("export");
    assert_eq!(
        e1,
        export_idml(&a, &cancel).expect("export twice"),
        "export is deterministic"
    );

    let b = import(&e1).expect("import -> export -> import");
    let e2 = export_idml(&b, &cancel).expect("export of the re-import");
    assert_eq!(e1, e2, "export is a fixed point");

    // Same typed model (byte spans and part hashes address different bytes), same Folio proposal.
    let strip = |m: &IdmlModel| {
        let mut m = m.clone();
        for s in &mut m.stories {
            s.part_sha256 = [0; 32];
            s.paragraphs
                .iter_mut()
                .flat_map(|p| p.runs.iter_mut())
                .for_each(|r| r.span = None);
        }
        m
    };
    assert_eq!(strip(&a.model), strip(&b.model));
    assert_eq!(
        a.proposal.snapshot.document(),
        b.proposal.snapshot.document()
    );
    assert_eq!(
        a.proposal
            .stories
            .iter()
            .map(|s| &s.story)
            .collect::<Vec<_>>(),
        b.proposal
            .stories
            .iter()
            .map(|s| &s.story)
            .collect::<Vec<_>>()
    );

    // Unknown parts, designmap children and graphic page items survive verbatim; only the table
    // (reported `unsupported`) is gone, and the second import no longer reports it.
    assert_eq!(
        b.package.part("Resources/Graphic.xml"),
        a.package.part("Resources/Graphic.xml")
    );
    let designmap = std::str::from_utf8(&b.package.part("designmap.xml").expect("designmap").bytes)
        .expect("utf8")
        .to_owned();
    assert!(
        designmap
            .contains("<Language Self=\"Language/$ID/English%3a USA\" Name=\"$ID/English: USA\"/>")
    );
    let spread = std::str::from_utf8(
        &b.package
            .part("Spreads/Spread_us1.xml")
            .expect("spread")
            .bytes,
    )
    .expect("utf8")
    .to_owned();
    assert!(spread.contains("<Rectangle Self=\"ur1\""));
    let story = std::str::from_utf8(
        &b.package
            .part("Stories/Story_ua1.xml")
            .expect("story")
            .bytes,
    )
    .expect("utf8")
    .to_owned();
    assert!(!story.contains("utbl"));
    let table = |l: &LossEntry| l.element == "Table";
    assert_eq!(a.loss.iter().filter(|l| table(l)).count(), 1);
    assert_eq!(
        a.loss
            .iter()
            .filter(|l| !table(l))
            .cloned()
            .collect::<Vec<_>>(),
        b.loss
    );
    // Structure is intact: paragraph styles, language and thread.
    assert_eq!(
        b.proposal.snapshot.document().text_threads()[0]
            .frames()
            .len(),
        2
    );
    assert_eq!(
        b.proposal.stories[0].story.character_ranges[0]
            .applied_language
            .as_deref(),
        Some("English__USA")
    );
}

#[test]
fn zip_traversal_duplicate_inflate_xml_entity() {
    let open = |b: &[u8], l: &Limits| open_idml(b, l, &CancellationToken::default());
    let mime = Z::stored("mimetype", IDML_MIMETYPE.as_bytes());
    let map = Z::deflated("designmap.xml", designmap(&[]).as_bytes());
    let good = vec![mime.clone(), map.clone()];
    assert!(open(&zip(&good), &limits()).is_ok());
    let with = |extra: Z| {
        let mut v = good.clone();
        v.push(extra);
        zip(&v)
    };

    // Traversal, absolute, backslash and drive names.
    for bad in [
        "../evil.xml",
        "a/../../evil.xml",
        "/abs.xml",
        "dir\\evil.xml",
        "C:evil.xml",
    ] {
        assert_eq!(
            open(&with(Z::deflated(bad, b"x")), &limits()).unwrap_err(),
            IdmlError::UnsafeEntryName,
            "{bad}"
        );
    }
    // Duplicates, exact and case-folded.
    assert_eq!(
        open(&with(Z::deflated("designmap.xml", b"x")), &limits()).unwrap_err(),
        IdmlError::DuplicateEntry
    );
    assert_eq!(
        open(&with(Z::deflated("DesignMap.XML", b"x")), &limits()).unwrap_err(),
        IdmlError::DuplicateEntry
    );
    // mimetype first, stored, exact.
    assert_eq!(
        open(&zip(&[map.clone(), mime.clone()]), &limits()).unwrap_err(),
        IdmlError::Mimetype("not_first")
    );
    assert_eq!(
        open(
            &zip(&[
                Z::deflated("mimetype", IDML_MIMETYPE.as_bytes()),
                map.clone()
            ]),
            &limits()
        )
        .unwrap_err(),
        IdmlError::Mimetype("compressed")
    );
    assert_eq!(
        open(
            &zip(&[Z::stored("mimetype", b"application/zip"), map.clone()]),
            &limits()
        )
        .unwrap_err(),
        IdmlError::Mimetype("wrong_value")
    );
    assert_eq!(
        open(&zip(std::slice::from_ref(&map)), &limits()).unwrap_err(),
        IdmlError::Mimetype("missing")
    );
    assert_eq!(
        open(&zip(std::slice::from_ref(&mime)), &limits()).unwrap_err(),
        IdmlError::NotIdml("designmap_missing")
    );
    // Inflate bomb: honest sizes trip the ratio before any inflate; a lying small size trips the cap.
    let zeros = vec![0u8; 20 << 20];
    assert_eq!(
        open(&with(Z::deflated("Spreads/bomb.xml", &zeros)), &limits()).unwrap_err(),
        IdmlError::BudgetExceeded("ratio")
    );
    let mut lie = Z::deflated("Spreads/bomb.xml", &zeros);
    lie.declared = Some(100);
    assert_eq!(
        open(&with(lie), &limits()).unwrap_err(),
        IdmlError::Zip("inflate_failed_or_size_mismatch")
    );
    let small = Limits {
        max_entry_bytes: 1000,
        ..limits()
    };
    assert_eq!(
        open(&with(Z::stored("Spreads/a.xml", &[b'a'; 2000])), &small).unwrap_err(),
        IdmlError::BudgetExceeded("entry_bytes")
    );
    // Entry count and container size limits.
    let few = Limits {
        max_entries: 2,
        ..limits()
    };
    assert_eq!(
        open(&with(Z::stored("Spreads/a.xml", b"a")), &few).unwrap_err(),
        IdmlError::BudgetExceeded("entries")
    );
    let tiny = Limits {
        max_bytes: 10,
        ..limits()
    };
    assert_eq!(
        open(&zip(&good), &tiny).unwrap_err(),
        IdmlError::BudgetExceeded("bytes")
    );
    // Damaged containers: flipped stored byte (CRC), truncation, garbage. No panic, typed errors.
    let mut flipped = zip(&[mime.clone(), Z::stored("designmap.xml", b"<Document/>")]);
    let at = flipped
        .windows(11)
        .position(|w| w == b"<Document/>")
        .expect("stored data");
    flipped[at + 1] ^= 1;
    assert_eq!(
        open(&flipped, &limits()).unwrap_err(),
        IdmlError::Zip("crc_mismatch")
    );
    let whole = zip(&good);
    for cut in [0, 5, 30, whole.len() - 10, whole.len() - 1] {
        assert!(
            matches!(
                open(&whole[..cut], &limits()).unwrap_err(),
                IdmlError::Zip(_)
            ),
            "cut {cut}"
        );
    }
    assert!(matches!(
        open(&[b'x'; 64], &limits()).unwrap_err(),
        IdmlError::Zip(_)
    ));

    // XML: DOCTYPE with an entity declaration or an external id is refused; malformed XML is typed.
    let bomb = format!(
        "{DECL}<!DOCTYPE Story [<!ENTITY a \"bbbbbbbbbb\"><!ENTITY b \"&a;&a;&a;&a;\">]><idPkg:Story xmlns:idPkg=\"{NS}\"><Story Self=\"ua1\"><ParagraphStyleRange><CharacterStyleRange><Content>&b;</Content></CharacterStyleRange></ParagraphStyleRange></Story></idPkg:Story>"
    );
    let external = format!(
        "{DECL}<!DOCTYPE Story SYSTEM \"file:///etc/passwd\"><idPkg:Story xmlns:idPkg=\"{NS}\"><Story Self=\"ua1\"/></idPkg:Story>"
    );
    for hostile in [bomb, external] {
        let mut p = parts();
        set(&mut p, "Stories/Story_ua1.xml", hostile);
        assert_eq!(import(&build(&p)).unwrap_err(), IdmlError::EntityDenied);
    }
    let mut p = parts();
    set(
        &mut p,
        "Stories/Story_ua1.xml",
        story_part("<Story Self=\"ua1\"><Content>&nope;</Content></Story>"),
    );
    assert_eq!(import(&build(&p)).unwrap_err(), IdmlError::MalformedXml);
    let mut p = parts();
    set(
        &mut p,
        "Spreads/Spread_us1.xml",
        spread("<TextFrame").to_owned(),
    );
    assert_eq!(import(&build(&p)).unwrap_err(), IdmlError::MalformedXml);
    // Distinct stable codes.
    let codes: std::collections::BTreeSet<_> = [
        IdmlError::UnsafeEntryName,
        IdmlError::DuplicateEntry,
        IdmlError::Mimetype("x"),
        IdmlError::BudgetExceeded("x"),
        IdmlError::Zip("x"),
        IdmlError::EntityDenied,
        IdmlError::MalformedXml,
    ]
    .iter()
    .map(IdmlError::code)
    .collect();
    assert_eq!(codes.len(), 7);
}

#[test]
fn indd_actual_native_editability_or_blocker() {
    let cancel = CancellationToken::default();
    let mut indd = vec![
        0x06, 0x06, 0xED, 0xF5, 0xD8, 0x1D, 0x46, 0xE5, 0xBD, 0x31, 0xEF, 0xE7, 0xFE, 0x74, 0xB7,
        0x1D,
    ];
    indd.extend_from_slice(b"DOCUMENT");
    indd.extend_from_slice(&[1, 8, 0, 0, 0]);
    indd.extend_from_slice(&[0xAB; 4096]);
    assert_eq!(
        identify_indd(&indd),
        IndKind::Document {
            little_endian: Some(true),
            version: Some(8)
        }
    );
    let err = match import_indd(&indd, &limits(), &cancel) {
        Ok(never) => match never {},
        Err(e) => e,
    };
    assert_eq!(err.code(), "native_editability_blocked");
    match &err {
        IdmlError::NativeEditabilityBlocked { preserved, .. } => {
            assert_eq!((preserved.kind, &preserved.bytes), ("indd_document", &indd));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        err.failure_code(),
        Some(hsk_studio_observe::FailureCode::Unavailable)
    );

    // Book and library headers, big-endian document, short header.
    let header = |tag: &[u8; 8], rest: &[u8]| {
        let mut v = indd[..16].to_vec();
        v.extend_from_slice(tag);
        v.extend_from_slice(rest);
        v
    };
    assert_eq!(
        identify_indd(&header(b"BOOKBOOK", &[1, 0, 0, 0, 0])),
        IndKind::Book
    );
    assert_eq!(
        identify_indd(&header(b"LIBRARY4", &[1, 0, 0, 0, 0])),
        IndKind::Library
    );
    assert_eq!(identify_indd(&header(b"MYSTERY!", &[])), IndKind::Unknown);
    assert_eq!(
        identify_indd(&header(b"DOCUMENT", &[2, 0, 0, 0, 9])),
        IndKind::Document {
            little_endian: Some(false),
            version: Some(9)
        }
    );
    assert_eq!(
        identify_indd(&header(b"DOCUMENT", &[])),
        IndKind::Document {
            little_endian: None,
            version: None
        }
    );

    // Anything else, including a valid IDML package, is not INDD and is never "readable".
    let idml = build(&parts());
    for other in [&idml[..], b"%PDF-1.7", b"", &indd[..15]] {
        assert_eq!(identify_indd(other), IndKind::NotIndd);
        assert!(matches!(
            import_indd(other, &limits(), &cancel),
            Err(IdmlError::Unsupported("not_an_indd_file"))
        ));
    }
    assert!(matches!(
        import_indd(
            &indd,
            &Limits {
                max_bytes: 100,
                ..limits()
            },
            &cancel
        ),
        Err(IdmlError::BudgetExceeded("bytes"))
    ));
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert!(matches!(
        import_indd(&indd, &limits(), &canceled),
        Err(IdmlError::Canceled)
    ));
}

#[test]
fn rtl_cjk_cyclic_dangling_cancel_stale() {
    let cancel = CancellationToken::default();

    // RTL + CJK story: unsupported composition, never certified for a Latin reflow.
    let rtl = String::from(
        "<Story Self=\"ua2\"><StoryPreference StoryDirection=\"RightToLeftDirection\"/>\
         <ParagraphStyleRange AppliedParagraphStyle=\"ParagraphStyle/$ID/NormalParagraphStyle\" ParagraphDirection=\"RightToLeftDirection\">\
         <CharacterStyleRange AppliedLanguage=\"$ID/Arabic\"><Content>\u{645}\u{631}\u{62d}\u{628}\u{627}</Content></CharacterStyleRange>\
         <CharacterStyleRange AppliedLanguage=\"$ID/Japanese\" KinsokuSet=\"KinsokuTable/$ID/Hard\"><Content>\u{65e5}\u{672c}\u{8a9e}</Content></CharacterStyleRange>\
         </ParagraphStyleRange></Story>"
    );
    let mut p = parts();
    set(&mut p, "designmap.xml", designmap(&["ua1", "ua2"]));
    set(&mut p, "Stories/Story_ua2.xml", story_part(&rtl));
    let r = import(&build(&p)).expect("import with rtl story");
    let (latin, arabic) = (&r.proposal.stories[0], &r.proposal.stories[1]);
    assert!(latin.reflow_certified && latin.composition.is_empty());
    assert!(!arabic.reflow_certified);
    assert_eq!(
        arabic.composition,
        [
            "cjk_language",
            "cjk_typography",
            "rtl_direction",
            "rtl_language"
        ]
    );
    assert_eq!(arabic.story.story_direction, StoryDirection::RightToLeft);
    assert_eq!(
        arabic.story.paragraphs[0].paragraph_direction,
        Some(StoryParagraphDirection::RightToLeft)
    );
    assert_eq!(
        arabic.story.inline_text(),
        Some("\u{645}\u{631}\u{62d}\u{628}\u{627}\u{65e5}\u{672c}\u{8a9e}")
    );
    for code in [
        "rtl_direction",
        "rtl_language",
        "cjk_language",
        "cjk_typography",
    ] {
        assert!(
            has(
                &r.loss,
                "Stories/Story_ua2.xml",
                "Story",
                LossKind::UnsupportedComposition,
                code,
                1
            ),
            "{code}"
        );
    }
    assert!(
        !r.loss
            .iter()
            .any(|l| l.kind == LossKind::UnsupportedComposition && l.part.ends_with("ua1.xml"))
    );

    // Thread errors are typed, never silently dropped.
    let with_frames = |a: &str, b: &str| {
        let mut p = parts();
        set(
            &mut p,
            "Spreads/Spread_us1.xml",
            spread(&format!("{a}\n{b}")),
        );
        import(&build(&p))
    };
    let (f1, f2) = (
        |prev, next| frame("uf1", "ua1", prev, next, -280, -20),
        |prev, next| frame("uf2", "ua1", prev, next, 20, 280),
    );
    assert_eq!(
        with_frames(&f1("uf2", "uf2"), &f2("uf1", "uf1")).unwrap_err(),
        IdmlError::CyclicThread
    );
    assert_eq!(
        with_frames(&f1("n", "uf2"), &f2("n", "n")).unwrap_err(),
        IdmlError::ThreadNotReciprocal
    );
    assert_eq!(
        with_frames(
            &f1("n", "uf2"),
            &frame("uf2", "missing", "uf1", "n", 20, 280)
        )
        .unwrap_err(),
        IdmlError::DanglingStory
    );
    // A link to a frame outside the supported subset ends the chain there and is reported.
    let outside = with_frames(&f1("n", "ugone"), &f2("n", "n")).expect("outside link");
    assert_eq!(outside.proposal.snapshot.document().layers.len(), 1);
    assert!(has(
        &outside.loss,
        "Spreads/Spread_us1.xml",
        "TextFrame",
        LossKind::Approximated,
        "thread_link_outside_subset",
        1
    ));
    assert!(has(
        &outside.loss,
        "Spreads/Spread_us1.xml",
        "TextFrame",
        LossKind::Approximated,
        "extra_thread_chain_not_projected",
        1
    ));

    // Stale source and invalid edits.
    let a = import(&build(&parts())).expect("import");
    let story = &a.model.stories[0];
    let span = story.paragraphs[0].runs[0].span.clone().expect("span");
    let edit = |sha, span, text| ContentEdit {
        part: &story.part,
        expected_part_sha256: sha,
        span,
        new_text: text,
    };
    let edited = replace_content(
        &a.package,
        &edit(story.part_sha256, span.clone(), "one"),
        &limits(),
        &cancel,
    )
    .expect("edit");
    assert_eq!(
        replace_content(
            &edited,
            &edit(story.part_sha256, span.clone(), "two"),
            &limits(),
            &cancel
        )
        .unwrap_err(),
        IdmlError::StaleSource
    );
    assert_eq!(
        replace_content(
            &a.package,
            &edit(story.part_sha256, 0..5, "x"),
            &limits(),
            &cancel
        )
        .unwrap_err(),
        IdmlError::InvalidEdit("span_is_not_an_element")
    );
    let text = std::str::from_utf8(&a.package.part(&story.part).expect("story").bytes)
        .expect("utf8")
        .to_owned();
    let start = text.find("<Table Self=\"utbl\"/>").expect("table");
    assert_eq!(
        replace_content(
            &a.package,
            &edit(story.part_sha256, start..start + 20, "x"),
            &limits(),
            &cancel
        )
        .unwrap_err(),
        IdmlError::InvalidEdit("not_content")
    );
    assert_eq!(
        replace_content(
            &a.package,
            &edit(story.part_sha256, span.clone(), "bell\u{7}"),
            &limits(),
            &cancel
        )
        .unwrap_err(),
        IdmlError::InvalidEdit("illegal_xml_character")
    );

    // Pre-cancelled calls return `canceled` (Observe: Canceled) and produce nothing.
    let stop = CancellationToken::default();
    stop.cancel();
    let bytes = build(&parts());
    let err = import_idml(&bytes, &limits(), &mut SequentialIdMint::new(1), &stop).unwrap_err();
    assert_eq!(
        (err.clone(), err.failure_code()),
        (IdmlError::Canceled, None)
    );
    assert_eq!(export_idml(&a, &stop).unwrap_err(), IdmlError::Canceled);
    assert_eq!(
        replace_content(
            &a.package,
            &edit(story.part_sha256, span, "x"),
            &limits(),
            &stop
        )
        .unwrap_err(),
        IdmlError::Canceled
    );
}
