use handpick::{RecoverySlots, RecoverySlotError};

#[test]
fn recovery_default_preserves_legacy_zero_and_preference_never_duplicates() {
    assert_eq!(RecoverySlots::default().current(), Some(0));
    for preferred in 0..8 {
        let mut policy = RecoverySlots::new(preferred).unwrap();
        let mut attempts = Vec::new();
        while let Some(slot) = policy.current() { attempts.push(slot); policy.busy(slot).unwrap(); }
        assert_eq!(attempts[0], preferred);
        assert_eq!(attempts.len(), 8);
        attempts.sort_unstable(); assert_eq!(attempts, (0..8).collect::<Vec<_>>());
        assert!(policy.exhausted());
    }
}

#[test]
fn recovery_rejects_invalid_or_out_of_order_outcomes_without_advancing() {
    assert_eq!(RecoverySlots::new(8), Err(RecoverySlotError::InvalidSlot));
    let mut policy = RecoverySlots::new(3).unwrap(); let before = policy.clone();
    assert_eq!(policy.busy(2), Err(RecoverySlotError::OutOfOrder));
    assert_eq!(policy.acquired(2), Err(RecoverySlotError::OutOfOrder));
    assert_eq!(policy.busy(8), Err(RecoverySlotError::InvalidSlot));
    assert_eq!(policy, before);
    assert_eq!(policy.busy(3), Ok(Some(0)));
    assert_eq!(policy.busy(3), Err(RecoverySlotError::OutOfOrder));
    assert_eq!(policy.current(), Some(0));
}

#[test]
fn recovery_acquisition_and_exhaustion_are_distinct_terminal_states() {
    let mut policy = RecoverySlots::new(7).unwrap();
    policy.acquired(7).unwrap(); assert_eq!(policy.current(), None);
    assert_eq!(policy.acquired_slot(), Some(7)); assert!(!policy.exhausted());
    assert_eq!(policy.busy(7), Err(RecoverySlotError::Closed));
    assert_eq!(policy.acquired(7), Err(RecoverySlotError::Closed));
    let mut exhausted = RecoverySlots::default();
    for slot in 0..8 { exhausted.busy(slot).unwrap(); }
    assert!(exhausted.exhausted()); assert_eq!(exhausted.acquired_slot(), None);
    assert_eq!(exhausted.busy(0), Err(RecoverySlotError::Closed));
    assert_eq!(exhausted.acquired(0), Err(RecoverySlotError::Closed));
}
