use std::{path::PathBuf, thread, time::Duration};

use aworkit_capability_host::{
    CodexAppServerEnvironmentV1, CodexAppServerProbeConfigV1, CodexAppServerProbeError,
    CodexAppServerProbeLimitsV1, probe_codex_app_server_v1,
};
use tempfile::TempDir;
use zeroize::Zeroizing;

fn python_executable() -> Option<PathBuf> {
    ["/usr/bin/python3", "/usr/local/bin/python3"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

fn config(
    mode: &str,
    timeout: Duration,
    sentinel: Option<PathBuf>,
) -> Option<CodexAppServerProbeConfigV1> {
    let executable = python_executable()?;
    let mut environment = vec![CodexAppServerEnvironmentV1::new(
        "AWORKIT_CODEX_FIXTURE_MODE".into(),
        Zeroizing::new(mode.to_owned()),
    )];
    if let Some(sentinel) = sentinel {
        environment.push(CodexAppServerEnvironmentV1::new(
            "AWORKIT_CODEX_FIXTURE_SENTINEL".into(),
            Zeroizing::new(sentinel.display().to_string()),
        ));
    }
    Some(CodexAppServerProbeConfigV1 {
        executable,
        arguments: vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/codex_app_server_fixture.py")
                .display()
                .to_string(),
        ],
        working_directory: None,
        inherit_environment: true,
        environment,
        limits: CodexAppServerProbeLimitsV1 {
            timeout,
            maximum_message_bytes: 64 * 1024,
            maximum_messages: 32,
            maximum_models: 16,
        },
    })
}

#[test]
fn real_stdio_probe_initializes_reads_safe_account_and_models_then_kills_the_tree() {
    let directory = TempDir::new().expect("temporary directory");
    let sentinel = directory.path().join("descendant-leaked");
    let Some(config) = config("success", Duration::from_secs(5), Some(sentinel.clone())) else {
        return;
    };

    let result = probe_codex_app_server_v1(config).expect("successful handshake");

    assert_eq!(result.protocol, "codex-app-server-jsonrpc-stdio");
    assert_eq!(result.server_identity.as_deref(), Some("codex-fixture/1.0"));
    assert_eq!(result.platform_family.as_deref(), Some("fixture"));
    assert_eq!(result.account.account_type.as_deref(), Some("chatgpt"));
    assert!(result.account.requires_openai_auth);
    assert_eq!(result.model_ids, ["model.fixture.one", "model.fixture.two"]);
    assert!(result.capabilities.progress);
    assert!(result.capabilities.continuation);
    assert!(result.capabilities.cancellation);
    assert!(result.capabilities.approvals);
    assert!(!format!("{result:?}").contains("example.invalid"));

    thread::sleep(Duration::from_secs(2));
    assert!(
        !sentinel.exists(),
        "the transient probe must terminate descendants in its process group"
    );
}

#[test]
fn malformed_timeout_and_nonzero_peers_fail_with_sanitized_errors() {
    for (mode, expected) in [
        ("malformed", CodexAppServerProbeError::Protocol),
        ("timeout", CodexAppServerProbeError::TimedOut),
        ("nonzero", CodexAppServerProbeError::Exited),
    ] {
        let Some(config) = config(mode, Duration::from_millis(250), None) else {
            return;
        };
        let error = probe_codex_app_server_v1(config).expect_err("fixture must fail");
        assert_eq!(error, expected, "mode {mode}");
        let message = error.to_string();
        assert!(!message.contains(mode));
        assert!(!message.contains("fixture"));
    }
}

#[test]
fn stderr_larger_than_one_message_bound_is_drained_without_deadlocking() {
    let Some(config) = config("noisy", Duration::from_secs(5), None) else {
        return;
    };
    let result = probe_codex_app_server_v1(config).expect("noisy peer handshake");
    assert_eq!(result.model_ids.len(), 2);
}
