use fforager_contracts::{
    DurabilityClass, JournalPayload, JournalRecord, JournalRecordError, ReconcileState,
};
use serde_json::{Value, json};

fn valid_journal_record() -> Value {
    json!({
        "schema": {"major": 1, "minor": 0},
        "job_id": "job_1",
        "producer_instance": "producer_1",
        "sequence": 1,
        "prior_record_hash": null,
        "payload_checksum": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "durability": "durable",
        "payload": {
            "kind": "output_checkpoint",
            "body": {
                "position": {
                    "received_bytes": 1024,
                    "validated_bytes": 768,
                    "durable_bytes": 512
                }
            }
        }
    })
}

#[test]
fn durable_wire_rejects_unknown_variant_fields_and_sequence_zero() {
    let payload = json!({
        "kind": "fragment_verified",
        "body": {
            "sequence": 1,
            "bytes": 16,
            "checksum": "checksum",
            "undeclared_nested": true
        }
    });
    assert!(
        serde_json::from_value::<JournalPayload>(payload).is_err(),
        "JournalPayload unknown body field must fail closed"
    );

    let reconcile = json!({
        "kind": "output_without_archive",
        "final_identity": "final_1",
        "undeclared_nested": true
    });
    assert!(
        serde_json::from_value::<ReconcileState>(reconcile).is_err(),
        "ReconcileState unknown variant field must fail closed"
    );

    let mut record = valid_journal_record();
    record["undeclared_top_level"] = json!(true);
    assert!(
        serde_json::from_value::<JournalRecord>(record).is_err(),
        "JournalRecord unknown top-level field must fail closed"
    );

    let mut record = valid_journal_record();
    record["sequence"] = json!(0);
    assert!(
        serde_json::from_value::<JournalRecord>(record).is_err(),
        "JournalRecord sequence zero must fail closed"
    );
}

#[test]
fn valid_journal_wire_round_trips_without_shape_change() {
    let expected = valid_journal_record();
    let record: JournalRecord =
        serde_json::from_value(expected.clone()).expect("canonical journal record must decode");
    assert_eq!(record.sequence, 1);
    assert_eq!(record.durability, DurabilityClass::Durable);
    assert!(record.validate().is_ok());
    assert_eq!(
        serde_json::to_value(record).expect("canonical journal record must encode"),
        expected
    );
}

#[test]
fn constructed_sequence_zero_record_fails_explicit_validation() {
    let mut record: JournalRecord = serde_json::from_value(valid_journal_record())
        .expect("canonical journal record must decode");
    record.sequence = 0;
    assert_eq!(record.validate(), Err(JournalRecordError::SequenceZero));
}
