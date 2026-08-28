import { useEffect, useMemo, useState } from "react";
import type { SettingsV2Snapshot } from "./configuration";
import {
  catalogEntryForType,
  type ConfigurationField,
} from "./nodeCatalog";
import type { JsonObject, JsonValue } from "./workflow";

interface NodeConfigurationFormProps {
  readonly nodeType: string;
  readonly configuration: JsonObject;
  readonly editable: boolean;
  readonly settings?: SettingsV2Snapshot;
  readonly onPendingDraftChange?: (pending: boolean) => void;
  readonly onChange: (patch: JsonObject) => void;
}

const FALLBACK_TIERS: readonly { readonly value: string; readonly label: string }[] = [
  { value: "tier:fast", label: "Fast" },
  { value: "tier:simple", label: "Simple" },
  { value: "tier:balanced", label: "Balanced" },
  { value: "tier:quality", label: "Quality" },
];

/**
 * Typed per-node configuration form. Known catalog node types render their
 * declared fields; unknown types render nothing (the raw JSON editor remains
 * available in the parent). All scalar edits are immediate single transactions;
 * JSON sub-objects use a local draft so they commit as one undoable step.
 */
export function NodeConfigurationForm({
  nodeType,
  configuration,
  editable,
  settings,
  onPendingDraftChange,
  onChange,
}: NodeConfigurationFormProps): React.JSX.Element | null {
  const entry = catalogEntryForType(nodeType);
  if (entry === undefined || entry.fields.length === 0) return null;
  const options = useMemo(
    () => resolveFieldOptions(settings),
    [settings],
  );
  return (
    <div className="node-config-form" role="group" aria-label="Node configuration">
      {entry.fields.map((field) => (
        <ConfigurationFieldInput
          configuration={configuration}
          editable={editable}
          field={field}
          key={field.key}
          options={options}
          onChange={onChange}
          onPendingDraftChange={onPendingDraftChange}
        />
      ))}
    </div>
  );
}

interface FieldOptions {
  readonly tiers: readonly { readonly value: string; readonly label: string }[];
  readonly tools: readonly { readonly value: string; readonly label: string }[];
  readonly mcpServers: readonly { readonly value: string; readonly label: string }[];
}

function resolveFieldOptions(settings?: SettingsV2Snapshot): FieldOptions {
  if (settings === undefined)
    return { tiers: FALLBACK_TIERS, tools: [], mcpServers: [] };
  const tiers =
    settings.settings.modelTiers.length > 0
      ? settings.settings.modelTiers.map(({ id, name }) => ({ value: id, label: name }))
      : FALLBACK_TIERS;
  const tools = settings.settings.tools
    .filter((tool) => tool.enabled)
    .map(({ id, name }) => ({ value: id, label: name }));
  const mcpServers = settings.settings.mcpServers
    .filter((server) => server.enabled)
    .map(({ id, name }) => ({ value: id, label: name }));
  return { tiers, tools, mcpServers };
}

function ConfigurationFieldInput({
  field,
  configuration,
  editable,
  options,
  onChange,
  onPendingDraftChange,
}: {
  readonly field: ConfigurationField;
  readonly configuration: JsonObject;
  readonly editable: boolean;
  readonly options: FieldOptions;
  readonly onChange: (patch: JsonObject) => void;
  readonly onPendingDraftChange?: (pending: boolean) => void;
}): React.JSX.Element {
  switch (field.kind) {
    case "modelTier":
      return (
        <label>
          {field.label}
          <select
            disabled={!editable}
            title={`Choose the model tier for ${field.label.toLowerCase()}`}
            value={stringValue(configuration[field.key])}
            onChange={(event) => onChange({ [field.key]: event.target.value })}
          >
            <option value="">Unset</option>
            {options.tiers.map(({ value, label }) => (
              <option key={value} value={value}>
                {label} ({value})
              </option>
            ))}
          </select>
        </label>
      );
    case "toolSingle":
      return (
        <ToolSingleField
          configuration={configuration}
          editable={editable}
          field={field}
          options={options}
          onChange={onChange}
        />
      );
    case "toolMulti":
      return (
        <ToolMultiField
          configuration={configuration}
          editable={editable}
          field={field}
          options={options}
          onChange={onChange}
        />
      );
    case "number":
      return (
        <label>
          {field.label}
          <input
            disabled={!editable}
            max={field.max}
            min={field.min}
            placeholder={field.defaultValue?.toString()}
            step={field.step ?? 1}
            title={`${field.label} between ${field.min} and ${field.max}`}
            type="number"
            value={numberValue(configuration[field.key])}
            onChange={(event) => {
              const parsed = Number(event.target.value);
              onChange({
                [field.key]: Number.isFinite(parsed) ? parsed : null,
              });
            }}
          />
        </label>
      );
    case "textarea":
      return (
        <label>
          {field.label}
          <textarea
            disabled={!editable}
            placeholder={field.placeholder}
            rows={4}
            title={`Edit ${field.label.toLowerCase()}`}
            value={stringValue(configuration[field.key])}
            onChange={(event) => onChange({ [field.key]: event.target.value })}
          />
        </label>
      );
    case "text":
      return (
        <label>
          {field.label}
          <input
            disabled={!editable}
            title={`Edit ${field.label.toLowerCase()}`}
            value={stringValue(configuration[field.key])}
            onChange={(event) => onChange({ [field.key]: event.target.value })}
          />
        </label>
      );
    case "json":
      return (
        <JsonField
          configuration={configuration}
          editable={editable}
          field={field}
          onChange={onChange}
          onPendingDraftChange={onPendingDraftChange}
        />
      );
    case "predicate":
      return (
        <PredicateField
          configuration={configuration}
          editable={editable}
          field={field}
          onChange={onChange}
        />
      );
  }
}

function ToolSingleField({
  field,
  configuration,
  editable,
  options,
  onChange,
}: {
  readonly field: Extract<ConfigurationField, { kind: "toolSingle" }>;
  readonly configuration: JsonObject;
  readonly editable: boolean;
  readonly options: FieldOptions;
  readonly onChange: (patch: JsonObject) => void;
}): React.JSX.Element {
  const current = stringValue(configuration[field.key]);
  const isBuiltIn = options.tools.some(({ value }) => value === current);
  return (
    <div className="config-field-stack">
      <label>
        {field.label}
        <select
          disabled={!editable}
          title={`Choose an enabled built-in tool`}
          value={isBuiltIn ? current : ""}
          onChange={(event) => {
            if (event.target.value !== "") onChange({ [field.key]: event.target.value });
          }}
        >
          <option value="">{isBuiltIn ? "Custom / MCP" : "Unset"}</option>
          {options.tools.map(({ value, label }) => (
            <option key={value} value={value}>
              {label} ({value})
            </option>
          ))}
        </select>
      </label>
      <label>
        Custom or MCP tool id
        <input
          disabled={!editable}
          placeholder="mcp://server/tool"
          title="Enter an mcp:// tool id or any other free-form tool reference"
          value={current}
          onChange={(event) => onChange({ [field.key]: event.target.value })}
        />
      </label>
      {options.mcpServers.length > 0 && (
        <small className="config-help">
          Enabled MCP servers:{" "}
          {options.mcpServers.map(({ value, label }) => `${label} (${value})`).join(", ")}
        </small>
      )}
    </div>
  );
}

function ToolMultiField({
  field,
  configuration,
  editable,
  options,
  onChange,
}: {
  readonly field: Extract<ConfigurationField, { kind: "toolMulti" }>;
  readonly configuration: JsonObject;
  readonly editable: boolean;
  readonly options: FieldOptions;
  readonly onChange: (patch: JsonObject) => void;
}): React.JSX.Element {
  const selected = stringArrayValue(configuration[field.key]);
  const known = new Set(options.tools.map(({ value }) => value));
  const extras = selected.filter((id) => !known.has(id));
  const [freeEntry, setFreeEntry] = useState("");
  const toggle = (toolId: string) => {
    const next = selected.includes(toolId)
      ? selected.filter((id) => id !== toolId)
      : [...selected, toolId];
    onChange({ [field.key]: next });
  };
  const addFreeEntry = () => {
    const id = freeEntry.trim();
    if (id === "" || selected.includes(id)) return;
    onChange({ [field.key]: [...selected, id] });
    setFreeEntry("");
  };
  return (
    <fieldset className="tool-multi-field">
      <legend>{field.label}</legend>
      {options.tools.map(({ value, label }) => (
        <label className="checkbox-row" key={value}>
          <input
            checked={selected.includes(value)}
            disabled={!editable}
            title={`Bind ${label} to this agent`}
            type="checkbox"
            onChange={() => toggle(value)}
          />
          <span>
            {label} <code>{value}</code>
          </span>
        </label>
      ))}
      {extras.map((id) => (
        <label className="checkbox-row" key={id}>
          <input
            checked
            disabled={!editable}
            title={`Remove the preserved tool binding ${id}`}
            type="checkbox"
            onChange={() =>
              onChange({ [field.key]: selected.filter((value) => value !== id) })
            }
          />
          <span>
            <code>{id}</code>
          </span>
        </label>
      ))}
      <div className="config-field-stack">
        <label>
          Add MCP tool
          <input
            disabled={!editable}
            placeholder="mcp://server/tool"
            title="Enter an mcp:// tool id from an enabled MCP server and add it"
            value={freeEntry}
            onChange={(event) => setFreeEntry(event.target.value)}
          />
        </label>
        <button
          disabled={!editable || freeEntry.trim() === ""}
          title="Add the entered MCP tool id to the agent bindings"
          type="button"
          onClick={addFreeEntry}
        >
          Add tool
        </button>
      </div>
      {options.mcpServers.length > 0 && (
        <small className="config-help">
          Enabled MCP servers:{" "}
          {options.mcpServers.map(({ value, label }) => `${label} (${value})`).join(", ")}
        </small>
      )}
    </fieldset>
  );
}

function JsonField({
  field,
  configuration,
  editable,
  onChange,
  onPendingDraftChange,
}: {
  readonly field: Extract<ConfigurationField, { kind: "json" }>;
  readonly configuration: JsonObject;
  readonly editable: boolean;
  readonly onChange: (patch: JsonObject) => void;
  readonly onPendingDraftChange?: (pending: boolean) => void;
}): React.JSX.Element {
  const applied = JSON.stringify(objectValue(configuration[field.key]), null, 2);
  const [draft, setDraft] = useState(applied);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setDraft(applied);
    setError(null);
  }, [applied]);
  const pending = draft !== applied;
  useEffect(() => {
    onPendingDraftChange?.(pending);
  }, [onPendingDraftChange, pending]);
  const apply = () => {
    try {
      const value: unknown = JSON.parse(draft);
      if (typeof value !== "object" || value === null || Array.isArray(value))
        throw new Error(`${field.label} must be a JSON object.`);
      setError(null);
      onChange({ [field.key]: value as JsonValue });
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };
  return (
    <div className="config-field-stack">
      <label>
        {field.label}
        <textarea
          aria-invalid={error !== null}
          disabled={!editable}
          rows={6}
          spellCheck={false}
          title={`Edit ${field.label.toLowerCase()} as one JSON object`}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
        />
      </label>
      <button
        disabled={!editable || !pending}
        title={`Apply ${field.label.toLowerCase()} as one undoable transaction`}
        type="button"
        onClick={apply}
      >
        Apply {field.label.toLowerCase()}
      </button>
      {error !== null && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function PredicateField({
  field,
  configuration,
  editable,
  onChange,
}: {
  readonly field: Extract<ConfigurationField, { kind: "predicate" }>;
  readonly configuration: JsonObject;
  readonly editable: boolean;
  readonly onChange: (patch: JsonObject) => void;
}): React.JSX.Element {
  const predicate = objectValue(configuration[field.key]);
  const op = typeof predicate.op === "string" ? predicate.op : "always";
  const path = typeof predicate.path === "string" ? predicate.path : "";
  const value = predicate.value === undefined ? "" : String(predicate.value);
  const needsPath = op === "exists" || op === "eq" || op === "neq";
  const needsValue = op === "eq" || op === "neq";
  const update = (patch: Record<string, JsonValue>) =>
    onChange({ [field.key]: { op, ...patch } });
  return (
    <fieldset className="predicate-field">
      <legend>{field.label}</legend>
      <label>
        Operator
        <select
          disabled={!editable}
          title="Choose the predicate operator over the incoming value"
          value={op}
          onChange={(event) => onChange({ [field.key]: { op: event.target.value } })}
        >
          {["always", "exists", "eq", "neq", "and", "or", "not"].map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
      </label>
      {needsPath && (
        <label>
          Path
          <input
            disabled={!editable}
            placeholder="$.result"
            title="JSON path the predicate inspects"
            value={path}
            onChange={(event) =>
              update({ path: event.target.value || null })
            }
          />
        </label>
      )}
      {needsValue && (
        <label>
          Value
          <input
            disabled={!editable}
            title="Value the predicate compares against"
            value={value}
            onChange={(event) => update({ value: event.target.value })}
          />
        </label>
      )}
    </fieldset>
  );
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value : "";
}
function numberValue(value: unknown): number | string {
  return typeof value === "number" && Number.isFinite(value) ? value : "";
}
function objectValue(value: unknown): JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as JsonObject)
    : {};
}
function stringArrayValue(value: unknown): readonly string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string")
    ? value
    : [];
}
