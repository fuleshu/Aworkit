import { expect, it } from "vitest";
import { projectSemanticTimeline } from "./activityProjection";
import { conversationFeed, hasEarlierActivity } from "./conversationFeed";
import type { RuntimeEvent } from "./corePort";

const span = (sequence: number, kind: string, id: string, payload: Record<string, unknown> = {}): RuntimeEvent => ({
  schemaVersion: 1, streamId: "chat.test", branchId: "main", eventId: `event.${sequence}`,
  sequence, kind, spanId: id, payload: { spanId: id, createdAt: String(sequence), ...payload },
});
const parents = [
  span(1, "span.started", "node", { spanKind: "graph_node", semanticRole: "agent", title: "Agent", hasInput: true, input: { evidenceNeeded: ["unchanged parent input"] } }),
  span(2, "span.started", "loop", { spanKind: "agent_loop", parentSpanId: "node", title: "Agent" }),
];
const earlier = [
  span(3, "span.started", "model.1", { spanKind: "model_call", title: "Model call 1", parentSpanId: "loop" }),
  span(4, "span.completed", "model.1", { status: "completed" }),
  span(5, "span.started", "tool.1", { spanKind: "tool_call", title: "Read file", parentSpanId: "loop" }),
  span(6, "span.completed", "tool.1", { status: "completed" }),
];
const recent = [
  span(100, "span.started", "model.2", { spanKind: "model_call", title: "Model call 2", parentSpanId: "loop", hasInput: true, input: { messages: ["exact input"] } }),
  span(101, "span.content_delta", "model.2", { channel: "reasoning", append: "Thinking" }),
  span(105, "span.failed", "model.2", { status: "failed", body: "Provider failed" }),
  span(107, "span.failed", "loop", { status: "failed", body: "Agent loop settled" }),
  span(108, "span.failed", "node", { status: "failed", body: "Agent failed" }),
];
it("prepends earlier model/tool activities and leaves existing cards unchanged", () => {
  const before = conversationFeed(projectSemanticTimeline([...parents, ...recent]), 101);
  const after = conversationFeed(projectSemanticTimeline([...parents, ...earlier, ...recent]), 3);
  expect(before.map(item => item.id)).toEqual(["model.2", "node"]);
  expect(after.map(item => item.id)).toEqual(["model.1", "tool.1", "model.2", "node"]);
  expect(after.slice(2)).toEqual(before);
  expect(hasEarlierActivity(before, after)).toBe(true);
  expect(before[0]).toMatchObject({ depth: 0, title: "Model call 2" });
  expect(before[1]).toMatchObject({ sequence: 108, createdAt: "108", metadata: { feedStatus: true } });
});
it("retains exact parents in Run details without treating hydration as another feed activity", () => {
  const items = projectSemanticTimeline([...parents, ...recent]);
  const feed = conversationFeed(items, 101);
  expect(items.find(item => item.id === "node")?.input).toEqual({ evidenceNeeded: ["unchanged parent input"] });
  expect(items.find(item => item.id === "loop")?.parentSpanId).toBe("node");
  expect(hasEarlierActivity(feed, conversationFeed(items, 99))).toBe(false);
  expect(feed.find(item => item.id === "loop")).toBeUndefined();
});
it("keeps a failed agent with no model call visible at its failure time", () => {
  const items = projectSemanticTimeline([parents[0], span(10, "span.failed", "node", { status: "failed", body: "Could not start" })]);
  expect(conversationFeed(items, 5)).toMatchObject([{ id: "node", sequence: 10, body: "Could not start" }]);
});
