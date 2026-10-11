//! SMB-TYPE-ENG-1 spike (feature `harfrust`, Operator decision D7 pending): shapes the pinned
//! fixtures through the vendored provider and HarfRust 0.14.0 and diffs glyph ids, clusters and
//! advance/offset point bits against the pinned reference (sha256 6579f9a9). Vendored parity is
//! asserted; HarfRust parity is reported (one JSON line per case), not asserted.
#![cfg(feature = "harfrust")]
#![allow(dead_code)]
include!("support/mod.rs");

fn diff(result: &Prepared<'_>, reference: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let actual: Vec<&Run<'_>> = result
        .paragraphs()
        .iter()
        .flat_map(|p| p.runs())
        .filter(|r| !r.glyphs().is_empty())
        .collect();
    let expected: Vec<&Value> = reference["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| !r["glyphs"].as_array().unwrap().is_empty())
        .collect();
    if actual.len() != expected.len() {
        out.push(format!(
            "visible runs {} != reference {}",
            actual.len(),
            expected.len()
        ));
    }
    for (ri, (run, r)) in actual.iter().zip(&expected).enumerate() {
        let eg = r["glyphs"].as_array().unwrap();
        if run.glyphs().len() != eg.len() {
            out.push(format!(
                "run{ri} glyphs {} != reference {}",
                run.glyphs().len(),
                eg.len()
            ));
        }
        for (gi, (g, e)) in run.glyphs().iter().zip(eg).enumerate() {
            if u64::from(g.glyph_id) != e["glyph_id"].as_u64().unwrap() {
                out.push(format!("run{ri}.g{gi} id {} != {}", g.glyph_id, e["glyph_id"]));
            }
            let cluster = (g.cluster.start, g.cluster.end);
            let expected_cluster = (
                e["cluster_start"].as_u64().unwrap(),
                e["cluster_end"].as_u64().unwrap(),
            );
            if cluster != expected_cluster {
                out.push(format!(
                    "run{ri}.g{gi} cluster {cluster:?} != {expected_cluster:?}"
                ));
            }
            let bits = [g.x_advance_pt, g.y_advance_pt, g.x_offset_pt, g.y_offset_pt]
                .map(|v| format!("{:016x}", v.to_bits()));
            let expected_bits: Vec<&str> = e["point_bits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if bits.iter().map(String::as_str).collect::<Vec<_>>() != expected_bits {
                out.push(format!(
                    "run{ri}.g{gi} pt adv/off {:?} != reference {:?}",
                    [g.x_advance_pt, g.y_advance_pt, g.x_offset_pt, g.y_offset_pt],
                    [
                        &e["x_advance_pt"],
                        &e["y_advance_pt"],
                        &e["x_offset_pt"],
                        &e["y_offset_pt"]
                    ]
                ));
            }
        }
    }
    out
}

fn outcome(result: &Result<Prepared<'_>, Rejected>, reference: Option<&Value>) -> Value {
    match result {
        Ok(prepared) => {
            let diffs = reference.map(|r| diff(prepared, r)).unwrap_or_default();
            serde_json::json!({
                "status": "shaped",
                "glyphs": prepared.counts().glyphs,
                "runs": prepared.paragraphs().iter().map(|p| p.runs().len()).sum::<usize>(),
                "work_units": prepared.counts().work_units,
                "operation_peak_requested_bytes": prepared.counts().operation_peak_requested_bytes,
                "diff_count": diffs.len(),
                "diff_samples": diffs.iter().take(6).collect::<Vec<_>>(),
            })
        }
        Err(rejected) => serde_json::json!({
            "status": format!("refused {:?}", rejected.error),
            "source": format!("{:?}", rejected.source),
            "work_units": rejected.counts.work_units,
        }),
    }
}

fn compare_case<const N: usize>(
    fonts: &Fonts<N>,
    case: &Value,
    candidates: Option<&[usize]>,
    reference: Option<&Value>,
    vendored_parity_required: bool,
) -> (Value, Value) {
    let mut report = (Value::Null, Value::Null);
    with_case(fonts, case, candidates, |request, ports, _, _, _| {
        let vendored = TextEngine.shaped_runs(request, ports);
        report.0 = outcome(&vendored, reference);
        if vendored_parity_required {
            let prepared = vendored.as_ref().unwrap_or_else(|r| {
                panic!("vendored provider refused {:?}", r.error)
            });
            verify_coverage(prepared);
            assert!(diff(prepared, reference.unwrap()).is_empty(), "vendored parity");
        }
        drop(vendored);
        let harfrust = TextEngine.shaped_runs_harfrust(request, ports);
        if let Ok(prepared) = &harfrust {
            verify_coverage(prepared);
        }
        report.1 = outcome(&harfrust, reference);
    });
    report
}

#[test]
pub fn harfrust_spike_parity_and_coverage() {
    bounded_case("harfrust_spike_parity_and_coverage", || {
        let fonts = Fonts::load_with_coverage_scripts();
        let recipe = json(&research().join("type-exact-oracle-recipe.json"));
        let reference = reference_file();
        let mut lines = Vec::new();
        for case in recipe["cases"].as_array().unwrap() {
            let name = case["case"].as_str().unwrap();
            let expected = find_reference(&reference, name);
            let (v, h) = compare_case(&fonts, case, None, Some(expected), true);
            lines.push(serde_json::json!({"case": name, "vendored": v, "harfrust": h}));
        }
        for name in ["latin_dlig_0", "latin_dlig_1"] {
            let expected = find_reference(&reference, name);
            let (v, h) = compare_case(&fonts, &expected["options"], None, Some(expected), true);
            lines.push(serde_json::json!({"case": name, "vendored": v, "harfrust": h}));
        }
        let arabic = &recipe["cases"][2];
        let name = "arabic_inter_coverage_fallback_index1";
        let (v, h) = compare_case(
            &fonts,
            arabic,
            Some(&[0, 1]),
            Some(find_reference(&reference, name)),
            true,
        );
        lines.push(serde_json::json!({"case": name, "vendored": v, "harfrust": h}));
        let mut missing = arabic.clone();
        missing["requested_identity"] = serde_json::json!(NAMES[0]);
        missing["explicit_substitution"] = serde_json::json!(true);
        let name = "arabic_missing_inter_explicit_substitution";
        let (v, h) = compare_case(
            &fonts,
            &missing,
            Some(&[1]),
            Some(find_reference(&reference, name)),
            true,
        );
        lines.push(serde_json::json!({"case": name, "vendored": v, "harfrust": h}));
        // Coverage gain: scripts the vendored subset refuses (no pinned reference exists).
        for (name, text, font, script, language, direction) in [
            ("devanagari_hindi", "\u{939}\u{93f}\u{928}\u{94d}\u{926}\u{940}", 4, "Deva", "hi", "ltr"),
            ("hebrew_shalom_points", "\u{5e9}\u{5c1}\u{5b8}\u{5dc}\u{5d5}\u{5b9}\u{5dd}", 5, "Hebr", "he", "rtl"),
        ] {
            let case = serde_json::json!({
                "case": name, "text": text, "font_index": font, "script": script,
                "language": language, "direction": direction, "features": [], "axes": [],
                "text_sha256": hash(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "text_utf8_hex": text.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>(),
            });
            let (v, h) = compare_case(&fonts, &case, None, None, false);
            assert_eq!(v["status"], "refused UnsupportedProfile", "{name} vendored");
            assert_eq!(h["status"], "shaped", "{name} harfrust");
            assert!(h["glyphs"].as_u64().unwrap() > 0, "{name} harfrust glyphs");
            lines.push(serde_json::json!({"case": name, "vendored": v, "harfrust": h}));
        }
        for line in &lines {
            println!("SPIKE {line}");
        }
    });
}
