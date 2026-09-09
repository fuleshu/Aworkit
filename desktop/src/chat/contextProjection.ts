import { z } from "zod";
import type { RuntimeEvent } from "./corePort";

const messageSchema = z.object({
  role: z.enum(["system", "user", "assistant"]), content: z.string(),
  images: z.array(z.unknown()).optional(),
}).strict();
const positionedSchema = z.object({
  afterExchanges: z.number().int().nonnegative(), content: z.string(),
  role: z.enum(["user", "assistant"]).optional(), images: z.array(z.unknown()).optional(),
  afterInputMessages: z.number().int().nonnegative().nullable().optional(),
  instructionEventId: z.string().nullable().optional(),
}).strict();
export const contextDocumentSchema = z.object({
  input: z.object({ messages: z.array(messageSchema) }).strict(),
  tools: z.array(z.record(z.string(), z.unknown())),
  exchanges: z.array(z.record(z.string(), z.unknown())),
  contextMessages: z.array(positionedSchema).default([]),
  retryNotice: z.string().nullable().optional(),
}).strict();
export type ContextDocument = z.infer<typeof contextDocumentSchema>;
export interface ContextSelection {
  readonly nodeId: string;
  readonly label: string;
  readonly document: ContextDocument;
  readonly sequence: number;
  readonly inputTokens: number | null;
  readonly outputTokens: number | null;
  readonly edited: boolean;
  readonly pressureTokens?: number;
  readonly pressureReported?: boolean;
}
export interface ContextModel { readonly name: string; readonly contextWindow: number | null }
export const record = (value: unknown): Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};

/** Mirrors the native context projection using the existing canonical event stream. */
export function projectContexts(events: readonly RuntimeEvent[]): ContextSelection[] {
  const spans = new Map(events.filter(e => e.kind === "span.started").map(e => [e.spanId, e]));
  const contexts = new Map<string, ContextSelection>();
  const modelSpans = new Map<string, string>();
  for (const event of events) {
    const fact = record(event.payload);
    if (event.kind === "span.started" && fact.spanKind === "model_call") {
      let parent = spans.get(String(fact.parentSpanId));
      let node: Record<string, unknown> | null = null;
      for (let depth = 0; parent && depth < 32; depth++) {
        const ancestor = record(parent.payload);
        if (ancestor.spanKind === "external_agent") break;
        if (ancestor.spanKind === "graph_node") { node = ancestor; break; }
        parent = spans.get(String(ancestor.parentSpanId));
      }
      if (!node || typeof node.nodeId !== "string" || !event.spanId) continue;
      const raw = record(fact.input);
      const parsed = contextDocumentSchema.safeParse({
        input: raw.tools !== undefined && raw.input !== undefined ? raw.input : raw,
        tools: raw.tools ?? [], exchanges: raw.exchanges ?? [],
        contextMessages: raw.contextMessages ?? [],
        ...(raw.retryNotice ? { retryNotice: raw.retryNotice } : {}),
      });
      if (!parsed.success) continue;
      modelSpans.set(event.spanId, node.nodeId);
      contexts.set(node.nodeId, {
        nodeId: node.nodeId, label: typeof node.label === "string" ? node.label : node.nodeId,
        sequence: event.sequence, document: parsed.data, inputTokens: null, outputTokens: null, edited: false,
      });
    } else if (["context.edited", "context.checkpoint"].includes(event.kind) && typeof fact.nodeId === "string" && fact.child == null) {
      const parsed = contextDocumentSchema.safeParse(event.kind === "context.checkpoint" ? record(fact.snapshot).document : fact.document);
      if (!parsed.success) continue;
      const previous = contexts.get(fact.nodeId);
      contexts.set(fact.nodeId, {
        nodeId: fact.nodeId, label: previous?.label ?? fact.nodeId,
        sequence: event.sequence, document: parsed.data, inputTokens: null, outputTokens: null, edited: event.kind === "context.edited",
        ...(event.kind === "context.checkpoint" && typeof fact.pressureTokens === "number" ? { pressureTokens: fact.pressureTokens, pressureReported: fact.pressureReported === true } : {}),
      });
    } else if (event.spanId && modelSpans.has(event.spanId)) {
      const nodeId = modelSpans.get(event.spanId)!;
      const previous = contexts.get(nodeId);
      if (!previous) continue;
      if (event.kind === "span.usage") {
        contexts.set(nodeId, { ...previous,
          inputTokens: typeof fact.inputTokens === "number" ? fact.inputTokens : null,
          outputTokens: typeof fact.outputTokens === "number" ? fact.outputTokens : null,
        });
      } else if (event.kind === "span.completed" && Array.isArray(fact.output)) {
        const output = fact.output.map(record);
        if (!output.some(e => e.kind === "tool_call")) {
          const content = output.filter(e => e.kind === "assistant_output").map(e => typeof e.text === "string" ? e.text : "").join("");
          contexts.set(nodeId, { ...previous, sequence: event.sequence,
            document: { ...previous.document, contextMessages: [
              ...previous.document.contextMessages,
              ...(content ? [{ afterExchanges: previous.document.exchanges.length, content, role: "assistant" as const }] : []),
            ] },
          });
        }
      }
    }
  }
  return [...contexts.values()].sort((a, b) => b.sequence - a.sequence);
}

export function contextModel(events: readonly RuntimeEvent[], fallback?: ContextModel | null): ContextModel | null {
  for (let index = events.length - 1; index >= 0; index--) {
    const model = record(record(events[index]?.payload).contextModel);
    if (typeof model.name === "string") return {
      name: model.name, contextWindow: typeof model.contextWindow === "number" && model.contextWindow > 0 ? model.contextWindow
        : fallback?.name === model.name ? fallback.contextWindow : null,
    };
  }
  return fallback ?? null;
}

/** Approximate prompt footprint. Counts are never cumulative Run usage or billed tokens. */
export function estimateContext(document: ContextDocument) {
  const tokens = (value: unknown) => Math.ceil((typeof value === "string" ? value : JSON.stringify(value)).length / 4);
  const resultTokens = (value:unknown):number => {
    const candidate=Array.isArray(value)?value:record(value).content;
    if(Array.isArray(candidate)&&candidate.every(block=>typeof record(block).type==="string"))return candidate.map(record).reduce((total,block)=>{
      if(block.type==="text"||block.type==="reasoning")return total+tokens(block.text??"")+4;
      if(block.type==="tool-result")return total+resultTokens(block.content)+4;
      if(block.type==="tool-call")return total+tokens(block.name??"")+tokens(block.arguments??null)+4;
      return total+tokens(block)+4;
    },0);
    return tokens(value??null)+4;
  };
  let system = 0, messages = 0;
  for (const message of document.input.messages) {
    if (message.role === "system") system += tokens(message.content) + 4;
    else messages += tokens(message.content) + 8 + (message.images ?? []).reduce<number>((n,image)=>n+tokens(image)+4,0);
  }
  const tools = document.tools.length ? tokens(document.tools) + 4 : 0;
  for (const exchange of document.exchanges) {
    messages += 4;
    for (const block of Array.isArray(exchange.assistantContent) ? exchange.assistantContent.map(record) : []) {
      messages += 4 + (block.kind === "text" ? tokens(block.text ?? "") : tokens(record(block.call).name ?? "") + tokens(record(block.call).arguments ?? {}));
    }
    for (const result of Array.isArray(exchange.results) ? exchange.results.map(record) : []) {
      messages += resultTokens(result.content) + 8;
      if (Array.isArray(result.images)) messages += result.images.reduce<number>((n,image)=>n+tokens(image)+4,0);
    }
  }
  for (const message of document.contextMessages) messages += tokens(message.content) + 8 + (message.images ?? []).reduce<number>((n,image)=>n+tokens(image)+4,0);
  if (document.retryNotice) messages += tokens(document.retryNotice) + 8;
  const hasImages = document.input.messages.some(m => m.images?.length) || document.contextMessages.some(m => m.images?.length)
    || document.exchanges.some(e => Array.isArray(e.results) && e.results.some(r => Array.isArray(record(r).images) && (record(r).images as unknown[]).length > 0));
  return { system, tools, messages, total: system + tools + messages, hasImages: Boolean(hasImages) };
}

export function compactTokens(value: number): string {
  return value >= 1_000_000 ? (value / 1_000_000).toFixed(1).replace(/\.0$/, "") + "M"
    : value >= 1000 ? (value / 1000).toFixed(value >= 10000 ? 0 : 1).replace(/\.0$/, "") + "K"
    : String(value);
}

/** Provider usage is authoritative even when the local character heuristic overestimates it. */
export function contextUsage(selection: ContextSelection) {
  const estimate = estimateContext(selection.document);
  const reported = selection.pressureTokens !== undefined ? selection.pressureReported === true : selection.inputTokens !== null && selection.inputTokens > 0;
  const total = selection.pressureTokens ?? (reported ? selection.inputTokens! + (selection.outputTokens ?? 0) : estimate.total);
  const scale = estimate.total > 0 ? total / estimate.total : 0;
  const system = Math.round(estimate.system * scale), tools = Math.round(estimate.tools * scale);
  return { ...estimate, total, system, tools, messages: Math.max(0, total - system - tools), reported };
}
