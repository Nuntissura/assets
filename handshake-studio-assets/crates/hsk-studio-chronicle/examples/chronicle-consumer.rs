use hsk_studio_accord::CancellationToken;
use hsk_studio_chronicle::*;
use hsk_studio_folio::{Inspection, Unresolved};
use hsk_studio_observe::{DeliveryClass, DeliveryError, Outcome, SinkPort};
use std::{env, fs::File, io::Read};
struct Sink {
    mode: String,
    frames: usize,
    bytes: usize,
}
impl SinkPort for Sink {
    fn try_send(
        &mut self,
        _: DeliveryClass,
        bytes: &[u8],
    ) -> std::result::Result<(), DeliveryError> {
        match self.mode.as_str() {
            "rejected" => Err(DeliveryError::Rejected),
            "indeterminate" => Err(DeliveryError::Indeterminate),
            _ => {
                if self.frames >= 1 || bytes.len() > hsk_studio_observe::MAX_FRAME_BYTES {
                    return Err(DeliveryError::Saturated);
                }
                self.frames += 1;
                self.bytes += bytes.len();
                Ok(())
            }
        }
    }
}
fn read(path: &str, max: usize) -> std::result::Result<Vec<u8>, String> {
    if max > 64 * 1024 * 1024 {
        return Err("input safety ceiling".into());
    }
    let mut out = Vec::new();
    File::open(path)
        .map_err(|_| "input unavailable")?
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| "input read failed")?;
    if out.len() > max {
        return Err("input budget".into());
    }
    Ok(out)
}
fn install(
    current: &mut hsk_studio_folio::Snapshot,
    vector: &mut [RevisionEntry],
    prepared: &Prepared,
) {
    if let Some(s) = prepared.successor() {
        for u in prepared.revision_updates() {
            let entry = vector
                .iter_mut()
                .find(|e| e.address == u.address)
                .expect("prepare checked current entry");
            entry.revision = u.revision;
        }
        *current = s.clone();
    }
}
fn run() -> std::result::Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--descriptor"] {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    if args == ["--schema"] {
        println!(
            "{}",
            serde_json::to_string(&patch_schema()).map_err(|_| "schema")?
        );
        return Ok(());
    }
    if args == ["--result-schema"] {
        println!(
            "{}",
            serde_json::to_string(&result_schema()).map_err(|_| "schema")?
        );
        return Ok(());
    }
    let (mut document, mut revisions) = (None, None);
    let mut patches = Vec::new();
    let (mut inverse, mut before, mut after) = (false, false, false);
    let mut delivery = "delivered".to_string();
    let mut budget = Budget::default();
    let mut n = 0;
    while n < args.len() {
        let flag = &args[n];
        match flag.as_str() {
            "--inverse" => {
                inverse = true;
                n += 1;
                continue;
            }
            "--cancel-before" => {
                before = true;
                n += 1;
                continue;
            }
            "--cancel-after" => {
                after = true;
                n += 1;
                continue;
            }
            _ => {}
        }
        let value = args.get(n + 1).ok_or("missing flag value")?;
        match flag.as_str() {
            "--document" => document = Some(value.clone()),
            "--revisions" => revisions = Some(value.clone()),
            "--patch" => {
                if patches.len() >= 1024 {
                    return Err("patch count budget".into());
                }
                patches.push(value.clone());
            }
            "--delivery" => delivery = value.clone(),
            "--max-bytes" => budget.input_bytes = value.parse().map_err(|_| "invalid budget")?,
            "--max-reads" => budget.reads = value.parse().map_err(|_| "invalid budget")?,
            "--max-writes" => budget.writes = value.parse().map_err(|_| "invalid budget")?,
            "--max-value-bytes" => {
                budget.value_bytes = value.parse().map_err(|_| "invalid budget")?
            }
            _ => return Err("unknown flag".into()),
        }
        n += 2;
    }
    if !["delivered", "rejected", "indeterminate"].contains(&delivery.as_str()) {
        return Err("invalid delivery mode".into());
    }
    if patches.is_empty() {
        return Err("patch required".into());
    }
    let t = CancellationToken::default();
    let document = read(
        &document.ok_or("document required")?,
        budget.folio.input_bytes,
    )?;
    let mut current =
        match hsk_studio_folio::inspect_bytes(&document, budget.folio, &t, &Unresolved)
            .map_err(|e| format!("folio:{:?}", e.code))?
        {
            Inspection::Editable(s) => s,
            _ => return Err("read-only document".into()),
        };
    let mut vector = decode_revisions(
        &read(&revisions.ok_or("revisions required")?, budget.input_bytes)?,
        budget,
        &t,
    )
    .map_err(|e| format!("{:?}", e.code))?;
    let mut outcomes = Vec::new();
    let mut first_inverse = None;
    let mut first_actor = None;
    for path in patches {
        let p = decode_patch(&read(&path, budget.input_bytes)?, budget, &t)
            .map_err(|e| format!("{:?}", e.code))?;
        let prepare_token = CancellationToken::default();
        if before {
            prepare_token.cancel();
        }
        let prepared = prepare(&p, &current, &vector, budget, &prepare_token, &Unresolved)
            .map_err(|e| format!("{:?}", e.code))?;
        // This bounded synchronous consumer owns local read plus publication; no host acceptance.
        if prepare_token.check().is_err() {
            return Err("Canceled before source publication".into());
        }
        if first_inverse.is_none() {
            first_inverse = prepared.inverse().cloned();
            first_actor = Some(p.actor.clone());
        }
        install(&mut current, &mut vector, &prepared);
        let emit_token = CancellationToken::default();
        if after {
            emit_token.cancel();
        }
        let mut sink = Sink {
            mode: delivery.clone(),
            frames: 0,
            bytes: 0,
        };
        let diagnostic = deliver(
            &p,
            current.document().revision,
            Outcome::Success,
            &mut sink,
            &emit_token,
        )
        .map_err(|e| format!("{:?}", e.code))?;
        outcomes.push(serde_json::json!({"result":prepared.wire(),"source_only":true,"delivery":format!("{:?}",diagnostic.receipt),"delivery_acknowledged":diagnostic.state.acknowledged,"delivery_indeterminate":diagnostic.state.indeterminate_count,"delivery_frames":sink.frames,"delivery_bytes":sink.bytes,"post_source_cancel":after}));
    }
    if inverse {
        let inv = first_inverse.ok_or("no changed result to invert")?;
        let p = inv.invocation(
            "consumer-inverse".into(),
            99,
            first_actor.ok_or("inverse actor")?,
            current.document().revision,
        );
        let prepared = prepare(&p, &current, &vector, budget, &t, &Unresolved)
            .map_err(|e| format!("{:?}", e.code))?;
        install(&mut current, &mut vector, &prepared);
        outcomes.push(serde_json::json!({"result":prepared.wire(),"source_only":true}));
    }
    println!(
        "{}",
        serde_json::json!({"outcomes":outcomes,"current":current.document(),"revisions":vector,"host_acceptance":false})
    );
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(2);
    }
}
