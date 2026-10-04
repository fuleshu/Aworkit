use super::*;
use serde_json::json;

#[test]
fn binding_hash_canonicalizes_nested_objects() {
    assert_eq!(
        digest(&json!({"executable":"python","env":{"a":"1","b":"2"}})),
        digest(&json!({"env":{"b":"2","a":"1"},"executable":"python"}))
    );
}

#[test]
fn project_grant_covers_different_scripts_but_not_other_tools_bindings_or_projects() {
    use crate::runtime::tool_loop::approval_policy::project_grant;
    use crate::runtime::tool_loop::{WorkflowToolBindingV1, freeze_file_tool_bindings};
    use aworkit_capability_host::ModelToolCallV1;
    let context = ApprovalContext {
        project_key: Some("project.with-native-workspace".into()),
        ..Default::default()
    };
    let mut binding = freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
        options: Default::default(),
        capability_id: "tool.python.host".into(),
        configuration: json!({"authorityMode":"host_python","requiresApproval":true,
            "isolatedInterpreter":true,"timeoutSeconds":30,"maximumOutputBytes":4096}),
        credential_bindings: vec![],
        definition: None,
    }])
    .unwrap()
    .remove(0);
    let call = ModelToolCallV1 {
        call_id: "python.first".into(),
        capability_id: binding.capability_id.clone(),
        provider_call_id: None,
        provider_context: None,
        name: binding.provider_name.clone(),
        arguments: json!({"script":"print(1)"}),
    };
    let first = project_grant(&context, &binding, &call).unwrap();
    let mut next = call.clone();
    next.call_id = "python.second".into();
    next.arguments = json!({"script":"print(2 + 3)"});
    assert_eq!(first, project_grant(&context, &binding, &next).unwrap());
    assert_eq!(first.scope, "Python scripts in this project");
    binding.description = "New documentation".into();
    binding.options.instructions = Some("New instructions".into());
    assert_eq!(first, project_grant(&context, &binding, &next).unwrap());
    let other = ApprovalContext {
        project_key: Some("other-project".into()),
        ..context.clone()
    };
    assert_ne!(first.id, project_grant(&other, &binding, &next).unwrap().id);
    assert!(project_grant(&ApprovalContext::default(), &binding, &next).is_none());
    binding.options.executable = Some("C:\\another\\python.exe".into());
    assert_ne!(
        first.id,
        project_grant(&context, &binding, &next).unwrap().id
    );
    binding.capability_id = "tool.shell.host".into();
    next.capability_id = binding.capability_id.clone();
    assert_ne!(
        first.id,
        project_grant(&context, &binding, &next).unwrap().id
    );
}

#[test]
fn legacy_project_grants_migrate_deduplicate_and_stay_revoked() {
    let root = tempfile::TempDir::new().unwrap();
    let database = root.path().join("approvals.sqlite3");
    let store = ApprovalStore::open(&database).unwrap();
    for capability in ["tool.python.host", "tool.shell.host", "mcp://fixture/echo"] {
        for script in ["print(1)", "print(2)"] {
            let mut grant = ProjectApprovalGrant {
                id: String::new(),
                project_key: "project.fixture".into(),
                project_name: "Fixture".into(),
                capability_id: capability.into(),
                binding_hash: digest(&capability),
                scope: "This exact action in this project".into(),
                action_summary: script.into(),
                action_hash: digest(&json!({"script":script})),
            };
            grant.id = digest(&(&grant.project_key, &grant.binding_hash, &grant.action_hash));
            store
                .resolve(
                    &grant.id,
                    &ApprovalResolution {
                        choice: ApprovalChoice::AlwaysApproveInProject,
                        reason: None,
                        filesystem: None,
                    },
                    Some(&grant),
                )
                .unwrap();
        }
    }
    assert_eq!(store.grants().unwrap().len(), 6);
    drop(store);
    let store = ApprovalStore::open(&database).unwrap();
    let grants = store.grants().unwrap();
    assert_eq!(grants.len(), 3);
    for grant in grants {
        assert_eq!(grant.action_hash, "project_tool");
        assert!(grant.action_summary.contains("different arguments"));
        store.revoke(&grant.id).unwrap();
    }
    drop(store);
    assert!(
        ApprovalStore::open(&database)
            .unwrap()
            .grants()
            .unwrap()
            .is_empty()
    );
}

/// Approval digests are stored: a grant id and a frozen binding hash are
/// re-derived on every read, so the shared digest rule must produce the exact
/// bytes the previous spelling produced. The expected values below were
/// captured from that spelling before the rule was shared.
#[test]
fn approval_grant_digests_keep_their_bytes() {
    use crate::runtime::tool_loop::{WorkflowToolBindingV1, freeze_file_tool_bindings};
    let binding = freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
        options: Default::default(),
        capability_id: "tool.python.host".into(),
        configuration: json!({"authorityMode":"host_python","requiresApproval":true,
            "isolatedInterpreter":true,"timeoutSeconds":30,"maximumOutputBytes":4096}),
        credential_bindings: vec![],
        definition: None,
    }])
    .unwrap()
    .remove(0);
    let mut authority = binding.clone();
    authority.description.clear();
    authority.options.instructions = None;
    assert_eq!(
        digest(&binding),
        "82081402ddceb4157f9e1a06f6100c41a51ac7781d895ab5fff6e1fc98112424"
    );
    assert_eq!(
        digest(&authority),
        "132783a720dd9e64731357da780317ae177097fbdef261b45ef8e02c1bc459da"
    );
    // The production tuple identity of one project grant.
    assert_eq!(
        digest(&("project.fixture", "abc123", "project_tool")),
        "1d61d53401e7bf411edbd6e171f2b28feae5da87ee6c0fd5148f59faebd481d6"
    );
    // Nested objects, integers, booleans, nulls and arrays.
    assert_eq!(
        digest(&json!({"timeoutSeconds":30,"maximumOutputBytes":4096,"revision":7,
            "nested":{"b":true,"a":null},"list":[1,2,3]})),
        "6e42b2573afc82a35229d06a6d7871cf224cc6edf7da00e9bdd39ad65ed8840c"
    );
    // Non-ASCII and escaped characters.
    assert_eq!(
        digest(&json!({"kanji":"漢","quote":"\"","esc":"a\\nb"})),
        "dff5b364dc764f9a79d838d0e29771ad7e278b27ae1de58c5b7ac5ca14d8a928"
    );
    assert_eq!(
        digest(&Vec::<String>::new()),
        "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
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
        filesystem: None,
    };
    store.resolve("decision.1", &denied, None).unwrap();
    assert!(
        store
            .resolve("decision.1", &ApprovalResolution::once(true), None)
            .is_err()
    );
    assert_eq!(store.resolution("decision.1").unwrap(), Some(denied));
}
