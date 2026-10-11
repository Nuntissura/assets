//! Real consumer of the public Score Plugin API: spawns the reference worker, activates it on the
//! worker executable itself as the "approved plugin file" (the reference backend verifies the file's
//! SHA-256 but does not load it), processes one stereo block through a score `Engine` and prints the
//! receipts. No device, no window; every child it spawns is reaped before it exits. Usage:
//! `score-plugin-consumer [--descriptor] --worker PATH [--gain G] [--frames N] [--latency L]
//! [--crash-on N] [--hang-on N] [--deadline-ms MS]` (`--crash-on` / `--hang-on` exist only when built
//! with the non-default cargo feature `test-faults`)
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, MAX_FRAME_BYTES, Observe, SinkPort,
};
use hsk_studio_score::{
    BlockSpec, ChannelLayout, Engine, InputBlock, OutputBlock, RequestContext, sha256, to_hex,
};
use hsk_studio_score_plugin::{
    ApprovedPlugin, Arch, DESCRIPTOR, HostOptions, IsolatedHost, Parameter, PluginFormat,
    PluginGrant, PluginProcessor, PluginProvider, ProcessConfig, SUPPORTED_CLAP_ABI, outcome_of,
    report,
};
use std::{env, path::PathBuf, time::Duration};

struct FrameSink(Vec<(DeliveryClass, usize)>);
impl SinkPort for FrameSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        assert!(bytes.len() <= MAX_FRAME_BYTES);
        self.0.push((class, bytes.len()));
        Ok(())
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--descriptor" || a == "--help") {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    let (mut worker, mut gain, mut frames, mut latency) = (None, 0.5_f64, 256_usize, 0_u32);
    let (mut crash_on, mut hang_on, mut deadline_ms) = (None, None, 500_u64);
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        i += 1;
        let value = args.get(i).ok_or("missing_argument")?;
        match flag {
            "--worker" => worker = Some(PathBuf::from(value)),
            "--gain" => gain = value.parse().map_err(|_| "bad_number")?,
            "--frames" => frames = value.parse().map_err(|_| "bad_number")?,
            "--latency" => latency = value.parse().map_err(|_| "bad_number")?,
            "--crash-on" => crash_on = Some(value.parse::<u64>().map_err(|_| "bad_number")?),
            "--hang-on" => hang_on = Some(value.parse::<u64>().map_err(|_| "bad_number")?),
            "--deadline-ms" => deadline_ms = value.parse().map_err(|_| "bad_number")?,
            _ => return Err("unknown_argument".into()),
        }
        i += 1;
    }
    let worker = worker.ok_or("missing_worker")?;
    let deadline = Duration::from_millis(deadline_ms);

    let mut options = HostOptions::default().with_worker_program(&worker);
    options.reference_latency = latency;
    #[cfg(feature = "test-faults")]
    {
        if let Some(n) = crash_on {
            options
                .test_args
                .extend(["--fault".to_owned(), format!("crash-on-block={n}")]);
        }
        if let Some(n) = hang_on {
            options
                .test_args
                .extend(["--fault".to_owned(), format!("hang-on-block={n}")]);
        }
    }
    #[cfg(not(feature = "test-faults"))]
    if crash_on.is_some() || hang_on.is_some() {
        return Err("fault_flags_need_feature_test_faults".into());
    }
    let host = IsolatedHost::new(options).map_err(|e| e.code())?;

    let file = std::fs::read(&worker).map_err(|_| "worker_unreadable")?;
    let content_sha256 = sha256(&file);
    let actor = ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session",
    )
    .map_err(|_| "invalid_context")?;
    let config = ProcessConfig {
        plugin: ApprovedPlugin {
            path: worker.clone(),
            content_sha256,
            format: PluginFormat::Clap,
            arch: Arch::host().ok_or("unknown_arch")?,
            abi_version: SUPPORTED_CLAP_ABI,
        },
        grant: Some(PluginGrant {
            actor: actor.clone(),
            content_sha256,
        }),
        sample_rate_hz: 48_000,
        layout: ChannelLayout::stereo(),
        max_block_frames: frames.max(1),
        parameters: vec![Parameter { id: 0, value: gain }],
        max_state_bytes: 64,
    };
    let cancel = CancellationToken::default();
    let descriptor = host
        .probe(&config.plugin, config.grant.as_ref(), &cancel, deadline)
        .map_err(|e| e.code())?;
    println!(
        "probe: plugin_id={} format={:?} abi={} sha256={}",
        descriptor.plugin_id,
        descriptor.format,
        descriptor.abi_version,
        to_hex(&content_sha256)
    );
    let mut instance = host
        .activate(&config, &cancel, deadline)
        .map_err(|e| e.code())?;

    let pattern = [0.0_f32, 0.25, -0.25, 0.5];
    let left: Vec<f32> = (0..frames).map(|n| pattern[n % 4]).collect();
    let right: Vec<f32> = left.iter().map(|s| -s).collect();
    let spec = BlockSpec {
        sample_rate_hz: 48_000,
        layout: ChannelLayout::stereo(),
        frames,
        start_sample: 0,
    };
    let channels: [&[f32]; 2] = [&left, &right];
    let input = InputBlock::new(&spec, &channels);
    let mut out =
        OutputBlock::with_capacity(ChannelLayout::stereo(), frames).map_err(|e| e.code())?;
    let resource = DomainId::parse("STRK-019abcde-0000-7000-8000-000000000001")
        .map_err(|_| "invalid_resource")?;
    let engine = Engine::new(resource.clone(), 7);
    let ctx = RequestContext {
        resource: resource.clone(),
        actor: Some(actor.clone()),
        expected_revision: 7,
    };

    let (result, typed) = {
        let mut processor = PluginProcessor::new(instance.as_mut(), deadline);
        let result = engine.process(&ctx, &input, &mut processor, &mut out, &cancel);
        let typed = processor.take_error();
        (result, typed)
    };
    match (&result, typed) {
        (Ok(receipt), _) => println!(
            "block: frames={} latency={} published={} first4_l={:?}",
            receipt.processed_frames,
            receipt.latency_samples,
            out.published_frames(),
            &out.published_channel(0).ok_or("nothing_published")?[..4.min(frames)]
        ),
        (Err(score), plugin) => println!(
            "block: refused score={} plugin={:?} published={}",
            score.code(),
            plugin.map(|e| e.code()),
            out.published_frames()
        ),
    }
    let fault = instance.last_fault().cloned();
    println!("fault: {fault:?}");
    let pid = host.ledger().last().map(|e| e.pid);
    let retired = instance.retire();
    println!("retire: {retired:?}");
    println!("ledger: {:?}", host.ledger());
    println!("stats: {:?} (pid {pid:?})", host.stats());

    let plugin_result: Result<(), hsk_studio_score_plugin::PluginError> = match fault {
        Some(f) => Err(f.error),
        None => Ok(()),
    };
    let mut observe = Observe::new(
        9,
        7,
        resource,
        actor,
        Budget::new(0, 0).map_err(|_| "budget")?,
    );
    let mut sink = FrameSink(Vec::new());
    report(&plugin_result, 9, 7, &cancel, &mut observe, &mut sink).map_err(|e| e.code())?;
    println!(
        "observe: outcome={:?} frames={:?}",
        outcome_of(&plugin_result),
        sink.0
    );
    Ok(())
}

fn main() {
    if let Err(code) = run() {
        eprintln!("error: {code}");
        std::process::exit(2);
    }
}
