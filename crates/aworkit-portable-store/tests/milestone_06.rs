use std::{
    collections::BTreeSet,
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use aworkit_portable_store::{
    CanonicalCodec, ExportPolicy, PortableCommit, PortableEvent, PortablePaths, PortableRepository,
    ProjectReference, ProjectionFeed, WorkspaceRoot, canonical_json, plan_rebind, retention_plan,
};
use serde_json::json;

fn root() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("aworkit-m06-{nonce}"));
    fs::create_dir_all(&root).expect("root");
    root
}
fn event(ordinal: u64) -> PortableEvent {
    PortableEvent {
        event_id: format!("event-{ordinal}"),
        chat_id: "chat-1".into(),
        branch_id: "branch-1".into(),
        ordinal,
        kind: "message".into(),
        payload: json!({"text":"portable"}),
    }
}

#[test]
fn canonical_bytes_and_export_omissions_are_deterministic() {
    assert_eq!(
        canonical_json(&json!({"z": 1, "a": [true, null]})).expect("canonical"),
        br#"{"a":[true,null],"z":1}"#
    );
    let codec = CanonicalCodec;
    let first = codec.encode(&event(0)).expect("encode");
    assert_eq!(
        codec.decode::<PortableEvent>(&first).expect("decode"),
        event(0)
    );
    assert!(
        ExportPolicy
            .scrub(&json!({"credentialValue":"never portable"}))
            .is_err()
    );
    let scrubbed = ExportPolicy
        .scrub(&json!({"debugCapture":"discard", "text":"keep"}))
        .expect("scrub");
    assert_eq!(scrubbed.value, json!({"text":"keep"}));
    assert_eq!(scrubbed.omissions.len(), 1);
}

#[test]
fn immutable_commits_reject_conflicts_and_imports_stay_read_only() {
    let root = root();
    let paths = PortablePaths::open(&root).expect("paths");
    let repository = PortableRepository::new(paths.clone());
    let commit = PortableCommit {
        branch_id: "branch-1".into(),
        expected_generation: 0,
        commit_id: "commit-1".into(),
        events: vec![event(0)],
        checkpoint: None,
    };
    let receipt = repository.prepare_publish_verify(&commit).expect("commit");
    assert_eq!(receipt.generation, 1);
    assert!(repository.prepare_publish_verify(&commit).is_err());
    let page = ProjectionFeed::new(paths.clone())
        .read_page(&receipt.head_segment_hash, 0, 10)
        .expect("page");
    assert_eq!(page.events, vec![event(0)]);
    assert!(
        !ProjectionFeed::new(paths)
            .validate_import("sha256:bogus")
            .accepted
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn rebind_and_retention_never_inherit_authority_or_collect_reachable_objects() {
    let required = vec![aworkit_portable_store::CapabilityRequirement {
        logical_id: "model".into(),
        version: "v1".into(),
    }];
    let plan = plan_rebind(&required, &BTreeSet::new());
    assert!(plan.child_branch_required && plan.fresh_authority_required);
    assert_eq!(plan.missing, required);
    let all = ["reachable", "old", "young"]
        .into_iter()
        .map(String::from)
        .collect();
    let reachable = ["reachable"].into_iter().map(String::from).collect();
    let expired = ["old"].into_iter().map(String::from).collect();
    let plan = retention_plan(&all, &reachable, &expired);
    assert_eq!(plan.collectable, vec!["old"]);
    assert!(plan.retained.contains(&"reachable".into()));
}

#[test]
fn project_references_reject_traversal_and_symlink_escapes() {
    let root = root();
    assert!(ProjectReference::parse("../outside").is_err());
    assert!(ProjectReference::parse("C:\\outside").is_err());
    assert!(ProjectReference::parse("folder\\file").is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        symlink(std::env::temp_dir(), root.join("escape")).expect("symlink");
        let workspace = WorkspaceRoot::open(&root).expect("workspace");
        let reference = ProjectReference::parse("escape").expect("reference");
        assert!(workspace.resolve_existing(&reference).is_err());
    }
    fs::remove_dir_all(root).expect("cleanup");
}
