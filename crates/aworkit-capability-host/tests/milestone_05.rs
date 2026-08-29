use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use aworkit_capability_host::{
    AdapterRegistry, AdmissionDispositionV1, ApprovedInvocationEnvelopeV1,
    ArgumentVectorInvocationV1, BuiltInProcessTools, CancellationToken, CapabilityDescriptor,
    CapabilityHost, CapabilityKind, ControlledProcessResult, DispatchEvidenceV1, EffectEvidenceV1,
    FileAuthority, FileEditRequestV1, FileEffectKindV1, FileGrepRequestV1, FileListRequestV1,
    FileReadRequestV1, FileSearchRequestV1, FileToolError, FileWriteRequestV1, FrozenModelGateway,
    HermeticProcessPort, HermeticProcessStep, HostControlEnvelopeV1, HostControlKindV1, HostError,
    HostToolLimitsV1, InjectionTargetV1, InvocationNormalizer, ModelCandidateV1, ModelEventV1,
    ModelRequestV1, ModelResolutionPlanV1, NormalizeError, NormalizedContentV1,
    OutcomeDispositionV1, PlatformProcessPort, ProcessSpecV1, ProcessTermination, ProjectFiles,
    ProviderAcceptanceV1, ProviderEnginePortV1, ProviderError, PythonInvocationV1, Redactor,
    RedeemLeaseRequestV1, RetrySafetyV1, SecretDeliveryV1, SecretFieldPlanV1, SecretLeaseClientV1,
    SecretLeaseHandleV1, SecretMaterializationError, SecretMaterializationPlanV1,
    SecretMaterializer, ShellInvocationV1, SideEffectClass, TerminalEvidenceV1, ToolAdapterError,
    ToolAuthorityModeV1, classify_outcome,
};
use aworkit_protocol::{
    AttestedExtensionSetV1, ProcessGeneration, SchemaVersion, StableId,
    attested_extension_set_hash_v1,
};
use serde_json::json;
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
                maximum_matches: 8,
                maximum_files: 8,
                maximum_file_bytes: 1024,
            },
            &CancellationToken::default(),
        )
        .expect("grep");
    assert_eq!(grep.files_scanned, 2);
    assert_eq!(grep.matches.len(), 2);
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
    #[cfg(unix)]
    assert_eq!(observed[0].arguments, vec!["-c", "printf ok"]);
    #[cfg(windows)]
    assert_eq!(observed[0].arguments, vec!["/D", "/S", "/C", "printf ok"]);
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
                ModelEventV1::AssistantOutput("answer".into()),
                ModelEventV1::Usage {
                    input_tokens: 2,
                    output_tokens: 1,
                },
            ],
            acceptance: ProviderAcceptanceV1::Accepted,
            calls: second_calls.clone(),
        }),
    ]);
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
