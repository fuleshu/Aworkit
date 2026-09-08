// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import { ContextUsage } from "./ContextUsage";
import { estimateContext, projectContexts } from "./contextProjection";
import { projectSemanticTimeline } from "./activityProjection";
import type { RuntimeEvent } from "./corePort";

beforeAll(() => { HTMLDialogElement.prototype.showModal = function() { this.setAttribute("open", ""); }; });
afterEach(cleanup);
const event = (sequence: number, kind: string, payload: Record<string, unknown>): RuntimeEvent =>
  ({ schemaVersion: 1, sequence, eventId: `e.${sequence}`, streamId: "chat.context", branchId: "main", kind, payload, ...(typeof payload.spanId === "string" ? { spanId: payload.spanId } : {}) });
const events = [
  event(1, "span.started", { spanId: "node", spanKind: "graph_node", nodeId: "agent.1", label: "Agent" }),
  event(2, "span.started", { spanId: "loop", spanKind: "agent_loop", parentSpanId: "node" }),
  event(3, "span.started", { spanId: "model", spanKind: "model_call", parentSpanId: "loop", input: { messages: [{ role: "system", content: "Original system" }, { role: "user", content: "Question" }] } }),
  event(4, "span.usage", { spanId: "model", inputTokens: 800, outputTokens: 100 }),
  event(5, "span.completed", { spanId: "model", output: [{ kind: "assistant_output", text: "The answer" }] }),
];
const open = () => {
  fireEvent.click(screen.getByRole("button", { name: /Context usage/ }));
  fireEvent.click(screen.getByRole("button", { name: "Display Context" }));
};

it("projects current context, final answer and per-request usage, excluding child agents", () => {
  const all = [...events,
    event(6, "span.started", { spanId: "child", spanKind: "external_agent", parentSpanId: "loop" }),
    event(7, "span.started", { spanId: "child-model", spanKind: "model_call", parentSpanId: "child", input: { messages: [{ role: "user", content: "private child" }] } }),
  ];
  const [context] = projectContexts(all);
  expect(projectContexts(all)).toHaveLength(1);
  expect(context.sequence).toBe(5);
  expect(context.inputTokens).toBe(800);
  expect(context.document.contextMessages[0]).toEqual({ afterExchanges: 0, role: "assistant", content: "The answer" });
  expect(estimateContext(context.document).total).toBeGreaterThan(0);
});

it("opens read only, unlocks editing, and saves on the close control", async () => {
  const save = vi.fn().mockResolvedValue(true);
  render(<ContextUsage events={events} model={{ name: "Fixture", contextWindow: 32000 }} editDisabledReason={null} onSave={save} />);
  open();
  const editor = screen.getByRole("textbox", { name: "Raw model context" });
  expect(editor).toHaveProperty("readOnly", true);
  fireEvent.click(screen.getByRole("button", { name: "Enable Edit" }));
  expect(editor).toHaveProperty("readOnly", false);
  fireEvent.change(editor, { target: { value: (editor as HTMLTextAreaElement).value.replace("Original system", "Edited system") } });
  fireEvent.click(screen.getByRole("button", { name: "Close context" }));
  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  expect(save.mock.calls[0][1].input.messages[0].content).toBe("Edited system");
  await waitFor(() => expect(screen.queryByRole("textbox")).toBeNull());
});

it("keeps invalid and rejected edits open, including Escape, and allows explicit discard", async () => {
  const save = vi.fn().mockResolvedValue(false);
  render(<ContextUsage events={events} editDisabledReason={null} onSave={save} />);
  open();
  fireEvent.click(screen.getByRole("button", { name: "Enable Edit" }));
  const editor = screen.getByRole("textbox");
  const original = (editor as HTMLTextAreaElement).value;
  fireEvent.change(editor, { target: { value: "{" } });
  fireEvent(screen.getByRole("dialog", { name: "Model context" }), new Event("cancel", { bubbles: false, cancelable: true }));
  expect(screen.getByRole("alert")).toBeTruthy();
  expect(save).not.toHaveBeenCalled();
  fireEvent.change(editor, { target: { value: original.replace("Question", "Revised") } });
  fireEvent.click(screen.getByRole("button", { name: "Save and Close" }));
  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  expect(screen.getByRole("textbox")).toHaveProperty("value", original.replace("Question", "Revised"));
  fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
  expect(screen.queryByRole("textbox")).toBeNull();
});

it("locks active context, closes unchanged without saving, and projects an edit event", () => {
  const save = vi.fn();
  render(<ContextUsage events={events} editDisabledReason="Finish the current turn." onSave={save} />);
  open();
  expect(screen.getByRole("button", { name: "Enable Edit" })).toHaveProperty("disabled", true);
  fireEvent.click(screen.getByRole("button", { name: "Close context" }));
  expect(save).not.toHaveBeenCalled();
  const document = projectContexts(events)[0].document;
  const edited = event(6, "context.edited", { nodeId: "agent.1", document, body: "User edited context." });
  expect(projectContexts([...events, edited])[0]).toMatchObject({ sequence: 6, edited: true, inputTokens: null });
  expect(projectSemanticTimeline([edited])[0].title).toBe("Context edited");
});

it("shows an honest empty state when model context or capacity is unavailable", () => {
  render(<ContextUsage events={[]} editDisabledReason={null} onSave={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Context usage" }));
  expect(screen.getByText(/Context is available after the first model call/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Display Context" })).toHaveProperty("disabled", true);
});
