import { describe, expect, it } from "vitest";
import {
  hasOpenSemanticSpan,
  projectSemanticTimeline,
} from "./activityProjection";
import type { RuntimeEvent } from "./corePort";

describe("canonical semantic timeline projection", () => {
  it("derives live running state from started spans until their terminal fact", () => {
    const started = span(1, "span.started", "span.run.live", {
      spanKind: "run",
      semanticRole: "run",
      title: "Run",
    });
    expect(hasOpenSemanticSpan([started])).toBe(true);
    expect(
      hasOpenSemanticSpan([
        started,
        span(2, "span.cancelled", "span.run.live", {
          status: "cancelled",
        }),
      ]),
    ).toBe(false);
  });

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

  it("upgrades legacy model terminal fragments at the replay boundary", () => {
    const events = [
      span(1, "span.started", "span.model.legacy", {
        spanKind: "model_call",
        semanticRole: "model_call",
        title: "Model call 1",
      }),
      span(2, "span.completed", "span.model.legacy", {
        status: "completed",
        hasOutput: true,
        output: [
          { kind: "reasoning_raw", data: "The" },
          { kind: "reasoning_raw", data: " user said hello." },
          { kind: "assistant_output", data: "Hello " },
          { kind: "assistant_output", data: "there!" },
          {
            kind: "usage",
            data: { input_tokens: 8, output_tokens: 3 },
          },
        ],
      }),
    ];

    const modelCall = projectSemanticTimeline(events)[0];

    expect(modelCall.output).toEqual([
      { kind: "reasoning_raw", text: "The user said hello." },
      { kind: "assistant_output", text: "Hello there!" },
      { kind: "usage", input_tokens: 8, output_tokens: 3 },
    ]);
    expect(modelCall.raw).toEqual(events);
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

  it("projects delegated model thought and speech as subagent-owned", () => {
    const items = projectSemanticTimeline([
      span(1, "span.started", "span.agent", {
        spanKind: "agent_loop",
        semanticRole: "agent_loop",
        title: "Agent",
      }),
      span(2, "span.started", "span.subagent", {
        parentSpanId: "span.agent",
        spanKind: "external_agent",
        semanticRole: "tool",
        title: "tool.subagent",
        capabilityId: "tool.subagent",
      }),
      span(3, "span.started", "span.subagent.model", {
        parentSpanId: "span.subagent",
        spanKind: "model_call",
        semanticRole: "model_call",
        title: "Model call 1",
      }),
      span(4, "span.content_delta", "span.subagent.model", {
        channel: "reasoning",
        sourceClassification: "source_provided",
        append: "Inspecting the delegated context.",
      }),
      span(5, "span.completed", "span.subagent.model", {
        status: "completed",
      }),
      span(6, "span.completed", "span.subagent", {
        status: "completed",
        hasOutput: true,
        output: { finalText: "**Delegated result**" },
      }),
    ]);

    expect(items.find((item) => item.id === "span.subagent")).toMatchObject({
      kind: "subagent",
      actor: "subagent",
      output: { finalText: "**Delegated result**" },
    });
    expect(
      items.find((item) => item.id === "span.subagent.model"),
    ).toMatchObject({
      kind: "thinking",
      actor: "subagent",
      body: "Inspecting the delegated context.",
    });
  });

  it("adds the nearest workflow node name and type to a provider call", () => {
    const items = projectSemanticTimeline([
      span(1, "span.started", "span.node.respond", {
        spanKind: "graph_node",
        semanticRole: "agent",
        nodeId: "respond",
        nodeType: "agent",
        label: "Friendly responder",
        title: "Friendly responder",
      }),
      span(2, "span.started", "span.agent", {
        parentSpanId: "span.node.respond",
        spanKind: "agent_loop",
        semanticRole: "agent_loop",
        title: "Agent",
      }),
      span(3, "span.started", "span.model", {
        parentSpanId: "span.agent",
        spanKind: "model_call",
        semanticRole: "model_call",
        title: "Model call 1",
      }),
    ]);

    expect(items.find(({ id }) => id === "span.model")?.metadata).toMatchObject({
      workflowNode: {
        id: "respond",
        name: "Friendly responder",
        type: "agent",
      },
    });
  });

  it("keeps detailed approval copy on one card and removes settled actions", () => {
    const requested = event(1, "approval.requested", {
      decisionId: "invoke.git-command",
      title: "Allow Git shell command?",
      body: "The model wants to run this host shell command:\n\ngit status --short",
      createdAt: "1",
    });

    expect(projectSemanticTimeline([requested])).toEqual([
      expect.objectContaining({
        id: "invoke.git-command",
        title: "Allow Git shell command?",
        body: expect.stringContaining("git status --short"),
        status: "pending",
        action: "approve",
      }),
    ]);

    expect(
      projectSemanticTimeline([
        requested,
        event(2, "approval.resolved", {
          decisionId: "invoke.git-command",
          approved: false,
        }),
      ]),
    ).toEqual([
      expect.objectContaining({
        id: "invoke.git-command",
        status: "rejected",
        action: undefined,
      }),
    ]);

    expect(
      projectSemanticTimeline([
        requested,
        event(2, "chat.cancelled", { createdAt: "2" }),
      ])[0],
    ).toMatchObject({ status: "cancelled", action: undefined });
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
