use super::*;
use crate::runtime::{settings_v2::McpServerConfigurationV2, tool_registry::McpToolConfiguration};

fn fixture() -> (Value, SettingsConfigurationV2) {
    let workflow = json!({"nodes":[
        {"type":"agent","configuration":{"toolIds":["tool.todo","mcp:adashi","mcp://adashi/read"]}},
        {"type":"tool","configuration":{"toolId":"mcp://adashi/read"}}
    ]});
    let mut settings = SettingsConfigurationV2::default();
    settings.mcp_servers.push(McpServerConfigurationV2 {
        id: "adashi".into(),
        name: "Adashi".into(),
        enabled: true,
        auto_connect: false,
        plugin: None,
        transport: IntegrationTransportV2::Stdio {
            command: "unavailable-mcp".into(),
            args: vec![],
            cwd: None,
            env: vec![],
        },
        tools: ["read", "write", "disabled"]
            .into_iter()
            .map(|name| McpToolConfiguration {
                name: name.into(),
                description: name.into(),
                input_schema: json!({"type":"object"}),
                enabled: name != "disabled",
                options: Default::default(),
            })
            .collect(),
    });
    (workflow, settings)
}

#[test]
fn server_selection_resolves_exact_enabled_functions_and_preserves_source() {
    let (workflow, settings) = fixture();
    let resolved = expand_server_selections(&workflow, &settings).unwrap();
    assert_eq!(
        resolved["nodes"][0]["configuration"]["toolIds"],
        json!(["tool.todo", "mcp://adashi/read", "mcp://adashi/write"])
    );
    assert_eq!(resolved["nodes"][1], workflow["nodes"][1]);
    assert_eq!(
        workflow["nodes"][0]["configuration"]["toolIds"][1],
        "mcp:adashi"
    );
    // Preview consumes the same expansion without starting the nonexistent server.
    let definitions = preview_mcp_definitions(&resolved, &settings).unwrap();
    assert_eq!(definitions.len(), 2);
}

#[test]
fn later_catalog_changes_apply_only_to_new_resolutions() {
    let (workflow, mut settings) = fixture();
    let frozen = expand_server_selections(&workflow, &settings).unwrap();
    settings.mcp_servers[0].tools[2].enabled = true;
    let next = expand_server_selections(&workflow, &settings).unwrap();
    assert_eq!(
        frozen["nodes"][0]["configuration"]["toolIds"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        next["nodes"][0]["configuration"]["toolIds"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn missing_disabled_and_unconfigured_servers_report_actionable_errors() {
    let (workflow, mut settings) = fixture();
    settings.mcp_servers[0].enabled = false;
    assert!(
        expand_server_selections(&workflow, &settings)
            .unwrap_err()
            .contains("disabled")
    );
    settings.mcp_servers[0].enabled = true;
    for tool in &mut settings.mcp_servers[0].tools {
        tool.enabled = false;
    }
    assert!(
        expand_server_selections(&workflow, &settings)
            .unwrap_err()
            .contains("no enabled functions")
    );
    settings.mcp_servers[0].tools.clear();
    assert!(
        expand_server_selections(&workflow, &settings)
            .unwrap_err()
            .contains("Connect and enable")
    );
    settings.mcp_servers.clear();
    assert!(
        expand_server_selections(&workflow, &settings)
            .unwrap_err()
            .contains("missing")
    );
}

#[test]
fn individual_bindings_and_other_server_prefixes_remain_exact() {
    let (mut workflow, settings) = fixture();
    workflow["nodes"][0]["configuration"]["toolIds"] =
        json!(["mcp://adashi/read", "mcp://adashi.other/write"]);
    assert_eq!(
        expand_server_selections(&workflow, &settings).unwrap(),
        workflow
    );
}
