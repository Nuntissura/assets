//! Real public Type caller; fixtures/readset/leases are shared with the owning source tests.
include!("../tests/support/mod.rs");
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--help"] || args.is_empty() {
        println!(
            "type-consumer --execute --case CASE [--verify-reference] [--finalize] [--sink rejected|unavailable|saturated|indeterminate]\nReads original hashed fonts and UTF8 recipes from HSK_TYPE_RESEARCH_ROOT/HSK_TYPE_FONT_ROOT; independent comparison uses the verified external producer artifact; HSK_TYPE_REFERENCE_FILE may relocate those exact bytes. Source-only shaped output; host adoption remains pending."
        );
        return;
    }
    let value = |key: &str| {
        args.iter()
            .position(|a| a == key)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    };
    assert!(args.iter().any(|a| a == "--execute"), "--execute required");
    let name = value("--case").expect("--case required");
    let recipe = json(&research().join("type-exact-oracle-recipe.json"));
    let case = recipe["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["case"] == name)
        .unwrap_or_else(|| panic!("unknown exact original fixture {name}"));
    let fonts = Fonts::load();
    let reference = args
        .iter()
        .any(|a| a == "--verify-reference")
        .then(reference_file);
    let mode = match value("--sink") {
        None => None,
        Some("rejected") => Some(obs::DeliveryError::Rejected),
        Some("unavailable") => Some(obs::DeliveryError::Unavailable),
        Some("saturated") => Some(obs::DeliveryError::Saturated),
        Some("indeterminate") => Some(obs::DeliveryError::Indeterminate),
        Some(_) => panic!("closed sink mode"),
    };
    with_case(&fonts, case, None, |request, ports, scene, ledger, _| {
        let result = prepared(request, ports);
        verify_coverage(&result);
        if let Some(reference) = &reference {
            compare_reference(&result, find_reference(reference, name));
        }
        let runs:Vec<Value>=result.paragraphs().iter().flat_map(|p|p.runs()).map(|r|serde_json::json!({"source":[r.source().start,r.source().end],"direction":format!("{:?}",r.direction()),"level":r.bidi_level(),"font_hash":r.resolved().content_hash.iter().map(|b|format!("{b:02x}")).collect::<String>(),"candidate_index":r.candidate_index(),"glyphs":r.glyphs().iter().map(|g|serde_json::json!({"gid":g.glyph_id,"cluster":[g.cluster.start,g.cluster.end],"points":[g.x_advance_pt,g.y_advance_pt,g.x_offset_pt,g.y_offset_pt]})).collect::<Vec<_>>()})).collect();
        let mut c = result.counts();
        let digest = hash(result.serialized())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let mut diagnostic_delivery: Option<String> = None;
        let (mut disposition, mut error, mut frames) = ("prepared".to_owned(), None, Vec::new());
        if args.iter().any(|a| a == "--finalize") {
            let mut port = final_port(request, ports, scene, ledger, mode);
            let outcome = finalize_with_diagnostics(result, &mut port);
            let inspection = outcome.inspection();
            c = inspection.counts;
            diagnostic_delivery = inspection.diagnostic_delivery.map(|e| format!("{e:?}"));
            disposition = format!("{:?}", inspection.disposition);
            error = inspection.error.map(|e| format!("{e:?}"));
            for i in 0..port.capture.count {
                frames.push(
                    port.capture.frames[i][..port.capture.lengths[i]]
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>(),
                );
            }
            for i in 0..port.diagnostics.count {
                frames.push(
                    port.diagnostics.frames[i][..port.diagnostics.lengths[i]]
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>(),
                );
            }
            drop(outcome);
            drop(port);
        } else {
            drop(result);
        }
        println!(
            "{}",
            serde_json::json!({"scope":"source_only","case":name,"composition":COMPOSITION_VERSION,"serialized_sha256":digest,"disposition":disposition,"error":error,"diagnostic_delivery":diagnostic_delivery,"live_current_after_retirement":ledger.snapshot().unwrap().current_requested_bytes,"counts":{"input_bytes":c.input_bytes,"scalars":c.scalars,"fonts":c.fonts,"runs":c.runs,"glyphs":c.glyphs,"fallback_attempts":c.fallback_attempts,"work":c.work_units,"current":c.current_requested_bytes,"operation_peak":c.operation_peak_requested_bytes,"retained_generations":c.retained_generations},"runs":runs,"observe_frames_hex":frames,"independent_reference_compared":reference.is_some(),"host_adoption":"pending"})
        );
    });
}
