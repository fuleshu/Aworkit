use std::collections::{BTreeMap, BTreeSet};

use aworkit_capability_host::CancellationToken;
use aworkit_capability_host::{
    AdapterRegistry, ApprovedInvocationEnvelopeV1, ArtifactTransferV1, BackendAvailabilityV1,
    BackendDispatchV1, BackendExecutionLocationV1, BackendTerminalV1, BackendUnavailableReasonV1,
    BoundedResultTransferV1, CancellationEvidenceV1, CapabilityDescriptor, CapabilityHost,
    CapabilityKind, CleanupVerificationV1, EnforcementCategoryV1, EnforcementClaimV1,
    EnforcementReportV1, EnforcementVerificationV1, HermeticCleanupV1, HermeticIsolationBackend,
    HermeticIsolationRunV1, HermeticVerificationV1, IsolatedCommandV1, IsolatedExecutionV1,
    IsolationBackendManifestV1, IsolationEventErrorV1, IsolationGatewayDispatcherV1,
    IsolationGatewayRequestV1, IsolationOutcomeV1, IsolationProfileV1, IsolationRawEventV1,
    IsolationRequirementV1, IsolationRuntime, IsolationRuntimeError, IsolationStrengthV1,
    MountAccessV1, MountRealizationV1, NetworkPolicyV1, PinnedBackendIdentityV1, ProcessLimitsV1,
    ResidualStatePolicyV1, ResourceLimitsV1, SideEffectClass, TransferLimitsV1, UserPolicyV1,
    content_hash_v1,
};
use aworkit_protocol::{
    AttestedExtensionSetV1, EXTENSION_HOST_PROTOCOL_V1, ProcessGeneration, SchemaVersion, StableId,
    attested_extension_set_hash_v1,
};

const NOW: u64 = 10_000;
const DEADLINE: u64 = 20_000;

fn manifest(location: BackendExecutionLocationV1) -> IsolationBackendManifestV1 {
    IsolationBackendManifestV1 {
        backend_id: "isolation.hermetic".to_owned(),
        adapter_version: "1.0.0".to_owned(),
        adapter_hash: content_hash_v1(b"hermetic-adapter"),
        execution_location: location,
        supported_hosts: BTreeSet::from(["linux-test".to_owned()]),
        verifiable_enforcement: BTreeSet::from([
            EnforcementCategoryV1::Mounts,
            EnforcementCategoryV1::Network,
            EnforcementCategoryV1::User,
            EnforcementCategoryV1::Processes,
            EnforcementCategoryV1::Resources,
            EnforcementCategoryV1::ResidualState,
        ]),
        enforces_deadlines: true,
        supports_cancellation: true,
        verifies_cleanup: true,
        maximum_transfer_bytes: 64 * 1024,
    }
}

fn profile(manifest: &IsolationBackendManifestV1) -> IsolationProfileV1 {
    let mut profile = IsolationProfileV1 {
        profile_id: "profile.hermetic.strict".to_owned(),
        profile_version: "1".to_owned(),
        profile_hash: String::new(),
        requirement: IsolationRequirementV1::Required,
        backend: PinnedBackendIdentityV1 {
            backend_id: manifest.backend_id.clone(),
            adapter_version: manifest.adapter_version.clone(),
            adapter_hash: manifest.adapter_hash.clone(),
            environment_id: "environment.pinned".to_owned(),
            environment_hash: content_hash_v1(b"environment-pinned"),
        },
        workspace_id: "workspace.pinned".to_owned(),
        host_platform: "linux-test".to_owned(),
        mounts: BTreeSet::from([MountRealizationV1 {
            source: "/project".to_owned(),
            source_identity: content_hash_v1(b"project-root-identity"),
            target: "/workspace".to_owned(),
            access: MountAccessV1::ReadWrite,
        }]),
        network: NetworkPolicyV1::Denied,
        user: UserPolicyV1 {
            principal: "sandbox-user".to_owned(),
            host_user_visible: false,
            privilege_escalation_denied: true,
        },
        processes: ProcessLimitsV1 {
            maximum_processes: 8,
            maximum_open_files: 64,
            descendant_containment: true,
        },
        resources: ResourceLimitsV1 {
            memory_bytes: 256 * 1024 * 1024,
            cpu_time_millis: 5_000,
            writable_bytes: 16 * 1024 * 1024,
        },
        residual_state: ResidualStatePolicyV1::DestroyEnvironment,
    };
    profile.rehash().expect("valid pinned profile");
    profile
}

fn execution(profile: &IsolationProfileV1) -> IsolatedExecutionV1 {
    let input = b"approved input".to_vec();
    IsolatedExecutionV1 {
        invocation_id: "invocation.isolated.1".to_owned(),
        profile_id: profile.profile_id.clone(),
        profile_hash: profile.profile_hash.clone(),
        workspace_id: profile.workspace_id.clone(),
        deadline_epoch_millis: DEADLINE,
        command: IsolatedCommandV1 {
            program: "/runtime/tool".to_owned(),
            arguments: vec!["--json".to_owned()],
            working_directory: "/workspace".to_owned(),
            environment: BTreeMap::from([("LANG".to_owned(), "C.UTF-8".to_owned())]),
        },
        input_hash: content_hash_v1(&input),
        input,
        transfer_limits: TransferLimitsV1 {
            maximum_input_bytes: 1_024,
            maximum_event_count: 16,
            maximum_event_bytes: 1_024,
            maximum_stream_bytes: 4_096,
            maximum_result_bytes: 2_048,
            maximum_artifact_count: 4,
            maximum_artifact_bytes: 2_048,
            maximum_total_artifact_bytes: 4_096,
        },
    }
}

fn exact_report(profile: &IsolationProfileV1, session_id: &str) -> EnforcementReportV1 {
    EnforcementReportV1 {
        session_id: session_id.to_owned(),
        backend: profile.backend.clone(),
        profile_id: profile.profile_id.clone(),
        profile_hash: profile.profile_hash.clone(),
        claims: profile
            .expected_realizations()
            .into_iter()
            .map(|realization| EnforcementClaimV1 {
                evidence: format!("verified {:?}", realization.category()),
                realization,
                verification: EnforcementVerificationV1::Verified,
            })
            .collect(),
    }
}

fn verified_backend(
    location: BackendExecutionLocationV1,
) -> (
    HermeticIsolationBackend,
    IsolationBackendManifestV1,
    IsolationProfileV1,
) {
    let manifest = manifest(location);
    let profile = profile(&manifest);
    let backend = HermeticIsolationBackend::new(
        manifest.clone(),
        profile.backend.environment_id.clone(),
        profile.backend.environment_hash.clone(),
    );
    (backend, manifest, profile)
}

#[test]
fn verified_backend_reports_exact_strength_hashed_transfers_and_cleanup() {
    let (backend, _, profile) = verified_backend(BackendExecutionLocationV1::Local);
    let result_content = br#"{"ok":true}"#.to_vec();
    let artifact_content = b"artifact bytes".to_vec();
    backend
        .push_run(HermeticIsolationRunV1::successful(vec![
            IsolationRawEventV1::DispatchAccepted {
                receipt: "hermetic accepted".to_owned(),
            },
            IsolationRawEventV1::StandardOutput(b"progress".to_vec()),
            IsolationRawEventV1::Artifact(ArtifactTransferV1 {
                correlation_id: "artifact.1".to_owned(),
                relative_path: "reports/result.txt".to_owned(),
                media_type: "text/plain".to_owned(),
                content_hash: content_hash_v1(&artifact_content),
                content: artifact_content,
            }),
            IsolationRawEventV1::Result(BoundedResultTransferV1 {
                media_type: "application/json".to_owned(),
                content_hash: content_hash_v1(&result_content),
                content: result_content,
            }),
        ]))
        .expect("queue run");
    let runtime = IsolationRuntime::new(backend.clone());

    let report = runtime
        .execute_at(
            &profile,
            &execution(&profile),
            &CancellationToken::default(),
            NOW,
        )
        .expect("verified isolated run");

    assert_eq!(
        report.strength,
        IsolationStrengthV1::VerifiedSecurityBoundary
    );
    assert!(report.enforcement.is_verified_for(&profile));
    assert_eq!(report.events.len(), 4);
    assert_eq!(report.execution_outcome, IsolationOutcomeV1::Completed);
    assert_eq!(report.overall_outcome, IsolationOutcomeV1::Completed);
    assert_eq!(
        report.cleanup.process_tree_terminated,
        CleanupVerificationV1::Verified
    );
    assert_eq!(backend.observed().unwrap().executions.len(), 1);
}

#[test]
fn required_unavailable_backend_fails_closed_without_host_or_backend_dispatch() {
    let (backend, _, profile) = verified_backend(BackendExecutionLocationV1::Local);
    backend
        .set_availability(BackendAvailabilityV1::Unavailable(
            BackendUnavailableReasonV1::NotInstalled,
        ))
        .unwrap();
    let runtime = IsolationRuntime::new(backend.clone());

    let error = runtime
        .execute_at(
            &profile,
            &execution(&profile),
            &CancellationToken::default(),
            NOW,
        )
        .expect_err("required isolation must fail closed");

    assert!(matches!(
        error,
        IsolationRuntimeError::Unavailable {
            requirement: IsolationRequirementV1::Required,
            reason: BackendUnavailableReasonV1::NotInstalled,
        }
    ));
    assert!(!error.host_fallback_permitted());
    assert!(error.definitely_not_started());
    let observed = backend.observed().unwrap();
    assert!(observed.verified_profiles.is_empty());
    assert!(observed.executions.is_empty());
}

#[test]
fn environment_identity_drift_is_cleaned_and_never_executed() {
    let manifest = manifest(BackendExecutionLocationV1::Local);
    let profile = profile(&manifest);
    let backend = HermeticIsolationBackend::new(
        manifest,
        "environment.drifted",
        content_hash_v1(b"environment-drifted"),
    );
    backend
        .push_run(HermeticIsolationRunV1::successful(Vec::new()))
        .unwrap();
    let runtime = IsolationRuntime::new(backend.clone());

    let error = runtime
        .execute_at(
            &profile,
            &execution(&profile),
            &CancellationToken::default(),
            NOW,
        )
        .expect_err("environment drift must reject enforcement");

    match error {
        IsolationRuntimeError::EnforcementRejected {
            detail,
            cleanup: Some(cleanup),
        } => {
            assert!(detail.contains("identity drift"));
            assert_eq!(
                cleanup.environment_state_removed,
                CleanupVerificationV1::Verified
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let observed = backend.observed().unwrap();
    assert!(observed.executions.is_empty());
    assert_eq!(observed.cleanup_sessions.len(), 1);
}

#[test]
fn one_unverified_enforcement_category_prevents_dispatch() {
    let (backend, _, profile) = verified_backend(BackendExecutionLocationV1::Local);
    let mut report = exact_report(&profile, "session.unverified");
    report
        .claims
        .iter_mut()
        .find(|claim| claim.realization.category() == EnforcementCategoryV1::Network)
        .expect("network claim")
        .verification = EnforcementVerificationV1::Unverified;
    backend
        .set_verification(HermeticVerificationV1::Report(report))
        .unwrap();
    backend
        .push_run(HermeticIsolationRunV1::successful(Vec::new()))
        .unwrap();
    let runtime = IsolationRuntime::new(backend.clone());

    let error = runtime
        .execute_at(
            &profile,
            &execution(&profile),
            &CancellationToken::default(),
            NOW,
        )
        .expect_err("unverified networking must fail closed");

    assert!(matches!(
        error,
        IsolationRuntimeError::EnforcementRejected { ref detail, .. }
            if detail.contains("Network")
    ));
    assert!(backend.observed().unwrap().executions.is_empty());
}

#[test]
fn corrupt_artifact_hash_becomes_uncertain_and_cleanup_is_preserved() {
    let (backend, _, profile) = verified_backend(BackendExecutionLocationV1::Local);
    backend
        .push_run(HermeticIsolationRunV1::successful(vec![
            IsolationRawEventV1::DispatchAccepted {
                receipt: "remote accepted".to_owned(),
            },
            IsolationRawEventV1::Artifact(ArtifactTransferV1 {
                correlation_id: "artifact.corrupt".to_owned(),
                relative_path: "out/data.bin".to_owned(),
                media_type: "application/octet-stream".to_owned(),
                content_hash: content_hash_v1(b"different bytes"),
                content: b"actual bytes".to_vec(),
            }),
        ]))
        .unwrap();
    let runtime = IsolationRuntime::new(backend);

    let report = runtime
        .execute_at(
            &profile,
            &execution(&profile),
            &CancellationToken::default(),
            NOW,
        )
        .expect("post-dispatch violations return evidence reports");

    assert_eq!(
        report.contract_violation,
        Some(IsolationEventErrorV1::ArtifactIntegrity)
    );
    assert_eq!(report.events.len(), 1);
    assert_eq!(
        report.execution_outcome,
        IsolationOutcomeV1::OutcomeUncertain
    );
    assert_eq!(report.overall_outcome, IsolationOutcomeV1::OutcomeUncertain);
    assert_eq!(
        report.cleanup.environment_state_removed,
        CleanupVerificationV1::Verified
    );
}

#[test]
fn accepted_remote_loss_is_uncertain_and_cleanup_failure_cannot_be_hidden() {
    let (backend, _, profile) = verified_backend(BackendExecutionLocationV1::Remote);
    backend
        .push_run(HermeticIsolationRunV1 {
            events: vec![IsolationRawEventV1::DispatchAccepted {
                receipt: "remote dispatch accepted".to_owned(),
            }],
            terminal: Ok(BackendTerminalV1::RemoteLost {
                dispatch: BackendDispatchV1::Accepted,
                detail: "remote terminal disconnected".to_owned(),
            }),
            cleanup: HermeticCleanupV1::Failed("remote cleanup unconfirmed".to_owned()),
        })
        .unwrap();

    let report = IsolationRuntime::new(backend)
        .execute_at(
            &profile,
            &execution(&profile),
            &CancellationToken::default(),
            NOW,
        )
        .expect("remote loss is an evidence-bearing report");

    assert_eq!(
        report.execution_outcome,
        IsolationOutcomeV1::OutcomeUncertain
    );
    assert_eq!(report.overall_outcome, IsolationOutcomeV1::OutcomeUncertain);
    assert_eq!(
        report.cleanup.remote_session_closed,
        CleanupVerificationV1::Failed
    );
}

#[test]
fn confirmed_cancellation_keeps_effect_semantics_separate_from_cleanup() {
    let (backend, _, profile) = verified_backend(BackendExecutionLocationV1::Local);
    backend
        .push_run(HermeticIsolationRunV1 {
            events: vec![IsolationRawEventV1::DispatchAccepted {
                receipt: "accepted before cancellation".to_owned(),
            }],
            terminal: Ok(BackendTerminalV1::Cancelled {
                dispatch: BackendDispatchV1::Accepted,
                cancellation: CancellationEvidenceV1 {
                    requested: true,
                    backend_acknowledged: true,
                    terminal_confirmed: true,
                    evidence: "backend confirmed terminal cancellation".to_owned(),
                },
            }),
            cleanup: HermeticCleanupV1::Unverified("cleanup proof unavailable".to_owned()),
        })
        .unwrap();

    let report = IsolationRuntime::new(backend)
        .execute_at(
            &profile,
            &execution(&profile),
            &CancellationToken::default(),
            NOW,
        )
        .expect("cancelled run report");

    assert_eq!(report.execution_outcome, IsolationOutcomeV1::Cancelled);
    assert_eq!(report.overall_outcome, IsolationOutcomeV1::OutcomeUncertain);
}

#[test]
fn elapsed_deadline_and_prelaunch_cancellation_do_not_prepare_a_session() {
    let (backend, _, profile) = verified_backend(BackendExecutionLocationV1::Local);
    let runtime = IsolationRuntime::new(backend.clone());
    let deadline_error = runtime
        .execute_at(
            &profile,
            &execution(&profile),
            &CancellationToken::default(),
            DEADLINE,
        )
        .expect_err("elapsed deadline");
    assert!(matches!(
        deadline_error,
        IsolationRuntimeError::Unavailable {
            reason: BackendUnavailableReasonV1::DeadlineElapsed,
            ..
        }
    ));

    let cancellation = CancellationToken::default();
    cancellation.cancel();
    let cancelled_error = runtime
        .execute_at(&profile, &execution(&profile), &cancellation, NOW)
        .expect_err("prelaunch cancellation");
    assert!(matches!(
        cancelled_error,
        IsolationRuntimeError::Unavailable {
            reason: BackendUnavailableReasonV1::CancelledBeforeDispatch,
            ..
        }
    ));
    assert!(backend.observed().unwrap().verified_profiles.is_empty());
}

#[test]
fn approved_envelope_is_the_production_entrypoint_for_isolation_dispatch() {
    let (backend, _, profile) = verified_backend(BackendExecutionLocationV1::Local);
    backend
        .push_run(HermeticIsolationRunV1::successful(Vec::new()))
        .expect("queue isolated run");
    let mut descriptor = CapabilityDescriptor::build(
        "isolation.hermetic",
        "1.0.0",
        CapabilityKind::Isolation,
        SideEffectClass::Unknown,
    )
    .expect("isolation descriptor");
    descriptor.required_isolation = Some(profile.profile_id.clone());
    descriptor.rehash().expect("rehash descriptor");
    let mut registry = AdapterRegistry::default();
    registry
        .register_capability(descriptor.clone())
        .expect("register isolation descriptor");
    let mut attested = AttestedExtensionSetV1 {
        host_id: StableId::parse("host.isolation").expect("host ID"),
        host_generation: ProcessGeneration(12),
        host_protocol: EXTENSION_HOST_PROTOCOL_V1,
        extensions: Vec::new(),
        set_hash: String::new(),
    };
    attested.set_hash = attested_extension_set_hash_v1(&attested).expect("attested set hash");
    let frozen = registry
        .materialize_attested_set(&attested)
        .expect("core-attested frozen registry");
    let host = CapabilityHost::from_attested_registry(frozen, b"isolation-core-key".to_vec(), 4)
        .expect("production gateway");

    let mut approved_execution = execution(&profile);
    approved_execution.deadline_epoch_millis = u64::MAX - 1;
    let request = IsolationGatewayRequestV1 {
        profile: profile.clone(),
        execution: approved_execution.clone(),
    };
    let mut envelope = ApprovedInvocationEnvelopeV1 {
        schema_version: SchemaVersion::V1,
        invocation_id: StableId::parse(approved_execution.invocation_id.clone())
            .expect("invocation ID"),
        decision_id: StableId::parse("decision.isolation").expect("decision ID"),
        host_generation: ProcessGeneration(12),
        capability_id: descriptor.capability_id.clone(),
        adapter_version: descriptor.version.clone(),
        binding_hash: descriptor.version_hash.clone(),
        extension: None,
        required_isolation_profile: Some(profile.profile_id.clone()),
        kind: CapabilityKind::Isolation,
        enforced_scopes: Vec::new(),
        deadline_epoch_millis: approved_execution.deadline_epoch_millis,
        cancellation_token: StableId::parse("cancel.isolation").expect("cancel token"),
        lease_handles: Vec::new(),
        max_output_bytes: approved_execution.transfer_limits.maximum_result_bytes,
        payload: serde_json::to_value(request).expect("gateway payload"),
        core_authentication_tag: String::new(),
    };
    envelope
        .sign(b"isolation-core-key")
        .expect("sign approved invocation");
    let dispatcher = IsolationGatewayDispatcherV1::new(IsolationRuntime::new(backend));

    let dispatched = host
        .dispatch_v1(&envelope, NOW, &dispatcher)
        .expect("admit and dispatch")
        .output
        .expect("one-shot dispatch")
        .expect("verified isolation result");
    assert_eq!(dispatched.overall_outcome, IsolationOutcomeV1::Completed);
    assert_eq!(dispatched.invocation_id, approved_execution.invocation_id);
}
