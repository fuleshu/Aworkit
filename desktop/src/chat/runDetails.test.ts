import { describe, expect, it } from "vitest";
import type { RuntimeEvent } from "./corePort";
import {
  projectRunDetails,
  rawRunDetailsJson,
} from "./runDetails";
import type { ChatProjection, EvidenceRecord, TimelineItem } from "./types";

const chat: ChatProjection = {
  chatId: "chat.test",
  runId: "run.test",
  title: "Run details",
  scope: "Project Atlas",
  workflowId: "workflow.standard-agent",
  workflowName: "Standard Agent",
  branch: "main",
  projectId: "project.atlas",
  phase: "completed",
  lockedWorkflow: true,
  recoveryPending: false,
  queuedInputs: [],
  expectedVersion: 9,
};

describe("Run details projection", () => {
  it("summarizes the entire run and uses one chronological execution log", () => {
    const items: TimelineItem[] = [
      item("event.chat.1", "message", "You", 1),
      item("span.model.1", "model", "Model call 1", 2, {
        spanId: "span.model.1",
        status: "completed",
      }),
      item("span.tool.1", "tool", "tool.files.read", 5, {
        spanId: "span.tool.1",
        parentSpanId: "span.model.1",
        status: "completed",
      }),
      item("event.chat.9", "message", "Aworkit", 9),
    ];
    const events = [
      event(1, "message.user", { body: "Read it", createdAt: "1000" }),
      spanEvent(2, "span.started", "span.model.1", {
        spanKind: "model_call",
        createdAt: "1001000",
      }),
      spanEvent(3, "span.usage", "span.model.1", {
        inputTokens: 12,
        outputTokens: 7,
        createdAt: "1002000",
      }),
      event(9, "message.assistant", {
        body: "Done",
        model: "qwen-test",
        inputUnits: 12,
        outputUnits: 7,
        createdAt: "1003",
      }),
    ];

    const view = projectRunDetails({ chat, items, events, records: [], selectedId: null });

    expect(view.title).toBe("Entire run");
    expect(view.summary).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ label: "Status", value: "Completed" }),
        expect.objectContaining({ label: "Workflow", value: "Standard Agent" }),
      ]),
    );
    expect(view.sections).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ kind: "fields", title: "Models and usage" }),
        expect.objectContaining({ kind: "log", title: "Execution log", entries: expect.any(Array) }),
      ]),
    );
  });

  it("maps a selected parent span to its children, usage, and exact scoped raw data", () => {
    const parent = item("span.agent.1", "step", "Agent", 1, {
      spanId: "span.agent.1",
      status: "completed",
      input: { request: "Inspect the project" },
      output: { result: "Settled" },
    });
    const child = item("span.model.1", "thinking", "Model call 1", 2, {
      spanId: "span.model.1",
      parentSpanId: "span.agent.1",
      status: "completed",
    });
    const events = [
      spanEvent(1, "span.started", "span.agent.1", { createdAt: "1000000" }),
      spanEvent(2, "span.started", "span.model.1", { createdAt: "1000100" }),
      spanEvent(3, "span.usage", "span.model.1", {
        inputTokens: 20,
        outputTokens: 5,
        createdAt: "1000200",
      }),
      spanEvent(4, "span.completed", "span.model.1", { createdAt: "1000300" }),
      spanEvent(5, "span.completed", "span.agent.1", { createdAt: "1000400" }),
      event(6, "message.assistant", { body: "Outside scope", createdAt: "1001" }),
    ];

    const view = projectRunDetails({
      chat,
      items: [parent, child],
      events,
      records: [],
      selectedId: parent.id,
    });

    expect(view.title).toBe("Agent");
    expect(view.breadcrumbs.map(({ label }) => label)).toEqual(["Entire run", "Agent"]);
    expect(view.sections).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ kind: "data", title: "Input" }),
        expect.objectContaining({ kind: "data", title: "Output" }),
        expect.objectContaining({ kind: "log", title: "Contained activity" }),
      ]),
    );
    const raw = rawRunDetailsJson(view);
    expect(raw).toContain("span.model.1");
    expect(raw).not.toContain("Outside scope");
  });

  it("redacts unavailable record values from Raw JSON", () => {
    const message = item("event.chat.1", "message", "Aworkit", 1);
    const records: EvidenceRecord[] = [
      {
        id: "evidence.event.chat.1",
        category: "debug",
        label: "Redacted record",
        value: "must-not-leak",
        state: "redacted",
      },
    ];
    const view = projectRunDetails({
      chat,
      items: [message],
      events: [event(1, "message.assistant", { createdAt: "1000" })],
      records,
      selectedId: message.id,
    });
    expect(rawRunDetailsJson(view)).not.toContain("must-not-leak");
    expect(rawRunDetailsJson(view)).toContain("redacted; value unavailable");
  });
});

function item(
  id: string,
  kind: TimelineItem["kind"],
  title: string,
  sequence: number,
  overrides: Partial<TimelineItem> = {},
): TimelineItem {
  return {
    id,
    kind,
    title,
    sequence,
    createdAt: String(1000 + sequence),
    ...overrides,
  };
}

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

function spanEvent(
  sequence: number,
  kind: string,
  spanId: string,
  payload: Record<string, unknown>,
): RuntimeEvent {
  return { ...event(sequence, kind, { spanId, ...payload }), spanId };
}
