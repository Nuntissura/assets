//! Lane SMB-FIG-SMART-ANIMATE-2: the `.fig` directional transition tokens (the file's own Kiwi
//! `TransitionType` enum lists `SLIDE_FROM_*`, `PUSH_FROM_*`, `MOVE_FROM_*`, `SLIDE_OUT_TO_*`,
//! `MOVE_OUT_TO_*`) map to the STU-PRO-118 type + direction with `transitionShouldSmartAnimate` as
//! `match_layers`; `CUSTOM_SPRING` maps to the STU-PRO-119 spring record (parameter order read as the
//! plugin API's {mass, stiffness, damping, initialVelocity}: UNVERIFIED for .fig, typed note);
//! unknown tokens, spring presets without published numbers and out-of-range springs stay typed
//! losses. The Operator fixture holds no directional or spring action, so the values are set on the
//! decoded action of that fixture (decode of real directional bytes: NOT_PROVEN).
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, SpringEasing, StudioAction, StudioTransition, TransitionDirection as D, TransitionType as T,
    validate_design_records,
};
use hsk_studio_interop_fig::*;

fn transition(p: &NativeProposal) -> Option<StudioTransition> {
    p.design
        .nodes
        .iter()
        .flat_map(|d| d.reactions.iter().flat_map(|r| r.actions.iter()))
        .find_map(|a| match a {
            StudioAction::Node { transition: Some(t), .. } => Some(t.clone()),
            _ => None,
        })
}

fn lost(p: &NativeProposal, code: &str) -> u32 {
    p.mapping_loss.iter().filter(|l| l.code == code).map(|l| l.count).sum()
}

#[test]
fn directional_tokens_and_custom_spring_map_to_folio_transitions() {
    let base = document("fig-prototype-pages.fig");
    // The file's schema carries every directional token this mapping reads.
    let cancel = CancellationToken::default();
    let FigOutcome::Imported(import) =
        import_fig(&fixture("fig-prototype-pages.fig"), &Limits::default(), &VersionPolicy::observed(), None, &cancel).unwrap()
    else {
        panic!("imports")
    };
    let enum_members: Vec<String> = import
        .schema_summary
        .unwrap()
        .definitions
        .iter()
        .find(|d| d.name == "TransitionType")
        .unwrap()
        .fields
        .iter()
        .map(|f| f.name.clone())
        .collect();
    let label = base.nodes.iter().position(|n| n.id == SourceId { session: 2, local: 14 }).unwrap();
    let with = |f: &dyn Fn(&mut InteractionAction)| {
        let mut doc = base.clone();
        f(&mut doc.nodes[label].interactions[0].actions[0]);
        map(&doc, Budget::default())
    };
    let sides = [("LEFT", D::Left), ("RIGHT", D::Right), ("TOP", D::Top), ("BOTTOM", D::Bottom)];
    let kinds = [("MOVE_FROM_", T::MoveIn), ("PUSH_FROM_", T::Push), ("SLIDE_FROM_", T::SlideIn), ("MOVE_OUT_TO_", T::MoveOut), ("SLIDE_OUT_TO_", T::SlideOut)];
    for (prefix, ty) in kinds {
        for (side, dir) in sides {
            let token = format!("{prefix}{side}");
            assert!(enum_members.contains(&token), "{token} in the file schema");
            let smart = token == "PUSH_FROM_LEFT";
            let p = with(&|a| {
                a.transition = Some(token.clone());
                a.smart_animate = smart.then_some(true);
            });
            let t = transition(&p).unwrap();
            assert_eq!((t.transition_type, t.direction, t.match_layers), (ty, Some(dir), Some(smart)), "{token}");
            assert_eq!(t.duration, 350);
            validate_design_records(&p.design, p.snapshot.document(), Budget::default(), &cancel, &Host).unwrap();
        }
    }
    // FADE is mapped as DISSOLVE with a note; an unknown token stays a typed loss.
    let p = with(&|a| a.transition = Some("FADE".into()));
    assert_eq!((transition(&p).unwrap().transition_type, lost(&p, "fade_mapped_as_dissolve")), (T::Dissolve, 1));
    let p = with(&|a| a.transition = Some("MOVE_FROM_NOWHERE".into()));
    assert!(transition(&p).is_none());
    assert_eq!(lost(&p, "transition_not_mapped"), 1);
    // CUSTOM_SPRING: plugin-API order, 3 values = zero initial velocity; out of range -> linear loss.
    let p = with(&|a| {
        a.easing = Some("CUSTOM_SPRING".into());
        a.easing_function = vec![1.0, 100.0, 20.0, 2.5];
    });
    let e = transition(&p).unwrap().easing;
    assert_eq!(e.easing_function_spring, Some(SpringEasing { mass: 1.0, stiffness: 100.0, damping: 20.0, initial_velocity: 2.5 }));
    assert!(e.easing_function_cubic_bezier.is_none());
    assert_eq!(lost(&p, "spring_parameter_order_unverified"), 1);
    validate_design_records(&p.design, p.snapshot.document(), Budget::default(), &cancel, &Host).unwrap();
    let p = with(&|a| {
        a.easing = Some("CUSTOM_SPRING".into());
        a.easing_function = vec![2.0, 300.0, 15.0];
    });
    assert_eq!(transition(&p).unwrap().easing.easing_function_spring.unwrap().initial_velocity, 0.0);
    for bad in [vec![0.0, 100.0, 20.0], vec![1.0, 100.0, -1.0], vec![1.0, 100.0]] {
        let p = with(&|a| {
            a.easing = Some("CUSTOM_SPRING".into());
            a.easing_function = bad.clone();
        });
        let e = transition(&p).unwrap().easing;
        assert!(e.easing_function_spring.is_none() && e.easing_function_cubic_bezier.is_some(), "{bad:?}");
        assert_eq!(lost(&p, "easing_without_studio_tuple_linear_used"), 1);
    }
    // Spring presets have no published numbers: typed loss, linear.
    let p = with(&|a| a.easing = Some("GENTLE_SPRING".into()));
    assert_eq!(lost(&p, "easing_without_studio_tuple_linear_used"), 1);
}
