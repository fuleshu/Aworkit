// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { render } from "../test/renderWithNotifications";
import { ChatWorkspaceScreen } from "./ChatWorkspaceScreen";
import type { ChatCorePort, RuntimeEvent, RuntimeReceipt, RuntimeSnapshot } from "./corePort";

vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getTotalSize: () => count * 100,
    getVirtualItems: () => Array.from({ length: count }, (_, index) => ({ index, key: index, start: index * 100 })),
    measureElement: () => {}, measure: () => {}, resizeItem: () => {}, scrollToIndex: () => {},
  }),
}));
afterEach(cleanup);

/** Hold the native command receipt after the canonical input reaches the UI. */
it.each([false, true])("clears a committed input while its response is pending (follow-up: %s)", async lockedWorkflow => {
  let receive!: (event: RuntimeEvent) => void;
  let settle!: (receipt: RuntimeReceipt) => void;
  let commandId = "";
  let current: RuntimeSnapshot = {
    version: 0, throughSequence: 0, reducerVersion: "test", stateHash: "sha256:test",
    chat: {
      chatId: "chat.test", runId: "run.test", title: "Submission test", scope: "No project",
      workflowId: lockedWorkflow ? "workflow.simple-chat" : null, workflowName: null,
      branch: null, projectId: null, phase: lockedWorkflow ? "waiting_input" : "draft",
      lockedWorkflow, recoveryPending: false, queuedInputs: [], expectedVersion: 0,
    },
    history: [], projects: [], evidence: [], events: [],
  };
  const port: ChatCorePort = {
    async snapshot() { return current; },
    async subscribeEvents(listener) { receive = listener; return () => {}; },
    command(intent) {
      commandId = intent.commandId;
      expect(intent.type).toBe(lockedWorkflow ? "enqueue" : "start");
      return new Promise(resolve => { settle = resolve; });
    },
  };
  render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={60_000} />);
  const input = await screen.findByRole("textbox", { name: "Chat input" });
  fireEvent.change(input, { target: { value: "Repeated message" } });
  const send = screen.getByRole("button", { name: lockedWorkflow ? "Queue" : "Send" });
  await waitFor(() => expect(send).toBeEnabled());
  fireEvent.click(send);
  await waitFor(() => expect(commandId).not.toBe(""));
  expect(input).toHaveValue("Repeated message");

  const message = (sequence: number, requestId: string): RuntimeEvent => ({
    schemaVersion: 1, streamId: "chat.test", branchId: "main", sequence,
    eventId: `event.${sequence}`, kind: "message.user", payload: { body: "Repeated message", requestId },
  });
  const previous = message(1, "previous.command");
  act(() => receive(previous));
  expect(input).toHaveValue("Repeated message");
  const committed = message(2, commandId);
  act(() => receive({ ...committed, streamId: "another.chat" }));
  expect(input).toHaveValue("Repeated message");
  act(() => receive(committed));
  await waitFor(() => expect(input).toHaveValue(""));
  expect(input).toBeDisabled();
  expect(screen.getAllByText("Repeated message").length).toBeGreaterThan(0);

  // A provider error after acceptance must not restore the already sent input.
  current = { ...current, version: 2, throughSequence: 2, events: [previous, committed] };
  await act(async () => settle({ commandId, accepted: false, currentVersion: 2, reason: "Provider failed after accepting the input" }));
  await waitFor(() => expect(input).toBeEnabled());
  expect(input).toHaveValue("");
});
