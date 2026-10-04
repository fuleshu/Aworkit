import { useEffect, useState } from "react";
import type {
  ComfyUiConfiguration,
  ComfyUiParameterKind,
  ComfyUiToolParameter,
  ComfyUiWorkflowTool,
} from "../configuration";
import type {
  ComfyUiAutocreateResult,
  ComfyUiInspectResult,
  ComfyUiProbeResult,
  ComfyUiStartResult,
} from "../settingsV2Port";

type ComfyUiSectionProps = {
  readonly value: ComfyUiConfiguration;
  readonly onChange: (value: ComfyUiConfiguration) => void;
  readonly onProbe: (value: ComfyUiConfiguration) => Promise<ComfyUiProbeResult>;
  readonly onStart: (value: ComfyUiConfiguration) => Promise<ComfyUiStartResult>;
  readonly onInspect: (
    tool: ComfyUiWorkflowTool,
    endpoint: string,
  ) => Promise<ComfyUiInspectResult>;
  readonly onAutocreate: (
    tool: ComfyUiWorkflowTool,
    endpoint: string,
  ) => Promise<ComfyUiAutocreateResult>;
  readonly onPickWorkflow: () => Promise<string | null>;
};

const PARAMETER_KINDS: readonly ComfyUiParameterKind[] = [
  "string",
  "integer",
  "number",
  "boolean",
];

/** Controlled editor for the persisted ComfyUI section and its native diagnostics. */
export function ComfyUiSection({
  value,
  onChange,
  onProbe,
  onStart,
  onInspect,
  onAutocreate,
  onPickWorkflow,
}: ComfyUiSectionProps): React.JSX.Element {
  const [launchArguments, setLaunchArguments] = useState(
    value.launchArguments.join(" "),
  );
  const [probeResult, setProbeResult] = useState<ComfyUiProbeResult | null>(null);
  const [startResult, setStartResult] = useState<ComfyUiStartResult | null>(null);
  const [inspectResults, setInspectResults] = useState<
    Readonly<Record<string, ComfyUiInspectResult>>
  >({});
  const [autocreateResults, setAutocreateResults] = useState<
    Readonly<Record<string, ComfyUiAutocreateResult>>
  >({});
  const [autocreating, setAutocreating] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const canonicalLaunchArguments = value.launchArguments.join(" ");
  useEffect(() => {
    setLaunchArguments(canonicalLaunchArguments);
  }, [canonicalLaunchArguments]);

  const replaceTool = (index: number, tool: ComfyUiWorkflowTool) => {
    onChange({
      ...value,
      workflowTools: replaceAt(value.workflowTools, index, tool),
    });
  };

  const addTool = () => {
    const name = "Workflow tool";
    onChange({
      ...value,
      workflowTools: [
        ...value.workflowTools,
        {
          id: toolIdFromName(name, value.workflowTools, value.workflowTools.length),
          name,
          description: "",
          workflowPath: "",
          enabled: true,
          parameters: [],
        },
      ],
    });
  };

  const run = async <Result,>(operation: () => Promise<Result>) => {
    setError(null);
    try {
      return await operation();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      return null;
    }
  };

  return (
    <div className="settings-section-stack">
      <h3 id="settings-comfyui-heading">Server and local launch</h3>
      <label className="settings-field" htmlFor="comfyui-endpoint">
        Endpoint
        <input
          id="comfyui-endpoint"
          title="HTTP or HTTPS base URL of the ComfyUI server, without credentials, query, or fragment"
          type="url"
          value={value.endpoint}
          onChange={(event) => onChange({ ...value, endpoint: event.target.value })}
        />
      </label>
      <div className="section-actions">
        <button
          title="Test whether the current ComfyUI endpoint is reachable"
          type="button"
          onClick={() =>
            void run(() => onProbe(value)).then((result) => {
              if (result !== null) setProbeResult(result);
            })
          }
        >
          Test connection
        </button>
      </div>
      {probeResult !== null && (
        <div className="settings-field-help" role="status">
          <strong>{probeResult.ok ? "Reachable" : "Not reachable"}</strong>
          <span>{probeResult.message}</span>
          <span>Version: {probeResult.version ?? "not reported"}</span>
          <span>Latency: {probeResult.latencyMillis} ms</span>
        </div>
      )}

      <label className="settings-field" htmlFor="comfyui-install-path">
        Install path
        <input
          id="comfyui-install-path"
          title="Local ComfyUI installation folder used when starting the server"
          type="text"
          value={value.installPath ?? ""}
          onChange={(event) =>
            onChange({
              ...value,
              installPath: emptyAsNull(event.target.value),
            })
          }
        />
      </label>
      <label className="settings-field" htmlFor="comfyui-launch-arguments">
        Launch arguments
        <input
          id="comfyui-launch-arguments"
          title="Shell-free, space-separated arguments passed directly to the ComfyUI launcher"
          type="text"
          value={launchArguments}
          onChange={(event) => {
            const source = event.target.value;
            setLaunchArguments(source);
            onChange({ ...value, launchArguments: splitArguments(source) });
          }}
        />
      </label>
      <label className="settings-checkbox" htmlFor="comfyui-auto-start">
        <input
          checked={value.autoStart}
          id="comfyui-auto-start"
          title="Allow Aworkit to start this local ComfyUI installation when needed"
          type="checkbox"
          onChange={(event) => onChange({ ...value, autoStart: event.target.checked })}
        />
        Auto-start local ComfyUI
      </label>
      <div className="section-actions">
        <button
          disabled={(value.installPath ?? "").trim() === ""}
          title="Start ComfyUI from the configured local installation and wait for its endpoint"
          type="button"
          onClick={() =>
            void run(() => onStart(value)).then((result) => {
              if (result !== null) setStartResult(result);
            })
          }
        >
          Start ComfyUI
        </button>
      </div>
      {startResult !== null && (
        <div className="settings-field-help" role="status">
          <strong>{startResult.ok ? "ComfyUI is ready" : "ComfyUI is not ready"}</strong>
          <span>{startResult.message}</span>
          {startResult.processId !== null && <span>Process: {startResult.processId}</span>}
          {startResult.logPath !== "" && <span>Log: {startResult.logPath}</span>}
          {!startResult.ok && startResult.outputTail.trim() !== "" && (
            <pre>{startResult.outputTail.slice(-4_000)}</pre>
          )}
        </div>
      )}

      <label className="settings-field" htmlFor="comfyui-workflow-folder">
        Workflow folder
        <input
          id="comfyui-workflow-folder"
          title="Folder the workflow authoring assistant is allowed to read from and write to"
          type="text"
          value={value.workflowFolder ?? ""}
          onChange={(event) =>
            onChange({
              ...value,
              workflowFolder: emptyAsNull(event.target.value),
            })
          }
        />
      </label>

      <h3>Workflow tools</h3>
      <p className="settings-field-help">
        Every enabled workflow tool is a native agent tool. Save this section,
        then tick it by name in the Agent node's Tools list. No bridge server,
        interpreter or registration step is involved.
      </p>
      {value.workflowTools.length === 0 ? (
        <p className="settings-empty">No workflow tools configured.</p>
      ) : (
        value.workflowTools.map((tool, toolIndex) => {
          const resultKey = String(toolIndex);
          const inspection = inspectResults[resultKey] ?? null;
          const proposal = autocreateResults[resultKey] ?? null;
          return (
            <fieldset className="settings-section-stack" key={toolIndex}>
              <legend>{tool.name || tool.id || `Workflow tool ${toolIndex + 1}`}</legend>
              <label className="settings-field" htmlFor={`comfyui-tool-${toolIndex}-path`}>
                Workflow path
                <input
                  id={`comfyui-tool-${toolIndex}-path`}
                  title="Absolute path to an API-format ComfyUI workflow JSON file"
                  type="text"
                  value={tool.workflowPath}
                  onChange={(event) =>
                    replaceTool(toolIndex, { ...tool, workflowPath: event.target.value })
                  }
                />
              </label>
              <div className="section-actions">
                <button
                  title="Choose an API-format ComfyUI workflow JSON file"
                  type="button"
                  onClick={() =>
                    void run(onPickWorkflow).then((path) => {
                      if (path !== null) replaceTool(toolIndex, { ...tool, workflowPath: path });
                    })
                  }
                >
                  Choose workflow JSON
                </button>
                <button
                  disabled={tool.workflowPath.trim() === ""}
                  title="Inspect the workflow JSON and list its editable inputs"
                  type="button"
                  onClick={() =>
                    void run(() => onInspect(tool, value.endpoint)).then((result) => {
                      if (result !== null)
                        setInspectResults((current) => ({ ...current, [resultKey]: result }));
                    })
                  }
                >
                  Inspect workflow
                </button>
                <button
                  disabled={tool.workflowPath.trim() === "" || autocreating === resultKey}
                  title="Ask the model configured for the Balanced tier to read this workflow and fill the name, description and parameter bindings"
                  type="button"
                  onClick={() => {
                    setAutocreating(resultKey);
                    void run(() => onAutocreate(tool, value.endpoint)).then((result) => {
                      setAutocreating(null);
                      if (result === null) return;
                      setAutocreateResults((current) => ({ ...current, [resultKey]: result }));
                      if (!result.ok) return;
                      const name = result.tool.name;
                      replaceTool(toolIndex, {
                        ...tool,
                        name,
                        id: toolIdFromName(name, value.workflowTools, toolIndex),
                        description: result.tool.description,
                        parameters: result.tool.parameters,
                      });
                    });
                  }}
                >
                  {autocreating === resultKey
                    ? "Creating…"
                    : "Auto create name, description and parameters"}
                </button>
              </div>
              <p className="settings-field-help">
                Auto create reads this workflow and asks the model configured for
                the Balanced tier to fill three things at once: the name, the
                description and the parameter bindings. It replaces the current
                name, description and parameters, so review them before saving.
              </p>
              {autocreating === resultKey && (
                <p className="settings-field-help" role="status">
                  Reading the workflow and asking the model… this can take a
                  moment.
                </p>
              )}
              {inspection !== null && (
                <div className="settings-field-help" role="status">
                  <strong>{inspection.inspection.nodeCount} workflow nodes</strong>
                  <span>{inspection.message}</span>
                  {inspection.inspection.inputs.length === 0 ? (
                    <span>No editable inputs found.</span>
                  ) : (
                    <ul>
                      {inspection.inspection.inputs.map((input) => (
                        <li key={`${input.nodeId}:${input.inputName}`}>
                          {input.title ?? input.classType}: {input.nodeId}.{input.inputName} ({input.valueKind})
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              )}
              {proposal !== null && (
                <div className="settings-field-help" role="status">
                  <strong>
                    {proposal.ok
                      ? "Applied to name, description and parameters"
                      : "No proposal applied"}
                  </strong>
                  <span>{proposal.message}</span>
                  {proposal.ok && (
                    <span>
                      Filled {proposal.tool.parameters.length} parameter(s):{" "}
                      {proposal.tool.parameters
                        .map((parameter) => parameter.name)
                        .join(", ") || "none"}
                    </span>
                  )}
                  {proposal.summary !== "" && <span>{proposal.summary}</span>}
                  <span>
                    {proposal.providerId} / {proposal.modelId} · {proposal.inputTokens} input, {proposal.outputTokens} output tokens
                  </span>
                </div>
              )}
              <label className="settings-checkbox" htmlFor={`comfyui-tool-${toolIndex}-enabled`}>
                <input
                  checked={tool.enabled}
                  id={`comfyui-tool-${toolIndex}-enabled`}
                  title="Expose this workflow to an Agent node as a native tool after Save"
                  type="checkbox"
                  onChange={(event) =>
                    replaceTool(toolIndex, { ...tool, enabled: event.target.checked })
                  }
                />
                Enabled
              </label>
              <label className="settings-field" htmlFor={`comfyui-tool-${toolIndex}-name`}>
                Name
                <input
                  id={`comfyui-tool-${toolIndex}-name`}
                  title="Human-readable workflow tool name shown in Settings and to the agent"
                  type="text"
                  value={tool.name}
                  onChange={(event) => {
                    const name = event.target.value;
                    replaceTool(toolIndex, {
                      ...tool,
                      name,
                      id: toolIdFromName(name, value.workflowTools, toolIndex),
                    });
                  }}
                />
              </label>
              <label className="settings-field" htmlFor={`comfyui-tool-${toolIndex}-description`}>
                Description
                <textarea
                  id={`comfyui-tool-${toolIndex}-description`}
                  title="Concise description of what this workflow tool creates or changes"
                  value={tool.description}
                  onChange={(event) =>
                    replaceTool(toolIndex, { ...tool, description: event.target.value })
                  }
                />
              </label>
              <h4>Parameters</h4>
              {tool.parameters.length === 0 ? (
                <p className="settings-empty">No parameters.</p>
              ) : (
                <table>
                  <thead>
                    <tr>
                      <th>Name</th>
                      <th>Type</th>
                      <th>Required</th>
                      <th>Default</th>
                      <th>Node ID</th>
                      <th>Input name</th>
                      <th>Action</th>
                    </tr>
                  </thead>
                  <tbody>
                    {tool.parameters.map((parameter, parameterIndex) => {
                      const bound =
                        inspection === null ||
                        inspection.inspection.inputs.some(
                          (input) =>
                            input.nodeId === parameter.nodeId &&
                            input.inputName === parameter.inputName,
                        );
                      return (
                        <tr key={`${parameterIndex}:${parameter.name}`}>
                          <td>
                            <input
                              aria-label={`Parameter ${parameterIndex + 1} name`}
                              title="Agent-facing parameter name: start with a letter or underscore and use only letters, numbers, or underscores"
                              type="text"
                              value={parameter.name}
                              onChange={(event) =>
                                replaceParameter(
                                  tool,
                                  toolIndex,
                                  parameterIndex,
                                  { ...parameter, name: event.target.value },
                                  replaceTool,
                                )
                              }
                            />
                          </td>
                          <td>
                            <select
                              aria-label={`Parameter ${parameterIndex + 1} type`}
                              title="JSON value type accepted by this workflow input"
                              value={parameter.valueKind}
                              onChange={(event) =>
                                replaceParameter(
                                  tool,
                                  toolIndex,
                                  parameterIndex,
                                  {
                                    ...parameter,
                                    valueKind: event.target.value as ComfyUiParameterKind,
                                  },
                                  replaceTool,
                                )
                              }
                            >
                              {PARAMETER_KINDS.map((kind) => (
                                <option key={kind} value={kind}>{kind}</option>
                              ))}
                            </select>
                          </td>
                          <td>
                            <input
                              aria-label={`Parameter ${parameterIndex + 1} required`}
                              checked={parameter.required}
                              title="Require the agent to provide this parameter"
                              type="checkbox"
                              onChange={(event) =>
                                replaceParameter(
                                  tool,
                                  toolIndex,
                                  parameterIndex,
                                  { ...parameter, required: event.target.checked },
                                  replaceTool,
                                )
                              }
                            />
                          </td>
                          <td>
                            <JsonValueField
                              id={`comfyui-tool-${toolIndex}-parameter-${parameterIndex}-default`}
                              label={`Parameter ${parameterIndex + 1} default`}
                              title="JSON default value, or null when there is no default"
                              value={parameter.defaultValue}
                              onChange={(defaultValue) =>
                                replaceParameter(
                                  tool,
                                  toolIndex,
                                  parameterIndex,
                                  { ...parameter, defaultValue },
                                  replaceTool,
                                )
                              }
                            />
                          </td>
                          <td>
                            <input
                              aria-label={`Parameter ${parameterIndex + 1} node ID`}
                              title="ComfyUI workflow node ID that receives this parameter"
                              type="text"
                              value={parameter.nodeId}
                              onChange={(event) =>
                                replaceParameter(
                                  tool,
                                  toolIndex,
                                  parameterIndex,
                                  { ...parameter, nodeId: event.target.value },
                                  replaceTool,
                                )
                              }
                            />
                          </td>
                          <td>
                            <input
                              aria-label={`Parameter ${parameterIndex + 1} input name`}
                              title="Input name inside the selected ComfyUI workflow node"
                              type="text"
                              value={parameter.inputName}
                              onChange={(event) =>
                                replaceParameter(
                                  tool,
                                  toolIndex,
                                  parameterIndex,
                                  { ...parameter, inputName: event.target.value },
                                  replaceTool,
                                )
                              }
                            />
                            {!bound && <small className="field-error">Binding not found</small>}
                          </td>
                          <td>
                            <button
                              aria-label={`Remove parameter ${parameterIndex + 1}`}
                              className="danger-action"
                              title="Remove this workflow parameter"
                              type="button"
                              onClick={() =>
                                replaceTool(toolIndex, {
                                  ...tool,
                                  parameters: tool.parameters.filter(
                                    (_, index) => index !== parameterIndex,
                                  ),
                                })
                              }
                            >
                              Remove
                            </button>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              )}
              <div className="section-actions">
                <button
                  title="Add a typed parameter binding to this workflow tool"
                  type="button"
                  onClick={() =>
                    replaceTool(toolIndex, {
                      ...tool,
                      parameters: [...tool.parameters, newParameter(tool.parameters.length)],
                    })
                  }
                >
                  Add parameter
                </button>
                <button
                  className="danger-action"
                  title="Remove this workflow tool from the Settings draft"
                  type="button"
                  onClick={() => {
                    // Per-tool results are keyed by position, so drop them
                    // rather than let a shifted tool inherit them.
                    setInspectResults({});
                    setAutocreateResults({});
                    onChange({
                      ...value,
                      workflowTools: value.workflowTools.filter(
                        (_, index) => index !== toolIndex,
                      ),
                    });
                  }}
                >
                  Remove
                </button>
              </div>
            </fieldset>
          );
        })
      )}
      <p className="settings-field-help" role="status">
        {nativeToolLabel(value)}
      </p>
      <div className="section-actions">
        <button
          title="Add another ComfyUI API workflow as an agent tool"
          type="button"
          onClick={addTool}
        >
          Add workflow tool
        </button>
      </div>
      {error !== null && <p className="field-error" role="alert">{error}</p>}
    </div>
  );
}

function replaceParameter(
  tool: ComfyUiWorkflowTool,
  toolIndex: number,
  parameterIndex: number,
  parameter: ComfyUiToolParameter,
  replaceTool: (index: number, tool: ComfyUiWorkflowTool) => void,
): void {
  replaceTool(toolIndex, {
    ...tool,
    parameters: replaceAt(tool.parameters, parameterIndex, parameter),
  });
}

function newParameter(index: number): ComfyUiToolParameter {
  return {
    name: `parameter_${index + 1}`,
    description: "",
    valueKind: "string",
    required: false,
    defaultValue: null,
    nodeId: "",
    inputName: "",
    choices: [],
  };
}

/// Says exactly what an Agent node offers from this section once it is saved.
function nativeToolLabel(comfyui: ComfyUiConfiguration): string {
  const enabled = comfyui.workflowTools.filter(
    tool => tool.enabled && tool.id.trim() !== "",
  ).length;
  return `Agent nodes list every enabled workflow tool by name under Tools — ${enabled} workflow tool(s) enabled. Once a workflow tool exists, the ComfyUI authoring helpers are offered there too.`;
}

/// Derives the stable tool id from its display name.
///
/// The id names the bridge configuration entry and the MCP function, so it must
/// stay an identifier: lower-case, alphanumeric with dashes, unique among the
/// configured tools. The user never types it.
function toolIdFromName(
  name: string,
  tools: readonly ComfyUiWorkflowTool[],
  index: number,
): string {
  const slug = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 96);
  const base = slug === "" ? "workflow-tool" : slug;
  const taken = new Set(
    tools.filter((_, position) => position !== index).map((tool) => tool.id),
  );
  if (!taken.has(base)) return base;
  for (let suffix = 2; ; suffix += 1) {
    const candidate = `${base.slice(0, 96)}-${suffix}`;
    if (!taken.has(candidate)) return candidate;
  }
}

function emptyAsNull(value: string): string | null {
  return value === "" ? null : value;
}

function splitArguments(value: string): string[] {
  const trimmed = value.trim();
  return trimmed === "" ? [] : trimmed.split(/\s+/u);
}

function replaceAt<T>(items: readonly T[], index: number, value: T): T[] {
  return [...items.slice(0, index), value, ...items.slice(index + 1)];
}

function JsonValueField({
  id,
  label,
  title,
  value,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly value: unknown;
  readonly onChange: (value: unknown) => void;
}): React.JSX.Element {
  const canonical = JSON.stringify(value);
  const [source, setSource] = useState(canonical);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setSource(canonical);
    setError(null);
  }, [canonical]);
  return (
    <>
      <input
        aria-invalid={error === null ? undefined : true}
        aria-label={label}
        id={id}
        spellCheck={false}
        title={title}
        type="text"
        value={source}
        onChange={(event) => {
          const next = event.target.value;
          setSource(next);
          try {
            const parsed: unknown = JSON.parse(next);
            setError(null);
            onChange(parsed);
          } catch {
            setError("Enter a valid JSON value.");
          }
        }}
      />
      {error !== null && <small className="field-error">{error}</small>}
    </>
  );
}
