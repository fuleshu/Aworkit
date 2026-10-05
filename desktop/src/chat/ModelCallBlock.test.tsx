// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { presentTimelineItems } from "./ConversationTimeline";
import { ModelCallBlock } from "./ModelCallBlock";
import type { TimelineItem } from "./types";

afterEach(cleanup);

describe("ModelCallBlock", () => {
  it("keeps the model lifecycle coherent while preserving thought and speech bubbles", () => {
    const onSelect = vi.fn();
    const item = modelCall();

    render(
      <ModelCallBlock item={item} selected={false} onSelect={onSelect} />,
    );

    const block = screen.getByRole("group", {
      name: "Model call: Friendly responder",
    });
    expect(within(block).getByText("Friendly responder")).toBeVisible();
    expect(within(block).getByText("Agent node · Model call 2")).toBeVisible();

    const thought = within(block).getByLabelText("Thinking: Model call 2");
    expect(thought).toHaveClass("thinking-turn");
    expect(within(thought).getByText("Considering the request.")).toBeVisible();
    expect(within(thought).getByText("Provider-supplied reasoning")).toBeVisible();

    const speech = within(block).getByLabelText("Model output: Model call 2");
    expect(speech).toHaveClass("speech-turn");
    expect(within(speech).getByText("Hello there!")).toBeVisible();

    const input = within(block).getByText("Input").closest("details");
    const output = within(block).getByText("Output").closest("details");
    expect(input).not.toHaveAttribute("open");
    expect(output).not.toHaveAttribute("open");

    fireEvent.click(within(speech).getByText("Hello there!"));
    expect(onSelect).toHaveBeenCalledWith("span.model.2");
  });

  it("expands and collapses Input and Output without selecting Run details", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<ModelCallBlock item={modelCall()} selected={false} onSelect={onSelect} />);
    for (const label of ["Input", "Output"]) {
      const summary = screen.getByText(label);
      const details = summary.closest("details");
      await user.click(summary);
      expect(details).toHaveAttribute("open");
      await user.click(summary);
      expect(details).not.toHaveAttribute("open");
    }
    expect(onSelect).not.toHaveBeenCalled();
    const block = screen.getByRole("group", { name: "Model call: Friendly responder" });
    block.focus();
    await user.keyboard("{Enter} ");
    expect(onSelect).toHaveBeenCalledTimes(2);
  });

  it("removes only the separately committed assistant message mirrored by the stream", () => {
    const call = modelCall();
    const mirrored: TimelineItem = {
      id: "message.assistant.1",
      kind: "message",
      title: "Aworkit",
      body: "Hello there!",
      createdAt: "now",
    };
    const distinct: TimelineItem = {
      ...mirrored,
      id: "message.assistant.2",
      body: "A transformed graph result",
    };

    expect(presentTimelineItems([call, mirrored, distinct]).map(({ id }) => id)).toEqual([
      "span.model.2",
      "message.assistant.2",
    ]);
  });

  it("states a released request body while keeping the call, its usage and its result", () => {
    render(
      <ModelCallBlock item={releasedModelCall()} selected={false} onSelect={vi.fn()} />,
    );

    const block = screen.getByRole("group", { name: "Model call: Friendly responder" });
    // The call and its settled lifecycle stay exactly as they were.
    expect(within(block).getByText("Friendly responder")).toBeVisible();
    expect(block.querySelector(".model-call-heading .status")?.textContent).toBe("completed");
    expect(within(block).getByLabelText("Thinking: Model call 2")).toBeVisible();
    expect(within(block).getByLabelText("Model output: Model call 2")).toBeVisible();

    // The released body is stated, never rendered.
    const notice = within(block).getByRole("note");
    expect(notice.textContent).toContain(
      "Request body released — this turn's snapshot was superseded by newer turns, so its 1.2 MB request body was released to bound this Chat's store size. Its usage, timing and result are unaffected.",
    );
    expect(notice.textContent).toContain("Released 2026-08-03 14:02:11");
    expect(notice.textContent).toContain("20 newest turns kept");
    expect(notice.getAttribute("title")).toContain(
      `Canonical digest before release: sha256:${"c".repeat(64)}`,
    );
    expect(within(block).queryByLabelText("Input JSON")).toBeNull();
    expect(within(block).queryByText("Input")).toBeNull();
    expect(block.textContent).not.toContain("[object Object]");
    expect(block.textContent).not.toContain("undefined");
    // The result stays inspectable.
    expect(within(block).getByText("Output").closest("details")).not.toBeNull();
  });

  it("renders the request body when a payload still carries it", () => {
    const item = modelCall();
    render(
      <ModelCallBlock
        item={{ ...item, metadata: { ...(item.metadata as Record<string, unknown>), prunedPayload: marker() } }}
        selected={false}
        onSelect={vi.fn()}
      />,
    );
    const input = screen.getByText("Input").closest("details");
    expect(input).not.toBeNull();
    expect(within(input as HTMLElement).getByLabelText("Input JSON").textContent).toContain("Hello");
    expect(screen.queryByRole("note")).toBeNull();
  });
});

function modelCall(): TimelineItem {
  return {
    id: "span.model.2",
    spanId: "span.model.2",
    kind: "thinking",
    actor: "model",
    title: "Model call 2",
    body: "Considering the request.",
    createdAt: "now",
    status: "completed",
    reasoningCategory: "source_provided",
    input: { messages: [{ role: "user", content: "Hello" }] },
    output: [
      { kind: "reasoning_raw", text: "Considering the request." },
      { kind: "assistant_output", text: "Hello there!" },
      { kind: "usage", input_tokens: 8, output_tokens: 3 },
    ],
    metadata: {
      spanKind: "model_call",
      hasInput: true,
      hasOutput: true,
      workflowNode: {
        id: "node.respond",
        name: "Friendly responder",
        type: "agent",
      },
      channels: {
        reasoning: "Considering the request.",
        progress: "",
        assistantOutput: "Hello there!",
      },
    },
  };
}

/** One model call whose compiled request body retention released. */
function releasedModelCall(): TimelineItem {
  const call = modelCall();
  return {
    ...call,
    input: undefined,
    metadata: {
      ...(call.metadata as Record<string, unknown>),
      prunedPayload: marker(),
    },
  };
}

function marker(): Record<string, unknown> {
  return {
    schemaVersion: 1,
    kind: "model_call_input",
    bytes: 1_200_000,
    digestBefore: `sha256:${"c".repeat(64)}`,
    prunedAt: "2026-08-03 14:02:11",
    retainedTurns: 20,
    reason: "Superseded by newer turns.",
  };
}
