import { describe, expect, it } from "vitest";
import { chatHeaderContext } from "./chatHeader";
import type { ChatProjection } from "./types";

function chat(overrides: Partial<ChatProjection> = {}): ChatProjection {
  return {
    chatId: "chat.8e2587c15a462c788adf2a4a91eb1f17496e4104",
    runId: "run.8e2587c15a462c788adf2a4a91eb1f17496e4104",
    title: "Header identity",
    scope: "Local",
    workflowId: "workflow.simple-chat",
    workflowName: "Simple Chat",
    branch: "main",
    projectId: null,
    phase: "waiting_input",
    lockedWorkflow: true,
    recoveryPending: false,
    queuedInputs: [],
    expectedVersion: 1,
    ...overrides,
  };
}

describe("Chat header identity line", () => {
  it("names the Chat the local store can resolve, not the Run", () => {
    expect(chatHeaderContext(chat())).toEqual([
      "Simple Chat",
      "main",
      "chat.8e2587c15a462c788adf2a4a91eb1f17496e4104",
    ]);
  });

  it("keeps the self-describing chat. prefix from the projection", () => {
    const identity = chatHeaderContext(chat())[2];
    expect(identity.startsWith("chat.")).toBe(true);
    expect(identity).not.toContain("run.");
  });

  it("drops empty workflow and branch fields instead of leaving separators", () => {
    expect(chatHeaderContext(chat({ workflowName: null, branch: null }))).toEqual([
      "chat.8e2587c15a462c788adf2a4a91eb1f17496e4104",
    ]);
  });

  it("hides the browser preview placeholder that names no durable Chat", () => {
    expect(
      chatHeaderContext(
        chat({ chatId: "chat.preview", runId: "run.draft", workflowName: null, branch: null }),
      ),
    ).toEqual([]);
  });
});
