//! R5 remediation proof: the XML of a range's `Properties` is shared by every run and paragraph
//! that comes from that range, never cloned per `Content` / `Br`. Own test binary so the counting
//! allocator below does not touch the foundation contract tests.
//!
//! Hostile shape: one `ParagraphStyleRange` and one `CharacterStyleRange` each carrying a large
//! `Properties` block, followed by many `<Content/><Br/>` pairs. Every pair becomes a paragraph and a
//! run. Before the fix each of them deep-copied both `Properties` blocks.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_layout::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

// ----- per-thread live-byte accounting -----------------------------------------------------------

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

fn track(delta: isize) {
    let _ = LIVE.try_with(|live| live.set(live.get() + delta));
}

struct Tracking;
// SAFETY: every method forwards to the system allocator unchanged; the counter only observes.
unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            track(layout.size() as isize);
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            track(layout.size() as isize);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        track(-(layout.size() as isize));
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            track(new_size as isize - layout.size() as isize);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

fn live() -> isize {
    LIVE.with(Cell::get)
}

fn bounded_case(name: &'static str, seconds: u64, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(seconds)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared {seconds}-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost deadline worker result")
        }
    }
}

// ----- fixture -------------------------------------------------------------------------------------

const NS: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";
const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
/// Size of the repeated `Properties` payload: large against everything else in the package.
const PAD: usize = 64 << 10;

fn part(name: &str, content: String) -> IdmlPart {
    IdmlPart::new(name.to_owned(), content.into_bytes())
}

/// A decodable package whose only story holds `paragraphs` paragraph/run pairs that all come from
/// one paragraph range and one character range, each with a `PAD`-byte `Properties` block.
fn hostile_package(paragraphs: usize) -> IdmlPackage {
    let props = format!(
        "<Properties><Leading type=\"unit\">14</Leading><Pad>{}</Pad></Properties>",
        "x".repeat(PAD)
    );
    let mut story = format!(
        "{DECL}<idPkg:Story xmlns:idPkg=\"{NS}\" DOMVersion=\"20.0\"><Story Self=\"ua1\">\
         <StoryPreference StoryDirection=\"LeftToRightDirection\"/>\
         <ParagraphStyleRange AppliedParagraphStyle=\"ParagraphStyle/P\">{props}\
         <CharacterStyleRange AppliedCharacterStyle=\"CharacterStyle/C\">{props}"
    );
    for _ in 0..paragraphs {
        story.push_str("<Content>x</Content><Br/>");
    }
    story.push_str("</CharacterStyleRange></ParagraphStyleRange></Story></idPkg:Story>");

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
            part("Stories/Story_ua1.xml", story),
        ],
    }
}

/// Heap bytes still held by the decoded import (model + proposal), measured with the import alive.
fn retained_bytes(paragraphs: usize, limits: &Limits) -> Result<usize> {
    let package = hostile_package(paragraphs);
    let before = live();
    let import = decode_idml(
        package,
        limits,
        &mut SequentialIdMint::new(1),
        &CancellationToken::default(),
    )?;
    let after = live();
    let paragraph_count: usize = import
        .model
        .stories
        .iter()
        .map(|s| s.paragraphs.len())
        .sum();
    assert_eq!(
        paragraph_count, paragraphs,
        "every Content/Br pair is one paragraph"
    );
    drop(import);
    Ok((after - before).max(0) as usize)
}

#[test]
fn range_properties_are_shared_not_cloned_per_content() {
    bounded_case("range_properties_shared", 60, || {
        let limits = Limits::default();
        let (few, many) = (20, 220);
        let small = retained_bytes(few, &limits).expect("decode few");
        let large = retained_bytes(many, &limits).expect("decode many");

        // Two Properties blocks of PAD bytes are held once; each extra paragraph adds only its own
        // run and paragraph records, so the growth per paragraph is a small fraction of PAD. A deep
        // copy per Content / Br grows by about 2 * PAD per paragraph.
        let per_paragraph = (large - small) / (many - few);
        assert!(
            per_paragraph < PAD / 8,
            "retained bytes grow {per_paragraph} per extra paragraph (Properties is {PAD} bytes): \
             the range Properties is cloned per Content/Br"
        );
        assert!(
            large < 4 * PAD + many * (PAD / 8),
            "{many} paragraphs retain {large} bytes for a {PAD}-byte Properties block"
        );
    });
}

/// Sharing fixes the decoder's memory, but the canonical export still writes the range's
/// `Properties` once per paragraph and run. The logical size is therefore charged against
/// `Limits::max_model_bytes`: a hostile package is a typed refusal at decode, and whatever decode
/// accepts exports to at most that many story bytes.
#[test]
fn logical_model_bytes_are_charged_and_bound_the_export() {
    bounded_case("logical_model_bytes", 60, || {
        let cancel = CancellationToken::default();
        let limit: u64 = 24 << 20;
        let limits = Limits {
            max_model_bytes: limit,
            ..Limits::default()
        };
        let decode = |paragraphs: usize| {
            decode_idml(
                hostile_package(paragraphs),
                &limits,
                &mut SequentialIdMint::new(1),
                &cancel,
            )
        };

        // Inside the budget: the export really is far larger than the input story part.
        let accepted = 150;
        let import = decode(accepted).expect("inside the model budget");
        let source_len = import
            .package
            .part("Stories/Story_ua1.xml")
            .map_or(0, |p| p.bytes.len());
        let exported = export_idml(&import, &cancel).expect("export");
        let reopen = Limits {
            max_ratio: 1 << 20, // the repeated Properties text deflates by orders of magnitude
            ..Limits::default()
        };
        let reopened = open_idml(&exported, &reopen, &cancel).expect("reopen the export");
        let story_len = reopened
            .part("Stories/Story_ua1.xml")
            .map_or(0, |p| p.bytes.len()) as u64;
        assert!(
            story_len >= (accepted * 2 * PAD) as u64 && story_len > 50 * source_len as u64,
            "the export repeats both Properties blocks per paragraph/run ({story_len} bytes from a \
             {source_len}-byte story part)"
        );
        assert!(
            story_len <= limit,
            "an accepted import exports {story_len} story bytes, over max_model_bytes {limit}"
        );

        // Past the budget: typed refusal, not an allocation of the amplified size.
        assert_eq!(
            decode(220).map(|_| ()).unwrap_err(),
            IdmlError::BudgetExceeded("model_bytes")
        );
    });
}
