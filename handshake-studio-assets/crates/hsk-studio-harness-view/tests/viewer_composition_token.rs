//! Lane SMB-TYPE-AUTHORED-TOKEN-1, brief item 3 (feature `viewer`; STU-TYP-130: a stored document
//! recomposes under the token it was authored with unless it is explicitly upgraded): the Viewer reads
//! the stored token. No token reads as the first; a token this build does not know is refused typed and
//! the file's bytes stay; every path that creates a document records the current token. The pin at the
//! end fails when Folio's copy of the list and Type's constants disagree. Vendor parity UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod support_context;

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;

use common::{Scratch, solid_psd};
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector_text::TEXT_NOT_PROVIDED;
use hsk_studio_folio::{self as folio, ABSENT_COMPOSITION_TOKEN, CURRENT_COMPOSITION_TOKEN, STUDIO_COMPOSITION_TOKENS, StudioCompositionTokenRecord};
use hsk_studio_harness_core::harness::TrustAll;
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::text::DEFAULT_FONT;
use hsk_studio_harness_view::view::vector::path_text::PathTextSpec;
use hsk_studio_harness_view::view::vector::{COMPOSITION_TOKEN_REFUSED, CompositionCause, LiveTextKind, ShapeKind, VectorTool};
use hsk_studio_package::{Extras, Limits, MemoryAssetSource, Quarantined, WriteOptions, read_package, sha256_hex_of, write_package_with};
use hsk_studio_session::{COMPOSITION_TOKEN_ENTRY, COMPOSITION_TOKEN_UNKNOWN, CompositionTokenChange, CompositionTokenStep, DOCUMENT_ENTRY_PREFIX, Session, SessionConfig, SessionPorts};
use hsk_studio_type as ty;
use std::path::{Path, PathBuf};

const W: usize = 96;
const H: usize = 64;
const V1: &str = STUDIO_COMPOSITION_TOKENS[0];
const V2: &str = STUDIO_COMPOSITION_TOKENS[1];
const V3: &str = STUDIO_COMPOSITION_TOKENS[2];
const FUTURE: &str = "hsk-type-shaped-runs@4/future";
const BROKEN: &str = "HH\u{2028}HH";

fn cancel() -> CancellationToken {
    CancellationToken::default()
}

/// White paper; shapes draw no fill and no stroke (only the live text inks). An imported document, so
/// it records the current token.
fn paper(label: &str) -> (Scratch, Viewer) {
    let scratch = Scratch::new(&format!("composition-token-{label}"));
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = support_context::viewer(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: false, stroke_on: false, stroke: [255, 255, 255], ..VectorTool::default() });
    v.text_size_pt = 12.0;
    (scratch, v)
}

/// Pixels of the frame inked by the story fill (the default brush colour over white).
fn inked(v: &mut Viewer) -> Vec<(usize, usize)> {
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {:?}", v.render_error()));
    let mut out = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 4;
            if i32::from(f.rgba8[i]) - i32::from(f.rgba8[i + 1]) > 60 {
                out.push((x, y));
            }
        }
    }
    out
}

fn entry_body(path: &Path) -> Vec<Vec<u8>> {
    let opened = read_package(&std::fs::read(path).unwrap(), &Limits::default(), folio::Budget::default(), &TrustAll, &cancel()).unwrap();
    let prefix = format!("{DOCUMENT_ENTRY_PREFIX}{COMPOSITION_TOKEN_ENTRY}.");
    opened.quarantined.iter().filter(|q| q.name.starts_with(&prefix)).map(|q| q.bytes.clone()).collect()
}

/// `path` rewritten with `body` as its composition token entry (`None` = no entry), everything else as
/// it is: what a file from before this lane, or from a later build, holds.
fn with_entry(path: &Path, body: Option<&[u8]>) {
    let opened = read_package(&std::fs::read(path).unwrap(), &Limits::default(), folio::Budget::default(), &TrustAll, &cancel()).unwrap();
    let prefix = format!("{DOCUMENT_ENTRY_PREFIX}{COMPOSITION_TOKEN_ENTRY}.");
    let mut held: Vec<Quarantined> = opened.quarantined.iter().filter(|q| !q.name.starts_with(&prefix)).cloned().collect();
    if let Some(bytes) = body {
        let sha256_hex = sha256_hex_of(bytes);
        held.push(Quarantined { name: format!("{prefix}{sha256_hex}"), bytes: bytes.to_vec(), sha256_hex });
    }
    held.sort_by(|a, b| a.name.cmp(&b.name));
    let assets = MemoryAssetSource::from_opened(&opened);
    let snapshot = opened.snapshot().unwrap().clone();
    let mut out = Vec::new();
    write_package_with(&snapshot, &assets, &Extras { quarantined: &held, previews: &[] }, &WriteOptions::default(), &Limits::default(), &cancel(), &mut out).unwrap();
    std::fs::write(path, out).unwrap();
}

/// `path` rewritten as authored under `token` (`None` = no token).
fn authored(path: &Path, token: Option<&str>) {
    let document_id = read_package(&std::fs::read(path).unwrap(), &Limits::default(), folio::Budget::default(), &TrustAll, &cancel()).unwrap().snapshot().unwrap().document().document_id.clone();
    let body = token.map(|t| serde_json::to_vec(&StudioCompositionTokenRecord::new(document_id, t)).unwrap());
    with_entry(path, body.as_deref());
}

/// A fresh viewer on `path`, with the extra font.
fn reopen(path: &Path, font: Option<(&str, Vec<u8>)>) -> Viewer {
    let mut v = support_context::viewer(Prefer::Cpu);
    if let Some((family, bytes)) = font {
        v.add_font(family, bytes);
    }
    v.open(path).unwrap();
    v
}

struct NoFrames;
impl hsk_studio_observe::SinkPort for NoFrames {
    fn try_send(&mut self, _: hsk_studio_observe::DeliveryClass, _: &[u8]) -> Result<(), hsk_studio_observe::DeliveryError> {
        Ok(())
    }
}

/// Runs `edit` on `path` through a session and writes the result back: the session commit is the
/// operation under test here, the Viewer only rereads the file.
fn through_session(path: &Path, edit: impl FnOnce(&mut Session)) {
    let actor = support_context::context().actor().clone();
    let ports = SessionPorts::new(Box::new(NoFrames)).with_resolver(Box::new(TrustAll));
    let mut session = Session::open_bytes(&std::fs::read(path).unwrap(), SessionConfig::new(actor), ports, &cancel()).unwrap_or_else(|f| panic!("{f:?}"));
    edit(&mut session);
    std::fs::write(path, session.export_checkpoint(&cancel()).unwrap().bytes).unwrap();
}

/// An area text with a forced line break the current token breaks, saved to `name`, with its ink.
fn saved_area_text(s: &Scratch, v: &mut Viewer, name: &str) -> (PathBuf, Vec<(usize, usize)>) {
    let container = v.draw_shape(ShapeKind::Rectangle, [6.0, 2.0], [90.0, 62.0], false).unwrap();
    v.bind_live_text(&container, BROKEN, LiveTextKind::Area { inset_pt: 2.0 }).unwrap();
    let ink = inked(v);
    assert!(!ink.is_empty(), "the control draws: {:?}", v.vector_text_reports());
    let out = s.0.join(name);
    v.save(&out, false).unwrap();
    (out, ink)
}

/// The bundled font with its GSUB table renamed, so its U+FB01 is reached only by the presentation-form
/// fallback composition v2 added: "fi" is two glyphs under @1 and one under @2 and @3.
fn font_without_gsub() -> Vec<u8> {
    let mut bytes = hsk_studio_harness_view::view::text::FontFile::bundled().unwrap().bytes;
    let tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let at = (0..tables).map(|i| 12 + 16 * i).find(|&at| &bytes[at..at + 4] == b"GSUB").expect("the bundled font has GSUB");
    // Still sorted before GPOS's successor: the directory stays in order for a binary search.
    bytes[at..at + 4].copy_from_slice(b"GSUC");
    bytes
}

#[test]
fn mt_30898_area_text_saved_with_no_token_is_refused_typed_with_no_ink_and_says_the_upgrade_is_the_way_out() {
    let (s, mut v) = paper("absent");
    let (file, ink) = saved_area_text(&s, &mut v, "text.handshake");
    authored(&file, None);

    let mut old = reopen(&file, None);
    let read = old.run().authored_composition().unwrap();
    assert_eq!((read.token, read.stored, read.note), (V1, false, Some(hsk_studio_session::COMPOSITION_TOKEN_ABSENT)));
    assert!(inked(&mut old).is_empty(), "no ink under the token the file reads as");
    assert!(old.render_error().is_none(), "the document still draws: {:?}", old.render_error());
    let reports = old.vector_text_reports().unwrap();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].report.outcome, Err(TEXT_NOT_PROVIDED));
    assert!(reports[0].unshaped.as_deref().is_some_and(|u| u.contains("MissingGlyph")), "{:?}", reports[0].unshaped);
    // The host sees the token is the cause and an upgrade exists, in codes and counters.
    let cause = CompositionCause { code: COMPOSITION_TOKEN_REFUSED, composed_under: Some(1), current: 3, upgrade_available: true };
    assert_eq!(reports[0].composition, Some(cause));
    let state = old.state_json();
    let live = &state["edit"]["vector"]["live_texts"][0];
    assert_eq!(live["code"], TEXT_NOT_PROVIDED, "{live}");
    assert_eq!(live["composition"]["code"], COMPOSITION_TOKEN_REFUSED, "{live}");
    assert_eq!(live["composition"]["composed_under"], 1, "{live}");
    assert_eq!(live["composition"]["current"], 3, "{live}");
    assert_eq!(live["composition"]["upgrade_available"], true, "{live}");

    // Saving never writes the entry on its own.
    let resaved = s.0.join("resaved.handshake");
    old.save(&resaved, false).unwrap();
    assert!(entry_body(&resaved).is_empty(), "a file with no token stays without one");

    // The upgrade creates the entry and the text comes back as it was.
    let mut undo = None;
    through_session(&resaved, |session| {
        let revision = session.document_revision();
        let step = CompositionTokenStep { command_id: "type.composition.upgrade".into(), correlation_id: 1, change: CompositionTokenChange::Upgrade { from: None, to: V3.into() } };
        let applied = session.submit_composition_token(&step, revision, &cancel()).unwrap();
        assert!(!applied.composition_upgrade.as_ref().unwrap().story_ids.is_empty(), "the diff names the story");
        undo = applied.composition_token_inverse;
    });
    assert_eq!(entry_body(&resaved).len(), 1);
    let mut upgraded = reopen(&resaved, None);
    assert_eq!(inked(&mut upgraded), ink, "the upgraded document recomposes under the new token after reopen");
    let reports = upgraded.vector_text_reports().unwrap();
    assert!(reports[0].unshaped.is_none() && reports[0].composition.is_none(), "{reports:?}");

    // Undo removes the entry and the refusal is back.
    through_session(&resaved, |session| {
        let revision = session.document_revision();
        session.submit_composition_token(&undo.take().unwrap(), revision, &cancel()).unwrap();
    });
    assert!(entry_body(&resaved).is_empty());
    let mut undone = reopen(&resaved, None);
    assert!(inked(&mut undone).is_empty());
    assert!(undone.vector_text_reports().unwrap()[0].composition.is_some());
}

#[test]
fn mt_30898_a_document_composes_under_the_token_stored_with_it() {
    // A forced line break: @1 and @2 shape it (no glyph: refused), @3 keeps it out.
    let (s, mut v) = paper("tokens");
    let (file, ink) = saved_area_text(&s, &mut v, "text.handshake");
    for (token, draws) in [(V1, false), (V2, false), (V3, true)] {
        authored(&file, Some(token));
        let mut again = reopen(&file, None);
        let read = again.run().authored_composition().unwrap();
        assert_eq!((read.token, read.stored, read.note), (token, true, None));
        assert_eq!(inked(&mut again), if draws { ink.clone() } else { Vec::new() }, "{token}");
        let cause = again.vector_text_reports().unwrap()[0].composition;
        let ordinal = STUDIO_COMPOSITION_TOKENS.iter().position(|t| *t == token).unwrap() + 1;
        assert_eq!(cause, (!draws).then_some(CompositionCause { code: COMPOSITION_TOKEN_REFUSED, composed_under: Some(ordinal), current: 3, upgrade_available: true }), "{token}");
    }

    // The presentation-form ligature: @1 has none, @2 and @3 do, as Type's fixtures give for each token.
    let font = font_without_gsub();
    let (s, mut v) = paper("ligature");
    v.add_font("NoGsub", font.clone());
    let line = v.pen_point(6.0, 40.0, None, 1.0).unwrap();
    v.pen_point(90.0, 40.0, None, 1.0).unwrap();
    v.pen_end();
    v.bind_live_text_with_font(&line, "fi", LiveTextKind::Path, "NoGsub").unwrap();
    let file = s.0.join("ligature.handshake");
    v.save(&file, false).unwrap();
    for (token, placed) in [(None, 2), (Some(V1), 2), (Some(V2), 1), (Some(V3), 1)] {
        authored(&file, token);
        let again = reopen(&file, Some(("NoGsub", font.clone())));
        let reports = again.vector_text_reports().unwrap();
        assert_eq!(reports[0].report.outcome.as_ref().map(|d| d.placed), Ok(placed), "{token:?}: {reports:?}");
    }

    // The path-text layout of the vector tool reads the token too.
    authored(&file, None);
    let mut again = reopen(&file, Some(("NoGsub", font)));
    let spec = PathTextSpec { text: "ab\u{2028}cd".into(), family: DEFAULT_FONT.into(), size_pt: 12.0, options: Default::default() };
    let refused = again.path_text_layout(&line, &spec).unwrap_err();
    assert!(refused.contains("MissingGlyph"), "{refused}");
    again.set_vector_tool(VectorTool::default());
}

#[test]
fn mt_30898_a_token_this_build_does_not_know_is_refused_typed_and_its_bytes_stay() {
    let (s, mut v) = paper("unknown");
    let (file, _) = saved_area_text(&s, &mut v, "text.handshake");
    let document_id = read_package(&std::fs::read(&file).unwrap(), &Limits::default(), folio::Budget::default(), &TrustAll, &cancel()).unwrap().snapshot().unwrap().document().document_id.clone();
    let body = serde_json::to_vec(&StudioCompositionTokenRecord::new(document_id, FUTURE)).unwrap();
    with_entry(&file, Some(&body));
    let mut later = reopen(&file, None);
    assert_eq!(later.run().authored_composition().unwrap_err(), COMPOSITION_TOKEN_UNKNOWN);
    assert!(inked(&mut later).is_empty(), "never the newest token");
    let reports = later.vector_text_reports().unwrap();
    assert_eq!(reports[0].report.outcome, Err(TEXT_NOT_PROVIDED));
    assert!(reports[0].unshaped.as_deref().is_some_and(|u| u.contains(COMPOSITION_TOKEN_UNKNOWN)), "{:?}", reports[0].unshaped);
    assert_eq!(reports[0].composition, Some(CompositionCause { code: COMPOSITION_TOKEN_UNKNOWN, composed_under: None, current: 3, upgrade_available: false }));
    let resaved = s.0.join("resaved.handshake");
    later.save(&resaved, false).unwrap();
    assert_eq!(entry_body(&resaved), [body], "save keeps the bytes");

    // A text layer under an unknown token: refused at the render, typed, not drawn under another token.
    let (s, mut v) = paper("unknown-layer");
    v.place_text(10.0, 20.0, "abcd").unwrap();
    let file = s.0.join("layer.handshake");
    v.save(&file, false).unwrap();
    let document_id = read_package(&std::fs::read(&file).unwrap(), &Limits::default(), folio::Budget::default(), &TrustAll, &cancel()).unwrap().snapshot().unwrap().document().document_id.clone();
    with_entry(&file, Some(&serde_json::to_vec(&StudioCompositionTokenRecord::new(document_id, FUTURE)).unwrap()));
    let mut later = reopen(&file, None);
    later.render_if_dirty();
    let error = format!("{:?}", later.render_error());
    assert!(error.contains(COMPOSITION_TOKEN_UNKNOWN), "{error}");
}

#[test]
fn mt_30898_every_path_that_creates_a_document_records_the_current_token() {
    // An import through the harness (PSD here; every format goes through ImportedDocument::to_package).
    let (s, mut v) = paper("created");
    let read = v.run().authored_composition().unwrap();
    assert_eq!((read.token, read.stored, read.note), (CURRENT_COMPOSITION_TOKEN, true, None));
    let out = s.0.join("saved.handshake");
    v.save(&out, false).unwrap();
    assert_eq!(entry_body(&out).len(), 1, "and a save keeps it");
    // A design file.
    let fixture = format!("{}/../hsk-studio-interop-fig/tests/fixtures/fig-components-autolayout.fig", env!("CARGO_MANIFEST_DIR"));
    let mut v = support_context::viewer(Prefer::Cpu);
    v.import_fig(Path::new(&fixture), &s.0.join("design.handshake"), false).unwrap();
    assert_eq!(v.run().authored_composition().unwrap().token, CURRENT_COMPOSITION_TOKEN);
    assert_eq!(entry_body(&s.0.join("design.handshake")).len(), 1);
    // An Illustrator file.
    use hsk_studio_press::{Colour, ExportOptions, FillRule, Limits as PressLimits, PageSpec, PathCommand, PdfExport};
    let mut ex = PdfExport::new(ExportOptions { info_title: Some("AIPrivateData Illustrator fixture"), ..ExportOptions::default() }, &PressLimits::default()).unwrap();
    let p = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 150.0, 75.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    let rect = [PathCommand::MoveTo(6.0, 6.0), PathCommand::LineTo(30.0, 6.0), PathCommand::LineTo(30.0, 24.0), PathCommand::Close];
    ex.fill_path(p, &rect, FillRule::NonZero, Colour::DeviceRgb([1.0, 0.0, 0.0])).unwrap();
    let source = s.0.join("art.ai");
    std::fs::write(&source, ex.finish(&cancel()).unwrap().0).unwrap();
    let mut v = support_context::viewer(Prefer::Cpu);
    v.import_ai(&source, &s.0.join("art.handshake"), false).unwrap();
    assert_eq!(v.run().authored_composition().unwrap().token, CURRENT_COMPOSITION_TOKEN);
    assert_eq!(entry_body(&s.0.join("art.handshake")).len(), 1);
}

/// The pin of Folio's copy of the list (lead's condition): it fails when Type gains a token, because the
/// current constant is no longer the list's last, and when any older constant moves.
#[test]
fn mt_30898_folios_token_list_is_types_constants_in_order() {
    assert_eq!(STUDIO_COMPOSITION_TOKENS[STUDIO_COMPOSITION_TOKENS.len() - 1], ty::COMPOSITION_VERSION, "the last is the current token");
    assert_eq!(CURRENT_COMPOSITION_TOKEN, ty::COMPOSITION_VERSION);
    assert_eq!(STUDIO_COMPOSITION_TOKENS[0], ty::COMPOSITION_VERSION_V1);
    assert_eq!(STUDIO_COMPOSITION_TOKENS[1], ty::COMPOSITION_VERSION_V2);
    assert_eq!(STUDIO_COMPOSITION_TOKENS.len(), 3, "a new token gets a named constant here");
    assert_eq!(ABSENT_COMPOSITION_TOKEN, ty::COMPOSITION_VERSION_V1);
}
