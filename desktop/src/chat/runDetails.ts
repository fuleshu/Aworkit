import type { RuntimeEvent } from "./corePort";
import type { ChatProjection, EvidenceRecord, TimelineItem } from "./types";

export interface RunDetailField {
  readonly label: string;
  readonly value: string;
  readonly status?: string;
}

export interface RunDetailsBreadcrumb {
  readonly id: string | null;
  readonly label: string;
}

export interface RunDetailsLogEntry {
  readonly id: string;
  readonly title: string;
  readonly kind: string;
  readonly status: string;
  readonly time: string;
  readonly depth: number;
}

export type RunDetailsSection =
  | {
      readonly kind: "fields";
      readonly title: string;
      readonly fields: readonly RunDetailField[];
    }
  | {
      readonly kind: "text";
      readonly title: string;
      readonly text: string;
    }
  | {
      readonly kind: "data";
      readonly title: string;
      readonly value: unknown;
    }
  | {
      readonly kind: "log";
      readonly title: string;
      readonly entries: readonly RunDetailsLogEntry[];
    };

export interface RunDetailsView {
  readonly scope: "run" | "item";
  readonly title: string;
  readonly status: string;
  readonly breadcrumbs: readonly RunDetailsBreadcrumb[];
  readonly summary: readonly RunDetailField[];
  readonly sections: readonly RunDetailsSection[];
  readonly raw: unknown;
}

interface RunDetailsInput {
  readonly chat: ChatProjection;
  readonly items: readonly TimelineItem[];
  readonly events: readonly RuntimeEvent[];
  readonly records: readonly EvidenceRecord[];
  readonly selectedId: string | null;
}

/** Builds one truthful human-facing view from the canonical semantic stream. */
export function projectRunDetails(input: RunDetailsInput): RunDetailsView {
  const selected = input.items.find(({ id }) => id === input.selectedId);
  return selected === undefined ? projectEntireRun(input) : projectItem(input, selected);
}

export function rawRunDetailsJson(view: RunDetailsView): string {
  try {
    return JSON.stringify(view.raw, null, 2);
  } catch {
    return "The selected Run details are not serializable.";
  }
}

export function humanizeRunDetailLabel(value: string): string {
  const known: Record<string, string> = {
    inputUnits: "Input tokens",
    outputUnits: "Output tokens",
    inputTokens: "Input tokens",
    outputTokens: "Output tokens",
    providerId: "Provider",
    modelId: "Model",
    modelTierId: "Model tier",
    reasoningEffort: "Reasoning effort",
    reasoning_effort: "Reasoning effort",
    enableThinking: "Thinking enabled",
    enable_thinking: "Thinking enabled",
    capabilityId: "Capability",
    callId: "Call",
    providerCallId: "Provider call",
  };
  if (known[value] !== undefined) return known[value];
  const words = value
    .replaceAll("_", " ")
    .replace(/([a-z0-9])([A-Z])/gu, "$1 $2")
    .trim();
  return words.length === 0
    ? "Value"
    : words.charAt(0).toUpperCase() + words.slice(1);
}

export function formatRunDetailPrimitive(value: unknown): string {
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (typeof value === "number") return value.toLocaleString();
  if (typeof value === "string") return value;
  if (value === null) return "Not available";
  return String(value);
}

function projectEntireRun(input: RunDetailsInput): RunDetailsView {
  const bounds = timeBounds(input.events);
  const usage = runUsage(input.events, input.records);
  const models = availableValues(input.events, input.records, "model");
  const activityFields = [
    countField("Model calls", input.items, isModelCall),
    countField("Tools", input.items, (item) =>
      ["tool", "mcp"].includes(item.kind),
    ),
    countField("Approvals", input.items, (item) => item.kind === "approval"),
    countField("Artifacts", input.items, (item) => item.kind === "artifact"),
    countField("Errors", input.items, (item) => item.kind === "error"),
  ];
  const sections: RunDetailsSection[] = [];
  if (usage.input > 0 || usage.output > 0 || models.length > 0) {
    sections.push({
      kind: "fields",
      title: "Models and usage",
      fields: compactFields([
        models.length > 0 ? field("Models", models.join(", ")) : undefined,
        usage.input > 0 ? field("Input tokens", usage.input.toLocaleString()) : undefined,
        usage.output > 0
          ? field("Output tokens", usage.output.toLocaleString())
          : undefined,
        usage.input + usage.output > 0
          ? field("Total tokens", (usage.input + usage.output).toLocaleString())
          : undefined,
      ]),
    });
  }
  sections.push({ kind: "fields", title: "Activity", fields: activityFields });
  sections.push({
    kind: "log",
    title: "Execution log",
    entries: input.items.map(logEntry),
  });
  return {
    scope: "run",
    title: "Entire run",
    status: input.chat.phase,
    breadcrumbs: [{ id: null, label: "Entire run" }],
    summary: compactFields([
      field("Status", phaseLabel(input.chat.phase), input.chat.phase),
      input.chat.workflowName !== null
        ? field("Workflow", input.chat.workflowName)
        : undefined,
      field("Scope", input.chat.scope),
      input.chat.branch !== null ? field("Branch", input.chat.branch) : undefined,
      bounds.start !== undefined ? field("Started", formatTime(bounds.start)) : undefined,
      bounds.duration !== undefined
        ? field("Duration", formatDuration(bounds.duration))
        : undefined,
      input.chat.queuedInputs.length > 0
        ? field("Queued inputs", input.chat.queuedInputs.length.toLocaleString())
        : undefined,
    ]),
    sections,
    raw: {
      scope: "run",
      chat: input.chat,
      timeline: input.items.map(withoutEmbeddedRaw),
      events: input.events,
      records: redactUnavailableRecords(input.records),
    },
  };
}

function projectItem(
  input: RunDetailsInput,
  selected: TimelineItem,
): RunDetailsView {
  const scopeItems = itemSubtree(input.items, selected);
  const scopeEvents = eventsForItems(input.events, scopeItems);
  const relatedRecords = recordsForEvents(input.records, scopeEvents, scopeItems);
  const bounds = timeBounds(scopeEvents);
  const parent = input.items.find(({ id }) => id === selected.parentSpanId);
  const usage = selectedUsage(scopeEvents, relatedRecords);
  const modelFields = modelDetailFields(selected, scopeEvents, relatedRecords, usage);
  const toolFields = toolDetailFields(selected, scopeEvents, relatedRecords);
  const descendants = scopeItems.filter(({ id }) => id !== selected.id);
  const sections: RunDetailsSection[] = [];
  const summary = (selected.body ?? "").trim();
  const duplicatesKnownField = [...modelFields, ...toolFields].some(
    ({ value }) => value.trim() === summary,
  );
  if (summary.length > 0 && !duplicatesKnownField) {
    sections.push({ kind: "text", title: "Summary", text: summary });
  }
  if (modelFields.length > 0)
    sections.push({ kind: "fields", title: "Model and usage", fields: modelFields });
  if (toolFields.length > 0)
    sections.push({ kind: "fields", title: "Tool execution", fields: toolFields });
  if (selected.input !== undefined)
    sections.push({ kind: "data", title: "Input", value: selected.input });
  if (selected.output !== undefined)
    sections.push({ kind: "data", title: "Output", value: selected.output });
  if (descendants.length > 0) {
    sections.push({
      kind: "log",
      title: "Contained activity",
      entries: descendants.map(logEntry),
    });
  }
  return {
    scope: "item",
    title: selected.title,
    status: selected.status ?? "completed",
    breadcrumbs: itemBreadcrumbs(input.items, selected),
    summary: compactFields([
      field("Status", phaseLabel(selected.status ?? "completed"), selected.status),
      field("Type", itemKindLabel(selected)),
      parent !== undefined ? field("Parent step", parent.title) : undefined,
      bounds.start !== undefined ? field("Started", formatTime(bounds.start)) : undefined,
      bounds.duration !== undefined
        ? field("Duration", formatDuration(bounds.duration))
        : undefined,
    ]),
    sections,
    raw: {
      scope: "timeline_item",
      selection: withoutEmbeddedRaw(selected),
      events: scopeEvents,
      records: redactUnavailableRecords(relatedRecords),
    },
  };
}

function modelDetailFields(
  item: TimelineItem,
  events: readonly RuntimeEvent[],
  records: readonly EvidenceRecord[],
  usage: { readonly input: number; readonly output: number },
): RunDetailField[] {
  const sources = [item.metadata, item.input, ...events.map(({ payload }) => payload), ...records.map(({ value }) => value)];
  const model = namedValue(sources, ["model", "modelId"]);
  const provider = namedValue(sources, ["providerId"]);
  const tier = namedValue(sources, ["modelTierId"]);
  const reasoning = namedValue(sources, ["reasoningEffort", "reasoning_effort"]);
  const thinking = namedValue(sources, ["enableThinking", "enable_thinking"]);
  const applies = isModelCall(item) || model !== undefined || usage.input + usage.output > 0;
  if (!applies) return [];
  return compactFields([
    model !== undefined ? field("Model", formatRunDetailPrimitive(model)) : undefined,
    provider !== undefined ? field("Provider", formatRunDetailPrimitive(provider)) : undefined,
    tier !== undefined ? field("Model tier", formatRunDetailPrimitive(tier)) : undefined,
    reasoning !== undefined
      ? field("Reasoning effort", formatRunDetailPrimitive(reasoning))
      : undefined,
    thinking !== undefined
      ? field("Thinking enabled", formatRunDetailPrimitive(thinking))
      : undefined,
    usage.input > 0 ? field("Input tokens", usage.input.toLocaleString()) : undefined,
    usage.output > 0 ? field("Output tokens", usage.output.toLocaleString()) : undefined,
  ]);
}

function toolDetailFields(
  item: TimelineItem,
  events: readonly RuntimeEvent[],
  records: readonly EvidenceRecord[],
): RunDetailField[] {
  if (!["tool", "mcp", "subagent"].includes(item.kind)) return [];
  const sources = [item.metadata, item.input, ...events.map(({ payload }) => payload), ...records.map(({ value }) => value)];
  const capability = namedValue(sources, ["capabilityId", "name"]);
  const path = namedValue(sources, ["path"]);
  const replay = namedValue(sources, ["automaticReplayAllowed"]);
  return compactFields([
    capability !== undefined
      ? field("Capability", formatRunDetailPrimitive(capability))
      : undefined,
    path !== undefined ? field("Path", formatRunDetailPrimitive(path)) : undefined,
    replay !== undefined
      ? field(
          "Automatic retry",
          replay === true ? "Allowed" : "Not allowed",
          replay === true ? "available" : "opaque",
        )
      : undefined,
  ]);
}

function selectedUsage(
  events: readonly RuntimeEvent[],
  records: readonly EvidenceRecord[],
): { readonly input: number; readonly output: number } {
  const fromEvents = events
    .filter(({ kind }) => kind === "span.usage")
    .reduce(
      (sum, event) => ({
        input: sum.input + numberAt(event.payload, "inputTokens"),
        output: sum.output + numberAt(event.payload, "outputTokens"),
      }),
      { input: 0, output: 0 },
    );
  if (fromEvents.input + fromEvents.output > 0) return fromEvents;
  return records.reduce(
    (sum, record) => ({
      input: sum.input + numberAt(record.value, "inputUnits"),
      output: sum.output + numberAt(record.value, "outputUnits"),
    }),
    { input: 0, output: 0 },
  );
}

function runUsage(
  events: readonly RuntimeEvent[],
  records: readonly EvidenceRecord[],
): { readonly input: number; readonly output: number } {
  const assistantUsage = events
    .filter(({ kind }) => kind === "message.assistant")
    .reduce(
      (sum, event) => ({
        input: sum.input + numberAt(event.payload, "inputUnits"),
        output: sum.output + numberAt(event.payload, "outputUnits"),
      }),
      { input: 0, output: 0 },
    );
  if (assistantUsage.input + assistantUsage.output > 0) return assistantUsage;
  const spanUsage = selectedUsage(events, []);
  if (spanUsage.input + spanUsage.output > 0) return spanUsage;
  return selectedUsage([], records);
}

function itemSubtree(
  items: readonly TimelineItem[],
  selected: TimelineItem,
): TimelineItem[] {
  const ids = new Set([selected.id]);
  let changed = true;
  while (changed) {
    changed = false;
    for (const item of items) {
      if (item.parentSpanId !== undefined && ids.has(item.parentSpanId) && !ids.has(item.id)) {
        ids.add(item.id);
        changed = true;
      }
    }
  }
  return items.filter(({ id }) => ids.has(id));
}

function eventsForItems(
  events: readonly RuntimeEvent[],
  items: readonly TimelineItem[],
): RuntimeEvent[] {
  const spanIds = new Set(
    items.flatMap((item) => (item.spanId === undefined ? [] : [item.spanId])),
  );
  const eventIds = new Set(items.flatMap((item) => rawEventIds(item.raw)));
  for (const item of items) if (item.spanId === undefined) eventIds.add(item.id);
  return events.filter(
    (event) =>
      eventIds.has(event.eventId) ||
      (event.spanId !== undefined && spanIds.has(event.spanId)),
  );
}

function recordsForEvents(
  records: readonly EvidenceRecord[],
  events: readonly RuntimeEvent[],
  items: readonly TimelineItem[],
): EvidenceRecord[] {
  const ids = new Set(events.map(({ eventId }) => `evidence.${eventId}`));
  for (const item of items) ids.add(`evidence.${item.id}`);
  return records.filter(({ id }) => ids.has(id));
}

function rawEventIds(value: unknown): string[] {
  if (Array.isArray(value)) return value.flatMap(rawEventIds);
  const object = asRecord(value);
  return typeof object.eventId === "string" ? [object.eventId] : [];
}

function itemBreadcrumbs(
  items: readonly TimelineItem[],
  selected: TimelineItem,
): RunDetailsBreadcrumb[] {
  const result: RunDetailsBreadcrumb[] = [{ id: null, label: "Entire run" }];
  const ancestors: TimelineItem[] = [];
  let parentId = selected.parentSpanId;
  const visited = new Set<string>();
  while (parentId !== undefined && !visited.has(parentId)) {
    visited.add(parentId);
    const parent = items.find(({ id }) => id === parentId);
    if (parent === undefined) break;
    ancestors.unshift(parent);
    parentId = parent.parentSpanId;
  }
  return result.concat(
    [...ancestors, selected].map(({ id, title }) => ({ id, label: title })),
  );
}

function logEntry(item: TimelineItem): RunDetailsLogEntry {
  return {
    id: item.id,
    title: item.title,
    kind: itemKindLabel(item),
    status: item.status ?? "completed",
    time: item.createdAt.length > 0 ? formatTime(item.createdAt) : "Time not recorded",
    depth: item.depth ?? 0,
  };
}

function isModelCall(item: TimelineItem): boolean {
  const metadata = asRecord(item.metadata);
  return metadata.spanKind === "model_call" || ["model", "thinking"].includes(item.kind);
}

function itemKindLabel(item: TimelineItem): string {
  const labels: Partial<Record<TimelineItem["kind"], string>> = {
    message: item.title === "You" ? "User message" : "Assistant message",
    thinking: "Model reasoning",
    model: "Model call",
    step: "Workflow step",
    tool: "Tool call",
    mcp: "MCP call",
    subagent: "Subagent",
    external_agent: "External agent",
    approval: "Approval",
    artifact: "Artifact",
    route: "Route",
    todo: "Task list",
    error: "Error",
  };
  return labels[item.kind] ?? humanizeRunDetailLabel(item.kind);
}

function availableValues(
  events: readonly RuntimeEvent[],
  records: readonly EvidenceRecord[],
  key: string,
): string[] {
  const values = new Set<string>();
  for (const source of [
    ...events.map(({ payload }) => payload),
    ...records.filter(({ state }) => state === "available").map(({ value }) => value),
  ]) {
    const value = asRecord(source)[key];
    if (typeof value === "string" && value.length > 0) values.add(value);
  }
  return [...values];
}

function namedValue(sources: readonly unknown[], keys: readonly string[]): unknown {
  for (const source of sources) {
    const found = findNamedValue(source, new Set(keys), 0);
    if (found !== undefined && found !== null && found !== "") return found;
  }
  return undefined;
}

function findNamedValue(
  value: unknown,
  keys: ReadonlySet<string>,
  depth: number,
): unknown {
  if (depth > 3) return undefined;
  if (Array.isArray(value)) {
    for (const item of value.slice(0, 20)) {
      const found = findNamedValue(item, keys, depth + 1);
      if (found !== undefined) return found;
    }
    return undefined;
  }
  const object = asRecord(value);
  for (const key of keys) if (Object.hasOwn(object, key)) return object[key];
  for (const nested of Object.values(object)) {
    const found = findNamedValue(nested, keys, depth + 1);
    if (found !== undefined) return found;
  }
  return undefined;
}

function numberAt(value: unknown, key: string): number {
  const candidate = asRecord(value)[key];
  return typeof candidate === "number" && Number.isFinite(candidate) ? candidate : 0;
}

function timeBounds(events: readonly RuntimeEvent[]): {
  readonly start?: number;
  readonly duration?: number;
} {
  const times = events
    .map(({ payload }) => parseTime(asRecord(payload).createdAt))
    .filter((value): value is number => value !== undefined);
  if (times.length === 0) return {};
  const start = Math.min(...times);
  const end = Math.max(...times);
  return { start, duration: Math.max(0, end - start) };
}

function parseTime(value: unknown): number | undefined {
  if (typeof value !== "string" && typeof value !== "number") return undefined;
  const numeric = Number(value);
  if (Number.isFinite(numeric)) return numeric < 100_000_000_000 ? numeric * 1_000 : numeric;
  const parsed = Date.parse(String(value));
  return Number.isNaN(parsed) ? undefined : parsed;
}

function formatTime(value: string | number): string {
  const parsed = parseTime(value);
  if (parsed === undefined) return String(value);
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "medium",
  }).format(new Date(parsed));
}

function formatDuration(milliseconds: number): string {
  if (milliseconds < 1_000) return `${Math.round(milliseconds)} ms`;
  if (milliseconds < 60_000) return `${(milliseconds / 1_000).toFixed(1)} s`;
  const minutes = Math.floor(milliseconds / 60_000);
  const seconds = Math.round((milliseconds % 60_000) / 1_000);
  return `${minutes} min ${seconds} s`;
}

function countField(
  label: string,
  items: readonly TimelineItem[],
  predicate: (item: TimelineItem) => boolean,
): RunDetailField {
  return field(label, items.filter(predicate).length.toLocaleString());
}

function field(label: string, value: string, status?: string): RunDetailField {
  return status === undefined ? { label, value } : { label, value, status };
}

function compactFields(
  fields: readonly (RunDetailField | undefined)[],
): RunDetailField[] {
  return fields.filter((value): value is RunDetailField => value !== undefined);
}

function phaseLabel(value: string): string {
  return humanizeRunDetailLabel(value);
}

function withoutEmbeddedRaw(item: TimelineItem): Omit<TimelineItem, "raw"> {
  const { raw: _raw, ...rest } = item;
  return rest;
}

function redactUnavailableRecords(
  records: readonly EvidenceRecord[],
): readonly EvidenceRecord[] {
  return records.map((record) => ({
    ...record,
    value:
      record.state === "available"
        ? record.value
        : `[${record.state}; value unavailable]`,
  }));
}

function asRecord(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}
