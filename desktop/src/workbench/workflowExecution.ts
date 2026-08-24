import { stableIdSchema } from "../protocol/schema";
import type { JsonObject, WorkflowDocument } from "./workflow";

export interface WorkflowExecutionIssue {
  readonly code:
    | "native_schema"
    | "native_identity"
    | "native_persistence_bound"
    | "native_node_set"
    | "native_transition_set"
    | "native_transition_identity"
    | "native_node_configuration"
    | "native_model_tier"
    | "native_agent_tools"
    | "native_agent_turns"
    | "native_project_scope";
  readonly message: string;
}

export interface WorkflowExecutionCompatibility {
  readonly executable: boolean;
  readonly issues: readonly WorkflowExecutionIssue[];
}

const REQUIRED_NODES = [
  ["input.1", "input"],
  ["agent.1", "agent"],
  ["output.1", "output"],
  ["wait.1", "wait"],
] as const;

const REQUIRED_TRANSITIONS = [
  ["input.1", "agent.1"],
  ["agent.1", "output.1"],
  ["output.1", "wait.1"],
] as const;

/** Must remain equal to the native frozen-workflow persistence contract. */
export const MAXIMUM_EXECUTABLE_WORKFLOW_BYTES = 128 * 1024;

/** Reports whether the saved Simple Chat Agent binds either implemented
 * project-scoped tool. Invalid/unsupported bindings remain a separate native
 * compatibility error; this predicate exists for selection-aware composer UX. */
export function simpleChatBindsProjectTools(
  document: WorkflowDocument,
): boolean {
  const agent = document.nodes.find((node) => node.id === "agent.1");
  const configuration = agent?.configuration;
  if (
    typeof configuration !== "object" ||
    configuration === null ||
    Array.isArray(configuration)
  )
    return false;
  const toolIds = (configuration as JsonObject).toolIds;
  return (
    Array.isArray(toolIds) &&
    toolIds.some(
      (toolId) =>
        toolId === "tool.files.read" || toolId === "tool.files.search",
    )
  );
}

/** Mirrors the deliberately narrow native runtime validator without claiming
 * that arbitrary visual-editor nodes have an executor. */
export function assessNativeWorkflow(
  document: WorkflowDocument,
  context?: { readonly projectScoped?: boolean },
): WorkflowExecutionCompatibility {
  const issues: WorkflowExecutionIssue[] = [];
  if (document.schemaVersion !== 1)
    issues.push({
      code: "native_schema",
      message:
        "Native execution supports workflow schemaVersion 1 only; this document remains losslessly inspectable and exportable.",
    });
  if (document.id !== "workflow.simple-chat")
    issues.push({
      code: "native_identity",
      message:
        "Native Simple Chat execution requires the canonical workflow.simple-chat document identity.",
    });
  if (
    new TextEncoder().encode(JSON.stringify(document)).length >
    MAXIMUM_EXECUTABLE_WORKFLOW_BYTES
  )
    issues.push({
      code: "native_persistence_bound",
      message:
        "This workflow exceeds the 128 KiB frozen execution bound. Remove oversized preserved metadata before running it; the complete document remains editable and exportable.",
    });
  const exactNodes =
    document.nodes.length === REQUIRED_NODES.length &&
    REQUIRED_NODES.every(
      ([id, type]) =>
        document.nodes.filter(
          (node) => node.id === id && node.type === type,
        ).length === 1,
    );
  if (!exactNodes)
    issues.push({
      code: "native_node_set",
      message:
        "Native execution currently requires exactly Input → Agent → Output → Wait for Input with the built-in node IDs.",
    });

  const exactTransitions =
    document.edges.length === REQUIRED_TRANSITIONS.length &&
    REQUIRED_TRANSITIONS.every(
      ([source, target]) =>
        document.edges.filter(
          (edge) => edge.source === source && edge.target === target,
        ).length === 1,
    );
  if (!exactTransitions)
    issues.push({
      code: "native_transition_set",
      message:
        "Native execution currently requires the three Simple Chat transitions with no additional transitions.",
    });
  if (
    document.edges.some(
      (edge) => !stableIdSchema.safeParse(edge.id).success,
    )
  )
    issues.push({
      code: "native_transition_identity",
      message:
        "Every Simple Chat transition ID must be a StableId of 1–128 ASCII letters, digits, periods, underscores, or hyphens.",
    });

  const agent = document.nodes.find((node) => node.id === "agent.1");
  if (!hasExactExecutableNodeConfiguration(document, agent))
    issues.push({
      code: "native_node_configuration",
      message:
        "Input, Output, and Wait configuration must be empty; Agent accepts only modelTierId, toolIds, maxTurns, and optional non-empty instructions up to 64 KiB.",
    });
  if (modelTierId(agent) !== "tier:balanced")
    issues.push({
      code: "native_model_tier",
      message:
        "The Simple Chat Agent must reference the portable tier:balanced model tier.",
    });
  const toolIds = nativeToolBindings(agent);
  if (toolIds === null)
    issues.push({
      code: "native_agent_tools",
      message:
        "Native Simple Chat accepts only unique tool.files.read/tool.files.search bindings; edit, shell, and Python require an approval path.",
    });
  const maximumTurns = nativeMaximumTurns(agent);
  if (
    toolIds !== null &&
    ((toolIds.length === 0 && maximumTurns !== 1) ||
      (toolIds.length > 0 &&
        (maximumTurns === null || maximumTurns < 2 || maximumTurns > 8)))
  )
    issues.push({
      code: "native_agent_turns",
      message:
        "Use maxTurns=1 without tools or maxTurns=2..8 with project file read/search.",
    });
  if (
    toolIds !== null &&
    toolIds.length > 0 &&
    context?.projectScoped === false
  )
    issues.push({
      code: "native_project_scope",
      message:
        "Project file read/search requires a saved project selection when the Chat starts.",
    });

  return { executable: issues.length === 0, issues };
}

function hasExactExecutableNodeConfiguration(
  document: WorkflowDocument,
  agent: JsonObject | undefined,
): boolean {
  for (const id of ["input.1", "output.1", "wait.1"]) {
    const configuration = document.nodes.find((node) => node.id === id)
      ?.configuration;
    if (configuration !== undefined) {
      if (
        typeof configuration !== "object" ||
        configuration === null ||
        Array.isArray(configuration) ||
        Object.keys(configuration).length > 0
      )
        return false;
    }
  }
  const configuration = agent?.configuration;
  if (
    typeof configuration !== "object" ||
    configuration === null ||
    Array.isArray(configuration)
  )
    return false;
  const keys = Object.keys(configuration).sort();
  const allowed = ["instructions", "maxTurns", "modelTierId", "toolIds"];
  const required = ["maxTurns", "modelTierId", "toolIds"];
  if (
    !required.every((key) => keys.includes(key)) ||
    keys.some((key) => !allowed.includes(key))
  )
    return false;
  const instructions = (configuration as JsonObject).instructions;
  return (
    instructions === undefined ||
    (typeof instructions === "string" &&
      instructions.trim().length > 0 &&
      !instructions.includes("\0") &&
      new TextEncoder().encode(instructions).length <= 64 * 1024)
  );
}

function nativeToolBindings(
  node: JsonObject | undefined,
): readonly string[] | null {
  const configuration = node?.configuration;
  if (
    typeof configuration !== "object" ||
    configuration === null ||
    Array.isArray(configuration)
  )
    return null;
  const toolIds = (configuration as JsonObject).toolIds;
  if (!Array.isArray(toolIds) || !toolIds.every((id) => typeof id === "string"))
    return null;
  const supported = new Set(["tool.files.read", "tool.files.search"]);
  const unique = new Set(toolIds);
  return unique.size === toolIds.length && toolIds.every((id) => supported.has(id))
    ? toolIds
    : null;
}

function nativeMaximumTurns(node: JsonObject | undefined): number | null {
  const configuration = node?.configuration;
  if (
    typeof configuration !== "object" ||
    configuration === null ||
    Array.isArray(configuration)
  )
    return null;
  const value = (configuration as JsonObject).maxTurns;
  return typeof value === "number" && Number.isInteger(value) ? value : null;
}

function modelTierId(node: JsonObject | undefined): unknown {
  const configuration = node?.configuration;
  if (
    typeof configuration !== "object" ||
    configuration === null ||
    Array.isArray(configuration)
  )
    return undefined;
  return (configuration as JsonObject).modelTierId;
}
