import { useEffect, useState } from "react";
import type {
  BuiltInToolConfiguration,
  CredentialMetadataConfiguration,
  ProjectConfiguration,
  ProviderConfiguration,
} from "../configuration";
import type { ToolProbeResult } from "../settingsV2Port";
import { settingsRecordFingerprint } from "./settingsDraft";
import { ToolConfigurationEditor } from "./ToolConfigurationEditor";
import { findNativeTool } from "../toolRegistry";
import { WebSearchSettingsEditor } from "./WebSearchSettingsEditor";
import { WebExtractionSettingsEditor } from "./WebExtractionSettingsEditor";

export interface CredentialWriteDraft {
  readonly replaceCredentialRef: string | null;
  readonly label: string;
  readonly kind: string;
  readonly boundProviderId: string | null;
  readonly boundEndpoint: string | null;
  readonly fields: Readonly<Record<string, string>>;
}

type SecretFieldDraft = { readonly name: string; readonly value: string };

export function CredentialsSection({
  credentials,
  providers,
  onStore,
  onDelete,
  confirm,
  onDirtyChange,
}: {
  readonly credentials: readonly CredentialMetadataConfiguration[];
  readonly providers: readonly ProviderConfiguration[];
  readonly onStore: (draft: CredentialWriteDraft) => Promise<void>;
  readonly onDelete: (credentialRef: string) => Promise<void>;
  readonly confirm: (title: string, body: string) => Promise<boolean>;
  readonly onDirtyChange?: (dirty: boolean) => void;
}): React.JSX.Element {
  const [editing, setEditing] = useState<CredentialMetadataConfiguration | "new" | null>(null);
  const [label, setLabel] = useState("");
  const [kind, setKind] = useState("api_key");
  const [boundProviderId, setBoundProviderId] = useState("");
  const [fields, setFields] = useState<readonly SecretFieldDraft[]>([
    { name: "api_key", value: "" },
  ]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    onDirtyChange?.(editing !== null && (fields.some(field => field.value !== "") || label.trim() !== ""));
    return () => onDirtyChange?.(false);
  }, [editing, fields, label, onDirtyChange]);

  const openEditor = (
    target: CredentialMetadataConfiguration | "new",
  ) => {
    setEditing(target);
    setLabel(target === "new" ? "" : target.label);
    setKind(target === "new" ? "api_key" : target.kind);
    setBoundProviderId(
      target === "new" ? "" : (target.boundProviderId ?? ""),
    );
    setFields(
      (target === "new" ? ["api_key"] : target.fieldNames).map((name) => ({
        name,
        value: "",
      })),
    );
    setError(null);
  };
  const save = () => {
    const provider = providers.find(({ id }) => id === boundProviderId);
    if (label.trim() === "") {
      setError("Credential label is required.");
      return;
    }
    if (
      fields.length === 0 ||
      fields.some(({ name, value }) => name.trim() === "" || value === "")
    ) {
      setError("Every credential field needs a name and a new secret value.");
      return;
    }
    if (new Set(fields.map(({ name }) => name.trim())).size !== fields.length) {
      setError("Credential field names must be unique.");
      return;
    }
    setBusy(true);
    setError(null);
    void onStore({
      replaceCredentialRef:
        editing === null || editing === "new" ? null : editing.credentialRef,
      label: label.trim(),
      kind,
      boundProviderId: provider?.id ?? null,
      boundEndpoint: provider?.baseUrl ?? null,
      fields: Object.fromEntries(
        fields.map(({ name, value }) => [name.trim(), value]),
      ),
    })
      .then(() => {
        setFields((current) => current.map(({ name }) => ({ name, value: "" })));
        setEditing(null);
      })
      .catch((failure: unknown) =>
        setError(failure instanceof Error ? failure.message : String(failure)),
      )
      .finally(() => setBusy(false));
  };
  return (
    <div className="settings-section-stack">
      <div className="section-heading-row">
        <p className="section-intro">
          Secret values go directly to the operating-system credential store.
          Aworkit configuration and this screen receive only opaque references and field names.
        </p>
        <button
          title="Create a new write-only operating-system credential record"
          type="button"
          onClick={() => openEditor("new")}
        >
          Add credential
        </button>
      </div>
      {editing !== null && (
        <section className="settings-record credential-editor" aria-labelledby="credential-editor-heading">
          <div className="settings-record-heading">
            <h3 id="credential-editor-heading">
              {editing === "new" ? "New credential" : `Replace ${editing.label}`}
            </h3>
            <button
              title="Close this credential editor and clear every entered secret from the UI"
              type="button"
              onClick={() => {
                setFields([]);
                setEditing(null);
              }}
            >
              Cancel
            </button>
          </div>
          <div className="settings-grid two-columns">
            <label className="settings-field" htmlFor="credential-label">
              Label
              <input
                id="credential-label"
                title="Human-readable label; never put a secret value in this field"
                type="text"
                value={label}
                onChange={(event) => setLabel(event.target.value)}
              />
            </label>
            <label className="settings-field" htmlFor="credential-kind">
              Kind
              <select
                id="credential-kind"
                title="Semantic credential type used by provider and integration forms"
                value={kind}
                onChange={(event) => setKind(event.target.value)}
              >
                <option value="api_key">API key</option>
                <option value="token">Access token</option>
                <option value="username_password">Username and password</option>
                <option value="custom">Custom fields</option>
              </select>
            </label>
          </div>
          <label className="settings-field" htmlFor="credential-bound-provider">
            Provider binding
            <select
              id="credential-bound-provider"
              title="Optionally bind this credential to one provider identity and its current endpoint to prevent cross-endpoint reuse"
              value={boundProviderId}
              onChange={(event) => setBoundProviderId(event.target.value)}
            >
              <option value="">Unbound integration credential</option>
              {providers.map((provider) => (
                <option key={provider.id} value={provider.id}>
                  {provider.name} · {provider.baseUrl}
                </option>
              ))}
            </select>
          </label>
          <fieldset className="settings-bindings">
            <legend>Write-only fields</legend>
            {fields.map((field, index) => (
              <div className="settings-binding-row" key={`${index}-${field.name}`}>
                <label className="settings-field" htmlFor={`credential-field-${index}-name`}>
                  Field name
                  <input
                    id={`credential-field-${index}-name`}
                    autoComplete="off"
                    spellCheck={false}
                    title="Stable non-secret field name used by credential bindings"
                    type="text"
                    value={field.name}
                    onChange={(event) =>
                      setFields(
                        replaceAt(fields, index, {
                          ...field,
                          name: event.target.value,
                        }),
                      )
                    }
                  />
                </label>
                <label className="settings-field" htmlFor={`credential-field-${index}-value`}>
                  New secret value
                  <input
                    id={`credential-field-${index}-value`}
                    autoComplete="new-password"
                    spellCheck={false}
                    title="Write-only value sent to the trusted core; saved values are never read back into the UI"
                    type="password"
                    value={field.value}
                    onChange={(event) =>
                      setFields(
                        replaceAt(fields, index, {
                          ...field,
                          value: event.target.value,
                        }),
                      )
                    }
                  />
                </label>
                <button
                  aria-label={`Remove credential field ${field.name}`}
                  className="danger-action"
                  title="Remove this field from the pending credential replacement"
                  type="button"
                  onClick={() => setFields(removeAt(fields, index))}
                >
                  Remove
                </button>
              </div>
            ))}
            <button
              title="Add another named secret field to this credential record"
              type="button"
              onClick={() =>
                setFields([
                  ...fields,
                  { name: `field_${fields.length + 1}`, value: "" },
                ])
              }
            >
              Add field
            </button>
          </fieldset>
          {error !== null && <p className="field-error" role="alert">{error}</p>}
          <div className="section-actions">
            <button
              className="primary-action"
              disabled={busy}
              title="Store this new secret record and atomically update opaque credential metadata"
              type="button"
              onClick={save}
            >
              {busy ? "Storing…" : editing === "new" ? "Store credential" : "Replace credential"}
            </button>
          </div>
        </section>
      )}
      {editing === null && error !== null && (
        <p className="field-error" role="alert">{error}</p>
      )}
      {credentials.length === 0 ? (
        <p className="settings-empty">No credential metadata configured.</p>
      ) : (
        <div className="settings-record-list">
          {credentials.map((credential) => (
            <section className="settings-record" key={credential.credentialRef}>
              <div className="settings-record-heading">
                <div>
                  <h3>{credential.label}</h3>
                  <code>{credential.credentialRef}</code>
                </div>
                <span
                  className="status configured"
                  title="Opaque metadata is configured. The operating-system secret is verified only when a Test or workflow redeems it."
                >
                  metadata configured · revision {credential.revision}
                </span>
              </div>
              <p>{credential.kind} · fields: {credential.fieldNames.join(", ")}</p>
              {credential.boundProviderId && (
                <p>Bound to {credential.boundProviderId} at {credential.boundEndpoint}</p>
              )}
              <div className="section-actions">
                <button
                  title={`Replace ${credential.label} with new write-only values; the current values remain hidden`}
                  type="button"
                  onClick={() => openEditor(credential)}
                >
                  Replace…
                </button>
                <button
                  className="danger-action"
                  disabled={busy}
                  title={`Delete ${credential.label} only when no provider, tool, MCP server, or external agent references it`}
                  type="button"
                  onClick={() => {
                    setBusy(true);
                    setError(null);
                    void confirm(
                      `Delete ${credential.label}?`,
                      "The operating-system credential record will be removed. Aworkit refuses deletion while a configuration binding still references it.",
                    )
                      .then(async (accepted) => {
                        if (accepted) await onDelete(credential.credentialRef);
                      })
                      .catch((failure: unknown) =>
                        setError(
                          failure instanceof Error
                            ? failure.message
                            : String(failure),
                        ),
                      )
                      .finally(() => setBusy(false));
                  }}
                >
                  Delete
                </button>
              </div>
            </section>
          ))}
        </div>
      )}
    </div>
  );
}

export function ToolsSection({
  onPickCommand,
  tools,
  credentials,
  projects,
  onChange,
  onProbe,
}: {
  readonly onPickCommand?: () => Promise<string | null>;
  readonly tools: readonly BuiltInToolConfiguration[];
  readonly credentials: readonly CredentialMetadataConfiguration[];
  readonly projects: readonly ProjectConfiguration[];
  readonly onChange: (tools: readonly BuiltInToolConfiguration[]) => void;
  readonly onProbe: (
    tool: BuiltInToolConfiguration,
    project: ProjectConfiguration | null,
  ) => Promise<ToolProbeResult>;
}): React.JSX.Element {
  const [selectedProjects, setSelectedProjects] = useState<
    Readonly<Record<string, string>>
  >({});
  const [probing, setProbing] = useState<string | null>(null);
  const [probeResults, setProbeResults] = useState<
    Readonly<Record<string, ToolProbeResult>>
  >({});
  const probeableProjects = projects.filter(
    ({ workspace }) => workspace.kind !== "remote",
  );
  return (
    <div className="settings-section-stack">
      <p className="section-intro">
        Enable the tools you want available, then select them in a workflow.
        Each selected tool contributes its instructions to the agent.
      </p>
      {tools.length === 0 ? (
        <p className="settings-empty">
          This runtime reported no installed built-in tools.
        </p>
      ) : (
        <div className="settings-record-list">
          {tools.map((tool, index) => {
            const bindingCompatible =
              tool.id === "tool.web_search"
                ? tool.credentialBindings.length <= 1 &&
                  tool.credentialBindings.every(({ name }) => name === "api_key")
                : tool.credentialBindings.length === 0;
            const selectedProjectId =
              selectedProjects[tool.id] ?? probeableProjects[0]?.id ?? "";
            const selectedProject =
              probeableProjects.find(({ id }) => id === selectedProjectId) ?? null;
            const probeFingerprint = settingsRecordFingerprint({
              tool,
              project: selectedProject,
            });
            const storedProbeResult = probeResults[tool.id];
            const probeResult =
              storedProbeResult?.draftFingerprint === probeFingerprint
                ? storedProbeResult
                : undefined;
            const updateTool = (next: BuiltInToolConfiguration) => {
              setProbeResults((current) => {
                if (!(tool.id in current)) return current;
                const nextResults = { ...current };
                delete nextResults[tool.id];
                return nextResults;
              });
              onChange(replaceAt(tools, index, next));
            };
            return (
              <section className="settings-record" key={tool.id}>
              <div className="settings-record-heading">
                <div>
                  <h3>{tool.name}</h3>
                  <code>{tool.id}</code>
                </div>
                <span
                  className={`status ${tool.enabled ? "configured" : "disabled"}`}
                >
                  {tool.enabled ? "enabled" : "disabled"}
                </span>
              </div>
              <label className="switch-label" htmlFor={`${tool.id}-enabled`}>
                <input
                  checked={tool.enabled}
                  id={`${tool.id}-enabled`}
                  title="Make this built-in tool available for workflow binding"
                  type="checkbox"
                  onChange={(event) =>
                    updateTool({
                      ...tool,
                      enabled: event.target.checked,
                    })
                  }
                />
                Available to workflows
              </label>
              <p>
                {toolDescription(tool.id)} {tool.requiresProject
                  ? "Files are confined to the selected project or, with No project, the Chat's private working folder."
                  : ""}
              </p>
              <div className="provider-actions">
                {tool.requiresProject && (
                  <label className="settings-field" htmlFor={`${tool.id}-probe-project`}>
                    Test project
                    <select
                      id={`${tool.id}-probe-project`}
                      title="Exact unsaved project workspace to use for this root-confined tool health probe"
                      value={selectedProjectId}
                      onChange={(event) => {
                        setProbeResults((current) => {
                          if (!(tool.id in current)) return current;
                          const nextResults = { ...current };
                          delete nextResults[tool.id];
                          return nextResults;
                        });
                        setSelectedProjects((current) => ({
                          ...current,
                          [tool.id]: event.target.value,
                        }));
                      }}
                    >
                      <option value="">Select a project</option>
                      {probeableProjects.map((project) => (
                        <option key={project.id} value={project.id}>
                          {project.name}
                        </option>
                      ))}
                    </select>
                    {projects.some(({ workspace }) => workspace.kind === "remote") && (
                      <span className="field-warning">
                        Remote prepared workspaces are omitted because no
                        built-in tool adapter can resolve them in this build.
                      </span>
                    )}
                  </label>
                )}
                <button
                  disabled={
                    probing !== null ||
                    (tool.requiresProject && selectedProject === null)
                  }
                  title={
                    tool.requiresProject && selectedProject === null
                      ? "Add or select a project before testing this project-scoped tool"
                      : tool.id === "tool.web_search" &&
                          tool.credentialBindings.length > 0
                        ? "Run one live configured web search; this verifies the provider adapter and may incur provider charges"
                        : "Exercise the installed bounded adapter without granting it to a workflow"
                  }
                  type="button"
                  onClick={() => {
                    const requestedFingerprint = settingsRecordFingerprint({
                      tool,
                      project: selectedProject,
                    });
                    setProbing(tool.id);
                    void onProbe(tool, selectedProject)
                      .then((result) =>
                        setProbeResults((current) => ({
                          ...current,
                          [tool.id]: result,
                        })),
                      )
                      .catch((failure: unknown) =>
                        setProbeResults((current) => ({
                          ...current,
                          [tool.id]: {
                            ok: false,
                            toolId: tool.id,
                            adapter: "unavailable",
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
                >
                  {probing === tool.id ? "Probing…" : "Probe adapter only"}
                </button>
                {probeResult !== undefined && (
                  <p
                    className={`provider-detail ${probeResult.ok ? "diagnostic" : "error"}`}
                    role="status"
                  >
                    {probeResult.message} · {probeResult.adapter}. Adapter
                    probe only; this is not workflow execution readiness.
                  </p>
                )}
              </div>
              {!bindingCompatible && (
                <div className="field-warning" role="alert">
                  <p>
                    This saved draft contains unsupported credential bindings
                    that do not match the installed adapter contract. Native
                    probes and workflow execution reject them.
                  </p>
                  <button
                    title="Remove every unsupported credential binding from this built-in tool draft"
                    type="button"
                    onClick={() =>
                      updateTool({ ...tool, credentialBindings: [] })
                    }
                  >
                    Remove unsupported bindings
                  </button>
                </div>
              )}
              <ToolConfigurationEditor tool={tool} onPickCommand={onPickCommand} onChange={updateTool}
                configuration={!["tool.web_search", "tool.web_fetch", "tool.web_extract"].includes(tool.id)} />
              {tool.id === "tool.web_search" && <WebSearchSettingsEditor tool={tool} credentials={credentials} onChange={updateTool} />}
              {(tool.id === "tool.web_fetch" || tool.id === "tool.web_extract") && <WebExtractionSettingsEditor tool={tool} onChange={updateTool} />}
              </section>
            );
          })}
        </div>
      )}
    </div>
  );
}

function toolDescription(toolId: string): string {
  return findNativeTool(toolId)?.description ?? "Tool plugin unavailable.";
}

function replaceAt<T>(values: readonly T[], index: number, value: T): T[] {
  return values.map((item, itemIndex) => (itemIndex === index ? value : item));
}

function removeAt<T>(values: readonly T[], index: number): T[] {
  return values.filter((_, itemIndex) => itemIndex !== index);
}
