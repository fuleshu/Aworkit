import { useRef, useState } from "react";
import type {
  CredentialMetadataConfiguration,
  ExternalAgentConfiguration,
  ExtensionConfiguration,
  McpServerConfiguration,
} from "../configuration";
import {
  ConnectionEditor,
  CredentialBindingsEditor,
} from "./SettingsFields";
import {
  mcpDraftFingerprint,
  settingsRecordFingerprint,
} from "./settingsDraft";

export interface IntegrationProbeResult {
  readonly ok: boolean;
  readonly message: string;
  readonly draftFingerprint: string;
  readonly details?: readonly string[];
  readonly capabilities?: ExternalAgentConfiguration["capabilities"];
}

export function McpServersSection({
  servers,
  credentials,
  onPickCommand,
  onChange,
  onProbe,
}: {
  readonly servers: readonly McpServerConfiguration[];
  readonly credentials: readonly CredentialMetadataConfiguration[];
  readonly onPickCommand: () => Promise<string | null>;
  readonly onChange: (servers: readonly McpServerConfiguration[]) => void;
  readonly onProbe: (server: McpServerConfiguration) => Promise<IntegrationProbeResult>;
}): React.JSX.Element {
  const integrationCredentials = credentials.filter(isIntegrationCredential);
  const [probes, setProbes] = useState<Readonly<Record<string, IntegrationProbeResult>>>({});
  const [probing, setProbing] = useState<string | null>(null);
  const addServer = () =>
    onChange([
      ...servers,
      {
        id: localId("mcp"),
        name: "MCP server",
        enabled: false,
        autoConnect: false,
        transport: {
          transport: "stdio",
          command: "",
          args: [],
          cwd: null,
          env: [],
        },
      },
    ]);
  return (
    <div className="settings-section-stack">
      <div className="section-heading-row">
        <p className="section-intro">
          Configure MCP tool, resource, and prompt servers. Discover and test
          validates the exact current server configuration.
        </p>
        <button
          title="Add a secret-free MCP transport configuration"
          type="button"
          onClick={addServer}
        >
          Add server
        </button>
      </div>
      {servers.length === 0 ? (
        <p className="settings-empty">No MCP servers configured.</p>
      ) : (
        <div className="settings-record-list">
          {servers.map((server, index) => {
            const draftFingerprint = mcpDraftFingerprint(server);
            const probe = probes[server.id];
            const currentProbe =
              probe?.draftFingerprint === draftFingerprint ? probe : undefined;
            const updateServer = (next: McpServerConfiguration) => {
              setProbes((current) => withoutRecord(current, server.id));
              onChange(replaceAt(servers, index, next));
            };
            return (
            <section className="settings-record" key={server.id}>
              <RecordHeading
                id={server.id}
                name={server.name}
                onRemove={() => {
                  setProbes((current) => withoutRecord(current, server.id));
                  onChange(removeAt(servers, index));
                }}
              />
              <div className="settings-grid two-columns">
                <TextField
                  id={`${server.id}-name`}
                  label="Server name"
                  title="Name shown for this MCP transport record; enable the server to make its tools bindable in workflows"
                  value={server.name}
                  onChange={(name) =>
                    updateServer({ ...server, name })
                  }
                />
              </div>
              <ConnectionEditor
                id={server.id}
                value={server.transport}
                credentials={integrationCredentials}
                showWorkingDirectory={false}
                onPickCommand={onPickCommand}
                onChange={(transport) =>
                  updateServer({
                    ...server,
                    transport:
                      transport.transport === "stdio"
                        ? { ...transport, cwd: null }
                        : transport,
                  })
                }
              />
              <ProbeActions
                id={server.id}
                label="Discover and test"
                probing={probing === server.id}
                result={currentProbe}
                onProbe={() => {
                  const requestedFingerprint = mcpDraftFingerprint(server);
                  setProbing(server.id);
                  void onProbe(server)
                    .then((result) =>
                      setProbes((current) => ({ ...current, [server.id]: result })),
                    )
                    .catch((failure: unknown) =>
                      setProbes((current) => ({
                        ...current,
                        [server.id]: {
                          ok: false,
                          message:
                            failure instanceof Error
                              ? failure.message
                              : String(failure),
                          draftFingerprint: requestedFingerprint,
                        },
                      })),
                    )
                    .finally(() => setProbing(null));
                }}
              />
            </section>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function ExternalAgentsSection({
  agents,
  mcpServers,
  credentials,
  onChange,
  onProbe,
}: {
  readonly agents: readonly ExternalAgentConfiguration[];
  readonly mcpServers: readonly McpServerConfiguration[];
  readonly credentials: readonly CredentialMetadataConfiguration[];
  readonly onChange: (agents: readonly ExternalAgentConfiguration[]) => void;
  readonly onProbe: (agent: ExternalAgentConfiguration) => Promise<IntegrationProbeResult>;
}): React.JSX.Element {
  const integrationCredentials = credentials.filter(isIntegrationCredential);
  const [probes, setProbes] = useState<Readonly<Record<string, IntegrationProbeResult>>>({});
  const [probing, setProbing] = useState<string | null>(null);
  const addAgent = () =>
    onChange([
      ...agents,
      {
        id: localId("agent"),
        name: "External agent",
        adapter: "codex_app_server",
        enabled: false,
        connection: {
          transport: "stdio",
          command: "codex",
          args: ["app-server"],
          cwd: null,
          env: [],
        },
        credentialBindings: [],
        mcpServerIds: [],
        capabilities: {
          progress: false,
          continuation: false,
          cancellation: false,
          approvals: false,
        },
        configuration: {},
      },
    ]);
  return (
    <div className="settings-section-stack">
      <div className="section-heading-row">
        <p className="section-intro">
          Handshake reports capabilities for the exact current transport draft.
          The result is ephemeral diagnostic evidence, not saved configuration.
          This build cannot start an external-agent workflow node or continue
          its session lifecycle.
        </p>
        <button
          title="Add an external-agent transport draft for diagnostic handshake only"
          type="button"
          onClick={addAgent}
        >
          Add agent
        </button>
      </div>
      {agents.length === 0 ? (
        <p className="settings-empty">No external agents configured.</p>
      ) : (
        <div className="settings-record-list">
          {agents.map((agent, index) => {
            const draftFingerprint = settingsRecordFingerprint(agent);
            const probe = probes[agent.id];
            const currentProbe =
              probe?.draftFingerprint === draftFingerprint ? probe : undefined;
            const updateAgent = (next: ExternalAgentConfiguration) => {
              setProbes((current) => withoutRecord(current, agent.id));
              onChange(replaceAt(agents, index, next));
            };
            const hasLegacyCapabilityMetadata = Object.values(
              agent.capabilities,
            ).some(Boolean);
            return (
            <section className="settings-record" key={agent.id}>
              <RecordHeading
                id={agent.id}
                name={agent.name}
                onRemove={() => {
                  setProbes((current) => withoutRecord(current, agent.id));
                  onChange(removeAt(agents, index));
                }}
              />
              <div className="settings-grid two-columns">
                <TextField
                  id={`${agent.id}-name`}
                  label="Agent name"
                  title="Name shown for this diagnostic external-agent record; this build cannot select it from a workflow node"
                  value={agent.name}
                  onChange={(name) =>
                    updateAgent({ ...agent, name })
                  }
                />
                <label className="settings-field" htmlFor={`${agent.id}-adapter`}>
                  Adapter
                  <select
                    id={`${agent.id}-adapter`}
                    title="Lifecycle protocol adapter; Codex App Server is the first rich target and ACP is the generic local path"
                    value={agent.adapter}
                    onChange={(event) =>
                      updateAgent({ ...agent, adapter: event.target.value })
                    }
                  >
                    <option value="codex_app_server">Codex App Server</option>
                    {agent.adapter !== "codex_app_server" && (
                      <option value={agent.adapter}>
                        {agent.adapter} (adapter not installed)
                      </option>
                    )}
                  </select>
                </label>
              </div>
              <Switch
                id={`${agent.id}-enabled`}
                label={unavailableExecutionLabel(agent.enabled)}
                title={unavailableExecutionTitle("External-agent", agent.enabled)}
                checked={agent.enabled}
                disabled={!agent.enabled}
                onChange={(enabled) =>
                  updateAgent({ ...agent, enabled })
                }
              />
              <ConnectionEditor
                id={`${agent.id}-connection`}
                value={agent.connection}
                credentials={integrationCredentials}
                allowedTransports={["stdio"]}
                onChange={(connection) =>
                  updateAgent({ ...agent, connection })
                }
              />
              <CredentialBindingsEditor
                id={`${agent.id}-credentials`}
                label="Adapter credential bindings"
                bindings={agent.credentialBindings}
                credentials={integrationCredentials}
                onChange={(credentialBindings) =>
                  updateAgent(
                    {
                      ...agent,
                      credentialBindings: [...credentialBindings],
                    },
                  )
                }
              />
              <fieldset className="settings-bindings">
                <legend>MCP forwarding metadata (not consumed)</legend>
                {mcpServers.map((server) => (
                  <Switch
                    key={server.id}
                    id={`${agent.id}-${server.id}`}
                    label={server.name}
                    title="Preserved metadata only: the installed handshake and runtime do not forward MCP servers"
                    checked={agent.mcpServerIds.includes(server.id)}
                    disabled
                    onChange={() => undefined}
                  />
                ))}
                {mcpServers.length === 0 && (
                  <p className="settings-empty">No MCP servers configured.</p>
                )}
                {agent.mcpServerIds.length > 0 && (
                  <button
                    title="Remove MCP forwarding metadata that the installed adapter cannot consume"
                    type="button"
                    onClick={() =>
                      updateAgent({ ...agent, mcpServerIds: [] })
                    }
                  >
                    Remove unsupported forwarding metadata
                  </button>
                )}
              </fieldset>
              {currentProbe?.ok && currentProbe.capabilities !== undefined ? (
                <div
                  className="capability-chips"
                  aria-label="Capabilities from current handshake"
                >
                  {Object.entries(currentProbe.capabilities).map(
                    ([name, supported]) => (
                      <span
                        className={`status ${supported ? "configured" : "disabled"}`}
                        key={name}
                      >
                        {name}: {supported ? "yes" : "no"}
                      </span>
                    ),
                  )}
                </div>
              ) : (
                <p className="field-warning">
                  No capabilities have been reported for this exact draft. Run
                  Start handshake to obtain ephemeral evidence.
                </p>
              )}
              <p className="field-warning">
                Saved capability booleans are legacy compatibility metadata and
                are never treated as negotiated evidence.
              </p>
              {hasLegacyCapabilityMetadata && (
                <div className="field-warning" role="alert">
                  <div
                    className="capability-chips"
                    aria-label="Unsupported saved capability metadata"
                  >
                    {Object.entries(agent.capabilities).map(
                      ([name, supported]) => (
                        <span className="status disabled" key={name}>
                          {name}: {supported ? "saved true (ignored)" : "saved false"}
                        </span>
                      ),
                    )}
                  </div>
                  <button
                    id={`${agent.id}-clear-capabilities`}
                    title="Clear saved capability booleans that are not valid handshake evidence"
                    type="button"
                    onClick={() =>
                      updateAgent({
                        ...agent,
                        capabilities: emptyExternalAgentCapabilities(),
                      })
                    }
                  >
                    Clear unsupported capability metadata
                  </button>
                </div>
              )}
              <ReadOnlyConfiguration
                label="Adapter configuration (preserved, not consumed)"
                value={agent.configuration}
              />
              {Object.keys(agent.configuration).length > 0 && (
                <button
                  title="Clear adapter configuration that the installed Codex handshake does not consume"
                  type="button"
                  onClick={() =>
                    updateAgent({ ...agent, configuration: {} })
                  }
                >
                  Clear unsupported adapter configuration
                </button>
              )}
              <ProbeActions
                id={agent.id}
                label="Start handshake"
                probing={probing === agent.id}
                result={currentProbe}
                onProbe={() => {
                  const requestedFingerprint = settingsRecordFingerprint(agent);
                  setProbing(agent.id);
                  void onProbe(agent)
                    .then((result) =>
                      setProbes((current) => ({
                        ...current,
                        [agent.id]: result,
                      })),
                    )
                    .catch((failure: unknown) =>
                      setProbes((current) => ({
                        ...current,
                        [agent.id]: {
                          ok: false,
                          message:
                            failure instanceof Error
                              ? failure.message
                              : String(failure),
                          draftFingerprint: requestedFingerprint,
                        },
                      })),
                    )
                    .finally(() => setProbing(null));
                }}
              />
            </section>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function ExtensionsSection({
  extensions,
  onChange,
  onDiscover,
  onRegister,
  registrationBlockedReason,
}: {
  readonly extensions: readonly ExtensionConfiguration[];
  readonly onChange: (
    update: (
      current: readonly ExtensionConfiguration[],
    ) => readonly ExtensionConfiguration[],
  ) => void;
  readonly onDiscover: () => Promise<ExtensionConfiguration | null>;
  readonly onRegister: (extension: ExtensionConfiguration) => Promise<void>;
  readonly registrationBlockedReason?: string;
}): React.JSX.Element {
  const [error, setError] = useState<string | null>(null);
  const [registering, setRegistering] = useState<string | null>(null);
  const discoverySequenceRef = useRef(0);
  const latestDiscoverySequenceByIdRef = useRef(new Map<string, number>());
  const discover = () => {
    const discoverySequence = ++discoverySequenceRef.current;
    const requestFingerprintsById = new Map(
      extensions.map((extension) => [
        extension.id,
        settingsRecordFingerprint(extension),
      ]),
    );
    setError(null);
    void onDiscover()
      .then((extension) => {
        if (extension === null) return;
        const latestSequence =
          latestDiscoverySequenceByIdRef.current.get(extension.id);
        if (
          latestSequence !== undefined &&
          latestSequence > discoverySequence
        )
          return;
        latestDiscoverySequenceByIdRef.current.set(
          extension.id,
          discoverySequence,
        );
        onChange((current) => {
          const requestFingerprint = requestFingerprintsById.get(
            extension.id,
          );
          if (requestFingerprint !== undefined) {
            const currentExtension = current.find(
              ({ id }) => id === extension.id,
            );
            if (
              currentExtension === undefined ||
              settingsRecordFingerprint(currentExtension) !==
                requestFingerprint
            )
              return current;
          }
          return reconcileDiscoveredExtension(current, extension);
        });
      })
      .catch((failure: unknown) =>
        setError(failure instanceof Error ? failure.message : String(failure)),
      );
  };
  return (
    <div className="settings-section-stack">
      <div className="section-heading-row">
        <p className="section-intro">
          Discovery reads and validates a manifest without executing it. Save the
          discovery before registering the local package. Registration verifies
          identity and integrity only; this build does not load, enable, or
          execute extension workflow code.
        </p>
        <button
          title="Choose and inspect an Aworkit extension manifest without executing its code"
          type="button"
          onClick={discover}
        >
          Discover manifest…
        </button>
      </div>
      {error !== null && <p className="field-error" role="alert">{error}</p>}
      {extensions.length === 0 ? (
        <p className="settings-empty">No extensions discovered.</p>
      ) : (
        <div className="settings-record-list">
          {extensions.map((extension) => (
            <section className="settings-record" key={extension.id}>
              <RecordHeading
                id={extension.id}
                name={extension.name}
                onRemove={
                  extension.status === "installed"
                    ? undefined
                    : () =>
                        onChange((current) =>
                          current.filter(({ id }) => id !== extension.id),
                        )
                }
              />
              <p>
                Version {extension.version} · <span className={`status ${extension.status}`}>{extension.status}</span>
              </p>
              <p className="settings-path">{extension.manifestPath}</p>
              {extension.entryPoint && (
                <p className="settings-path">Entry point: {extension.entryPoint}</p>
              )}
              {extension.contentHash && (
                <p className="settings-path">
                  {extension.status === "installed"
                    ? "Verified entry-point digest"
                    : "Declared content digest"}
                  : {extension.contentHash}
                </p>
              )}
              {extension.provenance && <p>{extension.provenance}</p>}
              {extension.compatibility && <p>Compatibility: {extension.compatibility}</p>}
              {extension.status === "discovered" && (
                <div className="provider-actions">
                  <button
                    disabled={
                      registering !== null || registrationBlockedReason !== undefined
                    }
                    title={
                      registrationBlockedReason ??
                      "Re-inspect this saved manifest, verify compatibility and the exact entry-point file digest, then register it while leaving it disabled and untrusted"
                    }
                    type="button"
                    onClick={() => {
                      setError(null);
                      setRegistering(extension.id);
                      void onRegister(extension)
                        .catch((failure: unknown) =>
                          setError(
                            failure instanceof Error
                              ? failure.message
                              : String(failure),
                          ),
                        )
                        .finally(() => setRegistering(null));
                    }}
                  >
                    {registering === extension.id
                      ? "Verifying…"
                      : "Register installed package"}
                  </button>
                </div>
              )}
              {extension.status === "installed" && (
                <p className="security-note">
                  Registration verified this package without executing it.
                  Extension enablement and execution are not available in this
                  build.
                </p>
              )}
              <div className="settings-inline-switches">
                <Switch
                  id={`${extension.id}-trust`}
                  label="I trust this code"
                  title="Record trust in the verified package metadata; this build still cannot load, enable, or execute its code"
                  checked={extension.trustAccepted}
                  disabled={extension.status !== "installed"}
                  onChange={(trustAccepted) =>
                    onChange((current) =>
                      current.map((item) =>
                        item.id === extension.id
                          ? {
                              ...item,
                              trustAccepted,
                              enabled: trustAccepted ? item.enabled : false,
                            }
                          : item,
                      ),
                    )
                  }
                />
                <Switch
                  id={`${extension.id}-enabled`}
                  label={unavailableExecutionLabel(extension.enabled)}
                  title={unavailableExecutionTitle("Extension", extension.enabled)}
                  checked={extension.enabled}
                  disabled={!extension.enabled}
                  onChange={(enabled) =>
                    onChange((current) =>
                      current.map((item) =>
                        item.id === extension.id
                          ? { ...item, enabled }
                          : item,
                      ),
                    )
                  }
                />
              </div>
              <ReadOnlyConfiguration
                label="Verified extension metadata (read-only)"
                value={extension.configuration}
              />
            </section>
          ))}
        </div>
      )}
    </div>
  );
}

function ProbeActions({
  id,
  label,
  probing,
  result,
  onProbe,
}: {
  readonly id: string;
  readonly label: string;
  readonly probing: boolean;
  readonly result: IntegrationProbeResult | undefined;
  readonly onProbe: () => void;
}): React.JSX.Element {
  return (
    <div className="provider-actions">
      <button
        disabled={probing}
        title={`${label} using the current unsaved transport draft`}
        type="button"
        onClick={onProbe}
      >
        {probing ? "Working…" : label}
      </button>
      {result !== undefined && (
        <div className={`provider-detail ${result.ok ? "diagnostic" : "error"}`} role="status">
          <p>{result.message}</p>
          {result.details !== undefined && (
            <ul>{result.details.map((detail) => <li key={detail}>{detail}</li>)}</ul>
          )}
        </div>
      )}
      <span hidden>{id}</span>
    </div>
  );
}

function RecordHeading({
  id,
  name,
  onRemove,
}: {
  readonly id: string;
  readonly name: string;
  readonly onRemove?: () => void;
}): React.JSX.Element {
  return (
    <div className="settings-record-heading">
      <div>
        <h3>{name}</h3>
        <code>{id}</code>
      </div>
      {onRemove !== undefined && (
        <button
          aria-label={`Remove ${name}`}
          className="danger-action"
          title={`Remove ${name} from the settings draft`}
          type="button"
          onClick={onRemove}
        >
          Remove
        </button>
      )}
    </div>
  );
}

function TextField({
  id,
  label,
  title,
  value,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly value: string;
  readonly onChange: (value: string) => void;
}): React.JSX.Element {
  return (
    <label className="settings-field" htmlFor={id}>
      {label}
      <input
        id={id}
        title={title}
        type="text"
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}

function Switch({
  id,
  label,
  title,
  checked,
  disabled = false,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly checked: boolean;
  readonly disabled?: boolean;
  readonly onChange: (checked: boolean) => void;
}): React.JSX.Element {
  return (
    <label className="switch-label" htmlFor={id}>
      <input
        checked={checked}
        disabled={disabled}
        id={id}
        title={title}
        type="checkbox"
        onChange={(event) => onChange(event.target.checked)}
      />
      {label}
    </label>
  );
}

function replaceAt<T>(values: readonly T[], index: number, value: T): T[] {
  return values.map((item, itemIndex) => (itemIndex === index ? value : item));
}

function removeAt<T>(values: readonly T[], index: number): T[] {
  return values.filter((_, itemIndex) => itemIndex !== index);
}

function reconcileDiscoveredExtension(
  current: readonly ExtensionConfiguration[],
  discovered: ExtensionConfiguration,
): readonly ExtensionConfiguration[] {
  const existingIndex = current.findIndex(({ id }) => id === discovered.id);
  if (existingIndex < 0) return [...current, discovered];
  if (current[existingIndex]?.status === "installed") return current;
  return current.flatMap((extension, index) => {
    if (extension.id !== discovered.id) return [extension];
    return index === existingIndex ? [discovered] : [];
  });
}

function withoutRecord<T>(
  values: Readonly<Record<string, T>>,
  id: string,
): Readonly<Record<string, T>> {
  if (!(id in values)) return values;
  const next = { ...values };
  delete next[id];
  return next;
}

function emptyExternalAgentCapabilities(): ExternalAgentConfiguration["capabilities"] {
  return {
    progress: false,
    continuation: false,
    cancellation: false,
    approvals: false,
  };
}

function isIntegrationCredential(
  credential: CredentialMetadataConfiguration,
): boolean {
  return (
    credential.boundProviderId == null && credential.boundEndpoint == null
  );
}

function ReadOnlyConfiguration({
  label,
  value,
}: {
  readonly label: string;
  readonly value: Readonly<Record<string, unknown>>;
}): React.JSX.Element {
  return (
    <details className="settings-record-details">
      <summary>{label}</summary>
      <p className="field-warning">
        This build preserves these non-secret values losslessly but does not
        send them to an extension or external-agent executor.
      </p>
      <pre>{JSON.stringify(value, null, 2)}</pre>
    </details>
  );
}

function localId(scope: string): string {
  const random = globalThis.crypto?.randomUUID?.().replaceAll("-", "");
  return `${scope}.${random ?? `${Date.now()}${Math.random().toString(16).slice(2)}`}`;
}

function unavailableExecutionLabel(enabled: boolean): string {
  return enabled
    ? "Disable non-executable legacy flag"
    : "Workflow execution not available";
}

function unavailableExecutionTitle(kind: string, enabled: boolean): string {
  return enabled
    ? `Turn off this legacy enabled flag; ${kind} workflow execution is not installed`
    : `Unavailable: this build has no ${kind.toLowerCase()} workflow executor`;
}
