use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use aworkit_capability_host::{
    AdapterRegistry, AdmissionDispositionV1, ApprovedInvocationEnvelopeV1,
    ArgumentVectorInvocationV1, BuiltInProcessTools, CancellationToken, CapabilityDescriptor,
    CapabilityHost, CapabilityKind, ControlledProcessResult, DispatchEvidenceV1, EffectEvidenceV1,
    FileAuthority, FileEditRequestV1, FileEffectKindV1, FileGrepRequestV1, FileListRequestV1,
    FileReadRequestV1, FileSearchRequestV1, FileToolError, FileWriteRequestV1, FrozenModelGateway,
    HermeticProcessPort, HermeticProcessStep, HostControlEnvelopeV1, HostControlKindV1, HostError,
    HostToolLimitsV1, InjectionTargetV1, InvocationNormalizer, ModelCandidateV1,
    ModelEventObserverV1, ModelEventV1, ModelRequestV1, ModelResolutionPlanV1, NormalizeError,
    NormalizedContentV1, OutcomeDispositionV1, NativeProcessPort, PlatformProcessPort,
    ProcessRunner, ProcessSpecV1,
    ProcessTermination, ProjectFiles, ProviderAcceptanceV1, ProviderEnginePortV1, ProviderError,
    PythonInvocationV1, Redactor, RedeemLeaseRequestV1, RetrySafetyV1, SecretDeliveryV1,
    SecretFieldPlanV1, SecretLeaseClientV1, SecretLeaseHandleV1, SecretMaterializationError,
    SecretMaterializationPlanV1, SecretMaterializer, ShellInvocationV1, SideEffectClass,
    TerminalEvidenceV1, ToolAdapterError, ToolAuthorityModeV1, classify_outcome,
};
use aworkit_protocol::{
    AttestedExtensionSetV1, ProcessGeneration, SchemaVersion, StableId,
    attested_extension_set_hash_v1,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use zeroize::Zeroizing;

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("stable test ID")
}

fn descriptor() -> CapabilityDescriptor {
    let mut descriptor = CapabilityDescriptor::build(
        "tool.file.read",
        "1.2.3",
        CapabilityKind::FileRead,
        SideEffectClass::ReadOnly,
    )
    .expect("descriptor");
    descriptor.supports_streaming = true;
    descriptor.supports_cancellation = true;
    descriptor.allowed_scopes = vec!["project.read".into(), "project.search".into()];
    descriptor.secret_slots = vec!["token".into()];
    descriptor.max_input_bytes = 1024;
    descriptor.max_output_bytes = 2048;
    descriptor.rehash().expect("rehash");
    descriptor
}

fn materialize_builtin_registry(
    registry: AdapterRegistry,
    generation: ProcessGeneration,
) -> aworkit_capability_host::FrozenAdapterRegistry {
    let mut set = AttestedExtensionSetV1 {
        host_id: id("host.primary"),
        host_generation: generation,
        host_protocol: 1,
        extensions: Vec::new(),
        set_hash: String::new(),
    };
    set.set_hash = attested_extension_set_hash_v1(&set).expect("empty attested set hash");
    registry
        .materialize_attested_set(&set)
        .expect("materialize built-ins under attestation")
}

fn envelope(descriptor: &CapabilityDescriptor, invocation: &str) -> ApprovedInvocationEnvelopeV1 {
    let mut envelope = ApprovedInvocationEnvelopeV1 {
        schema_version: SchemaVersion::V1,
        invocation_id: id(invocation),
        decision_id: id("decision.1"),
        host_generation: ProcessGeneration(7),
        capability_id: descriptor.capability_id.clone(),
        adapter_version: descriptor.version.clone(),
        binding_hash: descriptor.version_hash.clone(),
        extension: None,
        required_isolation_profile: descriptor.required_isolation.clone(),
        kind: descriptor.kind,
        enforced_scopes: vec!["project.read".into()],
        deadline_epoch_millis: 10_000,
        cancellation_token: id("cancel.1"),
        lease_handles: vec![id("lease.1")],
        max_output_bytes: 1024,
        payload: json!({"path":"notes.txt"}),
        core_authentication_tag: String::new(),
    };
    envelope.sign(b"core-key").expect("sign");
    envelope
}

#[test]
fn authenticated_gateway_fences_drift_authority_backpressure_and_deduplication() {
    let descriptor = descriptor();
    let mut registry = AdapterRegistry::default();
    registry
        .register_capability(descriptor.clone())
        .expect("register");
    let frozen = materialize_builtin_registry(registry, ProcessGeneration(7));
    let host = CapabilityHost::from_attested_registry(frozen, b"core-key".to_vec(), 1)
        .expect("authenticated host");

    let approved = envelope(&descriptor, "invocation.1");
    let first = host.admit_v1(&approved, 9_000).expect("admit");
    assert_eq!(first.disposition, AdmissionDispositionV1::Execute);
    assert!(first.should_execute());
    let duplicate = host.admit_v1(&approved, 9_000).expect("deduplicate");
    assert_eq!(duplicate.disposition, AdmissionDispositionV1::AlreadyActive);
    assert_eq!(duplicate.request_hash, first.request_hash);

    let second = envelope(&descriptor, "invocation.2");
    assert!(matches!(
        host.admit_v1(&second, 9_000),
        Err(HostError::Backpressure)
    ));
    let mut cancel = HostControlEnvelopeV1 {
        schema_version: SchemaVersion::V1,
        control_id: id("control.cancel.1"),
        invocation_id: approved.invocation_id.clone(),
        host_generation: ProcessGeneration(7),
        cancellation_token: approved.cancellation_token.clone(),
        kind: HostControlKindV1::Cancel,
        core_authentication_tag: String::new(),
    };
    cancel.sign(b"core-key").expect("sign cancellation");
    let mut retargeted = cancel.clone();
    retargeted.cancellation_token = id("cancel.other");
    retargeted
        .sign(b"core-key")
        .expect("sign retargeted control");
    assert!(matches!(
        host.apply_control_v1(&retargeted),
        Err(HostError::CancellationTokenMismatch)
    ));
    let mut unauthenticated = cancel.clone();
    unauthenticated.control_id = id("control.tampered");
    assert!(matches!(
        host.apply_control_v1(&unauthenticated),
        Err(HostError::Authentication)
    ));
    host.apply_control_v1(&cancel).expect("reserved cancel");
    assert!(host.is_cancelled(&approved.invocation_id));
    host.complete(&approved.invocation_id).expect("complete");
    assert_eq!(
        host.admit_v1(&approved, 9_000)
            .expect("completed tombstone")
            .disposition,
        AdmissionDispositionV1::AlreadyCompleted
    );

    let mut tampered = approved.clone();
    tampered.payload = json!({"path":"other.txt"});
    assert!(matches!(
        host.admit_v1(&tampered, 9_000),
        Err(HostError::Authentication)
    ));
    let mut changed_identity = approved.clone();
    changed_identity.payload = json!({"path":"other.txt"});
    changed_identity.sign(b"core-key").expect("resign");
    assert!(matches!(
        host.admit_v1(&changed_identity, 9_000),
        Err(HostError::InvocationIdentityConflict)
    ));

    let mut stale = second.clone();
    stale.host_generation = ProcessGeneration(6);
    stale.sign(b"core-key").expect("resign");
    assert!(matches!(
        host.admit_v1(&stale, 9_000),
        Err(HostError::StaleGeneration)
    ));
    let mut drift = second.clone();
    drift.binding_hash = format!("sha256:{}", "0".repeat(64));
    drift.sign(b"core-key").expect("resign");
    assert!(host.admit_v1(&drift, 9_000).is_err());
    let mut scope = second.clone();
    scope.enforced_scopes = vec!["outside".into()];
    scope.sign(b"core-key").expect("resign");
    assert!(matches!(
        host.admit_v1(&scope, 9_000),
        Err(HostError::ScopeBroadened)
    ));
    let mut noncanonical = second.clone();
    noncanonical.enforced_scopes = vec!["project.search".into(), "project.read".into()];
    noncanonical.sign(b"core-key").expect("resign");
    assert!(matches!(
        host.admit_v1(&noncanonical, 9_000),
        Err(HostError::NonCanonicalAuthority)
    ));
    let mut too_many_leases = second.clone();
    too_many_leases.lease_handles = vec![id("lease.1"), id("lease.2")];
    too_many_leases.sign(b"core-key").expect("resign");
    assert!(matches!(
        host.admit_v1(&too_many_leases, 9_000),
        Err(HostError::LeaseCountBroadened)
    ));
    assert!(matches!(
        host.admit_v1(&second, 10_000),
        Err(HostError::DeadlineElapsed)
    ));
}

#[test]
fn rooted_file_tools_enforce_bounds_identity_symlinks_cancellation_and_effects() {
    let temp = TempDir::new().expect("temp");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).expect("project");
    std::fs::write(project.join("notes.txt"), b"alpha beta alpha").expect("seed");
    let files = ProjectFiles::new(FileAuthority {
        root: project.clone(),
        allow_write: true,
    })
    .expect("files");

    let read = files
        .read_v1(
            &FileReadRequestV1 {
                path: PathBuf::from("notes.txt"),
                maximum_bytes: 64,
            },
            &CancellationToken::default(),
        )
        .expect("read");
    assert_eq!(read.bytes, b"alpha beta alpha");
    assert_eq!(read.effect.kind, FileEffectKindV1::Read);
    assert_eq!(read.effect.before_content_hash, read.content_hash);
    assert!(!read.effect.write_committed);
    assert!(matches!(
        files.read_v1(
            &FileReadRequestV1 {
                path: "notes.txt".into(),
                maximum_bytes: 2,
            },
            &CancellationToken::default(),
        ),
        Err(FileToolError::TooLarge)
    ));
    let search = files
        .search_v1(
            &FileSearchRequestV1 {
                path: "notes.txt".into(),
                needle: "alpha".into(),
                maximum_results: 1,
            },
            &CancellationToken::default(),
        )
        .expect("search");
    assert_eq!(search.offsets, vec![0]);
    assert_eq!(search.effect.kind, FileEffectKindV1::Search);

    assert!(matches!(
        files.edit_v1(
            &FileEditRequestV1 {
                path: "notes.txt".into(),
                expected_content_hash: format!("sha256:{}", "0".repeat(64)),
                replacement: b"changed".to_vec(),
            },
            &CancellationToken::default(),
        ),
        Err(FileToolError::Conflict)
    ));
    let edited = files
        .edit_v1(
            &FileEditRequestV1 {
                path: "notes.txt".into(),
                expected_content_hash: read.content_hash,
                replacement: b"changed".to_vec(),
            },
            &CancellationToken::default(),
        )
        .expect("atomic edit");
    assert!(edited.effect.write_committed);
    assert_eq!(edited.effect.kind, FileEffectKindV1::Edit);
    assert_eq!(
        std::fs::read(project.join("notes.txt")).unwrap(),
        b"changed"
    );

    // Extended v1 file tools: glob list, regex grep, and guarded write.
    std::fs::write(project.join("second.txt"), b"beta gamma beta").expect("second seed");
    let list = files
        .list_v1(
            &FileListRequestV1 {
                pattern: "*.txt".into(),
                maximum_entries: 8,
            },
            &CancellationToken::default(),
        )
        .expect("list");
    let listed: Vec<&str> = list
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&"notes.txt") && listed.contains(&"second.txt"));
    assert_eq!(list.effect.kind, FileEffectKindV1::List);
    let grep = files
        .grep_v1(
            &FileGrepRequestV1 {
                pattern: "beta".into(),
                path: None,
                maximum_matches: 8,
                maximum_files: 8,
                maximum_file_bytes: 1024,
            },
            &CancellationToken::default(),
        )
        .expect("grep");
    assert_eq!(grep.files_scanned, 2);
    assert_eq!(grep.matches.len(), 2);
    assert!(!grep.match_limit_reached);
    assert!(!grep.file_limit_reached);
    assert_eq!(grep.skipped_directories, 0);
    assert!(
        grep.matches
            .iter()
            .all(|found| found.line_text.contains("beta"))
    );
    assert_eq!(grep.effect.kind, FileEffectKindV1::Grep);
    std::fs::create_dir_all(project.join("src/nested")).expect("nested source tree");
    for (path, content) in [
        ("src/main.rs", b"fn main() {}".as_slice()),
        ("src/nested/lib.rs", b"pub fn nested() {}".as_slice()),
        ("src/app.ts", b"export {};".as_slice()),
        ("src/nested/view.tsx", b"export default null;".as_slice()),
        ("src/nested/data.json", b"{}".as_slice()),
        ("src/nested/skip.md", b"skip".as_slice()),
    ] {
        std::fs::write(project.join(path), content).expect("nested source file");
    }
    let list_paths = |pattern: &str| {
        files
            .list_v1(
                &FileListRequestV1 {
                    pattern: pattern.into(),
                    maximum_entries: 32,
                },
                &CancellationToken::default(),
            )
            .expect("recursive list")
            .entries
            .into_iter()
            .map(|entry| entry.path)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        list_paths("src/*.rs"),
        BTreeSet::from(["src/main.rs".to_owned()])
    );
    assert_eq!(
        list_paths("src/**/*.rs"),
        BTreeSet::from(["src/main.rs".to_owned(), "src/nested/lib.rs".to_owned()])
    );
    assert_eq!(
        list_paths("src/**/*.{ts,tsx,json}"),
        BTreeSet::from([
            "src/app.ts".to_owned(),
            "src/nested/data.json".to_owned(),
            "src/nested/view.tsx".to_owned()
        ])
    );
    assert_eq!(list_paths("src/**").len(), 6);
    assert!(matches!(
        files.write_v1(
            &FileWriteRequestV1 {
                path: "second.txt".into(),
                content: b"replaced".to_vec(),
                expected_content_hash: Some(format!("sha256:{}", "0".repeat(64))),
            },
            &CancellationToken::default(),
        ),
        Err(FileToolError::Conflict)
    ));
    let written = files
        .write_v1(
            &FileWriteRequestV1 {
                path: "second.txt".into(),
                content: b"replaced".to_vec(),
                expected_content_hash: None,
            },
            &CancellationToken::default(),
        )
        .expect("write");
    assert!(written.effect.write_committed);
    assert_eq!(written.effect.kind, FileEffectKindV1::Write);
    assert_eq!(
        std::fs::read(project.join("second.txt")).unwrap(),
        b"replaced"
    );

    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert!(matches!(
        files.read_v1(
            &FileReadRequestV1 {
                path: "notes.txt".into(),
                maximum_bytes: 64,
            },
            &cancelled,
        ),
        Err(FileToolError::Cancelled)
    ));
    assert!(matches!(
        files.read("../outside"),
        Err(FileToolError::OutsideRoot)
    ));

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside", project.join("escape")).expect("symlink");
        assert!(matches!(
            files.read("escape"),
            Err(FileToolError::SymlinkDenied)
        ));
    }

    let read_only = ProjectFiles::new(FileAuthority {
        root: project.clone(),
        allow_write: false,
    })
    .expect("read only");
    assert!(matches!(
        read_only.edit("notes.txt", b"changed", b"denied"),
        Err(FileToolError::WriteDenied)
    ));

    // Root-swap defense differs by platform: Unix detects the swap through
    // device/inode identity after the rename; Windows prevents the rename
    // itself because the capability handle omits FILE_SHARE_DELETE.
    #[cfg(unix)]
    {
        let moved = temp.path().join("moved-project");
        std::fs::rename(&project, &moved).expect("replace root");
        std::fs::create_dir(&project).expect("replacement root");
        assert!(matches!(
            files.read("notes.txt"),
            Err(FileToolError::RootChanged)
        ));
    }
    #[cfg(windows)]
    {
        let moved = temp.path().join("moved-project");
        assert!(
            std::fs::rename(&project, &moved).is_err(),
            "an open capability handle must pin the root against rename"
        );
        assert_eq!(files.read("notes.txt").expect("still readable"), b"changed");
    }
}

/// The regex walk must report what it actually covered: dependency trees are
/// skipped, and a stopped scan is never presented as a complete one.
#[test]
fn grep_reports_skipped_dependency_trees_and_stopped_scans() {
    let temp = TempDir::new().expect("temp");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src/deep")).expect("src");
    std::fs::create_dir_all(project.join("node_modules/dep")).expect("dependency tree");
    std::fs::create_dir_all(project.join(".git/objects")).expect("vcs tree");
    std::fs::write(project.join("src/app.ts"), b"const balance = 1;").expect("source");
    std::fs::write(project.join("src/deep/page.ts"), b"const balance = 2;").expect("source");
    std::fs::write(project.join("node_modules/dep/index.js"), b"const balance = 3;")
        .expect("dependency");
    std::fs::write(project.join(".git/objects/pack"), b"const balance = 4;").expect("vcs");
    let files = ProjectFiles::new(FileAuthority {
        root: project.clone(),
        allow_write: false,
    })
    .expect("files");

    let complete = files
        .grep_v1(
            &FileGrepRequestV1 {
                pattern: "balance".into(),
                path: None,
                maximum_matches: 8,
                maximum_files: 64,
                maximum_file_bytes: 1024,
            },
            &CancellationToken::default(),
        )
        .expect("grep");
    assert_eq!(
        complete.matches.len(),
        2,
        "scanned={} skipped={} match_limit={} file_limit={}",
        complete.files_scanned,
        complete.skipped_directories,
        complete.match_limit_reached,
        complete.file_limit_reached
    );
    assert!(!complete.match_limit_reached);
    assert!(!complete.file_limit_reached);
    assert_eq!(complete.skipped_directories, 2);

    // A low file ceiling stops the walk and must say so instead of implying that
    // the pattern is absent from the tree.
    let stopped = files
        .grep_v1(
            &FileGrepRequestV1 {
                pattern: "absent-everywhere".into(),
                path: None,
                maximum_matches: 8,
                maximum_files: 1,
                maximum_file_bytes: 1024,
            },
            &CancellationToken::default(),
        )
        .expect("grep");
    assert!(stopped.matches.is_empty());
    assert!(stopped.file_limit_reached);
    assert!(!stopped.match_limit_reached);
    assert_eq!(stopped.files_scanned, 1);

    let matched_out = files
        .grep_v1(
            &FileGrepRequestV1 {
                pattern: "balance".into(),
                path: None,
                maximum_matches: 1,
                maximum_files: 64,
                maximum_file_bytes: 1024,
            },
            &CancellationToken::default(),
        )
        .expect("grep");
    assert_eq!(matched_out.matches.len(), 1);
    assert!(matched_out.match_limit_reached);
    assert!(!matched_out.file_limit_reached);
    assert!(!matched_out.time_limit_reached);
}

/// A generated build tree must not consume the walk: a Rust `target/` can hold
/// more files than the entire source of a project, so descending into it spent
/// the whole file budget on compiler artefacts and made a workspace-wide regex
/// search look frozen. A walk that cannot finish inside its budget must stop and
/// say so instead of running unbounded.
#[test]
fn grep_skips_generated_build_trees_and_reports_the_time_bound() {
    let temp = TempDir::new().expect("temp");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("src");
    std::fs::create_dir_all(project.join("target/debug/deps")).expect("build tree");
    std::fs::write(project.join("src/app.rs"), b"let balance = 1;").expect("source");
    std::fs::write(project.join("target/debug/deps/app.rs"), b"let balance = 2;")
        .expect("build artefact");
    let files = ProjectFiles::new(FileAuthority {
        root: project.clone(),
        allow_write: false,
    })
    .expect("files");

    let request = FileGrepRequestV1 {
        pattern: "balance".into(),
        path: None,
        maximum_matches: 8,
        maximum_files: 64,
        maximum_file_bytes: 1024,
    };
    let walked = files
        .grep_v1(&request, &CancellationToken::default())
        .expect("grep");
    assert_eq!(
        walked.matches.len(),
        1,
        "build artefacts are not searchable source"
    );
    assert_eq!(walked.matches[0].path, "src/app.rs");
    assert_eq!(walked.skipped_directories, 1);
    assert!(!walked.file_limit_reached);
    assert!(!walked.match_limit_reached);
    assert!(!walked.time_limit_reached);

    // A budget that expires before the first file stops the walk with an empty,
    // explicitly incomplete result rather than an implied absent pattern.
    let expired = files
        .grep_v1_with_budget(&request, &CancellationToken::default(), Duration::ZERO)
        .expect("grep");
    assert!(expired.matches.is_empty());
    assert!(expired.time_limit_reached);
    assert!(!expired.file_limit_reached);
    assert!(!expired.match_limit_reached);
    assert_eq!(expired.files_scanned, 0);
}

/// A scope is the caller's answer to "where": a named file is scanned on its
/// own, a named directory replaces the root, and a scope that does not exist is
/// an error rather than a silently widened search.
#[test]
fn grep_scope_searches_one_named_file_or_directory() {
    let temp = TempDir::new().expect("temp");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src/nested")).expect("src");
    std::fs::write(project.join("src/app.rs"), b"let balance = 1;").expect("source");
    std::fs::write(project.join("src/sibling.rs"), b"let balance = 2;").expect("source");
    std::fs::write(project.join("src/nested/page.rs"), b"let balance = 3;").expect("source");
    let files = ProjectFiles::new(FileAuthority {
        root: project.clone(),
        allow_write: false,
    })
    .expect("files");
    let request = |path: Option<PathBuf>| FileGrepRequestV1 {
        pattern: "balance".into(),
        path,
        maximum_matches: 8,
        maximum_files: 64,
        maximum_file_bytes: 1024,
    };

    let one_file = files
        .grep_v1(
            &request(Some(PathBuf::from("src/app.rs"))),
            &CancellationToken::default(),
        )
        .expect("grep");
    assert_eq!(one_file.matches.len(), 1);
    assert_eq!(one_file.matches[0].path, "src/app.rs");
    assert_eq!(one_file.files_scanned, 1);

    let one_directory = files
        .grep_v1(
            &request(Some(PathBuf::from("src/nested"))),
            &CancellationToken::default(),
        )
        .expect("grep");
    assert_eq!(one_directory.matches.len(), 1);
    assert_eq!(one_directory.matches[0].path, "src/nested/page.rs");

    assert!(matches!(
        files.grep_v1(
            &request(Some(PathBuf::from("src/absent.rs"))),
            &CancellationToken::default()
        ),
        Err(FileToolError::MissingPath(_))
    ));
}

/// The project's own ignore declarations are authoritative: a root `.gitignore`
/// is honored, a nested one can re-include what the root excluded, hidden
/// entries and binary files are skipped, and every skip is reported so the
/// result never claims to be a complete scan of the tree.
#[test]
fn grep_honors_project_ignore_declarations_including_nested_ones() {
    let temp = TempDir::new().expect("temp");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("src");
    std::fs::create_dir_all(project.join("generated")).expect("generated");
    std::fs::write(project.join(".gitignore"), b"generated/\n*.log\n").expect("ignore");
    std::fs::write(project.join("src/.gitignore"), b"!keep.log\n").expect("nested ignore");
    std::fs::write(project.join("src/app.rs"), b"let balance = 1;").expect("source");
    std::fs::write(project.join("src/keep.log"), b"let balance = 2;").expect("re-included");
    std::fs::write(project.join("src/other.log"), b"let balance = 3;").expect("ignored");
    std::fs::write(project.join("notes.log"), b"let balance = 4;").expect("ignored");
    std::fs::write(project.join("generated/artifact.rs"), b"let balance = 5;").expect("ignored dir");
    std::fs::write(project.join(".hidden.rs"), b"let balance = 6;").expect("hidden");
    std::fs::write(project.join("binary.dat"), b"let balance = 7;\0binary").expect("binary");
    let files = ProjectFiles::new(FileAuthority {
        root: project.clone(),
        allow_write: false,
    })
    .expect("files");

    let result = files
        .grep_v1(
            &FileGrepRequestV1 {
                pattern: "balance".into(),
                path: None,
                maximum_matches: 32,
                maximum_files: 256,
                maximum_file_bytes: 1024,
            },
            &CancellationToken::default(),
        )
        .expect("grep");
    let paths: Vec<&str> = result.matches.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["src/app.rs", "src/keep.log"],
        "a nested !keep.log re-includes one file and nothing else"
    );
    assert_eq!(
        result.skipped_directories, 1,
        "the ignored generated/ directory is counted"
    );
    assert_eq!(
        result.skipped_files, 6,
        "two .gitignore declarations, notes.log, src/other.log, .hidden.rs and binary.dat"
    );
}

/// A search rooted below the project root still honors the declarations above it,
/// which is what makes a scoped search agree with the project it belongs to.
#[test]
fn grep_honors_declarations_above_the_capability_root() {
    let temp = TempDir::new().expect("temp");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join(".git")).expect("repository marker");
    std::fs::create_dir_all(project.join("src/skipped")).expect("src");
    std::fs::write(project.join(".gitignore"), b"skipped/\n").expect("ignore");
    std::fs::write(project.join("src/app.rs"), b"let balance = 1;").expect("source");
    std::fs::write(project.join("src/skipped/old.rs"), b"let balance = 2;").expect("ignored");
    let files = ProjectFiles::new(FileAuthority {
        root: project.join("src"),
        allow_write: false,
    })
    .expect("files");

    let result = files
        .grep_v1(
            &FileGrepRequestV1 {
                pattern: "balance".into(),
                path: None,
                maximum_matches: 32,
                maximum_files: 256,
                maximum_file_bytes: 1024,
            },
            &CancellationToken::default(),
        )
        .expect("grep");
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].path, "app.rs");
    assert_eq!(result.skipped_directories, 1);
}

/// Reading candidates on several cores must reproduce the single-threaded result
/// exactly, including which matches survive a match ceiling.
#[test]
fn parallel_scan_reproduces_the_serial_match_order() {
    let temp = TempDir::new().expect("temp");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("src");
    for index in 0..400 {
        let body = if index % 7 == 0 {
            format!("let balance = {index};\n")
        } else {
            format!("let other = {index};\n")
        };
        std::fs::write(project.join(format!("src/file_{index:03}.rs")), body).expect("source");
    }
    let files = ProjectFiles::new(FileAuthority {
        root: project.clone(),
        allow_write: false,
    })
    .expect("files");
    let request = |maximum_matches| FileGrepRequestV1 {
        pattern: "balance".into(),
        path: None,
        maximum_matches,
        maximum_files: 1024,
        maximum_file_bytes: 1024,
    };
    let cancelled = CancellationToken::default();

    for maximum_matches in [128, 3] {
        let serial = files
            .grep_v1_with_budget_and_workers(
                &request(maximum_matches),
                &cancelled,
                Duration::from_secs(30),
                1,
            )
            .expect("serial grep");
        let parallel = files
            .grep_v1_with_budget_and_workers(
                &request(maximum_matches),
                &cancelled,
                Duration::from_secs(30),
                4,
            )
            .expect("parallel grep");
        assert_eq!(
            serial.matches, parallel.matches,
            "match ceiling {maximum_matches} must not depend on the worker count"
        );
        // The parallel scan reads whole blocks, so it may examine more files
        // before it stops, but never fewer than the serial walk it must match.
        assert!(
            parallel.files_scanned >= serial.files_scanned,
            "serial={} parallel={}",
            serial.files_scanned,
            parallel.files_scanned
        );
    }
}

fn controlled(stdout: &[u8]) -> ControlledProcessResult {
    ControlledProcessResult {
        status: Some(0),
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
        termination: ProcessTermination::Exited,
        process_group_id: 42,
        output_truncated: false,
        tree_cleanup_attempted: false,
    }
}

#[cfg(windows)]
#[test]
fn configured_powershell_receives_powershell_arguments_without_shell_interpolation() {
    let platform = HermeticProcessPort::default();
    platform
        .push(HermeticProcessStep::Result(controlled(b"ok")))
        .unwrap();
    let invocation = ShellInvocationV1 {
        mode: ToolAuthorityModeV1::HostShell,
        shell_program: "C:\\Tools\\pwsh.exe".into(),
        command_text: "Write-Output 'quoted text; $literal'".into(),
        working_directory: None,
        environment: BTreeMap::new(),
        limits: HostToolLimitsV1::default(),
    };
    BuiltInProcessTools::new(platform.clone())
        .execute_shell(&invocation, &CancellationToken::default())
        .unwrap();
    assert_eq!(
        platform.observed().unwrap()[0].arguments,
        vec![
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Write-Output 'quoted text; $literal'"
        ]
    );
}

#[test]
fn process_port_and_builtin_tools_preserve_exact_authority_and_lifecycle_facts() {
    let platform = HermeticProcessPort::default();
    platform
        .push(HermeticProcessStep::Result(controlled(b"ok")))
        .unwrap();
    let tools = BuiltInProcessTools::new(platform.clone());
    let shell = ShellInvocationV1 {
        mode: ToolAuthorityModeV1::HostShell,
        shell_program: "/bin/sh".into(),
        command_text: "printf ok".into(),
        working_directory: None,
        environment: BTreeMap::new(),
        limits: HostToolLimitsV1::default(),
    };
    assert_eq!(
        tools
            .execute_shell(&shell, &CancellationToken::default())
            .unwrap()
            .stdout,
        b"ok"
    );
    let observed = platform.observed().unwrap();
    assert_eq!(observed[0].arguments, vec!["-c", "printf ok"]);
    assert!(platform.health().unwrap().process_tree_cleanup);

    let wrong_mode = ArgumentVectorInvocationV1 {
        mode: ToolAuthorityModeV1::HostShell,
        program: "/bin/echo".into(),
        arguments: vec![],
        working_directory: None,
        environment: BTreeMap::new(),
        limits: HostToolLimitsV1::default(),
    };
    assert!(matches!(
        tools.execute_argv(&wrong_mode, &CancellationToken::default()),
        Err(ToolAdapterError::AuthorityModeMismatch)
    ));
    let sandboxed = PythonInvocationV1 {
        mode: ToolAuthorityModeV1::SandboxedPython,
        interpreter: "/usr/bin/python3".into(),
        script: "print('x')".into(),
        arguments: vec![],
        working_directory: None,
        environment: BTreeMap::new(),
        limits: HostToolLimitsV1::default(),
    };
    assert!(matches!(
        tools.execute_python(&sandboxed, &CancellationToken::default()),
        Err(ToolAdapterError::VerifiedIsolationUnavailable)
    ));
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert!(
        platform
            .execute(
                &ProcessSpecV1 {
                    program: "/bin/true".into(),
                    arguments: vec![],
                    working_directory: None,
                    environment: BTreeMap::new(),
                    timeout: Duration::from_secs(1),
                    maximum_output_bytes: 64,
                    cancellation_grace: Duration::from_millis(5),
                },
                &cancelled,
            )
            .is_err()
    );

    #[cfg(unix)]
    {
        let started = Instant::now();
        let result = ProcessRunner::run_controlled(
            &ProcessSpecV1 {
                program: "/bin/sh".into(),
                arguments: vec!["-c".into(), "sleep 5 & wait".into()],
                working_directory: None,
                environment: BTreeMap::new(),
                timeout: Duration::from_millis(30),
                maximum_output_bytes: 128,
                cancellation_grace: Duration::from_millis(20),
            },
            &CancellationToken::default(),
        )
        .expect("timed process");
        assert_eq!(result.termination, ProcessTermination::TimedOut);
        assert!(result.tree_cleanup_attempted);
        assert!(started.elapsed() < Duration::from_secs(2));

        let clean = NativeProcessPort
            .execute(
                &ProcessSpecV1 {
                    program: "/bin/sh".into(),
                    arguments: vec!["-c".into(), "printf %s \"${HOME-unset}\"".into()],
                    working_directory: None,
                    environment: BTreeMap::new(),
                    timeout: Duration::from_secs(1),
                    maximum_output_bytes: 128,
                    cancellation_grace: Duration::from_millis(20),
                },
                &CancellationToken::default(),
            )
            .expect("sanitized environment");
        assert_eq!(clean.stdout, b"unset");
    }
}

struct ScriptedProvider {
    binding: &'static str,
    version: &'static str,
    events: Vec<ModelEventV1>,
    acceptance: ProviderAcceptanceV1,
    calls: Arc<AtomicUsize>,
}

#[derive(Default)]
struct RecordingModelObserver {
    completions: Mutex<Vec<(Value, String)>>,
}

impl ModelEventObserverV1 for RecordingModelObserver {
    fn model_turn_completed(&self, output: &Value, status: &str) {
        self.completions
            .lock()
            .expect("observer lock")
            .push((output.clone(), status.to_owned()));
    }
}

impl ProviderEnginePortV1 for ScriptedProvider {
    fn binding_id(&self) -> &str {
        self.binding
    }

    fn version_hash(&self) -> &str {
        self.version
    }

    fn execute(
        &self,
        _request: &ModelRequestV1,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        for event in self.events.clone() {
            emit(event)?;
        }
        Ok(self.acceptance)
    }
}

fn candidate(binding_id: &str, version_hash: &str) -> ModelCandidateV1 {
    ModelCandidateV1 {
        binding_id: binding_id.into(),
        version_hash: version_hash.into(),
    }
}

#[test]
fn model_gateway_enforces_frozen_fallback_stream_usage_bounds_and_cancellation() {
    let first_calls = Arc::new(AtomicUsize::new(0));
    let second_calls = Arc::new(AtomicUsize::new(0));
    let observer = Arc::new(RecordingModelObserver::default());
    let gateway = FrozenModelGateway::new(vec![
        Box::new(ScriptedProvider {
            binding: "primary",
            version: "hash-a",
            events: vec![],
            acceptance: ProviderAcceptanceV1::DefinitelyNotAccepted,
            calls: first_calls.clone(),
        }),
        Box::new(ScriptedProvider {
            binding: "fallback",
            version: "hash-b",
            events: vec![
                ModelEventV1::ReasoningRaw("The".into()),
                ModelEventV1::ReasoningRaw(" thought".into()),
                ModelEventV1::AssistantOutput("ans".into()),
                ModelEventV1::AssistantOutput("wer".into()),
                ModelEventV1::Usage {
                    input_tokens: 2,
                    output_tokens: 1,
                    cache: Default::default(),
                },
            ],
            acceptance: ProviderAcceptanceV1::Accepted,
            calls: second_calls.clone(),
        }),
    ])
    .with_observer(observer.clone());
    let plan = ModelResolutionPlanV1 {
        candidates: vec![
            candidate("primary", "hash-a"),
            candidate("fallback", "hash-b"),
        ],
        maximum_input_bytes: 128,
        maximum_output_bytes: 64,
    };
    let evidence = gateway
        .execute(
            &plan,
            &ModelRequestV1 {
                input: json!("hi"),
                parameters: Default::default(),
            },
        )
        .expect("fallback");
    assert_eq!(evidence.selected_binding, "fallback");
    assert_eq!(evidence.attempted_bindings, vec!["primary", "fallback"]);
    assert_eq!(evidence.events.len(), 5, "raw stream evidence stays exact");
    assert_eq!(
        observer
            .completions
            .lock()
            .expect("observer lock")
            .as_slice(),
        [(
            json!([
                {"kind":"reasoning_raw","text":"The thought"},
                {"kind":"assistant_output","text":"answer"},
                {"kind":"usage","input_tokens":2,"output_tokens":1}
            ]),
            "completed".to_owned()
        )]
    );
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second_calls.load(Ordering::SeqCst), 1);

    let late_calls = Arc::new(AtomicUsize::new(0));
    let ambiguous = FrozenModelGateway::new(vec![
        Box::new(ScriptedProvider {
            binding: "primary",
            version: "hash-a",
            events: vec![],
            acceptance: ProviderAcceptanceV1::Ambiguous,
            calls: Arc::new(AtomicUsize::new(0)),
        }),
        Box::new(ScriptedProvider {
            binding: "fallback",
            version: "hash-b",
            events: vec![ModelEventV1::Usage {
                input_tokens: 0,
                output_tokens: 0,
                cache: Default::default(),
            }],
            acceptance: ProviderAcceptanceV1::Accepted,
            calls: late_calls.clone(),
        }),
    ]);
    assert_eq!(
        ambiguous.execute(
            &plan,
            &ModelRequestV1 {
                input: json!(null),
                parameters: Default::default()
            }
        ),
        Err(ProviderError::AcceptanceAmbiguous)
    );
    assert_eq!(late_calls.load(Ordering::SeqCst), 0);

    let no_usage = FrozenModelGateway::new(vec![Box::new(ScriptedProvider {
        binding: "primary",
        version: "hash-a",
        events: vec![ModelEventV1::AssistantOutput("answer".into())],
        acceptance: ProviderAcceptanceV1::Accepted,
        calls: Arc::new(AtomicUsize::new(0)),
    })]);
    let one = ModelResolutionPlanV1 {
        candidates: vec![candidate("primary", "hash-a")],
        maximum_input_bytes: 8,
        maximum_output_bytes: 64,
    };
    assert_eq!(
        no_usage.execute(
            &one,
            &ModelRequestV1 {
                input: json!(null),
                parameters: Default::default()
            }
        ),
        Err(ProviderError::MissingOrDuplicateUsage)
    );
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert_eq!(
        gateway.execute_cancellable(
            &plan,
            &ModelRequestV1 {
                input: json!(null),
                parameters: Default::default()
            },
            &cancelled
        ),
        Err(ProviderError::Cancelled)
    );
    let duplicates = ModelResolutionPlanV1 {
        candidates: vec![
            candidate("primary", "hash-a"),
            candidate("primary", "hash-a"),
        ],
        maximum_input_bytes: 64,
        maximum_output_bytes: 64,
    };
    assert_eq!(
        gateway.execute(
            &duplicates,
            &ModelRequestV1 {
                input: json!(null),
                parameters: Default::default()
            }
        ),
        Err(ProviderError::InvalidPlan)
    );
}

#[test]
fn an_oversized_request_is_a_context_condition_not_a_plan_defect() {
    let calls = Arc::new(AtomicUsize::new(0));
    let gateway = FrozenModelGateway::new(vec![Box::new(ScriptedProvider {
        binding: "primary",
        version: "hash-a",
        events: vec![ModelEventV1::Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache: Default::default(),
        }],
        acceptance: ProviderAcceptanceV1::Accepted,
        calls: calls.clone(),
    })]);
    // The plan itself is valid; only the exact request exceeds its input bound.
    let plan = ModelResolutionPlanV1 {
        candidates: vec![candidate("primary", "hash-a")],
        maximum_input_bytes: 8,
        maximum_output_bytes: 64,
    };
    let oversized = ModelRequestV1 {
        input: json!({"messages":[{"role":"user","content":"x".repeat(64)}]}),
        parameters: Default::default(),
    };
    assert!(
        matches!(
            gateway.execute(&plan, &oversized),
            Err(ProviderError::InputBoundExceeded { .. })
        ),
        "an over-bound request must stay distinguishable from a malformed plan"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "an over-bound request never reaches the provider"
    );
}

#[test]
fn one_redaction_set_covers_every_stream_and_outcomes_are_conservative() {
    let redactor = Redactor::new(vec!["top-secret".into(), "secret".into()]);
    let mut streaming = redactor.stream();
    let combined = format!(
        "{}{}{}",
        streaming.push("prefix top-"),
        streaming.push("secret suffix"),
        streaming.finish()
    );
    assert!(!combined.contains("top-secret"));
    assert!(combined.contains("[REDACTED]"));

    let contents: Vec<fn(String) -> NormalizedContentV1> = vec![
        NormalizedContentV1::AssistantOutput,
        NormalizedContentV1::StandardOutput,
        NormalizedContentV1::StandardError,
        NormalizedContentV1::ReasoningRaw,
        NormalizedContentV1::ReasoningSummary,
        NormalizedContentV1::Progress,
        NormalizedContentV1::Diagnostic,
        NormalizedContentV1::FinalResult,
        NormalizedContentV1::Error,
    ];
    let mut normalizer = InvocationNormalizer::new(id("invocation.redact"), redactor);
    for (index, constructor) in contents.into_iter().enumerate() {
        let event = normalizer
            .event(constructor("contains top-secret".into()))
            .expect("event");
        assert_eq!(event.sequence, u64::try_from(index).unwrap() + 1);
        assert!(!format!("{:?}", event.content).contains("top-secret"));
    }
    let terminal = normalizer
        .terminal(EffectEvidenceV1 {
            dispatch: DispatchEvidenceV1::Unknown,
            terminal: TerminalEvidenceV1::MissingOrConflicting,
            descriptor_is_idempotent: true,
            host_guarantees_same_id_deduplication: true,
        })
        .expect("terminal");
    assert_eq!(terminal.disposition, OutcomeDispositionV1::OutcomeUncertain);
    assert_eq!(terminal.retry_safety, RetrySafetyV1::NotSafe);
    assert_eq!(
        normalizer.event(NormalizedContentV1::Progress("late".into())),
        Err(NormalizeError::TerminalClosed)
    );
    assert_eq!(
        normalizer.terminal(EffectEvidenceV1 {
            dispatch: DispatchEvidenceV1::Started,
            terminal: TerminalEvidenceV1::Succeeded,
            descriptor_is_idempotent: false,
            host_guarantees_same_id_deduplication: false,
        }),
        Err(NormalizeError::TerminalClosed)
    );

    let definitely_not_started = classify_outcome(
        id("invocation.safe"),
        EffectEvidenceV1 {
            dispatch: DispatchEvidenceV1::DefinitelyNotStarted,
            terminal: TerminalEvidenceV1::Failed,
            descriptor_is_idempotent: false,
            host_guarantees_same_id_deduplication: false,
        },
    );
    assert_eq!(
        definitely_not_started.retry_safety,
        RetrySafetyV1::EligibleUnderFrozenPolicy
    );
    let same_id_only = classify_outcome(
        id("invocation.same-id"),
        EffectEvidenceV1 {
            dispatch: DispatchEvidenceV1::Started,
            terminal: TerminalEvidenceV1::Failed,
            descriptor_is_idempotent: false,
            host_guarantees_same_id_deduplication: true,
        },
    );
    assert_eq!(
        same_id_only.retry_safety,
        RetrySafetyV1::SameInvocationIdOnly
    );
}

#[derive(Clone)]
struct SecretClient {
    fields: Arc<Mutex<BTreeMap<String, Zeroizing<Vec<u8>>>>>,
    requests: Arc<Mutex<Vec<RedeemLeaseRequestV1>>>,
    revoked: Arc<Mutex<Vec<StableId>>>,
}

impl SecretLeaseClientV1 for SecretClient {
    fn redeem(
        &self,
        request: &RedeemLeaseRequestV1,
    ) -> Result<SecretDeliveryV1, SecretMaterializationError> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(SecretDeliveryV1 {
            fields: self.fields.lock().unwrap().clone(),
        })
    }

    fn revoke(&self, lease_id: &StableId) -> Result<(), SecretMaterializationError> {
        self.revoked.lock().unwrap().push(lease_id.clone());
        Ok(())
    }
}

#[test]
fn secret_materialization_is_exact_field_scoped_redacted_and_revocable() {
    let client = SecretClient {
        fields: Arc::new(Mutex::new(BTreeMap::from([
            ("api_key".into(), Zeroizing::new(b"top-secret".to_vec())),
            ("tenant".into(), Zeroizing::new(b"acme".to_vec())),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
        revoked: Arc::new(Mutex::new(Vec::new())),
    };
    let materializer = SecretMaterializer::new(client.clone());
    let plan = SecretMaterializationPlanV1 {
        decision_id: id("decision.1"),
        invocation_id: id("invocation.1"),
        host_generation: ProcessGeneration(7),
        lease: SecretLeaseHandleV1 {
            lease_id: id("lease.1"),
        },
        fields: vec![
            SecretFieldPlanV1 {
                field: "api_key".into(),
                target: InjectionTargetV1::Header("Authorization".into()),
            },
            SecretFieldPlanV1 {
                field: "tenant".into(),
                target: InjectionTargetV1::Environment("TENANT".into()),
            },
        ],
    };
    let materialized = materializer.materialize(&plan).expect("materialize");
    assert_eq!(
        materialized.value("api_key"),
        Some(b"top-secret".as_slice())
    );
    assert_eq!(
        materialized.target("tenant"),
        Some(&InjectionTargetV1::Environment("TENANT".into()))
    );
    assert_eq!(
        materialized.redactor().redact("value=top-secret"),
        "value=[REDACTED]"
    );
    let request = client.requests.lock().unwrap()[0].clone();
    assert_eq!(
        request.requested_fields,
        BTreeSet::from(["api_key".into(), "tenant".into()])
    );

    *client.fields.lock().unwrap() =
        BTreeMap::from([("unexpected".into(), Zeroizing::new(b"value".to_vec()))]);
    assert_eq!(
        materializer.materialize(&plan).err(),
        Some(SecretMaterializationError::FieldMismatch)
    );
    assert_eq!(client.revoked.lock().unwrap().as_slice(), &[id("lease.1")]);

    let mut invalid = plan;
    invalid.fields.push(invalid.fields[0].clone());
    assert_eq!(
        materializer.materialize(&invalid).err(),
        Some(SecretMaterializationError::InvalidPlan)
    );
}
