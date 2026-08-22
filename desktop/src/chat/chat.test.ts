import { describe, expect, it } from "vitest";
import {
  canSubmit,
  controlsFor,
  emptyComposer,
  submitIntent,
  updateComposer,
} from "./composer";
import {
  escapedText,
  toConversationCard,
  visibleTimeline,
} from "./conversation";
import {
  inspectEvidence,
  queryEvidence,
  redactedEvidenceJson,
} from "./evidence";
import { normalizeRuntimeSnapshot, PreviewChatCorePort } from "./corePort";
import type { ChatProjection, EvidenceRecord } from "./types";
import { ChatWorkspaceController } from "./workspace";

const draftChat: ChatProjection = {
  chatId: "chat-1",
  runId: "run-1",
  title: "Chat",
  scope: "Project",
  workflowName: null,
  branch: "main",
  phase: "draft",
  lockedWorkflow: false,
  queuedInputs: [],
  expectedVersion: 0,
};

describe("Milestone 08 Chat and evidence experience", () => {
  it("creates first-send and queued-input intents while retaining drafts until a receipt", () => {
    const draft = updateComposer(emptyComposer, {
      draft: "hello",
      attachments: ["brief.md"],
    });
    expect(submitIntent(draft, draftChat, "chat.1")).toEqual({
      type: "start",
      commandId: "chat.1",
      workflowId: "workflow.repository-engineer",
      input: "hello",
      attachments: ["brief.md"],
    });
    expect(
      submitIntent(
        draft,
        { ...draftChat, lockedWorkflow: true, phase: "running" },
        "chat.2",
      ),
    ).toEqual({ type: "enqueue", commandId: "chat.2", input: "hello" });
    expect(draft.draft).toBe("hello");
  });

  it("blocks IME composition and exposes projection-derived terminal controls", () => {
    expect(
      canSubmit(
        { ...emptyComposer, draft: "你好", imeComposing: true },
        draftChat,
      ),
    ).toContain("IME");
    expect(
      canSubmit(
        { ...emptyComposer, draft: "later" },
        { ...draftChat, phase: "completed" },
      ),
    ).toContain("terminal");
    expect(controlsFor({ ...draftChat, phase: "running" })).toContain("pause");
    expect(controlsFor({ ...draftChat, phase: "completed" })).toEqual([
      "fork",
      "continue",
    ]);
  });

  it("freezes the last contiguous projection after a disconnect gap and resyncs explicitly", () => {
    const controller = new ChatWorkspaceController();
    controller.receive({
      sequence: 1,
      kind: "timeline.append",
      payload: {
        item: { id: "one", kind: "message", title: "one", createdAt: "now" },
      },
    });
    controller.receive({
      sequence: 3,
      kind: "timeline.append",
      payload: {
        item: {
          id: "three",
          kind: "message",
          title: "three",
          createdAt: "now",
        },
      },
    });
    expect(controller.isStale()).toBe(true);
    expect(controller.snapshot().timeline.map((item) => item.id)).toEqual([
      "one",
    ]);
    controller.resynchronize(3, { ...controller.snapshot(), timeline: [] });
    expect(controller.isStale()).toBe(false);
  });

  it("escapes rich activity content, preserves source labels, and keeps unknown records inspectable", () => {
    const card = toConversationCard({
      id: "unknown",
      kind: "unknown",
      title: "<script>",
      body: "<b>raw</b>",
      reasoningCategory: "source_provided",
      createdAt: "now",
      raw: { future: true },
    });
    expect(card.content).toContain("&lt;b&gt;");
    expect(card.reasoningLabel).toBe("Source-provided source provided");
    expect(card.inspectable).toBe(true);
    const items = ["one", "two", "three"].map((id) => ({
      id,
      kind: "message" as const,
      title: id,
      createdAt: "now",
    }));
    expect(visibleTimeline(items, 1, 1).map((item) => item.id)).toEqual([
      "two",
    ]);
    expect(escapedText("<>&")).toBe("&lt;&gt;&amp;");
    for (const kind of [
      "plan",
      "model",
      "tool",
      "mcp",
      "plugin",
      "subagent",
      "external_agent",
      "artifact",
      "approval",
      "route",
      "error",
      "verification",
      "repair",
    ] as const)
      expect(
        toConversationCard({
          id: kind,
          kind,
          title: kind,
          createdAt: "now",
        }).label,
      ).not.toBe("Unknown activity");
  });

  it("paginates evidence and never invents values for unavailable records", () => {
    const records: readonly EvidenceRecord[] = [
      {
        id: "usage",
        category: "usage",
        label: "Usage",
        value: { tokens: 1 },
        state: "available",
      },
      {
        id: "secret",
        category: "debug",
        label: "Debug capture",
        value: null,
        state: "redacted",
      },
      {
        id: "route",
        category: "routing",
        label: "Route",
        value: {},
        state: "opaque",
      },
    ];
    expect(
      queryEvidence(records, { filter: "usage", offset: 0, limit: 10 }).items,
    ).toHaveLength(1);
    expect(queryEvidence(records, { offset: 1, limit: 1 }).items[0]?.id).toBe(
      "secret",
    );
    expect(inspectEvidence(records[1]!)).toContain("redacted");
    expect(inspectEvidence(records[2]!)).toContain("opaque");
    expect(
      redactedEvidenceJson({ ...records[1]!, value: "must-not-leak" }),
    ).not.toContain("must-not-leak");
  });

  it("deduplicates exact native-port retries and rejects changed command content", async () => {
    const port = new PreviewChatCorePort();
    const intent = { type: "pause", commandId: "chat.pause.1" } as const;
    const first = await port.command(intent, 2);
    await expect(port.command(intent, 2)).resolves.toEqual(first);
    await expect(
      port.command({ type: "resume", commandId: intent.commandId }, 2),
    ).rejects.toThrow("reused with different content");
  });

  it("rejects malformed runtime snapshots and keeps future card/evidence values explicit", () => {
    expect(() =>
      normalizeRuntimeSnapshot({ version: 1, lastSequence: 1 }),
    ).toThrow();
    const normalized = normalizeRuntimeSnapshot({
      version: 1,
      lastSequence: 1,
      chat: {
        ...draftChat,
        workflowName: null,
        phase: "running",
      },
      timeline: [
        {
          id: "future",
          kind: "future-card",
          title: "Future",
          body: "<script>unsafe()</script>",
          createdAt: "now",
          status: null,
          action: null,
          metadata: { retained: true },
        },
      ],
      evidence: [
        {
          id: "future-evidence",
          category: "future-category",
          label: "Future evidence",
          state: "future-state",
          value: { mustNotInfer: true },
        },
      ],
      events: [{ sequence: 1 }],
    });
    expect(normalized.timeline[0]).toMatchObject({
      kind: "unknown",
      raw: { retained: true },
    });
    expect(normalized.evidence[0]).toMatchObject({
      category: "unknown",
      state: "opaque",
    });
  });
});
