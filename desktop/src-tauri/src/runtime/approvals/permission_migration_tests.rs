use super::*;
use crate::runtime::tool_loop::{WorkflowToolBindingV1, freeze_file_tool_bindings};
use rusqlite::params;
use serde_json::json;

#[test]
fn migration_requires_exact_evidence_preserves_authority_and_never_revives_revocations() {
    for capability in ["tool.shell.host", "tool.python.host"] {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("invocations.sqlite3");
        let store = ApprovalStore::open(&database).unwrap();
        let mut configuration = json!({"authorityMode":if capability.contains("python") {"host_python"} else {"host_shell"},
            "requiresApproval":true,"timeoutSeconds":30,"maximumOutputBytes":4096});
        if capability.contains("python") {
            configuration["isolatedInterpreter"] = json!(true);
        }
        let binding = freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
            options: Default::default(),
            capability_id: capability.into(),
            configuration,
            credential_bindings: vec![],
            definition: None,
        }])
        .unwrap()
        .remove(0);
        let mut grant = ProjectApprovalGrant {
            id: String::new(),
            project_key: "p1".into(),
            project_name: "Test".into(),
            capability_id: capability.into(),
            scope: String::new(),
            action_summary: String::new(),
            binding_hash: digest(&binding),
            action_hash: String::new(),
        };
        grant.set_tool_scope();
        let connection = store.connection().unwrap();
        connection
            .execute(
                "INSERT INTO approval_project_grants VALUES (?1,?2)",
                params![grant.id, serde_json::to_string(&grant).unwrap()],
            )
            .unwrap();
        connection.execute_batch("CREATE TABLE semantic_events (chat_id TEXT,branch_id TEXT,sequence INTEGER,kind TEXT,payload TEXT)").unwrap();
        let evidence =
            |project: &str, binding: &crate::runtime::tool_loop::StoredFileToolBindingV1| {
                json!({"record":{"approvals":{"projectKey":project},"toolBindings":[binding]}})
                    .to_string()
            };
        connection.execute("INSERT INTO semantic_events VALUES ('pipeline.execution','main',1,'pipeline.execution-prepared',?1)", [evidence("other",&binding)]).unwrap();
        assert_eq!(
            ApprovalStore::open(&database).unwrap().grants().unwrap(),
            vec![grant.clone()],
            "other project cannot prove a grant"
        );
        let mut current = binding.clone();
        current.description = "Updated help".into();
        current.options.instructions = Some("Updated guidance".into());
        connection.execute("INSERT INTO semantic_events VALUES ('pipeline.execution','main',2,'pipeline.execution-prepared',?1)", [evidence("p1",&current)]).unwrap();
        assert_eq!(
            ApprovalStore::open(&database).unwrap().grants().unwrap(),
            vec![grant.clone()],
            "similar current binding is not proof of the old hash"
        );
        connection.execute("INSERT INTO semantic_events VALUES ('pipeline.execution','main',3,'pipeline.execution-prepared',?1)", [evidence("p1",&binding)]).unwrap();
        let upgraded = ApprovalStore::open(&database)
            .unwrap()
            .grants()
            .unwrap()
            .remove(0);
        assert_eq!(
            upgraded.binding_hash,
            permission_identity::binding_hash(&current)
        );
        assert!(upgraded.binding_hash.starts_with("execution-v1:"));
        assert_eq!(
            ApprovalStore::open(&database).unwrap().grants().unwrap(),
            vec![upgraded.clone()]
        );
        for field in [
            "configuration",
            "inputSchema",
            "requiresApproval",
            "options",
            "limit",
        ] {
            let mut changed = serde_json::to_value(&current).unwrap();
            match field {
                "configuration" => changed[field]["timeoutSeconds"] = json!(31),
                "inputSchema" => changed[field]["description"] = json!("changed contract"),
                "requiresApproval" => changed[field] = json!(false),
                "options" => changed[field]["executable"] = json!("C:\\different.exe"),
                "limit" => changed[field]["timeout_seconds"] = json!(31),
                _ => unreachable!(),
            }
            let changed = serde_json::from_value(changed).unwrap();
            assert_ne!(
                upgraded.binding_hash,
                permission_identity::binding_hash(&changed),
                "{field}"
            );
        }
        store.revoke(&upgraded.id).unwrap();
        assert!(
            ApprovalStore::open(&database)
                .unwrap()
                .grants()
                .unwrap()
                .is_empty()
        );
    }
}
