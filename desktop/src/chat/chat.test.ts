import { describe, expect, it } from "vitest";
import { canSubmit, controlsFor, emptyComposer, submitIntent, updateComposer } from "./composer";
import { escapedText, toConversationCard, visibleTimeline } from "./conversation";
import { inspectEvidence, queryEvidence } from "./evidence";
import type { ChatProjection, EvidenceRecord } from "./types";
import { ChatWorkspaceController } from "./workspace";

const draftChat: ChatProjection = { chatId: "chat-1", runId: "run-1", title: "Chat", scope: "Project", workflowName: null, branch: "main", phase: "draft", lockedWorkflow: false, queuedInputs: [] };

describe("Milestone 08 Chat and evidence experience", () => {
  it("creates first-send and queued-input intents while retaining drafts until a receipt", () => {
    const draft = updateComposer(emptyComposer, { draft: "hello", attachments: ["brief.md"] });
    expect(submitIntent(draft, draftChat, "chat.1")).toEqual({ type: "start", commandId: "chat.1", workflowId: "starter", input: "hello", attachments: ["brief.md"] });
    expect(submitIntent(draft, { ...draftChat, lockedWorkflow: true, phase: "running" }, "chat.2")).toEqual({ type: "enqueue", commandId: "chat.2", input: "hello" });
    expect(draft.draft).toBe("hello");
  });

  it("blocks IME composition and exposes projection-derived terminal controls", () => {
    expect(canSubmit({ ...emptyComposer, draft: "你好", imeComposing: true }, draftChat)).toContain("IME");
    expect(canSubmit({ ...emptyComposer, draft: "later" }, { ...draftChat, phase: "completed" })).toContain("terminal");
    expect(controlsFor({ ...draftChat, phase: "running" })).toContain("pause");
    expect(controlsFor({ ...draftChat, phase: "completed" })).toEqual(["fork", "continue"]);
  });

  it("freezes the last contiguous projection after a disconnect gap and resyncs explicitly", () => {
    const controller = new ChatWorkspaceController();
    controller.receive({ sequence: 1, kind: "timeline.append", payload: { item: { id: "one", kind: "message", title: "one", createdAt: "now" } } });
    controller.receive({ sequence: 3, kind: "timeline.append", payload: { item: { id: "three", kind: "message", title: "three", createdAt: "now" } } });
    expect(controller.isStale()).toBe(true);
    expect(controller.snapshot().timeline.map((item) => item.id)).toEqual(["one"]);
    controller.resynchronize(3, { ...controller.snapshot(), timeline: [] });
    expect(controller.isStale()).toBe(false);
  });

  it("escapes rich activity content, preserves source labels, and keeps unknown records inspectable", () => {
    const card = toConversationCard({ id: "unknown", kind: "unknown", title: "<script>", body: "<b>raw</b>", reasoningCategory: "source_provided", createdAt: "now", raw: { future: true } });
    expect(card.content).toContain("&lt;b&gt;");
    expect(card.reasoningLabel).toBe("Source-provided source provided");
    expect(card.inspectable).toBe(true);
    const items = ["one", "two", "three"].map((id) => ({ id, kind: "message" as const, title: id, createdAt: "now" }));
    expect(visibleTimeline(items, 1, 1).map((item) => item.id)).toEqual(["two"]);
    expect(escapedText("<>&")).toBe("&lt;&gt;&amp;");
  });

  it("paginates evidence and never invents values for unavailable records", () => {
    const records: readonly EvidenceRecord[] = [
      { id: "usage", category: "usage", label: "Usage", value: { tokens: 1 }, state: "available" },
      { id: "secret", category: "debug", label: "Debug capture", value: null, state: "redacted" },
      { id: "route", category: "routing", label: "Route", value: {}, state: "opaque" },
    ];
    expect(queryEvidence(records, { filter: "usage", offset: 0, limit: 10 }).items).toHaveLength(1);
    expect(queryEvidence(records, { offset: 1, limit: 1 }).items[0]?.id).toBe("secret");
    expect(inspectEvidence(records[1]!)).toContain("redacted");
    expect(inspectEvidence(records[2]!)).toContain("opaque");
  });
});
