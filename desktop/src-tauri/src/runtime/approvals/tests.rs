use super::*;
use serde_json::json;

#[test]
fn exact_action_scope_preserves_arguments_and_canonicalizes_nested_objects() {
    assert_eq!(
        action_scope(
            "tool.shell.host",
            &json!({"command":"git status","env":{"a":"1","b":"2"}})
        )
        .1,
        action_scope(
            "tool.shell.host",
            &json!({"env":{"b":"2","a":"1"},"command":"git status"})
        )
        .1
    );
    assert_ne!(
        action_scope("tool.shell.host", &json!({"command":"git status"})).1,
        action_scope(
            "tool.shell.host",
            &json!({"command":"git status; rm -rf ."})
        )
        .1
    );
    assert_ne!(
        action_scope("tool.python.host", &json!({"script":"print('ok')"})).1,
        action_scope(
            "tool.python.host",
            &json!({"script":"import shutil; shutil.rmtree('.')"})
        )
        .1
    );
}

#[test]
fn modes_are_isolated_and_survive_restart() {
    let root = tempfile::TempDir::new().unwrap();
    let database = root.path().join("approvals.sqlite3");
    let store = ApprovalStore::open(&database).unwrap();
    store
        .set_mode("chat.first", ApprovalMode::FullAccess)
        .unwrap();
    drop(store);
    let store = ApprovalStore::open(&database).unwrap();
    assert_eq!(
        store
            .mode("chat.first", ApprovalMode::AskForApproval)
            .unwrap(),
        ApprovalMode::FullAccess
    );
    assert_eq!(
        store
            .mode("chat.second", ApprovalMode::AskForApproval)
            .unwrap(),
        ApprovalMode::AskForApproval
    );
}

#[test]
fn conflicting_durable_decisions_are_rejected() {
    let root = tempfile::TempDir::new().unwrap();
    let store = ApprovalStore::open(&root.path().join("approvals.sqlite3")).unwrap();
    let denied = ApprovalResolution {
        choice: ApprovalChoice::Deny,
        reason: Some("Preserve the file.".into()),
    };
    store.resolve("decision.1", &denied, None).unwrap();
    assert!(
        store
            .resolve("decision.1", &ApprovalResolution::once(true), None)
            .is_err()
    );
    assert_eq!(store.resolution("decision.1").unwrap(), Some(denied));
}
