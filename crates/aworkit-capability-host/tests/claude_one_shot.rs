//! One-shot Claude Code delegation against a real supervised child process.
//!
//! The fixture is launched exactly like the installed CLI, so these tests cover
//! process spawning, stdin submission, JSON-lines framing on a real pipe, and
//! process-tree teardown. They are skipped when no POSIX Python is available;
//! on Windows the classification logic is covered by the scripted stream tests
//! in `src/external_agent/claude_code.rs`.

use std::{
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use aworkit_capability_host::{
    CancellationToken, ClaudeOneShotBackendV1, ClaudeOneShotConfigV1, ClaudeOneShotLimitsV1,
    ClaudePermissionModeV1, ExternalAgentBackendRegistryV1, ExternalAgentBackendV1,
    OneShotDelegationV1, SubagentStartOptionsV1, SubagentStopReasonV1,
};
use aworkit_protocol::StableId;
use tempfile::TempDir;

fn fixture_executable() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_one_shot_fixture.py");
    path.is_file().then_some(path)
}

fn environment(
    mode: &str,
    sentinel: Option<&PathBuf>,
) -> Vec<aworkit_capability_host::CodexAppServerEnvironmentV1> {
    let mut environment = vec![aworkit_capability_host::CodexAppServerEnvironmentV1::new(
        "AWORKIT_CLAUDE_ONE_SHOT_MODE".to_owned(),
        zeroize::Zeroizing::new(mode.to_owned()),
    )];
    if let Some(sentinel) = sentinel {
        environment.push(aworkit_capability_host::CodexAppServerEnvironmentV1::new(
            "AWORKIT_CLAUDE_ONE_SHOT_SENTINEL".to_owned(),
            zeroize::Zeroizing::new(sentinel.display().to_string()),
        ));
    }
    environment
}

fn config(mode: &str, sentinel: Option<&PathBuf>) -> Option<ClaudeOneShotConfigV1> {
    Some(ClaudeOneShotConfigV1 {
        name: "claude-code".to_owned(),
        executable: fixture_executable()?,
        arguments: Vec::new(),
        working_directory: None,
        inherit_environment: true,
        environment: environment(mode, sentinel),
        permission_mode: ClaudePermissionModeV1::DontAsk,
        limits: ClaudeOneShotLimitsV1 {
            poll_interval: Duration::from_millis(20),
            ..ClaudeOneShotLimitsV1::default()
        },
    })
}

fn request(directory: &TempDir) -> OneShotDelegationV1 {
    OneShotDelegationV1 {
        run_id: StableId::parse("run.claude.fixture").expect("stable id"),
        task: "Summarize the fixture".to_owned(),
        working_directory: directory.path().to_path_buf(),
        deadline: Duration::from_secs(20),
        options: SubagentStartOptionsV1::default(),
    }
}

fn backend(mode: &str, sentinel: Option<&PathBuf>) -> Option<ClaudeOneShotBackendV1> {
    Some(ClaudeOneShotBackendV1::new(config(mode, sentinel)?).expect("valid configuration"))
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
}

#[test]
fn optional_model_and_effort_reach_the_child() {
    let directory = TempDir::new().expect("temporary directory");
    let Some(mut config) = config("success", None) else {
        return;
    };
    // The fixture requires both flags when it is told to expect them.
    config
        .environment
        .push(aworkit_capability_host::CodexAppServerEnvironmentV1::new(
            "AWORKIT_CLAUDE_FIXTURE_MODEL".to_owned(),
            zeroize::Zeroizing::new("sonnet".to_owned()),
        ));
    config
        .environment
        .push(aworkit_capability_host::CodexAppServerEnvironmentV1::new(
            "AWORKIT_CLAUDE_FIXTURE_EFFORT".to_owned(),
            zeroize::Zeroizing::new("high".to_owned()),
        ));
    let backend = ClaudeOneShotBackendV1::new(config).expect("valid configuration");
    let mut request = request(&directory);
    request.options.model = Some("sonnet".to_owned());
    request.options.reasoning_effort = Some("high".to_owned());

    let outcome = backend.run(&request, &CancellationToken::default());

    assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
    assert_eq!(outcome.answer.as_deref(), Some("fixture final answer"));
}

#[test]
fn denied_actions_are_reported_with_the_answer() {
    let directory = TempDir::new().expect("temporary directory");
    let Some(backend) = backend("denied", None) else {
        return;
    };

    let outcome = backend.run(&request(&directory), &CancellationToken::default());

    assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
    assert!(
        outcome
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("denied 3 action(s)"))
    );
}

#[test]
fn an_unauthenticated_child_is_reported_with_a_fixed_hint() {
    let directory = TempDir::new().expect("temporary directory");
    let Some(backend) = backend("not-logged-in", None) else {
        return;
    };

    let outcome = backend.run(&request(&directory), &CancellationToken::default());

    assert_eq!(outcome.stop_reason, SubagentStopReasonV1::AccessPolicy);
    assert!(
        outcome
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("not authenticated"))
    );
}

#[test]
fn a_stream_without_a_terminal_event_is_a_process_failure() {
    let directory = TempDir::new().expect("temporary directory");
    let Some(backend) = backend("silent", None) else {
        return;
    };

    let outcome = backend.run(&request(&directory), &CancellationToken::default());

    assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Process);
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
fn the_registry_admits_claude_code_and_gates_its_efforts() {
    let directory = TempDir::new().expect("temporary directory");
    let Some(backend) = backend("success", None) else {
        return;
    };
    let mut registry = ExternalAgentBackendRegistryV1::new();
    registry.register(Arc::new(backend)).expect("registers");

    assert_eq!(registry.names(), vec!["claude-code"]);
    let mut request = request(&directory);
    assert!(registry.resolve("claude-code", &request).is_ok());
    request.options.reasoning_effort = Some("medium".to_owned());
    assert!(registry.resolve("claude-code", &request).is_ok());
    request.options.reasoning_effort = Some("minimal".to_owned());
    assert!(registry.resolve("claude-code", &request).is_err());
}
