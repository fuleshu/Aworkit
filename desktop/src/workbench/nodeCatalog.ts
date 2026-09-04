/**
 * Typed V1 executable node catalog. The workflow document stays a lossless
 * JSON object; this module is the editor's read-only interpretation of the ten
 * supported node types, their typed ports, and the property forms that edit
 * their `configuration` objects. Unknown node types fall back to the preserved
 * raw-JSON editor and are never rejected or rewritten by this catalog.
 */
import type { JsonValue } from "./workflow";

/** Port data kind used for connection validation. */
export type PortKind = "text" | "flow" | "route";

export interface CatalogPort {
  readonly id: string;
  readonly kind: PortKind;
  readonly label: string;
}

/**
 * A declarative configuration field. `modelTier`, `toolMulti`, and `toolSingle`
 * are resolved against the live Settings snapshot at render time; the rest are
 * self-contained inputs. The key names match the canonical `configuration`
 * object written to the workflow document.
 */
export type ConfigurationField =
  | {
      readonly kind: "modelTier";
      readonly key: string;
      readonly label: string;
    }
  | {
      readonly kind: "toolMulti";
      readonly key: string;
      readonly label: string;
    }
  | {
      readonly kind: "toolSingle";
      readonly key: string;
      readonly label: string;
    }
  | {
      readonly kind: "reasoningEffort";
      readonly key: string;
      readonly label: string;
    }
  | {
      readonly kind: "thinkingToggle";
      readonly key: string;
      readonly label: string;
    }
  | {
      readonly kind: "number";
      readonly key: string;
      readonly label: string;
      readonly min: number;
      readonly max: number;
      readonly step?: number;
      readonly defaultValue?: number;
    }
  | {
      readonly kind: "textarea";
      readonly key: string;
      readonly label: string;
      readonly placeholder?: string;
    }
  | {
      readonly kind: "text";
      readonly key: string;
      readonly label: string;
    }
  | {
      readonly kind: "json";
      readonly key: string;
      readonly label: string;
    }
  | {
      readonly kind: "predicate";
      readonly key: string;
      readonly label: string;
    };

export interface NodeCatalogEntry {
  readonly type: string;
  readonly label: string;
  readonly icon: string;
  readonly description: string;
  readonly inputPorts: readonly CatalogPort[];
  readonly outputPorts: readonly CatalogPort[];
  readonly fields: readonly ConfigurationField[];
  readonly defaultConfiguration: Readonly<Record<string, JsonValue>>;
}

const textPort = (id: string, label: string): CatalogPort => ({
  id,
  kind: "text",
  label,
});
const flowPort = (id: string, label: string): CatalogPort => ({
  id,
  kind: "flow",
  label,
});
const routePort = (id: string, label: string): CatalogPort => ({
  id,
  kind: "route",
  label,
});

/** The ten V1 node types in palette order. */
export const NODE_CATALOG: readonly NodeCatalogEntry[] = [
  {
    type: "input",
    label: "Chat Input",
    icon: "→",
    description: "Entry node passing the latest user text.",
    inputPorts: [],
    outputPorts: [textPort("out", "User text")],
    fields: [],
    defaultConfiguration: {},
  },
  {
    type: "model_call",
    label: "Model Call",
    icon: "AI",
    description: "One tool-free completion; output feeds downstream.",
    inputPorts: [textPort("in", "Prompt")],
    outputPorts: [textPort("out", "Completion")],
    fields: [
      { kind: "modelTier", key: "modelTierId", label: "Model tier" },
      { kind: "reasoningEffort", key: "reasoningEffort", label: "Reasoning effort" },
      { kind: "thinkingToggle", key: "enableThinking", label: "Thinking" },
      { kind: "textarea", key: "instructions", label: "Instructions" },
      { kind: "text", key: "outputContract", label: "Output contract" },
      { kind: "number", key: "maximumTokens", label: "Maximum tokens", min: 1, max: 262_144, step: 1 },
    ],
    defaultConfiguration: { modelTierId: "tier:balanced" },
  },
  {
    type: "agent",
    label: "Agent",
    icon: "AI",
    description: "Model/tool loop that runs until the model returns a final answer.",
    inputPorts: [textPort("in", "Prompt")],
    outputPorts: [textPort("out", "Result")],
    fields: [
      { kind: "modelTier", key: "modelTierId", label: "Model tier" },
      { kind: "reasoningEffort", key: "reasoningEffort", label: "Reasoning effort" },
      { kind: "thinkingToggle", key: "enableThinking", label: "Thinking" },
      { kind: "toolMulti", key: "toolIds", label: "Tools" },
      { kind: "textarea", key: "instructions", label: "Instructions" },
    ],
    defaultConfiguration: {
      modelTierId: "tier:balanced",
      toolIds: [],
    },
  },
  {
    type: "tool",
    label: "Tool",
    icon: ">_",
    description: "One settled capability invocation.",
    inputPorts: [textPort("in", "Input")],
    outputPorts: [textPort("out", "Result")],
    fields: [
      { kind: "toolSingle", key: "toolId", label: "Tool" },
      { kind: "json", key: "parameters", label: "Parameters (JSON)" },
    ],
    defaultConfiguration: { toolId: "", parameters: {} },
  },
  {
    type: "condition",
    label: "Condition",
    icon: "◇",
    description: "Routes true/false over the incoming value.",
    inputPorts: [textPort("in", "Value")],
    outputPorts: [routePort("true", "True"), routePort("false", "False")],
    fields: [{ kind: "predicate", key: "predicate", label: "Predicate" }],
    defaultConfiguration: { predicate: { op: "always" } },
  },
  {
    type: "parallel",
    label: "Parallel",
    icon: "⋈",
    description: "Fork marker; every successor runs.",
    inputPorts: [flowPort("in", "Trigger")],
    outputPorts: [flowPort("out", "Branch")],
    fields: [],
    defaultConfiguration: {},
  },
  {
    type: "approval",
    label: "Approval",
    icon: "✓",
    description: "Suspends the Run for an explicit user decision.",
    inputPorts: [flowPort("in", "Trigger")],
    outputPorts: [flowPort("out", "Approved")],
    fields: [
      { kind: "text", key: "title", label: "Title" },
      { kind: "textarea", key: "message", label: "Message" },
    ],
    defaultConfiguration: {},
  },
  {
    type: "output",
    label: "Chat Output",
    icon: "←",
    description: "Collects the final assistant text into the timeline.",
    inputPorts: [textPort("in", "Text")],
    outputPorts: [textPort("out", "Text")],
    fields: [],
    defaultConfiguration: {},
  },
  {
    type: "wait",
    label: "Wait for Input",
    icon: "…",
    description: "Ends the pass; the Chat waits for the next input.",
    inputPorts: [textPort("in", "Text")],
    outputPorts: [],
    fields: [],
    defaultConfiguration: {},
  },
  {
    type: "completion",
    label: "Completion",
    icon: "■",
    description: "Terminal marker.",
    inputPorts: [flowPort("in", "Trigger")],
    outputPorts: [],
    fields: [],
    defaultConfiguration: {},
  },
];

/** Exact catalog node-type names; the palette and forms iterate these. */
export const CATALOG_NODE_TYPES = NODE_CATALOG.map(({ type }) => type);

export function isCatalogNodeType(type: string): boolean {
  return NODE_CATALOG.some((entry) => entry.type === type);
}

export function catalogEntryForType(type: string): NodeCatalogEntry | undefined {
  return NODE_CATALOG.find((entry) => entry.type === type);
}

/** Typed input-port handles for a node type; unknown types expose one generic port. */
export function catalogInputPorts(type: string): readonly CatalogPort[] {
  const entry = catalogEntryForType(type);
  return entry === undefined ? [textPort("in", "Input")] : entry.inputPorts;
}

/** Typed output-port handles for a node type; unknown types expose one generic port. */
export function catalogOutputPorts(type: string): readonly CatalogPort[] {
  const entry = catalogEntryForType(type);
  return entry === undefined ? [textPort("out", "Output")] : entry.outputPorts;
}

/**
 * Connection validation: a source output port may drive a target input port
 * only when their kinds are compatible. `flow` is a control trigger accepted by
 * gates and emittable by any node; `route` is a condition branch that only
 * feeds control (flow) targets; `text` carries text between text nodes.
 */
export function portKindsConnect(source: PortKind, target: PortKind): boolean {
  if (source === target) return true;
  if (target === "flow") return true;
  return false;
}

/**
 * Resolves the kind of an output-port handle for a node type. Returns
 * `undefined` for untyped (unknown) node types, `"unknown-port"` when the type
 * is known but has no such output handle, or the port kind when resolved.
 */
export function resolveOutputPortKind(
  type: string | undefined,
  portId: string,
): PortKind | "unknown-port" | undefined {
  if (type === undefined || !isCatalogNodeType(type)) return undefined;
  const port = catalogOutputPorts(type).find(({ id }) => id === portId);
  return port === undefined ? "unknown-port" : port.kind;
}

/** See `resolveOutputPortKind`; resolves an input-port handle kind. */
export function resolveInputPortKind(
  type: string | undefined,
  portId: string,
): PortKind | "unknown-port" | undefined {
  if (type === undefined || !isCatalogNodeType(type)) return undefined;
  const port = catalogInputPorts(type).find(({ id }) => id === portId);
  return port === undefined ? "unknown-port" : port.kind;
}
