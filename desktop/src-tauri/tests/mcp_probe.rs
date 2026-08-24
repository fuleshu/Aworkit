use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use aworkit_desktop::runtime::{
    CredentialStoreInputV2, DesktopRuntime, IntegrationTransportV2, McpProbeRequestV2,
    McpServerConfigurationV2, NamedCredentialBindingV2,
};
use aworkit_trusted_core::MemoryCredentialStore;
use tempfile::TempDir;

const SECRET: &str = "desktop-mcp-probe-secret";

fn python_executable() -> PathBuf {
    ["/usr/bin/python3", "/usr/local/bin/python3"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .or_else(|| {
            // Windows commonly exposes Python through PATH rather than either
            // Unix location. Ask that launcher for its resolved executable so
            // the MCP configuration still exercises an absolute path.
            Command::new("python")
                .args(["-c", "import sys; print(sys.executable)"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|path| PathBuf::from(path.trim()))
                .filter(|path| path.is_file())
        })
        .expect("a system Python 3 interpreter for the hermetic MCP fixture")
}

fn fixture_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_probe_fixture.py")
}

#[test]
fn desktop_probe_redeems_named_field_and_discovers_real_stdio_catalog() {
    let root = TempDir::new().expect("runtime root");
    let store = Arc::new(MemoryCredentialStore::default());
    let mut runtime =
        DesktopRuntime::open_with_credential_store(root.path(), store).expect("desktop runtime");

    runtime
        .settings_v2_store_credential(CredentialStoreInputV2 {
            command_id: "settings.mcp-probe.credential".into(),
            expected_version: 1,
            replace_credential_ref: None,
            label: "MCP fixture token".into(),
            kind: "token".into(),
            bound_provider_id: None,
            bound_endpoint: None,
            fields: BTreeMap::from([("token".into(), SECRET.to_owned().into())]),
        })
        .expect("write-only credential save");
    let credential_ref = runtime.settings_v2_snapshot().settings.credentials[0]
        .credential_ref
        .clone();

    let result = runtime
        .settings_v2_probe_mcp(McpProbeRequestV2 {
            server: McpServerConfigurationV2 {
                id: "mcp.desktop-probe".into(),
                name: "Desktop probe fixture".into(),
                // Test/Discover remains useful before the draft is enabled or saved.
                enabled: false,
                auto_connect: false,
                transport: IntegrationTransportV2::Stdio {
                    // Windows agents normally use the standard bare `python`
                    // command, while Unix coverage retains quoted-path support.
                    command: if cfg!(windows) {
                        "python".into()
                    } else {
                        format!("\"{}\"", python_executable().display())
                    },
                    args: vec![fixture_script().display().to_string()],
                    cwd: None,
                    env: vec![NamedCredentialBindingV2 {
                        name: "AWORKIT_MCP_TEST_TOKEN".into(),
                        credential_ref,
                        field: "token".into(),
                    }],
                },
            },
            draft_fingerprint: "draft.mcp.real-stdio".into(),
        })
        .expect("real MCP STDIO probe");

    assert_eq!(result.server_id, "mcp.desktop-probe");
    assert_eq!(result.protocol_version, "2025-11-25");
    assert!(result.features.tools);
    assert!(result.features.resources);
    assert!(result.features.prompts);
    assert_eq!(result.tool_names, ["echo"]);
    assert_eq!(result.resource_names, ["fixture://desktop-resource"]);
    assert_eq!(result.prompt_names, ["summarize"]);
    assert!(result.binding_hash.starts_with("sha256:"));
    assert!(result.catalog_hash.starts_with("sha256:"));
    assert_eq!(result.draft_fingerprint, "draft.mcp.real-stdio");
    assert_eq!(runtime.settings_v2_snapshot().version, 2);

    let projection = serde_json::to_string(&result).expect("secret-free projection");
    assert!(!projection.contains(SECRET));
}
