//! Lane SMB-FIG-SMART-ANIMATE: the `.fig` overlay settings (`overlayPositionType`,
//! `overlayBackgroundAppearance`, `overlayBackgroundInteraction`, action `overlayRelativePosition`)
//! become the Folio `frame_overlay` record and the action's `overlay_relative_position`
//! (STU-PRO-122..124) instead of decode losses, `autoRename` is decoded for smart-animate text
//! matching, and the components player places and dismisses overlays from those records. The
//! Operator prototype fixture stores no overlay fields (its census lists none), so the overlay values
//! are set on the decoded nodes of that fixture: the decode of real overlay bytes is NOT_PROVEN here.
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_components::{PlayerEvent, PlayerInput, PlayerSession, overlay_placement};
use hsk_studio_folio::{
    Budget, Code, Navigation, OverlayPosition, StudioAction, StudioOverlayBackground, StudioOverlayBackgroundInteraction,
    StudioOverlayPositionType, validate_design_records,
};
use hsk_studio_interop_fig::*;

fn sid(session: u32, local: u32) -> SourceId {
    SourceId { session, local }
}

fn node_of(p: &NativeProposal, s: SourceId) -> String {
    p.id_map.iter().find(|e| e.source == s).unwrap().node.node_id.clone()
}

fn frame_mut(doc: &mut FigDocument, s: SourceId) -> &mut FramePayload {
    match &mut doc.nodes.iter_mut().find(|n| n.id == s).unwrap().payload {
        NodePayload::Frame(f) => f,
        other => panic!("{other:?}"),
    }
}

fn action_mut(doc: &mut FigDocument, s: SourceId) -> &mut InteractionAction {
    &mut doc.nodes.iter_mut().find(|n| n.id == s).unwrap().interactions[0].actions[0]
}

fn overlay_actions(p: &NativeProposal) -> Vec<(Navigation, Option<OverlayPosition>)> {
    p.design
        .nodes
        .iter()
        .flat_map(|d| d.reactions.iter().flat_map(|r| r.actions.iter()))
        .filter_map(|a| match a {
            StudioAction::Node { navigation: Navigation::Overlay, overlay_relative_position, .. } => {
                Some((Navigation::Overlay, overlay_relative_position.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn overlay_settings_map_to_folio_records_and_drive_the_player() {
    let mut doc = document("fig-prototype-pages.fig");
    // The fixture carries no overlay fields; autoRename is decoded on its text layers.
    let p = map(&doc, Budget::default());
    assert!(p.design.frame_overlay.is_empty());
    let texts: Vec<Option<bool>> = doc
        .nodes
        .iter()
        .filter_map(|n| match &n.payload {
            NodePayload::Text(t) => Some(t.auto_rename),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 14);
    assert!(texts.iter().all(|t| t.is_some()), "{texts:?}");
    assert_eq!(doc.loss.iter().filter(|l| l.path.contains("autoRename")).count(), 0);
    // The animated screen (2:9) becomes a MANUAL overlay with a dimmed background that closes on an
    // outside click; the detail label (2:14) opens it at (12.3449, 40.126) -> precision 2.
    let animated = sid(2, 9);
    let f = frame_mut(&mut doc, animated);
    f.overlay_position_type = Some("MANUAL".into());
    f.overlay_background = Some((Some("SOLID_COLOR".into()), Some(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.25 })));
    f.overlay_background_interaction = Some("CLOSE_ON_CLICK_OUTSIDE".into());
    let a = action_mut(&mut doc, sid(2, 14));
    a.navigation = Some("OVERLAY".into());
    a.transition = None;
    a.overlay_relative_position = Some(Vec2 { x: 12.3449, y: 40.126 });
    let p = map(&doc, Budget::default());
    let o = &p.design.frame_overlay;
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].node.node_id, node_of(&p, animated));
    assert_eq!(o[0].overlay_position_type, Some(StudioOverlayPositionType::Manual));
    let StudioOverlayBackground::SolidColor { color } = &o[0].overlay_background else { panic!("solid") };
    assert_eq!((color.r, color.a), (0.0, 0.25));
    assert_eq!(o[0].overlay_background_interaction, StudioOverlayBackgroundInteraction::CloseOnClickOutside);
    assert_eq!(overlay_actions(&p), [(Navigation::Overlay, Some(OverlayPosition { x: 12.34, y: 40.13 }))]);
    let lost = |p: &NativeProposal, code: &str| p.mapping_loss.iter().filter(|l| l.code == code).map(|l| l.count).sum::<u32>();
    assert_eq!(lost(&p, "overlay_manual_position_missing_origin_used"), 0);
    let cancel = CancellationToken::default();
    validate_design_records(&p.design, p.snapshot.document(), Budget::default(), &cancel, &Host).unwrap();
    // STU-PRO-154: an OVERLAY action opening a MANUAL frame without a position is refused.
    let mut bad = p.design.clone();
    for d in &mut bad.nodes {
        for r in &mut d.reactions {
            for a in &mut r.actions {
                if let StudioAction::Node { overlay_relative_position, .. } = a {
                    *overlay_relative_position = None;
                }
            }
        }
    }
    let err = validate_design_records(&bad, p.snapshot.document(), Budget::default(), &cancel, &Host).unwrap_err();
    assert_eq!(err.code, Code::InvalidField);
    // The player opens the overlay with its position; MANUAL places it there; an outside click closes
    // it because the frame says CLOSE_ON_CLICK_OUTSIDE.
    let d = p.snapshot.document();
    let (start, detail) = (node_of(&p, sid(2, 3)), node_of(&p, sid(2, 6)));
    let mut s = PlayerSession::start(d, &p.design, &start).unwrap();
    s.dispatch(&PlayerInput::Click(node_of(&p, sid(2, 12))));
    assert_eq!(s.state().screen, detail);
    let ev = s.dispatch(&PlayerInput::Click(node_of(&p, sid(2, 14))));
    let over = node_of(&p, animated);
    assert_eq!(ev, [PlayerEvent::OverlayOpened(over.clone())]);
    let st = s.state();
    assert_eq!(st.overlay_positions, [Some(OverlayPosition { x: 12.34, y: 40.13 })]);
    let cfg = s.overlay_config(&over);
    assert_eq!(overlay_placement((360.0, 520.0), (360.0, 520.0), cfg, st.overlay_positions[0].as_ref()), (12.34, 40.13));
    assert_eq!(s.click_outside_overlay(), [PlayerEvent::OverlayClosed(over.clone())]);
    assert!(s.state().overlays.is_empty() && s.state().overlay_positions.is_empty());
    assert_eq!(s.click_outside_overlay(), [PlayerEvent::Diagnostic("no_overlay_open")]);
    // Hand-computed placements of a 200 x 100 overlay over a 360 x 520 screen.
    let mut c = o[0].clone();
    for (kind, want) in [
        (None, (80.0, 210.0)),
        (Some(StudioOverlayPositionType::TopLeft), (0.0, 0.0)),
        (Some(StudioOverlayPositionType::TopRight), (160.0, 0.0)),
        (Some(StudioOverlayPositionType::BottomCenter), (80.0, 420.0)),
        (Some(StudioOverlayPositionType::BottomRight), (160.0, 420.0)),
        (Some(StudioOverlayPositionType::Manual), (80.0, 210.0)),
    ] {
        c.overlay_position_type = kind;
        assert_eq!(overlay_placement((360.0, 520.0), (200.0, 100.0), Some(&c), None), want, "{kind:?}");
    }
    // Hostile tokens are typed losses with the spec fallbacks; a position on a NAVIGATE is dropped.
    let f = frame_mut(&mut doc, animated);
    f.overlay_position_type = Some("SIDEWAYS".into());
    f.overlay_background = Some((Some("SOLID_COLOR".into()), None));
    f.overlay_background_interaction = Some("CLOSE_ON_SNEEZE".into());
    let a = action_mut(&mut doc, sid(2, 14));
    a.navigation = Some("NAVIGATE".into());
    let p = map(&doc, Budget::default());
    let o = &p.design.frame_overlay[0];
    assert_eq!((o.overlay_position_type, o.position_type()), (None, StudioOverlayPositionType::Center));
    assert_eq!(o.overlay_background, StudioOverlayBackground::None {});
    assert_eq!(o.overlay_background_interaction, StudioOverlayBackgroundInteraction::None);
    for code in [
        "overlay_position_unknown_center_used",
        "overlay_background_colour_missing_none_used",
        "overlay_interaction_unknown_none_used",
        "overlay_position_on_non_overlay_navigation",
    ] {
        assert_eq!(lost(&p, code), 1, "{code}");
    }
    assert!(overlay_actions(&p).is_empty());
    // MANUAL without any stored position: origin with a typed loss, so the records stay valid.
    let f = frame_mut(&mut doc, animated);
    f.overlay_position_type = Some("MANUAL".into());
    let a = action_mut(&mut doc, sid(2, 14));
    a.navigation = Some("OVERLAY".into());
    a.overlay_relative_position = None;
    let p = map(&doc, Budget::default());
    assert_eq!(overlay_actions(&p), [(Navigation::Overlay, Some(OverlayPosition { x: 0.0, y: 0.0 }))]);
    assert_eq!(lost(&p, "overlay_manual_position_missing_origin_used"), 1);
    validate_design_records(&p.design, p.snapshot.document(), Budget::default(), &cancel, &Host).unwrap();
}
