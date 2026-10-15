//! Lane SMB-INTEROP-FIG-PROTOTYPE: PNG/JPEG frame export through interop-raster; flow starts,
//! frame overflow and easing tuples as Folio records (no longer losses); the components prototype
//! player stepped headlessly over the Operator prototype fixture with the frame state compared to
//! the Operator PNGs.
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_components::{PlayerEvent, PlayerInput, PlayerSession, cubic_bezier_ease};
use hsk_studio_folio::{Budget, StudioOverflowDirection};
use hsk_studio_interop_fig::*;

fn sid(session: u32, local: u32) -> SourceId {
    SourceId { session, local }
}

fn node_of(p: &NativeProposal, s: SourceId) -> String {
    p.id_map.iter().find(|e| e.source == s).unwrap().node.node_id.clone()
}

fn source_of(p: &NativeProposal, id: &str) -> Option<SourceId> {
    p.id_map.iter().find(|e| e.node.node_id == id).map(|e| e.source)
}

/// (mean, share within 8/255) of an RGB raster vs a PNG of the same size.
fn compare(rgb: &[u8], w: usize, png: &(usize, usize, Vec<u8>)) -> (f64, f64) {
    let mut d = Vec::new();
    for y in 0..png.1 {
        for x in 0..png.0 {
            let i = (y * w + x) * 3;
            let o = (y * png.0 + x) * 4;
            d.push((0..3).map(|c| (f64::from(rgb[i + c]) - f64::from(png.2[o + c])).abs()).fold(0.0, f64::max));
        }
    }
    let n = d.len() as f64;
    (d.iter().sum::<f64>() / n, d.iter().filter(|v| **v <= 8.0).count() as f64 / n)
}

#[test]
fn png_and_jpeg_frame_exports_through_interop_raster() {
    let cancel = CancellationToken::default();
    let doc = document("fig-prototype-pages.fig");
    let p = map(&doc, Budget::default());
    let (plans, _) = plan_frame_exports(&doc, &p, sid(2, 3));
    let png_file = export_frame_raster(&p, &plans[0], None, None, &cancel).unwrap();
    assert_eq!(png_file.file_name, "start-screen.png");
    // The file decodes (interop-raster) to the page the PDF route renders: same pixels.
    let decoded = hsk_studio_interop_raster::decode(&png_file.bytes, &hsk_studio_interop_raster::Limits::default(), &cancel).unwrap();
    let hsk_studio_interop_raster::Samples::U8(rgb) = &decoded.samples else { panic!("8-bit") };
    assert_eq!((decoded.width, decoded.height), (360, 520));
    let page = render_frame_export(&export_frame_pdf(&p, &plans[0], None, &cancel).unwrap(), &cancel).unwrap();
    assert_eq!(rgb, &page.rgb);
    let png = decode_png(&fixture("fig-prototype-pages-start.png"));
    let (mean, within8) = compare(rgb, 360, &png);
    eprintln!("png file: mean {mean:.6} <=8 {within8:.6}");
    assert!(mean <= 0.378 && within8 >= 0.992259, "png file {mean} {within8}");
    // JPEG plan from a setting with quality 0.9 (a fraction of 100).
    let mut jdoc = doc.clone();
    jdoc.nodes.iter_mut().find(|n| n.id == sid(2, 3)).unwrap().export_settings =
        vec![ExportSetting { image_type: Some("JPEG".into()), quality: Some(0.9), ..ExportSetting::default() }];
    let jp = map(&jdoc, Budget::default());
    let (jplans, _) = plan_frame_exports(&jdoc, &jp, sid(2, 3));
    let jpg = export_frame_raster(&jp, &jplans[0], Some(0.9), None, &cancel).unwrap();
    assert_eq!((jpg.file_name.as_str(), &jpg.bytes[..3]), ("start-screen.jpg", &[0xFF, 0xD8, 0xFF][..]));
    let decoded = hsk_studio_interop_raster::decode(&jpg.bytes, &hsk_studio_interop_raster::Limits::default(), &cancel).unwrap();
    let hsk_studio_interop_raster::Samples::U8(rgb) = &decoded.samples else { panic!("8-bit") };
    let (mean, within8) = compare(rgb, 360, &png);
    eprintln!("jpeg file q90: mean {mean:.6} <=8 {within8:.6}");
    // Measured: mean 1.498803/255, 98.8381 % within 8/255 (quality 90 baseline 4:4:4).
    assert!(mean <= 1.4989 && within8 >= 0.98838, "jpeg file {mean} {within8}");
    // A default-quality JPEG is a typed note; a PDF plan is refused here.
    let jpg = export_frame_raster(&jp, &jplans[0], None, None, &cancel).unwrap();
    assert!(jpg.loss.iter().any(|l| l.code == "jpeg_quality_default_used"));
    let mut pdf_plan = plans[0].clone();
    pdf_plan.format = FigExportFormat::Pdf;
    assert_eq!(export_frame_raster(&p, &pdf_plan, None, None, &cancel).unwrap_err(), FigError::ExportRefused("export_plan_is_pdf"));
}

#[test]
fn flow_start_overflow_and_easing_are_records() {
    let doc = document("fig-prototype-pages.fig");
    let p = map(&doc, Budget::default());
    let pages = &p.design.prototype_pages;
    eprintln!("pages {pages:?}");
    eprintln!("overflow {:?}", p.design.frame_overflow);
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].flow_starting_points.len(), 1);
    assert_eq!(source_of(&p, &pages[0].flow_starting_points[0].node_id), Some(sid(2, 3)));
    assert_eq!(p.design.frame_overflow.len(), 1);
    assert_eq!(source_of(&p, &p.design.frame_overflow[0].node.node_id), Some(sid(3, 2)));
    assert_eq!(p.design.frame_overflow[0].overflow_direction, StudioOverflowDirection::Vertical);
    let c = |path: &str| p.mapping_loss.iter().filter(|l| l.path.starts_with(path)).map(|l| l.count).sum::<u32>();
    assert_eq!(c("native.flow_start") + c("native.scroll_direction") + c("native.interactions.easing"), 0);
    // The smart-animate transition carries the Studio EASE_IN_AND_OUT tuple (INOUT_CUBIC).
    let smart = p
        .design
        .nodes
        .iter()
        .flat_map(|d| d.reactions.iter())
        .flat_map(|r| r.actions.iter())
        .find_map(|a| match a {
            hsk_studio_folio::StudioAction::Node { transition: Some(t), .. } => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(smart.easing.easing_function_cubic_bezier, hsk_studio_folio::easing_preset("EASE_IN_AND_OUT"));
    // Folio refuses a flow start named twice on a page and a dangling one.
    let mut bad = p.design.clone();
    let f = bad.prototype_pages[0].flow_starting_points[0].clone();
    bad.prototype_pages[0].flow_starting_points.push(f);
    let err = hsk_studio_folio::validate_design_records(&bad, p.snapshot.document(), Budget::default(), &CancellationToken::default(), &Host);
    assert_eq!(err.unwrap_err().code, hsk_studio_folio::Code::DuplicateIdentity);
    let mut bad = p.design.clone();
    bad.prototype_pages[0].flow_starting_points[0].node_id = "SLYR-019abcde-0000-7000-8000-0000000000ff".into();
    let err = hsk_studio_folio::validate_design_records(&bad, p.snapshot.document(), Budget::default(), &CancellationToken::default(), &Host);
    assert_eq!(err.unwrap_err().code, hsk_studio_folio::Code::DanglingReference);
}

#[test]
fn player_steps_the_prototype_fixture_and_frames_match_the_pngs() {
    let cancel = CancellationToken::default();
    let doc = document("fig-prototype-pages.fig");
    let p = map(&doc, Budget::default());
    let d = p.snapshot.document();
    let (start, detail, animated) = (node_of(&p, sid(2, 3)), node_of(&p, sid(2, 6)), node_of(&p, sid(2, 9)));
    let mut s = PlayerSession::start(d, &p.design, &start).unwrap();
    // The rendered current screen vs the Operator PNG of that screen.
    let check = |screen: &str, png_name: &str| {
        let src = source_of(&p, screen).unwrap();
        let (plans, _) = plan_frame_exports(&doc, &p, src);
        let page = render_frame_export(&export_frame_pdf(&p, &plans[0], None, &cancel).unwrap(), &cancel).unwrap();
        let (mean, within8) = compare(&page.rgb, page.width as usize, &decode_png(&fixture(png_name)));
        eprintln!("{png_name}: mean {mean:.6} <=8 {within8:.6}");
        assert!(mean <= 0.378 && within8 >= 0.99197, "{png_name}");
    };
    check(&s.state().screen, "fig-prototype-pages-start.png");
    // Click on the label "Click: navigate to details" (2:12): NAVIGATE, instant.
    let ev = s.dispatch(&PlayerInput::Click(node_of(&p, sid(2, 12))));
    assert!(ev.contains(&PlayerEvent::Navigated { from: start.clone(), to: detail.clone() }));
    assert_eq!((s.state().screen.clone(), s.state().history_depth), (detail.clone(), 1));
    check(&s.state().screen, "fig-prototype-pages-detail.png");
    // "Click: smart animate" (2:14): 350 ms smart animate, ease in and out.
    s.dispatch(&PlayerInput::Click(node_of(&p, sid(2, 14))));
    s.advance(175);
    let (t, raw, eased) = s.state().transition.unwrap();
    assert_eq!((t.to.as_str(), t.duration_ms, raw), (animated.as_str(), 350, 0.5));
    assert!((eased - 0.5).abs() < 1e-9, "symmetric curve at half time");
    s.advance(70);
    let (_, raw, eased) = s.state().transition.unwrap();
    assert!((raw - 0.7).abs() < 1e-9);
    assert!((eased - cubic_bezier_ease(&t.easing, 0.7)).abs() < 1e-12 && eased > 0.7, "ease-out half");
    let ev = s.advance(105);
    assert!(ev.contains(&PlayerEvent::TransitionEnded { to: animated.clone() }));
    assert!(s.state().transition.is_none());
    check(&s.state().screen, "fig-prototype-pages-animated.png");
    // "Click: back to start" (2:16) is a NAVIGATE to the start screen (history grows).
    s.dispatch(&PlayerInput::Click(node_of(&p, sid(2, 16))));
    assert_eq!((s.state().screen.clone(), s.state().history_depth), (start.clone(), 3));
    check(&s.state().screen, "fig-prototype-pages-start.png");
    // A click with no reaction on the node or its ancestors is a diagnostic, not a state change.
    let ev = s.dispatch(&PlayerInput::Click(start.clone()));
    assert_eq!(ev, [PlayerEvent::Diagnostic("no_reaction_for_trigger")]);
}

#[test]
fn player_overlay_back_close_scroll_hover_and_after_delay() {
    use hsk_studio_folio::{Navigation, StudioAction, StudioReaction, StudioTrigger};
    let doc = document("fig-prototype-pages.fig");
    let mut p = map(&doc, Budget::default());
    let (start, detail, animated) = (node_of(&p, sid(2, 3)), node_of(&p, sid(2, 6)), node_of(&p, sid(2, 9)));
    let node = |dest: &str, navigation: Navigation| StudioAction::Node {
        destination_id: Some(dest.into()),
        navigation,
        transition: None,
        preserve_scroll_position: false,
        overlay_relative_position: None,
        reset_video_position: false,
        reset_scroll_position: false,
        reset_interactive_components: false,
    };
    let on = |trigger: StudioTrigger, actions: Vec<StudioAction>| StudioReaction { trigger: Some(trigger), actions };
    let set = |p: &mut NativeProposal, id: &str, r: Vec<StudioReaction>| {
        match p.design.nodes.iter_mut().find(|d| d.node.node_id == id) {
            Some(d) => d.reactions = r,
            None => p.design.nodes.push(hsk_studio_folio::StudioNodeDesign {
                node: hsk_studio_folio::NodeRef { node_kind: hsk_studio_folio::NodeKind::Layer, node_id: id.to_owned() },
                auto_layout: None,
                child_layout: None,
                size_clamps: None,
                constraints: None,
                reactions: r,
                component_property_references: Default::default(),
                explicit_variable_modes: Default::default(),
                style_refs: Default::default(),
            }),
        }
    };
    // Detail screen: after 800 ms back; its label opens the animated screen as an overlay and
    // the animated label closes it; start label hover shows detail as overlay while hovered.
    set(&mut p, &detail, vec![on(StudioTrigger::AfterTimeout { timeout: 800 }, vec![StudioAction::Back {}])]);
    let (l12, l14, l16) = (node_of(&p, sid(2, 12)), node_of(&p, sid(2, 14)), node_of(&p, sid(2, 16)));
    set(&mut p, &l14, vec![on(StudioTrigger::OnClick {}, vec![node(&animated, Navigation::Overlay)])]);
    set(&mut p, &l16, vec![on(StudioTrigger::OnClick {}, vec![StudioAction::Close {}, node(&start, Navigation::Navigate)])]);
    set(&mut p, &l12, vec![
        on(StudioTrigger::OnClick {}, vec![node(&detail, Navigation::Navigate)]),
        on(StudioTrigger::OnHover {}, vec![node(&detail, Navigation::ScrollTo)]),
    ]);
    let d = p.snapshot.document();
    let mut s = PlayerSession::start(d, &p.design, &start).unwrap();
    // Hover: scroll-to while hovered, restored on leave.
    let label = node_of(&p, sid(2, 12));
    s.dispatch(&PlayerInput::HoverEnter(label.clone()));
    assert_eq!(s.state().scroll_target.as_deref(), Some(detail.as_str()));
    s.dispatch(&PlayerInput::HoverLeave(label.clone()));
    assert_eq!(s.state().screen, start);
    // Navigate, overlay open/close, after-delay back.
    s.dispatch(&PlayerInput::Click(label));
    assert_eq!(s.state().screen, detail);
    s.dispatch(&PlayerInput::Click(node_of(&p, sid(2, 14))));
    assert_eq!(s.state().overlays, std::slice::from_ref(&animated));
    let ev = s.dispatch(&PlayerInput::Click(node_of(&p, sid(2, 16))));
    // CLOSE ends the action array (STU-PRO-104): the navigate after it never runs.
    assert_eq!(ev, [PlayerEvent::OverlayClosed(animated.clone())]);
    assert_eq!((s.state().screen.clone(), s.state().overlays.len()), (detail.clone(), 0));
    assert!(s.advance(799).is_empty());
    let ev = s.advance(1);
    assert!(ev.contains(&PlayerEvent::TimeoutFired { node: detail.clone(), after_ms: 800 }));
    assert_eq!((s.state().screen.clone(), s.state().history_depth), (start.clone(), 0));
    // BACK on an empty history is a diagnostic (STU-PRO-131).
    set(&mut p, &start, vec![on(StudioTrigger::OnClick {}, vec![StudioAction::Back {}])]);
    let mut s = PlayerSession::start(p.snapshot.document(), &p.design, &start).unwrap();
    assert_eq!(s.dispatch(&PlayerInput::Click(start.clone())), [PlayerEvent::Diagnostic("back_on_empty_history")]);
    // Hostile: an unknown start node has no session; a navigation loop stops at MAX_STACK.
    assert!(PlayerSession::start(p.snapshot.document(), &p.design, "SLYR-019abcde-0000-7000-8000-0000000000ff").is_none());
    set(&mut p, &start, vec![on(StudioTrigger::OnClick {}, vec![node(&start, Navigation::Navigate)])]);
    let mut s = PlayerSession::start(p.snapshot.document(), &p.design, &start).unwrap();
    for _ in 0..hsk_studio_components::MAX_STACK {
        s.dispatch(&PlayerInput::Click(start.clone()));
    }
    assert_eq!(s.state().history_depth, hsk_studio_components::MAX_STACK);
    assert_eq!(s.dispatch(&PlayerInput::Click(start.clone())), [PlayerEvent::Diagnostic("history_over_limit")]);
}
