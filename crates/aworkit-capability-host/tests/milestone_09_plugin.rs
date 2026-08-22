use std::{collections::BTreeMap, path::PathBuf, process::Command, time::Duration};

use aworkit_capability_host::{
    AttestedPluginPinV1, NativePluginProcessV1, OutcomeDispositionV1, PinnedPluginManifestV1,
    PluginCancelResultV1, PluginDispatchPhaseV1, PluginEffectStatusV1, PluginFrameCodecV1,
    PluginFrameError, PluginHandshakeResultV1, PluginHealthResultV1, PluginHealthStatusV1,
    PluginInvocationEventKindV1, PluginInvocationEventV1, PluginInvocationRequestV1,
    PluginInvocationResultV1, PluginLifecycleError, PluginLifecycleLimitsV1,
    PluginLifecycleStateV1, PluginManifestError, PluginManifestLimitsV1, PluginPinError,
    PluginProcessError, PluginProcessLimitsV1, PluginProtocolErrorV1, PluginProtocolLimitsV1,
    PluginProtocolMessageV1, PluginReplayDispositionV1, PluginRestartPolicyV1,
    PluginShutdownRequestV1, PluginTerminalStatusV1, ProcessSpecV1, RetrySafetyV1,
    TRUSTED_PLUGIN_SECURITY_DISCLOSURE, TrustedPluginLifecycleV1, parse_extension_manifest_v1,
};
use aworkit_protocol::{ProcessGeneration, StableId};
use serde_json::{Value, json};

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("stable test ID")
}

fn content_hash(character: char) -> String {
    format!("sha256:{}", character.to_string().repeat(64))
}

fn manifest_json(program: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "extensionId": "extension.review",
        "version": "2.3.1",
        "contentHash": content_hash('a'),
        "aworkitVersionRequirement": ">=0.1.0,<0.2.0",
        "protocolVersion": 1,
        "entryPoint": {
            "program": program,
            "arguments": ["--plugin-protocol"]
        },
        "contributions": [
            {
                "contributionId": "tool.review",
                "kind": "tool",
                "inputSchema": {"type": "object"},
                "outputSchema": {"type": "object"},
                "futureCapability": {"retained": true}
            },
            {
                "contributionId": "evaluator.review",
                "kind": "evaluator",
                "inputSchema": {"type": "string"},
                "outputSchema": {"type": "boolean"}
            }
        ],
        "dependencies": [
            {
                "extensionId": "extension.shared",
                "versionRequirement": "^1.0.0"
            }
        ]
    }))
    .expect("manifest JSON")
}

fn manifest() -> aworkit_capability_host::ExtensionManifestV1 {
    parse_extension_manifest_v1(
        &manifest_json("/definitely/not/an/executable"),
        PluginManifestLimitsV1::default(),
    )
    .expect("manifest")
}

fn pin() -> AttestedPluginPinV1 {
    AttestedPluginPinV1 {
        extension_id: id("extension.review"),
        version: "2.3.1".into(),
        content_hash: content_hash('a'),
        protocol_version: 1,
        // Deliberately not in protocol order; the lifecycle canonicalizes the
        // handshake while exact-pin verification compares the complete set.
        contribution_ids: vec![id("tool.review"), id("evaluator.review")],
        host_generation: ProcessGeneration(41),
        enabled: true,
        compatible: true,
    }
}

fn pinned() -> PinnedPluginManifestV1 {
    PinnedPluginManifestV1::verify(manifest(), pin(), ProcessGeneration(41)).expect("pin")
}

fn lifecycle(maximum_restart_attempts: u32) -> TrustedPluginLifecycleV1 {
    TrustedPluginLifecycleV1::new(
        pinned(),
        PluginRestartPolicyV1 {
            maximum_restart_attempts,
            initial_backoff_millis: 10,
            maximum_backoff_millis: 100,
        },
    )
    .expect("lifecycle")
}

fn invoke(invocation_id: &str) -> PluginInvocationRequestV1 {
    PluginInvocationRequestV1 {
        invocation_id: id(invocation_id),
        contribution_id: id("tool.review"),
        input: json!({"path": "src/lib.rs"}),
        deadline_epoch_millis: 10_000,
    }
}

fn start_healthy(runtime: &mut TrustedPluginLifecycleV1) {
    runtime.begin_launch().expect("launch");
    let result = PluginHandshakeResultV1 {
        accepted: true,
        observed: runtime.expected_handshake().expected,
        error: None,
    };
    runtime.complete_handshake(&result, 100).expect("handshake");
}

fn python_program() -> PathBuf {
    let mut candidates = Vec::new();
    for key in ["PYTHON3", "PYTHON"] {
        if let Some(value) = std::env::var_os(key) {
            candidates.push(PathBuf::from(value));
        }
    }
    candidates.extend([
        PathBuf::from("/usr/bin/python3"),
        PathBuf::from("python3"),
        PathBuf::from("python"),
    ]);
    candidates
        .into_iter()
        .find(|candidate| {
            Command::new(candidate)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .expect("a Python 3 interpreter for the language-neutral fixture")
}

fn fixture_process_spec() -> ProcessSpecV1 {
    ProcessSpecV1 {
        program: python_program(),
        arguments: vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/plugin_protocol_fixture.py")
                .to_string_lossy()
                .into_owned(),
        ],
        working_directory: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))),
        environment: BTreeMap::from([("PYTHONUNBUFFERED".into(), "1".into())]),
        timeout: Duration::from_secs(2),
        maximum_output_bytes: 64 * 1024,
        cancellation_grace: Duration::from_millis(100),
    }
}

fn spawn_fixture(codec: PluginFrameCodecV1) -> NativePluginProcessV1 {
    NativePluginProcessV1::spawn(
        &fixture_process_spec(),
        codec,
        PluginProcessLimitsV1 {
            maximum_queued_frames: 8,
            maximum_stderr_bytes: 4096,
        },
    )
    .expect("supervised plugin fixture")
}

fn handshake_fixture(
    runtime: &mut TrustedPluginLifecycleV1,
    child: &mut NativePluginProcessV1,
    codec: PluginFrameCodecV1,
) {
    runtime.begin_launch().expect("launch");
    let request = codec
        .frame(
            id("host.handshake"),
            PluginProtocolMessageV1::HandshakeRequest(runtime.expected_handshake()),
        )
        .expect("handshake frame");
    child.send(&request).expect("send handshake");
    let response = child
        .receive(Duration::from_secs(2))
        .expect("handshake response");
    let PluginProtocolMessageV1::HandshakeResult(result) = response.message else {
        panic!("fixture returned a non-handshake response")
    };
    runtime
        .complete_handshake(&result, 100)
        .expect("complete handshake");
}

#[test]
fn manifest_parsing_is_inert_bounded_and_preserves_safe_unknown_contribution_fields() {
    let parsed = manifest();
    assert_eq!(parsed.entry_point.program, "/definitely/not/an/executable");
    assert_eq!(
        parsed.contributions[0]
            .opaque_fields
            .get("futureCapability"),
        Some(&json!({"retained": true}))
    );

    let mut limits = PluginManifestLimitsV1::default();
    limits.maximum_manifest_bytes = 32;
    assert!(matches!(
        parse_extension_manifest_v1(&manifest_json("missing"), limits),
        Err(PluginManifestError::ManifestSize)
    ));

    let duplicate = manifest_json("missing");
    let mut value: Value = serde_json::from_slice(&duplicate).expect("JSON");
    let contributions = value["contributions"]
        .as_array_mut()
        .expect("contributions");
    contributions[1]["contributionId"] = json!("tool.review");
    assert!(matches!(
        parse_extension_manifest_v1(
            &serde_json::to_vec(&value).expect("encode"),
            PluginManifestLimitsV1::default()
        ),
        Err(PluginManifestError::DuplicateContribution)
    ));
}

#[test]
fn exact_pin_rejects_disabled_incompatible_version_hash_generation_and_contribution_drift() {
    let manifest = manifest();

    let mut candidate = pin();
    candidate.enabled = false;
    assert_eq!(
        PinnedPluginManifestV1::verify(manifest.clone(), candidate, ProcessGeneration(41)),
        Err(PluginPinError::Disabled)
    );

    let mut candidate = pin();
    candidate.compatible = false;
    assert_eq!(
        PinnedPluginManifestV1::verify(manifest.clone(), candidate, ProcessGeneration(41)),
        Err(PluginPinError::Incompatible)
    );

    let mut candidate = pin();
    candidate.version = "2.3.2".into();
    assert_eq!(
        PinnedPluginManifestV1::verify(manifest.clone(), candidate, ProcessGeneration(41)),
        Err(PluginPinError::VersionDrift)
    );

    let mut candidate = pin();
    candidate.content_hash = content_hash('b');
    assert_eq!(
        PinnedPluginManifestV1::verify(manifest.clone(), candidate, ProcessGeneration(41)),
        Err(PluginPinError::ContentHashDrift)
    );

    assert_eq!(
        PinnedPluginManifestV1::verify(manifest.clone(), pin(), ProcessGeneration(42)),
        Err(PluginPinError::HostGenerationDrift)
    );

    let mut candidate = pin();
    candidate.contribution_ids.pop();
    assert_eq!(
        PinnedPluginManifestV1::verify(manifest, candidate, ProcessGeneration(41)),
        Err(PluginPinError::ContributionDrift)
    );
}

#[test]
fn framed_protocol_round_trips_split_and_coalesced_messages_with_generation_fencing() {
    let codec = PluginFrameCodecV1::new(ProcessGeneration(41), PluginProtocolLimitsV1::default())
        .expect("codec");
    let first = codec
        .frame(
            id("message.1"),
            PluginProtocolMessageV1::HandshakeRequest(
                TrustedPluginLifecycleV1::new(pinned(), PluginRestartPolicyV1::default())
                    .expect("lifecycle")
                    .expected_handshake(),
            ),
        )
        .expect("frame");
    let second = codec
        .frame(
            id("message.2"),
            PluginProtocolMessageV1::HealthResult(PluginHealthResultV1 {
                probe_id: id("probe.1"),
                status: PluginHealthStatusV1::Healthy,
                detail: None,
            }),
        )
        .expect("frame");
    let first_bytes = codec.encode(&first).expect("encode");
    let second_bytes = codec.encode(&second).expect("encode");
    let mut stream = codec.decoder();
    assert!(stream.push(&first_bytes[..3]).expect("partial").is_empty());
    let mut remainder = first_bytes[3..].to_vec();
    remainder.extend_from_slice(&second_bytes);
    assert_eq!(
        stream.push(&remainder).expect("frames"),
        vec![first, second]
    );
    stream.finish().expect("complete stream");

    let stale_codec =
        PluginFrameCodecV1::new(ProcessGeneration(42), PluginProtocolLimitsV1::default())
            .expect("stale codec");
    assert!(matches!(
        stale_codec.decode(&first_bytes),
        Err(PluginFrameError::HostGenerationDrift)
    ));
}

#[test]
fn handshake_and_invocation_follow_ordered_protocol_and_settle_once() {
    let mut runtime = lifecycle(3);
    assert!(!runtime.is_security_sandbox());
    assert_eq!(
        runtime.security_disclosure(),
        TRUSTED_PLUGIN_SECURITY_DISCLOSURE
    );
    assert!(
        runtime
            .security_disclosure()
            .contains("not a security sandbox")
    );
    start_healthy(&mut runtime);

    let request = invoke("invocation.success");
    runtime.begin_invocation(&request).expect("prepare");
    assert_eq!(
        runtime.active_dispatch_phase(),
        Some(PluginDispatchPhaseV1::Prepared)
    );
    runtime
        .mark_invocation_sent(&request.invocation_id)
        .expect("sent");
    runtime
        .accept_invocation(&request.invocation_id)
        .expect("accepted");
    runtime
        .observe_invocation_event(&PluginInvocationEventV1 {
            invocation_id: request.invocation_id.clone(),
            sequence: 1,
            event: PluginInvocationEventKindV1::Progress("working".into()),
        })
        .expect("progress");
    runtime
        .observe_invocation_event(&PluginInvocationEventV1 {
            invocation_id: request.invocation_id.clone(),
            sequence: 2,
            event: PluginInvocationEventKindV1::EffectMayHaveStarted,
        })
        .expect("effect evidence");
    let settlement = runtime
        .finish_invocation(&PluginInvocationResultV1 {
            invocation_id: request.invocation_id.clone(),
            status: PluginTerminalStatusV1::Succeeded,
            effect: PluginEffectStatusV1::Started,
            output: Some(json!({"reviewed": true})),
            error: None,
        })
        .expect("settle");
    assert_eq!(
        settlement.outcome.disposition,
        OutcomeDispositionV1::Succeeded
    );
    assert_eq!(settlement.replay, PluginReplayDispositionV1::NeverReplay);
    assert_eq!(runtime.state(), PluginLifecycleStateV1::Healthy);
    assert_eq!(
        runtime.begin_invocation(&request),
        Err(PluginLifecycleError::InvocationAlreadySettled)
    );
}

#[test]
fn crash_after_ambiguous_dispatch_is_uncertain_never_replayed_and_blocks_restart() {
    let mut runtime = lifecycle(3);
    start_healthy(&mut runtime);
    let request = invoke("invocation.uncertain");
    runtime.begin_invocation(&request).expect("prepare");
    runtime
        .mark_invocation_sent(&request.invocation_id)
        .expect("sent");

    let settlement = runtime
        .process_crashed(200, "child exited without a terminal frame")
        .expect("record crash")
        .expect("active settlement");
    assert_eq!(
        settlement.outcome.disposition,
        OutcomeDispositionV1::OutcomeUncertain
    );
    assert_eq!(settlement.outcome.retry_safety, RetrySafetyV1::NotSafe);
    assert_eq!(settlement.replay, PluginReplayDispositionV1::NeverReplay);
    assert_eq!(runtime.state(), PluginLifecycleStateV1::OutcomeUncertain);
    assert_eq!(
        runtime.begin_restart(10_000),
        Err(PluginLifecycleError::InvalidState)
    );
    assert_eq!(
        runtime.begin_invocation(&invoke("invocation.other")),
        Err(PluginLifecycleError::InvalidState)
    );
    runtime
        .quarantine_uncertain("manual inspection required")
        .expect("quarantine");
    assert_eq!(runtime.state(), PluginLifecycleStateV1::Quarantined);
}

#[test]
fn pre_dispatch_crash_allows_only_a_core_created_attempt_after_explicit_backoff_restart() {
    let mut runtime = lifecycle(2);
    start_healthy(&mut runtime);
    let request = invoke("invocation.not-started");
    runtime.begin_invocation(&request).expect("prepare");
    let settlement = runtime
        .process_crashed(300, "process disappeared before write")
        .expect("crash")
        .expect("settlement");
    assert_eq!(
        settlement.outcome.disposition,
        OutcomeDispositionV1::FailedDefiniteNotStarted
    );
    assert_eq!(
        settlement.replay,
        PluginReplayDispositionV1::CoreMayCreateNewAttempt
    );
    assert_eq!(runtime.state(), PluginLifecycleStateV1::RestartBackoff);
    assert_eq!(runtime.restart_not_before_millis(), Some(310));
    assert_eq!(
        runtime.begin_restart(309),
        Err(PluginLifecycleError::RestartBackoffActive)
    );
    runtime.begin_restart(310).expect("restart");
    let result = PluginHandshakeResultV1 {
        accepted: true,
        observed: runtime.expected_handshake().expected,
        error: None,
    };
    runtime.complete_handshake(&result, 311).expect("handshake");
    assert_eq!(
        runtime.begin_invocation(&request),
        Err(PluginLifecycleError::InvocationAlreadySettled)
    );
    runtime
        .begin_invocation(&invoke("invocation.new-attempt"))
        .expect("new core attempt");
}

#[test]
fn restart_budget_disable_drift_cancellation_and_shutdown_fail_closed() {
    let mut runtime = lifecycle(1);
    runtime.begin_launch().expect("launch");
    runtime
        .launch_failed(10, "launch failure")
        .expect("failure");
    assert_eq!(runtime.restart_not_before_millis(), Some(20));
    runtime.begin_restart(20).expect("restart");
    runtime
        .launch_failed(21, "second launch failure")
        .expect("budget exhaustion");
    assert_eq!(runtime.state(), PluginLifecycleStateV1::Quarantined);
    assert!(
        runtime
            .state_reason()
            .expect("reason")
            .contains("budget exhausted")
    );

    let mut runtime = lifecycle(3);
    start_healthy(&mut runtime);
    let request = invoke("invocation.cancel");
    runtime.begin_invocation(&request).expect("prepare");
    runtime
        .mark_invocation_sent(&request.invocation_id)
        .expect("sent");
    runtime
        .accept_invocation(&request.invocation_id)
        .expect("accepted");
    runtime
        .request_cancel(&request.invocation_id)
        .expect("request cancel");
    assert!(
        runtime
            .apply_cancel_result(&PluginCancelResultV1 {
                invocation_id: request.invocation_id.clone(),
                confirmed: false,
                effect: PluginEffectStatusV1::Unknown,
            })
            .expect("unconfirmed")
            .is_none()
    );
    let cancelled = runtime
        .apply_cancel_result(&PluginCancelResultV1 {
            invocation_id: request.invocation_id,
            confirmed: true,
            effect: PluginEffectStatusV1::Started,
        })
        .expect("confirmed")
        .expect("settled");
    assert_eq!(
        cancelled.outcome.disposition,
        OutcomeDispositionV1::CancelledWithEvidence
    );

    let drift = runtime
        .observe_identity(
            &id("extension.review"),
            "2.3.2",
            &content_hash('b'),
            ProcessGeneration(41),
        )
        .expect("drift");
    assert!(drift.is_none());
    assert_eq!(runtime.state(), PluginLifecycleStateV1::Quarantined);

    let mut runtime = lifecycle(3);
    start_healthy(&mut runtime);
    runtime.begin_shutdown().expect("shutdown");
    assert_eq!(
        runtime.complete_shutdown(false),
        Err(PluginLifecycleError::UncleanShutdown)
    );
    assert_eq!(runtime.state(), PluginLifecycleStateV1::Quarantined);
}

#[test]
fn rejected_handshake_and_health_failure_enter_bounded_restart_without_execution() {
    let mut runtime = lifecycle(3);
    runtime.begin_launch().expect("launch");
    let rejection = PluginHandshakeResultV1 {
        accepted: false,
        observed: runtime.expected_handshake().expected,
        error: Some(PluginProtocolErrorV1 {
            code: "unsupported".into(),
            message: "protocol rejected".into(),
        }),
    };
    assert_eq!(
        runtime.complete_handshake(&rejection, 1_000),
        Err(PluginLifecycleError::HandshakeRejected)
    );
    assert_eq!(runtime.state(), PluginLifecycleStateV1::RestartBackoff);

    runtime.begin_restart(1_010).expect("restart");
    let accepted = PluginHandshakeResultV1 {
        accepted: true,
        observed: runtime.expected_handshake().expected,
        error: None,
    };
    runtime
        .complete_handshake(&accepted, 1_011)
        .expect("healthy");
    runtime
        .observe_health(
            &PluginHealthResultV1 {
                probe_id: id("probe.degraded"),
                status: PluginHealthStatusV1::Degraded,
                detail: Some("heartbeat stalled".into()),
            },
            2_000,
        )
        .expect("degraded");
    assert_eq!(runtime.state(), PluginLifecycleStateV1::RestartBackoff);
    assert_eq!(runtime.state_reason(), Some("heartbeat stalled"));
}

#[test]
fn healthy_handshakes_do_not_replenish_the_lifetime_restart_budget() {
    let mut runtime = lifecycle(2);
    start_healthy(&mut runtime);
    for (crashed_at, restart_at) in [(1_000, 1_010), (2_000, 2_020)] {
        assert!(
            runtime
                .process_crashed(crashed_at, "idle plugin crash")
                .expect("crash")
                .is_none()
        );
        runtime.begin_restart(restart_at).expect("restart");
        let accepted = PluginHandshakeResultV1 {
            accepted: true,
            observed: runtime.expected_handshake().expected,
            error: None,
        };
        runtime
            .complete_handshake(&accepted, restart_at + 1)
            .expect("healthy handshake");
    }
    assert_eq!(runtime.restart_attempts(), 2);
    assert!(
        runtime
            .process_crashed(3_000, "third idle crash")
            .expect("budget exhaustion")
            .is_none()
    );
    assert_eq!(runtime.state(), PluginLifecycleStateV1::Quarantined);
    assert!(
        runtime
            .state_reason()
            .expect("reason")
            .contains("restart budget exhausted")
    );
}

#[test]
fn settlement_retention_is_bounded_and_capacity_exhaustion_fails_closed() {
    assert!(matches!(
        TrustedPluginLifecycleV1::new_with_limits(
            pinned(),
            PluginRestartPolicyV1::default(),
            PluginLifecycleLimitsV1 {
                maximum_retained_settlements: usize::MAX,
            },
        ),
        Err(PluginLifecycleError::InvalidLifecycleLimits)
    ));
    let mut runtime = TrustedPluginLifecycleV1::new_with_limits(
        pinned(),
        PluginRestartPolicyV1::default(),
        PluginLifecycleLimitsV1 {
            maximum_retained_settlements: 2,
        },
    )
    .expect("bounded lifecycle");
    start_healthy(&mut runtime);
    for invocation_id in ["invocation.retained-1", "invocation.retained-2"] {
        let request = invoke(invocation_id);
        runtime.begin_invocation(&request).expect("prepare");
        runtime
            .mark_invocation_sent(&request.invocation_id)
            .expect("sent");
        runtime
            .accept_invocation(&request.invocation_id)
            .expect("accepted");
        runtime
            .finish_invocation(&PluginInvocationResultV1 {
                invocation_id: request.invocation_id,
                status: PluginTerminalStatusV1::Succeeded,
                effect: PluginEffectStatusV1::Started,
                output: None,
                error: None,
            })
            .expect("settled");
    }
    assert_eq!(runtime.retained_settlement_count(), 2);
    assert_eq!(
        runtime.begin_invocation(&invoke("invocation.over-capacity")),
        Err(PluginLifecycleError::SettlementCapacityExhausted)
    );
    assert_eq!(runtime.retained_settlement_count(), 2);
    assert_eq!(runtime.state(), PluginLifecycleStateV1::Quarantined);
}

#[test]
fn oversized_transport_chunk_is_rejected_before_decoder_buffer_growth() {
    let limits = PluginProtocolLimitsV1 {
        maximum_frame_bytes: 64,
        maximum_text_bytes: 32,
        maximum_value_bytes: 32,
        maximum_contributions: 2,
    };
    let codec = PluginFrameCodecV1::new(ProcessGeneration(41), limits).expect("codec");
    let mut decoder = codec.decoder();
    assert!(matches!(
        decoder.push(&vec![0_u8; limits.maximum_frame_bytes + 5]),
        Err(PluginFrameError::ChunkTooLarge)
    ));
    decoder
        .finish()
        .expect("oversized chunk was never appended");
}

#[test]
fn supervised_language_neutral_subprocess_handshakes_invokes_and_shuts_down_cleanly() {
    let codec = PluginFrameCodecV1::new(ProcessGeneration(41), PluginProtocolLimitsV1::default())
        .expect("codec");
    let mut runtime = lifecycle(3);
    let mut child = spawn_fixture(codec);
    handshake_fixture(&mut runtime, &mut child, codec);

    let request = invoke("invocation.subprocess");
    runtime.begin_invocation(&request).expect("prepare");
    let frame = codec
        .frame(
            id("host.invoke"),
            PluginProtocolMessageV1::InvocationRequest(request.clone()),
        )
        .expect("invocation frame");
    child.send(&frame).expect("dispatch");
    runtime
        .mark_invocation_sent(&request.invocation_id)
        .expect("sent");

    let accepted = child.receive(Duration::from_secs(2)).expect("accepted");
    let PluginProtocolMessageV1::InvocationAccepted(accepted) = accepted.message else {
        panic!("fixture did not accept the invocation")
    };
    runtime
        .accept_invocation(&accepted.invocation_id)
        .expect("accepted evidence");
    let event = child.receive(Duration::from_secs(2)).expect("event");
    let PluginProtocolMessageV1::InvocationEvent(event) = event.message else {
        panic!("fixture did not emit invocation evidence")
    };
    runtime
        .observe_invocation_event(&event)
        .expect("effect event");
    let result = child.receive(Duration::from_secs(2)).expect("result");
    let PluginProtocolMessageV1::InvocationResult(result) = result.message else {
        panic!("fixture did not emit a terminal result")
    };
    let settled = runtime.finish_invocation(&result).expect("settled");
    assert_eq!(settled.outcome.disposition, OutcomeDispositionV1::Succeeded);

    runtime.begin_shutdown().expect("begin shutdown");
    let shutdown = codec
        .frame(
            id("host.shutdown"),
            PluginProtocolMessageV1::ShutdownRequest(PluginShutdownRequestV1 {
                reason: "test complete".into(),
            }),
        )
        .expect("shutdown frame");
    child.send(&shutdown).expect("send shutdown");
    let response = child
        .receive(Duration::from_secs(2))
        .expect("shutdown result");
    let PluginProtocolMessageV1::ShutdownResult(response) = response.message else {
        panic!("fixture did not confirm shutdown")
    };
    let exit = child
        .wait_for_exit(Duration::from_secs(2))
        .expect("clean process exit");
    runtime
        .complete_shutdown(response.clean && !exit.forced && exit.exit_code == Some(0))
        .expect("clean lifecycle shutdown");
    assert_eq!(runtime.state(), PluginLifecycleStateV1::Stopped);
}

#[test]
fn supervised_subprocess_crash_after_dispatch_is_uncertain_and_captures_bounded_diagnostics() {
    let codec = PluginFrameCodecV1::new(ProcessGeneration(41), PluginProtocolLimitsV1::default())
        .expect("codec");
    let mut runtime = lifecycle(3);
    let mut child = spawn_fixture(codec);
    handshake_fixture(&mut runtime, &mut child, codec);

    let mut request = invoke("invocation.subprocess-crash");
    request.input = json!({"crash": true});
    runtime.begin_invocation(&request).expect("prepare");
    let frame = codec
        .frame(
            id("host.crash"),
            PluginProtocolMessageV1::InvocationRequest(request.clone()),
        )
        .expect("crash frame");
    child.send(&frame).expect("dispatch");
    runtime
        .mark_invocation_sent(&request.invocation_id)
        .expect("sent");
    assert!(matches!(
        child.receive(Duration::from_secs(2)),
        Err(PluginProcessError::ProtocolStreamClosed)
            | Err(PluginProcessError::ProtocolStreamFailed(_))
    ));
    let exit = child
        .wait_for_exit(Duration::from_secs(2))
        .expect("crashed process reaped");
    assert_eq!(exit.exit_code, Some(17));
    let diagnostics = child.diagnostics().expect("diagnostics");
    assert!(!diagnostics.truncated);
    assert!(String::from_utf8_lossy(&diagnostics.stderr).contains("simulated crash after request"));

    let settlement = runtime
        .process_crashed(500, "fixture exited after dispatch")
        .expect("crash evidence")
        .expect("active settlement");
    assert_eq!(
        settlement.outcome.disposition,
        OutcomeDispositionV1::OutcomeUncertain
    );
    assert_eq!(settlement.outcome.retry_safety, RetrySafetyV1::NotSafe);
    assert_eq!(settlement.replay, PluginReplayDispositionV1::NeverReplay);
}
