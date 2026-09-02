import type { RuntimeEvent } from "./corePort";
import type { TimelineItem, TimelineKind } from "./types";

type FactPayload = Record<string, unknown>;

interface SpanProjection {
  readonly spanId: string;
  parentSpanId?: string;
  spanKind: string;
  semanticRole: string;
  title: string;
  firstSequence: number;
  createdAt: string;
  status: string;
  input?: unknown;
  output?: unknown;
  hasInput: boolean;
  hasOutput: boolean;
  body: string;
  reasoning: string;
  progress: string;
  assistantOutput: string;
  reasoningCategory?: TimelineItem["reasoningCategory"];
  metadata: FactPayload;
  sourceEvents: RuntimeEvent[];
}

/**
 * The sole Chat timeline reducer. It consumes the exact canonical envelopes
 * for both live operation and replay, folds span deltas in place, and orders
 * cards by the first committed event they represent.
 */
export function projectSemanticTimeline(
  events: readonly RuntimeEvent[],
): TimelineItem[] {
  const ordered = [...events].sort(
    (left, right) => left.sequence - right.sequence,
  );
  const spans = new Map<string, SpanProjection>();
  const facts: TimelineItem[] = [];

  for (const event of ordered) {
    const fact = payload(event);
    if (event.kind.startsWith("span.")) {
      reduceSpan(spans, event, fact);
      continue;
    }
    const item = projectFact(event, fact);
    if (item !== undefined) facts.push(item);
  }

  const visibleSpanIds = new Set(
    [...spans.values()].filter(shouldRenderSpan).map((span) => span.spanId),
  );
  const spanItems = [...spans.values()]
    .filter(shouldRenderSpan)
    .map((span) =>
      spanItem(
        span,
        visibleDepth(span, spans, visibleSpanIds),
        hasFollowingAssistantMessage(span, ordered),
        spanActor(span, spans),
        workflowNodeContext(span, spans),
      ),
    );
  return [...facts, ...spanItems].sort(
    (left, right) =>
      (left.sequence ?? Number.MAX_SAFE_INTEGER) -
      (right.sequence ?? Number.MAX_SAFE_INTEGER),
  );
}

function reduceSpan(
  spans: Map<string, SpanProjection>,
  event: RuntimeEvent,
  fact: FactPayload,
): void {
  const spanId = event.spanId ?? string(fact.spanId);
  if (spanId === undefined) return;
  let span = spans.get(spanId);
  if (span === undefined) {
    span = {
      spanId,
      parentSpanId: string(fact.parentSpanId),
      spanKind: string(fact.spanKind) ?? "unknown",
      semanticRole: string(fact.semanticRole) ?? "unknown",
      title: string(fact.title) ?? "Activity",
      firstSequence: event.sequence,
      createdAt: string(fact.createdAt) ?? "",
      status: string(fact.status) ?? "running",
      input: fact.input,
      output: fact.output,
      hasInput: fact.hasInput === true,
      hasOutput: fact.hasOutput === true,
      body: string(fact.body) ?? "",
      reasoning: "",
      progress: "",
      assistantOutput: "",
      metadata: { ...fact },
      sourceEvents: [],
    };
    spans.set(spanId, span);
  }
  span.sourceEvents.push(event);
  span.parentSpanId = string(fact.parentSpanId) ?? span.parentSpanId;
  span.spanKind = string(fact.spanKind) ?? span.spanKind;
  span.semanticRole = string(fact.semanticRole) ?? span.semanticRole;
  span.title = string(fact.title) ?? span.title;
  span.createdAt = string(fact.createdAt) ?? span.createdAt;
  span.status = string(fact.status) ?? span.status;
  span.hasInput = fact.hasInput === true || span.hasInput;
  span.hasOutput = fact.hasOutput === true || span.hasOutput;
  if (Object.hasOwn(fact, "input") && fact.input !== null)
    span.input = fact.input;
  if (Object.hasOwn(fact, "output") && fact.output !== null)
    span.output = fact.output;
  if (typeof fact.body === "string" && event.kind !== "span.content_delta")
    span.body = fact.body;
  Object.assign(span.metadata, fact);

  if (event.kind === "span.content_delta") {
    const append = string(fact.append) ?? string(fact.body) ?? "";
    const channel = string(fact.channel);
    if (channel === "reasoning") {
      span.reasoning += append;
      span.reasoningCategory = reasoningCategory(fact.sourceClassification);
    } else if (channel === "progress") {
      span.progress += append;
      span.reasoningCategory ??= "progress";
    } else if (channel === "assistant_output") {
      span.assistantOutput += append;
    }
  }
}

function shouldRenderSpan(span: SpanProjection): boolean {
  if (span.spanKind === "run") return false;
  if (span.spanKind !== "graph_node") return true;
  return !["input", "output", "wait", "completion"].includes(
    span.semanticRole,
  );
}

function visibleDepth(
  span: SpanProjection,
  spans: ReadonlyMap<string, SpanProjection>,
  visible: ReadonlySet<string>,
): number {
  let depth = 0;
  let parent = span.parentSpanId;
  const visited = new Set<string>();
  while (parent !== undefined && !visited.has(parent)) {
    visited.add(parent);
    if (visible.has(parent)) depth += 1;
    parent = spans.get(parent)?.parentSpanId;
  }
  return depth;
}

function spanItem(
  span: SpanProjection,
  depth: number,
  hasFinalAssistant: boolean,
  actor: TimelineItem["actor"],
  workflowNode: FactPayload | undefined,
): TimelineItem {
  const output =
    span.spanKind === "model_call"
      ? canonicalModelResultOutput(span.output)
      : span.output;
  const includeAssistantOutput =
    span.spanKind === "model_call" && !hasFinalAssistant;
  const streamedBody = [
    span.reasoning,
    span.progress,
    includeAssistantOutput ? span.assistantOutput : "",
  ]
    .filter((part) => part.length > 0)
    .join("\n");
  const body = streamedBody || span.body;
  const suppressRedundantOutput =
    hasFinalAssistant &&
    (span.spanKind === "agent_loop" ||
      (span.spanKind === "graph_node" &&
        ["agent", "model_call"].includes(span.semanticRole)));
  const metadata = {
    ...span.metadata,
    spanId: span.spanId,
    parentSpanId: span.parentSpanId,
    spanKind: span.spanKind,
    semanticRole: span.semanticRole,
    hasInput: span.hasInput,
    hasOutput: span.hasOutput && !suppressRedundantOutput,
    input: span.input,
    output: suppressRedundantOutput ? undefined : output,
    channels: {
      reasoning: span.reasoning,
      progress: span.progress,
      assistantOutput: span.assistantOutput,
    },
    actor,
    live: isBusy(span.status),
    workflowNode,
  };
  return {
    id: span.spanId,
    sequence: span.firstSequence,
    spanId: span.spanId,
    parentSpanId: span.parentSpanId,
    depth,
    kind: spanTimelineKind(span),
    actor,
    title: span.title,
    body,
    reasoningCategory: span.reasoningCategory,
    createdAt: span.createdAt,
    status: normalizeStatus(span.status),
    input: span.hasInput ? span.input : undefined,
    output:
      span.hasOutput && !suppressRedundantOutput ? output : undefined,
    raw: span.sourceEvents,
    metadata,
  };
}

/**
 * Upgrades terminal arrays written before the canonical Rust result projector
 * existed. Raw source events remain unchanged on `TimelineItem.raw`; every
 * visible consumer receives the same compact result from this reducer.
 */
function canonicalModelResultOutput(value: unknown): unknown {
  if (!Array.isArray(value)) return value;
  const compacted: unknown[] = [];
  const textIndexes = new Map<string, number>();
  for (const entry of value) {
    if (!isRecord(entry) || typeof entry.kind !== "string") {
      compacted.push(entry);
      continue;
    }
    const text =
      typeof entry.text === "string"
        ? entry.text
        : typeof entry.data === "string"
          ? entry.data
          : undefined;
    if (text !== undefined) {
      const existing = textIndexes.get(entry.kind);
      if (existing !== undefined) {
        const prior = compacted[existing];
        if (isRecord(prior) && typeof prior.text === "string") {
          compacted[existing] = { ...prior, text: prior.text + text };
        }
        continue;
      }
      const canonical: FactPayload = { ...entry, text };
      delete canonical.data;
      textIndexes.set(entry.kind, compacted.length);
      compacted.push(canonical);
      continue;
    }
    if (entry.kind === "usage" && isRecord(entry.data)) {
      const canonical: FactPayload = { ...entry, ...entry.data };
      delete canonical.data;
      compacted.push(canonical);
      continue;
    }
    compacted.push(entry);
  }
  return compacted;
}

/** Nearest graph-node ancestor supplies the user-authored workflow context. */
function workflowNodeContext(
  span: SpanProjection,
  spans: ReadonlyMap<string, SpanProjection>,
): FactPayload | undefined {
  let current =
    span.parentSpanId === undefined ? undefined : spans.get(span.parentSpanId);
  const visited = new Set<string>();
  while (current !== undefined && !visited.has(current.spanId)) {
    visited.add(current.spanId);
    if (current.spanKind === "graph_node") {
      return {
        id: current.metadata.nodeId ?? current.spanId,
        name: current.metadata.label ?? current.title,
        type: current.metadata.nodeType ?? current.semanticRole,
      };
    }
    current =
      current.parentSpanId === undefined
        ? undefined
        : spans.get(current.parentSpanId);
  }
  return undefined;
}

function spanActor(
  span: SpanProjection,
  spans: ReadonlyMap<string, SpanProjection>,
): TimelineItem["actor"] {
  let current: SpanProjection | undefined = span;
  const visited = new Set<string>();
  while (current !== undefined && !visited.has(current.spanId)) {
    visited.add(current.spanId);
    if (current.spanKind === "external_agent") return "subagent";
    current =
      current.parentSpanId === undefined
        ? undefined
        : spans.get(current.parentSpanId);
  }
  return "model";
}

function spanTimelineKind(span: SpanProjection): TimelineKind {
  if (span.spanKind === "model_call")
    return span.reasoning.length > 0 ? "thinking" : "model";
  if (span.spanKind === "agent_loop") return "step";
  if (span.spanKind === "external_agent") return "subagent";
  if (span.spanKind === "tool_call") {
    const capability = string(span.metadata.capabilityId) ?? "";
    if (capability === "tool.subagent") return "subagent";
    if (capability.startsWith("mcp.")) return "mcp";
    return "tool";
  }
  if (span.spanKind === "graph_node") {
    if (span.semanticRole === "plan") return "step";
    if (["agent", "model_call"].includes(span.semanticRole)) return "model";
    if (span.semanticRole === "tool") return "tool";
    if (span.semanticRole === "condition") return "route";
    if (span.status === "failed") return "error";
    return "step";
  }
  return span.status === "failed" ? "error" : "unknown";
}

function hasFollowingAssistantMessage(
  span: SpanProjection,
  events: readonly RuntimeEvent[],
): boolean {
  for (const event of events) {
    if (event.sequence <= span.firstSequence) continue;
    if (event.kind === "message.user") return false;
    if (event.kind === "message.assistant") return true;
  }
  return false;
}

function projectFact(
  event: RuntimeEvent,
  fact: FactPayload,
): TimelineItem | undefined {
  if (event.kind === "message.user" || event.kind === "message.assistant") {
    return baseItem(event, fact, {
      kind: "message",
      title: event.kind === "message.user" ? "You" : "Aworkit",
      status: "completed",
    });
  }
  if (event.kind === "approval.requested") {
    return {
      ...baseItem(event, fact, {
        kind: "approval",
        title: "Approval required",
        status: "pending",
      }),
      id: string(fact.decisionId) ?? event.eventId,
      action: "approve",
    };
  }
  if (event.kind === "approval.resolved") {
    return baseItem(event, fact, {
      kind: "approval",
      title: "Approval resolved",
      status: "completed",
    });
  }
  if (event.kind === "execution.failed") {
    return baseItem(event, fact, {
      kind: "error",
      title: string(fact.title) ?? "Execution failed",
      status: "failed",
    });
  }
  if (event.kind === "tool.todo") return todoCard(event, fact);
  return projectLegacyActivity(event, fact);
}

function projectLegacyActivity(
  event: RuntimeEvent,
  fact: FactPayload,
): TimelineItem | undefined {
  if (event.kind === "model.reasoning" || event.kind === "model.progress") {
    return baseItem(event, fact, {
      kind: "thinking",
      title: event.kind === "model.reasoning" ? "Thinking" : "Working",
      status: string(fact.status) ?? "completed",
    });
  }
  if (event.kind === "model.turn")
    return baseItem(event, fact, {
      kind: "model",
      title: string(fact.title) ?? "Model turn",
      status: string(fact.status) ?? "completed",
    });
  if (
    ["tool.completed", "tool.failed", "tool.waiting"].includes(event.kind)
  ) {
    return baseItem(event, fact, {
      kind: "tool",
      title: string(fact.title) ?? string(fact.capabilityId) ?? "Tool call",
      status: string(fact.status) ?? terminalStatus(event.kind),
    });
  }
  if (event.kind.startsWith("mcp."))
    return baseItem(event, fact, {
      kind: "mcp",
      title: string(fact.capabilityId) ?? "MCP call",
      status: terminalStatus(event.kind),
    });
  if (event.kind.startsWith("subagent."))
    return {
      ...baseItem(event, fact, {
        kind: "subagent",
        title: "Subagent run",
        status: terminalStatus(event.kind),
      }),
      actor: "subagent",
    };
  if (event.kind.startsWith("node.")) {
    const role = string(fact.nodeType) ?? "unknown";
    if (["input", "output", "wait", "completion"].includes(role))
      return undefined;
    return baseItem(event, fact, {
      kind: role === "condition" ? "route" : role === "tool" ? "tool" : "step",
      title: string(fact.label) ?? "Workflow node",
      status: string(fact.status) ?? terminalStatus(event.kind),
    });
  }
  return undefined;
}

function baseItem(
  event: RuntimeEvent,
  fact: FactPayload,
  display: {
    readonly kind: TimelineKind;
    readonly title: string;
    readonly status: string;
  },
): TimelineItem {
  return {
    id: event.eventId,
    sequence: event.sequence,
    kind: display.kind,
    actor:
      event.kind === "message.assistant" || event.kind.startsWith("model.")
        ? "model"
        : undefined,
    title: display.title,
    body: string(fact.body) ?? "",
    createdAt: string(fact.createdAt) ?? "",
    status: display.status,
    reasoningCategory: reasoningCategory(fact.reasoningCategory),
    input: fact.hasInput === true ? fact.input : undefined,
    output: fact.hasOutput === true ? fact.output : undefined,
    raw: event,
    metadata: fact,
  };
}

function todoCard(event: RuntimeEvent, fact: FactPayload): TimelineItem {
  const todos = Array.isArray(fact.todos) ? fact.todos : [];
  return {
    ...baseItem(event, fact, {
      kind: "todo",
      title: "Task list",
      status: "completed",
    }),
    body: todos
      .map((todo) => {
        const item = record(todo);
        const content = string(item.content) ?? String(item.content ?? "");
        const status = string(item.status) ?? "";
        return status.length === 0 ? content : `[${status}] ${content}`;
      })
      .join("\n"),
  };
}

function payload(event: RuntimeEvent): FactPayload {
  return record(event.payload);
}

function record(value: unknown): FactPayload {
  return isRecord(value) ? value : {};
}

function isRecord(value: unknown): value is FactPayload {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function string(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function reasoningCategory(
  value: unknown,
): TimelineItem["reasoningCategory"] {
  if (value === "summary") return "summary";
  if (value === "progress") return "progress";
  if (value === "source_provided" || value === "source-provided")
    return "source_provided";
  return undefined;
}

function terminalStatus(kind: string): string {
  if (kind.endsWith("failed")) return "failed";
  if (kind.endsWith("waiting") || kind.endsWith("started")) return "running";
  if (kind.endsWith("skipped")) return "skipped";
  return "completed";
}

function normalizeStatus(status: string): string {
  return status === "started" ? "running" : status;
}

function isBusy(status: string): boolean {
  return status === "running" || status === "started" || status === "waiting";
}
