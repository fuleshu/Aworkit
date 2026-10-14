//! Discover available MCP tools independently; a closed server degrades nothing.
//!
//! A server that is not running right now is never treated as an absent
//! capability. Its frozen connection and every tool interface it offers are
//! taken from saved Settings, so the tool stays in the Chat's offered set and
//! the session is retried on each pass and on each call. Development servers —
//! an editor or a bridge process that is stopped and started all day — therefore
//! never vanish from a Chat. A call made while the server is down settles as a
//! bounded result that names the outage and tells the model to start it.
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
        let snapshots = if preparations.is_empty() {
            Vec::new()
        } else {
            match self
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
            }
        };
        // Every connection this pass prepared is frozen whether or not its
        // session opened: the Chat keeps exactly the authority it started with
        // and retries the transport, instead of reading a closed server as a
        // capability that no longer exists.
        for preparation in preparations {
            manifests.insert(
                preparation.manifest.server_id.to_string(),
                preparation.manifest,
            );
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
                // A server that never opened already reported its own warning.
                // A server that did open but no longer advertises the tool is
                // worth naming: the tool stays offered either way.
                if snapshots.iter().any(|s| s.server_id.as_str() == server_id) {
                    warn(
                        workflow,
                        format!(
                            "MCP server '{server_id}' no longer provides '{tool}'; the tool stays offered and a call reports the error"
                        ),
                    );
                }
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
        // A server that is unreachable right now still offers the interfaces
        // saved in Settings. Live discovery above wins wherever it produced a
        // descriptor, so an edited server reaches the model immediately while a
        // closed one only falls back to its last saved catalog. Only servers
        // this pass actually prepared a frozen connection for qualify: a tool
        // whose connection could not be frozen at all (a missing credential or
        // an unverified plugin) is still dropped with its warning, because no
        // call could ever reach it.
        for (id, definition) in preview_mcp_definitions(workflow, &settings).unwrap_or_default() {
            let reachable = split_mcp_capability(&id)
                .is_ok_and(|(server, _)| manifests.contains_key(server));
            if reachable {
                definitions.entry(id).or_insert(definition);
            }
        }
        (definitions, manifests)
    }
}
