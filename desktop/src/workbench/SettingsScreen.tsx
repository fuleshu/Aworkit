import { useEffect, useMemo, useState } from "react";
import { projectAppearancePreference } from "./appearance";
import {
  authorityPreview,
  canCommitDraft,
  resolveCapabilities,
  updateDraft,
  type CapabilityRecord,
  type SettingsDraft,
  type SettingsSection,
} from "./settings";
import {
  createSettingsCorePort,
  nextWorkbenchCommandId,
  type SettingsCorePort,
} from "./corePort";

const capabilities: readonly CapabilityRecord[] = [
  {
    id: "model.local",
    label: "Local model",
    kind: "Provider",
    state: "ready",
    version: "1.0",
  },
  {
    id: "model.standard",
    label: "Standard model tier",
    kind: "Model tier",
    state: "ready",
  },
  { id: "tool.files", label: "Project files", kind: "Tool", state: "ready" },
  { id: "tool.shell", label: "Shell", kind: "Tool", state: "ready" },
  {
    id: "mcp.github",
    label: "GitHub MCP",
    kind: "MCP",
    state: "disabled",
    detail: "Not enabled",
  },
  {
    id: "agent.codex",
    label: "Codex App Server",
    kind: "External agent",
    state: "ready",
    version: "v1",
  },
  {
    id: "plugin.review",
    label: "Review extension",
    kind: "Extension",
    state: "incompatible",
    detail: "Requires host protocol v2",
  },
];
const sections: readonly {
  readonly id: SettingsSection;
  readonly label: string;
}[] = [
  { id: "providers", label: "Providers" },
  { id: "model_tiers", label: "Model tiers" },
  { id: "credentials", label: "Credentials" },
  { id: "tools", label: "Tools" },
  { id: "extensions", label: "Extensions" },
  { id: "mcp", label: "MCP servers" },
  { id: "external_agents", label: "External agents" },
  { id: "data", label: "Data & portable sessions" },
  { id: "projects", label: "Projects" },
  { id: "appearance", label: "Appearance" },
];

/** Progressive settings surface; sensitive values remain credential references, never plaintext. */
export function SettingsScreen({
  settingsPort,
}: {
  readonly settingsPort?: SettingsCorePort;
}): React.JSX.Element {
  const port = useMemo(
    () => settingsPort ?? createSettingsCorePort(),
    [settingsPort],
  );
  const [section, setSection] = useState<SettingsSection>("providers");
  const [projectedVersion, setProjectedVersion] = useState(3);
  const [draft, setDraft] = useState<SettingsDraft>({
    version: 3,
    appearance: "system",
    configuredCapabilities: new Set(
      capabilities
        .filter((item) => item.state === "ready")
        .map((item) => item.id),
    ),
    portableHistoryEnabled: false,
    dirtySections: new Set(),
  });
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [retryCommandId, setRetryCommandId] = useState<string | null>(null);
  const [projectRoots, setProjectRoots] = useState<readonly string[]>([]);
  useEffect(() => {
    let active = true;
    void port
      .snapshot()
      .then((snapshot) => {
        if (!active) return;
        setProjectedVersion(snapshot.version);
        setProjectRoots(snapshot.projectRoots);
        projectAppearancePreference(snapshot.appearance);
        setDraft({
          version: snapshot.version,
          appearance: snapshot.appearance,
          configuredCapabilities: new Set(snapshot.configuredCapabilities),
          portableHistoryEnabled: snapshot.portableHistoryEnabled,
          dirtySections: new Set(),
        });
        setError(null);
      })
      .catch((failure: unknown) => {
        if (active)
          setError(
            failure instanceof Error ? failure.message : String(failure),
          );
      });
    return () => {
      active = false;
    };
  }, [port]);
  const resolution = useMemo(
    () =>
      resolveCapabilities(
        [
          { id: "model.local", label: "Local model" },
          { id: "tool.files", label: "Project files" },
          { id: "plugin.review", label: "Review extension" },
        ],
        draft.configuredCapabilities,
        capabilities,
      ),
    [draft.configuredCapabilities],
  );
  const toggleCapability = (id: string, enabled: boolean) =>
    setDraft((current) => {
      const configured = new Set(current.configuredCapabilities);
      if (enabled) configured.add(id);
      else configured.delete(id);
      return updateDraft(
        current,
        { configuredCapabilities: configured },
        section,
      );
    });
  const save = async () => {
    if (!canCommitDraft(draft, projectedVersion)) return;
    const commandId = retryCommandId ?? nextWorkbenchCommandId("settings");
    const command = {
      commandId,
      expectedVersion: projectedVersion,
      appearance: draft.appearance,
      configuredCapabilities: [...draft.configuredCapabilities].sort(),
      portableHistoryEnabled: draft.portableHistoryEnabled ?? false,
    } as const;
    setRetryCommandId(commandId);
    setSaving(true);
    try {
      const receipt = await port.commit(command);
      if (!receipt.accepted) {
        setRetryCommandId(null);
        setError(receipt.reason ?? "The trusted core rejected settings.");
        try {
          projectAppearancePreference((await port.snapshot()).appearance);
        } catch {
          // Keep the last authoritative appearance if projection recovery fails.
        }
        return;
      }
      projectAppearancePreference(draft.appearance);
      setProjectedVersion(receipt.currentVersion);
      setDraft((current) => ({
        ...current,
        version: receipt.currentVersion,
        dirtySections: new Set(),
      }));
      setRetryCommandId(null);
      setError(null);
    } catch (failure) {
      const failureMessage =
        failure instanceof Error ? failure.message : String(failure);
      setError(failureMessage);
      try {
        const latest = await port.snapshot();
        projectAppearancePreference(latest.appearance);
        const sameCapabilities =
          [...latest.configuredCapabilities].sort().join("\0") ===
          command.configuredCapabilities.join("\0");
        if (
          latest.version > command.expectedVersion &&
          latest.appearance === command.appearance &&
          latest.portableHistoryEnabled === command.portableHistoryEnabled &&
          sameCapabilities
        ) {
          setProjectedVersion(latest.version);
          setDraft((current) => ({
            ...current,
            version: latest.version,
            dirtySections: new Set(),
          }));
          setRetryCommandId(null);
          setError(null);
        } else if (failureMessage.includes("version conflict")) {
          setRetryCommandId(null);
          setProjectedVersion(latest.version);
          setDraft((current) => ({ ...current, version: latest.version }));
        }
      } catch {
        // Preserve the complete draft and exact command for uncertain retry.
      }
    } finally {
      setSaving(false);
    }
  };
  return (
    <section className="settings-workspace">
      <header className="surface-toolbar">
        <div>
          <p className="eyebrow">AWORKIT</p>
          <h1>Settings</h1>
        </div>
        <div className="toolbar-actions">
          <span>Version {projectedVersion}</span>
          <button
            className="primary-action"
            disabled={saving || !canCommitDraft(draft, projectedVersion)}
            title={
              canCommitDraft(draft, projectedVersion)
                ? "Commit the complete version-checked settings draft"
                : "No valid settings changes to save"
            }
            type="button"
            onClick={() => void save()}
          >
            Save changes
          </button>
        </div>
      </header>
      {error !== null && (
        <div className="command-banner" role="status">
          {error} Your complete draft is still local. Uncertain delivery retries
          use the same command ID; a known version conflict is rebased as a new
          command.
        </div>
      )}
      <div className="settings-body">
        <nav aria-label="Settings sections">
          {sections.map((item) => (
            <button
              aria-current={item.id === section ? "page" : undefined}
              key={item.id}
              type="button"
              onClick={() => setSection(item.id)}
            >
              {item.label}
            </button>
          ))}
        </nav>
        <main>
          <h2>{sections.find((item) => item.id === section)?.label}</h2>
          <p className="section-intro">
            Changes remain local until one complete version-checked command is
            accepted by the trusted core.
          </p>
          {section === "appearance" ? (
            <AppearanceSettings
              draft={draft}
              onChange={(appearance) =>
                setDraft((current) =>
                  updateDraft(current, { appearance }, "appearance"),
                )
              }
            />
          ) : section === "credentials" ? (
            <CredentialSettings />
          ) : section === "data" ? (
            <DataSettings
              draft={draft}
              onChange={(portableHistoryEnabled) =>
                setDraft((current) =>
                  updateDraft(current, { portableHistoryEnabled }, "data"),
                )
              }
            />
          ) : section === "projects" ? (
            <ProjectSettings roots={projectRoots} />
          ) : (
            <CapabilitySettings
              section={section}
              draft={draft}
              onToggle={toggleCapability}
            />
          )}
          {section === "providers" && (
            <section className="resolution-summary">
              <h3>Repository Engineer resolution</h3>
              <dl>
                <div>
                  <dt>Ready</dt>
                  <dd>
                    {resolution.available
                      .map((item) => item.label)
                      .join(", ") || "None"}
                  </dd>
                </div>
                <div>
                  <dt>Missing</dt>
                  <dd>
                    {resolution.missing.map((item) => item.label).join(", ") ||
                      "None"}
                  </dd>
                </div>
                <div>
                  <dt>Incompatible</dt>
                  <dd>
                    {resolution.incompatible
                      .map((item) => item.label)
                      .join(", ") || "None"}
                  </dd>
                </div>
              </dl>
              <h3>Authority preview</h3>
              <ul>
                {authorityPreview(draft.configuredCapabilities).map((line) => (
                  <li key={line}>{line}</li>
                ))}
              </ul>
            </section>
          )}
        </main>
      </div>
    </section>
  );
}

function CapabilitySettings({
  section,
  draft,
  onToggle,
}: {
  readonly section: SettingsSection;
  readonly draft: SettingsDraft;
  readonly onToggle: (id: string, enabled: boolean) => void;
}): React.JSX.Element {
  const matching = capabilities.filter(
    (item) => mapSection(item.kind) === section,
  );
  return (
    <div className="setting-list">
      {matching.length === 0 ? (
        <p className="empty-state">
          No {section.replaceAll("_", " ")} are configured.
        </p>
      ) : (
        matching.map((item) => (
          <article key={item.id}>
            <div>
              <strong>{item.label}</strong>
              <span>
                {item.id}{" "}
                {item.version === undefined ? "" : `· ${item.version}`}
              </span>
              <small>{item.detail}</small>
            </div>
            <span className={`status ${item.state}`}>{item.state}</span>
            <label className="switch-label">
              <input
                checked={draft.configuredCapabilities.has(item.id)}
                disabled={item.state === "incompatible"}
                title={`Enable or disable ${item.label}`}
                type="checkbox"
                onChange={(event) => onToggle(item.id, event.target.checked)}
              />
              Enabled
            </label>
          </article>
        ))
      )}
    </div>
  );
}
function AppearanceSettings({
  draft,
  onChange,
}: {
  readonly draft: SettingsDraft;
  readonly onChange: (mode: SettingsDraft["appearance"]) => void;
}): React.JSX.Element {
  return (
    <fieldset className="appearance-options">
      <legend>Color mode</legend>
      {(["system", "light", "dark"] as const).map((mode) => (
        <label key={mode}>
          <input
            checked={draft.appearance === mode}
            name="appearance"
            type="radio"
            onChange={() => {
              onChange(mode);
              projectAppearancePreference(mode);
            }}
          />
          <span className={`theme-preview ${mode}`} />
          <strong>{mode.replace(/^./, (value) => value.toUpperCase())}</strong>
          <small>
            {mode === "system"
              ? "Follow the operating system"
              : `Always use ${mode} appearance`}
          </small>
        </label>
      ))}
    </fieldset>
  );
}
function CredentialSettings(): React.JSX.Element {
  return (
    <div className="setting-list">
      <article>
        <div>
          <strong>OpenAI credential</strong>
          <span>
            credential.openai.primary · stored by OS credential service
          </span>
        </div>
        <span className="status ready">reference ready</span>
        <button
          disabled
          title="Replace the credential reference through a native secure dialog"
          type="button"
        >
          Replace…
        </button>
      </article>
      <p className="security-note">
        Secret values are never rendered, serialized into settings JSON, or
        returned to the webview.
      </p>
    </div>
  );
}
function DataSettings({
  draft,
  onChange,
}: {
  readonly draft: SettingsDraft;
  readonly onChange: (enabled: boolean) => void;
}): React.JSX.Element {
  return (
    <div className="setting-list">
      <article>
        <div>
          <strong>Portable project history</strong>
          <span>
            Opt-in immutable JSONL/CAS history in the selected project
          </span>
        </div>
        <label className="switch-label">
          <input
            checked={draft.portableHistoryEnabled ?? false}
            title="Enable portable history for newly created Chats"
            type="checkbox"
            onChange={(event) => onChange(event.target.checked)}
          />
          Enabled
        </label>
      </article>
      <article>
        <div>
          <strong>Local history</strong>
          <span>Machine-local SQLite semantic history · default</span>
        </div>
        <span className="status ready">ready</span>
      </article>
    </div>
  );
}
function ProjectSettings({
  roots,
}: {
  readonly roots: readonly string[];
}): React.JSX.Element {
  return (
    <div className="setting-list">
      {roots.map((root) => (
        <article key={root}>
          <div>
            <strong>{root.split(/[\\/]/).at(-1) || root}</strong>
            <span>{root}</span>
          </div>
          <span className="status ready">revalidated</span>
        </article>
      ))}
      {roots.length === 0 && (
        <p className="empty-state">No trusted project roots are configured.</p>
      )}
    </div>
  );
}
function mapSection(kind: string): SettingsSection {
  const value = kind.toLowerCase();
  if (value.includes("model tier")) return "model_tiers";
  if (value.includes("provider")) return "providers";
  if (value.includes("tool")) return "tools";
  if (value.includes("extension")) return "extensions";
  if (value.includes("mcp")) return "mcp";
  return "external_agents";
}
