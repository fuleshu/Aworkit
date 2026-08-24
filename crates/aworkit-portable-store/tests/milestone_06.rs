use std::collections::{BTreeMap, BTreeSet};

use aworkit_portable_store::{
    ArtifactDescriptor, ArtifactError, ArtifactStore, BranchManifest, CanonicalCodec,
    ChildContinuationManifestRequestV1, CodecError, CommitError, CommitFaultPoint, ExportError,
    ExportPolicy, GitFactAvailabilityV1, IntegrityError, IntegrityIssueV1, LocalCapabilityV1,
    ManifestCatalog, ManifestEnvelopeV1, NonDestructiveRepairProposalV1, OmissionFact,
    PortableCapabilityRequirementV1, PortableCheckpoint, PortableCommit, PortableCommitContextV1,
    PortableEvent, PortableFrozenSnapshotV1, PortableGitFactsV1, PortableIntegrityEngine,
    PortablePaths, PortableProjectionEvidenceV1, PortableProvenanceV1, PortableRepository,
    PortableSegment, PortableTransitionRecordV1, ProjectReference, ProjectionFeed,
    ReachabilityScanV1, RebindResolutionV1, RepositoryCompatibility, RepositoryManifest,
    RetentionError, SessionManifest, WorkspaceError, WorkspaceRoot, canonical_json,
    plan_child_continuation, plan_continuation_rebind, portable_snapshot_hash, protocol_value_hash,
    retention_plan_two_phase,
};
use aworkit_protocol::{
    CommitBatchV1, EventV1, HistoryBackendV1, PortableCanonicalCommitPort, PortablePrepareV1,
    StableId,
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("stable test ID")
}

fn hash(fill: char) -> String {
    format!("sha256:{}", fill.to_string().repeat(64))
}

fn event(branch: &str, ordinal: u64, payload: Value) -> PortableEvent {
    PortableEvent {
        event_id: format!("event.{ordinal}"),
        chat_id: "chat.1".into(),
        branch_id: branch.into(),
        ordinal,
        kind: "message".into(),
        payload,
    }
}

fn context(artifacts: Vec<ArtifactDescriptor>) -> PortableCommitContextV1 {
    let snapshot = json!({
        "workflow": {"nodes": ["start", "finish"]},
        "settings": {"temperature": 0}
    });
    PortableCommitContextV1 {
        frozen_snapshot: Some(PortableFrozenSnapshotV1 {
            schema_version: 1,
            snapshot_hash: portable_snapshot_hash(&snapshot).expect("snapshot hash"),
            workflow_hash: hash('a'),
            portable_snapshot: snapshot,
            requirements: vec![PortableCapabilityRequirementV1 {
                logical_id: "model.primary".into(),
                version: "v1".into(),
                configuration_hash: hash('b'),
            }],
        }),
        provenance: PortableProvenanceV1 {
            git: PortableGitFactsV1 {
                head_commit: Some("c".repeat(40)),
                dirty_state_digest: Some(hash('d')),
                worktree_identity: Some(hash('e')),
                unavailable_reason: None,
            },
            workflow_revision_hash: hash('f'),
            configuration_revision_hashes: vec![hash('1'), hash('2')],
            redaction_profile_hash: ExportPolicy.policy_hash().into(),
            artifact_refs: artifacts.iter().map(|value| value.digest.clone()).collect(),
            artifact_metadata: artifacts,
            omissions: vec![],
        },
    }
}

fn commit(
    branch: &str,
    commit_id: &str,
    generation: u64,
    ordinal: u64,
    payload: Value,
) -> PortableCommit {
    PortableCommit {
        branch_id: branch.into(),
        expected_generation: generation,
        commit_id: commit_id.into(),
        context: Some(context(vec![])),
        events: vec![event(branch, ordinal, payload)],
        checkpoint: None,
    }
}

#[test]
fn canonical_records_segments_snapshots_and_export_policy_are_byte_exact() {
    assert_eq!(
        canonical_json(&json!({"z": 1.0, "a": [true, null]})).unwrap(),
        br#"{"a":[true,null],"z":1}"#
    );
    let utf16_order =
        String::from_utf8(canonical_json(&json!({"\u{e000}": 1, "\u{1f600}": 2})).unwrap())
            .unwrap();
    assert!(utf16_order.find('😀').unwrap() < utf16_order.find('\u{e000}').unwrap());

    let codec = CanonicalCodec;
    let canonical = codec.encode(&json!({"a":1,"b":2})).unwrap();
    assert_eq!(
        codec.decode::<Value>(&canonical).unwrap(),
        json!({"a":1,"b":2})
    );
    assert!(matches!(
        codec.decode::<Value>(b"{\"b\":2,\"a\":1}\n"),
        Err(CodecError::NonCanonical)
    ));
    assert!(matches!(
        codec.decode::<Value>(b"{\"a\":1}\r\n"),
        Err(CodecError::InvalidFraming)
    ));
    assert!(codec.decode::<Value>(b"{\"a\":1,\"a\":2}\n").is_err());

    let segment = PortableSegment {
        parent_segment_hash: None,
        base_checkpoint_hash: None,
        first_ordinal: 0,
        context: Some(context(vec![])),
        events: vec![event("branch.1", 0, json!({"text":"portable"}))],
    };
    let bytes = codec.encode_segment(&segment).unwrap();
    assert_eq!(codec.decode_segment(&bytes).unwrap(), segment);
    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 3);
    let mut tampered = bytes.clone();
    let index = tampered
        .windows(b"portable".len())
        .rposition(|window| window == b"portable")
        .unwrap();
    tampered[index] = b'P';
    assert!(matches!(
        codec.decode_segment(&tampered),
        Err(CodecError::TrailerMismatch)
    ));
    let mut gap = segment.clone();
    gap.events[0].ordinal = 1;
    assert!(matches!(
        codec.encode_segment(&gap),
        Err(CodecError::OrdinalGap)
    ));
    let mut raw_reasoning = segment;
    raw_reasoning.events[0].kind = "reasoning_raw".into();
    assert!(matches!(
        codec.encode_segment(&raw_reasoning),
        Err(CodecError::InvalidEventKind)
    ));

    assert!(matches!(
        ExportPolicy.scrub(&json!({"credentialValue":"never"})),
        Err(ExportError::ForbiddenField(_))
    ));
    assert!(
        ExportPolicy
            .scrub(&json!({"nested":{"reasoningRaw":"never"}}))
            .is_err()
    );
    assert!(
        ExportPolicy
            .scrub(&json!({"text":"Bearer abcdefghijklmnopqrstuvwxyz"}))
            .is_err()
    );
    assert!(
        ExportPolicy
            .scrub(&json!({"path":"/home/user/private"}))
            .is_err()
    );
    let scrubbed = ExportPolicy
        .scrub(&json!({"traceCapture":"omit", "text":"keep"}))
        .unwrap();
    assert_eq!(scrubbed.value, json!({"text":"keep"}));
    assert_eq!(scrubbed.omissions[0].pointer, "/traceCapture");

    let mut bad_context = context(vec![]);
    bad_context.frozen_snapshot.as_mut().unwrap().snapshot_hash = hash('0');
    let mut invalid = raw_reasoning;
    invalid.events[0].kind = "message".into();
    invalid.context = Some(bad_context);
    assert!(matches!(
        codec.encode_segment(&invalid),
        Err(CodecError::InvalidProvenance)
    ));
}

#[test]
fn workspace_is_root_anchored_case_safe_symlink_safe_and_git_read_only() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("data.txt"), b"inside").unwrap();
    std::fs::create_dir_all(project.join(".git/refs/heads")).unwrap();
    std::fs::write(project.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
    std::fs::write(project.join(".git/refs/heads/main"), "a".repeat(40)).unwrap();
    let before_head = std::fs::read(project.join(".git/HEAD")).unwrap();
    let before_ref = std::fs::read(project.join(".git/refs/heads/main")).unwrap();

    assert!(ProjectReference::parse("../outside").is_err());
    assert!(ProjectReference::parse("C:\\outside").is_err());
    assert!(ProjectReference::parse("folder\\file").is_err());
    let workspace = WorkspaceRoot::open(&project).unwrap();
    assert_eq!(
        workspace
            .read(&ProjectReference::parse("data.txt").unwrap())
            .unwrap(),
        b"inside"
    );
    let git = workspace.inspect_git_read_only().unwrap();
    assert!(matches!(
        git.availability,
        GitFactAvailabilityV1::Partial { .. }
    ));
    assert_eq!(git.branch_reference.as_deref(), Some("refs/heads/main"));
    assert_eq!(git.head_commit.as_deref(), Some("a".repeat(40).as_str()));
    assert_eq!(
        std::fs::read(project.join(".git/HEAD")).unwrap(),
        before_head
    );
    assert_eq!(
        std::fs::read(project.join(".git/refs/heads/main")).unwrap(),
        before_ref
    );

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside", project.join("escape")).unwrap();
        assert!(matches!(
            workspace.resolve_existing(&ProjectReference::parse("escape").unwrap()),
            Err(WorkspaceError::SymlinkDenied)
        ));
    }

    // A renamed or replaced project root is rejected rather than silently
    // changing the authority granted at open time.
    #[cfg(unix)]
    {
        // A live Unix directory descriptor survives rename, so the swap is
        // detected by inode identity on the next access.
        let moved = temp.path().join("moved");
        std::fs::rename(&project, &moved).unwrap();
        std::fs::create_dir(&project).unwrap();
        assert!(
            workspace
                .read(&ProjectReference::parse("data.txt").unwrap())
                .is_err()
        );
    }
    #[cfg(windows)]
    {
        // cap_std opens directory handles without FILE_SHARE_DELETE, so a live
        // root cannot be renamed or replaced underneath the workspace; that
        // handle policy itself enforces the no-rebind guarantee. Release the
        // handle, swap the root, and confirm a fresh open binds the replacement
        // directory rather than the stale authority.
        let moved = temp.path().join("moved");
        assert!(std::fs::rename(&project, &moved).is_err());
        drop(workspace);
        std::fs::rename(&project, &moved).unwrap();
        std::fs::create_dir(&project).unwrap();
        let reopened = WorkspaceRoot::open(&project).unwrap();
        assert!(matches!(
            reopened.read(&ProjectReference::parse("data.txt").unwrap()),
            Err(WorkspaceError::Io(_))
        ));
    }

    let aliases = temp.path().join("aliases");
    std::fs::create_dir_all(aliases.join(".aworkit/portable/refs")).unwrap();
    // Ref files that differ only by case are a case-insensitive alias and must
    // fail closed.
    #[cfg(unix)]
    {
        std::fs::write(aliases.join(".aworkit/portable/refs/Main.json"), b"{}").unwrap();
        std::fs::write(aliases.join(".aworkit/portable/refs/main.json"), b"{}").unwrap();
        let engine = PortableIntegrityEngine::new(PortableRepository::new(
            PortablePaths::open(&aliases).unwrap(),
        ));
        assert!(engine.inspect().is_err());
    }
    #[cfg(windows)]
    {
        // A case-insensitive volume collapses the two names into one file, so a
        // case alias can never be materialized. The equivalent guarantee is
        // exact-case component substitution: the ref is readable only with its
        // on-disk casing.
        std::fs::write(aliases.join(".aworkit/portable/refs/Main.json"), b"{}").unwrap();
        let paths = PortablePaths::open(&aliases).unwrap();
        assert!(matches!(
            paths
                .root()
                .read(&ProjectReference::parse(".aworkit/portable/refs/main.json").unwrap()),
            Err(WorkspaceError::CaseAliasDenied)
        ));
        assert_eq!(
            paths
                .root()
                .read(&ProjectReference::parse(".aworkit/portable/refs/Main.json").unwrap())
                .unwrap(),
            b"{}"
        );
    }
}

#[test]
fn prepare_publish_verify_is_idempotent_conflict_safe_and_covers_every_crash_window() {
    let temp = TempDir::new().unwrap();
    let repository = PortableRepository::new(PortablePaths::open(temp.path()).unwrap());
    let first = commit("branch.1", "commit.1", 0, 0, json!({"text":"one"}));
    let prepared = repository.prepare(&first).unwrap();
    assert_eq!(repository.read_branch("branch.1").unwrap().generation, 0);
    let published = repository.publish(&prepared).unwrap();
    assert_eq!(repository.verify(&prepared).unwrap(), published);
    assert_eq!(published.generation, 1);
    assert_eq!(
        repository.prepare_publish_verify(&first).unwrap(),
        published
    );

    let mut changed_identity = first.clone();
    changed_identity.events[0].payload = json!({"text":"changed"});
    assert!(matches!(
        repository.prepare(&changed_identity),
        Err(CommitError::CommitIdentityConflict)
    ));
    let stale = commit("branch.1", "commit.stale", 0, 1, json!({"text":"stale"}));
    assert!(matches!(
        repository.prepare(&stale),
        Err(CommitError::HeadConflict { .. })
    ));

    let second = commit("branch.1", "commit.2", 1, 1, json!({"text":"two"}));
    repository
        .inject_fault_once(CommitFaultPoint::AfterPrepare)
        .unwrap();
    assert!(matches!(
        repository.prepare(&second),
        Err(CommitError::InjectedFault(CommitFaultPoint::AfterPrepare))
    ));
    assert_eq!(repository.read_branch("branch.1").unwrap().generation, 1);
    let second_prepared = repository.prepare(&second).unwrap();
    repository
        .inject_fault_once(CommitFaultPoint::BeforeHeadPublication)
        .unwrap();
    assert!(matches!(
        repository.publish(&second_prepared),
        Err(CommitError::InjectedFault(
            CommitFaultPoint::BeforeHeadPublication
        ))
    ));
    assert_eq!(repository.read_branch("branch.1").unwrap().generation, 1);
    repository
        .inject_fault_once(CommitFaultPoint::AfterHeadPublication)
        .unwrap();
    assert!(matches!(
        repository.publish(&second_prepared),
        Err(CommitError::PublicationUncertain)
    ));
    assert_eq!(repository.verify(&second_prepared).unwrap().generation, 2);

    let third = commit("branch.1", "commit.3", 2, 2, json!({"text":"three"}));
    let second_path = temp.path().join(format!(
        ".aworkit/portable/segments/sha256/{}",
        second_prepared
            .segment_hash
            .strip_prefix("sha256:")
            .unwrap()
    ));
    std::fs::write(second_path, b"corrupt").unwrap();
    assert!(repository.prepare(&third).is_err());
}

fn protocol_batch(repository_id: &StableId, branch: &str) -> CommitBatchV1 {
    CommitBatchV1 {
        backend: HistoryBackendV1::PortableProject {
            repository_id: repository_id.clone(),
        },
        chat_id: id("chat.1"),
        run_id: id("run.1"),
        branch_id: id(branch),
        expected_head: 0,
        expected_aggregate_version: 0,
        events: vec![EventV1 {
            event_id: id("event.1"),
            schema_version: 1,
            kind: "message".into(),
            payload: json!({"text":"portable"}),
        }],
        attempts: vec![],
        checkpoint: None,
        deduplication: None,
        outbox: vec![],
        prepared_artifacts: vec![],
    }
}

#[test]
fn process_neutral_port_hashes_rich_records_and_separates_prepare_publish_verify() {
    let temp = TempDir::new().unwrap();
    let repository = PortableRepository::new(PortablePaths::open(temp.path()).unwrap());
    let repository_id = id("repository.1");
    let batch = protocol_batch(&repository_id, "branch.1");
    let record = serde_json::to_value(PortableTransitionRecordV1 {
        batch: batch.clone(),
        context: context(vec![]),
    })
    .unwrap();
    let checkpoint = None;
    let request = PortablePrepareV1 {
        operation_id: id("operation.1"),
        chat_id: batch.chat_id,
        branch_id: batch.branch_id,
        expected_generation: 0,
        expected_next_ordinal: 0,
        expected_head_hash: None,
        record_hash: protocol_value_hash("portable-record-v1", &record).unwrap(),
        record,
        checkpoint_hash: protocol_value_hash("portable-checkpoint-v1", &Value::Null).unwrap(),
        checkpoint,
    };
    let prepared = PortableCanonicalCommitPort::prepare(&repository, &request).unwrap();
    assert!(
        PortableCanonicalCommitPort::read_head(&repository, &request.branch_id)
            .unwrap()
            .is_none()
    );
    let receipt = PortableCanonicalCommitPort::publish(&repository, &prepared).unwrap();
    assert_eq!(
        PortableCanonicalCommitPort::verify(&repository, &receipt).unwrap(),
        receipt
    );
    assert_eq!(
        PortableCanonicalCommitPort::read_head(&repository, &request.branch_id).unwrap(),
        Some(receipt)
    );

    let mut bad_hash = request;
    bad_hash.operation_id = id("operation.bad-hash");
    bad_hash.record_hash = hash('0');
    assert!(PortableCanonicalCommitPort::prepare(&repository, &bad_hash).is_err());
}

#[test]
fn artifacts_manifests_and_negotiation_are_inert_bounded_and_immutable() {
    let temp = TempDir::new().unwrap();
    let paths = PortablePaths::open(temp.path()).unwrap();
    let artifacts = ArtifactStore::new(paths.clone());
    let descriptor = artifacts
        .admit_text("text/markdown", b"# Evidence\nportable")
        .unwrap();
    assert_eq!(
        artifacts.read_verified(&descriptor).unwrap(),
        b"# Evidence\nportable"
    );
    assert_eq!(
        artifacts.read_range(&descriptor, 2, 8).unwrap(),
        b"Evidence"
    );
    assert!(matches!(
        artifacts.admit_text("text/html", b"<script>x</script>"),
        Err(ArtifactError::ActiveOrUnknownMediaType)
    ));
    assert!(
        artifacts
            .admit_text("application/json", br#"{"debugCapture":"local"}"#)
            .is_err()
    );
    assert!(
        artifacts
            .admit_text("text/plain", b"Bearer abcdefghijklmnopqrstuvwxyz")
            .is_err()
    );

    let supported = BTreeSet::from(["segments-v1".to_owned()]);
    let repository_manifest = RepositoryManifest {
        family: "aworkit-portable-session".into(),
        major: 1,
        minor: 0,
        required_features: vec!["segments-v1".into()],
    };
    assert_eq!(
        repository_manifest.compatibility(&supported),
        RepositoryCompatibility::ReadWrite
    );
    let mut newer = repository_manifest.clone();
    newer.minor = 1;
    assert_eq!(
        newer.compatibility(&supported),
        RepositoryCompatibility::ReadOnlyNewerMinor
    );
    let mut unknown = repository_manifest.clone();
    unknown.family = "other-family".into();
    assert_eq!(
        unknown.compatibility(&supported),
        RepositoryCompatibility::UnsupportedFamily
    );
    let catalog = ManifestCatalog::new(paths);
    let repository_hash = catalog
        .publish(ManifestEnvelopeV1::Repository(repository_manifest.clone()))
        .unwrap();
    assert_eq!(
        catalog.read(&repository_hash).unwrap(),
        ManifestEnvelopeV1::Repository(repository_manifest)
    );
    let session = SessionManifest {
        session_id: "session.1".into(),
        chat_id: "chat.1".into(),
        run_id: "run.1".into(),
        frozen_snapshot_hash: hash('a'),
        canonical_branch_id: "branch.1".into(),
        export_policy_hash: ExportPolicy.policy_hash().into(),
    };
    catalog
        .publish(ManifestEnvelopeV1::Session(session))
        .unwrap();
    assert!(
        BranchManifest {
            branch_id: "branch.1".into(),
            session_id: "session.1".into(),
            parent_branch_id: Some("branch.1".into()),
            parent_checkpoint_hash: None,
            parent_head_hash: None,
        }
        .validate()
        .is_err()
    );
}

#[test]
fn hostile_imports_stay_inert_and_projection_pages_include_verified_evidence() {
    let temp = TempDir::new().unwrap();
    let paths = PortablePaths::open(temp.path()).unwrap();
    let artifact = ArtifactStore::new(paths.clone())
        .admit_text("text/plain", b"evidence")
        .unwrap();
    let repository = PortableRepository::new(paths.clone());
    let mut portable = commit(
        "branch.1",
        "commit.1",
        0,
        0,
        json!({"command":"do-not-execute", "debugCapture":"local-only"}),
    );
    portable.context = Some(context(vec![artifact.clone()]));
    let receipt = repository.prepare_publish_verify(&portable).unwrap();
    let feed = ProjectionFeed::new(paths.clone());
    assert!(feed.validate_lineage(&receipt.head_segment_hash).accepted);
    let page = feed.read_page(&receipt.head_segment_hash, 0, 1).unwrap();
    assert_eq!(page.events[0].payload, json!({"command":"do-not-execute"}));
    assert_eq!(page.next_ordinal, None);
    assert_eq!(page.source_segment_hash, receipt.head_segment_hash);
    assert!(page.evidence.iter().any(|evidence| {
        matches!(
            evidence,
            PortableProjectionEvidenceV1::Omission(OmissionFact { pointer, .. })
                if pointer == "/events/0/payload/debugCapture"
        )
    }));
    assert!(page.evidence.iter().any(|evidence| {
        matches!(
            evidence,
            PortableProjectionEvidenceV1::ArtifactMetadata(value) if value == &artifact
        )
    }));
    let empty = feed.read_page(&receipt.head_segment_hash, 0, 0).unwrap();
    assert_eq!(empty.next_ordinal, Some(0));
    assert_ne!(empty.projection_token, page.projection_token);
    assert!(!feed.validate_import("sha256:not-a-hash").accepted);

    let segment_path = temp.path().join(format!(
        ".aworkit/portable/segments/sha256/{}",
        receipt.head_segment_hash.strip_prefix("sha256:").unwrap()
    ));
    std::fs::write(segment_path, b"corrupt").unwrap();
    assert!(!feed.validate_import(&receipt.head_segment_hash).accepted);
    assert!(feed.read_page(&receipt.head_segment_hash, 0, 1).is_err());
}

#[test]
fn rebinding_integrity_repair_and_two_scan_collection_never_inherit_or_rewrite() {
    let requirement = aworkit_portable_store::CapabilityRequirement {
        logical_id: "model.primary".into(),
        version: "v1".into(),
    };
    let candidates = vec![
        LocalCapabilityV1 {
            logical_id: "model.primary".into(),
            portable_version: "v1".into(),
            local_binding_id: "provider.a".into(),
            version_hash: hash('a'),
            compatible: true,
        },
        LocalCapabilityV1 {
            logical_id: "model.primary".into(),
            portable_version: "v1".into(),
            local_binding_id: "provider.b".into(),
            version_hash: hash('b'),
            compatible: true,
        },
    ];
    let rebind = plan_continuation_rebind(
        "branch.parent",
        "branch.child",
        std::slice::from_ref(&requirement),
        &candidates,
    );
    assert!(matches!(
        rebind.resolutions[0],
        RebindResolutionV1::Ambiguous { .. }
    ));
    assert!(!rebind.can_continue_after_fresh_approval);
    assert!(!rebind.imported_approvals_accepted);
    assert!(!rebind.imported_secret_handles_accepted);
    let child = plan_child_continuation(
        "chat.parent",
        "branch.parent",
        "chat.child",
        "branch.child",
        std::slice::from_ref(&requirement),
        &candidates[..1],
    );
    assert!(child.fresh_snapshot_required);
    assert!(child.fresh_authority_required);
    assert!(child.fresh_approvals_required);
    assert!(!child.imported_runtime_resumable);
    assert!(child.can_create_after_user_confirmation);
    let not_fresh = plan_child_continuation(
        "chat.parent",
        "branch.parent",
        "chat.parent",
        "branch.parent",
        &[requirement],
        &candidates[..1],
    );
    assert!(!not_fresh.can_create_after_user_confirmation);

    let temp = TempDir::new().unwrap();
    let paths = PortablePaths::open(temp.path()).unwrap();
    let repository = PortableRepository::new(paths.clone());
    let receipt = repository
        .prepare_publish_verify(&commit("branch.1", "commit.1", 0, 0, json!({"text":"one"})))
        .unwrap();
    let engine = PortableIntegrityEngine::new(repository.clone());
    let report = engine.inspect().unwrap();
    assert!(!report.continuation_blocked);
    assert!(report.issues.is_empty());
    assert_eq!(
        engine
            .propose_non_destructive_repair("empty.branch", &receipt.head_segment_hash, None)
            .unwrap(),
        NonDestructiveRepairProposalV1::CreatePointerOnlyRepair {
            branch_id: "empty.branch".into(),
            expected_generation: 0,
            expected_current_head: None,
            candidate_verified_head: receipt.head_segment_hash.clone(),
        }
    );
    assert!(matches!(
        engine
            .propose_non_destructive_repair(
                "branch.1",
                &receipt.head_segment_hash,
                Some("repair.branch")
            )
            .unwrap(),
        NonDestructiveRepairProposalV1::CreateRepairBranch { .. }
    ));
    assert!(matches!(
        engine.propose_non_destructive_repair("branch.1", "sha256:bad", None),
        Ok(NonDestructiveRepairProposalV1::QuarantineForInspection { .. })
    ));

    let catalog = ManifestCatalog::new(paths.clone());
    let continuation_request = ChildContinuationManifestRequestV1 {
        plan: child,
        child_session_id: "session.child".into(),
        child_run_id: "run.child".into(),
        fresh_frozen_snapshot_hash: hash('7'),
        verified_parent_checkpoint_hash: None,
        verified_parent_head_hash: receipt.head_segment_hash.clone(),
        user_confirmed: true,
    };
    let continuation = catalog
        .publish_child_continuation(&continuation_request)
        .unwrap();
    assert_eq!(continuation.session.chat_id, "chat.child");
    assert_eq!(
        continuation.branch.parent_branch_id.as_deref(),
        Some("branch.parent")
    );
    assert_eq!(
        catalog.read(&continuation.session_manifest_hash).unwrap(),
        ManifestEnvelopeV1::Session(continuation.session)
    );
    let mut unconfirmed = continuation_request;
    unconfirmed.child_session_id = "session.unconfirmed".into();
    unconfirmed.user_confirmed = false;
    assert!(catalog.publish_child_continuation(&unconfirmed).is_err());

    let orphan = PortableSegment {
        parent_segment_hash: None,
        base_checkpoint_hash: None,
        first_ordinal: 0,
        context: Some(context(vec![])),
        events: vec![event("orphan.branch", 0, json!({"text":"orphan"}))],
    };
    let orphan_id = paths
        .publish("segments", &CanonicalCodec.encode_segment(&orphan).unwrap())
        .unwrap();
    let first = ReachabilityScanV1 {
        generation: 1,
        observed_epoch_millis: 100,
        branch_heads: report.branch_heads.clone(),
        reachable: report.reachable_segments.clone(),
    };
    let final_scan = ReachabilityScanV1 {
        generation: 2,
        observed_epoch_millis: 300,
        branch_heads: report.branch_heads.clone(),
        reachable: report.reachable_segments.clone(),
    };
    assert_eq!(
        retention_plan_two_phase(
            &BTreeSet::from([orphan_id.clone()]),
            &final_scan,
            &first,
            &BTreeMap::new(),
            10,
        )
        .err(),
        Some(RetentionError::StaleFinalScan)
    );
    let collected = engine
        .collect_verified_orphans(
            "segments",
            &first,
            &final_scan,
            &BTreeMap::from([(orphan_id.clone(), 100)]),
            100,
        )
        .unwrap();
    assert_eq!(collected, vec![orphan_id.clone()]);
    assert!(!paths.contains("segments", &orphan_id).unwrap());
    assert!(matches!(
        engine.collect_verified_orphans("claims", &first, &final_scan, &BTreeMap::new(), 0,),
        Err(IntegrityError::UnsafeNamespace)
    ));

    paths
        .remove_verified("segments", &receipt.head_segment_hash)
        .unwrap();
    let corrupt = engine.inspect().unwrap();
    assert!(corrupt.continuation_blocked);
    assert!(
        corrupt
            .issues
            .iter()
            .any(|issue| { matches!(issue, IntegrityIssueV1::MissingOrCorruptSegment { .. }) })
    );
}

#[test]
fn checkpoints_are_canonical_bounded_and_linked_to_the_last_event() {
    let temp = TempDir::new().unwrap();
    let repository = PortableRepository::new(PortablePaths::open(temp.path()).unwrap());
    let mut value = commit(
        "branch.checkpoint",
        "commit.checkpoint",
        0,
        0,
        json!({"text":"checkpointed"}),
    );
    value.checkpoint = Some(PortableCheckpoint {
        last_event_id: Some("event.0".into()),
        aggregate_version: 1,
        reducer_version: "reducer.v1".into(),
        snapshot_hash: value
            .context
            .as_ref()
            .and_then(|context| context.frozen_snapshot.as_ref())
            .map(|snapshot| snapshot.snapshot_hash.clone()),
        state_hash: hash('9'),
    });
    let receipt = repository.prepare_publish_verify(&value).unwrap();
    assert!(receipt.checkpoint_hash.is_some());
    let mut mismatch = commit(
        "branch.other",
        "commit.bad-checkpoint",
        0,
        0,
        json!({"text":"bad"}),
    );
    mismatch.checkpoint = value.checkpoint;
    mismatch.checkpoint.as_mut().unwrap().last_event_id = Some("event.other".into());
    assert!(matches!(
        repository.prepare(&mismatch),
        Err(CommitError::CheckpointMismatch)
    ));
}
