use handshake_dexterity::*;
use std::{collections::BTreeMap, ffi::OsString};

fn qairt() -> EngineProfile {
    EngineProfile {
        id: "qwen3-06b-qairt".into(), revision: 1,
        model: Model { id: "qwen3-06b".into(), format: ModelFormat::QairtW4a16,
            content_sha256: "a".repeat(64), compiled_context: Some(4096) },
        engine: Engine::GenieXQairt, device: Device::Npu,
        preset: Preset { name: "deterministic".into(), context_limit: 4096, max_tokens: 64,
            sampling: Sampling::Qairt { temperature: 0.0, seed: 42 } },
    }
}

fn cpu() -> EngineProfile {
    EngineProfile {
        id: "qwen3-4b-abliterated-llama-cpu".into(), revision: 1,
        model: Model { id: "qwen3-4b-abliterated".into(), format: ModelFormat::GgufQ4_0,
            content_sha256: "b".repeat(64), compiled_context: None },
        engine: Engine::GenieXLlamaCpp, device: Device::Cpu,
        preset: Preset { name: "writing".into(), context_limit: 2048, max_tokens: 256,
            sampling: Sampling::LlamaCpp { temperature: 0.6, top_k: 20, top_p: 0.95, min_p: 0.0 } },
    }
}

fn binding(profile: &EngineProfile) -> HostBinding {
    let root = std::env::temp_dir().join("dexterity-host-input-only");
    HostBinding {
        executable: root.join("geniex.exe"), cwd: root.clone(), data: root.join("data"),
        model_selector: match profile.engine {
            Engine::GenieXQairt => "local/Qwen3-0.6B",
            Engine::GenieXLlamaCpp => "local/Qwen3-4B-abliterated:Q4_0",
        }.into(),
        content_sha256: profile.model.content_sha256.clone(),
        capabilities: vec![RuntimeCapability { engine: profile.engine, device: profile.device }],
        environment: BTreeMap::from([("PRIVATE_VALUE".into(), "not-for-debug".into())]),
    }
}

#[test]
fn exact_two_requested_profiles_keep_explicit_compute_and_full_snapshot() {
    for profile in [qairt(), cpu()] {
        let resolved = Registry::new(vec![profile.clone()]).unwrap().resolve(&profile.id).unwrap();
        let plan = resolved.plan(binding(&profile), "hello").unwrap();
        let args = plan.arguments();
        let compute = args.iter().position(|arg| arg == "--compute").unwrap();
        assert_eq!(args[compute + 1], OsString::from(if profile.device == Device::Cpu { "cpu" } else { "npu" }));
        assert_eq!(plan.resolved().profile(), &profile);
        assert_eq!(plan.resolved().fingerprint(), resolved.fingerprint());
        assert!(!args.iter().any(|arg| arg == "auto"));
    }
}

#[test]
fn profile_fingerprint_covers_settings_and_revision_and_snapshot_is_owned() {
    let original = cpu();
    let registry = Registry::new(vec![original.clone()]).unwrap();
    let baseline = registry.resolve(&original.id).unwrap();
    let mut changed = original.clone();
    changed.preset.max_tokens += 1;
    let modified = Registry::new(vec![changed.clone()]).unwrap().resolve(&changed.id).unwrap();
    assert_ne!(baseline.fingerprint(), modified.fingerprint());
    changed.revision += 1;
    let revised = Registry::new(vec![changed.clone()]).unwrap().resolve(&changed.id).unwrap();
    assert_ne!(modified.fingerprint(), revised.fingerprint());
    assert_eq!(registry.resolve(&original.id).unwrap().profile(), &original);
    let round_trip = Registry::from_json(&serde_json::to_vec(&vec![original.clone()]).unwrap()).unwrap();
    assert_eq!(round_trip.resolve(&original.id).unwrap().fingerprint(), baseline.fingerprint());
}

#[test]
fn duplicates_missing_unknown_fields_and_unknown_enums_reject() {
    let profile = cpu();
    assert!(matches!(Registry::new(vec![profile.clone(), profile.clone()]), Err(Error::DuplicateProfile)));
    assert!(matches!(Registry::new(vec![]).unwrap().resolve("missing"), Err(Error::MissingProfile)));
    let mut wire = serde_json::to_value(vec![profile]).unwrap();
    wire[0]["ambient_override"] = true.into();
    assert!(matches!(Registry::from_json(&serde_json::to_vec(&wire).unwrap()), Err(Error::InvalidWire)));
    wire[0].as_object_mut().unwrap().remove("ambient_override");
    wire[0]["device"] = "auto".into();
    assert!(matches!(Registry::from_json(&serde_json::to_vec(&wire).unwrap()), Err(Error::InvalidWire)));
}

#[test]
fn invalid_context_tokens_sampling_and_cli_integer_overflow_reject() {
    for change in 0..7 {
        let mut profile = cpu();
        match change {
            0 => profile.preset.context_limit = 0,
            1 => profile.preset.max_tokens = 0,
            2 => profile.preset.max_tokens = profile.preset.context_limit + 1,
            3 => profile.preset.context_limit = u32::MAX,
            4 => profile.model.compiled_context = Some(1),
            5 => profile.preset.sampling = Sampling::LlamaCpp { temperature: f64::NAN, top_k: 20, top_p: 0.9, min_p: 0.0 },
            _ => profile.preset.sampling = Sampling::LlamaCpp { temperature: 0.5, top_k: u32::MAX, top_p: 0.9, min_p: 0.0 },
        }
        assert_eq!(profile.validate(), Err(Error::InvalidBounds));
    }
    let mut profile = qairt();
    profile.preset.sampling = Sampling::Qairt { temperature: 0.0, seed: u64::MAX };
    assert_eq!(profile.validate(), Err(Error::InvalidBounds));
}

#[test]
fn no_device_fallback_or_content_substitution() {
    let mut profile = qairt();
    profile.device = Device::Cpu;
    assert_eq!(profile.validate(), Err(Error::IncompatibleProfile));
    profile = cpu();
    let resolved = Registry::new(vec![profile.clone()]).unwrap().resolve(&profile.id).unwrap();
    let mut host = binding(&profile);
    host.capabilities[0].device = Device::Npu;
    assert!(matches!(resolved.plan(host, "hello"), Err(Error::UnsupportedCapability)));
    let mut host = binding(&profile);
    host.content_sha256 = "c".repeat(64);
    assert!(matches!(resolved.plan(host, "hello"), Err(Error::InvalidContentIdentity)));
}

#[test]
fn selectors_are_registrations_not_paths_options_or_download_requests() {
    let profile = cpu();
    let resolved = Registry::new(vec![profile.clone()]).unwrap().resolve(&profile.id).unwrap();
    for selector in ["../model", "local/../x:Q4_0", "local/a/b:Q4_0", "https://example/model", "--help", "local/x:Q8_0"] {
        let mut host = binding(&profile);
        host.model_selector = selector.into();
        assert!(matches!(resolved.plan(host, "hello"), Err(Error::InvalidSelector)));
    }
}

#[test]
fn prompt_is_one_argument_and_debug_never_exposes_prompt_or_environment() {
    let profile = cpu();
    let resolved = Registry::new(vec![profile.clone()]).unwrap().resolve(&profile.id).unwrap();
    let prompt = "private-prompt; $(not-a-shell)\nsecond line";
    let plan = resolved.plan(binding(&profile), prompt).unwrap();
    let args = plan.arguments();
    assert_eq!(args.last().unwrap(), &OsString::from(prompt));
    assert_eq!(args[args.len() - 2], OsString::from("--prompt"));
    let debug = format!("{plan:?} {:?}", plan.binding());
    assert!(!debug.contains("private-prompt"));
    assert!(!debug.contains("not-for-debug"));
    assert!(!debug.contains("PRIVATE_VALUE"));
    assert!(matches!(resolved.plan(binding(&profile), "bad\0prompt"), Err(Error::InvalidPrompt)));
}

#[test]
fn identical_inputs_produce_identical_plans_without_mutation() {
    let profile = cpu();
    let resolved = Registry::new(vec![profile.clone()]).unwrap().resolve(&profile.id).unwrap();
    let host = binding(&profile);
    let first = resolved.plan(host.clone(), "prompt").unwrap();
    let second = resolved.plan(host, "prompt").unwrap();
    assert_eq!(first.arguments(), second.arguments());
    assert_eq!(first.resolved().fingerprint(), second.resolved().fingerprint());
}

#[test]
fn registry_lists_validated_full_snapshots_in_stable_id_order() {
    let first = qairt();
    let second = cpu();
    let forward = Registry::new(vec![first.clone(), second.clone()]).unwrap();
    let reverse = Registry::new(vec![second.clone(), first.clone()]).unwrap();
    let listed: Vec<_> = reverse.profiles().collect();
    assert_eq!(reverse.profiles().len(), 2);
    assert_eq!(listed.iter().map(|item| item.profile().id.as_str()).collect::<Vec<_>>(),
               vec![first.id.as_str(), second.id.as_str()]);
    for (item, expected) in listed.iter().zip([&first, &second]) {
        assert_eq!(item.profile(), expected);
        assert_eq!(item.fingerprint(), reverse.resolve(&expected.id).unwrap().fingerprint());
    }
    assert_eq!(forward.profiles().map(|item| item.fingerprint()).collect::<Vec<_>>(),
               reverse.profiles().map(|item| item.fingerprint()).collect::<Vec<_>>());
    assert!(matches!(reverse.resolve("missing"), Err(Error::MissingProfile)));
    assert!(matches!(Registry::new(vec![first.clone(), first]), Err(Error::DuplicateProfile)));
}

#[test]
fn output_contract_preserves_legacy_arguments_and_adds_one_global_flag() {
    for profile in [qairt(), cpu()] {
        let resolved = Registry::new(vec![profile.clone()]).unwrap().resolve(&profile.id).unwrap();
        let host = binding(&profile);
        let legacy = resolved.plan(host.clone(), "hello").unwrap();
        let console = resolved.plan_with_output_contract(host.clone(), "hello", OutputContract::ConsoleV1).unwrap();
        let structured = resolved.plan_with_output_contract(host.clone(), "hello", OutputContract::TestModeJsonV1).unwrap();
        let mut expected: Vec<OsString> = vec!["--skip-update".into(), "--data-dir".into(), host.data.as_os_str().to_owned()];
        match profile.engine {
            Engine::GenieXQairt => expected.extend([
                "--log", "info", "infer", "local/Qwen3-0.6B", "--compute", "npu", "--power-mode", "balanced",
                "--max-tokens", "64", "--think=false", "--temperature", "0", "--seed", "42", "--prompt", "hello",
            ].map(OsString::from)),
            Engine::GenieXLlamaCpp => expected.extend([
                "infer", "local/Qwen3-4B-abliterated:Q4_0", "--compute", "cpu", "--power-mode", "balanced",
                "--nctx", "2048", "--max-tokens", "256", "--think=false", "--temperature", "0.6",
                "--top-k", "20", "--top-p", "0.95", "--min-p", "0", "--prompt", "hello",
            ].map(OsString::from)),
        }
        assert_eq!(legacy.arguments(), expected.as_slice());
        assert_eq!(console.arguments(), expected.as_slice());
        assert_eq!(legacy.output_contract(), OutputContract::ConsoleV1);
        assert_eq!(console.output_contract(), OutputContract::ConsoleV1);
        assert_eq!(structured.output_contract(), OutputContract::TestModeJsonV1);
        let infer = expected.iter().position(|arg| arg == "infer").unwrap();
        expected.insert(infer, "--test-mode".into());
        assert_eq!(structured.arguments(), expected.as_slice());
        assert_eq!(structured.arguments().iter().filter(|arg| *arg == "--test-mode").count(), 1);
        for plan in [&legacy, &console, &structured] {
            assert_eq!(plan.resolved().profile(), &profile);
            assert_eq!(plan.resolved().fingerprint(), resolved.fingerprint());
            assert_eq!(plan.binding().executable, host.executable);
            assert_eq!(plan.binding().cwd, host.cwd);
            assert_eq!(plan.binding().data, host.data);
            assert_eq!(plan.binding().model_selector, host.model_selector);
            assert_eq!(plan.binding().content_sha256, host.content_sha256);
            assert_eq!(plan.binding().capabilities, host.capabilities);
            assert_eq!(plan.binding().environment, host.environment);
        }
    }
}

#[test]
fn output_contract_fingerprint_binds_profile_and_mode_and_keeps_prompt_private() {
    for profile in [qairt(), cpu()] {
        let resolved = Registry::new(vec![profile.clone()]).unwrap().resolve(&profile.id).unwrap();
        let prompt = "private-output-prompt; $(not-a-shell)\nnext line";
        let console = resolved.plan(binding(&profile), prompt).unwrap();
        let structured = resolved.plan_with_output_contract(binding(&profile), prompt, OutputContract::TestModeJsonV1).unwrap();
        assert_ne!(console.output_contract_fingerprint(), structured.output_contract_fingerprint());
        for (plan, identity) in [(&console, "console-v1"), (&structured, "test-mode-json-v1")] {
            let expected = handshake_canonical_json::sha256_hex(&handshake_canonical_json::canonical_json_bytes(&serde_json::json!({
                "schema": "handshake.dexterity.output_contract@1",
                "profile_fingerprint": resolved.fingerprint(),
                "output_contract": identity,
            })));
            assert_eq!(plan.output_contract_fingerprint(), expected);
            assert_eq!(plan.arguments().last().unwrap(), &OsString::from(prompt));
            assert_eq!(plan.arguments()[plan.arguments().len() - 2], OsString::from("--prompt"));
            let debug = format!("{plan:?} {:?}", plan.binding());
            for secret in ["private-output-prompt", "not-for-debug", "PRIVATE_VALUE"] {
                assert!(!debug.contains(secret));
            }
            let mut alternate = binding(&profile);
            alternate.environment.insert("OTHER_PRIVATE_VALUE".into(), "another-secret".into());
            alternate.cwd = alternate.cwd.join("other");
            let repeated = resolved.plan_with_output_contract(alternate, "different prompt", plan.output_contract()).unwrap();
            assert_eq!(plan.output_contract_fingerprint(), repeated.output_contract_fingerprint());
            let mut changed_profile = profile.clone();
            changed_profile.revision += 1;
            let changed = Registry::new(vec![changed_profile.clone()]).unwrap().resolve(&changed_profile.id).unwrap()
                .plan_with_output_contract(binding(&changed_profile), prompt, plan.output_contract()).unwrap();
            assert_ne!(plan.output_contract_fingerprint(), changed.output_contract_fingerprint());
        }
    }
}
