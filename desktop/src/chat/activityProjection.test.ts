import { describe, expect, it } from "vitest";
import { projectSemanticTimeline } from "./activityProjection";
import type { RuntimeEvent } from "./corePort";

describe("canonical semantic timeline projection", () => {
  it("renders model-tool-model in committed order with hierarchical spans", () => {
    const events = [
      event(1, "message.user", { body: "List files", createdAt: "1" }),
      span(2, "span.started", "span.run.1", {
        spanKind: "run",
        semanticRole: "run",
        title: "Run",
      }),
      span(3, "span.started", "span.agent.1", {
        parentSpanId: "span.run.1",
        spanKind: "agent_loop",
        semanticRole: "agent_loop",
        title: "Agent",
      }),
      span(4, "span.started", "span.model.1", {
        parentSpanId: "span.agent.1",
        spanKind: "model_call",
        semanticRole: "model_call",
        title: "Model call 1",
        hasInput: true,
        input: { messages: ["List files"] },
      }),
      span(5, "span.content_delta", "span.model.1", {
        channel: "reasoning",
        sourceClassification: "source_provided",
        append: "I need the file tool.",
      }),
      span(6, "span.completed", "span.model.1", {
        status: "completed",
        hasOutput: true,
        output: { toolCall: "call.1" },
      }),
      span(7, "span.started", "span.tool.1", {
        parentSpanId: "span.agent.1",
        spanKind: "tool_call",
        semanticRole: "tool",
        title: "tool.files.list",
        capabilityId: "tool.files.list",
        hasInput: true,
        input: { path: "." },
      }),
      span(8, "span.completed", "span.tool.1", {
        status: "completed",
        hasOutput: true,
        output: ["Cargo.toml"],
      }),
      span(9, "span.started", "span.model.2", {
        parentSpanId: "span.agent.1",
        spanKind: "model_call",
        semanticRole: "model_call",
        title: "Model call 2",
      }),
    ];

    const items = projectSemanticTimeline(events);

    expect(items.map((item) => item.id)).toEqual([
      "event.chat.1",
      "span.agent.1",
      "span.model.1",
      "span.tool.1",
      "span.model.2",
    ]);
    expect(items.find((item) => item.id === "span.model.1")).toMatchObject({
      body: "I need the file tool.",
      depth: 1,
      input: { messages: ["List files"] },
      output: { toolCall: "call.1" },
    });
    expect(items.find((item) => item.id === "span.tool.1")).toMatchObject({
      depth: 1,
      input: { path: "." },
      output: ["Cargo.toml"],
    });
  });

  it("uses the same running card for deltas and terminal replay", () => {
    const running = [
      span(1, "span.started", "span.model.1", {
        spanKind: "model_call",
        semanticRole: "model_call",
        title: "Model call 1",
      }),
      span(2, "span.content_delta", "span.model.1", {
        channel: "reasoning",
        sourceClassification: "summary",
        append: "First",
      }),
      span(3, "span.content_delta", "span.model.1", {
        channel: "reasoning",
        sourceClassification: "summary",
        append: " second",
      }),
    ];
    expect(projectSemanticTimeline(running)[0]).toMatchObject({
      id: "span.model.1",
      body: "First second",
      status: "running",
    });
    expect(
      projectSemanticTimeline([
        ...running,
        span(4, "span.completed", "span.model.1", {
          status: "completed",
          hasOutput: true,
          output: "done",
        }),
      ])[0],
    ).toMatchObject({
      id: "span.model.1",
      body: "First second",
      status: "completed",
      output: "done",
    });
  });

  it("hides input, output, and wait graph plumbing", () => {
    const items = projectSemanticTimeline([
      span(1, "span.started", "span.input", {
        spanKind: "graph_node",
        semanticRole: "input",
        title: "Input",
      }),
      span(2, "span.started", "span.output", {
        spanKind: "graph_node",
        semanticRole: "output",
        title: "Output",
      }),
      span(3, "span.started", "span.wait", {
        spanKind: "graph_node",
        semanticRole: "wait",
        title: "Wait for input",
      }),
    ]);
    expect(items).toEqual([]);
  });
});

function event(
  sequence: number,
  kind: string,
  payload: Record<string, unknown>,
): RuntimeEvent {
  return {
    schemaVersion: 1,
    streamId: "chat.test",
    branchId: "main",
    sequence,
    eventId: `event.chat.${sequence}`,
    kind,
    payload,
  };
}

function span(
  sequence: number,
  kind: string,
  spanId: string,
  payload: Record<string, unknown>,
): RuntimeEvent {
  return event(sequence, kind, { spanId, ...payload, status: payload.status ?? "running" });
}
