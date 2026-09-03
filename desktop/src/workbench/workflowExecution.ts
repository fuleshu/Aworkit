import { stableIdSchema } from "../protocol/schema";
import type { JsonObject, WorkflowDocument } from "./workflow";

export interface WorkflowExecutionIssue {
  readonly code:
    | "native_schema"
    | "native_persistence_bound"
    | "native_node_set"
    | "native_transition_identity"
    | "native_node_configuration"
    | "native_model_tier"
    | "native_agent_tools"
    | "native_agent_turns"
    | "native_agent_timeout"
    | "native_tool_node"
    | "native_condition"
    | "native_condition_routes"
    | "native_approval"
    | "native_structure"
    | "native_project_scope";
  readonly message: string;
}

export interface WorkflowExecutionCompatibility {
  readonly executable: boolean;
  readonly issues: readonly WorkflowExecutionIssue[];
}

/** Must remain equal to the native frozen-workflow persistence contract. */
export const MAXIMUM_EXECUTABLE_WORKFLOW_BYTES = 128 * 1024;

const CATALOG_NODE_TYPES = new Set([
  "input",
  "agent",
  "model_call",
  "tool",
  "condition",
  "parallel",
  "approval",
  "output",
  "wait",
  "completion",
]);

/** Must remain equal to the native builtin_tool_binding_ids contract. */
const BUILTIN_TOOL_BINDING_IDS = new Set([
  "tool.files.read",
  "tool.files.search",
  "tool.files.list",
  "tool.files.grep",
  "tool.files.edit",
  "tool.files.write",
  "tool.shell.host",
  "tool.python.host",
  "tool.todo",
  "tool.web_search",
  "tool.web_fetch",
  "tool.subagent",
]);

const MAXIMUM_MODEL_CALL_TOKENS = 8192;
const MAXIMUM_INSTRUCTIONS_BYTES = 64 * 1024;
const PREDICATE_KINDS = new Set([
  "always",
  "exists",
  "eq",
  "neq",
  "and",
  "or",
  "not",
]);

const PROJECT_SCOPED_TOOL_IDS = [
  "tool.files.read",
  "tool.files.search",
  "tool.files.list",
  "tool.files.grep",
  "tool.files.edit",
  "tool.files.write",
];

function nodeBindsProjectTools(node: JsonObject | undefined): boolean {
  const configuration = node?.configuration;
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
        typeof toolId === "string" && PROJECT_SCOPED_TOOL_IDS.includes(toolId),
    )
  );
}

/** Reports whether any node in the document binds a project-scoped file tool. */
export function bindsProjectTools(document: WorkflowDocument): boolean {
  return document.nodes.some((node) => nodeBindsProjectTools(node));
}

/** Reports whether the selected workflow binds a project-scoped tool.
 * The seeded document is an ordinary configured workflow; this predicate only
 * exists for selection-aware composer UX. */
export function simpleChatBindsProjectTools(
  document: WorkflowDocument,
): boolean {
  const agent = document.nodes.find((node) => node.id === "agent.1");
  return nodeBindsProjectTools(agent);
}

/**
 * Mirrors the native v1 executable-catalog validator without claiming
 * authority: any saved workflow document is executable when it passes the
 * closed node catalog, per-type configuration contracts, and structural
 * rules. Workflows are plain JSON documents; none is hardwired.
 */
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
  if (
    new TextEncoder().encode(JSON.stringify(document)).length >
    MAXIMUM_EXECUTABLE_WORKFLOW_BYTES
  )
    issues.push({
      code: "native_persistence_bound",
      message:
        "This workflow exceeds the 128 KiB frozen execution bound. Remove oversized preserved metadata before running it; the complete document remains editable and exportable.",
    });

  // Closed node catalog plus per-type configuration contracts.
  const nodes = document.nodes;
  for (const node of nodes) {
    const id = nodeId(node);
    const type = nodeType(node);
    if (!CATALOG_NODE_TYPES.has(type))
      issues.push({
        code: "native_node_set",
        message: `Workflow node '${id}' has node type '${type}' with no installed executor in this build.`,
      });
    if (
      type === "input" ||
      type === "output" ||
      type === "wait" ||
      type === "completion" ||
      type === "parallel"
    ) {
      const configuration = node.configuration;
      if (
        configuration !== undefined &&
        (typeof configuration !== "object" ||
          configuration === null ||
          Array.isArray(configuration) ||
          Object.keys(configuration).length > 0)
      )
        issues.push({
          code: "native_node_configuration",
          message: `Workflow node '${id}' of type ${type} accepts no configuration.`,
        });
    }
    if (type === "agent") {
      const configuration = objectConfiguration(node);
      if (configuration === null) {
        issues.push({
          code: "native_node_configuration",
          message: `Workflow node '${id}' agent configuration accepts exactly modelTierId, toolIds, and optional timeoutSeconds, instructions, reasoningEffort, and enableThinking.`,
        });
      } else {
        const keys = new Set(Object.keys(configuration));
        const required = new Set(["modelTierId", "toolIds"]);
        const allowed = new Set([
          "enableThinking",
          "instructions",
          "modelTierId",
          "reasoningEffort",
          "timeoutSeconds",
          "toolIds",
        ]);
        if (
          ![...required].every((key) => keys.has(key)) ||
          [...keys].some((key) => !allowed.has(key))
        )
          issues.push({
            code: "native_node_configuration",
            message: `Workflow node '${id}' agent configuration accepts exactly modelTierId, toolIds, and optional timeoutSeconds, instructions, reasoningEffort, and enableThinking.`,
          });
        if (!validTierReference(configuration.modelTierId))
          issues.push({
            code: "native_model_tier",
            message: `Workflow node '${id}' agent modelTierId must reference a tier:<name> model tier.`,
          });
        const toolIds = configuration.toolIds;
        if (!Array.isArray(toolIds))
          issues.push({
            code: "native_agent_tools",
            message: `Workflow node '${id}' agent toolIds must be an array.`,
          });
        else {
          const unique = new Set(toolIds);
          for (const toolId of toolIds) {
            if (typeof toolId !== "string" || !isToolBindingId(toolId))
              issues.push({
                code: "native_agent_tools",
                message: `Workflow node '${id}' agent toolIds must reference tool.<name> or mcp:<server> bindings.`,
              });
            else if (
              !BUILTIN_TOOL_BINDING_IDS.has(toolId) &&
              !toolId.startsWith("mcp:")
            )
              issues.push({
                code: "native_agent_tools",
                message: `Workflow node '${id}' agent binds tool '${toolId}' with no installed executor in this build.`,
              });
          }
          if (unique.size !== toolIds.length)
            issues.push({
              code: "native_agent_tools",
              message: `Workflow node '${id}' agent toolIds must be unique.`,
            });
        }
        const timeoutSeconds = configuration.timeoutSeconds;
        if (
          timeoutSeconds !== undefined &&
          (typeof timeoutSeconds !== "number" ||
            !Number.isInteger(timeoutSeconds) ||
            timeoutSeconds < 30 ||
            timeoutSeconds > 3_600)
        )
          issues.push({
            code: "native_agent_timeout",
            message: `Workflow node '${id}' agent timeoutSeconds must be 30..=3600.`,
          });
        instructionsIssue(id, configuration.instructions, issues);
        modelReasoningIssues(id, configuration, issues);
      }
    }
    if (node.type === "model_call") {
      const configuration = objectConfiguration(node);
      if (configuration === null)
        issues.push({
          code: "native_node_configuration",
          message: `Workflow node '${id}' model_call configuration accepts exactly modelTierId plus optional instructions, maximumTokens, outputContract, reasoningEffort, and enableThinking.`,
        });
      else {
        const keys = new Set(Object.keys(configuration));
        const allowed = new Set([
          "enableThinking",
          "instructions",
          "maximumTokens",
          "modelTierId",
          "outputContract",
          "reasoningEffort",
        ]);
        if (!keys.has("modelTierId") || [...keys].some((key) => !allowed.has(key)))
          issues.push({
            code: "native_node_configuration",
            message: `Workflow node '${id}' model_call configuration accepts exactly modelTierId plus optional instructions, maximumTokens, outputContract, reasoningEffort, and enableThinking.`,
          });
        if (!validTierReference(configuration.modelTierId))
          issues.push({
            code: "native_model_tier",
            message: `Workflow node '${id}' model_call modelTierId must reference a tier:<name> model tier.`,
          });
        if (
          configuration.outputContract !== undefined &&
          configuration.outputContract !== "plan"
        )
          issues.push({
            code: "native_node_configuration",
            message: `Workflow node '${id}' model_call outputContract must be 'plan' when present.`,
          });
        const maximumTokens = configuration.maximumTokens;
        if (maximumTokens !== undefined) {
          if (
            typeof maximumTokens !== "number" ||
            !Number.isInteger(maximumTokens) ||
            maximumTokens < 1 ||
            maximumTokens > MAXIMUM_MODEL_CALL_TOKENS
          )
            issues.push({
              code: "native_node_configuration",
              message: `Workflow node '${id}' maximumTokens must be 1..=${MAXIMUM_MODEL_CALL_TOKENS}.`,
            });
        }
        modelReasoningIssues(id, configuration, issues);
        instructionsIssue(id, configuration.instructions, issues);
      }
    }
    if (node.type === "tool") {
      const configuration = objectConfiguration(node);
      if (configuration === null)
        issues.push({
          code: "native_tool_node",
          message: `Workflow node '${id}' tool configuration accepts exactly toolId plus optional parameters.`,
        });
      else {
        const keys = new Set(Object.keys(configuration));
        const allowed = new Set(["parameters", "toolId"]);
        if (!keys.has("toolId") || [...keys].some((key) => !allowed.has(key)))
          issues.push({
            code: "native_tool_node",
            message: `Workflow node '${id}' tool configuration accepts exactly toolId plus optional parameters.`,
          });
        const toolId = configuration.toolId;
        if (typeof toolId !== "string" || !isToolBindingId(toolId))
          issues.push({
            code: "native_tool_node",
            message: `Workflow node '${id}' tool toolId must reference a tool.<name> or mcp:<server> binding.`,
          });
        else if (
          !BUILTIN_TOOL_BINDING_IDS.has(toolId) &&
          !toolId.startsWith("mcp:")
        )
          issues.push({
            code: "native_tool_node",
            message: `Workflow node '${id}' tool binds '${toolId}' with no installed executor in this build.`,
          });
        const parameters = configuration.parameters;
        if (
          parameters !== undefined &&
          (typeof parameters !== "object" ||
            parameters === null ||
            Array.isArray(parameters))
        )
          issues.push({
            code: "native_tool_node",
            message: `Workflow node '${id}' tool parameters must be a JSON object.`,
          });
      }
    }
    if (node.type === "condition") {
      const configuration = objectConfiguration(node);
      if (configuration === null || !("predicate" in configuration))
        issues.push({
          code: "native_condition",
          message: `Workflow node '${id}' condition configuration accepts exactly a predicate object.`,
        });
      else if (!validPredicate(id, configuration.predicate, 0, issues)) {
        // The specific predicate issue was already recorded.
      }
    }
    if (node.type === "approval") {
      const configuration = objectConfiguration(node);
      if (configuration !== null) {
        const keys = Object.keys(configuration);
        const allowed = new Set(["message", "title"]);
        if (keys.some((key) => !allowed.has(key)))
          issues.push({
            code: "native_approval",
            message: `Workflow node '${id}' approval configuration accepts only title and message.`,
          });
        for (const [key, maximum] of [
          ["title", 4 * 1024],
          ["message", 16 * 1024],
        ] as const) {
          const value = configuration[key];
          if (
            value !== undefined &&
            (typeof value !== "string" ||
              value.length > maximum ||
              value.includes("\0"))
          )
            issues.push({
              code: "native_approval",
              message: `Workflow node '${id}' approval ${key} exceeds its bound.`,
            });
        }
      }
    }
    validateDeclaredPorts(id, node, issues);
  }

  const nodeIds = new Set(nodes.map(nodeId));
  const inputIds = nodes
    .filter((node) => nodeType(node) === "input")
    .map(nodeId);
  if (inputIds.length !== 1)
    issues.push({
      code: "native_structure",
      message: "An executable v1 workflow requires exactly one input node.",
    });
  const terminalIds = new Set(
    nodes
      .filter(
        (node) => nodeType(node) === "wait" || nodeType(node) === "completion",
      )
      .map(nodeId),
  );
  if (terminalIds.size === 0)
    issues.push({
      code: "native_structure",
      message: "An executable v1 workflow requires a wait or completion node.",
    });
  if (
    document.edges.some(
      (edge) => !stableIdSchema.safeParse(edge.id).success,
    )
  )
    issues.push({
      code: "native_transition_identity",
      message:
        "Every transition ID must be a StableId of 1–128 ASCII letters, digits, periods, underscores, or hyphens.",
    });

  const successors = new Map<string, string[]>();
  for (const node of nodes) successors.set(nodeId(node), []);
  for (const edge of document.edges) {
    const existing = successors.get(edgeSource(edge)) ?? [];
    existing.push(edgeTarget(edge));
    successors.set(edgeSource(edge), existing);
  }
  if (hasCycle(successors))
    issues.push({
      code: "native_structure",
      message: "An executable v1 workflow graph must be acyclic.",
    });
  if (inputIds.length === 1) {
    const entry = inputIds[0];
    const reachable = new Set<string>([entry]);
    const frontier = [entry];
    while (frontier.length > 0) {
      const id = frontier.pop();
      if (id === undefined) break;
      for (const next of successors.get(id) ?? []) {
        if (!reachable.has(next)) {
          reachable.add(next);
          frontier.push(next);
        }
      }
    }
    if (reachable.size !== nodeIds.size)
      issues.push({
        code: "native_structure",
        message:
          "An executable v1 workflow requires every node to be reachable from the input node.",
      });
  }
  for (const node of nodes) {
    if (node.type !== "condition") continue;
    const routes = new Set<string>();
    let invalidRoute = false;
    for (const edge of document.edges) {
      if (edge.source !== node.id) continue;
      const route =
        typeof edge.configuration === "object" &&
        edge.configuration !== null &&
        !Array.isArray(edge.configuration) &&
        typeof (edge.configuration as JsonObject).route === "string"
          ? ((edge.configuration as JsonObject).route as string)
          : null;
      if (route === null || !["true", "false", "fallback"].includes(route)) {
        invalidRoute = true;
        continue;
      }
      routes.add(route);
    }
    if (invalidRoute)
      issues.push({
        code: "native_condition_routes",
        message: `Transition leaving condition node '${node.id}' requires configuration.route of true, false, or fallback.`,
      });
    if (!routes.has("true") || !routes.has("false"))
      issues.push({
        code: "native_condition_routes",
        message: `Condition node '${node.id}' requires one true route and one false or fallback route.`,
      });
  }

  if (
    context?.projectScoped === false &&
    bindsProjectTools(document)
  )
    issues.push({
      code: "native_project_scope",
      message:
        "This workflow binds project file tools and requires a saved project selection when the Chat starts.",
    });

  return { executable: issues.length === 0, issues };
}

function objectConfiguration(node: JsonObject): JsonObject | null {
  const configuration = node.configuration;
  if (
    typeof configuration !== "object" ||
    configuration === null ||
    Array.isArray(configuration)
  )
    return null;
  return configuration as JsonObject;
}

function validTierReference(value: unknown): boolean {
  return typeof value === "string" && value.startsWith("tier:");
}

function isToolBindingId(value: string): boolean {
  return value.startsWith("tool.") || value.startsWith("mcp:");
}

function instructionsIssue(
  id: string,
  instructions: unknown,
  issues: WorkflowExecutionIssue[],
): void {
  if (instructions === undefined) return;
  if (
    typeof instructions !== "string" ||
    instructions.trim().length === 0 ||
    instructions.includes("\0") ||
    new TextEncoder().encode(instructions).length > MAXIMUM_INSTRUCTIONS_BYTES
  )
    issues.push({
      code: "native_node_configuration",
      message: `Workflow node '${id}' instructions must be a non-empty string of at most ${MAXIMUM_INSTRUCTIONS_BYTES / 1024} KiB.`,
    });
}

const REASONING_EFFORTS = new Set([
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
]);

function modelReasoningIssues(
  id: string,
  configuration: JsonObject,
  issues: WorkflowExecutionIssue[],
): void {
  const effort = configuration.reasoningEffort;
  if (
    effort !== undefined &&
    effort !== null &&
    (typeof effort !== "string" || !REASONING_EFFORTS.has(effort))
  )
    issues.push({
      code: "native_node_configuration",
      message: `Workflow node '${id}' reasoningEffort must be none, minimal, low, medium, high, xhigh, max, or null to inherit.`,
    });
  const enabled = configuration.enableThinking;
  if (
    enabled !== undefined &&
    enabled !== null &&
    typeof enabled !== "boolean"
  )
    issues.push({
      code: "native_node_configuration",
      message: `Workflow node '${id}' enableThinking must be a boolean or null to inherit.`,
    });
}

function validPredicate(
  id: string,
  value: unknown,
  depth: number,
  issues: WorkflowExecutionIssue[],
): boolean {
  if (depth > 4) {
    issues.push({
      code: "native_condition",
      message: `Workflow node '${id}' predicate nesting exceeds 4 levels.`,
    });
    return false;
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    issues.push({
      code: "native_condition",
      message: `Workflow node '${id}' predicate must be an object.`,
    });
    return false;
  }
  const object = value as JsonObject;
  const kind = object.kind;
  if (typeof kind !== "string" || !PREDICATE_KINDS.has(kind)) {
    issues.push({
      code: "native_condition",
      message: `Workflow node '${id}' predicate requires a supported kind.`,
    });
    return false;
  }
  if ((kind === "eq" || kind === "neq") && !("value" in object)) {
    issues.push({
      code: "native_condition",
      message: `Workflow node '${id}' predicate kind ${kind} requires a comparison value.`,
    });
    return false;
  }
  if (kind === "exists" && !("path" in object)) {
    issues.push({
      code: "native_condition",
      message: `Workflow node '${id}' predicate kind exists requires a path.`,
    });
    return false;
  }
  if (kind === "and" || kind === "or") {
    const operands = object.operands;
    if (
      !Array.isArray(operands) ||
      operands.length === 0 ||
      operands.length > 8
    ) {
      issues.push({
        code: "native_condition",
        message: `Workflow node '${id}' predicate operands must contain 1..=8 items.`,
      });
      return false;
    }
    for (const operand of operands) {
      if (!validPredicate(id, operand, depth + 1, issues)) return false;
    }
  }
  if (kind === "not") {
    if (!("operand" in object)) {
      issues.push({
        code: "native_condition",
        message: `Workflow node '${id}' predicate kind not requires operand.`,
      });
      return false;
    }
    if (!validPredicate(id, object.operand, depth + 1, issues)) return false;
  }
  return true;
}

function validateDeclaredPorts(
  id: string,
  node: JsonObject,
  issues: WorkflowExecutionIssue[],
): void {
  for (const portKey of ["inputPorts", "outputPorts"]) {
    const ports = node[portKey];
    if (ports === undefined) continue;
    if (!Array.isArray(ports)) {
      issues.push({
        code: "native_node_configuration",
        message: `Workflow node '${id}' ${portKey} must be an array.`,
      });
      continue;
    }
    for (const port of ports) {
      if (
        typeof port !== "object" ||
        port === null ||
        Array.isArray(port) ||
        typeof (port as JsonObject).name !== "string" ||
        ((port as JsonObject).name as string).trim().length === 0
      )
        issues.push({
          code: "native_node_configuration",
          message: `Workflow node '${id}' ${portKey} entries require a non-empty name.`,
        });
    }
  }
}

function hasCycle(successors: Map<string, string[]>): boolean {
  const visited = new Set<string>();
  const active = new Set<string>();
  const visit = (id: string): boolean => {
    if (active.has(id)) return true;
    if (visited.has(id)) return false;
    active.add(id);
    for (const next of successors.get(id) ?? []) {
      if (visit(next)) return true;
    }
    active.delete(id);
    visited.add(id);
    return false;
  };
  for (const id of successors.keys()) {
    if (visit(id)) return true;
  }
  return false;
}

function nodeId(node: JsonObject): string {
  return typeof node.id === "string" ? node.id : "";
}

function nodeType(node: JsonObject): string {
  return typeof node.type === "string" ? node.type : "";
}

function edgeSource(edge: JsonObject): string {
  return typeof edge.source === "string" ? edge.source : "";
}

function edgeTarget(edge: JsonObject): string {
  return typeof edge.target === "string" ? edge.target : "";
}
