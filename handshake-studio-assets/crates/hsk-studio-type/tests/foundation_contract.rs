include!("support/mod.rs");

#[test]
pub fn font_axis_missing_cancel_stale() {
    bounded_case("font_axis_missing_cancel_stale", || {
        let fonts = Fonts::load();
        let recipe = json(&research().join("type-exact-oracle-recipe.json"));
        let latin = &recipe["cases"][0];
        let variable = &recipe["cases"][4];
        with_case(
            &fonts,
            latin,
            None,
            |request, ports, scene, ledger, resolver| {
                let mut r = request;
                r.text_sha256 = [0; 32];
                reject(r, ports, Error::HashMismatch);
                r = request;
                r.composition_version = "unrecognized";
                reject(r, ports, Error::UnknownComposition);
                r = request;
                r.normalization = Normalization::Unsupported;
                reject(r, ports, Error::UnsupportedNormalization);
                r = request;
                r.text_read_index = 999;
                reject(r, ports, Error::AbsentRead);
                r = request;
                r.cancel_epoch = 2;
                reject(r, ports, Error::StaleEpoch);
                r = request;
                r.limits.input_bytes = 1;
                reject(r, ports, Error::Budget);
                r = request;
                r.limits.work_units = 1;
                reject(r, ports, Error::Budget);
                r = request;
                r.limits.requested_owned_bytes = 1;
                reject(r, ports, Error::LeaseUnavailable);
                r = request;
                r.limits.retained_generations = 0;
                reject(r, ports, Error::LeaseUnavailable);
                resolver.revision.store(2, Ordering::SeqCst);
                reject(request, ports, Error::RevokedFont);
                resolver.revision.store(1, Ordering::SeqCst);
                resolver.mode.store(1, Ordering::SeqCst);
                reject(request, ports, Error::UnavailableFont);
                resolver.mode.store(0, Ordering::SeqCst);
                scene.member.store(0, Ordering::SeqCst);
                reject(request, ports, Error::UnavailableRead);
                scene.member.store(1, Ordering::SeqCst);
                scene.revision.store(2, Ordering::SeqCst);
                reject(request, ports, Error::StaleRead);
                scene.revision.store(1, Ordering::SeqCst);
                ledger.denial.store(1, Ordering::SeqCst);
                ledger.begin();
                reject(request, ports, Error::Budget);
                ledger.denial.store(u64::MAX, Ordering::SeqCst);
                for call in [5, 20] {
                    ledger.begin();
                    ledger.denial.store(call, Ordering::SeqCst);
                    reject(request, ports, Error::Budget);
                    ledger.denial.store(u64::MAX, Ordering::SeqCst);
                }
                // A real post-prepare read conflict refuses before required publication.
                ledger.begin();
                let result = prepared(request, ports);
                let original = hash(request.text_utf8.as_bytes());
                {
                    let _lock = scene.gate.lock().unwrap();
                    scene.revision.store(2, Ordering::SeqCst);
                }
                let mut port = final_port(request, ports, scene, ledger, None);
                let final_result = finalize(result, &mut port);
                assert_eq!(final_result.inspection().error, Some(Error::StaleRead));
                assert_eq!(port.target.commits, 0);
                assert_eq!(&port.target.bytes[..port.target.len], b"original-preimage");
                assert_eq!(port.capture.count, 0);
                assert_eq!(hash(request.text_utf8.as_bytes()), original);
                drop(final_result);
                drop(port);
                scene.revision.store(1, Ordering::SeqCst);
                for mode in [
                    obs::DeliveryError::Rejected,
                    obs::DeliveryError::Unavailable,
                    obs::DeliveryError::Saturated,
                    obs::DeliveryError::Indeterminate,
                ] {
                    ledger.begin();
                    let base = ledger.snapshot().unwrap().current_requested_bytes;
                    let result = prepared(request, ports);
                    let current = ledger.snapshot().unwrap();
                    assert!(current.current_requested_bytes > base);
                    assert_eq!(current.retained_generations, 1);
                    let mut port = final_port(request, ports, scene, ledger, Some(mode));
                    let outcome = finalize(result, &mut port);
                    assert_eq!(port.target.commits, 0);
                    assert_eq!(&port.target.bytes[..port.target.len], b"original-preimage");
                    assert_eq!(outcome.inspection().delivery, Some(mode));
                    if mode == obs::DeliveryError::Indeterminate {
                        let Finalized::ReconciliationRequired { token } = outcome else {
                            panic!("pending ownership required")
                        };
                        assert_eq!(token.proposal().request().text_utf8, request.text_utf8);
                        assert!(ledger.snapshot().unwrap().current_requested_bytes > base);
                        assert_eq!(ledger.snapshot().unwrap().retained_generations, 1);
                        let key = token.key();
                        let outcome = reconcile(token, &mut port);
                        let Finalized::ReconciliationRequired { token } = outcome else {
                            panic!("still pending")
                        };
                        assert_eq!(token.key(), key);
                        assert_eq!(port.target.commits, 0);
                        assert_eq!(&port.target.bytes[..port.target.len], b"original-preimage");
                        port.reconciliation = Reconciliation::Rejected;
                        let rejected = reconcile(token, &mut port);
                        assert_eq!(
                            rejected.inspection().disposition,
                            ProposalDisposition::Rejected
                        );
                        drop(rejected);
                    } else {
                        assert_eq!(
                            outcome.inspection().disposition,
                            ProposalDisposition::Rejected
                        );
                        drop(outcome);
                    }
                    drop(port);
                    assert_eq!(ledger.snapshot().unwrap().current_requested_bytes, base);
                    assert_eq!(ledger.snapshot().unwrap().retained_generations, 0);
                }
                ledger.begin();
                let result = prepared(request, ports);
                let actual_hash = hash(result.serialized());
                let mut port = final_port(request, ports, scene, ledger, None);
                let accepted = finalize(result, &mut port);
                assert_eq!(
                    accepted.inspection().disposition,
                    ProposalDisposition::Accepted
                );
                assert_eq!(port.target.hash, Some(actual_hash));
                let Finalized::Accepted { proposal, .. } = &accepted else {
                    panic!("actual accepted owner")
                };
                assert_eq!(&port.target.bytes[..port.target.len], proposal.serialized());
                assert_eq!(port.capture.count, 1);
                assert_eq!(port.capture.frames[0][4], 5);
                request.cancellation.cancel();
                assert_eq!(
                    accepted.inspection().disposition,
                    ProposalDisposition::Accepted
                );
                drop(accepted);
                drop(port);
                reject(request, ports, Error::Canceled);
            },
        );
        with_case(&fonts, latin, None, |request, ports, scene, ledger, _| {
            let result = prepared(request, ports);
            let mut port = final_port(request, ports, scene, ledger, None);
            port.target.cancel_before_delivery = Some(request.cancellation);
            let outcome = finalize(result, &mut port);
            assert_eq!(
                outcome.inspection().disposition,
                ProposalDisposition::Rejected,
                "a canceled delivery receipt cannot publish"
            );
            assert_eq!(outcome.inspection().error, Some(Error::Canceled));
            assert_eq!(port.target.commits, 0);
            assert_eq!(&port.target.bytes[..port.target.len], b"original-preimage");
            assert_eq!(port.capture.count, 1);
            assert_eq!(port.capture.frames[0][5], 4);
            drop(outcome);
            drop(port);
        });
        for (field, value, error) in [
            ("face_index", serde_json::json!(1), Error::UnavailableFace),
            (
                "bad_font_hash",
                serde_json::json!(true),
                Error::HashMismatch,
            ),
            ("grant_revision", serde_json::json!(2), Error::RevokedFont),
            (
                "requested_identity",
                serde_json::json!("missing-requested"),
                Error::MissingFont,
            ),
        ] {
            let mut case = latin.clone();
            case[field] = value;
            with_case(&fonts, &case, None, |request, ports, _, _, _| {
                reject(request, ports, error)
            });
        }
        let mut corrupt = Fonts {
            data: std::array::from_fn(|i| fonts.data[i].clone()),
            hashes: fonts.hashes,
        };
        corrupt.data[0].truncate(4);
        corrupt.hashes[0] = hash(&corrupt.data[0]);
        with_case(&corrupt, latin, None, |request, ports, _, _, _| {
            reject(request, ports, Error::CorruptFont)
        });
        for axes in [
            serde_json::json!([{"tag":"wght","value":901}]),
            serde_json::json!([{"tag":"xxxx","value":400}]),
        ] {
            let mut case = variable.clone();
            case["axes"] = axes;
            with_case(&fonts, &case, None, |request, ports, _, _, _| {
                reject(request, ports, Error::InvalidAxis)
            });
        }
        let mut case = latin.clone();
        case["features"] = serde_json::json!([{"tag":"xxxx","value":1}]);
        with_case(&fonts, &case, None, |request, ports, _, _, _| {
            reject(request, ports, Error::UnsupportedFeature)
        });
        let mut case = latin.clone();
        case["features"] = serde_json::json!([{"tag":"liga","value":0},{"tag":"liga","value":1}]);
        with_case(&fonts, &case, None, |request, ports, _, _, _| {
            reject(request, ports, Error::UnsupportedFeature)
        });
        let mut inert = latin.clone();
        inert["features"] = serde_json::json!([{"tag":"xxxx","value":1,"declared":false}]);
        with_case(&fonts, &inert, None, |request, ports, _, _, _| {
            let result = prepared(request, ports);
            assert_eq!(
                result.paragraphs()[0].runs()[0].features()[0].state,
                FeatureState::UnavailableInert
            );
        });
        let mut nonfinite = variable.clone();
        nonfinite["axis_nan"] = serde_json::json!(true);
        with_case(&fonts, &nonfinite, None, |request, ports, _, _, _| {
            reject(request, ports, Error::InvalidAxis)
        });
        let mut malformed = recipe["cases"][2].clone();
        malformed["segments"] = serde_json::json!([{"start":1,"end":20,"font_index":1,"script":"Arab","language":"ar"}]);
        with_case(&fonts, &malformed, None, |request, ports, _, _, _| {
            reject(request, ports, Error::InvalidInput)
        });
        for (text, expected) in [
            ("", TextDisposition::EmptyInput),
            ("\u{2067}\u{2069}", TextDisposition::ControlOnly),
        ] {
            let mut case = serde_json::json!({"text":text,"text_sha256":hash(text.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>(),"text_utf8_hex":text.as_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>(),"font_index":0,"script":"Latn","language":"en","direction":"ltr","features":[],"axes":[]});
            if text.is_empty() {
                case["segments"] = serde_json::json!([]);
            }
            with_case(&fonts, &case, None, |request, ports, _, _, _| {
                let result = prepared(request, ports);
                assert_eq!(result.disposition(), expected);
                assert_eq!(result.counts().glyphs, 0);
                verify_coverage(&result);
            });
        }
    });
}

#[test]
pub fn mixed_script_bidi_font_runs() {
    bounded_case("mixed_script_bidi_font_runs", || {
        let fonts = Fonts::load();
        let recipe = json(&research().join("type-exact-oracle-recipe.json"));
        let reference = reference_file();
        assert_eq!(recipe["cases"].as_array().unwrap().len(), 7);
        let mut ligatures = [0; 2];
        let mut variable_hashes = [[0; 32]; 2];
        for (i, case) in recipe["cases"].as_array().unwrap().iter().enumerate() {
            let name = case["case"].as_str().unwrap();
            with_case(&fonts, case, None, |request, ports, _, ledger, _| {
                let base = ledger.snapshot().unwrap().current_requested_bytes;
                let result = prepared(request, ports);
                verify_coverage(&result);
                compare_reference(&result, find_reference(&reference, name));
                assert!(result.counts().glyphs > 0);
                assert!(
                    result.counts().operation_peak_requested_bytes
                        < ledger.snapshot().unwrap().historical_peak_requested_bytes
                );
                assert!(result.counts().current_requested_bytes > base);
                assert_eq!(result.counts().retained_generations, 1);
                if i < 2 {
                    ligatures[i] = result.counts().glyphs;
                }
                if i == 4 || i == 5 {
                    variable_hashes[i - 4] = hash(result.serialized());
                    assert_eq!(result.paragraphs()[0].runs()[0].axes().len(), 2);
                }
                ledger.begin();
                let second = prepared(request, ports);
                assert_eq!(
                    result.serialized(),
                    second.serialized(),
                    "two-run byte determinism {name}"
                );
                assert_eq!(ledger.snapshot().unwrap().retained_generations, 2);
                drop(second);
                assert_eq!(ledger.snapshot().unwrap().retained_generations, 1);
                drop(result);
                assert_eq!(ledger.snapshot().unwrap().current_requested_bytes, base);
            });
        }
        assert_eq!(
            ligatures,
            [13, 13],
            "independent static Inter liga fixtures preserve 13 glyphs"
        );
        assert_ne!(
            variable_hashes[0], variable_hashes[1],
            "actual variable coordinates alter output"
        );
        let arabic = &recipe["cases"][2];
        with_case(
            &fonts,
            arabic,
            Some(&[0, 1]),
            |request, ports, _, ledger, _| {
                let result = prepared(request, ports);
                verify_coverage(&result);
                compare_reference(
                    &result,
                    find_reference(&reference, "arabic_inter_coverage_fallback_index1"),
                );
                let run = &result.paragraphs()[0].runs()[0];
                assert_eq!(run.requested_identity(), NAMES[0]);
                assert_eq!(run.resolved().identity, NAMES[1]);
                assert_eq!(run.candidate_index(), 1);
                assert_eq!(
                    run.substitution(),
                    Some(SubstitutionReason::CoverageFallback)
                );
                assert_eq!(
                    run.source(),
                    SourceRange {
                        start: 0,
                        end: request.text_utf8.len() as u64
                    }
                );
                assert!(result.counts().fallback_attempts > 0);
                drop(result);
                assert_eq!(ledger.snapshot().unwrap().retained_generations, 0);
            },
        );
        let mut missing = arabic.clone();
        missing["requested_identity"] = serde_json::json!(NAMES[0]);
        missing["explicit_substitution"] = serde_json::json!(true);
        with_case(&fonts, &missing, Some(&[1]), |request, ports, _, _, _| {
            let result = prepared(request, ports);
            verify_coverage(&result);
            compare_reference(
                &result,
                find_reference(&reference, "arabic_missing_inter_explicit_substitution"),
            );
            let run = &result.paragraphs()[0].runs()[0];
            assert_eq!(run.requested_identity(), NAMES[0]);
            assert_eq!(run.resolved().identity, NAMES[1]);
            assert_eq!(run.candidate_index(), 0);
            assert_eq!(
                run.substitution(),
                Some(SubstitutionReason::MissingRequested)
            );
        });
        // Official Unicode16 override/isolate golden, translated from scalar to original UTF8.
        let goldens = json(&research().join("unicode16-selected-bidi-goldens.json"));
        let golden = &goldens[0];
        let chars: Vec<char> = golden["codepoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| char::from_u32(u32::from_str_radix(v.as_str().unwrap(), 16).unwrap()).unwrap())
            .collect();
        let text: String = chars.iter().collect();
        let case = serde_json::json!({"case":"official_bidi_line61","text":text,"text_utf8_hex":text.as_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>(),"text_sha256":hash(text.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>(),"font_index":0,"script":"Latn","language":"en","direction":"rtl","features":[],"axes":[]});
        with_case(&fonts, &case, None, |request, ports, _, _, _| {
            let result = prepared(request, ports);
            verify_coverage(&result);
            let offsets: Vec<usize> = request.text_utf8.char_indices().map(|(i, _)| i).collect();
            let mut actual_order = Vec::new();
            for run in result.paragraphs()[0].runs() {
                for g in run.glyphs() {
                    for (i, &offset) in offsets.iter().enumerate() {
                        if offset >= g.cluster.start as usize
                            && offset < g.cluster.end as usize
                            && chars[i].is_ascii_alphabetic()
                        {
                            assert_eq!(
                                run.bidi_level(),
                                golden["levels"][i].as_str().unwrap().parse::<u8>().unwrap()
                            );
                            if actual_order.last() != Some(&i) {
                                actual_order.push(i);
                            }
                        }
                    }
                }
            }
            let expected: Vec<usize> = golden["visual_indices"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i.as_str().unwrap().parse::<usize>().unwrap())
                .filter(|i| chars[*i].is_ascii_alphabetic())
                .collect();
            assert_eq!(
                actual_order, expected,
                "official override/isolate visual order"
            );
        });
    });
}
