import { findNativeTool, selectableTools } from "./toolRegistry";
import { McpServerSelection } from "./McpServerSelection";
import { useEffect, useMemo, useState } from "react";
import type { McpServerConfiguration, SettingsV2Snapshot } from "./configuration";
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
  readonly externalAgentTools: readonly { readonly value: string; readonly label: string }[];
  readonly mcpServers: readonly { readonly value: string; readonly label: string }[];
  readonly mcpConfigurations: readonly McpServerConfiguration[];
  readonly modelCapabilitiesByTier: Readonly<Record<string, readonly string[]>>;
}

/** Executors that delegate to a configured external agent product. */
const EXTERNAL_AGENT_EXECUTORS = new Set([
  "subagent_codex",
  "subagent_claude_code",
]);

/** Reasoning efforts the Claude Code CLI accepts; Codex takes the full set. */
const CLAUDE_REASONING_EFFORTS = ["low", "medium", "high", "xhigh", "max"];

/** Whether one selected delegation tool runs the Claude Code product. */
function runsClaudeCode(toolId: unknown): boolean {
  return (
    typeof toolId === "string" &&
    findNativeTool(toolId)?.executor === "subagent_claude_code"
  );
}

function resolveFieldOptions(settings?: SettingsV2Snapshot): FieldOptions {
  if (settings === undefined)
    return {
      tiers: FALLBACK_TIERS,
      tools: [],
      externalAgentTools: [],
      mcpServers: [],
      mcpConfigurations: [],
      modelCapabilitiesByTier: {},
    };
  const tiers =
    settings.settings.modelTiers.length > 0
      ? settings.settings.modelTiers.map(({ id, name }) => ({ value: id, label: name }))
      : FALLBACK_TIERS;
  const tools = selectableTools(settings.settings);
  // Only the installed delegation tools belong in this selector: they are the
  // ones that reach a Codex or Claude Code target.
  const externalAgentTools = tools.filter(({ value }) => {
    const entry = findNativeTool(value);
    return entry !== undefined && EXTERNAL_AGENT_EXECUTORS.has(entry.executor);
  });
  const mcpServers = settings.settings.mcpServers
    .filter((server) => server.enabled)
    .map(({ id, name }) => ({ value: id, label: name }));
  const modelCapabilitiesByTier = Object.fromEntries(
    settings.settings.modelTiers.flatMap((tier) => {
      const resolution = tier.resolution;
      if (resolution.strategy !== "exact") return [];
      const provider = settings.settings.providers.find(
        ({ id }) => id === resolution.target.providerId,
      );
      const model = provider?.models.find(
        ({ id }) => id === resolution.target.modelId,
      );
      return model === undefined ? [] : [[tier.id, model.capabilities]];
    }),
  );
  return { tiers, tools, externalAgentTools, mcpServers, mcpConfigurations: settings.settings.mcpServers, modelCapabilitiesByTier };
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
    case "reasoningEffort": {
      const tier = stringValue(configuration.modelTierId);
      const capabilities = options.modelCapabilitiesByTier[tier] ?? [];
      const advertised = capabilities
        .filter((capability) => capability.startsWith("reasoning_effort:"))
        .map((capability) => capability.slice("reasoning_effort:".length));
      const current = stringValue(configuration[field.key]);
      const values = orderedReasoningEfforts(
        advertised.length > 0 ? advertised : DEFAULT_REASONING_EFFORTS,
        current,
      );
      const source = advertised.length > 0 ? "advertised by the selected model" : "not advertised by the model API; the provider may reject unsupported values";
      return (
        <label>
          {field.label}
          <select
            disabled={!editable}
            title={`Override reasoning effort for this node (${source})`}
            value={current}
            onChange={(event) =>
              onChange({ [field.key]: event.target.value || null })
            }
          >
            <option value="">Inherit model default</option>
            {values.map((value) => (
              <option key={value} value={value}>
                {reasoningEffortLabel(value)}
              </option>
            ))}
          </select>
        </label>
      );
    }
    case "thinkingToggle": {
      const tier = stringValue(configuration.modelTierId);
      const capabilities = options.modelCapabilitiesByTier[tier] ?? [];
      const advertised = capabilities.includes("thinking_toggle");
      const current = configuration[field.key];
      const value = typeof current === "boolean" ? String(current) : "";
      return (
        <label>
          {field.label}
          <select
            disabled={!editable}
            title={`Override the provider's thinking toggle for this node (${advertised ? "advertised by the selected model" : "not advertised by the model API; intended for compatible servers such as vLLM/Qwen"})`}
            value={value}
            onChange={(event) =>
              onChange({
                [field.key]:
                  event.target.value === "" ? null : event.target.value === "true",
              })
            }
          >
            <option value="">Inherit model default</option>
            <option value="true">On</option>
            <option value="false">Off</option>
          </select>
        </label>
      );
    }
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
    case "compactionOverlay":
      return (
        <CompactionOverlayField
          field={field}
          configuration={configuration}
          editable={editable}
          onChange={onChange}
        />
      );
    case "externalAgentTool":
      return (
        <ExternalAgentToolField
          configuration={configuration}
          editable={editable}
          field={field}
          options={options}
          onChange={onChange}
        />
      );
    case "externalAgentEffort":
      return (
        <ExternalAgentEffortField
          configuration={configuration}
          editable={editable}
          field={field}
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

const DEFAULT_REASONING_EFFORTS = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
] as const;

function orderedReasoningEfforts(
  advertised: readonly string[],
  current: string,
): string[] {
  const supported = new Set(advertised);
  if (current !== "") supported.add(current);
  const ordered = DEFAULT_REASONING_EFFORTS.filter((value) => supported.has(value));
  const custom = [...supported]
    .filter(
      (value) => !(DEFAULT_REASONING_EFFORTS as readonly string[]).includes(value),
    )
    .sort();
  return [...ordered, ...custom];
}

function reasoningEffortLabel(value: string): string {
  return value === "xhigh"
    ? "Extra high"
    : value.charAt(0).toUpperCase() + value.slice(1);
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

/**
 * The Agent node's compaction overlay. Each control can stay on "Inherit", so
 * the node only answers for the knobs it cares about and the frozen Chat policy
 * supplies the rest. An overlay with nothing set is written as null.
 */
function CompactionOverlayField({
  field,
  configuration,
  editable,
  onChange,
}: {
  readonly field: Extract<ConfigurationField, { kind: "compactionOverlay" }>;
  readonly configuration: JsonObject;
  readonly editable: boolean;
  readonly onChange: (patch: JsonObject) => void;
}): React.JSX.Element {
  const overlay = objectValue(configuration[field.key]);
  const set = (key: string, value: JsonValue | undefined) => {
    const next: Record<string, JsonValue> = { ...overlay };
    if (value === undefined) delete next[key];
    else next[key] = value;
    onChange({
      [field.key]: Object.keys(next).length === 0 ? null : next,
    });
  };
  const toggle = (key: string, label: string, title: string) => (
    <label key={key}>
      {label}
      <select
        disabled={!editable}
        title={title}
        value={
          overlay[key] === true ? "on" : overlay[key] === false ? "off" : ""
        }
        onChange={(event) =>
          set(
            key,
            event.target.value === ""
              ? undefined
              : event.target.value === "on",
          )
        }
      >
        <option value="">Inherit from the model settings</option>
        <option value="on">On for this Agent</option>
        <option value="off">Off for this Agent</option>
      </select>
    </label>
  );
  const amount = (
    key: string,
    label: string,
    title: string,
    minimum: number,
    maximum: number,
  ) => (
    <label key={key}>
      {label}
      <input
        disabled={!editable}
        type="number"
        min={minimum}
        max={maximum}
        step={1}
        title={title}
        value={typeof overlay[key] === "number" ? String(overlay[key]) : ""}
        onChange={(event) =>
          set(
            key,
            event.target.value === "" ? undefined : Number(event.target.value),
          )
        }
      />
    </label>
  );
  return (
    <div className="config-field-stack">
      <small className="config-help">
        Leave any control on inherit to use the model's compaction settings.
      </small>
      {toggle(
        "auto",
        "Automatic compaction",
        "Reduce this Agent node's context automatically as it approaches the model limit",
      )}
      {toggle(
        "pruneToolResults",
        "Prune large tool results first",
        "Reduce large tool outputs to their beginning and end before summarizing",
      )}
      {amount(
        "thresholdChars",
        "Tool result character threshold",
        "Prune a tool result longer than this many Unicode characters",
        1,
        4_194_304,
      )}
      {amount(
        "headChars",
        "Keep beginning characters",
        "Characters preserved from the beginning of a pruned tool result",
        0,
        1_048_576,
      )}
      {amount(
        "tailChars",
        "Keep ending characters",
        "Characters preserved from the end of a pruned tool result",
        0,
        1_048_576,
      )}
    </div>
  );
}

/**
 * Selects one installed external delegation tool. The configured product target
 * comes from Settings, so this field names the delegation, not the executable.
 */
function ExternalAgentToolField({
  field,
  configuration,
  editable,
  options,
  onChange,
}: {
  readonly field: Extract<ConfigurationField, { kind: "externalAgentTool" }>;
  readonly configuration: JsonObject;
  readonly editable: boolean;
  readonly options: FieldOptions;
  readonly onChange: (patch: JsonObject) => void;
}): React.JSX.Element {
  const current = stringValue(configuration[field.key]);
  const isListed = options.externalAgentTools.some(
    ({ value }) => value === current,
  );
  return (
    <div className="config-field-stack">
      <label>
        {field.label}
        <select
          disabled={!editable}
          title="Installed delegation tool. Its Codex or Claude Code target, permission mode and default model come from Settings."
          value={isListed ? current : ""}
          onChange={(event) => {
            if (event.target.value !== "")
              onChange({ [field.key]: event.target.value });
          }}
        >
          <option value="">
            {isListed ? "Unset" : "Select an enabled delegation tool"}
          </option>
          {options.externalAgentTools.map(({ value, label }) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
      </label>
      {!isListed && current !== null && (
        <small className="config-help">
          {current} is not an enabled delegation tool in Settings.
        </small>
      )}
    </div>
  );
}

/**
 * Reasoning effort for one delegation. The Claude Code CLI accepts fewer levels
 * than Codex, so the offered values follow the selected product; an unset value
 * inherits the target's own setting.
 */
function ExternalAgentEffortField({
  field,
  configuration,
  editable,
  onChange,
}: {
  readonly field: Extract<ConfigurationField, { kind: "externalAgentEffort" }>;
  readonly configuration: JsonObject;
  readonly editable: boolean;
  readonly onChange: (patch: JsonObject) => void;
}): React.JSX.Element {
  // An absent or empty value inherits the target's own effort.
  const stored = stringValue(configuration[field.key]);
  const current = stored === null || stored === "" ? null : stored;
  const efforts = runsClaudeCode(configuration.toolId)
    ? CLAUDE_REASONING_EFFORTS
    : [...DEFAULT_REASONING_EFFORTS];
  const offered =
    current !== null && !efforts.includes(current) ? [] : efforts;
  return (
    <label>
      {field.label}
      <select
        disabled={!editable}
        title="Reasoning effort for this delegation only. Unset inherits the target's configured effort."
        value={current ?? ""}
        onChange={(event) =>
          onChange({
            [field.key]:
              event.target.value === "" ? null : event.target.value,
          })
        }
      >
        <option value="">Inherit from target</option>
        {offered.map((effort) => (
          <option key={effort} value={effort}>
            {effort}
          </option>
        ))}
        {current !== null && !efforts.includes(current) && (
          <option value={current}>{current}</option>
        )}
      </select>
    </label>
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
  const native = options.tools.filter(({ value }) => !value.startsWith("mcp:"));
  const known = new Set(native.map(({ value }) => value));
  const extras = selected.filter((id) => !known.has(id) && !options.mcpConfigurations.some(
    server => id === `mcp:${server.id}` || id.startsWith(`mcp://${server.id}/`),
  ));
  const toggle = (toolId: string) => {
    const next = selected.includes(toolId)
      ? selected.filter((id) => id !== toolId)
      : [...selected, toolId];
    onChange({ [field.key]: next });
  };
  return (
    <fieldset className="tool-multi-field">
      <legend>{field.label}</legend>
      {native.map(({ value, label }) => (
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
      {options.mcpConfigurations.length > 0 && <>
        <p className="config-help">MCP servers</p>
        {options.mcpConfigurations.map(server => <McpServerSelection key={server.id}
          server={server} selected={selected} editable={editable}
          onChange={next => onChange({ [field.key]: next })} />)}
      </>}
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
      {options.mcpConfigurations.length > 0 && <small className="config-help">Each server includes all functions enabled in Settings → MCP. Changes apply to new Chats.</small>}
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
  // The frozen runtime evaluates `kind`; `op` is only read as a legacy alias so
  // an older document stays editable and is corrected on the next save.
  const kind =
    typeof predicate.kind === "string"
      ? predicate.kind
      : typeof predicate.op === "string"
        ? predicate.op
        : "always";
  const path = typeof predicate.path === "string" ? predicate.path : "";
  const value = predicate.value === undefined ? "" : String(predicate.value);
  const needsPath = kind === "exists" || kind === "eq" || kind === "neq";
  const needsValue = kind === "eq" || kind === "neq";
  const update = (patch: Record<string, JsonValue>) =>
    onChange({ [field.key]: { kind, ...patch } });
  return (
    <fieldset className="predicate-field">
      <legend>{field.label}</legend>
      <label>
        Operator
        <select
          disabled={!editable}
          title="Choose the predicate operator over the incoming value"
          value={kind}
          onChange={(event) => onChange({ [field.key]: { kind: event.target.value } })}
        >
          {["always", "exists", "eq", "neq"].map((name) => (
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
