/** Manifest-defined settings rendered as typed controls, never a JSON editor. */
import type {
  BuiltInToolConfiguration,
  ExternalAgentConfiguration,
} from "../configuration";
import { findNativeTool } from "../toolRegistry";
import { ToolOptionsEditor } from "./ToolOptionsEditor";

/** Adapter each delegation executor runs, so a target list can be filtered. */
const DELEGATION_ADAPTERS: Readonly<Record<string, string>> = {
  subagent_codex: "codex_app_server",
  subagent_claude_code: "claude_code",
};

export function ToolConfigurationEditor({ tool, configuration = true, externalAgents = [], onChange, onPickCommand }: {
  readonly tool: BuiltInToolConfiguration;
  readonly configuration?: boolean;
  readonly externalAgents?: readonly ExternalAgentConfiguration[];
  readonly onChange: (tool: BuiltInToolConfiguration) => void;
  readonly onPickCommand?: () => Promise<string | null>;
}): React.JSX.Element {
  const manifest = findNativeTool(tool.id);
  if (!manifest) return <p role="alert">The tool plugin is unavailable.</p>;
  const adapter = DELEGATION_ADAPTERS[manifest.executor];
  const targets =
    adapter === undefined
      ? []
      : externalAgents.filter((agent) => agent.adapter === adapter);
  return <div className="settings-section-stack">
    <p>{manifest.requiresProject ? "Execution: native, confined to the project or the Chat's private working folder." : manifest.execution === "native" ? "Execution: native Aworkit plugin." : "Execution: host process."}</p>
    <ToolOptionsEditor id={tool.id} value={tool.options} defaultInstructions={manifest.instructions} execution={manifest.execution}
      onPickCommand={onPickCommand} onChange={options => onChange({ ...tool, options })} />
    {configuration && <div className="settings-grid two-columns">{manifest.fields.map(field => {
      const value = tool.configuration[field.key] ?? manifest.configuration[field.key];
      const update = (next: unknown) => onChange({ ...tool, configuration: { ...tool.configuration, [field.key]: next } });
      // A boolean field reads as one row: control, then label and help. Every
      // other kind keeps its label above a full-width control.
      // A target selector offers only the configured targets of this tool's own
      // product, and keeps an empty value meaning "the first enabled target".
      if (field.kind === "external_agent_target") {
        const selected = typeof value === "string" ? value : "";
        const selectedIsConfigured = targets.some(({ id }) => id === selected);
        return <label className="settings-field" htmlFor={`${tool.id}-${field.key}`} key={field.key}>{field.label}
          <select id={`${tool.id}-${field.key}`} title={field.help} disabled={field.readOnly}
            value={selectedIsConfigured ? selected : ""}
            onChange={e => update(e.target.value)}>
            <option value="">First enabled target</option>
            {targets.map(agent => <option key={agent.id} value={agent.id}>{agent.name}{agent.enabled ? "" : " (disabled)"}</option>)}
            {selected !== "" && !selectedIsConfigured &&
              <option value={selected}>{selected} (not configured)</option>}
          </select>
          {targets.length === 0 &&
            <small className="config-help">No target of this product is configured yet; add one under External agents.</small>}
        </label>;
      }
      return field.kind === "boolean"
        ? <label className="settings-field settings-checkbox-field" htmlFor={`${tool.id}-${field.key}`} key={field.key}>
            <input id={`${tool.id}-${field.key}`} type="checkbox" title={field.help} disabled={field.readOnly} checked={value === true} onChange={e => update(e.target.checked)} />
            <span><strong>{field.label}</strong>{field.help.length > 0 && <small>{field.help}</small>}</span>
          </label>
        : <label className="settings-field" htmlFor={`${tool.id}-${field.key}`} key={field.key}>{field.label}
            <input id={`${tool.id}-${field.key}`} type={field.kind === "integer" ? "number" : "text"} title={field.help} readOnly={field.readOnly}
              min={field.minimum} max={field.maximum} step={field.kind === "integer" ? 1 : undefined}
              value={Array.isArray(value) ? value.join(", ") : String(value ?? "")} onChange={e => update(field.kind === "integer" ? Number(e.target.value) : field.kind === "list" ? e.target.value.split(",").map(v => v.trim()).filter(Boolean) : e.target.value)} />
          </label>;
    })}</div>}
  </div>;
}
