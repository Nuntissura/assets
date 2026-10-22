use handshake_swarm_state::ModelSessionState;

#[test]
fn happy_path_transitions_are_legal() {
    use ModelSessionState::*;
    assert!(Queued.can_transition(Loading));
    assert!(Loading.can_transition(Ready));
    assert!(Ready.can_transition(Generating));
    assert!(Generating.can_transition(Completed));
}

#[test]
fn terminal_states_are_sinks() {
    use ModelSessionState::*;
    for to in [Queued, Loading, Ready, Generating, Cancelling] {
        assert!(!Completed.can_transition(to));
        assert!(!Failed.can_transition(to));
        assert!(!Cancelled.can_transition(to));
    }
}

#[test]
fn cannot_skip_loading() {
    use ModelSessionState::*;
    assert!(!Queued.can_transition(Ready));
    assert!(!Queued.can_transition(Generating));
}

#[test]
fn slot_occupancy_excludes_queued_and_terminal() {
    use ModelSessionState::*;
    assert!(!Queued.occupies_slot());
    assert!(Loading.occupies_slot());
    assert!(Generating.occupies_slot());
    assert!(!Completed.occupies_slot());
    assert!(!Cancelled.occupies_slot());
}
