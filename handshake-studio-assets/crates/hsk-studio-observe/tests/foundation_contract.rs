use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::*;
use std::io::{Cursor, Read};
fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared30s deadline: {e}"),
    }
}
fn observe(events: u32, bytes: usize) -> Observe {
    Observe::new(
        42,
        7,
        DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap(),
        ActorContext::new(
            "account",
            "principal",
            "owner",
            "owner-principal",
            "space",
            "session",
        )
        .unwrap(),
        Budget::new(events, bytes).unwrap(),
    )
}
fn event(outcome: Outcome) -> Observation<'static> {
    Observation {
        correlation_id: 42,
        revision: 7,
        outcome,
        progress: if outcome == Outcome::Progress {
            Some(Progress::new(1, 2).unwrap())
        } else {
            None
        },
        private_project_text: Some("PROJECT-TEXT-SECRET-DO-NOT-EMIT"),
    }
}
struct CollectorSink {
    frames: Vec<Vec<u8>>,
    classes: Vec<DeliveryClass>,
    limit: usize,
    forced: Option<DeliveryError>,
    progress: usize,
    terminal: usize,
}
impl CollectorSink {
    fn new(limit: usize) -> Self {
        Self {
            frames: vec![],
            classes: vec![],
            limit,
            forced: None,
            progress: 0,
            terminal: 0,
        }
    }
}
impl SinkPort for CollectorSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        if let Some(e) = self.forced {
            return Err(e);
        }
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(DeliveryError::Rejected);
        }
        match class {
            DeliveryClass::Progress if self.progress >= self.limit => {
                return Err(DeliveryError::Saturated);
            }
            DeliveryClass::Terminal if self.terminal >= 1 => return Err(DeliveryError::Saturated),
            DeliveryClass::Progress => self.progress += 1,
            DeliveryClass::Terminal => self.terminal += 1,
        }
        self.frames.push(bytes.to_vec());
        self.classes.push(class);
        Ok(())
    }
}
#[derive(Debug)]
struct Decoded {
    kind: u8,
    code: u8,
    progress: bool,
    correlation: u64,
    revision: u64,
    sequence: u64,
    completed: u64,
    total: u64,
    identities: Vec<String>,
}
// Collector independently decodes actual emitted bytes; it calls no producer serializer.
fn collect(frame: &[u8]) -> Decoded {
    assert!(frame.len() <= 2048);
    let mut reader = Cursor::new(frame);
    let mut header = [0; 8];
    reader.read_exact(&mut header).unwrap();
    assert_eq!(&header[..4], b"HSKO");
    assert_eq!(header[4], 1);
    assert!((1..=4).contains(&header[5]));
    assert!(header[6] <= 4);
    assert!(header[7] <= 1);
    let mut values = [0u64; 5];
    for v in &mut values {
        let mut b = [0; 8];
        reader.read_exact(&mut b).unwrap();
        *v = u64::from_be_bytes(b)
    }
    let mut identities = vec![];
    for _ in 0..7 {
        let mut len = [0; 2];
        reader.read_exact(&mut len).unwrap();
        let len = u16::from_be_bytes(len) as usize;
        assert!(len <= 128);
        let mut value = vec![0; len];
        reader.read_exact(&mut value).unwrap();
        identities.push(String::from_utf8(value).unwrap())
    }
    assert_eq!(reader.position() as usize, frame.len());
    Decoded {
        kind: header[5],
        code: header[6],
        progress: header[7] == 1,
        correlation: values[0],
        revision: values[1],
        sequence: values[2],
        completed: values[3],
        total: values[4],
        identities,
    }
}
#[test]
fn external_collector_reads_progress_and_success() {
    bounded_case("external_collector_reads_progress_and_success", || {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CollectorSink>();
        assert_send_sync::<Observe>();
        let mut emitter = observe(2, 4096);
        let mut sink = CollectorSink::new(2);
        let token = CancellationToken::default();
        let first = emitter
            .emit(event(Outcome::Progress), &token, &mut sink)
            .unwrap();
        let terminal = emitter
            .emit(event(Outcome::Success), &token, &mut sink)
            .unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(terminal.sequence, 2);
        let p = collect(&sink.frames[0]);
        assert_eq!(
            (
                p.kind,
                p.code,
                p.progress,
                p.correlation,
                p.revision,
                p.sequence,
                p.completed,
                p.total
            ),
            (1, 0, true, 42, 7, 1, 1, 2)
        );
        assert_eq!(
            p.identities,
            vec![
                "SDOC-019abcde-0000-7000-8000-000000000001",
                "account",
                "principal",
                "owner",
                "owner-principal",
                "space",
                "session"
            ]
        );
        let t = collect(&sink.frames[1]);
        assert_eq!((t.kind, t.sequence, t.progress), (2, 2, false));
        assert_eq!(emitter.state().acknowledged, 2);
        assert!(emitter.state().terminal_closed)
    })
}
#[test]
fn project_content_redaction_is_byte_identical() {
    bounded_case("project_content_redaction_is_byte_identical", || {
        let mut a = observe(1, 2048);
        let mut b = observe(1, 2048);
        let mut sa = CollectorSink::new(1);
        let mut sb = CollectorSink::new(1);
        let token = CancellationToken::default();
        a.emit(event(Outcome::Progress), &token, &mut sa).unwrap();
        let mut safe = event(Outcome::Progress);
        safe.private_project_text = None;
        b.emit(safe, &token, &mut sb).unwrap();
        assert_eq!(sa.frames, sb.frames);
        assert!(
            !sa.frames[0]
                .windows(b"PROJECT-TEXT-SECRET".len())
                .any(|s| s == b"PROJECT-TEXT-SECRET")
        );
        collect(&sa.frames[0]);
    })
}
#[test]
fn rejected_and_unavailable_required_delivery_never_acknowledge() {
    bounded_case(
        "rejected_and_unavailable_required_delivery_never_acknowledge",
        || {
            for fail in [DeliveryError::Rejected, DeliveryError::Unavailable] {
                let mut emitter = observe(1, 2048);
                let mut sink = CollectorSink::new(1);
                sink.forced = Some(fail);
                let token = CancellationToken::default();
                assert_eq!(
                    emitter.emit(event(Outcome::Success), &token, &mut sink),
                    Err(Error::Delivery(fail))
                );
                assert_eq!(emitter.state().acknowledged, 0);
                assert_eq!(emitter.state().dropped_attempts, 1);
                assert_eq!(
                    emitter.state().rejection_count + emitter.state().unavailable_count,
                    1
                );
                assert!(sink.frames.is_empty());
                sink.forced = None;
                assert_eq!(
                    emitter
                        .emit(event(Outcome::Success), &token, &mut sink)
                        .unwrap()
                        .sequence,
                    1
                );
            }
        },
    )
}
#[test]
fn sink_saturation_preserves_terminal_cancel_route() {
    bounded_case("sink_saturation_preserves_terminal_cancel_route", || {
        let mut emitter = observe(10, 20480);
        let mut sink = CollectorSink::new(1);
        let token = CancellationToken::default();
        emitter
            .emit(event(Outcome::Progress), &token, &mut sink)
            .unwrap();
        assert_eq!(
            emitter.emit(event(Outcome::Progress), &token, &mut sink),
            Err(Error::Delivery(DeliveryError::Saturated))
        );
        assert_eq!(emitter.state().saturation_count, 1);
        assert_eq!(emitter.state().acknowledged, 1);
        token.cancel();
        let terminal = emitter
            .emit(event(Outcome::Success), &token, &mut sink)
            .unwrap();
        assert_eq!(terminal.outcome, Outcome::Canceled);
        assert_eq!(sink.classes[1], DeliveryClass::Terminal);
        assert_eq!(collect(&sink.frames[1]).kind, 4);
        assert_eq!(emitter.state().canceled_delivered, 1);
    })
}
#[test]
fn emitter_budget_saturation_and_cancel_reserve() {
    bounded_case("emitter_budget_saturation_and_cancel_reserve", || {
        for (events, bytes) in [(0, 2048), (1, 0)] {
            let mut emitter = observe(events, bytes);
            let mut sink = CollectorSink::new(1);
            let token = CancellationToken::default();
            assert_eq!(
                emitter.emit(event(Outcome::Progress), &token, &mut sink),
                Err(Error::Saturated)
            );
            assert_eq!(emitter.state().saturation_count, 1);
            assert_eq!(emitter.state().dropped_attempts, 1);
            assert!(sink.frames.is_empty());
            token.cancel();
            assert_eq!(
                emitter
                    .emit(event(Outcome::Progress), &token, &mut sink)
                    .unwrap()
                    .outcome,
                Outcome::Canceled
            );
            assert_eq!(collect(&sink.frames[0]).kind, 4);
        }
    })
}
#[test]
fn canceled_required_delivery_failure_still_returns_error() {
    bounded_case(
        "canceled_required_delivery_failure_still_returns_error",
        || {
            let mut emitter = observe(0, 0);
            let mut sink = CollectorSink::new(0);
            sink.forced = Some(DeliveryError::Rejected);
            let token = CancellationToken::default();
            token.cancel();
            assert_eq!(
                emitter.emit(event(Outcome::Success), &token, &mut sink),
                Err(Error::Delivery(DeliveryError::Rejected))
            );
            assert_eq!(emitter.state().canceled_delivered, 0);
            assert_eq!(emitter.state().acknowledged, 0);
        },
    )
}
#[test]
fn typed_failure_and_closed_terminal() {
    bounded_case("typed_failure_and_closed_terminal", || {
        let mut emitter = observe(0, 0);
        let mut sink = CollectorSink::new(0);
        let token = CancellationToken::default();
        emitter
            .emit(
                event(Outcome::Failure(FailureCode::Loss)),
                &token,
                &mut sink,
            )
            .unwrap();
        let frame = collect(&sink.frames[0]);
        assert_eq!((frame.kind, frame.code), (3, 3));
        assert_eq!(
            emitter.emit(event(Outcome::Success), &token, &mut sink),
            Err(Error::Closed)
        );
        assert_eq!(sink.frames.len(), 1);
    })
}
#[test]
fn revision_correlation_and_progress_rejection() {
    bounded_case("revision_correlation_and_progress_rejection", || {
        let mut emitter = observe(1, 2048);
        let mut sink = CollectorSink::new(1);
        let token = CancellationToken::default();
        let mut input = event(Outcome::Success);
        input.revision = 8;
        assert_eq!(
            emitter.emit(input, &token, &mut sink),
            Err(Error::RevisionMismatch)
        );
        input.revision = 7;
        input.correlation_id = 43;
        assert_eq!(
            emitter.emit(input, &token, &mut sink),
            Err(Error::RevisionMismatch)
        );
        let mut input = event(Outcome::Progress);
        input.progress = None;
        assert_eq!(
            emitter.emit(input, &token, &mut sink),
            Err(Error::InvalidProgress)
        );
        assert_eq!(Progress::new(2, 1), Err(Error::InvalidProgress));
        assert_eq!(
            Budget::new(MAX_PROGRESS_EVENTS + 1, 0),
            Err(Error::InvalidBudget)
        );
        assert!(sink.frames.is_empty());
    })
}
#[test]
fn indeterminate_delivery_blocks_duplicate_retry() {
    bounded_case("indeterminate_delivery_blocks_duplicate_retry", || {
        let mut emitter = observe(1, 2048);
        let mut sink = CollectorSink::new(1);
        sink.forced = Some(DeliveryError::Indeterminate);
        let token = CancellationToken::default();
        assert_eq!(
            emitter.emit(event(Outcome::Success), &token, &mut sink),
            Err(Error::Delivery(DeliveryError::Indeterminate))
        );
        assert_eq!(emitter.state().indeterminate_count, 1);
        assert_eq!(emitter.state().acknowledged, 0);
        sink.forced = None;
        assert_eq!(
            emitter.emit(event(Outcome::Success), &token, &mut sink),
            Err(Error::ReconciliationRequired)
        );
        assert!(sink.frames.is_empty());
    })
}
