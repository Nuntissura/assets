//! Lane SMB-DOC-NEW-1, session half: a new document is written and opened as a package, and one
//! artboard or page edit is one revision, one receipt and one Observe frame, with an inverse.
mod sequence_records_support;
use hsk_studio_accord::{ActorContext, CommandContext};
use hsk_studio_chronicle::Property;
use hsk_studio_folio::*;
use hsk_studio_session::*;
use sequence_records_support::*;
use serde_json::json;

const COMMAND: &str = "artboard.add";

fn blank(artboards: &[(&str, &str)]) -> StudioDocument {
    let len = |value: f64| json!({"value": value, "unit": "px"});
    let boards: Vec<_> = artboards
        .iter()
        .map(|(id, name)| json!({"schema_id": "hsk.studio.artboard@1", "artboard_id": id, "name": name, "parent": null, "bounds": {"x": len(0.0), "y": len(0.0), "width": len(10.0), "height": len(10.0)}}))
        .collect();
    serde_json::from_value(json!({
        "schema_id": "hsk.studio.document@1", "document_id": doc_id(), "revision": 0, "geometry_unit": "px", "colour_mode": 2, "bits_per_channel": 8,
        "working_space_rgb": null, "working_space_cmyk": null, "working_space_gray": null, "working_space_spot": null, "blending_space": null,
        "artboards": boards, "page_spreads": [], "layers": [],
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": [], "edges": [], "outputs": []},
    }))
    .unwrap()
}

fn board(id: &str, name: &str) -> StudioArtboard {
    blank(&[(id, name)]).artboards.remove(0)
}

fn new_session(document: StudioDocument, frames: &Frames) -> Result<Session, Failed> {
    Session::create_new(document, SessionConfig::new(actor()), ports(frames), &token())
}

fn step_to(session: &Session, write: Vec<StudioArtboard>) -> ContainerStep {
    let document = session.snapshot().document();
    ContainerStep { command_id: COMMAND.into(), correlation_id: 7, expect_artboards: document.artboards.clone(), write_artboards: write, expect_spreads: document.page_spreads.clone(), write_spreads: document.page_spreads.clone() }
}

fn ctx(session: &Session, correlation: u64) -> CommandContext {
    context(COMMAND, session.document_revision(), correlation)
}

fn submit(session: &mut Session, step: &ContainerStep) -> Result<Applied, Failed> {
    let (c, revision) = (ctx(session, step.correlation_id), session.document_revision());
    session.submit_containers_as(&c, step, revision, &token())
}

#[test]
fn a_new_document_is_a_dirty_package_with_the_current_token() {
    let frames = Frames::default();
    let session = new_session(blank(&[(identity("SART", 1).as_str(), "first")]), &frames).unwrap();
    assert_eq!(session.document_revision(), 0);
    assert!(session.is_dirty(), "a new document is unsaved");
    assert!(session.origin().is_none());
    assert_eq!(frames.count(), 1, "one Observe frame");
    assert_eq!(session.authored_composition(&token()).unwrap().token, CURRENT_COMPOSITION_TOKEN);
    assert!(session.authored_composition(&token()).unwrap().stored);
    let bad = {
        let mut d = blank(&[]);
        d.colour_mode = 99;
        d
    };
    let frames = Frames::default();
    assert_eq!(new_session(bad, &frames).err().map(|f| f.code()), Some("folio_rejected"));
    let canceled = token();
    canceled.cancel();
    let frames = Frames::default();
    assert!(Session::create_new(blank(&[]), SessionConfig::new(actor()), ports(&frames), &canceled).is_err());
}

#[test]
fn a_container_step_is_one_revision_with_an_inverse_and_a_true_name_revision() {
    let frames = Frames::default();
    let first = identity("SART", 1);
    let second = identity("SART", 2);
    let mut session = new_session(blank(&[(first.as_str(), "first")]), &frames).unwrap();
    let opened = frames.count();
    let original = session.snapshot().document().artboards.clone();
    let mut write = original.clone();
    write.push(board(&second, "second"));
    let step = step_to(&session, write.clone());
    let applied = submit(&mut session, &step).unwrap();
    assert!(applied.changed);
    assert_eq!((applied.previous_revision, applied.revision), (0, 1));
    assert_eq!(applied.provider, CONTAINER_STEP_PROVIDER);
    assert_eq!(frames.count(), opened + 1, "one frame");
    assert_eq!(session.snapshot().document().artboards, write);
    let name_revision = |s: &Session, id: &str| s.revisions().iter().find(|e| e.address.target.node_id == id && e.address.property == Property::Name).map(|e| e.revision);
    assert_eq!(name_revision(&session, &second), Some(1), "a created artboard's name is at the step's revision");
    assert_eq!(name_revision(&session, &first), Some(0));

    // The same lists again: no change, no revision.
    let same = step_to(&session, write.clone());
    let applied = submit(&mut session, &same).unwrap();
    assert!(!applied.changed);
    assert_eq!(session.document_revision(), 1);

    // A rename advances that name's entry only.
    let mut renamed = write.clone();
    renamed[0].name = "again".into();
    let rename = step_to(&session, renamed);
    submit(&mut session, &rename).unwrap();
    assert_eq!(name_revision(&session, &first), Some(2));
    assert_eq!(name_revision(&session, &second), Some(1));

    // Undo restores the exact lists; the entry of a removed artboard stays for the redo.
    let undo = rename.inverse(COMMAND.into(), 9);
    submit(&mut session, &undo).unwrap();
    let undo = step.inverse(COMMAND.into(), 10);
    submit(&mut session, &undo).unwrap();
    assert_eq!(session.snapshot().document().artboards, original);
    assert!(name_revision(&session, &second).is_some());
    let redo = undo.inverse(COMMAND.into(), 11);
    submit(&mut session, &redo).unwrap();
    assert_eq!(session.snapshot().document().artboards, write);
}

#[test]
fn refusals_change_nothing_and_are_typed() {
    let frames = Frames::default();
    let first = identity("SART", 1);
    let mut session = new_session(blank(&[(first.as_str(), "first")]), &frames).unwrap();
    let before = session.snapshot().document().clone();
    let mut write = before.artboards.clone();
    write[0].name = "changed".into();
    let good = step_to(&session, write);

    let mut stale = good.clone();
    stale.expect_artboards[0].name = "not the live one".into();
    assert_eq!(submit(&mut session, &stale).err().map(|f| f.code()), Some("value_conflict"));

    let mut bad_command = good.clone();
    bad_command.command_id = String::new();
    let c = CommandContext::new(actor(), hsk_studio_accord::ClientSessionId::parse("c").unwrap(), None, 0, hsk_studio_accord::CorrelationId::new(5), None);
    assert_eq!(session.submit_containers_as(&c, &bad_command, 0, &token()).err().map(|f| f.code()), Some("invalid_command"));

    let mut other_command = good.clone();
    other_command.command_id = "artboard.other".into();
    let c = ctx(&session, 5);
    assert_eq!(session.submit_containers_as(&c, &other_command, 0, &token()).err().map(|f| f.code()), Some("command_mismatch"));

    let other = ActorContext::new("x", "x", "x", "x", "x", "x").unwrap();
    let c = CommandContext::new(other, hsk_studio_accord::ClientSessionId::parse("c").unwrap(), Some(hsk_studio_accord::CommandId::parse(COMMAND).unwrap()), 0, hsk_studio_accord::CorrelationId::new(5), None);
    assert_eq!(session.submit_containers_as(&c, &good, 0, &token()).err().map(|f| f.code()), Some("actor_mismatch"));

    let mut hostile = good.clone();
    hostile.write_artboards[0].bounds.width.value = f64::NAN;
    assert_eq!(submit(&mut session, &hostile).err().map(|f| f.code()), Some("folio_rejected"));

    let mut duplicate = good.clone();
    duplicate.write_artboards.push(duplicate.write_artboards[0].clone());
    assert_eq!(submit(&mut session, &duplicate).err().map(|f| f.code()), Some("folio_rejected"));

    assert_eq!(session.snapshot().document(), &before);
    assert_eq!(session.document_revision(), 0);
}
