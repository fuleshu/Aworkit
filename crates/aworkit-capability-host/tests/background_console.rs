//! Actual console handles for Windows launch routes, including batch wrappers.
#![cfg(windows)]

use aworkit_capability_host::*;
use aworkit_protocol::{ProcessGeneration, StableId};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
};

const HASH: &str = "sha256:5555555555555555555555555555555555555555555555555555555555555555";
fn id(s: &str) -> StableId {
    StableId::parse(s).unwrap()
}
fn python() -> PathBuf {
    let mut command = Command::new("python");
    command.args(["-c", "import sys; print(sys.executable)"]);
    aworkit_process::command::configure_background_command(&mut command);
    let output = command.output().unwrap();
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_fixture.py")
}
fn assert_no_console(bytes: &[u8]) {
    let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(value["rootConsole"], 0, "{value}");
    assert_eq!(value["childConsole"], 0, "{value}");
}

#[test]
fn direct_and_group_background_commands_keep_pipes_without_consoles() {
    for grouped in [false, true] {
        let mut command = Command::new(python());
        command
            .arg(fixture())
            .arg("--console-probe")
            .stdout(Stdio::piped());
        let output = if grouped {
            aworkit_process::command::spawn_background_group(&mut command)
                .unwrap()
                .wait_with_output()
                .unwrap()
        } else {
            aworkit_process::command::configure_background_command(&mut command);
            command.output().unwrap()
        };
        assert!(output.status.success());
        assert_no_console(&output.stdout);
    }
}

#[test]
fn mcp_executable_and_batch_discovery_calls_and_reconnect_have_no_console() {
    let directory = tempfile::Builder::new()
        .prefix("aworkit console test ")
        .tempdir()
        .unwrap();
    let launcher = directory.path().join("MCP launcher.cmd");
    let audit = directory.path().join("startup.jsonl");
    let python = python();
    std::fs::write(
        &launcher,
        format!(
            "@echo off\r\n\"{}\" \"{}\"\r\n",
            python.display(),
            fixture().display()
        ),
    )
    .unwrap();
    for batch in [false, true] {
        let config = McpPeerTransportConfigV1 {
            server_id: id("mcp.console"),
            binding_hash: HASH.into(),
            endpoint: McpTransportEndpointV1::Stdio(McpStdioTransportConfigV1 {
                executable: if batch {
                    launcher.clone()
                } else {
                    python.clone()
                },
                arguments: if batch {
                    vec![]
                } else {
                    vec![fixture().display().to_string()]
                },
                working_directory: Some(directory.path().to_path_buf()),
                public_environment: BTreeMap::from([(
                    "AWORKIT_CONSOLE_AUDIT".into(),
                    audit.display().to_string(),
                )]),
            }),
        };
        let manager = McpSessionManager::new(
            ProcessGeneration(17),
            Arc::new(ProductionMcpPeer::new(vec![config]).unwrap()),
        );
        for attempt in 0..2 {
            let snapshot = if attempt == 0 {
                manager
                    .open(McpServerManifestV1 {
                        server_id: id("mcp.console"),
                        adapter_version: "rmcp-3.1.4".into(),
                        binding_hash: HASH.into(),
                        host_generation: ProcessGeneration(17),
                        configured: true,
                        enabled: true,
                        core_attested: true,
                        transport: McpTransportKindV1::Stdio,
                        minimum_protocol_version: MCP_PROTOCOL_2025_11_25,
                        maximum_protocol_version: MCP_PROTOCOL_2026_07_28,
                        maximum_in_flight: 2,
                        maximum_progress_events: 4,
                        secret_slots: vec![],
                        workspace_roots: vec![],
                    })
                    .unwrap()
            } else {
                manager.reconnect(&id("mcp.console")).unwrap()
            };
            let outcome = manager
                .invoke(
                    &id("mcp.console"),
                    &McpCallV1 {
                        invocation_id: id(&format!("invocation.console.{attempt}")),
                        kind: McpCallKindV1::Tool,
                        name: "echo".into(),
                        expected_schema_hash: Some(
                            snapshot.catalog.tools[0].input_schema_hash.clone(),
                        ),
                        arguments: serde_json::json!({"message":"__console_probe__"}),
                    },
                )
                .unwrap();
            let result = outcome.result.unwrap();
            assert_no_console(
                result["structuredContent"]["echo"]
                    .as_str()
                    .unwrap()
                    .as_bytes(),
            );
        }
        manager.close(&id("mcp.console")).unwrap();
    }
    let starts = std::fs::read_to_string(audit).unwrap();
    assert!(starts.lines().count() >= 4);
    for line in starts.lines() {
        assert_no_console(line.as_bytes());
    }
}
