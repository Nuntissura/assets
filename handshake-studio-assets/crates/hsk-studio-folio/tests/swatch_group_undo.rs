//! Lane SMB-R53-FOLLOWUPS-1, STUDIO-R53 F-R53-6: `undo_swatch_group_command` admits a caller-supplied
//! state, so it validates every restored swatch with the palette's swatch rules (id, shape) and the
//! swatch bound before restoring anything; a forged state is refused typed and the palette is not
//! replaced. Names are invented.
use hsk_studio_accord::{ActorContext, ClientSessionId, CommandContext, CommandId, CorrelationId};
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn swatch(n: u32, group: Option<&str>) -> Value {
    let mut v = json!({"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",n),"name":format!("Plimsoll {n}"),"swatch_kind":"process","color_model":"RGB","components":[0.1,0.2,0.3]});
    if let Some(g) = group {
        v["group_ref"] = json!(g);
    }
    v
}
fn records() -> StudioVectorRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "swatches":[swatch(1,None),swatch(2,Some("tide")),swatch(3,Some("tide"))],
        "swatch_groups":[{"group_id":"root","name":"Plimsoll Root","root":true},{"group_id":"tide","name":"Plimsoll Tide"}]}))
    .unwrap()
}
fn ctx(command: &str) -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse(command).unwrap()), 7, CorrelationId::new(41), None)
}
fn undo<T: StudioSwatchContainer>(palette: &T, state: &StudioSwatchPaletteState) -> Result<StudioSwatchGroupOutcome<T>, StudioSwatchGroupError> {
    undo_swatch_group_command(palette, &ctx("swatch.group.undo"), 7, state)
}

#[test]
fn undo_refuses_restored_swatches_that_fail_the_swatch_rules() {
    let r = records();
    let delete = StudioSwatchGroupCommand::Delete { group_id: "tide".into(), replace_with: "root".into() };
    let o = apply_swatch_group_command(&r, &ctx(delete.command_id()), 7, &delete).unwrap();
    // The genuine undo state restores.
    assert_eq!(undo(&o.palette, &o.undo).unwrap().palette, r);

    // A restored process swatch without components (shape), a spot ink with an out-of-range Lab,
    // a swatch whose id is not an SSWT id, a name with a control character.
    let forge = |f: &dyn Fn(&mut StudioSwatch)| {
        let mut state = o.undo.clone();
        f(&mut state.swatches[1]);
        state
    };
    let shapeless = forge(&|s: &mut StudioSwatch| s.components.clear());
    let wrong_id = forge(&|s: &mut StudioSwatch| s.swatch_id = identity("SLYR", 2));
    let control = forge(&|s: &mut StudioSwatch| s.name = "Plimsoll\u{7}".into());
    let spot = forge(&|s: &mut StudioSwatch| {
        *s = serde_json::from_value(json!({"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",2),"name":"Plimsoll Ink","swatch_kind":"spot",
            "spot":{"ink_name":"Plimsoll Ink","measured_lab":[150.0,0.0,0.0]},"group_ref":"tide"}))
        .unwrap()
    });
    for (label, state, code) in [("shapeless", &shapeless, Code::InvalidField), ("wrong id", &wrong_id, Code::InvalidId), ("control", &control, Code::InvalidField), ("spot lab", &spot, Code::InvalidField)] {
        assert_eq!(undo(&o.palette, state).unwrap_err(), StudioSwatchGroupError::InvalidPalette(code), "{label}");
    }

    // The same rules on a host palette.
    let mut app = StudioSwatchPalette::new("plimsoll-app", "Plimsoll Application", StudioPaletteScope::Application);
    app.swatches = o.palette.swatches.clone();
    app.swatch_groups = o.palette.swatch_groups.clone();
    assert_eq!(undo(&app, &shapeless).unwrap_err(), StudioSwatchGroupError::InvalidPalette(Code::InvalidField));
}

#[test]
fn undo_refuses_a_restored_swatch_list_over_the_palette_bound() {
    let r = records();
    let mut state = StudioSwatchPaletteState { swatches: r.swatches.clone(), swatch_groups: r.swatch_groups.clone() };
    let template = r.swatches[0].clone();
    state.swatches = (0..=PALETTE_SWATCHES_MAX as u32)
        .map(|n| StudioSwatch { swatch_id: identity("SSWT", n + 100), ..template.clone() })
        .collect();
    assert_eq!(undo(&r, &state).unwrap_err(), StudioSwatchGroupError::InvalidPalette(Code::BudgetExceeded));
}
