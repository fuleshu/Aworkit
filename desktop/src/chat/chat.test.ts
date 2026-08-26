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
import {
  chatIntentPayload,
  normalizeRuntimeSnapshot,
  PreviewChatCorePort,
} from "./corePort";
import type { ChatProjection, EvidenceRecord } from "./types";
import { bundledDefaultWorkflowId } from "../workbench/bundledWorkflows";

const draftChat: ChatProjection = {
  chatId: "chat-1",
  runId: "run-1",
  title: "Chat",
  scope: "Project",
  workflowName: null,
  branch: "main",
  projectId: "project.test",
  phase: "draft",
  lockedWorkflow: false,
  recoveryPending: false,
  queuedInputs: [],
  expectedVersion: 0,
};

describe("Milestone 08 Chat and evidence experience", () => {
  it("creates first-send and queued-input intents while retaining drafts until a receipt", () => {
    const draft = updateComposer(emptyComposer, {
      draft: "hello",
      attachments: ["brief.md"],
      projectId: "project.atlas",
    });
    expect(submitIntent(draft, draftChat, "chat.1")).toEqual({
      type: "start",
      commandId: "chat.1",
      workflowId: bundledDefaultWorkflowId,
      projectId: "project.atlas",
      input: "hello",
      attachments: [],
    });
    expect(
      submitIntent(
        draft,
        { ...draftChat, lockedWorkflow: true, phase: "waiting_input" },
        "chat.2",
      ),
    ).toEqual({ type: "enqueue", commandId: "chat.2", input: "hello" });
    expect(draft.draft).toBe("hello");
  });

  it("projects projectId into start IPC only and never into a follow-up", () => {
    expect(
      chatIntentPayload({
        type: "start",
        commandId: "chat.project",
        workflowId: "workflow.simple-chat",
        projectId: "project.atlas",
        input: "hello",
        attachments: [],
      }),
    ).toEqual({
      workflowId: "workflow.simple-chat",
      projectId: "project.atlas",
      input: "hello",
      attachments: [],
    });
    expect(
      chatIntentPayload({
        type: "enqueue",
        commandId: "chat.follow-up",
        input: "again",
      }),
    ).toEqual({ input: "again" });
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
    expect(
      canSubmit(
        { ...emptyComposer, draft: "later" },
        { ...draftChat, disabledReason: "Configure an Exact model tier." },
      ),
    ).toBe("Configure an Exact model tier.");
    expect(
      canSubmit(
        { ...emptyComposer, draft: "read it", projectId: null },
        { ...draftChat, projectId: null },
        { workflowRequiresProject: true },
      ),
    ).toBe(
      "Select a saved project before sending because the selected workflow binds project file tools.",
    );
    expect(
      canSubmit(
        { ...emptyComposer, draft: "read it", projectId: "project.atlas" },
        { ...draftChat, projectId: null },
        { workflowRequiresProject: true },
      ),
    ).toBeNull();
    expect(
      canSubmit(
        { ...emptyComposer, draft: "later" },
        { ...draftChat, recoveryPending: true, phase: "paused" },
      ),
    ).toBe("Resume the interrupted command before composing another input.");
    expect(controlsFor({ ...draftChat, phase: "running" })).toEqual([
      "cancel",
    ]);
    expect(controlsFor({ ...draftChat, phase: "waiting_input" })).toEqual([]);
    expect(controlsFor({ ...draftChat, phase: "completed" })).toEqual([]);
  });

  it("leaves React to escape rich text, labels reasoning honestly, and keeps unknown records inspectable", () => {
    const card = toConversationCard({
      id: "unknown",
      kind: "unknown",
      title: "<script>",
      body: "<b>raw</b>",
      reasoningCategory: "source_provided",
      createdAt: "now",
      raw: { future: true },
    });
    expect(card.content).toBe("<b>raw</b>");
    expect(card.reasoningLabel).toBe("Provider-supplied reasoning");
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
      "step",
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
    const first = await port.command(intent, 0);
    await expect(port.command(intent, 0)).resolves.toEqual(first);
    await expect(
      port.command({ type: "resume", commandId: intent.commandId }, 0),
    ).rejects.toThrow("reused with different content");
  });

  it("rejects malformed runtime snapshots and keeps future evidence values explicit", () => {
    expect(() =>
      normalizeRuntimeSnapshot({ version: 1, throughSequence: 1 }),
    ).toThrow();
    const normalized = normalizeRuntimeSnapshot({
      version: 1,
      throughSequence: 1,
      reducerVersion: "chat.semantic.reducer.v1",
      stateHash: `sha256:${"0".repeat(64)}`,
      chat: {
        ...draftChat,
        workflowName: null,
        phase: "waiting_input",
      },
      projects: [],
      evidence: [
        {
          id: "future-evidence",
          category: "future-category",
          label: "Future evidence",
          state: "future-state",
          value: { mustNotInfer: true },
        },
      ],
      events: [
        {
          schemaVersion: 1,
          streamId: "chat.test",
          branchId: "main",
          sequence: 1,
          eventId: "event.chat.1",
          kind: "future.event",
          payload: { retained: true },
        },
      ],
    });
    expect(normalized.events[0]).toMatchObject({
      kind: "future.event",
      payload: { retained: true },
    });
    expect(normalized.evidence[0]).toMatchObject({
      category: "unknown",
      state: "opaque",
    });
    expect(normalized.chat.phase).toBe("waiting_input");
  });
});
