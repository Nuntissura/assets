use std::cell::Cell;

use serde_json::json;

use crate::{ClaimScope, ClaimStatus};

#[derive(Debug)]
struct UnknownStatus {
    message: String,
    token: Box<u8>,
}

#[test]
fn claim_scope_preserves_all_tagged_fields_kind_names_and_scope_ids() {
    for (scope, wire, kind, id) in [
        (ClaimScope::Worktree { worktree_id: "tree".into() },
            json!({"kind": "worktree", "worktree_id": "tree"}), "worktree", "tree"),
        (ClaimScope::Workspace { workspace_id: "workspace".into() },
            json!({"kind": "workspace", "workspace_id": "workspace"}), "workspace", "workspace"),
        (ClaimScope::RichDocument { workspace_id: "workspace".into(), document_id: "doc".into() },
            json!({"kind": "rich_document", "workspace_id": "workspace", "document_id": "doc"}), "rich_document", "workspace/doc"),
        (ClaimScope::GraphMutation { workspace_id: "workspace".into(), graph_id: "graph".into() },
            json!({"kind": "graph_mutation", "workspace_id": "workspace", "graph_id": "graph"}), "graph_mutation", "workspace/graph"),
        (ClaimScope::IndexRun { workspace_id: "workspace".into(), source_root_id: "root".into() },
            json!({"kind": "index_run", "workspace_id": "workspace", "source_root_id": "root"}), "index_run", "workspace/root"),
    ] {
        assert_eq!(scope.kind_str(), kind);
        assert_eq!(scope.scope_id(), id);
        assert_eq!(serde_json::to_value(&scope).unwrap(), wire);
        assert_eq!(serde_json::from_value::<ClaimScope>(wire).unwrap(), scope);
    }
}

#[test]
fn claim_scope_ids_preserve_literal_segments_without_normalization() {
    let scope = ClaimScope::RichDocument { workspace_id: " workspace/ ".into(), document_id: "/doc ".into() };
    assert_eq!(scope.scope_id(), " workspace/ //doc ");
    assert_eq!(ClaimScope::IndexRun { workspace_id: String::new(), source_root_id: String::new() }.scope_id(), "/");
    assert!(serde_json::from_value::<ClaimScope>(json!({"kind": "rich_document", "workspace_id": "workspace"})).is_err());
    assert!(serde_json::from_value::<ClaimScope>(json!({"kind": "unknown", "workspace_id": "workspace"})).is_err());
}

#[test]
fn known_claim_statuses_preserve_wire_names_and_never_call_unknown_mapper() {
    for (status, wire) in [(ClaimStatus::Active, "active"), (ClaimStatus::Held, "held"),
        (ClaimStatus::Released, "released"), (ClaimStatus::Reclaimed, "reclaimed")] {
        assert_eq!(status.as_str(), wire);
        assert_eq!(serde_json::to_value(status).unwrap(), json!(wire));
        assert_eq!(serde_json::from_value::<ClaimStatus>(json!(wire)).unwrap(), status);
        let calls = Cell::new(0);
        let parsed = ClaimStatus::parse(wire, |message| {
            calls.set(calls.get() + 1);
            UnknownStatus { message, token: Box::new(0) }
        }).unwrap();
        assert_eq!(parsed, status);
        assert_eq!(calls.get(), 0);
    }
}

#[test]
fn unknown_claim_status_mapper_runs_once_and_preserves_nonclone_error() {
    for value in ["", "ACTIVE", " active ", "unknown"] {
        let calls = Cell::new(0);
        let token = Box::new(42);
        let address = &*token as *const u8;
        let error = ClaimStatus::parse(value, |message| {
            calls.set(calls.get() + 1);
            UnknownStatus { message, token }
        }).unwrap_err();
        assert_eq!(calls.get(), 1);
        assert_eq!(error.message, format!("unknown claim status: {value}"));
        assert_eq!(&*error.token as *const u8, address);
    }
}

#[test]
fn work_claim_request_public_fields_clone_and_debug_preserve_data() {
    use crate::{AgentLaneIdentity, AgentLaneKind, LocalCloudAttribution, WorkClaimRequest};

    for mt_id in [Some(" MT/ ".to_owned()), None] {
        let scope = ClaimScope::RichDocument {
            workspace_id: " scope workspace/ ".into(),
            document_id: " /document ".into(),
        };
        let lane = AgentLaneIdentity {
            lane_id: " lane/ ".into(),
            actor_id: " actor/ ".into(),
            lane_kind: AgentLaneKind::Local,
            attribution: LocalCloudAttribution::local(" runtime/ ", " model/ "),
        };
        let request = WorkClaimRequest {
            workspace_id: " workspace/ ".into(),
            wp_id: " WP/ ".into(),
            mt_id: mt_id.clone(),
            scope: scope.clone(),
            lane: lane.clone(),
            session_id: " session/ ".into(),
            ttl_seconds: -17,
            reason: " reason/ ".into(),
        };
        let cloned = request.clone();
        for value in [&request, &cloned] {
            assert_eq!(value.workspace_id, " workspace/ ");
            assert_eq!(value.wp_id, " WP/ ");
            assert_eq!(value.mt_id, mt_id);
            assert_eq!(value.scope, scope);
            assert_eq!(value.lane, lane);
            assert_eq!(value.session_id, " session/ ");
            assert_eq!(value.ttl_seconds, -17);
            assert_eq!(value.reason, " reason/ ");
            assert_eq!(
                format!("{value:?}"),
                format!(
                    "WorkClaimRequest {{ workspace_id: {:?}, wp_id: {:?}, mt_id: {:?}, scope: {:?}, lane: {:?}, session_id: {:?}, ttl_seconds: {:?}, reason: {:?} }}",
                    value.workspace_id, value.wp_id, value.mt_id, value.scope,
                    value.lane, value.session_id, value.ttl_seconds, value.reason,
                ),
            );
        }
    }
}
