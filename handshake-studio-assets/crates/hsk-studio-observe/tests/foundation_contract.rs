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

#[test]
fn detail_v2_closed_fields_and_legacy_bytes_preserved() {
    bounded_case("detail_v2_closed_fields_and_legacy_bytes_preserved", || {
        let token = CancellationToken::default();
        let mut legacy = observe(0, 0);
        let mut legacy_sink = CollectorSink::new(0);
        legacy
            .emit(event(Outcome::Success), &token, &mut legacy_sink)
            .unwrap();
        let mut expected = b"HSKO".to_vec();
        expected.extend_from_slice(&[1, 2, 0, 0]);
        for value in [42u64, 7, 1, 0, 0] {
            expected.extend_from_slice(&value.to_be_bytes());
        }
        for text in [
            "SDOC-019abcde-0000-7000-8000-000000000001",
            "account",
            "principal",
            "owner",
            "owner-principal",
            "space",
            "session",
        ] {
            expected.extend_from_slice(&(text.len() as u16).to_be_bytes());
            expected.extend_from_slice(text.as_bytes());
        }
        assert_eq!(legacy_sink.frames[0], expected);
        let address = DiagnosticAddress::new(
            DiagnosticTarget::Layer,
            DiagnosticProperty::Name,
            DomainId::parse("SLYR-019abcde-0000-7000-8000-000000000001").unwrap(),
        )
        .unwrap();
        let detail = DiagnosticDetail::new(
            Disposition::Rejected,
            Some(DiagnosticCode::RevisionConflict),
            Some(address),
            [1, 1, 0, 0, 0],
        )
        .unwrap();
        let mut emitter = observe(0, 0);
        let mut sink = CollectorSink::new(0);
        emitter
            .emit_detail(
                event(Outcome::Failure(FailureCode::Validation)),
                &detail,
                &token,
                &mut sink,
            )
            .unwrap();
        let bytes = &sink.frames[0];
        let mut expected_v2 = expected.clone();
        expected_v2[4] = 2;
        expected_v2[5] = 3;
        expected_v2[6] = 1;
        expected_v2.extend_from_slice(&[3, 12, 1]);
        for count in [1u32, 1, 0, 0, 0] {
            expected_v2.extend_from_slice(&count.to_be_bytes());
        }
        expected_v2.extend_from_slice(&[1, 1]);
        let layer = "SLYR-019abcde-0000-7000-8000-000000000001";
        expected_v2.extend_from_slice(&(layer.len() as u16).to_be_bytes());
        expected_v2.extend_from_slice(layer.as_bytes());
        assert_eq!(*bytes, expected_v2);
        assert_eq!(bytes[4], 2);
        let mut reader = Cursor::new(bytes);
        reader.set_position(48);
        for _ in 0..7 {
            let mut len = [0; 2];
            reader.read_exact(&mut len).unwrap();
            reader.set_position(reader.position() + u16::from_be_bytes(len) as u64);
        }
        let mut tags = [0; 3];
        reader.read_exact(&mut tags).unwrap();
        assert_eq!(tags, [3, 12, 1]);
        for expected in [1u32, 1, 0, 0, 0] {
            let mut value = [0; 4];
            reader.read_exact(&mut value).unwrap();
            assert_eq!(u32::from_be_bytes(value), expected);
        }
        let mut tags = [0; 2];
        reader.read_exact(&mut tags).unwrap();
        assert_eq!(tags, [1, 1]);
        let mut len = [0; 2];
        reader.read_exact(&mut len).unwrap();
        let mut id = vec![0; u16::from_be_bytes(len) as usize];
        reader.read_exact(&mut id).unwrap();
        assert_eq!(
            String::from_utf8(id).unwrap(),
            "SLYR-019abcde-0000-7000-8000-000000000001"
        );
        assert_eq!(reader.position() as usize, bytes.len());
        assert!(
            !bytes
                .windows(b"PROJECT-TEXT-SECRET-DO-NOT-EMIT".len())
                .any(|v| v == b"PROJECT-TEXT-SECRET-DO-NOT-EMIT")
        );
    });
}
#[test]
fn detail_invalid_address_counts_and_delivery_refused_safely() {
    bounded_case(
        "detail_invalid_address_counts_and_delivery_refused_safely",
        || {
            let id = DomainId::parse("SART-019abcde-0000-7000-8000-000000000001").unwrap();
            assert_eq!(
                DiagnosticAddress::new(
                    DiagnosticTarget::Layer,
                    DiagnosticProperty::Name,
                    id.clone()
                ),
                Err(Error::InvalidDetail)
            );
            assert_eq!(
                DiagnosticAddress::new(DiagnosticTarget::Artboard, DiagnosticProperty::Visible, id),
                Err(Error::InvalidDetail)
            );
            assert_eq!(
                DiagnosticDetail::new(Disposition::Rejected, None, None, [1, 1, 0, 0, 0]),
                Err(Error::InvalidDetail)
            );
            assert_eq!(
                DiagnosticDetail::new(
                    Disposition::Rejected,
                    Some(DiagnosticCode::BudgetExceeded),
                    None,
                    [1, 1, 1, 0, 0]
                ),
                Err(Error::InvalidDetail)
            );
            assert_eq!(
                DiagnosticDetail::new(
                    Disposition::NoChange,
                    None,
                    None,
                    [MAX_DETAIL_COUNT + 1, 0, 0, 0, 0]
                ),
                Err(Error::InvalidDetail)
            );
            let detail =
                DiagnosticDetail::new(Disposition::Changed, None, None, [1, 1, 1, 1, 1]).unwrap();
            let token = CancellationToken::default();
            token.cancel();
            let mut emitter = observe(0, 0);
            let mut sink = CollectorSink::new(0);
            sink.forced = Some(DeliveryError::Indeterminate);
            assert_eq!(
                emitter.emit_detail(event(Outcome::Success), &detail, &token, &mut sink),
                Err(Error::Delivery(DeliveryError::Indeterminate))
            );
            assert!(emitter.state().reconciliation_required);
            assert_eq!(emitter.state().acknowledged, 0);
            assert_eq!(
                emitter.emit_detail(event(Outcome::Success), &detail, &token, &mut sink),
                Err(Error::ReconciliationRequired)
            );
        },
    );
}

#[derive(Debug, PartialEq, Eq)]
struct ResourceFrame {
    outcome: u8,
    disposition: u8,
    code: u8,
    epoch: u64,
    counts: [u32; 3],
    tile_limit: u32,
    bytes: [u64; 3],
    byte_limit: u64,
    address: Option<(String, String, i64, i64)>,
}
// External-style collector consumes documented v3 bytes without a producer serializer.
fn collect_resource(frame: &[u8]) -> ResourceFrame {
    assert!(frame.len() <= 2048);
    let mut reader = Cursor::new(frame);
    let mut header = [0; 8];
    reader.read_exact(&mut header).unwrap();
    assert_eq!(&header[..4], b"HSKO");
    assert_eq!(header[4], 3);
    assert_eq!(header[7], 0);
    assert!((2..=4).contains(&header[5]));
    assert!(header[6] <= 4);
    reader.set_position(48);
    for _ in 0..7 {
        let mut len = [0; 2];
        reader.read_exact(&mut len).unwrap();
        let len = u16::from_be_bytes(len);
        assert!(len <= 128);
        reader.set_position(reader.position() + len as u64);
    }
    let mut tags = [0; 3];
    reader.read_exact(&mut tags).unwrap();
    assert!((1..=4).contains(&tags[0]));
    assert!(tags[1] <= 13);
    assert!(tags[2] <= 1);
    let mut b = [0; 8];
    reader.read_exact(&mut b).unwrap();
    let epoch = u64::from_be_bytes(b);
    let mut counts = [0; 3];
    for count in &mut counts {
        let mut b = [0; 4];
        reader.read_exact(&mut b).unwrap();
        *count = u32::from_be_bytes(b);
    }
    let mut b = [0; 4];
    reader.read_exact(&mut b).unwrap();
    let tile_limit = u32::from_be_bytes(b);
    assert!(counts[0] <= 1_048_576 && tile_limit <= 1_048_576);
    assert!(counts[1] <= counts[0] && counts[1] <= tile_limit && counts[2] <= counts[1]);
    let mut bytes = [0; 3];
    for count in &mut bytes {
        let mut b = [0; 8];
        reader.read_exact(&mut b).unwrap();
        *count = u64::from_be_bytes(b);
    }
    let mut b = [0; 8];
    reader.read_exact(&mut b).unwrap();
    let byte_limit = u64::from_be_bytes(b);
    assert!(
        bytes[0]
            .checked_add(bytes[1])
            .and_then(|n| n.checked_add(bytes[2]))
            .unwrap()
            <= byte_limit
    );
    let address = if tags[2] == 1 {
        let mut property = [0; 1];
        reader.read_exact(&mut property).unwrap();
        assert_eq!(property[0], 1);
        let mut strings = Vec::new();
        for _ in 0..2 {
            let mut len = [0; 2];
            reader.read_exact(&mut len).unwrap();
            let n = u16::from_be_bytes(len) as usize;
            assert!(n > 0 && n <= 128);
            let mut s = vec![0; n];
            reader.read_exact(&mut s).unwrap();
            strings.push(String::from_utf8(s).unwrap());
        }
        assert!(strings[0].starts_with("SLYR-"));
        assert!(
            strings[1]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        );
        let mut b = [0; 8];
        reader.read_exact(&mut b).unwrap();
        let column = i64::from_be_bytes(b);
        reader.read_exact(&mut b).unwrap();
        let row = i64::from_be_bytes(b);
        Some((strings.remove(0), strings.remove(0), column, row))
    } else {
        None
    };
    assert_eq!(reader.position() as usize, frame.len());
    ResourceFrame {
        outcome: header[5],
        disposition: tags[0],
        code: tags[1],
        epoch,
        counts,
        tile_limit,
        bytes,
        byte_limit,
        address,
    }
}
#[test]
fn resource_v3_actual_tile_tuple_epoch_bytes_and_privacy() {
    bounded_case(
        "resource_v3_actual_tile_tuple_epoch_bytes_and_privacy",
        || {
            let layer = "SLYR-019abcde-0000-7000-8000-000000000001";
            let address = TileAddress::new(
                DomainId::parse(layer).unwrap(),
                "tile_01-A",
                i64::MIN,
                i64::MAX,
            )
            .unwrap();
            let counts = ResourceCounts::new([2, 2, 1], 2, [4096, 1024, 4096], 9216).unwrap();
            let detail = ResourceDetail::new(
                ResourceDisposition::Changed,
                None,
                Some(address),
                99,
                counts,
            )
            .unwrap();
            let mut emitter = observe(0, 0);
            let mut sink = CollectorSink::new(0);
            let receipt = emitter
                .emit_resource(
                    event(Outcome::Success),
                    &detail,
                    &CancellationToken::default(),
                    &mut sink,
                )
                .unwrap();
            assert_eq!(receipt.outcome, Outcome::Success);
            assert_eq!(
                collect_resource(&sink.frames[0]),
                ResourceFrame {
                    outcome: 2,
                    disposition: 1,
                    code: 0,
                    epoch: 99,
                    counts: [2, 2, 1],
                    tile_limit: 2,
                    bytes: [4096, 1024, 4096],
                    byte_limit: 9216,
                    address: Some((layer.into(), "tile_01-A".into(), i64::MIN, i64::MAX))
                }
            );
            assert!(
                !sink.frames[0]
                    .windows(b"PROJECT-TEXT-SECRET-DO-NOT-EMIT".len())
                    .any(|w| w == b"PROJECT-TEXT-SECRET-DO-NOT-EMIT")
            );
            assert_eq!(sink.classes, [DeliveryClass::Terminal]);
            assert!(emitter.state().terminal_closed);
            assert_eq!(
                emitter.emit_resource(
                    event(Outcome::Success),
                    &detail,
                    &CancellationToken::default(),
                    &mut sink
                ),
                Err(Error::Closed)
            );
        },
    );
}
#[test]
fn resource_v3_closed_address_count_byte_and_disposition_negatives() {
    bounded_case(
        "resource_v3_closed_address_count_byte_and_disposition_negatives",
        || {
            let layer = DomainId::parse("SLYR-019abcde-0000-7000-8000-000000000001").unwrap();
            for key in ["", "a/b", "a.b", "a\\b", "private label", "λ"] {
                assert_eq!(
                    TileAddress::new(layer.clone(), key, 0, 0),
                    Err(Error::InvalidDetail)
                );
            }
            assert_eq!(
                TileAddress::new(layer.clone(), &"a".repeat(129), 0, 0),
                Err(Error::InvalidDetail)
            );
            assert!(TileAddress::new(layer, &"a".repeat(128), -1, 2).is_ok());
            assert_eq!(
                TileAddress::new(
                    DomainId::parse("SART-019abcde-0000-7000-8000-000000000001").unwrap(),
                    "tile",
                    0,
                    0
                ),
                Err(Error::InvalidDetail)
            );
            for (tiles, limit) in [
                ([2, 3, 0], 3),
                ([2, 2, 3], 2),
                ([2, 2, 0], 1),
                ([MAX_DETAIL_COUNT + 1, 0, 0], 0),
                ([0, 0, 0], MAX_DETAIL_COUNT + 1),
            ] {
                assert_eq!(
                    ResourceCounts::new(tiles, limit, [0; 3], 0),
                    Err(Error::InvalidDetail)
                );
            }
            assert_eq!(
                ResourceCounts::new([1, 1, 1], 1, [2, 3, 4], 8),
                Err(Error::InvalidDetail)
            );
            assert_eq!(
                ResourceCounts::new([1, 1, 1], 1, [u64::MAX, 1, 0], u64::MAX),
                Err(Error::InvalidDetail)
            );
            let changed = ResourceCounts::new([1, 1, 1], 1, [0; 3], 0).unwrap();
            assert_eq!(
                ResourceDetail::new(
                    ResourceDisposition::Rejected,
                    Some(ResourceCode::Validation),
                    None,
                    0,
                    changed
                ),
                Err(Error::InvalidDetail)
            );
            assert_eq!(
                ResourceDetail::new(ResourceDisposition::NoChange, None, None, 0, changed),
                Err(Error::InvalidDetail)
            );
            let empty = ResourceCounts::new([0; 3], 0, [0; 3], 0).unwrap();
            assert_eq!(
                ResourceDetail::new(ResourceDisposition::Changed, None, None, 0, empty),
                Err(Error::InvalidDetail)
            );
            assert_eq!(
                ResourceDetail::new(
                    ResourceDisposition::ReconciliationRequired,
                    None,
                    None,
                    0,
                    empty
                ),
                Err(Error::InvalidDetail)
            );
            let rejected = ResourceDetail::new(
                ResourceDisposition::Rejected,
                Some(ResourceCode::StaleEpoch),
                None,
                10,
                empty,
            )
            .unwrap();
            let mut emitter = observe(0, 0);
            let mut sink = CollectorSink::new(0);
            assert_eq!(
                emitter.emit_resource(
                    event(Outcome::Success),
                    &rejected,
                    &CancellationToken::default(),
                    &mut sink
                ),
                Err(Error::InvalidDetail)
            );
            assert!(sink.frames.is_empty());
        },
    );
}
#[test]
fn resource_v3_reconciliation_cancel_and_reserved_terminal_delivery() {
    bounded_case(
        "resource_v3_reconciliation_cancel_and_reserved_terminal_delivery",
        || {
            let counts = ResourceCounts::new([1, 1, 1], 1, [16, 4, 16], 36).unwrap();
            let pending = ResourceDetail::new(
                ResourceDisposition::ReconciliationRequired,
                Some(ResourceCode::Unavailable),
                None,
                8,
                counts,
            )
            .unwrap();
            let mut emitter = observe(0, 0);
            let mut sink = CollectorSink::new(0);
            let receipt = emitter
                .emit_resource(
                    event(Outcome::Failure(FailureCode::Unavailable)),
                    &pending,
                    &CancellationToken::default(),
                    &mut sink,
                )
                .unwrap();
            assert_eq!(receipt.outcome, Outcome::Failure(FailureCode::Unavailable));
            let frame = collect_resource(&sink.frames[0]);
            assert_eq!((frame.disposition, frame.code), (4, 2));
            assert_eq!(emitter.state().acknowledged, 1);
            assert!(!emitter.state().reconciliation_required);
            let detail =
                ResourceDetail::new(ResourceDisposition::Changed, None, None, 8, counts).unwrap();
            let token = CancellationToken::default();
            token.cancel();
            let mut emitter = observe(0, 0);
            let mut sink = CollectorSink::new(0);
            assert_eq!(
                emitter.emit(
                    event(Outcome::Progress),
                    &CancellationToken::default(),
                    &mut sink
                ),
                Err(Error::Saturated)
            );
            let receipt = emitter
                .emit_resource(event(Outcome::Success), &detail, &token, &mut sink)
                .unwrap();
            assert_eq!(receipt.outcome, Outcome::Canceled);
            let frame = collect_resource(&sink.frames[0]);
            assert_eq!((frame.outcome, frame.disposition, frame.epoch), (4, 1, 8));
            assert_eq!(emitter.state().canceled_delivered, 1);
            for error in [
                DeliveryError::Rejected,
                DeliveryError::Unavailable,
                DeliveryError::Indeterminate,
            ] {
                let mut emitter = observe(0, 0);
                let mut sink = CollectorSink::new(0);
                sink.forced = Some(error);
                assert_eq!(
                    emitter.emit_resource(
                        event(Outcome::Success),
                        &detail,
                        &CancellationToken::default(),
                        &mut sink
                    ),
                    Err(Error::Delivery(error))
                );
                assert_eq!(emitter.state().acknowledged, 0);
                assert!(sink.frames.is_empty());
                if error == DeliveryError::Indeterminate {
                    assert!(emitter.state().reconciliation_required);
                    assert_eq!(
                        emitter.emit_resource(
                            event(Outcome::Success),
                            &detail,
                            &CancellationToken::default(),
                            &mut sink
                        ),
                        Err(Error::ReconciliationRequired)
                    );
                }
            }
        },
    );
}

#[test]
fn resource_v3_nochange_and_closed_rejection_codes() {
    bounded_case("resource_v3_nochange_and_closed_rejection_codes", || {
        let counts = ResourceCounts::new([2, 2, 0], 2, [0, 4, 0], 4).unwrap();
        let detail =
            ResourceDetail::new(ResourceDisposition::NoChange, None, None, 19, counts).unwrap();
        let mut emitter = observe(0, 0);
        let mut sink = CollectorSink::new(0);
        emitter
            .emit_resource(
                event(Outcome::Success),
                &detail,
                &CancellationToken::default(),
                &mut sink,
            )
            .unwrap();
        let frame = collect_resource(&sink.frames[0]);
        assert_eq!(
            (frame.disposition, frame.code, frame.counts, frame.bytes),
            (2, 0, [2, 2, 0], [0, 4, 0])
        );
        for (code, expected) in [
            (ResourceCode::StaleEpoch, 5),
            (ResourceCode::LeaseUnavailable, 6),
            (ResourceCode::UnsupportedFinalization, 7),
        ] {
            let counts = ResourceCounts::new([2, 0, 0], 1, [0; 3], 0).unwrap();
            let address = TileAddress::new(
                DomainId::parse("SLYR-019abcde-0000-7000-8000-000000000001").unwrap(),
                &"a".repeat(128),
                -7,
                -9,
            )
            .unwrap();
            let detail = ResourceDetail::new(
                ResourceDisposition::Rejected,
                Some(code),
                Some(address),
                20,
                counts,
            )
            .unwrap();
            let mut emitter = observe(0, 0);
            let mut sink = CollectorSink::new(0);
            emitter
                .emit_resource(
                    event(Outcome::Failure(FailureCode::Unavailable)),
                    &detail,
                    &CancellationToken::default(),
                    &mut sink,
                )
                .unwrap();
            let frame = collect_resource(&sink.frames[0]);
            assert_eq!(
                (frame.disposition, frame.code, frame.counts),
                (3, expected, [2, 0, 0])
            );
            let address = frame.address.unwrap();
            assert_eq!(address.1.len(), 128);
            assert_eq!((address.2, address.3), (-7, -9));
        }
    });
}
