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
