//! Observe-local real stdout byte sink. Caller captures stdout as diagnostic frames.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::*;
use std::{
    env,
    io::{self, Write},
};
// Caller-owned bounded admission; actual stdout collection runs outside the port callback.
struct OutputSink {
    frames: Vec<(DeliveryClass, Vec<u8>)>,
    mode: String,
}
impl SinkPort for OutputSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        match self.mode.as_str() {
            "reject" => return Err(DeliveryError::Rejected),
            "saturate" => return Err(DeliveryError::Saturated),
            _ => {}
        }
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(DeliveryError::Rejected);
        }
        if self.frames.iter().any(|(accepted, _)| *accepted == class) {
            return Err(DeliveryError::Saturated);
        }
        self.frames.push((class, bytes.to_vec()));
        Ok(())
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--descriptor"] || args == ["--help"] {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    let mut mode = "success".to_owned();
    let mut text = None;
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).ok_or("missing_argument")?;
        match args[i].as_str() {
            "--mode" => mode = value.clone(),
            "--private-text" => text = Some(value.as_str()),
            _ => return Err("unknown_argument".into()),
        }
        i += 2
    }
    if !["success", "error", "cancel", "reject", "saturate"].contains(&mode.as_str()) {
        return Err("unsupported_mode".into());
    }
    let actor = ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session",
    )
    .map_err(|_| "invalid_context")?;
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001")
        .map_err(|_| "invalid_resource")?;
    let mut emitter = Observe::new(
        42,
        7,
        resource,
        actor,
        Budget::new(1, MAX_FRAME_BYTES).map_err(|e| e.code())?,
    );
    let token = CancellationToken::default();
    let mut sink = OutputSink {
        frames: Vec::with_capacity(2),
        mode: mode.clone(),
    };
    let progress = Observation {
        correlation_id: 42,
        revision: 7,
        outcome: Outcome::Progress,
        progress: Some(Progress::new(1, 2).map_err(|e| e.code())?),
        private_project_text: text,
    };
    if let Err(e) = emitter.emit(progress, &token, &mut sink) {
        eprintln!(
            "dropped_attempts={} acknowledged={}",
            emitter.state().dropped_attempts,
            emitter.state().acknowledged
        );
        return Err(e.code().into());
    }
    if mode == "cancel" {
        token.cancel()
    }
    let terminal = Observation {
        correlation_id: 42,
        revision: 7,
        outcome: if mode == "error" {
            Outcome::Failure(FailureCode::Validation)
        } else {
            Outcome::Success
        },
        progress: None,
        private_project_text: text,
    };
    let receipt = emitter
        .emit(terminal, &token, &mut sink)
        .map_err(|e| e.code())?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for (_, frame) in &sink.frames {
        writer
            .write_all(frame)
            .map_err(|_| "collector_output_indeterminate")?;
    }
    writer
        .flush()
        .map_err(|_| "collector_output_indeterminate")?;
    eprintln!(
        "delivered_sequence={} outcome_code={} acknowledged={}",
        receipt.sequence,
        receipt.outcome.wire(),
        emitter.state().acknowledged
    );
    Ok(())
}
fn main() {
    if let Err(code) = run() {
        eprintln!(
            "rejected:{code}; consult --descriptor and correct only the caller sink/input; no host proof"
        );
        std::process::exit(2)
    }
}
