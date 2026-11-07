//! Lane SMB-OBSERVE-DIAG-SINK-1: what one `Observe::emit` costs, measured, never asserted (a timing assertion is a
//! flaky test). Run it in release, one test thread:
//! `cargo nextest run -p hsk-studio-observe --release --run-ignored only -E 'test(emit_cost)' --no-capture`.
//! It uses only the API that existed before the ring wiring, so the same file measures both sides of the change.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, Observation, Observe, Outcome, Progress, SinkPort};
use std::hint::black_box;
use std::time::Instant;

struct Quiet;
impl SinkPort for Quiet {
    fn try_send(&mut self, _class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        black_box(bytes);
        Ok(())
    }
}

const PER_EMITTER: u32 = 1024;
const EMITTERS: usize = 4000;

#[test]
#[ignore = "measurement only: prints ns per emit, asserts no timing; run in release with --run-ignored only"]
fn emit_cost_with_no_port_bound() {
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-00000000beef").unwrap();
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let cancel = CancellationToken::default();
    let event = Observation {
        correlation_id: 42,
        revision: 7,
        outcome: Outcome::Progress,
        progress: Some(Progress::new(1, 2).unwrap()),
        private_project_text: None,
    };
    let mut per_batch = Vec::with_capacity(EMITTERS);
    for _ in 0..EMITTERS {
        let budget = Budget::new(PER_EMITTER, 2048 * PER_EMITTER as usize).unwrap();
        let mut emitter = Observe::new(42, 7, resource.clone(), actor.clone(), budget);
        let start = Instant::now();
        for _ in 0..PER_EMITTER {
            black_box(emitter.emit(black_box(event), &cancel, &mut Quiet)).unwrap();
        }
        per_batch.push(start.elapsed().as_nanos() as f64 / f64::from(PER_EMITTER));
    }
    per_batch.sort_by(f64::total_cmp);
    println!(
        "MEASURE emit_cost ns_per_emit min={:.1} median={:.1} p99={:.1} emits={}",
        per_batch[0],
        per_batch[per_batch.len() / 2],
        per_batch[per_batch.len() * 99 / 100],
        EMITTERS * PER_EMITTER as usize,
    );
}
