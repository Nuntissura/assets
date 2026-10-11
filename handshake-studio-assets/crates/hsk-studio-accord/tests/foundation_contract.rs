use hsk_studio_accord::*;
type Result<T> = std::result::Result<T, ValidationError>;
const INPUT: &str = r#"{"envelope_version":1,"schema_id":"hsk.studio.document@1","resource_id":"SDOC-019abcde-0000-7000-8000-000000000001","revision":7,"actor":{"account_id":"account","principal_id":"principal","owner_account_id":"owner","owner_principal_id":"owner-principal","access_space_id":"space","session_id":"session"},"geometry":{"document_unit":"mm","lengths":[{"value":1,"unit":"in"}]},"time_ticks":254016000000,"frame_rate":{"ticks_per_frame":8475675675}}"#;
fn decode(s: &str) -> Result<ValidatedEnvelope> {
    parse_and_validate(s, 7, &CancellationToken::default())
}
// Each canonical test case owns its deadline; panic payloads reach the Cargo harness.
fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(body));
        let _ = sender.send(outcome);
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => resume_unwind(payload),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared 30-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost its deadline worker result")
        }
    }
}
#[test]
fn canonical_transport_round_trip_and_receipt() {
    bounded_case("canonical_transport_round_trip_and_receipt", move || {
        let result = decode(INPUT).unwrap();
        assert_eq!(result.geometry()[0].value(), 25.4);
        assert_eq!(result.geometry()[0].unit(), Unit::Millimetres);
        assert_eq!(result.time().value(), TICKS_PER_SECOND);
        assert_eq!(
            result.normalization(),
            FrameRateNormalization {
                source_ticks_per_frame: 8_475_675_675,
                canonical_ticks_per_frame: 8_475_667_200,
                changed: true
            }
        );
        let round_trip = decode(&result.to_json()).unwrap();
        assert_eq!(round_trip.geometry(), result.geometry());
        assert_eq!(round_trip.resource_id(), result.resource_id());
        assert_eq!(round_trip.frame_rate(), result.frame_rate());
        assert!(!round_trip.normalization().changed);
    });
}

#[test]
fn invalid_prefix_and_uuid_version() {
    bounded_case("invalid_prefix_and_uuid_version", move || {
        assert_eq!(
            decode(&INPUT.replace("SDOC-", "SLYR-")).unwrap_err(),
            ValidationError::PrefixMismatch
        );
        assert_eq!(
            DomainId::parse("SDOC-019abcde-0000-4000-8000-000000000001"),
            Err(ValidationError::InvalidId)
        );
    });
}

#[test]
fn nonfinite_geometry_rejects() {
    bounded_case("nonfinite_geometry_rejects", move || {
        assert_eq!(
            decode(&INPUT.replace("\"value\":1", "\"value\":1e999")).unwrap_err(),
            ValidationError::NonFinite
        );
        assert_eq!(
            Length::new(f64::NAN, Unit::Pixels),
            Err(ValidationError::NonFinite)
        );
    });
}

#[test]
fn checked_tick_overflow() {
    bounded_case("checked_tick_overflow", move || {
        assert_eq!(
            Ticks::from_seconds(u64::MAX),
            Err(ValidationError::TickOverflow)
        );
        assert_eq!(
            Ticks::new(u64::MAX).checked_add(Ticks::new(1)),
            Err(ValidationError::TickOverflow)
        );
        let (r, _) = FrameRate::from_import(10_584_000_000).unwrap();
        assert_eq!(
            Ticks::checked_frames(u64::MAX, r),
            Err(ValidationError::TickOverflow)
        );
    });
}

#[test]
fn tick_subtraction_ordering_and_hash() {
    bounded_case("tick_subtraction_ordering_and_hash", move || {
        let (a, b) = (Ticks::new(1000), Ticks::new(400));
        assert_eq!(a.checked_sub(b), Ok(Ticks::new(600)));
        assert_eq!(a.checked_sub(a), Ok(Ticks::new(0)));
        assert_eq!(b.checked_sub(a), Err(ValidationError::TickOverflow));
        assert_eq!(b.saturating_sub(a), Ticks::new(0));
        assert_eq!(a.saturating_sub(b), Ticks::new(600));
        assert_eq!(a.checked_sub(b).and_then(|d| d.checked_add(b)), Ok(a));
        assert!(b < a && a > b && a.max(b) == a);
        let mut sorted = vec![a, Ticks::new(0), b];
        sorted.sort();
        assert_eq!(sorted, [Ticks::new(0), b, a]);
        let set: std::collections::HashSet<Ticks> = [a, b, a].into_iter().collect();
        assert_eq!(set.len(), 2);
    });
}

#[test]
fn unknown_version_and_required_field() {
    bounded_case("unknown_version_and_required_field", move || {
        assert_eq!(
            decode(&INPUT.replace("envelope_version\":1", "envelope_version\":2")).unwrap_err(),
            ValidationError::UnsupportedVersion
        );
        assert_eq!(
            decode(&INPUT.replacen('{', "{\"required_future\":1,", 1)).unwrap_err(),
            ValidationError::UnknownField
        );
        assert_eq!(
            decode(&INPUT.replace("document@1", "document@2")).unwrap_err(),
            ValidationError::UnknownSchema
        );
    });
}

#[test]
fn cancel_and_revision_leave_input_immutable() {
    bounded_case("cancel_and_revision_leave_input_immutable", move || {
        let input = INPUT.to_string();
        let saved = input.clone();
        let token = CancellationToken::default();
        token.clone().cancel();
        assert_eq!(
            parse_and_validate(&input, 7, &token).unwrap_err(),
            ValidationError::Canceled
        );
        assert_eq!(input, saved);
        assert_eq!(
            parse_and_validate(&input, 8, &CancellationToken::default()).unwrap_err(),
            ValidationError::RevisionMismatch
        );
    });
}

#[test]
fn mixed_units_and_missing_resolution() {
    bounded_case("mixed_units_and_missing_resolution", move || {
        let mixed = INPUT.replace(
            "{\"value\":1,\"unit\":\"in\"}",
            "{\"value\":1,\"unit\":\"in\"},{\"value\":1,\"unit\":\"mm\"}",
        );
        assert_eq!(decode(&mixed).unwrap_err(), ValidationError::MixedUnits);
        assert_eq!(
            decode(&INPUT.replace("\"unit\":\"in\"", "\"unit\":\"px\"")).unwrap_err(),
            ValidationError::MissingResolution
        );
        assert_eq!(
            Length::new(96.0, Unit::Pixels)
                .unwrap()
                .convert(Unit::Inches, Some(96.0))
                .unwrap()
                .value(),
            1.0
        );
    });
}

#[test]
fn normative_rate_table_and_both_legacy_receipts() {
    bounded_case("normative_rate_table_and_both_legacy_receipts", move || {
        assert_eq!(BROADCAST_TICKS_PER_FRAME.len(), 11);
        for rate in BROADCAST_TICKS_PER_FRAME {
            let (r, receipt) = FrameRate::from_import(*rate).unwrap();
            assert_eq!(r.ticks_per_frame(), *rate);
            assert!(!receipt.changed)
        }
        assert_eq!(
            FrameRate::from_import(10_594_594_594)
                .unwrap()
                .0
                .ticks_per_frame(),
            10_594_584_000
        );
        assert_eq!(
            FrameRate::from_import(0),
            Err(ValidationError::UnsupportedFrameRate)
        );
    });
}

#[test]
fn json_duplicate_trailing_and_integer_precision() {
    bounded_case("json_duplicate_trailing_and_integer_precision", move || {
        assert_eq!(
            decode(&INPUT.replacen('{', "{\"revision\":7,", 1)).unwrap_err(),
            ValidationError::DuplicateField
        );
        assert_eq!(
            decode(&(INPUT.to_string() + " trailing")).unwrap_err(),
            ValidationError::InvalidJson
        );
        assert_eq!(
            decode(&INPUT.replace("revision\":7", "revision\":7.0")).unwrap_err(),
            ValidationError::InvalidType
        );
        assert_eq!(
            decode(&INPUT.replace("254016000000", &u64::MAX.to_string()))
                .unwrap()
                .time()
                .value(),
            u64::MAX
        );
    });
}

#[test]
fn unicode_context_and_surrogate_handling() {
    bounded_case("unicode_context_and_surrogate_handling", move || {
        let input = INPUT.replace("\"account\"", r#""account\uD83D\uDE00""#);
        assert_eq!(decode(&input).unwrap().actor().account_id(), "account😀");
        assert_eq!(
            decode(&INPUT.replace("\"account\"", r#""account\uDE00""#)).unwrap_err(),
            ValidationError::InvalidJson
        );
    });
}

#[test]
fn bounded_input_and_explicit_context() {
    bounded_case("bounded_input_and_explicit_context", move || {
        assert_eq!(
            decode(&" ".repeat(MAX_INPUT_BYTES + 1)).unwrap_err(),
            ValidationError::InputLimit
        );
        assert_eq!(
            decode(&INPUT.replace("\"principal\"", "\"\"")).unwrap_err(),
            ValidationError::InvalidContext
        );
        assert_eq!(
            Length::new(-1.0, Unit::Points).unwrap().nonnegative(),
            Err(ValidationError::OutOfRange)
        );
    });
}

#[test]
fn unsupported_identity_mapping_is_not_guessed() {
    bounded_case("unsupported_identity_mapping_is_not_guessed", move || {
        assert_eq!(
            decode(&INPUT.replace("document@1", "gradient@1")).unwrap_err(),
            ValidationError::UnsupportedIdentityMapping
        );
        assert_eq!(CANONICAL_SCHEMAS.len(), 37);
        assert!(CANONICAL_SCHEMAS.contains(&SCHEMA_STUDIO_LAYER_GRAPH));
        assert!(CANONICAL_SCHEMAS.contains(&SCHEMA_STUDIO_RASTER_TILE));
    });
}
