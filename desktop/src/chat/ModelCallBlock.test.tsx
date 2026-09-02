// @vitest-environment jsdom
import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { presentTimelineItems } from "./ConversationTimeline";
import { ModelCallBlock } from "./ModelCallBlock";
import type { TimelineItem } from "./types";

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
