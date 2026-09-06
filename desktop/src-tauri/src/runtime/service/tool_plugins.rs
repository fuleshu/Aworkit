//! Reconnect persisted MCP Chats using their own immutable connection snapshots.
use super::*;

impl DesktopRuntime {
    pub(super) fn restore_frozen_mcp(
        &mut self,
        context: &FrozenChatExecutionContextV1,
    ) -> Result<Vec<aworkit_capability_host::McpServerManifestV1>, String> {
        if context.mcp_configurations.is_empty() {
            return Ok(context.mcp_manifests.values().cloned().collect());
        }
        let mut preparations = Vec::new();
        for frozen in &context.mcp_configurations {
            let prepared = prepare_mcp_server(&frozen.server, &frozen.credentials)?;
            let expected = context
                .mcp_manifests
                .get(&frozen.server.id)
                .ok_or("Frozen MCP connection has no matching manifest")?;
            if prepared.manifest.binding_hash != expected.binding_hash {
                return Err(format!(
                    "Frozen MCP connection '{}' changed",
                    frozen.server.id
                ));
            }
            preparations.push(McpRunServerPreparationV1 {
                manifest: expected.clone(),
                endpoint: prepared.endpoint,
                materialization: materialize_bindings(
                    &mut self.credentials,
                    &prepared.secret_bindings,
                )?,
            });
        }
        let snapshots = self
            .pipeline
            .prepare_mcp_sessions(&context.identity.run_id, &mut preparations)?;
        for tool in context
            .tools
            .iter()
            .filter(|tool| tool.tool_id.starts_with(MCP_CAPABILITY_PREFIX))
        {
            let (server, name) = split_mcp_capability(&tool.tool_id)?;
            let definition = tool
                .definition
                .as_ref()
                .ok_or("Frozen MCP tool definition is missing")?;
            let current = snapshots
                .iter()
                .find(|snapshot| snapshot.server_id.as_str() == server)
                .and_then(|snapshot| snapshot.catalog.tools.iter().find(|tool| tool.name == name));
            if current.is_none_or(|tool| tool.input_schema != definition.input_schema) {
                return Err(format!(
                    "MCP schema changed for '{}'; start a New Chat to use the new definition",
                    tool.tool_id
                ));
            }
        }
        Ok(preparations
            .into_iter()
            .map(|prepared| prepared.manifest)
            .collect())
    }
}

pub(super) fn freeze_mcp_configurations(
    settings: &SettingsConfigurationV2,
    manifests: &BTreeMap<String, aworkit_capability_host::McpServerManifestV1>,
) -> Vec<super::super::history::FrozenMcpConfigurationV1> {
    settings
        .mcp_servers
        .iter()
        .filter(|server| manifests.contains_key(&server.id))
        .map(|server| {
            let bindings = match &server.transport {
                IntegrationTransportV2::Stdio { env, .. } => env,
                IntegrationTransportV2::Http { headers, .. } => headers,
            };
            let mut snapshot = server.clone();
            // Tool choices/definitions have their own frozen bindings; the transport
            // snapshot retains no unrelated catalog or credential metadata.
            snapshot.tools.clear();
            super::super::history::FrozenMcpConfigurationV1 {
                server: snapshot,
                credentials: settings
                    .credentials
                    .iter()
                    .filter(|credential| {
                        bindings
                            .iter()
                            .any(|binding| binding.credential_ref == credential.credential_ref)
                    })
                    .cloned()
                    .collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::super::settings_v2::McpServerConfigurationV2;
    use super::*;

    #[test]
    fn frozen_mcp_transport_keeps_only_referenced_metadata_and_passes_history_guard() {
        let mut settings = SettingsConfigurationV2::default();
        settings.credentials = ["credential.selected", "credential.unrelated"]
            .into_iter()
            .map(|reference| CredentialMetadataConfigurationV2 {
                credential_ref: reference.into(),
                label: "MCP access".into(),
                kind: "token".into(),
                field_names: vec!["token".into()],
                revision: 1,
                bound_provider_id: None,
                bound_endpoint: None,
            })
            .collect();
        let server = McpServerConfigurationV2 {
            id: "mcp.frozen".into(),
            name: "Frozen transport".into(),
            enabled: true,
            auto_connect: false,
            plugin: None,
            tools: vec![],
            transport: IntegrationTransportV2::Http {
                url: "https://example.com/mcp".into(),
                headers: vec![super::super::super::settings_v2::NamedCredentialBindingV2 {
                    name: "Authorization".into(),
                    credential_ref: "credential.selected".into(),
                    field: "token".into(),
                }],
            },
        };
        let prepared = prepare_mcp_server(&server, &settings.credentials).unwrap();
        settings.mcp_servers.push(server);
        let frozen = freeze_mcp_configurations(
            &settings,
            &BTreeMap::from([("mcp.frozen".into(), prepared.manifest)]),
        );
        assert_eq!(frozen.len(), 1);
        assert_eq!(frozen[0].credentials.len(), 1);
        assert_eq!(
            frozen[0].credentials[0].credential_ref,
            "credential.selected"
        );
        let value = serde_json::to_value(&frozen[0]).unwrap();
        assert!(value.get("opaqueBindings").is_some());
        assert!(value.get("credentials").is_none());
        let root = tempfile::tempdir().unwrap();
        let store =
            aworkit_local_store::LocalHistoryStore::open(root.path().join("history.sqlite3"))
                .unwrap();
        let batch = |head, payload| aworkit_local_store::CommitBatch {
            chat_id: "chat.mcp-frozen".into(),
            branch_id: "main".into(),
            expected_head: head,
            events: vec![aworkit_local_store::Event {
                event_id: format!("event.mcp.{head}"),
                kind: "chat.execution-context-frozen".into(),
                payload,
            }],
            attempt: None,
            checkpoint: None,
            deduplication: None,
            outbox: vec![],
        };
        store
            .commit(&batch(0, value.clone()))
            .expect("opaque MCP references must remain valid durable history");
        let mut bad = value;
        bad["credentials"] = json!({"token":"must-not-be-persisted"});
        assert!(store.commit(&batch(1, bad)).is_err());
    }
}
