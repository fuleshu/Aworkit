//! Discover available MCP tools independently; failures degrade one capability.
use super::*;
use crate::runtime::workflow_capabilities::warn;

impl DesktopRuntime {
    pub(super) fn discover_workflow_mcp(
        &mut self,
        workflow: &mut Value,
        run_id: &StableId,
    ) -> (
        BTreeMap<String, DiscoveredMcpDefinition>,
        BTreeMap<String, aworkit_capability_host::McpServerManifestV1>,
    ) {
        let mut definitions = BTreeMap::new();
        let mut manifests = BTreeMap::new();
        let settings = self.documents.settings().clone();
        let ids = graph_mcp_tool_ids(workflow);
        let mut preparations = Vec::new();
        let mut seen = BTreeSet::new();
        for id in &ids {
            let result = (|| {
                let (server_id, _) = split_mcp_capability(id)?;
                if !seen.insert(server_id.to_owned()) {
                    return Ok(None);
                }
                let server = settings
                    .mcp_servers
                    .iter()
                    .find(|s| s.id == server_id && s.enabled)
                    .ok_or_else(|| format!("MCP server '{server_id}' is missing or disabled"))?;
                let prepared = prepare_mcp_server(server, &settings.credentials)?;
                let materialization =
                    materialize_bindings(&mut self.credentials, &prepared.secret_bindings)?;
                Ok::<_, String>(Some(McpRunServerPreparationV1 {
                    manifest: prepared.manifest,
                    endpoint: prepared.endpoint,
                    materialization,
                }))
            })();
            match result {
                Ok(Some(preparation)) => preparations.push(preparation),
                Ok(None) => {}
                Err(error) => warn(workflow, error),
            }
        }
        if preparations.is_empty() {
            return (definitions, manifests);
        }
        let snapshots = match self
            .pipeline
            .prepare_mcp_sessions(run_id, &mut preparations)
        {
            Ok((snapshots, warnings)) => {
                for warning in warnings {
                    warn(workflow, warning);
                }
                snapshots
            }
            Err(error) => {
                warn(workflow, format!("MCP connections unavailable: {error}"));
                Vec::new()
            }
        };
        for preparation in preparations {
            if snapshots
                .iter()
                .any(|s| s.server_id == preparation.manifest.server_id)
            {
                manifests.insert(
                    preparation.manifest.server_id.to_string(),
                    preparation.manifest,
                );
            }
        }
        for id in ids {
            let Ok((server_id, tool)) = split_mcp_capability(&id) else {
                continue;
            };
            let descriptor = snapshots
                .iter()
                .find(|s| s.server_id.as_str() == server_id)
                .and_then(|s| s.catalog.tools.iter().find(|t| t.name == tool));
            let Some(descriptor) = descriptor else {
                warn(
                    workflow,
                    format!("MCP tool '{id}' was not discovered and is unavailable"),
                );
                continue;
            };
            let label = settings
                .mcp_servers
                .iter()
                .find(|s| s.id == server_id)
                .map(|s| s.name.as_str())
                .unwrap_or(server_id);
            definitions.insert(
                id.clone(),
                DiscoveredMcpDefinition::from_descriptor(&id, server_id, label, descriptor),
            );
        }
        (definitions, manifests)
    }
}
