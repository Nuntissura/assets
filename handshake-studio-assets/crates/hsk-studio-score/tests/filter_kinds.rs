//! Lane SMB-SCORE-EFFECTS-6 items 3 and 4: the graphic equalizer sheets as typed rows and the
//! filter-kind table. Oracles: the spec sheets (row counts, slots, mapping of the normalised
//! default), each processor's own `automation_parameters()` and `identity()`, and the score params
//! structs the rows must equal field for field.
mod common_dsp;
use common_dsp::*;
use hsk_studio_score::*;

fn pairs(params: &[FilterKindParam]) -> Vec<(&str, f64)> {
    params.iter().map(|p| (p.key.as_str(), p.value)).collect()
}

#[test]
fn graphic_eq_sheets_are_reproduced_row_for_row() {
    for (effect, count) in [("GraphicEQ10Bands", 13usize), ("GraphicEQ20Bands", 23), ("GraphicEQ30Bands", 33)] {
        let rows = spec_parameter_rows(effect).unwrap_or_else(|| panic!("{effect}"));
        assert_eq!(rows.len(), count, "{effect}");
        let names: Vec<&str> = rows.iter().map(|r| r.parameter).collect();
        assert_eq!(&names[..3], ["accuracy", "gain", "range"]);
        for (i, r) in rows.iter().enumerate() {
            let slot = if i < 3 { i } else { i + 1 };
            assert_eq!(r.slot, format!("in{slot}"), "{effect} {}", r.parameter);
            if i >= 3 {
                assert_eq!(r.parameter, format!("amp{}", i - 2));
            }
            assert_eq!((r.soft_min, r.soft_max, r.unit, r.precision), (None, None, None, None), "STU-FX-136b: absent, not copied");
            let mapped = r.value_of(r.default_normalised.unwrap()).unwrap();
            assert!((mapped - r.default.unwrap()).abs() <= 1e-3, "{effect} {}: {mapped} vs {:?}", r.parameter, r.default);
            assert!(r.mapping.starts_with("value = normalised * ("), "{}", r.mapping);
            // Spec rows: gain and amp bind, accuracy and range are refused as undeclared.
            let bound = bind_spec_row(effect, r.parameter);
            match r.parameter {
                "accuracy" | "range" => assert!(matches!(bound, Err(SpecRowError::Undeclared(_))), "{effect} {}", r.parameter),
                _ => assert!(bound.unwrap().starts_with("GraphicEqParams."), "{effect} {}", r.parameter),
            }
        }
    }
    assert_eq!(spec_parameter_rows("DeEsser"), None);
    let r = &GRAPHIC_EQ_10_BANDS_ROWS[0];
    assert_eq!((r.hard_min, r.hard_max, r.default, r.default_normalised), (Some(10.0), Some(8192.0), Some(999.997454), Some(0.120997)));
}

#[test]
fn every_row_builds_at_its_defaults_and_its_keys_are_the_score_automation_order() {
    bounded_case("filter_kinds_rows", || {
        assert_eq!(FILTER_KINDS.len(), 12);
        for row in &FILTER_KINDS {
            let kind = row.filter_kind;
            let defaults = audio_filter_defaults(kind).unwrap_or_else(|| panic!("{kind}"));
            let set = pairs(&defaults);
            for channels in [1usize, 2] {
                let p = build_audio_filter(kind, &set, RATE, channels).unwrap_or_else(|e| panic!("{kind} x{channels}: {e:?}"));
                assert!(p.identity().starts_with(&format!("{}:", row.identity_tag)), "{kind}: {}", p.identity());
                // The table's automation keys equal the processor's automation names in order.
                let names: Vec<&str> = p.automation_parameters().iter().map(|a| a.name).collect();
                let mut keyed: Vec<(usize, &str)> = defaults.iter().filter_map(|d| d.automation.map(|i| (i, d.key.as_str()))).collect();
                keyed.sort();
                assert_eq!(keyed.iter().map(|(i, _)| *i).collect::<Vec<_>>(), (0..names.len()).collect::<Vec<_>>(), "{kind}");
                for (i, key) in keyed {
                    assert!(key == names[i] || key.ends_with(&format!("_{}", names[i])), "{kind}: key {key} vs automation {}", names[i]);
                }
                // Defaults inside each automatable range.
                for d in defaults.iter().filter(|d| d.automation.is_some()) {
                    let a = p.automation_parameters()[d.automation.unwrap()];
                    assert!((a.min..=a.max).contains(&d.value), "{kind} {}: {} outside {}..{}", d.key, d.value, a.min, a.max);
                }
            }
            if row.identity_tag != kind {
                assert_eq!((kind, row.identity_tag), ("multiband_compressor", "multiband"));
            }
            assert_eq!(audio_filter_parameters(kind, &set).unwrap(), defaults, "{kind}: canonical round trip");
            assert_eq!(audio_filter_kind(kind), Some(row));
        }
        assert_eq!(audio_filter_kind("de_esser"), None);
        assert_eq!(audio_filter_kind("distortion"), None);
    });
}

#[test]
fn only_spec_bound_rows_claim_a_catalogue_row_and_authored_rows_on_empty_rows_carry_an_sd_item() {
    for row in &FILTER_KINDS {
        let claims = row.catalogue_name.is_some() || row.import_key.is_some();
        let bound = matches!(row.binding, BindingClass::SpecTable | BindingClass::SpecTablePartial);
        assert_eq!(claims, bound, "{}", row.filter_kind);
        if let Some(key) = row.import_key {
            assert!(key.starts_with("AudioFilter:"), "{key}");
        }
        if row.sd_item.is_some() {
            assert_eq!(row.parameter_source, ParameterSource::EngineAuthored, "{}", row.filter_kind);
        }
    }
    let sd: Vec<&str> = FILTER_KINDS.iter().filter(|r| r.sd_item.is_some()).map(|r| r.filter_kind).collect();
    assert_eq!(sd, ["feedback_delay", "hum_remover", "multiband_compressor"]);
    let codes: Vec<&str> = FILTER_KINDS.iter().map(|r| r.binding.code()).collect();
    assert!(codes.contains(&"spec_table") && codes.contains(&"spec_table_partial") && codes.contains(&"authored_on_tabled_row") && codes.contains(&"authored_existing"));
    // The rows equal the score params structs field for field (spot checks of the stored forms).
    let keys = |k: &str| audio_filter_defaults(k).unwrap().into_iter().map(|p| p.key).collect::<Vec<_>>();
    assert_eq!(keys("hard_limiter"), ["max_amp_db", "input_boost_db", "lookahead_ms", "release_ms"]);
    assert_eq!(keys("feedback_delay"), ["time_ms", "feedback", "damping_hz", "mix", "ping_pong"]);
    assert_eq!(keys("hum_remover"), ["fundamental_hz", "harmonics", "bandwidth_hz"]);
    let mb = keys("multiband_compressor");
    assert_eq!(&mb[..4], ["bands", "crossover_0_hz", "crossover_1_hz", "band_0_mode"]);
    assert_eq!(mb.len(), 3 + 3 * 12);
    assert_eq!(keys("graphic_eq_30_bands").len(), 33);
}

#[test]
fn hostile_sets_are_refused_typed_before_any_cast_or_allocation() {
    bounded_case("filter_kinds_hostile", || {
        let defaults = |k: &str| audio_filter_defaults(k).unwrap();
        // Whole-number fields: 1e308, -1, 2.5, NaN, infinity refuse.
        for (kind, key) in [("multiband_compressor", "bands"), ("chorus", "voices"), ("phaser", "stages"), ("hum_remover", "harmonics"), ("click_repair", "ar_order"), ("multiband_compressor", "band_1_mode"), ("feedback_delay", "ping_pong")] {
            for (v, want) in [
                (1e308, AudioFilterError::Score(ScoreError::OutOfRange)),
                (-1.0, AudioFilterError::Score(ScoreError::OutOfRange)),
                (2.5, AudioFilterError::NotWholeNumber),
                (f64::NAN, AudioFilterError::Score(ScoreError::NotFinite)),
                (f64::INFINITY, AudioFilterError::Score(ScoreError::NotFinite)),
            ] {
                let mut d = defaults(kind);
                d.iter_mut().find(|p| p.key == key).unwrap_or_else(|| panic!("{kind} {key}")).value = v;
                assert_eq!(build_audio_filter(kind, &pairs(&d), RATE, 2).err(), Some(want), "{kind} {key} = {v}");
                assert_eq!(audio_filter_parameters(kind, &pairs(&d)).err(), Some(want), "{kind} {key} = {v}");
            }
        }
        // Every real parameter of every kind: NaN, infinity and far past its range refuse.
        for row in &FILTER_KINDS {
            let kind = row.filter_kind;
            for (i, param) in defaults(kind).iter().enumerate() {
                for v in [f64::NAN, f64::INFINITY, 1e12] {
                    let mut d = defaults(kind);
                    d[i].value = v;
                    let got = build_audio_filter(kind, &pairs(&d), RATE, 2).err();
                    assert!(got.is_some(), "{kind} {} = {v} built", param.key);
                }
            }
            // Missing, unknown, duplicate keys and an oversized set.
            let d = defaults(kind);
            let mut set = pairs(&d);
            let missing = &set[1..];
            assert_eq!(build_audio_filter(kind, missing, RATE, 1).err(), Some(AudioFilterError::MissingParameter), "{kind}");
            set.push(("flux", 1.0));
            assert_eq!(build_audio_filter(kind, &set, RATE, 1).err(), Some(AudioFilterError::UnknownParameter), "{kind}");
            set.pop();
            set.push((d[0].key.as_str(), d[0].value));
            assert_eq!(build_audio_filter(kind, &set, RATE, 1).err(), Some(AudioFilterError::DuplicateParameter), "{kind}");
            let big: Vec<(&str, f64)> = vec![("x", 0.0); MAX_AUDIO_FILTER_PARAMS + 1];
            assert_eq!(build_audio_filter(kind, &big, RATE, 1).err(), Some(AudioFilterError::TooManyParameters));
            // Rate 0 and 44101 refuse.
            for bad in [0, 44_101] {
                assert_eq!(build_audio_filter(kind, &pairs(&d), bad, 1).err(), Some(AudioFilterError::Score(ScoreError::InvalidRate)), "{kind} at {bad}");
            }
        }
        assert_eq!(build_audio_filter("flux_capacitor", &[], RATE, 1).err(), Some(AudioFilterError::UnknownKind));
        assert_eq!(AudioFilterError::UnknownKind.code(), "audio_filter_unknown_kind");
        assert_eq!(AudioFilterError::Score(ScoreError::OutOfRange).code(), "out_of_range");
        // Sizes from stored values are bounded before allocation: a 5 s delay is the longest line.
        let mut d = defaults("feedback_delay");
        d[0].value = 5000.5;
        assert_eq!(build_audio_filter("feedback_delay", &pairs(&d), RATE, 2).err(), Some(AudioFilterError::Score(ScoreError::OutOfRange)));
        // A multiband set with two bands needs exactly one crossover and two bands of fields.
        let mut two = defaults("multiband_compressor");
        two[0].value = 2.0;
        assert_eq!(build_audio_filter("multiband_compressor", &pairs(&two), RATE, 2).err(), Some(AudioFilterError::UnknownParameter));
        let kept: Vec<(&str, f64)> = pairs(&two).into_iter().filter(|(k, _)| *k != "crossover_1_hz" && !k.starts_with("band_2_")).collect();
        let p = build_audio_filter("multiband_compressor", &kept, RATE, 2).unwrap();
        assert_eq!(p.automation_parameters().len(), 6);
        // Ping-pong needs stereo.
        let mut pp = defaults("feedback_delay");
        pp[4].value = 1.0;
        assert_eq!(build_audio_filter("feedback_delay", &pairs(&pp), RATE, 1).err(), Some(AudioFilterError::Score(ScoreError::InvalidLayout)));
        assert!(build_audio_filter("feedback_delay", &pairs(&pp), RATE, 2).is_ok());
    });
}

#[test]
fn a_built_row_sounds_like_the_processor_built_directly() {
    bounded_case("filter_kinds_equivalence", || {
        let signal = [noise(21, 6000), noise(22, 6000)];
        let stereo = ChannelLayout::stereo();
        let direct: Vec<(&str, Box<dyn Processor>)> = vec![
            ("reverb", Box::new(Reverb::new(RATE, 2, ReverbParams::default()).unwrap())),
            ("flanger", Box::new(Flanger::new(RATE, 2, FlangerParams::default()).unwrap())),
            ("hum_remover", Box::new(HumRemover::new(RATE, 2, HumParams::default()).unwrap())),
            ("multiband_compressor", Box::new(MultibandCompressor::new(RATE, 2, MultibandParams::three_band()).unwrap())),
            ("hard_limiter", Box::new(HardLimiter::new(RATE, 2, HardLimiterParams::default()).unwrap())),
        ];
        for (kind, mut reference) in direct {
            let d = audio_filter_defaults(kind).unwrap();
            let mut built = build_audio_filter(kind, &pairs(&d), RATE, 2).unwrap();
            let a = run_blocks(reference.as_mut(), &stereo, RATE, &signal, &[1000]);
            let b = run_blocks(built.as_mut(), &stereo, RATE, &signal, &[1000]);
            assert_eq!(bits(&a[0]), bits(&b[0]), "{kind}");
            assert_eq!(bits(&a[1]), bits(&b[1]), "{kind}");
        }
    });
}
