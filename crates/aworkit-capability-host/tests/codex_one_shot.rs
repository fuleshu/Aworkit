//! One-shot Codex delegation against a real supervised child process.
//!
//! One gate is opt-in because it asserts environment inheritance and therefore
//! needs the variable in the launcher, not in the delegation config:
//!
//! ```text
//! AWORKIT_FIXTURE_AMBIENT_KEY=ambient-present \
//!   cargo test -p aworkit-capability-host --test codex_one_shot -- --ignored ambient
//! ```
//!
//! The fixture is launched exactly like the installed binary, so these tests
//! cover process spawning, JSON-lines framing on a real pipe, the reader
//! thread, and process-tree teardown. They are skipped when no POSIX Python is
//! available; on Windows the same protocol behavior is covered by the scripted
//! wire tests in `src/external_agent/codex_one_shot.rs`.

use std::{
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use aworkit_capability_host::{
    CancellationToken, CodexOneShotBackendV1, CodexOneShotConfigV1, CodexOneShotLimitsV1,
    CodexPermissionModeV1, ExternalAgentBackendRegistryV1, ExternalAgentBackendV1,
    OneShotDelegationV1, SubagentStopReasonV1,
};
use aworkit_protocol::StableId;
use tempfile::TempDir;

fn fixture_executable() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_one_shot_fixture.py");
    path.is_file().then_some(path)
}

fn config(mode: &str, sentinel: Option<&PathBuf>) -> Option<CodexOneShotConfigV1> {
    let executable = fixture_executable()?;
    let mut environment = vec![aworkit_capability_host::CodexAppServerEnvironmentV1::new(
        "AWORKIT_CODEX_ONE_SHOT_MODE".to_owned(),
        zeroize::Zeroizing::new(mode.to_owned()),
    )];
    if let Some(sentinel) = sentinel {
        environment.push(aworkit_capability_host::CodexAppServerEnvironmentV1::new(
            "AWORKIT_CODEX_ONE_SHOT_SENTINEL".to_owned(),
            zeroize::Zeroizing::new(sentinel.display().to_string()),
        ));
    }
    Some(CodexOneShotConfigV1 {
        name: "codex".to_owned(),
        executable,
        arguments: vec!["app-server".to_owned(), "--stdio".to_owned()],
        working_directory: None,
        inherit_environment: true,
        environment,
        permission_mode: CodexPermissionModeV1::Never,
        limits: CodexOneShotLimitsV1 {
            handshake_timeout: Duration::from_secs(10),
            poll_interval: Duration::from_millis(20),
            ..CodexOneShotLimitsV1::default()
        },
    })
}

fn request(directory: &TempDir) -> OneShotDelegationV1 {
    OneShotDelegationV1 {
        run_id: StableId::parse("run.fixture").expect("stable id"),
        task: "Summarize the fixture".to_owned(),
        working_directory: directory.path().to_path_buf(),
        deadline: Duration::from_secs(20),
        options: Default::default(),
    }
}

fn backend(mode: &str, sentinel: Option<&PathBuf>) -> Option<CodexOneShotBackendV1> {
    Some(CodexOneShotBackendV1::new(config(mode, sentinel)?).expect("valid configuration"))
}

#[test]
fn a_real_child_process_returns_the_final_answer() {
    let directory = TempDir::new().expect("temporary directory");
    let Some(backend) = backend("success", None) else {
        return;
    };

    let outcome = backend.run(&request(&directory), &CancellationToken::default());

    assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
    assert_eq!(outcome.answer.as_deref(), Some("fixture final answer"));
    assert!(outcome.diagnostic.is_none());
}

#[test]
fn approvals_and_user_input_are_answered_without_a_human() {
    let directory = TempDir::new().expect("temporary directory");
    // The fixture exits non-zero unless both server requests were answered with
    // the fixed unattended policy.
    let Some(backend) = backend("approval", None) else {
        return;
    };

    let outcome = backend.run(&request(&directory), &CancellationToken::default());

    assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
    assert_eq!(outcome.answer.as_deref(), Some("fixture final answer"));
    assert!(
        outcome
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("user input")),
        "expected the unattended fact in {:?}",
        outcome.diagnostic
    );
}

#[test]
fn a_completed_turn_with_only_commentary_is_an_invalid_result() {
    let directory = TempDir::new().expect("temporary directory");
    let Some(backend) = backend("commentary", None) else {
        return;
    };

    let outcome = backend.run(&request(&directory), &CancellationToken::default());

    assert_eq!(outcome.stop_reason, SubagentStopReasonV1::InvalidResult);
    assert!(outcome.answer.is_none());
}

#[test]
fn a_failed_turn_reports_its_normalized_category() {
    let directory = TempDir::new().expect("temporary directory");
    let Some(backend) = backend("failed", None) else {
        return;
    };

    let outcome = backend.run(&request(&directory), &CancellationToken::default());

    assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Limit);
    assert!(
        outcome
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("category: limit"))
    );
}

#[test]
fn a_non_ephemeral_thread_is_refused_without_submitting_a_turn() {
    let directory = TempDir::new().expect("temporary directory");
    let Some(backend) = backend("non-ephemeral", None) else {
        return;
    };

    let outcome = backend.run(&request(&directory), &CancellationToken::default());

    assert_eq!(outcome.stop_reason, SubagentStopReasonV1::ProductError);
    assert!(
        outcome
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("ephemeral"))
    );
}

#[test]
fn cancellation_terminates_the_complete_child_tree() {
    let directory = TempDir::new().expect("temporary directory");
    let sentinel = directory.path().join("descendant-leaked");
    let Some(backend) = backend("offspring", Some(&sentinel)) else {
        return;
    };
    let backend = Arc::new(backend);
    let request = request(&directory);
    let cancellation = CancellationToken::default();
    let worker = {
        let backend = Arc::clone(&backend);
        let cancellation = cancellation.clone();
        thread::spawn(move || backend.run(&request, &cancellation))
    };

    // Let the run reach the point where the fixture has spawned its descendant.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(500) {
        thread::sleep(Duration::from_millis(50));
    }
    cancellation.cancel();
    let outcome = worker.join().expect("the run thread settles");

    assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Aborted);
    thread::sleep(Duration::from_secs(2));
    assert!(
        !sentinel.exists(),
        "cancellation must terminate descendants in the child process group"
    );
}

#[test]
fn a_registered_codex_backend_exposes_no_optional_start_capabilities() {
    let directory = TempDir::new().expect("temporary directory");
    let Some(backend) = backend("success", None) else {
        return;
    };
    let mut registry = ExternalAgentBackendRegistryV1::new();
    registry.register(Arc::new(backend)).expect("registers");

    assert_eq!(registry.names(), vec!["codex"]);
    let request = request(&directory);
    assert!(registry.resolve("codex", &request).is_ok());
}

/// A delegated child inherits the environment that launched Aworkit, so an
/// operator can supply a product API key without storing it anywhere in Aworkit.
/// The scripted peer fails the run unless the variable arrived from the
/// launcher, so a completed run is the proof.
#[test]
#[ignore = "requires AWORKIT_FIXTURE_AMBIENT_KEY in the launcher environment"]
fn an_ambient_environment_variable_reaches_the_delegated_child() {
    let Some(backend) = backend("ambient", None) else {
        return;
    };
    let directory = TempDir::new().expect("temporary directory");

    let outcome = backend.run(&request(&directory), &CancellationToken::default());

    assert_eq!(
        outcome.stop_reason,
        SubagentStopReasonV1::Completed,
        "the child did not inherit the launcher environment: {:?}",
        outcome.diagnostic
    );
    assert_eq!(outcome.answer.as_deref(), Some("fixture final answer"));
}
