use std::{path::PathBuf, sync::Arc};

use aworkit_desktop::runtime::{
    DesktopRuntime, ExternalAgentCapabilitiesV2, ExternalAgentConfigurationV2,
    ExternalAgentProbeRequestV2, IntegrationTransportV2,
};
use aworkit_trusted_core::MemoryCredentialStore;
use tempfile::TempDir;

fn python_executable() -> PathBuf {
    ["/usr/bin/python3", "/usr/local/bin/python3"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .expect("a system Python 3 interpreter for the hermetic Codex fixture")
}

#[test]
fn desktop_probe_runs_documented_codex_app_server_handshake_without_starting_a_turn() {
    let root = TempDir::new().expect("runtime root");
    let mut runtime = DesktopRuntime::open_with_credential_store(
        root.path(),
        Arc::new(MemoryCredentialStore::default()),
    )
    .expect("desktop runtime");
    let fixture_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    let result = runtime
        .settings_v2_probe_external_agent(ExternalAgentProbeRequestV2 {
            agent: ExternalAgentConfigurationV2 {
                id: "agent.codex-fixture".into(),
                name: "Codex fixture".into(),
                adapter: "codex_app_server".into(),
                enabled: false,
                connection: IntegrationTransportV2::Stdio {
                    command: python_executable().display().to_string(),
                    args: vec!["app-server".into()],
                    cwd: Some(fixture_directory.display().to_string()),
                    env: Vec::new(),
                },
                credential_bindings: Vec::new(),
                mcp_server_ids: Vec::new(),
                capabilities: ExternalAgentCapabilitiesV2::default(),
                configuration: Default::default(),
            },
            draft_fingerprint: "draft.external-agent.codex-fixture".into(),
        })
        .expect("real Codex App Server protocol probe");

    assert_eq!(result.protocol, "codex-app-server-jsonrpc-stdio");
    assert_eq!(
        result.server_identity.as_deref(),
        Some("codex-desktop-fixture/1.0")
    );
    assert_eq!(result.account_type.as_deref(), Some("apiKey"));
    assert!(!result.requires_openai_auth);
    assert_eq!(result.model_ids, ["model.desktop.fixture"]);
    assert!(result.capabilities.progress);
    assert!(result.capabilities.continuation);
    assert!(result.capabilities.cancellation);
    assert!(result.capabilities.approvals);
    assert_eq!(
        result.draft_fingerprint,
        "draft.external-agent.codex-fixture"
    );
    assert_eq!(runtime.settings_v2_snapshot().version, 1);
    assert!(
        !serde_json::to_string(&result)
            .expect("secret-free result")
            .contains("must-not-cross")
    );
}
