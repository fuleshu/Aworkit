//! Discover available MCP tools independently; a closed server degrades nothing.
//!
//! A server that is not running right now is never treated as an absent
//! capability, and assembling a pass never waits for one. Its frozen connection
//! is taken from saved Settings and every tool interface it offers is taken from
//! the catalog saved with that connection, so the tool stays in the Chat's
//! offered set without contacting anything. Development servers — an editor or a
//! bridge process that is stopped and started all day — therefore never vanish
//! from a Chat and never delay one. The session opens on the first call and is
//! retried on every later call; a call made while the server is down settles as a
//! bounded result that names the outage and tells the model to start it, and from
//! the first successful call onward the live interface replaces the saved one.
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
        // Every prepared connection is frozen, with no session opened here: the
        // Chat keeps exactly the authority it started with and reaches the server
        // on the first call instead of making every pass wait for it.
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
                // Only a session a call already opened can say anything about a
                // tool's live interface; a server nobody has called yet is
                // described from Settings below. A session that dropped the tool
                // is worth naming: the tool stays offered and the call reports it.
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
        // A tool whose server nobody has called yet is described by the catalog
        // saved with its Settings connection, which is what the user last
        // connected with; a session an earlier call opened wins above. Only
        // servers this pass froze a connection for qualify: a tool whose
        // connection could not be frozen at all (a missing credential or an
        // unverified plugin) is still dropped with its warning, because no call
        // could ever reach it.
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
