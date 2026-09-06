/** Manifest-defined settings rendered as typed controls, never a JSON editor. */
import type { BuiltInToolConfiguration } from "../configuration";
import { findNativeTool } from "../toolRegistry";
import { ToolOptionsEditor } from "./ToolOptionsEditor";

export function ToolConfigurationEditor({ tool, configuration = true, onChange, onPickCommand }: {
  readonly tool: BuiltInToolConfiguration;
  readonly configuration?: boolean;
  readonly onChange: (tool: BuiltInToolConfiguration) => void;
  readonly onPickCommand?: () => Promise<string | null>;
}): React.JSX.Element {
  const manifest = findNativeTool(tool.id);
  if (!manifest) return <p role="alert">The tool plugin is unavailable.</p>;
  return <div className="settings-section-stack">
    <p>{manifest.requiresProject ? "Execution: native, confined to the selected project." : manifest.execution === "native" ? "Execution: native Aworkit plugin." : "Execution: host process."}</p>
    <ToolOptionsEditor id={tool.id} value={tool.options} defaultInstructions={manifest.instructions} execution={manifest.execution}
      onPickCommand={onPickCommand} onChange={options => onChange({ ...tool, options })} />
    {configuration && <div className="settings-grid two-columns">{manifest.fields.map(field => {
      const value = tool.configuration[field.key] ?? manifest.configuration[field.key];
      const update = (next: unknown) => onChange({ ...tool, configuration: { ...tool.configuration, [field.key]: next } });
      return <label className="settings-field" htmlFor={`${tool.id}-${field.key}`} key={field.key}>{field.label}
        {field.kind === "boolean" ? <input id={`${tool.id}-${field.key}`} type="checkbox" title={field.help} disabled={field.readOnly} checked={value === true} onChange={e => update(e.target.checked)} />
          : <input id={`${tool.id}-${field.key}`} type={field.kind === "integer" ? "number" : "text"} title={field.help} readOnly={field.readOnly}
            min={field.minimum} max={field.maximum} step={field.kind === "integer" ? 1 : undefined}
            value={Array.isArray(value) ? value.join(", ") : String(value ?? "")} onChange={e => update(field.kind === "integer" ? Number(e.target.value) : field.kind === "list" ? e.target.value.split(",").map(v => v.trim()).filter(Boolean) : e.target.value)} />}
      </label>;
    })}</div>}
  </div>;
}
